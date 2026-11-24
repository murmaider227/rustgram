//! End-to-end integration tests across `tg-crypto`, `tg-storage`, `tg-core`, and `tg-lang`.
//!
//! Uses only isolated `tempfile::TempDir` directories and synthetic keys/data.
//! Never touches `%APPDATA%\Telegram Desktop` or any real Telegram account.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tg_core::domain::{Domain, DomainStorage as _, Environment, StartResult};
use tg_core::globals::GlobalConfig;
use tg_core::launcher::{WorkDirEnv, parse_arguments, resolve_working_dir};
use tg_core::persistence::TdataDomainStorage;
use tg_core::settings::{CoreSettings, EmbeddedThemeType, WindowPosition};
use tg_core::settings_proxy::{ProxyData, ProxySettingsMode, ProxyType};
use tg_crypto::{LOCAL_ENCRYPT_SALT_SIZE, create_legacy_local_key};
use tg_lang::{
    CloudCountriesList, CloudCountry, CloudCountryCode, CountriesInstance,
    Instance as LangInstance, LangPackDifference, LangPackString, Language, Pack,
    SerializeLoadOutcome, key_ids, push_recent_language, read_recent_languages,
};
use tg_storage::account::{
    DraftCursor, StoredDraft, StoredFileLocation, StoredLocations, SuggestOptions, WebPageDraft,
};
use tg_storage::serialize::TextTag;
use tg_storage::serialize::peer::{
    SerializedPeer, SerializedPeerKind, SerializedUser, serialize_self_user,
};
use tg_storage::tdf::{
    FileWriteDescriptor, compute_data_name_key, prepare_encrypted, to_file_part,
};
use tg_storage::{
    AccountStorage, AccountWriteScheduler, AsyncWriteManager, AuthKey, DraftKey, LocalStorage,
    MtpAuthKeyEntry, MtpAuthorization, PasscodeKdf, PasscodeKdfKind, PeerId, QDataStreamWriter,
    QtColor, QtDateTime,
};

fn fast_test_argon2id() -> PasscodeKdf {
    PasscodeKdf {
        kind: PasscodeKdfKind::Argon2id,
        memory: 64,
        time: 1,
        parallel: 1,
    }
}

fn synthetic_self_peer(user_id: u64, username: &str) -> SerializedPeer {
    SerializedPeer {
        id: PeerId::from_user(user_id),
        userpic_photo_id: 0,
        userpic_location: Default::default(),
        userpic_has_video: false,
        kind: SerializedPeerKind::User(SerializedUser {
            first_name: "Test".into(),
            last_name: format!("User{user_id}"),
            phone: "0000000000".into(),
            username: username.into(),
            access_hash: 0x1122_3344_5566_7788,
            flags: 0,
            bot_inline_placeholder: String::new(),
            lastseen: 0,
            is_contact: false,
            bot_info_version: -1,
            supports_guest_chat: false,
        }),
    }
}

#[test]
fn end_to_end_two_accounts_passcode_settings_langpack_mtp_drafts_restart() {
    let tmp = TempDir::new().expect("tempdir");
    let workdir_str = tmp.path().to_string_lossy().into_owned();

    // Resolve working dir the same way tg-app will at startup.
    let parsed = parse_arguments(["rustgram.exe", "-workdir", &workdir_str]);
    let resolved = resolve_working_dir(
        &parsed,
        &WorkDirEnv {
            exe_dir: tmp.path().to_path_buf(),
            app_data_dir: None,
            current_dir: tmp.path().to_path_buf(),
        },
    );
    let tdata = resolved.path.join("tdata");

    // Prepare synthetic test data.
    let auth_acc0 = MtpAuthorization {
        user_id: 7_000_000_001,
        main_dc_id: 2,
        keys: vec![
            MtpAuthKeyEntry {
                dc_id: 2,
                key: AuthKey::new([0x22u8; 256]),
            },
            MtpAuthKeyEntry {
                dc_id: 4,
                key: AuthKey::new([0x44u8; 256]),
            },
        ],
        keys_to_destroy: vec![],
    };
    let auth_acc1 = MtpAuthorization {
        user_id: 7_000_000_002,
        main_dc_id: 4,
        keys: vec![
            MtpAuthKeyEntry {
                dc_id: 2,
                key: AuthKey::new([0xA2u8; 256]),
            },
            MtpAuthKeyEntry {
                dc_id: 4,
                key: AuthKey::new([0xA4u8; 256]),
            },
        ],
        keys_to_destroy: vec![MtpAuthKeyEntry {
            dc_id: 1,
            key: AuthKey::new([0x11u8; 256]),
        }],
    };

    let peer0 = PeerId::from_user(42_000_001);
    let draft0 = StoredDraft {
        key: DraftKey::local(0, PeerId::ZERO),
        text: "Hello Rustgram bold draft!".into(),
        tags: vec![TextTag {
            offset: 13,
            length: 4,
            id: "bold".into(),
        }],
        reply_peer_id: peer0,
        reply_msg_id: 987_654,
        suggest: SuggestOptions {
            exists: true,
            price_whole: 10,
            price_nano: 500,
            ton: false,
            date: 1_750_000_000,
        },
        webpage: WebPageDraft {
            url: "https://example.invalid/preview".into(),
            force_large_media: true,
            force_small_media: false,
            invert: false,
            manual: true,
            removed: false,
        },
        cursor: DraftCursor {
            position: 17,
            anchor: 13,
            scroll: 0,
        },
    };

    let mut expected_settings = CoreSettings {
        auto_lock: 300,
        song_volume: 0.65,
        video_volume: 0.80,
        dialogs_with_chat_width_ratio: 0.35,
        // Matches core_settings.cpp:1321-1323: when dialogsWithChatWidthRatio > 0,
        // addFromSerialized aligns dialogsNoChatWidthRatio to it.
        dialogs_no_chat_width_ratio: 0.35,
        download_path: "D:/Downloads/RustgramTest".into(),
        accounts_order: vec![auth_acc1.user_id, auth_acc0.user_id],
        window_position: WindowPosition {
            moncrc: 12345,
            maximized: 0,
            scale: 125,
            x: 100,
            y: 120,
            w: 1100,
            h: 760,
        },
        ..CoreSettings::default()
    };
    expected_settings.themes_accent_colors.set(
        EmbeddedThemeType::Night,
        QtColor::new(0x11, 0x22, 0x33, 0xFF),
    );
    expected_settings
        .proxy
        .set_settings(ProxySettingsMode::Enabled);
    expected_settings.proxy.set_selected(ProxyData {
        proxy_type: ProxyType::Socks5,
        host: "127.0.0.1".into(),
        port: 1080,
        user: "u".into(),
        password: "p".into(),
    });

    let target_lang = Language {
        id: "pt-br".into(),
        plural_id: "pt".into(),
        base_id: "pt".into(),
        name: "Portuguese (Brazil)".into(),
        native_name: "Português (Brasil)".into(),
    };

    // === SESSION 1: Create domain, accounts, settings, langpack, countries, MTP auth, drafts, passcode ===
    {
        let mut local = LocalStorage::open_or_init(&tdata).expect("init local storage");
        let mut cfg = GlobalConfig {
            auto_start: true,
            config_scale: 125,
            ..GlobalConfig::default()
        };
        cfg.validate_scale();
        cfg.write_to_global_settings(local.settings_mut());

        // 1. Persist CoreSettings into dbiApplicationSettings of tdata/settingss
        expected_settings
            .save_to_local_storage(&mut local)
            .expect("save CoreSettings");

        // 2. Persist Lang::Instance into dbiLangPackKey + recent languages + countries cache
        let mut lang = LangInstance::new();
        lang.switch_to_id(&target_lang);
        lang.apply_difference(
            Pack::Base,
            &LangPackDifference {
                lang_code: "pt".into(),
                from_version: 0,
                version: 3,
                strings: vec![LangPackString::Normal {
                    key: "lng_menu_contacts".into(),
                    value: "Contatos".into(),
                }],
            },
        )
        .expect("apply base lang diff");
        lang.apply_difference(
            Pack::Current,
            &LangPackDifference {
                lang_code: "pt-br".into(),
                from_version: 0,
                version: 7,
                strings: vec![LangPackString::Normal {
                    key: "lng_menu_settings".into(),
                    value: "Configurações".into(),
                }],
            },
        )
        .expect("apply current lang diff");
        lang.save_to_local_storage(&mut local)
            .expect("save langpack");
        assert_ne!(local.settings().lang_pack_key, 0);

        push_recent_language(&mut local, &Language::default_english()).expect("push en");
        push_recent_language(&mut local, &target_lang).expect("push pt-br");

        let mut countries = CountriesInstance::new();
        assert!(countries.apply_cloud_list(CloudCountriesList::Modified {
            hash: 998877,
            countries: vec![CloudCountry {
                hidden: false,
                iso2: "BR".into(),
                default_name: "Brazil".into(),
                name: None,
                country_codes: vec![CloudCountryCode {
                    country_code: "55".into(),
                    prefixes: None,
                    patterns: Some(vec!["XX XXXXX XXXX".into()]),
                }],
            }],
        }));
        countries
            .save_to_local_storage(&local)
            .expect("save countries cache");
        assert!(tdata.join("countries").exists());

        // 3. Create multi-account Domain with 2 accounts
        let storage = TdataDomainStorage::from_working_dir(&resolved.path, cfg.data_file());
        assert!(!storage.needs_passcode().expect("fresh needs_passcode"));
        let mut domain = Domain::new(cfg.data_file(), storage);
        assert_eq!(
            domain.start(b"").expect("start fresh"),
            StartResult::Success
        );

        // Account 0: MTP auth + self user + drafts + cursors
        domain
            .storage()
            .write_account_mtp_auth(0, &auth_acc0)
            .expect("write mtp acc0");
        {
            let acc0 = domain
                .storage_mut()
                .account_storage_mut(0)
                .expect("acc0 storage");
            acc0.map_mut().self_serialized = serialize_self_user(
                &synthetic_self_peer(auth_acc0.user_id, "alice_synth"),
                "Bio0",
            );
            acc0.write_drafts(peer0, std::slice::from_ref(&draft0))
                .expect("write draft0");
            acc0.write_draft_cursors(peer0, std::slice::from_ref(&draft0))
                .expect("write cursor0");
        }
        domain
            .account_mut(0)
            .expect("acc0")
            .create_session(tg_core::AccountSession {
                user_id: auth_acc0.user_id as i64,
                unique_id: auth_acc0.user_id,
                premium: false,
                is_test_mode: false,
                username: "alice_synth".into(),
                unread_badge: 0,
                unread_badge_muted: false,
            });

        // Account 1: add + activate + MTP auth + self user
        let idx1 = domain
            .add_activated(Environment::Production)
            .expect("add account 1");
        assert_eq!(idx1, 1);
        domain
            .storage()
            .write_account_mtp_auth(1, &auth_acc1)
            .expect("write mtp acc1");
        {
            let acc1 = domain
                .storage_mut()
                .account_storage_mut(1)
                .expect("acc1 storage");
            acc1.map_mut().self_serialized =
                serialize_self_user(&synthetic_self_peer(auth_acc1.user_id, "bob_synth"), "Bio1");
            acc1.write_map().expect("write acc1 map");
        }

        // 4. Arm modern Argon2id passcode + app lock
        domain
            .storage_mut()
            .set_passcode(b"top-secret-pass-42", fast_test_argon2id(), true)
            .expect("set passcode");
        assert!(domain.storage().has_passcode());
        assert!(domain.storage().app_lock_enabled());
    }

    // === SESSION 2: Fresh "restart" reading everything back with the passcode ===
    {
        let local = LocalStorage::open_or_init(&tdata).expect("reopen local storage");

        // 1. Verify GlobalConfig & CoreSettings
        let mut cfg = GlobalConfig::default();
        cfg.apply_global_settings(local.settings());
        assert!(cfg.auto_start);
        assert_eq!(cfg.scale, 125);

        let restored_settings =
            CoreSettings::load_from_local_storage(&local).expect("load CoreSettings");
        assert_eq!(restored_settings, expected_settings);

        // 2. Verify Lang::Instance, recent languages, and countries cache
        let mut restored_lang = LangInstance::new();
        let outcome = restored_lang
            .load_from_local_storage(&local)
            .expect("load langpack");
        assert_eq!(outcome, Some(SerializeLoadOutcome::LoadedFromCache));
        assert_eq!(restored_lang.id(), "pt-br");
        assert_eq!(restored_lang.base_id(), "pt");
        assert_eq!(restored_lang.version(Pack::Current), 7);
        assert_eq!(restored_lang.version(Pack::Base), 3);
        assert_eq!(
            restored_lang.format_plain(key_ids::lng_menu_settings),
            "Configurações"
        );
        assert_eq!(
            restored_lang.format_plain(key_ids::lng_menu_contacts),
            "Contatos"
        );

        let recent = read_recent_languages(&local).expect("read recent languages");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "pt-br");
        assert_eq!(recent[1].id, "en");

        let mut restored_countries = CountriesInstance::new();
        assert!(
            restored_countries
                .load_from_local_storage(&local)
                .expect("load countries")
        );
        assert_eq!(restored_countries.hash(), 998877);
        assert_eq!(restored_countries.list().len(), 1);
        assert_eq!(
            restored_countries.format_phone("5511987654321"),
            "+55 11 98765 4321"
        );

        // 3. Verify passcode gate & unlock
        let storage = TdataDomainStorage::from_working_dir(&resolved.path, cfg.data_file());
        assert!(storage.needs_passcode().expect("needs_passcode true"));

        let mut domain = Domain::new(cfg.data_file(), storage);
        assert_eq!(
            domain.start(b"").expect("empty passcode rejected"),
            StartResult::IncorrectPasscode
        );
        assert_eq!(
            domain
                .start(b"wrong-pass")
                .expect("wrong passcode rejected"),
            StartResult::IncorrectPasscode
        );
        assert_eq!(
            domain.start(b"top-secret-pass-42").expect("unlock domain"),
            StartResult::Success
        );

        // 4. Verify 2 accounts, ordering, sessions, MTP authorizations (DCs 2 & 4), and drafts
        assert_eq!(domain.accounts().len(), 2);
        assert_eq!(domain.active_index(), Some(1));

        let ordered = domain.ordered_accounts(&restored_settings.accounts_order);
        let ordered_indices: Vec<i32> = ordered.iter().map(|a| a.index).collect();
        assert_eq!(ordered_indices, vec![1, 0]);

        let acc0_meta = domain.account(0).expect("account 0");
        assert_eq!(acc0_meta.main_dc_id, 2);
        assert_eq!(
            acc0_meta
                .session
                .as_ref()
                .map(|s| (s.user_id, s.username.as_str())),
            Some((auth_acc0.user_id as i64, "alice_synth"))
        );

        let acc1_meta = domain.account(1).expect("account 1");
        assert_eq!(acc1_meta.main_dc_id, 4);
        assert_eq!(
            acc1_meta
                .session
                .as_ref()
                .map(|s| (s.user_id, s.username.as_str())),
            Some((auth_acc1.user_id as i64, "bob_synth"))
        );

        let read_auth0 = domain
            .storage()
            .read_account_mtp_auth(0)
            .expect("read auth0")
            .expect("auth0 present");
        assert_eq!(read_auth0, auth_acc0);

        let read_auth1 = domain
            .storage()
            .read_account_mtp_auth(1)
            .expect("read auth1")
            .expect("auth1 present");
        assert_eq!(read_auth1, auth_acc1);

        let acc0_storage = domain
            .storage()
            .account_storage(0)
            .expect("acc0 storage present");
        let restored_drafts = acc0_storage
            .read_drafts_with_cursors(peer0)
            .expect("read drafts");
        assert_eq!(restored_drafts, vec![draft0]);
    }
}

#[test]
fn legacy_no_passcode_key_datas_and_pre_multi_account_map_paths() {
    // Part A: Legacy 3-blob `key_datas` with no passcode.
    {
        let tmp = TempDir::new().expect("tempdir legacy 3-blob");
        let tdata = tmp.path().join("tdata");
        let auth = MtpAuthorization {
            user_id: 555_111_222,
            main_dc_id: 2,
            keys: vec![
                MtpAuthKeyEntry {
                    dc_id: 2,
                    key: AuthKey::new([0x12u8; 256]),
                },
                MtpAuthKeyEntry {
                    dc_id: 4,
                    key: AuthKey::new([0x34u8; 256]),
                },
            ],
            keys_to_destroy: vec![],
        };

        {
            let storage = TdataDomainStorage::new(&tdata, "data");
            let mut domain = Domain::new("data", storage);
            assert_eq!(domain.start(b"").expect("start"), StartResult::Success);
            domain
                .storage()
                .write_account_mtp_auth(0, &auth)
                .expect("write mtp");
            domain
                .storage_mut()
                .set_legacy_passcode(b"")
                .expect("write legacy 3-blob no-passcode");
            assert!(
                domain
                    .storage()
                    .domain_storage()
                    .expect("ds")
                    .key_data()
                    .legacy
            );
            assert!(!domain.storage().has_passcode());
        }

        // Fresh restart with empty passcode must succeed.
        let reopened_storage = TdataDomainStorage::new(&tdata, "data");
        assert!(!reopened_storage.needs_passcode().expect("no passcode"));
        let mut reopened_domain = Domain::new("data", reopened_storage);
        assert_eq!(
            reopened_domain.start(b"").expect("start legacy 3-blob"),
            StartResult::Success
        );
        assert!(!reopened_domain.storage().has_passcode());
        assert_eq!(
            reopened_domain
                .storage()
                .read_account_mtp_auth(0)
                .expect("read auth"),
            Some(auth)
        );
    }

    // Part B: Pre-multi-account legacy single-account `map` with no `key_datas` and no passcode.
    {
        let tmp = TempDir::new().expect("tempdir legacy single-account");
        let tdata = tmp.path().join("tdata");
        let acc_dir = tdata.join(to_file_part(compute_data_name_key("data")));
        fs::create_dir_all(&acc_dir).expect("mkdir acc");

        let salt = [0x3Au8; LOCAL_ENCRYPT_SALT_SIZE];
        let wrap_key = create_legacy_local_key(b"", &salt);
        let local_key = AuthKey::new([0x77u8; 256]);
        let key_enc = prepare_encrypted(&local_key.data, &wrap_key).expect("wrap key");

        let map_payload = QDataStreamWriter::new();
        let map_enc = prepare_encrypted(map_payload.as_bytes(), &local_key).expect("enc map");

        let mut map_file = FileWriteDescriptor::new("map", &acc_dir);
        map_file.write_data(&salt);
        map_file.write_data(&key_enc);
        map_file.write_data(&map_enc);
        map_file.finish().expect("write legacy map");

        // Write account 0 MTP data encrypted with the same legacy local_key.
        let auth = MtpAuthorization {
            user_id: 999_000_111,
            main_dc_id: 4,
            keys: vec![MtpAuthKeyEntry {
                dc_id: 4,
                key: AuthKey::new([0x88u8; 256]),
            }],
            keys_to_destroy: vec![],
        };
        let acc_tmp = AccountStorage::open(&tdata, "data", 0, local_key).expect("open tmp");
        acc_tmp.write_mtp_data(&auth).expect("write legacy mtp");
        assert!(!tdata.join("key_datas").exists());

        // Start Domain via TdataDomainStorage: should detect no passcode, load legacy map,
        // migrate to key_datas, and recover MTP authorization.
        let storage = TdataDomainStorage::new(&tdata, "data");
        assert!(!storage.needs_passcode().expect("legacy needs_passcode"));
        let mut domain = Domain::new("data", storage);
        assert_eq!(
            domain.start(b"").expect("start legacy single-account"),
            StartResult::Success
        );
        assert!(tdata.join("key_datas").exists());
        assert_eq!(
            domain
                .storage()
                .read_account_mtp_auth(0)
                .expect("read auth"),
            Some(auth)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn end_to_end_debounced_account_writer_persists_across_restart() {
    let tmp = TempDir::new().expect("tempdir");
    let tdata = tmp.path().join("tdata");

    let storage = TdataDomainStorage::new(&tdata, "data");
    let mut domain = Domain::new("data", storage);
    assert_eq!(domain.start(b"").expect("start"), StartResult::Success);

    let local_key = domain.storage().local_key().expect("local key").clone();
    let acc_storage = domain.storage().account_storage(0).expect("acc0").clone();
    let shared_acc = Arc::new(Mutex::new(acc_storage));

    let writer = AsyncWriteManager::spawn();
    let scheduler = AccountWriteScheduler::spawn(Arc::clone(&shared_acc), writer.clone());

    let mut prefs = BTreeMap::new();
    prefs.insert(b"chat_zoom".to_vec(), b"125".to_vec());
    scheduler.write_prefs_delayed(prefs.clone());

    let locations = StoredLocations {
        locations: vec![StoredFileLocation {
            media_key: (1, 2),
            path: "D:/Downloads/doc.pdf".into(),
            bookmark: vec![],
            modified: QtDateTime::from_unix_millis_utc(1_710_000_000_000),
            size: 12345,
        }],
        aliases: vec![],
        downloads_serialized: vec![],
    };
    scheduler.write_locations_delayed(locations.clone());

    tokio::time::advance(Duration::from_millis(1_100)).await;
    scheduler.shutdown().await.expect("shutdown scheduler");
    writer.stop().await;

    // Fresh reopen from disk verifies debounced background writes landed durably.
    let reopened = AccountStorage::open(&tdata, "data", 0, local_key).expect("reopen acc0");
    assert_eq!(reopened.read_prefs().expect("prefs"), prefs);
    assert_eq!(reopened.read_locations().expect("locs"), locations);
}
