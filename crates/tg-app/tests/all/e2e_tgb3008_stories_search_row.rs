//! TGB-3008: the collapsed stories strip sits inside the chat-list search field.
//!
//! tdesktop (`Dialogs::Widget::updateControlsGeometry`) gives `Stories::List` the right end of the
//! search field as its collapsed position (`setLayoutConstraints({filterLeft + filterWidth,
//! filterTop + added}, al_right, ...)`): up to three overlapping 21 px userpics with rings, no
//! title, no "Add Story" button, no chevron and no row of its own, so the first chat row starts
//! right under the 54 px search bar. A click expands the full 77 px strip below the search bar
//! (`expandedStoriesTop = filterAreaTop + filterAreaHeight`); scrolling the chat list down by one
//! row or past its end collapses it again (`setupStories`: `_storiesExplicitExpandScrollTop +
//! dialogsRowHeight`, `position.overscroll > 0`).
//!
//! The old port drew a separate 35 px row above the search field with the userpics, the text
//! "N stories", a "+ Add Story" button and a chevron, which pushed the search field down.
//!
//! With `STORIES_SNAP_DIR` set, the tests also write crops of the chat-list column there.

use std::time::Duration;

use egui::{Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, Rect, TouchPhase, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_app::stories::AppStoriesHost;
use tg_app::window::controller::WindowController;

use crate::common::{AppTestState, apply_deterministic_style, click_at, step_frames};

const WORKSPACE: Vec2 = Vec2::new(1280.0, 800.0);
/// `st::topBarHeight - filterTop - dialogsFilter.heightMin`: the search bar below the field.
const BELOW_FIELD: f32 = 10.0;
/// `st::dialogsStoriesFull.height`.
const FULL_STRIP_H: f32 = 77.0;
/// `dialogsStories`: `left + photoLeft + photo + photoLeft + left` for one userpic.
const SMALL_SINGLE_W: f32 = 37.0;
/// `dialogsStories.shift`.
const SMALL_SHIFT: f32 = 16.0;
/// `dialogsStories`: `photoTop + photo + photoTop`.
const SMALL_H: f32 = 29.0;

struct StoriesState {
    app: AppTestState,
    stories: AppStoriesHost,
    last_time: f64,
}

impl StoriesState {
    fn new() -> Self {
        Self {
            app: AppTestState::main_workspace(),
            stories: AppStoriesHost::new(),
            last_time: 0.0,
        }
    }

    fn controller(&self) -> &WindowController {
        &self.app.controller
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let time = ui.input(|i| i.time);
        let delta = (time - self.last_time).max(0.0);
        self.last_time = time;
        if delta > 0.0 {
            tg_app::clock::advance_test_now(Duration::from_secs_f64(delta));
        }
        let palette = self.app.controller.palette();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette.window_bg))
            .show(ui, |ui| {
                self.app
                    .controller
                    .show_with_stories(ui, Some(&mut self.stories));
            });
    }
}

type StoriesHarness = Harness<'static, StoriesState>;

fn build() -> StoriesHarness {
    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(WORKSPACE)
        .with_step_dt(1.0 / 60.0)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(|ui, st: &mut StoriesState| st.show(ui), StoriesState::new());
    harness
        .state()
        .controller()
        .theme_manager()
        .apply_to_egui(&harness.ctx);
    step_frames(&mut harness, 30);
    harness
}

/// The chat-list column (left of the history), from the last layout.
fn dialogs_column(harness: &StoriesHarness) -> Rect {
    let geo = harness
        .state()
        .controller()
        .session()
        .expect("signed-in session")
        .layout();
    let left = geo.filters_width as f32;
    Rect::from_min_size(
        Pos2::new(left, 0.0),
        Vec2::new(geo.dialogs_width as f32, WORKSPACE.y),
    )
}

fn search_field(harness: &mut StoriesHarness) -> Rect {
    harness
        .state_mut()
        .app
        .controller
        .main_widget_mut()
        .dialogs_mut()
        .search_field_rect()
        .expect("the chat-list search field is shown")
}

fn strip_rect(harness: &StoriesHarness) -> Rect {
    harness
        .state()
        .stories
        .last_strip_rect()
        .expect("the mock stories strip is shown")
}

fn snapshot(harness: &mut StoriesHarness, name: &str) {
    let Some(dir) = std::env::var_os("STORIES_SNAP_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let column = dialogs_column(harness);
    let image = harness.render().expect("render snapshot");
    let ppp = image.width() as f32 / WORKSPACE.x;
    let x = (column.left() * ppp) as u32;
    let w = ((column.width() * ppp) as u32).min(image.width() - x);
    let h = ((260.0 * ppp) as u32).min(image.height());
    let crop = image::imageops::crop_imm(&image, x, 0, w, h).to_image();
    std::fs::create_dir_all(&dir).expect("create snapshot dir");
    crop.save(dir.join(format!("{name}.png")))
        .expect("save snapshot");
}

fn wheel(harness: &mut StoriesHarness, at: Pos2, dy: f32, steps: usize) {
    harness.input_mut().events.push(Event::PointerMoved(at));
    harness.step();
    for _ in 0..steps {
        harness.input_mut().events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: Vec2::new(0.0, dy),
            modifiers: Modifiers::NONE,
            phase: TouchPhase::Move,
            source: MouseWheelSource::Unknown,
        });
        harness.step();
    }
    step_frames(harness, 30);
}

#[test]
fn collapsed_stories_sit_at_the_right_end_of_the_search_field() {
    let mut harness = build();
    snapshot(&mut harness, "collapsed");
    let field = search_field(&mut harness);
    let strip = strip_rect(&harness);
    let shown = harness
        .state()
        .stories
        .strip()
        .bar_state()
        .small_visible_indices()
        .len();
    assert!((1..=3).contains(&shown), "the mock data shows 1-3 userpics");

    // Inside the search field, not a row above it.
    assert!(
        field.contains_rect(strip),
        "collapsed stories must sit inside the search field {field:?}, got {strip:?}"
    );
    assert!(
        (strip.top() - (field.top() + 3.0)).abs() < 0.5,
        "filterTop + added, got {strip:?} in {field:?}"
    );
    assert!(
        (strip.height() - SMALL_H).abs() < 0.5,
        "collapsed geometry is photoTop + photo + photoTop, got {strip:?}"
    );
    let expected_w = SMALL_SINGLE_W + SMALL_SHIFT * (shown as f32 - 1.0);
    assert!(
        (strip.width() - expected_w).abs() < 0.5,
        "{shown} overlapping userpics are {expected_w} px wide, got {strip:?}"
    );
    assert!(
        (strip.right() - field.right()).abs() < 0.5,
        "the userpics are right-aligned at the end of the search field, got {strip:?} in {field:?}"
    );

    // No title, no "Add Story" button, no chevron.
    assert!(harness.query_by_label("+ Add Story").is_none());
    assert!(
        !harness
            .state_mut()
            .app
            .controller
            .main_widget_mut()
            .dialogs_mut()
            .search_active
    );
}

#[test]
fn clicking_collapsed_stories_expands_the_full_strip_below_the_search_bar() {
    let mut harness = build();
    let field = search_field(&mut harness);
    let strip = strip_rect(&harness);
    click_at(&mut harness, strip.center());
    step_frames(&mut harness, 40);
    snapshot(&mut harness, "expanded");

    assert!(
        harness.state().stories.strip().is_expanded(),
        "a click on the collapsed userpics expands the stories"
    );
    assert!(
        !harness
            .state_mut()
            .app
            .controller
            .main_widget_mut()
            .dialogs_mut()
            .search_active,
        "the click goes to the stories, not to the search field under them"
    );
    let band = strip_rect(&harness);
    let column = dialogs_column(&harness);
    assert!(
        (band.top() - (field.bottom() + BELOW_FIELD)).abs() < 1.0,
        "the expanded strip starts right under the search bar ({field:?}), got {band:?}"
    );
    assert_eq!(
        search_field(&mut harness),
        field,
        "the search field stays where it was"
    );
    assert!(
        (band.height() - FULL_STRIP_H).abs() < 0.5,
        "the expanded strip is dialogsStoriesFull.height tall, got {band:?}"
    );
    assert!((band.width() - column.width()).abs() < 1.0);
    assert!(harness.query_by_label("+ Add Story").is_none());

    // Scrolling the chat list down (a row, or past its end: the mock list is short) collapses
    // the strip again.
    let list_point = Pos2::new(column.center().x, 400.0);
    wheel(&mut harness, list_point, -40.0, 6);
    assert!(
        !harness.state().stories.strip().is_expanded(),
        "scrolling the chat list down collapses the stories"
    );
    let collapsed = strip_rect(&harness);
    assert!(
        field.contains_rect(collapsed),
        "collapsed again inside the search field, got {collapsed:?}"
    );
}
