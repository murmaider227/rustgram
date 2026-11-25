//! Security review #2 (SEC2-A) regression tests for `tg-app-media::webview`:
//! - L3: the passcode lock closes the Instant View and Mini App windows, and none opens while locked
//! - L9: without an account folder a Mini App gets its own throwaway WebView2 profile
//! - L12: bot download names go through `tg_platform::file_safety` (BiDi, `"CON .txt"`)

use tempfile::TempDir;

use crate::webview::AppWebViewHost;
use crate::webview::backend::{BotInfo, WebViewLaunchInfo};
use crate::webview::mini_app::{MiniAppWindow, sanitize_bot_download_filename};
use crate::webview::mock::SAMPLE_IV_URL;

fn launch(bot_id: u64) -> WebViewLaunchInfo {
    WebViewLaunchInfo {
        bot: BotInfo {
            id: bot_id,
            username: format!("sec2a_bot_{bot_id}"),
            title: "Sec2a Test Bot".to_owned(),
            verified: false,
            in_attach_menu: false,
            in_main_menu: false,
            allow_clipboard_read: false,
            is_game: false,
        },
        url: "https://bot.example/app".to_owned(),
        query_id: 0,
        same_origin: true,
        fullscreen: false,
        local_routes: None,
    }
}

#[test]
fn l3_lock_closes_webview_windows_and_blocks_new_ones() {
    let mut host = AppWebViewHost::new();
    host.open_sample_instant_view();
    host.open_sample_mini_app();
    assert!(host.instant_view().is_some());
    assert!(host.mini_app().is_some());

    host.set_passcode_locked(true);
    assert!(
        host.instant_view().is_none(),
        "lock must close Instant View"
    );
    assert!(host.mini_app().is_none(), "lock must close the Mini App");

    host.open_sample_mini_app();
    host.open_instant_view(SAMPLE_IV_URL, None, None);
    assert!(host.mini_app().is_none(), "no Mini App opens while locked");
    assert!(
        host.instant_view().is_none(),
        "no Instant View opens while locked"
    );

    host.set_passcode_locked(false);
    host.open_sample_mini_app();
    assert!(host.mini_app().is_some(), "windows open again after unlock");
}

#[test]
fn l9_profile_without_account_folder_is_throwaway_and_not_shared() {
    let root = TempDir::new().expect("temp root");
    let mut a = MiniAppWindow::new(launch(42));
    let mut b = MiniAppWindow::new(launch(42));
    a.set_user_data_root(root.path().to_path_buf());
    b.set_user_data_root(root.path().to_path_buf());

    let shared = tg_webview::resolve_user_data_dir(root.path(), "bot_42");
    let pa = a.webview_user_data_dir();
    let pb = b.webview_user_data_dir();
    assert!(a.uses_ephemeral_profile());
    assert_ne!(
        pa, shared,
        "no shared per-bot profile without an account folder"
    );
    assert_ne!(pa, pb, "two windows never share a throwaway profile");
    assert!(pa.starts_with(root.path().join("ephemeral")), "{pa:?}");

    std::fs::create_dir_all(&pa).expect("create profile");
    std::fs::write(pa.join("Cookies"), b"x").expect("write cookie");
    a.close_immediately();
    assert!(!pa.exists(), "throwaway profile is deleted on close");

    let account = root.path().join("account_1");
    let c = MiniAppWindow::new(launch(42)).with_bot_storage_dir(account.clone());
    assert!(!c.uses_ephemeral_profile());
    assert!(c.webview_user_data_dir().starts_with(&account));
}

#[test]
fn l12_bot_download_names_use_shared_file_safety() {
    let spoof = sanitize_bot_download_filename("invoice\u{202E}fdp.exe");
    assert!(
        !spoof.contains('\u{202E}'),
        "BiDi override stripped: {spoof:?}"
    );
    assert_eq!(spoof, "invoicefdp.exe");
    assert_eq!(sanitize_bot_download_filename("CON .txt"), "_CON .txt");
    assert_eq!(sanitize_bot_download_filename("nul"), "_nul");
    assert_eq!(sanitize_bot_download_filename("../../evil.exe"), "evil.exe");
    for raw in ["a\u{2066}b.txt", "x\u{200F}.pdf", "y\u{061C}.doc"] {
        let out = sanitize_bot_download_filename(raw);
        assert!(
            !out.chars().any(tg_platform::file_safety::is_bidi_control),
            "{raw:?} -> {out:?}"
        );
    }
}
