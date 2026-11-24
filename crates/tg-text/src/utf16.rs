//! UTF-16 <-> UTF-8 offset conversion utilities.
//!
//! Telegram entity offsets and lengths (as well as Qt `QString` indices in tdesktop)
//! are measured in UTF-16 code units, whereas Rust `&str` is UTF-8. Every entity and
//! tag offset in `tg-text` uses UTF-16 code units for wire and tdesktop compatibility.

use std::ops::Range;

/// Returns the length of `s` in UTF-16 code units.
#[inline]
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Converts a UTF-16 code unit offset in `s` to a UTF-8 byte offset.
///
/// Returns `None` if `utf16_offset` is beyond the end of `s` or lands in the middle
/// of a UTF-16 surrogate pair (i.e., on the low surrogate of a non-BMP character).
pub fn utf16_to_utf8_offset(s: &str, utf16_offset: usize) -> Option<usize> {
    let mut cur_u16 = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if cur_u16 == utf16_offset {
            return Some(byte_idx);
        }
        let step = ch.len_utf16();
        if cur_u16 + step > utf16_offset {
            // Lands inside a surrogate pair.
            return None;
        }
        cur_u16 += step;
    }
    if cur_u16 == utf16_offset {
        Some(s.len())
    } else {
        None
    }
}

/// Converts a UTF-16 code unit offset in `s` to a UTF-8 byte offset, snapping down
/// to the start of the enclosing character if `utf16_offset` lands inside a surrogate
/// pair, and clamping to `s.len()` if out of bounds.
pub fn utf16_to_utf8_floor(s: &str, utf16_offset: usize) -> usize {
    let mut cur_u16 = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        let step = ch.len_utf16();
        if cur_u16 + step > utf16_offset {
            return byte_idx;
        }
        cur_u16 += step;
    }
    s.len()
}

/// Converts a UTF-16 code unit offset in `s` to a UTF-8 byte offset, snapping up
/// to the end of the enclosing character if `utf16_offset` lands inside a surrogate
/// pair, and clamping to `s.len()` if out of bounds.
pub fn utf16_to_utf8_ceil(s: &str, utf16_offset: usize) -> usize {
    let mut cur_u16 = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if cur_u16 >= utf16_offset {
            return byte_idx;
        }
        cur_u16 += ch.len_utf16();
    }
    s.len()
}

/// Converts a UTF-8 byte offset in `s` to a UTF-16 code unit offset.
///
/// Returns `None` if `utf8_offset` is out of bounds or not on a UTF-8 character boundary.
pub fn utf8_to_utf16_offset(s: &str, utf8_offset: usize) -> Option<usize> {
    if utf8_offset > s.len() || !s.is_char_boundary(utf8_offset) {
        return None;
    }
    Some(s[..utf8_offset].encode_utf16().count())
}

/// Converts a UTF-16 `(offset, length)` pair to a UTF-8 byte `Range<usize>`.
///
/// Returns `None` if the range overflows, is out of bounds, or splits a surrogate pair.
pub fn utf16_range_to_utf8(s: &str, offset: usize, length: usize) -> Option<Range<usize>> {
    let end_u16 = offset.checked_add(length)?;
    let start_u8 = utf16_to_utf8_offset(s, offset)?;
    if length == 0 {
        return Some(start_u8..start_u8);
    }
    let rel_end = utf16_to_utf8_offset(&s[start_u8..], end_u16 - offset)?;
    Some(start_u8..start_u8 + rel_end)
}

/// Converts a UTF-8 byte range in `s` to a UTF-16 `(offset, length)` pair.
///
/// Returns `None` if `range.start > range.end`, out of bounds, or not on UTF-8 char boundaries.
pub fn utf8_range_to_utf16(s: &str, range: Range<usize>) -> Option<(usize, usize)> {
    if range.start > range.end
        || range.end > s.len()
        || !s.is_char_boundary(range.start)
        || !s.is_char_boundary(range.end)
    {
        return None;
    }
    let start_u16 = s[..range.start].encode_utf16().count();
    let len_u16 = s[range.start..range.end].encode_utf16().count();
    Some((start_u16, len_u16))
}

/// Slices `s` by UTF-16 `offset` and `length`.
///
/// Returns `None` if out of bounds or splitting a surrogate pair.
#[inline]
pub fn slice_utf16(s: &str, offset: usize, length: usize) -> Option<&str> {
    let range = utf16_range_to_utf8(s, offset, length)?;
    Some(&s[range])
}

/// Encodes `s` into a vector of UTF-16 code units.
#[inline]
pub fn to_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Decodes a UTF-16 slice into a `String`, replacing unpaired surrogates with U+FFFD.
#[inline]
pub fn from_utf16_lossy(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// Precomputed UTF-8 <-> UTF-16 offset index for O(log n) bulk conversions on one string.
#[derive(Debug, Clone)]
pub struct OffsetMapper {
    /// Sorted pairs of `(utf8_byte_offset, utf16_unit_offset)` at every character boundary,
    /// including `(s.len(), utf16_len(s))`.
    entries: Vec<(usize, usize)>,
}

impl OffsetMapper {
    /// Builds an `OffsetMapper` for `s`.
    pub fn new(s: &str) -> Self {
        let mut entries = Vec::with_capacity(s.len().min(64) + 1);
        let mut u16_pos = 0usize;
        for (byte_idx, ch) in s.char_indices() {
            entries.push((byte_idx, u16_pos));
            u16_pos += ch.len_utf16();
        }
        entries.push((s.len(), u16_pos));
        Self { entries }
    }

    /// Total UTF-8 byte length of the indexed string.
    #[inline]
    pub fn utf8_len(&self) -> usize {
        self.entries.last().map_or(0, |&(b, _)| b)
    }

    /// Total UTF-16 code unit length of the indexed string.
    #[inline]
    pub fn utf16_len(&self) -> usize {
        self.entries.last().map_or(0, |&(_, u)| u)
    }

    /// Converts a UTF-8 byte offset to a UTF-16 code unit offset in O(log n).
    pub fn utf8_to_utf16(&self, utf8_offset: usize) -> Option<usize> {
        self.entries
            .binary_search_by_key(&utf8_offset, |&(b, _)| b)
            .ok()
            .map(|idx| self.entries[idx].1)
    }

    /// Converts a UTF-16 code unit offset to a UTF-8 byte offset in O(log n).
    /// Returns `None` if out of bounds or splitting a surrogate pair.
    pub fn utf16_to_utf8(&self, utf16_offset: usize) -> Option<usize> {
        self.entries
            .binary_search_by_key(&utf16_offset, |&(_, u)| u)
            .ok()
            .map(|idx| self.entries[idx].0)
    }

    /// Converts a UTF-16 offset to a UTF-8 offset, snapping down on surrogate split and clamping.
    pub fn utf16_to_utf8_floor(&self, utf16_offset: usize) -> usize {
        match self
            .entries
            .binary_search_by_key(&utf16_offset, |&(_, u)| u)
        {
            Ok(idx) => self.entries[idx].0,
            Err(0) => 0,
            Err(idx) => self.entries[idx - 1].0,
        }
    }

    /// Converts a UTF-16 offset to a UTF-8 offset, snapping up on surrogate split and clamping.
    pub fn utf16_to_utf8_ceil(&self, utf16_offset: usize) -> usize {
        match self
            .entries
            .binary_search_by_key(&utf16_offset, |&(_, u)| u)
        {
            Ok(idx) => self.entries[idx].0,
            Err(idx) if idx < self.entries.len() => self.entries[idx].0,
            Err(_) => self.utf8_len(),
        }
    }

    /// Converts a UTF-8 byte range to a UTF-16 `(offset, length)` pair.
    pub fn utf8_range_to_utf16(&self, range: Range<usize>) -> Option<(usize, usize)> {
        if range.start > range.end {
            return None;
        }
        let start = self.utf8_to_utf16(range.start)?;
        let end = self.utf8_to_utf16(range.end)?;
        Some((start, end - start))
    }

    /// Converts a UTF-16 `(offset, length)` pair to a UTF-8 byte range.
    pub fn utf16_range_to_utf8(&self, offset: usize, length: usize) -> Option<Range<usize>> {
        let end_u16 = offset.checked_add(length)?;
        let start = self.utf16_to_utf8(offset)?;
        let end = self.utf16_to_utf8(end_u16)?;
        Some(start..end)
    }
}
