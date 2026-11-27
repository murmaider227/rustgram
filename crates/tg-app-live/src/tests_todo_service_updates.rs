//! Another member's checklist ticks and appended tasks reach a checklist already loaded in
//! history through live `updateNewMessage` service messages (tdesktop 7.4.7 `fcf66d9`,
//! `376009b`; TGB-3150). Fake RPC client only.

use std::sync::Arc;

use tg_api::{DcId, InvocationError, RpcClient, convert::apply_tl_users_and_chats};
use tg_data::{
    FullMsgId as DataFullMsgId, MsgId as DataMsgId, PeerId as DataPeerId, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{RemoteCall, enums as tl_enums, types as tl_types};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::convert::message_to_history_message;
use tg_app_core::core_types::dto::todo::TodoListAttachment as UiTodoList;
use tg_app_core::history::backend::HistoryMedia;
use tg_app_core::stand_in::types::PeerId;

const SELF: i64 = 1000;
const MEMBER: i64 = 1001;
const CHECKLIST: i32 = 77;

/// Answers nothing: these tests only feed updates.
#[derive(Clone)]
struct SilentRpc {
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl RpcClient for SilentRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        _req: &R,
    ) -> Result<R::Return, Self::Error> {
        Err(InvocationError::Dropped)
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        _req: &R,
    ) -> Result<R::Return, Self::Error> {
        Err(InvocationError::Dropped)
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn live_session() -> LiveSession {
    let (updates_tx, _) = broadcast::channel(16);
    let rt = Arc::new(
        SessionRuntime::start(
            DataUserId(SELF as u64),
            crate::ErasedRpcClient::new(SilentRpc { updates_tx }),
            SessionRuntimeConfig {
                init_updates_state: false,
                run_get_difference_on_start: false,
                load_initial_dialogs: false,
                fetch_self_user_on_start: false,
                ..SessionRuntimeConfig::default()
            },
        )
        .expect("start SessionRuntime"),
    );
    let session = Arc::clone(rt.session());
    crate::media::block_on_runtime(rt.tokio_handle(), async move {
        let mut s = session.write().await;
        let users = [
            tl_user(SELF, "Taylor", true),
            tl_user(MEMBER, "Mira", false),
        ];
        apply_tl_users_and_chats(&mut s, &users, &[]);
    });
    LiveSession::new(rt, 1)
}

fn tl_user(id: i64, first_name: &str, is_self: bool) -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: is_self,
        contact: !is_self,
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
        id,
        access_hash: Some(id * 101 + 3),
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

fn user_peer(id: i64) -> tl_enums::Peer {
    tl_enums::Peer::User(tl_types::PeerUser { user_id: id })
}

fn text(s: &str) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: s.to_owned(),
        entities: Vec::new(),
    })
}

fn todo_item(id: i32, title: &str) -> tl_enums::TodoItem {
    tl_enums::TodoItem::TodoItem(tl_types::TodoItem {
        id,
        title: text(title),
    })
}

fn checklist_message() -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: true,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
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
        id: CHECKLIST,
        from_id: Some(user_peer(SELF)),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: user_peer(MEMBER),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date: 1_795_341_600,
        message: String::new(),
        media: Some(tl_enums::MessageMedia::ToDo(tl_types::MessageMediaToDo {
            todo: tl_enums::TodoList::TodoList(tl_types::TodoList {
                others_can_append: true,
                others_can_complete: true,
                title: text("Weekend"),
                list: vec![todo_item(1, "Tickets"), todo_item(2, "Hotel")],
            }),
            completions: None,
        })),
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

/// A service message from the other member that refers to the checklist.
fn member_service(id: i32, date: i32, action: tl_enums::MessageAction) -> tl_enums::Message {
    tl_enums::Message::Service(tl_types::MessageService {
        out: false,
        mentioned: false,
        media_unread: false,
        reactions_are_possible: false,
        silent: false,
        post: false,
        legacy: false,
        id,
        from_id: Some(user_peer(MEMBER)),
        peer_id: user_peer(MEMBER),
        saved_peer_id: None,
        reply_to: Some(tl_enums::MessageReplyHeader::MessageReplyHeader(
            tl_types::MessageReplyHeader {
                reply_to_scheduled: false,
                forum_topic: false,
                quote: false,
                reply_to_ephemeral: false,
                reply_to_msg_id: Some(CHECKLIST),
                reply_to_peer_id: None,
                reply_from: None,
                reply_media: None,
                reply_to_top_id: None,
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            },
        )),
        date,
        action,
        reactions: None,
        ttl_period: None,
    })
}

fn new_message_update(message: tl_enums::Message, pts: i32) -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message,
            pts,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_341_600,
        seq: 0,
    })
}

/// The checklist as the history view converts it.
fn loaded_checklist(live: &LiveSession) -> UiTodoList {
    let session = Arc::clone(live.runtime().session());
    crate::media::block_on_runtime(live.runtime().tokio_handle(), async move {
        let s = session.read().await;
        let id = DataFullMsgId::new(
            DataPeerId::from_user(DataUserId(MEMBER as u64)),
            DataMsgId(i64::from(CHECKLIST)),
        );
        let message = s.message(id).expect("checklist stored");
        match message_to_history_message(&s, message).media {
            Some(HistoryMedia::TodoList(todo)) => todo,
            other => panic!("expected a checklist, got {other:?}"),
        }
    })
}

#[test]
fn another_members_ticks_and_tasks_update_the_loaded_checklist() {
    let live = live_session();
    live.apply_updates_to_session(&new_message_update(checklist_message(), 10));
    assert_eq!(loaded_checklist(&live).completed_count(), 0);

    live.apply_updates_to_session(&new_message_update(
        member_service(
            78,
            1_795_341_700,
            tl_enums::MessageAction::TodoCompletions(tl_types::MessageActionTodoCompletions {
                completed: vec![2],
                incompleted: Vec::new(),
            }),
        ),
        11,
    ));
    let todo = loaded_checklist(&live);
    assert_eq!(todo.completed_count(), 1, "footer count");
    let hotel = &todo.items[1];
    assert_eq!(hotel.id, 2);
    assert_eq!(hotel.completed_by, Some(PeerId::from_user(MEMBER as u64)));

    live.apply_updates_to_session(&new_message_update(
        member_service(
            79,
            1_795_341_800,
            tl_enums::MessageAction::TodoAppendTasks(tl_types::MessageActionTodoAppendTasks {
                list: vec![todo_item(3, "Visas")],
            }),
        ),
        12,
    ));
    let todo = loaded_checklist(&live);
    let titles: Vec<&str> = todo.items.iter().map(|t| t.text.text.as_str()).collect();
    assert_eq!(titles, ["Tickets", "Hotel", "Visas"]);
    assert_eq!(todo.completed_count(), 1);
}
