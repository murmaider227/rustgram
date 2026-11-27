//! Overflow-safe bounds of text entities.
//!
//! Entity offsets and lengths come from the server unvalidated (TGB-3081), so `offset + length`
//! in i32 overflows on hostile values: a panic in debug builds, a wrapped negative end in release
//! that turns into a huge `usize` after a cast (TGB-3083). Every end computation goes through
//! [`entity_end`].

use std::ops::Range;

use tg_text::EntityInText;

/// End of `entity` in UTF-16 code units, `offset + length` saturated at the i32 bounds.
#[must_use]
pub fn entity_end(entity: &EntityInText) -> i32 {
    entity.offset().saturating_add(entity.length())
}

/// UTF-16 range of `entity` clamped to a text of `text_utf16_len` code units, or `None` when
/// nothing of it lies inside the text.
#[must_use]
pub fn entity_utf16_range(entity: &EntityInText, text_utf16_len: usize) -> Option<Range<usize>> {
    let clamp = |value: i32| usize::try_from(value.max(0)).map_or(0, |v| v.min(text_utf16_len));
    let start = clamp(entity.offset());
    let end = clamp(entity_end(entity));
    (end > start).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_text::EntityType;

    #[test]
    fn end_saturates_on_hostile_bounds() {
        let hostile = EntityInText::new(EntityType::Bold, i32::MAX, 2);
        assert_eq!(entity_end(&hostile), i32::MAX);
        let negative = EntityInText::new(EntityType::Bold, i32::MIN, -2);
        assert_eq!(entity_end(&negative), i32::MIN);
        assert_eq!(entity_end(&EntityInText::new(EntityType::Bold, 3, 4)), 7);
    }

    #[test]
    fn range_is_clamped_to_the_text() {
        let e = |offset, length| EntityInText::new(EntityType::Bold, offset, length);
        assert_eq!(entity_utf16_range(&e(2, 3), 10), Some(2..5));
        assert_eq!(entity_utf16_range(&e(8, 30), 10), Some(8..10));
        assert_eq!(entity_utf16_range(&e(-4, 6), 10), Some(0..2));
        assert_eq!(entity_utf16_range(&e(i32::MAX, 2), 10), None);
        assert_eq!(entity_utf16_range(&e(3, -1), 10), None);
    }
}
