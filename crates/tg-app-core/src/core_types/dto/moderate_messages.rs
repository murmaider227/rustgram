//! Pure bulk-moderation DTOs (`boxes/moderate_messages.rs` model).

use crate::stand_in::types::PeerId;

/// Ban / temporary restriction duration presets for [`ModerateMessagesBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestrictDurationPreset {
    /// Permanent ban (`until_date = 0` / `RESTRICT_UNTIL_FOREVER`).
    #[default]
    Forever,
    /// Restrict for 1 day (`86_400` s).
    OneDay,
    /// Restrict for 1 week (`604_800` s).
    OneWeek,
    /// Restrict for 1 month (`2_592_000` s).
    OneMonth,
}

impl RestrictDurationPreset {
    /// All restriction duration presets in display order.
    pub const ALL: [Self; 4] = [Self::Forever, Self::OneDay, Self::OneWeek, Self::OneMonth];

    /// Human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Forever => "Ban forever",
            Self::OneDay => "Restrict 1 day",
            Self::OneWeek => "Restrict 1 week",
            Self::OneMonth => "Restrict 1 month",
        }
    }

    /// Returns restriction duration in seconds (`None` = permanent ban).
    #[must_use]
    pub const fn duration_secs(self) -> Option<i32> {
        match self {
            Self::Forever => None,
            Self::OneDay => Some(86_400),
            Self::OneWeek => Some(604_800),
            Self::OneMonth => Some(2_592_000),
        }
    }
}

/// Author entry participating in a moderation selection (`ModerateOptions::participants`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerateParticipant {
    /// Participant peer ID.
    pub peer_id: PeerId,
    /// Participant display name.
    pub name: String,
    /// Optional username without `@`.
    pub username: Option<String>,
    /// Selected message IDs authored by this participant.
    pub selected_msg_ids: Vec<i32>,
    /// Optional total messages authored by this participant in the group
    /// (used for dynamic title count like `makeMessageTitleData`).
    pub total_messages_count: Option<u32>,
    /// Whether this participant can be banned/restricted by the current admin.
    pub can_ban: bool,
    /// Whether all messages from this participant can be deleted.
    pub can_delete_all: bool,
}

/// Payload executed by [`InfoBackend::moderate_messages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerateMessagesRequest {
    /// Group / supergroup peer ID.
    pub peer_id: PeerId,
    /// Selected message IDs to delete (`channels.deleteMessages`).
    pub message_ids: Vec<i32>,
    /// Participants to report for spam (`channels.reportSpam`).
    pub report_spam_peers: Vec<PeerId>,
    /// Participants whose entire group history should be deleted (`channels.deleteParticipantHistory`).
    pub delete_all_from_peers: Vec<PeerId>,
    /// Participants whose reactions should be removed.
    pub delete_all_reactions_peers: Vec<PeerId>,
    /// Participants to ban or restrict (`channels.editBanned`).
    pub ban_peers: Vec<PeerId>,
    /// `None` = permanent kick/ban (`view_messages`), `Some(secs)` = temporary send restriction.
    pub restrict_duration_secs: Option<i32>,
    /// Optional community peer ID to also ban participants from (`communities.toggleParticipantBanned`).
    pub ban_in_community_id: Option<PeerId>,
}

/// Execution summary returned by [`InfoBackend::moderate_messages`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModerateMessagesResult {
    /// Count of explicitly selected messages deleted.
    pub deleted_selected: usize,
    /// Number of participants reported for spam.
    pub reported_peers: usize,
    /// Number of participants whose full history was purged.
    pub deleted_all_peers: usize,
    /// Number of participants banned or restricted.
    pub banned_peers: usize,
    /// Number of participants banned from the linked community.
    pub community_banned_peers: usize,
}
