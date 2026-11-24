//! Adaptive layout modes, column thresholds, and persisted column width ratios.
//!
//! Port origin: `Telegram/SourceFiles/window/window_adaptive.h`,
//! `Telegram/SourceFiles/window/window_adaptive.cpp`,
//! `Telegram/SourceFiles/window/window_columns.h`,
//! `Telegram/SourceFiles/window/window_columns.cpp`,
//! `Telegram/SourceFiles/window/window_session_controller.cpp` (`computeColumnLayout`,
//! `shrinkDialogsAndThirdColumns`, `countDialogsWidthFromRatio`, `enoughSpaceForFilters`),
//! `Telegram/SourceFiles/mainwidget.cpp` (`updateWindowAdaptiveLayout`),
//! and `Telegram/SourceFiles/window/window.style`.

use tg_app_core::stand_in::settings::SavedWindowColumns;

// Exact layout constants from `Telegram/SourceFiles/window/window.style`,
// `dialogs/dialogs.style`, `ui/chat/chat.style`, and `core/core_settings.h`.

pub use tg_app_core::core_types::window_types::{
    ADAPTIVE_CHAT_WIDE_WIDTH, COLUMN_MAX_WIDTH_LEFT, COLUMN_MAX_WIDTH_THIRD, COLUMN_MIN_WIDTH_LEFT,
    COLUMN_MIN_WIDTH_MAIN, COLUMN_MIN_WIDTH_THIRD, ChatLayout, Column, ColumnLayout,
    DEFAULT_DIALOGS_WIDTH_RATIO, DEFAULT_THIRD_COLUMN_WIDTH, DIALOGS_SMALL_COLUMN_WIDTH,
    HISTORY_RESIZE_WIDTH, WINDOW_BIG_DEFAULT_HEIGHT, WINDOW_BIG_DEFAULT_WIDTH,
    WINDOW_DEFAULT_HEIGHT, WINDOW_DEFAULT_WIDTH, WINDOW_FILTERS_WIDTH, WINDOW_MIN_HEIGHT,
    WINDOW_MIN_WIDTH, WindowLayout,
};

/// Reactive holder of the current adaptive window and chat layout (`Window::Adaptive`).
#[derive(Debug, Clone, Default)]
pub struct Adaptive {
    window_layout: WindowLayout,
    chat_layout: ChatLayout,
    adaptive_for_wide: bool,
}

impl Adaptive {
    /// Creates a new `Adaptive` state with wide-chat support enabled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            window_layout: WindowLayout::Normal,
            chat_layout: ChatLayout::Normal,
            adaptive_for_wide: true,
        }
    }

    /// Updates the current window column layout.
    pub fn set_window_layout(&mut self, value: WindowLayout) {
        self.window_layout = value;
    }

    /// Updates the current chat layout.
    pub fn set_chat_layout(&mut self, value: ChatLayout) {
        self.chat_layout = value;
    }

    /// Returns the active window layout mode.
    #[must_use]
    pub const fn window_layout(&self) -> WindowLayout {
        self.window_layout
    }

    /// Returns the active chat layout mode.
    #[must_use]
    pub const fn chat_layout(&self) -> ChatLayout {
        self.chat_layout
    }

    /// Returns `true` if in single-column mode.
    #[must_use]
    pub const fn is_one_column(&self) -> bool {
        matches!(self.window_layout, WindowLayout::OneColumn)
    }

    /// Returns `true` if in two-column normal mode.
    #[must_use]
    pub const fn is_normal(&self) -> bool {
        matches!(self.window_layout, WindowLayout::Normal)
    }

    /// Returns `true` if in three-column mode.
    #[must_use]
    pub const fn is_three_column(&self) -> bool {
        matches!(self.window_layout, WindowLayout::ThreeColumn)
    }

    /// Returns `true` if wide chat layout is active (`chatWideValue`).
    #[must_use]
    pub const fn is_chat_wide(&self) -> bool {
        self.adaptive_for_wide && matches!(self.chat_layout, ChatLayout::Wide)
    }
}

/// Manages dialogs width ratios and third-column toggles (`Window::Columns`).
#[derive(Debug, Clone)]
pub struct WindowColumnsState {
    state: SavedWindowColumns,
    dialogs_width_set_to_zero_without_chat: bool,
    tabbed_replaced_with_info: bool,
    dirty: bool,
}

impl Default for WindowColumnsState {
    fn default() -> Self {
        Self::from_saved(SavedWindowColumns::default())
    }
}

impl WindowColumnsState {
    /// Initializes column state from persisted settings.
    #[must_use]
    pub fn from_saved(state: SavedWindowColumns) -> Self {
        Self {
            state,
            dialogs_width_set_to_zero_without_chat: false,
            tabbed_replaced_with_info: false,
            dirty: false,
        }
    }

    /// Returns a snapshot suitable for saving to settings.
    #[must_use]
    pub const fn saved(&self) -> SavedWindowColumns {
        self.state
    }

    /// Consumes and returns whether column settings changed since last save.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Port of `Window::Columns::updateDialogsWidthRatio(ratio, nochat)`.
    pub fn update_dialogs_width_ratio(&mut self, ratio: f64, nochat: bool) {
        let with_chat = self.state.dialogs_with_chat_width_ratio;
        let change_with_chat =
            !nochat || (with_chat > 0.0) || self.dialogs_width_set_to_zero_without_chat;
        let changed_with_chat = change_with_chat && (with_chat - ratio).abs() > f64::EPSILON;

        let change_no_chat = nochat || (with_chat - ratio).abs() > f64::EPSILON;
        let changed_no_chat =
            change_no_chat && (self.state.dialogs_no_chat_width_ratio - ratio).abs() > f64::EPSILON;

        if !changed_with_chat && !changed_no_chat {
            return;
        }
        if changed_with_chat {
            self.dialogs_width_set_to_zero_without_chat = nochat && ratio <= 0.0;
            self.state.dialogs_with_chat_width_ratio = ratio;
        }
        if changed_no_chat {
            self.state.dialogs_no_chat_width_ratio = ratio;
        }
        self.dirty = true;
    }

    /// Port of `Window::Columns::dialogsWidthRatio(nochat)`.
    #[must_use]
    pub fn dialogs_width_ratio(&self, nochat: bool) -> f64 {
        let with_chat = self.state.dialogs_with_chat_width_ratio;
        if !nochat || with_chat > 0.0 {
            with_chat
        } else {
            self.state.dialogs_no_chat_width_ratio
        }
    }

    /// Returns whether the tabbed selector occupies the third column.
    #[must_use]
    pub const fn tabbed_selector_section_enabled(&self) -> bool {
        self.state.tabbed_selector_section_enabled
    }

    /// Enables or disables the tabbed selector third column (mutually exclusive with info).
    pub fn set_tabbed_selector_section_enabled(&mut self, enabled: bool) {
        let info_changed = enabled && self.state.third_section_info_enabled;
        if info_changed || self.state.tabbed_selector_section_enabled != enabled {
            self.state.tabbed_selector_section_enabled = enabled;
            if info_changed {
                self.state.third_section_info_enabled = false;
            }
            self.dirty = true;
        }
        self.tabbed_replaced_with_info = false;
    }

    /// Returns whether peer info is enabled in the third column.
    #[must_use]
    pub const fn third_section_info_enabled(&self) -> bool {
        self.state.third_section_info_enabled
    }

    /// Enables or disables the peer info third column (mutually exclusive with tabbed selector).
    pub fn set_third_section_info_enabled(&mut self, enabled: bool) {
        if self.state.third_section_info_enabled != enabled {
            self.state.third_section_info_enabled = enabled;
            if enabled {
                self.state.tabbed_selector_section_enabled = false;
            }
            self.tabbed_replaced_with_info = false;
            self.dirty = true;
        }
    }

    /// Returns the stored third-column width before clamping.
    #[must_use]
    pub const fn third_column_width(&self) -> i32 {
        self.state.third_column_width
    }

    /// Updates the stored third-column width.
    pub fn set_third_column_width(&mut self, width: i32) {
        if self.state.third_column_width != width {
            self.state.third_column_width = width;
            self.dirty = true;
        }
    }

    /// Clamps and persists the third-column width after a drag gesture finishes.
    pub fn finish_third_column_resize(&mut self) {
        let clamped = self
            .state
            .third_column_width
            .clamp(COLUMN_MIN_WIDTH_THIRD, COLUMN_MAX_WIDTH_THIRD);
        self.set_third_column_width(clamped);
    }

    /// Returns the saved window extension delta for the third column.
    #[must_use]
    pub const fn third_section_extended_by(&self) -> i32 {
        self.state.third_section_extended_by
    }

    /// Sets the saved window extension delta for the third column.
    pub fn set_third_section_extended_by(&mut self, saved_value: i32) {
        if self.state.third_section_extended_by != saved_value {
            self.state.third_section_extended_by = saved_value;
            self.dirty = true;
        }
    }
}

/// Inputs required to run tdesktop's adaptive column layout calculation.
#[derive(Debug, Clone, Copy)]
pub struct LayoutInputs {
    /// Total inner window width in logical pixels.
    pub window_width: i32,
    /// Whether this window owns a dialogs list (always `true` for the primary window).
    pub has_dialogs: bool,
    /// Whether the user enabled the vertical chat folders sidebar.
    pub filters_enabled: bool,
    /// Whether a main chat or section is currently shown.
    pub main_section_shown: bool,
    /// Whether dialogs are temporarily forced wide (e.g. search active).
    pub chats_force_display_wide: bool,
    /// Guard flag while extending window for third column.
    pub extending_for_third_section: bool,
}

/// Minimum body width required for `WindowLayout::Normal` (260 + 380 = 640 px).
#[must_use]
pub const fn minimal_normal_width() -> i32 {
    COLUMN_MIN_WIDTH_LEFT + COLUMN_MIN_WIDTH_MAIN
}

/// Minimum body width required for `WindowLayout::ThreeColumn` (260 + 380 + 292 = 932 px).
#[must_use]
pub const fn minimal_three_column_width(has_dialogs: bool) -> i32 {
    (if has_dialogs {
        COLUMN_MIN_WIDTH_LEFT
    } else {
        0
    }) + COLUMN_MIN_WIDTH_MAIN
        + COLUMN_MIN_WIDTH_THIRD
}

/// Port of `SessionController::enoughSpaceForFilters()`.
#[must_use]
pub const fn enough_space_for_filters(window_width: i32) -> bool {
    window_width >= WINDOW_MIN_WIDTH + WINDOW_FILTERS_WIDTH
}

/// Port of `SessionController::countDialogsWidthFromRatio()`.
#[must_use]
pub fn count_dialogs_width_from_ratio(
    body_width: i32,
    has_dialogs: bool,
    nochat: bool,
    columns: &WindowColumnsState,
) -> i32 {
    if !has_dialogs {
        return 0;
    }
    let ratio = columns.dialogs_width_ratio(nochat);
    let raw = (f64::from(body_width) * ratio).round() as i32;
    raw.clamp(COLUMN_MIN_WIDTH_LEFT, COLUMN_MAX_WIDTH_LEFT)
}

/// Port of `SessionController::countThirdColumnWidthFromRatio()`.
#[must_use]
pub fn count_third_column_width(columns: &WindowColumnsState) -> i32 {
    columns
        .third_column_width()
        .clamp(COLUMN_MIN_WIDTH_THIRD, COLUMN_MAX_WIDTH_THIRD)
}

/// Port of `SessionController::shrinkDialogsAndThirdColumns()`.
#[must_use]
pub fn shrink_dialogs_and_third_columns(
    dialogs_width: i32,
    third_width: i32,
    body_width: i32,
    has_dialogs: bool,
) -> (i32, i32) {
    let chat_width = COLUMN_MIN_WIDTH_MAIN;
    if dialogs_width + third_width + chat_width <= body_width {
        return (dialogs_width, third_width);
    }
    let denom = (dialogs_width + third_width).max(1);
    let available = (body_width - chat_width).max(0);
    let mut third_new = (available * third_width) / denom;
    let mut dialogs_new = (available * dialogs_width) / denom;
    if third_new < COLUMN_MIN_WIDTH_THIRD {
        third_new = COLUMN_MIN_WIDTH_THIRD;
        dialogs_new = body_width - third_new - chat_width;
    } else if has_dialogs && dialogs_new < COLUMN_MIN_WIDTH_LEFT {
        dialogs_new = COLUMN_MIN_WIDTH_LEFT;
        third_new = body_width - dialogs_new - chat_width;
    }
    (dialogs_new, third_new)
}

/// Computes the exact column geometry and adaptive layout mode.
///
/// Mirrors `SessionController::computeColumnLayout()` combined with
/// `MainWidget::updateWindowAdaptiveLayout()`.
#[must_use]
pub fn compute_column_layout(inputs: LayoutInputs, columns: &WindowColumnsState) -> ColumnLayout {
    let filters_width = if inputs.filters_enabled && enough_space_for_filters(inputs.window_width) {
        WINDOW_FILTERS_WIDTH
    } else {
        0
    };
    let body_width = (inputs.window_width - filters_width).max(0);
    let nochat = !inputs.main_section_shown;
    let dialogs_ratio = columns.dialogs_width_ratio(nochat);

    let use_one_column = inputs.has_dialogs && body_width < minimal_normal_width();
    let use_normal = body_width < minimal_three_column_width(inputs.has_dialogs)
        || inputs.extending_for_third_section
        || (!columns.tabbed_selector_section_enabled() && !columns.third_section_info_enabled());

    let (window_layout, mut dialogs_width, mut chat_width, third_width) = if use_one_column {
        (WindowLayout::OneColumn, body_width, body_width, 0)
    } else if use_normal {
        let mut d = count_dialogs_width_from_ratio(body_width, inputs.has_dialogs, nochat, columns);
        d = d.min((body_width - COLUMN_MIN_WIDTH_MAIN).max(0));
        (WindowLayout::Normal, d, body_width - d, 0)
    } else {
        let d_raw = count_dialogs_width_from_ratio(body_width, inputs.has_dialogs, nochat, columns);
        let t_raw = count_third_column_width(columns);
        let (d, t) = shrink_dialogs_and_third_columns(d_raw, t_raw, body_width, inputs.has_dialogs);
        (WindowLayout::ThreeColumn, d, body_width - d - t, t)
    };

    // Compact avatar-only dialogs rail (`MainWidget::updateWindowAdaptiveLayout`).
    let use_small_column = inputs.has_dialogs
        && !matches!(window_layout, WindowLayout::OneColumn)
        && dialogs_ratio <= 0.0
        && !inputs.chats_force_display_wide;

    if use_small_column {
        dialogs_width = DIALOGS_SMALL_COLUMN_WIDTH;
        chat_width = (body_width - dialogs_width - third_width).max(0);
    }

    let chat_layout = if chat_width >= ADAPTIVE_CHAT_WIDE_WIDTH {
        ChatLayout::Wide
    } else {
        ChatLayout::Normal
    };

    ColumnLayout {
        filters_width,
        body_width,
        dialogs_width,
        chat_width,
        third_width,
        window_layout,
        chat_layout,
        dialogs_small_column: use_small_column,
    }
}

/// Converts a dragged dialogs divider X offset (relative to body left) into the new width ratio.
///
/// Matches `MainWidget::ensureFirstColumnResizeAreaCreated`: dragging narrower than
/// `COLUMN_MIN_WIDTH_LEFT / 2` (130 px) snaps ratio to `0.0` (compact 66px avatar rail).
#[must_use]
pub fn dialogs_drag_to_ratio(new_width: i32, body_width: i32) -> f64 {
    if body_width <= 0 {
        return DEFAULT_DIALOGS_WIDTH_RATIO;
    }
    if new_width < COLUMN_MIN_WIDTH_LEFT / 2 {
        0.0
    } else {
        let max_allowed = (body_width - COLUMN_MIN_WIDTH_MAIN)
            .clamp(COLUMN_MIN_WIDTH_LEFT, COLUMN_MAX_WIDTH_LEFT);
        let clamped = new_width.clamp(COLUMN_MIN_WIDTH_LEFT, max_allowed);
        f64::from(clamped) / f64::from(body_width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_inputs(window_width: i32) -> LayoutInputs {
        LayoutInputs {
            window_width,
            has_dialogs: true,
            filters_enabled: false,
            main_section_shown: true,
            chats_force_display_wide: false,
            extending_for_third_section: false,
        }
    }

    #[test]
    fn one_column_below_640_normal_at_640() {
        let cols = WindowColumnsState::default();
        let narrow = compute_column_layout(base_inputs(639), &cols);
        assert_eq!(narrow.window_layout, WindowLayout::OneColumn);
        assert_eq!(narrow.dialogs_width, 639);
        assert_eq!(narrow.chat_width, 639);
        assert_eq!(narrow.third_width, 0);

        let normal = compute_column_layout(base_inputs(640), &cols);
        assert_eq!(normal.window_layout, WindowLayout::Normal);
        assert_eq!(normal.dialogs_width, COLUMN_MIN_WIDTH_LEFT);
        assert_eq!(normal.chat_width, COLUMN_MIN_WIDTH_MAIN);
        assert_eq!(normal.third_width, 0);
    }

    #[test]
    fn three_column_threshold_at_932_when_info_enabled() {
        let mut cols = WindowColumnsState::default();
        cols.set_third_section_info_enabled(true);

        // Just below 260 + 380 + 292 = 932 -> stays Normal.
        let at_931 = compute_column_layout(base_inputs(931), &cols);
        assert_eq!(at_931.window_layout, WindowLayout::Normal);
        assert_eq!(at_931.third_width, 0);

        // Exactly 932 -> switches to ThreeColumn with exact min widths.
        let at_932 = compute_column_layout(base_inputs(932), &cols);
        assert_eq!(at_932.window_layout, WindowLayout::ThreeColumn);
        assert_eq!(at_932.dialogs_width, COLUMN_MIN_WIDTH_LEFT);
        assert_eq!(at_932.chat_width, COLUMN_MIN_WIDTH_MAIN);
        assert_eq!(at_932.third_width, COLUMN_MIN_WIDTH_THIRD);
    }

    #[test]
    fn wide_window_without_third_section_stays_normal() {
        let cols = WindowColumnsState::default();
        let wide = compute_column_layout(base_inputs(1400), &cols);
        assert_eq!(wide.window_layout, WindowLayout::Normal);
        assert_eq!(wide.third_width, 0);
        assert!(wide.dialogs_width <= COLUMN_MAX_WIDTH_LEFT);
        assert_eq!(wide.chat_layout, ChatLayout::Wide);
    }

    #[test]
    fn filters_sidebar_threshold_matches_tdesktop() {
        assert!(!enough_space_for_filters(
            WINDOW_MIN_WIDTH + WINDOW_FILTERS_WIDTH - 1
        ));
        assert!(enough_space_for_filters(
            WINDOW_MIN_WIDTH + WINDOW_FILTERS_WIDTH
        ));

        let cols = WindowColumnsState::default();
        let mut inputs = base_inputs(711); // 711 - 72 = 639 (< 640) -> OneColumn!
        inputs.filters_enabled = true;
        let layout = compute_column_layout(inputs, &cols);
        assert_eq!(layout.filters_width, WINDOW_FILTERS_WIDTH);
        assert_eq!(layout.body_width, 639);
        assert_eq!(layout.window_layout, WindowLayout::OneColumn);

        inputs.window_width = 712; // 712 - 72 = 640 -> Normal!
        let layout_normal = compute_column_layout(inputs, &cols);
        assert_eq!(layout_normal.body_width, 640);
        assert_eq!(layout_normal.window_layout, WindowLayout::Normal);
    }

    #[test]
    fn compact_dialogs_column_when_dragged_below_half_min() {
        let ratio = dialogs_drag_to_ratio(COLUMN_MIN_WIDTH_LEFT / 2 - 1, 1000);
        assert_eq!(ratio, 0.0);

        let mut cols = WindowColumnsState::default();
        cols.update_dialogs_width_ratio(ratio, false);
        let layout = compute_column_layout(base_inputs(1000), &cols);
        assert!(layout.dialogs_small_column);
        assert_eq!(layout.dialogs_width, DIALOGS_SMALL_COLUMN_WIDTH);
        assert_eq!(layout.chat_width, 1000 - DIALOGS_SMALL_COLUMN_WIDTH);
    }

    #[test]
    fn third_column_shrink_preserves_minimums() {
        let mut cols = WindowColumnsState::default();
        cols.set_third_section_info_enabled(true);
        cols.set_third_column_width(COLUMN_MAX_WIDTH_THIRD);
        cols.update_dialogs_width_ratio(0.45, false);

        let layout = compute_column_layout(base_inputs(960), &cols);
        assert_eq!(layout.window_layout, WindowLayout::ThreeColumn);
        assert!(layout.dialogs_width >= COLUMN_MIN_WIDTH_LEFT);
        assert!(layout.third_width >= COLUMN_MIN_WIDTH_THIRD);
        assert!(layout.chat_width >= COLUMN_MIN_WIDTH_MAIN);
        assert_eq!(
            layout.dialogs_width + layout.chat_width + layout.third_width,
            960
        );
    }

    #[test]
    fn third_column_info_and_tabbed_are_mutually_exclusive() {
        let mut cols = WindowColumnsState::default();
        cols.set_third_section_info_enabled(true);
        assert!(cols.third_section_info_enabled());
        assert!(!cols.tabbed_selector_section_enabled());

        cols.set_tabbed_selector_section_enabled(true);
        assert!(!cols.third_section_info_enabled());
        assert!(cols.tabbed_selector_section_enabled());
    }
}
