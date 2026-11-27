//! Wave 9-W CHATUI snapshots (fake data only): run with `--ignored`, `CHATUI_SNAP_DIR=<dir>` and
//! `CHATUI_SNAP_STAGE=before|after`; each test writes `<dir>/<stage>/<name>.png`.
//!
//! * `tgb3161_corner_buttons`: a scrolled-up group on the fake RPC session (the real widget and
//!   backend): a mention, "Mark all as read", then a new mention and a message.
//! * `tgb3136_previews`, `tgb3137_partial_selection`, `tgb3138_selected_emoji`: the painters
//!   with the input the old code gave them on the `before` stage (no spoiler ranges, no
//!   selection rectangles for the cover, no overlay), the new input on `after`.

use std::path::PathBuf;

use egui::{Color32, Pos2, Rect, Vec2};
use egui_kittest::{Harness, HarnessBuilder};
use tg_text::{EntityInText, EntityType, TextWithEntities, utf16_len};
use tg_ui::fonts::register_tdesktop_fonts;

use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::Palette;

fn stage_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("CHATUI_SNAP_DIR").map(PathBuf::from)?;
    let stage = std::env::var("CHATUI_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = dir.join(stage);
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    Some(dir)
}

fn after() -> bool {
    std::env::var("CHATUI_SNAP_STAGE").map_or(true, |s| s != "before")
}

fn save<S>(harness: &mut Harness<'_, S>, name: &str) {
    if let Some(dir) = stage_dir() {
        let image = harness.render().expect("render snapshot");
        image
            .save(dir.join(format!("{name}.png")))
            .expect("save snapshot");
    }
}

fn builder<S>(size: Vec2) -> HarnessBuilder<S> {
    HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(0.05)
        .with_max_steps(8)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(register_tdesktop_fonts)
}

/// `text` with a spoiler over `hidden`.
fn spoiler_text(text: &str, hidden: &str) -> TextWithEntities {
    let at = text.find(hidden).expect("hidden part is in the text");
    let mut out = TextWithEntities {
        text: text.to_owned(),
        entities: vec![EntityInText::new(
            EntityType::Spoiler,
            utf16_len(&text[..at]) as i32,
            utf16_len(hidden) as i32,
        )],
    };
    out.sort_entities();
    out
}

#[test]
#[ignore = "writes the TGB-3161 snapshot under CHATUI_SNAP_DIR"]
fn tgb3161_corner_buttons() {
    use super::tests_tgb3161_corner_buttons::{deliver, open_group_of_30};
    use super::tests_wp79_live_history::start_live_fixture;
    use tg_app_core::stand_in::strings::EnglishStrings;
    use tg_app_core::stand_in::types::PeerId;
    use tg_app_live::session::LiveSession;
    use tg_tl::Identifiable;

    struct State {
        widget: crate::history::HistoryWidget,
        live: LiveSession,
        group: PeerId,
    }
    let (_rt, rpc, live, _peer_a, group) = start_live_fixture(100);
    open_group_of_30(&rpc);
    let state = State {
        widget: crate::history::HistoryWidget::new(),
        live,
        group,
    };
    let palette = Palette::day();
    let mut harness = builder(Vec2::new(720.0, 520.0)).build_ui_state(
        move |ui, st: &mut State| {
            let _ = st.widget.show_with_backend(
                ui,
                st.group,
                &mut st.live,
                true,
                false,
                false,
                &palette,
                &EnglishStrings,
            );
        },
        state,
    );
    let steps = |h: &mut Harness<'_, State>, n: usize| {
        for _ in 0..n {
            h.step();
            assert!(
                h.state_mut()
                    .live
                    .flush_background_rpcs(std::time::Duration::from_secs(10))
            );
        }
    };
    steps(&mut harness, 3);
    harness
        .state_mut()
        .widget
        .inner_mut()
        .set_scroll_offset(0.0);
    steps(&mut harness, 3);
    // A mention, "Mark all as read" on the "@" button, then a new mention and a message.
    deliver(&rpc, &harness.state().live, 31, true);
    steps(&mut harness, 2);
    rpc.enqueue_ok(
        tg_tl::functions::messages::ReadMentions::CONSTRUCTOR_ID,
        &tg_tl::enums::messages::AffectedHistory::AffectedHistory(
            tg_tl::types::messages::AffectedHistory {
                pts: 101,
                pts_count: 0,
                offset: 0,
            },
        ),
    );
    {
        use tg_app_core::history::backend::HistoryBackend;
        let group = harness.state().group;
        harness
            .state_mut()
            .live
            .read_unread_mentions(group, None)
            .expect("messages.readMentions");
    }
    steps(&mut harness, 2);
    for (id, mentions_me) in [(32, true), (33, false)] {
        deliver(&rpc, &harness.state().live, id, mentions_me);
        steps(&mut harness, 2);
    }
    steps(&mut harness, 8);
    save(&mut harness, "tgb3161_corner_buttons");
}

#[test]
#[ignore = "writes the TGB-3136 snapshot under CHATUI_SNAP_DIR"]
fn tgb3136_previews() {
    use tg_app_core::dialogs::backend::{DialogListQuery, MessageSearchResult};
    use tg_app_core::history::backend::PinnedMessageSummary;
    use tg_app_core::stand_in::types::{FilterId, MsgId};
    use tg_app_info::dialogs::backend::DialogsBackend;
    use tg_app_info::dialogs::layout::{
        DialogColors, RowPaintState, paint_dialog_row, paint_message_search_row,
    };
    use tg_app_info::dialogs::mock::MockDialogsStore;

    let text = "the code is 4711, keep it to yourself";
    let hidden = 12..16;
    let spoilers = if after() { vec![hidden] } else { Vec::new() };
    let store = MockDialogsStore::new();
    let mut row = store
        .dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 40)
        .entries
        .into_iter()
        .find(|d| {
            d.special == tg_app_core::dialogs::backend::SpecialRowKind::None
                && d.media_preview.is_none()
                && d.draft.is_none()
                && d.typing.is_none()
        })
        .expect("a plain mock row");
    row.last_message = text.to_owned();
    row.last_message_spoilers.clone_from(&spoilers);
    row.last_sender = None;
    let result = MessageSearchResult {
        peer_id: row.peer_id,
        kind: row.kind,
        special: row.special,
        peer_title: row.title.clone(),
        color_index: row.color_index,
        msg_id: MsgId(77),
        sender_name: None,
        text: text.to_owned(),
        text_spoilers: spoilers.clone(),
        time: row.last_message_time,
        send_state: row.send_state,
        archived: false,
        media_preview: None,
    };
    let pinned = PinnedMessageSummary {
        msg_id: MsgId(77),
        sender_name: String::new(),
        preview: text.to_owned(),
        preview_spoilers: spoilers,
    };
    let now = row.last_message_time;
    let mut harness = builder(Vec2::new(420.0, 200.0)).build_ui(move |ui| {
        let palette = Palette::day();
        let dialog_colors = DialogColors::new(&palette);
        let colors = HistoryColors::from_palette(&palette);
        ui.painter().rect_filled(ui.max_rect(), 0.0, Color32::WHITE);
        let _ = paint_dialog_row(
            ui.painter(),
            Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 62.0)),
            &row,
            now,
            RowPaintState::default(),
            &palette,
            &dialog_colors,
        );
        paint_message_search_row(
            ui.painter(),
            Rect::from_min_size(Pos2::new(0.0, 66.0), Vec2::new(420.0, 62.0)),
            &result,
            "code",
            now,
            false,
            false,
            true,
            &palette,
            &dialog_colors,
        );
        let _ = crate::history::bars::paint_pinned_bar(
            ui,
            Rect::from_min_size(Pos2::new(0.0, 140.0), Vec2::new(420.0, 50.0)),
            std::slice::from_ref(&pinned),
            0,
            false,
            &colors,
        );
    });
    for _ in 0..4 {
        harness.step();
    }
    save(&mut harness, "tgb3136_previews");
}

#[test]
#[ignore = "writes the TGB-3137 snapshot under CHATUI_SNAP_DIR"]
fn tgb3137_partial_selection() {
    use crate::history::text_bridge::{
        char_range_rects, entities_to_rich_text, message_text_style,
    };
    use tg_ui::text::{PreparedRichText, render_prepared_rich_text_selected};

    let text = spoiler_text(
        "Riddle of the day: a cat on the windowsill with tea, do not peek!",
        "a cat on the windowsill with tea",
    );
    let mut prepared = PreparedRichText::new(entities_to_rich_text(&text));
    let palette = Palette::day();
    let colors = HistoryColors::from_palette(&palette);
    let mut harness = builder(Vec2::new(300.0, 90.0)).build_ui(move |ui| {
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, colors.msg_in_bg);
        let style = message_text_style();
        let text_pal = colors.text_palette(false);
        let origin = Pos2::new(10.0, 10.0);
        let wrap = 280.0;
        // Chars 10..40 are selected: the cover splits at the selection's edge.
        let galley = prepared.plain_galley(ui, &style, &text_pal, colors.text_fg, wrap);
        let selected = char_range_rects(&galley, origin, 10, 40);
        for r in &selected {
            ui.painter().rect_filled(*r, 2.0, colors.text_select_bg);
        }
        let shown: &[Rect] = if after() { &selected } else { &[] };
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(Rect::from_min_size(origin, Vec2::new(wrap, 70.0))),
            |ui| {
                let _ = render_prepared_rich_text_selected(
                    ui,
                    egui::Id::unique("snap_tgb3137"),
                    &mut prepared,
                    &style,
                    &text_pal,
                    colors.text_fg,
                    wrap,
                    Some(0.0),
                    shown,
                );
            },
        );
    });
    for _ in 0..4 {
        harness.step();
    }
    save(&mut harness, "tgb3137_partial_selection");
}

#[test]
#[ignore = "writes the TGB-3138 snapshot under CHATUI_SNAP_DIR"]
fn tgb3138_selected_emoji() {
    use crate::history::inline_media::{HistoryMediaRuntime, LottieFrameState};
    use tg_app_core::stand_in::types::MsgId;

    let json = crate::history::inline_media::tests_tgb3138_colored::circle_lottie_json();
    tg_ui::custom_emoji::register_custom_emoji_asset(
        "snap_tgb3138",
        &tg_ui::custom_emoji::CustomEmojiAsset::lottie(json.clone()),
    );
    let palette = Palette::day();
    let colors = HistoryColors::from_palette(&palette);
    let overlay = after().then_some(colors.msg_sticker_overlay);
    let mut rt = HistoryMediaRuntime::new(false);
    rt.set_background_decode(Some(false));
    let mut harness = builder(Vec2::new(300.0, 140.0)).build_ui(move |ui| {
        // The selected-message background behind both.
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, colors.canvas_bg);
        crate::history::large_emoji::paint_custom(
            ui.ctx(),
            ui.painter(),
            "snap_tgb3138",
            Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::splat(100.0)),
            colors.text_fg,
            overlay,
        );
        if let LottieFrameState::Ready { texture, .. } =
            rt.lottie_frame_colored(ui.ctx(), MsgId(5), 3138, &json, 0.0, 100, overlay)
        {
            ui.painter().image(
                texture.id(),
                Rect::from_min_size(Pos2::new(170.0, 20.0), Vec2::splat(100.0)),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    });
    for _ in 0..4 {
        harness.step();
    }
    save(&mut harness, "tgb3138_selected_emoji");
}
