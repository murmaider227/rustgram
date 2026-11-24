//! 1:1 voice/video call protocol state machine.
//!
//! Port of `calls/calls_call.h` and `calls/calls_call.cpp` (protocol state machine,
//! DH handshake, timeouts, rating/debug upload, signaling routing, conference migration).

use rand::RngCore;
use tg_crypto::auth_key::AUTH_KEY_SIZE;

use crate::dh::{
    DhConfig, RANDOM_POWER_SIZE, SHA256_SIZE, compute_ga_hash, compute_key_sha_for_fingerprint,
    finish_dh_key_exchange, generate_mod_exp_first_with_rng, verify_ga_hash,
};
use crate::emoji_fingerprint::{EMOJI_IN_FINGERPRINT, compute_emoji_fingerprint};
use crate::protocol::{
    CallProtocolConfig, DEFAULT_VERSION, Endpoint, HANGUP_TIMEOUT_MS, RtcServer, ServerCallConfig,
    collect_endpoints_and_servers,
};
use crate::signal_bars::{SIGNAL_BAR_FINISHED, SIGNAL_BAR_STARTING, SignalBarsState};
use crate::types::{
    CallId, InputGroupCall, InputPhoneCall, MsgId, PeerId, PhoneCallAccepted, PhoneCallActive,
    PhoneCallDiscardReason, PhoneCallProtocol, PhoneCallUpdate, UpdatePhoneCallSignalingData,
    UserId,
};

/// Call direction (`Calls::CallType` in `calls_call.h:65-68`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallType {
    /// Incoming call initiated by the remote peer.
    Incoming,
    /// Outgoing call initiated by the local user.
    Outgoing,
}

/// Complete 1:1 call state machine states (`Calls::Call::State` in `calls_call.h:144-162`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallState {
    /// Incoming call created, waiting for DH config and `phone.receivedCall`.
    Starting,
    /// Media controller initialized, waiting for peer init packet.
    WaitingInit,
    /// Media controller sent init, waiting for init ack.
    WaitingInitAck,
    /// Media transport connected and call is active.
    Established,
    /// Sending `phone.discardCall` after an error.
    FailedHangingUp,
    /// Terminal failed state.
    Failed,
    /// Sending `phone.discardCall` after user hangup/timeout.
    HangingUp,
    /// Hanging up while migrating 1:1 call to a conference call.
    MigrationHangingUp,
    /// Terminal ended state.
    Ended,
    /// Incoming call was answered or declined on another logged-in device.
    EndedByOtherDevice,
    /// Exchanging DH keys (`phone.acceptCall` / `phone.confirmCall` in flight).
    ExchangingKeys,
    /// Outgoing `phone.requestCall` accepted by server, waiting for peer to receive it.
    Waiting,
    /// Outgoing `phone.requestCall` is in flight (or waiting for DH config).
    Requesting,
    /// Incoming call has sent `phone.receivedCall` and is ringing for the user.
    WaitingIncoming,
    /// Outgoing call received `receive_date` and is ringing on the callee's device.
    Ringing,
    /// Callee was busy or declined; allows `redial()`.
    Busy,
    /// Outgoing call waiting for explicit user confirmation before requesting DH/API.
    WaitingUserConfirmation,
}

impl CallState {
    /// Returns `true` if the call was accepted by both sides and may follow a
    /// `phoneCallDiscardReasonMigrateConferenceCall` migration.
    ///
    /// Exact port of `AcceptedByBothSides` in `calls_call.cpp:57-69`.
    pub const fn accepted_by_both_sides(self) -> bool {
        matches!(
            self,
            CallState::ExchangingKeys
                | CallState::WaitingInit
                | CallState::WaitingInitAck
                | CallState::Established
                | CallState::MigrationHangingUp
        )
    }

    /// Returns `true` if the state is terminal or hanging up.
    pub const fn is_finishing_or_finished(self) -> bool {
        matches!(
            self,
            CallState::FailedHangingUp
                | CallState::Failed
                | CallState::HangingUp
                | CallState::MigrationHangingUp
                | CallState::Ended
                | CallState::EndedByOtherDevice
                | CallState::Busy
        )
    }
}

/// Logical error categories emitted by a call (`Calls::ErrorType` in `calls_call.h:52-58`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallErrorType {
    /// No camera device available.
    NoCamera,
    /// No microphone device available.
    NoMicrophone,
    /// Attempted to enable video before call was established.
    NotStartedCall,
    /// Negotiated media engine does not support video.
    NotVideoCall,
    /// Other error.
    Unknown,
}

/// Remote participant audio state (`Calls::Call::RemoteAudioState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAudioState {
    /// Remote microphone muted.
    Muted,
    /// Remote microphone active.
    Active,
}

/// Video track state (`Webrtc::VideoState` / `tgcalls::VideoState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoState {
    /// Video disabled.
    Inactive,
    /// Video temporarily paused.
    Paused,
    /// Video actively streaming.
    Active,
}

/// Remote battery state (`Calls::Call::RemoteBatteryState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteBatteryState {
    /// Remote device reported low battery.
    Low,
    /// Remote device battery is normal.
    Normal,
}

/// Sound effects requested by the call state machine (`Call::Delegate::CallSound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSound {
    /// Key exchange / connecting chime (`call_connect`).
    Connecting,
    /// Remote busy tone (`call_busy`).
    Busy,
    /// Call ended tone (`call_end`).
    Ended,
}

/// Media controller lifecycle states (`tgcalls::State` handled in `calls_call.cpp:1299-1329`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerState {
    /// Controller is waiting for peer init packet.
    WaitInit,
    /// Controller is waiting for init ack.
    WaitInitAck,
    /// Media connection established.
    Established,
    /// Media controller failed with an error code (e.g. `"ERROR_INCOMPATIBLE"`, `"ERROR_AUDIO_IO"`).
    Failed(String),
}

/// Timers managed by the caller on behalf of [`Call`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallTimerKind {
    /// Ringing / receive timeout (`_discardByTimeoutTimer`).
    DiscardByTimeout,
    /// Hangup completion timeout (`_finishByTimeoutTimer`, 5 000 ms).
    FinishByTimeout,
}

/// Descriptor passed to the media engine when the call is confirmed (`tgcalls::Descriptor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerDescriptor {
    /// Negotiated library version (first element of `phoneCall.protocol.library_versions`,
    /// falling back to [`DEFAULT_VERSION`] `"2.4.4"`).
    pub version: String,
    /// Initialization timeout in milliseconds (`serverConfig.callConnectTimeoutMs`).
    pub initialization_timeout_ms: u64,
    /// Packet receive timeout in milliseconds (`serverConfig.callPacketTimeoutMs`).
    pub receive_timeout_ms: u64,
    /// Whether direct P2P connections are allowed (`phoneCall.p2p_allowed`).
    pub enable_p2p: bool,
    /// Maximum negotiated API layer (`phoneCall.protocol.max_layer`).
    pub max_api_layer: i32,
    /// 256-byte shared Diffie-Hellman auth key.
    pub encryption_key: [u8; AUTH_KEY_SIZE],
    /// `true` if this side initiated the call.
    pub is_outgoing: bool,
    /// Legacy UDP relay reflector endpoints (`AppendEndpoint`).
    pub endpoints: Vec<Endpoint>,
    /// WebRTC STUN/TURN servers (`AppendServer`).
    pub rtc_servers: Vec<RtcServer>,
    /// Optional custom JSON parameters from the server.
    pub custom_parameters: Option<String>,
}

/// Output actions emitted by [`Call`] (Sans-IO pattern).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallAction {
    /// Send `phone.requestCall`.
    RequestCall {
        /// Request a video call.
        video: bool,
        /// Target user ID.
        user_id: UserId,
        /// Random 32-bit deduplication ID.
        random_id: i32,
        /// 32-byte `SHA256(g_a)` commitment.
        g_a_hash: [u8; SHA256_SIZE],
        /// Advertised call protocol.
        protocol: PhoneCallProtocol,
    },
    /// Send `phone.receivedCall`.
    ReceivedCall {
        /// Call handle.
        peer: InputPhoneCall,
    },
    /// Send `phone.acceptCall`.
    AcceptCall {
        /// Call handle.
        peer: InputPhoneCall,
        /// Callee's DH public value `g_b`.
        g_b: Vec<u8>,
        /// Advertised call protocol.
        protocol: PhoneCallProtocol,
    },
    /// Send `phone.confirmCall`.
    ConfirmCall {
        /// Call handle.
        peer: InputPhoneCall,
        /// Caller's DH public value `g_a`.
        g_a: Vec<u8>,
        /// Computed 64-bit key fingerprint.
        key_fingerprint: u64,
        /// Advertised call protocol.
        protocol: PhoneCallProtocol,
    },
    /// Send `phone.discardCall`.
    DiscardCall {
        /// Whether video was active on either side.
        video: bool,
        /// Call handle.
        peer: InputPhoneCall,
        /// Elapsed call duration in whole seconds.
        duration: i32,
        /// Discard reason.
        reason: PhoneCallDiscardReason,
        /// Preferred relay connection ID reported by media controller (0 if none).
        connection_id: i64,
    },
    /// Send `phone.setCallRating`.
    SetCallRating {
        /// Whether the rating was user-initiated (`flags.0`).
        user_initiative: bool,
        /// Call handle.
        peer: InputPhoneCall,
        /// Rating (`1..=5`).
        rating: i32,
        /// Optional feedback comment.
        comment: String,
    },
    /// Send `phone.saveCallDebug`.
    SaveCallDebug {
        /// Call handle.
        peer: InputPhoneCall,
        /// JSON debug log string from media controller.
        debug_json: String,
    },
    /// Send `phone.sendSignalingData`.
    SendSignalingData {
        /// Call handle.
        peer: InputPhoneCall,
        /// Encrypted signaling packet bytes.
        data: Vec<u8>,
    },
    /// Send `phone.getGroupCall` (for accepting a conference invite or migrating a 1:1 call).
    GetGroupCall {
        /// Input group call (`Slug` or `InviteMessage`).
        call: InputGroupCall,
        /// Participant preview limit (`5` in `calls_call.cpp:460, 957`).
        limit: i32,
    },
    /// Decline incoming conference call invites for `conference_id`.
    DeclineIncomingConferenceInvites {
        /// Conference call ID.
        conference_id: CallId,
    },
    /// Instantiate and start the media controller with negotiated parameters.
    StartController(Box<ControllerDescriptor>),
    /// Stop and destroy the active media controller.
    DestroyController,
    /// Deliver decrypted/raw incoming signaling bytes to the media controller.
    ReceiveSignalingData(Vec<u8>),
    /// Request microphone (and optionally camera) permissions before calling `actually_answer`.
    RequestPermissions {
        /// Whether camera permission is also required.
        video: bool,
    },
    /// Play a one-shot call sound.
    PlaySound(CallSound),
    /// Start looping incoming/outgoing ringtone.
    StartWaitingTrack {
        /// `true` for outgoing ringback tone, `false` for incoming ringtone.
        outgoing: bool,
    },
    /// Stop looping ringtone.
    StopWaitingTrack,
    /// Arm or reset a timer.
    SetTimer {
        /// Timer kind.
        kind: CallTimerKind,
        /// Duration in milliseconds.
        duration_ms: u64,
    },
    /// Cancel an armed timer.
    CancelTimer(CallTimerKind),
    /// Notify the delegate that the call finished cleanly (`callFinished`).
    NotifyFinished,
    /// Notify the delegate that the call failed (`callFailed`).
    NotifyFailed,
    /// Notify the delegate to start a DH refresh for redial (`callRedial`).
    NotifyRedial,
    /// Present the call rating dialog (`showRatingBox`).
    ShowRatingBox,
    /// Surface a user-visible call error message (e.g. `USER_PRIVACY_RESTRICTED`).
    ShowError(String),
    /// State transitioned from `from` to `to`.
    StateChanged {
        /// Previous state.
        from: CallState,
        /// New state.
        to: CallState,
    },
    /// Signal bar count changed.
    SignalBarCountChanged(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinishType {
    None,
    Ended,
    Failed,
}

/// Sans-IO 1:1 call protocol state machine.
///
/// Mirrors `Calls::Call` from `calls/calls_call.h` and `calls/calls_call.cpp`.
#[derive(Debug, Clone)]
pub struct Call {
    self_user_id: UserId,
    peer_user_id: UserId,
    call_type: CallType,
    state: CallState,
    server_config: ServerCallConfig,
    protocol_config: CallProtocolConfig,

    id: CallId,
    access_hash: i64,

    conference_id: CallId,
    conference_invite_msg_id: MsgId,
    conference_participants: Vec<PeerId>,
    conference_supported: bool,

    dh_config: DhConfig,
    g_a: Vec<u8>,
    g_b: Vec<u8>,
    g_a_hash: Vec<u8>,
    random_power: Vec<u8>,
    auth_key: Option<[u8; AUTH_KEY_SIZE]>,
    key_fingerprint: u64,

    finish_after_requesting_call: FinishType,
    pending_finish_final_state: Option<CallState>,
    answer_after_dh_config_received: bool,

    controller_started: bool,
    preferred_relay_id: i64,
    debug_log: String,
    debug_requested: bool,
    debug_log_sent: bool,

    muted: bool,
    remote_audio_state: RemoteAudioState,
    remote_video_state: VideoState,
    outgoing_video_state: VideoState,
    remote_battery_state: RemoteBatteryState,
    signal_bars: SignalBarsState,

    start_time_ms: Option<u64>,
    rating: i32,
    discarded_duration: i32,
    rating_requested: bool,
    rating_in_panel: bool,
    rating_box_shown: bool,

    actions: Vec<CallAction>,
}

impl Call {
    /// Create a new outgoing 1:1 call (starts in [`CallState::WaitingUserConfirmation`]).
    ///
    /// Port of `Call::Call` outgoing branch (`calls_call.cpp:255-256`).
    pub fn new_outgoing(
        self_user_id: UserId,
        peer_user_id: UserId,
        video: bool,
        server_config: ServerCallConfig,
        protocol_config: CallProtocolConfig,
    ) -> Self {
        let initial_video = if video {
            VideoState::Active
        } else {
            VideoState::Inactive
        };
        let mut call = Self::new_base(
            self_user_id,
            peer_user_id,
            CallType::Outgoing,
            CallState::Starting,
            initial_video,
            server_config,
            protocol_config,
        );
        call.set_state(CallState::WaitingUserConfirmation, 0);
        call
    }

    /// Create a new incoming 1:1 call (starts in [`CallState::Starting`], arms ring timeout
    /// and starts incoming waiting track).
    ///
    /// Port of `Call::Call` incoming branch (`calls_call.cpp:257-261`).
    pub fn new_incoming(
        self_user_id: UserId,
        peer_user_id: UserId,
        video: bool,
        server_config: ServerCallConfig,
        protocol_config: CallProtocolConfig,
    ) -> Self {
        let initial_video = if video {
            VideoState::Active
        } else {
            VideoState::Inactive
        };
        let mut call = Self::new_base(
            self_user_id,
            peer_user_id,
            CallType::Incoming,
            CallState::Starting,
            initial_video,
            server_config,
            protocol_config,
        );
        call.actions.push(CallAction::SetTimer {
            kind: CallTimerKind::DiscardByTimeout,
            duration_ms: server_config.call_ring_timeout_ms,
        });
        call.actions
            .push(CallAction::StartWaitingTrack { outgoing: false });
        call
    }

    /// Create an incoming conference call invitation placeholder
    /// (starts in [`CallState::WaitingIncoming`]).
    ///
    /// Port of conference invite `Call::Call` overload (`calls_call.cpp:266-307`).
    #[allow(clippy::too_many_arguments)]
    pub fn new_conference_invite(
        self_user_id: UserId,
        inviter_user_id: UserId,
        local_call_id: CallId,
        conference_id: CallId,
        conference_invite_msg_id: MsgId,
        conference_participants: Vec<PeerId>,
        video: bool,
        server_config: ServerCallConfig,
        protocol_config: CallProtocolConfig,
    ) -> Self {
        let initial_video = if video {
            VideoState::Active
        } else {
            VideoState::Inactive
        };
        let mut call = Self::new_base(
            self_user_id,
            inviter_user_id,
            CallType::Incoming,
            CallState::WaitingIncoming,
            initial_video,
            server_config,
            protocol_config,
        );
        call.id = local_call_id;
        call.conference_id = conference_id;
        call.conference_invite_msg_id = conference_invite_msg_id;
        call.conference_participants = conference_participants;
        call.actions
            .push(CallAction::StartWaitingTrack { outgoing: false });
        call
    }

    fn new_base(
        self_user_id: UserId,
        peer_user_id: UserId,
        call_type: CallType,
        state: CallState,
        initial_video: VideoState,
        server_config: ServerCallConfig,
        protocol_config: CallProtocolConfig,
    ) -> Self {
        Self {
            self_user_id,
            peer_user_id,
            call_type,
            state,
            server_config,
            protocol_config,
            id: CallId::ZERO,
            access_hash: 0,
            conference_id: CallId::ZERO,
            conference_invite_msg_id: MsgId::ZERO,
            conference_participants: Vec::new(),
            conference_supported: false,
            dh_config: DhConfig::default(),
            g_a: Vec::new(),
            g_b: Vec::new(),
            g_a_hash: Vec::new(),
            random_power: Vec::new(),
            auth_key: None,
            key_fingerprint: 0,
            finish_after_requesting_call: FinishType::None,
            pending_finish_final_state: None,
            answer_after_dh_config_received: false,
            controller_started: false,
            preferred_relay_id: 0,
            debug_log: String::new(),
            debug_requested: false,
            debug_log_sent: false,
            muted: false,
            remote_audio_state: RemoteAudioState::Active,
            remote_video_state: initial_video,
            outgoing_video_state: initial_video,
            remote_battery_state: RemoteBatteryState::Normal,
            signal_bars: SignalBarsState::new(),
            start_time_ms: None,
            rating: 0,
            discarded_duration: 0,
            rating_requested: false,
            rating_in_panel: false,
            rating_box_shown: false,
            actions: Vec::new(),
        }
    }

    /// Drain all actions accumulated since the last call to `take_actions`.
    pub fn take_actions(&mut self) -> Vec<CallAction> {
        std::mem::take(&mut self.actions)
    }

    /// Direction of this call (`Incoming` or `Outgoing`).
    pub const fn call_type(&self) -> CallType {
        self.call_type
    }

    /// Remote peer user ID.
    pub const fn peer_user_id(&self) -> UserId {
        self.peer_user_id
    }

    /// Call ID (0 until assigned by server or `phoneCallRequested`).
    pub const fn id(&self) -> CallId {
        self.id
    }

    /// Call access hash.
    pub const fn access_hash(&self) -> i64 {
        self.access_hash
    }

    /// Current state machine state.
    pub const fn state(&self) -> CallState {
        self.state
    }

    /// Whether this call represents an incoming conference invitation.
    pub const fn is_conference_invite(&self) -> bool {
        !self.conference_id.is_zero()
    }

    /// Conference ID if this is a conference invite.
    pub const fn conference_id(&self) -> CallId {
        self.conference_id
    }

    /// Conference invite message ID if this is a conference invite.
    pub const fn conference_invite_msg_id(&self) -> MsgId {
        self.conference_invite_msg_id
    }

    /// Other participants listed on a conference invitation.
    pub fn conference_participants(&self) -> &[PeerId] {
        &self.conference_participants
    }

    /// Whether the negotiated call supports migration to a conference call.
    pub const fn conference_supported(&self) -> bool {
        self.conference_supported
    }

    /// Returns `true` if this is an incoming call waiting to be answered or declined.
    ///
    /// Exact port of `Call::isIncomingWaiting` (`calls_call.cpp:328-334`).
    pub const fn is_incoming_waiting(&self) -> bool {
        matches!(self.call_type, CallType::Incoming)
            && matches!(self.state, CallState::Starting | CallState::WaitingIncoming)
    }

    /// Whether local microphone is muted.
    pub const fn muted(&self) -> bool {
        self.muted
    }

    /// Toggle local microphone mute.
    pub fn set_muted(&mut self, mute: bool) {
        self.muted = mute;
    }

    /// Current remote audio state.
    pub const fn remote_audio_state(&self) -> RemoteAudioState {
        self.remote_audio_state
    }

    /// Current remote video state.
    pub const fn remote_video_state(&self) -> VideoState {
        self.remote_video_state
    }

    /// Current outgoing video state.
    pub const fn outgoing_video_state(&self) -> VideoState {
        self.outgoing_video_state
    }

    /// Whether outgoing video is currently active or paused.
    pub const fn is_sharing_video(&self) -> bool {
        !matches!(self.outgoing_video_state, VideoState::Inactive)
    }

    /// Current remote battery state.
    pub const fn remote_battery_state(&self) -> RemoteBatteryState {
        self.remote_battery_state
    }

    /// Current signal bars state.
    pub const fn signal_bars(&self) -> &SignalBarsState {
        &self.signal_bars
    }

    /// Elapsed duration in milliseconds since [`CallState::Established`], or 0.
    pub const fn duration_ms(&self, now_ms: u64) -> u64 {
        match self.start_time_ms {
            Some(start) => now_ms.saturating_sub(start),
            None => 0,
        }
    }

    /// Whether the server requested a call quality rating.
    pub const fn rating_requested(&self) -> bool {
        self.rating_requested
    }

    /// Whether the server requested uploading a call debug log (`phoneCallDiscarded.need_debug`)
    /// and upload is still waiting for [`Self::set_debug_log`].
    pub const fn debug_requested(&self) -> bool {
        self.debug_requested
    }

    /// Whether the rating UI has been taken into the call panel.
    pub const fn rating_in_panel(&self) -> bool {
        self.rating_in_panel
    }

    /// Duration in seconds reported by `phoneCallDiscarded`.
    pub const fn discarded_duration(&self) -> i32 {
        self.discarded_duration
    }

    /// Computed 64-bit key fingerprint (0 before key agreement completes).
    pub const fn key_fingerprint(&self) -> u64 {
        self.key_fingerprint
    }

    /// 256-byte shared auth key once DH agreement completes.
    pub const fn auth_key(&self) -> Option<&[u8; AUTH_KEY_SIZE]> {
        self.auth_key.as_ref()
    }

    /// Whether `get_key_sha_for_fingerprint` / emoji fingerprint is ready.
    ///
    /// Port of `Call::isKeyShaForFingerprintReady` (`calls_call.cpp:791-793`).
    pub fn is_key_sha_for_fingerprint_ready(&self) -> bool {
        self.key_fingerprint != 0 && self.auth_key.is_some() && !self.g_a.is_empty()
    }

    /// Compute `SHA256(auth_key || g_a)` for emoji fingerprint display.
    ///
    /// Port of `Call::getKeyShaForFingerprint` (`calls_call.cpp:795-811`).
    pub fn key_sha_for_fingerprint(&self) -> Option<[u8; SHA256_SIZE]> {
        if !self.is_key_sha_for_fingerprint_ready() {
            return None;
        }
        let key = self.auth_key.as_ref()?;
        Some(compute_key_sha_for_fingerprint(key, &self.g_a))
    }

    /// Compute the 4 call verification emoji once the key exchange completes.
    pub fn emoji_fingerprint(&self) -> Option<[String; EMOJI_IN_FINGERPRINT]> {
        let sha = self.key_sha_for_fingerprint()?;
        Some(compute_emoji_fingerprint(&sha))
    }

    /// Record the preferred relay connection ID reported by the media controller
    /// (sent in `phone.discardCall`).
    pub fn set_preferred_relay_id(&mut self, connection_id: i64) {
        self.preferred_relay_id = connection_id;
    }

    /// Record the debug JSON log from the media controller (uploaded if `need_debug` is set).
    ///
    /// If `phoneCallDiscarded` with `need_debug = true` already arrived before the media
    /// controller finished producing its debug log, calling this method automatically emits
    /// [`CallAction::SaveCallDebug`] once.
    pub fn set_debug_log(&mut self, debug_json: String) {
        self.debug_log = debug_json;
        if self.debug_requested
            && !self.debug_log_sent
            && !self.debug_log.is_empty()
            && !self.id.is_zero()
            && self.access_hash != 0
        {
            self.debug_requested = false;
            self.debug_log_sent = true;
            self.actions.push(CallAction::SaveCallDebug {
                peer: self.input_phone_call(),
                debug_json: self.debug_log.clone(),
            });
        }
    }

    /// Update remote media state reported by the media controller.
    pub fn update_remote_media_state(&mut self, audio: RemoteAudioState, video: VideoState) {
        self.remote_audio_state = audio;
        self.remote_video_state = video;
    }

    /// Update remote battery state reported by the media controller.
    pub fn update_remote_battery_state(&mut self, is_low: bool) {
        self.remote_battery_state = if is_low {
            RemoteBatteryState::Low
        } else {
            RemoteBatteryState::Normal
        };
    }

    /// Confirm an outgoing call (`WaitingUserConfirmation -> Requesting`).
    ///
    /// Port of `Call::applyUserConfirmation` (`calls_call.cpp:430-436`).
    pub fn apply_user_confirmation(&mut self) {
        if self.is_conference_invite() {
            return;
        }
        if self.state == CallState::WaitingUserConfirmation {
            self.set_state(CallState::Requesting, 0);
        }
    }

    /// Provide the validated [`DhConfig`] and 256-byte server random seed, computing
    /// `g_a` / `g_b` with OS randomness and sending `phone.requestCall` or `phone.receivedCall`.
    ///
    /// Port of `Call::start` (`calls_call.cpp:336-357`).
    pub fn start(&mut self, dh_config: DhConfig, random_seed: &[u8]) {
        let mut rng = rand::rngs::OsRng;
        let random_id = rng.next_u32() as i32;
        self.start_with_rng(dh_config, random_seed, random_id, |buf| rng.fill_bytes(buf));
    }

    /// Deterministic version of [`Self::start`] for testing.
    pub fn start_with_rng<F>(
        &mut self,
        dh_config: DhConfig,
        random_seed: &[u8],
        random_id: i32,
        fill_local_random: F,
    ) where
        F: FnMut(&mut [u8; RANDOM_POWER_SIZE]),
    {
        if self.is_conference_invite() {
            return;
        }
        self.dh_config = dh_config;
        if !self.generate_mod_exp_first(random_seed, fill_local_random) {
            return;
        }
        match self.state {
            CallState::Starting | CallState::Requesting => match self.call_type {
                CallType::Outgoing => self.start_outgoing(random_id),
                CallType::Incoming => self.start_incoming(),
            },
            CallState::ExchangingKeys if self.answer_after_dh_config_received => {
                self.actually_answer();
            }
            _ => {}
        }
    }

    fn generate_mod_exp_first<F>(&mut self, random_seed: &[u8], fill_local_random: F) -> bool
    where
        F: FnMut(&mut [u8; RANDOM_POWER_SIZE]),
    {
        match generate_mod_exp_first_with_rng(&self.dh_config, random_seed, fill_local_random) {
            Ok(first) => {
                self.random_power = first.random_power.to_vec();
                if self.call_type == CallType::Incoming {
                    self.g_b = first.modexp.clone();
                } else {
                    self.g_a = first.modexp.clone();
                    self.g_a_hash = compute_ga_hash(&self.g_a).to_vec();
                }
                true
            }
            Err(_) => {
                self.finish(
                    FinishType::Failed,
                    PhoneCallDiscardReason::Disconnect,
                    0,
                    false,
                );
                false
            }
        }
    }

    fn start_outgoing(&mut self, random_id: i32) {
        if self.call_type != CallType::Outgoing
            || self.state != CallState::Requesting
            || self.g_a_hash.len() != SHA256_SIZE
        {
            return;
        }
        let mut hash = [0u8; SHA256_SIZE];
        hash.copy_from_slice(&self.g_a_hash);
        self.actions.push(CallAction::RequestCall {
            video: self.is_sharing_video(),
            user_id: self.peer_user_id,
            random_id,
            g_a_hash: hash,
            protocol: self.protocol_config.to_tl_protocol(),
        });
    }

    fn start_incoming(&mut self) {
        if self.call_type != CallType::Incoming || self.state != CallState::Starting {
            return;
        }
        self.actions.push(CallAction::ReceivedCall {
            peer: self.input_phone_call(),
        });
    }

    /// Process the API result of `phone.requestCall`.
    ///
    /// Port of `Call::startOutgoing` `.done()` / `.fail()` (`calls_call.cpp:379-411`).
    pub fn handle_request_call_result(
        &mut self,
        result: Result<PhoneCallUpdate, &str>,
        now_ms: u64,
    ) {
        match result {
            Ok(phone_call) => {
                self.set_state(CallState::Waiting, now_ms);
                let waiting = match phone_call {
                    PhoneCallUpdate::Waiting(w) => w,
                    _ => {
                        self.finish(
                            FinishType::Failed,
                            PhoneCallDiscardReason::Disconnect,
                            now_ms,
                            false,
                        );
                        return;
                    }
                };
                self.id = waiting.id;
                self.access_hash = waiting.access_hash;
                if self.finish_after_requesting_call != FinishType::None {
                    if self.finish_after_requesting_call == FinishType::Failed {
                        self.finish(
                            FinishType::Failed,
                            PhoneCallDiscardReason::Disconnect,
                            now_ms,
                            false,
                        );
                    } else {
                        self.hangup(None, now_ms, false);
                    }
                    return;
                }
                self.actions.push(CallAction::SetTimer {
                    kind: CallTimerKind::DiscardByTimeout,
                    duration_ms: self.server_config.call_receive_timeout_ms,
                });
                self.handle_update(PhoneCallUpdate::Waiting(waiting), now_ms);
            }
            Err(error) => self.handle_request_error(error, now_ms),
        }
    }

    /// Process the API result of `phone.receivedCall`.
    ///
    /// Port of `Call::startIncoming` `.done()` / `.fail()` (`calls_call.cpp:421-427`).
    pub fn handle_received_call_result(&mut self, result: Result<(), &str>, now_ms: u64) {
        match result {
            Ok(()) => {
                if self.state == CallState::Starting {
                    self.set_state(CallState::WaitingIncoming, now_ms);
                }
            }
            Err(error) => self.handle_request_error(error, now_ms),
        }
    }

    /// Request permissions to answer an incoming call (`Call::answer`, `calls_call.cpp:438-443`).
    pub fn answer(&mut self) {
        self.actions.push(CallAction::RequestPermissions {
            video: self.is_sharing_video(),
        });
    }

    /// Answer an incoming call (after permissions are granted).
    ///
    /// Port of `Call::actuallyAnswer` and `Call::acceptConferenceInvite` (`calls_call.cpp:453-529`).
    pub fn actually_answer(&mut self) {
        if self.call_type != CallType::Incoming {
            return;
        }
        if self.is_conference_invite() {
            if self.state != CallState::WaitingIncoming {
                return;
            }
            self.set_state(CallState::ExchangingKeys, 0);
            self.actions.push(CallAction::GetGroupCall {
                call: InputGroupCall::InviteMessage {
                    msg_id: self.conference_invite_msg_id,
                },
                limit: 5,
            });
            return;
        }

        if self.state != CallState::Starting
            && self.state != CallState::WaitingIncoming
            && !(self.state == CallState::ExchangingKeys && self.answer_after_dh_config_received)
        {
            return;
        }
        self.set_state(CallState::ExchangingKeys, 0);
        if self.g_b.is_empty() {
            self.answer_after_dh_config_received = true;
            return;
        }
        self.answer_after_dh_config_received = false;

        self.actions.push(CallAction::AcceptCall {
            peer: self.input_phone_call(),
            g_b: self.g_b.clone(),
            protocol: self.protocol_config.to_tl_protocol(),
        });
    }

    /// Process the API result of `phone.acceptCall`.
    ///
    /// Port of `Call::actuallyAnswer` `.done()` / `.fail()` (`calls_call.cpp:513-528`).
    pub fn handle_accept_call_result(
        &mut self,
        result: Result<PhoneCallUpdate, &str>,
        now_ms: u64,
    ) {
        match result {
            Ok(phone_call) => {
                if !matches!(phone_call, PhoneCallUpdate::Waiting(_)) {
                    self.finish(
                        FinishType::Failed,
                        PhoneCallDiscardReason::Disconnect,
                        now_ms,
                        false,
                    );
                    return;
                }
                self.handle_update(phone_call, now_ms);
            }
            Err(error) => self.handle_request_error(error, now_ms),
        }
    }

    /// Process the API result of `phone.confirmCall`.
    ///
    /// Port of `Call::confirmAcceptedCall` `.done()` / `.fail()` (`calls_call.cpp:1052-1067`).
    pub fn handle_confirm_call_result(
        &mut self,
        result: Result<PhoneCallUpdate, &str>,
        now_ms: u64,
    ) {
        match result {
            Ok(phone_call) => match phone_call {
                PhoneCallUpdate::Active(active) => {
                    self.create_and_start_controller(&active, now_ms);
                }
                _ => {
                    self.finish(
                        FinishType::Failed,
                        PhoneCallDiscardReason::Disconnect,
                        now_ms,
                        false,
                    );
                }
            },
            Err(error) => self.handle_request_error(error, now_ms),
        }
    }

    /// Process the API result of `phone.discardCall`.
    ///
    /// Port of `Call::finish` `.done()` / `.fail()` (`calls_call.cpp:1650-1657`).
    pub fn handle_discard_call_result(&mut self, now_ms: u64) {
        if let Some(final_state) = self.pending_finish_final_state.take() {
            self.set_state(final_state, now_ms);
        }
    }

    /// Process the API result of `phone.sendSignalingData`.
    ///
    /// Port of `Call::sendSignalingData` `.done()` / `.fail()` (`calls_call.cpp:774-780`).
    pub fn handle_send_signaling_data_result(&mut self, result: Result<bool, &str>, now_ms: u64) {
        match result {
            Ok(true) => {}
            Ok(false) => {
                self.finish(
                    FinishType::Failed,
                    PhoneCallDiscardReason::Disconnect,
                    now_ms,
                    false,
                );
            }
            Err(error) => self.handle_request_error(error, now_ms),
        }
    }

    /// Process a `PhoneCall` update (`updatePhoneCall.phone_call`).
    ///
    /// Returns `true` if the update matched this call.
    /// Port of `Call::handleUpdate` (`calls_call.cpp:813-948`).
    pub fn handle_update(&mut self, call: PhoneCallUpdate, now_ms: u64) -> bool {
        match call {
            PhoneCallUpdate::Requested(data) => {
                if self.call_type != CallType::Incoming
                    || !self.id.is_zero()
                    || self.peer_user_id != data.admin_id
                {
                    return false;
                }
                if self.self_user_id != data.participant_id {
                    self.finish(
                        FinishType::Failed,
                        PhoneCallDiscardReason::Disconnect,
                        now_ms,
                        false,
                    );
                    return true;
                }
                self.id = data.id;
                self.access_hash = data.access_hash;
                if data.g_a_hash.len() != SHA256_SIZE {
                    self.finish(
                        FinishType::Failed,
                        PhoneCallDiscardReason::Disconnect,
                        now_ms,
                        false,
                    );
                    return true;
                }
                self.g_a_hash = data.g_a_hash;
                true
            }
            PhoneCallUpdate::Empty { id } => {
                if id != self.id {
                    return false;
                }
                self.finish(
                    FinishType::Failed,
                    PhoneCallDiscardReason::Disconnect,
                    now_ms,
                    false,
                );
                true
            }
            PhoneCallUpdate::Waiting(data) => {
                if data.id != self.id {
                    return false;
                }
                if self.call_type == CallType::Outgoing
                    && self.state == CallState::Waiting
                    && data.receive_date.unwrap_or(0) != 0
                {
                    self.actions.push(CallAction::SetTimer {
                        kind: CallTimerKind::DiscardByTimeout,
                        duration_ms: self.server_config.call_ring_timeout_ms,
                    });
                    self.set_state(CallState::Ringing, now_ms);
                    self.actions
                        .push(CallAction::StartWaitingTrack { outgoing: true });
                }
                true
            }
            PhoneCallUpdate::Active(data) => {
                if data.id != self.id {
                    return false;
                }
                if self.call_type == CallType::Incoming
                    && self.state == CallState::ExchangingKeys
                    && !self.controller_started
                {
                    self.start_confirmed_call(&data, now_ms);
                }
                true
            }
            PhoneCallUpdate::Discarded(data) => {
                if data.id != self.id {
                    return false;
                }
                if data.need_debug
                    && !self.id.is_zero()
                    && self.access_hash != 0
                    && !self.debug_log_sent
                {
                    if !self.debug_log.is_empty() {
                        self.debug_log_sent = true;
                        self.debug_requested = false;
                        self.actions.push(CallAction::SaveCallDebug {
                            peer: self.input_phone_call(),
                            debug_json: self.debug_log.clone(),
                        });
                    } else {
                        self.debug_requested = true;
                    }
                }
                if data.need_rating && !self.id.is_zero() && self.access_hash != 0 {
                    self.rating_requested = true;
                }
                if let Some(duration) = data.duration {
                    self.discarded_duration = duration;
                }
                let accepted = self.state.accepted_by_both_sides();
                match &data.reason {
                    Some(PhoneCallDiscardReason::MigrateConferenceCall { slug }) if accepted => {
                        self.finish_by_migration(slug.clone(), now_ms);
                    }
                    Some(PhoneCallDiscardReason::Busy) => {
                        self.set_state(CallState::Busy, now_ms);
                    }
                    _ if self.call_type == CallType::Outgoing
                        || self.state == CallState::HangingUp =>
                    {
                        self.set_state(CallState::Ended, now_ms);
                    }
                    _ => {
                        self.set_state(CallState::EndedByOtherDevice, now_ms);
                    }
                }
                if self.rating_requested && !self.rating_in_panel && !self.rating_box_shown {
                    self.rating_box_shown = true;
                    self.actions.push(CallAction::ShowRatingBox);
                }
                true
            }
            PhoneCallUpdate::Accepted(data) => {
                if data.id != self.id {
                    return false;
                }
                if self.call_type != CallType::Outgoing {
                    self.finish(
                        FinishType::Failed,
                        PhoneCallDiscardReason::Disconnect,
                        now_ms,
                        false,
                    );
                } else if self.check_call_common_fields(
                    data.access_hash,
                    data.admin_id,
                    data.participant_id,
                    now_ms,
                ) {
                    self.confirm_accepted_call(&data, now_ms);
                }
                true
            }
        }
    }

    /// Route an incoming `updatePhoneCallSignalingData` to the media controller.
    ///
    /// Port of `Call::handleSignalingData` (`calls_call.cpp:1003-1015`).
    pub fn handle_signaling_data(&mut self, data: UpdatePhoneCallSignalingData) -> bool {
        if data.phone_call_id != self.id || !self.controller_started {
            return false;
        }
        self.actions
            .push(CallAction::ReceiveSignalingData(data.data));
        true
    }

    /// Emit encrypted signaling data produced by the media controller via `phone.sendSignalingData`.
    ///
    /// Port of `Call::sendSignalingData` (`calls_call.cpp:766-781`).
    pub fn emit_signaling_data(&mut self, data: Vec<u8>) {
        if self.is_conference_invite() {
            return;
        }
        self.actions.push(CallAction::SendSignalingData {
            peer: self.input_phone_call(),
            data,
        });
    }

    /// Process a state transition emitted by the media controller.
    ///
    /// Port of `Call::handleControllerStateChange` (`calls_call.cpp:1299-1329`).
    pub fn handle_controller_state_change(&mut self, state: ControllerState, now_ms: u64) {
        if self.is_conference_invite() {
            return;
        }
        match state {
            ControllerState::WaitInit => self.set_state(CallState::WaitingInit, now_ms),
            ControllerState::WaitInitAck => self.set_state(CallState::WaitingInitAck, now_ms),
            ControllerState::Established => self.set_state(CallState::Established, now_ms),
            ControllerState::Failed(error) => self.handle_controller_error(&error, now_ms),
        }
    }

    /// Process a signal bar count change from the media controller.
    ///
    /// Port of `Call::handleControllerBarCountChange` (`calls_call.cpp:1331-1337`).
    pub fn handle_controller_bar_count_change(&mut self, count: i32) {
        self.set_signal_bar_count(count);
    }

    /// Hang up the call (optionally supplying a conference migration slug).
    ///
    /// Port of `Call::hangup` (`calls_call.cpp:710-733`).
    pub fn hangup(
        &mut self,
        migrate_slug: Option<String>,
        now_ms: u64,
        migrating_conference_invite: bool,
    ) {
        if self.rating_in_panel {
            self.finish_rating();
            return;
        }
        if matches!(self.state, CallState::Busy | CallState::MigrationHangingUp) {
            self.actions.push(CallAction::NotifyFinished);
            return;
        }
        let missed = self.state == CallState::Ringing
            || (self.state == CallState::Waiting && self.call_type == CallType::Outgoing);
        let declined = self.is_incoming_waiting();
        let reason = if let Some(slug) = migrate_slug.filter(|s| !s.is_empty()) {
            PhoneCallDiscardReason::MigrateConferenceCall { slug }
        } else if missed {
            PhoneCallDiscardReason::Missed
        } else if declined {
            PhoneCallDiscardReason::Busy
        } else {
            PhoneCallDiscardReason::Hangup
        };
        self.finish(
            FinishType::Ended,
            reason,
            now_ms,
            migrating_conference_invite,
        );
    }

    /// Redial after [`CallState::Busy`].
    ///
    /// Port of `Call::redial` (`calls_call.cpp:735-747`).
    pub fn redial(&mut self) {
        if self.is_conference_invite() || self.state != CallState::Busy {
            return;
        }
        self.call_type = CallType::Outgoing;
        self.set_state(CallState::Requesting, 0);
        self.answer_after_dh_config_received = false;
        self.actions
            .push(CallAction::StartWaitingTrack { outgoing: true });
        self.actions.push(CallAction::NotifyRedial);
    }

    /// Handle an expired timer previously armed via [`CallAction::SetTimer`].
    pub fn on_timer(&mut self, kind: CallTimerKind, now_ms: u64) {
        match kind {
            CallTimerKind::DiscardByTimeout => {
                self.hangup(None, now_ms, false);
            }
            CallTimerKind::FinishByTimeout => {
                if let Some(final_state) = self.pending_finish_final_state.take() {
                    self.set_state(final_state, now_ms);
                }
            }
        }
    }

    /// Move a pending rating request into the call panel (`Call::takeRatingToPanel`).
    pub fn take_rating_to_panel(&mut self) {
        if self.rating_requested {
            self.rating_in_panel = true;
        }
    }

    /// Enter panel rating mode in response to [`CallAction::ShowRatingBox`].
    pub fn enter_panel_rating(&mut self) {
        if !self.id.is_zero() && self.access_hash != 0 {
            self.rating_in_panel = true;
            self.rating_requested = true;
        }
    }

    /// Select a star rating (`1..=5`) for panel rating mode (`Call::setRating`).
    pub fn set_rating(&mut self, rating: i32) {
        self.rating = rating;
    }

    /// Submit the panel rating and notify finished (`Call::finishRating`, `calls_call.cpp:653-673`).
    pub fn finish_rating(&mut self) {
        if !self.rating_in_panel {
            return;
        }
        self.rating_in_panel = false;
        self.rating_requested = false;
        if self.rating > 0 && !self.id.is_zero() && self.access_hash != 0 {
            self.actions.push(CallAction::SetCallRating {
                user_initiative: false,
                peer: self.input_phone_call(),
                rating: self.rating,
                comment: String::new(),
            });
        }
        self.actions.push(CallAction::NotifyFinished);
    }

    /// Submit a rating with an optional comment from the rating dialog (`Call::showRatingBox`).
    pub fn submit_rating(&mut self, rating: i32, comment: String) {
        self.rating_requested = false;
        self.rating = 0;
        if rating > 0 && !self.id.is_zero() && self.access_hash != 0 {
            self.actions.push(CallAction::SetCallRating {
                user_initiative: false,
                peer: self.input_phone_call(),
                rating,
                comment,
            });
        }
    }

    /// Handle a failed migration `phone.getGroupCall` request (`calls_call.cpp:974-976`).
    pub fn handle_migration_failed(&mut self, now_ms: u64) {
        self.set_state(CallState::Failed, now_ms);
    }

    fn finish_by_migration(&mut self, slug: String, now_ms: u64) {
        if self.is_conference_invite() || self.state == CallState::MigrationHangingUp {
            return;
        }
        self.set_state(CallState::MigrationHangingUp, now_ms);
        self.actions.push(CallAction::GetGroupCall {
            call: InputGroupCall::Slug { slug },
            limit: 5,
        });
    }

    fn confirm_accepted_call(&mut self, call: &PhoneCallAccepted, now_ms: u64) {
        // Guard against late/out-of-order `phoneCallAccepted` updates after hangup/finish:
        // in tdesktop (`calls/calls_call.cpp`, `Call::confirmAcceptedCall` / `Call::finish`),
        // a finishing/destroyed call must not re-enter `ExchangingKeys` or send `phone.confirmCall`.
        if self.call_type != CallType::Outgoing
            || self.is_conference_invite()
            || self.state == CallState::ExchangingKeys
            || self.state.is_finishing_or_finished()
            || self.controller_started
        {
            return;
        }
        match finish_dh_key_exchange(&self.dh_config, &call.g_b, &self.random_power) {
            Ok((auth_key, fingerprint)) => {
                self.auth_key = Some(auth_key);
                self.key_fingerprint = fingerprint;
            }
            Err(_) => {
                self.finish(
                    FinishType::Failed,
                    PhoneCallDiscardReason::Disconnect,
                    now_ms,
                    false,
                );
                return;
            }
        }
        self.set_state(CallState::ExchangingKeys, now_ms);
        self.actions.push(CallAction::ConfirmCall {
            peer: self.input_phone_call(),
            g_a: self.g_a.clone(),
            key_fingerprint: self.key_fingerprint,
            protocol: self.protocol_config.to_tl_protocol(),
        });
    }

    fn start_confirmed_call(&mut self, call: &PhoneCallActive, now_ms: u64) {
        if self.call_type != CallType::Incoming || self.is_conference_invite() {
            return;
        }
        if verify_ga_hash(&call.g_a_or_b, &self.g_a_hash).is_err() {
            self.finish(
                FinishType::Failed,
                PhoneCallDiscardReason::Disconnect,
                now_ms,
                false,
            );
            return;
        }
        self.g_a = call.g_a_or_b.clone();
        match finish_dh_key_exchange(&self.dh_config, &call.g_a_or_b, &self.random_power) {
            Ok((auth_key, fingerprint)) => {
                self.auth_key = Some(auth_key);
                self.key_fingerprint = fingerprint;
            }
            Err(_) => {
                self.finish(
                    FinishType::Failed,
                    PhoneCallDiscardReason::Disconnect,
                    now_ms,
                    false,
                );
                return;
            }
        }
        self.create_and_start_controller(call, now_ms);
    }

    fn create_and_start_controller(&mut self, call: &PhoneCallActive, now_ms: u64) {
        // Guard against late `phone.confirmCall` responses arriving after hangup/finish
        // (`calls/calls_call.cpp`, `Call::createAndStartController` / `Call::finish`).
        if self.is_conference_invite() || self.state.is_finishing_or_finished() {
            return;
        }
        self.actions
            .push(CallAction::CancelTimer(CallTimerKind::DiscardByTimeout));
        if !self.check_call_fields(call, now_ms) {
            return;
        }
        let Some(auth_key) = self.auth_key else {
            return;
        };

        self.conference_supported = call.conference_supported;
        let version = call
            .protocol
            .library_versions
            .first()
            .cloned()
            .unwrap_or_else(|| DEFAULT_VERSION.to_string());
        let (endpoints, rtc_servers) = collect_endpoints_and_servers(&call.connections);

        self.controller_started = true;
        self.actions.push(CallAction::StartController(Box::new(
            ControllerDescriptor {
                version,
                initialization_timeout_ms: self.server_config.call_connect_timeout_ms,
                receive_timeout_ms: self.server_config.call_packet_timeout_ms,
                enable_p2p: call.p2p_allowed,
                max_api_layer: call.protocol.max_layer,
                encryption_key: auth_key,
                is_outgoing: self.call_type == CallType::Outgoing,
                endpoints,
                rtc_servers,
                custom_parameters: call.custom_parameters.clone(),
            },
        )));
    }

    fn check_call_common_fields(
        &mut self,
        access_hash: i64,
        admin_id: UserId,
        participant_id: UserId,
        now_ms: u64,
    ) -> bool {
        if access_hash != self.access_hash {
            self.finish(
                FinishType::Failed,
                PhoneCallDiscardReason::Disconnect,
                now_ms,
                false,
            );
            return false;
        }
        let expected_admin = if self.call_type == CallType::Outgoing {
            self.self_user_id
        } else {
            self.peer_user_id
        };
        let expected_participant = if self.call_type == CallType::Outgoing {
            self.peer_user_id
        } else {
            self.self_user_id
        };
        if admin_id != expected_admin || participant_id != expected_participant {
            self.finish(
                FinishType::Failed,
                PhoneCallDiscardReason::Disconnect,
                now_ms,
                false,
            );
            return false;
        }
        true
    }

    fn check_call_fields(&mut self, call: &PhoneCallActive, now_ms: u64) -> bool {
        if !self.check_call_common_fields(
            call.access_hash,
            call.admin_id,
            call.participant_id,
            now_ms,
        ) {
            return false;
        }
        if call.key_fingerprint != self.key_fingerprint {
            self.finish(
                FinishType::Failed,
                PhoneCallDiscardReason::Disconnect,
                now_ms,
                false,
            );
            return false;
        }
        true
    }

    fn finish(
        &mut self,
        finish_type: FinishType,
        reason: PhoneCallDiscardReason,
        now_ms: u64,
        migrating_conference_invite: bool,
    ) {
        if finish_type == FinishType::None {
            return;
        }
        self.set_signal_bar_count(SIGNAL_BAR_FINISHED);

        let final_state = if finish_type == FinishType::Ended {
            CallState::Ended
        } else {
            CallState::Failed
        };
        let hangup_state = if finish_type == FinishType::Ended {
            CallState::HangingUp
        } else {
            CallState::FailedHangingUp
        };

        if self.state == CallState::Requesting {
            self.pending_finish_final_state = Some(final_state);
            self.actions.push(CallAction::SetTimer {
                kind: CallTimerKind::FinishByTimeout,
                duration_ms: HANGUP_TIMEOUT_MS,
            });
            self.finish_after_requesting_call = finish_type;
            return;
        }
        if matches!(
            self.state,
            CallState::HangingUp
                | CallState::FailedHangingUp
                | CallState::EndedByOtherDevice
                | CallState::Ended
                | CallState::Failed
        ) {
            return;
        }
        if self.is_conference_invite() {
            if migrating_conference_invite {
                self.actions.push(CallAction::NotifyFinished);
            } else {
                self.actions
                    .push(CallAction::DeclineIncomingConferenceInvites {
                        conference_id: self.conference_id,
                    });
                self.set_state(final_state, now_ms);
            }
            return;
        }
        if self.id.is_zero() {
            self.set_state(final_state, now_ms);
            return;
        }

        self.set_state(hangup_state, now_ms);
        let duration = (self.duration_ms(now_ms) / 1000) as i32;
        self.pending_finish_final_state = Some(final_state);
        self.actions.push(CallAction::SetTimer {
            kind: CallTimerKind::FinishByTimeout,
            duration_ms: HANGUP_TIMEOUT_MS,
        });

        let video = self.remote_video_state != VideoState::Inactive
            || self.outgoing_video_state != VideoState::Inactive;
        self.actions.push(CallAction::DiscardCall {
            video,
            peer: self.input_phone_call(),
            duration,
            reason,
            connection_id: self.preferred_relay_id,
        });
    }

    fn set_state(&mut self, state: CallState, now_ms: u64) {
        let was = self.state;
        if matches!(
            was,
            CallState::Failed | CallState::Ended | CallState::EndedByOtherDevice
        ) {
            return;
        }
        if was == CallState::FailedHangingUp && state != CallState::Failed {
            return;
        }
        // Once hanging up, only terminal call states may supersede `HangingUp`
        // (`calls/calls_call.cpp`, `Call::setState` / `Call::finish`).
        if was == CallState::HangingUp
            && !matches!(
                state,
                CallState::Ended
                    | CallState::EndedByOtherDevice
                    | CallState::Failed
                    | CallState::Busy
            )
        {
            return;
        }
        if was == CallState::MigrationHangingUp
            && state != CallState::Ended
            && state != CallState::Failed
        {
            return;
        }
        if was == state {
            return;
        }

        self.state = state;
        self.actions.push(CallAction::StateChanged {
            from: was,
            to: state,
        });

        if !matches!(
            state,
            CallState::Starting
                | CallState::Requesting
                | CallState::Waiting
                | CallState::WaitingIncoming
                | CallState::Ringing
        ) {
            self.actions.push(CallAction::StopWaitingTrack);
        }

        if state.is_finishing_or_finished() {
            self.actions
                .push(CallAction::CancelTimer(CallTimerKind::DiscardByTimeout));
        }

        if matches!(
            state,
            CallState::Ended | CallState::EndedByOtherDevice | CallState::Failed
        ) {
            self.pending_finish_final_state = None;
            self.actions
                .push(CallAction::CancelTimer(CallTimerKind::FinishByTimeout));
        }

        if matches!(
            state,
            CallState::Ended | CallState::EndedByOtherDevice | CallState::Failed | CallState::Busy
        ) {
            self.destroy_controller();
        }

        match state {
            CallState::Established => {
                self.start_time_ms = Some(now_ms);
            }
            CallState::ExchangingKeys => {
                self.actions
                    .push(CallAction::PlaySound(CallSound::Connecting));
            }
            CallState::Ended => {
                if was != CallState::WaitingUserConfirmation {
                    self.actions.push(CallAction::PlaySound(CallSound::Ended));
                }
                self.actions.push(CallAction::NotifyFinished);
            }
            CallState::EndedByOtherDevice => {
                self.actions.push(CallAction::NotifyFinished);
            }
            CallState::Failed => {
                self.actions.push(CallAction::PlaySound(CallSound::Ended));
                self.actions.push(CallAction::NotifyFailed);
            }
            CallState::Busy => {
                self.actions.push(CallAction::PlaySound(CallSound::Busy));
            }
            _ => {}
        }
    }

    fn destroy_controller(&mut self) {
        if self.controller_started {
            self.controller_started = false;
            self.actions.push(CallAction::DestroyController);
        }
        self.set_signal_bar_count(SIGNAL_BAR_FINISHED);
    }

    fn set_signal_bar_count(&mut self, count: i32) {
        if self.signal_bars.update(count) {
            self.actions.push(CallAction::SignalBarCountChanged(count));
        }
    }

    fn handle_request_error(&mut self, error: &str, now_ms: u64) {
        if !error.is_empty() {
            self.actions.push(CallAction::ShowError(error.to_string()));
        }
        self.finish(
            FinishType::Failed,
            PhoneCallDiscardReason::Disconnect,
            now_ms,
            false,
        );
    }

    fn handle_controller_error(&mut self, error: &str, now_ms: u64) {
        if matches!(error, "ERROR_INCOMPATIBLE" | "ERROR_AUDIO_IO") {
            self.actions.push(CallAction::ShowError(error.to_string()));
        }
        self.finish(
            FinishType::Failed,
            PhoneCallDiscardReason::Disconnect,
            now_ms,
            false,
        );
    }

    const fn input_phone_call(&self) -> InputPhoneCall {
        InputPhoneCall {
            id: self.id,
            access_hash: self.access_hash,
        }
    }

    /// Initial signal bar sentinel value (`Call::kSignalBarStarting`).
    pub const SIGNAL_BAR_STARTING: i32 = SIGNAL_BAR_STARTING;

    /// Finished signal bar sentinel value (`Call::kSignalBarFinished`).
    pub const SIGNAL_BAR_FINISHED: i32 = SIGNAL_BAR_FINISHED;
}
