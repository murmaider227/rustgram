//! Live poll results reload and poll statistics RPCs for [`LiveSession`].
//!
//! Port origin: `Telegram/SourceFiles/api/api_polls.cpp` (`Api::Polls::reloadResults`
//! lines 461-480 and `Api::Polls::requestStats` lines 482-541) and
//! `Telegram/SourceFiles/data/data_poll.cpp` (`PollData::applyResults`).
//!
//! Implements:
//! - `messages.getPollResults` (sending cached `poll_hash`, applying returned `Update::MessagePoll`
//!   both by `(peer, msg_id)` and by `poll_id`, preserving `chosen` flags when `PollResults::min`
//!   is `true`, and refreshing cached `poll_hash`).
//! - `stats.getPollStats` with automatic `stats.loadAsyncGraph` resolution when Telegram returns
//!   `StatsGraph::Async`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{input_peer_for, peer_id_from_tl, text_with_entities_from_tl};
use tg_data::{
    FullMsgId as DataFullMsgId, MessageContent, MessageMedia, MsgId as DataMsgId,
    PeerId as DataPeerId, PollAnswer as DataPollAnswer, PollAnswerVoters as DataPollAnswerVoters,
    PollData, PollId, Session,
};
use tg_stats::StatisticalGraph;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::RwLock;

use crate::convert::{known_peer_ids, to_data_msg, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use crate::stats::statistical_graph_from_tl;
use tg_app_core::history::backend::{PollAttachment, PollOption};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Default timeout for synchronous poll reload / poll stats RPCs.
const POLL_STATS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct PollHashCache {
    /// Last server-reported `poll.hash` keyed by `PollId` (`poll.id`).
    by_poll_id: HashMap<i64, i64>,
}

struct PollCacheEntry {
    session_weak: Weak<RwLock<Session>>,
    cache: Arc<Mutex<PollHashCache>>,
}

fn poll_cache_registry() -> &'static Mutex<HashMap<usize, PollCacheEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, PollCacheEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn poll_hash_cache_for(live: &LiveSession) -> Arc<Mutex<PollHashCache>> {
    let session_arc = live.runtime().session();
    let key = Arc::as_ptr(session_arc) as usize;
    let session_weak = Arc::downgrade(session_arc);

    let mut map = poll_cache_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.retain(|_, entry| entry.session_weak.strong_count() > 0);
    if let Some(entry) = map.get(&key)
        && entry.session_weak.ptr_eq(&session_weak)
    {
        return Arc::clone(&entry.cache);
    }
    let created = Arc::new(Mutex::new(PollHashCache::default()));
    map.insert(
        key,
        PollCacheEntry {
            session_weak,
            cache: Arc::clone(&created),
        },
    );
    created
}

fn poll_data_to_attachment(poll: &PollData) -> PollAttachment {
    let options = poll
        .answers
        .iter()
        .map(|ans| {
            let tally = poll.results.results.iter().find(|r| r.option == ans.option);
            PollOption {
                text: ans.text.text.clone(),
                votes: tally.map_or(0, |t| t.voters.max(0) as u32),
                chosen: tally.is_some_and(|t| t.chosen),
                correct: tally.is_some_and(|t| t.correct),
            }
        })
        .collect();
    PollAttachment {
        question: poll.question.text.clone(),
        is_quiz: poll.quiz,
        is_anonymous: !poll.public_voters,
        multiple_choice: poll.multiple_choice,
        closed: poll.closed,
        options,
        total_voters: poll.results.total_voters.unwrap_or(0).max(0) as u32,
        explanation: poll.results.solution.as_ref().map(|s| s.text.clone()),
    }
}

/// Applies a single `UpdateMessagePoll` onto a mutable `PollData` while respecting
/// `PollResults::min` (`data_poll.cpp:187-235`: when `min == true`, preserve previously
/// known `chosen` flags on matching options).
fn apply_tl_poll_update_to_data(
    poll: &mut PollData,
    upd: &tl_types::UpdateMessagePoll,
    hash_cache: &mut PollHashCache,
) {
    if let Some(tl_enums::Poll::Poll(tl_p)) = &upd.poll {
        poll.id = PollId::from_tl(tl_p.id);
        poll.question = text_with_entities_from_tl(&tl_p.question);
        poll.closed = tl_p.closed;
        poll.public_voters = tl_p.public_voters;
        poll.multiple_choice = tl_p.multiple_choice;
        poll.quiz = tl_p.quiz;
        poll.close_period = tl_p.close_period;
        poll.close_date = tl_p.close_date;
        poll.answers = tl_p
            .answers
            .iter()
            .filter_map(|a| {
                let tl_enums::PollAnswer::PollAnswer(ans) = a else {
                    return None;
                };
                Some(DataPollAnswer {
                    text: text_with_entities_from_tl(&ans.text),
                    option: ans.option.clone(),
                })
            })
            .collect();
        hash_cache.by_poll_id.insert(tl_p.id, tl_p.hash);
    }

    let tl_enums::PollResults::PollResults(res) = &upd.results;
    if let Some(total) = res.total_voters {
        poll.results.total_voters = Some(total);
    }
    if let Some(ref recent) = res.recent_voters {
        poll.results.recent_voters = recent.iter().map(peer_id_from_tl).collect();
    }
    if let Some(ref solution_text) = res.solution {
        poll.results.solution = Some(tg_data::TextWithEntities {
            text: solution_text.clone(),
            entities: res
                .solution_entities
                .as_deref()
                .map(|ents| {
                    text_with_entities_from_tl(&tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: solution_text.clone(),
                            entities: ents.to_vec(),
                        },
                    ))
                    .entities
                })
                .unwrap_or_default(),
        });
    }
    if let Some(ref voters_list) = res.results {
        let prev_chosen: HashMap<Vec<u8>, bool> = poll
            .results
            .results
            .iter()
            .map(|r| (r.option.clone(), r.chosen))
            .collect();
        poll.results.results = voters_list
            .iter()
            .map(|v| {
                let tl_enums::PollAnswerVoters::PollAnswerVoters(av) = v;
                let chosen = if res.min {
                    prev_chosen.get(&av.option).copied().unwrap_or(av.chosen)
                } else {
                    av.chosen
                };
                DataPollAnswerVoters {
                    option: av.option.clone(),
                    voters: av.voters.unwrap_or(0),
                    chosen,
                    correct: av.correct,
                }
            })
            .collect();
    }
}

fn apply_poll_updates_to_session(
    session: &mut Session,
    fallback_target: DataFullMsgId,
    updates: &tl_enums::Updates,
    hash_cache: &mut PollHashCache,
) {
    let list: Vec<&tl_enums::Update> = match updates {
        tl_enums::Updates::Updates(u) => u.updates.iter().collect(),
        tl_enums::Updates::Combined(u) => u.updates.iter().collect(),
        tl_enums::Updates::UpdateShort(u) => vec![&u.update],
        _ => Vec::new(),
    };

    for upd in list {
        let tl_enums::Update::MessagePoll(poll_upd) = upd else {
            continue;
        };
        let explicit_target = match (&poll_upd.peer, poll_upd.msg_id) {
            (Some(p), Some(mid)) => Some(DataFullMsgId::new(
                peer_id_from_tl(p),
                DataMsgId(i64::from(mid)),
            )),
            _ => None,
        };

        let mut applied = false;
        if let Some(target) = explicit_target
            && let Some(msg) = session.message_mut(target)
            && let MessageContent::Regular {
                media: MessageMedia::Poll(ref mut poll),
                ..
            } = msg.content
        {
            apply_tl_poll_update_to_data(poll, poll_upd, hash_cache);
            applied = true;
        }

        if !applied {
            // Fall back to the requested `(peer_id, msg_id)` when `updateMessagePoll`
            // omits `peer`/`msg_id` (standard Telegram server behavior for `getPollResults`).
            if let Some(msg) = session.message_mut(fallback_target)
                && let MessageContent::Regular {
                    media: MessageMedia::Poll(ref mut poll),
                    ..
                } = msg.content
                && (poll.id.0 == 0 || poll.id == PollId::from_tl(poll_upd.poll_id))
            {
                apply_tl_poll_update_to_data(poll, poll_upd, hash_cache);
                applied = true;
            }
        }

        // Also update any matching `poll_id` stored in any loaded peer history.
        if !applied {
            let poll_id = PollId::from_tl(poll_upd.poll_id);
            let matching_ids: Vec<DataFullMsgId> = known_peer_ids(session)
                .into_iter()
                .flat_map(|pid: DataPeerId| {
                    let mut found = Vec::new();
                    if let Some(hist) = session.history(pid) {
                        for slice in hist.loaded_ranges.slices() {
                            for &mid in &slice.messages {
                                let full = DataFullMsgId::new(pid, mid);
                                if let Some(MessageMedia::Poll(p)) =
                                    session.message(full).and_then(|m| m.media())
                                    && p.id == poll_id
                                {
                                    found.push(full);
                                }
                            }
                        }
                    }
                    found
                })
                .collect();
            for full_id in matching_ids {
                if let Some(msg) = session.message_mut(full_id)
                    && let MessageContent::Regular {
                        media: MessageMedia::Poll(ref mut poll),
                        ..
                    } = msg.content
                {
                    apply_tl_poll_update_to_data(poll, poll_upd, hash_cache);
                }
            }
        }
    }
}

impl LiveSession {
    /// Seeds or updates the cached `poll_hash` for `poll_id` (used by `messages.getPollResults`).
    pub fn set_cached_poll_hash(&self, poll_id: i64, poll_hash: i64) {
        let cache = poll_hash_cache_for(self);
        let mut guard = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.by_poll_id.insert(poll_id, poll_hash);
    }

    /// Refreshes poll results via `messages.getPollResults` (`api/api_polls.cpp:461-480`).
    pub(crate) fn reload_poll_results_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Result<Option<PollAttachment>, String> {
        let data_peer = to_data_peer(peer_id);
        let data_msg = to_data_msg(msg_id);
        let full_id = DataFullMsgId::new(data_peer, data_msg);

        let session_arc = Arc::clone(self.runtime().session());
        let handle = self.runtime().tokio_handle().clone();
        let (input_peer, poll_id) = block_on_runtime(&handle, async move {
            let s = session_arc.read().await;
            let input = input_peer_for(&s, data_peer);
            let pid = s.message(full_id).and_then(|m| match m.media() {
                Some(MessageMedia::Poll(p)) => Some(p.id.to_tl()),
                _ => None,
            });
            (input, pid)
        });
        if matches!(input_peer, tl_enums::InputPeer::Empty) {
            return Err("Peer not found in session.".to_owned());
        }

        let cache = poll_hash_cache_for(self);
        let poll_hash = poll_id
            .and_then(|pid| {
                cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .by_poll_id
                    .get(&pid)
                    .copied()
            })
            .unwrap_or(0);

        let rpc = self.runtime().rpc().clone();
        let req = tl_fns::messages::GetPollResults {
            peer: input_peer,
            msg_id: msg_id.0,
            poll_hash,
        };
        let updates = block_on_runtime(&handle, async move {
            with_rpc_timeout(
                POLL_STATS_RPC_TIMEOUT,
                async move { rpc.invoke(&req).await },
            )
            .await
        })?;

        let _ = self.apply_updates_to_session(&updates);

        let session_arc = Arc::clone(self.runtime().session());
        let attachment = block_on_runtime(&handle, async move {
            let mut s = session_arc.write().await;
            let mut cache_guard = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            apply_poll_updates_to_session(&mut s, full_id, &updates, &mut cache_guard);
            s.message(full_id).and_then(|m| match m.media() {
                Some(MessageMedia::Poll(p)) => Some(poll_data_to_attachment(p)),
                _ => None,
            })
        });

        self.bridge_state().notify_ui();
        Ok(attachment)
    }

    /// Fetches poll vote statistics via `stats.getPollStats` and resolves `StatsGraphAsync`
    /// via `stats.loadAsyncGraph` (`api/api_polls.cpp:482-541`).
    pub(crate) fn poll_stats_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        dark: bool,
    ) -> Result<StatisticalGraph, String> {
        let data_peer = to_data_peer(peer_id);
        let session_arc = Arc::clone(self.runtime().session());
        let handle = self.runtime().tokio_handle().clone();
        let input_peer = block_on_runtime(&handle, async move {
            let s = session_arc.read().await;
            input_peer_for(&s, data_peer)
        });
        if matches!(input_peer, tl_enums::InputPeer::Empty) {
            return Err("Peer not found in session.".to_owned());
        }

        let rpc = self.runtime().rpc().clone();
        let req = tl_fns::stats::GetPollStats {
            dark,
            peer: input_peer,
            msg_id: msg_id.0,
        };
        let tl_enums::stats::PollStats::PollStats(resp) = block_on_runtime(&handle, async move {
            with_rpc_timeout(
                POLL_STATS_RPC_TIMEOUT,
                async move { rpc.invoke(&req).await },
            )
            .await
        })?;

        let graph = statistical_graph_from_tl(&resp.votes_graph);
        // `api/api_polls.cpp:529-535`: if `votes_graph` is `statsGraphAsync` (empty chart,
        // no error, non-empty token), hydrate it via `stats.loadAsyncGraph(token, x = None)`.
        if graph.chart.is_empty() && graph.error.is_empty() && !graph.zoom_token.is_empty() {
            let token = graph.zoom_token;
            let rpc2 = self.runtime().rpc().clone();
            let async_req = tl_fns::stats::LoadAsyncGraph { token, x: None };
            let resolved_tl = block_on_runtime(&handle, async move {
                with_rpc_timeout(POLL_STATS_RPC_TIMEOUT, async move {
                    rpc2.invoke(&async_req).await
                })
                .await
            })?;
            return Ok(statistical_graph_from_tl(&resolved_tl));
        }

        Ok(graph)
    }
}
