//! Files that earlier releases installed next to `rustgram.exe` and the current one no longer ships.
//!
//! The updater only copies files (tdesktop's `Updater.exe` too, `_other/updater_win.cpp`), so a file
//! that a new release stops shipping stays in the install folder after an update. tdesktop removes
//! such files by name (`DeleteMyModules` in `platform/win/specific_win.cpp`: the old
//! `modules\<arch>\d3d\d3dcompiler_47.dll` and `TelegramWidgetHelper.exe`); Rustgram does the same at
//! startup and in the installer, with the names below and nothing else.

use std::fs;
use std::path::{Path, PathBuf};

use crate::DEFAULT_EXE_NAME;

/// Runtime files an earlier release installed next to `rustgram.exe` that the current release
/// does not ship. Only these exact names are ever deleted.
///
/// When a release stops shipping a runtime file (an FFmpeg major version bump renames every DLL),
/// add the old name here. `tg-xtask dist` refuses to package a file listed here.
pub const OBSOLETE_INSTALL_FILES: &[&str] = &[
    // FFmpeg 8.0 runtime, shipped by v0.1.0-alpha.2 to v0.2.0-alpha.2 (TGB-3146 moved to N-129333).
    "avcodec-62.dll",
    "avformat-62.dll",
    "avutil-60.dll",
    "swresample-6.dll",
    "swscale-9.dll",
];

/// Deletes the [`OBSOLETE_INSTALL_FILES`] found in `install_dir` and returns the deleted paths.
///
/// Does nothing unless `install_dir` holds `rustgram.exe`, so it never touches a folder that is not
/// a Rustgram install. A file that cannot be deleted (in use, read-only folder) is left for the
/// next start. Only regular files are deleted, never directories or links.
pub fn remove_obsolete_install_files(install_dir: &Path) -> Vec<PathBuf> {
    if !install_dir.join(DEFAULT_EXE_NAME).is_file() {
        return Vec::new();
    }
    let mut removed = Vec::new();
    for name in OBSOLETE_INSTALL_FILES {
        let path = install_dir.join(name);
        let is_file = fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_file());
        if is_file && fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_only_listed_files_in_a_rustgram_folder() {
        let dir = tempfile::TempDir::new().expect("temp");
        let root = dir.path();
        fs::write(root.join(DEFAULT_EXE_NAME), b"exe").expect("exe");
        fs::write(root.join("avutil-60.dll"), b"old").expect("old dll");
        fs::write(root.join("avutil-61.dll"), b"new").expect("new dll");
        fs::write(root.join("notes.txt"), b"user file").expect("user file");
        fs::create_dir(root.join("swscale-9.dll")).expect("dir with a listed name");

        let removed = remove_obsolete_install_files(root);

        assert_eq!(removed, vec![root.join("avutil-60.dll")]);
        assert!(!root.join("avutil-60.dll").exists());
        assert!(root.join("avutil-61.dll").is_file());
        assert!(root.join("notes.txt").is_file());
        assert!(root.join("swscale-9.dll").is_dir());
        assert!(remove_obsolete_install_files(root).is_empty());
    }

    #[test]
    fn leaves_a_folder_without_rustgram_exe_alone() {
        let dir = tempfile::TempDir::new().expect("temp");
        fs::write(dir.path().join("avcodec-62.dll"), b"someone else's").expect("dll");
        assert!(remove_obsolete_install_files(dir.path()).is_empty());
        assert!(dir.path().join("avcodec-62.dll").is_file());
    }

    #[test]
    fn names_are_plain_file_names() {
        for name in OBSOLETE_INSTALL_FILES {
            assert!(
                crate::package::is_safe_archive_path(name) && !name.contains('/'),
                "{name}"
            );
            assert!(!name.eq_ignore_ascii_case(DEFAULT_EXE_NAME));
            assert!(!name.eq_ignore_ascii_case(crate::UPDATER_EXE_NAME));
        }
    }
}
