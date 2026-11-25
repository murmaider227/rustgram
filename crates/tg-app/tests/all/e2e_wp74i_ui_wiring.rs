//! End-to-end tests and before/after screenshots for the WP-74 UI wiring (after WP-76):
//! 1. Emoji search-row mood groups read from `ChatHelpersBackend::emoji_groups()`
//!    (`messages.getEmojiGroups`), with the built-in list kept on an empty reply or an error.
//! 2. Chat-list preview line: 16 px thumbnail / type icon before the text and the blue
//!    `dialogsTextFgService` colour for media and service previews (`DialogItem.media_preview`).
//! 3. "Block user" in the chat-list menu only for users who are not contacts (`is_contact`).
//! 4. Chat-list dates: tdesktop's `< 20h` rule and unpadded `H:mm`.
//! 5. Search results: matched words in `dialogsTextFgService`, no background.

use std::sync::Mutex;

use egui::{Color32, Pos2, Rect, Vec2};
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use tg_app::chat_helpers::backend::EmojiGroupInfo;
use tg_app::chat_helpers::emoji_list_widget::{EMOJI_MOOD_GROUPS, EmojiListWidget};
use tg_app::chat_helpers::mock::MockChatHelpersBackend;
use tg_app::dialogs::backend::{
    DialogItem, DialogListQuery, DialogMediaPreview, DialogMediaPreviewKind, DialogPeerKind,
    DialogsBackend, SpecialRowKind,
};
use tg_app::dialogs::entry_menu::EntryMenuRow;
use tg_app::dialogs::inner::{BLOCK_USER_ACTION, block_user_row, dialog_context_menu_items};
use tg_app::dialogs::layout::{
    DialogColors, RowPaintState, format_localized_dialogs_date, paint_dialog_row,
    search_snippet_job,
};
use tg_app::dialogs::mini_preview::{colorized_prefix_len, preview_type_icon};
use tg_app::dialogs::mock::MockDialogsStore;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::{FilterId, PeerId};
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_emoji::EmojiTextureCache;
use tg_history::{DialogSendState, LocalDateTime};
use tg_lang::tr;
use tg_ui::fonts::{font_id_from_style, register_tdesktop_fonts};
use tg_ui::tg_style::st;
use tg_ui::widgets::menu::MenuItem;

use crate::common::{
    AppTestState, DEFAULT_MAX_FRAMES, TEST_STEP_DT, apply_deterministic_style, build_harness,
    build_widget_harness, right_click_at, settle_layout, step_frames, wait_for_menu_item,
    wait_until,
};

static WGPU_WP74I_LOCK: Mutex<()> = Mutex::new(());

const WORKSPACE_SIZE: Vec2 = Vec2::new(1280.0, 800.0);
const DIALOGS_TOP_BAR_H: f32 = 54.0;
const ROW_H: f32 = 62.0;

// ── 1. Emoji mood groups ─────────────────────────────────────────────────────────────────────

fn server_group(title: &str, emoticons: &[&str], is_premium: bool) -> EmojiGroupInfo {
    EmojiGroupInfo {
        icon_emoji_id: 5_000_000_000 + title.len() as i64,
        title: title.to_owned(),
        emoticons: emoticons.iter().map(|e| (*e).to_owned()).collect(),
        is_premium,
        is_greeting: false,
    }
}

struct EmojiState {
    widget: EmojiListWidget,
    backend: MockChatHelpersBackend,
    cache: EmojiTextureCache,
    palette: Palette,
}

fn emoji_harness(backend: MockChatHelpersBackend) -> Harness<'static, EmojiState> {
    build_widget_harness(
        EmojiState {
            widget: EmojiListWidget::new(),
            backend,
            cache: EmojiTextureCache::new(),
            palette: Palette::for_mode(ThemeMode::Day),
        },
        Vec2::new(420.0, 420.0),
        |ui, s| {
            let _ = s.widget.show(ui, &mut s.backend, &mut s.cache, &s.palette);
        },
    )
}

#[test]
fn wp74i_emoji_mood_groups_come_from_the_backend() {
    let mut backend = MockChatHelpersBackend::new();
    backend.set_emoji_groups(Ok(vec![
        server_group("Sunny", &["☀️", "🌞"], false),
        server_group("Party", &["🎉", "🥳"], false),
        server_group("Premium", &[], true),
    ]));
    let mut harness = emoji_harness(backend);
    step_frames(&mut harness, 6);

    let titles: Vec<_> = harness
        .state()
        .widget
        .mood_groups()
        .iter()
        .map(|g| g.title.clone())
        .collect();
    assert_eq!(titles, vec!["Sunny".to_owned(), "Party".to_owned()]);
    assert!(harness.query_by_label("Sunny").is_some());
    assert!(harness.query_by_label("Party").is_some());
    assert!(
        harness.query_by_label(EMOJI_MOOD_GROUPS[0].title).is_none(),
        "the built-in list is replaced by the server groups"
    );
    assert!(
        harness.query_by_label("Premium").is_none(),
        "the premium sentinel group has no emoticons to filter by"
    );
    assert_eq!(
        harness.state().backend.recorded_emoji_groups_calls(),
        1,
        "the backend is polled once when shown, not every frame"
    );

    harness.get_by_label("Sunny").click();
    step_frames(&mut harness, 3);
    assert_eq!(harness.state().widget.active_group(), Some(0));
}

#[test]
fn wp74i_emoji_mood_groups_fall_back_on_empty_or_error() {
    for reply in [Ok(Vec::new()), Err("FLOOD_WAIT_3".to_owned())] {
        let mut backend = MockChatHelpersBackend::new();
        backend.set_emoji_groups(reply);
        let mut harness = emoji_harness(backend);
        step_frames(&mut harness, 4);
        assert_eq!(
            harness.state().widget.mood_groups().len(),
            EMOJI_MOOD_GROUPS.len()
        );
        assert!(harness.query_by_label(EMOJI_MOOD_GROUPS[0].title).is_some());
        assert_eq!(harness.state().backend.recorded_emoji_groups_calls(), 1);
    }
}

// ── 2. Chat-list media preview ───────────────────────────────────────────────────────────────

/// A 40x40 stripped thumbnail of one solid colour.
fn solid_stripped_thumb(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(40, 40, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
    tg_media::image::inline::encode_stripped_thumb(&img)
}

/// A plain user row from the mock store with no badges, ticks or draft, and the given preview.
fn preview_row(text: &str, preview: Option<DialogMediaPreview>) -> DialogItem {
    let store = MockDialogsStore::new();
    let slice = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 40);
    let mut item = slice
        .entries
        .into_iter()
        .find(|d| d.kind == DialogPeerKind::User && d.special == SpecialRowKind::None)
        .expect("a user row in the mock store");
    item.last_message = text.to_owned();
    item.last_sender = None;
    item.media_preview = preview;
    item.draft = None;
    item.typing = None;
    item.send_state = DialogSendState::None;
    item.badges = Default::default();
    item.muted = false;
    item.pinned = false;
    item.pinned_order = None;
    item.verified = false;
    item.premium = false;
    item.online = false;
    item
}

struct RowsState {
    rows: Vec<DialogItem>,
    palette: Palette,
}

const ROW_W: f32 = 360.0;

fn build_rows_harness(rows: Vec<DialogItem>, mode: ThemeMode) -> Harness<'static, RowsState> {
    let height = ROW_H * rows.len() as f32;
    HarnessBuilder::default()
        .with_size(Vec2::new(ROW_W, height))
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut RowsState| {
                let colors = DialogColors::new(&st.palette);
                let painter = ui.painter().clone();
                let now = LocalDateTime::new(2026, 11, 22, 14, 30, 0);
                for (i, item) in st.rows.iter().enumerate() {
                    let rect = Rect::from_min_size(
                        Pos2::new(0.0, i as f32 * ROW_H),
                        Vec2::new(ROW_W, ROW_H),
                    );
                    let _ = paint_dialog_row(
                        &painter,
                        rect,
                        item,
                        now,
                        RowPaintState::default(),
                        &st.palette,
                        &colors,
                    );
                }
            },
            RowsState {
                rows,
                palette: Palette::for_mode(mode),
            },
        )
}

/// Pixels of the preview line of row `row` (right of the avatar, left of the badges) that
/// satisfy `pred`.
fn count_preview_pixels(
    img: &image::RgbaImage,
    row: usize,
    pred: impl Fn([u8; 3]) -> bool,
) -> usize {
    let scale = img.width() as f32 / ROW_W;
    let y0 = ((row as f32 * ROW_H + 32.0) * scale) as u32;
    let y1 = ((row as f32 * ROW_H + 56.0) * scale) as u32;
    let x0 = (70.0 * scale) as u32;
    let x1 = ((ROW_W - 40.0) * scale) as u32;
    let mut n = 0;
    for y in y0..y1.min(img.height()) {
        for x in x0..x1.min(img.width()) {
            let p = img.get_pixel(x, y).0;
            if pred([p[0], p[1], p[2]]) {
                n += 1;
            }
        }
    }
    n
}

const fn is_red(p: [u8; 3]) -> bool {
    p[0] > 180 && p[1] < 90 && p[2] < 90
}

const fn is_blue(p: [u8; 3]) -> bool {
    p[2] as i32 > p[0] as i32 + 50 && p[2] > 120
}

#[test]
fn wp74i_dialog_preview_draws_thumbnail_icon_and_service_color() {
    let _guard = WGPU_WP74I_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let photo = DialogMediaPreview {
        kind: DialogMediaPreviewKind::Photo,
        stripped_thumb: Some(solid_stripped_thumb([230, 30, 30])),
        use_service_color: true,
        has_caption: false,
    };
    let file = DialogMediaPreview {
        kind: DialogMediaPreviewKind::File,
        stripped_thumb: None,
        use_service_color: true,
        has_caption: false,
    };
    let rows = vec![
        preview_row(&tr::lng_in_dlg_photo(), Some(photo)),
        preview_row("sample_notes.txt", Some(file)),
        preview_row("Plain text message", None),
    ];
    let mut harness = build_rows_harness(rows, ThemeMode::Day);
    step_frames(&mut harness, 3);
    let img = harness.render().expect("wgpu render");

    assert!(
        count_preview_pixels(&img, 0, is_red) > 40,
        "the photo row shows its 16 px thumbnail before the text"
    );
    assert!(
        count_preview_pixels(&img, 0, is_blue) > 20,
        "a caption-less photo preview is drawn in dialogsTextFgService"
    );
    assert!(
        count_preview_pixels(&img, 1, is_blue) > 40,
        "file icon + file name are drawn in dialogsTextFgService"
    );
    assert_eq!(
        count_preview_pixels(&img, 2, is_blue),
        0,
        "plain text stays in dialogsTextFg"
    );
}

#[test]
fn wp74i_dialog_preview_rules() {
    use DialogMediaPreviewKind as K;
    let is = |kind, expected: &'static tg_ui::tg_style::structs::IconEmoji| {
        preview_type_icon(kind).is_some_and(|icon| std::ptr::eq(icon, expected))
    };
    assert!(is(K::File, &st::dialogs_mini_file_icon));
    assert!(is(K::Audio, &st::dialogs_mini_audio_icon));
    assert!(is(K::Poll, &st::dialogs_mini_poll_icon));
    assert!(is(K::Contact, &st::dialogs_mini_contact_icon));
    assert!(preview_type_icon(K::Photo).is_none());
    assert!(preview_type_icon(K::Voice).is_none());

    let with_caption = DialogMediaPreview {
        kind: K::File,
        stripped_thumb: None,
        use_service_color: false,
        has_caption: true,
    };
    let text = "notes.txt, see page 2";
    assert_eq!(
        &text[..colorized_prefix_len(Some(&with_caption), false, text)],
        "notes.txt,",
        "the media part is blue, the caption is not"
    );
    assert_eq!(colorized_prefix_len(Some(&with_caption), true, text), 0);
    assert_eq!(colorized_prefix_len(None, false, text), 0);
    let service = DialogMediaPreview {
        kind: K::Service,
        stripped_thumb: None,
        use_service_color: true,
        has_caption: false,
    };
    assert_eq!(
        colorized_prefix_len(Some(&service), false, "Channel created"),
        "Channel created".len()
    );
}

// ── 3. Block user ────────────────────────────────────────────────────────────────────────────

fn has_block_row(items: &[MenuItem]) -> Option<String> {
    items.iter().find_map(|m| match m {
        MenuItem::Action { id, label, .. } if id == BLOCK_USER_ACTION => Some(label.clone()),
        _ => None,
    })
}

#[test]
fn wp74i_block_user_only_for_non_contacts() {
    let mut harness = build_harness(AppTestState::main_workspace(), WORKSPACE_SIZE);
    settle_layout(&mut harness);
    let all = DialogListQuery::Filter(FilterId::ALL);
    let menu = |h: &Harness<'static, AppTestState>, peer: PeerId| {
        dialog_context_menu_items(peer, h.state().controller.mock_session(), all)
    };

    assert_eq!(
        has_block_row(&menu(&harness, session_peers::USER_THEO)),
        Some(tr::lng_profile_block_user()),
        "a user who is not a contact gets Block user"
    );
    assert_eq!(
        has_block_row(&menu(&harness, session_peers::USER_MIRA)),
        None,
        "contacts never get Block user in the chat list"
    );
    assert_eq!(
        has_block_row(&menu(&harness, session_peers::BOT_CATALOG)),
        Some(tr::lng_profile_block_bot())
    );
    for peer in [
        session_peers::SELF,
        session_peers::GROUP_CORE,
        session_peers::CHANNEL_DEV_LOG,
    ] {
        assert_eq!(has_block_row(&menu(&harness, peer)), None);
    }
    let theo_item = harness
        .state()
        .controller
        .mock_session()
        .dialog_by_peer(session_peers::USER_THEO)
        .expect("Theo row");
    let Some(EntryMenuRow::Item(row)) = block_user_row(&theo_item, true) else {
        panic!("blocked user row");
    };
    assert_eq!(row.label, tr::lng_profile_unblock_user());
    assert_eq!(row.icon, Some(st::menu_icon_unblock));
    let Some(EntryMenuRow::Item(row)) = block_user_row(&theo_item, false) else {
        panic!("user row");
    };
    assert_eq!(row.icon, Some(st::menu_icon_block));

    // Right-click Theo's row and pick "Block user".
    open_dialog_menu(&mut harness, session_peers::USER_THEO);
    let label = tr::lng_profile_block_user();
    wait_for_menu_item(&mut harness, &label);
    harness.get_by_label(&label).click();
    // `PeerMenuBlockUserBox`: confirm with the red "Block" button.
    let block_ok = tr::lng_blocked_list_confirm_ok();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Block user must ask with the Block box",
        |h| h.query_by_label(&block_ok).is_some(),
    );
    harness.get_by_label(&block_ok).click();
    wait_until(
        &mut harness,
        DEFAULT_MAX_FRAMES,
        "Block user must block Theo through the backend",
        |h| {
            h.state()
                .controller
                .mock_session()
                .is_peer_blocked(session_peers::USER_THEO)
        },
    );
    assert_eq!(
        has_block_row(&menu(&harness, session_peers::USER_THEO)),
        Some(tr::lng_profile_unblock_user()),
        "a blocked user gets Unblock user"
    );
}

fn dialog_row_center(harness: &Harness<'_, AppTestState>, visual_row_index: usize) -> Pos2 {
    let geo = harness
        .state()
        .controller
        .session()
        .expect("session")
        .layout();
    let x = geo.filters_width as f32 + geo.dialogs_width as f32 * 0.5;
    let y = DIALOGS_TOP_BAR_H + (visual_row_index as f32 + 0.5) * ROW_H;
    Pos2::new(x, y)
}

fn open_dialog_menu(harness: &mut Harness<'static, AppTestState>, peer: PeerId) {
    let slice = harness.state().controller.mock_session().dialog_slice(
        DialogListQuery::Filter(FilterId::ALL),
        0,
        20,
    );
    let idx = slice
        .entries
        .iter()
        .position(|d| d.peer_id == peer)
        .expect("peer in All Chats");
    let pos = dialog_row_center(harness, 1 + idx);
    right_click_at(harness, pos);
    wait_until(
        harness,
        DEFAULT_MAX_FRAMES,
        "right-clicking a dialog row must open its context menu",
        |h| {
            h.state_mut()
                .controller
                .main_widget_mut()
                .dialogs_mut()
                .inner
                .is_context_menu_open()
        },
    );
}

// ── 4. Dates ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn wp74i_dialog_dates_follow_format_dialogs_date() {
    let now = LocalDateTime::new(2026, 11, 22, 7, 0, 0);
    assert_eq!(
        format_localized_dialogs_date(LocalDateTime::new(2026, 11, 22, 6, 5, 0), now, true),
        "6:05",
        "QLocale::ShortFormat: no leading zero on the hour"
    );
    assert_eq!(
        format_localized_dialogs_date(LocalDateTime::new(2026, 11, 21, 22, 40, 0), now, true),
        "22:40",
        "yesterday 22:40 is within 20 hours of 07:00: time, not weekday"
    );
    let three_days =
        format_localized_dialogs_date(LocalDateTime::new(2026, 11, 19, 9, 0, 0), now, true);
    assert!(
        !three_days.contains(':') && !three_days.contains('.'),
        "within a week: weekday name, got {three_days}"
    );
    assert_eq!(
        format_localized_dialogs_date(LocalDateTime::new(2026, 11, 22, 6, 5, 0), now, false),
        "6:05 AM"
    );
}

// ── 5. Search highlight ──────────────────────────────────────────────────────────────────────

#[test]
fn wp74i_search_matches_use_service_color_without_background() {
    let text_fg = Color32::from_rgb(0x99, 0x99, 0x99);
    let service_fg = Color32::from_rgb(0x3e, 0x88, 0xf7);
    let job = search_snippet_job(
        "build the nightly build",
        "build",
        font_id_from_style(st::dialogs_text_font),
        text_fg,
        service_fg,
    );
    let matched: Vec<_> = job
        .sections
        .iter()
        .filter(|s| s.format.color == service_fg)
        .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
        .collect();
    assert_eq!(matched, vec!["build", "build"]);
    assert!(
        job.sections
            .iter()
            .all(|s| s.format.background == Color32::TRANSPARENT),
        "tdesktop colors matches; it draws no highlight box"
    );
}
