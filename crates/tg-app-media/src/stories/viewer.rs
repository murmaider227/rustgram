//! Fullscreen story viewer overlay over the window.
//!
//! Port origin:
//! - `Telegram/SourceFiles/media/stories/media_stories_view.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_controller.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_header.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_slider.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_caption_full_view.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_reply.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_reactions.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_recent_views.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_sibling.cpp`

use std::collections::BTreeSet;

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Id, Key, Margin, Painter, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextEdit, Ui, Vec2,
};
use tg_data::{FullStoryId, PeerId, ReactionId, TimeId};
use tg_stories::{
    PauseState, RecentViewsType, ReplyAreaKind, ReplyAreaState, ReplyContext, ReplyPlaceholder,
    ReplyWriteRestriction, SiblingPreview, StealthActivationDecision, StoriesContext, StoriesEvent,
    StoriesStore, Story, StoryAreaHit, StoryMedia, StoryPrivacy, StoryViewerController,
    VIEWS_MENU_ADD_PER_PAGE, ViewerConfig, ViewerEvent,
};

use crate::stories::areas::{paint_story_areas, reaction_display_text};
use crate::stories::backend::{PeerProfile, StoriesBackend};
use crate::stories::boxes::{StoryModalBox, StoryModalOutcome, ViewsFilter, show_story_modal};
use crate::stories::layout::{
    FULL_CONTENT_FADE, SIBLING_FADE, SIBLING_FADE_OVER, SIBLING_SCALE_OVER, STORIES_RADIUS,
    SiblingCardLayout, ViewerLayout, compute_slider_segments, format_compact_count,
    format_story_timestamp, format_temperature,
};
use crate::stories::media_loader::StoryMediaCache;
use crate::stories::strip::paint_avatar_circle;

/// Quick reactions offered in the story reaction picker strip.
pub const QUICK_REACTIONS: &[&str] = &[
    "\u{2764}",
    "\u{1F525}",
    "\u{1F60D}",
    "\u{1F44F}",
    "\u{1F602}",
    "\u{1F62E}",
    "\u{1F622}",
    "\u{1F389}",
];

/// Press-and-hold threshold in milliseconds before a pointer press becomes a hold-to-pause
/// instead of a left/right tap (`180 ms`).
pub const HOLD_TO_PAUSE_THRESHOLD_MS: i64 = 180;

/// Fullscreen story viewer overlay combining `StoryViewerController`, media loaders,
/// sibling previews, interactive areas, caption expand, reply/reactions, and modal boxes.
pub struct StoryViewerOverlay {
    controller: StoryViewerController,
    reply_draft: String,
    reaction_picker_open: bool,
    header_menu_open: bool,
    volume: f64,
    muted: bool,
    volume_popup_open: bool,
    press_started_ms: Option<i64>,
    press_started_pos: Option<Pos2>,
    modal: Option<StoryModalBox>,
    toast_text: Option<(String, i64)>,
    edit_story_requested: Option<FullStoryId>,
    premium_requested: bool,
    update_app_requested: bool,
    sibling_slide_offset: f32,
    last_shown_peer: Option<PeerId>,
    views_loaded_limit: usize,
    pending_video_stream: Option<crate::media::VideoStreamDescriptor>,
}

impl Default for StoryViewerOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl StoryViewerOverlay {
    /// Creates a closed [`StoryViewerOverlay`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            controller: StoryViewerController::new(),
            reply_draft: String::new(),
            reaction_picker_open: false,
            header_menu_open: false,
            volume: 0.85,
            muted: false,
            volume_popup_open: false,
            press_started_ms: None,
            press_started_pos: None,
            modal: None,
            toast_text: None,
            edit_story_requested: None,
            premium_requested: false,
            update_app_requested: false,
            sibling_slide_offset: 0.0,
            last_shown_peer: None,
            views_loaded_limit: VIEWS_MENU_ADD_PER_PAGE,
            pending_video_stream: None,
        }
    }

    /// Takes a pending live-story video stream open request emitted when the user clicks the
    /// `"LIVE"` badge on a live story (`StoryMedia::VideoStream`).
    pub fn take_pending_video_stream(&mut self) -> Option<crate::media::VideoStreamDescriptor> {
        self.pending_video_stream.take()
    }

    /// Returns `true` when the fullscreen story viewer is open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.controller.shown_id().is_some()
    }

    /// Returns a read-only reference to the underlying [`StoryViewerController`].
    #[must_use]
    pub const fn controller(&self) -> &StoryViewerController {
        &self.controller
    }

    /// Returns a mutable reference to the underlying [`StoryViewerController`].
    pub fn controller_mut(&mut self) -> &mut StoryViewerController {
        &mut self.controller
    }

    /// Returns the currently displayed modal box, if any.
    #[must_use]
    pub fn active_modal(&self) -> Option<&StoryModalBox> {
        self.modal.as_ref()
    }

    /// Returns the currently visible toast message, if any.
    #[must_use]
    pub fn active_toast(&self) -> Option<&str> {
        self.toast_text.as_ref().map(|(s, _)| s.as_str())
    }

    /// Takes a pending `"Edit story..."` request triggered from the header menu, if any.
    pub fn take_edit_story_request(&mut self) -> Option<FullStoryId> {
        self.edit_story_requested.take()
    }

    /// Consumes a pending request to open the Telegram Premium screen (emitted by the live
    /// Stealth Mode box when the account lacks Premium).
    pub const fn take_premium_request(&mut self) -> bool {
        std::mem::replace(&mut self.premium_requested, false)
    }

    /// Consumes a pending request to check for application updates (`Core::UpdateApplication`
    /// emitted by clicking `"Update Rustgram"` on an unsupported story card, matching
    /// `Controller::Unsupported` in `media_stories_controller.cpp:287-289`).
    pub const fn take_update_app_request(&mut self) -> bool {
        std::mem::replace(&mut self.update_app_requested, false)
    }

    /// Opens a peer's stories in `context` starting at `peer_id`'s first unread (or first) story.
    pub fn open_peer(
        &mut self,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        peer_id: PeerId,
        context: StoriesContext,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        let Some(story_id) = store
            .source(peer_id)
            .and_then(|s| s.to_open())
            .map(|d| d.id)
        else {
            return false;
        };
        self.open_story(
            store,
            backend,
            FullStoryId::new(peer_id, story_id),
            context,
            now_secs,
            now_ms,
        )
    }

    /// Opens a specific `story_id` in `context`.
    pub fn open_story(
        &mut self,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        story_id: FullStoryId,
        context: StoriesContext,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        let cfg = self.viewer_config_for(backend, story_id.peer);
        let opened = self
            .controller
            .show(store, story_id, context, cfg, now_secs, now_ms);
        if opened {
            self.controller.ready(now_ms);
            self.reply_draft.clear();
            self.reaction_picker_open = false;
            self.header_menu_open = false;
            self.modal = None;
            self.last_shown_peer = Some(story_id.peer);
            self.flush_store_actions(store, backend, now_secs, now_ms);
        }
        opened
    }

    /// Opens the Stealth Mode dialog directly (e.g., from strip context menu `"View anonymously"`).
    pub fn open_stealth_prompt(
        &mut self,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        peer_id: PeerId,
        context: StoriesContext,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        if self.open_peer(store, backend, peer_id, context, now_secs, now_ms) {
            self.modal = Some(StoryModalBox::StealthMode {
                pending_open_peer: Some(peer_id),
            });
            self.controller.set_layer_shown(true, now_ms);
        }
    }

    /// Programmatically opens the Own-Story Stats/Views Box for the currently shown story
    /// (useful for tests and screenshot capture).
    pub fn open_views_box(
        &mut self,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        let Some(story_id) = self.controller.shown_id() else {
            return false;
        };
        let profile = backend.peer_profile(story_id.peer);
        self.views_loaded_limit = VIEWS_MENU_ADD_PER_PAGE;
        let _ = self.controller.load_views(
            store,
            self.views_loaded_limit,
            true,
            profile.can_view_channel_reactions,
        );
        self.flush_store_actions(store, backend, now_secs, now_ms);
        self.controller.on_views_loaded(store);
        self.modal = Some(StoryModalBox::ViewsStats {
            story_id,
            is_channel: profile.is_broadcast,
            filter: ViewsFilter::All,
            search: String::new(),
        });
        self.controller.set_layer_shown(true, now_ms);
        true
    }

    /// Closes the viewer and stops active video playback.
    pub fn close(&mut self, store: &mut StoriesStore, media_cache: &mut StoryMediaCache) {
        self.controller.close(store);
        media_cache.clear_active_video();
        self.modal = None;
        self.header_menu_open = false;
        self.reaction_picker_open = false;
        self.reply_draft.clear();
        self.press_started_ms = None;
        self.press_started_pos = None;
    }

    /// Drives one frame of playback, input handling, and fullscreen overlay painting.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        media_cache: &mut StoryMediaCache,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let Some(shown_id) = self.controller.shown_id() else {
            return;
        };

        // Sync window focus state into controller pause state machine.
        let window_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        self.controller.set_window_active(window_focused, now_ms);

        // Expire transient toast banner.
        if let Some((_, until_ms)) = &self.toast_text
            && now_ms >= *until_ms
        {
            self.toast_text = None;
        }

        // Advance sibling horizontal slide animation smoothly toward 0.
        if self.sibling_slide_offset.abs() > 0.5 {
            self.sibling_slide_offset *= 0.78;
            ctx.request_repaint();
        } else {
            self.sibling_slide_offset = 0.0;
        }

        // Ensure current story + sibling media are loaded and tick photo/video playback.
        let cfg = self.viewer_config_for(backend, shown_id.peer);
        self.drive_media_and_playback(
            ctx,
            store,
            backend,
            media_cache,
            shown_id,
            cfg,
            now_secs,
            now_ms,
        );

        // Drain viewer events (e.g., auto-advance reached end -> Close, video pause changed, share/stealth/report).
        if self.process_viewer_events(store, backend, media_cache, now_secs, now_ms) {
            return;
        }
        let Some(shown_id) = self.controller.shown_id() else {
            return;
        };
        let Ok(story) = store.lookup(shown_id).cloned() else {
            self.close(store, media_cache);
            return;
        };

        // Track peer transition for sibling slide effect.
        if self.last_shown_peer != Some(shown_id.peer) {
            self.last_shown_peer = Some(shown_id.peer);
        }

        // Handle keyboard navigation when no text field / modal consumes input.
        if self.modal.is_none() && !ctx.egui_wants_keyboard_input() {
            let cfg_now = self.viewer_config_for(backend, shown_id.peer);
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                if self.header_menu_open {
                    self.header_menu_open = false;
                    self.controller.set_menu_shown(false, now_ms);
                } else if self.reaction_picker_open {
                    self.reaction_picker_open = false;
                    self.controller.set_reply_active(false, now_ms);
                } else if self.controller.is_caption_expanded() {
                    self.controller.caption_closed(now_ms);
                } else {
                    self.close(store, media_cache);
                    return;
                }
            } else if ctx.input(|i| i.key_pressed(Key::ArrowRight)) {
                if !self
                    .controller
                    .subjump_for(store, 1, cfg_now, now_secs, now_ms)
                {
                    self.close(store, media_cache);
                    return;
                }
                self.sibling_slide_offset = 54.0;
            } else if ctx.input(|i| i.key_pressed(Key::ArrowLeft)) {
                let _ = self
                    .controller
                    .subjump_for(store, -1, cfg_now, now_secs, now_ms);
                self.sibling_slide_offset = -54.0;
            } else if ctx.input(|i| i.key_pressed(Key::Space)) {
                let paused = !self.controller.is_explicitly_paused();
                self.controller.toggle_paused(paused, now_ms);
            } else if ctx.input(|i| i.key_pressed(Key::M)) {
                self.muted = !self.muted;
                if let Some(vid) = media_cache.active_video_mut(shown_id) {
                    vid.set_volume(self.volume, self.muted);
                }
            }
        }

        // Keep requesting repaints while actively playing so the progress slider animates smoothly.
        if self.controller.playing_allowed() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }

        let viewport = ctx.input(|i| i.content_rect());
        let layout = ViewerLayout::compute(viewport);
        let peer_profile = backend.peer_profile(shown_id.peer);

        // Preload sibling textures so side preview cards render their actual gradient/video cover.
        let left_sib = self.controller.sibling_left().cloned();
        let right_sib = self.controller.sibling_right().cloned();
        for sib in left_sib.iter().chain(right_sib.iter()) {
            let sid = sib.full_id();
            if media_cache.preview_texture(sid).is_none()
                && let Some(asset) = backend.load_story_media(sid)
            {
                let _ = media_cache.ensure_photo(ctx, sid, &asset);
            }
        }

        let mut close_requested = false;
        let mut jump_delta: Option<i32> = None;
        let mut subjump_delta: Option<i32> = None;
        let mut open_views_requested = false;

        egui::Area::new(Id::unique("tg_stories_viewer_overlay"))
            .order(egui::Order::Foreground)
            .fade_in(false)
            .fixed_pos(viewport.min)
            .show(ctx, |ui| {
                // Fullscreen dark scrim backdrop.
                let (bg_rect, bg_resp) = ui.allocate_exact_size(viewport.size(), Sense::click());
                let painter = ui.painter().clone();
                painter.rect_filled(
                    bg_rect,
                    0.0,
                    Color32::from_rgba_unmultiplied(11, 14, 19, 250),
                );

                // Close when clicking outer backdrop (`Controller::closeByClickAt`).
                if bg_resp.clicked()
                    && let Some(pos) = bg_resp.interact_pointer_pos()
                    && layout.close_by_click_at(pos)
                    && self.modal.is_none()
                {
                    let in_left = left_sib
                        .as_ref()
                        .is_some_and(|_| layout.sibling_left.geometry.contains(pos));
                    let in_right = right_sib
                        .as_ref()
                        .is_some_and(|_| layout.sibling_right.geometry.contains(pos));
                    if !in_left && !in_right {
                        close_requested = true;
                    }
                }

                // 1. Sibling preview cards on the left and right.
                if let Some(left) = &left_sib
                    && self.paint_sibling_card(
                        ui,
                        &layout.sibling_left,
                        left,
                        backend,
                        media_cache,
                        self.sibling_slide_offset,
                        "story_sibling_left",
                    )
                {
                    jump_delta = Some(-1);
                }
                if let Some(right) = &right_sib
                    && self.paint_sibling_card(
                        ui,
                        &layout.sibling_right,
                        right,
                        backend,
                        media_cache,
                        self.sibling_slide_offset,
                        "story_sibling_right",
                    )
                {
                    jump_delta = Some(1);
                }

                // 2. Main 9:16 portrait story card (photo, video, live stream, or unsupported).
                let content_rect = layout
                    .content
                    .translate(Vec2::new(self.sibling_slide_offset * 0.25, 0.0));
                self.paint_story_content_card(
                    ui,
                    content_rect,
                    &story,
                    store,
                    backend,
                    media_cache,
                    now_secs,
                    now_ms,
                    &mut subjump_delta,
                );

                // 3. Top segmented progress slider.
                self.paint_slider(&painter, layout.slider);

                // 4. Header (avatar, privacy badge, name, counter, timestamp, repost, play/pause, mute, menu).
                self.paint_header(
                    ui,
                    layout.header,
                    &story,
                    &peer_profile,
                    store,
                    backend,
                    media_cache,
                    now_secs,
                    now_ms,
                );

                // 5. Caption overlay (collapsed 2-line + "... more") or expanded scrollable caption sheet.
                self.paint_caption_overlay(
                    ui,
                    content_rect,
                    layout.header,
                    &story,
                    backend,
                    now_ms,
                );

                // 6. Left/Right side navigation arrows outside story card.
                if self.controller.subjump_available(-1)
                    && paint_side_arrow_button(ui, layout.prev_arrow, true, "story_prev_arrow")
                {
                    subjump_delta = Some(-1);
                }
                if self.controller.subjump_available(1)
                    && paint_side_arrow_button(ui, layout.next_arrow, false, "story_next_arrow")
                {
                    subjump_delta = Some(1);
                }

                // 7. Top-right Close (X) button.
                if paint_close_overlay_button(ui, layout.close_button) {
                    close_requested = true;
                }

                // 8. Footer: Reply composer OR Recent views bar (Self / Channel / Changelog).
                self.paint_footer(
                    ui,
                    layout.controls,
                    &story,
                    store,
                    backend,
                    now_secs,
                    now_ms,
                    &mut open_views_requested,
                );

                // 9. Reaction picker popover above footer.
                if self.reaction_picker_open {
                    self.paint_reaction_picker(ui, layout.controls, &story, store, now_ms);
                }

                // 10. Toast notification banner at bottom.
                if let Some((msg, _)) = &self.toast_text {
                    paint_viewer_toast(&painter, viewport, msg);
                }
            });

        // Header `•••` context menu popup.
        if self.header_menu_open {
            self.paint_header_menu(ctx, layout.header, &story, store, backend, now_secs, now_ms);
        }

        // Apply navigation requests emitted during painting.
        if let Some(delta) = jump_delta {
            let cfg_nav = self.viewer_config_for(backend, shown_id.peer);
            if self
                .controller
                .jump_for(store, delta, cfg_nav, now_secs, now_ms)
            {
                self.sibling_slide_offset = if delta > 0 { 64.0 } else { -64.0 };
                self.controller.ready(now_ms);
            }
        } else if let Some(delta) = subjump_delta {
            let cfg_nav = self.viewer_config_for(backend, shown_id.peer);
            if !self
                .controller
                .subjump_for(store, delta, cfg_nav, now_secs, now_ms)
            {
                close_requested = true;
            } else {
                self.controller.ready(now_ms);
            }
        }

        if open_views_requested {
            self.open_views_box(store, backend, now_secs, now_ms);
        }

        // Render active modal box (ViewsStats / StealthMode / Share / Report / ConfirmDelete).
        if let Some(mut modal) = self.modal.take() {
            let views_snapshot = store
                .lookup(shown_id)
                .map(|s| {
                    if backend.peer_profile(shown_id.peer).is_broadcast {
                        s.channel_reactions.clone()
                    } else {
                        s.views.clone()
                    }
                })
                .unwrap_or_default();
            let outcome = show_story_modal(
                ctx,
                viewport,
                &mut modal,
                &views_snapshot,
                store.stealth_mode(),
                backend,
                now_secs,
            );
            let keep_open =
                self.handle_modal_outcome(outcome, &modal, store, backend, now_secs, now_ms);
            if keep_open {
                self.modal = Some(modal);
            } else {
                self.controller.set_layer_shown(false, now_ms);
            }
        }

        // Flush any newly emitted store actions & events.
        self.flush_store_actions(store, backend, now_secs, now_ms);
        let _ = self.process_viewer_events(store, backend, media_cache, now_secs, now_ms);

        if close_requested {
            self.close(store, media_cache);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn drive_media_and_playback(
        &mut self,
        ctx: &egui::Context,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        media_cache: &mut StoryMediaCache,
        shown_id: FullStoryId,
        cfg: ViewerConfig,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let Ok(story) = store.lookup(shown_id).cloned() else {
            return;
        };
        match &story.media {
            StoryMedia::Photo(_) | StoryMedia::VideoStream(_) => {
                media_cache.clear_active_video();
                if let Some(asset) = backend.load_story_media(shown_id) {
                    let _ = media_cache.ensure_photo(ctx, shown_id, &asset);
                }
                if !self.controller.is_video_stream() {
                    self.controller.tick(store, cfg, now_secs, now_ms);
                }
            }
            StoryMedia::Video { silent, .. } => {
                if let Some(asset) = backend.load_story_media(shown_id) {
                    if let Some(video) = media_cache.ensure_video(
                        Some(ctx),
                        shown_id,
                        &asset,
                        *silent,
                        self.volume,
                        self.muted,
                        now_ms,
                    ) {
                        video.set_paused(!self.controller.playing_allowed(), now_ms);
                        let track_state = video.step(Some(ctx), now_ms);
                        self.controller.update_video_playback(
                            store,
                            track_state,
                            cfg,
                            now_secs,
                            now_ms,
                        );
                    } else {
                        // Fallback when mock video story uses a generated photo cover (no MP4 attached).
                        let _ = media_cache.ensure_photo(ctx, shown_id, &asset);
                        self.controller.tick(store, cfg, now_secs, now_ms);
                    }
                }
            }
            StoryMedia::Unsupported => {
                media_cache.clear_active_video();
                self.controller.tick(store, cfg, now_secs, now_ms);
            }
        }
    }

    fn process_viewer_events(
        &mut self,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        media_cache: &mut StoryMediaCache,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        let events = self.controller.take_events();
        for ev in events {
            match ev {
                ViewerEvent::Close => {
                    self.close(store, media_cache);
                    return true;
                }
                ViewerEvent::StoryShown { id, .. } => {
                    self.controller.ready(now_ms);
                    if let Ok(story) = store.lookup(id)
                        && !matches!(story.media, StoryMedia::Video { .. })
                    {
                        media_cache.clear_active_video();
                    }
                }
                ViewerEvent::VideoPauseChanged(paused) => {
                    if let Some(id) = self.controller.shown_id()
                        && let Some(vid) = media_cache.active_video_mut(id)
                    {
                        vid.set_paused(paused, now_ms);
                    }
                }
                ViewerEvent::ShareRequested(capability) => {
                    self.modal = Some(StoryModalBox::Share {
                        capability,
                        selected: BTreeSet::new(),
                        search: String::new(),
                        comment: String::new(),
                    });
                    self.controller.set_layer_shown(true, now_ms);
                }
                ViewerEvent::ReportRequested(story_id) => {
                    self.modal = Some(StoryModalBox::Report {
                        story_id,
                        selected_reason: 0,
                    });
                    self.controller.set_layer_shown(true, now_ms);
                }
                ViewerEvent::StealthModeRequested(decision) => match decision {
                    StealthActivationDecision::ReadyToActivate => {
                        self.flush_store_actions(store, backend, now_secs, now_ms);
                        self.show_toast("Stealth Mode enabled for 25 minutes", now_ms);
                    }
                    StealthActivationDecision::AlreadyActive { seconds_left } => {
                        self.show_toast(
                            format!(
                                "Stealth Mode already active ({})",
                                tg_stories::time_left_text(seconds_left)
                            ),
                            now_ms,
                        );
                    }
                    StealthActivationDecision::RequiresPremium
                    | StealthActivationDecision::InCooldown { .. } => {
                        self.modal = Some(StoryModalBox::StealthMode {
                            pending_open_peer: None,
                        });
                        self.controller.set_layer_shown(true, now_ms);
                    }
                },
                ViewerEvent::UnsupportedStoryShown(_) | ViewerEvent::MoreViewsLoaded => {}
            }
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_sibling_card(
        &self,
        ui: &mut Ui,
        card: &SiblingCardLayout,
        sibling: &SiblingPreview,
        backend: &dyn StoriesBackend,
        media_cache: &StoryMediaCache,
        slide_offset: f32,
        id_salt: &str,
    ) -> bool {
        if card.geometry.width() < 48.0 || card.geometry.height() < 64.0 {
            return false;
        }
        let geom = card.geometry.translate(Vec2::new(slide_offset * 0.35, 0.0));
        let resp = ui.interact(geom, Id::unique(id_salt), Sense::click());
        let scale = if resp.hovered() {
            1.0 + SIBLING_SCALE_OVER
        } else {
            1.0
        };
        let draw_rect = Rect::from_center_size(geom.center(), geom.size() * scale);
        let painter = ui.painter();

        // Sibling media preview or dark fallback.
        painter.rect_filled(
            draw_rect,
            CornerRadius::same(STORIES_RADIUS as u8),
            Color32::from_rgb(0x18, 0x20, 0x2A),
        );
        if let Some(tex) = media_cache.preview_texture(sibling.full_id()) {
            painter.image(
                tex.id(),
                draw_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // Dimming overlay (`kSiblingFade = 0.5` -> `kSiblingFadeOver = 0.4`).
        let fade = if resp.hovered() {
            SIBLING_FADE_OVER
        } else {
            SIBLING_FADE
        };
        painter.rect_filled(
            draw_rect,
            CornerRadius::same(STORIES_RADIUS as u8),
            Color32::from_rgba_unmultiplied(0, 0, 0, (fade * 255.0).round() as u8),
        );

        let profile = backend.peer_profile(sibling.peer_id);
        let up_r = (card.userpic.width() * 0.5 * scale).max(16.0);
        let up_c = Pos2::new(draw_rect.center().x, draw_rect.center().y - up_r * 0.4);
        painter.circle_stroke(
            up_c,
            up_r + 2.5,
            Stroke::new(2.0, Color32::from_rgba_unmultiplied(255, 255, 255, 210)),
        );
        paint_avatar_circle(painter, up_c, up_r, &profile, up_r * 0.65);

        let name = if sibling.is_self {
            "My Story"
        } else {
            &profile.short_name
        };
        let opacity = if resp.hovered() { 255 } else { 215 };
        painter.text(
            Pos2::new(draw_rect.center().x, up_c.y + up_r + 16.0),
            Align2::CENTER_CENTER,
            name,
            FontId::proportional(card.name_font_size),
            Color32::from_rgba_unmultiplied(255, 255, 255, opacity),
        );

        resp.clicked()
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_story_content_card(
        &mut self,
        ui: &mut Ui,
        content_rect: Rect,
        story: &Story,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        media_cache: &StoryMediaCache,
        now_secs: TimeId,
        now_ms: i64,
        subjump_delta: &mut Option<i32>,
    ) {
        let painter = ui.painter();
        let rounding = CornerRadius::same(STORIES_RADIUS as u8);
        let bg_color = if story.unsupported() {
            // `st::storiesComposeBg` (`Controller::Unsupported`, `media_stories_controller.cpp:234`).
            Color32::from_rgb(0x21, 0x21, 0x21)
        } else {
            Color32::from_rgb(0x14, 0x1A, 0x22)
        };
        painter.rect_filled(content_rect, rounding, bg_color);

        let full_id = story.full_id();
        let mut unsupported_btn_rect: Option<Rect> = None;

        if story.unsupported() {
            // Reserve button geometry here so `interact_rect` excludes it from tap-navigation;
            // the label and interactive button are painted after the scrims/interact layer so
            // they sit on top and receive pointer clicks cleanly (`Controller::Unsupported` in
            // `media_stories_controller.cpp:230-290` + `media_view.style:1033-1047`).
            let text_max_w = (content_rect.width() - 48.0).max(160.0);
            let label_galley = layout_unsupported_story_label(painter, text_max_w);
            let (_, btn_rect) =
                compute_unsupported_story_rects(content_rect, label_galley.size().y);
            unsupported_btn_rect = Some(btn_rect);
        } else if let Some(vid_tex) = media_cache.active_video_mut_ref_tex(full_id) {
            painter.image(
                vid_tex.id(),
                content_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else if let Some(photo_tex) = media_cache.preview_texture(full_id) {
            painter.image(
                photo_tex.id(),
                content_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // Top and bottom legibility shadow gradients (`storiesShadowTop`, `storiesShadowBottom`).
        paint_vertical_scrim(
            painter,
            Rect::from_min_size(content_rect.min, Vec2::new(content_rect.width(), 96.0)),
            135,
            0,
        );
        paint_vertical_scrim(
            painter,
            Rect::from_min_size(
                Pos2::new(content_rect.left(), content_rect.bottom() - 135.0),
                Vec2::new(content_rect.width(), 135.0),
            ),
            0,
            175,
        );

        // Live video stream badge (clickable -> opens `Media::View::VideoStream` overlay).
        if let Some(stream) = story.media.as_video_stream() {
            let live_rect = Rect::from_min_size(
                Pos2::new(content_rect.left() + 14.0, content_rect.top() + 58.0),
                Vec2::new(132.0, 24.0),
            );
            let live_resp = ui.interact(
                live_rect,
                Id::unique(("story_live_stream_open", stream.call_id.0)),
                Sense::click(),
            );
            if live_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            painter.rect_filled(
                live_rect,
                CornerRadius::same(12),
                Color32::from_rgba_unmultiplied(
                    0xE5,
                    0x39,
                    0x35,
                    if live_resp.hovered() { 255 } else { 230 },
                ),
            );
            painter.text(
                live_rect.center(),
                Align2::CENTER_CENTER,
                format!(
                    "LIVE • {} watching",
                    format_compact_count(stream.viewers_count)
                ),
                FontId::proportional(11.0),
                Color32::WHITE,
            );
            if live_resp.clicked() {
                let peer_title = backend.peer_profile(stream.peer_id).name;
                self.pending_video_stream = Some(crate::media::VideoStreamDescriptor {
                    call_id: stream.call_id.0 as i64,
                    access_hash: stream.access_hash as u64,
                    peer_id: stream.peer_id.0,
                    title: if peer_title.is_empty() {
                        "Live Story".to_owned()
                    } else {
                        format!("{peer_title} · Live")
                    },
                    call_link_slug: String::new(),
                    join_msg_id: story.id,
                    rtmp_stream: stream.rtmp_stream,
                    stream_url: None,
                    stream_dc_id: 0,
                    is_live_story: true,
                });
            }
        }

        // Video indicator chip for video stories.
        if let StoryMedia::Video { document, silent } = &story.media {
            let dur = document.duration.unwrap_or(5.0).round() as i32;
            let badge = if *silent {
                format!("VIDEO • 0:{dur:02} (Silent)")
            } else {
                format!("VIDEO • 0:{dur:02}")
            };
            let chip_rect = Rect::from_min_size(
                Pos2::new(content_rect.left() + 14.0, content_rect.top() + 58.0),
                Vec2::new(124.0, 22.0),
            );
            painter.rect_filled(
                chip_rect,
                CornerRadius::same(11),
                Color32::from_rgba_unmultiplied(0, 0, 0, 120),
            );
            painter.text(
                chip_rect.center(),
                Align2::CENTER_CENTER,
                badge,
                FontId::proportional(10.5),
                Color32::WHITE,
            );
        }

        // Interactive content area pointer interaction (hold-to-pause + area hits + left/right taps).
        let interact_rect = Rect::from_min_max(
            Pos2::new(content_rect.left(), content_rect.top() + 54.0),
            Pos2::new(content_rect.right(), content_rect.bottom() - 62.0),
        );
        let resp = ui.interact(
            interact_rect,
            Id::unique("story_content_interact"),
            Sense::click_and_drag(),
        );
        let hovered_norm = resp
            .hover_pos()
            .and_then(|pos| normalized_in(content_rect, pos));

        // Paint rotated interactive areas over the media frame.
        paint_story_areas(
            painter,
            story,
            content_rect,
            self.controller.weather_in_celsius(),
            hovered_norm,
        );

        // Paint unsupported-story label and interactive "Update Rustgram" button above the
        // content interaction layer so clicks hit the button directly (`Controller::Unsupported`,
        // `media_stories_controller.cpp:253-289`, `media_view.style:1033-1047`).
        if story.unsupported() {
            let text_max_w = (content_rect.width() - 48.0).max(160.0);
            let label_galley = layout_unsupported_story_label(ui.painter(), text_max_w);
            let (text_pos, btn_rect) =
                compute_unsupported_story_rects(content_rect, label_galley.size().y);
            ui.painter().galley(text_pos, label_galley, Color32::WHITE);

            let update_label = tg_lang::tr::lng_update_telegram().replace("Telegram", "Rustgram");
            let btn_resp = ui.interact(
                btn_rect,
                ui.scope_id()
                    .with(("story_unsupported_update", full_id.story)),
                Sense::click(),
            );
            btn_resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, update_label.clone())
            });
            if btn_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let btn_bg = if btn_resp.hovered() {
                Color32::from_rgb(0x34, 0x97, 0xC9)
            } else {
                Color32::from_rgb(0x39, 0xA5, 0xDB)
            };
            ui.painter()
                .rect_filled(btn_rect, CornerRadius::same(6), btn_bg);
            if btn_resp.has_focus() {
                ui.painter().rect_stroke(
                    btn_rect.expand(2.0),
                    CornerRadius::same(8),
                    Stroke::new(2.0, Color32::WHITE),
                    egui::StrokeKind::Outside,
                );
            }
            ui.painter().text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                update_label,
                FontId::new(14.0, tg_ui::fonts::family_semibold()),
                Color32::WHITE,
            );
            if btn_resp.clicked() {
                self.update_app_requested = true;
                self.show_toast("Checking for Rustgram updates\u{2026}".to_owned(), now_ms);
            }
        }

        // Dim story canvas when reply composer or expanded caption is active (`kFullContentFade`).
        if self.controller.is_content_faded() {
            painter.rect_filled(
                content_rect,
                rounding,
                Color32::from_rgba_unmultiplied(0, 0, 0, (FULL_CONTENT_FADE * 255.0).round() as u8),
            );
        }

        // Handle press-and-hold vs quick tap on the content canvas.
        if self.modal.is_none() {
            if resp.is_pointer_button_down_on() {
                if self.press_started_ms.is_none() {
                    self.press_started_ms = Some(now_ms);
                    self.press_started_pos = resp.interact_pointer_pos();
                } else if let Some(started) = self.press_started_ms
                    && now_ms - started >= HOLD_TO_PAUSE_THRESHOLD_MS
                {
                    self.controller.content_pressed(true, now_ms);
                }
            } else if let Some(started) = self.press_started_ms.take() {
                let held_ms = now_ms - started;
                let was_hold = held_ms >= HOLD_TO_PAUSE_THRESHOLD_MS;
                self.controller.content_pressed(false, now_ms);
                if !was_hold && let Some(pos) = self.press_started_pos.take() {
                    if unsupported_btn_rect.is_some_and(|r| r.contains(pos)) {
                        // Click handled by the "Update Rustgram" CTA button above.
                    } else if self.controller.is_caption_expanded() {
                        self.controller.caption_closed(now_ms);
                    } else if let Some((nx, ny)) = normalized_in(content_rect, pos)
                        && let Some(hit) = self.controller.hit_test_area(store, nx, ny)
                    {
                        self.handle_area_hit(hit, store, backend, now_secs, now_ms);
                    } else {
                        let rel_x = (pos.x - content_rect.left()) / content_rect.width().max(1.0);
                        *subjump_delta = Some(if rel_x < 0.30 { -1 } else { 1 });
                    }
                } else {
                    self.press_started_pos = None;
                }
            }
        }
    }

    fn handle_area_hit(
        &mut self,
        hit: StoryAreaHit,
        store: &mut StoriesStore,
        _backend: &mut dyn StoriesBackend,
        _now_secs: TimeId,
        now_ms: i64,
    ) {
        match hit {
            StoryAreaHit::Weather { millicelsius, .. } => {
                let celsius = self.controller.toggle_weather_unit();
                let formatted = format_temperature(millicelsius, celsius);
                self.show_toast(format!("Weather unit toggled: {formatted}"), now_ms);
            }
            StoryAreaHit::SuggestedReaction { reaction } => {
                let glyph = reaction_display_text(&reaction);
                self.controller.send_reaction(store, Some(reaction));
                self.show_toast(format!("Reacted {glyph} to story"), now_ms);
            }
            StoryAreaHit::Location { point, title } => {
                let name = if title.is_empty() {
                    format!("{:.4}, {:.4}", point.lat, point.lon)
                } else {
                    format!("{title} ({:.4}, {:.4})", point.lat, point.lon)
                };
                self.show_toast(format!("Location: {name}"), now_ms);
            }
            StoryAreaHit::Url { url } => {
                self.show_toast(format!("Link: {url}"), now_ms);
            }
            StoryAreaHit::ChannelPost { item_id } => {
                self.show_toast(format!("Open channel post #{}", item_id.msg.0), now_ms);
            }
        }
    }

    fn paint_slider(&self, painter: &Painter, slider_rect: Rect) {
        let state = self.controller.slider_state();
        let segments = compute_slider_segments(slider_rect, state);
        for seg in segments {
            let r = (seg.rect.height() / 2.0).max(1.0);
            let inactive = Color32::from_rgba_unmultiplied(255, 255, 255, 102);
            let active = Color32::WHITE;
            painter.rect_filled(seg.rect, CornerRadius::same(r.round() as u8), inactive);
            if seg.fill_ratio > 0.0 {
                let min_w = seg.rect.height();
                let active_w = (seg.rect.width() * seg.fill_ratio)
                    .max(min_w)
                    .min(seg.rect.width());
                let active_rect =
                    Rect::from_min_size(seg.rect.min, Vec2::new(active_w, seg.rect.height()));
                painter.rect_filled(active_rect, CornerRadius::same(r.round() as u8), active);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_header(
        &mut self,
        ui: &mut Ui,
        header_rect: Rect,
        story: &Story,
        profile: &PeerProfile,
        _store: &mut StoriesStore,
        backend: &dyn StoriesBackend,
        media_cache: &mut StoryMediaCache,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let painter = ui.painter();
        let avatar_c = Pos2::new(header_rect.left() + 26.0, header_rect.center().y);
        paint_avatar_circle(painter, avatar_c, 14.0, profile, 10.5);

        // Privacy badge overlay on avatar (`LookupPrivacyBadge` in `media_stories_header.cpp:79-102`).
        if let Some(privacy) = StoryViewerController::privacy_badge(story.privacy) {
            let bc = Pos2::new(avatar_c.x + 11.0, avatar_c.y + 10.0);
            match privacy {
                StoryPrivacy::CloseFriends => {
                    painter.circle_filled(bc, 7.0, Color32::from_rgb(0x43, 0xB5, 0x4E));
                    // Crisp vector 4-point star inside badge.
                    let star = vec![
                        Pos2::new(bc.x, bc.y - 4.0),
                        Pos2::new(bc.x + 1.3, bc.y - 1.3),
                        Pos2::new(bc.x + 4.0, bc.y),
                        Pos2::new(bc.x + 1.3, bc.y + 1.3),
                        Pos2::new(bc.x, bc.y + 4.0),
                        Pos2::new(bc.x - 1.3, bc.y + 1.3),
                        Pos2::new(bc.x - 4.0, bc.y),
                        Pos2::new(bc.x - 1.3, bc.y - 1.3),
                    ];
                    painter.add(egui::Shape::convex_polygon(
                        star,
                        Color32::WHITE,
                        Stroke::NONE,
                    ));
                }
                StoryPrivacy::Contacts => {
                    painter.circle_filled(bc, 7.0, Color32::from_rgb(0x40, 0xA7, 0xE3));
                    painter.circle_filled(bc, 2.8, Color32::WHITE);
                }
                StoryPrivacy::SelectedContacts => {
                    painter.circle_filled(bc, 7.0, Color32::from_rgb(0xF5, 0x9E, 0x0B));
                    let diamond = vec![
                        Pos2::new(bc.x, bc.y - 3.5),
                        Pos2::new(bc.x + 3.5, bc.y),
                        Pos2::new(bc.x, bc.y + 3.5),
                        Pos2::new(bc.x - 3.5, bc.y),
                    ];
                    painter.add(egui::Shape::convex_polygon(
                        diamond,
                        Color32::WHITE,
                        Stroke::NONE,
                    ));
                }
                StoryPrivacy::Public | StoryPrivacy::Other => {}
            }
        }

        // Peer name + verified tag + story counter (`• 1/3`).
        let name_x = header_rect.left() + 48.0;
        let total = self.controller.shown_count();
        let counter = if total > 1 {
            format!(" • {}/{}", self.controller.current_index() + 1, total)
        } else {
            String::new()
        };
        let verified = if profile.is_verified { " \u{2713}" } else { "" };
        painter.text(
            Pos2::new(name_x, header_rect.top() + 12.0),
            Align2::LEFT_CENTER,
            format!("{}{verified}{counter}", profile.name),
            FontId::proportional(13.5),
            Color32::WHITE,
        );

        // Timestamp + edited + repost attribution.
        let mut details = format_story_timestamp(story.date, now_secs);
        if story.edited {
            details.push_str(" • edited");
        }
        if story.is_repost() {
            let src_name = story
                .repost
                .source_peer
                .map(|pid| backend.peer_profile(pid).name)
                .unwrap_or_else(|| story.repost.source_name.clone());
            details = format!("\u{21BB} {src_name} • {details}");
        }
        painter.text(
            Pos2::new(name_x, header_rect.top() + 29.0),
            Align2::LEFT_CENTER,
            details,
            FontId::proportional(11.5),
            Color32::from_rgba_unmultiplied(255, 255, 255, 204),
        );

        // Right-side header controls: Menu (`•••`), Volume/Mute (video), Play/Pause.
        let mut rx = header_rect.right() - 10.0;
        let cy = header_rect.center().y;

        // Context menu button (`•••`).
        let menu_rect = Rect::from_center_size(Pos2::new(rx - 14.0, cy), Vec2::splat(28.0));
        if paint_header_icon_button(ui, menu_rect, "•••", "story_hdr_menu", "More options") {
            self.header_menu_open = !self.header_menu_open;
            self.controller
                .set_menu_shown(self.header_menu_open, now_ms);
        }
        rx -= 32.0;

        // Volume / Mute button for video stories.
        if matches!(story.media, StoryMedia::Video { silent: false, .. }) {
            let vol_rect = Rect::from_center_size(Pos2::new(rx - 14.0, cy), Vec2::splat(28.0));
            let vol_label = if self.muted || self.volume <= 0.01 {
                "\u{1F507}"
            } else {
                "\u{1F50A}"
            };
            if paint_header_icon_button(
                ui,
                vol_rect,
                vol_label,
                "story_hdr_vol",
                "Mute / Unmute (M)",
            ) {
                self.muted = !self.muted;
                if let Some(vid) = media_cache.active_video_mut(story.full_id()) {
                    vid.set_volume(self.volume, self.muted);
                }
            }
            if vol_rect.contains(ui.input(|i| i.pointer.hover_pos().unwrap_or(Pos2::ZERO))) {
                self.volume_popup_open = true;
            }
            rx -= 32.0;
        } else {
            self.volume_popup_open = false;
        }

        // Play / Pause button (`PauseState`) with crisp monochrome vector geometry.
        if !self.controller.is_video_stream() {
            let pp_rect = Rect::from_center_size(Pos2::new(rx - 14.0, cy), Vec2::splat(28.0));
            let paused = matches!(
                self.controller.pause_state(),
                PauseState::Paused | PauseState::InactivePaused
            );
            let resp = ui.interact(pp_rect, Id::unique("story_hdr_pause"), Sense::click());
            let c = pp_rect.center();
            if resp.hovered() {
                ui.painter().circle_filled(
                    c,
                    13.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 34),
                );
            }
            let fg = Color32::from_rgba_unmultiplied(
                255,
                255,
                255,
                if resp.hovered() { 255 } else { 220 },
            );
            if paused {
                let tri = vec![
                    Pos2::new(c.x - 4.0, c.y - 6.0),
                    Pos2::new(c.x + 6.0, c.y),
                    Pos2::new(c.x - 4.0, c.y + 6.0),
                ];
                ui.painter()
                    .add(egui::Shape::convex_polygon(tri, fg, Stroke::NONE));
            } else {
                ui.painter().rect_filled(
                    Rect::from_min_size(Pos2::new(c.x - 4.5, c.y - 5.5), Vec2::new(3.0, 11.0)),
                    CornerRadius::same(1),
                    fg,
                );
                ui.painter().rect_filled(
                    Rect::from_min_size(Pos2::new(c.x + 1.5, c.y - 5.5), Vec2::new(3.0, 11.0)),
                    CornerRadius::same(1),
                    fg,
                );
            }
            if resp
                .on_hover_text(if paused {
                    "Resume (Space)"
                } else {
                    "Pause (Space)"
                })
                .clicked()
            {
                self.controller.toggle_paused(!paused, now_ms);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_header_menu(
        &mut self,
        ctx: &egui::Context,
        header_rect: Rect,
        story: &Story,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let peer_ctx = store.peer_meta(story.peer_id).ctx;
        let cfg = self.viewer_config_for(backend, story.peer_id);
        let mut close_menu = false;

        let menu_pos = Pos2::new(header_rect.right() - 210.0, header_rect.bottom() + 4.0);
        let area = egui::Area::new(Id::unique("story_viewer_header_menu"))
            .order(egui::Order::Tooltip)
            .fixed_pos(menu_pos)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(Color32::from_rgb(0x20, 0x29, 0x33))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(0x34, 0x42, 0x52)))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(196.0);
                        if story.can_share(now_secs) && ui.button("Share story...").clicked() {
                            let _ = self.controller.share_requested(store, now_secs);
                            close_menu = true;
                        }
                        if story.has_direct_link(peer_ctx, now_secs)
                            && ui.button("Copy story link").clicked()
                        {
                            if let Some(link) = backend.export_story_link(story.full_id()) {
                                ctx.copy_text(link.clone());
                                self.show_toast(format!("Copied {link}"), now_ms);
                            }
                            close_menu = true;
                        }
                        if self.controller.allow_stealth_mode(store, true)
                            && ui.button("Stealth Mode...").clicked()
                        {
                            self.modal = Some(StoryModalBox::StealthMode {
                                pending_open_peer: None,
                            });
                            self.controller.set_layer_shown(true, now_ms);
                            close_menu = true;
                        }
                        if peer_ctx.is_self || peer_ctx.can_edit_stories {
                            if ui.button("Edit story...").clicked() {
                                self.edit_story_requested = Some(story.full_id());
                                close_menu = true;
                            }
                            let save_label = if story.in_profile {
                                "Remove from profile"
                            } else {
                                "Save to profile"
                            };
                            if ui.button(save_label).clicked() {
                                let next = !story.in_profile;
                                let _ = self.controller.toggle_in_profile_requested(
                                    store, next, cfg, now_secs, now_ms,
                                );
                                self.show_toast(
                                    if next {
                                        "Saved to profile"
                                    } else {
                                        "Removed from profile"
                                    },
                                    now_ms,
                                );
                                close_menu = true;
                            }
                            if story.in_profile {
                                let pin_label = if story.pinned_to_top {
                                    "Unpin from top"
                                } else {
                                    "Pin to top"
                                };
                                if ui.button(pin_label).clicked() {
                                    let _ = self.controller.toggle_pinned_to_top_requested(
                                        store,
                                        !story.pinned_to_top,
                                    );
                                    close_menu = true;
                                }
                            }
                        }
                        if story.can_report(peer_ctx) && ui.button("Report story...").clicked() {
                            let _ = self.controller.report_requested(store);
                            close_menu = true;
                        }
                        if story.can_delete(peer_ctx) {
                            ui.separator();
                            if ui
                                .button(
                                    RichText::new("Delete story")
                                        .color(Color32::from_rgb(0xEC, 0x5B, 0x5B)),
                                )
                                .clicked()
                            {
                                self.modal = Some(StoryModalBox::ConfirmDelete {
                                    story_id: story.full_id(),
                                });
                                self.controller.set_layer_shown(true, now_ms);
                                close_menu = true;
                            }
                        }
                    });
            });

        if ctx.input(|i| i.pointer.any_pressed())
            && !area
                .response
                .rect
                .contains(ctx.input(|i| i.pointer.interact_pos().unwrap_or(Pos2::ZERO)))
        {
            close_menu = true;
        }
        if close_menu {
            self.header_menu_open = false;
            self.controller.set_menu_shown(false, now_ms);
        }
    }

    fn paint_caption_overlay(
        &mut self,
        ui: &mut Ui,
        content_rect: Rect,
        header_rect: Rect,
        story: &Story,
        backend: &dyn StoriesBackend,
        now_ms: i64,
    ) {
        let caption = self.controller.caption_text().text.clone();
        if caption.is_empty() && !story.is_repost() {
            return;
        }

        if self.controller.is_caption_expanded() {
            // Full scrollable expanded caption sheet (`media_stories_caption_full_view.cpp`).
            let top = header_rect.bottom().max(content_rect.top() + 56.0);
            let sheet_rect = Rect::from_min_max(
                Pos2::new(content_rect.left() + 12.0, top + 8.0),
                Pos2::new(content_rect.right() - 12.0, content_rect.bottom() - 12.0),
            );
            let mut sheet_ui = ui.new_child(egui::UiBuilder::new().max_rect(sheet_rect));
            Frame::NONE
                .fill(Color32::from_rgba_unmultiplied(16, 20, 26, 224))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(14))
                .show(&mut sheet_ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Caption")
                                .size(13.0)
                                .strong()
                                .color(Color32::from_rgb(0x88, 0xC8, 0xF8)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Less").clicked() {
                                self.controller.caption_closed(now_ms);
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ScrollArea::vertical()
                        .id_salt("story_full_caption_scroll")
                        .show(ui, |ui| {
                            ui.label(RichText::new(&caption).size(13.5).color(Color32::WHITE));
                        });
                });
            return;
        }

        if self.controller.skip_caption() {
            return;
        }

        // Collapsed 2-line caption at the bottom of the story card with clickable "... more".
        let needs_expand = caption.chars().count() > 96 || caption.contains('\n');
        let preview_text = if needs_expand {
            let trimmed: String = caption.replace('\n', " ").chars().take(92).collect();
            format!("{trimmed}…")
        } else {
            caption
        };

        let box_h = if story.is_repost() { 68.0 } else { 52.0 };
        let cap_rect = Rect::from_min_max(
            Pos2::new(
                content_rect.left() + 14.0,
                content_rect.bottom() - box_h - 10.0,
            ),
            Pos2::new(content_rect.right() - 14.0, content_rect.bottom() - 10.0),
        );
        let mut cap_ui = ui.new_child(egui::UiBuilder::new().max_rect(cap_rect));
        if story.is_repost() {
            let src = story
                .repost
                .source_peer
                .map(|pid| backend.peer_profile(pid).name)
                .unwrap_or_else(|| story.repost.source_name.clone());
            cap_ui.label(
                RichText::new(format!("\u{21BB} Reposted from {src}"))
                    .size(11.5)
                    .strong()
                    .color(Color32::from_rgb(0x90, 0xD8, 0xFF)),
            );
        }
        let resp = cap_ui.add(
            egui::Label::new(RichText::new(preview_text).size(13.0).color(Color32::WHITE))
                .wrap()
                .sense(Sense::click()),
        );
        if needs_expand {
            let more_resp = cap_ui.add(
                egui::Button::new(
                    RichText::new("more")
                        .size(12.0)
                        .strong()
                        .color(Color32::from_rgb(0x70, 0xC8, 0xFA)),
                )
                .frame(false),
            );
            if more_resp.clicked() || resp.clicked() {
                self.controller.show_full_caption(now_ms);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_footer(
        &mut self,
        ui: &mut Ui,
        controls_rect: Rect,
        story: &Story,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        now_secs: TimeId,
        now_ms: i64,
        open_views_requested: &mut bool,
    ) {
        let recent = self.controller.recent_views().clone();
        let reply_state = self.controller.reply_area().clone();

        // Own stories / Channel / Changelog recent views bar (`media_stories_recent_views.cpp`).
        match recent.kind {
            Some(RecentViewsType::SelfViews) => {
                let mut f_ui = ui.new_child(egui::UiBuilder::new().max_rect(controls_rect));
                f_ui.centered_and_justified(|ui| {
                    let pill_w = controls_rect.width().min(280.0);
                    let (pill_rect, resp) =
                        ui.allocate_exact_size(Vec2::new(pill_w, 40.0), Sense::click());
                    ui.painter().rect_filled(
                        pill_rect,
                        CornerRadius::same(20),
                        if resp.hovered() {
                            Color32::from_rgb(0x2B, 0x36, 0x44)
                        } else {
                            Color32::from_rgb(0x1F, 0x27, 0x31)
                        },
                    );
                    // Overlapping up to 3 recent viewer avatars.
                    let mut x = pill_rect.left() + 20.0;
                    for &viewer_id in recent.recent_viewers.iter().take(3) {
                        let vp = backend.peer_profile(viewer_id);
                        let c = Pos2::new(x, pill_rect.center().y);
                        ui.painter()
                            .circle_filled(c, 12.5, Color32::from_rgb(0x1F, 0x27, 0x31));
                        paint_avatar_circle(ui.painter(), c, 11.0, &vp, 8.5);
                        x += 16.0;
                    }
                    let label = if recent.views == 0 {
                        "No views yet".to_owned()
                    } else if recent.views == 1 {
                        "1 view".to_owned()
                    } else {
                        format!(
                            "{} views • {} reactions",
                            format_compact_count(recent.views),
                            format_compact_count(recent.reactions)
                        )
                    };
                    ui.painter().text(
                        Pos2::new(x + 12.0, pill_rect.center().y),
                        Align2::LEFT_CENTER,
                        label,
                        FontId::proportional(13.0),
                        Color32::WHITE,
                    );
                    if resp.on_hover_text("Open views & statistics").clicked() {
                        *open_views_requested = true;
                    }
                });
                return;
            }
            Some(RecentViewsType::Channel) => {
                let mut f_ui = ui.new_child(egui::UiBuilder::new().max_rect(controls_rect));
                f_ui.horizontal_centered(|ui| {
                    let views_label = format!(
                        "\u{1F441} {} views",
                        format_compact_count(recent.views.max(1))
                    );
                    let views_btn = egui::Button::new(
                        RichText::new(views_label).size(13.0).color(Color32::WHITE),
                    )
                    .fill(Color32::from_rgb(0x1F, 0x27, 0x31))
                    .corner_radius(CornerRadius::same(18))
                    .min_size(Vec2::new(112.0, 38.0));
                    if ui.add(views_btn).clicked() && recent.can_view_reactions {
                        *open_views_requested = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let liked = story.sent_reaction.is_some();
                        let heart_text = if recent.reactions > 0 {
                            format!(
                                "{} {}",
                                story
                                    .sent_reaction
                                    .as_ref()
                                    .map(reaction_display_text)
                                    .unwrap_or_else(|| "\u{2764}".to_owned()),
                                format_compact_count(recent.reactions)
                            )
                        } else {
                            "\u{2764}".to_owned()
                        };
                        let like_btn = egui::Button::new(
                            RichText::new(heart_text).size(13.5).color(if liked {
                                Color32::from_rgb(0xFF, 0x5A, 0x6E)
                            } else {
                                Color32::WHITE
                            }),
                        )
                        .fill(Color32::from_rgb(0x1F, 0x27, 0x31))
                        .corner_radius(CornerRadius::same(18))
                        .min_size(Vec2::new(66.0, 38.0));
                        if ui.add(like_btn).clicked() {
                            self.controller.toggle_liked(store);
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("\u{263A}").size(15.0).color(Color32::WHITE),
                                )
                                .fill(Color32::from_rgb(0x1F, 0x27, 0x31))
                                .corner_radius(CornerRadius::same(18))
                                .min_size(Vec2::splat(38.0)),
                            )
                            .on_hover_text("Pick reaction")
                            .clicked()
                        {
                            self.reaction_picker_open = !self.reaction_picker_open;
                            self.controller
                                .set_reply_active(self.reaction_picker_open, now_ms);
                        }
                    });
                });
                return;
            }
            Some(RecentViewsType::Changelog) => {
                let mut f_ui = ui.new_child(egui::UiBuilder::new().max_rect(controls_rect));
                f_ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new("You can't reply to this story")
                            .size(13.0)
                            .color(Color32::from_rgb(0x9A, 0xAA, 0xBA)),
                    );
                });
                return;
            }
            Some(RecentViewsType::Other) | None => {}
        }

        // Standard Reply / Comment composer (`media_stories_reply.cpp`).
        let mut f_ui = ui.new_child(egui::UiBuilder::new().max_rect(controls_rect));
        match reply_state {
            ReplyAreaState::Hidden => {}
            ReplyAreaState::CantReply => {
                f_ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("You can't reply to this story")
                            .size(13.0)
                            .color(Color32::from_rgb(0x9A, 0xAA, 0xBA)),
                    );
                });
            }
            ReplyAreaState::Visible {
                kind,
                restriction,
                placeholder,
                likes_enabled,
                ..
            } => {
                if restriction != ReplyWriteRestriction::None {
                    let msg = match restriction {
                        ReplyWriteRestriction::None => "",
                        ReplyWriteRestriction::LoadingHidden => "Loading live stream chat...",
                        ReplyWriteRestriction::VideoStreamCommentsDisabled => {
                            "Comments are disabled for this live stream"
                        }
                        ReplyWriteRestriction::AccountFrozen => "Your account is currently limited",
                        ReplyWriteRestriction::PremiumRequired => {
                            "Telegram Premium is required to reply to this user"
                        }
                    };
                    f_ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(msg)
                                .size(12.5)
                                .color(Color32::from_rgb(0xA6, 0xB4, 0xC2)),
                        );
                    });
                    return;
                }

                let hint = format_reply_placeholder(&placeholder, kind);
                Frame::NONE
                    .fill(Color32::from_rgb(0x1F, 0x27, 0x31))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(0x32, 0x3E, 0x4C)))
                    .corner_radius(CornerRadius::same(21))
                    .inner_margin(Margin::symmetric(10, 4))
                    .show(&mut f_ui, |ui| {
                        ui.visuals_mut().weak_text_color =
                            Some(Color32::from_rgb(0x9A, 0xAA, 0xBA));
                        ui.horizontal_centered(|ui| {
                            let right_reserve = if likes_enabled { 92.0 } else { 54.0 };
                            let input_w = (ui.available_width() - right_reserve).max(90.0);
                            let edit_resp = ui.add_sized(
                                Vec2::new(input_w, 32.0),
                                TextEdit::singleline(&mut self.reply_draft)
                                    .frame(Frame::NONE)
                                    .hint_text(
                                        RichText::new(hint)
                                            .color(Color32::from_rgb(0x9A, 0xAA, 0xBA)),
                                    )
                                    .text_color(Color32::WHITE),
                            );
                            let focused = edit_resp.has_focus();
                            self.controller
                                .set_reply_active(focused || self.reaction_picker_open, now_ms);

                            let send_now = !self.reply_draft.trim().is_empty()
                                && focused
                                && ui.input(|i| i.key_pressed(Key::Enter));

                            if !self.reply_draft.trim().is_empty() {
                                let send_btn = egui::Button::new(
                                    RichText::new("Send")
                                        .size(12.5)
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .fill(Color32::from_rgb(0x40, 0xA7, 0xE3))
                                .corner_radius(CornerRadius::same(14));
                                if ui.add(send_btn).clicked() || send_now {
                                    let text = std::mem::take(&mut self.reply_draft);
                                    if backend
                                        .send_story_reply(story.full_id(), &text, now_secs)
                                        .is_ok()
                                    {
                                        self.show_toast("Reply sent", now_ms);
                                    }
                                    self.controller.set_reply_active(false, now_ms);
                                }
                            } else if likes_enabled {
                                // Reaction picker toggle button.
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("\u{263A}")
                                                .size(16.0)
                                                .color(Color32::from_rgb(0xB8, 0xC6, 0xD4)),
                                        )
                                        .frame(false),
                                    )
                                    .on_hover_text("Choose reaction")
                                    .clicked()
                                {
                                    self.reaction_picker_open = !self.reaction_picker_open;
                                    self.controller
                                        .set_reply_active(self.reaction_picker_open, now_ms);
                                }

                                // Quick-like heart toggle (`Controller::toggleLiked`).
                                let liked = story.sent_reaction.is_some();
                                let heart = story
                                    .sent_reaction
                                    .as_ref()
                                    .map(reaction_display_text)
                                    .unwrap_or_else(|| "\u{2764}".to_owned());
                                if ui
                                    .add(
                                        egui::Button::new(RichText::new(heart).size(17.0).color(
                                            if liked {
                                                Color32::from_rgb(0xFF, 0x5A, 0x6E)
                                            } else {
                                                Color32::from_rgb(0xB8, 0xC6, 0xD4)
                                            },
                                        ))
                                        .frame(false),
                                    )
                                    .on_hover_text(if liked {
                                        "Remove reaction"
                                    } else {
                                        "Like story"
                                    })
                                    .clicked()
                                {
                                    self.controller.toggle_liked(store);
                                }
                            }

                            // Share shortcut button on right.
                            if story.can_share(now_secs)
                                && ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("\u{21AA}")
                                                .size(15.0)
                                                .color(Color32::from_rgb(0xB8, 0xC6, 0xD4)),
                                        )
                                        .frame(false),
                                    )
                                    .on_hover_text("Share story")
                                    .clicked()
                            {
                                let _ = self.controller.share_requested(store, now_secs);
                            }
                        });
                    });
            }
        }
    }

    fn paint_reaction_picker(
        &mut self,
        ui: &mut Ui,
        controls_rect: Rect,
        story: &Story,
        store: &mut StoriesStore,
        now_ms: i64,
    ) {
        let bar_w = 304.0_f32.min(controls_rect.width());
        let bar_rect = Rect::from_center_size(
            Pos2::new(controls_rect.center().x, controls_rect.top() - 28.0),
            Vec2::new(bar_w, 42.0),
        );
        let mut r_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect));
        Frame::NONE
            .fill(Color32::from_rgb(0x22, 0x2B, 0x36))
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x3B, 0x4A, 0x5C)))
            .corner_radius(CornerRadius::same(21))
            .inner_margin(Margin::symmetric(10, 4))
            .show(&mut r_ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for &emoji in QUICK_REACTIONS {
                        let id = ReactionId::Emoji(emoji.to_owned());
                        let selected = story.sent_reaction.as_ref() == Some(&id);
                        let btn = egui::Button::new(RichText::new(emoji).size(18.0))
                            .fill(if selected {
                                Color32::from_rgb(0x3B, 0x82, 0xC4)
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(CornerRadius::same(16))
                            .min_size(Vec2::splat(30.0));
                        if ui.add(btn).clicked() {
                            let next = if selected { None } else { Some(id) };
                            self.controller.send_reaction(store, next);
                            self.reaction_picker_open = false;
                            self.controller.set_reply_active(false, now_ms);
                            self.show_toast(
                                if selected {
                                    "Reaction removed".to_owned()
                                } else {
                                    format!("Reacted {emoji}")
                                },
                                now_ms,
                            );
                        }
                    }
                });
            });
    }

    fn handle_modal_outcome(
        &mut self,
        outcome: StoryModalOutcome,
        modal: &StoryModalBox,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        now_secs: TimeId,
        now_ms: i64,
    ) -> bool {
        match outcome {
            StoryModalOutcome::None => true,
            StoryModalOutcome::Close => false,
            StoryModalOutcome::LoadMoreViews => {
                self.views_loaded_limit += VIEWS_MENU_ADD_PER_PAGE;
                let is_channel = matches!(
                    modal,
                    StoryModalBox::ViewsStats {
                        is_channel: true,
                        ..
                    }
                );
                let _ =
                    self.controller
                        .load_views(store, self.views_loaded_limit, false, is_channel);
                self.flush_store_actions(store, backend, now_secs, now_ms);
                self.controller.on_views_loaded(store);
                true
            }
            StoryModalOutcome::ToggleBlockViewer { peer_id, blocked } => {
                backend.set_viewer_stories_blocked(peer_id, blocked);
                let name = backend.peer_profile(peer_id).short_name;
                self.show_toast(
                    if blocked {
                        format!("Hid your stories from {name}")
                    } else {
                        format!("{name} can see your stories again")
                    },
                    now_ms,
                );
                true
            }
            StoryModalOutcome::ActivateStealthMode { .. } => {
                let _ =
                    self.controller
                        .setup_stealth_mode(store, now_secs, backend.account_premium());
                self.flush_store_actions(store, backend, now_secs, now_ms);
                self.show_toast("Stealth Mode enabled for 25 minutes", now_ms);
                false
            }
            StoryModalOutcome::ToggleMockPremium => {
                if !backend.supports_mock_premium_toggle() {
                    self.show_toast(
                        "Telegram Premium is required to enable Stealth Mode",
                        now_ms,
                    );
                    false
                } else {
                    let next = !backend.account_premium();
                    backend.set_account_premium(next);
                    self.show_toast(
                        if next {
                            "Mock Telegram Premium enabled — you can activate Stealth Mode"
                        } else {
                            "Mock Telegram Premium disabled"
                        },
                        now_ms,
                    );
                    true
                }
            }
            StoryModalOutcome::OpenPremium => {
                self.premium_requested = true;
                false
            }
            StoryModalOutcome::SubmitShare {
                story_id,
                targets,
                comment,
            } => {
                if backend.share_story(story_id, &targets, &comment).is_ok() {
                    self.show_toast(
                        format!(
                            "Story shared with {} {}",
                            targets.len(),
                            if targets.len() == 1 { "chat" } else { "chats" }
                        ),
                        now_ms,
                    );
                }
                false
            }
            StoryModalOutcome::CopyStoryLink(story_id) => {
                if let Some(link) = backend.export_story_link(story_id) {
                    self.show_toast(format!("Link copied: {link}"), now_ms);
                }
                false
            }
            StoryModalOutcome::SubmitReport { story_id, reason } => {
                let _ = backend.report_story(story_id, &reason);
                self.show_toast(format!("Story reported ({reason})"), now_ms);
                false
            }
            StoryModalOutcome::ConfirmDelete(_story_id) => {
                if let Some(shown) = self.controller.shown_id() {
                    let cfg = self.viewer_config_for(backend, shown.peer);
                    self.controller
                        .delete_confirmed(store, cfg, now_secs, now_ms);
                    self.flush_store_actions(store, backend, now_secs, now_ms);
                    self.show_toast("Story deleted", now_ms);
                }
                false
            }
        }
    }

    fn flush_store_actions(
        &mut self,
        store: &mut StoriesStore,
        backend: &mut dyn StoriesBackend,
        now_secs: TimeId,
        now_ms: i64,
    ) {
        let actions = store.take_actions();
        if !actions.is_empty() {
            backend.execute_actions(store, actions, now_secs);
        }
        let events = store.take_events();
        for ev in events {
            match ev {
                StoriesEvent::ItemsChanged(peer_id) => {
                    let cfg = self.viewer_config_for(backend, peer_id);
                    self.controller
                        .on_items_changed(store, peer_id, cfg, now_secs, now_ms);
                }
                StoriesEvent::SourceChanged(peer_id) => {
                    self.controller.on_source_changed(store, peer_id);
                }
                StoriesEvent::AlbumIdsChanged(key) => {
                    let cfg = self.viewer_config_for(backend, key.peer_id);
                    self.controller.on_album_ids_changed(
                        store,
                        key.peer_id,
                        key.album_id,
                        cfg,
                        now_secs,
                        now_ms,
                    );
                }
                StoriesEvent::StoryUpdated { id, flags } => {
                    if flags.contains(tg_stories::StoryUpdateFlags::DESTROYED) {
                        self.controller.on_story_destroyed(id);
                    } else if flags.contains(tg_stories::StoryUpdateFlags::VIEWS_CHANGED) {
                        self.controller.on_views_loaded(store);
                    }
                }
                _ => {}
            }
        }
    }

    fn viewer_config_for(&self, backend: &dyn StoriesBackend, peer_id: PeerId) -> ViewerConfig {
        let profile = backend.peer_profile(peer_id);
        ViewerConfig {
            account_premium: backend.account_premium(),
            premium_possible: true,
            can_view_channel_reactions: profile.can_view_channel_reactions,
            reply_ctx: ReplyContext {
                account_premium: backend.account_premium(),
                account_frozen: false,
                can_send_anything: true,
                peer_requires_premium_to_write: profile.requires_premium_to_write,
                stars_per_message: profile.stars_per_message,
                can_manage_video_stream: profile.is_self,
            },
        }
    }

    fn show_toast(&mut self, message: impl Into<String>, now_ms: i64) {
        self.toast_text = Some((message.into(), now_ms + 3_200));
    }
}

/// Formats `ReplyPlaceholder` into the reply input hint text (`PlaceholderText` in `media_stories_reply.cpp`).
#[must_use]
pub fn format_reply_placeholder(placeholder: &ReplyPlaceholder, kind: ReplyAreaKind) -> String {
    match placeholder {
        ReplyPlaceholder::Reply => "Reply privately...".to_owned(),
        ReplyPlaceholder::Comment => "Comment...".to_owned(),
        ReplyPlaceholder::VideoStreamComment => "Write a live comment...".to_owned(),
        ReplyPlaceholder::PaidStars {
            stars,
            video_stream,
        } => {
            if *video_stream {
                format!("Comment for {stars} Stars...")
            } else {
                format!("Reply for {stars} Stars...")
            }
        }
        ReplyPlaceholder::StealthModeCountdown { time_left } => {
            let prefix = match kind {
                ReplyAreaKind::Comment => "Stealth Mode",
                ReplyAreaKind::VideoStreamComment => "Live",
                ReplyAreaKind::Reply => "Stealth Mode",
            };
            format!("{prefix} active: {time_left}")
        }
    }
}

fn normalized_in(rect: Rect, pos: Pos2) -> Option<(f64, f64)> {
    if !rect.contains(pos) {
        return None;
    }
    let nx = f64::from(pos.x - rect.left()) / f64::from(rect.width().max(1.0));
    let ny = f64::from(pos.y - rect.top()) / f64::from(rect.height().max(1.0));
    Some((nx.clamp(0.0, 1.0), ny.clamp(0.0, 1.0)))
}

fn paint_vertical_scrim(painter: &Painter, rect: Rect, top_alpha: u8, bottom_alpha: u8) {
    let top_c = Color32::from_rgba_unmultiplied(0, 0, 0, top_alpha);
    let bot_c = Color32::from_rgba_unmultiplied(0, 0, 0, bottom_alpha);
    let mut mesh = egui::epaint::Mesh::default();
    let uv = egui::epaint::WHITE_UV;
    mesh.vertices.push(egui::epaint::Vertex {
        pos: rect.left_top(),
        uv,
        color: top_c,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: rect.right_top(),
        uv,
        color: top_c,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: rect.right_bottom(),
        uv,
        color: bot_c,
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: rect.left_bottom(),
        uv,
        color: bot_c,
    });
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

fn layout_unsupported_story_label(
    painter: &Painter,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob {
        halign: egui::Align::Center,
        wrap: egui::text::TextWrapping {
            max_width,
            ..Default::default()
        },
        ..Default::default()
    };
    job.append(
        "This story is not supported\nby your version of Rustgram.",
        0.0,
        egui::text::TextFormat {
            font_id: FontId::new(14.0, tg_ui::fonts::family_regular()),
            color: Color32::WHITE,
            line_height: Some(21.0),
            ..Default::default()
        },
    );
    painter.layout_job(job)
}

fn compute_unsupported_story_rects(content_rect: Rect, label_height: f32) -> (Pos2, Rect) {
    let top_pad = 24.0_f32; // `st::storiesUnsupportedTop`
    let skip = 18.0_f32; // `st::storiesUnsupportedSkip`
    let btn_h = 42.0_f32; // `st::storiesUnsupportedUpdate.height`
    let btn_w = (content_rect.width() - 102.0).clamp(160.0, 260.0);
    let total_h = top_pad + label_height + skip + btn_h;
    let block_top = content_rect.top() + (content_rect.height() - total_h) * 0.5;
    let text_pos = Pos2::new(content_rect.center().x, block_top + top_pad);
    let btn_rect = Rect::from_min_size(
        Pos2::new(
            content_rect.center().x - btn_w * 0.5,
            block_top + total_h - btn_h,
        ),
        Vec2::new(btn_w, btn_h),
    );
    (text_pos, btn_rect)
}

fn paint_header_icon_button(
    ui: &mut Ui,
    rect: Rect,
    label: &str,
    id_salt: &str,
    tooltip: &str,
) -> bool {
    let resp = ui.interact(rect, Id::unique(id_salt), Sense::click());
    if resp.hovered() {
        ui.painter().circle_filled(
            rect.center(),
            rect.width() * 0.48,
            Color32::from_rgba_unmultiplied(255, 255, 255, 34),
        );
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(14.0),
        Color32::from_rgba_unmultiplied(255, 255, 255, if resp.hovered() { 255 } else { 215 }),
    );
    resp.on_hover_text(tooltip).clicked()
}

fn paint_side_arrow_button(ui: &mut Ui, rect: Rect, left: bool, id_salt: &str) -> bool {
    let resp = ui.interact(rect, Id::unique(id_salt), Sense::click());
    let c = rect.center();
    let alpha = if resp.hovered() { 245 } else { 150 };
    if resp.hovered() {
        ui.painter()
            .circle_filled(c, 22.0, Color32::from_rgba_unmultiplied(255, 255, 255, 28));
    }
    let s = Stroke::new(2.4, Color32::from_rgba_unmultiplied(255, 255, 255, alpha));
    if left {
        ui.painter().line_segment(
            [Pos2::new(c.x + 5.0, c.y - 10.0), Pos2::new(c.x - 5.0, c.y)],
            s,
        );
        ui.painter().line_segment(
            [Pos2::new(c.x - 5.0, c.y), Pos2::new(c.x + 5.0, c.y + 10.0)],
            s,
        );
    } else {
        ui.painter().line_segment(
            [Pos2::new(c.x - 5.0, c.y - 10.0), Pos2::new(c.x + 5.0, c.y)],
            s,
        );
        ui.painter().line_segment(
            [Pos2::new(c.x + 5.0, c.y), Pos2::new(c.x - 5.0, c.y + 10.0)],
            s,
        );
    }
    resp.clicked()
}

fn paint_close_overlay_button(ui: &mut Ui, rect: Rect) -> bool {
    let resp = ui.interact(rect, Id::unique("story_viewer_close_btn"), Sense::click());
    let c = rect.center();
    ui.painter().circle_filled(
        c,
        18.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, if resp.hovered() { 42 } else { 20 }),
    );
    let s = Stroke::new(2.0, Color32::WHITE);
    ui.painter().line_segment(
        [
            Pos2::new(c.x - 6.0, c.y - 6.0),
            Pos2::new(c.x + 6.0, c.y + 6.0),
        ],
        s,
    );
    ui.painter().line_segment(
        [
            Pos2::new(c.x + 6.0, c.y - 6.0),
            Pos2::new(c.x - 6.0, c.y + 6.0),
        ],
        s,
    );
    resp.on_hover_text("Close (Esc)").clicked()
}

fn paint_viewer_toast(painter: &Painter, viewport: Rect, message: &str) {
    let galley = painter.layout_no_wrap(
        message.to_owned(),
        FontId::proportional(13.0),
        Color32::WHITE,
    );
    let w = (galley.size().x + 28.0).min(viewport.width() - 32.0);
    let rect = Rect::from_center_size(
        Pos2::new(viewport.center().x, viewport.bottom() - 28.0),
        Vec2::new(w, 34.0),
    );
    painter.rect_filled(
        rect,
        CornerRadius::same(17),
        Color32::from_rgba_unmultiplied(26, 33, 42, 238),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(17),
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
        egui::StrokeKind::Inside,
    );
    painter.galley(
        Pos2::new(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        Color32::WHITE,
    );
}

trait VideoTexExt {
    fn active_video_mut_ref_tex(&self, story_id: FullStoryId) -> Option<&egui::TextureHandle>;
}

impl VideoTexExt for StoryMediaCache {
    fn active_video_mut_ref_tex(&self, story_id: FullStoryId) -> Option<&egui::TextureHandle> {
        // Expose active video texture via cache helper.
        self.active_video_texture(story_id)
    }
}
