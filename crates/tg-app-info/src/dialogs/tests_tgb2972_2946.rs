//! TGB-2972: a chat-list row and the profile header paint the peer's custom emoji status after
//! the name (`Ui::PeerBadge`, `Info::Profile::Badge`), with the premium star until it loads.
//! TGB-2946: a forum's chat-list row and the community / delete-chat headers paint the loaded
//! photo as a rounded square (`PeerUserpicShape::Forum`).
//!
//! With `$TGB2972_SNAP_DIR` set, the rows and the header are also saved as PNGs there (mock
//! data, a synthetic emoji and photo).

use std::sync::Arc;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::Harness;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{FilterId, PeerId};
use tg_ui::custom_emoji::{CustomEmojiAsset, register_custom_emoji_asset};
use tg_ui::tg_style::st;
use tg_ui::theme::Colors;
use tg_ui::userpic_images::{UserpicImage, UserpicImageSource, install_userpic_source};

use crate::dialogs::backend::{DialogItem, DialogListQuery, DialogPeerKind, DialogsBackend};
use crate::dialogs::emoji_status::ROW_STATUS_WIDTH;
use crate::dialogs::layout::{DialogColors, RowPaintState, paint_dialog_row, row_peer_badge_width};
use crate::dialogs::mock::{MOCK_NOW, MockDialogsStore};
use crate::info::backend::InfoBackend;
use crate::info::colors::InfoColors;
use crate::info::cover::ProfileCover;
use crate::info::mock::{MockBackend, mock_peers};

/// The wgpu renders of this module run one at a time: several devices created at once crash
/// the test process on some drivers.
static RENDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn render_lock() -> std::sync::MutexGuard<'static, ()> {
    RENDER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const ROW_W: f32 = 420.0;
const ROW_H: f32 = 62.0;
/// The synthetic emoji status: a solid green square, a colour nothing else in a row uses.
const GREEN: [u8; 3] = [0x00, 0xE0, 0x40];
/// The synthetic photo: solid magenta.
const MAGENTA: [u8; 3] = [0xE0, 0x10, 0xC0];

fn green_asset() -> CustomEmojiAsset {
    let side = 40u32;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    for _ in 0..side * side {
        rgba.extend_from_slice(&[GREEN[0], GREEN[1], GREEN[2], 0xFF]);
    }
    CustomEmojiAsset::static_rgba(side, side, rgba)
}

struct MagentaPhotos;

impl UserpicImageSource for MagentaPhotos {
    fn userpic(&self, _peer_id: i64) -> Option<Arc<UserpicImage>> {
        let side = 64;
        let mut rgba = Vec::with_capacity(side * side * 4);
        for _ in 0..side * side {
            rgba.extend_from_slice(&[MAGENTA[0], MAGENTA[1], MAGENTA[2], 0xFF]);
        }
        Some(Arc::new(UserpicImage {
            photo_id: 77,
            width: side,
            height: side,
            rgba,
        }))
    }
}

fn base_row() -> DialogItem {
    let store = MockDialogsStore::new();
    let slice = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 40);
    let mut item = slice
        .entries
        .into_iter()
        .find(|d| d.special == crate::dialogs::backend::SpecialRowKind::None)
        .expect("a plain mock row");
    item.peer_id = PeerId::from_user(9_729_721);
    item.kind = DialogPeerKind::User;
    item.title = "Ada Brightwater".to_owned();
    item.verified = false;
    item.premium = true;
    item.emoji_status_id = None;
    item.muted = false;
    item.online = false;
    item.active_call = false;
    item
}

fn render_rows(rows: Vec<DialogItem>, photos: bool, png: &str) -> image::RgbaImage {
    let height = ROW_H * rows.len() as f32;
    let mut harness = Harness::builder()
        .with_size(Vec2::new(ROW_W, height))
        .with_context_setup(move |ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            if photos {
                install_userpic_source(ctx, Arc::new(MagentaPhotos));
            }
        })
        .wgpu()
        .build_ui(move |ui| {
            let palette = Palette::for_mode(ThemeMode::Day);
            let colors = DialogColors::new(&palette);
            let painter = ui.painter().clone();
            painter.rect_filled(ui.max_rect(), 0.0, palette.window_bg);
            for (i, item) in rows.iter().enumerate() {
                let rect =
                    Rect::from_min_size(Pos2::new(0.0, i as f32 * ROW_H), Vec2::new(ROW_W, ROW_H));
                let _ = paint_dialog_row(
                    &painter,
                    rect,
                    item,
                    MOCK_NOW,
                    RowPaintState::default(),
                    &palette,
                    &colors,
                );
            }
        });
    harness.run_steps(3);
    let image = harness.render().expect("render rows");
    save(&image, png);
    image
}

fn save(image: &image::RgbaImage, name: &str) {
    if let Some(dir) = std::env::var_os("TGB2972_SNAP_DIR") {
        let path = std::path::PathBuf::from(dir).join(name);
        image.save(&path).expect("save snapshot");
    }
}

fn is_near(px: &image::Rgba<u8>, rgb: [u8; 3]) -> bool {
    px.0[..3]
        .iter()
        .zip(rgb)
        .all(|(&a, b)| (i32::from(a) - i32::from(b)).abs() <= 12)
}

fn count_in(image: &image::RgbaImage, area: Rect, rgb: [u8; 3]) -> usize {
    let mut n = 0;
    for y in area.top() as u32..(area.bottom() as u32).min(image.height()) {
        for x in area.left() as u32..(area.right() as u32).min(image.width()) {
            if is_near(image.get_pixel(x, y), rgb) {
                n += 1;
            }
        }
    }
    n
}

/// The name line of row `i`: from the name's left edge to the date.
fn name_line(i: usize) -> Rect {
    let top = i as f32 * ROW_H;
    Rect::from_min_max(
        Pos2::new(60.0, top + 4.0),
        Pos2::new(ROW_W - 50.0, top + 32.0),
    )
}

#[test]
fn a_row_paints_its_loaded_emoji_status_and_the_star_until_then() {
    let _render = render_lock();
    const LOADED: u64 = 9_729_701;
    const LOADING: u64 = 9_729_702;
    assert!(register_custom_emoji_asset(
        LOADED.to_string(),
        &green_asset()
    ));

    let mut with_status = base_row();
    with_status.emoji_status_id = Some(LOADED);
    let mut loading = base_row();
    loading.emoji_status_id = Some(LOADING);
    let star_only = base_row();
    let mut verified = base_row();
    verified.verified = true;
    verified.emoji_status_id = Some(LOADED);

    assert_eq!(row_peer_badge_width(&with_status), ROW_STATUS_WIDTH);
    assert_eq!(ROW_STATUS_WIDTH, 22, "`premium->width() - 4 * skip`");
    assert_eq!(row_peer_badge_width(&star_only), 18);
    assert_eq!(row_peer_badge_width(&verified), 18, "the check wins");

    let image = render_rows(
        vec![with_status, loading, star_only, verified],
        false,
        "tgb2972_rows.png",
    );
    let status_px = count_in(&image, name_line(0), GREEN);
    // A 20 px frame; the row clips nothing of it.
    assert!(status_px >= 300, "row 0 painted {status_px} status pixels");
    assert_eq!(count_in(&image, name_line(1), GREEN), 0, "not loaded yet");
    assert_eq!(count_in(&image, name_line(2), GREEN), 0);
    assert_eq!(
        count_in(&image, name_line(3), GREEN),
        0,
        "a verified row paints only the check"
    );
    // The loading row keeps the star where the status goes (`dialogsVerifiedIconBg`).
    let palette = Palette::for_mode(ThemeMode::Day);
    let star = DialogColors::new(&palette).color("dialogsVerifiedIconBg");
    let star_rgb = [star.r(), star.g(), star.b()];
    assert!(
        count_in(&image, name_line(1), star_rgb) > 20,
        "the fallback star"
    );
}

#[test]
fn a_forum_row_paints_its_photo_as_a_rounded_square() {
    let _render = render_lock();
    let mut forum = base_row();
    forum.kind = DialogPeerKind::Forum;
    forum.premium = false;
    let mut group = base_row();
    group.kind = DialogPeerKind::Group;
    group.premium = false;
    let image = render_rows(vec![forum, group], true, "tgb2946_forum_rows.png");

    let pad = &st::default_dialog_row.padding;
    let photo = st::default_dialog_row.photo_size.0 as f32;
    let left = pad.left.0 as f32;
    let top = pad.top.0 as f32;
    // Inside the rounded square's corner (radius 0.3 * size) but outside the circle.
    let corner = |row: usize| {
        image
            .get_pixel((left + 6.0) as u32, (row as f32 * ROW_H + top + 6.0) as u32)
            .to_owned()
    };
    let middle = |row: usize| {
        image
            .get_pixel(
                (left + photo * 0.5) as u32,
                (row as f32 * ROW_H + top + photo * 0.5) as u32,
            )
            .to_owned()
    };
    assert!(is_near(&middle(0), MAGENTA), "forum photo painted");
    assert!(is_near(&middle(1), MAGENTA), "group photo painted");
    assert!(
        is_near(&corner(0), MAGENTA),
        "the forum photo fills the rounded square's corner"
    );
    assert!(!is_near(&corner(1), MAGENTA), "the group photo is a circle");
}

#[test]
fn the_profile_header_paints_the_emoji_status() {
    let _render = render_lock();
    const LOADED: u64 = 9_729_703;
    assert!(register_custom_emoji_asset(
        LOADED.to_string(),
        &green_asset()
    ));
    let backend = MockBackend::new();
    let mut peer = backend
        .peer_profile(mock_peers::USER_MIRA)
        .expect("mock profile");
    peer.verified = false;
    peer.premium = true;
    let mut star = peer.clone();
    star.emoji_status_id = None;
    peer.emoji_status_id = Some(LOADED);

    let render = |profile: crate::info::backend::PeerProfile, png: &str| {
        let colors = InfoColors::for_mode(ThemeMode::Day);
        let mut harness = Harness::builder()
            .with_size(Vec2::new(420.0, 320.0))
            .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
            .wgpu()
            .build_ui(move |ui| {
                let _ =
                    ProfileCover::show_with_buttons(ui, &profile, &colors, None, None, false, &[]);
            });
        harness.run_steps(3);
        let image = harness.render().expect("render cover");
        save(&image, png);
        image
    };
    let all = Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 320.0));
    let with_status = render(peer, "tgb2972_profile.png");
    assert!(count_in(&with_status, all, GREEN) >= 300, "status painted");
    let with_star = render(star, "tgb2972_profile_star.png");
    assert_eq!(count_in(&with_star, all, GREEN), 0);
}
