//! Dialog row painting, badges, special avatars, typing animation, filter tags, and search highlighting.
//!
//! Port origin: `Telegram/SourceFiles/dialogs/ui/dialogs_layout.h`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_layout.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_message_view.cpp`,
//! `Telegram/SourceFiles/ui/chat/chats_filter_tag.cpp`,
//! and `Telegram/SourceFiles/dialogs/dialogs.style`.

use std::ops::Range;
use std::sync::OnceLock;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};
use tg_history::{
    AvatarCornerBadge, DialogChatTypeIcon, DialogRowInput, DialogRowLayout, DialogRowMode,
    DialogRowStyle, DialogSendState, FontMetrics, LocalDateTime, elide_text_right,
    format_unread_counter, layout_dialog_row,
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
    ArchiveSummary, DialogItem, FolderTagBadge, GlobalPeerResult, MessageSearchResult,
    SpecialRowKind,
};
use crate::dialogs::strings;
use tg_app_core::stand_in::theme::Palette;

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

/// Formats a dialog timestamp using `tg-lang`'s localized weekday/month helpers (`langDayOfWeek`,
/// `langDayOfMonth`) matching `Dialogs::Ui::RowPainter::PaintRowDate` in `dialogs_layout.cpp`.
#[must_use]
pub fn format_localized_dialogs_date(
    dt: LocalDateTime,
    now: LocalDateTime,
    use_24h: bool,
) -> String {
    let dt_days = dt.to_local_epoch_secs().div_euclid(86_400);
    let now_days = now.to_local_epoch_secs().div_euclid(86_400);
    let diff = now_days - dt_days;

    if diff == 0 {
        if use_24h {
            format!("{:02}:{:02}", dt.hour, dt.minute)
        } else {
            let suffix = if dt.hour < 12 { "AM" } else { "PM" };
            let h12 = match dt.hour % 12 {
                0 => 12,
                h => h,
            };
            format!("{h12}:{:02} {suffix}", dt.minute)
        }
    } else if (1..=6).contains(&diff) {
        // 1970-01-01 (day 0) was Thursday (ISO weekday 4); map to 1 = Mon .. 7 = Sun.
        let wd = (dt_days + 3).rem_euclid(7) as u8 + 1;
        tg_lang::with_current(|inst| day_of_week(inst, wd))
    } else {
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

    let peer_badge_width = if item.verified || item.premium { 18 } else { 0 };
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
    let (layout, date_text, counter_text) =
        compute_item_layout(painter, item, now, rect.width(), state);
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
    paint_peer_avatar(
        painter,
        photo_rect,
        item.special,
        item.color_index,
        &item.title,
        colors,
    );

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
        paint_row_badges(painter, origin, &layout, item, &counter_text, state, colors);
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
            &date_text,
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
    let title_str = if layout.name_elided {
        elide_text_right(&item.title, layout.name_rect.width, |s| {
            measure_text_i32(painter, s, name_font.clone())
        })
    } else {
        item.title.clone()
    };
    paint_semibold_text(
        painter,
        Pos2::new(name_r.left(), name_r.center().y),
        Align2::LEFT_CENTER,
        &title_str,
        name_font,
        name_fg,
    );

    // 9. Verified or Premium peer badge immediately after title.
    if let Some(pb_r) = layout.peer_badge_rect {
        let r = to_egui_rect(origin, pb_r);
        if item.verified {
            paint_verified_badge(painter, r, state.active, state.selected, colors);
        } else if item.premium {
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
        paint_row_badges(painter, origin, &layout, item, &counter_text, state, colors);

    // 12. Preview line: typing animation OR red "Draft: " prefix OR sender + last message.
    if let Some(typing_r) = layout.typing_rect
        && let Some(typing) = &item.typing
    {
        paint_typing_indicator(
            painter,
            to_egui_rect(origin, typing_r),
            &typing.label(),
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
            let prefix = format!(
                "{} ",
                tg_lang::tr::lng_dialogs_text_from_wrapped(&tg_lang::tr::lng_from_draft())
            );
            let draft_fg = colors.draft_fg(state.active);
            let prefix_g = clipped.layout_no_wrap(prefix, preview_font.clone(), draft_fg);
            let prefix_w = prefix_g.size().x;
            clipped.galley(
                Pos2::new(r.left(), r.center().y - prefix_g.size().y * 0.5),
                prefix_g,
                draft_fg,
            );
            let avail = (r.width() - prefix_w).max(0.0) as i32;
            let body = elide_text_right(&draft.text, avail, |s| {
                measure_text_i32(painter, s, preview_font.clone())
            });
            clipped.text(
                Pos2::new(r.left() + prefix_w, r.center().y),
                Align2::LEFT_CENTER,
                body,
                preview_font,
                sub_fg,
            );
        } else if let Some(sender) = &item.last_sender
            && !item.last_message.is_empty()
        {
            let prefix = format!("{} ", tg_lang::tr::lng_dialogs_text_from_wrapped(sender));
            let prefix_g = clipped.layout_no_wrap(prefix, preview_font.clone(), service_fg);
            let prefix_w = prefix_g.size().x;
            clipped.galley(
                Pos2::new(r.left(), r.center().y - prefix_g.size().y * 0.5),
                prefix_g,
                service_fg,
            );
            let avail = (r.width() - prefix_w).max(0.0) as i32;
            let body = elide_text_right(&item.last_message, avail, |s| {
                measure_text_i32(painter, s, preview_font.clone())
            });
            clipped.text(
                Pos2::new(r.left() + prefix_w, r.center().y),
                Align2::LEFT_CENTER,
                body,
                preview_font,
                sub_fg,
            );
        } else if !item.last_message.is_empty() {
            let body = elide_text_right(&item.last_message, r.width() as i32, |s| {
                measure_text_i32(painter, s, preview_font.clone())
            });
            clipped.text(
                Pos2::new(r.left(), r.center().y),
                Align2::LEFT_CENTER,
                body,
                preview_font,
                sub_fg,
            );
        } else {
            clipped.text(
                Pos2::new(r.left(), r.center().y),
                Align2::LEFT_CENTER,
                strings::PREVIEW_HISTORY_CLEARED,
                preview_font,
                sub_fg,
            );
        }
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
            let elided = elide_text_right(&preview_joined, preview_w, |s| {
                measure_text_i32(painter, s, preview_font.clone())
            });
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
    paint_peer_avatar(
        painter,
        photo_rect,
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
    let elided_title = elide_text_right(&peer.title, name_max_w, |s| {
        measure_text_i32(painter, s, name_font.clone())
    });
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
    let elided_sub = elide_text_right(&peer.subtitle, sub_max_w, |s| {
        measure_text_i32(painter, s, sub_font.clone())
    });
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
    paint_peer_avatar(
        painter,
        photo_rect,
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
    let name_avail = (right_edge - date_w - 8.0 - name_x).max(16.0) as i32;
    let elided_name = elide_text_right(&msg.peer_title, name_avail, |s| {
        measure_text_i32(painter, s, name_font.clone())
    });
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
    if let Some(sender) = &msg.sender_name {
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

    let highlight_fg = if active {
        colors.color("dialogsNameFgActive")
    } else {
        colors.active_text_fg()
    };
    let highlight_bg = colors.search_highlight_bg(active);
    let job = build_highlighted_snippet_job(
        &msg.text,
        query,
        preview_font,
        sub_fg,
        highlight_fg,
        highlight_bg,
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
    let elided = elide_text_right(label, avail, |s| measure_text_i32(painter, s, font.clone()));
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
