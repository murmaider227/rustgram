//! Offline `help.getConfig` and `langpack.*` answers for the intro's language link.
//!
//! The fake server suggests no language by default (`suggested_lang_code` omitted), so the
//! offline intro shows no language link, as tdesktop does when the server suggests nothing.

use tg_api::rpc::InvocationError;
use tg_tl::{
    Deserializable, Identifiable, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};

/// Language the fake server suggests through `help.getConfig` (`suggested_lang_code`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeLanguageSuggestion {
    /// Suggested language code (`suggested_lang_code`, e.g. `"de"`).
    pub lang_code: String,
    /// English language name (`langPackLanguage.name`).
    pub name: String,
    /// Native language name (`langPackLanguage.native_name`).
    pub native_name: String,
    /// `lng_switch_to_this` in the suggested language; `None` makes `langpack.getStrings`
    /// return no strings, as for a language without that key.
    pub switch_text: Option<String>,
    /// Strings returned by `langpack.getLangPack` for the suggested language.
    pub strings: Vec<(String, String)>,
}

fn decode<R: Deserializable>(bytes: &[u8]) -> Result<R, InvocationError> {
    R::from_bytes(bytes)
        .map_err(|e| InvocationError::Deserialize(format!("fake langpack request decode: {e}")))
}

fn tl_string(key: &str, value: &str) -> tl_enums::LangPackString {
    tl_enums::LangPackString::LangPackString(tl_types::LangPackString {
        key: key.to_owned(),
        value: value.to_owned(),
    })
}

/// Answers `help.getConfig` and the `langpack.*` calls used by the intro, or returns `None`
/// for any other constructor.
pub(crate) fn dispatch(
    suggestion: Option<&FakeLanguageSuggestion>,
    this_dc: i32,
    constructor_id: u32,
    request_bytes: &[u8],
) -> Option<Result<Vec<u8>, InvocationError>> {
    let matching = |code: &str| suggestion.filter(|s| s.lang_code == code);
    let res =
        match constructor_id {
            tl_fns::help::GetConfig::CONSTRUCTOR_ID => {
                Ok(config(suggestion.map(|s| s.lang_code.clone()), this_dc).to_bytes())
            }
            tl_fns::langpack::GetStrings::CONSTRUCTOR_ID => {
                decode::<tl_fns::langpack::GetStrings>(request_bytes).map(|req| {
                    let strings: Vec<tl_enums::LangPackString> = matching(&req.lang_code)
                        .and_then(|s| s.switch_text.as_deref())
                        .filter(|_| req.keys.iter().any(|k| k == "lng_switch_to_this"))
                        .map(|text| vec![tl_string("lng_switch_to_this", text)])
                        .unwrap_or_default();
                    strings.to_bytes()
                })
            }
            tl_fns::langpack::GetLanguage::CONSTRUCTOR_ID => {
                decode::<tl_fns::langpack::GetLanguage>(request_bytes).and_then(|req| {
                    let s = matching(&req.lang_code).ok_or_else(lang_code_invalid)?;
                    Ok(
                        tl_enums::LangPackLanguage::LangPackLanguage(tl_types::LangPackLanguage {
                            official: true,
                            rtl: false,
                            beta: false,
                            name: s.name.clone(),
                            native_name: s.native_name.clone(),
                            lang_code: s.lang_code.clone(),
                            base_lang_code: None,
                            plural_code: s.lang_code.clone(),
                            strings_count: i32::try_from(s.strings.len()).unwrap_or(i32::MAX),
                            translated_count: i32::try_from(s.strings.len()).unwrap_or(i32::MAX),
                            translations_url: String::new(),
                        })
                        .to_bytes(),
                    )
                })
            }
            tl_fns::langpack::GetLangPack::CONSTRUCTOR_ID => {
                decode::<tl_fns::langpack::GetLangPack>(request_bytes).and_then(|req| {
                    let s = matching(&req.lang_code).ok_or_else(lang_code_invalid)?;
                    Ok(tl_enums::LangPackDifference::LangPackDifference(
                        tl_types::LangPackDifference {
                            lang_code: s.lang_code.clone(),
                            from_version: 0,
                            version: 1,
                            strings: s.strings.iter().map(|(k, v)| tl_string(k, v)).collect(),
                        },
                    )
                    .to_bytes())
                })
            }
            _ => return None,
        };
    Some(res)
}

fn lang_code_invalid() -> InvocationError {
    InvocationError::Rpc(tg_api::rpc::RpcError::new(400, "LANG_CODE_NOT_SUPPORTED"))
}

fn config(suggested_lang_code: Option<String>, this_dc: i32) -> tl_enums::Config {
    tl_enums::Config::Config(tl_types::Config {
        default_p2p_contacts: false,
        preload_featured_stickers: false,
        revoke_pm_inbox: false,
        blocked_mode: false,
        force_try_ipv6: false,
        date: 1_750_000_000,
        expires: 1_750_003_600,
        test_mode: true,
        this_dc,
        dc_options: Vec::new(),
        dc_txt_domain_name: String::new(),
        chat_size_max: 200,
        megagroup_size_max: 200_000,
        forwarded_count_max: 100,
        online_update_period_ms: 210_000,
        offline_blur_timeout_ms: 5_000,
        offline_idle_timeout_ms: 30_000,
        online_cloud_timeout_ms: 300_000,
        notify_cloud_delay_ms: 30_000,
        notify_default_delay_ms: 1_500,
        push_chat_period_ms: 60_000,
        push_chat_limit: 2,
        edit_time_limit: 172_800,
        revoke_time_limit: 2_147_483_647,
        revoke_pm_time_limit: 2_147_483_647,
        rating_e_decay: 2_419_200,
        stickers_recent_limit: 200,
        channels_read_media_period: 604_800,
        tmp_sessions: None,
        call_receive_timeout_ms: 20_000,
        call_ring_timeout_ms: 90_000,
        call_connect_timeout_ms: 30_000,
        call_packet_timeout_ms: 10_000,
        me_url_prefix: "https://t.me/".to_owned(),
        autoupdate_url_prefix: None,
        gif_search_username: None,
        venue_search_username: None,
        img_search_username: None,
        static_maps_provider: None,
        caption_length_max: 1024,
        message_length_max: 4096,
        webfile_dc_id: 4,
        // `suggested_lang_code` and both pack versions share `flags.2` in the schema.
        lang_pack_version: suggested_lang_code.as_ref().map(|_| 0),
        base_lang_pack_version: suggested_lang_code.as_ref().map(|_| 0),
        suggested_lang_code,
        reactions_default: None,
        autologin_token: None,
    })
}
