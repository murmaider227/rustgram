//! Asynchronous coalescing file writer and debounced per-account write timers.
//!
//! Ports:
//! - `Storage::details::WriteManager` and `AsyncWriteManager` from
//!   `Telegram/SourceFiles/storage/details/storage_file_utilities.cpp`
//! - `Storage::Account` delayed write timers (`writeMapDelayed`, `writeMapQueued`,
//!   `writePrefsDelayed`, `writeLocationsDelayed`, `writeSearchSuggestionsDelayed`)
//!   from `Telegram/SourceFiles/storage/storage_account.cpp`.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::account::{AccountStorage, SearchSuggestionsBlobs, StoredLocations};
use crate::error::{Result, StorageError};
use crate::tdf::write_tdf_file_atomic;

/// Debounce timeout for `writeMapDelayed`, `writePrefsDelayed`, and `writeLocationsDelayed`
/// (`kDelayedWriteTimeout = crl::time(1000)` in `storage_account.cpp:52`).
pub const DELAYED_WRITE_TIMEOUT: Duration = Duration::from_millis(1_000);

/// Throttle delay for `writeSearchSuggestionsDelayed`
/// (`kWriteSearchSuggestionsDelay = 5 * crl::time(1000)` in `storage_account.cpp:53`).
pub const WRITE_SEARCH_SUGGESTIONS_DELAY: Duration = Duration::from_millis(5_000);

/// Pending atomic TDF$ file write submitted to [`AsyncWriteManager`] (`WriteEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteEntry {
    /// Directory containing the TDF$ file.
    pub base_path: PathBuf,
    /// Base filename without the `'s'`/`'0'`/`'1'` suffix (e.g., `"map"`, `"settings"`, `"key_data"`).
    pub name: String,
    /// Fully encoded TDF$ file bytes (`"TDF$" || version || payload || md5`).
    pub full_bytes: Vec<u8>,
}

impl WriteEntry {
    /// Logical key identifying the target file (`base_path / name`) for coalescing.
    #[must_use]
    pub fn base_key(&self) -> PathBuf {
        self.base_path.join(&self.name)
    }
}

/// Pending file/folder removal submitted to [`AsyncWriteManager`] (`RemoveEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoveEntry {
    /// Individual files to remove.
    pub files: Vec<PathBuf>,
    /// Optional directory to remove recursively.
    pub folder: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WriteOperation {
    Write(WriteEntry),
    Remove(RemoveEntry),
}

/// Pure queue state implementing `WriteManager`'s removal-barrier coalescing semantics.
#[derive(Debug, Default)]
struct WriteManagerQueue {
    scheduled: VecDeque<WriteOperation>,
}

impl WriteManagerQueue {
    /// Index right after the last [`WriteOperation::Remove`] barrier (`WriteManager::segment`).
    fn segment_start(&self) -> usize {
        self.scheduled
            .iter()
            .rposition(|op| matches!(op, WriteOperation::Remove(_)))
            .map_or(0, |pos| pos + 1)
    }

    /// Find an already-scheduled [`WriteEntry`] with `base_key` AFTER the last removal barrier
    /// (`WriteManager::scheduled`).
    fn scheduled_pos(&self, base_key: &Path) -> Option<usize> {
        let start = self.segment_start();
        for (idx, op) in self.scheduled.iter().enumerate().skip(start) {
            if let WriteOperation::Write(entry) = op
                && entry.base_key() == base_key
            {
                return Some(idx);
            }
        }
        None
    }

    /// Enqueue a write, coalescing with any pending write for the same file after the last removal.
    fn write(&mut self, entry: WriteEntry) {
        let key = entry.base_key();
        if let Some(pos) = self.scheduled_pos(&key) {
            self.scheduled[pos] = WriteOperation::Write(entry);
        } else {
            self.scheduled.push_back(WriteOperation::Write(entry));
        }
    }

    /// Enqueue a removal barrier (`WriteManager::remove`).
    fn remove(&mut self, entry: RemoveEntry) {
        self.scheduled.push_back(WriteOperation::Remove(entry));
    }

    /// Prepare a synchronous write (`WriteManager::writeSync`):
    /// returns all operations up to and including the last removal barrier that must run first,
    /// and removes any pending coalesced write for the same file from the post-barrier tail.
    fn take_before_sync(&mut self, base_key: &Path) -> Vec<WriteOperation> {
        let count = self.segment_start();
        let mut prefix = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(op) = self.scheduled.pop_front() {
                prefix.push(op);
            }
        }
        if let Some(pos) = self.scheduled_pos(base_key) {
            self.scheduled.remove(pos);
        }
        prefix
    }

    fn pop_front(&mut self) -> Option<WriteOperation> {
        self.scheduled.pop_front()
    }

    fn drain_all(&mut self) -> Vec<WriteOperation> {
        self.scheduled.drain(..).collect()
    }
}

enum WriteCommand {
    Write(WriteEntry),
    WriteSync(WriteEntry, oneshot::Sender<Result<()>>),
    Remove(RemoveEntry),
    Sync(oneshot::Sender<()>),
    Stop(oneshot::Sender<()>),
}

/// Asynchronous background TDF$ file write manager (`Storage::details::AsyncWriteManager`).
///
/// Serializes disk writes on a background tokio task while coalescing repeated writes to the
/// same target file that occur after the last removal barrier.
#[derive(Debug, Clone)]
pub struct AsyncWriteManager {
    tx: mpsc::UnboundedSender<WriteCommand>,
}

impl AsyncWriteManager {
    /// Spawn a new background write manager task on the current Tokio runtime.
    #[must_use]
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(write_manager_loop(rx));
        Self { tx }
    }

    /// Queue an atomic TDF$ file write (`AsyncWriteManager::write`).
    pub fn write(&self, entry: WriteEntry) {
        let _ = self.tx.send(WriteCommand::Write(entry));
    }

    /// Perform a durable synchronous write respecting earlier removal barriers
    /// (`AsyncWriteManager::writeSync`).
    pub async fn write_sync(&self, entry: WriteEntry) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(WriteCommand::WriteSync(entry, reply_tx))
            .map_err(|_| StorageError::CorruptData("write manager stopped".into()))?;
        reply_rx
            .await
            .map_err(|_| StorageError::CorruptData("write manager dropped reply".into()))?
    }

    /// Queue a removal barrier (`AsyncWriteManager::remove`).
    pub fn remove(&self, entry: RemoveEntry) {
        let _ = self.tx.send(WriteCommand::Remove(entry));
    }

    /// Queue removal of individual files (`ScheduleRemoveFiles`).
    pub fn schedule_remove_files(&self, files: Vec<PathBuf>) {
        if !files.is_empty() {
            self.remove(RemoveEntry {
                files,
                folder: None,
            });
        }
    }

    /// Queue recursive removal of a folder (`ScheduleRemoveFolder`).
    pub fn schedule_remove_folder(&self, folder: PathBuf) {
        self.remove(RemoveEntry {
            files: Vec::new(),
            folder: Some(folder),
        });
    }

    /// Flush all queued writes and removals to disk and wait until complete (`AsyncWriteManager::sync`).
    pub async fn sync(&self) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send(WriteCommand::Sync(reply_tx)).is_ok() {
            let _ = reply_rx.await;
        }
    }

    /// Flush all queued operations and stop the background writer task (`AsyncWriteManager::stop`).
    pub async fn stop(self) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send(WriteCommand::Stop(reply_tx)).is_ok() {
            let _ = reply_rx.await;
        }
    }
}

async fn write_manager_loop(mut rx: mpsc::UnboundedReceiver<WriteCommand>) {
    let mut queue = WriteManagerQueue::default();

    while let Some(first_cmd) = rx.recv().await {
        let mut pending_cmds = VecDeque::new();
        pending_cmds.push_back(first_cmd);
        while let Ok(cmd) = rx.try_recv() {
            pending_cmds.push_back(cmd);
        }

        let mut stop_reply = None;

        while let Some(cmd) = pending_cmds.pop_front() {
            match cmd {
                WriteCommand::Write(entry) => {
                    queue.write(entry);
                }
                WriteCommand::Remove(entry) => {
                    queue.remove(entry);
                }
                WriteCommand::WriteSync(entry, reply) => {
                    let prefix = queue.take_before_sync(&entry.base_key());
                    let res = tokio::task::spawn_blocking(move || {
                        for op in prefix {
                            execute_operation(op);
                        }
                        write_tdf_file_atomic(&entry.base_path, &entry.name, &entry.full_bytes)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        Err(StorageError::CorruptData(format!(
                            "write task panicked: {e}"
                        )))
                    });
                    let _ = reply.send(res);
                }
                WriteCommand::Sync(reply) => {
                    let ops = queue.drain_all();
                    if !ops.is_empty() {
                        let _ = tokio::task::spawn_blocking(move || {
                            for op in ops {
                                execute_operation(op);
                            }
                        })
                        .await;
                    }
                    let _ = reply.send(());
                }
                WriteCommand::Stop(reply) => {
                    stop_reply = Some(reply);
                    break;
                }
            }
        }

        if let Some(reply) = stop_reply {
            let ops = queue.drain_all();
            if !ops.is_empty() {
                let _ = tokio::task::spawn_blocking(move || {
                    for op in ops {
                        execute_operation(op);
                    }
                })
                .await;
            }
            let _ = reply.send(());
            return;
        }

        while let Some(op) = queue.pop_front() {
            let _ = tokio::task::spawn_blocking(move || execute_operation(op)).await;
            // Drain any newly arrived commands between disk writes so subsequent updates coalesce.
            let mut saw_barrier = false;
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    WriteCommand::Write(entry) => queue.write(entry),
                    WriteCommand::Remove(entry) => queue.remove(entry),
                    other => {
                        pending_cmds.push_back(other);
                        saw_barrier = true;
                        break;
                    }
                }
            }
            if saw_barrier {
                while let Ok(cmd) = rx.try_recv() {
                    pending_cmds.push_back(cmd);
                }
                while let Some(cmd) = pending_cmds.pop_front() {
                    match cmd {
                        WriteCommand::Write(entry) => queue.write(entry),
                        WriteCommand::Remove(entry) => queue.remove(entry),
                        WriteCommand::WriteSync(entry, reply) => {
                            let prefix = queue.take_before_sync(&entry.base_key());
                            let res = tokio::task::spawn_blocking(move || {
                                for op in prefix {
                                    execute_operation(op);
                                }
                                write_tdf_file_atomic(
                                    &entry.base_path,
                                    &entry.name,
                                    &entry.full_bytes,
                                )
                            })
                            .await
                            .unwrap_or_else(|e| {
                                Err(StorageError::CorruptData(format!(
                                    "write task panicked: {e}"
                                )))
                            });
                            let _ = reply.send(res);
                        }
                        WriteCommand::Sync(reply) => {
                            let ops = queue.drain_all();
                            if !ops.is_empty() {
                                let _ = tokio::task::spawn_blocking(move || {
                                    for op in ops {
                                        execute_operation(op);
                                    }
                                })
                                .await;
                            }
                            let _ = reply.send(());
                        }
                        WriteCommand::Stop(reply) => {
                            let ops = queue.drain_all();
                            if !ops.is_empty() {
                                let _ = tokio::task::spawn_blocking(move || {
                                    for op in ops {
                                        execute_operation(op);
                                    }
                                })
                                .await;
                            }
                            let _ = reply.send(());
                            return;
                        }
                    }
                }
            }
        }
    }

    // Channel closed: drain any remaining operations before exiting.
    let ops = queue.drain_all();
    if !ops.is_empty() {
        let _ = tokio::task::spawn_blocking(move || {
            for op in ops {
                execute_operation(op);
            }
        })
        .await;
    }
}

fn execute_operation(op: WriteOperation) {
    match op {
        WriteOperation::Write(entry) => {
            let _ = write_tdf_file_atomic(&entry.base_path, &entry.name, &entry.full_bytes);
        }
        WriteOperation::Remove(entry) => {
            for file in &entry.files {
                let _ = fs::remove_file(file);
            }
            if let Some(folder) = &entry.folder {
                let _ = fs::remove_dir_all(folder);
            }
        }
    }
}

enum SchedulerCommand {
    WriteMapDelayed,
    WriteMapQueued,
    WritePrefsDelayed(BTreeMap<Vec<u8>, Vec<u8>>),
    WriteLocationsDelayed(StoredLocations),
    WriteSearchSuggestionsDelayed(SearchSuggestionsBlobs),
    Flush(oneshot::Sender<Result<()>>),
    Shutdown(oneshot::Sender<Result<()>>),
}

/// Async debounced background writer for an [`AccountStorage`]
/// (`Storage::Account`'s `_writeMapTimer`, `_writePrefsTimer`, `_writeLocationsTimer`,
/// and `_writeSearchSuggestionsTimer` backed by [`AsyncWriteManager`]).
#[derive(Debug, Clone)]
pub struct AccountWriteScheduler {
    tx: mpsc::UnboundedSender<SchedulerCommand>,
}

impl AccountWriteScheduler {
    /// Spawn a scheduler task using tdesktop's exact default delays:
    /// - `1000 ms` debounce for `map`, `prefs`, and `locations`
    /// - `5000 ms` leading-edge delay for `search_suggestions`
    #[must_use]
    pub fn spawn(account: Arc<Mutex<AccountStorage>>, writer: AsyncWriteManager) -> Self {
        Self::spawn_with_delays(
            account,
            writer,
            DELAYED_WRITE_TIMEOUT,
            WRITE_SEARCH_SUGGESTIONS_DELAY,
        )
    }

    /// Spawn a scheduler task with custom timer durations (useful for tests).
    #[must_use]
    pub fn spawn_with_delays(
        account: Arc<Mutex<AccountStorage>>,
        writer: AsyncWriteManager,
        delayed_write_timeout: Duration,
        search_suggestions_delay: Duration,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(scheduler_loop(
            account,
            writer,
            rx,
            delayed_write_timeout,
            search_suggestions_delay,
        ));
        Self { tx }
    }

    /// Mark `map` dirty and debounce its write by [`DELAYED_WRITE_TIMEOUT`] (`writeMapDelayed`).
    pub fn write_map_delayed(&self) {
        let _ = self.tx.send(SchedulerCommand::WriteMapDelayed);
    }

    /// Mark `map` dirty and schedule an immediate write on the next turn (`writeMapQueued`).
    pub fn write_map_queued(&self) {
        let _ = self.tx.send(SchedulerCommand::WriteMapQueued);
    }

    /// Schedule a debounced write of account preferences (`writePrefsDelayed`, 1000 ms).
    pub fn write_prefs_delayed(&self, prefs: BTreeMap<Vec<u8>, Vec<u8>>) {
        let _ = self.tx.send(SchedulerCommand::WritePrefsDelayed(prefs));
    }

    /// Schedule a debounced write of downloaded file locations (`writeLocationsDelayed`, 1000 ms).
    pub fn write_locations_delayed(&self, locations: StoredLocations) {
        let _ = self
            .tx
            .send(SchedulerCommand::WriteLocationsDelayed(locations));
    }

    /// Schedule a throttled write of search suggestions (`writeSearchSuggestionsDelayed`, 5000 ms).
    pub fn write_search_suggestions_delayed(&self, suggestions: SearchSuggestionsBlobs) {
        let _ = self
            .tx
            .send(SchedulerCommand::WriteSearchSuggestionsDelayed(suggestions));
    }

    /// Cancel active timers, flush all pending dirty writes in order, and await disk durability.
    pub async fn flush(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SchedulerCommand::Flush(reply_tx))
            .map_err(|_| StorageError::CorruptData("account write scheduler stopped".into()))?;
        reply_rx.await.map_err(|_| {
            StorageError::CorruptData("account write scheduler dropped reply".into())
        })?
    }

    /// Flush all pending dirty writes, stop the scheduler task, and await disk durability
    /// (matching `Storage::Account::~Account`).
    pub async fn shutdown(self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send(SchedulerCommand::Shutdown(reply_tx)).is_err() {
            return Ok(());
        }
        reply_rx.await.map_err(|_| {
            StorageError::CorruptData("account write scheduler dropped reply".into())
        })?
    }
}

struct SchedulerState {
    map_dirty: bool,
    map_deadline: Option<Instant>,
    pending_prefs: Option<BTreeMap<Vec<u8>, Vec<u8>>>,
    prefs_deadline: Option<Instant>,
    pending_locations: Option<StoredLocations>,
    locations_deadline: Option<Instant>,
    pending_search_suggestions: Option<SearchSuggestionsBlobs>,
    search_suggestions_deadline: Option<Instant>,
}

impl SchedulerState {
    fn new() -> Self {
        Self {
            map_dirty: false,
            map_deadline: None,
            pending_prefs: None,
            prefs_deadline: None,
            pending_locations: None,
            locations_deadline: None,
            pending_search_suggestions: None,
            search_suggestions_deadline: None,
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.map_deadline,
            self.prefs_deadline,
            self.locations_deadline,
            self.search_suggestions_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn flush_prefs(
        &mut self,
        account: &Arc<Mutex<AccountStorage>>,
        writer: &AsyncWriteManager,
        delayed_write_timeout: Duration,
    ) -> Result<()> {
        self.prefs_deadline = None;
        let Some(prefs) = self.pending_prefs.take() else {
            return Ok(());
        };
        let (entry, removed, map_changed) = {
            let mut guard = account
                .lock()
                .map_err(|_| StorageError::CorruptData("account lock poisoned".into()))?;
            guard.prepare_prefs_write(&prefs)?
        };
        if !removed.is_empty() {
            writer.schedule_remove_files(removed);
        }
        if let Some(entry) = entry {
            writer.write(entry);
            if map_changed {
                // Matches `writeMapQueued()` when a new prefs key is allocated.
                self.map_dirty = true;
                self.flush_map(account, writer)?;
            }
        } else if map_changed {
            // Matches `writeMapDelayed()` when prefs key is cleared.
            self.map_dirty = true;
            self.map_deadline = Some(Instant::now() + delayed_write_timeout);
        }
        Ok(())
    }

    fn flush_locations(
        &mut self,
        account: &Arc<Mutex<AccountStorage>>,
        writer: &AsyncWriteManager,
        delayed_write_timeout: Duration,
    ) -> Result<()> {
        self.locations_deadline = None;
        let Some(locations) = self.pending_locations.take() else {
            return Ok(());
        };
        let (entry, removed, map_changed) = {
            let mut guard = account
                .lock()
                .map_err(|_| StorageError::CorruptData("account lock poisoned".into()))?;
            guard.prepare_locations_write(&locations)?
        };
        if !removed.is_empty() {
            writer.schedule_remove_files(removed);
        }
        if let Some(entry) = entry {
            writer.write(entry);
            if map_changed {
                self.map_dirty = true;
                self.flush_map(account, writer)?;
            }
        } else if map_changed {
            self.map_dirty = true;
            self.map_deadline = Some(Instant::now() + delayed_write_timeout);
        }
        Ok(())
    }

    fn flush_search_suggestions(
        &mut self,
        account: &Arc<Mutex<AccountStorage>>,
        writer: &AsyncWriteManager,
        delayed_write_timeout: Duration,
    ) -> Result<()> {
        self.search_suggestions_deadline = None;
        let Some(suggestions) = self.pending_search_suggestions.take() else {
            return Ok(());
        };
        let (entry, removed, map_changed) = {
            let mut guard = account
                .lock()
                .map_err(|_| StorageError::CorruptData("account lock poisoned".into()))?;
            guard.prepare_search_suggestions_write(&suggestions)?
        };
        if !removed.is_empty() {
            writer.schedule_remove_files(removed);
        }
        if let Some(entry) = entry {
            writer.write(entry);
            if map_changed {
                self.map_dirty = true;
                self.flush_map(account, writer)?;
            }
        } else if map_changed {
            self.map_dirty = true;
            self.map_deadline = Some(Instant::now() + delayed_write_timeout);
        }
        Ok(())
    }

    fn flush_map(
        &mut self,
        account: &Arc<Mutex<AccountStorage>>,
        writer: &AsyncWriteManager,
    ) -> Result<()> {
        self.map_deadline = None;
        if !self.map_dirty {
            return Ok(());
        }
        self.map_dirty = false;
        let entry = {
            let guard = account
                .lock()
                .map_err(|_| StorageError::CorruptData("account lock poisoned".into()))?;
            guard.prepare_map_write_entry()?
        };
        writer.write(entry);
        Ok(())
    }

    fn flush_all(
        &mut self,
        account: &Arc<Mutex<AccountStorage>>,
        writer: &AsyncWriteManager,
        delayed_write_timeout: Duration,
    ) -> Result<()> {
        self.flush_prefs(account, writer, delayed_write_timeout)?;
        self.flush_locations(account, writer, delayed_write_timeout)?;
        self.flush_search_suggestions(account, writer, delayed_write_timeout)?;
        self.flush_map(account, writer)?;
        Ok(())
    }
}

async fn scheduler_loop(
    account: Arc<Mutex<AccountStorage>>,
    writer: AsyncWriteManager,
    mut rx: mpsc::UnboundedReceiver<SchedulerCommand>,
    delayed_write_timeout: Duration,
    search_suggestions_delay: Duration,
) {
    let mut state = SchedulerState::new();

    loop {
        let next_deadline = state.next_deadline();
        let maybe_cmd = if let Some(deadline) = next_deadline {
            tokio::select! {
                cmd = rx.recv() => Some(cmd),
                () = tokio::time::sleep_until(deadline) => None,
            }
        } else {
            Some(rx.recv().await)
        };

        match maybe_cmd {
            Some(Some(cmd)) => match cmd {
                SchedulerCommand::WriteMapDelayed => {
                    state.map_dirty = true;
                    state.map_deadline = Some(Instant::now() + delayed_write_timeout);
                }
                SchedulerCommand::WriteMapQueued => {
                    state.map_dirty = true;
                    let _ = state.flush_map(&account, &writer);
                }
                SchedulerCommand::WritePrefsDelayed(prefs) => {
                    state.pending_prefs = Some(prefs);
                    state.prefs_deadline = Some(Instant::now() + delayed_write_timeout);
                }
                SchedulerCommand::WriteLocationsDelayed(locations) => {
                    state.pending_locations = Some(locations);
                    state.locations_deadline = Some(Instant::now() + delayed_write_timeout);
                }
                SchedulerCommand::WriteSearchSuggestionsDelayed(suggestions) => {
                    state.pending_search_suggestions = Some(suggestions);
                    if state.search_suggestions_deadline.is_none() {
                        state.search_suggestions_deadline =
                            Some(Instant::now() + search_suggestions_delay);
                    }
                }
                SchedulerCommand::Flush(reply) => {
                    let res = state.flush_all(&account, &writer, delayed_write_timeout);
                    writer.sync().await;
                    let _ = reply.send(res);
                }
                SchedulerCommand::Shutdown(reply) => {
                    let res = state.flush_all(&account, &writer, delayed_write_timeout);
                    writer.sync().await;
                    let _ = reply.send(res);
                    return;
                }
            },
            Some(None) => {
                // Sender dropped: flush any dirty state before exiting (matches ~Account).
                let _ = state.flush_all(&account, &writer, delayed_write_timeout);
                writer.sync().await;
                return;
            }
            None => {
                let now = Instant::now();
                if state.prefs_deadline.is_some_and(|d| d <= now) {
                    let _ = state.flush_prefs(&account, &writer, delayed_write_timeout);
                }
                if state.locations_deadline.is_some_and(|d| d <= now) {
                    let _ = state.flush_locations(&account, &writer, delayed_write_timeout);
                }
                if state.search_suggestions_deadline.is_some_and(|d| d <= now) {
                    let _ =
                        state.flush_search_suggestions(&account, &writer, delayed_write_timeout);
                }
                if state.map_deadline.is_some_and(|d| d <= now) {
                    let _ = state.flush_map(&account, &writer);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::StoredFileLocation;
    use crate::domain::DomainStorage;
    use crate::qdatastream::QtDateTime;
    use crate::tdf::{APP_VERSION, encode_tdf_bytes, read_file};
    use tempfile::TempDir;

    #[test]
    fn write_manager_queue_coalesces_only_after_removal_barrier() {
        let mut q = WriteManagerQueue::default();
        let base = PathBuf::from("tdata");

        let e1 = WriteEntry {
            base_path: base.clone(),
            name: "map".into(),
            full_bytes: vec![1],
        };
        let e2 = WriteEntry {
            base_path: base.clone(),
            name: "map".into(),
            full_bytes: vec![2],
        };
        let e3 = WriteEntry {
            base_path: base.clone(),
            name: "map".into(),
            full_bytes: vec![3],
        };
        let e4 = WriteEntry {
            base_path: base.clone(),
            name: "map".into(),
            full_bytes: vec![4],
        };

        // e1 and e2 coalesce before any removal
        q.write(e1);
        q.write(e2.clone());
        assert_eq!(q.scheduled.len(), 1);
        assert_eq!(q.scheduled[0], WriteOperation::Write(e2.clone()));

        // Removal creates a barrier; e3 does NOT coalesce into e2
        q.remove(RemoveEntry {
            files: vec![base.join("maps")],
            folder: None,
        });
        q.write(e3);
        assert_eq!(q.scheduled.len(), 3);

        // e4 coalesces into e3 (after the barrier), leaving e2 and removal intact
        q.write(e4.clone());
        assert_eq!(q.scheduled.len(), 3);
        assert_eq!(q.scheduled[0], WriteOperation::Write(e2));
        assert_eq!(q.scheduled[2], WriteOperation::Write(e4));
    }

    #[tokio::test]
    async fn async_write_manager_write_remove_and_sync() {
        let dir = TempDir::new().expect("tempdir");
        let writer = AsyncWriteManager::spawn();

        let bytes_v1 = encode_tdf_bytes(APP_VERSION, b"v1").expect("encode v1");
        let bytes_v2 = encode_tdf_bytes(APP_VERSION, b"v2").expect("encode v2");

        writer.write(WriteEntry {
            base_path: dir.path().to_path_buf(),
            name: "test".into(),
            full_bytes: bytes_v1,
        });
        writer
            .write_sync(WriteEntry {
                base_path: dir.path().to_path_buf(),
                name: "test".into(),
                full_bytes: bytes_v2,
            })
            .await
            .expect("write_sync");

        let read = read_file("test", dir.path()).expect("read back");
        assert_eq!(read.data, b"v2");

        // Schedule remove followed by a fresh write: barrier ensures remove does not erase v3.
        let bytes_v3 = encode_tdf_bytes(APP_VERSION, b"v3").expect("encode v3");
        writer.schedule_remove_files(vec![dir.path().join("tests")]);
        writer.write(WriteEntry {
            base_path: dir.path().to_path_buf(),
            name: "test".into(),
            full_bytes: bytes_v3,
        });
        writer.stop().await;

        let read_v3 = read_file("test", dir.path()).expect("read v3");
        assert_eq!(read_v3.data, b"v3");
    }

    #[tokio::test(start_paused = true)]
    async fn account_write_scheduler_debounces_with_tdesktop_delays() {
        let dir = TempDir::new().expect("tempdir");
        let tdata = dir.path().join("tdata");
        let domain = DomainStorage::create_fresh(&tdata, "data").expect("create domain");
        let acc =
            AccountStorage::open(&tdata, "data", 0, domain.local_key().clone()).expect("open acc");
        let base_path = acc.base_path().to_path_buf();
        let local_key = domain.local_key().clone();
        let account = Arc::new(Mutex::new(acc));

        let writer = AsyncWriteManager::spawn();
        let scheduler = AccountWriteScheduler::spawn(Arc::clone(&account), writer.clone());

        // Schedule prefs + locations + search suggestions
        let mut prefs = BTreeMap::new();
        prefs.insert(b"theme".to_vec(), b"dark".to_vec());
        scheduler.write_prefs_delayed(prefs.clone());

        let locs = StoredLocations {
            locations: vec![StoredFileLocation {
                media_key: (11, 22),
                path: "C:/Downloads/photo.jpg".into(),
                bookmark: vec![],
                modified: QtDateTime::from_unix_millis_utc(1_700_000_000_000),
                size: 4096,
            }],
            aliases: vec![],
            downloads_serialized: vec![],
        };
        scheduler.write_locations_delayed(locs.clone());

        let suggestions = SearchSuggestionsBlobs {
            top_peers: b"top_peers_blob".to_vec(),
            ..Default::default()
        };
        scheduler.write_search_suggestions_delayed(suggestions.clone());
        // Let the scheduler task drain the initial commands at t=0 so timers arm at 1000ms / 5000ms.
        tokio::task::yield_now().await;

        // At 500ms: nothing written yet (1000ms debounce hasn't elapsed).
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
        assert!(!base_path.join("maps").exists());

        // Update prefs again at 500ms -> resets 1000ms debounce to t=1500ms.
        prefs.insert(b"lang".to_vec(), b"uk".to_vec());
        scheduler.write_prefs_delayed(prefs.clone());
        tokio::task::yield_now().await;

        // At t=1100ms: locations (armed at t=0) fired at 1000ms, so map exists,
        // but prefs (re-armed at t=500ms) has not fired yet.
        tokio::time::advance(Duration::from_millis(600)).await;
        tokio::task::yield_now().await;
        writer.sync().await;
        let mid_reopen = AccountStorage::open(&tdata, "data", 0, local_key.clone()).expect("mid");
        assert_eq!(mid_reopen.read_locations().expect("locs"), locs);
        assert!(mid_reopen.read_prefs().expect("prefs not yet").is_empty());
        assert_eq!(
            mid_reopen.read_search_suggestions().expect("sug not yet"),
            SearchSuggestionsBlobs::default()
        );

        // At t=1600ms: prefs fired at 1500ms, search_suggestions (5000ms) still pending.
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
        writer.sync().await;
        let mid2 = AccountStorage::open(&tdata, "data", 0, local_key.clone()).expect("mid2");
        assert_eq!(mid2.read_prefs().expect("prefs"), prefs);
        assert_eq!(
            mid2.read_search_suggestions().expect("sug still pending"),
            SearchSuggestionsBlobs::default()
        );

        // At t=5100ms: search_suggestions fired at 5000ms.
        tokio::time::advance(Duration::from_millis(3500)).await;
        tokio::task::yield_now().await;
        scheduler.shutdown().await.expect("shutdown");
        writer.stop().await;

        let final_reopen =
            AccountStorage::open(&tdata, "data", 0, local_key).expect("final reopen");
        assert_eq!(final_reopen.read_prefs().expect("final prefs"), prefs);
        assert_eq!(final_reopen.read_locations().expect("final locs"), locs);
        assert_eq!(
            final_reopen
                .read_search_suggestions()
                .expect("final suggestions"),
            suggestions
        );
    }
}
