//! Spoilers and collapsible quotes in message text keep their state in the history widget
//! (TGB-3009).
//!
//! - A click reveals the spoilers of that message only.
//! - Another chat hides revealed spoilers again (`Data::Session::hideShownSpoilers`). The
//!   reveal used to live in egui memory under the bare message id, so it stayed forever and a
//!   message with the same id in another chat (ids repeat across channels) showed revealed.
//! - A collapsed quote is measured with the three lines it shows, an expanded one with all of
//!   them: the bubble used to be measured from the whole text as plain spans, leaving empty
//!   space under a collapsed quote (the owner's report) and cutting off an expanded one.

use egui::epaint::TextureId;
use egui::{Rect, Shape, Vec2};
use egui_kittest::Harness;
use tg_text::{EntityInText, EntityType, utf16_len};
use tg_ui::text::layout_text_with_entities_expanded;

use super::message_text_state::{body_layout_options, ends_with_quote};
use super::tests_spoiler_snap::{
    SnapState, build, build_with, click, message, messages, painted_text_rect, text_of,
};
use super::text_bridge::message_text_style;
use crate::history::backend::HistorySlice;
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

const SIZE: Vec2 = Vec2::new(620.0, 560.0);

/// Particle quads of the spoiler cover painted inside `rect` on the last frame: quads of a
/// mesh with a texture other than the font atlas (text is drawn from the font atlas).
fn cover_quads(harness: &Harness<'static, SnapState>, rect: Rect) -> usize {
    harness
        .output()
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            Shape::Mesh(mesh) if mesh.texture_id != TextureId::default() => Some(mesh),
            _ => None,
        })
        .map(|mesh| {
            mesh.vertices
                .iter()
                .filter(|v| rect.expand(1.0).contains(v.pos))
                .count()
                / 4
        })
        .sum()
}

fn text_rect(harness: &Harness<'static, SnapState>, id: i32) -> Rect {
    painted_text_rect(&harness.state().inner, MsgId(id)).expect("message painted")
}

fn steps(harness: &mut Harness<'static, SnapState>, count: usize) {
    for _ in 0..count {
        harness.step();
    }
}

#[test]
fn a_click_reveals_the_spoilers_of_that_message_only() {
    let mut harness = build(ThemeMode::Day, SIZE);
    let (plain, outgoing) = (text_rect(&harness, 3), text_rect(&harness, 4));
    assert!(
        cover_quads(&harness, plain) > 100,
        "hidden spoiler is covered"
    );
    assert!(cover_quads(&harness, outgoing) > 50);

    // "кот на подоконнике" starts after "Загадка дня, ответ: ".
    click(&mut harness, plain.min + Vec2::new(170.0, 9.0));
    steps(&mut harness, 6);
    assert_eq!(cover_quads(&harness, text_rect(&harness, 3)), 0, "revealed");
    assert!(
        cover_quads(&harness, text_rect(&harness, 4)) > 50,
        "the other message keeps its cover"
    );
}

#[test]
fn spoilers_are_hidden_again_in_another_chat() {
    let mut harness = build(ThemeMode::Day, SIZE);
    let plain = text_rect(&harness, 3);
    click(&mut harness, plain.min + Vec2::new(170.0, 9.0));
    steps(&mut harness, 6);
    assert_eq!(cover_quads(&harness, text_rect(&harness, 3)), 0);

    // Another chat whose messages have the same ids.
    let (first, msgs) = messages();
    let other = PeerId::from_chat(88_032);
    let moved: Vec<_> = msgs
        .into_iter()
        .map(|mut m| {
            m.peer_id = other;
            m
        })
        .collect();
    let open = |harness: &mut Harness<'static, SnapState>, peer, msgs: Vec<_>| {
        let total_count = Vec::len(&msgs);
        harness.state_mut().inner.apply_slice(
            peer,
            HistorySlice {
                messages: msgs,
                loaded_at_top: true,
                loaded_at_bottom: true,
                unread_count: 0,
                first_unread_id: None,
                total_count,
            },
            PeerKind::Group,
            true,
        );
        steps(harness, 8);
    };
    open(&mut harness, other, moved);
    assert!(
        cover_quads(&harness, text_rect(&harness, 3)) > 100,
        "message 3 of the other chat is hidden"
    );

    // Back in the first chat the spoiler is hidden again too.
    let (_, msgs) = messages();
    open(&mut harness, first, msgs);
    assert!(cover_quads(&harness, text_rect(&harness, 3)) > 100);
}

/// A quote of `lines` long lines under a heading line, collapsed (expandable) when `collapsed`.
fn long_quote(lines: usize) -> tg_text::TextWithEntities {
    let head = "Заголовок перед цитатой:\n";
    let quote = (0..lines)
        .map(|i| {
            format!("Строка цитаты номер {i}, достаточно длинная, чтобы занять ширину пузыря.")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let quote_at = utf16_len(head) as i32;
    text_of(
        &[(head, &[]), (quote.as_str(), &[EntityType::Spoiler])],
        &[EntityInText::with_data(
            EntityType::Blockquote,
            quote_at,
            utf16_len(&quote) as i32,
            "1",
        )],
    )
}

/// The height the block renderer paints `text` with at `width`, with `expanded` quotes.
fn painted_height(harness: &Harness<'static, SnapState>, id: i32, expanded: &[usize]) -> f32 {
    let inner = &harness.state().inner;
    let layout = inner.cached_message_layout(MsgId(id)).expect("laid out");
    let width = layout.text_rect.width as f32;
    let colors = HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));
    let msg_text = long_quote(10);
    let laid = layout_text_with_entities_expanded(
        &harness.ctx,
        &msg_text,
        &message_text_style(),
        &colors.text_palette(false),
        &body_layout_options(colors.text_fg, width, expanded),
        expanded,
    );
    laid.size.y.ceil()
}

#[test]
fn a_collapsed_quote_is_measured_with_the_lines_it_shows_and_grows_when_expanded() {
    let peer = PeerId::from_chat(88_033);
    let text = long_quote(10);
    assert!(ends_with_quote(&text));
    let msgs = vec![message(peer, 7, 611, "Тестовый Бот", text)];
    let mut harness = build_with(ThemeMode::Day, Vec2::new(620.0, 900.0), peer, msgs);
    let date_line = 18.0;

    let collapsed = text_rect(&harness, 7);
    let shown = painted_height(&harness, 7, &[]);
    // The heading line and three quote lines with the quote's paddings: about 5 lines, not 11.
    assert!(shown < 18.0 * 6.0, "collapsed quote height {shown}");
    assert_eq!(
        collapsed.height(),
        shown + date_line,
        "the text block is the painted text and the time's own line"
    );

    // The expand toggle sits at the quote's bottom right.
    let inner_text = long_quote(10);
    let colors = HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));
    let laid = layout_text_with_entities_expanded(
        &harness.ctx,
        &inner_text,
        &message_text_style(),
        &colors.text_palette(false),
        &body_layout_options(colors.text_fg, collapsed.width(), &[]),
        &[],
    );
    let (block, toggle) = laid
        .blocks
        .iter()
        .enumerate()
        .find_map(|(i, b)| b.toggle_rect.map(|t| (i, t)))
        .expect("a collapsible quote");
    click(&mut harness, collapsed.min + toggle.center().to_vec2());
    steps(&mut harness, 4);

    let expanded = text_rect(&harness, 7);
    let shown_expanded = painted_height(&harness, 7, &[block]);
    assert!(
        shown_expanded > 18.0 * 11.0,
        "expanded quote {shown_expanded}"
    );
    assert_eq!(expanded.height(), shown_expanded + date_line);
}

#[test]
fn a_selected_message_paints_its_spoiler_in_the_selected_date_colour() {
    // TGB-3029: `inTextPaletteSelected.spoilerFg: msgInDateFgSelected` (and out).
    use crate::history::bubble::selected_text_palette;
    use egui::Color32;
    let colors = crate::history::theme::HistoryColors::from_palette(
        &tg_app_core::stand_in::theme::Palette::day(),
    );
    assert_eq!(
        selected_text_palette(&colors, false, true).spoiler_fg,
        Color32::from_rgb(0x6A, 0x9C, 0xC5)
    );
    assert_eq!(
        selected_text_palette(&colors, true, true).spoiler_fg,
        Color32::from_rgb(0x56, 0xB2, 0xA6)
    );
    assert_eq!(
        selected_text_palette(&colors, false, false).spoiler_fg,
        colors.msg_in_date_fg
    );
    assert_eq!(
        colors.text_palette(true).select_spoiler_fg,
        colors.msg_out_date_fg_selected
    );
}
