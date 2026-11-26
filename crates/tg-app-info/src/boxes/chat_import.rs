//! External chat history import modal box & WhatsApp / LINE / KakaoTalk archive parser.
//!
//! Port origin: Telegram API `messages.{check,init,start}HistoryImport`,
//! `messages.checkHistoryImportPeer`, `messages.uploadImportedMedia`, and desktop import UI.
//!
//! Supports `.zip` archives and standalone `.txt` transcripts from WhatsApp, LINE, and
//! KakaoTalk via drag-and-drop or file picker.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use egui::{Align2, CornerRadius, FontId, Pos2, Rect, Stroke, Ui, Vec2};
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use crate::boxes::{BoxHost, BoxKind};
use crate::info::backend::{InfoBackend, PeerProfile};
use tg_app_core::stand_in::types::PeerId;

pub use tg_app_core::core_types::dto::chat_import::*;

/// File picker strategy used by [`ChatImportBoxState`] in [`ChatImportStep::SelectFile`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ImportFilePicker {
    /// Open the native OS file dialog (`rfd::FileDialog`).
    #[default]
    Native,
    /// Deterministic path provider for headless/UI tests (avoids blocking OS dialog).
    PresetPath(PathBuf),
}

/// Modal box state for importing WhatsApp / LINE / KakaoTalk chat history archives.
#[derive(Debug)]
pub struct ChatImportBoxState {
    /// Current wizard step.
    pub step: ChatImportStep,
    /// Optional local path of the loaded archive.
    pub file_path: Option<PathBuf>,
    /// Optional preferred destination chat (when launched from a chat's Info menu).
    pub preferred_peer: Option<(PeerId, String)>,
    /// File picker strategy invoked by the `"Choose Export File..."` button.
    pub file_picker: ImportFilePicker,
    /// Parsed archive contents (transcript + bundled media).
    pub archive: Option<ParsedImportArchive>,
    /// Server response from `messages.checkHistoryImport`.
    pub parsed_info: Option<HistoryImportParsedInfo>,
    /// Filter query in the peer picker step.
    pub peer_query: String,
    /// Selected destination peer ID.
    pub selected_peer: Option<PeerId>,
    /// Selected destination peer title.
    pub selected_peer_title: String,
    /// Confirmation message returned by `messages.checkHistoryImportPeer`.
    pub confirm_text: String,
    /// Last progress fraction (`0.0..=1.0`) reported during upload.
    pub progress: f32,
    /// Status text describing current phase.
    pub status_text: String,
    /// Final summary once completed.
    pub summary: Option<ChatImportSummary>,
    /// Validation or RPC error message.
    pub error: Option<String>,
}

impl Default for ChatImportBoxState {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatImportBoxState {
    /// Creates an empty Chat History Import dialog at [`ChatImportStep::SelectFile`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            step: ChatImportStep::SelectFile,
            file_path: None,
            preferred_peer: None,
            file_picker: ImportFilePicker::Native,
            archive: None,
            parsed_info: None,
            peer_query: String::new(),
            selected_peer: None,
            selected_peer_title: String::new(),
            confirm_text: String::new(),
            progress: 0.0,
            status_text: String::new(),
            summary: None,
            error: None,
        }
    }

    /// Creates a Chat History Import dialog at [`ChatImportStep::SelectFile`] pre-associated with
    /// a destination chat (e.g. opened from a chat profile's `"Import Chat History"` menu item).
    #[must_use]
    pub fn for_peer(peer_id: PeerId, peer_title: impl Into<String>) -> Self {
        let mut state = Self::new();
        state.preferred_peer = Some((peer_id, peer_title.into()));
        state
    }

    /// Overrides the file picker strategy (e.g. [`ImportFilePicker::PresetPath`] in headless tests).
    #[must_use]
    pub fn with_file_picker(mut self, picker: ImportFilePicker) -> Self {
        self.file_picker = picker;
        self
    }

    /// Creates a Chat History Import dialog preloaded with a parsed archive and checked
    /// against `backend.check_history_import(...)`.
    pub fn from_archive(
        archive: ParsedImportArchive,
        file_path: Option<PathBuf>,
        initial_peer: Option<(PeerId, String)>,
        backend: &dyn InfoBackend,
    ) -> Self {
        let mut state = Self::new();
        state.file_path = file_path;
        state.load_parsed_archive(archive, initial_peer, backend);
        state
    }

    /// Loads `archive`, invokes `messages.checkHistoryImport`, and advances to peer selection
    /// (or straight to confirmation if `initial_peer` is compatible and passes peer check).
    pub fn load_parsed_archive(
        &mut self,
        archive: ParsedImportArchive,
        initial_peer: Option<(PeerId, String)>,
        backend: &dyn InfoBackend,
    ) {
        self.error = None;
        self.summary = None;
        self.progress = 0.0;
        match backend.check_history_import(&archive.import_head) {
            Ok(parsed) => {
                self.archive = Some(archive);
                self.parsed_info = Some(parsed);
                self.step = ChatImportStep::SelectPeer;
                if let Some((pid, title)) = initial_peer
                    && let Ok(confirm) = backend.check_history_import_peer(pid)
                {
                    self.selected_peer = Some(pid);
                    self.selected_peer_title = title;
                    self.confirm_text = confirm;
                    self.step = ChatImportStep::Confirm;
                }
            }
            Err(err) => {
                self.archive = Some(archive);
                self.parsed_info = None;
                self.error = Some(err);
                self.step = ChatImportStep::SelectFile;
            }
        }
    }

    /// Loads an export `.zip` or `.txt` file from disk and validates it with `backend`.
    pub fn load_file_path(
        &mut self,
        path: &Path,
        initial_peer: Option<(PeerId, String)>,
        backend: &dyn InfoBackend,
    ) -> Result<(), String> {
        let archive = parse_import_file(path)?;
        self.file_path = Some(path.to_path_buf());
        self.load_parsed_archive(archive, initial_peer, backend);
        if let Some(ref err) = self.error {
            return Err(err.clone());
        }
        Ok(())
    }

    /// Selects destination `peer`, runs `messages.checkHistoryImportPeer`, and advances to Confirm.
    pub fn select_peer(
        &mut self,
        peer_id: PeerId,
        peer_title: impl Into<String>,
        backend: &dyn InfoBackend,
    ) -> Result<(), String> {
        self.error = None;
        let confirm = backend.check_history_import_peer(peer_id)?;
        self.selected_peer = Some(peer_id);
        self.selected_peer_title = peer_title.into();
        self.confirm_text = confirm;
        self.step = ChatImportStep::Confirm;
        Ok(())
    }

    /// Executes the full import flow (`initHistoryImport` -> `uploadImportedMedia` ->
    /// `startHistoryImport`) via `backend`.
    pub fn start_import(
        &mut self,
        backend: &mut dyn InfoBackend,
    ) -> Result<ChatImportSummary, String> {
        let Some(peer_id) = self.selected_peer else {
            let msg = "Select a destination chat first.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        };
        let Some(archive) = self.archive.as_ref() else {
            let msg = "Load a chat export archive first.".to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        };
        self.error = None;
        self.progress = 0.15;
        self.status_text = format!("Importing into {}...", self.selected_peer_title);
        match backend.run_history_import(peer_id, archive) {
            Ok(summary) => {
                self.progress = 1.0;
                self.status_text = format!(
                    "Imported chat history ({} media uploaded).",
                    summary.uploaded_media_count
                );
                self.summary = Some(summary.clone());
                self.step = ChatImportStep::Completed;
                Ok(summary)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor for the current wizard step.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = match self.step {
            ChatImportStep::SelectFile => "Import Chat History",
            ChatImportStep::SelectPeer => "Choose Destination Chat",
            ChatImportStep::Confirm => "Confirm History Import",
            ChatImportStep::Completed => "Import Started",
        };
        let mut modal = BoxModal::new(id, title).width(st::box_wide_width.0 as f32 + 24.0);
        match self.step {
            ChatImportStep::SelectFile => {
                modal = modal.add_button(BoxButton::default_btn("cancel", "Cancel"));
            }
            ChatImportStep::SelectPeer => {
                modal = modal
                    .with_left_button(BoxButton::default_btn("import_back", "Back"))
                    .add_button(BoxButton::default_btn("cancel", "Cancel"));
            }
            ChatImportStep::Confirm => {
                modal = modal
                    .with_left_button(BoxButton::default_btn("import_back", "Back"))
                    .add_button(BoxButton::default_btn("cancel", "Cancel"))
                    .add_button(BoxButton::default_btn("import_start", "Import"));
            }
            ChatImportStep::Completed => {
                modal = modal.add_button(BoxButton::default_btn("close", "Done"));
            }
        }
        modal
    }

    /// Renders the body of the Chat History Import modal box.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &dyn InfoBackend,
        all_peers: &[PeerProfile],
        colors: &dyn Colors,
    ) -> Option<ChatImportBoxAction> {
        let mut action = None;
        let text_fg = colors.color("windowFg");
        let sub_fg = colors.color("windowSubTextFg");
        let accent = colors.color("windowBgActive");
        let err_fg = colors.color("boxTextFgError");

        match self.step {
            ChatImportStep::SelectFile => {
                ui.label(
                    egui::RichText::new(
                        "Import messages and media from WhatsApp, LINE, or KakaoTalk (.zip or .txt export).",
                    )
                    .font(FontId::new(13.0, family_regular()))
                    .color(text_fg),
                );
                ui.add_space(10.0);

                let btn_style = RoundButtonStyle::default_active(colors);
                if RoundButton::new("Choose Export File (.zip / .txt)...", btn_style)
                    .show(ui)
                    .clicked()
                {
                    let picked = match &self.file_picker {
                        ImportFilePicker::Native => rfd::FileDialog::new()
                            .set_title("Select Chat Export Archive")
                            .add_filter("Chat Exports", &["zip", "txt"])
                            .pick_file(),
                        ImportFilePicker::PresetPath(path) => Some(path.clone()),
                    };
                    if let Some(path) = picked {
                        let initial = self.preferred_peer.clone();
                        let _ = self.load_file_path(&path, initial, backend);
                        action = Some(ChatImportBoxAction::RefreshModal);
                    }
                }

                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Tip: You can also drag and drop an exported .zip file onto the window.",
                    )
                    .font(FontId::new(11.5, family_regular()))
                    .color(sub_fg),
                );

                if let Some(ref archive) = self.archive {
                    ui.add_space(8.0);
                    paint_archive_summary_card(ui, archive, self.parsed_info.as_ref(), colors);
                }
            }
            ChatImportStep::SelectPeer => {
                if let Some(ref archive) = self.archive {
                    paint_archive_summary_card(ui, archive, self.parsed_info.as_ref(), colors);
                    ui.add_space(8.0);
                }

                let field_st = InputFieldStyle::default_input(colors);
                InputField::single_line(
                    "tg_chat_import_peer_search",
                    &mut self.peer_query,
                    "Search destination chat...",
                    field_st,
                )
                .show(ui);
                ui.add_space(6.0);

                let q = self.peer_query.trim().to_lowercase();
                let parsed = self.parsed_info.as_ref();
                let mut picked: Option<(PeerId, String)> = None;

                for peer in all_peers {
                    if let Some(p) = parsed
                        && !p.allows_peer(peer)
                    {
                        continue;
                    }
                    if !q.is_empty() && !peer.title.to_lowercase().contains(&q) {
                        continue;
                    }
                    let row_h = 40.0;
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    if resp.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            colors.color("windowBgOver"),
                        );
                    }
                    ui.painter().text(
                        Pos2::new(rect.left() + 10.0, rect.center().y - 6.0),
                        Align2::LEFT_CENTER,
                        &peer.title,
                        FontId::new(13.5, family_semibold()),
                        text_fg,
                    );
                    ui.painter().text(
                        Pos2::new(rect.left() + 10.0, rect.center().y + 9.0),
                        Align2::LEFT_CENTER,
                        if peer.kind.is_group() {
                            "Group"
                        } else {
                            "Private Chat"
                        },
                        FontId::new(11.0, family_regular()),
                        sub_fg,
                    );
                    if resp.clicked() {
                        picked = Some((peer.id, peer.title.clone()));
                    }
                }

                if let Some((pid, title)) = picked
                    && self.select_peer(pid, title, backend).is_ok()
                {
                    action = Some(ChatImportBoxAction::RefreshModal);
                }
            }
            ChatImportStep::Confirm => {
                if let Some(ref archive) = self.archive {
                    paint_archive_summary_card(ui, archive, self.parsed_info.as_ref(), colors);
                    ui.add_space(8.0);
                }
                ui.label(
                    egui::RichText::new(format!("Destination: {}", self.selected_peer_title))
                        .font(FontId::new(13.5, family_semibold()))
                        .color(text_fg),
                );
                ui.add_space(4.0);
                let confirm_msg = if self.confirm_text.trim().is_empty() {
                    "Messages from this export will be imported into the selected chat."
                } else {
                    self.confirm_text.as_str()
                };
                ui.label(
                    egui::RichText::new(confirm_msg)
                        .font(FontId::new(12.5, family_regular()))
                        .color(sub_fg),
                );
            }
            ChatImportStep::Completed => {
                let bar_w = ui.available_width().max(120.0);
                let (bar_rect, _) =
                    ui.allocate_exact_size(Vec2::new(bar_w, 8.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    bar_rect,
                    CornerRadius::same(4),
                    colors.color("windowBgOver"),
                );
                let fill = Rect::from_min_size(
                    bar_rect.min,
                    Vec2::new(
                        bar_rect.width() * self.progress.clamp(0.0, 1.0),
                        bar_rect.height(),
                    ),
                );
                ui.painter()
                    .rect_filled(fill, CornerRadius::same(4), accent);
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(&self.status_text)
                        .font(FontId::new(13.5, family_semibold()))
                        .color(text_fg),
                );
                if let Some(ref sum) = self.summary {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Import ID: {} · Media uploaded: {} · Skipped: {}",
                            sum.import_id, sum.uploaded_media_count, sum.skipped_media_count
                        ))
                        .font(FontId::new(12.0, family_regular()))
                        .color(sub_fg),
                    );
                }
            }
        }

        if let Some(ref err) = self.error {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(err)
                    .font(FontId::new(12.0, family_regular()))
                    .color(err_fg),
            );
        }

        action
    }
}

fn paint_archive_summary_card(
    ui: &mut Ui,
    archive: &ParsedImportArchive,
    parsed: Option<&HistoryImportParsedInfo>,
    colors: &dyn Colors,
) {
    let card_w = ui.available_width().max(160.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(card_w, 64.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), colors.color("windowBgOver"));
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, colors.color("shadowFg")),
        egui::StrokeKind::Inside,
    );

    let title_line = match parsed.and_then(|p| p.title.as_deref()) {
        Some(t) if !t.is_empty() => format!("{} · {}", archive.source_app.label(), t),
        _ => format!("{} · {}", archive.source_app.label(), archive.archive_name),
    };
    ui.painter().text(
        Pos2::new(rect.left() + 12.0, rect.top() + 20.0),
        Align2::LEFT_CENTER,
        title_line,
        FontId::new(13.0, family_semibold()),
        colors.color("windowFg"),
    );

    let detail = format!(
        "{} · {} lines · {} media{}",
        archive.history_file_name,
        archive.line_count,
        archive.media_files.len(),
        parsed
            .map(|p| format!(" · {}", p.target_summary()))
            .unwrap_or_default()
    );
    ui.painter().text(
        Pos2::new(rect.left() + 12.0, rect.top() + 43.0),
        Align2::LEFT_CENTER,
        detail,
        FontId::new(11.5, family_regular()),
        colors.color("windowSubTextFg"),
    );
}

// ---------------------------------------------------------------------------
// File drop / open flow integration helper
// ---------------------------------------------------------------------------

/// Checks `egui` dropped files for a WhatsApp / LINE / KakaoTalk export `.zip` or `.txt`
/// and pushes [`BoxKind::ChatImport`] onto `box_host` when one is detected.
///
/// Returns `true` if an export archive was handled and the import box was opened.
pub fn try_handle_dropped_import_files(
    ctx: &egui::Context,
    box_host: &mut BoxHost,
    backend: &dyn InfoBackend,
    colors: &dyn Colors,
    active_peer: Option<(PeerId, String)>,
) -> bool {
    let dropped = ctx.input(|i| i.raw.dropped_files.clone());
    if dropped.is_empty() {
        return false;
    }
    for file in dropped {
        let path = file.path();
        if !is_supported_import_path(path) {
            continue;
        }
        let parsed = if path.is_file() {
            parse_import_file(path)
                .ok()
                .filter(|arch| arch.source_app != ImportSourceApp::Unknown)
                .map(|arch| (arch, Some(path.to_path_buf())))
        } else if let Ok(bytes) = file.bytes() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("export.zip");
            parse_import_bytes(name, &bytes)
                .ok()
                .filter(|arch| arch.source_app != ImportSourceApp::Unknown)
                .map(|arch| (arch, None))
        } else {
            None
        };

        if let Some((archive, file_path)) = parsed {
            let state = ChatImportBoxState::from_archive(archive, file_path, active_peer, backend);
            box_host.push(ctx, BoxKind::ChatImport(state), colors);
            return true;
        }
    }
    false
}

/// Returns `true` if `path` has a `.zip` or `.txt` extension eligible for chat import.
#[must_use]
pub fn is_supported_import_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_supported_import_name)
}

/// Returns `true` if `file_name` ends with `.zip` or `.txt` (case-insensitive).
#[must_use]
pub fn is_supported_import_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".zip") || lower.ends_with(".txt")
}

// ---------------------------------------------------------------------------
// WhatsApp / LINE / KakaoTalk ZIP & transcript parsing
// ---------------------------------------------------------------------------

/// Parses a chat export `.zip` or `.txt` file from disk.
pub fn parse_import_file(path: &Path) -> Result<ParsedImportArchive, String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("export.zip")
        .to_owned();
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read '{name}': {e}"))?;
    parse_import_bytes(&name, &bytes)
}

/// Parses a chat export `.zip` archive or raw `.txt` transcript from memory.
pub fn parse_import_bytes(archive_name: &str, bytes: &[u8]) -> Result<ParsedImportArchive, String> {
    if bytes.is_empty() {
        return Err("Export file is empty.".to_owned());
    }
    let is_zip = bytes.len() >= 4 && &bytes[..4] == b"PK\x03\x04";
    if is_zip {
        parse_import_zip(archive_name, bytes)
    } else {
        if bytes.len() > MAX_TRANSCRIPT_BYTES {
            return Err("Chat transcript exceeds maximum supported size (64 MiB).".to_owned());
        }
        let text = decode_utf8_lossy_strict(bytes)?;
        let source_app = detect_import_source_app(archive_name, &text);
        let import_head = extract_import_head(&text);
        let line_count = text.lines().count();
        Ok(ParsedImportArchive {
            archive_name: archive_name.to_owned(),
            source_app,
            history_file_name: archive_name.to_owned(),
            history_bytes: bytes.to_vec(),
            import_head,
            line_count,
            media_files: Vec::new(),
        })
    }
}

fn parse_import_zip(archive_name: &str, bytes: &[u8]) -> Result<ParsedImportArchive, String> {
    let cursor = Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor)
        .map_err(|e| format!("Invalid ZIP archive '{archive_name}': {e}"))?;

    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "Archive contains too many entries ({}, max {MAX_ARCHIVE_ENTRIES}).",
            zip.len()
        ));
    }

    let mut text_candidates: Vec<(String, Vec<u8>)> = Vec::new();
    let mut media_files: Vec<ImportedMediaFile> = Vec::new();

    for idx in 0..zip.len() {
        let mut entry = zip
            .by_index(idx)
            .map_err(|e| format!("Failed reading ZIP entry #{idx}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let raw_name = entry
            .name()
            .map_err(|e| format!("Failed reading ZIP entry #{idx} name: {e}"))?
            .replace('\\', "/");
        if raw_name.contains("..") || raw_name.starts_with('/') || raw_name.starts_with("__MACOSX/")
        {
            continue;
        }
        let base_name = raw_name
            .rsplit('/')
            .next()
            .unwrap_or(&raw_name)
            .trim()
            .to_owned();
        if base_name.is_empty() || base_name.starts_with('.') {
            continue;
        }

        let lower = base_name.to_ascii_lowercase();
        if lower.ends_with(".txt") {
            let declared = entry.size() as usize;
            if declared > MAX_TRANSCRIPT_BYTES {
                return Err(format!("Transcript '{base_name}' exceeds 64 MiB limit."));
            }
            let mut buf = Vec::with_capacity(declared.min(1024 * 1024));
            entry
                .by_ref()
                .take((MAX_TRANSCRIPT_BYTES + 1) as u64)
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed extracting '{base_name}': {e}"))?;
            if buf.len() > MAX_TRANSCRIPT_BYTES {
                return Err(format!("Transcript '{base_name}' exceeds 64 MiB limit."));
            }
            text_candidates.push((base_name, buf));
        } else if let Some((kind, mime)) = classify_media_extension(&lower) {
            let declared = entry.size() as usize;
            if declared > MAX_MEDIA_ENTRY_BYTES {
                continue;
            }
            let mut buf = Vec::with_capacity(declared.min(256 * 1024));
            entry
                .by_ref()
                .take((MAX_MEDIA_ENTRY_BYTES + 1) as u64)
                .read_to_end(&mut buf)
                .map_err(|e| format!("Failed extracting media '{base_name}': {e}"))?;
            if buf.is_empty() || buf.len() > MAX_MEDIA_ENTRY_BYTES {
                continue;
            }
            media_files.push(ImportedMediaFile {
                file_name: base_name,
                mime_type: mime.to_owned(),
                kind,
                bytes: buf,
            });
        }
    }

    if text_candidates.is_empty() {
        return Err("No chat history .txt transcript found inside the ZIP archive.".to_owned());
    }

    // Prefer standard chat log filenames (`_chat.txt`, `WhatsApp Chat*`, `[LINE]*`, `KakaoTalk*`),
    // breaking ties by largest transcript size.
    text_candidates.sort_by(|(na, ba), (nb, bb)| {
        transcript_priority(nb)
            .cmp(&transcript_priority(na))
            .then_with(|| bb.len().cmp(&ba.len()))
    });

    let (history_file_name, history_bytes) = text_candidates.remove(0);
    let text = decode_utf8_lossy_strict(&history_bytes)?;
    let source_app = detect_import_source_app(&history_file_name, &text);
    let import_head = extract_import_head(&text);
    let line_count = text.lines().count();

    Ok(ParsedImportArchive {
        archive_name: archive_name.to_owned(),
        source_app,
        history_file_name,
        history_bytes,
        import_head,
        line_count,
        media_files,
    })
}

fn transcript_priority(file_name: &str) -> u8 {
    let lower = file_name.to_ascii_lowercase();
    if lower == "_chat.txt" {
        4
    } else if lower.contains("whatsapp") || lower.contains("[line]") || lower.contains("kakaotalk")
    {
        3
    } else if lower.contains("chat") || lower.contains("talk") {
        2
    } else {
        1
    }
}

fn decode_utf8_lossy_strict(bytes: &[u8]) -> Result<String, String> {
    let stripped = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(stripped) {
        Ok(s) => Ok(s.to_owned()),
        Err(_) => {
            // Support UTF-16LE/BE exports occasionally produced by KakaoTalk/Windows.
            if stripped.len() >= 2 && stripped[0] == 0xFF && stripped[1] == 0xFE {
                let units: Vec<u16> = stripped[2..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| u16::from_le_bytes(c))
                    .collect();
                String::from_utf16(&units).map_err(|e| format!("Invalid UTF-16LE transcript: {e}"))
            } else {
                Err("Chat history file is not valid UTF-8 text.".to_owned())
            }
        }
    }
}

/// Extracts the leading up-to-100 lines (capped at [`MAX_IMPORT_HEAD_BYTES`]) for
/// `messages.checkHistoryImport(import_head)`.
#[must_use]
pub fn extract_import_head(text: &str) -> String {
    let mut head = String::new();
    for (idx, line) in text.lines().enumerate() {
        if idx >= MAX_IMPORT_HEAD_LINES {
            break;
        }
        let add_len = line.len() + usize::from(!head.is_empty());
        if head.len() + add_len > MAX_IMPORT_HEAD_BYTES {
            break;
        }
        if !head.is_empty() {
            head.push('\n');
        }
        head.push_str(line);
    }
    head
}

/// Detects whether a transcript matches WhatsApp, LINE, or KakaoTalk export syntax.
#[must_use]
pub fn detect_import_source_app(file_name: &str, text: &str) -> ImportSourceApp {
    let lower_name = file_name.to_ascii_lowercase();
    let sample: Vec<&str> = text
        .lines()
        .map(|l| {
            l.trim_start_matches(['\u{feff}', '\u{200e}', '\u{200f}'])
                .trim()
        })
        .filter(|l| !l.is_empty())
        .take(25)
        .collect();

    // LINE signature: `[LINE] Chat history with ...` or `HH:MM\tSender\tMessage`
    if lower_name.contains("[line]")
        || sample.iter().any(|l| {
            l.starts_with("[LINE]") || (l.matches('\t').count() >= 2 && looks_like_hhmm_prefix(l))
        })
    {
        return ImportSourceApp::Line;
    }

    // KakaoTalk signature: header containing `KakaoTalk` / `Date Saved :` / `저장한 날짜` or
    // `[Sender] [HH:MM AM/PM] Text` or `--------------- YYYY... ---------------`
    if lower_name.contains("kakaotalk")
        || sample.iter().any(|l| {
            l.contains("KakaoTalk")
                || l.starts_with("Date Saved :")
                || l.starts_with("저장한 날짜")
                || (l.starts_with("--------------- ") && l.ends_with(" ---------------"))
                || looks_like_kakao_line(l)
        })
    {
        return ImportSourceApp::KakaoTalk;
    }

    // WhatsApp signature: `[DD/MM/YYYY, HH:MM:SS] Sender: Msg` or `MM/DD/YY, HH:MM - Sender: Msg`
    if lower_name == "_chat.txt"
        || lower_name.contains("whatsapp")
        || sample.iter().any(|l| looks_like_whatsapp_line(l))
    {
        return ImportSourceApp::WhatsApp;
    }

    ImportSourceApp::Unknown
}

fn looks_like_hhmm_prefix(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 5
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b':'
        && bytes[3].is_ascii_digit()
        && bytes[4].is_ascii_digit()
}

fn looks_like_kakao_line(line: &str) -> bool {
    // `[Sender] [10:15 AM] Message` or `[Sender] [오전 10:15] Message`
    if let Some(rest) = line.strip_prefix('[')
        && let Some((_, after_sender)) = rest.split_once("] [")
        && after_sender.contains("] ")
    {
        return true;
    }
    false
}

fn looks_like_whatsapp_line(line: &str) -> bool {
    // Bracketed iOS format: `[12/03/2025, 14:22:05] Name: Hello`
    if let Some(rest) = line.strip_prefix('[')
        && let Some((ts, body)) = rest.split_once("] ")
        && (ts.contains('/') || ts.contains('.') || ts.contains('-'))
        && ts.contains(':')
        && body.contains(':')
    {
        return true;
    }
    // Android format: `12/03/2025, 14:22 - Name: Hello`
    if let Some((ts, body)) = line.split_once(" - ")
        && (ts.contains('/') || ts.contains('.') || ts.contains('-'))
        && ts.contains(':')
        && !body.is_empty()
    {
        return true;
    }
    false
}

fn classify_media_extension(lower_name: &str) -> Option<(ImportedMediaKind, &'static str)> {
    let ext = lower_name.rsplit('.').next()?;
    match ext {
        "jpg" | "jpeg" => Some((ImportedMediaKind::Photo, "image/jpeg")),
        "png" => Some((ImportedMediaKind::Photo, "image/png")),
        "webp" => Some((ImportedMediaKind::Photo, "image/webp")),
        "mp4" | "m4v" => Some((ImportedMediaKind::Video, "video/mp4")),
        "mov" => Some((ImportedMediaKind::Video, "video/quicktime")),
        "webm" => Some((ImportedMediaKind::Video, "video/webm")),
        "ogg" | "opus" => Some((ImportedMediaKind::Audio, "audio/ogg")),
        "mp3" => Some((ImportedMediaKind::Audio, "audio/mpeg")),
        "m4a" => Some((ImportedMediaKind::Audio, "audio/mp4")),
        "wav" => Some((ImportedMediaKind::Audio, "audio/wav")),
        "pdf" => Some((ImportedMediaKind::Document, "application/pdf")),
        "gif" => Some((ImportedMediaKind::Document, "image/gif")),
        "tgs" => Some((ImportedMediaKind::Document, "application/x-tgsticker")),
        "vcf" => Some((ImportedMediaKind::Document, "text/vcard")),
        _ => None,
    }
}

/// Helper for building a valid synthetic export `.zip` archive in memory (used by unit
/// and live `FakeRpcClient` tests; contains only synthetic entries).
pub fn build_synthetic_export_zip(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            writer
                .start_file(*name, opts)
                .map_err(|e| format!("zip start_file '{name}': {e}"))?;
            writer
                .write_all(data)
                .map_err(|e| format!("zip write_all '{name}': {e}"))?;
        }
        writer.finish().map_err(|e| format!("zip finish: {e}"))?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_whatsapp_zip_with_media() {
        let transcript = concat!(
            "[15/01/2025, 09:30:10] User One: Hello from synthetic export!\n",
            "[15/01/2025, 09:31:02] User Two: <attached: IMG-0001.jpg>\n",
            "[15/01/2025, 09:32:44] User One: <attached: AUDIO-0001.opus>\n",
        );
        let zip_bytes = build_synthetic_export_zip(&[
            ("_chat.txt", transcript.as_bytes()),
            ("IMG-0001.jpg", &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]),
            ("AUDIO-0001.opus", b"OggSsynthetic-opus-bytes"),
            ("__MACOSX/._chat.txt", b"ignored metadata"),
        ])
        .expect("build zip");

        let parsed = parse_import_bytes("WhatsApp Chat - Sample.zip", &zip_bytes)
            .expect("parse whatsapp zip");
        assert_eq!(parsed.source_app, ImportSourceApp::WhatsApp);
        assert_eq!(parsed.history_file_name, "_chat.txt");
        assert_eq!(parsed.line_count, 3);
        assert_eq!(parsed.media_count(), 2);
        assert!(
            parsed
                .import_head
                .contains("User One: Hello from synthetic export!")
        );
        assert_eq!(parsed.media_files[0].file_name, "IMG-0001.jpg");
        assert_eq!(parsed.media_files[0].kind, ImportedMediaKind::Photo);
        assert_eq!(parsed.media_files[1].file_name, "AUDIO-0001.opus");
        assert_eq!(parsed.media_files[1].kind, ImportedMediaKind::Audio);
    }

    #[test]
    fn detects_line_and_kakaotalk_export_formats() {
        let line_log = concat!(
            "[LINE] Chat history with Project Team\n",
            "Saved on: 2025/02/10 11:00\n",
            "\n",
            "Mon, 10/02/2025\n",
            "10:15\tMember A\tDesign doc uploaded\n",
            "10:16\tMember B\tLooks good!\n",
        );
        let line_zip =
            build_synthetic_export_zip(&[("[LINE] Project Team.txt", line_log.as_bytes())])
                .expect("line zip");
        let parsed_line = parse_import_bytes("line_export.zip", &line_zip).expect("parse line");
        assert_eq!(parsed_line.source_app, ImportSourceApp::Line);
        assert_eq!(parsed_line.line_count, 6);

        let kakao_log = concat!(
            "Sample Group KakaoTalk Chats\n",
            "Date Saved : 2025-03-01 12:00:00\n",
            "--------------- Saturday, March 1, 2025 ---------------\n",
            "[Account A] [12:01 PM] Synthetic test message\n",
            "[Account B] [12:02 PM] Photo attached\n",
        );
        let kakao_zip = build_synthetic_export_zip(&[
            ("KakaoTalk_Chat.txt", kakao_log.as_bytes()),
            ("clip.mp4", b"\x00\x00\x00\x18ftypmp42"),
        ])
        .expect("kakao zip");
        let parsed_kakao = parse_import_bytes("kakao_export.zip", &kakao_zip).expect("parse kakao");
        assert_eq!(parsed_kakao.source_app, ImportSourceApp::KakaoTalk);
        assert_eq!(parsed_kakao.media_count(), 1);
        assert_eq!(parsed_kakao.media_files[0].kind, ImportedMediaKind::Video);
    }

    #[test]
    fn rejects_zip_without_transcript_and_skips_path_traversal() {
        let zip_bytes = build_synthetic_export_zip(&[
            ("../evil.txt", b"should be skipped"),
            ("photo.jpg", &[0xFF, 0xD8, 0xFF, 0xD9]),
        ])
        .expect("zip");
        let err = parse_import_bytes("bad.zip", &zip_bytes).unwrap_err();
        assert!(err.contains("No chat history .txt"));
    }

    #[test]
    fn import_head_truncates_to_line_limit() {
        let mut big = String::new();
        for i in 0..150 {
            big.push_str(&format!("[01/01/2025, 10:00:00] User A: line {i}\n"));
        }
        let head = extract_import_head(&big);
        assert_eq!(head.lines().count(), MAX_IMPORT_HEAD_LINES);
    }
}
