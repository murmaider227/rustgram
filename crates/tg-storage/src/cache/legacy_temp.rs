//! Startup sweep of temporary folders that older Rustgram builds left in the system temp
//! directory (`%TEMP%`).
//!
//! Older builds wrote plaintext media there: streamed video (`rustgram_stream_*`), the
//! pre-account media cache (`rustgram_media_cache_*`), files opened in external apps
//! (`rustgram_external*`), round-video captures (`rustgram_round_videos*`), video-editor exports
//! (`rustgram_video_edit_*`) and notification userpics (`rustgram-notify-userpics-*`). Current
//! builds keep all of these inside the account folder under `tdata`, so anything matching these
//! names is a leftover. The only folder current builds still create in the temp directory is
//! `rustgram_tmpstream_<pid>_<random>` (streamed media before an account folder exists, encrypted
//! with a key that never leaves RAM); leftovers of crashed processes are swept here as well.

use std::fs;
use std::path::Path;
use std::sync::Once;
use std::time::{Duration, SystemTime};

/// Folder-name prefix for streamed media written before an account folder exists. The content is
/// encrypted with a per-loader random key that is never written to disk.
pub const EPHEMERAL_STREAM_DIR_PREFIX: &str = "rustgram_tmpstream_";

/// Entries modified more recently than this are left alone, so a sweep never removes a folder
/// that another running process (an older build, or a test of one) is still writing.
pub const LEGACY_TEMP_MIN_AGE: Duration = Duration::from_secs(15 * 60);

/// Name prefixes of temp-directory entries created by older builds (or by crashed processes of
/// this build, for [`EPHEMERAL_STREAM_DIR_PREFIX`]).
const LEGACY_TEMP_PREFIXES: &[&str] = &[
    "rustgram_stream_",
    "rustgram_media_cache_",
    "rustgram_external",
    "rustgram_round_videos",
    "rustgram_video_edit_",
    "rustgram-notify-userpics-",
    EPHEMERAL_STREAM_DIR_PREFIX,
];

/// Builds the temp-directory folder name for an ephemeral encrypted stream of process `pid`.
#[must_use]
pub fn ephemeral_stream_dir_name(pid: u32, nonce: u64) -> String {
    format!("{EPHEMERAL_STREAM_DIR_PREFIX}{pid}_{nonce:016x}")
}

/// Returns `true` when `name` is a temp-directory entry this module is allowed to delete.
#[must_use]
pub fn is_legacy_temp_name(name: &str) -> bool {
    LEGACY_TEMP_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn belongs_to_pid(name: &str, pid: u32) -> bool {
    let pid = pid.to_string();
    [
        format!("{EPHEMERAL_STREAM_DIR_PREFIX}{pid}_"),
        format!("_pid_{pid}"),
        format!("rustgram-notify-userpics-{pid}"),
    ]
    .iter()
    .any(|marker| {
        name.find(marker.as_str()).is_some_and(|at| {
            // The pid must end the name or be followed by a separator, so pid 12 does not
            // match a folder of pid 123.
            let rest = &name[at + marker.len()..];
            marker.ends_with('_') || rest.is_empty() || rest.starts_with(['_', '-'])
        })
    })
}

/// Deletes legacy Rustgram temp entries directly inside `temp_root`.
///
/// Skips entries that belong to process `own_pid`, entries modified within `min_age` of `now`
/// and every name that is not on the legacy list. Returns the number of entries removed.
pub fn sweep_legacy_temp_dirs_in(
    temp_root: &Path,
    own_pid: u32,
    now: SystemTime,
    min_age: Duration,
) -> usize {
    let Ok(entries) = fs::read_dir(temp_root) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !is_legacy_temp_name(name) || belongs_to_pid(name, own_pid) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let recently_modified = meta
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_none_or(|age| age < min_age);
        if recently_modified {
            continue;
        }
        let path = entry.path();
        let result = if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if result.is_ok() || !path.exists() {
            removed += 1;
        }
    }
    removed
}

/// Sweeps the system temp directory once per process, on a background thread so startup does not
/// wait for the directory listing.
pub fn sweep_legacy_temp_dirs_once() {
    static SWEEP: Once = Once::new();
    SWEEP.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("legacy-temp-sweep".to_owned())
            .spawn(|| {
                sweep_legacy_temp_dirs_in(
                    &std::env::temp_dir(),
                    std::process::id(),
                    SystemTime::now(),
                    LEGACY_TEMP_MIN_AGE,
                );
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sec3_b1_sweep_removes_legacy_temp_folders_and_keeps_everything_else() {
        let root = TempDir::new().expect("tempdir");
        let own_pid = 4242u32;
        let legacy = [
            "rustgram_stream_00000000000dbf89_0000000000000002_1f2e3d4c5b6a7988",
            "rustgram_media_cache_16045690984503098046",
            "rustgram_external",
            "rustgram_external_pid_777",
            "rustgram_round_videos_pid_777",
            "rustgram_video_edit_pid_777",
            "rustgram-notify-userpics-777",
            "rustgram_tmpstream_777_00000000000000aa",
        ];
        let kept = [
            // Not on the legacy list: test and tool folders, unrelated apps.
            "rustgram-snap",
            "rustgram_media_unit_test",
            "rustgram_downloads",
            "RustgramDownloads",
            "other_app_cache",
            // Owned by the sweeping process itself.
            "rustgram_tmpstream_4242_00000000000000bb",
            "rustgram_external_pid_4242",
            "rustgram-notify-userpics-4242",
        ];
        for name in legacy.iter().chain(kept.iter()) {
            let dir = root.path().join(name);
            fs::create_dir_all(&dir).expect("mkdir");
            fs::write(dir.join("stream.bin"), b"PLAINTEXT_VIDEO_BYTES").expect("write");
        }
        let legacy_file = root.path().join("rustgram_video_edit_0_0_1.mp4");
        fs::write(&legacy_file, b"PLAINTEXT_EXPORT").expect("write file");

        // Entries modified a moment ago may belong to a process that is still running.
        let now = SystemTime::now();
        assert_eq!(
            sweep_legacy_temp_dirs_in(root.path(), own_pid, now, LEGACY_TEMP_MIN_AGE),
            0,
            "fresh entries must not be swept"
        );
        for name in &legacy {
            assert!(root.path().join(name).exists(), "{name} swept too early");
        }

        let later = now + LEGACY_TEMP_MIN_AGE + Duration::from_secs(60);
        let removed = sweep_legacy_temp_dirs_in(root.path(), own_pid, later, LEGACY_TEMP_MIN_AGE);
        assert_eq!(removed, legacy.len() + 1);
        for name in &legacy {
            assert!(
                !root.path().join(name).exists(),
                "legacy temp folder {name} must be removed"
            );
        }
        assert!(!legacy_file.exists(), "legacy temp file must be removed");
        for name in &kept {
            assert!(root.path().join(name).exists(), "{name} must be kept");
        }
    }

    #[test]
    fn sec3_b1_pid_match_does_not_confuse_prefixes_of_other_pids() {
        assert!(belongs_to_pid("rustgram_tmpstream_12_00ff", 12));
        assert!(!belongs_to_pid("rustgram_tmpstream_123_00ff", 12));
        assert!(belongs_to_pid("rustgram_external_pid_12", 12));
        assert!(!belongs_to_pid("rustgram_external_pid_123", 12));
        assert!(belongs_to_pid("rustgram-notify-userpics-12", 12));
        assert!(!belongs_to_pid("rustgram-notify-userpics-123", 12));
        assert_eq!(
            ephemeral_stream_dir_name(12, 0xff),
            "rustgram_tmpstream_12_00000000000000ff"
        );
    }
}
