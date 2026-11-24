//! Tests for cloud language pack difference merging (deletions & version gaps -> full refetch),
//! plural selection across languages, tdata persistence round-trip, and `setlanguage` link parsing.

use std::path::PathBuf;

use tg_lang::{
    CloudAction, CloudManager, CloudManagerConfig, Instance, LangCloudClient, LangPackDifference,
    LangPackLanguageInfo, LangPackString, Language, Pack, PluralCount, PluralForm, PluralRule,
    RequestId, SerializeLoadOutcome, SetLanguageLink, format_set_language_link,
    is_valid_language_code, key_ids, parse_set_language_link, push_recent_language_to_tdata,
    read_recent_languages_from_tdata, remove_recent_language_from_tdata,
};

#[derive(Default)]
struct RecordingCloudClient {
    connected: bool,
    next_id: RequestId,
    calls: Vec<String>,
}

impl RecordingCloudClient {
    fn new() -> Self {
        Self {
            connected: true,
            next_id: 1,
            calls: Vec::new(),
        }
    }

    fn alloc(&mut self) -> RequestId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

impl LangCloudClient for RecordingCloudClient {
    fn is_connected(&self) -> bool {
        self.connected
    }

    fn request_languages(&mut self, lang_pack: &str) -> RequestId {
        self.calls.push(format!("getLanguages({lang_pack})"));
        self.alloc()
    }

    fn request_lang_pack(&mut self, lang_pack: &str, lang_code: &str) -> RequestId {
        self.calls
            .push(format!("getLangPack({lang_pack},{lang_code})"));
        self.alloc()
    }

    fn request_difference(
        &mut self,
        lang_pack: &str,
        lang_code: &str,
        from_version: i32,
    ) -> RequestId {
        self.calls.push(format!(
            "getDifference({lang_pack},{lang_code},{from_version})"
        ));
        self.alloc()
    }

    fn request_language(&mut self, lang_pack: &str, lang_code: &str) -> RequestId {
        self.calls
            .push(format!("getLanguage({lang_pack},{lang_code})"));
        self.alloc()
    }

    fn request_strings(
        &mut self,
        lang_pack: &str,
        lang_code: &str,
        keys: Vec<String>,
    ) -> RequestId {
        self.calls.push(format!(
            "getStrings({lang_pack},{lang_code},{})",
            keys.join(",")
        ));
        self.alloc()
    }

    fn cancel_request(&mut self, request_id: RequestId) {
        self.calls.push(format!("cancel({request_id})"));
    }

    fn reinit_connection(&mut self) {}
}

fn unique_temp_tdata(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "tg_lang_tdata_{tag}_{}_{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&dir).expect("create temp tdata dir");
    dir
}

#[test]
fn difference_merge_deletions_and_version_gap_triggers_full_refetch() {
    let mut inst = Instance::new();
    let mut mgr = CloudManager::new(CloudManagerConfig::default());
    let mut client = RecordingCloudClient::new();

    // Switch to pt-br with base pack pt.
    let pt_br = Language {
        id: "pt-br".to_owned(),
        plural_id: "pt".to_owned(),
        base_id: "pt".to_owned(),
        name: "Portuguese (Brazil)".to_owned(),
        native_name: "Português (Brasil)".to_owned(),
    };
    mgr.switch_to_language(&mut inst, &pt_br, &mut client);
    assert!(
        client
            .calls
            .contains(&"getLangPack(tdesktop,pt-br)".to_owned())
    );
    assert!(
        client
            .calls
            .contains(&"getLangPack(tdesktop,pt)".to_owned())
    );
    client.calls.clear();

    // Apply initial base pack v4 and current pack v5.
    mgr.apply_lang_pack_difference(
        &mut inst,
        &LangPackDifference {
            lang_code: "pt".to_owned(),
            from_version: 0,
            version: 4,
            strings: vec![
                LangPackString::Normal {
                    key: "lng_menu_settings".to_owned(),
                    value: "Definições".to_owned(),
                },
                LangPackString::Normal {
                    key: "lng_cancel".to_owned(),
                    value: "Cancelar (PT)".to_owned(),
                },
            ],
        },
        &mut client,
    )
    .expect("apply base v4");

    mgr.apply_lang_pack_difference(
        &mut inst,
        &LangPackDifference {
            lang_code: "pt-br".to_owned(),
            from_version: 0,
            version: 5,
            strings: vec![
                LangPackString::Normal {
                    key: "lng_menu_settings".to_owned(),
                    value: "Configurações".to_owned(),
                },
                LangPackString::Normal {
                    key: "lng_close".to_owned(),
                    value: "Fechar Antigo".to_owned(),
                },
            ],
        },
        &mut client,
    )
    .expect("apply current v5");
    assert_eq!(inst.version(Pack::Current), 5);
    assert_eq!(
        inst.format_plain(key_ids::lng_menu_settings),
        "Configurações"
    );
    assert_eq!(inst.format_plain(key_ids::lng_close), "Fechar Antigo");

    // Incremental diff with deletion falls back from current -> base -> English.
    let actions = mgr
        .apply_lang_pack_difference(
            &mut inst,
            &LangPackDifference {
                lang_code: "pt-br".to_owned(),
                from_version: 5,
                version: 6,
                strings: vec![LangPackString::Deleted {
                    key: "lng_menu_settings".to_owned(),
                }],
            },
            &mut client,
        )
        .expect("apply deletion diff");
    assert_eq!(actions, vec![CloudAction::WriteLangPack]);
    assert_eq!(inst.version(Pack::Current), 6);
    assert_eq!(inst.format_plain(key_ids::lng_menu_settings), "Definições");

    // Push update arrives with a version gap (local=6, from_version=10):
    // Step 1: CloudManager requests incremental getDifference(from_version=6).
    client.calls.clear();
    let gap_actions = mgr
        .apply_lang_pack_difference(
            &mut inst,
            &LangPackDifference {
                lang_code: "pt-br".to_owned(),
                from_version: 10,
                version: 11,
                strings: vec![LangPackString::Normal {
                    key: "lng_close".to_owned(),
                    value: "ShouldWait".to_owned(),
                }],
            },
            &mut client,
        )
        .expect("gap push triggers getDifference");
    assert!(gap_actions.is_empty());
    assert_eq!(client.calls, vec!["getDifference(tdesktop,pt-br,6)"]);
    assert_eq!(inst.version(Pack::Current), 6);

    // Step 2: If getDifference ITSELF still returns a gap (from_version=10 > 6),
    // CloudManager immediately escalates to a full getLangPack refetch!
    client.calls.clear();
    let still_gap = mgr
        .apply_lang_pack_difference_from_source(
            &mut inst,
            &LangPackDifference {
                lang_code: "pt-br".to_owned(),
                from_version: 10,
                version: 12,
                strings: vec![],
            },
            true,
            &mut client,
        )
        .expect("persistent gap triggers full refetch");
    assert!(still_gap.is_empty());
    assert_eq!(client.calls, vec!["getLangPack(tdesktop,pt-br)"]);

    // Step 3: Full getLangPack response (from_version=0, version=12) replaces stale
    // overrides (`lng_close` is not in the new full pack -> resets to English "Close")
    // while preserving base pack fallback (`lng_menu_settings` -> "Definições").
    let full_actions = mgr
        .apply_lang_pack_difference_from_source(
            &mut inst,
            &LangPackDifference {
                lang_code: "pt-br".to_owned(),
                from_version: 0,
                version: 12,
                strings: vec![LangPackString::Normal {
                    key: "lng_cancel".to_owned(),
                    value: "Cancelar (BR)".to_owned(),
                }],
            },
            false,
            &mut client,
        )
        .expect("apply full refetch");
    assert_eq!(full_actions, vec![CloudAction::WriteLangPack]);
    assert_eq!(inst.version(Pack::Current), 12);
    assert_eq!(inst.format_plain(key_ids::lng_cancel), "Cancelar (BR)");
    assert_eq!(inst.format_plain(key_ids::lng_close), "Close");
    assert_eq!(inst.format_plain(key_ids::lng_menu_settings), "Definições");

    // Also verify RPC error LANG_PACK_DIFFERENCE_TOO_OLD on getDifference escalates to full refetch.
    client.calls.clear();
    mgr.request_lang_pack_difference(&inst, Pack::Current, &mut client);
    assert_eq!(client.calls, vec!["getDifference(tdesktop,pt-br,12)"]);
    client.calls.clear();
    mgr.on_pack_request_failed_with_error(
        &inst,
        Pack::Current,
        "LANG_PACK_DIFFERENCE_TOO_OLD",
        &mut client,
    );
    assert_eq!(client.calls, vec!["getLangPack(tdesktop,pt-br)"]);
}

#[test]
fn plural_selection_across_multiple_languages() {
    // Russian (One / Few / Many)
    let mut ru = Instance::new();
    ru.switch_to_id(&Language {
        id: "ru".to_owned(),
        plural_id: "ru".to_owned(),
        base_id: String::new(),
        name: "Russian".to_owned(),
        native_name: "Русский".to_owned(),
    });
    ru.apply_difference(
        Pack::Current,
        &LangPackDifference {
            lang_code: "ru".to_owned(),
            from_version: 0,
            version: 1,
            strings: vec![LangPackString::Pluralized {
                key: "lng_seconds".to_owned(),
                zero_value: None,
                one_value: Some("{count} секунда".to_owned()),
                two_value: None,
                few_value: Some("{count} секунды".to_owned()),
                many_value: Some("{count} секунд".to_owned()),
                other_value: "{count} секунды".to_owned(),
            }],
        },
    )
    .expect("apply ru plural");
    assert_eq!(
        ru.format_plural(key_ids::lng_seconds, 1.into(), &[]),
        "1 секунда"
    );
    assert_eq!(
        ru.format_plural(key_ids::lng_seconds, 2.into(), &[]),
        "2 секунды"
    );
    assert_eq!(
        ru.format_plural(key_ids::lng_seconds, 5.into(), &[]),
        "5 секунд"
    );
    assert_eq!(
        ru.format_plural(key_ids::lng_seconds, 11.into(), &[]),
        "11 секунд"
    );
    assert_eq!(
        ru.format_plural(key_ids::lng_seconds, 21.into(), &[]),
        "21 секунда"
    );
    assert_eq!(
        ru.format_plural(key_ids::lng_seconds, 24.into(), &[]),
        "24 секунды"
    );

    // Arabic (all 6 CLDR forms: zero, one, two, few, many, other)
    let mut ar = Instance::new();
    ar.switch_to_id(&Language {
        id: "ar".to_owned(),
        plural_id: "ar".to_owned(),
        base_id: String::new(),
        name: "Arabic".to_owned(),
        native_name: "العربية".to_owned(),
    });
    ar.apply_difference(
        Pack::Current,
        &LangPackDifference {
            lang_code: "ar".to_owned(),
            from_version: 0,
            version: 1,
            strings: vec![LangPackString::Pluralized {
                key: "lng_seconds".to_owned(),
                zero_value: Some("zero:{count}".to_owned()),
                one_value: Some("one:{count}".to_owned()),
                two_value: Some("two:{count}".to_owned()),
                few_value: Some("few:{count}".to_owned()),
                many_value: Some("many:{count}".to_owned()),
                other_value: "other:{count}".to_owned(),
            }],
        },
    )
    .expect("apply ar plural");
    assert_eq!(
        ar.format_plural(key_ids::lng_seconds, 0.into(), &[]),
        "zero:0"
    );
    assert_eq!(
        ar.format_plural(key_ids::lng_seconds, 1.into(), &[]),
        "one:1"
    );
    assert_eq!(
        ar.format_plural(key_ids::lng_seconds, 2.into(), &[]),
        "two:2"
    );
    assert_eq!(
        ar.format_plural(key_ids::lng_seconds, 7.into(), &[]),
        "few:7"
    );
    assert_eq!(
        ar.format_plural(key_ids::lng_seconds, 42.into(), &[]),
        "many:42"
    );
    assert_eq!(
        ar.format_plural(key_ids::lng_seconds, 100.into(), &[]),
        "other:100"
    );

    // Polish vs Japanese vs French rule spot checks
    assert_eq!(
        PluralRule::for_language("pl").choose_int(21),
        PluralForm::Many
    );
    assert_eq!(
        PluralRule::for_language("pl").choose_int(22),
        PluralForm::Few
    );
    assert_eq!(
        PluralRule::for_language("fr").choose_int(0),
        PluralForm::One
    );
    assert_eq!(
        PluralRule::for_language("fr").choose_int(2),
        PluralForm::Other
    );
    assert_eq!(
        PluralRule::for_language("ja").choose_int(1),
        PluralForm::Other
    );

    // Shortened count formatting inside plural selection
    assert_eq!(
        ru.format_plural(key_ids::lng_seconds, PluralCount::short(12_345), &[]),
        "12.3K секунд"
    );
}

#[test]
fn tdata_langpack_and_recent_languages_roundtrip() {
    let tdata_dir = unique_temp_tdata("roundtrip");

    // 1. Fresh tdata has no persisted langpack yet.
    let mut fresh = Instance::new();
    assert_eq!(fresh.load_from_tdata_dir(&tdata_dir).unwrap(), None);

    // 2. Configure pt-br + base pt, apply diffs, and persist into encrypted tdata files.
    let mut inst = Instance::new();
    let pt_br = Language {
        id: "pt-br".to_owned(),
        plural_id: "pt".to_owned(),
        base_id: "pt".to_owned(),
        name: "Portuguese (Brazil)".to_owned(),
        native_name: "Português (Brasil)".to_owned(),
    };
    inst.switch_to_id(&pt_br);
    inst.apply_difference(
        Pack::Base,
        &LangPackDifference {
            lang_code: "pt".to_owned(),
            from_version: 0,
            version: 14,
            strings: vec![LangPackString::Normal {
                key: "lng_menu_contacts".to_owned(),
                value: "Contactos".to_owned(),
            }],
        },
    )
    .unwrap();
    inst.apply_difference(
        Pack::Current,
        &LangPackDifference {
            lang_code: "pt-br".to_owned(),
            from_version: 0,
            version: 29,
            strings: vec![LangPackString::Normal {
                key: "lng_menu_settings".to_owned(),
                value: "Configurações".to_owned(),
            }],
        },
    )
    .unwrap();
    inst.save_to_tdata_dir(&tdata_dir).expect("save langpack");

    // 3. Persist recent languages into tdata (custom `#custom` IDs must be ignored).
    push_recent_language_to_tdata(&tdata_dir, &pt_br).expect("push pt-br");
    push_recent_language_to_tdata(
        &tdata_dir,
        &Language {
            id: "#custom".to_owned(),
            ..Language::default()
        },
    )
    .expect("ignore custom");

    // 4. Reopen tdata in a clean Instance and verify exact versioned restoration.
    let mut restored = Instance::new();
    let outcome = restored
        .load_from_tdata_dir(&tdata_dir)
        .expect("load langpack from tdata");
    assert_eq!(outcome, Some(SerializeLoadOutcome::LoadedFromCache));
    assert_eq!(restored.id(), "pt-br");
    assert_eq!(restored.base_id(), "pt");
    assert_eq!(restored.plural_id(), "pt");
    assert_eq!(restored.name(), "Portuguese (Brazil)");
    assert_eq!(restored.native_name(), "Português (Brasil)");
    assert_eq!(restored.version(Pack::Current), 29);
    assert_eq!(restored.version(Pack::Base), 14);
    assert_eq!(
        restored.format_plain(key_ids::lng_menu_settings),
        "Configurações"
    );
    assert_eq!(
        restored.format_plain(key_ids::lng_menu_contacts),
        "Contactos"
    );

    let recents = read_recent_languages_from_tdata(&tdata_dir).expect("read recents");
    assert_eq!(recents, vec![pt_br.clone()]);
    remove_recent_language_from_tdata(&tdata_dir, "pt-br").expect("remove recent");
    assert!(
        read_recent_languages_from_tdata(&tdata_dir)
            .expect("read recents after remove")
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(&tdata_dir);
}

#[test]
fn setlanguage_link_parsing_and_formatting() {
    // tg:// links
    assert_eq!(
        parse_set_language_link("tg://setlanguage"),
        Some(SetLanguageLink::OpenLanguageBox)
    );
    assert_eq!(
        parse_set_language_link("TG://SetLanguage/"),
        Some(SetLanguageLink::OpenLanguageBox)
    );
    assert_eq!(
        parse_set_language_link("tg://setlanguage?lang=de"),
        Some(SetLanguageLink::SwitchLanguage("de".to_owned()))
    );
    assert_eq!(
        parse_set_language_link("tg://setlanguage/?lang=pt-br&source=share#top"),
        Some(SetLanguageLink::SwitchLanguage("pt-br".to_owned()))
    );
    assert_eq!(
        parse_set_language_link("tg://setlanguage?foo=1&lang=custom_pack.v2"),
        Some(SetLanguageLink::SwitchLanguage("custom_pack.v2".to_owned()))
    );

    // t.me / telegram.me / telegram.dog links
    assert_eq!(
        parse_set_language_link("https://t.me/setlanguage/uk"),
        Some(SetLanguageLink::SwitchLanguage("uk".to_owned()))
    );
    assert_eq!(
        parse_set_language_link("http://t.me/setlanguage/pt-br?ref=1"),
        Some(SetLanguageLink::SwitchLanguage("pt-br".to_owned()))
    );
    assert_eq!(
        parse_set_language_link("t.me/setlanguage/ru"),
        Some(SetLanguageLink::SwitchLanguage("ru".to_owned()))
    );
    assert_eq!(
        parse_set_language_link("https://www.telegram.me/setlanguage/es"),
        Some(SetLanguageLink::SwitchLanguage("es".to_owned()))
    );
    assert_eq!(
        parse_set_language_link("https://telegram.dog/setlanguage/fr#section"),
        Some(SetLanguageLink::SwitchLanguage("fr".to_owned()))
    );
    assert_eq!(
        parse_set_language_link("https://t.me/setlanguage/pt%2Dbr"),
        Some(SetLanguageLink::SwitchLanguage("pt-br".to_owned()))
    );

    // Formatting round-trip
    let link = format_set_language_link("pirate-en");
    assert_eq!(link, "https://t.me/setlanguage/pirate-en");
    assert_eq!(
        parse_set_language_link(&link),
        Some(SetLanguageLink::SwitchLanguage("pirate-en".to_owned()))
    );

    // Rejected invalid links
    assert_eq!(parse_set_language_link(""), None);
    assert_eq!(parse_set_language_link("tg://resolve?domain=foo"), None);
    assert_eq!(parse_set_language_link("tg://setlanguage?lang="), None);
    assert_eq!(
        parse_set_language_link("tg://setlanguage?lang=bad!code"),
        None
    );
    assert_eq!(parse_set_language_link("https://t.me/setlanguage/"), None);
    assert_eq!(
        parse_set_language_link("https://t.me/setlanguage/de/extra"),
        None
    );
    assert_eq!(
        parse_set_language_link("https://example.com/setlanguage/de"),
        None
    );
    assert!(!is_valid_language_code("../etc"));
    assert!(is_valid_language_code("pt-br_2.0"));
}

#[test]
fn cloud_manager_retains_full_language_info_flags() {
    let mut mgr = CloudManager::new(CloudManagerConfig::default());
    let infos = vec![
        LangPackLanguageInfo {
            official: true,
            rtl: false,
            beta: false,
            name: "English".to_owned(),
            native_name: "English".to_owned(),
            lang_code: "en".to_owned(),
            base_lang_code: None,
            plural_code: "en".to_owned(),
            strings_count: 5000,
            translated_count: 5000,
            translations_url: "https://translations.telegram.org/en/".to_owned(),
        },
        LangPackLanguageInfo {
            official: true,
            rtl: true,
            beta: true,
            name: "Arabic".to_owned(),
            native_name: "العربية".to_owned(),
            lang_code: "ar".to_owned(),
            base_lang_code: None,
            plural_code: "ar".to_owned(),
            strings_count: 5000,
            translated_count: 4600,
            translations_url: "https://translations.telegram.org/ar/".to_owned(),
        },
        LangPackLanguageInfo {
            official: false,
            rtl: false,
            beta: false,
            name: "Community Pack".to_owned(),
            native_name: "Community".to_owned(),
            lang_code: "comm-en".to_owned(),
            base_lang_code: Some("en".to_owned()),
            plural_code: "en".to_owned(),
            strings_count: 5000,
            translated_count: 2500,
            translations_url: "https://translations.telegram.org/comm-en/".to_owned(),
        },
    ];
    let actions = mgr.on_languages_received(&infos);
    assert!(actions.contains(&CloudAction::LanguageListChanged));
    assert_eq!(mgr.language_infos().len(), 3);
    let ar = mgr.find_language_info("ar").expect("ar info");
    assert!(ar.official && ar.rtl && ar.beta);
    let comm = mgr.find_language_info("comm-en").expect("comm info");
    assert!(!comm.official);
    assert_eq!(comm.base_lang_code.as_deref(), Some("en"));
    assert_eq!(comm.translated_percent(), 50);
}
