//! Privacy rows of Settings > Privacy (`account.getPrivacy`, `Api::UserPrivacy::reload`).
//!
//! The section reads every key on every frame; a key is requested once (a failure is not asked
//! again each frame) and the missing keys go out together, so the first frame waits for one
//! round trip instead of one per row.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_app_core::settings::backend::{PrivacyKey, PrivacyOption, SettingsBackend};
use tg_data::{User, UserId as DataUserId};
use tg_session::SessionRuntime;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};

use super::tests_reply_targets::test_runtime_config;
use crate::LiveSession;

/// Fake server: every request takes `delay`; `account.getPrivacy` answers "everybody" (or fails
/// when `fail`), anything else fails.
#[derive(Clone, Default)]
struct PrivacyServer {
    delay: Duration,
    fail: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    other: Arc<Mutex<Vec<u32>>>,
}

impl RpcClient for PrivacyServer {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes(bytes[..4].try_into().unwrap_or([0; 4]));
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if ctor != tl_fns::account::GetPrivacy::CONSTRUCTOR_ID {
            if let Ok(mut other) = self.other.lock() {
                other.push(ctor);
            }
            return Err(InvocationError::Dropped);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(InvocationError::Dropped);
        }
        let answer =
            tl_enums::account::PrivacyRules::PrivacyRules(tl_types::account::PrivacyRules {
                rules: vec![tl_enums::PrivacyRule::PrivacyValueAllowAll],
                chats: Vec::new(),
                users: Vec::new(),
            });
        R::Return::from_bytes(&answer.to_bytes())
            .map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke(req).await
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}
}

fn live_with(server: &PrivacyServer) -> LiveSession {
    let runtime = SessionRuntime::start(
        DataUserId(100),
        crate::ErasedRpcClient::new(server.clone()),
        test_runtime_config(),
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(runtime), 1);
    live.write_session(|s| {
        let mut me = User::new(DataUserId(100), true);
        me.first_name = "Self".to_owned();
        s.upsert_user(me, tg_data::PeerUpdateFlags::NAME);
    });
    live
}

#[test]
fn a_failed_privacy_key_is_not_requested_again_on_every_frame() {
    let server = PrivacyServer::default();
    server.fail.store(true, Ordering::SeqCst);
    let live = live_with(&server);

    for _frame in 0..3 {
        let rules = live.privacy_rules();
        assert_eq!(rules.len(), PrivacyKey::ALL.len());
    }
    assert_eq!(
        server.calls.load(Ordering::SeqCst),
        PrivacyKey::ALL.len(),
        "each key is asked once, not once per frame"
    );

    // Reopening the screen (the caches are invalidated) asks again.
    live.invalidate_settings_caches();
    server.fail.store(false, Ordering::SeqCst);
    let rules = live.privacy_rules();
    assert_eq!(
        server.calls.load(Ordering::SeqCst),
        2 * PrivacyKey::ALL.len()
    );
    assert!(
        rules
            .values()
            .all(|rule| rule.option == PrivacyOption::Everybody)
    );
}

#[test]
fn the_privacy_section_asks_for_all_keys_at_once() {
    let round_trip = Duration::from_millis(100);
    let server = PrivacyServer {
        delay: round_trip,
        ..PrivacyServer::default()
    };
    let live = live_with(&server);

    let started = Instant::now();
    let rules = live.privacy_rules();
    let first_frame = started.elapsed();
    eprintln!(
        "privacy_rules() first frame: {first_frame:?} for {} keys, {round_trip:?} round trip",
        PrivacyKey::ALL.len()
    );
    assert!(
        rules
            .values()
            .all(|rule| rule.option == PrivacyOption::Everybody)
    );
    assert_eq!(server.calls.load(Ordering::SeqCst), PrivacyKey::ALL.len());
    assert!(
        server.max_in_flight.load(Ordering::SeqCst) > 1,
        "the keys were requested one after another"
    );

    // Later frames answer from the cache.
    let started = Instant::now();
    let again = live.privacy_rule(PrivacyKey::LastSeen);
    eprintln!("privacy_rule() cached: {:?}", started.elapsed());
    assert_eq!(again.option, PrivacyOption::Everybody);
    assert_eq!(server.calls.load(Ordering::SeqCst), PrivacyKey::ALL.len());
}

#[test]
fn the_global_privacy_requests_go_out_together() {
    let round_trip = Duration::from_millis(100);
    let server = PrivacyServer {
        delay: round_trip,
        ..PrivacyServer::default()
    };
    let live = live_with(&server);

    let started = Instant::now();
    let _ = crate::settings_sync::fetch_global_privacy_live(&live);
    eprintln!(
        "fetch_global_privacy_live first frame: {:?}, {round_trip:?} round trip",
        started.elapsed()
    );
    let asked = server.other.lock().map(|o| o.len()).unwrap_or_default();
    assert_eq!(asked, 3);
    assert_eq!(
        server.max_in_flight.load(Ordering::SeqCst),
        3,
        "the three requests went out one after another"
    );
}
