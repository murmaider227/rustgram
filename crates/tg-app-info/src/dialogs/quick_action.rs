//! Configurable swipe / hover / middle-click quick action on dialog rows.
//!
//! Port origin:
//! - `Telegram/SourceFiles/dialogs/dialogs_quick_action.h`
//! - `Telegram/SourceFiles/dialogs/dialogs_quick_action.cpp`
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_quick_action.h`
//! - `Telegram/SourceFiles/dialogs/dialogs.style` (`dialogsQuickActionSize`, `dialogsQuickActionRippleSize`)

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Painter, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_core::settings::QuickDialogAction;
use tg_lang::tr;
use tg_ui::anim::{SimpleAnimation, Transition, with_alpha};
use tg_ui::fonts::font_id_from_style;
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::{Colors, ColorsResolver};

use crate::dialogs::backend::{
    DialogItem, DialogListQuery, DialogPeerKind, DialogsBackend, SpecialRowKind,
};
use tg_app_core::stand_in::types::PeerId;

/// Icon size in the quick-action strip (`st::dialogsQuickActionSize = 20px`).
pub const QUICK_ACTION_ICON_SIZE: f32 = 20.0;
/// Full revealed quick-action strip width (`st::dialogsQuickActionRippleSize = 80px`).
pub const QUICK_ACTION_STRIP_WIDTH: f32 = 80.0;
/// Horizontal swipe distance threshold (in logical pixels) to commit the quick action.
pub const QUICK_ACTION_SWIPE_THRESHOLD: f32 = 56.0;
/// Duration of the quick-action swipe snap-back animation (`ms`).
pub const QUICK_ACTION_SNAP_DURATION_MS: u32 = 180;
/// Duration of the hover quick-action pill fade-in animation (`ms`).
pub const QUICK_ACTION_HOVER_FADE_MS: u32 = 150;

/// Resolved contextual action label for a dialog row (`Dialogs::Ui::QuickDialogActionLabel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuickDialogActionLabel {
    /// Mute chat notifications.
    Mute,
    /// Unmute chat notifications.
    Unmute,
    /// Pin chat to top of the current list.
    Pin,
    /// Unpin chat from top of the current list.
    Unpin,
    /// Mark unread chat as read.
    Read,
    /// Mark read chat as unread.
    Unread,
    /// Move chat into Archive.
    Archive,
    /// Remove chat from Archive.
    Unarchive,
    /// Delete conversation or leave group/channel.
    Delete,
    /// Ungroup a collapsed community row.
    Ungroup,
    /// Action unavailable / disabled for this row.
    Disabled,
}

/// Resolves the concrete [`QuickDialogActionLabel`] for `item` given configured `action` and
/// whether `item` is currently pinned in the active folder/list.
///
/// Direct port of `Dialogs::ResolveQuickDialogLabel` (`dialogs_quick_action.cpp:192-231`).
#[must_use]
pub fn resolve_quick_dialog_label(
    item: &DialogItem,
    action: QuickDialogAction,
    is_pinned_in_list: bool,
) -> QuickDialogActionLabel {
    let is_saved = item.special == SpecialRowKind::SavedMessages;
    match action {
        QuickDialogAction::Mute => {
            if is_saved {
                QuickDialogActionLabel::Disabled
            } else if item.muted {
                QuickDialogActionLabel::Unmute
            } else {
                QuickDialogActionLabel::Mute
            }
        }
        QuickDialogAction::Pin => {
            if is_pinned_in_list {
                QuickDialogActionLabel::Unpin
            } else {
                QuickDialogActionLabel::Pin
            }
        }
        QuickDialogAction::Read => {
            let unread = item.badges.unread || item.badges.unread_counter > 0;
            if item.kind == DialogPeerKind::Forum && !unread {
                QuickDialogActionLabel::Disabled
            } else if unread {
                QuickDialogActionLabel::Read
            } else {
                QuickDialogActionLabel::Unread
            }
        }
        QuickDialogAction::Archive => {
            if is_saved {
                QuickDialogActionLabel::Disabled
            } else if item.archived {
                QuickDialogActionLabel::Unarchive
            } else {
                QuickDialogActionLabel::Archive
            }
        }
        QuickDialogAction::Delete => {
            if is_saved {
                QuickDialogActionLabel::Disabled
            } else {
                QuickDialogActionLabel::Delete
            }
        }
        QuickDialogAction::Disabled => QuickDialogActionLabel::Disabled,
    }
}

/// Returns the localized label for `label` via `tg_lang::tr::*`
/// (`Dialogs::ResolveQuickDialogLabel`, `dialogs_quick_action.cpp:233-258`).
#[must_use]
pub fn quick_dialog_label_text(label: QuickDialogActionLabel) -> String {
    match label {
        QuickDialogActionLabel::Mute => tr::lng_settings_quick_dialog_action_mute(),
        QuickDialogActionLabel::Unmute => tr::lng_settings_quick_dialog_action_unmute(),
        QuickDialogActionLabel::Pin => tr::lng_settings_quick_dialog_action_pin(),
        QuickDialogActionLabel::Unpin => tr::lng_settings_quick_dialog_action_unpin(),
        QuickDialogActionLabel::Read => tr::lng_settings_quick_dialog_action_read(),
        QuickDialogActionLabel::Unread => tr::lng_settings_quick_dialog_action_unread(),
        QuickDialogActionLabel::Archive => tr::lng_settings_quick_dialog_action_archive(),
        QuickDialogActionLabel::Unarchive => tr::lng_settings_quick_dialog_action_unarchive(),
        QuickDialogActionLabel::Delete => tr::lng_settings_quick_dialog_action_delete(),
        QuickDialogActionLabel::Ungroup => tr::lng_community_ungroup(),
        QuickDialogActionLabel::Disabled => tr::lng_settings_quick_dialog_action_disabled(),
    }
}

/// Returns the tdesktop Lottie icon stem name for `label`
/// (`Dialogs::ResolveQuickDialogLottieIconName`, `dialogs_quick_action.cpp:165-190`).
#[must_use]
pub const fn quick_dialog_lottie_icon_name(label: QuickDialogActionLabel) -> &'static str {
    match label {
        QuickDialogActionLabel::Mute => "swipe_mute",
        QuickDialogActionLabel::Unmute => "swipe_unmute",
        QuickDialogActionLabel::Pin => "swipe_pin",
        QuickDialogActionLabel::Unpin => "swipe_unpin",
        QuickDialogActionLabel::Read => "swipe_read",
        QuickDialogActionLabel::Unread => "swipe_unread",
        QuickDialogActionLabel::Archive => "swipe_archive",
        QuickDialogActionLabel::Unarchive => "swipe_unarchive",
        QuickDialogActionLabel::Delete => "swipe_delete",
        QuickDialogActionLabel::Ungroup => "swipe_ungroup",
        QuickDialogActionLabel::Disabled => "swipe_disabled",
    }
}

/// Returns the `tg-style` menu icon corresponding to `label`.
#[must_use]
pub fn quick_action_style_icon(label: QuickDialogActionLabel) -> Icon {
    match label {
        QuickDialogActionLabel::Mute => st::menu_icon_mute,
        QuickDialogActionLabel::Unmute => st::menu_icon_unmute,
        QuickDialogActionLabel::Pin => st::menu_icon_pin,
        QuickDialogActionLabel::Unpin => st::menu_icon_unpin,
        QuickDialogActionLabel::Read => st::menu_icon_mark_read,
        QuickDialogActionLabel::Unread => st::menu_icon_mark_unread,
        QuickDialogActionLabel::Archive => st::menu_icon_archive,
        QuickDialogActionLabel::Unarchive => st::menu_icon_unarchive,
        QuickDialogActionLabel::Delete | QuickDialogActionLabel::Ungroup => st::menu_icon_delete,
        QuickDialogActionLabel::Disabled => st::menu_icon_show_in_folder,
    }
}

/// Resolves the quick-action strip background color (`Dialogs::ResolveQuickActionBg`,
/// `dialogs_quick_action.cpp:260-279`).
#[must_use]
pub fn quick_action_bg_color(label: QuickDialogActionLabel, colors: &dyn Colors) -> Color32 {
    match label {
        QuickDialogActionLabel::Delete | QuickDialogActionLabel::Ungroup => {
            colors.color("attentionButtonFg")
        }
        QuickDialogActionLabel::Disabled => colors.color("windowSubTextFgOver"),
        QuickDialogActionLabel::Mute
        | QuickDialogActionLabel::Unmute
        | QuickDialogActionLabel::Pin
        | QuickDialogActionLabel::Unpin
        | QuickDialogActionLabel::Read
        | QuickDialogActionLabel::Unread
        | QuickDialogActionLabel::Archive
        | QuickDialogActionLabel::Unarchive => colors.color("windowBgActive"),
    }
}

/// Executes `action` on `peer_id` through `backend`, returning the localized toast message
/// (`Dialogs::PerformQuickDialogAction`, `dialogs_quick_action.cpp:96-163`).
pub fn perform_quick_dialog_action(
    backend: &mut dyn DialogsBackend,
    peer_id: PeerId,
    action: QuickDialogAction,
    list: DialogListQuery,
) -> Option<String> {
    let item = backend.dialog_by_peer(peer_id)?;
    let slice = backend.dialog_slice(list, 0, 20);
    let is_pinned = slice
        .entries
        .iter()
        .take(slice.pinned_count)
        .any(|d| d.peer_id == peer_id);
    let label = resolve_quick_dialog_label(&item, action, is_pinned);
    if label == QuickDialogActionLabel::Disabled {
        return None;
    }

    match action {
        QuickDialogAction::Mute => {
            let muted = backend.toggle_mute_dialog(peer_id);
            Some(if muted {
                tr::lng_quick_dialog_action_toast_mute_success()
            } else {
                tr::lng_quick_dialog_action_toast_unmute_success()
            })
        }
        QuickDialogAction::Pin => {
            let pinned = backend.toggle_pin(peer_id, list);
            Some(if pinned {
                tr::lng_quick_dialog_action_toast_pin_success()
            } else {
                tr::lng_quick_dialog_action_toast_unpin_success()
            })
        }
        QuickDialogAction::Read => {
            let unread = item.badges.unread || item.badges.unread_counter > 0;
            if unread {
                backend.mark_dialog_read(peer_id);
                Some(tr::lng_quick_dialog_action_toast_read_success())
            } else {
                backend.mark_dialog_unread(peer_id);
                Some(tr::lng_quick_dialog_action_toast_unread_success())
            }
        }
        QuickDialogAction::Archive => {
            let archived = backend.toggle_archive(peer_id);
            Some(if archived {
                tr::lng_quick_dialog_action_toast_archive_success()
            } else {
                tr::lng_quick_dialog_action_toast_unarchive_success()
            })
        }
        // Never deletes directly: the caller opens the `DeleteAndLeaveHandler` confirmation box.
        QuickDialogAction::Delete => None,
        QuickDialogAction::Disabled => None,
    }
}

/// Active horizontal swipe-to-act gesture state on a single dialog row.
#[derive(Debug, Clone)]
pub struct DialogSwipeState {
    /// Peer whose row is currently swiped or snapping back.
    pub peer_id: PeerId,
    /// Current horizontal reveal distance in logical pixels (`0.0..=QUICK_ACTION_STRIP_WIDTH`).
    pub reveal_px: f32,
    /// `true` while the pointer is actively dragging horizontally.
    pub dragging: bool,
    /// Whether the gesture crossed [`QUICK_ACTION_SWIPE_THRESHOLD`] during drag.
    pub armed: bool,
    /// Snap-back animation once the drag ends.
    snap_anim: SimpleAnimation,
}

impl DialogSwipeState {
    /// Starts a new horizontal swipe on `peer_id`.
    #[must_use]
    pub fn start(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            reveal_px: 0.0,
            dragging: true,
            armed: false,
            snap_anim: SimpleAnimation::new(),
        }
    }

    /// Updates horizontal drag by signed `delta_x` (negative = swipe left).
    pub fn update_drag(&mut self, total_left_px: f32) {
        self.dragging = true;
        self.reveal_px = total_left_px.clamp(0.0, QUICK_ACTION_STRIP_WIDTH);
        self.armed = self.reveal_px >= QUICK_ACTION_SWIPE_THRESHOLD;
    }

    /// Finishes the drag gesture, returning `true` if the action should be executed, and starts
    /// the smooth snap-back animation.
    pub fn finish_drag(&mut self, now_secs: f64, reduced_motion: bool) -> bool {
        self.dragging = false;
        let committed = self.armed;
        let duration = if reduced_motion {
            0
        } else {
            QUICK_ACTION_SNAP_DURATION_MS
        };
        self.snap_anim.start(
            now_secs,
            f64::from(self.reveal_px),
            0.0,
            duration,
            Transition::EaseOutCirc,
        );
        if reduced_motion {
            self.reveal_px = 0.0;
        }
        committed
    }

    /// Samples current reveal width and returns `true` if the state is still active.
    pub fn sample(&mut self, now_secs: f64) -> bool {
        if self.dragging {
            return true;
        }
        self.reveal_px = self.snap_anim.sample(now_secs, 0.0) as f32;
        self.snap_anim.animating() || self.reveal_px > 0.5
    }
}

/// Hover quick-action pill animation tracker for the currently hovered dialog row.
#[derive(Debug, Default)]
pub struct DialogHoverQuickAction {
    hovered_peer: Option<PeerId>,
    fade_anim: SimpleAnimation,
}

impl DialogHoverQuickAction {
    /// Updates hovered peer and returns opacity in `0.0..=1.0` for `peer_id`.
    pub fn opacity_for(
        &mut self,
        peer_id: PeerId,
        is_hovered: bool,
        now_secs: f64,
        reduced_motion: bool,
    ) -> f32 {
        if is_hovered && self.hovered_peer != Some(peer_id) {
            self.hovered_peer = Some(peer_id);
            let dur = if reduced_motion {
                0
            } else {
                QUICK_ACTION_HOVER_FADE_MS
            };
            self.fade_anim
                .start(now_secs, 0.0, 1.0, dur, Transition::EaseOutCubic);
        }
        if self.hovered_peer == Some(peer_id) {
            if is_hovered {
                self.fade_anim.sample(now_secs, 1.0) as f32
            } else {
                self.hovered_peer = None;
                0.0
            }
        } else {
            0.0
        }
    }

    /// Returns `true` if the hover fade animation is actively running.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.fade_anim.animating()
    }
}

/// Paints the full-height swipe quick-action strip revealed on the right edge of `row_rect`
/// (`Dialogs::DrawQuickAction`, `dialogs_quick_action.cpp:286-317`).
pub fn paint_quick_action_strip(
    painter: &Painter,
    row_rect: Rect,
    reveal_px: f32,
    label: QuickDialogActionLabel,
    armed: bool,
    colors: &dyn Colors,
) {
    let width = reveal_px.clamp(0.0, row_rect.width().min(QUICK_ACTION_STRIP_WIDTH));
    if width < 2.0 {
        return;
    }
    let strip_rect = Rect::from_min_max(
        Pos2::new(row_rect.right() - width, row_rect.top()),
        row_rect.right_bottom(),
    );
    let bg = if armed {
        quick_action_bg_color(label, colors)
    } else {
        with_alpha(quick_action_bg_color(label, colors), 0.82)
    };
    painter.rect_filled(strip_rect, CornerRadius::ZERO, bg);

    let fg = Color32::WHITE;
    let icon_ratio = if armed { 1.08 } else { 0.92 };
    let icon_size = QUICK_ACTION_ICON_SIZE * icon_ratio;
    let inner_height = icon_size * 2.0;
    let top = strip_rect.top() + (strip_rect.height() - inner_height) * 0.5;

    let icon_rect = Rect::from_center_size(
        Pos2::new(strip_rect.center().x, top + icon_size * 0.5),
        Vec2::splat(icon_size),
    );
    let resolver = ColorsResolver(colors);
    paint_style_icon(
        painter,
        icon_rect,
        quick_action_style_icon(label),
        &resolver,
        Some(fg),
    );

    if width >= 44.0 {
        let text = quick_dialog_label_text(label);
        painter.text(
            Pos2::new(strip_rect.center().x, top + inner_height - 2.0),
            Align2::CENTER_BOTTOM,
            text,
            font_id_from_style(st::semibold_font),
            fg,
        );
    }
}

/// Paints and interacts with the floating hover quick-action pill on the right edge of a dialog
/// row, returning `true` when clicked.
#[allow(clippy::too_many_arguments)]
pub fn show_hover_quick_action_pill(
    ui: &mut Ui,
    row_rect: Rect,
    peer_id: PeerId,
    peer_title: &str,
    label: QuickDialogActionLabel,
    opacity: f32,
    colors: &dyn Colors,
) -> bool {
    if label == QuickDialogActionLabel::Disabled || opacity <= 0.02 || row_rect.width() < 140.0 {
        return false;
    }
    let label_text = quick_dialog_label_text(label);
    let pill_w = (label_text.len() as f32 * 6.5 + 38.0).clamp(68.0, 108.0);
    let pill_h = 28.0;
    let slide_x = (1.0 - opacity) * 6.0;
    let pill_rect = Rect::from_center_size(
        Pos2::new(
            row_rect.right() - 10.0 - pill_w * 0.5 + slide_x,
            row_rect.center().y,
        ),
        Vec2::new(pill_w, pill_h),
    );
    let resp = ui.interact(
        pill_rect,
        ui.scope_id().with(("tg_dialog_quick_action", peer_id.0)),
        Sense::click(),
    );
    let accessible = format!("Quick action: {label_text} ({peer_title})");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, accessible.clone()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let base_bg = quick_action_bg_color(label, colors);
    let alpha = f64::from(opacity);
    let bg = with_alpha(base_bg, if resp.hovered() { alpha } else { alpha * 0.92 });
    let fg = with_alpha(Color32::WHITE, alpha);
    ui.painter()
        .rect_filled(pill_rect, CornerRadius::same(14), bg);
    if resp.hovered() {
        ui.painter().rect_stroke(
            pill_rect,
            CornerRadius::same(14),
            Stroke::new(1.0, with_alpha(Color32::WHITE, 0.35 * alpha)),
            egui::StrokeKind::Inside,
        );
    }

    let icon_rect = Rect::from_center_size(
        Pos2::new(pill_rect.left() + 15.0, pill_rect.center().y),
        Vec2::splat(16.0),
    );
    let resolver = ColorsResolver(colors);
    paint_style_icon(
        ui.painter(),
        icon_rect,
        quick_action_style_icon(label),
        &resolver,
        Some(fg),
    );
    ui.painter().text(
        Pos2::new(pill_rect.left() + 28.0, pill_rect.center().y),
        Align2::LEFT_CENTER,
        label_text,
        egui::FontId::new(12.0, tg_ui::fonts::family_semibold()),
        fg,
    );

    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::mock::MockDialogsStore;
    use tg_app_core::stand_in::types::FilterId;

    #[test]
    fn resolve_label_respects_item_state_and_saved_messages() {
        let store = MockDialogsStore::new();
        let saved_id = store.saved_messages_peer_id();
        let saved = store.dialog_by_peer(saved_id).expect("saved messages");
        assert_eq!(
            resolve_quick_dialog_label(&saved, QuickDialogAction::Mute, true),
            QuickDialogActionLabel::Disabled
        );
        assert_eq!(
            resolve_quick_dialog_label(&saved, QuickDialogAction::Archive, true),
            QuickDialogActionLabel::Disabled
        );
        assert_eq!(
            resolve_quick_dialog_label(&saved, QuickDialogAction::Delete, true),
            QuickDialogActionLabel::Disabled
        );
        assert_eq!(
            resolve_quick_dialog_label(&saved, QuickDialogAction::Pin, true),
            QuickDialogActionLabel::Unpin
        );

        let slice = store.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 20);
        let peer = slice
            .entries
            .iter()
            .find(|d| d.special == SpecialRowKind::None)
            .expect("regular dialog");
        assert_eq!(
            resolve_quick_dialog_label(peer, QuickDialogAction::Mute, false),
            if peer.muted {
                QuickDialogActionLabel::Unmute
            } else {
                QuickDialogActionLabel::Mute
            }
        );
        assert_eq!(
            resolve_quick_dialog_label(peer, QuickDialogAction::Archive, false),
            QuickDialogActionLabel::Archive
        );
        assert_eq!(
            resolve_quick_dialog_label(peer, QuickDialogAction::Disabled, false),
            QuickDialogActionLabel::Disabled
        );
    }

    #[test]
    fn perform_quick_dialog_action_mutates_backend_for_all_modes() {
        let mut store = MockDialogsStore::new();
        let list = DialogListQuery::Filter(FilterId::ALL);
        let peer_id = store
            .dialog_slice(list, 0, 20)
            .entries
            .iter()
            .find(|d| d.special == SpecialRowKind::None && !d.pinned && !d.muted)
            .expect("unpinned unmuted peer")
            .peer_id;

        // Mute -> Unmute
        let toast_mute =
            perform_quick_dialog_action(&mut store, peer_id, QuickDialogAction::Mute, list);
        assert!(toast_mute.is_some());
        assert!(store.dialog_by_peer(peer_id).unwrap().muted);
        let toast_unmute =
            perform_quick_dialog_action(&mut store, peer_id, QuickDialogAction::Mute, list);
        assert!(toast_unmute.is_some());
        assert!(!store.dialog_by_peer(peer_id).unwrap().muted);

        // Pin -> Unpin
        assert!(
            perform_quick_dialog_action(&mut store, peer_id, QuickDialogAction::Pin, list)
                .is_some()
        );
        assert!(store.dialog_by_peer(peer_id).unwrap().pinned);
        assert!(
            perform_quick_dialog_action(&mut store, peer_id, QuickDialogAction::Pin, list)
                .is_some()
        );
        assert!(!store.dialog_by_peer(peer_id).unwrap().pinned);

        // Read toggle
        let was_unread = {
            let d = store.dialog_by_peer(peer_id).unwrap();
            d.badges.unread || d.badges.unread_counter > 0
        };
        assert!(
            perform_quick_dialog_action(&mut store, peer_id, QuickDialogAction::Read, list)
                .is_some()
        );
        let now_unread = {
            let d = store.dialog_by_peer(peer_id).unwrap();
            d.badges.unread || d.badges.unread_counter > 0
        };
        assert_ne!(was_unread, now_unread);

        // Archive -> Unarchive
        assert!(
            perform_quick_dialog_action(&mut store, peer_id, QuickDialogAction::Archive, list)
                .is_some()
        );
        assert!(store.dialog_by_peer(peer_id).unwrap().archived);
        assert!(
            perform_quick_dialog_action(&mut store, peer_id, QuickDialogAction::Archive, list)
                .is_some()
        );
        assert!(!store.dialog_by_peer(peer_id).unwrap().archived);
    }

    #[test]
    fn swipe_state_threshold_and_snap_back() {
        let mut swipe = DialogSwipeState::start(PeerId(42));
        swipe.update_drag(32.0);
        assert!(!swipe.armed);
        assert!(!swipe.finish_drag(1.0, false));
        assert!(swipe.sample(1.05));
        assert!(!swipe.sample(1.5));

        let mut swipe2 = DialogSwipeState::start(PeerId(42));
        swipe2.update_drag(70.0);
        assert!(swipe2.armed);
        assert!(swipe2.finish_drag(2.0, true));
        assert_eq!(swipe2.reveal_px, 0.0);
    }
}
