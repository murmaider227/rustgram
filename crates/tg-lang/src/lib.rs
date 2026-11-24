//! `tg-lang`: Telegram Desktop localization, plural rules, cloud language packs,
//! and international country / phone formatting.
//!
//! Port of `Telegram/SourceFiles/lang/`, `Telegram/SourceFiles/countries/`, and
//! `Telegram/codegen/codegen/lang/`.

/// Build-time generated phrase keys, tag indices, default English table, and typed `tr` functions.
pub mod auto {
    include!(concat!(env!("OUT_DIR"), "/lang_auto.rs"));
}

pub mod cloud_manager;
pub mod countries;
pub mod file_parser;
pub mod hardcoded;
pub mod instance;
pub mod keys;
pub mod links;
pub mod strings_parser;
pub mod tag;

pub use tg_storage::qdatastream::{DataStreamError, QDataStreamReader, QDataStreamWriter};

pub use auto::{
    COUNT_TAG_ID, KEYS_COUNT, TAGS_COUNT, get_key_index, get_original_value, get_tag_index,
    is_plural_key, is_tag_replaced, key_ids, key_name, key_tags, tags, tr,
};
pub use cloud_manager::{
    CloudAction, CloudManager, CloudManagerConfig, LangCloudClient, RequestId,
};
pub use countries::{
    CallingCodeInfo, CloudCountriesList, CloudCountry, CloudCountryCode, CountriesCacheData,
    CountriesInstance, CountryInfo, FormatArgs, FormatResult, Naming,
};
pub use file_parser::{FileParser, FileParserError, decode_lang_file_bytes, read_lang_file};
pub use instance::{
    CLOUD_LANG_PACK_NAME, CUSTOM_LANGUAGE_ID, Instance, LangError, LangEvent, LangPackDifference,
    LangPackLanguageInfo, LangPackString, Language, Pack, SerializeLoadOutcome, global_generation,
    global_instance, global_tdata_dir, load_global_from_tdata, push_recent_language,
    push_recent_language_to_tdata, read_recent_languages, read_recent_languages_from_tdata,
    remove_recent_language, remove_recent_language_from_tdata, save_recent_languages,
    save_recent_languages_to_tdata, set_global_tdata_dir, with_current, with_current_mut,
};
pub use keys::{PlainPhrase, SimpleDate, SimpleTime, default_language_id, language_id_or_default};
pub use links::{
    SetLanguageLink, format_set_language_link, is_valid_language_code, parse_set_language_link,
};
pub use strings_parser::{
    ParsedEntry, ParsedLangPack, StringsParseError, parse_lang_strings,
    parse_lang_strings_with_options, strip_comments,
};
pub use tag::{
    CountFormat, CreditsAmount, CreditsType, PluralCount, PluralForm, PluralResult, PluralRule,
    ShortenedCount, StringWithNumbers, TextEntity, TextWithEntities, format_count_decimal,
    format_count_to_short, format_credits_amount_decimal, format_credits_amount_rounded,
    format_credits_amount_to_short, format_double, format_exact_count_decimal, plural_with_rule,
    replace_tag_entities, replace_tag_numbers, replace_tag_str,
};
