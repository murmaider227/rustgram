//! Dialogs-list stories strip widget (`StoriesStrip`).
//!
//! Port origin: `Telegram/SourceFiles/dialogs/ui/dialogs_stories_list.h`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_stories_list.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_stories_content.cpp`,
//! `Telegram/SourceFiles/dialogs/dialogs_widget.cpp` (`setupStories`, `updateControlsGeometry`,
//! `storiesToggleExplicitExpand`) and `Telegram/SourceFiles/dialogs/dialogs.style`.
//!
//! Collapsed, the strip has no row of its own: up to three overlapping 21 px userpics sit at the
//! right end of the chat-list search field (`setLayoutConstraints(..., al_right, ...)`). Expanded,
//! it is a 77 px band right under the search bar that pushes the chat list down.
//!
//! Internally this differs from tdesktop: tdesktop morphs one `List` widget between the small and
//! the full geometry and drives it from the elastic-scroll overscroll; here the collapsed userpics
//! are painted over the search field and the band is allocated in the column with an eased height.

use std::f32::consts::PI;

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_STRIP_ID: AtomicU64 = AtomicU64::new(1);

use egui::epaint::PathStroke;
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
use crate::stories::layout::{
    StoriesStripMetrics, collapsed_photo_center, collapsed_strip_geometry, compute_strip_layout,
    ring_segment_arc_angles,
};
use tg_app_core::stand_in::theme::Palette;

/// Expand/collapse duration in seconds (`kStoriesExpandDuration`, 200 ms).
const EXPAND_ANIM_SECS: f64 = 0.2;
/// `st::dialogsRowHeight`: scrolling the chat list this far past the expand point collapses the
/// expanded strip (`_storiesExplicitExpandScrollTop + st::dialogsRowHeight`).
const COLLAPSE_SCROLL_SKIP: f32 = 62.0;
/// `dialogsStoriesList.readOpacity`: read stories' userpics in the collapsed strip.
const READ_OPACITY: f32 = 0.6;
/// `st::groupCallLive1`: start (top-right) of `Ui::UnreadStoryOutlineGradient`.
const UNREAD_GRADIENT_FROM: Color32 = Color32::from_rgb(0x0D, 0xCC, 0x39);
/// `st::groupCallMuted1`: end (bottom-left) of `Ui::UnreadStoryOutlineGradient`.
const UNREAD_GRADIENT_TILL: Color32 = Color32::from_rgb(0x09, 0x92, 0xEF);

/// User actions emitted by [`StoriesStrip::show_in_search_row`].
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
    /// Open the story composer (`SendStoryBox`): the "+" badge on the own userpic.
    AddStory,
    /// Dismiss a failed outgoing story upload indicator for `peer_id`.
    DismissFailedUpload(PeerId),
}

/// Where the strip lives in the chat-list column (`Dialogs::Widget::updateControlsGeometry`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoriesSearchSlot {
    /// The rounded search field; the collapsed userpics right-align to its right edge.
    pub field_rect: Rect,
    /// The search field background, used for the cut-outs between overlapping userpics.
    pub field_bg: Color32,
    /// The chat-list column: a wheel scroll up there at the top of the list expands the strip.
    pub column_rect: Rect,
    /// The chat list's vertical scroll offset.
    pub list_scroll_y: f32,
    /// The chat list's largest scroll offset (`0` when all chats fit).
    pub list_scroll_max: f32,
}

impl StoriesSearchSlot {
    /// A slot for a lone search field (previews and tests): the list is at its top and only the
    /// field itself counts as the column.
    #[must_use]
    pub const fn for_field(field_rect: Rect, palette: &Palette) -> Self {
        Self {
            field_rect,
            field_bg: palette.window_bg_over,
            column_rect: field_rect,
            list_scroll_y: 0.0,
            list_scroll_max: 0.0,
        }
    }
}

/// What [`StoriesStrip::show_in_search_row`] drew this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoriesStripFrame {
    /// Triggered actions.
    pub actions: Vec<StoriesStripAction>,
    /// The collapsed userpics inside the search field, when visible.
    pub collapsed_rect: Option<Rect>,
    /// The expanded band below the search bar, when it has any height.
    pub band_rect: Option<Rect>,
}

/// The dialogs stories strip (`Dialogs::Stories::List`).
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
    expand_scroll_top: f32,
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
            expand_scroll_top: 0.0,
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

    /// The collapsed geometry inside `field_rect` for the current content
    /// (`List::countSmallGeometry`), or `None` while there is nothing to show.
    #[must_use]
    pub fn collapsed_rect(&self, field_rect: Rect) -> Option<Rect> {
        let count = self.bar_state.small_visible_indices().len();
        (count > 0).then(|| collapsed_strip_geometry(&self.metrics, field_rect, count))
    }

    /// Height of the expanded band for the current animation progress
    /// (`_aboveScrollAdded`, eased with `anim::sineInOut`).
    #[must_use]
    pub fn band_height(&self) -> f32 {
        let t = self.anim_progress.clamp(0.0, 1.0);
        let eased = 0.5 - (PI as f64 * t).cos() / 2.0;
        (f64::from(self.metrics.full_height) * eased).round() as f32
    }

    fn step_animation(&mut self, ui: &Ui) {
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

        let uploading = self
            .bar_state
            .content()
            .elements
            .iter()
            .any(|e| e.uploading && !e.upload_failed);
        if uploading {
            self.upload_spin_phase = (self.upload_spin_phase + dt as f32 * 2.1) % (2.0 * PI);
            ui.ctx().request_repaint();
        }
    }

    fn expand(&mut self, list_scroll_y: f32, actions: &mut Vec<StoriesStripAction>) {
        if !self.bar_state.is_expanded() {
            self.expand_scroll_top = list_scroll_y;
            self.request_expanded(true);
            actions.push(StoriesStripAction::ExpandedChanged(true));
        }
    }

    fn collapse(&mut self, actions: &mut Vec<StoriesStripAction>) {
        if self.bar_state.is_expanded() {
            self.request_expanded(false);
            actions.push(StoriesStripAction::ExpandedChanged(false));
        }
    }

    /// Renders the strip for the chat-list search row: the collapsed userpics over the right end
    /// of `slot.field_rect`, and, while expanded, the full strip in a band allocated at the
    /// cursor of `ui` (right under the search bar).
    pub fn show_in_search_row(
        &mut self,
        ui: &mut Ui,
        slot: StoriesSearchSlot,
        store: &StoriesStore,
        backend: &dyn StoriesBackend,
        palette: &Palette,
    ) -> StoriesStripFrame {
        self.sync_from_store(store, backend);
        let mut frame = StoriesStripFrame::default();
        // `Dialogs::Stories::List` stays hidden while its content has no elements.
        if self.bar_state.content().elements.is_empty() {
            if self.bar_state.is_expanded() || self.anim_progress > 0.0 {
                self.set_expanded_immediate(false);
            }
            return frame;
        }
        self.step_animation(ui);
        let content = self.bar_state.content().clone();
        let hidden_count = store.sources(StorySourcesList::Hidden).len();

        // A collapse is due once the list scrolled a row past where the strip expanded.
        if self.bar_state.is_expanded()
            && slot.list_scroll_y > self.expand_scroll_top + COLLAPSE_SCROLL_SKIP
        {
            self.collapse(&mut frame.actions);
        }

        // Raw wheel input of this frame (positive = up): the chat list's scroll area consumes
        // the smoothed delta, and a pull is a direction, not a distance.
        let (pointer, scroll_y) = ui.input(|i| {
            let wheel: f32 = i
                .raw
                .events
                .iter()
                .map(|e| match e {
                    egui::Event::MouseWheel { delta, .. } => delta.y,
                    _ => 0.0,
                })
                .sum();
            (i.pointer.hover_pos(), wheel)
        });
        let small_opacity = (1.0 - self.anim_progress as f32).clamp(0.0, 1.0);
        if small_opacity > 0.01
            && let Some(small) = self.collapsed_rect(slot.field_rect)
        {
            self.paint_collapsed(
                ui,
                small,
                slot.field_bg,
                &content,
                backend,
                palette,
                small_opacity,
            );
            let resp = ui.interact(
                small,
                ui.make_persistent_id(("stories_strip_small", self.instance_id)),
                // Also senses drag so the press does not reach the search field's text edit
                // under the userpics (it focuses on a drag start).
                Sense::click_and_drag(),
            );
            let tooltip = format_tooltip_summary(&self.bar_state.tooltip_summary());
            if resp.on_hover_text(tooltip).clicked() {
                self.expand(slot.list_scroll_y, &mut frame.actions);
            }
            frame.collapsed_rect = Some(small);
        }

        // Scrolling up at the top of the chat list pulls the strip down; scrolling down past
        // the bottom collapses it (`position.overscroll > 0` in `setupStories`).
        let over_column = pointer.is_some_and(|p| slot.column_rect.contains(p));
        if over_column && scroll_y > 0.5 && slot.list_scroll_y <= 0.5 {
            self.expand(slot.list_scroll_y, &mut frame.actions);
        } else if over_column && scroll_y < -0.5 && slot.list_scroll_y >= slot.list_scroll_max - 0.5
        {
            self.collapse(&mut frame.actions);
        }

        let band_h = self.band_height();
        if band_h >= 1.0 {
            let width = ui.available_width().max(120.0);
            self.bar_state.set_viewport_width(width as i32);
            let (band, band_resp) =
                ui.allocate_exact_size(Vec2::new(width, band_h), Sense::click_and_drag());
            // The full strip slides out from under the search bar.
            let full = Rect::from_min_size(
                Pos2::new(band.left(), band.bottom() - self.metrics.full_height),
                Vec2::new(width, self.metrics.full_height),
            );
            let painter = ui.painter().with_clip_rect(band);
            painter.rect_filled(band, 0.0, palette.window_bg);

            if band_resp.hovered() && self.anim_progress > 0.5 {
                let delta = ui.input(|i| i.smooth_scroll_delta);
                if delta.x.abs() > 0.5 && self.bar_state.scroll_by((-delta.x).round() as i32) {
                    frame
                        .actions
                        .push(StoriesStripAction::LoadMore(self.active_list));
                }
                // The band is outside the chat list's scroll area; a wheel down over it scrolls
                // the list in tdesktop, which collapses the strip after a row.
                if scroll_y < -0.5 {
                    self.collapse(&mut frame.actions);
                }
            }
            if self.anim_progress > 0.5 && band_resp.dragged() {
                let dx = -band_resp.drag_delta().x;
                if dx.abs() > 0.5 && self.bar_state.scroll_by(dx.round() as i32) {
                    frame
                        .actions
                        .push(StoriesStripAction::LoadMore(self.active_list));
                }
            }

            let layout = compute_strip_layout(&self.bar_state, &self.metrics, width, 1.0);
            self.paint_expanded_bar(
                ui,
                &painter,
                full,
                band,
                &layout,
                &content,
                backend,
                palette,
                hidden_count,
                &mut frame.actions,
            );
            frame.band_rect = Some(band);
        }

        if let Some(menu_peer) = self.context_menu_peer {
            self.show_context_menu(
                ui.ctx(),
                menu_peer,
                backend,
                palette,
                hidden_count,
                &mut frame.actions,
            );
        }
        frame
    }

    /// Paints the collapsed userpics (`List::paint` with `expandRatio == 0`): right to left so the
    /// first one is on top, each cut out of its neighbour in the field colour, with the unread
    /// gradient ring and read userpics at `readOpacity`.
    #[allow(clippy::too_many_arguments)]
    fn paint_collapsed(
        &self,
        ui: &Ui,
        small: Rect,
        field_bg: Color32,
        content: &DialogStoriesContent,
        backend: &dyn StoriesBackend,
        palette: &Palette,
        opacity: f32,
    ) {
        let painter = ui.painter();
        let m = &self.metrics;
        let radius = m.small_photo / 2.0;
        let line = m.small_line;
        let visible = self.bar_state.small_visible_indices();
        for (slot_idx, &elem_idx) in visible.iter().enumerate().rev() {
            let Some(elem) = content.elements.get(elem_idx) else {
                continue;
            };
            let center = collapsed_photo_center(m, small, slot_idx);
            let profile = backend.peer_profile(PeerId(elem.id as i64));
            let unread = elem.is_unread_in_small();
            let ring_radius = radius + 1.5 * line;
            if elem.uploading {
                let color = if elem.upload_failed {
                    palette.attention_fg
                } else {
                    UNREAD_GRADIENT_TILL
                };
                let sweep = ((f32::from(elem.upload_progress) / 100.0) * 2.0 * PI).max(0.35);
                let start = -PI / 2.0 + self.upload_spin_phase;
                paint_arc(
                    painter,
                    center,
                    ring_radius,
                    start,
                    start + sweep,
                    Stroke::new(line, color.gamma_multiply(opacity)),
                );
            } else if elem.has_video_stream {
                painter.circle_stroke(
                    center,
                    ring_radius,
                    Stroke::new(line, palette.attention_fg.gamma_multiply(opacity)),
                );
            } else if unread {
                paint_gradient_circle(painter, center, radius, ring_radius, line, opacity);
            }
            painter.circle_filled(center, radius + line, field_bg.gamma_multiply(opacity));
            let photo_opacity = if unread { 1.0 } else { READ_OPACITY };
            paint_avatar_circle_faded(
                painter,
                center,
                radius,
                &profile,
                9.0,
                opacity * photo_opacity,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_expanded_bar(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        strip_rect: Rect,
        hit_rect: Rect,
        layout: &crate::stories::layout::StripRenderLayout,
        content: &DialogStoriesContent,
        backend: &dyn StoriesBackend,
        palette: &Palette,
        hidden_count: usize,
        actions: &mut Vec<StoriesStripAction>,
    ) {
        let photo_d = self.metrics.full_photo;
        let photo_r = photo_d / 2.0;
        let photo_top = strip_rect.top() + self.metrics.full_photo_top;
        let name_y = strip_rect.top() + self.metrics.full_name_top;

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
                cell_rect.intersect(hit_rect),
                ui.make_persistent_id((
                    "stories_strip_item",
                    self.instance_id,
                    self.active_list.index(),
                    elem.id,
                )),
                Sense::click(),
            );

            paint_story_ring(
                painter,
                center,
                photo_r + 3.0,
                elem,
                &self.metrics,
                palette,
                self.upload_spin_phase,
            );
            paint_avatar_circle(painter, center, photo_r, &profile, 14.5);

            let mut add_clicked = false;
            if elem.has_video_stream {
                let pill_rect = Rect::from_center_size(
                    Pos2::new(center.x, center.y + photo_r - 1.0),
                    Vec2::new(28.0, 12.0),
                );
                painter.rect_filled(pill_rect, CornerRadius::same(6), palette.attention_fg);
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
            } else if elem.mine && !elem.uploading {
                // `List::paintAddBadge`: a click on it posts a new story (`addStoryClicks`).
                let badge_c = Pos2::new(center.x + photo_r * 0.68, center.y + photo_r * 0.68);
                painter.circle_filled(badge_c, 9.0, palette.window_bg);
                painter.circle_filled(badge_c, 7.0, palette.window_bg_active);
                let plus_stroke = Stroke::new(2.0, palette.window_fg_active);
                painter.line_segment(
                    [
                        Pos2::new(badge_c.x - 4.0, badge_c.y),
                        Pos2::new(badge_c.x + 4.0, badge_c.y),
                    ],
                    plus_stroke,
                );
                painter.line_segment(
                    [
                        Pos2::new(badge_c.x, badge_c.y - 4.0),
                        Pos2::new(badge_c.x, badge_c.y + 4.0),
                    ],
                    plus_stroke,
                );
                let badge_rect = Rect::from_center_size(badge_c, Vec2::splat(18.0));
                add_clicked = item_resp.clicked()
                    && item_resp
                        .interact_pointer_pos()
                        .is_some_and(|p| badge_rect.contains(p));
            }

            let name_color = if elem.unread_count > 0 || elem.has_video_stream || elem.mine {
                palette.window_bold_fg
            } else {
                palette.window_sub_text_fg
            };
            painter.text(
                Pos2::new(center.x, name_y + 6.0),
                Align2::CENTER_CENTER,
                truncate_short_name(&elem.name, 9),
                FontId::proportional(11.0),
                name_color,
            );

            let item_resp = item_resp.on_hover_text(profile.name.clone());
            if add_clicked {
                actions.push(StoriesStripAction::AddStory);
            } else if item_resp.clicked() {
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

        // Archive chip at the end of the main strip if hidden sources exist.
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
                    cell_rect.intersect(hit_rect),
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
                    Stroke::new(1.0, palette.unread_badge_muted_bg),
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

        // A failed upload keeps a dismiss pill in the corner.
        if let Some(failed) = content
            .elements
            .iter()
            .find(|e| e.uploading && e.upload_failed)
        {
            let pill_rect = Rect::from_min_size(
                Pos2::new(strip_rect.right() - 108.0, strip_rect.bottom() - 23.0),
                Vec2::new(102.0, 18.0),
            );
            let pill_resp = ui.interact(
                pill_rect.intersect(hit_rect),
                ui.make_persistent_id(("stories_strip_upload_status", self.instance_id)),
                Sense::click(),
            );
            painter.rect_filled(pill_rect, CornerRadius::same(9), palette.attention_fg);
            painter.text(
                pill_rect.center(),
                Align2::CENTER_CENTER,
                "Upload failed ×",
                FontId::proportional(10.0),
                Color32::WHITE,
            );
            if pill_resp.on_hover_text("Dismiss failed upload").clicked() {
                actions.push(StoriesStripAction::DismissFailedUpload(PeerId(
                    failed.id as i64,
                )));
            }
        }

        // "Main" pill back from the archived list.
        if self.active_list == StorySourcesList::Hidden {
            let back_rect = Rect::from_min_size(
                Pos2::new(strip_rect.right() - 68.0, strip_rect.top() + 4.0),
                Vec2::new(60.0, 18.0),
            );
            let back_resp = ui.interact(
                back_rect.intersect(hit_rect),
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
    paint_avatar_circle_faded(painter, center, radius, profile, font_size, 1.0);
}

/// [`paint_avatar_circle`] at `opacity` (`readOpacity` for read stories in the collapsed strip).
pub fn paint_avatar_circle_faded(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    profile: &PeerProfile,
    font_size: f32,
    opacity: f32,
) {
    let [r0, g0, b0] = profile.avatar_top_rgb;
    let [r1, g1, b1] = profile.avatar_bottom_rgb;
    let mid = Color32::from_rgb(
        ((u16::from(r0) + u16::from(r1)) / 2) as u8,
        ((u16::from(g0) + u16::from(g1)) / 2) as u8,
        ((u16::from(b0) + u16::from(b1)) / 2) as u8,
    );
    painter.circle_filled(center, radius, mid.gamma_multiply(opacity));
    // Subtle top highlight arc for tdesktop avatar depth.
    painter.circle_filled(
        Pos2::new(center.x, center.y - radius * 0.22),
        radius * 0.65,
        Color32::from_rgba_unmultiplied(r0, g0, b0, 110).gamma_multiply(opacity),
    );
    painter.text(
        center,
        Align2::CENTER_CENTER,
        &profile.initials,
        FontId::proportional(font_size),
        Color32::WHITE.gamma_multiply(opacity),
    );
}

/// `Ui::UnreadStoryOutlineGradient` over the userpic square of `photo_radius` around `center`:
/// `groupCallLive1` at the top-right corner to `groupCallMuted1` at the bottom-left one.
fn unread_gradient_stroke(center: Pos2, photo_radius: f32, width: f32, opacity: f32) -> PathStroke {
    let from = Pos2::new(center.x + photo_radius, center.y - photo_radius);
    let dir = Vec2::new(-2.0 * photo_radius, 2.0 * photo_radius);
    let len_sq = dir.length_sq().max(f32::EPSILON);
    PathStroke::new_uv(width, move |_bounds, pos| {
        let t = ((pos - from).dot(dir) / len_sq).clamp(0.0, 1.0);
        UNREAD_GRADIENT_FROM
            .lerp_to_gamma(UNREAD_GRADIENT_TILL, t)
            .gamma_multiply(opacity)
    })
}

/// Unread ring of a collapsed userpic: one gradient circle of `ring_radius`.
fn paint_gradient_circle(
    painter: &Painter,
    center: Pos2,
    photo_radius: f32,
    ring_radius: f32,
    width: f32,
    opacity: f32,
) {
    let points = arc_points(center, ring_radius, -PI / 2.0, 3.0 * PI / 2.0);
    painter.add(Shape::closed_line(
        points,
        unread_gradient_stroke(center, photo_radius, width, opacity),
    ));
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
            UNREAD_GRADIENT_TILL
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
    let photo_radius = metrics.full_photo / 2.0;
    for seg in &segments {
        let (start, end) = ring_segment_arc_angles(total, seg.index);
        let points = arc_points(center, radius, start, end);
        match seg.kind {
            RingSegmentKind::VideoStream => {
                painter.add(Shape::line(
                    points,
                    Stroke::new(metrics.full_line_unread, palette.attention_fg),
                ));
            }
            RingSegmentKind::Unread => {
                painter.add(Shape::line(
                    points,
                    unread_gradient_stroke(center, photo_radius, metrics.full_line_unread, 1.0),
                ));
            }
            RingSegmentKind::Read => {
                painter.add(Shape::line(
                    points,
                    Stroke::new(metrics.full_line_read, palette.unread_badge_muted_bg),
                ));
            }
        }
    }
}

fn arc_points(center: Pos2, radius: f32, start_rad: f32, end_rad: f32) -> Vec<Pos2> {
    let sweep = (end_rad - start_rad).abs();
    let steps = ((sweep * radius / 3.0).ceil() as usize).clamp(4, 64);
    (0..=steps)
        .map(|i| {
            let a = start_rad + (end_rad - start_rad) * (i as f32 / steps as f32);
            Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
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
    painter.add(Shape::line(
        arc_points(center, radius, start_rad, end_rad),
        stroke,
    ));
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
