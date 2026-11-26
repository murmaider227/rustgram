//! Persistent storage for Telegram updates state (`pts`, `qts`, `date`, `seq`, and
//! per-channel `pts`) so restarts resume via `updates.getDifference` instead of dropping
//! missed updates.
//!
//! Port origin: `api/api_updates.cpp` + `storage/storage_account.cpp` (`prefs` storage).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tg_data::PersistedUpdatesState;
use tokio::sync::watch;
use tracing::warn;

use crate::error::{Result, SessionError};

/// Key used inside `tg_storage::AccountStorage` encrypted preferences (`read_prefs`/`write_prefs`).
pub const UPDATES_STATE_PREFS_KEY: &[u8] = b"tgfork_updates_state_v1";

/// Persistence backend for [`PersistedUpdatesState`].
pub trait UpdatesStateStore: Send + Sync {
    /// Loads the last saved updates state, returning `Ok(None)` if none exists yet or
    /// if stored bytes are unreadable/corrupt.
    fn load(&self) -> Result<Option<PersistedUpdatesState>>;

    /// Atomically persists `state`.
    fn save(&self, state: &PersistedUpdatesState) -> Result<()>;
}

/// File-backed [`UpdatesStateStore`] using atomic temp-file write + rename.
#[derive(Debug, Clone)]
pub struct FileUpdatesStateStore {
    path: PathBuf,
}

impl FileUpdatesStateStore {
    /// Creates a file store writing to `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the underlying file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl UpdatesStateStore for FileUpdatesStateStore {
    fn load(&self) -> Result<Option<PersistedUpdatesState>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(PersistedUpdatesState::from_bytes(&bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(SessionError::Io(err)),
        }
    }

    fn save(&self, state: &PersistedUpdatesState) -> Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let bytes = state.to_bytes();
        let tmp_path = self.path.with_extension("tmp");
        {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        // `fs::rename` replaces an existing file on every platform (`MoveFileExW` with
        // `MOVEFILE_REPLACE_EXISTING` on Windows), so the old state stays readable until the
        // new one is in place.
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}

/// Writes [`PersistedUpdatesState`] snapshots on Tokio's blocking pool, one write at a time,
/// so a slow disk never stalls the session actor or the runtime's worker threads.
///
/// Snapshots supersede each other: when several are submitted while a write runs, only the
/// newest is written next, and an older one never overwrites a newer one.
pub struct UpdatesStatePersister {
    latest: watch::Sender<Option<(u64, PersistedUpdatesState)>>,
    written: watch::Receiver<(u64, bool)>,
    generation: u64,
    failed_seen: u64,
}

impl UpdatesStatePersister {
    /// Starts the worker task on the current Tokio runtime.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn spawn(store: Arc<dyn UpdatesStateStore>) -> Self {
        let (latest_tx, mut latest_rx) = watch::channel(None::<(u64, PersistedUpdatesState)>);
        let (written_tx, written_rx) = watch::channel((0_u64, true));
        tokio::spawn(async move {
            while latest_rx.changed().await.is_ok() {
                let Some((generation, state)) = latest_rx.borrow_and_update().clone() else {
                    continue;
                };
                let store = Arc::clone(&store);
                let ok = match tokio::task::spawn_blocking(move || store.save(&state)).await {
                    Ok(Ok(())) => true,
                    Ok(Err(err)) => {
                        warn!(error = %err, "failed to persist updates state");
                        false
                    }
                    Err(err) => {
                        warn!(error = %err, "updates state write task failed");
                        false
                    }
                };
                let _ = written_tx.send((generation, ok));
            }
        });
        Self {
            latest: latest_tx,
            written: written_rx,
            generation: 0,
            failed_seen: 0,
        }
    }

    /// Queues `state` for writing, replacing any snapshot not yet started.
    pub fn submit(&mut self, state: PersistedUpdatesState) {
        self.generation += 1;
        let _ = self.latest.send(Some((self.generation, state)));
    }

    /// Returns `true` once after a write failed, so the caller submits the state again.
    pub fn take_failed(&mut self) -> bool {
        let (generation, ok) = *self.written.borrow();
        if ok || generation <= self.failed_seen {
            return false;
        }
        self.failed_seen = generation;
        true
    }

    /// Number of snapshots submitted so far.
    #[must_use]
    pub const fn submitted(&self) -> u64 {
        self.generation
    }

    /// Waits until the last submitted snapshot was written (or failed), at most `timeout`.
    /// Returns `false` on timeout.
    pub async fn flush(&mut self, timeout: Duration) -> bool {
        let target = self.generation;
        if target == 0 {
            return true;
        }
        let wait = self
            .written
            .wait_for(|&(generation, _)| generation >= target);
        matches!(tokio::time::timeout(timeout, wait).await, Ok(Ok(_)))
    }
}

/// Thread-safe in-memory [`UpdatesStateStore`] useful for tests and ephemeral sessions.
#[derive(Debug, Default)]
pub struct InMemoryUpdatesStateStore {
    state: Mutex<Option<PersistedUpdatesState>>,
}

impl InMemoryUpdatesStateStore {
    /// Creates an empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an in-memory store pre-seeded with `state`.
    #[must_use]
    pub fn with_state(state: PersistedUpdatesState) -> Self {
        Self {
            state: Mutex::new(Some(state)),
        }
    }

    /// Returns a clone of the currently stored state, if any.
    #[must_use]
    pub fn snapshot(&self) -> Option<PersistedUpdatesState> {
        self.state.lock().clone()
    }
}

impl UpdatesStateStore for InMemoryUpdatesStateStore {
    fn load(&self) -> Result<Option<PersistedUpdatesState>> {
        Ok(self.state.lock().clone())
    }

    fn save(&self, state: &PersistedUpdatesState) -> Result<()> {
        *self.state.lock() = Some(state.clone());
        Ok(())
    }
}
