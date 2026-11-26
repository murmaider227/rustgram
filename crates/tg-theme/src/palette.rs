//! Palette runtime, fallback resolution, binary serialization, and bubble readability.
//!
//! Port of `lib_ui/ui/style/style_core_palette.cpp` and `SourceFiles/ui/chat/chat_theme_readability.cpp`.

use std::collections::{BTreeMap, BTreeSet};

use crate::color::{Color, Oklab, chroma, count_contrast, ensure_contrast, from_lch, hue, to_lab};
use crate::colorizer::{Colorizer, ColorizerColor, colorize_rgb, colorize_with_contrast};

/// Slot initialization state matching `style::palette_data::Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotStatus {
    Initial,
    Created,
    Loaded,
}

/// Result of attempting to set a palette color by name (`style::palette::SetResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetResult {
    /// Color was written for the first time.
    Ok,
    /// Target color key does not exist in `colors.palette`.
    KeyNotFound,
    /// Referenced source key does not exist or has not been loaded yet.
    ValueNotFound,
    /// Target color was already set; the new value still overwrote it.
    Duplicate,
}

/// Static metadata for one row in `colors.palette` (`style::main_palette::row`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteRow {
    /// Index in `Palette` (`2..PALETTE_COUNT`).
    pub index: usize,
    /// Original camelCase name from `colors.palette`.
    pub name: &'static str,
    /// Value string (`#rrggbb[aa]` or copied color name).
    pub value: &'static str,
    /// Explicit fallback name if declared with `| fallback`, otherwise `""`.
    pub fallback: &'static str,
    /// Palette index of the fallback/copy source, if any.
    pub fallback_index: Option<usize>,
    /// Default resolved RGBA color for the day theme.
    pub default_color: Color,
    /// Comment description from `colors.palette`.
    pub description: &'static str,
}

/// Typed slot of a [`Palette`] colour, generated from `colors.palette`
/// (`PaletteColor::WINDOW_BG` for `windowBg`).
///
/// A misspelt colour is a compile error instead of a transparent paint, and [`Palette::at`]
/// reads the slot directly instead of searching the colour names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaletteColor(u16);

impl PaletteColor {
    /// Palette slot index (`0..Palette::COUNT`).
    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Original camelCase name (`"windowBg"`), as used by `.tdesktop-theme` files.
    #[must_use]
    pub fn name(self) -> &'static str {
        Palette::name_at_index(self.index()).unwrap_or("transparent")
    }

    /// Typed colour for a slot index, or `None` outside `0..Palette::COUNT`.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        if index < PALETTE_COUNT {
            Some(Self(index as u16))
        } else {
            None
        }
    }

    /// Callers pass indices from [`Palette::index_of_name`], which are always in range.
    const fn from_index_unchecked(index: usize) -> Self {
        Self(index as u16)
    }
}

include!(concat!(env!("OUT_DIR"), "/palette_generated.rs"));

struct FinalizeHelper<'a> {
    with: &'a Colorizer,
    ignore_keys: BTreeSet<usize>,
    keep_contrast: BTreeMap<usize, (ColorizerColor, ColorizerColor)>,
}

impl Default for Palette {
    /// Returns the finalized default (day) palette.
    fn default() -> Self {
        let mut p = Self::new_uninitialized();
        p.finalize();
        p
    }
}

impl Palette {
    /// Total number of color slots (including `transparent` and `white`).
    pub const COUNT: usize = PALETTE_COUNT;

    /// Number of named color entries declared in `colors.palette`.
    pub const ENTRY_COUNT: usize = PALETTE_ENTRY_COUNT;

    /// CRC32 checksum of the palette schema (`style::palette::Checksum()`).
    pub const CHECKSUM: i32 = PALETTE_CHECKSUM;

    /// All color metadata rows from `colors.palette`.
    pub const ROWS: &'static [PaletteRow] = PALETTE_ROWS;

    /// Creates a default palette finalized with the given `Colorizer`.
    #[must_use]
    pub fn with_colorizer(colorizer: &Colorizer) -> Self {
        let mut p = Self::new_uninitialized();
        p.finalize_with(colorizer);
        p
    }

    /// Returns whether `finalize` has completed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Looks up a color by original camelCase name.
    #[must_use]
    pub fn color_by_name(&self, name: &str) -> Option<Color> {
        let index = Self::index_of_name(name)?;
        self.raw_color_at(index)
    }

    /// Reads a typed colour slot (`palette.at(PaletteColor::WINDOW_BG)`) without a name lookup.
    #[inline]
    #[must_use]
    pub fn at(&self, color: PaletteColor) -> Color {
        self.raw_color_at(color.index()).unwrap_or(self.transparent)
    }

    /// Looks up a color by palette index (`0..Self::COUNT`).
    #[must_use]
    pub fn color_at_index(&self, index: usize) -> Option<Color> {
        self.raw_color_at(index)
    }

    /// Sets a color directly by index (`0..Self::COUNT`), marking it `Loaded`.
    pub fn set_color_at_index(&mut self, index: usize, color: Color) -> bool {
        if let Some(slot) = self.raw_color_at_mut(index) {
            *slot = color;
            self.status[index] = SlotStatus::Loaded;
            true
        } else {
            false
        }
    }

    /// Sets a color by original camelCase name (`style::palette::setColor`).
    pub fn set_color(&mut self, name: &str, color: Color) -> SetResult {
        let Some(index) = Self::index_of_name(name) else {
            return SetResult::KeyNotFound;
        };
        let duplicate = self.status[index] != SlotStatus::Initial;
        self.set_color_at_index(index, color);
        if duplicate {
            SetResult::Duplicate
        } else {
            SetResult::Ok
        }
    }

    /// Sets a color by copying from another previously loaded camelCase key.
    /// Matches `style::palette::setColor(name, from)`.
    pub fn set_color_from(&mut self, name: &str, from: &str) -> SetResult {
        let Some(name_index) = Self::index_of_name(name) else {
            return SetResult::KeyNotFound;
        };
        let duplicate = self.status[name_index] != SlotStatus::Initial;
        let Some(from_index) = Self::index_of_name(from) else {
            return SetResult::ValueNotFound;
        };
        if self.status[from_index] != SlotStatus::Loaded {
            return SetResult::ValueNotFound;
        }
        if let Some(color) = self.raw_color_at(from_index) {
            self.set_color_at_index(name_index, color);
        }
        if duplicate {
            SetResult::Duplicate
        } else {
            SetResult::Ok
        }
    }

    /// Resets the palette to the uncolorized day defaults.
    pub fn reset(&mut self) {
        self.clear();
        self.finalize();
    }

    /// Resets the palette to day defaults shifted by `colorizer`.
    pub fn reset_with(&mut self, colorizer: &Colorizer) {
        self.clear();
        self.finalize_with(colorizer);
    }

    /// Clears all loaded slots back to `Initial`.
    pub fn clear(&mut self) {
        self.status.fill(SlotStatus::Initial);
        self.ready = false;
    }

    /// Finalizes all uninitialized slots using default colors and fallback chains.
    pub fn finalize(&mut self) {
        self.finalize_with(&Colorizer::default());
    }

    /// Finalizes all uninitialized slots using default colors, fallback chains,
    /// optional `Colorizer`, and mandatory contrast pairs.
    ///
    /// Exact port of `style::palette::finalize(const colorizer &with)` and
    /// `palette_data::finalize`.
    pub fn finalize_with(&mut self, with: &Colorizer) {
        if self.ready {
            return;
        }
        self.ready = true;
        let helper = Self::prepare_finalize_helper(with);

        for (index, &(fallback_index, default_color)) in PALETTE_SPECS.iter().enumerate() {
            self.compute(index, fallback_index, default_color, helper.as_ref());
        }

        for &(over_idx, under_idx) in &CONTRAST_PAIRS {
            if let (Some(over), Some(under)) =
                (self.raw_color_at(over_idx), self.raw_color_at(under_idx))
            {
                let good = ensure_contrast(over, under);
                if good != over
                    && let Some(slot) = self.raw_color_at_mut(over_idx)
                {
                    *slot = good;
                }
            }
        }
    }

    fn prepare_finalize_helper(with: &Colorizer) -> Option<FinalizeHelper<'_>> {
        if !with.is_active() {
            return None;
        }
        let mut ignore_keys = BTreeSet::new();
        ignore_keys.insert(0);
        for key in &with.ignore_keys {
            if let Some(index) = Self::index_of_name(key)
                && index > 0
            {
                ignore_keys.insert(index);
            }
        }
        let mut keep_contrast = BTreeMap::new();
        for (key, &contrast) in &with.keep_contrast {
            if let Some(index) = Self::index_of_name(key)
                && index > 0
            {
                keep_contrast.insert(index, contrast);
            }
        }
        Some(FinalizeHelper {
            with,
            ignore_keys,
            keep_contrast,
        })
    }

    fn compute(
        &mut self,
        index: usize,
        fallback_index: Option<usize>,
        mut value: Color,
        helper: Option<&FinalizeHelper<'_>>,
    ) {
        if self.status[index] != SlotStatus::Initial {
            return;
        }
        if let Some(fb) = fallback_index
            && self.status[fb] == SlotStatus::Loaded
        {
            self.status[index] = SlotStatus::Loaded;
            if let Some(fb_color) = self.raw_color_at(fb)
                && let Some(slot) = self.raw_color_at_mut(index)
            {
                *slot = fb_color;
            }
            return;
        }

        match helper {
            None => {
                self.status[index] = SlotStatus::Created;
            }
            Some(h) if h.ignore_keys.contains(&index) => {
                self.status[index] = SlotStatus::Created;
            }
            Some(h) => {
                if let Some(&contrast) = h.keep_contrast.get(&index) {
                    colorize_with_contrast(
                        contrast,
                        &mut value.r,
                        &mut value.g,
                        &mut value.b,
                        h.with,
                    );
                } else {
                    colorize_rgb(&mut value.r, &mut value.g, &mut value.b, h.with);
                }
                self.status[index] = SlotStatus::Loaded;
            }
        }
        if let Some(slot) = self.raw_color_at_mut(index) {
            *slot = value;
        }
    }

    /// Serializes all `COUNT` colors as `COUNT * 4` RGBA bytes (`style::palette::save`).
    #[must_use]
    pub fn save(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::COUNT * 4);
        if self.ready {
            for i in 0..Self::COUNT {
                let c = self.raw_color_at(i).unwrap_or(Color::TRANSPARENT);
                out.extend_from_slice(&[c.r, c.g, c.b, c.a]);
            }
        } else {
            let mut copy = self.clone();
            copy.finalize();
            for i in 0..Self::COUNT {
                let c = copy.raw_color_at(i).unwrap_or(Color::TRANSPARENT);
                out.extend_from_slice(&[c.r, c.g, c.b, c.a]);
            }
        }
        out
    }

    /// Loads `COUNT * 4` RGBA bytes produced by [`Self::save`] (`style::palette::load`).
    pub fn load(&mut self, cache: &[u8]) -> bool {
        if cache.len() != Self::COUNT * 4 {
            return false;
        }
        for (i, &[r, g, b, a]) in cache.as_chunks::<4>().0.iter().enumerate() {
            self.set_color_at_index(i, Color::rgba(r, g, b, a));
        }
        self.ready = true;
        true
    }

    /// Returns `true` if `dialogsBg` HSV value is below 0.5 (`Window::Theme::IsThemeDarkValue`).
    #[must_use]
    pub fn is_dark_dialogs(&self) -> bool {
        self.dialogs_bg.value_f() < 0.5
    }

    /// Returns `true` if `windowBg` has higher contrast against white than black
    /// (`Ui::IsDarkPalette`).
    #[must_use]
    pub fn is_dark_palette(&self) -> bool {
        count_contrast(self.window_bg, Color::BLACK) < count_contrast(self.window_bg, Color::WHITE)
    }
}

impl tg_style::ColorResolver for Palette {
    fn resolve(&self, name: &str) -> Option<tg_style::Color> {
        match name {
            "transparent" => Some(self.transparent.to_style_color()),
            "white" => Some(self.white.to_style_color()),
            _ => self.color_by_name(name).map(Color::to_style_color),
        }
    }
}

/// Options for [`ensure_bubbles_readable`] (`Ui::BubblesReadabilityArgs`).
#[derive(Debug, Clone, Default)]
pub struct BubblesReadabilityArgs<'a> {
    /// Whether the palette is dark.
    pub dark: bool,
    /// Optional outgoing bubble gradient colors (if multi-stop).
    pub outgoing_gradient: Vec<Color>,
    /// Optional reference palette whose baseline contrast floors should not be exceeded.
    pub reference: Option<&'a Palette>,
    /// Whether to derive outgoing selection color from the outgoing gradient stops.
    pub derive_outgoing_selection: bool,
}

const ROLES: usize = 6;
const READABILITY_STEP: f64 = 0.005;
const READABILITY_STEPS: usize = 160;
const OUTGOING_LIFT: f64 = 0.08;
const MIN_CHROMA: f64 = 0.03;
const OUTGOING_CHROMA_SCALE: f64 = 1.15;
const OUTGOING_CHROMA_MIN: f64 = 0.05;
const OUTGOING_CHROMA_MAX: f64 = 0.16;
const OLIVE_FROM: f64 = 95.0;
const OLIVE_SPLIT: f64 = 110.0;
const OLIVE_TO: f64 = 125.0;
const OLIVE_WARM: f64 = 75.0;
const OLIVE_GREEN: f64 = 140.0;
const OLIVE_LIGHTNESS: f64 = 0.66;
const OLIVE_RANGE: f64 = 0.18;
const WHITE_TEXT_CONTRAST: f64 = 4.5;
const ALPHA_STEP: u8 = 8;

#[derive(Clone)]
struct Floors {
    bubble_text: f64,
    text: f64,
    separation: f64,
    selected: [f64; ROLES],
    unselected: [f64; ROLES],
    names: f64,
    names_unselected: f64,
}

struct SideKeys {
    bg: &'static str,
    bg_selected: &'static str,
    text: &'static str,
    text_selected: &'static str,
    selected: [Option<&'static str>; ROLES],
    unselected: [Option<&'static str>; ROLES],
    bg_dependents: &'static [&'static str],
    dependents: &'static [&'static str],
    names: bool,
}

const INCOMING_SIDE: SideKeys = SideKeys {
    bg: "msgInBg",
    bg_selected: "msgInBgSelected",
    text: "historyTextInFg",
    text_selected: "historyTextInFgSelected",
    selected: [
        Some("historyLinkInFgSelected"),
        Some("msgInServiceFgSelected"),
        Some("msgInDateFgSelected"),
        Some("msgInMonoFgSelected"),
        Some("msgInReplyBarSelColor"),
        None,
    ],
    unselected: [
        Some("historyLinkInFg"),
        Some("msgInServiceFg"),
        Some("msgInDateFg"),
        Some("msgInMonoFg"),
        Some("msgInReplyBarColor"),
        None,
    ],
    bg_dependents: &[
        "historyFileInIconFg",
        "historyFileThumbIconFg",
        "historyFileInRadialFg",
    ],
    dependents: &[
        "historyFileInIconFgSelected",
        "historyFileThumbIconFgSelected",
        "historyFileInRadialFgSelected",
    ],
    names: true,
};

const OUTGOING_SIDE: SideKeys = SideKeys {
    bg: "msgOutBg",
    bg_selected: "msgOutBgSelected",
    text: "historyTextOutFg",
    text_selected: "historyTextOutFgSelected",
    selected: [
        Some("historyLinkOutFgSelected"),
        Some("msgOutServiceFgSelected"),
        Some("msgOutDateFgSelected"),
        Some("msgOutMonoFgSelected"),
        Some("msgOutReplyBarSelColor"),
        Some("historyOutIconFgSelected"),
    ],
    unselected: [
        Some("historyLinkOutFg"),
        Some("msgOutServiceFg"),
        Some("msgOutDateFg"),
        Some("msgOutMonoFg"),
        Some("msgOutReplyBarColor"),
        Some("historyOutIconFg"),
    ],
    bg_dependents: &["historyFileOutIconFg", "historyFileOutRadialFg"],
    dependents: &[
        "historyFileOutIconFgSelected",
        "historyFileOutRadialFgSelected",
    ],
    names: false,
};

const PEER_NAMES: [&str; 8] = [
    "historyPeer1NameFg",
    "historyPeer2NameFg",
    "historyPeer3NameFg",
    "historyPeer4NameFg",
    "historyPeer5NameFg",
    "historyPeer6NameFg",
    "historyPeer7NameFg",
    "historyPeer8NameFg",
];

const PEER_NAMES_SELECTED: [&str; 8] = [
    "historyPeer1NameFgSelected",
    "historyPeer2NameFgSelected",
    "historyPeer3NameFgSelected",
    "historyPeer4NameFgSelected",
    "historyPeer5NameFgSelected",
    "historyPeer6NameFgSelected",
    "historyPeer7NameFgSelected",
    "historyPeer8NameFgSelected",
];

fn global_floors(dark: bool) -> Floors {
    let selected = if dark {
        [3.0, 3.0, 3.0, 3.0, 2.5, 3.0]
    } else {
        [2.5, 2.2, 1.7, 2.4, 2.0, 1.7]
    };
    let names = if dark { 3.0 } else { 1.8 };
    Floors {
        bubble_text: 4.5,
        text: 4.5,
        separation: if dark { 0.10 } else { 0.06 },
        selected,
        unselected: selected,
        names,
        names_unselected: names,
    }
}

fn nudged_hue(hue_deg: f64, lightness: f64) -> f64 {
    if !(OLIVE_FROM..=OLIVE_TO).contains(&hue_deg) || lightness >= OLIVE_LIGHTNESS {
        return hue_deg;
    }
    let progress = ((OLIVE_LIGHTNESS - lightness) / OLIVE_RANGE).min(1.0);
    let target = if hue_deg < OLIVE_SPLIT {
        OLIVE_WARM
    } else {
        OLIVE_GREEN
    };
    hue_deg + (target - hue_deg) * progress
}

fn make_lch(lightness: f64, chroma_val: f64, hue_deg: f64, nudge: bool) -> Color {
    from_lch(
        lightness,
        chroma_val,
        if nudge {
            nudged_hue(hue_deg, lightness)
        } else {
            hue_deg
        },
    )
}

fn composite_over(over: Color, under: Color) -> Color {
    if over.a == 255 {
        return over;
    }
    let alpha = f64::from(over.a) / 255.0;
    let mix = |a: u8, b: u8| -> u8 {
        (f64::from(a) * alpha + f64::from(b) * (1.0 - alpha))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgb(
        mix(over.r, under.r),
        mix(over.g, under.g),
        mix(over.b, under.b),
    )
}

fn contrast_over(over: Color, under: Color) -> f64 {
    count_contrast(composite_over(over, under), under)
}

fn relative_luminance(color: Color) -> f64 {
    let map = |v: f64| -> f64 {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    map(color.red_f()) * 0.2126 + map(color.green_f()) * 0.7152 + map(color.blue_f()) * 0.0722
}

fn delta_e(a: Color, b: Color) -> f64 {
    let first = to_lab(a);
    let second = to_lab(b);
    ((first.lightness - second.lightness).powi(2)
        + (first.a - second.a).powi(2)
        + (first.b - second.b).powi(2))
    .sqrt()
}

fn nearest_feasible<F: Fn(Color) -> bool>(
    lightness: f64,
    chroma_val: f64,
    hue_deg: f64,
    ok: F,
    nudge: bool,
) -> Option<Color> {
    let first = make_lch(lightness, chroma_val, hue_deg, nudge);
    if ok(first) {
        return Some(first);
    }
    for step in 1..=READABILITY_STEPS {
        for sign in [1.0, -1.0] {
            let now = lightness + sign * (step as f64) * READABILITY_STEP;
            if !(0.05..=0.98).contains(&now) {
                continue;
            }
            let color = make_lch(now, chroma_val, hue_deg, nudge);
            if ok(color) {
                return Some(color);
            }
        }
    }
    None
}

fn ensure_readability_contrast(
    color: Color,
    backgrounds: &[Color],
    floor: f64,
    nudge: bool,
) -> Color {
    let ok = |now: Color| -> bool {
        backgrounds
            .iter()
            .all(|&bg| contrast_over(now, bg) >= floor)
    };
    if ok(color) || backgrounds.is_empty() {
        return color;
    }
    let lab = to_lab(color);
    let c = chroma(lab);
    let h = hue(lab);
    let up = relative_luminance(composite_over(color, backgrounds[0]))
        >= relative_luminance(backgrounds[0]);
    let scan = |direction: i32| -> Option<Color> {
        for step in 1..=READABILITY_STEPS {
            let lightness = lab.lightness + f64::from(direction) * (step as f64) * READABILITY_STEP;
            if !(0.0..=1.0).contains(&lightness) {
                break;
            }
            let now = make_lch(lightness, c, h, nudge).with_alpha(color.a);
            if ok(now) {
                return Some(now);
            }
        }
        None
    };
    let opacify = |direction: i32| -> Option<Color> {
        let edge = make_lch(if direction > 0 { 1.0 } else { 0.0 }, c, h, nudge);
        let mut alpha = color.a;
        while alpha < 255 {
            alpha = alpha.saturating_add(ALPHA_STEP);
            let now = edge.with_alpha(alpha);
            if ok(now) {
                return Some(now);
            }
        }
        None
    };
    let dirs = if up { [1, -1] } else { [-1, 1] };
    for dir in dirs {
        if let Some(found) = scan(dir) {
            return found;
        }
        if let Some(opaque) = opacify(dir) {
            return opaque;
        }
    }
    color
}

fn choose_selected_bg(
    stops: &[Color],
    selected: Color,
    text: Color,
    dark: bool,
    floors: &Floors,
    derive: bool,
) -> Color {
    let ok = |color: Color| -> bool {
        if contrast_over(text, color) < floors.text {
            return false;
        }
        stops
            .iter()
            .all(|&stop| delta_e(color, stop) >= floors.separation)
    };
    if !derive && ok(selected) {
        return selected;
    }
    let lab = to_lab(selected);
    let mut lightness = lab.lightness;
    let mut c = chroma(lab);
    let mut h = hue(lab);
    if derive && !stops.is_empty() {
        let mut mean = Oklab::default();
        let mut lightest = 0.0f64;
        for &stop in stops {
            let value = to_lab(stop);
            mean.lightness += value.lightness;
            mean.a += value.a;
            mean.b += value.b;
            lightest = lightest.max(value.lightness);
        }
        let n = stops.len() as f64;
        mean.lightness /= n;
        mean.a /= n;
        mean.b /= n;
        let mean_chroma = chroma(mean);
        if mean_chroma >= MIN_CHROMA {
            c = (mean_chroma * OUTGOING_CHROMA_SCALE)
                .clamp(OUTGOING_CHROMA_MIN, OUTGOING_CHROMA_MAX);
            h = hue(mean);
        }
        lightness = lightest + OUTGOING_LIFT;
    }
    nearest_feasible(lightness, c, h, ok, dark)
        .map_or(selected, |found| found.with_alpha(selected.a))
}

fn get_or_black(palette: &Palette, name: &str) -> Color {
    palette.color_by_name(name).unwrap_or(Color::BLACK)
}

fn side_stops(palette: &Palette, side: &SideKeys, gradient: &[Color]) -> Vec<Color> {
    if gradient.len() > 1 {
        gradient.to_vec()
    } else {
        vec![get_or_black(palette, side.bg)]
    }
}

fn measure_floors(palette: &Palette, side: &SideKeys, stops: &[Color]) -> Floors {
    let selected = get_or_black(palette, side.bg_selected);
    let max = f64::MAX;
    let mut result = Floors {
        bubble_text: max,
        text: contrast_over(get_or_black(palette, side.text_selected), selected),
        separation: max,
        selected: [max; ROLES],
        unselected: [max; ROLES],
        names: max,
        names_unselected: max,
    };
    for &stop in stops {
        result.bubble_text = result
            .bubble_text
            .min(contrast_over(get_or_black(palette, side.text), stop));
        result.separation = result.separation.min(delta_e(selected, stop));
    }
    for i in 0..ROLES {
        if let (Some(sel_key), Some(unsel_key)) = (side.selected[i], side.unselected[i]) {
            result.selected[i] = contrast_over(get_or_black(palette, sel_key), selected);
            let mut min_u = max;
            for &stop in stops {
                min_u = min_u.min(contrast_over(get_or_black(palette, unsel_key), stop));
            }
            result.unselected[i] = min_u;
        }
    }
    if side.names {
        for i in 0..PEER_NAMES.len() {
            let name_c = get_or_black(palette, PEER_NAMES[i]);
            let name_sel = get_or_black(palette, PEER_NAMES_SELECTED[i]);
            result.names = result.names.min(contrast_over(name_sel, selected));
            if let Some(&first_stop) = stops.first() {
                result.names_unselected = result
                    .names_unselected
                    .min(contrast_over(name_c, first_stop));
            }
        }
    }
    result
}

fn lowest_floors(mut a: Floors, b: &Floors) -> Floors {
    a.bubble_text = a.bubble_text.min(b.bubble_text);
    a.text = a.text.min(b.text);
    a.separation = a.separation.min(b.separation);
    for i in 0..ROLES {
        a.selected[i] = a.selected[i].min(b.selected[i]);
        a.unselected[i] = a.unselected[i].min(b.unselected[i]);
    }
    a.names = a.names.min(b.names);
    a.names_unselected = a.names_unselected.min(b.names_unselected);
    a
}

fn replace_with_dependents(palette: &mut Palette, key: &str, now: Color, dependents: &[&str]) {
    let was = get_or_black(palette, key);
    if now == was {
        return;
    }
    palette.set_color(key, now);
    for &dep in dependents {
        if get_or_black(palette, dep) == was {
            palette.set_color(dep, now);
        }
    }
}

fn set_contrasted(palette: &mut Palette, key: &str, backgrounds: &[Color], floor: f64) {
    let cur = get_or_black(palette, key);
    let now = ensure_readability_contrast(cur, backgrounds, floor, true);
    if now != cur {
        palette.set_color(key, now);
    }
}

fn ensure_side_readable(
    palette: &mut Palette,
    side: &SideKeys,
    args: &BubblesReadabilityArgs<'_>,
    outgoing: bool,
) {
    let gradient: &[Color] = if outgoing {
        &args.outgoing_gradient
    } else {
        &[]
    };
    let global = global_floors(args.dark);
    let floors = match args.reference {
        Some(reference) => {
            let ref_stops = side_stops(reference, side, &[]);
            lowest_floors(global, &measure_floors(reference, side, &ref_stops))
        }
        None => global,
    };
    if gradient.len() < 2 {
        let bubble = get_or_black(palette, side.bg);
        let text = get_or_black(palette, side.text);
        let now = ensure_readability_contrast(bubble, &[text], floors.bubble_text, true);
        replace_with_dependents(palette, side.bg, now, side.bg_dependents);
    }
    let stops = side_stops(palette, side, gradient);
    let selected = get_or_black(palette, side.bg_selected);
    let text_sel = get_or_black(palette, side.text_selected);
    let now = choose_selected_bg(
        &stops,
        selected,
        text_sel,
        args.dark,
        &floors,
        outgoing && args.derive_outgoing_selection,
    );
    replace_with_dependents(palette, side.bg_selected, now, side.dependents);

    for i in 0..ROLES {
        if let Some(key) = side.selected[i] {
            set_contrasted(palette, key, &[now], floors.selected[i]);
        }
    }
    for i in 0..ROLES {
        if let Some(key) = side.unselected[i] {
            set_contrasted(palette, key, &stops, floors.unselected[i]);
        }
    }
    if side.names {
        for i in 0..PEER_NAMES.len() {
            set_contrasted(palette, PEER_NAMES_SELECTED[i], &[now], floors.names);
            set_contrasted(palette, PEER_NAMES[i], &stops, floors.names_unselected);
        }
    }
}

/// Ensures incoming and outgoing message bubble text, links, dates, reply bars,
/// and peer names remain readable after accent colorization (`Ui::EnsureBubblesReadable`).
pub fn ensure_bubbles_readable(palette: &mut Palette, args: &BubblesReadabilityArgs<'_>) {
    ensure_side_readable(palette, &INCOMING_SIDE, args, false);
    ensure_side_readable(palette, &OUTGOING_SIDE, args, true);
}

/// Ensures a peer name color is readable against `background` (`Ui::EnsurePeerNameReadable`).
#[must_use]
pub fn ensure_peer_name_readable(color: Color, background: Color, dark: bool) -> Color {
    ensure_readability_contrast(color, &[background], global_floors(dark).names, true)
}

/// Darkens gradient stops that do not meet 4.5:1 contrast under white text
/// (`Ui::DarkenUnderWhiteText`).
#[must_use]
pub fn darken_under_white_text(mut colors: Vec<Color>) -> Vec<Color> {
    for color in &mut colors {
        if contrast_over(Color::WHITE, *color) >= WHITE_TEXT_CONTRAST {
            continue;
        }
        let lab = to_lab(*color);
        let c = chroma(lab);
        let h = hue(lab);
        for step in 1..=READABILITY_STEPS {
            let now = make_lch(lab.lightness - (step as f64) * READABILITY_STEP, c, h, true);
            if contrast_over(Color::WHITE, now) >= WHITE_TEXT_CONTRAST {
                *color = now;
                break;
            }
        }
    }
    colors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_entry_count_matches_colors_palette() {
        // Count non-empty non-comment lines in ../../resources/colors.palette
        let src = include_str!("../../../resources/colors.palette");
        let file_entries = src
            .lines()
            .filter(|line| {
                let code = line.split("//").next().unwrap_or("").trim();
                !code.is_empty()
            })
            .count();
        assert_eq!(Palette::ENTRY_COUNT, file_entries);
        assert_eq!(Palette::COUNT, file_entries + 2);
        assert_eq!(Palette::ROWS.len(), file_entries);
    }

    #[test]
    fn default_palette_values_and_fallbacks_resolved() {
        let p = Palette::default();
        assert!(p.is_ready());
        assert_eq!(p.transparent, Color::rgba(255, 255, 255, 0));
        assert_eq!(p.white, Color::rgba(255, 255, 255, 255));
        assert_eq!(p.window_bg, Color::rgb(0xff, 0xff, 0xff));
        assert_eq!(p.window_fg, Color::rgb(0x00, 0x00, 0x00));
        assert_eq!(p.window_fg_over, p.window_fg);
        assert_eq!(p.window_bg_active, Color::rgb(0x40, 0xa7, 0xe3));
        assert_eq!(p.active_button_bg, p.window_bg_active);
        assert_eq!(p.shadow_fg, Color::rgba(0x00, 0x00, 0x00, 0x18));
        assert!(!p.is_dark_dialogs());
        assert!(!p.is_dark_palette());

        // Lookup by original camelCase name and by index
        let idx = Palette::index_of_name("windowBgActive").unwrap();
        assert_eq!(p.color_at_index(idx), Some(Color::rgb(0x40, 0xa7, 0xe3)));
        assert_eq!(p.color_by_name("windowBgActive"), Some(p.window_bg_active));
        assert_eq!(Palette::name_at_index(idx), Some("windowBgActive"));
        assert_eq!(Palette::index_of_name("transparent"), None);
        assert_eq!(Palette::index_of_name("white"), None);
    }

    #[test]
    fn fallback_resolution_propagates_loaded_overrides() {
        let mut p = Palette::new_uninitialized();
        let custom_bg = Color::rgb(0x10, 0x20, 0x30);
        assert_eq!(p.set_color("windowBg", custom_bg), SetResult::Ok);
        assert_eq!(p.set_color("windowBg", custom_bg), SetResult::Duplicate);
        // lightButtonBg: windowBg; -> should inherit custom_bg
        // menuBg: windowBg; -> should inherit custom_bg
        // emojiPanCategories: #f7f7f7 | windowBg; -> explicit fallback to Loaded windowBg!
        p.finalize();
        assert_eq!(p.window_bg, custom_bg);
        assert_eq!(p.light_button_bg, custom_bg);
        assert_eq!(p.menu_bg, custom_bg);
        assert_eq!(p.emoji_pan_categories, custom_bg);
        // Contrast enforcement ensured windowBoldFg contrasts with dark windowBg
        assert!(crate::color::good_for_contrast(
            p.window_bold_fg,
            p.window_bg
        ));
    }

    #[test]
    fn binary_save_load_round_trip() {
        let p = Palette::default();
        let bytes = p.save();
        assert_eq!(bytes.len(), Palette::COUNT * 4);
        let mut loaded = Palette::new_uninitialized();
        assert!(loaded.load(&bytes));
        assert_eq!(loaded, p);
        assert!(!loaded.load(&bytes[..10]));
    }

    #[test]
    fn tg_style_color_resolver_for_embedded_and_accent_palettes() {
        use crate::embedded::{EmbeddedType, default_accent_colors, prepare_embedded_palette};
        use tg_style::{ColorRef, ColorResolver};

        let day = Palette::default();
        assert_eq!(
            ColorRef::new("windowBg").resolve(&day),
            tg_style::Color::rgb(0xff, 0xff, 0xff)
        );
        assert_eq!(
            ColorRef::new("windowBgActive").resolve(&day),
            tg_style::Color::rgb(0x40, 0xa7, 0xe3)
        );
        assert_eq!(
            ColorRef::TRANSPARENT.resolve(&day),
            tg_style::Color::TRANSPARENT
        );
        assert_eq!(ColorRef::WHITE.resolve(&day), tg_style::Color::WHITE);
        assert_eq!(day.resolve("doesNotExist"), None);

        // Night theme resolves dark background via tg_style::ColorRef
        let night = prepare_embedded_palette(EmbeddedType::Night, None).unwrap();
        let night_bg = ColorRef::new("windowBg").resolve(&night);
        assert_ne!(night_bg, tg_style::Color::WHITE);
        assert!(night_bg.r < 64 && night_bg.g < 64 && night_bg.b < 64);

        // Accent colorization propagates through ColorResolver
        let green = default_accent_colors(EmbeddedType::Default)[1];
        let accented = prepare_embedded_palette(EmbeddedType::Default, Some(green)).unwrap();
        let day_active = ColorRef::new("activeButtonBg").resolve(&day);
        let acc_active = ColorRef::new("activeButtonBg").resolve(&accented);
        assert_ne!(day_active, acc_active);

        // Real tg_style::Icon rasterizes using tg_theme::Palette as ColorResolver
        let icon_img = tg_style::st::box_title_close
            .icon
            .rasterize_at(tg_style::Scale::S100, 1, &night)
            .unwrap();
        assert!(icon_img.width > 0 && icon_img.height > 0);
    }
}
