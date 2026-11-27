//! Walk-time actions: the runner writes one JSON command per line on stdin and reads one JSON
//! answer per line on stdout. Only the chats the fixture created (and the A<->B private chat)
//! can be addressed.

use std::path::Path;

use serde_json::{Value, json};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::ops::{self, Out};
use crate::side::u16len;
use crate::world::World;

const CHATS: &[&str] = &["pm", "basic", "super", "channel", "archived", "saved"];

pub async fn run(w: &mut World, media: &Path) -> Result<(), String> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let ready = json!({ "ok": true, "ready": true }).to_string() + "\n";
    stdout
        .write_all(ready.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdout.flush().await.map_err(|e| e.to_string())?;
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(line) {
            Ok(cmd) => {
                if cmd["op"] == "quit" {
                    break;
                }
                match one(w, media, &cmd).await {
                    Ok(v) => v,
                    Err(e) => json!({ "ok": false, "error": e }),
                }
            }
            Err(e) => json!({ "ok": false, "error": format!("bad json: {e}") }),
        };
        let out = answer.to_string() + "\n";
        stdout
            .write_all(out.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdout.flush().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn one(w: &mut World, media: &Path, cmd: &Value) -> Result<Value, String> {
    let op = cmd["op"].as_str().unwrap_or("");
    let who = cmd["who"].as_str().unwrap_or("b");
    let chat = cmd["chat"].as_str().unwrap_or("pm");
    if !CHATS.contains(&chat) {
        return Err(format!("chat {chat} is not a walk chat"));
    }
    let side = w.side(who)?;
    let peer = w.peer(who, chat)?;
    match op {
        "send" => {
            let text = cmd["text"].as_str().unwrap_or("walk");
            let mut out = Out::default();
            if cmd["mention"].as_bool() == Some(true) {
                let other = if who == "a" { "b" } else { "a" };
                let word = text.split_whitespace().next().unwrap_or(text);
                out.entities
                    .push(tl_enums::MessageEntity::InputMessageEntityMentionName(
                        tl_types::InputMessageEntityMentionName {
                            offset: 0,
                            length: u16len(word.trim_end_matches(',')),
                            user_id: w.input_user(who, other)?,
                        },
                    ));
            }
            out.reply_to = cmd["reply_to"].as_i64().and_then(|v| i32::try_from(v).ok());
            let id = ops::send_text(side, &peer, text, &out).await?;
            Ok(json!({ "ok": true, "id": id }))
        }
        "typing" => {
            ops::typing(side, &peer, true).await?;
            Ok(json!({ "ok": true }))
        }
        "cancel_typing" => {
            ops::typing(side, &peer, false).await?;
            Ok(json!({ "ok": true }))
        }
        "react" => {
            let id = cmd["id"]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or("no id")?;
            ops::react(side, &peer, id, cmd["emoji"].as_str().unwrap_or("👍")).await?;
            Ok(json!({ "ok": true }))
        }
        "edit" => {
            let id = cmd["id"]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or("no id")?;
            ops::edit(side, &peer, id, cmd["text"].as_str().unwrap_or("edited")).await?;
            Ok(json!({ "ok": true }))
        }
        "delete" => {
            let ids: Vec<i32> = cmd["ids"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_i64())
                        .filter_map(|v| i32::try_from(v).ok())
                        .collect()
                })
                .unwrap_or_default();
            // Only messages this run created (the runner tracks them) may be deleted.
            ops::delete(side, &peer, ids).await?;
            Ok(json!({ "ok": true }))
        }
        "last" => {
            let n = cmd["n"].as_i64().unwrap_or(5).clamp(1, 100) as i32;
            let msgs = ops::history(side, &peer, 0, n).await?;
            let list: Vec<Value> = msgs
                .iter()
                .map(|m| json!({ "id": m.id, "out": m.out, "text": m.message, "mentioned": m.mentioned }))
                .collect();
            Ok(json!({ "ok": true, "messages": list }))
        }
        "dialog" => {
            let got = side
                .call(
                    "getPeerDialogs",
                    &tl_fns::messages::GetPeerDialogs {
                        peers: vec![tl_enums::InputDialogPeer::InputDialogPeer(
                            tl_types::InputDialogPeer { peer },
                        )],
                    },
                )
                .await?;
            let tl_enums::messages::PeerDialogs::PeerDialogs(p) = got;
            let d = p.dialogs.into_iter().find_map(|d| match d {
                tl_enums::Dialog::Dialog(d) => Some(d),
                _ => None,
            });
            Ok(match d {
                Some(d) => json!({
                    "ok": true,
                    "unread": d.unread_count,
                    "unread_mentions": d.unread_mentions_count,
                    "unread_reactions": d.unread_reactions_count,
                    "read_inbox_max_id": d.read_inbox_max_id,
                    "top_message": d.top_message,
                    "has_draft": matches!(d.draft, Some(tl_enums::DraftMessage::DraftMessage(_))),
                    "folder_id": d.folder_id,
                }),
                None => json!({ "ok": false, "error": "no dialog" }),
            })
        }
        "read_mentions" => {
            side.call(
                "readMentions",
                &tl_fns::messages::ReadMentions {
                    peer,
                    top_msg_id: None,
                },
            )
            .await?;
            Ok(json!({ "ok": true }))
        }
        "cleanup" => {
            // Deletes this account's own recent messages that carry the walk marker
            // (messages the clients sent during the walk).
            let marker = cmd["marker"].as_str().unwrap_or("walk tmp");
            let msgs = ops::history(side, &peer, 0, 100).await?;
            let ids: Vec<i32> = msgs
                .iter()
                .filter(|m| m.out && m.message.contains(marker))
                .map(|m| m.id)
                .collect();
            let n = ids.len();
            if n > 0 {
                ops::delete(side, &peer, ids).await?;
            }
            Ok(json!({ "ok": true, "deleted": n }))
        }
        "refresh" => {
            crate::fixture::save_draft(w).await?;
            crate::fixture::story(w, media).await?;
            Ok(json!({ "ok": true, "notes": w.state["notes"].clone() }))
        }
        "state" => Ok(
            json!({ "ok": true, "msgs": w.state["msgs"].clone(), "notes": w.state["notes"].clone() }),
        ),
        _ => Err(format!("unknown op {op}")),
    }
}
