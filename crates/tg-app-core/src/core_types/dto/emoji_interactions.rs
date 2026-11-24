//! Pure interactive-emoji protocol DTOs, JSON codec, and timing coordinator.

use std::collections::{HashMap, HashSet};

use crate::history::backend::HistoryMessage;
use crate::stand_in::types::{MsgId, PeerId, PeerKind};

/// Minimum spacing between successive animation launches on the same message (`kMinDelay = 200ms`).
pub const EMOJI_INTERACTION_MIN_DELAY_MS: u64 = 200;

/// Outgoing tap accumulation window before emitting `sendMessageEmojiInteraction` (`kAccumulateDelay = 1000ms`).
pub const EMOJI_INTERACTION_ACCUMULATE_DELAY_MS: u64 = 1_000;

/// Throttle interval for `sendMessageEmojiInteractionSeen` (`kAccumulateSeenRequests = 1000ms`).
pub const EMOJI_INTERACTION_SEEN_THROTTLE_MS: u64 = 1_000;

/// Acceptance window for incoming `Seen` acks after our outgoing send completes (`kAcceptSeenSinceRequest = 3000ms`).
pub const EMOJI_INTERACTION_ACCEPT_SEEN_MS: u64 = 3_000;

/// Maximum staleness before dropping unstarted scheduled interactions (`kMaxDelay = 2000ms`).
pub const EMOJI_INTERACTION_MAX_DELAY_MS: u64 = 2_000;

/// Wire JSON schema version (`kJsonVersion = 1`).
pub const EMOJI_INTERACTION_JSON_VERSION: i32 = 1;

/// Max simultaneous overlay animations (`kMaxPlays = 5`).
pub const EMOJI_INTERACTION_MAX_PLAYS: usize = 5;

/// Max simultaneous overlay animations while another animation is under 200ms old (`kMaxPlaysWithSmallDelay = 3`).
pub const EMOJI_INTERACTION_MAX_PLAYS_SMALL_DELAY: usize = 3;

/// Overlay playback duration in milliseconds for procedural burst animations.
pub const EMOJI_INTERACTION_PLAY_DURATION_MS: u64 = 1_200;

/// Logical overlay box size in points for server Lottie emoji interaction effects
/// (`HistoryView::Sticker::EmojiEffectSize`).
pub const EMOJI_INTERACTION_EFFECT_SIZE_PX: f32 = 180.0;

/// Rasterization resolution in pixels for server Lottie emoji interaction frames.
pub const EMOJI_INTERACTION_LOTTIE_RENDER_PX: u32 = 160;

/// Single tap entry inside an [`EmojiInteractionsBunch`] (`EmojiInteractionsBunch::Single`).
#[derive(Debug, Clone, PartialEq)]
pub struct EmojiInteractionSingle {
    /// 1-based animation variant index (`1..=10`).
    pub index: i32,
    /// Relative start offset in seconds (`0.0..=1.0`).
    pub time: f64,
}

/// Wire payload serialized into `sendMessageEmojiInteraction.interaction` (`DataJSON`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmojiInteractionsBunch {
    /// Ordered sequence of tap animations.
    pub interactions: Vec<EmojiInteractionSingle>,
}

/// Outgoing batch ready to send via `messages.setTyping(sendMessageEmojiInteraction)`.
#[derive(Debug, Clone, PartialEq)]
pub struct OutgoingEmojiBatch {
    /// Target 1:1 user peer.
    pub peer_id: PeerId,
    /// Target single-emoji message id.
    pub msg_id: MsgId,
    /// Raw emoticon string.
    pub emoticon: String,
    /// Serialized bunch payload.
    pub bunch: EmojiInteractionsBunch,
}

/// Incoming remote tap batch received from `updateUserTyping(sendMessageEmojiInteraction)`.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingEmojiInteraction {
    /// Sender 1:1 user peer.
    pub peer_id: PeerId,
    /// Target message id in that chat.
    pub msg_id: MsgId,
    /// Emoticon string.
    pub emoticon: String,
    /// Parsed interaction bunch.
    pub bunch: EmojiInteractionsBunch,
}

/// Play request emitted to [`HistoryViewEmojiInteractions`] for on-screen animation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiInteractionPlayRequest {
    /// Peer owning the message.
    pub peer_id: PeerId,
    /// Target message id.
    pub msg_id: MsgId,
    /// Emoticon to animate.
    pub emoticon: String,
    /// 0-based animation variant index.
    pub variant_index: usize,
    /// Scheduled start time in milliseconds.
    pub should_have_started_at_ms: u64,
    /// `true` if triggered by the remote peer (requires sending `EmojiInteractionSeen`).
    pub incoming: bool,
}

/// Parses a `DataJSON` string from `sendMessageEmojiInteraction` (`EmojiInteractions::Parse`).
///
/// Mirrors tdesktop validation strictly:
/// - JSON object with `"v": 1` and non-empty `"a"` array.
/// - Any entry with `"i"` outside `1..=10` invalidates the entire bunch.
/// - Entries with `"t"` outside `0.0..=1.0` or non-increasing `"t"` are skipped.
#[must_use]
pub fn parse_interactions_json(json: &str) -> EmojiInteractionsBunch {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(json) else {
        return EmojiInteractionsBunch::default();
    };
    let Some(root) = val.as_object() else {
        return EmojiInteractionsBunch::default();
    };
    if root.get("v").and_then(serde_json::Value::as_i64)
        != Some(i64::from(EMOJI_INTERACTION_JSON_VERSION))
    {
        return EmojiInteractionsBunch::default();
    }
    let Some(actions) = root.get("a").and_then(serde_json::Value::as_array) else {
        return EmojiInteractionsBunch::default();
    };
    if actions.is_empty() {
        return EmojiInteractionsBunch::default();
    }

    let mut result = EmojiInteractionsBunch::default();
    for entry in actions {
        let Some(obj) = entry.as_object() else {
            return EmojiInteractionsBunch::default();
        };
        let Some(index_i64) = obj.get("i").and_then(serde_json::Value::as_i64) else {
            return EmojiInteractionsBunch::default();
        };
        if !(1..=10).contains(&index_i64) {
            return EmojiInteractionsBunch::default();
        }
        let time = obj
            .get("t")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(-1.0);
        if !(0.0..=1.0).contains(&time)
            || result
                .interactions
                .last()
                .is_some_and(|prev| time <= prev.time)
        {
            continue;
        }
        result.interactions.push(EmojiInteractionSingle {
            index: index_i64 as i32,
            time,
        });
    }
    result
}

/// Serializes an [`EmojiInteractionsBunch`] into compact JSON (`EmojiInteractions::ToJson`).
#[must_use]
pub fn interactions_to_json(bunch: &EmojiInteractionsBunch) -> String {
    let list: Vec<serde_json::Value> = bunch
        .interactions
        .iter()
        .map(|s| {
            serde_json::json!({
                "i": s.index,
                "t": s.time,
            })
        })
        .collect();
    serde_json::json!({
        "v": EMOJI_INTERACTION_JSON_VERSION,
        "a": list,
    })
    .to_string()
}

/// Canonical interactive emojis supported by Telegram's interactive emoji sticker pack.
const INTERACTIVE_EMOJIS: &[(&str, usize)] = &[
    ("🎆", 3),
    ("🎉", 3),
    ("🎈", 3),
    ("👍", 3),
    ("👎", 3),
    ("💩", 3),
    ("❤", 3),
    ("🔥", 3),
    ("🥳", 3),
    ("👏", 3),
    ("😂", 3),
    ("😍", 3),
    ("😢", 3),
    ("😱", 3),
    ("🎃", 3),
    ("🎅", 3),
    ("❄", 3),
    ("⚡", 3),
    ("🏆", 3),
    ("🎯", 3),
    ("💎", 3),
    ("👻", 3),
    ("🤩", 3),
    ("🤬", 3),
    ("🤯", 3),
    ("🤗", 3),
    ("🙏", 3),
    ("👌", 3),
    ("🕊", 3),
    ("🤡", 3),
    ("🥰", 3),
    ("🐳", 3),
    ("🍌", 3),
    ("💋", 3),
    ("🖕", 3),
    ("😈", 3),
    ("😴", 3),
    ("😭", 3),
    ("🤓", 3),
    ("🍾", 3),
    ("🍓", 3),
    ("💘", 3),
    ("🦄", 3),
    ("☃", 3),
    ("🎄", 3),
];

/// Colored heart emoji variants that map to the canonical red heart (`❤`) interaction
/// (`kHearts` in `stickers_emoji_pack.cpp:240-252`).
const COLORED_HEARTS: &[&str] = &[
    "\u{1F49B}", // 💛
    "\u{1F499}", // 💙
    "\u{1F49A}", // 💚
    "\u{1F49C}", // 💜
    "\u{1F9E1}", // 🧡
    "\u{1F5A4}", // 🖤
    "\u{1F90E}", // 🤎
    "\u{1F90D}", // 🤍
];

/// Normalizes an emoticon string (trimming whitespace, stripping variation selector `U+FE0F`,
/// and mapping colored hearts to `❤`) and returns its canonical interactive emoji key if
/// supported (`chooseInteractionEmoji`).
#[must_use]
pub fn choose_interaction_emoji(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 4 {
        return None;
    }
    let normalized: String = trimmed.chars().filter(|&c| c != '\u{FE0F}').collect();
    let candidate = if COLORED_HEARTS.contains(&normalized.as_str()) {
        "\u{2764}"
    } else {
        normalized.as_str()
    };
    INTERACTIVE_EMOJIS
        .iter()
        .find(|(key, _)| *key == candidate)
        .map(|(key, _)| *key)
}

/// Returns the number of interactive animation variants for `canonical_emoji`.
#[must_use]
pub fn animation_variant_count(canonical_emoji: &str) -> usize {
    INTERACTIVE_EMOJIS
        .iter()
        .find(|(key, _)| *key == canonical_emoji)
        .map_or(0, |(_, count)| *count)
}

/// Returns `true` if `msg` in a chat of `peer_kind` is eligible for interactive emoji taps
/// (`CanPlayEmojiInteraction` + `item->isRegular() && peer->isUser()`).
#[must_use]
pub fn can_play_emoji_interaction_message(
    msg: &HistoryMessage,
    peer_kind: PeerKind,
) -> Option<&'static str> {
    if !matches!(peer_kind, PeerKind::User)
        || msg.is_service()
        || msg.id.0 <= 0
        || msg.media.is_some()
        || msg.extra.rich_message.is_some()
    {
        return None;
    }
    choose_interaction_emoji(&msg.text.text)
}

#[derive(Debug, Clone)]
struct QueuedAnimation {
    peer_id: PeerId,
    emoticon: String,
    canonical_emoji: String,
    scheduled_at_ms: u64,
    started_at_ms: Option<u64>,
    incoming: bool,
    index: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct PlaySentState {
    last_done_ms: Option<u64>,
    in_flight: bool,
}

/// Session-level coordinator for interactive emoji tap synchronization (`ChatHelpers::EmojiInteractions`).
#[derive(Debug, Default)]
pub struct EmojiInteractionCoordinator {
    outgoing: HashMap<(PeerId, MsgId), Vec<QueuedAnimation>>,
    incoming: HashMap<(PeerId, MsgId), Vec<QueuedAnimation>>,
    autoplay: HashMap<(PeerId, MsgId), Vec<QueuedAnimation>>,
    autoplayed_msgs: HashSet<(PeerId, MsgId)>,
    play_started_seen_at: HashMap<(PeerId, String), u64>,
    plays_sent: HashMap<(PeerId, String), PlaySentState>,
    rng_state: u64,
}

fn canonical_emoji_for_variants(
    emoticon: &str,
    server_variants: Option<usize>,
) -> Option<(String, usize)> {
    if let Some(count) = server_variants
        && count > 0
    {
        let canonical = choose_interaction_emoji(emoticon)
            .map(str::to_owned)
            .or_else(|| {
                crate::core_types::emoji_interaction::normalize_interaction_emoticon(emoticon)
            })?;
        return Some((canonical, count));
    }
    let canonical = choose_interaction_emoji(emoticon)?;
    let count = animation_variant_count(canonical);
    (count > 0).then(|| (canonical.to_owned(), count))
}

impl EmojiInteractionCoordinator {
    /// Creates a new coordinator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rng_state: 0x9E37_79B9_7F4A_7C15,
            ..Self::default()
        }
    }

    fn next_index(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            return 0;
        }
        // xorshift64*
        let mut x = self.rng_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng_state = x;
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D)) as usize) % bound
    }

    /// Queues an outgoing interactive emoji tap (`EmojiInteractions::startOutgoing`).
    pub fn start_outgoing(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoticon: &str,
        now_ms: u64,
    ) -> Vec<EmojiInteractionPlayRequest> {
        self.start_outgoing_with_variants(peer_id, msg_id, emoticon, None, now_ms)
    }

    /// Queues an outgoing interactive emoji tap using `server_variants` from `EmojiStickersPack`
    /// when available, falling back to the built-in interactive emoji table otherwise.
    pub fn start_outgoing_with_variants(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoticon: &str,
        server_variants: Option<usize>,
        now_ms: u64,
    ) -> Vec<EmojiInteractionPlayRequest> {
        let Some((canonical, variants)) = canonical_emoji_for_variants(emoticon, server_variants)
        else {
            return Vec::new();
        };
        let prev_index = {
            let list = self.outgoing.entry((peer_id, msg_id)).or_default();
            if list
                .first()
                .is_some_and(|first| first.canonical_emoji != canonical)
            {
                list.clear();
            }
            list.last().map(|a| a.index)
        };
        let chosen = if let Some(prev) = prev_index
            && variants > 1
        {
            let mut idx = self.next_index(variants - 1);
            if idx >= prev {
                idx += 1;
            }
            idx
        } else {
            self.next_index(variants)
        };
        self.outgoing
            .entry((peer_id, msg_id))
            .or_default()
            .push(QueuedAnimation {
                peer_id,
                emoticon: emoticon.to_owned(),
                canonical_emoji: canonical,
                scheduled_at_ms: now_ms,
                started_at_ms: None,
                incoming: false,
                index: chosen,
            });
        self.check_animations(now_ms)
    }

    /// Triggers one-time autoplay for a freshly received interactive emoji message
    /// (`EmojiInteractions::startAutoplay`).
    pub fn start_autoplay(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoticon: &str,
        now_ms: u64,
    ) -> Vec<EmojiInteractionPlayRequest> {
        self.start_autoplay_with_variants(peer_id, msg_id, emoticon, None, now_ms)
    }

    /// Triggers one-time autoplay using `server_variants` from `EmojiStickersPack` when available.
    pub fn start_autoplay_with_variants(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoticon: &str,
        server_variants: Option<usize>,
        now_ms: u64,
    ) -> Vec<EmojiInteractionPlayRequest> {
        let Some((canonical, variants)) = canonical_emoji_for_variants(emoticon, server_variants)
        else {
            return Vec::new();
        };
        if !self.autoplayed_msgs.insert((peer_id, msg_id)) {
            return Vec::new();
        }
        let idx = self.next_index(variants);
        let list = self.autoplay.entry((peer_id, msg_id)).or_default();
        if !list.is_empty() {
            return Vec::new();
        }
        list.push(QueuedAnimation {
            peer_id,
            emoticon: emoticon.to_owned(),
            canonical_emoji: canonical,
            scheduled_at_ms: now_ms,
            started_at_ms: None,
            incoming: false,
            index: idx,
        });
        self.check_animations(now_ms)
    }

    /// Schedules an incoming remote interaction bunch (`EmojiInteractions::startIncoming`).
    pub fn start_incoming(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoticon: &str,
        bunch: &EmojiInteractionsBunch,
        now_ms: u64,
    ) -> Vec<EmojiInteractionPlayRequest> {
        self.start_incoming_with_variants(peer_id, msg_id, emoticon, bunch, None, now_ms)
    }

    /// Schedules an incoming remote interaction bunch using `server_variants` from
    /// `EmojiStickersPack` when available.
    pub fn start_incoming_with_variants(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        emoticon: &str,
        bunch: &EmojiInteractionsBunch,
        server_variants: Option<usize>,
        now_ms: u64,
    ) -> Vec<EmojiInteractionPlayRequest> {
        if bunch.interactions.is_empty() {
            return Vec::new();
        }
        let Some((canonical, variants)) = canonical_emoji_for_variants(emoticon, server_variants)
        else {
            return Vec::new();
        };
        let list = self.incoming.entry((peer_id, msg_id)).or_default();
        if list
            .first()
            .is_some_and(|first| first.canonical_emoji != canonical)
        {
            list.clear();
        }
        for single in &bunch.interactions {
            let at = now_ms.saturating_add((single.time * 1000.0).round() as u64);
            if list.last().is_some_and(|prev| prev.scheduled_at_ms >= at) {
                continue;
            }
            let idx = single.index.saturating_sub(1) as usize;
            if idx < variants {
                list.push(QueuedAnimation {
                    peer_id,
                    emoticon: emoticon.to_owned(),
                    canonical_emoji: canonical.clone(),
                    scheduled_at_ms: at,
                    started_at_ms: None,
                    incoming: true,
                    index: idx,
                });
            }
        }
        if list.is_empty() {
            self.incoming.remove(&(peer_id, msg_id));
            return Vec::new();
        }
        self.check_animations(now_ms)
    }

    /// Advances animation scheduling and returns any play requests ready to start at `now_ms`.
    pub fn check_animations(&mut self, now_ms: u64) -> Vec<EmojiInteractionPlayRequest> {
        let mut requests = Vec::new();
        Self::check_map(now_ms, &mut self.outgoing, &mut requests);
        Self::check_map(now_ms, &mut self.incoming, &mut requests);
        Self::check_map(now_ms, &mut self.autoplay, &mut requests);
        requests
    }

    fn check_map(
        now_ms: u64,
        map: &mut HashMap<(PeerId, MsgId), Vec<QueuedAnimation>>,
        out: &mut Vec<EmojiInteractionPlayRequest>,
    ) {
        let mut to_remove = Vec::new();
        for (&(peer_id, msg_id), animations) in map.iter_mut() {
            // Drop expired unstarted requests (`scheduledAt + kMaxDelay <= now`).
            if let Some(pos) = animations.iter().position(|a| {
                a.started_at_ms.is_none()
                    && a.scheduled_at_ms
                        .saturating_add(EMOJI_INTERACTION_MAX_DELAY_MS)
                        <= now_ms
            }) {
                if pos == 0 {
                    to_remove.push((peer_id, msg_id));
                    continue;
                }
                animations.truncate(pos);
            }

            let mut last_started: Option<u64> = None;
            for anim in animations.iter_mut() {
                if let Some(s) = anim.started_at_ms {
                    last_started = Some(s);
                } else if anim.scheduled_at_ms <= now_ms
                    && last_started.is_none_or(|ls| {
                        ls.saturating_add(EMOJI_INTERACTION_MIN_DELAY_MS) <= now_ms
                    })
                {
                    anim.started_at_ms = Some(now_ms);
                    out.push(EmojiInteractionPlayRequest {
                        peer_id: anim.peer_id,
                        msg_id,
                        emoticon: anim.emoticon.clone(),
                        variant_index: anim.index,
                        should_have_started_at_ms: anim.scheduled_at_ms,
                        incoming: anim.incoming,
                    });
                    break;
                } else {
                    break;
                }
            }
        }
        for key in to_remove {
            map.remove(&key);
        }
    }

    /// Drains any outgoing tap batches whose 1-second accumulation window has elapsed
    /// (`EmojiInteractions::sendAccumulatedOutgoing`), and cleans expired incoming/autoplay queues.
    pub fn poll_outgoing_batches(&mut self, now_ms: u64) -> Vec<OutgoingEmojiBatch> {
        let mut batches = Vec::new();
        let mut empty_keys = Vec::new();

        for (&(peer_id, msg_id), animations) in self.outgoing.iter_mut() {
            let Some(first_started) = animations.first().and_then(|a| a.started_at_ms) else {
                continue;
            };
            let interval_end = first_started.saturating_add(EMOJI_INTERACTION_ACCUMULATE_DELAY_MS);
            if interval_end > now_ms {
                continue;
            }
            let till = animations
                .iter()
                .position(|a| a.started_at_ms.is_none_or(|s| s >= interval_end))
                .unwrap_or(animations.len());
            if till > 0 {
                let emoticon = animations[0].emoticon.clone();
                let canonical = animations[0].canonical_emoji.clone();
                let mut bunch = EmojiInteractionsBunch::default();
                for anim in &animations[..till] {
                    if let Some(started) = anim.started_at_ms {
                        bunch.interactions.push(EmojiInteractionSingle {
                            index: (anim.index + 1) as i32,
                            time: (started.saturating_sub(first_started)) as f64 / 1000.0,
                        });
                    }
                }
                if !bunch.interactions.is_empty() {
                    self.plays_sent.insert(
                        (peer_id, canonical),
                        PlaySentState {
                            last_done_ms: Some(now_ms),
                            in_flight: false,
                        },
                    );
                    batches.push(OutgoingEmojiBatch {
                        peer_id,
                        msg_id,
                        emoticon,
                        bunch,
                    });
                }
                animations.drain(..till);
            }
            if animations.is_empty() {
                empty_keys.push((peer_id, msg_id));
            }
        }
        for key in empty_keys {
            self.outgoing.remove(&key);
        }

        // Prune finished incoming / autoplay animations past `kMinDelay`.
        Self::prune_started(&mut self.incoming, now_ms);
        Self::prune_started(&mut self.autoplay, now_ms);
        self.prune_seen_windows(now_ms);

        batches
    }

    fn prune_started(map: &mut HashMap<(PeerId, MsgId), Vec<QueuedAnimation>>, now_ms: u64) {
        map.retain(|_, animations| {
            let till = animations
                .iter()
                .position(|a| {
                    a.started_at_ms
                        .is_none_or(|s| s.saturating_add(EMOJI_INTERACTION_MIN_DELAY_MS) > now_ms)
                })
                .unwrap_or(animations.len());
            animations.drain(..till);
            !animations.is_empty()
        });
    }

    fn prune_seen_windows(&mut self, now_ms: u64) {
        self.play_started_seen_at
            .retain(|_, ts| ts.saturating_add(EMOJI_INTERACTION_SEEN_THROTTLE_MS) > now_ms);
        self.plays_sent.retain(|_, state| {
            state.in_flight
                || state
                    .last_done_ms
                    .is_none_or(|ts| ts.saturating_add(EMOJI_INTERACTION_ACCEPT_SEEN_MS) > now_ms)
        });
    }

    /// Returns `true` if a `sendMessageEmojiInteractionSeen` ack should be sent for `(peer_id, emoticon)`
    /// (`EmojiInteractions::playStarted`, throttled to 1 second).
    pub fn should_send_play_started(
        &mut self,
        peer_id: PeerId,
        emoticon: &str,
        now_ms: u64,
    ) -> bool {
        let key = (peer_id, emoticon.to_owned());
        if let Some(&prev) = self.play_started_seen_at.get(&key)
            && now_ms.saturating_sub(prev) < EMOJI_INTERACTION_SEEN_THROTTLE_MS
        {
            return false;
        }
        self.play_started_seen_at.insert(key, now_ms);
        true
    }

    /// Validates an incoming `sendMessageEmojiInteractionSeen` update against our recently sent
    /// interaction plays (`EmojiInteractions::seenOutgoing`).
    ///
    /// Returns `true` if the UI should show the `"watching your animation"` banner.
    pub fn accept_seen_outgoing(&self, peer_id: PeerId, emoticon: &str, now_ms: u64) -> bool {
        let Some(canonical) = choose_interaction_emoji(emoticon)
            .map(str::to_owned)
            .or_else(|| {
                crate::core_types::emoji_interaction::normalize_interaction_emoticon(emoticon)
            })
        else {
            return false;
        };
        let Some(state) = self.plays_sent.get(&(peer_id, canonical)) else {
            return false;
        };
        match state.last_done_ms {
            None => state.in_flight,
            Some(done) => done.saturating_add(EMOJI_INTERACTION_ACCEPT_SEEN_MS) > now_ms,
        }
    }

    /// Clears queued interactions when a message is destroyed or edited.
    pub fn clear_message(&mut self, peer_id: PeerId, msg_id: MsgId) {
        let key = (peer_id, msg_id);
        self.outgoing.remove(&key);
        self.incoming.remove(&key);
        self.autoplay.remove(&key);
    }
}
