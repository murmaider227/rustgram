//! Peer accents for sender and reply names (TGB-3012), and the server colour sets they index.
//!
//! Port of `PeerData::colorIndex` / `colorCollectible` as read by `Element::colorIndex`
//! (`history_view_element.cpp`) and `Reply::_colorPeer` (`history_view_reply.cpp`), and of
//! `Api::PeerColors::apply` (`api_peer_colors.cpp`) feeding `ChatStyle::setColorIndices`.

use std::time::{SystemTime, UNIX_EPOCH};

use tg_app_core::themes::backend::PeerColorOptionRecord;
use tg_data::{Message, PeerId as DataPeerId, Session};
use tg_theme::Color;
use tg_ui::peer_color::{
    CollectibleAccent, ColorIndexData, ColorIndices, ColorSet, PeerAccent, set_color_indices,
};

/// The accent of `peer`: its server colour index and collectible colour, or the id-based
/// default while the peer is unknown (`Data::DecideColorIndex`).
#[must_use]
pub fn peer_accent(session: &Session, peer: DataPeerId) -> PeerAccent {
    session.peer(peer).map_or_else(
        || PeerAccent::for_peer_id(peer.bare()),
        |p| {
            let common = p.common();
            PeerAccent {
                index: common.color_index,
                collectible: common.color_collectible.map(|c| CollectibleAccent {
                    day: c.for_theme(false),
                    night: c.for_theme(true),
                }),
            }
        },
    )
}

/// The peer whose colours a message's name and the replies to it use
/// (`HistoryItem::contentColorsFrom`): the original sender of a forward, else the sender.
#[must_use]
pub fn content_colors_from(msg: &Message) -> DataPeerId {
    msg.forward_from
        .as_ref()
        .and_then(|f| f.from_id)
        .unwrap_or_else(|| msg.from_id.unwrap_or(msg.peer_id))
}

/// Installs the colour sets of `help.peerColors` for painting (`PeerColors::apply`), and
/// returns `true` when they changed.
pub fn apply_color_sets(options: &[PeerColorOptionRecord]) -> bool {
    set_color_indices(color_indices_from(options))
}

/// The colour sets of `help.peerColors` by colour index (`PeerColors::apply`): an option
/// without light colours leaves its index empty, missing dark colours repeat the light ones.
#[must_use]
pub fn color_indices_from(options: &[PeerColorOptionRecord]) -> ColorIndices {
    let mut table: ColorIndices =
        [ColorIndexData::default(); tg_ui::userpic::COLOR_INDEX_COUNT as usize];
    for option in options {
        let Some(slot) = table.get_mut(usize::from(option.color_id)) else {
            continue;
        };
        let light = color_set(&option.light_colors);
        if light[0].is_none() {
            continue;
        }
        let dark = color_set(&option.dark_colors);
        *slot = ColorIndexData {
            light,
            dark: if dark[0].is_some() { dark } else { light },
        };
    }
    table
}

impl crate::session::LiveSession {
    /// Asks `help.getPeerColors` at session start (`Api::PeerColors::request`), at most once an
    /// hour; the answer goes to [`apply_color_sets`] and repaints.
    pub fn request_peer_color_sets(&self) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        if let Err(err) = self.refresh_peer_colors_if_due(now_ms, false) {
            tracing::debug!(error = %err, "help.getPeerColors failed");
        }
    }
}

fn rgb_of(c: &Color) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

fn color_set(colors: &[Color]) -> ColorSet {
    let mut set: ColorSet = [None; 3];
    // `parseColors`: an empty list or more than three colours is a bad set.
    if colors.is_empty() || colors.len() > set.len() {
        return set;
    }
    for (out, c) in set.iter_mut().zip(colors) {
        *out = Some(rgb_of(c));
    }
    set
}

#[cfg(test)]
#[path = "tests_peer_accent.rs"]
#[cfg(test)]
mod tests;
