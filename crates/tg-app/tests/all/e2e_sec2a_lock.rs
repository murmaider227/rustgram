//! Security review #2 (SEC2-A) integration tests and before/after snapshots:
//! - L2: the passcode lock is app-wide across accounts and account switching is refused while locked
//! - L3/L10: locking closes Instant View / Mini App windows; Ctrl+Shift+P/B/T do nothing while locked
//! - SEC3-A A1: locking closes Recent Calls and Ctrl+Shift+H does nothing while locked

use std::path::Path;
use std::sync::Arc;

use egui::{Event, Key, Modifiers, Pos2, Rect, Vec2};
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_app::live::{MultiAccountFrameAction, MultiAccountWorkspaceHost};
use tg_app::live_host::host::{refuse_account_switch_while_locked, sync_app_wide_passcode_lock};
use tg_app::notifications::manager::NotificationSystem;
use tg_app::notifications::settings::{NotificationPreferences, NotifySettingsStore};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::tray::MockTrayPlatform;
use tg_app::webview::runtime_error::RuntimeErrorKind;
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{RemoteCall, enums as tl_enums};
use tokio::sync::broadcast;

/// RPC fake that answers every call with an error (no network, no account).
#[derive(Clone)]
struct OfflineRpc {
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl OfflineRpc {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(4);
        Self { updates_tx }
    }
}

impl RpcClient for OfflineRpc {
    type Error = InvocationError;

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        _req: &R,
    ) -> Result<R::Return, InvocationError> {
        Err(InvocationError::Rpc(RpcError::new(501, "OFFLINE_TEST")))
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        _req: &R,
    ) -> Result<R::Return, InvocationError> {
        Err(InvocationError::Rpc(RpcError::new(501, "OFFLINE_TEST")))
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn offline_runtime(
    rt: &tokio::runtime::Runtime,
    user: u64,
) -> Arc<SessionRuntime<tg_app::live::ErasedRpcClient>> {
    Arc::new(
        SessionRuntime::start_on(
            DataUserId(user),
            tg_app::live::ErasedRpcClient::new(OfflineRpc::new()),
            rt.handle().clone(),
            SessionRuntimeConfig {
                init_updates_state: false,
                run_get_difference_on_start: false,
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

fn two_account_host(
    rt: &tokio::runtime::Runtime,
    settings_dir: &Path,
) -> MultiAccountWorkspaceHost {
    let mut host = MultiAccountWorkspaceHost::new_headless_with_tray(
        ShellSettings::default(),
        settings_dir.join("sec2a_lock.json"),
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
        offline_runtime(rt, 1000),
        None,
        true,
        false,
    );
    host.add_account(
        1,
        DataUserId(2000),
        false,
        false,
        offline_runtime(rt, 2000),
        None,
        false,
        false,
    );
    let verifier: tg_app::settings::local_passcode::PasscodeVerifierFn =
        Arc::new(|candidate: &str| candidate == "2468");
    for slot in [0, 1] {
        host.account_mut(slot)
            .expect("slot")
            .workspace
            .controller_mut()
            .set_external_passcode_verifier(Some(Arc::clone(&verifier)));
    }
    host
}

fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("tokio runtime")
}

#[test]
fn l2_lock_is_app_wide_and_account_switch_is_refused() {
    let rt = tokio_rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut host = two_account_host(&rt, dir.path());
    let ctx = egui::Context::default();

    // Background account has an Instant View window open before the lock.
    host.account_mut(1)
        .expect("slot 1")
        .workspace
        .webview_host_mut()
        .open_instant_view_with_error("https://example.com/a", RuntimeErrorKind::ProcessCrashed);
    assert!(
        host.account(1)
            .expect("slot 1")
            .workspace
            .has_additional_windows_open()
    );

    host.active_workspace_mut()
        .expect("active")
        .controller_mut()
        .setup_passcode_lock();
    let locked = sync_app_wide_passcode_lock(&mut host, false, &ctx);
    assert!(locked, "locking the active account locks the app");
    let background = &host.account(1).expect("slot 1").workspace;
    assert!(
        background.controller().is_passcode_locked(),
        "every other account is locked too"
    );
    assert!(
        !background.has_additional_windows_open(),
        "other accounts' Instant View / Mini App windows are closed"
    );

    // A switch made during a locked frame (menu, tray, notification click) is undone.
    assert!(host.switch_to(1));
    let action = refuse_account_switch_while_locked(
        &mut host,
        true,
        0,
        MultiAccountFrameAction::SwitchedAccount(1),
    );
    assert_eq!(action, MultiAccountFrameAction::None);
    assert_eq!(host.active_slot(), 0, "still on the locked account");

    // Unlock: all accounts unlock together.
    host.active_workspace_mut()
        .expect("active")
        .controller_mut()
        .clear_passcode_lock();
    let locked = sync_app_wide_passcode_lock(&mut host, true, &ctx);
    assert!(!locked);
    assert!(
        !host
            .account(1)
            .expect("slot 1")
            .workspace
            .controller()
            .is_passcode_locked()
    );
    drop(host);
}

#[test]
fn l3_l10_lock_closes_windows_and_blocks_overlay_shortcuts() {
    let rt = tokio_rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut host = two_account_host(&rt, dir.path());
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0));

    let ws = host.active_workspace_mut().expect("active");
    ws.webview_host_mut()
        .open_instant_view_with_error("https://example.com/a", RuntimeErrorKind::ProcessCrashed);
    ws.controller_mut().setup_passcode_lock();

    for key in [Key::P, Key::B, Key::T] {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![
                Event::ModifiersChanged(Modifiers::COMMAND | Modifiers::SHIFT),
                Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::COMMAND | Modifiers::SHIFT,
                },
            ],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            let frame_ctx = ui.ctx().clone();
            let _ = ws.show(&frame_ctx, ui);
        });
        out.textures_delta.clear();
        assert!(
            !ws.payments_open(),
            "Ctrl+Shift+P must not open Payments while locked"
        );
        assert!(
            !ws.business_open(),
            "Ctrl+Shift+B must not open Business while locked"
        );
    }
    assert!(
        !ws.has_additional_windows_open(),
        "the lock closed Instant View and keeps everything closed"
    );
    drop(host);
}

/// SEC3-A A1: the lock closes Recent Calls, Ctrl+Shift+H does not reopen it, and the controller's
/// Recent Calls request is refused until unlock.
#[test]
fn a1_lock_closes_recent_calls_and_blocks_calls_shortcut() {
    let rt = tokio_rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut host = two_account_host(&rt, dir.path());
    let ctx = egui::Context::default();
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0));

    let ws = host.active_workspace_mut().expect("active");
    ws.calls_host_mut().open_recent_calls();
    assert!(ws.calls_host().is_recent_calls_open());
    ws.controller_mut().setup_passcode_lock();

    for _ in 0..2 {
        let raw = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![
                Event::ModifiersChanged(Modifiers::COMMAND | Modifiers::SHIFT),
                Event::Key {
                    key: Key::H,
                    physical_key: Some(Key::H),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::COMMAND | Modifiers::SHIFT,
                },
            ],
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            let frame_ctx = ui.ctx().clone();
            let _ = ws.show(&frame_ctx, ui);
        });
        out.textures_delta.clear();
        assert!(
            !ws.calls_host().is_recent_calls_open(),
            "Recent Calls must stay closed while locked"
        );
    }
    assert!(ws.calls_host().is_passcode_locked());
    ws.calls_host_mut().open_recent_calls();
    assert!(!ws.calls_host().is_recent_calls_open());
    drop(host);
}
