//! A failed cloud draft save does not strand the drafts after it (TGB-2917): before, they
//! stayed marked "in flight" after `flush_due` returned the error and were never saved.

mod common;

use std::time::{Duration, Instant};

use common::{FakeRpcClient, rpc_err};
use tg_api::{DraftManager, SAVE_CLOUD_DRAFT_TIMEOUT};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

fn draft(user_id: i64, text: &str) -> tl_fns::messages::SaveDraft {
    tl_fns::messages::SaveDraft {
        no_webpage: false,
        invert_media: false,
        reply_to: None,
        peer: tl_enums::InputPeer::User(tl_types::InputPeerUser {
            user_id,
            access_hash: user_id * 3,
        }),
        message: text.into(),
        entities: None,
        media: None,
        effect: None,
        suggested_post: None,
        rich_message: None,
    }
}

#[tokio::test]
async fn drafts_after_a_failed_save_are_saved_by_the_next_flush() {
    let client = FakeRpcClient::new(2);
    let t0 = Instant::now();
    let mut drafts = DraftManager::new();
    drafts.schedule_save(None, draft(11, "one"), t0);
    drafts.schedule_save(None, draft(12, "two"), t0);

    client.enqueue_err::<tl_fns::messages::SaveDraft>(rpc_err(500, "INTERNAL"));
    let due = t0 + SAVE_CLOUD_DRAFT_TIMEOUT;
    assert!(drafts.flush_due(&client, due).await.is_err());
    assert_eq!(client.calls_of::<tl_fns::messages::SaveDraft>().len(), 1);

    // The draft that never went out is still waiting and goes with the next flush.
    assert!(
        drafts.is_quit_prevent(),
        "the unsent draft is still pending"
    );
    client.enqueue_ok::<tl_fns::messages::SaveDraft>(&true);
    let saved = drafts
        .flush_due(&client, due + Duration::from_millis(10))
        .await
        .expect("second flush");
    assert_eq!(saved, 1);
    assert_eq!(client.calls_of::<tl_fns::messages::SaveDraft>().len(), 2);
    assert!(!drafts.is_quit_prevent());
}
