//! `tg-ui`: Telegram Desktop-styled `egui` widget toolkit.
//!
//! Port of `Telegram/lib_ui/ui/` (widgets, layers, effects, text rendering,
//! painter/round-rect/shadows, toast, custom Windows title bar) and userpic /
//! unread badge rendering from `Telegram/SourceFiles/ui/`.
//!
//! Backed directly by `tg-style` (generated `.style` values, DPI scale, icons),
//! `tg-theme` (palettes, embedded day/night themes, accent colorizer, wallpapers),
//! and `tg-text` (`TextWithEntities`, UTF-16 entity offsets).

pub use eframe;
pub use egui;
pub use tg_style;
pub use tg_text;
pub use tg_theme;

pub mod anim;
pub mod clipboard;
pub mod colored;
pub mod custom_emoji;
pub mod effects;
pub mod elide;
pub mod entity_bounds;
pub mod entity_link;
pub mod fonts;
pub mod icons;
pub mod layers;
pub mod painter;
pub mod peer_color;
pub mod platform;
#[cfg(any(test, feature = "soft-raster"))]
pub mod soft_raster;
pub mod spoiler_line;
pub mod stand_in;
pub mod sticker_thumbs;
pub mod style;
pub mod text;
pub mod theme;
pub mod toast;
pub mod unread_badge;
pub mod userpic;
pub mod userpic_images;
pub mod wheel;
pub mod widgets;

/// Apply Telegram Desktop visuals and bundled Open Sans / Hack fonts to an `egui::Context`.
///
/// Automatically detects light vs dark palette from `windowBg` contrast.
pub fn apply_tdesktop_visuals(ctx: &egui::Context, colors: &dyn theme::Colors) {
    theme::apply_palette_visuals(ctx, colors);
}

#[cfg(test)]
mod text_weight_tests;

#[cfg(test)]
mod custom_emoji_tests;

#[cfg(test)]
mod custom_emoji_lru_tests;

#[cfg(test)]
mod tests {
    use egui::{Color32, Pos2, Rect};
    use tg_style::{ColorRef, Scale, st};
    use tg_text::{EntityInText, EntityType, TextSelection, TextWithEntities, utf16_len};
    use tg_theme::{EmbeddedType, Palette, default_accent_colors, prepare_embedded_palette};

    use crate::anim::{
        ContinuousValue, SimpleAnimation, Transition, bumpy, color_lerp, contrast_ratio,
        ease_in_circ, ease_in_cubic, ease_in_quint, ease_out_back, ease_out_circ, ease_out_cubic,
        ease_out_quint, half_sine, interpolate_i32, linear, premultiplied_argb, sine_in_out,
        with_alpha,
    };
    use crate::effects::{
        InfiniteRadialAnimation, RippleMask, RippleState, clamp_point_to_round_rect,
    };
    use crate::fonts::{
        build_tdesktop_font_definitions, embedded_font_bytes, family_italic, family_monospace,
        family_regular, family_semibold, family_semibold_italic, register_tdesktop_fonts,
    };
    use crate::icons::{IconCacheKey, IconTextureCache, rasterize_icon_to_color_image};
    use crate::painter::{BoxShadow, RoundRectCache, image_prepare};
    use crate::style::{
        BoxShadowStyle, InfiniteRadialAnimationStyle, Point, RippleAnimationStyle,
        RoundButtonStyle, TextPalette, TextStyle, ToggleStyle, UnreadBadgeStyle,
    };
    use crate::text::{
        RichBlockKind, RichText, RichTextOptions, TextEntityKind, build_layout_job,
        layout_text_with_entities,
    };
    use crate::theme::{Colors, DayPalette, ThemeManager, parse_hex_color};
    use crate::unread_badge::{format_unread_badge_text, unread_badge_size_for_text_width};
    use crate::userpic::{
        color_index_to_palette_index, decide_color_index, extract_initials, userpic_colors,
    };
    use crate::widgets::buttons::compute_round_button_width;
    use crate::widgets::checkbox::Toggle;
    use crate::widgets::input_field::estimate_wrapped_lines;
    use crate::widgets::scroll_area::{compute_scroll_to, compute_scrollbar_thumb};

    #[test]
    fn animation_curves_endpoints_and_known_values() {
        let eps = 1e-6;
        let transitions = [
            Transition::Linear,
            Transition::SineInOut,
            Transition::HalfSine,
            Transition::EaseOutBack,
            Transition::EaseInCirc,
            Transition::EaseOutCirc,
            Transition::EaseInCubic,
            Transition::EaseOutCubic,
            Transition::EaseInQuint,
            Transition::EaseOutQuint,
            Transition::Bumpy(1.25),
        ];
        for tr in transitions {
            assert!(
                tr.eval(100.0, 0.0).abs() < 1e-4,
                "t=0 failed for {tr:?}: {}",
                tr.eval(100.0, 0.0)
            );
            assert!(
                (tr.eval(100.0, 1.0) - 100.0).abs() < 1e-4,
                "t=1 failed for {tr:?}: {}",
                tr.eval(100.0, 1.0)
            );
        }

        assert!((linear(10.0, 0.5) - 5.0).abs() < eps);
        assert!((sine_in_out(10.0, 0.5) - 5.0).abs() < eps);
        assert!((half_sine(1.0, 0.5) - std::f64::consts::FRAC_1_SQRT_2).abs() < eps);
        assert!((ease_out_circ(1.0, 0.5) - (0.75_f64).sqrt()).abs() < eps);
        assert!((ease_in_circ(1.0, 0.5) - (1.0 - (0.75_f64).sqrt())).abs() < eps);
        assert!((ease_in_cubic(8.0, 0.5) - 1.0).abs() < eps);
        assert!((ease_out_cubic(8.0, 0.5) - 7.0).abs() < eps);
        assert!((ease_in_quint(32.0, 0.5) - 1.0).abs() < eps);
        assert!((ease_out_quint(32.0, 0.5) - 31.0).abs() < eps);
        assert!(ease_out_back(1.0, 0.5) > 1.0);
        assert!(bumpy(1.25, 1.0, 0.4) > 1.0);
    }

    #[test]
    fn simple_animation_and_continuous_value_progression() {
        let mut anim = SimpleAnimation::new();
        anim.start(10.0, 0.0, 100.0, 200, Transition::SineInOut);
        assert!(anim.animating());
        assert!((anim.sample(10.0, 100.0) - 0.0).abs() < 1e-6);
        assert!((anim.sample(10.1, 100.0) - 50.0).abs() < 1e-6);
        assert!((anim.sample(10.25, 100.0) - 100.0).abs() < 1e-6);
        assert!(!anim.animating());

        anim.start(20.0, 0.0, 100.0, 200, Transition::Linear);
        let mid = anim.sample(20.05, 100.0);
        assert!((mid - 25.0).abs() < 1e-6);
        anim.change(20.05, 0.0, 100, Transition::Linear);
        assert!((anim.sample(20.10, 0.0) - 12.5).abs() < 1e-6);
        assert!((anim.sample(20.16, 0.0) - 0.0).abs() < 1e-6);

        let mut cv = ContinuousValue::new(100.0);
        cv.start_default(50.0);
        assert!((cv.update(50.0) - 25.0).abs() < 1e-6);
        assert!((cv.update(80.0) - 50.0).abs() < 1e-6);
    }

    #[test]
    fn color_math_lerp_alpha_hex_and_contrast() {
        let black = Color32::from_rgb(0, 0, 0);
        let white = Color32::from_rgb(255, 255, 255);
        assert_eq!(color_lerp(black, white, 0.0), black);
        assert_eq!(color_lerp(black, white, 1.0), white);
        let mid = color_lerp(black, white, 0.5);
        assert!((i32::from(mid.r()) - 128).abs() <= 1);
        assert!((i32::from(mid.g()) - 128).abs() <= 1);
        assert!((i32::from(mid.b()) - 128).abs() <= 1);

        let half = with_alpha(Color32::from_rgb(0x40, 0xa7, 0xe3), 0.5);
        assert_eq!(half.to_srgba_unmultiplied()[3], 128);

        assert_eq!(
            parse_hex_color("#40a7e3"),
            Some(Color32::from_rgb(0x40, 0xa7, 0xe3))
        );
        assert_eq!(
            parse_hex_color("#0000007f"),
            Some(Color32::from_rgba_unmultiplied(0, 0, 0, 0x7f))
        );
        assert_eq!(parse_hex_color("invalid"), None);

        assert_eq!(
            premultiplied_argb(Color32::from_rgb(255, 0, 0)),
            0xFFFF_0000
        );
        assert!((contrast_ratio(black, white) - 21.0).abs() < 1e-3);
        assert_eq!(interpolate_i32(0, 10, 0.5), 5);
        assert_eq!(interpolate_i32(10, 20, 0.25), 13);
    }

    #[test]
    fn userpic_color_index_mapping_and_initials() {
        assert_eq!(decide_color_index(0), 0);
        assert_eq!(decide_color_index(7), 0);
        assert_eq!(decide_color_index(123_456), (123_456 % 7) as u8);

        let expected_palette = [0u8, 7, 4, 1, 6, 3, 5];
        for (color_idx, &pal_idx) in expected_palette.iter().enumerate() {
            assert_eq!(color_index_to_palette_index(color_idx as u8), pal_idx);
            assert_eq!(color_index_to_palette_index(color_idx as u8 + 7), pal_idx);
        }

        let day = Palette::default();
        let c0 = userpic_colors(0, &day);
        assert_eq!(c0.color1, Color32::from_rgb(0xff, 0x84, 0x5e));
        assert_eq!(c0.color2, Color32::from_rgb(0xd4, 0x52, 0x46));

        let c5 = userpic_colors(5, &day);
        assert_eq!(c5.color1, Color32::from_rgb(0x5c, 0xaf, 0xfa));
        assert_eq!(c5.color2, Color32::from_rgb(0x40, 0x8a, 0xcf));

        assert_eq!(extract_initials("Alex Doe"), "AD");
        assert_eq!(extract_initials("Rustgram"), "R");
        assert_eq!(extract_initials("Jean-Luc Picard"), "JP");
        assert_eq!(extract_initials("Anne-Marie"), "AM");
        assert_eq!(extract_initials("  Alice   Bob   Carol "), "AC");
        assert_eq!(extract_initials(""), "");
    }

    #[test]
    fn layout_math_buttons_scroll_badges_shadow_and_images() {
        let day = DayPalette::new();

        let btn_st = RoundButtonStyle::default_active(&day);
        assert_eq!(btn_st.width, -34.0);
        assert_eq!(compute_round_button_width(66.0, &btn_st, None), 100.0);
        assert_eq!(
            compute_round_button_width(66.0, &btn_st, Some(200.0)),
            200.0
        );

        assert_eq!(compute_scroll_to(50, 80, 0, 500, 0, 200), 0);
        assert_eq!(compute_scroll_to(220, 260, 0, 500, 0, 200), 60);
        assert_eq!(compute_scroll_to(40, 80, 0, 500, 100, 200), 40);
        assert_eq!(compute_scroll_to(120, -1, 0, 500, 0, 200), 120);

        assert_eq!(
            compute_scrollbar_thumb(200.0, 180.0, 0.0, 194.0, 20.0),
            None
        );
        let (ty, th) = compute_scrollbar_thumb(200.0, 1000.0, 400.0, 200.0, 20.0).unwrap();
        assert!((th - 40.0).abs() < 1e-3);
        assert!((ty - 80.0).abs() < 1e-3);

        assert_eq!(format_unread_badge_text("42", 0), "42");
        assert_eq!(format_unread_badge_text("12345", 2), "..45");
        assert_eq!(format_unread_badge_text("123", 2), "123");
        let ub = UnreadBadgeStyle::default();
        assert_eq!(unread_badge_size_for_text_width(5.0, &ub).x, ub.size);
        assert_eq!(unread_badge_size_for_text_width(24.0, &ub).x, 34.0);

        let ext = BoxShadow::extend_for(&BoxShadowStyle {
            blur_radius: 5.0,
            offset: Point::new(0.0, 1.0),
            opacity: 0.25,
        });
        assert_eq!(
            (ext.left, ext.top, ext.right, ext.bottom),
            (5.0, 4.0, 5.0, 6.0)
        );

        let mut cache = RoundRectCache::new();
        let _ = cache.get(8.0, Color32::WHITE);
        let _ = cache.get(8.0, Color32::WHITE);
        let _ = cache.get(4.0, Color32::WHITE);
        assert_eq!(cache.len(), 2);

        let r = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 40.0));
        let clamped = clamp_point_to_round_rect(Pos2::new(0.0, 0.0), r, 10.0);
        assert!(clamped.x > 0.0 && clamped.y > 0.0);
        let mut rip = RippleState::new();
        let rip_st = RippleAnimationStyle::default_ripple(&day);
        rip.add(egui::vec2(50.0, 20.0), 0.0, r.size(), &rip_st, 0.0);
        assert!(!rip.is_empty());
        rip.last_stop(&rip_st, 0.1);
        assert_eq!(RippleMask::RoundRect(4.0), RippleMask::RoundRect(4.0));

        let circle = image_prepare::vertical_gradient_circle(
            32,
            Color32::from_rgb(0x5c, 0xaf, 0xfa),
            Color32::from_rgb(0x40, 0x8a, 0xcf),
        );
        assert_eq!(circle.pixels[0].a(), 0);
        assert_eq!(circle.pixels[16 * 32 + 16].a(), 255);

        assert_eq!(estimate_wrapped_lines("a\nb\nc", 200.0, 14.0), 3);
        let rich = RichText::new()
            .push_bold("Bold ")
            .push_italic("Italic ")
            .push_code("code")
            .push_plain(" ")
            .push_link("rustgram", "https://example.org")
            .push_plain(" ")
            .push_spoiler("secret");
        assert_eq!(rich.spans.len(), 5);
        assert_eq!(rich.spans[4].kind, TextEntityKind::Spoiler);
        let job = build_layout_job(
            &rich,
            &TextStyle::default_style(),
            &TextPalette::default_palette(&day),
            Color32::BLACK,
            300.0,
            None,
            false,
        );
        assert_eq!(job.text, rich.text);
    }

    #[test]
    fn palette_resolution_through_tg_style_and_theme_manager() {
        let mut tm = ThemeManager::new();
        assert!(!tm.is_dark());
        assert_eq!(tm.color("windowBg"), Color32::WHITE);
        assert_eq!(
            tm.color("windowBgActive"),
            Color32::from_rgb(0x40, 0xa7, 0xe3)
        );
        assert_eq!(
            tm.color("titleButtonCloseBgOver"),
            Color32::from_rgb(0xe8, 0x11, 0x23)
        );
        assert!(tm.get("nonexistentColorKey").is_none());

        // ColorRef resolution through tg-style ColorResolver
        assert_eq!(
            ColorRef::new("windowBg").resolve(&tm),
            tg_style::Color::WHITE
        );
        assert_eq!(
            st::default_active_button.text_bg.resolve(&tm),
            tg_style::Color::rgb(0x40, 0xa7, 0xe3)
        );

        // Switching to Night theme updates resolved colors and bumps generation.
        let gen0 = tm.generation();
        tm.set_embedded(EmbeddedType::Night, None);
        assert!(tm.is_dark());
        assert!(tm.generation() > gen0);
        assert_ne!(tm.color("windowBg"), Color32::WHITE);
        let night_btn = RoundButtonStyle::default_active(&tm);
        assert_ne!(night_btn.text_bg, Color32::from_rgb(0x40, 0xa7, 0xe3));

        // Applying accent color updates activeButtonBg and bumps generation again.
        let gen1 = tm.generation();
        let accents = default_accent_colors(EmbeddedType::Night);
        tm.set_accent(Some(accents[1]));
        assert!(tm.generation() > gen1);
        let accented_btn = RoundButtonStyle::default_active(&tm);
        assert_ne!(accented_btn.text_bg, night_btn.text_bg);

        // DayPalette compatibility wrapper also works with overrides.
        let custom = DayPalette::new().with_override("windowBg", Color32::BLACK);
        assert_eq!(custom.color("windowBg"), Color32::BLACK);

        // Toggle & radial styles read from real tg_style::st::*.
        let tog_st = ToggleStyle::default_toggle(&tm);
        assert_eq!(Toggle::toggle_size(&tog_st), egui::vec2(34.0, 20.0));
        let rad_st = InfiniteRadialAnimationStyle::default_radial(&tm);
        let rad = InfiniteRadialAnimation::new(rad_st);
        for ms in (0..=3000).step_by(100) {
            let st_val = rad.compute_state(ms as f64 / 1000.0);
            assert!(st_val.arc_length_rad >= rad_st.arc_min * std::f64::consts::TAU - 1e-4);
            assert!(st_val.arc_length_rad <= rad_st.arc_max * std::f64::consts::TAU + 1e-4);
        }
    }

    #[test]
    fn icon_texture_cache_keys_and_rasterization() {
        let icon_a = st::box_title_close_icon;
        let icon_b = st::back_button_icon;

        // Distinct icons, scales, DPRs, colors, and palette generations produce distinct keys.
        let k1 = IconCacheKey::for_palette(icon_a, Scale::S100, 1, 1);
        let k2 = IconCacheKey::for_palette(icon_a, Scale::S100, 1, 1);
        let k_other_icon = IconCacheKey::for_palette(icon_b, Scale::S100, 1, 1);
        let k_other_scale = IconCacheKey::for_palette(icon_a, Scale::S150, 1, 1);
        let k_other_dpr = IconCacheKey::for_palette(icon_a, Scale::S100, 2, 1);
        let k_other_gen = IconCacheKey::for_palette(icon_a, Scale::S100, 1, 2);
        let k_color_red = IconCacheKey::with_color(icon_a, Scale::S100, 1, Color32::RED);
        let k_color_blue = IconCacheKey::with_color(icon_a, Scale::S100, 1, Color32::BLUE);

        assert_eq!(k1, k2);
        assert_ne!(k1, k_other_icon);
        assert_ne!(k1, k_other_scale);
        assert_ne!(k1, k_other_dpr);
        assert_ne!(k1, k_other_gen);
        assert_ne!(k1, k_color_red);
        assert_ne!(k_color_red, k_color_blue);
        // Explicit color override normalizes palette_generation to 0 so theme switches keep it.
        assert_eq!(k_color_red.palette_generation, 0);

        // Rasterize real tg_style icon with both day and night palettes.
        let day = Palette::default();
        let night = prepare_embedded_palette(EmbeddedType::Night, None).unwrap();
        let (img_day, sz_day) =
            rasterize_icon_to_color_image(icon_a, &day, Scale::S100, 1, None).unwrap();
        let (img_night, sz_night) =
            rasterize_icon_to_color_image(icon_a, &night, Scale::S100, 2, None).unwrap();
        assert!(img_day.size[0] > 0 && img_day.size[1] > 0);
        assert_eq!(img_night.size[0], img_day.size[0] * 2);
        assert_eq!(sz_day, sz_night);

        // Cache invalidation clears textures and advances generation.
        let ctx = egui::Context::default();
        let mut cache = IconTextureCache::new();
        assert!(cache.is_empty());
        let _ = cache
            .get_or_rasterize(&ctx, icon_a, &day, Scale::S100, 1, None)
            .unwrap();
        assert_eq!(cache.len(), 1);
        let _ = cache
            .get_or_rasterize(&ctx, icon_a, &day, Scale::S100, 1, None)
            .unwrap();
        assert_eq!(cache.len(), 1);
        let _ = cache
            .get_or_rasterize(&ctx, icon_a, &day, Scale::S150, 1, None)
            .unwrap();
        assert_eq!(cache.len(), 2);

        let prev_gen = cache.palette_generation();
        cache.invalidate();
        assert!(cache.is_empty());
        assert_eq!(cache.palette_generation(), prev_gen + 1);
    }

    #[test]
    fn bundled_fonts_registered_with_all_faces_and_cyrillic() {
        for (name, bytes) in embedded_font_bytes() {
            assert!(bytes.len() > 10_000, "font {name} unexpectedly small");
        }
        let defs = build_tdesktop_font_definitions();
        for key in [
            "open_sans_regular",
            "open_sans_semibold",
            "open_sans_italic",
            "open_sans_semibold_italic",
            "hack_regular",
        ] {
            assert!(defs.font_data.contains_key(key), "missing font {key}");
        }
        assert!(defs.families.contains_key(&family_regular()));
        assert!(defs.families.contains_key(&family_semibold()));
        assert!(defs.families.contains_key(&family_italic()));
        assert!(defs.families.contains_key(&family_semibold_italic()));
        assert!(defs.families.contains_key(&family_monospace()));
        // Monospace must include OpenSans-Regular fallback for Cyrillic rendering.
        assert!(
            defs.families[&family_monospace()]
                .iter()
                .any(|f| f == "open_sans_regular")
        );

        // Lay out Cyrillic text in both Proportional and Monospace without missing-glyph panic.
        let ctx = egui::Context::default();
        register_tdesktop_fonts(&ctx);
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let g_prop = ui.painter().layout_no_wrap(
                "Привет, мир! Rustgram".to_owned(),
                egui::FontId::new(13.0, family_regular()),
                Color32::BLACK,
            );
            assert!(g_prop.size().x > 40.0);
            let g_mono = ui.painter().layout_no_wrap(
                "код_на_русском()".to_owned(),
                egui::FontId::new(13.0, family_monospace()),
                Color32::BLACK,
            );
            assert!(g_mono.size().x > 40.0);
        });
        out.textures_delta.clear();
    }

    #[test]
    fn text_with_entities_utf16_layout_elision_and_hit_testing() {
        let ctx = egui::Context::default();
        register_tdesktop_fonts(&ctx);
        let day = Palette::default();
        let text_st = TextStyle::history_text();
        let text_pal = TextPalette::default_palette(&day);

        // Text includes astral emoji (🎉 = 2 UTF-16 code units, 4 UTF-8 bytes) and Cyrillic
        // ("Привет") so UTF-16 -> UTF-8 offset conversion is genuinely exercised!
        let prefix = "🎉 Привет ";
        let link_str = "https://example.org";
        let mid1 = " ";
        let mention_str = "@alex_doe";
        let mid2 = " ";
        let tag_str = "#rust";
        let mid3 = " ";
        let spoiler_str = "секрет";
        let mid4 = " ";
        let emoji_str = "⭐";
        let sep = "\n";
        let quote_str = "Quoted block with left accent bar";
        let pre_str = "fn main() { println!(\"Привет\"); }";

        let full = format!(
            "{prefix}{link_str}{mid1}{mention_str}{mid2}{tag_str}{mid3}{spoiler_str}{mid4}{emoji_str}{sep}{quote_str}{sep}{pre_str}"
        );

        let u16_off = |byte_pos: usize| utf16_len(&full[..byte_pos]) as i32;
        let u16_l = |s: &str| utf16_len(s) as i32;

        let link_b = prefix.len();
        let mention_b = link_b + link_str.len() + mid1.len();
        let tag_b = mention_b + mention_str.len() + mid2.len();
        let spoiler_b = tag_b + tag_str.len() + mid3.len();
        let emoji_b = spoiler_b + spoiler_str.len() + mid4.len();
        let quote_b = emoji_b + emoji_str.len() + sep.len();
        let pre_b = quote_b + quote_str.len() + sep.len();

        let twe = TextWithEntities::with_entities(
            full.clone(),
            vec![
                EntityInText::new(EntityType::Bold, u16_off("🎉 ".len()), u16_l("Привет")),
                EntityInText::new(EntityType::Url, u16_off(link_b), u16_l(link_str)),
                EntityInText::new(EntityType::Mention, u16_off(mention_b), u16_l(mention_str)),
                EntityInText::new(EntityType::Hashtag, u16_off(tag_b), u16_l(tag_str)),
                EntityInText::new(EntityType::Spoiler, u16_off(spoiler_b), u16_l(spoiler_str)),
                EntityInText::with_data(
                    EntityType::CustomEmoji,
                    u16_off(emoji_b),
                    u16_l(emoji_str),
                    "5368324170671202286",
                ),
                EntityInText::new(EntityType::Blockquote, u16_off(quote_b), u16_l(quote_str)),
                EntityInText::with_data(EntityType::Pre, u16_off(pre_b), u16_l(pre_str), "rust"),
            ],
        );

        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let opts = RichTextOptions::new(Color32::BLACK, 380.0);
            let laid = layout_text_with_entities(ui.ctx(), &twe, &text_st, &text_pal, &opts);

            // Partitioned into 3 blocks: Normal, Blockquote, Pre.
            assert_eq!(laid.blocks.len(), 3);
            assert_eq!(laid.blocks[0].kind, RichBlockKind::Normal);
            assert_eq!(
                laid.blocks[1].kind,
                RichBlockKind::Blockquote { collapsed: false }
            );
            assert_eq!(
                laid.blocks[2].kind,
                RichBlockKind::Pre {
                    language: "rust".to_owned()
                }
            );
            assert_eq!(laid.pre_block_text(2), Some(pre_str));

            // Extracted 3 clickable links: Url, Mention, Hashtag.
            assert_eq!(laid.links.len(), 3);
            assert_eq!(laid.links[0].kind, EntityType::Url);
            assert_eq!(laid.links[0].target, link_str);
            assert_eq!(laid.links[1].kind, EntityType::Mention);
            assert_eq!(laid.links[1].target, mention_str);
            assert_eq!(laid.links[2].kind, EntityType::Hashtag);
            assert_eq!(
                laid.links[2].target,
                crate::entity_link::EntityLink::Hashtag(tag_str.to_owned()).target()
            );

            // Hit-test each link region center -> returns exact link!
            assert!(!laid.link_regions.is_empty());
            for region in &laid.link_regions {
                let hit = laid.hit_test(region.rect.center());
                assert_eq!(hit.link_index, Some(region.link_index));
                assert_eq!(hit.link.as_ref(), Some(&laid.links[region.link_index]));
            }

            // Hit-test spoiler region center -> in_spoiler = true!
            assert!(!laid.spoiler_rects.is_empty());
            let sp_hit = laid.hit_test(laid.spoiler_rects[0].center());
            assert!(sp_hit.in_spoiler);

            // Custom emoji placeholder captured with document ID & hit-testable!
            assert_eq!(laid.custom_emojis.len(), 1);
            assert_eq!(laid.custom_emojis[0].document_id, "5368324170671202286");
            let ce_hit = laid.hit_test(laid.custom_emojis[0].rect.center());
            assert_eq!(
                ce_hit.custom_emoji_id.as_deref(),
                Some("5368324170671202286")
            );

            // UTF-16 selection rects and selected_text match "Привет" after astral emoji!
            let sel = TextSelection::new(
                u16_off("🎉 ".len()) as u16,
                (u16_off("🎉 ".len()) + u16_l("Привет")) as u16,
            );
            assert_eq!(laid.selected_text(sel), "Привет");
            let sel_rects = laid.selection_rects(sel);
            assert!(!sel_rects.is_empty());

            // Pre block copy button hit-test.
            let copy_rect = laid.blocks[2].copy_rect.expect("pre block must have copy_rect");
            let copy_hit = laid.hit_test(copy_rect.center());
            assert_eq!(copy_hit.pre_copy_block, Some(2));

            // Max-lines elision test on long text.
            let long_text = TextWithEntities::simple(
                "Line one with plenty of words to wrap across multiple lines in a narrow container for elision testing.",
            );
            let elide_opts = RichTextOptions::new(Color32::BLACK, 120.0)
                .max_lines(2)
                .elide(true);
            let elided_laid =
                layout_text_with_entities(ui.ctx(), &long_text, &text_st, &text_pal, &elide_opts);
            assert!(elided_laid.elided);
            assert!(elided_laid.line_count <= 2);
        });
        out.textures_delta.clear();
    }
}
