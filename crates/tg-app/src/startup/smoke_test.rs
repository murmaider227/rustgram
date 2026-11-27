//! `rustgram --smoke-test -workdir <scratch dir>`: opens the local storage the way a normal start
//! does, then exits without a window, a network connection or any system integration.
//!
//! The release workflow runs the packaged `rustgram.exe` this way twice: on an empty working dir
//! (first start) and on a copy of `tests/fixtures/smoke_profile_v0_1_0_alpha_2` (a profile written
//! by the v0.1.0-alpha.2 storage code), so a release that cannot read an older profile fails before
//! it is published.
//!
//! What runs, in startup order (`AccountManager::open` / `unlock` and the live host):
//! `startup_prefs.json` and `shell_state.json` (parsed strictly: the app itself silently falls back
//! to defaults on a parse error, which would reset the user's settings), `tdata/settingss`, the
//! core settings and language pack stored in it, `key_datas` and every account slot (MTProto
//! authorization, preferences, saved updates state) and each account's encrypted media cache.
//! Everything runs twice, so the second pass reads back what the first one wrote.
//!
//! The mode never starts without `-workdir` and never accepts the default profile
//! ([`super::cli_guard`] refuses both with exit code 2).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tg_core::domain::DomainStorage;
use tg_core::{CoreSettings, TdataDomainStorage};
use tg_storage::LocalStorage;
use tg_storage::cache::media_cache::MediaCacheIndex;

use crate::core_types::startup_types::StartupPrefs;
use crate::stand_in::settings::ShellSettings;

/// Command-line switch that selects this mode.
pub const SMOKE_TEST_FLAG: &str = "--smoke-test";

/// Report written into the working dir (release GUI builds have no console to print to).
pub const REPORT_FILE_NAME: &str = "smoke_test_report.txt";

/// Exit code when a check failed.
pub const EXIT_FAILED: i32 = 1;

/// What one account slot looked like on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCheck {
    /// Account slot index in `key_datas`.
    pub index: i32,
    /// Signed-in user id (0 when the slot is not signed in).
    pub user_id: u64,
    /// Main datacenter of the saved authorization.
    pub main_dc_id: i32,
    /// Number of saved MTProto auth keys.
    pub auth_keys: usize,
    /// Entries in the account's media cache after opening it (`None`: no cache on disk).
    pub media_cache_entries: Option<u64>,
}

/// Result of one pass over the working dir.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileCheck {
    /// The profile needs a local passcode, so the accounts were not opened.
    pub passcode_locked: bool,
    /// Account slots in `key_datas` order.
    pub accounts: Vec<AccountCheck>,
    /// Human-readable notes for the report.
    pub notes: Vec<String>,
    /// Checks that failed; empty means the pass succeeded.
    pub failures: Vec<String>,
}

/// Outcome of [`run_smoke_test`]: both passes plus the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeOutcome {
    /// First pass (opens, initializes and migrates).
    pub first: ProfileCheck,
    /// Second pass (reads back what the first pass left).
    pub second: ProfileCheck,
    /// Failures of both passes plus any difference between them.
    pub failures: Vec<String>,
}

impl SmokeOutcome {
    /// Process exit code: 0 when every check passed, [`EXIT_FAILED`] otherwise.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.failures.is_empty() {
            0
        } else {
            EXIT_FAILED
        }
    }

    /// Plain-text report (also written to [`REPORT_FILE_NAME`]).
    #[must_use]
    pub fn report(&self, version: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{version}: storage smoke test");
        let check = &self.second;
        if check.passcode_locked {
            let _ = writeln!(out, "profile is passcode-locked: accounts were not opened");
        }
        let _ = writeln!(out, "accounts: {}", check.accounts.len());
        for acc in &check.accounts {
            let cache = acc
                .media_cache_entries
                .map_or_else(|| "none".to_owned(), |n| format!("{n} entries"));
            let _ = writeln!(
                out,
                "  slot {}: signed in {}, main DC {}, {} auth keys, media cache {cache}",
                acc.index,
                acc.user_id != 0,
                acc.main_dc_id,
                acc.auth_keys
            );
        }
        for note in &self.first.notes {
            let _ = writeln!(out, "note: {note}");
        }
        if self.failures.is_empty() {
            let _ = writeln!(out, "result: PASS");
        } else {
            for failure in &self.failures {
                let _ = writeln!(out, "FAIL: {failure}");
            }
            let _ = writeln!(out, "result: FAIL");
        }
        out
    }
}

/// Runs both passes over `working_dir` (profile `data_name`, shell settings at `settings_path`).
#[must_use]
pub fn run_smoke_test(working_dir: &Path, data_name: &str, settings_path: &Path) -> SmokeOutcome {
    let first = check_profile(working_dir, data_name, settings_path);
    let second = check_profile(working_dir, data_name, settings_path);
    let mut failures: Vec<String> = first
        .failures
        .iter()
        .map(|f| format!("first pass: {f}"))
        .chain(second.failures.iter().map(|f| format!("second pass: {f}")))
        .collect();
    if first.failures.is_empty() && second.failures.is_empty() && first.accounts != second.accounts
    {
        failures.push(format!(
            "accounts changed between passes: {:?} then {:?}",
            first.accounts, second.accounts
        ));
    }
    SmokeOutcome {
        first,
        second,
        failures,
    }
}

/// Opens everything once. Never panics on profile data; every problem becomes a failure line.
#[must_use]
pub fn check_profile(working_dir: &Path, data_name: &str, settings_path: &Path) -> ProfileCheck {
    let mut check = ProfileCheck::default();
    if let Err(err) = std::fs::create_dir_all(working_dir) {
        check
            .failures
            .push(format!("cannot create the working dir: {err}"));
        return check;
    }

    check_json(
        &StartupPrefs::path_in(working_dir),
        "tdata/startup_prefs.json",
        &mut check,
        |raw| serde_json::from_str::<StartupPrefs>(raw).map(drop),
    );
    check_json(settings_path, "shell_state.json", &mut check, |raw| {
        serde_json::from_str::<ShellSettings>(raw).map(drop)
    });

    let tdata = working_dir.join("tdata");
    let local = match LocalStorage::open_or_init(&tdata) {
        Ok(local) => local,
        Err(err) => {
            check
                .failures
                .push(format!("tdata/settingss does not open: {err}"));
            return check;
        }
    };
    if let Err(err) = CoreSettings::load_from_local_storage(&local) {
        check.failures.push(format!(
            "core settings in tdata/settingss do not load (the app would reset them): {err}"
        ));
    }
    if let Err(err) = tg_lang::Instance::new().load_from_local_storage(&local) {
        check.failures.push(format!(
            "the saved language pack does not load (the app would drop it): {err}"
        ));
    }

    let mut domain = TdataDomainStorage::from_working_dir(working_dir, data_name);
    match domain.needs_passcode() {
        Ok(true) => {
            check.passcode_locked = true;
            check
                .notes
                .push("local passcode set: account data was not opened".to_owned());
            return check;
        }
        Ok(false) => {}
        Err(err) => {
            check
                .failures
                .push(format!("tdata/key_datas cannot be inspected: {err}"));
            return check;
        }
    }
    if let Err(err) = domain.load_accounts(b"") {
        check
            .failures
            .push(format!("accounts do not load from tdata: {err}"));
        return check;
    }
    let indices = domain
        .domain_storage()
        .map(|d| d.accounts().indices.clone())
        .unwrap_or_default();
    for index in indices {
        if let Some(acc) = check_account(&domain, &tdata, data_name, index, &mut check) {
            check.accounts.push(acc);
        }
    }
    check
}

fn check_json(
    path: &Path,
    label: &str,
    check: &mut ProfileCheck,
    parse: impl Fn(&str) -> serde_json::Result<()>,
) {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            if let Err(err) = parse(&raw) {
                check.failures.push(format!(
                    "{label} no longer parses (the app would silently reset it to defaults): {err}"
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            check.notes.push(format!("{label} absent (first start)"));
        }
        Err(err) => check
            .failures
            .push(format!("{label} cannot be read: {err}")),
    }
}

fn check_account(
    domain: &TdataDomainStorage,
    tdata: &Path,
    data_name: &str,
    index: i32,
    check: &mut ProfileCheck,
) -> Option<AccountCheck> {
    let auth = match domain.read_account_mtp_auth(index) {
        Ok(auth) => auth,
        Err(err) => {
            check.failures.push(format!(
                "slot {index}: saved authorization does not read: {err}"
            ));
            return None;
        }
    };
    let prefs = match domain.account_storage(index).map(|acc| acc.read_prefs()) {
        Some(Ok(prefs)) => prefs,
        Some(Err(err)) => {
            check
                .failures
                .push(format!("slot {index}: preferences do not read: {err}"));
            return None;
        }
        None => {
            check
                .failures
                .push(format!("slot {index}: account storage was not opened"));
            return None;
        }
    };
    if let Some(raw) = prefs.get(tg_session::UPDATES_STATE_PREFS_KEY)
        && tg_data::PersistedUpdatesState::from_bytes(raw).is_none()
    {
        check.failures.push(format!(
            "slot {index}: saved updates state does not parse (missed messages would be lost)"
        ));
    }
    let media_cache_entries = check_media_cache(tdata, data_name, index, &prefs, check);
    let (user_id, main_dc_id, auth_keys) =
        auth.map_or((0, 0, 0), |a| (a.user_id, a.main_dc_id, a.keys.len()));
    Some(AccountCheck {
        index,
        user_id,
        main_dc_id,
        auth_keys,
        media_cache_entries,
    })
}

fn cache_generation(prefs: &BTreeMap<Vec<u8>, Vec<u8>>) -> Option<String> {
    let raw = prefs.get(tg_session::account::CACHE_GENERATION_PREFS_KEY)?;
    let generation = std::str::from_utf8(raw).ok()?;
    (generation.len() == 16 && generation.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| generation.to_owned())
}

fn account_cache_dir(
    tdata: &Path,
    data_name: &str,
    index: i32,
    prefs: &BTreeMap<Vec<u8>, Vec<u8>>,
) -> PathBuf {
    let generation = cache_generation(prefs);
    tdata
        .join("media_cache")
        .join(tg_session::account::account_media_cache_dir_name(
            data_name,
            index,
            generation.as_deref(),
        ))
}

fn dir_has_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            dir_has_files(&path)
        } else {
            true
        }
    })
}

fn check_media_cache(
    tdata: &Path,
    data_name: &str,
    index: i32,
    prefs: &BTreeMap<Vec<u8>, Vec<u8>>,
    check: &mut ProfileCheck,
) -> Option<u64> {
    let dir = account_cache_dir(tdata, data_name, index, prefs);
    if !dir.exists() {
        return None;
    }
    let had_blobs = dir_has_files(&dir.join("blobs"));
    let Some(key) =
        tg_session::account::resolve_account_cache_key_for_tdata(tdata, data_name, index)
    else {
        check.failures.push(format!(
            "slot {index}: the media cache key cannot be derived from tdata"
        ));
        return None;
    };
    let cache = match MediaCacheIndex::open_with_key(&dir, key) {
        Ok(cache) => cache,
        Err(err) => {
            check.failures.push(format!(
                "slot {index}: the media cache does not open: {err}"
            ));
            return None;
        }
    };
    match cache.stats() {
        Ok(stats) => {
            if had_blobs && stats.entry_count == 0 {
                check.failures.push(format!(
                    "slot {index}: the media cache was purged on open (schema or key mismatch)"
                ));
            }
            Some(stats.entry_count)
        }
        Err(err) => {
            check
                .failures
                .push(format!("slot {index}: media cache stats fail: {err}"));
            None
        }
    }
}

/// Entry point for `run_from_cli`: runs the test, writes the report into the working dir, prints
/// it, and returns the process exit code.
#[must_use]
pub fn run_and_report(working_dir: &Path, data_name: &str, settings_path: &Path) -> i32 {
    let outcome = run_smoke_test(working_dir, data_name, settings_path);
    let report = outcome.report(&super::cli_guard::version_text());
    let mut code = outcome.exit_code();
    if let Err(err) = std::fs::write(working_dir.join(REPORT_FILE_NAME), &report) {
        code = EXIT_FAILED;
        eprintln!("Rustgram: could not write {REPORT_FILE_NAME}: {err}");
    }
    super::cli_guard::EarlyExit {
        code,
        message: report,
        to_stderr: code != 0,
    }
    .emit();
    code
}

#[cfg(test)]
#[path = "smoke_test_tests.rs"]
#[cfg(test)]
mod tests;
