//! `FakeRpcClient` tests for live `InfoBackend::{personal_channel, personal_channel_candidates,
//! peer_profile_balance}` wiring on `LiveSession`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    Channel, ChannelFlags, ChannelId, PeerId as DataPeerId, PeerUpdateFlags, User,
    UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::convert::from_data_peer;
use crate::session::LiveSession;
use tg_app_core::info::backend::{InfoBackend, ProfileBalanceInfo};
use tg_app_core::payments::backend::PaymentsBackend;

type RecordedRequests = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;
type QueuedResponses = Arc<Mutex<VecDeque<(u32, Result<Vec<u8>, InvocationError>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedRequests,
    responses: QueuedResponses,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(32);
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

    fn recorded_of(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, b)| b.clone())
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
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
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
            Some(Ok(raw)) => {
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
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

fn make_live_session() -> (tokio::runtime::Runtime, FakeRpcClient, LiveSession) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rpc = FakeRpcClient::new();
    let runtime = SessionRuntime::start_on(
        DataUserId(100),
        crate::ErasedRpcClient::new(rpc.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            load_initial_dialogs: false,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(runtime), 1);
    (rt, rpc, live)
}

fn make_tl_channel(id: i64, title: &str, username: Option<&str>, subs: i32) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: true,
        left: false,
        broadcast: true,
        verified: false,
        megagroup: false,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 11),
        title: title.to_owned(),
        username: username.map(str::to_owned),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(subs),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    })
}

fn make_user_full_with_personal_channel(
    user_id: i64,
    channel_id: i64,
    channel_title: &str,
    channel_username: &str,
    subs: i32,
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
            id: user_id,
            about: None,
            settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
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
            }),
            personal_photo: None,
            profile_photo: None,
            fallback_photo: None,
            notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                tl_types::PeerNotifySettings {
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
                },
            ),
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
            personal_channel_id: Some(channel_id),
            personal_channel_message: Some(42),
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
            wallet_card: None,
            share_theme: None,
        }),
        chats: vec![make_tl_channel(
            channel_id,
            channel_title,
            Some(channel_username),
            subs,
        )],
        users: Vec::new(),
    })
}

fn make_stars_status(stars: i64, ton_nanos: Option<i64>) -> tl_enums::payments::StarsStatus {
    let balance = match ton_nanos {
        Some(nanos) => {
            tl_enums::StarsAmount::StarsTonAmount(tl_types::StarsTonAmount { amount: nanos })
        }
        None => tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
            amount: stars,
            nanos: 0,
        }),
    };
    tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
        balance,
        subscriptions: None,
        subscriptions_next_offset: None,
        subscriptions_missing_balance: None,
        history: None,
        next_offset: None,
        chats: Vec::new(),
        users: Vec::new(),
    })
}

#[test]
fn live_personal_channel_hydrates_from_get_full_user_and_candidates_from_admined_channels() {
    let (_rt, rpc, live) = make_live_session();
    let self_uid = DataUserId(100);
    let self_pid = from_data_peer(DataPeerId::from_user(self_uid));
    live.write_session(|s| {
        let mut me = User::new(self_uid, true);
        me.first_name = "Test".to_owned();
        s.upsert_user(me, PeerUpdateFlags::empty());
    });

    // 1. `personal_channel` triggers `users.getFullUser(InputUser::Self_)` and reads `personal_channel_id`.
    let uf = make_user_full_with_personal_channel(100, 9001, "My Notes", "my_notes_ch", 321);
    rpc.enqueue_ok(tl_fns::users::GetFullUser::CONSTRUCTOR_ID, &uf);

    let pc = live
        .personal_channel(self_pid)
        .expect("personal_channel hydrated via getFullUser");
    assert_eq!(
        pc.peer_id,
        from_data_peer(DataPeerId::from_channel(ChannelId(9001)))
    );
    assert_eq!(pc.title, "My Notes");
    assert_eq!(pc.username.as_deref(), Some("my_notes_ch"));
    assert_eq!(pc.subscribers_count, 321);

    // Second call hits session cache with no extra RPC.
    let pc2 = live
        .personal_channel(self_pid)
        .expect("cached personal_channel");
    assert_eq!(pc2, pc);
    assert_eq!(
        rpc.recorded_of(tl_fns::users::GetFullUser::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // 2. `personal_channel_candidates` sends `channels.getAdminedPublicChannels { for_personal: true }`.
    let admined_resp = tl_enums::messages::Chats::Chats(tl_types::messages::Chats {
        chats: vec![
            make_tl_channel(9001, "My Notes", Some("my_notes_ch"), 321),
            make_tl_channel(9002, "Second Broadcast", Some("second_ch"), 88),
        ],
    });
    rpc.enqueue_ok(
        tl_fns::channels::GetAdminedPublicChannels::CONSTRUCTOR_ID,
        &admined_resp,
    );

    let candidates = live.personal_channel_candidates();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].title, "My Notes");
    assert_eq!(candidates[1].title, "Second Broadcast");
    assert_eq!(candidates[1].username.as_deref(), Some("second_ch"));
    assert_eq!(candidates[1].subscribers_count, 88);

    let raw_admined = rpc.recorded_of(tl_fns::channels::GetAdminedPublicChannels::CONSTRUCTOR_ID);
    assert_eq!(raw_admined.len(), 1);
    let req = tl_fns::channels::GetAdminedPublicChannels::from_bytes(&raw_admined[0])
        .expect("decode GetAdminedPublicChannels");
    assert!(req.for_personal);
    assert!(!req.by_location);
}

#[test]
fn live_peer_profile_balance_reuses_payments_cache_and_hydrates_channel_balance() {
    let (_rt, rpc, live) = make_live_session();
    let self_pid = from_data_peer(DataPeerId::from_user(DataUserId(100)));
    let ch_id = ChannelId(9100);
    let ch_pid = from_data_peer(DataPeerId::from_channel(ch_id));
    live.write_session(|s| {
        let mut ch = Channel::new(ch_id);
        ch.common.name = "Creator Channel".to_owned();
        ch.access_hash = 555_001;
        ch.flags = ChannelFlags::CREATOR | ChannelFlags::BROADCAST;
        s.upsert_channel(ch, PeerUpdateFlags::empty());
    });

    // 1. Load balances via `PaymentsBackend::load_credits_overview` first.
    rpc.enqueue_ok(
        tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
        &make_stars_status(1_450, None),
    );
    rpc.enqueue_ok(
        tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
        &make_stars_status(0, Some(12_345_000_000)),
    );
    let _overview = live.load_credits_overview().expect("load_credits_overview");

    let count_before = rpc
        .recorded_of(tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID)
        .len();
    assert_eq!(count_before, 2);

    // `peer_profile_balance(self)` reuses the payments cache with ZERO additional RPCs.
    let self_bal = live
        .peer_profile_balance(self_pid)
        .expect("self balance from payments cache");
    assert_eq!(
        self_bal,
        ProfileBalanceInfo {
            ton_nanos: 12_345_000_000,
            stars_balance: 1_450,
        }
    );
    assert_eq!(
        rpc.recorded_of(tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID)
            .len(),
        count_before
    );

    // 2. Creator channel hydrates via `payments.getStarsStatus` for that channel peer.
    rpc.enqueue_ok(
        tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
        &make_stars_status(600, None),
    );
    rpc.enqueue_ok(
        tl_fns::payments::GetStarsStatus::CONSTRUCTOR_ID,
        &make_stars_status(0, Some(4_500_000_000)),
    );
    let ch_bal = live
        .peer_profile_balance(ch_pid)
        .expect("creator channel balance");
    assert_eq!(
        ch_bal,
        ProfileBalanceInfo {
            ton_nanos: 4_500_000_000,
            stars_balance: 600,
        }
    );
}
