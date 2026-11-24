//! Port of `export/export_controller.h` and `export/export_controller.cpp`.
//!
//! Implements the step-planning state machine (`Step`, `ProcessingState`, `ExportState`,
//! `ExportController`) driven via an async [`ExportSource`] trait that `tg-api` implements,
//! plus an [`InMemoryExportSource`] for tests.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;

use crate::data::{
    ContactsList, DialogInfo, DialogType, DialogsInfo, File, MessagesSlice, PersonalInfo,
    ProfileMusicInfo, ProfileMusicSlice, SessionsList, StoriesInfo, StoriesSlice, UserpicsInfo,
    UserpicsSlice, finalize_dialogs_info,
};
use crate::output::file::OutputFile;
use crate::output::result::OutputResult;
use crate::output::stats::Stats;
use crate::output::{AbstractWriter, create_writer, normalize_path};
use crate::settings::{Environment, ExportTypes, Settings};
use crate::stand_in::{InputPeer, PeerId};

/// API error reported by an [`ExportSource`] (stand-in for `MTP::Error`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApiError {
    /// Numeric RPC error code.
    pub code: i32,
    /// Telegram RPC error identifier (e.g. `"TAKEOUT_INVALID"`, `"TAKEOUT_INIT_DELAY_86400"`).
    pub kind: String,
    /// Human-readable description.
    pub description: String,
}

impl ApiError {
    /// Creates a new `ApiError`.
    pub fn new(code: i32, kind: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            code,
            kind: kind.into(),
            description: description.into(),
        }
    }

    /// Parses the wait duration in seconds from a `TAKEOUT_INIT_DELAY_<seconds>` error
    /// (port of `export/view/export_view_panel_controller.cpp` takeout delay parsing).
    #[must_use]
    pub fn parse_takeout_init_delay(&self) -> Option<i32> {
        self.kind
            .strip_prefix("TAKEOUT_INIT_DELAY_")
            .and_then(|s| s.parse::<i32>().ok())
    }

    /// Returns `true` if the server rejected the takeout session with `TAKEOUT_INVALID`.
    #[must_use]
    pub fn is_takeout_invalid(&self) -> bool {
        self.kind == "TAKEOUT_INVALID"
    }
}

/// Counts gathered during the `Initializing` step (port of `ApiWrap::StartInfo`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartInfo {
    /// Total profile pictures count.
    pub userpics_count: i32,
    /// Total stories count.
    pub stories_count: i32,
    /// Total profile music tracks count.
    pub profile_music_count: i32,
    /// Total dialogs count (`chats + left`).
    pub dialogs_count: i32,
}

/// File download progress update (port of `ApiWrap::DownloadProgress`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Random request ID (used by `skip_file`).
    pub random_id: u64,
    /// Relative path being downloaded.
    pub path: String,
    /// Current item index within the slice.
    pub item_index: i32,
    /// Bytes downloaded so far.
    pub ready: i64,
    /// Total file bytes.
    pub total: i64,
}

/// Export pipeline step (port of `Export::ProcessingState::Step`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum Step {
    /// Initializing takeout session and counting items.
    #[default]
    Initializing = 0,
    /// Collecting dialogs list and left channels.
    DialogsList = 1,
    /// Exporting personal info.
    PersonalInfo = 2,
    /// Exporting profile pictures.
    Userpics = 3,
    /// Exporting stories archive.
    Stories = 4,
    /// Exporting profile music.
    ProfileMusic = 5,
    /// Exporting contacts and frequent contacts.
    Contacts = 6,
    /// Exporting active sessions and web sessions.
    Sessions = 7,
    /// Exporting `other_data.json`.
    OtherData = 8,
    /// Exporting per-dialog message histories.
    Dialogs = 9,
    /// Exporting a single forum topic.
    Topic = 10,
}

impl Step {
    const COUNT: usize = 11;
}

/// Entity classification shown in progress UI (port of `Export::ProcessingState::EntityType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum EntityType {
    /// Regular user/group/channel chat.
    Chat,
    /// Saved Messages.
    SavedMessages,
    /// Replies chat.
    RepliesMessages,
    /// Verification Codes chat.
    VerifyCodes,
    /// Single forum topic.
    Topic,
    /// Non-chat entity (personal info, contacts, sessions, etc.).
    #[default]
    Other,
}

/// Initial password-check state (port of `Export::PasswordCheckState`).
#[derive(Debug, Clone, PartialEq)]
pub struct PasswordCheckState {
    /// Password hint string.
    pub hint: String,
    /// Unconfirmed recovery email pattern.
    pub unconfirmed_pattern: String,
    /// Still requesting password state.
    pub requesting: bool,
    /// Account has a cloud password.
    pub has_password: bool,
    /// Password verified.
    pub checked: bool,
    /// Single-peer export target if applicable.
    pub single_peer: InputPeer,
}

impl Default for PasswordCheckState {
    fn default() -> Self {
        Self {
            hint: String::new(),
            unconfirmed_pattern: String::new(),
            requesting: false,
            has_password: false,
            checked: false,
            single_peer: InputPeer::Empty,
        }
    }
}

/// Live export progress state (port of `Export::ProcessingState`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessingState {
    /// Current step.
    pub step: Step,
    /// Cumulative substeps completed before the current step.
    pub substeps_passed: i32,
    /// Number of substeps allocated to the current step.
    pub substeps_now: i32,
    /// Total substeps across the entire export.
    pub substeps_total: i32,
    /// Entity classification.
    pub entity_type: EntityType,
    /// Current entity display name (e.g. chat title or topic title).
    pub entity_name: String,
    /// Zero-based index of the current entity.
    pub entity_index: i32,
    /// Total entities in the current step.
    pub entity_count: i32,
    /// Current item index (e.g. messages written so far).
    pub item_index: i32,
    /// Total expected items for the current entity.
    pub item_count: i32,
    /// Active file download random ID (for skipping).
    pub bytes_random_id: u64,
    /// Active file basename.
    pub bytes_name: String,
    /// Active file bytes loaded.
    pub bytes_loaded: i64,
    /// Active file total bytes.
    pub bytes_count: i64,
}

/// Final completion summary (port of `Export::FinishedState`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FinishedState {
    /// Path to main entry file (`export_results.html`, `messages.html`, or `result.json`).
    pub path: String,
    /// Total files written.
    pub files_count: i32,
    /// Total bytes written.
    pub bytes_count: i64,
}

/// Overall export controller state machine state (port of `Export::State`).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ExportState {
    /// Uninitialized / empty.
    #[default]
    None,
    /// Password check stage.
    PasswordCheck(PasswordCheckState),
    /// Actively exporting data.
    Processing(ProcessingState),
    /// Stopped due to a Telegram API error.
    ApiError(ApiError),
    /// Stopped due to a local file I/O error at `path`.
    OutputError {
        /// File path that failed.
        path: String,
    },
    /// Cancelled by the caller.
    Cancelled,
    /// Finished successfully.
    Finished(FinishedState),
}

impl ExportState {
    /// Returns true if the export has reached a terminal state.
    pub const fn is_stopped(&self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::ApiError(_) | Self::OutputError { .. } | Self::Finished(_)
        )
    }
}

/// Normalizes settings before starting an export (port of `NormalizeSettings` in `export_controller.cpp`).
///
/// For single-peer exports, enables `ANY_CHATS_MASK` for both `types` and `full_chats`.
pub fn normalize_settings(settings: &Settings) -> Settings {
    let mut result = settings.clone();
    if result.only_single_peer() {
        result.types = ExportTypes::ANY_CHATS_MASK;
        result.full_chats = ExportTypes::ANY_CHATS_MASK;
    }
    result
}

/// Plans the exact ordered sequence of export [`Step`]s for `settings`
/// (port of `ControllerObject::fillExportSteps`).
pub fn plan_export_steps(settings: &Settings) -> Vec<Step> {
    let mut steps = vec![Step::Initializing];
    if settings.only_single_topic() {
        steps.push(Step::Topic);
        return steps;
    }
    if settings.types.intersects(ExportTypes::ANY_CHATS_MASK) {
        steps.push(Step::DialogsList);
    }
    if settings.types.contains(ExportTypes::PERSONAL_INFO) {
        steps.push(Step::PersonalInfo);
    }
    if settings.types.contains(ExportTypes::USERPICS) {
        steps.push(Step::Userpics);
    }
    if settings.types.contains(ExportTypes::STORIES) {
        steps.push(Step::Stories);
    }
    if settings.types.contains(ExportTypes::PROFILE_MUSIC) {
        steps.push(Step::ProfileMusic);
    }
    if settings.types.contains(ExportTypes::CONTACTS) {
        steps.push(Step::Contacts);
    }
    if settings.types.contains(ExportTypes::SESSIONS) {
        steps.push(Step::Sessions);
    }
    if settings.types.contains(ExportTypes::OTHER_DATA) {
        steps.push(Step::OtherData);
    }
    if settings.types.intersects(ExportTypes::ANY_CHATS_MASK) {
        steps.push(Step::Dialogs);
    }
    steps
}

/// Computes the per-step substep weights and total substeps
/// (port of `ControllerObject::fillSubstepsInSteps`).
pub fn compute_substeps(settings: &Settings, info: &StartInfo) -> ([i32; Step::COUNT], i32) {
    let mut counts = [0_i32; Step::COUNT];
    counts[Step::Initializing as usize] = 1;
    if settings.types.intersects(ExportTypes::ANY_CHATS_MASK) {
        counts[Step::DialogsList as usize] = 1;
    }
    if settings.types.contains(ExportTypes::PERSONAL_INFO) {
        counts[Step::PersonalInfo as usize] = 1;
    }
    if settings.types.contains(ExportTypes::USERPICS) {
        counts[Step::Userpics as usize] = 1;
    }
    if settings.types.contains(ExportTypes::STORIES) {
        counts[Step::Stories as usize] = 1;
    }
    if settings.types.contains(ExportTypes::PROFILE_MUSIC) {
        counts[Step::ProfileMusic as usize] = 1;
    }
    if settings.types.contains(ExportTypes::CONTACTS) {
        counts[Step::Contacts as usize] = 1;
    }
    if settings.types.contains(ExportTypes::SESSIONS) {
        counts[Step::Sessions as usize] = 1;
    }
    if settings.types.contains(ExportTypes::OTHER_DATA) {
        counts[Step::OtherData as usize] = 1;
    }
    if settings.types.intersects(ExportTypes::ANY_CHATS_MASK) {
        counts[Step::Dialogs as usize] = info.dialogs_count;
    }
    if settings.only_single_topic() {
        counts[Step::Topic as usize] = 1;
    }
    let total = counts.iter().sum();
    (counts, total)
}

/// Event emitted by streaming [`ExportSource`] methods (`userpics`, `stories`, `profile_music`,
/// `messages`, `topic_messages`).
#[derive(Debug, Clone)]
pub enum StreamEvent<Start, Slice> {
    /// Stream started with header metadata.
    Start(Start),
    /// Per-file download progress update.
    Progress(DownloadProgress),
    /// A batch/slice of exported items ready to be written.
    Slice(Slice),
}

/// Async data source trait driving [`ExportController`] (to be implemented by `tg-api`).
///
/// Mirrors the callbacks of `Export::ApiWrap` cleanly using Rust async methods and synchronous
/// streaming event callbacks for slices and progress updates.
pub trait ExportSource: Send {
    /// Initializes the takeout session and returns high-level counts.
    fn start_export(
        &mut self,
        settings: &Settings,
        stats: &Stats,
    ) -> impl Future<Output = Result<StartInfo, ApiError>> + Send;

    /// Collects the complete list of dialogs and left channels across takeout splits.
    fn request_dialogs_list(
        &mut self,
        on_progress: &mut (dyn FnMut(i32) -> bool + Send),
    ) -> impl Future<Output = Result<DialogsInfo, ApiError>> + Send;

    /// Fetches personal account information.
    fn request_personal_info(
        &mut self,
    ) -> impl Future<Output = Result<PersonalInfo, ApiError>> + Send;

    /// Streams profile pictures via [`StreamEvent`] callbacks.
    fn request_userpics(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<UserpicsInfo, UserpicsSlice>) -> bool + Send),
    ) -> impl Future<Output = Result<(), ApiError>> + Send;

    /// Streams archived stories via [`StreamEvent`] callbacks.
    fn request_stories(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<StoriesInfo, StoriesSlice>) -> bool + Send),
    ) -> impl Future<Output = Result<(), ApiError>> + Send;

    /// Streams profile music tracks via [`StreamEvent`] callbacks.
    fn request_profile_music(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<ProfileMusicInfo, ProfileMusicSlice>) -> bool + Send),
    ) -> impl Future<Output = Result<(), ApiError>> + Send;

    /// Fetches saved contacts and frequent contacts.
    fn request_contacts(&mut self) -> impl Future<Output = Result<ContactsList, ApiError>> + Send;

    /// Fetches active sessions and web sessions.
    fn request_sessions(&mut self) -> impl Future<Output = Result<SessionsList, ApiError>> + Send;

    /// Downloads `other_data.json` to `relative_path` inside the export folder and returns its `File`.
    fn request_other_data(
        &mut self,
        relative_path: &str,
    ) -> impl Future<Output = Result<File, ApiError>> + Send;

    /// Streams messages for a single `dialog` (across its splits) via [`StreamEvent`] callbacks.
    fn request_messages(
        &mut self,
        dialog: &DialogInfo,
        on_event: &mut (dyn FnMut(StreamEvent<DialogInfo, MessagesSlice>) -> bool + Send),
    ) -> impl Future<Output = Result<(), ApiError>> + Send;

    /// Streams messages for a single forum topic via [`StreamEvent`] callbacks.
    fn request_topic_messages(
        &mut self,
        peer_id: PeerId,
        input_peer: &InputPeer,
        topic_root_id: i32,
        on_event: &mut (dyn FnMut(StreamEvent<i32, MessagesSlice>) -> bool + Send),
    ) -> impl Future<Output = Result<(), ApiError>> + Send;

    /// Closes the takeout session cleanly on the server.
    fn finish_export(&mut self) -> impl Future<Output = Result<(), ApiError>> + Send;

    /// Finishes an active takeout session with `success = false` after cancellation
    /// (`ApiWrap::cancelExportFast`, `export_api_wrap.cpp:1944-1951`).
    fn finish_export_cancelled(&mut self) -> impl Future<Output = Result<(), ApiError>> + Send {
        async { Ok(()) }
    }

    /// Skips an in-flight file download by random ID.
    fn skip_file(&mut self, random_id: u64);

    /// Cancels any active API requests immediately.
    fn cancel_export_fast(&mut self);
}

/// Handle allowing callers to cancel a running [`ExportController`] or inspect its live state.
#[derive(Clone)]
pub struct ExportControlHandle {
    cancelled: Arc<AtomicBool>,
    state_rx: watch::Receiver<ExportState>,
}

impl ExportControlHandle {
    /// Signals fast cancellation of the export.
    pub fn cancel_fast(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Returns a clone of the latest [`ExportState`].
    pub fn current_state(&self) -> ExportState {
        self.state_rx.borrow().clone()
    }

    /// Returns a `watch::Receiver` subscribed to state changes.
    pub fn subscribe(&self) -> watch::Receiver<ExportState> {
        self.state_rx.clone()
    }
}

/// Asynchronous export pipeline controller (port of `Export::Controller` / `ControllerObject`).
pub struct ExportController<S: ExportSource> {
    source: S,
    cancelled: Arc<AtomicBool>,
    state_tx: watch::Sender<ExportState>,
    state_history: Vec<ExportState>,
    substeps_in_step: [i32; Step::COUNT],
    substeps_total: i32,
    substeps_passed: i32,
    last_processing_step: Step,
    stats: Stats,
    topic_root_id: i32,
    topic_peer_id: u64,
    topic_title: String,
    skip_path_normalization: bool,
}

impl<S: ExportSource> ExportController<S> {
    /// Creates a new controller for a full account or single-peer export.
    pub fn new(source: S, single_peer: InputPeer) -> (Self, ExportControlHandle) {
        Self::with_topic(source, single_peer, 0, 0, String::new())
    }

    /// Creates a new controller configured for a single forum topic export.
    pub fn with_topic(
        source: S,
        single_peer: InputPeer,
        topic_root_id: i32,
        topic_peer_id: u64,
        topic_title: String,
    ) -> (Self, ExportControlHandle) {
        let initial = ExportState::PasswordCheck(PasswordCheckState {
            checked: false,
            requesting: false,
            single_peer,
            ..Default::default()
        });
        let (state_tx, state_rx) = watch::channel(initial.clone());
        let cancelled = Arc::new(AtomicBool::new(false));
        let handle = ExportControlHandle {
            cancelled: Arc::clone(&cancelled),
            state_rx,
        };
        let controller = Self {
            source,
            cancelled,
            state_tx,
            state_history: vec![initial],
            substeps_in_step: [0; Step::COUNT],
            substeps_total: 0,
            substeps_passed: 0,
            last_processing_step: Step::Initializing,
            stats: Stats::new(),
            topic_root_id,
            topic_peer_id,
            topic_title,
            skip_path_normalization: false,
        };
        (controller, handle)
    }

    /// Disables dated-subfolder normalization (useful when the caller already prepared the exact path).
    pub fn set_skip_path_normalization(&mut self, skip: bool) {
        self.skip_path_normalization = skip;
    }

    /// Returns all state transitions emitted during the export.
    pub fn state_history(&self) -> &[ExportState] {
        &self.state_history
    }

    /// Returns a shared reference to the underlying [`ExportSource`].
    pub fn source(&self) -> &S {
        &self.source
    }

    /// Returns the accumulated output stats.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Runs the full export state machine to completion and returns the terminal [`ExportState`].
    pub async fn run(&mut self, settings: &Settings, environment: &Environment) -> ExportState {
        let mut settings = normalize_settings(settings);
        if self.topic_root_id != 0 {
            settings.single_topic_root_id = self.topic_root_id;
            settings.single_topic_peer_id = self.topic_peer_id;
            settings.single_topic_title = self.topic_title.clone();
        }
        if !self.skip_path_normalization {
            settings.path = normalize_path(&settings);
        } else if !settings.path.ends_with('/') {
            settings.path.push('/');
        }

        let mut writer: Box<dyn AbstractWriter> = create_writer(settings.format);
        let steps = plan_export_steps(&settings);
        let mut dialogs_info = DialogsInfo::default();

        for step in steps {
            if self.check_cancelled().await {
                return self.current_state();
            }
            match step {
                Step::Initializing => {
                    self.set_state(ExportState::Processing(ProcessingState::default()));
                    let info = match self.source.start_export(&settings, &self.stats).await {
                        Ok(info) => info,
                        Err(err) => {
                            self.set_state(ExportState::ApiError(err));
                            return self.current_state();
                        }
                    };
                    if self.io_catch_error(writer.start(&settings, environment, Some(&self.stats)))
                    {
                        return self.current_state();
                    }
                    let (counts, total) = compute_substeps(&settings, &info);
                    self.substeps_in_step = counts;
                    self.substeps_total = total;
                }
                Step::DialogsList => {
                    let st = self.prepare_dialogs_list_state(0);
                    self.set_state(ExportState::Processing(st));
                    let mut progress_states = Vec::new();
                    let cancelled = Arc::clone(&self.cancelled);
                    let mut on_progress = |count: i32| -> bool {
                        if cancelled.load(Ordering::SeqCst) {
                            return false;
                        }
                        if count > 0 {
                            progress_states.push(count - 1);
                        }
                        true
                    };
                    match self.source.request_dialogs_list(&mut on_progress).await {
                        Ok(result) => {
                            for processed in progress_states {
                                let s = self.prepare_dialogs_list_state(processed);
                                self.set_state(ExportState::Processing(s));
                            }
                            dialogs_info = result;
                        }
                        Err(err) => {
                            self.set_state(ExportState::ApiError(err));
                            return self.current_state();
                        }
                    }
                }
                Step::PersonalInfo => {
                    let st = self.prepare_state(Step::PersonalInfo);
                    self.set_state(ExportState::Processing(st));
                    match self.source.request_personal_info().await {
                        Ok(info) => {
                            if self.io_catch_error(writer.write_personal(&info, Some(&self.stats)))
                            {
                                return self.current_state();
                            }
                        }
                        Err(err) => {
                            self.set_state(ExportState::ApiError(err));
                            return self.current_state();
                        }
                    }
                }
                Step::Userpics => {
                    let mut userpics_written = 0_i32;
                    let mut userpics_count = 0_i32;
                    let mut io_err: Option<OutputResult> = None;
                    let mut progress_events: Vec<(i32, i32, DownloadProgress)> = Vec::new();
                    let stats_ref = &self.stats;
                    let cancelled = Arc::clone(&self.cancelled);

                    let mut on_event = |event: StreamEvent<UserpicsInfo, UserpicsSlice>| -> bool {
                        if cancelled.load(Ordering::SeqCst) {
                            return false;
                        }
                        match event {
                            StreamEvent::Start(start) => {
                                let res = writer.write_userpics_start(&start, Some(stats_ref));
                                if res.is_error() {
                                    io_err = Some(res);
                                    return false;
                                }
                                userpics_written = 0;
                                userpics_count = start.count;
                                true
                            }
                            StreamEvent::Progress(p) => {
                                progress_events.push((userpics_written, userpics_count, p));
                                true
                            }
                            StreamEvent::Slice(slice) => {
                                let res = writer.write_userpics_slice(&slice, Some(stats_ref));
                                if res.is_error() {
                                    io_err = Some(res);
                                    return false;
                                }
                                userpics_written += slice.list.len() as i32;
                                progress_events.push((
                                    userpics_written,
                                    userpics_count,
                                    DownloadProgress::default(),
                                ));
                                true
                            }
                        }
                    };

                    let api_res = self.source.request_userpics(&mut on_event).await;
                    for (written, total, progress) in progress_events {
                        let s = self.prepare_item_download_state(
                            Step::Userpics,
                            written,
                            total,
                            &progress,
                        );
                        self.set_state(ExportState::Processing(s));
                    }
                    if let Some(err) = io_err {
                        self.io_catch_error(err);
                        return self.current_state();
                    }
                    if let Err(err) = api_res {
                        self.set_state(ExportState::ApiError(err));
                        return self.current_state();
                    }
                    if self.io_catch_error(writer.write_userpics_end(Some(&self.stats))) {
                        return self.current_state();
                    }
                }
                Step::Stories => {
                    let mut stories_written = 0_i32;
                    let mut stories_count = 0_i32;
                    let mut io_err: Option<OutputResult> = None;
                    let mut progress_events: Vec<(i32, i32, DownloadProgress)> = Vec::new();
                    let stats_ref = &self.stats;
                    let cancelled = Arc::clone(&self.cancelled);

                    let mut on_event = |event: StreamEvent<StoriesInfo, StoriesSlice>| -> bool {
                        if cancelled.load(Ordering::SeqCst) {
                            return false;
                        }
                        match event {
                            StreamEvent::Start(start) => {
                                let res = writer.write_stories_start(&start, Some(stats_ref));
                                if res.is_error() {
                                    io_err = Some(res);
                                    return false;
                                }
                                stories_written = 0;
                                stories_count = start.count;
                                true
                            }
                            StreamEvent::Progress(p) => {
                                progress_events.push((stories_written, stories_count, p));
                                true
                            }
                            StreamEvent::Slice(slice) => {
                                let res = writer.write_stories_slice(&slice, Some(stats_ref));
                                if res.is_error() {
                                    io_err = Some(res);
                                    return false;
                                }
                                stories_written += slice.list.len() as i32;
                                progress_events.push((
                                    stories_written,
                                    stories_count,
                                    DownloadProgress::default(),
                                ));
                                true
                            }
                        }
                    };

                    let api_res = self.source.request_stories(&mut on_event).await;
                    for (written, total, progress) in progress_events {
                        let s = self.prepare_item_download_state(
                            Step::Stories,
                            written,
                            total,
                            &progress,
                        );
                        self.set_state(ExportState::Processing(s));
                    }
                    if let Some(err) = io_err {
                        self.io_catch_error(err);
                        return self.current_state();
                    }
                    if let Err(err) = api_res {
                        self.set_state(ExportState::ApiError(err));
                        return self.current_state();
                    }
                    if self.io_catch_error(writer.write_stories_end(Some(&self.stats))) {
                        return self.current_state();
                    }
                }
                Step::ProfileMusic => {
                    let mut music_written = 0_i32;
                    let mut music_count = 0_i32;
                    let mut io_err: Option<OutputResult> = None;
                    let mut progress_events: Vec<(i32, i32, DownloadProgress)> = Vec::new();
                    let stats_ref = &self.stats;
                    let cancelled = Arc::clone(&self.cancelled);

                    let mut on_event =
                        |event: StreamEvent<ProfileMusicInfo, ProfileMusicSlice>| -> bool {
                            if cancelled.load(Ordering::SeqCst) {
                                return false;
                            }
                            match event {
                                StreamEvent::Start(start) => {
                                    let res =
                                        writer.write_profile_music_start(&start, Some(stats_ref));
                                    if res.is_error() {
                                        io_err = Some(res);
                                        return false;
                                    }
                                    music_written = 0;
                                    music_count = start.count;
                                    true
                                }
                                StreamEvent::Progress(p) => {
                                    progress_events.push((music_written, music_count, p));
                                    true
                                }
                                StreamEvent::Slice(slice) => {
                                    let res =
                                        writer.write_profile_music_slice(&slice, Some(stats_ref));
                                    if res.is_error() {
                                        io_err = Some(res);
                                        return false;
                                    }
                                    music_written += slice.list.len() as i32;
                                    progress_events.push((
                                        music_written,
                                        music_count,
                                        DownloadProgress::default(),
                                    ));
                                    true
                                }
                            }
                        };

                    let api_res = self.source.request_profile_music(&mut on_event).await;
                    for (written, total, progress) in progress_events {
                        let s = self.prepare_item_download_state(
                            Step::ProfileMusic,
                            written,
                            total,
                            &progress,
                        );
                        self.set_state(ExportState::Processing(s));
                    }
                    if let Some(err) = io_err {
                        self.io_catch_error(err);
                        return self.current_state();
                    }
                    if let Err(err) = api_res {
                        self.set_state(ExportState::ApiError(err));
                        return self.current_state();
                    }
                    if self.io_catch_error(writer.write_profile_music_end(Some(&self.stats))) {
                        return self.current_state();
                    }
                }
                Step::Contacts => {
                    let st = self.prepare_state(Step::Contacts);
                    self.set_state(ExportState::Processing(st));
                    match self.source.request_contacts().await {
                        Ok(contacts) => {
                            if self.io_catch_error(
                                writer.write_contacts_list(&contacts, Some(&self.stats)),
                            ) {
                                return self.current_state();
                            }
                        }
                        Err(err) => {
                            self.set_state(ExportState::ApiError(err));
                            return self.current_state();
                        }
                    }
                }
                Step::Sessions => {
                    let st = self.prepare_state(Step::Sessions);
                    self.set_state(ExportState::Processing(st));
                    match self.source.request_sessions().await {
                        Ok(sessions) => {
                            if self.io_catch_error(
                                writer.write_sessions_list(&sessions, Some(&self.stats)),
                            ) {
                                return self.current_state();
                            }
                        }
                        Err(err) => {
                            self.set_state(ExportState::ApiError(err));
                            return self.current_state();
                        }
                    }
                }
                Step::OtherData => {
                    let st = self.prepare_state(Step::OtherData);
                    self.set_state(ExportState::Processing(st));
                    match self
                        .source
                        .request_other_data("lists/other_data.json")
                        .await
                    {
                        Ok(file) => {
                            if self
                                .io_catch_error(writer.write_other_data(&file, Some(&self.stats)))
                            {
                                return self.current_state();
                            }
                        }
                        Err(err) => {
                            self.set_state(ExportState::ApiError(err));
                            return self.current_state();
                        }
                    }
                }
                Step::Dialogs => {
                    if self.io_catch_error(
                        writer.write_dialogs_start(&dialogs_info, Some(&self.stats)),
                    ) {
                        return self.current_state();
                    }
                    let total_dialogs = dialogs_info.total_count();
                    for dialog_idx in 0..total_dialogs {
                        if self.check_cancelled().await {
                            return self.current_state();
                        }
                        let dialog = match dialogs_info.item(dialog_idx) {
                            Some(d) => d.clone(),
                            None => break,
                        };
                        let mut messages_written = 0_i32;
                        let mut messages_count = 0_i32;
                        let mut io_err: Option<OutputResult> = None;
                        let mut progress_events: Vec<(i32, i32, DownloadProgress)> = Vec::new();
                        let stats_ref = &self.stats;
                        let cancelled = Arc::clone(&self.cancelled);

                        let mut on_event = |event: StreamEvent<DialogInfo, MessagesSlice>| -> bool {
                            if cancelled.load(Ordering::SeqCst) {
                                return false;
                            }
                            match event {
                                StreamEvent::Start(info) => {
                                    let res = writer.write_dialog_start(&info, Some(stats_ref));
                                    if res.is_error() {
                                        io_err = Some(res);
                                        return false;
                                    }
                                    messages_written = 0;
                                    messages_count = info.messages_count_per_split.iter().sum();
                                    progress_events.push((
                                        messages_written,
                                        messages_count,
                                        DownloadProgress::default(),
                                    ));
                                    true
                                }
                                StreamEvent::Progress(p) => {
                                    progress_events.push((messages_written, messages_count, p));
                                    true
                                }
                                StreamEvent::Slice(slice) => {
                                    let res = writer.write_dialog_slice(&slice, Some(stats_ref));
                                    if res.is_error() {
                                        io_err = Some(res);
                                        return false;
                                    }
                                    messages_written += slice.list.len() as i32;
                                    progress_events.push((
                                        messages_written,
                                        messages_count,
                                        DownloadProgress::default(),
                                    ));
                                    true
                                }
                            }
                        };

                        let api_res = self.source.request_messages(&dialog, &mut on_event).await;
                        for (written, count, progress) in progress_events {
                            let s = self.prepare_dialog_state(
                                &dialogs_info,
                                dialog_idx,
                                written,
                                count,
                                &progress,
                            );
                            self.set_state(ExportState::Processing(s));
                        }
                        if let Some(err) = io_err {
                            self.io_catch_error(err);
                            return self.current_state();
                        }
                        if let Err(err) = api_res {
                            self.set_state(ExportState::ApiError(err));
                            return self.current_state();
                        }
                        if self.io_catch_error(writer.write_dialog_end(Some(&self.stats))) {
                            return self.current_state();
                        }
                    }
                    if self.io_catch_error(writer.write_dialogs_end(Some(&self.stats))) {
                        return self.current_state();
                    }
                }
                Step::Topic => {
                    let topic_info = DialogInfo {
                        kind: DialogType::PublicSupergroup,
                        name: settings.single_topic_title.clone(),
                        peer_id: PeerId::from_raw(settings.single_topic_peer_id),
                        relative_path: String::new(),
                        ..Default::default()
                    };
                    if self
                        .io_catch_error(writer.write_dialog_start(&topic_info, Some(&self.stats)))
                    {
                        return self.current_state();
                    }
                    let mut messages_written = 0_i32;
                    let mut messages_count = 0_i32;
                    let mut io_err: Option<OutputResult> = None;
                    let mut progress_events: Vec<(i32, i32, DownloadProgress)> = Vec::new();
                    let stats_ref = &self.stats;
                    let cancelled = Arc::clone(&self.cancelled);

                    let mut on_event = |event: StreamEvent<i32, MessagesSlice>| -> bool {
                        if cancelled.load(Ordering::SeqCst) {
                            return false;
                        }
                        match event {
                            StreamEvent::Start(count) => {
                                messages_written = 0;
                                messages_count = count;
                                progress_events.push((
                                    messages_written,
                                    messages_count,
                                    DownloadProgress::default(),
                                ));
                                true
                            }
                            StreamEvent::Progress(p) => {
                                progress_events.push((messages_written, messages_count, p));
                                true
                            }
                            StreamEvent::Slice(slice) => {
                                let res = writer.write_dialog_slice(&slice, Some(stats_ref));
                                if res.is_error() {
                                    io_err = Some(res);
                                    return false;
                                }
                                messages_written += slice.list.len() as i32;
                                progress_events.push((
                                    messages_written,
                                    messages_count,
                                    DownloadProgress::default(),
                                ));
                                true
                            }
                        }
                    };

                    let api_res = self
                        .source
                        .request_topic_messages(
                            PeerId::from_raw(settings.single_topic_peer_id),
                            &settings.single_peer,
                            settings.single_topic_root_id,
                            &mut on_event,
                        )
                        .await;
                    for (written, count, progress) in progress_events {
                        let s = self.prepare_topic_state(
                            &settings.single_topic_title,
                            written,
                            count,
                            &progress,
                        );
                        self.set_state(ExportState::Processing(s));
                    }
                    if let Some(err) = io_err {
                        self.io_catch_error(err);
                        return self.current_state();
                    }
                    if let Err(err) = api_res {
                        self.set_state(ExportState::ApiError(err));
                        return self.current_state();
                    }
                    if self.io_catch_error(writer.write_dialog_end(Some(&self.stats))) {
                        return self.current_state();
                    }
                }
            }
        }

        if self.check_cancelled().await {
            return self.current_state();
        }
        if self.io_catch_error(writer.finish(Some(&self.stats))) {
            return self.current_state();
        }
        if let Err(err) = self.source.finish_export().await {
            self.set_state(ExportState::ApiError(err));
            return self.current_state();
        }
        self.set_state(ExportState::Finished(FinishedState {
            path: writer.main_file_path(),
            files_count: self.stats.files_count(),
            bytes_count: self.stats.bytes_count(),
        }));
        self.current_state()
    }

    fn current_state(&self) -> ExportState {
        self.state_tx.borrow().clone()
    }

    fn set_state(&mut self, state: ExportState) {
        if self.state_tx.borrow().is_stopped() {
            return;
        }
        self.state_history.push(state.clone());
        let _ = self.state_tx.send_replace(state);
    }

    async fn check_cancelled(&mut self) -> bool {
        if self.cancelled.load(Ordering::SeqCst) {
            self.source.cancel_export_fast();
            let _ = self.source.finish_export_cancelled().await;
            self.set_state(ExportState::Cancelled);
            true
        } else {
            false
        }
    }

    fn io_catch_error(&mut self, result: OutputResult) -> bool {
        if result.is_error() {
            self.set_state(ExportState::OutputError { path: result.path });
            true
        } else {
            false
        }
    }

    fn prepare_state(&mut self, step: Step) -> ProcessingState {
        if step != self.last_processing_step {
            self.substeps_passed += self.substeps_in_step[self.last_processing_step as usize];
            self.last_processing_step = step;
        }
        ProcessingState {
            step,
            substeps_passed: self.substeps_passed,
            substeps_now: self.substeps_in_step[step as usize],
            substeps_total: self.substeps_total,
            ..Default::default()
        }
    }

    fn prepare_dialogs_list_state(&mut self, processed: i32) -> ProcessingState {
        let mut s = self.prepare_state(Step::DialogsList);
        s.entity_index = processed;
        s.entity_count = processed.max(self.substeps_in_step[Step::Dialogs as usize]);
        s
    }

    fn prepare_item_download_state(
        &mut self,
        step: Step,
        written: i32,
        total: i32,
        progress: &DownloadProgress,
    ) -> ProcessingState {
        let mut s = self.prepare_state(step);
        s.entity_index = written + progress.item_index;
        s.entity_count = total.max(s.entity_index);
        fill_download_bytes(&mut s, progress);
        s
    }

    fn prepare_dialog_state(
        &mut self,
        info: &DialogsInfo,
        index: usize,
        written: i32,
        count: i32,
        progress: &DownloadProgress,
    ) -> ProcessingState {
        let mut s = self.prepare_state(Step::Dialogs);
        if let Some(dialog) = info.item(index) {
            s.entity_index = index as i32;
            s.entity_count = info.total_count() as i32;
            s.entity_name = dialog.name.clone();
            s.entity_type = match dialog.kind {
                DialogType::SelfChat => EntityType::SavedMessages,
                DialogType::Replies => EntityType::RepliesMessages,
                DialogType::VerifyCodes => EntityType::VerifyCodes,
                _ => EntityType::Chat,
            };
        }
        s.item_index = written + progress.item_index;
        s.item_count = count.max(s.item_index);
        fill_download_bytes(&mut s, progress);
        s
    }

    fn prepare_topic_state(
        &mut self,
        topic_title: &str,
        written: i32,
        count: i32,
        progress: &DownloadProgress,
    ) -> ProcessingState {
        let mut s = self.prepare_state(Step::Topic);
        s.entity_type = EntityType::Topic;
        s.entity_name = topic_title.to_string();
        s.entity_index = 0;
        s.entity_count = 1;
        s.item_index = written + progress.item_index;
        s.item_count = count.max(s.item_index);
        fill_download_bytes(&mut s, progress);
        s
    }
}

fn fill_download_bytes(state: &mut ProcessingState, progress: &DownloadProgress) {
    state.bytes_random_id = progress.random_id;
    if !progress.path.is_empty() {
        let name = progress.path.rsplit('/').next().unwrap_or(&progress.path);
        state.bytes_name = name.to_string();
    }
    state.bytes_loaded = progress.ready;
    state.bytes_count = progress.total;
}

/// In-memory fake [`ExportSource`] for unit and golden tests.
///
/// Serves configured account data, media files, multi-split dialog message slices,
/// and topic slices without any network calls.
#[derive(Debug, Default, Clone)]
pub struct InMemoryExportSource {
    /// Personal info returned by `request_personal_info`.
    pub personal_info: PersonalInfo,
    /// Userpic slices streamed by `request_userpics`.
    pub userpic_slices: Vec<UserpicsSlice>,
    /// Story slices streamed by `request_stories`.
    pub story_slices: Vec<StoriesSlice>,
    /// Profile music slices streamed by `request_profile_music`.
    pub profile_music_slices: Vec<ProfileMusicSlice>,
    /// Contacts list returned by `request_contacts`.
    pub contacts: ContactsList,
    /// Sessions list returned by `request_sessions`.
    pub sessions: SessionsList,
    /// Raw JSON bytes written for `other_data.json` if `Some`.
    pub other_data_json: Option<Vec<u8>>,
    /// Dialogs list returned by `request_dialogs_list`.
    pub dialogs: DialogsInfo,
    /// Message slices per dialog `PeerId` (supporting multiple splits/slices per chat).
    pub dialog_messages: BTreeMap<PeerId, Vec<MessagesSlice>>,
    /// Topic message slices streamed by `request_topic_messages`.
    pub topic_messages: Vec<MessagesSlice>,
    /// Optional binary files to materialize inside the export directory `(relative_path -> bytes)`.
    pub files_to_write: BTreeMap<String, Vec<u8>>,
    /// Optional forced API error to simulate failures.
    pub fail_on_start: Option<ApiError>,
    /// Skipped file random IDs recorded for test assertions.
    pub skipped_files: Vec<u64>,
    /// Whether `cancel_export_fast` was called.
    pub cancelled: bool,
    /// Whether `finish_export` was called.
    pub finished: bool,

    base_path: String,
    settings: Settings,
}

impl ExportSource for InMemoryExportSource {
    async fn start_export(
        &mut self,
        settings: &Settings,
        stats: &Stats,
    ) -> Result<StartInfo, ApiError> {
        if let Some(ref err) = self.fail_on_start {
            return Err(err.clone());
        }
        self.base_path = settings.path.clone();
        self.settings = settings.clone();
        for (rel, bytes) in &self.files_to_write {
            let full = format!("{}{rel}", self.base_path);
            let _ = OutputFile::write_bytes(&full, bytes, Some(stats));
        }
        Ok(StartInfo {
            userpics_count: self
                .userpic_slices
                .iter()
                .map(|s| s.list.len() as i32)
                .sum(),
            stories_count: self
                .story_slices
                .iter()
                .map(|s| s.list.len() as i32 + s.skipped)
                .sum(),
            profile_music_count: self
                .profile_music_slices
                .iter()
                .map(|s| s.list.len() as i32 + s.skipped)
                .sum(),
            dialogs_count: self.dialogs.total_count() as i32,
        })
    }

    async fn request_dialogs_list(
        &mut self,
        on_progress: &mut (dyn FnMut(i32) -> bool + Send),
    ) -> Result<DialogsInfo, ApiError> {
        let mut info = self.dialogs.clone();
        finalize_dialogs_info(&mut info, &self.settings);
        let total = info.total_count() as i32;
        if total > 0 {
            let _ = on_progress(total);
        }
        Ok(info)
    }

    async fn request_personal_info(&mut self) -> Result<PersonalInfo, ApiError> {
        Ok(self.personal_info.clone())
    }

    async fn request_userpics(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<UserpicsInfo, UserpicsSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        let count: i32 = self
            .userpic_slices
            .iter()
            .map(|s| s.list.len() as i32)
            .sum();
        if !on_event(StreamEvent::Start(UserpicsInfo { count })) {
            return Ok(());
        }
        for slice in self.userpic_slices.clone() {
            if slice.list.is_empty() {
                continue;
            }
            for (idx, photo) in slice.list.iter().enumerate() {
                if !on_event(StreamEvent::Progress(DownloadProgress {
                    random_id: photo.id,
                    path: photo.image.file.relative_path.clone(),
                    item_index: idx as i32,
                    ready: photo.image.file.size,
                    total: photo.image.file.size,
                })) {
                    return Ok(());
                }
            }
            if !on_event(StreamEvent::Slice(slice)) {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn request_stories(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<StoriesInfo, StoriesSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        let count: i32 = self
            .story_slices
            .iter()
            .map(|s| s.list.len() as i32 + s.skipped)
            .sum();
        if !on_event(StreamEvent::Start(StoriesInfo { count })) {
            return Ok(());
        }
        for slice in self.story_slices.clone() {
            for (idx, story) in slice.list.iter().enumerate() {
                let path = story
                    .file()
                    .map(|f| f.relative_path.clone())
                    .unwrap_or_default();
                let size = story.file().map(|f| f.size).unwrap_or(0);
                if !on_event(StreamEvent::Progress(DownloadProgress {
                    random_id: story.id as u64,
                    path,
                    item_index: idx as i32,
                    ready: size,
                    total: size,
                })) {
                    return Ok(());
                }
            }
            if !on_event(StreamEvent::Slice(slice)) {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn request_profile_music(
        &mut self,
        on_event: &mut (dyn FnMut(StreamEvent<ProfileMusicInfo, ProfileMusicSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        let count: i32 = self
            .profile_music_slices
            .iter()
            .map(|s| s.list.len() as i32 + s.skipped)
            .sum();
        if !on_event(StreamEvent::Start(ProfileMusicInfo { count })) {
            return Ok(());
        }
        for slice in self.profile_music_slices.clone() {
            for (idx, msg) in slice.list.iter().enumerate() {
                let path = msg
                    .file()
                    .map(|f| f.relative_path.clone())
                    .unwrap_or_default();
                let size = msg.file().map(|f| f.size).unwrap_or(0);
                if !on_event(StreamEvent::Progress(DownloadProgress {
                    random_id: msg.id as u64,
                    path,
                    item_index: idx as i32,
                    ready: size,
                    total: size,
                })) {
                    return Ok(());
                }
            }
            if !on_event(StreamEvent::Slice(slice)) {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn request_contacts(&mut self) -> Result<ContactsList, ApiError> {
        Ok(self.contacts.clone())
    }

    async fn request_sessions(&mut self) -> Result<SessionsList, ApiError> {
        Ok(self.sessions.clone())
    }

    async fn request_other_data(&mut self, relative_path: &str) -> Result<File, ApiError> {
        let bytes = self.other_data_json.clone().unwrap_or_default();
        let full = format!("{}{relative_path}", self.base_path);
        let _ = OutputFile::write_bytes(&full, &bytes, None);
        Ok(File {
            size: bytes.len() as i64,
            relative_path: relative_path.to_string(),
            suggested_path: relative_path.to_string(),
            ..Default::default()
        })
    }

    async fn request_messages(
        &mut self,
        dialog: &DialogInfo,
        on_event: &mut (dyn FnMut(StreamEvent<DialogInfo, MessagesSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        let slices = self
            .dialog_messages
            .get(&dialog.peer_id)
            .cloned()
            .unwrap_or_default();
        let mut started_dialog = dialog.clone();
        if started_dialog.messages_count_per_split.is_empty() {
            started_dialog.messages_count_per_split =
                slices.iter().map(|s| s.list.len() as i32).collect();
        }
        if !on_event(StreamEvent::Start(started_dialog)) {
            return Ok(());
        }
        for slice in slices {
            if slice.list.is_empty() {
                continue;
            }
            for (idx, msg) in slice.list.iter().enumerate() {
                if let Some(f) = msg.file()
                    && !f.relative_path.is_empty()
                    && !on_event(StreamEvent::Progress(DownloadProgress {
                        random_id: msg.id as u64,
                        path: f.relative_path.clone(),
                        item_index: idx as i32,
                        ready: f.size,
                        total: f.size,
                    }))
                {
                    return Ok(());
                }
            }
            if !on_event(StreamEvent::Slice(slice)) {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn request_topic_messages(
        &mut self,
        _peer_id: PeerId,
        _input_peer: &InputPeer,
        _topic_root_id: i32,
        on_event: &mut (dyn FnMut(StreamEvent<i32, MessagesSlice>) -> bool + Send),
    ) -> Result<(), ApiError> {
        let total: i32 = self
            .topic_messages
            .iter()
            .map(|s| s.list.len() as i32)
            .sum();
        if !on_event(StreamEvent::Start(total)) {
            return Ok(());
        }
        for slice in self.topic_messages.clone() {
            if slice.list.is_empty() {
                continue;
            }
            for (idx, msg) in slice.list.iter().enumerate() {
                if let Some(f) = msg.file()
                    && !f.relative_path.is_empty()
                    && !on_event(StreamEvent::Progress(DownloadProgress {
                        random_id: msg.id as u64,
                        path: f.relative_path.clone(),
                        item_index: idx as i32,
                        ready: f.size,
                        total: f.size,
                    }))
                {
                    return Ok(());
                }
            }
            if !on_event(StreamEvent::Slice(slice)) {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn finish_export(&mut self) -> Result<(), ApiError> {
        self.finished = true;
        Ok(())
    }

    fn skip_file(&mut self, random_id: u64) {
        self.skipped_files.push(random_id);
    }

    fn cancel_export_fast(&mut self) {
        self.cancelled = true;
    }
}
