//! TGB-2946: a bot command row in the field autocomplete paints the bot's userpic
//! (`row.user->paintUserpicLeft`): its photo once loaded, else its empty userpic (gradient and
//! initials), not a "/" glyph on a private colour table.
//!
//! With `$TGB2946_SNAP_DIR` set, the popup is saved as `tgb2946_bot_command_row.png` there.

use egui::{Color32, Pos2, Rect, Vec2};
use egui_kittest::Harness;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_chat::UserId;
use tg_chat::autocomplete::{AutocompleteKind, AutocompleteQuery, BotCommandRow};
use tg_emoji::EmojiTextureCache;
use tg_ui::userpic::{decide_color_index, userpic_colors};

use super::{AutocompleteRows, FieldAutocompleteView};
use crate::chat_helpers::mock::MockChatHelpersBackend;

const BOT_ID: u64 = 2_946_013;
const SIZE: Vec2 = Vec2::new(420.0, 260.0);

fn row() -> BotCommandRow {
    BotCommandRow {
        user_id: UserId(BOT_ID),
        bot_username: "nova_helper_bot".to_owned(),
        bot_name: "Nova Helper".to_owned(),
        command: "start".to_owned(),
        description: "Start the bot".to_owned(),
        ephemeral: false,
        is_shortcut: false,
        with_username: false,
    }
}

fn near(a: Color32, b: Color32, tolerance: i32) -> bool {
    [(a.r(), b.r()), (a.g(), b.g()), (a.b(), b.b())]
        .iter()
        .all(|&(x, y)| (i32::from(x) - i32::from(y)).abs() <= tolerance)
}

#[test]
fn a_bot_command_row_paints_the_bots_empty_userpic() {
    let mut harness = Harness::builder().with_size(SIZE).wgpu().build_ui_state(
        |ui, view: &mut FieldAutocompleteView| {
            let palette = Palette::for_mode(ThemeMode::Day);
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, palette.window_bg);
            let ctx = ui.ctx().clone();
            let mut backend = MockChatHelpersBackend::new();
            let mut emoji_cache = EmojiTextureCache::new();
            let compose =
                Rect::from_min_size(Pos2::new(0.0, SIZE.y - 50.0), Vec2::new(SIZE.x, 50.0));
            let _ = view.show(
                &ctx,
                compose,
                "/",
                1,
                &mut backend,
                &mut emoji_cache,
                &palette,
            );
        },
        {
            let mut view = FieldAutocompleteView::new();
            view.rows = AutocompleteRows::BotCommands {
                query: AutocompleteQuery {
                    kind: AutocompleteKind::BotCommands,
                    query: "/".to_owned(),
                    from_start: true,
                    start_byte: 0,
                    end_byte: 1,
                },
                rows: vec![row()],
            };
            view
        },
    );
    harness.run_steps(30);
    let image = harness.render().expect("render autocomplete");
    if let Some(dir) = std::env::var_os("TGB2946_SNAP_DIR") {
        image
            .save(std::path::PathBuf::from(dir).join("tgb2946_bot_command_row.png"))
            .expect("save");
    }
    let colors = tg_ui::theme::DayPalette::new();
    let gradient = userpic_colors(decide_color_index(BOT_ID), &colors);
    let mut top_stop = 0;
    for px in image.pixels() {
        let c = Color32::from_rgb(px.0[0], px.0[1], px.0[2]);
        if near(c, gradient.color1, 10) {
            top_stop += 1;
        }
    }
    assert!(
        top_stop >= 8,
        "the empty userpic's gradient top ({:?}) is painted: {top_stop} px",
        gradient.color1
    );
}
