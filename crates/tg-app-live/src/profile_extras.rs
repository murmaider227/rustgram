//! Live Layer-233 RPC glue for WP-26 profile extras, peer boxes, and chat Business-bot bar:
//! - Profile Stories & Albums (`stories.getPinnedStories`, `stories.getStoriesArchive`,
//!   `stories.getAlbums`, `stories.getAlbumStories`)
//! - Peer Star Gifts (`payments.getSavedStarGifts`, `payments.getStarGiftCollections`,
//!   `payments.saveStarGift`) reusing `tg_payments::star_gifts`
//! - Saved Music on Profile (`users.getSavedMusic`, `account.saveMusic`)
//! - Emoji Userpic Builder (`photos.uploadProfilePhoto`, `channels.editPhoto`,
//!   `messages.editChatPhoto` with `videoSizeEmojiMarkup`)
//! - Per-chat allowed reactions (`messages.setChatAvailableReactions`)
//! - Collectible usernames toggle & reorder (`channels.{toggle,reorder}Usernames`,
//!   `account.{toggle,reorder}Usernames`, `bots.{toggle,reorder}Usernames`)
//! - Bot custom verification (`bots.setCustomVerification`)
//! - Business-bot pause/resume/disconnect bar (`account.toggleConnectedBotPaused`,
//!   `account.disablePeerConnectedBot`, `messages.getPeerSettings`)
//!
//! Port origin: `info/stories/*`, `info/peer_gifts/*`, `info/saved/info_saved_music*`,
//! `info/userpic/*`, `boxes/peers/edit_peer_reactions.cpp`, `boxes/peers/edit_peer_usernames_list.cpp`,
//! `boxes/peers/verify_peers_box.cpp`, and `history/view/history_view_contact_status.cpp`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_data::{Peer, PeerId as DataPeerId, Session};
use tg_payments::star_gifts::{
    GIFTS_PER_PAGE, PeerGiftsFilter, SavedStarGift, build_get_saved_star_gifts_filtered,
    build_get_star_gift_collections, build_save_star_gift, gift_collections_from_tl,
    saved_star_gifts_from_tl,
};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::{peer_display_title, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::history::backend::BusinessBotBarState;
use tg_app_core::info::backend::{
    ChatAvailableReactionsMode, EmojiUserpicRequest, PeerReactionsConfig, PeerVerificationStatus,
    ProfileMainTab, ProfilePeerGiftsPage, ProfilePhotoRef, ProfileStoriesPage, ProfileStoryAlbum,
    ProfileStoryItem, ReportReason, SavedMusicTrack, default_standard_reactions,
};
use tg_app_core::info::values::Birthday;
use tg_app_core::payments::backend::ReceivedGiftItem;
use tg_app_core::settings::backend::SupportContact;
use tg_app_core::stand_in::types::PeerId;

/// Default RPC timeout for profile extras operations.
const EXTRAS_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-session live cache for WP-26/WP-58 states (reactions, verifications, business bot bar,
/// main profile tabs).
#[derive(Default)]
struct ProfileExtrasLiveState {
    reactions: HashMap<PeerId, PeerReactionsConfig>,
    verifications: HashMap<(PeerId, PeerId), PeerVerificationStatus>,
    business_bots: HashMap<PeerId, BusinessBotBarState>,
    main_profile_tabs: HashMap<PeerId, ProfileMainTab>,
}

struct ExtrasStateEntry {
    owner: Weak<LiveBridgeState>,
    state: ProfileExtrasLiveState,
}

fn extras_state_registry() -> &'static Mutex<HashMap<usize, ExtrasStateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, ExtrasStateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_extras_state<R>(live: &LiveSession, f: impl FnOnce(&mut ProfileExtrasLiveState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = extras_state_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| ExtrasStateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: ProfileExtrasLiveState::default(),
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
        with_rpc_timeout(EXTRAS_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn input_peer(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let ip = read_session(live, |s| input_peer_for(s, to_data_peer(peer_id)));
    (!matches!(ip, tl_enums::InputPeer::Empty)).then_some(ip)
}

fn input_user(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputUser> {
    let data_peer = to_data_peer(peer_id);
    let uid = data_peer.to_user()?;
    let iu = read_session(live, |s| input_user_for(s, uid));
    if matches!(iu, tl_enums::InputUser::Empty) {
        Some(tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: uid.to_tl(),
            access_hash: 0,
        }))
    } else {
        Some(iu)
    }
}

fn input_channel(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputChannel> {
    let data_peer = to_data_peer(peer_id);
    read_session(live, |s| {
        let ch = s.peer(data_peer)?.as_channel()?;
        Some(tl_enums::InputChannel::InputChannel(
            tl_types::InputChannel {
                channel_id: ch.channel_id().0 as i64,
                access_hash: ch.access_hash as i64,
            },
        ))
    })
}

// =============================================================================
// 1. Profile Stories & Albums (`stories.getPinnedStories`, `getStoriesArchive`, `getAlbums`, `getAlbumStories`)
// =============================================================================

/// Fetches pinned stories for `peer_id` via `stories.getPinnedStories`.
pub fn peer_pinned_stories(
    live: &LiveSession,
    peer_id: PeerId,
    offset_id: i32,
    limit: i32,
) -> Result<ProfileStoriesPage, String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let req = tl_fns::stories::GetPinnedStories {
        peer,
        offset_id,
        limit: limit.clamp(1, 100),
    };
    let res = invoke_rpc(live, req)?;
    Ok(parse_tl_stories_page(live, peer_id, res))
}

/// Fetches archived stories for `peer_id` via `stories.getStoriesArchive`.
pub fn peer_stories_archive(
    live: &LiveSession,
    peer_id: PeerId,
    offset_id: i32,
    limit: i32,
) -> Result<ProfileStoriesPage, String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let req = tl_fns::stories::GetStoriesArchive {
        peer,
        offset_id,
        limit: limit.clamp(1, 100),
    };
    let res = invoke_rpc(live, req)?;
    Ok(parse_tl_stories_page(live, peer_id, res))
}

/// Fetches story albums for `peer_id` via `stories.getAlbums`.
pub fn peer_story_albums(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<Vec<ProfileStoryAlbum>, String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let req = tl_fns::stories::GetAlbums { peer, hash: 0 };
    let res = invoke_rpc(live, req)?;
    match res {
        tl_enums::stories::Albums::Albums(a) => Ok(a
            .albums
            .into_iter()
            .map(|tl_enums::StoryAlbum::StoryAlbum(alb)| ProfileStoryAlbum {
                album_id: alb.album_id,
                title: alb.title,
                count: 0,
            })
            .collect()),
        tl_enums::stories::Albums::NotModified => Ok(Vec::new()),
    }
}

/// Fetches stories belonging to `album_id` via `stories.getAlbumStories`.
pub fn peer_album_stories(
    live: &LiveSession,
    peer_id: PeerId,
    album_id: i32,
    offset: i32,
    limit: i32,
) -> Result<ProfileStoriesPage, String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let req = tl_fns::stories::GetAlbumStories {
        peer,
        album_id,
        offset,
        limit: limit.clamp(1, 100),
    };
    let res = invoke_rpc(live, req)?;
    Ok(parse_tl_stories_page(live, peer_id, res))
}

fn parse_tl_stories_page(
    live: &LiveSession,
    peer_id: PeerId,
    tl_stories: tl_enums::stories::Stories,
) -> ProfileStoriesPage {
    let tl_enums::stories::Stories::Stories(inner) = tl_stories;
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &inner.users, &inner.chats);
    });
    let pinned_to_top = inner.pinned_to_top.unwrap_or_default();
    let mut stories = Vec::with_capacity(inner.stories.len());
    for item in inner.stories {
        if let tl_enums::StoryItem::StoryItem(st) = item {
            let (is_video, duration_secs) = story_media_video_duration(&st.media);
            let views_count = st
                .views
                .map(|tl_enums::StoryViews::StoryViews(v)| v.views_count.max(0) as u32)
                .unwrap_or(0);
            let palette_idx = (st.id.unsigned_abs() % 5) as usize;
            let preview_rgb = [
                [0x4A, 0x8F, 0xD6],
                [0x8B, 0x5C, 0xF6],
                [0x2E, 0xA6, 0x52],
                [0xE6, 0x7E, 0x22],
                [0xD6, 0x4A, 0x6E],
            ][palette_idx];
            stories.push(ProfileStoryItem {
                id: st.id,
                peer_id,
                date: st.date,
                caption: st.caption.unwrap_or_default(),
                pinned: st.pinned || pinned_to_top.contains(&st.id),
                is_video,
                duration_secs,
                views_count,
                album_ids: st.albums.unwrap_or_default(),
                preview_rgb,
            });
        }
    }
    ProfileStoriesPage {
        total_count: inner.count,
        stories,
        pinned_to_top,
    }
}

fn story_media_video_duration(media: &tl_enums::MessageMedia) -> (bool, u32) {
    if let tl_enums::MessageMedia::Document(doc_media) = media
        && let Some(tl_enums::Document::Document(doc)) = &doc_media.document
    {
        for attr in &doc.attributes {
            if let tl_enums::DocumentAttribute::Video(v) = attr {
                return (true, v.duration.max(0.0).round() as u32);
            }
        }
    }
    (false, 0)
}

// =============================================================================
// 2. Peer Star Gifts (`payments.getSavedStarGifts`, `getStarGiftCollections`, `saveStarGift`)
// =============================================================================

/// Fetches saved Star Gifts and collections for `peer_id` reusing `tg_payments::star_gifts`.
pub fn peer_saved_gifts(
    live: &LiveSession,
    peer_id: PeerId,
    filter: &PeerGiftsFilter,
) -> Result<ProfilePeerGiftsPage, String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let data_peer = to_data_peer(peer_id);
    let is_self = read_session(live, |s| s.self_peer_id() == data_peer);

    let saved_tl = invoke_rpc(
        live,
        build_get_saved_star_gifts_filtered(peer.clone(), filter, "", GIFTS_PER_PAGE),
    )?;
    let tl_enums::payments::SavedStarGifts::SavedStarGifts(ref inner) = saved_tl;
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &inner.users, &inner.chats);
    });
    let slice = saved_star_gifts_from_tl(&saved_tl, data_peer, is_self);
    if let Some(enabled) = slice.chat_notifications_enabled {
        crate::star_gifts::record_chat_star_gift_notifications_enabled(live, data_peer, enabled);
    }

    let collections = match invoke_rpc(live, build_get_star_gift_collections(peer, 0)) {
        Ok(cols_tl) => gift_collections_from_tl(&cols_tl).unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let gifts: Vec<ReceivedGiftItem> = read_session(live, |s| {
        slice
            .list
            .iter()
            .map(|saved| saved_gift_to_received_item(saved, s))
            .collect()
    });

    Ok(ProfilePeerGiftsPage {
        total_count: slice.count,
        gifts,
        collections,
    })
}

/// Shows or hides a saved Star Gift on `peer_id` via `payments.saveStarGift`.
pub fn toggle_peer_gift_saved(
    live: &LiveSession,
    peer_id: PeerId,
    msg_id: i32,
    saved_id: u64,
    unsave: bool,
) -> Result<(), String> {
    let stargift = if msg_id != 0 {
        tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser { msg_id })
    } else {
        let Some(peer) = input_peer(live, peer_id) else {
            return Err("Peer not found.".to_owned());
        };
        tl_enums::InputSavedStarGift::Chat(tl_types::InputSavedStarGiftChat {
            peer,
            saved_id: saved_id as i64,
        })
    };
    let _ = invoke_rpc(live, build_save_star_gift(stargift, unsave))?;
    live.bridge_state().notify_ui();
    Ok(())
}

fn saved_gift_to_received_item(saved: &SavedStarGift, session: &Session) -> ReceivedGiftItem {
    let title = if let Some(ref unique) = saved.info.unique {
        unique.display_name()
    } else if !saved.info.resell_title.is_empty() {
        saved.info.resell_title.clone()
    } else {
        format!("Star Gift #{}", saved.info.id)
    };
    let sender_name = if saved.anonymous || saved.from_id == DataPeerId::ZERO {
        "Anonymous".to_owned()
    } else {
        peer_display_title(session, saved.from_id)
    };
    let artwork_kind = if saved.info.unique.is_some() {
        "gem"
    } else if saved.info.birthday {
        "cake"
    } else {
        "star"
    };
    ReceivedGiftItem {
        saved: saved.clone(),
        title,
        sender_name,
        artwork_kind: artwork_kind.to_owned(),
    }
}

// =============================================================================
// 3. Saved Profile Music (`users.getSavedMusic`, `account.saveMusic`)
// =============================================================================

/// Fetches saved profile audio tracks for `peer_id` via `users.getSavedMusic`.
pub fn peer_saved_music(
    live: &LiveSession,
    peer_id: PeerId,
    offset: i32,
    limit: i32,
) -> Result<Vec<SavedMusicTrack>, String> {
    let Some(id) = input_user(live, peer_id) else {
        return Ok(Vec::new());
    };
    let req = tl_fns::users::GetSavedMusic {
        id,
        offset: offset.max(0),
        limit: limit.clamp(1, 100),
        hash: 0,
    };
    let res = invoke_rpc(live, req)?;
    match res {
        tl_enums::users::SavedMusic::SavedMusic(m) => Ok(m
            .documents
            .into_iter()
            .filter_map(tl_doc_to_saved_music)
            .collect()),
        tl_enums::users::SavedMusic::NotModified(_) => Ok(Vec::new()),
    }
}

/// Saves or removes a music track from the signed-in user's profile via `account.saveMusic`.
pub fn save_profile_music(
    live: &LiveSession,
    track: &SavedMusicTrack,
    unsave: bool,
) -> Result<(), String> {
    let id = tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
        id: track.document_id,
        access_hash: track.access_hash,
        file_reference: track.file_reference.clone(),
    });
    let req = tl_fns::account::SaveMusic {
        unsave,
        id,
        after_id: None,
    };
    let _ = invoke_rpc(live, req)?;
    crate::live_extras::record_saved_music_mutation(live, track.document_id, unsave);
    live.bridge_state().notify_ui();
    Ok(())
}

fn tl_doc_to_saved_music(doc: tl_enums::Document) -> Option<SavedMusicTrack> {
    let tl_enums::Document::Document(d) = doc else {
        return None;
    };
    let mut title = String::new();
    let mut performer = String::new();
    let mut duration_secs = 0u32;
    for attr in &d.attributes {
        match attr {
            tl_enums::DocumentAttribute::Audio(a) => {
                duration_secs = a.duration.max(0) as u32;
                if let Some(ref t) = a.title
                    && !t.is_empty()
                {
                    title = t.clone();
                }
                if let Some(ref p) = a.performer
                    && !p.is_empty()
                {
                    performer = p.clone();
                }
            }
            tl_enums::DocumentAttribute::Filename(f) if title.is_empty() => {
                title = f.file_name.clone();
            }
            _ => {}
        }
    }
    if title.is_empty() {
        title = format!("Audio #{}", d.id);
    }
    if performer.is_empty() {
        performer = "Unknown Artist".to_owned();
    }
    Some(SavedMusicTrack {
        document_id: d.id,
        access_hash: d.access_hash,
        file_reference: d.file_reference,
        title,
        performer,
        duration_secs,
        size_bytes: d.size.max(0) as u64,
        saved_on_profile: true,
    })
}

// =============================================================================
// 4. Emoji Userpic Builder (`photos.uploadProfilePhoto` / `channels.editPhoto` / `messages.editChatPhoto`)
// =============================================================================

/// Uploads an emoji + gradient userpic encoded as `videoSizeEmojiMarkup`.
pub fn upload_emoji_userpic(
    live: &LiveSession,
    peer_id: PeerId,
    req: EmojiUserpicRequest,
) -> Result<(), String> {
    if req.background_colors.is_empty() || req.background_colors.len() > 4 {
        return Err("Background must have between 1 and 4 colors.".to_owned());
    }
    let bg_colors: Vec<i32> = req
        .background_colors
        .iter()
        .map(|&c| (c & 0x00FF_FFFF) as i32)
        .collect();
    let markup = tl_enums::VideoSize::EmojiMarkup(tl_types::VideoSizeEmojiMarkup {
        emoji_id: req.emoji_id,
        background_colors: bg_colors,
    });

    let data_peer = to_data_peer(peer_id);
    if data_peer.is_channel() {
        let Some(channel) = input_channel(live, peer_id) else {
            return Err("Channel not found.".to_owned());
        };
        let photo =
            tl_enums::InputChatPhoto::InputChatUploadedPhoto(tl_types::InputChatUploadedPhoto {
                file: None,
                video: None,
                video_start_ts: None,
                video_emoji_markup: Some(markup),
            });
        let _ = invoke_rpc(live, tl_fns::channels::EditPhoto { channel, photo })?;
    } else if let Some(chat_id) = data_peer.to_chat() {
        let photo =
            tl_enums::InputChatPhoto::InputChatUploadedPhoto(tl_types::InputChatUploadedPhoto {
                file: None,
                video: None,
                video_start_ts: None,
                video_emoji_markup: Some(markup),
            });
        let _ = invoke_rpc(
            live,
            tl_fns::messages::EditChatPhoto {
                chat_id: chat_id.to_tl(),
                photo,
            },
        )?;
    } else {
        let is_self = read_session(live, |s| s.self_peer_id() == data_peer);
        let bot = if is_self {
            None
        } else {
            let is_bot = read_session(live, |s| {
                s.peer(data_peer)
                    .and_then(Peer::as_user)
                    .is_some_and(|u| u.is_bot())
            });
            if is_bot {
                input_user(live, peer_id)
            } else {
                None
            }
        };
        let _ = invoke_rpc(
            live,
            tl_fns::photos::UploadProfilePhoto {
                fallback: false,
                bot,
                file: None,
                video: None,
                video_start_ts: None,
                video_emoji_markup: Some(markup),
            },
        )?;
    }
    live.bridge_state().notify_ui();
    Ok(())
}

// =============================================================================
// 5. Per-Chat Available Reactions (`messages.setChatAvailableReactions`)
// =============================================================================

/// Returns cached or default reactions configuration for `peer_id`.
pub fn peer_available_reactions(live: &LiveSession, peer_id: PeerId) -> PeerReactionsConfig {
    with_extras_state(live, |st| {
        st.reactions
            .get(&peer_id)
            .cloned()
            .unwrap_or_else(|| PeerReactionsConfig {
                mode: ChatAvailableReactionsMode::All { allow_custom: true },
                reactions_limit: Some(11),
                paid_enabled: false,
                available_standard: default_standard_reactions(),
            })
    })
}

/// Saves allowed reactions for `peer_id` via `messages.setChatAvailableReactions`.
pub fn set_peer_available_reactions(
    live: &LiveSession,
    peer_id: PeerId,
    config: PeerReactionsConfig,
) -> Result<(), String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let available_reactions = match &config.mode {
        ChatAvailableReactionsMode::None => tl_enums::ChatReactions::None,
        ChatAvailableReactionsMode::All { allow_custom } => {
            tl_enums::ChatReactions::All(tl_types::ChatReactionsAll {
                allow_custom: *allow_custom,
            })
        }
        ChatAvailableReactionsMode::Some {
            emojis,
            custom_emoji_ids,
        } => {
            let mut reactions = Vec::with_capacity(emojis.len() + custom_emoji_ids.len());
            for emoticon in emojis {
                reactions.push(tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: emoticon.clone(),
                }));
            }
            for &document_id in custom_emoji_ids {
                reactions.push(tl_enums::Reaction::CustomEmoji(
                    tl_types::ReactionCustomEmoji { document_id },
                ));
            }
            tl_enums::ChatReactions::Some(tl_types::ChatReactionsSome { reactions })
        }
    };

    let req = tl_fns::messages::SetChatAvailableReactions {
        peer,
        available_reactions,
        reactions_limit: config.reactions_limit,
        paid_enabled: Some(config.paid_enabled),
    };
    let _ = invoke_rpc(live, req)?;
    with_extras_state(live, |st| {
        st.reactions.insert(peer_id, config);
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// =============================================================================
// 6. Collectible Usernames (`channels.{toggle,reorder}Usernames`, `account.*`, `bots.*`)
// =============================================================================

/// Activates or deactivates a collectible username on a channel, bot, or self account.
pub fn toggle_peer_username(
    live: &LiveSession,
    peer_id: PeerId,
    username: &str,
    active: bool,
) -> Result<(), String> {
    let clean = username.trim().trim_start_matches('@').to_owned();
    if clean.is_empty() {
        return Err("Username cannot be empty.".to_owned());
    }
    let data_peer = to_data_peer(peer_id);
    if data_peer.is_channel() {
        let Some(channel) = input_channel(live, peer_id) else {
            return Err("Channel not found.".to_owned());
        };
        let _ = invoke_rpc(
            live,
            tl_fns::channels::ToggleUsername {
                channel,
                username: clean,
                active,
            },
        )?;
    } else {
        let is_bot = read_session(live, |s| {
            s.peer(data_peer)
                .and_then(Peer::as_user)
                .is_some_and(|u| u.is_bot())
        });
        if is_bot {
            let Some(bot) = input_user(live, peer_id) else {
                return Err("Bot user not found.".to_owned());
            };
            let _ = invoke_rpc(
                live,
                tl_fns::bots::ToggleUsername {
                    bot,
                    username: clean,
                    active,
                },
            )?;
        } else {
            let _ = invoke_rpc(
                live,
                tl_fns::account::ToggleUsername {
                    username: clean,
                    active,
                },
            )?;
        }
    }
    live.bridge_state().notify_ui();
    Ok(())
}

/// Reorders active usernames on a channel, bot, or self account.
pub fn reorder_peer_usernames(
    live: &LiveSession,
    peer_id: PeerId,
    order: &[String],
) -> Result<(), String> {
    let clean_order: Vec<String> = order
        .iter()
        .map(|u| u.trim().trim_start_matches('@').to_owned())
        .filter(|u| !u.is_empty())
        .collect();
    let data_peer = to_data_peer(peer_id);
    if data_peer.is_channel() {
        let Some(channel) = input_channel(live, peer_id) else {
            return Err("Channel not found.".to_owned());
        };
        let _ = invoke_rpc(
            live,
            tl_fns::channels::ReorderUsernames {
                channel,
                order: clean_order,
            },
        )?;
    } else {
        let is_bot = read_session(live, |s| {
            s.peer(data_peer)
                .and_then(Peer::as_user)
                .is_some_and(|u| u.is_bot())
        });
        if is_bot {
            let Some(bot) = input_user(live, peer_id) else {
                return Err("Bot user not found.".to_owned());
            };
            let _ = invoke_rpc(
                live,
                tl_fns::bots::ReorderUsernames {
                    bot,
                    order: clean_order,
                },
            )?;
        } else {
            let _ = invoke_rpc(
                live,
                tl_fns::account::ReorderUsernames { order: clean_order },
            )?;
        }
    }
    live.bridge_state().notify_ui();
    Ok(())
}

// =============================================================================
// 7. Bot Custom Verification (`bots.setCustomVerification`)
// =============================================================================

/// Returns cached bot custom verification status for `(bot_id, peer_id)`.
pub fn peer_verification_status(
    live: &LiveSession,
    bot_id: PeerId,
    peer_id: PeerId,
) -> PeerVerificationStatus {
    with_extras_state(live, |st| {
        st.verifications
            .get(&(bot_id, peer_id))
            .cloned()
            .unwrap_or(PeerVerificationStatus {
                bot_id,
                peer_id,
                verified: false,
                custom_description: String::new(),
            })
    })
}

/// Grants or revokes third-party bot verification via `bots.setCustomVerification`.
pub fn set_peer_custom_verification(
    live: &LiveSession,
    bot_id: PeerId,
    peer_id: PeerId,
    enabled: bool,
    custom_description: Option<&str>,
) -> Result<(), String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Target peer not found.".to_owned());
    };
    let bot = input_user(live, bot_id);
    let desc_clean = custom_description
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let Some(ref d) = desc_clean
        && d.chars().count() > 70
    {
        return Err("Custom verification description must be at most 70 characters.".to_owned());
    }
    let tl_desc = if enabled {
        desc_clean.as_ref().map(|text| {
            tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: text.clone(),
                entities: Vec::new(),
            })
        })
    } else {
        None
    };

    let req = tl_fns::bots::SetCustomVerification {
        enabled,
        bot,
        peer,
        custom_description: tl_desc,
    };
    let _ = invoke_rpc(live, req)?;
    with_extras_state(live, |st| {
        if enabled {
            st.verifications.insert(
                (bot_id, peer_id),
                PeerVerificationStatus {
                    bot_id,
                    peer_id,
                    verified: true,
                    custom_description: desc_clean.unwrap_or_default(),
                },
            );
        } else {
            st.verifications.remove(&(bot_id, peer_id));
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// =============================================================================
// 8. Chat Business-Bot Pause / Resume / Disable Bar
// =============================================================================

/// Updates cached Business-bot bar state for `peer_id` (e.g. after `messages.getPeerSettings`).
pub fn set_business_bot_bar_state(
    live: &LiveSession,
    peer_id: PeerId,
    state: Option<BusinessBotBarState>,
) {
    with_extras_state(live, |st| {
        if let Some(s) = state {
            st.business_bots.insert(peer_id, s);
        } else {
            st.business_bots.remove(&peer_id);
        }
    });
}

/// Fetches `messages.getPeerSettings` for `peer_id` and populates the Business-bot bar cache.
pub fn refresh_business_bot_bar(
    live: &LiveSession,
    peer_id: PeerId,
) -> Result<Option<BusinessBotBarState>, String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let res = invoke_rpc(live, tl_fns::messages::GetPeerSettings { peer })?;
    let tl_enums::messages::PeerSettings::PeerSettings(inner) = res;
    write_session(live, |s| {
        apply_tl_users_and_chats(s, &inner.users, &inner.chats);
    });
    let tl_enums::PeerSettings::PeerSettings(ps) = inner.settings;
    let bar = ps.business_bot_id.map(|raw_bot_id| {
        let bot_data_peer = DataPeerId::from_user(tg_data::UserId::from_tl(raw_bot_id));
        let (bot_name, bot_username) = read_session(live, |s| {
            let name = peer_display_title(s, bot_data_peer);
            let uname = s
                .peer(bot_data_peer)
                .and_then(Peer::as_user)
                .map(|u| u.usernames.username())
                .filter(|u| !u.is_empty())
                .map(str::to_owned);
            (name, uname)
        });
        BusinessBotBarState {
            bot_id: PeerId::from_user(raw_bot_id as u64),
            bot_name,
            bot_username,
            manage_url: ps.business_bot_manage_url,
            can_reply: ps.business_bot_can_reply,
            paused: ps.business_bot_paused,
        }
    });
    set_business_bot_bar_state(live, peer_id, bar.clone());
    Ok(bar)
}

/// Returns the cached Business-bot bar state for `peer_id`, if any.
pub fn business_bot_bar(live: &LiveSession, peer_id: PeerId) -> Option<BusinessBotBarState> {
    with_extras_state(live, |st| st.business_bots.get(&peer_id).cloned())
}

/// Pauses or resumes the connected Business bot in `peer_id` via `account.toggleConnectedBotPaused`.
pub fn toggle_business_bot_paused(
    live: &LiveSession,
    peer_id: PeerId,
    paused: bool,
) -> Result<(), String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let _ = invoke_rpc(
        live,
        tl_fns::account::ToggleConnectedBotPaused { peer, paused },
    )?;
    with_extras_state(live, |st| {
        if let Some(bot) = st.business_bots.get_mut(&peer_id) {
            bot.paused = paused;
        }
    });
    live.bridge_state().notify_ui();
    Ok(())
}

/// Disconnects the Business bot from `peer_id` via `account.disablePeerConnectedBot`.
pub fn disable_business_bot(live: &LiveSession, peer_id: PeerId) -> Result<(), String> {
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let _ = invoke_rpc(live, tl_fns::account::DisablePeerConnectedBot { peer })?;
    with_extras_state(live, |st| {
        st.business_bots.remove(&peer_id);
    });
    live.bridge_state().notify_ui();
    Ok(())
}

// =============================================================================
// 9. Main Profile Tab (`account.setMainProfileTab`, `channels.setMainProfileTab`)
// =============================================================================

/// Converts [`ProfileMainTab`] to TL [`tl_enums::ProfileTab`] matching
/// `Data::ProfileTabToMTP` (`data_peer.cpp:2327-2340`).
#[must_use]
pub fn profile_tab_to_tl(tab: ProfileMainTab) -> tl_enums::ProfileTab {
    match tab {
        ProfileMainTab::Posts => tl_enums::ProfileTab::Posts,
        ProfileMainTab::Gifts => tl_enums::ProfileTab::Gifts,
        ProfileMainTab::Media => tl_enums::ProfileTab::Media,
        ProfileMainTab::Files => tl_enums::ProfileTab::Files,
        ProfileMainTab::Music => tl_enums::ProfileTab::Music,
        ProfileMainTab::Voice => tl_enums::ProfileTab::Voice,
        ProfileMainTab::Links => tl_enums::ProfileTab::Links,
        ProfileMainTab::Gifs => tl_enums::ProfileTab::Gifs,
    }
}

/// Sets the main profile tab for `peer_id` matching `TabsHost::setMainTab`
/// (`info_profile_tabs_host.cpp:408-422`): channels call `channels.setMainProfileTab`,
/// while user accounts call `account.setMainProfileTab`.
pub fn set_main_profile_tab(
    live: &LiveSession,
    peer_id: PeerId,
    tab: ProfileMainTab,
) -> Result<(), String> {
    let tl_tab = profile_tab_to_tl(tab);
    let data_peer = to_data_peer(peer_id);
    if data_peer.is_channel() {
        let Some(channel) = input_channel(live, peer_id) else {
            return Err("Channel not found.".to_owned());
        };
        let _ = invoke_rpc(
            live,
            tl_fns::channels::SetMainProfileTab {
                channel,
                tab: tl_tab,
            },
        )?;
    } else {
        let _ = invoke_rpc(live, tl_fns::account::SetMainProfileTab { tab: tl_tab })?;
    }
    with_extras_state(live, |st| {
        st.main_profile_tabs.insert(peer_id, tab);
    });
    live.bridge_state().notify_ui();
    Ok(())
}

/// Returns the cached main profile tab for `peer_id`, if set in this session.
#[must_use]
pub fn main_profile_tab(live: &LiveSession, peer_id: PeerId) -> Option<ProfileMainTab> {
    with_extras_state(live, |st| st.main_profile_tabs.get(&peer_id).copied())
}

// =============================================================================
// 10. Suggest Birthday (`users.suggestBirthday`, `core/local_url_handlers.cpp:1013-1035`)
// =============================================================================

fn is_valid_birthday(birthday: Birthday) -> bool {
    if !(1..=12).contains(&birthday.month) || birthday.day == 0 {
        return false;
    }
    let max_day = match birthday.month {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if birthday.day > max_day {
        return false;
    }
    birthday.year.is_none_or(|y| (1900..=2100).contains(&y))
}

/// Suggests a birthday to another user via `users.suggestBirthday`
/// (`core/local_url_handlers.cpp:1013-1035`).
pub fn suggest_user_birthday(
    live: &LiveSession,
    peer_id: PeerId,
    birthday: Birthday,
) -> Result<(), String> {
    if !is_valid_birthday(birthday) {
        return Err("Invalid birthday date.".to_owned());
    }
    let Some(id) = input_user(live, peer_id) else {
        return Err("Target user not found.".to_owned());
    };
    let tl_birthday = tl_enums::Birthday::Birthday(tl_types::Birthday {
        day: i32::from(birthday.day),
        month: i32::from(birthday.month),
        year: birthday.year.filter(|&y| y > 0).map(i32::from),
    });
    let updates = invoke_rpc(
        live,
        tl_fns::users::SuggestBirthday {
            id,
            birthday: tl_birthday,
        },
    )?;
    let _ = live.apply_updates_to_session(&updates);
    Ok(())
}

// =============================================================================
// 11. Report Profile Photo (`account.reportProfilePhoto`, `api_report.cpp:71-85`)
// =============================================================================

fn report_reason_to_tl(reason: ReportReason) -> tl_enums::ReportReason {
    match reason {
        ReportReason::Spam => tl_enums::ReportReason::InputReportReasonSpam,
        ReportReason::Violence => tl_enums::ReportReason::InputReportReasonViolence,
        ReportReason::Pornography => tl_enums::ReportReason::InputReportReasonPornography,
        ReportReason::ChildAbuse => tl_enums::ReportReason::InputReportReasonChildAbuse,
        ReportReason::Copyright => tl_enums::ReportReason::InputReportReasonCopyright,
        ReportReason::IllegalDrugs => tl_enums::ReportReason::InputReportReasonIllegalDrugs,
        ReportReason::PersonalDetails => tl_enums::ReportReason::InputReportReasonPersonalDetails,
        ReportReason::Fake => tl_enums::ReportReason::InputReportReasonFake,
        ReportReason::Other => tl_enums::ReportReason::InputReportReasonOther,
    }
}

/// Reports a peer's profile photo via `account.reportProfilePhoto`
/// (`Api::SendPhotoReport` in `api_report.cpp:71-85`).
pub fn report_profile_photo(
    live: &LiveSession,
    peer_id: PeerId,
    photo: ProfilePhotoRef,
    reason: ReportReason,
    comment: &str,
) -> Result<(), String> {
    if photo.id == 0 {
        return Err("Profile photo ID cannot be zero.".to_owned());
    }
    let Some(peer) = input_peer(live, peer_id) else {
        return Err("Peer not found.".to_owned());
    };
    let photo_id = tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
        id: photo.id,
        access_hash: photo.access_hash,
        file_reference: photo.file_reference,
    });
    let _ = invoke_rpc(
        live,
        tl_fns::account::ReportProfilePhoto {
            peer,
            photo_id,
            reason: report_reason_to_tl(reason),
            message: comment.to_owned(),
        },
    )?;
    live.bridge_state().notify_ui();
    Ok(())
}

// =============================================================================
// 12. Confirm Bot Connection (`account.confirmBotConnection`, `api_authorizations.cpp:447-460`)
// =============================================================================

/// Confirms an unreviewed connected Business bot authorization via `account.confirmBotConnection`.
///
/// Mirrors `Api::Authorizations::review` (`api_authorizations.cpp:447-460`): if the bot's
/// `access_hash` is not yet cached in `Session`, fetches `account.getConnectedBots` first to
/// populate the user entry before invoking `account.confirmBotConnection`.
pub fn confirm_bot_connection(live: &LiveSession, bot_id: i64) -> Result<bool, String> {
    if bot_id <= 0 {
        return Err("Bot user ID must be positive.".to_owned());
    }
    let uid = tg_data::UserId::from_tl(bot_id);
    let mut bot_input = read_session(live, |s| input_user_for(s, uid));
    let needs_access_hash = match &bot_input {
        tl_enums::InputUser::InputUser(u) => u.access_hash == 0,
        tl_enums::InputUser::Empty => true,
        _ => false,
    };
    if needs_access_hash
        && let Ok(tl_enums::account::ConnectedBots::ConnectedBots(resp)) =
            invoke_rpc(live, tl_fns::account::GetConnectedBots {})
    {
        write_session(live, |s| {
            apply_tl_users_and_chats(s, &resp.users, &[]);
        });
        bot_input = read_session(live, |s| input_user_for(s, uid));
    }
    if matches!(bot_input, tl_enums::InputUser::Empty) {
        return Err("Connected bot user not found in session.".to_owned());
    }
    let ok = invoke_rpc(
        live,
        tl_fns::account::ConfirmBotConnection { bot_id: bot_input },
    )?;
    live.bridge_state().notify_ui();
    Ok(ok)
}

// =============================================================================
// 13. Telegram Support Contact (`help.getSupport`, `settings_main.cpp:1231-1251`)
// =============================================================================

/// Fetches the Telegram Support volunteer account via `help.getSupport` and registers its
/// user record in the local `Session` (`OpenAskQuestionConfirm` in `settings_main.cpp:1237-1246`).
pub fn get_support_contact(live: &LiveSession) -> Result<SupportContact, String> {
    let tl_enums::help::Support::Support(support) = invoke_rpc(live, tl_fns::help::GetSupport {})?;
    let user_id = match &support.user {
        tl_enums::User::User(u) => u.id,
        tl_enums::User::Empty(e) => e.id,
    };
    write_session(live, |s| {
        apply_tl_users_and_chats(s, std::slice::from_ref(&support.user), &[]);
    });
    let display_name = if user_id > 0 {
        let data_peer = DataPeerId::from_user(tg_data::UserId::from_tl(user_id));
        read_session(live, |s| peer_display_title(s, data_peer))
    } else {
        "Telegram Support".to_owned()
    };
    live.bridge_state().notify_ui();
    Ok(SupportContact {
        phone_number: support.phone_number,
        user_id,
        display_name,
    })
}
