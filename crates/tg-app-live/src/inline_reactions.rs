//! The reaction buttons a message shows under its text (TGB-3017).
//!
//! Port of the ordering in `HistoryView::Reactions::InlineList::layoutButtons`
//! (`history/view/reactions/history_view_reactions.cpp:182-206`): the paid reaction first, then
//! by count without the user's own reaction, descending.
//!
//! Internal design (differs from tdesktop): ties keep the server's order instead of the index in
//! `Data::Reactions::list(Type::All)`; the session does not hold that catalog, and the server
//! already lists reactions by popularity.

use tg_app_core::history::backend::{ReactionCounter, RecentReactors};
use tg_data::{Message, MessageReactionsSummary, PeerId as DataPeerId, ReactionId, Session};

/// `kMaxRecentUserpics`: more reactions than this in total show counts.
const MAX_RECENT_USERPICS: i64 = 3;

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

/// The reactors whose userpics replace the counts of a group or channel message's reaction
/// buttons (`InlineListDataFromMessage`, `history_view_reactions.cpp:952-977`): only when the
/// server listed every reactor of every reaction and there are at most
/// [`MAX_RECENT_USERPICS`] reactions in total. Otherwise empty: the buttons show counts.
///
/// Private chats are not handled here: there tdesktop always shows the chat partner and the
/// user, which the history view builds from the peer it shows.
#[must_use]
pub fn recent_userpics(summary: &MessageReactionsSummary) -> Vec<(ReactionId, Vec<DataPeerId>)> {
    // `MessageReactions::change` (`data_message_reactions.cpp:2232-2252`): a reactor of a
    // reaction the message does not list is dropped, and a list holds at most the count.
    let mut recent: Vec<(ReactionId, Vec<DataPeerId>)> = Vec::new();
    for reactor in &summary.recent_reactors {
        let Some(count) = summary
            .results
            .iter()
            .find(|r| r.id == reactor.reaction)
            .map(|r| r.count)
        else {
            continue;
        };
        match recent.iter_mut().find(|(id, _)| *id == reactor.reaction) {
            Some((_, peers)) if (peers.len() as i64) < i64::from(count) => {
                peers.push(reactor.peer_id);
            }
            Some(_) => {}
            None if count > 0 => recent.push((reactor.reaction.clone(), vec![reactor.peer_id])),
            None => {}
        }
    }
    if recent.len() != summary.results.len() {
        return Vec::new();
    }
    let mut sum = 0i64;
    for reaction in &summary.results {
        sum += i64::from(reaction.count);
        if sum > MAX_RECENT_USERPICS {
            return Vec::new();
        }
        let listed = recent
            .iter()
            .find(|(id, _)| *id == reaction.id)
            .map_or(0, |(_, peers)| peers.len());
        if i64::from(reaction.count) != listed as i64 {
            return Vec::new();
        }
    }
    recent
}

/// [`recent_userpics`] of `msg` with the reactors' display names, for `MessageExtra`.
#[must_use]
pub fn recent_reactors_for(session: &Session, msg: &Message) -> Vec<RecentReactors> {
    let Some(summary) = msg.reactions.as_ref() else {
        return Vec::new();
    };
    if msg.peer_id.is_user() || summary.reactions_as_tags {
        return Vec::new();
    }
    recent_userpics(summary)
        .into_iter()
        .map(|(id, peers)| RecentReactors {
            reaction: reaction_key(&id),
            peers: peers
                .into_iter()
                .map(|peer| {
                    let name = session
                        .peer(peer)
                        .map(|p| p.name().to_owned())
                        .unwrap_or_default();
                    (crate::convert::from_data_peer(peer), name)
                })
                .collect(),
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

    fn reactor(peer: DataPeerId, id: ReactionId) -> tg_data::RecentReactor {
        tg_data::RecentReactor {
            peer_id: peer,
            reaction: id,
            date: 0,
            unread: false,
            my: false,
        }
    }

    fn heart() -> ReactionId {
        ReactionId::Emoji("❤".into())
    }

    #[test]
    fn known_reactors_replace_the_count() {
        let a = DataPeerId::from_user(tg_data::UserId(11));
        let b = DataPeerId::from_user(tg_data::UserId(12));
        let summary = MessageReactionsSummary {
            results: vec![
                count(heart(), 2, true),
                count(ReactionId::Emoji("👍".into()), 1, false),
            ],
            recent_reactors: vec![
                reactor(a, heart()),
                reactor(b, ReactionId::Emoji("👍".into())),
                reactor(b, heart()),
            ],
            ..MessageReactionsSummary::default()
        };
        assert_eq!(
            recent_userpics(&summary),
            [
                (heart(), vec![a, b]),
                (ReactionId::Emoji("👍".into()), vec![b])
            ]
        );
    }

    #[test]
    fn counts_stay_when_the_reactors_are_not_all_known() {
        let a = DataPeerId::from_user(tg_data::UserId(11));
        // Two hearts, one reactor known.
        let partial = MessageReactionsSummary {
            results: vec![count(heart(), 2, false)],
            recent_reactors: vec![reactor(a, heart())],
            ..MessageReactionsSummary::default()
        };
        assert!(recent_userpics(&partial).is_empty());
        // Four reactions in total: more than `kMaxRecentUserpics`.
        let many = MessageReactionsSummary {
            results: vec![count(heart(), 4, false)],
            recent_reactors: (1..=4)
                .map(|i| reactor(DataPeerId::from_user(tg_data::UserId(i)), heart()))
                .collect(),
            ..MessageReactionsSummary::default()
        };
        assert!(recent_userpics(&many).is_empty());
        // A reaction without any listed reactor.
        let missing = MessageReactionsSummary {
            results: vec![count(heart(), 1, false), count(ReactionId::Paid, 1, false)],
            recent_reactors: vec![reactor(a, heart())],
            ..MessageReactionsSummary::default()
        };
        assert!(recent_userpics(&missing).is_empty());
        // No recent list at all (a big channel).
        let none = MessageReactionsSummary {
            results: vec![count(heart(), 1, false)],
            ..MessageReactionsSummary::default()
        };
        assert!(recent_userpics(&none).is_empty());
    }

    #[test]
    fn a_reactor_of_an_unlisted_reaction_is_dropped() {
        let a = DataPeerId::from_user(tg_data::UserId(11));
        let b = DataPeerId::from_user(tg_data::UserId(12));
        let summary = MessageReactionsSummary {
            results: vec![count(heart(), 1, false)],
            recent_reactors: vec![
                reactor(b, ReactionId::Emoji("👍".into())),
                reactor(a, heart()),
                reactor(b, heart()),
            ],
            ..MessageReactionsSummary::default()
        };
        assert_eq!(recent_userpics(&summary), [(heart(), vec![a])]);
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
