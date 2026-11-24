//! Tests for `rustgram-widget-helper.exe` path resolution from startup settings into
//! `WindowController` -> `MainWidget` -> `HistoryWidget` -> `LiveWidgetHost`, and framed IPC
//! launch/exchange using `FakeWidgetSandboxLauncher` (zero real OS process spawning).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;
use tg_app::stand_in::mock_session::MockSession;
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::startup::{StartupPlan, StartupPrefs};
use tg_app::webview::widget_host::LiveWidgetHost;
use tg_app::window::controller::WindowController;
use tg_core::launcher::WorkDirEnv;
use tg_platform::widget_sandbox::{
    FakeWidgetSandboxLauncher, WIDGET_HELPER_EXE_NAME, WidgetHelperChannel,
    resolve_widget_helper_path, serve_widget_helper_channel,
};
use tg_tgw::{ControlMessage, RuntimeState};

#[test]
fn startup_plan_resolves_widget_helper_path_next_to_main_exe_and_forwards_to_window() {
    let dir = TempDir::new().expect("temp dir");
    let install_dir = dir.path().join("install");
    let env = WorkDirEnv {
        exe_dir: install_dir.clone(),
        app_data_dir: Some(dir.path().join("appdata")),
        current_dir: dir.path().join("cwd"),
    };
    let main_exe = install_dir.join("rustgram.exe");
    let expected_helper = install_dir.join(WIDGET_HELPER_EXE_NAME);

    // 1. Direct helper path resolution from main executable path.
    assert_eq!(resolve_widget_helper_path(&main_exe), expected_helper);

    // 2. StartupPlan resolves widget_helper_path next to main_exe.
    let plan = StartupPlan::build(
        ["rustgram.exe"],
        &env,
        Some(&main_exe),
        &StartupPrefs::default(),
    );
    assert_eq!(plan.widget_helper_path, expected_helper);

    // 3. Bare executable name falls back to env.exe_dir.
    let fallback_plan = StartupPlan::build(
        ["rustgram.exe"],
        &env,
        Some(Path::new("rustgram.exe")),
        &StartupPrefs::default(),
    );
    assert_eq!(fallback_plan.widget_helper_path, expected_helper);

    // 4. WindowController forwards widget_helper_path down to MainWidget and HistoryWidget.
    let settings_path = dir.path().join("shell_state.json");
    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        settings_path,
    );
    assert_eq!(ctrl.widget_helper_path(), None);
    ctrl.set_widget_helper_path(plan.widget_helper_path.clone());
    assert_eq!(ctrl.widget_helper_path(), Some(expected_helper.as_path()));
    assert_eq!(
        ctrl.main_widget().history().widget_helper_path(),
        Some(expected_helper.as_path())
    );
}

#[test]
fn live_widget_host_launches_resolved_helper_with_fake_sandbox_launcher_and_framed_ipc() {
    let dir = TempDir::new().expect("temp dir");
    let install_dir = dir.path().join("bin");
    let env = WorkDirEnv {
        exe_dir: install_dir.clone(),
        app_data_dir: Some(dir.path().join("appdata")),
        current_dir: dir.path().join("cwd"),
    };
    let main_exe = install_dir.join("rustgram.exe");
    let plan = StartupPlan::build(
        ["rustgram.exe"],
        &env,
        Some(&main_exe),
        &StartupPrefs::default(),
    );
    let expected_helper = install_dir.join(WIDGET_HELPER_EXE_NAME);
    assert_eq!(plan.widget_helper_path, expected_helper);

    let launcher = Arc::new(FakeWidgetSandboxLauncher::new());
    let mut host = LiveWidgetHost::new(PeerId(42), MsgId(101), b"<html>widget</html>", false)
        .with_sandbox_launcher(launcher.clone())
        .with_helper_executable(plan.widget_helper_path.clone());
    assert_eq!(host.helper_executable(), Some(expected_helper.as_path()));
    assert_eq!(host.resolved_helper_executable(), expected_helper);

    let pid = host
        .launch_configured_sandbox_helper()
        .expect("launch sandbox helper via FakeWidgetSandboxLauncher");
    assert_eq!(host.sandbox_pid(), Some(pid));
    assert_eq!(host.state(), RuntimeState::Live);

    // Verify FakeWidgetSandboxLauncher received the exact resolved rustgram-widget-helper path.
    let launches = launcher.launched_configs();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].0, pid);
    assert_eq!(launches[0].1.executable, expected_helper);

    // Drive the actual helper framed IPC serve loop against the FakeWidgetChannel peer end.
    let helper_channel = launcher
        .take_peer_channel(pid)
        .expect("helper peer channel");
    let helper_thread = std::thread::spawn(move || serve_widget_helper_channel(&helper_channel));

    let open_bytes = ControlMessage::Open(2.0).encode().expect("encode Open");
    host.send_sandbox_frame(&open_bytes).expect("send Open");
    let content_reply = host.recv_sandbox_frame().expect("recv Content");
    let loaded_reply = host.recv_sandbox_frame().expect("recv Loaded");
    assert_eq!(
        ControlMessage::decode(&content_reply).expect("decode Content"),
        ControlMessage::Content
    );
    assert_eq!(
        ControlMessage::decode(&loaded_reply).expect("decode Loaded"),
        ControlMessage::Loaded
    );

    host.send_sandbox_frame(br#"{"type":"ready"}"#)
        .expect("send ready JSON");
    assert_eq!(
        host.recv_sandbox_frame().expect("recv ready JSON"),
        br#"{"type":"ready"}"#
    );

    let stop_bytes = ControlMessage::Stopped("done".to_owned())
        .encode()
        .expect("encode Stopped");
    host.send_sandbox_frame(&stop_bytes).expect("send Stopped");
    helper_thread
        .join()
        .expect("join helper thread")
        .expect("helper serve loop exited cleanly");

    // Verify clean teardown and fallback helper resolution when unset.
    let completion = host
        .terminate_sandbox_helper()
        .expect("terminate helper")
        .expect("completion");
    assert!(completion.terminated);
    assert_eq!(host.sandbox_pid(), None);

    let default_host = LiveWidgetHost::new(PeerId(1), MsgId(2), b"<html></html>", true);
    let default_path: PathBuf = default_host.resolved_helper_executable();
    assert_eq!(
        default_path.file_name().and_then(|s| s.to_str()),
        Some(WIDGET_HELPER_EXE_NAME)
    );
    // Keep unused trait import satisfied if needed in channel checks.
    let _ =
        <tg_platform::widget_sandbox::FakeWidgetChannel as WidgetHelperChannel>::max_message_bytes;
}
