//! Multi-account `tdata` layout, Premium limit (3 -> 4..6), background slot metadata,
//! active-account logout handoff, and restart persistence tests for `tg-session::AccountManager`.
//!
//! Uses only isolated `tempfile::TempDir` directories, `FakeTransportFactory`, and synthetic
//! users/keys. Never contacts real Telegram servers or uses real credentials.

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tg_api::ApiCredentials;
use tg_core::domain::{DomainError, Environment};
use tg_crypto::AuthKey;
use tg_data::UserId;
use tg_mtproto::FakeTransportFactory;
use tg_session::{
    AccountManager, MAX_ACCOUNTS, PREMIUM_MAX_ACCOUNTS, SessionError, WorkingDirOpenOptions,
};
use tg_storage::tdf::{compose_data_string, compute_data_name_key, to_file_part};
use tg_tl::types as tl_types;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn synthetic_credentials() -> ApiCredentials {
    ApiCredentials {
        api_id: 99_002,
        api_hash: "fedcba9876543210fedcba9876543210".to_owned(),
    }
}

fn synthetic_user(
    id: i64,
    first_name: &str,
    last_name: &str,
    username: &str,
    phone: &str,
    premium: bool,
) -> tl_types::User {
    tl_types::User {
        self_: true,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 1000 + 17),
        first_name: Some(first_name.to_owned()),
        last_name: Some(last_name.to_owned()),
        username: Some(username.to_owned()),
        phone: Some(phone.to_owned()),
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn open_opts(dir: &std::path::Path) -> WorkingDirOpenOptions {
    WorkingDirOpenOptions::new(dir, synthetic_credentials())
        .with_transport_factory(Arc::new(FakeTransportFactory::new()))
        .with_pfs(false)
        .with_logout_timeout(Duration::from_millis(120))
        .with_destroy_keys_timeout(Duration::from_millis(120))
}

#[tokio::test]
async fn tdata_per_account_layout_limits_and_restart_roundtrip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        assert_eq!(MAX_ACCOUNTS, 3);
        assert_eq!(PREMIUM_MAX_ACCOUNTS, 6);

        let tmp = TempDir::new().expect("tempdir");
        let tdata = tmp.path().join("tdata");

        let key0 = AuthKey::new([0x10u8; 256]);
        let key1 = AuthKey::new([0x20u8; 256]);
        let key2 = AuthKey::new([0x30u8; 256]);
        let key3 = AuthKey::new([0x40u8; 256]);

        let expected_stem0 = to_file_part(compute_data_name_key(&compose_data_string("data", 0)));
        let expected_stem1 = to_file_part(compute_data_name_key(&compose_data_string("data", 1)));
        let expected_stem2 = to_file_part(compute_data_name_key(&compose_data_string("data", 2)));
        let expected_stem3 = to_file_part(compute_data_name_key(&compose_data_string("data", 3)));
        assert_eq!(expected_stem0, "D877F783D5D3EF8C");

        // Phase 1: Create 3 non-premium accounts -> 4th rejected -> upgrade account 1 to Premium -> 4th succeeds.
        {
            let mut mgr = AccountManager::open_and_unlock(open_opts(tmp.path()), b"")
                .expect("open and unlock");
            assert_eq!(mgr.max_accounts(), 3);
            assert_eq!(mgr.account_data_name(0), "data");
            assert_eq!(mgr.account_key_file_part(0), expected_stem0);

            // Slot 0 ("data")
            mgr.account_mut(0)
                .expect("slot 0")
                .client
                .import_persistent_keys(vec![(2, key0.clone())]);
            let user0 = synthetic_user(10_001, "Alpha", "One", "alpha_one", "+10000000001", false);
            mgr.complete_login(0, &user0).expect("login 0");

            // Slot 1 ("data#2")
            let idx1 = mgr
                .add_account(Environment::Production)
                .expect("add slot 1");
            assert_eq!(idx1, 1);
            assert_eq!(mgr.account_data_name(idx1), "data#2");
            assert_eq!(mgr.account_key_file_part(idx1), expected_stem1);
            mgr.account_mut(idx1)
                .expect("slot 1")
                .client
                .import_persistent_keys(vec![(2, key1.clone())]);
            let user1 = synthetic_user(10_002, "Beta", "Two", "beta_two", "+10000000002", false);
            mgr.complete_login(idx1, &user1).expect("login 1");

            // Slot 2 ("data#3")
            let idx2 = mgr
                .add_account(Environment::Production)
                .expect("add slot 2");
            assert_eq!(idx2, 2);
            assert_eq!(mgr.account_data_name(idx2), "data#3");
            assert_eq!(mgr.account_key_file_part(idx2), expected_stem2);
            mgr.account_mut(idx2)
                .expect("slot 2")
                .client
                .import_persistent_keys(vec![(4, key2.clone())]);
            let user2 = synthetic_user(
                10_003,
                "Gamma",
                "Three",
                "gamma_three",
                "+10000000003",
                false,
            );
            mgr.complete_login(idx2, &user2).expect("login 2");

            assert_eq!(mgr.accounts_authed_count(), 3);
            assert_eq!(mgr.max_accounts(), 3);
            assert!(!mgr.can_add_account());

            // 4th account blocked without Premium.
            let err = mgr
                .add_account(Environment::Production)
                .expect_err("4th account must be rejected without Premium");
            assert!(matches!(
                err,
                SessionError::Domain(DomainError::AccountLimitReached { limit: 3 })
            ));

            // Upgrade slot 1 to Premium -> cap becomes 4.
            let user1_premium =
                synthetic_user(10_002, "Beta", "Two", "beta_two", "+10000000002", true);
            mgr.update_account_user(idx1, &user1_premium)
                .expect("update user 1 premium");
            assert_eq!(mgr.max_accounts(), 4);
            assert!(mgr.can_add_account());

            // Now adding slot 3 ("data#4") succeeds.
            let idx3 = mgr
                .add_account(Environment::Production)
                .expect("add slot 3 with Premium");
            assert_eq!(idx3, 3);
            assert_eq!(mgr.account_data_name(idx3), "data#4");
            assert_eq!(mgr.account_key_file_part(idx3), expected_stem3);
            mgr.account_mut(idx3)
                .expect("slot 3")
                .client
                .import_persistent_keys(vec![(2, key3.clone())]);
            let user3 =
                synthetic_user(10_004, "Delta", "Four", "delta_four", "+10000000004", false);
            mgr.complete_login(idx3, &user3).expect("login 3");

            // 5th account blocked when only 1 account is Premium (limit = 4).
            let err5 = mgr
                .add_account(Environment::Production)
                .expect_err("5th account must be rejected with 1 Premium account");
            assert!(matches!(
                err5,
                SessionError::Domain(DomainError::AccountLimitReached { limit: 4 })
            ));

            // Switch active to slot 1 before exit and save all accounts.
            mgr.switch_account(idx1).expect("switch to slot 1");
            mgr.save_all_accounts().expect("save all accounts");
            mgr.shutdown();
        }

        // Verify exact on-disk tdata layout matches tdesktop (`key_datas`, `<stem>s`, `<stem>/maps`).
        assert!(tdata.join("key_datas").exists());
        for stem in [
            &expected_stem0,
            &expected_stem1,
            &expected_stem2,
            &expected_stem3,
        ] {
            assert!(
                tdata.join(format!("{stem}s")).exists(),
                "missing per-account MTP key file {stem}s"
            );
            assert!(
                tdata.join(stem).join("maps").exists(),
                "missing per-account maps file {stem}/maps"
            );
        }

        // Phase 2: Reopen from disk and confirm all 4 accounts, Premium limit, display names,
        // active slot, and auth keys round-tripped cleanly.
        {
            let mut reopened = AccountManager::open_and_unlock(open_opts(tmp.path()), b"")
                .expect("reopen after restart");
            assert_eq!(reopened.active_index(), Some(1));
            assert_eq!(reopened.accounts_authed_count(), 4);
            assert_eq!(
                reopened.max_accounts(),
                4,
                "Premium flag must persist across restart and keep max_accounts() == 4"
            );

            let slots = reopened.account_slots();
            assert_eq!(slots.len(), 4);
            assert_eq!(slots[0].data_name, "data");
            assert_eq!(slots[0].display_name, "Alpha One");
            assert_eq!(slots[0].user_id, Some(UserId(10_001)));
            assert!(!slots[0].premium);
            assert!(!slots[0].is_active);

            assert_eq!(slots[1].data_name, "data#2");
            assert_eq!(slots[1].display_name, "Beta Two");
            assert_eq!(slots[1].user_id, Some(UserId(10_002)));
            assert!(slots[1].premium);
            assert!(slots[1].is_active);

            assert_eq!(slots[2].data_name, "data#3");
            assert_eq!(slots[2].display_name, "Gamma Three");
            assert_eq!(slots[2].user_id, Some(UserId(10_003)));

            assert_eq!(slots[3].data_name, "data#4");
            assert_eq!(slots[3].display_name, "Delta Four");
            assert_eq!(slots[3].user_id, Some(UserId(10_004)));

            // Logging out the active account (slot 1) removes its files and switches to a remaining account.
            let _ = reopened
                .logout_account(1)
                .await
                .expect("logout active slot 1");
            assert!(reopened.account(1).is_none());
            assert_eq!(reopened.accounts_authed_count(), 3);
            let new_active = reopened.active_index().expect("remaining active slot");
            assert_ne!(new_active, 1);
            assert!(
                reopened
                    .account(new_active)
                    .expect("new active account")
                    .is_authorized()
            );
            assert!(
                !tdata.join(format!("{expected_stem1}s")).exists(),
                "logged-out account key file must be wiped"
            );
            assert!(
                !tdata.join(&expected_stem1).exists(),
                "logged-out account directory must be wiped"
            );

            reopened.shutdown();
        }
    })
    .await
    .expect("test timed out");
}

#[tokio::test]
async fn cancel_pending_add_account_prunes_slot_and_preserves_existing_accounts() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let tmp = TempDir::new().expect("tempdir");
        let tdata = tmp.path().join("tdata");
        let mut mgr =
            AccountManager::open_and_unlock(open_opts(tmp.path()), b"").expect("open and unlock");

        let user0 = synthetic_user(
            20_001,
            "Primary",
            "User",
            "primary_u",
            "+10000000101",
            false,
        );
        mgr.complete_login(0, &user0).expect("login 0");

        // Start adding a second account, then cancel before login completes.
        let pending_idx = mgr
            .add_account(Environment::Production)
            .expect("add pending slot");
        assert_eq!(pending_idx, 1);
        assert_eq!(mgr.active_index(), Some(1));
        assert_eq!(mgr.account_slots().len(), 2);

        let stem1 = mgr.account_key_file_part(pending_idx);
        mgr.cancel_pending_account(0).expect("cancel pending add");
        assert_eq!(mgr.active_index(), Some(0));
        assert_eq!(mgr.account_slots().len(), 1);
        assert!(mgr.account(pending_idx).is_none());
        assert!(!tdata.join(&stem1).exists());

        // Live Premium toggle updates max_accounts() immediately and persists across restart.
        assert_eq!(mgr.max_accounts(), 3);
        mgr.set_account_premium(0, true)
            .expect("set account 0 premium");
        assert_eq!(mgr.max_accounts(), 4);
        mgr.save_all_accounts().expect("save all");
        mgr.shutdown();

        let mut reopened = AccountManager::open_and_unlock(open_opts(tmp.path()), b"")
            .expect("reopen after premium toggle");
        assert_eq!(reopened.max_accounts(), 4);
        assert!(reopened.account_slots()[0].premium);
        reopened.shutdown();
    })
    .await
    .expect("test timed out");
}
