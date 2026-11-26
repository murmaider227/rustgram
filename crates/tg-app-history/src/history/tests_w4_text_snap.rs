//! Wave 4 TEXT package fixtures and evidence (TGB-2973, TGB-2965, TGB-2960). Mock data only.
//!
//! `w4_text_snapshot` writes `w4_text_bubbles.png` (inline bot on the name line, web page
//! previews without a site name or title) and `w4_text_hashtag_click.png` (the chat after a click
//! on a hashtag) into `$W4_TEXT_SNAP_DIR` when that variable is set. With
//! `$W4_TEXT_SNAP_BEFORE` set, the previews carry what the old live mapping produced (the
//! display URL as site name, the URL as title). The file uses only APIs that predate the fixes,
//! so the same snapshot renders the "before" images on the old code.

use egui::{Event, Modifiers, PointerButton, Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_text::{EntityInText, EntityType, TextWithEntities};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::backend::{
    ForwardHeader, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra,
};
use crate::history::inner::{HistoryInnerWidget, InnerEvent};
use crate::history::mock::MockHistoryStore;
use crate::history::sections_model::LinkPreviewData;
use crate::history::theme::HistoryColors;
use crate::history::{HistoryAction, HistoryWidget};
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};

const BASE_UNIX: i32 = 1_795_340_000;

/// Ids of the messages [`link_messages`] builds.
pub(crate) const HASHTAG_MSG: i32 = 900_031;
pub(crate) const CASHTAG_MSG: i32 = 900_032;
pub(crate) const COMMAND_MSG: i32 = 900_033;
pub(crate) const MENTION_MSG: i32 = 900_034;
/// User id behind the name mention of [`MENTION_MSG`].
pub(crate) const MENTIONED_USER: u64 = 4242;

/// A group the mock store knows (`MockHistoryStore::new` seeds its peer info).
pub(crate) fn group_peer() -> PeerId {
    PeerId::from_chat(2002)
}

pub(crate) fn group_info(peer: PeerId) -> HistoryPeerInfo {
    HistoryPeerInfo {
        summary: PeerSummary {
            id: peer,
            kind: PeerKind::Group,
            title: "Studio Crew".to_owned(),
            status: "4 members".to_owned(),
            username: None,
            phone_or_link: None,
            about: String::new(),
            initials: "SC".to_owned(),
            avatar_rgb: [0x5E, 0x72, 0xE4],
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

pub(crate) fn message(peer: PeerId, id: i32, from: u64, name: &str, text: &str) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: PeerId::from_user(from),
        sender_name: name.to_owned(),
        date_unix: BASE_UNIX + (id % 1000) * 60,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple(text),
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

/// A message sent through the inline bot `@pic_finder`.
pub(crate) fn via_message(peer: PeerId, id: i32, outgoing: bool) -> HistoryMessage {
    let mut msg = message(peer, id, 1002, "Ivo Marten", "sunset over the bay");
    msg.outgoing = outgoing;
    msg.extra.via_bot = Some("pic_finder".to_owned());
    msg
}

/// A message whose text is one entity of `kind` with `data`.
fn entity_message(
    peer: PeerId,
    id: i32,
    text: &str,
    kind: EntityType,
    data: &str,
) -> HistoryMessage {
    let mut msg = message(peer, id, 1001, "Mira Vance", "");
    msg.outgoing = true;
    let len = text.encode_utf16().count() as i32;
    msg.text = TextWithEntities {
        text: text.to_owned(),
        entities: vec![EntityInText::with_data(kind, 0, len, data.to_owned())],
    };
    msg
}

/// One outgoing message per clickable entity kind; each text is only the entity.
pub(crate) fn link_messages(peer: PeerId) -> Vec<HistoryMessage> {
    vec![
        entity_message(peer, HASHTAG_MSG, "#release", EntityType::Hashtag, ""),
        entity_message(peer, CASHTAG_MSG, "$TON", EntityType::Cashtag, ""),
        entity_message(peer, COMMAND_MSG, "/start", EntityType::BotCommand, ""),
        entity_message(
            peer,
            MENTION_MSG,
            "Lia Moss",
            EntityType::MentionName,
            &MENTIONED_USER.to_string(),
        ),
    ]
}

/// A real page as live maps it: `site_name` / `title` empty when the page has none, or (before
/// TGB-2960) the display URL and the URL in their place.
pub(crate) fn page_preview(
    site_name: Option<&str>,
    title: Option<&str>,
    before: bool,
) -> LinkPreviewData {
    let url = "https://example.org/notes/42";
    LinkPreviewData {
        url: url.to_owned(),
        site_name: site_name.map_or_else(
            || {
                if before {
                    "example.org/notes/42".to_owned()
                } else {
                    String::new()
                }
            },
            str::to_owned,
        ),
        title: title.map_or_else(
            || {
                if before {
                    url.to_owned()
                } else {
                    String::new()
                }
            },
            str::to_owned,
        ),
        description: "Meeting notes and the plan for the next release.".to_owned(),
        thumb_rgb: Some([0x5E, 0x72, 0xE4]),
        large_media: false,
        above_text: false,
    }
}

pub(crate) fn preview_message(peer: PeerId, id: i32, preview: LinkPreviewData) -> HistoryMessage {
    let mut msg = message(peer, id, 1003, "Noor Ilves", "https://example.org/notes/42");
    msg.extra.link_preview = Some(preview);
    msg
}

pub(crate) struct InnerState {
    pub(crate) inner: HistoryInnerWidget,
    pub(crate) store: MockHistoryStore,
    pub(crate) peer_info: HistoryPeerInfo,
    pub(crate) list_rect: Rect,
    pub(crate) colors: HistoryColors,
    pub(crate) events: Vec<InnerEvent>,
}

/// A history list of `messages` in a group chat, painted like the chat view.
pub(crate) fn inner_harness(
    messages: Vec<HistoryMessage>,
    size: Vec2,
) -> Harness<'static, InnerState> {
    let peer = group_peer();
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
    let state = InnerState {
        inner,
        store: MockHistoryStore::new(),
        peer_info: group_info(peer),
        list_rect: Rect::from_min_size(Pos2::ZERO, size),
        colors: HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day)),
        events: Vec::new(),
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
            |ui, st: &mut InnerState| {
                ui.painter()
                    .rect_filled(st.list_rect, 0.0, st.colors.canvas_bg);
                let evs = st.inner.show(
                    ui,
                    st.list_rect,
                    &st.peer_info,
                    false,
                    "",
                    &st.colors,
                    &mut st.store,
                );
                st.events.extend(evs);
            },
            state,
        );
    harness.run_steps(4);
    harness
}

pub(crate) struct ChatState {
    pub(crate) widget: HistoryWidget,
    pub(crate) store: MockHistoryStore,
    pub(crate) peer: PeerId,
    pub(crate) actions: Vec<HistoryAction>,
    palette: Palette,
}

/// The full chat view of a group holding [`link_messages`].
pub(crate) fn chat_harness(size: Vec2) -> Harness<'static, ChatState> {
    let peer = group_peer();
    let mut store = MockHistoryStore::new();
    for msg in link_messages(peer) {
        store.push_message(peer, msg);
    }
    let state = ChatState {
        widget: HistoryWidget::new(),
        store,
        peer,
        actions: Vec::new(),
        palette: Palette::day(),
    };
    let mut harness = HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(0.1)
        .with_max_steps(30)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
        })
        .build_ui_state(
            |ui, st: &mut ChatState| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        if let Some(action) = st.widget.show_with_backend(
                            ui,
                            st.peer,
                            &mut st.store,
                            true,
                            false,
                            false,
                            &st.palette,
                            &EnglishStrings,
                        ) {
                            st.actions.push(action);
                        }
                    });
            },
            state,
        );
    for _ in 0..4 {
        harness.step();
    }
    harness
}

/// A real left click (move, press, release) at `pos`.
pub(crate) fn click_at<S>(harness: &mut Harness<'_, S>, pos: Pos2) {
    harness.input_mut().events.push(Event::PointerMoved(pos));
    harness.step();
    for pressed in [true, false] {
        harness.input_mut().events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        harness.step();
    }
}

/// Clicks the first glyphs of message `id`'s text (outgoing bubbles have no name line).
pub(crate) fn click_message_text(harness: &mut Harness<'static, ChatState>, id: i32) {
    let bubble = harness
        .state()
        .widget
        .inner()
        .last_painted_bubble_rect(MsgId(id))
        .expect("message painted");
    let pos = bubble.left_top() + Vec2::new(11.0 + 8.0, 8.0 + 8.0);
    click_at(harness, pos);
    harness.step();
}

fn save(harness: &mut Harness<'_, impl Sized>, name: &str) {
    if let Some(dir) = std::env::var_os("W4_TEXT_SNAP_DIR") {
        let img = harness.render().expect("render snapshot");
        let path = std::path::PathBuf::from(dir).join(name);
        img.save(&path).expect("save snapshot");
    }
}

#[test]
fn w4_text_snapshot() {
    let before = std::env::var_os("W4_TEXT_SNAP_BEFORE").is_some();
    let peer = group_peer();
    let mut forwarded = via_message(peer, 14, false);
    forwarded.forward = Some(ForwardHeader {
        from_name: "Tavi Roan".to_owned(),
        from_peer: None,
        original_date: String::new(),
    });
    let messages = vec![
        via_message(peer, 11, false),
        via_message(peer, 12, true),
        forwarded,
        preview_message(
            peer,
            21,
            page_preview(Some("Example Notes"), Some("Release plan"), before),
        ),
        preview_message(peer, 22, page_preview(None, Some("Release plan"), before)),
        preview_message(peer, 23, page_preview(Some("Example Notes"), None, before)),
    ];
    let mut harness = inner_harness(messages, Vec2::new(560.0, 760.0));
    save(&mut harness, "w4_text_bubbles.png");

    let mut chat = chat_harness(Vec2::new(720.0, 560.0));
    click_message_text(&mut chat, HASHTAG_MSG);
    for _ in 0..4 {
        chat.step();
    }
    save(&mut chat, "w4_text_hashtag_click.png");
}
