//! TGB-2951: a session the server no longer accepts logs out silently, like tdesktop.
//!
//! - A fatal `401` (`AUTH_KEY_UNREGISTERED`) on the first startup sync request asks the window
//!   host to log the account out, with no toast (`Main::Account::logOut` via the MTP global fail
//!   handler, `main/main_account.cpp:469-473`).
//! - `SESSION_REVOKED` arriving mid-session does the same.
//! - A startup sync that fails for another reason is only logged, never shown as a toast
//!   (`Api::Updates::differenceFail`, `api/api_updates.cpp:630-636`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_app::live::{MultiAccountFrameAction, MultiAccountWorkspaceHost};
use tg_app::notifications::manager::NotificationSystem;
use tg_app::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::tray::MockTrayPlatform;
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{Identifiable, RemoteCall, enums as tl_enums, functions as tl_fns};
use tokio::sync::broadcast;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAIN_DC: DcId = 2;

/// RPC fake for one account. While `fail` holds an error, every call answers with it; a `401`
/// on the main DC is also sent to the auth-failure stream, as `tg_mtproto::Client` does
/// (`Instance::Private::onErrorDefault`, `mtproto/mtp_instance.cpp:1666-1679`).
#[derive(Clone)]
struct AuthFailRpc {
    fail: Arc<Mutex<Option<RpcError>>>,
    calls: Arc<Mutex<Vec<u32>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    auth_fail_tx: broadcast::Sender<RpcError>,
    report_auth_failures: Arc<AtomicBool>,
}

impl AuthFailRpc {
    fn new(fail: Option<RpcError>) -> Self {
        let (updates_tx, _) = broadcast::channel(4);
        let (auth_fail_tx, _) = broadcast::channel(8);
        Self {
            fail: Arc::new(Mutex::new(fail)),
            calls: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
            auth_fail_tx,
            report_auth_failures: Arc::new(AtomicBool::new(true)),
        }
    }

    fn set_fail(&self, err: RpcError) {
        *self.fail.lock().expect("fail lock") = Some(err);
    }

    fn calls(&self) -> Vec<u32> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn answer<R: RemoteCall>(&self, dc_id: DcId, req: &R) -> Result<R::Return, InvocationError> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.calls.lock().expect("calls lock").push(ctor);
        let fail = self.fail.lock().expect("fail lock").clone();
        match fail {
            Some(err) => {
                if err.code == 401
                    && dc_id == MAIN_DC
                    && self.report_auth_failures.load(Ordering::Acquire)
                {
                    let _ = self.auth_fail_tx.send(err.clone());
                }
                Err(InvocationError::Rpc(err))
            }
            None => Err(InvocationError::Dropped),
        }
    }
}

impl RpcClient for AuthFailRpc {
    type Error = InvocationError;

    fn main_dc_id(&self) -> DcId {
        MAIN_DC
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        self.answer(MAIN_DC, req)
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, InvocationError> {
        self.answer(dc_id, req)
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// Starts the account's runtime the way `start_mtproto_erased_on` does: with the client's
/// auth-failure stream wired in.
fn start_runtime(
    rt: &tokio::runtime::Runtime,
    rpc: &AuthFailRpc,
    startup_sync: bool,
) -> Arc<SessionRuntime<tg_app::live::ErasedRpcClient>> {
    Arc::new(
        SessionRuntime::start_with_new_session_on(
            DataUserId(1000),
            tg_app::live::ErasedRpcClient::new(rpc.clone()),
            None,
            Some(rpc.auth_fail_tx.subscribe()),
            rt.handle().clone(),
            SessionRuntimeConfig {
                init_updates_state: startup_sync,
                run_get_difference_on_start: startup_sync,
                load_initial_dialogs: false,
                fetch_self_user_on_start: false,
                changes_capacity: 8,
                idle_ping_enabled: false,
                ..SessionRuntimeConfig::default()
            },
        )
        .expect("start session runtime"),
    )
}

fn one_account_host(
    runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    settings_dir: &std::path::Path,
) -> MultiAccountWorkspaceHost {
    let mut host = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        settings_dir.join("tgb2951.json"),
        NotificationSystem::new(
            NotificationPreferences::default(),
            NotifySettingsStore::default(),
        ),
        Box::new(MockTrayPlatform::default()),
    );
    host.add_account(
        0,
        DataUserId(1000),
        false,
        false,
        runtime,
        None,
        true,
        false,
    );
    host
}

/// Runs frames until the host asks to log out slot 0 or `deadline` passes. Fails if any frame
/// shows a toast. Returns whether the logout was asked for.
fn run_frames_until_logout(
    host: &mut MultiAccountWorkspaceHost,
    ctx: &egui::Context,
    deadline: Instant,
) -> bool {
    loop {
        let mut action = MultiAccountFrameAction::None;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            action = host.show(ctx, ui);
        });
        out.textures_delta.clear();
        let toast = host
            .account(0)
            .expect("slot 0")
            .workspace
            .controller()
            .layer_stack()
            .toast_text()
            .map(str::to_owned);
        assert_eq!(toast, None, "no toast is shown");
        if action == MultiAccountFrameAction::LogoutAccount(0) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn unregistered_key_on_the_first_sync_request_logs_out_without_a_toast() {
    let rt = tokio_rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let rpc = AuthFailRpc::new(Some(RpcError::new(401, "AUTH_KEY_UNREGISTERED")));
    let runtime = start_runtime(&rt, &rpc, true);
    let mut host = one_account_host(runtime, dir.path());
    let ctx = egui::Context::default();

    let logged_out = run_frames_until_logout(&mut host, &ctx, Instant::now() + TEST_TIMEOUT);
    assert!(logged_out, "a 401 on startup logs the account out");
    assert_eq!(
        rpc.calls().first().copied(),
        Some(tl_fns::updates::GetState::CONSTRUCTOR_ID),
        "the 401 answered the first sync request"
    );
}

/// The same logout comes from the startup error alone, for a client that has no auth-failure
/// stream (or whose stream has not been read yet when startup returns).
#[test]
fn unregistered_key_on_startup_logs_out_without_an_auth_failure_stream() {
    let rt = tokio_rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let rpc = AuthFailRpc::new(Some(RpcError::new(401, "AUTH_KEY_UNREGISTERED")));
    rpc.report_auth_failures.store(false, Ordering::Release);
    let runtime = start_runtime(&rt, &rpc, true);
    let mut host = one_account_host(runtime, dir.path());
    let ctx = egui::Context::default();

    assert!(
        run_frames_until_logout(&mut host, &ctx, Instant::now() + TEST_TIMEOUT),
        "the startup 401 itself logs the account out"
    );
}

#[test]
fn session_revoked_mid_session_logs_out_without_a_toast() {
    let rt = tokio_rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let rpc = AuthFailRpc::new(None);
    let runtime = start_runtime(&rt, &rpc, false);
    rt.block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
        .expect("wait_ready timeout")
        .expect("startup ready");
    let mut host = one_account_host(Arc::clone(&runtime), dir.path());
    let ctx = egui::Context::default();

    assert!(
        !run_frames_until_logout(&mut host, &ctx, Instant::now() + Duration::from_millis(200)),
        "a working session stays logged in"
    );

    // The session is revoked from another device; the next request gets the 401.
    rpc.set_fail(RpcError::new(401, "SESSION_REVOKED"));
    let res = rt.block_on(async { runtime.rpc().invoke(&tl_fns::help::GetNearestDc {}).await });
    assert!(res.is_err());

    assert!(
        run_frames_until_logout(&mut host, &ctx, Instant::now() + TEST_TIMEOUT),
        "SESSION_REVOKED logs the account out"
    );
}

#[test]
fn failed_startup_sync_shows_no_toast() {
    let rt = tokio_rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let rpc = AuthFailRpc::new(Some(RpcError::new(400, "TEST_SYNC_FAILED")));
    let runtime = start_runtime(&rt, &rpc, true);
    let mut host = one_account_host(runtime, dir.path());
    let ctx = egui::Context::default();

    // Frames run until the UI side has received the sync error, then a few more.
    let deadline = Instant::now() + TEST_TIMEOUT;
    let sync_error_seen = || {
        host.account(0)
            .expect("slot 0")
            .workspace
            .live_session()
            .bridge_state()
            .last_error()
            .is_some()
    };
    while !sync_error_seen() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(sync_error_seen(), "the startup sync failed");
    assert!(
        !run_frames_until_logout(&mut host, &ctx, Instant::now() + Duration::from_millis(300)),
        "a non-401 failure does not log out"
    );
}
