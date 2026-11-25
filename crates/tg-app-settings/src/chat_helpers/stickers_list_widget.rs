//! Stickers picker widget with animated `tg-lottie` previews, recent/favorite sections,
//! emoji/keyword search, premium-locked badges, and bottom pack icon strip.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/stickers_list_widget.h`,
//! `Telegram/SourceFiles/chat_helpers/stickers_list_widget.cpp`, and `stickers_list_footer.cpp`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Key, Margin, Pos2, Rect, ScrollArea, Sense,
    Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_chat::stickers::{FAVED_SET_ID, RECENT_SET_ID};
use tg_chat::{DocumentId, StickerSetId};
use tg_lang::tr;
use tg_lottie::{Composition, FrameBuffer, Size as LottieSize, render_composition_into};
use tg_ui::fonts::font_id_from_style;
use tg_ui::tg_style::st;

use crate::chat_helpers::backend::{
    CLOUD_SEARCH_PAGE_LIMIT, ChatHelpersBackend, OLD_FEATURED_LOAD_LIMIT, SearchStickersRequest,
    StickerMedia,
};
use crate::chat_helpers::emoji_list_widget::{
    EMOJI_FOOTER_HEIGHT, EMOJI_HEADER_HEIGHT, EMOJI_MOOD_GROUPS, MoodIconCache, emoji_list_margin,
    footer_slot_rect, list_rect_below_search, paint_footer_background, paint_footer_icon,
    paint_footer_selection, render_tabbed_search, theme_color,
};
use crate::chat_helpers::gift_box_pack::{GiftBoxPack, GiftBoxPackKind};
use crate::chat_helpers::paint_helper_icon;
use tg_app_core::stand_in::theme::Palette;

/// Sticker grid columns in the 345px panel (`floor((345 - 22) / 64) = 5`).
pub const STICKER_COLUMNS: usize = 5;
/// Sticker cell side matching `st::stickerPanSize`.
pub const STICKER_CELL_SIZE: f32 = st::sticker_pan_size.width.0 as f32;
/// Rasterized preview resolution for animated Lottie stickers in the grid.
pub const LOTTIE_PREVIEW_PX: u32 = 96;

/// Events emitted by [`StickersListWidget::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StickersListEvent {
    /// User selected a sticker to send.
    Chosen {
        /// Selected sticker document ID.
        document_id: DocumentId,
        /// Owning sticker set ID, if known.
        set_id: Option<StickerSetId>,
    },
    /// User clicked a Premium-locked sticker without an active Premium subscription.
    PremiumRequired(DocumentId),
    /// User installed a featured/trending sticker pack.
    SetInstalled(StickerSetId),
    /// User uninstalled a sticker pack.
    SetRemoved(StickerSetId),
    /// User cleared recent stickers (`messages.clearRecentStickers`).
    RecentCleared,
}

/// Maximum number of new `ChatHelpersBackend::sticker_media` lookups started per UI frame.
///
/// A live backend may have to fetch a sticker's file before it can return it; spreading new
/// lookups over frames keeps opening the panel responsive (the rest show placeholders first,
/// like tdesktop's path-thumbnail placeholders while `DocumentMedia` loads). Only first
/// lookups count: re-checking a sticker whose download is already running is a cache check, so
/// stickers still loading never hold back the lookups of the cells after them.
pub const STICKER_MEDIA_FETCHES_PER_FRAME: usize = 2;

thread_local! {
    static LOTTIE_WORK_ON_THREAD: Cell<u64> = const { Cell::new(0) };
}

/// Number of Lottie parse or rasterize operations run so far on the calling thread.
///
/// The panel's UI thread must stay at zero: tdesktop renders Lottie frames on background
/// workers (`Lottie::SinglePlayer` / `crl::async`) and the UI only paints finished frames.
#[must_use]
pub fn lottie_work_on_current_thread() -> u64 {
    LOTTIE_WORK_ON_THREAD.with(Cell::get)
}

fn note_lottie_work() {
    LOTTIE_WORK_ON_THREAD.with(|c| c.set(c.get().saturating_add(1)));
}

enum LottieJob {
    Parse {
        doc_id: DocumentId,
        media: StickerMedia,
    },
    Render {
        doc_id: DocumentId,
        composition: Arc<Composition>,
        frame_idx: u32,
        size: [u32; 2],
    },
}

enum LottieResult {
    Parsed {
        doc_id: DocumentId,
        composition: Arc<Composition>,
    },
    Frame {
        doc_id: DocumentId,
        frame_idx: u32,
        image: egui::ColorImage,
    },
    Failed {
        doc_id: DocumentId,
    },
}

fn parse_lottie_media(media: &StickerMedia) -> Option<Composition> {
    match media {
        StickerMedia::LottieJson(json) => Composition::from_json(json).ok(),
        StickerMedia::LottieTgs(bytes) => {
            let raw = tg_lottie::unpack_gzip(bytes).ok()?;
            let s = std::str::from_utf8(&raw).ok()?;
            Composition::from_json(s).ok()
        }
        StickerMedia::StaticRgba { .. } => None,
    }
}

fn run_lottie_job(job: LottieJob) -> LottieResult {
    note_lottie_work();
    match job {
        LottieJob::Parse { doc_id, media } => match parse_lottie_media(&media) {
            Some(comp) => LottieResult::Parsed {
                doc_id,
                composition: Arc::new(comp),
            },
            None => LottieResult::Failed { doc_id },
        },
        LottieJob::Render {
            doc_id,
            composition,
            frame_idx,
            size,
        } => {
            let mut buffer = FrameBuffer::new(LottieSize::new(size[0], size[1]));
            let frame_f = composition.in_point + f64::from(frame_idx);
            render_composition_into(&composition, frame_f, &mut buffer);
            // `tg-lottie` renders premultiplied RGBA8 (via tiny-skia).
            let image = egui::ColorImage::from_rgba_premultiplied(
                [buffer.width as usize, buffer.height as usize],
                &buffer.data,
            );
            LottieResult::Frame {
                doc_id,
                frame_idx,
                image,
            }
        }
    }
}

/// Background Lottie workers: parse `.tgs` / JSON and rasterize frames off the UI thread.
struct LottieWorkers {
    jobs: Sender<LottieJob>,
    results: Receiver<LottieResult>,
}

impl LottieWorkers {
    fn spawn(ctx: &egui::Context) -> Option<Self> {
        let (job_tx, job_rx) = channel::<LottieJob>();
        let (res_tx, res_rx) = channel::<LottieResult>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let count = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 4));
        let mut spawned = 0usize;
        for idx in 0..count {
            let job_rx = Arc::clone(&job_rx);
            let res_tx = res_tx.clone();
            let ctx = ctx.clone();
            let started = std::thread::Builder::new()
                .name(format!("rustgram-lottie-{idx}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let Ok(rx) = job_rx.lock() else {
                                return;
                            };
                            match rx.recv() {
                                Ok(job) => job,
                                Err(_) => return,
                            }
                        };
                        if res_tx.send(run_lottie_job(job)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                });
            if started.is_ok() {
                spawned += 1;
            }
        }
        (spawned > 0).then_some(Self {
            jobs: job_tx,
            results: res_rx,
        })
    }
}

struct LottiePreviewSlot {
    composition: Option<Arc<Composition>>,
    size: [u32; 2],
    shown_frame: Option<u32>,
    in_flight: bool,
    texture: Option<TextureHandle>,
}

/// Fits a `width x height` source into a `max_side` square, keeping its aspect ratio.
fn fit_preview_size(width: u32, height: u32, max_side: u32) -> [u32; 2] {
    if width == 0 || height == 0 {
        return [max_side, max_side];
    }
    let longest = width.max(height) as f32;
    let w = ((width as f32 / longest) * max_side as f32)
        .round()
        .max(1.0) as u32;
    let h = ((height as f32 / longest) * max_side as f32)
        .round()
        .max(1.0) as u32;
    [w, h]
}

/// Returns `rect`'s largest centered sub-rect with the aspect ratio of `size`.
#[must_use]
pub fn fit_rect_keep_aspect(rect: Rect, size: Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return rect;
    }
    let scale = (rect.width() / size.x).min(rect.height() / size.y);
    Rect::from_center_size(rect.center(), size * scale)
}

/// Sticker preview texture cache. Static images are uploaded directly; Lottie stickers are
/// parsed and rasterized by background workers, and the UI thread only uploads finished frames.
#[derive(Default)]
pub struct StickerMediaCache {
    static_textures: HashMap<DocumentId, TextureHandle>,
    lottie_slots: HashMap<DocumentId, LottiePreviewSlot>,
    failed: HashMap<DocumentId, ()>,
    /// Stickers whose first lookup returned `None` (the backend is still loading them).
    pending: HashSet<DocumentId>,
    workers: Option<LottieWorkers>,
    workers_unavailable: bool,
    fetch_pass: u64,
    fetches_this_pass: usize,
    frames_uploaded: u64,
}

impl std::fmt::Debug for StickerMediaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StickerMediaCache")
            .field("static_count", &self.static_textures.len())
            .field("lottie_count", &self.lottie_slots.len())
            .field("frames_uploaded", &self.frames_uploaded)
            .finish_non_exhaustive()
    }
}

impl StickerMediaCache {
    /// Creates an empty media cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of Lottie frames received from the background workers and uploaded so far.
    #[must_use]
    pub const fn lottie_frames_uploaded(&self) -> u64 {
        self.frames_uploaded
    }

    /// Number of Lottie stickers still waiting for their first frame.
    #[must_use]
    pub fn pending_lottie_count(&self) -> usize {
        self.lottie_slots
            .values()
            .filter(|s| s.texture.is_none())
            .count()
    }

    /// Returns an `egui::TextureId` for `doc_id` at animation timestamp `time_secs`.
    ///
    /// Returns `(texture_id, is_animated)`; `None` while the preview is still being prepared.
    pub fn texture_for(
        &mut self,
        ctx: &egui::Context,
        doc_id: DocumentId,
        time_secs: f64,
        backend: &dyn ChatHelpersBackend,
    ) -> Option<(egui::TextureId, bool)> {
        self.texture_for_sized(ctx, doc_id, time_secs, backend)
            .map(|(id, _, animated)| (id, animated))
    }

    /// Like [`Self::texture_for`], also returning the texture's pixel size (for aspect-correct
    /// drawing).
    pub fn texture_for_sized(
        &mut self,
        ctx: &egui::Context,
        doc_id: DocumentId,
        time_secs: f64,
        backend: &dyn ChatHelpersBackend,
    ) -> Option<(egui::TextureId, Vec2, bool)> {
        self.texture_with(ctx, doc_id, time_secs, |id| backend.sticker_media(id))
    }

    fn texture_with(
        &mut self,
        ctx: &egui::Context,
        doc_id: DocumentId,
        time_secs: f64,
        fetch: impl FnOnce(DocumentId) -> Option<StickerMedia>,
    ) -> Option<(egui::TextureId, Vec2, bool)> {
        self.poll_workers(ctx);
        if let Some(tex) = self.static_textures.get(&doc_id) {
            return Some((tex.id(), tex.size_vec2(), false));
        }
        if self.lottie_slots.contains_key(&doc_id) {
            self.request_lottie_frame(doc_id, time_secs);
            return self
                .lottie_slots
                .get(&doc_id)
                .and_then(|slot| slot.texture.as_ref())
                .map(|tex| (tex.id(), tex.size_vec2(), true));
        }
        if self.failed.contains_key(&doc_id) {
            return None;
        }

        let pass = ctx.cumulative_pass_nr();
        if pass != self.fetch_pass {
            self.fetch_pass = pass;
            self.fetches_this_pass = 0;
        }
        if !self.pending.contains(&doc_id) {
            if self.fetches_this_pass >= STICKER_MEDIA_FETCHES_PER_FRAME {
                ctx.request_repaint();
                return None;
            }
            self.fetches_this_pass += 1;
        }

        let Some(media) = fetch(doc_id) else {
            self.pending.insert(doc_id);
            return None;
        };
        self.pending.remove(&doc_id);
        match media {
            StickerMedia::StaticRgba {
                width,
                height,
                rgba,
            } => {
                let expected = (width as usize)
                    .saturating_mul(height as usize)
                    .saturating_mul(4);
                if width == 0 || height == 0 || rgba.len() != expected {
                    self.failed.insert(doc_id, ());
                    return None;
                }
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
                    &rgba,
                );
                let handle = ctx.load_texture(
                    format!("tg_sticker_static_{}", doc_id.0),
                    img,
                    TextureOptions::LINEAR,
                );
                let out = (handle.id(), handle.size_vec2(), false);
                self.static_textures.insert(doc_id, handle);
                Some(out)
            }
            media @ (StickerMedia::LottieJson(_) | StickerMedia::LottieTgs(_)) => {
                if self.workers.is_none() && !self.workers_unavailable {
                    self.workers = LottieWorkers::spawn(ctx);
                    self.workers_unavailable = self.workers.is_none();
                }
                let sent = self
                    .workers
                    .as_ref()
                    .is_some_and(|w| w.jobs.send(LottieJob::Parse { doc_id, media }).is_ok());
                if sent {
                    self.lottie_slots.insert(
                        doc_id,
                        LottiePreviewSlot {
                            composition: None,
                            size: [LOTTIE_PREVIEW_PX, LOTTIE_PREVIEW_PX],
                            shown_frame: None,
                            in_flight: true,
                            texture: None,
                        },
                    );
                } else {
                    self.failed.insert(doc_id, ());
                }
                None
            }
        }
    }

    fn request_lottie_frame(&mut self, doc_id: DocumentId, time_secs: f64) {
        let Some(workers) = self.workers.as_ref() else {
            return;
        };
        let Some(slot) = self.lottie_slots.get_mut(&doc_id) else {
            return;
        };
        if slot.in_flight {
            return;
        }
        let Some(comp) = slot.composition.as_ref() else {
            return;
        };
        let total_frames = (comp.out_point - comp.in_point).max(1.0);
        let fps = comp.frame_rate.clamp(1.0, 60.0);
        let frame_idx =
            ((time_secs.max(0.0) * fps) as u64 % (total_frames.round() as u64).max(1)) as u32;
        if slot.shown_frame == Some(frame_idx) {
            return;
        }
        let job = LottieJob::Render {
            doc_id,
            composition: Arc::clone(comp),
            frame_idx,
            size: slot.size,
        };
        if workers.jobs.send(job).is_ok() {
            slot.in_flight = true;
        }
    }

    /// Takes finished parse / frame results from the workers and uploads frames to textures.
    fn poll_workers(&mut self, ctx: &egui::Context) {
        let Some(workers) = self.workers.as_ref() else {
            return;
        };
        let results: Vec<LottieResult> = workers.results.try_iter().collect();
        for result in results {
            match result {
                LottieResult::Parsed {
                    doc_id,
                    composition,
                } => {
                    if let Some(slot) = self.lottie_slots.get_mut(&doc_id) {
                        slot.size = fit_preview_size(
                            composition.width,
                            composition.height,
                            LOTTIE_PREVIEW_PX,
                        );
                        slot.composition = Some(composition);
                        slot.in_flight = false;
                    }
                }
                LottieResult::Frame {
                    doc_id,
                    frame_idx,
                    image,
                } => {
                    let Some(slot) = self.lottie_slots.get_mut(&doc_id) else {
                        continue;
                    };
                    if let Some(handle) = slot.texture.as_mut() {
                        handle.set(image, TextureOptions::LINEAR);
                    } else {
                        slot.texture = Some(ctx.load_texture(
                            format!("tg_sticker_lottie_{}", doc_id.0),
                            image,
                            TextureOptions::LINEAR,
                        ));
                    }
                    slot.shown_frame = Some(frame_idx);
                    slot.in_flight = false;
                    self.frames_uploaded += 1;
                }
                LottieResult::Failed { doc_id } => {
                    self.lottie_slots.remove(&doc_id);
                    self.failed.insert(doc_id, ());
                }
            }
        }
    }
}

/// Stateful Stickers tab widget (`StickersListWidget`).
#[derive(Debug, Default)]
pub struct StickersListWidget {
    search_query: String,
    active_set_id: Option<StickerSetId>,
    scroll_to_set: Option<StickerSetId>,
    focused_index: Option<usize>,
    media_cache: StickerMediaCache,
    gift_box_pack: GiftBoxPack,
    gift_box_kind: GiftBoxPackKind,
    // Clear recent stickers confirmation (`messages.clearRecentStickers`)
    confirm_clear_recent: bool,
    clear_recent_error: Option<String>,
    // Remote sticker & emoji-pack search cache + pagination (`messages.searchStickers` /
    // `messages.searchEmojiStickerSets`)
    last_remote_query: String,
    remote_search_stickers: Vec<DocumentId>,
    remote_search_next_offset: Option<i32>,
    remote_emoji_sets: Vec<StickerSetId>,
    remote_search_error: Option<String>,
    // Older featured sticker packs pagination (`messages.getOldFeaturedStickers`)
    old_featured_set_ids: Vec<StickerSetId>,
    old_featured_next_offset: i32,
    old_featured_total_count: Option<i32>,
    old_featured_exhausted: bool,
    old_featured_error: Option<String>,
    columns: usize,
    // Search row emoji group + header remove-set confirmation + footer settings button.
    active_group: Option<usize>,
    mood_icons: MoodIconCache,
    confirm_remove_set: Option<StickerSetId>,
    settings_requested: bool,
}

impl StickersListWidget {
    /// Creates a new [`StickersListWidget`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the gift-box sticker pack resolver (`Stickers::GiftBoxPack`).
    #[must_use]
    pub fn gift_box_pack(&self) -> &GiftBoxPack {
        &self.gift_box_pack
    }

    /// Returns mutable access to the gift-box sticker pack resolver (`Stickers::GiftBoxPack`).
    pub fn gift_box_pack_mut(&mut self) -> &mut GiftBoxPack {
        &mut self.gift_box_pack
    }

    /// Returns the active gift-box ribbon mode (Premium/Stars vs TON).
    #[must_use]
    pub const fn gift_box_kind(&self) -> GiftBoxPackKind {
        self.gift_box_kind
    }

    /// Switches the gift-box sticker ribbon between Premium/Stars and TON tiers.
    pub fn set_gift_box_kind(&mut self, kind: GiftBoxPackKind) {
        self.gift_box_kind = kind;
    }

    /// Returns the current search query.
    #[must_use]
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Sets the search query programmatically.
    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.search_query = query.into();
        self.active_group = None;
        self.focused_index = None;
        if self.search_query.trim().is_empty() {
            self.clear_remote_search_state();
        }
    }

    /// Selects (or clears) an emoji group of the search row; stickers are then filtered by
    /// the group's emoji like tdesktop's `TabbedSearch` group buttons.
    pub fn set_active_group(&mut self, group: Option<usize>) {
        self.active_group = group.filter(|&g| g < EMOJI_MOOD_GROUPS.len());
        self.search_query.clear();
        self.focused_index = None;
        self.clear_remote_search_state();
    }

    /// Returns the selected emoji group of the search row, if any.
    #[must_use]
    pub const fn active_group(&self) -> Option<usize> {
        self.active_group
    }

    /// Returns `true` once after the footer's sticker settings button was clicked
    /// (the host opens the installed sticker sets box, `StickersBox`).
    pub fn take_settings_requested(&mut self) -> bool {
        std::mem::take(&mut self.settings_requested)
    }

    /// Query used for searching: the typed text, or the selected emoji group's icon emoji.
    fn effective_query(&self) -> String {
        let typed = self.search_query.trim();
        if !typed.is_empty() {
            return typed.to_owned();
        }
        self.active_group
            .and_then(|g| EMOJI_MOOD_GROUPS.get(g))
            .map(|g| g.icon.to_owned())
            .unwrap_or_default()
    }

    /// Returns whether the inline "Clear Recent Stickers" confirmation strip is open.
    #[must_use]
    pub const fn is_clear_recent_confirm_open(&self) -> bool {
        self.confirm_clear_recent
    }

    /// Opens or closes the inline "Clear Recent Stickers" confirmation strip.
    pub fn set_clear_recent_confirm_open(&mut self, open: bool) {
        self.confirm_clear_recent = open;
        if !open {
            self.clear_recent_error = None;
        }
    }

    /// Returns the currently loaded remote sticker search hits.
    #[must_use]
    pub fn remote_search_stickers(&self) -> &[DocumentId] {
        &self.remote_search_stickers
    }

    /// Returns the next remote sticker search pagination offset, if more pages exist.
    #[must_use]
    pub const fn remote_search_next_offset(&self) -> Option<i32> {
        self.remote_search_next_offset
    }

    /// Returns the currently loaded remote custom-emoji sticker set IDs for the active query.
    #[must_use]
    pub fn remote_emoji_sets(&self) -> &[StickerSetId] {
        &self.remote_emoji_sets
    }

    /// Returns the currently loaded older featured sticker set IDs.
    #[must_use]
    pub fn old_featured_set_ids(&self) -> &[StickerSetId] {
        &self.old_featured_set_ids
    }

    /// Returns whether older featured sticker pagination has reached the end.
    #[must_use]
    pub const fn old_featured_exhausted(&self) -> bool {
        self.old_featured_exhausted
    }

    fn clear_remote_search_state(&mut self) {
        self.last_remote_query.clear();
        self.remote_search_stickers.clear();
        self.remote_search_next_offset = None;
        self.remote_emoji_sets.clear();
        self.remote_search_error = None;
    }

    /// Fetches the initial remote search page (stickers + custom-emoji sets) if `query` changed.
    fn sync_remote_search(&mut self, query: &str, backend: &dyn ChatHelpersBackend) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            self.clear_remote_search_state();
            return;
        }
        if self.last_remote_query == trimmed {
            return;
        }
        self.last_remote_query = trimmed.to_owned();
        self.remote_search_stickers.clear();
        self.remote_search_next_offset = None;
        self.remote_emoji_sets.clear();
        self.remote_search_error = None;

        let req = SearchStickersRequest {
            emojis: false,
            query: trimmed.to_owned(),
            emoticon: String::new(),
            lang_codes: vec!["en".to_owned()],
            offset: 0,
            limit: CLOUD_SEARCH_PAGE_LIMIT,
        };
        match backend.search_stickers_remote(&req) {
            Ok(page) => {
                self.remote_search_stickers = page.stickers;
                self.remote_search_next_offset = page.next_offset;
            }
            Err(err) => {
                self.remote_search_error = Some(err);
            }
        }
        match backend.search_emoji_sticker_sets(trimmed, false) {
            Ok(res) => {
                self.remote_emoji_sets = res.set_ids;
            }
            Err(err) => {
                if self.remote_search_error.is_none() {
                    self.remote_search_error = Some(err);
                }
            }
        }
    }

    /// Fetches the next page of remote sticker search results via `messages.searchStickers`.
    pub fn load_more_remote_search(&mut self, backend: &dyn ChatHelpersBackend) {
        let Some(offset) = self.remote_search_next_offset else {
            return;
        };
        let query = self.effective_query();
        let trimmed = query.as_str();
        if trimmed.is_empty() {
            return;
        }
        let req = SearchStickersRequest {
            emojis: false,
            query: trimmed.to_owned(),
            emoticon: String::new(),
            lang_codes: vec!["en".to_owned()],
            offset,
            limit: CLOUD_SEARCH_PAGE_LIMIT,
        };
        match backend.search_stickers_remote(&req) {
            Ok(page) => {
                for id in page.stickers {
                    if !self.remote_search_stickers.contains(&id) {
                        self.remote_search_stickers.push(id);
                    }
                }
                self.remote_search_next_offset = page.next_offset;
                self.remote_search_error = None;
            }
            Err(err) => {
                self.remote_search_error = Some(err);
            }
        }
    }

    /// Loads the next page of older featured sticker sets via `messages.getOldFeaturedStickers`.
    pub fn load_more_old_featured(&mut self, backend: &dyn ChatHelpersBackend) {
        match backend
            .load_old_featured_stickers(self.old_featured_next_offset, OLD_FEATURED_LOAD_LIMIT)
        {
            Ok(page) => {
                for id in &page.set_ids {
                    if !self.old_featured_set_ids.contains(id) {
                        self.old_featured_set_ids.push(*id);
                    }
                }
                let progressed = page.next_offset > self.old_featured_next_offset;
                self.old_featured_next_offset = page.next_offset;
                self.old_featured_total_count = Some(page.total_count);
                self.old_featured_exhausted = page.set_ids.is_empty()
                    || !progressed
                    || (page.total_count > 0 && page.next_offset >= page.total_count);
                self.old_featured_error = None;
            }
            Err(err) => {
                self.old_featured_error = Some(err);
            }
        }
    }

    /// Jumps scroll to a specific sticker set section.
    pub fn jump_to_set(&mut self, set_id: StickerSetId) {
        self.search_query.clear();
        self.active_set_id = Some(set_id);
        self.scroll_to_set = Some(set_id);
    }

    /// Provides mutable access to the sticker media cache (shared with autocomplete sticker previews).
    pub fn media_cache_mut(&mut self) -> &mut StickerMediaCache {
        &mut self.media_cache
    }

    /// Handles keyboard navigation (`Arrow` keys, `Enter`, `Escape`).
    pub fn handle_keyboard(
        &mut self,
        ctx: &egui::Context,
        backend: &mut dyn ChatHelpersBackend,
    ) -> (bool, Option<StickersListEvent>) {
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.confirm_clear_recent {
                self.confirm_clear_recent = false;
                self.clear_recent_error = None;
                return (true, None);
            }
            if self.confirm_remove_set.is_some() {
                self.confirm_remove_set = None;
                return (true, None);
            }
            if !self.search_query.is_empty() || self.active_group.is_some() {
                self.search_query.clear();
                self.active_group = None;
                self.clear_remote_search_state();
                self.focused_index = None;
                return (true, None);
            }
            return (false, None);
        }

        let left = ctx.input(|i| i.key_pressed(Key::ArrowLeft));
        let right = ctx.input(|i| i.key_pressed(Key::ArrowRight));
        let up = ctx.input(|i| i.key_pressed(Key::ArrowUp));
        let down = ctx.input(|i| i.key_pressed(Key::ArrowDown));
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));

        if !(left || right || up || down || enter) {
            return (false, None);
        }

        let flat = self.collect_visible_docs(backend);
        if flat.is_empty() {
            return (false, None);
        }
        let max_idx = flat.len() - 1;

        if enter {
            if let Some(idx) = self.focused_index
                && let Some(&doc_id) = flat.get(idx)
            {
                return (true, Some(self.activate_sticker(doc_id, backend)));
            }
            return (false, None);
        }

        let cur = self.focused_index.unwrap_or(0);
        let next = if right {
            (cur + 1).min(max_idx)
        } else if left {
            cur.saturating_sub(1)
        } else if down {
            (cur + self.columns.max(1)).min(max_idx)
        } else if up {
            cur.saturating_sub(self.columns.max(1))
        } else {
            cur
        };
        self.focused_index = Some(next);
        (true, None)
    }

    fn collect_visible_docs(&self, backend: &dyn ChatHelpersBackend) -> Vec<DocumentId> {
        let query = self.effective_query();
        if !query.is_empty() {
            let mut docs = backend.search_stickers(&query);
            for &id in &self.remote_search_stickers {
                if !docs.contains(&id) {
                    docs.push(id);
                }
            }
            return docs;
        }
        let mut docs = Vec::new();
        for sec in backend.sticker_sections() {
            docs.extend(sec.stickers);
        }
        for sec in backend.featured_sticker_sections() {
            docs.extend(sec.stickers);
        }
        docs
    }

    fn activate_sticker(
        &self,
        doc_id: DocumentId,
        backend: &mut dyn ChatHelpersBackend,
    ) -> StickersListEvent {
        let is_locked = backend
            .sticker_document(doc_id)
            .is_some_and(|d| d.is_premium_sticker)
            && !backend.is_premium();
        if is_locked {
            return StickersListEvent::PremiumRequired(doc_id);
        }
        let set_id = backend.sticker_document(doc_id).and_then(|d| d.set_id);
        backend.record_sticker_used(doc_id);
        StickersListEvent::Chosen {
            document_id: doc_id,
            set_id,
        }
    }

    /// Renders the Stickers tab: search bar, Favorites/Recent/Installed/Trending grids, and pack footer.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
    ) -> Option<StickersListEvent> {
        let mut event = None;
        let full_rect = ui.available_rect_before_wrap();
        let footer_rect = Rect::from_min_max(
            Pos2::new(full_rect.left(), full_rect.bottom() - EMOJI_FOOTER_HEIGHT),
            full_rect.max,
        );
        let body_rect = Rect::from_min_max(
            full_rect.min,
            Pos2::new(full_rect.right(), footer_rect.top()),
        );

        let my_sections = backend.sticker_sections();
        let featured_sections = backend.featured_sticker_sections();
        let time_secs = ui.input(|i| i.time);
        let mut any_animated_visible = false;

        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            // 1. Search row (`TabbedSearch`: search by emoji or pack keyword, emoji groups).
            let search = render_tabbed_search(
                ui,
                "tg_stickers_search_field",
                &mut self.search_query,
                self.active_group,
                &mut self.mood_icons,
                palette,
            );
            if search.changed {
                self.active_group = None;
                self.focused_index = None;
            }
            if let Some(g) = search.group_clicked {
                let next = (self.active_group != Some(g)).then_some(g);
                self.set_active_group(next);
            }

            // 2. Scrollable stickers list.
            let scroll_rect = list_rect_below_search(body_rect, &search);
            ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
                // tdesktop scroll bars overlay the list (`emojiScroll`), they take no width.
                ui.spacing_mut().scroll = egui::style::ScrollStyle::floating();
                ScrollArea::vertical()
                    .id_salt("tg_stickers_list_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Frame::NONE
                            .inner_margin(emoji_list_margin())
                            .show(ui, |ui| {
                                if !self.effective_query().is_empty() {
                                    let query_trimmed = self.effective_query();
                                    self.sync_remote_search(&query_trimmed, backend);

                                    let mut hits = backend.search_stickers(&query_trimmed);
                                    for &id in &self.remote_search_stickers {
                                        if !hits.contains(&id) {
                                            hits.push(id);
                                        }
                                    }

                                    let emoji_pack_sections: Vec<tg_chat::stickers::StickerPanelSection> =
                                        self.remote_emoji_sets
                                            .iter()
                                            .filter_map(|&sid| backend.sticker_set_section(sid))
                                            .collect();

                                    if let Some(ref err) = self.remote_search_error.clone() {
                                        ui.add_space(6.0);
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new(err)
                                                    .size(12.0)
                                                    .color(palette.attention_fg),
                                            );
                                            if ui
                                                .small_button("Retry")
                                                .on_hover_text("Retry remote sticker search")
                                                .clicked()
                                            {
                                                self.last_remote_query.clear();
                                                self.sync_remote_search(&query_trimmed, backend);
                                            }
                                        });
                                    }

                                    if hits.is_empty() && emoji_pack_sections.is_empty() {
                                        ui.add_space(28.0);
                                        ui.vertical_centered(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} for \"{query_trimmed}\".",
                                                    tr::lng_stickers_nothing_found()
                                                ))
                                                .size(13.5)
                                                .color(palette.window_sub_text_fg),
                                            );
                                            ui.add_space(8.0);
                                            let clear_btn = ui.button(
                                                egui::RichText::new("Clear search")
                                                    .size(12.0)
                                                    .color(palette.window_active_text_fg),
                                            );
                                            if clear_btn.clicked() {
                                                self.search_query.clear();
                                                self.active_group = None;
                                                self.clear_remote_search_state();
                                            }
                                        });
                                    } else {
                                        let mut g_idx = 0usize;
                                        if !hits.is_empty() {
                                            if let Some(ev) = self.paint_sticker_grid(
                                                ui,
                                                &hits,
                                                &mut g_idx,
                                                time_secs,
                                                &mut any_animated_visible,
                                                backend,
                                                palette,
                                            ) {
                                                event = Some(ev);
                                            }

                                            if let Some(next_off) = self.remote_search_next_offset {
                                                ui.add_space(6.0);
                                                ui.vertical_centered(|ui| {
                                                    let resp = ui.button(
                                                        egui::RichText::new("Load more stickers")
                                                            .size(12.0)
                                                            .color(palette.window_active_text_fg),
                                                    );
                                                    resp.widget_info(|| {
                                                        egui::WidgetInfo::labeled(
                                                            egui::Role::Button,
                                                            true,
                                                            format!(
                                                                "Load more stickers (offset {next_off})"
                                                            ),
                                                        )
                                                    });
                                                    if resp.clicked() {
                                                        self.load_more_remote_search(backend);
                                                    }
                                                });
                                            }
                                        }

                                        // Matching remote custom-emoji packs (`messages.searchEmojiStickerSets`).
                                        if !emoji_pack_sections.is_empty() {
                                            ui.add_space(8.0);
                                            ui.painter().line_segment(
                                                [
                                                    Pos2::new(
                                                        ui.available_rect_before_wrap().left(),
                                                        ui.cursor().top(),
                                                    ),
                                                    Pos2::new(
                                                        ui.available_rect_before_wrap().right(),
                                                        ui.cursor().top(),
                                                    ),
                                                ],
                                                Stroke::new(1.0, palette.divider),
                                            );
                                            ui.add_space(4.0);
                                            ui.label(
                                                egui::RichText::new("Emoji Packs")
                                                    .font(font_id_from_style(
                                                        st::stickers_trending_header_font,
                                                    ))
                                                    .color(palette.window_sub_text_fg),
                                            );
                                            for sec in &emoji_pack_sections {
                                                let _ = self.paint_section_header(
                                                    ui,
                                                    sec.id,
                                                    &sec.title,
                                                    sec.stickers.len(),
                                                    true,
                                                    backend,
                                                    palette,
                                                    &mut event,
                                                );
                                                if let Some(ev) = self.paint_sticker_grid(
                                                    ui,
                                                    &sec.stickers,
                                                    &mut g_idx,
                                                    time_secs,
                                                    &mut any_animated_visible,
                                                    backend,
                                                    palette,
                                                ) {
                                                    event = Some(ev);
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    self.clear_remote_search_state();
                                    let mut g_idx = 0usize;
                                    for sec in &my_sections {
                                        let hdr_r = self.paint_section_header(
                                            ui,
                                            sec.id,
                                            &sec.title,
                                            sec.stickers.len(),
                                            false,
                                            backend,
                                            palette,
                                            &mut event,
                                        );
                                        if self.scroll_to_set == Some(sec.id) {
                                            ui.scroll_to_rect(hdr_r, Some(egui::Align::TOP));
                                            self.scroll_to_set = None;
                                        }
                                        if hdr_r.top() <= scroll_rect.top() + 36.0 {
                                            self.active_set_id = Some(sec.id);
                                        }
                                        if sec.id == RECENT_SET_ID && self.confirm_clear_recent {
                                            self.paint_clear_recent_confirm_strip(
                                                ui,
                                                backend,
                                                palette,
                                                &mut event,
                                            );
                                        }
                                        if self.confirm_remove_set == Some(sec.id) {
                                            self.paint_remove_set_confirm_strip(
                                                ui,
                                                sec.id,
                                                &sec.title,
                                                backend,
                                                palette,
                                                &mut event,
                                            );
                                        }
                                        if let Some(ev) = self.paint_sticker_grid(
                                            ui,
                                            &sec.stickers,
                                            &mut g_idx,
                                            time_secs,
                                            &mut any_animated_visible,
                                            backend,
                                            palette,
                                        ) {
                                            event = Some(ev);
                                        }
                                    }

                                    // Featured / Trending packs + older featured packs (`messages.getOldFeaturedStickers`)
                                    let mut extra_old_sections = Vec::new();
                                    for &sid in &self.old_featured_set_ids {
                                        if !featured_sections.iter().any(|f| f.id == sid)
                                            && let Some(sec) = backend.sticker_set_section(sid)
                                        {
                                            extra_old_sections.push(sec);
                                        }
                                    }
                                    let has_any_featured = !featured_sections.is_empty()
                                        || !extra_old_sections.is_empty();
                                    if has_any_featured || !self.old_featured_exhausted {
                                        ui.add_space(6.0);
                                        ui.painter().line_segment(
                                            [
                                                Pos2::new(
                                                    ui.available_rect_before_wrap().left(),
                                                    ui.cursor().top(),
                                                ),
                                                Pos2::new(
                                                    ui.available_rect_before_wrap().right(),
                                                    ui.cursor().top(),
                                                ),
                                            ],
                                            Stroke::new(1.0, palette.divider),
                                        );
                                        ui.add_space(4.0);
                                        for sec in featured_sections
                                            .iter()
                                            .chain(extra_old_sections.iter())
                                        {
                                            let _ = self.paint_section_header(
                                                ui,
                                                sec.id,
                                                &sec.title,
                                                sec.stickers.len(),
                                                true,
                                                backend,
                                                palette,
                                                &mut event,
                                            );
                                            if let Some(ev) = self.paint_sticker_grid(
                                                ui,
                                                &sec.stickers,
                                                &mut g_idx,
                                                time_secs,
                                                &mut any_animated_visible,
                                                backend,
                                                palette,
                                            ) {
                                                event = Some(ev);
                                            }
                                        }

                                        if let Some(ref err) = self.old_featured_error.clone() {
                                            ui.add_space(4.0);
                                            ui.label(
                                                egui::RichText::new(err)
                                                    .size(11.5)
                                                    .color(palette.attention_fg),
                                            );
                                        }
                                        if !self.old_featured_exhausted {
                                            ui.add_space(6.0);
                                            let btn_text = match self.old_featured_total_count {
                                                Some(total)
                                                    if total > self.old_featured_next_offset =>
                                                {
                                                    let rem = total - self.old_featured_next_offset;
                                                    format!("Load older trending packs ({rem} more)")
                                                }
                                                _ => "Load older trending packs".to_owned(),
                                            };
                                            ui.vertical_centered(|ui| {
                                                let resp = ui.button(
                                                    egui::RichText::new(&btn_text)
                                                        .size(12.0)
                                                        .color(palette.window_active_text_fg),
                                                );
                                                resp.widget_info(|| {
                                                    egui::WidgetInfo::labeled(
                                                        egui::Role::Button,
                                                        true,
                                                        "Load older trending packs",
                                                    )
                                                });
                                                if resp.clicked() {
                                                    self.load_more_old_featured(backend);
                                                }
                                            });
                                        }
                                    }
                                }
                            });
                    });
            });
        });

        // 3. Bottom pack icon footer (`StickersListFooter`).
        let icons = backend.sticker_tab_icons(&my_sections, 24);
        self.paint_footer(ui, footer_rect, &icons, time_secs, backend, palette);

        if any_animated_visible {
            // Smooth 30fps playback for visible Lottie animated stickers.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }

        event
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_section_header(
        &mut self,
        ui: &mut Ui,
        set_id: StickerSetId,
        title: &str,
        count: usize,
        is_featured: bool,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
        event_out: &mut Option<StickersListEvent>,
    ) -> Rect {
        let width = ui.available_width().max(120.0);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(width, EMOJI_HEADER_HEIGHT), Sense::hover());
        let pan = &st::default_emoji_pan;
        let left = (pan.header_left.0 - pan.margin.left.0 - pan.padding.left.0).max(0) as f32;
        let text_top = rect.top() + pan.header_top.0 as f32;

        ui.painter().text(
            Pos2::new(rect.left() + left, text_top),
            Align2::LEFT_TOP,
            title,
            if is_featured {
                font_id_from_style(st::stickers_trending_header_font)
            } else {
                font_id_from_style(st::emoji_pan_header_font)
            },
            if is_featured {
                theme_color(ui.ctx(), "stickersTrendingHeaderFg", palette.window_bold_fg)
            } else {
                theme_color(ui.ctx(), "emojiPanHeaderFg", palette.window_sub_text_fg)
            },
        );
        // `stickerPanRemoveSet` (20 x 20 small close button) right-aligned on the title line.
        let remove_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 20.0, text_top - 3.0),
            Vec2::splat(20.0),
        );

        if is_featured {
            let installed = backend.is_set_installed(set_id);
            let label = if installed {
                tr::lng_stickers_featured_installed()
            } else {
                tr::lng_stickers_featured_add()
            };
            let btn_rect = Rect::from_min_size(
                Pos2::new(rect.right() - 56.0, rect.center().y - 11.0),
                Vec2::new(52.0, 22.0),
            );
            let resp = ui.interact(
                btn_rect,
                ui.make_persistent_id(("tg_featured_add_btn", set_id.0)),
                Sense::click(),
            );
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{label} {title}"))
            });
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let fill = if installed {
                palette.window_bg_over
            } else {
                palette.window_bg_active
            };
            let fg = if installed {
                palette.window_active_text_fg
            } else {
                palette.window_fg_active
            };
            ui.painter()
                .rect_filled(btn_rect, CornerRadius::same(11), fill);
            ui.painter().text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(11.5),
                fg,
            );
            if resp.clicked() {
                if installed {
                    if backend.remove_sticker_set(set_id) {
                        *event_out = Some(StickersListEvent::SetRemoved(set_id));
                    }
                } else if backend.install_sticker_set(set_id) {
                    *event_out = Some(StickersListEvent::SetInstalled(set_id));
                }
            }
        } else if set_id == RECENT_SET_ID && count > 0 {
            let resp = ui.interact(
                remove_rect,
                ui.make_persistent_id("tg_clear_recent_stickers_btn"),
                Sense::click(),
            );
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "Clear Recent Stickers")
            });
            paint_remove_set_cross(ui, remove_rect, resp.hovered(), palette);
            if resp.on_hover_text("Clear Recent Stickers").clicked() {
                self.confirm_clear_recent = !self.confirm_clear_recent;
                self.clear_recent_error = None;
            }
        } else if set_id != FAVED_SET_ID && set_id != RECENT_SET_ID {
            // Installed (non-special) sets have a remove button (`hasRemoveButton`).
            let resp = ui.interact(
                remove_rect,
                ui.make_persistent_id(("tg_remove_sticker_set_btn", set_id.0)),
                Sense::click(),
            );
            let label = tr::lng_stickers_remove_pack(title);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
            paint_remove_set_cross(ui, remove_rect, resp.hovered(), palette);
            if resp.clicked() {
                self.confirm_remove_set = if self.confirm_remove_set == Some(set_id) {
                    None
                } else {
                    Some(set_id)
                };
            }
        }
        let _ = count;

        rect
    }

    fn paint_remove_set_confirm_strip(
        &mut self,
        ui: &mut Ui,
        set_id: StickerSetId,
        title: &str,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
        event_out: &mut Option<StickersListEvent>,
    ) {
        Frame::NONE
            .fill(palette.window_bg_over)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(tr::lng_stickers_remove_pack(title))
                        .size(12.0)
                        .color(palette.window_bold_fg),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(
                            egui::RichText::new(tr::lng_stickers_remove_pack_confirm())
                                .size(11.5)
                                .color(palette.attention_fg),
                        )
                        .clicked()
                    {
                        self.confirm_remove_set = None;
                        if backend.remove_sticker_set(set_id) {
                            *event_out = Some(StickersListEvent::SetRemoved(set_id));
                        }
                    }
                    if ui
                        .button(
                            egui::RichText::new(tr::lng_cancel())
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                        )
                        .clicked()
                    {
                        self.confirm_remove_set = None;
                    }
                });
            });
        ui.add_space(4.0);
    }

    fn paint_clear_recent_confirm_strip(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
        event_out: &mut Option<StickersListEvent>,
    ) {
        Frame::NONE
            .fill(palette.window_bg_over)
            .stroke(Stroke::new(1.0, palette.divider))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Clear all recent stickers?")
                        .size(12.0)
                        .color(palette.window_bold_fg),
                );
                if let Some(ref err) = self.clear_recent_error {
                    ui.label(
                        egui::RichText::new(err)
                            .size(11.0)
                            .color(palette.attention_fg),
                    );
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let confirm_btn = ui.button(
                        egui::RichText::new("Clear Recent")
                            .size(11.5)
                            .color(palette.attention_fg),
                    );
                    if confirm_btn.clicked() {
                        match backend.clear_recent_stickers(false) {
                            Ok(true) => {
                                self.confirm_clear_recent = false;
                                self.clear_recent_error = None;
                                *event_out = Some(StickersListEvent::RecentCleared);
                            }
                            Ok(false) => {
                                self.clear_recent_error =
                                    Some("Could not clear recent stickers. Try again.".to_owned());
                            }
                            Err(err) => {
                                self.clear_recent_error = Some(err);
                            }
                        }
                    }
                    if ui
                        .button(
                            egui::RichText::new(tr::lng_cancel())
                                .size(11.5)
                                .color(palette.window_sub_text_fg),
                        )
                        .clicked()
                    {
                        self.confirm_clear_recent = false;
                        self.clear_recent_error = None;
                    }
                });
            });
        ui.add_space(4.0);
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_sticker_grid(
        &mut self,
        ui: &mut Ui,
        docs: &[DocumentId],
        global_idx: &mut usize,
        time_secs: f64,
        any_animated: &mut bool,
        backend: &mut dyn ChatHelpersBackend,
        palette: &Palette,
    ) -> Option<StickersListEvent> {
        let mut chosen = None;
        let avail_w = ui.available_width().max(STICKER_CELL_SIZE);
        // `StickersListWidget::resizeGetHeight`: as many `stickerPanWidthMin` columns as fit.
        let cols = ((avail_w / STICKER_CELL_SIZE).floor() as usize).max(1);
        self.columns = cols;
        let cell_w = avail_w / cols as f32;
        let cell_h = STICKER_CELL_SIZE;
        let rows = docs.len().div_ceil(cols);
        let total_h = rows as f32 * cell_h;

        let (grid_rect, _) = ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::hover());
        let clip = ui.clip_rect();
        let first_row =
            (((clip.top() - grid_rect.top()) / cell_h).floor().max(0.0) as usize).min(rows);
        let last_row =
            (((clip.bottom() - grid_rect.top()) / cell_h).ceil().max(0.0) as usize).min(rows);

        let base_idx = *global_idx;
        *global_idx += docs.len();
        let is_premium_user = backend.is_premium();

        for row in first_row..last_row {
            for col in 0..cols {
                let local_i = row * cols + col;
                let Some(&doc_id) = docs.get(local_i) else {
                    break;
                };
                let item_idx = base_idx + local_i;
                let cell_rect = Rect::from_min_size(
                    Pos2::new(
                        grid_rect.left() + col as f32 * cell_w,
                        grid_rect.top() + row as f32 * cell_h,
                    ),
                    Vec2::new(cell_w, cell_h),
                );
                let id = ui.make_persistent_id(("tg_sticker_cell", item_idx, doc_id.0));
                let resp = ui.interact(cell_rect, id, Sense::click());

                let is_focused = self.focused_index == Some(item_idx);
                if resp.hovered() || is_focused {
                    // `_overBg` (`emojiPanHover`) behind the selected sticker.
                    ui.painter().rect_filled(
                        cell_rect,
                        CornerRadius::same(st::emoji_pan_radius.0 as u8),
                        theme_color(ui.ctx(), "emojiPanHover", palette.window_bg_over),
                    );
                }
                if is_focused {
                    ui.painter().rect_stroke(
                        cell_rect.shrink(2.0),
                        CornerRadius::same(8),
                        Stroke::new(1.5, palette.window_bg_active),
                        StrokeKind::Inside,
                    );
                }

                let draw_side = cell_w.min(cell_h) - 12.0;
                let draw_rect = Rect::from_center_size(cell_rect.center(), Vec2::splat(draw_side));

                if let Some((tex_id, tex_size, animated)) =
                    self.media_cache
                        .texture_for_sized(ui.ctx(), doc_id, time_secs, backend)
                {
                    if animated {
                        *any_animated = true;
                    }
                    ui.painter().image(
                        tex_id,
                        fit_rect_keep_aspect(draw_rect, tex_size),
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    // Loading placeholder (`PaintStickerThumbnailPath`, `pathBg`).
                    ui.painter().circle_filled(
                        cell_rect.center(),
                        draw_side * 0.35,
                        sticker_path_bg(ui),
                    );
                }

                let is_premium_sticker = backend
                    .sticker_document(doc_id)
                    .is_some_and(|d| d.is_premium_sticker);
                let is_locked = is_premium_sticker && !is_premium_user;

                // Premium-locked marker pill (`StickerPremiumMark` in bottom-right corner).
                if is_locked {
                    paint_premium_lock_badge(ui.painter(), cell_rect);
                } else if is_premium_sticker {
                    paint_premium_star_badge(ui.painter(), cell_rect);
                }

                // Favorites are toggled from the context menu (`fillContextMenu`).
                let is_faved = backend.is_sticker_faved(doc_id);
                let mut faved_clicked = false;
                let _ = resp.context_menu(|ui| {
                    let label = if is_faved {
                        tr::lng_faved_stickers_remove()
                    } else {
                        tr::lng_faved_stickers_add()
                    };
                    if ui.button(label).clicked() {
                        backend.toggle_faved_sticker(doc_id);
                        faved_clicked = true;
                        ui.close();
                    }
                });

                if !faved_clicked && resp.clicked() {
                    self.focused_index = Some(item_idx);
                    chosen = Some(self.activate_sticker(doc_id, backend));
                }
            }
        }

        chosen
    }

    fn paint_footer(
        &mut self,
        ui: &mut Ui,
        footer_rect: Rect,
        icons: &[tg_chat::stickers::StickerTabIcon],
        time_secs: f64,
        backend: &dyn ChatHelpersBackend,
        palette: &Palette,
    ) {
        // `StickersListFooter` with `stickersSettings`: the settings button first, then one
        // `iconWidth` slot per section, left aligned, rounded `categoriesBgOver` selection.
        paint_footer_background(ui, footer_rect, palette);
        let compose = &st::default_compose_icons;
        let icon_fg = theme_color(ui.ctx(), "emojiIconFg", palette.window_sub_text_fg);
        let icon_fg_active = theme_color(ui.ctx(), "emojiSubIconFgActive", palette.window_bold_fg);

        let settings_slot = footer_slot_rect(footer_rect, 0);
        let settings_resp = ui.interact(
            settings_slot,
            ui.make_persistent_id("tg_sticker_footer_settings"),
            Sense::click(),
        );
        settings_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, tr::lng_stickers_installed_tab())
        });
        if !paint_footer_icon(ui, settings_slot, compose.settings) {
            ui.painter()
                .circle_stroke(settings_slot.center(), 7.0, Stroke::new(1.5, icon_fg));
        }
        if settings_resp.clicked() {
            self.settings_requested = true;
        }

        let searching = !self.effective_query().is_empty();
        for (idx, icon) in icons.iter().enumerate() {
            let slot_rect = footer_slot_rect(footer_rect, idx + 1);
            if slot_rect.right() > footer_rect.right() {
                break;
            }
            let resp = ui.interact(
                slot_rect,
                ui.make_persistent_id(("tg_sticker_footer", idx, icon.set_id.0)),
                Sense::click(),
            );
            let active = !searching && self.active_set_id == Some(icon.set_id);
            if active {
                paint_footer_selection(ui, footer_rect, idx + 1, idx + 1);
            }

            if icon.set_id == FAVED_SET_ID {
                paint_star_icon(
                    ui.painter(),
                    slot_rect.center(),
                    8.0,
                    if active { icon_fg_active } else { icon_fg },
                    false,
                );
            } else if icon.set_id == RECENT_SET_ID {
                let ic = if active {
                    compose.recent_active
                } else {
                    compose.recent
                };
                if !paint_footer_icon(ui, slot_rect, ic) {
                    paint_clock_icon(ui.painter(), slot_rect.center(), 7.5, icon_fg);
                }
            } else if let Some(doc_id) = icon.thumbnail_document_id
                && let Some((tex_id, tex_size, _)) =
                    self.media_cache
                        .texture_for_sized(ui.ctx(), doc_id, time_secs, backend)
            {
                let thumb = fit_rect_keep_aspect(
                    Rect::from_center_size(slot_rect.center(), Vec2::splat(24.0)),
                    tex_size,
                );
                ui.painter().image(
                    tex_id,
                    thumb,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                ui.painter()
                    .circle_filled(slot_rect.center(), 9.0, sticker_path_bg(ui));
            }

            if resp.clicked() {
                self.active_group = None;
                self.jump_to_set(icon.set_id);
            }
        }
    }
}

/// Loading placeholder color (`defaultEmojiPan.pathBg` = `windowBgRipple`).
fn sticker_path_bg(ui: &Ui) -> Color32 {
    theme_color(
        ui.ctx(),
        "windowBgRipple",
        ui.visuals().widgets.inactive.bg_fill,
    )
}

/// Paints the `stickerPanRemoveSet` small close cross (`smallCloseIcon` / `smallCloseIconOver`).
fn paint_remove_set_cross(ui: &Ui, rect: Rect, hovered: bool, palette: &Palette) {
    let dark = ui.ctx().global_style().visuals.dark_mode;
    let icon = if hovered {
        st::small_close_icon_over
    } else {
        st::small_close_icon
    };
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if !paint_helper_icon(
        ui.painter(),
        Rect::from_center_size(rect.center(), Vec2::splat(12.0)),
        icon,
        dark,
        None,
    ) {
        let c = rect.center();
        let fg = palette.window_sub_text_fg;
        ui.painter().line_segment(
            [c + Vec2::new(-4.0, -4.0), c + Vec2::new(4.0, 4.0)],
            Stroke::new(1.4, fg),
        );
        ui.painter().line_segment(
            [c + Vec2::new(4.0, -4.0), c + Vec2::new(-4.0, 4.0)],
            Stroke::new(1.4, fg),
        );
    }
}

/// Paints Telegram's bottom-right circular Premium lock marker on a sticker cell (`st::stickers_premium_lock`).
pub(crate) fn paint_premium_lock_badge(painter: &egui::Painter, cell_rect: Rect) {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let center = Pos2::new(cell_rect.right() - 12.0, cell_rect.bottom() - 12.0);
    painter.circle_filled(center, 9.5, Color32::from_rgb(0x7B, 0x5C, 0xF5));
    painter.circle_stroke(center, 9.5, Stroke::new(1.2, Color32::WHITE));
    let icon_rect = Rect::from_center_size(center, Vec2::splat(13.0));
    if !paint_helper_icon(
        painter,
        icon_rect,
        st::stickers_premium_lock,
        dark,
        Some(Color32::WHITE),
    ) {
        let shackle_c = Pos2::new(center.x, center.y - 2.0);
        painter.circle_stroke(shackle_c, 2.6, Stroke::new(1.3, Color32::WHITE));
        painter.rect_filled(
            Rect::from_center_size(Pos2::new(center.x, center.y + 2.0), Vec2::new(7.5, 5.5)),
            CornerRadius::same(1),
            Color32::WHITE,
        );
    }
}

fn paint_premium_star_badge(painter: &egui::Painter, cell_rect: Rect) {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let center = Pos2::new(cell_rect.right() - 11.0, cell_rect.bottom() - 11.0);
    painter.circle_filled(center, 8.0, Color32::from_rgb(0x88, 0x62, 0xF8));
    let icon_rect = Rect::from_center_size(center, Vec2::splat(12.0));
    if !paint_helper_icon(
        painter,
        icon_rect,
        st::info_premium_star,
        dark,
        Some(Color32::WHITE),
    ) {
        paint_star_icon(painter, center, 4.8, Color32::WHITE, true);
    }
}

/// Paints Telegram's favorite star icon (`st::menu_icon_unfave` / `st::menu_icon_fave`),
/// falling back to a 5-point vector star if the icon mask is unavailable.
pub(crate) fn paint_star_icon(
    painter: &egui::Painter,
    center: Pos2,
    outer_r: f32,
    color: Color32,
    filled: bool,
) {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let icon = if filled {
        st::info_premium_star
    } else {
        st::menu_icon_fave
    };
    let icon_rect = Rect::from_center_size(center, Vec2::splat((outer_r * 2.1).max(12.0)));
    if paint_helper_icon(painter, icon_rect, icon, dark, Some(color)) {
        return;
    }
    let inner_r = outer_r * 0.44;
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let angle = -std::f32::consts::FRAC_PI_2 + (i as f32) * std::f32::consts::PI / 5.0;
        let r = if i % 2 == 0 { outer_r } else { inner_r };
        pts.push(Pos2::new(
            center.x + r * angle.cos(),
            center.y + r * angle.sin(),
        ));
    }
    if filled {
        for i in 0..10 {
            let next = (i + 1) % 10;
            painter.add(egui::Shape::convex_polygon(
                vec![center, pts[i], pts[next]],
                color,
                Stroke::NONE,
            ));
        }
    } else {
        painter.add(egui::Shape::closed_line(pts, Stroke::new(1.5, color)));
    }
}

/// Paints Telegram's recent clock icon (`st::info_icon_media_stories_recent`) used in Recent footers.
pub(crate) fn paint_clock_icon(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    let dark = painter.ctx().global_style().visuals.dark_mode;
    let icon_rect = Rect::from_center_size(center, Vec2::splat((radius * 2.2).max(16.0)));
    if paint_helper_icon(
        painter,
        icon_rect,
        st::info_icon_media_stories_recent,
        dark,
        Some(color),
    ) {
        return;
    }
    painter.circle_stroke(center, radius, Stroke::new(1.5, color));
    painter.line_segment(
        [center, Pos2::new(center.x, center.y - radius * 0.55)],
        Stroke::new(1.5, color),
    );
    painter.line_segment(
        [
            center,
            Pos2::new(center.x + radius * 0.45, center.y + radius * 0.15),
        ],
        Stroke::new(1.5, color),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_helpers::mock::MockChatHelpersBackend;
    use crate::chat_helpers::tabbed_panel::{TabbedPanel, panel_docks_at_width};
    use crate::chat_helpers::tabbed_selector::SelectorTab;
    use tg_app_core::stand_in::theme::ThemeMode;
    use tg_emoji::EmojiTextureCache;

    fn run_frame(ctx: &egui::Context, time: f64, mut paint: impl FnMut(&egui::Context)) {
        let input = egui::RawInput {
            time: Some(time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| paint(ui.ctx()));
        output.textures_delta.clear();
    }

    /// Opening the Stickers tab froze the app for 6-8 s because every visible Lottie sticker
    /// was parsed and rasterized on the UI thread. The UI thread must do none of that work.
    #[test]
    fn opening_sticker_panel_does_no_lottie_rasterization_on_ui_thread() {
        let ctx = egui::Context::default();
        let mut backend = MockChatHelpersBackend::new();
        let mut emoji_cache = EmojiTextureCache::new();
        let palette = Palette::for_mode(ThemeMode::Day);
        let mut panel = TabbedPanel::new();
        panel.set_active_tab(SelectorTab::Stickers);
        panel.show_immediate();
        let anchor = Rect::from_min_size(Pos2::new(560.0, 740.0), Vec2::new(420.0, 44.0));

        let lottie_docs = backend
            .sticker_sections()
            .iter()
            .flat_map(|s| s.stickers.clone())
            .filter(|&d| matches!(backend.sticker_media(d), Some(StickerMedia::LottieJson(_))))
            .count();
        assert!(lottie_docs >= 5, "mock must have Lottie stickers to open");

        let ui_work_before = lottie_work_on_current_thread();
        let mut uploaded = 0;
        for frame in 0..400u32 {
            run_frame(&ctx, f64::from(frame) / 30.0, |ctx| {
                let _ = panel.show(
                    ctx,
                    anchor,
                    &mut backend,
                    &mut emoji_cache,
                    &palette,
                    |_| {},
                );
            });
            uploaded = panel
                .selector_mut()
                .sticker_media_cache_mut()
                .lottie_frames_uploaded();
            if uploaded >= 4 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            lottie_work_on_current_thread(),
            ui_work_before,
            "Lottie parsing / rasterization must run on the background workers"
        );
        assert!(
            uploaded >= 4,
            "background workers must deliver Lottie frames to the panel (got {uploaded})"
        );
    }

    #[test]
    fn sticker_media_fetches_are_spread_over_frames() {
        let ctx = egui::Context::default();
        let backend = MockChatHelpersBackend::new();
        let mut cache = StickerMediaCache::new();
        let docs: Vec<DocumentId> = backend
            .sticker_sections()
            .iter()
            .flat_map(|s| s.stickers.clone())
            .collect();
        assert!(docs.len() > STICKER_MEDIA_FETCHES_PER_FRAME);
        let mut started = 0;
        run_frame(&ctx, 0.0, |ctx| {
            for &d in &docs {
                let _ = cache.texture_for(ctx, d, 0.0, &backend);
            }
            started = cache.static_textures.len() + cache.lottie_slots.len();
        });
        assert!(
            started <= STICKER_MEDIA_FETCHES_PER_FRAME,
            "one frame started {started} media lookups"
        );
    }

    /// Stage 3 (live): the panel filled in a few stickers at a time. The two stickers still
    /// downloading used up the per-frame lookup budget on every frame, so the cells after them
    /// never started loading until those two finished.
    #[test]
    fn stickers_still_loading_do_not_hold_back_later_cells() {
        let ctx = egui::Context::default();
        let mut cache = StickerMediaCache::new();
        let docs: Vec<DocumentId> = (1..=8).map(DocumentId).collect();
        let loading = |id: DocumentId| id.0 <= 2;
        let fetch = |id: DocumentId| {
            (!loading(id)).then(|| StickerMedia::StaticRgba {
                width: 1,
                height: 1,
                rgba: vec![255; 4],
            })
        };
        for frame in 0..4 {
            run_frame(&ctx, f64::from(frame) / 30.0, |ctx| {
                for &d in &docs {
                    let _ = cache.texture_with(ctx, d, 0.0, fetch);
                }
            });
        }
        assert_eq!(
            cache.static_textures.len(),
            6,
            "every loaded sticker must get its texture while two others are still loading"
        );
        assert_eq!(cache.pending.len(), 2);
    }

    #[test]
    fn previews_keep_the_sticker_aspect_ratio() {
        assert_eq!(fit_preview_size(512, 512, 96), [96, 96]);
        assert_eq!(fit_preview_size(512, 256, 96), [96, 48]);
        assert_eq!(fit_preview_size(300, 600, 96), [48, 96]);
        let cell = Rect::from_min_size(Pos2::ZERO, Vec2::splat(52.0));
        let wide = fit_rect_keep_aspect(cell, Vec2::new(512.0, 256.0));
        assert!((wide.width() - 52.0).abs() < 0.01);
        assert!((wide.height() - 26.0).abs() < 0.01);
        assert!((wide.center().y - 26.0).abs() < 0.01);
    }

    #[test]
    fn panel_docks_only_when_three_columns_fit() {
        assert!(panel_docks_at_width(1440.0, true));
        assert!(panel_docks_at_width(932.0, true));
        assert!(!panel_docks_at_width(931.0, true));
        assert!(!panel_docks_at_width(390.0, true));
        assert!(panel_docks_at_width(672.0, false));
    }
}
