//! Section mementos, navigation parameters, and slide animation state.
//!
//! Port origin: `Telegram/SourceFiles/window/section_memento.h`,
//! `Telegram/SourceFiles/window/section_widget.h`,
//! `Telegram/SourceFiles/window/section_widget.cpp`,
//! `Telegram/SourceFiles/window/window_slide_animation.h`,
//! and `Telegram/SourceFiles/window/window_slide_animation.cpp`.

use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_info::statistics::StatisticsScreen;

pub use tg_app_core::core_types::window_types::{
    InfoTab, SLIDE_DURATION_MS, SectionShow, SectionWay, SettingsPage, SlideAnimation,
    SlideDirection,
};

/// Serializable/storable state snapshot of a section (`Window::SectionMemento`).
///
/// Mirrors `SectionMemento` subclasses (`ChatMemento`, `Info::Memento`, `Settings::Memento`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionMemento {
    /// Peer chat history view.
    PeerHistory {
        /// Target peer id (`PeerId::ZERO` clears to empty state).
        peer_id: PeerId,
        /// Target message id to scroll to / highlight.
        msg_id: MsgId,
    },
    /// Peer profile / info view (hosted in third column when wide, or main column when narrow).
    PeerInfo {
        /// Target peer id.
        peer_id: PeerId,
        /// Selected info tab.
        tab: InfoTab,
    },
    /// Application settings section.
    Settings {
        /// Active settings category page.
        page: SettingsPage,
    },
    /// Channel, supergroup, boosts, or monetization statistics section.
    Statistics {
        /// Target peer id (channel, supergroup, or bot).
        peer_id: PeerId,
        /// Initial statistics screen to display.
        screen: StatisticsScreen,
    },
}

impl SectionMemento {
    /// Returns the peer associated with this memento, if any.
    #[must_use]
    pub const fn peer_id(&self) -> Option<PeerId> {
        match *self {
            Self::PeerHistory { peer_id, .. } if !peer_id.is_zero() => Some(peer_id),
            Self::PeerInfo { peer_id, .. } if !peer_id.is_zero() => Some(peer_id),
            Self::Statistics { peer_id, .. } if !peer_id.is_zero() => Some(peer_id),
            _ => None,
        }
    }

    /// Returns `true` if `self` and `other` belong to the same section category
    /// (`SectionWidget::sameTypeAs`).
    #[must_use]
    pub const fn same_type_as(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::PeerHistory { .. }, Self::PeerHistory { .. })
                | (Self::PeerInfo { .. }, Self::PeerInfo { .. })
                | (Self::Settings { .. }, Self::Settings { .. })
                | (Self::Statistics { .. }, Self::Statistics { .. })
        )
    }
}

/// Entry stored on the `MainWidget` / `SessionController` back stack (`StackItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackItem {
    /// Main column section snapshot.
    pub memento: SectionMemento,
    /// Optional third-column section snapshot associated with this stack level.
    pub third_section: Option<SectionMemento>,
}
