//! Telegram Stories UI subsystem for `tg-app`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/media/stories/media_stories_view.cpp` &
//!   `media_stories_{controller,header,slider,caption_full_view,reply,reactions,recent_views,sibling,share,stealth}.cpp`
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_stories_list.cpp` & `dialogs_stories_content.cpp`
//!
//! Exposes [`StoriesStrip`] (ready for `dialogs/` to embed at the top of the chat list),
//! [`StoryViewerOverlay`] (fullscreen viewer over the window), [`StoriesBackend`] +
//! [`MockStoriesBackend`], and [`AppStoriesHost`] (minimal runtime hook for `app.rs`).

pub mod areas;
pub use tg_app_core::stories::backend;
pub mod boxes;
pub mod compose;
pub mod layout;
pub mod media_loader;
pub mod mock;
pub mod strip;
pub mod viewer;

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tg_data::{FullStoryId, PeerId, TimeId};
use tg_stories::{StoriesContext, StoriesStore};

pub use areas::{format_url_badge, paint_story_areas, reaction_display_text};
pub use backend::{
    DEFAULT_STORY_PERIOD_SECS, EditStoryRequest, PeerProfile, ReportedStoryRecord,
    STORY_PERIOD_OPTIONS, SendStoryRequest, SentStoryReply, ShareTarget, SharedStoryRecord,
    StoriesBackend, StoryDraft, StoryMediaAreasInput, StoryMediaAsset, StoryPrivacyInput,
    StoryUploadMedia,
};
pub use boxes::{StoryModalBox, StoryModalOutcome, ViewsFilter, filter_story_views};
pub use compose::{
    MediaAreaEditorKind, PrivacyPeerListKind, STORY_CAPTION_LIMIT_DEFAULT,
    STORY_CAPTION_LIMIT_PREMIUM, SendStoryBox, SendStoryOutcome, StoryDraftsBox,
    StoryDraftsOutcome, StoryPrivacyBox, StoryPrivacyOutcome, format_story_period,
    load_story_media_from_path, parse_story_caption_entities, privacy_mode_label,
};
pub use layout::{
    HeaderLayout, STORIES_MAX_HEIGHT, STORIES_MAX_WIDTH, STORIES_RADIUS, SiblingCardLayout,
    SliderSegmentRect, StoriesStripMetrics, StripRenderLayout, ViewerLayout, argb_u32_to_color32,
    choose_weather_fg, compute_slider_segments, compute_strip_layout, format_compact_count,
    format_story_timestamp, format_temperature, project_area_rect, ring_segment_arc_angles,
    rotated_rect_corners, rotated_rounded_rect_points,
};
pub use media_loader::{
    DecodedRgba, StoryMediaCache, StoryVideoPlayback, decode_photo_via_tg_media,
    video_frame_to_color_image,
};
pub use mock::{GradientPreset, MockStoriesBackend, generate_gradient_story_png, mock_peers};
pub use strip::{StoriesStrip, StoriesStripAction, format_tooltip_summary, paint_avatar_circle};
pub use viewer::{
    HOLD_TO_PAUSE_THRESHOLD_MS, QUICK_REACTIONS, StoryViewerOverlay, format_reply_placeholder,
};

use tg_app_core::stand_in::theme::Palette;

/// Self-contained stories host wiring [`StoriesStore`], [`StoriesStrip`], [`StoryViewerOverlay`],
/// [`SendStoryBox`], [`StoryDraftsBox`], [`StoryMediaCache`], and a [`StoriesBackend`] into
/// [`crate::app::TgforkApp`] with a minimal footprint in `app.rs`.
pub struct AppStoriesHost {
    started_at: Instant,
    store: StoriesStore,
    backend: Box<dyn StoriesBackend>,
    strip: StoriesStrip,
    viewer: StoryViewerOverlay,
    composer: Option<SendStoryBox>,
    drafts_box: Option<StoryDraftsBox>,
    drafts: Vec<StoryDraft>,
    next_draft_id: u64,
    toast_text: Option<(String, i64)>,
    media_cache: StoryMediaCache,
    last_strip_rect: Option<egui::Rect>,
}

impl Default for AppStoriesHost {
    fn default() -> Self {
        Self::new()
    }
}

impl AppStoriesHost {
    /// Initializes the stories host with [`MockStoriesBackend`] and loads initial strips.
    #[must_use]
    pub fn new() -> Self {
        let now_secs = current_unix_secs();
        let mut mock = MockStoriesBackend::new(now_secs);
        let mut store = StoriesStore::new(mock.self_peer_id());
        mock.bootstrap_store(&mut store, now_secs);
        Self {
            started_at: Instant::now(),
            store,
            backend: Box::new(mock),
            strip: StoriesStrip::new(),
            viewer: StoryViewerOverlay::new(),
            composer: None,
            drafts_box: None,
            drafts: Vec::new(),
            next_draft_id: 1,
            toast_text: None,
            media_cache: StoryMediaCache::new(),
            last_strip_rect: None,
        }
    }

    /// Initializes the stories host with a custom [`StoriesBackend`] and [`StoriesStore`].
    #[must_use]
    pub fn with_backend(store: StoriesStore, backend: Box<dyn StoriesBackend>) -> Self {
        Self {
            started_at: Instant::now(),
            store,
            backend,
            strip: StoriesStrip::new(),
            viewer: StoryViewerOverlay::new(),
            composer: None,
            drafts_box: None,
            drafts: Vec::new(),
            next_draft_id: 1,
            toast_text: None,
            media_cache: StoryMediaCache::new(),
            last_strip_rect: None,
        }
    }

    /// Returns a reference to the [`StoriesStore`].
    #[must_use]
    pub const fn store(&self) -> &StoriesStore {
        &self.store
    }

    /// Returns a mutable reference to the [`StoriesStore`].
    pub fn store_mut(&mut self) -> &mut StoriesStore {
        &mut self.store
    }

    /// Returns a reference to the [`StoriesStrip`] widget.
    #[must_use]
    pub const fn strip(&self) -> &StoriesStrip {
        &self.strip
    }

    /// Returns a mutable reference to the [`StoriesStrip`] widget.
    pub fn strip_mut(&mut self) -> &mut StoriesStrip {
        &mut self.strip
    }

    /// Returns a reference to the [`StoryViewerOverlay`].
    #[must_use]
    pub const fn viewer(&self) -> &StoryViewerOverlay {
        &self.viewer
    }

    /// Returns a mutable reference to the [`StoryViewerOverlay`].
    pub fn viewer_mut(&mut self) -> &mut StoryViewerOverlay {
        &mut self.viewer
    }

    /// Returns a reference to the active [`SendStoryBox`] composer, if open.
    #[must_use]
    pub fn composer(&self) -> Option<&SendStoryBox> {
        self.composer.as_ref()
    }

    /// Returns a mutable reference to the active [`SendStoryBox`] composer, if open.
    pub fn composer_mut(&mut self) -> Option<&mut SendStoryBox> {
        self.composer.as_mut()
    }

    /// Returns a reference to the active [`StoryDraftsBox`], if open.
    #[must_use]
    pub fn drafts_box(&self) -> Option<&StoryDraftsBox> {
        self.drafts_box.as_ref()
    }

    /// Returns all currently saved local [`StoryDraft`] entries.
    #[must_use]
    pub fn drafts(&self) -> &[StoryDraft] {
        &self.drafts
    }

    /// Saves or updates a local [`StoryDraft`] and returns its assigned draft ID.
    pub fn save_draft(&mut self, mut draft: StoryDraft) -> u64 {
        if draft.id != 0
            && let Some(existing) = self.drafts.iter_mut().find(|d| d.id == draft.id)
        {
            *existing = draft;
            return existing.id;
        }
        let id = self.next_draft_id;
        self.next_draft_id = self.next_draft_id.saturating_add(1);
        draft.id = id;
        self.drafts.insert(0, draft);
        id
    }

    /// Deletes a saved draft by `id`, returning `true` if a draft was removed.
    pub fn delete_draft(&mut self, id: u64) -> bool {
        let before = self.drafts.len();
        self.drafts.retain(|d| d.id != id);
        self.drafts.len() < before
    }

    /// Opens the story composer ([`SendStoryBox`]) for the signed-in account.
    pub fn open_composer(&mut self) {
        let self_peer = self.backend.self_peer_id();
        let close_friends = self.backend.close_friends();
        self.drafts_box = None;
        self.composer = Some(SendStoryBox::new(self_peer, &close_friends));
    }

    /// Opens the story composer in edit mode for an existing story in the store.
    pub fn open_composer_for_edit(&mut self, story_id: FullStoryId) -> bool {
        let Ok(story) = self.store.lookup(story_id).cloned() else {
            return false;
        };
        let close_friends = self.backend.close_friends();
        self.viewer.close(&mut self.store, &mut self.media_cache);
        self.drafts_box = None;
        self.composer = Some(SendStoryBox::for_edit(&story, &close_friends));
        true
    }

    /// Closes the story viewer, composer and drafts box when the app locks behind the local
    /// passcode (`Application::closeAdditionalWindows`); nothing reopens until the host renders
    /// the stories host again after unlock.
    pub fn close_for_passcode_lock(&mut self) {
        if self.viewer.is_open() {
            self.viewer.close(&mut self.store, &mut self.media_cache);
        }
        self.composer = None;
        self.drafts_box = None;
    }

    /// Returns `true` when the viewer, composer or drafts box is showing.
    #[must_use]
    pub fn has_open_window(&self) -> bool {
        self.viewer.is_open() || self.composer.is_some() || self.drafts_box.is_some()
    }

    /// Opens the [`StoryDraftsBox`] modal.
    pub fn open_drafts(&mut self, choosing_media: bool) {
        self.composer = None;
        self.drafts_box = Some(StoryDraftsBox::new(choosing_media));
    }

    /// Returns the most recent active toast message from composer/upload actions, if any.
    #[must_use]
    pub fn active_toast(&self) -> Option<&str> {
        self.toast_text.as_ref().map(|(s, _)| s.as_str())
    }

    /// Opens the exact `(peer_id, story_id)` requested by a deep link (`t.me/<user>/s/<id>` or
    /// `t.me/<user>/s/live`), resolving missing stories or peer lists through the backend first.
    pub fn open_story_deep_link(&mut self, peer_id: PeerId, story_id: Option<i32>) -> bool {
        let now_secs = current_unix_secs();
        let now_ms = self.elapsed_ms();
        let opened = match story_id {
            Some(sid) if sid > 0 => {
                let full_id = FullStoryId::new(peer_id, sid);
                if self.store.lookup(full_id).is_err() {
                    self.store.resolve(full_id, false);
                    self.store.flush_resolve_requests();
                    let pending = self.store.take_actions();
                    if !pending.is_empty() {
                        self.backend
                            .execute_actions(&mut self.store, pending, now_secs);
                    }
                }
                if self.store.lookup(full_id).is_err() {
                    return false;
                }
                let context = if self
                    .store
                    .source(peer_id)
                    .is_some_and(|src| src.index_of(sid).is_some())
                {
                    StoriesContext::Peer
                } else {
                    StoriesContext::Single
                };
                self.viewer.open_story(
                    &mut self.store,
                    self.backend.as_mut(),
                    full_id,
                    context,
                    now_secs,
                    now_ms,
                )
            }
            _ => {
                if self.store.source(peer_id).is_none_or(|s| s.ids.is_empty()) {
                    self.store.request_peer_stories(peer_id);
                    let pending = self.store.take_actions();
                    if !pending.is_empty() {
                        self.backend
                            .execute_actions(&mut self.store, pending, now_secs);
                    }
                }
                self.viewer.open_peer(
                    &mut self.store,
                    self.backend.as_mut(),
                    peer_id,
                    StoriesContext::Peer,
                    now_secs,
                    now_ms,
                )
            }
        };
        if opened {
            self.composer = None;
            self.drafts_box = None;
        }
        opened
    }

    /// Returns the screen rectangle occupied by [`StoriesStrip`] on the most recent frame,
    /// or `None` if no stories strip was visible.
    #[must_use]
    pub const fn last_strip_rect(&self) -> Option<egui::Rect> {
        self.last_strip_rect
    }

    /// Returns `true` if the stories store currently has at least one visible or archived source.
    #[must_use]
    pub fn has_stories(&self) -> bool {
        !self
            .store
            .sources(tg_stories::StorySourcesList::NotHidden)
            .is_empty()
            || !self
                .store
                .sources(tg_stories::StorySourcesList::Hidden)
                .is_empty()
            || !self.backend.pending_uploads().is_empty()
    }

    /// Opens the first available story in the main strip (`StorySourcesList::NotHidden`).
    pub fn open_first_story(&mut self) -> bool {
        let Some(target_peer) = self
            .store
            .sources(tg_stories::StorySourcesList::NotHidden)
            .first()
            .map(|s| s.id)
        else {
            return false;
        };
        let now_secs = current_unix_secs();
        let now_ms = self.elapsed_ms();
        self.viewer.open_peer(
            &mut self.store,
            self.backend.as_mut(),
            target_peer,
            StoriesContext::SourcesList(tg_stories::StorySourcesList::NotHidden),
            now_secs,
            now_ms,
        )
    }

    /// Opens a specific `(peer_id, story_id)` in [`StoryViewerOverlay`], falling back to the
    /// peer's first story if `story_id` is `None` or not yet cached locally.
    pub fn open_story_by_id(&mut self, peer_id: tg_data::PeerId, story_id: Option<i32>) -> bool {
        let now_secs = current_unix_secs();
        let now_ms = self.elapsed_ms();
        if let Some(sid) = story_id
            && self.viewer.open_story(
                &mut self.store,
                self.backend.as_mut(),
                tg_data::FullStoryId::new(peer_id, sid),
                StoriesContext::Single,
                now_secs,
                now_ms,
            )
        {
            return true;
        }
        self.viewer.open_peer(
            &mut self.store,
            self.backend.as_mut(),
            peer_id,
            StoriesContext::SourcesList(tg_stories::StorySourcesList::NotHidden),
            now_secs,
            now_ms,
        )
    }

    /// Renders the embeddable [`StoriesStrip`] inside `ui` (ready for `dialogs/` to call directly)
    /// and dispatches all emitted [`StoriesStripAction`]s.
    pub fn show_strip(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let top_before = ui.cursor().top();
        let left = ui.max_rect().left();
        let width = ui.available_width();
        let actions = self
            .strip
            .show(ui, &self.store, self.backend.as_ref(), palette);
        let bottom_after = ui.cursor().top();
        if bottom_after > top_before + 1.0 {
            self.last_strip_rect = Some(egui::Rect::from_min_max(
                egui::Pos2::new(left, top_before),
                egui::Pos2::new(left + width, bottom_after),
            ));
        } else {
            self.last_strip_rect = None;
        }
        let now_secs = current_unix_secs();
        let now_ms = self.elapsed_ms();
        self.handle_strip_actions(actions, now_secs, now_ms);
    }

    /// Consumes a pending request from the story viewer to open the Telegram Premium screen.
    pub const fn take_open_premium_request(&mut self) -> bool {
        self.viewer.take_premium_request()
    }

    /// Consumes a pending request from an unsupported story card to check for app updates.
    pub const fn take_update_app_request(&mut self) -> bool {
        self.viewer.take_update_app_request()
    }

    /// Drives store timers and renders the fullscreen [`StoryViewerOverlay`] when open.
    ///
    /// Also binds `Ctrl+Shift+S` to open the featured interactive story viewer for quick manual testing.
    pub fn update_and_render(&mut self, ctx: &egui::Context, palette: &Palette) {
        let _ = palette;
        let now_secs = current_unix_secs();
        let now_ms = self.elapsed_ms();

        // Advance `StoriesStore` timers (read-batching, polling, expiry) and execute queued actions.
        self.store.tick(now_secs, now_ms);
        let pending = self.store.take_actions();
        if !pending.is_empty() {
            self.backend
                .execute_actions(&mut self.store, pending, now_secs);
        }

        // Debug shortcut: Ctrl+Shift+S opens the story viewer if closed.
        let open_shortcut = ctx.input(|i| {
            i.modifiers
                .matches_logically(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)
                && i.key_pressed(egui::Key::S)
        });
        if open_shortcut && !self.viewer.is_open() {
            let _ = self.open_first_story();
        }

        if self.viewer.is_open() {
            self.viewer.show(
                ctx,
                &mut self.store,
                self.backend.as_mut(),
                &mut self.media_cache,
                now_secs,
                now_ms,
            );
            if let Some(edit_id) = self.viewer.take_edit_story_request() {
                let _ = self.open_composer_for_edit(edit_id);
            }
        }

        let viewport = ctx.input(|i| i.content_rect());
        if let Some(mut composer) = self.composer.take() {
            let outcome = composer.show_modal(
                ctx,
                viewport,
                self.backend.as_ref(),
                self.drafts.len(),
                now_secs,
            );
            let keep_open = self.handle_composer_outcome(outcome, &mut composer, now_secs, now_ms);
            if keep_open {
                self.composer = Some(composer);
            }
        }

        if let Some(mut drafts_box) = self.drafts_box.take() {
            let outcome =
                drafts_box.show_modal(ctx, viewport, &self.drafts, self.backend.as_ref(), now_secs);
            let keep_open = self.handle_drafts_outcome(outcome, now_ms);
            if keep_open {
                self.drafts_box = Some(drafts_box);
            }
        }

        if let Some((_, until_ms)) = &self.toast_text
            && now_ms >= *until_ms
        {
            self.toast_text = None;
        }
        if let Some((msg, _)) = &self.toast_text {
            egui::Area::new(egui::Id::unique("stories_host_toast"))
                .order(egui::Order::Tooltip)
                .fade_in(false)
                .anchor(egui::Align2::CENTER_BOTTOM, egui::Vec2::new(0.0, -24.0))
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(egui::Color32::from_rgba_unmultiplied(24, 32, 42, 240))
                        .corner_radius(egui::CornerRadius::same(16))
                        .inner_margin(egui::Margin::symmetric(14, 8))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(msg)
                                    .size(12.5)
                                    .color(egui::Color32::WHITE),
                            );
                        });
                });
        }
    }

    fn handle_composer_outcome(
        &mut self,
        outcome: SendStoryOutcome,
        composer: &mut SendStoryBox,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        match outcome {
            SendStoryOutcome::None => true,
            SendStoryOutcome::Close => false,
            SendStoryOutcome::OpenDrafts => {
                self.drafts_box = Some(StoryDraftsBox::new(true));
                false
            }
            SendStoryOutcome::SaveDraft(draft) => {
                let id = self.save_draft(draft);
                composer.draft_id = Some(id);
                self.show_toast("Story draft saved", now_ms);
                false
            }
            SendStoryOutcome::UpdateCloseFriends(peers) => {
                if let Err(err) = self.backend.set_close_friends(peers) {
                    composer.error_message = Some(err);
                } else {
                    self.show_toast("Close Friends updated", now_ms);
                }
                true
            }
            SendStoryOutcome::SaveStoriesBlocklist(peers) => {
                match self.backend.save_stories_blocklist(&peers) {
                    Ok(_) => {
                        self.show_toast(
                            if peers.is_empty() {
                                "Stories blocklist cleared"
                            } else {
                                "Stories blocklist updated"
                            },
                            now_ms,
                        );
                    }
                    Err(err) => {
                        composer.error_message = Some(err);
                    }
                }
                true
            }
            SendStoryOutcome::SubmitSend(req) => {
                let draft_to_clear = composer.draft_id;
                match self
                    .backend
                    .send_story(Some(&mut self.store), req, now_secs)
                {
                    Ok(_) => {
                        if let Some(did) = draft_to_clear {
                            let _ = self.delete_draft(did);
                        }
                        self.show_toast("Story posted!", now_ms);
                        false
                    }
                    Err(err) => {
                        composer.error_message = Some(err);
                        true
                    }
                }
            }
            SendStoryOutcome::SubmitEdit(req) => {
                match self
                    .backend
                    .edit_story(Some(&mut self.store), req, now_secs)
                {
                    Ok(()) => {
                        self.show_toast("Story updated!", now_ms);
                        false
                    }
                    Err(err) => {
                        composer.error_message = Some(err);
                        true
                    }
                }
            }
        }
    }

    fn handle_drafts_outcome(&mut self, outcome: StoryDraftsOutcome, now_ms: i64) -> bool {
        match outcome {
            StoryDraftsOutcome::None => true,
            StoryDraftsOutcome::Close => false,
            StoryDraftsOutcome::CreateNew => {
                self.open_composer();
                false
            }
            StoryDraftsOutcome::ResumeDraft(id) => {
                if let Some(draft) = self.drafts.iter().find(|d| d.id == id).cloned() {
                    let close_friends = self.backend.close_friends();
                    self.composer = Some(SendStoryBox::from_draft(&draft, &close_friends));
                }
                false
            }
            StoryDraftsOutcome::DeleteDraft(id) => {
                if self.delete_draft(id) {
                    self.show_toast("Draft deleted", now_ms);
                }
                true
            }
        }
    }

    fn show_toast(&mut self, msg: impl Into<String>, now_ms: i64) {
        let Some(msg) = tg_app_core::user_error::visible_toast_text(msg.into()) else {
            return;
        };
        self.toast_text = Some((msg, now_ms + 3_200));
    }

    /// Dispatches actions emitted by [`StoriesStrip::show`].
    pub fn handle_strip_actions(
        &mut self,
        actions: Vec<StoriesStripAction>,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        for act in actions {
            match act {
                StoriesStripAction::OpenStory { peer_id, list } => {
                    let _ = self.viewer.open_peer(
                        &mut self.store,
                        self.backend.as_mut(),
                        peer_id,
                        StoriesContext::SourcesList(list),
                        now_secs,
                        now_ms,
                    );
                }
                StoriesStripAction::OpenStealth { peer_id, list } => {
                    self.viewer.open_stealth_prompt(
                        &mut self.store,
                        self.backend.as_mut(),
                        peer_id,
                        StoriesContext::SourcesList(list),
                        now_secs,
                        now_ms,
                    );
                }
                StoriesStripAction::ToggleHidden { peer_id, hidden } => {
                    self.store.toggle_hidden(peer_id, hidden);
                    let pending = self.store.take_actions();
                    self.backend
                        .execute_actions(&mut self.store, pending, now_secs);
                }
                StoriesStripAction::LoadMore(list) => {
                    self.store.load_more(list);
                    let pending = self.store.take_actions();
                    self.backend
                        .execute_actions(&mut self.store, pending, now_secs);
                }
                StoriesStripAction::AddStory => {
                    self.open_composer();
                }
                StoriesStripAction::DismissFailedUpload(peer_id) => {
                    self.backend.dismiss_failed_uploads(peer_id);
                }
                StoriesStripAction::SwitchList(_) | StoriesStripAction::ExpandedChanged(_) => {}
            }
        }
    }

    fn elapsed_ms(&self) -> i64 {
        self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
    }
}

fn current_unix_secs() -> TimeId {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as TimeId)
        .unwrap_or(1_760_000_000)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use egui::{Color32, Pos2, Rect, Vec2};
    use tg_data::{FullStoryId, ReactionId};
    use tg_stories::{
        DialogBarMode, PauseState, PlaybackStatus, ReplyAreaKind, ReplyPlaceholder,
        ShareCapability, SliderState, StealthActivationDecision, StoriesContext, StoriesStore,
        StoryArea, StoryAreaHit, StorySourcesList, StoryViewerController, TooltipSummary,
        ViewerConfig, ViewerEvent,
    };

    use super::*;

    const TEST_NOW_SECS: TimeId = 1_760_000_000;

    fn setup_mock_store() -> (StoriesStore, MockStoriesBackend) {
        let mut backend = MockStoriesBackend::new(TEST_NOW_SECS);
        let mut store = StoriesStore::new(backend.self_peer_id());
        backend.bootstrap_store(&mut store, TEST_NOW_SECS);
        (store, backend)
    }

    // -------------------------------------------------------------------------
    // 1. Viewer layout geometry, slider math, tap zones, close-by-click
    // -------------------------------------------------------------------------

    #[test]
    fn viewer_layout_switches_between_normal_and_outside_header_and_computes_siblings() {
        let normal_vp = Rect::from_min_size(Pos2::ZERO, Vec2::new(1180.0, 760.0));
        let normal = ViewerLayout::compute(normal_vp);
        assert_eq!(normal.header_layout, HeaderLayout::Normal);
        assert!(normal.content.height() > normal.content.width());
        // Portrait aspect ratio 9:16 (540:960).
        let aspect = normal.content.width() / normal.content.height();
        assert!((aspect - (540.0 / 960.0)).abs() < 0.02);
        // Header sits inside the top of the story frame in Normal mode.
        assert!(normal.slider.top() >= normal.content.top());
        assert!(normal.header.top() >= normal.slider.bottom());
        // Sibling cards sit to the left and right of content.
        assert!(normal.sibling_left.geometry.right() <= normal.content.left());
        assert!(normal.sibling_right.geometry.left() >= normal.content.right());

        // Tall viewport triggers `HeaderLayout::Outside`.
        let tall_vp = Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 1180.0));
        let tall = ViewerLayout::compute(tall_vp);
        assert_eq!(tall.header_layout, HeaderLayout::Outside);
        assert!(tall.header.bottom() <= tall.content.top());

        // Left/right tap split & close-by-click zones.
        assert!(normal.is_left_tap(Pos2::new(
            normal.content.left() + 10.0,
            normal.content.center().y
        )));
        assert!(!normal.is_left_tap(Pos2::new(
            normal.content.center().x + 20.0,
            normal.content.center().y
        )));
        assert!(normal.close_by_click_at(Pos2::new(10.0, 400.0)));
        assert!(!normal.close_by_click_at(normal.content.center()));
    }

    #[test]
    fn slider_segments_match_tdesktop_active_completed_upcoming_and_hide_on_livestream() {
        let bar = Rect::from_min_size(Pos2::new(100.0, 10.0), Vec2::new(300.0, 2.0));
        let segs = compute_slider_segments(
            bar,
            SliderState {
                index: 1,
                total: 3,
                active_progress: 0.45,
                video_stream: false,
            },
        );
        assert_eq!(segs.len(), 3);
        assert!((segs[0].fill_ratio - 1.0).abs() < 1e-5);
        assert!(segs[1].is_active);
        assert!((segs[1].fill_ratio - 0.45).abs() < 1e-5);
        assert!((segs[2].fill_ratio - 0.0).abs() < 1e-5);
        assert!(segs[0].rect.right() < segs[1].rect.left());

        let live_segs = compute_slider_segments(
            bar,
            SliderState {
                index: 0,
                total: 1,
                active_progress: 0.0,
                video_stream: true,
            },
        );
        assert!(live_segs.is_empty());
    }

    // -------------------------------------------------------------------------
    // 2. Interactive areas: rotated coordinates, hit testing, weather contrast
    // -------------------------------------------------------------------------

    #[test]
    fn rotated_area_projection_weather_contrast_and_hit_testing_work() {
        let (mut store, mut backend) = setup_mock_store();
        let mut ctrl = StoryViewerController::new();
        let featured = MockStoriesBackend::featured_interactive_story_id();
        assert!(ctrl.show(
            &mut store,
            featured,
            StoriesContext::SourcesList(StorySourcesList::NotHidden),
            ViewerConfig::default(),
            TEST_NOW_SECS,
            1_000,
        ));

        // Hit-test the weather widget center (73%, 42%).
        let weather_hit = ctrl.hit_test_area(&store, 0.73, 0.42);
        assert!(matches!(weather_hit, Some(StoryAreaHit::Weather { .. })));
        assert!(ctrl.weather_in_celsius());
        assert!(!ctrl.toggle_weather_unit());
        assert_eq!(format_temperature(22_400, true), "22°C");
        assert_eq!(format_temperature(22_400, false), "72°F");

        // Hit-test the location area center (36%, 29%).
        let loc_hit = ctrl.hit_test_area(&store, 0.36, 0.29);
        assert!(matches!(
            loc_hit,
            Some(StoryAreaHit::Location { title, .. }) if title == "Harbor Observatory"
        ));

        // Hit-test the URL area center (42%, 55%).
        let url_hit = ctrl.hit_test_area(&store, 0.42, 0.55);
        assert!(matches!(url_hit, Some(StoryAreaHit::Url { .. })));

        // Hit-test the suggested reaction area center (74%, 68%).
        let react_hit = ctrl.hit_test_area(&store, 0.74, 0.68);
        assert!(matches!(
            react_hit,
            Some(StoryAreaHit::SuggestedReaction { .. })
        ));

        // Verify luminance contrast rule (`ChooseWeatherFg`, threshold 0.705).
        assert_eq!(
            choose_weather_fg(Color32::from_rgb(255, 250, 220)),
            Color32::BLACK
        );
        assert_eq!(
            choose_weather_fg(Color32::from_rgb(20, 45, 90)),
            Color32::WHITE
        );

        // Verify rotated polygon corners rotate symmetrically around area center.
        let area = StoryArea::from_tl_coords(50.0, 50.0, 40.0, 10.0, 15.0, Some(10.0));
        let content = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(400.0, 700.0));
        let (proj, radius) = project_area_rect(area, content);
        assert!(radius > 0.0);
        let corners = rotated_rect_corners(proj, area.rotation);
        let avg_x = corners.iter().map(|p| p.x).sum::<f32>() / 4.0;
        let avg_y = corners.iter().map(|p| p.y).sum::<f32>() / 4.0;
        assert!((avg_x - proj.center().x).abs() < 1.0);
        assert!((avg_y - proj.center().y).abs() < 1.0);

        let _ = &mut backend;
    }

    // -------------------------------------------------------------------------
    // 3. Mock backend action execution, strip expand/collapse, hide/archive
    // -------------------------------------------------------------------------

    #[test]
    fn mock_backend_executes_actions_and_strip_handles_expand_hysteresis_and_hide() {
        let (mut store, mut backend) = setup_mock_store();
        assert!(backend.executed_actions_count > 0);
        assert!(!store.sources(StorySourcesList::NotHidden).is_empty());
        assert_eq!(store.sources(StorySourcesList::Hidden).len(), 1);

        let mut strip = StoriesStrip::new();
        strip.sync_from_store(&store, &backend);
        assert_eq!(strip.mode(), DialogBarMode::Small);
        assert!(!strip.is_expanded());

        // Tooltip summary skips self when other peers exist.
        match strip.bar_state().tooltip_summary() {
            TooltipSummary::Peers(names) => {
                assert!(!names.is_empty());
                assert!(!names.contains(&"My Story".to_owned()));
            }
            other => panic!("expected Peers tooltip summary, got {other:?}"),
        }

        // Pull above 0.72 * 77 = 55.4 -> expands!
        assert_eq!(strip.apply_pull_height(60), Some(true));
        assert!(strip.is_expanded());
        // Pull back below 0.68 * 77 = 52.3 -> collapses!
        assert_eq!(strip.apply_pull_height(45), Some(false));
        assert!(!strip.is_expanded());

        // Hide Aurora Studio via `StoriesStore::toggle_hidden` -> moves from NotHidden to Hidden!
        let before_main = store.sources(StorySourcesList::NotHidden).len();
        let before_hidden = store.sources(StorySourcesList::Hidden).len();
        store.toggle_hidden(mock_peers::AURORA, true);
        let acts = store.take_actions();
        backend.execute_actions(&mut store, acts, TEST_NOW_SECS);
        assert_eq!(
            store.sources(StorySourcesList::NotHidden).len(),
            before_main - 1
        );
        assert_eq!(
            store.sources(StorySourcesList::Hidden).len(),
            before_hidden + 1
        );
    }

    // -------------------------------------------------------------------------
    // 4. Auto-advance, hold-to-pause, reactions, stealth mode, views stats filter
    // -------------------------------------------------------------------------

    #[test]
    fn viewer_overlay_drives_auto_advance_hold_pause_reactions_and_stealth() {
        let (mut store, mut backend) = setup_mock_store();
        let mut viewer = StoryViewerOverlay::new();
        assert!(viewer.open_peer(
            &mut store,
            &mut backend,
            mock_peers::AURORA,
            StoriesContext::SourcesList(StorySourcesList::NotHidden),
            TEST_NOW_SECS,
            10_000,
        ));
        assert_eq!(
            viewer.controller().shown_id(),
            Some(FullStoryId::new(mock_peers::AURORA, 201))
        );
        assert_eq!(viewer.controller().pause_state(), PauseState::Playing);

        // Hold-to-pause pauses immediately and resumes on release.
        viewer.controller_mut().content_pressed(true, 11_000);
        assert_eq!(viewer.controller().pause_state(), PauseState::Paused);
        viewer.controller_mut().content_pressed(false, 12_000);
        assert_eq!(viewer.controller().pause_state(), PauseState::Playing);

        // Auto-advance after 5000 ms of playing moves from story 201 -> 202.
        viewer.controller_mut().tick(
            &mut store,
            ViewerConfig::default(),
            TEST_NOW_SECS + 6,
            16_500,
        );
        assert_eq!(
            viewer.controller().shown_id(),
            Some(FullStoryId::new(mock_peers::AURORA, 202))
        );

        // Reaction + Stealth Mode activation execute cleanly against MockStoriesBackend.
        viewer
            .controller_mut()
            .send_reaction(&mut store, Some(ReactionId::Emoji("\u{1F525}".to_owned())));
        let decision = viewer
            .controller_mut()
            .setup_stealth_mode(&mut store, TEST_NOW_SECS, true);
        assert_eq!(decision, StealthActivationDecision::ReadyToActivate);
        let acts = store.take_actions();
        backend.execute_actions(&mut store, acts, TEST_NOW_SECS);
        assert!(store.stealth_mode().is_active(TEST_NOW_SECS + 10));

        // Share capability & views filter on own story.
        assert!(viewer.open_story(
            &mut store,
            &mut backend,
            MockStoriesBackend::featured_self_story_id(),
            StoriesContext::SourcesList(StorySourcesList::NotHidden),
            TEST_NOW_SECS,
            20_000,
        ));
        let cap = viewer
            .controller_mut()
            .share_requested(&store, TEST_NOW_SECS);
        assert!(matches!(
            cap,
            ShareCapability::ShareStory {
                can_copy_link: true,
                ..
            }
        ));
        assert!(
            viewer
                .controller()
                .pending_events()
                .iter()
                .any(|e| matches!(e, ViewerEvent::ShareRequested(_)))
        );

        let self_story = store
            .lookup(MockStoriesBackend::featured_self_story_id())
            .expect("self story present");
        let only_reposts =
            filter_story_views(&self_story.views.list, ViewsFilter::Reposts, "", |pid| {
                backend.peer_profile(pid).name
            });
        assert_eq!(only_reposts.len(), 2);
        let search_cedar =
            filter_story_views(&self_story.views.list, ViewsFilter::All, "cedar", |pid| {
                backend.peer_profile(pid).name
            });
        assert_eq!(search_cedar.len(), 1);
    }

    // -------------------------------------------------------------------------
    // 5. Photo decoding via tg-media & video playback via tg-video::StreamingPlayer
    // -------------------------------------------------------------------------

    #[test]
    fn mock_photo_decodes_via_tg_media_and_timestamps_format() {
        let mut backend = MockStoriesBackend::new(TEST_NOW_SECS);
        let asset = backend
            .load_story_media(MockStoriesBackend::featured_interactive_story_id())
            .expect("generated gradient PNG");
        let StoryMediaAsset::PhotoBytes(bytes) = asset else {
            panic!("expected PhotoBytes");
        };
        let (full, blurred) = decode_photo_via_tg_media(&bytes).expect("tg-media decode succeeds");
        assert_eq!(full.width, 360);
        assert_eq!(full.height, 640);
        assert_eq!(full.rgba.len(), 360 * 640 * 4);
        assert!(blurred.width >= 16 && blurred.height >= 16);

        assert_eq!(
            format_story_timestamp(TEST_NOW_SECS - 20, TEST_NOW_SECS),
            "just now"
        );
        assert_eq!(
            format_story_timestamp(TEST_NOW_SECS - 300, TEST_NOW_SECS),
            "5m ago"
        );
        assert_eq!(
            format_story_timestamp(TEST_NOW_SECS - 7200, TEST_NOW_SECS),
            "2h ago"
        );
        assert_eq!(
            format_reply_placeholder(
                &ReplyPlaceholder::StealthModeCountdown {
                    time_left: "24:59".to_owned(),
                },
                ReplyAreaKind::Reply
            ),
            "Stealth Mode active: 24:59"
        );
    }

    #[test]
    fn video_story_decodes_and_advances_through_tg_video_streaming_player() {
        let ffmpeg = crate::media::mock::find_ffmpeg_bin();
        let ffmpeg_available = std::process::Command::new(&ffmpeg)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !ffmpeg_available {
            return;
        }
        let temp_mp4 = std::env::temp_dir().join(format!(
            "rustgram_story_video_test_{}.mp4",
            std::process::id()
        ));
        let status = Command::new(&ffmpeg)
            .args([
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=180x320:rate=15:duration=1.2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(&temp_mp4)
            .status()
            .expect("spawn ffmpeg");
        assert!(status.success(), "ffmpeg failed to generate test mp4");

        let (mut store, mut backend) = setup_mock_store();
        let vid_id = MockStoriesBackend::featured_video_story_id();
        backend.set_video_file(vid_id, temp_mp4.clone());

        let asset = backend.load_story_media(vid_id).expect("video asset");
        let mut cache = StoryMediaCache::new();
        let player = cache
            .ensure_video(None, vid_id, &asset, true, 0.8, false, 1_000)
            .expect("StreamingPlayer opened test mp4");
        assert!(player.current_frame().is_some());
        let state0 = player.step(None, 1_100);
        assert_eq!(state0.status, PlaybackStatus::Playing);
        let state_mid = player.step(None, 1_500);
        assert!(state_mid.position >= state0.position);

        // Feed StreamingPlayer TrackState directly into StoryViewerController.
        let mut viewer = StoryViewerOverlay::new();
        assert!(viewer.open_story(
            &mut store,
            &mut backend,
            vid_id,
            StoriesContext::SourcesList(StorySourcesList::NotHidden),
            TEST_NOW_SECS,
            1_000,
        ));
        viewer.controller_mut().update_video_playback(
            &mut store,
            state_mid,
            ViewerConfig::default(),
            TEST_NOW_SECS,
            1_500,
        );
        assert!(viewer.controller().slider_state().active_progress > 0.0);

        // Pause propagates cleanly.
        player.set_paused(true, 1_500);
        let state_paused = player.step(None, 2_000);
        assert_eq!(state_paused.status, PlaybackStatus::Paused);

        // Clean up temp video file.
        let _ = std::fs::remove_file(&temp_mp4);
    }

    #[test]
    fn caption_expand_sibling_navigation_and_non_premium_stealth_work() {
        let (mut store, mut backend) = setup_mock_store();
        let mut viewer = StoryViewerOverlay::new();
        assert!(viewer.open_story(
            &mut store,
            &mut backend,
            MockStoriesBackend::featured_interactive_story_id(),
            StoriesContext::SourcesList(StorySourcesList::NotHidden),
            TEST_NOW_SECS,
            1_000,
        ));

        // Expand caption pauses playback and fades canvas; closing resumes playback.
        assert!(viewer.controller_mut().show_full_caption(1_100));
        assert!(viewer.controller().is_caption_expanded());
        assert!(viewer.controller().is_content_faded());
        assert_eq!(viewer.controller().pause_state(), PauseState::Inactive);
        viewer.controller_mut().caption_closed(1_200);
        assert!(!viewer.controller().is_caption_expanded());
        assert_eq!(viewer.controller().pause_state(), PauseState::Playing);

        // Sibling previews exist in the SourcesList context and jump_for switches peers.
        let initial_peer = viewer.controller().shown_id().unwrap().peer;
        let has_sibling = viewer.controller().sibling_left().is_some()
            || viewer.controller().sibling_right().is_some();
        assert!(has_sibling);
        let delta = if viewer.controller().sibling_right().is_some() {
            1
        } else {
            -1
        };
        assert!(viewer.controller_mut().jump_for(
            &mut store,
            delta,
            ViewerConfig::default(),
            TEST_NOW_SECS,
            1_300,
        ));
        assert_ne!(viewer.controller().shown_id().unwrap().peer, initial_peer);

        // Non-premium Stealth Mode decision requests Premium unlock.
        backend.set_account_premium(false);
        let dec = viewer.controller_mut().setup_stealth_mode(
            &mut store,
            TEST_NOW_SECS,
            backend.account_premium(),
        );
        assert_eq!(dec, StealthActivationDecision::RequiresPremium);

        // Outgoing replies, shares, reports, and viewer-block state persist on MockStoriesBackend.
        assert!(
            backend
                .send_story_reply(
                    MockStoriesBackend::featured_interactive_story_id(),
                    "Awesome view!",
                    TEST_NOW_SECS
                )
                .is_ok()
        );
        assert_eq!(backend.sent_replies.len(), 1);
        assert!(
            backend
                .share_story(
                    MockStoriesBackend::featured_interactive_story_id(),
                    &[mock_peers::CEDAR],
                    "Check this out"
                )
                .is_ok()
        );
        assert_eq!(backend.shared_records.len(), 1);
        assert!(
            backend
                .report_story(MockStoriesBackend::featured_interactive_story_id(), "Spam")
                .is_ok()
        );
        assert_eq!(backend.reported_records.len(), 1);

        backend.set_viewer_stories_blocked(mock_peers::VIEWER_ORBIT, true);
        assert!(backend.is_viewer_stories_blocked(mock_peers::VIEWER_ORBIT));
    }

    /// Captures real `eframe` PNG screenshots of the stories strip, interactive fullscreen viewer,
    /// and own-story stats/views box into `crates/tg-app/docs/stories*.png`.
    ///
    /// Run explicitly via:
    /// `cargo test -p tg-app capture_stories_screenshots -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn capture_stories_screenshots() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
        let _ = std::fs::create_dir_all(&docs_dir);

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Scene {
            Strip,
            ViewerInteractive,
            ViewerViewsBox,
        }

        struct SceneSpec {
            scene: Scene,
            out_path: PathBuf,
            size: Vec2,
        }

        struct ScreenshotApp {
            scenes: Vec<SceneSpec>,
            current: usize,
            initialized_scene: Option<usize>,
            host: AppStoriesHost,
            collapsed_strip: StoriesStrip,
            night_strip: StoriesStrip,
            palette: Palette,
            frames_in_scene: u32,
            requested_for_scene: Option<usize>,
            completed_count: Arc<AtomicUsize>,
        }

        impl ScreenshotApp {
            fn setup_scene(&mut self, idx: usize) {
                let now_secs = current_unix_secs();
                let mut host = AppStoriesHost::new();
                match self.scenes[idx].scene {
                    Scene::Strip => {}
                    Scene::ViewerInteractive => {
                        let _ = host.viewer.open_story(
                            &mut host.store,
                            host.backend.as_mut(),
                            MockStoriesBackend::featured_interactive_story_id(),
                            StoriesContext::SourcesList(StorySourcesList::NotHidden),
                            now_secs,
                            1_000,
                        );
                        let cfg = ViewerConfig::default();
                        host.viewer
                            .controller_mut()
                            .tick(&mut host.store, cfg, now_secs, 2_800);
                        host.viewer.controller_mut().toggle_paused(true, 2_800);
                    }
                    Scene::ViewerViewsBox => {
                        let _ = host.viewer.open_story(
                            &mut host.store,
                            host.backend.as_mut(),
                            MockStoriesBackend::featured_self_story_id(),
                            StoriesContext::SourcesList(StorySourcesList::NotHidden),
                            now_secs,
                            1_000,
                        );
                        host.viewer.open_views_box(
                            &mut host.store,
                            host.backend.as_mut(),
                            now_secs,
                            1_200,
                        );
                    }
                }
                self.host = host;
                self.initialized_scene = Some(idx);
                self.frames_in_scene = 0;
            }
        }

        impl eframe::App for ScreenshotApp {
            fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
                let ctx = ui.ctx().clone();

                let done_so_far = self.completed_count.load(Ordering::Acquire);
                if done_so_far >= self.scenes.len() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                if done_so_far > self.current {
                    self.current = done_so_far;
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                        self.scenes[self.current].size,
                    ));
                    self.setup_scene(self.current);
                } else if self.initialized_scene != Some(self.current) {
                    self.setup_scene(self.current);
                }

                let scene = self.scenes[self.current].scene;
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(self.palette.window_bg))
                    .show(ui, |ui| match scene {
                        Scene::Strip => {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.set_width(360.0);
                                    ui.add_space(8.0);
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.label(
                                            egui::RichText::new("Expanded Stories Strip (Full)")
                                                .size(13.0)
                                                .strong()
                                                .color(self.palette.window_bold_fg),
                                        );
                                    });
                                    ui.add_space(4.0);
                                    self.host.strip_mut().set_expanded_immediate(true);
                                    self.host.show_strip(ui, &self.palette);
                                    for (name, msg, time, rgb) in [
                                        (
                                            "Aurora Studio",
                                            "Shared a new story from Harbor Observatory",
                                            "14:22",
                                            [0xFF, 0x8A, 0x65],
                                        ),
                                        (
                                            "Cedar Labs",
                                            "Streaming video clip ready for review",
                                            "13:50",
                                            [0x66, 0xBB, 0x6A],
                                        ),
                                        (
                                            "Design Journal",
                                            "Issue #18 — Typography grids",
                                            "12:15",
                                            [0xAB, 0x47, 0xBC],
                                        ),
                                        (
                                            "Kestrel Dev",
                                            "Clean modular layout architecture!",
                                            "11:40",
                                            [0x26, 0xC6, 0xDA],
                                        ),
                                    ] {
                                        let (r, _) = ui.allocate_exact_size(
                                            Vec2::new(360.0, 58.0),
                                            egui::Sense::hover(),
                                        );
                                        let c = Pos2::new(r.left() + 32.0, r.center().y);
                                        ui.painter().circle_filled(
                                            c,
                                            20.0,
                                            Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                                        );
                                        ui.painter().text(
                                            Pos2::new(r.left() + 64.0, r.top() + 18.0),
                                            egui::Align2::LEFT_CENTER,
                                            name,
                                            egui::FontId::proportional(13.5),
                                            self.palette.window_bold_fg,
                                        );
                                        ui.painter().text(
                                            Pos2::new(r.left() + 64.0, r.top() + 38.0),
                                            egui::Align2::LEFT_CENTER,
                                            msg,
                                            egui::FontId::proportional(12.0),
                                            self.palette.window_sub_text_fg,
                                        );
                                        ui.painter().text(
                                            Pos2::new(r.right() - 12.0, r.top() + 18.0),
                                            egui::Align2::RIGHT_CENTER,
                                            time,
                                            egui::FontId::proportional(11.0),
                                            self.palette.window_sub_text_fg,
                                        );
                                    }
                                });

                                ui.separator();

                                ui.vertical(|ui| {
                                    ui.set_width(360.0);
                                    ui.add_space(8.0);
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.label(
                                            egui::RichText::new("Collapsed Stories Strip (Small)")
                                                .size(13.0)
                                                .strong()
                                                .color(self.palette.window_bold_fg),
                                        );
                                    });
                                    ui.add_space(4.0);
                                    let _ = self.collapsed_strip.show(
                                        ui,
                                        self.host.store(),
                                        self.host.backend.as_ref(),
                                        &self.palette,
                                    );

                                    ui.add_space(20.0);
                                    ui.horizontal(|ui| {
                                        ui.add_space(12.0);
                                        ui.label(
                                            egui::RichText::new("Night Mode — Expanded Strip")
                                                .size(13.0)
                                                .strong()
                                                .color(self.palette.window_bold_fg),
                                        );
                                    });
                                    ui.add_space(4.0);
                                    let night = Palette::night();
                                    let _ = self.night_strip.show(
                                        ui,
                                        self.host.store(),
                                        self.host.backend.as_ref(),
                                        &night,
                                    );
                                });
                            });
                        }
                        Scene::ViewerInteractive | Scene::ViewerViewsBox => {
                            self.host.update_and_render(&ctx, &self.palette);
                        }
                    });

                self.frames_in_scene += 1;
                if self.requested_for_scene != Some(self.current) && self.frames_in_scene >= 10 {
                    self.requested_for_scene = Some(self.current);
                    let path = self.scenes[self.current].out_path.clone();
                    let completed = Arc::clone(&self.completed_count);
                    let ctx_cb = ctx.clone();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::ScreenshotCallback::new(move |img| {
                            let [w, h] = img.size;
                            let mut raw = Vec::with_capacity(w * h * 4);
                            for px in &img.pixels {
                                raw.extend_from_slice(&px.to_srgba_unmultiplied());
                            }
                            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                                let _ = buf.save(&path);
                            }
                            completed.fetch_add(1, Ordering::AcqRel);
                            ctx_cb.request_repaint();
                        }),
                    ));
                } else {
                    ctx.request_repaint();
                }
            }
        }

        let scenes = vec![
            SceneSpec {
                scene: Scene::Strip,
                out_path: docs_dir.join("stories_strip.png"),
                size: Vec2::new(760.0, 380.0),
            },
            SceneSpec {
                scene: Scene::ViewerInteractive,
                out_path: docs_dir.join("stories_viewer.png"),
                size: Vec2::new(1120.0, 780.0),
            },
            SceneSpec {
                scene: Scene::ViewerViewsBox,
                out_path: docs_dir.join("stories_views_box.png"),
                size: Vec2::new(1120.0, 780.0),
            },
        ];

        let initial_size = scenes[0].size;
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Rustgram Stories Capture")
                .with_inner_size(initial_size)
                .with_decorations(false),
            event_loop_builder: Some(Box::new(|builder| {
                #[cfg(target_os = "windows")]
                {
                    use winit::platform::windows::EventLoopBuilderExtWindows as _;
                    builder.with_any_thread(true);
                }
            })),
            ..Default::default()
        };

        let _ = eframe::run_native(
            "Rustgram Stories Capture",
            options,
            Box::new(move |cc| {
                let palette = Palette::day();
                cc.egui_ctx.set_visuals(
                    palette.to_egui_visuals(tg_app_core::stand_in::theme::ThemeMode::Day),
                );
                Ok(Box::new(ScreenshotApp {
                    scenes,
                    current: 0,
                    initialized_scene: None,
                    host: AppStoriesHost::new(),
                    collapsed_strip: StoriesStrip::new(),
                    night_strip: StoriesStrip::expanded(),
                    palette,
                    frames_in_scene: 0,
                    requested_for_scene: None,
                    completed_count: Arc::new(AtomicUsize::new(0)),
                }))
            }),
        );
    }
}
