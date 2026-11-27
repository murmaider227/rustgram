//! Wave 7 stories strip parity: real userpics (TGB-3037) and the search field's "Add Story"
//! button (TGB-3038).

use std::sync::Arc;

use egui::{Color32, Context, Pos2, RawInput, Rect, Vec2};
use tg_stories::{ParsedAllStories, StoriesStore, StorySourcesList};
use tg_ui::soft_raster::{RenderOptions, render};
use tg_ui::userpic_images::{UserpicImage, UserpicImageSource, install_userpic_source};

use crate::stories::AppStoriesHost;
use crate::stories::backend::StoriesBackend;
use crate::stories::layout::{StoriesStripMetrics, collapsed_photo_center};
use crate::stories::mock::MockStoriesBackend;
use crate::stories::strip::{
    StoriesSearchSlot, StoriesStrip, StoriesStripAction, StoriesStripFrame, paint_avatar_circle,
};
use tg_app_core::stand_in::theme::Palette;

const NOW: i32 = 1_760_000_000;

/// Every peer has a solid `color` photo.
struct SolidUserpics(Color32);

impl UserpicImageSource for SolidUserpics {
    fn userpic(&self, _peer_id: i64) -> Option<Arc<UserpicImage>> {
        let [r, g, b, _] = self.0.to_array();
        Some(Arc::new(UserpicImage {
            photo_id: i64::from(r) << 16 | i64::from(g) << 8 | i64::from(b),
            width: 8,
            height: 8,
            rgba: [r, g, b, 255].repeat(64),
        }))
    }
}

fn field() -> Rect {
    Rect::from_min_size(Pos2::new(60.0, 9.0), Vec2::new(300.0, 35.0))
}

fn mock_store() -> (StoriesStore, MockStoriesBackend) {
    let mut backend = MockStoriesBackend::new(NOW);
    let mut store = StoriesStore::new(backend.self_peer_id());
    backend.bootstrap_store(&mut store, NOW);
    (store, backend)
}

/// A store whose main list was fetched and came back empty.
fn empty_known_store(backend: &MockStoriesBackend) -> StoriesStore {
    let mut store = StoriesStore::new(backend.self_peer_id());
    store.apply_all_stories(
        StorySourcesList::NotHidden,
        ParsedAllStories::All {
            has_more: false,
            count: 0,
            state: "s1".to_owned(),
            peer_stories: Vec::new(),
            stealth_mode: Default::default(),
        },
        NOW,
    );
    store
}

/// The pixel at `pos` (points, 1 px per point) of the collapsed strip painted over the field.
fn collapsed_pixel(photo: Color32, slot_index: usize) -> [u8; 4] {
    let ctx = Context::default();
    install_userpic_source(&ctx, Arc::new(SolidUserpics(photo)));
    let (store, backend) = mock_store();
    let palette = Palette::day();
    let mut strip = StoriesStrip::new();
    let options = RenderOptions {
        central_panel: false,
        ..RenderOptions::docs(400, 60, 1.0)
    };
    let frame = render(&ctx, &options, |ui| {
        strip.show_in_search_row(
            ui,
            StoriesSearchSlot::for_field(field(), &palette),
            &store,
            &backend,
            &palette,
        );
    });
    let small = strip
        .collapsed_rect(field())
        .expect("mock stories are shown collapsed");
    let center = collapsed_photo_center(&StoriesStripMetrics::default(), small, slot_index);
    frame.pixels[center.y as usize * frame.width + center.x as usize]
}

#[test]
fn collapsed_strip_paints_the_peers_photos() {
    let red = collapsed_pixel(Color32::from_rgb(255, 0, 0), 0);
    let blue = collapsed_pixel(Color32::from_rgb(0, 0, 255), 0);
    assert_ne!(red, blue, "the userpic follows the loaded photo");
    assert!(
        red[0] > red[2] + 100 && blue[2] > blue[0] + 100,
        "{red:?} {blue:?}"
    );
}

#[test]
fn userpic_falls_back_to_the_empty_userpic_and_fades_the_photo() {
    let (_, backend) = mock_store();
    let profile = backend.peer_profile(backend.self_peer_id());
    let paint = |source: Option<Color32>, opacity: f32| {
        let ctx = Context::default();
        if let Some(color) = source {
            install_userpic_source(&ctx, Arc::new(SolidUserpics(color)));
        }
        let options = RenderOptions {
            central_panel: false,
            ..RenderOptions::docs(40, 40, 1.0)
        };
        let frame = render(&ctx, &options, |ui| {
            crate::stories::strip::paint_avatar_circle_faded(
                ui.painter(),
                Pos2::new(20.0, 20.0),
                15.0,
                &profile,
                10.0,
                opacity,
            );
        });
        frame.pixels[10 * 40 + 20]
    };
    let photo = paint(Some(Color32::from_rgb(255, 0, 0)), 1.0);
    assert_eq!(photo, [255, 0, 0, 255]);
    let faded = paint(Some(Color32::from_rgb(255, 0, 0)), 0.6);
    assert!(
        faded[1] > 90 && faded[1] < 110 && faded[0] == 255,
        "{faded:?}"
    );
    let [top_r, top_g, top_b] = profile.avatar_top_rgb;
    let empty = paint(None, 1.0);
    assert_ne!(empty, [255, 255, 255, 255], "the empty userpic is painted");
    let near = |a: u8, b: u8| a.abs_diff(b) < 40;
    assert!(
        near(empty[0], top_r) && near(empty[1], top_g) && near(empty[2], top_b),
        "upper half takes the top gradient colour: {empty:?} vs {:?}",
        profile.avatar_top_rgb
    );
    let _ = paint_avatar_circle;
}

fn run_frame(
    ctx: &Context,
    strip: &mut StoriesStrip,
    store: &StoriesStore,
    backend: &MockStoriesBackend,
    events: Vec<egui::Event>,
) -> StoriesStripFrame {
    let palette = Palette::day();
    let mut out = StoriesStripFrame::default();
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 200.0))),
        events,
        ..RawInput::default()
    };
    let output = ctx.run_ui(input, |ui| {
        out = strip.show_in_search_row(
            ui,
            StoriesSearchSlot::for_field(field(), &palette),
            store,
            backend,
            &palette,
        );
    });
    let mut deltas = output.textures_delta;
    deltas.clear();
    out
}

fn click(
    ctx: &Context,
    strip: &mut StoriesStrip,
    store: &StoriesStore,
    b: &MockStoriesBackend,
    at: Pos2,
) -> Vec<StoriesStripAction> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let mut actions = Vec::new();
    for events in [
        vec![egui::Event::PointerMoved(at)],
        vec![button(true)],
        vec![button(false)],
    ] {
        actions.extend(run_frame(ctx, strip, store, b, events).actions);
    }
    actions
}

#[test]
fn add_story_button_sits_at_the_right_end_of_the_search_field_and_opens_the_composer() {
    let backend = MockStoriesBackend::new(NOW);
    let store = empty_known_store(&backend);
    let ctx = Context::default();
    let mut strip = StoriesStrip::new();
    let frame = run_frame(&ctx, &mut strip, &store, &backend, Vec::new());
    // `updateAddStoryPosition`: 26 x 26, `dialogsAddStoryRight` = 4 px from the field's right
    // edge, centred vertically.
    let expected = Rect::from_min_size(
        Pos2::new(field().right() - 4.0 - 26.0, field().center().y - 13.0),
        Vec2::splat(26.0),
    );
    assert_eq!(frame.add_story_rect, Some(expected));
    assert_eq!(frame.collapsed_rect, None);

    let actions = click(&ctx, &mut strip, &store, &backend, expected.center());
    assert!(
        actions.contains(&StoriesStripAction::AddStory),
        "{actions:?}"
    );
}

#[test]
fn add_story_button_hides_when_stories_exist_unknown_or_hidden_from_its_menu() {
    let backend = MockStoriesBackend::new(NOW);
    let ctx = Context::default();

    // Sources not fetched yet (`sourcesKnown`).
    let unknown = StoriesStore::new(backend.self_peer_id());
    let mut strip = StoriesStrip::new();
    assert_eq!(
        run_frame(&ctx, &mut strip, &unknown, &backend, Vec::new()).add_story_rect,
        None
    );

    // Hidden from the button's menu (`AddStoryButtonHidden`).
    let known = empty_known_store(&backend);
    strip.set_add_story_button_hidden(true);
    assert_eq!(
        run_frame(&ctx, &mut strip, &known, &backend, Vec::new()).add_story_rect,
        None
    );

    // The strip has stories: the collapsed userpics take the place.
    let (store, backend) = mock_store();
    let mut strip = StoriesStrip::new();
    let frame = run_frame(&ctx, &mut strip, &store, &backend, Vec::new());
    assert_eq!(frame.add_story_rect, None);
    assert!(frame.collapsed_rect.is_some());
}

#[test]
fn hidden_add_story_pref_is_loaded_once_and_saved_on_change() {
    let mut host = AppStoriesHost::new();
    let (mut persisted, mut tooltip) = (true, false);
    assert!(!host.sync_strip_prefs(&mut persisted, &mut tooltip));
    assert!(host.strip_mut().add_story_button_hidden());
    assert!(!host.strip_mut().click_tooltip_hidden());

    // "Return Story button" from the own userpic's menu.
    host.strip_mut().set_add_story_button_hidden(false);
    assert!(host.sync_strip_prefs(&mut persisted, &mut tooltip));
    assert!(!persisted);
    assert!(!host.sync_strip_prefs(&mut persisted, &mut tooltip));
    host.strip_mut().set_click_tooltip_hidden(true);
    assert!(host.sync_strip_prefs(&mut persisted, &mut tooltip));
    assert!(tooltip);
}

/// TGB-3039 item 4: a failed upload has no "Upload failed" pill; its ring turns red, fading in
/// over `kFailFadeDuration` with `EaseOutQuint`, and the source menu offers "Remove".
#[test]
fn failed_upload_ring_fades_in_like_tdesktop() {
    let (store, mut backend) = mock_store();
    let me = backend.self_peer_id();
    backend.set_pending_uploads(vec![tg_stories::StoryUploadStatus {
        peer_id: me,
        peer_name: "My Story".to_owned(),
        progress: 0.4,
        failed: true,
    }]);
    let ctx = Context::default();
    let mut strip = StoriesStrip::expanded();
    let palette = Palette::day();
    let at = |time: f64, strip: &mut StoriesStrip| {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 200.0))),
            time: Some(time),
            ..RawInput::default()
        };
        let output = ctx.run_ui(input, |ui| {
            strip.show_in_search_row(
                ui,
                StoriesSearchSlot::for_field(field(), &palette),
                &store,
                &backend,
                &palette,
            );
        });
        let mut deltas = output.textures_delta;
        deltas.clear();
    };
    at(10.0, &mut strip);
    let id = me.0 as u64;
    assert_eq!(strip.fail_fade_progress(id, 10.0), 0.0);
    let half = strip.fail_fade_progress(id, 10.0 + 0.175);
    assert!((half - (1.0 - 0.5f32.powi(5))).abs() < 1e-4, "{half}");
    assert_eq!(strip.fail_fade_progress(id, 10.35), 1.0);
    // The fade starts once: later frames do not restart it.
    at(11.0, &mut strip);
    assert_eq!(strip.fail_fade_progress(id, 11.0), 1.0);
}

/// TGB-3039 item 3: `List::computeTooltipText` with the names in semibold.
#[test]
fn click_tooltip_text_lists_the_names_like_tdesktop() {
    use tg_stories::TooltipSummary;
    let names =
        |list: &[&str]| TooltipSummary::Peers(list.iter().map(|s| (*s).to_owned()).collect());
    let job =
        crate::stories::strip::click_tooltip_job(&names(&["Ann", "Bo", "Cy"]), Color32::WHITE)
            .expect("tooltip text");
    assert_eq!(job.text, "Click here to view updates from Ann, Bo and Cy.");
    let bold: Vec<&str> = job
        .sections
        .iter()
        .filter(|s| s.format.font_id.family == tg_ui::fonts::family_semibold())
        .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
        .collect();
    assert_eq!(bold, ["Ann", "Bo", "Cy"]);
    let one = crate::stories::strip::click_tooltip_job(&names(&["Ann"]), Color32::WHITE)
        .expect("tooltip text");
    assert_eq!(one.text, "Click here to view updates from Ann.");
    let mine = crate::stories::strip::click_tooltip_job(&TooltipSummary::MineOnly, Color32::WHITE)
        .expect("tooltip text");
    assert_eq!(mine.text, "Click here to view your story.");
    assert!(
        crate::stories::strip::click_tooltip_job(&TooltipSummary::Empty, Color32::WHITE).is_none()
    );
}

/// TGB-3039 item 3: the tooltip shows under the collapsed userpics until the strip is clicked
/// (`List::mousePressEvent` calls `_tooltipHide`), then never again.
#[test]
fn click_tooltip_shows_until_the_strip_is_clicked() {
    let (store, backend) = mock_store();
    let palette = Palette::day();
    let mut strip = StoriesStrip::new();
    let ctx = Context::default();
    let options = RenderOptions {
        central_panel: false,
        ..RenderOptions::docs(400, 120, 1.0)
    };
    // A fresh context per shot: `render` restarts egui's clock at 0.
    let shot = |strip: &mut StoriesStrip| {
        render(&Context::default(), &options, |ui| {
            strip.show_in_search_row(
                ui,
                StoriesSearchSlot::for_field(field(), &palette),
                &store,
                &backend,
                &palette,
            );
        })
    };
    let small = {
        let _ = shot(&mut strip);
        strip.collapsed_rect(field()).expect("collapsed userpics")
    };
    let m = StoriesStripMetrics::default();
    let below = Pos2::new(
        small.left() + m.small_left + m.small_photo_left + m.small_photo / 2.0,
        small.bottom() + 4.0 + 3.0,
    );
    let pixel = |frame: &tg_ui::soft_raster::RgbaFrame| {
        frame.pixels[below.y as usize * frame.width + below.x as usize]
    };
    let with_tooltip = shot(&mut strip);
    assert!(pixel(&with_tooltip)[0] < 120, "{:?}", pixel(&with_tooltip));

    let actions = click(&ctx, &mut strip, &store, &backend, small.center());
    assert!(
        actions.contains(&StoriesStripAction::SetClickTooltipHidden(true)),
        "{actions:?}"
    );
    assert!(strip.click_tooltip_hidden());
    strip.set_expanded_immediate(false);
    let without = shot(&mut strip);
    assert_eq!(pixel(&without), [255, 255, 255, 255]);
}

/// TGB-3039 item 2: halfway through expanding, the first collapsed userpic is halfway between
/// its place in the search field and its cell in the full strip (`List::computeLayout` lerps).
#[test]
fn expanding_morphs_the_small_userpics_into_the_full_strip() {
    let ctx = Context::default();
    install_userpic_source(&ctx, Arc::new(SolidUserpics(Color32::from_rgb(255, 0, 0))));
    let (store, backend) = mock_store();
    let palette = Palette::day();
    let mut strip = StoriesStrip::new();
    strip.set_click_tooltip_hidden(true);
    strip.request_expanded(true);
    strip.set_anim_progress(0.5);
    let options = RenderOptions {
        central_panel: false,
        passes: 1,
        ..RenderOptions::docs(400, 160, 1.0)
    };
    // The band is allocated at the top of the column, below a 50 px search bar.
    let frame = render(&ctx, &options, |ui| {
        ui.add_space(50.0);
        strip.show_in_search_row(
            ui,
            StoriesSearchSlot::for_field(field(), &palette),
            &store,
            &backend,
            &palette,
        );
    });
    let m = StoriesStripMetrics::default();
    let small = strip.collapsed_rect(field()).expect("collapsed geometry");
    let first = strip.bar_state().small_visible_indices()[0];
    let layout = crate::stories::layout::compute_strip_layout(strip.bar_state(), &m, 400.0, 1.0);
    let full_center = Pos2::new(
        layout.left_full + (first as f32 + 0.5) * layout.single_full,
        50.0 + m.full_photo_top + m.full_photo / 2.0,
    );
    let small_center = collapsed_photo_center(&m, small, 0);
    // The frame stepped the animation on from 0.5; the morph eases it with sine in-out.
    let t = strip.anim_progress();
    assert!(t > 0.5 && t < 0.7, "{t}");
    let ratio = (0.5 - (std::f64::consts::PI * t).cos() / 2.0) as f32;
    let mid = Pos2::new(
        small_center.x + (full_center.x - small_center.x) * ratio,
        small_center.y + (full_center.y - small_center.y) * ratio,
    );
    let at = frame.pixels[mid.y as usize * frame.width + mid.x as usize];
    assert!(at[0] > 200 && at[1] < 60 && at[2] < 60, "{at:?} at {mid:?}");
}
