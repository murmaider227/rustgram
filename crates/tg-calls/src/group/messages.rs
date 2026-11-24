//! In-call group messages, E2E encrypted conference messages, and paid Star reactions.
//!
//! Port of:
//! - `Telegram/SourceFiles/calls/group/calls_group_messages.h/.cpp`
//! - `Telegram/SourceFiles/calls/group/calls_group_message_encryption.h/.cpp`
//! - `Telegram/SourceFiles/calls/group/ui/calls_group_stars_coloring.h/.cpp`
//!
//! Provides:
//! - Exact JSON serialization/deserialization for E2E encrypted conference messages
//!   (`SerializeMessage` / `DeserializeMessage` in `calls_group_message_encryption.cpp`).
//! - Stars coloring tiers & minimum star calculation (`StarsColoringForCount`,
//!   `StarsRequiredForMessage`).
//! - Sans-IO [`GroupCallMessages`] state machine handling plain (`phone.sendGroupCallMessage`),
//!   E2E encrypted (`phone.sendGroupCallEncryptedMessage`), message deletion
//!   (`phone.deleteGroupCallMessages`, `phone.deleteGroupCallParticipantMessages`), TTL &
//!   video-stream 100-message eviction, and paid Star reaction batching/top donors
//!   (`phone.getGroupCallStars`).
//! - Layer 233 `tg-tl` RPC builders for `crates/tg-app/src/live/calls.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Map, Value};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::group::e2e::{ConferenceEncryptDecrypt, DEFAULT_CALL_CHANNEL_ID};
use crate::tl_conv::TlConversionError;
use crate::types::{
    GroupCallMessageEntity, InputGroupCall, PeerId, PhoneGroupCallStarsResponse, UnixTime,
    UpdateDeleteGroupCallMessages, UpdateGroupCallEncryptedMessage, UpdateGroupCallMessage,
};

/// Maximum retained messages in a video stream (`kMaxShownVideoStreamMessages = 100`,
/// `calls_group_messages.cpp:34`).
pub const MAX_SHOWN_VIDEO_STREAM_MESSAGES: usize = 100;

/// Short-poll interval for refreshing `phone.getGroupCallStars`
/// (`kStarsStatsShortPollDelay = 30_000 ms`, `calls_group_messages.cpp:35`).
pub const STARS_STATS_SHORT_POLL_DELAY_MS: u64 = 30_000;

/// Default message TTL in seconds when appConfig does not override it.
pub const DEFAULT_GROUP_CALL_MESSAGE_TTL_SECS: i32 = 10;

// ---------------------------------------------------------------------------
// Stars Coloring & Limits (`calls/group/ui/calls_group_stars_coloring.h/.cpp`)
// ---------------------------------------------------------------------------

/// Visual tier & message quota for a paid Star message (`Calls::Group::Ui::StarsColoring`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StarsColoring {
    /// Light theme background RGB hex.
    pub bg_light: i32,
    /// Dark theme background RGB hex.
    pub bg_dark: i32,
    /// Minimum star threshold for this tier.
    pub from_stars: i32,
    /// Duration in seconds that a paid message stays pinned.
    pub seconds_pin: i32,
    /// Maximum text length in UTF-16 code units allowed at this tier.
    pub characters_max: i32,
    /// Maximum emoji count allowed at this tier.
    pub emoji_limit: i32,
}

/// Select the highest [`StarsColoring`] tier whose `from_stars <= stars`.
///
/// Port of `StarsColoringForCount` (`calls_group_stars_coloring.cpp:20-30`).
#[must_use]
pub fn stars_coloring_for_count(colorings: &[StarsColoring], stars: i32) -> Option<StarsColoring> {
    if colorings.is_empty() {
        return None;
    }
    for (idx, entry) in colorings.iter().enumerate() {
        if entry.from_stars > stars {
            return Some(colorings[idx.saturating_sub(1)]);
        }
    }
    colorings.last().copied()
}

/// Compute the minimum Star count required for a message with `utf16_len` characters and
/// `emoji_count` emojis (`StarsRequiredForMessage`, `calls_group_stars_coloring.cpp:32-55`).
#[must_use]
pub fn stars_required_for_message(
    colorings: &[StarsColoring],
    utf16_len: i32,
    emoji_count: i32,
) -> i32 {
    if colorings.is_empty() {
        return 0;
    }
    for entry in colorings {
        if emoji_count <= entry.emoji_limit && utf16_len <= entry.characters_max {
            return entry.from_stars;
        }
    }
    colorings
        .last()
        .map_or(0, |last| last.from_stars.saturating_add(1))
}

// ---------------------------------------------------------------------------
// E2E Conference Message JSON Wire Codec (`calls_group_message_encryption.cpp`)
// ---------------------------------------------------------------------------

/// Plaintext payload carried inside `phone.sendGroupCallEncryptedMessage` /
/// `updateGroupCallEncryptedMessage` (`Calls::Group::PreparedMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEncryptedMessage {
    /// Random 64-bit deduplication identifier.
    pub random_id: u64,
    /// Message UTF-8 text.
    pub text: String,
    /// Supported inline formatting entities (offsets/lengths in UTF-16 code units).
    pub entities: Vec<GroupCallMessageEntity>,
}

/// Serialize a conference message into Telegram Desktop's compact JSON format
/// (`Calls::Group::SerializeMessage`, `calls_group_message_encryption.cpp:312-326`).
#[must_use]
pub fn serialize_encrypted_message(data: &PreparedEncryptedMessage) -> Vec<u8> {
    let mut entities_json = Vec::with_capacity(data.entities.len());
    for entity in &data.entities {
        if let Some(obj) = entity_to_json(entity) {
            entities_json.push(obj);
        }
    }

    let mut msg_obj = Map::new();
    msg_obj.insert(
        "_".to_string(),
        Value::String("textWithEntities".to_string()),
    );
    msg_obj.insert("text".to_string(), Value::String(data.text.clone()));
    msg_obj.insert("entities".to_string(), Value::Array(entities_json));

    let mut root = Map::new();
    root.insert(
        "_".to_string(),
        Value::String("groupCallMessage".to_string()),
    );
    root.insert(
        "random_id".to_string(),
        Value::String((data.random_id as i64).to_string()),
    );
    root.insert("message".to_string(), Value::Object(msg_obj));

    serde_json::to_vec(&Value::Object(root)).unwrap_or_default()
}

/// Deserialize a decrypted conference message JSON payload, validating `_`, `_min_layer`,
/// `random_id`, and entity UTF-16 bounds (`Calls::Group::DeserializeMessage`,
/// `calls_group_message_encryption.cpp:328-365`).
#[must_use]
pub fn deserialize_encrypted_message(data: &[u8]) -> Option<PreparedEncryptedMessage> {
    let value: Value = serde_json::from_slice(data).ok()?;
    let root = value.as_object()?;
    if is_unsupported_json_object(root, Some("groupCallMessage")) {
        return None;
    }
    let random_id = json_get_u64(root, "random_id")?;
    if random_id == 0 {
        return None;
    }
    let msg_obj = root.get("message")?.as_object()?;
    if is_unsupported_json_object(msg_obj, Some("textWithEntities")) {
        return None;
    }
    let text = msg_obj.get("text")?.as_str()?.to_string();
    let utf16_len = text.encode_utf16().count();
    let entities_arr = msg_obj.get("entities")?.as_array()?;
    let mut entities = Vec::with_capacity(entities_arr.len());
    for item in entities_arr {
        if let Some(obj) = item.as_object()
            && let Some(ent) = json_to_entity(utf16_len, obj)
        {
            entities.push(ent);
        }
    }
    Some(PreparedEncryptedMessage {
        random_id,
        text,
        entities,
    })
}

fn entity_to_json(entity: &GroupCallMessageEntity) -> Option<Value> {
    let mut obj = Map::new();
    match *entity {
        GroupCallMessageEntity::Bold { offset, length } => {
            obj.insert("_".into(), Value::String("messageEntityBold".into()));
            obj.insert("offset".into(), Value::from(offset));
            obj.insert("length".into(), Value::from(length));
        }
        GroupCallMessageEntity::Italic { offset, length } => {
            obj.insert("_".into(), Value::String("messageEntityItalic".into()));
            obj.insert("offset".into(), Value::from(offset));
            obj.insert("length".into(), Value::from(length));
        }
        GroupCallMessageEntity::Underline { offset, length } => {
            obj.insert("_".into(), Value::String("messageEntityUnderline".into()));
            obj.insert("offset".into(), Value::from(offset));
            obj.insert("length".into(), Value::from(length));
        }
        GroupCallMessageEntity::Strike { offset, length } => {
            obj.insert("_".into(), Value::String("messageEntityStrike".into()));
            obj.insert("offset".into(), Value::from(offset));
            obj.insert("length".into(), Value::from(length));
        }
        GroupCallMessageEntity::Spoiler { offset, length } => {
            obj.insert("_".into(), Value::String("messageEntitySpoiler".into()));
            obj.insert("offset".into(), Value::from(offset));
            obj.insert("length".into(), Value::from(length));
        }
        GroupCallMessageEntity::CustomEmoji {
            offset,
            length,
            document_id,
        } => {
            obj.insert("_".into(), Value::String("messageEntityCustomEmoji".into()));
            obj.insert("offset".into(), Value::from(offset));
            obj.insert("length".into(), Value::from(length));
            obj.insert("document_id".into(), Value::String(document_id.to_string()));
        }
        GroupCallMessageEntity::Code { .. } => {
            // `calls_group_message_encryption.cpp:148-169` only serializes Bold/Italic/Underline/
            // Strike/Spoiler/CustomEmoji into conference encrypted JSON.
            return None;
        }
    }
    Some(Value::Object(obj))
}

fn json_get_i32(obj: &Map<String, Value>, key: &str) -> Option<i32> {
    let val = obj.get(key)?;
    if let Some(n) = val.as_i64() {
        i32::try_from(n).ok()
    } else if let Some(f) = val.as_f64() {
        Some(f.round() as i32)
    } else if let Some(s) = val.as_str() {
        s.parse::<i32>().ok()
    } else {
        None
    }
}

fn json_get_u64(obj: &Map<String, Value>, key: &str) -> Option<u64> {
    let val = obj.get(key)?;
    if let Some(n) = val.as_u64() {
        Some(n)
    } else if let Some(n) = val.as_i64() {
        Some(n as u64)
    } else if let Some(f) = val.as_f64() {
        (f >= 0.0).then_some(f.round() as u64)
    } else if let Some(s) = val.as_str() {
        s.parse::<i64>().ok().map(|v| v as u64)
    } else {
        None
    }
}

fn is_unsupported_json_object(obj: &Map<String, Value>, expected_cons: Option<&str>) -> bool {
    if let Some(min_layer) = json_get_i32(obj, "_min_layer")
        && min_layer > tg_tl::LAYER
    {
        return true;
    }
    if let Some(cons) = expected_cons {
        let actual = obj.get("_").and_then(Value::as_str);
        if actual != Some(cons) {
            return true;
        }
    }
    false
}

fn json_to_entity(utf16_len: usize, obj: &Map<String, Value>) -> Option<GroupCallMessageEntity> {
    if is_unsupported_json_object(obj, None) {
        return None;
    }
    let cons = obj.get("_")?.as_str()?;
    let offset = json_get_i32(obj, "offset")?;
    let length = json_get_i32(obj, "length")?;
    if offset < 0 || length <= 0 {
        return None;
    }
    let off = offset as usize;
    let len = length as usize;
    if off >= utf16_len || len > utf16_len || off.checked_add(len)? > utf16_len {
        return None;
    }
    match cons {
        "messageEntityBold" => Some(GroupCallMessageEntity::Bold { offset, length }),
        "messageEntityItalic" => Some(GroupCallMessageEntity::Italic { offset, length }),
        "messageEntityUnderline" => Some(GroupCallMessageEntity::Underline { offset, length }),
        "messageEntityStrike" => Some(GroupCallMessageEntity::Strike { offset, length }),
        "messageEntitySpoiler" => Some(GroupCallMessageEntity::Spoiler { offset, length }),
        "messageEntityCustomEmoji" => {
            let doc_id = json_get_u64(obj, "document_id")? as i64;
            if doc_id == 0 {
                return None;
            }
            Some(GroupCallMessageEntity::CustomEmoji {
                offset,
                length,
                document_id: doc_id,
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// In-Call Messages & Paid Star Reactions State Machine (`Calls::Group::Messages`)
// ---------------------------------------------------------------------------

/// A single visible in-call group message (`Calls::Group::Message`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCallChatMessage {
    /// Server message ID (or negative local placeholder ID while sending).
    pub id: i32,
    /// Unix send timestamp in seconds (0 while sending).
    pub date: UnixTime,
    /// Unix timestamp when a paid message unpins (0 for unpaid messages).
    pub pin_finish_date: UnixTime,
    /// Author peer ID.
    pub peer: PeerId,
    /// Message text.
    pub text: String,
    /// Allowed inline entities.
    pub entities: Vec<GroupCallMessageEntity>,
    /// Paid Stars attached to this message (0 for free messages).
    pub stars: i64,
    /// `true` if sending failed.
    pub failed: bool,
    /// `true` if sent by an admin.
    pub admin: bool,
    /// `true` if sent by the local user.
    pub mine: bool,
}

/// Top Star donor entry (`Calls::Group::StarsDonor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarsDonor {
    /// Donor peer ID (or `None` for anonymous donors).
    pub peer: Option<PeerId>,
    /// Total Stars donated by this peer.
    pub stars: i64,
    /// `true` if this entry represents the local user.
    pub my: bool,
}

/// Aggregated paid Stars leaderboard (`Calls::Group::StarsTop`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StarsTop {
    /// Ranked top donors (descending by `stars`).
    pub top_donors: Vec<StarsDonor>,
    /// Total Stars sent to this call/stream.
    pub total: i64,
}

/// Local + server paid Star summary (`Messages::PaidLocalState`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaidLocalState {
    /// Total Stars (server total + locally scheduled unsent Stars).
    pub total: i64,
    /// Local user's total Stars (server + locally scheduled).
    pub my: i64,
}

/// Request to delete in-call messages (`Calls::Group::MessageDeleteRequest`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageDeleteRequest {
    /// Specific message ID to delete (when `delete_all_from` is `None`).
    pub id: i32,
    /// If `Some`, delete all messages sent by this participant.
    pub delete_all_from: Option<PeerId>,
    /// Whether to report as spam.
    pub report_spam: bool,
}

/// Sans-IO actions emitted by [`GroupCallMessages`] for the caller (`live/calls.rs`) to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupMessageAction {
    /// Send a regular or paid group call message (`phone.sendGroupCallMessage`).
    SendPlainMessage {
        /// Random 64-bit deduplication ID.
        random_id: u64,
        /// Message text.
        text: String,
        /// Formatting entities.
        entities: Vec<GroupCallMessageEntity>,
        /// Paid Stars (0 if free).
        allow_paid_stars: i64,
        /// Send-as peer ID.
        send_as: PeerId,
    },
    /// Send an E2E encrypted conference message (`phone.sendGroupCallEncryptedMessage`).
    SendEncryptedMessage {
        /// Random 64-bit deduplication ID.
        random_id: u64,
        /// Encrypted wire bytes produced by [`ConferenceEncryptDecrypt`].
        encrypted_message: Vec<u8>,
    },
    /// Delete specific message IDs (`phone.deleteGroupCallMessages`).
    DeleteMessages {
        /// Message IDs to remove.
        message_ids: Vec<i32>,
        /// Report spam flag.
        report_spam: bool,
    },
    /// Delete all messages from a participant (`phone.deleteGroupCallParticipantMessages`).
    DeleteParticipantMessages {
        /// Participant peer ID.
        participant: PeerId,
        /// Report spam flag.
        report_spam: bool,
    },
    /// Fetch paid Stars leaderboard (`phone.getGroupCallStars`).
    RequestStarsStats,
    /// Local placeholder message ID was promoted to a real server/conference ID.
    MessageIdUpdated {
        /// Previous negative local ID.
        local_id: i32,
        /// Assigned real ID.
        real_id: i32,
    },
    /// Visible message list changed.
    MessagesChanged,
    /// Paid Star reaction state or donor leaderboard changed.
    StarsChanged {
        /// Optional newly-contributed donor peer & count.
        delta: Option<(PeerId, i64)>,
    },
}

#[derive(Debug, Clone)]
struct PendingMessage {
    text: String,
    entities: Vec<GroupCallMessageEntity>,
    stars: i64,
}

#[derive(Debug, Clone, Default)]
struct PaidBatchState {
    top: StarsTop,
    scheduled: i64,
    scheduled_shown_peer: Option<PeerId>,
    sending: i64,
    sending_shown_peer: Option<PeerId>,
    sending_pending: bool,
}

/// Sans-IO manager for in-call group messages (plain + E2E encrypted conference) and paid Star
/// reactions (`Calls::Group::Messages` in `calls/group/calls_group_messages.cpp`).
#[derive(Debug)]
pub struct GroupCallMessages {
    call_peer: PeerId,
    my_peer: PeerId,
    messages_from: PeerId,
    is_conference: bool,
    is_video_stream: bool,
    is_creator: bool,
    min_paid_stars: i64,
    ttl_secs: i32,
    colorings: Vec<StarsColoring>,
    e2e: Option<Arc<ConferenceEncryptDecrypt>>,
    ready: bool,
    applying_initial: bool,

    next_local_id: i32,
    conference_id_auto_inc: i32,
    conference_id_by_random_id: BTreeMap<u64, i32>,
    sending_id_by_random_id: BTreeMap<u64, i32>,
    skipped_ids: BTreeSet<i32>,

    pending: Vec<PendingMessage>,
    messages: Vec<GroupCallChatMessage>,
    paid: PaidBatchState,
    actions: Vec<GroupMessageAction>,
}

impl GroupCallMessages {
    /// Create a new in-call messages & paid reactions manager.
    #[must_use]
    pub fn new(
        call_peer: PeerId,
        my_peer: PeerId,
        messages_from: PeerId,
        is_conference: bool,
        is_video_stream: bool,
        is_creator: bool,
    ) -> Self {
        Self {
            call_peer,
            my_peer,
            messages_from,
            is_conference,
            is_video_stream,
            is_creator,
            min_paid_stars: 0,
            ttl_secs: DEFAULT_GROUP_CALL_MESSAGE_TTL_SECS,
            colorings: Vec::new(),
            e2e: None,
            ready: false,
            applying_initial: false,
            next_local_id: -1,
            conference_id_auto_inc: 0,
            conference_id_by_random_id: BTreeMap::new(),
            sending_id_by_random_id: BTreeMap::new(),
            skipped_ids: BTreeSet::new(),
            pending: Vec::new(),
            messages: Vec::new(),
            paid: PaidBatchState::default(),
            actions: Vec::new(),
        }
    }

    /// Attach or replace the conference E2E encryptor/decryptor handle.
    pub fn set_e2e(&mut self, e2e: Option<Arc<ConferenceEncryptDecrypt>>) {
        self.e2e = e2e;
        if self.is_ready() {
            self.flush_pending();
        }
    }

    /// Mark the underlying group call metadata as loaded and ready to send queued messages.
    pub fn set_call_ready(&mut self, ready: bool) {
        self.ready = ready;
        if self.is_ready() {
            self.flush_pending();
        }
    }

    /// Update the `send_as` peer used for outgoing messages and paid Star reactions.
    pub fn set_messages_from(&mut self, peer: PeerId) {
        self.messages_from = peer;
    }

    /// Configure TTL in seconds (default 10 s), minimum paid message stars, and Stars coloring tiers.
    pub fn configure_limits(
        &mut self,
        ttl_secs: i32,
        min_paid_stars: i64,
        colorings: Vec<StarsColoring>,
    ) {
        self.ttl_secs = ttl_secs.max(1);
        self.min_paid_stars = min_paid_stars.max(0);
        self.colorings = colorings;
    }

    /// Toggle whether initial call updates are being applied (suppresses duplicate Star bumps).
    pub fn set_applying_initial(&mut self, value: bool) {
        self.applying_initial = value;
    }

    /// Whether the manager can currently emit outbound messages (`Messages::ready`).
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready && (!self.is_conference || self.e2e.as_ref().is_some_and(|e| e.is_ready()))
    }

    /// Currently visible in-call messages.
    #[must_use]
    pub fn messages(&self) -> &[GroupCallChatMessage] {
        &self.messages
    }

    /// Current Stars donor leaderboard.
    #[must_use]
    pub fn stars_top(&self) -> &StarsTop {
        &self.paid.top
    }

    /// Scheduled unsent paid Star reactions count.
    #[must_use]
    pub const fn reactions_paid_scheduled(&self) -> i64 {
        self.paid.scheduled
    }

    /// Peer ID currently shown for local paid Star reactions (`reactionsLocalShownPeer`).
    #[must_use]
    pub fn reactions_local_shown_peer(&self) -> PeerId {
        if let Some(p) = self.paid.scheduled_shown_peer {
            return p;
        }
        if let Some(p) = self.paid.sending_shown_peer {
            return p;
        }
        for entry in &self.paid.top.top_donors {
            if entry.my {
                return entry.peer.unwrap_or(PeerId::ZERO);
            }
        }
        self.messages_from
    }

    /// Combined server + locally scheduled Star counts (`Messages::starsLocalState`).
    #[must_use]
    pub fn stars_local_state(&self) -> PaidLocalState {
        let my_server = self
            .paid
            .top
            .top_donors
            .iter()
            .find(|d| d.my)
            .map_or(0, |d| d.stars);
        PaidLocalState {
            total: self.paid.top.total.saturating_add(self.paid.scheduled),
            my: my_server.saturating_add(self.paid.scheduled),
        }
    }

    /// Drain all emitted [`GroupMessageAction`]s.
    pub fn take_actions(&mut self) -> Vec<GroupMessageAction> {
        std::mem::take(&mut self.actions)
    }

    /// Request a refresh of `phone.getGroupCallStars` (no-op unless video stream).
    pub fn request_stars_stats(&mut self) {
        if self.is_video_stream {
            self.actions.push(GroupMessageAction::RequestStarsStats);
        }
    }

    /// Apply a `phone.GroupCallStars` response from the server (`Messages::requestStarsStats`).
    pub fn apply_stars_stats(&mut self, stats: &PhoneGroupCallStarsResponse) {
        self.paid.top = StarsTop {
            total: stats.total_stars,
            top_donors: stats
                .top_donors
                .iter()
                .map(|d| StarsDonor {
                    peer: d.peer_id,
                    stars: d.stars,
                    my: d.my,
                })
                .collect(),
        };
        self.actions
            .push(GroupMessageAction::StarsChanged { delta: None });
    }

    /// Send an in-call message (plain or E2E encrypted if `is_conference`), with optional paid Stars.
    ///
    /// Port of `Messages::send` (`calls_group_messages.cpp:135-213`).
    pub fn send(
        &mut self,
        text: String,
        entities: Vec<GroupCallMessageEntity>,
        stars: i64,
        random_id: u64,
        now_unix: UnixTime,
    ) {
        if text.is_empty() && stars <= 0 {
            return;
        }
        if !self.is_ready() {
            self.pending.push(PendingMessage {
                text,
                entities,
                stars,
            });
            return;
        }

        let local_id = self.alloc_local_id();
        self.sending_id_by_random_id.insert(random_id, local_id);

        let from = self.messages_from;
        let skip = self.should_skip_message(&text, stars);
        if skip {
            self.skipped_ids.insert(local_id);
        } else {
            self.messages.push(GroupCallChatMessage {
                id: local_id,
                date: 0,
                pin_finish_date: 0,
                peer: from,
                text: text.clone(),
                entities: entities.clone(),
                stars,
                failed: false,
                admin: (from == self.call_peer) || (self.is_creator && from == self.my_peer),
                mine: true,
            });
        }

        if !self.is_conference {
            self.actions.push(GroupMessageAction::SendPlainMessage {
                random_id,
                text,
                entities,
                allow_paid_stars: stars.max(0),
                send_as: from,
            });
        } else if let Some(ref e2e) = self.e2e {
            let prepared = PreparedEncryptedMessage {
                random_id,
                text,
                entities,
            };
            let json_bytes = serialize_encrypted_message(&prepared);
            if let Ok(encrypted_message) =
                e2e.encrypt_frame(DEFAULT_CALL_CHANNEL_ID, &json_bytes, 0)
            {
                self.actions.push(GroupMessageAction::SendEncryptedMessage {
                    random_id,
                    encrypted_message,
                });
            } else {
                self.handle_send_failed(random_id, now_unix);
                return;
            }
        }

        self.add_stars(from, stars, true);
        if !skip {
            self.prune_expired(now_unix, true);
        }
    }

    /// Process an incoming plain `updateGroupCallMessage`.
    pub fn handle_incoming_message(&mut self, update: &UpdateGroupCallMessage, now_unix: UnixTime) {
        if !self.is_ready() || self.is_conference {
            return;
        }
        let m = &update.message;
        self.receive_inner(
            m.id,
            m.from_id,
            m.text.clone(),
            m.entities.clone(),
            m.date,
            m.paid_message_stars.unwrap_or(0),
            m.from_admin,
            now_unix,
        );
    }

    /// Process an incoming E2E `updateGroupCallEncryptedMessage`
    /// (`Messages::received(MTPDupdateGroupCallEncryptedMessage)`, `calls_group_messages.cpp:233-267`).
    pub fn handle_incoming_encrypted_message(
        &mut self,
        update: &UpdateGroupCallEncryptedMessage,
        now_unix: UnixTime,
    ) -> bool {
        if !self.is_ready() || !self.is_conference {
            return false;
        }
        let Some(ref e2e) = self.e2e else {
            return false;
        };
        let Some(sender_uid) = update.from_id.to_user() else {
            return false;
        };
        let Ok(decrypted) = e2e.decrypt_frame(
            sender_uid,
            DEFAULT_CALL_CHANNEL_ID,
            &update.encrypted_message,
        ) else {
            return false;
        };
        let Some(parsed) = deserialize_encrypted_message(&decrypted) else {
            return false;
        };
        self.conference_id_auto_inc = self.conference_id_auto_inc.saturating_add(1);
        let real_id = self.conference_id_auto_inc;
        if self
            .conference_id_by_random_id
            .contains_key(&parsed.random_id)
        {
            return true; // Duplicate already received.
        }
        self.conference_id_by_random_id
            .insert(parsed.random_id, real_id);

        self.receive_inner(
            real_id,
            update.from_id,
            parsed.text,
            parsed.entities,
            now_unix,
            0,
            false,
            now_unix,
        );
        true
    }

    /// Handle `updateDeleteGroupCallMessages`.
    pub fn handle_deleted_messages(&mut self, update: &UpdateDeleteGroupCallMessages) {
        let before = self.messages.len();
        self.messages.retain(|m| !update.messages.contains(&m.id));
        if self.messages.len() < before {
            self.actions.push(GroupMessageAction::MessagesChanged);
        }
    }

    /// Handle `updateMessageID` acknowledging a plain group call message.
    pub fn handle_message_sent(&mut self, random_id: u64, real_id: i32, now_unix: UnixTime) {
        self.promote_sent_id(random_id, real_id, now_unix);
    }

    /// Handle successful `phone.sendGroupCallEncryptedMessage` response (`Bool::True`).
    pub fn handle_encrypted_message_sent(&mut self, random_id: u64, now_unix: UnixTime) {
        self.conference_id_auto_inc = self.conference_id_auto_inc.saturating_add(1);
        let real_id = self.conference_id_auto_inc;
        self.conference_id_by_random_id.insert(random_id, real_id);
        self.promote_sent_id(random_id, real_id, now_unix);
    }

    /// Mark an in-flight message send as failed (`Messages::failed`).
    pub fn handle_send_failed(&mut self, random_id: u64, now_unix: UnixTime) {
        let Some(local_id) = self.sending_id_by_random_id.remove(&random_id) else {
            return;
        };
        if let Some(msg) = self.messages.iter_mut().find(|m| m.id == local_id)
            && msg.date == 0
        {
            msg.date = now_unix;
            msg.stars = 0;
            msg.failed = true;
            self.prune_expired(now_unix, true);
        }
    }

    /// Queue `count` paid Stars to be sent as a batched Star reaction (`reactionsPaidAdd`).
    pub fn reactions_paid_add(&mut self, count: i64) {
        if count <= 0 {
            return;
        }
        self.paid.scheduled = self.paid.scheduled.saturating_add(count);
        self.paid.scheduled_shown_peer = Some(self.messages_from);
        self.actions
            .push(GroupMessageAction::StarsChanged { delta: None });
    }

    /// Cancel locally scheduled unsent paid Star reactions (`reactionsPaidScheduledCancel`).
    pub fn reactions_paid_cancel(&mut self) {
        if self.paid.scheduled_shown_peer.is_none() && self.paid.scheduled == 0 {
            return;
        }
        self.paid.scheduled = 0;
        self.paid.scheduled_shown_peer = None;
        self.actions
            .push(GroupMessageAction::StarsChanged { delta: None });
    }

    /// Flush scheduled paid Star reactions via `phone.sendGroupCallMessage` with empty text
    /// (`Messages::reactionsPaidSend`, `calls_group_messages.cpp:608-654`).
    pub fn reactions_paid_send(&mut self, random_id: u64, now_unix: UnixTime) {
        self.paid.sending_pending = false;
        if self.paid.scheduled <= 0 {
            return;
        }
        if self.paid.sending > 0 || !self.is_ready() {
            self.paid.sending_pending = true;
            return;
        }
        let stars = self.paid.scheduled;
        let from = self.paid.scheduled_shown_peer.unwrap_or(self.messages_from);
        self.paid.sending = stars;
        self.paid.sending_shown_peer = Some(from);
        self.paid.scheduled = 0;
        self.paid.scheduled_shown_peer = None;

        let local_id = self.alloc_local_id();
        self.sending_id_by_random_id.insert(random_id, local_id);

        let skip = self.should_skip_message("", stars);
        if skip {
            self.skipped_ids.insert(local_id);
        } else {
            self.messages.push(GroupCallChatMessage {
                id: local_id,
                date: 0,
                pin_finish_date: 0,
                peer: from,
                text: String::new(),
                entities: Vec::new(),
                stars,
                failed: false,
                admin: from == self.call_peer,
                mine: true,
            });
        }

        self.actions.push(GroupMessageAction::SendPlainMessage {
            random_id,
            text: String::new(),
            entities: Vec::new(),
            allow_paid_stars: stars,
            send_as: from,
        });
        self.add_stars(from, stars, true);
        if !skip {
            self.prune_expired(now_unix, true);
        }
    }

    /// Complete an in-flight paid Star reaction send (`Messages::finishPaidSending`).
    pub fn finish_paid_sending(&mut self, success: bool, next_random_id: u64, now_unix: UnixTime) {
        let amount = self.paid.sending;
        let from = self.paid.sending_shown_peer.take();
        self.paid.sending = 0;

        if amount > 0 && !success {
            // Roll back optimistic bump on failure.
            self.paid.top.total = self.paid.top.total.saturating_sub(amount);
            if let Some(peer) = from
                && let Some(donor) = self
                    .paid
                    .top
                    .top_donors
                    .iter_mut()
                    .find(|d| d.peer == Some(peer))
            {
                donor.stars = donor.stars.saturating_sub(amount);
            }
            self.actions
                .push(GroupMessageAction::StarsChanged { delta: None });
        }
        if self.paid.sending_pending {
            self.reactions_paid_send(next_random_id, now_unix);
        }
    }

    /// Request deletion of one message or all messages from a participant
    /// (`Messages::deleteConfirmed`, `calls_group_messages.cpp:669-704`).
    pub fn delete_confirmed(&mut self, req: MessageDeleteRequest) {
        let before = self.messages.len();
        if let Some(from) = req.delete_all_from {
            self.actions
                .push(GroupMessageAction::DeleteParticipantMessages {
                    participant: from,
                    report_spam: req.report_spam,
                });
            self.messages.retain(|m| m.peer != from);
        } else {
            self.actions.push(GroupMessageAction::DeleteMessages {
                message_ids: vec![req.id],
                report_spam: req.report_spam,
            });
            self.messages.retain(|m| m.id != req.id);
        }
        if self.messages.len() < before {
            self.actions.push(GroupMessageAction::MessagesChanged);
        }
    }

    /// Sweep expired TTL messages (or enforce max-100 cap in video streams) at `now_unix`
    /// (`Messages::checkDestroying`, `calls_group_messages.cpp:400-452`).
    ///
    /// Returns the delay in seconds until the next message expires (or `None` if no timer needed).
    pub fn prune_expired(&mut self, now_unix: UnixTime, emit_change: bool) -> Option<i32> {
        let initial = self.messages.len();
        let mut next_delay: Option<i32> = None;

        if self.is_video_stream {
            if self.messages.len() > MAX_SHOWN_VIDEO_STREAM_MESSAGES {
                let mut to_remove = self.messages.len() - MAX_SHOWN_VIDEO_STREAM_MESSAGES;
                let mut idx = 0;
                while to_remove > 0 && idx < self.messages.len() {
                    let m = &self.messages[idx];
                    if m.date != 0 && m.pin_finish_date <= now_unix {
                        self.messages.remove(idx);
                        to_remove -= 1;
                    } else {
                        let rem = m.pin_finish_date.saturating_sub(now_unix);
                        if rem > 0 {
                            next_delay = Some(next_delay.map_or(rem, |n| n.min(rem)));
                        }
                        idx += 1;
                        to_remove -= 1;
                    }
                }
            }
        } else {
            let ttl = self.ttl_secs;
            let mut idx = 0;
            while idx < self.messages.len() {
                let m = &self.messages[idx];
                if m.date == 0 {
                    if m.id < 0 {
                        idx += 1;
                    } else {
                        self.messages.remove(idx);
                    }
                } else if m.date.saturating_add(ttl) <= now_unix {
                    self.messages.remove(idx);
                } else {
                    let rem = m.date.saturating_add(ttl).saturating_sub(now_unix);
                    next_delay = Some(next_delay.map_or(rem, |n| n.min(rem)));
                    idx += 1;
                }
            }
        }

        if emit_change || self.messages.len() < initial {
            self.actions.push(GroupMessageAction::MessagesChanged);
        }
        next_delay
    }

    #[allow(clippy::too_many_arguments)]
    fn receive_inner(
        &mut self,
        id: i32,
        from: PeerId,
        text: String,
        entities: Vec<GroupCallMessageEntity>,
        date: UnixTime,
        stars: i64,
        from_admin: bool,
        now_unix: UnixTime,
    ) {
        if let Some(pos) = self.messages.iter().position(|m| m.id == id) {
            if (from == self.my_peer || from == self.messages_from) && self.messages[pos].date == 0
            {
                let existing_stars = self.messages[pos].stars;
                let pin_finish = self.pin_finish_date(date, existing_stars);
                self.messages[pos].date = date;
                self.messages[pos].pin_finish_date = pin_finish;
                self.prune_expired(now_unix, true);
            }
            return;
        }
        if self.skipped_ids.contains(&id) {
            return;
        }
        let mine = from == self.my_peer || from == self.messages_from;
        let skip = self.should_skip_message(&text, stars);
        if skip {
            self.skipped_ids.insert(id);
        } else {
            let pin_finish_date = self.pin_finish_date(date, stars);
            self.messages.push(GroupCallChatMessage {
                id,
                date,
                pin_finish_date,
                peer: from,
                text,
                entities,
                stars,
                failed: false,
                admin: from_admin,
                mine,
            });
            self.messages.sort_by_key(|m| m.id);
        }
        if !self.applying_initial {
            self.add_stars(from, stars, mine);
        }
        if !skip {
            self.prune_expired(now_unix, true);
        }
    }

    fn promote_sent_id(&mut self, random_id: u64, real_id: i32, now_unix: UnixTime) {
        let Some(local_id) = self.sending_id_by_random_id.remove(&random_id) else {
            return;
        };
        let Some(pos) = self.messages.iter().position(|m| m.id == local_id) else {
            self.skipped_ids.insert(real_id);
            return;
        };
        self.messages[pos].id = real_id;
        if self.messages[pos].date == 0 {
            self.messages[pos].date = now_unix;
            let stars = self.messages[pos].stars;
            self.messages[pos].pin_finish_date = self.pin_finish_date(now_unix, stars);
        }
        self.actions
            .push(GroupMessageAction::MessageIdUpdated { local_id, real_id });
        self.prune_expired(now_unix, true);
    }

    fn flush_pending(&mut self) {
        let queued = std::mem::take(&mut self.pending);
        for item in queued {
            let random_id = tg_crypto::random::random_u64();
            self.send(item.text, item.entities, item.stars, random_id, 0);
        }
        if self.paid.sending_pending {
            let random_id = tg_crypto::random::random_u64();
            self.reactions_paid_send(random_id, 0);
        }
    }

    fn should_skip_message(&self, text: &str, stars: i64) -> bool {
        text.is_empty() && stars < self.min_paid_stars
    }

    fn pin_finish_date(&self, date: UnixTime, stars: i64) -> UnixTime {
        if date == 0 || stars <= 0 {
            return 0;
        }
        let secs =
            stars_coloring_for_count(&self.colorings, stars as i32).map_or(0, |c| c.seconds_pin);
        date.saturating_add(secs)
    }

    fn add_stars(&mut self, from: PeerId, stars: i64, mine: bool) {
        if stars <= 0 {
            return;
        }
        self.paid.top.total = self.paid.top.total.saturating_add(stars);
        if let Some(donor) = self
            .paid
            .top
            .top_donors
            .iter_mut()
            .find(|d| d.peer == Some(from))
        {
            donor.stars = donor.stars.saturating_add(stars);
        } else {
            self.paid.top.top_donors.push(StarsDonor {
                peer: Some(from),
                stars,
                my: mine,
            });
        }
        self.paid
            .top
            .top_donors
            .sort_by_key(|a| std::cmp::Reverse(a.stars));
        self.actions.push(GroupMessageAction::StarsChanged {
            delta: Some((from, stars)),
        });
    }

    fn alloc_local_id(&mut self) -> i32 {
        let id = self.next_local_id;
        self.next_local_id = self.next_local_id.saturating_sub(1);
        id
    }
}

// ---------------------------------------------------------------------------
// Layer 233 `phone.*` In-Call Message & Stars RPC Builders
// ---------------------------------------------------------------------------

/// Convert [`GroupCallMessageEntity`] list to `tg-tl` `MessageEntity` vector.
#[must_use]
pub fn entities_to_tl(entities: &[GroupCallMessageEntity]) -> Vec<tl_enums::MessageEntity> {
    entities
        .iter()
        .map(|e| match *e {
            GroupCallMessageEntity::Bold { offset, length } => {
                tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold { offset, length })
            }
            GroupCallMessageEntity::Italic { offset, length } => {
                tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic { offset, length })
            }
            GroupCallMessageEntity::Underline { offset, length } => {
                tl_enums::MessageEntity::Underline(tl_types::MessageEntityUnderline {
                    offset,
                    length,
                })
            }
            GroupCallMessageEntity::Strike { offset, length } => {
                tl_enums::MessageEntity::Strike(tl_types::MessageEntityStrike { offset, length })
            }
            GroupCallMessageEntity::Spoiler { offset, length } => {
                tl_enums::MessageEntity::Spoiler(tl_types::MessageEntitySpoiler { offset, length })
            }
            GroupCallMessageEntity::Code { offset, length } => {
                tl_enums::MessageEntity::Code(tl_types::MessageEntityCode { offset, length })
            }
            GroupCallMessageEntity::CustomEmoji {
                offset,
                length,
                document_id,
            } => tl_enums::MessageEntity::CustomEmoji(tl_types::MessageEntityCustomEmoji {
                offset,
                length,
                document_id,
            }),
        })
        .collect()
}

/// Build `phone.sendGroupCallMessage#b1d11410`.
pub fn build_send_group_call_message(
    call: InputGroupCall,
    random_id: u64,
    text: String,
    entities: &[GroupCallMessageEntity],
    allow_paid_stars: Option<i64>,
    send_as: Option<tl_enums::InputPeer>,
) -> Result<tl_fns::phone::SendGroupCallMessage, TlConversionError> {
    Ok(tl_fns::phone::SendGroupCallMessage {
        call: call.try_into()?,
        random_id: random_id as i64,
        message: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
            text,
            entities: entities_to_tl(entities),
        }),
        allow_paid_stars: allow_paid_stars.filter(|&s| s > 0),
        send_as,
    })
}

/// Build `phone.sendGroupCallEncryptedMessage#e5afa56d`.
pub fn build_send_group_call_encrypted_message(
    call: InputGroupCall,
    encrypted_message: Vec<u8>,
) -> Result<tl_fns::phone::SendGroupCallEncryptedMessage, TlConversionError> {
    Ok(tl_fns::phone::SendGroupCallEncryptedMessage {
        call: call.try_into()?,
        encrypted_message,
    })
}

/// Build `phone.deleteGroupCallMessages#f64f54f7`.
pub fn build_delete_group_call_messages(
    call: InputGroupCall,
    messages: Vec<i32>,
    report_spam: bool,
) -> Result<tl_fns::phone::DeleteGroupCallMessages, TlConversionError> {
    Ok(tl_fns::phone::DeleteGroupCallMessages {
        report_spam,
        call: call.try_into()?,
        messages,
    })
}

/// Build `phone.deleteGroupCallParticipantMessages#1dbfeca0`.
pub fn build_delete_group_call_participant_messages(
    call: InputGroupCall,
    participant: tl_enums::InputPeer,
    report_spam: bool,
) -> Result<tl_fns::phone::DeleteGroupCallParticipantMessages, TlConversionError> {
    Ok(tl_fns::phone::DeleteGroupCallParticipantMessages {
        report_spam,
        call: call.try_into()?,
        participant,
    })
}

/// Build `phone.getGroupCallStars#6f636302`.
pub fn build_get_group_call_stars(
    call: InputGroupCall,
) -> Result<tl_fns::phone::GetGroupCallStars, TlConversionError> {
    Ok(tl_fns::phone::GetGroupCallStars {
        call: call.try_into()?,
    })
}

/// Build `phone.saveDefaultSendAs#4167add1`.
pub fn build_save_default_send_as(
    call: InputGroupCall,
    send_as: tl_enums::InputPeer,
) -> Result<tl_fns::phone::SaveDefaultSendAs, TlConversionError> {
    Ok(tl_fns::phone::SaveDefaultSendAs {
        call: call.try_into()?,
        send_as,
    })
}
