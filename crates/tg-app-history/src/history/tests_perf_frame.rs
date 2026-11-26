//! Chat view frame path (TGB-2893, package E): what one frame of an open chat costs.
//!
//! A chat with 2,000 loaded text messages is shown headless. The tests count the work a frame
//! does (text preparations, backend reads, media descriptor requests) and print timings of the
//! history frame (`measured:` lines; run with `--nocapture`).

use std::cell::Cell;
use std::time::{Duration, Instant};

use egui::{CentralPanel, Context, Frame, RawInput, Rect, pos2, vec2};
use tg_app_core::core_types::dto::inline_media::InlineMediaDescriptor;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};
use tg_text::{EntityInText, EntityType, TextWithEntities};

use crate::history::backend::{
    DraftMessage, HistoryBackend, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra,
    PinnedMessageSummary, ReactionCounter, SendMessageRequest, TypingAction,
};
use crate::history::inner::HistoryInnerWidget;
use crate::history::theme::HistoryColors;

pub(super) const PEER: PeerId = PeerId(4_200);
const SELF_ID: PeerId = PeerId(100);
pub(super) const MESSAGE_COUNT: i32 = 2_000;
pub(super) const VIEW_W: f32 = 900.0;
pub(super) const VIEW_H: f32 = 900.0;

const WORDS: [&str; 16] = [
    "layout",
    "bubble",
    "galley",
    "viewport",
    "anchor",
    "message",
    "history",
    "painter",
    "tessellate",
    "preload",
    "segment",
    "wrap",
    "cursor",
    "selection",
    "spoiler",
    "entity",
];

/// A UTF-16 entity over the `word`-th word of `text` (texts here are ASCII).
fn entity_over_word(text: &str, word: usize, kind: EntityType) -> Option<EntityInText> {
    let mut offset = 0usize;
    for (i, part) in text.split(' ').enumerate() {
        if i == word {
            return Some(match kind {
                EntityType::CustomUrl => EntityInText::with_data(
                    kind,
                    offset as i32,
                    part.len() as i32,
                    format!("https://example.org/{part}"),
                ),
                _ => EntityInText::new(kind, offset as i32, part.len() as i32),
            });
        }
        offset += part.len() + 1;
    }
    None
}

/// Text of message `id`: 12 to 150 words, with bold / italic / link / code / spoiler entities.
pub(super) fn message_text(id: i32) -> TextWithEntities {
    let words = match id % 5 {
        0 => 150,
        1 => 12,
        2 => 40,
        3 => 70,
        _ => 25,
    };
    text_of_words(id, words)
}

/// A one- or two-line text (3 to 14 words) with the same kinds of entities.
fn short_message_text(id: i32) -> TextWithEntities {
    text_of_words(id, 3 + (id as usize * 5) % 12)
}

fn text_of_words(id: i32, words: usize) -> TextWithEntities {
    let mut text = String::new();
    for w in 0..words {
        if w > 0 {
            text.push(' ');
        }
        text.push_str(WORDS[((id as usize) * 7 + w * 3) % WORDS.len()]);
    }
    let mut entities = Vec::new();
    let kinds = [
        (1, EntityType::Bold),
        (4, EntityType::Italic),
        (6, EntityType::CustomUrl),
        (9, EntityType::Code),
    ];
    for (word, kind) in kinds {
        entities.extend(entity_over_word(&text, word, kind));
    }
    if id % 7 == 0 {
        entities.extend(entity_over_word(&text, 10, EntityType::Spoiler));
    }
    if id % 11 == 0 {
        entities.extend(entity_over_word(&text, 2, EntityType::Underline));
    }
    TextWithEntities::with_entities(text, entities)
}

pub(super) fn text_message(id: i32) -> HistoryMessage {
    message_with_text(id, message_text(id))
}

fn message_with_text(id: i32, text: TextWithEntities) -> HistoryMessage {
    let outgoing = id % 3 == 0;
    HistoryMessage {
        id: MsgId(id),
        peer_id: PEER,
        from_id: if outgoing { SELF_ID } else { PEER },
        sender_name: if outgoing { "Me" } else { "Test Peer" }.to_owned(),
        date_unix: 1_700_000_000 + id * 240,
        outgoing,
        is_post: false,
        text,
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: id % 13 == 0,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

/// A backend holding one chat. It counts the reads the open chat makes and has a version that
/// only moves when the test changes the chat.
pub(super) struct CountingBackend {
    pub(super) messages: Vec<HistoryMessage>,
    pub(super) version: u64,
    /// Behave like a backend that does not track changes (the trait's default version).
    pub(super) unversioned: bool,
    pub(super) title: String,
    pub(super) peer_info_calls: Cell<usize>,
    pub(super) self_peer_calls: Cell<usize>,
    pub(super) pinned_calls: Cell<usize>,
    pub(super) descriptor_calls: Cell<usize>,
    pub(super) descriptor_ids: std::cell::RefCell<Vec<MsgId>>,
    /// Hands out pages like a server (`limit_before` / `limit_after` around the anchor)
    /// instead of the whole chat.
    pub(super) paged: bool,
    /// While set, an older page is "on the way" (`try_load_slice_before` says `Loading`).
    pub(super) older_pending: Cell<bool>,
    /// Older pages asked for with `try_load_slice_before`.
    pub(super) older_requests: Cell<usize>,
    /// Kind of the chat (a private chat unless a test makes it a group).
    pub(super) kind: PeerKind,
}

impl CountingBackend {
    pub(super) fn with_messages(messages: Vec<HistoryMessage>) -> Self {
        Self {
            messages,
            version: 1,
            unversioned: false,
            title: "Test Peer".to_owned(),
            peer_info_calls: Cell::new(0),
            self_peer_calls: Cell::new(0),
            pinned_calls: Cell::new(0),
            descriptor_calls: Cell::new(0),
            descriptor_ids: std::cell::RefCell::new(Vec::new()),
            paged: false,
            older_pending: Cell::new(false),
            older_requests: Cell::new(0),
            kind: PeerKind::User,
        }
    }

    pub(super) fn text_chat() -> Self {
        Self::with_messages((1..=MESSAGE_COUNT).map(text_message).collect())
    }

    /// A chat of short messages: many bubbles fit the viewport.
    fn short_text_chat() -> Self {
        Self::with_messages(
            (1..=MESSAGE_COUNT)
                .map(|id| message_with_text(id, short_message_text(id)))
                .collect(),
        )
    }

    pub(super) fn peer(&self) -> HistoryPeerInfo {
        HistoryPeerInfo::from_summary(PeerSummary {
            id: PEER,
            kind: self.kind,
            title: self.title.clone(),
            status: "online".to_owned(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "TP".to_owned(),
            avatar_rgb: [0x40, 0x80, 0xc0],
            muted: false,
            can_send: true,
            is_saved_messages: false,
        })
    }

    fn slice(&self, messages: Vec<HistoryMessage>) -> HistorySlice {
        let first = self.messages.first().map(|m| m.id);
        let last = self.messages.last().map(|m| m.id);
        HistorySlice {
            loaded_at_top: messages.first().map(|m| m.id) == first,
            loaded_at_bottom: messages.last().map(|m| m.id) == last,
            total_count: self.messages.len(),
            first_unread_id: None,
            unread_count: 0,
            messages,
        }
    }
}

impl HistoryBackend for CountingBackend {
    fn chat_version(&self, _peer: PeerId) -> u64 {
        if self.unversioned {
            crate::history::backend::unversioned_backend_version()
        } else {
            self.version
        }
    }

    fn self_peer(&self) -> (PeerId, String) {
        self.self_peer_calls.set(self.self_peer_calls.get() + 1);
        (SELF_ID, "Me".to_owned())
    }

    fn peer_info(&self, peer_id: PeerId) -> Option<HistoryPeerInfo> {
        self.peer_info_calls.set(self.peer_info_calls.get() + 1);
        (peer_id == PEER).then(|| self.peer())
    }

    /// Hands out the whole chat, so the view holds all 2,000 messages at once (unless `paged`).
    fn load_slice_around(
        &self,
        _peer_id: PeerId,
        around_id: MsgId,
        limit_before: usize,
        limit_after: usize,
    ) -> HistorySlice {
        if !self.paged {
            return self.slice(self.messages.clone());
        }
        let anchor = if around_id == MsgId::SHOW_AT_END || around_id == MsgId::SHOW_AT_UNREAD {
            self.messages.len().saturating_sub(1)
        } else {
            self.messages
                .partition_point(|m| m.id < around_id)
                .min(self.messages.len().saturating_sub(1))
        };
        let start = anchor.saturating_sub(limit_before);
        let end = (anchor + 1 + limit_after).min(self.messages.len());
        self.slice(self.messages[start..end].to_vec())
    }

    fn try_load_slice_before(
        &self,
        peer_id: PeerId,
        before_id: MsgId,
        limit: usize,
    ) -> crate::history::backend::Loadable<HistorySlice> {
        self.older_requests.set(self.older_requests.get() + 1);
        if self.older_pending.get() {
            return crate::history::backend::Loadable::Loading;
        }
        crate::history::backend::Loadable::Ready(self.load_slice_before(peer_id, before_id, limit))
    }

    fn load_slice_before(&self, _peer_id: PeerId, before_id: MsgId, limit: usize) -> HistorySlice {
        let end = self.messages.partition_point(|m| m.id < before_id);
        self.slice(self.messages[end.saturating_sub(limit)..end].to_vec())
    }

    fn load_slice_after(&self, _peer_id: PeerId, after_id: MsgId, limit: usize) -> HistorySlice {
        let start = self.messages.partition_point(|m| m.id <= after_id);
        let end = (start + limit).min(self.messages.len());
        self.slice(self.messages[start..end].to_vec())
    }

    fn send_message(
        &mut self,
        _peer_id: PeerId,
        _request: SendMessageRequest,
    ) -> Result<HistoryMessage, String> {
        Err("not supported".to_owned())
    }

    fn edit_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _text: TextWithEntities,
    ) -> Result<HistoryMessage, String> {
        Err("not supported".to_owned())
    }

    fn delete_messages(
        &mut self,
        _peer_id: PeerId,
        _msg_ids: &[MsgId],
        _revoke_for_all: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    fn forward_messages(
        &mut self,
        _from_peer: PeerId,
        _msg_ids: &[MsgId],
        _to_peer: PeerId,
    ) -> Result<Vec<HistoryMessage>, String> {
        Ok(Vec::new())
    }

    fn mark_history_read(&mut self, _peer_id: PeerId, _max_id: MsgId) {}

    fn send_typing(&mut self, _peer_id: PeerId, _action: TypingAction) {}

    fn toggle_reaction(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _emoji: &str,
    ) -> Result<Vec<ReactionCounter>, String> {
        Ok(Vec::new())
    }

    fn pin_message(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _unpin: bool,
    ) -> Result<Vec<PinnedMessageSummary>, String> {
        Ok(Vec::new())
    }

    fn pinned_messages(&self, _peer_id: PeerId) -> Vec<PinnedMessageSummary> {
        self.pinned_calls.set(self.pinned_calls.get() + 1);
        Vec::new()
    }

    fn vote_poll(
        &mut self,
        _peer_id: PeerId,
        _msg_id: MsgId,
        _option_indices: &[usize],
    ) -> Result<(), String> {
        Ok(())
    }

    fn save_draft(&mut self, _peer_id: PeerId, _draft: Option<DraftMessage>) {}

    fn load_draft(&self, _peer_id: PeerId) -> Option<DraftMessage> {
        None
    }

    fn dismiss_contact_status(&mut self, _peer_id: PeerId) {}

    fn add_contact(&mut self, _peer_id: PeerId) {}

    fn toggle_block_peer(&mut self, _peer_id: PeerId) {}

    fn toggle_mute(&mut self, _peer_id: PeerId) -> bool {
        false
    }

    fn inline_media_descriptor(
        &self,
        _peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<InlineMediaDescriptor> {
        self.descriptor_calls.set(self.descriptor_calls.get() + 1);
        self.descriptor_ids.borrow_mut().push(msg_id);
        None
    }
}

pub(super) fn new_context() -> Context {
    let ctx = Context::default();
    tg_ui::fonts::register_tdesktop_fonts(&ctx);
    ctx
}

pub(super) fn raw_input(time: f64) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(VIEW_W, VIEW_H))),
        time: Some(time),
        focused: true,
        ..RawInput::default()
    }
}

/// Runs one frame of the message list and returns how long the frame took.
pub(super) fn list_frame(
    ctx: &Context,
    inner: &mut HistoryInnerWidget,
    backend: &mut CountingBackend,
    input: RawInput,
    search: &str,
) -> Duration {
    let peer = backend.peer();
    let colors = HistoryColors::from_palette(&Palette::day());
    let started = Instant::now();
    let mut out = ctx.run_ui(input, |ui| {
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let rect = ui.max_rect();
            let _ = inner.show(ui, rect, &peer, false, search, &colors, backend);
        });
    });
    let took = started.elapsed();
    out.textures_delta.clear();
    took
}

pub(super) fn open_chat(
    ctx: &Context,
    backend: &mut CountingBackend,
    warm_up_frames: usize,
) -> HistoryInnerWidget {
    let mut inner = HistoryInnerWidget::new();
    inner.load_peer(PEER, MsgId::SHOW_AT_END, &*backend, PeerKind::User);
    for frame in 0..warm_up_frames {
        list_frame(ctx, &mut inner, backend, raw_input(frame as f64 / 60.0), "");
    }
    inner
}

pub(super) struct FrameStats {
    pub(super) min_us: f64,
    pub(super) median_us: f64,
    pub(super) mean_us: f64,
    pub(super) p95_us: f64,
    pub(super) max_us: f64,
}

pub(super) fn frame_stats(samples: &[Duration]) -> FrameStats {
    let mut us: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e6).collect();
    us.sort_by(f64::total_cmp);
    let pick = |q: f64| us[((us.len() - 1) as f64 * q).round() as usize];
    FrameStats {
        min_us: pick(0.0),
        median_us: pick(0.5),
        mean_us: us.iter().sum::<f64>() / us.len() as f64,
        p95_us: pick(0.95),
        max_us: pick(1.0),
    }
}

fn print_stats(label: &str, painted: usize, samples: &[Duration]) {
    let s = frame_stats(samples);
    println!(
        "measured: {label}: {} frames, {painted} bubbles painted per frame, min {:.0} us, \
         median {:.0} us, mean {:.0} us, p95 {:.0} us, max {:.0} us",
        samples.len(),
        s.min_us,
        s.median_us,
        s.mean_us,
        s.p95_us,
        s.max_us
    );
}

/// Timings of the history frame: idle at the bottom, scrolling up, and with a search filter.
/// Prints `measured:` lines; asserts nothing about time (the machine is shared).
#[test]
fn history_frame_timings_2000_messages() {
    // What a frame costs before any message is painted, to read the rows below against.
    let ctx = new_context();
    let empty: Vec<Duration> = (0..120)
        .map(|f| {
            let started = Instant::now();
            let mut out = ctx.run_ui(raw_input(f as f64 / 60.0), |ui| {
                CentralPanel::default().frame(Frame::NONE).show(ui, |_| {});
            });
            let took = started.elapsed();
            out.textures_delta.clear();
            took
        })
        .collect();
    print_stats("empty egui frame (no chat)", 0, &empty);
    let mut no_messages = CountingBackend::with_messages(Vec::new());
    let mut inner = open_chat(&ctx, &mut no_messages, 4);
    let bare: Vec<Duration> = (0..120)
        .map(|f| {
            let input = raw_input(1.0 + f as f64 / 60.0);
            list_frame(&ctx, &mut inner, &mut no_messages, input, "")
        })
        .collect();
    print_stats("chat with no messages (wallpaper, scroll area)", 0, &bare);

    println!("measured: --- long texts (12 to 150 words per message) ---");
    frame_timings(CountingBackend::text_chat());
    println!("measured: --- short texts (3 to 14 words per message) ---");
    frame_timings(CountingBackend::short_text_chat());
}

fn frame_timings(mut backend: CountingBackend) {
    let ctx = new_context();

    let open_started = Instant::now();
    let mut inner = open_chat(&ctx, &mut backend, 0);
    let before_first_frame = backend.descriptor_calls.get();
    list_frame(&ctx, &mut inner, &mut backend, raw_input(0.0), "");
    println!(
        "measured: open chat (load_peer + first frame, {MESSAGE_COUNT} messages): {:.1} ms; \
         media descriptor requests: {before_first_frame} before the first frame, {} after it",
        open_started.elapsed().as_secs_f64() * 1e3,
        backend.descriptor_calls.get()
    );
    for frame in 1..8 {
        list_frame(
            &ctx,
            &mut inner,
            &mut backend,
            raw_input(frame as f64 / 60.0),
            "",
        );
    }
    assert_eq!(inner.messages().len(), MESSAGE_COUNT as usize);

    // Several rounds so one slow round (another agent's build) does not decide the result;
    // the minimum is the figure least disturbed by other work on the machine.
    for round in 0..5 {
        let idle: Vec<Duration> = (0..120)
            .map(|f| {
                let t = 1.0 + f64::from(round) * 2.0 + f as f64 / 60.0;
                list_frame(&ctx, &mut inner, &mut backend, raw_input(t), "")
            })
            .collect();
        print_stats(
            &format!("idle frame, round {round}"),
            inner.last_frame_painted_count(),
            &idle,
        );
    }

    let filtered: Vec<Duration> = (0..60)
        .map(|f| {
            let t = 12.0 + f as f64 / 60.0;
            list_frame(&ctx, &mut inner, &mut backend, raw_input(t), "Galley")
        })
        .collect();
    print_stats(
        "idle frame with a search filter",
        inner.last_frame_painted_count(),
        &filtered,
    );

    // Scroll up 45 px per frame: new messages are laid out as they come into view.
    let mut scroll: Vec<Duration> = Vec::new();
    let mut y = inner.scroll_offset_y();
    for f in 0..120 {
        y = (y - 45.0).max(0.0);
        inner.set_scroll_offset(y);
        let t = 14.0 + f as f64 / 60.0;
        scroll.push(list_frame(&ctx, &mut inner, &mut backend, raw_input(t), ""));
    }
    print_stats(
        "scrolling frame (45 px per frame)",
        inner.last_frame_painted_count(),
        &scroll,
    );
}
