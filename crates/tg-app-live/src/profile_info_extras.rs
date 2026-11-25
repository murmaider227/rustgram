//! Live `InfoBackend` helpers for:
//! - `personal_channel` (session cache + `users.getFullUser` hydration for `personal_channel_id`)
//! - `personal_channel_candidates` (`channels.getAdminedPublicChannels { for_personal: true }` +
//!   session admin channels cache fallback)
//! - `peer_profile_balance` (reusing the Stars/TON balance loaded by the payments layer or
//!   hydrating once via `payments.getStarsStatus`)

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{
    apply_tl_chat_full, apply_tl_user_full, apply_tl_users_and_chats, input_channel_for,
    input_peer_for, input_user_for, photo_from_tl,
};
use tg_data::{
    ChannelFlags, ChannelId, LoadedStatus, PeerId as DataPeerId, PhotoData, PhotoId, Session,
    UNKNOWN_PHOTO_ID,
};
use tg_payments::credits::{build_get_stars_status, stars_status_from_tl};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::{from_data_peer, known_peer_ids, now_unix_i32, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::payments::{
    cached_self_profile_balance, record_self_stars_balance, record_self_ton_balance,
};
use crate::session::LiveSession;
use tg_app_core::info::backend::{PersonalChannelOption, ProfileBalanceInfo, ProfilePhotoRef};
use tg_app_core::stand_in::types::PeerId;

const PROFILE_INFO_RPC_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct ProfileInfoExtrasState {
    fetched_full_users: HashSet<PeerId>,
    fetched_photo_full_peers: HashSet<(PeerId, PhotoId)>,
    fetched_user_photos: HashSet<(PeerId, PhotoId)>,
    candidates_loaded: bool,
    candidates: Vec<PersonalChannelOption>,
    balance_attempted: HashSet<PeerId>,
    peer_balances: HashMap<PeerId, ProfileBalanceInfo>,
}

struct StateEntry {
    owner: Weak<LiveBridgeState>,
    state: ProfileInfoExtrasState,
}

fn registry() -> &'static Mutex<HashMap<usize, StateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, StateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_state<R>(live: &LiveSession, f: impl FnOnce(&mut ProfileInfoExtrasState) -> R) -> R {
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| StateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: ProfileInfoExtrasState::default(),
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
        with_rpc_timeout(
            PROFILE_INFO_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
}

fn channel_option_from_session(session: &Session, cid: ChannelId) -> Option<PersonalChannelOption> {
    let ch = session.channel(cid)?;
    let uname = ch.usernames.username();
    Some(PersonalChannelOption {
        peer_id: from_data_peer(DataPeerId::from_channel(cid)),
        title: if ch.common.name.is_empty() {
            format!("Channel {}", cid.0)
        } else {
            ch.common.name.clone()
        },
        username: (!uname.is_empty()).then(|| uname.to_owned()),
        subscribers_count: ch.members_count.max(0) as u32,
    })
}

/// Returns the personal channel linked on `peer_id`'s profile, hydrating once via
/// `users.getFullUser` if `personal_channel_id` (or its channel metadata) is not yet cached.
pub(crate) fn personal_channel(
    live: &LiveSession,
    peer_id: PeerId,
) -> Option<PersonalChannelOption> {
    let uid = to_data_peer(peer_id).to_user()?;

    let (cached_cid, cached_opt, input_user) = read_session(live, |s| {
        let cid = s.user(uid).and_then(|u| u.personal_channel_id);
        let opt = cid.and_then(|c| channel_option_from_session(s, c));
        let iu = if s.self_user_id() == uid {
            tl_enums::InputUser::Self_
        } else {
            let resolved = input_user_for(s, uid);
            if matches!(resolved, tl_enums::InputUser::Empty) {
                tl_enums::InputUser::InputUser(tl_types::InputUser {
                    user_id: uid.to_tl(),
                    access_hash: 0,
                })
            } else {
                resolved
            }
        };
        (cid, opt, iu)
    });

    if cached_opt.is_some() {
        return cached_opt;
    }

    let should_fetch = with_state(live, |st| st.fetched_full_users.insert(peer_id));
    if should_fetch
        && let Ok(tl_enums::users::UserFull::UserFull(uf)) =
            invoke_rpc(live, tl_fns::users::GetFullUser { id: input_user })
    {
        write_session(live, |s| {
            apply_tl_users_and_chats(s, &uf.users, &uf.chats);
            apply_tl_user_full(s, &uf.full_user);
        });
    }

    read_session(live, |s| {
        let cid = s
            .user(uid)
            .and_then(|u| u.personal_channel_id)
            .or(cached_cid)?;
        Some(
            channel_option_from_session(s, cid).unwrap_or_else(|| PersonalChannelOption {
                peer_id: from_data_peer(DataPeerId::from_channel(cid)),
                title: format!("Channel {}", cid.0),
                username: None,
                subscribers_count: 0,
            }),
        )
    })
}

/// Returns the user's own broadcast channels eligible as a personal channel.
///
/// Queries `channels.getAdminedPublicChannels { for_personal: true }` once and caches the result,
/// falling back to creator/admin broadcast channels already cached in `Session`.
pub(crate) fn personal_channel_candidates(live: &LiveSession) -> Vec<PersonalChannelOption> {
    if let Some(cached) = with_state(live, |st| {
        st.candidates_loaded.then(|| st.candidates.clone())
    }) {
        return cached;
    }

    let fetched = match invoke_rpc(
        live,
        tl_fns::channels::GetAdminedPublicChannels {
            by_location: false,
            check_limit: false,
            for_personal: true,
            for_community_peer: false,
        },
    ) {
        Ok(resp) => {
            let chats = match resp {
                tl_enums::messages::Chats::Chats(c) => c.chats,
                tl_enums::messages::Chats::Slice(c) => c.chats,
            };
            write_session(live, |s| {
                apply_tl_users_and_chats(s, &[], &chats);
            });
            let mut out = Vec::new();
            for chat in chats {
                if let tl_enums::Chat::Channel(ch) = chat
                    && !ch.megagroup
                    && !ch.gigagroup
                {
                    let cid = ChannelId::from_tl(ch.id);
                    let username = ch.username.filter(|u| !u.is_empty()).or_else(|| {
                        ch.usernames.and_then(|list| {
                            list.into_iter()
                                .find_map(|tl_enums::Username::Username(u)| {
                                    (u.active && !u.username.is_empty()).then_some(u.username)
                                })
                        })
                    });
                    out.push(PersonalChannelOption {
                        peer_id: from_data_peer(DataPeerId::from_channel(cid)),
                        title: if ch.title.is_empty() {
                            format!("Channel {}", cid.0)
                        } else {
                            ch.title
                        },
                        username,
                        subscribers_count: ch.participants_count.unwrap_or(0).max(0) as u32,
                    });
                }
            }
            Some(out)
        }
        Err(_) => None,
    };

    let candidates = fetched.unwrap_or_else(|| {
        read_session(live, |s| {
            let mut out = Vec::new();
            for pid in known_peer_ids(s) {
                let Some(cid) = pid.to_channel() else {
                    continue;
                };
                let Some(ch) = s.channel(cid) else {
                    continue;
                };
                if ch.is_megagroup() || ch.is_gigagroup() {
                    continue;
                }
                if !ch.flags.contains(ChannelFlags::CREATOR) && ch.admin_rights.is_empty() {
                    continue;
                }
                if let Some(opt) = channel_option_from_session(s, cid) {
                    out.push(opt);
                }
            }
            out
        })
    });

    with_state(live, |st| {
        st.candidates_loaded = true;
        st.candidates = candidates.clone();
    });
    candidates
}

/// Returns the Stars / TON balance summary for `peer_id` (`info_profile_balance_pill.cpp`).
///
/// For the signed-in account, reuses the Stars and TON balances already loaded by `payments.rs`
/// (`PaymentsBackend::load_credits_overview`, `load_star_gifts`, etc.), hydrating once via
/// `payments.getStarsStatus` (`ton: false` and `ton: true`) when not yet populated.
/// For owned/admined channels, loads and caches `payments.getStarsStatus` for that channel peer.
pub(crate) fn peer_profile_balance(
    live: &LiveSession,
    peer_id: PeerId,
) -> Option<ProfileBalanceInfo> {
    let data_peer = to_data_peer(peer_id);
    let (self_pid, is_self, admin_channel_input) = read_session(live, |s| {
        let self_pid = s.self_peer_id();
        let is_self = data_peer == self_pid;
        let admin_ch = if is_self {
            None
        } else {
            data_peer.to_channel().and_then(|cid| {
                let ch = s.channel(cid)?;
                let eligible = !ch.is_megagroup()
                    && !ch.is_gigagroup()
                    && (ch.flags.contains(ChannelFlags::CREATOR) || !ch.admin_rights.is_empty());
                if !eligible {
                    return None;
                }
                let ip = input_peer_for(s, data_peer);
                (!matches!(ip, tl_enums::InputPeer::Empty)).then_some(ip)
            })
        };
        (self_pid, is_self, admin_ch)
    });

    if is_self {
        if let Some(bal) = cached_self_profile_balance(live) {
            return (bal.ton_nanos > 0 || bal.stars_balance > 0).then_some(bal);
        }
        let should_fetch = with_state(live, |st| st.balance_attempted.insert(peer_id));
        if should_fetch {
            let now = now_unix_i32();
            if let Ok(stars_tl) = invoke_rpc(
                live,
                build_get_stars_status(tl_enums::InputPeer::Self_, false),
            ) {
                let bal = stars_status_from_tl(&stars_tl, self_pid, now).balance;
                record_self_stars_balance(live, bal);
            }
            if let Ok(ton_tl) = invoke_rpc(
                live,
                build_get_stars_status(tl_enums::InputPeer::Self_, true),
            ) {
                let bal = stars_status_from_tl(&ton_tl, self_pid, now).balance;
                record_self_ton_balance(live, bal);
            }
        }
        return cached_self_profile_balance(live)
            .filter(|bal| bal.ton_nanos > 0 || bal.stars_balance > 0);
    }

    if let Some(cached) = with_state(live, |st| st.peer_balances.get(&peer_id).copied()) {
        return (cached.ton_nanos > 0 || cached.stars_balance > 0).then_some(cached);
    }

    let channel_ip = admin_channel_input?;
    let should_fetch = with_state(live, |st| st.balance_attempted.insert(peer_id));
    if !should_fetch {
        return None;
    }

    let now = now_unix_i32();
    let stars_balance = invoke_rpc(live, build_get_stars_status(channel_ip.clone(), false))
        .ok()
        .map(|tl| {
            stars_status_from_tl(&tl, data_peer, now)
                .balance
                .whole()
                .max(0)
        })
        .unwrap_or(0);
    let ton_nanos = invoke_rpc(live, build_get_stars_status(channel_ip, true))
        .ok()
        .map(|tl| {
            let amt = stars_status_from_tl(&tl, data_peer, now).balance;
            amt.whole()
                .saturating_mul(1_000_000_000)
                .saturating_add(amt.nano())
                .max(0)
        })
        .unwrap_or(0);
    let info = ProfileBalanceInfo {
        ton_nanos,
        stars_balance,
    };
    with_state(live, |st| {
        st.peer_balances.insert(peer_id, info);
    });
    (info.ton_nanos > 0 || info.stars_balance > 0).then_some(info)
}

/// Validates that a [`ProfilePhotoRef`] carries a real non-zero server photo ID and
/// access hash (never a bogus placeholder fabricated from `peer_id` with `access_hash == 0`).
pub(crate) fn validate_profile_photo_ref(photo: &ProfilePhotoRef) -> Result<(), String> {
    if photo.id == 0 {
        return Err("Profile photo ID cannot be zero.".to_owned());
    }
    if photo.access_hash == 0 {
        return Err(
            "Profile photo reference is missing a valid server access_hash (cannot send placeholder InputPhoto)."
                .to_owned(),
        );
    }
    Ok(())
}

fn photo_data_to_profile_ref(photo: &PhotoData) -> Option<ProfilePhotoRef> {
    let id = photo.id.to_tl();
    let access_hash = photo.access_hash as i64;
    if id == 0 || access_hash == 0 {
        return None;
    }
    Some(ProfilePhotoRef {
        id,
        access_hash,
        file_reference: photo.file_reference.clone(),
    })
}

/// Resolves the real server [`ProfilePhotoRef`] (`id`, `access_hash`, `file_reference`) for
/// `peer_id`'s currently active profile photo, matching `api/api_peer_photo.cpp`.
///
/// Resolution order:
/// 1. Cached `PhotoData` in `Session` (populated from `UserFull`, `ChatFull`, `ChannelFull`,
///    `photos.getUserPhotos`, or `photos.uploadContactProfilePhoto`).
/// 2. Full-peer hydration (`users.getFullUser`, `messages.getFullChat`, or
///    `channels.getFullChannel`) when full info has not yet been fetched for this photo ID.
/// 3. For users, fallback to `photos.getUserPhotos(offset = 0, max_id = 0, limit = 1)` if a
///    user photo exists or full-user metadata did not yield a cached `PhotoData`.
///
/// Returns `None` when the peer has no active profile photo or no valid server reference is
/// available; never fabricates an `InputPhoto` from `peer_id.bare()`.
pub(crate) fn active_profile_photo_ref(
    live: &LiveSession,
    peer_id: PeerId,
) -> Option<ProfilePhotoRef> {
    let data_peer = to_data_peer(peer_id);
    if data_peer.is_zero() || data_peer.is_fake() {
        return None;
    }

    let (cached, loaded_status, photo_id) = read_session(live, |s| {
        let cached = s
            .peer_active_photo(data_peer)
            .and_then(photo_data_to_profile_ref);
        let (status, pid) = s
            .peer(data_peer)
            .map(|p| (p.common().loaded_status, p.common().userpic_photo_id))
            .unwrap_or((LoadedStatus::Not, UNKNOWN_PHOTO_ID));
        (cached, status, pid)
    });

    if cached.is_some() {
        return cached;
    }
    if loaded_status >= LoadedStatus::Full && photo_id == PhotoId::ZERO {
        return None;
    }

    // Hydrate full peer info once per `(peer_id, photo_id)` if full info is not yet loaded.
    if loaded_status < LoadedStatus::Full
        && with_state(live, |st| {
            st.fetched_photo_full_peers.insert((peer_id, photo_id))
        })
    {
        if let Some(uid) = data_peer.to_user() {
            let input_user = read_session(live, |s| input_user_for(s, uid));
            if !matches!(input_user, tl_enums::InputUser::Empty)
                && let Ok(tl_enums::users::UserFull::UserFull(uf)) =
                    invoke_rpc(live, tl_fns::users::GetFullUser { id: input_user })
            {
                write_session(live, |s| {
                    apply_tl_users_and_chats(s, &uf.users, &uf.chats);
                    apply_tl_user_full(s, &uf.full_user);
                });
            }
        } else if let Some(cid) = data_peer.to_chat() {
            if let Ok(tl_enums::messages::ChatFull::ChatFull(cf)) = invoke_rpc(
                live,
                tl_fns::messages::GetFullChat {
                    chat_id: cid.to_tl(),
                },
            ) {
                write_session(live, |s| {
                    apply_tl_users_and_chats(s, &cf.users, &cf.chats);
                    apply_tl_chat_full(s, &cf.full_chat);
                });
            }
        } else if let Some(chid) = data_peer.to_channel() {
            let channel = read_session(live, |s| input_channel_for(s, chid));
            if !matches!(channel, tl_enums::InputChannel::Empty)
                && let Ok(tl_enums::messages::ChatFull::ChatFull(cf)) =
                    invoke_rpc(live, tl_fns::channels::GetFullChannel { channel })
            {
                write_session(live, |s| {
                    apply_tl_users_and_chats(s, &cf.users, &cf.chats);
                    apply_tl_chat_full(s, &cf.full_chat);
                });
            }
        }
    }

    let (after_full, after_status, after_photo_id) = read_session(live, |s| {
        let cached = s
            .peer_active_photo(data_peer)
            .and_then(photo_data_to_profile_ref);
        let (status, pid) = s
            .peer(data_peer)
            .map(|p| (p.common().loaded_status, p.common().userpic_photo_id))
            .unwrap_or((LoadedStatus::Not, UNKNOWN_PHOTO_ID));
        (cached, status, pid)
    });
    if after_full.is_some() {
        return after_full;
    }
    if after_status >= LoadedStatus::Full && after_photo_id == PhotoId::ZERO {
        return None;
    }

    // Fallback for users: fetch the active profile photo via `photos.getUserPhotos`
    // (`offset = 0, max_id = 0, limit = 1`).
    if let Some(uid) = data_peer.to_user()
        && with_state(live, |st| {
            st.fetched_user_photos.insert((peer_id, after_photo_id))
        })
    {
        let input_user = read_session(live, |s| input_user_for(s, uid));
        if !matches!(input_user, tl_enums::InputUser::Empty)
            && let Ok(resp) = invoke_rpc(
                live,
                tl_fns::photos::GetUserPhotos {
                    user_id: input_user,
                    offset: 0,
                    max_id: 0,
                    limit: 1,
                },
            )
        {
            let (photos, users) = match resp {
                tl_enums::photos::Photos::Photos(p) => (p.photos, p.users),
                tl_enums::photos::Photos::Slice(p) => (p.photos, p.users),
            };
            write_session(live, |s| {
                apply_tl_users_and_chats(s, &users, &[]);
                let mut first_valid: Option<PhotoId> = None;
                for tl_photo in &photos {
                    if let Some(data) = photo_from_tl(tl_photo) {
                        if first_valid.is_none() {
                            first_valid = Some(data.id);
                        }
                        s.cache_photo(data);
                    }
                }
                if let Some(pid) = first_valid
                    && let Some(user) = s.user_mut(uid)
                    && (user.common.userpic_photo_id == UNKNOWN_PHOTO_ID
                        || user.common.userpic_photo_id == PhotoId::ZERO)
                {
                    user.common.userpic_photo_id = pid;
                }
            });
        }
    }

    read_session(live, |s| {
        s.peer_active_photo(data_peer)
            .and_then(photo_data_to_profile_ref)
    })
}
