//! Telegram Premium settings section (`settings/sections/settings_premium.cpp`) shown as a layer:
//! the gradient cover with the Premium star, title and about text (`Ui::Premium::TopBar`), the
//! subscription options (`Ui::Premium::AddGiftOptions` with `premiumSubscriptionOption`), the
//! feature list (`AddSummaryPremium`: icons on a full-height gradient, bold title, gray about)
//! and the pinned gradient Subscribe button (`CreateSubscribeButton`).

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2,
    WidgetInfo,
};
use tg_app_core::stand_in::theme::Palette;
use tg_lang::tr;
use tg_payments::premium::PremiumSubscriptionOption;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::{DefaultPaletteResolver, Icon, st};

use crate::payments::backend::PaymentsBackend;
use crate::payments::gift_card::{colors, paint_gradient_star};
use crate::payments::premium::PremiumView;

/// `infoDesiredWidth`: width of the settings layer the section is shown in.
pub const PREMIUM_LAYER_WIDTH: f32 = 392.0;
/// `settingsPremiumTopHeight`.
const TOP_HEIGHT: f32 = 202.0;
/// `defaultPremiumCover.starSize`.
const STAR_SIZE: Vec2 = Vec2::new(84.0, 81.0);
/// `defaultPremiumCover.starTopSkip`.
const STAR_TOP_SKIP: f32 = 37.0;
/// `defaultPremiumCover.titlePadding` (top, bottom).
const TITLE_PADDING: (f32, f32) = (18.0, 11.0);
/// `boxRowPadding` horizontal.
const ROW_PADDING_H: f32 = 22.0;
/// `premiumAboutTextStyle.lineHeight`.
const ABOUT_LINE_HEIGHT: f32 = 18.0;
/// `settingsPremiumOptionsPadding`.
const OPTIONS_PADDING: (f32, f32) = (9.0, 2.0);
/// `premiumSubscriptionOption.rowPadding` (left, top, right, bottom).
const OPTION_ROW_PADDING: [f32; 4] = [9.0, 2.0, 17.0, 3.0];
/// `premiumSubscriptionOption.rowMargins` (left, right).
const OPTION_ROW_MARGINS: (f32, f32) = (14.0, 5.0);
/// `premiumSubscriptionOption.rowHeight`.
const OPTION_ROW_HEIGHT: f32 = 39.0;
/// `premiumSubscriptionOption.textLeft`.
const OPTION_TEXT_LEFT: f32 = 51.0;
/// `premiumSubscriptionOption.subtitleTop`.
const OPTION_SUBTITLE_TOP: f32 = 1.0;
/// `premiumSubscriptionOption.badgeHeight` / `badgeRadius` / `badgeShift.x`.
const OPTION_BADGE: (f32, f32, f32) = (15.0, 4.0, 9.0);
/// `settingsPremiumRowTitlePadding` (left, top, right, bottom).
const FEATURE_TITLE_PADDING: [f32; 4] = [59.0, 5.0, 46.0, 1.0];
/// `settingsPremiumRowAboutPadding` (left, top, right, bottom).
const FEATURE_ABOUT_PADDING: [f32; 4] = [59.0, 0.0, 46.0, 6.0];
/// `settingsButton.iconLeft`.
const ICON_LEFT: f32 = 20.0;
/// `settingsIconRadius`.
const ICON_RADIUS: f32 = 6.0;
/// `settingsPremiumButtonPadding` (left, top, right, bottom).
const BUTTON_PADDING: [f32; 4] = [11.0, 11.0, 11.0, 3.0];
/// `premiumPreviewBox.button.height`.
const BUTTON_HEIGHT: f32 = 44.0;
/// `roundRadiusLarge`.
const BUTTON_RADIUS: f32 = 6.0;
/// `TopTransitionSkip()`: `(settingsButton.padding.top + settingsPremiumRowTitlePadding.top) / 2`.
const TOP_TRANSITION_SKIP: f32 = 7.0;
/// `defaultVerticalListSkip`.
const LIST_SKIP: f32 = 6.0;

/// One feature row of the Premium section (`EntryMap` in `settings_premium.cpp`).
#[derive(Debug, Clone, Copy)]
pub struct PremiumFeatureEntry {
    /// Feature key from `premium_promo_order`.
    pub key: &'static str,
    /// Row icon.
    pub icon: &'static Icon,
    /// Title (`lng_premium_summary_subtitle_*`).
    pub title: fn() -> String,
    /// About text (`lng_premium_summary_about_*`).
    pub about: fn() -> String,
    /// Shows the "NEW" badge after the title.
    pub new_badge: bool,
}

/// `FallbackOrder()` in `settings_premium.cpp`.
pub const FALLBACK_ORDER: &[&str] = &[
    "stories",
    "more_upload",
    "double_limits",
    "last_seen",
    "voice_to_text",
    "faster_download",
    "translations",
    "animated_emoji",
    "emoji_status",
    "saved_tags",
    "wallpapers",
    "profile_badge",
    "message_privacy",
    "advanced_chat_management",
    "no_ads",
    "infinite_reactions",
    "animated_userpics",
    "premium_stickers",
    "business",
    "effects",
    "ai_compose",
    "rich_formatting",
];

macro_rules! entry {
    ($key:literal, $icon:ident, $title:ident, $about:ident) => {
        entry!($key, $icon, $title, $about, false)
    };
    ($key:literal, $icon:ident, $title:ident, $about:ident, $new:literal) => {
        PremiumFeatureEntry {
            key: $key,
            icon: &st::$icon,
            title: tr::$title,
            about: tr::$about,
            new_badge: $new,
        }
    };
}

/// `EntryMap()` in `settings_premium.cpp`.
#[must_use]
pub fn premium_feature_entry(key: &str) -> Option<PremiumFeatureEntry> {
    Some(match key {
        "saved_tags" => entry!(
            "saved_tags",
            settings_premium_icon_tags,
            lng_premium_summary_subtitle_tags_for_messages,
            lng_premium_summary_about_tags_for_messages
        ),
        "last_seen" => entry!(
            "last_seen",
            settings_premium_icon_last_seen,
            lng_premium_summary_subtitle_last_seen,
            lng_premium_summary_about_last_seen
        ),
        "message_privacy" => entry!(
            "message_privacy",
            settings_premium_icon_privacy,
            lng_premium_summary_subtitle_message_privacy,
            lng_premium_summary_about_message_privacy
        ),
        "wallpapers" => entry!(
            "wallpapers",
            settings_premium_icon_wallpapers,
            lng_premium_summary_subtitle_wallpapers,
            lng_premium_summary_about_wallpapers
        ),
        "peer_colors" => entry!(
            "peer_colors",
            settings_premium_icon_peer_colors,
            lng_premium_summary_subtitle_peer_colors,
            lng_premium_summary_about_peer_colors
        ),
        "stories" => entry!(
            "stories",
            settings_premium_icon_stories,
            lng_premium_summary_subtitle_stories,
            lng_premium_summary_about_stories
        ),
        "double_limits" => entry!(
            "double_limits",
            settings_premium_icon_double,
            lng_premium_summary_subtitle_double_limits,
            lng_premium_summary_about_double_limits
        ),
        "more_upload" => entry!(
            "more_upload",
            settings_premium_icon_files,
            lng_premium_summary_subtitle_more_upload,
            lng_premium_summary_about_more_upload
        ),
        "faster_download" => entry!(
            "faster_download",
            settings_premium_icon_speed,
            lng_premium_summary_subtitle_faster_download,
            lng_premium_summary_about_faster_download
        ),
        "voice_to_text" => entry!(
            "voice_to_text",
            settings_premium_icon_voice,
            lng_premium_summary_subtitle_voice_to_text,
            lng_premium_summary_about_voice_to_text
        ),
        "no_ads" => entry!(
            "no_ads",
            settings_premium_icon_channels_off,
            lng_premium_summary_subtitle_no_ads,
            lng_premium_summary_about_no_ads
        ),
        "emoji_status" => entry!(
            "emoji_status",
            settings_premium_icon_status,
            lng_premium_summary_subtitle_emoji_status,
            lng_premium_summary_about_emoji_status
        ),
        "infinite_reactions" => entry!(
            "infinite_reactions",
            settings_premium_icon_like,
            lng_premium_summary_subtitle_infinite_reactions,
            lng_premium_summary_about_infinite_reactions
        ),
        "premium_stickers" => entry!(
            "premium_stickers",
            settings_icon_stickers,
            lng_premium_summary_subtitle_premium_stickers,
            lng_premium_summary_about_premium_stickers
        ),
        "animated_emoji" => entry!(
            "animated_emoji",
            settings_icon_emoji,
            lng_premium_summary_subtitle_animated_emoji,
            lng_premium_summary_about_animated_emoji
        ),
        "advanced_chat_management" => entry!(
            "advanced_chat_management",
            settings_icon_chat,
            lng_premium_summary_subtitle_advanced_chat_management,
            lng_premium_summary_about_advanced_chat_management
        ),
        "profile_badge" => entry!(
            "profile_badge",
            settings_premium_icon_star,
            lng_premium_summary_subtitle_profile_badge,
            lng_premium_summary_about_profile_badge
        ),
        "animated_userpics" => entry!(
            "animated_userpics",
            settings_premium_icon_play,
            lng_premium_summary_subtitle_animated_userpics,
            lng_premium_summary_about_animated_userpics
        ),
        "translations" => entry!(
            "translations",
            settings_premium_icon_translations,
            lng_premium_summary_subtitle_translation,
            lng_premium_summary_about_translation
        ),
        "business" => entry!(
            "business",
            settings_premium_icon_business,
            lng_premium_summary_subtitle_business,
            lng_premium_summary_about_business
        ),
        "effects" => entry!(
            "effects",
            settings_premium_icon_effects,
            lng_premium_summary_subtitle_effects,
            lng_premium_summary_about_effects
        ),
        "todo" => entry!(
            "todo",
            settings_premium_icon_checklist,
            lng_premium_summary_subtitle_todo_lists,
            lng_premium_summary_about_todo_lists
        ),
        "no_forwards" => entry!(
            "no_forwards",
            settings_premium_icon_no_forwards,
            lng_premium_summary_subtitle_no_forwards,
            lng_premium_summary_about_no_forwards
        ),
        "ai_compose" => entry!(
            "ai_compose",
            settings_premium_icon_ai_compose,
            lng_premium_summary_subtitle_ai_compose,
            lng_premium_summary_about_ai_compose,
            true
        ),
        "rich_formatting" => entry!(
            "rich_formatting",
            settings_premium_icon_rich,
            lng_premium_summary_subtitle_rich_formatting,
            lng_premium_summary_about_rich_formatting,
            true
        ),
        _ => return None,
    })
}

/// Feature rows in display order (`premium_promo_order` falls back to [`FALLBACK_ORDER`]).
#[must_use]
pub fn premium_feature_rows(order: &[String]) -> Vec<PremiumFeatureEntry> {
    let keys: Vec<&str> = if order.is_empty() {
        FALLBACK_ORDER.to_vec()
    } else {
        order.iter().map(String::as_str).collect()
    };
    keys.into_iter().filter_map(premium_feature_entry).collect()
}

/// Option row texts after `SubscriptionOptionsForRows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PremiumOptionRow {
    /// "Annual", "Semiannual", "Monthly", ...
    pub duration: String,
    /// Discount badge ("−40%"), empty when none.
    pub discount: String,
    /// Struck-out undiscounted total, empty when none.
    pub cost_no_discount: String,
    /// "$X per year" (empty for the monthly option).
    pub per_year: String,
    /// "$Y / month", right-aligned.
    pub per_month: String,
}

/// `SubscriptionOptionsForRows` for one option.
#[must_use]
pub fn premium_option_row(option: &PremiumSubscriptionOption) -> PremiumOptionRow {
    let monthly = option.months == 1;
    let duration = match option.months {
        1 => tr::lng_premium_subscribe_months_1(),
        6 => tr::lng_premium_subscribe_months_6(),
        12 => tr::lng_premium_subscribe_months_12(),
        24 => tr::lng_premium_subscribe_months_24(),
        m if m < 12 => tr::lng_months(i64::from(m)),
        m => tr::lng_years(i64::from(m / 12)),
    };
    PremiumOptionRow {
        duration,
        discount: option.discount.clone(),
        cost_no_discount: if monthly {
            String::new()
        } else {
            option.cost_no_discount.clone()
        },
        per_year: if monthly {
            String::new()
        } else {
            tr::lng_premium_subscribe_total(&option.cost_per_year)
        },
        per_month: tr::lng_premium_gift_per(&option.cost_per_month),
    }
}

/// What the user did in the section this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PremiumSectionOutcome {
    /// Close button pressed.
    pub close: bool,
    /// Toast to show.
    pub toast: Option<String>,
    /// Subscribe pressed: the selected option's invoice link (`botUrl`).
    pub open_invoice: Option<String>,
}

fn gradient_at(stops: &[(f32, Color32)], t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mut prev = stops[0];
    for &(pos, color) in stops {
        if t <= pos {
            let span = (pos - prev.0).max(f32::EPSILON);
            let k = ((t - prev.0) / span).clamp(0.0, 1.0);
            let mix =
                |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * k).round() as u8;
            return Color32::from_rgb(
                mix(prev.1.r(), color.r()),
                mix(prev.1.g(), color.g()),
                mix(prev.1.b(), color.b()),
            );
        }
        prev = (pos, color);
    }
    prev.1
}

/// `ButtonGradientStops()`.
const BUTTON_STOPS: [(f32, Color32); 3] = [
    (0.0, colors::PREMIUM_BUTTON_BG1),
    (0.6, colors::PREMIUM_BUTTON_BG2),
    (1.0, colors::PREMIUM_BUTTON_BG3),
];
/// `FullHeightGradientStops()`.
const FULL_HEIGHT_STOPS: [(f32, Color32); 5] = [
    (0.0, colors::PREMIUM_ICON_BG1),
    (0.28, colors::PREMIUM_ICON_BG2),
    (0.55, colors::PREMIUM_BUTTON_BG2),
    (0.75, colors::PREMIUM_BUTTON_BG1),
    (1.0, colors::PREMIUM_ICON_BG3),
];
/// `GiftGradientStops()`.
const GIFT_STOPS: [(f32, Color32); 2] = [
    (0.0, colors::PREMIUM_BUTTON_BG1),
    (1.0, colors::PREMIUM_BUTTON_BG2),
];

/// Fills `rect` with a linear gradient from `from` to `to` (points) using a vertex-colored grid.
fn fill_linear_gradient(
    painter: &egui::Painter,
    rect: Rect,
    from: Pos2,
    to: Pos2,
    stops: &[(f32, Color32)],
) {
    let cols = 32;
    let rows = 8;
    let dir = to - from;
    let len2 = dir.length_sq().max(f32::EPSILON);
    let mut mesh = Mesh::default();
    for r in 0..=rows {
        for c in 0..=cols {
            let p = Pos2::new(
                rect.left() + rect.width() * c as f32 / cols as f32,
                rect.top() + rect.height() * r as f32 / rows as f32,
            );
            let t = (p - from).dot(dir) / len2;
            mesh.vertices.push(Vertex {
                pos: p,
                uv: WHITE_UV,
                color: gradient_at(stops, t),
            });
        }
    }
    let w = cols + 1;
    for r in 0..rows {
        for c in 0..cols {
            let i = (r * w + c) as u32;
            let w = w as u32;
            mesh.add_triangle(i, i + 1, i + w);
            mesh.add_triangle(i + 1, i + w + 1, i + w);
        }
    }
    painter.add(Shape::mesh(mesh));
}

fn marked_job(
    text: &str,
    size: f32,
    color: Color32,
    width: f32,
    center: bool,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for (i, part) in text.split("**").enumerate() {
        if part.is_empty() {
            continue;
        }
        job.append(
            part,
            0.0,
            egui::TextFormat {
                font_id: FontId::new(
                    size,
                    if i % 2 == 1 {
                        family_semibold()
                    } else {
                        family_regular()
                    },
                ),
                color,
                line_height: Some(ABOUT_LINE_HEIGHT),
                ..Default::default()
            },
        );
    }
    job.wrap.max_width = width;
    if center {
        job.halign = egui::Align::Center;
    }
    job
}

impl PremiumView {
    /// Whether the account already has Premium (an option is marked `current`).
    #[must_use]
    pub fn am_premium(&self) -> bool {
        self.bundle.promo.period_options.iter().any(|o| o.current)
    }

    /// Renders the Premium section filling `ui` (`Settings::Premium` in a layer).
    pub fn show_section(
        &mut self,
        ui: &mut Ui,
        _backend: &dyn PaymentsBackend,
        palette: &Palette,
    ) -> PremiumSectionOutcome {
        let mut outcome = PremiumSectionOutcome::default();
        let full = ui.max_rect();
        ui.painter()
            .rect_filled(full, CornerRadius::ZERO, palette.window_bg);
        let premium = self.am_premium();
        let options = self.bundle.promo.period_options.clone();
        let show_options = !premium && options.len() >= 2;
        let bottom_h = if premium {
            0.0
        } else {
            BUTTON_PADDING[1] + BUTTON_HEIGHT + BUTTON_PADDING[3]
        };
        let content =
            Rect::from_min_max(full.min, Pos2::new(full.right(), full.bottom() - bottom_h));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("tg_premium_section_scroll")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                let left = ui.max_rect().left();
                let width = ui.available_width();
                let top = ui.max_rect().top();
                let mut y = self.paint_cover(ui, left, top, width, premium, &mut outcome);
                if show_options {
                    y = self.paint_options(ui, left, y, width, &options);
                } else {
                    y += TOP_TRANSITION_SKIP * 2.0;
                }
                y = paint_features(ui, left, y, width, palette);
                ui.allocate_space(Vec2::new(width, (y - top).max(0.0)));
            });

        if !premium {
            let button = Rect::from_min_size(
                Pos2::new(
                    full.left() + BUTTON_PADDING[0],
                    content.bottom() + BUTTON_PADDING[1],
                ),
                Vec2::new(
                    full.width() - BUTTON_PADDING[0] - BUTTON_PADDING[2],
                    BUTTON_HEIGHT,
                ),
            );
            let label = self.subscribe_button_label();
            let resp = ui.interact(button, Id::unique("tg_premium_subscribe"), Sense::click());
            resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, &label));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let painter = ui.painter_at(button);
            fill_linear_gradient(
                &painter,
                button,
                button.left_center(),
                button.right_center(),
                &BUTTON_STOPS,
            );
            paint_round_corners(&painter, button, BUTTON_RADIUS, palette.window_bg);
            let galley = ui.ctx().fonts_mut(|f| {
                f.layout_no_wrap(label, FontId::new(13.0, family_semibold()), Color32::WHITE)
            });
            ui.painter().galley(
                Pos2::new(
                    button.center().x - galley.size().x / 2.0,
                    button.center().y - galley.size().y / 2.0,
                ),
                galley,
                Color32::WHITE,
            );
            if resp.clicked()
                && let Some(option) = self.selected_option()
            {
                outcome.open_invoice = Some(option.bot_url.clone());
            }
        }
        outcome
    }

    /// Paints the cover (`Ui::Premium::TopBar` with `defaultPremiumCover`); returns its bottom.
    fn paint_cover(
        &self,
        ui: &mut Ui,
        left: f32,
        top: f32,
        width: f32,
        premium: bool,
        outcome: &mut PremiumSectionOutcome,
    ) -> f32 {
        let about = if premium && !self.bundle.promo.status_text.trim().is_empty() {
            self.bundle.promo.status_text.clone()
        } else {
            tr::lng_premium_summary_top_about()
        };
        let about_job = marked_job(
            &about,
            12.0,
            Color32::WHITE,
            width - 2.0 * ROW_PADDING_H,
            true,
        );
        let about_galley = ui.ctx().fonts_mut(|f| f.layout_job(about_job));
        let extra = (about_galley.size().y - ABOUT_LINE_HEIGHT).max(0.0);
        let rect = Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, TOP_HEIGHT + extra));
        let painter = ui.painter_at(rect);
        // `TopBar::paintEvent`: gradient from (0, h*2/3) to (w, h - h*2/3).
        let p0 = Pos2::new(rect.left(), rect.top() + rect.height() * 2.0 / 3.0);
        let p1 = Pos2::new(rect.right(), rect.top() + rect.height() / 3.0);
        fill_linear_gradient(&painter, rect, p0, p1, &BUTTON_STOPS);

        let star = Rect::from_min_size(
            Pos2::new(
                rect.center().x - STAR_SIZE.x / 2.0,
                rect.top() + STAR_TOP_SKIP,
            ),
            STAR_SIZE,
        );
        paint_gradient_star(
            ui.ctx(),
            &painter,
            star,
            &[(0.0, Color32::WHITE), (1.0, Color32::WHITE)],
        );

        let title = tr::lng_premium_summary_title();
        let title_galley = ui.ctx().fonts_mut(|f| {
            f.layout_no_wrap(title, FontId::new(16.0, family_semibold()), Color32::WHITE)
        });
        let title_top = star.bottom() + TITLE_PADDING.0;
        painter.galley(
            Pos2::new(rect.center().x - title_galley.size().x / 2.0, title_top),
            title_galley.clone(),
            Color32::WHITE,
        );
        let about_top = title_top + title_galley.size().y + TITLE_PADDING.1;
        painter.galley(
            Pos2::new(rect.center().x, about_top),
            about_galley,
            Color32::WHITE,
        );

        // Close button (`settingsPremiumTopBarClose`: 48x54, `info/info_close`).
        let close = Rect::from_min_size(
            Pos2::new(rect.right() - 48.0, rect.top()),
            Vec2::new(48.0, 54.0),
        );
        let resp = ui.interact(close, Id::unique("tg_premium_close"), Sense::click());
        let label = tr::lng_close();
        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, &label));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            painter.circle_filled(close.center(), 18.0, Color32::from_white_alpha(30));
        }
        paint_style_icon(
            &painter,
            close,
            st::settings_premium_top_bar_close.icon,
            &DefaultPaletteResolver,
            Some(Color32::WHITE),
        );
        if resp.clicked() {
            outcome.close = true;
        }
        rect.bottom()
    }

    /// Paints the subscription option radio rows; returns the bottom after the divider.
    fn paint_options(
        &mut self,
        ui: &mut Ui,
        left: f32,
        top: f32,
        width: f32,
        options: &[PremiumSubscriptionOption],
    ) -> f32 {
        let mut y = top + OPTIONS_PADDING.0;
        let row_step = OPTION_ROW_PADDING[1] + OPTION_ROW_HEIGHT + OPTION_ROW_PADDING[3];
        let rows_from = y + OPTION_ROW_PADDING[1];
        let rows_to = y + row_step * options.len() as f32 - OPTION_ROW_PADDING[3];
        let sub_fg = Color32::from_gray(0x99);
        let title_fg = Color32::from_gray(0x22);
        for (index, option) in options.iter().enumerate() {
            let row = Rect::from_min_size(
                Pos2::new(left + OPTION_ROW_PADDING[0], y + OPTION_ROW_PADDING[1]),
                Vec2::new(
                    width - OPTION_ROW_PADDING[0] - OPTION_ROW_PADDING[2],
                    OPTION_ROW_HEIGHT,
                ),
            );
            let texts = premium_option_row(option);
            let selected = index == self.selected_tier;
            let resp = ui.interact(
                row,
                Id::unique(("tg_premium_option", index)),
                Sense::click(),
            );
            resp.widget_info(|| {
                WidgetInfo::selected(egui::Role::RadioButton, true, selected, &texts.duration)
            });
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.selected_tier = index;
            }
            let painter = ui.painter();
            let t_top = (row.top() - rows_from) / (rows_to - rows_from).max(1.0);
            let t_mid = (row.center().y - rows_from) / (rows_to - rows_from).max(1.0);
            let brush = gradient_at(&GIFT_STOPS, t_mid);

            // Radio (`GradientRadioView`, `defaultRadio`: 20px, 2px).
            let center = Pos2::new(row.left() + OPTION_ROW_MARGINS.0 + 10.0, row.center().y);
            if index == self.selected_tier {
                painter.circle_stroke(center, 9.0, Stroke::new(2.0, brush));
                painter.circle_filled(center, 5.0, brush);
            } else {
                painter.circle_stroke(center, 9.0, Stroke::new(2.0, Color32::from_gray(0xc8)));
            }
            let _ = t_top;

            let title_font = FontId::new(13.0, family_semibold());
            let title_galley = ui.ctx().fonts_mut(|f| {
                f.layout_no_wrap(texts.duration.clone(), title_font.clone(), title_fg)
            });
            let x = row.left() + OPTION_TEXT_LEFT;
            let centered = texts.per_year.is_empty() && texts.discount.is_empty();
            let title_y = if centered {
                row.center().y - title_galley.size().y / 2.0
            } else {
                row.top() + OPTION_SUBTITLE_TOP
            };
            painter.galley(Pos2::new(x, title_y), title_galley.clone(), title_fg);
            if !texts.discount.is_empty() {
                let badge_font = FontId::new(12.0, family_semibold());
                let badge_galley = ui.ctx().fonts_mut(|f| {
                    f.layout_no_wrap(texts.discount.clone(), badge_font, Color32::WHITE)
                });
                let badge = Rect::from_min_size(
                    Pos2::new(
                        x + title_galley.size().x + OPTION_BADGE.2,
                        title_y + (title_galley.size().y - OPTION_BADGE.0) / 2.0,
                    ),
                    Vec2::new(badge_galley.size().x + 6.0, OPTION_BADGE.0),
                );
                painter.rect_filled(badge, CornerRadius::same(OPTION_BADGE.1 as u8), brush);
                painter.galley(
                    Pos2::new(
                        badge.center().x - badge_galley.size().x / 2.0,
                        badge.center().y - badge_galley.size().y / 2.0,
                    ),
                    badge_galley,
                    Color32::WHITE,
                );
            }
            if !texts.per_year.is_empty() {
                let mut job = egui::text::LayoutJob::default();
                let small = FontId::new(11.0, family_regular());
                if !texts.cost_no_discount.is_empty() {
                    job.append(
                        &texts.cost_no_discount,
                        0.0,
                        egui::TextFormat {
                            font_id: small.clone(),
                            color: sub_fg,
                            strikethrough: Stroke::new(1.0, sub_fg),
                            ..Default::default()
                        },
                    );
                    job.append(" ", 0.0, egui::TextFormat::simple(small.clone(), sub_fg));
                }
                job.append(
                    &texts.per_year,
                    0.0,
                    egui::TextFormat::simple(small, sub_fg),
                );
                let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                painter.galley(
                    Pos2::new(
                        x,
                        row.center().y + if texts.discount.is_empty() { 0.0 } else { 1.0 },
                    ),
                    galley,
                    sub_fg,
                );
            }
            let right = ui.ctx().fonts_mut(|f| {
                f.layout_no_wrap(texts.per_month, FontId::new(13.0, family_regular()), sub_fg)
            });
            painter.galley(
                Pos2::new(
                    row.right() - OPTION_ROW_MARGINS.1 - right.size().x,
                    row.center().y - right.size().y / 2.0,
                ),
                right,
                sub_fg,
            );
            y += row_step;
        }
        y += OPTIONS_PADDING.1;
        // `Ui::AddDivider`: a `boxDividerBg` strip with shadows.
        let divider = Rect::from_min_size(Pos2::new(left, y), Vec2::new(width, 10.0));
        ui.painter()
            .rect_filled(divider, CornerRadius::ZERO, Color32::from_gray(0xf1));
        y += divider.height();
        y + TOP_TRANSITION_SKIP * 2.0 - LIST_SKIP
    }
}

/// Masks the square corners of a gradient fill in `rect` with `bg` (the painter must be clipped
/// to `rect`): an outside stroke of the rounded rect covers exactly the corner leftovers.
fn paint_round_corners(painter: &egui::Painter, rect: Rect, radius: f32, bg: Color32) {
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius as u8),
        Stroke::new(radius, bg),
        egui::StrokeKind::Outside,
    );
}

/// Paints the feature rows (`AddSummaryPremium` + `AddPremiumSummaryIcons`); returns the bottom.
fn paint_features(ui: &mut Ui, left: f32, top: f32, width: f32, palette: &Palette) -> f32 {
    let rows = premium_feature_rows(&[]);
    let title_font = FontId::new(13.0, family_semibold());
    let about_font = FontId::new(13.0, family_regular());
    let text_w = width - FEATURE_TITLE_PADDING[0] - FEATURE_TITLE_PADDING[2];
    let mut layout = Vec::with_capacity(rows.len());
    let mut y = top;
    for entry in &rows {
        let title = ui.ctx().fonts_mut(|f| {
            f.layout(
                (entry.title)(),
                title_font.clone(),
                palette.window_bold_fg,
                text_w,
            )
        });
        let about = ui.ctx().fonts_mut(|f| {
            f.layout(
                (entry.about)(),
                about_font.clone(),
                palette.window_sub_text_fg,
                text_w,
            )
        });
        let height = FEATURE_TITLE_PADDING[1]
            + title.size().y
            + FEATURE_TITLE_PADDING[3]
            + FEATURE_ABOUT_PADDING[1]
            + about.size().y
            + FEATURE_ABOUT_PADDING[3];
        layout.push((y, height, title, about));
        y += height;
    }
    let ascent = 13.0 * 0.8;
    let icon_top =
        |row_top: f32, title_h: f32| row_top + FEATURE_TITLE_PADDING[1] + title_h - ascent;
    let (Some(first), Some(last)) = (layout.first(), layout.last()) else {
        return y;
    };
    let from = icon_top(first.0, first.2.size().y);
    let to = icon_top(last.0, last.2.size().y) + 24.0;
    for (entry, (row_top, height, title, about)) in rows.iter().zip(layout) {
        let row = Rect::from_min_size(Pos2::new(left, row_top), Vec2::new(width, height));
        let resp = ui.interact(
            row,
            Id::unique(("tg_premium_feature", entry.key)),
            Sense::click(),
        );
        let label = (entry.title)();
        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, &label));
        let painter = ui.painter();
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            painter.rect_filled(row, CornerRadius::ZERO, palette.window_bg_over);
        }
        let title_pos = Pos2::new(
            left + FEATURE_TITLE_PADDING[0],
            row_top + FEATURE_TITLE_PADDING[1],
        );
        let title_w = title.size().x;
        let title_h = title.size().y;
        painter.galley(title_pos, title, palette.window_bold_fg);
        if entry.new_badge {
            let badge_galley = ui.ctx().fonts_mut(|f| {
                f.layout_no_wrap(
                    tr::lng_premium_summary_new_badge(),
                    FontId::new(10.0, family_semibold()),
                    Color32::WHITE,
                )
            });
            let badge = Rect::from_min_size(
                Pos2::new(title_pos.x + title_w + 4.0, title_pos.y + 1.0),
                badge_galley.size() + Vec2::new(8.0, 2.0),
            );
            painter.rect_filled(
                badge,
                CornerRadius::same(4),
                Color32::from_rgb(0x40, 0xa7, 0xe3),
            );
            painter.galley(
                badge.min + Vec2::new(4.0, 1.0),
                badge_galley,
                Color32::WHITE,
            );
        }
        painter.galley(
            Pos2::new(
                left + FEATURE_ABOUT_PADDING[0],
                title_pos.y + title_h + FEATURE_TITLE_PADDING[3] + FEATURE_ABOUT_PADDING[1],
            ),
            about,
            palette.window_sub_text_fg,
        );

        // Icon on the full-height gradient (`AddPremiumSummaryIcons`).
        let icon_rect = Rect::from_min_size(
            Pos2::new(left + ICON_LEFT, icon_top(row_top, title_h)),
            Vec2::splat(24.0),
        );
        let span = (to - from).max(1.0);
        let c_top = gradient_at(&FULL_HEIGHT_STOPS, (icon_rect.top() - from) / span);
        let c_bottom = gradient_at(&FULL_HEIGHT_STOPS, (icon_rect.bottom() - from) / span);
        let icon_painter = painter.with_clip_rect(icon_rect);
        fill_linear_gradient(
            &icon_painter,
            icon_rect,
            icon_rect.center_top(),
            icon_rect.center_top() + Vec2::new(0.0, icon_rect.height() * 0.1),
            &[(0.0, c_top), (1.0, c_bottom)],
        );
        paint_round_corners(&icon_painter, icon_rect, ICON_RADIUS, palette.window_bg);
        paint_style_icon(
            painter,
            icon_rect,
            *entry.icon,
            &DefaultPaletteResolver,
            Some(Color32::WHITE),
        );

        // Arrow (`settingsPremiumArrow`).
        let arrow = Rect::from_center_size(
            Pos2::new(row.right() - 18.0, row.center().y - 1.0),
            Vec2::splat(20.0),
        );
        paint_style_icon(
            painter,
            arrow,
            st::settings_premium_arrow,
            &DefaultPaletteResolver,
            None,
        );
    }
    y + FEATURE_ABOUT_PADDING[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_rows_follow_fallback_order_with_tdesktop_texts() {
        let rows = premium_feature_rows(&[]);
        assert_eq!(rows.len(), FALLBACK_ORDER.len());
        assert_eq!(rows[0].key, "stories");
        assert_eq!(rows[2].key, "double_limits");
        assert_eq!(
            (rows[2].title)(),
            tr::lng_premium_summary_subtitle_double_limits()
        );
        assert!(rows.iter().filter(|r| r.new_badge).count() == 2);
        let order = vec![
            "no_ads".to_owned(),
            "unknown".to_owned(),
            "business".to_owned(),
        ];
        let keys: Vec<_> = premium_feature_rows(&order).iter().map(|r| r.key).collect();
        assert_eq!(keys, vec!["no_ads", "business"]);
    }

    #[test]
    fn option_rows_match_subscription_options_for_rows() {
        let annual = tg_payments::premium::create_subscription_option(12, 499.0, 3999, "USD", "");
        let row = premium_option_row(&annual);
        assert_eq!(row.duration, tr::lng_premium_subscribe_months_12());
        assert!(row.per_month.contains("$3.33"), "{row:?}");
        assert!(row.per_year.contains("$39.99"), "{row:?}");
        assert!(!row.discount.is_empty());
        let monthly = tg_payments::premium::create_subscription_option(1, 499.0, 499, "USD", "");
        let row = premium_option_row(&monthly);
        assert_eq!(row.duration, tr::lng_premium_subscribe_months_1());
        assert!(row.per_year.is_empty() && row.cost_no_discount.is_empty());
    }
}
