//! Port of `export/output/export_output_html_and_json.h` and `export/output/export_output_html_and_json.cpp`.

use crate::data::{
    ContactsList, DialogInfo, DialogsInfo, File, MessagesSlice, PersonalInfo, ProfileMusicInfo,
    ProfileMusicSlice, SessionsList, StoriesInfo, StoriesSlice, UserpicsInfo, UserpicsSlice,
};
use crate::output::html::HtmlWriter;
use crate::output::json::JsonWriter;
use crate::output::result::OutputResult;
use crate::output::stats::Stats;
use crate::output::{AbstractWriter, OutputFormat};
use crate::settings::{Environment, Settings};

/// Combined writer that outputs both HTML and JSON simultaneously
/// (port of `Export::Output::HtmlAndJsonWriter`).
pub struct HtmlAndJsonWriter {
    writers: Vec<Box<dyn AbstractWriter>>,
}

impl Default for HtmlAndJsonWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl HtmlAndJsonWriter {
    /// Creates a new combined HTML + JSON writer.
    pub fn new() -> Self {
        Self {
            writers: vec![Box::new(HtmlWriter::new()), Box::new(JsonWriter::new())],
        }
    }

    fn invoke<F>(&mut self, mut method: F) -> OutputResult
    where
        F: FnMut(&mut dyn AbstractWriter) -> OutputResult,
    {
        let mut result = OutputResult::success();
        for writer in &mut self.writers {
            let current = method(writer.as_mut());
            if current.is_error() {
                result = current;
            }
        }
        result
    }
}

impl AbstractWriter for HtmlAndJsonWriter {
    fn format(&self) -> OutputFormat {
        OutputFormat::HtmlAndJson
    }

    fn start(
        &mut self,
        settings: &Settings,
        environment: &Environment,
        stats: Option<&Stats>,
    ) -> OutputResult {
        self.invoke(|w| w.start(settings, environment, stats))
    }

    fn write_personal(&mut self, data: &PersonalInfo, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_personal(data, stats))
    }

    fn write_userpics_start(&mut self, data: &UserpicsInfo, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_userpics_start(data, stats))
    }

    fn write_userpics_slice(
        &mut self,
        data: &UserpicsSlice,
        stats: Option<&Stats>,
    ) -> OutputResult {
        self.invoke(|w| w.write_userpics_slice(data, stats))
    }

    fn write_userpics_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_userpics_end(stats))
    }

    fn write_stories_start(&mut self, data: &StoriesInfo, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_stories_start(data, stats))
    }

    fn write_stories_slice(&mut self, data: &StoriesSlice, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_stories_slice(data, stats))
    }

    fn write_stories_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_stories_end(stats))
    }

    fn write_profile_music_start(
        &mut self,
        data: &ProfileMusicInfo,
        stats: Option<&Stats>,
    ) -> OutputResult {
        self.invoke(|w| w.write_profile_music_start(data, stats))
    }

    fn write_profile_music_slice(
        &mut self,
        data: &ProfileMusicSlice,
        stats: Option<&Stats>,
    ) -> OutputResult {
        self.invoke(|w| w.write_profile_music_slice(data, stats))
    }

    fn write_profile_music_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_profile_music_end(stats))
    }

    fn write_contacts_list(&mut self, data: &ContactsList, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_contacts_list(data, stats))
    }

    fn write_sessions_list(&mut self, data: &SessionsList, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_sessions_list(data, stats))
    }

    fn write_other_data(&mut self, data: &File, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_other_data(data, stats))
    }

    fn write_dialogs_start(&mut self, data: &DialogsInfo, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_dialogs_start(data, stats))
    }

    fn write_dialog_start(&mut self, data: &DialogInfo, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_dialog_start(data, stats))
    }

    fn write_dialog_slice(&mut self, data: &MessagesSlice, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_dialog_slice(data, stats))
    }

    fn write_dialog_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_dialog_end(stats))
    }

    fn write_dialogs_end(&mut self, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.write_dialogs_end(stats))
    }

    fn finish(&mut self, stats: Option<&Stats>) -> OutputResult {
        self.invoke(|w| w.finish(stats))
    }

    fn main_file_path(&self) -> String {
        self.writers
            .first()
            .map(|w| w.main_file_path())
            .unwrap_or_default()
    }
}
