//! The reaction buttons a message shows under its text (TGB-3017).
//!
//! Port of the ordering in `HistoryView::Reactions::InlineList::layoutButtons`
//! (`history/view/reactions/history_view_reactions.cpp:182-206`): the paid reaction first, then
//! by count without the user's own reaction, descending.
//!
//! Internal design (differs from tdesktop): ties keep the server's order instead of the index in
//! `Data::Reactions::list(Type::All)`; the session does not hold that catalog, and the server
//! already lists reactions by popularity.

use tg_app_core::history::backend::ReactionCounter;
use tg_data::{MessageReactionsSummary, ReactionId};

/// UI key of a reaction (`"👍"`, `"custom:<document id>"`, `"⭐"` for the paid reaction).
#[must_use]
pub fn reaction_key(id: &ReactionId) -> String {
    match id {
        ReactionId::Emoji(e) => e.clone(),
        ReactionId::Custom(doc) => format!("custom:{}", doc.0),
        ReactionId::Paid => "⭐".to_owned(),
    }
}

/// Buttons for `summary` in tdesktop's order.
#[must_use]
pub fn reaction_counters(summary: &MessageReactionsSummary) -> Vec<ReactionCounter> {
    let mut sorted: Vec<_> = summary.results.iter().collect();
    sorted.sort_by_key(|r| {
        let paid = matches!(r.id, ReactionId::Paid);
        let without_mine = i64::from(r.count) - i64::from(r.is_chosen());
        (!paid, std::cmp::Reverse(without_mine))
    });
    sorted
        .into_iter()
        .map(|r| ReactionCounter {
            emoji: reaction_key(&r.id),
            count: r.count.max(0) as u32,
            chosen: r.is_chosen(),
        })
        .collect()
}

/// Custom emoji document ids among a message's reaction buttons, for the emoji loader.
pub fn custom_emoji_ids(reactions: &[ReactionCounter]) -> impl Iterator<Item = u64> + '_ {
    reactions.iter().filter_map(|r| {
        r.emoji
            .strip_prefix("custom:")
            .and_then(|id| id.parse::<u64>().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_data::{DocumentId, MessageReactionCount};

    fn count(id: ReactionId, count: i32, mine: bool) -> MessageReactionCount {
        MessageReactionCount {
            id,
            count,
            chosen_order: mine.then_some(0),
        }
    }

    #[test]
    fn paid_goes_first_then_count_without_mine() {
        let summary = MessageReactionsSummary {
            results: vec![
                count(ReactionId::Emoji("👍".into()), 5, false),
                count(
                    ReactionId::Custom(DocumentId(5_199_576_980_080_601_299)),
                    6,
                    true,
                ),
                count(ReactionId::Emoji("🔥".into()), 9, false),
                count(ReactionId::Paid, 2, false),
            ],
            ..MessageReactionsSummary::default()
        };
        let keys: Vec<String> = reaction_counters(&summary)
            .into_iter()
            .map(|r| r.emoji)
            .collect();
        assert_eq!(
            keys,
            ["⭐", "🔥", "👍", "custom:5199576980080601299"],
            "6 with mine counts as 5 and keeps the server order after the other 5"
        );
    }

    #[test]
    fn custom_ids_are_collected() {
        let summary = MessageReactionsSummary {
            results: vec![
                count(ReactionId::Custom(DocumentId(42)), 1, false),
                count(ReactionId::Emoji("👍".into()), 1, false),
                count(ReactionId::Paid, 1, false),
            ],
            ..MessageReactionsSummary::default()
        };
        let counters = reaction_counters(&summary);
        assert_eq!(custom_emoji_ids(&counters).collect::<Vec<_>>(), [42]);
    }
}
