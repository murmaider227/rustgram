//! What a painted message keeps between frames, next to its cached layout.
//!
//! The layout cache held geometry only, so every frame converted each visible message's
//! entities to spans, built its layout job and recomputed its link rectangles. A
//! [`MessagePaintCache`] lives in the same cache entry as the layout (dropped with it when the
//! message is edited or the width changes) and holds the text prepared once
//! (`Ui::Text::String` in tdesktop: prepare once, draw many times).
//!
//! Each kept value is checked against the text it was made from before it is reused, so a
//! message changed without its layout being invalidated can never show old text.

use tg_text::{EntityType, TextWithEntities};
use tg_ui::text::{PreparedEntityText, PreparedRichText, note_text_preparation};

use crate::history::text_bridge::entities_to_rich_text;

/// Whether `text` needs the block renderer (`<pre>`, quotes, custom emoji, coloured code);
/// other texts are one galley of spans.
#[must_use]
pub(crate) fn uses_rich_entity_renderer(text: &TextWithEntities) -> bool {
    text.entities.iter().any(|e| {
        matches!(
            e.entity_type(),
            EntityType::Pre
                | EntityType::Blockquote
                | EntityType::CustomEmoji
                | EntityType::Colorized
        )
    })
}

/// A message body prepared for painting.
///
/// The variants differ in size, but the enum only ever lives in the `Box` of
/// [`MessagePaintCache`], so neither is boxed again.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum PreparedBody {
    /// One galley of styled spans (`render_rich_text`).
    Spans {
        /// The text the spans were converted from.
        source: TextWithEntities,
        text: PreparedRichText,
    },
    /// Blocks laid out by the entity renderer (`render_text_with_entities`).
    Blocks(PreparedEntityText),
}

impl PreparedBody {
    fn new(text: &TextWithEntities) -> Self {
        if uses_rich_entity_renderer(text) {
            Self::Blocks(PreparedEntityText::new(text.clone()))
        } else {
            note_text_preparation();
            Self::Spans {
                source: text.clone(),
                text: PreparedRichText::new(entities_to_rich_text(text)),
            }
        }
    }

    fn is_for(&self, text: &TextWithEntities) -> bool {
        match self {
            Self::Spans { source, .. } => source == text,
            Self::Blocks(prepared) => prepared.source() == text,
        }
    }
}

/// Per-message paint state kept between frames.
#[derive(Debug, Default)]
pub struct MessagePaintCache {
    /// Boxed: most cached layouts are of messages that are not being painted.
    body: Option<Box<PreparedBody>>,
    /// `(text, its lowercase form)` for the search tint.
    lowercase: Option<(String, String)>,
}

impl MessagePaintCache {
    /// Whether anything is kept for this message (tests).
    #[cfg(test)]
    pub(crate) const fn holds_text(&self) -> bool {
        self.body.is_some() || self.lowercase.is_some()
    }

    /// The body prepared for `text`: the kept one while the text is the same, a new one after
    /// it changed (an edit, a summary shown instead of the message).
    pub(crate) fn body(&mut self, text: &TextWithEntities) -> &mut PreparedBody {
        if !self.body.as_ref().is_some_and(|body| body.is_for(text)) {
            self.body = None;
        }
        self.body
            .get_or_insert_with(|| Box::new(PreparedBody::new(text)))
    }

    /// Whether `text` contains `needle_lowercase` ignoring case (`needle_lowercase` is already
    /// lowercase). The lowercase form of the text is made once, not on every frame.
    pub(crate) fn contains_lowercase(&mut self, text: &str, needle_lowercase: &str) -> bool {
        if !self.lowercase.as_ref().is_some_and(|(of, _)| of == text) {
            self.lowercase = Some((text.to_owned(), text.to_lowercase()));
        }
        self.lowercase
            .as_ref()
            .is_some_and(|(_, lower)| lower.contains(needle_lowercase))
    }
}
