//! Pure `EmojiSetsManager`, safe ZIP unpacking, and `Manage Emoji Sets` modal UI.
//!
//! Port origin:
//! - `Telegram/SourceFiles/chat_helpers/emoji_sets_manager.h`
//! - `Telegram/SourceFiles/chat_helpers/emoji_sets_manager.cpp`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_storage::{UnpackOptions, UnpackReport, unpack_blob_bytes};

use crate::stand_in::theme::Palette;

/// Formats a byte count as a human-readable size string (`B`, `KB`, `MB`, `GB`).
#[must_use]
pub fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Formats duration in seconds as `M:SS`.
#[must_use]
pub fn format_duration(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Public Telegram channel username hosting downloadable desktop emoji set blobs
/// (`Ui::Emoji::kCloudLocationUsername`).
pub const EMOJI_CLOUD_LOCATION_USERNAME: &str = "tdesktop_emoji";

/// Cloud archive loader callback trait implemented by `LiveSession` so [`EmojiSetsManager`]
/// can download `@tdesktop_emoji` posts without depending on `LiveSession` or MTProto.
pub trait EmojiSetCloudLoader {
    /// Downloads the emoji set ZIP archive bytes from `@username` message `post_id`.
    fn download_emoji_set_archive(&self, username: &str, post_id: i32) -> Result<Vec<u8>, String>;

    /// Downloads, optionally SHA-256-verifies, and atomically unpacks an emoji set archive into
    /// `dest_dir` via `tg_storage::cloud_blob`.
    ///
    /// Default implementation downloads raw archive bytes via [`Self::download_emoji_set_archive`]
    /// and delegates to [`unpack_emoji_set_zip_verified`]. `LiveSession` overrides this with
    /// staged [`tg_storage::CloudBlobLoader`] streaming + atomic directory swap.
    fn download_and_unpack_emoji_set(
        &self,
        id: i32,
        username: &str,
        post_id: i32,
        size_bytes: u64,
        dest_dir: &Path,
        expected_sha256: Option<[u8; 32]>,
    ) -> Result<UnpackReport, String> {
        let _ = (id, size_bytes);
        let bytes = self.download_emoji_set_archive(username, post_id)?;
        unpack_emoji_set_zip_verified(&bytes, dest_dir, expected_sha256)
    }
}

/// Metadata describing one downloadable official emoji set (`kSets` in `emoji_sets_manager.cpp:42-47`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmojiSetDescriptor {
    /// Stable numeric set ID (`0` = Mac built-in, `1` = Android, `2` = Twemoji, `3` = JoyPixels).
    pub id: i32,
    /// Post ID inside `@tdesktop_emoji` channel (`0` for built-in set).
    pub post_id: i32,
    /// Compressed archive size in bytes (`0` for built-in set).
    pub size_bytes: u64,
    /// Human-readable display name.
    pub name: &'static str,
    /// Resource preview identifier (`set0_preview.webp` .. `set3_preview.webp`).
    pub preview_key: &'static str,
    /// Sample preview emoji glyphs rendered when bitmap preview sprites are not present.
    pub sample_glyphs: [&'static str; 4],
}

/// Official tdesktop emoji sets table (`kSets` in `chat_helpers/emoji_sets_manager.cpp:42-47`).
pub const EMOJI_SETS: [EmojiSetDescriptor; 4] = [
    EmojiSetDescriptor {
        id: 0,
        post_id: 0,
        size_bytes: 0,
        name: "Mac",
        preview_key: "set0_preview.webp",
        sample_glyphs: ["😀", "😎", "🎉", "🔥"],
    },
    EmojiSetDescriptor {
        id: 1,
        post_id: 3223,
        size_bytes: 7_854_643,
        name: "Android",
        preview_key: "set1_preview.webp",
        sample_glyphs: ["😃", "🤖", "🎯", "⚡"],
    },
    EmojiSetDescriptor {
        id: 2,
        post_id: 3224,
        size_bytes: 6_072_532,
        name: "Twemoji",
        preview_key: "set2_preview.webp",
        sample_glyphs: ["😊", "🐦", "🌟", "💬"],
    },
    EmojiSetDescriptor {
        id: 3,
        post_id: 3225,
        size_bytes: 7_805_861,
        name: "JoyPixels",
        preview_key: "set3_preview.webp",
        sample_glyphs: ["😁", "🎨", "🚀", "💎"],
    },
];

/// Download location pointing into `@tdesktop_emoji` (`MTP::DedicatedLoader::Location`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiCloudLocation {
    /// Public channel username (`"tdesktop_emoji"`).
    pub username: String,
    /// Message post ID inside the channel.
    pub post_id: i32,
}

/// Lifecycle state of one emoji set (`Storage::CloudBlob::BlobState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmojiSetState {
    /// Currently active emoji set.
    Active,
    /// Downloaded and unpacked on disk, ready to activate immediately.
    Ready,
    /// Available for download from Telegram cloud (`size_bytes`).
    Available {
        /// Archive download size in bytes.
        size_bytes: u64,
    },
    /// Download currently in progress (`already` / `size`).
    Loading {
        /// Bytes downloaded so far.
        already: u64,
        /// Total archive size in bytes.
        size: u64,
    },
    /// Download or archive unpacking failed.
    Failed(String),
}

impl EmojiSetState {
    /// Returns download progress in `0.0..=1.0` for `Loading` states (`CountProgress`).
    #[must_use]
    pub fn progress(&self) -> f64 {
        match self {
            Self::Loading { already, size } if *size > 0 => {
                (*already as f64 / *size as f64).clamp(0.0, 1.0)
            }
            Self::Active | Self::Ready => 1.0,
            _ => 0.0,
        }
    }

    /// Formats the status subtitle shown under each row (`StateDescription`).
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            Self::Active => "Active".to_owned(),
            Self::Ready => "Ready".to_owned(),
            Self::Available { size_bytes } => {
                if *size_bytes == 0 {
                    "Built-in".to_owned()
                } else {
                    format_file_size(*size_bytes)
                }
            }
            Self::Loading { already, size } => {
                let pct = if *size > 0 {
                    ((*already as f64 / *size as f64) * 100.0).round() as u32
                } else {
                    0
                };
                format!("Downloading {pct}%...")
            }
            Self::Failed(err) => {
                if err.is_empty() {
                    "Download failed".to_owned()
                } else {
                    format!("Failed: {err}")
                }
            }
        }
    }
}

/// Validates a file name inside a downloaded emoji set ZIP archive
/// (`GoodSetPartName` in `chat_helpers/emoji_sets_manager.cpp:158-161`).
#[must_use]
pub fn good_emoji_set_part_name(name: &str) -> bool {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return false;
    }
    name == "config.json" || (name.starts_with("emoji_") && name.ends_with(".webp"))
}

/// Safely unpacks an emoji-set ZIP archive into `dest_dir` via `tg_storage::cloud_blob` with
/// optional constant-time SHA-256 verification, zip-slip/symlink/size-limit hardening, and atomic
/// directory swap (`UnpackSet` -> `Storage::CloudBlob::UnpackBlob` in `emoji_sets_manager.cpp:163`).
///
/// Any verification or extraction failure leaves a pre-existing `dest_dir` completely untouched.
pub fn unpack_emoji_set_zip_verified(
    zip_bytes: &[u8],
    dest_dir: &Path,
    expected_sha256: Option<[u8; 32]>,
) -> Result<UnpackReport, String> {
    let options = UnpackOptions {
        expected_sha256,
        ..UnpackOptions::default()
    };
    unpack_blob_bytes(zip_bytes, dest_dir, &options, good_emoji_set_part_name)
        .map_err(|e| format!("Emoji set cloud blob unpack failed: {e}"))
}

/// Safely unpacks an emoji-set ZIP archive into `dest_dir`, validating every entry with
/// [`good_emoji_set_part_name`] via `tg_storage::cloud_blob` atomic directory swap
/// (`UnpackSet` in `emoji_sets_manager.cpp:163-165`).
pub fn unpack_emoji_set_zip(zip_bytes: &[u8], dest_dir: &Path) -> Result<usize, String> {
    let report = unpack_emoji_set_zip_verified(zip_bytes, dest_dir, None)?;
    Ok(report.extracted_files.len())
}

/// Action emitted by [`EmojiSetsManager::show_manage_sets_box`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManageEmojiSetsAction {
    /// User clicked a set that needs to be downloaded (`LoadAndSwitchTo`).
    DownloadAndSwitch(i32),
    /// User switched immediately to an already-ready set.
    Switched(i32),
    /// User closed the Manage Emoji Sets dialog.
    Close,
}

/// Manager for desktop emoji sets (`Ui::Emoji::ManageSetsBox` + `GlobalLoader` in
/// `chat_helpers/emoji_sets_manager.cpp`).
#[derive(Debug, Clone)]
pub struct EmojiSetsManager {
    current_set_id: i32,
    pending_switch_id: Option<i32>,
    storage_root: PathBuf,
    loading: Option<(i32, u64, u64)>,
    failed: HashMap<i32, String>,
    ready_override: HashMap<i32, bool>,
}

impl Default for EmojiSetsManager {
    fn default() -> Self {
        Self::new(std::env::temp_dir().join("rustgram_emoji_sets"))
    }
}

impl EmojiSetsManager {
    /// Creates a new [`EmojiSetsManager`] rooted at `storage_root`.
    #[must_use]
    pub fn new(storage_root: PathBuf) -> Self {
        Self {
            current_set_id: 0,
            pending_switch_id: None,
            storage_root,
            loading: None,
            failed: HashMap::new(),
            ready_override: HashMap::new(),
        }
    }

    /// Returns the static descriptor for `id`, if valid.
    #[must_use]
    pub fn descriptor(id: i32) -> Option<&'static EmojiSetDescriptor> {
        EMOJI_SETS.iter().find(|s| s.id == id)
    }

    /// Returns the `@tdesktop_emoji` cloud location for `id` (`GetDownloadLocation`).
    #[must_use]
    pub fn download_location(id: i32) -> Option<EmojiCloudLocation> {
        let desc = Self::descriptor(id)?;
        if desc.post_id <= 0 {
            return None;
        }
        Some(EmojiCloudLocation {
            username: EMOJI_CLOUD_LOCATION_USERNAME.to_owned(),
            post_id: desc.post_id,
        })
    }

    /// Returns the currently active emoji set ID (`CurrentSetId`).
    #[must_use]
    pub const fn current_set_id(&self) -> i32 {
        self.current_set_id
    }

    /// Returns the root directory storing unpacked emoji sets.
    #[must_use]
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// Returns the on-disk unpacked folder for set `id` (`internal::SetDataPath`).
    #[must_use]
    pub fn set_data_path(&self, id: i32) -> PathBuf {
        self.storage_root.join(format!("set_{id}"))
    }

    /// Returns `true` if set `id` is built-in (`id == 0`) or unpacked on disk (`SetIsReady`).
    #[must_use]
    pub fn is_set_ready(&self, id: i32) -> bool {
        if id == 0 {
            return true;
        }
        if let Some(&ready) = self.ready_override.get(&id) {
            return ready;
        }
        self.set_data_path(id).join("config.json").is_file()
    }

    /// Computes the current [`EmojiSetState`] for `id` (`ComputeState`).
    #[must_use]
    pub fn state_for(&self, id: i32) -> EmojiSetState {
        if let Some((loading_id, already, size)) = self.loading
            && loading_id == id
        {
            return EmojiSetState::Loading { already, size };
        }
        if let Some(err) = self.failed.get(&id) {
            return EmojiSetState::Failed(err.clone());
        }
        if id == self.current_set_id {
            return EmojiSetState::Active;
        }
        if self.is_set_ready(id) {
            return EmojiSetState::Ready;
        }
        let size_bytes = Self::descriptor(id).map_or(0, |d| d.size_bytes);
        EmojiSetState::Available { size_bytes }
    }

    /// Marks `id` as currently downloading with progress `already / total`.
    pub fn begin_loading(&mut self, id: i32, already: u64, total: u64) {
        self.failed.remove(&id);
        self.pending_switch_id = Some(id);
        self.loading = Some((id, already, total));
    }

    /// Updates download progress for the currently active loader.
    pub fn update_loading_progress(&mut self, id: i32, already: u64, total: u64) {
        if self.loading.is_some_and(|(cur, _, _)| cur == id) {
            self.loading = Some((id, already, total));
        }
    }

    /// Records a download or unpack failure for `id` (`Loader::fail` / `ClearNeedSwitchToId`).
    pub fn mark_failed(&mut self, id: i32, error: impl Into<String>) {
        if self.loading.is_some_and(|(cur, _, _)| cur == id) {
            self.loading = None;
        }
        if self.pending_switch_id == Some(id) {
            self.pending_switch_id = None;
        }
        self.failed.insert(id, error.into());
    }

    /// Switches immediately to `id` if it is ready (`SwitchToSet`). Returns `true` on success.
    pub fn switch_to_set(&mut self, id: i32) -> bool {
        if Self::descriptor(id).is_none() || !self.is_set_ready(id) {
            return false;
        }
        self.current_set_id = id;
        self.pending_switch_id = None;
        self.failed.remove(&id);
        if self.loading.is_some_and(|(cur, _, _)| cur == id) {
            self.loading = None;
        }
        true
    }

    /// Unpacks downloaded ZIP bytes for `id` into [`Self::set_data_path`] via
    /// `tg_storage::cloud_blob` (optionally verifying `expected_sha256`) and activates it.
    pub fn finish_download_and_switch_verified(
        &mut self,
        id: i32,
        zip_bytes: &[u8],
        expected_sha256: Option<[u8; 32]>,
    ) -> Result<UnpackReport, String> {
        if Self::descriptor(id).is_none() {
            self.pending_switch_id = None;
            return Err(format!("Unknown emoji set id {id}"));
        }
        let dest = self.set_data_path(id);
        match unpack_emoji_set_zip_verified(zip_bytes, &dest, expected_sha256) {
            Ok(report) => {
                self.ready_override.insert(id, true);
                self.current_set_id = id;
                self.pending_switch_id = None;
                self.loading = None;
                self.failed.remove(&id);
                Ok(report)
            }
            Err(err) => {
                self.mark_failed(id, err.clone());
                Err(err)
            }
        }
    }

    /// Unpacks downloaded ZIP bytes for `id` into [`Self::set_data_path`] and activates it
    /// (`Loader::unpack` -> `SwitchToSet`).
    pub fn finish_download_and_switch(
        &mut self,
        id: i32,
        zip_bytes: &[u8],
    ) -> Result<usize, String> {
        self.finish_download_and_switch_verified(id, zip_bytes, None)
            .map(|report| report.extracted_files.len())
    }

    /// Downloads emoji set `id` from `@tdesktop_emoji` post `post_id` via `loader` using
    /// `tg_storage::cloud_blob` staged verification + atomic directory swap, optionally enforcing
    /// `expected_sha256`, and switches to it on completion.
    pub fn load_and_switch_to_verified<L: EmojiSetCloudLoader + ?Sized>(
        &mut self,
        loader: &L,
        id: i32,
        expected_sha256: Option<[u8; 32]>,
        force_redownload: bool,
    ) -> Result<UnpackReport, String> {
        let Some(desc) = Self::descriptor(id) else {
            self.pending_switch_id = None;
            return Err(format!("Unknown emoji set id {id}"));
        };
        if !force_redownload && (id == 0 || self.is_set_ready(id)) {
            self.switch_to_set(id);
            return Ok(UnpackReport {
                target_dir: self.set_data_path(id),
                sha256: [0u8; 32],
                extracted_files: Vec::new(),
                total_uncompressed_bytes: 0,
            });
        }
        let Some(loc) = Self::download_location(id) else {
            return Err(format!("Emoji set {id} has no cloud location"));
        };

        self.begin_loading(id, 0, desc.size_bytes);
        let dest = self.set_data_path(id);

        match loader.download_and_unpack_emoji_set(
            id,
            &loc.username,
            loc.post_id,
            desc.size_bytes,
            &dest,
            expected_sha256,
        ) {
            Ok(report) => {
                self.ready_override.insert(id, true);
                self.current_set_id = id;
                self.pending_switch_id = None;
                self.loading = None;
                self.failed.remove(&id);
                Ok(report)
            }
            Err(err) => {
                self.mark_failed(id, err.clone());
                Err(err)
            }
        }
    }

    /// Downloads emoji set `id` from `@tdesktop_emoji` post `post_id` via `loader`
    /// (`contacts.resolveUsername` -> `channels.getMessages` -> `upload.getFile`) and switches
    /// to it on completion (`Ui::Emoji::LoadAndSwitchTo`).
    pub fn load_and_switch_to<L: EmojiSetCloudLoader + ?Sized>(
        &mut self,
        loader: &L,
        id: i32,
    ) -> Result<usize, String> {
        self.load_and_switch_to_verified(loader, id, None, false)
            .map(|report| report.extracted_files.len())
    }

    /// Renders the `Manage Emoji Sets` modal box (`Ui::Emoji::ManageSetsBox`).
    pub fn show_manage_sets_box(
        &mut self,
        ui: &mut Ui,
        palette: &Palette,
    ) -> Option<ManageEmojiSetsAction> {
        let mut action = None;
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("Emoji Sets")
                    .size(16.0)
                    .strong()
                    .color(palette.window_bold_fg),
            );
            ui.add_space(6.0);

            for set in &EMOJI_SETS {
                let state = self.state_for(set.id);
                let row_h = 56.0;
                let row_w = ui.available_width().max(260.0);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(row_w, row_h), Sense::click());
                let clickable = matches!(
                    state,
                    EmojiSetState::Ready
                        | EmojiSetState::Available { .. }
                        | EmojiSetState::Failed(_)
                );
                if resp.hovered() && clickable {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let bg = if resp.hovered() && clickable {
                    palette.window_bg_over
                } else {
                    palette.window_bg
                };
                ui.painter().rect_filled(rect, CornerRadius::same(8), bg);

                // 2x2 emoji preview tile (`Row::paintPreview`).
                let preview_rect = Rect::from_min_size(
                    Pos2::new(rect.left() + 12.0, rect.center().y - 20.0),
                    Vec2::splat(40.0),
                );
                ui.painter().rect_filled(
                    preview_rect,
                    CornerRadius::same(8),
                    palette.window_bg_over,
                );
                for (i, glyph) in set.sample_glyphs.iter().enumerate() {
                    let gx = preview_rect.left() + 10.0 + (i % 2) as f32 * 20.0;
                    let gy = preview_rect.top() + 10.0 + (i / 2) as f32 * 20.0;
                    ui.painter().text(
                        Pos2::new(gx, gy),
                        Align2::CENTER_CENTER,
                        *glyph,
                        FontId::proportional(14.0),
                        palette.window_fg,
                    );
                }

                // Set title + status subtitle (`Row::setupLabels`).
                let text_x = preview_rect.right() + 12.0;
                ui.painter().text(
                    Pos2::new(text_x, rect.top() + 12.0),
                    Align2::LEFT_TOP,
                    set.name,
                    FontId::proportional(14.0),
                    palette.window_bold_fg,
                );
                let status_col = match state {
                    EmojiSetState::Active => palette.window_active_text_fg,
                    EmojiSetState::Failed(_) => Color32::from_rgb(0xD9, 0x48, 0x48),
                    _ => palette.window_sub_text_fg,
                };
                ui.painter().text(
                    Pos2::new(text_x, rect.top() + 32.0),
                    Align2::LEFT_TOP,
                    state.description(),
                    FontId::proportional(12.0),
                    status_col,
                );

                // Right radio / radial progress indicator (`Row::paintRadio`).
                let radio_center = Pos2::new(rect.right() - 24.0, rect.center().y);
                let radio_r = 9.0;
                let active_stroke = match state {
                    EmojiSetState::Active | EmojiSetState::Loading { .. } => {
                        palette.window_bg_active
                    }
                    _ => palette.window_sub_text_fg,
                };
                ui.painter()
                    .circle_stroke(radio_center, radio_r, Stroke::new(2.0, active_stroke));
                match state {
                    EmojiSetState::Active => {
                        ui.painter()
                            .circle_filled(radio_center, 4.5, palette.window_bg_active);
                    }
                    EmojiSetState::Loading { .. } => {
                        let frac = state.progress().max(0.15) as f32;
                        let segments = 16usize;
                        let count = ((segments as f32 * frac).ceil() as usize).max(2);
                        let mut pts = Vec::with_capacity(count + 1);
                        for step in 0..=count {
                            let angle = -std::f32::consts::FRAC_PI_2
                                + (step as f32 / segments as f32) * std::f32::consts::TAU;
                            pts.push(Pos2::new(
                                radio_center.x + angle.cos() * (radio_r - 2.0),
                                radio_center.y + angle.sin() * (radio_r - 2.0),
                            ));
                        }
                        ui.painter().add(egui::Shape::line(
                            pts,
                            Stroke::new(2.2, palette.window_bg_active),
                        ));
                    }
                    _ => {}
                }

                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::Role::RadioButton,
                        matches!(state, EmojiSetState::Active),
                        set.name.to_owned(),
                    )
                });

                if resp.clicked() && clickable {
                    if self.is_set_ready(set.id) {
                        if self.switch_to_set(set.id) {
                            action = Some(ManageEmojiSetsAction::Switched(set.id));
                        }
                    } else {
                        self.begin_loading(set.id, 0, set.size_bytes);
                        action = Some(ManageEmojiSetsAction::DownloadAndSwitch(set.id));
                    }
                }
            }

            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    action = Some(ManageEmojiSetsAction::Close);
                }
            });
        });
        action
    }
}

/// Convenience wrapper for rendering a [`ManageSetsBox`](EmojiSetsManager::show_manage_sets_box)
/// inside a framed egui window.
pub fn show_emoji_sets_modal_window(
    ctx: &egui::Context,
    manager: &mut EmojiSetsManager,
    palette: &Palette,
) -> Option<ManageEmojiSetsAction> {
    let mut result = None;
    egui::Window::new("Manage Emoji Sets")
        .id(Id::unique("tg_manage_emoji_sets_box"))
        .collapsible(false)
        .resizable(false)
        .fixed_size(Vec2::new(360.0, 320.0))
        .show(ctx, |ui| {
            result = manager.show_manage_sets_box(ui, palette);
        });
    result
}
