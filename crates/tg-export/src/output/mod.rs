//! Port of `export/output/export_output_abstract.h` and `export/output/export_output_abstract.cpp`.

pub mod assets;
pub mod file;
pub mod html;
pub mod html_and_json;
pub mod json;
pub mod result;
pub mod stats;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::data::{
    ContactsList, DialogInfo, DialogsInfo, File, MessagesSlice, PersonalInfo, ProfileMusicInfo,
    ProfileMusicSlice, SessionsList, StoriesInfo, StoriesSlice, UserpicsInfo, UserpicsSlice,
};
use crate::output::html::HtmlWriter;
use crate::output::html_and_json::HtmlAndJsonWriter;
use crate::output::json::JsonWriter;
use crate::output::result::OutputResult;
use crate::output::stats::Stats;
use crate::settings::{Environment, Settings};
use crate::stand_in::unix_to_civil;

/// Export output format (port of `Export::Output::Format`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum OutputFormat {
    /// Human-readable HTML export (`export_results.html` / `messages.html`).
    #[default]
    Html = 0,
    /// Machine-readable JSON export (`result.json`).
    Json = 1,
    /// Both HTML and JSON written to the same folder.
    HtmlAndJson = 2,
}

impl OutputFormat {
    /// Converts to the raw `u32` tag persisted in account settings.
    pub const fn to_raw(self) -> u32 {
        self as u32
    }

    /// Parses from the raw `u32` tag persisted in account settings.
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Html),
            1 => Some(Self::Json),
            2 => Some(Self::HtmlAndJson),
            _ => None,
        }
    }
}

/// Resolves and normalizes the target export directory path, creating a dated subfolder
/// (`DataExport_YYYY-MM-DD` or `ChatExport_YYYY-MM-DD`) when `force_sub_path` is set or
/// the directory already contains entries (port of `Export::Output::NormalizePath`).
pub fn normalize_path(settings: &Settings) -> String {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (year, month, day, _, _, _) = unix_to_civil(now_secs, 0);
    normalize_path_with_date(settings, &format!("{year:04}-{month:02}-{day:02}"))
}

/// Deterministic variant of [`normalize_path`] taking an explicit `YYYY-MM-DD` ISO date string.
pub fn normalize_path_with_date(settings: &Settings, iso_date: &str) -> String {
    let normalized_raw = settings.path.replace('\\', "/");
    let mut result = if normalized_raw.ends_with('/') {
        normalized_raw
    } else {
        format!("{normalized_raw}/")
    };

    let folder = Path::new(&result);
    if !folder.exists() && !settings.force_sub_path {
        return result;
    }

    let is_empty = match std::fs::read_dir(folder) {
        Ok(mut iter) => iter.next().is_none(),
        Err(_) => true,
    };
    if is_empty && !settings.force_sub_path {
        return result;
    }

    let base = if settings.only_single_peer() {
        format!("ChatExport_{iso_date}")
    } else {
        format!("DataExport_{iso_date}")
    };

    let mut index = 0_u32;
    loop {
        let candidate = if index == 0 {
            base.clone()
        } else {
            format!("{base} ({index})")
        };
        let full = format!("{result}{candidate}");
        if !Path::new(&full).exists() {
            result.push_str(&candidate);
            result.push('/');
            return result;
        }
        index += 1;
    }
}

/// Abstract export writer interface (port of `Export::Output::AbstractWriter`).
pub trait AbstractWriter: Send {
    /// Returns the writer's output format.
    fn format(&self) -> OutputFormat;

    /// Initializes the writer for an export session.
    fn start(
        &mut self,
        settings: &Settings,
        environment: &Environment,
        stats: Option<&Stats>,
    ) -> OutputResult;

    /// Writes personal account information.
    fn write_personal(&mut self, data: &PersonalInfo, stats: Option<&Stats>) -> OutputResult;

    /// Starts the profile pictures section.
    fn write_userpics_start(&mut self, data: &UserpicsInfo, stats: Option<&Stats>) -> OutputResult;
    /// Writes a slice of profile pictures.
    fn write_userpics_slice(&mut self, data: &UserpicsSlice, stats: Option<&Stats>)
    -> OutputResult;
    /// Ends the profile pictures section.
    fn write_userpics_end(&mut self, stats: Option<&Stats>) -> OutputResult;

    /// Starts the stories section.
    fn write_stories_start(&mut self, data: &StoriesInfo, stats: Option<&Stats>) -> OutputResult;
    /// Writes a slice of stories.
    fn write_stories_slice(&mut self, data: &StoriesSlice, stats: Option<&Stats>) -> OutputResult;
    /// Ends the stories section.
    fn write_stories_end(&mut self, stats: Option<&Stats>) -> OutputResult;

    /// Starts the profile music section.
    fn write_profile_music_start(
        &mut self,
        data: &ProfileMusicInfo,
        stats: Option<&Stats>,
    ) -> OutputResult;
    /// Writes a slice of profile music items.
    fn write_profile_music_slice(
        &mut self,
        data: &ProfileMusicSlice,
        stats: Option<&Stats>,
    ) -> OutputResult;
    /// Ends the profile music section.
    fn write_profile_music_end(&mut self, stats: Option<&Stats>) -> OutputResult;

    /// Writes saved contacts and frequent contacts.
    fn write_contacts_list(&mut self, data: &ContactsList, stats: Option<&Stats>) -> OutputResult;

    /// Writes active sessions and web sessions.
    fn write_sessions_list(&mut self, data: &SessionsList, stats: Option<&Stats>) -> OutputResult;

    /// Writes server-supplied `other_data.json`.
    fn write_other_data(&mut self, data: &File, stats: Option<&Stats>) -> OutputResult;

    /// Starts the dialogs section.
    fn write_dialogs_start(&mut self, data: &DialogsInfo, stats: Option<&Stats>) -> OutputResult;
    /// Starts an individual dialog's messages output.
    fn write_dialog_start(&mut self, data: &DialogInfo, stats: Option<&Stats>) -> OutputResult;
    /// Writes a slice of messages for the current dialog.
    fn write_dialog_slice(&mut self, data: &MessagesSlice, stats: Option<&Stats>) -> OutputResult;
    /// Finishes the current dialog.
    fn write_dialog_end(&mut self, stats: Option<&Stats>) -> OutputResult;
    /// Finishes the dialogs section.
    fn write_dialogs_end(&mut self, stats: Option<&Stats>) -> OutputResult;

    /// Finalizes and closes all main output files.
    fn finish(&mut self, stats: Option<&Stats>) -> OutputResult;

    /// Returns the path to the primary entry file (e.g. `export_results.html` or `result.json`).
    fn main_file_path(&self) -> String;
}

/// Factory returning the appropriate writer for `format` (port of `Export::Output::CreateWriter`).
pub fn create_writer(format: OutputFormat) -> Box<dyn AbstractWriter> {
    match format {
        OutputFormat::Html => Box::new(HtmlWriter::new()),
        OutputFormat::Json => Box::new(JsonWriter::new()),
        OutputFormat::HtmlAndJson => Box::new(HtmlAndJsonWriter::new()),
    }
}
