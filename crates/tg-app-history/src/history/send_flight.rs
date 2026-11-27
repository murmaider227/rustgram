//! Send animation of a text message (`Ui::MessageSendingTextFlight`,
//! `message_sending_text_flight.cpp`): the sent message is drawn from where its text was in
//! the field and slides into its place in the list, while the field text fades into it.
//!
//! tdesktop draws the real message view in a widget above the chat, and the list skips that
//! message while it flies (`hasAnimatedMessage`). Here the list paints the message into its own
//! layer (above the composer) with the cross-fade opacity, and the whole layer is translated by
//! the flight offset after the list is done; the message keeps its own bubble, width and side.

use std::sync::Arc;

use egui::emath::TSTransform;
use egui::{Galley, Id, LayerId, Pos2, Rect};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_ui::effects::{FlyEffectPolicy, MessageSendingTextFlight};

/// The field text when it was sent (`MessageSendingAnimationFrom::FieldText`).
#[derive(Clone)]
pub struct SendingFieldText {
    /// The field's laid-out text.
    pub galley: Arc<Galley>,
    /// Where the galley was drawn on screen.
    pub pos: Pos2,
}

impl std::fmt::Debug for SendingFieldText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendingFieldText")
            .field("pos", &self.pos)
            .field("size", &self.galley.size())
            .finish_non_exhaustive()
    }
}

impl SendingFieldText {
    /// The field text if all of it is visible in `clip` (`GrabSendingFieldText` refuses text
    /// scrolled out of the field).
    #[must_use]
    pub fn visible_in(galley: Arc<Galley>, pos: Pos2, clip: Rect) -> Option<Self> {
        let ink = galley.rect.translate(pos.to_vec2());
        let fits = galley.rows.iter().any(|row| !row.glyphs.is_empty())
            && clip.expand(1.0).contains_rect(ink);
        fits.then_some(Self { galley, pos })
    }
}

/// What the list needs to paint the flying message (`SendingInfoTo`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SendFlightRow {
    /// The message that flies.
    pub msg_id: MsgId,
    /// The layer the list paints it into.
    pub layer: LayerId,
    /// Opacity of the message (the cross-fade `mix`).
    pub opacity: f32,
    /// Filled by the list: where the message's text starts on screen this frame, `None` when
    /// it was not painted.
    pub text_origin: Option<Pos2>,
    /// Filled by the list: the message has no text bubble to fly into (media, a sticker or an
    /// emoji-only message: `MessageSendingTextFlight::Prepare` returns `v::null`).
    pub refused: bool,
}

/// One running send flight.
#[derive(Debug)]
pub struct SendFlight {
    peer_id: PeerId,
    msg_id: MsgId,
    source: SendingFieldText,
    flight: MessageSendingTextFlight,
    layer: LayerId,
}

impl SendFlight {
    /// Starts the flight of `source` into message `msg_id` of `peer_id`, or `None` when power
    /// saving turns chat effects off.
    #[must_use]
    pub fn start(
        peer_id: PeerId,
        msg_id: MsgId,
        source: SendingFieldText,
        parent: LayerId,
        now_sec: f64,
        policy: FlyEffectPolicy,
    ) -> Option<Self> {
        let flight = MessageSendingTextFlight::start(source.pos, now_sec, policy);
        if flight.finished(now_sec) {
            return None;
        }
        Some(Self {
            peer_id,
            msg_id,
            source,
            flight,
            layer: LayerId::new(parent.order, Id::unique(("tg_send_flight", peer_id.0))),
        })
    }

    /// The message that flies.
    #[must_use]
    pub const fn msg_id(&self) -> MsgId {
        self.msg_id
    }

    /// Follows the local message to its server id (`itemIdChanged`).
    pub fn follow_id(&mut self, local_id: MsgId, sent_id: MsgId) {
        if self.msg_id == local_id {
            self.msg_id = sent_id;
        }
    }

    /// The row the list paints this frame, or `None` when the flight belongs to another chat.
    #[must_use]
    pub fn row(&self, peer_id: PeerId, now_sec: f64) -> Option<SendFlightRow> {
        (self.peer_id == peer_id).then(|| SendFlightRow {
            msg_id: self.msg_id,
            layer: self.layer,
            opacity: self.flight.mix(now_sec),
            text_origin: None,
            refused: false,
        })
    }

    /// Draws the field text and moves the message's layer after the list painted `row`.
    /// Returns `false` when the flight is over.
    pub fn after_list(
        &mut self,
        ctx: &egui::Context,
        parent: LayerId,
        row: Option<SendFlightRow>,
        text_fg: egui::Color32,
        now_sec: f64,
    ) -> bool {
        let Some(row) = row.filter(|r| r.msg_id == self.msg_id && !r.refused) else {
            return false;
        };
        let Some(target) = row.text_origin else {
            // tdesktop finishes once the message has no place in the list.
            return false;
        };
        if self.flight.finished(now_sec) {
            return false;
        }
        let frame = self.flight.frame(now_sec, target);
        if frame.mix < 1.0 {
            // The field text moves with the message: drawn over the message's own text, so
            // the layer's offset puts it where it was in the field at the start.
            let mut painter = egui::Painter::new(ctx.clone(), self.layer, ctx.content_rect());
            painter.set_opacity(1.0 - frame.mix);
            painter.galley(target, Arc::clone(&self.source.galley), text_fg);
        }
        ctx.set_sublayer(parent, self.layer);
        ctx.transform_layer_shapes(self.layer, TSTransform::from_translation(frame.offset));
        ctx.request_repaint();
        true
    }
}

#[cfg(test)]
#[path = "tests_send_flight.rs"]
#[cfg(test)]
mod tests;
