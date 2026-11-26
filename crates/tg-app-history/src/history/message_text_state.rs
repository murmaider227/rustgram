//! What the reader changed in a message's text, and measuring the text the way it is painted.
//!
//! tdesktop keeps both in the message's `Ui::Text::String`: whether its spoilers were revealed
//! (`setSpoilerRevealed`, hidden again for every message when the chat changes,
//! `Data::Session::hideShownSpoilers`) and which collapsible quotes are expanded. Here the
//! history widget keeps them per message of the open chat ([`MessageTextState`]), so they do
//! not live in egui memory under a bare message id (ids repeat across channels) and the bubble
//! can be measured with the quotes as they are shown.

use egui::{Color32, Ui};
use tg_text::{EntityType, TextWithEntities, utf16_len};
use tg_ui::stand_in::TextPalette;
use tg_ui::text::{RichTextOptions, layout_text_with_entities_expanded, spoiler_reveal_progress};

use crate::history::paint_cache::uses_rich_entity_renderer;
use crate::history::text_bridge::{measure_rich_text, message_text_style};

/// The reader's state of one message's text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageTextState {
    /// When the spoilers were revealed (`ui.input` time); `None` while they are hidden.
    pub spoilers_revealed_at: Option<f64>,
    /// Expanded collapsible quotes: block indices of the message text.
    pub expanded_quotes: Vec<usize>,
}

impl MessageTextState {
    /// How far the spoilers have faded in at `now` (`0` hidden, `1` revealed).
    #[must_use]
    pub fn spoiler_reveal(&self, now: f64) -> f32 {
        self.spoilers_revealed_at
            .map_or(0.0, |at| spoiler_reveal_progress(now - at))
    }

    /// Expands a collapsed quote or collapses an expanded one.
    pub fn toggle_quote(&mut self, block: usize) {
        if let Some(index) = self.expanded_quotes.iter().position(|b| *b == block) {
            self.expanded_quotes.remove(index);
        } else {
            self.expanded_quotes.push(block);
            self.expanded_quotes.sort_unstable();
        }
    }
}

/// Options the message body is laid out with (`paint_message_element` paints with the same
/// width and quote styles, so the measured height is the painted one).
#[must_use]
pub(crate) fn body_layout_options(
    base_color: Color32,
    wrap_width: f32,
    expanded_quotes: &[usize],
) -> RichTextOptions {
    let mut options = RichTextOptions::new(base_color, wrap_width.max(40.0));
    options.expanded_blockquotes = Some(expanded_quotes.to_vec());
    options
}

/// `(single_line_width, wrapped_height, longest_wrapped_line_width)` of a message text as it
/// is painted: texts with quotes, code blocks or custom emoji are laid out by the block
/// renderer (a collapsed quote shows three lines, an expanded one all of them, with the quote
/// paddings); other texts as one galley of spans ([`measure_rich_text`]).
#[must_use]
pub fn measure_message_text(
    ui: &Ui,
    source: &TextWithEntities,
    palette: &TextPalette,
    base_color: Color32,
    max_wrap_width: f32,
    expanded_quotes: &[usize],
) -> (i32, i32, i32) {
    if source.text.is_empty() {
        return (0, 0, 0);
    }
    if !uses_rich_entity_renderer(source) {
        return measure_rich_text(ui, source, palette, base_color, max_wrap_width);
    }
    let style = message_text_style();
    let wrapped = layout_text_with_entities_expanded(
        ui.ctx(),
        source,
        &style,
        palette,
        &body_layout_options(base_color, max_wrap_width, expanded_quotes),
        expanded_quotes,
    );
    let single = layout_text_with_entities_expanded(
        ui.ctx(),
        source,
        &style,
        palette,
        &body_layout_options(base_color, 100_000.0, expanded_quotes),
        expanded_quotes,
    );
    (
        single.size.x.ceil() as i32,
        wrapped.size.y.ceil() as i32,
        wrapped.size.x.ceil() as i32,
    )
}

/// Whether the text ends with a quote or a code block: tdesktop then puts the time on a line
/// of its own below it (`String::updateSkipBlock`, `_endsWithQuoteOrOtherDirection`).
#[must_use]
pub fn ends_with_quote(text: &TextWithEntities) -> bool {
    let end = utf16_len(text.text.trim_end()) as i32;
    end > 0
        && text.entities.iter().any(|e| {
            matches!(e.entity_type(), EntityType::Blockquote | EntityType::Pre)
                && e.offset() + e.length() >= end
        })
}
