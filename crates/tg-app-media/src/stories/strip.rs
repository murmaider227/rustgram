//! Collapsible dialogs-list top stories strip widget (`StoriesStrip`).
//!
//! Port origin: `Telegram/SourceFiles/dialogs/ui/dialogs_stories_list.h`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_stories_list.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_stories_content.cpp`, and
//! `Telegram/SourceFiles/dialogs/dialogs.style`.
//!
//! Designed to be embedded directly at the top of `dialogs/` (or standalone)
//! without modifying `crates/tg-app/src/dialogs/`.

use std::f32::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_STRIP_ID: AtomicU64 = AtomicU64::new(1);

use egui::{
    Align2, Color32, CornerRadius, FontId, Id, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke,
    Ui, Vec2,
};
use tg_data::PeerId;
use tg_stories::{
    DialogBarMode, DialogStoriesBarState, DialogStoriesContent, DialogStoryElement,
    RingSegmentKind, StoriesStore, StorySourcesList, TooltipSummary, build_dialog_stories_content,
    compute_ring_segments,
};

use crate::stories::backend::{PeerProfile, StoriesBackend};
use crate::stories::layout::{StoriesStripMetrics, compute_strip_layout, ring_segment_arc_angles};
use tg_app_core::stand_in::theme::Palette;

/// Expand/collapse animation duration in seconds (`st::slideWrapDuration * 1.4` ≈ 280 ms).
const EXPAND_ANIM_SECS: f64 = 0.28;

/// User actions emitted by [`StoriesStrip::show`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoriesStripAction {
    /// Open the story viewer for `peer_id` in `list`.
    OpenStory {
        /// Peer whose story should open.
        peer_id: PeerId,
        /// Source list context (`NotHidden` or `Hidden`).
        list: StorySourcesList,
    },
    /// Open Stealth Mode box and then view `peer_id`'s stories anonymously.
    OpenStealth {
        /// Peer to open anonymously.
        peer_id: PeerId,
        /// Source list context.
        list: StorySourcesList,
    },
    /// Hide (archive) or unhide `peer_id`'s stories (`stories.togglePeerStoriesHidden`).
    ToggleHidden {
        /// Target peer.
        peer_id: PeerId,
        /// `true` to hide/archive, `false` to restore to main strip.
        hidden: bool,
    },
    /// Switch between main (`NotHidden`) and archived (`Hidden`) stories strips.
    SwitchList(StorySourcesList),
    /// Request loading the next page of sources (`stories.getAllStories`).
    LoadMore(StorySourcesList),
    /// Expanded state toggled (`true` = Full, `false` = Small).
    ExpandedChanged(bool),
    /// Open the story composer (`SendStoryBox`) to create and post a new story.
    AddStory,
    /// Dismiss a failed outgoing story upload indicator for `peer_id`.
    DismissFailedUpload(PeerId),
}

/// Collapsible top stories strip widget (`Dialogs::Stories::List`).
pub struct StoriesStrip {
    instance_id: u64,
    metrics: StoriesStripMetrics,
    bar_state: DialogStoriesBarState,
    active_list: StorySourcesList,
    anim_progress: f64,
    last_frame_time: Option<f64>,
    context_menu_peer: Option<PeerId>,
    context_menu_pos: Pos2,
    upload_spin_phase: f32,
}

impl Default for StoriesStrip {
    fn default() -> Self {
        Self::new()
    }
}

impl StoriesStrip {
    /// Creates a new [`StoriesStrip`] starting in the collapsed (`Small`) state.
    #[must_use]
    pub fn new() -> Self {
        let metrics = StoriesStripMetrics::default();
        let bar_state = DialogStoriesBarState::new(
            metrics.full_height as i32,
            metrics.single_full_width() as i32,
            metrics.full_left as i32,
        );
        Self {
            instance_id: NEXT_STRIP_ID.fetch_add(1, Ordering::Relaxed),
            metrics,
            bar_state,
            active_list: StorySourcesList::NotHidden,
            anim_progress: 0.0,
            last_frame_time: None,
            context_menu_peer: None,
            context_menu_pos: Pos2::ZERO,
            upload_spin_phase: 0.0,
        }
    }

    /// Creates a new [`StoriesStrip`] already expanded (`Full`) for previews/tests.
    #[must_use]
    pub fn expanded() -> Self {
        let mut strip = Self::new();
        strip.set_expanded_immediate(true);
        strip
    }

    /// Returns the currently displayed source list (`NotHidden` or `Hidden`).
    #[must_use]
    pub const fn active_list(&self) -> StorySourcesList {
        self.active_list
    }

    /// Switches between main and archived stories lists.
    pub fn set_active_list(&mut self, list: StorySourcesList) {
        self.active_list = list;
        self.context_menu_peer = None;
    }

    /// Returns a reference to the underlying `tg-stories` [`DialogStoriesBarState`].
    #[must_use]
    pub const fn bar_state(&self) -> &DialogStoriesBarState {
        &self.bar_state
    }

    /// Returns the current [`DialogBarMode`] (`Small`, `Changing`, or `Full`).
    #[must_use]
    pub const fn mode(&self) -> DialogBarMode {
        self.bar_state.mode()
    }

    /// Returns `true` if logically expanded.
    #[must_use]
    pub const fn is_expanded(&self) -> bool {
        self.bar_state.is_expanded()
    }

    /// Returns current animated expansion progress in `[0.0, 1.0]`.
    #[must_use]
    pub const fn anim_progress(&self) -> f64 {
        self.anim_progress
    }

    /// Requests smooth animation to expanded (`true`) or collapsed (`false`).
    pub fn request_expanded(&mut self, expanded: bool) {
        self.bar_state.set_expanded(expanded);
        self.context_menu_peer = None;
    }

    /// Immediately snaps the strip to expanded (`true`) or collapsed (`false`) without animation.
    pub fn set_expanded_immediate(&mut self, expanded: bool) {
        self.bar_state.set_expanded(expanded);
        self.anim_progress = if expanded { 1.0 } else { 0.0 };
        self.context_menu_peer = None;
    }

    /// Applies an external vertical pull height (e.g., from pulling down the dialogs scroll list)
    /// using tdesktop's exact `0.72` / `0.68` expand/collapse hysteresis.
    pub fn apply_pull_height(&mut self, height_px: i32) -> Option<bool> {
        let toggled = self.bar_state.set_expanded_height(height_px);
        let ratio = (f64::from(height_px.clamp(0, self.metrics.full_height as i32))
            / f64::from(self.metrics.full_height))
        .clamp(0.0, 1.0);
        self.anim_progress = ratio;
        toggled
    }

    /// Refreshes strip items from `store` and `backend`.
    pub fn sync_from_store(&mut self, store: &StoriesStore, backend: &dyn StoriesBackend) {
        let uploads = backend.pending_uploads();
        let content = build_dialog_stories_content(
            store,
            self.active_list,
            |peer_id| backend.peer_profile(peer_id).short_name,
            &uploads,
        );
        self.bar_state.set_content(content);
    }

    /// Renders the stories strip widget inside `ui`, returning any triggered [`StoriesStripAction`]s.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        store: &StoriesStore,
        backend: &dyn StoriesBackend,
        palette: &Palette,
    ) -> Vec<StoriesStripAction> {
        self.sync_from_store(store, backend);
        let mut actions = Vec::new();

        let uploads = backend.pending_uploads();
        // `Dialogs::Stories::List` stays hidden while its content has no elements, so the chat
        // list search sits directly under the window title when there are no stories.
        if self.bar_state.content().elements.is_empty() && uploads.is_empty() {
            return actions;
        }
        let hidden_count = store.sources(StorySourcesList::Hidden).len();

        // Advance expand/collapse animation smoothly.
        let now_t = ui.input(|i| i.time);
        let dt = self
            .last_frame_time
            .map(|prev| (now_t - prev).clamp(0.0, 0.1))
            .unwrap_or(0.016);
        self.last_frame_time = Some(now_t);

        let target = if self.bar_state.is_expanded() {
            1.0
        } else {
            0.0
        };
        if (self.anim_progress - target).abs() > 0.001 {
            let step = dt / EXPAND_ANIM_SECS;
            if self.anim_progress < target {
                self.anim_progress = (self.anim_progress + step).min(target);
            } else {
                self.anim_progress = (self.anim_progress - step).max(target);
            }
            ui.ctx().request_repaint();
        } else {
            self.anim_progress = target;
        }

        // Advance upload spinner when any upload is active.
        let has_upload = uploads.iter().any(|u| !u.failed)
            || self
                .bar_state
                .content()
                .elements
                .iter()
                .any(|e| e.uploading && !e.upload_failed);
        if has_upload {
            self.upload_spin_phase = (self.upload_spin_phase + dt as f32 * 2.1) % (2.0 * PI);
            ui.ctx().request_repaint();
        }

        let width = ui.available_width().max(120.0);
        self.bar_state.set_viewport_width(width as i32);
        let layout =
            compute_strip_layout(&self.bar_state, &self.metrics, width, self.anim_progress);

        let (strip_rect, strip_resp) =
            ui.allocate_exact_size(Vec2::new(width, layout.height), Sense::click_and_drag());

        // Background + bottom 1px divider.
        let painter = ui.painter().with_clip_rect(strip_rect);
        painter.rect_filled(strip_rect, 0.0, palette.window_bg);
        painter.line_segment(
            [
                Pos2::new(strip_rect.left(), strip_rect.bottom() - 0.5),
                Pos2::new(strip_rect.right(), strip_rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, palette.divider),
        );

        // Handle wheel scrolling: horizontal scrolls items; vertical down/up expands/collapses.
        if strip_resp.hovered() {
            let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
            if self.anim_progress > 0.5 {
                let dx = if scroll_delta.x.abs() > 0.5 {
                    -scroll_delta.x
                } else {
                    0.0
                };
                if dx.abs() > 0.5 && self.bar_state.scroll_by(dx.round() as i32) {
                    actions.push(StoriesStripAction::LoadMore(self.active_list));
                }
                if scroll_delta.y < -18.0 && self.bar_state.is_expanded() {
                    self.request_expanded(false);
                    actions.push(StoriesStripAction::ExpandedChanged(false));
                }
            } else if scroll_delta.y > 18.0 && !self.bar_state.is_expanded() {
                self.request_expanded(true);
                actions.push(StoriesStripAction::ExpandedChanged(true));
            }
        }

        // Handle horizontal drag scrolling in expanded mode.
        if self.anim_progress > 0.5 && strip_resp.dragged() {
            let dx = -strip_resp.drag_delta().x;
            if dx.abs() > 0.5 && self.bar_state.scroll_by(dx.round() as i32) {
                actions.push(StoriesStripAction::LoadMore(self.active_list));
            }
        }

        let content = self.bar_state.content().clone();

        if layout.expand_ratio < 0.35 {
            self.paint_collapsed_bar(
                ui,
                &painter,
                strip_rect,
                &content,
                &uploads,
                backend,
                palette,
                hidden_count,
                &strip_resp,
                &mut actions,
            );
        } else {
            self.paint_expanded_bar(
                ui,
                &painter,
                strip_rect,
                &layout,
                &content,
                &uploads,
                backend,
                palette,
                hidden_count,
                &mut actions,
            );
        }

        // Bottom thin upload progress bar synced from `backend.pending_uploads()`.
        if let Some(first_upload) = uploads.first() {
            let bar_h = 2.5;
            let bar_frac = if first_upload.failed {
                1.0
            } else {
                (first_upload.progress as f32).clamp(0.05, 1.0)
            };
            let bar_color = if first_upload.failed {
                palette.attention_fg
            } else {
                Color32::from_rgb(0x3B, 0xA8, 0xF5)
            };
            let bar_rect = Rect::from_min_size(
                Pos2::new(strip_rect.left(), strip_rect.bottom() - bar_h),
                Vec2::new(strip_rect.width() * bar_frac, bar_h),
            );
            painter.rect_filled(bar_rect, 0.0, bar_color);
        }

        // Render right-click context menu if open.
        if let Some(menu_peer) = self.context_menu_peer {
            self.show_context_menu(
                ui.ctx(),
                menu_peer,
                backend,
                palette,
                hidden_count,
                &mut actions,
            );
        }

        actions
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_collapsed_bar(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        strip_rect: Rect,
        content: &DialogStoriesContent,
        uploads: &[tg_stories::StoryUploadStatus],
        backend: &dyn StoriesBackend,
        palette: &Palette,
        hidden_count: usize,
        strip_resp: &egui::Response,
        actions: &mut Vec<StoriesStripAction>,
    ) {
        if strip_resp.hovered() {
            painter.rect_filled(strip_rect, 0.0, palette.window_bg_over);
        }

        let visible_indices = self.bar_state.small_visible_indices();
        let photo_d = self.metrics.small_photo;
        let radius = photo_d / 2.0;
        let cy = strip_rect.center().y;
        let base_x = strip_rect.left() + self.metrics.small_left + 8.0 + radius;

        // Paint overlapping mini userpics right-to-left so the leftmost story sits on top.
        for (slot_idx, &elem_idx) in visible_indices.iter().enumerate().rev() {
            let elem = &content.elements[elem_idx];
            let peer_id = PeerId(elem.id as i64);
            let profile = backend.peer_profile(peer_id);
            let cx = base_x + (slot_idx as f32) * self.metrics.small_shift;
            let center = Pos2::new(cx, cy);

            // Cutout halo matching `dialogsBg`.
            painter.circle_filled(center, radius + 3.0, palette.window_bg);
            // Unread/live ring in small mode.
            let ring_color = if elem.has_video_stream {
                Color32::from_rgb(0xE5, 0x39, 0x35)
            } else if elem.is_unread_in_small() {
                Color32::from_rgb(0x3B, 0xA8, 0xF5)
            } else {
                palette.unread_badge_muted_bg
            };
            painter.circle_stroke(
                center,
                radius + 1.4,
                Stroke::new(self.metrics.small_line, ring_color),
            );
            paint_avatar_circle(painter, center, radius - 0.5, &profile, 9.5);
        }

        let thumbs_right = if visible_indices.is_empty() {
            strip_rect.left() + 12.0
        } else {
            base_x
                + ((visible_indices.len().saturating_sub(1)) as f32) * self.metrics.small_shift
                + radius
                + 11.0
        };

        let title = if let Some(u) = uploads.first() {
            if u.failed {
                "Story upload failed".to_owned()
            } else {
                let pct = (u.progress.clamp(0.0, 1.0) * 100.0).round() as u8;
                format!("Uploading story {pct}%")
            }
        } else {
            match self.active_list {
                StorySourcesList::NotHidden => {
                    if content.total == 0 {
                        "Stories".to_owned()
                    } else if content.total == 1 {
                        "1 story".to_owned()
                    } else {
                        format!("{} stories", content.total)
                    }
                }
                StorySourcesList::Hidden => format!("Archived stories ({})", content.total),
            }
        };
        let title_color = if uploads.first().is_some_and(|u| u.failed) {
            palette.attention_fg
        } else {
            palette.window_bold_fg
        };
        painter.text(
            Pos2::new(thumbs_right, cy),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(13.0),
            title_color,
        );

        // "+ Add Story" button on the right, left of the expand chevron (leaving strip_rect.center()
        // unobstructed so clicking the center of the collapsed bar still expands the strip).
        let add_btn_w = 84.0_f32.min((strip_rect.width() * 0.34).max(66.0));
        let add_btn_rect = Rect::from_min_size(
            Pos2::new(strip_rect.right() - 26.0 - add_btn_w, cy - 11.0),
            Vec2::new(add_btn_w, 22.0),
        );
        let mut add_clicked = false;
        let mut add_ui = ui.new_child(egui::UiBuilder::new().max_rect(add_btn_rect));
        let add_btn = egui::Button::new(
            RichText::new("+ Add Story")
                .size(11.0)
                .strong()
                .color(palette.window_fg_active),
        )
        .fill(palette.window_bg_active)
        .corner_radius(CornerRadius::same(11))
        .min_size(add_btn_rect.size());
        if add_ui
            .add(add_btn)
            .on_hover_text("Post a new story")
            .clicked()
        {
            add_clicked = true;
            actions.push(StoriesStripAction::AddStory);
        }

        // Right-side expand chevron indicator.
        let chev_x = strip_rect.right() - 14.0;
        let chev_stroke = Stroke::new(1.6, palette.window_sub_text_fg);
        painter.line_segment(
            [
                Pos2::new(chev_x - 4.0, cy - 2.0),
                Pos2::new(chev_x, cy + 2.5),
            ],
            chev_stroke,
        );
        painter.line_segment(
            [
                Pos2::new(chev_x, cy + 2.5),
                Pos2::new(chev_x + 4.0, cy - 2.0),
            ],
            chev_stroke,
        );

        // Tooltip listing up to `MAX_TOOLTIP_NAMES` (3) peer names (`List::computeTooltipText`).
        let tooltip = format_tooltip_summary(&self.bar_state.tooltip_summary());
        let resp = strip_resp.clone().on_hover_text(tooltip);
        let clicked_in_add = resp
            .interact_pointer_pos()
            .is_some_and(|p| add_btn_rect.contains(p));
        if resp.clicked() && !add_clicked && !clicked_in_add {
            self.request_expanded(true);
            actions.push(StoriesStripAction::ExpandedChanged(true));
        } else if resp.secondary_clicked() && hidden_count > 0 {
            let next = self.active_list.other();
            self.set_active_list(next);
            actions.push(StoriesStripAction::SwitchList(next));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_expanded_bar(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        strip_rect: Rect,
        layout: &crate::stories::layout::StripRenderLayout,
        content: &DialogStoriesContent,
        uploads: &[tg_stories::StoryUploadStatus],
        backend: &dyn StoriesBackend,
        palette: &Palette,
        hidden_count: usize,
        actions: &mut Vec<StoriesStripAction>,
    ) {
        let photo_d = layout.photo_size;
        let photo_r = photo_d / 2.0;
        let photo_top = strip_rect.top() + self.metrics.full_photo_top;
        let name_y = strip_rect.top() + self.metrics.full_name_top;

        // Paint each visible story source cell first so top-right controls paint & hit-test above them.
        for (i, elem) in content.elements.iter().enumerate() {
            let cell_left = strip_rect.left() + layout.left_full + (i as f32) * layout.single_full;
            let cell_rect = Rect::from_min_size(
                Pos2::new(cell_left, strip_rect.top()),
                Vec2::new(layout.single_full, strip_rect.height()),
            );
            if cell_rect.right() < strip_rect.left() || cell_rect.left() > strip_rect.right() {
                continue;
            }

            let peer_id = PeerId(elem.id as i64);
            let profile = backend.peer_profile(peer_id);
            let center = Pos2::new(cell_rect.center().x, photo_top + photo_r);

            let item_resp = ui.interact(
                cell_rect.intersect(strip_rect),
                ui.make_persistent_id((
                    "stories_strip_item",
                    self.instance_id,
                    self.active_list.index(),
                    elem.id,
                )),
                Sense::click(),
            );
            if item_resp.hovered() {
                painter.circle_filled(center, photo_r + 6.0, palette.window_bg_over);
            }

            // Paint segmented unread/read/live ring around avatar.
            paint_story_ring(
                painter,
                center,
                photo_r + 3.2,
                elem,
                &self.metrics,
                palette,
                self.upload_spin_phase,
            );

            // Paint circular avatar (slightly dimmed if all read, matching `readOpacity: 0.6`).
            paint_avatar_circle(painter, center, photo_r - 0.5, &profile, 14.5);

            // Paint LIVE pill on live video stream sources, or `+` badge on self avatar.
            if elem.has_video_stream {
                let pill_rect = Rect::from_center_size(
                    Pos2::new(center.x, center.y + photo_r - 1.0),
                    Vec2::new(28.0, 12.0),
                );
                painter.rect_filled(
                    pill_rect,
                    CornerRadius::same(6),
                    Color32::from_rgb(0xE5, 0x39, 0x35),
                );
                painter.rect_stroke(
                    pill_rect,
                    CornerRadius::same(6),
                    Stroke::new(1.5, palette.window_bg),
                    egui::StrokeKind::Outside,
                );
                painter.text(
                    pill_rect.center(),
                    Align2::CENTER_CENTER,
                    "LIVE",
                    FontId::proportional(8.0),
                    Color32::WHITE,
                );
            } else if elem.mine {
                let badge_c = Pos2::new(center.x + photo_r * 0.68, center.y + photo_r * 0.68);
                painter.circle_filled(badge_c, 8.0, palette.window_bg);
                painter.circle_filled(badge_c, 6.5, palette.window_bg_active);
                let plus_stroke = Stroke::new(1.6, Color32::WHITE);
                painter.line_segment(
                    [
                        Pos2::new(badge_c.x - 3.0, badge_c.y),
                        Pos2::new(badge_c.x + 3.0, badge_c.y),
                    ],
                    plus_stroke,
                );
                painter.line_segment(
                    [
                        Pos2::new(badge_c.x, badge_c.y - 3.0),
                        Pos2::new(badge_c.x, badge_c.y + 3.0),
                    ],
                    plus_stroke,
                );
            }

            // Peer short name beneath userpic (`nameTop = 56px`, `font = 11px`).
            if layout.name_opacity > 0.05 {
                let name_color = if elem.unread_count > 0 || elem.has_video_stream || elem.mine {
                    palette.window_bold_fg
                } else {
                    palette.window_sub_text_fg
                };
                let alpha = (layout.name_opacity * 255.0).round() as u8;
                let faded = Color32::from_rgba_unmultiplied(
                    name_color.r(),
                    name_color.g(),
                    name_color.b(),
                    alpha,
                );
                let label = truncate_short_name(&elem.name, 9);
                painter.text(
                    Pos2::new(center.x, name_y + 6.0),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(11.0),
                    faded,
                );
            }

            let hover_hint = if elem.has_video_stream {
                format!("{} — Live video stream", profile.name)
            } else if elem.unread_count > 0 {
                format!(
                    "{} — {} unread of {} stories (Right-click for menu)",
                    profile.name, elem.unread_count, elem.count
                )
            } else {
                format!(
                    "{} — {} stories (Right-click for menu)",
                    profile.name, elem.count
                )
            };
            let item_resp = item_resp.on_hover_text(hover_hint);
            if item_resp.clicked() {
                self.context_menu_peer = None;
                actions.push(StoriesStripAction::OpenStory {
                    peer_id,
                    list: self.active_list,
                });
            } else if item_resp.secondary_clicked() {
                self.context_menu_peer = Some(peer_id);
                self.context_menu_pos = item_resp
                    .interact_pointer_pos()
                    .unwrap_or_else(|| cell_rect.center_bottom());
            }
        }

        // Render an archive chip at the end of the main strip if hidden sources exist.
        if self.active_list == StorySourcesList::NotHidden && hidden_count > 0 {
            let idx = content.elements.len();
            let cell_left =
                strip_rect.left() + layout.left_full + (idx as f32) * layout.single_full;
            let cell_rect = Rect::from_min_size(
                Pos2::new(cell_left, strip_rect.top()),
                Vec2::new(layout.single_full, strip_rect.height()),
            );
            if cell_rect.left() < strip_rect.right() {
                let center = Pos2::new(cell_rect.center().x, photo_top + photo_r);
                let resp = ui.interact(
                    cell_rect.intersect(strip_rect),
                    ui.make_persistent_id(("stories_strip_archived_chip", self.instance_id)),
                    Sense::click(),
                );
                painter.circle_filled(
                    center,
                    photo_r,
                    if resp.hovered() {
                        palette.window_bg_over
                    } else {
                        palette.input_bg
                    },
                );
                painter.circle_stroke(
                    center,
                    photo_r + 2.5,
                    Stroke::new(1.2, palette.unread_badge_muted_bg),
                );
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    format!("{hidden_count}"),
                    FontId::proportional(13.0),
                    palette.window_sub_text_fg,
                );
                painter.text(
                    Pos2::new(center.x, name_y + 6.0),
                    Align2::CENTER_CENTER,
                    "Archived",
                    FontId::proportional(11.0),
                    palette.window_sub_text_fg,
                );
                if resp.on_hover_text("Show archived stories").clicked() {
                    self.set_active_list(StorySourcesList::Hidden);
                    actions.push(StoriesStripAction::SwitchList(StorySourcesList::Hidden));
                }
            }
        }

        // Collapse chevron button in the top-right corner of the strip.
        let collapse_rect = Rect::from_min_size(
            Pos2::new(strip_rect.right() - 24.0, strip_rect.top() + 4.0),
            Vec2::splat(20.0),
        );
        let collapse_resp = ui.interact(
            collapse_rect,
            ui.make_persistent_id(("stories_strip_collapse_btn", self.instance_id)),
            Sense::click(),
        );
        if collapse_resp.hovered() {
            painter.circle_filled(collapse_rect.center(), 9.0, palette.window_bg_over);
        }
        let c = collapse_rect.center();
        let chev_stroke = Stroke::new(1.5, palette.window_sub_text_fg);
        painter.line_segment(
            [Pos2::new(c.x - 3.5, c.y + 1.5), Pos2::new(c.x, c.y - 2.0)],
            chev_stroke,
        );
        painter.line_segment(
            [Pos2::new(c.x, c.y - 2.0), Pos2::new(c.x + 3.5, c.y + 1.5)],
            chev_stroke,
        );
        if collapse_resp
            .on_hover_text("Collapse stories strip")
            .clicked()
        {
            self.request_expanded(false);
            actions.push(StoriesStripAction::ExpandedChanged(false));
        }

        // "+ Add Story" button in the top-right controls area of the expanded strip.
        let add_right_anchor = if self.active_list == StorySourcesList::Hidden {
            strip_rect.right() - 94.0
        } else {
            strip_rect.right() - 28.0
        };
        let add_btn_rect = Rect::from_min_size(
            Pos2::new(add_right_anchor - 84.0, strip_rect.top() + 4.0),
            Vec2::new(82.0, 20.0),
        );
        if add_btn_rect.left() > strip_rect.left() + 4.0 {
            let mut add_ui = ui.new_child(egui::UiBuilder::new().max_rect(add_btn_rect));
            let add_btn = egui::Button::new(
                RichText::new("+ Add Story")
                    .size(10.5)
                    .strong()
                    .color(palette.window_fg_active),
            )
            .fill(palette.window_bg_active)
            .corner_radius(CornerRadius::same(10))
            .min_size(add_btn_rect.size());
            if add_ui
                .add(add_btn)
                .on_hover_text("Post a new story")
                .clicked()
            {
                actions.push(StoriesStripAction::AddStory);
            }
        }

        // Upload status pill (with Dismiss button if failed) in the bottom-right corner.
        if let Some(upload) = uploads.first() {
            let status_text = if upload.failed {
                "Upload failed ×".to_owned()
            } else {
                let pct = (upload.progress.clamp(0.0, 1.0) * 100.0).round() as u8;
                format!("Uploading {pct}%")
            };
            let pill_rect = Rect::from_min_size(
                Pos2::new(strip_rect.right() - 108.0, strip_rect.bottom() - 23.0),
                Vec2::new(102.0, 18.0),
            );
            let pill_resp = ui.interact(
                pill_rect,
                ui.make_persistent_id(("stories_strip_upload_status", self.instance_id)),
                if upload.failed {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            painter.rect_filled(
                pill_rect,
                CornerRadius::same(9),
                if upload.failed {
                    palette.attention_fg
                } else {
                    Color32::from_rgb(0x3B, 0xA8, 0xF5)
                },
            );
            painter.text(
                pill_rect.center(),
                Align2::CENTER_CENTER,
                status_text,
                FontId::proportional(10.0),
                Color32::WHITE,
            );
            if upload.failed && pill_resp.on_hover_text("Dismiss failed upload").clicked() {
                actions.push(StoriesStripAction::DismissFailedUpload(upload.peer_id));
            }
        }

        // Archived badge/switch pill when viewing hidden list.
        if self.active_list == StorySourcesList::Hidden {
            let back_rect = Rect::from_min_size(
                Pos2::new(strip_rect.right() - 88.0, strip_rect.top() + 4.0),
                Vec2::new(60.0, 18.0),
            );
            let back_resp = ui.interact(
                back_rect,
                ui.make_persistent_id(("stories_strip_back_main", self.instance_id)),
                Sense::click(),
            );
            painter.rect_filled(
                back_rect,
                CornerRadius::same(9),
                if back_resp.hovered() {
                    palette.window_bg_active
                } else {
                    palette.input_bg
                },
            );
            painter.text(
                back_rect.center(),
                Align2::CENTER_CENTER,
                "Main",
                FontId::proportional(10.5),
                if back_resp.hovered() {
                    palette.window_fg_active
                } else {
                    palette.window_sub_text_fg
                },
            );
            if back_resp.clicked() {
                self.set_active_list(StorySourcesList::NotHidden);
                actions.push(StoriesStripAction::SwitchList(StorySourcesList::NotHidden));
            }
        }
    }

    fn show_context_menu(
        &mut self,
        ctx: &egui::Context,
        peer_id: PeerId,
        backend: &dyn StoriesBackend,
        palette: &Palette,
        hidden_count: usize,
        actions: &mut Vec<StoriesStripAction>,
    ) {
        let profile = backend.peer_profile(peer_id);
        let mut close_menu = false;

        let area_resp = egui::Area::new(Id::unique("stories_strip_context_menu"))
            .order(egui::Order::Tooltip)
            .fixed_pos(self.context_menu_pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(6, 6))
                    .show(ui, |ui| {
                        ui.set_min_width(190.0);
                        ui.label(
                            RichText::new(&profile.name)
                                .size(12.0)
                                .strong()
                                .color(palette.window_sub_text_fg),
                        );
                        ui.separator();

                        if ui.button("Open story").clicked() {
                            actions.push(StoriesStripAction::OpenStory {
                                peer_id,
                                list: self.active_list,
                            });
                            close_menu = true;
                        }
                        if profile.is_self && ui.button("Add new story...").clicked() {
                            actions.push(StoriesStripAction::AddStory);
                            close_menu = true;
                        }
                        if !profile.is_self && ui.button("View anonymously (Stealth)").clicked() {
                            actions.push(StoriesStripAction::OpenStealth {
                                peer_id,
                                list: self.active_list,
                            });
                            close_menu = true;
                        }
                        if !profile.is_self {
                            let (label, hide_target) = match self.active_list {
                                StorySourcesList::NotHidden => ("Hide stories (Archive)", true),
                                StorySourcesList::Hidden => ("Show in main stories", false),
                            };
                            if ui.button(label).clicked() {
                                actions.push(StoriesStripAction::ToggleHidden {
                                    peer_id,
                                    hidden: hide_target,
                                });
                                close_menu = true;
                            }
                        }
                        if hidden_count > 0 || self.active_list == StorySourcesList::Hidden {
                            ui.separator();
                            let next = self.active_list.other();
                            let switch_label = match next {
                                StorySourcesList::Hidden => {
                                    format!("View archived stories ({hidden_count})")
                                }
                                StorySourcesList::NotHidden => "Back to main stories".to_owned(),
                            };
                            if ui.button(switch_label).clicked() {
                                self.set_active_list(next);
                                actions.push(StoriesStripAction::SwitchList(next));
                                close_menu = true;
                            }
                        }
                    });
            });

        let outside_click = ctx.input(|i| i.pointer.any_pressed())
            && !area_resp
                .response
                .rect
                .contains(ctx.input(|i| i.pointer.interact_pos().unwrap_or(Pos2::ZERO)));
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) || outside_click {
            close_menu = true;
        }
        if close_menu {
            self.context_menu_peer = None;
        }
    }
}

/// Paints a circular gradient-tinted userpic with centered initials.
pub fn paint_avatar_circle(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    profile: &PeerProfile,
    font_size: f32,
) {
    let [r0, g0, b0] = profile.avatar_top_rgb;
    let [r1, g1, b1] = profile.avatar_bottom_rgb;
    let mid = Color32::from_rgb(
        ((u16::from(r0) + u16::from(r1)) / 2) as u8,
        ((u16::from(g0) + u16::from(g1)) / 2) as u8,
        ((u16::from(b0) + u16::from(b1)) / 2) as u8,
    );
    painter.circle_filled(center, radius, mid);
    // Subtle top highlight arc for tdesktop avatar depth.
    painter.circle_filled(
        Pos2::new(center.x, center.y - radius * 0.22),
        radius * 0.65,
        Color32::from_rgba_unmultiplied(r0, g0, b0, 110),
    );
    painter.text(
        center,
        Align2::CENTER_CENTER,
        &profile.initials,
        FontId::proportional(font_size),
        Color32::WHITE,
    );
}

/// Paints segmented story rings matching `List::validateSegments` (`dialogs_stories_list.cpp`).
pub fn paint_story_ring(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    element: &DialogStoryElement,
    metrics: &StoriesStripMetrics,
    palette: &Palette,
    upload_spin_phase: f32,
) {
    // Active upload ring overrides regular segments.
    if element.uploading {
        let color = if element.upload_failed {
            palette.attention_fg
        } else {
            Color32::from_rgb(0x3B, 0xA8, 0xF5)
        };
        let sweep = ((f32::from(element.upload_progress) / 100.0) * 2.0 * PI).max(0.35);
        let start = -PI / 2.0 + upload_spin_phase;
        paint_arc(
            painter,
            center,
            radius,
            start,
            start + sweep,
            Stroke::new(metrics.full_line_unread, color),
        );
        return;
    }

    let segments = compute_ring_segments(element);
    if segments.is_empty() {
        return;
    }
    let total = segments.len() as u32;
    for seg in &segments {
        let (start, end) = ring_segment_arc_angles(total, seg.index);
        let (color, width) = match seg.kind {
            RingSegmentKind::VideoStream => (
                Color32::from_rgb(0xE5, 0x39, 0x35),
                metrics.full_line_unread + 0.5,
            ),
            RingSegmentKind::Unread => {
                // Smooth teal -> sky-blue gradient sweep along segment angle.
                let frac = (seg.index as f32) / (total.max(1) as f32);
                let r = (42.0 + 54.0 * frac) as u8;
                let g = (212.0 - 52.0 * frac) as u8;
                let b = (176.0 + 70.0 * frac) as u8;
                (Color32::from_rgb(r, g, b), metrics.full_line_unread)
            }
            RingSegmentKind::Read => (palette.unread_badge_muted_bg, metrics.full_line_read),
        };
        paint_arc(
            painter,
            center,
            radius,
            start,
            end,
            Stroke::new(width, color),
        );
    }
}

/// Draws a smooth circular arc polyline from `start_rad` to `end_rad`.
pub fn paint_arc(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    start_rad: f32,
    end_rad: f32,
    stroke: Stroke,
) {
    let sweep = (end_rad - start_rad).abs();
    let steps = ((sweep * radius / 3.0).ceil() as usize).clamp(4, 64);
    let mut points = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let a = start_rad + (end_rad - start_rad) * t;
        points.push(Pos2::new(
            center.x + radius * a.cos(),
            center.y + radius * a.sin(),
        ));
    }
    painter.add(Shape::line(points, stroke));
}

/// Formats `TooltipSummary` into the exact collapsed-strip tooltip text.
#[must_use]
pub fn format_tooltip_summary(summary: &TooltipSummary) -> String {
    match summary {
        TooltipSummary::Empty => "No active stories".to_owned(),
        TooltipSummary::MineOnly => "Click to view your stories".to_owned(),
        TooltipSummary::Peers(names) => {
            format!("Click to expand stories from {}", names.join(", "))
        }
    }
}

fn truncate_short_name(name: &str, max_chars: usize) -> String {
    let count = name.chars().count();
    if count <= max_chars {
        name.to_owned()
    } else {
        let prefix: String = name.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}
