//! WP-71 `FakeRpcClient` verification suite:
//! 1. Real `InfoBackend::active_profile_photo_ref` resolution from cached `UserFull` /
//!    `ChatFull` / `ChannelFull` and `photos.getUserPhotos` fallback, plus rejection of
//!    bogus placeholder `InputPhoto` refs (`id == 0 || access_hash == 0`) in
//!    `photos.deletePhotos`, `account.reportProfilePhoto`, and `messages.getAttachedStickers`.
//! 2. Optimistic local strip of `participant_id`'s reactions across cached messages in
//!    `peer_id` on `messages.deleteParticipantReactions` (`Session::removeReactionsFromParticipant`).
//! 3. Live / background hook wiring for:
//!    - `account.getSavedMusicIds` (triggered on `peer_saved_music`, respecting 30 s throttle)
//!    - `messages.getAttachMenuBot` (via `BotsBackend::load_attach_menu_bot` & add hydration)
//!    - `help.saveAppLog` (via `PaymentsBackend::log_premium_promo_event`, `peer == 0`)
//!    - `contacts.getStatuses` (periodic throttle, reconnect, and `PrivacyKey::LastSeen` change)
//!    - `messages.getEmojiKeywordsLanguages` (triggered on live emoji keyword search with 1 h TTL)
//!    - `messages.getMessagesReactions` (triggered on visible reacted group/channel messages with
//!      20 s `kPollEach` throttle, skipping 1:1 user chats)
//!
//! Zero network traffic; synthetic IDs and fictional names only.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::cached_lists::EMOJI_KEYWORDS_REFRESH_MS;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app_core::bots::backend::BotsBackend;
use tg_app_core::chat_helpers::backend::{AttachedStickersMedia, ChatHelpersBackend};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::info::backend::{InfoBackend, ProfilePhotoRef, ReportReason};
use tg_app_core::payments::backend::{PaymentsBackend, PremiumPromoLogEvent};
use tg_app_core::settings::backend::{PrivacyKey, PrivacyRule, SettingsBackend};
use tg_app_core::stand_in::backend::Backend;
use tg_app_core::stand_in::types::MsgId;
use tg_bots::mini_app::AttachMenuToggleState;
use tg_bots::stand_in::UserId as BotUserId;
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatId, FullMsgId as DataFullMsgId, Message,
    MessageFlags, MessageReactionCount, MessageReactionsSummary, MsgId as DataMsgId,
    PeerId as DataPeerId, PeerUpdateFlags, PhotoId, ReactionId, RecentReactor,
    TextWithEntities as DataText, User, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, Deserializable, Identifiable, RemoteCall, Serializable,
    enums as tl_enums, functions as tl_fns, types as tl_types,
};
use tokio::sync::broadcast;

use crate::contacts_extras::CONTACT_STATUSES_REFRESH_INTERVAL_MS;
use crate::convert::from_data_peer;
use crate::session::LiveSession;
use crate::stickers_reactions_extras::REACTIONS_POLL_INTERVAL_MS;

type RecordedRpc = (u32, Vec<u8>);
type QueuedReply = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RecordedRpc>>>,
    responses: Arc<Mutex<VecDeque<QueuedReply>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn enqueue_bool(&self, expected_ctor: u32, value: bool) {
        let ctor = if value { BOOL_TRUE_ID } else { BOOL_FALSE_ID };
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(ctor.to_le_bytes().to_vec())));
    }

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes(bytes[..4].try_into().unwrap_or([0; 4]));
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        let popped = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
        };
        match popped {
            Some(Ok(raw)) => <R::Return as Deserializable>::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string())),
            Some(Err(err)) => Err(err),
            None => Err(InvocationError::Dropped),
        }
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

const BASE_NOW_MS: i64 = 1_750_000_000_000;
const SELF_UID: u64 = 100;
const PEER_UID: u64 = 201;
const PEER_U_HASH: u64 = 20_100_999;
const OTHER_UID: u64 = 202;
const OTHER_U_HASH: u64 = 20_200_888;
const BOT_UID: u64 = 777;
const BOT_HASH: u64 = 77_700_555;
const CHAT_ID: u64 = 400_001;
const CHANNEL_ID: u64 = 500_001;
const CHANNEL_HASH: u64 = 50_001_777;

fn make_live_session() -> (tokio::runtime::Runtime, FakeRpcClient, LiveSession) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = FakeRpcClient::new();
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_750_000_000,
            seq: 0,
            unread_count: 0,
        }),
    );
    let runtime = SessionRuntime::start_on(
        DataUserId(SELF_UID),
        crate::ErasedRpcClient::new(rpc.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: false,
            idle_ping_enabled: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(runtime), 71);
    live.set_chat_helpers_now_ms(Some(BASE_NOW_MS));

    rt.block_on(async {
        let mut s = live.runtime().session().write().await;

        let mut self_u = User::new(DataUserId(SELF_UID), true);
        self_u.set_name("Self", "Account", "", "");
        self_u.common.loaded_status = tg_data::LoadedStatus::Normal;
        s.upsert_user(self_u, PeerUpdateFlags::NAME);

        let mut peer_u = User::new(DataUserId(PEER_UID), false);
        peer_u.set_name("Fictional", "Contact", "", "");
        peer_u.access_hash = PEER_U_HASH;
        peer_u.common.loaded_status = tg_data::LoadedStatus::Normal;
        s.upsert_user(peer_u, PeerUpdateFlags::NAME);

        let mut other_u = User::new(DataUserId(OTHER_UID), false);
        other_u.set_name("Second", "Member", "", "");
        other_u.access_hash = OTHER_U_HASH;
        other_u.common.loaded_status = tg_data::LoadedStatus::Normal;
        s.upsert_user(other_u, PeerUpdateFlags::NAME);

        let mut bot_u = User::new(DataUserId(BOT_UID), false);
        bot_u.set_name("Helper", "Bot", "", "helper_test_bot");
        bot_u.access_hash = BOT_HASH;
        bot_u.bot_info = Some(tg_data::BotInfo::default());
        bot_u.common.loaded_status = tg_data::LoadedStatus::Normal;
        s.upsert_user(bot_u, PeerUpdateFlags::NAME);

        let mut chat = Chat::new(ChatId(CHAT_ID));
        chat.set_name("Basic Team Chat");
        chat.common.loaded_status = tg_data::LoadedStatus::Normal;
        s.upsert_chat(chat, PeerUpdateFlags::NAME);

        let mut ch = Channel::new(ChannelId(CHANNEL_ID));
        ch.set_name("Demo Supergroup", "");
        ch.access_hash = CHANNEL_HASH;
        ch.flags |= ChannelFlags::MEGAGROUP;
        ch.common.loaded_status = tg_data::LoadedStatus::Normal;
        s.upsert_channel(ch, PeerUpdateFlags::NAME);
    });

    (rt, rpc, live)
}

fn make_tl_photo(id: i64, access_hash: i64, file_reference: Vec<u8>) -> tl_enums::Photo {
    tl_enums::Photo::Photo(tl_types::Photo {
        has_stickers: true,
        id,
        access_hash,
        file_reference,
        date: 1_750_000_000,
        sizes: Vec::new(),
        video_sizes: None,
        dc_id: 2,
    })
}

fn make_empty_notify_settings() -> tl_enums::PeerNotifySettings {
    tl_enums::PeerNotifySettings::PeerNotifySettings(tl_types::PeerNotifySettings {
        show_previews: None,
        silent: None,
        mute_until: None,
        ios_sound: None,
        android_sound: None,
        other_sound: None,
        stories_muted: None,
        stories_hide_sender: None,
        stories_ios_sound: None,
        stories_android_sound: None,
        stories_other_sound: None,
    })
}

fn make_empty_peer_settings() -> tl_enums::PeerSettings {
    tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
        report_spam: false,
        add_contact: false,
        block_contact: false,
        share_contact: false,
        need_contacts_exception: false,
        report_geo: false,
        autoarchived: false,
        invite_members: false,
        request_chat_broadcast: false,
        business_bot_paused: false,
        business_bot_can_reply: false,
        geo_distance: None,
        request_chat_title: None,
        request_chat_date: None,
        business_bot_id: None,
        business_bot_manage_url: None,
        charge_paid_message_stars: None,
        registration_month: None,
        phone_country: None,
        name_change_date: None,
        photo_change_date: None,
    })
}

fn make_tl_user_full(
    id: i64,
    profile_photo: Option<tl_enums::Photo>,
    personal_photo: Option<tl_enums::Photo>,
    fallback_photo: Option<tl_enums::Photo>,
) -> tl_enums::users::UserFull {
    tl_enums::users::UserFull::UserFull(tl_types::users::UserFull {
        full_user: tl_enums::UserFull::UserFull(tl_types::UserFull {
            blocked: false,
            phone_calls_available: false,
            phone_calls_private: false,
            can_pin_message: false,
            has_scheduled: false,
            video_calls_available: false,
            voice_messages_forbidden: false,
            translations_disabled: false,
            stories_pinned_available: false,
            blocked_my_stories_from: false,
            wallpaper_overridden: false,
            contact_require_premium: false,
            read_dates_private: false,
            sponsored_enabled: false,
            can_view_revenue: false,
            bot_can_manage_emoji_status: false,
            display_gifts_button: false,
            noforwards_my_enabled: false,
            noforwards_peer_enabled: false,
            unofficial_security_risk: false,
            id,
            about: None,
            settings: make_empty_peer_settings(),
            personal_photo,
            profile_photo,
            fallback_photo,
            notify_settings: make_empty_notify_settings(),
            bot_info: None,
            pinned_msg_id: None,
            common_chats_count: 0,
            folder_id: None,
            ttl_period: None,
            theme: None,
            private_forward_name: None,
            bot_group_admin_rights: None,
            bot_broadcast_admin_rights: None,
            wallpaper: None,
            stories: None,
            business_work_hours: None,
            business_location: None,
            business_greeting_message: None,
            business_away_message: None,
            business_intro: None,
            birthday: None,
            personal_channel_id: None,
            personal_channel_message: None,
            stargifts_count: None,
            starref_program: None,
            bot_verification: None,
            send_paid_messages_stars: None,
            disallowed_gifts: None,
            stars_rating: None,
            stars_my_pending_rating: None,
            stars_my_pending_rating_date: None,
            main_tab: None,
            saved_music: None,
            note: None,
            bot_manager_id: None,
            share_theme: None,
            wallet_card: None,
        }),
        chats: Vec::new(),
        users: Vec::new(),
    })
}

fn make_tl_user(id: i64, first_name: &str, access_hash: i64, bot: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: id == SELF_UID as i64,
        contact: !bot && id != SELF_UID as i64,
        mutual_contact: false,
        deleted: false,
        bot,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: bot,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: true,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(access_hash),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: None,
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    })
}

// =============================================================================
// 1. InfoBackend::active_profile_photo_ref & placeholder InputPhoto guards
// =============================================================================

#[test]
fn wp71_active_profile_photo_ref_resolves_user_chat_channel_and_rejects_bogus_refs() {
    let (rt, rpc, mut live) = make_live_session();
    let self_peer = from_data_peer(DataPeerId::from_user(DataUserId(SELF_UID)));
    let user_peer = from_data_peer(DataPeerId::from_user(DataUserId(PEER_UID)));
    let no_photo_peer = from_data_peer(DataPeerId::from_user(DataUserId(OTHER_UID)));
    let chat_peer = from_data_peer(DataPeerId::from_chat(ChatId(CHAT_ID)));
    let channel_peer = from_data_peer(DataPeerId::from_channel(ChannelId(CHANNEL_ID)));

    // 1a. UserFull caches profile_photo & personal_photo (personal_photo takes priority).
    let expected_user_photo = ProfilePhotoRef {
        id: 8_800_111,
        access_hash: 99_111_222,
        file_reference: vec![0x11, 0x22, 0x33],
    };
    rpc.enqueue_ok(
        tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
        &make_tl_user_full(
            PEER_UID as i64,
            Some(make_tl_photo(8_800_100, 99_100_000, vec![0x01])),
            Some(make_tl_photo(
                expected_user_photo.id,
                expected_user_photo.access_hash,
                expected_user_photo.file_reference.clone(),
            )),
            None,
        ),
    );
    let resolved_user =
        InfoBackend::active_profile_photo_ref(&live, user_peer).expect("active user profile photo");
    assert_eq!(resolved_user, expected_user_photo);
    // Second call hits Session cache without another GetFullUser RPC.
    assert_eq!(
        InfoBackend::active_profile_photo_ref(&live, user_peer),
        Some(expected_user_photo.clone())
    );
    assert_eq!(
        rpc.recorded_of(tl_fns::users::GetFullUser::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // 1b. User with no photo returns None (never peer_id.bare()).
    rpc.enqueue_ok(
        tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
        &make_tl_user_full(OTHER_UID as i64, None, None, None),
    );
    assert_eq!(
        InfoBackend::active_profile_photo_ref(&live, no_photo_peer),
        None
    );

    // 1c. Basic chat hydrates via messages.getFullChat.
    let expected_chat_photo = ProfilePhotoRef {
        id: 7_700_222,
        access_hash: 88_222_333,
        file_reference: vec![0xAA, 0xBB],
    };
    rpc.enqueue_ok(
        tl_fns::messages::GetFullChat::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatFull::ChatFull(tl_types::messages::ChatFull {
            full_chat: tl_enums::ChatFull::ChatFull(tl_types::ChatFull {
                can_set_username: false,
                has_scheduled: false,
                translations_disabled: false,
                has_welcome_messages: false,
                id: CHAT_ID as i64,
                about: String::new(),
                participants: tl_enums::ChatParticipants::Forbidden(
                    tl_types::ChatParticipantsForbidden {
                        chat_id: CHAT_ID as i64,
                        self_participant: None,
                    },
                ),
                chat_photo: Some(make_tl_photo(
                    expected_chat_photo.id,
                    expected_chat_photo.access_hash,
                    expected_chat_photo.file_reference.clone(),
                )),
                notify_settings: make_empty_notify_settings(),
                exported_invite: None,
                bot_info: None,
                pinned_msg_id: None,
                folder_id: None,
                call: None,
                ttl_period: None,
                groupcall_default_join_as: None,
                theme_emoticon: None,
                requests_pending: None,
                recent_requesters: None,
                available_reactions: None,
                reactions_limit: None,
            }),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    assert_eq!(
        InfoBackend::active_profile_photo_ref(&live, chat_peer),
        Some(expected_chat_photo)
    );

    // 1d. Channel hydrates via channels.getFullChannel.
    let expected_ch_photo = ProfilePhotoRef {
        id: 6_600_333,
        access_hash: 77_333_444,
        file_reference: vec![0xDE, 0xAD],
    };
    rpc.enqueue_ok(
        tl_fns::channels::GetFullChannel::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatFull::ChatFull(tl_types::messages::ChatFull {
            full_chat: tl_enums::ChatFull::ChannelFull(tl_types::ChannelFull {
                can_view_participants: true,
                can_set_username: false,
                can_set_stickers: false,
                hidden_prehistory: false,
                can_set_location: false,
                has_scheduled: false,
                can_view_stats: false,
                blocked: false,
                can_delete_channel: false,
                antispam: false,
                participants_hidden: false,
                translations_disabled: false,
                stories_pinned_available: false,
                view_forum_as_messages: false,
                restricted_sponsored: false,
                can_view_revenue: false,
                paid_media_allowed: false,
                can_view_stars_revenue: false,
                paid_reactions_available: false,
                stargifts_available: false,
                paid_messages_available: false,
                has_welcome_messages: false,
                id: CHANNEL_ID as i64,
                about: String::new(),
                participants_count: Some(12),
                admins_count: None,
                kicked_count: None,
                banned_count: None,
                online_count: None,
                read_inbox_max_id: 0,
                read_outbox_max_id: 0,
                unread_count: 0,
                chat_photo: make_tl_photo(
                    expected_ch_photo.id,
                    expected_ch_photo.access_hash,
                    expected_ch_photo.file_reference.clone(),
                ),
                notify_settings: make_empty_notify_settings(),
                exported_invite: None,
                bot_info: Vec::new(),
                migrated_from_chat_id: None,
                migrated_from_max_id: None,
                pinned_msg_id: None,
                stickerset: None,
                available_min_id: None,
                folder_id: None,
                linked_chat_id: None,
                location: None,
                slowmode_seconds: None,
                slowmode_next_send_date: None,
                stats_dc: None,
                pts: 1,
                call: None,
                ttl_period: None,
                pending_suggestions: None,
                groupcall_default_join_as: None,
                theme_emoticon: None,
                requests_pending: None,
                recent_requesters: None,
                default_send_as: None,
                available_reactions: None,
                reactions_limit: None,
                stories: None,
                wallpaper: None,
                boosts_applied: None,
                boosts_unrestrict: None,
                emojiset: None,
                bot_verification: None,
                stargifts_count: None,
                send_paid_messages_stars: None,
                main_tab: None,
                guard_bot_id: None,
                share_theme: None,
            }),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    assert_eq!(
        InfoBackend::active_profile_photo_ref(&live, channel_peer),
        Some(expected_ch_photo)
    );

    // 1e. Fallback to `photos.getUserPhotos` when user has a `userpic_photo_id` without cached PhotoData.
    let expected_self_photo = ProfilePhotoRef {
        id: 5_500_444,
        access_hash: 66_444_555,
        file_reference: vec![0x42, 0x43],
    };
    rt.block_on(async {
        let mut s = live.runtime().session().write().await;
        if let Some(u) = s.user_mut(DataUserId(SELF_UID)) {
            u.common.loaded_status = tg_data::LoadedStatus::Full;
            u.common.userpic_photo_id = PhotoId::from_tl(expected_self_photo.id);
        }
    });
    rpc.enqueue_ok(
        tl_fns::photos::GetUserPhotos::CONSTRUCTOR_ID,
        &tl_enums::photos::Photos::Photos(tl_types::photos::Photos {
            photos: vec![make_tl_photo(
                expected_self_photo.id,
                expected_self_photo.access_hash,
                expected_self_photo.file_reference.clone(),
            )],
            users: Vec::new(),
        }),
    );
    assert_eq!(
        InfoBackend::active_profile_photo_ref(&live, self_peer),
        Some(expected_self_photo.clone())
    );
    let user_photo_reqs = rpc.recorded_of(tl_fns::photos::GetUserPhotos::CONSTRUCTOR_ID);
    assert_eq!(user_photo_reqs.len(), 1);
    assert_eq!(
        user_photo_reqs[0],
        (tl_fns::photos::GetUserPhotos {
            user_id: tl_enums::InputUser::Self_,
            offset: 0,
            max_id: 0,
            limit: 1,
        })
        .to_bytes()
    );

    // 1f. Bogus placeholder refs (`id == peer_id.bare(), access_hash == 0`) are rejected
    // before any RPC is transmitted for delete, report, and attached stickers.
    let bogus = ProfilePhotoRef {
        id: user_peer.bare() as i64,
        access_hash: 0,
        file_reference: Vec::new(),
    };
    assert!(InfoBackend::delete_profile_photos(&mut live, std::slice::from_ref(&bogus)).is_err());
    assert!(
        InfoBackend::report_profile_photo(&mut live, user_peer, bogus, ReportReason::Spam, "")
            .is_err()
    );
    assert!(
        ChatHelpersBackend::get_attached_stickers(
            &live,
            &AttachedStickersMedia::Photo {
                id: user_peer.bare() as i64,
                access_hash: 0,
                file_reference: Vec::new(),
            },
        )
        .is_err()
    );
    assert!(
        rpc.recorded_of(tl_fns::photos::DeletePhotos::CONSTRUCTOR_ID)
            .is_empty()
    );
    assert!(
        rpc.recorded_of(tl_fns::account::ReportProfilePhoto::CONSTRUCTOR_ID)
            .is_empty()
    );
    assert!(
        rpc.recorded_of(tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID)
            .is_empty()
    );

    // 1g. Deleting the real self photo clears it from Session's photo cache.
    rpc.enqueue_ok(
        tl_fns::photos::DeletePhotos::CONSTRUCTOR_ID,
        &vec![expected_self_photo.id],
    );
    let deleted = InfoBackend::delete_profile_photos(&mut live, &[expected_self_photo])
        .expect("delete real self photo");
    assert_eq!(deleted, vec![5_500_444]);
    assert_eq!(
        InfoBackend::active_profile_photo_ref(&live, self_peer),
        None
    );
}

// =============================================================================
// 2. deleteParticipantReactions optimistic local strip
// =============================================================================

#[test]
fn wp71_delete_participant_reactions_strips_only_target_participant_in_target_peer() {
    let (rt, rpc, mut live) = make_live_session();
    let ch_data = DataPeerId::from_channel(ChannelId(CHANNEL_ID));
    let other_chat_data = DataPeerId::from_chat(ChatId(CHAT_ID));
    let target_data = DataPeerId::from_user(DataUserId(PEER_UID));
    let bystander_data = DataPeerId::from_user(DataUserId(OTHER_UID));

    let msg_only_target = DataFullMsgId::new(ch_data, DataMsgId(101));
    let msg_mixed = DataFullMsgId::new(ch_data, DataMsgId(102));
    let msg_other_chat = DataFullMsgId::new(other_chat_data, DataMsgId(201));

    rt.block_on(async {
        let mut s = live.runtime().session().write().await;

        let mut m1 = Message::new_text(
            ch_data,
            DataMsgId(101),
            Some(target_data),
            1_750_000_010,
            DataText::plain("only target reacted"),
        );
        m1.flags |= MessageFlags::CAN_VIEW_REACTIONS | MessageFlags::HAS_UNREAD_REACTION;
        s.insert_message(m1);
        s.update_message_reactions(
            msg_only_target,
            MessageReactionsSummary {
                results: vec![MessageReactionCount {
                    id: ReactionId::Emoji("👍".to_owned()),
                    count: 1,
                    chosen_order: None,
                }],
                recent_reactors: vec![RecentReactor {
                    peer_id: target_data,
                    reaction: ReactionId::Emoji("👍".to_owned()),
                    date: 1_750_000_011,
                    unread: true,
                    my: false,
                }],
                can_see_list: true,
                reactions_as_tags: false,
                min: false,
            },
        );

        let m2 = Message::new_text(
            ch_data,
            DataMsgId(102),
            Some(bystander_data),
            1_750_000_020,
            DataText::plain("target and bystander reacted"),
        );
        s.insert_message(m2);
        s.update_message_reactions(
            msg_mixed,
            MessageReactionsSummary {
                results: vec![
                    MessageReactionCount {
                        id: ReactionId::Emoji("👍".to_owned()),
                        count: 2,
                        chosen_order: None,
                    },
                    MessageReactionCount {
                        id: ReactionId::Emoji("🔥".to_owned()),
                        count: 1,
                        chosen_order: None,
                    },
                ],
                recent_reactors: vec![
                    RecentReactor {
                        peer_id: target_data,
                        reaction: ReactionId::Emoji("👍".to_owned()),
                        date: 1_750_000_021,
                        unread: false,
                        my: false,
                    },
                    RecentReactor {
                        peer_id: bystander_data,
                        reaction: ReactionId::Emoji("👍".to_owned()),
                        date: 1_750_000_022,
                        unread: false,
                        my: false,
                    },
                    RecentReactor {
                        peer_id: bystander_data,
                        reaction: ReactionId::Emoji("🔥".to_owned()),
                        date: 1_750_000_023,
                        unread: false,
                        my: false,
                    },
                ],
                can_see_list: true,
                reactions_as_tags: false,
                min: false,
            },
        );

        let m3 = Message::new_text(
            other_chat_data,
            DataMsgId(201),
            Some(target_data),
            1_750_000_030,
            DataText::plain("other chat message"),
        );
        s.insert_message(m3);
        s.update_message_reactions(
            msg_other_chat,
            MessageReactionsSummary {
                results: vec![MessageReactionCount {
                    id: ReactionId::Emoji("❤️".to_owned()),
                    count: 1,
                    chosen_order: None,
                }],
                recent_reactors: vec![RecentReactor {
                    peer_id: target_data,
                    reaction: ReactionId::Emoji("❤️".to_owned()),
                    date: 1_750_000_031,
                    unread: false,
                    my: false,
                }],
                can_see_list: true,
                reactions_as_tags: false,
                min: false,
            },
        );
    });

    // When RPC returns false, local cached reactions remain untouched.
    rpc.enqueue_bool(
        tl_fns::messages::DeleteParticipantReactions::CONSTRUCTOR_ID,
        false,
    );
    assert!(
        !ChatHelpersBackend::delete_participant_reactions(
            &mut live,
            from_data_peer(ch_data),
            from_data_peer(target_data),
        )
        .expect("rpc ok(false)")
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        assert!(s.message(msg_only_target).expect("m1").reactions.is_some());
    });

    // When RPC returns true, target participant's reactions are stripped across ch_data immediately.
    rpc.enqueue_bool(
        tl_fns::messages::DeleteParticipantReactions::CONSTRUCTOR_ID,
        true,
    );
    assert!(
        ChatHelpersBackend::delete_participant_reactions(
            &mut live,
            from_data_peer(ch_data),
            from_data_peer(target_data),
        )
        .expect("rpc ok(true)")
    );

    rt.block_on(async {
        let s = live.runtime().session().read().await;

        let m1 = s.message(msg_only_target).expect("m1");
        assert!(m1.reactions.is_none());
        assert!(!m1.flags.contains(MessageFlags::CAN_VIEW_REACTIONS));
        assert!(!m1.has_unread_reaction());
        assert_eq!(
            s.history(ch_data).expect("history").unread_reactions_count,
            0
        );

        let m2 = s.message(msg_mixed).expect("m2");
        let r2 = m2.reactions.as_ref().expect("bystander reactions remain");
        assert_eq!(r2.results.len(), 2);
        assert_eq!(r2.results[0].id, ReactionId::Emoji("👍".to_owned()));
        assert_eq!(r2.results[0].count, 1);
        assert_eq!(r2.results[1].id, ReactionId::Emoji("🔥".to_owned()));
        assert_eq!(r2.results[1].count, 1);
        assert!(
            r2.recent_reactors
                .iter()
                .all(|rr| rr.peer_id != target_data)
        );
        assert_eq!(r2.recent_reactors.len(), 2);

        // Reactions in other peers must remain completely untouched.
        let m3 = s.message(msg_other_chat).expect("m3");
        let r3 = m3.reactions.as_ref().expect("other chat intact");
        assert_eq!(r3.results[0].count, 1);
        assert_eq!(r3.recent_reactors.len(), 1);
    });
}

// =============================================================================
// 3. Background & Live Trigger Hooks
// =============================================================================

#[test]
fn wp71_profile_saved_music_triggers_get_saved_music_ids_with_30s_throttle() {
    let (_rt, rpc, live) = make_live_session();
    let other_peer = from_data_peer(DataPeerId::from_user(DataUserId(PEER_UID)));

    let make_audio_doc = |id: i64, title: &str| {
        tl_enums::Document::Document(tl_types::Document {
            id,
            access_hash: id * 13,
            file_reference: vec![1, 2],
            date: 1_750_000_000,
            mime_type: "audio/mpeg".to_owned(),
            size: 100_000,
            thumbs: None,
            video_thumbs: None,
            dc_id: 2,
            attributes: vec![tl_enums::DocumentAttribute::Audio(
                tl_types::DocumentAttributeAudio {
                    voice: false,
                    duration: 120,
                    title: Some(title.to_owned()),
                    performer: Some("Synth Ensemble".to_owned()),
                    waveform: None,
                },
            )],
        })
    };

    rpc.enqueue_ok(
        tl_fns::users::GetSavedMusic::CONSTRUCTOR_ID,
        &tl_enums::users::SavedMusic::SavedMusic(tl_types::users::SavedMusic {
            count: 2,
            documents: vec![
                make_audio_doc(9001, "Saved Track"),
                make_audio_doc(9002, "Unsaved Track"),
            ],
        }),
    );
    rpc.enqueue_ok(
        tl_fns::account::GetSavedMusicIds::CONSTRUCTOR_ID,
        &tl_enums::account::SavedMusicIds::SavedMusicIds(tl_types::account::SavedMusicIds {
            ids: vec![9001],
        }),
    );

    let tracks = InfoBackend::peer_saved_music(&live, other_peer, 0, 20).expect("saved music");
    assert_eq!(tracks.len(), 2);
    assert!(tracks[0].saved_on_profile);
    assert!(!tracks[1].saved_on_profile);
    assert_eq!(
        rpc.recorded_of(tl_fns::account::GetSavedMusicIds::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Immediate second call must NOT trigger another `account.getSavedMusicIds` (30s throttle).
    rpc.enqueue_ok(
        tl_fns::users::GetSavedMusic::CONSTRUCTOR_ID,
        &tl_enums::users::SavedMusic::SavedMusic(tl_types::users::SavedMusic {
            count: 1,
            documents: vec![make_audio_doc(9001, "Saved Track")],
        }),
    );
    let _ = InfoBackend::peer_saved_music(&live, other_peer, 0, 20).expect("second load");
    assert_eq!(
        rpc.recorded_of(tl_fns::account::GetSavedMusicIds::CONSTRUCTOR_ID)
            .len(),
        1
    );
}

#[test]
fn wp71_bots_single_attach_menu_bot_lookup_triggers_get_attach_menu_bot() {
    let (_rt, rpc, mut live) = make_live_session();

    let make_bot_reply = |bot_id: i64, name: &str| {
        tl_enums::AttachMenuBotsBot::AttachMenuBotsBot(tl_types::AttachMenuBotsBot {
            bot: tl_enums::AttachMenuBot::AttachMenuBot(tl_types::AttachMenuBot {
                inactive: false,
                has_settings: true,
                request_write_access: false,
                show_in_attach_menu: true,
                show_in_side_menu: false,
                side_menu_disclaimer_needed: false,
                bot_id,
                short_name: name.to_owned(),
                peer_types: Some(vec![tl_enums::AttachMenuPeerType::PM]),
                icons: Vec::new(),
            }),
            users: vec![make_tl_user(bot_id, name, BOT_HASH as i64, true)],
        })
    };

    // 1. Direct single-bot lookup via `BotsBackend::load_attach_menu_bot`.
    rpc.enqueue_ok(
        tl_fns::messages::GetAttachMenuBot::CONSTRUCTOR_ID,
        &make_bot_reply(BOT_UID as i64, "LookupMiniApp"),
    );
    let entry =
        BotsBackend::load_attach_menu_bot(&live, BotUserId(BOT_UID)).expect("load_attach_menu_bot");
    assert_eq!(entry.bot_id, BotUserId(BOT_UID));
    assert_eq!(entry.short_name, "LookupMiniApp");
    assert!(entry.in_attach_menu);
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetAttachMenuBot::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // 2. Enabling an uncached bot via `toggle_attach_menu_bot` hydrates via `messages.getAttachMenuBot`.
    let bot2_uid = 778u64;
    live.write_session(|s| {
        let mut bot2 = User::new(DataUserId(bot2_uid), false);
        bot2.set_name("Second", "MiniBot", "", "second_mini_bot");
        bot2.access_hash = 77_800_111;
        bot2.bot_info = Some(tg_data::BotInfo::default());
        s.upsert_user(bot2, PeerUpdateFlags::NAME);
    });
    rpc.enqueue_bool(
        tl_fns::messages::ToggleBotInAttachMenu::CONSTRUCTOR_ID,
        true,
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetAttachMenuBot::CONSTRUCTOR_ID,
        &make_bot_reply(bot2_uid as i64, "AddedMiniApp"),
    );
    let store = BotsBackend::toggle_attach_menu_bot(
        &mut live,
        BotUserId(bot2_uid),
        AttachMenuToggleState::Added,
    )
    .expect("toggle_attach_menu_bot");
    let hydrated = store
        .find(BotUserId(bot2_uid))
        .expect("newly added bot hydrated in store");
    assert_eq!(hydrated.short_name, "AddedMiniApp");
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetAttachMenuBot::CONSTRUCTOR_ID)
            .len(),
        2
    );
}

#[test]
fn wp71_payments_premium_promo_event_triggers_help_save_app_log() {
    let (_rt, rpc, live) = make_live_session();

    rpc.enqueue_bool(tl_fns::help::SaveAppLog::CONSTRUCTOR_ID, true);
    let ok_show = PaymentsBackend::log_premium_promo_event(
        &live,
        &PremiumPromoLogEvent::ScreenShow {
            order: vec!["stories".to_owned(), "double_limits".to_owned()],
            source: String::new(),
        },
    )
    .expect("log screen show");
    assert!(ok_show);

    rpc.enqueue_bool(tl_fns::help::SaveAppLog::CONSTRUCTOR_ID, true);
    let ok_accept =
        PaymentsBackend::log_premium_promo_event(&live, &PremiumPromoLogEvent::ScreenAccept)
            .expect("log screen accept");
    assert!(ok_accept);

    let recorded = rpc.recorded_of(tl_fns::help::SaveAppLog::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 2);

    let req0 = tl_fns::help::SaveAppLog::from_bytes(&recorded[0]).expect("decode show");
    assert_eq!(req0.events.len(), 1);
    let tl_enums::InputAppEvent::InputAppEvent(ref ev0) = req0.events[0];
    assert_eq!(ev0.r#type, "premium.promo_screen_show");
    assert_eq!(ev0.peer, 0);

    let req1 = tl_fns::help::SaveAppLog::from_bytes(&recorded[1]).expect("decode accept");
    assert_eq!(req1.events.len(), 1);
    let tl_enums::InputAppEvent::InputAppEvent(ref ev1) = req1.events[0];
    assert_eq!(ev1.r#type, "premium.promo_screen_accept");
    assert_eq!(ev1.peer, 0);
    assert_eq!(ev1.data, tl_enums::JSONValue::JsonNull);
}

#[test]
fn wp71_contact_statuses_periodic_reconnect_and_privacy_hooks_trigger_get_statuses() {
    let (rt, rpc, mut live) = make_live_session();

    let status_reply = |expires: i32| {
        vec![tl_enums::ContactStatus::ContactStatus(
            tl_types::ContactStatus {
                user_id: PEER_UID as i64,
                status: tl_enums::UserStatus::Online(tl_types::UserStatusOnline { expires }),
            },
        )]
    };

    // 1. Periodic poll fires when due, skips within interval, fires again after interval.
    rpc.enqueue_ok(
        tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID,
        &status_reply(1_750_000_500),
    );
    let first = live
        .poll_contact_statuses_if_due(BASE_NOW_MS as u64)
        .expect("first periodic poll");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].online_till, 1_750_000_500);
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID)
            .len(),
        1
    );

    let throttled = live
        .poll_contact_statuses_if_due(BASE_NOW_MS as u64 + 60_000)
        .expect("throttled poll");
    assert!(throttled.is_empty());
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID)
            .len(),
        1
    );

    rpc.enqueue_ok(
        tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID,
        &status_reply(1_750_000_900),
    );
    let after_window = live
        .poll_contact_statuses_if_due(BASE_NOW_MS as u64 + CONTACT_STATUSES_REFRESH_INTERVAL_MS)
        .expect("post-interval poll");
    assert_eq!(after_window[0].online_till, 1_750_000_900);
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID)
            .len(),
        2
    );

    // 2. Updating LastSeen privacy triggers `contacts.getStatuses` (`api_user_privacy.cpp:214`).
    rpc.enqueue_ok(
        tl_fns::account::SetPrivacy::CONSTRUCTOR_ID,
        &tl_enums::account::PrivacyRules::PrivacyRules(tl_types::account::PrivacyRules {
            rules: vec![tl_enums::PrivacyRule::PrivacyValueAllowAll],
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );
    rpc.enqueue_ok(
        tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID,
        &status_reply(1_750_001_200),
    );
    SettingsBackend::set_privacy_rule(&mut live, PrivacyKey::LastSeen, PrivacyRule::default());
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID)
            .len(),
        3
    );

    // 3. `Backend::retry_connection` triggers `contacts.getStatuses`.
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: 1_750_000_000,
            seq: 0,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID,
        &status_reply(1_750_001_500),
    );
    Backend::retry_connection(&mut live);
    assert_eq!(
        rpc.recorded_of(tl_fns::contacts::GetStatuses::CONSTRUCTOR_ID)
            .len(),
        4
    );

    rt.block_on(async {
        let s = live.runtime().session().read().await;
        assert_eq!(
            s.user(DataUserId(PEER_UID))
                .expect("user")
                .lastseen
                .online_till(),
            1_750_001_500
        );
    });
}

#[test]
fn wp71_emoji_keyword_lookup_triggers_get_emoji_keywords_languages_with_ttl() {
    let (_rt, rpc, mut live) = make_live_session();

    rpc.enqueue_ok(
        tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID,
        &vec![tl_enums::EmojiLanguage::EmojiLanguage(
            tl_types::EmojiLanguage {
                lang_code: "en".to_owned(),
            },
        )],
    );
    let _ = ChatHelpersBackend::search_emoji(&mut live, "smile");
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Within 1-hour TTL -> `search_emoji_suggestions` uses cache, zero extra RPCs.
    let _ = ChatHelpersBackend::search_emoji_suggestions(&mut live, "heart", false);
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // Past 1-hour TTL -> refetches `messages.getEmojiKeywordsLanguages`.
    live.set_chat_helpers_now_ms(Some(BASE_NOW_MS + EMOJI_KEYWORDS_REFRESH_MS as i64 + 1));
    rpc.enqueue_ok(
        tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID,
        &vec![tl_enums::EmojiLanguage::EmojiLanguage(
            tl_types::EmojiLanguage {
                lang_code: "en".to_owned(),
            },
        )],
    );
    let _ = ChatHelpersBackend::search_emoji_suggestions(&mut live, "star", false);
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetEmojiKeywordsLanguages::CONSTRUCTOR_ID)
            .len(),
        2
    );
}

#[test]
fn wp71_visible_messages_reactions_poll_triggers_get_messages_reactions_with_20s_throttle() {
    let (rt, rpc, live) = make_live_session();
    let ch_data = DataPeerId::from_channel(ChannelId(CHANNEL_ID));
    let ch_peer = from_data_peer(ch_data);
    let user_data = DataPeerId::from_user(DataUserId(PEER_UID));
    let user_peer = from_data_peer(user_data);

    let full_10 = DataFullMsgId::new(ch_data, DataMsgId(10));
    let full_11 = DataFullMsgId::new(ch_data, DataMsgId(11));
    let full_user_20 = DataFullMsgId::new(user_data, DataMsgId(20));

    rt.block_on(async {
        let mut s = live.runtime().session().write().await;

        let m10 = Message::new_text(
            ch_data,
            DataMsgId(10),
            Some(user_data),
            1_750_000_010,
            DataText::plain("reacted post"),
        );
        s.insert_message(m10);
        s.update_message_reactions(
            full_10,
            MessageReactionsSummary {
                results: vec![MessageReactionCount {
                    id: ReactionId::Emoji("👍".to_owned()),
                    count: 1,
                    chosen_order: None,
                }],
                recent_reactors: Vec::new(),
                can_see_list: false,
                reactions_as_tags: false,
                min: false,
            },
        );

        let m11 = Message::new_text(
            ch_data,
            DataMsgId(11),
            Some(user_data),
            1_750_000_011,
            DataText::plain("unreacted post"),
        );
        s.insert_message(m11);

        let mu = Message::new_text(
            user_data,
            DataMsgId(20),
            Some(user_data),
            1_750_000_020,
            DataText::plain("dm message with reaction"),
        );
        s.insert_message(mu);
        s.update_message_reactions(
            full_user_20,
            MessageReactionsSummary {
                results: vec![MessageReactionCount {
                    id: ReactionId::Emoji("🔥".to_owned()),
                    count: 1,
                    chosen_order: None,
                }],
                recent_reactors: Vec::new(),
                can_see_list: false,
                reactions_as_tags: false,
                min: false,
            },
        );
    });

    // Enqueue empty `messages.getHistory` reply for initial load_slice_around history check.
    rpc.enqueue_ok(
        tl_fns::messages::GetHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: Vec::new(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let make_reactions_update = |count: i32| {
        tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::MessageReactions(
                tl_types::UpdateMessageReactions {
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: CHANNEL_ID as i64,
                    }),
                    msg_id: 10,
                    top_msg_id: None,
                    saved_peer_id: None,
                    reactions: tl_enums::MessageReactions::MessageReactions(
                        tl_types::MessageReactions {
                            min: false,
                            can_see_list: false,
                            reactions_as_tags: false,
                            results: vec![tl_enums::ReactionCount::ReactionCount(
                                tl_types::ReactionCount {
                                    chosen_order: None,
                                    reaction: tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                                        emoticon: "👍".to_owned(),
                                    }),
                                    count,
                                },
                            )],
                            recent_reactions: None,
                            top_reactors: None,
                        },
                    ),
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_750_000_050,
            seq: 0,
        })
    };

    // 1. `load_slice_around` polls ONLY reacted msg 10 (not unreacted msg 11) and updates slice count.
    rpc.enqueue_ok(
        tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID,
        &make_reactions_update(5),
    );
    let slice = HistoryBackend::load_slice_around(&live, ch_peer, MsgId(10), 5, 5);
    let msg10_ui = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(10))
        .expect("msg 10 in slice");
    assert_eq!(msg10_ui.reactions.len(), 1);
    assert_eq!(msg10_ui.reactions[0].count, 5);
    let recorded = rpc.recorded_of(tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID);
    assert_eq!(recorded.len(), 1);
    let decoded =
        tl_fns::messages::GetMessagesReactions::from_bytes(&recorded[0]).expect("decode poll");
    assert_eq!(decoded.id, vec![10]);

    // 2. Within 20-second `kPollEach` window, `poll_messages_reactions` does not re-poll.
    let skipped = HistoryBackend::poll_messages_reactions(&live, ch_peer, &[MsgId(10), MsgId(11)])
        .expect("within window");
    assert!(skipped.is_empty());
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // 3. After 20 seconds, `poll_messages_reactions` polls msg 10 again.
    live.set_chat_helpers_now_ms(Some(BASE_NOW_MS + REACTIONS_POLL_INTERVAL_MS as i64));
    rpc.enqueue_ok(
        tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID,
        &make_reactions_update(9),
    );
    let repolled = HistoryBackend::poll_messages_reactions(&live, ch_peer, &[MsgId(10), MsgId(11)])
        .expect("after 20s");
    assert_eq!(repolled, vec![MsgId(10)]);
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID)
            .len(),
        2
    );
    rt.block_on(async {
        let s = live.runtime().session().read().await;
        assert_eq!(
            s.message(full_10)
                .expect("m10")
                .reactions
                .as_ref()
                .expect("r10")
                .results[0]
                .count,
            9
        );
        assert!(s.message(full_11).expect("m11").reactions.is_none());
    });

    // 4. 1:1 user peers are skipped (`data_message_reactions.cpp:1516`).
    let dm_polled = HistoryBackend::poll_messages_reactions(&live, user_peer, &[MsgId(20)])
        .expect("dm skipped");
    assert!(dm_polled.is_empty());
    assert_eq!(
        rpc.recorded_of(tl_fns::messages::GetMessagesReactions::CONSTRUCTOR_ID)
            .len(),
        2
    );
}
