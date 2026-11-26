//! TGB-2951: a fatal `401` on the startup sync logs the account out instead of reporting a sync
//! error (`Instance::Private::onErrorDefault`, `mtproto/mtp_instance.cpp:1666-1679`;
//! `Main::Account::startMtp`, `main/main_account.cpp:469-473`).

use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::UserId;
use tg_session::{LogoutReason, SessionEvent, SessionRuntime, SessionRuntimeConfig};
use tg_tl::{RemoteCall, enums as tl_enums};
use tokio::sync::broadcast;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Answers every call with one RPC error and has no auth-failure stream.
#[derive(Clone)]
struct FailingRpc {
    err: RpcError,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FailingRpc {
    fn new(code: i32, name: &str) -> Self {
        let (updates_tx, _) = broadcast::channel(4);
        Self {
            err: RpcError::new(code, name),
            updates_tx,
        }
    }
}

impl RpcClient for FailingRpc {
    type Error = InvocationError;

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        _req: &R,
    ) -> Result<R::Return, InvocationError> {
        Err(InvocationError::Rpc(self.err.clone()))
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        _req: &R,
    ) -> Result<R::Return, InvocationError> {
        Err(InvocationError::Rpc(self.err.clone()))
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

async fn first_startup_event(rpc: FailingRpc) -> SessionEvent {
    let rt = SessionRuntime::start_on(
        UserId(42),
        rpc,
        tokio::runtime::Handle::current(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: true,
            load_initial_dialogs: true,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start runtime");
    let mut events = rt.subscribe_events();
    let event = tokio::time::timeout(TEST_TIMEOUT, events.recv())
        .await
        .expect("startup event in time")
        .expect("event");
    rt.shutdown_async().await;
    event
}

#[tokio::test]
async fn fatal_401_names_on_startup_log_out() {
    for name in [
        "AUTH_KEY_UNREGISTERED",
        "AUTH_KEY_INVALID",
        "SESSION_REVOKED",
        "SESSION_EXPIRED",
        "USER_DEACTIVATED",
    ] {
        assert_eq!(
            first_startup_event(FailingRpc::new(401, name)).await,
            SessionEvent::ForceLogout {
                reason: LogoutReason::Unauthorized {
                    error: name.to_owned(),
                },
            },
            "{name}"
        );
    }
}

#[tokio::test]
async fn other_startup_failures_stay_sync_errors() {
    // `AUTH_KEY_PERM_EMPTY` is handled inside MTProto (temporary key binding), never a logout.
    for (code, name) in [(401, "AUTH_KEY_PERM_EMPTY"), (400, "SOMETHING_WRONG")] {
        let event = first_startup_event(FailingRpc::new(code, name)).await;
        assert!(
            matches!(event, SessionEvent::SyncError(_)),
            "{code} {name}: {event:?}"
        );
    }
}
