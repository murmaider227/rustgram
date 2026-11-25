//! Security review #2 (SEC2-A, item L6): `web_app_open_tg_link` must only ever produce a
//! `https://t.me/...` link. The old code built `https://t.me{path_full}` from any string, so
//! `"@evil.example/"` produced `https://t.me@evil.example/` (host `evil.example`).

use std::time::Instant;

use tg_theme::Palette;
use tg_webview::{
    MiniAppAction, MiniAppController, MiniAppPanelConfig, WebMessage, telegram_link_from_path,
};

fn controller() -> MiniAppController {
    MiniAppController::new(MiniAppPanelConfig {
        initial_url: "https://bot.example/app".to_owned(),
        same_origin: true,
        allow_clipboard_read: false,
        fullscreen: false,
    })
}

fn open_tg_link(ctrl: &mut MiniAppController, path_full: &str) -> Vec<MiniAppAction> {
    let msg = WebMessage {
        text: serde_json::json!(["web_app_open_tg_link", { "path_full": path_full }]).to_string(),
        source_url: "https://bot.example/app".to_owned(),
    };
    ctrl.handle_web_message(&msg, &Palette::default(), Instant::now())
        .unwrap_or_default()
}

#[test]
fn l6_open_tg_link_cannot_escape_t_me() {
    let mut ctrl = controller();
    for evil in [
        "@evil.example/",
        "@evil.example/path",
        ".evil.example/",
        ":443@evil.example/",
        "//evil.example/",
        "/\\evil.example",
        "/ x",
        "evil",
    ] {
        let actions = open_tg_link(&mut ctrl, evil);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, MiniAppAction::OpenTgLink { .. })),
            "{evil:?} must not produce a link, got {actions:?}"
        );
        assert_eq!(telegram_link_from_path(evil), None, "{evil:?}");
    }

    let actions = open_tg_link(&mut ctrl, "/rustgram_news?start=1");
    assert!(
        actions.iter().any(|a| matches!(
            a,
            MiniAppAction::OpenTgLink { url, .. } if url == "https://t.me/rustgram_news?start=1"
        )),
        "a normal path still opens, got {actions:?}"
    );
}
