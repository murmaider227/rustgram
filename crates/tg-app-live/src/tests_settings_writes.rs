//! Settings writes apply at once and send in the background (TGB-3104): a toggle or choice
//! on the Privacy, Sessions or Notifications screen never waits for the server, as in
//! tdesktop (`SelfDestruct::updateAccountTTL`, `Authorizations::updateTTL` /
//! `toggleCallsDisabled` / `requestTerminate`, `NotifySettings::defaultUpdate`).

use std::time::{Duration, Instant};

use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_tl::{
    Identifiable, Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};

use super::tests_reply_targets::{FakeRpcClient, PEER_ID, build_live, tl_message, tl_user_peer};
use crate::LiveSession;
use tg_app_core::settings::backend::{NotifyScope, ScopeNotifySettings, SettingsBackend};

/// Simulated round trip of the fake server.
const ROUND_TRIP: Duration = Duration::from_millis(400);

const OTHER_SESSION: u64 = 77_001;

fn authorization(hash: u64) -> tl_enums::Authorization {
    tl_enums::Authorization::Authorization(tl_types::Authorization {
        current: hash == 0,
        official_app: true,
        password_pending: false,
        encrypted_requests_disabled: false,
        call_requests_disabled: false,
        unconfirmed: false,
        hash: hash as i64,
        device_model: "Desktop".to_owned(),
        platform: "Windows".to_owned(),
        system_version: "11".to_owned(),
        api_id: 2040,
        app_name: "Rustgram".to_owned(),
        app_version: "0.1".to_owned(),
        date_created: 1_700_000_000,
        date_active: 1_700_000_000,
        ip: "127.0.0.1".to_owned(),
        country: "CH".to_owned(),
        region: "ZH".to_owned(),
    })
}

/// A session whose server answers every request after [`ROUND_TRIP`]: the sessions list holds
/// this session and one other, every write succeeds.
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
        std::thread::sleep(ROUND_TRIP);
        match ctor {
            c if c == tl_fns::account::GetAuthorizations::CONSTRUCTOR_ID => Some(
                tl_enums::account::Authorizations::Authorizations(
                    tl_types::account::Authorizations {
                        authorization_ttl_days: 90,
                        authorizations: vec![authorization(0), authorization(OTHER_SESSION)],
                    },
                )
                .to_bytes(),
            ),
            c if c == tl_fns::account::SetAccountTTL::CONSTRUCTOR_ID
                || c == tl_fns::account::SetAuthorizationTTL::CONSTRUCTOR_ID
                || c == tl_fns::account::ResetAuthorization::CONSTRUCTOR_ID
                || c == tl_fns::auth::ResetAuthorizations::CONSTRUCTOR_ID
                || c == tl_fns::account::ChangeAuthorizationSettings::CONSTRUCTOR_ID
                || c == tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID =>
            {
                Some(true.to_bytes())
            }
            _ => None,
        }
    });
    (rpc, live)
}

/// Runs `write` and checks it returned well before one round trip.
fn returns_at_once(what: &str, write: impl FnOnce()) {
    let started = Instant::now();
    write();
    let took = started.elapsed();
    eprintln!("{what} returned after {took:?}");
    assert!(
        took < ROUND_TRIP / 2,
        "{what} waited {took:?} for the server"
    );
}

#[test]
fn settings_writes_apply_at_once_and_send_in_the_background() {
    let (rpc, mut live) = slow_server();
    // Load the sessions list first so the session switches have something to change.
    let _ = live.active_sessions();
    assert!(live.wait_settings_reads(Duration::from_secs(10)));
    assert_eq!(live.active_sessions().len(), 2);

    returns_at_once("set_account_ttl_days", || live.set_account_ttl_days(90));
    assert_eq!(live.account_ttl_days(), 90, "shown at once");

    returns_at_once("set_session_ttl_days", || live.set_session_ttl_days(30));
    assert_eq!(live.session_ttl_days(), 30, "shown at once");

    returns_at_once("update_session_permissions", || {
        live.update_session_permissions(OTHER_SESSION, false, false);
    });
    let other = live
        .active_sessions()
        .into_iter()
        .find(|s| s.hash == OTHER_SESSION)
        .expect("other session");
    assert!(
        !other.accept_calls && !other.accept_secret_chats,
        "shown at once"
    );

    let muted = ScopeNotifySettings {
        enabled: false,
        ..ScopeNotifySettings::default()
    };
    returns_at_once("set_scope_notifications", || {
        live.set_scope_notifications(NotifyScope::Groups, muted.clone());
    });
    assert!(!live.scope_notifications(NotifyScope::Groups).enabled);

    // Everything reaches the server, in the order it was changed.
    assert!(live.flush_background_rpcs(Duration::from_secs(20)));
    assert_eq!(
        rpc.calls::<tl_fns::account::SetAccountTTL>().len(),
        1,
        "account.setAccountTTL"
    );
    assert_eq!(rpc.calls::<tl_fns::account::SetAuthorizationTTL>().len(), 1);
    let changed = rpc.calls::<tl_fns::account::ChangeAuthorizationSettings>();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].hash, OTHER_SESSION as i64);
    assert_eq!(changed[0].call_requests_disabled, Some(true));
    assert_eq!(changed[0].encrypted_requests_disabled, Some(true));
    assert_eq!(
        rpc.calls::<tl_fns::account::UpdateNotifySettings>().len(),
        1
    );
    // A late answer of an older read does not undo the changes.
    assert_eq!(live.account_ttl_days(), 90);
    assert_eq!(live.session_ttl_days(), 30);
}

#[test]
fn terminating_a_session_returns_at_once_and_drops_it_when_the_server_confirms() {
    let (rpc, mut live) = slow_server();
    let _ = live.active_sessions();
    assert!(live.wait_settings_reads(Duration::from_secs(10)));
    assert_eq!(live.active_sessions().len(), 2);

    let mut accepted = false;
    returns_at_once("terminate_session", || {
        accepted = live.terminate_session(OTHER_SESSION);
    });
    assert!(accepted);
    assert!(
        live.active_sessions()
            .iter()
            .any(|s| s.hash == OTHER_SESSION),
        "tdesktop removes the session when the server confirms"
    );
    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    assert!(
        live.active_sessions()
            .iter()
            .all(|s| s.hash != OTHER_SESSION),
        "the confirmed session leaves the list"
    );
    assert_eq!(rpc.calls::<tl_fns::account::ResetAuthorization>().len(), 1);

    let mut count = 0;
    returns_at_once("terminate_all_other_sessions", || {
        count = live.terminate_all_other_sessions();
    });
    assert_eq!(count, 0, "no other session is left");
    assert!(live.flush_background_rpcs(Duration::from_secs(10)));
    assert_eq!(rpc.calls::<tl_fns::auth::ResetAuthorizations>().len(), 1);
    assert_eq!(live.active_sessions().len(), 1, "this session stays");
}
