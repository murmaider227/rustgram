//! Regression test ensuring `tg_api::scheduled::get_quick_replies` preserves the server-reported
//! `QuickReply::count` when ingesting the single top preview message from `messages.quickReplies`
//! (`ShortcutMessages::preloadShortcuts` in `data/business/data_shortcut_messages.cpp:460-479`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tg_api::scheduled::{
    QuickRepliesOutcome, QuickReplyMessagesOutcome, get_quick_replies, get_quick_reply_messages,
};
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{Session, UserId};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};

type ResponseQueue = Arc<Mutex<VecDeque<(u32, Vec<u8>)>>>;

#[derive(Clone, Default)]
struct MockRpc {
    responses: ResponseQueue,
}

impl MockRpc {
    fn enqueue<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, resp.to_bytes()));
    }
}

impl RpcClient for MockRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let popped = self.responses.lock().expect("lock").pop_front();
        match popped {
            Some((expected, raw)) => {
                assert_eq!(expected, ctor);
                R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
            }
            None => Err(InvocationError::Dropped),
        }
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        self.invoke(req).await
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}
}

fn tl_quick_reply_msg(id: i32, shortcut_id: i32, date: i32, text: &str) -> tl_enums::Message {
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
        guestchat_via_from: None,
        rich_message: None,
        id,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 100 }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date,
        message: text.to_owned(),
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
        quick_reply_shortcut_id: Some(shortcut_id),
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
    })
}

#[tokio::test]
async fn get_quick_replies_keeps_server_count_when_ingesting_preview_message() {
    let rpc = MockRpc::default();
    let mut session = Session::new(UserId(100));

    // Server reports a shortcut with 5 messages, sending only the top preview message (#50).
    let resp = tl_enums::messages::QuickReplies::QuickReplies(tl_types::messages::QuickReplies {
        quick_replies: vec![tl_enums::QuickReply::QuickReply(tl_types::QuickReply {
            shortcut_id: 7,
            shortcut: "hello".to_owned(),
            top_message: 50,
            count: 5,
        })],
        messages: vec![tl_quick_reply_msg(50, 7, 1_700_000_050, "Preview top")],
        chats: Vec::new(),
        users: Vec::new(),
    });
    rpc.enqueue(tl_fns::messages::GetQuickReplies::CONSTRUCTOR_ID, &resp);

    let outcome = get_quick_replies(&rpc, &mut session)
        .await
        .expect("get_quick_replies");
    assert_eq!(outcome, QuickRepliesOutcome::Updated(1));

    let sc = session
        .shortcut_messages()
        .shortcut(7)
        .expect("shortcut 7 present");
    assert_eq!(
        sc.count, 5,
        "server QuickReply::count (5) must NOT be overwritten with preview slice length (1)"
    );
    assert!(
        !session.shortcut_messages().has_unknown_top_messages(),
        "top preview message must still be ingested so has_unknown_top_messages() is false"
    );
    assert_eq!(session.shortcut_messages().messages(7).len(), 1);

    // Fetching full shortcut messages reconciles the list and updates count to the actual count.
    let full_resp = tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
        messages: vec![
            tl_quick_reply_msg(48, 7, 1_700_000_048, "One"),
            tl_quick_reply_msg(49, 7, 1_700_000_049, "Two"),
            tl_quick_reply_msg(50, 7, 1_700_000_050, "Three"),
        ],
        topics: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    });
    rpc.enqueue(
        tl_fns::messages::GetQuickReplyMessages::CONSTRUCTOR_ID,
        &full_resp,
    );

    let full_outcome = get_quick_reply_messages(&rpc, &mut session, 7, 10_000, false)
        .await
        .expect("get_quick_reply_messages");
    assert_eq!(full_outcome, QuickReplyMessagesOutcome::Updated(3));
    assert_eq!(
        session.shortcut_messages().shortcut(7).expect("sc 7").count,
        3
    );
    assert_eq!(session.shortcut_messages().messages(7).len(), 3);
}
