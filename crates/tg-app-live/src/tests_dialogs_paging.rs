//! Chat-list paging, folder membership and archive placement against a fake server with more
//! chats than one `messages.getDialogs` page (the server caps pages at 100 like Telegram).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    ChannelId, ChatId, FullMsgId, Message, MsgId, PeerId as DataPeerId, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::convert::to_data_peer;
use crate::session::LiveSession;
use tg_app_core::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app_core::stand_in::types::FilterId;

const MAIN_CHATS: usize = 250;
const ARCHIVED_CHATS: usize = 30;
const SERVER_PAGE_CAP: usize = 100;
const BASE_DATE: i32 = 1_760_000_000;
const FOLDER_ID: i32 = 2;
/// A user near the end of the main list (fourth `getDialogs` request).
const LATE_MAIN_USER: usize = 248;
/// An archived user beyond the archive's first page.
const LATE_ARCHIVED_USER: usize = MAIN_CHATS + 28;
/// An archived basic group beyond the archive's first page.
const LATE_ARCHIVED_GROUP: usize = MAIN_CHATS + 25;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    BasicGroup,
    Megagroup,
    Broadcast,
    User,
}

/// Chat `index` (`0..MAIN_CHATS` main list, then archive), newest first in each list.
fn kind(index: usize) -> Kind {
    match index % 5 {
        0 => Kind::BasicGroup,
        1 => Kind::Megagroup,
        2 => Kind::Broadcast,
        _ => Kind::User,
    }
}

fn archived(index: usize) -> bool {
    index >= MAIN_CHATS
}

fn tl_peer(index: usize) -> tl_enums::Peer {
    let id = index as i64;
    match kind(index) {
        Kind::BasicGroup => tl_enums::Peer::Chat(tl_types::PeerChat {
            chat_id: 1_000 + id,
        }),
        Kind::Megagroup | Kind::Broadcast => tl_enums::Peer::Channel(tl_types::PeerChannel {
            channel_id: 5_000 + id,
        }),
        Kind::User => tl_enums::Peer::User(tl_types::PeerUser {
            user_id: 10_000 + id,
        }),
    }
}

fn data_peer(index: usize) -> DataPeerId {
    let id = index as u64;
    match kind(index) {
        Kind::BasicGroup => DataPeerId::from_chat(ChatId(1_000 + id)),
        Kind::Megagroup | Kind::Broadcast => DataPeerId::from_channel(ChannelId(5_000 + id)),
        Kind::User => DataPeerId::from_user(DataUserId(10_000 + id)),
    }
}

fn index_of_input(peer: &tl_enums::InputPeer) -> Option<usize> {
    let (raw, base) = match peer {
        tl_enums::InputPeer::Chat(c) => (c.chat_id, 1_000),
        tl_enums::InputPeer::Channel(c) => (c.channel_id, 5_000),
        tl_enums::InputPeer::User(u) => (u.user_id, 10_000),
        _ => return None,
    };
    let index = usize::try_from(raw - base).ok()?;
    (index < MAIN_CHATS + ARCHIVED_CHATS && tl_peer(index) == input_to_peer(peer)?).then_some(index)
}

fn input_to_peer(peer: &tl_enums::InputPeer) -> Option<tl_enums::Peer> {
    match peer {
        tl_enums::InputPeer::Chat(c) => Some(tl_enums::Peer::Chat(tl_types::PeerChat {
            chat_id: c.chat_id,
        })),
        tl_enums::InputPeer::Channel(c) => Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
            channel_id: c.channel_id,
        })),
        tl_enums::InputPeer::User(u) => Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: u.user_id,
        })),
        _ => None,
    }
}

fn input_peer(index: usize) -> tl_enums::InputPeer {
    let id = index as i64;
    match kind(index) {
        Kind::BasicGroup => tl_enums::InputPeer::Chat(tl_types::InputPeerChat {
            chat_id: 1_000 + id,
        }),
        Kind::Megagroup | Kind::Broadcast => {
            tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                channel_id: 5_000 + id,
                access_hash: 77,
            })
        }
        Kind::User => tl_enums::InputPeer::User(tl_types::InputPeerUser {
            user_id: 10_000 + id,
            access_hash: 77,
        }),
    }
}

fn date(index: usize) -> i32 {
    let position = if archived(index) {
        index - MAIN_CHATS
    } else {
        index
    };
    BASE_DATE - (position as i32) * 60
}

fn top_message_id(index: usize) -> i32 {
    match kind(index) {
        Kind::Megagroup | Kind::Broadcast => 1_000 + index as i32,
        Kind::BasicGroup | Kind::User => 100_000 - index as i32,
    }
}

fn tl_dialog(index: usize) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_peer(index),
        top_message: top_message_id(index),
        read_inbox_max_id: top_message_id(index),
        read_outbox_max_id: 0,
        unread_count: 0,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
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
        pts: matches!(kind(index), Kind::Megagroup | Kind::Broadcast).then_some(10),
        draft: None,
        folder_id: archived(index).then_some(1),
        ttl_period: None,
    })
}

fn tl_message(index: usize, id: i32, date: i32) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: kind(index) == Kind::Broadcast,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        guestchat_via_from: None,
        id,
        from_id: None,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_peer(index),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date,
        message: format!("message in chat {index}"),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    })
}

fn tl_user(index: usize) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: false,
        contact: index.is_multiple_of(2),
        mutual_contact: false,
        deleted: false,
        bot: false,
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
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id: 10_000 + index as i64,
        access_hash: Some(77),
        first_name: Some(format!("User {index}")),
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

fn tl_chat(index: usize) -> tl_enums::Chat {
    let id = index as i64;
    if kind(index) == Kind::BasicGroup {
        return tl_enums::Chat::Chat(tl_types::Chat {
            creator: false,
            left: false,
            deactivated: false,
            call_active: false,
            call_not_empty: false,
            noforwards: false,
            id: 1_000 + id,
            title: format!("Group {index}"),
            photo: tl_enums::ChatPhoto::Empty,
            participants_count: 5,
            date: BASE_DATE - 100_000,
            version: 1,
            migrated_to: None,
            admin_rights: None,
            default_banned_rights: None,
        });
    }
    let megagroup = kind(index) == Kind::Megagroup;
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: false,
        left: false,
        broadcast: !megagroup,
        verified: false,
        megagroup,
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
        id: 5_000 + id,
        access_hash: Some(77),
        title: format!("Channel {index}"),
        username: None,
        photo: tl_enums::ChatPhoto::Empty,
        date: BASE_DATE - 100_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(50),
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

struct Payload {
    dialogs: Vec<tl_enums::Dialog>,
    messages: Vec<tl_enums::Message>,
    chats: Vec<tl_enums::Chat>,
    users: Vec<tl_enums::User>,
}

fn payload(indices: &[usize]) -> Payload {
    let mut out = Payload {
        dialogs: Vec::new(),
        messages: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    };
    for &index in indices {
        out.dialogs.push(tl_dialog(index));
        out.messages
            .push(tl_message(index, top_message_id(index), date(index)));
        if kind(index) == Kind::User {
            out.users.push(tl_user(index));
        } else {
            out.chats.push(tl_chat(index));
        }
    }
    out
}

fn empty_state() -> tl_enums::updates::State {
    tl_enums::updates::State::State(tl_types::updates::State {
        pts: 1,
        qts: 0,
        date: BASE_DATE,
        seq: 0,
        unread_count: 0,
    })
}

fn peer_dialogs(indices: &[usize]) -> tl_enums::messages::PeerDialogs {
    let p = payload(indices);
    tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
        dialogs: p.dialogs,
        messages: p.messages,
        chats: p.chats,
        users: p.users,
        state: empty_state(),
    })
}

fn work_folder() -> tl_enums::DialogFilter {
    tl_enums::DialogFilter::DialogFilter(tl_types::DialogFilter {
        contacts: false,
        non_contacts: false,
        groups: true,
        broadcasts: false,
        bots: false,
        exclude_muted: false,
        exclude_read: false,
        exclude_archived: true,
        title_noanimate: false,
        id: FOLDER_ID,
        title: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
            text: "Work".to_owned(),
            entities: Vec::new(),
        }),
        emoticon: None,
        color: None,
        pinned_peers: Vec::new(),
        include_peers: vec![input_peer(LATE_MAIN_USER), input_peer(LATE_ARCHIVED_USER)],
        exclude_peers: Vec::new(),
    })
}

/// Constructor ID and, for `messages.getDialogs`, its `folder_id`.
type RecordedCall = (u32, Option<i32>);

/// A server holding `MAIN_CHATS` chats in the main list and `ARCHIVED_CHATS` in the archive.
#[derive(Clone)]
struct FakeServer {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    peer_dialog_requests: Arc<Mutex<Vec<Vec<usize>>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeServer {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(8);
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            peer_dialog_requests: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn get_dialogs_calls(&self, folder: i32) -> usize {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|(ctor, f)| {
                *ctor == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID && *f == Some(folder)
            })
            .count()
    }

    fn requested_peer_dialogs(&self) -> Vec<usize> {
        self.peer_dialog_requests
            .lock()
            .expect("peer dialog requests")
            .iter()
            .flatten()
            .copied()
            .collect()
    }

    fn answer(&self, ctor: u32, body: &[u8]) -> Result<Vec<u8>, InvocationError> {
        fn bad(e: impl std::fmt::Display) -> InvocationError {
            InvocationError::Deserialize(e.to_string())
        }
        if ctor == tl_fns::messages::GetDialogs::CONSTRUCTOR_ID {
            let req = tl_fns::messages::GetDialogs::from_bytes(body).map_err(bad)?;
            self.calls
                .lock()
                .expect("calls")
                .push((ctor, req.folder_id));
            let range = if req.folder_id == Some(1) {
                MAIN_CHATS..MAIN_CHATS + ARCHIVED_CHATS
            } else {
                0..MAIN_CHATS
            };
            let count = range.len() as i32;
            let page: Vec<usize> = range
                .filter(|&i| req.offset_date == 0 || date(i) < req.offset_date)
                .take((req.limit.max(0) as usize).min(SERVER_PAGE_CAP))
                .collect();
            let p = payload(&page);
            return Ok(
                tl_enums::messages::Dialogs::Slice(tl_types::messages::DialogsSlice {
                    count,
                    dialogs: p.dialogs,
                    messages: p.messages,
                    chats: p.chats,
                    users: p.users,
                })
                .to_bytes(),
            );
        }
        self.calls.lock().expect("calls").push((ctor, None));
        if ctor == tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID {
            return Ok(peer_dialogs(&[]).to_bytes());
        }
        if ctor == tl_fns::messages::GetPeerDialogs::CONSTRUCTOR_ID {
            let req = tl_fns::messages::GetPeerDialogs::from_bytes(body).map_err(bad)?;
            let indices: Vec<usize> = req
                .peers
                .iter()
                .filter_map(|p| match p {
                    tl_enums::InputDialogPeer::InputDialogPeer(d) => index_of_input(&d.peer),
                    _ => None,
                })
                .collect();
            self.peer_dialog_requests
                .lock()
                .expect("peer dialog requests")
                .push(indices.clone());
            return Ok(peer_dialogs(&indices).to_bytes());
        }
        if ctor == tl_fns::messages::GetDialogFilters::CONSTRUCTOR_ID {
            return Ok(tl_enums::messages::DialogFilters::DialogFilters(
                tl_types::messages::DialogFilters {
                    tags_enabled: false,
                    filters: vec![tl_enums::DialogFilter::Default, work_folder()],
                },
            )
            .to_bytes());
        }
        Err(InvocationError::Dropped)
    }
}

impl RpcClient for FakeServer {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(2, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let raw = self.answer(ctor, &bytes)?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn start() -> (tokio::runtime::Runtime, FakeServer, LiveSession) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let server = FakeServer::new();
    let runtime = SessionRuntime::start_on(
        DataUserId(100),
        crate::ErasedRpcClient::new(server.clone()),
        rt.handle().clone(),
        SessionRuntimeConfig {
            load_initial_dialogs: true,
            ..SessionRuntimeConfig::default()
        },
    )
    .expect("start SessionRuntime");
    let live = LiveSession::new(Arc::new(runtime), 1);
    (rt, server, live)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn list_peers(live: &LiveSession, list: DialogListQuery) -> (Vec<DataPeerId>, bool) {
    let slice = live.dialog_slice(list, 0, 10_000);
    (
        slice
            .entries
            .iter()
            .map(|item| to_data_peer(item.peer_id))
            .collect(),
        slice.has_more,
    )
}

#[test]
fn chat_list_loads_every_page_and_folders_get_chats_from_later_pages_and_the_archive() {
    let (_rt, server, live) = start();
    live.start_chats_list_request(|| {});
    // The folder tabs load their filters like the main window does on start.
    let _ = live.chat_folders();

    wait_until("the main list to load completely", || {
        !live.dialogs_preload_running()
            && live.read_session(|s| s.main_list().loaded())
            && list_peers(&live, DialogListQuery::Filter(FilterId::ALL))
                .0
                .len()
                == MAIN_CHATS
    });

    // Every main chat is listed in server order, none from the archive.
    let (main, main_has_more) = list_peers(&live, DialogListQuery::Filter(FilterId::ALL));
    let expected_main: Vec<DataPeerId> = (0..MAIN_CHATS).map(data_peer).collect();
    assert_eq!(main, expected_main);
    assert!(!main_has_more, "a fully loaded list must not ask for more");
    // 20 on the first request, then pages capped at 100, then the empty page that ends it.
    assert!(server.get_dialogs_calls(0) >= 4);

    // The folder holds every non-archived group plus both include_peers, even the archived user
    // that no loaded page brought (`messages.getPeerDialogs`).
    wait_until("the folder's include peers", || {
        live.read_session(|s| s.history(data_peer(LATE_ARCHIVED_USER)).is_some())
    });
    let (folder, _) = list_peers(&live, DialogListQuery::Filter(FilterId(FOLDER_ID)));
    let folder: HashSet<DataPeerId> = folder.into_iter().collect();
    let mut expected_folder: HashSet<DataPeerId> = (0..MAIN_CHATS)
        .filter(|&i| matches!(kind(i), Kind::BasicGroup | Kind::Megagroup))
        .map(data_peer)
        .collect();
    expected_folder.insert(data_peer(LATE_MAIN_USER));
    expected_folder.insert(data_peer(LATE_ARCHIVED_USER));
    assert_eq!(folder, expected_folder);
    assert!(
        server
            .requested_peer_dialogs()
            .contains(&LATE_ARCHIVED_USER),
        "the archived include peer must be requested with messages.getPeerDialogs"
    );

    // The archive got its first page; archived groups are there and never in the main list.
    let (archive, archive_has_more) = list_peers(&live, DialogListQuery::Archive);
    assert!(archive.len() >= 20);
    assert!(archive_has_more);
    assert!(archive.contains(&data_peer(MAIN_CHATS)));
    let main_set: HashSet<DataPeerId> = main.into_iter().collect();
    for index in MAIN_CHATS..MAIN_CHATS + ARCHIVED_CHATS {
        assert!(
            !main_set.contains(&data_peer(index)),
            "archived chat {index} in main list"
        );
    }

    // Scrolling the archive to its end (each `dialog_slice` past the loaded chats asks for the
    // next page) loads its remaining chats.
    wait_until("the rest of the archive", || {
        let (archive, has_more) = list_peers(&live, DialogListQuery::Archive);
        archive.len() == ARCHIVED_CHATS && !has_more
    });
    let (archive, _) = list_peers(&live, DialogListQuery::Archive);
    let expected_archive: Vec<DataPeerId> = (MAIN_CHATS..MAIN_CHATS + ARCHIVED_CHATS)
        .map(data_peer)
        .collect();
    assert_eq!(archive, expected_archive);
    assert!(server.get_dialogs_calls(1) >= 2);
}

#[test]
fn new_message_in_an_archived_group_not_loaded_yet_never_shows_in_the_main_list() {
    let (_rt, server, live) = start();
    live.start_chats_list_request(|| {});
    wait_until("the first pages", || {
        !live.dialogs_preload_running() && live.read_session(|s| s.main_list().loaded())
    });
    let group = data_peer(LATE_ARCHIVED_GROUP);
    assert!(live.read_session(|s| s.history(group).is_none()));

    // A new message arrives (`updateNewMessage`) for an archived group whose dialog no page
    // brought yet.
    live.write_session(|s| {
        let msg = Message::new_text(
            group,
            MsgId(200_000),
            None,
            BASE_DATE + 10,
            tg_data::TextWithEntities::plain("hello"),
        );
        s.apply_sent_or_new_message(msg);
        assert!(s.message(FullMsgId::new(group, MsgId(200_000))).is_some());
    });
    let in_main = |live: &LiveSession| {
        live.read_session(|s| {
            s.main_dialogs()
                .contains(&tg_data::DialogKey::History(group))
        })
    };
    assert!(
        !in_main(&live),
        "a chat of unknown folder must stay out of the main list"
    );

    wait_until("the group's dialog entry", || {
        assert!(!in_main(&live), "archived group shown in the main list");
        live.read_session(|s| s.history(group).is_some_and(tg_data::History::is_archived))
    });
    assert!(
        server
            .requested_peer_dialogs()
            .contains(&LATE_ARCHIVED_GROUP)
    );
    assert!(live.read_session(|s| {
        s.archive_dialogs()
            .contains(&tg_data::DialogKey::History(group))
    }));
    assert!(!in_main(&live));
}

#[test]
fn a_fully_loaded_list_is_not_requested_again() {
    let (_rt, server, live) = start();
    live.start_chats_list_request(|| {});
    wait_until("the main list", || {
        !live.dialogs_preload_running() && live.read_session(|s| s.main_list().loaded())
    });
    let calls = server.get_dialogs_calls(0);
    let _ = live.dialog_slice(DialogListQuery::Filter(FilterId::ALL), 0, 10_000);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        server.get_dialogs_calls(0),
        calls,
        "a loaded list must not be requested again"
    );
}
