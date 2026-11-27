//! Screenshot harness for spoilers in message text (TGB-3009, mock data).
//!
//! Writes `spoiler_<theme>_<state>.png` (`hidden` / `revealed`) into `$SPOILER_SNAP_DIR` when
//! that variable is set; otherwise it only renders the frames (a smoke test for the painter).
//! The messages cover the shapes of the owner's report: a spoiler inside an expandable quote
//! after a line with a mention, a spoiler inside a plain quote next to plain words, and
//! spoilers in plain incoming and outgoing text.

use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_text::{EntityInText, EntityType, TextWithEntities, utf16_len};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra};
use crate::history::inner::HistoryInnerWidget;
use crate::history::mock::MockHistoryStore;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;

pub(crate) struct SnapState {
    pub(crate) inner: HistoryInnerWidget,
    store: MockHistoryStore,
    peer_info: HistoryPeerInfo,
    list_rect: Rect,
    colors: HistoryColors,
}

impl SnapState {
    pub(crate) fn new(
        inner: HistoryInnerWidget,
        store: MockHistoryStore,
        peer_info: HistoryPeerInfo,
        list_rect: Rect,
        colors: HistoryColors,
    ) -> Self {
        Self {
            inner,
            store,
            peer_info,
            list_rect,
            colors,
        }
    }

    /// One frame of the history list over the chat background.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        ui.painter()
            .rect_filled(self.list_rect, 0.0, self.colors.canvas_bg);
        let _ = self.inner.show(
            ui,
            self.list_rect,
            &self.peer_info,
            false,
            "",
            &self.colors,
            &mut self.store,
        );
    }
}

fn peer_info(peer: PeerId) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::Group,
            title: "Snapshot".to_owned(),
            status: String::new(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "TG".to_owned(),
            avatar_rgb: [0x6C, 0x9F, 0xE0],
            muted: false,
            can_send: true,
            is_saved_messages: false,
        },
        typing_text: None,
        contact_status: None,
        unread_count: 0,
        can_pin: false,
        can_delete: false,
        link_base: String::new(),
    }
}

/// Builds a text from `(part, entity types)` pieces; each piece's entities cover it whole.
pub(crate) fn text_of(parts: &[(&str, &[EntityType])], extra: &[EntityInText]) -> TextWithEntities {
    let mut text = String::new();
    let mut entities = Vec::new();
    for (part, kinds) in parts {
        let offset = utf16_len(&text) as i32;
        let length = utf16_len(part) as i32;
        for kind in *kinds {
            entities.push(EntityInText::new(*kind, offset, length));
        }
        text.push_str(part);
    }
    entities.extend(extra.iter().cloned());
    let mut out = TextWithEntities { text, entities };
    out.sort_entities();
    out
}

pub(crate) fn message(
    peer: PeerId,
    id: i32,
    from: i64,
    name: &str,
    text: TextWithEntities,
) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: PeerId(from),
        sender_name: name.to_owned(),
        date_unix: BASE_UNIX + id * 20,
        outgoing: false,
        is_post: false,
        text,
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

/// The owner's shape: a line with a mention, then an expandable quote that is all spoiler.
pub(crate) fn quoted_spoiler_text() -> TextWithEntities {
    let head = "Послание от Тестовый Бот для ";
    let mention = "@sample_user";
    let colon = ":\n";
    // Long enough that the collapsed quote hides most of it, like the owner's message.
    let quote = concat!(
        "На фото тестовая открытка с котом: он сидит на подоконнике, рядом чашка чая и ",
        "клетчатый плед. Свет мягкий, без вспышки, фон слегка размыт, на стекле видны ",
        "капли дождя, а за окном горят фонари. На подоконнике стоят два цветочных горшка, ",
        "в одном из них кактус, в другом фиалка. Кот смотрит на улицу и, кажется, следит ",
        "за птицей на ветке. На стене висит календарь с осенним пейзажем, а на столе ",
        "лежит раскрытая книга и очки в тонкой оправе. Вся сцена выглядит уютной и ",
        "спокойной, как воскресное утро."
    );
    let quote_at = utf16_len(head) + utf16_len(mention) + utf16_len(colon);
    text_of(
        &[
            (head, &[]),
            (mention, &[EntityType::Mention]),
            (colon, &[]),
            (quote, &[EntityType::Spoiler]),
        ],
        &[EntityInText::with_data(
            EntityType::Blockquote,
            quote_at as i32,
            utf16_len(quote) as i32,
            "1",
        )],
    )
}

pub(crate) fn messages() -> (PeerId, Vec<HistoryMessage>) {
    let peer = PeerId::from_chat(88_031);
    let quoted = message(peer, 1, 601, "Тестовый Бот", quoted_spoiler_text());
    let quote_part = {
        let before = "Короткая цитата: ";
        let hidden = "ответ спрятан";
        let after = " и дальше обычный текст.";
        let all = format!("{before}{hidden}{after}");
        text_of(
            &[
                (before, &[]),
                (hidden, &[EntityType::Spoiler]),
                (after, &[]),
            ],
            &[EntityInText::new(
                EntityType::Blockquote,
                0,
                utf16_len(&all) as i32,
            )],
        )
    };
    let quote_part = message(peer, 2, 602, "Тестовая Оля", quote_part);
    let plain = message(
        peer,
        3,
        602,
        "Тестовая Оля",
        text_of(
            &[
                ("Загадка дня, ответ: ", &[]),
                ("кот на подоконнике", &[EntityType::Spoiler]),
                (" — не подсматривай!", &[]),
            ],
            &[],
        ),
    );
    let mut outgoing = message(
        peer,
        4,
        603,
        "Я",
        text_of(
            &[
                ("Мой ответ: ", &[]),
                ("плед и чай", &[EntityType::Spoiler]),
                (", угадала?", &[]),
            ],
            &[],
        ),
    );
    outgoing.outgoing = true;
    (peer, vec![quoted, quote_part, plain, outgoing])
}

pub(crate) fn click(harness: &mut Harness<'static, SnapState>, pos: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(pos));
    harness.step();
    harness.input_mut().events.push(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    harness.input_mut().events.push(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    // Far enough apart that no two clicks make a double click.
    for _ in 0..10 {
        harness.step();
    }
}

/// Where the text of `id` was painted on the last frame.
pub(crate) fn painted_text_rect(inner: &HistoryInnerWidget, id: MsgId) -> Option<Rect> {
    let bubble = inner.last_painted_bubble_rect(id)?;
    let layout = inner.cached_message_layout(id)?;
    let dx = (layout.text_rect.x - layout.geometry.x) as f32;
    let dy = (layout.text_rect.y - layout.geometry.y) as f32;
    Some(Rect::from_min_size(
        bubble.min + Vec2::new(dx, dy),
        Vec2::new(
            layout.text_rect.width as f32,
            layout.text_rect.height as f32,
        ),
    ))
}

pub(crate) fn build(mode: ThemeMode, size: Vec2) -> Harness<'static, SnapState> {
    let (peer, messages) = messages();
    build_with(mode, size, peer, messages)
}

/// A group chat `peer` showing `messages` (all loaded, scrolled to the bottom).
pub(crate) fn build_with(
    mode: ThemeMode,
    size: Vec2,
    peer: PeerId,
    messages: Vec<HistoryMessage>,
) -> Harness<'static, SnapState> {
    let mut inner = HistoryInnerWidget::new();
    let total_count = messages.len();
    inner.apply_slice(
        peer,
        HistorySlice {
            messages,
            loaded_at_top: true,
            loaded_at_bottom: true,
            unread_count: 0,
            first_unread_id: None,
            total_count,
        },
        PeerKind::Group,
        true,
    );
    let state = SnapState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: peer_info(peer),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors: HistoryColors::from_palette(&Palette::for_mode(mode)),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut SnapState| {
                ui.painter()
                    .rect_filled(st.list_rect, 0.0, st.colors.canvas_bg);
                let _ = st.inner.show(
                    ui,
                    st.list_rect,
                    &st.peer_info,
                    false,
                    "",
                    &st.colors,
                    &mut st.store,
                );
            },
            state,
        );
    for _ in 0..6 {
        harness.step();
    }
    harness
}

/// Clicks the spoiler of each message: the quote's first line in message 1 and 2, the
/// spoiler words in messages 3 and 4 (their x is measured from the text start).
fn reveal_all(harness: &mut Harness<'static, SnapState>) {
    let line = 18.0;
    let targets: [(i32, Vec2); 4] = [
        (1, Vec2::new(40.0, line + 4.0 + 12.0)),
        (2, Vec2::new(150.0, 12.0)),
        (3, Vec2::new(170.0, 9.0)),
        (4, Vec2::new(100.0, 9.0)),
    ];
    for (id, offset) in targets {
        if let Some(rect) = painted_text_rect(&harness.state().inner, MsgId(id)) {
            click(harness, rect.min + offset);
        }
    }
    harness
        .input_mut()
        .events
        .push(Event::PointerMoved(Pos2::new(2.0, 2.0)));
    for _ in 0..12 {
        harness.step();
    }
}

#[test]
fn spoilers_hidden_and_revealed_snapshot() {
    let dir = std::env::var_os("SPOILER_SNAP_DIR").map(std::path::PathBuf::from);
    for (tag, mode) in [("day", ThemeMode::Day), ("night", ThemeMode::Night)] {
        let mut harness = build(mode, Vec2::new(620.0, 720.0));
        let hidden = dir
            .as_ref()
            .map(|_| harness.render().expect("render hidden"));
        reveal_all(&mut harness);
        let revealed = dir
            .as_ref()
            .map(|_| harness.render().expect("render revealed"));
        if let Some(ref dir) = dir {
            std::fs::create_dir_all(dir).expect("create snap dir");
            for (state, img) in [("hidden", hidden), ("revealed", revealed)] {
                if let Some(img) = img {
                    img.save(dir.join(format!("spoiler_{tag}_{state}.png")))
                        .expect("save snapshot");
                }
            }
        }
    }
}
