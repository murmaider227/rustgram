//! WP-66 Live RPC implementations for the 18 missing contacts, spam-bar, contact-token,
//! fee-waiver token, birthdays, contact notes, profile-photo delete/contact-photo, and
//! `help.hidePromoData` RPCs.
//!
//! Port origin (Telegram Desktop, Layer 233):
//! - `Telegram/SourceFiles/boxes/contacts_box.cpp`, `boxes/peers/edit_contact_box.cpp` (`contacts.deleteContacts`)
//! - `Telegram/SourceFiles/history/view/history_view_contact_status.cpp`
//!   (`contacts.acceptContact`, `messages.reportSpam`, `messages.hidePeerSettingsBar`)
//! - `Telegram/SourceFiles/info/profile/info_profile_qr_link.cpp` (`contacts.exportContactToken`)
//! - `Telegram/SourceFiles/window/window_session_controller_contact.cpp` (`contacts.importContactToken`)
//! - `Telegram/SourceFiles/api/api_premium.cpp`
//!   (`contacts.exportNoPaidMessagesToken`, `contacts.importNoPaidMessagesToken`)
//! - `Telegram/SourceFiles/info/profile/info_profile_actions.cpp` (`contacts.updateContactNote`)
//! - `Telegram/SourceFiles/data/components/promo_suggestions.cpp` (`contacts.getBirthdays`)
//! - `Telegram/SourceFiles/window/window_peer_menu.cpp` (`contacts.blockFromReplies`, `help.hidePromoData`)
//! - `Telegram/SourceFiles/api/api_stories.cpp` (`contacts.setBlocked`)
//! - `Telegram/SourceFiles/data/components/top_peers.cpp` (`contacts.toggleTopPeers`)
//! - `Telegram/SourceFiles/apiwrap.cpp` (`contacts.getStatuses`)
//! - `Telegram/SourceFiles/api/api_peer_search.cpp` (`contacts.getSponsoredPeers`)
//! - `Telegram/SourceFiles/api/api_peer_photo.cpp` (`photos.deletePhotos`, `photos.uploadContactProfilePhoto`)

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::contacts_extras::{
    apply_contact_birthdays, apply_contact_statuses, apply_deleted_contacts_local,
    apply_deleted_photos_local, apply_sponsored_peers, apply_uploaded_contact_photo_local,
    clear_peer_bar_settings,
};
use tg_api::convert::{apply_tl_user, input_peer_for, input_user_for, text_with_entities_to_tl};
use tg_data::{
    Peer, PeerId as DataPeerId, PeerUpdateFlags, Session, TextWithEntities as DataText, UserFlags,
};
use tg_text::TextWithEntities as UiText;
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::{
    data_text_to_ui_text, from_data_peer, now_unix_i32, peer_display_title, peer_primary_username,
    peer_status_text, to_data_peer, ui_text_to_data_text,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use crate::settings_sync::upload_file_bytes;
use tg_app_core::core_types::dto::contacts_extras::{
    BlockFromRepliesRequest, ContactBirthdayEntry, ContactProfilePhotoMode,
    ContactProfilePhotoUpload, ContactStatusInfo, ExportedContactTokenInfo,
    ExportedNoPaidMessagesTokenInfo, NoPaidMessagesTokenImportResult, SponsoredPeerInfo,
};
use tg_app_core::info::backend::ProfilePhotoRef;
use tg_app_core::info::values::Birthday as UiBirthday;
use tg_app_core::stand_in::types::PeerId;

/// Default timeout for synchronous WP-66 RPC calls.
const CONTACTS_EXTRAS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Seconds per UTC calendar day (for per-day `contacts.getBirthdays` caching,
/// matching `PromoSuggestions::_contactBirthdaysLastDayRequest` in `promo_suggestions.cpp:285`).
const SECS_PER_DAY: i32 = 86_400;

#[derive(Default)]
struct ContactsExtrasState {
    birthdays_cached_day: Option<i32>,
    birthdays: Vec<ContactBirthdayEntry>,
}

struct StateEntry {
    owner: Weak<LiveBridgeState>,
    state: ContactsExtrasState,
}

fn state_registry() -> &'static Mutex<HashMap<usize, StateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, StateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_state<R>(live: &LiveSession, f: impl FnOnce(&mut ContactsExtrasState) -> R) -> R {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = state_registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| StateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: ContactsExtrasState::default(),
    });
    f(&mut entry.state)
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

fn invoke_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(CONTACTS_EXTRAS_RPC_TIMEOUT, async move {
            rpc.invoke(&req).await
        })
        .await
    })
}

/// Applies an `Updates` reply through `SessionRuntime`'s actor (`UpdatesHandler`), matching
/// `brief_wave11.md` and `ApiWrap::applyUpdates`.
fn apply_updates_via_actor(live: &LiveSession, updates: tl_enums::Updates) {
    let runtime = Arc::clone(live.runtime());
    if let Err(err) = block_on_runtime(live.runtime().tokio_handle(), async move {
        runtime.apply_updates(updates).await
    }) {
        tracing::debug!(error = %err, "SessionRuntime::apply_updates failed");
    }
    live.bridge_state().notify_ui();
}

fn resolve_input_user(live: &LiveSession, peer_id: PeerId) -> Result<tl_enums::InputUser, String> {
    let Some(uid) = to_data_peer(peer_id).to_user() else {
        return Err(format!("Peer {} is not a user.", peer_id.0));
    };
    let iu = read_session(live, |s| input_user_for(s, uid));
    if matches!(iu, tl_enums::InputUser::Empty) {
        return Err(format!("User {} is not loaded with an access hash.", uid.0));
    }
    Ok(iu)
}

fn resolve_input_peer(live: &LiveSession, peer_id: PeerId) -> Result<tl_enums::InputPeer, String> {
    let ip = read_session(live, |s| input_peer_for(s, to_data_peer(peer_id)));
    if matches!(ip, tl_enums::InputPeer::Empty) {
        return Err(format!("Peer {} is not loaded.", peer_id.0));
    }
    Ok(ip)
}

// =============================================================================
// 1. Delete Contacts (`contacts.deleteContacts`)
// =============================================================================

/// Deletes one or more contacts via `contacts.deleteContacts`
/// (`boxes/contacts_box.cpp:325`, `boxes/peers/edit_contact_box.cpp:762`).
pub(crate) fn delete_contacts(live: &mut LiveSession, peer_ids: &[PeerId]) -> Result<(), String> {
    if peer_ids.is_empty() {
        return Err("At least one contact is required.".to_owned());
    }
    let mut user_ids = Vec::with_capacity(peer_ids.len());
    let mut inputs = Vec::with_capacity(peer_ids.len());
    for &pid in peer_ids {
        let Some(uid) = to_data_peer(pid).to_user() else {
            return Err(format!("Peer {} is not a user.", pid.0));
        };
        inputs.push(resolve_input_user(live, pid)?);
        user_ids.push(uid);
    }

    let updates = invoke_rpc(live, tl_fns::contacts::DeleteContacts { id: inputs })?;
    apply_updates_via_actor(live, updates);
    write_session(live, |s| {
        apply_deleted_contacts_local(s, &user_ids);
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// =============================================================================
// 2. Contact Status Bar: `contacts.acceptContact`, `messages.reportSpam`,
//    `messages.hidePeerSettingsBar`
// =============================================================================

/// Accepts a non-contact / shares own phone number via `contacts.acceptContact`
/// (`history/view/history_view_contact_status.cpp:916-925`), clearing `PeerBarSettings` and
/// feeding returned `Updates` through `SessionRuntime::apply_updates`.
pub(crate) fn accept_contact(live: &mut LiveSession, peer_id: PeerId) -> Result<(), String> {
    let id = resolve_input_user(live, peer_id)?;
    let data_peer = to_data_peer(peer_id);
    write_session(live, |s| clear_peer_bar_settings(s, data_peer));
    if let Ok(mut c) = live.cache.lock() {
        c.dismissed_contact_banners.insert(peer_id);
    }

    let updates = invoke_rpc(live, tl_fns::contacts::AcceptContact { id })?;
    apply_updates_via_actor(live, updates);
    // Ensure bar settings stay cleared even if updates omitted `updatePeerSettings`.
    write_session(live, |s| clear_peer_bar_settings(s, data_peer));
    live.bridge_state().notify_ui();
    Ok(())
}

/// Reports a peer for spam via `messages.reportSpam`
/// (`history/view/history_view_contact_status.cpp:967-969`, `window/window_peer_menu.cpp:2858`)
/// and clears `PeerBarSettings`.
pub(crate) fn report_spam(live: &mut LiveSession, peer_id: PeerId) -> Result<bool, String> {
    let peer = resolve_input_peer(live, peer_id)?;
    let ok = invoke_rpc(live, tl_fns::messages::ReportSpam { peer })?;
    let data_peer = to_data_peer(peer_id);
    write_session(live, |s| clear_peer_bar_settings(s, data_peer));
    if let Ok(mut c) = live.cache.lock() {
        c.dismissed_contact_banners.insert(peer_id);
    }
    live.bridge_state().notify_ui();
    Ok(ok)
}

/// Hides the peer settings / contact status bar via `messages.hidePeerSettingsBar`
/// (`history/view/history_view_contact_status.cpp:1014-1017, 1048-1051`)
/// and clears `PeerBarSettings`.
pub(crate) fn hide_peer_settings_bar(
    live: &mut LiveSession,
    peer_id: PeerId,
) -> Result<bool, String> {
    let peer = resolve_input_peer(live, peer_id)?;
    let data_peer = to_data_peer(peer_id);
    write_session(live, |s| clear_peer_bar_settings(s, data_peer));
    if let Ok(mut c) = live.cache.lock() {
        c.dismissed_contact_banners.insert(peer_id);
    }
    let ok = invoke_rpc(live, tl_fns::messages::HidePeerSettingsBar { peer })?;
    live.bridge_state().notify_ui();
    Ok(ok)
}

// =============================================================================
// 3. Contact Tokens & Paid-Messages Fee-Waiver Tokens
// =============================================================================

/// Exports a temporary contact QR / deep-link token via `contacts.exportContactToken`
/// (`info/profile/info_profile_qr_link.cpp:197-232`).
pub(crate) fn export_contact_token(live: &LiveSession) -> Result<ExportedContactTokenInfo, String> {
    let tl_enums::ExportedContactToken::ExportedContactToken(resp) =
        invoke_rpc(live, tl_fns::contacts::ExportContactToken {})?;
    if resp.url.is_empty() {
        return Err("Contact link is currently unavailable.".to_owned());
    }
    Ok(ExportedContactTokenInfo {
        url: resp.url,
        expires: resp.expires,
    })
}

/// Resolves a temporary contact token via `contacts.importContactToken`
/// (`window/window_session_controller_contact.cpp:18-54`).
pub(crate) fn import_contact_token(live: &mut LiveSession, token: &str) -> Result<PeerId, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("Contact link token is empty.".to_owned());
    }
    let user_tl = invoke_rpc(
        live,
        tl_fns::contacts::ImportContactToken {
            token: trimmed.to_owned(),
        },
    )?;
    let resolved = write_session(live, |s| {
        let pid = apply_tl_user(s, &user_tl)?;
        let inaccessible = s
            .user(pid.to_user_or_zero())
            .is_none_or(|u| u.is_inaccessible());
        if inaccessible {
            None
        } else {
            Some(from_data_peer(pid))
        }
    });
    live.bridge_state().notify_ui();
    resolved.ok_or_else(|| "This contact link is invalid or has expired.".to_owned())
}

/// Exports a paid-message fee-waiver token via `contacts.exportNoPaidMessagesToken`
/// (`api/api_premium.cpp:530-548`).
pub(crate) fn export_no_paid_messages_token(
    live: &LiveSession,
    revoke: bool,
    temporary: bool,
) -> Result<ExportedNoPaidMessagesTokenInfo, String> {
    let tl_enums::ExportedNoPaidMessagesToken::ExportedNoPaidMessagesToken(resp) = invoke_rpc(
        live,
        tl_fns::contacts::ExportNoPaidMessagesToken { revoke, temporary },
    )?;
    Ok(ExportedNoPaidMessagesTokenInfo {
        token: resp.token,
        url: resp.url,
        expires: resp.expires,
    })
}

/// Redeems a paid-message fee-waiver token via `contacts.importNoPaidMessagesToken`
/// (`api/api_premium.cpp:487-528`).
pub(crate) fn import_no_paid_messages_token(
    live: &mut LiveSession,
    peer_id: PeerId,
    token: &str,
) -> Result<NoPaidMessagesTokenImportResult, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("Fee-waiver token is empty.".to_owned());
    }
    let Some(uid) = to_data_peer(peer_id).to_user() else {
        return Err(format!("Peer {} is not a user.", peer_id.0));
    };
    let (input_user, priced_before) = read_session(live, |s| {
        let iu = input_user_for(s, uid);
        let priced = s.user(uid).is_some_and(|u| {
            u.flags.contains(UserFlags::HAS_STARS_PER_MESSAGE) || u.stars_per_message > 0
        });
        (iu, priced)
    });
    if matches!(input_user, tl_enums::InputUser::Empty) {
        return Err(format!("User {} is not loaded with an access hash.", uid.0));
    }

    let user_tl = invoke_rpc(
        live,
        tl_fns::contacts::ImportNoPaidMessagesToken {
            user_id: input_user,
            token: trimmed.to_owned(),
        },
    )?;

    let (resolved_pid, priced_after) = write_session(live, |s| {
        let Some(pid) = apply_tl_user(s, &user_tl) else {
            return (None, false);
        };
        let priced = s.user(pid.to_user_or_zero()).is_some_and(|u| {
            u.flags.contains(UserFlags::HAS_STARS_PER_MESSAGE) || u.stars_per_message > 0
        });
        (Some(from_data_peer(pid)), priced)
    });
    live.bridge_state().notify_ui();

    let user_id = resolved_pid
        .ok_or_else(|| "Server returned an empty user for fee-waiver token.".to_owned())?;
    Ok(NoPaidMessagesTokenImportResult {
        user_id,
        price_removed: priced_before && !priced_after,
    })
}

// =============================================================================
// 4. Contact Note (`contacts.updateContactNote`)
// =============================================================================

/// Returns the cached private contact note for `peer_id`, if non-empty.
pub(crate) fn contact_note(live: &LiveSession, peer_id: PeerId) -> Option<UiText> {
    let uid = to_data_peer(peer_id).to_user()?;
    read_session(live, |s| {
        let user = s.user(uid)?;
        (!user.note.text.is_empty()).then(|| data_text_to_ui_text(&user.note))
    })
}

/// Updates or clears (when `note.text.is_empty()`) a private contact note via
/// `contacts.updateContactNote` (`info/profile/info_profile_actions.cpp:789-802`).
pub(crate) fn update_contact_note(
    live: &mut LiveSession,
    peer_id: PeerId,
    note: UiText,
) -> Result<bool, String> {
    let Some(uid) = to_data_peer(peer_id).to_user() else {
        return Err(format!("Peer {} is not a user.", peer_id.0));
    };
    let data_note = if note.text.is_empty() {
        DataText::default()
    } else {
        ui_text_to_data_text(&note)
    };
    let (id, tl_note) = read_session(live, |s| {
        let iu = input_user_for(s, uid);
        let tl = if data_note.text.is_empty() {
            tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: String::new(),
                entities: Vec::new(),
            })
        } else {
            text_with_entities_to_tl(&data_note, s)
        };
        (iu, tl)
    });
    if matches!(id, tl_enums::InputUser::Empty) {
        return Err(format!("User {} is not loaded with an access hash.", uid.0));
    }

    let ok = invoke_rpc(
        live,
        tl_fns::contacts::UpdateContactNote { id, note: tl_note },
    )?;
    write_session(live, |s| {
        if let Some(user) = s.user_mut(uid) {
            user.note = data_note;
            s.changes().peer_updated(
                DataPeerId::from_user(uid),
                PeerUpdateFlags::CONTACT_NOTE | PeerUpdateFlags::FULL_INFO,
            );
        }
    });
    live.bridge_state().notify_ui();
    Ok(ok)
}

// =============================================================================
// 5. Contact Birthdays (`contacts.getBirthdays`)
// =============================================================================

/// Fetches contact birthdays via `contacts.getBirthdays`
/// (`data/components/promo_suggestions.cpp:284-346`), caching per UTC calendar day unless `force`.
pub(crate) fn contact_birthdays(
    live: &LiveSession,
    force: bool,
) -> Result<Vec<ContactBirthdayEntry>, String> {
    let today = now_unix_i32() / SECS_PER_DAY;
    if !force
        && let Some(cached) = with_state(live, |st| {
            (st.birthdays_cached_day == Some(today)).then(|| st.birthdays.clone())
        })
    {
        return Ok(cached);
    }

    let resp = invoke_rpc(live, tl_fns::contacts::GetBirthdays {})?;
    let entries = write_session(live, |s| {
        let parsed = apply_contact_birthdays(s, resp);
        parsed
            .into_iter()
            .map(|cb| {
                let data_pid = DataPeerId::from_user(cb.user_id);
                let display_name = peer_display_title(s, data_pid);
                ContactBirthdayEntry {
                    peer_id: from_data_peer(data_pid),
                    display_name,
                    birthday: UiBirthday {
                        day: cb.birthday.day,
                        month: cb.birthday.month,
                        year: (cb.birthday.year != 0).then_some(cb.birthday.year),
                    },
                }
            })
            .collect::<Vec<_>>()
    });

    with_state(live, |st| {
        st.birthdays_cached_day = Some(today);
        st.birthdays = entries.clone();
    });
    live.bridge_state().notify_ui();
    Ok(entries)
}

// =============================================================================
// 6. Block From Replies (`contacts.blockFromReplies`) & Bulk SetBlocked (`contacts.setBlocked`)
// =============================================================================

/// Blocks a sender from the Replies chat via `contacts.blockFromReplies`
/// (`window/window_peer_menu.cpp:2845-2854`) and feeds returned `Updates` through `SessionRuntime`.
pub(crate) fn block_from_replies(
    live: &mut LiveSession,
    request: BlockFromRepliesRequest,
) -> Result<(), String> {
    if request.msg_id.0 <= 0 {
        return Err("block_from_replies requires a positive message ID.".to_owned());
    }
    let updates = invoke_rpc(
        live,
        tl_fns::contacts::BlockFromReplies {
            delete_message: request.delete_message,
            delete_history: request.delete_history,
            report_spam: request.report_spam,
            msg_id: request.msg_id.0,
        },
    )?;
    apply_updates_via_actor(live, updates);
    Ok(())
}

/// Bulk-replaces the hidden-stories blocklist (`my_stories_from = true`,
/// `api/api_stories.cpp:595-621`) or general blocked-peers list (`my_stories_from = false`)
/// via `contacts.setBlocked`.
pub(crate) fn set_blocked_peers(
    live: &mut LiveSession,
    peers: &[DataPeerId],
    my_stories_from: bool,
) -> Result<bool, String> {
    let prev_count = if my_stories_from {
        crate::stories::blocked_viewers_count(live)
    } else {
        live.cache
            .lock()
            .map(|c| c.blocked_peers.len())
            .unwrap_or(0)
    };
    let (ids, resolved_peers) = read_session(live, |s| {
        let mut ids = Vec::with_capacity(peers.len());
        let mut resolved = Vec::with_capacity(peers.len());
        for &pid in peers {
            let ip = input_peer_for(s, pid);
            if !matches!(ip, tl_enums::InputPeer::Empty) {
                ids.push(ip);
                resolved.push(pid);
            }
        }
        (ids, resolved)
    });
    let limit = prev_count.max(ids.len()) as i32;

    let ok = invoke_rpc(
        live,
        tl_fns::contacts::SetBlocked {
            my_stories_from,
            id: ids,
            limit,
        },
    )?;

    if ok {
        if my_stories_from {
            crate::stories::sync_blocked_viewers(live, &resolved_peers);
        } else {
            write_session(live, |s| {
                for &pid in &resolved_peers {
                    s.set_peer_blocked(pid, true);
                }
            });
            if let Ok(mut c) = live.cache.lock() {
                c.blocked_loaded = false;
            }
        }
        live.bridge_state().notify_ui();
    }
    Ok(ok)
}

// =============================================================================
// 7. Top Peers Toggle (`contacts.toggleTopPeers`), Statuses (`contacts.getStatuses`),
//    Sponsored Peers (`contacts.getSponsoredPeers`)
// =============================================================================

/// Enables or disables top-peers suggestions via `contacts.toggleTopPeers`
/// (`data/components/top_peers.cpp:195-216`) and syncs `search_and_info`'s top-peers cache.
pub(crate) fn toggle_top_peers(live: &mut LiveSession, enabled: bool) -> Result<bool, String> {
    crate::search_and_info::set_top_peers_enabled_state(live, enabled);
    let ok = invoke_rpc(live, tl_fns::contacts::ToggleTopPeers { enabled })?;
    live.bridge_state().notify_ui();
    Ok(ok)
}

/// Refreshes online/last-seen statuses for all contacts via `contacts.getStatuses`
/// (`apiwrap.cpp:2210-2229`) and updates every loaded user in `Session`.
pub(crate) fn refresh_contact_statuses(
    live: &mut LiveSession,
) -> Result<Vec<ContactStatusInfo>, String> {
    let statuses = invoke_rpc(live, tl_fns::contacts::GetStatuses {})?;
    let now = now_unix_i32();
    let out = write_session(live, |s| {
        let updates = apply_contact_statuses(s, &statuses);
        updates
            .into_iter()
            .map(|u| {
                let data_pid = DataPeerId::from_user(u.user_id);
                let (status_text, is_online) = s
                    .peer(data_pid)
                    .map(|p| peer_status_text(p, now))
                    .unwrap_or_else(|| {
                        let online = u.status.online_till() > now;
                        (
                            if online {
                                "online".to_owned()
                            } else {
                                "offline".to_owned()
                            },
                            online,
                        )
                    });
                ContactStatusInfo {
                    peer_id: from_data_peer(data_pid),
                    status_text,
                    is_online,
                    online_till: u.status.online_till(),
                }
            })
            .collect::<Vec<_>>()
    });
    live.bridge_state().notify_ui();
    Ok(out)
}

/// Queries sponsored search peers via `contacts.getSponsoredPeers`
/// (`api/api_peer_search.cpp:100-131`) and hydrates returned users/chats into `Session`.
pub(crate) fn sponsored_peers(
    live: &LiveSession,
    query: &str,
) -> Result<Vec<SponsoredPeerInfo>, String> {
    let resp = invoke_rpc(
        live,
        tl_fns::contacts::GetSponsoredPeers {
            q: query.to_owned(),
        },
    )?;
    let out = write_session(live, |s| {
        let entries = apply_sponsored_peers(s, resp);
        entries
            .into_iter()
            .map(|entry| {
                let (title, username, color_index, verified, premium) = s
                    .peer(entry.peer_id)
                    .map(|p| {
                        let verified = match p {
                            Peer::User(u) => u.is_verified(),
                            Peer::Channel(ch) => ch.is_verified(),
                            Peer::Chat(_) => false,
                        };
                        let premium = match p {
                            Peer::User(u) => u.is_premium(),
                            Peer::Chat(_) | Peer::Channel(_) => false,
                        };
                        (
                            peer_display_title(s, entry.peer_id),
                            peer_primary_username(p),
                            p.common().color_index % 7,
                            verified,
                            premium,
                        )
                    })
                    .unwrap_or_else(|| (String::new(), None, 0, false, false));
                SponsoredPeerInfo {
                    peer_id: from_data_peer(entry.peer_id),
                    random_id: entry.random_id,
                    title,
                    username,
                    sponsor_info: entry.sponsor_info,
                    additional_info: entry.additional_info,
                    color_index,
                    verified,
                    premium,
                }
            })
            .collect::<Vec<_>>()
    });
    live.bridge_state().notify_ui();
    Ok(out)
}

// =============================================================================
// 8. Profile Photos: `photos.deletePhotos` & `photos.uploadContactProfilePhoto`
// =============================================================================

/// Deletes one or more of the signed-in account's own profile photos via `photos.deletePhotos`
/// (`api/api_peer_photo.cpp:601-607`).
pub(crate) fn delete_profile_photos(
    live: &mut LiveSession,
    photos: &[ProfilePhotoRef],
) -> Result<Vec<i64>, String> {
    if photos.is_empty() {
        return Ok(Vec::new());
    }
    let id: Vec<tl_enums::InputPhoto> = photos
        .iter()
        .map(|p| {
            tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                id: p.id,
                access_hash: p.access_hash,
                file_reference: p.file_reference.clone(),
            })
        })
        .collect();
    let deleted = invoke_rpc(live, tl_fns::photos::DeletePhotos { id })?;
    write_session(live, |s| apply_deleted_photos_local(s, &deleted));
    live.bridge_state().notify_ui();
    Ok(deleted)
}

/// Uploads, suggests, or clears a personal contact profile photo via
/// `photos.uploadContactProfilePhoto` (`api/api_peer_photo.cpp:611-631, 770-796`).
pub(crate) fn upload_contact_profile_photo(
    live: &mut LiveSession,
    peer_id: PeerId,
    request: ContactProfilePhotoUpload,
) -> Result<i64, String> {
    let Some(uid) = to_data_peer(peer_id).to_user() else {
        return Err(format!("Peer {} is not a user.", peer_id.0));
    };
    let user_id = resolve_input_user(live, peer_id)?;

    let (suggest, save) = match request.mode {
        ContactProfilePhotoMode::Suggest => (true, false),
        ContactProfilePhotoMode::Save => (false, true),
    };
    let is_clear = save
        && request.image.is_none()
        && request.video.is_none()
        && request.emoji_markup.is_none();
    if suggest
        && request.image.is_none()
        && request.video.is_none()
        && request.emoji_markup.is_none()
    {
        return Err(
            "Suggesting a contact profile photo requires an image, video, or emoji markup."
                .to_owned(),
        );
    }

    let file = match &request.image {
        Some((name, bytes)) => Some(upload_file_bytes(live, name, bytes)?),
        None => None,
    };
    let video = match &request.video {
        Some((name, bytes)) => Some(upload_file_bytes(live, name, bytes)?),
        None => None,
    };
    let has_video = video.is_some();
    let video_emoji_markup = request.emoji_markup.map(|m| {
        let colors: Vec<i32> = m
            .background_colors
            .into_iter()
            .take(4)
            .map(|c| (c & 0x00FF_FFFF) as i32)
            .collect();
        tl_enums::VideoSize::EmojiMarkup(tl_types::VideoSizeEmojiMarkup {
            emoji_id: m.emoji_id,
            background_colors: colors,
        })
    });

    let req = tl_fns::photos::UploadContactProfilePhoto {
        suggest,
        save,
        user_id,
        file,
        video,
        // Match `api/api_peer_photo.cpp:776`: only set `video_start_ts` flag when `video` is present.
        video_start_ts: if has_video {
            request.video_start_ts
        } else {
            None
        },
        video_emoji_markup,
    };
    let resp = invoke_rpc(live, req)?;
    let photo_id = write_session(live, |s| {
        apply_uploaded_contact_photo_local(s, uid, save, is_clear, has_video, resp)
    });
    live.bridge_state().notify_ui();
    Ok(photo_id)
}

// =============================================================================
// 9. Hide Promo Data (`help.hidePromoData`)
// =============================================================================

/// Hides a promoted / PSA peer from the dialog list via `help.hidePromoData`
/// (`window/window_peer_menu.cpp:525-530`) and clears it from `LivePromoState`.
pub(crate) fn hide_promo_data(live: &mut LiveSession, peer_id: PeerId) -> Result<bool, String> {
    let peer = resolve_input_peer(live, peer_id)?;
    let ok = invoke_rpc(live, tl_fns::help::HidePromoData { peer })?;
    if let Ok(mut guard) = live.promo_state.lock() {
        guard.hide_promo_peer(peer_id);
    }
    live.bridge_state().notify_ui();
    Ok(ok)
}
