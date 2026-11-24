//! Live RPC glue for Connected Websites (`account.getWebAuthorizations`,
//! `account.resetWebAuthorization`, `account.resetWebAuthorizations`) and Custom Ringtones
//! (`account.getSavedRingtones`, `account.saveRingtone`, `account.uploadRingtone`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_websites.cpp`
//! - `Telegram/SourceFiles/api/api_ringtones.cpp`
//! - `Telegram/SourceFiles/boxes/ringtones_box.cpp`

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for};
use tg_data::{BlockStatus, PeerId as DataPeerId, Session, UserId};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::{debug, warn};

use crate::bridge::LiveBridgeState;
use crate::convert::{format_date_time, peer_display_title};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::settings::backend::{DEFAULT_RINGTONE_MAX_SIZE, SavedRingtone, WebAuthorization};

/// Timeout for settings-extra RPC calls.
const SETTINGS_EXTRA_RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum chunk size for `upload.saveFilePart` (512 KiB; Telegram small file part limit).
const RINGTONE_UPLOAD_PART_SIZE: usize = 512 * 1024;

static NEXT_UPLOAD_FILE_ID: AtomicI64 = AtomicI64::new(0x5249_4E47_0000_0001);

#[derive(Default)]
struct SettingsExtraState {
    websites_loaded: bool,
    websites: Vec<WebAuthorization>,
    ringtones_loaded: bool,
    ringtones_hash: i64,
    ringtones: Vec<SavedRingtone>,
}

struct SettingsExtraEntry {
    owner: Weak<LiveBridgeState>,
    state: SettingsExtraState,
}

fn registry() -> &'static Mutex<HashMap<usize, SettingsExtraEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, SettingsExtraEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_state<R>(live: &LiveSession, f: impl FnOnce(&mut SettingsExtraState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| SettingsExtraEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: SettingsExtraState::default(),
    });
    f(&mut entry.state)
}

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
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

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
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

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            SETTINGS_EXTRA_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
}

/// Clears cached connected websites and saved ringtones for `live`.
pub(crate) fn invalidate_caches(live: &LiveSession) {
    with_state(live, |st| {
        st.websites_loaded = false;
        st.ringtones_loaded = false;
    });
}

// -----------------------------------------------------------------------------
// Connected Websites (`account.getWebAuthorizations`, `resetWebAuthorization*`)
// -----------------------------------------------------------------------------

fn web_authorization_from_tl(
    auth: &tl_types::WebAuthorization,
    bot_names: &HashMap<i64, String>,
) -> WebAuthorization {
    // Match `ParseEntry` in `api/api_websites.cpp:20-37` (`date_active == 0` -> `date_created`).
    let entry = tg_api::account::WebAuthorizationEntry::from_tl(auth);
    let bot_name = bot_names
        .get(&auth.bot_id)
        .filter(|name| !name.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("Bot #{}", auth.bot_id));
    WebAuthorization {
        hash: entry.hash as u64,
        bot_id: auth.bot_id,
        bot_name,
        domain: entry.domain,
        browser: entry.browser,
        platform: entry.platform,
        ip: entry.ip,
        region: entry.region,
        date_created: entry.date_created,
        date_active: entry.active_time,
        active_time: format_date_time(entry.active_time),
    }
}

/// Loads (or returns cached) connected website authorizations (`account.getWebAuthorizations`).
pub(crate) fn web_authorizations(live: &LiveSession) -> Vec<WebAuthorization> {
    if let Some(cached) = with_state(live, |st| st.websites_loaded.then(|| st.websites.clone())) {
        return cached;
    }

    match invoke_rpc(live, tl_fns::account::GetWebAuthorizations {}) {
        Ok(tl_enums::account::WebAuthorizations::WebAuthorizations(resp)) => {
            let bot_names = write_session(live, |session| {
                apply_tl_users_and_chats(session, &resp.users, &[]);
                let mut names = HashMap::new();
                for auth in &resp.authorizations {
                    let tl_enums::WebAuthorization::WebAuthorization(inner) = auth;
                    let pid = DataPeerId::from_user(UserId::from_tl(inner.bot_id));
                    if session.peer(pid).is_some() {
                        names.insert(inner.bot_id, peer_display_title(session, pid));
                    }
                }
                names
            });

            let mut list: Vec<WebAuthorization> = resp
                .authorizations
                .iter()
                .map(|a| {
                    let tl_enums::WebAuthorization::WebAuthorization(inner) = a;
                    web_authorization_from_tl(inner, &bot_names)
                })
                .collect();
            list.sort_by_key(|a| std::cmp::Reverse(a.date_active));

            with_state(live, |st| {
                st.websites_loaded = true;
                st.websites = list.clone();
            });
            list
        }
        Err(err) => {
            debug!(error = %err, "account.getWebAuthorizations failed");
            with_state(live, |st| st.websites.clone())
        }
    }
}

/// Disconnects a single website authorization (`account.resetWebAuthorization`) and optionally
/// blocks its bot (`contacts.block`).
pub(crate) fn disconnect_website(
    live: &LiveSession,
    hash: u64,
    block_bot: bool,
) -> Result<bool, String> {
    let ok = invoke_rpc(
        live,
        tl_fns::account::ResetWebAuthorization { hash: hash as i64 },
    )?;
    if !ok {
        return Ok(false);
    }

    let removed_bot_id = with_state(live, |st| {
        let bot_id = st
            .websites
            .iter()
            .find(|w| w.hash == hash)
            .map(|w| w.bot_id);
        st.websites.retain(|w| w.hash != hash);
        bot_id
    });

    if block_bot && let Some(bot_id) = removed_bot_id {
        let data_peer = DataPeerId::from_user(UserId::from_tl(bot_id));
        let input_peer = read_session(live, |session| {
            let ip = input_peer_for(session, data_peer);
            (!matches!(ip, tl_enums::InputPeer::Empty)).then_some(ip)
        });
        if let Some(id) = input_peer {
            match invoke_rpc(
                live,
                tl_fns::contacts::Block {
                    my_stories_from: false,
                    id,
                },
            ) {
                Ok(_) => {
                    write_session(live, |session| {
                        if let Some(peer) = session.peer_mut(data_peer) {
                            peer.common_mut().block_status = BlockStatus::Blocked;
                        }
                    });
                }
                Err(err) => {
                    warn!(bot_id, error = %err, "contacts.block after disconnect_website failed");
                }
            }
        }
    }

    Ok(true)
}

/// Disconnects all connected websites (`account.resetWebAuthorizations`).
pub(crate) fn disconnect_all_websites(live: &LiveSession) -> Result<usize, String> {
    let ok = invoke_rpc(live, tl_fns::account::ResetWebAuthorizations {})?;
    if !ok {
        return Ok(0);
    }
    let removed = with_state(live, |st| {
        let count = st.websites.len();
        st.websites.clear();
        st.websites_loaded = true;
        count
    });
    Ok(removed)
}

// -----------------------------------------------------------------------------
// Custom Ringtones (`account.getSavedRingtones`, `saveRingtone`, `uploadRingtone`)
// -----------------------------------------------------------------------------

fn ringtone_from_tl_document(doc: &tl_enums::Document) -> Option<SavedRingtone> {
    let tl_enums::Document::Document(d) = doc else {
        return None;
    };
    let mut file_name = String::new();
    let mut title = String::new();
    let mut duration_secs = 0.0f64;

    for attr in &d.attributes {
        match attr {
            tl_enums::DocumentAttribute::Filename(f) => {
                file_name = f.file_name.clone();
            }
            tl_enums::DocumentAttribute::Audio(a) => {
                duration_secs = f64::from(a.duration.max(0));
                if let Some(t) = &a.title
                    && !t.trim().is_empty()
                {
                    title = t.trim().to_owned();
                }
            }
            _ => {}
        }
    }

    if title.is_empty() && !file_name.is_empty() {
        title = file_name
            .rsplit_once('.')
            .map(|(stem, _)| stem.to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| file_name.clone());
    }

    Some(SavedRingtone {
        id: d.id,
        access_hash: d.access_hash,
        file_reference: d.file_reference.clone(),
        title,
        file_name,
        mime_type: d.mime_type.clone(),
        size: d.size,
        duration_secs,
        date: d.date,
    })
}

/// Loads (or returns cached) custom notification ringtones (`account.getSavedRingtones`).
pub(crate) fn saved_ringtones(live: &LiveSession) -> Vec<SavedRingtone> {
    let (loaded, hash, cached) = with_state(live, |st| {
        (st.ringtones_loaded, st.ringtones_hash, st.ringtones.clone())
    });
    if loaded {
        return cached;
    }

    match invoke_rpc(live, tl_fns::account::GetSavedRingtones { hash }) {
        Ok(tl_enums::account::SavedRingtones::SavedRingtones(resp)) => {
            let list: Vec<SavedRingtone> = resp
                .ringtones
                .iter()
                .filter_map(ringtone_from_tl_document)
                .collect();
            with_state(live, |st| {
                st.ringtones_loaded = true;
                st.ringtones_hash = resp.hash;
                st.ringtones = list.clone();
            });
            list
        }
        Ok(tl_enums::account::SavedRingtones::NotModified) => with_state(live, |st| {
            st.ringtones_loaded = true;
            st.ringtones.clone()
        }),
        Err(err) => {
            debug!(error = %err, "account.getSavedRingtones failed");
            cached
        }
    }
}

/// Saves (`unsave == false`) or removes (`unsave == true`) a custom ringtone
/// (`account.saveRingtone`).
pub(crate) fn save_ringtone(live: &LiveSession, id: i64, unsave: bool) -> Result<(), String> {
    let existing = with_state(live, |st| st.ringtones.iter().find(|r| r.id == id).cloned());
    let (access_hash, file_reference) = existing
        .as_ref()
        .map(|r| (r.access_hash, r.file_reference.clone()))
        .unwrap_or((0, Vec::new()));

    let resp = invoke_rpc(
        live,
        tl_fns::account::SaveRingtone {
            id: tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
                id,
                access_hash,
                file_reference,
            }),
            unsave,
        },
    )?;

    with_state(live, |st| {
        if unsave {
            st.ringtones.retain(|r| r.id != id);
            st.ringtones_hash = 0;
        } else if let tl_enums::account::SavedRingtone::Converted(converted) = resp
            && let Some(parsed) = ringtone_from_tl_document(&converted.document)
        {
            st.ringtones.retain(|r| r.id != id && r.id != parsed.id);
            st.ringtones.insert(0, parsed);
            st.ringtones_hash = 0;
        }
    });

    Ok(())
}

/// Uploads a custom notification ringtone via `upload.saveFilePart` + `account.uploadRingtone`.
pub(crate) fn upload_ringtone(
    live: &LiveSession,
    file_name: &str,
    mime_type: &str,
    content: &[u8],
) -> Result<SavedRingtone, String> {
    if content.is_empty() {
        return Err("Selected audio file is empty.".to_owned());
    }
    if content.len() > DEFAULT_RINGTONE_MAX_SIZE {
        return Err(format!(
            "Ringtone exceeds the {} KB limit.",
            DEFAULT_RINGTONE_MAX_SIZE / 1024
        ));
    }

    let trimmed_name = if file_name.trim().is_empty() {
        "ringtone.mp3".to_owned()
    } else {
        file_name.trim().to_owned()
    };
    let trimmed_mime = if mime_type.trim().is_empty() {
        "audio/mpeg".to_owned()
    } else {
        mime_type.trim().to_owned()
    };

    let file_id = NEXT_UPLOAD_FILE_ID.fetch_add(1, Ordering::Relaxed);
    let mut part_count: i32 = 0;
    for (idx, chunk) in content.chunks(RINGTONE_UPLOAD_PART_SIZE).enumerate() {
        let saved = invoke_rpc(
            live,
            tl_fns::upload::SaveFilePart {
                file_id,
                file_part: idx as i32,
                bytes: chunk.to_vec(),
            },
        )?;
        if !saved {
            return Err(format!("Server rejected ringtone upload part {idx}."));
        }
        part_count += 1;
    }

    let doc = invoke_rpc(
        live,
        tl_fns::account::UploadRingtone {
            file: tl_enums::InputFile::InputFile(tl_types::InputFile {
                id: file_id,
                parts: part_count,
                name: trimmed_name.clone(),
                md5_checksum: String::new(),
            }),
            file_name: trimmed_name,
            mime_type: trimmed_mime,
        },
    )?;

    let parsed = ringtone_from_tl_document(&doc)
        .ok_or_else(|| "Server returned an empty ringtone document.".to_owned())?;

    with_state(live, |st| {
        st.ringtones.retain(|r| r.id != parsed.id);
        st.ringtones.insert(0, parsed.clone());
        st.ringtones_loaded = true;
        st.ringtones_hash = 0;
    });

    Ok(parsed)
}
