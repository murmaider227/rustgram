//! Port of Telegram Desktop's `.style` system (build-time codegen + runtime scaling, fonts, colors, and icons).
//!
//! Port of `Telegram/codegen/codegen/style/` and `Telegram/lib_ui/ui/style/`.
//!
//! # Architecture
//! - **Build-time codegen (`build.rs`)**: Parses all 77 `.style` files from `lib_ui/ui/` and
//!   `SourceFiles/` plus `ui/colors.palette`, validates all referenced icon assets (`1x`/`@2x`/`@3x`
//!   PNGs and SVGs), packs the icons into a single embedded archive, and emits typed Rust structs
//!   and `static` variables.
//! - **Generated namespaces**:
//!   - [`structs`]: Every declared style struct (`TextStyle`, `FlatButton`, `RoundButton`, `Box`,
//!     `InputField`, etc.). Each struct is also re-exported inside the module where it was declared.
//!   - [`styles`] & top-level directory re-exports (e.g. `tg_style::ui::widgets::default_flat_button`,
//!     `tg_style::dialogs::dialogs_row_height`, `tg_style::boxes::box_width`): Style variables
//!     grouped by source `.style` file. When a file's stem matches its parent directory (e.g.
//!     `ui/widgets/widgets.style`), its items live directly in that directory's module
//!     (`tg_style::ui::widgets::*`).
//!   - [`st`]: Flat re-export of every style variable in `snake_case`, mirroring C++ `st::*`
//!     (e.g. `tg_style::st::default_flat_button`).
//! - **Runtime (`scale`, `types`, `color`, `font`, `icon`)**: Plain Rust types with zero GUI
//!   framework dependencies. `Pixels`, `Point`, `Size`, `Margins`, `Rect`, and `Font` scale on
//!   access using tdesktop's exact `ConvertScale` rounding rules; `ColorRef` resolves palette
//!   names at runtime via [`ColorResolver`]; `Icon` rasterizes multi-layer PNG/SVG/SolidRect
//!   masks with DPI variant selection and colorization.

pub mod color;
pub mod font;
pub mod icon;
pub mod scale;
pub mod types;

pub use color::{Color, ColorRef, ColorResolver, DefaultPaletteResolver, default_palette_color};
pub use font::{DEFAULT_FONT_FAMILY, Font, FontFlags, SYSTEM_FONT_TAG};
pub use icon::{
    ICON_ASSET_COUNT, Icon, IconError, IconMaskRef, IconModifier, MonoIcon, RgbaImage,
    icon_asset_bytes, icon_asset_keys,
};
pub use scale::{
    SCALE_ALWAYS_ALLOW_MAX, SCALE_AUTO, SCALE_DEFAULT, SCALE_MAX, SCALE_MIN, STANDARD_SCALES,
    Scale, base_dpi, check_scale, convert_float_scale, convert_scale, convert_scale_current,
    convert_scale_exact_f64, convert_scale_f64, device_pixel_ratio, max_scale_for_ratio, scale,
    set_device_pixel_ratio, set_scale,
};
pub use types::{
    Align, Cursor, Margins, Pixels, Point, Rect, ScaledMargins, ScaledPoint, ScaledRect,
    ScaledSize, Size,
};

include!(concat!(env!("OUT_DIR"), "/generated_styles.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn all_style_files_and_counts_generated() {
        assert_eq!(STYLE_FILE_COUNT, 77);
        assert_eq!(STYLE_STRUCT_COUNT, 142);
        assert_eq!(STYLE_VARIABLE_COUNT, 7054);
        assert_eq!(PALETTE_COLOR_COUNT, 588);
        assert_eq!(ICON_ASSET_COUNT, 2623);
    }

    #[test]
    fn known_basic_style_values() {
        assert_eq!(ui::basic::fsize, Pixels::new(13));
        assert_eq!(ui::basic::box_font_size, Pixels::new(14));
        assert_eq!(ui::basic::normal_font.size, Pixels::new(13));
        assert_eq!(ui::basic::normal_font.flags, FontFlags::NONE);
        assert_eq!(ui::basic::semibold_font.size, Pixels::new(13));
        assert!(ui::basic::semibold_font.flags.is_semibold());
        assert!(ui::basic::semibold_font.flags.is_bold());
        assert!(ui::basic::link_font_over.flags.is_underline());
        assert_eq!(ui::basic::emoji_size, Pixels::new(18));
        assert_eq!(ui::basic::round_radius_large, Pixels::new(6));
        assert_eq!(ui::basic::date_radius, Pixels::new(6));
        assert_eq!(ui::basic::radial_size, Size::new(50, 50));
        assert_eq!(ui::basic::location_size, Size::new(320, 240));
        assert_eq!(ui::basic::slide_duration, 240);
        assert_eq!(ui::basic::k_link_underline_never, 0);
        assert_eq!(ui::basic::k_link_underline_active, 1);
        assert_eq!(ui::basic::k_link_underline_always, 2);

        // Struct inheritance: semiboldTextStyle inherits from defaultTextStyle
        assert_eq!(ui::basic::default_text_style.font, ui::basic::normal_font);
        assert_eq!(ui::basic::default_text_style.link_underline, 1);
        assert_eq!(
            ui::basic::semibold_text_style.font,
            ui::basic::semibold_font
        );
        assert_eq!(ui::basic::semibold_text_style.link_underline, 1);
        assert_eq!(ui::basic::semibold_text_style.line_height, Pixels::new(0));

        // Color reference preservation
        assert_eq!(
            ui::basic::default_text_palette.link_fg,
            ColorRef::new("windowActiveTextFg")
        );
        assert_eq!(
            ui::basic::default_text_palette.select_fg,
            ColorRef::new("transparent")
        );
        assert_eq!(ui::basic::rounded_fg, ColorRef::new("radialFg"));
        assert_eq!(ui::basic::rounded_bg, ColorRef::new("radialBg"));

        // Flat `st::*` re-export matches module path
        assert_eq!(st::fsize, ui::basic::fsize);
        assert_eq!(st::semibold_text_style, ui::basic::semibold_text_style);
    }

    #[test]
    fn known_widgets_and_cross_module_values() {
        // From ui/widgets/widgets.style
        assert_eq!(
            ui::widgets::default_infinite_radial_animation.linear_period,
            1000
        );
        assert_eq!(
            ui::widgets::default_infinite_radial_animation.sine_period,
            3000
        );
        assert!((ui::widgets::default_infinite_radial_animation.arc_min - 0.0625).abs() < 1e-12);
        assert!((ui::widgets::default_infinite_radial_animation.arc_max - 0.75).abs() < 1e-12);
        assert_eq!(st::default_icon_button, ui::widgets::default_icon_button);

        // From ui/layers/layers.style (including struct inheritance & negative px)
        assert_eq!(ui::layers::box_radius, Pixels::new(8));
        assert_eq!(ui::layers::box_title_height, Pixels::new(48));
        assert_eq!(ui::layers::box_title_position, Point::new(24, 13));
        assert_eq!(ui::layers::box_row_padding, Margins::new(24, 0, 24, 0));
        assert_eq!(ui::layers::default_box_button.width, Pixels::new(-30));
        assert_eq!(ui::layers::default_box_button.height, Pixels::new(34));
        assert_eq!(ui::layers::attention_box_button.width, Pixels::new(-30));
        assert_eq!(
            ui::layers::attention_box_button.text_fg,
            ColorRef::new("attentionButtonFg")
        );
        assert_eq!(ui::layers::box_title_close.width, Pixels::new(48));

        // From dialogs/dialogs.style
        assert_eq!(dialogs::dialogs_row_height, Pixels::new(62));
        assert_eq!(dialogs::dialogs_filter_padding, Point::new(7, 7));
        assert_eq!(dialogs::default_dialog_row.height, Pixels::new(62));
        assert_eq!(dialogs::default_dialog_row.photo_size, Pixels::new(46));
        assert_eq!(
            dialogs::default_dialog_row.padding,
            Margins::new(10, 8, 10, 8)
        );
        assert_eq!(dialogs::default_forum_topic_icon.size, Pixels::new(21));
        assert!(dialogs::default_forum_topic_icon.font.flags.is_bold());

        // From ui/chat/chat.style
        assert_eq!(ui::chat::msg_max_width, Pixels::new(430));
        assert_eq!(ui::chat::msg_min_width, Pixels::new(160));
        assert_eq!(ui::chat::msg_padding, Margins::new(11, 8, 11, 8));
        assert_eq!(ui::chat::msg_tag_badge_padding, Margins::new(5, -1, 5, 0));
        assert_eq!(ui::chat::tag_info_admins_only_label.align, Align::Top);
        assert_eq!(ui::chat::history_edit_cover_radial.size, Size::new(16, 16));
        assert_eq!(ui::chat::history_edit_cover_radial.linear_period, 1000);
    }

    #[test]
    fn convert_scale_known_answers() {
        // 100% identity
        for v in [-20, -1, 0, 1, 2, 3, 13, 100] {
            assert_eq!(convert_scale(v, Scale::S100), v);
        }

        // 0 always maps to 0
        for s in STANDARD_SCALES {
            assert_eq!(convert_scale(0, s), 0);
        }

        // Exact half-pixel values round DOWN due to `- 0.01`
        assert_eq!(convert_scale(1, Scale::S150), 1); // 1.5 - 0.01 = 1.49 -> 1
        assert_eq!(convert_scale(3, Scale::S150), 4); // 4.5 - 0.01 = 4.49 -> 4
        assert_eq!(convert_scale(2, Scale::S125), 2); // 2.5 - 0.01 = 2.49 -> 2
        assert_eq!(convert_scale(6, Scale::S125), 7); // 7.5 - 0.01 = 7.49 -> 7

        // Values above half round UP
        assert_eq!(convert_scale(3, Scale::S125), 4); // 3.75 - 0.01 = 3.74 -> 4
        assert_eq!(convert_scale(13, Scale::S125), 16); // 16.25 - 0.01 = 16.24 -> 16
        assert_eq!(convert_scale(13, Scale::S150), 19); // 19.5 - 0.01 = 19.49 -> 19
        assert_eq!(convert_scale(13, Scale::S175), 23); // 22.75 - 0.01 = 22.74 -> 23
        assert_eq!(convert_scale(13, Scale::S200), 26);
        assert_eq!(convert_scale(13, Scale::S225), 29); // 29.25 - 0.01 = 29.24 -> 29
        assert_eq!(convert_scale(13, Scale::S250), 32); // 32.5 - 0.01 = 32.49 -> 32
        assert_eq!(convert_scale(13, Scale::S275), 36); // 35.75 - 0.01 = 35.74 -> 36
        assert_eq!(convert_scale(13, Scale::S300), 39);
        assert_eq!(convert_scale(10, Scale::S110), 11);

        // Negative symmetry
        assert_eq!(convert_scale(-3, Scale::S150), -4);
        assert_eq!(convert_scale(-1, Scale::S150), -1);

        // Non-zero positive never shrinks to 0 even at 50%
        assert_eq!(convert_scale(1, Scale::MIN), 1);
        assert_eq!(convert_scale(-1, Scale::MIN), -1);

        // MaxScaleForRatio
        assert_eq!(max_scale_for_ratio(1), 300);
        assert_eq!(max_scale_for_ratio(2), 200);
        assert_eq!(max_scale_for_ratio(3), 200);
    }

    #[test]
    fn color_resolver_and_defaults() {
        let def = DefaultPaletteResolver;
        assert_eq!(
            ColorRef::new("windowBg").resolve(&def),
            Color::rgb(255, 255, 255)
        );
        assert_eq!(ColorRef::TRANSPARENT.resolve(&def), Color::TRANSPARENT);
        assert_eq!(ColorRef::WHITE.resolve(&def), Color::WHITE);

        let mut custom = HashMap::new();
        custom.insert("windowBg", Color::rgb(23, 33, 43));
        let resolver = |name: &str| custom.get(name).copied();
        assert_eq!(
            ColorRef::new("windowBg").resolve(&resolver),
            Color::rgb(23, 33, 43)
        );
        // Falls back to default palette when custom map doesn't contain the key
        assert_eq!(
            ColorRef::new("windowFg").resolve(&resolver),
            default_palette_color("windowFg").unwrap()
        );
    }

    #[test]
    fn every_bundled_icon_asset_is_accessible_and_valid() {
        let mut png_count = 0usize;
        let mut svg_count = 0usize;
        for key in icon_asset_keys() {
            let bytes = icon_asset_bytes(key).expect("indexed asset must exist");
            assert!(!bytes.is_empty(), "empty icon asset {key}");
            if key.ends_with(".png") {
                assert_eq!(
                    &bytes[0..8],
                    &[137, 80, 78, 71, 13, 10, 26, 10],
                    "bad PNG {key}"
                );
                png_count += 1;
            } else if key.ends_with(".svg") {
                let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
                    .unwrap_or_else(|e| panic!("invalid SVG {key}: {e}"));
                assert!(
                    tree.size().width() > 0.0 && tree.size().height() > 0.0,
                    "zero-size SVG {key}"
                );
                svg_count += 1;
            }
        }
        assert_eq!(png_count, 776 * 3);
        assert_eq!(svg_count, 295);
    }

    #[test]
    fn multi_layer_solid_and_modifier_icons() {
        let resolver = DefaultPaletteResolver;

        // Multi-layer icon combining SolidRect + PNG parts
        assert!(calls::call_title_minimize_icon_over.parts.len() >= 2);
        let multi = calls::call_title_minimize_icon_over
            .rasterize_at(Scale::S100, 1, &resolver)
            .expect("multi-layer icon rasterize");
        assert!(multi.width > 0 && multi.height > 0);
        let straight = multi.to_straight_rgba();
        assert_eq!(straight.len(), multi.pixels.len());

        // Contrast helper known answers
        assert!(color::good_for_contrast(Color::BLACK, Color::WHITE));
        assert!(!color::good_for_contrast(
            Color::rgb(120, 120, 120),
            Color::rgb(125, 125, 125)
        ));
    }

    #[test]
    fn icon_png_and_svg_rasterization_and_dpi_scaling() {
        let resolver = DefaultPaletteResolver;

        // PNG icon: simple_close is 10x10 at 1x, 20x20 at 2x, 30x30 at 3x
        let img1x = ui::basic::small_close_icon
            .rasterize_at(Scale::S100, 1, &resolver)
            .expect("1x PNG rasterize");
        assert_eq!((img1x.width, img1x.height), (10, 10));

        let img2x = ui::basic::small_close_icon
            .rasterize_at(Scale::S100, 2, &resolver)
            .expect("2x PNG rasterize");
        assert_eq!((img2x.width, img2x.height), (20, 20));
        assert_eq!((img2x.logical_width(), img2x.logical_height()), (10, 10));

        let img150 = ui::basic::small_close_icon
            .rasterize_at(Scale::S150, 1, &resolver)
            .expect("150% PNG rasterize");
        assert_eq!((img150.width, img150.height), (15, 15));

        // PNG with flip_horizontal modifier
        let flipped = ui::layers::box_title_close
            .icon
            .rasterize_at(Scale::S100, 1, &resolver)
            .expect("box_title_close rasterize");
        assert!(flipped.width > 0 && flipped.height > 0);

        // SVG icon with -24x24 size override
        let svg_icon = ui::menu_icons::menu_icon_copy
            .rasterize_at(Scale::S100, 2, &resolver)
            .expect("SVG rasterize");
        assert_eq!((svg_icon.width, svg_icon.height), (48, 48));
        assert!(svg_icon.pixels.iter().any(|&b| b > 0));

        // SVG icon with -48x48 size override (from ui/chat/chat.style)
        let tag_svg = ui::chat::tag_info_icon
            .rasterize_at(Scale::S100, 1, &resolver)
            .expect("tag_info_icon SVG rasterize");
        assert_eq!((tag_svg.width, tag_svg.height), (48, 48));
        assert!(tag_svg.pixels.iter().any(|&b| b > 0));

        // SolidRect icon with padding and color override check
        const SOLID_PARTS: [MonoIcon; 1] = [MonoIcon {
            mask: IconMaskRef::SolidRect {
                width: Pixels::new(8),
                height: Pixels::new(4),
            },
            color: ColorRef::new("windowActiveTextFg"),
            padding: Margins::new(1, 2, 1, 2),
        }];
        let solid = Icon::new(&SOLID_PARTS);
        let rendered = solid
            .rasterize_with_color(Scale::S100, 1, Color::rgba(255, 0, 0, 255))
            .expect("solid rect rasterize");
        assert_eq!((rendered.width, rendered.height), (10, 8));
        // Corner (0,0) is padding -> transparent; (1,2) is solid red
        assert_eq!(&rendered.pixels[0..4], &[0, 0, 0, 0]);
        let px_idx = ((2 * 10 + 1) * 4) as usize;
        assert_eq!(&rendered.pixels[px_idx..px_idx + 4], &[255, 0, 0, 255]);
    }

    #[test]
    fn font_descriptor_flags_and_scaling() {
        let base = Font::new("", Pixels::new(13), FontFlags::NONE);
        assert_eq!(base.effective_family(), DEFAULT_FONT_FAMILY);
        assert_eq!(base.scaled_size_at(Scale::S100), 13);
        assert_eq!(base.scaled_size_at(Scale::S150), 19);
        assert_eq!(base.scaled_size_at(Scale::S200), 26);

        let semi = base.semibold(true).italic(true).underline(true);
        assert!(semi.flags.is_semibold());
        assert!(semi.flags.is_bold());
        assert!(semi.flags.is_italic());
        assert!(semi.flags.is_underline());

        let sub = base.sub_or_super(true);
        assert_eq!(sub.scaled_size_at(Scale::S100), 10); // round(13 * 0.75) = 10

        // Monospace clears other flags (matching FontData::otherFlagsFont)
        let mono = semi.monospace(true);
        assert_eq!(mono.flags, FontFlags::MONOSPACE);
        assert!(mono.flags.is_monospace());
    }

    #[test]
    fn global_scale_updates_px_on_access() {
        let prev_scale = scale();
        let prev_dpr = device_pixel_ratio();
        set_scale(Scale::S150);
        set_device_pixel_ratio(2);
        assert_eq!(scale(), Scale::S150);
        assert_eq!(device_pixel_ratio(), 2);
        assert_eq!(ui::basic::fsize.scaled(), 19);
        assert_eq!(
            ui::layers::box_title_position.scaled(),
            ScaledPoint { x: 36, y: 19 }
        );
        assert_eq!(check_scale(300), 200);
        set_scale(prev_scale);
        set_device_pixel_ratio(prev_dpr);
    }
}
