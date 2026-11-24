//! Export progress view-model mapping [`ExportState`] into displayable rows.
//!
//! Port of `Telegram/SourceFiles/export/view/export_view_content.h`,
//! `Telegram/SourceFiles/export/view/export_view_content.cpp`, and size/download
//! formatting from `Telegram/SourceFiles/ui/text/format_values.cpp`.

use tg_export::{EntityType, ExportState, FinishedState, ProcessingState, Settings, Step};

/// Row ID marker indicating the export has completed (`Content::kDoneId` in `export_view_content.cpp`).
pub const DONE_ID: &str = "done";

/// Single progress bar row in the Export Progress widget or Top Bar (`Export::View::Content::Row`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContentRow {
    /// Stable row identifier used to detect transitions (`"main"`, `"chat0"`, `"file0_3"`, `"done"`, or empty when hidden).
    pub id: String,
    /// Left-aligned primary label (step name, chat title, file basename, or summary line).
    pub label: String,
    /// Right-aligned secondary info (counter `"2 / 5"`, download bytes `"1.5 / 3.5 MB"`, or empty).
    pub info: String,
    /// Completion fraction in `[0.0, 1.0]`.
    pub progress: f64,
    /// Active file download random ID (non-zero enables the delayed `"Skip this file"` link).
    pub random_id: u64,
}

/// Complete set of rows rendered by [`crate::export::progress_view::ProgressView`] and
/// [`crate::export::top_bar::ExportTopBar`] (`Export::View::Content`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Content {
    /// Ordered progress rows (padded to 2 rows for single-peer exports, 3 rows for full exports).
    pub rows: Vec<ContentRow>,
}

impl Content {
    /// Returns `true` when this content represents the finished state (`rows[0].id == DONE_ID`).
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.rows.first().is_some_and(|r| r.id == DONE_ID)
    }

    /// Returns the active file download's `random_id`, if any (last row's `random_id`).
    #[must_use]
    pub fn active_file_random_id(&self) -> u64 {
        self.rows.last().map_or(0, |r| r.random_id)
    }
}

/// Converts any [`ExportState`] carrying progress or completion data into [`Content`]
/// (port of `ContentFromState(settings, producer)`).
#[must_use]
pub fn content_from_state(settings: &Settings, state: &ExportState) -> Option<Content> {
    match state {
        ExportState::Processing(processing) => Some(content_from_processing(settings, processing)),
        ExportState::Finished(finished) => Some(content_from_finished(finished)),
        _ => None,
    }
}

/// Maps a live [`ProcessingState`] into progress rows (exact port of `ContentFromState`
/// in `export_view_content.cpp:19-168`).
#[must_use]
pub fn content_from_processing(settings: &Settings, state: &ProcessingState) -> Content {
    let mut result = Content::default();

    let push = |rows: &mut Vec<ContentRow>,
                id: String,
                label: String,
                info: String,
                progress: f64,
                random_id: u64| {
        rows.push(ContentRow {
            id,
            label,
            info,
            progress: progress.clamp(0.0, 1.0),
            random_id,
        });
    };

    let push_main = |rows: &mut Vec<ContentRow>, label: &str| {
        let info = if state.entity_count > 0 {
            format!("{} / {}", state.entity_index + 1, state.entity_count)
        } else {
            String::new()
        };
        if state.substeps_total <= 0 {
            push(rows, "main".to_owned(), label.to_owned(), info, 0.0, 0);
            return;
        }
        let substeps_total = f64::from(state.substeps_total);
        let done = f64::from(state.substeps_passed);
        let add = f64::from(state.substeps_now);
        let done_progress = done / substeps_total;
        let add_part = |index: i32, count: i32| -> f64 {
            if count > 0 {
                (add * f64::from(index)) / (substeps_total * f64::from(count))
            } else {
                0.0
            }
        };
        let add_progress = if state.entity_count == 1 && state.entity_index == 0 {
            add_part(state.item_index, state.item_count)
        } else {
            add_part(state.entity_index, state.entity_count)
        };
        push(
            rows,
            "main".to_owned(),
            label.to_owned(),
            info,
            done_progress + add_progress,
            0,
        );
    };

    let push_bytes = |rows: &mut Vec<ContentRow>, id: String, label: &str, random_id: u64| {
        if state.bytes_count <= 0 {
            return;
        }
        let progress = (state.bytes_loaded as f64) / (state.bytes_count as f64);
        let info = format_download_text(state.bytes_loaded, state.bytes_count);
        push(rows, id, label.to_owned(), info, progress, random_id);
    };

    match state.step {
        Step::Initializing => {
            push_main(&mut result.rows, "Initializing...");
        }
        Step::DialogsList => {
            push_main(&mut result.rows, "Processing chats...");
        }
        Step::PersonalInfo => {
            push_main(&mut result.rows, "Account information");
        }
        Step::Userpics => {
            push_main(&mut result.rows, "Profile photos");
            push_bytes(
                &mut result.rows,
                format!("userpic{}", state.entity_index),
                &state.bytes_name,
                state.bytes_random_id,
            );
        }
        Step::Contacts => {
            push_main(&mut result.rows, "Contacts list");
        }
        Step::Stories => {
            push_main(&mut result.rows, "Story archive");
            push_bytes(
                &mut result.rows,
                format!("story{}", state.entity_index),
                &state.bytes_name,
                state.bytes_random_id,
            );
        }
        Step::ProfileMusic => {
            push_main(&mut result.rows, "Music on Profiles");
            push_bytes(
                &mut result.rows,
                format!("music{}", state.entity_index),
                &state.bytes_name,
                state.bytes_random_id,
            );
        }
        Step::Sessions => {
            push_main(&mut result.rows, "Active sessions");
        }
        Step::OtherData => {
            push_main(&mut result.rows, "Miscellaneous data");
        }
        Step::Dialogs => {
            if state.entity_count > 1 {
                push_main(&mut result.rows, "Chats");
            }
            let chat_label = if state.entity_name.is_empty() {
                "Deleted Account".to_owned()
            } else {
                match state.entity_type {
                    EntityType::Chat => state.entity_name.clone(),
                    EntityType::SavedMessages => "Saved Messages".to_owned(),
                    EntityType::RepliesMessages => "Replies".to_owned(),
                    EntityType::VerifyCodes => "Verification Codes".to_owned(),
                    EntityType::Topic | EntityType::Other => state.entity_name.clone(),
                }
            };
            let item_info = if state.item_count > 0 {
                format!("{} / {}", state.item_index, state.item_count)
            } else {
                String::new()
            };
            let item_progress = if state.item_count > 0 {
                f64::from(state.item_index) / f64::from(state.item_count)
            } else {
                0.0
            };
            push(
                &mut result.rows,
                format!("chat{}", state.entity_index),
                chat_label,
                item_info,
                item_progress,
                0,
            );
            push_bytes(
                &mut result.rows,
                format!("file{}_{}", state.entity_index, state.item_index),
                &state.bytes_name,
                state.bytes_random_id,
            );
        }
        Step::Topic => {
            push_main(&mut result.rows, "Chats");
            let topic_label = if state.entity_name.is_empty() {
                "Deleted Account".to_owned()
            } else {
                state.entity_name.clone()
            };
            let item_info = if state.item_count > 0 {
                format!("{} / {}", state.item_index, state.item_count)
            } else {
                String::new()
            };
            let item_progress = if state.item_count > 0 {
                f64::from(state.item_index) / f64::from(state.item_count)
            } else {
                0.0
            };
            push(
                &mut result.rows,
                "topic".to_owned(),
                topic_label,
                item_info,
                item_progress,
                0,
            );
            push_bytes(
                &mut result.rows,
                format!("file_topic_{}", state.item_index),
                &state.bytes_name,
                state.bytes_random_id,
            );
        }
    }

    let required_rows = if settings.only_single_peer() { 2 } else { 3 };
    while result.rows.len() < required_rows {
        result.rows.push(ContentRow::default());
    }
    result
}

/// Maps a completed [`FinishedState`] into the 3-row completion summary
/// (exact port of `ContentFromState(const FinishedState&)` in `export_view_content.cpp:170-194`).
#[must_use]
pub fn content_from_finished(state: &FinishedState) -> Content {
    Content {
        rows: vec![
            ContentRow {
                id: DONE_ID.to_owned(),
                label: "Data export completed.".to_owned(),
                info: String::new(),
                progress: 1.0,
                random_id: 0,
            },
            ContentRow {
                id: DONE_ID.to_owned(),
                label: format!("Total files: {}", state.files_count),
                info: String::new(),
                progress: 1.0,
                random_id: 0,
            },
            ContentRow {
                id: DONE_ID.to_owned(),
                label: format!("Total size: {}", format_size_text(state.bytes_count)),
                info: String::new(),
                progress: 1.0,
                random_id: 0,
            },
        ],
    }
}

/// Formats tenths as `"{whole}.{frac}"` (port of `FormatTenths` in `format_values.cpp:24-28`).
fn format_tenths(tenths: i64) -> String {
    let clamped = tenths.max(0);
    format!("{}.{}", clamped / 10, clamped % 10)
}

/// Formats a byte count matching `Ui::FormatSizeText` in `format_values.cpp:54-66`.
#[must_use]
pub fn format_size_text(size: i64) -> String {
    let size = size.max(0);
    if size >= 1024 * 1024 {
        format!("{} MB", format_tenths((size * 10) / (1024 * 1024)))
    } else if size >= 1024 {
        format!("{} KB", format_tenths((size * 10) / 1024))
    } else {
        format!("{size} B")
    }
}

/// Formats active download progress matching `Ui::FormatDownloadText` (`lng_save_downloaded`:
/// `"{ready} / {total} {mb}"`) in `format_values.cpp:30-73`.
#[must_use]
pub fn format_download_text(ready: i64, total: i64) -> String {
    let ready = ready.max(0);
    let total = total.max(0);
    if total >= 1024 * 1024 {
        let ready_str = format_tenths((ready * 10) / (1024 * 1024));
        let total_str = format_tenths((total * 10) / (1024 * 1024));
        format!("{ready_str} / {total_str} MB")
    } else if total >= 1024 {
        format!("{} / {} KB", ready / 1024, total / 1024)
    } else {
        format!("{ready} / {total} B")
    }
}
