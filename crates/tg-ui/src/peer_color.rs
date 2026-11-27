//! Peer accent colours for sender names: the server colour index, the colour sets the server
//! sends for indexes past the seven palette colours, and collectible colours.
//!
//! Port of `Ui::FromNameFg`, `ChatStyle::computeColorIndexValues`,
//! `ChatStyle::collectibleNameColor` (`ui/chat/chat_style.cpp`) and the colour sets of
//! `Api::PeerColors::apply` (`api/api_peer_colors.cpp`).
//!
//! tdesktop keeps the colour sets in every `ChatStyle` (`setColorIndices`). Here they live in
//! one process-wide table, replaced whole when `help.getPeerColors` answers; painting reads it
//! under a shared lock and never copies it.

use std::sync::{Arc, PoisonError, RwLock};

use egui::Color32;

use crate::theme::Colors;
use crate::userpic::{COLOR_INDEX_COUNT, SIMPLE_COLOR_INDEX_COUNT, decide_color_index};

/// Name accent of a collectible peer colour, as `0xRRGGBB` (`Ui::ColorCollectible`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CollectibleAccent {
    /// `accentColor`, used by day themes.
    pub day: u32,
    /// `darkAccentColor` (or `accentColor` when the server sent none), used by night themes.
    pub night: u32,
}

/// A peer's accent as the server describes it (`PeerData::colorIndex` and
/// `PeerData::colorCollectible`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PeerAccent {
    /// Colour index `0..64`: the server's `peerColor.color`, or `id % 7` without one.
    pub index: u8,
    /// Collectible colour; when set, names use its accent instead of `index`.
    pub collectible: Option<CollectibleAccent>,
}

impl PeerAccent {
    /// The default accent of a peer the server sent no colour for (`Data::DecideColorIndex`).
    #[must_use]
    pub const fn for_peer_id(bare_peer_id: u64) -> Self {
        Self {
            index: decide_color_index(bare_peer_id),
            collectible: None,
        }
    }
}

/// Up to three colours of one colour index for one theme kind, as `0xRRGGBB`
/// (`help.peerColorSet.colors`). The first is the name and outline colour.
pub type ColorSet = [Option<u32>; 3];

/// Light and dark colour sets of one colour index (`Ui::ColorIndexData`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorIndexData {
    /// `colors` of `help.peerColorOption`.
    pub light: ColorSet,
    /// `dark_colors`, or `colors` when the server sent none.
    pub dark: ColorSet,
}

/// The colour sets of every colour index (`Ui::ColorIndicesCompressed::colors`).
pub type ColorIndices = [ColorIndexData; COLOR_INDEX_COUNT as usize];

static COLOR_INDICES: RwLock<Option<Arc<ColorIndices>>> = RwLock::new(None);

/// Replaces the colour sets from `help.getPeerColors` (`PeerColors::apply`). Returns `true` when
/// they changed, so the caller repaints.
pub fn set_color_indices(indices: ColorIndices) -> bool {
    let mut slot = COLOR_INDICES
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    if slot.as_deref() == Some(&indices) {
        return false;
    }
    *slot = Some(Arc::new(indices));
    true
}

/// Forgets the colour sets (log out), so indexes past 6 fall back to the palette.
pub fn clear_color_indices() {
    *COLOR_INDICES
        .write()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

/// The colour sets of `index`, if the server sent them.
#[must_use]
pub fn color_index_data(index: u8) -> Option<ColorIndexData> {
    let slot = COLOR_INDICES.read().unwrap_or_else(PoisonError::into_inner);
    slot.as_deref()
        .and_then(|table| table.get(usize::from(index)).copied())
}

/// `rgb` (`0xRRGGBB`) as an opaque colour (`Ui::ColorFromSerialized`).
#[must_use]
pub const fn color_from_rgb(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The sender name colour for `accent` (`Ui::FromNameFg` for an unselected message).
///
/// A collectible colour gives its accent. An index past the seven palette colours gives the
/// first colour of its server colour set; without one it falls back to `index % 7`, as
/// `ChatStyle::computeColorIndexValues` does. The seven palette colours come from the active
/// theme (`historyPeerNNameFg`), so `colors` must resolve those names for the current theme.
#[must_use]
pub fn peer_accent_name_color(accent: PeerAccent, colors: &dyn Colors, dark: bool) -> Color32 {
    if let Some(collectible) = accent.collectible {
        return color_from_rgb(if dark {
            collectible.night
        } else {
            collectible.day
        });
    }
    let index = accent.index % COLOR_INDEX_COUNT;
    if index >= SIMPLE_COLOR_INDEX_COUNT
        && let Some(data) = color_index_data(index)
        && let Some(first) = (if dark { data.dark } else { data.light })[0]
    {
        return color_from_rgb(first);
    }
    crate::userpic::peer_name_color(index, colors)
}

/// The sender name colour for `accent` in a selected message (`Ui::FromNameFg` with
/// `selected`): `historyPeerNNameFgSelected` for the seven palette colours, otherwise the
/// unselected colour made readable on `selected_bg` (`msgInBgSelected`,
/// `Ui::EnsurePeerNameReadable`).
#[must_use]
pub fn peer_accent_name_color_selected(
    accent: PeerAccent,
    colors: &dyn Colors,
    dark: bool,
    selected_bg: Color32,
) -> Color32 {
    let index = accent.index % COLOR_INDEX_COUNT;
    let extended = accent.collectible.is_some()
        || (index >= SIMPLE_COLOR_INDEX_COUNT
            && color_index_data(index)
                .is_some_and(|d| (if dark { d.dark } else { d.light })[0].is_some()));
    if !extended {
        return crate::userpic::peer_name_color_selected(index, colors);
    }
    let base = peer_accent_name_color(accent, colors, dark);
    let readable = tg_theme::palette::ensure_peer_name_readable(
        tg_theme::Color::rgb(base.r(), base.g(), base.b()),
        tg_theme::Color::rgb(selected_bg.r(), selected_bg.g(), selected_bg.b()),
        dark,
    );
    Color32::from_rgb(readable.r, readable.g, readable.b)
}

#[cfg(test)]
#[path = "peer_color_tests.rs"]
#[cfg(test)]
mod tests;
