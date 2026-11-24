//! Startup profile data directory layout and host wiring for Instant View disk cache,
//! video editor scene drafts, and rich-document HTML export dialogs.
//!
//! Port origin: `Telegram/SourceFiles/core/launcher.cpp` and `Telegram/SourceFiles/iv/iv_controller.cpp`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tg_app_media::webview::AppWebViewHost;
use tg_app_media::webview::rich_editor::{ExportFolderDialog, NativeExportFolderDialog};
use tg_app_window::window::controller::WindowController;

/// Profile-scoped subdirectories derived from the resolved working directory (`<working_dir>/tdata`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDataPaths {
    /// Root profile data directory (`<working_dir>/tdata`).
    pub profile_data_dir: PathBuf,
    /// Instant View disk media cache directory (`<profile_data_dir>/iv_cache`).
    pub iv_cache_dir: PathBuf,
    /// Persisted video editor scene drafts directory (`<profile_data_dir>/video_drafts`).
    pub video_drafts_dir: PathBuf,
    /// Default initial folder for `"Save as HTML"` exports (`<profile_data_dir>/exports`).
    pub rich_export_dir: PathBuf,
}

impl ProfileDataPaths {
    /// Derives [`ProfileDataPaths`] from a tdesktop working directory (`<working_dir>/tdata`).
    #[must_use]
    pub fn from_working_dir(working_dir: &Path) -> Self {
        Self::from_profile_data_dir(&working_dir.join("tdata"))
    }

    /// Derives [`ProfileDataPaths`] directly from a profile data directory (`tdata`).
    #[must_use]
    pub fn from_profile_data_dir(profile_data_dir: &Path) -> Self {
        Self {
            profile_data_dir: profile_data_dir.to_path_buf(),
            iv_cache_dir: profile_data_dir.join("iv_cache"),
            video_drafts_dir: profile_data_dir.join("video_drafts"),
            rich_export_dir: profile_data_dir.join("exports"),
        }
    }
}

/// Connects [`ProfileDataPaths`] and an export folder dialog into [`AppWebViewHost`] and
/// [`WindowController`].
///
/// Production startup passes [`NativeExportFolderDialog`] (`rfd::FileDialog`); tests inject a
/// fake [`ExportFolderDialog`] with a temp directory.
pub fn apply_profile_data_paths(
    paths: &ProfileDataPaths,
    webview_host: &mut AppWebViewHost,
    controller: &mut WindowController,
    export_dialog: Option<Arc<dyn ExportFolderDialog>>,
) {
    webview_host.set_iv_cache_dir(Some(paths.iv_cache_dir.clone()));
    controller.set_profile_media_dirs(
        Some(paths.video_drafts_dir.clone()),
        Some(paths.rich_export_dir.clone()),
        export_dialog,
    );
}

/// Convenience helper for production startup wiring [`ProfileDataPaths`] with
/// [`NativeExportFolderDialog`] (`rfd::FileDialog`).
pub fn apply_profile_data_paths_native(
    working_dir: &Path,
    webview_host: &mut AppWebViewHost,
    controller: &mut WindowController,
) -> ProfileDataPaths {
    let paths = ProfileDataPaths::from_working_dir(working_dir);
    apply_profile_data_paths(
        &paths,
        webview_host,
        controller,
        Some(Arc::new(NativeExportFolderDialog)),
    );
    paths
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::stand_in::settings::ShellSettings;
    use crate::stand_in::types::PeerId;
    use tempfile::TempDir;
    use tg_app_window::mock_session::MockSession;

    use super::*;

    struct FakeExportDialog {
        seen_initial: Mutex<Vec<Option<PathBuf>>>,
        next_result: Mutex<Option<PathBuf>>,
    }

    impl FakeExportDialog {
        fn new(result: Option<PathBuf>) -> Self {
            Self {
                seen_initial: Mutex::new(Vec::new()),
                next_result: Mutex::new(result),
            }
        }

        fn take_seen(&self) -> Vec<Option<PathBuf>> {
            std::mem::take(&mut *self.seen_initial.lock().expect("seen lock"))
        }
    }

    impl ExportFolderDialog for FakeExportDialog {
        fn pick_folder(&self, initial_dir: Option<&Path>) -> Option<PathBuf> {
            self.seen_initial
                .lock()
                .expect("seen lock")
                .push(initial_dir.map(Path::to_path_buf));
            self.next_result.lock().expect("result lock").clone()
        }
    }

    #[test]
    fn startup_profile_data_paths_wire_iv_cache_video_drafts_and_rich_export_dialog() {
        let dir = TempDir::new().expect("temp profile dir");
        let chosen_export = dir.path().join("picked_html_export");
        std::fs::create_dir_all(&chosen_export).expect("create chosen export dir");

        let paths = ProfileDataPaths::from_working_dir(dir.path());
        assert_eq!(paths.profile_data_dir, dir.path().join("tdata"));
        assert_eq!(
            paths.iv_cache_dir,
            dir.path().join("tdata").join("iv_cache")
        );
        assert_eq!(
            paths.video_drafts_dir,
            dir.path().join("tdata").join("video_drafts")
        );
        assert_eq!(
            paths.rich_export_dir,
            dir.path().join("tdata").join("exports")
        );

        let mut webview_host = AppWebViewHost::new();
        let mut controller = WindowController::with_session(
            MockSession::new(true),
            ShellSettings::default(),
            dir.path().join("shell_settings.json"),
        );
        let fake_dialog = Arc::new(FakeExportDialog::new(Some(chosen_export.clone())));

        apply_profile_data_paths(
            &paths,
            &mut webview_host,
            &mut controller,
            Some(fake_dialog.clone()),
        );

        // 1. AppWebViewHost::iv_cache_dir wired to `<working_dir>/tdata/iv_cache`.
        assert_eq!(
            webview_host.iv_cache_dir(),
            Some(paths.iv_cache_dir.as_path())
        );

        // 2. AttachFilesBoxState inherits `video_drafts_dir` and assigns `VideoEditorModalState::draft_path`.
        let hist = controller.main_widget_mut().history_mut();
        assert_eq!(
            hist.video_drafts_dir(),
            Some(paths.video_drafts_dir.as_path())
        );
        hist.open_attach_box(false);
        let attach = hist.attach_box_mut().expect("attach box open");
        assert_eq!(
            attach.video_drafts_dir.as_deref(),
            Some(paths.video_drafts_dir.as_path())
        );
        let draft_path = crate::history::video_editor::VideoEditorModalState::default_draft_path(
            &paths.video_drafts_dir,
            "demo_clip.mp4",
        );
        assert!(draft_path.starts_with(&paths.video_drafts_dir));

        let mut modal = crate::history::video_editor::VideoEditorModalState::open_for_file(
            0,
            "demo_clip.mp4",
            1280,
            720,
            10_000,
            1_000_000,
            None,
            7,
        );
        modal.draft_path = Some(draft_path.clone());
        modal.model.rotate_cw();
        let fp = modal
            .save_draft_to_path(&draft_path)
            .expect("save draft into profile video_drafts dir");
        assert!(draft_path.is_file());

        let mut reloaded = crate::history::video_editor::VideoEditorModalState::open_for_file(
            0,
            "demo_clip.mp4",
            1280,
            720,
            10_000,
            1_000_000,
            None,
            7,
        );
        let fp2 = reloaded
            .load_draft_from_path(&draft_path)
            .expect("load draft from profile video_drafts dir");
        assert_eq!(fp, fp2);
        assert_eq!(reloaded.model.geometry.angle, 90);

        // 3. RichEditorWindow receives `rich_export_dir` + fake dialog and invokes it on export.
        hist.open_rich_editor_compose(PeerId::from_user(7));
        let editor = hist.rich_editor().expect("rich editor open");
        assert_eq!(editor.export_dir(), Some(paths.rich_export_dir.as_path()));
        // Exercise the fake dialog directly with the configured initial dir.
        let picked = fake_dialog
            .pick_folder(editor.export_dir())
            .expect("fake dialog returns chosen export dir");
        assert_eq!(picked, chosen_export);
        assert_eq!(
            fake_dialog.take_seen(),
            vec![Some(paths.rich_export_dir.clone())]
        );
    }
}
