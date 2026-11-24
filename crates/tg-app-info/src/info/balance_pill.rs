//! Profile TON / Stars balance pill (`Info::Profile::BalancePill`).
//!
//! Port origin: `Telegram/SourceFiles/info/profile/info_profile_balance_pill.h`,
//! `Telegram/SourceFiles/info/profile/info_profile_balance_pill.cpp`,
//! `Telegram/SourceFiles/info/profile/info_profile_music_button.cpp`,
//! and `Telegram/SourceFiles/info/profile/info_profile_top_bar.style`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::stand_in::Colors;

use crate::info::backend::ProfileBalanceInfo;
use crate::payments::credits::format_stars_count;
use crate::payments::ui_helpers::{STAR_GOLD, paint_star_emblem, with_alpha};

/// Nanotons in 1 TON (`Ui::kNanosInOne` in `ton_common.h`).
pub const NANOS_PER_TON: i64 = 1_000_000_000;

/// TON diamond accent color (`st::currencyFg`).
const TON_BLUE: Color32 = Color32::from_rgb(0x00, 0x98, 0xEA);

/// Formatted whole + optional fractional parts for a TON amount (`Wallet::AmountParts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceAmountParts {
    /// Whole integer part (with thousands grouping if >= 1000).
    pub whole: String,
    /// Optional fractional part including leading decimal point (e.g. `".45"`), or empty.
    pub fraction: String,
}

/// Formats `ton_nanos` into whole and trimmed fractional parts matching `BalanceParts` in
/// `info_profile_balance_pill.cpp:36-43`.
#[must_use]
pub fn format_ton_balance_parts(ton_nanos: i64) -> BalanceAmountParts {
    let clamped = ton_nanos.max(0);
    let whole_val = clamped / NANOS_PER_TON;
    let rem = clamped % NANOS_PER_TON;
    let whole = format_stars_count(whole_val);
    if rem == 0 {
        return BalanceAmountParts {
            whole,
            fraction: String::new(),
        };
    }
    // Display up to 2 significant decimal digits (or up to 4 for tiny sub-cent balances) without trailing zeros.
    let frac_4 = format!("{:04}", rem / 100_000);
    let trimmed = if rem >= 10_000_000 {
        let two = &frac_4[..2];
        two.trim_end_matches('0')
    } else {
        frac_4.trim_end_matches('0')
    };
    let fraction = if trimmed.is_empty() {
        String::new()
    } else {
        format!(".{trimmed}")
    };
    BalanceAmountParts { whole, fraction }
}

/// Which balance pill was clicked in [`ProfileBalancePill::show`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalancePillClick {
    /// Clicked the TON balance pill (`info_profile_balance_pill.cpp`).
    Ton,
    /// Clicked the Telegram Stars balance pill.
    Stars,
}

/// Renders Telegram Desktop's compact profile balance pill(s) (`info_profile_balance_pill.cpp`).
///
/// Hidden automatically when both `ton_nanos <= 0` and `stars_balance <= 0`
/// (matching `BalancePill::setContent` lines 133-139).
pub struct ProfileBalancePill;

impl ProfileBalancePill {
    /// Renders the TON and/or Stars balance pills inline with `24px` accessible hit targets
    /// (visual pill height `20px`, full pill radius `height / 2`, tabular figures, and chevron).
    pub fn show(
        ui: &mut Ui,
        balance: &ProfileBalanceInfo,
        colors: &dyn Colors,
    ) -> Option<BalancePillClick> {
        let has_ton = balance.ton_nanos > 0;
        let has_stars = balance.stars_balance > 0;
        if !has_ton && !has_stars {
            return None;
        }

        let mut clicked = None;
        let label_text = tr::lng_wallet_profile_balance();

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);

            if has_ton {
                let parts = format_ton_balance_parts(balance.ton_nanos);
                let accessible = format!("{label_text} {}{} TON", parts.whole, parts.fraction);
                if paint_ton_pill(ui, &label_text, &parts, &accessible, colors) {
                    clicked = Some(BalancePillClick::Ton);
                }
            }

            if has_stars {
                let stars_str = format_stars_count(balance.stars_balance);
                let accessible = format!("Stars balance: {stars_str} Stars");
                if paint_stars_pill(ui, &stars_str, &accessible, colors) {
                    clicked = Some(BalancePillClick::Stars);
                }
            }
        });

        clicked
    }
}

fn paint_ton_pill(
    ui: &mut Ui,
    label_text: &str,
    parts: &BalanceAmountParts,
    accessible_name: &str,
    colors: &dyn Colors,
) -> bool {
    let label_font = FontId::new(10.5, family_regular());
    let amount_font = FontId::new(11.0, family_semibold());
    let frac_font = FontId::new(9.0, family_semibold());
    let primary = colors.color("windowBoldFg");
    let secondary = colors.color("windowSubTextFg");

    let label_galley = ui
        .painter()
        .layout_no_wrap(label_text.to_owned(), label_font, secondary);
    let whole_galley = ui
        .painter()
        .layout_no_wrap(parts.whole.clone(), amount_font, primary);
    let frac_galley = if parts.fraction.is_empty() {
        None
    } else {
        Some(
            ui.painter()
                .layout_no_wrap(parts.fraction.clone(), frac_font, primary),
        )
    };
    let frac_w = frac_galley.as_ref().map_or(0.0, |g| g.size().x);

    // Metrics matching `info_profile_top_bar.style` (`Padding(6,0,5,0)`, `IconSize 12`, skips).
    let pad_l = 8.0;
    let pad_r = 7.0;
    let icon_size = 11.0;
    let label_skip = 4.0;
    let icon_skip = 3.0;
    let arrow_skip = 4.0;
    let arrow_w = 5.0;
    let pill_w = (pad_l
        + label_galley.size().x
        + label_skip
        + icon_size
        + icon_skip
        + whole_galley.size().x
        + frac_w
        + arrow_skip
        + arrow_w
        + pad_r)
        .min(ui.available_width().max(120.0));

    // Allocate 24px vertical hit target (WCAG 2.5.8) while painting the compact 20px pill inside.
    let (hit_rect, resp) = ui.allocate_exact_size(Vec2::new(pill_w, 24.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, accessible_name));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let pill_rect = Rect::from_center_size(hit_rect.center(), Vec2::new(pill_w, 20.0));
    let bg = if resp.hovered() {
        with_alpha(TON_BLUE, 0.16)
    } else {
        colors.color("windowBgOver")
    };
    let border = if resp.hovered() || resp.has_focus() {
        TON_BLUE
    } else {
        colors.color("shadowFg")
    };
    ui.painter()
        .rect_filled(pill_rect, CornerRadius::same(10), bg);
    ui.painter().rect_stroke(
        pill_rect,
        CornerRadius::same(10),
        Stroke::new(0.75, border),
        egui::StrokeKind::Inside,
    );

    let clip = ui
        .painter()
        .with_clip_rect(pill_rect.shrink2(Vec2::new(2.0, 0.0)));
    let cy = pill_rect.center().y;
    let mut x = pill_rect.left() + pad_l;

    // 1. "Balance:" secondary label
    clip.galley(
        Pos2::new(x, cy - label_galley.size().y * 0.5),
        label_galley.clone(),
        secondary,
    );
    x += label_galley.size().x + label_skip;

    // 2. TON diamond emblem (`IconCurrencyTwoTone`)
    paint_ton_diamond(
        &clip,
        Pos2::new(x + icon_size * 0.5, cy),
        icon_size * 0.45,
        TON_BLUE,
    );
    x += icon_size + icon_skip;

    // 3. Whole + smaller fractional amount (`Wallet::AmountPainter`)
    let whole_top = cy - whole_galley.size().y * 0.5;
    clip.galley(Pos2::new(x, whole_top), whole_galley.clone(), primary);
    x += whole_galley.size().x;
    if let Some(fg) = frac_galley {
        clip.galley(
            Pos2::new(x, whole_top + whole_galley.size().y - fg.size().y - 0.5),
            fg,
            primary,
        );
        x += frac_w;
    }

    // 4. Trailing chevron (`infoProfileBalancePillArrow`)
    let arrow_x = (x + arrow_skip).min(pill_rect.right() - pad_r - arrow_w * 0.5);
    clip.text(
        Pos2::new(arrow_x, cy),
        Align2::LEFT_CENTER,
        "›",
        FontId::new(11.5, family_semibold()),
        primary,
    );

    resp.clicked()
}

fn paint_stars_pill(
    ui: &mut Ui,
    stars_text: &str,
    accessible_name: &str,
    colors: &dyn Colors,
) -> bool {
    let amount_font = FontId::new(11.0, family_semibold());
    let primary = colors.color("windowBoldFg");
    let amount_galley = ui
        .painter()
        .layout_no_wrap(stars_text.to_owned(), amount_font, primary);

    let pad_l = 8.0;
    let pad_r = 7.0;
    let icon_size = 11.0;
    let icon_skip = 3.0;
    let arrow_skip = 4.0;
    let arrow_w = 5.0;
    let pill_w =
        (pad_l + icon_size + icon_skip + amount_galley.size().x + arrow_skip + arrow_w + pad_r)
            .min(ui.available_width().max(80.0));

    let (hit_rect, resp) = ui.allocate_exact_size(Vec2::new(pill_w, 24.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, accessible_name));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let pill_rect = Rect::from_center_size(hit_rect.center(), Vec2::new(pill_w, 20.0));
    let bg = if resp.hovered() {
        with_alpha(STAR_GOLD, 0.22)
    } else {
        colors.color("windowBgOver")
    };
    let border = if resp.hovered() || resp.has_focus() {
        STAR_GOLD
    } else {
        colors.color("shadowFg")
    };
    ui.painter()
        .rect_filled(pill_rect, CornerRadius::same(10), bg);
    ui.painter().rect_stroke(
        pill_rect,
        CornerRadius::same(10),
        Stroke::new(0.75, border),
        egui::StrokeKind::Inside,
    );

    let cy = pill_rect.center().y;
    let mut x = pill_rect.left() + pad_l;
    paint_star_emblem(
        ui.painter(),
        Pos2::new(x + icon_size * 0.5, cy),
        icon_size * 0.48,
        STAR_GOLD,
    );
    x += icon_size + icon_skip;

    ui.painter().galley(
        Pos2::new(x, cy - amount_galley.size().y * 0.5),
        amount_galley.clone(),
        primary,
    );
    x += amount_galley.size().x + arrow_skip;

    ui.painter().text(
        Pos2::new(x, cy),
        Align2::LEFT_CENTER,
        "›",
        FontId::new(11.5, family_semibold()),
        primary,
    );

    resp.clicked()
}

fn paint_ton_diamond(painter: &egui::Painter, center: Pos2, r: f32, color: Color32) {
    let pts = vec![
        Pos2::new(center.x, center.y - r),
        Pos2::new(center.x + r, center.y - r * 0.15),
        Pos2::new(center.x, center.y + r),
        Pos2::new(center.x - r, center.y - r * 0.15),
    ];
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ton_balance_whole_and_fraction_cleanly() {
        let exact = format_ton_balance_parts(12 * NANOS_PER_TON);
        assert_eq!(exact.whole, "12");
        assert_eq!(exact.fraction, "");

        let with_cents = format_ton_balance_parts(12 * NANOS_PER_TON + 450_000_000);
        assert_eq!(with_cents.whole, "12");
        assert_eq!(with_cents.fraction, ".45");

        let with_one_digit = format_ton_balance_parts(5 * NANOS_PER_TON + 500_000_000);
        assert_eq!(with_one_digit.whole, "5");
        assert_eq!(with_one_digit.fraction, ".5");

        let large = format_ton_balance_parts(1_250 * NANOS_PER_TON + 70_000_000);
        assert_eq!(large.whole, "1,250");
        assert_eq!(large.fraction, ".07");
    }
}
