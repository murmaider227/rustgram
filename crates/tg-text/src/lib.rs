//! `tg-text`: Telegram Desktop's pure text model, entity/markdown/link parsing, tag conversion,
//! text utilities, UTF-16 offset utilities, and emoji database/suggestions.
//!
//! Ported from `Telegram/lib_ui/ui/text/*`, `Telegram/lib_base/base/qthelp_url.*`,
//! `Telegram/lib_ui/ui/emoji_config.*`, `Telegram/lib_ui/emoji_suggestions/*`,
//! `Telegram/codegen/codegen/emoji/*`, `Telegram/SourceFiles/ui/text/*`, and
//! `Telegram/SourceFiles/chat_helpers/message_field.*`.

pub mod char_class;
pub mod emoji;
pub mod entity;
pub mod highlight;
pub mod markdown;
pub mod message_field;
pub mod parse;
pub mod tags;
pub mod url;
pub mod utf16;
pub mod utilities;

pub use char_class::is_diacritic;
pub use entity::{
    ALL_TEXT_SELECTION, EntitiesInText, EntityInText, EntityLinkData, EntityLinkShown, EntityType,
    FormattedDateFlags, MentionNameFields, ReplacedTextRanges, TextForMimeData, TextForMimeDataTag,
    TextParseFlags, TextSelection, TextWithEntities, deserialize_formatted_date_data,
    entity_replaces_text, mention_name_data_from_fields, mention_name_data_to_fields,
    merge_and_sort_entities, serialize_formatted_date_data, sort_entities,
};
pub use highlight::{
    HIGHLIGHT_CACHE_LIMIT, HighlightedSpan, SyntaxTokenKind, highlight_code_entities,
    highlight_code_spans, is_supported_language, lookup_language_alias, try_highlight_syntax,
};
pub use markdown::{
    MarkdownTag, MarkdownTagAccumulator, apply_markdown, apply_markdown_with_tags,
    find_markdown_tags, parse_markdown, parse_markdown_in_place, read_pre_language_name,
};
pub use message_field::prepare_mention_tag;
pub use parse::{
    PrepareTextOption, apply_server_cleaning, cut_part, escape_for_rich_parsing, name_sort_key,
    parse_entities, parse_entities_in_place, prepare_for_sending, prepare_for_sending_str,
    prepare_search_words, remove_accents, remove_emoji, single_line, single_line_with_entities,
    trim,
};
pub use tags::{
    MENTION_TAG_START, TextTag, TextTags, TextWithTags, convert_entities_to_text_tags,
    convert_text_tags_to_entities, deserialize_tags, serialize_tags, serialize_tags_size,
    simplify_tags,
};
pub use utf16::{
    OffsetMapper, from_utf16_lossy, slice_utf16, to_utf16, utf8_range_to_utf16,
    utf8_to_utf16_offset, utf16_len, utf16_range_to_utf8, utf16_to_utf8_ceil, utf16_to_utf8_floor,
    utf16_to_utf8_offset,
};
