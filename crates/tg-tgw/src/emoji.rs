//! Unicode 17.0 emoji cell detection, advance/box metrics, and tile wait tracking for widgets.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_emoji.h/.cpp` and `tgw_emoji_images.h/.cpp`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Calibrated horizontal advance multiplier (`kEmojiAdvance = 1.0` in `tgw_emoji.cpp:28`).
pub const EMOJI_ADVANCE: f64 = 1.0;

/// Calibrated vertical baseline rise (`kEmojiRise = 0.35` in `tgw_emoji.cpp:29`).
pub const EMOJI_RISE: f64 = 0.35;

const TEXT_PRESENTATION_SELECTOR: char = '\u{FE0E}';
const EMOJI_PRESENTATION_SELECTOR: char = '\u{FE0F}';
const ENCLOSING_KEYCAP: char = '\u{20E3}';
const ZERO_WIDTH_JOINER: char = '\u{200D}';
const FIRST_PICTOGRAPH: u32 = 0x1F000;
const LAST_PICTOGRAPH: u32 = 0x1FAFF;
const FIRST_REGIONAL: u32 = 0x1F1E6;
const LAST_REGIONAL: u32 = 0x1F1FF;
const FIRST_TAG: u32 = 0xE0020;
const LAST_TAG: u32 = 0xE007F;
const SKIN_TONE_FIRST: u32 = 0x1F3FB;
const SKIN_TONE_LAST: u32 = 0x1F3FF;

/// All 81 Unicode 17.0 `Emoji_Presentation` code point ranges (`kEmojiPresentation` in `tgw_emoji.cpp:57-139`).
pub const EMOJI_PRESENTATION_RANGES: &[(u32, u32)] = &[
    (0x231A, 0x231B),
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x2693, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x1F004, 0x1F004),
    (0x1F0CF, 0x1F0CF),
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F1E6, 0x1F1FF),
    (0x1F201, 0x1F201),
    (0x1F21A, 0x1F21A),
    (0x1F22F, 0x1F22F),
    (0x1F232, 0x1F236),
    (0x1F238, 0x1F23A),
    (0x1F250, 0x1F251),
    (0x1F300, 0x1F320),
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D8),
    (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA70, 0x1FA7C),
    (0x1FA80, 0x1FA8A),
    (0x1FA8E, 0x1FAC6),
    (0x1FAC8, 0x1FAC8),
    (0x1FACD, 0x1FADC),
    (0x1FADF, 0x1FAEA),
    (0x1FAEF, 0x1FAF8),
];

/// Returns `true` if `code` has Unicode `Emoji_Presentation=Yes` (`IsEmojiPresentation`).
#[must_use]
pub fn is_emoji_presentation(code: u32) -> bool {
    let idx = EMOJI_PRESENTATION_RANGES.partition_point(|&(first, _)| first <= code);
    idx > 0 && code <= EMOJI_PRESENTATION_RANGES[idx - 1].1
}

/// Returns `true` if `code` is a Regional Indicator Symbol (`U+1F1E6..=U+1F1FF`).
#[must_use]
pub const fn is_regional_indicator(code: u32) -> bool {
    code >= FIRST_REGIONAL && code <= LAST_REGIONAL
}

/// Returns `true` if `code` is a ZWJ or Unicode Tag code point (`IsJoining`).
#[must_use]
pub const fn is_joining(code: u32) -> bool {
    code == (ZERO_WIDTH_JOINER as u32) || (code >= FIRST_TAG && code <= LAST_TAG)
}

/// Returns `true` if `ch` is a strong right-to-left character (Hebrew, Arabic, Syriac, Thaana,
/// RLM/RLE/RLO/RLI) matching `HasRightToLeft` in `tgw_emoji.cpp:194-204`.
#[must_use]
pub const fn is_rtl_char(ch: char) -> bool {
    let c = ch as u32;
    matches!(
        c,
        0x0590..=0x08FF
            | 0x200F
            | 0x202B
            | 0x202E
            | 0x2067
            | 0xFB1D..=0xFDFF
            | 0xFE70..=0xFEFF
            | 0x10800..=0x10FFF
            | 0x1E800..=0x1EFFF
    )
}

fn may_start_keycap_or_text_emoji(ch: char) -> bool {
    ch.is_ascii_digit() || matches!(ch, '#' | '*' | '©' | '®') || (ch as u32) >= 0x2000
}

/// Located emoji cell within a UTF-8 string (`Tgw::EmojiCell` in `tgw_emoji.h:16-20`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiCell {
    /// Start byte offset in the source string.
    pub from: usize,
    /// End byte offset (exclusive) in the source string.
    pub till: usize,
    /// `true` if resolved to a known emoji sequence, `false` for unpaired regional indicator.
    pub resolved: bool,
}

/// Bounding rectangle for an emoji glyph (`EmojiBox` in `tgw_emoji.cpp:292-295`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmojiRect {
    /// Left X coordinate.
    pub x: f64,
    /// Top Y coordinate relative to baseline.
    pub y: f64,
    /// Square width.
    pub width: f64,
    /// Square height.
    pub height: f64,
}

/// Finds emoji cells in `text` (`Tgw::FindEmojiCells` in `tgw_emoji.cpp:223-263`).
///
/// Matches tdesktop semantics:
/// - ASCII fast path returns empty.
/// - Pairs consecutive Regional Indicator symbols into flags; lone regionals emit `resolved = false`.
/// - Suppresses emoji cells immediately followed by VS15 (`U+FE0E`).
/// - Absorbs trailing skin tone modifiers, VS16 (`U+FE0F`), ZWJ sequences, and tag sequences.
/// - Returns empty if the string contains any strong RTL character (so bidi shaping handles it).
#[must_use]
pub fn find_emoji_cells(text: &str) -> Vec<EmojiCell> {
    if text.is_ascii() {
        return Vec::new();
    }
    if text.chars().any(is_rtl_char) {
        return Vec::new();
    }

    let mut cells = Vec::new();
    let mut regionals = 0usize;
    let mut idx = 0usize;
    let bytes = text.as_bytes();

    while idx < bytes.len() {
        let Some(ch) = text[idx..].chars().next() else {
            break;
        };
        let ch_len = ch.len_utf8();
        let code = ch as u32;

        if is_regional_indicator(code) {
            let next_idx = idx + ch_len;
            let paired = regionals.is_multiple_of(2)
                && text[next_idx..]
                    .chars()
                    .next()
                    .is_some_and(|n| is_regional_indicator(n as u32));
            if paired {
                let second_len = text[next_idx..].chars().next().map_or(0, char::len_utf8);
                let till = next_idx + second_len;
                regionals += 2;
                idx = push_cell(&mut cells, text, idx, till, true);
            } else {
                regionals += 1;
                idx = push_cell(&mut cells, text, idx, next_idx, false);
            }
            continue;
        }
        regionals = 0;

        if !may_start_keycap_or_text_emoji(ch) {
            idx += ch_len;
            continue;
        }

        // Check keycap sequence: [0-9#*] + optional FE0F + U+20E3.
        let rest_after_ch = &text[idx + ch_len..];
        if (ch.is_ascii_digit() || ch == '#' || ch == '*')
            && (rest_after_ch.starts_with(ENCLOSING_KEYCAP)
                || rest_after_ch.starts_with("\u{FE0F}\u{20E3}"))
        {
            let extra = if rest_after_ch.starts_with(ENCLOSING_KEYCAP) {
                ENCLOSING_KEYCAP.len_utf8()
            } else {
                EMOJI_PRESENTATION_SELECTOR.len_utf8() + ENCLOSING_KEYCAP.len_utf8()
            };
            idx = push_cell(&mut cells, text, idx, idx + ch_len + extra, true);
            continue;
        }

        // Check explicit VS16 presentation or Unicode Emoji_Presentation.
        let has_vs16 = rest_after_ch.starts_with(EMOJI_PRESENTATION_SELECTOR);
        if is_emoji_presentation(code) || has_vs16 {
            let mut till = idx + ch_len;
            if has_vs16 {
                till += EMOJI_PRESENTATION_SELECTOR.len_utf8();
            }
            // Absorb skin-tone modifier if present.
            if let Some(mod_ch) = text[till..].chars().next()
                && (SKIN_TONE_FIRST..=SKIN_TONE_LAST).contains(&(mod_ch as u32))
            {
                till += mod_ch.len_utf8();
            }
            // Absorb ZWJ-chained emoji components.
            while let Some(next_ch) = text[till..].chars().next() {
                if next_ch == ZERO_WIDTH_JOINER {
                    let after_zwj = till + ZERO_WIDTH_JOINER.len_utf8();
                    if let Some(joined_ch) = text[after_zwj..].chars().next() {
                        till = after_zwj + joined_ch.len_utf8();
                        if text[till..].starts_with(EMOJI_PRESENTATION_SELECTOR) {
                            till += EMOJI_PRESENTATION_SELECTOR.len_utf8();
                        }
                        if let Some(mod_ch) = text[till..].chars().next()
                            && (SKIN_TONE_FIRST..=SKIN_TONE_LAST).contains(&(mod_ch as u32))
                        {
                            till += mod_ch.len_utf8();
                        }
                        continue;
                    }
                }
                break;
            }
            idx = push_cell(&mut cells, text, idx, till, true);
        } else {
            idx += ch_len;
        }
    }

    cells
}

fn push_cell(
    cells: &mut Vec<EmojiCell>,
    text: &str,
    from: usize,
    mut till: usize,
    resolved: bool,
) -> usize {
    if text[till..].starts_with(TEXT_PRESENTATION_SELECTOR) {
        return till + TEXT_PRESENTATION_SELECTOR.len_utf8();
    }
    if resolved {
        while let Some(ch) = text[till..].chars().next() {
            if is_joining(ch as u32) {
                till += ch.len_utf8();
            } else {
                break;
            }
        }
    }
    cells.push(EmojiCell {
        from,
        till,
        resolved,
    });
    till
}

/// Counts code points that are likely color emoji glyphs (`Tgw::ColorGlyphCandidates` in `tgw_emoji.cpp:265-286`).
#[must_use]
pub fn color_glyph_candidates(text: &str) -> usize {
    if text.is_ascii() {
        return 0;
    }
    text.chars()
        .filter(|&ch| {
            let code = ch as u32;
            (FIRST_PICTOGRAPH..=LAST_PICTOGRAPH).contains(&code)
                || ch == EMOJI_PRESENTATION_SELECTOR
                || ch == ENCLOSING_KEYCAP
                || is_emoji_presentation(code)
        })
        .count()
}

/// Horizontal advance for an emoji cell at `font_size` (`Tgw::EmojiAdvance` in `tgw_emoji.cpp:288-290`).
#[must_use]
pub fn emoji_advance(font_size: f64) -> f64 {
    font_size * EMOJI_ADVANCE
}

/// Glyph bounding box for an emoji cell at `(x, font_size)` (`Tgw::EmojiBox` in `tgw_emoji.cpp:292-295`).
#[must_use]
pub fn emoji_box(x: f64, font_size: f64) -> EmojiRect {
    let side = emoji_advance(font_size);
    EmojiRect {
        x,
        y: -font_size * EMOJI_RISE - side / 2.0,
        width: side,
        height: side,
    }
}

/// Walks `text` split around `cells`, measuring non-emoji spans via `measure_run` and invoking
/// `visit(x, slice, is_emoji)` for each segment (`Tgw::WalkTextWithEmoji` in `tgw_emoji.cpp:297-327`).
pub fn walk_text_with_emoji<M, V>(
    text: &str,
    cells: &[EmojiCell],
    font_size: f64,
    mut measure_run: M,
    mut visit: V,
) -> f64
where
    M: FnMut(&str) -> f64,
    V: FnMut(f64, &str, bool),
{
    let mut x = 0.0;
    let mut from = 0usize;
    for cell in cells {
        if cell.from > from {
            let piece = &text[from..cell.from];
            visit(x, piece, false);
            x += measure_run(piece);
        }
        let emoji_slice = &text[cell.from..cell.till];
        visit(x, emoji_slice, cell.resolved);
        x += emoji_advance(font_size);
        from = cell.till;
    }
    if from < text.len() {
        let piece = &text[from..];
        visit(x, piece, false);
        x += measure_run(piece);
    }
    x
}

// =============================================================================
// Emoji image accounting & tile wait bitset (`tgw_emoji_images.h/.cpp`)
// =============================================================================

/// Pixel side lengths for emoji sprite sheets (`Tgw::EmojiSides` in `tgw_emoji_images.h:16-20`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmojiSides {
    /// Normal inline emoji tile size in device pixels.
    pub normal: i32,
    /// Large emoji tile size.
    pub large: i32,
    /// Isolated single-emoji tile size.
    pub isolated: i32,
}

static NEXT_EMOJI_ACCOUNT_ID: AtomicU64 = AtomicU64::new(1);

/// Per-backend emoji image accounting token (`Tgw::EmojiAccount` in `tgw_emoji_images.h:22-35`).
#[derive(Debug)]
pub struct EmojiAccount {
    id: u64,
    closed: bool,
}

impl Default for EmojiAccount {
    fn default() -> Self {
        Self::new()
    }
}

impl EmojiAccount {
    /// Allocates a unique emoji account ID.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: NEXT_EMOJI_ACCOUNT_ID.fetch_add(1, Ordering::Relaxed),
            closed: false,
        }
    }

    /// Returns this account's unique identifier.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Marks the account closed (releasing pending wait records).
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Returns `true` if [`Self::close`] has been called.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Bitset of pending emoji glyph tiles awaited by a widget frame (`Tgw::EmojiWaits`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EmojiWaits {
    glyphs: Vec<u64>,
    tiles: bool,
}

impl EmojiWaits {
    /// Creates an empty wait set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks that general emoji tile sheets are awaited.
    pub fn wait_tile(&mut self) {
        self.tiles = true;
    }

    /// Marks glyph `index` as awaited.
    pub fn wait_glyph(&mut self, index: usize) {
        let word = index / 64;
        let bit = 1u64 << (index % 64);
        if word >= self.glyphs.len() {
            self.glyphs.resize(word + 1, 0);
        }
        self.glyphs[word] |= bit;
    }

    /// Unions `other` into `self`.
    pub fn merge(&mut self, other: &Self) {
        self.tiles |= other.tiles;
        if other.glyphs.len() > self.glyphs.len() {
            self.glyphs.resize(other.glyphs.len(), 0);
        }
        for (dst, src) in self.glyphs.iter_mut().zip(other.glyphs.iter()) {
            *dst |= *src;
        }
    }

    /// Clears all pending waits.
    pub fn clear(&mut self) {
        self.tiles = false;
        self.glyphs.clear();
    }

    /// Returns `true` if `glyph` (`None` = tile sheet, `Some(i)` = specific glyph) is awaited.
    #[must_use]
    pub fn wants(&self, glyph: Option<usize>) -> bool {
        match glyph {
            None => self.tiles,
            Some(index) => {
                let word = index / 64;
                let bit = 1u64 << (index % 64);
                self.glyphs.get(word).is_some_and(|w| (*w & bit) != 0)
            }
        }
    }
}
