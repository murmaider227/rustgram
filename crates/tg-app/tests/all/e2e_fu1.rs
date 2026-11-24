//! Integration tests and before/after visual snapshot capture for Follow-Up 1 (`own_fu1`).
//!
//! Covers:
//! 1. Startup wiring of profile data paths (`AppWebViewHost::set_iv_cache_dir`,
//!    `VideoEditorModalState::draft_path`, `RichEditorWindow::set_export_dir` + native/fake dialog).
//! 2. `MusicAttachBox` sending real audio via `messages.sendMedia` / `messages.sendMultiMedia`
//!    (`InputMediaDocument` from `SavedMusicTrack`).
//! 3. AI summary header sparkle particles kept clear of the text area.
//! 4. Deep link `tg://settings/experimental/<id>` opening `SettingsSection::Experimental` with
//!    that option highlighted.
//! 5. `settings_row` truncating long titles with an ellipsis and keeping a gap before right text.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tempfile::TempDir;
use tg_app::history::backend::{HistoryBackend, HistoryMessage, MessageExtra};
use tg_app::settings::SettingsSection;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::types::MsgId;
use tg_app::startup::{ProfileDataPaths, apply_profile_data_paths};
use tg_app::webview::AppWebViewHost;
use tg_app::webview::rich_editor::{ExportFolderDialog, RichEditorTab};
use tg_app::window::section::{SectionShow, SettingsPage};
use tg_text::TextWithEntities;

use crate::common::{AppTestState, build_wgpu_harness, settle_layout, step_frames};

static WGPU_FU1_LOCK: Mutex<()> = Mutex::new(());

fn save_harness_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create snap dir");
    }
    let img = harness.render().expect("wgpu render");
    img.save(path).expect("save png");
}

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_fu1_snapshots() {
    let _guard = WGPU_FU1_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = std::env::var("RUSTGRAM_FU1_SNAP_STAGE")
        .or_else(|_| std::env::var("TGFORK_FU1_SNAP_STAGE"))
        .unwrap_or_else(|_| "after".to_owned());
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("fu1"));

    for (size_label, size) in [
        ("1440x900", Vec2::new(1440.0, 900.0)),
        ("390x844", Vec2::new(390.0, 844.0)),
    ] {
        // Screen 1 (Item 3): History workspace with expanded AI summary header and chat effects ON
        // so sparkle particles are rendered.
        {
            let mut state = AppTestState::main_workspace();
            let mut handle = state.controller.session_handle();
            let peer = session_peers::USER_MIRA;
            let summary_id = MsgId(9601);
            handle.push_history_message(
                peer,
                HistoryMessage {
                    id: summary_id,
                    peer_id: peer,
                    from_id: peer,
                    sender_name: "Mira Vance".to_owned(),
                    date_unix: 1_795_362_000,
                    outgoing: false,
                    is_post: false,
                    text: TextWithEntities::simple(
                        "Long engineering status note covering protocol migration milestones, \
                         headless rendering regression checks, and release checklist items for \
                         the upcoming desktop build.",
                    ),
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
                    extra: MessageExtra {
                        summary_from_language: Some("en".to_owned()),
                        ..MessageExtra::default()
                    },
                },
            );
            state
                .controller
                .session_mut()
                .expect("session")
                .show_peer_history(peer, SectionShow::default(), MsgId::SHOW_AT_END);

            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            let _ = handle.toggle_message_summary(peer, summary_id, Some("en"));
            {
                let hist = h.state_mut().controller.main_widget_mut().history_mut();
                hist.inner_mut().invalidate_message_layout(summary_id);
                hist.inner_mut().request_scroll_to_bottom();
            }
            step_frames(&mut h, 10);
            save_harness_png(
                &mut h,
                &dir.join(format!("summary_header_{size_label}_{stage}.png")),
            );
        }

        // Screen 2 (Item 5): Settings > Passkeys with long passkey titles next to right-aligned "Delete".
        {
            let state = AppTestState::main_workspace();
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_settings(SettingsPage::PrivacySecurity);
            }
            settle_layout(&mut h);
            if let Some(s) = h.state_mut().controller.main_widget_mut().settings_mut() {
                s.push_section(SettingsSection::Passkeys);
            }
            step_frames(&mut h, 6);
            save_harness_png(
                &mut h,
                &dir.join(format!("settings_row_passkeys_{size_label}_{stage}.png")),
            );
        }
    }
}

struct FakeExportFolderDialog {
    initial_dirs: Mutex<Vec<Option<PathBuf>>>,
    response: Mutex<Option<PathBuf>>,
}

impl FakeExportFolderDialog {
    fn new(response: Option<PathBuf>) -> Self {
        Self {
            initial_dirs: Mutex::new(Vec::new()),
            response: Mutex::new(response),
        }
    }

    fn set_response(&self, response: Option<PathBuf>) {
        *self.response.lock().expect("response lock") = response;
    }

    fn take_initial_dirs(&self) -> Vec<Option<PathBuf>> {
        std::mem::take(&mut *self.initial_dirs.lock().expect("initial lock"))
    }
}

impl ExportFolderDialog for FakeExportFolderDialog {
    fn pick_folder(&self, initial_dir: Option<&Path>) -> Option<PathBuf> {
        self.initial_dirs
            .lock()
            .expect("initial lock")
            .push(initial_dir.map(Path::to_path_buf));
        self.response.lock().expect("response lock").clone()
    }
}

#[test]
fn fu1_item1_profile_data_paths_and_fake_export_dialog_e2e() {
    let dir = TempDir::new().expect("temp dir");
    let chosen_dir = dir.path().join("user_picked_export");
    std::fs::create_dir_all(&chosen_dir).expect("create chosen export dir");

    let paths = ProfileDataPaths::from_working_dir(dir.path());
    let mut webview_host = AppWebViewHost::new();
    let mut state = AppTestState::main_workspace();
    let fake_dialog = Arc::new(FakeExportFolderDialog::new(None));

    apply_profile_data_paths(
        &paths,
        &mut webview_host,
        &mut state.controller,
        Some(fake_dialog.clone()),
    );
    assert_eq!(
        webview_host.iv_cache_dir(),
        Some(paths.iv_cache_dir.as_path())
    );

    // 1. Open AttachFilesBox with a video candidate and open VideoEditorModalState via UI;
    //    verify `draft_path` points into `<working_dir>/tdata/video_drafts/` and Save Draft writes it.
    state
        .controller
        .session_mut()
        .expect("session")
        .show_peer_history(session_peers::USER_MIRA, SectionShow::default(), MsgId(0));
    {
        let hist = state.controller.main_widget_mut().history_mut();
        hist.open_attach_box(false);
        if let Some(attach) = hist.attach_box_mut() {
            attach.candidates.clear();
            attach.add_sample_video("profile_clip.mp4", 1280, 720, 2_000_000);
        }
    }
    let mut h = build_wgpu_harness(state, Vec2::new(1280.0, 820.0));
    settle_layout(&mut h);
    step_frames(&mut h, 3);
    h.get_all_by_label("Edit")
        .last()
        .expect("video Edit button")
        .click();
    step_frames(&mut h, 4);

    let draft_path = {
        let hist = h.state().controller.main_widget().history();
        let ve = hist
            .attach_box()
            .and_then(|a| a.active_video_editor.as_ref())
            .expect("video editor modal open");
        let p = ve.draft_path.clone().expect("draft_path populated");
        assert!(
            p.starts_with(&paths.video_drafts_dir),
            "draft_path must be under profile video_drafts dir: {}",
            p.display()
        );
        p
    };
    h.get_by_label("Save Draft").click();
    step_frames(&mut h, 3);
    assert!(
        draft_path.is_file(),
        "clicking Save Draft must persist scene draft into profile video_drafts dir"
    );

    // Close video editor & attach box, then open RichEditorWindow and test fake export dialog.
    h.get_all_by_label("Cancel")
        .last()
        .expect("video editor Cancel button")
        .click();
    step_frames(&mut h, 2);
    {
        let hist = h.state_mut().controller.main_widget_mut().history_mut();
        hist.close_attach_box();
        hist.open_rich_editor_compose(session_peers::USER_MIRA);
        let ed = hist.rich_editor_mut().expect("rich editor open");
        assert_eq!(ed.export_dir(), Some(paths.rich_export_dir.as_path()));
        ed.set_tab(RichEditorTab::Markdown);
    }
    step_frames(&mut h, 3);

    // Cancelled dialog (`None`) must not write any export.
    h.get_by_label(&tg_lang::tr::lng_context_save_html())
        .click();
    step_frames(&mut h, 2);
    assert_eq!(
        fake_dialog.take_initial_dirs(),
        vec![Some(paths.rich_export_dir.clone())]
    );
    assert!(
        h.state()
            .controller
            .main_widget()
            .history()
            .rich_editor()
            .expect("rich editor")
            .last_html_export()
            .is_none(),
        "cancelling export dialog must not write HTML"
    );

    // Picking `chosen_dir` exports HTML into that folder.
    fake_dialog.set_response(Some(chosen_dir.clone()));
    h.get_by_label(&tg_lang::tr::lng_context_save_html())
        .click();
    step_frames(&mut h, 2);
    assert_eq!(
        fake_dialog.take_initial_dirs(),
        vec![Some(paths.rich_export_dir.clone())]
    );
    let export_res = h
        .state()
        .controller
        .main_widget()
        .history()
        .rich_editor()
        .expect("rich editor")
        .last_html_export()
        .expect("HTML exported to fake-dialog directory");
    assert!(export_res.html_path.starts_with(&chosen_dir));
    assert!(export_res.html_path.is_file());
}

#[test]
fn fu1_item2_music_attach_sends_audio_document_media_not_text_e2e() {
    let state = AppTestState::main_workspace();
    let mut h = build_wgpu_harness(state, Vec2::new(1280.0, 820.0));
    settle_layout(&mut h);

    let before_count = h
        .state()
        .controller
        .session_backend()
        .load_slice_around(session_peers::GROUP_CORE, MsgId::SHOW_AT_END, 50, 10)
        .messages
        .len();

    let ctx = h.ctx.clone();
    let colors = tg_app::settings::theme_bridge::LiveThemeColors::default();
    let self_id = h
        .state()
        .controller
        .session_backend()
        .saved_messages_peer_id();
    let music_box = tg_app::boxes::bot_info_extras::MusicAttachBoxState::new(
        session_peers::GROUP_CORE,
        self_id,
    );
    h.state_mut().controller.box_host_mut().push(
        &ctx,
        tg_app::boxes::BoxKind::MusicAttach(music_box),
        &colors,
    );
    step_frames(&mut h, 4);

    if let Some(tg_app::boxes::BoxKind::MusicAttach(ms)) =
        h.state_mut().controller.box_host_mut().top_kind_mut()
    {
        assert!(!ms.tracks.is_empty(), "MusicAttachBox must load tracks");
        let first_id = ms.tracks[0].document_id;
        ms.toggle_select(first_id);
    }
    step_frames(&mut h, 3);

    h.get_by_label("Send (1)").click();
    step_frames(&mut h, 4);

    let slice = h.state().controller.session_backend().load_slice_around(
        session_peers::GROUP_CORE,
        MsgId::SHOW_AT_END,
        50,
        10,
    );
    assert_eq!(slice.messages.len(), before_count + 1);
    let last = slice.messages.last().expect("sent audio message");
    assert!(
        last.text.text.is_empty(),
        "MusicAttachBox must send real audio media, not a formatted text message; got: {:?}",
        last.text.text
    );
    match last.media.as_ref() {
        Some(tg_app::history::backend::HistoryMedia::Document(doc)) => {
            assert_eq!(doc.mime_type, "audio/mpeg");
            assert!(doc.file_name.ends_with(".mp3"));
        }
        other => panic!("expected HistoryMedia::Document(audio/mpeg), got {other:?}"),
    }
}

#[test]
fn fu1_item4_deep_link_settings_experimental_highlights_option_e2e() {
    let state = AppTestState::main_workspace();
    let mut h = build_wgpu_harness(state, Vec2::new(1280.0, 820.0));
    settle_layout(&mut h);

    // 1. `tg://settings/experimental/<id>` opens SettingsSection::Experimental with `<id>` highlighted.
    h.state_mut()
        .controller
        .open_link("tg://settings/experimental/unlimited-message-width");
    step_frames(&mut h, 4);
    {
        let settings = h
            .state()
            .controller
            .main_widget()
            .settings()
            .expect("settings open");
        assert_eq!(settings.current_section(), SettingsSection::Experimental);
        assert_eq!(
            settings.experimental.highlighted_id.as_deref(),
            Some("unlimited-message-width")
        );
    }

    // 2. Short alias `tg://settings/exp/<id>` also works (case-insensitive).
    h.state_mut()
        .controller
        .open_link("tg://settings/exp/Show-Peer-Id-Below-About");
    step_frames(&mut h, 4);
    {
        let settings = h
            .state()
            .controller
            .main_widget()
            .settings()
            .expect("settings open");
        assert_eq!(settings.current_section(), SettingsSection::Experimental);
        assert_eq!(
            settings.experimental.highlighted_id.as_deref(),
            Some("show-peer-id-below-about")
        );
    }

    // 3. Bare `tg://settings/experimental` opens Experimental with no highlight.
    h.state_mut()
        .controller
        .open_link("tg://settings/experimental");
    step_frames(&mut h, 4);
    {
        let settings = h
            .state()
            .controller
            .main_widget()
            .settings()
            .expect("settings open");
        assert_eq!(settings.current_section(), SettingsSection::Experimental);
        assert_eq!(settings.experimental.highlighted_id, None);
    }
}
