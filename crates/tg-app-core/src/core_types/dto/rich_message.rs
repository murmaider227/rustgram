//! Pure [`RichMessagePayload`] DTO and TL conversion helper (`webview/rich_page_view.rs` model).

use tg_iv::rich_page::{
    RichPage, RichParseMode, flatten_rich_page_summary, rich_page_from_tl_rich_message,
};
use tg_tl::{enums as tl_enums, types as tl_types};

/// Rich document payload attached to a [`crate::history::backend::HistoryMessage`].
///
/// Stores both the parsed editable [`RichPage`] and (when received from or converted for Layer-233
/// TL) the wire [`tl_types::RichMessage`] so editing can round-trip through
/// [`tg_iv::rich_page::rich_page_from_tl_rich_message`].
#[derive(Debug, Clone)]
pub struct RichMessagePayload {
    /// Parsed rich document model for bubble rendering and editing.
    pub page: RichPage,
    /// Optional Layer-233 TL `RichMessage` wire representation.
    pub tl_rich_message: Option<tl_types::RichMessage>,
}

impl PartialEq for RichMessagePayload {
    fn eq(&self, other: &Self) -> bool {
        self.page == other.page && self.tl_rich_message == other.tl_rich_message
    }
}

impl Eq for RichMessagePayload {}

impl RichMessagePayload {
    /// Builds a payload directly from a received Layer-233 TL [`tl_types::RichMessage`].
    #[must_use]
    pub fn from_tl(msg: tl_types::RichMessage) -> Self {
        let page = rich_page_from_tl_rich_message(&msg, RichParseMode::Normal);
        Self {
            page,
            tl_rich_message: Some(msg),
        }
    }

    /// Builds a payload from a local [`RichPage`] and its serialized [`tl_types::InputRichMessage`].
    ///
    /// Synthesizes a matching [`tl_types::RichMessage`] from the serialized blocks so subsequent
    /// "Edit" actions can decode through [`rich_page_from_tl_rich_message`].
    #[must_use]
    pub fn from_page_and_input(page: RichPage, input: &tl_types::InputRichMessage) -> Self {
        let tl_msg = tl_types::RichMessage {
            rtl: input.rtl,
            part: false,
            blocks: input.blocks.clone(),
            photos: Vec::new(),
            documents: Vec::new(),
        };
        Self {
            page,
            tl_rich_message: Some(tl_msg),
        }
    }

    /// Builds a payload from a [`RichPage`] alone (no wire TL handle attached).
    #[must_use]
    pub fn from_page(page: RichPage) -> Self {
        Self {
            page,
            tl_rich_message: None,
        }
    }

    /// Returns the [`RichPage`] ready for editing, preferring [`rich_page_from_tl_rich_message`]
    /// when wire TL data is attached.
    #[must_use]
    pub fn to_editable_page(&self) -> RichPage {
        if let Some(ref tl) = self.tl_rich_message {
            let parsed = rich_page_from_tl_rich_message(tl, RichParseMode::Normal);
            if !parsed.blocks.is_empty() {
                return parsed;
            }
        }
        self.page.clone()
    }

    /// Returns a concise plain-text summary suitable for reply previews and dialog snippets.
    #[must_use]
    pub fn summary_text(&self) -> String {
        flatten_rich_page_summary(&self.page, true).text
    }
}

/// Extracts a [`RichMessagePayload`] from a TL [`tl_enums::RichMessage`] if present.
#[must_use]
pub fn payload_from_tl_enum(rm: &tl_enums::RichMessage) -> RichMessagePayload {
    let tl_enums::RichMessage::RichMessage(inner) = rm;
    RichMessagePayload::from_tl(inner.clone())
}
