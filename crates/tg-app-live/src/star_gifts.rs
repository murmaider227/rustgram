//! Live [`StarGiftsMarketplaceBackend`] implementation for [`LiveSession`] wired to
//! Layer 233 Telegram TL RPCs for Star Gifts marketplace, collections, auctions, crafting,
//! giveaways, boost replacement, and Stars affiliate programs.
//!
//! Port origin:
//! - `info/peer_gifts/info_peer_gifts_widget.cpp`, `info_peer_gifts_collections.cpp`
//! - `boxes/star_gift_box.cpp`, `star_gift_resale_box.cpp`, `star_gift_auction_box.cpp`,
//!   `star_gift_craft_box.cpp`
//! - `info/channel_statistics/boosts/create_giveaway_box.cpp`, `boxes/peers/replace_boost_box.cpp`
//! - `info/bot/starref/info_bot_starref_common.cpp`, `setup_widget.cpp`, `join_widget.cpp`
//! - `api/api_premium.cpp`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_data::{CollectibleId, PeerId as DataPeerId, Session, TimeId, UserId as DataUserId};
use tg_payments::credits::{CreditTopupOption, CreditsAmount, stars_giveaway_options_from_tl};
use tg_payments::form::PaymentForm;
use tg_payments::giveaways::{
    GiveawayDraft, GiveawayLimits, GiveawayPrizeKind, PremiumGiftCodeCatalog, build_apply_boost,
    build_get_premium_gift_code_options, build_get_stars_giveaway_options,
    build_launch_prepaid_giveaway, parse_for_channel_boost_slots,
};
use tg_payments::premium::{
    GiveawayInfo, MyBoostSlot, boosts_status_from_tl, giveaway_info_from_tl, my_boosts_from_tl,
};
use tg_payments::star_gift_auctions::{
    ActiveAuctionsSnapshot, GiftAuctionAcquiredGift, GiftAuctionState, acquired_gifts_from_tl,
    active_auctions_from_tl, auction_state_from_tl, build_auction_bid_invoice,
    build_get_star_gift_active_auctions, build_get_star_gift_auction_acquired_gifts,
    build_get_star_gift_auction_state_by_id, compute_active_auctions_hash,
};
use tg_payments::star_gift_craft::{
    build_craft_star_gift, build_get_craft_star_gifts, parse_craft_too_early_error,
};
use tg_payments::star_gifts::{
    CanSendGiftResult, GIFT_WITHDRAW_TMP_PASSWORD_PERIOD_SECS, GIFTS_PER_PAGE, GiftCollection,
    PeerGiftsFilter, ResaleGiftsDescriptor, ResaleGiftsFilter, SavedStarGift, SavedStarGiftId,
    build_convert_star_gift, build_create_star_gift_collection, build_delete_star_gift,
    build_delete_star_gift_collection, build_get_resale_star_gifts, build_get_saved_star_gift,
    build_get_saved_star_gifts_filtered, build_get_star_gift_collections,
    build_get_star_gift_withdrawal_url, build_get_unique_star_gift,
    build_get_unique_star_gift_value_info, build_reorder_star_gift_collections,
    build_resale_invoice, build_save_star_gift, build_toggle_chat_star_gift_notifications,
    build_toggle_star_gifts_pinned_to_top, build_transfer_star_gift, build_unwear_emoji_status,
    build_update_star_gift_collection, build_update_star_gift_price, build_upgrade_star_gift,
    build_wear_unique_gift, build_withdraw_star_gift, compute_gift_collections_hash,
    gift_collection_from_tl, gift_collections_from_tl, is_tmp_password_refused_error,
    resale_star_gifts_from_tl, saved_star_gifts_from_tl, star_gift_from_tl,
    star_gift_withdrawal_url_from_tl, unique_gift_value_from_tl, unique_star_gift_from_tl,
    validate_collection_title,
};
use tg_payments::starref::{
    ConnectedStarRefBot, STARREF_PER_PAGE, StarRefProgram, SuggestedStarRefSort,
    build_connect_star_ref_bot, build_edit_connected_star_ref_bot,
    build_get_connected_star_ref_bots, build_get_suggested_star_ref_bots,
    build_update_star_ref_program, connected_star_ref_bots_from_tl, star_ref_program_from_tl,
    suggested_star_ref_bots_from_tl,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::{known_peer_ids, now_unix_i32, peer_display_title};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::payments::{
    cached_received_gift, cached_valid_tmp_password, clear_tmp_password, obtain_tmp_password,
    remove_cached_received_gift,
};
use crate::session::LiveSession;
use tg_app_core::payments::backend::{PaymentsBackend, ReceivedGiftItem};
use tg_app_core::payments::marketplace_backend::{
    ConnectedStarRefItem, CraftExecutionOutcome, GiveawayOptionsBundle, PeerGiftsPage,
    ReplaceBoostBundle, StarGiftsMarketplaceBackend, StarRefOverviewBundle, SuggestedStarRefItem,
    UniqueGiftDetailBundle,
};

const STAR_GIFTS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct StarGiftsLiveState {
    saved_gifts: HashMap<u64, SavedStarGift>,
    collections: Vec<GiftCollection>,
    collections_by_peer: HashMap<DataPeerId, Vec<GiftCollection>>,
    chat_notifications_enabled: HashMap<DataPeerId, bool>,
    worn_collectible_id: Option<CollectibleId>,
    active_auctions: Vec<GiftAuctionState>,
    last_catalog: Option<PremiumGiftCodeCatalog>,
    last_stars_options: Vec<tg_payments::credits::CreditsGiveawayOption>,
    random_seq: u64,
}

impl StarGiftsLiveState {
    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.random_seq = self.random_seq.wrapping_add(1);
        ((now << 24) ^ self.random_seq ^ 0x5347_4D50_1600) as i64
    }
}

struct StarGiftsStateEntry {
    owner: Weak<LiveBridgeState>,
    state: StarGiftsLiveState,
}

fn star_gifts_state_registry() -> &'static Mutex<HashMap<usize, StarGiftsStateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, StarGiftsStateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_sg_state<R>(live: &LiveSession, f: impl FnOnce(&mut StarGiftsLiveState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = star_gifts_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| StarGiftsStateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: StarGiftsLiveState::default(),
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
    R: tg_tl::RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            STAR_GIFTS_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
}

fn resolve_input_peer(
    live: &LiveSession,
    name: &str,
) -> Result<(DataPeerId, tl_enums::InputPeer), String> {
    let cleaned = name.trim().trim_start_matches('@');
    if cleaned.is_empty() {
        let self_pid = read_session(live, Session::self_peer_id);
        return Ok((self_pid, tl_enums::InputPeer::Self_));
    }
    let local = read_session(live, |s| {
        for pid in known_peer_ids(s) {
            if let Some(peer) = s.peer(pid) {
                let matches_title = peer.name().eq_ignore_ascii_case(cleaned);
                let matches_uname = peer
                    .as_user()
                    .is_some_and(|u| u.usernames.username().eq_ignore_ascii_case(cleaned))
                    || peer
                        .as_channel()
                        .is_some_and(|c| c.usernames.username().eq_ignore_ascii_case(cleaned));
                if matches_title || matches_uname {
                    return Some((pid, input_peer_for(s, pid)));
                }
            }
        }
        None
    });
    if let Some((pid, ip)) = local
        && !matches!(ip, tl_enums::InputPeer::Empty)
    {
        return Ok((pid, ip));
    }
    let tl_enums::contacts::ResolvedPeer::ResolvedPeer(resolved) = invoke_rpc(
        live,
        tl_fns::contacts::ResolveUsername {
            username: cleaned.to_owned(),
            referer: None,
        },
    )?;
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &resolved.users, &resolved.chats);
    });
    let pid = tg_api::convert::peer_id_from_tl(&resolved.peer);
    let ip = read_session(live, |s| input_peer_for(s, pid));
    if matches!(ip, tl_enums::InputPeer::Empty) {
        Err(format!("Could not resolve peer '{name}'."))
    } else {
        Ok((pid, ip))
    }
}

fn resolve_channel_peer(
    live: &LiveSession,
    channel_name: &str,
) -> Result<(DataPeerId, String, tl_enums::InputPeer), String> {
    if !channel_name.trim().is_empty() {
        let (pid, ip) = resolve_input_peer(live, channel_name)?;
        let title = read_session(live, |s| peer_display_title(s, pid));
        return Ok((pid, title, ip));
    }
    let found = read_session(live, |s| {
        let pid = known_peer_ids(s)
            .into_iter()
            .find(|&p| s.peer(p).is_some_and(|peer| peer.as_channel().is_some()))
            .unwrap_or_else(|| s.self_peer_id());
        let title = peer_display_title(s, pid);
        let ip = input_peer_for(s, pid);
        let ip = if matches!(ip, tl_enums::InputPeer::Empty) {
            tl_enums::InputPeer::Self_
        } else {
            ip
        };
        (pid, title, ip)
    });
    Ok(found)
}

fn resolve_input_user(
    live: &LiveSession,
    username: &str,
) -> Result<(DataUserId, tl_enums::InputUser), String> {
    let cleaned = username.trim().trim_start_matches('@');
    if cleaned.is_empty() {
        return Err("Bot username cannot be empty.".to_owned());
    }
    let local = read_session(live, |s| {
        for pid in known_peer_ids(s) {
            if let Some(peer) = s.peer(pid)
                && let Some(u) = peer.as_user()
                && (u.usernames.username().eq_ignore_ascii_case(cleaned)
                    || peer.name().eq_ignore_ascii_case(cleaned))
                && let Some(uid) = pid.to_user()
            {
                let iu = input_user_for(s, uid);
                if !matches!(iu, tl_enums::InputUser::Empty) {
                    return Some((uid, iu));
                }
            }
        }
        None
    });
    if let Some(found) = local {
        return Ok(found);
    }
    let (pid, _) = resolve_input_peer(live, cleaned)?;
    let uid = pid
        .to_user()
        .ok_or_else(|| format!("'{username}' is not a user/bot."))?;
    let iu = read_session(live, |s| input_user_for(s, uid));
    if matches!(iu, tl_enums::InputUser::Empty) {
        Err(format!("Missing access hash for user '{username}'."))
    } else {
        Ok((uid, iu))
    }
}

fn saved_gift_to_input(
    live: &LiveSession,
    saved: &SavedStarGift,
) -> Option<tl_enums::InputSavedStarGift> {
    if let Some(ref unique) = saved.info.unique
        && !unique.slug.is_empty()
        && saved.manage_id.is_none()
    {
        return Some(tl_enums::InputSavedStarGift::Slug(
            tl_types::InputSavedStarGiftSlug {
                slug: unique.slug.clone(),
            },
        ));
    }
    match saved.manage_id? {
        SavedStarGiftId::User(msg_id) => Some(tl_enums::InputSavedStarGift::User(
            tl_types::InputSavedStarGiftUser {
                msg_id: msg_id.as_i32().unwrap_or(0),
            },
        )),
        SavedStarGiftId::Chat { peer_id, saved_id } => {
            let peer = read_session(live, |s| input_peer_for(s, peer_id));
            Some(tl_enums::InputSavedStarGift::Chat(
                tl_types::InputSavedStarGiftChat {
                    peer,
                    saved_id: saved_id as i64,
                },
            ))
        }
        SavedStarGiftId::Slug => saved.info.unique.as_ref().map(|u| {
            tl_enums::InputSavedStarGift::Slug(tl_types::InputSavedStarGiftSlug {
                slug: u.slug.clone(),
            })
        }),
    }
}

fn input_for_cached_gift(
    live: &LiveSession,
    gift_id: u64,
) -> Result<(SavedStarGift, tl_enums::InputSavedStarGift), String> {
    let saved = with_sg_state(live, |st| st.saved_gifts.get(&gift_id).cloned())
        .or_else(|| cached_received_gift(live, gift_id))
        .ok_or_else(|| format!("Saved Star Gift #{gift_id} not loaded."))?;
    let input = saved_gift_to_input(live, &saved)
        .ok_or_else(|| format!("Saved Star Gift #{gift_id} has no manageable ID."))?;
    Ok((saved, input))
}

fn artwork_kind_for(index: usize) -> String {
    const KINDS: [&str; 8] = [
        "star", "cake", "gem", "trophy", "rose", "crown", "rocket", "heart",
    ];
    KINDS[index % KINDS.len()].to_owned()
}

fn saved_list_to_items(live: &LiveSession, list: Vec<SavedStarGift>) -> Vec<ReceivedGiftItem> {
    read_session(live, |s| {
        list.into_iter()
            .enumerate()
            .map(|(idx, saved)| {
                let title = if let Some(ref u) = saved.info.unique {
                    u.display_name()
                } else if !saved.info.resell_title.is_empty() {
                    saved.info.resell_title.clone()
                } else {
                    format!("Star Gift #{}", saved.info.id)
                };
                let sender_name = if saved.anonymous || saved.from_id == DataPeerId::ZERO {
                    "Anonymous".to_owned()
                } else {
                    peer_display_title(s, saved.from_id)
                };
                ReceivedGiftItem {
                    saved,
                    title,
                    sender_name,
                    artwork_kind: artwork_kind_for(idx),
                }
            })
            .collect()
    })
}

fn extract_crafted_gift_from_updates(updates: &tl_enums::Updates) -> Option<CraftExecutionOutcome> {
    let list = match updates {
        tl_enums::Updates::Updates(u) => &u.updates,
        tl_enums::Updates::Combined(u) => &u.updates,
        _ => return None,
    };
    for upd in list {
        if matches!(upd, tl_enums::Update::StarGiftCraftFail) {
            return Some(CraftExecutionOutcome::Failed {
                notice: "Crafting attempt failed; input gifts were consumed.".to_owned(),
            });
        }
        let msg = match upd {
            tl_enums::Update::NewMessage(m) => Some(&m.message),
            tl_enums::Update::NewChannelMessage(m) => Some(&m.message),
            _ => None,
        };
        if let Some(tl_enums::Message::Service(svc)) = msg
            && let tl_enums::MessageAction::StarGiftUnique(act) = &svc.action
        {
            let parsed = star_gift_from_tl(&act.gift);
            if let Some(ref unique) = parsed.unique {
                return Some(CraftExecutionOutcome::Succeeded {
                    result_title: unique.display_name(),
                    result_slug: unique.slug.clone(),
                    notice: format!("Crafted unique gift \"{}\"!", unique.display_name()),
                });
            }
        }
    }
    None
}

impl StarGiftsMarketplaceBackend for LiveSession {
    fn load_peer_gifts(
        &self,
        peer_name: &str,
        filter: &PeerGiftsFilter,
        offset: &str,
    ) -> Result<PeerGiftsPage, String> {
        let (peer_id, input_peer) = resolve_input_peer(self, peer_name)?;
        let self_pid = read_session(self, Session::self_peer_id);
        let is_self = peer_id == self_pid;

        let saved_tl = invoke_rpc(
            self,
            build_get_saved_star_gifts_filtered(input_peer.clone(), filter, offset, GIFTS_PER_PAGE),
        )?;
        let tl_enums::payments::SavedStarGifts::SavedStarGifts(ref inner) = saved_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        let slice = saved_star_gifts_from_tl(&saved_tl, peer_id, is_self);

        if let Some(enabled) = slice.chat_notifications_enabled {
            with_sg_state(self, |st| {
                st.chat_notifications_enabled.insert(peer_id, enabled);
            });
        }

        // Port of `info/peer_gifts/info_peer_gifts_widget.cpp:429-432`: hash cached collections
        // for this peer via `Api::CountHash` so unchanged lists return `NotModified`.
        let collections_hash = with_sg_state(self, |st| {
            st.collections_by_peer
                .get(&peer_id)
                .map(|cols| compute_gift_collections_hash(cols))
                .unwrap_or(0)
        });
        let collections = match invoke_rpc(
            self,
            build_get_star_gift_collections(input_peer, collections_hash),
        ) {
            Ok(cols_tl) => {
                if let Some(parsed) = gift_collections_from_tl(&cols_tl) {
                    with_sg_state(self, |st| {
                        st.collections = parsed.clone();
                        st.collections_by_peer.insert(peer_id, parsed.clone());
                    });
                    parsed
                } else {
                    with_sg_state(self, |st| {
                        st.collections_by_peer
                            .get(&peer_id)
                            .cloned()
                            .unwrap_or_else(|| st.collections.clone())
                    })
                }
            }
            Err(_) => with_sg_state(self, |st| {
                st.collections_by_peer
                    .get(&peer_id)
                    .cloned()
                    .unwrap_or_else(|| st.collections.clone())
            }),
        };

        with_sg_state(self, |st| {
            for g in &slice.list {
                st.saved_gifts.insert(g.info.id, g.clone());
            }
        });

        let items = saved_list_to_items(self, slice.list);
        let display_peer = read_session(self, |s| peer_display_title(s, peer_id));
        let worn_collectible_id = with_sg_state(self, |st| st.worn_collectible_id);

        Ok(PeerGiftsPage {
            peer_name: display_peer,
            items,
            next_offset: slice.offset,
            total_count: slice.count,
            collections,
            worn_collectible_id,
        })
    }

    fn get_unique_star_gift(&self, slug: &str) -> Result<UniqueGiftDetailBundle, String> {
        let cleaned = slug.trim();
        if cleaned.is_empty() {
            return Err("Unique gift slug cannot be empty.".to_owned());
        }
        let tl_res = invoke_rpc(self, build_get_unique_star_gift(cleaned))?;
        let tl_enums::payments::UniqueStarGift::UniqueStarGift(ref inner) = tl_res;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        let gift = unique_star_gift_from_tl(&tl_res);
        let value_info = invoke_rpc(self, build_get_unique_star_gift_value_info(cleaned))
            .ok()
            .map(|v| unique_gift_value_from_tl(&v))
            .or_else(|| gift.unique.as_ref().and_then(|u| u.value.clone()));

        let owner_name = match &gift.unique {
            Some(u) if u.owner_id != DataPeerId::ZERO => {
                read_session(self, |s| peer_display_title(s, u.owner_id))
            }
            Some(u) if !u.owner_name.is_empty() => u.owner_name.clone(),
            _ => "Unknown".to_owned(),
        };
        let worn = with_sg_state(self, |st| st.worn_collectible_id);
        let is_worn = gift.unique.as_ref().is_some_and(|u| worn == Some(u.id));

        Ok(UniqueGiftDetailBundle {
            gift,
            owner_name,
            value_info,
            is_worn,
        })
    }

    fn upgrade_star_gift(
        &mut self,
        gift_id: u64,
        keep_original_details: bool,
    ) -> Result<String, String> {
        let (_, stargift) = input_for_cached_gift(self, gift_id)?;
        let updates = invoke_rpc(
            self,
            build_upgrade_star_gift(stargift, keep_original_details),
        )?;
        let _ = self.apply_updates_to_session(&updates);
        Ok(format!(
            "Upgraded Star Gift #{gift_id} to a unique collectible."
        ))
    }

    fn transfer_star_gift(&mut self, gift_id: u64, recipient: &str) -> Result<String, String> {
        let (_, stargift) = input_for_cached_gift(self, gift_id)?;
        let (_, to_peer) = resolve_input_peer(self, recipient)?;
        let updates = invoke_rpc(self, build_transfer_star_gift(stargift, to_peer))?;
        let _ = self.apply_updates_to_session(&updates);
        with_sg_state(self, |st| {
            st.saved_gifts.remove(&gift_id);
        });
        self.bridge_state().notify_ui();
        Ok(format!("Transferred Star Gift #{gift_id} to {recipient}."))
    }

    fn convert_star_gift(&mut self, gift_id: u64) -> Result<String, String> {
        let (_, stargift) = input_for_cached_gift(self, gift_id)?;
        let _ = invoke_rpc(self, build_convert_star_gift(stargift))?;
        with_sg_state(self, |st| {
            st.saved_gifts.remove(&gift_id);
        });
        self.bridge_state().notify_ui();
        Ok(format!("Converted Star Gift #{gift_id} to Stars."))
    }

    fn save_star_gift(&mut self, gift_id: u64, unsave: bool) -> Result<String, String> {
        let (_, stargift) = input_for_cached_gift(self, gift_id)?;
        let _ = invoke_rpc(self, build_save_star_gift(stargift, unsave))?;
        with_sg_state(self, |st| {
            if let Some(entry) = st.saved_gifts.get_mut(&gift_id) {
                entry.hidden = unsave;
            }
        });
        self.bridge_state().notify_ui();
        Ok(if unsave {
            format!("Hid Star Gift #{gift_id} from profile.")
        } else {
            format!("Displayed Star Gift #{gift_id} on profile.")
        })
    }

    fn toggle_star_gifts_pinned_to_top(&mut self, gift_ids: &[u64]) -> Result<String, String> {
        let mut inputs = Vec::with_capacity(gift_ids.len());
        for &gid in gift_ids {
            let (_, sg) = input_for_cached_gift(self, gid)?;
            inputs.push(sg);
        }
        let _ = invoke_rpc(
            self,
            build_toggle_star_gifts_pinned_to_top(tl_enums::InputPeer::Self_, inputs),
        )?;
        with_sg_state(self, |st| {
            for &gid in gift_ids {
                if let Some(entry) = st.saved_gifts.get_mut(&gid) {
                    entry.pinned = !entry.pinned;
                }
            }
        });
        self.bridge_state().notify_ui();
        Ok(format!("Toggled pin state for {} gift(s).", gift_ids.len()))
    }

    fn wear_unique_gift(
        &mut self,
        collectible_id: Option<CollectibleId>,
        until: Option<TimeId>,
    ) -> Result<String, String> {
        let req = match collectible_id {
            Some(id) => build_wear_unique_gift(id, until),
            None => build_unwear_emoji_status(),
        };
        let _ = invoke_rpc(self, req)?;
        with_sg_state(self, |st| {
            st.worn_collectible_id = collectible_id;
        });
        self.bridge_state().notify_ui();
        match collectible_id {
            Some(id) => Ok(format!(
                "Now wearing collectible #{} as emoji status.",
                id.raw()
            )),
            None => Ok("Removed collectible emoji status.".to_owned()),
        }
    }

    fn load_star_gift_collections(&self, peer_name: &str) -> Result<Vec<GiftCollection>, String> {
        let (peer_id, peer) = resolve_input_peer(self, peer_name)?;
        let hash = with_sg_state(self, |st| {
            st.collections_by_peer
                .get(&peer_id)
                .map(|cols| compute_gift_collections_hash(cols))
                .unwrap_or(0)
        });
        let tl_res = invoke_rpc(self, build_get_star_gift_collections(peer, hash))?;
        let cols = gift_collections_from_tl(&tl_res).unwrap_or_else(|| {
            with_sg_state(self, |st| {
                st.collections_by_peer
                    .get(&peer_id)
                    .cloned()
                    .unwrap_or_else(|| st.collections.clone())
            })
        });
        with_sg_state(self, |st| {
            st.collections = cols.clone();
            st.collections_by_peer.insert(peer_id, cols.clone());
        });
        Ok(cols)
    }

    fn create_star_gift_collection(
        &mut self,
        peer_name: &str,
        title: &str,
        gift_ids: &[u64],
    ) -> Result<GiftCollection, String> {
        let valid_title = validate_collection_title(title).map_err(str::to_owned)?;
        let (_, peer) = resolve_input_peer(self, peer_name)?;
        let mut inputs = Vec::with_capacity(gift_ids.len());
        for &gid in gift_ids {
            let (_, sg) = input_for_cached_gift(self, gid)?;
            inputs.push(sg);
        }
        let tl_col = invoke_rpc(
            self,
            build_create_star_gift_collection(peer, valid_title, inputs),
        )?;
        let col = gift_collection_from_tl(&tl_col);
        with_sg_state(self, |st| st.collections.push(col.clone()));
        self.bridge_state().notify_ui();
        Ok(col)
    }

    fn update_star_gift_collection(
        &mut self,
        peer_name: &str,
        collection_id: i32,
        title: Option<&str>,
        add_gift_ids: &[u64],
        delete_gift_ids: &[u64],
    ) -> Result<GiftCollection, String> {
        let valid_title = match title {
            Some(t) => Some(validate_collection_title(t).map_err(str::to_owned)?),
            None => None,
        };
        let (_, peer) = resolve_input_peer(self, peer_name)?;
        let mut adds = Vec::with_capacity(add_gift_ids.len());
        for &gid in add_gift_ids {
            let (_, sg) = input_for_cached_gift(self, gid)?;
            adds.push(sg);
        }
        let mut dels = Vec::with_capacity(delete_gift_ids.len());
        for &gid in delete_gift_ids {
            let (_, sg) = input_for_cached_gift(self, gid)?;
            dels.push(sg);
        }
        let tl_col = invoke_rpc(
            self,
            build_update_star_gift_collection(
                peer,
                collection_id,
                valid_title,
                (!dels.is_empty()).then_some(dels),
                (!adds.is_empty()).then_some(adds),
                None,
            ),
        )?;
        let col = gift_collection_from_tl(&tl_col);
        with_sg_state(self, |st| {
            if let Some(existing) = st.collections.iter_mut().find(|c| c.id == col.id) {
                *existing = col.clone();
            }
        });
        self.bridge_state().notify_ui();
        Ok(col)
    }

    fn reorder_star_gift_collections(
        &mut self,
        peer_name: &str,
        order: &[i32],
    ) -> Result<String, String> {
        let (_, peer) = resolve_input_peer(self, peer_name)?;
        let _ = invoke_rpc(
            self,
            build_reorder_star_gift_collections(peer, order.to_vec()),
        )?;
        self.bridge_state().notify_ui();
        Ok("Reordered Star Gift collections.".to_owned())
    }

    fn delete_star_gift_collection(
        &mut self,
        peer_name: &str,
        collection_id: i32,
    ) -> Result<String, String> {
        let (_, peer) = resolve_input_peer(self, peer_name)?;
        let _ = invoke_rpc(self, build_delete_star_gift_collection(peer, collection_id))?;
        with_sg_state(self, |st| {
            st.collections.retain(|c| c.id != collection_id);
        });
        self.bridge_state().notify_ui();
        Ok(format!("Deleted collection #{collection_id}."))
    }

    fn load_resale_star_gifts(
        &self,
        gift_id: u64,
        filter: &ResaleGiftsFilter,
        offset: &str,
    ) -> Result<ResaleGiftsDescriptor, String> {
        let tl_res = invoke_rpc(self, build_get_resale_star_gifts(gift_id, filter, offset))?;
        let tl_enums::payments::ResaleStarGifts::ResaleStarGifts(ref inner) = tl_res;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        Ok(resale_star_gifts_from_tl(gift_id, &tl_res))
    }

    fn update_star_gift_price(
        &mut self,
        gift_id: u64,
        resell_amount: CreditsAmount,
    ) -> Result<String, String> {
        let (_, stargift) = input_for_cached_gift(self, gift_id)?;
        let updates = invoke_rpc(self, build_update_star_gift_price(stargift, resell_amount))?;
        let _ = self.apply_updates_to_session(&updates);
        with_sg_state(self, |st| {
            if let Some(entry) = st.saved_gifts.get_mut(&gift_id)
                && let Some(ref mut u) = entry.info.unique
            {
                if resell_amount.is_empty() {
                    u.stars_for_resale = 0;
                    u.nano_ton_for_resale = 0;
                    u.only_accept_ton = false;
                } else if resell_amount.is_ton() {
                    u.stars_for_resale = 0;
                    u.nano_ton_for_resale =
                        resell_amount.whole() * 1_000_000_000 + resell_amount.nano();
                    u.only_accept_ton = true;
                } else {
                    u.stars_for_resale = resell_amount.whole();
                    u.nano_ton_for_resale = 0;
                    u.only_accept_ton = false;
                }
            }
        });
        self.bridge_state().notify_ui();
        Ok(if resell_amount.is_empty() {
            format!("Unlisted Star Gift #{gift_id} from resale.")
        } else {
            format!("Updated resale price for Star Gift #{gift_id}.")
        })
    }

    fn buy_resale_star_gift(
        &mut self,
        slug: &str,
        recipient: &str,
        ton: bool,
        message: &str,
    ) -> Result<String, String> {
        let (_, to_id) = resolve_input_peer(self, recipient)?;
        let msg_tl = (!message.trim().is_empty()).then(|| {
            tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: message.trim().to_owned(),
                entities: Vec::new(),
            })
        });
        let invoice = build_resale_invoice(slug, to_id, ton, true, msg_tl);
        let form = invoke_rpc(
            self,
            PaymentForm::build_get_payment_form_request(invoice.clone(), None),
        )?;
        let form_id = match &form {
            tl_enums::payments::PaymentForm::StarGift(f) => f.form_id,
            tl_enums::payments::PaymentForm::Stars(f) => f.form_id,
            tl_enums::payments::PaymentForm::PaymentForm(f) => f.form_id,
        };
        let _res = invoke_rpc(self, tl_fns::payments::SendStarsForm { form_id, invoice })?;
        self.bridge_state().notify_ui();
        Ok(format!("Purchased resale gift '{slug}' (form #{form_id})."))
    }

    fn load_active_auctions(&self) -> Result<ActiveAuctionsSnapshot, String> {
        let hash = with_sg_state(self, |st| compute_active_auctions_hash(&st.active_auctions));
        let tl_res = invoke_rpc(self, build_get_star_gift_active_auctions(hash))?;
        if let tl_enums::payments::StarGiftActiveAuctions::StarGiftActiveAuctions(ref inner) =
            tl_res
        {
            write_session(self, |s| {
                apply_tl_users_and_chats(s, &inner.users, &inner.chats);
            });
        }
        let snapshot = active_auctions_from_tl(&tl_res).unwrap_or_else(|| {
            with_sg_state(self, |st| ActiveAuctionsSnapshot {
                auctions: st.active_auctions.clone(),
            })
        });
        with_sg_state(self, |st| {
            st.active_auctions = snapshot.auctions.clone();
        });
        Ok(snapshot)
    }

    fn load_auction_state(&self, gift_id: u64, version: i32) -> Result<GiftAuctionState, String> {
        let tl_res = invoke_rpc(
            self,
            build_get_star_gift_auction_state_by_id(gift_id, version),
        )?;
        let tl_enums::payments::StarGiftAuctionState::StarGiftAuctionState(ref inner) = tl_res;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        Ok(auction_state_from_tl(&tl_res))
    }

    fn load_auction_acquired_gifts(
        &self,
        gift_id: u64,
    ) -> Result<Vec<GiftAuctionAcquiredGift>, String> {
        let tl_res = invoke_rpc(self, build_get_star_gift_auction_acquired_gifts(gift_id))?;
        let tl_enums::payments::StarGiftAuctionAcquiredGifts::StarGiftAuctionAcquiredGifts(
            ref inner,
        ) = tl_res;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        Ok(acquired_gifts_from_tl(&tl_res))
    }

    fn place_auction_bid(
        &mut self,
        gift_id: u64,
        bid_amount: i64,
        update_bid: bool,
        hide_name: bool,
        recipient: &str,
        message: &str,
    ) -> Result<String, String> {
        let peer = if recipient.trim().is_empty() {
            None
        } else {
            Some(resolve_input_peer(self, recipient)?.1)
        };
        let msg_tl = (!message.trim().is_empty()).then(|| {
            tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: message.trim().to_owned(),
                entities: Vec::new(),
            })
        });
        let invoice =
            build_auction_bid_invoice(gift_id, bid_amount, update_bid, hide_name, peer, msg_tl);
        let form = invoke_rpc(
            self,
            PaymentForm::build_get_payment_form_request(invoice.clone(), None),
        )?;
        let form_id = match &form {
            tl_enums::payments::PaymentForm::StarGift(f) => f.form_id,
            tl_enums::payments::PaymentForm::Stars(f) => f.form_id,
            tl_enums::payments::PaymentForm::PaymentForm(f) => f.form_id,
        };
        let _res = invoke_rpc(self, tl_fns::payments::SendStarsForm { form_id, invoice })?;
        self.bridge_state().notify_ui();
        Ok(format!(
            "Placed auction bid of {bid_amount} Stars on gift #{gift_id} (form #{form_id})."
        ))
    }

    fn load_craft_star_gifts(
        &self,
        gift_id: u64,
        offset: &str,
    ) -> Result<Vec<ReceivedGiftItem>, String> {
        let self_pid = read_session(self, Session::self_peer_id);
        let tl_res = invoke_rpc(
            self,
            build_get_craft_star_gifts(gift_id, offset, GIFTS_PER_PAGE),
        )?;
        let tl_enums::payments::SavedStarGifts::SavedStarGifts(ref inner) = tl_res;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        let slice = saved_star_gifts_from_tl(&tl_res, self_pid, true);
        with_sg_state(self, |st| {
            for g in &slice.list {
                st.saved_gifts.insert(g.info.id, g.clone());
            }
        });
        Ok(saved_list_to_items(self, slice.list))
    }

    fn craft_star_gift(&mut self, gift_ids: &[u64]) -> Result<CraftExecutionOutcome, String> {
        if gift_ids.is_empty() {
            return Err("Select at least one unique gift to craft.".to_owned());
        }
        let mut inputs = Vec::with_capacity(gift_ids.len());
        for &gid in gift_ids {
            let (_, sg) = input_for_cached_gift(self, gid)?;
            inputs.push(sg);
        }
        let updates = match invoke_rpc(self, build_craft_star_gift(inputs)) {
            Ok(u) => u,
            Err(err) => {
                if let Some(wait_secs) = parse_craft_too_early_error(&err) {
                    return Err(format!(
                        "One of the selected gifts cannot be crafted for another {wait_secs}s."
                    ));
                }
                return Err(err);
            }
        };
        let _ = self.apply_updates_to_session(&updates);
        with_sg_state(self, |st| {
            for &gid in gift_ids {
                st.saved_gifts.remove(&gid);
            }
        });
        self.bridge_state().notify_ui();
        Ok(extract_crafted_gift_from_updates(&updates).unwrap_or(
            CraftExecutionOutcome::Succeeded {
                result_title: "Crafted Unique Gift".to_owned(),
                result_slug: String::new(),
                notice: "Crafting completed!".to_owned(),
            },
        ))
    }

    fn load_giveaway_options(&self, channel_name: &str) -> Result<GiveawayOptionsBundle, String> {
        let (peer_id, channel_title, input_peer) = resolve_channel_peer(self, channel_name)?;
        let prem_tl = invoke_rpc(
            self,
            build_get_premium_gift_code_options(Some(input_peer.clone())),
        )?;
        let premium_catalog = PremiumGiftCodeCatalog::from_tl(&prem_tl);

        let stars_options = invoke_rpc(self, build_get_stars_giveaway_options())
            .map(|opts| stars_giveaway_options_from_tl(&opts))
            .unwrap_or_default();

        let prepaid_giveaways =
            invoke_rpc(self, tl_fns::premium::GetBoostsStatus { peer: input_peer })
                .map(|st| boosts_status_from_tl(&st, false).prepaid_giveaways)
                .unwrap_or_default();

        with_sg_state(self, |st| {
            st.last_catalog = Some(premium_catalog.clone());
            st.last_stars_options = stars_options.clone();
        });

        Ok(GiveawayOptionsBundle {
            channel_peer_id: peer_id,
            channel_title,
            premium_catalog,
            stars_options,
            prepaid_giveaways,
            limits: GiveawayLimits::default(),
        })
    }

    fn create_or_launch_giveaway(&mut self, draft: &GiveawayDraft) -> Result<String, String> {
        draft
            .validate(now_unix_i32(), &GiveawayLimits::default())
            .map_err(|e| format!("Invalid giveaway parameters: {e:?}"))?;

        let boost_peer_tl = read_session(self, |s| {
            let ip = input_peer_for(s, draft.boost_peer);
            if matches!(ip, tl_enums::InputPeer::Empty) {
                tl_enums::InputPeer::Self_
            } else {
                ip
            }
        });
        let additional_peers_tl: Vec<tl_enums::InputPeer> = read_session(self, |s| {
            draft
                .additional_peers
                .iter()
                .map(|&pid| input_peer_for(s, pid))
                .filter(|ip| !matches!(ip, tl_enums::InputPeer::Empty))
                .collect()
        });
        let random_id = with_sg_state(self, StarGiftsLiveState::next_random_id);

        let (currency, amount, store_product, store_quantity) = match draft.prize {
            GiveawayPrizeKind::Premium { months } => {
                let store = with_sg_state(self, |st| {
                    st.last_catalog
                        .as_ref()
                        .and_then(|c| c.store_for(draft.quantity, months))
                });
                match store {
                    Some(s) => (s.currency, s.amount, s.store_product, s.store_quantity),
                    None => ("USD".to_owned(), 0, String::new(), 0),
                }
            }
            GiveawayPrizeKind::Stars { stars } => {
                let opt = with_sg_state(self, |st| {
                    st.last_stars_options
                        .iter()
                        .find(|o| o.credits == stars)
                        .cloned()
                });
                match opt {
                    Some(o) => (o.currency, o.amount as i64, o.store_product, 0),
                    None => ("USD".to_owned(), 0, String::new(), 0),
                }
            }
        };

        let invoice_desc = draft.to_invoice(
            boost_peer_tl.clone(),
            additional_peers_tl,
            currency,
            amount,
            store_product,
            store_quantity,
            random_id,
        );

        if let Some(prepaid_id) = draft.prepaid_id {
            let req = build_launch_prepaid_giveaway(boost_peer_tl, prepaid_id, &invoice_desc)
                .ok_or_else(|| "Failed to build launchPrepaidGiveaway request.".to_owned())?;
            let updates = invoke_rpc(self, req)?;
            let _ = self.apply_updates_to_session(&updates);
            return Ok(format!("Launched prepaid giveaway #{prepaid_id}."));
        }

        let input_invoice = invoice_desc
            .to_input_invoice()
            .ok_or_else(|| "Failed to build giveaway InputInvoice.".to_owned())?;
        let tl_form = invoke_rpc(
            self,
            PaymentForm::build_get_payment_form_request(input_invoice, None),
        )?;
        let form_id = match tl_form {
            tl_enums::payments::PaymentForm::PaymentForm(f) => f.form_id,
            tl_enums::payments::PaymentForm::Stars(f) => f.form_id,
            tl_enums::payments::PaymentForm::StarGift(f) => f.form_id,
        };
        Ok(format!("Giveaway payment form #{form_id} prepared."))
    }

    fn load_giveaway_info(&self, peer_name: &str, msg_id: i32) -> Result<GiveawayInfo, String> {
        let (_, peer) = resolve_input_peer(self, peer_name)?;
        let tl_res = invoke_rpc(self, tl_fns::payments::GetGiveawayInfo { peer, msg_id })?;
        Ok(giveaway_info_from_tl(&tl_res))
    }

    fn load_boost_slots_for_channel(
        &self,
        channel_name: &str,
    ) -> Result<ReplaceBoostBundle, String> {
        let (target_pid, channel_title, _) = resolve_channel_peer(self, channel_name)?;
        let my_tl = invoke_rpc(self, tl_fns::premium::GetMyBoosts {})?;
        let tl_enums::premium::MyBoosts::MyBoosts(ref inner) = my_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        let my_slots = my_boosts_from_tl(&my_tl);
        let now = now_unix_i32();
        let slots = parse_for_channel_boost_slots(target_pid, &my_slots, now);
        let peer_names = read_session(self, |s| {
            slots
                .other
                .iter()
                .map(|t| (t.peer_id, peer_display_title(s, t.peer_id)))
                .collect()
        });
        Ok(ReplaceBoostBundle {
            channel_peer_id: target_pid,
            channel_title,
            slots,
            peer_names,
            now,
        })
    }

    fn apply_boost_slots(
        &mut self,
        channel_name: &str,
        slots: &[i32],
    ) -> Result<Vec<MyBoostSlot>, String> {
        let (_, _, input_peer) = resolve_channel_peer(self, channel_name)?;
        let my_tl = invoke_rpc(self, build_apply_boost(input_peer, slots.to_vec()))?;
        let tl_enums::premium::MyBoosts::MyBoosts(ref inner) = my_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        self.bridge_state().notify_ui();
        Ok(my_boosts_from_tl(&my_tl))
    }

    fn load_starref_overview(
        &self,
        peer_name: &str,
        sort: SuggestedStarRefSort,
    ) -> Result<StarRefOverviewBundle, String> {
        let (peer_id, input_peer) = resolve_input_peer(self, peer_name)?;
        let display_peer = read_session(self, |s| peer_display_title(s, peer_id));

        let conn_tl = invoke_rpc(
            self,
            build_get_connected_star_ref_bots(input_peer.clone(), None, STARREF_PER_PAGE),
        )?;
        let tl_enums::payments::ConnectedStarRefBots::ConnectedStarRefBots(ref c_inner) = conn_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &c_inner.users, &[]);
        });
        let conn_slice = connected_star_ref_bots_from_tl(&conn_tl);

        let sugg_tl = invoke_rpc(
            self,
            build_get_suggested_star_ref_bots(input_peer, sort, "", STARREF_PER_PAGE),
        )?;
        let tl_enums::payments::SuggestedStarRefBots::SuggestedStarRefBots(ref s_inner) = sugg_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &s_inner.users, &[]);
        });
        let sugg_slice = suggested_star_ref_bots_from_tl(&sugg_tl);

        let (connected, suggested) = read_session(self, |s| {
            let conn = conn_slice
                .bots
                .into_iter()
                .map(|info| {
                    let pid = DataPeerId::from_user(info.bot_id);
                    let bot_name = peer_display_title(s, pid);
                    let bot_username = s
                        .peer(pid)
                        .and_then(|p| p.as_user())
                        .map(|u| u.usernames.username().to_owned())
                        .unwrap_or_default();
                    ConnectedStarRefItem {
                        bot_name,
                        bot_username,
                        info,
                    }
                })
                .collect();
            let sugg = sugg_slice
                .programs
                .into_iter()
                .map(|program| {
                    let pid = DataPeerId::from_user(program.bot_id);
                    let bot_name = peer_display_title(s, pid);
                    let bot_username = s
                        .peer(pid)
                        .and_then(|p| p.as_user())
                        .map(|u| u.usernames.username().to_owned())
                        .unwrap_or_default();
                    SuggestedStarRefItem {
                        bot_name,
                        bot_username,
                        program,
                    }
                })
                .collect();
            (conn, sugg)
        });

        Ok(StarRefOverviewBundle {
            peer_name: display_peer,
            managed_bot_username: String::new(),
            managed_bot_name: String::new(),
            managed_bot_id: DataUserId::ZERO,
            managed_program: None,
            connected,
            suggested,
            sort,
        })
    }

    fn update_starref_program(
        &mut self,
        bot_username: &str,
        commission_permille: i32,
        duration_months: i32,
    ) -> Result<StarRefProgram, String> {
        let (_, bot_user) = resolve_input_user(self, bot_username)?;
        let tl_prog = invoke_rpc(
            self,
            build_update_star_ref_program(bot_user, commission_permille, duration_months),
        )?;
        self.bridge_state().notify_ui();
        Ok(star_ref_program_from_tl(&tl_prog))
    }

    fn connect_starref_bot(
        &mut self,
        peer_name: &str,
        bot_username: &str,
    ) -> Result<ConnectedStarRefBot, String> {
        let (_, peer) = resolve_input_peer(self, peer_name)?;
        let (_, bot) = resolve_input_user(self, bot_username)?;
        let tl_res = invoke_rpc(self, build_connect_star_ref_bot(peer, bot))?;
        let tl_enums::payments::ConnectedStarRefBots::ConnectedStarRefBots(ref inner) = tl_res;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &[]);
        });
        let slice = connected_star_ref_bots_from_tl(&tl_res);
        self.bridge_state().notify_ui();
        slice
            .bots
            .into_iter()
            .next()
            .ok_or_else(|| "Server returned empty connected affiliate bots list.".to_owned())
    }

    fn edit_connected_starref_bot(
        &mut self,
        peer_name: &str,
        link: &str,
        revoked: bool,
    ) -> Result<ConnectedStarRefBot, String> {
        let (_, peer) = resolve_input_peer(self, peer_name)?;
        let tl_res = invoke_rpc(self, build_edit_connected_star_ref_bot(peer, link, revoked))?;
        let tl_enums::payments::ConnectedStarRefBots::ConnectedStarRefBots(ref inner) = tl_res;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &[]);
        });
        let slice = connected_star_ref_bots_from_tl(&tl_res);
        self.bridge_state().notify_ui();
        slice
            .bots
            .into_iter()
            .next()
            .ok_or_else(|| "Server returned empty edited affiliate bots list.".to_owned())
    }

    fn check_can_send_gift(&self, gift_id: u64) -> Result<CanSendGiftResult, String> {
        PaymentsBackend::check_can_send_gift(self, gift_id)
    }

    fn load_stars_gift_options(&self, recipient: &str) -> Result<Vec<CreditTopupOption>, String> {
        PaymentsBackend::load_stars_gift_options(self, recipient)
    }

    fn withdraw_star_gift(
        &mut self,
        gift_id: u64,
        password: &str,
        keep_on_profile: bool,
    ) -> Result<String, String> {
        let (_, stargift) = input_for_cached_gift(self, gift_id)?;
        let ok = invoke_with_gift_tmp_password(self, password, |tmp_password| {
            invoke_rpc(
                self,
                build_withdraw_star_gift(stargift.clone(), tmp_password, keep_on_profile),
            )
        })?;
        if !ok {
            // Port of `boxes/transfer_gift_box.cpp:562-564, 707-712`.
            return Err(
                "STARGIFT_WITHDRAW_FALSE: payments.withdrawStarGift returned false".to_owned(),
            );
        }
        if !keep_on_profile {
            with_sg_state(self, |st| {
                st.saved_gifts.remove(&gift_id);
            });
            remove_cached_received_gift(self, gift_id);
        }
        self.bridge_state().notify_ui();
        Ok(format!("Withdrew Star Gift #{gift_id} to TON wallet."))
    }

    fn get_star_gift_withdrawal_url(
        &mut self,
        gift_id: u64,
        password: &str,
    ) -> Result<String, String> {
        let (_, stargift) = input_for_cached_gift(self, gift_id)?;
        let resp = invoke_with_gift_tmp_password(self, password, |tmp_password| {
            invoke_rpc(
                self,
                build_get_star_gift_withdrawal_url(stargift.clone(), tmp_password),
            )
        })?;
        Ok(star_gift_withdrawal_url_from_tl(&resp))
    }

    fn delete_star_gift(&mut self, gift_id: u64) -> Result<String, String> {
        let (_, stargift) = input_for_cached_gift(self, gift_id)?;
        let ok = invoke_rpc(self, build_delete_star_gift(stargift))?;
        if !ok {
            return Err("payments.deleteStarGift returned false.".to_owned());
        }
        with_sg_state(self, |st| {
            st.saved_gifts.remove(&gift_id);
        });
        remove_cached_received_gift(self, gift_id);
        self.bridge_state().notify_ui();
        Ok(format!("Deleted Star Gift #{gift_id}."))
    }

    fn get_saved_star_gift(&self, gift_id: u64) -> Result<ReceivedGiftItem, String> {
        let (existing, stargift) = input_for_cached_gift(self, gift_id)?;
        let self_pid = read_session(self, Session::self_peer_id);
        let owner_peer_id = match existing.manage_id {
            Some(SavedStarGiftId::Chat { peer_id, .. }) => peer_id,
            _ => self_pid,
        };
        let is_self = owner_peer_id == self_pid;
        let saved_tl = invoke_rpc(self, build_get_saved_star_gift(vec![stargift]))?;
        let tl_enums::payments::SavedStarGifts::SavedStarGifts(ref inner) = saved_tl;
        write_session(self, |s| {
            apply_tl_users_and_chats(s, &inner.users, &inner.chats);
        });
        let slice = saved_star_gifts_from_tl(&saved_tl, owner_peer_id, is_self);
        with_sg_state(self, |st| {
            for g in &slice.list {
                st.saved_gifts.insert(g.info.id, g.clone());
            }
        });
        saved_list_to_items(self, slice.list)
            .into_iter()
            .next()
            .ok_or_else(|| format!("Server returned no saved Star Gift for #{gift_id}."))
    }

    fn chat_star_gift_notifications_enabled(&self, peer_name: &str) -> bool {
        let Ok((peer_id, _)) = resolve_input_peer(self, peer_name) else {
            return false;
        };
        with_sg_state(self, |st| {
            st.chat_notifications_enabled
                .get(&peer_id)
                .copied()
                .unwrap_or(false)
        })
    }

    fn toggle_chat_star_gift_notifications(
        &mut self,
        peer_name: &str,
        enabled: bool,
    ) -> Result<bool, String> {
        let (peer_id, input_peer) = resolve_input_peer(self, peer_name)?;
        let _ = invoke_rpc(
            self,
            build_toggle_chat_star_gift_notifications(input_peer, enabled),
        )?;
        with_sg_state(self, |st| {
            st.chat_notifications_enabled.insert(peer_id, enabled);
        });
        self.bridge_state().notify_ui();
        Ok(enabled)
    }
}

/// Records the server-reported `chat_notifications_enabled` flag for `peer_id` into
/// [`StarGiftsLiveState`] (e.g. after `payments.getSavedStarGifts`).
pub(crate) fn record_chat_star_gift_notifications_enabled(
    live: &LiveSession,
    peer_id: DataPeerId,
    enabled: bool,
) {
    with_sg_state(live, |st| {
        st.chat_notifications_enabled.insert(peer_id, enabled);
    });
}

/// `InfoBackend::chat_star_gift_notifications_enabled` implementation for [`LiveSession`]
/// (`info/peer_gifts/info_peer_gifts_widget.cpp:2655-2666`).
pub(crate) fn info_chat_star_gift_notifications_enabled(
    live: &LiveSession,
    peer_id: tg_app_core::stand_in::types::PeerId,
) -> Option<bool> {
    let data_peer = crate::convert::to_data_peer(peer_id);
    if let Some(cached) = with_sg_state(live, |st| {
        st.chat_notifications_enabled.get(&data_peer).copied()
    }) {
        return Some(cached);
    }
    let is_channel = read_session(live, |s| {
        s.peer(data_peer).is_some_and(|p| p.as_channel().is_some())
    });
    is_channel.then_some(false)
}

/// `InfoBackend::toggle_chat_star_gift_notifications` implementation for [`LiveSession`]:
/// invokes `payments.toggleChatStarGiftNotifications` and updates [`StarGiftsLiveState`]
/// (`info/peer_gifts/info_peer_gifts_widget.cpp:2655-2666`).
pub(crate) fn info_toggle_chat_star_gift_notifications(
    live: &LiveSession,
    peer_id: tg_app_core::stand_in::types::PeerId,
    enabled: bool,
) -> Result<bool, String> {
    let data_peer = crate::convert::to_data_peer(peer_id);
    let Some(input_peer) = read_session(live, |s| {
        s.peer(data_peer).map(|_| input_peer_for(s, data_peer))
    }) else {
        return Err(format!("Peer {} not found in session.", peer_id.0));
    };
    if matches!(input_peer, tl_enums::InputPeer::Empty) {
        return Err(format!("Peer {} not found in session.", peer_id.0));
    }
    let _ = invoke_rpc(
        live,
        build_toggle_chat_star_gift_notifications(input_peer, enabled),
    )?;
    with_sg_state(live, |st| {
        st.chat_notifications_enabled.insert(data_peer, enabled);
    });
    live.bridge_state().notify_ui();
    Ok(enabled)
}

/// Runs a Star Gift withdrawal RPC with a valid temporary 2FA password, matching
/// `GiftPasswordFlow` in `boxes/transfer_gift_box.cpp:227-456`:
/// - Uses a non-expired cached `tmp_password` if available; otherwise obtains one via
///   `account.getPassword` + SRP `account.getTmpPassword(GIFT_WITHDRAW_TMP_PASSWORD_PERIOD_SECS)`.
/// - If the server rejects a cached `tmp_password` (`TMP_PASSWORD_INVALID`,
///   `PASSWORD_HASH_INVALID`, `SRP_PASSWORD_CHANGED`), drops it and retries once with a fresh
///   SRP exchange when `password` is non-empty.
fn invoke_with_gift_tmp_password<R>(
    live: &LiveSession,
    password: &str,
    mut send: impl FnMut(Vec<u8>) -> Result<R, String>,
) -> Result<R, String> {
    if let Some(stored) = cached_valid_tmp_password(live) {
        match send(stored) {
            Ok(res) => return Ok(res),
            Err(err) if is_tmp_password_refused_error(&err) => {
                clear_tmp_password(live);
                if password.is_empty() {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        }
    }
    if password.is_empty() {
        return Err("Cloud 2FA password is required to withdraw a Star Gift.".to_owned());
    }
    let fresh = obtain_tmp_password(live, password, GIFT_WITHDRAW_TMP_PASSWORD_PERIOD_SECS)?;
    match send(fresh) {
        Ok(res) => Ok(res),
        Err(err) => {
            if is_tmp_password_refused_error(&err) {
                clear_tmp_password(live);
            }
            Err(err)
        }
    }
}
