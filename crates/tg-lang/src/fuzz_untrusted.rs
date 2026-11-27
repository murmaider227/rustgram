//! Panic fuzz over server lang packs, lang files, countries data and number formatting
//! (TGB-3065).

use tg_fuzz::Fuzz;

use crate::countries::{CloudCountriesList, CloudCountry, CloudCountryCode, CountriesInstance};
use crate::instance::{Instance, LangPackDifference, LangPackString, Pack};
use crate::tag::{
    CountFormat, CreditsAmount, CreditsType, PluralRule, StringWithNumbers, TextEntity,
    TextWithEntities,
};

const ITERATIONS: usize = 3_000;

const VALUE_TOKENS: &[&str] = &[
    "{count}", "{user}", "{name}", "{date}", "{", "}", "{{", "}}", "{count", "count}", "**", "__",
    "[a]", "(b)", "#", "\\", "\\{", "%1$s", "\u{1}", "\u{2}",
];

const LANGS: &[&str] = &[
    "en",
    "ru",
    "uk",
    "ar",
    "pl",
    "cs",
    "lt",
    "lv",
    "ga",
    "gd",
    "cy",
    "he",
    "sl",
    "mt",
    "br",
    "x",
    "",
    "pt-br",
    "zh-hans",
    "\u{e9}\u{e9}",
];

fn random_key(fuzz: &mut Fuzz) -> String {
    if fuzz.rng().one_in(8) {
        return fuzz.text(&["lng_", "#one", "#other", "_"], 4);
    }
    let index = fuzz.rng().below(crate::KEYS_COUNT) as u16;
    let mut name = crate::key_name(index).unwrap_or("lng_unknown").to_string();
    if fuzz.rng().one_in(3) {
        name.push_str(
            fuzz.rng()
                .pick(&["#zero", "#one", "#two", "#few", "#many", "#other"]),
        );
    }
    name
}

fn random_value(fuzz: &mut Fuzz) -> String {
    fuzz.text(VALUE_TOKENS, 12)
}

fn random_string(fuzz: &mut Fuzz) -> LangPackString {
    let key = random_key(fuzz);
    match fuzz.rng().below(3) {
        0 => LangPackString::Normal {
            key,
            value: random_value(fuzz),
        },
        1 => {
            let opt = |fuzz: &mut Fuzz| fuzz.rng().one_in(2).then(|| random_value(fuzz));
            LangPackString::Pluralized {
                key,
                zero_value: opt(fuzz),
                one_value: opt(fuzz),
                two_value: opt(fuzz),
                few_value: opt(fuzz),
                many_value: opt(fuzz),
                other_value: random_value(fuzz),
            }
        }
        _ => LangPackString::Deleted { key },
    }
}

#[test]
fn server_lang_pack_strings_do_not_panic() {
    let mut fuzz = Fuzz::new(0x1A46_0001, ITERATIONS);
    for i in 0..fuzz.iterations() {
        let strings: Vec<LangPackString> = (0..1 + fuzz.rng().below(6))
            .map(|_| random_string(&mut fuzz))
            .collect();
        let difference = LangPackDifference {
            lang_code: fuzz.rng().pick(LANGS).to_string(),
            from_version: fuzz.rng().int(3) as i32,
            version: fuzz.rng().int(3) as i32,
            strings: strings.clone(),
        };
        let replacement = random_value(&mut fuzz);
        let count = fuzz.rng().int(1_000_000) as f64 / if fuzz.rng().one_in(2) { 1.0 } else { 7.0 };
        fuzz.check("apply_difference and formatting", i, &difference, || {
            let _ = Instance::parse_strings(&strings);
            let mut instance = Instance::new();
            let _ = instance.apply_difference(Pack::Current, &difference);
            for item in &strings {
                let key = match item {
                    LangPackString::Normal { key, .. }
                    | LangPackString::Pluralized { key, .. }
                    | LangPackString::Deleted { key } => key,
                };
                let Some(index) = crate::get_key_index(key) else {
                    continue;
                };
                let value = instance.get_value(index).to_string();
                for &tag in crate::key_tags(index) {
                    let _ = crate::tag::replace_tag_str(value.clone(), tag, &replacement);
                    let _ = crate::tag::replace_tag_numbers(
                        StringWithNumbers {
                            text: value.clone(),
                            ..StringWithNumbers::default()
                        },
                        tag,
                        &replacement,
                    );
                    let entities = vec![TextEntity {
                        entity_type: 1,
                        offset: 0,
                        length: value.len(),
                        data: String::new(),
                    }];
                    let _ = crate::tag::replace_tag_entities(
                        TextWithEntities {
                            text: value.clone(),
                            entities: entities.clone(),
                        },
                        tag,
                        &TextWithEntities {
                            text: replacement.clone(),
                            entities,
                        },
                    );
                }
            }
            let rule = PluralRule::for_language(&difference.lang_code);
            let _ = rule.choose_f64(count);
            let _ = crate::tag::plural_with_rule(rule, true, count, CountFormat::CountShort);
            let _ = crate::tag::plural_with_rule(rule, false, count, CountFormat::CountDecimal);
        });
        let key = random_key(&mut fuzz);
        let value = random_value(&mut fuzz);
        fuzz.check("parse_key_value", i, &(&key, &value), || {
            let _ = crate::instance::parse_key_value(&key, value.as_bytes());
        });
    }
    fuzz.finish();
}

#[test]
fn lang_files_strings_and_caches_do_not_panic() {
    let mut fuzz = Fuzz::new(0x1A46_0002, ITERATIONS);
    let strings_seed = b"\"lng_status_online\" = \"online\";\n\"lng_forwarded#one\" = \"{count} message\";\n// c\n/* c */\n".to_vec();
    let cache_seed = CountriesInstance::new().serialize_cache();
    for i in 0..fuzz.iterations() {
        let data = match fuzz.rng().below(3) {
            0 => fuzz.mutate(&strings_seed),
            1 => fuzz.mutate(&cache_seed),
            _ => fuzz
                .text(
                    &[
                        "\"",
                        " = ",
                        ";",
                        "\n",
                        "lng_status_online",
                        "#one",
                        "//",
                        "/*",
                        "*/",
                        "{count}",
                    ],
                    30,
                )
                .into_bytes(),
        };
        fuzz.check("lang file parsers", i, &data, || {
            let _ = crate::file_parser::decode_lang_file_bytes(&data);
            let _ = crate::file_parser::FileParser::parse_content(&data, |_, _| {});
            let _ = crate::strings_parser::strip_comments(&data);
            if let Ok(text) = std::str::from_utf8(&data) {
                let _ = crate::strings_parser::parse_lang_strings(text);
                let _ = crate::strings_parser::parse_lang_strings_with_options(text, true);
                let _ = crate::links::parse_set_language_link(text);
            }
            let _ = crate::countries::deserialize_countries_cache(&data);
            let mut instance = Instance::new();
            let _ = instance.fill_from_serialized(&data, 0);
        });
    }
    fuzz.finish();
}

#[test]
fn server_countries_and_phone_formatting_do_not_panic() {
    let mut fuzz = Fuzz::new(0x1A46_0003, ITERATIONS);
    const PATTERN: &[&str] = &[
        "X", "XX", "XXX", " ", "-", "0", "1", "(", ")", "+", "\u{e9}",
    ];
    for i in 0..fuzz.iterations() {
        let codes = (0..1 + fuzz.rng().below(3))
            .map(|_| CloudCountryCode {
                country_code: fuzz.text(&["1", "7", "38", "380", "44"], 2),
                prefixes: fuzz
                    .rng()
                    .one_in(2)
                    .then(|| vec![fuzz.text(&["0", "9"], 3)]),
                patterns: fuzz
                    .rng()
                    .one_in(2)
                    .then(|| vec![fuzz.text(PATTERN, 8), fuzz.text(PATTERN, 8)]),
            })
            .collect();
        let list = CloudCountriesList::Modified {
            hash: fuzz.rng().int(9) as i32,
            countries: vec![CloudCountry {
                iso2: fuzz.text(&["UA", "US", "a"], 2),
                default_name: fuzz.text(&[], 3),
                country_codes: codes,
                ..CloudCountry::default()
            }],
        };
        let phone = fuzz.text(&["1", "7", "38", "380", "44", "+", "9", "0"], 10);
        fuzz.check("countries", i, &(&list, &phone), || {
            let mut countries = CountriesInstance::new();
            let _ = countries.apply_cloud_list(list.clone());
            let _ = countries.format_phone(&phone);
            let _ = countries.extract_phone_code(&phone);
            let _ = countries.groups(&phone);
            let _ = countries.valid_phone_code(&phone);
            let _ = countries.country_iso2_by_phone(&phone);
            let _ = CountriesInstance::flag_emoji_by_iso2(&phone);
            let _ = crate::countries::deserialize_countries_cache(&countries.serialize_cache());
        });
    }
    fuzz.finish();
}

#[test]
fn number_formatting_with_edge_values_does_not_panic() {
    let mut fuzz = Fuzz::new(0x1A46_0004, ITERATIONS * 5);
    for i in 0..fuzz.iterations() {
        let a = fuzz.rng().int(10_000_000_000);
        let b = fuzz.rng().int(10_000_000_000);
        let f = match fuzz.rng().below(4) {
            0 => *fuzz.rng().pick(&[
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::MAX,
                f64::MIN,
                -0.0,
            ]),
            _ => a as f64 / (1 + fuzz.rng().below(1000)) as f64,
        };
        let s = fuzz.text(&["0", "1", ".", "5", "00"], 8);
        fuzz.check("number formatting", i, &(a, b, f, &s), || {
            let _ = crate::tag::format_count_to_short(a, false);
            let _ = crate::tag::format_count_to_short(a, true);
            let _ = crate::tag::format_count_decimal(a);
            let _ = crate::tag::format_count_decimal_with_sep(a, '\u{a0}');
            let _ = crate::tag::format_exact_count_decimal(f);
            let _ = crate::tag::format_double(f);
            let _ = crate::tag::non_zero_part_to_int(&s);
            for kind in [CreditsType::Stars, CreditsType::Ton] {
                let amount = CreditsAmount::new(a, b, kind);
                let _ = crate::tag::format_credits_amount_to_short(amount);
                let _ = crate::tag::format_credits_amount_decimal(amount);
                let _ = crate::tag::format_credits_amount_rounded(amount);
            }
            for lang in LANGS {
                let rule = PluralRule::for_language(lang);
                let _ = rule.choose_int(a);
                let _ = rule.choose_f64(f);
                let _ = crate::tag::plural_with_rule(rule, true, f, CountFormat::CountShort);
                let _ = crate::tag::plural_with_rule(rule, true, f, CountFormat::Count);
            }
        });
    }
    fuzz.finish();
}

#[test]
fn credits_amount_normalizes_extreme_nanos_without_overflow() {
    // Old code negated `nano` (overflow on i64::MIN) and added shifts unchecked.
    let amount = CreditsAmount::new(0, i64::MIN, CreditsType::Stars);
    assert!((0..1_000_000_000).contains(&amount.nano()));
    let amount = CreditsAmount::new(i64::MAX, 2_500_000_000, CreditsType::Ton);
    assert_eq!((amount.whole(), amount.nano()), (i64::MAX, 500_000_000));
    let amount = CreditsAmount::new(5, -1, CreditsType::Stars);
    assert_eq!((amount.whole(), amount.nano()), (4, 999_999_999));
}
