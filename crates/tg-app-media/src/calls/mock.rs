//! Fake [`CallsBackend`] simulating 1:1 incoming/outgoing calls, DH key exchange, `tg-voip`
//! media engine lifecycle, and group calls with synthetic speaking audio levels.
//!
//! Uses only synthetic/role peer names (no real people's names) and zero network traffic.

use std::collections::HashMap;

use egui::Color32;
use tg_calls::call::{
    Call, CallAction, CallState, CallType, ControllerDescriptor, ControllerState,
};
use tg_calls::dh::{DH_GOOD_PRIME as GOOD_PRIME, DhConfig, RANDOM_POWER_SIZE};
use tg_calls::group::{DEFAULT_VOLUME, MAX_VOLUME, MIN_VOLUME, SPEAK_LEVEL_THRESHOLD};
use tg_calls::protocol::{CallProtocolConfig, ServerCallConfig};
use tg_calls::types::{
    CallId, GroupCallInfo, GroupCallObject, GroupCallParticipantData, GroupCallParticipantVideo,
    GroupCallParticipantVideoSourceGroup, PeerId as CallsPeerId, PhoneCallAccepted,
    PhoneCallActive, PhoneCallDiscardReason, PhoneCallDiscarded, PhoneCallRequested,
    PhoneCallUpdate, PhoneCallWaiting, UserId,
};
use tg_calls::{
    AudioLevelSample, GroupCallAction, GroupCallSession, GroupCallSound, GroupCallState,
    InstanceState, MuteState, RemoteAudioState, RemoteBatteryState, VideoEndpoint, VideoState,
};
use tg_voip::{
    CallMediaEngine, CameraDeviceInfo, FinalCallState, MediaEngine, MediaEngineEvent, NetworkType,
    VoiceDspConfig, VoipResult, VoipStatsSnapshot,
};

use crate::calls::backend::{
    ActiveCallSnapshot, AudioDeviceOption, BackendSoundEvent, CallAudioSettings, CallPeerInfo,
    CallsBackend, GroupCallSnapshot, GroupMemberView, JoinAsCandidate, RecentCallDirection,
    RecentCallEntry, RtmpStreamInfo, from_calls_peer_id, to_calls_peer_id,
};
use crate::calls::screen_picker::ScreenShareSource;
use tg_app_core::stand_in::types::PeerId;

/// Minimal offline implementation of [`tg_voip::CallMediaEngine`] driven by `tg_calls::Call`
/// during mock 1:1 call simulations.
#[derive(Debug, Default)]
pub struct SimulatedVoipEngine {
    started: bool,
    muted: bool,
    dsp: VoiceDspConfig,
    input_device: String,
    output_device: String,
    network_type: NetworkType,
    pending_events: Vec<MediaEngineEvent>,
    stats: VoipStatsSnapshot,
}

impl SimulatedVoipEngine {
    /// Updates noise suppression in the underlying `tg-voip` [`VoiceDspConfig`].
    pub fn set_noise_suppression(&mut self, enabled: bool) {
        self.dsp.enable_ns = enabled;
    }

    /// Returns a snapshot of simulated VoIP telemetry.
    #[must_use]
    pub fn stats_snapshot(&self) -> VoipStatsSnapshot {
        self.stats.clone()
    }
}

impl CallMediaEngine for SimulatedVoipEngine {
    fn start(&mut self, descriptor: ControllerDescriptor) -> VoipResult<()> {
        self.started = true;
        self.stats.version = descriptor.version;
        self.stats.audio_bitrate_bps = 32_000;
        self.stats.rtt_ms = 28.0;
        self.stats.signal_bars = 4;
        self.pending_events
            .push(MediaEngineEvent::StateChanged(ControllerState::WaitInit));
        Ok(())
    }

    fn receive_signaling_data(&mut self, _data: Vec<u8>) {}

    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    fn set_audio_devices(&mut self, input_id: &str, output_id: &str) {
        self.set_audio_input_device(input_id);
        self.set_audio_output_device(output_id);
    }

    fn set_audio_input_device(&mut self, input_id: &str) {
        self.input_device = input_id.to_owned();
    }

    fn set_audio_output_device(&mut self, output_id: &str) {
        self.output_device = output_id.to_owned();
    }

    fn set_network_type(&mut self, network_type: NetworkType) {
        self.network_type = network_type;
    }

    fn poll_event(&mut self) -> Option<MediaEngineEvent> {
        if self.pending_events.is_empty() {
            None
        } else {
            Some(self.pending_events.remove(0))
        }
    }

    fn stop(&mut self) -> FinalCallState {
        self.started = false;
        FinalCallState {
            debug_log: "{\"engine\":\"tg-voip-mock\",\"status\":\"ok\"}".to_owned(),
            stats: self.stats.clone(),
            is_rating_suggested: true,
            ..FinalCallState::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimPhase {
    Idle,
    OutgoingRequesting { at_ms: u64 },
    OutgoingWaiting { at_ms: u64 },
    OutgoingRinging { at_ms: u64 },
    IncomingStarting { at_ms: u64 },
    IncomingRinging,
    Handshaking { at_ms: u64 },
    WaitingInit { at_ms: u64 },
    WaitingInitAck { at_ms: u64 },
    Established,
    Rating,
}

struct MockActiveCall {
    local: Call,
    remote: Call,
    peer: CallPeerInfo,
    voip: SimulatedVoipEngine,
    phase: SimPhase,
    screen_share_active: bool,
    camera_state: VideoState,
    remote_video_state: VideoState,
    snapshot: ActiveCallSnapshot,
}

type MemberVideoSizes = (Option<(i32, i32)>, Option<(i32, i32)>);

struct MockActiveGroup {
    session: GroupCallSession,
    group_peer: CallPeerInfo,
    title: String,
    join_started_ms: u64,
    connected_done: bool,
    messages_enabled: bool,
    recording: bool,
    camera_active: bool,
    screen_share_active: bool,
    ptt_release_at_ms: Option<u64>,
    schedule_date: Option<i32>,
    schedule_subscribed: bool,
    member_peers: Vec<(CallPeerInfo, u32, bool, bool)>, // (peer, ssrc, invited, calling)
    video_sizes: HashMap<PeerId, MemberVideoSizes>,
    snapshot: GroupCallSnapshot,
}

/// Offline [`CallsBackend`] implementation simulating complete 1:1 and group call lifecycles.
pub struct MockCallsBackend {
    self_peer: CallPeerInfo,
    peers: HashMap<PeerId, CallPeerInfo>,
    active_group_peers: Vec<CallPeerInfo>,
    active_call: Option<MockActiveCall>,
    active_group: Option<MockActiveGroup>,
    recent_calls: Vec<RecentCallEntry>,
    default_join_as: HashMap<PeerId, PeerId>,
    exported_invite_count: u32,
    cleared_history_calls: Vec<bool>,
    audio_settings: CallAudioSettings,
    input_devices: Vec<AudioDeviceOption>,
    output_devices: Vec<AudioDeviceOption>,
    camera_devices: Vec<CameraDeviceInfo>,
    sound_queue: Vec<BackendSoundEvent>,
    toast_queue: Vec<String>,
    next_call_id: u64,
    next_msg_id: i32,
}

impl Default for MockCallsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockCallsBackend {
    /// Local mock account peer ID.
    pub const PEER_SELF: PeerId = PeerId::from_user(1001);
    /// Sample peer: Design Studio.
    pub const PEER_DESIGN_STUDIO: PeerId = PeerId::from_user(2001);
    /// Sample peer: Audio Lab.
    pub const PEER_AUDIO_LAB: PeerId = PeerId::from_user(2002);
    /// Sample peer: Relay Desk.
    pub const PEER_RELAY_DESK: PeerId = PeerId::from_user(2003);
    /// Sample peer: Field Ops.
    pub const PEER_FIELD_OPS: PeerId = PeerId::from_user(2004);
    /// Sample peer: QA Bot.
    pub const PEER_QA_BOT: PeerId = PeerId::from_user(2005);
    /// Sample group: Core Systems Sync.
    pub const PEER_CORE_SYNC: PeerId = PeerId::from_channel(3001);
    /// Sample broadcast channel: Release Radio.
    pub const PEER_RELEASE_RADIO: PeerId = PeerId::from_channel(3002);

    /// Constructs a rich mock calls backend with sample contacts, active voice chats, and history.
    #[must_use]
    pub fn new() -> Self {
        let self_peer = make_peer(
            Self::PEER_SELF,
            "Desktop Operator",
            "Operator",
            "DO",
            Color32::from_rgb(0x41, 0x9F, 0xD9),
            "Local desktop account",
            false,
            false,
            0,
        );
        let design = make_peer(
            Self::PEER_DESIGN_STUDIO,
            "Design Studio",
            "Design",
            "DS",
            Color32::from_rgb(0x6D, 0x67, 0xE4),
            "UI prototyping team",
            false,
            false,
            0,
        );
        let audio = make_peer(
            Self::PEER_AUDIO_LAB,
            "Audio Lab",
            "Audio",
            "AL",
            Color32::from_rgb(0x2E, 0xA8, 0x7A),
            "Acoustics & DSP bench",
            false,
            false,
            0,
        );
        let relay = make_peer(
            Self::PEER_RELAY_DESK,
            "Relay Desk",
            "Relay",
            "RD",
            Color32::from_rgb(0xE0, 0x7A, 0x5F),
            "Transport diagnostics",
            false,
            false,
            0,
        );
        let field = make_peer(
            Self::PEER_FIELD_OPS,
            "Field Ops",
            "Ops",
            "FO",
            Color32::from_rgb(0xD9, 0x8C, 0x2B),
            "Mobile test rig",
            false,
            false,
            0,
        );
        let qa = make_peer(
            Self::PEER_QA_BOT,
            "QA Harness",
            "QA",
            "QH",
            Color32::from_rgb(0x9C, 0x5B, 0xD4),
            "Automated test peer",
            false,
            false,
            0,
        );
        let core_sync = make_peer(
            Self::PEER_CORE_SYNC,
            "Core Systems Sync",
            "Core Sync",
            "CS",
            Color32::from_rgb(0x2A, 0x8F, 0xD6),
            "Weekly architecture call",
            true,
            false,
            48,
        );
        let release_radio = make_peer(
            Self::PEER_RELEASE_RADIO,
            "Release Radio",
            "Radio",
            "RR",
            Color32::from_rgb(0xC6, 0x54, 0x93),
            "Build announcements stream",
            true,
            true,
            1_240,
        );

        let mut peers = HashMap::new();
        for p in [
            &self_peer,
            &design,
            &audio,
            &relay,
            &field,
            &qa,
            &core_sync,
            &release_radio,
        ] {
            peers.insert(p.id, p.clone());
        }

        let recent_calls = vec![
            RecentCallEntry {
                msg_id: 108,
                peer: design.clone(),
                direction: RecentCallDirection::Out,
                is_video: true,
                days_ago: 0,
                hour: 15,
                minute: 42,
                duration_secs: Some(745),
            },
            RecentCallEntry {
                msg_id: 107,
                peer: design.clone(),
                direction: RecentCallDirection::Out,
                is_video: false,
                days_ago: 0,
                hour: 14,
                minute: 10,
                duration_secs: Some(128),
            },
            RecentCallEntry {
                msg_id: 106,
                peer: audio.clone(),
                direction: RecentCallDirection::Missed,
                is_video: false,
                days_ago: 0,
                hour: 11,
                minute: 25,
                duration_secs: None,
            },
            RecentCallEntry {
                msg_id: 105,
                peer: relay.clone(),
                direction: RecentCallDirection::In,
                is_video: false,
                days_ago: 1,
                hour: 18,
                minute: 5,
                duration_secs: Some(412),
            },
            RecentCallEntry {
                msg_id: 104,
                peer: field.clone(),
                direction: RecentCallDirection::In,
                is_video: true,
                days_ago: 2,
                hour: 9,
                minute: 30,
                duration_secs: Some(1_920),
            },
        ];

        // Query real OS audio devices via `tg-voip::MediaEngine::list_audio_devices` with safe fallback.
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
        if input_devices.len() == 1 {
            input_devices.push(AudioDeviceOption {
                id: "mock-mic-studio".to_owned(),
                name: "Studio USB Condenser".to_owned(),
            });
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
        if output_devices.len() == 1 {
            output_devices.push(AudioDeviceOption {
                id: "mock-out-headphones".to_owned(),
                name: "Reference Studio Headphones".to_owned(),
            });
        }

        let camera_devices = vec![
            CameraDeviceInfo {
                id: "cam-default".to_owned(),
                name: "Integrated HD Webcam (1080p)".to_owned(),
            },
            CameraDeviceInfo {
                id: "cam-desk".to_owned(),
                name: "Desk Capture Virtual Camera".to_owned(),
            },
        ];

        Self {
            self_peer,
            peers,
            active_group_peers: vec![core_sync, release_radio],
            active_call: None,
            active_group: None,
            recent_calls,
            default_join_as: HashMap::new(),
            exported_invite_count: 0,
            cleared_history_calls: Vec::new(),
            audio_settings: CallAudioSettings::default(),
            input_devices,
            output_devices,
            camera_devices,
            sound_queue: Vec::new(),
            toast_queue: Vec::new(),
            next_call_id: 90_001,
            next_msg_id: 200,
        }
    }

    /// Directly establishes an active 1:1 call with real DH emoji fingerprint and `elapsed_secs`
    /// duration (handy for instant screenshot rendering and tests).
    pub fn setup_demo_active_call(&mut self, peer_id: PeerId, elapsed_secs: u64, now_ms: u64) {
        let started = now_ms.saturating_sub(elapsed_secs.saturating_mul(1000) + 2_000);
        self.start_outgoing_call(peer_id, false, started);
        self.confirm_outgoing_call(false, started + 10);
        for step in [250_u64, 600, 1_100, 1_500, 1_800, 2_000] {
            self.tick(started + step);
        }
        self.tick(now_ms);
    }

    /// Directly starts an incoming ringing call ready in `CallState::WaitingIncoming`.
    pub fn setup_demo_incoming_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64) {
        self.simulate_incoming_call(peer_id, video, now_ms.saturating_sub(400));
        self.tick(now_ms);
    }

    /// Directly joins the demo group call in `GroupCallState::Joined` with synthetic speaking levels.
    pub fn setup_demo_group_call(&mut self, now_ms: u64) {
        self.join_group_call(
            Self::PEER_CORE_SYNC,
            Self::PEER_SELF,
            now_ms.saturating_sub(1_200),
        );
        self.tick(now_ms.saturating_sub(600));
        self.tick(now_ms);
        self.set_group_mute_state(MuteState::Active, now_ms);
        self.tick(now_ms);
    }

    /// Configures a scheduled demo group call with a countdown bar (`calls_group_scheduled_labels.cpp`).
    pub fn setup_demo_scheduled_group_call(&mut self, now_ms: u64, schedule_date_unix: i32) {
        self.join_group_call(Self::PEER_CORE_SYNC, Self::PEER_SELF, now_ms);
        if let Some(group) = self.active_group.as_mut() {
            group.schedule_date = Some(schedule_date_unix);
            group.schedule_subscribed = false;
            group.connected_done = true;
            Self::refresh_group_snapshot(group, now_ms);
        }
    }

    /// Returns the persisted default `join_as` peer for `group_peer_id`, if saved.
    #[must_use]
    pub fn default_join_as_for(&self, group_peer_id: PeerId) -> Option<PeerId> {
        self.default_join_as.get(&group_peer_id).copied()
    }

    /// Returns the sequence of `revoke` flags passed to `delete_phone_call_history`.
    #[must_use]
    pub fn cleared_history_calls(&self) -> &[bool] {
        &self.cleared_history_calls
    }

    fn peer_or_fallback(&self, peer_id: PeerId) -> CallPeerInfo {
        self.peers.get(&peer_id).cloned().unwrap_or_else(|| {
            make_peer(
                peer_id,
                "Test Contact",
                "Contact",
                "TC",
                Color32::from_rgb(0x41, 0x9F, 0xD9),
                "",
                false,
                false,
                0,
            )
        })
    }

    fn dh_config() -> DhConfig {
        DhConfig {
            version: 1,
            g: 3,
            p: GOOD_PRIME.to_vec(),
        }
    }

    fn drain_call_actions(
        call: &mut Call,
        voip: &mut SimulatedVoipEngine,
        sounds: &mut Vec<BackendSoundEvent>,
    ) {
        for act in call.take_actions() {
            match act {
                CallAction::StartWaitingTrack { outgoing } => {
                    sounds.push(BackendSoundEvent::StartWaitingTrack { outgoing });
                }
                CallAction::StopWaitingTrack => {
                    sounds.push(BackendSoundEvent::StopWaitingTrack);
                }
                CallAction::PlaySound(s) => {
                    sounds.push(BackendSoundEvent::CallSound(s));
                }
                CallAction::StartController(desc) => {
                    let _ = voip.start(*desc);
                }
                CallAction::DestroyController => {
                    let _ = voip.stop();
                }
                _ => {}
            }
        }
    }

    fn refresh_call_snapshot(active: &mut MockActiveCall, now_ms: u64) {
        let local = &active.local;
        let duration_ms = local.duration_ms(now_ms);
        let remote_audio_level = if local.state() == CallState::Established
            && local.remote_audio_state() == RemoteAudioState::Active
        {
            0.22 + 0.45 * ((now_ms as f32 * 0.005).sin().abs())
        } else {
            0.0
        };
        let ended_duration_secs = if active.phase == SimPhase::Rating {
            Some((duration_ms / 1000).max(1) as i64)
        } else {
            None
        };
        active.snapshot = ActiveCallSnapshot {
            peer: active.peer.clone(),
            call_type: local.call_type(),
            state: local.state(),
            duration_ms,
            ended_duration_secs,
            muted: local.muted(),
            camera_state: active.camera_state,
            screen_share_active: active.screen_share_active,
            remote_audio_state: local.remote_audio_state(),
            remote_video_state: active.remote_video_state,
            remote_battery_state: local.remote_battery_state(),
            signal_bars: *local.signal_bars(),
            emoji_fingerprint: local.emoji_fingerprint(),
            is_conference_invite: local.is_conference_invite(),
            conference_supported: local.conference_supported(),
            rating_requested: active.phase == SimPhase::Rating,
            submitted_rating: active.snapshot.submitted_rating,
            voip_stats: active.voip.stats_snapshot(),
            remote_audio_level,
        };
    }

    fn refresh_group_snapshot(group: &mut MockActiveGroup, now_ms: u64) {
        let list = group.session.participants_list();
        let mut members = Vec::with_capacity(group.member_peers.len());
        let local_level = if matches!(
            group.session.muted(),
            MuteState::Active | MuteState::PushToTalk
        ) {
            0.28 + 0.52 * ((now_ms as f32 * 0.0065).sin().abs())
        } else {
            0.0
        };

        for (idx, (peer, ssrc, invited, calling)) in group.member_peers.iter().enumerate() {
            let calls_peer = to_calls_peer_id(peer.id);
            let is_self = peer.id == from_calls_peer_id(group.session.join_as());
            let proto = list
                .participants()
                .iter()
                .find(|p| p.peer_id == calls_peer)
                .cloned()
                .unwrap_or(tg_calls::GroupCallParticipant {
                    peer_id: calls_peer,
                    video_params: None,
                    date: 1_700_000_000,
                    last_active: 1_700_000_000,
                    raised_hand_rating: 0,
                    ssrc: *ssrc,
                    volume: DEFAULT_VOLUME,
                    sounding: false,
                    speaking: false,
                    additional_sounding: false,
                    additional_speaking: false,
                    muted: true,
                    muted_by_me: false,
                    can_self_unmute: true,
                    only_min_loaded: false,
                    video_joined: false,
                    apply_volume_from_min: false,
                });

            let level = if is_self {
                local_level
            } else if *invited || proto.muted || proto.muted_by_me {
                0.0
            } else {
                // Smooth synthetic voice burst per unmuted speaker.
                let phase = (now_ms as f32) * 0.004 + (idx as f32) * 1.9;
                let wave = (phase.sin() * 0.5 + 0.5).powf(1.4);
                (wave * 0.88).clamp(0.0, 1.0)
            };

            let (cam_sz, scr_sz) = group
                .video_sizes
                .get(&peer.id)
                .copied()
                .unwrap_or((None, None));
            let (cam_sz, scr_sz) = if is_self {
                (
                    group.camera_active.then_some((1280, 720)),
                    group.screen_share_active.then_some((1920, 1080)),
                )
            } else {
                (cam_sz, scr_sz)
            };

            members.push(GroupMemberView {
                peer: peer.clone(),
                participant: proto,
                is_self,
                audio_level: level,
                is_invited: *invited,
                is_calling: *calling,
                camera_frame_size: cam_sz,
                screen_frame_size: scr_sz,
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
            join_as: from_calls_peer_id(group.session.join_as()),
            state: group.session.state(),
            instance_state: group.session.instance_state(),
            mute_state: group.session.muted(),
            can_manage: true,
            join_muted: list.join_muted(),
            messages_enabled: group.messages_enabled,
            recording: group.recording,
            camera_active: group.camera_active,
            screen_share_active: group.screen_share_active,
            local_audio_level: local_level,
            members,
            large_video,
            large_video_pinned: group.session.video_endpoint_pinned(),
            invite_link_speaker: "https://t.me/coresync?voicechat=spk_demo_7a2c".to_owned(),
            invite_link_listener: "https://t.me/coresync?voicechat=lst_demo_9f41".to_owned(),
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

impl CallsBackend for MockCallsBackend {
    fn tick(&mut self, now_ms: u64) {
        // 1. Advance 1:1 call simulation through authentic `tg_calls::Call` transitions.
        if let Some(active) = self.active_call.as_mut() {
            let dh = Self::dh_config();
            let proto = CallProtocolConfig::default().to_tl_protocol();
            let self_uid = UserId(self.self_peer.id.bare());
            let peer_uid = UserId(active.peer.id.bare());

            match active.phase {
                SimPhase::OutgoingRequesting { at_ms } if now_ms >= at_ms => {
                    active
                        .local
                        .start_with_rng(dh.clone(), &[0x21; RANDOM_POWER_SIZE], 401, |b| {
                            for (i, byte) in b.iter_mut().enumerate() {
                                *byte = (i as u8).wrapping_mul(11).wrapping_add(3);
                            }
                        });
                    let mut g_a_hash = [0u8; 32];
                    for act in active.local.take_actions() {
                        if let CallAction::RequestCall { g_a_hash: h, .. } = act {
                            g_a_hash = h;
                        }
                    }
                    let call_id = CallId(self.next_call_id);
                    self.next_call_id += 1;
                    let access_hash = (call_id.0 as i64) * 17 + 99;
                    let waiting = PhoneCallWaiting {
                        video: active.camera_state == VideoState::Active,
                        id: call_id,
                        access_hash,
                        date: 1_700_000_000,
                        admin_id: self_uid,
                        participant_id: peer_uid,
                        protocol: proto.clone(),
                        receive_date: None,
                    };
                    active
                        .local
                        .handle_request_call_result(Ok(PhoneCallUpdate::Waiting(waiting)), now_ms);
                    // Deliver incoming phoneCallRequested to simulated remote peer.
                    let _ = active.remote.handle_update(
                        PhoneCallUpdate::Requested(PhoneCallRequested {
                            video: active.camera_state == VideoState::Active,
                            id: call_id,
                            access_hash,
                            date: 1_700_000_000,
                            admin_id: self_uid,
                            participant_id: peer_uid,
                            g_a_hash: g_a_hash.to_vec(),
                            protocol: proto.clone(),
                        }),
                        now_ms,
                    );
                    active
                        .remote
                        .start_with_rng(dh, &[0x44; RANDOM_POWER_SIZE], 402, |b| {
                            for (i, byte) in b.iter_mut().enumerate() {
                                *byte = (i as u8).wrapping_mul(13).wrapping_add(7);
                            }
                        });
                    let _ = active.remote.take_actions();
                    active.remote.handle_received_call_result(Ok(()), now_ms);
                    Self::drain_call_actions(
                        &mut active.local,
                        &mut active.voip,
                        &mut self.sound_queue,
                    );
                    active.phase = SimPhase::OutgoingWaiting {
                        at_ms: now_ms + 300,
                    };
                }
                SimPhase::OutgoingWaiting { at_ms } if now_ms >= at_ms => {
                    let waiting = PhoneCallWaiting {
                        video: active.camera_state == VideoState::Active,
                        id: active.local.id(),
                        access_hash: active.local.access_hash(),
                        date: 1_700_000_000,
                        admin_id: self_uid,
                        participant_id: peer_uid,
                        protocol: proto.clone(),
                        receive_date: Some(1_700_000_001),
                    };
                    let _ = active
                        .local
                        .handle_update(PhoneCallUpdate::Waiting(waiting), now_ms);
                    Self::drain_call_actions(
                        &mut active.local,
                        &mut active.voip,
                        &mut self.sound_queue,
                    );
                    active.phase = SimPhase::OutgoingRinging {
                        at_ms: now_ms + 500,
                    };
                }
                SimPhase::OutgoingRinging { at_ms } if now_ms >= at_ms => {
                    // Simulated remote peer answers and produces g_b.
                    active.remote.actually_answer();
                    let g_b = active
                        .remote
                        .take_actions()
                        .into_iter()
                        .find_map(|a| match a {
                            CallAction::AcceptCall { g_b, .. } => Some(g_b),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let accepted = PhoneCallAccepted {
                        video: active.camera_state == VideoState::Active,
                        id: active.local.id(),
                        access_hash: active.local.access_hash(),
                        date: 1_700_000_000,
                        admin_id: self_uid,
                        participant_id: peer_uid,
                        g_b: g_b.clone(),
                        protocol: proto.clone(),
                    };
                    let _ = active
                        .local
                        .handle_update(PhoneCallUpdate::Accepted(accepted), now_ms);
                    let mut confirm_info = None;
                    for act in active.local.take_actions() {
                        match act {
                            CallAction::ConfirmCall {
                                g_a,
                                key_fingerprint,
                                ..
                            } => confirm_info = Some((g_a, key_fingerprint)),
                            CallAction::StopWaitingTrack => {
                                self.sound_queue.push(BackendSoundEvent::StopWaitingTrack);
                            }
                            CallAction::PlaySound(s) => {
                                self.sound_queue.push(BackendSoundEvent::CallSound(s));
                            }
                            _ => {}
                        }
                    }
                    if let Some((_g_a, key_fp)) = confirm_info {
                        let active_msg = PhoneCallActive {
                            p2p_allowed: true,
                            video: active.camera_state == VideoState::Active,
                            conference_supported: true,
                            id: active.local.id(),
                            access_hash: active.local.access_hash(),
                            date: 1_700_000_000,
                            admin_id: self_uid,
                            participant_id: peer_uid,
                            g_a_or_b: g_b,
                            key_fingerprint: key_fp,
                            protocol: proto.clone(),
                            connections: Vec::new(),
                            start_date: 1_700_000_002,
                            custom_parameters: None,
                        };
                        active.local.handle_confirm_call_result(
                            Ok(PhoneCallUpdate::Active(active_msg)),
                            now_ms,
                        );
                        Self::drain_call_actions(
                            &mut active.local,
                            &mut active.voip,
                            &mut self.sound_queue,
                        );
                    }
                    active.phase = SimPhase::WaitingInit {
                        at_ms: now_ms + 250,
                    };
                }
                SimPhase::IncomingStarting { at_ms } if now_ms >= at_ms => {
                    active.local.handle_received_call_result(Ok(()), now_ms);
                    Self::drain_call_actions(
                        &mut active.local,
                        &mut active.voip,
                        &mut self.sound_queue,
                    );
                    active.phase = SimPhase::IncomingRinging;
                }
                SimPhase::Handshaking { at_ms } if now_ms >= at_ms => {
                    // Callee side: complete DH with caller's `g_a`.
                    let g_b = active
                        .local
                        .take_actions()
                        .into_iter()
                        .find_map(|a| match a {
                            CallAction::AcceptCall { g_b, .. } => Some(g_b),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let accepted = PhoneCallAccepted {
                        video: active.camera_state == VideoState::Active,
                        id: active.local.id(),
                        access_hash: active.local.access_hash(),
                        date: 1_700_000_000,
                        admin_id: peer_uid,
                        participant_id: self_uid,
                        g_b,
                        protocol: proto.clone(),
                    };
                    let _ = active
                        .remote
                        .handle_update(PhoneCallUpdate::Accepted(accepted), now_ms);
                    let confirm = active
                        .remote
                        .take_actions()
                        .into_iter()
                        .find_map(|a| match a {
                            CallAction::ConfirmCall {
                                g_a,
                                key_fingerprint,
                                ..
                            } => Some((g_a, key_fingerprint)),
                            _ => None,
                        });
                    if let Some((g_a, key_fp)) = confirm {
                        let active_msg = PhoneCallActive {
                            p2p_allowed: true,
                            video: active.camera_state == VideoState::Active,
                            conference_supported: true,
                            id: active.local.id(),
                            access_hash: active.local.access_hash(),
                            date: 1_700_000_000,
                            admin_id: peer_uid,
                            participant_id: self_uid,
                            g_a_or_b: g_a,
                            key_fingerprint: key_fp,
                            protocol: proto.clone(),
                            connections: Vec::new(),
                            start_date: 1_700_000_002,
                            custom_parameters: None,
                        };
                        let _ = active
                            .local
                            .handle_update(PhoneCallUpdate::Active(active_msg), now_ms);
                        Self::drain_call_actions(
                            &mut active.local,
                            &mut active.voip,
                            &mut self.sound_queue,
                        );
                    }
                    active.phase = SimPhase::WaitingInit {
                        at_ms: now_ms + 250,
                    };
                }
                SimPhase::WaitingInit { at_ms } if now_ms >= at_ms => {
                    active
                        .local
                        .handle_controller_state_change(ControllerState::WaitInitAck, now_ms);
                    active.local.handle_controller_bar_count_change(2);
                    Self::drain_call_actions(
                        &mut active.local,
                        &mut active.voip,
                        &mut self.sound_queue,
                    );
                    active.phase = SimPhase::WaitingInitAck {
                        at_ms: now_ms + 250,
                    };
                }
                SimPhase::WaitingInitAck { at_ms } if now_ms >= at_ms => {
                    active
                        .local
                        .handle_controller_state_change(ControllerState::Established, now_ms);
                    active.local.handle_controller_bar_count_change(4);
                    Self::drain_call_actions(
                        &mut active.local,
                        &mut active.voip,
                        &mut self.sound_queue,
                    );
                    active.phase = SimPhase::Established;
                }
                _ => {}
            }

            // Poll any pending `tg-voip` events into `tg-calls::Call`.
            while let Some(ev) = active.voip.poll_event() {
                match ev {
                    MediaEngineEvent::SignalBarsChanged(b) => {
                        active.local.handle_controller_bar_count_change(b);
                    }
                    MediaEngineEvent::RemoteMediaStateChanged { audio, video } => {
                        active.local.update_remote_media_state(audio, video);
                    }
                    MediaEngineEvent::RemoteBatteryChanged(b) => {
                        active
                            .local
                            .update_remote_battery_state(b == RemoteBatteryState::Low);
                    }
                    _ => {}
                }
            }

            Self::refresh_call_snapshot(active, now_ms);
        }

        // 2. Advance active group call connection & synthetic speaking audio levels.
        if let Some(group) = self.active_group.as_mut() {
            if !group.connected_done && now_ms.saturating_sub(group.join_started_ms) >= 350 {
                group.connected_done = true;
                group.session.on_join_payload_ready(
                    1001,
                    r#"{"ufrag":"mock","pwd":"mockpwd","fingerprints":[],"ssrc":1001}"#.to_owned(),
                );
                group.session.handle_join_done(MuteState::Muted, true);
                group.session.set_instance_connected(true, false);
                for act in group.session.take_actions() {
                    if let GroupCallAction::PlaySound(s) = act {
                        self.sound_queue.push(BackendSoundEvent::GroupCallSound(s));
                    }
                }
            }

            if let Some(cancel_at) = group.ptt_release_at_ms
                && now_ms >= cancel_at
            {
                group.ptt_release_at_ms = None;
                group.session.push_to_talk_cancel();
                let _ = group.session.take_actions();
            }

            // Feed synthetic audio levels into `GroupCallSession::handle_audio_levels`.
            if group.connected_done {
                let mut samples = Vec::new();
                for (idx, (peer, ssrc, invited, _)) in group.member_peers.iter().enumerate() {
                    if *invited {
                        continue;
                    }
                    let is_self = peer.id == from_calls_peer_id(group.session.join_as());
                    if is_self {
                        let lvl = if matches!(
                            group.session.muted(),
                            MuteState::Active | MuteState::PushToTalk
                        ) {
                            0.42 + 0.38 * ((now_ms as f32 * 0.006).sin().abs())
                        } else {
                            0.0
                        };
                        samples.push(AudioLevelSample {
                            ssrc: 0,
                            level: lvl,
                            voice: lvl >= SPEAK_LEVEL_THRESHOLD,
                        });
                    } else if idx == 1 || idx == 2 {
                        let phase = (now_ms as f32) * 0.0035 + (idx as f32) * 2.1;
                        let wave = (phase.sin() * 0.5 + 0.5).powf(1.3) * 0.85;
                        samples.push(AudioLevelSample {
                            ssrc: *ssrc,
                            level: wave,
                            voice: wave >= SPEAK_LEVEL_THRESHOLD,
                        });
                    }
                }
                group.session.audio_levels_updated(&samples, now_ms);
                let _ = group.session.take_actions();
            }

            Self::refresh_group_snapshot(group, now_ms);
        }
    }

    fn active_call(&self) -> Option<&ActiveCallSnapshot> {
        self.active_call.as_ref().map(|c| &c.snapshot)
    }

    fn start_outgoing_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64) {
        let peer = self.peer_or_fallback(peer_id);
        let self_uid = UserId(self.self_peer.id.bare());
        let peer_uid = UserId(peer.id.bare());
        let mut local = Call::new_outgoing(
            self_uid,
            peer_uid,
            video,
            ServerCallConfig::default(),
            CallProtocolConfig::default(),
        );
        let _ = local.take_actions();
        let remote = Call::new_incoming(
            peer_uid,
            self_uid,
            video,
            ServerCallConfig::default(),
            CallProtocolConfig::default(),
        );
        let camera_state = if video {
            VideoState::Active
        } else {
            VideoState::Inactive
        };
        let mut voip = SimulatedVoipEngine::default();
        voip.set_noise_suppression(self.audio_settings.noise_suppression);
        let mut active = MockActiveCall {
            local,
            remote,
            peer: peer.clone(),
            voip,
            phase: SimPhase::Idle,
            screen_share_active: false,
            camera_state,
            remote_video_state: VideoState::Inactive,
            snapshot: ActiveCallSnapshot {
                peer,
                call_type: CallType::Outgoing,
                state: CallState::WaitingUserConfirmation,
                duration_ms: 0,
                ended_duration_secs: None,
                muted: false,
                camera_state,
                screen_share_active: false,
                remote_audio_state: RemoteAudioState::Active,
                remote_video_state: VideoState::Inactive,
                remote_battery_state: RemoteBatteryState::Normal,
                signal_bars: tg_calls::SignalBarsState::new(),
                emoji_fingerprint: None,
                is_conference_invite: false,
                conference_supported: true,
                rating_requested: false,
                submitted_rating: None,
                voip_stats: VoipStatsSnapshot::default(),
                remote_audio_level: 0.0,
            },
        };
        Self::refresh_call_snapshot(&mut active, now_ms);
        self.active_call = Some(active);
    }

    fn confirm_outgoing_call(&mut self, video: bool, now_ms: u64) {
        if let Some(active) = self.active_call.as_mut() {
            if video {
                active.camera_state = VideoState::Active;
            }
            active.local.apply_user_confirmation();
            Self::drain_call_actions(&mut active.local, &mut active.voip, &mut self.sound_queue);
            active.phase = SimPhase::OutgoingRequesting {
                at_ms: now_ms + 150,
            };
            Self::refresh_call_snapshot(active, now_ms);
        }
    }

    fn simulate_incoming_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64) {
        let peer = self.peer_or_fallback(peer_id);
        let self_uid = UserId(self.self_peer.id.bare());
        let peer_uid = UserId(peer.id.bare());
        let dh = Self::dh_config();
        let proto = CallProtocolConfig::default().to_tl_protocol();

        // Caller (remote) generates g_a and g_a_hash.
        let mut remote = Call::new_outgoing(
            peer_uid,
            self_uid,
            video,
            ServerCallConfig::default(),
            CallProtocolConfig::default(),
        );
        remote.apply_user_confirmation();
        remote.start_with_rng(dh.clone(), &[0x52; RANDOM_POWER_SIZE], 801, |b| {
            for (i, byte) in b.iter_mut().enumerate() {
                *byte = (i as u8).wrapping_mul(17).wrapping_add(5);
            }
        });
        let g_a_hash = remote
            .take_actions()
            .into_iter()
            .find_map(|a| match a {
                CallAction::RequestCall { g_a_hash, .. } => Some(g_a_hash),
                _ => None,
            })
            .unwrap_or([0u8; 32]);

        let call_id = CallId(self.next_call_id);
        self.next_call_id += 1;
        let access_hash = (call_id.0 as i64) * 19 + 42;
        remote.handle_request_call_result(
            Ok(PhoneCallUpdate::Waiting(PhoneCallWaiting {
                video,
                id: call_id,
                access_hash,
                date: 1_700_000_000,
                admin_id: peer_uid,
                participant_id: self_uid,
                protocol: proto.clone(),
                receive_date: Some(1_700_000_001),
            })),
            now_ms,
        );
        let _ = remote.take_actions();

        // Local incoming call state machine.
        let mut local = Call::new_incoming(
            self_uid,
            peer_uid,
            video,
            ServerCallConfig::default(),
            CallProtocolConfig::default(),
        );
        let mut voip = SimulatedVoipEngine::default();
        voip.set_noise_suppression(self.audio_settings.noise_suppression);
        Self::drain_call_actions(&mut local, &mut voip, &mut self.sound_queue);

        let _ = local.handle_update(
            PhoneCallUpdate::Requested(PhoneCallRequested {
                video,
                id: call_id,
                access_hash,
                date: 1_700_000_000,
                admin_id: peer_uid,
                participant_id: self_uid,
                g_a_hash: g_a_hash.to_vec(),
                protocol: proto,
            }),
            now_ms,
        );
        local.start_with_rng(dh, &[0x73; RANDOM_POWER_SIZE], 802, |b| {
            for (i, byte) in b.iter_mut().enumerate() {
                *byte = (i as u8).wrapping_mul(23).wrapping_add(9);
            }
        });
        Self::drain_call_actions(&mut local, &mut voip, &mut self.sound_queue);

        let camera_state = if video {
            VideoState::Active
        } else {
            VideoState::Inactive
        };
        let mut active = MockActiveCall {
            local,
            remote,
            peer: peer.clone(),
            voip,
            phase: SimPhase::IncomingStarting {
                at_ms: now_ms + 120,
            },
            screen_share_active: false,
            camera_state,
            remote_video_state: VideoState::Inactive,
            snapshot: ActiveCallSnapshot {
                peer,
                call_type: CallType::Incoming,
                state: CallState::Starting,
                duration_ms: 0,
                ended_duration_secs: None,
                muted: false,
                camera_state,
                screen_share_active: false,
                remote_audio_state: RemoteAudioState::Active,
                remote_video_state: VideoState::Inactive,
                remote_battery_state: RemoteBatteryState::Normal,
                signal_bars: tg_calls::SignalBarsState::new(),
                emoji_fingerprint: None,
                is_conference_invite: false,
                conference_supported: true,
                rating_requested: false,
                submitted_rating: None,
                voip_stats: VoipStatsSnapshot::default(),
                remote_audio_level: 0.0,
            },
        };
        Self::refresh_call_snapshot(&mut active, now_ms);
        self.active_call = Some(active);
    }

    fn answer_call(&mut self, now_ms: u64) {
        if let Some(active) = self.active_call.as_mut() {
            active.local.answer();
            let _ = active.local.take_actions();
            active.local.actually_answer();
            self.sound_queue.push(BackendSoundEvent::StopWaitingTrack);
            self.sound_queue.push(BackendSoundEvent::CallSound(
                tg_calls::CallSound::Connecting,
            ));
            active.phase = SimPhase::Handshaking {
                at_ms: now_ms + 250,
            };
            Self::refresh_call_snapshot(active, now_ms);
        }
    }

    fn hangup_call(&mut self, now_ms: u64) {
        let Some(mut active) = self.active_call.take() else {
            return;
        };
        let was_established = active.local.state() == CallState::Established;
        let duration_secs = (active.local.duration_ms(now_ms) / 1000) as i64;
        let direction = if active.local.call_type() == CallType::Outgoing {
            RecentCallDirection::Out
        } else if was_established {
            RecentCallDirection::In
        } else {
            RecentCallDirection::Missed
        };

        // Record into Recent Calls history.
        self.next_msg_id += 1;
        self.recent_calls.insert(
            0,
            RecentCallEntry {
                msg_id: self.next_msg_id,
                peer: active.peer.clone(),
                direction,
                is_video: active.camera_state == VideoState::Active,
                days_ago: 0,
                hour: 16,
                minute: 5,
                duration_secs: was_established.then_some(duration_secs.max(1)),
            },
        );

        active.local.hangup(None, now_ms, false);
        Self::drain_call_actions(&mut active.local, &mut active.voip, &mut self.sound_queue);
        self.sound_queue.push(BackendSoundEvent::StopWaitingTrack);

        if was_established {
            let _ = active.local.handle_update(
                PhoneCallUpdate::Discarded(PhoneCallDiscarded {
                    need_rating: true,
                    need_debug: false,
                    video: active.camera_state == VideoState::Active,
                    id: active.local.id(),
                    reason: Some(PhoneCallDiscardReason::Hangup),
                    duration: Some(duration_secs.max(1) as i32),
                }),
                now_ms,
            );
            Self::drain_call_actions(&mut active.local, &mut active.voip, &mut self.sound_queue);
            active.phase = SimPhase::Rating;
            Self::refresh_call_snapshot(&mut active, now_ms);
            self.active_call = Some(active);
        }
    }

    fn redial_call(&mut self, now_ms: u64) {
        if let Some(peer_id) = self.active_call.as_ref().map(|c| c.peer.id) {
            self.start_outgoing_call(peer_id, false, now_ms);
            self.confirm_outgoing_call(false, now_ms);
        }
    }

    fn set_call_muted(&mut self, muted: bool) {
        if let Some(active) = self.active_call.as_mut() {
            active.local.set_muted(muted);
            active.voip.set_muted(muted);
            active.snapshot.muted = muted;
        }
    }

    fn toggle_call_camera(&mut self) {
        if let Some(active) = self.active_call.as_mut() {
            active.camera_state = if active.camera_state == VideoState::Active {
                VideoState::Inactive
            } else {
                VideoState::Active
            };
            active.snapshot.camera_state = active.camera_state;
        }
    }

    fn toggle_call_screen_share(&mut self) {
        if let Some(active) = self.active_call.as_mut() {
            active.screen_share_active = !active.screen_share_active;
            active.snapshot.screen_share_active = active.screen_share_active;
        }
    }

    fn submit_call_rating(&mut self, rating: u8, _comment: String) {
        if let Some(active) = self.active_call.as_mut() {
            active.local.set_rating(i32::from(rating));
            let _ = active.local.take_actions();
            active.snapshot.submitted_rating = Some(rating);
            self.toast_queue
                .push(format!("Thanks for rating this call ({rating}/5)"));
        }
    }

    fn finish_call_rating(&mut self) {
        self.active_call = None;
    }

    fn active_group_call(&self) -> Option<&GroupCallSnapshot> {
        self.active_group.as_ref().map(|g| &g.snapshot)
    }

    fn join_group_call(&mut self, group_peer_id: PeerId, join_as: PeerId, now_ms: u64) {
        let group_peer = self.peer_or_fallback(group_peer_id);
        let calls_group_peer = to_calls_peer_id(group_peer.id);
        let calls_join_as = to_calls_peer_id(join_as);
        let mut session = GroupCallSession::new(
            calls_group_peer,
            calls_join_as,
            CallId(77_001),
            99_887_766,
            true,
            0,
            false,
        );
        let _ = session.take_actions();

        let self_display = self.peer_or_fallback(join_as);
        let design = self.peer_or_fallback(Self::PEER_DESIGN_STUDIO);
        let audio = self.peer_or_fallback(Self::PEER_AUDIO_LAB);
        let relay = self.peer_or_fallback(Self::PEER_RELAY_DESK);
        let field = self.peer_or_fallback(Self::PEER_FIELD_OPS);
        let qa = self.peer_or_fallback(Self::PEER_QA_BOT);

        let member_peers = vec![
            (self_display.clone(), 1001, false, false),
            (design.clone(), 2001, false, false),
            (audio.clone(), 2002, false, false),
            (relay.clone(), 2003, false, false),
            (field.clone(), 2004, false, false),
            (qa.clone(), 2005, true, false),
        ];

        // Populate `ParticipantsList` via authentic `process_full_call`.
        let proto_participants = vec![
            make_group_participant(calls_join_as, 1001, true, true, true, None, None, None),
            make_group_participant(
                to_calls_peer_id(design.id),
                2001,
                false,
                true,
                false,
                None,
                Some(make_video("cam-design", 3101, false)),
                Some(make_video("scr-design", 3102, false)),
            ),
            make_group_participant(
                to_calls_peer_id(audio.id),
                2002,
                false,
                true,
                false,
                None,
                Some(make_video("cam-audio", 3201, false)),
                None,
            ),
            make_group_participant(
                to_calls_peer_id(relay.id),
                2003,
                true,
                true,
                false,
                None,
                Some(make_video("cam-relay", 3301, false)),
                None,
            ),
            make_group_participant(
                to_calls_peer_id(field.id),
                2004,
                true,
                false,
                false,
                Some(500),
                None,
                None,
            ),
        ];

        let info = GroupCallInfo {
            id: CallId(77_001),
            access_hash: 99_887_766,
            participants_count: 5,
            title: Some(format!("{} — Architecture Review", group_peer.name)),
            stream_dc_id: None,
            record_start_date: None,
            schedule_date: None,
            unmuted_video_limit: 12,
            version: 1,
            join_muted: false,
            can_change_join_muted: true,
            join_date_asc: false,
            schedule_start_subscribed: false,
            record_video_active: false,
            rtmp_stream: false,
            listeners_hidden: false,
            conference: false,
            creator: true,
            messages_enabled: true,
            can_change_messages_enabled: true,
            invite_link: Some("https://t.me/coresync?voicechat=demo".to_owned()),
            send_paid_messages_stars: None,
        };
        session.process_full_call(GroupCallObject::Active(info), &proto_participants);
        let _ = session.take_actions();

        let mut video_sizes = HashMap::new();
        video_sizes.insert(design.id, (Some((1280, 720)), Some((1920, 1080))));
        video_sizes.insert(audio.id, (Some((1280, 720)), None));
        video_sizes.insert(relay.id, (Some((960, 720)), None));

        let mut group = MockActiveGroup {
            session,
            group_peer: group_peer.clone(),
            title: format!("{} — Architecture Review", group_peer.name),
            join_started_ms: now_ms,
            connected_done: false,
            messages_enabled: true,
            recording: false,
            camera_active: false,
            screen_share_active: false,
            ptt_release_at_ms: None,
            schedule_date: None,
            schedule_subscribed: false,
            member_peers,
            video_sizes,
            snapshot: GroupCallSnapshot {
                group_peer,
                title: String::new(),
                join_as,
                state: GroupCallState::Joining,
                instance_state: InstanceState::Disconnected,
                mute_state: MuteState::Muted,
                can_manage: true,
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
            },
        };
        Self::refresh_group_snapshot(&mut group, now_ms);
        self.active_group = Some(group);
    }

    fn switch_group_join_as(&mut self, join_as: PeerId, now_ms: u64) {
        if let Some(group_peer_id) = self.active_group.as_ref().map(|g| g.group_peer.id) {
            self.join_group_call(group_peer_id, join_as, now_ms);
            self.toast_queue
                .push("Updated display identity for Voice Chat".to_owned());
        }
    }

    fn leave_group_call(&mut self, discard: bool, _now_ms: u64) {
        if let Some(mut group) = self.active_group.take() {
            if discard {
                group.session.discard();
            } else {
                group.session.hangup();
            }
            let _ = group.session.take_actions();
            self.sound_queue
                .push(BackendSoundEvent::GroupCallSound(GroupCallSound::Ended));
        }
    }

    fn set_group_mute_state(&mut self, state: MuteState, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            group.session.set_muted_and_update(state);
            let _ = group.session.take_actions();
            Self::refresh_group_snapshot(group, now_ms);
        }
    }

    fn set_group_push_to_talk(&mut self, pressed: bool, now_ms: u64) {
        let delay = self.audio_settings.push_to_talk_delay_ms;
        if let Some(group) = self.active_group.as_mut() {
            group.session.push_to_talk(pressed, delay);
            let _ = group.session.take_actions();
            if pressed {
                group.ptt_release_at_ms = None;
            } else if delay > 0 {
                group.ptt_release_at_ms = Some(now_ms.saturating_add(delay));
            }
            Self::refresh_group_snapshot(group, now_ms);
        }
    }

    fn toggle_group_camera(&mut self, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            group.camera_active = !group.camera_active;
            group.session.toggle_camera(group.camera_active);
            let _ = group.session.take_actions();
            Self::refresh_group_snapshot(group, now_ms);
        }
    }

    fn toggle_group_screen_share(&mut self, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            group.screen_share_active = !group.screen_share_active;
            group
                .session
                .toggle_screen_sharing(group.screen_share_active);
            let _ = group.session.take_actions();
            Self::refresh_group_snapshot(group, now_ms);
        }
    }

    fn set_participant_volume(&mut self, peer_id: PeerId, volume: i32) {
        if let Some(group) = self.active_group.as_mut() {
            let clamped = volume.clamp(MIN_VOLUME, MAX_VOLUME);
            group
                .session
                .change_participant_volume(to_calls_peer_id(peer_id), clamped, false);
            let _ = group.session.take_actions();
            Self::refresh_group_snapshot(group, 0);
        }
    }

    fn set_participant_muted(&mut self, peer_id: PeerId, muted: bool) {
        if let Some(group) = self.active_group.as_mut() {
            group
                .session
                .toggle_participant_mute(to_calls_peer_id(peer_id), muted, false);
            let _ = group.session.take_actions();
            Self::refresh_group_snapshot(group, 0);
        }
    }

    fn allow_participant_to_speak(&mut self, peer_id: PeerId) {
        if let Some(group) = self.active_group.as_mut() {
            group
                .session
                .toggle_participant_mute(to_calls_peer_id(peer_id), false, false);
            let _ = group.session.take_actions();
            self.sound_queue.push(BackendSoundEvent::GroupCallSound(
                GroupCallSound::AllowedToSpeak,
            ));
            Self::refresh_group_snapshot(group, 0);
        }
    }

    fn select_large_video(&mut self, endpoint: Option<VideoEndpoint>, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            let ep = endpoint.unwrap_or_default();
            group.session.show_video_endpoint_large(ep, now_ms);
            let _ = group.session.take_actions();
            Self::refresh_group_snapshot(group, now_ms);
        }
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
            let _ = group.session.take_actions();
            Self::refresh_group_snapshot(group, 0);
        }
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
            group.recording = start;
            group.snapshot.recording = start;
            let _ = (title, video, video_portrait);
            let msg = if start {
                "Group call recording started (demo)."
            } else {
                "Group call recording stopped (demo)."
            };
            self.toast_queue.push(msg.to_owned());
        }
    }

    fn request_group_rtmp_url(
        &mut self,
        peer_id: PeerId,
        revoke: bool,
    ) -> Result<RtmpStreamInfo, String> {
        let suffix = if revoke { "revoked_key" } else { "demo_key" };
        Ok(RtmpStreamInfo {
            url: "rtmps://rtmp.example.test/s/".to_owned(),
            key: format!("{}:{suffix}", peer_id.bare()),
        })
    }

    fn start_call_screen_share_with_source(&mut self, source: &ScreenShareSource) {
        if let Some(active) = self.active_call.as_mut() {
            active.screen_share_active = true;
            active.snapshot.screen_share_active = true;
            self.toast_queue.push(format!("Sharing: {}", source.name));
        }
    }

    fn start_group_screen_share_with_source(&mut self, source: &ScreenShareSource, now_ms: u64) {
        if let Some(group) = self.active_group.as_mut() {
            group.screen_share_active = true;
            group.session.toggle_screen_sharing(true);
            let _ = group.session.take_actions();
            Self::refresh_group_snapshot(group, now_ms);
            self.toast_queue.push(format!("Sharing: {}", source.name));
        }
    }

    fn set_noise_suppression(&mut self, enabled: bool) {
        self.audio_settings.noise_suppression = enabled;
        if let Some(active) = self.active_call.as_mut() {
            active.voip.set_noise_suppression(enabled);
        }
    }

    fn set_audio_devices(&mut self, input_id: String, output_id: String) {
        self.audio_settings.input_device_id = input_id.clone();
        self.audio_settings.output_device_id = output_id.clone();
        if let Some(active) = self.active_call.as_mut() {
            active.voip.set_audio_devices(&input_id, &output_id);
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

    fn export_group_call_invite(&mut self, can_self_unmute: bool) -> Result<String, String> {
        let Some(group) = self.active_group.as_mut() else {
            return Err("No active group call".to_owned());
        };
        self.exported_invite_count += 1;
        let role = if can_self_unmute { "spk" } else { "lst" };
        let link = format!(
            "https://t.me/coresync?voicechat={role}_{}",
            self.exported_invite_count
        );
        if can_self_unmute {
            group.snapshot.invite_link_speaker = link.clone();
        } else {
            group.snapshot.invite_link_listener = link.clone();
        }
        Ok(link)
    }

    fn invite_to_group_call(&mut self, user_ids: &[PeerId]) -> Result<Vec<PeerId>, String> {
        let Some(group) = self.active_group.as_mut() else {
            return Err("No active group call".to_owned());
        };
        let mut invited = Vec::new();
        for &pid in user_ids {
            if group.member_peers.iter().any(|(p, _, _, _)| p.id == pid) {
                continue;
            }
            let peer = self.peers.get(&pid).cloned().unwrap_or_else(|| {
                make_peer(
                    pid,
                    &format!("User {}", pid.bare()),
                    &format!("U{}", pid.bare()),
                    "US",
                    Color32::from_rgb(0x41, 0x9F, 0xD9),
                    "Invited participant",
                    false,
                    false,
                    0,
                )
            });
            let next_ssrc = 2100 + group.member_peers.len() as u32;
            group.member_peers.push((peer, next_ssrc, true, false));
            invited.push(pid);
        }
        Self::refresh_group_snapshot(group, 0);
        if !invited.is_empty() {
            self.toast_queue.push(format!(
                "Invited {} participant(s) to Voice Chat",
                invited.len()
            ));
        }
        Ok(invited)
    }

    fn save_default_group_call_join_as(
        &mut self,
        group_peer_id: PeerId,
        join_as: PeerId,
    ) -> Result<bool, String> {
        self.default_join_as.insert(group_peer_id, join_as);
        Ok(true)
    }

    fn start_scheduled_group_call(&mut self) -> Result<(), String> {
        let Some(group) = self.active_group.as_mut() else {
            return Err("No active scheduled group call".to_owned());
        };
        group.schedule_date = None;
        group.schedule_subscribed = false;
        group.connected_done = true;
        Self::refresh_group_snapshot(group, 0);
        self.toast_queue
            .push("Scheduled Voice Chat started".to_owned());
        Ok(())
    }

    fn toggle_group_call_start_subscription(&mut self, subscribed: bool) -> Result<(), String> {
        let Some(group) = self.active_group.as_mut() else {
            return Err("No active scheduled group call".to_owned());
        };
        group.schedule_subscribed = subscribed;
        let msg = if subscribed {
            "Reminder set for scheduled Voice Chat"
        } else {
            "Reminder cancelled"
        };
        self.toast_queue.push(msg.to_owned());
        Ok(())
    }

    fn delete_phone_call_history(&mut self, revoke: bool) -> Result<usize, String> {
        let count = self.recent_calls.len();
        self.recent_calls.clear();
        self.cleared_history_calls.push(revoke);
        Ok(count)
    }

    fn active_group_call_schedule_date(&self) -> Option<i32> {
        self.active_group.as_ref().and_then(|g| g.schedule_date)
    }

    fn active_group_call_schedule_subscribed(&self) -> bool {
        self.active_group
            .as_ref()
            .is_some_and(|g| g.schedule_subscribed)
    }

    fn group_invite_candidates(&self) -> Vec<CallPeerInfo> {
        let mut list: Vec<CallPeerInfo> = self
            .peers
            .values()
            .filter(|p| !p.is_channel && p.id != self.self_peer.id)
            .cloned()
            .collect();
        list.sort_by_key(|p| p.id.bare());
        list
    }

    fn active_chat_group_calls(&self) -> &[CallPeerInfo] {
        &self.active_group_peers
    }

    fn join_as_candidates(&self, _group_peer_id: PeerId) -> Vec<JoinAsCandidate> {
        let mut out = vec![JoinAsCandidate {
            peer: self.self_peer.clone(),
            is_personal: true,
            subtitle: "Personal account".to_owned(),
        }];
        for grp in &self.active_group_peers {
            let sub = if grp.is_broadcast {
                format!("{} subscribers", grp.members_count)
            } else {
                format!("{} members", grp.members_count)
            };
            out.push(JoinAsCandidate {
                peer: grp.clone(),
                is_personal: false,
                subtitle: sub,
            });
        }
        out
    }

    fn drain_sound_events(&mut self) -> Vec<BackendSoundEvent> {
        std::mem::take(&mut self.sound_queue)
    }

    fn drain_toasts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.toast_queue)
    }
}

#[allow(clippy::too_many_arguments)]
fn make_peer(
    id: PeerId,
    name: &str,
    short_name: &str,
    initials: &str,
    avatar_color: Color32,
    about: &str,
    is_channel: bool,
    is_broadcast: bool,
    members_count: u32,
) -> CallPeerInfo {
    CallPeerInfo {
        id,
        name: name.to_owned(),
        short_name: short_name.to_owned(),
        initials: initials.to_owned(),
        avatar_color,
        about: about.to_owned(),
        is_channel,
        is_broadcast,
        members_count,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_group_participant(
    peer: CallsPeerId,
    source: u32,
    muted: bool,
    can_self_unmute: bool,
    is_self: bool,
    raise_hand_rating: Option<u64>,
    video: Option<GroupCallParticipantVideo>,
    presentation: Option<GroupCallParticipantVideo>,
) -> GroupCallParticipantData {
    GroupCallParticipantData {
        muted,
        left: false,
        can_self_unmute,
        just_joined: false,
        versioned: false,
        min: false,
        muted_by_you: false,
        volume_by_admin: false,
        is_self,
        video_joined: video.is_some() || presentation.is_some(),
        peer,
        date: 1_700_000_000,
        active_date: Some(1_700_000_050),
        source,
        volume: Some(DEFAULT_VOLUME),
        about: None,
        raise_hand_rating,
        video,
        presentation,
    }
}

fn make_video(endpoint: &str, ssrc: u32, paused: bool) -> GroupCallParticipantVideo {
    GroupCallParticipantVideo {
        paused,
        endpoint: endpoint.to_owned(),
        source_groups: vec![GroupCallParticipantVideoSourceGroup {
            semantics: "SIM".to_owned(),
            sources: vec![ssrc],
        }],
        audio_source: None,
    }
}
