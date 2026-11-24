//! Additional live MTProto RPC wiring for saved music IDs (`account.getSavedMusicIds`),
//! single attach-menu bot lookup (`messages.getAttachMenuBot`), and non-personal app-log
//! telemetry (`help.saveAppLog`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_saved_music.cpp` (`SavedMusic::loadIds`, `SavedMusic::has`)
//! - `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp` (`messages.getAttachMenuBot`)
//! - `Telegram/SourceFiles/settings/sections/settings_premium.cpp` (`SendAppLog`,
//!   `SendScreenShow`, `SendScreenAccept`)

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tg_api::RpcClient;
use tg_api::channels::count_hash_i64;
use tg_api::convert::{apply_tl_users_and_chats, input_user_for};
use tg_bots::mini_app::AttachMenuBot;
use tg_bots::stand_in::UserId as BotUserId;
use tg_bots::tl_convert::attach_menu_bot_from_tl;
use tg_data::{Session, UserId as DataUserId};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;

const LIVE_EXTRAS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Throttle interval for `account.getSavedMusicIds` (`kReloadIdsEvery = 30s` in
/// `data/data_saved_music.cpp:26`).
pub const SAVED_MUSIC_IDS_RELOAD_INTERVAL: Duration = Duration::from_secs(30);

// =============================================================================
// 1. Saved Music IDs (`account.getSavedMusicIds`)
// =============================================================================

#[derive(Default)]
struct SavedMusicIdsState {
    ids: Vec<i64>,
    last_received: Option<Instant>,
}

struct SavedMusicStateEntry {
    owner: Weak<LiveBridgeState>,
    state: SavedMusicIdsState,
}

fn saved_music_state_registry() -> &'static Mutex<HashMap<usize, SavedMusicStateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, SavedMusicStateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_saved_music_state<R>(
    live: &LiveSession,
    f: impl FnOnce(&mut SavedMusicIdsState) -> R,
) -> R {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = saved_music_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| SavedMusicStateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: SavedMusicIdsState::default(),
    });
    f(&mut entry.state)
}

/// Outcome of [`load_saved_music_ids`] (`account.getSavedMusicIds`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMusicIdsResult {
    /// Current ordered list of saved profile music document IDs.
    pub ids: Vec<i64>,
    /// `true` when the server returned `account.savedMusicIds` (list changed);
    /// `false` on `account.savedMusicIdsNotModified` or throttle-cache hit.
    pub modified: bool,
    /// `true` when returned directly from the 30-second local throttle cache without an RPC.
    pub from_cache: bool,
}

/// Computes `Api::CountHash(_myIds)` for `account.getSavedMusicIds`
/// (`data/data_saved_music.cpp:78`).
#[must_use]
pub fn compute_saved_music_ids_hash(ids: &[i64]) -> i64 {
    count_hash_i64(ids.iter().copied())
}

/// Fetches the signed-in user's saved music document IDs via `account.getSavedMusicIds`,
/// respecting tdesktop's 30-second reload throttle (`kReloadIdsEvery`) unless `force` is `true`.
///
/// Port of `Data::SavedMusic::loadIds` (`data/data_saved_music.cpp:71-92`).
pub fn load_saved_music_ids(
    live: &LiveSession,
    force: bool,
) -> Result<SavedMusicIdsResult, String> {
    let (cached_ids, throttled) = with_saved_music_state(live, |st| {
        let within_window = !force
            && st
                .last_received
                .is_some_and(|ts| ts.elapsed() < SAVED_MUSIC_IDS_RELOAD_INTERVAL);
        (st.ids.clone(), within_window)
    });
    if throttled {
        return Ok(SavedMusicIdsResult {
            ids: cached_ids,
            modified: false,
            from_cache: true,
        });
    }

    let hash = compute_saved_music_ids_hash(&cached_ids);
    let rpc = live.runtime().rpc().clone();
    let res = block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(LIVE_EXTRAS_RPC_TIMEOUT, async move {
            rpc.invoke(&tl_fns::account::GetSavedMusicIds { hash })
                .await
        })
        .await
    });

    match res {
        Ok(tl_enums::account::SavedMusicIds::SavedMusicIds(data)) => {
            let ids = data.ids;
            with_saved_music_state(live, |st| {
                st.ids = ids.clone();
                st.last_received = Some(Instant::now());
            });
            live.bridge_state().notify_ui();
            Ok(SavedMusicIdsResult {
                ids,
                modified: true,
                from_cache: false,
            })
        }
        Ok(tl_enums::account::SavedMusicIds::NotModified) => {
            let ids = with_saved_music_state(live, |st| {
                st.last_received = Some(Instant::now());
                st.ids.clone()
            });
            Ok(SavedMusicIdsResult {
                ids,
                modified: false,
                from_cache: false,
            })
        }
        Err(err) => {
            with_saved_music_state(live, |st| {
                st.last_received = Some(Instant::now());
            });
            Err(err)
        }
    }
}

/// Returns `true` if `document_id` is in the cached saved profile music ID list
/// (`Data::SavedMusic::has`, `data/data_saved_music.cpp:94-96`).
#[must_use]
pub fn has_saved_music_id(live: &LiveSession, document_id: i64) -> bool {
    with_saved_music_state(live, |st| st.ids.contains(&document_id))
}

/// Returns a snapshot of the cached saved profile music document IDs.
#[must_use]
pub fn cached_saved_music_ids(live: &LiveSession) -> Vec<i64> {
    with_saved_music_state(live, |st| st.ids.clone())
}

/// Updates the local saved music ID cache after `account.saveMusic` succeeds
/// (`data/data_saved_music.cpp:119,161`).
pub(crate) fn record_saved_music_mutation(live: &LiveSession, document_id: i64, unsave: bool) {
    with_saved_music_state(live, |st| {
        st.ids.retain(|&id| id != document_id);
        if !unsave {
            st.ids.insert(0, document_id);
        }
    });
}

// =============================================================================
// 2. Single Attach-Menu Bot Lookup (`messages.getAttachMenuBot`)
// =============================================================================

/// Fetches a single attach-menu bot entry via `messages.getAttachMenuBot`, ingests returned
/// `users` into `Session`, updates the session's [`tg_bots::AttachMenuStore`], and returns the
/// parsed [`AttachMenuBot`].
///
/// Port of `InlineBots::AttachWebView::requestAddToMenu` (`inline_bots/bot_attach_web_view.cpp:2769-2787`).
pub fn get_attach_menu_bot(live: &LiveSession, bot_id: BotUserId) -> Result<AttachMenuBot, String> {
    let Some(input_user) = read_session(live, |s| {
        s.user(DataUserId(bot_id.0))
            .map(|_| input_user_for(s, DataUserId(bot_id.0)))
    }) else {
        return Err(format!("Bot {} not found in session.", bot_id.0));
    };
    if matches!(input_user, tl_enums::InputUser::Empty) {
        return Err(format!("Bot {} not found in session.", bot_id.0));
    }

    let rpc = live.runtime().rpc().clone();
    let tl_enums::AttachMenuBotsBot::AttachMenuBotsBot(resp) =
        block_on_runtime(live.runtime().tokio_handle(), async move {
            with_rpc_timeout(LIVE_EXTRAS_RPC_TIMEOUT, async move {
                rpc.invoke(&tl_fns::messages::GetAttachMenuBot { bot: input_user })
                    .await
            })
            .await
        })?;

    if !resp.users.is_empty() {
        write_session(live, |s| {
            apply_tl_users_and_chats(s, &resp.users, &[]);
        });
    }

    let parsed = attach_menu_bot_from_tl(&resp.bot);
    if parsed.bot_id != bot_id {
        return Err(format!(
            "messages.getAttachMenuBot returned bot {} instead of requested {}.",
            parsed.bot_id.0, bot_id.0
        ));
    }

    crate::bots::upsert_attach_menu_bot(live.runtime().session(), parsed.clone());
    live.bridge_state().notify_ui();
    Ok(parsed)
}

// =============================================================================
// 3. Non-Personal App Log Telemetry (`help.saveAppLog`)
// =============================================================================

/// Non-personal UI telemetry events sent to `help.saveAppLog` (`peer = 0`), matching the exact
/// set tdesktop sends in `settings/sections/settings_premium.cpp:499-549`.
///
/// Personal identifiers (peer IDs, phone numbers, message text, or account identifiers) are
/// intentionally excluded by type construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonPersonalAppLogEvent {
    /// Premium promo screen opened (`"premium.promo_screen_show"`,
    /// `settings/sections/settings_premium.cpp:519-542`).
    PremiumPromoScreenShow {
        /// Ordered list of promo feature keys shown on screen (`premium_promo_order`).
        order: Vec<String>,
        /// Entry-point reference string (`source`; empty defaults to `"settings"` via `ResolveRef`).
        source: String,
    },
    /// Premium promo screen accepted (`"premium.promo_screen_accept"`,
    /// `settings/sections/settings_premium.cpp:544-549`).
    PremiumPromoScreenAccept,
}

impl NonPersonalAppLogEvent {
    /// Resolves an empty promo source reference to `"settings"` (`ResolveRef`,
    /// `settings_premium.cpp:515-517`).
    #[must_use]
    pub fn resolve_source(source: &str) -> &str {
        if source.is_empty() {
            "settings"
        } else {
            source
        }
    }

    /// Encodes this non-personal event as a TL `InputAppEvent` with `peer = 0`.
    #[must_use]
    pub fn to_tl(&self, time_secs: f64) -> tl_enums::InputAppEvent {
        match self {
            Self::PremiumPromoScreenShow { order, source } => {
                let list = order
                    .iter()
                    .map(|item| {
                        tl_enums::JSONValue::JsonString(tl_types::JsonString {
                            value: item.clone(),
                        })
                    })
                    .collect();
                let values = vec![
                    tl_enums::JSONObjectValue::JsonObjectValue(tl_types::JsonObjectValue {
                        key: "premium_promo_order".to_owned(),
                        value: tl_enums::JSONValue::JsonArray(tl_types::JsonArray { value: list }),
                    }),
                    tl_enums::JSONObjectValue::JsonObjectValue(tl_types::JsonObjectValue {
                        key: "source".to_owned(),
                        value: tl_enums::JSONValue::JsonString(tl_types::JsonString {
                            value: Self::resolve_source(source).to_owned(),
                        }),
                    }),
                ];
                tl_enums::InputAppEvent::InputAppEvent(tl_types::InputAppEvent {
                    time: time_secs,
                    r#type: "premium.promo_screen_show".to_owned(),
                    peer: 0,
                    data: tl_enums::JSONValue::JsonObject(tl_types::JsonObject { value: values }),
                })
            }
            Self::PremiumPromoScreenAccept => {
                tl_enums::InputAppEvent::InputAppEvent(tl_types::InputAppEvent {
                    time: time_secs,
                    r#type: "premium.promo_screen_accept".to_owned(),
                    peer: 0,
                    data: tl_enums::JSONValue::JsonNull,
                })
            }
        }
    }
}

/// Sends non-personal app-log events via `help.saveAppLog` using the current Unix timestamp
/// (fractional seconds, matching `settings/sections/settings_premium.cpp:503-512`).
///
/// Returns `Ok(false)` without issuing an RPC if `events` is empty.
pub fn save_app_log(live: &LiveSession, events: &[NonPersonalAppLogEvent]) -> Result<bool, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    save_app_log_at(live, events, now)
}

/// Sends non-personal app-log events via `help.saveAppLog` with an explicit Unix timestamp
/// `time_secs` (fractional seconds, `peer = 0`).
///
/// Returns `Ok(false)` without issuing an RPC if `events` is empty.
pub fn save_app_log_at(
    live: &LiveSession,
    events: &[NonPersonalAppLogEvent],
    time_secs: f64,
) -> Result<bool, String> {
    if events.is_empty() {
        return Ok(false);
    }
    let tl_events: Vec<tl_enums::InputAppEvent> =
        events.iter().map(|ev| ev.to_tl(time_secs)).collect();
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(LIVE_EXTRAS_RPC_TIMEOUT, async move {
            rpc.invoke(&tl_fns::help::SaveAppLog { events: tl_events })
                .await
        })
        .await
    })
}

// =============================================================================
// Convenience Methods on LiveSession
// =============================================================================

impl LiveSession {
    /// Loads saved profile music document IDs via `account.getSavedMusicIds`, honoring the
    /// 30-second throttle window (`data/data_saved_music.cpp:71-92`).
    pub fn load_saved_music_ids(&self) -> Result<SavedMusicIdsResult, String> {
        load_saved_music_ids(self, false)
    }

    /// Forces a fresh `account.getSavedMusicIds` request using the current cached list's
    /// `CountHash`.
    pub fn refresh_saved_music_ids(&self) -> Result<SavedMusicIdsResult, String> {
        load_saved_music_ids(self, true)
    }

    /// Returns `true` if `document_id` is in the cached saved music IDs list.
    #[must_use]
    pub fn has_saved_music_id(&self, document_id: i64) -> bool {
        has_saved_music_id(self, document_id)
    }

    /// Returns the cached saved music document IDs.
    #[must_use]
    pub fn cached_saved_music_ids(&self) -> Vec<i64> {
        cached_saved_music_ids(self)
    }

    /// Fetches a single attach-menu bot entry via `messages.getAttachMenuBot`, applies returned
    /// users into `Session`, and updates the session's `AttachMenuStore`.
    pub fn fetch_attach_menu_bot(&self, bot_id: BotUserId) -> Result<AttachMenuBot, String> {
        get_attach_menu_bot(self, bot_id)
    }

    /// Sends non-personal app-log events via `help.saveAppLog` (`peer = 0`).
    pub fn save_app_log(&self, events: &[NonPersonalAppLogEvent]) -> Result<bool, String> {
        save_app_log(self, events)
    }

    /// Sends non-personal app-log events via `help.saveAppLog` at an explicit timestamp.
    pub fn save_app_log_at(
        &self,
        events: &[NonPersonalAppLogEvent],
        time_secs: f64,
    ) -> Result<bool, String> {
        save_app_log_at(self, events, time_secs)
    }
}

fn read_session<R: Send>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R {
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

fn write_session<R: Send>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R {
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
