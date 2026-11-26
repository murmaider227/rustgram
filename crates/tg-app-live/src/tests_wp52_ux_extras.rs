//! Live `FakeRpcClient` tests for WP-52 UX Extras:
//! - Dialog quick actions (`account.updateNotifySettings`, `messages.toggleDialogPin`,
//!   `messages.readHistory`, `messages.markDialogUnread`, `folders.editPeerFolders`)
//! - History double-click quick react (`messages.sendReaction`)
//! - Settings FAQ suggestions (`messages.getWebPage` TOC parsing)
//! - Bot Info Extras (`bots.getBotInfo`, `messages.translateText`, `bots.setBotInfo` with `lang_code`)
//! - Music Attach Box (`users.getSavedMusic`, `account.saveMusic`)
//!
//! Runs exclusively against an in-process `FakeRpcClient` with synthetic IDs/users.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::convert::apply_tl_users_and_chats;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::UserId as DataUserId;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use tg_app_core::core_types::dto::managed_bot::BotInfoChange;
use tg_app_core::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::info::backend::InfoBackend;
use tg_app_core::settings::backend::SettingsBackend;
use tg_app_core::stand_in::types::{FilterId, MsgId, PeerId};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type RecordedCalls = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: RecordedCalls,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
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

    fn recorded_calls(&self) -> Vec<(u32, Vec<u8>)> {
        self.recorded.lock().expect("lock recorded").clone()
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, bytes));
        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };
        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
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

fn synthetic_user(
    id: i64,
    first: &str,
    username: &str,
    is_bot: bool,
    is_self: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self && !is_bot,
        mutual_contact: false,
        deleted: false,
        bot: is_bot,
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
        bot_can_edit: is_bot,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: is_bot,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 17 + 7),
        first_name: Some(first.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: Some("en".to_owned()),
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

fn synthetic_dialog(peer_uid: i64, top_message: i32, unread_count: i32) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        top_message,
        read_inbox_max_id: top_message - unread_count,
        read_outbox_max_id: top_message,
        unread_count,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
            tl_types::PeerNotifySettings {
                show_previews: Some(true),
                silent: Some(false),
                mute_until: Some(0),
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
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    })
}

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: 1_795_348_800,
        seq: 1,
    })
}

fn build_live_session() -> (
    Arc<SessionRuntime<crate::ErasedRpcClient>>,
    FakeRpcClient,
    LiveSession,
) {
    let rpc = FakeRpcClient::new(2);
    let self_user = synthetic_user(100, "DemoSelf", "demo_self", false, true);
    let peer_user = synthetic_user(201, "AlphaPeer", "alpha_peer", false, false);
    let bot_user = synthetic_user(5005, "HelperBot", "helper_demo_bot", true, false);

    rpc.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user.clone())],
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_795_348_800,
            seq: 1,
            unread_count: 0,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: 1_795_348_801,
            seq: 1,
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: vec![synthetic_dialog(201, 50, 3)],
            messages: Vec::new(),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(peer_user.clone())],
        }),
    );
    rpc.enqueue_ok(
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_795_348_801,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(100),
            crate::ErasedRpcClient::new(rpc.clone()),
            SessionRuntimeConfig::default(),
        )
        .expect("start SessionRuntime"),
    );
    runtime
        .tokio_handle()
        .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
        .expect("wait_ready timeout")
        .expect("startup ready");

    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        apply_tl_users_and_chats(
            &mut s,
            &[
                tl_enums::User::User(self_user),
                tl_enums::User::User(peer_user),
                tl_enums::User::User(bot_user),
            ],
            &[],
        );
    });
    let live = LiveSession::new(Arc::clone(&runtime), 52);
    (runtime, rpc, live)
}

#[test]
fn wp52_dialog_quick_actions_dispatch_expected_tl_rpcs() {
    let (_rt, rpc, mut live) = build_live_session();
    let peer = PeerId::from_user(201);
    let baseline = rpc.recorded_calls().len();

    // 1. Mute -> account.updateNotifySettings
    rpc.enqueue_ok(tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID, &true);
    assert!(live.toggle_mute_dialog(peer));

    // 2. Pin -> messages.toggleDialogPin
    rpc.enqueue_ok(tl_fns::messages::ToggleDialogPin::CONSTRUCTOR_ID, &true);
    assert!(live.toggle_pin(peer, DialogListQuery::Filter(FilterId::ALL)));

    // 3. Mark read -> messages.readHistory
    rpc.enqueue_ok(
        tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
        &tl_enums::messages::AffectedMessages::AffectedMessages(
            tl_types::messages::AffectedMessages {
                pts: 10,
                pts_count: 1,
            },
        ),
    );
    live.mark_dialog_read(peer);
    // `messages.readHistory` goes out from the background queue.
    assert!(live.flush_background_rpcs(std::time::Duration::from_secs(10)));

    // 4. Mark unread -> messages.markDialogUnread
    rpc.enqueue_ok(tl_fns::messages::MarkDialogUnread::CONSTRUCTOR_ID, &true);
    live.mark_dialog_unread(peer);

    // 5. Archive -> folders.editPeerFolders
    rpc.enqueue_ok(
        tl_fns::folders::EditPeerFolders::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    assert!(live.toggle_archive(peer));

    let ctors: Vec<u32> = rpc
        .recorded_calls()
        .into_iter()
        .skip(baseline)
        .map(|(c, _)| c)
        .collect();
    assert_eq!(
        ctors,
        vec![
            tl_fns::account::UpdateNotifySettings::CONSTRUCTOR_ID,
            tl_fns::messages::ToggleDialogPin::CONSTRUCTOR_ID,
            tl_fns::messages::ReadHistory::CONSTRUCTOR_ID,
            tl_fns::messages::MarkDialogUnread::CONSTRUCTOR_ID,
            tl_fns::folders::EditPeerFolders::CONSTRUCTOR_ID,
        ]
    );
}

#[test]
fn wp52_history_quick_react_sends_messages_send_reaction() {
    let (_rt, rpc, mut live) = build_live_session();
    let peer = PeerId::from_user(201);

    rpc.enqueue_ok(
        tl_fns::messages::SendReaction::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    let _ = live.toggle_reaction(peer, MsgId(50), "❤️");

    let calls = rpc.recorded_calls();
    assert!(
        calls
            .iter()
            .any(|(c, _)| *c == tl_fns::messages::SendReaction::CONSTRUCTOR_ID)
    );
}

#[test]
fn wp52_faq_suggestions_parses_cached_webpage_toc() {
    let (_rt, rpc, live) = build_live_session();
    let base_url = "https://telegram.org/faq";
    let page = tl_types::Page {
        part: false,
        rtl: false,
        v2: true,
        url: base_url.to_owned(),
        blocks: vec![
            tl_enums::PageBlock::Paragraph(tl_types::PageBlockParagraph {
                text: tl_enums::RichText::TextBold(tl_types::TextBold {
                    text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                        text: "Privacy and Security".to_owned(),
                    })),
                }),
            }),
            tl_enums::PageBlock::List(tl_types::PageBlockList {
                items: vec![
                    tl_enums::PageListItem::Text(tl_types::PageListItemText {
                        checkbox: false,
                        checked: false,
                        text: tl_enums::RichText::TextUrl(tl_types::TextUrl {
                            text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                                text: "How does Two-Step Verification work?".to_owned(),
                            })),
                            url: format!("{base_url}#q-two-step"),
                            webpage_id: 0,
                        }),
                    }),
                    tl_enums::PageListItem::Text(tl_types::PageListItemText {
                        checkbox: false,
                        checked: false,
                        text: tl_enums::RichText::TextUrl(tl_types::TextUrl {
                            text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                                text: "Who can see my phone number?".to_owned(),
                            })),
                            url: format!("{base_url}#q-phone"),
                            webpage_id: 0,
                        }),
                    }),
                ],
            }),
            tl_enums::PageBlock::Divider,
        ],
        photos: Vec::new(),
        documents: Vec::new(),
        views: None,
    };
    let wp = tl_types::WebPage {
        has_large_media: false,
        video_cover_photo: false,
        id: 777,
        url: base_url.to_owned(),
        display_url: base_url.to_owned(),
        hash: 0,
        r#type: Some("article".to_owned()),
        site_name: Some("Telegram".to_owned()),
        title: Some("Telegram FAQ".to_owned()),
        description: None,
        photo: None,
        embed_url: None,
        embed_type: None,
        embed_width: None,
        embed_height: None,
        duration: None,
        author: None,
        document: None,
        cached_page: Some(tl_enums::Page::Page(page)),
        attributes: None,
    };
    rpc.enqueue_ok(
        tl_fns::messages::GetWebPage::CONSTRUCTOR_ID,
        &tl_enums::messages::WebPage::WebPage(tl_types::messages::WebPage {
            webpage: tl_enums::WebPage::WebPage(wp),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let entries = live
        .fetch_faq_suggestions("https://telegram.org/faq#general-questions")
        .expect("fetch_faq_suggestions");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].section, "Privacy and Security");
    assert_eq!(entries[0].title, "How does Two-Step Verification work?");
    assert_eq!(entries[0].url, "https://telegram.org/faq#q-two-step");

    let calls = rpc.recorded_calls();
    let (_, req_bytes) = calls
        .iter()
        .find(|(c, _)| *c == tl_fns::messages::GetWebPage::CONSTRUCTOR_ID)
        .expect("messages.getWebPage recorded");
    let decoded = tl_fns::messages::GetWebPage::from_bytes(req_bytes).expect("decode GetWebPage");
    assert_eq!(decoded.url, "https://telegram.org/faq");
}

#[test]
fn wp52_bot_info_translate_and_music_attach_rpcs() {
    let (_rt, rpc, mut live) = build_live_session();
    let bot_id = PeerId::from_user(5005);
    let self_id = PeerId::from_user(100);

    // 1. bots.getBotInfo + messages.translateText + bots.setBotInfo with lang_code
    rpc.enqueue_ok(
        tl_fns::bots::GetBotInfo::CONSTRUCTOR_ID,
        &tl_enums::bots::BotInfo::BotInfo(tl_types::bots::BotInfo {
            name: "Helper Bot".to_owned(),
            about: "Short bio".to_owned(),
            description: "Explains bot commands.".to_owned(),
        }),
    );
    let info = live.get_bot_info(bot_id, "").expect("getBotInfo");
    assert_eq!(info.description, "Explains bot commands.");

    rpc.enqueue_ok(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedText::TranslateResult(tl_types::messages::TranslateResult {
            result: vec![
                tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                    text: "Bot Ayudante".to_owned(),
                    entities: Vec::new(),
                }),
                tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                    text: "Bio corta".to_owned(),
                    entities: Vec::new(),
                }),
                tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                    text: "Explica comandos.".to_owned(),
                    entities: Vec::new(),
                }),
            ],
        }),
    );
    let tr = live
        .translate_bot_info_texts(&[info.name, info.about, info.description], "es")
        .expect("translate_bot_info_texts");
    assert_eq!(tr[0], "Bot Ayudante");

    rpc.enqueue_ok(tl_fns::bots::SetBotInfo::CONSTRUCTOR_ID, &true);
    live.set_bot_info(
        bot_id,
        "es",
        &BotInfoChange {
            name: Some(tr[0].clone()),
            about: Some(tr[1].clone()),
            description: Some(tr[2].clone()),
        },
    )
    .expect("setBotInfo");

    // 2. users.getSavedMusic + account.saveMusic
    let audio_doc = tl_enums::Document::Document(tl_types::Document {
        id: 88001,
        access_hash: 99001,
        file_reference: vec![1, 2, 3],
        date: 1_740_000_000,
        mime_type: "audio/mpeg".to_owned(),
        size: 4_200_000,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: vec![tl_enums::DocumentAttribute::Audio(
            tl_types::DocumentAttributeAudio {
                voice: false,
                duration: 215,
                title: Some("Midnight Signal".to_owned()),
                performer: Some("Synth Lab".to_owned()),
                waveform: None,
            },
        )],
    });
    rpc.enqueue_ok(
        tl_fns::users::GetSavedMusic::CONSTRUCTOR_ID,
        &tl_enums::users::SavedMusic::SavedMusic(tl_types::users::SavedMusic {
            count: 1,
            documents: vec![audio_doc],
        }),
    );
    let tracks = live
        .peer_saved_music(self_id, 0, 50)
        .expect("getSavedMusic");
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Midnight Signal");
    assert_eq!(tracks[0].performer, "Synth Lab");

    rpc.enqueue_ok(tl_fns::account::SaveMusic::CONSTRUCTOR_ID, &true);
    live.save_profile_music(&tracks[0], false)
        .expect("save_profile_music");

    // Verify bots.setBotInfo carried exact lang_code = "es".
    let calls = rpc.recorded_calls();
    let (_, set_bytes) = calls
        .iter()
        .find(|(c, _)| *c == tl_fns::bots::SetBotInfo::CONSTRUCTOR_ID)
        .expect("bots.setBotInfo recorded");
    let decoded_set = tl_fns::bots::SetBotInfo::from_bytes(set_bytes).expect("decode SetBotInfo");
    assert_eq!(decoded_set.lang_code, "es");
    assert_eq!(decoded_set.name.as_deref(), Some("Bot Ayudante"));
}
