//! Built-in embedded themes, accent colorizer construction, and accent recipes.
//!
//! Port of `SourceFiles/window/themes/window_themes_embedded.h` and `window_themes_embedded.cpp`.

use std::collections::{BTreeMap, BTreeSet};

use crate::color::{Color, ensure_contrast};
use crate::colorizer::{Colorizer, ColorizerColor, colorize_color, colorize_named};
use crate::palette::{Palette, PaletteRow};
use crate::theme_file::{ThemeError, load_theme_from_bytes, read_palette_values};

/// Virtual resource path for the built-in Day Blue theme.
pub const DAY_BLUE_THEME_PATH: &str = ":/gui/day-blue.tdesktop-theme";
/// Virtual resource path for the built-in Night (Tinted) theme.
pub const NIGHT_THEME_PATH: &str = ":/gui/night.tdesktop-theme";
/// Virtual resource path for the built-in Night Green (Night) theme.
pub const NIGHT_GREEN_THEME_PATH: &str = ":/gui/night-green.tdesktop-theme";
/// Virtual resource path for the custom day base theme.
pub const DAY_CUSTOM_BASE_PATH: &str = ":/gui/day-custom-base.tdesktop-theme";
/// Virtual resource path for the custom night base theme.
pub const NIGHT_CUSTOM_BASE_PATH: &str = ":/gui/night-custom-base.tdesktop-theme";

const DAY_BLUE_BYTES: &[u8] = include_bytes!("../assets/day-blue.tdesktop-theme");
const DAY_CUSTOM_BASE_BYTES: &[u8] = include_bytes!("../assets/day-custom-base.tdesktop-theme");
const NIGHT_BYTES: &[u8] = include_bytes!("../assets/night.tdesktop-theme");
const NIGHT_GREEN_BYTES: &[u8] = include_bytes!("../assets/night-green.tdesktop-theme");
const NIGHT_CUSTOM_BASE_BYTES: &[u8] = include_bytes!("../assets/night-custom-base.tdesktop-theme");

const MAX_ACCENT_COLORS: usize = 4;

/// Keys excluded from accent HSV colorization (`kColorizeIgnoredKeys`).
pub const COLORIZE_IGNORED_KEYS: &[&str] = &[
    "boxTextFgGood",
    "boxTextFgError",
    "callIconFg",
    "historyPeer1NameFg",
    "historyPeer1NameFgSelected",
    "historyPeer1UserpicBg",
    "historyPeer2NameFg",
    "historyPeer2NameFgSelected",
    "historyPeer2UserpicBg",
    "historyPeer3NameFg",
    "historyPeer3NameFgSelected",
    "historyPeer3UserpicBg",
    "historyPeer4NameFg",
    "historyPeer4NameFgSelected",
    "historyPeer4UserpicBg",
    "historyPeer5NameFg",
    "historyPeer5NameFgSelected",
    "historyPeer5UserpicBg",
    "historyPeer6NameFg",
    "historyPeer6NameFgSelected",
    "historyPeer6UserpicBg",
    "historyPeer7NameFg",
    "historyPeer7NameFgSelected",
    "historyPeer7UserpicBg",
    "historyPeer8NameFg",
    "historyPeer8NameFgSelected",
    "historyPeer8UserpicBg",
    "historyPeer1UserpicBg2",
    "historyPeer2UserpicBg2",
    "historyPeer3UserpicBg2",
    "historyPeer4UserpicBg2",
    "historyPeer5UserpicBg2",
    "historyPeer6UserpicBg2",
    "historyPeer7UserpicBg2",
    "historyPeer8UserpicBg2",
    "msgFile1Bg",
    "msgFile1BgDark",
    "msgFile1BgOver",
    "msgFile1BgSelected",
    "msgFile2Bg",
    "msgFile2BgDark",
    "msgFile2BgOver",
    "msgFile2BgSelected",
    "msgFile3Bg",
    "msgFile3BgDark",
    "msgFile3BgOver",
    "msgFile3BgSelected",
    "msgFile4Bg",
    "msgFile4BgDark",
    "msgFile4BgOver",
    "msgFile4BgSelected",
    "mediaviewFileRedCornerFg",
    "mediaviewFileYellowCornerFg",
    "mediaviewFileGreenCornerFg",
    "mediaviewFileBlueCornerFg",
    "settingsIconBg1",
    "settingsIconBg2",
    "settingsIconBg3",
    "settingsIconBg4",
    "settingsIconBg5",
    "settingsIconBg6",
    "settingsIconBg8",
    "settingsIconBgArchive",
    "premiumButtonBg1",
    "premiumButtonBg2",
    "premiumButtonBg3",
    "premiumIconBg1",
    "premiumIconBg2",
];

/// Built-in embedded theme identifier (`Window::Theme::EmbeddedType`).
///
/// Discriminant values match C++ serialization in `AccentColors::serialize()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum EmbeddedType {
    /// Day Blue ("Day") theme.
    DayBlue = 0,
    /// Classic ("Default") day theme.
    Default = 1,
    /// Tinted Night ("Tinted") theme.
    Night = 2,
    /// Night Green ("Night") theme.
    NightGreen = 3,
}

impl EmbeddedType {
    /// Converts a serialized `i32` discriminant back into an `EmbeddedType`.
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::DayBlue),
            1 => Some(Self::Default),
            2 => Some(Self::Night),
            3 => Some(Self::NightGreen),
            _ => None,
        }
    }

    /// Returns whether this embedded theme is a dark theme.
    #[must_use]
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Night | Self::NightGreen)
    }
}

/// Preview metadata and base accent color for an embedded theme (`Window::Theme::EmbeddedScheme`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedScheme {
    /// Theme identifier.
    pub kind: EmbeddedType,
    /// Preview background swatch color.
    pub background: Color,
    /// Preview outgoing message swatch color.
    pub sent: Color,
    /// Preview incoming message swatch color.
    pub received: Color,
    /// Preview inactive radio swatch color.
    pub radiobutton_inactive: Color,
    /// Preview active radio swatch color.
    pub radiobutton_active: Color,
    /// Human-readable default English name (Classic / Day / Tinted / Night).
    pub name: &'static str,
    /// Virtual resource path (`""` for `Default`, `":/gui/..."` for others).
    pub path: &'static str,
    /// Base accent color used as `Colorizer::was`.
    pub accent_color: Color,
}

/// Per-embedded-theme custom accent color map (`Window::Theme::AccentColors`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccentColors {
    data: BTreeMap<EmbeddedType, Color>,
}

impl AccentColors {
    /// Sets a custom accent color for `kind`.
    pub fn set(&mut self, kind: EmbeddedType, value: Color) {
        self.data.insert(kind, value);
    }

    /// Clears any custom accent color for `kind`.
    pub fn clear(&mut self, kind: EmbeddedType) {
        self.data.remove(&kind);
    }

    /// Returns the custom accent color for `kind`, if set.
    #[must_use]
    pub fn get(&self, kind: EmbeddedType) -> Option<Color> {
        self.data.get(&kind).copied()
    }

    /// Returns `true` if no custom accents are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Serializes to the exact Qt `QDataStream` (`Qt_5_1`, big-endian) format
    /// used by `Window::Theme::AccentColors::serialize()`.
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        if self.data.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(4 + self.data.len() * 8);
        out.extend_from_slice(&(self.data.len() as i32).to_be_bytes());
        for (&kind, &color) in &self.data {
            out.extend_from_slice(&(kind as i32).to_be_bytes());
            out.extend_from_slice(&color.to_serialized_rgba32().to_be_bytes());
        }
        out
    }

    /// Deserializes from bytes produced by `AccentColors::serialize()`.
    pub fn set_from_serialized(&mut self, serialized: &[u8]) -> bool {
        if serialized.is_empty() {
            self.data.clear();
            return true;
        }
        if serialized.len() < 4 {
            return false;
        }
        let count =
            i32::from_be_bytes([serialized[0], serialized[1], serialized[2], serialized[3]]);
        if count <= 0 || (count as usize) > MAX_ACCENT_COLORS {
            return false;
        }
        let expected_len = 4 + (count as usize) * 8;
        if serialized.len() < expected_len {
            return false;
        }
        let mut parsed = BTreeMap::new();
        for i in 0..(count as usize) {
            let offset = 4 + i * 8;
            let raw_type = i32::from_be_bytes([
                serialized[offset],
                serialized[offset + 1],
                serialized[offset + 2],
                serialized[offset + 3],
            ]);
            let raw_color = u32::from_be_bytes([
                serialized[offset + 4],
                serialized[offset + 5],
                serialized[offset + 6],
                serialized[offset + 7],
            ]);
            let Some(kind) = EmbeddedType::from_i32(raw_type) else {
                return false;
            };
            parsed.insert(kind, Color::from_serialized_rgba32(raw_color));
        }
        self.data = parsed;
        true
    }
}

/// Returns `true` if `path` refers to an embedded theme (`""` or `":/gui/..."`).
#[must_use]
pub fn is_embedded_theme_path(path: &str) -> bool {
    path.is_empty() || path.starts_with(":/gui/")
}

/// Returns the embedded `.tdesktop-theme` archive bytes for `kind` (`None` for `Default`,
/// which uses `colors.palette` directly).
#[must_use]
pub fn embedded_theme_bytes(kind: EmbeddedType) -> Option<&'static [u8]> {
    match kind {
        EmbeddedType::Default => None,
        EmbeddedType::DayBlue => Some(DAY_BLUE_BYTES),
        EmbeddedType::Night => Some(NIGHT_BYTES),
        EmbeddedType::NightGreen => Some(NIGHT_GREEN_BYTES),
    }
}

/// Returns the embedded archive bytes for a virtual `":/gui/..."` resource path.
#[must_use]
pub fn embedded_resource_bytes(path: &str) -> Option<&'static [u8]> {
    match path {
        DAY_BLUE_THEME_PATH => Some(DAY_BLUE_BYTES),
        NIGHT_THEME_PATH => Some(NIGHT_BYTES),
        NIGHT_GREEN_THEME_PATH => Some(NIGHT_GREEN_BYTES),
        DAY_CUSTOM_BASE_PATH => Some(DAY_CUSTOM_BASE_BYTES),
        NIGHT_CUSTOM_BASE_PATH => Some(NIGHT_CUSTOM_BASE_BYTES),
        _ => None,
    }
}

/// Returns the embedded `day-custom-base.tdesktop-theme` bytes.
#[must_use]
pub fn day_custom_base_bytes() -> &'static [u8] {
    DAY_CUSTOM_BASE_BYTES
}

/// Returns the embedded `night-custom-base.tdesktop-theme` bytes.
#[must_use]
pub fn night_custom_base_bytes() -> &'static [u8] {
    NIGHT_CUSTOM_BASE_BYTES
}

fn c_hex(hex: &str) -> Color {
    Color::from_hex(hex).unwrap_or(Color::BLACK)
}

fn c_hsv(hex: &str) -> ColorizerColor {
    ColorizerColor::from_color(c_hex(hex))
}

/// Returns the 4 built-in themes in tdesktop's canonical order (`Window::Theme::EmbeddedThemes`).
#[must_use]
pub fn embedded_themes() -> Vec<EmbeddedScheme> {
    vec![
        EmbeddedScheme {
            kind: EmbeddedType::Default,
            background: c_hex("9bd494"),
            sent: c_hex("eaffdc"),
            received: c_hex("ffffff"),
            radiobutton_inactive: c_hex("eaffdc"),
            radiobutton_active: c_hex("ffffff"),
            name: "Classic",
            path: "",
            accent_color: c_hex("40a7e3"),
        },
        EmbeddedScheme {
            kind: EmbeddedType::DayBlue,
            background: c_hex("7ec4ea"),
            sent: c_hex("d7f0ff"),
            received: c_hex("ffffff"),
            radiobutton_inactive: c_hex("d7f0ff"),
            radiobutton_active: c_hex("ffffff"),
            name: "Day",
            path: DAY_BLUE_THEME_PATH,
            accent_color: c_hex("40a7e3"),
        },
        EmbeddedScheme {
            kind: EmbeddedType::Night,
            background: c_hex("485761"),
            sent: c_hex("5ca7d4"),
            received: c_hex("6b808d"),
            radiobutton_inactive: c_hex("6b808d"),
            radiobutton_active: c_hex("5ca7d4"),
            name: "Tinted",
            path: NIGHT_THEME_PATH,
            accent_color: c_hex("5288c1"),
        },
        EmbeddedScheme {
            kind: EmbeddedType::NightGreen,
            background: c_hex("485761"),
            sent: c_hex("6b808d"),
            received: c_hex("6b808d"),
            radiobutton_inactive: c_hex("6b808d"),
            radiobutton_active: c_hex("75bfb5"),
            name: "Night",
            path: NIGHT_GREEN_THEME_PATH,
            accent_color: c_hex("3fc1b0"),
        },
    ]
}

/// Returns the `EmbeddedScheme` for a specific `EmbeddedType`.
#[must_use]
pub fn embedded_scheme(kind: EmbeddedType) -> EmbeddedScheme {
    embedded_themes()
        .into_iter()
        .find(|s| s.kind == kind)
        .unwrap_or_else(|| embedded_themes().swap_remove(0))
}

/// Returns the 8 preset accent colors for an embedded theme (`Window::Theme::DefaultAccentColors`).
#[must_use]
pub fn default_accent_colors(kind: EmbeddedType) -> Vec<Color> {
    let hexes: &[&str] = match kind {
        EmbeddedType::DayBlue | EmbeddedType::Default => &[
            "45bce7", "52b440", "d46c99", "df8a49", "9978c8", "c55245", "687b98", "dea922",
        ],
        EmbeddedType::Night => &[
            "58bfe8", "466f42", "aa6084", "a46d3c", "917bbd", "ab5149", "697b97", "9b834b",
        ],
        EmbeddedType::NightGreen => &[
            "60a8e7", "4e9c57", "ca7896", "cc925c", "a58ed2", "d27570", "7b8799", "cbac67",
        ],
    };
    hexes.iter().map(|h| c_hex(h)).collect()
}

/// Sets the target accent on a `Colorizer`, clamping HSL lightness to
/// `[colorizer.lightness_min, colorizer.lightness_max]`.
/// Port of `Window::Theme::ApplyColorizerAccent`.
pub fn apply_colorizer_accent(colorizer: &mut Colorizer, color: Color) {
    colorizer.now = ColorizerColor::from_color(color);
    let now_lightness = color.lightness();
    let limited = now_lightness.clamp(colorizer.lightness_min, colorizer.lightness_max);
    if limited != now_lightness {
        let hsl = color.to_hsl();
        let clamped_rgb = Color::from_hsl(hsl.h, hsl.s, limited, 255);
        colorizer.now = ColorizerColor::from_color(clamped_rgb);
    }
}

/// Builds a `Colorizer` for an `EmbeddedScheme` shifted to `color`.
/// Exact port of `Window::Theme::ColorizerFrom`.
#[must_use]
pub fn colorizer_from(scheme: &EmbeddedScheme, color: Color) -> Colorizer {
    let ignore_keys: BTreeSet<String> = COLORIZE_IGNORED_KEYS
        .iter()
        .map(|&s| s.to_string())
        .collect();
    let mut keep_contrast: BTreeMap<String, (ColorizerColor, ColorizerColor)> = BTreeMap::new();
    let mut lightness_min = 0;
    let mut lightness_max = 255;

    match scheme.kind {
        EmbeddedType::Default | EmbeddedType::DayBlue => {
            lightness_max = 160;
        }
        EmbeddedType::Night => {
            for (key, check, replace) in [
                ("activeButtonFg", "2f6ea5", "17212b"),
                ("profileVerifiedCheckFg", "5288c1", "17212b"),
                ("overviewCheckFgActive", "5288c1", "17212b"),
                ("historyFileInIconFg", "3f96d0", "182533"),
                ("historyFileInIconFgSelected", "6ab4f4", "2e70a5"),
                ("historyFileInRadialFg", "3f96d0", "182533"),
                ("historyFileInRadialFgSelected", "6ab4f4", "2e70a5"),
                ("historyFileOutIconFg", "4c9ce2", "2b5278"),
                ("historyFileOutIconFgSelected", "58abf3", "2e70a5"),
                ("historyFileOutRadialFg", "4c9ce2", "2b5278"),
                ("historyFileOutRadialFgSelected", "58abf3", "2e70a5"),
            ] {
                keep_contrast.insert(key.to_string(), (c_hsv(check), c_hsv(replace)));
            }
            lightness_min = 64;
        }
        EmbeddedType::NightGreen => {
            for (key, check, replace) in [
                ("activeButtonFg", "2da192", "282e33"),
                ("profileVerifiedCheckFg", "3fc1b0", "282e33"),
                ("overviewCheckFgActive", "3fc1b0", "282e33"),
            ] {
                keep_contrast.insert(key.to_string(), (c_hsv(check), c_hsv(replace)));
            }
            lightness_min = 64;
        }
    }

    let mut result = Colorizer {
        hue_threshold: 15,
        lightness_min,
        lightness_max,
        was: ColorizerColor::from_color(scheme.accent_color),
        now: ColorizerColor::default(),
        ignore_keys,
        keep_contrast,
    };
    apply_colorizer_accent(&mut result, color);
    result
}

/// Colorizes an `EmbeddedScheme`'s preview swatch colors in place (`Window::Theme::Colorize`).
pub fn colorize_scheme(scheme: &mut EmbeddedScheme, colorizer: &Colorizer) {
    for field in [
        &mut scheme.background,
        &mut scheme.sent,
        &mut scheme.received,
        &mut scheme.radiobutton_active,
        &mut scheme.radiobutton_inactive,
    ] {
        if let Some(changed) = colorize_color(*field, colorizer) {
            *field = changed;
        }
    }
}

/// Builds a finalized `Palette` for one of the 4 embedded themes, optionally
/// shifted to `accent`. Port of `Window::Theme::PrepareEmbeddedPalette`.
pub fn prepare_embedded_palette(
    kind: EmbeddedType,
    accent: Option<Color>,
) -> Result<Palette, ThemeError> {
    let scheme = embedded_scheme(kind);
    let colorizer = accent
        .map(|c| colorizer_from(&scheme, c))
        .unwrap_or_default();

    let Some(bytes) = embedded_theme_bytes(kind) else {
        return Ok(Palette::with_colorizer(&colorizer));
    };

    let instance = load_theme_from_bytes(bytes, &colorizer, None, false)?;
    Ok(instance.palette)
}

/// Builds a finalized `Palette` from `day-custom-base` or `night-custom-base`
/// with optional accent colorizer (`Window::Theme::PreparePaletteCallback`).
pub fn prepare_base_palette(dark: bool, accent: Option<Color>) -> Result<Palette, ThemeError> {
    let scheme = embedded_scheme(if dark {
        EmbeddedType::Night
    } else {
        EmbeddedType::Default
    });
    let colorizer = accent
        .map(|c| colorizer_from(&scheme, c))
        .unwrap_or_default();
    let bytes = if dark {
        NIGHT_CUSTOM_BASE_BYTES
    } else {
        DAY_CUSTOM_BASE_BYTES
    };
    let instance = load_theme_from_bytes(bytes, &colorizer, None, false)?;
    Ok(instance.palette)
}

#[derive(Debug, Clone)]
struct RecipeLeaf {
    rule: &'static str,
    raw: Color,
}

#[derive(Debug, Clone)]
struct RecipeKey {
    name: &'static str,
    index: usize,
    leaf: usize,
}

#[derive(Debug, Clone, Copy)]
enum RecipeOperand {
    Fixed(Color),
    Leaf(usize),
}

#[derive(Debug, Clone)]
struct RecipeContrast {
    name: &'static str,
    index: usize,
    over: RecipeOperand,
    under: RecipeOperand,
}

/// Precomputed dependency graph for fast accent re-application on an embedded theme
/// without re-parsing the zip archive (`Window::Theme::EmbeddedAccentRecipe`).
#[derive(Debug, Clone)]
pub struct EmbeddedAccentRecipe {
    scheme: EmbeddedScheme,
    leaves: Vec<RecipeLeaf>,
    keys: Vec<RecipeKey>,
    contrast: Vec<RecipeContrast>,
}

impl EmbeddedAccentRecipe {
    /// Builds the accent recipe for `kind`.
    pub fn new(kind: EmbeddedType) -> Result<Self, ThemeError> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Status {
            Initial,
            Created,
            Loaded,
        }
        #[derive(Clone, Copy)]
        struct SlotSource {
            status: Status,
            leaf: Option<usize>,
        }
        struct SourceLeaf {
            rule: &'static str,
            raw: Color,
            dependent: bool,
        }

        let scheme = embedded_scheme(kind);
        let colorizer = colorizer_from(&scheme, scheme.accent_color);

        let mut default_names = [""; Palette::COUNT];
        let mut default_raw = [Color::TRANSPARENT; Palette::COUNT];
        let mut default_fallback: [Option<usize>; Palette::COUNT] = [None; Palette::COUNT];
        default_raw[0] = Color::TRANSPARENT;
        default_raw[1] = Color::WHITE;
        for row in Palette::ROWS {
            let PaletteRow {
                index,
                name,
                fallback_index,
                default_color,
                ..
            } = *row;
            default_names[index] = name;
            default_raw[index] = default_color;
            default_fallback[index] = fallback_index;
        }

        let follows_accent = |rule: &str, raw: Color| -> bool {
            if colorizer.ignore_keys.contains(rule) {
                return false;
            }
            if colorizer.keep_contrast.contains_key(rule) {
                return true;
            }
            let hsv = raw.to_hsv();
            (hsv.h - colorizer.was.hue).abs() < colorizer.hue_threshold
        };

        let mut source_leaves: Vec<SourceLeaf> = Vec::new();
        let mut slots = [SlotSource {
            status: Status::Initial,
            leaf: None,
        }; Palette::COUNT];

        let mut add_leaf =
            |slots: &mut [SlotSource], index: usize, rule: &'static str, raw: Color| {
                let leaf_idx = source_leaves.len();
                slots[index].leaf = Some(leaf_idx);
                source_leaves.push(SourceLeaf {
                    rule,
                    raw,
                    dependent: (index > 0) && follows_accent(rule, raw),
                });
            };

        if let Some(bytes) = embedded_theme_bytes(kind) {
            let palette_bytes = crate::theme_file::read_palette_content_from_bytes(bytes)?;
            let mut unsupported: BTreeMap<String, String> = BTreeMap::new();
            read_palette_values(&palette_bytes, |name, raw_val| {
                let value = unsupported
                    .get(raw_val)
                    .map_or(raw_val, String::as_str)
                    .to_string();
                let index = Palette::index_of_name(name);
                if value.starts_with('#') && (value.len() == 7 || value.len() == 9) {
                    if let Some(raw) = Color::from_hex(&value) {
                        match index {
                            None => {
                                unsupported.insert(name.to_string(), value);
                            }
                            Some(idx) => {
                                slots[idx].status = Status::Loaded;
                                add_leaf(&mut slots, idx, default_names[idx], raw);
                            }
                        }
                    }
                } else if let Some(idx) = index {
                    if let Some(from) = Palette::index_of_name(&value)
                        && slots[from].status == Status::Loaded
                    {
                        slots[idx] = slots[from];
                    }
                } else {
                    unsupported.insert(name.to_string(), value);
                }
                true
            })?;
        }

        for index in 0..Palette::COUNT {
            if slots[index].status != Status::Initial {
                continue;
            }
            if let Some(fb) = default_fallback[index]
                && slots[fb].status == Status::Loaded
            {
                slots[index] = slots[fb];
            } else {
                slots[index].status =
                    if index == 0 || colorizer.ignore_keys.contains(default_names[index]) {
                        Status::Created
                    } else {
                        Status::Loaded
                    };
                add_leaf(&mut slots, index, default_names[index], default_raw[index]);
            }
        }

        let mut leaves: Vec<RecipeLeaf> = Vec::new();
        let mut compact: Vec<Option<usize>> = vec![None; source_leaves.len()];

        let mut leaf_of = |index: usize| -> Option<usize> {
            let leaf = slots[index].leaf?;
            if compact[leaf].is_none() && source_leaves[leaf].dependent {
                let id = leaves.len();
                compact[leaf] = Some(id);
                leaves.push(RecipeLeaf {
                    rule: source_leaves[leaf].rule,
                    raw: source_leaves[leaf].raw,
                });
            }
            compact[leaf]
        };

        let mut contrast: Vec<RecipeContrast> = Vec::new();
        for (over_name, under_name) in [
            ("dialogsMenuIconFg", "dialogsBg"),
            ("windowBoldFg", "windowBg"),
        ] {
            if let (Some(over_idx), Some(under_idx)) = (
                Palette::index_of_name(over_name),
                Palette::index_of_name(under_name),
            ) {
                let over_op = match leaf_of(over_idx) {
                    Some(id) => RecipeOperand::Leaf(id),
                    None => RecipeOperand::Fixed(
                        slots[over_idx]
                            .leaf
                            .map_or(Color::TRANSPARENT, |l| source_leaves[l].raw),
                    ),
                };
                let under_op = match leaf_of(under_idx) {
                    Some(id) => RecipeOperand::Leaf(id),
                    None => RecipeOperand::Fixed(
                        slots[under_idx]
                            .leaf
                            .map_or(Color::TRANSPARENT, |l| source_leaves[l].raw),
                    ),
                };
                if matches!(over_op, RecipeOperand::Leaf(_))
                    || matches!(under_op, RecipeOperand::Leaf(_))
                {
                    contrast.push(RecipeContrast {
                        name: over_name,
                        index: over_idx,
                        over: over_op,
                        under: under_op,
                    });
                }
            }
        }

        let mut keys: Vec<RecipeKey> = Vec::new();
        for (index, &name) in default_names
            .iter()
            .enumerate()
            .take(Palette::COUNT)
            .skip(2)
        {
            let Some(leaf) = leaf_of(index) else {
                continue;
            };
            if contrast.iter().any(|c| c.index == index) {
                continue;
            }
            keys.push(RecipeKey { name, index, leaf });
        }

        Ok(Self {
            scheme,
            leaves,
            keys,
            contrast,
        })
    }

    /// Applies `accent` to `palette` in place, returning `true` if any color changed.
    pub fn apply(&self, palette: &mut Palette, accent: Color) -> bool {
        let colorizer = colorizer_from(&self.scheme, accent);
        let values: Vec<Color> = self
            .leaves
            .iter()
            .map(|leaf| {
                let mut r = leaf.raw.r;
                let mut g = leaf.raw.g;
                let mut b = leaf.raw.b;
                colorize_named(leaf.rule, &mut r, &mut g, &mut b, &colorizer);
                Color::rgba(r, g, b, leaf.raw.a)
            })
            .collect();

        let eval = |op: RecipeOperand| -> Color {
            match op {
                RecipeOperand::Leaf(id) => values[id],
                RecipeOperand::Fixed(c) => c,
            }
        };

        let mut written = false;
        let mut write_color = |name: &str, index: usize, color: Color| {
            if palette.color_at_index(index) != Some(color) {
                palette.set_color(name, color);
                written = true;
            }
        };

        for key in &self.keys {
            write_color(key.name, key.index, values[key.leaf]);
        }
        for pair in &self.contrast {
            let adjusted = ensure_contrast(eval(pair.over), eval(pair.under));
            write_color(pair.name, pair.index, adjusted);
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_embedded_themes_load_and_colorize() {
        for scheme in embedded_themes() {
            let base = prepare_embedded_palette(scheme.kind, None).unwrap();
            assert!(base.is_ready());
            assert_eq!(base.is_dark_palette(), scheme.kind.is_dark());

            let accents = default_accent_colors(scheme.kind);
            assert_eq!(accents.len(), 8);
            let shifted0 = prepare_embedded_palette(scheme.kind, Some(accents[0])).unwrap();
            let shifted1 = prepare_embedded_palette(scheme.kind, Some(accents[1])).unwrap();
            assert_ne!(base, shifted1);
            assert_ne!(shifted0, shifted1);

            // Verify EmbeddedAccentRecipe transitioning from accent[0] -> accent[1]
            // produces the exact same palette as full reload with accent[1]!
            let recipe = EmbeddedAccentRecipe::new(scheme.kind).unwrap();
            let mut fast = shifted0;
            assert!(recipe.apply(&mut fast, accents[1]));
            assert_eq!(fast, shifted1);
        }
    }

    #[test]
    fn base_themes_load_cleanly() {
        let day = prepare_base_palette(false, None).unwrap();
        let night = prepare_base_palette(true, None).unwrap();
        assert!(!day.is_dark_palette());
        assert!(night.is_dark_palette());
    }

    #[test]
    fn accent_colors_qdatastream_round_trip() {
        let mut accents = AccentColors::default();
        accents.set(EmbeddedType::Default, Color::rgb(0x52, 0xb4, 0x40));
        accents.set(EmbeddedType::Night, Color::rgb(0xaa, 0x60, 0x84));

        let bytes = accents.serialize();
        assert_eq!(bytes.len(), 4 + 2 * 8);
        // Count = 2 in big-endian
        assert_eq!(&bytes[0..4], &2i32.to_be_bytes());

        let mut restored = AccentColors::default();
        assert!(restored.set_from_serialized(&bytes));
        assert_eq!(restored, accents);

        // Reject invalid count
        assert!(!restored.set_from_serialized(&5i32.to_be_bytes()));
    }

    #[test]
    fn colorizer_from_clamps_lightness_and_keeps_contrast() {
        let day = embedded_scheme(EmbeddedType::Default);
        // Very light color should be clamped to lightness_max = 160
        let c = colorizer_from(&day, Color::rgb(240, 240, 255));
        assert_eq!(c.lightness_max, 160);
        assert_eq!(c.hue_threshold, 15);
        assert!(c.now.to_color(255).lightness() <= 160);

        let night = embedded_scheme(EmbeddedType::Night);
        // Very dark color should be clamped to lightness_min = 64 (within 1 LSB of HSV round-trip)
        let cn = colorizer_from(&night, Color::rgb(5, 5, 10));
        assert_eq!(cn.lightness_min, 64);
        assert!(cn.now.to_color(255).lightness() >= 63);
        assert!(cn.keep_contrast.contains_key("activeButtonFg"));
        assert!(cn.ignore_keys.contains("boxTextFgGood"));
    }
}
