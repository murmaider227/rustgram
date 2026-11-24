//! Pure welcome-messages DTOs (`boxes/welcome_messages.rs` model).

use crate::stand_in::types::PeerId;

/// Default maximum number of welcome message templates per group
/// (`ephemeral_welcome_messages_max`, `welcome_messages.cpp:49-53`).
pub const WELCOME_MESSAGES_DEFAULT_LIMIT: usize = 5;

/// Single ephemeral welcome message template (`ephemeralMessage#dd27bee9`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WelcomeMessageItem {
    /// Ephemeral template message ID (`ephemeralMessage.id`).
    pub id: i32,
    /// Owning group/channel peer ID.
    pub peer_id: PeerId,
    /// Author peer ID (`from_id`).
    pub from_id: Option<PeerId>,
    /// Unix timestamp (`date`).
    pub date: i32,
    /// Message text body.
    pub text: String,
    /// True if outgoing (`out`).
    pub out: bool,
    /// True if marked as welcome template (`welcome_template`).
    pub welcome_template: bool,
    /// True if media is placed above text (`invert_media`).
    pub invert_media: bool,
}

/// High-level events emitted by [`WelcomeMessagesSectionWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WelcomeMessagesAction {
    /// Navigate back to the parent chat history.
    Back,
    /// A welcome message template was created, edited, or deleted.
    Changed,
    /// Show a transient toast message.
    Toast(String),
}
