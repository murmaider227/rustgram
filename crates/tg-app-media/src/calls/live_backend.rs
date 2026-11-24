//! Real `CallsBackend` implementation wiring `tg-calls` state machines (`CallsInstance`, `Call`,
//! `GroupCallSession`) directly to `tg-voip`'s [`MediaEngine`] (`CallMediaEngine`) and
//! [`GroupMediaEngine`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/calls/calls_instance.cpp`
//! - `Telegram/SourceFiles/calls/calls_call.cpp`
//! - `Telegram/SourceFiles/calls/group/calls_group_call.cpp`
//!
//! Unlike [`crate::calls::mock::MockCallsBackend`] (used for offline demo mode), this backend
//! runs the real `tg-voip` audio/video/UDP/DTLS-SRTP media pipelines on a Tokio runtime and emits
//! authentic `tg-calls` network actions (`InstanceAction` / `GroupCallAction`) for an outer
//! MTProto / `tg-api` driver (or a loopback test harness) to execute.

use std::collections::HashMap;
use std::sync::Arc;

use egui::Color32;
use tg_calls::call::{CallAction, CallTimerKind, ControllerState};
use tg_calls::group::{DEFAULT_VOLUME, MAX_VOLUME, MIN_VOLUME};
use tg_calls::instance::{CallsInstance, InstanceAction};
use tg_calls::protocol::{CallProtocolConfig, ServerCallConfig};
use tg_calls::types::{
    CallId, DhConfigResponse, GroupCallObject, GroupCallParticipantData, PhoneCallUpdate,
    UpdateGroupCallConnection, UpdatePhoneCallSignalingData, UserId,
};
use tg_calls::{
    GroupCallAction, GroupCallSession, GroupCallSound, GroupCallTimerKind, InstanceMode, MuteState,
    RemoteBatteryState, VersionedGroupCallUpdate, VideoEndpoint, VideoState,
};
use tg_voip::{
    BroadcastChunkLoader, CallMediaEngine, CameraCaptureConfig, CameraDeviceInfo, GroupMediaConfig,
    GroupMediaEngine, GroupMediaEvent, MediaEngine, MediaEngineConfig, MediaEngineEvent,
    NetworkType, RgbaVideoFrame, ScreenCaptureConfig, list_cameras,
};
use tokio::runtime::{Handle, Runtime};

use crate::calls::backend::{
    ActiveCallSnapshot, AudioDeviceOption, BackendSoundEvent, CallAudioSettings, CallPeerInfo,
    CallsBackend, GroupCallSnapshot, GroupMemberView, JoinAsCandidate, RecentCallEntry,
    from_calls_peer_id, to_calls_peer_id,
};
use crate::calls::screen_picker::ScreenShareSource;
use tg_app_core::stand_in::types::PeerId;

/// Configuration for [`LiveCallsBackend`].
#[derive(Debug, Clone)]
pub struct LiveCallsConfig {
    /// Local signed-in account peer metadata.
    pub self_peer: CallPeerInfo,
    /// Server call timeout configuration (`help.getConfig`).
    pub server_config: ServerCallConfig,
    /// Advertised VoIP protocol layer and `tgcalls` library versions.
    pub protocol_config: CallProtocolConfig,
    /// Base `tg-voip` 1:1 [`MediaEngineConfig`] (hardware WASAPI by default; tests may set
    /// `AudioBackendKind::Virtual` and loopback bind address).
    pub media_config: MediaEngineConfig,
    /// Base `tg-voip` [`GroupMediaConfig`] (hardware WASAPI by default).
    pub group_media_config: GroupMediaConfig,
}

impl LiveCallsConfig {
    /// Creates a default live configuration for `self_peer`.
    #[must_use]
    pub fn new(self_peer: CallPeerInfo) -> Self {
        Self {
            self_peer,
            server_config: ServerCallConfig::default(),
            protocol_config: CallProtocolConfig::default(),
            media_config: MediaEngineConfig::default(),
            group_media_config: GroupMediaConfig::default(),
        }
    }
}

struct LiveActiveGroup {
    session: GroupCallSession,
    engine: GroupMediaEngine,
    group_peer: CallPeerInfo,
    title: String,
    can_manage: bool,
    messages_enabled: bool,
    recording: bool,
    camera_active: bool,
    screen_share_active: bool,
    local_audio_level: f32,
    ssrc_levels: HashMap<u32, f32>,
    timers: HashMap<GroupCallTimerKind, (u64, Option<u64>)>,
    snapshot: GroupCallSnapshot,
}

/// Real [`CallsBackend`] wiring `tg-calls` state machines to `tg-voip`'s [`MediaEngine`]
/// ([`CallMediaEngine`]) and [`GroupMediaEngine`].
pub struct LiveCallsBackend {
    _owned_runtime: Option<Runtime>,
    runtime_handle: Handle,
    self_peer: CallPeerInfo,
    peers: HashMap<PeerId, CallPeerInfo>,
    active_group_peers: Vec<CallPeerInfo>,
    recent_calls: Vec<RecentCallEntry>,

    instance: CallsInstance,
    media_config: MediaEngineConfig,
    group_media_config: GroupMediaConfig,
    broadcast_loader: Option<Arc<dyn BroadcastChunkLoader>>,

    call_engine: Option<MediaEngine>,
    call_timers: HashMap<CallTimerKind, u64>,
    call_remote_audio_level: f32,
    call_screen_share_active: bool,
    call_submitted_rating: Option<u8>,
    call_latest_video_frame: Option<RgbaVideoFrame>,
    call_snapshot: Option<ActiveCallSnapshot>,

    active_group: Option<LiveActiveGroup>,

    audio_settings: CallAudioSettings,
    input_devices: Vec<AudioDeviceOption>,
    output_devices: Vec<AudioDeviceOption>,
    camera_devices: Vec<CameraDeviceInfo>,

    outbound_call_actions: Vec<InstanceAction>,
    outbound_group_actions: Vec<GroupCallAction>,
    sound_queue: Vec<BackendSoundEvent>,
    toast_queue: Vec<String>,
}

impl LiveCallsBackend {
    /// Creates a new [`LiveCallsBackend`] owning a dedicated multi-threaded Tokio runtime.
    pub fn new(config: LiveCallsConfig) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("rustgram-voip")
            .build()?;
        let handle = runtime.handle().clone();
        Ok(Self::build(config, Some(runtime), handle))
    }

    /// Creates a new [`LiveCallsBackend`] sharing an existing Tokio runtime [`Handle`].
    #[must_use]
    pub fn with_runtime(config: LiveCallsConfig, runtime_handle: Handle) -> Self {
        Self::build(config, None, runtime_handle)
    }

    fn build(
        config: LiveCallsConfig,
        owned_runtime: Option<Runtime>,
        runtime_handle: Handle,
    ) -> Self {
        let self_user_id = UserId(config.self_peer.id.bare());
        let instance =
            CallsInstance::new(self_user_id, config.server_config, config.protocol_config);

        let mut peers = HashMap::new();
        peers.insert(config.self_peer.id, config.self_peer.clone());

        let mut input_devices = vec![AudioDeviceOption {
            id: tg_audio::DEFAULT_DEVICE_ID.to_owned(),
            name: "System Default Microphone".to_owned(),
        }];
        if let Ok(devs) = MediaEngine::list_audio_devices(tg_audio::DeviceType::Capture) {
            for d in devs {
                if !input_devices.iter().any(|x| x.id == d.id) {
                    input_devices.push(AudioDeviceOption {
                        id: d.id,
                        name: d.name,
                    });
                }
            }
        }

        let mut output_devices = vec![AudioDeviceOption {
            id: tg_audio::DEFAULT_DEVICE_ID.to_owned(),
            name: "System Default Speakers".to_owned(),
        }];
        if let Ok(devs) = MediaEngine::list_audio_devices(tg_audio::DeviceType::Playback) {
            for d in devs {
                if !output_devices.iter().any(|x| x.id == d.id) {
                    output_devices.push(AudioDeviceOption {
                        id: d.id,
                        name: d.name,
                    });
                }
            }
        }

        let camera_devices = list_cameras().unwrap_or_default();
        let mut audio_settings = CallAudioSettings {
            output_device_id: config.media_config.output_device_id.clone(),
            input_device_id: config.media_config.input_device_id.clone(),
            noise_suppression: config.media_config.dsp.enable_ns,
            ..CallAudioSettings::default()
        };
        if let Some(first_cam) = camera_devices.first() {
            audio_settings.camera_device_id = first_cam.id.clone();
        }

        Self {
            _owned_runtime: owned_runtime,
            runtime_handle,
            self_peer: config.self_peer,
            peers,
            active_group_peers: Vec::new(),
            recent_calls: Vec::new(),
            instance,
            media_config: config.media_config,
            group_media_config: config.group_media_config,
            broadcast_loader: None,
            call_engine: None,
            call_timers: HashMap::new(),
            call_remote_audio_level: 0.0,
            call_screen_share_active: false,
            call_submitted_rating: None,
            call_latest_video_frame: None,
            call_snapshot: None,
            active_group: None,
            audio_settings,
            input_devices,
            output_devices,
            camera_devices,
            outbound_call_actions: Vec::new(),
            outbound_group_actions: Vec::new(),
            sound_queue: Vec::new(),
            toast_queue: Vec::new(),
        }
    }

    /// Registers display metadata for a known user, group, or channel peer.
    pub fn register_peer(&mut self, peer: CallPeerInfo) {
        self.peers.insert(peer.id, peer);
    }

    /// Sets the [`BroadcastChunkLoader`] attached to newly created [`GroupMediaEngine`] instances.
    pub fn set_broadcast_loader(&mut self, loader: Arc<dyn BroadcastChunkLoader>) {
        self.broadcast_loader = Some(Arc::clone(&loader));
        if let Some(group) = self.active_group.as_mut() {
            group.engine.set_broadcast_loader(loader);
        }
    }

    /// Updates the OS network link type on the active 1:1 [`MediaEngine`].
    pub fn set_network_type(&mut self, network_type: NetworkType) {
        self.media_config.initial_network_type = network_type;
        if let Some(engine) = self.call_engine.as_mut() {
            CallMediaEngine::set_network_type(engine, network_type);
        }
    }

    /// Returns a reference to the underlying `tg-calls` [`CallsInstance`].
    #[must_use]
    pub fn calls_instance(&self) -> &CallsInstance {
        &self.instance
    }

    /// Returns a reference to the active 1:1 `tg-voip` [`MediaEngine`], if started.
    #[must_use]
    pub fn active_media_engine(&self) -> Option<&MediaEngine> {
        self.call_engine.as_ref()
    }

    /// Returns a reference to the active `tg-voip` [`GroupMediaEngine`], if in a group call.
    #[must_use]
    pub fn active_group_media_engine(&self) -> Option<&GroupMediaEngine> {
        self.active_group.as_ref().map(|g| &g.engine)
    }

    /// Returns the most recently decoded remote RGBA video frame from [`MediaEngine`], if any.
    #[must_use]
    pub fn latest_remote_video_frame(&self) -> Option<&RgbaVideoFrame> {
        self.call_latest_video_frame.as_ref()
    }

    /// Drains queued 1:1 MTProto RPC actions emitted by `tg-calls` (`GetDhConfig`, `RequestCall`,
    /// `ReceivedCall`, `AcceptCall`, `ConfirmCall`, `DiscardCall`, `SendSignalingData`, etc.).
    pub fn drain_call_network_actions(&mut self) -> Vec<InstanceAction> {
        std::mem::take(&mut self.outbound_call_actions)
    }

    /// Drains queued group call MTProto RPC actions emitted by [`GroupCallSession`].
    pub fn drain_group_network_actions(&mut self) -> Vec<GroupCallAction> {
        std::mem::take(&mut self.outbound_group_actions)
    }

    /// Feeds a `messages.getDhConfig` response into [`CallsInstance`] and executes emitted actions.
    pub fn feed_dh_config_response(&mut self, response: Result<DhConfigResponse, ()>, now_ms: u64) {
        self.instance.handle_dh_config_response(response);
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    /// Feeds a `phone.getCallConfig` completion into [`CallsInstance`].
    pub fn feed_call_config_response(&mut self, ok: bool, now_ms: u64) {
        self.instance.handle_call_config_response(ok, now_ms);
        self.process_instance_actions(now_ms);
    }

    /// Feeds an incoming `updatePhoneCall` into [`CallsInstance`] and executes emitted actions.
    pub fn feed_call_update(&mut self, update: PhoneCallUpdate, now_unix_sec: i32, now_ms: u64) {
        self.instance
            .handle_call_update(update, now_unix_sec, now_ms);
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    /// Feeds an incoming `updatePhoneCallSignalingData` through [`CallsInstance`] into [`MediaEngine`].
    pub fn feed_signaling_data(&mut self, data: UpdatePhoneCallSignalingData, now_ms: u64) -> bool {
        let handled = self.instance.handle_signaling_data(data);
        self.process_instance_actions(now_ms);
        handled
    }

    /// Feeds the RPC response of `phone.requestCall`.
    pub fn feed_request_call_result(&mut self, result: Result<PhoneCallUpdate, &str>, now_ms: u64) {
        if let Some(call) = self.instance.current_call_mut() {
            call.handle_request_call_result(result, now_ms);
        }
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    /// Feeds the RPC response of `phone.receivedCall`.
    pub fn feed_received_call_result(&mut self, result: Result<(), &str>, now_ms: u64) {
        if let Some(call) = self.instance.current_call_mut() {
            call.handle_received_call_result(result, now_ms);
        }
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    /// Feeds the RPC response of `phone.acceptCall`.
    pub fn feed_accept_call_result(&mut self, result: Result<PhoneCallUpdate, &str>, now_ms: u64) {
        if let Some(call) = self.instance.current_call_mut() {
            call.handle_accept_call_result(result, now_ms);
        }
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    /// Feeds the RPC response of `phone.confirmCall`.
    pub fn feed_confirm_call_result(&mut self, result: Result<PhoneCallUpdate, &str>, now_ms: u64) {
        if let Some(call) = self.instance.current_call_mut() {
            call.handle_confirm_call_result(result, now_ms);
        }
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    /// Feeds the RPC completion of `phone.discardCall`.
    pub fn feed_discard_call_result(&mut self, now_ms: u64) {
        if let Some(call) = self.instance.current_call_mut() {
            call.handle_discard_call_result(now_ms);
        }
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    /// Joins an existing group call with explicit server `call_id` and `access_hash`, wiring
    /// [`GroupCallSession`] to a real [`GroupMediaEngine`].
    #[allow(clippy::too_many_arguments)]
    pub fn join_existing_group_call(
        &mut self,
        group_peer_id: PeerId,
        join_as: PeerId,
        call_id: CallId,
        access_hash: i64,
        can_manage: bool,
        now_ms: u64,
    ) -> anyhow::Result<()> {
        // Stop any existing group call cleanly first.
        if let Some(mut old) = self.active_group.take() {
            old.engine.stop();
        }

        let mut cfg = self.group_media_config.clone();
        cfg.input_device_id = self.audio_settings.input_device_id.clone();
        cfg.output_device_id = self.audio_settings.output_device_id.clone();
        cfg.dsp.enable_ns = self.audio_settings.noise_suppression;

        let mut engine = GroupMediaEngine::new(cfg, self.runtime_handle.clone())?;
        if let Some(loader) = self.broadcast_loader.as_ref() {
            engine.set_broadcast_loader(Arc::clone(loader));
        }

        let group_peer = self.peer_or_fallback(group_peer_id);
        let mut session = GroupCallSession::new(
            to_calls_peer_id(group_peer.id),
            to_calls_peer_id(join_as),
            call_id,
            access_hash,
            can_manage,
            0,
            false,
        );
        session.initial_join();

        let title = group_peer.name.clone();
        let initial_snapshot = GroupCallSnapshot {
            group_peer: group_peer.clone(),
            title: title.clone(),
            join_as,
            state: session.state(),
            instance_state: session.instance_state(),
            mute_state: session.muted(),
            can_manage,
            join_muted: false,
            messages_enabled: true,
            recording: false,
            camera_active: false,
            screen_share_active: false,
            local_audio_level: 0.0,
            members: Vec::new(),
            large_video: None,
            large_video_pinned: false,
            invite_link_speaker: String::new(),
            invite_link_listener: String::new(),
            is_conference: false,
            emoji_fingerprint: None,
            e2e_verified_count: 0,
            chat_messages: Vec::new(),
            stars_top: Vec::new(),
            stars_total: 0,
            stars_my_total: 0,
            stars_scheduled: 0,
            min_paid_stars: 0,
            is_video_stream: false,
        };

        self.active_group = Some(LiveActiveGroup {
            session,
            engine,
            group_peer,
            title,
            can_manage,
            messages_enabled: true,
            recording: false,
            camera_active: false,
            screen_share_active: false,
            local_audio_level: 0.0,
            ssrc_levels: HashMap::new(),
            timers: HashMap::new(),
            snapshot: initial_snapshot,
        });
        self.instance.set_in_group_call(true);
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
        Ok(())
    }

    /// Feeds full group call metadata (`phone.getGroupCall`) into [`GroupCallSession`].
    pub fn feed_group_full_call(
        &mut self,
        call: GroupCallObject,
        participants: &[GroupCallParticipantData],
        now_ms: u64,
    ) {
        if let Some(group) = self.active_group.as_mut() {
            if let GroupCallObject::Active(ref info) = call {
                if let Some(ref title) = info.title {
                    group.title = title.clone();
                }
                group.messages_enabled = info.messages_enabled;
                group.recording = info.record_start_date.unwrap_or(0) != 0;
            }
            group.session.process_full_call(call, participants);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    /// Feeds the completion of `phone.joinGroupCall` into [`GroupCallSession`].
    pub fn feed_group_join_done(
        &mut self,
        was_mute_state: MuteState,
        was_video_stopped: bool,
        now_ms: u64,
    ) {
        if let Some(group) = self.active_group.as_mut() {
            group
                .session
                .handle_join_done(was_mute_state, was_video_stopped);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    /// Feeds an incoming `updateGroupCallConnection` JSON into [`GroupCallSession`] -> [`GroupMediaEngine`].
    pub fn feed_group_connection_update(
        &mut self,
        update: &UpdateGroupCallConnection,
        now_ms: u64,
    ) {
        if let Some(group) = self.active_group.as_mut() {
            group.session.handle_connection_update(update);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    /// Feeds a versioned participant delta (`updateGroupCallParticipants`) into [`GroupCallSession`].
    pub fn feed_group_participants_update(
        &mut self,
        update: VersionedGroupCallUpdate,
        now_ms: u64,
    ) {
        if let Some(group) = self.active_group.as_mut() {
            group.session.handle_versioned_update(update);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    fn peer_or_fallback(&self, peer_id: PeerId) -> CallPeerInfo {
        self.peers
            .get(&peer_id)
            .cloned()
            .unwrap_or_else(|| CallPeerInfo {
                id: peer_id,
                name: format!("Peer {}", peer_id.bare()),
                short_name: format!("P{}", peer_id.bare() % 100),
                initials: "TG".to_owned(),
                avatar_color: Color32::from_rgb(0x41, 0x9F, 0xD9),
                about: String::new(),
                is_channel: matches!(
                    peer_id.kind(),
                    tg_app_core::stand_in::types::PeerKind::Group
                        | tg_app_core::stand_in::types::PeerKind::Channel
                ),
                is_broadcast: false,
                members_count: 0,
            })
    }

    fn process_instance_actions(&mut self, now_ms: u64) {
        // Loop because handling local actions (e.g. RequestPermissions -> actually_answer,
        // StartController failure -> handle_controller_state_change) can emit follow-up actions.
        for _ in 0..8 {
            let actions = self.instance.take_actions();
            if actions.is_empty() {
                break;
            }
            for action in actions {
                match action {
                    InstanceAction::GetDhConfig { .. }
                    | InstanceAction::GetCallConfig
                    | InstanceAction::DiscardIncomingBusy { .. } => {
                        self.outbound_call_actions.push(action);
                    }
                    InstanceAction::Call(call_act) => match call_act {
                        CallAction::StartController(desc) => {
                            let mut cfg = self.media_config.clone();
                            cfg.input_device_id = self.audio_settings.input_device_id.clone();
                            cfg.output_device_id = self.audio_settings.output_device_id.clone();
                            cfg.dsp.enable_ns = self.audio_settings.noise_suppression;

                            let mut engine = MediaEngine::new(cfg, self.runtime_handle.clone());
                            match CallMediaEngine::start(&mut engine, *desc) {
                                Ok(()) => {
                                    if let Some(call) = self.instance.current_call()
                                        && call.muted()
                                    {
                                        CallMediaEngine::set_muted(&mut engine, true);
                                    }
                                    self.call_engine = Some(engine);
                                }
                                Err(err) => {
                                    self.toast_queue
                                        .push(format!("VoIP media engine failed to start: {err}"));
                                    if let Some(call) = self.instance.current_call_mut() {
                                        call.handle_controller_state_change(
                                            ControllerState::Failed("ERROR_AUDIO_IO".to_owned()),
                                            now_ms,
                                        );
                                    }
                                }
                            }
                        }
                        CallAction::DestroyController => {
                            if let Some(mut engine) = self.call_engine.take() {
                                let final_state = CallMediaEngine::stop(&mut engine);
                                if let Some(call) = self.instance.current_call_mut() {
                                    if !final_state.debug_log.is_empty() {
                                        call.set_debug_log(final_state.debug_log);
                                    }
                                    if final_state.preferred_relay_id != 0 {
                                        call.set_preferred_relay_id(final_state.preferred_relay_id);
                                    }
                                }
                            }
                            self.call_screen_share_active = false;
                            self.call_latest_video_frame = None;
                        }
                        CallAction::ReceiveSignalingData(bytes) => {
                            if let Some(engine) = self.call_engine.as_mut() {
                                CallMediaEngine::receive_signaling_data(engine, bytes);
                            }
                        }
                        CallAction::RequestPermissions { .. } => {
                            if let Some(call) = self.instance.current_call_mut() {
                                call.actually_answer();
                            }
                        }
                        CallAction::StartWaitingTrack { outgoing } => {
                            self.sound_queue
                                .push(BackendSoundEvent::StartWaitingTrack { outgoing });
                        }
                        CallAction::StopWaitingTrack => {
                            self.sound_queue.push(BackendSoundEvent::StopWaitingTrack);
                        }
                        CallAction::PlaySound(sound) => {
                            self.sound_queue.push(BackendSoundEvent::CallSound(sound));
                        }
                        CallAction::SetTimer { kind, duration_ms } => {
                            self.call_timers
                                .insert(kind, now_ms.saturating_add(duration_ms));
                        }
                        CallAction::CancelTimer(kind) => {
                            self.call_timers.remove(&kind);
                        }
                        CallAction::ShowRatingBox => {
                            if let Some(call) = self.instance.current_call_mut() {
                                call.take_rating_to_panel();
                            }
                        }
                        CallAction::ShowError(err) => {
                            self.toast_queue.push(err);
                        }
                        CallAction::NotifyFinished | CallAction::NotifyFailed => {
                            let keep_rating = self
                                .instance
                                .current_call()
                                .is_some_and(|c| c.rating_in_panel());
                            if !keep_rating {
                                if let Some(mut engine) = self.call_engine.take() {
                                    let _ = CallMediaEngine::stop(&mut engine);
                                }
                                self.instance.destroy_current_call();
                                self.call_timers.clear();
                                self.call_snapshot = None;
                                self.call_submitted_rating = None;
                                self.call_latest_video_frame = None;
                            }
                        }
                        CallAction::NotifyRedial
                        | CallAction::StateChanged { .. }
                        | CallAction::SignalBarCountChanged(_) => {}
                        // All MTProto RPC actions are queued for the API driver.
                        other => {
                            self.outbound_call_actions.push(InstanceAction::Call(other));
                        }
                    },
                }
            }
        }
    }

    fn poll_call_media_events(&mut self, now_ms: u64) {
        let mut events = Vec::new();
        if let Some(engine) = self.call_engine.as_mut() {
            while let Some(ev) = CallMediaEngine::poll_event(engine) {
                events.push(ev);
            }
        }
        if events.is_empty() {
            return;
        }
        for ev in events {
            match ev {
                MediaEngineEvent::StateChanged(st) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.handle_controller_state_change(st, now_ms);
                    }
                }
                MediaEngineEvent::SignalBarsChanged(bars) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.handle_controller_bar_count_change(bars);
                    }
                }
                MediaEngineEvent::SignalingData(bytes) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.emit_signaling_data(bytes);
                    }
                }
                MediaEngineEvent::RemoteMediaStateChanged { audio, video } => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.update_remote_media_state(audio, video);
                    }
                }
                MediaEngineEvent::RemoteBatteryChanged(battery) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.update_remote_battery_state(battery == RemoteBatteryState::Low);
                    }
                }
                MediaEngineEvent::AudioLevelChanged(level) => {
                    self.call_remote_audio_level = level.clamp(0.0, 1.0);
                }
                MediaEngineEvent::PreferredRelayChanged(relay_id) => {
                    if let Some(call) = self.instance.current_call_mut() {
                        call.set_preferred_relay_id(relay_id);
                    }
                }
                MediaEngineEvent::RemoteVideoFrame(frame) => {
                    self.call_latest_video_frame = Some(frame);
                }
                MediaEngineEvent::RemoteVideoRotationChanged(_)
                | MediaEngineEvent::RemoteScreencastStateChanged(_)
                | MediaEngineEvent::KeyframeRequested
                | MediaEngineEvent::DataChannelMessage { .. } => {}
            }
        }
        self.process_instance_actions(now_ms);
    }

    fn process_group_actions(&mut self, now_ms: u64) {
        for _ in 0..8 {
            let Some(group) = self.active_group.as_mut() else {
                break;
            };
            let actions = group.session.take_actions();
            if actions.is_empty() {
                break;
            }
            let mut finished = false;
            for action in actions {
                match action {
                    GroupCallAction::EmitJoinPayload => {
                        match group.engine.emit_join_payload_json(Vec::new()) {
                            Ok((ssrc, json)) => {
                                group.session.on_join_payload_ready(ssrc, json);
                            }
                            Err(err) => {
                                self.toast_queue
                                    .push(format!("Failed to build group join payload: {err}"));
                            }
                        }
                    }
                    GroupCallAction::SetJoinResponsePayload(json) => {
                        if let Err(err) = group.engine.set_join_response_payload(&json) {
                            self.toast_queue
                                .push(format!("Group media engine error: {err}"));
                        }
                    }
                    GroupCallAction::SetInstanceMuted(muted) => {
                        group.engine.set_muted(muted);
                    }
                    GroupCallAction::SetInstanceVolume { ssrc, volume } => {
                        group.engine.set_ssrc_volume(ssrc, volume);
                    }
                    GroupCallAction::RemoveInstanceSsrcs(ssrcs) => {
                        for s in &ssrcs {
                            group.ssrc_levels.remove(s);
                        }
                        group.engine.remove_ssrcs(ssrcs);
                    }
                    GroupCallAction::SetInstanceMode { mode, .. } => {
                        if mode == InstanceMode::None {
                            group.engine.stop();
                        } else if let Err(err) = group.engine.start() {
                            self.toast_queue
                                .push(format!("Group media engine start failed: {err}"));
                        }
                    }
                    GroupCallAction::PlaySound(sound) => {
                        self.sound_queue
                            .push(BackendSoundEvent::GroupCallSound(sound));
                    }
                    GroupCallAction::AllowedToSpeak => {
                        self.sound_queue.push(BackendSoundEvent::GroupCallSound(
                            GroupCallSound::AllowedToSpeak,
                        ));
                    }
                    GroupCallAction::SetTimer {
                        kind,
                        duration_ms,
                        repeating,
                    } => {
                        let interval = repeating.then_some(duration_ms);
                        group
                            .timers
                            .insert(kind, (now_ms.saturating_add(duration_ms), interval));
                    }
                    GroupCallAction::CancelTimer(kind) => {
                        group.timers.remove(&kind);
                    }
                    GroupCallAction::NotifyFinished | GroupCallAction::NotifyFailed => {
                        group.engine.stop();
                        finished = true;
                    }
                    GroupCallAction::EmitScreenJoinPayload
                    | GroupCallAction::SetScreenJoinResponsePayload(_)
                    | GroupCallAction::SetRequestedVideoChannels(_)
                    | GroupCallAction::LevelUpdate(_)
                    | GroupCallAction::ParticipantUpdated(_)
                    | GroupCallAction::StateChanged { .. }
                    | GroupCallAction::MuteStateChanged { .. } => {}
                    // All group MTProto RPC actions are queued for the API driver.
                    rpc => {
                        self.outbound_group_actions.push(rpc);
                    }
                }
            }
            if finished {
                self.active_group = None;
                self.instance.set_in_group_call(false);
                break;
            }
        }
    }

    fn poll_group_media_events(&mut self, now_ms: u64) {
        let mut events = Vec::new();
        if let Some(group) = self.active_group.as_mut() {
            while let Some(ev) = group.engine.poll_event() {
                events.push(ev);
            }
        }
        if events.is_empty() {
            return;
        }
        if let Some(group) = self.active_group.as_mut() {
            for ev in events {
                match ev {
                    GroupMediaEvent::NetworkStateChanged {
                        connected,
                        transitioning_to_rtc,
                    } => {
                        group
                            .session
                            .set_instance_connected(connected, transitioning_to_rtc);
                    }
                    GroupMediaEvent::AudioLevelsUpdated(samples) => {
                        for s in &samples {
                            if s.ssrc == 0 {
                                group.local_audio_level = s.level.clamp(0.0, 1.0);
                            } else {
                                group.ssrc_levels.insert(s.ssrc, s.level.clamp(0.0, 1.0));
                            }
                        }
                        group.session.audio_levels_updated(&samples, now_ms);
                    }
                    GroupMediaEvent::ModeChanged(_) => {}
                    GroupMediaEvent::Error(msg) => {
                        self.toast_queue.push(msg);
                    }
                }
            }
        }
        self.process_group_actions(now_ms);
    }

    fn refresh_call_snapshot(&mut self, now_ms: u64) {
        let Some(call) = self.instance.current_call() else {
            self.call_snapshot = None;
            return;
        };
        let peer_id = PeerId::from_user(call.peer_user_id().raw());
        let peer = self.peer_or_fallback(peer_id);
        let duration_ms = call.duration_ms(now_ms);
        let ended_duration_secs = if call.rating_in_panel() || call.rating_requested() {
            let secs = i64::from(call.discarded_duration()).max((duration_ms / 1000) as i64);
            Some(secs.max(1))
        } else {
            None
        };
        let voip_stats = self
            .call_engine
            .as_ref()
            .map(MediaEngine::stats)
            .unwrap_or_default();

        self.call_snapshot = Some(ActiveCallSnapshot {
            peer,
            call_type: call.call_type(),
            state: call.state(),
            duration_ms,
            ended_duration_secs,
            muted: call.muted(),
            camera_state: call.outgoing_video_state(),
            screen_share_active: self.call_screen_share_active,
            remote_audio_state: call.remote_audio_state(),
            remote_video_state: call.remote_video_state(),
            remote_battery_state: call.remote_battery_state(),
            signal_bars: *call.signal_bars(),
            emoji_fingerprint: call.emoji_fingerprint(),
            is_conference_invite: call.is_conference_invite(),
            conference_supported: call.conference_supported(),
            rating_requested: call.rating_in_panel() || call.rating_requested(),
            submitted_rating: self.call_submitted_rating,
            voip_stats,
            remote_audio_level: self.call_remote_audio_level,
        });
    }

    fn refresh_group_snapshot(&mut self) {
        let Some(group) = self.active_group.as_mut() else {
            return;
        };
        let list = group.session.participants_list();
        let join_as = from_calls_peer_id(group.session.join_as());
        let mut members = Vec::with_capacity(list.participants().len());

        for proto in list.participants() {
            let pid = from_calls_peer_id(proto.peer_id);
            let is_self = pid == join_as;
            let peer = self
                .peers
                .get(&pid)
                .cloned()
                .unwrap_or_else(|| CallPeerInfo {
                    id: pid,
                    name: format!("Peer {}", pid.bare()),
                    short_name: format!("P{}", pid.bare() % 100),
                    initials: "TG".to_owned(),
                    avatar_color: Color32::from_rgb(0x41, 0x9F, 0xD9),
                    about: String::new(),
                    is_channel: false,
                    is_broadcast: false,
                    members_count: 0,
                });
            let audio_level = if is_self {
                group.local_audio_level
            } else {
                group.ssrc_levels.get(&proto.ssrc).copied().unwrap_or(0.0)
            };
            let camera_frame_size = proto
                .video_params
                .as_ref()
                .filter(|v| !v.camera.is_empty())
                .map(|_| (1280, 720));
            let screen_frame_size = proto
                .video_params
                .as_ref()
                .filter(|v| !v.screen.is_empty())
                .map(|_| (1920, 1080));

            members.push(GroupMemberView {
                peer,
                participant: proto.clone(),
                is_self,
                audio_level,
                is_invited: false,
                is_calling: false,
                camera_frame_size,
                screen_frame_size,
            });
        }

        let large_ep = group.session.video_endpoint_large();
        let large_video = if large_ep.is_empty() {
            None
        } else {
            Some(large_ep.clone())
        };

        group.snapshot = GroupCallSnapshot {
            group_peer: group.group_peer.clone(),
            title: group.title.clone(),
            join_as,
            state: group.session.state(),
            instance_state: group.session.instance_state(),
            mute_state: group.session.muted(),
            can_manage: group.can_manage,
            join_muted: list.join_muted(),
            messages_enabled: group.messages_enabled,
            recording: group.recording,
            camera_active: group.camera_active,
            screen_share_active: group.screen_share_active,
            local_audio_level: group.local_audio_level,
            members,
            large_video,
            large_video_pinned: group.session.video_endpoint_pinned(),
            invite_link_speaker: String::new(),
            invite_link_listener: String::new(),
            is_conference: false,
            emoji_fingerprint: None,
            e2e_verified_count: 0,
            chat_messages: Vec::new(),
            stars_top: Vec::new(),
            stars_total: 0,
            stars_my_total: 0,
            stars_scheduled: 0,
            min_paid_stars: 0,
            is_video_stream: false,
        };
    }
}

impl Drop for LiveCallsBackend {
    fn drop(&mut self) {
        if let Some(mut engine) = self.call_engine.take() {
            let _ = CallMediaEngine::stop(&mut engine);
        }
        if let Some(mut group) = self.active_group.take() {
            group.engine.stop();
        }
    }
}

impl CallsBackend for LiveCallsBackend {
    fn tick(&mut self, now_ms: u64) {
        // 1. Fire any expired 1:1 call timers into `tg_calls::Call`.
        let due_call_timers: Vec<CallTimerKind> = self
            .call_timers
            .iter()
            .filter_map(|(&kind, &due)| (now_ms >= due).then_some(kind))
            .collect();
        for kind in due_call_timers {
            self.call_timers.remove(&kind);
            if let Some(call) = self.instance.current_call_mut() {
                call.on_timer(kind, now_ms);
            }
        }
        self.process_instance_actions(now_ms);

        // 2. Poll real `tg-voip::MediaEngine` events into `tg_calls::Call`.
        self.poll_call_media_events(now_ms);
        self.refresh_call_snapshot(now_ms);

        // 3. Fire any expired group call timers into `tg_calls::GroupCallSession`.
        if let Some(group) = self.active_group.as_mut() {
            let due_group_timers: Vec<(GroupCallTimerKind, Option<u64>)> = group
                .timers
                .iter()
                .filter_map(|(&kind, &(due, repeat))| (now_ms >= due).then_some((kind, repeat)))
                .collect();
            for (kind, repeat) in due_group_timers {
                if let Some(interval) = repeat {
                    group
                        .timers
                        .insert(kind, (now_ms.saturating_add(interval), Some(interval)));
                } else {
                    group.timers.remove(&kind);
                }
                group.session.on_timer(kind, now_ms);
            }
        }
        self.process_group_actions(now_ms);

        // 4. Poll real `tg-voip::GroupMediaEngine` events into `tg_calls::GroupCallSession`.
        self.poll_group_media_events(now_ms);
        self.refresh_group_snapshot();
    }

    fn active_call(&self) -> Option<&ActiveCallSnapshot> {
        self.call_snapshot.as_ref()
    }

    fn start_outgoing_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64) {
        self.call_submitted_rating = None;
        self.call_screen_share_active = false;
        self.call_latest_video_frame = None;
        let _ = self
            .instance
            .start_outgoing_call(UserId(peer_id.bare()), video, false, now_ms);
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    fn confirm_outgoing_call(&mut self, _video: bool, now_ms: u64) {
        self.instance.confirm_outgoing_call(now_ms);
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    fn simulate_incoming_call(&mut self, _peer_id: PeerId, _video: bool, _now_ms: u64) {
        self.toast_queue
            .push("Incoming calls in live mode arrive via MTProto updatePhoneCall".to_owned());
    }

    fn answer_call(&mut self, now_ms: u64) {
        if let Some(call) = self.instance.current_call_mut() {
            call.answer();
        }
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    fn hangup_call(&mut self, now_ms: u64) {
        if let Some(call) = self.instance.current_call_mut() {
            call.hangup(None, now_ms, false);
        }
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    fn redial_call(&mut self, now_ms: u64) {
        self.instance.redial_current_call();
        self.process_instance_actions(now_ms);
        self.refresh_call_snapshot(now_ms);
    }

    fn set_call_muted(&mut self, muted: bool) {
        if let Some(call) = self.instance.current_call_mut() {
            call.set_muted(muted);
        }
        if let Some(engine) = self.call_engine.as_mut() {
            CallMediaEngine::set_muted(engine, muted);
        }
        if let Some(snap) = self.call_snapshot.as_mut() {
            snap.muted = muted;
        }
    }

    fn toggle_call_camera(&mut self) {
        let currently_active = self
            .call_snapshot
            .as_ref()
            .is_some_and(|s| s.camera_state == VideoState::Active);
        if let Some(engine) = self.call_engine.as_ref() {
            if currently_active {
                engine.stop_video();
                if let Some(snap) = self.call_snapshot.as_mut() {
                    snap.camera_state = VideoState::Inactive;
                }
            } else {
                let cam_id = self.audio_settings.camera_device_id.clone();
                match engine.start_camera_video(&cam_id, CameraCaptureConfig::default()) {
                    Ok(()) => {
                        self.call_screen_share_active = false;
                        if let Some(snap) = self.call_snapshot.as_mut() {
                            snap.camera_state = VideoState::Active;
                            snap.screen_share_active = false;
                        }
                    }
                    Err(err) => {
                        self.toast_queue.push(format!("Camera unavailable: {err}"));
                    }
                }
            }
        }
    }

    fn toggle_call_screen_share(&mut self) {
        if let Some(engine) = self.call_engine.as_ref() {
            if self.call_screen_share_active {
                engine.stop_video();
                self.call_screen_share_active = false;
                if let Some(snap) = self.call_snapshot.as_mut() {
                    snap.screen_share_active = false;
                }
            } else {
                match engine.start_screen_share(ScreenCaptureConfig::default()) {
                    Ok(()) => {
                        self.call_screen_share_active = true;
                        if let Some(snap) = self.call_snapshot.as_mut() {
                            snap.screen_share_active = true;
                            snap.camera_state = VideoState::Inactive;
                        }
                    }
                    Err(err) => {
                        self.toast_queue
                            .push(format!("Screen capture unavailable: {err}"));
                    }
                }
            }
        }
    }

    fn submit_call_rating(&mut self, rating: u8, comment: String) {
        self.call_submitted_rating = Some(rating);
        if let Some(call) = self.instance.current_call_mut() {
            if comment.is_empty() {
                call.set_rating(i32::from(rating));
            } else {
                call.submit_rating(i32::from(rating), comment);
            }
        }
        self.process_instance_actions(0);
        if let Some(snap) = self.call_snapshot.as_mut() {
            snap.submitted_rating = Some(rating);
        }
    }

    fn finish_call_rating(&mut self) {
        if let Some(call) = self.instance.current_call_mut() {
            call.finish_rating();
        }
        self.process_instance_actions(0);
        self.instance.destroy_current_call();
        self.call_snapshot = None;
        self.call_submitted_rating = None;
    }

    fn active_group_call(&self) -> Option<&GroupCallSnapshot> {
        self.active_group.as_ref().map(|g| &g.snapshot)
    }

    fn join_group_call(&mut self, group_peer_id: PeerId, join_as: PeerId, now_ms: u64) {
        if let Err(err) = self.join_existing_group_call(
            group_peer_id,
            join_as,
            CallId(group_peer_id.bare()),
            0,
            false,
            now_ms,
        ) {
            self.toast_queue
                .push(format!("Failed to start group call engine: {err}"));
        }
    }

    fn switch_group_join_as(&mut self, join_as: PeerId, now_ms: u64) {
        if let Some(group) = self.active_group.as_ref() {
            let gpeer = group.group_peer.id;
            let can_manage = group.can_manage;
            let id = group.session.id();
            let access_hash = group.session.access_hash();
            let _ =
                self.join_existing_group_call(gpeer, join_as, id, access_hash, can_manage, now_ms);
        }
    }

    fn leave_group_call(&mut self, discard: bool, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            if discard && group.can_manage {
                group.session.discard();
            } else {
                group.session.hangup();
            }
        }
        self.process_group_actions(now_ms);
        if let Some(mut group) = self.active_group.take() {
            group.engine.stop();
            self.instance.set_in_group_call(false);
        }
    }

    fn set_group_mute_state(&mut self, state: MuteState, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            group.session.set_muted_and_update(state);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    fn set_group_push_to_talk(&mut self, pressed: bool, now_ms: u64) {
        let delay = self.audio_settings.push_to_talk_delay_ms;
        if let Some(group) = self.active_group.as_mut() {
            group.session.push_to_talk(pressed, delay);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    fn toggle_group_camera(&mut self, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            group.camera_active = !group.camera_active;
            group.session.toggle_camera(group.camera_active);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    fn toggle_group_screen_share(&mut self, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            group.screen_share_active = !group.screen_share_active;
            group
                .session
                .toggle_screen_sharing(group.screen_share_active);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    fn set_participant_volume(&mut self, peer_id: PeerId, volume: i32) {
        if let Some(group) = self.active_group.as_mut() {
            let clamped = volume.clamp(MIN_VOLUME, MAX_VOLUME);
            group
                .session
                .change_participant_volume(to_calls_peer_id(peer_id), clamped, false);
        }
        self.process_group_actions(0);
        self.refresh_group_snapshot();
    }

    fn set_participant_muted(&mut self, peer_id: PeerId, muted: bool) {
        if let Some(group) = self.active_group.as_mut() {
            group
                .session
                .toggle_participant_mute(to_calls_peer_id(peer_id), muted, false);
        }
        self.process_group_actions(0);
        self.refresh_group_snapshot();
    }

    fn allow_participant_to_speak(&mut self, peer_id: PeerId) {
        if let Some(group) = self.active_group.as_mut() {
            group
                .session
                .toggle_participant_mute(to_calls_peer_id(peer_id), false, false);
        }
        self.process_group_actions(0);
        self.refresh_group_snapshot();
    }

    fn select_large_video(&mut self, endpoint: Option<VideoEndpoint>, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            group
                .session
                .show_video_endpoint_large(endpoint.unwrap_or_default(), now_ms);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    fn toggle_pin_large_video(&mut self) {
        if let Some(group) = self.active_group.as_mut() {
            if group.session.video_endpoint_pinned() {
                group.session.pin_video_endpoint(VideoEndpoint::default());
            } else {
                let current = group.session.video_endpoint_large().clone();
                if !current.is_empty() {
                    group.session.pin_video_endpoint(current);
                }
            }
        }
        self.process_group_actions(0);
        self.refresh_group_snapshot();
    }

    fn set_group_join_muted(&mut self, join_muted: bool) {
        if let Some(group) = self.active_group.as_mut() {
            group.snapshot.join_muted = join_muted;
        }
    }

    fn set_group_messages_enabled(&mut self, enabled: bool) {
        if let Some(group) = self.active_group.as_mut() {
            group.messages_enabled = enabled;
            group.snapshot.messages_enabled = enabled;
        }
    }

    fn toggle_group_recording(
        &mut self,
        start: bool,
        title: Option<String>,
        video: bool,
        video_portrait: bool,
    ) {
        if let Some(group) = self.active_group.as_mut() {
            group
                .session
                .toggle_recording(start, title, video, video_portrait);
        }
        self.process_group_actions(0);
        self.refresh_group_snapshot();
    }

    fn start_call_screen_share_with_source(&mut self, source: &ScreenShareSource) {
        if let Some(engine) = self.call_engine.as_ref() {
            let cfg = ScreenCaptureConfig {
                display_id: source.display_id.clone(),
                fps: 15,
            };
            match engine.start_screen_share(cfg) {
                Ok(()) => {
                    self.call_screen_share_active = true;
                    if let Some(snap) = self.call_snapshot.as_mut() {
                        snap.screen_share_active = true;
                        snap.camera_state = VideoState::Inactive;
                    }
                }
                Err(err) => {
                    self.toast_queue
                        .push(format!("Screen capture unavailable: {err}"));
                }
            }
        }
    }

    fn start_group_screen_share_with_source(&mut self, _source: &ScreenShareSource, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut()
            && !group.screen_share_active
        {
            group.screen_share_active = true;
            group.session.toggle_screen_sharing(true);
        }
        self.process_group_actions(now_ms);
        self.refresh_group_snapshot();
    }

    fn set_noise_suppression(&mut self, enabled: bool) {
        self.audio_settings.noise_suppression = enabled;
        self.media_config.dsp.enable_ns = enabled;
        self.group_media_config.dsp.enable_ns = enabled;
    }

    fn set_audio_devices(&mut self, input_id: String, output_id: String) {
        self.audio_settings.input_device_id = input_id.clone();
        self.audio_settings.output_device_id = output_id.clone();
        self.media_config.input_device_id = input_id.clone();
        self.media_config.output_device_id = output_id.clone();
        self.group_media_config.input_device_id = input_id.clone();
        self.group_media_config.output_device_id = output_id.clone();
        if let Some(engine) = self.call_engine.as_mut() {
            CallMediaEngine::set_audio_devices(engine, &input_id, &output_id);
        }
        if let Some(group) = self.active_group.as_ref() {
            group.engine.set_audio_devices(&input_id, &output_id);
        }
    }

    fn set_camera_device(&mut self, camera_id: String) {
        self.audio_settings.camera_device_id = camera_id;
    }

    fn set_push_to_talk_settings(&mut self, enabled: bool, shortcut: String, delay_ms: u64) {
        self.audio_settings.push_to_talk_enabled = enabled;
        self.audio_settings.push_to_talk_shortcut = shortcut;
        self.audio_settings.push_to_talk_delay_ms = delay_ms.min(2_000);
    }

    fn audio_settings(&self) -> &CallAudioSettings {
        &self.audio_settings
    }

    fn input_devices(&self) -> &[AudioDeviceOption] {
        &self.input_devices
    }

    fn output_devices(&self) -> &[AudioDeviceOption] {
        &self.output_devices
    }

    fn camera_devices(&self) -> &[CameraDeviceInfo] {
        &self.camera_devices
    }

    fn recent_calls(&self) -> &[RecentCallEntry] {
        &self.recent_calls
    }

    fn delete_recent_calls(&mut self, msg_ids: &[i32]) {
        self.recent_calls
            .retain(|entry| !msg_ids.contains(&entry.msg_id));
    }

    fn active_chat_group_calls(&self) -> &[CallPeerInfo] {
        &self.active_group_peers
    }

    fn join_as_candidates(&self, _group_peer_id: PeerId) -> Vec<JoinAsCandidate> {
        vec![JoinAsCandidate {
            peer: self.self_peer.clone(),
            is_personal: true,
            subtitle: "Personal account".to_owned(),
        }]
    }

    fn drain_sound_events(&mut self) -> Vec<BackendSoundEvent> {
        std::mem::take(&mut self.sound_queue)
    }

    fn drain_toasts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.toast_queue)
    }
}

/// Helper verifying that default volume constant from `tg_calls::group` matches UI expectations.
#[allow(dead_code)]
const _DEFAULT_VOL_CHECK: i32 = DEFAULT_VOLUME;
