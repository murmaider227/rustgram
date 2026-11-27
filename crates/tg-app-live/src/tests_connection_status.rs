//! The connection indicator and a late `SessionEvent::Ready` (TGB-3077): once the MTProto
//! transport reports its own state, the session finishing its startup must not mark a
//! reconnecting transport as connected.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tg_data::UserId as DataUserId;
use tg_session::{SessionEvent, SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Identifiable, Serializable, enums as tl_enums, functions as tl_fns, types as tl_types,
};

use super::tests_reply_targets::{FakeRpcClient, SELF_ID, test_runtime_config, wait_until};
use crate::{LiveBridgeState, LiveRuntimeBridge};
use tg_app_core::stand_in::types::ConnectionStatus;

/// A runtime whose startup (`updates.getState`) waits until `release` is set, so its
/// [`SessionEvent::Ready`] arrives whenever the test wants it to.
fn runtime_with_held_startup(
    release: &Arc<AtomicBool>,
) -> (SessionRuntime<crate::ErasedRpcClient>, LiveRuntimeBridge) {
    let rpc = FakeRpcClient::new();
    let gate = Arc::clone(release);
    rpc.set_handler(move |ctor, _| {
        if ctor != tl_fns::updates::GetState::CONSTRUCTOR_ID {
            return None;
        }
        while !gate.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Some(
            tl_enums::updates::State::State(tl_types::updates::State {
                pts: 1,
                qts: 0,
                date: 1_700_000_000,
                seq: 1,
                unread_count: 0,
            })
            .to_bytes(),
        )
    });
    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        ..test_runtime_config()
    };
    let runtime = SessionRuntime::start(
        DataUserId(SELF_ID as u64),
        crate::ErasedRpcClient::new(rpc),
        cfg,
    )
    .expect("start SessionRuntime");
    let bridge = LiveRuntimeBridge::spawn(&runtime, 1);
    (runtime, bridge)
}

/// Lets the held startup finish and waits until the bridge has handled its `Ready`.
fn deliver_ready(release: &AtomicBool, state: &LiveBridgeState) {
    release.store(true, Ordering::Release);
    let mut seen = false;
    assert!(
        wait_until(|| {
            seen |= state
                .drain_session_events()
                .iter()
                .any(|ev| matches!(ev, SessionEvent::Ready));
            seen
        }),
        "the session never became ready"
    );
}

#[test]
fn late_ready_does_not_hide_a_transport_reconnect() {
    let release = Arc::new(AtomicBool::new(false));
    let (runtime, bridge) = runtime_with_held_startup(&release);
    let state = Arc::clone(bridge.state());
    assert!(!runtime.is_ready());

    // The live proxy controller hands the indicator to the transport, then a proxy switch
    // starts a reconnect (`LiveProxyController::apply_settings_internal`).
    state.follow_transport_connection_state();
    state.set_connection_status(ConnectionStatus::Connecting);

    deliver_ready(&release, &state);
    assert_eq!(state.connection_status(), ConnectionStatus::Connecting);
}

#[test]
fn ready_marks_the_connection_up_without_a_transport_watch() {
    let release = Arc::new(AtomicBool::new(false));
    let (_runtime, bridge) = runtime_with_held_startup(&release);
    let state = Arc::clone(bridge.state());
    assert_eq!(state.connection_status(), ConnectionStatus::Connecting);

    deliver_ready(&release, &state);
    assert_eq!(state.connection_status(), ConnectionStatus::Connected);
}
