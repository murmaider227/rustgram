//! Pure rich-compose DTOs (`MentionCandidate`, `CustomEmojiPreset`, `RichThreadDraft`).

use tg_text::TextWithEntities;

use crate::history::backend::ThreadDraft;
use crate::history::sections_model::ReplyQuote;
use crate::stand_in::types::MsgId;

/// Chat participant entry offered by the `@mention` autocomplete popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionCandidate {
    /// Telegram bare user ID.
    pub user_id: u64,
    /// Telegram user access hash (for `MentionName` entity serialization).
    pub access_hash: u64,
    /// Full display name (e.g. `"Sam Chen"`).
    pub display_name: String,
    /// Optional `@username` without leading `@`.
    pub username: Option<String>,
    /// Avatar circle RGB color.
    pub avatar_rgb: [u8; 3],
}

/// Built-in custom emoji entry offered by the compose custom-emoji bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomEmojiPreset {
    /// Telegram custom emoji `document_id`.
    pub document_id: u64,
    /// Fallback Unicode emoji glyph.
    pub emoji: &'static str,
    /// Friendly name.
    pub name: &'static str,
}

/// Sample custom emoji catalog for the compose custom-emoji quick picker.
pub const CUSTOM_EMOJI_PRESETS: &[CustomEmojiPreset] = &[
    CustomEmojiPreset {
        document_id: 5368324170671202286,
        emoji: "✨",
        name: "Sparkles",
    },
    CustomEmojiPreset {
        document_id: 5368324170671202287,
        emoji: "🚀",
        name: "Launch",
    },
    CustomEmojiPreset {
        document_id: 5368324170671202288,
        emoji: "🔥",
        name: "Flame",
    },
    CustomEmojiPreset {
        document_id: 5368324170671202289,
        emoji: "🎉",
        name: "Party",
    },
    CustomEmojiPreset {
        document_id: 5368324170671202290,
        emoji: "⚡",
        name: "Bolt",
    },
];

/// Draft payload preserving full `tg-text` UTF-16 entities alongside reply/quote/webpage state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RichThreadDraft {
    /// Draft text and UTF-16 entities.
    pub text: TextWithEntities,
    /// Optional reply target message id.
    pub reply_to: Option<MsgId>,
    /// Optional quoted excerpt within `reply_to`.
    pub reply_quote: Option<ReplyQuote>,
    /// True if link preview was disabled for this draft.
    pub no_webpage: bool,
}

impl RichThreadDraft {
    /// Converts into a legacy plain-text [`ThreadDraft`].
    #[must_use]
    pub fn to_legacy(&self) -> ThreadDraft {
        ThreadDraft {
            text: self.text.text.clone(),
            reply_to: self.reply_to,
            reply_quote: self.reply_quote.clone(),
            no_webpage: self.no_webpage,
        }
    }

    /// Creates a [`RichThreadDraft`] from a legacy [`ThreadDraft`] (with empty entities).
    #[must_use]
    pub fn from_legacy(d: ThreadDraft) -> Self {
        Self {
            text: TextWithEntities::simple(d.text),
            reply_to: d.reply_to,
            reply_quote: d.reply_quote,
            no_webpage: d.no_webpage,
        }
    }
}
