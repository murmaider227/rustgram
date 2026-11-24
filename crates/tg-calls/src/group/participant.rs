//! Group call participants list model, versioned update queue, and speaking state.
//!
//! Port of `data/data_group_call.h` and `data/data_group_call.cpp`
//! (`GroupCallParticipant`, `LastSpokeTimes`, `enqueueUpdate`, `processQueuedUpdates`,
//! `applyParticipantsSlice`, `applyLastSpoke`).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::group::video::ParticipantVideoParams;
use crate::group::{DEFAULT_VOLUME, SOUND_STATUS_KEPT_FOR_MS};
use crate::types::{
    CallId, GroupCallInfo, GroupCallObject, GroupCallParticipantData, PeerId, UnixTime,
    UpdateGroupCallParticipants,
};

/// Timestamps (in monotonic milliseconds) of a participant's most recent audio activity.
///
/// Port of `Data::LastSpokeTimes` (`data_group_call.h:34-37`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LastSpokeTimes {
    /// Last timestamp where level exceeded [`crate::group::SPEAK_LEVEL_THRESHOLD`].
    pub anything_ms: u64,
    /// Last timestamp where voice activity detection (`voice == true`) fired.
    pub voice_ms: u64,
}

/// Group call participant model (`Data::GroupCallParticipant` in `data_group_call.h:39-60`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCallParticipant {
    /// Participant peer ID.
    pub peer_id: PeerId,
    /// Camera and screen-share video stream parameters, if any.
    pub video_params: Option<ParticipantVideoParams>,
    /// Join timestamp (Unix seconds).
    pub date: UnixTime,
    /// Last active speaking timestamp (Unix seconds).
    pub last_active: UnixTime,
    /// Non-zero monotonically increasing rating while hand is raised.
    pub raised_hand_rating: u64,
    /// Primary audio SSRC.
    pub ssrc: u32,
    /// Volume in `1..=20000` (10000 = 100%).
    pub volume: i32,
    /// True if primary SSRC produced sound above threshold within the last 1500 ms.
    pub sounding: bool,
    /// True if primary SSRC produced voice within the last 1500 ms.
    pub speaking: bool,
    /// True if additional screen-audio SSRC produced sound within the last 1500 ms.
    pub additional_sounding: bool,
    /// True if additional screen-audio SSRC produced voice within the last 1500 ms.
    pub additional_speaking: bool,
    /// Whether the participant is muted in the call.
    pub muted: bool,
    /// Whether the local user locally muted this participant.
    pub muted_by_me: bool,
    /// Whether the participant is permitted to unmute themselves.
    pub can_self_unmute: bool,
    /// Whether only `min` updates have been observed for this participant so far.
    pub only_min_loaded: bool,
    /// Whether video is joined for this participant.
    pub video_joined: bool,
    /// Whether subsequent `min` updates should overwrite `volume`.
    pub apply_volume_from_min: bool,
}

impl GroupCallParticipant {
    /// Camera endpoint ID (empty string if none).
    pub fn camera_endpoint(&self) -> &str {
        self.video_params
            .as_ref()
            .map_or("", ParticipantVideoParams::camera_endpoint)
    }

    /// Screen-share endpoint ID (empty string if none).
    pub fn screen_endpoint(&self) -> &str {
        self.video_params
            .as_ref()
            .map_or("", ParticipantVideoParams::screen_endpoint)
    }

    /// Additional screen-share audio SSRC (0 if none).
    pub fn additional_audio_ssrc(&self) -> u32 {
        self.video_params
            .as_ref()
            .map_or(0, ParticipantVideoParams::additional_audio_ssrc)
    }

    /// Whether the camera stream is paused.
    pub fn is_camera_paused(&self) -> bool {
        self.video_params.as_ref().is_some_and(|v| v.camera.paused)
    }

    /// Whether the screen-share stream is paused.
    pub fn is_screen_paused(&self) -> bool {
        self.video_params.as_ref().is_some_and(|v| v.screen.paused)
    }
}

/// Before/after diff emitted whenever a participant joins, changes, or leaves.
///
/// Port of `Data::GroupCall::ParticipantUpdate` (`data_group_call.h:135-138`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantUpdate {
    /// Previous participant state (`None` if just added).
    pub was: Option<GroupCallParticipant>,
    /// New participant state (`None` if left/removed).
    pub now: Option<GroupCallParticipant>,
}

/// Origin of a participant slice (`Data::GroupCall::ApplySliceSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplySliceSource {
    /// Full call reload (`phone.getGroupCall`).
    FullReloaded,
    /// Paginated slice (`phone.getGroupParticipants`).
    SliceLoaded,
    /// Unknown SSRC/peer resolution (`phone.getGroupParticipants` by ssrc/id).
    UnknownLoaded,
    /// Server push (`updateGroupCallParticipants`).
    UpdateReceived,
    /// Optimistic local update constructed by client (`applyMeInCallLocally` / `applyParticipantLocally`).
    UpdateConstructed,
}

/// Queued versioned update type (`Data::GroupCall::QueuedType` in `data_group_call.h:231-235`).
///
/// Variant declaration order matches tdesktop's sorting precedence within the same version:
/// `VersionedParticipant < Participant < Call`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum QueuedType {
    VersionedParticipant = 0,
    Participant = 1,
    Call = 2,
}

/// Versioned group call update item that can be enqueued for ordered delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionedGroupCallUpdate {
    /// `updateGroupCall` (`groupCall` or `groupCallDiscarded`).
    Call(GroupCallObject),
    /// `updateGroupCallParticipants`.
    Participants(UpdateGroupCallParticipants),
}

/// Outcomes produced by [`ParticipantsList`] when applying updates or speaking events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParticipantsListEvents {
    /// Ordered participant diffs emitted (suppressed for `FullReloaded`).
    pub participant_updates: Vec<ParticipantUpdate>,
    /// Peer IDs of participants currently detected as speaking (`_participantSpeaking`).
    pub speaking_peers: Vec<PeerId>,
    /// Unknown SSRCs that need resolution via `phone.getGroupParticipants`.
    pub unknown_ssrcs_to_resolve: Vec<u32>,
    /// Versioned updates that were applied in order and should be forwarded to the active session.
    pub applied_updates: Vec<VersionedGroupCallUpdate>,
    /// Set to `true` if out-of-order updates remain queued (caller should arm 3s reload timer).
    pub has_pending_queued_updates: bool,
    /// Set to `true` if `groupCallDiscarded` was observed.
    pub discarded: bool,
}

/// Maintains the synchronized group call metadata, participant list, SSRC index, and
/// versioned update queue.
///
/// Port of `Data::GroupCall` (`data/data_group_call.cpp`).
#[derive(Debug, Clone)]
pub struct ParticipantsList {
    call_id: CallId,
    version: i32,
    participants: Vec<GroupCallParticipant>,
    peer_by_audio_ssrc: HashMap<u32, PeerId>,
    queued_updates: BTreeMap<(i32, QueuedType, u64), VersionedGroupCallUpdate>,
    next_queue_seq: u64,
    server_participants_count: i32,
    all_participants_loaded: bool,
    listeners_hidden: bool,
    join_muted: bool,
    can_change_join_muted: bool,
    joined_to_top: bool,
    unmuted_video_limit: i32,
    title: String,
    record_start_date: UnixTime,
    record_video: bool,
    schedule_date: UnixTime,
    schedule_start_subscribed: bool,
    rtmp: bool,
    conference: bool,
    creator: bool,
    messages_enabled: bool,
    can_change_messages_enabled: bool,
    invite_link: String,
    unknown_spoken_ssrcs: HashMap<u32, LastSpokeTimes>,
}

impl ParticipantsList {
    /// Create a new empty participants list for `call_id`.
    pub fn new(call_id: CallId) -> Self {
        Self {
            call_id,
            version: 0,
            participants: Vec::new(),
            peer_by_audio_ssrc: HashMap::new(),
            queued_updates: BTreeMap::new(),
            next_queue_seq: 0,
            server_participants_count: 0,
            all_participants_loaded: false,
            listeners_hidden: false,
            join_muted: false,
            can_change_join_muted: true,
            joined_to_top: false,
            unmuted_video_limit: 0,
            title: String::new(),
            record_start_date: 0,
            record_video: false,
            schedule_date: 0,
            schedule_start_subscribed: false,
            rtmp: false,
            conference: false,
            creator: false,
            messages_enabled: false,
            can_change_messages_enabled: true,
            invite_link: String::new(),
            unknown_spoken_ssrcs: HashMap::new(),
        }
    }

    /// Group call ID.
    pub const fn call_id(&self) -> CallId {
        self.call_id
    }

    /// Current applied group call state version (0 if not yet loaded).
    pub const fn version(&self) -> i32 {
        self.version
    }

    /// Whether the group call metadata has been loaded (`version > 0`).
    pub const fn loaded(&self) -> bool {
        self.version > 0
    }

    /// Current participants slice.
    pub fn participants(&self) -> &[GroupCallParticipant] {
        &self.participants
    }

    /// Compute the displayed participant count (`Data::GroupCall::computeParticipantsCount`).
    pub fn full_count(&self) -> usize {
        if self.all_participants_loaded && !self.listeners_hidden {
            self.participants.len()
        } else {
            self.participants
                .len()
                .max(self.server_participants_count.max(0) as usize)
        }
    }

    /// Whether all participants have been loaded.
    pub const fn all_participants_loaded(&self) -> bool {
        self.all_participants_loaded
    }

    /// Whether new joiners start muted (`join_muted`).
    pub const fn join_muted(&self) -> bool {
        self.join_muted
    }

    /// Whether admins can change `join_muted`.
    pub const fn can_change_join_muted(&self) -> bool {
        self.can_change_join_muted
    }

    /// Whether new joiners appear at the top (`!join_date_asc`).
    pub const fn joined_to_top(&self) -> bool {
        self.joined_to_top
    }

    /// Maximum simultaneous video senders allowed.
    pub const fn unmuted_video_limit(&self) -> i32 {
        self.unmuted_video_limit
    }

    /// Group call title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Recording start Unix timestamp (0 if not recording).
    pub const fn record_start_date(&self) -> UnixTime {
        self.record_start_date
    }

    /// Scheduled start Unix timestamp (0 if active).
    pub const fn schedule_date(&self) -> UnixTime {
        self.schedule_date
    }

    /// Whether the local user is subscribed to a start notification for a scheduled group call.
    pub const fn schedule_start_subscribed(&self) -> bool {
        self.schedule_start_subscribed
    }

    /// Conference invite link, if any.
    pub fn invite_link(&self) -> &str {
        &self.invite_link
    }

    /// Whether this call is an RTMP livestream.
    pub const fn rtmp(&self) -> bool {
        self.rtmp
    }

    /// Whether listeners are hidden.
    pub const fn listeners_hidden(&self) -> bool {
        self.listeners_hidden
    }

    /// Look up a participant by peer ID.
    pub fn participant_by_peer(&self, peer_id: PeerId) -> Option<&GroupCallParticipant> {
        self.participants.iter().find(|p| p.peer_id == peer_id)
    }

    /// Look up a participant peer ID by primary or additional audio SSRC.
    pub fn peer_by_audio_ssrc(&self, ssrc: u32) -> Option<PeerId> {
        self.peer_by_audio_ssrc.get(&ssrc).copied()
    }

    /// Look up a participant by camera or screen video endpoint ID.
    pub fn participant_by_endpoint(&self, endpoint: &str) -> Option<&GroupCallParticipant> {
        if endpoint.is_empty() {
            return None;
        }
        self.participants
            .iter()
            .find(|p| p.camera_endpoint() == endpoint || p.screen_endpoint() == endpoint)
    }

    /// Compute the next local `raised_hand_rating` (`max + 1`, or `1` if empty).
    ///
    /// Exact port of `FindLocalRaisedHandRating` (`calls_group_call.cpp:80-87`).
    pub fn find_local_raised_hand_rating(&self) -> u64 {
        self.participants
            .iter()
            .map(|p| p.raised_hand_rating)
            .max()
            .map_or(1, |max| max.saturating_add(1))
    }

    /// Apply a full call snapshot (`phone.getGroupCall`).
    ///
    /// Port of `Data::GroupCall::processFullCallFields` (`data_group_call.cpp:479-499`).
    pub fn process_full_call(
        &mut self,
        call: GroupCallObject,
        participants: &[GroupCallParticipantData],
    ) -> ParticipantsListEvents {
        let mut events = ParticipantsListEvents::default();
        match call {
            GroupCallObject::Active(info) => {
                self.participants.clear();
                self.peer_by_audio_ssrc.clear();
                self.all_participants_loaded = false;
                self.apply_participants_slice(
                    participants,
                    ApplySliceSource::FullReloaded,
                    &mut events,
                );
                self.apply_call_fields(&info);
                self.process_queued_updates(false, &mut events);
            }
            GroupCallObject::Discarded(disc) => {
                events.discarded = true;
                events.applied_updates.push(VersionedGroupCallUpdate::Call(
                    GroupCallObject::Discarded(disc),
                ));
            }
        }
        events
    }

    /// Enqueue or immediately apply a versioned update (`updateGroupCall` or
    /// `updateGroupCallParticipants`).
    ///
    /// Exact port of `Data::GroupCall::enqueueUpdate` and `processQueuedUpdates`
    /// (`data_group_call.cpp:391-447, 599-634`).
    pub fn enqueue_update(&mut self, update: VersionedGroupCallUpdate) -> ParticipantsListEvents {
        let mut events = ParticipantsListEvents::default();
        // Match `Instance::handleGroupCallUpdate` (`calls/calls_instance.cpp:789-817`) and
        // `GroupCall::handleUpdate` (`calls/group/calls_group_call.cpp:2491, 2543`): ignore
        // updates belonging to a different group call ID.
        if !self.call_id.is_zero() {
            let update_call_id = match &update {
                VersionedGroupCallUpdate::Call(GroupCallObject::Active(info)) => info.id,
                VersionedGroupCallUpdate::Call(GroupCallObject::Discarded(disc)) => disc.id,
                VersionedGroupCallUpdate::Participants(data) => data.call_id,
            };
            if update_call_id != self.call_id {
                return events;
            }
        }
        let initial = self.version == 0;

        match update {
            VersionedGroupCallUpdate::Call(GroupCallObject::Discarded(_)) => {
                events.discarded = true;
                events.applied_updates.push(update);
                return events;
            }
            VersionedGroupCallUpdate::Call(GroupCallObject::Active(ref info)) => {
                let version = info.version;
                if self.version == 0 || self.version == version {
                    self.apply_enqueued_update(update, &mut events);
                } else if self.version < version {
                    let seq = self.next_queue_seq;
                    self.next_queue_seq += 1;
                    self.queued_updates
                        .insert((version, QueuedType::Call, seq), update);
                }
            }
            VersionedGroupCallUpdate::Participants(ref data) => {
                let version = data.version;
                let increment = data.participants.iter().any(|p| p.versioned);
                let required = if increment { version - 1 } else { version };
                if self.version == required {
                    self.apply_enqueued_update(update, &mut events);
                } else if self.version < required {
                    let kind = if increment {
                        QueuedType::VersionedParticipant
                    } else {
                        QueuedType::Participant
                    };
                    let seq = self.next_queue_seq;
                    self.next_queue_seq += 1;
                    self.queued_updates.insert((version, kind, seq), update);
                }
            }
        }

        self.process_queued_updates(initial, &mut events);
        events.has_pending_queued_updates = !self.queued_updates.is_empty();
        events
    }

    /// Apply a locally constructed optimistic participant update without touching version.
    ///
    /// Port of `Data::GroupCall::applyLocalUpdate` (`data_group_call.cpp:559-564`).
    pub fn apply_local_update(
        &mut self,
        participants: &[GroupCallParticipantData],
    ) -> ParticipantsListEvents {
        let mut events = ParticipantsListEvents::default();
        self.apply_participants_slice(
            participants,
            ApplySliceSource::UpdateConstructed,
            &mut events,
        );
        events
    }

    /// Apply audio level speaking timestamps for `ssrc` at `now_ms`.
    ///
    /// Exact port of `Data::GroupCall::applyLastSpoke` (`data_group_call.cpp:853-895`).
    pub fn apply_last_spoke(
        &mut self,
        ssrc: u32,
        when: LastSpokeTimes,
        now_ms: u64,
    ) -> ParticipantsListEvents {
        let mut events = ParticipantsListEvents::default();
        let Some(&peer_id) = self.peer_by_audio_ssrc.get(&ssrc) else {
            self.unknown_spoken_ssrcs.insert(ssrc, when);
            events.unknown_ssrcs_to_resolve.push(ssrc);
            return events;
        };

        let Some(participant) = self.participants.iter_mut().find(|p| p.peer_id == peer_id) else {
            return events;
        };

        let sounding = when.anything_ms.saturating_add(SOUND_STATUS_KEPT_FOR_MS) >= now_ms
            && when.anything_ms > 0
            && participant.can_self_unmute;
        let speaking = sounding
            && when.voice_ms > 0
            && when.voice_ms.saturating_add(SOUND_STATUS_KEPT_FOR_MS) >= now_ms;

        if speaking {
            events.speaking_peers.push(peer_id);
        }

        let use_additional = ssrc != participant.ssrc;
        let (now_sounding, now_speaking) = if use_additional {
            (
                participant.additional_sounding,
                participant.additional_speaking,
            )
        } else {
            (participant.sounding, participant.speaking)
        };

        if now_sounding != sounding || now_speaking != speaking {
            let was = participant.clone();
            if use_additional {
                participant.additional_sounding = sounding;
                participant.additional_speaking = speaking;
            } else {
                participant.sounding = sounding;
                participant.speaking = speaking;
            }
            events.participant_updates.push(ParticipantUpdate {
                was: Some(was),
                now: Some(participant.clone()),
            });
        }

        events
    }

    /// Apply resolved unknown participants loaded via `phone.getGroupParticipants(ssrcs)`.
    ///
    /// Port of `requestUnknownParticipants` `.done()` (`data_group_call.cpp:1044-1075`).
    pub fn apply_resolved_unknown_participants(
        &mut self,
        participants: &[GroupCallParticipantData],
        resolved_ssrcs: &[u32],
        now_ms: u64,
    ) -> ParticipantsListEvents {
        let mut events = ParticipantsListEvents::default();
        self.apply_participants_slice(participants, ApplySliceSource::UnknownLoaded, &mut events);
        for &ssrc in resolved_ssrcs {
            if let Some(when) = self.unknown_spoken_ssrcs.remove(&ssrc)
                && (when.voice_ms > 0 || when.anything_ms > 0)
            {
                let sub = self.apply_last_spoke(ssrc, when, now_ms);
                events.participant_updates.extend(sub.participant_updates);
                events.speaking_peers.extend(sub.speaking_peers);
            }
        }
        events
    }

    /// Number of buffered out-of-order updates waiting for a missing version.
    #[must_use]
    pub fn queued_updates_len(&self) -> usize {
        self.queued_updates.len()
    }

    /// Force-apply all queued out-of-order updates before issuing a reload (`phone.getGroupCall`).
    ///
    /// Port of `Data::GroupCall::reload` (`data/data_group_call.cpp:661-667`).
    pub fn flush_queued_updates_for_reload(&mut self) -> ParticipantsListEvents {
        let mut events = ParticipantsListEvents::default();
        while let Some((_, update)) = self.queued_updates.pop_first() {
            self.apply_enqueued_update(update, &mut events);
        }
        events.has_pending_queued_updates = false;
        events
    }

    /// Determine whether a `phone.getGroupCall` reload response needs a follow-up
    /// `phone.getGroupParticipants` fetch to avoid shrinking the loaded participants list.
    ///
    /// Exact port of `Data::GroupCall::requestParticipantsAfterReload`
    /// (`data/data_group_call.cpp:688-699`).
    #[must_use]
    pub fn needs_participants_after_reload(
        &self,
        call: &GroupCallObject,
        received_count: usize,
    ) -> bool {
        let server_count = match call {
            GroupCallObject::Active(info) => info.participants_count.max(0) as usize,
            GroupCallObject::Discarded(_) => 0,
        };
        received_count < server_count && received_count < self.participants.len()
    }

    /// Update the server-reported participant count after a `phone.getGroupParticipants` slice.
    ///
    /// Port of `Data::GroupCall::setServerParticipantsCount` (`data/data_group_call.cpp:207`).
    pub fn set_server_participants_count(&mut self, count: i32) {
        self.server_participants_count = count;
        self.all_participants_loaded =
            self.server_participants_count == self.participants.len() as i32;
    }

    /// Mark all participants loaded (`Data::GroupCall::setParticipantsLoaded`,
    /// `data/data_group_call.cpp:228-230`).
    pub fn set_all_participants_loaded(&mut self) {
        self.all_participants_loaded = true;
    }

    /// Drain all currently queued unknown SSRCs to request from the server.
    pub fn take_unknown_ssrcs(&mut self) -> HashSet<u32> {
        self.unknown_spoken_ssrcs.keys().copied().collect()
    }

    fn apply_call_fields(&mut self, data: &GroupCallInfo) {
        let initial = self.version == 0;
        self.version = if data.version == 0 { 1 } else { data.version };
        self.rtmp = data.rtmp_stream;
        self.creator = data.creator;
        self.listeners_hidden = data.listeners_hidden;
        self.conference = data.conference;
        if data.conference {
            self.can_change_join_muted = false;
            self.join_muted = false;
            self.can_change_messages_enabled = data.messages_enabled;
            if !data.messages_enabled || initial {
                self.messages_enabled = data.messages_enabled;
            }
        } else {
            self.can_change_join_muted = data.can_change_join_muted;
            self.join_muted = data.join_muted;
            self.can_change_messages_enabled = data.can_change_messages_enabled;
            self.messages_enabled = data.messages_enabled;
        }
        self.joined_to_top = !data.join_date_asc;
        self.server_participants_count = data.participants_count;
        self.title = data.title.clone().unwrap_or_default();
        self.record_video = data.record_video_active;
        self.record_start_date = data.record_start_date.unwrap_or(0);
        self.schedule_date = data.schedule_date.unwrap_or(0);
        self.schedule_start_subscribed = data.schedule_start_subscribed;
        self.unmuted_video_limit = data.unmuted_video_limit;
        self.all_participants_loaded =
            self.server_participants_count == self.participants.len() as i32;
        self.invite_link = data.invite_link.clone().unwrap_or_default();
    }

    fn apply_enqueued_update(
        &mut self,
        update: VersionedGroupCallUpdate,
        events: &mut ParticipantsListEvents,
    ) {
        match &update {
            VersionedGroupCallUpdate::Call(GroupCallObject::Active(info)) => {
                self.apply_call_fields(info);
            }
            VersionedGroupCallUpdate::Call(GroupCallObject::Discarded(_)) => {
                events.discarded = true;
            }
            VersionedGroupCallUpdate::Participants(data) => {
                self.version = if data.version == 0 { 1 } else { data.version };
                self.apply_participants_slice(
                    &data.participants,
                    ApplySliceSource::UpdateReceived,
                    events,
                );
            }
        }
        events.applied_updates.push(update);
    }

    fn process_queued_updates(&mut self, initial: bool, events: &mut ParticipantsListEvents) {
        if self.version == 0 {
            return;
        }
        while let Some((&(version, kind, _), _)) = self.queued_updates.first_key_value() {
            let incremented = kind == QueuedType::VersionedParticipant;
            if version < self.version || (version == self.version && incremented && !initial) {
                self.queued_updates.pop_first();
            } else if version == self.version || (version == self.version + 1 && incremented) {
                if let Some((_, update)) = self.queued_updates.pop_first() {
                    self.apply_enqueued_update(update, events);
                }
            } else {
                break;
            }
        }
    }

    /// Exact port of `Data::GroupCall::applyParticipantsSlice` (`data_group_call.cpp:701-851`).
    pub fn apply_participants_slice(
        &mut self,
        list: &[GroupCallParticipantData],
        source: ApplySliceSource,
        events: &mut ParticipantsListEvents,
    ) {
        for data in list {
            let pos = self
                .participants
                .iter()
                .position(|p| p.peer_id == data.peer);
            if data.left {
                if let Some(idx) = pos {
                    let removed = self.participants.remove(idx);
                    self.peer_by_audio_ssrc.remove(&removed.ssrc);
                    let add_ssrc = removed.additional_audio_ssrc();
                    if add_ssrc != 0 {
                        self.peer_by_audio_ssrc.remove(&add_ssrc);
                    }
                    if source != ApplySliceSource::FullReloaded {
                        events.participant_updates.push(ParticipantUpdate {
                            was: Some(removed),
                            now: None,
                        });
                    }
                }
                if self.server_participants_count > 0 {
                    self.server_participants_count -= 1;
                }
                continue;
            }

            let was = pos.map(|idx| self.participants[idx].clone());
            let can_self_unmute = !data.muted || data.can_self_unmute;
            let last_active = data
                .active_date
                .unwrap_or_else(|| was.as_ref().map_or(0, |w| w.last_active));
            let volume = if let Some(ref w) = was
                && !w.apply_volume_from_min
                && data.min
            {
                w.volume
            } else {
                data.volume.unwrap_or(DEFAULT_VOLUME)
            };
            let apply_volume_from_min = if let Some(ref w) = was
                && data.min
            {
                w.apply_volume_from_min
            } else {
                data.min || data.volume_by_admin
            };
            let muted_by_me = if let Some(ref w) = was
                && data.min
            {
                w.muted_by_me
            } else {
                data.muted_by_you
            };
            let only_min_loaded = data.min && was.as_ref().is_none_or(|w| w.only_min_loaded);
            let raised_hand_rating = data.raise_hand_rating.unwrap_or(0);
            let local_update = source == ApplySliceSource::UpdateConstructed;
            let video_params = if local_update {
                was.as_ref().and_then(|w| w.video_params.clone())
            } else {
                ParticipantVideoParams::parse(data.video.as_ref(), data.presentation.as_ref())
            };

            let value = GroupCallParticipant {
                peer_id: data.peer,
                video_params,
                date: data.date,
                last_active,
                raised_hand_rating,
                ssrc: data.source,
                volume,
                sounding: can_self_unmute && was.as_ref().is_some_and(|w| w.sounding),
                speaking: can_self_unmute && was.as_ref().is_some_and(|w| w.speaking),
                additional_sounding: can_self_unmute
                    && was.as_ref().is_some_and(|w| w.additional_sounding),
                additional_speaking: can_self_unmute
                    && was.as_ref().is_some_and(|w| w.additional_speaking),
                muted: data.muted,
                muted_by_me,
                can_self_unmute,
                only_min_loaded,
                video_joined: data.video_joined,
                apply_volume_from_min,
            };

            if let Some(idx) = pos {
                let old_ssrc = self.participants[idx].ssrc;
                let old_add = self.participants[idx].additional_audio_ssrc();
                if old_ssrc != value.ssrc {
                    self.peer_by_audio_ssrc.remove(&old_ssrc);
                    if value.ssrc != 0 {
                        self.peer_by_audio_ssrc.insert(value.ssrc, data.peer);
                    }
                }
                let new_add = value.additional_audio_ssrc();
                if old_add != new_add {
                    if old_add != 0 {
                        self.peer_by_audio_ssrc.remove(&old_add);
                    }
                    if new_add != 0 {
                        self.peer_by_audio_ssrc.insert(new_add, data.peer);
                    }
                }
                self.participants[idx] = value.clone();
            } else {
                if value.ssrc != 0 {
                    self.peer_by_audio_ssrc.insert(value.ssrc, data.peer);
                }
                let new_add = value.additional_audio_ssrc();
                if new_add != 0 {
                    self.peer_by_audio_ssrc.insert(new_add, data.peer);
                }
                self.participants.push(value.clone());
            }

            if data.just_joined {
                self.server_participants_count += 1;
            }
            if source != ApplySliceSource::FullReloaded {
                events.participant_updates.push(ParticipantUpdate {
                    was,
                    now: Some(value),
                });
            }
        }
    }
}
