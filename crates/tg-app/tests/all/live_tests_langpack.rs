//! Live integration tests for cloud language packs (`LiveLangPackHandle`, `LiveSession`,
//! `TgLangStrings`, `SettingsController`, and `tg-session` tdata startup restoration) over
//! an in-process `FakeRpcClient`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::UserId as DataUserId;
use tg_lang::{Pack, SerializeLoadOutcome, tr};
use tg_session::{AccountManager, SessionRuntime, SessionRuntimeConfig, WorkingDirOpenOptions};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use tg_app::live::langpack::SetLanguageLinkOutcome;
use tg_app::live::session::LiveSession;
use tg_app::settings::backend::{LanguagePrompt, SettingsBackend};
use tg_app::settings::language::{LanguageState, filter_languages, prepare_language_lists};
use tg_app::settings::{SettingsController, SettingsSection};
use tg_app::stand_in::strings::{Strings, TgLangStrings};

/// Process-wide lock for tests that observe `tg_lang::global_instance()` / `TgLangStrings` so
/// parallel test threads never race on the global pack.
static GLOBAL_LANG_TEST_LOCK: Mutex<()> = Mutex::new(());

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeLangRpc {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeLangRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<R>(&self, resp: &R::Return)
    where
        R: RemoteCall + Identifiable,
        R::Return: Serializable,
    {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((R::CONSTRUCTOR_ID, Ok(resp.to_bytes())));
    }

    fn enqueue_rpc_err<R: Identifiable>(&self, code: i32, name: &str) {
        self.responses.lock().expect("lock responses").push_back((
            R::CONSTRUCTOR_ID,
            Err(InvocationError::Rpc(RpcError {
                code,
                name: name.to_owned(),
                value: None,
            })),
        ));
    }

    fn push_update(&self, update: tl_enums::Update) {
        let _ = self
            .updates_tx
            .send(tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
                update,
                date: 1_750_000_100,
            }));
    }

    fn calls_of<R: Identifiable + Deserializable>(&self) -> Vec<R> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter_map(|(ctor, bytes)| {
                if *ctor == R::CONSTRUCTOR_ID {
                    R::from_bytes(bytes).ok()
                } else {
                    None
                }
            })
            .collect()
    }
}

impl RpcClient for FakeLangRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };
        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

#[allow(clippy::too_many_arguments)]
fn tl_lang(
    code: &str,
    plural: &str,
    base: Option<&str>,
    name: &str,
    native: &str,
    official: bool,
    beta: bool,
    rtl: bool,
    strings_count: i32,
    translated_count: i32,
) -> tl_enums::LangPackLanguage {
    tl_enums::LangPackLanguage::LangPackLanguage(tl_types::LangPackLanguage {
        official,
        rtl,
        beta,
        name: name.to_owned(),
        native_name: native.to_owned(),
        lang_code: code.to_owned(),
        base_lang_code: base.map(ToOwned::to_owned),
        plural_code: plural.to_owned(),
        strings_count,
        translated_count,
        translations_url: format!("https://translations.telegram.org/{code}/"),
    })
}

fn tl_diff(
    lang_code: &str,
    from_version: i32,
    version: i32,
    strings: Vec<tl_enums::LangPackString>,
) -> tl_enums::LangPackDifference {
    tl_enums::LangPackDifference::LangPackDifference(tl_types::LangPackDifference {
        lang_code: lang_code.to_owned(),
        from_version,
        version,
        strings,
    })
}

fn tl_str(key: &str, value: &str) -> tl_enums::LangPackString {
    tl_enums::LangPackString::LangPackString(tl_types::LangPackString {
        key: key.to_owned(),
        value: value.to_owned(),
    })
}

fn tl_deleted(key: &str) -> tl_enums::LangPackString {
    tl_enums::LangPackString::Deleted(tl_types::LangPackStringDeleted {
        key: key.to_owned(),
    })
}

fn tl_config_with_lang(
    suggested: Option<&str>,
    version: Option<i32>,
    base_version: Option<i32>,
) -> tl_enums::Config {
    tl_enums::Config::Config(tl_types::Config {
        default_p2p_contacts: false,
        preload_featured_stickers: false,
        revoke_pm_inbox: false,
        blocked_mode: false,
        force_try_ipv6: false,
        date: 1_750_000_000,
        expires: 1_750_003_600,
        test_mode: true,
        this_dc: 2,
        dc_options: Vec::new(),
        dc_txt_domain_name: "apv3.stel.com".to_owned(),
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
        rating_e_decay: 2419200,
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
        suggested_lang_code: suggested.map(ToOwned::to_owned),
        lang_pack_version: version,
        base_lang_pack_version: base_version,
        reactions_default: None,
        autologin_token: None,
    })
}

fn start_minimal_live_session(
    fake: FakeLangRpc,
) -> (
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    LiveSession,
) {
    fake.enqueue_ok::<tl_fns::updates::GetState>(&tl_enums::updates::State::State(
        tl_types::updates::State {
            pts: 1,
            qts: 0,
            date: 1_750_000_000,
            seq: 1,
            unread_count: 0,
        },
    ));
    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        changes_capacity: 64,
        idle_ping_enabled: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId(900_001),
            tg_app::live::ErasedRpcClient::new(fake),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    runtime.wait_ready_blocking().expect("runtime ready");
    let live = LiveSession::new(Arc::clone(&runtime), 1);
    (runtime, live)
}

fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    pred()
}

#[test]
fn live_langpack_full_flow_base_pack_updates_gaps_tdata_and_runtime_strings() {
    let _guard = GLOBAL_LANG_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    tg_lang::with_current_mut(|g| g.switch_to_id(&tg_lang::Language::default_english()));

    let tmp = TempDir::new().expect("tempdir");
    let tdata_dir = tmp.path().join("tdata");
    std::fs::create_dir_all(&tdata_dir).expect("mkdir tdata");

    let fake = FakeLangRpc::new();
    let (_runtime, mut live) = start_minimal_live_session(fake.clone());
    assert_eq!(
        live.langpack()
            .set_tdata_dir(tdata_dir.clone())
            .expect("attach empty tdata"),
        None
    );

    // 1. langpack.getLanguages with official, beta, RTL, and unofficial packs.
    fake.enqueue_ok::<tl_fns::langpack::GetLanguages>(&vec![
        tl_lang(
            "en", "en", None, "English", "English", true, false, false, 5000, 5000,
        ),
        tl_lang(
            "pt-br",
            "pt",
            Some("pt"),
            "Portuguese (Brazil)",
            "Português (Brasil)",
            true,
            true,
            false,
            5000,
            4900,
        ),
        tl_lang(
            "ar",
            "ar",
            None,
            "Arabic",
            "العربية",
            true,
            false,
            true,
            5000,
            4800,
        ),
        tl_lang(
            "pirate",
            "en",
            Some("en"),
            "Pirate English",
            "Pirate",
            false,
            false,
            false,
            5000,
            3200,
        ),
    ]);

    let langs = SettingsBackend::languages(&live);
    assert_eq!(langs.len(), 4);
    assert!(langs[1].official && langs[1].beta);
    assert!(langs[2].rtl);
    assert!(!langs[3].official);
    assert_eq!(filter_languages(&langs, "brasil").len(), 1);

    // Prepare TgLangStrings BEFORE applying the new language — it must live-update without restart!
    let strings = TgLangStrings::new();
    assert_eq!(strings.menu_settings(), "Settings");
    assert_eq!(strings.menu_contacts(), "Contacts");
    let gen_before = live.bridge_state().generation();

    // 2. Apply "pt-br" -> fetches BOTH `langpack.getLangPack("pt-br")` and base `langpack.getLangPack("pt")`.
    fake.enqueue_ok::<tl_fns::langpack::GetLangPack>(&tl_diff(
        "pt-br",
        0,
        10,
        vec![
            tl_str("lng_menu_settings", "Configurações"),
            tl_str("lng_cancel", "Cancelar BR"),
        ],
    ));
    fake.enqueue_ok::<tl_fns::langpack::GetLangPack>(&tl_diff(
        "pt",
        0,
        7,
        vec![
            tl_str("lng_menu_settings", "Definições"),
            tl_str("lng_menu_contacts", "Contactos"),
        ],
    ));

    let applied_name = SettingsBackend::apply_language(&mut live, "pt-br").expect("apply pt-br");
    assert_eq!(applied_name, "Portuguese (Brazil)");
    assert_eq!(live.langpack().active_language_code(), "pt-br");
    assert_eq!(live.langpack().base_language_code(), "pt");
    assert_eq!(live.langpack().versions(), (10, 7));
    assert!(live.langpack().reinit_count() >= 1);
    assert!(live.bridge_state().generation() > gen_before);

    // Runtime switch verified via immutable `&TgLangStrings` AND `tg_lang::tr::*`!
    assert_eq!(strings.menu_settings(), "Configurações");
    assert_eq!(strings.menu_contacts(), "Contactos");
    assert_eq!(strings.cancel(), "Cancelar BR");
    assert_eq!(tr::lng_menu_settings(), "Configurações");

    let pack_calls = fake.calls_of::<tl_fns::langpack::GetLangPack>();
    assert_eq!(pack_calls.len(), 2);
    assert!(pack_calls.iter().any(|c| c.lang_code == "pt-br"));
    assert!(pack_calls.iter().any(|c| c.lang_code == "pt"));

    // 3. Push `updateLangPack` with a deletion of `lng_menu_settings` in `pt-br` ->
    // falls back to base pack `"Definições"` live via background event stream!
    fake.push_update(tl_enums::Update::LangPack(tl_types::UpdateLangPack {
        difference: tl_diff("pt-br", 10, 11, vec![tl_deleted("lng_menu_settings")]),
    }));
    assert!(
        wait_until(Duration::from_secs(3), || {
            live.langpack().versions().0 == 11
        }),
        "expected updateLangPack v11 to be applied"
    );
    assert_eq!(strings.menu_settings(), "Definições");
    assert_eq!(tr::lng_menu_settings(), "Definições");

    // 4. Push `updateLangPackTooLong("pt-br")` -> triggers `langpack.getDifference(from_version=11)`.
    // Simulate a server version gap response (from_version=20 > 11) -> escalates to full `getLangPack`!
    fake.enqueue_ok::<tl_fns::langpack::GetDifference>(&tl_diff("pt-br", 20, 22, vec![]));
    fake.enqueue_ok::<tl_fns::langpack::GetLangPack>(&tl_diff(
        "pt-br",
        0,
        22,
        vec![tl_str("lng_menu_settings", "Ajustes v22")],
    ));
    fake.push_update(tl_enums::Update::LangPackTooLong(
        tl_types::UpdateLangPackTooLong {
            lang_code: "pt-br".to_owned(),
        },
    ));
    assert!(
        wait_until(Duration::from_secs(3), || {
            live.langpack().versions().0 == 22
        }),
        "expected updateLangPackTooLong gap -> full getLangPack v22"
    );
    assert_eq!(strings.menu_settings(), "Ajustes v22");
    // Stale `lng_cancel` from v10 was dropped by the full refetch -> reverts to English default.
    assert_eq!(strings.cancel(), "Cancel");
    // Base pack value `lng_menu_contacts` survives intact!
    assert_eq!(strings.menu_contacts(), "Contactos");

    // 5. Verify tdata persistence on disk AND startup loading before UI via `AccountManager::open`.
    // Reset global instance to English first to prove `AccountManager::open` restores from tdata.
    tg_lang::with_current_mut(|g| g.switch_to_id(&tg_lang::Language::default_english()));
    assert_eq!(strings.menu_settings(), "Settings");

    let open_opts = WorkingDirOpenOptions::new(
        tmp.path(),
        tg_api::ApiCredentials {
            api_id: 12345,
            api_hash: "0123456789abcdef0123456789abcdef".to_owned(),
        },
    );
    let _mgr = AccountManager::open(open_opts).expect("AccountManager::open restores langpack");
    // Before any UI is created, global instance & TgLangStrings already show the persisted pack!
    assert_eq!(strings.menu_settings(), "Ajustes v22");
    assert_eq!(strings.menu_contacts(), "Contactos");
    assert_eq!(
        tg_lang::with_current(|g| (
            g.id().to_owned(),
            g.base_id().to_owned(),
            g.version(Pack::Current),
            g.version(Pack::Base)
        )),
        ("pt-br".to_owned(), "pt".to_owned(), 22, 7)
    );

    // Restore clean English default for other tests.
    tg_lang::with_current_mut(|g| g.switch_to_id(&tg_lang::Language::default_english()));
    tg_lang::set_global_tdata_dir(None);
}

#[test]
fn live_setlanguage_links_open_confirmation_and_apply_unofficial_pack() {
    let _guard = GLOBAL_LANG_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    tg_lang::with_current_mut(|g| g.switch_to_id(&tg_lang::Language::default_english()));

    let fake = FakeLangRpc::new();
    let (_runtime, live) = start_minimal_live_session(fake.clone());
    let mut ctrl = SettingsController::new(Box::new(live.clone()));

    // 1. Bare `tg://setlanguage` opens the Language section without a switch prompt.
    let has_prompt = ctrl
        .open_set_language_link("tg://setlanguage")
        .expect("bare setlanguage link");
    assert!(!has_prompt);
    assert_eq!(ctrl.current_section(), SettingsSection::Language);

    // 2. `https://t.me/setlanguage/pirate` fetches `langpack.getLanguage` and opens ConfirmLinkSwitch.
    fake.enqueue_ok::<tl_fns::langpack::GetLanguage>(&tl_lang(
        "pirate",
        "en",
        None,
        "Pirate English",
        "Pirate",
        false,
        false,
        false,
        1000,
        750,
    ));
    let has_prompt = ctrl
        .open_set_language_link("https://t.me/setlanguage/pirate")
        .expect("t.me setlanguage link");
    assert!(has_prompt);
    match ctrl.language.pending_prompt.as_ref() {
        Some(LanguagePrompt::ConfirmLinkSwitch { info, body, .. }) => {
            assert_eq!(info.lang_code, "pirate");
            assert!(!info.official);
            assert_eq!(info.translated_percent(), 75);
            assert!(body.contains("Pirate"));
            assert!(body.contains("75"));
        }
        other => panic!("expected ConfirmLinkSwitch, got {other:?}"),
    }

    // Confirming downloads `langpack.getLangPack("pirate")` and applies live!
    fake.enqueue_ok::<tl_fns::langpack::GetLangPack>(&tl_diff(
        "pirate",
        0,
        3,
        vec![tl_str("lng_menu_settings", "Ship Rigging")],
    ));
    let applied = ctrl
        .backend
        .confirm_language_prompt()
        .expect("confirm link switch");
    assert_eq!(applied.as_deref(), Some("Pirate English"));
    assert_eq!(tr::lng_menu_settings(), "Ship Rigging");

    // Recent non-official language appears in prepare_language_lists above official packs.
    let (recent_non_official, _official) =
        prepare_language_lists(&[], &ctrl.backend.recent_languages(), "pirate");
    assert_eq!(recent_non_official.len(), 1);
    assert_eq!(recent_non_official[0].lang_code, "pirate");

    // 3. Already-using link returns AlreadyUsing prompt without RPC.
    let already = live
        .langpack()
        .handle_set_language_link("tg://setlanguage?lang=pirate")
        .expect("already using");
    assert!(matches!(
        already,
        SetLanguageLinkOutcome::Prompt(p) if matches!(*p, LanguagePrompt::AlreadyUsing { .. })
    ));

    // 4. Not-ready pack (`strings_count == 0`) returns NotReady prompt.
    fake.enqueue_ok::<tl_fns::langpack::GetLanguage>(&tl_lang(
        "empty-pack",
        "en",
        None,
        "Empty Pack",
        "Empty",
        false,
        false,
        false,
        0,
        0,
    ));
    let not_ready = live
        .langpack()
        .handle_set_language_link("tg://setlanguage?lang=empty-pack")
        .expect("not ready");
    assert!(matches!(
        not_ready,
        SetLanguageLinkOutcome::Prompt(p) if matches!(*p, LanguagePrompt::NotReady { .. })
    ));

    // 5. Unsupported language code (`LANG_CODE_NOT_SUPPORTED`) returns NotFound prompt.
    fake.enqueue_rpc_err::<tl_fns::langpack::GetLanguage>(400, "LANG_CODE_NOT_SUPPORTED");
    let not_found = live
        .langpack()
        .handle_set_language_link("https://t.me/setlanguage/nonexistent")
        .expect("not found");
    assert!(matches!(
        not_found,
        SetLanguageLinkOutcome::Prompt(p) if matches!(*p, LanguagePrompt::NotFound { .. })
    ));

    tg_lang::with_current_mut(|g| g.switch_to_id(&tg_lang::Language::default_english()));
}

#[test]
fn live_suggested_language_from_help_get_config_offer_and_decline() {
    let _guard = GLOBAL_LANG_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    tg_lang::with_current_mut(|g| g.copy_from(&tg_lang::Instance::new()));

    let tmp = TempDir::new().expect("tempdir");
    let tdata_dir = tmp.path().join("tdata");
    std::fs::create_dir_all(&tdata_dir).expect("mkdir tdata");

    let fake = FakeLangRpc::new();
    let (_runtime, live) = start_minimal_live_session(fake.clone());
    live.langpack()
        .set_tdata_dir(tdata_dir.clone())
        .expect("set tdata");

    // 1. help.getConfig returns suggested_lang_code = "de" -> triggers getLanguages + getStrings("lng_switch_to_this").
    fake.enqueue_ok::<tl_fns::help::GetConfig>(&tl_config_with_lang(Some("de"), Some(0), Some(0)));
    fake.enqueue_ok::<tl_fns::langpack::GetLanguages>(&vec![
        tl_lang(
            "en", "en", None, "English", "English", true, false, false, 5000, 5000,
        ),
        tl_lang(
            "de", "de", None, "German", "Deutsch", true, false, false, 5000, 4950,
        ),
    ]);
    fake.enqueue_ok::<tl_fns::langpack::GetStrings>(&vec![tl_str(
        "lng_switch_to_this",
        "Auf Deutsch fortfahren",
    )]);

    let prompt = live
        .langpack()
        .sync_from_config()
        .expect("sync_from_config");
    match prompt {
        Some(LanguagePrompt::SuggestedLanguage {
            language,
            switch_cta,
            prompt_text,
        }) => {
            assert_eq!(language.id, "de");
            assert_eq!(language.native_name, "Deutsch");
            assert_eq!(switch_cta, "Auf Deutsch fortfahren");
            assert!(prompt_text.contains("Deutsch"));
        }
        other => panic!("expected SuggestedLanguage prompt, got {other:?}"),
    }
    assert_eq!(live.langpack().suggested_language(), "de");

    // 2. Dismissing the offer locks in English and persists to tdata so it won't be re-offered.
    live.langpack().dismiss_pending_prompt();
    assert!(live.langpack().pending_prompt().is_none());

    let mut reloaded = tg_lang::Instance::new();
    let outcome = reloaded
        .load_from_tdata_dir(&tdata_dir)
        .expect("reload after decline");
    assert_eq!(outcome, Some(SerializeLoadOutcome::LoadedFromCache));
    assert_eq!(reloaded.id(), "en");

    // Also verify LanguageState constructor tracks active & selected codes cleanly.
    let st = LanguageState::new("en");
    assert_eq!(st.active_lang_code, "en");
    assert_eq!(st.selected_lang_code, "en");

    tg_lang::with_current_mut(|g| g.switch_to_id(&tg_lang::Language::default_english()));
    tg_lang::set_global_tdata_dir(None);
}

#[test]
fn live_langpack_auto_tdata_from_account_manager_runtime_plurals_and_rpc_error() {
    let _guard = GLOBAL_LANG_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    tg_lang::with_current_mut(|g| g.switch_to_id(&tg_lang::Language::default_english()));
    tg_lang::set_global_tdata_dir(None);

    let tmp = TempDir::new().expect("tempdir");
    let open_opts = WorkingDirOpenOptions::new(
        tmp.path(),
        tg_api::ApiCredentials {
            api_id: 12345,
            api_hash: "0123456789abcdef0123456789abcdef".to_owned(),
        },
    );
    // Opening AccountManager records the active tdata directory globally so LiveSession::new
    // persists language switches automatically without live/host.rs calling set_tdata_dir.
    let _mgr = AccountManager::open(open_opts).expect("open AccountManager");

    let fake = FakeLangRpc::new();
    let (_runtime, mut live) = start_minimal_live_session(fake.clone());

    fake.enqueue_ok::<tl_fns::langpack::GetLanguages>(&vec![
        tl_lang(
            "en", "en", None, "English", "English", true, false, false, 5000, 5000,
        ),
        tl_lang(
            "ru",
            "ru",
            None,
            "Russian",
            "Русский",
            true,
            false,
            false,
            5000,
            5000,
        ),
    ]);
    assert_eq!(SettingsBackend::languages(&live).len(), 2);

    // Apply Russian with pluralized `lng_reconnecting` and verify live plural selection via TgLangStrings!
    fake.enqueue_ok::<tl_fns::langpack::GetLangPack>(&tl_diff(
        "ru",
        0,
        5,
        vec![tl_enums::LangPackString::Pluralized(
            tl_types::LangPackStringPluralized {
                key: "lng_reconnecting".to_owned(),
                zero_value: None,
                one_value: Some("Переподключение через {count} секунду...".to_owned()),
                two_value: None,
                few_value: Some("Переподключение через {count} секунды...".to_owned()),
                many_value: Some("Переподключение через {count} секунд...".to_owned()),
                other_value: "Переподключение через {count} секунды...".to_owned(),
            },
        )],
    ));

    let strings = TgLangStrings::new();
    let applied = SettingsBackend::apply_language(&mut live, "ru").expect("apply ru");
    assert_eq!(applied, "Russian");
    assert_eq!(
        strings.reconnecting_in(1),
        "Переподключение через 1 секунду..."
    );
    assert_eq!(
        strings.reconnecting_in(2),
        "Переподключение через 2 секунды..."
    );
    assert_eq!(
        strings.reconnecting_in(5),
        "Переподключение через 5 секунд..."
    );
    assert_eq!(
        strings.reconnecting_in(21),
        "Переподключение через 21 секунду..."
    );

    // Verify automatic persistence to tdata (without manual set_tdata_dir)!
    let mut reloaded = tg_lang::Instance::new();
    let outcome = reloaded
        .load_from_tdata_dir(&tmp.path().join("tdata"))
        .expect("reload auto-persisted tdata");
    assert_eq!(outcome, Some(SerializeLoadOutcome::LoadedFromCache));
    assert_eq!(reloaded.id(), "ru");
    assert_eq!(reloaded.plural_id(), "ru");
    assert_eq!(reloaded.version(Pack::Current), 5);

    // Verify SettingsController::new mirrors the persisted global language instance.
    let ctrl = SettingsController::new(Box::new(live.clone()));
    assert_eq!(ctrl.language.active_lang_code, "ru");

    // Verify transport/RPC failure surfaces as Err rather than silently succeeding.
    let err = SettingsBackend::apply_language(&mut live, "fr")
        .expect_err("missing fake response should return Err");
    assert!(!err.is_empty());

    tg_lang::with_current_mut(|g| g.switch_to_id(&tg_lang::Language::default_english()));
    tg_lang::set_global_tdata_dir(None);
}
