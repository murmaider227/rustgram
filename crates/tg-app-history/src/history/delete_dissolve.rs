//! Deleted messages turn to dust where they were painted (TGB-3011).
//!
//! tdesktop (`Ui::ThanosEffectController::captureView`) renders the removed view onto an image
//! and hands it to `Ui::ThanosEffect`, then the list closes the hole
//! ([`super::delete_collapse`]). egui keeps no per-message picture, so the messages stay in the
//! list for one more frame, that frame is captured with `ViewportCommand::Screenshot`, and each
//! message's painted bubble ([`super::inner::HistoryInnerWidget::last_painted_bubble_rect`]) is
//! cut out of it. Then the messages leave the list. The delay is one or two frames; a backend
//! that never delivers the screenshot gets the removal without dust after a few frames.

use std::sync::{Arc, Mutex, PoisonError};

use egui::{ColorImage, CornerRadius, Rect};
use tg_ui::effects::thanos::{particle_budget, snapshot_region};

use super::HistoryWidget;
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

/// Frames to wait for a screenshot before the messages go without dust.
const SNAPSHOT_FRAMES: u32 = 3;

type SharedShot = Arc<Mutex<Option<Arc<ColorImage>>>>;

/// Messages waiting for the snapshot they dissolve from.
#[derive(Debug)]
pub(crate) struct PendingDissolve {
    peer: PeerId,
    ids: Vec<MsgId>,
    shot: SharedShot,
    /// Frames since the screenshot was asked for (`None`: not asked yet).
    frames_since_request: Option<u32>,
}

impl PendingDissolve {
    fn take_shot(&self) -> Option<Arc<ColorImage>> {
        self.shot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// `rect` limited to `visible`, with the corners cut by the edges of the view left square
/// (`captureView` captures only the visible part of a view).
fn visible_part(rect: Rect, corners: CornerRadius, visible: Rect) -> Option<(Rect, CornerRadius)> {
    let part = rect.intersect(visible);
    if !part.is_positive() {
        return None;
    }
    let mut corners = corners;
    if part.top() > rect.top() {
        corners.nw = 0;
        corners.ne = 0;
    }
    if part.bottom() < rect.bottom() {
        corners.sw = 0;
        corners.se = 0;
    }
    Some((part, corners))
}

impl HistoryWidget {
    /// Removes `ids` from the open chat the way tdesktop does: the messages on screen turn to
    /// dust and the holes close. Without chat effects or animations (power saving) they just go
    /// (`ThanosEffect::Supported`).
    pub(crate) fn remove_messages_with_effect(
        &mut self,
        ctx: &egui::Context,
        peer_id: PeerId,
        ids: &[MsgId],
        peer_kind: PeerKind,
    ) {
        if !self.thanos_policy().allows_effect() {
            self.inner.remove_messages(ids, peer_kind);
            return;
        }
        match self.pending_dissolve.as_mut() {
            Some(pending) if pending.peer == peer_id => {
                for id in ids {
                    if !pending.ids.contains(id) {
                        pending.ids.push(*id);
                    }
                }
            }
            _ => {
                self.pending_dissolve = Some(PendingDissolve {
                    peer: peer_id,
                    ids: ids.to_vec(),
                    shot: SharedShot::default(),
                    frames_since_request: None,
                });
            }
        }
        ctx.request_repaint();
    }

    /// Before the list paints: once the snapshot has come (or did not), the waiting messages
    /// turn to dust and leave the list.
    pub(crate) fn finish_pending_dissolve(
        &mut self,
        ctx: &egui::Context,
        peer_id: PeerId,
        peer_kind: PeerKind,
        list_rect: Rect,
    ) {
        let Some(pending) = self.pending_dissolve.as_ref() else {
            return;
        };
        if pending.peer != peer_id {
            // Another chat is open: the deleted messages are not in it.
            self.pending_dissolve = None;
            return;
        }
        let shot = pending.take_shot();
        let waited_out = pending
            .frames_since_request
            .is_some_and(|frames| frames >= SNAPSHOT_FRAMES);
        if shot.is_none() && !waited_out {
            return;
        }
        let Some(pending) = self.pending_dissolve.take() else {
            return;
        };
        let policy = self.thanos_policy();
        if !policy.allows_effect() {
            // Effects were turned off while the messages waited.
            self.inner.remove_messages(&pending.ids, peer_kind);
            return;
        }
        if let Some(shot) = shot {
            let parts: Vec<(Rect, CornerRadius)> = pending
                .ids
                .iter()
                .filter(|id| {
                    self.inner
                        .messages()
                        .iter()
                        .any(|m| m.id == **id && m.id.0 > 0 && !m.is_service())
                })
                .filter_map(|id| {
                    let rect = self.inner.last_painted_bubble_rect(*id)?;
                    let corners = self.inner.bubble_corners(*id);
                    visible_part(rect, corners, list_rect)
                })
                .collect();
            let budget = particle_budget(parts.len());
            let pixels_per_point = ctx.pixels_per_point();
            for (rect, corners) in parts {
                if let Some(image) = snapshot_region(&shot, rect, pixels_per_point, corners) {
                    self.thanos_effect
                        .add_item(ctx, image, rect, budget, policy);
                }
            }
        }
        let now_ms = ctx.input(|i| i.time) * 1000.0;
        self.inner
            .remove_messages_collapsing(&pending.ids, peer_kind, now_ms);
        ctx.request_repaint();
    }

    /// After the list paints: asks for a screenshot of this frame while messages wait for it.
    pub(crate) fn request_dissolve_snapshot(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_dissolve.as_mut() else {
            return;
        };
        pending.frames_since_request = Some(match pending.frames_since_request {
            None => {
                let shot = Arc::clone(&pending.shot);
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::ScreenshotCallback::new(move |image| {
                        *shot.lock().unwrap_or_else(PoisonError::into_inner) = Some(image);
                    }),
                ));
                0
            }
            Some(frames) => frames + 1,
        });
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Vec2};

    #[test]
    fn a_bubble_cut_by_the_view_keeps_square_corners_at_the_cut() {
        let view = Rect::from_min_max(Pos2::new(0.0, 100.0), Pos2::new(800.0, 600.0));
        let bubble = Rect::from_min_size(Pos2::new(400.0, 80.0), Vec2::new(200.0, 60.0));
        let (part, corners) =
            visible_part(bubble, CornerRadius::same(16), view).expect("partly visible");
        assert_eq!(part.top(), 100.0);
        assert_eq!(
            (corners.nw, corners.ne, corners.sw, corners.se),
            (0, 0, 16, 16)
        );
        assert!(
            visible_part(
                bubble.translate(Vec2::new(0.0, -200.0)),
                CornerRadius::ZERO,
                view
            )
            .is_none()
        );
    }
}
