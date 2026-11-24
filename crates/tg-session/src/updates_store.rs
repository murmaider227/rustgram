//! Persistent storage for Telegram updates state (`pts`, `qts`, `date`, `seq`, and
//! per-channel `pts`) so restarts resume via `updates.getDifference` instead of dropping
//! missed updates.
//!
//! Port origin: `api/api_updates.cpp` + `storage/storage_account.cpp` (`prefs` storage).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use tg_data::PersistedUpdatesState;

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
        // On Windows `fs::rename` fails if destination exists; remove destination first if needed.
        if self.path.exists() {
            let _ = fs::remove_file(&self.path);
        }
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
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
