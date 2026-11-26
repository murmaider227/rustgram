//! Dialog row painting, badges, special avatars, typing animation, filter tags, and search highlighting.
//!
//! Port origin: `Telegram/SourceFiles/dialogs/ui/dialogs_layout.h`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_layout.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_message_view.cpp`,
//! `Telegram/SourceFiles/ui/chat/chats_filter_tag.cpp`,
//! and `Telegram/SourceFiles/dialogs/dialogs.style`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Galley, Id, Painter, Pos2, Rect, Stroke, StrokeKind,
    Vec2,
};
use tg_history::datetime::format_time_short;
use tg_history::dialogs::elide_text_right_hinted;
use tg_history::{
    AvatarCornerBadge, DialogChatTypeIcon, DialogRowInput, DialogRowLayout, DialogRowMode,
    DialogRowStyle, DialogSendState, DialogsDateKind, FontMetrics, LocalDateTime,
    classify_dialogs_date, elide_text_right, format_unread_counter, layout_dialog_row,
};
use tg_lang::keys::{SimpleDate, day_of_month, day_of_week};
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::{Align, UnreadBadgeStyle};
use tg_ui::tg_style::structs::ThreeStateIcon;
use tg_ui::tg_style::{Color as StyleColor, ColorResolver, Icon, st};
use tg_ui::tg_theme::{EmbeddedType, Palette as ThemePalette};
use tg_ui::theme::{Colors, ThemeManager, egui_color_to_style};
use tg_ui::unread_badge::{UnreadBadgeKind, paint_unread_badge};
use tg_ui::userpic::{
    EmptyUserpic, paint_vertical_gradient_circle, peer_name_color, userpic_colors,
};

use crate::dialogs::backend::{
    ArchiveSummary, DialogItem, DialogPeerKind, FolderTagBadge, GlobalPeerResult,
    MessageSearchResult, SpecialRowKind,
};
use crate::dialogs::emoji_status;
use crate::dialogs::mini_preview::{PreviewLineColors, paint_preview_line};
use crate::dialogs::search_snippet::message_search_snippet;
use tg_app_core::stand_in::theme::Palette;
use tg_ui::widgets::media_marker::{paint_media_marker, split_media_marker};

/// Height of the collapsed Archive bar (`st::dialogsImportantBarHeight = 37px`).
pub const COLLAPSED_ARCHIVE_HEIGHT: f32 = st::dialogs_important_bar_height.0 as f32;

/// Height of a section divider bar (`st::searchedBarHeight = 29px`).
pub const SEARCHED_BAR_HEIGHT: f32 = st::searched_bar_height.0 as f32;

fn day_theme_palette() -> &'static ThemePalette {
    static DAY: OnceLock<ThemePalette> = OnceLock::new();
    DAY.get_or_init(|| {
        ThemeManager::with_embedded(EmbeddedType::Default)
            .palette()
            .clone()
    })
}

fn night_theme_palette() -> &'static ThemePalette {
    static NIGHT: OnceLock<ThemePalette> = OnceLock::new();
    NIGHT.get_or_init(|| {
        ThemeManager::with_embedded(EmbeddedType::Night)
            .palette()
            .clone()
    })
}

/// Theme bridge backed by `tg-ui`'s [`ThemeManager`] (`tg_theme::Palette`) for both Day and
/// Night modes, implementing [`Colors`] and [`tg_ui::tg_style::ColorResolver`].
pub struct DialogColors<'a> {
    theme_palette: &'a ThemePalette,
    is_night: bool,
}

impl<'a> DialogColors<'a> {
    /// Creates a color bridge for the active `palette`, resolving real `tg-theme` tokens via
    /// [`ThemeManager`]'s Classic Day or Night palette.
    #[must_use]
    pub fn new(palette: &'a Palette) -> Self {
        let is_night = palette.window_bg.r() < 128;
        Self {
            theme_palette: if is_night {
                night_theme_palette()
            } else {
                day_theme_palette()
            },
            is_night,
        }
    }

    /// Creates a color bridge directly from a live [`ThemeManager`].
    #[must_use]
    pub fn from_theme(theme: &'a ThemeManager) -> Self {
        Self {
            theme_palette: theme.palette(),
            is_night: theme.is_dark(),
        }
    }

    /// Returns `true` when the active palette is dark/night mode.
    #[must_use]
    pub const fn is_night(&self) -> bool {
        self.is_night
    }

    /// Primary window / dialogs list background (`dialogsBg`).
    #[must_use]
    pub fn window_bg(&self) -> Color32 {
        self.color("dialogsBg")
    }

    /// Primary bold title foreground (`windowBoldFg`).
    #[must_use]
    pub fn bold_fg(&self) -> Color32 {
        self.color("windowBoldFg")
    }

    /// Secondary/muted foreground (`windowSubTextFg`).
    #[must_use]
    pub fn sub_text_fg(&self) -> Color32 {
        self.color("windowSubTextFg")
    }

    /// Active accent text foreground (`windowActiveTextFg`).
    #[must_use]
    pub fn active_text_fg(&self) -> Color32 {
        self.color("windowActiveTextFg")
    }

    /// Hairline divider foreground (`shadowFg`).
    #[must_use]
    pub fn divider(&self) -> Color32 {
        self.color("shadowFg")
    }

    /// Background color for search section headers (`searchedBarBg`).
    #[must_use]
    pub fn searched_bar_bg(&self) -> Color32 {
        self.color("searchedBarBg")
    }

    /// Foreground color for search section headers (`searchedBarFg`).
    #[must_use]
    pub fn searched_bar_fg(&self) -> Color32 {
        self.color("searchedBarFg")
    }

    /// Draft prefix color (`dialogsDraftFg` / `dialogsDraftFgActive`).
    #[must_use]
    pub fn draft_fg(&self, active: bool) -> Color32 {
        self.color(if active {
            "dialogsDraftFgActive"
        } else {
            "dialogsDraftFg"
        })
    }

    /// Online badge fill (`dialogsOnlineBadgeFg` / `dialogsOnlineBadgeFgActive`).
    #[must_use]
    pub fn online_badge_fg(&self, active: bool) -> Color32 {
        self.color(if active {
            "dialogsOnlineBadgeFgActive"
        } else {
            "dialogsOnlineBadgeFg"
        })
    }

    /// Accent color for verified badges (`dialogsVerifiedIconBg` / `dialogsVerifiedIconBgActive`).
    #[must_use]
    pub fn verified_bg(&self, active: bool) -> Color32 {
        self.color(if active {
            "dialogsVerifiedIconBgActive"
        } else {
            "dialogsVerifiedIconBg"
        })
    }

    /// Checkmark stroke color inside verified badges (`dialogsVerifiedIconCheck`).
    #[must_use]
    pub fn verified_check_fg(&self, active: bool) -> Color32 {
        self.color(if active {
            "dialogsVerifiedIconCheckActive"
        } else {
            "dialogsVerifiedIconCheck"
        })
    }

    /// Premium star badge color (`dialogsVerifiedIconBg` / `Active`).
    #[must_use]
    pub fn premium_fg(&self, active: bool) -> Color32 {
        self.verified_bg(active)
    }

    /// Outgoing check / sending clock tint (`dialogsSentIconFg` / `dialogsSentIconFgActive`).
    #[must_use]
    pub fn send_state_fg(&self, active: bool) -> Color32 {
        self.color(if active {
            "dialogsSentIconFgActive"
        } else {
            "dialogsSentIconFg"
        })
    }

    /// Highlight background for matched search query text.
    #[must_use]
    pub fn search_highlight_bg(&self, active: bool) -> Color32 {
        if active {
            Color32::from_rgba_unmultiplied(255, 255, 255, 58)
        } else {
            let accent = self.color("windowBgActive");
            Color32::from_rgba_unmultiplied(
                accent.r(),
                accent.g(),
                accent.b(),
                if self.is_night { 95 } else { 48 },
            )
        }
    }
}

impl Colors for DialogColors<'_> {
    fn get(&self, name: &str) -> Option<Color32> {
        self.theme_palette.get(name)
    }
}

impl ColorResolver for DialogColors<'_> {
    fn resolve(&self, name: &str) -> Option<StyleColor> {
        self.theme_palette
            .get(name)
            .map(egui_color_to_style)
            .or_else(|| self.theme_palette.resolve(name))
    }
}

/// Formats a dialog timestamp like `Ui::FormatDialogsDate` (`ui/text/format_values.cpp:724-740`):
/// the time (`"H:mm"`, `QLocale::ShortFormat`) for the same day or the last 20 hours, the localized
/// weekday (`langDayOfWeek`) within a week, otherwise the localized date (`langDayOfMonth`).
#[must_use]
pub fn format_localized_dialogs_date(
    dt: LocalDateTime,
    now: LocalDateTime,
    use_24h: bool,
) -> String {
    match classify_dialogs_date(dt, now) {
        DialogsDateKind::Time if use_24h => format_time_short(dt),
        DialogsDateKind::Time => {
            let suffix = if dt.hour < 12 { "AM" } else { "PM" };
            let h12 = match dt.hour % 12 {
                0 => 12,
                h => h,
            };
            format!("{h12}:{:02} {suffix}", dt.minute)
        }
        DialogsDateKind::Weekday => {
            // 1970-01-01 (day 0) was Thursday (ISO weekday 4); map to 1 = Mon .. 7 = Sun.
            let dt_days = dt.to_local_epoch_secs().div_euclid(86_400);
            let wd = (dt_days + 3).rem_euclid(7) as u8 + 1;
            tg_lang::with_current(|inst| day_of_week(inst, wd))
        }
        DialogsDateKind::Date => localized_short_date(dt, now),
    }
}

fn localized_short_date(dt: LocalDateTime, now: LocalDateTime) -> String {
    let d = SimpleDate {
        year: dt.date.year,
        month: dt.date.month,
        day: dt.date.day,
    };
    let t = SimpleDate {
        year: now.date.year,
        month: now.date.month,
        day: now.date.day,
    };
    tg_lang::with_current(|inst| day_of_month(inst, d, t))
}

fn pick_three_state(icon: ThreeStateIcon, active: bool, selected: bool) -> Icon {
    if active {
        icon.active
    } else if selected {
        icon.over
    } else {
        icon.icon
    }
}

/// Visual state flags passed into row painting (`Dialogs::Ui::PaintContext`).
#[derive(Debug, Clone, Copy)]
pub struct RowPaintState {
    /// True if this row corresponds to the currently open chat.
    pub active: bool,
    /// True if hovered or keyboard-highlighted.
    pub selected: bool,
    /// True when rendered inside the 66px compact avatar rail.
    pub narrow: bool,
    /// True if this row is currently being dragged for pinned reorder.
    pub dragging: bool,
    /// Show folder filter tag pills beneath the preview text (`DialogRowStyle::TAGGED`).
    pub show_folder_tags: bool,
    /// Animation clock in seconds (for typing dots).
    pub anim_time_secs: f64,
    /// Use 24-hour clock for timestamps.
    pub use_24h: bool,
}

impl Default for RowPaintState {
    fn default() -> Self {
        Self {
            active: false,
            selected: false,
            narrow: false,
            dragging: false,
            show_folder_tags: false,
            anim_time_secs: 0.0,
            use_24h: true,
        }
    }
}

/// Converts a `tg_history::Rect` offset by `origin` into an `egui::Rect`.
#[inline]
fn to_egui_rect(origin: Pos2, r: tg_history::Rect) -> Rect {
    Rect::from_min_size(
        Pos2::new(origin.x + r.x as f32, origin.y + r.y as f32),
        Vec2::new(r.width as f32, r.height as f32),
    )
}

/// Chooses the appropriate [`DialogRowStyle`] for a dialog entry.
#[must_use]
pub fn row_style_for(item: &DialogItem, show_folder_tags: bool, narrow: bool) -> DialogRowStyle {
    if !narrow && show_folder_tags && !item.folder_tags.is_empty() {
        DialogRowStyle::TAGGED
    } else {
        DialogRowStyle::DEFAULT
    }
}

/// Computes the exact `tg-history` layout for `item` at `width`.
pub fn compute_item_layout(
    painter: &Painter,
    item: &DialogItem,
    now: LocalDateTime,
    width: f32,
    state: RowPaintState,
) -> (DialogRowLayout, String, String) {
    let style = row_style_for(item, state.show_folder_tags, state.narrow);
    let mode = if state.narrow {
        DialogRowMode::Narrow
    } else {
        DialogRowMode::Normal
    };

    let date_text = if item.last_message_id.0 != 0 || item.effective_draft().is_some() {
        format_localized_dialogs_date(item.display_time(), now, state.use_24h)
    } else {
        String::new()
    };

    let counter_text = format_unread_counter(
        item.badges.unread_counter,
        item.badges.mention || item.badges.reaction || item.badges.poll,
        state.narrow,
    );

    let name_font = font_id_from_style(st::semibold_font);
    let date_font = font_id_from_style(st::dialogs_date_font);
    let badge_font = font_id_from_style(st::dialogs_unread_font);

    let name_w = measure_text_i32(painter, &item.title, name_font);
    let date_w = if date_text.is_empty() {
        0
    } else {
        measure_text_i32(painter, &date_text, date_font)
    };
    let counter_w = if counter_text.is_empty() {
        0
    } else {
        measure_text_i32(painter, &counter_text, badge_font)
    };

    let peer_badge_width = row_peer_badge_width(item);
    let send_state = if let Some(draft) = item.effective_draft() {
        if draft.saving {
            DialogSendState::Sending
        } else {
            DialogSendState::None
        }
    } else {
        item.send_state
    };
    let avatar_badge = if item.active_call {
        AvatarCornerBadge::ActiveCall
    } else if item.online {
        AvatarCornerBadge::Online
    } else {
        AvatarCornerBadge::None
    };

    let input = DialogRowInput {
        style,
        mode,
        width: width.round() as i32,
        chat_type_icon: if item.special == SpecialRowKind::None {
            item.kind.chat_type_icon()
        } else {
            DialogChatTypeIcon::None
        },
        name_text_width: name_w,
        peer_badge_width,
        show_mute_icon: item.muted && item.special == SpecialRowKind::None,
        date_text_width: date_w,
        send_state,
        badges: item.badges,
        unread_counter_text_width: counter_w,
        display_pinned_icon: item.display_pinned_icon(),
        right_button_width: if item.has_bot_app_button { 52 } else { 0 },
        is_typing: item.typing.is_some(),
        avatar_badge,
        has_draft: item.effective_draft().is_some(),
        has_item: item.last_message_id.0 != 0 || !item.last_message.is_empty(),
        has_topics_line: false,
    };

    let layout = layout_dialog_row(&input, &FontMetrics::default());
    (layout, date_text, counter_text)
}

/// Paints a full or compact dialog row inside `rect` using `tg-history`'s computed geometry
/// and `tg-ui` primitives (`RowPainter::Paint` in `dialogs_layout.cpp`).
///
/// Returns `true` if the optional bot `"Open"` right button exists and `pointer` lies in it.
#[allow(clippy::too_many_arguments)]
pub fn paint_dialog_row(
    painter: &Painter,
    rect: Rect,
    item: &DialogItem,
    now: LocalDateTime,
    state: RowPaintState,
    palette: &Palette,
    colors: &DialogColors<'_>,
) -> Option<Rect> {
    let _ = palette;
    let prepared = prepare_dialog_row(painter, item, now, rect.width(), state);
    paint_prepared_dialog_row(painter, rect, item, &prepared, state, colors)
}

/// The part of a dialog row that does not depend on where or in which state it is painted:
/// its geometry and the strings already formatted for one row width.
///
/// Built by [`prepare_dialog_row`]. The chat list keeps it for every painted row and builds it
/// again only when the row, its width or the fonts change, so a frame without changes formats
/// and measures nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDialogRow {
    layout: DialogRowLayout,
    date_text: String,
    counter_text: String,
    /// The title cut to the name column, when it does not fit.
    elided_title: Option<String>,
    /// "typing" / "Sam is typing" of the row's send action.
    typing_label: Option<String>,
    /// "Draft: " in front of the row's unsent draft.
    draft_prefix: Option<String>,
}

impl PreparedDialogRow {
    /// Geometry of the row.
    #[must_use]
    pub const fn layout(&self) -> &DialogRowLayout {
        &self.layout
    }
}

fn draft_prefix_text() -> String {
    format!(
        "{} ",
        tg_lang::tr::lng_dialogs_text_from_wrapped(&tg_lang::tr::lng_from_draft())
    )
}

/// Formats and measures everything [`paint_prepared_dialog_row`] needs to paint `item` in a row
/// `width` wide.
#[must_use]
pub fn prepare_dialog_row(
    painter: &Painter,
    item: &DialogItem,
    now: LocalDateTime,
    width: f32,
    state: RowPaintState,
) -> PreparedDialogRow {
    let (layout, date_text, counter_text) = compute_item_layout(painter, item, now, width, state);
    // The compact rail paints the avatar and its badges only.
    let with_text = !state.narrow;
    let elided_title = (with_text && layout.name_elided).then(|| {
        elide_text(
            painter,
            &item.title,
            layout.name_rect.width,
            &font_id_from_style(st::semibold_font),
        )
    });
    let typing_label = item
        .typing
        .as_ref()
        .filter(|_| with_text && layout.typing_rect.is_some())
        .map(|typing| typing.label());
    let draft_prefix = (with_text && item.effective_draft().is_some()).then(draft_prefix_text);
    PreparedDialogRow {
        layout,
        date_text,
        counter_text,
        elided_title,
        typing_label,
        draft_prefix,
    }
}

/// [`paint_dialog_row`] with the row's text already prepared by [`prepare_dialog_row`] for the
/// same `item` and the width of `rect`.
pub fn paint_prepared_dialog_row(
    painter: &Painter,
    rect: Rect,
    item: &DialogItem,
    prepared: &PreparedDialogRow,
    state: RowPaintState,
    colors: &DialogColors<'_>,
) -> Option<Rect> {
    let layout = &prepared.layout;
    let date_text = prepared.date_text.as_str();
    let counter_text = prepared.counter_text.as_str();
    let origin = rect.min;

    // 1. Row background (`dialogsBgActive` / `dialogsBgOver` / `dialogsBg`).
    let row_bg = if state.active {
        colors.color("dialogsBgActive")
    } else if state.dragging || state.selected {
        colors.color("dialogsBgOver")
    } else {
        colors.color("dialogsBg")
    };
    painter.rect_filled(rect, CornerRadius::ZERO, row_bg);

    if state.dragging {
        painter.rect_stroke(
            rect.shrink(0.5),
            CornerRadius::ZERO,
            Stroke::new(1.0, colors.color("windowBgActive")),
            StrokeKind::Inside,
        );
    }

    // 2. Userpic / special avatar (`EmptyUserpic` + Saved Messages / Replies / VerificationCodes).
    let photo_rect = to_egui_rect(origin, layout.photo_rect);
    if item.kind == DialogPeerKind::Forum && item.special == SpecialRowKind::None {
        // `PeerData::userpicShape`: a forum (and a community) is a rounded square.
        EmptyUserpic::for_color_index(item.color_index, &item.title, colors)
            .paint_rounded_or_photo(
                painter,
                photo_rect,
                photo_rect.width() * FORUM_USERPIC_RADIUS_MULTIPLIER,
                item.peer_id.0,
            );
    } else {
        paint_peer_userpic(
            painter,
            photo_rect,
            item.peer_id,
            item.special,
            item.color_index,
            &item.title,
            colors,
        );
    }

    // 3. Avatar corner badge (green online dot or active call badge).
    if let Some(badge_r) = layout.avatar_badge_rect {
        let r = to_egui_rect(origin, badge_r);
        if item.active_call {
            paint_active_call_badge(painter, r, row_bg, colors.color("windowBgActive"));
        } else if item.online {
            paint_online_dot(painter, r, row_bg, colors.online_badge_fg(state.active));
        }
    }

    // 4. Narrow compact rail stops after avatar + overlaid badges (`dialogs_layout.cpp:556-561`).
    if state.narrow {
        paint_row_badges(painter, origin, layout, item, counter_text, state, colors);
        return None;
    }

    let name_fg = colors.color(if state.active {
        "dialogsNameFgActive"
    } else if state.selected {
        "dialogsNameFgOver"
    } else {
        "dialogsNameFg"
    });
    let date_fg = colors.color(if state.active {
        "dialogsDateFgActive"
    } else if state.selected {
        "dialogsDateFgOver"
    } else {
        "dialogsDateFg"
    });
    let sub_fg = colors.color(if state.active {
        "dialogsTextFgActive"
    } else if state.selected {
        "dialogsTextFgOver"
    } else {
        "dialogsTextFg"
    });
    let service_fg = colors.color(if state.active {
        "dialogsTextFgServiceActive"
    } else if state.selected {
        "dialogsTextFgServiceOver"
    } else {
        "dialogsTextFgService"
    });

    // 5. Leading chat type icon (Group / Channel / Forum / Bot).
    if let Some(icon_r) = layout.chat_type_icon_rect {
        paint_chat_type_icon(
            painter,
            to_egui_rect(origin, icon_r),
            item.kind.chat_type_icon(),
            state.active,
            state.selected,
            colors,
        );
    }

    // 6. Top-right timestamp (`PaintRowTopRight`).
    if let Some(date_r) = layout.date_rect {
        let r = to_egui_rect(origin, date_r);
        painter.text(
            Pos2::new(r.right(), r.center().y),
            Align2::RIGHT_CENTER,
            date_text,
            font_id_from_style(st::dialogs_date_font),
            date_fg,
        );
    }

    // 7. Send state ticks / clock to the left of date.
    if let Some(ss_r) = layout.send_state_rect {
        let send_state = if let Some(draft) = item.effective_draft() {
            if draft.saving {
                DialogSendState::Sending
            } else {
                DialogSendState::None
            }
        } else {
            item.send_state
        };
        paint_send_state_icon(
            painter,
            to_egui_rect(origin, ss_r),
            send_state,
            state.active,
            state.selected,
            colors,
        );
    }

    // 8. Chat title (elided to `layout.name_rect.width` with semibold visual weight).
    let name_r = to_egui_rect(origin, layout.name_rect);
    let name_font = font_id_from_style(st::semibold_font);
    let title_str = prepared.elided_title.as_deref().unwrap_or(&item.title);
    paint_semibold_text(
        painter,
        Pos2::new(name_r.left(), name_r.center().y),
        Align2::LEFT_CENTER,
        title_str,
        name_font,
        name_fg,
    );

    // 9. Verified check, emoji status or Premium star immediately after title
    // (`PeerBadge::drawGetWidth`: a verified row paints only the check).
    if let Some(pb_r) = layout.peer_badge_rect {
        let r = to_egui_rect(origin, pb_r);
        if item.verified {
            paint_verified_badge(painter, r, state.active, state.selected, colors);
        } else if !paint_row_emoji_status(painter, r, item, state, colors) && item.premium {
            // Until the status document loads, a Premium peer keeps the star in its place.
            paint_premium_star(painter, r, state.active, state.selected, colors);
        }
    }

    // 10. Mute speaker icon next to title + badge.
    if let Some(mute_r) = layout.mute_icon_rect {
        paint_mute_icon(
            painter,
            to_egui_rect(origin, mute_r),
            state.active,
            state.selected,
            colors,
        );
    }

    // 11. Bottom-right badges (unread pill, mention @, reaction heart, poll, pin, bot "Open").
    let right_btn_rect =
        paint_row_badges(painter, origin, layout, item, counter_text, state, colors);

    // 12. Preview line: typing animation OR red "Draft: " prefix OR sender + last message.
    if let Some(typing_r) = layout.typing_rect
        && let Some(typing) = &item.typing
    {
        let label = prepared
            .typing_label
            .as_deref()
            .map_or_else(|| Cow::Owned(typing.label()), Cow::Borrowed);
        paint_typing_indicator(
            painter,
            to_egui_rect(origin, typing_r),
            &label,
            state.anim_time_secs,
            service_fg,
        );
    } else if let Some(msg_r) = layout.message_rect {
        let r = to_egui_rect(origin, msg_r);
        let preview_font = font_id_from_style(st::dialogs_text_font);
        let clipped = painter.with_clip_rect(
            painter
                .clip_rect()
                .intersect(r.expand2(Vec2::new(0.0, 2.0))),
        );
        if let Some(draft) = item.effective_draft() {
            let prefix = prepared
                .draft_prefix
                .clone()
                .unwrap_or_else(draft_prefix_text);
            let draft_fg = colors.draft_fg(state.active);
            let prefix_g = clipped.layout_no_wrap(prefix, preview_font.clone(), draft_fg);
            let prefix_w = prefix_g.size().x;
            clipped.galley(
                Pos2::new(r.left(), r.center().y - prefix_g.size().y * 0.5),
                prefix_g,
                draft_fg,
            );
            let avail = (r.width() - prefix_w).max(0.0) as i32;
            let body = elide_text(painter, &draft.text, avail, &preview_font);
            clipped.text(
                Pos2::new(r.left() + prefix_w, r.center().y),
                Align2::LEFT_CENTER,
                body,
                preview_font,
                sub_fg,
            );
        } else if !item.last_message.is_empty() {
            // Sender, 16 px mini image or type icon, blue media/service text
            // (`MessageView::paint`).
            paint_preview_line(
                &clipped,
                r,
                item,
                &preview_font,
                PreviewLineColors {
                    text: sub_fg,
                    service: service_fg,
                },
                colors,
            );
        }
        // No last message, or one that displays nothing (`item->isEmpty()`): tdesktop
        // paints no preview line (`Dialogs::Ui::PaintRow`).
    }

    // 13. Folder filter tags beneath preview (`ui/chat/chats_filter_tag.cpp`, `tagTop = 52px`).
    if state.show_folder_tags && !item.folder_tags.is_empty() {
        let style = row_style_for(item, true, false);
        paint_folder_tag_chips(
            painter,
            Pos2::new(
                origin.x + style.name_left as f32,
                origin.y + style.tag_top as f32,
            ),
            rect.right() - style.padding.right as f32,
            &item.folder_tags,
            state.active,
            colors,
        );
    }

    right_btn_rect
}

/// Paints the Archive row in either collapsed (37px) or expanded (62px) presentation
/// (`PaintCollapsedRow` and `PaintFolderEntryText` in `dialogs_layout.cpp`).
pub fn paint_archive_row(
    painter: &Painter,
    rect: Rect,
    summary: &ArchiveSummary,
    selected: bool,
    narrow: bool,
    palette: &Palette,
    colors: &DialogColors<'_>,
) {
    let _ = palette;
    let bg = if selected {
        colors.color("dialogsBgOver")
    } else {
        colors.color("dialogsBg")
    };
    painter.rect_filled(rect, CornerRadius::ZERO, bg);
    let archive_title = tg_lang::tr::lng_archived_name();

    if summary.collapsed {
        let unread_str = if summary.unread_count > 0 {
            summary.unread_count.to_string()
        } else {
            String::new()
        };
        let title_font = font_id_from_style(st::semibold_font);
        let badge_font = font_id_from_style(st::dialogs_unread_font);
        let input = DialogRowInput {
            style: DialogRowStyle::DEFAULT,
            mode: DialogRowMode::Collapsed,
            width: rect.width().round() as i32,
            name_text_width: measure_text_i32(painter, &archive_title, title_font.clone()),
            badges: tg_history::DialogBadgesState {
                unread_counter: summary.unread_count as i32,
                unread: summary.unread_count > 0,
                unread_muted: true,
                ..Default::default()
            },
            unread_counter_text_width: measure_text_i32(painter, &unread_str, badge_font),
            ..Default::default()
        };
        let layout = layout_dialog_row(&input, &FontMetrics::default());

        if narrow {
            let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(22.0));
            paint_archive_avatar(painter, icon_rect, colors);
        } else {
            let name_r = to_egui_rect(rect.min, layout.name_rect);
            paint_semibold_text(
                painter,
                Pos2::new(name_r.left(), rect.center().y),
                Align2::LEFT_CENTER,
                &archive_title,
                title_font,
                colors.color("dialogsNameFg"),
            );
            if let Some(badge_r) = layout.unread_badge_rect {
                let r = to_egui_rect(rect.min, badge_r);
                let style = UnreadBadgeStyle {
                    muted: true,
                    align: Align::Right,
                    ..UnreadBadgeStyle::default()
                };
                paint_unread_badge(
                    painter,
                    &unread_str,
                    Pos2::new(r.right(), r.top()),
                    &style,
                    UnreadBadgeKind::Dialogs,
                    0,
                    colors,
                );
            }
        }
    } else {
        // Expanded 62px Archive row (`st::defaultDialogRow`).
        let pad_left = st::default_dialog_row.padding.left.0 as f32;
        let pad_top = st::default_dialog_row.padding.top.0 as f32;
        let photo_size = st::default_dialog_row.photo_size.0 as f32;
        let photo_rect = Rect::from_min_size(
            Pos2::new(rect.left() + pad_left, rect.top() + pad_top),
            Vec2::splat(photo_size),
        );
        paint_archive_avatar(painter, photo_rect, colors);

        if narrow {
            if summary.unread_count > 0 {
                let badge_style = UnreadBadgeStyle {
                    muted: summary.all_muted,
                    align: Align::Right,
                    ..UnreadBadgeStyle::default()
                };
                paint_unread_badge(
                    painter,
                    &format_unread_counter(summary.unread_count as i32, false, true),
                    Pos2::new(photo_rect.right(), photo_rect.bottom() - 19.0),
                    &badge_style,
                    UnreadBadgeKind::Dialogs,
                    0,
                    colors,
                );
            }
        } else {
            let text_left = rect.left() + st::default_dialog_row.name_left.0 as f32;
            let right_edge = rect.right() - st::default_dialog_row.padding.right.0 as f32;

            paint_semibold_text(
                painter,
                Pos2::new(text_left, rect.top() + 19.0),
                Align2::LEFT_CENTER,
                &archive_title,
                font_id_from_style(st::semibold_font),
                colors.color("dialogsNameFg"),
            );

            let mut badge_reserved = 0.0_f32;
            if summary.unread_count > 0 {
                let badge_style = UnreadBadgeStyle {
                    muted: summary.all_muted,
                    align: Align::Right,
                    ..UnreadBadgeStyle::default()
                };
                let badge_rect = paint_unread_badge(
                    painter,
                    &summary.unread_count.to_string(),
                    Pos2::new(right_edge, rect.top() + 33.0),
                    &badge_style,
                    UnreadBadgeKind::Dialogs,
                    0,
                    colors,
                );
                badge_reserved = badge_rect.width() + 8.0;
            }

            let preview_joined = summary.preview_titles.join(", ");
            let preview_w = (right_edge - badge_reserved - text_left).max(0.0) as i32;
            let preview_font = font_id_from_style(st::dialogs_text_font);
            let elided = elide_text(painter, &preview_joined, preview_w, &preview_font);
            painter.text(
                Pos2::new(text_left, rect.top() + 43.0),
                Align2::LEFT_CENTER,
                elided,
                preview_font,
                colors.color("dialogsTextFg"),
            );
        }
    }

    // Subtle separator under Archive row.
    painter.line_segment(
        [
            Pos2::new(rect.left(), rect.bottom() - 0.5),
            Pos2::new(rect.right(), rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, colors.divider()),
    );
}

/// Paints a global peer search hit (`InnerWidget::paintPeerSearchResult` in `dialogs_inner_widget.cpp`).
pub fn paint_global_peer_row(
    painter: &Painter,
    rect: Rect,
    peer: &GlobalPeerResult,
    active: bool,
    selected: bool,
    palette: &Palette,
    colors: &DialogColors<'_>,
) {
    let _ = palette;
    let bg = if active {
        colors.color("dialogsBgActive")
    } else if selected {
        colors.color("dialogsBgOver")
    } else {
        colors.color("dialogsBg")
    };
    painter.rect_filled(rect, CornerRadius::ZERO, bg);

    let photo_rect = Rect::from_min_size(
        Pos2::new(
            rect.left() + st::default_dialog_row.padding.left.0 as f32,
            rect.top() + st::default_dialog_row.padding.top.0 as f32,
        ),
        Vec2::splat(st::default_dialog_row.photo_size.0 as f32),
    );
    paint_peer_userpic(
        painter,
        photo_rect,
        peer.peer_id,
        SpecialRowKind::None,
        peer.color_index,
        &peer.title,
        colors,
    );

    let name_fg = colors.color(if active {
        "dialogsNameFgActive"
    } else {
        "dialogsNameFg"
    });
    let sub_fg = colors.color(if active {
        "dialogsTextFgActive"
    } else {
        "dialogsTextFg"
    });

    let mut name_left = rect.left() + st::default_dialog_row.name_left.0 as f32;
    let right_edge = rect.right() - st::default_dialog_row.padding.right.0 as f32;
    let chat_icon = peer.kind.chat_type_icon();
    if !matches!(chat_icon, DialogChatTypeIcon::None) {
        paint_chat_type_icon(
            painter,
            Rect::from_min_size(
                Pos2::new(name_left, rect.top() + 10.0),
                Vec2::new(16.0, 18.0),
            ),
            chat_icon,
            active,
            selected,
            colors,
        );
        name_left += 19.0;
    }

    let badge_w = if peer.verified || peer.premium {
        18.0
    } else {
        0.0
    };
    let name_max_w = (right_edge - name_left - badge_w).max(16.0) as i32;
    let name_font = font_id_from_style(st::semibold_font);
    let elided_title = elide_text(painter, &peer.title, name_max_w, &name_font);
    let actual_w = measure_text_i32(painter, &elided_title, name_font.clone()) as f32;
    paint_semibold_text(
        painter,
        Pos2::new(name_left, rect.top() + 19.0),
        Align2::LEFT_CENTER,
        &elided_title,
        name_font,
        name_fg,
    );

    if peer.verified {
        paint_verified_badge(
            painter,
            Rect::from_min_size(
                Pos2::new(name_left + actual_w + 3.0, rect.top() + 10.0),
                Vec2::new(16.0, 18.0),
            ),
            active,
            selected,
            colors,
        );
    } else if peer.premium {
        paint_premium_star(
            painter,
            Rect::from_min_size(
                Pos2::new(name_left + actual_w + 3.0, rect.top() + 10.0),
                Vec2::new(16.0, 18.0),
            ),
            active,
            selected,
            colors,
        );
    }

    let sub_font = font_id_from_style(st::dialogs_text_font);
    let sub_max_w =
        (right_edge - (rect.left() + st::default_dialog_row.name_left.0 as f32)).max(16.0) as i32;
    let elided_sub = elide_text(painter, &peer.subtitle, sub_max_w, &sub_font);
    painter.text(
        Pos2::new(
            rect.left() + st::default_dialog_row.name_left.0 as f32,
            rect.top() + 43.0,
        ),
        Align2::LEFT_CENTER,
        elided_sub,
        sub_font,
        sub_fg,
    );
}

/// Paints a sponsored search peer hit (`contacts.getSponsoredPeers`, `dialogs_inner_widget.cpp:1641-1670`)
/// with a right-aligned `"Ad"` badge pill (`lng_search_sponsored_button`), returning the badge rect.
#[allow(clippy::too_many_arguments)]
pub fn paint_sponsored_peer_row(
    painter: &Painter,
    rect: Rect,
    peer: &tg_app_core::core_types::dto::contacts_extras::SponsoredPeerInfo,
    active: bool,
    selected: bool,
    badge_hovered: bool,
    palette: &Palette,
    colors: &DialogColors<'_>,
) -> Rect {
    let _ = palette;
    let bg = if active {
        colors.color("dialogsBgActive")
    } else if selected {
        colors.color("dialogsBgOver")
    } else {
        colors.color("dialogsBg")
    };
    painter.rect_filled(rect, CornerRadius::ZERO, bg);

    let photo_rect = Rect::from_min_size(
        Pos2::new(
            rect.left() + st::default_dialog_row.padding.left.0 as f32,
            rect.top() + st::default_dialog_row.padding.top.0 as f32,
        ),
        Vec2::splat(st::default_dialog_row.photo_size.0 as f32),
    );
    paint_peer_userpic(
        painter,
        photo_rect,
        peer.peer_id,
        SpecialRowKind::None,
        peer.color_index,
        &peer.title,
        colors,
    );

    let name_fg = colors.color(if active {
        "dialogsNameFgActive"
    } else {
        "dialogsNameFg"
    });
    let sub_fg = colors.color(if active {
        "dialogsTextFgActive"
    } else {
        "dialogsTextFg"
    });

    let right_edge = rect.right() - st::default_dialog_row.padding.right.0 as f32;
    let raw_ad_label = tg_lang::tr::lng_search_sponsored_button();
    let ad_label = raw_ad_label
        .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '⋮' | '⋯' | '…' | '.'));
    let ad_label = if ad_label.is_empty() { "Ad" } else { ad_label };
    let ad_font = FontId::new(11.0, family_semibold());
    let ad_text_w = measure_text_i32(painter, ad_label, ad_font.clone()) as f32;
    let ad_w = (ad_text_w + 22.0).max(40.0);
    let ad_h = 20.0;
    let ad_rect = Rect::from_min_size(
        Pos2::new(right_edge - ad_w, rect.top() + 9.0),
        Vec2::new(ad_w, ad_h),
    );
    let accent = colors.color("windowBgActive");
    let ad_fill = if active {
        Color32::from_rgba_unmultiplied(255, 255, 255, if badge_hovered { 68 } else { 44 })
    } else {
        Color32::from_rgba_unmultiplied(
            accent.r(),
            accent.g(),
            accent.b(),
            if badge_hovered { 52 } else { 30 },
        )
    };
    let ad_fg = if active {
        Color32::WHITE
    } else {
        colors.active_text_fg()
    };
    painter.rect_filled(ad_rect, CornerRadius::same(10), ad_fill);
    painter.text(
        Pos2::new(ad_rect.left() + 8.0, ad_rect.center().y),
        Align2::LEFT_CENTER,
        ad_label,
        ad_font,
        ad_fg,
    );
    // Crisp 3-dot disclosure glyph painted as vectors (never depends on U+22EE font coverage).
    let dots_x = ad_rect.right() - 8.0;
    let dots_cy = ad_rect.center().y;
    for dy in [-3.4_f32, 0.0, 3.4] {
        painter.circle_filled(Pos2::new(dots_x, dots_cy + dy), 1.15, ad_fg);
    }

    let name_left = rect.left() + st::default_dialog_row.name_left.0 as f32;
    let badge_w = if peer.verified || peer.premium {
        18.0
    } else {
        0.0
    };
    let name_max_w = (ad_rect.left() - 8.0 - name_left - badge_w).max(16.0) as i32;
    let name_font = font_id_from_style(st::semibold_font);
    let elided_title = elide_text(painter, &peer.title, name_max_w, &name_font);
    let actual_w = measure_text_i32(painter, &elided_title, name_font.clone()) as f32;
    paint_semibold_text(
        painter,
        Pos2::new(name_left, rect.top() + 19.0),
        Align2::LEFT_CENTER,
        &elided_title,
        name_font,
        name_fg,
    );

    if peer.verified {
        paint_verified_badge(
            painter,
            Rect::from_min_size(
                Pos2::new(name_left + actual_w + 3.0, rect.top() + 10.0),
                Vec2::new(16.0, 18.0),
            ),
            active,
            selected,
            colors,
        );
    } else if peer.premium {
        paint_premium_star(
            painter,
            Rect::from_min_size(
                Pos2::new(name_left + actual_w + 3.0, rect.top() + 10.0),
                Vec2::new(16.0, 18.0),
            ),
            active,
            selected,
            colors,
        );
    }

    let subtitle = match (&peer.username, &peer.sponsor_info) {
        (Some(u), Some(info)) if !u.is_empty() && !info.is_empty() => format!("@{u} · {info}"),
        (Some(u), _) if !u.is_empty() => format!("@{u}"),
        (_, Some(info)) if !info.is_empty() => info.clone(),
        (_, _) => peer
            .additional_info
            .clone()
            .unwrap_or_else(|| "Sponsored".to_owned()),
    };
    let sub_font = font_id_from_style(st::dialogs_text_font);
    let sub_max_w = (right_edge - name_left).max(16.0) as i32;
    let elided_sub = elide_text(painter, &subtitle, sub_max_w, &sub_font);
    painter.text(
        Pos2::new(name_left, rect.top() + 43.0),
        Align2::LEFT_CENTER,
        elided_sub,
        sub_font,
        sub_fg,
    );

    ad_rect
}

/// Paints a message search result row with query occurrences highlighted (`RowPainter::Paint(FakeRow)`).
#[allow(clippy::too_many_arguments)]
pub fn paint_message_search_row(
    painter: &Painter,
    rect: Rect,
    msg: &MessageSearchResult,
    query: &str,
    now: LocalDateTime,
    active: bool,
    selected: bool,
    use_24h: bool,
    palette: &Palette,
    colors: &DialogColors<'_>,
) {
    let _ = palette;
    let bg = if active {
        colors.color("dialogsBgActive")
    } else if selected {
        colors.color("dialogsBgOver")
    } else {
        colors.color("dialogsBg")
    };
    painter.rect_filled(rect, CornerRadius::ZERO, bg);

    let photo_rect = Rect::from_min_size(
        Pos2::new(
            rect.left() + st::default_dialog_row.padding.left.0 as f32,
            rect.top() + st::default_dialog_row.padding.top.0 as f32,
        ),
        Vec2::splat(st::default_dialog_row.photo_size.0 as f32),
    );
    paint_peer_userpic(
        painter,
        photo_rect,
        msg.peer_id,
        msg.special,
        msg.color_index,
        &msg.peer_title,
        colors,
    );

    let name_fg = colors.color(if active {
        "dialogsNameFgActive"
    } else {
        "dialogsNameFg"
    });
    let sub_fg = colors.color(if active {
        "dialogsTextFgActive"
    } else {
        "dialogsTextFg"
    });
    let service_fg = colors.color(if active {
        "dialogsTextFgServiceActive"
    } else {
        "dialogsTextFgService"
    });
    let date_fg = colors.color(if active {
        "dialogsDateFgActive"
    } else {
        "dialogsDateFg"
    });

    let text_left = rect.left() + st::default_dialog_row.name_left.0 as f32;
    let right_edge = rect.right() - st::default_dialog_row.padding.right.0 as f32;

    let date_text = format_localized_dialogs_date(msg.time, now, use_24h);
    let date_font = font_id_from_style(st::dialogs_date_font);
    let date_w = measure_text_i32(painter, &date_text, date_font.clone()) as f32;
    painter.text(
        Pos2::new(right_edge, rect.top() + 19.0),
        Align2::RIGHT_CENTER,
        &date_text,
        date_font,
        date_fg,
    );
    // Ticks left of the date, placed like `layout_dialog_row`'s `send_state_rect`.
    let mut top_right_w = date_w;
    if msg.send_state != DialogSendState::None {
        let skip = tg_history::consts::DIALOGS_SEND_STATE_SKIP as f32;
        let date_skip = tg_history::consts::DIALOGS_DATE_SKIP as f32;
        let ss_rect = Rect::from_min_size(
            Pos2::new(
                right_edge - date_w - date_skip - skip,
                rect.top() + st::default_dialog_row.name_top.0 as f32,
            ),
            Vec2::new(skip, FontMetrics::default().semibold.height as f32),
        );
        paint_send_state_icon(painter, ss_rect, msg.send_state, active, selected, colors);
        top_right_w += skip;
    }

    let mut name_x = text_left;
    let chat_icon = if msg.special == SpecialRowKind::None {
        msg.kind.chat_type_icon()
    } else {
        DialogChatTypeIcon::None
    };
    if !matches!(chat_icon, DialogChatTypeIcon::None) {
        paint_chat_type_icon(
            painter,
            Rect::from_min_size(Pos2::new(name_x, rect.top() + 10.0), Vec2::new(16.0, 18.0)),
            chat_icon,
            active,
            selected,
            colors,
        );
        name_x += 19.0;
    }

    let name_font = font_id_from_style(st::semibold_font);
    let name_avail = (right_edge - top_right_w - 8.0 - name_x).max(16.0) as i32;
    let elided_name = elide_text(painter, &msg.peer_title, name_avail, &name_font);
    paint_semibold_text(
        painter,
        Pos2::new(name_x, rect.top() + 19.0),
        Align2::LEFT_CENTER,
        &elided_name,
        name_font,
        name_fg,
    );

    // Highlighted snippet on line 2.
    let clip_rect = Rect::from_min_size(
        Pos2::new(text_left, rect.top() + 33.0),
        Vec2::new((right_edge - text_left).max(16.0), 20.0),
    );
    let clipped = painter.with_clip_rect(painter.clip_rect().intersect(clip_rect));
    let preview_font = font_id_from_style(st::dialogs_text_font);
    let mut x = text_left;
    // `MessageView::prepare`: only group chats show the sender before the text.
    let show_sender = matches!(msg.kind, DialogPeerKind::Group | DialogPeerKind::Forum)
        && msg.special == SpecialRowKind::None;
    let sender = msg
        .sender_name
        .as_deref()
        .filter(|s| show_sender && !s.is_empty());

    // Poll / checklist results start with tdesktop's icon emoji instead of the "📊 " marker.
    let (marker, msg_text) = match split_media_marker(&msg.text) {
        Some((icon, rest)) => (Some(icon), rest),
        None => (None, msg.text.as_str()),
    };
    // Photo / video / file results draw the chat list's mini image or type icon
    // (`MessageView::prepare` with `ItemPreview` images).
    let media = msg.media_preview.as_ref().filter(|p| {
        marker.is_none()
            && (crate::dialogs::mini_preview::preview_has_image(clipped.ctx(), Some(p))
                || crate::dialogs::mini_preview::preview_type_icon(p.kind).is_some())
    });
    let line_h = clipped
        .layout_no_wrap("Ag".to_owned(), preview_font.clone(), sub_fg)
        .size()
        .y;
    let line_top = clip_rect.center().y - line_h * 0.5;
    // With a left icon the sender is cut off into `_senderCache` and drawn before it;
    // otherwise it stays inside the text and counts in the search cut (`PreviewWithSender`).
    let inline_sender = if let Some(preview) = media {
        let has_images =
            crate::dialogs::mini_preview::preview_has_image(clipped.ctx(), Some(preview));
        if let Some(sender) = sender {
            let wrapped = tg_lang::tr::lng_dialogs_text_from_wrapped(sender);
            let prefix = if has_images {
                wrapped
            } else {
                format!("{wrapped} ")
            };
            let g = clipped.layout_no_wrap(prefix, preview_font.clone(), service_fg);
            let w = g.size().x;
            clipped.galley(Pos2::new(x, line_top), g, service_fg);
            x += w;
            if has_images {
                x += (st::dialogs_mini_preview_skip.0 + st::dialogs_mini_preview_right.0) as f32;
            }
        }
        let colored =
            crate::dialogs::mini_preview::colorized_prefix_len(Some(preview), has_images, msg_text);
        x += crate::dialogs::mini_preview::paint_leading_media(
            &clipped,
            x,
            line_top,
            clip_rect.right(),
            preview,
            if colored > 0 { service_fg } else { sub_fg },
            colors,
        );
        None
    } else if let Some(icon) = marker {
        if let Some(sender) = sender {
            let prefix = format!("{} ", tg_lang::tr::lng_dialogs_text_from_wrapped(sender));
            let g = clipped.layout_no_wrap(prefix, preview_font.clone(), service_fg);
            let w = g.size().x;
            clipped.galley(
                Pos2::new(x, clip_rect.center().y - g.size().y * 0.5),
                g,
                service_fg,
            );
            x += w;
        }
        let line_h = clipped
            .layout_no_wrap("Ag".to_owned(), preview_font.clone(), sub_fg)
            .size()
            .y;
        x += paint_media_marker(
            &clipped,
            Pos2::new(x, clip_rect.center().y - line_h * 0.5),
            icon,
            service_fg,
            colors,
        );
        None
    } else {
        sender
    };
    let mut snippet = message_search_snippet(inline_sender, msg_text, query);
    // Caption-less media ("Photo") and service text are drawn in `dialogsTextFgService`.
    if let Some(preview) = media {
        let has_images =
            crate::dialogs::mini_preview::preview_has_image(clipped.ctx(), Some(preview));
        let colored =
            crate::dialogs::mini_preview::colorized_prefix_len(Some(preview), has_images, msg_text);
        if colored > 0 && colored >= msg_text.len() {
            for run in &mut snippet.runs {
                run.service = true;
            }
        }
    }
    let job = snippet.layout_job(
        &preview_font,
        sub_fg,
        service_fg,
        (clip_rect.right() - x).max(1.0),
    );
    let galley = clipped.layout_job(job);
    clipped.galley(
        Pos2::new(x, clip_rect.center().y - galley.size().y * 0.5),
        galley,
        sub_fg,
    );
}

/// Finds all case-insensitive non-overlapping UTF-8 byte ranges of `query` inside `text`.
///
/// Pure logic helper used for message search query highlighting and unit tests.
#[must_use]
pub fn find_highlight_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let lower_chars: Vec<(usize, char)> = text
        .char_indices()
        .flat_map(|(byte_idx, ch)| ch.to_lowercase().map(move |lc| (byte_idx, lc)))
        .collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let nlen = needle_chars.len();
    if nlen == 0 || lower_chars.len() < nlen {
        return ranges;
    }
    let mut i = 0usize;
    while i + nlen <= lower_chars.len() {
        if lower_chars[i..i + nlen]
            .iter()
            .zip(needle_chars.iter())
            .all(|((_, a), b)| a == b)
        {
            let start = lower_chars[i].0;
            let end = if i + nlen < lower_chars.len() {
                lower_chars[i + nlen].0
            } else {
                text.len()
            };
            if end > start && text.is_char_boundary(start) && text.is_char_boundary(end) {
                ranges.push(start..end);
            }
            i += nlen;
        } else {
            i += 1;
        }
    }
    ranges
}

/// Search-result snippet: matches are `EntityType::Colorized` (`MessageView::prepare` with
/// `searchLowerText`), drawn in the dialogs text palette's `linkFg` (`dialogsTextFgService`,
/// `...Active` on the selected row) with no background.
#[must_use]
pub fn search_snippet_job(
    snippet: &str,
    query: &str,
    font_id: FontId,
    text_fg: Color32,
    service_fg: Color32,
) -> LayoutJob {
    build_highlighted_snippet_job(
        snippet,
        query,
        font_id,
        text_fg,
        service_fg,
        Color32::TRANSPARENT,
    )
}

/// Builds a single-line [`LayoutJob`] with all occurrences of `query` highlighted.
#[must_use]
pub fn build_highlighted_snippet_job(
    text: &str,
    query: &str,
    font_id: FontId,
    base_fg: Color32,
    highlight_fg: Color32,
    highlight_bg: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let ranges = find_highlight_ranges(text, query);
    let mut cursor = 0usize;
    for r in ranges {
        if r.start > cursor {
            job.append(
                &text[cursor..r.start],
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: base_fg,
                    ..Default::default()
                },
            );
        }
        job.append(
            &text[r.start..r.end],
            0.0,
            TextFormat {
                font_id: font_id.clone(),
                color: highlight_fg,
                background: highlight_bg,
                ..Default::default()
            },
        );
        cursor = r.end;
    }
    if cursor < text.len() {
        job.append(
            &text[cursor..],
            0.0,
            TextFormat {
                font_id,
                color: base_fg,
                ..Default::default()
            },
        );
    }
    job
}

/// Paints a section header bar (`searchedBarBg` + `searchedBarFg`, 29px tall).
pub fn paint_section_bar(
    painter: &Painter,
    rect: Rect,
    title: &str,
    right_action: Option<(&str, bool)>,
    colors: &DialogColors<'_>,
) -> Option<Rect> {
    painter.rect_filled(rect, CornerRadius::ZERO, colors.searched_bar_bg());
    let font = font_id_from_style(st::searched_bar_font);
    let fg = colors.searched_bar_fg();
    let pad_x = st::searched_bar_position.x.0 as f32;
    paint_semibold_text(
        painter,
        Pos2::new(rect.left() + pad_x, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        font.clone(),
        fg,
    );

    if let Some((label, hovered)) = right_action {
        let g = painter.layout_no_wrap(label.to_owned(), font.clone(), fg);
        let action_rect = Rect::from_min_size(
            Pos2::new(rect.right() - pad_x - g.size().x, rect.top()),
            Vec2::new(g.size().x + 6.0, rect.height()),
        );
        painter.text(
            Pos2::new(rect.right() - pad_x, rect.center().y),
            Align2::RIGHT_CENTER,
            label,
            font,
            fg,
        );
        if hovered {
            let y = rect.center().y + g.size().y * 0.45;
            painter.line_segment(
                [
                    Pos2::new(rect.right() - pad_x - g.size().x, y),
                    Pos2::new(rect.right() - pad_x, y),
                ],
                Stroke::new(1.0, fg),
            );
        }
        return Some(action_rect);
    }
    None
}

// ---------------------------------------------------------------------------
// Internal helpers: avatars, badges, icons, typing animation, filter tags
// ---------------------------------------------------------------------------

/// Paints `peer_id`'s real photo userpic when it is loaded (`PeerData::paintUserpic`), else the
/// avatar of [`paint_peer_avatar`]. Special rows always keep their special avatar.
#[allow(clippy::too_many_arguments)]
pub fn paint_peer_userpic(
    painter: &Painter,
    rect: Rect,
    peer_id: tg_app_core::stand_in::types::PeerId,
    special: SpecialRowKind,
    color_index: u8,
    title: &str,
    colors: &DialogColors<'_>,
) {
    if matches!(special, SpecialRowKind::None)
        && tg_ui::userpic_images::paint_userpic_image(painter, rect, peer_id.0)
    {
        return;
    }
    paint_peer_avatar(painter, rect, special, color_index, title, colors);
}

/// `Ui::ForumUserpicRadiusMultiplier()`: corner radius of a forum userpic per pixel of its size.
pub const FORUM_USERPIC_RADIUS_MULTIPLIER: f32 = 0.3;

/// Paints an avatar for a normal peer or special row (`SavedMessages`, `Replies`, `VerificationCodes`).
pub fn paint_peer_avatar(
    painter: &Painter,
    rect: Rect,
    special: SpecialRowKind,
    color_index: u8,
    title: &str,
    colors: &DialogColors<'_>,
) {
    match special {
        SpecialRowKind::SavedMessages => {
            EmptyUserpic::paint_saved_messages(painter, rect, colors);
        }
        SpecialRowKind::Replies => {
            let top = colors.color("historyPeerSavedMessagesBg");
            let bottom = colors.color("historyPeerSavedMessagesBg2");
            paint_vertical_gradient_circle(painter, rect, top, bottom);
            paint_style_icon(
                painter,
                rect.shrink(rect.width() * 0.22),
                st::dialogs_replies_userpic,
                colors,
                Some(Color32::WHITE),
            );
        }
        SpecialRowKind::VerificationCodes => {
            let top = colors.color("historyPeerSavedMessagesBg");
            let bottom = colors.color("historyPeerSavedMessagesBg2");
            paint_vertical_gradient_circle(painter, rect, top, bottom);
            paint_style_icon(
                painter,
                rect.shrink(rect.width() * 0.22),
                st::dialogs_my_notes_userpic,
                colors,
                Some(Color32::WHITE),
            );
        }
        SpecialRowKind::None => {
            EmptyUserpic::for_color_index(color_index, title, colors).paint_circle(painter, rect);
        }
    }
}

/// Paints the gray Archive box circular avatar (`historyPeerArchiveUserpicBg` + `st::dialogsArchiveUserpic`).
pub fn paint_archive_avatar(painter: &Painter, rect: Rect, colors: &DialogColors<'_>) {
    let bg = colors.color("historyPeerArchiveUserpicBg");
    painter.circle_filled(rect.center(), rect.width().min(rect.height()) * 0.5, bg);
    paint_style_icon(
        painter,
        rect.shrink(rect.width() * 0.20),
        st::dialogs_archive_userpic,
        colors,
        Some(Color32::WHITE),
    );
}

fn paint_online_dot(painter: &Painter, rect: Rect, bg_cutout: Color32, dot_color: Color32) {
    let radius = rect.width().min(rect.height()) * 0.5;
    let stroke_w = st::dialogs_online_badge_stroke.0 as f32;
    painter.circle_filled(rect.center(), radius + stroke_w, bg_cutout);
    painter.circle_filled(rect.center(), radius, dot_color);
}

fn paint_active_call_badge(painter: &Painter, rect: Rect, bg_cutout: Color32, accent: Color32) {
    let r = rect.width().min(rect.height()) * 0.5;
    let c = rect.center();
    painter.circle_filled(c, r + 1.8, bg_cutout);
    painter.circle_filled(c, r, accent);
    let stroke = Stroke::new(1.5, Color32::WHITE);
    for (dx, half_h) in [(-3.0, 2.2), (0.0, 4.0), (3.0, 2.6)] {
        painter.line_segment(
            [
                Pos2::new(c.x + dx, c.y - half_h),
                Pos2::new(c.x + dx, c.y + half_h),
            ],
            stroke,
        );
    }
}

fn paint_row_badges(
    painter: &Painter,
    origin: Pos2,
    layout: &DialogRowLayout,
    item: &DialogItem,
    counter_text: &str,
    state: RowPaintState,
    colors: &DialogColors<'_>,
) -> Option<Rect> {
    if let Some(unread_r) = layout.unread_badge_rect {
        let r = to_egui_rect(origin, unread_r);
        if item.badges.unread_counter <= 0 {
            let dot_bg = if state.active {
                colors.color("dialogsUnreadBgActive")
            } else if item.badges.unread_muted {
                colors.color("dialogsUnreadBgMuted")
            } else {
                colors.color("dialogsUnreadBg")
            };
            painter.circle_filled(r.center(), r.width().min(r.height()) * 0.5, dot_bg);
        } else {
            let style = UnreadBadgeStyle {
                muted: item.badges.unread_muted,
                active: state.active,
                selected: state.selected,
                align: Align::Right,
                ..UnreadBadgeStyle::default()
            };
            paint_unread_badge(
                painter,
                counter_text,
                Pos2::new(r.right(), r.top()),
                &style,
                UnreadBadgeKind::Dialogs,
                0,
                colors,
            );
        }
    }

    if let Some(mention_r) = layout.mention_badge_rect {
        let r = to_egui_rect(origin, mention_r);
        let bg = if state.active {
            colors.color("dialogsUnreadBgActive")
        } else if item.badges.mention_muted {
            colors.color("dialogsUnreadBgMuted")
        } else {
            colors.color("dialogsUnreadBg")
        };
        painter.circle_filled(r.center(), r.width().min(r.height()) * 0.5, bg);
        let icon = pick_three_state(
            if item.badges.mention_muted {
                st::dialogs_unread_mention_badge_muted
            } else {
                st::dialogs_unread_mention_badge
            },
            state.active,
            state.selected,
        );
        paint_style_icon(painter, r.shrink(3.0), icon, colors, None);
    }

    if let Some(reaction_r) = layout.reaction_badge_rect {
        let r = to_egui_rect(origin, reaction_r);
        let bg = if state.active {
            colors.color("dialogsUnreadBgActive")
        } else if item.badges.reaction_muted {
            colors.color("dialogsUnreadBgMuted")
        } else {
            colors.color("dialogsUnreadBg")
        };
        painter.circle_filled(r.center(), r.width().min(r.height()) * 0.5, bg);
        let icon = pick_three_state(
            if item.badges.reaction_muted {
                st::dialogs_unread_reaction_badge_muted
            } else {
                st::dialogs_unread_reaction_badge
            },
            state.active,
            state.selected,
        );
        paint_style_icon(painter, r.shrink(3.0), icon, colors, None);
    }

    if let Some(poll_r) = layout.poll_badge_rect {
        let r = to_egui_rect(origin, poll_r);
        let bg = if state.active {
            colors.color("dialogsUnreadBgActive")
        } else if item.badges.poll_muted {
            colors.color("dialogsUnreadBgMuted")
        } else {
            colors.color("dialogsUnreadBg")
        };
        painter.circle_filled(r.center(), r.width().min(r.height()) * 0.5, bg);
        let icon = pick_three_state(
            if item.badges.poll_muted {
                st::dialogs_unread_poll_muted
            } else {
                st::dialogs_unread_poll
            },
            state.active,
            state.selected,
        );
        paint_style_icon(painter, r, icon, colors, None);
    }

    if let Some(pin_r) = layout.pinned_icon_rect {
        let r = to_egui_rect(origin, pin_r);
        let icon = pick_three_state(st::dialogs_pinned_icon, state.active, state.selected);
        paint_style_icon(painter, r, icon, colors, None);
    }

    if let Some(btn_r) = layout.right_button_rect {
        let r = to_egui_rect(origin, btn_r);
        let bg = if state.active {
            colors.color("dialogsUnreadBgActive")
        } else {
            colors.color("windowBgActive")
        };
        let fg = if state.active {
            colors.color("dialogsBgActive")
        } else {
            colors.color("windowFgActive")
        };
        painter.rect_filled(r, CornerRadius::same(10), bg);
        paint_semibold_text(
            painter,
            r.center(),
            Align2::CENTER_CENTER,
            &tg_lang::tr::lng_profile_open_app_short(),
            FontId::new(11.0, family_semibold()),
            fg,
        );
        return Some(r);
    }

    None
}

fn paint_chat_type_icon(
    painter: &Painter,
    rect: Rect,
    kind: DialogChatTypeIcon,
    active: bool,
    selected: bool,
    colors: &DialogColors<'_>,
) {
    let icon = match kind {
        DialogChatTypeIcon::None => return,
        DialogChatTypeIcon::Group => pick_three_state(st::dialogs_chat_icon, active, selected),
        DialogChatTypeIcon::Channel => pick_three_state(st::dialogs_channel_icon, active, selected),
        DialogChatTypeIcon::Forum => pick_three_state(st::dialogs_forum_icon, active, selected),
        DialogChatTypeIcon::Bot => pick_three_state(st::dialogs_bot_icon, active, selected),
    };
    paint_style_icon(painter, rect, icon, colors, None);
}

fn paint_verified_badge(
    painter: &Painter,
    rect: Rect,
    active: bool,
    selected: bool,
    colors: &DialogColors<'_>,
) {
    let icon = if active {
        st::dialogs_verified_icon_active
    } else if selected {
        st::dialogs_verified_icon_over
    } else {
        st::dialogs_verified_icon
    };
    paint_style_icon(painter, rect, icon, colors, None);
}

/// Width of the badge after a chat-list row's name (`PeerBadge::drawGetWidth`): the verified
/// check wins over an emoji status, which takes [`emoji_status::ROW_STATUS_WIDTH`]; otherwise the
/// Premium star.
#[must_use]
pub fn row_peer_badge_width(item: &DialogItem) -> i32 {
    if item.verified {
        18
    } else if item.emoji_status_id.is_some() {
        emoji_status::ROW_STATUS_WIDTH
    } else if item.premium {
        18
    } else {
        0
    }
}

/// Paints the row's emoji status in the badge rect `r`; `false` when the row has none or its
/// document is not loaded yet. Tinted with `premiumFg` (`dialogsVerifiedIconBg*`).
fn paint_row_emoji_status(
    painter: &Painter,
    r: Rect,
    item: &DialogItem,
    state: RowPaintState,
    colors: &DialogColors<'_>,
) -> bool {
    let Some(document_id) = item.emoji_status_id else {
        return false;
    };
    let tint = colors.color(if state.active {
        "dialogsVerifiedIconBgActive"
    } else if state.selected {
        "dialogsVerifiedIconBgOver"
    } else {
        "dialogsVerifiedIconBg"
    });
    emoji_status::paint_status(
        painter,
        Id::unique(("tg_dialog_row_status", item.peer_id.0)),
        document_id,
        emoji_status::status_rect(r.min),
        tint,
        state.anim_time_secs,
        emoji_status::StatusLoops::Limited(emoji_status::ROW_PLAY_LOOPS),
    )
}

fn paint_premium_star(
    painter: &Painter,
    rect: Rect,
    active: bool,
    selected: bool,
    colors: &DialogColors<'_>,
) {
    let icon = pick_three_state(st::dialogs_premium_icon, active, selected);
    paint_style_icon(painter, rect, icon, colors, None);
}

fn paint_mute_icon(
    painter: &Painter,
    rect: Rect,
    active: bool,
    selected: bool,
    colors: &DialogColors<'_>,
) {
    let icon = pick_three_state(st::dialogs_mute_icon, active, selected);
    paint_style_icon(painter, rect, icon, colors, None);
}

fn paint_send_state_icon(
    painter: &Painter,
    rect: Rect,
    state: DialogSendState,
    active: bool,
    selected: bool,
    colors: &DialogColors<'_>,
) {
    let icon = match state {
        DialogSendState::None => return,
        DialogSendState::Sending => pick_three_state(st::dialogs_sending_icon, active, selected),
        DialogSendState::Sent => pick_three_state(st::dialogs_sent_icon, active, selected),
        DialogSendState::Received => pick_three_state(st::dialogs_received_icon, active, selected),
        DialogSendState::Locked => pick_three_state(st::dialogs_lock_icon, active, selected),
    };
    paint_style_icon(painter, rect, icon, colors, None);
}

fn paint_typing_indicator(
    painter: &Painter,
    rect: Rect,
    label: &str,
    anim_time_secs: f64,
    fg: Color32,
) {
    // Three smoothly pulsing dots followed by the activity text (`SendActionPainter`).
    let phase = (anim_time_secs * 3.2) % 3.0;
    let dot_y = rect.center().y + 1.0;
    for i in 0..3 {
        let dist = ((phase - i as f64).abs()).min((phase - i as f64 + 3.0).abs());
        let scale = (1.0 - (dist / 1.1).min(1.0)) as f32;
        let radius = 1.6 + 1.1 * scale;
        let alpha = (120.0 + 135.0 * scale) as u8;
        let dot_color = Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), alpha);
        painter.circle_filled(
            Pos2::new(rect.left() + 3.0 + i as f32 * 5.5, dot_y),
            radius,
            dot_color,
        );
    }

    let text_left = rect.left() + 21.0;
    let avail = (rect.right() - text_left).max(0.0) as i32;
    let font = font_id_from_style(st::dialogs_text_font);
    let elided = elide_text(painter, label, avail, &font);
    painter.text(
        Pos2::new(text_left, rect.center().y),
        Align2::LEFT_CENTER,
        elided,
        font,
        fg,
    );
}

/// Paints compact colored folder tag pills (`ui/chat/chats_filter_tag.cpp`).
fn paint_folder_tag_chips(
    painter: &Painter,
    origin: Pos2,
    max_right: f32,
    tags: &[FolderTagBadge],
    row_active: bool,
    colors: &DialogColors<'_>,
) {
    let font = FontId::new(10.0, family_regular());
    let mut x = origin.x;
    for tag in tags {
        let fg = if row_active {
            colors.color("dialogsNameFgActive")
        } else {
            peer_name_color(tag.color_index, colors)
        };
        let bg = if row_active {
            Color32::from_rgba_unmultiplied(255, 255, 255, 42)
        } else {
            let pair = userpic_colors(tag.color_index, colors);
            Color32::from_rgba_unmultiplied(pair.color2.r(), pair.color2.g(), pair.color2.b(), 34)
        };
        let g = painter.layout_no_wrap(tag.title.clone(), font.clone(), fg);
        let pill_w = g.size().x + 10.0;
        let pill_h = 15.0;
        if x + pill_w > max_right {
            break;
        }
        let pill = Rect::from_min_size(Pos2::new(x, origin.y), Vec2::new(pill_w, pill_h));
        painter.rect_filled(pill, CornerRadius::same(4), bg);
        painter.galley(
            Pos2::new(pill.left() + 5.0, pill.center().y - g.size().y * 0.5),
            g,
            fg,
        );
        x += pill_w + 4.0;
    }
}

/// Draws text using tdesktop's bundled Open Sans SemiBold font family.
pub fn paint_semibold_text(
    painter: &Painter,
    pos: Pos2,
    anchor: Align2,
    text: &str,
    font_id: FontId,
    color: Color32,
) {
    let semibold = FontId::new(font_id.size, family_semibold());
    painter.text(pos, anchor, text, semibold, color);
}

/// Measures single-line text width rounded to logical integer pixels.
#[must_use]
pub fn measure_text_i32(painter: &Painter, text: &str, font_id: FontId) -> i32 {
    if text.is_empty() {
        return 0;
    }
    painter
        .layout_no_wrap(text.to_owned(), font_id, Color32::WHITE)
        .size()
        .x
        .ceil() as i32
}

/// Everything outside a row's own data that its measured text depends on: the fonts, the display
/// scale and the language. Caches of measured or elided text are dropped when it changes.
///
/// egui has no counter of font changes (fonts registered in one frame apply from the next one),
/// so the fonts are identified by the width of a probe string in the two chat-list families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEpoch {
    font_probes: [u32; 2],
    pixels_per_point: u32,
    language: u64,
}

/// The [`TextEpoch`] of this frame.
#[must_use]
pub fn text_epoch(painter: &Painter) -> TextEpoch {
    const PROBE: &str = "Ag\u{2026}0";
    let probe = |font_id: FontId| {
        painter
            .layout_no_wrap(PROBE.to_owned(), font_id, Color32::WHITE)
            .size()
            .x
            .to_bits()
    };
    TextEpoch {
        font_probes: [
            probe(font_id_from_style(st::semibold_font)),
            probe(font_id_from_style(st::dialogs_text_font)),
        ],
        pixels_per_point: painter.pixels_per_point().to_bits(),
        language: tg_lang::global_generation(),
    }
}

/// Elided strings of the last two frames, kept in the egui context.
#[derive(Clone, Default)]
struct ElisionMemo(Arc<Mutex<ElisionMemoState>>);

#[derive(Default)]
struct ElisionMemoState {
    frame: u64,
    epoch: Option<TextEpoch>,
    /// By hash of `(text, max_width, font)`.
    entries: HashMap<u64, ElisionEntry>,
}

struct ElisionEntry {
    used_frame: u64,
    /// `None` when the text fits as it is.
    elided: Option<Box<str>>,
}

/// Elides `text` on the right with `'…'` to `max_width` logical pixels in `font_id`
/// ([`elide_text_right`] measured with [`measure_text_i32`]).
///
/// The string is laid out once: its glyph positions give the cut, and
/// [`elide_text_right_hinted`] confirms it with the prefix measurements the rows always used,
/// so the result is the one that measuring every prefix gives. The result is remembered while
/// the same string is elided on consecutive frames, which makes a repeated row free.
#[must_use]
pub fn elide_text(painter: &Painter, text: &str, max_width: i32, font_id: &FontId) -> String {
    if max_width <= 0 || text.is_empty() {
        return String::new();
    }
    let ctx = painter.ctx();
    let memo = ctx.data_mut(|d| d.get_temp_mut_or_default::<ElisionMemo>(Id::NULL).clone());
    let mut memo = memo.0.lock().unwrap_or_else(PoisonError::into_inner);
    // The root viewport's frame counter: a separate chat window is a viewport with its own.
    let frame = ctx.cumulative_frame_nr_for(egui::ViewportId::ROOT);
    if memo.epoch.is_none() || memo.frame != frame {
        memo.frame = frame;
        let epoch = text_epoch(painter);
        if memo.epoch == Some(epoch) {
            memo.entries
                .retain(|_, entry| frame.wrapping_sub(entry.used_frame) <= 1);
        } else {
            memo.entries.clear();
            memo.epoch = Some(epoch);
        }
    }
    let key = egui::util::hash((text, max_width, font_id));
    if let Some(entry) = memo.entries.get_mut(&key) {
        entry.used_frame = frame;
        return entry.elided.as_deref().unwrap_or(text).to_owned();
    }
    let elided = elide_text_uncached(painter, text, max_width, font_id);
    memo.entries.insert(
        key,
        ElisionEntry {
            used_frame: frame,
            elided: (elided != text).then(|| elided.as_str().into()),
        },
    );
    elided
}

/// Forgets the elided strings kept in `ctx`, so that a test can compare a frame that elides
/// everything anew with one that reuses them.
#[cfg(test)]
pub(crate) fn forget_elided_text(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<ElisionMemo>(Id::NULL));
}

fn elide_text_uncached(painter: &Painter, text: &str, max_width: i32, font_id: &FontId) -> String {
    let galley = painter.layout_no_wrap(text.to_owned(), font_id.clone(), Color32::WHITE);
    let full_width = galley.size().x.ceil() as i32;
    if full_width <= max_width {
        return text.to_owned();
    }
    let ellipsis_width = measure_text_i32(painter, "\u{2026}", font_id.clone());
    let measure = |prefix: &str| {
        if prefix.len() == text.len() {
            full_width
        } else {
            measure_text_i32(painter, prefix, font_id.clone())
        }
    };
    match galley_cut_hint(&galley, text, max_width - ellipsis_width) {
        Some(hint_end) => elide_text_right_hinted(text, max_width, hint_end, measure),
        None => elide_text_right(text, max_width, measure),
    }
}

/// Byte length of the longest prefix of `text` whose glyphs end within `budget` pixels in
/// `galley` (the whole `text` laid out on one row), or `None` if the galley does not hold one
/// glyph per character on a single row.
fn galley_cut_hint(galley: &Galley, text: &str, budget: i32) -> Option<usize> {
    let [placed_row] = galley.rows.as_slice() else {
        return None;
    };
    let glyphs = &placed_row.row.glyphs;
    let mut chars = text.chars();
    let mut end = 0usize;
    let mut hint = 0usize;
    for glyph in glyphs {
        let ch = chars.next()?;
        if ch != glyph.chr {
            return None;
        }
        end += ch.len_utf8();
        if glyph.max_x().ceil() as i32 > budget {
            break;
        }
        hint = end;
    }
    Some(hint)
}
