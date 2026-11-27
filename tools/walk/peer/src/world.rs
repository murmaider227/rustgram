//! Both accounts plus the local state file that records what the fixture created.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::side::Side;

pub struct World {
    pub a: Side,
    pub b: Side,
    pub state: Value,
    pub state_path: PathBuf,
}

/// Users and chats one account can see in its first dialog pages and contacts.
#[derive(Default)]
pub struct Seen {
    pub users: Vec<tl_types::User>,
    pub chats: Vec<tl_enums::Chat>,
    pub dialogs: Vec<tl_types::Dialog>,
}

pub async fn seen(side: &Side, folder: Option<i32>) -> Result<Seen, String> {
    let mut out = Seen::default();
    let got = side
        .call(
            "getDialogs",
            &tl_fns::messages::GetDialogs {
                exclude_pinned: false,
                folder_id: folder,
                offset_date: 0,
                offset_id: 0,
                offset_peer: tl_enums::InputPeer::Empty,
                limit: 100,
                hash: 0,
            },
        )
        .await?;
    let (dialogs, chats, users) = match got {
        tl_enums::messages::Dialogs::Dialogs(d) => (d.dialogs, d.chats, d.users),
        tl_enums::messages::Dialogs::Slice(d) => (d.dialogs, d.chats, d.users),
        tl_enums::messages::Dialogs::NotModified(_) => (Vec::new(), Vec::new(), Vec::new()),
    };
    out.dialogs = dialogs
        .into_iter()
        .filter_map(|d| match d {
            tl_enums::Dialog::Dialog(d) => Some(d),
            _ => None,
        })
        .collect();
    out.chats = chats;
    out.users = users
        .into_iter()
        .filter_map(|u| match u {
            tl_enums::User::User(u) => Some(u),
            tl_enums::User::Empty(_) => None,
        })
        .collect();
    if let Ok(tl_enums::contacts::Contacts::Contacts(c)) = side
        .call("getContacts", &tl_fns::contacts::GetContacts { hash: 0 })
        .await
    {
        for u in c.users {
            if let tl_enums::User::User(u) = u {
                out.users.push(u);
            }
        }
    }
    Ok(out)
}

impl World {
    pub fn load_state(path: &PathBuf) -> Value {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(
                || json!({ "version": 1, "chats": {}, "msgs": {}, "done": {}, "notes": {} }),
            )
    }

    pub fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.state).map_err(|e| e.to_string())?;
        let tmp = self.state_path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("write state: {e}"))?;
        std::fs::rename(&tmp, &self.state_path).map_err(|e| format!("rename state: {e}"))
    }

    pub fn todo(&self, step: &str) -> bool {
        self.state["done"][step].as_bool() != Some(true)
    }

    pub fn done(&mut self, step: &str) -> Result<(), String> {
        self.state["done"][step] = Value::Bool(true);
        eprintln!("[fixture] done: {step}");
        self.save()
    }

    pub fn note(&mut self, key: &str, text: &str) -> Result<(), String> {
        self.state["notes"][key] = Value::String(text.to_owned());
        eprintln!("[fixture] note {key}: {text}");
        self.save()
    }

    pub fn set_msg(&mut self, key: &str, id: i32) -> Result<(), String> {
        self.state["msgs"][key] = json!(id);
        self.save()
    }

    pub fn msg(&self, key: &str) -> Option<i32> {
        self.state["msgs"][key]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
    }

    pub fn side(&self, who: &str) -> Result<&Side, String> {
        match who {
            "a" => Ok(&self.a),
            "b" => Ok(&self.b),
            _ => Err(format!("unknown account {who}")),
        }
    }

    /// Fills the cross-account user access hashes (A sees B, B sees A).
    pub async fn ensure_user_hashes(&mut self) -> Result<(), String> {
        self.state["ids"] = json!({ "a": self.a.self_id, "b": self.b.self_id });
        for _ in 0..2 {
            if self.state["hash"]["a_b"].is_i64() && self.state["hash"]["b_a"].is_i64() {
                return self.save();
            }
            let sa = seen(&self.a, None).await?;
            if let Some(h) = sa
                .users
                .iter()
                .find(|u| u.id == self.b.self_id)
                .and_then(|u| u.access_hash)
            {
                self.state["hash"]["a_b"] = json!(h);
            }
            // A fresh account A (made by `newaccount`) is unknown to B: B adds it by its TEST
            // number, which only the local state file holds.
            if !self.state["hash"]["b_a"].is_i64()
                && let Some(phone) = self.state["a_phone"].as_str().map(str::to_owned)
            {
                let got = self
                    .b
                    .call(
                        "importContacts",
                        &tg_tl::functions::contacts::ImportContacts {
                            contacts: vec![tl_enums::InputContact::InputPhoneContact(
                                tl_types::InputPhoneContact {
                                    client_id: 1,
                                    phone,
                                    first_name: "Walk Peer A".to_owned(),
                                    last_name: String::new(),
                                    note: None,
                                },
                            )],
                        },
                    )
                    .await?;
                let tl_enums::contacts::ImportedContacts::ImportedContacts(imp) = got;
                for u in imp.users {
                    if let tl_enums::User::User(u) = u
                        && u.id == self.a.self_id
                        && let Some(h) = u.access_hash
                    {
                        self.state["hash"]["b_a"] = json!(h);
                    }
                }
            }
            let sb = seen(&self.b, None).await?;
            if let Some(h) = sb
                .users
                .iter()
                .find(|u| u.id == self.a.self_id)
                .and_then(|u| u.access_hash)
            {
                self.state["hash"]["b_a"] = json!(h);
            }
            // One side knows the other: a message from it introduces it to the other side.
            if self.state["hash"]["a_b"].is_i64() && !self.state["hash"]["b_a"].is_i64() {
                let peer = self.peer("a", "pm")?;
                crate::ops::send_text(&self.a, &peer, "Hi! (walk fixture)", &Default::default())
                    .await?;
                tokio::time::sleep(Duration::from_secs(2)).await;
            } else if self.state["hash"]["b_a"].is_i64() && !self.state["hash"]["a_b"].is_i64() {
                let peer = self.peer("b", "pm")?;
                crate::ops::send_text(&self.b, &peer, "Hi! (walk fixture)", &Default::default())
                    .await?;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        if self.state["hash"]["a_b"].is_i64() && self.state["hash"]["b_a"].is_i64() {
            self.save()
        } else {
            Err(
                "accounts A and B do not know each other (no dialog or contact on either side)"
                    .into(),
            )
        }
    }

    /// The input peer of chat `chat` as seen by account `who`.
    pub fn peer(&self, who: &str, chat: &str) -> Result<tl_enums::InputPeer, String> {
        input_peer(&self.state, who, chat)
    }

    pub fn input_user(&self, who_sees: &str, target: &str) -> Result<tl_enums::InputUser, String> {
        let id = self.state["ids"][target].as_i64().ok_or("no user id")?;
        let hash = self.state["hash"][format!("{who_sees}_{target}")]
            .as_i64()
            .ok_or("no user hash")?;
        Ok(tl_enums::InputUser::InputUser(tl_types::InputUser {
            user_id: id,
            access_hash: hash,
        }))
    }

    pub fn input_channel(&self, who: &str, chat: &str) -> Result<tl_enums::InputChannel, String> {
        match self.peer(who, chat)? {
            tl_enums::InputPeer::Channel(c) => Ok(tl_enums::InputChannel::InputChannel(
                tl_types::InputChannel {
                    channel_id: c.channel_id,
                    access_hash: c.access_hash,
                },
            )),
            _ => Err(format!("{chat} is not a channel")),
        }
    }
}

/// The input peer of fixture chat `chat` as account `who` ("a" or "b") sees it, from the state file.
pub fn input_peer(state: &Value, who: &str, chat: &str) -> Result<tl_enums::InputPeer, String> {
    let other = if who == "a" { "b" } else { "a" };
    match chat {
        "saved" => Ok(tl_enums::InputPeer::Self_),
        "pm" => {
            let id = state["ids"][other].as_i64().ok_or("no peer id")?;
            let hash = state["hash"][format!("{who}_{other}")]
                .as_i64()
                .ok_or("no peer access hash")?;
            Ok(tl_enums::InputPeer::User(tl_types::InputPeerUser {
                user_id: id,
                access_hash: hash,
            }))
        }
        key => {
            let c = &state["chats"][key];
            let id = c["id"]
                .as_i64()
                .ok_or(format!("chat {key} not created yet"))?;
            match c["kind"].as_str() {
                Some("chat") => Ok(tl_enums::InputPeer::Chat(tl_types::InputPeerChat {
                    chat_id: id,
                })),
                Some("channel") => {
                    let hash = c[format!("hash_{who}")]
                        .as_i64()
                        .ok_or(format!("chat {key}: no access hash for {who}"))?;
                    Ok(tl_enums::InputPeer::Channel(tl_types::InputPeerChannel {
                        channel_id: id,
                        access_hash: hash,
                    }))
                }
                _ => Err(format!("chat {key}: unknown kind")),
            }
        }
    }
}
