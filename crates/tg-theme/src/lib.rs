//! `tg-theme`: Telegram Desktop palette, themes, accent colorization, night mode,
//! wallpapers, and background gradient rendering.
//!
//! Pure Rust, GUI-framework-independent port of tdesktop's `window/themes/`,
//! `lib_ui/ui/style/` (palette & colorizer), `data/data_wall_paper.cpp`,
//! `ui/chat/chat_theme.cpp`, and `lib_ui/ui/image/image_prepare.cpp` (gradients & blur).

pub mod color;
pub mod colorizer;
pub mod crc32;
pub mod embedded;
pub mod gradient;
pub mod night_mode;
pub mod palette;
pub mod svg_pattern;
pub mod theme_file;
pub mod wallpaper;

pub use color::{
    Color, Hsl, Hsv, Oklab, chroma, count_contrast, count_perceived_brightness, ensure_contrast,
    from_lch, good_for_contrast, hue, is_light_background, theme_adjusted_color, to_lab,
};
pub use colorizer::{
    Colorizer, ColorizerColor, colorize_color, colorize_hex, colorize_hsv, colorize_image,
    colorize_named, colorize_rgb, colorize_with_contrast,
};
pub use crc32::crc32;
pub use embedded::{
    AccentColors, COLORIZE_IGNORED_KEYS, DAY_BLUE_THEME_PATH, DAY_CUSTOM_BASE_PATH,
    EmbeddedAccentRecipe, EmbeddedScheme, EmbeddedType, NIGHT_CUSTOM_BASE_PATH,
    NIGHT_GREEN_THEME_PATH, NIGHT_THEME_PATH, apply_colorizer_accent, colorize_scheme,
    colorizer_from, day_custom_base_bytes, default_accent_colors, embedded_resource_bytes,
    embedded_scheme, embedded_theme_bytes, embedded_themes, is_embedded_theme_path,
    night_custom_base_bytes, prepare_base_palette, prepare_embedded_palette,
};
pub use gradient::{
    COMPLEX_GRADIENT_POSITIONS, ChatBackgroundRects, DEFAULT_BACKGROUND_COLOR, blur_large_image,
    calculate_image_mono_color, compose_pattern_on_gradient, compute_chat_background_rects,
    compute_real_progress, compute_real_rotation, count_average_color_image,
    count_average_color_slice, dither_image, dither_image_with_seed, generate_background_image,
    generate_complex_gradient, generate_dithered_gradient, generate_gradient,
    generate_linear_gradient, generate_small_complex_gradient, invert_pattern_image,
    is_pattern_inverted, prepare_blurred_background, prepare_image_for_tiled,
    prepare_pattern_image, preprocess_background_image,
};
pub use night_mode::{NightModeState, NightModeSwitch, ToggleDecision, default_theme_path};
pub use palette::{
    BubblesReadabilityArgs, PALETTE_CHECKSUM, PALETTE_COUNT, PALETTE_ENTRY_COUNT, PALETTE_ROWS,
    Palette, PaletteColor, PaletteRow, SetResult, darken_under_white_text, ensure_bubbles_readable,
    ensure_peer_name_readable,
};
pub use svg_pattern::{
    PatternTileMode, SVG_MAX_ENTITY_REFS, SVG_MAX_NESTING_DEPTH, SVG_MAX_PIXEL_AREA,
    SVG_MAX_XML_BYTES, SVG_PATTERN_MAX_DIMENSION, SvgPatternError, SvgRasterOptions,
    TGV_MAX_COMPRESSED_BYTES, TGV_MAX_DECOMPRESSED_BYTES, composite_pattern_on_gradient_opaque,
    compress_tgv, decode_svg_or_tgv_bytes, decompress_tgv, decompress_tgv_with_limit,
    is_gzip_bytes, is_svg_bytes, rasterize_svg_pattern_mask, rasterize_svg_rgba,
    rasterize_tgv_pattern_mask, render_tiled_pattern_wallpaper,
    should_invert_pattern_for_intensity, tile_pattern_mask, tint_pattern_pixel,
    validate_svg_xml_safety,
};
pub use theme_file::{
    ADJUSTABLE_COLOR_KEYS, BACKGROUND_PIXEL_LIMIT, CloudTheme, CloudThemeSettings, CloudThemeType,
    ParsedTheme, SavedTheme, THEME_BACKGROUND_SIZE_LIMIT, THEME_FILE_SIZE_LIMIT,
    THEME_SCHEME_SIZE_LIMIT, ThemeCache, ThemeError, ThemeInstance, ThemeObject, ThemePreviewData,
    WebViewThemeParams, cached_theme_path, colorize_in_content, export_palette_for_editor,
    generate_default_palette, generate_theme_name, is_good_slug, load_color_scheme,
    load_from_cache, load_theme_from_bytes, load_theme_from_file, nearest_theme_color_name,
    pack_theme, parse_theme, read_cloud_from_text, read_palette_content_from_bytes,
    read_palette_content_from_file, read_palette_values, replace_adjustable_colors,
    replace_value_in_palette_content, save_theme_to_file, strip_cloud_text_fields, strip_comments,
    web_view_params, write_cloud_to_text,
};
pub use wallpaper::{
    CUSTOM_BACKGROUND_ID, DEFAULT_BACKGROUND_ID, INCORRECT_DEFAULT_BACKGROUND_ID,
    LEGACY1_DEFAULT_BACKGROUND_ID, LEGACY2_DEFAULT_BACKGROUND_ID, LEGACY3_DEFAULT_BACKGROUND_ID,
    LEGACY4_DEFAULT_BACKGROUND_ID, ParsedWallPaperUrl, TESTING_DEFAULT_BACKGROUND_ID,
    TESTING_EDITOR_BACKGROUND_ID, TESTING_THEME_BACKGROUND_ID, THEME_BACKGROUND_ID,
    UNINITIALIZED_BACKGROUND_ID, WallPaper, WallPaperFlags, WallPaperId, color_from_rgb6,
    colors_from_string, from_legacy_background_id, is_valid_wallpaper_slug, parse_wallpaper_url,
    string_from_colors,
};
