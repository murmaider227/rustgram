//! Port of `ReplyKeyboard` layout math from `history/history_item_components.cpp`,
//! `history/view/history_view_element.cpp` (`KeyboardStyle::minButtonWidth`),
//! and `chat_helpers/bot_keyboard.cpp` (`BotKeyboard::resizeGetHeight` / `updateStyle`).

use serde::{Deserialize, Serialize};

use crate::markup::{ButtonType, ButtonTypeIcon, ReplyMarkupData};

/// Scrollbar width for `botKbScroll` (`defaultSolidScroll.width = 14px`).
pub const BOT_KB_SCROLL_WIDTH: i32 = 14;
/// Top scroll padding for `botKbScroll` (`defaultSolidScroll.deltat = 6px`).
pub const BOT_KB_SCROLL_DELTA_TOP: i32 = 6;
/// Bottom scroll padding for `botKbScroll` (`defaultSolidScroll.deltab = 6px`).
pub const BOT_KB_SCROLL_DELTA_BOTTOM: i32 = 6;

/// Pixel rectangle with top-left origin `(x, y)` and `(width, height)` size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    /// Left coordinate in pixels.
    pub x: i32,
    /// Top coordinate in pixels.
    pub y: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl Rect {
    /// Creates a new [`Rect`].
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if `(px, py)` falls inside `[x, x + width) x [y, y + height)`.
    /// Matches Qt's `QRect::contains(QPoint)`.
    pub const fn contains(self, px: i32, py: i32) -> bool {
        self.width > 0
            && self.height > 0
            && px >= self.x
            && px < self.x + self.width
            && py >= self.y
            && py < self.y + self.height
    }
}

/// Corner rounding radius category matching `Ui::BubbleCornerRounding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CornerRounding {
    /// Standard small button corner (`bubbleRadiusSmall`).
    #[default]
    Small,
    /// Large outer bubble corner (`bubbleRadiusLarge`).
    Large,
}

/// Per-corner rounding for a bubble or keyboard button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ButtonRounding {
    /// Top-left corner.
    pub top_left: CornerRounding,
    /// Top-right corner.
    pub top_right: CornerRounding,
    /// Bottom-left corner.
    pub bottom_left: CornerRounding,
    /// Bottom-right corner.
    pub bottom_right: CornerRounding,
}

impl ButtonRounding {
    /// All four corners large (used by `BotKeyboard`'s `kBotKeyboardRounding`).
    pub const ALL_LARGE: Self = Self {
        top_left: CornerRounding::Large,
        top_right: CornerRounding::Large,
        bottom_left: CornerRounding::Large,
        bottom_right: CornerRounding::Large,
    };

    /// All four corners small.
    pub const ALL_SMALL: Self = Self {
        top_left: CornerRounding::Small,
        top_right: CornerRounding::Small,
        bottom_left: CornerRounding::Small,
        bottom_right: CornerRounding::Small,
    };
}

/// Style dimensions driving `ReplyKeyboard` layout calculations.
///
/// Exact defaults come from `ui/chat/chat.style` (`msgBotKbButton`, `botKbButton`,
/// `botKbTinyButton`) and `Resources/icons/inline_button_*.png` (10x10 px at 1x).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BotKeyboardMetrics {
    /// Gap between adjacent buttons (`margin` / `buttonSkip()`).
    pub button_skip: i32,
    /// Horizontal inner text padding on each side (`padding` / `buttonPadding()`).
    pub button_padding: i32,
    /// Natural height of a single button (`height` / `buttonHeight()`).
    pub button_height: i32,
    /// Padding around corner icons (`msgBotKbIconPadding = 4`).
    pub icon_padding: i32,
    /// Width of the URL / Auth icon (`inline_button_url = 10`).
    pub url_icon_width: i32,
    /// Width of the Payment icon (`inline_button_card = 10`).
    pub payment_icon_width: i32,
    /// Width of the SwitchInline / SwitchPm icon (`inline_button_switch = 10`).
    pub switch_pm_icon_width: i32,
    /// Width of the WebView icon (`inline_button_web = 10`).
    pub webview_icon_width: i32,
    /// Width of the Copy icon (`inline_button_copy = 10`).
    pub copy_icon_width: i32,
    /// Width of the loading spinner icon (`historySendingInvertedIcon`, 12 px at 1x).
    pub loading_icon_width: i32,
    /// Whether corner/loading icons reserve space in `min_button_width`.
    /// `true` for inline keyboards (`KeyboardStyle`), `false` for reply keyboards (`BotKeyboard::Style`).
    pub reserve_icon_space: bool,
}

impl BotKeyboardMetrics {
    /// Inline message keyboard metrics (`st::msgBotKbButton`, `ui/chat/chat.style:275-287`).
    pub const INLINE: Self = Self {
        button_skip: 2,
        button_padding: 10,
        button_height: 36,
        icon_padding: 4,
        url_icon_width: 10,
        payment_icon_width: 10,
        switch_pm_icon_width: 10,
        webview_icon_width: 10,
        copy_icon_width: 10,
        loading_icon_width: 12,
        reserve_icon_space: true,
    };

    /// Normal reply keyboard metrics (`st::botKbButton`, `ui/chat/chat.style:294-302`).
    pub const REPLY_NORMAL: Self = Self {
        button_skip: 10,
        button_padding: 10,
        button_height: 38,
        icon_padding: 0,
        url_icon_width: 0,
        payment_icon_width: 0,
        switch_pm_icon_width: 0,
        webview_icon_width: 0,
        copy_icon_width: 0,
        loading_icon_width: 0,
        reserve_icon_space: false,
    };

    /// Compact reply keyboard metrics (`st::botKbTinyButton`, `ui/chat/chat.style:303-309`).
    pub const REPLY_TINY: Self = Self {
        button_skip: 4,
        button_padding: 3,
        button_height: 25,
        icon_padding: 0,
        url_icon_width: 0,
        payment_icon_width: 0,
        switch_pm_icon_width: 0,
        webview_icon_width: 0,
        copy_icon_width: 0,
        loading_icon_width: 0,
        reserve_icon_space: false,
    };

    /// Computes minimum chrome width (padding + reserved icon width) for a button type.
    ///
    /// Port of `KeyboardStyle::minButtonWidth` (`history/view/history_view_element.cpp:411-447`)
    /// and `BotKeyboard::Style::minButtonWidth` (`chat_helpers/bot_keyboard.cpp:183-186`).
    pub const fn min_button_width(self, icon_type: ButtonType) -> i32 {
        let mut result = 2 * self.button_padding;
        if !self.reserve_icon_space {
            return result;
        }
        let mut icon_width = match icon_type.icon() {
            ButtonTypeIcon::None => 0,
            ButtonTypeIcon::Url => self.url_icon_width,
            ButtonTypeIcon::Payment => self.payment_icon_width,
            ButtonTypeIcon::SwitchPm => self.switch_pm_icon_width,
            ButtonTypeIcon::Webview => self.webview_icon_width,
            ButtonTypeIcon::Copy => self.copy_icon_width,
        };
        if icon_type.loads_on_activate() && self.loading_icon_width > icon_width {
            icon_width = self.loading_icon_width;
        }
        if icon_width > 0 {
            let icon_reserved = 2 * icon_width + 4 * self.icon_padding;
            if icon_reserved > result {
                result = icon_reserved;
            }
        }
        result
    }
}

/// Measured input for a single button in a keyboard row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutButtonInput {
    /// Measured single-line text width in pixels (clamped to `>= 1` during layout).
    pub text_width: i32,
    /// Effective button type for icon reservation (e.g. `MarkupButton::effective_icon_type()`).
    pub icon_type: ButtonType,
    /// Original button type (used for admin-log click filtering).
    pub button_type: ButtonType,
}

impl LayoutButtonInput {
    /// Builds a [`LayoutButtonInput`] with the given text width and button type.
    pub const fn new(text_width: i32, button_type: ButtonType) -> Self {
        Self {
            text_width,
            icon_type: button_type,
            button_type,
        }
    }
}

/// Laid-out geometry and display flags for a single keyboard button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LaidOutButton {
    /// Zero-based row index.
    pub row: usize,
    /// Zero-based column index within the row.
    pub column: usize,
    /// Positioned rectangle relative to the keyboard origin.
    pub rect: Rect,
    /// `true` when the full button label fits without elision (`textw >= buttonw`).
    pub full_displayed: bool,
    /// Effective button type.
    pub button_type: ButtonType,
}

/// Complete computed keyboard layout (analogous to `ReplyKeyboard` in
/// `history/history_item_components.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonsLayout {
    rows: Vec<Vec<LayoutButtonInput>>,
    laid_out: Vec<Vec<LaidOutButton>>,
    metrics: BotKeyboardMetrics,
    width: i32,
    height: i32,
    rtl: bool,
}

impl ButtonsLayout {
    /// Creates a new layout from pre-measured button rows and style metrics.
    pub fn new(rows: Vec<Vec<LayoutButtonInput>>, metrics: BotKeyboardMetrics) -> Self {
        let laid_out = rows
            .iter()
            .enumerate()
            .map(|(r_idx, row)| {
                row.iter()
                    .enumerate()
                    .map(|(c_idx, b)| LaidOutButton {
                        row: r_idx,
                        column: c_idx,
                        rect: Rect::default(),
                        full_displayed: true,
                        button_type: b.button_type,
                    })
                    .collect()
            })
            .collect();
        Self {
            rows,
            laid_out,
            metrics,
            width: 0,
            height: 0,
            rtl: false,
        }
    }

    /// Creates a [`ButtonsLayout`] from a [`ReplyMarkupData`] using a caller-provided
    /// text-width measurement function (so `tg-bots` stays GUI-independent).
    pub fn from_markup<F>(
        markup: &ReplyMarkupData,
        metrics: BotKeyboardMetrics,
        mut measure_text_width: F,
    ) -> Self
    where
        F: FnMut(&str) -> i32,
    {
        let rows = markup
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|btn| LayoutButtonInput {
                        text_width: measure_text_width(&btn.text),
                        icon_type: btn.effective_icon_type(),
                        button_type: btn.button_type(),
                    })
                    .collect()
            })
            .collect();
        Self::new(rows, metrics)
    }

    /// Enables or disables right-to-left coordinate mirroring.
    pub fn set_rtl(&mut self, rtl: bool) {
        self.rtl = rtl;
    }

    /// Replaces the style metrics used by subsequent layout calculations.
    pub fn set_metrics(&mut self, metrics: BotKeyboardMetrics) {
        self.metrics = metrics;
    }

    /// Returns the active [`BotKeyboardMetrics`].
    pub const fn metrics(&self) -> BotKeyboardMetrics {
        self.metrics
    }

    /// Returns the currently laid-out rows.
    pub fn rows(&self) -> &[Vec<LaidOutButton>] {
        &self.laid_out
    }

    /// Returns the last resized width.
    pub const fn width(&self) -> i32 {
        self.width
    }

    /// Returns the last resized height.
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// Returns the natural width that fits every row with equal-width buttons
    /// without truncating any button label.
    ///
    /// Port of `ReplyKeyboard::naturalWidth` (`history_item_components.cpp:1075-1097`).
    pub fn natural_width(&self) -> i32 {
        let mut result = 0;
        for row in &self.rows {
            if row.is_empty() {
                continue;
            }
            let mut max_min_button_width = 0;
            for button in row {
                max_min_button_width =
                    max_min_button_width.max(self.metrics.min_button_width(button.icon_type));
            }
            let mut row_max_button_width = 0;
            for button in row {
                let candidate = button.text_width.max(1) + max_min_button_width;
                row_max_button_width = row_max_button_width.max(candidate);
            }
            let row_size = row.len() as i32;
            let row_width =
                row_size * row_max_button_width + (row_size - 1) * self.metrics.button_skip;
            result = result.max(row_width);
        }
        result
    }

    /// Returns the natural height of all rows plus inter-row skips.
    ///
    /// Port of `ReplyKeyboard::naturalHeight` (`history_item_components.cpp:1099-1102`).
    pub fn natural_height(&self) -> i32 {
        if self.rows.is_empty() {
            return 0;
        }
        let count = self.rows.len() as i32;
        (count - 1) * self.metrics.button_skip + count * self.metrics.button_height
    }

    /// Checks whether `width` has enough space to render with the given `metrics`
    /// (used by `BotKeyboard::updateStyle` to choose between `REPLY_NORMAL` and `REPLY_TINY`).
    ///
    /// Port of `ReplyKeyboard::isEnoughSpace` (`history_item_components.cpp:1051-1069`).
    /// Note tdesktop's exact rule: a row only triggers `false` if it overflows AND has `> 3` buttons.
    pub fn is_enough_space(&self, width: i32, metrics: BotKeyboardMetrics) -> bool {
        for row in &self.rows {
            let s = row.len() as i32;
            if s == 0 {
                continue;
            }
            let mut width_left =
                width - ((s - 1) * metrics.button_skip + s * 2 * metrics.button_padding);
            for button in row {
                width_left -= button.text_width.max(1);
                if width_left < 0 {
                    if row.len() > 3 {
                        return false;
                    } else {
                        break;
                    }
                }
            }
        }
        true
    }

    /// Computes button rectangles for the given `(width, height)`.
    ///
    /// Port of `ReplyKeyboard::resize` (`history_item_components.cpp:992-1049`).
    pub fn resize(&mut self, width: i32, height: i32) {
        self.width = width;
        self.height = height;

        let mut y = 0.0_f64;
        let row_count = self.rows.len();
        let button_height = if row_count == 0 {
            self.metrics.button_height as f64
        } else {
            (height + self.metrics.button_skip) as f64 / row_count as f64
        };
        let skip = self.metrics.button_skip;
        let padding = self.metrics.button_padding;

        for (r_idx, row) in self.rows.iter().enumerate() {
            let s = row.len() as i32;
            if s == 0 {
                y += button_height;
                continue;
            }

            let width_for_buttons = width - ((s - 1) * skip);
            let mut width_for_text = width_for_buttons;
            let mut width_of_text = 0_i32;
            let mut max_min_button_width = 0_i32;
            for button in row {
                width_of_text += button.text_width.max(1);
                let min_bw = self.metrics.min_button_width(button.icon_type);
                width_for_text -= min_bw;
                max_min_button_width = max_min_button_width.max(min_bw);
            }
            let exact = width_for_text == width_of_text;
            let enough = (width_for_buttons - s * max_min_button_width) >= width_of_text;

            let mut x = 0.0_f64;
            for (c_idx, button) in row.iter().enumerate() {
                let buttonw = button.text_width.max(1);
                let minw = self.metrics.min_button_width(button.icon_type) as f64;
                let (w, textw) = if exact {
                    let textw = buttonw as f64;
                    (textw + minw, textw)
                } else if enough {
                    let w = width_for_buttons as f64 / s as f64;
                    (w, w - minw)
                } else {
                    let textw = width_for_text as f64 / s as f64;
                    let w = (minw + textw).max(2.0 * padding as f64);
                    (w, textw)
                };

                let mut rect_x = x.floor() as i32;
                let rect_w = (x + w).floor() as i32 - rect_x;
                let rect_y = safe_round(y);
                let rect_h = safe_round(button_height - skip as f64);
                if self.rtl {
                    rect_x = width - rect_x - rect_w;
                }
                x += w + skip as f64;

                let out = &mut self.laid_out[r_idx][c_idx];
                out.rect = Rect::new(rect_x, rect_y, rect_w, rect_h);
                out.full_displayed = textw >= buttonw as f64;
            }
            y += button_height;
        }
    }

    /// Resizes a reply keyboard container (`BotKeyboard::resizeGetHeight` + `updateStyle`
    /// in `chat_helpers/bot_keyboard.cpp:387-416`), selecting between `REPLY_NORMAL`
    /// and `REPLY_TINY` and applying `maximize_size` (`!resize`).
    ///
    /// Returns the total outer height of the reply keyboard panel.
    pub fn resize_reply_keyboard(
        &mut self,
        outer_width: i32,
        max_outer_height: i32,
        maximize_size: bool,
    ) -> i32 {
        let test_impl_width =
            outer_width - BotKeyboardMetrics::REPLY_NORMAL.button_skip - BOT_KB_SCROLL_WIDTH;
        let chosen = if self.is_enough_space(test_impl_width, BotKeyboardMetrics::REPLY_NORMAL) {
            BotKeyboardMetrics::REPLY_NORMAL
        } else {
            BotKeyboardMetrics::REPLY_TINY
        };
        self.set_metrics(chosen);

        let mut total_height =
            BOT_KB_SCROLL_DELTA_TOP + BOT_KB_SCROLL_DELTA_BOTTOM + self.natural_height();
        if maximize_size {
            total_height = total_height.max(max_outer_height);
        }
        let impl_width = outer_width - chosen.button_skip - BOT_KB_SCROLL_WIDTH;
        let impl_height = total_height - (BOT_KB_SCROLL_DELTA_TOP + BOT_KB_SCROLL_DELTA_BOTTOM);
        self.resize(impl_width, impl_height);
        total_height
    }

    /// Computes per-corner rounding for the button at `(row, column)`.
    ///
    /// Port of `ReplyKeyboard::paint` (`history_item_components.cpp:1131-1154`).
    /// When `is_inline` is `true`, top corners are always `Small` (attached under the message bubble).
    pub fn button_rounding(
        &self,
        row: usize,
        column: usize,
        outer: ButtonRounding,
        is_inline: bool,
    ) -> ButtonRounding {
        let rows_count = self.laid_out.len();
        let cols_count = self.laid_out.get(row).map_or(0, Vec::len);
        let top_left =
            if row == 0 && column == 0 && !is_inline && outer.top_left == CornerRounding::Large {
                CornerRounding::Large
            } else {
                CornerRounding::Small
            };
        let top_right = if row == 0
            && column + 1 == cols_count
            && !is_inline
            && outer.top_right == CornerRounding::Large
        {
            CornerRounding::Large
        } else {
            CornerRounding::Small
        };
        let bottom_left =
            if row + 1 == rows_count && column == 0 && outer.bottom_left == CornerRounding::Large {
                CornerRounding::Large
            } else {
                CornerRounding::Small
            };
        let bottom_right = if row + 1 == rows_count
            && column + 1 == cols_count
            && outer.bottom_right == CornerRounding::Large
        {
            CornerRounding::Large
        } else {
            CornerRounding::Small
        };
        ButtonRounding {
            top_left,
            top_right,
            bottom_left,
            bottom_right,
        }
    }

    /// Hit-tests `(px, py)` against laid-out buttons, ignoring buttons that overflow `self.width`.
    ///
    /// Port of `ReplyKeyboard::getLink` (`history_item_components.cpp:1183-1207`).
    /// When `is_admin_log_entry` is `true`, only `Url` and `Callback` buttons are interactive.
    pub fn hit_test(&self, px: i32, py: i32, is_admin_log_entry: bool) -> Option<(usize, usize)> {
        if self.width <= 0 {
            return None;
        }
        for row in &self.laid_out {
            for button in row {
                if button.rect.x + button.rect.width > self.width {
                    break;
                }
                if button.rect.contains(px, py) {
                    if is_admin_log_entry
                        && button.button_type != ButtonType::Url
                        && button.button_type != ButtonType::Callback
                    {
                        return None;
                    }
                    return Some((button.row, button.column));
                }
            }
        }
        None
    }

    /// Returns `(row, column)` for a zero-based flattened button index.
    ///
    /// Port of `ReplyKeyboard::getLinkByIndex` (`history_item_components.cpp:1209-1220`).
    pub fn coords_by_index(&self, mut index: usize) -> Option<(usize, usize)> {
        for (r_idx, row) in self.laid_out.iter().enumerate() {
            if index < row.len() {
                return Some((r_idx, index));
            }
            index -= row.len();
        }
        None
    }
}

/// Port of `base::SafeRound` (`std::round`).
#[inline]
fn safe_round(v: f64) -> i32 {
    v.round() as i32
}
