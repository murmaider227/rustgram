//! Settings reads return at once and load in the background (TGB-3070): the window no longer
//! waits for `account.getAuthorizations`, `contacts.getBlocked`, `account.getAccountTTL`,
//! `account.getPassword` or the notification requests the first time a screen asks.

use std::time::{Duration, Instant};

use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_tl::{
    Identifiable, Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};

use super::tests_reply_targets::{FakeRpcClient, PEER_ID, build_live, tl_message, tl_user_peer};
use crate::LiveSession;
use tg_app_core::settings::backend::{NotifyScope, SettingsBackend, SettingsRead};

/// Simulated round trip of the fake server.
const ROUND_TRIP: Duration = Duration::from_millis(400);

const ALL_READS: [SettingsRead; 7] = [
    SettingsRead::ActiveSessions,
    SettingsRead::BlockedUsers,
    SettingsRead::AccountTtl,
    SettingsRead::CloudPassword,
    SettingsRead::ScopeNotifications(NotifyScope::PrivateChats),
    SettingsRead::ScopeNotifications(NotifyScope::Groups),
    SettingsRead::ScopeNotifications(NotifyScope::Channels),
];

/// A session whose server answers the account TTL (365 days), the sessions list (TTL 90
/// days) and the blocked list after [`ROUND_TRIP`]; every other request fails after it.
fn slow_server() -> (FakeRpcClient, LiveSession) {
    let peer = DataPeerId::from_user(DataUserId(PEER_ID as u64));
    let (rpc, live) = build_live(
        peer,
        vec![tl_message(
            10,
            tl_user_peer(PEER_ID),
            PEER_ID,
            "hello",
            None,
        )],
    );
    rpc.set_handler(|ctor, _| {
        if ctor == tl_fns::account::SetAccountTTL::CONSTRUCTOR_ID {
            return Some(true.to_bytes());
        }
        std::thread::sleep(ROUND_TRIP);
        match ctor {
            c if c == tl_fns::account::GetAccountTTL::CONSTRUCTOR_ID => Some(
                tl_enums::AccountDaysTTL::AccountDaysTTL(tl_types::AccountDaysTTL { days: 365 })
                    .to_bytes(),
            ),
            c if c == tl_fns::account::GetAuthorizations::CONSTRUCTOR_ID => Some(
                tl_enums::account::Authorizations::Authorizations(
                    tl_types::account::Authorizations {
                        authorization_ttl_days: 90,
                        authorizations: Vec::new(),
                    },
                )
                .to_bytes(),
            ),
            c if c == tl_fns::contacts::GetBlocked::CONSTRUCTOR_ID => Some(
                tl_enums::contacts::Blocked::Blocked(tl_types::contacts::Blocked {
                    blocked: Vec::new(),
                    chats: Vec::new(),
                    users: Vec::new(),
                })
                .to_bytes(),
            ),
            _ => None,
        }
    });
    (rpc, live)
}

#[test]
fn settings_reads_return_before_the_round_trip_and_fill_in_later() {
    let (rpc, live) = slow_server();

    let started = Instant::now();
    assert_eq!(live.account_ttl_days(), 180, "placeholder");
    assert!(live.active_sessions().is_empty());
    assert_eq!(live.session_ttl_days(), 180, "placeholder");
    assert!(live.blocked_users().is_empty());
    let _ = live.cloud_password_state();
    for scope in NotifyScope::ALL {
        let _ = live.scope_notifications(scope);
    }
    let waited = started.elapsed();
    eprintln!("first Settings reads returned after {waited:?}");
    assert!(waited < ROUND_TRIP / 2, "the UI waited {waited:?}");
    for read in ALL_READS {
        assert!(
            live.settings_read_pending(read),
            "{read:?} shows its loading state"
        );
    }

    assert!(live.wait_settings_reads(Duration::from_secs(10)));
    for read in ALL_READS {
        assert!(!live.settings_read_pending(read), "{read:?} answered");
    }
    assert_eq!(live.account_ttl_days(), 365);
    assert_eq!(live.session_ttl_days(), 90);
    // Each read went out once, the notification scopes' two requests each together.
    assert_eq!(rpc.calls::<tl_fns::account::GetAccountTTL>().len(), 1);
    assert_eq!(rpc.calls::<tl_fns::account::GetAuthorizations>().len(), 1);
    assert_eq!(rpc.calls::<tl_fns::contacts::GetBlocked>().len(), 1);
    assert_eq!(rpc.calls::<tl_fns::account::GetPassword>().len(), 1);
    assert_eq!(rpc.calls::<tl_fns::account::GetNotifySettings>().len(), 3);
    assert_eq!(rpc.calls::<tl_fns::account::GetNotifyExceptions>().len(), 3);
}

#[test]
fn a_late_answer_does_not_overwrite_a_change_made_meanwhile() {
    let (_rpc, mut live) = slow_server();
    assert_eq!(
        live.account_ttl_days(),
        180,
        "placeholder, the request is on its way"
    );
    live.set_account_ttl_days(30);
    assert!(live.wait_settings_reads(Duration::from_secs(10)));
    assert_eq!(
        live.account_ttl_days(),
        30,
        "the server's older 365 days arrived later"
    );
}

#[test]
fn blocking_someone_while_the_list_loads_reloads_it() {
    let (rpc, live) = slow_server();
    assert!(live.blocked_users().is_empty());
    // A block from a profile while the first list is on its way.
    if let Ok(mut c) = live.cache.lock() {
        c.invalidate_blocked_users();
    }
    assert!(live.wait_settings_reads(Duration::from_secs(10)));
    let _ = live.blocked_users();
    assert!(live.wait_settings_reads(Duration::from_secs(10)));
    assert_eq!(
        rpc.calls::<tl_fns::contacts::GetBlocked>().len(),
        2,
        "the stale first answer is dropped and the list is asked again"
    );
}
