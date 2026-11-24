//! Live AI Message Summary (`messages.summarizeText`), Birthday Suggestion (`account.updateBirthday`),
//! and Userpic Suggestion (`photos.updateProfilePhoto`) wiring for [`LiveSession`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_transcribes.cpp` (`Transcribes::toggleSummary`, `summarize`, `summary`)
//! - `Telegram/SourceFiles/history/view/history_view_summary_header.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_birthday_suggestion.cpp`
//! - `Telegram/SourceFiles/history/view/media/history_view_userpic_suggestion.cpp`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, entities_from_tl, input_peer_for};
use tg_data::Session;
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::{data_text_to_ui_text, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::history::backend::MessageSummaryEntry;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const SUMMARY_RPC_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
struct SummarySessionState {
    entries: HashMap<(PeerId, MsgId), MessageSummaryEntry>,
}

struct SummaryStateSlot {
    owner: Weak<LiveBridgeState>,
    state: SummarySessionState,
}

fn summary_registry() -> &'static Mutex<HashMap<usize, SummaryStateSlot>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, SummaryStateSlot>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_summary_state<R>(live: &LiveSession, f: impl FnOnce(&mut SummarySessionState) -> R) -> R {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = summary_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.retain(|_, slot| slot.owner.strong_count() > 0);
    let slot = map.entry(key).or_insert_with(|| SummaryStateSlot {
        owner: Arc::downgrade(live.bridge_state()),
        state: SummarySessionState::default(),
    });
    f(&mut slot.state)
}

fn read_session_sync<R: Send>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R {
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session_sync<R: Send>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R {
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn invoke_summary_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(SUMMARY_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn resolve_input_peer(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let data_peer = to_data_peer(peer_id);
    let peer = read_session_sync(live, |s| input_peer_for(s, data_peer));
    match peer {
        tl_enums::InputPeer::Empty => None,
        other => Some(other),
    }
}

impl LiveSession {
    /// Returns the cached AI summary entry for `(peer_id, msg_id)`, if any
    /// (`Api::Transcribes::summary`).
    #[must_use]
    pub fn message_summary_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
    ) -> Option<MessageSummaryEntry> {
        with_summary_state(self, |st| st.entries.get(&(peer_id, msg_id)).cloned())
    }

    /// Toggles AI message summary visibility, calling `messages.summarizeText` if not yet cached
    /// or if the requested target language changed (`Api::Transcribes::toggleSummary`).
    pub fn toggle_message_summary_live(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        to_lang: Option<&str>,
    ) -> Result<MessageSummaryEntry, String> {
        let key = (peer_id, msg_id);
        let normalized_lang = to_lang
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let target_lang_key = normalized_lang.clone().unwrap_or_default();

        // Fast path: cached result for this `(peer_id, msg_id)` and language -> toggle visibility
        // without issuing another `messages.summarizeText` RPC.
        let cached_toggle = with_summary_state(self, |st| {
            if let Some(entry) = st.entries.get_mut(&key)
                && !entry.loading
                && !entry.text.text.is_empty()
                && entry.to_lang == target_lang_key
            {
                entry.shown = if entry.premium_required {
                    false
                } else {
                    !entry.shown
                };
                return Some(entry.clone());
            }
            None
        });
        if let Some(toggled) = cached_toggle {
            self.bridge_state().notify_ui();
            return Ok(toggled);
        }

        let Some(input_peer) = resolve_input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };

        with_summary_state(self, |st| {
            let entry = st.entries.entry(key).or_default();
            entry.loading = true;
            entry.shown = true;
            entry.to_lang = target_lang_key.clone();
        });

        let req = tl_fns::messages::SummarizeText {
            peer: input_peer,
            id: msg_id.0,
            to_lang: normalized_lang,
            tone: None,
        };
        match invoke_summary_rpc(self, req) {
            Ok(tl_enums::TextWithEntities::TextWithEntities(res)) => {
                let data_text =
                    tg_data::TextWithEntities::new(res.text, entities_from_tl(&res.entities));
                let ui_text = data_text_to_ui_text(&data_text);
                let entry = MessageSummaryEntry {
                    text: ui_text,
                    loading: false,
                    shown: true,
                    premium_required: false,
                    to_lang: target_lang_key,
                };
                with_summary_state(self, |st| {
                    st.entries.insert(key, entry.clone());
                });
                self.bridge_state().notify_ui();
                Ok(entry)
            }
            Err(err) => {
                let premium_required = err.contains("SUMMARY_FLOOD_PREMIUM");
                with_summary_state(self, |st| {
                    let entry = st.entries.entry(key).or_default();
                    entry.loading = false;
                    entry.shown = false;
                    entry.premium_required = premium_required;
                });
                self.bridge_state().notify_ui();
                Err(err)
            }
        }
    }

    /// Applies a suggested birthday (`account.updateBirthday`) from a `SuggestBirthday` card.
    pub fn apply_suggested_birthday_live(
        &self,
        day: u8,
        month: u8,
        year: Option<u16>,
    ) -> Result<(), String> {
        let req = tl_fns::account::UpdateBirthday {
            birthday: Some(tl_enums::Birthday::Birthday(tl_types::Birthday {
                day: i32::from(day),
                month: i32::from(month),
                year: year.map(i32::from),
            })),
        };
        let _ = invoke_summary_rpc(self, req)?;
        write_session_sync(self, |s| {
            let self_pid = s.self_peer_id();
            if let Some(tg_data::Peer::User(u)) = s.peer_mut(self_pid) {
                u.birthday = Some(tg_data::Birthday {
                    day,
                    month,
                    year: year.unwrap_or(0),
                });
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Applies a suggested profile photo (`photos.updateProfilePhoto`) from a `SuggestProfilePhoto` card.
    pub fn apply_suggested_profile_photo_live(
        &self,
        photo_id: i64,
        access_hash: i64,
        file_reference: &[u8],
    ) -> Result<(), String> {
        let req = tl_fns::photos::UpdateProfilePhoto {
            fallback: false,
            bot: None,
            id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                id: photo_id,
                access_hash,
                file_reference: file_reference.to_vec(),
            }),
        };
        let tl_enums::photos::Photo::Photo(resp) = invoke_summary_rpc(self, req)?;
        write_session_sync(self, |s| {
            apply_tl_users_and_chats(s, &resp.users, &[]);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }
}
