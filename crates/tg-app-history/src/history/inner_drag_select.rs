//! A text drag that leaves the message it began in (TGB-3026).
//!
//! Port origin: `HistoryInner::mouseActionUpdate` (`_selectScroll.checkDeltaScroll`),
//! `Ui::DraggingScrollManager` and `HistoryInner::mouseActionFinish` (`applyDragSelection`).
//! While the left button is held above or below the list, the list scrolls (up to 37 px per
//! 15 ms) and the drag follows the message under the pointer; on release, a drag that crossed
//! messages turns into selected messages, which opens the selection bar.
//!
//! Internal design: tdesktop scrolls from a 15 ms timer. Here each frame scrolls by the ticks
//! that passed since the previous one and asks for the next frame while the pointer is outside.

use egui::{Context, Rect};
use tg_history::selection::{
    DRAG_SCROLL_TICK_MS, FlatSelectionEndpoint, ListSelectionEndpoint, drag_scroll_delta,
    drag_scroll_step,
};

use super::{FullMsgId, HistoryInnerWidget, to_hist_full};

/// Ticks one frame may catch up on after a stall, so a hitch does not fling the list.
const MAX_TICKS_PER_FRAME: u32 = 4;

impl HistoryInnerWidget {
    /// Runs once a frame after the list is painted: scrolls while a text drag is held past an
    /// edge, moves its end to the message under the pointer, and applies it on release.
    pub(super) fn drive_text_drag(&mut self, ctx: &Context, list_rect: Rect) {
        if !self.text_drag_active {
            return;
        }
        let (down, pointer, now) =
            ctx.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos(), i.time));
        if !down {
            self.finish_text_drag();
            return;
        }
        let Some(pointer) = pointer else {
            self.drag_scroll_clock = None;
            return;
        };
        let delta = drag_scroll_delta(
            pointer.y.floor() as i32,
            list_rect.top().floor() as i32,
            list_rect.bottom().floor() as i32,
        );
        if delta == 0 {
            self.drag_scroll_clock = None;
            return;
        }
        // The drag's end follows the message at the list's edge the pointer is beyond.
        let edge_y = pointer.y.clamp(list_rect.top(), list_rect.bottom() - 1.0);
        let content_y = edge_y - list_rect.top() + self.scroll_offset_y + self.bottom_stretch;
        self.extend_text_drag_to(content_y.floor() as i32);

        let tick = f64::from(DRAG_SCROLL_TICK_MS) / 1000.0;
        let last = *self.drag_scroll_clock.get_or_insert(now - tick);
        let ticks = ((now - last) / tick)
            .floor()
            .clamp(0.0, f64::from(MAX_TICKS_PER_FRAME));
        if ticks >= 1.0 {
            // `ticks` is a small whole number (at most `MAX_TICKS_PER_FRAME`).
            let step = drag_scroll_step(delta) as f32 * ticks as f32;
            let top = self.pending_scroll_y.unwrap_or(self.scroll_offset_y);
            self.pending_scroll_y = Some((top + step).max(0.0));
            self.drag_scroll_clock = Some(last + ticks * tick);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(u64::from(
            DRAG_SCROLL_TICK_MS,
        )));
    }

    /// Moves the end of the text drag to the message at `content_y` when that is another
    /// message than the one the drag began in (whole messages are selected then).
    fn extend_text_drag_to(&mut self, content_y: i32) {
        let Some(from) = self.list.selection.from else {
            return;
        };
        let Some(index) = self.list.find_item_index_by_y(content_y) else {
            return;
        };
        if index == from.item_index {
            return;
        }
        let Some(msg) = self.messages.get(index) else {
            return;
        };
        self.list.selection.update_drag(ListSelectionEndpoint {
            item_id: to_hist_full(FullMsgId {
                peer: self.peer_id,
                msg: msg.id,
            }),
            item_index: index,
            text: FlatSelectionEndpoint::new(0, false),
        });
    }

    /// `mouseActionFinish`: a drag that crossed messages selects them (service messages
    /// cannot be selected); a drag inside one message keeps its text selection.
    fn finish_text_drag(&mut self) {
        self.text_drag_active = false;
        self.drag_scroll_clock = None;
        let Some((first, last)) = self.list.selection.dragged_items() else {
            return;
        };
        let last = last.min(self.messages.len().saturating_sub(1));
        let peer = self.peer_id;
        let items: Vec<_> = self
            .messages
            .get(first..=last)
            .unwrap_or_default()
            .iter()
            .filter(|m| !m.is_service())
            .map(|m| to_hist_full(FullMsgId { peer, msg: m.id }))
            .collect();
        self.list.selection.apply_drag_selection(items);
    }
}
