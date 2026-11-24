//! Group call protocol state machine: lifecycle, mute/raise-hand, volume, speaking
//! indicators, join/rejoin/presentation flow, and active video endpoints.
//!
//! Port of `calls/group/calls_group_call.h` and `calls/group/calls_group_call.cpp`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use tg_crypto::e2e::E2eKeyPair;

use crate::call::VideoState;
use crate::group::e2e::{ConferenceEncryptDecrypt, E2eAction, E2eCall, E2eError, SUBCHAINS_COUNT};
use crate::group::messages::{GroupCallMessages, GroupMessageAction};
use crate::group::participant::{
    ApplySliceSource, GroupCallParticipant, LastSpokeTimes, ParticipantUpdate, ParticipantsList,
    VersionedGroupCallUpdate,
};
use crate::group::payload::{ParsedJoinResponse, RtmpInfo, parse_join_response};
use crate::group::video::{
    RequestedVideoTrackInput, VideoChannelDescription, VideoEndpoint, VideoEndpointType,
    VideoQuality, compute_requested_video_channels,
};
use crate::group::{
    CHECK_JOINED_TIMEOUT_MS, CHECK_LAST_SPOKE_INTERVAL_MS, DEFAULT_VOLUME,
    FIX_MANUAL_LARGE_VIDEO_MS, FIX_SPEAKING_LARGE_VIDEO_MS, KEEP_LAST_SPOKE_FOR_MS, MAX_VOLUME,
    MIN_VOLUME, REQUEST_PER_PAGE, RTMP_ENDPOINT_ID, SPEAK_LEVEL_THRESHOLD,
    UPDATE_SEND_ACTION_EACH_MS, WAIT_FOR_UPDATES_TIMEOUT_MS,
};
use crate::types::{
    CallId, GroupCallDiscarded, GroupCallInfo, GroupCallObject, GroupCallParticipantData,
    GroupCallStreamChannel, InputGroupCall, PeerId, UnixTime, UpdateGroupCallChainBlocks,
    UpdateGroupCallConnection, UpdateGroupCallParticipants, UserId,
};

/// Group call lifecycle states (`Calls::GroupCall::State` in `calls_group_call.h:341-351`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupCallState {
    /// Creating a brand new group call (`phone.createGroupCall`).
    Creating,
    /// Scheduled group call waiting for its start time.
    Waiting,
    /// Generating local WebRTC join payload and calling `phone.joinGroupCall`.
    Joining,
    /// Join succeeded; waiting for WebRTC/stream transport to connect.
    Connecting,
    /// Connected and participating in the group call.
    Joined,
    /// Leaving after an error (`phone.leaveGroupCall` in flight).
    FailedHangingUp,
    /// Terminal failed state.
    Failed,
    /// Leaving cleanly (`phone.leaveGroupCall` in flight).
    HangingUp,
    /// Terminal ended state.
    Ended,
}

/// Underlying WebRTC / broadcast engine network state (`GroupCall::InstanceState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstanceState {
    /// Transport disconnected.
    Disconnected,
    /// Transitioning from broadcast stream to interactive RTC.
    TransitionToRtc,
    /// Transport connected.
    Connected,
}

/// Media instance transport mode (`GroupCall::InstanceMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstanceMode {
    /// Not started.
    None,
    /// Interactive WebRTC SFU connection.
    Rtc,
    /// One-way chunked broadcast / RTMP stream.
    Stream,
}

/// Local microphone / speaking permission state (`Calls::MuteState` in `calls_group_call.h:71-77`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MuteState {
    /// Unmuted and speaking freely.
    Active,
    /// Temporarily unmuted via Push-to-Talk shortcut.
    PushToTalk,
    /// Self-muted (may unmute at any time).
    Muted,
    /// Muted by admin (`!can_self_unmute`); cannot self-unmute.
    ForceMuted,
    /// Muted by admin with hand raised to request speaking permission.
    RaisedHand,
}

/// Single audio level sample emitted by the media engine (`tgcalls::GroupLevelUpdate`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioLevelSample {
    /// Audio SSRC (0 represents the local user's microphone).
    pub ssrc: u32,
    /// Normalized amplitude in `0.0..=1.0`.
    pub level: f32,
    /// Voice activity detector flag.
    pub voice: bool,
}

/// Normalized audio level event forwarded to the UI (`Calls::LevelUpdate`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelUpdate {
    /// Resolved audio SSRC.
    pub ssrc: u32,
    /// Amplitude (zeroed when local user is muted).
    pub value: f32,
    /// Voice activity flag (false when local user is muted).
    pub voice: bool,
    /// `true` if this sample belongs to the local user.
    pub me: bool,
}

/// Sound effects requested by [`GroupCallSession`] (`GroupCall::Delegate::GroupCallSound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupCallSound {
    /// Successfully joined for the first time (`group_call_start`).
    Started,
    /// Reconnecting pulse (`group_call_connect`).
    Connecting,
    /// Admin granted permission to speak (`group_call_allowed`).
    AllowedToSpeak,
    /// Hanging up / leaving (`group_call_end`).
    Ended,
    /// Recording started (`group_call_recording_start`).
    RecordingStarted,
}

/// Timers managed by the caller for [`GroupCallSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupCallTimerKind {
    /// Periodic `checkLastSpoke` sweep (`_lastSpokeCheckTimer`).
    CheckLastSpoke,
    /// Periodic `phone.checkGroupCall` while connecting (`_checkJoinedTimer`, 4 000 ms).
    CheckJoined,
    /// Push-to-Talk release delay (`_pushToTalkCancelTimer`).
    PushToTalkCancel,
    /// Version-gap reload timer (`_reloadByQueuedUpdatesTimer`, 3 000 ms;
    /// `data/data_group_call.cpp:628-634`).
    ReloadByQueuedUpdates,
}

/// Pending self-update flags (`GroupCall::SendUpdateType`, `calls_group_call.h:504-510`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfUpdateKind {
    /// Toggle `muted`.
    Mute,
    /// Toggle `raise_hand`.
    RaiseHand,
    /// Toggle `video_stopped`.
    CameraStopped,
    /// Toggle `video_paused`.
    CameraPaused,
    /// Toggle `presentation_paused`.
    ScreenPaused,
}

/// Actions emitted by [`GroupCallSession`] (Sans-IO).
#[derive(Debug, Clone, PartialEq)]
pub enum GroupCallAction {
    /// Request local media engine to emit a WebRTC join payload (`emitJoinPayload`).
    EmitJoinPayload,
    /// Request screen-share media instance to emit a presentation join payload.
    EmitScreenJoinPayload,
    /// Send `phone.createGroupCall`.
    CreateGroupCall {
        /// Owning chat/channel peer ID.
        peer: PeerId,
        /// Random deduplication ID.
        random_id: i32,
        /// Optional scheduled start Unix timestamp.
        schedule_date: Option<UnixTime>,
        /// Whether to create an RTMP stream.
        rtmp_stream: bool,
    },
    /// Send `phone.joinGroupCall`.
    JoinGroupCall {
        /// Group call handle.
        call: InputGroupCall,
        /// Peer to join as.
        join_as: PeerId,
        /// Start muted flag.
        muted: bool,
        /// Video stopped flag.
        video_stopped: bool,
        /// Optional invite hash.
        invite_hash: Option<String>,
        /// WebRTC join payload JSON.
        params_json: String,
    },
    /// Send `phone.leaveGroupCall`.
    LeaveGroupCall {
        /// Group call handle.
        call: InputGroupCall,
        /// Local audio SSRC being left.
        source: u32,
    },
    /// Send `phone.discardGroupCall`.
    DiscardGroupCall {
        /// Group call handle.
        call: InputGroupCall,
    },
    /// Send `phone.checkGroupCall`.
    CheckGroupCall {
        /// Group call handle.
        call: InputGroupCall,
        /// Local audio/presentation SSRCs to verify.
        sources: Vec<u32>,
    },
    /// Send `phone.joinGroupCallPresentation`.
    JoinGroupCallPresentation {
        /// Group call handle.
        call: InputGroupCall,
        /// Screen-share WebRTC join payload JSON.
        params_json: String,
    },
    /// Send `phone.leaveGroupCallPresentation`.
    LeaveGroupCallPresentation {
        /// Group call handle.
        call: InputGroupCall,
    },
    /// Send `phone.editGroupCallParticipant`.
    EditGroupCallParticipant {
        /// Group call handle.
        call: InputGroupCall,
        /// Target participant peer ID.
        participant: PeerId,
        /// Optional `muted` flag.
        muted: Option<bool>,
        /// Optional `volume` (`1..=20000`; omitted on self-updates).
        volume: Option<i32>,
        /// Optional `raise_hand` flag.
        raise_hand: Option<bool>,
        /// Optional `video_stopped` flag.
        video_stopped: Option<bool>,
        /// Optional `video_paused` flag.
        video_paused: Option<bool>,
        /// Optional `presentation_paused` flag.
        presentation_paused: Option<bool>,
    },
    /// Send `phone.editGroupCallTitle`.
    EditGroupCallTitle {
        /// Group call handle.
        call: InputGroupCall,
        /// New title.
        title: String,
    },
    /// Send `phone.toggleGroupCallRecord`.
    ToggleGroupCallRecord {
        /// Group call handle.
        call: InputGroupCall,
        /// Start or stop recording.
        start: bool,
        /// Optional title.
        title: Option<String>,
        /// Record video.
        video: bool,
        /// Portrait orientation when recording video.
        video_portrait: bool,
    },
    /// Send `phone.getGroupParticipants` to resolve unknown speaking SSRCs.
    ResolveUnknownSsrcs {
        /// Group call handle.
        call: InputGroupCall,
        /// Unresolved audio SSRCs.
        ssrcs: Vec<u32>,
    },
    /// Send `phone.getGroupCall` to reload call metadata/participants after a version gap
    /// (`data/data_group_call.cpp:669-672`, `limit = 3`).
    ReloadGroupCall {
        /// Group call handle.
        call: InputGroupCall,
        /// Participant slice limit (`3` in `Data::GroupCall::reload`).
        limit: i32,
    },
    /// Send `phone.getGroupParticipants` with pagination `offset` and `limit` (`50`) after a
    /// reload when `phone.getGroupCall` returned fewer participants than previously loaded
    /// (`data/data_group_call.cpp:187-195`).
    GetGroupParticipants {
        /// Group call handle.
        call: InputGroupCall,
        /// Pagination offset (`participants_next_offset`).
        offset: String,
        /// Page size (`REQUEST_PER_PAGE = 50`).
        limit: i32,
    },
    /// Send `phone.getGroupCallStreamChannels` (on `stream_dc_id`) for RTMP stream timestamp
    /// synchronization (`calls/group/calls_group_call.cpp:3345-3370`).
    GetGroupCallStreamChannels {
        /// Group call handle.
        call: InputGroupCall,
        /// Broadcast stream DC ID (`GroupCall.stream_dc_id`), if known.
        stream_dc_id: Option<i32>,
    },
    /// Send typing/speaking action in the chat (`Api::SendProgressType::Speaking`).
    SendSpeakingAction {
        /// Owning chat/channel peer ID.
        peer: PeerId,
    },
    /// Update media instance microphone mute state.
    SetInstanceMuted(bool),
    /// Update media instance playback volume (`0.0..=2.0`) for an audio SSRC.
    SetInstanceVolume {
        /// Target audio SSRC (1 for RTMP).
        ssrc: u32,
        /// Linear volume multiplier (`volume / 10000.0`, or `0.0` if locally muted).
        volume: f64,
    },
    /// Remove departed SSRCs from the media instance.
    RemoveInstanceSsrcs(Vec<u32>),
    /// Set media instance connection mode (`None`, `Rtc`, or `Stream`).
    SetInstanceMode {
        /// New mode.
        mode: InstanceMode,
        /// Whether this is an RTMP stream.
        rtmp: bool,
    },
    /// Deliver SFU join response JSON to the main media instance.
    SetJoinResponsePayload(String),
    /// Deliver SFU join response JSON to the screen-share media instance.
    SetScreenJoinResponsePayload(String),
    /// Update the set of requested incoming video channels and qualities.
    SetRequestedVideoChannels(Vec<VideoChannelDescription>),
    /// Forward a normalized audio level update to the UI.
    LevelUpdate(LevelUpdate),
    /// Participant list diff emitted.
    ParticipantUpdated(Box<ParticipantUpdate>),
    /// Group call state transitioned.
    StateChanged {
        /// Previous state.
        from: GroupCallState,
        /// New state.
        to: GroupCallState,
    },
    /// Local mute state changed.
    MuteStateChanged {
        /// Previous mute state.
        from: MuteState,
        /// New mute state.
        to: MuteState,
    },
    /// User was just un-force-muted by an admin and allowed to speak.
    AllowedToSpeak,
    /// Play a group call sound effect.
    PlaySound(GroupCallSound),
    /// Arm or reset a timer.
    SetTimer {
        /// Timer kind.
        kind: GroupCallTimerKind,
        /// Duration in milliseconds.
        duration_ms: u64,
        /// Whether the timer repeats until cancelled.
        repeating: bool,
    },
    /// Cancel a timer.
    CancelTimer(GroupCallTimerKind),
    /// Notify delegate that group call finished cleanly.
    NotifyFinished,
    /// Notify delegate that group call failed.
    NotifyFailed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum JoinAction {
    #[default]
    None,
    Joining,
    Leaving,
}

#[derive(Debug, Clone, Default)]
struct JoinState {
    ssrc: u32,
    action: JoinAction,
    payload_ssrc: u32,
    payload_json: String,
    next_action_pending: bool,
}

impl JoinState {
    fn finish(&mut self, updated_ssrc: u32) {
        self.action = JoinAction::None;
        self.ssrc = updated_ssrc;
    }
}

/// Active video track state tracked per [`VideoEndpoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveVideoTrack {
    /// Track owner peer ID.
    pub peer_id: PeerId,
    /// Whether the track is paused.
    pub paused: bool,
    /// Desired quality tier.
    pub quality: VideoQuality,
    /// Whether at least one frame has been rendered (or track started paused).
    pub shown: bool,
}

#[derive(Debug, Default)]
struct SubchainRequestTracker {
    pending: Vec<(Vec<Vec<u8>>, i32)>,
    in_flight: bool,
    in_short_poll: bool,
}

/// Sans-IO group call protocol state machine.
///
/// Combines `Calls::GroupCall` (`calls_group_call.cpp`) with `Data::GroupCall`
/// (`data_group_call.cpp`) into a clean, deterministic state machine.
#[derive(Debug)]
pub struct GroupCallSession {
    peer_id: PeerId,
    join_as: PeerId,
    can_manage: bool,
    id: CallId,
    access_hash: i64,
    stream_dc_id: Option<i32>,
    join_hash: Option<String>,
    conference_slug: Option<String>,
    conference: bool,
    e2e: Option<E2eCall>,
    e2e_subchains: [SubchainRequestTracker; SUBCHAINS_COUNT],
    messages: GroupCallMessages,

    state: GroupCallState,
    instance_state: InstanceState,
    instance_transitioning: bool,
    instance_mode: InstanceMode,
    had_joined_state: bool,
    rtmp: bool,
    rtmp_info: RtmpInfo,
    video_stream: bool,
    single_source_volume: i32,
    schedule_date: UnixTime,

    muted: MuteState,
    initial_mute_state_sent: bool,
    self_update_in_flight: bool,
    pending_self_updates: Vec<SelfUpdateKind>,
    queued_self_updates: Vec<GroupCallParticipantData>,

    join_state: JoinState,
    screen_join_state: JoinState,
    my_ssrcs: HashSet<u32>,

    camera_state: VideoState,
    screen_state: VideoState,
    camera_endpoint: String,
    screen_endpoint: String,
    video_is_working: bool,

    active_video_tracks: BTreeMap<VideoEndpoint, ActiveVideoTrack>,
    shown_video_tracks: BTreeSet<VideoEndpoint>,
    video_endpoint_large: VideoEndpoint,
    video_endpoint_pinned: bool,
    video_large_till_ms: u64,

    last_spoke: HashMap<u32, LastSpokeTimes>,
    last_spoke_timer_active: bool,
    last_send_progress_ms: u64,
    reload_by_queued_timer_active: bool,
    last_queued_updates_count: usize,
    reload_in_flight: bool,
    saved_reload_full: Option<(GroupCallObject, Vec<GroupCallParticipantData>)>,
    stream_channels_in_flight: bool,
    empty_rtmp: bool,
    last_rtmp_timestamp_ms: i64,

    participants: ParticipantsList,
    actions: Vec<GroupCallAction>,
}

impl GroupCallSession {
    /// Create a session for joining an existing group call (`id != 0`) or creating a new one
    /// (`id == 0`).
    pub fn new(
        peer_id: PeerId,
        join_as: PeerId,
        call_id: CallId,
        access_hash: i64,
        can_manage: bool,
        schedule_date: UnixTime,
        rtmp: bool,
    ) -> Self {
        let mut session = Self {
            peer_id,
            join_as,
            can_manage,
            id: call_id,
            access_hash,
            stream_dc_id: None,
            join_hash: None,
            conference_slug: None,
            conference: false,
            e2e: None,
            e2e_subchains: [
                SubchainRequestTracker::default(),
                SubchainRequestTracker::default(),
            ],
            messages: GroupCallMessages::new(peer_id, join_as, join_as, false, false, can_manage),
            state: GroupCallState::Creating,
            instance_state: InstanceState::Disconnected,
            instance_transitioning: false,
            instance_mode: InstanceMode::None,
            had_joined_state: false,
            rtmp,
            rtmp_info: RtmpInfo::default(),
            video_stream: false,
            single_source_volume: DEFAULT_VOLUME,
            schedule_date,
            muted: MuteState::Muted,
            initial_mute_state_sent: false,
            self_update_in_flight: false,
            pending_self_updates: Vec::new(),
            queued_self_updates: Vec::new(),
            join_state: JoinState::default(),
            screen_join_state: JoinState::default(),
            my_ssrcs: HashSet::new(),
            camera_state: VideoState::Inactive,
            screen_state: VideoState::Inactive,
            camera_endpoint: String::new(),
            screen_endpoint: String::new(),
            video_is_working: false,
            active_video_tracks: BTreeMap::new(),
            shown_video_tracks: BTreeSet::new(),
            video_endpoint_large: VideoEndpoint::default(),
            video_endpoint_pinned: false,
            video_large_till_ms: 0,
            last_spoke: HashMap::new(),
            last_spoke_timer_active: false,
            last_send_progress_ms: 0,
            reload_by_queued_timer_active: false,
            last_queued_updates_count: 0,
            reload_in_flight: false,
            saved_reload_full: None,
            stream_channels_in_flight: false,
            empty_rtmp: false,
            last_rtmp_timestamp_ms: 0,
            participants: ParticipantsList::new(call_id),
            actions: Vec::new(),
        };
        if !call_id.is_zero() {
            session.initial_join();
        }
        session
    }

    /// Drain all emitted [`GroupCallAction`]s.
    pub fn take_actions(&mut self) -> Vec<GroupCallAction> {
        std::mem::take(&mut self.actions)
    }

    /// Current lifecycle state.
    pub const fn state(&self) -> GroupCallState {
        self.state
    }

    /// Current underlying media transport state.
    pub const fn instance_state(&self) -> InstanceState {
        self.instance_state
    }

    /// Current media transport mode (`None`, `Rtc`, `Stream`).
    pub const fn instance_mode(&self) -> InstanceMode {
        self.instance_mode
    }

    /// Current local mute / raise-hand state.
    pub const fn muted(&self) -> MuteState {
        self.muted
    }

    /// Returns `true` when the local user is prevented from unmuting by an admin or RTMP mode.
    ///
    /// Port of `GroupCall::mutedByAdmin` (`calls_group_call.cpp:902-907`).
    pub const fn muted_by_admin(&self) -> bool {
        self.rtmp || matches!(self.muted, MuteState::ForceMuted | MuteState::RaisedHand)
    }

    /// Current group call ID.
    pub const fn id(&self) -> CallId {
        self.id
    }

    /// Access hash of the group call (`inputGroupCall.access_hash`).
    pub const fn access_hash(&self) -> i64 {
        self.access_hash
    }

    /// Broadcast/livestream DC ID reported by `GroupCall.stream_dc_id`, if any.
    pub const fn stream_dc_id(&self) -> Option<i32> {
        self.stream_dc_id
    }

    /// Current local audio SSRC (0 if not joined).
    pub const fn my_ssrc(&self) -> u32 {
        self.join_state.ssrc
    }

    /// Current peer ID we joined as.
    pub const fn join_as(&self) -> PeerId {
        self.join_as
    }

    /// Scheduled start Unix timestamp (0 if active / not scheduled).
    pub const fn schedule_date(&self) -> UnixTime {
        self.schedule_date
    }

    /// Whether the local user is subscribed to the scheduled start notification.
    pub const fn schedule_start_subscribed(&self) -> bool {
        self.participants.schedule_start_subscribed()
    }

    /// Access the synchronized [`ParticipantsList`].
    pub fn participants_list(&self) -> &ParticipantsList {
        &self.participants
    }

    /// Active video tracks keyed by [`VideoEndpoint`].
    pub fn active_video_tracks(&self) -> &BTreeMap<VideoEndpoint, ActiveVideoTrack> {
        &self.active_video_tracks
    }

    /// Currently shown video tracks (with at least one rendered frame or paused).
    pub fn shown_video_tracks(&self) -> &BTreeSet<VideoEndpoint> {
        &self.shown_video_tracks
    }

    /// Currently enlarged/spotlight video endpoint (empty if none).
    pub fn video_endpoint_large(&self) -> &VideoEndpoint {
        &self.video_endpoint_large
    }

    /// Whether the enlarged video endpoint is pinned by the user.
    pub const fn video_endpoint_pinned(&self) -> bool {
        self.video_endpoint_pinned
    }

    /// Current RTMP ingest info.
    pub fn rtmp_info(&self) -> &RtmpInfo {
        &self.rtmp_info
    }

    /// Current camera endpoint assigned by the SFU.
    pub fn camera_endpoint(&self) -> &str {
        &self.camera_endpoint
    }

    /// Current screen-share endpoint assigned by the SFU.
    pub fn screen_endpoint(&self) -> &str {
        &self.screen_endpoint
    }

    /// Set an invite hash to include in `phone.joinGroupCall` (or rejoin immediately if muted by admin).
    ///
    /// Port of `GroupCall::rejoinWithHash` (`calls_group_call.cpp:1570-1575`).
    pub fn rejoin_with_hash(&mut self, hash: String) {
        if !hash.is_empty() && self.muted_by_admin() {
            self.join_hash = Some(hash);
            self.rejoin();
        }
    }

    /// Issue `phone.createGroupCall` for a newly created group call.
    pub fn start_create(&mut self, random_id: i32) {
        self.actions.push(GroupCallAction::CreateGroupCall {
            peer: self.peer_id,
            random_id,
            schedule_date: (self.schedule_date != 0).then_some(self.schedule_date),
            rtmp_stream: self.rtmp,
        });
    }

    /// Transition into `Waiting` (scheduled) or `Joining` (active) and request join payload.
    ///
    /// Port of `GroupCall::initialJoin` (`calls_group_call.cpp:1337-1346`).
    pub fn initial_join(&mut self) {
        self.set_state(if self.schedule_date != 0 {
            GroupCallState::Waiting
        } else {
            GroupCallState::Joining
        });
        if self.schedule_date != 0 {
            return;
        }
        self.rejoin();
    }

    /// Initiate a join/rejoin cycle by requesting a fresh WebRTC join payload from the media engine.
    ///
    /// Port of `GroupCall::rejoin` (`calls_group_call.cpp:1594-1642`).
    pub fn rejoin(&mut self) {
        if !matches!(
            self.state,
            GroupCallState::Joining | GroupCallState::Joined | GroupCallState::Connecting
        ) {
            return;
        }
        if self.join_state.action != JoinAction::None {
            return;
        }
        self.join_state.action = JoinAction::Joining;
        self.join_state.ssrc = 0;
        self.initial_mute_state_sent = false;
        self.set_state(GroupCallState::Joining);
        self.instance_mode = InstanceMode::None;
        self.actions.push(GroupCallAction::SetInstanceMode {
            mode: InstanceMode::None,
            rtmp: self.rtmp,
        });
        self.actions.push(GroupCallAction::EmitJoinPayload);
    }

    /// Deliver the local WebRTC join payload produced by the media engine (`emitJoinPayload`
    /// callback), emitting `phone.joinGroupCall`.
    ///
    /// Port of `GroupCall::sendJoinRequest` (`calls_group_call.cpp:1644-1686`).
    pub fn on_join_payload_ready(&mut self, audio_ssrc: u32, json: String) {
        self.join_state.payload_ssrc = audio_ssrc;
        self.join_state.payload_json = json.clone();
        if self.state != GroupCallState::Joining {
            self.join_state.finish(0);
            self.check_next_join_action();
            return;
        }
        let muted = self.muted != MuteState::Active;
        let video_stopped = self.camera_state == VideoState::Inactive;
        self.actions.push(GroupCallAction::JoinGroupCall {
            call: self.input_call_safe(),
            join_as: self.join_as,
            muted,
            video_stopped,
            invite_hash: self.join_hash.clone(),
            params_json: json,
        });
    }

    /// Handle successful `phone.joinGroupCall` response.
    ///
    /// Port of `GroupCall::joinDone` (`calls_group_call.cpp:1780-1861`).
    pub fn handle_join_done(&mut self, was_mute_state: MuteState, was_video_stopped: bool) {
        let ssrc = self.join_state.payload_ssrc;
        self.join_state.finish(ssrc);
        self.my_ssrcs.insert(ssrc);

        let next_state = if self.instance_state == InstanceState::Disconnected {
            GroupCallState::Connecting
        } else {
            GroupCallState::Joined
        };
        self.set_state(next_state);
        self.maybe_send_muted_update(was_mute_state);
        self.apply_queued_self_updates();
        self.check_first_time_joined();

        self.screen_join_state.next_action_pending = true;
        self.check_next_join_action();

        let is_sharing_camera = self.camera_state != VideoState::Inactive;
        if was_video_stopped == is_sharing_camera {
            self.send_self_update(SelfUpdateKind::CameraStopped);
        }
        if self.camera_state == VideoState::Paused {
            self.send_self_update(SelfUpdateKind::CameraPaused);
        }
        self.send_pending_self_updates();
    }

    /// Handle failed `phone.joinGroupCall` response.
    ///
    /// Retries automatically on `GROUPCALL_SSRC_DUPLICATE_MUCH` (`calls_group_call.cpp:1877-1880`).
    pub fn handle_join_fail(&mut self, error: &str) {
        self.join_state.finish(0);
        if matches!(
            self.state,
            GroupCallState::HangingUp | GroupCallState::FailedHangingUp
        ) {
            self.join_state.next_action_pending = false;
            let final_state = if self.state == GroupCallState::HangingUp {
                GroupCallState::Ended
            } else {
                GroupCallState::Failed
            };
            self.set_state(final_state);
            return;
        }
        if !self.id.is_zero() && error == "GROUPCALL_SSRC_DUPLICATE_MUCH" {
            self.rejoin();
            return;
        }
        self.hangup();
    }

    /// Handle `updateGroupCallConnection` carrying SFU RTC or broadcast/RTMP parameters.
    ///
    /// Port of `GroupCall::handlePossibleCreateOrJoinResponse(MTPDupdateGroupCallConnection)`
    /// (`calls_group_call.cpp:2393-2442`).
    pub fn handle_connection_update(&mut self, update: &UpdateGroupCallConnection) {
        let parsed = match parse_join_response(&update.params_json) {
            Ok(p) => p,
            Err(_) => return,
        };

        if update.presentation {
            if let ParsedJoinResponse::Rtc { video_endpoint, .. } = parsed {
                self.set_screen_endpoint(video_endpoint);
            }
            self.actions
                .push(GroupCallAction::SetScreenJoinResponsePayload(
                    update.params_json.clone(),
                ));
            return;
        }

        match parsed {
            ParsedJoinResponse::BroadcastStream { rtmp, rtmp_info } => {
                if rtmp {
                    self.rtmp = true;
                    self.rtmp_info = rtmp_info;
                }
                self.instance_mode = InstanceMode::Stream;
                self.actions.push(GroupCallAction::SetInstanceMode {
                    mode: InstanceMode::Stream,
                    rtmp: self.rtmp,
                });
            }
            ParsedJoinResponse::Rtc { video_endpoint, .. } => {
                self.instance_mode = InstanceMode::Rtc;
                self.actions.push(GroupCallAction::SetInstanceMode {
                    mode: InstanceMode::Rtc,
                    rtmp: self.rtmp,
                });
                self.set_camera_endpoint(video_endpoint);
                self.actions.push(GroupCallAction::SetJoinResponsePayload(
                    update.params_json.clone(),
                ));
            }
        }
        self.update_instance_volumes();
        self.fill_active_video_endpoints();
        self.update_requested_video_channels();
    }

    /// Process a versioned group call update (`updateGroupCall` / `updateGroupCallParticipants`).
    pub fn handle_versioned_update(&mut self, update: VersionedGroupCallUpdate) {
        let events = self.participants.enqueue_update(update);
        self.process_participant_list_events(events);
    }

    /// Process a full `phone.getGroupCall` snapshot.
    pub fn process_full_call(
        &mut self,
        call: GroupCallObject,
        participants: &[GroupCallParticipantData],
    ) {
        if let GroupCallObject::Active(ref info) = call {
            if self.id.is_zero() {
                self.id = info.id;
                self.access_hash = info.access_hash;
            }
            if info.stream_dc_id.is_some() {
                self.stream_dc_id = info.stream_dc_id;
            }
            if !self.can_manage && info.join_muted && self.muted == MuteState::Muted {
                self.set_muted(MuteState::ForceMuted);
            }
        }
        let events = self.participants.process_full_call(call, participants);
        self.process_participant_list_events(events);
        self.fill_active_video_endpoints();
    }

    /// Populate call ID/access hash after `phone.createGroupCall` returns `updateGroupCall`
    /// and kick off `initial_join()`.
    pub fn apply_created_call(&mut self, info: &GroupCallInfo) {
        if !self.id.is_zero() {
            return;
        }
        self.id = info.id;
        self.access_hash = info.access_hash;
        if info.stream_dc_id.is_some() {
            self.stream_dc_id = info.stream_dc_id;
        }
        self.schedule_date = info.schedule_date.unwrap_or(0);
        self.participants = ParticipantsList::new(info.id);
        let events = self
            .participants
            .enqueue_update(VersionedGroupCallUpdate::Call(GroupCallObject::Active(
                info.clone(),
            )));
        self.process_participant_list_events(events);
        if self.state == GroupCallState::Creating {
            self.initial_join();
        }
    }

    /// Apply a paginated participant slice loaded via `phone.getGroupParticipants`.
    pub fn apply_participants_slice(
        &mut self,
        participants: &[GroupCallParticipantData],
        source: ApplySliceSource,
    ) {
        let mut events = crate::group::participant::ParticipantsListEvents::default();
        self.participants
            .apply_participants_slice(participants, source, &mut events);
        self.process_participant_list_events(events);
        self.fill_active_video_endpoints();
    }

    /// Apply participants resolved via `phone.getGroupParticipants(sources)` for unknown SSRCs.
    pub fn handle_resolved_unknown_participants(
        &mut self,
        participants: &[GroupCallParticipantData],
        resolved_ssrcs: &[u32],
        now_ms: u64,
    ) {
        let events = self.participants.apply_resolved_unknown_participants(
            participants,
            resolved_ssrcs,
            now_ms,
        );
        self.process_speaking_events(events, now_ms);
        self.fill_active_video_endpoints();
    }

    /// Process media transport connectivity updates (`setInstanceConnected`,
    /// `calls_group_call.cpp:3778-3808`).
    pub fn set_instance_connected(&mut self, is_connected: bool, transitioning_to_rtc: bool) {
        let next_state = if !is_connected {
            InstanceState::Disconnected
        } else if transitioning_to_rtc {
            InstanceState::TransitionToRtc
        } else {
            InstanceState::Connected
        };
        if self.instance_state == next_state && self.instance_transitioning == transitioning_to_rtc
        {
            return;
        }
        let connected = next_state != InstanceState::Disconnected;
        let now_can_speak = connected
            && self.instance_transitioning
            && !transitioning_to_rtc
            && self.muted == MuteState::Muted;

        self.instance_transitioning = transitioning_to_rtc;
        self.instance_state = next_state;

        if self.state == GroupCallState::Connecting && connected {
            self.set_state(GroupCallState::Joined);
        } else if self.state == GroupCallState::Joined && !connected {
            self.set_state(GroupCallState::Connecting);
        }
        if now_can_speak {
            self.notify_about_allowed_to_speak();
        }
        if !self.had_joined_state && self.state == GroupCallState::Joined {
            self.check_first_time_joined();
        }
    }

    /// Change local mute state without immediately sending a server update (except when
    /// transitioning into `Active` or first mute sync).
    ///
    /// Port of `GroupCall::setMuted` (`calls_group_call.cpp:2297-2333`).
    pub fn set_muted(&mut self, mute: MuteState) {
        let previous = self.muted;
        if previous == mute {
            return;
        }
        let was_speaking = matches!(previous, MuteState::Active | MuteState::PushToTalk);
        self.muted = mute;
        self.actions.push(GroupCallAction::MuteStateChanged {
            from: previous,
            to: mute,
        });

        let instance_muted = !matches!(mute, MuteState::Active | MuteState::PushToTalk);
        self.actions
            .push(GroupCallAction::SetInstanceMuted(instance_muted));

        if self.muted_by_admin() {
            self.toggle_camera(false);
            self.toggle_screen_sharing(false);
        }

        let now_speaking = matches!(mute, MuteState::Active | MuteState::PushToTalk);
        if was_speaking && !now_speaking && self.join_state.ssrc != 0 {
            self.actions.push(GroupCallAction::LevelUpdate(LevelUpdate {
                ssrc: self.join_state.ssrc,
                value: 0.0,
                voice: false,
                me: true,
            }));
        }

        if self.join_state.ssrc != 0 && (!self.initial_mute_state_sent || mute == MuteState::Active)
        {
            self.initial_mute_state_sent = true;
            self.maybe_send_muted_update(previous);
        }
    }

    /// Change local mute or raise-hand state and synchronize with the server.
    ///
    /// Port of `GroupCall::setMutedAndUpdate` (`calls_group_call.cpp:2335-2345`).
    pub fn set_muted_and_update(&mut self, mute: MuteState) {
        let was = self.muted;
        let send = self.initial_mute_state_sent && mute != MuteState::Active;
        self.set_muted(mute);
        if send {
            self.maybe_send_muted_update(was);
        }
    }

    /// Handle Push-to-Talk key press / release (`calls_group_call.cpp:4200-4218`).
    pub fn push_to_talk(&mut self, pressed: bool, delay_ms: u64) {
        if self.muted_by_admin() || self.muted == MuteState::Active {
            return;
        }
        if pressed {
            self.actions.push(GroupCallAction::CancelTimer(
                GroupCallTimerKind::PushToTalkCancel,
            ));
            self.set_muted(MuteState::PushToTalk);
        } else if delay_ms > 0 {
            self.actions.push(GroupCallAction::SetTimer {
                kind: GroupCallTimerKind::PushToTalkCancel,
                duration_ms: delay_ms,
                repeating: false,
            });
        } else {
            self.push_to_talk_cancel();
        }
    }

    /// End Push-to-Talk and revert to `Muted`.
    pub fn push_to_talk_cancel(&mut self) {
        self.actions.push(GroupCallAction::CancelTimer(
            GroupCallTimerKind::PushToTalkCancel,
        ));
        if self.muted == MuteState::PushToTalk {
            self.set_muted(MuteState::Muted);
        }
    }

    /// Mute or unmute another participant (admin mute or local `muted_by_you`).
    ///
    /// Port of `GroupCall::toggleMute` (`calls_group_call.cpp:3992-4001`).
    pub fn toggle_participant_mute(
        &mut self,
        participant_peer: PeerId,
        mute: bool,
        locally_only: bool,
    ) {
        if self.rtmp || self.video_stream {
            self.single_source_volume = if mute { 0 } else { DEFAULT_VOLUME };
            self.update_instance_volumes();
            return;
        }
        self.apply_participant_locally(participant_peer, mute, None);
        if !locally_only {
            self.emit_edit_participant(participant_peer, mute, None);
        }
    }

    /// Adjust another participant's playback volume (clamped to `1..=20000`).
    ///
    /// Port of `GroupCall::changeVolume` (`calls_group_call.cpp:4003-4012`).
    pub fn change_participant_volume(
        &mut self,
        participant_peer: PeerId,
        volume: i32,
        locally_only: bool,
    ) {
        if self.rtmp || self.video_stream {
            self.single_source_volume = volume.clamp(0, MAX_VOLUME);
            self.update_instance_volumes();
            return;
        }
        let clamped = volume.clamp(MIN_VOLUME, MAX_VOLUME);
        self.apply_participant_locally(participant_peer, false, Some(clamped));
        if !locally_only {
            self.emit_edit_participant(participant_peer, false, Some(clamped));
        }
    }

    /// Toggle outgoing camera video state.
    pub fn toggle_camera(&mut self, active: bool) {
        let next = if active {
            VideoState::Active
        } else {
            VideoState::Inactive
        };
        if self.camera_state == next {
            return;
        }
        self.camera_state = next;
        self.send_self_update(SelfUpdateKind::CameraStopped);
        self.fill_active_video_endpoints();
    }

    /// Toggle outgoing screen-share state (starts or leaves presentation connection).
    pub fn toggle_screen_sharing(&mut self, active: bool) {
        let next = if active {
            VideoState::Active
        } else {
            VideoState::Inactive
        };
        if self.screen_state == next {
            return;
        }
        self.screen_state = next;
        self.screen_join_state.next_action_pending = true;
        self.check_next_join_action();
    }

    /// Deliver local screen-share join payload to send `phone.joinGroupCallPresentation`.
    pub fn on_screen_join_payload_ready(&mut self, audio_ssrc: u32, json: String) {
        if self.screen_state == VideoState::Inactive || self.join_state.ssrc == 0 {
            self.screen_join_state.finish(0);
            self.check_next_join_action();
            return;
        }
        self.screen_join_state.payload_ssrc = audio_ssrc;
        self.screen_join_state.payload_json = json.clone();
        self.actions
            .push(GroupCallAction::JoinGroupCallPresentation {
                call: self.input_call(),
                params_json: json,
            });
    }

    /// Mark `phone.joinGroupCallPresentation` completed.
    pub fn handle_screen_join_done(&mut self) {
        let ssrc = self.screen_join_state.payload_ssrc;
        self.screen_join_state.finish(ssrc);
        self.my_ssrcs.insert(ssrc);
        self.check_next_join_action();
        if self.screen_state == VideoState::Paused {
            self.send_self_update(SelfUpdateKind::ScreenPaused);
        }
        self.send_pending_self_updates();
    }

    /// Mark `phone.leaveGroupCallPresentation` completed.
    pub fn handle_screen_leave_done(&mut self) {
        self.screen_join_state.finish(0);
        self.set_screen_endpoint(String::new());
        self.check_next_join_action();
    }

    /// Mark an in-flight self `phone.editGroupCallParticipant` request completed and flush
    /// the next queued self-update on success.
    ///
    /// Port of `GroupCall::sendSelfUpdate` `.done()` / `.fail()` (`calls_group_call.cpp:3940-3951`).
    pub fn handle_self_update_done(&mut self, error: Option<&str>) {
        self.self_update_in_flight = false;
        // Match `GroupCall::sendSelfUpdate` (`calls/group/calls_group_call.cpp:3940-3951`):
        // only flush `sendPendingSelfUpdates()` on `.done()`; on `.fail()`, rejoin if
        // `GROUPCALL_FORBIDDEN` and do not immediately re-fire pending updates on errors
        // such as `FLOOD_WAIT`.
        match error {
            None => self.send_pending_self_updates(),
            Some("GROUPCALL_FORBIDDEN") => self.rejoin(),
            Some(_) => {}
        }
    }

    /// Process audio level samples from the media engine and update speaking indicators.
    ///
    /// Exact port of `GroupCall::audioLevelsUpdated` (`calls_group_call.cpp:3635-3699`).
    pub fn audio_levels_updated(&mut self, samples: &[AudioLevelSample], now_ms: u64) {
        if samples.is_empty() {
            return;
        }
        let mut check = false;
        let mut check_now = false;
        let me_muted = !matches!(self.muted, MuteState::Active | MuteState::PushToTalk);

        for sample in samples {
            let ssrc = if sample.ssrc != 0 {
                sample.ssrc
            } else {
                self.join_state.ssrc
            };
            if ssrc == 0 {
                continue;
            }
            let me = ssrc == self.join_state.ssrc;
            let ignore = me && me_muted;

            self.actions.push(GroupCallAction::LevelUpdate(LevelUpdate {
                ssrc,
                value: if ignore { 0.0 } else { sample.level },
                voice: !ignore && sample.voice,
                me,
            }));

            if sample.level <= SPEAK_LEVEL_THRESHOLD {
                continue;
            }
            if me
                && sample.voice
                && (self.last_send_progress_ms == 0
                    || self
                        .last_send_progress_ms
                        .saturating_add(UPDATE_SEND_ACTION_EACH_MS)
                        < now_ms)
            {
                self.last_send_progress_ms = now_ms;
                self.actions
                    .push(GroupCallAction::SendSpeakingAction { peer: self.peer_id });
            }

            check = true;
            if let Some(entry) = self.last_spoke.get_mut(&ssrc) {
                let threshold = CHECK_LAST_SPOKE_INTERVAL_MS / 3;
                if entry.anything_ms.saturating_add(threshold) <= now_ms
                    || (sample.voice && entry.voice_ms.saturating_add(threshold) <= now_ms)
                {
                    check_now = true;
                }
                entry.anything_ms = now_ms;
                if sample.voice {
                    entry.voice_ms = now_ms;
                }
            } else {
                self.last_spoke.insert(
                    ssrc,
                    LastSpokeTimes {
                        anything_ms: now_ms,
                        voice_ms: if sample.voice { now_ms } else { 0 },
                    },
                );
                check_now = true;
            }
        }

        if check_now {
            self.check_last_spoke(now_ms);
        } else if check && !self.last_spoke_timer_active {
            self.last_spoke_timer_active = true;
            self.actions.push(GroupCallAction::SetTimer {
                kind: GroupCallTimerKind::CheckLastSpoke,
                duration_ms: CHECK_LAST_SPOKE_INTERVAL_MS / 2,
                repeating: true,
            });
        }
    }

    /// Sweep `_lastSpoke`, expiring stale entries after 2 000 ms and updating participant
    /// sounding/speaking flags (1 500 ms window).
    ///
    /// Exact port of `GroupCall::checkLastSpoke` (`calls_group_call.cpp:3701-3739`).
    pub fn check_last_spoke(&mut self, now_ms: u64) {
        let mut has_recent = false;
        let list = std::mem::take(&mut self.last_spoke);
        let mut kept = HashMap::new();
        let my_ssrc = self.join_state.ssrc;
        let my_unmuted = matches!(self.muted, MuteState::Active | MuteState::PushToTalk);

        for (ssrc, when) in list {
            if when.anything_ms.saturating_add(KEEP_LAST_SPOKE_FOR_MS) >= now_ms {
                has_recent = true;
                kept.insert(ssrc, when);
            }
            let effective_when = if ssrc != my_ssrc || my_unmuted {
                when
            } else {
                LastSpokeTimes::default()
            };
            let events = self
                .participants
                .apply_last_spoke(ssrc, effective_when, now_ms);
            self.process_speaking_events(events, now_ms);
        }
        self.last_spoke = kept;

        if !has_recent {
            if self.last_spoke_timer_active {
                self.last_spoke_timer_active = false;
                self.actions.push(GroupCallAction::CancelTimer(
                    GroupCallTimerKind::CheckLastSpoke,
                ));
            }
        } else if !self.last_spoke_timer_active {
            self.last_spoke_timer_active = true;
            self.actions.push(GroupCallAction::SetTimer {
                kind: GroupCallTimerKind::CheckLastSpoke,
                duration_ms: CHECK_LAST_SPOKE_INTERVAL_MS / 3,
                repeating: true,
            });
        }
    }

    /// Handle periodic `checkJoined` timer while in `Connecting` state.
    ///
    /// Port of `GroupCall::checkJoined` (`calls_group_call.cpp:3741-3776`).
    pub fn check_joined(&mut self) {
        if self.state != GroupCallState::Connecting
            || self.id.is_zero()
            || self.join_state.ssrc == 0
        {
            return;
        }
        let mut sources = vec![self.join_state.ssrc];
        if self.screen_join_state.ssrc != 0 {
            sources.push(self.screen_join_state.ssrc);
        }
        self.actions.push(GroupCallAction::CheckGroupCall {
            call: self.input_call(),
            sources,
        });
    }

    /// Process the `Vector<int>` response of `phone.checkGroupCall`.
    pub fn handle_check_group_call_result(&mut self, result: Result<&[u32], &str>) {
        match result {
            Ok(active_ssrcs) => {
                if !active_ssrcs.contains(&self.join_state.ssrc) {
                    self.join_state.next_action_pending = true;
                    self.check_next_join_action();
                } else {
                    if self.state == GroupCallState::Connecting {
                        self.actions.push(GroupCallAction::SetTimer {
                            kind: GroupCallTimerKind::CheckJoined,
                            duration_ms: CHECK_JOINED_TIMEOUT_MS,
                            repeating: false,
                        });
                    }
                    if self.screen_join_state.ssrc != 0
                        && !active_ssrcs.contains(&self.screen_join_state.ssrc)
                    {
                        self.screen_join_state.next_action_pending = true;
                        self.check_next_join_action();
                    }
                }
            }
            Err(_) => {
                self.rejoin();
            }
        }
    }

    /// Dispatch an expired timer.
    pub fn on_timer(&mut self, kind: GroupCallTimerKind, now_ms: u64) {
        match kind {
            GroupCallTimerKind::CheckLastSpoke => self.check_last_spoke(now_ms),
            GroupCallTimerKind::CheckJoined => self.check_joined(),
            GroupCallTimerKind::PushToTalkCancel => self.push_to_talk_cancel(),
            GroupCallTimerKind::ReloadByQueuedUpdates => self.reload_participants_on_gap(),
        }
    }

    /// Flush out-of-order queued participant updates and reload `phone.getGroupCall(limit=3)`
    /// after `WAIT_FOR_UPDATES_TIMEOUT_MS` (3 s) expires.
    ///
    /// Port of `Data::GroupCall::reload` (`data/data_group_call.cpp:651-686`).
    pub fn reload_participants_on_gap(&mut self) {
        if self.reload_in_flight || self.id.is_zero() {
            return;
        }
        let flushed = self.participants.flush_queued_updates_for_reload();
        if self.reload_by_queued_timer_active {
            self.reload_by_queued_timer_active = false;
            self.actions.push(GroupCallAction::CancelTimer(
                GroupCallTimerKind::ReloadByQueuedUpdates,
            ));
        }
        self.last_queued_updates_count = 0;
        self.process_participant_list_events(flushed);

        self.reload_in_flight = true;
        self.actions.push(GroupCallAction::ReloadGroupCall {
            call: self.input_call(),
            limit: 3,
        });
    }

    /// Process the `phone.getGroupCall` response triggered by a version-gap reload.
    ///
    /// If the returned slice is smaller than both `call.participants_count` and the locally
    /// loaded participant count, buffers the call metadata and emits
    /// `GroupCallAction::GetGroupParticipants` (`limit = 50`) before applying the full snapshot,
    /// matching `Data::GroupCall::reload` and `requestParticipantsAfterReload`
    /// (`data/data_group_call.cpp:672-699`).
    pub fn handle_reload_group_call_result(
        &mut self,
        result: Result<(GroupCallObject, &[GroupCallParticipantData], &str), &str>,
    ) {
        match result {
            Ok((call, participants, next_offset)) => {
                if self
                    .participants
                    .needs_participants_after_reload(&call, participants.len())
                {
                    self.saved_reload_full = Some((call, participants.to_vec()));
                    self.actions.push(GroupCallAction::GetGroupParticipants {
                        call: self.input_call(),
                        offset: next_offset.to_string(),
                        limit: REQUEST_PER_PAGE,
                    });
                    return;
                }
                self.reload_in_flight = false;
                self.process_full_call(call, participants);
            }
            Err(_) => {
                self.reload_in_flight = false;
            }
        }
    }

    /// Process the follow-up `phone.getGroupParticipants` response after a version-gap reload.
    ///
    /// Port of `Data::GroupCall::requestParticipants` `.done()` / `.fail()`
    /// (`data/data_group_call.cpp:195-225`).
    pub fn handle_reload_participants_result(
        &mut self,
        result: Result<(&[GroupCallParticipantData], i32), &str>,
    ) {
        self.reload_in_flight = false;
        let saved = self.saved_reload_full.take();
        let reloaded = saved.is_some();
        if let Some((call, initial_participants)) = saved {
            self.process_full_call(call, &initial_participants);
        }
        match result {
            Ok((participants, total_count)) => {
                let source = if reloaded {
                    ApplySliceSource::FullReloaded
                } else {
                    ApplySliceSource::SliceLoaded
                };
                self.apply_participants_slice(participants, source);
                self.participants.set_server_participants_count(total_count);
                if participants.is_empty() {
                    self.participants.set_all_participants_loaded();
                }
            }
            Err(_) => {
                let loaded = self.participants.participants().len() as i32;
                self.participants.set_server_participants_count(loaded);
                self.participants.set_all_participants_loaded();
            }
        }
    }

    /// Request current RTMP broadcast stream channels (`phone.getGroupCallStreamChannels`).
    ///
    /// Port of `GroupCall::requestCurrentTimeStart` (`calls/group/calls_group_call.cpp:3329-3371`).
    pub fn request_stream_channels(&mut self) {
        if !self.rtmp || self.id.is_zero() || self.stream_channels_in_flight {
            return;
        }
        self.stream_channels_in_flight = true;
        self.actions
            .push(GroupCallAction::GetGroupCallStreamChannels {
                call: self.input_call(),
                stream_dc_id: self.stream_dc_id,
            });
    }

    /// Process a `phone.getGroupCallStreamChannels` response and return the resolved current
    /// stream timestamp in milliseconds (`0` when empty or on error).
    ///
    /// Rejoins automatically on `GROUPCALL_JOIN_MISSING` or `GROUPCALL_FORBIDDEN`, matching
    /// `GroupCall::requestCurrentTimeStart` (`calls/group/calls_group_call.cpp:3347-3368`).
    pub fn handle_stream_channels_result(
        &mut self,
        result: Result<&[GroupCallStreamChannel], &str>,
    ) -> i64 {
        self.stream_channels_in_flight = false;
        match result {
            Ok(channels) => {
                self.empty_rtmp = channels.is_empty();
                let ts = channels.first().map_or(0, |c| c.last_timestamp_ms);
                self.last_rtmp_timestamp_ms = ts;
                ts
            }
            Err(error) => {
                if error == "GROUPCALL_JOIN_MISSING" || error == "GROUPCALL_FORBIDDEN" {
                    self.rejoin();
                }
                0
            }
        }
    }

    /// Handle an error returned from `upload.getFile` for an `inputGroupCallStream` chunk.
    ///
    /// Rejoins on `GROUPCALL_JOIN_MISSING` or `GROUPCALL_FORBIDDEN`, matching
    /// `GroupCall::broadcastPartStart` (`calls/group/calls_group_call.cpp:3244-3247`).
    pub fn handle_broadcast_part_error(&mut self, error: &str) {
        if error == "GROUPCALL_JOIN_MISSING" || error == "GROUPCALL_FORBIDDEN" {
            self.rejoin();
        }
    }

    /// Whether the most recent `phone.getGroupCallStreamChannels` returned an empty channel list.
    #[must_use]
    pub const fn empty_rtmp(&self) -> bool {
        self.empty_rtmp
    }

    /// Latest `last_timestamp_ms` reported by `phone.getGroupCallStreamChannels`.
    #[must_use]
    pub const fn last_rtmp_timestamp_ms(&self) -> i64 {
        self.last_rtmp_timestamp_ms
    }

    /// Pin a video endpoint in large spotlight view (`calls_group_call.cpp:3954-3960`).
    pub fn pin_video_endpoint(&mut self, endpoint: VideoEndpoint) {
        self.video_endpoint_pinned = false;
        if !endpoint.is_empty() {
            self.set_video_endpoint_large(endpoint);
            self.video_endpoint_pinned = true;
        }
    }

    /// Select a video endpoint in large view for [`FIX_MANUAL_LARGE_VIDEO_MS`] (5 s) without pinning.
    ///
    /// Port of `GroupCall::showVideoEndpointLarge` (`calls_group_call.cpp:3962-3969`).
    pub fn show_video_endpoint_large(&mut self, endpoint: VideoEndpoint, now_ms: u64) {
        if self.video_endpoint_large == endpoint {
            return;
        }
        self.video_endpoint_pinned = false;
        self.set_video_endpoint_large(endpoint);
        self.video_large_till_ms = now_ms.saturating_add(FIX_MANUAL_LARGE_VIDEO_MS);
    }

    /// Mark an active video track as having rendered a frame (so it becomes eligible for
    /// speaker auto-switching and screen-share auto-pinning).
    pub fn mark_video_track_shown(&mut self, endpoint: &VideoEndpoint, shown: bool) {
        if let Some(track) = self.active_video_tracks.get_mut(endpoint) {
            track.shown = shown;
        }
        let changed = if shown {
            self.shown_video_tracks.insert(endpoint.clone())
        } else {
            self.shown_video_tracks.remove(endpoint)
        };
        if changed && shown && endpoint.kind == VideoEndpointType::Screen {
            self.pin_video_endpoint(endpoint.clone());
        }
    }

    /// Request a specific video quality tier for an active video endpoint.
    pub fn request_video_quality(&mut self, endpoint: &VideoEndpoint, quality: VideoQuality) {
        if endpoint.is_empty() {
            return;
        }
        if let Some(track) = self.active_video_tracks.get_mut(endpoint)
            && track.quality != quality
        {
            track.quality = quality;
            self.update_requested_video_channels();
        }
    }

    /// Leave the group call cleanly (`GroupCall::hangup`).
    pub fn hangup(&mut self) {
        self.finish(false);
    }

    /// Fail and leave the group call.
    pub fn fail(&mut self) {
        self.finish(true);
    }

    /// Discard (end for all participants) the group call (`GroupCall::discard`).
    pub fn discard(&mut self) {
        if self.id.is_zero() {
            self.hangup();
            return;
        }
        self.actions.push(GroupCallAction::DiscardGroupCall {
            call: self.input_call(),
        });
    }

    /// Mark `phone.discardGroupCall` completed (`calls_group_call.cpp:2184-2191`).
    pub fn handle_discard_done(&mut self) {
        self.hangup();
    }

    /// Mark `phone.leaveGroupCall` completed (`calls_group_call.cpp:2252-2259`).
    pub fn handle_leave_done(&mut self) {
        self.join_state.finish(0);
        let final_state = match self.state {
            GroupCallState::HangingUp => GroupCallState::Ended,
            GroupCallState::FailedHangingUp => GroupCallState::Failed,
            _ => return,
        };
        self.set_state(final_state);
    }

    /// Request changing the group call title (`phone.editGroupCallTitle`).
    pub fn change_title(&mut self, title: String) {
        if self.participants.title() == title {
            return;
        }
        self.actions.push(GroupCallAction::EditGroupCallTitle {
            call: self.input_call(),
            title,
        });
    }

    /// Request starting or stopping group call recording (`phone.toggleGroupCallRecord`).
    pub fn toggle_recording(
        &mut self,
        enabled: bool,
        title: Option<String>,
        video: bool,
        video_portrait: bool,
    ) {
        let already = self.participants.record_start_date() != 0;
        if already == enabled {
            return;
        }
        self.actions.push(GroupCallAction::ToggleGroupCallRecord {
            call: self.input_call(),
            start: enabled,
            title: title.filter(|t| !t.is_empty()),
            video,
            video_portrait,
        });
    }

    fn finish(&mut self, failed: bool) {
        let final_state = if failed {
            GroupCallState::Failed
        } else {
            GroupCallState::Ended
        };
        let hangup_state = if failed {
            GroupCallState::FailedHangingUp
        } else {
            GroupCallState::HangingUp
        };
        if matches!(
            self.state,
            GroupCallState::HangingUp
                | GroupCallState::FailedHangingUp
                | GroupCallState::Ended
                | GroupCallState::Failed
        ) {
            return;
        }
        if self.join_state.action == JoinAction::None && self.join_state.ssrc == 0 {
            self.set_state(final_state);
            return;
        }
        self.set_state(hangup_state);
        self.join_state.next_action_pending = true;
        self.check_next_join_action();
    }

    fn leave(&mut self) {
        if self.join_state.action != JoinAction::None {
            return;
        }
        self.join_state.action = JoinAction::Leaving;
        let ssrc = std::mem::take(&mut self.join_state.ssrc);
        self.actions.push(GroupCallAction::LeaveGroupCall {
            call: self.input_call(),
            source: ssrc,
        });
    }

    fn check_next_join_action(&mut self) {
        if self.join_state.action != JoinAction::None {
            return;
        }
        if self.join_state.next_action_pending {
            self.join_state.next_action_pending = false;
            if matches!(
                self.state,
                GroupCallState::HangingUp | GroupCallState::FailedHangingUp
            ) {
                self.leave();
            } else {
                self.rejoin();
            }
        } else if self.join_state.ssrc == 0 {
            self.rejoin();
        } else if self.screen_join_state.action != JoinAction::None
            || !self.screen_join_state.next_action_pending
        {
        } else {
            self.screen_join_state.next_action_pending = false;
            if self.screen_state != VideoState::Inactive {
                self.rejoin_presentation();
            } else {
                self.leave_presentation();
            }
        }
    }

    fn rejoin_presentation(&mut self) {
        if self.join_state.ssrc == 0
            || self.screen_join_state.action == JoinAction::Joining
            || self.screen_state == VideoState::Inactive
        {
            return;
        }
        if self.screen_join_state.action != JoinAction::None {
            self.screen_join_state.next_action_pending = true;
            return;
        }
        self.screen_join_state.action = JoinAction::Joining;
        self.screen_join_state.ssrc = 0;
        self.actions.push(GroupCallAction::EmitScreenJoinPayload);
    }

    fn leave_presentation(&mut self) {
        if self.screen_join_state.ssrc == 0 {
            self.set_screen_endpoint(String::new());
            return;
        }
        if self.screen_join_state.action == JoinAction::Leaving {
            return;
        }
        if self.screen_join_state.action != JoinAction::None {
            self.screen_join_state.next_action_pending = true;
            return;
        }
        self.screen_join_state.action = JoinAction::Leaving;
        self.actions
            .push(GroupCallAction::LeaveGroupCallPresentation {
                call: self.input_call(),
            });
    }

    fn set_state(&mut self, state: GroupCallState) {
        let current = self.state;
        if current == GroupCallState::Failed {
            return;
        }
        if current == GroupCallState::Ended && state != GroupCallState::Failed {
            return;
        }
        if current == GroupCallState::FailedHangingUp && state != GroupCallState::Failed {
            return;
        }
        if current == GroupCallState::HangingUp
            && state != GroupCallState::Ended
            && state != GroupCallState::Failed
        {
            return;
        }
        if current == state {
            return;
        }

        self.state = state;
        self.actions.push(GroupCallAction::StateChanged {
            from: current,
            to: state,
        });

        if current == GroupCallState::Connecting && state != GroupCallState::Connecting {
            self.actions.push(GroupCallAction::CancelTimer(
                GroupCallTimerKind::CheckJoined,
            ));
        }

        match state {
            GroupCallState::HangingUp | GroupCallState::FailedHangingUp => {
                self.actions
                    .push(GroupCallAction::PlaySound(GroupCallSound::Ended));
            }
            GroupCallState::Ended => {
                if self.last_spoke_timer_active {
                    self.last_spoke_timer_active = false;
                    self.actions.push(GroupCallAction::CancelTimer(
                        GroupCallTimerKind::CheckLastSpoke,
                    ));
                }
                self.actions.push(GroupCallAction::CancelTimer(
                    GroupCallTimerKind::PushToTalkCancel,
                ));
                if self.reload_by_queued_timer_active {
                    self.reload_by_queued_timer_active = false;
                    self.actions.push(GroupCallAction::CancelTimer(
                        GroupCallTimerKind::ReloadByQueuedUpdates,
                    ));
                }
                self.actions.push(GroupCallAction::NotifyFinished);
            }
            GroupCallState::Failed => {
                if self.last_spoke_timer_active {
                    self.last_spoke_timer_active = false;
                    self.actions.push(GroupCallAction::CancelTimer(
                        GroupCallTimerKind::CheckLastSpoke,
                    ));
                }
                self.actions.push(GroupCallAction::CancelTimer(
                    GroupCallTimerKind::PushToTalkCancel,
                ));
                if self.reload_by_queued_timer_active {
                    self.reload_by_queued_timer_active = false;
                    self.actions.push(GroupCallAction::CancelTimer(
                        GroupCallTimerKind::ReloadByQueuedUpdates,
                    ));
                }
                self.actions.push(GroupCallAction::NotifyFailed);
            }
            GroupCallState::Connecting => {
                self.actions.push(GroupCallAction::SetTimer {
                    kind: GroupCallTimerKind::CheckJoined,
                    duration_ms: CHECK_JOINED_TIMEOUT_MS,
                    repeating: false,
                });
            }
            _ => {}
        }
    }

    fn check_first_time_joined(&mut self) {
        if self.had_joined_state || self.state != GroupCallState::Joined {
            return;
        }
        self.had_joined_state = true;
        self.actions
            .push(GroupCallAction::PlaySound(GroupCallSound::Started));
    }

    fn notify_about_allowed_to_speak(&mut self) {
        if !self.had_joined_state {
            return;
        }
        self.actions
            .push(GroupCallAction::PlaySound(GroupCallSound::AllowedToSpeak));
        self.actions.push(GroupCallAction::AllowedToSpeak);
    }

    fn maybe_send_muted_update(&mut self, previous: MuteState) {
        let now = self.muted;
        if (previous == MuteState::Active && now == MuteState::Muted)
            || (now == MuteState::Active
                && matches!(previous, MuteState::Muted | MuteState::PushToTalk))
        {
            self.send_self_update(SelfUpdateKind::Mute);
        } else if (now == MuteState::ForceMuted && previous == MuteState::RaisedHand)
            || (now == MuteState::RaisedHand && previous == MuteState::ForceMuted)
        {
            self.send_self_update(SelfUpdateKind::RaiseHand);
        }
    }

    fn send_self_update(&mut self, kind: SelfUpdateKind) {
        if (!matches!(
            self.state,
            GroupCallState::Connecting | GroupCallState::Joined
        )) || self.self_update_in_flight
        {
            if !self.pending_self_updates.contains(&kind) {
                self.pending_self_updates.push(kind);
            }
            return;
        }
        self.self_update_in_flight = true;
        let is_muted = self.muted != MuteState::Active;
        let raise_hand = self.muted == MuteState::RaisedHand;
        let camera_stopped = self.camera_state == VideoState::Inactive;
        let camera_paused = self.camera_state == VideoState::Paused;
        let screen_paused = self.screen_state == VideoState::Paused;

        // Match `GroupCall::sendSelfUpdate` (`calls/group/calls_group_call.cpp:3915-3940`):
        // only the flag corresponding to `kind` is set; `Flag::f_volume` is NEVER set on
        // self-updates (C++ passes `MTP_int(100000)` only as an unserialized positional argument).
        self.actions
            .push(GroupCallAction::EditGroupCallParticipant {
                call: self.input_call(),
                participant: self.join_as,
                muted: (kind == SelfUpdateKind::Mute).then_some(is_muted),
                volume: None,
                raise_hand: (kind == SelfUpdateKind::RaiseHand).then_some(raise_hand),
                video_stopped: (kind == SelfUpdateKind::CameraStopped).then_some(camera_stopped),
                video_paused: (kind == SelfUpdateKind::CameraPaused).then_some(camera_paused),
                presentation_paused: (kind == SelfUpdateKind::ScreenPaused)
                    .then_some(screen_paused),
            });
    }

    fn send_pending_self_updates(&mut self) {
        if !matches!(
            self.state,
            GroupCallState::Connecting | GroupCallState::Joined
        ) || self.self_update_in_flight
        {
            return;
        }
        let priority = [
            SelfUpdateKind::Mute,
            SelfUpdateKind::RaiseHand,
            SelfUpdateKind::CameraStopped,
            SelfUpdateKind::CameraPaused,
            SelfUpdateKind::ScreenPaused,
        ];
        for kind in priority {
            if kind == SelfUpdateKind::ScreenPaused
                && self.screen_join_state.action != JoinAction::None
            {
                continue;
            }
            if let Some(pos) = self.pending_self_updates.iter().position(|k| *k == kind) {
                self.pending_self_updates.remove(pos);
                self.send_self_update(kind);
                return;
            }
        }
    }

    fn process_participant_list_events(
        &mut self,
        events: crate::group::participant::ParticipantsListEvents,
    ) {
        if !events.unknown_ssrcs_to_resolve.is_empty() && !self.id.is_zero() {
            self.actions.push(GroupCallAction::ResolveUnknownSsrcs {
                call: self.input_call(),
                ssrcs: events.unknown_ssrcs_to_resolve,
            });
        }
        for update in events.participant_updates {
            self.on_participant_updated(&update);
            self.actions
                .push(GroupCallAction::ParticipantUpdated(Box::new(update)));
        }
        for applied in events.applied_updates {
            match applied {
                VersionedGroupCallUpdate::Call(GroupCallObject::Discarded(d)) => {
                    self.handle_possible_discarded(d);
                }
                VersionedGroupCallUpdate::Call(GroupCallObject::Active(info)) => {
                    self.handle_group_call_info_update(&info);
                }
                VersionedGroupCallUpdate::Participants(p) => {
                    self.handle_participants_update_for_self(&p);
                }
            }
        }
        // Manage the 3 s version-gap reload timer, matching `Data::GroupCall::processQueuedUpdates`
        // (`data/data_group_call.cpp:628-634`).
        let queued_len = self.participants.queued_updates_len();
        if queued_len == 0 {
            if self.reload_by_queued_timer_active {
                self.reload_by_queued_timer_active = false;
                self.actions.push(GroupCallAction::CancelTimer(
                    GroupCallTimerKind::ReloadByQueuedUpdates,
                ));
            }
        } else if queued_len != self.last_queued_updates_count
            || !self.reload_by_queued_timer_active
        {
            self.reload_by_queued_timer_active = true;
            self.actions.push(GroupCallAction::SetTimer {
                kind: GroupCallTimerKind::ReloadByQueuedUpdates,
                duration_ms: WAIT_FOR_UPDATES_TIMEOUT_MS,
                repeating: false,
            });
        }
        self.last_queued_updates_count = queued_len;
    }

    fn process_speaking_events(
        &mut self,
        events: crate::group::participant::ParticipantsListEvents,
        now_ms: u64,
    ) {
        if !events.unknown_ssrcs_to_resolve.is_empty() && !self.id.is_zero() {
            self.actions.push(GroupCallAction::ResolveUnknownSsrcs {
                call: self.input_call(),
                ssrcs: events.unknown_ssrcs_to_resolve,
            });
        }
        for update in events.participant_updates {
            self.actions
                .push(GroupCallAction::ParticipantUpdated(Box::new(update)));
        }
        // Auto-switch large video endpoint to active speaker (`calls_group_call.cpp:1085-1119`).
        if !self.video_endpoint_large.is_empty() {
            for peer_id in events.speaking_peers {
                if self.video_endpoint_large.peer == Some(peer_id) {
                    self.video_large_till_ms = self
                        .video_large_till_ms
                        .max(now_ms.saturating_add(FIX_SPEAKING_LARGE_VIDEO_MS));
                    continue;
                }
                if self.video_endpoint_pinned || self.video_large_till_ms > now_ms {
                    continue;
                }
                let Some(p) = self.participants.participant_by_peer(peer_id) else {
                    continue;
                };
                let screen_id = p.screen_endpoint().to_string();
                let camera_id = p.camera_endpoint().to_string();
                let screen_ep = VideoEndpoint::new(VideoEndpointType::Screen, peer_id, screen_id);
                let camera_ep = VideoEndpoint::new(VideoEndpointType::Camera, peer_id, camera_id);
                if !screen_ep.is_empty() && self.shown_video_tracks.contains(&screen_ep) {
                    self.set_video_endpoint_large(screen_ep);
                    self.video_large_till_ms = now_ms.saturating_add(FIX_SPEAKING_LARGE_VIDEO_MS);
                } else if !camera_ep.is_empty() && self.shown_video_tracks.contains(&camera_ep) {
                    self.set_video_endpoint_large(camera_ep);
                    self.video_large_till_ms = now_ms.saturating_add(FIX_SPEAKING_LARGE_VIDEO_MS);
                }
            }
        }
    }

    fn handle_possible_discarded(&mut self, data: GroupCallDiscarded) {
        if data.id == self.id {
            self.join_state.finish(0);
            self.join_state.next_action_pending = false;
            match self.state {
                GroupCallState::HangingUp => self.set_state(GroupCallState::Ended),
                GroupCallState::FailedHangingUp => self.set_state(GroupCallState::Failed),
                _ => self.hangup(),
            }
        }
    }

    fn handle_group_call_info_update(&mut self, info: &GroupCallInfo) {
        if info.id != self.id {
            return;
        }
        if info.stream_dc_id.is_some() {
            self.stream_dc_id = info.stream_dc_id;
        }
        let was_scheduled = self.schedule_date != 0;
        self.schedule_date = info.schedule_date.unwrap_or(0);
        if was_scheduled && self.schedule_date == 0 {
            self.initial_join();
        }
    }

    fn handle_participants_update_for_self(&mut self, data: &UpdateGroupCallParticipants) {
        if data.call_id != self.id {
            return;
        }
        let joined = matches!(
            self.state,
            GroupCallState::Joined | GroupCallState::Connecting
        );
        for p in &data.participants {
            let is_self = p.is_self || (p.min && p.peer == self.join_as);
            if is_self {
                if joined {
                    self.apply_self_update(p);
                } else {
                    self.queued_self_updates.push(p.clone());
                }
            }
        }
    }

    fn apply_queued_self_updates(&mut self) {
        let queued = std::mem::take(&mut self.queued_self_updates);
        for p in queued {
            if !matches!(
                self.state,
                GroupCallState::Joined | GroupCallState::Connecting
            ) {
                break;
            }
            self.apply_self_update(&p);
        }
    }

    /// Exact port of `GroupCall::applySelfUpdate` (`calls_group_call.cpp:2619-2661`).
    fn apply_self_update(&mut self, data: &GroupCallParticipantData) {
        if data.left {
            if data.source == self.join_state.ssrc {
                // Removed from the call by server, rejoin.
                self.rejoin();
            }
            return;
        }
        if data.source != self.join_state.ssrc {
            if !self.my_ssrcs.contains(&data.source) {
                // Joined from another device -> hangup cleanly.
                self.join_state.finish(0);
                self.hangup();
            }
            return;
        }

        if data.muted && !data.can_self_unmute {
            let next = if data.raise_hand_rating.unwrap_or(0) != 0 {
                MuteState::RaisedHand
            } else {
                MuteState::ForceMuted
            };
            self.set_muted(next);
        } else if self.instance_mode == InstanceMode::Stream {
            // Un-force-muted while in broadcast stream mode -> rejoin to upgrade to RTC.
            self.rejoin();
        } else if self.muted_by_admin() {
            self.set_muted(MuteState::Muted);
            if !self.instance_transitioning {
                self.notify_about_allowed_to_speak();
            }
        } else if data.muted && self.muted != MuteState::Muted {
            self.set_muted(MuteState::Muted);
        }
    }

    fn on_participant_updated(&mut self, update: &ParticipantUpdate) {
        match (&update.was, &update.now) {
            (Some(was), None) => {
                let mut removed = Vec::new();
                if was.ssrc != 0 {
                    removed.push(was.ssrc);
                }
                let add = was.additional_audio_ssrc();
                if add != 0 {
                    removed.push(add);
                }
                if !removed.is_empty() {
                    self.actions
                        .push(GroupCallAction::RemoveInstanceSsrcs(removed));
                }
                self.fill_active_video_endpoints();
            }
            (was, Some(now)) => {
                if self.video_stream {
                    self.actions.push(GroupCallAction::SetInstanceVolume {
                        ssrc: now.ssrc,
                        volume: self.single_source_volume_value(),
                    });
                } else if !self.rtmp {
                    self.update_instance_volume(was.as_ref(), now);
                }
                if now.peer_id == self.join_as {
                    if self.video_is_working != now.video_joined {
                        self.fill_active_video_endpoints();
                    }
                } else {
                    self.fill_active_video_endpoints();
                }
            }
            (None, None) => {}
        }
    }

    fn single_source_volume_value(&self) -> f64 {
        f64::from(self.single_source_volume) / f64::from(DEFAULT_VOLUME)
    }

    fn update_instance_volumes(&mut self) {
        if self.rtmp {
            self.actions.push(GroupCallAction::SetInstanceVolume {
                ssrc: 1,
                volume: self.single_source_volume_value(),
            });
            return;
        }
        let participants: Vec<GroupCallParticipant> = self.participants.participants().to_vec();
        if self.video_stream {
            let value = self.single_source_volume_value();
            for p in &participants {
                self.actions.push(GroupCallAction::SetInstanceVolume {
                    ssrc: p.ssrc,
                    volume: value,
                });
            }
        } else {
            for p in &participants {
                self.update_instance_volume(None, p);
            }
        }
    }

    fn update_instance_volume(
        &mut self,
        was: Option<&GroupCallParticipant>,
        now: &GroupCallParticipant,
    ) {
        let non_default = now.muted_by_me || now.volume != DEFAULT_VOLUME;
        let volume_changed = match was {
            Some(w) => w.volume != now.volume || w.muted_by_me != now.muted_by_me,
            None => non_default,
        };
        let add_ssrc = now.additional_audio_ssrc();
        let set_primary =
            now.ssrc != 0 && (volume_changed || was.is_some_and(|w| w.ssrc != now.ssrc));
        let set_additional = add_ssrc != 0
            && (volume_changed || was.is_some_and(|w| w.additional_audio_ssrc() != add_ssrc));

        let local_volume = if now.muted_by_me {
            0.0
        } else {
            f64::from(now.volume) / f64::from(DEFAULT_VOLUME)
        };

        if set_primary {
            self.actions.push(GroupCallAction::SetInstanceVolume {
                ssrc: now.ssrc,
                volume: local_volume,
            });
        }
        if set_additional {
            self.actions.push(GroupCallAction::SetInstanceVolume {
                ssrc: add_ssrc,
                volume: local_volume,
            });
        }
    }

    fn apply_participant_locally(
        &mut self,
        participant_peer: PeerId,
        mute: bool,
        volume: Option<i32>,
    ) {
        let Some(p) = self
            .participants
            .participant_by_peer(participant_peer)
            .cloned()
        else {
            return;
        };
        if p.ssrc == 0 {
            return;
        }
        let is_muted = p.muted || (mute && self.can_manage);
        let can_self_unmute = if !self.can_manage {
            p.can_self_unmute
        } else {
            !mute
        };
        let is_muted_by_you = mute && !self.can_manage;
        let synthetic = GroupCallParticipantData {
            muted: is_muted,
            left: false,
            can_self_unmute,
            just_joined: false,
            versioned: false,
            min: false,
            muted_by_you: is_muted_by_you,
            volume_by_admin: p.apply_volume_from_min && volume.is_none(),
            is_self: participant_peer == self.join_as,
            video_joined: p.video_joined,
            peer: participant_peer,
            date: p.date,
            active_date: (p.last_active != 0).then_some(p.last_active),
            source: p.ssrc,
            volume: Some(volume.unwrap_or(p.volume)),
            about: None,
            raise_hand_rating: (p.raised_hand_rating != 0).then_some(p.raised_hand_rating),
            video: None,
            presentation: None,
        };
        let events = self.participants.apply_local_update(&[synthetic]);
        self.process_participant_list_events(events);
    }

    fn emit_edit_participant(&mut self, participant_peer: PeerId, mute: bool, volume: Option<i32>) {
        // Match `GroupCall::editParticipant` (`calls/group/calls_group_call.cpp:4018-4021`):
        // skip emitting `phone.editGroupCallParticipant` if the participant is not in the call.
        if self
            .participants
            .participant_by_peer(participant_peer)
            .is_none()
        {
            return;
        }
        self.actions
            .push(GroupCallAction::EditGroupCallParticipant {
                call: self.input_call(),
                participant: participant_peer,
                muted: Some(mute),
                volume: volume.map(|v| v.clamp(MIN_VOLUME, MAX_VOLUME)),
                raise_hand: None,
                video_stopped: None,
                video_paused: None,
                presentation_paused: None,
            });
    }

    fn set_camera_endpoint(&mut self, endpoint: String) {
        if self.camera_endpoint == endpoint {
            return;
        }
        self.camera_endpoint = endpoint;
        self.fill_active_video_endpoints();
    }

    fn set_screen_endpoint(&mut self, endpoint: String) {
        if self.screen_endpoint == endpoint {
            return;
        }
        self.screen_endpoint = endpoint;
        self.fill_active_video_endpoints();
    }

    fn set_video_endpoint_large(&mut self, endpoint: VideoEndpoint) {
        if endpoint.is_empty() {
            self.video_endpoint_pinned = false;
        }
        self.video_endpoint_large = endpoint;
    }

    /// Synchronize `_activeVideoTracks` from the current participant list + own endpoints.
    ///
    /// Port of `GroupCall::fillActiveVideoEndpoints` (`calls_group_call.cpp:3498-3574`).
    fn fill_active_video_endpoints(&mut self) {
        if self.rtmp {
            self.video_is_working = true;
            let ep = VideoEndpoint::new(VideoEndpointType::Screen, self.peer_id, RTMP_ENDPOINT_ID);
            self.active_video_tracks
                .entry(ep)
                .or_insert(ActiveVideoTrack {
                    peer_id: self.peer_id,
                    paused: false,
                    quality: VideoQuality::Thumbnail,
                    shown: false,
                });
            self.update_requested_video_channels();
            return;
        }

        if let Some(me) = self.participants.participant_by_peer(self.join_as) {
            if me.video_joined {
                self.video_is_working = true;
            } else {
                self.video_is_working = false;
                self.camera_state = VideoState::Inactive;
                self.screen_state = VideoState::Inactive;
            }
        }

        let mut desired: BTreeMap<VideoEndpoint, (PeerId, bool)> = BTreeMap::new();
        for p in self.participants.participants() {
            if p.peer_id == self.join_as {
                continue;
            }
            let cam = p.camera_endpoint();
            if !cam.is_empty() && cam != self.camera_endpoint && cam != self.screen_endpoint {
                desired.insert(
                    VideoEndpoint::new(VideoEndpointType::Camera, p.peer_id, cam),
                    (p.peer_id, p.is_camera_paused()),
                );
            }
            let scr = p.screen_endpoint();
            if !scr.is_empty() && scr != self.camera_endpoint && scr != self.screen_endpoint {
                desired.insert(
                    VideoEndpoint::new(VideoEndpointType::Screen, p.peer_id, scr),
                    (p.peer_id, p.is_screen_paused()),
                );
            }
        }
        if self.camera_state != VideoState::Inactive && !self.camera_endpoint.is_empty() {
            desired.insert(
                VideoEndpoint::new(
                    VideoEndpointType::Camera,
                    self.join_as,
                    self.camera_endpoint.clone(),
                ),
                (self.join_as, self.camera_state == VideoState::Paused),
            );
        }
        if self.screen_state != VideoState::Inactive && !self.screen_endpoint.is_empty() {
            desired.insert(
                VideoEndpoint::new(
                    VideoEndpointType::Screen,
                    self.join_as,
                    self.screen_endpoint.clone(),
                ),
                (self.join_as, self.screen_state == VideoState::Paused),
            );
        }

        // Remove disappeared endpoints.
        let removed: Vec<VideoEndpoint> = self
            .active_video_tracks
            .keys()
            .filter(|ep| !desired.contains_key(*ep))
            .cloned()
            .collect();
        for ep in removed {
            if self.video_endpoint_large == ep {
                self.set_video_endpoint_large(VideoEndpoint::default());
            }
            self.shown_video_tracks.remove(&ep);
            self.active_video_tracks.remove(&ep);
        }

        // Add or update existing endpoints.
        for (ep, (peer_id, paused)) in desired {
            self.active_video_tracks
                .entry(ep.clone())
                .and_modify(|t| t.paused = paused)
                .or_insert_with(|| {
                    if paused {
                        self.shown_video_tracks.insert(ep);
                    }
                    ActiveVideoTrack {
                        peer_id,
                        paused,
                        quality: VideoQuality::Thumbnail,
                        shown: paused,
                    }
                });
        }

        self.update_requested_video_channels();
    }

    fn update_requested_video_channels(&mut self) {
        let mut inputs = Vec::with_capacity(self.active_video_tracks.len());
        let empty_groups = Vec::new();

        for (endpoint, track) in &self.active_video_tracks {
            if endpoint.is_rtmp() {
                inputs.push(RequestedVideoTrackInput {
                    endpoint,
                    desired_quality: track.quality,
                    audio_ssrc: 0,
                    user_id: 0,
                    ssrc_groups: &empty_groups,
                });
                continue;
            }
            let Some(p) = self.participants.participant_by_endpoint(&endpoint.id) else {
                continue;
            };
            let Some(ref params) = p.video_params else {
                continue;
            };
            let ssrc_groups = if params.camera.endpoint_id == endpoint.id {
                &params.camera.ssrc_groups
            } else {
                &params.screen.ssrc_groups
            };
            inputs.push(RequestedVideoTrackInput {
                endpoint,
                desired_quality: track.quality,
                audio_ssrc: p.ssrc,
                user_id: p.peer_id.to_user().map_or(0, |u| u.to_tl()),
                ssrc_groups,
            });
        }

        let self_cam = if self.camera_state != VideoState::Inactive {
            self.camera_endpoint.as_str()
        } else {
            ""
        };
        let self_scr = if self.screen_state != VideoState::Inactive {
            self.screen_endpoint.as_str()
        } else {
            ""
        };
        let channels = compute_requested_video_channels(&inputs, self_cam, self_scr);
        self.actions
            .push(GroupCallAction::SetRequestedVideoChannels(channels));
    }

    fn input_call(&self) -> InputGroupCall {
        InputGroupCall::Id {
            id: self.id,
            access_hash: self.access_hash,
        }
    }

    fn input_call_safe(&self) -> InputGroupCall {
        if let Some(ref slug) = self.conference_slug
            && !slug.is_empty()
        {
            return InputGroupCall::Slug { slug: slug.clone() };
        }
        self.input_call()
    }

    // -----------------------------------------------------------------------
    // Conference E2E (`tde2e`) & In-Call Messages Integration
    // -----------------------------------------------------------------------

    /// Enable E2E conference mode on this session (`GroupCall::initConferenceE2E`).
    pub fn enable_conference_e2e(&mut self, my_user_id: UserId) {
        self.enable_conference_e2e_with_key_pair(my_user_id, E2eKeyPair::generate());
    }

    /// Enable E2E conference mode with a caller-provided key pair (deterministic in tests).
    pub fn enable_conference_e2e_with_key_pair(
        &mut self,
        my_user_id: UserId,
        key_pair: E2eKeyPair,
    ) {
        self.conference = true;
        self.e2e_subchains = [
            SubchainRequestTracker::default(),
            SubchainRequestTracker::default(),
        ];
        let call = E2eCall::with_key_pair(my_user_id, key_pair);
        let enc_dec = call.encrypt_decrypt();
        self.e2e = Some(call);
        self.messages = GroupCallMessages::new(
            self.peer_id,
            PeerId::from_user(my_user_id),
            self.join_as,
            true,
            false,
            self.can_manage,
        );
        self.messages.set_e2e(Some(enc_dec));
        if !self.id.is_zero() {
            self.messages.set_call_ready(true);
        }
    }

    /// Whether this session is running in E2E conference mode.
    #[must_use]
    pub const fn is_conference(&self) -> bool {
        self.conference
    }

    /// Set or update the conference invite link slug (used in `inputGroupCallSlug` before ID is known).
    pub fn set_conference_slug(&mut self, slug: Option<String>) {
        self.conference_slug = slug;
    }

    /// Return the current safe `InputGroupCall` handle (slug if known, else id/access_hash).
    #[must_use]
    pub fn input_call_handle(&self) -> InputGroupCall {
        self.input_call_safe()
    }

    /// Access the active [`E2eCall`] controller, if conference E2E is enabled.
    #[must_use]
    pub fn e2e(&self) -> Option<&E2eCall> {
        self.e2e.as_ref()
    }

    /// Mutably access the active [`E2eCall`] controller, if conference E2E is enabled.
    pub fn e2e_mut(&mut self) -> Option<&mut E2eCall> {
        self.e2e.as_mut()
    }

    /// Return the shared thread-safe [`ConferenceEncryptDecrypt`] handle for `tg-voip`
    /// (`GroupCall::e2eEncryptDecrypt`, `calls_group_call.cpp:4227-4229`).
    #[must_use]
    pub fn e2e_encrypt_decrypt(&self) -> Option<Arc<ConferenceEncryptDecrypt>> {
        self.e2e.as_ref().map(E2eCall::encrypt_decrypt)
    }

    /// Drain emitted [`E2eAction`]s from the conference E2E controller.
    pub fn take_e2e_actions(&mut self) -> Vec<E2eAction> {
        self.e2e
            .as_mut()
            .map_or_else(Vec::new, E2eCall::take_actions)
    }

    /// Handle an incoming `updateGroupCallChainBlocks` update
    /// (`GroupCall::handleUpdate(MTPDupdateGroupCallChainBlocks)`, `calls_group_call.cpp:2565-2603`).
    pub fn handle_chain_blocks_update(
        &mut self,
        update: &UpdateGroupCallChainBlocks,
    ) -> Result<(), E2eError> {
        if update.call_id != self.id {
            return Ok(());
        }
        let Some(ref mut e2e) = self.e2e else {
            return Ok(());
        };
        if !(0..SUBCHAINS_COUNT as i32).contains(&update.sub_chain_id) {
            return Ok(());
        }
        let sc = update.sub_chain_id as usize;
        if self.e2e_subchains[sc].in_flight {
            self.e2e_subchains[sc]
                .pending
                .push((update.blocks.clone(), update.next_offset));
            Ok(())
        } else {
            let from_short_poll = self.e2e_subchains[sc].in_short_poll;
            e2e.apply_blocks(
                update.sub_chain_id,
                update.next_offset,
                &update.blocks,
                from_short_poll,
            )
        }
    }

    /// Mark an outgoing `phone.getGroupCallChainBlocks` request for `subchain` as in-flight
    /// so concurrent push updates are buffered until the response arrives.
    pub fn begin_subchain_request(&mut self, subchain: i32) {
        if (0..SUBCHAINS_COUNT as i32).contains(&subchain) {
            self.e2e_subchains[subchain as usize].in_flight = true;
        }
    }

    /// Complete an in-flight `phone.getGroupCallChainBlocks` request for `subchain`,
    /// applying its response blocks with `from_short_poll = true`, draining buffered push updates,
    /// and invoking `E2eCall::subchain_blocks_request_finished` (`calls_group_call.cpp:1899-1918`).
    pub fn finish_subchain_request(
        &mut self,
        subchain: i32,
        response_updates: &[UpdateGroupCallChainBlocks],
    ) -> Result<(), E2eError> {
        if !(0..SUBCHAINS_COUNT as i32).contains(&subchain) {
            return Ok(());
        }
        let sc = subchain as usize;
        self.e2e_subchains[sc].in_flight = false;
        self.e2e_subchains[sc].in_short_poll = true;
        for upd in response_updates {
            self.handle_chain_blocks_update(upd)?;
        }
        self.e2e_subchains[sc].in_short_poll = false;
        let pending = std::mem::take(&mut self.e2e_subchains[sc].pending);
        if let Some(ref mut e2e) = self.e2e {
            for (blocks, next) in pending {
                e2e.apply_blocks(subchain, next, &blocks, false)?;
            }
            e2e.subchain_blocks_request_finished(subchain)?;
        }
        Ok(())
    }

    /// Compute participants present in the E2E blockchain who are no longer in the active
    /// server participant list (`Data::GroupCall::checkStaleParticipants`,
    /// `data_group_call.cpp:233-256`).
    #[must_use]
    pub fn find_stale_conference_participants(&self) -> BTreeSet<UserId> {
        let Some(ref e2e) = self.e2e else {
            return BTreeSet::new();
        };
        let with_access = e2e.participants_set();
        if with_access.is_empty() {
            return BTreeSet::new();
        }
        let mut active = BTreeSet::new();
        active.insert(e2e.blockchain().my_user_id());
        for p in self.participants.participants() {
            if let Some(uid) = p.peer_id.to_user() {
                active.insert(uid);
            }
        }
        with_access.difference(&active).copied().collect()
    }

    /// Build a signed E2E remove block for `user_ids` (used with
    /// [`crate::group::e2e::build_delete_conference_participants`]).
    pub fn make_conference_remove_block(
        &mut self,
        user_ids: &BTreeSet<UserId>,
    ) -> Result<Option<Vec<u8>>, E2eError> {
        let Some(ref mut e2e) = self.e2e else {
            return Ok(None);
        };
        e2e.make_remove_block(user_ids)
    }

    /// Access the in-call messages & paid Star reactions manager.
    #[must_use]
    pub fn messages(&self) -> &GroupCallMessages {
        &self.messages
    }

    /// Mutably access the in-call messages & paid Star reactions manager.
    pub fn messages_mut(&mut self) -> &mut GroupCallMessages {
        &mut self.messages
    }

    /// Drain emitted [`GroupMessageAction`]s.
    pub fn take_message_actions(&mut self) -> Vec<GroupMessageAction> {
        self.messages.take_actions()
    }
}
