//! High-level 1:1 VoIP [`MediaEngine`] and [`CallMediaEngine`] trait driven by `tg-calls`.
//!
//! Port of `TelegramMessenger/tgcalls`:
//! - `tgcalls/Instance.h` (`Instance`, `Descriptor`, `FinalState`)
//! - `tgcalls/v2/InstanceV2Impl.cpp` (lifecycle, signaling orchestration, bitrate & signal bars)
//! - `crates/tg-calls/docs/media-engine.md` §7 (`CallMediaEngine` crate boundary)

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::Rng;
use tg_audio::device::{DEFAULT_DEVICE_ID, DeviceInfo, DeviceType};
use tg_calls::{
    ControllerDescriptor, ControllerState, RemoteAudioState, RemoteBatteryState,
    SIGNAL_BAR_STARTING, VideoState,
};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

static ACTIVE_MEDIA_TASKS: AtomicUsize = AtomicUsize::new(0);

/// Number of currently running 1:1 `MediaEngine` background tasks in this process.
#[must_use]
pub fn active_media_task_count() -> usize {
    ACTIVE_MEDIA_TASKS.load(Ordering::Acquire)
}

struct MediaTaskCounterGuard;

impl MediaTaskCounterGuard {
    fn enter() -> Self {
        ACTIVE_MEDIA_TASKS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for MediaTaskCounterGuard {
    fn drop(&mut self) {
        ACTIVE_MEDIA_TASKS.fetch_sub(1, Ordering::AcqRel);
    }
}

use crate::audio_io::{AudioBackendKind, VirtualAudioHandle, VoipAudioIo};
use crate::dsp::{DEFAULT_VOICE_BITRATE_BPS, VoiceDsp, VoiceDspConfig, VoiceOpusEncoder};
use crate::error::{VoipError, VoipResult};
use crate::jitter::{AdaptiveJitterBuffer, JitterBufferConfig, VOIP_FRAME_MS, VOIP_FRAME_SAMPLES};
use crate::quality::{
    DataSaving, NetworkType, QualityController, TrafficStats, VideoQualityProfile,
    VoipStatsSnapshot,
};
use crate::rtp::{OPUS_RTP_PAYLOAD_TYPE, RtpHeader, VoipPacket};
use crate::signaling::{
    SignalingChannel, SignalingDtlsFingerprint, SignalingMessage, SignalingVideoState,
};
use crate::transport::{TransportWireMode, VoipTransport};
use crate::turn::{SCTP_PPID_BINARY, SCTP_PPID_STRING, SctpDataChannelRouter};
use crate::video::{
    CameraCaptureConfig, RgbaVideoFrame, ScreenCaptureConfig, VideoCaptureSource, VideoDecoder,
    VideoEncoder, VideoEncoderConfig, create_h264_decoder, create_h264_encoder, normalize_rotation,
    open_camera, open_screen_capture,
};
use crate::video_rtp::{H264RtpDepacketizer, H264RtpPacketizer};

/// Events emitted by the media engine for `tg_calls::Call` and the UI layer.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaEngineEvent {
    /// Controller lifecycle transition -> `Call::handle_controller_state_change`.
    StateChanged(ControllerState),
    /// Signal quality bar count (`0..=4`) -> `Call::handle_controller_bar_count_change`.
    SignalBarsChanged(i32),
    /// Encrypted signaling blob -> `Call::emit_signaling_data` (`phone.sendSignalingData`).
    SignalingData(Vec<u8>),
    /// Remote participant mute/video update -> `Call::update_remote_media_state`.
    RemoteMediaStateChanged {
        /// Remote microphone state.
        audio: RemoteAudioState,
        /// Remote camera video state.
        video: VideoState,
    },
    /// Remote participant low-battery update -> `Call::update_remote_battery_state`.
    RemoteBatteryChanged(RemoteBatteryState),
    /// Local or remote RMS audio level (`0.0..=1.0`) for UI speaking indicator.
    AudioLevelChanged(f32),
    /// Preferred Telegram relay `endpoint_id` updated (0 for P2P/none).
    PreferredRelayChanged(i64),
    /// Decoded remote video frame in RGBA8 with rotation applied, ready for `tg-app`.
    RemoteVideoFrame(RgbaVideoFrame),
    /// Remote video rotation changed (`0`, `90`, `180`, `270` degrees clockwise).
    RemoteVideoRotationChanged(u16),
    /// Remote screencast state changed.
    RemoteScreencastStateChanged(VideoState),
    /// Remote peer requested a keyframe (RTCP PLI/FIR).
    KeyframeRequested,
    /// Application message delivered over an SCTP data channel (`VoipPacket::Sctp`).
    DataChannelMessage {
        /// SCTP stream ID.
        stream_id: u16,
        /// Data channel label (e.g. `"data"`).
        label: String,
        /// Payload Protocol Identifier (`SCTP_PPID_STRING` or `SCTP_PPID_BINARY`).
        ppid: u32,
        /// Message payload bytes.
        payload: Vec<u8>,
    },
}

/// Final telemetry and debug log returned when stopping a call (`tgcalls::FinalState`).
#[derive(Debug, Clone, Default)]
pub struct FinalCallState {
    /// JSON debug log string for `phone.saveCallDebug`.
    pub debug_log: String,
    /// Cumulative traffic byte counts split by Wi-Fi / mobile.
    pub traffic_stats: TrafficStats,
    /// Complete final stats snapshot.
    pub stats: VoipStatsSnapshot,
    /// Preferred relay connection ID for `phone.discardCall`.
    pub preferred_relay_id: i64,
    /// Whether the UI should prompt the user to rate the call.
    pub is_rating_suggested: bool,
}

/// Trait boundary matching `crates/tg-calls/docs/media-engine.md` §7.
pub trait CallMediaEngine: Send {
    /// Start the media engine with the confirmed [`ControllerDescriptor`] from `tg-calls`.
    fn start(&mut self, descriptor: ControllerDescriptor) -> VoipResult<()>;

    /// Deliver an incoming `updatePhoneCallSignalingData` payload from `tg-calls`.
    fn receive_signaling_data(&mut self, data: Vec<u8>);

    /// Mute or unmute local microphone capture (and notify the remote peer via `MediaState`).
    fn set_muted(&mut self, muted: bool);

    /// Switch both input and output audio devices (`"default"` follows OS default).
    fn set_audio_devices(&mut self, input_id: &str, output_id: &str);

    /// Switch microphone input device (`"default"` follows OS default).
    fn set_audio_input_device(&mut self, input_id: &str);

    /// Switch speaker output device (`"default"` follows OS default).
    fn set_audio_output_device(&mut self, output_id: &str);

    /// Update active OS network link type (`WiFi`, `Lte`, etc.) for bitrate adaptation.
    fn set_network_type(&mut self, network_type: NetworkType);

    /// Non-blocking poll for the next [`MediaEngineEvent`].
    fn poll_event(&mut self) -> Option<MediaEngineEvent>;

    /// Stop the engine and return its [`FinalCallState`] (including `phone.saveCallDebug` JSON).
    fn stop(&mut self) -> FinalCallState;
}

/// Configuration for creating a [`MediaEngine`].
#[derive(Debug, Clone)]
pub struct MediaEngineConfig {
    /// Hardware `cpal` WASAPI or headless virtual PCM ring buffers.
    pub audio_backend: AudioBackendKind,
    /// Virtual audio ring buffer capacity in samples (used only when `Virtual`).
    pub virtual_ring_capacity: usize,
    /// Voice DSP (WebRTC AEC3 + NS + AGC2 + HPF) configuration.
    pub dsp: VoiceDspConfig,
    /// Adaptive jitter buffer settings.
    pub jitter: JitterBufferConfig,
    /// Encrypted transport wire mode (`MtProtoRaw` or `LegacyMessage`).
    pub wire_mode: TransportWireMode,
    /// UDP socket local bind address (`0.0.0.0:0` for production, `127.0.0.1:0` for loopback).
    pub bind_addr: SocketAddr,
    /// Initial network link type.
    pub initial_network_type: NetworkType,
    /// Data-saving bitrate cap mode.
    pub data_saving: DataSaving,
    /// Initial preferred input device ID (`"default"`).
    pub input_device_id: String,
    /// Initial preferred output device ID (`"default"`).
    pub output_device_id: String,
    /// Quality/keepalive tick interval in milliseconds (default 1000 ms).
    pub quality_tick_ms: u64,
}

impl Default for MediaEngineConfig {
    fn default() -> Self {
        Self {
            audio_backend: AudioBackendKind::Hardware,
            virtual_ring_capacity: VOIP_FRAME_SAMPLES * 250, // 5 seconds for tests
            dsp: VoiceDspConfig::default(),
            jitter: JitterBufferConfig::default(),
            wire_mode: TransportWireMode::MtProtoRaw,
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            initial_network_type: NetworkType::WiFi,
            data_saving: DataSaving::Never,
            input_device_id: DEFAULT_DEVICE_ID.to_string(),
            output_device_id: DEFAULT_DEVICE_ID.to_string(),
            quality_tick_ms: 1_000,
        }
    }
}

enum EngineCommand {
    ReceiveSignaling(Vec<u8>),
    SetMuted(bool),
    SetInputDevice(String),
    SetOutputDevice(String),
    SetInputVolume(f32),
    SetOutputVolume(f32),
    SetNetworkType(NetworkType),
    SetDataSaving(DataSaving),
    SetLowBattery(bool),
    StartVideo {
        source: Box<dyn VideoCaptureSource>,
        encoder: Box<dyn VideoEncoder>,
        is_screencast: bool,
    },
    StopVideo,
    SetVideoPaused(bool),
    SetVideoRotation(u16),
    SetVideoDecoder(Box<dyn VideoDecoder>),
    RequestRemoteKeyframe,
    SendDataChannel {
        stream_id: u16,
        label: String,
        ppid: u32,
        payload: Vec<u8>,
    },
    Stop(oneshot::Sender<FinalCallState>),
}

/// 1:1 VoIP media engine implementing [`CallMediaEngine`] over Tokio UDP + `cpal` +
/// WebRTC AEC3/NS/AGC2 + `opus-pure` + MTProto-2.0 AES-256-CTR.
pub struct MediaEngine {
    config: MediaEngineConfig,
    runtime: Handle,
    cmd_tx: Option<mpsc::UnboundedSender<EngineCommand>>,
    event_rx: Option<mpsc::UnboundedReceiver<MediaEngineEvent>>,
    task: Option<tokio::task::JoinHandle<()>>,
    teardown_task: Option<tokio::task::JoinHandle<()>>,
    virtual_audio: Option<Arc<VirtualAudioHandle>>,
    shared_stats: Arc<Mutex<VoipStatsSnapshot>>,
    shared_debug_log: Arc<Mutex<String>>,
    shared_established: Arc<AtomicBool>,
    shared_input_device: Arc<Mutex<String>>,
    shared_output_device: Arc<Mutex<String>>,
    last_final_state: Option<FinalCallState>,
}

impl MediaEngine {
    /// Create a new idle [`MediaEngine`] bound to the given Tokio runtime `handle`.
    #[must_use]
    pub fn new(config: MediaEngineConfig, runtime: Handle) -> Self {
        let input_dev = config.input_device_id.clone();
        let output_dev = config.output_device_id.clone();
        Self {
            config,
            runtime,
            cmd_tx: None,
            event_rx: None,
            task: None,
            teardown_task: None,
            virtual_audio: None,
            shared_stats: Arc::new(Mutex::new(VoipStatsSnapshot::default())),
            shared_debug_log: Arc::new(Mutex::new(String::new())),
            shared_established: Arc::new(AtomicBool::new(false)),
            shared_input_device: Arc::new(Mutex::new(input_dev)),
            shared_output_device: Arc::new(Mutex::new(output_dev)),
            last_final_state: None,
        }
    }

    /// Create a new idle [`MediaEngine`] using the current Tokio runtime handle.
    pub fn with_current_runtime(config: MediaEngineConfig) -> VoipResult<Self> {
        let handle = Handle::try_current()
            .map_err(|e| VoipError::InvalidState(format!("no active Tokio runtime: {e}")))?;
        Ok(Self::new(config, handle))
    }

    /// Enumerate available OS capture or playback devices via `tg-audio`.
    pub fn list_audio_devices(device_type: DeviceType) -> VoipResult<Vec<DeviceInfo>> {
        VoipAudioIo::list_devices(device_type)
    }

    /// Access the virtual audio PCM handle when configured with [`AudioBackendKind::Virtual`].
    #[must_use]
    pub fn virtual_audio(&self) -> Option<Arc<VirtualAudioHandle>> {
        self.virtual_audio.clone()
    }

    /// Set microphone input gain (`0.0..=2.0`).
    pub fn set_input_volume(&self, volume: f32) {
        self.send_cmd(EngineCommand::SetInputVolume(volume));
    }

    /// Set speaker output gain (`0.0..=2.0`).
    pub fn set_output_volume(&self, volume: f32) {
        self.send_cmd(EngineCommand::SetOutputVolume(volume));
    }

    /// Update data-saving bitrate policy (`DataSaving::Never`, `Mobile`, `Always`).
    pub fn set_data_saving(&self, mode: DataSaving) {
        self.send_cmd(EngineCommand::SetDataSaving(mode));
    }

    /// Notify remote peer of local low-battery state via encrypted `MediaState` signaling.
    pub fn set_is_low_battery(&self, is_low: bool) {
        self.send_cmd(EngineCommand::SetLowBattery(is_low));
    }

    /// Start sending video using a custom [`VideoCaptureSource`] and [`VideoEncoder`].
    pub fn start_video(
        &self,
        source: Box<dyn VideoCaptureSource>,
        encoder: Box<dyn VideoEncoder>,
        is_screencast: bool,
    ) {
        self.send_cmd(EngineCommand::StartVideo {
            source,
            encoder,
            is_screencast,
        });
    }

    /// Open a Windows Media Foundation camera and H.264 encoder and begin sending 1:1 video.
    pub fn start_camera_video(
        &self,
        device_id: &str,
        config: CameraCaptureConfig,
    ) -> VoipResult<()> {
        let source = open_camera(device_id, config)?;
        let enc_cfg = VideoEncoderConfig {
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate_bps: VideoQualityProfile::for_bitrate(750_000).bitrate_bps,
            gop_size: config.fps.max(1) * 2,
        };
        let encoder = create_h264_encoder(enc_cfg)?;
        self.start_video(source, encoder, false);
        Ok(())
    }

    /// Open DXGI desktop screen capture and an H.264 encoder and begin screen sharing.
    pub fn start_screen_share(&self, config: ScreenCaptureConfig) -> VoipResult<()> {
        let fps = config.fps.max(1);
        let source = open_screen_capture(config)?;
        let enc_cfg = VideoEncoderConfig {
            width: 1280,
            height: 720,
            fps,
            bitrate_bps: 1_200_000,
            gop_size: fps * 2,
        };
        let encoder = create_h264_encoder(enc_cfg)?;
        self.start_video(source, encoder, true);
        Ok(())
    }

    /// Stop sending local camera or screen-share video.
    pub fn stop_video(&self) {
        self.send_cmd(EngineCommand::StopVideo);
    }

    /// Pause or resume local video capture (`VideoState::Paused` / `VideoState::Active`).
    pub fn set_video_paused(&self, paused: bool) {
        self.send_cmd(EngineCommand::SetVideoPaused(paused));
    }

    /// Update local video orientation (`0`, `90`, `180`, `270` degrees clockwise) via `MediaState`.
    pub fn set_video_rotation(&self, rotation: u16) {
        self.send_cmd(EngineCommand::SetVideoRotation(normalize_rotation(
            rotation,
        )));
    }

    /// Inject a [`VideoDecoder`] for incoming H.264 streams (otherwise initialized lazily via MF).
    pub fn set_video_decoder(&self, decoder: Box<dyn VideoDecoder>) {
        self.send_cmd(EngineCommand::SetVideoDecoder(decoder));
    }

    /// Ask the remote peer to emit an immediate H.264 IDR keyframe via RTCP PLI and FIR.
    pub fn request_remote_keyframe(&self) {
        self.send_cmd(EngineCommand::RequestRemoteKeyframe);
    }

    /// Async receive of the next [`MediaEngineEvent`].
    pub async fn recv_event(&mut self) -> Option<MediaEngineEvent> {
        self.event_rx.as_mut()?.recv().await
    }

    /// Snapshot of live telemetry statistics.
    #[must_use]
    pub fn stats(&self) -> VoipStatsSnapshot {
        self.shared_stats
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Current JSON debug log formatted for `phone.saveCallDebug`.
    #[must_use]
    pub fn debug_log(&self) -> String {
        self.shared_debug_log
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Preferred Telegram relay `endpoint_id` (`0` if P2P or none).
    #[must_use]
    pub fn preferred_relay_id(&self) -> i64 {
        self.stats().preferred_relay_id
    }

    /// Currently applied audio input device ID.
    #[must_use]
    pub fn input_device_id(&self) -> String {
        self.shared_input_device
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Currently applied audio output device ID.
    #[must_use]
    pub fn output_device_id(&self) -> String {
        self.shared_output_device
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    fn snapshot_final_state(&self) -> FinalCallState {
        let stats = self.stats();
        let debug_log = self.debug_log();
        let established = self.shared_established.load(Ordering::Acquire);
        FinalCallState {
            debug_log,
            traffic_stats: stats.traffic,
            preferred_relay_id: stats.preferred_relay_id,
            is_rating_suggested: established && stats.packets_received > 0,
            stats,
        }
    }

    /// Asynchronously stop the running media engine and await its [`FinalCallState`].
    ///
    /// This is the preferred stop API whenever the caller is in an async context and wants to
    /// await complete graceful teardown and the exact final telemetry snapshot.
    pub async fn stop_async(&mut self) -> FinalCallState {
        if self.cmd_tx.is_none() && self.task.is_none() {
            if let Some(mut teardown) = self.teardown_task.take()
                && tokio::time::timeout(Duration::from_secs(2), &mut teardown)
                    .await
                    .is_err()
            {
                teardown.abort();
                let _ = teardown.await;
            }
            return self
                .last_final_state
                .clone()
                .unwrap_or_else(|| self.snapshot_final_state());
        }

        let mut received_final = None;
        if let Some(tx) = self.cmd_tx.take() {
            let (reply_tx, reply_rx) = oneshot::channel();
            if tx.send(EngineCommand::Stop(reply_tx)).is_ok()
                && let Ok(Ok(state)) = tokio::time::timeout(Duration::from_secs(2), reply_rx).await
            {
                received_final = Some(state);
            }
        }
        if let Some(mut handle) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(2), &mut handle)
                .await
                .is_err()
        {
            handle.abort();
            let _ = handle.await;
        }
        if let Some(mut teardown) = self.teardown_task.take()
            && tokio::time::timeout(Duration::from_secs(2), &mut teardown)
                .await
                .is_err()
        {
            teardown.abort();
            let _ = teardown.await;
        }
        let final_state = received_final.unwrap_or_else(|| self.snapshot_final_state());
        self.last_final_state = Some(final_state.clone());
        final_state
    }

    /// Send a UTF-8 text message over an SCTP data channel (`VoipPacket::Sctp`).
    pub fn send_data_channel_text(&self, label: impl Into<String>, text: impl Into<String>) {
        self.send_cmd(EngineCommand::SendDataChannel {
            stream_id: 0,
            label: label.into(),
            ppid: SCTP_PPID_STRING,
            payload: text.into().into_bytes(),
        });
    }

    /// Send raw binary bytes over an SCTP data channel (`VoipPacket::Sctp`).
    pub fn send_data_channel_bytes(
        &self,
        stream_id: u16,
        label: impl Into<String>,
        payload: Vec<u8>,
    ) {
        self.send_cmd(EngineCommand::SendDataChannel {
            stream_id,
            label: label.into(),
            ppid: SCTP_PPID_BINARY,
            payload,
        });
    }

    fn send_cmd(&self, cmd: EngineCommand) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(cmd);
        }
    }
}

impl CallMediaEngine for MediaEngine {
    fn start(&mut self, descriptor: ControllerDescriptor) -> VoipResult<()> {
        if self.task.is_some() {
            return Err(VoipError::InvalidState(
                "media engine already started".into(),
            ));
        }

        let (audio_io, virtual_handle) = match self.config.audio_backend {
            AudioBackendKind::Hardware => (
                VoipAudioIo::open_hardware(
                    &self.config.input_device_id,
                    &self.config.output_device_id,
                )?,
                None,
            ),
            AudioBackendKind::Virtual => {
                let (io, handle) = VoipAudioIo::open_virtual(self.config.virtual_ring_capacity);
                (io, Some(handle))
            }
        };
        self.virtual_audio = virtual_handle;

        let dsp = VoiceDsp::new(self.config.dsp)?;
        let encoder = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS)?;
        let jitter = AdaptiveJitterBuffer::new(self.config.jitter)?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Allocate fresh shared state for this run so any concurrently tearing-down prior run
        // cannot overwrite the new run's stats, debug log, or device IDs.
        self.shared_stats = Arc::new(Mutex::new(VoipStatsSnapshot::default()));
        self.shared_debug_log = Arc::new(Mutex::new(String::new()));
        self.shared_established = Arc::new(AtomicBool::new(false));
        self.shared_input_device = Arc::new(Mutex::new(audio_io.input_device_id().to_string()));
        self.shared_output_device = Arc::new(Mutex::new(audio_io.output_device_id().to_string()));
        self.last_final_state = None;

        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);

        let config = self.config.clone();
        let shared_stats = Arc::clone(&self.shared_stats);
        let shared_debug_log = Arc::clone(&self.shared_debug_log);
        let shared_established = Arc::clone(&self.shared_established);
        let shared_input_device = Arc::clone(&self.shared_input_device);
        let shared_output_device = Arc::clone(&self.shared_output_device);

        let task = self.runtime.spawn(async move {
            run_media_loop(
                descriptor,
                config,
                audio_io,
                dsp,
                encoder,
                jitter,
                cmd_rx,
                event_tx,
                shared_stats,
                shared_debug_log,
                shared_established,
                shared_input_device,
                shared_output_device,
            )
            .await;
        });
        self.task = Some(task);
        Ok(())
    }

    fn receive_signaling_data(&mut self, data: Vec<u8>) {
        self.send_cmd(EngineCommand::ReceiveSignaling(data));
    }

    fn set_muted(&mut self, muted: bool) {
        self.send_cmd(EngineCommand::SetMuted(muted));
    }

    fn set_audio_devices(&mut self, input_id: &str, output_id: &str) {
        self.set_audio_input_device(input_id);
        self.set_audio_output_device(output_id);
    }

    fn set_audio_input_device(&mut self, input_id: &str) {
        self.send_cmd(EngineCommand::SetInputDevice(input_id.to_string()));
    }

    fn set_audio_output_device(&mut self, output_id: &str) {
        self.send_cmd(EngineCommand::SetOutputDevice(output_id.to_string()));
    }

    fn set_network_type(&mut self, network_type: NetworkType) {
        self.send_cmd(EngineCommand::SetNetworkType(network_type));
    }

    fn poll_event(&mut self) -> Option<MediaEngineEvent> {
        self.event_rx.as_mut()?.try_recv().ok()
    }

    fn stop(&mut self) -> FinalCallState {
        // Non-blocking, runtime-safe stop: safe from inside a Tokio runtime (current_thread or
        // multi_thread), from a plain OS thread, and from the egui UI thread. Never calls
        // `Handle::block_on` and never blocks the caller.
        if self.cmd_tx.is_none() && self.task.is_none() {
            return self
                .last_final_state
                .clone()
                .unwrap_or_else(|| self.snapshot_final_state());
        }

        let mut immediate_final = None;
        if let Some(tx) = self.cmd_tx.take() {
            let (reply_tx, mut reply_rx) = oneshot::channel();
            let _ = tx.send(EngineCommand::Stop(reply_tx));
            if let Ok(state) = reply_rx.try_recv() {
                immediate_final = Some(state);
            }
        }

        if let Some(mut handle) = self.task.take() {
            // Finish graceful teardown on the engine's own Tokio runtime with a bounded abort
            // watchdog so neither the caller thread nor background tasks/threads can hang or leak.
            let teardown = self.runtime.spawn(async move {
                if tokio::time::timeout(Duration::from_secs(2), &mut handle)
                    .await
                    .is_err()
                {
                    handle.abort();
                    let _ = handle.await;
                }
            });
            self.teardown_task = Some(teardown);
        }

        let final_state = immediate_final.unwrap_or_else(|| self.snapshot_final_state());
        self.last_final_state = Some(final_state.clone());
        final_state
    }
}

impl Drop for MediaEngine {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_media_loop(
    descriptor: ControllerDescriptor,
    config: MediaEngineConfig,
    mut audio_io: VoipAudioIo,
    mut dsp: VoiceDsp,
    mut encoder: VoiceOpusEncoder,
    mut jitter: AdaptiveJitterBuffer,
    mut cmd_rx: mpsc::UnboundedReceiver<EngineCommand>,
    event_tx: mpsc::UnboundedSender<MediaEngineEvent>,
    shared_stats: Arc<Mutex<VoipStatsSnapshot>>,
    shared_debug_log: Arc<Mutex<String>>,
    shared_established: Arc<AtomicBool>,
    shared_input_device: Arc<Mutex<String>>,
    shared_output_device: Arc<Mutex<String>>,
) {
    let _task_counter = MediaTaskCounterGuard::enter();
    let start_instant = Instant::now();
    let elapsed_ms = || start_instant.elapsed().as_millis() as u64;

    let mut transport = match VoipTransport::bind(
        config.bind_addr,
        descriptor.encryption_key,
        descriptor.is_outgoing,
        descriptor.enable_p2p,
        &descriptor.endpoints,
        &descriptor.rtc_servers,
        config.wire_mode,
    )
    .await
    {
        Ok(t) => t,
        Err(err) => {
            tracing::error!("tg-voip transport bind failed: {err}");
            let _ = event_tx.send(MediaEngineEvent::StateChanged(ControllerState::Failed(
                "ERROR_TRANSPORT".into(),
            )));
            return;
        }
    };

    let mut signaling = SignalingChannel::new(descriptor.encryption_key, descriptor.is_outgoing);
    let mut quality = QualityController::new(config.initial_network_type, config.data_saving);
    let mut local_muted = false;
    let mut local_low_battery = false;
    let mut local_video_state = SignalingVideoState::Inactive;
    let mut local_screencast_state = SignalingVideoState::Inactive;
    let mut local_video_rotation: u16 = 0;
    let mut remote_video_rotation: u16 = 0;
    let mut remote_screencast_state = SignalingVideoState::Inactive;

    let audio_ssrc: u32 = rand::thread_rng().r#gen::<u32>().max(1);
    let video_ssrc: u32 = rand::thread_rng().r#gen::<u32>().max(2);
    let mut remote_video_ssrc: u32 = 0;
    let mut fir_seq_nr: u8 = 0;
    let mut last_pli_sent_ms: u64 = 0;

    let mut video_capture: Option<Box<dyn VideoCaptureSource>> = None;
    let mut video_encoder: Option<Box<dyn VideoEncoder>> = None;
    let mut video_decoder: Option<Box<dyn VideoDecoder>> = None;
    let mut video_decoder_init_failed = false;
    let mut video_is_screencast = false;
    let mut video_paused = false;
    let mut force_next_keyframe = true;
    let mut last_video_profile = quality.video_profile();
    let mut last_video_send_ms: u64 = 0;
    let mut h264_packetizer = H264RtpPacketizer::new(video_ssrc);
    let mut h264_depacketizer = H264RtpDepacketizer::new();

    let mut rtp_seq: u16 = 0;
    let mut rtp_ts: u32 = 0;

    let mut state = ControllerState::WaitInit;
    let _ = event_tx.send(MediaEngineEvent::StateChanged(state.clone()));

    let mut last_signal_bars = SIGNAL_BAR_STARTING;
    let mut last_remote_packet_ms: Option<u64> = None;
    let mut established_once = false;
    let mut frame_counter: u64 = 0;

    // Emit initial encrypted `tgcalls` v2 signaling: InitialSetup + NegotiateChannels + Candidates.
    let setup_msg = SignalingMessage::InitialSetup {
        ufrag: format!("tgv{:08x}", rand::thread_rng().r#gen::<u32>()),
        pwd: format!(
            "tgvpwd{:016x}{:08x}",
            rand::thread_rng().r#gen::<u64>(),
            rand::thread_rng().r#gen::<u32>()
        ),
        supports_renomination: true,
        fingerprints: vec![SignalingDtlsFingerprint {
            hash: "sha-256".into(),
            setup: if descriptor.is_outgoing {
                "actpass".into()
            } else {
                "active".into()
            },
            // MTProto-2.0 transport auth relies on the DH key (`MtProtoDtlsTransport.cpp`).
            fingerprint: "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00".into(),
        }],
    };
    if let Ok(enc) = signaling.encode(&setup_msg) {
        let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
    }

    let neg_msg = SignalingMessage::default_audio_video_negotiate(1, audio_ssrc, video_ssrc);
    if let Ok(enc) = signaling.encode(&neg_msg) {
        let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
    }

    let init_cands = transport.initial_local_candidates();
    if !init_cands.is_empty()
        && let Ok(enc) = signaling.encode(&SignalingMessage::Candidates {
            candidates: init_cands,
        })
    {
        let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
    }

    let _ = transport
        .send_initial_probes(elapsed_ms(), quality.network_type())
        .await;
    quality.set_network_type(config.initial_network_type, 0, transport.endpoint_kind());

    // Publish initial live snapshot so an immediate `stop()` already has valid metadata & JSON.
    {
        let jb_stats = jitter.stats();
        let snap = build_snapshot(&descriptor.version, &transport, &quality, &jb_stats);
        let dbg_json = snap.to_debug_log_json(&quality);
        if let Ok(mut g) = shared_stats.lock() {
            *g = snap;
        }
        if let Ok(mut g) = shared_debug_log.lock() {
            *g = dbg_json;
        }
    }

    let mut media_interval = tokio::time::interval(Duration::from_millis(u64::from(VOIP_FRAME_MS)));
    let mut sctp_router = SctpDataChannelRouter::new();
    media_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut quality_interval =
        tokio::time::interval(Duration::from_millis(config.quality_tick_ms.max(100)));
    quality_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut udp_buf = vec![0u8; 4096];
    let mut capture_frame = [0.0f32; VOIP_FRAME_SAMPLES];
    let mut playout_frame = [0.0f32; VOIP_FRAME_SAMPLES];

    let mut stop_reply: Option<oneshot::Sender<FinalCallState>> = None;

    loop {
        tokio::select! {
            biased;

            // 1. Control commands from `tg-calls` / UI (highest priority so Stop is never starved).
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    break;
                };
                let now = elapsed_ms();
                match cmd {
                    EngineCommand::ReceiveSignaling(encrypted) => {
                        match signaling.decode(&encrypted) {
                            Ok(SignalingMessage::InitialSetup { .. }) => {
                                if state == ControllerState::WaitInit {
                                    state = ControllerState::WaitInitAck;
                                    let _ = event_tx.send(MediaEngineEvent::StateChanged(state.clone()));
                                }
                            }
                            Ok(SignalingMessage::NegotiateChannels { contents, .. }) => {
                                for content in contents {
                                    if content.media_type == crate::signaling::SignalingMediaType::Video {
                                        remote_video_ssrc = content.ssrc;
                                    }
                                }
                                if state == ControllerState::WaitInit {
                                    state = ControllerState::WaitInitAck;
                                    let _ = event_tx.send(MediaEngineEvent::StateChanged(state.clone()));
                                }
                            }
                            Ok(SignalingMessage::Candidates { candidates }) => {
                                for c in candidates {
                                    transport.add_remote_candidate(&c.sdp_string);
                                }
                                // Immediately probe newly learned remote routes.
                                let _ = transport
                                    .tick_keepalives(now, quality.network_type())
                                    .await;
                            }
                            Ok(
                                ref msg @ SignalingMessage::MediaState {
                                    video_rotation,
                                    screencast_state,
                                    ..
                                },
                            ) => {
                                if video_rotation != remote_video_rotation {
                                    remote_video_rotation = video_rotation;
                                    let _ = event_tx.send(
                                        MediaEngineEvent::RemoteVideoRotationChanged(video_rotation),
                                    );
                                }
                                if screencast_state != remote_screencast_state {
                                    remote_screencast_state = screencast_state;
                                    let _ = event_tx.send(
                                        MediaEngineEvent::RemoteScreencastStateChanged(
                                            screencast_state.to_call_video_state(),
                                        ),
                                    );
                                }
                                if let Some((audio, video, battery)) = msg.as_call_media_state() {
                                    let _ = event_tx.send(MediaEngineEvent::RemoteMediaStateChanged {
                                        audio,
                                        video,
                                    });
                                    let _ = event_tx.send(MediaEngineEvent::RemoteBatteryChanged(battery));
                                }
                            }
                            Err(err) => {
                                tracing::warn!("tg-voip failed to decode signaling packet: {err}");
                            }
                        }
                    }
                    EngineCommand::SetMuted(muted) => {
                        local_muted = muted;
                        audio_io.set_muted(muted);
                        let msg = SignalingMessage::MediaState {
                            muted: local_muted,
                            low_battery: local_low_battery,
                            video_state: local_video_state,
                            video_rotation: local_video_rotation,
                            screencast_state: local_screencast_state,
                        };
                        if let Ok(enc) = signaling.encode(&msg) {
                            let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
                        }
                    }
                    EngineCommand::SetInputDevice(id) => {
                        match audio_io.set_input_device(&id) {
                            Ok(()) => {
                                if let Ok(mut g) = shared_input_device.lock() {
                                    *g = audio_io.input_device_id().to_string();
                                }
                            }
                            Err(err) => {
                                tracing::error!("tg-voip failed to switch input device '{id}': {err}");
                            }
                        }
                    }
                    EngineCommand::SetOutputDevice(id) => {
                        match audio_io.set_output_device(&id) {
                            Ok(()) => {
                                if let Ok(mut g) = shared_output_device.lock() {
                                    *g = audio_io.output_device_id().to_string();
                                }
                            }
                            Err(err) => {
                                tracing::error!("tg-voip failed to switch output device '{id}': {err}");
                            }
                        }
                    }
                    EngineCommand::SetInputVolume(vol) => {
                        audio_io.set_input_volume(vol);
                    }
                    EngineCommand::SetOutputVolume(vol) => {
                        audio_io.set_output_volume(vol);
                    }
                    EngineCommand::SetNetworkType(nt) => {
                        quality.set_network_type(nt, now, transport.endpoint_kind());
                        encoder.set_bitrate_bps(quality.bitrate_bps());
                        if let Some(ref mut venc) = video_encoder {
                            venc.set_bitrate_bps(quality.video_bitrate_bps());
                        }
                    }
                    EngineCommand::SetDataSaving(ds) => {
                        quality.set_data_saving(ds);
                        encoder.set_bitrate_bps(quality.bitrate_bps());
                        if let Some(ref mut venc) = video_encoder {
                            venc.set_bitrate_bps(quality.video_bitrate_bps());
                        }
                    }
                    EngineCommand::SetLowBattery(low) => {
                        local_low_battery = low;
                        let msg = SignalingMessage::MediaState {
                            muted: local_muted,
                            low_battery: local_low_battery,
                            video_state: local_video_state,
                            video_rotation: local_video_rotation,
                            screencast_state: local_screencast_state,
                        };
                        if let Ok(enc) = signaling.encode(&msg) {
                            let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
                        }
                    }
                    EngineCommand::StartVideo {
                        source,
                        encoder: venc,
                        is_screencast,
                    } => {
                        if let Some(mut prev) = video_capture.take() {
                            prev.stop();
                        }
                        video_capture = Some(source);
                        video_encoder = Some(venc);
                        video_is_screencast = is_screencast;
                        video_paused = false;
                        force_next_keyframe = true;
                        if is_screencast {
                            local_screencast_state = SignalingVideoState::Active;
                            local_video_state = SignalingVideoState::Inactive;
                        } else {
                            local_video_state = SignalingVideoState::Active;
                            local_screencast_state = SignalingVideoState::Inactive;
                        }
                        let msg = SignalingMessage::MediaState {
                            muted: local_muted,
                            low_battery: local_low_battery,
                            video_state: local_video_state,
                            video_rotation: local_video_rotation,
                            screencast_state: local_screencast_state,
                        };
                        if let Ok(enc) = signaling.encode(&msg) {
                            let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
                        }
                    }
                    EngineCommand::StopVideo => {
                        if let Some(mut prev) = video_capture.take() {
                            prev.stop();
                        }
                        video_encoder = None;
                        video_paused = false;
                        local_video_state = SignalingVideoState::Inactive;
                        local_screencast_state = SignalingVideoState::Inactive;
                        let msg = SignalingMessage::MediaState {
                            muted: local_muted,
                            low_battery: local_low_battery,
                            video_state: local_video_state,
                            video_rotation: local_video_rotation,
                            screencast_state: local_screencast_state,
                        };
                        if let Ok(enc) = signaling.encode(&msg) {
                            let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
                        }
                    }
                    EngineCommand::SetVideoPaused(paused) => {
                        video_paused = paused;
                        if video_capture.is_some() {
                            let next = if paused {
                                SignalingVideoState::Suspended
                            } else {
                                force_next_keyframe = true;
                                SignalingVideoState::Active
                            };
                            if video_is_screencast {
                                local_screencast_state = next;
                            } else {
                                local_video_state = next;
                            }
                            let msg = SignalingMessage::MediaState {
                                muted: local_muted,
                                low_battery: local_low_battery,
                                video_state: local_video_state,
                                video_rotation: local_video_rotation,
                                screencast_state: local_screencast_state,
                            };
                            if let Ok(enc) = signaling.encode(&msg) {
                                let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
                            }
                        }
                    }
                    EngineCommand::SetVideoRotation(rot) => {
                        local_video_rotation = rot;
                        let msg = SignalingMessage::MediaState {
                            muted: local_muted,
                            low_battery: local_low_battery,
                            video_state: local_video_state,
                            video_rotation: local_video_rotation,
                            screencast_state: local_screencast_state,
                        };
                        if let Ok(enc) = signaling.encode(&msg) {
                            let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
                        }
                    }
                    EngineCommand::SetVideoDecoder(dec) => {
                        video_decoder = Some(dec);
                        video_decoder_init_failed = false;
                    }
                    EngineCommand::RequestRemoteKeyframe => {
                        last_pli_sent_ms = now;
                        fir_seq_nr = fir_seq_nr.wrapping_add(1);
                        let pli = VoipPacket::RtcpPli {
                            sender_ssrc: video_ssrc,
                            media_ssrc: remote_video_ssrc,
                        };
                        let fir = VoipPacket::RtcpFir {
                            sender_ssrc: video_ssrc,
                            media_ssrc: remote_video_ssrc,
                            seq_nr: fir_seq_nr,
                        };
                        let _ = transport
                            .send_voip_packet(&pli, now, quality.network_type())
                            .await;
                        let _ = transport
                            .send_voip_packet(&fir, now, quality.network_type())
                            .await;
                    }
                    EngineCommand::SendDataChannel {
                        stream_id,
                        label,
                        ppid,
                        payload,
                    } => {
                        let raw = sctp_router.prepare_send(stream_id, label, ppid, payload);
                        let pkt = VoipPacket::Sctp(raw);
                        let _ = transport
                            .send_voip_packet(&pkt, now, quality.network_type())
                            .await;
                    }
                    EngineCommand::Stop(reply_tx) => {
                        stop_reply = Some(reply_tx);
                        break;
                    }
                }
            }

            // 2. Incoming UDP datagrams (reflector / P2P / STUN).
            recv_res = transport.recv_next(&mut udp_buf, elapsed_ms(), quality.network_type()) => {
                if let Ok(outcome) = recv_res {
                    let now = elapsed_ms();

                    if !outcome.new_local_candidates.is_empty()
                        && let Ok(enc) = signaling.encode(&SignalingMessage::Candidates {
                            candidates: outcome.new_local_candidates,
                        })
                    {
                        let _ = event_tx.send(MediaEngineEvent::SignalingData(enc));
                    }

                    let mut stats_dirty = false;
                    if let Some(relay_id) = outcome.preferred_relay_changed {
                        stats_dirty = true;
                        let _ = event_tx.send(MediaEngineEvent::PreferredRelayChanged(relay_id));
                    }

                    for pkt in outcome.packets {
                        last_remote_packet_ms = Some(now);
                        if state != ControllerState::Established {
                            state = ControllerState::Established;
                            established_once = true;
                            shared_established.store(true, Ordering::Release);
                            stats_dirty = true;
                            let _ = event_tx.send(MediaEngineEvent::StateChanged(state.clone()));
                        }

                        match pkt {
                            VoipPacket::RtpAudio { header, payload } => {
                                jitter.push(header.sequence_number, header.timestamp, now, payload);
                            }
                            VoipPacket::RtpVideo { header, payload } => {
                                remote_video_ssrc = header.ssrc;
                                if let Ok(depkt) = h264_depacketizer.push_packet(&header, &payload) {
                                    if depkt.needs_keyframe
                                        && now.saturating_sub(last_pli_sent_ms) >= 500
                                    {
                                        last_pli_sent_ms = now;
                                        let pli = VoipPacket::RtcpPli {
                                            sender_ssrc: video_ssrc,
                                            media_ssrc: header.ssrc,
                                        };
                                        let _ = transport
                                            .send_voip_packet(&pli, now, quality.network_type())
                                            .await;
                                    }
                                    if let Some(au) = depkt.frame {
                                        if video_decoder.is_none() && !video_decoder_init_failed {
                                            match create_h264_decoder() {
                                                Ok(dec) => video_decoder = Some(dec),
                                                Err(e) => {
                                                    tracing::warn!("tg-voip H.264 decoder init failed: {e}");
                                                    video_decoder_init_failed = true;
                                                }
                                            }
                                        }
                                        if let Some(ref mut dec) = video_decoder
                                            && let Ok(Some(mut raw_frame)) = dec.decode(&au.annex_b)
                                        {
                                            raw_frame.rotation = remote_video_rotation;
                                            if let Ok(rgba) = raw_frame.to_rgba_rotated() {
                                                let _ = event_tx
                                                    .send(MediaEngineEvent::RemoteVideoFrame(rgba));
                                            }
                                        }
                                    }
                                }
                            }
                            VoipPacket::RtcpPli { .. } | VoipPacket::RtcpFir { .. } => {
                                force_next_keyframe = true;
                                let _ = event_tx.send(MediaEngineEvent::KeyframeRequested);
                            }
                            VoipPacket::Ping { sent_ms, nonce } => {
                                let pong = VoipPacket::Pong { sent_ms, nonce };
                                let _ = transport
                                    .send_voip_packet(&pong, now, quality.network_type())
                                    .await;
                            }
                            VoipPacket::Pong { sent_ms, .. } => {
                                let rtt = now.saturating_sub(sent_ms) as f64;
                                quality.observe_rtt(rtt);
                            }
                            VoipPacket::ReceiverReport { rtt_ms, .. } => {
                                if rtt_ms > 0 {
                                    quality.observe_rtt(f64::from(rtt_ms));
                                }
                            }
                            VoipPacket::Sctp(raw) => {
                                if let Ok(sctp_out) = sctp_router.handle_incoming(&raw) {
                                    if let Some(sack) = sctp_out.reply_sack {
                                        let ack_pkt = VoipPacket::Sctp(sack);
                                        let _ = transport
                                            .send_voip_packet(&ack_pkt, now, quality.network_type())
                                            .await;
                                    }
                                    if let Some(msg) = sctp_out.message {
                                        let _ = event_tx.send(
                                            MediaEngineEvent::DataChannelMessage {
                                                stream_id: msg.stream_id,
                                                label: msg.label,
                                                ppid: msg.ppid,
                                                payload: msg.payload,
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }

                    if stats_dirty {
                        let jb_stats = jitter.stats();
                        let snap =
                            build_snapshot(&descriptor.version, &transport, &quality, &jb_stats);
                        let dbg_json = snap.to_debug_log_json(&quality);
                        if let Ok(mut g) = shared_stats.lock() {
                            *g = snap;
                        }
                        if let Ok(mut g) = shared_debug_log.lock() {
                            *g = dbg_json;
                        }
                    }
                }
            }

            // 3. 20 ms real-time voice frame tick: playout -> AEC3 render -> capture -> DSP -> Opus -> UDP.
            _ = media_interval.tick() => {
                let now = elapsed_ms();
                frame_counter += 1;

                // Playout path: jitter buffer -> Opus decode / FEC / PLC -> speaker + AEC3 render ref.
                if jitter.pull_frame(&mut playout_frame).is_ok() {
                    let _ = dsp.feed_render_frame(&playout_frame);
                    let _ = audio_io.push_playback_frame(&playout_frame);
                }

                // Capture path: mic -> HPF/AEC3/NS/AGC2 -> Opus encode -> RTP -> AES-256-CTR -> UDP.
                if audio_io.try_pop_capture_frame(&mut capture_frame) {
                    let level = dsp
                        .process_capture_frame(&mut capture_frame)
                        .unwrap_or(0.0);
                    if frame_counter.is_multiple_of(5) {
                        let _ = event_tx.send(MediaEngineEvent::AudioLevelChanged(level));
                    }

                    if let Ok(opus_payload) = encoder.encode_frame(&capture_frame) {
                        let header = RtpHeader {
                            marker: rtp_seq == 0,
                            payload_type: OPUS_RTP_PAYLOAD_TYPE,
                            sequence_number: rtp_seq,
                            timestamp: rtp_ts,
                            ssrc: audio_ssrc,
                        };
                        rtp_seq = rtp_seq.wrapping_add(1);
                        rtp_ts = rtp_ts.wrapping_add(VOIP_FRAME_SAMPLES as u32);

                        let pkt = VoipPacket::RtpAudio {
                            header,
                            payload: opus_payload,
                        };
                        let _ = transport
                            .send_voip_packet(&pkt, now, quality.network_type())
                            .await;
                    }
                }

                // Video capture/encode/packetize tick paced by adaptive `QualityController` FPS.
                let min_video_interval_ms = 1_000u64 / u64::from(last_video_profile.fps.max(1));
                if !video_paused
                    && now.saturating_sub(last_video_send_ms) >= min_video_interval_ms
                    && let (Some(src), Some(enc)) = (video_capture.as_mut(), video_encoder.as_mut())
                    && let Ok(Some(frame)) = src.next_frame()
                {
                    last_video_send_ms = now;
                    let keyframe = force_next_keyframe;
                    if let Ok(bitstream) = enc.encode(&frame, keyframe)
                        && !bitstream.is_empty()
                    {
                        force_next_keyframe = false;
                        let rtp_ts_90k = H264RtpPacketizer::timestamp_us_to_rtp(frame.timestamp_us);
                        for vpkt in h264_packetizer.packetize_annex_b(&bitstream, rtp_ts_90k) {
                            let wire = VoipPacket::RtpVideo {
                                header: vpkt.header,
                                payload: vpkt.payload,
                            };
                            let _ = transport
                                .send_voip_packet(&wire, now, quality.network_type())
                                .await;
                        }
                    }
                }
            }

            // 4. Periodic keepalive, bitrate adaptation, signal bars, timeouts, and telemetry.
            _ = quality_interval.tick() => {
                let now = elapsed_ms();
                let _ = transport
                    .tick_keepalives(now, quality.network_type())
                    .await;

                let fraction_lost = jitter.take_interval_fraction_lost();
                let jb_stats = jitter.stats();
                let rr = VoipPacket::ReceiverReport {
                    fraction_lost,
                    jitter_ms: jb_stats.jitter_ms.round().clamp(0.0, f64::from(u16::MAX)) as u16,
                    rtt_ms: quality.rtt_ms().round().clamp(0.0, f64::from(u16::MAX)) as u16,
                    packets_received: jb_stats.packets_received.min(u64::from(u32::MAX)) as u32,
                };
                let _ = transport
                    .send_voip_packet(&rr, now, quality.network_type())
                    .await;

                let (new_bps, loss_perc, bars) = quality.on_quality_tick(fraction_lost, now);
                encoder.set_bitrate_bps(new_bps);
                encoder.set_packet_loss_perc(loss_perc);

                let vprofile = quality.video_profile();
                if let Some(ref mut venc) = video_encoder {
                    if vprofile.width != last_video_profile.width
                        || vprofile.height != last_video_profile.height
                        || vprofile.fps != last_video_profile.fps
                    {
                        if venc
                            .reconfigure(
                                vprofile.width,
                                vprofile.height,
                                vprofile.fps,
                                vprofile.bitrate_bps,
                            )
                            .is_ok()
                        {
                            force_next_keyframe = true;
                        }
                    } else if vprofile.bitrate_bps != last_video_profile.bitrate_bps {
                        venc.set_bitrate_bps(vprofile.bitrate_bps);
                    }
                }
                last_video_profile = vprofile;

                if state == ControllerState::Established && bars != last_signal_bars {
                    last_signal_bars = bars;
                    let _ = event_tx.send(MediaEngineEvent::SignalBarsChanged(bars));
                }

                // Update live stats & debug log snapshots.
                let snap = build_snapshot(&descriptor.version, &transport, &quality, &jb_stats);
                let dbg_json = snap.to_debug_log_json(&quality);
                if let Ok(mut g) = shared_stats.lock() {
                    *g = snap;
                }
                if let Ok(mut g) = shared_debug_log.lock() {
                    *g = dbg_json;
                }

                // Enforce initialization & receive timeouts (`ControllerDescriptor`).
                if !established_once
                    && descriptor.initialization_timeout_ms > 0
                    && now >= descriptor.initialization_timeout_ms
                {
                    let _ = event_tx.send(MediaEngineEvent::StateChanged(ControllerState::Failed(
                        "ERROR_TIMEOUT".into(),
                    )));
                    break;
                }
                if let Some(last_rx) = last_remote_packet_ms
                    && descriptor.receive_timeout_ms > 0
                    && now.saturating_sub(last_rx) >= descriptor.receive_timeout_ms
                {
                    let _ = event_tx.send(MediaEngineEvent::StateChanged(ControllerState::Failed(
                        "ERROR_TIMEOUT".into(),
                    )));
                    break;
                }
            }
        }
    }

    if let Some(mut cap) = video_capture.take() {
        cap.stop();
    }
    audio_io.stop();

    let jb_stats = jitter.stats();
    let final_snap = build_snapshot(&descriptor.version, &transport, &quality, &jb_stats);
    let debug_log = final_snap.to_debug_log_json(&quality);
    if let Ok(mut g) = shared_stats.lock() {
        *g = final_snap.clone();
    }
    if let Ok(mut g) = shared_debug_log.lock() {
        *g = debug_log.clone();
    }

    let final_state = FinalCallState {
        debug_log,
        traffic_stats: final_snap.traffic,
        preferred_relay_id: final_snap.preferred_relay_id,
        is_rating_suggested: established_once && final_snap.packets_received > 0,
        stats: final_snap,
    };
    if let Some(tx) = stop_reply {
        let _ = tx.send(final_state);
    }
}

fn build_snapshot(
    version: &str,
    transport: &VoipTransport,
    quality: &QualityController,
    jb_stats: &crate::jitter::JitterBufferStats,
) -> VoipStatsSnapshot {
    let mut snap = VoipStatsSnapshot {
        version: version.to_string(),
        outgoing_codec: "opus".to_string(),
        audio_bitrate_bps: quality.bitrate_bps(),
        signal_bars: quality.signal_bars(),
        rtt_ms: quality.rtt_ms(),
        loss_ratio: quality.loss_ratio(),
        packets_sent: transport.packets_sent(),
        packets_received: transport.packets_received(),
        packets_decrypt_failed: transport.packets_decrypt_failed(),
        endpoint_kind: transport.endpoint_kind(),
        preferred_relay_id: transport.preferred_relay_id(),
        traffic: transport.traffic_stats(),
        ..VoipStatsSnapshot::default()
    };
    snap.apply_jitter(jb_stats);
    snap
}
