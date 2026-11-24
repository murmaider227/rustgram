//! Integration tests for `tg-lang` (`RpcLangCloudClient`, `CloudManager`, `Instance`,
//! and `help.getCountriesList` / `CountriesInstance`) over `FakeRpcClient`.

mod common;

use common::FakeRpcClient;
use tg_api::lang::{RpcLangCloudClient, sync_countries_list};
use tg_lang::{
    CloudAction, CloudManager, CloudManagerConfig, CountriesInstance, Instance as LangInstance,
    LangCloudClient, Naming, Pack, key_ids,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

fn sample_tl_lang(code: &str, name: &str, native: &str) -> tl_enums::LangPackLanguage {
    tl_enums::LangPackLanguage::LangPackLanguage(tl_types::LangPackLanguage {
        official: true,
        rtl: false,
        beta: false,
        name: name.to_owned(),
        native_name: native.to_owned(),
        lang_code: code.to_owned(),
        base_lang_code: None,
        plural_code: code.to_owned(),
        strings_count: 5000,
        translated_count: 4900,
        translations_url: format!("https://translations.example/{code}"),
    })
}

#[tokio::test]
async fn lang_cloud_client_languages_switch_difference_and_strings() {
    let rpc = FakeRpcClient::new(2);

    // 1. langpack.getLanguages
    rpc.enqueue_ok::<tl_fns::langpack::GetLanguages>(&vec![
        sample_tl_lang("en", "English", "English"),
        sample_tl_lang("de", "German", "Deutsch"),
    ]);
    // 2. langpack.getLanguage("de") -> triggers switch -> langpack.getLangPack("de")
    rpc.enqueue_ok::<tl_fns::langpack::GetLanguage>(&sample_tl_lang("de", "German", "Deutsch"));
    rpc.enqueue_ok::<tl_fns::langpack::GetLangPack>(
        &tl_enums::LangPackDifference::LangPackDifference(tl_types::LangPackDifference {
            lang_code: "de".into(),
            from_version: 0,
            version: 10,
            strings: vec![tl_enums::LangPackString::LangPackString(
                tl_types::LangPackString {
                    key: "lng_cancel".into(),
                    value: "Abbrechen".into(),
                },
            )],
        }),
    );
    // 3. Incremental langpack.getDifference("de", from_version = 10)
    rpc.enqueue_ok::<tl_fns::langpack::GetDifference>(
        &tl_enums::LangPackDifference::LangPackDifference(tl_types::LangPackDifference {
            lang_code: "de".into(),
            from_version: 10,
            version: 11,
            strings: vec![tl_enums::LangPackString::LangPackString(
                tl_types::LangPackString {
                    key: "lng_close".into(),
                    value: "Schließen".into(),
                },
            )],
        }),
    );
    // 4. langpack.getStrings("fr", ["lng_cancel"])
    rpc.enqueue_ok::<tl_fns::langpack::GetStrings>(&vec![
        tl_enums::LangPackString::LangPackString(tl_types::LangPackString {
            key: "lng_cancel".into(),
            value: "Annuler".into(),
        }),
    ]);

    let mut client = RpcLangCloudClient::new(&rpc);
    let mut manager = CloudManager::new(CloudManagerConfig::default());
    let mut langpack = LangInstance::new();

    // Fetch language list.
    manager.request_language_list(&mut client);
    assert_eq!(client.pending_count(), 1);
    let actions = client
        .poll_pending(&mut manager, &mut langpack)
        .await
        .unwrap();
    assert!(actions.contains(&CloudAction::LanguageListChanged));
    assert_eq!(manager.language_list().len(), 2);

    // Switch to German: getLanguage -> perform_switch (reinit_connection + getLangPack).
    manager.switch_to_language_id(&langpack, "de", &mut client);
    let actions = client
        .poll_pending(&mut manager, &mut langpack)
        .await
        .unwrap();
    assert!(client.take_reinit_requested());
    assert!(actions.contains(&CloudAction::WriteLangPack));
    assert_eq!(langpack.id(), "de");
    assert_eq!(langpack.version(Pack::Current), 10);
    assert_eq!(langpack.format_plain(key_ids::lng_cancel), "Abbrechen");

    // Incremental version sync -> langpack.getDifference(from_version=10).
    manager.set_current_versions(&langpack, 11, 0, &mut client);
    let actions = client
        .poll_pending(&mut manager, &mut langpack)
        .await
        .unwrap();
    assert!(actions.contains(&CloudAction::WriteLangPack));
    assert_eq!(langpack.version(Pack::Current), 11);
    assert_eq!(langpack.format_plain(key_ids::lng_close), "Schließen");

    // Request a single string in another language (langpack.getStrings).
    let req_id = client.request_strings("tdesktop", "fr", vec!["lng_cancel".into()]);
    assert!(req_id > 0);
    // Test cancellation before poll.
    let cancel_id = client.request_languages("tdesktop");
    client.cancel_request(cancel_id);
    client
        .poll_pending(&mut manager, &mut langpack)
        .await
        .unwrap();
    assert_eq!(rpc.calls_of::<tl_fns::langpack::GetLanguages>().len(), 1);
    assert_eq!(rpc.calls_of::<tl_fns::langpack::GetStrings>().len(), 1);
}

#[tokio::test]
async fn countries_list_sync_modified_and_not_modified() {
    let rpc = FakeRpcClient::new(2);
    rpc.enqueue_ok::<tl_fns::help::GetCountriesList>(
        &tl_enums::help::CountriesList::CountriesList(tl_types::help::CountriesList {
            countries: vec![tl_enums::help::Country::Country(tl_types::help::Country {
                hidden: false,
                iso2: "DE".into(),
                default_name: "Germany".into(),
                name: Some("Deutschland".into()),
                country_codes: vec![tl_enums::help::CountryCode::CountryCode(
                    tl_types::help::CountryCode {
                        country_code: "49".into(),
                        prefixes: Some(vec!["15".into(), "16".into()]),
                        patterns: Some(vec!["XXX XXXXXXX".into()]),
                    },
                )],
            })],
            hash: 123_456,
        }),
    );
    rpc.enqueue_ok::<tl_fns::help::GetCountriesList>(&tl_enums::help::CountriesList::NotModified);

    let mut countries = CountriesInstance::new();
    let changed = sync_countries_list(&rpc, &mut countries, "de")
        .await
        .unwrap();
    assert!(changed);
    assert_eq!(countries.hash(), 123_456);
    assert_eq!(
        countries.country_name_by_iso2("DE", Naming::Default),
        "Germany"
    );

    // Second call passes hash = 123_456 and handles NotModified cleanly.
    let changed_again = sync_countries_list(&rpc, &mut countries, "de")
        .await
        .unwrap();
    assert!(!changed_again);
    assert_eq!(countries.hash(), 123_456);

    let calls = rpc.calls_of::<tl_fns::help::GetCountriesList>();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1.lang_code, "de");
    assert_eq!(calls[1].1.hash, 123_456);
}
