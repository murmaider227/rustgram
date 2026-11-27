//! `retire`: B removes the fixture chats of a previous account A (after A's login was lost), so a
//! new fixture with a new account A does not find the old chats by title. Only chats the fixture
//! created are touched: B deletes the supergroup and channel it owns, leaves the basic groups, and
//! clears its side of the private chat with the old A.

use std::path::Path;

use serde_json::Value;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::side::{Side, load_test_credentials};

pub async fn run(b_dir: &Path, old_state: &Path) -> Result<(), String> {
    let creds = load_test_credentials()?;
    let b = Side::open("b", b_dir, &creds).await?;
    let state: Value = crate::world::World::load_state(&old_state.to_path_buf());
    if state["ids"]["b"].as_i64() != Some(b.self_id) {
        return Err("the old state file belongs to another account B".into());
    }
    let mut done = Vec::new();
    for key in ["super", "channel"] {
        let c = &state["chats"][key];
        if c["owner"].as_str() != Some("b") {
            continue;
        }
        let (Some(id), Some(hash)) = (c["id"].as_i64(), c["hash_b"].as_i64()) else {
            continue;
        };
        let r = b
            .call(
                "deleteChannel",
                &tl_fns::channels::DeleteChannel {
                    channel: tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                        channel_id: id,
                        access_hash: hash,
                    }),
                },
            )
            .await;
        done.push(format!(
            "{key}: {}",
            if r.is_ok() { "deleted" } else { "kept" }
        ));
    }
    for key in ["basic", "archived"] {
        let Some(id) = state["chats"][key]["id"].as_i64() else {
            continue;
        };
        let r = b
            .call(
                "deleteChatUser",
                &tl_fns::messages::DeleteChatUser {
                    revoke_history: false,
                    chat_id: id,
                    user_id: tl_enums::InputUser::Self_,
                },
            )
            .await;
        done.push(format!(
            "{key}: {}",
            if r.is_ok() { "left" } else { "kept" }
        ));
    }
    if let (Some(id), Some(hash)) = (state["ids"]["a"].as_i64(), state["hash"]["b_a"].as_i64()) {
        let r = b
            .call(
                "deleteHistory",
                &tl_fns::messages::DeleteHistory {
                    just_clear: false,
                    revoke: false,
                    peer: tl_enums::InputPeer::User(tl_types::InputPeerUser {
                        user_id: id,
                        access_hash: hash,
                    }),
                    max_id: 0,
                    min_date: None,
                    max_date: None,
                },
            )
            .await;
        done.push(format!(
            "pm: {}",
            if r.is_ok() { "cleared" } else { "kept" }
        ));
    }
    println!("retired: {}", done.join(", "));
    Ok(())
}
