//! "About Cocoon" modal box explaining confidential compute for translations and AI tools.
//!
//! Port origin: `Telegram/SourceFiles/ui/boxes/about_cocoon_box.h`,
//! `Telegram/SourceFiles/ui/boxes/about_cocoon_box.cpp`,
//! and `Telegram/SourceFiles/ui/chat/chat.style` (`cocoon*`, `stakeBox`).
//!
//! Uses only neutral icons from `tg-style` (`menuIconLock`, `menuIconStats`,
//! `menuIconGiftPremium`) and never copies Telegram or Cocoon trademark logo artwork.

use egui::accesskit::Role;
use egui::{
    Align2, Area, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Order, Pos2, Rect,
    ScrollArea, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle};
use tg_ui::text::RichText;
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;
use tg_ui::widgets::labels::FlatLabel;

// Exact cover palette constants from `about_cocoon_box.cpp:35-42`.
const COVER_GRADIENT_EDGE: Color32 = Color32::from_rgb(0x06, 0x11, 0x29);
const COVER_GRADIENT_CENTER: Color32 = Color32::from_rgb(0x09, 0x16, 0x43);
const COVER_ACCENT_LEFT: Color32 = Color32::from_rgb(0x68, 0xC9, 0xFF);
const COVER_ACCENT_RIGHT: Color32 = Color32::from_rgb(0xCB, 0x57, 0xFF);
const COVER_SUBTITLE_FG: Color32 = Color32::from_rgb(0xB8, 0xC9, 0xEF);
const COVER_BOLD_FG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// Number of radial mini-star particles on the dark cover (`kParticlesCount = 50`).
const COVER_PARTICLE_COUNT: usize = 50;
/// Normalized radial particle speed (`setSpeed(0.05)` in `about_cocoon_box.cpp:99`).
const COVER_PARTICLE_SPEED: f64 = 0.05;

/// Link descriptor matching `CocoonLinkInfo` (`about_cocoon_box.cpp:212-234`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CocoonLinkInfo {
    /// Visible link label text.
    pub text: String,
    /// Target URL (`tg://resolve?domain=...` or `https://...`).
    pub link: String,
}

/// Builds the `@cocoon` mention link (`CocoonMention` in `about_cocoon_box.cpp:217-222`).
#[must_use]
pub fn cocoon_mention_link() -> CocoonLinkInfo {
    let text = tr::lng_translate_cocoon_private_mention();
    let username = text.replace('@', "");
    let link = format!("tg://resolve?domain={username}");
    CocoonLinkInfo { text, link }
}

/// Builds the `cocoon.org` domain link (`CocoonDomain` in `about_cocoon_box.cpp:224-228`).
#[must_use]
pub fn cocoon_domain_link() -> CocoonLinkInfo {
    let text = tr::lng_translate_cocoon_everyone_domain();
    let link = format!("https://{text}");
    CocoonLinkInfo { text, link }
}

/// Builds the `t.me/cocoon?direct` direct link (`CocoonDirect` in `about_cocoon_box.cpp:230-234`).
#[must_use]
pub fn cocoon_direct_link() -> CocoonLinkInfo {
    let text = tr::lng_translate_cocoon_text_link();
    let link = format!("https://{text}");
    CocoonLinkInfo { text, link }
}

/// Action emitted by [`AboutCocoonBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AboutCocoonAction {
    /// User clicked one of the links inside the box (`@cocoon`, `cocoon.org`, `t.me/cocoon?direct`).
    OpenLink(String),
    /// User dismissed the box via `"Understood"`, close button, backdrop, or `Escape`.
    Close,
}

/// Stateful "About Cocoon" dialog (`Ui::AboutCocoonBox`).
#[derive(Debug, Clone, Default)]
pub struct AboutCocoonBoxState {
    /// Most recently clicked link URL inside this modal, if any.
    pub last_clicked_link: Option<String>,
}

impl AboutCocoonBoxState {
    /// Creates a new `AboutCocoonBox` modal state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a `tg-ui` [`BoxModal`] descriptor (`boxWideWidth = 390`).
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_translate_cocoon_title())
            .width(st::box_wide_width.0 as f32)
            .add_button(BoxButton::default_btn(
                "close",
                tr::lng_translate_cocoon_done(),
            ))
    }

    /// Renders the standalone `AboutCocoonBox` modal overlay (dark gradient cover with neutral
    /// lock emblem, 3 feature rows, divider, footer link, and wide `"Understood"` CTA).
    ///
    /// Ambient star particles inside the cover are strictly gated off when `paused_effects` is
    /// `true` or `egui` reduced-motion (`style.animation_time == 0.0`) is active.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
        paused_effects: bool,
    ) -> Option<AboutCocoonAction> {
        let screen = ui.ctx().viewport_rect();
        let mut action = None;
        let mut backdrop_clicked = false;

        Area::new(Id::unique("tg_about_cocoon_backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .show(ui.ctx(), |ui| {
                let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(
                    r,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 125),
                );
                if resp.clicked() {
                    backdrop_clicked = true;
                }
            });

        let box_radius = (st::box_radius.0 as f32).clamp(8.0, 14.0) as u8;
        let box_w = (st::box_wide_width.0 as f32).min((screen.width() - 24.0).max(280.0));
        let max_h = (screen.height() - 32.0).max(360.0);

        let window_bg = colors.color("windowBg");
        let window_shadow = colors.color("windowShadowFg");
        let reduced_motion =
            paused_effects || tg_ui::anim::disabled() || ui.style().animation_time == 0.0;

        Area::new(Id::unique("tg_about_cocoon_modal"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(window_bg)
                    .stroke(Stroke::new(1.0, window_shadow))
                    .corner_radius(CornerRadius::same(box_radius))
                    .inner_margin(egui::Margin::ZERO)
                    .show(ui, |ui| {
                        ui.set_min_width(box_w);
                        ui.set_max_width(box_w);
                        ui.set_max_height(max_h);

                        ScrollArea::vertical()
                            .id_salt("tg_about_cocoon_scroll")
                            .max_height(max_h)
                            .show(ui, |ui| {
                                ui.set_min_width(box_w);
                                ui.set_max_width(box_w);
                                if let Some(ev) = self.paint_contents(
                                    ui,
                                    box_w,
                                    box_radius,
                                    colors,
                                    reduced_motion,
                                    true,
                                ) {
                                    action = Some(ev);
                                }
                            });
                    });
            });

        if backdrop_clicked || ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            action = Some(AboutCocoonAction::Close);
        }
        action
    }

    /// Renders the box body when hosted inside `BoxHost` (`BoxKind::AboutCocoon`).
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        colors: &dyn Colors,
        paused_effects: bool,
    ) -> Option<String> {
        let box_w = ui.available_width().max(260.0);
        let reduced_motion =
            paused_effects || tg_ui::anim::disabled() || ui.style().animation_time == 0.0;
        let box_radius = (st::box_radius.0 as f32).clamp(8.0, 14.0) as u8;
        match self.paint_contents(ui, box_w, box_radius, colors, reduced_motion, false) {
            Some(AboutCocoonAction::OpenLink(url)) => Some(url),
            _ => None,
        }
    }

    fn paint_contents(
        &mut self,
        ui: &mut Ui,
        box_w: f32,
        box_radius: u8,
        colors: &dyn Colors,
        reduced_motion: bool,
        include_cta: bool,
    ) -> Option<AboutCocoonAction> {
        let mut action = None;

        // 1. Dark cosmic cover with radial particles, neutral lock badge, title, subtitle, and close X.
        if paint_cocoon_cover(ui, box_w, box_radius, colors, reduced_motion) {
            action = Some(AboutCocoonAction::Close);
        }

        // 2. Three feature list rows (`Ui::FeatureListEntry` layout, `boxRowPadding` = 24px).
        let row_pad_x = (st::box_row_padding.left.0 as f32).clamp(16.0, 28.0);
        let mention = cocoon_mention_link();
        let domain = cocoon_domain_link();
        let direct = cocoon_direct_link();

        ui.add_space(16.0);

        let private_text = build_linked_phrase(
            &tr::lng_translate_cocoon_private_text(&mention.text),
            &mention.text,
            &mention.link,
        );
        if let Some(url) = paint_feature_row(
            ui,
            box_w,
            row_pad_x,
            "cocoon_feat_private",
            st::menu_icon_lock,
            &tr::lng_translate_cocoon_private_title(),
            private_text,
            colors,
        ) {
            self.last_clicked_link = Some(url.clone());
            action = Some(AboutCocoonAction::OpenLink(url));
        }

        ui.add_space(14.0);

        let efficient_text = parse_bold_rich_text(&tr::lng_translate_cocoon_efficient_text());
        if let Some(url) = paint_feature_row(
            ui,
            box_w,
            row_pad_x,
            "cocoon_feat_efficient",
            st::menu_icon_stats,
            &tr::lng_translate_cocoon_efficient_title(),
            efficient_text,
            colors,
        ) {
            self.last_clicked_link = Some(url.clone());
            action = Some(AboutCocoonAction::OpenLink(url));
        }

        ui.add_space(14.0);

        let everyone_text = build_linked_phrase(
            &tr::lng_translate_cocoon_everyone_text(&domain.text),
            &domain.text,
            &domain.link,
        );
        if let Some(url) = paint_feature_row(
            ui,
            box_w,
            row_pad_x,
            "cocoon_feat_everyone",
            st::menu_icon_gift_premium,
            &tr::lng_translate_cocoon_everyone_title(),
            everyone_text,
            colors,
        ) {
            self.last_clicked_link = Some(url.clone());
            action = Some(AboutCocoonAction::OpenLink(url));
        }

        // 3. Separator (`cocoonJoinSeparatorPadding`: margins(24px, 4px, 24px, 12px)).
        let sep_pad = st::cocoon_join_separator_padding;
        ui.add_space((sep_pad.top.0 as f32).max(12.0));
        let sep_left = (sep_pad.left.0 as f32).max(row_pad_x);
        let sep_right = (sep_pad.right.0 as f32).max(row_pad_x);
        let (sep_rect, _) = ui.allocate_exact_size(Vec2::new(box_w, 1.0), Sense::hover());
        ui.painter().line_segment(
            [
                Pos2::new(sep_rect.left() + sep_left, sep_rect.center().y),
                Pos2::new(sep_rect.right() - sep_right, sep_rect.center().y),
            ],
            Stroke::new(1.0, colors.color("shadowFg")),
        );
        ui.add_space((sep_pad.bottom.0 as f32).max(10.0));

        // 4. Footer callout (`cocoonJoinLabel`, 12px centered `windowSubTextFg` with `t.me/cocoon?direct`).
        let footer_text = build_linked_phrase(
            &tr::lng_translate_cocoon_text(&direct.text),
            &direct.text,
            &direct.link,
        );
        let mut footer_style = FlatLabelStyle::sub_text(colors);
        footer_style.style.font.size = 12.0;
        footer_style.style.line_height = 17.0;
        let inner_w = (box_w - 2.0 * row_pad_x).max(120.0);
        ui.horizontal(|ui| {
            ui.add_space(row_pad_x);
            ui.allocate_ui_with_layout(
                Vec2::new(inner_w, 0.0),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    let resp = FlatLabel::rich(footer_text, footer_style)
                        .id_salt("cocoon_join_label")
                        .max_width(inner_w)
                        .show(ui);
                    if let Some(url) = resp.clicked_link {
                        self.last_clicked_link = Some(url.clone());
                        action = Some(AboutCocoonAction::OpenLink(url));
                    }
                },
            );
        });

        // 5. Full-width primary CTA button (`stakeBox`: height 42px, padding 24/12/24/24).
        if include_cta {
            let btn_pad = st::stake_box.button_padding;
            let btn_h = (st::stake_box.button_height.0 as f32).clamp(38.0, 46.0);
            ui.add_space((btn_pad.top.0 as f32).max(12.0));
            let done_label = tr::lng_translate_cocoon_done();
            let pad_x = (btn_pad.left.0 as f32).clamp(16.0, 24.0);
            let btn_w = (box_w - 2.0 * pad_x).max(120.0);
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(btn_w, btn_h), Sense::click());
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &done_label));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let bg = if resp.is_pointer_button_down_on() || resp.hovered() {
                    colors.color("activeButtonBgOver")
                } else {
                    colors.color("activeButtonBg")
                };
                let fg = colors.color("activeButtonFg");
                let cr = CornerRadius::same(8);
                ui.painter().rect_filled(rect, cr, bg);
                if resp.has_focus() {
                    ui.painter().rect_stroke(
                        rect.expand(1.5),
                        CornerRadius::same(9),
                        Stroke::new(2.0, colors.color("windowActiveTextFg")),
                        egui::StrokeKind::Outside,
                    );
                }
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    &done_label,
                    FontId::new(14.0, family_semibold()),
                    fg,
                );
                if resp.clicked() {
                    action = Some(AboutCocoonAction::Close);
                }
            });
            ui.add_space((btn_pad.bottom.0 as f32).clamp(16.0, 24.0));
        }

        action
    }
}

/// Paints the dark cosmic cover (`AddCocoonBoxCover` + `AddUniqueCloseButton`).
/// Returns `true` if the user clicked the top-right close button.
fn paint_cocoon_cover(
    ui: &mut Ui,
    box_w: f32,
    box_radius: u8,
    colors: &dyn Colors,
    reduced_motion: bool,
) -> bool {
    let logo_top = (st::cocoon_logo_top.0 as f32).clamp(24.0, 36.0);
    let logo_size = (st::cocoon_logo_size.0 as f32).clamp(72.0, 96.0);
    let title_top_gap = (st::cocoon_title_top.0 as f32).clamp(10.0, 14.0);
    let title_size = (st::cocoon_title_font.size.0 as f32).clamp(28.0, 36.0);
    let sub_top_gap = (st::cocoon_subtitle_top.0 as f32).clamp(6.0, 10.0);
    let sub_bottom = (st::cocoon_subtitle_bottom.0 as f32).clamp(14.0, 20.0);
    let pad_x = (st::box_row_padding.left.0 as f32).clamp(16.0, 24.0);
    let text_w = (box_w - 2.0 * pad_x).max(160.0);

    // Pre-lay out the subtitle to measure exact cover height.
    let explain = tr::lng_translate_cocoon_explain();
    let subtitle_raw = tr::lng_translate_cocoon_subtitle(&explain);
    let subtitle_job = build_cover_subtitle_job(&subtitle_raw, text_w);
    let subtitle_galley = ui.painter().layout_job(subtitle_job);
    let subtitle_h = subtitle_galley.size().y;

    let title_text = tr::lng_translate_cocoon_title();
    let title_tint = lerp_color32(COVER_ACCENT_LEFT, COVER_ACCENT_RIGHT, 0.35);
    let title_galley = ui.painter().layout_no_wrap(
        title_text,
        FontId::new(title_size, family_semibold()),
        title_tint,
    );
    let title_h = title_galley.size().y;

    let cover_h =
        logo_top + logo_size + title_top_gap + title_h + sub_top_gap + subtitle_h + sub_bottom;
    let (cover_rect, _) = ui.allocate_exact_size(Vec2::new(box_w, cover_h), Sense::hover());

    let top_cr = CornerRadius {
        nw: box_radius,
        ne: box_radius,
        sw: 0,
        se: 0,
    };
    let painter = ui.painter().with_clip_rect(cover_rect);

    // Base deep-navy fill + soft radial highlight matching `CreateTopBgGradient(gradientCenter, gradientEdge)`.
    painter.rect_filled(cover_rect, top_cr, COVER_GRADIENT_EDGE);
    let logo_center = Pos2::new(
        cover_rect.center().x,
        cover_rect.top() + logo_top + logo_size * 0.5,
    );
    for (radius_mul, alpha) in [(2.6_f32, 34_u8), (1.9_f32, 58_u8), (1.3_f32, 95_u8)] {
        painter.circle_filled(
            logo_center,
            logo_size * radius_mul,
            Color32::from_rgba_unmultiplied(
                COVER_GRADIENT_CENTER.r(),
                COVER_GRADIENT_CENTER.g(),
                COVER_GRADIENT_CENTER.b(),
                alpha,
            ),
        );
    }

    // Subtle radial mini-star particles (`StarParticles::Type::RadialInside`).
    // Strictly static/suppressed under reduced motion so snapshots and low-power mode never flicker.
    let particles_radius = logo_size * 0.5 + logo_top * 1.1;
    let now_secs = if reduced_motion {
        0.0
    } else {
        ui.ctx().request_repaint();
        ui.ctx().input(|i| i.time)
    };
    paint_cocoon_star_particles(
        &painter,
        logo_center,
        particles_radius,
        now_secs,
        reduced_motion,
    );

    // Neutral lock emblem from existing `tg-style` icon set (no Telegram/Cocoon logo artwork).
    let badge_radius = logo_size * 0.42;
    painter.circle_filled(
        logo_center,
        badge_radius + 7.0,
        Color32::from_rgba_unmultiplied(
            COVER_ACCENT_LEFT.r(),
            COVER_ACCENT_LEFT.g(),
            COVER_ACCENT_LEFT.b(),
            26,
        ),
    );
    painter.circle_filled(
        logo_center,
        badge_radius + 3.0,
        Color32::from_rgba_unmultiplied(
            COVER_ACCENT_RIGHT.r(),
            COVER_ACCENT_RIGHT.g(),
            COVER_ACCENT_RIGHT.b(),
            40,
        ),
    );
    // Concentric layered cyan-violet emblem badge (clean symmetry without trademark logo art).
    let badge_mid = lerp_color32(COVER_ACCENT_LEFT, COVER_ACCENT_RIGHT, 0.42);
    let badge_inner = lerp_color32(COVER_ACCENT_LEFT, COVER_ACCENT_RIGHT, 0.62);
    painter.circle_filled(logo_center, badge_radius, badge_mid);
    painter.circle_filled(
        logo_center,
        badge_radius * 0.82,
        Color32::from_rgba_unmultiplied(badge_inner.r(), badge_inner.g(), badge_inner.b(), 210),
    );
    painter.circle_stroke(
        logo_center,
        badge_radius,
        Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 255, 255, 130)),
    );
    let emblem_rect = Rect::from_center_size(logo_center, Vec2::splat(logo_size * 0.42));
    paint_style_icon(
        &painter,
        emblem_rect,
        st::menu_icon_lock,
        &ColorsResolver(colors),
        Some(COVER_BOLD_FG),
    );

    // Centered "Cocoon" title tinted with midpoint cyan-violet gradient accent.
    let title_y = cover_rect.top() + logo_top + logo_size + title_top_gap;
    let title_pos = Pos2::new(cover_rect.center().x - title_galley.size().x * 0.5, title_y);
    painter.galley(title_pos, title_galley, title_tint);

    // Centered rich subtitle (`halign: Align::Center` centers around `pos.x`, so pass `cover_rect.center().x`).
    let sub_y = title_y + title_h + sub_top_gap;
    let sub_pos = Pos2::new(cover_rect.center().x, sub_y);
    painter.galley(sub_pos, subtitle_galley, COVER_SUBTITLE_FG);

    // Top-right close button (`AddUniqueCloseButton` / `uniqueCloseButton`).
    let close_size = 36.0;
    let close_rect = Rect::from_min_size(
        Pos2::new(
            cover_rect.right() - close_size - 6.0,
            cover_rect.top() + 6.0,
        ),
        Vec2::splat(close_size),
    );
    let close_resp = ui.interact(
        close_rect,
        Id::unique("tg_about_cocoon_close_btn"),
        Sense::click(),
    );
    let close_label = tr::lng_close();
    close_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &close_label));
    if close_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        painter.circle_filled(
            close_rect.center(),
            16.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 28),
        );
    }
    if close_resp.has_focus() {
        painter.circle_stroke(
            close_rect.center(),
            16.0,
            Stroke::new(1.5, COVER_ACCENT_LEFT),
        );
    }
    let close_icon_rect = Rect::from_center_size(close_rect.center(), Vec2::splat(16.0));
    paint_style_icon(
        &painter,
        close_icon_rect,
        st::box_title_close_icon,
        &ColorsResolver(colors),
        Some(COVER_BOLD_FG),
    );

    close_resp.clicked()
}

/// Paints deterministic radial 4-point mini-star particles (`StarParticles::Type::RadialInside`).
fn paint_cocoon_star_particles(
    painter: &egui::Painter,
    center: Pos2,
    max_radius: f32,
    now_secs: f64,
    reduced_motion: bool,
) {
    const GOLDEN_ANGLE_RAD: f32 = 2.399_963_2;
    let min_radius = max_radius * 0.54;
    let count = if reduced_motion {
        22
    } else {
        COVER_PARTICLE_COUNT
    };
    for idx in 0..count {
        // SplitMix64 mixing prevents linear correlation between angle, phase, and radius.
        let mut z = (idx as u64)
            .wrapping_add(1)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;

        let phase_frac = ((z >> 16) & 0xFFFF) as f64 / 65535.0;
        let color_t = ((z >> 32) & 0xFF) as f32 / 255.0;
        let size_frac = (z & 0xFF) as f32 / 255.0;

        let t = if reduced_motion {
            0.18 + (phase_frac as f32) * 0.68
        } else {
            ((phase_frac + now_secs * COVER_PARTICLE_SPEED * 2.2) % 1.0) as f32
        };
        let r = min_radius + (max_radius - min_radius) * t;
        let angle = (idx as f32) * GOLDEN_ANGLE_RAD + 0.35;
        let pos = Pos2::new(center.x + angle.cos() * r, center.y + angle.sin() * r);

        // Smooth fade in and fade out along radial travel.
        let alpha_curve = (t * std::f32::consts::PI).sin().clamp(0.0, 1.0);
        let max_alpha = if reduced_motion { 125.0 } else { 190.0 };
        let alpha = (alpha_curve * max_alpha).round() as u8;
        if alpha < 8 {
            continue;
        }
        let base = lerp_color32(COVER_ACCENT_LEFT, COVER_ACCENT_RIGHT, color_t);
        let color = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha);
        let star_r = 2.0 + size_frac * 2.8;
        paint_four_point_star(painter, pos, star_r, color);
    }
}

fn paint_four_point_star(painter: &egui::Painter, center: Pos2, r: f32, color: Color32) {
    let half = r * 0.32;
    // Two intersecting convex diamonds form a crisp 4-point star without concave triangulation artifacts.
    let vert = vec![
        Pos2::new(center.x, center.y - r),
        Pos2::new(center.x + half, center.y),
        Pos2::new(center.x, center.y + r),
        Pos2::new(center.x - half, center.y),
    ];
    let horiz = vec![
        Pos2::new(center.x - r, center.y),
        Pos2::new(center.x, center.y - half),
        Pos2::new(center.x + r, center.y),
        Pos2::new(center.x, center.y + half),
    ];
    painter.add(Shape::convex_polygon(vert, color, Stroke::NONE));
    painter.add(Shape::convex_polygon(horiz, color, Stroke::NONE));
}

#[allow(clippy::too_many_arguments)]
fn paint_feature_row(
    ui: &mut Ui,
    box_w: f32,
    pad_x: f32,
    id_salt: &str,
    icon: tg_ui::tg_style::Icon,
    title: &str,
    body: RichText,
    colors: &dyn Colors,
) -> Option<String> {
    let icon_col_w = 34.0;
    let text_w = (box_w - 2.0 * pad_x - icon_col_w).max(120.0);
    let mut clicked = None;
    let accent = colors.color("windowActiveTextFg");
    let bold_fg = colors.color("windowBoldFg");
    let mut body_style = FlatLabelStyle::sub_text(colors);
    body_style.style.font.size = 13.0;
    body_style.style.line_height = 18.0;

    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        ui.allocate_ui_with_layout(
            Vec2::new(icon_col_w, 28.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add_space(2.0);
                let (ic_rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                paint_style_icon(
                    ui.painter(),
                    ic_rect,
                    icon,
                    &ColorsResolver(colors),
                    Some(accent),
                );
            },
        );
        ui.vertical(|ui| {
            ui.set_min_width(text_w);
            ui.set_max_width(text_w);
            ui.label(
                egui::RichText::new(title)
                    .font(FontId::new(14.0, family_semibold()))
                    .color(bold_fg),
            );
            ui.add_space(2.0);
            let resp = FlatLabel::rich(body, body_style)
                .id_salt(id_salt)
                .max_width(text_w)
                .show(ui);
            if resp.clicked_link.is_some() {
                clicked = resp.clicked_link;
            }
        });
    });

    clicked
}

/// Parses `**bold**` markers into a centered `egui::text::LayoutJob` for the dark cover subtitle.
fn build_cover_subtitle_job(raw: &str, max_w: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: max_w,
            ..Default::default()
        },
        halign: egui::Align::Center,
        ..Default::default()
    };
    let regular = egui::text::TextFormat {
        font_id: FontId::new(13.5, family_regular()),
        color: COVER_SUBTITLE_FG,
        line_height: Some(19.0),
        ..Default::default()
    };
    let bold = egui::text::TextFormat {
        font_id: FontId::new(13.5, family_semibold()),
        color: COVER_BOLD_FG,
        line_height: Some(19.0),
        ..Default::default()
    };
    let mut rest = raw;
    let mut is_bold = false;
    while let Some(pos) = rest.find("**") {
        let seg = &rest[..pos];
        if !seg.is_empty() {
            job.append(
                seg,
                0.0,
                if is_bold {
                    bold.clone()
                } else {
                    regular.clone()
                },
            );
        }
        is_bold = !is_bold;
        rest = &rest[pos + 2..];
    }
    if !rest.is_empty() {
        job.append(rest, 0.0, if is_bold { bold } else { regular });
    }
    job
}

/// Converts a phrase containing `**bold**` markers into [`RichText`].
#[must_use]
pub fn parse_bold_rich_text(raw: &str) -> RichText {
    let mut out = RichText::new();
    let mut rest = raw;
    let mut is_bold = false;
    while let Some(pos) = rest.find("**") {
        let seg = &rest[..pos];
        if !seg.is_empty() {
            out = if is_bold {
                out.push_bold(seg)
            } else {
                out.push_plain(seg)
            };
        }
        is_bold = !is_bold;
        rest = &rest[pos + 2..];
    }
    if !rest.is_empty() {
        out = if is_bold {
            out.push_bold(rest)
        } else {
            out.push_plain(rest)
        };
    }
    out
}

/// Builds a [`RichText`] turning the first occurrence of `link_label` inside `phrase` into a
/// clickable link targeting `link_url`.
#[must_use]
pub fn build_linked_phrase(phrase: &str, link_label: &str, link_url: &str) -> RichText {
    if !link_label.is_empty()
        && let Some(pos) = phrase.find(link_label)
    {
        let before = &phrase[..pos];
        let after = &phrase[pos + link_label.len()..];
        let mut rt = parse_bold_rich_text(before);
        rt = rt.push_link(link_label, link_url);
        let tail = parse_bold_rich_text(after);
        let base_len = rt.text.len();
        rt.text.push_str(&tail.text);
        for mut sp in tail.spans {
            sp.start += base_len;
            sp.end += base_len;
            rt.spans.push(sp);
        }
        return rt;
    }
    parse_bold_rich_text(phrase)
}

fn lerp_color32(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix =
        |x: u8, y: u8| -> u8 { (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8 };
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}
