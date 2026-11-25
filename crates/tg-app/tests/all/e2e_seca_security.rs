//! Security review #1 (`SEC-A`) integration tests and before/after visual snapshot capture.
//!
//! Covers:
//! - A1: Hidden-link scheme policy + "Open this link?" confirmation box + widget `ConfirmExternal`.
//! - A2: Safe file names / no writes outside target directory / unique non-overwriting saves.
//! - A3: Executable & extensionless launch warning confirmation box + Zone.Identifier enforcement.
//! - A4: Image pixel-area limit in `MediaOverlayWidget` (`READ_MAX_AREA`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::boxes::BoxKind;
use tg_app::boxes::security_confirm::LaunchWarningKind;
use tg_app::history::backend::{HistoryMessage, MessageExtra};
use tg_app::media::{
    MediaBackend, MediaItem, MediaItemId, MediaItemKind, MediaOverlayWidget, OverlayEvent,
};
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::window::controller::WebViewHostRequest;
use tg_app::window::section::SectionShow;
use tg_text::{EntityInText, EntityType, TextWithEntities};

use crate::common::{AppTestState, build_wgpu_harness, settle_layout, step_frames};

static WGPU_SECA_LOCK: Mutex<()> = Mutex::new(());

fn save_harness_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

#[test]
fn seca_a1_hidden_link_policy_blocks_dangerous_and_confirms_external() {
    let mut state = AppTestState::main_workspace();
    let peer = session_peers::USER_MIRA;
    state
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(peer, SectionShow::default(), MsgId::SHOW_AT_END);

    // 1. Disallowed schemes, UNC shares, and local paths are rejected in BOTH open_hidden_link
    //    and open_link, never queued as OpenExternalUrl, and show `lng_open_link_unsupported`.
    for dangerous in [
        "file:///C:/Windows/System32/calc.exe",
        r"\\host\share\x.exe",
        "ms-msdt:/id PCWDiagnostic /skip force",
        r"C:\Windows\x.dll",
        "javascript:alert(1)",
    ] {
        state.controller.open_hidden_link(dangerous);
        assert!(
            state.controller.take_webview_requests().is_empty(),
            "hidden dangerous link '{dangerous}' must never queue OpenExternalUrl"
        );
        assert!(
            state.controller.box_host().top_kind().is_none(),
            "disallowed scheme '{dangerous}' must be rejected, not offered in confirm box"
        );
        assert_eq!(
            state.controller.layer_stack().toast_text(),
            Some(tg_lang::tr::lng_open_link_unsupported().as_str())
        );

        state.controller.open_link(dangerous);
        assert!(
            state.controller.take_webview_requests().is_empty(),
            "direct dangerous link '{dangerous}' must never queue OpenExternalUrl"
        );
        assert!(
            tg_platform::system_info::open_url(dangerous).is_err(),
            "tg_platform::system_info::open_url must reject '{dangerous}'"
        );
    }

    // 2. Trusted Telegram hidden link opens without external confirmation.
    state
        .controller
        .open_hidden_link("https://telegram.org/faq");
    assert!(state.controller.box_host().top_kind().is_none());
    assert_eq!(
        state.controller.take_webview_requests(),
        vec![WebViewHostRequest::OpenExternalUrl(
            "https://telegram.org/faq".to_owned()
        )]
    );

    // 3. Widget `ConfirmExternal` forces confirmation even on allowlisted hosts.
    state
        .controller
        .open_external_link_requiring_confirmation("https://telegram.org/faq");
    assert!(
        matches!(
            state.controller.box_host().top_kind(),
            Some(BoxKind::HiddenLinkConfirm(b)) if b.url == "https://telegram.org/faq"
        ),
        "ConfirmExternal must show HiddenLinkConfirm modal"
    );
    assert!(state.controller.take_webview_requests().is_empty());

    // 4. Untrusted hidden link opens "Open this link?" confirmation box; Cancel does not open,
    //    Open queues OpenExternalUrl.
    let mut h = build_wgpu_harness(state, Vec2::new(1280.0, 800.0));
    settle_layout(&mut h);
    // Dismiss the previous forced-confirm box via Cancel.
    h.get_by_label(&tg_lang::tr::lng_cancel()).click();
    step_frames(&mut h, 6);
    assert!(h.state().controller.box_host().top_kind().is_none());
    assert!(h.state_mut().controller.take_webview_requests().is_empty());

    let target = "https://phishing.example/verify?next=account";
    h.state_mut().controller.open_hidden_link(target);
    step_frames(&mut h, 8);
    assert!(
        h.state_mut().controller.take_webview_requests().is_empty(),
        "hidden link must not open before user confirms"
    );
    assert!(matches!(
        h.state().controller.box_host().top_kind(),
        Some(BoxKind::HiddenLinkConfirm(b)) if b.url == target
    ));

    // Clicking "Open" confirms and queues OpenExternalUrl.
    h.get_by_label(&tg_lang::tr::lng_open_link()).click();
    step_frames(&mut h, 6);
    assert!(h.state().controller.box_host().top_kind().is_none());
    assert_eq!(
        h.state_mut().controller.take_webview_requests(),
        vec![WebViewHostRequest::OpenExternalUrl(target.to_owned())]
    );

    // 5. Suspicious mixed-script hidden link detects homograph characters, and Copy Link sets
    //    `copied` without dismissing or opening the URL.
    let spoof = "https://p\u{0430}ypal.com/login";
    h.state_mut().controller.open_hidden_link(spoof);
    step_frames(&mut h, 6);
    assert!(
        matches!(
            h.state().controller.box_host().top_kind(),
            Some(BoxKind::HiddenLinkConfirm(b)) if !b.suspicious_ranges.is_empty()
        ),
        "mixed-script Cyrillic homograph must flag suspicious ranges"
    );
    h.get_by_label(&tg_lang::tr::lng_context_copy_link())
        .click();
    step_frames(&mut h, 6);
    assert!(
        matches!(
            h.state().controller.box_host().top_kind(),
            Some(BoxKind::HiddenLinkConfirm(b)) if b.copied
        ),
        "Copy Link must keep the modal open and mark `copied = true`"
    );
    assert!(
        h.state_mut().controller.take_webview_requests().is_empty(),
        "Copy Link must never open the URL"
    );
    h.get_by_label(&tg_lang::tr::lng_cancel()).click();
    step_frames(&mut h, 6);
    assert!(h.state().controller.box_host().top_kind().is_none());
}

#[test]
fn seca_a2_remote_filenames_sanitized_contained_and_unique() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let chosen = tmp.path().join("chosen_downloads");
    std::fs::create_dir_all(&chosen).expect("mkdir");
    let canon_chosen = std::fs::canonicalize(&chosen).expect("canon");

    // Pre-seed `x.bat` to verify non-overwrite unique naming (`x (1).bat`).
    std::fs::write(chosen.join("x.bat"), b"KEEP_ME").expect("seed existing");

    let p1 = tg_platform::file_safety::write_unique_file_in_dir(
        &chosen,
        r"..\..\Startup\x.bat",
        b"REMOTE_BAT",
    )
    .expect("save traversal bat safely inside chosen folder");
    assert_eq!(p1.parent(), Some(canon_chosen.as_path()));
    assert_eq!(p1.file_name().and_then(|n| n.to_str()), Some("x (1).bat"));
    assert_eq!(std::fs::read(chosen.join("x.bat")).unwrap(), b"KEEP_ME");
    assert_eq!(std::fs::read(&p1).unwrap(), b"REMOTE_BAT");
    assert!(!tmp.path().join("Startup").exists());

    let p2 = tg_platform::file_safety::write_unique_file_in_dir(
        &chosen,
        r"C:\Windows\x.dll",
        b"REMOTE_DLL",
    )
    .expect("save absolute drive-letter dll safely inside chosen folder");
    assert_eq!(p2.parent(), Some(canon_chosen.as_path()));
    assert_eq!(p2.file_name().and_then(|n| n.to_str()), Some("x.dll"));

    // `.lnk` with trailing dot/space is still masked with `.download`.
    let p3 = tg_platform::file_safety::write_unique_file_in_dir(&chosen, "shortcut.lnk. ", b"LNK")
        .expect("save masked lnk");
    assert_eq!(
        p3.file_name().and_then(|n| n.to_str()),
        Some("shortcut.lnk.download")
    );
}

#[test]
fn seca_a3_launch_exe_and_extensionless_ask_before_launching() {
    let mut state = AppTestState::main_workspace();
    let peer = session_peers::USER_MIRA;
    state
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(peer, SectionShow::default(), MsgId::SHOW_AT_END);

    let mut h = build_wgpu_harness(state, Vec2::new(1280.0, 800.0));
    settle_layout(&mut h);

    // 1. Executable `x.exe` prompts with `LaunchFileConfirm` (Executable) and does not launch yet.
    let exe_id = MediaItemId(770_001);
    h.state_mut()
        .controller
        .request_open_external_item(exe_id, "x.exe");
    step_frames(&mut h, 8);
    assert!(matches!(
        h.state().controller.box_host().top_kind(),
        Some(BoxKind::LaunchFileConfirm(b))
            if b.item_id == exe_id
                && b.file_name == "x.exe"
                && b.kind == LaunchWarningKind::Executable { extension: "exe".to_owned() }
                && b.can_remember()
    ));

    // Clicking Cancel dismisses without launching.
    h.get_by_label(&tg_lang::tr::lng_cancel()).click();
    step_frames(&mut h, 6);
    assert!(h.state().controller.box_host().top_kind().is_none());

    // 2. Extensionless file `README` prompts with `LaunchWarningKind::Extensionless` and
    //    cannot be remembered (`can_remember() == false`).
    let noext_id = MediaItemId(770_002);
    h.state_mut()
        .controller
        .request_open_external_item(noext_id, "README");
    step_frames(&mut h, 8);
    assert!(matches!(
        h.state().controller.box_host().top_kind(),
        Some(BoxKind::LaunchFileConfirm(b))
            if b.item_id == noext_id
                && b.kind == LaunchWarningKind::Extensionless
                && !b.can_remember()
    ));
    h.get_by_label(&tg_lang::tr::lng_cancel()).click();
    step_frames(&mut h, 6);

    // 3. Confirming `x.exe` via "Run" button triggers launch and closes the modal.
    h.state_mut()
        .controller
        .request_open_external_item(exe_id, "x.exe");
    step_frames(&mut h, 8);
    h.get_by_label(&tg_lang::tr::lng_launch_exe_sure()).click();
    step_frames(&mut h, 6);
    assert!(h.state().controller.box_host().top_kind().is_none());
}

#[test]
fn seca_a4_overlay_rejects_oversized_image_area() {
    let backend = tg_app::media::MockMediaBackend::new();
    let bomb_id = MediaItemId(770_004);

    // Synthesize a 67-byte PNG header claiming 20_000 x 20_000 pixels (400M px > READ_MAX_AREA).
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
    fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out.extend_from_slice(&crc32(&out[start..]).to_be_bytes());
    }
    let mut bomb_png = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&20_000u32.to_be_bytes());
    ihdr.extend_from_slice(&20_000u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    push_chunk(&mut bomb_png, b"IHDR", &ihdr);
    push_chunk(&mut bomb_png, b"IDAT", &[0x00]);
    push_chunk(&mut bomb_png, b"IEND", &[]);

    backend.insert_item(
        MediaItem {
            id: bomb_id,
            peer_id: 1,
            msg_id: 770_004,
            kind: MediaItemKind::Photo,
            width: 20_000,
            height: 20_000,
            duration_ms: 0,
            file_size: bomb_png.len() as u64,
            file_name: "bomb.png".to_owned(),
            mime_type: "image/png".to_owned(),
            sender_name: "Test".to_owned(),
            date_text: "Today".to_owned(),
            caption: TextWithEntities::default(),
            rotation_degrees: 0,
            has_alpha: false,
            allows_forward: true,
            allows_delete: true,
            allows_save: true,
            local_path: None,
            grouped_id: None,
            attached_stickers_media: None,
        },
        bomb_png,
    );

    let mut overlay = MediaOverlayWidget::new();
    overlay.open_item(&backend, 1, bomb_id, 1_000).unwrap();
    let err = overlay
        .error_message()
        .expect("oversized photo must fail with area limit error");
    assert!(
        err.contains("area limit") || err.contains("exceed"),
        "expected area limit error, got: {err}"
    );
    // Also verify OverlayEvent::ConfirmOpenExternal is wired cleanly.
    let mut events = Vec::new();
    let exe_id = MediaItemId(770_005);
    backend.insert_item(
        MediaItem {
            id: exe_id,
            peer_id: 1,
            msg_id: 770_005,
            kind: MediaItemKind::Document,
            width: 0,
            height: 0,
            duration_ms: 0,
            file_size: 2,
            file_name: "installer.exe".to_owned(),
            mime_type: "application/x-msdownload".to_owned(),
            sender_name: "Test".to_owned(),
            date_text: "Today".to_owned(),
            caption: TextWithEntities::default(),
            rotation_degrees: 0,
            has_alpha: false,
            allows_forward: true,
            allows_delete: true,
            allows_save: true,
            local_path: Some(PathBuf::from("installer.exe")),
            grouped_id: None,
            attached_stickers_media: None,
        },
        b"MZ".to_vec(),
    );
    overlay.open_item(&backend, 1, exe_id, 1_100).unwrap();
    overlay.request_open_external_current(&backend, 1_100, &mut events);
    assert!(events.iter().any(|e| matches!(
        e,
        OverlayEvent::ConfirmOpenExternal { item_id, file_name }
            if *item_id == exe_id && file_name == "installer.exe"
    )));
    // And verify backend save_to_disk rejects `..` traversal.
    assert!(
        backend
            .save_to_disk(exe_id, Path::new(r"..\..\Startup\evil.exe"))
            .is_err()
    );
}

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_seca_snapshots() {
    let _guard = WGPU_SECA_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_SECA_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("seca"));

    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        // Screen 1 (A1): Hidden link confirmation modal ("Open this link?") over chat workspace.
        {
            let mut state = AppTestState::main_workspace();
            let handle = state.controller.session_handle();
            let peer = session_peers::USER_MIRA;
            let text_str = "Please review the release notes here.";
            let start = text_str.find("release notes").expect("phrase") as i32;
            let len = "release notes".len() as i32;
            let text = TextWithEntities::with_entities(
                text_str.to_owned(),
                vec![EntityInText::with_data(
                    EntityType::CustomUrl,
                    start,
                    len,
                    "https://phishing.example/verify?next=account".to_owned(),
                )],
            );
            handle.push_history_message(
                peer,
                HistoryMessage {
                    id: MsgId(9801),
                    peer_id: peer,
                    from_id: peer,
                    sender_name: "Mira Vance".to_owned(),
                    date_unix: 1_795_362_100,
                    outgoing: false,
                    is_post: false,
                    text,
                    media: None,
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: Vec::new(),
                    views: None,
                    edited: false,
                    is_read: true,
                    is_sending: false,
                    is_pinned: false,
                    extra: MessageExtra::default(),
                },
            );
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(peer, SectionShow::default(), MsgId::SHOW_AT_END);

            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .open_hidden_link("https://phishing.example/verify?next=account");
            step_frames(&mut h, 14);
            save_harness_png(
                &mut h,
                &dir.join(format!("hidden_link_confirm_{size_label}_{stage}.png")),
            );
        }

        // Screen 2 (A3): Executable launch warning modal ("installer.exe" / `lng_launch_exe_warning`).
        {
            let mut state = AppTestState::main_workspace();
            let peer = session_peers::USER_MIRA;
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(peer, SectionShow::default(), MsgId::SHOW_AT_END);

            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            h.state_mut()
                .controller
                .request_open_external_item(MediaItemId(980_002), "installer.exe");
            step_frames(&mut h, 14);
            save_harness_png(
                &mut h,
                &dir.join(format!("launch_exe_confirm_{size_label}_{stage}.png")),
            );
        }
    }
}
