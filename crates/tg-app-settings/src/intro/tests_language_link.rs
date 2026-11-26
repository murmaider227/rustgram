//! The intro's language link follows the server suggestion (`createLanguageLink`): no
//! suggestion means no link; a suggested language with loaded strings shows the server text.
//! These tests never switch the process-wide language (other tests render English text).

use std::sync::Arc;

use tg_api::types::ApiCredentials;

use crate::intro::fake_invoker::{FakeLanguageSuggestion, FakeRpcClient, FakeRpcConfig};
use crate::intro::language_link::{
    apply_downloaded_language_to, download_language, effective_suggestion,
};
use crate::intro::{DynIntroRpc, IntroWidget};

const SERVER_SWITCH_TEXT: &str = "Continue in Testlandish";

fn german(switch_text: Option<&str>) -> FakeLanguageSuggestion {
    FakeLanguageSuggestion {
        lang_code: "de".to_owned(),
        name: "German".to_owned(),
        native_name: "Deutsch".to_owned(),
        switch_text: switch_text.map(ToOwned::to_owned),
        strings: vec![
            (
                "lng_intro_about".to_owned(),
                "Server intro text for de".to_owned(),
            ),
            (
                "lng_start_msgs".to_owned(),
                "Server start for de".to_owned(),
            ),
        ],
    }
}

fn intro_with(suggestion: Option<FakeLanguageSuggestion>) -> IntroWidget {
    let fake = FakeRpcClient::new(FakeRpcConfig::default());
    fake.set_language_suggestion(suggestion);
    let mut intro = IntroWidget::with_invoker(
        ApiCredentials {
            api_id: 0,
            api_hash: String::new(),
        },
        fake,
    );
    intro.request_language_suggestion(None);
    intro
}

#[test]
fn no_server_suggestion_means_no_language_link() {
    let intro = intro_with(None);
    assert_eq!(intro.start.suggestion, None);
    assert_eq!(intro.start.language_link_label_for("en"), None);
}

#[test]
fn default_intro_has_no_hard_coded_suggestion() {
    let intro = IntroWidget::new();
    assert_eq!(intro.start.suggestion, None);
    assert_eq!(intro.start.language_link_label_for("en"), None);
}

#[test]
fn suggested_german_with_loaded_strings_shows_the_server_text() {
    let intro = intro_with(Some(german(Some(SERVER_SWITCH_TEXT))));
    let suggestion = intro.start.suggestion.as_ref().expect("suggestion loaded");
    assert_eq!(suggestion.lang_id, "de");
    assert_eq!(suggestion.label, SERVER_SWITCH_TEXT);
    assert_eq!(
        intro.start.language_link_label_for("en").as_deref(),
        Some(SERVER_SWITCH_TEXT)
    );
}

#[test]
fn suggestion_without_switch_string_shows_no_link() {
    let intro = intro_with(Some(german(None)));
    assert_eq!(intro.start.suggestion, None);
    assert_eq!(intro.start.language_link_label_for("en"), None);
}

#[test]
fn english_suggestion_is_not_offered() {
    assert_eq!(effective_suggestion(""), None);
    assert_eq!(effective_suggestion("en"), None);
    assert_eq!(effective_suggestion("de"), Some("de"));

    let mut english = german(Some(SERVER_SWITCH_TEXT));
    english.lang_code = "en".to_owned();
    let intro = intro_with(Some(english));
    assert_eq!(intro.start.suggestion, None);
    assert_eq!(intro.start.language_link_label_for("en"), None);
}

#[test]
fn non_english_language_offers_continue_in_english() {
    let intro = intro_with(None);
    assert_eq!(
        intro.start.language_link_label_for("de").as_deref(),
        Some(tg_lang::get_original_value(
            tg_lang::key_ids::lng_switch_to_this
        ))
    );
}

#[test]
fn add_account_intro_hides_the_link() {
    let mut intro = intro_with(Some(german(Some(SERVER_SWITCH_TEXT))));
    intro.disable_language_link();
    assert_eq!(intro.start.language_link_label_for("en"), None);
    assert_eq!(intro.start.language_link_label_for("de"), None);
}

#[test]
fn clicking_the_link_downloads_and_applies_the_server_pack() {
    let fake = Arc::new(FakeRpcClient::new(FakeRpcConfig::default()));
    fake.set_language_suggestion(Some(german(Some(SERVER_SWITCH_TEXT))));
    let rpc: Arc<dyn DynIntroRpc> = fake;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let downloaded = rt
        .block_on(download_language(rpc.as_ref(), "de"))
        .expect("download de");
    assert_eq!(downloaded.language.id, "de");
    assert_eq!(downloaded.language.native_name, "Deutsch");

    let mut inst = tg_lang::Instance::new();
    apply_downloaded_language_to(&mut inst, &downloaded).expect("apply de");
    assert_eq!(inst.id(), "de");
    assert_eq!(
        inst.get_value(tg_lang::key_ids::lng_intro_about),
        "Server intro text for de"
    );

    assert!(
        rt.block_on(download_language(rpc.as_ref(), "fr")).is_err(),
        "a language the server does not know is not applied"
    );
}
