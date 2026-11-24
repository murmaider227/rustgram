//! High-level group call media engine (`GroupMediaEngine`, port of `tgcalls::GroupInstanceCustomImpl`).
//!
//! Reference:
//! - `TelegramMessenger/tgcalls` (`tgcalls/group/GroupInstanceCustomImpl.cpp`)
//! - `crates/tg-calls/src/group/state.rs` (`GroupCallSession`, `GroupCallAction`, `AudioLevelSample`)
//!
//! Supports both Telegram group call modes seamlessly:
//! - **Interactive RTC mode (`InstanceMode::Rtc`)**: Generates `GroupJoinPayload` (audio SSRC,
//!   ICE `ufrag`/`pwd`, ECDSA P-256 SHA-256 DTLS fingerprint), connects via ICE-lite STUN checks
//!   to SFU UDP candidates, completes DTLS 1.2 handshake + peer fingerprint verification, derives
//!   SRTP keys, sends local Opus RTP (encrypted via SRTP), receives & mixes multi-SSRC SFU Opus
//!   audio, applies per-SSRC volumes, and emits [`tg_calls::AudioLevelSample`] updates.
//! - **Broadcast Stream mode (`InstanceMode::Stream`)**: Pulls time-aligned media chunks via
//!   [`BroadcastChunkLoader`] (`upload.getFile` + `inputGroupCallStream`), decodes Ogg/framed
//!   Opus audio, and plays it through the audio output device.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_audio::device::DEFAULT_DEVICE_ID;
use tg_calls::group::SPEAK_LEVEL_THRESHOLD;
use tg_calls::types::UserId;
use tg_calls::{
    AudioLevelSample, ConferenceEncryptDecrypt, DEFAULT_CALL_CHANNEL_ID, GroupJoinPayload,
    InstanceMode, JoinSsrcGroup, ParsedJoinResponse, RtcJoinResponse, parse_join_response,
};
use tokio::net::UdpSocket;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

static ACTIVE_GROUP_MEDIA_TASKS: AtomicUsize = AtomicUsize::new(0);

/// Number of currently running `GroupMediaEngine` background tasks in this process.
#[must_use]
pub fn active_group_media_task_count() -> usize {
    ACTIVE_GROUP_MEDIA_TASKS.load(Ordering::Acquire)
}

struct GroupMediaTaskCounterGuard;

impl GroupMediaTaskCounterGuard {
    fn enter() -> Self {
        ACTIVE_GROUP_MEDIA_TASKS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for GroupMediaTaskCounterGuard {
    fn drop(&mut self) {
        ACTIVE_GROUP_MEDIA_TASKS.fetch_sub(1, Ordering::AcqRel);
    }
}

use crate::audio_io::{AudioBackendKind, VirtualAudioHandle, VoipAudioIo};
use crate::dsp::{DEFAULT_VOICE_BITRATE_BPS, VoiceDsp, VoiceDspConfig, VoiceOpusEncoder};
use crate::error::{VoipError, VoipResult};
use crate::group::broadcast::{BroadcastChunkLoader, BroadcastChunkScale, BroadcastStreamPlayer};
use crate::group::dtls_srtp::{
    DtlsIdentity, DtlsSrtpSession, WebRtcPacketKind, classify_webrtc_packet, is_rtcp_packet,
};
use crate::group::ice::{
    IceLiteClient, IceStunOutcome, LocalIceCredentials, build_group_join_payload,
};
use crate::group::mixer::GroupAudioMixer;
use crate::jitter::{JitterBufferConfig, VOIP_FRAME_MS, VOIP_FRAME_SAMPLES};
use crate::rtp::{OPUS_RTP_PAYLOAD_TYPE, RtpHeader, parse_rtp_audio_level_ext};

/// Events emitted by [`GroupMediaEngine`] to drive `tg_calls::GroupCallSession` and `tg-app`.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupMediaEvent {
    /// Network connectivity update -> `GroupCallSession::set_instance_connected`.
    NetworkStateChanged {
        /// `true` once ICE + DTLS-SRTP (or broadcast chunk stream) is active.
        connected: bool,
        /// `true` while upgrading from broadcast stream to interactive RTC.
        transitioning_to_rtc: bool,
    },
    /// Per-SSRC speaking levels -> `GroupCallSession::audio_levels_updated`.
    /// (`ssrc == 0` represents the local user's microphone, matching `tgcalls`).
    AudioLevelsUpdated(Vec<AudioLevelSample>),
    /// Active transport mode changed (`None`, `Rtc`, `Stream`).
    ModeChanged(InstanceMode),
    /// Non-fatal or fatal transport error message.
    Error(String),
}

/// Configuration for creating a [`GroupMediaEngine`].
#[derive(Debug, Clone)]
pub struct GroupMediaConfig {
    /// Hardware WASAPI or headless virtual PCM backend.
    pub audio_backend: AudioBackendKind,
    /// Virtual audio ring capacity in samples (when `Virtual`).
    pub virtual_ring_capacity: usize,
    /// Voice DSP (AEC3 + NS + AGC2 + HPF) configuration.
    pub dsp: VoiceDspConfig,
    /// Per-SSRC jitter buffer configuration.
    pub jitter: JitterBufferConfig,
    /// Local UDP socket bind address (`0.0.0.0:0` in production, `127.0.0.1:0` in tests).
    pub bind_addr: SocketAddr,
    /// Initial preferred capture device ID (`"default"`).
    pub input_device_id: String,
    /// Initial preferred playback device ID (`"default"`).
    pub output_device_id: String,
    /// Start locally muted (`true` by default for Telegram group calls).
    pub initial_muted: bool,
    /// Broadcast chunk scale for `InstanceMode::Stream` (default 1000 ms).
    pub broadcast_scale: BroadcastChunkScale,
    /// Enable conference E2E frame encryption/decryption (`tde2e`) on RTP media payloads.
    ///
    /// When `false` (default), group calls use standard SFU DTLS-SRTP transport encryption only.
    /// When `true`, outgoing audio/video payloads are additionally encrypted with the active
    /// [`ConferenceEncryptDecrypt`] epoch key before SRTP and incoming payloads are authenticated
    /// and decrypted after SRTP (never falling back to plaintext).
    pub e2e_frame_encryption: bool,
    /// E2E media channel ID passed to [`ConferenceEncryptDecrypt`] (`0` by default).
    pub e2e_channel_id: i32,
    /// Leading unencrypted prefix length for E2E audio frames (`0` by default).
    pub e2e_audio_unencrypted_prefix: usize,
    /// Leading unencrypted prefix length for E2E video frames (`1` byte NAL header by default).
    pub e2e_video_unencrypted_prefix: usize,
}

impl Default for GroupMediaConfig {
    fn default() -> Self {
        Self {
            audio_backend: AudioBackendKind::Hardware,
            virtual_ring_capacity: VOIP_FRAME_SAMPLES * 250,
            dsp: VoiceDspConfig::default(),
            jitter: JitterBufferConfig::default(),
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            input_device_id: DEFAULT_DEVICE_ID.to_string(),
            output_device_id: DEFAULT_DEVICE_ID.to_string(),
            initial_muted: true,
            broadcast_scale: BroadcastChunkScale::Ms1000,
            e2e_frame_encryption: false,
            e2e_channel_id: DEFAULT_CALL_CHANNEL_ID,
            e2e_audio_unencrypted_prefix: 0,
            e2e_video_unencrypted_prefix: 1,
        }
    }
}

/// Apply optional E2E frame encryption to an outgoing audio or video frame payload.
///
/// When `enabled` is `false`, returns `payload` unchanged. When `enabled` is `true`,
/// requires an active [`ConferenceEncryptDecrypt`] handler (never leaking plaintext).
pub fn transform_outgoing_e2e_frame(
    enabled: bool,
    handler: Option<&ConferenceEncryptDecrypt>,
    channel_id: i32,
    payload: &[u8],
    unencrypted_prefix: usize,
) -> VoipResult<Vec<u8>> {
    if !enabled {
        return Ok(payload.to_vec());
    }
    let Some(e2e) = handler else {
        return Err(VoipError::InvalidState(
            "E2E frame encryption enabled without ConferenceEncryptDecrypt handler".into(),
        ));
    };
    let prefix = unencrypted_prefix.min(payload.len());
    e2e.encrypt_frame(channel_id, payload, prefix)
        .map_err(|e| VoipError::GroupCall(format!("E2E frame encrypt failed: {e}")))
}

/// Apply optional E2E frame decryption to an incoming audio or video frame payload.
///
/// When `enabled` is `false`, returns `packet` unchanged. When `enabled` is `true`,
/// authenticates and decrypts `packet` using `sender_user_id`.
pub fn transform_incoming_e2e_frame(
    enabled: bool,
    handler: Option<&ConferenceEncryptDecrypt>,
    sender_user_id: Option<UserId>,
    channel_id: i32,
    packet: &[u8],
) -> VoipResult<Vec<u8>> {
    if !enabled {
        return Ok(packet.to_vec());
    }
    let Some(e2e) = handler else {
        return Err(VoipError::InvalidState(
            "E2E frame decryption enabled without ConferenceEncryptDecrypt handler".into(),
        ));
    };
    let Some(uid) = sender_user_id else {
        return Err(VoipError::GroupCall(
            "E2E frame decryption failed: unknown sender SSRC".into(),
        ));
    };
    e2e.decrypt_frame(uid, channel_id, packet)
        .map_err(|e| VoipError::GroupCall(format!("E2E frame decrypt failed: {e}")))
}

enum GroupCommand {
    UpdateCredentials(LocalIceCredentials),
    ApplyRtcJoinResponse(Box<RtcJoinResponse>),
    StartBroadcastStream,
    SetMuted(bool),
    SetSsrcVolume { ssrc: u32, volume: f64 },
    RemoveSsrcs(Vec<u32>),
    SetInputDevice(String),
    SetOutputDevice(String),
    SetBroadcastLoader(Arc<dyn BroadcastChunkLoader>),
    SetE2eEncryptDecrypt(Option<Arc<ConferenceEncryptDecrypt>>),
    SetE2eFrameEncryption(bool),
    Stop(oneshot::Sender<()>),
}

/// High-level group call media engine (`GroupInstanceCustomImpl` port).
pub struct GroupMediaEngine {
    config: GroupMediaConfig,
    runtime: Handle,
    identity: DtlsIdentity,
    ice_creds: LocalIceCredentials,
    cmd_tx: Option<mpsc::UnboundedSender<GroupCommand>>,
    event_rx: Option<mpsc::UnboundedReceiver<GroupMediaEvent>>,
    task: Option<tokio::task::JoinHandle<()>>,
    teardown_task: Option<tokio::task::JoinHandle<()>>,
    virtual_audio: Option<Arc<VirtualAudioHandle>>,
    broadcast_loader: Option<Arc<dyn BroadcastChunkLoader>>,
    e2e_handler: Option<Arc<ConferenceEncryptDecrypt>>,
    ssrc_users: Arc<Mutex<HashMap<u32, UserId>>>,
    shared_input_device: Arc<Mutex<String>>,
    shared_output_device: Arc<Mutex<String>>,
}

impl GroupMediaEngine {
    /// Create a new [`GroupMediaEngine`] with fresh ephemeral DTLS identity and ICE credentials.
    pub fn new(config: GroupMediaConfig, runtime: Handle) -> VoipResult<Self> {
        let identity = DtlsIdentity::generate()?;
        let ice_creds = LocalIceCredentials::generate();
        let input_dev = config.input_device_id.clone();
        let output_dev = config.output_device_id.clone();
        Ok(Self {
            config,
            runtime,
            identity,
            ice_creds,
            cmd_tx: None,
            event_rx: None,
            task: None,
            teardown_task: None,
            virtual_audio: None,
            broadcast_loader: None,
            e2e_handler: None,
            ssrc_users: Arc::new(Mutex::new(HashMap::new())),
            shared_input_device: Arc::new(Mutex::new(input_dev)),
            shared_output_device: Arc::new(Mutex::new(output_dev)),
        })
    }

    /// Create a new [`GroupMediaEngine`] using the current Tokio runtime handle.
    pub fn with_current_runtime(config: GroupMediaConfig) -> VoipResult<Self> {
        let handle = Handle::try_current()
            .map_err(|e| VoipError::InvalidState(format!("no active Tokio runtime: {e}")))?;
        Self::new(config, handle)
    }

    /// Generate the local [`GroupJoinPayload`] (`ssrc`, `ufrag`, `pwd`, `fingerprints`) for
    /// `phone.joinGroupCall` (`GroupCallAction::EmitJoinPayload`).
    #[must_use]
    pub fn emit_join_payload(&self, ssrc_groups: Vec<JoinSsrcGroup>) -> GroupJoinPayload {
        build_group_join_payload(&self.ice_creds, &self.identity, ssrc_groups)
    }

    /// Generate `(audio_ssrc, compact_json)` ready to pass directly to
    /// `GroupCallSession::on_join_payload_ready(ssrc, json)`.
    pub fn emit_join_payload_json(
        &self,
        ssrc_groups: Vec<JoinSsrcGroup>,
    ) -> VoipResult<(u32, String)> {
        let payload = self.emit_join_payload(ssrc_groups);
        let json = payload
            .to_json()
            .map_err(|e| VoipError::GroupCall(e.to_string()))?;
        Ok((payload.ssrc, json))
    }

    /// Regenerate local ICE credentials & SSRC (e.g. on `GROUPCALL_SSRC_DUPLICATE_MUCH` rejoin).
    ///
    /// Mirrors tdesktop `GroupCall::rejoin` (`calls_group_call.cpp:1594-1642`), which requests a
    /// fresh join payload (new SSRC and ICE ufrag/pwd) on each rejoin; forwards the updated
    /// credentials to the running worker loop so subsequent rejoins do not reuse stale values.
    pub fn regenerate_credentials(&mut self) {
        let mut next = LocalIceCredentials::generate();
        while next.audio_ssrc == self.ice_creds.audio_ssrc || next.ufrag == self.ice_creds.ufrag {
            next = LocalIceCredentials::generate();
        }
        self.ice_creds = next.clone();
        self.send_cmd(GroupCommand::UpdateCredentials(next));
    }

    /// Local audio SSRC.
    #[must_use]
    pub const fn local_ssrc(&self) -> u32 {
        self.ice_creds.audio_ssrc
    }

    /// Local DTLS certificate SHA-256 fingerprint.
    #[must_use]
    pub fn local_fingerprint(&self) -> &str {
        &self.identity.fingerprint_sha256
    }

    /// Attach a [`BroadcastChunkLoader`] (`upload.getFile` `inputGroupCallStream`) for large
    /// broadcast/RTMP group calls.
    pub fn set_broadcast_loader(&mut self, loader: Arc<dyn BroadcastChunkLoader>) {
        self.broadcast_loader = Some(Arc::clone(&loader));
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(GroupCommand::SetBroadcastLoader(loader));
        }
    }

    /// Attach or detach a conference E2E frame encryptor/decryptor (`tde2e`).
    ///
    /// Mirrors `tgcalls::GroupInstanceDescriptor::e2eEncryptDecrypt` (`calls_group_call.cpp:3127`).
    pub fn set_e2e_encrypt_decrypt(&mut self, handler: Option<Arc<ConferenceEncryptDecrypt>>) {
        self.e2e_handler = handler.clone();
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(GroupCommand::SetE2eEncryptDecrypt(handler));
        }
    }

    /// Enable or disable E2E frame encryption at runtime.
    pub fn set_e2e_frame_encryption(&mut self, enabled: bool) {
        self.config.e2e_frame_encryption = enabled;
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(GroupCommand::SetE2eFrameEncryption(enabled));
        }
    }

    /// Whether E2E frame encryption is currently enabled on this engine.
    #[must_use]
    pub const fn e2e_frame_encryption_enabled(&self) -> bool {
        self.config.e2e_frame_encryption
    }

    /// Associate a remote participant's RTP audio/video `ssrc` with their Telegram `user_id`
    /// so incoming E2E frames can look up the sender's Ed25519 signing key.
    pub fn register_ssrc_user(&self, ssrc: u32, user_id: UserId) {
        if let Ok(mut map) = self.ssrc_users.lock() {
            map.insert(ssrc, user_id);
        }
    }

    /// Remove an `ssrc -> user_id` mapping when a participant leaves or changes SSRC.
    pub fn unregister_ssrc(&self, ssrc: u32) {
        if let Ok(mut map) = self.ssrc_users.lock() {
            map.remove(&ssrc);
        }
    }

    /// Encrypt an outgoing video frame payload when `e2e_frame_encryption` is enabled
    /// (pass-through when disabled).
    pub fn encrypt_video_frame(&self, frame: &[u8]) -> VoipResult<Vec<u8>> {
        transform_outgoing_e2e_frame(
            self.config.e2e_frame_encryption,
            self.e2e_handler.as_deref(),
            self.config.e2e_channel_id,
            frame,
            self.config.e2e_video_unencrypted_prefix,
        )
    }

    /// Authenticate and decrypt an incoming video frame payload from `sender_ssrc` when
    /// `e2e_frame_encryption` is enabled (pass-through when disabled).
    pub fn decrypt_video_frame(&self, sender_ssrc: u32, packet: &[u8]) -> VoipResult<Vec<u8>> {
        let sender_uid = self
            .ssrc_users
            .lock()
            .ok()
            .and_then(|m| m.get(&sender_ssrc).copied());
        transform_incoming_e2e_frame(
            self.config.e2e_frame_encryption,
            self.e2e_handler.as_deref(),
            sender_uid,
            self.config.e2e_channel_id,
            packet,
        )
    }

    /// Start the background media worker (idempotent; also called automatically by
    /// [`Self::set_join_response_payload`]).
    pub fn start(&mut self) -> VoipResult<()> {
        if self.task.is_some() {
            return Ok(());
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
        audio_io.set_muted(self.config.initial_muted);
        self.virtual_audio = virtual_handle;

        let dsp = VoiceDsp::new(self.config.dsp)?;
        let encoder = VoiceOpusEncoder::new(DEFAULT_VOICE_BITRATE_BPS)?;
        let mixer = GroupAudioMixer::new(self.config.jitter);

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);
        self.shared_input_device = Arc::new(Mutex::new(audio_io.input_device_id().to_string()));
        self.shared_output_device = Arc::new(Mutex::new(audio_io.output_device_id().to_string()));

        let config = self.config.clone();
        let identity = self.identity.clone();
        let creds = self.ice_creds.clone();
        let initial_loader = self.broadcast_loader.clone();
        let initial_e2e = self.e2e_handler.clone();
        let ssrc_users = Arc::clone(&self.ssrc_users);
        let shared_input_device = Arc::clone(&self.shared_input_device);
        let shared_output_device = Arc::clone(&self.shared_output_device);

        let task = self.runtime.spawn(async move {
            run_group_loop(
                config,
                identity,
                creds,
                audio_io,
                dsp,
                encoder,
                mixer,
                initial_loader,
                initial_e2e,
                ssrc_users,
                cmd_rx,
                event_tx,
                shared_input_device,
                shared_output_device,
            )
            .await;
        });
        self.task = Some(task);
        Ok(())
    }

    /// Deliver `updateGroupCallConnection.params` JSON from `tg-calls`
    /// (`GroupCallAction::SetJoinResponsePayload`), automatically switching into RTC or Broadcast
    /// Stream mode.
    pub fn set_join_response_payload(&mut self, params_json: &str) -> VoipResult<()> {
        self.start()?;
        let parsed = parse_join_response(params_json)
            .map_err(|e| VoipError::GroupCall(format!("invalid join response JSON: {e}")))?;
        match parsed {
            ParsedJoinResponse::Rtc { params, .. } => {
                self.send_cmd(GroupCommand::ApplyRtcJoinResponse(params));
            }
            ParsedJoinResponse::BroadcastStream { .. } => {
                self.send_cmd(GroupCommand::StartBroadcastStream);
            }
        }
        Ok(())
    }

    /// Mute or unmute local microphone transmission (`GroupCallAction::SetInstanceMuted`).
    pub fn set_muted(&self, muted: bool) {
        self.send_cmd(GroupCommand::SetMuted(muted));
    }

    /// Set playback linear volume (`0.0..=2.0`) for an audio SSRC
    /// (`GroupCallAction::SetInstanceVolume`).
    pub fn set_ssrc_volume(&self, ssrc: u32, volume: f64) {
        self.send_cmd(GroupCommand::SetSsrcVolume { ssrc, volume });
    }

    /// Remove departed audio SSRCs from the mixer (`GroupCallAction::RemoveInstanceSsrcs`).
    pub fn remove_ssrcs(&self, ssrcs: Vec<u32>) {
        self.send_cmd(GroupCommand::RemoveSsrcs(ssrcs));
    }

    /// Switch capture and playback audio devices (`"default"` follows OS default).
    pub fn set_audio_devices(&self, input_id: &str, output_id: &str) {
        self.send_cmd(GroupCommand::SetInputDevice(input_id.to_string()));
        self.send_cmd(GroupCommand::SetOutputDevice(output_id.to_string()));
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

    /// Access the virtual PCM handle when configured with [`AudioBackendKind::Virtual`].
    #[must_use]
    pub fn virtual_audio(&self) -> Option<Arc<VirtualAudioHandle>> {
        self.virtual_audio.clone()
    }

    /// Non-blocking poll for the next [`GroupMediaEvent`].
    pub fn poll_event(&mut self) -> Option<GroupMediaEvent> {
        self.event_rx.as_mut()?.try_recv().ok()
    }

    /// Async receive for the next [`GroupMediaEvent`].
    pub async fn recv_event(&mut self) -> Option<GroupMediaEvent> {
        self.event_rx.as_mut()?.recv().await
    }

    /// Gracefully stop the group media engine asynchronously.
    pub async fn stop_async(&mut self) {
        if let Some(tx) = self.cmd_tx.take() {
            let (reply_tx, reply_rx) = oneshot::channel();
            if tx.send(GroupCommand::Stop(reply_tx)).is_ok() {
                let _ = tokio::time::timeout(Duration::from_secs(2), reply_rx).await;
            }
        }
        if let Some(mut h) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(2), &mut h)
                .await
                .is_err()
        {
            h.abort();
            let _ = h.await;
        }
        if let Some(mut teardown) = self.teardown_task.take()
            && tokio::time::timeout(Duration::from_secs(2), &mut teardown)
                .await
                .is_err()
        {
            teardown.abort();
            let _ = teardown.await;
        }
    }

    /// Stop the group media engine synchronously without blocking the calling thread.
    ///
    /// Safe to invoke from inside a Tokio runtime, from a plain OS thread, or on the UI thread,
    /// and idempotent if called multiple times.
    pub fn stop(&mut self) {
        if self.cmd_tx.is_none() && self.task.is_none() {
            return;
        }
        if let Some(tx) = self.cmd_tx.take() {
            let (reply_tx, _) = oneshot::channel();
            let _ = tx.send(GroupCommand::Stop(reply_tx));
        }
        if let Some(mut h) = self.task.take() {
            let teardown = self.runtime.spawn(async move {
                if tokio::time::timeout(Duration::from_secs(2), &mut h)
                    .await
                    .is_err()
                {
                    h.abort();
                    let _ = h.await;
                }
            });
            self.teardown_task = Some(teardown);
        }
    }

    fn send_cmd(&self, cmd: GroupCommand) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(cmd);
        }
    }
}

impl Drop for GroupMediaEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_group_loop(
    config: GroupMediaConfig,
    identity: DtlsIdentity,
    mut creds: LocalIceCredentials,
    mut audio_io: VoipAudioIo,
    mut dsp: VoiceDsp,
    mut encoder: VoiceOpusEncoder,
    mut mixer: GroupAudioMixer,
    mut broadcast_loader: Option<Arc<dyn BroadcastChunkLoader>>,
    mut e2e_handler: Option<Arc<ConferenceEncryptDecrypt>>,
    ssrc_users: Arc<Mutex<HashMap<u32, UserId>>>,
    mut cmd_rx: mpsc::UnboundedReceiver<GroupCommand>,
    event_tx: mpsc::UnboundedSender<GroupMediaEvent>,
    shared_input_device: Arc<Mutex<String>>,
    shared_output_device: Arc<Mutex<String>>,
) {
    let _task_counter = GroupMediaTaskCounterGuard::enter();
    let start_instant = Instant::now();
    let elapsed_ms = || start_instant.elapsed().as_millis() as u64;

    let socket = match UdpSocket::bind(config.bind_addr).await {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(GroupMediaEvent::Error(format!(
                "group UDP socket bind failed: {e}"
            )));
            return;
        }
    };

    let mut mode = InstanceMode::None;
    let mut local_muted = config.initial_muted;
    let mut e2e_enabled = config.e2e_frame_encryption;
    let mut ice: Option<IceLiteClient> = None;
    let mut dtls_srtp: Option<DtlsSrtpSession> = None;
    let mut dtls_timeout: Option<Instant> = None;
    let mut opus_pt: u8 = OPUS_RTP_PAYLOAD_TYPE;
    let mut audio_level_ext_id: Option<u8> = Some(1);
    let mut rtp_seq: u16 = 0;
    let mut rtp_ts: u32 = 0;
    let mut is_connected = false;
    let mut broadcast_player: Option<BroadcastStreamPlayer> = None;

    let mut media_interval = tokio::time::interval(Duration::from_millis(u64::from(VOIP_FRAME_MS)));
    media_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut ice_interval = tokio::time::interval(Duration::from_millis(200));
    ice_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut udp_buf = vec![0u8; 4096];
    let mut capture_frame = [0.0f32; VOIP_FRAME_SAMPLES];
    let mut playout_frame = [0.0f32; VOIP_FRAME_SAMPLES];
    let mut frame_counter = 0u64;
    let mut stop_reply: Option<oneshot::Sender<()>> = None;

    loop {
        tokio::select! {
            biased;

            // 1. Control commands from `GroupMediaEngine` / `tg-calls` (highest priority so Stop
            // is never starved behind UDP or audio ticks).
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    break;
                };
                match cmd {
                    GroupCommand::UpdateCredentials(new_creds) => {
                        creds = new_creds;
                    }
                    GroupCommand::ApplyRtcJoinResponse(resp) => {
                        let was_stream = mode == InstanceMode::Stream;
                        mode = InstanceMode::Rtc;
                        broadcast_player = None;
                        let _ = event_tx.send(GroupMediaEvent::ModeChanged(mode));
                        if was_stream {
                            let _ = event_tx.send(GroupMediaEvent::NetworkStateChanged {
                                connected: true,
                                transitioning_to_rtc: true,
                            });
                        }

                        // Resolve negotiated Opus payload type & RFC 6464 audio-level ext ID.
                        if let Some(pt) = resp
                            .audio
                            .payload_types
                            .iter()
                            .find(|p| p.name.eq_ignore_ascii_case("opus"))
                        {
                            opus_pt = (pt.id & 0x7F) as u8;
                        }
                        audio_level_ext_id = resp
                            .audio
                            .rtp_hdrexts
                            .iter()
                            .find(|e| e.uri.contains("ssrc-audio-level"))
                            .map(|e| e.id as u8)
                            .or(Some(1));

                        match IceLiteClient::from_join_response(creds.clone(), &resp) {
                            Ok(mut ice_client) => {
                                let checks = ice_client.poll_outgoing_checks(elapsed_ms());
                                for (addr, pkt) in checks {
                                    let _ = socket.send_to(&pkt, addr).await;
                                }
                                ice = Some(ice_client);
                            }
                            Err(e) => {
                                let _ = event_tx.send(GroupMediaEvent::Error(e.to_string()));
                                continue;
                            }
                        }

                        match DtlsSrtpSession::new(
                            &identity,
                            true,
                            resp.transport.fingerprints.clone(),
                            Instant::now(),
                        ) {
                            Ok(session) => {
                                dtls_srtp = Some(session);
                                dtls_timeout = None;
                                rtp_seq = 0;
                                rtp_ts = 0;
                                is_connected = false;
                            }
                            Err(e) => {
                                let _ = event_tx.send(GroupMediaEvent::Error(e.to_string()));
                            }
                        }
                    }
                    GroupCommand::StartBroadcastStream => {
                        mode = InstanceMode::Stream;
                        ice = None;
                        dtls_srtp = None;
                        is_connected = false;
                        let _ = event_tx.send(GroupMediaEvent::ModeChanged(mode));
                        if let Some(ref loader) = broadcast_loader {
                            match BroadcastStreamPlayer::new(
                                Arc::clone(loader),
                                config.broadcast_scale,
                            ) {
                                Ok(p) => broadcast_player = Some(p),
                                Err(e) => {
                                    let _ = event_tx.send(GroupMediaEvent::Error(e.to_string()));
                                }
                            }
                        }
                    }
                    GroupCommand::SetMuted(muted) => {
                        local_muted = muted;
                        audio_io.set_muted(muted);
                    }
                    GroupCommand::SetSsrcVolume { ssrc, volume } => {
                        mixer.set_volume(ssrc, volume);
                        if let Some(ref mut player) = broadcast_player {
                            player.set_volume(volume);
                        }
                    }
                    GroupCommand::RemoveSsrcs(ssrcs) => {
                        mixer.remove_ssrcs(&ssrcs);
                    }
                    GroupCommand::SetInputDevice(id) => {
                        if audio_io.set_input_device(&id).is_ok()
                            && let Ok(mut g) = shared_input_device.lock()
                        {
                            *g = audio_io.input_device_id().to_string();
                        }
                    }
                    GroupCommand::SetOutputDevice(id) => {
                        if audio_io.set_output_device(&id).is_ok()
                            && let Ok(mut g) = shared_output_device.lock()
                        {
                            *g = audio_io.output_device_id().to_string();
                        }
                    }
                    GroupCommand::SetBroadcastLoader(loader) => {
                        broadcast_loader = Some(Arc::clone(&loader));
                        if mode == InstanceMode::Stream
                            && broadcast_player.is_none()
                            && let Ok(p) = BroadcastStreamPlayer::new(loader, config.broadcast_scale)
                        {
                            broadcast_player = Some(p);
                        }
                    }
                    GroupCommand::SetE2eEncryptDecrypt(handler) => {
                        e2e_handler = handler;
                    }
                    GroupCommand::SetE2eFrameEncryption(enabled) => {
                        e2e_enabled = enabled;
                    }
                    GroupCommand::Stop(reply) => {
                        stop_reply = Some(reply);
                        break;
                    }
                }
            }

            // 2. Incoming UDP packets from the SFU (STUN / DTLS / SRTP).
            recv_res = socket.recv_from(&mut udp_buf) => {
                if let Ok((n, src)) = recv_res {
                    let pkt = &udp_buf[..n];
                    let now_ms = elapsed_ms();
                    match classify_webrtc_packet(pkt) {
                        WebRtcPacketKind::Stun => {
                            if let Some(ref mut ice_client) = ice {
                                match ice_client.handle_stun(pkt, src, now_ms) {
                                    Ok(IceStunOutcome::NewlyNominated(addr)) => {
                                        // Start/pump DTLS 1.2 client flight immediately upon nomination!
                                        if let Some(ref mut session) = dtls_srtp
                                            && let Ok(out) = session.poll_outputs()
                                        {
                                            for d in out.outgoing_packets {
                                                let _ = socket.send_to(&d, addr).await;
                                            }
                                            dtls_timeout = out.next_timeout;
                                        }
                                    }
                                    Ok(IceStunOutcome::SendResponse { to, bytes }) => {
                                        let _ = socket.send_to(&bytes, to).await;
                                    }
                                    Ok(_) | Err(_) => {}
                                }
                            }
                        }
                        WebRtcPacketKind::Dtls => {
                            if let Some(ref mut session) = dtls_srtp {
                                match session.handle_packet(pkt) {
                                    Ok(()) => match session.poll_outputs() {
                                        Ok(out) => {
                                            if let Some(addr) = ice.as_ref().and_then(IceLiteClient::primary_addr) {
                                                for d in out.outgoing_packets {
                                                    let _ = socket.send_to(&d, addr).await;
                                                }
                                            }
                                            dtls_timeout = out.next_timeout;
                                            if out.just_connected && session.is_ready() && !is_connected {
                                                is_connected = true;
                                                let _ = event_tx.send(GroupMediaEvent::NetworkStateChanged {
                                                    connected: true,
                                                    transitioning_to_rtc: false,
                                                });
                                            }
                                        }
                                        Err(e) => {
                                            let _ = event_tx.send(GroupMediaEvent::Error(e.to_string()));
                                        }
                                    },
                                    Err(e) => {
                                        let _ = event_tx.send(GroupMediaEvent::Error(e.to_string()));
                                    }
                                }
                            }
                        }
                        WebRtcPacketKind::SrtpOrSrtcp => {
                            if let Some(ref mut session) = dtls_srtp
                                && session.is_ready()
                            {
                                if is_rtcp_packet(pkt) {
                                    let _ = session.unprotect_rtcp(pkt);
                                } else if let Ok(rtp_plain) = session.unprotect_rtp(pkt)
                                    && let Ok((hdr, payload_off)) = RtpHeader::parse(&rtp_plain)
                                    && hdr.payload_type == opus_pt
                                    && payload_off < rtp_plain.len()
                                {
                                    let ext_lvl = audio_level_ext_id
                                        .and_then(|id| parse_rtp_audio_level_ext(&rtp_plain, id));
                                    let sender_uid = if e2e_enabled {
                                        ssrc_users
                                            .lock()
                                            .ok()
                                            .and_then(|m| m.get(&hdr.ssrc).copied())
                                    } else {
                                        None
                                    };
                                    if let Ok(opus_payload) = transform_incoming_e2e_frame(
                                        e2e_enabled,
                                        e2e_handler.as_deref(),
                                        sender_uid,
                                        config.e2e_channel_id,
                                        &rtp_plain[payload_off..],
                                    ) {
                                        let _ = mixer.push_opus_packet(
                                            hdr.ssrc,
                                            hdr.sequence_number,
                                            hdr.timestamp,
                                            now_ms,
                                            opus_payload,
                                            ext_lvl,
                                        );
                                    }
                                }
                            }
                        }
                        WebRtcPacketKind::Unknown => {}
                    }
                }
            }

            // 3. 20 ms real-time audio tick: mix/playout + capture/send + speaking levels.
            _ = media_interval.tick() => {
                let now_ms = elapsed_ms();
                frame_counter += 1;

                match mode {
                    InstanceMode::Rtc => {
                        let mut levels = mixer.mix_frame(now_ms, &mut playout_frame);
                        let _ = dsp.feed_render_frame(&playout_frame);
                        let _ = audio_io.push_playback_frame(&playout_frame);

                        if audio_io.try_pop_capture_frame(&mut capture_frame) {
                            let rms = dsp.process_capture_frame(&mut capture_frame).unwrap_or(0.0);
                            if local_muted {
                                levels.push(AudioLevelSample {
                                    ssrc: 0,
                                    level: 0.0,
                                    voice: false,
                                });
                            } else {
                                let norm = (rms * 2.5).clamp(0.0, 1.0);
                                levels.push(AudioLevelSample {
                                    ssrc: 0,
                                    level: norm,
                                    voice: norm >= SPEAK_LEVEL_THRESHOLD,
                                });

                                if let (Some(session), Some(sfu_addr)) = (
                                    dtls_srtp.as_mut(),
                                    ice.as_ref().and_then(IceLiteClient::primary_addr),
                                )
                                    && session.is_ready()
                                    && let Ok(opus_payload) = encoder.encode_frame(&capture_frame)
                                    && let Ok(wire_payload) = transform_outgoing_e2e_frame(
                                        e2e_enabled,
                                        e2e_handler.as_deref(),
                                        config.e2e_channel_id,
                                        &opus_payload,
                                        config.e2e_audio_unencrypted_prefix,
                                    )
                                {
                                    let hdr = RtpHeader {
                                        marker: rtp_seq == 0,
                                        payload_type: opus_pt,
                                        sequence_number: rtp_seq,
                                        timestamp: rtp_ts,
                                        ssrc: creds.audio_ssrc,
                                    };
                                    rtp_seq = rtp_seq.wrapping_add(1);
                                    rtp_ts = rtp_ts.wrapping_add(VOIP_FRAME_SAMPLES as u32);

                                    let mut rtp_raw =
                                        Vec::with_capacity(RtpHeader::LEN + wire_payload.len());
                                    rtp_raw.extend_from_slice(&hdr.to_bytes());
                                    rtp_raw.extend_from_slice(&wire_payload);

                                    if let Ok(srtp_pkt) = session.protect_rtp(&rtp_raw) {
                                        let _ = socket.send_to(&srtp_pkt, sfu_addr).await;
                                    }
                                }
                            }
                        }

                        if frame_counter.is_multiple_of(5) && !levels.is_empty() {
                            let _ = event_tx.send(GroupMediaEvent::AudioLevelsUpdated(levels));
                        }
                    }
                    InstanceMode::Stream => {
                        if let Some(ref mut player) = broadcast_player
                            && let Some(lvl) = player.pull_frame(&mut playout_frame)
                        {
                            let _ = audio_io.push_playback_frame(&playout_frame);
                            if frame_counter.is_multiple_of(5) {
                                let _ = event_tx.send(GroupMediaEvent::AudioLevelsUpdated(vec![lvl]));
                            }
                        }
                    }
                    InstanceMode::None => {}
                }
            }

            // 4. Periodic ICE checks, DTLS retransmission timeouts, and broadcast chunk fetching.
            _ = ice_interval.tick() => {
                let now_ms = elapsed_ms();
                let now_inst = Instant::now();

                if mode == InstanceMode::Rtc {
                    if let Some(ref mut ice_client) = ice {
                        for (addr, check_pkt) in ice_client.poll_outgoing_checks(now_ms) {
                            let _ = socket.send_to(&check_pkt, addr).await;
                        }
                    }
                    if let (Some(session), Some(deadline)) = (dtls_srtp.as_mut(), dtls_timeout)
                        && now_inst >= deadline
                        && session.handle_timeout(now_inst).is_ok()
                        && let Ok(out) = session.poll_outputs()
                    {
                        if let Some(addr) = ice.as_ref().and_then(IceLiteClient::primary_addr) {
                            for d in out.outgoing_packets {
                                let _ = socket.send_to(&d, addr).await;
                            }
                        }
                        dtls_timeout = out.next_timeout;
                        if out.just_connected && session.is_ready() && !is_connected {
                            is_connected = true;
                            let _ = event_tx.send(GroupMediaEvent::NetworkStateChanged {
                                connected: true,
                                transitioning_to_rtc: false,
                            });
                        }
                    }
                } else if mode == InstanceMode::Stream
                    && let Some(ref mut player) = broadcast_player
                {
                    let was_conn = player.is_connected();
                    if let Err(e) = player.fetch_next_chunk_if_needed().await {
                        let _ = event_tx.send(GroupMediaEvent::Error(format!(
                            "broadcast chunk decode error: {e}"
                        )));
                    }
                    if !was_conn && player.is_connected() && !is_connected {
                        is_connected = true;
                        let _ = event_tx.send(GroupMediaEvent::NetworkStateChanged {
                            connected: true,
                            transitioning_to_rtc: false,
                        });
                    }
                }
            }
        }
    }

    audio_io.stop();
    if let Some(tx) = stop_reply {
        let _ = tx.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::ice::{build_ice_binding_response, verify_stun_integrity_and_fingerprint};
    use crate::jitter::VOIP_SAMPLE_RATE;
    use crate::reflector::stun_attr;
    use tg_calls::{
        IceCandidateParam, JoinResponseMediaSection, JoinResponseTransport,
        RtpHeaderExtensionParam, RtpPayloadTypeParam,
    };

    fn extract_stun_username(packet: &[u8]) -> Option<String> {
        if packet.len() < 20 {
            return None;
        }
        let msg_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
        let end = (20 + msg_len).min(packet.len());
        let mut cursor = 20usize;
        while cursor + 4 <= end {
            let attr_type = u16::from_be_bytes([packet[cursor], packet[cursor + 1]]);
            let attr_len = u16::from_be_bytes([packet[cursor + 2], packet[cursor + 3]]) as usize;
            let val_start = cursor + 4;
            let val_end = val_start + attr_len;
            if val_end > end {
                return None;
            }
            if attr_type == stun_attr::USERNAME {
                return std::str::from_utf8(&packet[val_start..val_end])
                    .ok()
                    .map(ToOwned::to_owned);
            }
            cursor = val_start + ((attr_len + 3) & !3);
        }
        None
    }

    async fn spawn_mock_sfu(
        sfu_ufrag: &str,
        sfu_pwd: &str,
        client_fps: Vec<tg_calls::DtlsFingerprint>,
    ) -> (
        String,
        tokio::task::JoinHandle<(Option<String>, Option<u32>)>,
    ) {
        let sfu_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sfu_addr = sfu_sock.local_addr().unwrap();
        let sfu_id = DtlsIdentity::generate().unwrap();

        let resp = RtcJoinResponse {
            transport: JoinResponseTransport {
                ufrag: sfu_ufrag.to_string(),
                pwd: sfu_pwd.to_string(),
                fingerprints: vec![sfu_id.to_group_fingerprint("passive")],
                candidates: vec![IceCandidateParam {
                    port: sfu_addr.port().to_string(),
                    protocol: "udp".into(),
                    network: "0".into(),
                    generation: "0".into(),
                    id: "1".into(),
                    component: "1".into(),
                    foundation: "1".into(),
                    priority: "2130706431".into(),
                    ip: "127.0.0.1".into(),
                    kind: "host".into(),
                }],
                rtcp_mux: true,
            },
            audio: JoinResponseMediaSection {
                endpoint: String::new(),
                payload_types: vec![RtpPayloadTypeParam {
                    id: u32::from(OPUS_RTP_PAYLOAD_TYPE),
                    name: "opus".into(),
                    clockrate: 48_000,
                    channels: Some(2),
                    parameters: serde_json::Map::new(),
                }],
                rtp_hdrexts: vec![RtpHeaderExtensionParam {
                    id: 1,
                    uri: "urn:ietf:params:rtp-hdrext:ssrc-audio-level".into(),
                }],
            },
            video: JoinResponseMediaSection::default(),
        };
        let join_json = serde_json::to_string(&resp).unwrap();
        let sfu_pwd_bytes = sfu_pwd.as_bytes().to_vec();

        let handle = tokio::spawn(async move {
            let mut server =
                DtlsSrtpSession::new(&sfu_id, false, client_fps, Instant::now()).unwrap();
            let mut buf = vec![0u8; 4096];
            let mut seen_username: Option<String> = None;
            let mut seen_ssrc: Option<u32> = None;
            let stop_at = Instant::now() + Duration::from_secs(4);

            while Instant::now() < stop_at && (seen_username.is_none() || seen_ssrc.is_none()) {
                let remaining = stop_at.saturating_duration_since(Instant::now());
                let Ok(Ok((n, src))) =
                    tokio::time::timeout(remaining, sfu_sock.recv_from(&mut buf)).await
                else {
                    break;
                };
                let data = &buf[..n];
                match classify_webrtc_packet(data) {
                    WebRtcPacketKind::Stun => {
                        if verify_stun_integrity_and_fingerprint(data, Some(&sfu_pwd_bytes)).is_ok()
                        {
                            if seen_username.is_none() {
                                seen_username = extract_stun_username(data);
                            }
                            let mut tx_id = [0u8; 12];
                            tx_id.copy_from_slice(&data[8..20]);
                            let resp_pkt =
                                build_ice_binding_response(tx_id, src, Some(&sfu_pwd_bytes));
                            let _ = sfu_sock.send_to(&resp_pkt, src).await;
                        }
                    }
                    WebRtcPacketKind::Dtls => {
                        if server.handle_packet(data).is_ok()
                            && let Ok(out) = server.poll_outputs()
                        {
                            for pkt in out.outgoing_packets {
                                let _ = sfu_sock.send_to(&pkt, src).await;
                            }
                        }
                    }
                    WebRtcPacketKind::SrtpOrSrtcp => {
                        if server.is_ready()
                            && !is_rtcp_packet(data)
                            && let Ok(plain) = server.unprotect_rtp(data)
                            && let Ok((hdr, _)) = RtpHeader::parse(&plain)
                        {
                            seen_ssrc = Some(hdr.ssrc);
                        }
                    }
                    WebRtcPacketKind::Unknown => {}
                }
            }
            (seen_username, seen_ssrc)
        });

        (join_json, handle)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn regenerate_credentials_updates_running_worker_on_rejoin() {
        let mut engine = GroupMediaEngine::with_current_runtime(GroupMediaConfig {
            audio_backend: AudioBackendKind::Virtual,
            virtual_ring_capacity: VOIP_SAMPLE_RATE as usize * 4,
            dsp: VoiceDspConfig::bypass(),
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            initial_muted: false,
            ..GroupMediaConfig::default()
        })
        .unwrap();

        // Join #1 with initial credentials.
        let (ssrc1, join_json1) = engine.emit_join_payload_json(Vec::new()).unwrap();
        let payload1 = GroupJoinPayload::from_json(&join_json1).unwrap();
        let (sfu1_json, sfu1_task) =
            spawn_mock_sfu("sfu1_uf", "sfu1_password_1234567890", payload1.fingerprints).await;

        engine.set_join_response_payload(&sfu1_json).unwrap();
        let v_audio = engine.virtual_audio().unwrap();
        let mic_tone: Vec<f32> = (0..VOIP_SAMPLE_RATE as usize)
            .map(|i| {
                (2.0 * std::f32::consts::PI * 400.0 * (i as f32 / VOIP_SAMPLE_RATE as f32)).sin()
                    * 0.3
            })
            .collect();
        v_audio.push_capture_pcm(&mic_tone);

        let (seen_user1, seen_ssrc1) = sfu1_task.await.unwrap();
        assert_eq!(
            seen_user1.as_deref(),
            Some(format!("sfu1_uf:{}", payload1.ufrag).as_str()),
            "initial join must use initial ICE ufrag"
        );
        assert_eq!(
            seen_ssrc1,
            Some(ssrc1),
            "initial join must use initial SSRC"
        );

        // Rejoin #2 while `run_group_loop` is still running: regenerate credentials and apply a
        // new SFU join response (mirroring `GroupCall::rejoin`).
        engine.regenerate_credentials();
        let (ssrc2, join_json2) = engine.emit_join_payload_json(Vec::new()).unwrap();
        let payload2 = GroupJoinPayload::from_json(&join_json2).unwrap();
        assert_ne!(ssrc2, ssrc1);
        assert_ne!(payload2.ufrag, payload1.ufrag);

        let (sfu2_json, sfu2_task) =
            spawn_mock_sfu("sfu2_uf", "sfu2_password_1234567890", payload2.fingerprints).await;
        engine.set_join_response_payload(&sfu2_json).unwrap();
        v_audio.push_capture_pcm(&mic_tone);

        let (seen_user2, seen_ssrc2) = sfu2_task.await.unwrap();
        engine.stop_async().await;

        assert_eq!(
            seen_user2.as_deref(),
            Some(format!("sfu2_uf:{}", payload2.ufrag).as_str()),
            "rejoin must use regenerated ICE ufrag instead of stale worker credentials"
        );
        assert_eq!(
            seen_ssrc2,
            Some(ssrc2),
            "rejoin must send RTP with regenerated SSRC instead of stale worker SSRC"
        );
    }
}
