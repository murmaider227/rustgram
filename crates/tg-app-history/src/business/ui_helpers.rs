//! Shared Telegram Desktop-styled egui widgets and recipients selector helper for
//! Telegram Business and Telegram Premium screens.
//!
//! Port origin: `Telegram/SourceFiles/settings/business/settings_recipients_helper.cpp`
//! and `Telegram/SourceFiles/ui/effects/premium_top_bar.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};

use crate::business::model::{
    BusinessChatTypes, BusinessRecipients, BusinessRecipientsType, BusinessUserRef,
};
use tg_app_core::stand_in::theme::Palette;

/// Telegram Business primary gradient start (sky blue).
pub const BUSINESS_BLUE: Color32 = Color32::from_rgb(0x2A, 0x9D, 0xF4);
/// Telegram Business primary gradient end (teal-emerald).
pub const BUSINESS_TEAL: Color32 = Color32::from_rgb(0x14, 0xB8, 0xA6);
/// Telegram Business coin gold accent.
pub const BUSINESS_GOLD: Color32 = Color32::from_rgb(0xF5, 0xA6, 0x23);
/// Telegram Premium gradient start (indigo).
pub const PREMIUM_INDIGO: Color32 = Color32::from_rgb(0x6B, 0x67, 0xF7);
/// Telegram Premium gradient mid (purple).
pub const PREMIUM_PURPLE: Color32 = Color32::from_rgb(0x9D, 0x5B, 0xE5);
/// Telegram Premium gradient end (rose pink).
pub const PREMIUM_PINK: Color32 = Color32::from_rgb(0xE0, 0x5B, 0x9E);
/// Positive green accent (Open / Enabled / Success).
pub const SUCCESS_GREEN: Color32 = Color32::from_rgb(0x2E, 0xA6, 0x52);
/// Subtle green badge surface.
pub const SUCCESS_SURFACE: Color32 = Color32::from_rgb(0xE8, 0xF8, 0xEE);

/// Applies an alpha multiplier `0.0..=1.0` to `color`.
#[must_use]
pub fn with_alpha(color: Color32, alpha: f32) -> Color32 {
    let a = (f32::from(color.a()) * alpha.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// Linearly interpolates between two RGB colors.
#[must_use]
pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let r = (f32::from(a.r()) * (1.0 - t) + f32::from(b.r()) * t).round() as u8;
    let g = (f32::from(a.g()) * (1.0 - t) + f32::from(b.g()) * t).round() as u8;
    let bl = (f32::from(a.b()) * (1.0 - t) + f32::from(b.b()) * t).round() as u8;
    Color32::from_rgb(r, g, bl)
}

/// Paints a smooth 3-stop horizontal gradient across `rect`.
pub fn paint_horizontal_gradient(
    painter: &egui::Painter,
    rect: Rect,
    left: Color32,
    mid: Color32,
    right: Color32,
) {
    let bands = 18usize;
    let step = rect.width() / bands as f32;
    for i in 0..bands {
        let t = i as f32 / (bands - 1) as f32;
        let col = if t < 0.5 {
            lerp_color(left, mid, t * 2.0)
        } else {
            lerp_color(mid, right, (t - 0.5) * 2.0)
        };
        let band = Rect::from_min_max(
            Pos2::new(rect.left() + i as f32 * step, rect.top()),
            Pos2::new(rect.left() + (i + 1) as f32 * step + 0.5, rect.bottom()),
        );
        painter.rect_filled(band, CornerRadius::ZERO, col);
    }
}

/// Paints a crisp 5-pointed star emblem centered at `center`.
pub fn paint_star_emblem(painter: &egui::Painter, center: Pos2, radius: f32, fill: Color32) {
    let mut pts = Vec::with_capacity(10);
    let inner_r = radius * 0.44;
    for i in 0..10 {
        let angle = -std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::PI / 5.0;
        let r = if i % 2 == 0 { radius } else { inner_r };
        pts.push(Pos2::new(
            center.x + r * angle.cos(),
            center.y + r * angle.sin(),
        ));
    }
    for i in 0..10 {
        let next = (i + 1) % 10;
        painter.add(egui::Shape::convex_polygon(
            vec![center, pts[i], pts[next]],
            fill,
            Stroke::NONE,
        ));
    }
    painter.circle_filled(
        Pos2::new(center.x - radius * 0.14, center.y - radius * 0.12),
        radius * 0.12,
        Color32::from_rgba_unmultiplied(255, 255, 255, 165),
    );
}

/// Paints a stylized Business gold coin / briefcase emblem centered at `center`.
pub fn paint_business_emblem(painter: &egui::Painter, center: Pos2, radius: f32) {
    painter.circle_filled(
        center,
        radius,
        Color32::from_rgba_unmultiplied(255, 255, 255, 55),
    );
    painter.circle_filled(center, radius * 0.84, BUSINESS_GOLD);
    painter.circle_stroke(
        center,
        radius * 0.84,
        Stroke::new(2.0, Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
    );
    // Briefcase glyph inside coin
    let bw = radius * 0.86;
    let bh = radius * 0.62;
    let case_rect = Rect::from_center_size(
        Pos2::new(center.x, center.y + radius * 0.08),
        Vec2::new(bw, bh),
    );
    painter.rect_filled(case_rect, CornerRadius::same(4), Color32::WHITE);
    let handle_rect = Rect::from_center_size(
        Pos2::new(center.x, case_rect.top() - radius * 0.10),
        Vec2::new(bw * 0.44, radius * 0.24),
    );
    painter.rect_stroke(
        handle_rect,
        CornerRadius::same(3),
        Stroke::new(2.0, Color32::WHITE),
        StrokeKind::Outside,
    );
    painter.rect_filled(
        Rect::from_center_size(case_rect.center(), Vec2::new(bw * 0.22, bh * 0.22)),
        CornerRadius::same(2),
        BUSINESS_GOLD,
    );
}

/// Paints a Telegram Desktop-style circular avatar with initials.
pub fn paint_avatar(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    rgb: [u8; 3],
    initials: &str,
) {
    painter.circle_filled(center, radius, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initials,
        FontId::proportional((radius * 0.78).max(10.0)),
        Color32::WHITE,
    );
}

/// Paints a tdesktop thick section divider strip.
pub fn section_divider(ui: &mut Ui, palette: &Palette) {
    ui.add_space(6.0);
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 8.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, palette.window_bg_over);
    ui.painter().line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, palette.divider),
    );
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, palette.divider),
    );
    ui.add_space(6.0);
}

/// Paints a 1px hairline separator.
pub fn thin_divider(ui: &mut Ui, palette: &Palette) {
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, palette.divider),
    );
}

/// Renders a blue subsection header (`Ui::AddSubsectionTitle`).
pub fn subsection_title(ui: &mut Ui, text: &str, palette: &Palette) {
    ui.label(
        egui::RichText::new(text)
            .size(13.0)
            .strong()
            .color(palette.window_active_text_fg),
    );
}

/// Renders a muted explanatory divider text block (`Ui::AddDividerText`).
pub fn divider_caption(ui: &mut Ui, text: &str, palette: &Palette) {
    Frame::NONE
        .fill(palette.window_bg_over)
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(text)
                    .size(11.5)
                    .color(palette.window_sub_text_fg),
            );
        });
}

/// Draws a Telegram Desktop-style pill toggle switch (`Ui::ToggleView`).
pub fn toggle_switch(ui: &mut Ui, checked: &mut bool, palette: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(38.0, 21.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let mut changed = false;
    if resp.clicked() {
        *checked = !*checked;
        changed = true;
    }
    let track_col = if *checked {
        palette.window_bg_active
    } else {
        with_alpha(palette.window_sub_text_fg, 0.35)
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(11), track_col);
    let knob_x = if *checked {
        rect.right() - 10.5
    } else {
        rect.left() + 10.5
    };
    ui.painter()
        .circle_filled(Pos2::new(knob_x, rect.center().y), 8.0, Color32::WHITE);
    changed
}

/// Draws a full-width toggle settings row (`SettingsButton::toggleOn`), wrapping label and
/// optional subtitle strictly left of the toggle switch so text never overlaps on narrow widths.
pub fn settings_toggle_row(
    ui: &mut Ui,
    label: &str,
    subtitle: Option<&str>,
    checked: &mut bool,
    palette: &Palette,
) -> bool {
    let width = ui.available_width().max(160.0);
    let text_offset_x = 6.0_f32;
    let switch_w = 38.0_f32;
    let right_pad = 6.0_f32;
    let text_gap = 12.0_f32;
    let text_max_w = (width - right_pad - switch_w - text_gap - text_offset_x).max(48.0);

    let title_galley = ui.painter().layout(
        label.to_owned(),
        FontId::proportional(13.5),
        palette.window_fg,
        text_max_w,
    );
    let sub_galley = subtitle.filter(|s| !s.is_empty()).map(|sub| {
        ui.painter().layout(
            sub.to_owned(),
            FontId::proportional(11.5),
            palette.window_sub_text_fg,
            text_max_w,
        )
    });

    let pad_v = 8.0_f32;
    let sub_gap = 3.0_f32;
    let h = if let Some(ref sg) = sub_galley {
        (pad_v + title_galley.size().y + sub_gap + sg.size().y + pad_v).max(50.0)
    } else {
        (pad_v + title_galley.size().y + pad_v).max(42.0)
    };

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, h), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(6),
            Stroke::new(1.5, palette.window_bg_active),
            egui::StrokeKind::Inside,
        );
    }
    let mut changed = false;
    if resp.clicked() {
        *checked = !*checked;
        changed = true;
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, *checked, label));

    let switch_left = rect.right() - right_pad - switch_w;
    let text_x = rect.left() + text_offset_x;
    let text_clip = Rect::from_min_max(
        Pos2::new(text_x, rect.top()),
        Pos2::new(
            (text_x + text_max_w).min(switch_left - text_gap * 0.5),
            rect.bottom(),
        ),
    );
    let text_painter = ui.painter().with_clip_rect(text_clip);

    if let Some(sg) = sub_galley {
        let content_h = title_galley.size().y + sub_gap + sg.size().y;
        let top_y = (rect.center().y - content_h * 0.5).max(rect.top() + pad_v * 0.75);
        text_painter.galley(Pos2::new(text_x, top_y), title_galley, palette.window_fg);
        text_painter.galley(
            Pos2::new(text_x, top_y + content_h - sg.size().y),
            sg,
            palette.window_sub_text_fg,
        );
    } else {
        let top_y = rect.center().y - title_galley.size().y * 0.5;
        text_painter.galley(Pos2::new(text_x, top_y), title_galley, palette.window_fg);
    }

    let switch_rect = Rect::from_min_size(
        Pos2::new(switch_left, rect.center().y - 10.5),
        Vec2::new(switch_w, 21.0),
    );
    let anim_secs = if ui.style().animation_time <= 0.0 {
        0.0
    } else {
        0.16
    };
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, *checked, anim_secs);
    let off_col = with_alpha(palette.window_sub_text_fg, 0.35);
    let track_col = Color32::from_rgba_unmultiplied(
        (f32::from(off_col.r())
            + (f32::from(palette.window_bg_active.r()) - f32::from(off_col.r())) * t)
            .round() as u8,
        (f32::from(off_col.g())
            + (f32::from(palette.window_bg_active.g()) - f32::from(off_col.g())) * t)
            .round() as u8,
        (f32::from(off_col.b())
            + (f32::from(palette.window_bg_active.b()) - f32::from(off_col.b())) * t)
            .round() as u8,
        (f32::from(off_col.a())
            + (f32::from(palette.window_bg_active.a()) - f32::from(off_col.a())) * t)
            .round() as u8,
    );
    ui.painter()
        .rect_filled(switch_rect, CornerRadius::same(11), track_col);
    let knob_x = (switch_rect.left() + 10.5)
        + ((switch_rect.right() - 10.5) - (switch_rect.left() + 10.5)) * t;
    ui.painter().circle_filled(
        Pos2::new(knob_x, switch_rect.center().y),
        8.0,
        Color32::WHITE,
    );
    changed
}

/// Draws a Telegram-style radio button row (`Ui::Radiobutton` / `Ui::Radioenum`).
pub fn radio_row(ui: &mut Ui, label: &str, selected: bool, palette: &Palette) -> bool {
    let width = ui.available_width().max(140.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
    }
    let center = Pos2::new(rect.left() + 14.0, rect.center().y);
    let ring_col = if selected {
        palette.window_bg_active
    } else {
        palette.window_sub_text_fg
    };
    ui.painter()
        .circle_stroke(center, 7.5, Stroke::new(1.6, ring_col));
    if selected {
        ui.painter()
            .circle_filled(center, 4.2, palette.window_bg_active);
    }
    ui.painter().text(
        Pos2::new(rect.left() + 30.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        palette.window_fg,
    );
    resp.clicked()
}

/// Draws a full-width rounded primary action button.
pub fn primary_button(
    ui: &mut Ui,
    label: &str,
    fill: Color32,
    fg: Color32,
    height: f32,
) -> Response {
    let width = ui.available_width().max(120.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let bg = if resp.is_pointer_button_down_on() {
        with_alpha(fill, 0.85)
    } else if resp.hovered() {
        with_alpha(fill, 0.92)
    } else {
        fill
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.5),
        fg,
    );
    resp
}

/// Draws a compact pill button sized to fit `label`.
pub fn pill_button(
    ui: &mut Ui,
    label: &str,
    bg: Color32,
    fg: Color32,
    border: Option<Color32>,
) -> Response {
    let galley =
        ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), FontId::proportional(12.0), fg));
    let size = Vec2::new(galley.size().x + 20.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if resp.hovered() {
        with_alpha(bg, 0.88)
    } else {
        bg
    };
    ui.painter().rect_filled(rect, CornerRadius::same(14), fill);
    if let Some(stroke_col) = border {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(14),
            Stroke::new(1.0, stroke_col),
            StrokeKind::Inside,
        );
    }
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
    resp
}

/// Draws a static icon header card (analog of `AddDividerTextWithLottie` in tdesktop business screens).
pub fn business_section_intro(
    ui: &mut Ui,
    badge_glyph: &str,
    badge_bg: Color32,
    title: &str,
    about: &str,
    palette: &Palette,
) {
    Frame::NONE
        .fill(palette.window_bg_over)
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
                ui.painter().rect_filled(
                    icon_rect,
                    CornerRadius::same(16),
                    with_alpha(badge_bg, 0.16),
                );
                ui.painter().rect_stroke(
                    icon_rect,
                    CornerRadius::same(16),
                    Stroke::new(1.5, with_alpha(badge_bg, 0.45)),
                    StrokeKind::Inside,
                );
                ui.painter().text(
                    icon_rect.center(),
                    Align2::CENTER_CENTER,
                    badge_glyph,
                    FontId::proportional(17.0),
                    badge_bg,
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(title)
                        .size(15.5)
                        .strong()
                        .color(palette.window_bold_fg),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(about)
                        .size(12.0)
                        .color(palette.window_sub_text_fg),
                );
            });
        });
}

/// Formats a Unix timestamp as `DD Mon YYYY, HH:MM` UTC without external chrono dependencies.
#[must_use]
pub fn format_unix_datetime(ts: i32) -> String {
    if ts <= 0 {
        return "—".to_owned();
    }
    let secs = i64::from(ts);
    let days = secs.div_euclid(86_400);
    let day_secs = secs.rem_euclid(86_400);
    let hour = day_secs / 3_600;
    let minute = (day_secs % 3_600) / 60;
    let (year, month, day) = civil_from_days(days);
    let month_name = match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    };
    format!("{day:02} {month_name} {year}, {hour:02}:{minute:02}")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

// -----------------------------------------------------------------------------
// Recipients Selector Helper (`settings_recipients_helper.cpp`)
// -----------------------------------------------------------------------------

/// UI expansion state for [`render_recipients_selector`].
#[derive(Debug, Clone, Default)]
pub struct RecipientsPickerUiState {
    /// Whether the inline Included chats editor is expanded.
    pub editing_included: bool,
    /// Whether the inline Excluded chats editor is expanded.
    pub editing_excluded: bool,
}

/// Renders Telegram Business Recipients selector (`AddBusinessRecipientsSelector` in
/// `settings_recipients_helper.cpp`).
///
/// Returns `true` when `recipients` changed.
pub fn render_recipients_selector(
    ui: &mut Ui,
    title: &str,
    recipients: &mut BusinessRecipients,
    kind: BusinessRecipientsType,
    available_users: &[BusinessUserRef],
    picker_state: &mut RecipientsPickerUiState,
    palette: &Palette,
) -> bool {
    let mut changed = false;
    let include_with_excluded = kind == BusinessRecipientsType::Bots;

    subsection_title(ui, title, palette);
    ui.add_space(4.0);

    let all_except = recipients.all_but_excluded || recipients.included.is_empty();
    if radio_row(ui, "All private chats", all_except, palette) && !recipients.all_but_excluded {
        recipients.all_but_excluded = true;
        picker_state.editing_included = false;
        changed = true;
    }
    if radio_row(ui, "Selected chats", !all_except, palette) {
        if recipients.included.is_empty() {
            // Mirror `settings_recipients_helper.cpp`: open the included picker immediately
            // when switching to Selected chats while included is empty.
            picker_state.editing_included = true;
            recipients.included.types = BusinessChatTypes::NEW_CHATS;
            recipients.all_but_excluded = false;
            changed = true;
        } else if recipients.all_but_excluded {
            recipients.all_but_excluded = false;
            changed = true;
        }
    }

    ui.add_space(6.0);
    thin_divider(ui, palette);
    ui.add_space(6.0);

    // Included section (visible when `all_but_excluded == false`)
    if !recipients.all_but_excluded {
        ui.horizontal(|ui| {
            subsection_title(ui, "Included Chats", palette);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let lbl = if picker_state.editing_included {
                    "Done"
                } else {
                    "+ Add Chats"
                };
                if pill_button(
                    ui,
                    lbl,
                    with_alpha(palette.window_bg_active, 0.14),
                    palette.window_active_text_fg,
                    None,
                )
                .clicked()
                {
                    picker_state.editing_included = !picker_state.editing_included;
                }
            });
        });
        ui.add_space(4.0);
        if render_chats_chips(
            ui,
            &mut recipients.included.types,
            &mut recipients.included.user_ids,
            available_users,
            palette.window_bg_active,
            palette,
        ) {
            if recipients.included.is_empty() {
                recipients.all_but_excluded = true;
            }
            changed = true;
        }
        if picker_state.editing_included
            && render_chats_picker_drawer(
                ui,
                &mut recipients.included.types,
                &mut recipients.included.user_ids,
                available_users,
                false,
                palette,
            )
        {
            if include_with_excluded {
                for uid in &recipients.included.user_ids {
                    recipients.excluded.user_ids.retain(|e| e != uid);
                }
            }
            if !recipients.included.is_empty() {
                recipients.excluded.types = BusinessChatTypes::EMPTY;
            } else {
                recipients.all_but_excluded = true;
            }
            changed = true;
        }
        ui.add_space(6.0);
    }

    // Excluded section (visible when `all_but_excluded == true`, or always in `Bots` mode)
    if recipients.all_but_excluded || include_with_excluded {
        let users_only = include_with_excluded && !recipients.all_but_excluded;
        ui.horizontal(|ui| {
            subsection_title(ui, "Excluded Chats", palette);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let lbl = if picker_state.editing_excluded {
                    "Done"
                } else {
                    "- Exclude Chats"
                };
                if pill_button(
                    ui,
                    lbl,
                    with_alpha(palette.attention_fg, 0.14),
                    palette.attention_fg,
                    None,
                )
                .clicked()
                {
                    picker_state.editing_excluded = !picker_state.editing_excluded;
                }
            });
        });
        ui.add_space(4.0);
        if render_chats_chips(
            ui,
            &mut recipients.excluded.types,
            &mut recipients.excluded.user_ids,
            available_users,
            palette.attention_fg,
            palette,
        ) {
            changed = true;
        }
        if picker_state.editing_excluded
            && render_chats_picker_drawer(
                ui,
                &mut recipients.excluded.types,
                &mut recipients.excluded.user_ids,
                available_users,
                users_only,
                palette,
            )
        {
            if include_with_excluded {
                if users_only {
                    recipients.excluded.types = BusinessChatTypes::EMPTY;
                }
                for uid in &recipients.excluded.user_ids {
                    recipients.included.user_ids.retain(|inc| inc != uid);
                }
            }
            if recipients.all_but_excluded && !recipients.excluded.is_empty() {
                recipients.included = Default::default();
            }
            changed = true;
        }
    }

    changed
}

fn render_chats_chips(
    ui: &mut Ui,
    types: &mut BusinessChatTypes,
    user_ids: &mut Vec<i64>,
    available_users: &[BusinessUserRef],
    accent: Color32,
    palette: &Palette,
) -> bool {
    let mut changed = false;
    let flags = [
        (BusinessChatTypes::NEW_CHATS, "New Chats"),
        (BusinessChatTypes::EXISTING_CHATS, "Existing Chats"),
        (BusinessChatTypes::CONTACTS, "Contacts"),
        (BusinessChatTypes::NON_CONTACTS, "Non-Contacts"),
    ];
    let has_any = !types.is_empty() || !user_ids.is_empty();
    if !has_any {
        ui.label(
            egui::RichText::new("None selected")
                .size(11.5)
                .color(palette.window_sub_text_fg),
        );
        return false;
    }

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        for (flag, label) in flags {
            if types.contains(flag)
                && pill_button(
                    ui,
                    &format!("{label}  x"),
                    with_alpha(accent, 0.14),
                    accent,
                    Some(with_alpha(accent, 0.35)),
                )
                .clicked()
            {
                *types = types.difference(flag);
                changed = true;
            }
        }
        let mut to_remove = None;
        for &uid in user_ids.iter() {
            let name = available_users
                .iter()
                .find(|u| u.id == uid)
                .map_or("User", |u| u.name.as_str());
            if pill_button(
                ui,
                &format!("{name}  x"),
                palette.window_bg_over,
                palette.window_fg,
                Some(palette.divider),
            )
            .clicked()
            {
                to_remove = Some(uid);
            }
        }
        if let Some(uid) = to_remove {
            user_ids.retain(|&id| id != uid);
            changed = true;
        }
    });
    changed
}

fn render_chats_picker_drawer(
    ui: &mut Ui,
    types: &mut BusinessChatTypes,
    user_ids: &mut Vec<i64>,
    available_users: &[BusinessUserRef],
    users_only: bool,
    palette: &Palette,
) -> bool {
    let mut changed = false;
    ui.add_space(4.0);
    Frame::NONE
        .fill(palette.window_bg_over)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            if !users_only {
                ui.label(
                    egui::RichText::new("Chat Types")
                        .size(11.5)
                        .strong()
                        .color(palette.window_sub_text_fg),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    let flags = [
                        (BusinessChatTypes::NEW_CHATS, "New Chats"),
                        (BusinessChatTypes::EXISTING_CHATS, "Existing Chats"),
                        (BusinessChatTypes::CONTACTS, "Contacts"),
                        (BusinessChatTypes::NON_CONTACTS, "Non-Contacts"),
                    ];
                    for (flag, label) in flags {
                        let mut active = types.contains(flag);
                        if ui.checkbox(&mut active, label).changed() {
                            if active {
                                *types = types.union(flag);
                            } else {
                                *types = types.difference(flag);
                            }
                            changed = true;
                        }
                    }
                });
                ui.add_space(6.0);
            }
            ui.label(
                egui::RichText::new("Specific Chats")
                    .size(11.5)
                    .strong()
                    .color(palette.window_sub_text_fg),
            );
            ui.add_space(4.0);
            for user in available_users {
                let mut selected = user_ids.contains(&user.id);
                let tag = if user.is_contact { "contact" } else { "lead" };
                let lbl = format!("{} (@{}, {tag})", user.name, user.username);
                if ui.checkbox(&mut selected, lbl).changed() {
                    if selected {
                        if !user_ids.contains(&user.id) {
                            user_ids.push(user.id);
                        }
                    } else {
                        user_ids.retain(|&id| id != user.id);
                    }
                    changed = true;
                }
            }
        });
    changed
}
