//! Live RTMP/HLS video-stream chunk polling & FFmpeg decode loop.
//!
//! Polls time-aligned chunks from Telegram via `upload.getFile(inputGroupCallStream)` on
//! `stream_dc_id`, decodes them to RGBA8 video frames via `tg-video` (`FfmpegMedia`), and
//! feeds them into [`crate::media::VideoStreamViewer::push_rgba_frame`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/calls/group/calls_group_call.cpp` (`LoadPartTask`,
//!   `broadcastPartStart`, `requestCurrentTimeStart`)
//! - `Telegram/SourceFiles/media/view/media_view_video_stream.cpp`

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tg_video::{
    DecodedMediaFrame, FfmpegMedia, FfmpegOpenOptions, FrameData, MAX_FRAME_AREA, OutputPixelFormat,
};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::live::bridge::LiveBridgeState;
use crate::live::session::LiveSession;
use crate::media::{
    MediaOverlayWidget, VideoStreamDescriptor, VideoStreamQuality, VideoStreamStatus,
    VideoStreamViewer,
};

/// Maximum chunk size for `upload.getFile(inputGroupCallStream)` (`calls_group_call.cpp:3221`).
pub const STREAM_CHUNK_LIMIT_BYTES: i32 = 128 * 1024;

/// Maximum number of decoded RGBA8 frames buffered ahead of the UI thread.
const MAX_BUFFERED_FRAMES: usize = 48;

/// Default minimum/maximum inter-frame display duration in milliseconds.
const MIN_FRAME_DURATION_MS: i64 = 16;
const MAX_FRAME_DURATION_MS: i64 = 100;
const DEFAULT_FRAME_DURATION_MS: i64 = 33;

/// Converts a Telegram `inputGroupCallStream.scale` value (`0..=3`) to its chunk period in ms.
///
/// Exact inverse of `GroupCall::LoadPartTask` in `calls/group/calls_group_call.cpp:449-456`.
#[must_use]
pub const fn scale_to_period_ms(scale: i32) -> i64 {
    match scale {
        0 => 1_000,
        1 => 500,
        2 => 250,
        3 => 125,
        _ => 1_000,
    }
}

/// Maps [`VideoStreamQuality`] to the TL `inputGroupCallStream.video_quality` integer
/// (`calls/group/calls_group_call.cpp:3215-3219`).
#[must_use]
pub const fn video_stream_quality_to_tl(quality: VideoStreamQuality) -> i32 {
    match quality {
        VideoStreamQuality::Low => 0,
        VideoStreamQuality::Medium => 1,
        VideoStreamQuality::Full => 2,
    }
}

/// A single decoded RGBA8 frame ready to push into [`VideoStreamViewer::push_rgba_frame`].
#[derive(Debug, Clone)]
pub struct DecodedStreamFrame {
    /// Decoded frame width in pixels.
    pub width: u32,
    /// Decoded frame height in pixels.
    pub height: u32,
    /// Frame presentation timestamp in milliseconds relative to the chunk.
    pub pts_ms: i64,
    /// Frame display duration in milliseconds.
    pub duration_ms: i64,
    /// Tightly-packed straight RGBA8 pixel bytes (`width * height * 4`).
    pub rgba: Vec<u8>,
}

/// Metadata update fetched from `phone.getGroupCall` by the background stream loop.
#[derive(Debug, Clone)]
struct PendingStreamMetadata {
    title: Option<String>,
    viewer_count: u32,
}

/// Terminal state reported by the background stream loop.
#[derive(Debug, Clone)]
enum StreamTerminalState {
    Finished,
    Failed(String),
}

/// Tuning parameters for [`VideoStreamDecodeLoop`].
#[derive(Debug, Clone, Copy)]
pub struct VideoStreamLoopConfig {
    /// Timeout for individual stream RPC calls.
    pub rpc_timeout: Duration,
    /// Backoff sleep when the server reports `TIME_TOO_BIG` / `FLOOD_WAIT` or empty chunk.
    pub not_ready_delay: Duration,
    /// Backoff sleep after transient RPC/decode failures before retrying.
    pub error_retry_delay: Duration,
    /// Maximum consecutive fatal errors before marking the viewer failed.
    pub max_consecutive_errors: u32,
}

impl Default for VideoStreamLoopConfig {
    fn default() -> Self {
        Self {
            rpc_timeout: Duration::from_secs(15),
            not_ready_delay: Duration::from_millis(200),
            error_retry_delay: Duration::from_millis(300),
            max_consecutive_errors: 8,
        }
    }
}

/// Decodes a single `upload.getFile(inputGroupCallStream)` media segment into RGBA8 video frames
/// using `tg-video`'s FFmpeg wrapper (`FfmpegMedia::open_bytes`).
pub fn decode_stream_chunk_rgba(bytes: Vec<u8>) -> Result<Vec<DecodedStreamFrame>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let opts = FfmpegOpenOptions {
        seekable: true,
        ignore_editlist: true,
        hw_allowed: false,
        open_video: true,
        open_audio: false,
        video_max_area: MAX_FRAME_AREA,
        thread_count: 1,
    };
    let mut media = FfmpegMedia::open_bytes(bytes, opts)
        .map_err(|e| format!("FFmpeg failed to open stream chunk: {e}"))?;
    if !media.info().has_video {
        return Ok(Vec::new());
    }
    let fallback_dur_ms = {
        let fps = media.info().video_fps;
        if fps.is_finite() && fps > 0.5 {
            (1000.0 / fps).round() as i64
        } else {
            DEFAULT_FRAME_DURATION_MS
        }
    }
    .clamp(MIN_FRAME_DURATION_MS, MAX_FRAME_DURATION_MS);

    let mut frames = Vec::new();
    while let Some(decoded) = media
        .decode_next(true, false, OutputPixelFormat::Rgba, None, true, 0, 0)
        .map_err(|e| format!("FFmpeg decode_next failed on stream chunk: {e}"))?
    {
        if let DecodedMediaFrame::Video(vf) = decoded
            && let FrameData::Rgba(rgba) = vf.data
        {
            let expected = (vf.width as usize)
                .saturating_mul(vf.height as usize)
                .saturating_mul(4);
            if vf.width > 0 && vf.height > 0 && rgba.len() == expected {
                let dur_ms = if vf.duration_ms > 0 {
                    vf.duration_ms
                        .clamp(MIN_FRAME_DURATION_MS, MAX_FRAME_DURATION_MS)
                } else {
                    fallback_dur_ms
                };
                frames.push(DecodedStreamFrame {
                    width: vf.width,
                    height: vf.height,
                    pts_ms: vf.pts_ms,
                    duration_ms: dur_ms,
                    rgba,
                });
            }
        }
    }

    // Refine frame durations from consecutive PTS deltas when container omitted packet durations.
    for i in 0..frames.len().saturating_sub(1) {
        let delta = frames[i + 1].pts_ms - frames[i].pts_ms;
        if delta > 0 {
            frames[i].duration_ms = delta.clamp(MIN_FRAME_DURATION_MS, MAX_FRAME_DURATION_MS);
        }
    }

    Ok(frames)
}

#[derive(Default)]
struct SharedStreamQueue {
    frames: VecDeque<DecodedStreamFrame>,
    metadata: Option<PendingStreamMetadata>,
    terminal: Option<StreamTerminalState>,
}

/// Active background RTMP/HLS stream polling + FFmpeg decode loop for one live group call.
pub struct VideoStreamDecodeLoop {
    call_id: i64,
    access_hash: u64,
    cancel: Arc<AtomicBool>,
    quality_tl: Arc<AtomicI32>,
    running: Arc<AtomicBool>,
    chunks_decoded: Arc<AtomicU64>,
    frames_decoded: Arc<AtomicU64>,
    queue: Arc<Mutex<SharedStreamQueue>>,
    next_frame_due_ms: Option<i64>,
    task: Option<JoinHandle<()>>,
}

impl VideoStreamDecodeLoop {
    /// Spawns a background stream decode loop on `live.runtime().tokio_handle()`.
    #[must_use]
    pub fn start(
        live: &LiveSession,
        descriptor: VideoStreamDescriptor,
        initial_quality: VideoStreamQuality,
        config: VideoStreamLoopConfig,
    ) -> Self {
        let call_id = descriptor.call_id;
        let access_hash = descriptor.access_hash;
        let cancel = Arc::new(AtomicBool::new(false));
        let quality_tl = Arc::new(AtomicI32::new(video_stream_quality_to_tl(initial_quality)));
        let running = Arc::new(AtomicBool::new(true));
        let chunks_decoded = Arc::new(AtomicU64::new(0));
        let frames_decoded = Arc::new(AtomicU64::new(0));
        let queue = Arc::new(Mutex::new(SharedStreamQueue::default()));

        let rpc = live.runtime().rpc().clone();
        let bridge_state = Arc::clone(live.bridge_state());
        let cancel_bg = Arc::clone(&cancel);
        let quality_bg = Arc::clone(&quality_tl);
        let running_bg = Arc::clone(&running);
        let chunks_bg = Arc::clone(&chunks_decoded);
        let frames_bg = Arc::clone(&frames_decoded);
        let queue_bg = Arc::clone(&queue);

        let task = live.runtime().tokio_handle().spawn(async move {
            run_stream_decode_loop(
                rpc,
                bridge_state,
                descriptor,
                config,
                cancel_bg,
                quality_bg,
                chunks_bg,
                frames_bg,
                queue_bg,
            )
            .await;
            running_bg.store(false, Ordering::Release);
        });

        Self {
            call_id,
            access_hash,
            cancel,
            quality_tl,
            running,
            chunks_decoded,
            frames_decoded,
            queue,
            next_frame_due_ms: None,
            task: Some(task),
        }
    }

    /// Returns the group call ID driven by this loop.
    #[must_use]
    pub const fn call_id(&self) -> i64 {
        self.call_id
    }

    /// Returns the group call access hash driven by this loop.
    #[must_use]
    pub const fn access_hash(&self) -> u64 {
        self.access_hash
    }

    /// Updates the requested video quality tier for subsequent `upload.getFile` chunk fetches.
    pub fn set_quality(&self, quality: VideoStreamQuality) {
        self.quality_tl
            .store(video_stream_quality_to_tl(quality), Ordering::Release);
    }

    /// Returns `true` while the background polling/decode task is still alive.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Total number of `upload.getFile` chunks successfully decoded so far.
    #[must_use]
    pub fn chunks_decoded(&self) -> u64 {
        self.chunks_decoded.load(Ordering::Acquire)
    }

    /// Total number of RGBA8 video frames decoded so far.
    #[must_use]
    pub fn frames_decoded(&self) -> u64 {
        self.frames_decoded.load(Ordering::Acquire)
    }

    /// Returns `true` if decoded frames are waiting in the playout queue.
    #[must_use]
    pub fn has_buffered_frames(&self) -> bool {
        self.queue
            .lock()
            .is_ok_and(|guard| !guard.frames.is_empty())
    }

    /// Drains metadata, decoded RGBA8 frames, and terminal events into `viewer`, calling
    /// [`VideoStreamViewer::push_rgba_frame`] for each presented frame.
    ///
    /// Returns the number of RGBA8 frames pushed into `viewer` on this tick.
    pub fn drain_into_viewer(
        &mut self,
        viewer: &mut VideoStreamViewer,
        world_now_ms: i64,
    ) -> usize {
        self.set_quality(viewer.quality());

        let (metadata, frames_to_push, terminal) = {
            let Ok(mut guard) = self.queue.lock() else {
                return 0;
            };
            let meta = guard.metadata.take();
            let mut to_push = Vec::new();

            if !guard.frames.is_empty() {
                if !viewer.has_video_frames() || self.next_frame_due_ms.is_none() {
                    if let Some(first) = guard.frames.pop_front() {
                        self.next_frame_due_ms =
                            Some(world_now_ms.saturating_add(first.duration_ms));
                        to_push.push(first);
                    }
                } else {
                    while let Some(due_ms) = self.next_frame_due_ms {
                        if world_now_ms < due_ms {
                            break;
                        }
                        let Some(next) = guard.frames.pop_front() else {
                            self.next_frame_due_ms = None;
                            break;
                        };
                        let next_due = due_ms.saturating_add(next.duration_ms).max(world_now_ms);
                        self.next_frame_due_ms = Some(next_due);
                        to_push.push(next);
                        if to_push.len() >= 4 {
                            // Keep latency low without starving the UI thread.
                            break;
                        }
                    }
                }
            }

            let term = if guard.frames.is_empty() {
                guard.terminal.take()
            } else {
                None
            };
            (meta, to_push, term)
        };

        if let Some(meta) = metadata {
            viewer.update_metadata(meta.title, meta.viewer_count, false);
        }

        let pushed = frames_to_push.len();
        if let Some(latest) = frames_to_push.into_iter().last() {
            viewer.push_rgba_frame(latest.width, latest.height, latest.rgba);
        }

        if let Some(term) = terminal {
            match term {
                StreamTerminalState::Finished => viewer.mark_finished(),
                StreamTerminalState::Failed(err) => viewer.mark_failed(err),
            }
        }

        pushed
    }

    /// Cancels and aborts the background polling/decode loop.
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(handle) = self.task.take() {
            handle.abort();
        }
        self.running.store(false, Ordering::Release);
    }
}

impl Drop for VideoStreamDecodeLoop {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_stream_decode_loop<C: RpcClient + Clone + 'static>(
    rpc: C,
    bridge_state: Arc<LiveBridgeState>,
    descriptor: VideoStreamDescriptor,
    config: VideoStreamLoopConfig,
    cancel: Arc<AtomicBool>,
    quality_tl: Arc<AtomicI32>,
    chunks_decoded: Arc<AtomicU64>,
    frames_decoded: Arc<AtomicU64>,
    queue: Arc<Mutex<SharedStreamQueue>>,
) {
    let input_call = tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
        id: descriptor.call_id,
        access_hash: descriptor.access_hash as i64,
    });

    let mut stream_dc_id: DcId = descriptor.stream_dc_id;

    // 1. Hydrate group call metadata + `stream_dc_id` if not already known.
    if stream_dc_id <= 0 {
        let req = tl_fns::phone::GetGroupCall {
            call: input_call.clone(),
            limit: 20,
        };
        match tokio::time::timeout(config.rpc_timeout, rpc.invoke(&req)).await {
            Ok(Ok(tl_enums::phone::GroupCall::GroupCall(gc))) => match gc.call {
                tl_enums::GroupCall::GroupCall(c) => {
                    stream_dc_id = c.stream_dc_id.unwrap_or(0);
                    if let Ok(mut guard) = queue.lock() {
                        guard.metadata = Some(PendingStreamMetadata {
                            title: c.title,
                            viewer_count: c.participants_count.max(0) as u32,
                        });
                    }
                    bridge_state.notify_ui();
                }
                tl_enums::GroupCall::Discarded(_) => {
                    push_terminal(&queue, &bridge_state, StreamTerminalState::Finished);
                    return;
                }
            },
            Ok(Err(err)) => {
                let inv: InvocationError = err.into();
                if is_stream_ended_error(&inv) {
                    push_terminal(&queue, &bridge_state, StreamTerminalState::Finished);
                    return;
                }
                debug!(error = %inv, "phone.getGroupCall failed before stream loop; continuing with main DC");
            }
            Err(_) => {
                debug!("phone.getGroupCall timed out before stream loop; continuing with main DC");
            }
        }
    }

    if cancel.load(Ordering::Acquire) {
        return;
    }

    let effective_dc = |dc: DcId| -> DcId { if dc > 0 { dc } else { rpc.main_dc_id() } };

    // 2. Discover initial video channel, scale, and server timestamp via
    // `phone.getGroupCallStreamChannels`.
    let (mut video_channel, mut scale, mut next_time_ms) = query_stream_channel(
        &rpc,
        &input_call,
        effective_dc(stream_dc_id),
        config.rpc_timeout,
    )
    .await;

    let mut consecutive_errors: u32 = 0;

    // 3. Continuous chunk polling + FFmpeg decode loop.
    while !cancel.load(Ordering::Acquire) {
        // Apply backpressure if the UI playout queue is full.
        let queue_len = queue.lock().map_or(0, |g| g.frames.len());
        if queue_len >= MAX_BUFFERED_FRAMES {
            tokio::time::sleep(Duration::from_millis(25)).await;
            continue;
        }

        let period_ms = scale_to_period_ms(scale);
        let q_tl = quality_tl.load(Ordering::Acquire);
        let (vc_opt, vq_opt) = if video_channel != 0 {
            (Some(video_channel), Some(q_tl))
        } else {
            (None, None)
        };

        let req = tl_fns::upload::GetFile {
            precise: false,
            cdn_supported: false,
            location: tl_enums::InputFileLocation::InputGroupCallStream(
                tl_types::InputGroupCallStream {
                    call: input_call.clone(),
                    time_ms: next_time_ms,
                    scale,
                    video_channel: vc_opt,
                    video_quality: vq_opt,
                },
            ),
            offset: 0,
            limit: STREAM_CHUNK_LIMIT_BYTES,
        };

        let dc = effective_dc(stream_dc_id);
        match tokio::time::timeout(config.rpc_timeout, rpc.invoke_in_dc(dc, &req)).await {
            Ok(Ok(tl_enums::upload::File::File(file))) => {
                if file.bytes.is_empty() {
                    tokio::time::sleep(config.not_ready_delay).await;
                    continue;
                }

                let raw_bytes = file.bytes;
                let decoded_res =
                    tokio::task::spawn_blocking(move || decode_stream_chunk_rgba(raw_bytes)).await;

                match decoded_res {
                    Ok(Ok(frames)) => {
                        consecutive_errors = 0;
                        chunks_decoded.fetch_add(1, Ordering::Release);
                        let frame_count = frames.len() as u64;
                        if frame_count > 0 {
                            frames_decoded.fetch_add(frame_count, Ordering::Release);
                            if let Ok(mut guard) = queue.lock() {
                                for frame in frames {
                                    if guard.frames.len() >= MAX_BUFFERED_FRAMES {
                                        guard.frames.pop_front();
                                    }
                                    guard.frames.push_back(frame);
                                }
                            }
                            bridge_state.notify_ui();
                        }
                        next_time_ms = next_time_ms.saturating_add(period_ms);
                    }
                    Ok(Err(err)) => {
                        warn!(error = %err, time_ms = next_time_ms, "stream chunk FFmpeg decode failed");
                        consecutive_errors += 1;
                        next_time_ms = next_time_ms.saturating_add(period_ms);
                        if consecutive_errors >= config.max_consecutive_errors {
                            push_terminal(&queue, &bridge_state, StreamTerminalState::Failed(err));
                            return;
                        }
                        tokio::time::sleep(config.error_retry_delay).await;
                    }
                    Err(join_err) => {
                        warn!(error = %join_err, "stream chunk decode task panicked");
                        push_terminal(
                            &queue,
                            &bridge_state,
                            StreamTerminalState::Failed(
                                "Video stream decoder interrupted".to_owned(),
                            ),
                        );
                        return;
                    }
                }
            }
            Ok(Ok(tl_enums::upload::File::CdnRedirect(_))) => {
                debug!("inputGroupCallStream returned fileCdnRedirect; resyncing channels");
                let (ch, sc, ts) =
                    query_stream_channel(&rpc, &input_call, dc, config.rpc_timeout).await;
                video_channel = ch;
                scale = sc;
                next_time_ms = ts;
                tokio::time::sleep(config.not_ready_delay).await;
            }
            Ok(Err(err)) => {
                let inv: InvocationError = err.into();
                if is_stream_ended_error(&inv) {
                    push_terminal(&queue, &bridge_state, StreamTerminalState::Finished);
                    return;
                }
                if is_not_ready_error(&inv) {
                    tokio::time::sleep(config.not_ready_delay).await;
                    continue;
                }
                consecutive_errors += 1;
                debug!(error = %inv, consecutive_errors, "inputGroupCallStream chunk fetch error");
                if consecutive_errors >= config.max_consecutive_errors {
                    push_terminal(
                        &queue,
                        &bridge_state,
                        StreamTerminalState::Failed(inv.to_string()),
                    );
                    return;
                }
                let (ch, sc, ts) =
                    query_stream_channel(&rpc, &input_call, dc, config.rpc_timeout).await;
                video_channel = ch;
                scale = sc;
                next_time_ms = ts;
                tokio::time::sleep(config.error_retry_delay).await;
            }
            Err(_) => {
                tokio::time::sleep(config.not_ready_delay).await;
            }
        }
    }
}

async fn query_stream_channel<C: RpcClient + Clone + 'static>(
    rpc: &C,
    input_call: &tl_enums::InputGroupCall,
    dc_id: DcId,
    timeout: Duration,
) -> (i32, i32, i64) {
    let req = tl_fns::phone::GetGroupCallStreamChannels {
        call: input_call.clone(),
    };
    let resp = match tokio::time::timeout(timeout, rpc.invoke_in_dc(dc_id, &req)).await {
        Ok(Ok(r)) => Some(r),
        _ => None,
    };

    if let Some(tl_enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(list)) = resp {
        let chosen = list
            .channels
            .iter()
            .map(|c| {
                let tl_enums::GroupCallStreamChannel::GroupCallStreamChannel(inner) = c;
                inner
            })
            .find(|c| c.channel > 0)
            .or_else(|| {
                list.channels.first().map(|c| {
                    let tl_enums::GroupCallStreamChannel::GroupCallStreamChannel(inner) = c;
                    inner
                })
            });

        if let Some(ch) = chosen {
            let period = scale_to_period_ms(ch.scale);
            let ts = if ch.last_timestamp_ms > 0 {
                (ch.last_timestamp_ms / period) * period
            } else {
                aligned_now_ms(period)
            };
            return (ch.channel, ch.scale, ts);
        }
    }

    (1, 0, aligned_now_ms(1_000))
}

fn aligned_now_ms(period: i64) -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().min(i64::MAX as u128) as i64);
    ((now / period) * period - period).max(0)
}

fn push_terminal(
    queue: &Arc<Mutex<SharedStreamQueue>>,
    bridge_state: &Arc<LiveBridgeState>,
    state: StreamTerminalState,
) {
    if let Ok(mut guard) = queue.lock() {
        guard.terminal = Some(state);
    }
    bridge_state.notify_ui();
}

fn error_name(err: &InvocationError) -> &str {
    match err {
        InvocationError::Rpc(rpc_err) => rpc_err.name.as_str(),
        _ => "",
    }
}

fn is_not_ready_error(err: &InvocationError) -> bool {
    let name = error_name(err);
    name == "TIME_TOO_BIG" || name.starts_with("FLOOD_WAIT")
}

fn is_stream_ended_error(err: &InvocationError) -> bool {
    let name = error_name(err);
    matches!(
        name,
        "GROUPCALL_INVALID" | "CALL_ALREADY_DECLINED" | "GROUPCALL_FORBIDDEN"
    )
}

// =============================================================================
// Per-session live window registry (zero fields added to LiveSession / LiveActiveWorkspace)
// =============================================================================

struct ActiveLoopEntry {
    owner: Weak<LiveBridgeState>,
    loop_handle: VideoStreamDecodeLoop,
}

fn loop_registry() -> &'static Mutex<HashMap<usize, ActiveLoopEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, ActiveLoopEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Synchronizes the active [`VideoStreamDecodeLoop`] for `live` with any [`VideoStreamViewer`]
/// currently open inside `overlay`.
///
/// - When a stream is open in `overlay` (from a chat message bubble or a live story), starts a
///   background `upload.getFile(inputGroupCallStream)` + FFmpeg decode loop if not already
///   running, syncs quality changes, and pushes decoded RGBA8 frames via
///   [`VideoStreamViewer::push_rgba_frame`].
/// - When `overlay` closes or switches away from a live stream, stops and drops the loop cleanly.
pub fn sync_live_video_stream(
    live: &LiveSession,
    overlay: &mut MediaOverlayWidget,
    world_now_ms: i64,
) {
    sync_live_video_stream_with_config(
        live,
        overlay,
        world_now_ms,
        VideoStreamLoopConfig::default(),
    );
}

/// Variant of [`sync_live_video_stream`] accepting a custom [`VideoStreamLoopConfig`] (used in
/// deterministic tests to shorten retry backoffs).
pub fn sync_live_video_stream_with_config(
    live: &LiveSession,
    overlay: &mut MediaOverlayWidget,
    world_now_ms: i64,
    config: VideoStreamLoopConfig,
) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = loop_registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);

    let Some(viewer) = overlay.video_stream_mut() else {
        map.remove(&key);
        return;
    };

    if matches!(
        viewer.status(),
        VideoStreamStatus::Finished | VideoStreamStatus::Failed(_)
    ) {
        map.remove(&key);
        return;
    }

    let desc = viewer.descriptor().clone();
    let quality = viewer.quality();

    let needs_spawn = match map.get(&key) {
        Some(existing) => {
            existing.loop_handle.call_id() != desc.call_id
                || existing.loop_handle.access_hash() != desc.access_hash
        }
        None => true,
    };

    if needs_spawn {
        let handle = VideoStreamDecodeLoop::start(live, desc, quality, config);
        map.insert(
            key,
            ActiveLoopEntry {
                owner: Arc::downgrade(live.bridge_state()),
                loop_handle: handle,
            },
        );
    }

    if let Some(entry) = map.get_mut(&key) {
        let _ = entry.loop_handle.drain_into_viewer(viewer, world_now_ms);
        if entry.loop_handle.has_buffered_frames()
            && let Ok(guard) = live.bridge_state().egui_ctx.lock()
            && let Some(ctx) = guard.as_ref()
        {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}

/// Stops and removes any active live video-stream decode loop for `live` (called on exit/logout).
pub fn stop_live_video_stream(live: &LiveSession) {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    if let Ok(mut map) = loop_registry().lock() {
        map.remove(&key);
    }
}

/// Returns `(chunks_decoded, frames_decoded)` for the active video-stream loop of `live`, if any.
#[must_use]
pub fn active_video_stream_stats(live: &LiveSession) -> Option<(u64, u64)> {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let map = loop_registry().lock().ok()?;
    map.get(&key).map(|entry| {
        (
            entry.loop_handle.chunks_decoded(),
            entry.loop_handle.frames_decoded(),
        )
    })
}

/// Hydrates an open [`crate::media::VideoStreamViewer`] with live metadata from
/// `phone.getGroupCall` + `phone.getGroupCallStreamChannels`.
pub fn populate_video_stream_metadata(
    live: &crate::live::LiveSession,
    viewer: &mut crate::media::VideoStreamViewer,
) -> Result<crate::live::dice_pack::ResolvedVideoStreamInfo, String> {
    let desc = viewer.descriptor().clone();
    match crate::live::dice_pack::fetch_video_stream_info(live, desc.call_id, desc.access_hash) {
        Ok(info) => {
            viewer.update_metadata(
                Some(info.title.clone()),
                info.participants_count.max(0) as u32,
                false,
            );
            Ok(info)
        }
        Err(err) => {
            if err.contains("has ended") {
                viewer.mark_finished();
            } else {
                viewer.mark_failed(err.clone());
            }
            Err(err)
        }
    }
}
