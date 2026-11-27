//! Chat export fixes ported from Telegram Desktop 7.4.6 / 7.4.7 (TGB-3149):
//! `57c122e` (bot-allowed text, monotonic per-file message ids, JSON `button_text`)
//! and `3fc0789` (entity bounds measured in UTF-16).
//!
//! All sample names and payloads are fictional.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use tg_export::controller::{ExportController, ExportState, InMemoryExportSource};
use tg_export::data::{
    ContactInfo, DialogInfo, DialogType, DialogsInfo, Message, MessagesSlice, Peer, ServiceAction,
    TextEntityInput, TextPart, TextPartType, User, finalize_dialogs_info, parse_text_entities,
};
use tg_export::output::OutputFormat;
use tg_export::output::html::MESSAGES_IN_FILE;
use tg_export::settings::{Environment, ExportTypes, Settings};
use tg_export::stand_in::{InputPeer, UserId};
use tg_tl::types::InputPeerUser;

static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(1);

const SELF_ID: u64 = 7001;
const PEER_ID: u64 = 7002;

fn temp_export_dir(label: &str) -> PathBuf {
    let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "tg_export_747_{}_{}_{}",
        label,
        std::process::id(),
        n
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn dir_with_slash(dir: &Path) -> String {
    let mut s = dir.to_string_lossy().replace('\\', "/");
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

fn user(id: u64, first: &str, is_bot: bool, is_self: bool) -> User {
    User {
        bare_id: id,
        info: ContactInfo {
            user_id: UserId(id),
            first_name: first.to_string(),
            date: 1_700_000_000,
            ..Default::default()
        },
        is_bot,
        is_self,
        ..Default::default()
    }
}

/// Exports `messages` as a single-peer chat with a bot and returns the export directory.
async fn export_bot_chat(label: &str, format: OutputFormat, messages: Vec<Message>) -> PathBuf {
    let dir = temp_export_dir(label);
    let me = user(SELF_ID, "Alden", false, true);
    let bot = user(PEER_ID, "Cinder", true, false);
    let self_id = me.id();
    let bot_id = bot.id();
    let mut peers = BTreeMap::new();
    peers.insert(self_id, Peer::User(me));
    peers.insert(bot_id, Peer::User(bot.clone()));

    let settings = Settings {
        path: dir_with_slash(&dir),
        format,
        types: ExportTypes::BOT_CHATS,
        full_chats: ExportTypes::BOT_CHATS,
        single_peer: InputPeer::User(InputPeerUser {
            user_id: PEER_ID as i64,
            access_hash: 1,
        }),
        ..Default::default()
    };
    let mut dialogs = DialogsInfo {
        chats: vec![DialogInfo {
            kind: DialogType::Bot,
            name: bot.info.first_name.clone(),
            peer_id: bot_id,
            ..Default::default()
        }],
        left: Vec::new(),
    };
    finalize_dialogs_info(&mut dialogs, &settings);

    let messages = messages
        .into_iter()
        .map(|m| Message {
            peer_id: bot_id,
            self_id,
            ..m
        })
        .collect();
    let mut dialog_messages = BTreeMap::new();
    dialog_messages.insert(
        bot_id,
        vec![MessagesSlice {
            list: messages,
            peers,
        }],
    );
    let mut source = InMemoryExportSource::default();
    source.dialogs = dialogs;
    source.dialog_messages = dialog_messages;

    let (mut controller, _handle) = ExportController::new(source, settings.single_peer.clone());
    controller.set_skip_path_normalization(true);
    let state = controller.run(&settings, &Environment::default()).await;
    assert!(matches!(state, ExportState::Finished(_)), "got {state:?}");
    dir
}

fn bot_allowed(id: i32, app_id: u64, app: &str, domain: &str) -> Message {
    Message {
        id,
        date: 1_703_981_940 + id,
        action: ServiceAction::BotAllowed {
            app_id,
            app: app.to_string(),
            domain: domain.to_string(),
            attach_menu: false,
            from_request: false,
        },
        ..Default::default()
    }
}

/// tdesktop `57c122e`: the HTML text picks "opened <app>" when the action names an app
/// (`data.appId`), and "logged in on <domain>" otherwise. The old code tested
/// `app.isEmpty()`, the inverse.
#[tokio::test]
async fn bot_allowed_html_names_the_app_or_the_domain() {
    let dir = export_bot_chat(
        "bot_allowed",
        OutputFormat::Html,
        vec![
            bot_allowed(19, 0, "", "example.org"),
            bot_allowed(74, 5074, "Mini <App>", ""),
        ],
    )
    .await;
    let html = fs::read_to_string(dir.join("messages.html")).expect("messages.html");
    let _ = fs::remove_dir_all(&dir);

    assert!(
        html.contains("You allowed this bot to message you when you logged in on example.org"),
        "domain case:\n{html}"
    );
    assert!(
        html.contains("You allowed this bot to message you when you opened Mini &lt;App&gt;"),
        "app case:\n{html}"
    );
    assert!(
        !html.contains("when you opened \n"),
        "empty app name printed"
    );
    assert!(
        !html.contains("when you logged in on \n"),
        "empty domain printed"
    );
}

/// tdesktop `57c122e`: `send_webview_data` writes the button text as `button_text`, leaving
/// the message's own `text` member alone (before, the object had two `text` members).
#[tokio::test]
async fn webview_data_sent_json_uses_button_text_key() {
    let dir = export_bot_chat(
        "webview_json",
        OutputFormat::Json,
        vec![Message {
            id: 31,
            date: 1_703_982_660,
            out: true,
            action: ServiceAction::WebViewDataSent {
                text: "Open <Shop>".to_string(),
            },
            ..Default::default()
        }],
    )
    .await;
    let raw = fs::read_to_string(dir.join("result.json")).expect("result.json");
    let _ = fs::remove_dir_all(&dir);

    let start = raw
        .find("\"action\": \"send_webview_data\"")
        .expect("send_webview_data action in result.json");
    let object_end = raw[start..].find('}').map_or(raw.len(), |end| start + end);
    let object = &raw[start..object_end];
    assert!(
        object.contains("\"button_text\": \"Open <Shop>\""),
        "object:\n{object}"
    );
    assert_eq!(
        object.matches("\"text\": ").count(),
        1,
        "exactly one `text` member (the message text):\n{object}"
    );

    let json: Value = serde_json::from_str(&raw).expect("valid JSON");
    let message = &json["messages"][0];
    assert_eq!(message["button_text"], "Open <Shop>");
    assert_eq!(message["text"], "");
}

/// tdesktop `3fc0789`: entity bounds are checked against the UTF-16 length. An entity that
/// ends past the UTF-16 end but inside the UTF-8 byte length is dropped, not clamped.
#[test]
fn entity_past_utf16_end_is_dropped() {
    // "caf\u{e9}": 4 UTF-16 units, 5 UTF-8 bytes.
    let text = "caf\u{e9}";
    let past_end = TextEntityInput {
        kind: TextPartType::Bold,
        offset: 3,
        length: 2,
        additional: String::new(),
    };
    assert_eq!(
        parse_text_entities(text, &[past_end]),
        vec![TextPart::plain(text)]
    );

    let inside = TextEntityInput {
        kind: TextPartType::Bold,
        offset: 3,
        length: 1,
        additional: String::new(),
    };
    assert_eq!(
        parse_text_entities(text, &[inside]),
        vec![
            TextPart::plain("caf"),
            TextPart {
                kind: TextPartType::Bold,
                text: "\u{e9}".to_string(),
                additional: String::new(),
            },
        ]
    );
}

fn text_message(id: i32, date: i32, reply_to_msg_id: i32) -> Message {
    Message {
        id,
        date,
        from_id: user(PEER_ID, "Cinder", true, false).id(),
        reply_to_msg_id,
        text: vec![TextPart::plain(format!("Msg #{id}"))],
        ..Default::default()
    }
}

/// tdesktop `57c122e`: the per-file "last message id" list stays monotonic and is searched
/// with `lower_bound`. A chat whose ids are not in order (here the first file ends with a
/// large id) must still link a reply to the first file whose running maximum covers it.
#[tokio::test]
async fn reply_links_with_non_monotonic_ids_across_files() {
    let per_file = MESSAGES_IN_FILE as i32;
    let mut messages = Vec::new();
    let mut date = 1_700_000_000;
    // File 1: ids 1..=999, then 50_000 as the last message.
    for id in 1..per_file {
        date += 1;
        messages.push(text_message(id, date, 0));
    }
    date += 1;
    messages.push(text_message(50_000, date, 0));
    // File 2: ids 1001..=2000 (its last id is below file 1's).
    for id in per_file + 1..=2 * per_file {
        date += 1;
        messages.push(text_message(id, date, 0));
    }
    // File 3: replies into file 1, past the running maximum, and into its own file.
    for (id, reply) in [
        (60_001, 1_500),
        (60_002, 50_000),
        (60_003, 60_001),
        (60_004, 70_000),
    ] {
        date += 1;
        messages.push(text_message(id, date, reply));
    }

    let dir = export_bot_chat("links", OutputFormat::Html, messages).await;
    let page3 = fs::read_to_string(dir.join("messages3.html")).expect("messages3.html");
    let _ = fs::remove_dir_all(&dir);

    // Running maxima are [50000, 50000]: both 1500 and 50000 resolve to the first file.
    assert!(
        page3.contains("<a href=\"messages.html#go_to_message1500\">this message</a>"),
        "{page3}"
    );
    assert!(
        page3.contains("<a href=\"messages.html#go_to_message50000\">this message</a>"),
        "{page3}"
    );
    for local in [60_001, 70_000] {
        assert!(
            page3.contains(&format!(
                "<a href=\"#go_to_message{local}\" onclick=\"return GoToMessage({local})\">this message</a>"
            )),
            "same-file link for {local}:\n{page3}"
        );
    }
}
