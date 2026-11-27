//! One-account mode: the walk's clients run as ONE fixture account ("main", a or b) and the peer
//! acts only as that account (server-side checks, cleanup, the account's own list state). Used
//! while no second account can act as the other side; anything that needs the other side answers
//! with an error the runner turns into "blocked".
//!
//! ```text
//! tg-walk-peer solo-fixture --main <login> --who a|b --state <state.json>
//! tg-walk-peer solo-serve   --main <login> --who a|b --state <state.json>
//! ```

use std::path::Path;

use serde_json::{Value, json};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::ops::{self, Out};
use crate::side::{Side, load_test_credentials, now, twe};
use crate::world::{World, input_peer};

struct Solo {
    side: Side,
    who: String,
    state: Value,
    state_path: std::path::PathBuf,
}

impl Solo {
    fn peer(&self, chat: &str) -> Result<tl_enums::InputPeer, String> {
        input_peer(&self.state, &self.who, chat)
    }

    fn done(&mut self, step: &str) -> Result<(), String> {
        self.state[format!("done_{}", self.who)][step] = Value::Bool(true);
        let text = serde_json::to_string_pretty(&self.state).map_err(|e| e.to_string())?;
        std::fs::write(&self.state_path, text).map_err(|e| format!("write state: {e}"))
    }

    fn todo(&self, step: &str) -> bool {
        self.state[format!("done_{}", self.who)][step].as_bool() != Some(true)
    }
}

async fn open(main: &Path, who: &str, state_path: &Path) -> Result<Solo, String> {
    if who != "a" && who != "b" {
        return Err("--who must be a or b".into());
    }
    let creds = load_test_credentials()?;
    let side = Side::open(if who == "a" { "a" } else { "b" }, main, &creds).await?;
    let state = World::load_state(&state_path.to_path_buf());
    if state["ids"][who].as_i64() != Some(side.self_id) {
        return Err(format!("the login is not account {who} of this state file"));
    }
    Ok(Solo {
        side,
        who: who.to_owned(),
        state,
        state_path: state_path.to_path_buf(),
    })
}

/// The main account's own chat-list state (what the fixture gave account A before).
pub async fn fixture(main: &Path, who: &str, state_path: &Path) -> Result<(), String> {
    let mut s = open(main, who, state_path).await?;
    if s.todo("saved") {
        ops::send_text(
            &s.side,
            &tl_enums::InputPeer::Self_,
            "Walk saved note: shopping list, milk, bread, coffee.",
            &Out::default(),
        )
        .await?;
        s.done("saved")?;
    }
    if s.todo("scheduled") {
        ops::send_text(
            &s.side,
            &s.peer("pm")?,
            "Walk scheduled message: happy new year!",
            &Out {
                schedule: Some(now() + 200 * 24 * 3600),
                ..Out::default()
            },
        )
        .await?;
        s.done("scheduled")?;
    }
    if s.todo("folder") {
        let tl_enums::messages::DialogFilters::DialogFilters(f) = s
            .side
            .call("getDialogFilters", &tl_fns::messages::GetDialogFilters)
            .await?;
        let mut used = std::collections::HashSet::new();
        for d in &f.filters {
            if let tl_enums::DialogFilter::DialogFilter(d) = d {
                used.insert(d.id);
            }
        }
        let id = (2..255).find(|i| !used.contains(i)).unwrap_or(2);
        s.side
            .call(
                "updateDialogFilter",
                &tl_fns::messages::UpdateDialogFilter {
                    id,
                    filter: Some(tl_enums::DialogFilter::DialogFilter(
                        tl_types::DialogFilter {
                            contacts: false,
                            non_contacts: false,
                            groups: false,
                            broadcasts: false,
                            bots: false,
                            exclude_muted: false,
                            exclude_read: false,
                            exclude_archived: false,
                            title_noanimate: false,
                            id,
                            title: twe("Walk"),
                            emoticon: None,
                            color: None,
                            pinned_peers: Vec::new(),
                            include_peers: vec![
                                s.peer("super")?,
                                s.peer("basic")?,
                                s.peer("channel")?,
                            ],
                            exclude_peers: Vec::new(),
                        },
                    )),
                },
            )
            .await?;
        s.done("folder")?;
    }
    if s.todo("archive") {
        s.side
            .call(
                "editPeerFolders",
                &tl_fns::folders::EditPeerFolders {
                    folder_peers: vec![tl_enums::InputFolderPeer::InputFolderPeer(
                        tl_types::InputFolderPeer {
                            peer: s.peer("archived")?,
                            folder_id: 1,
                        },
                    )],
                },
            )
            .await?;
        s.done("archive")?;
    }
    if s.todo("mute") {
        s.side
            .call(
                "updateNotifySettings",
                &tl_fns::account::UpdateNotifySettings {
                    peer: tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
                        peer: s.peer("channel")?,
                    }),
                    settings: tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(
                        tl_types::InputPeerNotifySettings {
                            show_previews: None,
                            silent: None,
                            mute_until: Some(i32::MAX),
                            sound: None,
                            stories_muted: None,
                            stories_hide_sender: None,
                            stories_sound: None,
                        },
                    ),
                },
            )
            .await?;
        s.done("mute")?;
    }
    if s.todo("pin_dialog") {
        s.side
            .call(
                "toggleDialogPin",
                &tl_fns::messages::ToggleDialogPin {
                    pinned: true,
                    peer: tl_enums::InputDialogPeer::InputDialogPeer(tl_types::InputDialogPeer {
                        peer: s.peer("super")?,
                    }),
                },
            )
            .await?;
        s.done("pin_dialog")?;
    }
    save_draft(&s).await?;
    println!("solo fixture complete");
    Ok(())
}

async fn save_draft(s: &Solo) -> Result<(), String> {
    s.side
        .call(
            "saveDraft",
            &tl_fns::messages::SaveDraft {
                no_webpage: false,
                invert_media: false,
                reply_to: None,
                peer: s.peer("basic")?,
                message: "Walk draft: remember to bring the charger".to_owned(),
                entities: None,
                media: None,
                effect: None,
                suggested_post: None,
                rich_message: None,
            },
        )
        .await
        .map(|_| ())
}

pub async fn serve(main: &Path, who: &str, state_path: &Path) -> Result<(), String> {
    let s = open(main, who, state_path).await?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let ready = json!({ "ok": true, "ready": true, "solo": who }).to_string() + "\n";
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
            Ok(cmd) if cmd["op"] == "quit" => break,
            Ok(cmd) => one(&s, &cmd)
                .await
                .unwrap_or_else(|e| json!({ "ok": false, "error": e })),
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

async fn one(s: &Solo, cmd: &Value) -> Result<Value, String> {
    let op = cmd["op"].as_str().unwrap_or("");
    // The runner says "a" for the clients' account and "b" for the other side.
    let who = cmd["who"].as_str().unwrap_or("b");
    if who != "a" && op != "refresh" && op != "state" {
        return Err(
            "needs the second account (none can act as the other side yet, TGB-3158)".into(),
        );
    }
    let chat = cmd["chat"].as_str().unwrap_or("pm");
    match op {
        "refresh" => {
            save_draft(s).await?;
            Ok(json!({ "ok": true }))
        }
        "state" => Ok(json!({ "ok": true, "msgs": s.state["msgs"].clone() })),
        "send" => {
            let id = ops::send_text(
                &s.side,
                &s.peer(chat)?,
                cmd["text"].as_str().unwrap_or("walk tmp"),
                &Out::default(),
            )
            .await?;
            Ok(json!({ "ok": true, "id": id }))
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
            ops::delete(&s.side, &s.peer(chat)?, ids).await?;
            Ok(json!({ "ok": true }))
        }
        "last" => {
            let n = cmd["n"].as_i64().unwrap_or(5).clamp(1, 100) as i32;
            let msgs = ops::history(&s.side, &s.peer(chat)?, 0, n).await?;
            let list: Vec<Value> = msgs
                .iter()
                .map(|m| json!({ "id": m.id, "out": m.out, "text": m.message }))
                .collect();
            Ok(json!({ "ok": true, "messages": list }))
        }
        "cleanup" => {
            let marker = cmd["marker"].as_str().unwrap_or("walk tmp");
            let peer = s.peer(chat)?;
            let msgs = ops::history(&s.side, &peer, 0, 100).await?;
            let any = cmd["any"].as_bool() == Some(true);
            let ids: Vec<i32> = msgs
                .iter()
                .filter(|m| (m.out || any) && m.message.contains(marker))
                .map(|m| m.id)
                .collect();
            let n = ids.len();
            if n > 0 {
                ops::delete(&s.side, &peer, ids).await?;
            }
            Ok(json!({ "ok": true, "deleted": n }))
        }
        "dialog" => {
            let got = s
                .side
                .call(
                    "getPeerDialogs",
                    &tl_fns::messages::GetPeerDialogs {
                        peers: vec![tl_enums::InputDialogPeer::InputDialogPeer(
                            tl_types::InputDialogPeer {
                                peer: s.peer(chat)?,
                            },
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
                }),
                None => json!({ "ok": false, "error": "no dialog" }),
            })
        }
        _ => Err(format!("op {op} is not available in one-account mode")),
    }
}
