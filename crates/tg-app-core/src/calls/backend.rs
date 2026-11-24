//! Decoupled `CallsBackend` trait and UI view snapshots bridging `tg-calls` and `tg-voip`.
//!
//! Port origin: `Telegram/SourceFiles/calls/calls_instance.h`, `calls_call.h`,
//! `calls/group/calls_group_call.h`, `calls_box_controller.h`, and `calls_choose_join_as.h`.

use egui::Color32;
use tg_calls::{
    CallSound, CallState, CallType, GroupCallParticipant, GroupCallSound, GroupCallState,
    InstanceState, MuteState, RemoteAudioState, RemoteBatteryState, SignalBarsState, VideoEndpoint,
    VideoState,
};
use tg_voip::{CameraDeviceInfo, VoipStatsSnapshot};

use crate::core_types::screen_picker::{ScreenShareSource, enumerate_screen_sources};
use crate::stand_in::types::{PeerId, PeerKind};

/// RTMP livestream server URL and stream key returned by `phone.getGroupCallStreamRtmpUrl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtmpStreamInfo {
    /// RTMP / RTMPS ingest server URL (e.g. `rtmps://dc4-1.rtmp.t.me/s/`).
    pub url: String,
    /// Secret stream key for OBS / external broadcasting software.
    pub key: String,
}

/// Converts an app `PeerId` into a `tg_calls` protocol `PeerId`.
#[must_use]
pub const fn to_calls_peer_id(peer: PeerId) -> tg_calls::types::PeerId {
    match peer.kind() {
        PeerKind::User => tg_calls::types::PeerId::from_user(tg_calls::types::UserId(peer.bare())),
        PeerKind::Group => tg_calls::types::PeerId::from_chat(tg_calls::types::ChatId(peer.bare())),
        PeerKind::Channel => {
            tg_calls::types::PeerId::from_channel(tg_calls::types::ChannelId(peer.bare()))
        }
    }
}

/// Converts a `tg_calls` protocol `PeerId` back into an app `PeerId`.
#[must_use]
pub const fn from_calls_peer_id(peer: tg_calls::types::PeerId) -> PeerId {
    if peer.is_channel() {
        PeerId::from_channel(peer.bare())
    } else if peer.is_chat() {
        PeerId::from_chat(peer.bare())
    } else {
        PeerId::from_user(peer.bare())
    }
}

/// Peer display metadata for 1:1 calls, group call participants, recent calls, and join-as rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPeerInfo {
    /// Unique peer identifier.
    pub id: PeerId,
    /// Full display name.
    pub name: String,
    /// Short first name (used in compact top bar & mute tooltips).
    pub short_name: String,
    /// 1-2 character avatar monogram.
    pub initials: String,
    /// Avatar background accent color.
    pub avatar_color: Color32,
    /// Optional bio / status line shown in participant details.
    pub about: String,
    /// Whether this peer is a group/channel.
    pub is_channel: bool,
    /// Whether this peer is a broadcast channel (livestream).
    pub is_broadcast: bool,
    /// Member or subscriber count for channels/groups.
    pub members_count: u32,
}

/// Audio/video device option shown in device menus and group call settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceOption {
    /// Stable device ID (`"default"` for system default).
    pub id: String,
    /// Human-readable device name.
    pub name: String,
}

/// Persisted/active audio, video, and Push-to-Talk settings for calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallAudioSettings {
    /// Selected speaker / playback device ID.
    pub output_device_id: String,
    /// Selected microphone / capture device ID.
    pub input_device_id: String,
    /// Selected webcam device ID.
    pub camera_device_id: String,
    /// WebRTC noise suppression enabled (`tg-voip::VoiceDspConfig::noise_suppression`).
    pub noise_suppression: bool,
    /// Push-to-Talk mode enabled in group calls.
    pub push_to_talk_enabled: bool,
    /// Serialized / displayable Push-to-Talk shortcut (e.g. `"Ctrl + Space"`).
    pub push_to_talk_shortcut: String,
    /// Push-to-Talk release hold delay in milliseconds (`0..=2000`, step 10).
    pub push_to_talk_delay_ms: u64,
}

impl Default for CallAudioSettings {
    fn default() -> Self {
        Self {
            output_device_id: tg_audio::DEFAULT_DEVICE_ID.to_owned(),
            input_device_id: tg_audio::DEFAULT_DEVICE_ID.to_owned(),
            camera_device_id: "cam-default".to_owned(),
            noise_suppression: true,
            push_to_talk_enabled: false,
            push_to_talk_shortcut: "Space".to_owned(),
            push_to_talk_delay_ms: 250,
        }
    }
}

/// Snapshot of the active 1:1 call driven by `tg_calls::Call` and `tg_voip::CallMediaEngine`.
#[derive(Debug, Clone)]
pub struct ActiveCallSnapshot {
    /// Remote peer metadata.
    pub peer: CallPeerInfo,
    /// Incoming or outgoing direction.
    pub call_type: CallType,
    /// Current 1:1 call state machine state (`tg_calls::CallState`).
    pub state: CallState,
    /// Elapsed call duration in milliseconds once established.
    pub duration_ms: u64,
    /// Final duration in seconds preserved for the post-call rating screen.
    pub ended_duration_secs: Option<i64>,
    /// Local microphone mute state.
    pub muted: bool,
    /// Local camera video state.
    pub camera_state: VideoState,
    /// Local screen-sharing state.
    pub screen_share_active: bool,
    /// Remote microphone state.
    pub remote_audio_state: RemoteAudioState,
    /// Remote video state.
    pub remote_video_state: VideoState,
    /// Remote battery status.
    pub remote_battery_state: RemoteBatteryState,
    /// Signal quality bars (`tg_calls::SignalBarsState`).
    pub signal_bars: SignalBarsState,
    /// 4-emoji verification fingerprint once DH exchange completes (`tg_calls::compute_emoji_fingerprint`).
    pub emoji_fingerprint: Option<[String; 4]>,
    /// Whether this call is an incoming conference invite.
    pub is_conference_invite: bool,
    /// Whether conference migration ("Add People") is supported.
    pub conference_supported: bool,
    /// Whether the post-call rating card should be displayed in the panel.
    pub rating_requested: bool,
    /// Selected star rating (`0` = none yet, `1..=5` = submitted).
    pub submitted_rating: Option<u8>,
    /// Live VoIP telemetry snapshot from `tg-voip`.
    pub voip_stats: VoipStatsSnapshot,
    /// Current synthetic/live remote audio level (`0.0..=1.0`).
    pub remote_audio_level: f32,
}

/// Display entry for a single participant inside an active group call.
#[derive(Debug, Clone)]
pub struct GroupMemberView {
    /// Participant peer info.
    pub peer: CallPeerInfo,
    /// Underlying `tg_calls::GroupCallParticipant` protocol model.
    pub participant: GroupCallParticipant,
    /// Whether this participant is the local user (`join_as`).
    pub is_self: bool,
    /// Live normalized audio level (`0.0..=1.0`) from `LevelUpdate`.
    pub audio_level: f32,
    /// Whether the participant is currently invited (not yet joined).
    pub is_invited: bool,
    /// Whether the participant is actively being rung.
    pub is_calling: bool,
    /// Optional simulated camera frame dimensions `(w, h)`.
    pub camera_frame_size: Option<(i32, i32)>,
    /// Optional simulated screen-share frame dimensions `(w, h)`.
    pub screen_frame_size: Option<(i32, i32)>,
}

/// Display entry for a single visible in-call group chat message (`Calls::Group::Message`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCallMessageView {
    /// Server message ID (or negative local placeholder ID while sending).
    pub id: i32,
    /// Unix send timestamp in seconds (0 while sending).
    pub date: i32,
    /// Author peer metadata.
    pub peer: CallPeerInfo,
    /// Message text (empty for star-only paid reactions).
    pub text: String,
    /// Paid Stars attached to this message (0 for free messages).
    pub stars: i64,
    /// `true` if sending failed.
    pub failed: bool,
    /// `true` if sent by a group/channel admin.
    pub admin: bool,
    /// `true` if sent by the local user.
    pub mine: bool,
    /// `true` while a paid message is currently pinned at the top.
    pub pinned: bool,
}

/// Display entry for a top paid Star donor (`Calls::Group::StarsDonor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupStarsDonorView {
    /// Donor peer metadata (`None` for anonymous donors).
    pub peer: Option<CallPeerInfo>,
    /// Total Stars donated by this peer.
    pub stars: i64,
    /// `true` if this entry represents the local user.
    pub is_my: bool,
}

/// Snapshot of an active group call / voice chat driven by `tg_calls::GroupCallSession`.
#[derive(Debug, Clone)]
pub struct GroupCallSnapshot {
    /// Owning group/channel peer.
    pub group_peer: CallPeerInfo,
    /// Custom voice chat title (falls back to `group_peer.name` if empty).
    pub title: String,
    /// Currently selected `join_as` peer ID.
    pub join_as: PeerId,
    /// Current lifecycle state (`tg_calls::GroupCallState`).
    pub state: GroupCallState,
    /// Current media instance connection state (`tg_calls::InstanceState`).
    pub instance_state: InstanceState,
    /// Local mute / raise-hand / PTT state (`tg_calls::MuteState`).
    pub mute_state: MuteState,
    /// Whether the local user has admin rights in this group call.
    pub can_manage: bool,
    /// Whether new participants join muted by default.
    pub join_muted: bool,
    /// Whether group call chat messages are enabled.
    pub messages_enabled: bool,
    /// Whether the call is currently being recorded.
    pub recording: bool,
    /// Local camera active.
    pub camera_active: bool,
    /// Local screen share active.
    pub screen_share_active: bool,
    /// Local microphone RMS level (`0.0..=1.0`).
    pub local_audio_level: f32,
    /// Ordered participants list.
    pub members: Vec<GroupMemberView>,
    /// Currently spotlighted/large video endpoint (if any).
    pub large_video: Option<VideoEndpoint>,
    /// Whether `large_video` is pinned by the user.
    pub large_video_pinned: bool,
    /// Shareable speaker invite link.
    pub invite_link_speaker: String,
    /// Shareable listener invite link.
    pub invite_link_listener: String,
    /// Whether this call is an end-to-end encrypted (`tde2e`) conference call.
    pub is_conference: bool,
    /// 4-emoji conference E2E verification fingerprint once commit-reveal completes.
    pub emoji_fingerprint: Option<[String; 4]>,
    /// Number of participants currently registered in the E2E conference blockchain.
    pub e2e_verified_count: usize,
    /// Currently visible in-call messages (plain or E2E decrypted).
    pub chat_messages: Vec<GroupCallMessageView>,
    /// Ranked paid Star donors leaderboard.
    pub stars_top: Vec<GroupStarsDonorView>,
    /// Total paid Stars sent to this call/stream (server total + locally scheduled).
    pub stars_total: i64,
    /// Local user's total paid Stars sent (server + locally scheduled).
    pub stars_my_total: i64,
    /// Locally queued unsent paid Star reactions awaiting batch flush.
    pub stars_scheduled: i64,
    /// Minimum paid Stars required to post a highlighted message (0 if free).
    pub min_paid_stars: i64,
    /// Whether this group call is a video livestream / broadcast.
    pub is_video_stream: bool,
}

impl GroupCallSnapshot {
    /// Display title (custom call title if non-empty, otherwise chat name).
    #[must_use]
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            &self.group_peer.name
        } else {
            &self.title
        }
    }

    /// Count of joined participants (excluding not-yet-joined invitees).
    #[must_use]
    pub fn joined_count(&self) -> usize {
        self.members.iter().filter(|m| !m.is_invited).count()
    }
}

/// Direction/outcome of a historical call in the Recent Calls list (`calls_box_controller.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecentCallDirection {
    /// Outgoing call placed by local user.
    Out,
    /// Answered incoming call.
    In,
    /// Missed or busy incoming call.
    Missed,
}

/// Single call history item (`calls_box_controller.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentCallEntry {
    /// Message ID of the call service message.
    pub msg_id: i32,
    /// Remote peer info.
    pub peer: CallPeerInfo,
    /// Outgoing, incoming, or missed.
    pub direction: RecentCallDirection,
    /// Whether it was a video call.
    pub is_video: bool,
    /// Days ago (`0` = today, `1` = yesterday, etc.) for grouping & display.
    pub days_ago: u32,
    /// Local hour (`0..=23`).
    pub hour: u8,
    /// Local minute (`0..=59`).
    pub minute: u8,
    /// Duration in seconds (`None` for missed/cancelled).
    pub duration_secs: Option<i64>,
}

/// Candidate identity peer for `ChooseJoinAsBox` (`calls_choose_join_as.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinAsCandidate {
    /// Peer metadata.
    pub peer: CallPeerInfo,
    /// True for the local user's personal account.
    pub is_personal: bool,
    /// Secondary caption ("Personal account", "42 members", "1,240 subscribers").
    pub subtitle: String,
}

/// Sound event emitted by [`CallsBackend`] for [`crate::calls::audio::CallRingtonePlayer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendSoundEvent {
    /// Start looping incoming or outgoing waiting tone.
    StartWaitingTrack {
        /// `true` for outgoing ringback, `false` for incoming ringtone.
        outgoing: bool,
    },
    /// Stop active waiting tone loop.
    StopWaitingTrack,
    /// Play a 1:1 call sound cue.
    CallSound(CallSound),
    /// Play a group call sound cue.
    GroupCallSound(GroupCallSound),
}

/// Backend contract decoupling the Calls UI from MTProto / network transport.
///
/// Implemented by [`crate::calls::mock::MockCallsBackend`] for offline demoing and tests,
/// and later by a live `tg-api` + `tg-calls` + `tg-voip` integration.
pub trait CallsBackend {
    /// Advances timers, simulated progressions, and audio levels to `now_ms`.
    fn tick(&mut self, now_ms: u64);

    /// Returns the currently active (or post-call rating) 1:1 call snapshot, if any.
    fn active_call(&self) -> Option<&ActiveCallSnapshot>;

    /// Starts an outgoing 1:1 voice or video call to `peer_id`.
    fn start_outgoing_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64);

    /// Confirms an outgoing call when in `WaitingUserConfirmation`.
    fn confirm_outgoing_call(&mut self, video: bool, now_ms: u64);

    /// Triggers an incoming 1:1 call from `peer_id` (useful for demos and tests).
    fn simulate_incoming_call(&mut self, peer_id: PeerId, video: bool, now_ms: u64);

    /// Answers a ringing incoming 1:1 call.
    fn answer_call(&mut self, now_ms: u64);

    /// Declines an incoming call or hangs up an active/outgoing call.
    fn hangup_call(&mut self, now_ms: u64);

    /// Redials a call that ended in `CallState::Busy`.
    fn redial_call(&mut self, now_ms: u64);

    /// Sets local microphone mute on the active 1:1 call.
    fn set_call_muted(&mut self, muted: bool);

    /// Toggles local camera video on the active 1:1 call.
    fn toggle_call_camera(&mut self);

    /// Toggles screen sharing on the active 1:1 call.
    fn toggle_call_screen_share(&mut self);

    /// Submits a 1..=5 star call quality rating and optional comment (`phone.setCallRating`).
    fn submit_call_rating(&mut self, rating: u8, comment: String);

    /// Dismisses the post-call rating card and clears the ended 1:1 call.
    fn finish_call_rating(&mut self);

    /// Returns the active group call snapshot, if any.
    fn active_group_call(&self) -> Option<&GroupCallSnapshot>;

    /// Joins or starts a group call in `group_peer_id` appearing as `join_as`.
    fn join_group_call(&mut self, group_peer_id: PeerId, join_as: PeerId, now_ms: u64);

    /// Switches `join_as` identity inside an active group call.
    fn switch_group_join_as(&mut self, join_as: PeerId, now_ms: u64);

    /// Leaves (or discards, if `discard && can_manage`) the active group call.
    fn leave_group_call(&mut self, discard: bool, now_ms: u64);

    /// Updates local mute / raise-hand state in the active group call.
    fn set_group_mute_state(&mut self, state: MuteState, now_ms: u64);

    /// Engages or releases Push-to-Talk in the active group call.
    fn set_group_push_to_talk(&mut self, pressed: bool, now_ms: u64);

    /// Toggles local camera sharing in the active group call.
    fn toggle_group_camera(&mut self, now_ms: u64);

    /// Toggles local screen sharing in the active group call.
    fn toggle_group_screen_share(&mut self, now_ms: u64);

    /// Adjusts another participant's playback volume (`1..=20000`, `10000` = 100%).
    fn set_participant_volume(&mut self, peer_id: PeerId, volume: i32);

    /// Mutes or unmutes a participant (admin mute or local mute).
    fn set_participant_muted(&mut self, peer_id: PeerId, muted: bool);

    /// Allows a participant with a raised hand to speak (admin action).
    fn allow_participant_to_speak(&mut self, peer_id: PeerId);

    /// Sets or clears the spotlight/large video endpoint in the group call viewport.
    fn select_large_video(&mut self, endpoint: Option<VideoEndpoint>, now_ms: u64);

    /// Toggles pinning of the currently spotlighted video tile.
    fn toggle_pin_large_video(&mut self);

    /// Toggles "new participants join muted" (admin setting).
    fn set_group_join_muted(&mut self, join_muted: bool);

    /// Toggles group call chat messages (admin setting).
    fn set_group_messages_enabled(&mut self, enabled: bool);

    /// Starts or stops group call recording (`phone.toggleGroupCallRecord`).
    fn toggle_group_recording(
        &mut self,
        _start: bool,
        _title: Option<String>,
        _video: bool,
        _video_portrait: bool,
    ) {
    }

    /// Fetches or revokes the RTMP livestream server URL and stream key (`phone.getGroupCallStreamRtmpUrl`).
    fn request_group_rtmp_url(
        &mut self,
        _peer_id: PeerId,
        _revoke: bool,
    ) -> Result<RtmpStreamInfo, String> {
        Err("RTMP stream keys are only available in live sessions.".to_owned())
    }

    /// Enumerates shareable monitors and windows for the screen-sharing source picker.
    fn screen_share_sources(&self) -> Vec<ScreenShareSource> {
        enumerate_screen_sources()
    }

    /// Starts 1:1 screen sharing using a specific monitor or window chosen from the picker.
    fn start_call_screen_share_with_source(&mut self, _source: &ScreenShareSource) {
        self.toggle_call_screen_share();
    }

    /// Starts group call screen sharing using a specific monitor or window chosen from the picker.
    fn start_group_screen_share_with_source(&mut self, _source: &ScreenShareSource, now_ms: u64) {
        self.toggle_group_screen_share(now_ms);
    }

    /// Creates and joins a new E2E encrypted conference call (`phone.createConferenceCall`).
    fn create_conference_call(&mut self, _now_ms: u64) {}

    /// Joins an existing E2E encrypted conference call by invite slug (`tg://call?slug=...`).
    fn join_conference_call_by_slug(&mut self, _slug: String, _now_ms: u64) {}

    /// Removes or kicks participants from an active E2E conference call
    /// (`phone.deleteConferenceCallParticipants`).
    fn remove_conference_participants(&mut self, _user_ids: &[u64], _kick: bool, _now_ms: u64) {}

    /// Sends an in-call chat message (plain `phone.sendGroupCallMessage` or E2E encrypted
    /// `phone.sendGroupCallEncryptedMessage` in conferences), optionally attaching paid Stars.
    fn send_group_call_message(&mut self, _text: String, _paid_stars: i64, _now_ms: u64) {}

    /// Deletes one in-call message (or all messages from a participant) via
    /// `phone.deleteGroupCallMessages` / `phone.deleteGroupCallParticipantMessages`.
    fn delete_group_call_message(
        &mut self,
        _msg_id: i32,
        _delete_all_from: Option<PeerId>,
        _report_spam: bool,
    ) {
    }

    /// Queues `count` paid Stars into the local reaction batch.
    fn add_group_paid_stars_reaction(&mut self, _count: i64) {}

    /// Flushes locally queued paid Star reactions via `phone.sendGroupCallMessage`.
    fn flush_group_paid_stars_reaction(&mut self, _now_ms: u64) {}

    /// Cancels locally queued unsent paid Star reactions.
    fn cancel_group_paid_stars_reaction(&mut self) {}

    /// Refreshes the paid Stars leaderboard (`phone.getGroupCallStars`).
    fn refresh_group_stars(&mut self) {}

    /// Refreshes recent call history from the server (`messages.search(InputMessagesFilterPhoneCalls)`).
    fn refresh_recent_calls(&mut self) {}

    /// Exports a shareable speaker (`can_self_unmute = true`) or listener (`false`) invite link
    /// for the active group call (`phone.exportGroupCallInvite`, `calls_group_settings.cpp:939-974`).
    fn export_group_call_invite(&mut self, _can_self_unmute: bool) -> Result<String, String> {
        Err("Group call invite export is only available in live sessions.".to_owned())
    }

    /// Invites up to N users (batched in slices of 10 per `kMaxInvitePerSlice`) to the active
    /// group call (`phone.inviteToGroupCall`, `calls_group_call.cpp:4086-4155`), returning the
    /// successfully invited peer IDs.
    fn invite_to_group_call(&mut self, _user_ids: &[PeerId]) -> Result<Vec<PeerId>, String> {
        Err("Group call invites are only available in live sessions.".to_owned())
    }

    /// Saves the user's default `join_as` identity for `group_peer_id` on the server
    /// (`phone.saveDefaultGroupCallJoinAs`, `calls_group_call.cpp:1586-1592`).
    fn save_default_group_call_join_as(
        &mut self,
        _group_peer_id: PeerId,
        _join_as: PeerId,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// Starts a scheduled group call immediately (`phone.startScheduledGroupCall`,
    /// `calls_group_call.cpp:2262-2271`).
    fn start_scheduled_group_call(&mut self) -> Result<(), String> {
        Err("Starting scheduled group calls is only available in live sessions.".to_owned())
    }

    /// Subscribes or unsubscribes from start notifications for the active scheduled group call
    /// (`phone.toggleGroupCallStartSubscription`, `calls_group_call.cpp:2273-2283`).
    fn toggle_group_call_start_subscription(&mut self, _subscribed: bool) -> Result<(), String> {
        Err("Scheduled group call subscriptions are only available in live sessions.".to_owned())
    }

    /// Clears the entire phone call history, optionally revoking for both sides
    /// (`messages.deletePhoneCallHistory`, `calls_box_controller.cpp:756-785`), returning the
    /// total number of deleted call messages.
    fn delete_phone_call_history(&mut self, _revoke: bool) -> Result<usize, String> {
        Ok(0)
    }

    /// Returns the scheduled start Unix timestamp of the active group call, if scheduled.
    fn active_group_call_schedule_date(&self) -> Option<i32> {
        None
    }

    /// Returns whether the local user is subscribed to the active scheduled group call's start.
    fn active_group_call_schedule_subscribed(&self) -> bool {
        false
    }

    /// Returns candidate peers that can be invited to the active group call
    /// (`calls_group_invite_controller.cpp`).
    fn group_invite_candidates(&self) -> Vec<CallPeerInfo> {
        Vec::new()
    }

    /// Updates WebRTC noise suppression.
    fn set_noise_suppression(&mut self, enabled: bool);

    /// Updates selected audio input/output devices.
    fn set_audio_devices(&mut self, input_id: String, output_id: String);

    /// Updates selected camera device.
    fn set_camera_device(&mut self, camera_id: String);

    /// Updates Push-to-Talk settings (enabled, shortcut label, release delay ms).
    fn set_push_to_talk_settings(&mut self, enabled: bool, shortcut: String, delay_ms: u64);

    /// Returns current audio/video/PTT settings.
    fn audio_settings(&self) -> &CallAudioSettings;

    /// Returns available microphone capture devices.
    fn input_devices(&self) -> &[AudioDeviceOption];

    /// Returns available speaker playback devices.
    fn output_devices(&self) -> &[AudioDeviceOption];

    /// Returns available camera devices.
    fn camera_devices(&self) -> &[CameraDeviceInfo];

    /// Returns recent call history items (un-grouped, newest first).
    fn recent_calls(&self) -> &[RecentCallEntry];

    /// Deletes recent call items by their message IDs.
    fn delete_recent_calls(&mut self, msg_ids: &[i32]);

    /// Returns active group calls available to join from the Recent Calls box.
    fn active_chat_group_calls(&self) -> &[CallPeerInfo];

    /// Returns selectable `join_as` identity candidates for a group/channel.
    fn join_as_candidates(&self, group_peer_id: PeerId) -> Vec<JoinAsCandidate>;

    /// Drains queued sound events emitted since the last frame.
    fn drain_sound_events(&mut self) -> Vec<BackendSoundEvent>;

    /// Drains queued toast messages emitted since the last frame.
    fn drain_toasts(&mut self) -> Vec<String>;
}
