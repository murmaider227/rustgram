//! Multi-account domain lifecycle, `tdata` persistence, session resume, login persistence,
//! and logout with `KeysDestroyer`.
//!
//! Port origin: `Telegram/SourceFiles/main/main_account.cpp`, `main/main_domain.cpp`,
//! and `storage/storage_domain.cpp`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tg_api::{ApiCredentials, DcId, LoginStateMachine, LoginStep, RpcClient};
use tg_core::domain::{AccountSession, Domain, DomainStorage as _, Environment, StartResult};
pub use tg_core::domain::{MAX_ACCOUNTS, PREMIUM_MAX_ACCOUNTS};
use tg_core::globals::GlobalConfig;
use tg_core::launcher::{
    ParsedArgs, ResolvedWorkingDir, WorkDirEnv, ensure_working_dir, resolve_working_dir,
};
use tg_core::persistence::TdataDomainStorage;
use tg_core::settings::CoreSettings;
use tg_core::settings_proxy::{ProxyData as CoreProxyData, ProxySettingsMode, SettingsProxy};
use tg_crypto::AuthKey;
use tg_data::{PersistedUpdatesState, TimeId, UserId};
use tg_lang::{Instance as LangInstance, default_language_id};
use tg_mtproto::{
    Client as MtprotoClient, KeysDestroyResult, KeysDestroyer, TgTransportFactory, TransportFactory,
};
use tg_storage::cache::media_cache::{
    derive_media_cache_key, register_cache_key, rekey_or_purge_cache_dir, unregister_cache_key,
};
use tg_storage::peer_id::PeerId as StoragePeerId;
use tg_storage::serialize::peer::{
    SerializedPeer, SerializedPeerKind, SerializedUser, deserialize_self_user, serialize_self_user,
};
use tg_storage::tdf::{
    clear_key, clear_name, compose_data_string, compute_data_name_key, to_file_part,
};
use tg_storage::{
    LocalStorage, MtpAuthKeyEntry, MtpAuthorization, PasscodeKdf, StorageEncryptionKey,
    StorageError, StoredSessionSettings,
};
use tg_tl::types as tl_types;
use tracing::{debug, info, warn};

/// `UserDataFlag::Self` bit from tdesktop `data/data_user.h:141`.
const USER_DATA_FLAG_SELF: i32 = 1 << 13;
/// `UserDataFlag::Premium` bit from tdesktop `data/data_user.h:142`.
const USER_DATA_FLAG_PREMIUM: i32 = 1 << 14;

use crate::config::{build_mtproto_client_config, mtproto_env_to_core};
use crate::error::{Result, SessionError};
use crate::proxy::{
    DEFAULT_PROXY_CHECK_TIMEOUT, check_proxy as run_proxy_check, core_proxy_to_transport,
    resolve_effective_proxy,
};
use crate::rpc::MtprotoRpc;
use crate::updates_store::UPDATES_STATE_PREFS_KEY;

/// Default maximum time to wait for `auth.logOut` RPCs during logout before proceeding with
/// key destruction and local storage cleanup.
pub const DEFAULT_LOGOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default per-key timeout for [`KeysDestroyer`] during logout or pending key cleanup.
pub const DEFAULT_DESTROY_KEYS_TIMEOUT: Duration = Duration::from_secs(15);

/// Relative-path queue inside `tdata/` recording account directories/files that could not be
/// removed immediately during logout (e.g. held open by WebView2 or an external viewer) so they
/// are retried on subsequent startups and shutdowns.
pub const PENDING_CLEANUP_FILENAME: &str = ".pending_cleanup";

static UNLOCKED_DOMAIN_KEYS: OnceLock<Mutex<HashMap<PathBuf, (String, AuthKey)>>> = OnceLock::new();

fn domain_key_registry() -> &'static Mutex<HashMap<PathBuf, (String, AuthKey)>> {
    UNLOCKED_DOMAIN_KEYS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn normalize_tdata_path(tdata_dir: &Path) -> PathBuf {
    fs::canonicalize(tdata_dir).unwrap_or_else(|_| tdata_dir.to_path_buf())
}

fn register_unlocked_domain_key(tdata_dir: &Path, data_name: &str, local_key: AuthKey) {
    let norm = normalize_tdata_path(tdata_dir);
    if let Ok(mut guard) = domain_key_registry().lock() {
        guard.insert(norm, (data_name.to_owned(), local_key));
    }
}

fn unregister_unlocked_domain_key(tdata_dir: &Path) {
    let norm = normalize_tdata_path(tdata_dir);
    if let Ok(mut guard) = domain_key_registry().lock() {
        guard.remove(&norm);
        guard.remove(tdata_dir);
    }
}

/// Looks up the unlocked `(data_name, local_key)` registered in memory for `tdata_dir` after
/// [`AccountManager::unlock`], so account media caches can derive per-account encryption keys
/// without touching `tdata` passcodes again.
#[must_use]
pub fn unlocked_domain_key_for_tdata(tdata_dir: &Path) -> Option<(String, AuthKey)> {
    let norm = normalize_tdata_path(tdata_dir);
    let guard = domain_key_registry().lock().ok()?;
    guard.get(&norm).or_else(|| guard.get(tdata_dir)).cloned()
}

/// Computes the 16-hex-char tdesktop account key folder name
/// (`ToFilePart(compute_data_name_key(compose_data_string(data_name, slot_index)))`).
#[must_use]
pub fn account_data_key_file_part(data_name: &str, slot_index: i32) -> String {
    let acc_data_name = compose_data_string(data_name, slot_index);
    to_file_part(compute_data_name_key(&acc_data_name))
}

/// Computes the per-account temporary folder name inside `tdata` (`temp_<acc_data_name>`).
#[must_use]
pub fn account_temp_dir_name(data_name: &str, slot_index: i32) -> String {
    let acc_data_name = compose_data_string(data_name, slot_index);
    format!("temp_{acc_data_name}")
}

/// Resolves the per-account [`StorageEncryptionKey`] for `slot_index` under `tdata_dir` from the
/// unlocked in-memory domain key (supporting passcode-locked `tdata`) or by opening an open
/// (no-passcode) `tdata` domain directly.
#[must_use]
pub fn resolve_account_cache_key_for_tdata(
    tdata_dir: &Path,
    data_name: &str,
    slot_index: i32,
) -> Option<StorageEncryptionKey> {
    let (name, local_key) = match unlocked_domain_key_for_tdata(tdata_dir) {
        Some(unlocked) => unlocked,
        None => {
            let domain = tg_storage::domain::DomainStorage::open(tdata_dir, data_name, b"").ok()?;
            (data_name.to_owned(), domain.local_key().clone())
        }
    };
    let generation = resolve_cache_generation(tdata_dir, &name, slot_index, &local_key);
    let acc_data_name = compose_data_string(&name, slot_index);
    Some(derive_media_cache_key(
        &local_key,
        &cache_key_name(&acc_data_name, generation.as_deref()),
    ))
}

/// Encrypted per-account prefs key holding the random media-cache generation chosen at login.
///
/// The cache folder and its key depend on it, so an account that later takes over the same slot
/// never opens (or can decrypt) files the previous login left behind.
pub const CACHE_GENERATION_PREFS_KEY: &[u8] = b"tgfork_cache_generation_v1";

/// Marker inserted into a path renamed for deletion (`<name>.deleted-<random>`). A tombstone
/// never belongs to a live account, so the pending-cleanup queue may always delete it.
const TOMBSTONE_MARKER: &str = ".deleted-";

type GenerationKey = (PathBuf, String);

static CACHE_GENERATIONS: OnceLock<Mutex<HashMap<GenerationKey, String>>> = OnceLock::new();

fn generation_registry() -> &'static Mutex<HashMap<GenerationKey, String>> {
    CACHE_GENERATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_cache_generation(tdata_dir: &Path, acc_data_name: &str, generation: Option<&str>) {
    let key = (normalize_tdata_path(tdata_dir), acc_data_name.to_owned());
    if let Ok(mut guard) = generation_registry().lock() {
        match generation {
            Some(generation) => {
                guard.insert(key, generation.to_owned());
            }
            None => {
                guard.remove(&key);
            }
        }
    }
}

fn registered_cache_generation(tdata_dir: &Path, acc_data_name: &str) -> Option<String> {
    let key = (normalize_tdata_path(tdata_dir), acc_data_name.to_owned());
    generation_registry().lock().ok()?.get(&key).cloned()
}

fn is_valid_cache_generation(generation: &str) -> bool {
    generation.len() == 16
        && generation
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    tg_crypto::random::random_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_cache_generation(prefs: &BTreeMap<Vec<u8>, Vec<u8>>) -> Option<String> {
    let raw = prefs.get(CACHE_GENERATION_PREFS_KEY)?;
    let generation = std::str::from_utf8(raw).ok()?;
    is_valid_cache_generation(generation).then(|| generation.to_owned())
}

fn resolve_cache_generation(
    tdata_dir: &Path,
    data_name: &str,
    slot_index: i32,
    local_key: &AuthKey,
) -> Option<String> {
    let acc_data_name = compose_data_string(data_name, slot_index);
    if let Some(generation) = registered_cache_generation(tdata_dir, &acc_data_name) {
        return Some(generation);
    }
    let storage =
        tg_storage::AccountStorage::open(tdata_dir, data_name, slot_index, local_key.clone())
            .ok()?;
    read_cache_generation(&storage.read_prefs().ok()?)
}

/// Key-derivation name for an account's media cache: the tdesktop data string, plus the
/// per-login generation for accounts signed in by this build.
fn cache_key_name(acc_data_name: &str, generation: Option<&str>) -> String {
    match generation {
        Some(generation) => format!("{acc_data_name}#cache-generation:{generation}"),
        None => acc_data_name.to_owned(),
    }
}

/// Media cache folder name for a slot: `<file_part>-<generation>` for accounts signed in by this
/// build, `<file_part>` for accounts signed in before the generation existed.
#[must_use]
pub fn account_media_cache_dir_name(
    data_name: &str,
    slot_index: i32,
    generation: Option<&str>,
) -> String {
    let file_part = account_data_key_file_part(data_name, slot_index);
    match generation {
        Some(generation) => format!("{file_part}-{generation}"),
        None => file_part,
    }
}

/// Resolves the account's media cache folder (`tdata/media_cache/<file_part>[-<generation>]`)
/// using the generation registered by [`AccountManager`] after unlock or login.
#[must_use]
pub fn account_media_cache_dir_for_tdata(
    tdata_dir: &Path,
    data_name: &str,
    slot_index: i32,
) -> PathBuf {
    let acc_data_name = compose_data_string(data_name, slot_index);
    let generation = registered_cache_generation(tdata_dir, &acc_data_name).or_else(|| {
        let (_, local_key) = unlocked_domain_key_for_tdata(tdata_dir)?;
        resolve_cache_generation(tdata_dir, data_name, slot_index, &local_key)
    });
    tdata_dir
        .join("media_cache")
        .join(account_media_cache_dir_name(
            data_name,
            slot_index,
            generation.as_deref(),
        ))
}

/// Lists `media_cache/<file_part>-<generation>` folders of the slot other than `keep`.
fn stale_media_cache_dirs(tdata_dir: &Path, file_part: &str, keep: &str) -> Vec<PathBuf> {
    let prefix = format!("{file_part}-");
    let Ok(entries) = fs::read_dir(tdata_dir.join("media_cache")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name != keep
                    && name
                        .strip_prefix(&prefix)
                        .is_some_and(is_valid_cache_generation)
            })
        })
        .map(|entry| entry.path())
        .collect()
}

fn is_tombstone_name(name: &str) -> bool {
    name.contains(TOMBSTONE_MARKER)
}

/// Renames `target` to a sibling tombstone (`<name>.deleted-<random>`) so its live name is free
/// at once, then deletes the tombstone. Returns `None` when `target` is gone (or was never
/// there), `Some(tombstone)` when the tombstone is still on disk, and `Some(target)` when the
/// rename failed and the live path itself could not be removed.
fn remove_via_tombstone(target: &Path) -> Option<PathBuf> {
    if fs::symlink_metadata(target).is_err() {
        return None;
    }
    let tombstone = target
        .file_name()
        .and_then(|n| n.to_str())
        .map(|name| target.with_file_name(format!("{name}{TOMBSTONE_MARKER}{}", random_hex(6))));
    let victim = match tombstone {
        Some(tombstone) if fs::rename(target, &tombstone).is_ok() => tombstone,
        _ => target.to_path_buf(),
    };
    if try_remove_path_with_retries(&victim) {
        None
    } else {
        Some(victim)
    }
}

/// Deletes tombstones left directly inside `dir` (e.g. when the queue file could not be written).
fn sweep_tombstones_in(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_str().is_some_and(is_tombstone_name)
            && try_remove_path_with_retries(&entry.path())
        {
            removed += 1;
        }
    }
    removed
}

fn is_tombstone_entry(rel: &Path) -> bool {
    rel.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_tombstone_name)
}

fn is_safe_relative_tdata_subpath(rel: &Path) -> bool {
    let mut saw_normal = false;
    for comp in rel.components() {
        match comp {
            Component::Normal(_) => saw_normal = true,
            _ => return false,
        }
    }
    saw_normal
}

fn try_remove_path_with_retries(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    for attempt in 0..3u32 {
        let res = if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        if res.is_ok() || !path.exists() {
            return true;
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(20 * u64::from(attempt + 1)));
        }
    }
    // Best-effort partial cleanup inside a busy directory so unlocked files are still purged
    // immediately while locked handles stay queued in `.pending_cleanup`.
    if path.is_dir() {
        best_effort_prune_dir_contents(path);
        let _ = fs::remove_dir(path);
    }
    !path.exists()
}

fn best_effort_prune_dir_contents(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            best_effort_prune_dir_contents(&p);
            let _ = fs::remove_dir(&p);
        } else {
            let _ = fs::remove_file(&p);
        }
    }
}

/// Queue-line prefix for a live account path that could be neither renamed to a tombstone nor
/// deleted at logout. Such entries are dropped as soon as a new login takes the slot
/// ([`drop_live_pending_cleanups`]), so the queue can never delete a newer account's data.
const LIVE_ENTRY_PREFIX: &str = "live:";

/// Records `target` (which must live strictly inside `tdata_dir`) in `tdata/.pending_cleanup`
/// so future startups and shutdowns retry removing it once external locks release.
///
/// Tombstones (`<name>.deleted-<random>`) are recorded as is; any other path is recorded as a
/// `live:` entry that a later login in the same slot cancels.
pub fn record_pending_cleanup(tdata_dir: &Path, target: &Path) {
    let Ok(rel) = target.strip_prefix(tdata_dir) else {
        return;
    };
    if !is_safe_relative_tdata_subpath(rel) {
        return;
    }
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    if rel_str.is_empty() || rel_str.contains('\n') || rel_str.contains('\r') {
        return;
    }
    let line = if is_tombstone_entry(rel) {
        rel_str
    } else {
        format!("{LIVE_ENTRY_PREFIX}{rel_str}")
    };
    let mut entries = read_pending_queue(tdata_dir);
    if !entries.iter().any(|existing| existing == &line) {
        entries.push(line);
    }
    write_pending_queue(tdata_dir, &entries);
}

fn read_pending_queue(tdata_dir: &Path) -> Vec<String> {
    fs::read_to_string(tdata_dir.join(PENDING_CLEANUP_FILENAME))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

fn write_pending_queue(tdata_dir: &Path, entries: &[String]) {
    let queue_path = tdata_dir.join(PENDING_CLEANUP_FILENAME);
    let res = if entries.is_empty() {
        match fs::remove_file(&queue_path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    } else {
        fs::write(&queue_path, format!("{}\n", entries.join("\n")))
    };
    if let Err(err) = res {
        warn!(path = %queue_path.display(), error = %err, "failed to write pending cleanup queue");
    }
}

/// Removes queued `live:` entries for `slot_paths` (and their ancestors and descendants): a new
/// login now owns those paths, so a later retry must not delete them.
fn drop_live_pending_cleanups(tdata_dir: &Path, slot_paths: &[PathBuf]) {
    let entries = read_pending_queue(tdata_dir);
    let kept: Vec<String> = entries
        .iter()
        .filter(|line| {
            let Some(rel) = line.strip_prefix(LIVE_ENTRY_PREFIX) else {
                return true;
            };
            let target = tdata_dir.join(rel);
            !slot_paths
                .iter()
                .any(|p| p.starts_with(&target) || target.starts_with(p))
        })
        .cloned()
        .collect();
    if kept.len() != entries.len() {
        write_pending_queue(tdata_dir, &kept);
    }
}

/// Retries deleting all paths recorded in `tdata/.pending_cleanup`, removing entries that now
/// succeed and retaining entries still held open by external processes.
///
/// Returns the number of previously pending targets successfully deleted.
///
/// Untagged entries that are not tombstones (live account paths written by older builds, which
/// did not cancel them on a new login) are dropped without deleting anything: that slot may now
/// hold a different account's data. Tombstones left directly in `tdata`, `media_cache`,
/// `webview_profiles` and `bot_storage` are swept too, in case the queue itself was not written.
pub fn process_pending_cleanups(tdata_dir: &Path) -> usize {
    let mut cleaned = ["", "media_cache", "webview_profiles", "bot_storage"]
        .iter()
        .map(|sub| sweep_tombstones_in(&tdata_dir.join(sub)))
        .sum::<usize>();
    let entries = read_pending_queue(tdata_dir);
    if entries.is_empty() {
        return cleaned;
    }
    let mut remaining = Vec::new();
    for line in &entries {
        let (rel_str, live) = match line.strip_prefix(LIVE_ENTRY_PREFIX) {
            Some(rest) => (rest, true),
            None => (line.as_str(), false),
        };
        let rel = Path::new(rel_str);
        if !is_safe_relative_tdata_subpath(rel) {
            continue;
        }
        if !live && !is_tombstone_entry(rel) {
            warn!(
                entry = line.as_str(),
                "dropping untagged pending cleanup entry from an older build"
            );
            continue;
        }
        let target = tdata_dir.join(rel);
        if fs::symlink_metadata(&target).is_err() {
            continue;
        }
        match remove_via_tombstone(&target) {
            None => cleaned += 1,
            Some(busy) => match busy.strip_prefix(tdata_dir) {
                Ok(busy_rel) if is_tombstone_entry(busy_rel) => {
                    remaining.push(busy_rel.to_string_lossy().replace('\\', "/"));
                }
                _ => remaining.push(line.clone()),
            },
        }
    }
    if remaining != entries {
        write_pending_queue(tdata_dir, &remaining);
    }
    cleaned
}

/// Removes stale unlocked temporary files inside `tdata/temp_*` directories (skipping any files
/// currently held open by an external viewer app).
pub fn cleanup_stale_temp_dirs(tdata_dir: &Path) {
    let Ok(entries) = fs::read_dir(tdata_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with("temp_") {
            best_effort_prune_dir_contents(&path);
            let _ = fs::remove_dir(&path);
        }
    }
}

/// Options controlling how [`AccountManager`] opens a working directory.
#[derive(Clone)]
pub struct WorkingDirOpenOptions {
    /// Resolved working directory path (parent of `tdata`).
    pub working_dir: PathBuf,
    /// Profile data name (`"data"` by default, or `-key <name>`).
    pub data_name: String,
    /// Telegram API credentials (`api_id` and `api_hash`, never hardcoded).
    pub credentials: ApiCredentials,
    /// Default Telegram environment for new/unconfigured slots (`Environment::Test` when
    /// launched with `-testmode`, matching `MTP::Environment::Test`).
    pub default_environment: Environment,
    /// Optional explicit language code override (otherwise loaded from `tdata` / `tg-lang`).
    pub lang_code_override: Option<String>,
    /// Optional custom MTProto [`TransportFactory`] (e.g. `FakeTransportFactory` for tests).
    pub transport_factory: Option<Arc<dyn TransportFactory>>,
    /// Whether to disable PFS temporary keys (useful for deterministic fake-transport tests;
    /// defaults to `true`, matching tdesktop).
    pub use_pfs: bool,
    /// Maximum time to wait for `auth.logOut` RPCs before continuing with local cleanup.
    pub logout_timeout: Duration,
    /// Per-key timeout passed to [`KeysDestroyer`] when destroying stale auth keys.
    pub destroy_keys_timeout: Duration,
}

impl std::fmt::Debug for WorkingDirOpenOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkingDirOpenOptions")
            .field("working_dir", &self.working_dir)
            .field("data_name", &self.data_name)
            .field("api_id", &self.credentials.api_id)
            .field("default_environment", &self.default_environment)
            .field("lang_code_override", &self.lang_code_override)
            .field("has_custom_transport", &self.transport_factory.is_some())
            .field("use_pfs", &self.use_pfs)
            .field("logout_timeout", &self.logout_timeout)
            .field("destroy_keys_timeout", &self.destroy_keys_timeout)
            .finish()
    }
}

impl WorkingDirOpenOptions {
    /// Creates options from an explicit working directory path and API credentials.
    #[must_use]
    pub fn new(working_dir: impl Into<PathBuf>, credentials: ApiCredentials) -> Self {
        Self {
            working_dir: working_dir.into(),
            data_name: "data".to_owned(),
            credentials,
            default_environment: Environment::Production,
            lang_code_override: None,
            transport_factory: None,
            use_pfs: true,
            logout_timeout: DEFAULT_LOGOUT_TIMEOUT,
            destroy_keys_timeout: DEFAULT_DESTROY_KEYS_TIMEOUT,
        }
    }

    /// Resolves the working directory and profile key name using `tg-core`'s
    /// [`resolve_working_dir`] (`LaunchArgs` / `ParsedArgs`).
    #[must_use]
    pub fn from_launch_args(
        args: &ParsedArgs,
        env: &WorkDirEnv,
        credentials: ApiCredentials,
    ) -> (Self, ResolvedWorkingDir) {
        let resolved = resolve_working_dir(args, env);
        let data_name = if args.key.is_empty() {
            "data".to_owned()
        } else {
            args.key.clone()
        };
        let default_environment = if args.test_mode {
            Environment::Test
        } else {
            Environment::Production
        };
        let opts = Self {
            working_dir: resolved.path.clone(),
            data_name,
            credentials,
            default_environment,
            lang_code_override: None,
            transport_factory: None,
            use_pfs: true,
            logout_timeout: DEFAULT_LOGOUT_TIMEOUT,
            destroy_keys_timeout: DEFAULT_DESTROY_KEYS_TIMEOUT,
        };
        (opts, resolved)
    }

    /// Overrides the default Telegram [`Environment`] for new/unconfigured slots.
    #[must_use]
    pub const fn with_environment(mut self, environment: Environment) -> Self {
        self.default_environment = environment;
        self
    }

    /// Enables or disables Telegram test-DC mode (`Environment::Test`) for new/unconfigured slots.
    #[must_use]
    pub const fn with_test_mode(mut self, test_mode: bool) -> Self {
        self.default_environment = if test_mode {
            Environment::Test
        } else {
            Environment::Production
        };
        self
    }

    /// Overrides the profile data name (`-key`).
    #[must_use]
    pub fn with_data_name(mut self, data_name: impl Into<String>) -> Self {
        let name = data_name.into();
        if !name.is_empty() {
            self.data_name = name;
        }
        self
    }

    /// Injects a custom [`TransportFactory`] (e.g. [`tg_mtproto::FakeTransportFactory`]).
    #[must_use]
    pub fn with_transport_factory(mut self, factory: Arc<dyn TransportFactory>) -> Self {
        self.transport_factory = Some(factory);
        self
    }

    /// Controls whether MTProto Perfect Forward Secrecy (PFS) temporary keys are used.
    #[must_use]
    pub fn with_pfs(mut self, use_pfs: bool) -> Self {
        self.use_pfs = use_pfs;
        self
    }

    /// Overrides the timeout applied to `auth.logOut` RPCs during account logout.
    #[must_use]
    pub const fn with_logout_timeout(mut self, timeout: Duration) -> Self {
        self.logout_timeout = timeout;
        self
    }

    /// Overrides the per-key timeout passed to [`KeysDestroyer`].
    #[must_use]
    pub const fn with_destroy_keys_timeout(mut self, timeout: Duration) -> Self {
        self.destroy_keys_timeout = timeout;
        self
    }
}

/// Summary of one account slot managed by [`AccountManager`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSlotInfo {
    /// Slot index inside `Domain` (`0`, `1`, ...).
    pub index: i32,
    /// Per-account `tdata` data name (`"data"`, `"data#2"`, `"data#3"`, ...).
    pub data_name: String,
    /// Target Telegram environment.
    pub environment: Environment,
    /// Current main DC ID.
    pub main_dc_id: DcId,
    /// Authenticated Telegram user ID, if signed in.
    pub user_id: Option<UserId>,
    /// Display name derived from cached self user (`first_name last_name` or `@username`).
    pub display_name: String,
    /// Public username without `@`, if known.
    pub username: String,
    /// Phone number string, if known.
    pub phone: String,
    /// Whether this account has Telegram Premium (expands `max_accounts()` from 3 up to 6).
    pub premium: bool,
    /// Unread badge count reported by this account.
    pub unread_badge: i32,
    /// Whether all unread items in this account are muted.
    pub unread_badge_muted: bool,
    /// Number of persistent DC auth keys currently loaded in memory.
    pub persistent_keys_count: usize,
    /// Whether this slot is the active account in `Domain`.
    pub is_active: bool,
}

/// Live runtime state for one account slot (`Main::Account`).
pub struct LiveAccount {
    /// Account slot index.
    pub index: i32,
    /// Target Telegram environment.
    pub environment: Environment,
    /// Underlying MTProto client seeded with any persisted keys.
    pub client: MtprotoClient,
    /// `tg-api`-compatible RPC adapter over `client`.
    pub rpc: MtprotoRpc,
    /// Authenticated user ID (`None` if not yet logged in).
    pub user_id: Option<UserId>,
    /// Cached self TL user (populated upon login or fetch).
    pub self_user: Option<tl_types::User>,
    /// Keys loaded from `MtpAuthorization::keys_to_destroy` awaiting server cleanup.
    pub keys_to_destroy: Vec<(DcId, AuthKey)>,
}

impl std::fmt::Debug for LiveAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveAccount")
            .field("index", &self.index)
            .field("environment", &self.environment)
            .field("main_dc_id", &self.client.main_dc_id())
            .field("user_id", &self.user_id)
            .field("keys_count", &self.client.export_persistent_keys().len())
            .field("keys_to_destroy", &self.keys_to_destroy.len())
            .finish()
    }
}

impl LiveAccount {
    /// Returns `true` if this account has an authenticated Telegram user ID.
    #[must_use]
    pub const fn is_authorized(&self) -> bool {
        self.user_id.is_some()
    }

    /// Returns the account's current main DC ID.
    #[must_use]
    pub fn main_dc_id(&self) -> DcId {
        self.client.main_dc_id()
    }

    /// Builds an [`MtpAuthorization`] snapshot from the client's current state
    /// (`Main::Account::serializeMtpAuthorization`).
    #[must_use]
    pub fn snapshot_mtp_authorization(&self) -> MtpAuthorization {
        let keys = self
            .client
            .export_persistent_keys()
            .into_iter()
            .map(|(dc_id, key)| MtpAuthKeyEntry { dc_id, key })
            .collect();
        let keys_to_destroy = self
            .keys_to_destroy
            .iter()
            .map(|(dc_id, key)| MtpAuthKeyEntry {
                dc_id: *dc_id,
                key: key.clone(),
            })
            .collect();
        MtpAuthorization {
            user_id: self.user_id.map_or(0, |u| u.0),
            main_dc_id: self.client.main_dc_id(),
            keys,
            keys_to_destroy,
        }
    }

    /// Starts a [`LoginFlow`] over this account's [`MtprotoRpc`].
    #[must_use]
    pub fn login_flow(&self, credentials: ApiCredentials) -> LoginFlow<MtprotoRpc> {
        LoginFlow::new(self.rpc.clone(), credentials)
    }

    /// Destroys any stale keys queued in `self.keys_to_destroy` via `KeysDestroyer`,
    /// keeping any unconfirmed keys for retry on next launch.
    pub async fn destroy_pending_keys(&mut self) -> Option<KeysDestroyResult> {
        if self.keys_to_destroy.is_empty() {
            return None;
        }
        let batch = std::mem::take(&mut self.keys_to_destroy);
        let result = self.client.destroy_stale_keys(batch).await;
        self.keys_to_destroy = result.remaining.clone();
        Some(result)
    }

    /// Shuts down all active MTProto sessions for this account.
    pub fn shutdown(&self) {
        self.client.shutdown();
    }
}

/// Manages a `tdata` working directory, multi-account [`Domain`], and live [`LiveAccount`]
/// instances (port of `Main::Domain` + `Main::Account` runtime wiring).
pub struct AccountManager {
    options: WorkingDirOpenOptions,
    local_storage: LocalStorage,
    global_config: GlobalConfig,
    core_settings: CoreSettings,
    lang_code: String,
    domain: Domain<TdataDomainStorage>,
    live_accounts: BTreeMap<i32, LiveAccount>,
}

impl AccountManager {
    /// Opens (or initializes) a working directory's `tdata` without unlocking `key_datas` yet.
    ///
    /// Call [`Self::needs_passcode`] to check whether a local passcode is required, then
    /// call [`Self::unlock`] to decrypt accounts and seed MTProto clients.
    pub fn open(options: WorkingDirOpenOptions) -> Result<Self> {
        ensure_working_dir(&options.working_dir)?;
        let tdata = options.working_dir.join("tdata");
        process_pending_cleanups(&tdata);
        cleanup_stale_temp_dirs(&tdata);

        let mut local_storage = LocalStorage::open_or_init(&tdata)?;
        let mut global_config = GlobalConfig::default();
        global_config.apply_global_settings(local_storage.settings());
        global_config.working_dir = options.working_dir.clone();
        global_config.key_file = if options.data_name == "data" {
            String::new()
        } else {
            options.data_name.clone()
        };

        let core_settings =
            CoreSettings::load_from_local_storage(&local_storage).unwrap_or_else(|err| {
                warn!(error = %err, "failed to load CoreSettings from tdata; using defaults");
                CoreSettings::default()
            });

        // Load persisted language pack from tdata into the process-wide `tg_lang::global_instance()`
        // before any UI is constructed (`Local::start` -> `Local::readLangPack`).
        let mut lang_instance = LangInstance::new();
        let loaded_outcome = match lang_instance.load_from_local_storage(&local_storage) {
            Ok(outcome) => outcome,
            Err(err) => {
                warn!(error = %err, "failed to load persisted langpack from tdata; using defaults");
                None
            }
        };
        if loaded_outcome == Some(tg_lang::SerializeLoadOutcome::ReloadedModifiedCustomFile)
            && let Err(err) = lang_instance.save_to_local_storage(&mut local_storage)
        {
            warn!(error = %err, "failed to re-save modified custom langpack to tdata");
        }
        if loaded_outcome.is_some() {
            tg_lang::with_current_mut(|global| {
                global.copy_from(&lang_instance);
            });
        }
        tg_lang::set_global_tdata_dir(Some(tdata.clone()));
        let lang_code = options.lang_code_override.clone().unwrap_or_else(|| {
            let id = lang_instance.id();
            if id.is_empty() {
                default_language_id().to_owned()
            } else {
                id.to_owned()
            }
        });

        let storage =
            TdataDomainStorage::from_working_dir(&options.working_dir, &options.data_name)
                .with_default_environment(options.default_environment);
        let domain = Domain::new(&options.data_name, storage);

        Ok(Self {
            options,
            local_storage,
            global_config,
            core_settings,
            lang_code,
            domain,
            live_accounts: BTreeMap::new(),
        })
    }

    /// Convenience helper that opens the working directory and immediately unlocks with
    /// `passcode` (pass `b""` when no local passcode is set).
    pub fn open_and_unlock(options: WorkingDirOpenOptions, passcode: &[u8]) -> Result<Self> {
        let mut mgr = Self::open(options)?;
        mgr.unlock(passcode)?;
        Ok(mgr)
    }

    /// Returns `true` if `tdata/key_datas` requires a non-empty local passcode before unlock.
    pub fn needs_passcode(&self) -> Result<bool> {
        Ok(self.domain.storage().needs_passcode()?)
    }

    /// Returns `true` once [`Self::unlock`] has succeeded.
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        self.domain.started()
    }

    /// Returns the working directory path (parent of `tdata`).
    #[must_use]
    pub fn working_dir(&self) -> &Path {
        &self.options.working_dir
    }

    /// Returns the API credentials configured for this manager.
    #[must_use]
    pub fn credentials(&self) -> &ApiCredentials {
        &self.options.credentials
    }

    /// Borrows the loaded [`LocalStorage`].
    #[must_use]
    pub fn local_storage(&self) -> &LocalStorage {
        &self.local_storage
    }

    /// Mutably borrows the loaded [`LocalStorage`].
    pub fn local_storage_mut(&mut self) -> &mut LocalStorage {
        &mut self.local_storage
    }

    /// Borrows the loaded [`GlobalConfig`].
    #[must_use]
    pub fn global_config(&self) -> &GlobalConfig {
        &self.global_config
    }

    /// Mutably borrows the loaded [`GlobalConfig`].
    pub fn global_config_mut(&mut self) -> &mut GlobalConfig {
        &mut self.global_config
    }

    /// Borrows the loaded [`CoreSettings`].
    #[must_use]
    pub fn core_settings(&self) -> &CoreSettings {
        &self.core_settings
    }

    /// Borrows the loaded [`SettingsProxy`].
    #[must_use]
    pub fn proxy_settings(&self) -> &SettingsProxy {
        &self.core_settings.proxy
    }

    /// Updates [`SettingsProxy`], persists `CoreSettings` to `tdata/settingss`, and
    /// reconnects all live account DCs through the new proxy without losing sessions.
    pub fn set_proxy_settings(&mut self, proxy: SettingsProxy) -> Result<()> {
        self.core_settings.proxy = proxy;
        self.core_settings
            .save_to_local_storage(&mut self.local_storage)?;
        self.apply_proxy_to_live_accounts();
        Ok(())
    }

    /// Selects `proxy` with `mode`, persists to `tdata/settingss`, and reconnects all DCs.
    pub fn select_proxy(&mut self, proxy: CoreProxyData, mode: ProxySettingsMode) -> Result<()> {
        self.core_settings.proxy.set_selected(proxy);
        self.core_settings.proxy.set_settings(mode);
        self.core_settings
            .save_to_local_storage(&mut self.local_storage)?;
        self.apply_proxy_to_live_accounts();
        Ok(())
    }

    /// Persists the current [`CoreSettings`] (including proxy settings) to `tdata/settingss`.
    pub fn persist_core_settings(&mut self) -> Result<()> {
        self.core_settings
            .save_to_local_storage(&mut self.local_storage)?;
        Ok(())
    }

    /// Checks real MTProto reachability and RTT for `proxy` against the active account's DC.
    pub async fn check_proxy(
        &self,
        proxy: &CoreProxyData,
        timeout: Option<Duration>,
    ) -> std::result::Result<Duration, String> {
        let tp = core_proxy_to_transport(proxy);
        let try_ipv6 = self.core_settings.proxy.try_ipv6();
        let timeout = timeout.unwrap_or(DEFAULT_PROXY_CHECK_TIMEOUT);
        if let Some(acc) = self.active_account() {
            run_proxy_check(
                acc.client.main_dc_id(),
                acc.client.dc_options(),
                &tp,
                try_ipv6,
                timeout,
            )
            .await
        } else {
            let dc_options = tg_mtproto::DcOptions::new(crate::config::core_env_to_mtproto(
                self.options.default_environment,
            ));
            run_proxy_check(
                crate::config::DEFAULT_MAIN_DC_ID,
                &dc_options,
                &tp,
                try_ipv6,
                timeout,
            )
            .await
        }
    }

    fn apply_proxy_to_live_accounts(&self) {
        let effective = resolve_effective_proxy(&self.core_settings.proxy);
        let try_ipv6 = self.core_settings.proxy.try_ipv6();
        for live in self.live_accounts.values() {
            live.client.set_try_ipv6(try_ipv6);
            live.client.set_proxy(effective.clone());
        }
    }

    /// Mutably borrows the loaded [`CoreSettings`].
    pub fn core_settings_mut(&mut self) -> &mut CoreSettings {
        &mut self.core_settings
    }

    /// Updates in-memory [`CoreSettings`] and persists `dbiApplicationSettings` + `dbiPowerSaving`
    /// to `tdata/settingss` (`Local::writeSettings`).
    pub fn save_core_settings(&mut self, core: &CoreSettings) -> Result<()> {
        self.core_settings = core.clone();
        self.core_settings
            .save_to_local_storage(&mut self.local_storage)?;
        Ok(())
    }

    /// Updates both [`CoreSettings`] and [`GlobalConfig`] and writes `tdata/settingss` atomically.
    pub fn save_global_and_core_settings(
        &mut self,
        core: &CoreSettings,
        global: &GlobalConfig,
    ) -> Result<()> {
        let proxy_changed = self.core_settings.proxy != core.proxy;
        self.core_settings = core.clone();
        self.global_config = global.clone();
        self.global_config
            .write_to_global_settings(self.local_storage.settings_mut());
        self.core_settings
            .save_to_local_storage(&mut self.local_storage)?;
        if proxy_changed {
            self.apply_proxy_to_live_accounts();
        }
        Ok(())
    }

    /// Loads per-account [`StoredSessionSettings`] from `dbiSessionSettings` (`0x4d`) for `index`,
    /// returning defaults when no session settings file exists yet.
    pub fn load_session_settings(&self, index: i32) -> Result<StoredSessionSettings> {
        let Some(acc_storage) = self.domain.storage().account_storage(index) else {
            return Err(SessionError::AccountNotFound(index));
        };
        Ok(acc_storage
            .read_parsed_session_settings()?
            .map(|(parsed, _)| parsed)
            .unwrap_or_default())
    }

    /// Persists per-account [`StoredSessionSettings`] into encrypted `lskUserSettings` for `index`.
    pub fn save_session_settings(
        &mut self,
        index: i32,
        settings: &StoredSessionSettings,
    ) -> Result<()> {
        let Some(acc_storage) = self.domain.storage_mut().account_storage_mut(index) else {
            return Err(SessionError::AccountNotFound(index));
        };
        acc_storage.write_parsed_session_settings(settings)?;
        Ok(())
    }

    /// Loads the persisted set of bot peer IDs trusted to open Mini Apps for account `index`
    /// (`Storage::Account::isPeerTrustedOpenWebView`).
    pub fn trusted_mini_app_bots(&self, index: i32) -> Result<BTreeSet<i64>> {
        Ok(self.load_session_settings(index)?.trusted_mini_app_bots)
    }

    /// Returns `true` if `bot_peer_id` is persisted as trusted to open Mini Apps for account
    /// `index`.
    pub fn is_mini_app_bot_trusted(&self, index: i32, bot_peer_id: i64) -> Result<bool> {
        Ok(self
            .load_session_settings(index)?
            .trusted_mini_app_bots
            .contains(&bot_peer_id))
    }

    /// Marks `bot_peer_id` as trusted to open Mini Apps for account `index` and persists the
    /// updated encrypted `StoredSessionSettings` to `tdata`.
    pub fn trust_mini_app_bot(&mut self, index: i32, bot_peer_id: i64) -> Result<()> {
        let mut settings = self.load_session_settings(index)?;
        if settings.trusted_mini_app_bots.insert(bot_peer_id) {
            self.save_session_settings(index, &settings)?;
        }
        Ok(())
    }

    /// Removes `bot_peer_id` from the persisted trusted Mini App bots set for account `index`.
    pub fn untrust_mini_app_bot(&mut self, index: i32, bot_peer_id: i64) -> Result<()> {
        let mut settings = self.load_session_settings(index)?;
        if settings.trusted_mini_app_bots.remove(&bot_peer_id) {
            self.save_session_settings(index, &settings)?;
        }
        Ok(())
    }

    /// Returns `true` if the unlocked domain currently has a local passcode configured
    /// (`Storage::Domain::hasPasscode`).
    #[must_use]
    pub fn has_local_passcode(&self) -> bool {
        self.domain.storage().has_passcode()
    }

    /// Returns `true` if app launch lock is currently armed (`Storage::Domain::appLockEnabled`).
    #[must_use]
    pub fn app_lock_enabled(&self) -> bool {
        self.domain.storage().app_lock_enabled()
    }

    /// Checks `passcode` against the active local passcode wrap in `tdata/key_datas`
    /// (`Storage::Domain::checkPasscode`).
    #[must_use]
    pub fn check_local_passcode(&self, passcode: &[u8]) -> bool {
        self.domain.storage().check_passcode(passcode)
    }

    /// Sets, changes, or removes the local passcode in `tdata/key_datas` using tdesktop's
    /// default Argon2id KDF (`Core::LocalPasscode` + `Storage::Domain::setPasscode`).
    ///
    /// - When a passcode is already configured, `current_passcode` must match it (otherwise
    ///   returns [`SessionError::IncorrectPasscode`]).
    /// - Passing an empty `new_passcode` removes the passcode, restores the open wrap, clears
    ///   `CoreSettings::system_unlock_enabled`, and persists `tdata/settingss`.
    pub fn set_local_passcode(
        &mut self,
        current_passcode: Option<&[u8]>,
        new_passcode: &[u8],
    ) -> Result<()> {
        self.set_local_passcode_with_kdf(
            current_passcode,
            new_passcode,
            PasscodeKdf::default_argon2id(),
        )
    }

    /// Variant of [`Self::set_local_passcode`] accepting an explicit [`PasscodeKdf`]
    /// (useful for deterministic fast tests).
    pub fn set_local_passcode_with_kdf(
        &mut self,
        current_passcode: Option<&[u8]>,
        new_passcode: &[u8],
        kdf: PasscodeKdf,
    ) -> Result<()> {
        let old_local_key = self.domain.storage().local_key().cloned();
        let domain_storage = self
            .domain
            .storage_mut()
            .domain_storage_mut()
            .ok_or_else(|| {
                SessionError::Storage(StorageError::CorruptData(
                    "domain storage is not unlocked".into(),
                ))
            })?;

        let verification = if domain_storage.has_passcode() {
            let current = current_passcode.ok_or(SessionError::IncorrectPasscode)?;
            let token = domain_storage
                .verify_passcode(current)
                .ok_or(SessionError::IncorrectPasscode)?;
            Some(token)
        } else {
            None
        };

        domain_storage
            .set_passcode_verified(new_passcode, kdf, verification)
            .map_err(|err| match err {
                StorageError::IncorrectPasscode => SessionError::IncorrectPasscode,
                other => SessionError::Storage(other),
            })?;

        self.sync_media_cache_keys(old_local_key.as_ref());

        if new_passcode.is_empty() && self.core_settings.system_unlock_enabled {
            self.core_settings.system_unlock_enabled = false;
            self.core_settings
                .save_to_local_storage(&mut self.local_storage)?;
        }
        Ok(())
    }

    /// Removes the configured local passcode after verifying `current_passcode`.
    pub fn remove_local_passcode(&mut self, current_passcode: &[u8]) -> Result<()> {
        self.set_local_passcode(Some(current_passcode), b"")
    }

    /// Removes the configured local passcode using an explicit [`PasscodeKdf`].
    pub fn remove_local_passcode_with_kdf(
        &mut self,
        current_passcode: &[u8],
        kdf: PasscodeKdf,
    ) -> Result<()> {
        self.set_local_passcode_with_kdf(Some(current_passcode), b"", kdf)
    }

    /// Borrows the underlying multi-account [`Domain`].
    #[must_use]
    pub fn domain(&self) -> &Domain<TdataDomainStorage> {
        &self.domain
    }

    /// Mutably borrows the underlying multi-account [`Domain`].
    pub fn domain_mut(&mut self) -> &mut Domain<TdataDomainStorage> {
        &mut self.domain
    }

    /// Unlocks `tdata` with `passcode`, loads all saved accounts, and starts a
    /// [`tg_mtproto::Client`] for each account seeded with its stored [`MtpAuthorization`]
    /// (main DC, user ID, and persistent auth keys per DC) so existing sessions resume
    /// without logging in again.
    pub fn unlock(&mut self, passcode: &[u8]) -> Result<()> {
        match self.domain.start(passcode)? {
            StartResult::Success => {}
            StartResult::IncorrectPasscode | StartResult::IncorrectPasscodeLegacy => {
                return Err(SessionError::IncorrectPasscode);
            }
        }

        let slot_defs: Vec<(i32, Environment)> = self
            .domain
            .accounts()
            .iter()
            .map(|a| (a.index, a.environment))
            .collect();

        self.sync_media_cache_keys(None);

        self.live_accounts.clear();
        for (index, env) in slot_defs {
            let live = self.create_live_account_from_storage(index, env)?;
            let effective_env = live.environment;
            let main_dc_id = live.main_dc_id();
            let user_id = live.user_id;
            let self_user = live.self_user.clone();
            self.live_accounts.insert(index, live);

            if let Some(core_acc) = self.domain.account_mut(index) {
                core_acc.environment = effective_env;
                core_acc.main_dc_id = main_dc_id;
                if let Some(uid) = user_id {
                    let is_test_mode = effective_env == Environment::Test;
                    let unique_id = uid.0
                        | if is_test_mode {
                            0x0100_0000_0000_0000
                        } else {
                            0
                        };
                    let premium = self_user.as_ref().is_some_and(|u| u.premium);
                    let username = self_user
                        .as_ref()
                        .and_then(|u| u.username.clone())
                        .or_else(|| core_acc.session.as_ref().map(|s| s.username.clone()))
                        .unwrap_or_default();
                    let unread_badge = core_acc.session.as_ref().map_or(0, |s| s.unread_badge);
                    let unread_badge_muted = core_acc
                        .session
                        .as_ref()
                        .is_some_and(|s| s.unread_badge_muted);
                    core_acc.create_session(AccountSession {
                        user_id: uid.0 as i64,
                        unique_id,
                        premium,
                        is_test_mode,
                        username,
                        unread_badge,
                        unread_badge_muted,
                    });
                }
            }
        }

        Ok(())
    }

    /// Returns summary metadata for all loaded account slots.
    #[must_use]
    pub fn account_slots(&self) -> Vec<AccountSlotInfo> {
        let active = self.domain.active_index();
        self.domain
            .accounts()
            .iter()
            .map(|acc| self.build_slot_info(acc, active))
            .collect()
    }

    /// Returns summary metadata for all loaded account slots sorted according to
    /// `CoreSettings::accounts_order` (`Main::Domain::orderedAccounts`).
    #[must_use]
    pub fn ordered_account_slots(&self) -> Vec<AccountSlotInfo> {
        let active = self.domain.active_index();
        self.domain
            .ordered_accounts(&self.core_settings.accounts_order)
            .into_iter()
            .map(|acc| self.build_slot_info(acc, active))
            .collect()
    }

    fn build_slot_info(
        &self,
        acc: &tg_core::domain::Account,
        active: Option<i32>,
    ) -> AccountSlotInfo {
        let live = self.live_accounts.get(&acc.index);
        let self_user = live.and_then(|l| l.self_user.as_ref());
        let username = self_user
            .and_then(|u| u.username.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| acc.session.as_ref().map(|s| s.username.clone()))
            .unwrap_or_default();
        let phone = self_user.and_then(|u| u.phone.clone()).unwrap_or_default();
        let display_name = self_user
            .map(tl_user_display_name)
            .filter(|s| !s.is_empty())
            .or_else(|| (!username.is_empty()).then(|| format!("@{username}")))
            .unwrap_or_else(|| match live.and_then(|l| l.user_id) {
                Some(uid) => format!("User {}", uid.0),
                None => format!("Account {}", acc.index + 1),
            });
        let premium = self_user
            .map(|u| u.premium)
            .unwrap_or_else(|| acc.session.as_ref().is_some_and(|s| s.premium));
        let (unread_badge, unread_badge_muted) = acc
            .session
            .as_ref()
            .map_or((0, false), |s| (s.unread_badge, s.unread_badge_muted));

        AccountSlotInfo {
            index: acc.index,
            data_name: self.account_data_name(acc.index),
            environment: acc.environment,
            main_dc_id: live.map_or(acc.main_dc_id, LiveAccount::main_dc_id),
            user_id: live.and_then(|l| l.user_id),
            display_name,
            username,
            phone,
            premium,
            unread_badge,
            unread_badge_muted,
            persistent_keys_count: live.map_or(0, |l| l.client.export_persistent_keys().len()),
            is_active: active == Some(acc.index),
        }
    }

    /// Computes the dynamic maximum account limit (`Main::Domain::maxAccounts`):
    /// `3` by default, `+1` for each signed-in Premium (or test-mode) account, capped at `6`
    /// (`PREMIUM_MAX_ACCOUNTS`).
    #[must_use]
    pub fn max_accounts(&self) -> usize {
        self.domain.max_accounts()
    }

    /// Returns `true` if another account slot can be added right now (either below
    /// [`Self::max_accounts`] or an unauthenticated slot already exists).
    #[must_use]
    pub fn can_add_account(&self) -> bool {
        self.domain.accounts().len() < self.domain.max_accounts()
            || self.domain.accounts().iter().any(|a| !a.session_exists())
    }

    /// Returns the number of currently authenticated accounts (`Main::Domain::accountsAuthedCount`).
    #[must_use]
    pub fn accounts_authed_count(&self) -> usize {
        self.domain.accounts_authed_count()
    }

    /// Returns the tdesktop per-account data string (`"data"`, `"data#2"`, `"data#3"`, ...).
    #[must_use]
    pub fn account_data_name(&self, index: i32) -> String {
        compose_data_string(self.domain.storage().data_name(), index)
    }

    /// Returns the 16-char uppercase hex `tdata` key/directory stem for account slot `index`
    /// (`ToFilePart(ComputeDataNameKey(ComposeDataString(data_name, index)))`).
    #[must_use]
    /// Returns the account's 16-hex-character `ToFilePart(compute_data_name_key)` folder name.
    pub fn account_key_file_part(&self, index: i32) -> String {
        let name = self.account_data_name(index);
        to_file_part(compute_data_name_key(&name))
    }

    /// Returns the unlocked domain 256-byte local key, if unlocked.
    #[must_use]
    pub fn local_key(&self) -> Option<&AuthKey> {
        self.domain.storage().local_key()
    }

    /// Derives the per-account [`StorageEncryptionKey`] for `index` from the unlocked local key.
    #[must_use]
    pub fn account_cache_key(&self, index: i32) -> Option<StorageEncryptionKey> {
        let local_key = self.local_key()?;
        let acc_data_name = self.account_data_name(index);
        let generation = self.account_cache_generation(index);
        Some(derive_media_cache_key(
            local_key,
            &cache_key_name(&acc_data_name, generation.as_deref()),
        ))
    }

    /// Returns the random media-cache generation stored for slot `index` at its last login
    /// (`None` for accounts signed in before generations existed).
    #[must_use]
    pub fn account_cache_generation(&self, index: i32) -> Option<String> {
        let prefs = self
            .domain
            .storage()
            .account_storage(index)?
            .read_prefs()
            .ok()?;
        read_cache_generation(&prefs)
    }

    /// Returns slot `index`'s media cache folder (`tdata/media_cache/<file_part>[-<generation>]`).
    #[must_use]
    pub fn account_media_cache_dir(&self, index: i32) -> PathBuf {
        let generation = self.account_cache_generation(index);
        self.domain
            .storage()
            .tdata_path()
            .join("media_cache")
            .join(account_media_cache_dir_name(
                self.domain.storage().data_name(),
                index,
                generation.as_deref(),
            ))
    }

    /// Starts a new media-cache generation for slot `index` (called once per login), so the
    /// cache folder and key differ from anything an earlier login in the slot left behind,
    /// and removes those leftovers.
    fn rotate_cache_generation(&mut self, index: i32, user_id: UserId) -> Result<()> {
        let generation = random_hex(8);
        let Some(acc_storage) = self.domain.storage_mut().account_storage_mut(index) else {
            return Err(SessionError::AccountNotFound(index));
        };
        let mut prefs = acc_storage.read_prefs().unwrap_or_default();
        prefs.insert(
            CACHE_GENERATION_PREFS_KEY.to_vec(),
            generation.as_bytes().to_vec(),
        );
        acc_storage.write_prefs(&prefs)?;

        let tdata = self.domain.storage().tdata_path().to_path_buf();
        let data_name = self.domain.storage().data_name().to_owned();
        let acc_data_name = compose_data_string(&data_name, index);
        register_cache_generation(&tdata, &acc_data_name, Some(&generation));
        let file_part = account_data_key_file_part(&data_name, index);
        let current = account_media_cache_dir_name(&data_name, index, Some(&generation));
        let mut leftovers = stale_media_cache_dirs(&tdata, &file_part, &current);
        leftovers.extend([
            tdata.join("media_cache").join(format!("account_{index}")),
            tdata.join("webview_profiles").join(&file_part),
            tdata.join("bot_storage").join(&file_part),
            tdata.join("bot_storage").join(format!("account_{index}")),
            tdata.join(format!("wallet_{file_part}")),
            tdata.join(&file_part).join("wallet"),
        ]);
        for leftover in &leftovers {
            unregister_cache_key(leftover);
            match remove_via_tombstone(leftover) {
                Some(busy) if busy != *leftover => record_pending_cleanup(&tdata, &busy),
                Some(busy) => warn!(
                    index,
                    path = %busy.display(),
                    "previous login's account data is busy and stays in place"
                ),
                None => {}
            }
        }
        // The new login owns every path of this slot now: queued retries for them must not run.
        let mut owned = leftovers;
        owned.extend([
            tdata.join(&file_part),
            tdata.join("media_cache").join(&current),
            tdata.join(format!("user_{acc_data_name}")),
            tdata.join(format!("temp_{acc_data_name}")),
            tdata
                .join("bot_storage")
                .join(format!("user_{}", user_id.0)),
        ]);
        drop_live_pending_cleanups(&tdata, &owned);
        if let Some(local_key) = self.domain.storage().local_key() {
            let key = derive_media_cache_key(
                local_key,
                &cache_key_name(&acc_data_name, Some(&generation)),
            );
            register_cache_key(&tdata.join("media_cache").join(current), key);
        }
        Ok(())
    }

    /// Synchronizes the in-memory domain/cache key registries and re-keys (or purges unreadable
    /// entries in) every account's on-disk `tdata/media_cache/<key_part>` directory.
    ///
    /// Pass `old_local_key = Some(prev)` when the domain local key may have rotated so
    /// `file_locations` are re-encrypted under the new derived key while one-way-keyed blobs are
    /// purged cleanly.
    pub fn sync_media_cache_keys(&self, old_local_key: Option<&AuthKey>) {
        let Some(local_key) = self.domain.storage().local_key().cloned() else {
            return;
        };
        let tdata = self.domain.storage().tdata_path();
        let data_name = self.domain.storage().data_name();
        register_unlocked_domain_key(tdata, data_name, local_key.clone());
        for acc in self.domain.accounts() {
            let acc_data_name = compose_data_string(data_name, acc.index);
            let generation = self.account_cache_generation(acc.index);
            register_cache_generation(tdata, &acc_data_name, generation.as_deref());
            let cache_dir = tdata.join("media_cache").join(account_media_cache_dir_name(
                data_name,
                acc.index,
                generation.as_deref(),
            ));
            let key_name = cache_key_name(&acc_data_name, generation.as_deref());
            let new_enc_key = derive_media_cache_key(&local_key, &key_name);
            let old_enc_key = old_local_key.map(|old| derive_media_cache_key(old, &key_name));
            if let Err(err) =
                rekey_or_purge_cache_dir(&cache_dir, old_enc_key.as_ref(), new_enc_key)
            {
                warn!(
                    index = acc.index,
                    path = %cache_dir.display(),
                    error = %err,
                    "failed to synchronize account media cache encryption key"
                );
            }
        }
    }

    /// Retries any pending account file cleanups queued under `tdata/.pending_cleanup`.
    pub fn retry_pending_cleanups(&self) -> usize {
        process_pending_cleanups(self.domain.storage().tdata_path())
    }

    /// Persists all loaded account slots to `tdata`.
    pub fn save_all_accounts(&mut self) -> Result<()> {
        let indices: Vec<i32> = self.live_accounts.keys().copied().collect();
        for idx in indices {
            let _ = self.save_account(idx)?;
        }
        Ok(())
    }

    /// Updates cached profile metadata (name, username, phone, premium flag) for account slot
    /// `index` and persists `self_serialized` into `AccountMap` so restarts preserve Premium
    /// account limits and display names immediately.
    pub fn update_account_user(&mut self, index: i32, user: &tl_types::User) -> Result<()> {
        let live = self
            .live_accounts
            .get_mut(&index)
            .ok_or(SessionError::AccountNotFound(index))?;
        live.self_user = Some(user.clone());
        let is_test_mode = live.environment == Environment::Test;

        if let Some(acc_storage) = self.domain.storage_mut().account_storage_mut(index) {
            let serialized_peer = tl_user_to_serialized_peer(user);
            acc_storage.map_mut().self_serialized = serialize_self_user(&serialized_peer, "");
            acc_storage.write_map()?;
        }

        if let Some(core_acc) = self.domain.account_mut(index)
            && let Some(session) = core_acc.session.as_mut()
        {
            session.premium = user.premium;
            session.is_test_mode = is_test_mode;
            session.username = user.username.clone().unwrap_or_default();
        }
        Ok(())
    }

    /// Synchronizes the cached Telegram Premium flag for account slot `index` (e.g. when live
    /// `users.getUsers` updates `User::premium` after startup) and persists it into `AccountMap`
    /// so `max_accounts()` immediately reflects the new cap and survives restarts.
    pub fn set_account_premium(&mut self, index: i32, premium: bool) -> Result<()> {
        let updated_user = {
            let live = self
                .live_accounts
                .get_mut(&index)
                .ok_or(SessionError::AccountNotFound(index))?;
            if let Some(user) = live.self_user.as_mut() {
                if user.premium == premium {
                    None
                } else {
                    user.premium = premium;
                    Some(user.clone())
                }
            } else {
                None
            }
        };

        if let Some(user) = updated_user
            && let Some(acc_storage) = self.domain.storage_mut().account_storage_mut(index)
        {
            let serialized_peer = tl_user_to_serialized_peer(&user);
            acc_storage.map_mut().self_serialized = serialize_self_user(&serialized_peer, "");
            acc_storage.write_map()?;
        }

        if let Some(core_acc) = self.domain.account_mut(index)
            && let Some(session) = core_acc.session.as_mut()
        {
            session.premium = premium;
        }
        Ok(())
    }

    /// Updates the unread badge count and muted state tracked on `Domain` for slot `index`.
    pub fn update_account_unread(
        &mut self,
        index: i32,
        unread_badge: i32,
        unread_badge_muted: bool,
    ) {
        if let Some(core_acc) = self.domain.account_mut(index)
            && let Some(session) = core_acc.session.as_mut()
        {
            session.unread_badge = unread_badge;
            session.unread_badge_muted = unread_badge_muted;
        }
    }

    /// Returns `(total_unread, all_muted)` summed across all authenticated accounts
    /// (`Main::Domain::unreadBadge` / `unreadBadgeMuted`).
    #[must_use]
    pub fn total_unread_badge(&self) -> (i32, bool) {
        self.domain.unread_badge()
    }

    /// Cancels an in-progress "Add Account" flow by switching back to `return_to_index`,
    /// which automatically prunes any redundant unauthenticated slot and wipes its `tdata`.
    pub fn cancel_pending_account(&mut self, return_to_index: i32) -> Result<()> {
        self.switch_account(return_to_index)
    }

    /// Returns the active account slot index, if unlocked.
    #[must_use]
    pub fn active_index(&self) -> Option<i32> {
        self.domain.active_index()
    }

    /// Borrows the currently active [`LiveAccount`].
    #[must_use]
    pub fn active_account(&self) -> Option<&LiveAccount> {
        let idx = self.domain.active_index()?;
        self.live_accounts.get(&idx)
    }

    /// Mutably borrows the currently active [`LiveAccount`].
    pub fn active_account_mut(&mut self) -> Option<&mut LiveAccount> {
        let idx = self.domain.active_index()?;
        self.live_accounts.get_mut(&idx)
    }

    /// Borrows a [`LiveAccount`] by slot `index`.
    #[must_use]
    pub fn account(&self, index: i32) -> Option<&LiveAccount> {
        self.live_accounts.get(&index)
    }

    /// Mutably borrows a [`LiveAccount`] by slot `index`.
    pub fn account_mut(&mut self, index: i32) -> Option<&mut LiveAccount> {
        self.live_accounts.get_mut(&index)
    }

    /// Persists the account's current [`MtpAuthorization`] (user ID, main DC, all persistent
    /// DC auth keys created by the client, and any remaining `keys_to_destroy`) plus serialized
    /// `MTP::Config` to `tg-storage`, matching `Main::Account::serializeMtpAuthorization` +
    /// `Storage::Account::writeMtpData`.
    pub fn save_account(&mut self, index: i32) -> Result<MtpAuthorization> {
        let live = self
            .live_accounts
            .get(&index)
            .ok_or(SessionError::AccountNotFound(index))?;
        let auth = live.snapshot_mtp_authorization();
        let serialized_cfg = live.client.serialize_config();

        self.domain.storage().write_account_mtp_auth(index, &auth)?;
        if let Some(acc_storage) = self.domain.storage().account_storage(index) {
            acc_storage.write_mtp_config(&serialized_cfg)?;
        }
        if let Some(core_acc) = self.domain.account_mut(index) {
            core_acc.main_dc_id = auth.main_dc_id;
        }
        Ok(auth)
    }

    /// Loads the persisted [`PersistedUpdatesState`] for account slot `index` from
    /// encrypted `tdata` account preferences, returning `Ok(None)` if not yet saved.
    pub fn load_updates_state(&self, index: i32) -> Result<Option<PersistedUpdatesState>> {
        let Some(acc_storage) = self.domain.storage().account_storage(index) else {
            return Err(SessionError::AccountNotFound(index));
        };
        let prefs = acc_storage.read_prefs()?;
        Ok(prefs
            .get(UPDATES_STATE_PREFS_KEY)
            .and_then(|bytes| PersistedUpdatesState::from_bytes(bytes)))
    }

    /// Saves [`PersistedUpdatesState`] for account slot `index` into encrypted `tdata`
    /// account preferences so the next launch resumes from saved `(pts, qts, date, seq)`.
    pub fn save_updates_state(&mut self, index: i32, state: &PersistedUpdatesState) -> Result<()> {
        let Some(acc_storage) = self.domain.storage_mut().account_storage_mut(index) else {
            return Err(SessionError::AccountNotFound(index));
        };
        let mut prefs = acc_storage.read_prefs()?;
        prefs.insert(UPDATES_STATE_PREFS_KEY.to_vec(), state.to_bytes());
        acc_storage.write_prefs(&prefs)?;
        Ok(())
    }

    /// Finalizes a successful login for account slot `index` after [`LoginStateMachine`]
    /// reaches [`LoginStep::Authorized`], writing the new [`MtpAuthorization`] (including all
    /// DC keys created by `tg-mtproto`), `MTP::Config`, and `self_serialized` user record to
    /// `tdata` and attaching the authenticated [`AccountSession`] on `Domain`.
    pub fn complete_login(
        &mut self,
        index: i32,
        user: &tl_types::User,
    ) -> Result<MtpAuthorization> {
        let user_id = UserId::from_tl(user.id);
        let is_test_mode = {
            let live = self
                .live_accounts
                .get_mut(&index)
                .ok_or(SessionError::AccountNotFound(index))?;
            live.user_id = Some(user_id);
            live.self_user = Some(user.clone());
            live.environment == Environment::Test
        };

        // Persist `self_serialized` into `AccountMap` so restarts know username/phone immediately.
        if let Some(acc_storage) = self.domain.storage_mut().account_storage_mut(index) {
            let serialized_peer = tl_user_to_serialized_peer(user);
            acc_storage.map_mut().self_serialized = serialize_self_user(&serialized_peer, "");
            acc_storage.write_map()?;
        }
        self.rotate_cache_generation(index, user_id)?;

        let auth = self.save_account(index)?;

        if let Some(core_acc) = self.domain.account_mut(index) {
            let unique_id = user_id.0
                | if is_test_mode {
                    0x0100_0000_0000_0000
                } else {
                    0
                };
            core_acc.main_dc_id = auth.main_dc_id;
            core_acc.create_session(AccountSession {
                user_id: user.id,
                unique_id,
                premium: user.premium,
                is_test_mode,
                username: user.username.clone().unwrap_or_default(),
                unread_badge: 0,
                unread_badge_muted: false,
            });
        }

        info!(
            index,
            user_id = auth.user_id,
            main_dc_id = auth.main_dc_id,
            keys = auth.keys.len(),
            "saved MTP authorization after login"
        );
        Ok(auth)
    }

    /// Destroys any pending `keys_to_destroy` loaded from storage for slot `index` and updates
    /// persisted `MtpAuthorization` with any remaining keys.
    pub async fn destroy_pending_keys(&mut self, index: i32) -> Result<Option<KeysDestroyResult>> {
        let (client, env, batch, main_dc_id) = {
            let live = self
                .live_accounts
                .get_mut(&index)
                .ok_or(SessionError::AccountNotFound(index))?;
            if live.keys_to_destroy.is_empty() {
                return Ok(None);
            }
            let batch = std::mem::take(&mut live.keys_to_destroy);
            (
                live.client.clone(),
                live.environment,
                batch,
                live.client.main_dc_id(),
            )
        };

        let result = self
            .run_keys_destroyer(&client, env, main_dc_id, batch)
            .await;

        if let Some(live) = self.live_accounts.get_mut(&index) {
            live.keys_to_destroy = result.remaining.clone();
        }
        let _ = self.save_account(index)?;
        Ok(Some(result))
    }

    /// Logs out account slot `index` (`Main::Account::logOut` + `loggedOut` + `KeysDestroyer`):
    /// 1. Invokes `auth.logOut` on the main DC and all non-CDN guest DCs (bounded by
    ///    `WorkingDirOpenOptions::logout_timeout` so unreachable networks never hang logout).
    /// 2. Destroys all persistent auth keys on the server via [`tg_mtproto::KeysDestroyer`]
    ///    (bounded per key by `WorkingDirOpenOptions::destroy_keys_timeout`).
    /// 3. Shuts down MTProto sessions and wipes the account's `tdata` storage files.
    /// 4. If other accounts exist, switches to another account, hands any remaining unconfirmed
    ///    keys to the active account for retry, and removes this slot; if this was the sole
    ///    account, resets slot `index` to a fresh unauthenticated client and removes any local
    ///    passcode per tdesktop's `passcodeRemovalAuthorized` rule.
    pub async fn logout_account(&mut self, index: i32) -> Result<KeysDestroyResult> {
        let (client, env, existing_to_destroy, main_dc_id, logged_out_user_id) = {
            let live = self
                .live_accounts
                .get(&index)
                .ok_or(SessionError::AccountNotFound(index))?;
            (
                live.client.clone(),
                live.environment,
                live.keys_to_destroy.clone(),
                live.client.main_dc_id(),
                live.user_id,
            )
        };

        let active_keys = client.export_persistent_keys();

        // Step 1: auth.logOut on main + guest DCs only when persistent keys exist
        // (`Main::Account::logOut` in `main/main_account.cpp:531-542`,
        // `MTP::Instance::Private::logoutGuestDcs` in `mtproto/mtp_instance.cpp:756-779`).
        // Skipping `client.logout()` when zero keys exist avoids dialing a DC to mint and leak
        // a brand-new unauthorized auth key during cleanup of an unauthenticated slot.
        if !active_keys.is_empty()
            && tokio::time::timeout(self.options.logout_timeout, client.logout())
                .await
                .is_err()
        {
            warn!(
                index,
                timeout_ms = self.options.logout_timeout.as_millis(),
                "auth.logOut timed out; proceeding with KeysDestroyer and local cleanup"
            );
        }

        // Collect all persistent keys (both pre-existing and any created before shutdown),
        // deduplicated by `(dc_id, key_id)`, so no persistent key is leaked on the server.
        let mut all_keys = active_keys;
        for (dc_id, key) in client.export_persistent_keys() {
            let key_id = key.key_id_u64();
            if !all_keys
                .iter()
                .any(|(d, k)| *d == dc_id && k.key_id_u64() == key_id)
            {
                all_keys.push((dc_id, key));
            }
        }
        for (dc_id, key) in existing_to_destroy {
            let key_id = key.key_id_u64();
            if !all_keys
                .iter()
                .any(|(d, k)| *d == dc_id && k.key_id_u64() == key_id)
            {
                all_keys.push((dc_id, key));
            }
        }

        // Step 2: stop all active MTProto sessions before running KeysDestroyer on the keys.
        client.shutdown();

        // Step 3: destroy persistent keys on Telegram servers via KeysDestroyer.
        let destroy_result = if all_keys.is_empty() {
            KeysDestroyResult {
                outcomes: Vec::new(),
                remaining: Vec::new(),
            }
        } else {
            self.run_keys_destroyer(&client, env, main_dc_id, all_keys)
                .await
        };

        // Step 4: update Domain and clean up on-disk tdata for this account.
        if let Some(core_acc) = self.domain.account_mut(index) {
            core_acc.log_out();
        }

        let remaining_entries: Vec<MtpAuthKeyEntry> = destroy_result
            .remaining
            .iter()
            .map(|(dc_id, key)| MtpAuthKeyEntry {
                dc_id: *dc_id,
                key: key.clone(),
            })
            .collect();

        if self.domain.accounts().len() > 1 {
            // Multi-account: remove this slot completely, wipe its cache/webview/wallet/temp
            // folders, and drop its live client.
            self.live_accounts.remove(&index);
            let _ = self.domain.remove(index)?;
            self.wipe_account_files_for(index, logged_out_user_id, false);

            // Hand any unconfirmed keys to the surviving active account so they are retried later.
            if !destroy_result.remaining.is_empty()
                && let Some(active_idx) = self.domain.active_index()
            {
                if let Some(active_live) = self.live_accounts.get_mut(&active_idx) {
                    active_live
                        .keys_to_destroy
                        .extend(destroy_result.remaining.iter().cloned());
                }
                let _ = self.save_account(active_idx);
            }
        } else {
            // Last remaining account: wipe account files (including media cache, WebView2
            // profiles, wallet folder, and temp copies), reopen empty AccountStorage, and
            // create a fresh unauthenticated client with no old auth keys.
            self.wipe_account_files_for(index, logged_out_user_id, true);
            self.domain
                .storage_mut()
                .start_added(index, env)
                .map_err(|e| SessionError::Domain(tg_core::DomainError::Storage(e)))?;

            if !remaining_entries.is_empty() {
                let residual = MtpAuthorization {
                    user_id: 0,
                    main_dc_id,
                    keys: Vec::new(),
                    keys_to_destroy: remaining_entries,
                };
                self.domain
                    .storage()
                    .write_account_mtp_auth(index, &residual)?;
            }

            let _ = self.domain.remove_redundant_accounts()?;
            let fresh = self.create_live_account_from_storage(index, env)?;
            self.live_accounts.insert(index, fresh);
            self.sync_media_cache_keys(None);
        }

        info!(
            index,
            remaining_keys = destroy_result.remaining.len(),
            "account logged out and storage cleaned up"
        );
        Ok(destroy_result)
    }

    /// Adds and activates a new account slot (`Main::Domain::addActivated`), returning its index.
    pub fn add_account(&mut self, environment: Environment) -> Result<i32> {
        let index = self.domain.add_activated(environment)?;
        if !self.live_accounts.contains_key(&index) {
            let live = self.create_live_account_from_storage(index, environment)?;
            self.live_accounts.insert(index, live);
        }
        self.prune_removed_live_accounts();
        self.sync_media_cache_keys(None);
        Ok(index)
    }

    /// Switches the active account slot to `index` (`Main::Domain::activate`).
    ///
    /// Automatically cleans up any redundant unauthenticated account slots, shutting down
    /// their MTProto clients just like tdesktop.
    pub fn switch_account(&mut self, index: i32) -> Result<()> {
        self.domain.activate(index)?;
        self.prune_removed_live_accounts();
        Ok(())
    }

    /// Removes an account slot by `index`.
    ///
    /// If the account is authorized, runs full logout (`auth.logOut` + `KeysDestroyer`) first;
    /// otherwise removes its files and shuts down its client directly.
    pub async fn remove_account(&mut self, index: i32) -> Result<Option<KeysDestroyResult>> {
        if !self.live_accounts.contains_key(&index) {
            return Err(SessionError::AccountNotFound(index));
        }
        let is_authed = self
            .live_accounts
            .get(&index)
            .is_some_and(LiveAccount::is_authorized);
        if is_authed {
            let res = self.logout_account(index).await?;
            return Ok(Some(res));
        }

        // Unauthenticated account removal: fail if sole account (matches Domain::remove).
        let _ = self.domain.remove(index)?;
        if let Some(live) = self.live_accounts.remove(&index) {
            live.shutdown();
        }
        self.wipe_account_files_for(index, None, false);
        self.prune_removed_live_accounts();
        Ok(None)
    }

    /// Shuts down all live MTProto clients cleanly.
    pub fn shutdown(&mut self) {
        let tdata = self.domain.storage().tdata_path().to_path_buf();
        for live in self.live_accounts.values() {
            live.shutdown();
        }
        self.live_accounts.clear();
        self.domain.finish();
        cleanup_stale_temp_dirs(&tdata);
        process_pending_cleanups(&tdata);
        unregister_unlocked_domain_key(&tdata);
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    async fn run_keys_destroyer(
        &self,
        client: &MtprotoClient,
        environment: Environment,
        main_dc_id: DcId,
        keys: Vec<(DcId, AuthKey)>,
    ) -> KeysDestroyResult {
        let mut cfg = build_mtproto_client_config(
            &self.options.credentials,
            main_dc_id,
            environment,
            Some(&self.lang_code),
        );
        cfg.proxy = client.proxy();
        cfg.use_pfs = false;
        let factory: Arc<dyn TransportFactory> = match &self.options.transport_factory {
            Some(f) => Arc::clone(f),
            None => Arc::new(TgTransportFactory::from_config(&cfg)),
        };
        let destroyer =
            KeysDestroyer::with_transport(cfg, client.dc_options().clone(), factory, keys)
                .with_timeout(self.options.destroy_keys_timeout);
        destroyer.destroy_all().await
    }

    fn create_live_account_from_storage(
        &self,
        index: i32,
        environment: Environment,
    ) -> Result<LiveAccount> {
        let mtp_auth = self.domain.storage().read_account_mtp_auth(index)?;
        let stored_config = self
            .domain
            .storage()
            .account_storage(index)
            .and_then(|acc| acc.read_mtp_config().ok().flatten());

        let effective_env = stored_config
            .as_deref()
            .and_then(tg_mtproto::Config::from_serialized)
            .map(|cfg| mtproto_env_to_core(cfg.environment()))
            .unwrap_or(environment);

        let initial_dc = mtp_auth.as_ref().map_or(0, |a| a.main_dc_id);
        let mut client_cfg = build_mtproto_client_config(
            &self.options.credentials,
            initial_dc,
            effective_env,
            Some(&self.lang_code),
        );
        client_cfg.use_pfs = self.options.use_pfs;
        client_cfg.proxy = resolve_effective_proxy(&self.core_settings.proxy);
        client_cfg.use_ipv6 = self.core_settings.proxy.try_ipv6();

        let (client, _updates_rx) = match &self.options.transport_factory {
            Some(factory) => MtprotoClient::with_transport(client_cfg, Arc::clone(factory)),
            None => MtprotoClient::new(client_cfg),
        };

        if let Some(cfg_bytes) = stored_config
            && !client.load_serialized_config(&cfg_bytes)
        {
            debug!(
                index,
                "stored MTP::Config ignored (corrupt or env mismatch)"
            );
        }

        let mut user_id = None;
        let mut keys_to_destroy = Vec::new();
        if let Some(auth) = mtp_auth {
            if auth.main_dc_id > 0 {
                client.set_main_dc_id(auth.main_dc_id);
            }
            if !auth.keys.is_empty() {
                let pairs: Vec<(DcId, AuthKey)> = auth
                    .keys
                    .into_iter()
                    .map(|entry| (entry.dc_id, entry.key))
                    .collect();
                client.import_persistent_keys(pairs);
            }
            if auth.user_id != 0 {
                user_id = Some(UserId(auth.user_id));
            }
            keys_to_destroy = auth
                .keys_to_destroy
                .into_iter()
                .map(|entry| (entry.dc_id, entry.key))
                .collect();
        }

        let self_user = self
            .domain
            .storage()
            .account_storage(index)
            .and_then(|acc| {
                if acc.map().self_serialized.is_empty() {
                    return None;
                }
                let ver = acc.old_map_version().max(tg_storage::APP_VERSION);
                deserialize_self_user(ver, &acc.map().self_serialized)
                    .ok()
                    .and_then(|(peer, _)| serialized_peer_to_tl_user(&peer))
            })
            // `Storage::Account::readSelf` drops a record that is not the authorized user
            // (`storage_account.cpp:3363`).
            .filter(|user| user_id == Some(UserId::from_tl(user.id)));

        let rpc = MtprotoRpc::new(client.clone());
        Ok(LiveAccount {
            index,
            environment: effective_env,
            client,
            rpc,
            user_id,
            self_user,
            keys_to_destroy,
        })
    }

    fn wipe_account_files_for(&self, index: i32, user_id: Option<UserId>, is_last_account: bool) {
        let tdata = self.domain.storage().tdata_path();
        let data_name = self.domain.storage().data_name();
        let acc_data_name = compose_data_string(data_name, index);
        let key = compute_data_name_key(&acc_data_name);
        let file_part = to_file_part(key);
        clear_key(key, tdata);
        register_cache_generation(tdata, &acc_data_name, None);

        let keyed_media_cache = tdata.join("media_cache").join(&file_part);
        let legacy_slot_media_cache = tdata.join("media_cache").join(format!("account_{index}"));
        let generation_caches = stale_media_cache_dirs(tdata, &file_part, &file_part);
        unregister_cache_key(&keyed_media_cache);
        unregister_cache_key(&legacy_slot_media_cache);
        for dir in &generation_caches {
            unregister_cache_key(dir);
        }

        let base_dir = tdata.join(&file_part);
        let mut targets = generation_caches;
        targets.extend([
            base_dir.join("wallet"),
            base_dir.clone(),
            keyed_media_cache,
            legacy_slot_media_cache,
            tdata.join(format!("user_{acc_data_name}")),
            tdata.join(format!("temp_{acc_data_name}")),
            tdata.join("bot_storage").join(&file_part),
            tdata.join("bot_storage").join(format!("account_{index}")),
            tdata.join("webview_profiles").join(&file_part),
            tdata.join(format!("wallet_{file_part}")),
        ]);
        if let Some(uid) = user_id
            && uid.0 != 0
        {
            targets.push(tdata.join("bot_storage").join(format!("user_{}", uid.0)));
        }
        if is_last_account {
            targets.push(tdata.join("webview_profiles"));
        }

        for target in targets {
            if let Some(busy) = remove_via_tombstone(&target) {
                warn!(
                    index,
                    path = %busy.display(),
                    "account cleanup target busy during logout; recording pending cleanup"
                );
                record_pending_cleanup(tdata, &busy);
            }
        }
        // If the account folder could not be renamed away, at least drop its map so a new
        // account in this slot does not start from the old map, prefs or self user.
        if base_dir.exists() {
            clear_name("map", &base_dir);
        }
    }

    fn prune_removed_live_accounts(&mut self) {
        let valid: Vec<i32> = self.domain.accounts().iter().map(|a| a.index).collect();
        let removed: Vec<i32> = self
            .live_accounts
            .keys()
            .copied()
            .filter(|idx| !valid.contains(idx))
            .collect();
        for idx in removed {
            if let Some(live) = self.live_accounts.remove(&idx) {
                live.shutdown();
            }
        }
    }
}

impl Drop for AccountManager {
    fn drop(&mut self) {
        let tdata = self.domain.storage().tdata_path().to_path_buf();
        for live in self.live_accounts.values() {
            live.shutdown();
        }
        cleanup_stale_temp_dirs(&tdata);
        process_pending_cleanups(&tdata);
        unregister_unlocked_domain_key(&tdata);
    }
}

/// High-level ergonomic wrapper driving [`tg_api::LoginStateMachine`] over any [`RpcClient`]
/// (typically [`MtprotoRpc`]).
pub struct LoginFlow<C: RpcClient = MtprotoRpc> {
    rpc: C,
    machine: LoginStateMachine,
}

impl<C: RpcClient> LoginFlow<C> {
    /// Creates a new `LoginFlow` starting at `LoginStep::Start`.
    #[must_use]
    pub fn new(rpc: C, credentials: ApiCredentials) -> Self {
        let country = tg_platform::system_info::system_country();
        Self {
            rpc,
            machine: LoginStateMachine::new(credentials, country),
        }
    }

    /// Creates a new `LoginFlow` starting directly at `LoginStep::PhoneInput`.
    #[must_use]
    pub fn at_phone_input(rpc: C, credentials: ApiCredentials) -> Self {
        Self {
            rpc,
            machine: LoginStateMachine::at_phone_input(credentials),
        }
    }

    /// Borrows the underlying RPC client.
    #[must_use]
    pub fn rpc(&self) -> &C {
        &self.rpc
    }

    /// Borrows the underlying [`LoginStateMachine`].
    #[must_use]
    pub fn machine(&self) -> &LoginStateMachine {
        &self.machine
    }

    /// Returns the current [`LoginStep`].
    #[must_use]
    pub fn step(&self) -> &LoginStep {
        self.machine.step()
    }

    /// Fetches nearest DC and updates the client's main DC ID.
    pub async fn fetch_nearest_dc(&mut self) -> Result<tl_types::NearestDc> {
        Ok(self.machine.fetch_nearest_dc(&self.rpc).await?)
    }

    /// Submits a phone number (`auth.sendCode`).
    pub async fn submit_phone(&mut self, phone: &str) -> Result<&LoginStep> {
        Ok(self.machine.submit_phone(&self.rpc, phone).await?)
    }

    /// Submits a login email address when `LoginStep::EmailSetup` is active.
    pub async fn submit_email_setup(&mut self, email: &str) -> Result<&LoginStep> {
        Ok(self.machine.submit_email_setup(&self.rpc, email).await?)
    }

    /// Submits a login verification code (`auth.signIn` or `account.verifyEmail`).
    pub async fn submit_code(&mut self, code: &str) -> Result<&LoginStep> {
        Ok(self.machine.submit_code(&self.rpc, code).await?)
    }

    /// Resends the verification code via `auth.resendCode`.
    pub async fn resend_code(&mut self) -> Result<&LoginStep> {
        Ok(self.machine.resend_code(&self.rpc).await?)
    }

    /// Submits the 2FA cloud password using SRP (`auth.checkPassword`).
    pub async fn submit_password(&mut self, password: &str) -> Result<&LoginStep> {
        Ok(self
            .machine
            .submit_password(&self.rpc, password, Instant::now())
            .await?)
    }

    /// Completes new-account registration (`auth.signUp`).
    pub async fn submit_sign_up(
        &mut self,
        first_name: &str,
        last_name: &str,
    ) -> Result<&LoginStep> {
        Ok(self
            .machine
            .submit_sign_up(&self.rpc, first_name, last_name)
            .await?)
    }

    /// Starts QR code login (`auth.exportLoginToken`).
    pub async fn start_qr_login(
        &mut self,
        now_unix: TimeId,
        except_ids: Vec<i64>,
    ) -> Result<&LoginStep> {
        Ok(self
            .machine
            .start_qr_login(&self.rpc, now_unix, except_ids)
            .await?)
    }

    /// Refreshes the QR login token (`auth.exportLoginToken` / `auth.importLoginToken`).
    pub async fn refresh_qr_token(&mut self, now_unix: TimeId) -> Result<&LoginStep> {
        Ok(self.machine.refresh_qr_token(&self.rpc, now_unix).await?)
    }

    /// Feeds an incoming `Updates` object while waiting for QR login scan confirmation.
    pub async fn on_qr_updates(
        &mut self,
        updates: &tg_tl::enums::Updates,
        now_unix: TimeId,
    ) -> Result<bool> {
        Ok(self
            .machine
            .on_qr_updates(&self.rpc, updates, now_unix)
            .await?)
    }

    /// Returns the authorized TL `User` if login has reached [`LoginStep::Authorized`].
    #[must_use]
    pub fn authorized_user(&self) -> Option<&tl_types::User> {
        match self.machine.step() {
            LoginStep::Authorized { user, .. } => Some(user),
            _ => None,
        }
    }
}

fn tl_user_display_name(user: &tl_types::User) -> String {
    let first = user.first_name.as_deref().unwrap_or("").trim();
    let last = user.last_name.as_deref().unwrap_or("").trim();
    match (first.is_empty(), last.is_empty()) {
        (false, false) => format!("{first} {last}"),
        (false, true) => first.to_owned(),
        (true, false) => last.to_owned(),
        (true, true) => user
            .username
            .as_deref()
            .filter(|u| !u.is_empty())
            .map(|u| format!("@{u}"))
            .unwrap_or_default(),
    }
}

fn tl_user_to_serialized_peer(user: &tl_types::User) -> SerializedPeer {
    let mut flags = 0i32;
    if user.self_ {
        flags |= USER_DATA_FLAG_SELF;
    }
    if user.premium {
        flags |= USER_DATA_FLAG_PREMIUM;
    }
    SerializedPeer {
        id: StoragePeerId::from_user(user.id.max(0) as u64),
        userpic_photo_id: 0,
        userpic_location: Default::default(),
        userpic_has_video: false,
        kind: SerializedPeerKind::User(SerializedUser {
            first_name: user.first_name.clone().unwrap_or_default(),
            last_name: user.last_name.clone().unwrap_or_default(),
            phone: user.phone.clone().unwrap_or_default(),
            username: user.username.clone().unwrap_or_default(),
            access_hash: user.access_hash.unwrap_or(0) as u64,
            flags,
            bot_inline_placeholder: user.bot_inline_placeholder.clone().unwrap_or_default(),
            lastseen: 0,
            is_contact: user.contact,
            bot_info_version: user.bot_info_version.unwrap_or(-1),
            supports_guest_chat: false,
        }),
    }
}

fn serialized_peer_to_tl_user(peer: &SerializedPeer) -> Option<tl_types::User> {
    let SerializedPeerKind::User(u) = &peer.kind else {
        return None;
    };
    let bare_id = peer.id.bare_id() as i64;
    let is_premium = (u.flags & USER_DATA_FLAG_PREMIUM) != 0;
    Some(tl_types::User {
        self_: true,
        contact: u.is_contact,
        mutual_contact: false,
        deleted: false,
        bot: u.bot_info_version >= 0,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: is_premium,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id: bare_id,
        access_hash: Some(u.access_hash as i64),
        first_name: (!u.first_name.is_empty()).then(|| u.first_name.clone()),
        last_name: (!u.last_name.is_empty()).then(|| u.last_name.clone()),
        username: (!u.username.is_empty()).then(|| u.username.clone()),
        phone: (!u.phone.is_empty()).then(|| u.phone.clone()),
        photo: None,
        status: None,
        bot_info_version: (u.bot_info_version >= 0).then_some(u.bot_info_version),
        restriction_reason: None,
        bot_inline_placeholder: (!u.bot_inline_placeholder.is_empty())
            .then(|| u.bot_inline_placeholder.clone()),
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    })
}

#[cfg(test)]
mod passcode_and_settings_tests {
    use super::*;
    use tempfile::TempDir;
    use tg_core::settings::{InputSubmitSettings, WorkMode};
    use tg_storage::{AutoDownloadSource, AutoDownloadType, PasscodeKdfKind};

    fn fast_kdf() -> PasscodeKdf {
        PasscodeKdf {
            kind: PasscodeKdfKind::Argon2id,
            memory: 64,
            time: 1,
            parallel: 1,
        }
    }

    pub(super) fn test_creds() -> ApiCredentials {
        ApiCredentials {
            api_id: 12_345,
            api_hash: "0123456789abcdef0123456789abcdef".to_owned(),
        }
    }

    pub(super) fn synthetic_user() -> tg_tl::types::User {
        tg_tl::types::User {
            self_: true,
            contact: false,
            mutual_contact: false,
            deleted: false,
            bot: false,
            bot_chat_history: false,
            bot_nochats: false,
            verified: false,
            restricted: false,
            min: false,
            bot_inline_geo: false,
            support: false,
            scam: false,
            apply_min_photo: false,
            fake: false,
            bot_attach_menu: false,
            premium: false,
            attach_menu_enabled: false,
            bot_can_edit: false,
            close_friend: false,
            stories_hidden: false,
            stories_unavailable: false,
            contact_require_premium: false,
            bot_business: false,
            bot_has_main_app: false,
            bot_forum_view: false,
            bot_forum_can_manage_topics: false,
            bot_can_manage_bots: false,
            bot_guestchat: false,
            bot_guard: false,
            bot_forum_sidebar: false,
            id: 777_001,
            access_hash: Some(42),
            first_name: Some("Test".to_owned()),
            last_name: Some("Account".to_owned()),
            username: Some("test_account".to_owned()),
            phone: Some("9996620001".to_owned()),
            photo: None,
            status: None,
            bot_info_version: None,
            restriction_reason: None,
            bot_inline_placeholder: None,
            lang_code: Some("en".to_owned()),
            emoji_status: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            bot_active_users: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_community_id: None,
        }
    }

    #[test]
    fn account_manager_passcode_set_change_remove_reencrypts_tdata() {
        let dir = TempDir::new().expect("tempdir");
        let opts = WorkingDirOpenOptions::new(dir.path(), test_creds());
        let mut mgr = AccountManager::open_and_unlock(opts.clone(), b"").expect("open");
        // Sign in slot 0 so `Domain::start` does not auto-clear passcode on empty domain.
        mgr.complete_login(0, &synthetic_user())
            .expect("complete login");
        assert!(!mgr.has_local_passcode());
        assert!(!mgr.needs_passcode().expect("needs"));

        // 1. Set passcode -> re-encrypts key_datas and arms app lock.
        mgr.set_local_passcode_with_kdf(None, b"alpha-pass", fast_kdf())
            .expect("set passcode");
        assert!(mgr.has_local_passcode());
        assert!(mgr.app_lock_enabled());
        assert!(mgr.check_local_passcode(b"alpha-pass"));
        assert!(!mgr.check_local_passcode(b"wrong"));
        assert!(mgr.needs_passcode().expect("needs after set"));

        // Reopen without passcode must fail; reopen with alpha-pass succeeds.
        assert!(matches!(
            AccountManager::open_and_unlock(opts.clone(), b""),
            Err(SessionError::IncorrectPasscode)
        ));
        let mut reopened =
            AccountManager::open_and_unlock(opts.clone(), b"alpha-pass").expect("reopen alpha");
        assert!(reopened.has_local_passcode());

        // 2. Changing with wrong current passcode fails.
        assert!(matches!(
            reopened.set_local_passcode_with_kdf(Some(b"nope"), b"beta-pass", fast_kdf()),
            Err(SessionError::IncorrectPasscode)
        ));

        // 3. Changing with right current passcode succeeds and persists.
        reopened
            .set_local_passcode_with_kdf(Some(b"alpha-pass"), b"beta-pass", fast_kdf())
            .expect("change passcode");
        assert!(reopened.check_local_passcode(b"beta-pass"));
        assert!(!reopened.check_local_passcode(b"alpha-pass"));

        let mut reopened2 =
            AccountManager::open_and_unlock(opts.clone(), b"beta-pass").expect("reopen beta");
        reopened2.core_settings_mut().system_unlock_enabled = true;
        let core_clone = reopened2.core_settings().clone();
        reopened2.save_core_settings(&core_clone).expect("save");

        // 4. Removing passcode clears app lock and system_unlock_enabled.
        reopened2
            .remove_local_passcode_with_kdf(b"beta-pass", fast_kdf())
            .expect("remove passcode");
        assert!(!reopened2.has_local_passcode());
        assert!(!reopened2.core_settings().system_unlock_enabled);
        assert!(!reopened2.needs_passcode().expect("needs false"));

        let reopened_open = AccountManager::open_and_unlock(opts, b"").expect("reopen open");
        assert!(!reopened_open.has_local_passcode());
        assert!(!reopened_open.core_settings().system_unlock_enabled);
    }

    #[test]
    fn account_manager_persists_core_global_and_session_settings() {
        let dir = TempDir::new().expect("tempdir");
        let opts = WorkingDirOpenOptions::new(dir.path(), test_creds());
        {
            let mut mgr = AccountManager::open_and_unlock(opts.clone(), b"").expect("open");
            let mut core = mgr.core_settings().clone();
            core.desktop_notify = false;
            core.sound_notify = false;
            core.send_submit_way = InputSubmitSettings::CtrlEnter;
            core.spellchecker_enabled = false;
            core.work_mode = WorkMode::TrayOnly;
            core.auto_lock = 300;
            core.window_position.x = 120;
            core.window_position.y = 80;
            core.window_position.w = 1280;
            core.window_position.h = 840;
            let mut global = mgr.global_config().clone();
            global.auto_start = true;
            global.start_minimized = true;
            global.config_scale = 125;
            mgr.save_global_and_core_settings(&core, &global)
                .expect("save global+core");

            let idx = mgr.active_index().expect("active idx");
            let mut sess = StoredSessionSettings {
                archive_collapsed: true,
                phone_number_hidden: true,
                dialogs_filters_enabled: true,
                ..StoredSessionSettings::default()
            };
            sess.auto_download.set_bytes_limit(
                AutoDownloadSource::User,
                AutoDownloadType::Photo,
                16 * 1024 * 1024,
            );
            mgr.save_session_settings(idx, &sess)
                .expect("save session settings");
        }

        let reopened = AccountManager::open_and_unlock(opts, b"").expect("reopen");
        assert!(!reopened.core_settings().desktop_notify);
        assert!(!reopened.core_settings().sound_notify);
        assert_eq!(
            reopened.core_settings().send_submit_way,
            InputSubmitSettings::CtrlEnter
        );
        assert!(!reopened.core_settings().spellchecker_enabled);
        assert_eq!(reopened.core_settings().work_mode, WorkMode::TrayOnly);
        assert_eq!(reopened.core_settings().auto_lock, 300);
        assert_eq!(reopened.core_settings().window_position.w, 1280);
        assert!(reopened.global_config().auto_start);
        assert!(reopened.global_config().start_minimized);
        assert_eq!(reopened.global_config().config_scale, 125);

        let idx = reopened.active_index().expect("active idx");
        let loaded_sess = reopened
            .load_session_settings(idx)
            .expect("load session settings");
        assert!(loaded_sess.archive_collapsed);
        assert!(loaded_sess.phone_number_hidden);
        assert!(loaded_sess.dialogs_filters_enabled);
        assert_eq!(
            loaded_sess
                .auto_download
                .bytes_limit(AutoDownloadSource::User, AutoDownloadType::Photo),
            16 * 1024 * 1024
        );
    }

    #[tokio::test]
    async fn d2_logout_wipes_media_cache_webview_wallet_and_temp() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let opts = WorkingDirOpenOptions::new(tmp.path(), test_creds())
            .with_pfs(false)
            .with_logout_timeout(Duration::from_millis(50))
            .with_destroy_keys_timeout(Duration::from_millis(50));

        let mut mgr = AccountManager::open_and_unlock(opts, b"").expect("open");
        let slot0 = mgr.active_index().expect("slot 0");
        let uid0 = UserId(777_001);
        if let Some(live) = mgr.live_accounts.get_mut(&slot0) {
            live.user_id = Some(uid0);
        }

        let tdata = tmp.path().join("tdata");
        let key_part = mgr.account_key_file_part(slot0);
        let keyed_cache = tdata.join("media_cache").join(&key_part);
        let legacy_cache = tdata.join("media_cache").join(format!("account_{slot0}"));
        let webview_profile = tdata
            .join("bot_storage")
            .join(format!("user_{}", uid0.0))
            .join("webview")
            .join("bot_99");
        let wallet_dir = tdata.join(&key_part).join("wallet");
        let temp_dir = tdata.join("temp_data");
        let user_db_dir = tdata.join("user_data").join("wvbots");

        for d in [
            &keyed_cache,
            &legacy_cache,
            &webview_profile,
            &wallet_dir,
            &temp_dir,
            &user_db_dir,
        ] {
            fs::create_dir_all(d).expect("mkdir");
            fs::write(d.join("secret.dat"), b"sensitive-account-data").expect("seed file");
        }

        let _ = mgr.logout_account(slot0).await.expect("logout");

        assert!(
            !keyed_cache.exists(),
            "logout must remove data-keyed media cache folder"
        );
        assert!(
            !legacy_cache.exists(),
            "logout must remove legacy slot media cache folder"
        );
        assert!(
            !tdata
                .join("bot_storage")
                .join(format!("user_{}", uid0.0))
                .exists(),
            "logout must remove account WebView2 profile & bot storage folder"
        );
        assert!(
            !wallet_dir.exists(),
            "logout must remove account wallet folder"
        );
        assert!(!temp_dir.exists(), "logout must remove account temp folder");
        assert!(
            !tdata.join("user_data").exists(),
            "logout must remove account user_data database folder"
        );

        // Multi-account isolation: adding slot 1 and logging out slot 1 wipes slot 1's cache
        // while keeping slot 0's cache intact.
        let slot0_cache = tdata.join("media_cache").join(&key_part);
        fs::create_dir_all(&slot0_cache).expect("recreate slot0 cache");
        fs::write(slot0_cache.join("keep.tdef"), b"slot0").expect("seed slot0");

        let slot1 = mgr
            .add_account(Environment::Production)
            .expect("add slot 1");
        let key_part1 = mgr.account_key_file_part(slot1);
        let slot1_cache = tdata.join("media_cache").join(&key_part1);
        fs::create_dir_all(&slot1_cache).expect("mkdir slot1 cache");
        fs::write(slot1_cache.join("wipe.tdef"), b"slot1").expect("seed slot1");

        let _ = mgr.logout_account(slot1).await.expect("logout slot 1");
        assert!(
            !slot1_cache.exists(),
            "multi-account logout must remove logged-out slot's media cache folder"
        );
        assert!(
            slot0_cache.join("keep.tdef").is_file(),
            "logging out slot 1 must not delete slot 0's media cache"
        );
    }

    #[tokio::test]
    async fn d2_busy_file_records_pending_cleanup_and_retries_later() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let opts = WorkingDirOpenOptions::new(tmp.path(), test_creds())
            .with_pfs(false)
            .with_logout_timeout(Duration::from_millis(50))
            .with_destroy_keys_timeout(Duration::from_millis(50));

        let mut mgr = AccountManager::open_and_unlock(opts.clone(), b"").expect("open");
        let slot0 = mgr.active_index().expect("slot 0");
        let tdata = tmp.path().join("tdata");
        let key_part = mgr.account_key_file_part(slot0);
        let busy_cache = tdata.join("media_cache").join(&key_part);
        fs::create_dir_all(&busy_cache).expect("mkdir");
        let locked_file = busy_cache.join("locked.bin");
        let unlocked_file = busy_cache.join("unlocked.bin");
        fs::write(&locked_file, b"busy").expect("write locked");
        fs::write(&unlocked_file, b"free").expect("write unlocked");

        // Hold an exclusive Windows file handle (share_mode = 0) so remove_dir_all fails during logout.
        #[cfg(windows)]
        let lock_guard = {
            use std::os::windows::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(&locked_file)
                .expect("open exclusively")
        };

        let _ = mgr.logout_account(slot0).await.expect("logout");

        #[cfg(windows)]
        {
            assert!(
                !unlocked_file.exists(),
                "unlocked files in a busy folder should still be purged immediately"
            );
            assert!(
                tdata.join(PENDING_CLEANUP_FILENAME).is_file(),
                "failed removal during logout must record a pending cleanup entry"
            );
            drop(lock_guard);

            let cleaned = mgr.retry_pending_cleanups();
            assert!(
                cleaned >= 1,
                "pending cleanup retry must remove unlocked folder"
            );
            assert!(!busy_cache.exists());
            assert!(!tdata.join(PENDING_CLEANUP_FILENAME).exists());
        }
    }

    #[test]
    fn passcode_change_and_key_rotation_sync_media_cache_keys() {
        use tg_storage::cache::media_cache::{
            LocalFileRecord, MediaCacheIndex, MediaCacheTag, lookup_cache_key_for_path,
        };

        let tmp = TempDir::new().expect("tempdir");
        let opts = WorkingDirOpenOptions::new(tmp.path(), test_creds());
        let mut mgr = AccountManager::open_and_unlock(opts, b"").expect("open");
        mgr.complete_login(0, &synthetic_user()).expect("login");

        let tdata = tmp.path().join("tdata");
        let cache_dir0 = mgr.account_media_cache_dir(0);
        let key0 = mgr.account_cache_key(0).expect("slot 0 cache key");
        let dl_path = tmp.path().join("downloaded_video.mp4");

        {
            let cache = MediaCacheIndex::open_with_key(&cache_dir0, key0.clone()).expect("open");
            cache
                .put_blob("photo:2:100:m", MediaCacheTag::Photo, b"cached-photo")
                .expect("put");
            cache
                .upsert_file_location(&LocalFileRecord {
                    key_high: 7,
                    key_low: 8,
                    path: dl_path.clone(),
                    size: 512,
                    modified_ms: 1_700_000_000_000,
                })
                .expect("upsert loc");
        }

        // Changing local passcode keeps cache key registry synchronized and existing cache readable.
        mgr.set_local_passcode_with_kdf(None, b"pass-one", fast_kdf())
            .expect("set pass");
        mgr.set_local_passcode_with_kdf(Some(b"pass-one"), b"pass-two", fast_kdf())
            .expect("change pass");
        let current_key0 = mgr.account_cache_key(0).expect("key0 after change");
        assert_eq!(
            lookup_cache_key_for_path(&cache_dir0)
                .as_ref()
                .map(|k| k.as_bytes()),
            Some(current_key0.as_bytes())
        );
        {
            let cache = MediaCacheIndex::open_with_key(&cache_dir0, current_key0.clone())
                .expect("reopen after passcode change");
            assert_eq!(
                cache.get_blob("photo:2:100:m").expect("get").as_deref(),
                Some(&b"cached-photo"[..])
            );
        }

        // Adding a second account registers its media cache key immediately.
        let slot1 = mgr
            .add_account(Environment::Production)
            .expect("add slot 1");
        let cache_dir1 = tdata
            .join("media_cache")
            .join(mgr.account_key_file_part(slot1));
        let key1 = mgr.account_cache_key(slot1).expect("key1");
        assert_eq!(
            lookup_cache_key_for_path(&cache_dir1)
                .as_ref()
                .map(|k| k.as_bytes()),
            Some(key1.as_bytes())
        );

        // When a cache directory was encrypted with a previous local key, `sync_media_cache_keys`
        // re-encrypts `file_locations` under the active derived key and purges unreadable blobs.
        let stale_local_key = AuthKey::new([0x9Au8; 256]);
        let generation0 = mgr.account_cache_generation(0);
        let stale_cache_key = derive_media_cache_key(
            &stale_local_key,
            &cache_key_name(&mgr.account_data_name(0), generation0.as_deref()),
        );
        let stale_blob_path;
        {
            let stale_cache =
                MediaCacheIndex::open_with_key(&cache_dir0, stale_cache_key).expect("open stale");
            let entry = stale_cache
                .put_blob("doc:2:777", MediaCacheTag::Document, b"stale-secret")
                .expect("put stale");
            stale_blob_path = cache_dir0.join(&entry.rel_path);
            stale_cache
                .upsert_file_location(&LocalFileRecord {
                    key_high: 9,
                    key_low: 10,
                    path: dl_path.clone(),
                    size: 1024,
                    modified_ms: 1_700_000_002_000,
                })
                .expect("upsert stale loc");
        }
        assert!(stale_blob_path.is_file());

        mgr.sync_media_cache_keys(Some(&stale_local_key));
        assert_eq!(
            lookup_cache_key_for_path(&cache_dir0)
                .as_ref()
                .map(|k| k.as_bytes()),
            Some(current_key0.as_bytes())
        );
        assert!(
            !stale_blob_path.exists(),
            "stale-key blob must be purged after key rotation"
        );
        let synced =
            MediaCacheIndex::open_with_key(&cache_dir0, current_key0).expect("open synced");
        assert_eq!(synced.stats().expect("stats").entry_count, 0);
        assert_eq!(
            synced
                .get_file_location(9, 10)
                .expect("loc")
                .map(|r| r.path),
            Some(dl_path)
        );
    }
}

#[cfg(test)]
#[path = "account_sec3_tests.rs"]
mod sec3_tests;
