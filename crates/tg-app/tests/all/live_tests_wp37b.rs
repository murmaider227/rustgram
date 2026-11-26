//! WP-37b follow-up integration & UI tests against fakes:
//! 1. Live RTMP/HLS stream decode loop (`upload.getFile(InputGroupCallStream)` -> FFmpeg decode
//!    via `tg-video` -> `VideoStreamViewer::push_rgba_frame`) both directly and through
//!    `LiveActiveWorkspace`.
//! 2. Settings > Chat > "Emoji Sets" cloud download triggered through `SettingsBackend` callback
//!    (tested both with a fake downloader backend and with `LiveSession` driving
//!    `EmojiSetsManager::load_and_switch_to`).
//! 3. `updateEmojiGameInfo` forwarded from the live update pump (`rpc.subscribe_updates()`) to
//!    `apply_emoji_game_info_update` and reflected in `cached_dice_game_options`.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{PeerUpdateFlags, User, UserFlags, UserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use tg_app::chat_helpers::{EmojiSetState, EmojiSetsManager};
use tg_app::live::dice_pack::cached_dice_game_options;
use tg_app::live::host::LiveActiveWorkspace;
use tg_app::live::session::LiveSession;
use tg_app::live::video_stream_loop::{
    VideoStreamDecodeLoop, VideoStreamLoopConfig, active_video_stream_stats,
    decode_stream_chunk_rgba, scale_to_period_ms, video_stream_quality_to_tl,
};
use tg_app::media::{
    VideoStreamDescriptor, VideoStreamQuality, VideoStreamStatus, VideoStreamViewer,
};
use tg_app::settings::backend::SettingsBackend;
use tg_app::settings::chat::{ChatSettingsState, render_chat_settings_section};
use tg_app::settings::mock::MockSettingsBackend;
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::settings::ShellSettings;

// =============================================================================
// Local FakeRpcClient + Synthetic LiveSession Harness
// =============================================================================

#[derive(Debug, Clone)]
struct RecordedRpcCall {
    dc_id: DcId,
    ctor: u32,
    bytes: Vec<u8>,
}

type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

type ResponseHandler =
    Arc<dyn Fn(DcId, u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRpcCall>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    handler: Arc<Mutex<Option<ResponseHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            handler: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn set_handler(
        &self,
        f: impl Fn(DcId, u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync + 'static,
    ) {
        *self.handler.lock().expect("lock handler") = Some(Arc::new(f));
    }

    fn recorded_calls(&self, ctor: u32) -> Vec<RecordedRpcCall> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|c| c.ctor == ctor)
            .cloned()
            .collect()
    }

    fn send_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push(RecordedRpcCall {
                dc_id,
                ctor,
                bytes: bytes.clone(),
            });

        if let Some(h) = self.handler.lock().expect("lock handler").clone()
            && let Some(res) = h(dc_id, ctor, &bytes)
        {
            return match res {
                Ok(raw) => R::Return::from_bytes(&raw)
                    .map_err(|e| InvocationError::Deserialize(e.to_string())),
                Err(err) => Err(err),
            };
        }

        let next = self.responses.lock().expect("lock responses").pop_front();
        match next {
            Some((expected, Ok(raw))) => {
                assert_eq!(
                    ctor, expected,
                    "unexpected RPC constructor: got {ctor:#010x}, expected {expected:#010x}"
                );
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            Some((expected, Err(err))) => {
                assert_eq!(ctor, expected);
                Err(err)
            }
            None => Err(InvocationError::Rpc(RpcError {
                code: 500,
                name: format!("UNMOCKED_RPC_{ctor:#010x}"),
                value: None,
            })),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock dc")
    }
}

fn make_live_session() -> (LiveSession, FakeRpcClient, tempfile::TempDir) {
    let fake = FakeRpcClient::new(2);
    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let self_uid = UserId(77_001);
    let runtime = Arc::new(
        SessionRuntime::start(
            self_uid,
            tg_app::live::ErasedRpcClient::new(fake.clone()),
            cfg,
        )
        .expect("start test SessionRuntime"),
    );
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        let mut me = User::new(self_uid, true);
        me.first_name = "Tester".to_owned();
        me.flags.insert(UserFlags::SELF);
        s.upsert_user(me, PeerUpdateFlags::NAME);
    });
    let live = LiveSession::new(runtime, 372);
    let tmp = tempfile::tempdir().expect("tempdir");
    live.set_media_cache_dir(tmp.path())
        .expect("set_media_cache_dir");
    (live, fake, tmp)
}

fn generate_local_test_stream_chunk_bytes() -> Vec<u8> {
    let dir = tempfile::tempdir().expect("tempdir for test stream");
    let path = dir.path().join("chunk.mp4");
    tg_video::generate_test_clip(&path, 64, 48, 10, 400, false)
        .expect("tg_video::generate_test_clip should succeed");
    std::fs::read(&path).expect("read generated mp4 chunk")
}

fn build_emoji_zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let opts = SimpleFileOptions::default();
        for (name, data) in entries {
            zip.start_file(*name, opts).expect("start_file");
            zip.write_all(data).expect("write zip entry");
        }
        zip.finish().expect("finish zip");
    }
    cursor.into_inner()
}

fn make_group_call_response(
    call_id: i64,
    access_hash: i64,
    title: &str,
    stream_dc_id: i32,
    participants_count: i32,
) -> tl_enums::phone::GroupCall {
    tl_enums::phone::GroupCall::GroupCall(tl_types::phone::GroupCall {
        call: tl_enums::GroupCall::GroupCall(tl_types::GroupCall {
            join_muted: true,
            can_change_join_muted: false,
            join_date_asc: false,
            schedule_start_subscribed: false,
            can_start_video: false,
            record_video_active: false,
            rtmp_stream: true,
            listeners_hidden: false,
            conference: false,
            creator: false,
            messages_enabled: true,
            can_change_messages_enabled: false,
            min: false,
            id: call_id,
            access_hash,
            participants_count,
            title: Some(title.to_owned()),
            stream_dc_id: Some(stream_dc_id),
            record_start_date: None,
            schedule_date: None,
            unmuted_video_count: None,
            unmuted_video_limit: 10,
            version: 1,
            invite_link: None,
            send_paid_messages_stars: None,
            default_send_as: None,
        }),
        participants: Vec::new(),
        participants_next_offset: String::new(),
        chats: Vec::new(),
        users: Vec::new(),
    })
}

fn make_stream_channels_response(
    channel: i32,
    scale: i32,
    last_timestamp_ms: i64,
) -> tl_enums::phone::GroupCallStreamChannels {
    tl_enums::phone::GroupCallStreamChannels::GroupCallStreamChannels(
        tl_types::phone::GroupCallStreamChannels {
            channels: vec![tl_enums::GroupCallStreamChannel::GroupCallStreamChannel(
                tl_types::GroupCallStreamChannel {
                    channel,
                    scale,
                    last_timestamp_ms,
                },
            )],
        },
    )
}

fn decode_upload_get_file_stream(bytes: &[u8]) -> (tl_types::InputGroupCallStream, i32) {
    let mut cur = tg_tl::Cursor::new(bytes);
    let ctor = cur.read_u32().expect("ctor");
    assert_eq!(ctor, tl_fns::upload::GetFile::CONSTRUCTOR_ID);
    let flags = cur.read_u32().expect("flags");
    let _ = flags;
    let loc = tl_enums::InputFileLocation::deserialize(&mut cur).expect("location");
    let _offset = cur.read_i64().expect("offset");
    let limit = cur.read_i32().expect("limit");
    match loc {
        tl_enums::InputFileLocation::InputGroupCallStream(s) => (s, limit),
        other => panic!("expected InputGroupCallStream, got {other:?}"),
    }
}

// =============================================================================
// 1. Live RTMP/HLS Stream Decode Loop (direct + LiveActiveWorkspace)
// =============================================================================

#[test]
fn wp37b_live_video_stream_decode_loop_polls_chunks_decodes_ffmpeg_and_pushes_frames() {
    assert_eq!(scale_to_period_ms(0), 1_000);
    assert_eq!(scale_to_period_ms(1), 500);
    assert_eq!(scale_to_period_ms(2), 250);
    assert_eq!(scale_to_period_ms(3), 125);
    assert_eq!(video_stream_quality_to_tl(VideoStreamQuality::Low), 0);
    assert_eq!(video_stream_quality_to_tl(VideoStreamQuality::Medium), 1);
    assert_eq!(video_stream_quality_to_tl(VideoStreamQuality::Full), 2);

    let chunk_mp4 = generate_local_test_stream_chunk_bytes();
    let sanity_frames = decode_stream_chunk_rgba(chunk_mp4.clone()).expect("decode local clip");
    assert!(
        !sanity_frames.is_empty(),
        "locally generated test stream must yield RGBA frames"
    );
    assert_eq!(sanity_frames[0].width, 64);
    assert_eq!(sanity_frames[0].height, 48);
    assert_eq!(sanity_frames[0].rgba.len(), 64 * 48 * 4);

    let (live, fake, _tmp) = make_live_session();
    let call_id = 88_1001i64;
    let access_hash = 44_2002u64;
    let stream_dc = 4i32;
    let base_ts = 1_735_000_000_000i64;

    let get_file_attempts = Arc::new(Mutex::new(0u32));
    let attempts_cb = Arc::clone(&get_file_attempts);
    let chunk_cb = chunk_mp4.clone();

    fake.set_handler(move |_dc, ctor, bytes| {
        if ctor == tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID {
            return Some(Ok(make_group_call_response(
                call_id,
                access_hash as i64,
                "RTMP Broadcast Test",
                stream_dc,
                142,
            )
            .to_bytes()));
        }
        if ctor == tl_fns::phone::GetGroupCallStreamChannels::CONSTRUCTOR_ID {
            return Some(Ok(make_stream_channels_response(1, 1, base_ts).to_bytes()));
        }
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let (stream_loc, _limit) = decode_upload_get_file_stream(bytes);
            let mut n = attempts_cb.lock().expect("lock attempts");
            *n += 1;
            let step = *n;
            // Attempt 1: serve chunk at `base_ts`.
            // Attempt 2: return `TIME_TOO_BIG` once to verify retry without advancing timestamp.
            // Attempt 3: serve second chunk at `base_ts + 500`.
            // Attempt 4+: return `GROUPCALL_INVALID` to verify clean stream finish.
            return match step {
                1 => {
                    assert_eq!(stream_loc.time_ms, base_ts);
                    Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                        r#type: tl_enums::storage::FileType::FileMp4,
                        mtime: 1_735_000_000,
                        bytes: chunk_cb.clone(),
                    })
                    .to_bytes()))
                }
                2 => {
                    assert_eq!(stream_loc.time_ms, base_ts + 500);
                    Some(Err(InvocationError::Rpc(RpcError {
                        code: 400,
                        name: "TIME_TOO_BIG".to_owned(),
                        value: None,
                    })))
                }
                3 => {
                    assert_eq!(stream_loc.time_ms, base_ts + 500);
                    Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                        r#type: tl_enums::storage::FileType::FileMp4,
                        mtime: 1_735_000_001,
                        bytes: chunk_cb.clone(),
                    })
                    .to_bytes()))
                }
                _ => Some(Err(InvocationError::Rpc(RpcError {
                    code: 400,
                    name: "GROUPCALL_INVALID".to_owned(),
                    value: None,
                }))),
            };
        }
        None
    });

    let descriptor = VideoStreamDescriptor {
        call_id,
        access_hash,
        peer_id: -100_999,
        title: "Connecting...".to_owned(),
        call_link_slug: String::new(),
        join_msg_id: 77,
        rtmp_stream: true,
        stream_url: None,
        stream_dc_id: 0,
        is_live_story: false,
    };
    let mut viewer = VideoStreamViewer::new(descriptor.clone(), 0.8);
    assert!(viewer.is_loading());
    assert!(!viewer.has_video_frames());

    let cfg = VideoStreamLoopConfig {
        rpc_timeout: Duration::from_secs(5),
        not_ready_delay: Duration::from_millis(10),
        error_retry_delay: Duration::from_millis(10),
        max_consecutive_errors: 4,
    };
    let mut decode_loop =
        VideoStreamDecodeLoop::start(&live, descriptor, VideoStreamQuality::Medium, cfg);

    // Wait until both chunks are decoded and the stream finishes on `GROUPCALL_INVALID`.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if decode_loop.chunks_decoded() >= 2 && !decode_loop.is_running() {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    assert_eq!(
        decode_loop.chunks_decoded(),
        2,
        "expected both RTMP chunks to be fetched and decoded"
    );
    assert!(
        decode_loop.frames_decoded() >= 2,
        "expected FFmpeg to decode video frames from both chunks"
    );

    // First drain pushes the initial RGBA8 frame immediately into VideoStreamViewer,
    // clearing the loading shimmer and marking the stream Live.
    let pushed = decode_loop.drain_into_viewer(&mut viewer, 1_000);
    assert!(pushed >= 1, "first drain must push an RGBA frame");
    assert!(viewer.has_video_frames());
    assert!(!viewer.is_loading());
    assert_eq!(viewer.status(), &VideoStreamStatus::Live);
    assert_eq!(viewer.descriptor().title, "RTMP Broadcast Test");

    // Advance virtual clock to drain remaining frames and observe clean stream finish.
    let mut t = 1_000i64;
    for _ in 0..40 {
        t += 120;
        let _ = decode_loop.drain_into_viewer(&mut viewer, t);
        if matches!(viewer.status(), VideoStreamStatus::Finished) {
            break;
        }
    }
    assert_eq!(viewer.status(), &VideoStreamStatus::Finished);
    assert!(viewer.close_requested());

    // Verify `upload.getFile` calls targeted `stream_dc` with exact `InputGroupCallStream` fields.
    let file_calls = fake.recorded_calls(tl_fns::upload::GetFile::CONSTRUCTOR_ID);
    assert!(file_calls.len() >= 4);
    for call in &file_calls {
        assert_eq!(call.dc_id, stream_dc, "chunks must route to stream_dc_id");
        let (stream_loc, limit) = decode_upload_get_file_stream(&call.bytes);
        assert_eq!(stream_loc.scale, 1);
        assert_eq!(stream_loc.video_channel, Some(1));
        assert_eq!(stream_loc.video_quality, Some(1)); // started with VideoStreamQuality::Medium
        assert_eq!(limit, 128 * 1024);
    }
}

#[test]
fn wp37b_live_workspace_runs_video_stream_decode_loop_and_stops_on_close() {
    let chunk_mp4 = generate_local_test_stream_chunk_bytes();
    let (live, fake, tmp) = make_live_session();
    let call_id = 88_2002i64;
    let access_hash = 55_3003u64;
    let base_ts = 1_735_100_000_000i64;
    let served = Arc::new(Mutex::new(0u32));
    let served_cb = Arc::clone(&served);

    fake.set_handler(move |_dc, ctor, _bytes| {
        if ctor == tl_fns::phone::GetGroupCall::CONSTRUCTOR_ID {
            return Some(Ok(make_group_call_response(
                call_id,
                access_hash as i64,
                "Live Window Stream",
                2,
                55,
            )
            .to_bytes()));
        }
        if ctor == tl_fns::phone::GetGroupCallStreamChannels::CONSTRUCTOR_ID {
            return Some(Ok(make_stream_channels_response(1, 0, base_ts).to_bytes()));
        }
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let mut n = served_cb.lock().expect("lock served");
            *n += 1;
            if *n <= 2 {
                return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FileMp4,
                    mtime: 1_735_100_000,
                    bytes: chunk_mp4.clone(),
                })
                .to_bytes()));
            }
            return Some(Err(InvocationError::Rpc(RpcError {
                code: 400,
                name: "TIME_TOO_BIG".to_owned(),
                value: None,
            })));
        }
        None
    });

    let settings_path = tmp.path().join("settings.json");
    let mut workspace = LiveActiveWorkspace::new_headless(
        live.clone(),
        ShellSettings::default(),
        settings_path,
        false,
    );

    // Open a live video stream inside the live window's MediaOverlayWidget.
    let now_ms = workspace.controller().elapsed_ms();
    workspace
        .controller_mut()
        .media_overlay_mut()
        .open_video_stream(
            VideoStreamDescriptor {
                call_id,
                access_hash,
                peer_id: -10_042,
                title: "Opening...".to_owned(),
                call_link_slug: String::new(),
                join_msg_id: 10,
                rtmp_stream: true,
                stream_url: None,
                stream_dc_id: 0,
                is_live_story: false,
            },
            now_ms,
        );

    let ctx = egui::Context::default();
    let run_frame = |ws: &mut LiveActiveWorkspace| {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1100.0, 760.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = ws.show(&ctx, ui);
        });
        out.textures_delta.clear();
    };

    // Frame 1 spawns the decode loop via `sync_live_video_stream`.
    run_frame(&mut workspace);

    // Wait for background FFmpeg decode of the first chunk, then pump frames until viewer is Live.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        run_frame(&mut workspace);
        if workspace
            .controller()
            .media_overlay()
            .video_stream()
            .is_some_and(|v| v.has_video_frames())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let viewer = workspace
        .controller()
        .media_overlay()
        .video_stream()
        .expect("video stream still open");
    assert!(
        viewer.has_video_frames(),
        "LiveActiveWorkspace should push FFmpeg-decoded frames into VideoStreamViewer"
    );
    assert_eq!(viewer.status(), &VideoStreamStatus::Live);
    assert_eq!(viewer.descriptor().title, "Live Window Stream");
    let (chunks, frames) = active_video_stream_stats(&live).expect("active stream stats");
    assert!(chunks >= 1 && frames >= 1);

    // Closing the overlay stops and removes the background stream loop on the next tick.
    workspace.controller_mut().media_overlay_mut().close();
    run_frame(&mut workspace);
    assert!(
        active_video_stream_stats(&live).is_none(),
        "closing overlay must stop and drop the active stream decode loop"
    );
}

// =============================================================================
// 2. Settings > Chat > "Emoji Sets" via SettingsBackend callback
// =============================================================================

struct FakeEmojiDownloaderBackend {
    inner: MockSettingsBackend,
    requested_sets: Arc<Mutex<Vec<i32>>>,
    zip_bytes: Result<Vec<u8>, String>,
}

impl FakeEmojiDownloaderBackend {
    fn new(zip_bytes: Result<Vec<u8>, String>) -> (Self, Arc<Mutex<Vec<i32>>>) {
        let requested = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner: MockSettingsBackend::new(),
                requested_sets: Arc::clone(&requested),
                zip_bytes,
            },
            requested,
        )
    }
}

macro_rules! forward_settings_backend {
    () => {
        fn profile(&self) -> tg_app::settings::backend::ProfileInfo {
            self.inner.profile()
        }
        fn update_name(&mut self, first: &str, last: &str) -> Result<(), String> {
            self.inner.update_name(first, last)
        }
        fn update_bio(&mut self, bio: &str) -> Result<(), String> {
            self.inner.update_bio(bio)
        }
        fn check_username(&self, u: &str) -> tg_app::settings::backend::UsernameStatus {
            self.inner.check_username(u)
        }
        fn update_username(&mut self, u: &str) -> Result<(), String> {
            self.inner.update_username(u)
        }
        fn update_birthday(
            &mut self,
            b: Option<tg_app::settings::backend::Birthday>,
        ) -> Result<(), String> {
            self.inner.update_birthday(b)
        }
        fn update_profile_photo(&mut self, color: u8, label: Option<String>) {
            self.inner.update_profile_photo(color, label)
        }
        fn set_phone_hidden(&mut self, hidden: bool) {
            self.inner.set_phone_hidden(hidden)
        }
        fn contacts(&self) -> Vec<tg_app::settings::backend::PeerRef> {
            self.inner.contacts()
        }
        fn privacy_rules(
            &self,
        ) -> std::collections::BTreeMap<
            tg_app::settings::backend::PrivacyKey,
            tg_app::settings::backend::PrivacyRule,
        > {
            self.inner.privacy_rules()
        }
        fn privacy_rule(
            &self,
            key: tg_app::settings::backend::PrivacyKey,
        ) -> tg_app::settings::backend::PrivacyRule {
            self.inner.privacy_rule(key)
        }
        fn set_privacy_rule(
            &mut self,
            key: tg_app::settings::backend::PrivacyKey,
            rule: tg_app::settings::backend::PrivacyRule,
        ) {
            self.inner.set_privacy_rule(key, rule)
        }
        fn global_privacy(&self) -> tg_app::settings::backend::GlobalPrivacySettings {
            self.inner.global_privacy()
        }
        fn set_global_privacy(&mut self, s: tg_app::settings::backend::GlobalPrivacySettings) {
            self.inner.set_global_privacy(s)
        }
        fn account_ttl_days(&self) -> i32 {
            self.inner.account_ttl_days()
        }
        fn set_account_ttl_days(&mut self, days: i32) {
            self.inner.set_account_ttl_days(days)
        }
        fn clear_payment_info(&mut self, ship: bool, cred: bool) {
            self.inner.clear_payment_info(ship, cred)
        }
        fn active_sessions(&self) -> Vec<tg_app::settings::backend::ActiveSession> {
            self.inner.active_sessions()
        }
        fn terminate_session(&mut self, hash: u64) -> bool {
            self.inner.terminate_session(hash)
        }
        fn terminate_all_other_sessions(&mut self) -> usize {
            self.inner.terminate_all_other_sessions()
        }
        fn update_session_permissions(&mut self, hash: u64, calls: bool, secret: bool) {
            self.inner.update_session_permissions(hash, calls, secret)
        }
        fn session_ttl_days(&self) -> i32 {
            self.inner.session_ttl_days()
        }
        fn set_session_ttl_days(&mut self, days: i32) {
            self.inner.set_session_ttl_days(days)
        }
        fn blocked_users(&self) -> Vec<tg_app::settings::backend::PeerRef> {
            self.inner.blocked_users()
        }
        fn block_peer(&mut self, peer_id: i64) {
            self.inner.block_peer(peer_id)
        }
        fn unblock_peer(&mut self, peer_id: i64) {
            self.inner.unblock_peer(peer_id)
        }
        fn cloud_password_state(&self) -> tg_app::settings::backend::CloudPasswordState {
            self.inner.cloud_password_state()
        }
        fn verify_cloud_password(
            &self,
            pw: &str,
        ) -> Result<(), tg_app::settings::backend::CloudPasswordError> {
            self.inner.verify_cloud_password(pw)
        }
        fn set_cloud_password(
            &mut self,
            cur: Option<&str>,
            new_pw: &str,
            hint: &str,
            email: Option<&str>,
        ) -> Result<
            tg_app::settings::backend::CloudPasswordState,
            tg_app::settings::backend::CloudPasswordError,
        > {
            self.inner.set_cloud_password(cur, new_pw, hint, email)
        }
        fn confirm_recovery_email(
            &mut self,
            code: &str,
        ) -> Result<
            tg_app::settings::backend::CloudPasswordState,
            tg_app::settings::backend::CloudPasswordError,
        > {
            self.inner.confirm_recovery_email(code)
        }
        fn cancel_recovery_email(&mut self) -> tg_app::settings::backend::CloudPasswordState {
            self.inner.cancel_recovery_email()
        }
        fn disable_cloud_password(
            &mut self,
            cur: &str,
        ) -> Result<
            tg_app::settings::backend::CloudPasswordState,
            tg_app::settings::backend::CloudPasswordError,
        > {
            self.inner.disable_cloud_password(cur)
        }
        fn scope_notifications(
            &self,
            scope: tg_app::settings::backend::NotifyScope,
        ) -> tg_app::settings::backend::ScopeNotifySettings {
            self.inner.scope_notifications(scope)
        }
        fn set_scope_notifications(
            &mut self,
            scope: tg_app::settings::backend::NotifyScope,
            s: tg_app::settings::backend::ScopeNotifySettings,
        ) {
            self.inner.set_scope_notifications(scope, s)
        }
        fn notification_events(&self) -> tg_app::settings::backend::NotificationEventsSettings {
            self.inner.notification_events()
        }
        fn set_notification_events(
            &mut self,
            e: tg_app::settings::backend::NotificationEventsSettings,
        ) {
            self.inner.set_notification_events(e)
        }
        fn reset_all_notifications(&mut self) {
            self.inner.reset_all_notifications()
        }
        fn auto_download(&self) -> tg_app::settings::backend::AutoDownloadSettings {
            self.inner.auto_download()
        }
        fn set_auto_download(&mut self, s: tg_app::settings::backend::AutoDownloadSettings) {
            self.inner.set_auto_download(s)
        }
        fn sticker_sets(&self) -> Vec<tg_app::settings::backend::StickerSetInfo> {
            self.inner.sticker_sets()
        }
        fn set_sticker_set_archived(&mut self, id: u64, archived: bool) {
            self.inner.set_sticker_set_archived(id, archived)
        }
        fn remove_sticker_set(&mut self, id: u64) {
            self.inner.remove_sticker_set(id)
        }
        fn folders(&self) -> Vec<tg_app::settings::backend::ChatFolder> {
            self.inner.folders()
        }
        fn suggested_folders(&self) -> Vec<tg_app::settings::backend::SuggestedFolder> {
            self.inner.suggested_folders()
        }
        fn save_folder(
            &mut self,
            folder: tg_app::settings::backend::ChatFolder,
        ) -> Result<tg_app::settings::backend::ChatFolder, String> {
            self.inner.save_folder(folder)
        }
        fn delete_folder(&mut self, id: i32) -> bool {
            self.inner.delete_folder(id)
        }
        fn reorder_folders(&mut self, ids: &[i32]) {
            self.inner.reorder_folders(ids)
        }
        fn accept_suggested_folder(
            &mut self,
            idx: usize,
        ) -> Option<tg_app::settings::backend::ChatFolder> {
            self.inner.accept_suggested_folder(idx)
        }
        fn languages(&self) -> Vec<tg_lang::LangPackLanguageInfo> {
            self.inner.languages()
        }
        fn local_storage_stats(&self) -> tg_app::settings::backend::LocalStorageStats {
            self.inner.local_storage_stats()
        }
        fn clear_local_storage(&mut self) -> i64 {
            self.inner.clear_local_storage()
        }
        fn set_local_storage_limits(&mut self, size_limit_bytes: i64, time_limit_days: i32) {
            self.inner
                .set_local_storage_limits(size_limit_bytes, time_limit_days)
        }
        fn request_export(&mut self, req: &tg_app::settings::backend::ExportRequest) -> String {
            self.inner.request_export(req)
        }
    };
}

impl SettingsBackend for FakeEmojiDownloaderBackend {
    forward_settings_backend!();

    fn download_and_switch_emoji_set(
        &mut self,
        manager: &mut EmojiSetsManager,
        set_id: i32,
    ) -> Result<usize, String> {
        self.requested_sets
            .lock()
            .expect("lock requested")
            .push(set_id);
        match &self.zip_bytes {
            Ok(bytes) => manager.finish_download_and_switch(set_id, bytes),
            Err(err) => Err(err.clone()),
        }
    }
}

fn make_ui_harness<'a, S>(
    size: Vec2,
    app: impl FnMut(&mut egui::Ui, &mut S) + 'a,
    state: S,
) -> Harness<'a, S> {
    Harness::builder()
        .with_size(size)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui_state(app, state)
}

#[test]
fn wp37b_settings_chat_emoji_sets_triggers_fake_downloader_and_handles_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let zip_bytes = build_emoji_zip_bytes(&[
        ("config.json", br#"{"version":1,"id":1}"#),
        ("emoji_0.webp", b"RIFFfakewebp"),
    ]);
    let (mut backend, requested) = FakeEmojiDownloaderBackend::new(Ok(zip_bytes));
    let mut core = tg_core::CoreSettings::default();
    let mut global = tg_core::GlobalConfig::default();
    let mut live_theme = LiveThemeColors::build(tg_theme::EmbeddedType::Default, None);
    let colors = LiveThemeColors::build(tg_theme::EmbeddedType::Default, None);

    let init_state = (
        ChatSettingsState {
            emoji_sets_open: true,
            emoji_sets_manager: EmojiSetsManager::new(tmp.path().to_path_buf()),
            ..Default::default()
        },
        None::<String>,
    );

    let mut harness = make_ui_harness(
        Vec2::new(500.0, 2400.0),
        move |ui, (st, last_toast): &mut (ChatSettingsState, Option<String>)| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let out = render_chat_settings_section(
                    ui,
                    st,
                    &mut core,
                    &mut global,
                    &mut live_theme,
                    &mut backend,
                    &colors,
                );
                if let Some(t) = out.toast {
                    *last_toast = Some(t);
                }
            });
        },
        init_state,
    );

    harness.run_steps(2);
    harness.get_by_label("Android").click();
    harness.run_steps(2);

    assert_eq!(
        *requested.lock().expect("lock requested"),
        vec![1],
        "clicking Android in Settings > Chat > Emoji Sets must invoke SettingsBackend callback"
    );
    assert_eq!(harness.state().0.emoji_sets_manager.current_set_id(), 1);
    assert_eq!(
        harness.state().0.emoji_sets_manager.state_for(1),
        EmojiSetState::Active
    );
    assert!(
        harness
            .state()
            .0
            .emoji_sets_manager
            .set_data_path(1)
            .join("config.json")
            .is_file()
    );
    assert_eq!(
        harness.state().1.as_deref(),
        Some("Emoji set switched to Android")
    );

    // Verify failure path transitions out of `Loading` into `Failed`.
    let tmp_err = tempfile::tempdir().expect("tempdir err");
    let (mut fail_backend, fail_requested) =
        FakeEmojiDownloaderBackend::new(Err("network offline".to_owned()));
    let mut core2 = tg_core::CoreSettings::default();
    let mut global2 = tg_core::GlobalConfig::default();
    let mut live_theme2 = LiveThemeColors::build(tg_theme::EmbeddedType::Default, None);
    let colors2 = LiveThemeColors::build(tg_theme::EmbeddedType::Default, None);

    let err_state = (
        ChatSettingsState {
            emoji_sets_open: true,
            emoji_sets_manager: EmojiSetsManager::new(tmp_err.path().to_path_buf()),
            ..Default::default()
        },
        None::<String>,
    );
    let mut err_harness = make_ui_harness(
        Vec2::new(500.0, 2400.0),
        move |ui, (st, last_toast): &mut (ChatSettingsState, Option<String>)| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let out = render_chat_settings_section(
                    ui,
                    st,
                    &mut core2,
                    &mut global2,
                    &mut live_theme2,
                    &mut fail_backend,
                    &colors2,
                );
                if let Some(t) = out.toast {
                    *last_toast = Some(t);
                }
            });
        },
        err_state,
    );
    err_harness.run_steps(2);
    err_harness.get_by_label("Twemoji").click();
    err_harness.run_steps(2);

    assert_eq!(
        *fail_requested.lock().expect("lock fail_requested"),
        vec![2]
    );
    assert_eq!(err_harness.state().0.emoji_sets_manager.current_set_id(), 0);
    assert_eq!(
        err_harness.state().0.emoji_sets_manager.state_for(2),
        EmojiSetState::Failed("network offline".to_owned())
    );
    assert!(
        err_harness
            .state()
            .1
            .as_deref()
            // tdesktop marks the set "Failed" (`storage/storage_cloud_blob.cpp:96-97`, TGB-2957).
            .is_some_and(|t| t == tg_lang::tr::lng_attach_failed())
    );
}

#[test]
fn wp37b_settings_chat_emoji_sets_live_session_runs_load_and_switch_to_over_fake_rpc() {
    let (mut live, fake, tmp) = make_live_session();
    let zip_bytes = build_emoji_zip_bytes(&[
        ("config.json", br#"{"version":1,"id":2}"#),
        ("emoji_0.webp", b"RIFFtwemojiwebp"),
    ]);

    // Enqueue `@tdesktop_emoji` resolution -> post #3224 (Twemoji) -> `upload.getFile` ZIP bytes.
    let emoji_channel = tl_enums::Chat::Channel(tl_types::Channel {
        creator: false,
        left: false,
        broadcast: true,
        verified: true,
        megagroup: false,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: true,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id: 500_002,
        access_hash: Some(900_002),
        title: "TDesktop Emoji".to_owned(),
        username: Some("tdesktop_emoji".to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_730_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    });
    let resolved_peer =
        tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 500_002,
            }),
            chats: vec![emoji_channel],
            users: Vec::new(),
        });
    let archive_doc = tl_enums::Document::Document(tl_types::Document {
        id: 77_3224,
        access_hash: 88_3224,
        file_reference: vec![1, 2, 3],
        date: 1_730_000_000,
        mime_type: "application/zip".to_owned(),
        size: zip_bytes.len() as i64,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl_enums::DocumentAttribute::Filename(
            tl_types::DocumentAttributeFilename {
                file_name: "twemoji.zip".to_owned(),
            },
        )],
    });
    let post_msg = tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: true,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        guestchat_via_from: None,
        id: 3224,
        from_id: None,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel {
            channel_id: 500_002,
        }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date: 1_730_000_000,
        message: String::new(),
        media: Some(tl_enums::MessageMedia::Document(
            tl_types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: false,
                round: false,
                voice: false,
                document: Some(archive_doc),
                alt_documents: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: None,
            },
        )),
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    });
    let chan_msgs =
        tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
            inexact: false,
            pts: 10,
            count: 1,
            offset_id_offset: None,
            messages: vec![post_msg],
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        });

    fake.set_handler(move |_dc, ctor, _bytes| {
        if ctor == tl_fns::messages::GetAllStickers::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::messages::AllStickers::AllStickers(
                tl_types::messages::AllStickers {
                    hash: 0,
                    sets: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::messages::GetArchivedStickers::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::messages::ArchivedStickers::ArchivedStickers(
                tl_types::messages::ArchivedStickers {
                    count: 0,
                    sets: Vec::new(),
                },
            )
            .to_bytes()));
        }
        if ctor == tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID {
            return Some(Ok(resolved_peer.to_bytes()));
        }
        if ctor == tl_fns::channels::GetMessages::CONSTRUCTOR_ID {
            return Some(Ok(chan_msgs.to_bytes()));
        }
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FileUnknown,
                mtime: 1_730_000_000,
                bytes: zip_bytes.clone(),
            })
            .to_bytes()));
        }
        None
    });

    let emoji_root = tmp.path().join("emoji_sets");
    let mut core = tg_core::CoreSettings::default();
    let mut global = tg_core::GlobalConfig::default();
    let mut live_theme = LiveThemeColors::build(tg_theme::EmbeddedType::Default, None);
    let colors = LiveThemeColors::build(tg_theme::EmbeddedType::Default, None);

    let init_state = (
        ChatSettingsState {
            emoji_sets_open: true,
            emoji_sets_manager: EmojiSetsManager::new(emoji_root),
            ..Default::default()
        },
        None::<String>,
    );

    let mut harness = make_ui_harness(
        Vec2::new(500.0, 2400.0),
        move |ui, (st, last_toast): &mut (ChatSettingsState, Option<String>)| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let out = render_chat_settings_section(
                    ui,
                    st,
                    &mut core,
                    &mut global,
                    &mut live_theme,
                    &mut live,
                    &colors,
                );
                if let Some(t) = out.toast {
                    *last_toast = Some(t);
                }
            });
        },
        init_state,
    );

    harness.run_steps(2);
    harness.get_by_label("Twemoji").click();
    harness.run_steps(2);

    assert_eq!(harness.state().0.emoji_sets_manager.current_set_id(), 2);
    assert_eq!(
        harness.state().0.emoji_sets_manager.state_for(2),
        EmojiSetState::Active
    );
    assert_eq!(
        harness.state().1.as_deref(),
        Some("Emoji set switched to Twemoji")
    );
    assert_eq!(
        fake.recorded_calls(tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_calls(tl_fns::channels::GetMessages::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.recorded_calls(tl_fns::upload::GetFile::CONSTRUCTOR_ID)
            .len(),
        1
    );
}

// =============================================================================
// 3. `updateEmojiGameInfo` forwarded through live update pump
// =============================================================================

#[test]
fn wp37b_update_emoji_game_info_forwarded_from_live_update_pump() {
    let (live, fake, tmp) = make_live_session();
    let settings_path = tmp.path().join("settings.json");
    let _workspace = LiveActiveWorkspace::new_headless(
        live.clone(),
        ShellSettings::default(),
        settings_path,
        false,
    );

    assert!(cached_dice_game_options(&live).is_none());
    let gen_before = live.bridge_state().generation();

    // 1. Feed `UpdateShort(Update::EmojiGameInfo(EmojiGameDiceInfo))` through the live update pump.
    fake.send_update(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::EmojiGameInfo(tl_types::UpdateEmojiGameInfo {
            info: tl_enums::messages::EmojiGameInfo::EmojiGameDiceInfo(
                tl_types::messages::EmojiGameDiceInfo {
                    game_hash: "pump_seed_hash_01".to_owned(),
                    prev_stake: 500_000_000,
                    current_streak: 4,
                    params: vec![100, 200, 300, 400, 500, 600, 2500],
                    plays_left: Some(9),
                },
            ),
        }),
        date: 1_735_200_000,
    }));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = None;
    while Instant::now() < deadline {
        if let Some(opts) = cached_dice_game_options(&live)
            && opts.seed_hash == "pump_seed_hash_01"
        {
            got = Some(opts);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let opts = got.expect("live update pump should forward UpdateShort(UpdateEmojiGameInfo)");
    assert!(opts.is_available());
    assert_eq!(opts.previous_stake_nano_ton, 500_000_000);
    assert_eq!(opts.milli_rewards, [100, 200, 300, 400, 500, 600]);
    assert_eq!(opts.jackpot_milli_reward, 2500);
    assert_eq!(opts.current_streak, 4);
    assert_eq!(opts.plays_left, 9);
    assert!(
        live.bridge_state().generation() > gen_before,
        "updateEmojiGameInfo must bump UI bridge generation"
    );

    // 2. Feed `Updates::Updates` carrying `EmojiGameUnavailable` through the pump.
    fake.send_update(tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::EmojiGameInfo(
            tl_types::UpdateEmojiGameInfo {
                info: tl_enums::messages::EmojiGameInfo::EmojiGameUnavailable,
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_735_200_005,
        seq: 1,
    }));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut became_unavailable = false;
    while Instant::now() < deadline {
        if let Some(cur) = cached_dice_game_options(&live)
            && !cur.is_available()
        {
            became_unavailable = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        became_unavailable,
        "live update pump should forward Updates(UpdateEmojiGameInfo(Unavailable))"
    );

    // 3. Feed `Updates::Combined` carrying a fresh active `EmojiGameDiceInfo`.
    fake.send_update(tl_enums::Updates::Combined(tl_types::UpdatesCombined {
        updates: vec![tl_enums::Update::EmojiGameInfo(
            tl_types::UpdateEmojiGameInfo {
                info: tl_enums::messages::EmojiGameInfo::EmojiGameDiceInfo(
                    tl_types::messages::EmojiGameDiceInfo {
                        game_hash: "pump_combined_hash_02".to_owned(),
                        prev_stake: 1_000_000_000,
                        current_streak: 7,
                        params: vec![150, 250, 350, 450, 550, 650, 3000],
                        plays_left: Some(2),
                    },
                ),
            },
        )],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_735_200_010,
        seq_start: 2,
        seq: 2,
    }));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut combined_ok = false;
    while Instant::now() < deadline {
        if let Some(cur) = cached_dice_game_options(&live)
            && cur.seed_hash == "pump_combined_hash_02"
            && cur.current_streak == 7
            && cur.plays_left == 2
        {
            combined_ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        combined_ok,
        "live update pump should forward UpdatesCombined(UpdateEmojiGameInfo)"
    );
}
