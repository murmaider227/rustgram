//! WP-60 conformance tests for:
//! - Rich-message AI compose (`messages.composeRichMessageWithAI`) & translation
//!   (`messages.translateRichMessage`), including `SUMMARY_FLOOD_PREMIUM` / `FLOOD_WAIT` handling
//! - Paid-message fee revenue (`account.getPaidMessagesRevenue`), fee-exception toggling
//!   (`account.toggleNoPaidMessagesException`), and contact requirements
//!   (`users.getRequirementsToContact`)
//! - `tg-iv` `messages.getRichMessage` hydration & round-trip serialization conformance
//! - `tg-tgw` live widget query, public-data polling, edit-data, and incoming DM conformance
//!
//! All tests run strictly in-process against `FakeRpcClient` (zero network traffic, zero real
//! AI/translation services, zero real accounts or payments).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_data::{
    Channel, ChannelFlags, ChannelId as DataChannelId, FullMsgId as DataFullMsgId, Message,
    MessageContent, MessageMedia, MsgId as DataMsgId, PeerId as DataPeerId, PeerUpdateFlags, User,
    UserFlags, UserId as DataUserId, WidgetData,
};
use tg_iv::rich_page::{
    EntityInText, EntityType, InMemoryMediaResolver, RichBlock, RichBlockKind, RichPage,
    RichParseMode, SerializeRichMessageMode, SerializeRichMessageStatus,
    TEXT_DIFF_DELETED_COLOR_INDEX, TEXT_DIFF_INSERTED_COLOR_INDEX, TextWithEntities as IvText,
    rich_page_from_tl_rich_message, serialize_input_rich_message,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tgw::{PublicDataPollResult, WidgetRequestFailure, WidgetRequestOutcome, pack_payload_zlib};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::convert::now_unix_i32;
use crate::rich_message::{fetch_rich_message_live, get_cached_rich_message};
use crate::widgets::{
    record_widget_updates_from_batch, record_widget_updates_from_batch_for_self,
    widget_edit_shared_data_live, widget_query_typed_live, widgets_state_for_session,
};
use tg_app_core::core_types::dto::compose_ai::{
    AiComposeToneRef, ComposeAiBackend, ComposeRichAiRequest, TranslateRichMessageRequest,
    TranslateRichMessageTarget,
};
use tg_app_core::history::backend::HistoryBackend;
use tg_app_core::info::backend::{InfoBackend, RequirementToContact};
use tg_app_core::stand_in::types::{MsgId, PeerId};

#[derive(Clone)]
struct CapturedRpc {
    ctor: u32,
    bytes: Vec<u8>,
}

type QueuedRpcReply = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct Wp60FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    captured: Arc<Mutex<Vec<CapturedRpc>>>,
    responses: Arc<Mutex<VecDeque<QueuedRpcReply>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wp60FakeRpcClient {
    fn new(dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(dc)),
            captured: Arc::new(Mutex::new(Vec::new())),
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

    fn enqueue_err(&self, expected_ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Err(err)));
    }

    fn captured(&self) -> Vec<CapturedRpc> {
        self.captured.lock().expect("lock captured").clone()
    }

    fn clear_captured(&self) {
        self.captured.lock().expect("lock captured").clear();
    }
}

impl RpcClient for Wp60FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.captured
            .lock()
            .expect("lock captured")
            .push(CapturedRpc { ctor, bytes });

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

fn synthetic_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
    tl_types::User {
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
        access_hash: Some(id * 1000 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Fixture".to_owned()),
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

#[allow(clippy::too_many_arguments)]
fn synthetic_message(
    id: i32,
    peer_uid: i64,
    from_uid: i64,
    out: bool,
    date: i32,
    text: &str,
    media: Option<tl_enums::MessageMedia>,
    rich_message: Option<tl_enums::RichMessage>,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out,
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
        id,
        from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
            user_id: from_uid,
        })),
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        guestchat_via_from: None,
        reply_to: None,
        date,
        message: text.to_owned(),
        media,
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
        rich_message,
    })
}

struct Wp60Harness {
    self_uid: i64,
    peer_uid: i64,
    peer_id: PeerId,
    channel_id: PeerId,
    fake: Wp60FakeRpcClient,
    _runtime: Arc<SessionRuntime<crate::ErasedRpcClient>>,
    live: LiveSession,
}

fn make_wp60_harness() -> Wp60Harness {
    let self_uid = 710_001i64;
    let peer_uid = 710_002i64;
    let channel_raw_id = 720_001i64;

    let self_user = synthetic_user(self_uid, "Avery", "avery_self", true);
    let peer_user = synthetic_user(peer_uid, "Quinn", "quinn_peer", false);
    let top_msg = synthetic_message(
        10,
        peer_uid,
        peer_uid,
        false,
        1_750_000_000,
        "Hello",
        None,
        None,
    );

    let dialog = tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: peer_uid }),
        top_message: 10,
        read_inbox_max_id: 10,
        read_outbox_max_id: 10,
        unread_count: 0,
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
    });

    let fake = Wp60FakeRpcClient::new(2);
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(self_user)],
    );
    fake.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 100,
            qts: 0,
            date: 1_750_000_000,
            seq: 1,
            unread_count: 0,
        }),
    );
    fake.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: 1_750_000_001,
            seq: 1,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: vec![dialog],
            messages: vec![top_msg],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(peer_user)],
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetPinnedDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::PeerDialogs::PeerDialogs(tl_types::messages::PeerDialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            state: tl_enums::updates::State::State(tl_types::updates::State {
                pts: 100,
                qts: 0,
                date: 1_750_000_000,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(self_uid),
            crate::ErasedRpcClient::new(fake.clone()),
            SessionRuntimeConfig::default(),
        )
        .expect("start WP-60 test runtime"),
    );
    runtime
        .tokio_handle()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(15), runtime.wait_ready()).await
        })
        .expect("wait_ready timeout")
        .expect("startup ready");

    // Seed a synthetic channel peer for monoforum paid-message revenue tests.
    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        let mut ch = Channel::new(DataChannelId::from_tl(channel_raw_id));
        ch.common.name = "Release Announcements".to_owned();
        ch.access_hash = 0x55AA_1234_9988;
        ch.flags = ChannelFlags::BROADCAST;
        s.upsert_channel(ch, PeerUpdateFlags::empty());

        if let Some(u) = s.user_mut(DataUserId::from_tl(peer_uid)) {
            u.stars_per_message = 25;
            u.flags.insert(UserFlags::HAS_STARS_PER_MESSAGE);
        }
    });

    fake.clear_captured();
    let live = LiveSession::new(Arc::clone(&runtime), 60);
    Wp60Harness {
        self_uid,
        peer_uid,
        peer_id: PeerId::from_user(peer_uid as u64),
        channel_id: PeerId::from_channel(channel_raw_id as u64),
        fake,
        _runtime: runtime,
        live,
    }
}

// ============================================================================
// 1. messages.composeRichMessageWithAI
// ============================================================================

#[test]
fn compose_rich_message_with_ai_round_trip_and_diff() {
    let mut h = make_wp60_harness();

    // Build a multi-block RichPage with bold/italic entities, code, and math.
    let mut para = RichBlock::new(RichBlockKind::Paragraph);
    para.text = tg_iv::rich_page::RichTextNode::from_text(IvText {
        text: "Shipping layer 233 protocol fixes today.".to_owned(),
        entities: vec![
            EntityInText::new(EntityType::Bold, 0, 8),
            EntityInText::new(EntityType::Italic, 9, 9),
        ],
    });
    let source_page = RichPage {
        blocks: vec![
            RichBlock::heading(1, "Release Notes"),
            para,
            RichBlock::code("rust", "fn ok() -> bool { true }"),
            RichBlock::math(r"E = mc^2"),
        ],
        ..RichPage::default()
    };

    // 1. Style + translate + emojify call.
    let styled_tl_rm = tl_enums::RichMessage::RichMessage(tl_types::RichMessage {
        rtl: false,
        part: false,
        blocks: vec![
            tl_enums::PageBlock::Heading1(tl_types::PageBlockHeading1 {
                text: tl_enums::RichText::TextPlain(tl_types::TextPlain {
                    text: "Notas de lanzamiento".to_owned(),
                }),
            }),
            tl_enums::PageBlock::Paragraph(tl_types::PageBlockParagraph {
                text: tl_enums::RichText::TextConcat(tl_types::TextConcat {
                    texts: vec![
                        tl_enums::RichText::TextBold(tl_types::TextBold {
                            text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                                text: "Publicando".to_owned(),
                            })),
                        }),
                        tl_enums::RichText::TextPlain(tl_types::TextPlain {
                            text: " mejoras del protocolo hoy.".to_owned(),
                        }),
                    ],
                }),
            }),
            tl_enums::PageBlock::Math(tl_types::PageBlockMath {
                source: r"E = mc^2".to_owned(),
            }),
        ],
        photos: Vec::new(),
        documents: Vec::new(),
    });
    h.fake.enqueue_ok(
        tl_fns::messages::ComposeRichMessageWithAI::CONSTRUCTOR_ID,
        &tl_enums::messages::ComposedRichMessageWithAI::ComposedRichMessageWithAI(
            tl_types::messages::ComposedRichMessageWithAI {
                result: styled_tl_rm,
            },
        ),
    );

    let res = ComposeAiBackend::compose_rich_message_with_ai(
        &mut h.live,
        ComposeRichAiRequest {
            rich_page: Some(source_page.clone()),
            proofread: false,
            emojify: true,
            translate_to_lang: Some("es".to_owned()),
            tone: Some(AiComposeToneRef::DefaultTone("formal".to_owned())),
        },
    )
    .expect("composeRichMessageWithAI should succeed");
    assert_eq!(res.page.blocks.len(), 3);
    assert_eq!(res.page.blocks[0].kind, RichBlockKind::Heading);
    assert_eq!(res.page.blocks[0].heading_level, 1);
    assert_eq!(res.page.blocks[0].text.text.text, "Notas de lanzamiento");
    assert!(
        res.page.blocks[1]
            .text
            .text
            .entities
            .iter()
            .any(|e| e.kind == EntityType::Bold)
    );
    assert_eq!(res.page.blocks[2].kind, RichBlockKind::Math);
    assert_eq!(res.page, res.display_page);

    // Inspect exact TL wire encoding.
    let captured = h.fake.captured();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0].ctor,
        tl_fns::messages::ComposeRichMessageWithAI::CONSTRUCTOR_ID
    );
    let decoded = tl_fns::messages::ComposeRichMessageWithAI::from_bytes(&captured[0].bytes)
        .expect("decode ComposeRichMessageWithAI");
    assert!(!decoded.proofread);
    assert!(decoded.emojify);
    assert_eq!(decoded.translate_to_lang.as_deref(), Some("es"));
    assert_eq!(
        decoded.tone,
        Some(tl_enums::InputAiComposeTone::Default(
            tl_types::InputAiComposeToneDefault {
                tone: "formal".to_owned(),
            }
        ))
    );
    let Some(tl_enums::InputRichMessage::InputRichMessage(sent_irm)) = decoded.text else {
        panic!("expected InputRichMessage::InputRichMessage on ComposeRichMessageWithAI");
    };
    assert_eq!(sent_irm.blocks.len(), 4);

    // 2. Proofread mode returns both clean `page` and diff-highlighted `display_page`.
    h.fake.clear_captured();
    let diff_tl_rm = tl_enums::RichMessage::RichMessage(tl_types::RichMessage {
        rtl: false,
        part: false,
        blocks: vec![tl_enums::PageBlock::Paragraph(
            tl_types::PageBlockParagraph {
                text: tl_enums::RichText::TextConcat(tl_types::TextConcat {
                    texts: vec![
                        tl_enums::RichText::TextPlain(tl_types::TextPlain {
                            text: "We ".to_owned(),
                        }),
                        tl_enums::RichText::TextDiff(tl_types::TextDiff {
                            text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                                text: "received".to_owned(),
                            })),
                            old_text: Box::new(tl_enums::RichText::TextPlain(
                                tl_types::TextPlain {
                                    text: "recieved".to_owned(),
                                },
                            )),
                        }),
                        tl_enums::RichText::TextPlain(tl_types::TextPlain {
                            text: " the build.".to_owned(),
                        }),
                    ],
                }),
            },
        )],
        photos: Vec::new(),
        documents: Vec::new(),
    });
    h.fake.enqueue_ok(
        tl_fns::messages::ComposeRichMessageWithAI::CONSTRUCTOR_ID,
        &tl_enums::messages::ComposedRichMessageWithAI::ComposedRichMessageWithAI(
            tl_types::messages::ComposedRichMessageWithAI { result: diff_tl_rm },
        ),
    );
    let proofread_res = ComposeAiBackend::compose_rich_message_with_ai(
        &mut h.live,
        ComposeRichAiRequest {
            rich_page: Some(RichPage {
                blocks: vec![RichBlock::paragraph("We recieved the build.")],
                ..RichPage::default()
            }),
            proofread: true,
            emojify: false,
            translate_to_lang: None,
            tone: None,
        },
    )
    .expect("proofread composeRichMessageWithAI should succeed");
    assert_eq!(
        proofread_res.page.blocks[0].text.text.text,
        "We received the build."
    );
    assert_eq!(
        proofread_res.display_page.blocks[0].text.text.text,
        "We recievedreceived the build."
    );
    let diff_ents = &proofread_res.display_page.blocks[0].text.text.entities;
    assert!(diff_ents.iter().any(|e| e.kind == EntityType::StrikeOut));
    assert!(diff_ents.iter().any(|e| e.kind == EntityType::Underline));
    assert!(diff_ents.iter().any(|e| {
        e.kind == EntityType::Colorized && e.data == TEXT_DIFF_DELETED_COLOR_INDEX.to_string()
    }));
    assert!(diff_ents.iter().any(|e| {
        e.kind == EntityType::Colorized && e.data == TEXT_DIFF_INSERTED_COLOR_INDEX.to_string()
    }));

    // 3. Prompt-only generation (`create_ai_box.cpp:626`: `text: None`, `tone: SingleUse`).
    h.fake.clear_captured();
    h.fake.enqueue_ok(
        tl_fns::messages::ComposeRichMessageWithAI::CONSTRUCTOR_ID,
        &tl_enums::messages::ComposedRichMessageWithAI::ComposedRichMessageWithAI(
            tl_types::messages::ComposedRichMessageWithAI {
                result: tl_enums::RichMessage::RichMessage(tl_types::RichMessage {
                    rtl: false,
                    part: false,
                    blocks: vec![tl_enums::PageBlock::Paragraph(
                        tl_types::PageBlockParagraph {
                            text: tl_enums::RichText::TextPlain(tl_types::TextPlain {
                                text: "Generated summary outline.".to_owned(),
                            }),
                        },
                    )],
                    photos: Vec::new(),
                    documents: Vec::new(),
                }),
            },
        ),
    );
    let gen_res = HistoryBackend::compose_rich_message_with_ai(
        &mut h.live,
        ComposeRichAiRequest {
            rich_page: None,
            proofread: false,
            emojify: false,
            translate_to_lang: Some("en".to_owned()),
            tone: Some(AiComposeToneRef::SingleUsePrompt(
                "Outline quarterly milestones".to_owned(),
            )),
        },
    )
    .expect("prompt-only composeRichMessageWithAI succeeds");
    assert_eq!(
        gen_res.page.blocks[0].text.text.text,
        "Generated summary outline."
    );
    let decoded_gen =
        tl_fns::messages::ComposeRichMessageWithAI::from_bytes(&h.fake.captured()[0].bytes)
            .expect("decode prompt-only ComposeRichMessageWithAI");
    assert!(decoded_gen.text.is_none());
    assert_eq!(
        decoded_gen.tone,
        Some(tl_enums::InputAiComposeTone::SingleUse(
            tl_types::InputAiComposeToneSingleUse {
                custom_prompt: "Outline quarterly milestones".to_owned(),
            }
        ))
    );
}

#[test]
fn compose_rich_message_with_ai_surfaces_flood_and_premium_errors() {
    let mut h = make_wp60_harness();
    let sample = RichPage {
        blocks: vec![RichBlock::paragraph("Hello rich AI")],
        ..RichPage::default()
    };

    for (err_name, value) in [
        ("SUMMARY_FLOOD_PREMIUM", None),
        ("AICOMPOSE_FLOOD_PREMIUM", None),
        ("FLOOD_WAIT_42", Some(42)),
        ("INPUT_TEXT_TOO_LONG", None),
    ] {
        h.fake.enqueue_err(
            tl_fns::messages::ComposeRichMessageWithAI::CONSTRUCTOR_ID,
            InvocationError::Rpc(RpcError {
                code: 420,
                name: err_name.to_owned(),
                value,
            }),
        );
        let err = ComposeAiBackend::compose_rich_message_with_ai(
            &mut h.live,
            ComposeRichAiRequest {
                rich_page: Some(sample.clone()),
                proofread: false,
                emojify: false,
                translate_to_lang: None,
                tone: Some(AiComposeToneRef::DefaultTone("concise".to_owned())),
            },
        )
        .expect_err("RPC error must be surfaced");
        assert!(
            err.contains(err_name),
            "expected error containing {err_name}, got: {err}"
        );
    }
}

// ============================================================================
// 2. messages.translateRichMessage
// ============================================================================

#[test]
fn translate_rich_message_by_ids_inline_pages_batching_and_errors() {
    let mut h = make_wp60_harness();

    // 1. Translate by `(peer_id, msg_ids)` with `tone`.
    let make_rm = |s: &str| {
        tl_enums::RichMessage::RichMessage(tl_types::RichMessage {
            rtl: false,
            part: false,
            blocks: vec![tl_enums::PageBlock::Paragraph(
                tl_types::PageBlockParagraph {
                    text: tl_enums::RichText::TextPlain(tl_types::TextPlain { text: s.to_owned() }),
                },
            )],
            photos: Vec::new(),
            documents: Vec::new(),
        })
    };
    h.fake.enqueue_ok(
        tl_fns::messages::TranslateRichMessage::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedRichMessage::TranslatedRichMessage(
            tl_types::messages::TranslatedRichMessage {
                result: vec![make_rm("Bonjour"), make_rm("Merci")],
            },
        ),
    );
    let by_id = ComposeAiBackend::translate_rich_message(
        &mut h.live,
        TranslateRichMessageRequest {
            target: TranslateRichMessageTarget::Messages {
                peer_id: h.peer_id,
                msg_ids: vec![MsgId(101), MsgId(102)],
            },
            to_lang: "fr".to_owned(),
            tone: Some("formal".to_owned()),
        },
    )
    .expect("translateRichMessage by ids succeeds");
    assert_eq!(by_id.pages.len(), 2);
    assert_eq!(by_id.pages[0].blocks[0].text.text.text, "Bonjour");
    assert_eq!(by_id.pages[1].blocks[0].text.text.text, "Merci");

    let decoded_by_id =
        tl_fns::messages::TranslateRichMessage::from_bytes(&h.fake.captured()[0].bytes)
            .expect("decode TranslateRichMessage");
    assert!(decoded_by_id.peer.is_some());
    assert_eq!(decoded_by_id.id, Some(vec![101, 102]));
    assert!(decoded_by_id.text.is_none());
    assert_eq!(decoded_by_id.to_lang, "fr");
    assert_eq!(decoded_by_id.tone.as_deref(), Some("formal"));

    // 2. Batching > 20 message IDs splits into chunks of 20 (`kRequestCountLimit = 20`).
    h.fake.clear_captured();
    let batch1: Vec<_> = (1..=20).map(|i| make_rm(&format!("tr-{i}"))).collect();
    let batch2: Vec<_> = (21..=25).map(|i| make_rm(&format!("tr-{i}"))).collect();
    h.fake.enqueue_ok(
        tl_fns::messages::TranslateRichMessage::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedRichMessage::TranslatedRichMessage(
            tl_types::messages::TranslatedRichMessage { result: batch1 },
        ),
    );
    h.fake.enqueue_ok(
        tl_fns::messages::TranslateRichMessage::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedRichMessage::TranslatedRichMessage(
            tl_types::messages::TranslatedRichMessage { result: batch2 },
        ),
    );
    let ids_25: Vec<MsgId> = (1..=25).map(MsgId).collect();
    let batched = HistoryBackend::translate_rich_message(
        &mut h.live,
        TranslateRichMessageRequest {
            target: TranslateRichMessageTarget::Messages {
                peer_id: h.peer_id,
                msg_ids: ids_25,
            },
            to_lang: "de".to_owned(),
            tone: None,
        },
    )
    .expect("batched translateRichMessage succeeds");
    assert_eq!(batched.pages.len(), 25);
    assert_eq!(h.fake.captured().len(), 2);
    let req1 = tl_fns::messages::TranslateRichMessage::from_bytes(&h.fake.captured()[0].bytes)
        .expect("decode chunk 1");
    let req2 = tl_fns::messages::TranslateRichMessage::from_bytes(&h.fake.captured()[1].bytes)
        .expect("decode chunk 2");
    assert_eq!(req1.id.as_ref().map(Vec::len), Some(20));
    assert_eq!(req2.id.as_ref().map(Vec::len), Some(5));

    // 3. Inline `RichPages` target (`flags.1`: `text`, no `peer`/`id`).
    h.fake.clear_captured();
    h.fake.enqueue_ok(
        tl_fns::messages::TranslateRichMessage::CONSTRUCTOR_ID,
        &tl_enums::messages::TranslatedRichMessage::TranslatedRichMessage(
            tl_types::messages::TranslatedRichMessage {
                result: vec![make_rm("Hallo Welt")],
            },
        ),
    );
    let inline_res = ComposeAiBackend::translate_rich_message(
        &mut h.live,
        TranslateRichMessageRequest {
            target: TranslateRichMessageTarget::RichPages(vec![RichPage {
                blocks: vec![RichBlock::paragraph("Hello World")],
                ..RichPage::default()
            }]),
            to_lang: "de".to_owned(),
            tone: None,
        },
    )
    .expect("inline translateRichMessage succeeds");
    assert_eq!(inline_res.pages.len(), 1);
    assert_eq!(inline_res.pages[0].blocks[0].text.text.text, "Hallo Welt");
    let decoded_inline =
        tl_fns::messages::TranslateRichMessage::from_bytes(&h.fake.captured()[0].bytes)
            .expect("decode inline TranslateRichMessage");
    assert!(decoded_inline.peer.is_none());
    assert!(decoded_inline.id.is_none());
    assert_eq!(decoded_inline.text.as_ref().map(Vec::len), Some(1));

    // 4. Surfaces SUMMARY_FLOOD_PREMIUM and FLOOD_WAIT errors.
    for (err_name, value) in [("SUMMARY_FLOOD_PREMIUM", None), ("FLOOD_WAIT_15", Some(15))] {
        h.fake.enqueue_err(
            tl_fns::messages::TranslateRichMessage::CONSTRUCTOR_ID,
            InvocationError::Rpc(RpcError {
                code: 420,
                name: err_name.to_owned(),
                value,
            }),
        );
        let err = ComposeAiBackend::translate_rich_message(
            &mut h.live,
            TranslateRichMessageRequest {
                target: TranslateRichMessageTarget::Messages {
                    peer_id: h.peer_id,
                    msg_ids: vec![MsgId(101)],
                },
                to_lang: "ja".to_owned(),
                tone: None,
            },
        )
        .expect_err("expected translateRichMessage error");
        assert!(
            err.contains(err_name),
            "expected {err_name} in error, got: {err}"
        );
    }
}

// ============================================================================
// 3. Paid-message fee revenue, exception toggle, and requirements to contact
// ============================================================================

#[test]
fn paid_messages_revenue_and_toggle_exception_dm_and_monoforum() {
    let mut h = make_wp60_harness();

    // 1. 1:1 DM `account.getPaidMessagesRevenue` (`parent_peer = None`).
    h.fake.enqueue_ok(
        tl_fns::account::GetPaidMessagesRevenue::CONSTRUCTOR_ID,
        &tl_enums::account::PaidMessagesRevenue::PaidMessagesRevenue(
            tl_types::account::PaidMessagesRevenue { stars_amount: 250 },
        ),
    );
    let dm_rev = h
        .live
        .get_paid_messages_revenue(None, h.peer_id)
        .expect("1:1 getPaidMessagesRevenue succeeds");
    assert_eq!(dm_rev.stars_amount, 250);
    let dec_dm_rev =
        tl_fns::account::GetPaidMessagesRevenue::from_bytes(&h.fake.captured()[0].bytes)
            .expect("decode GetPaidMessagesRevenue");
    assert!(dec_dm_rev.parent_peer.is_none());
    assert!(matches!(
        dec_dm_rev.user_id,
        tl_enums::InputUser::InputUser(u) if u.user_id == h.peer_uid
    ));

    // 2. Monoforum channel DM `account.getPaidMessagesRevenue` (`parent_peer = Some(channel)`).
    h.fake.clear_captured();
    h.fake.enqueue_ok(
        tl_fns::account::GetPaidMessagesRevenue::CONSTRUCTOR_ID,
        &tl_enums::account::PaidMessagesRevenue::PaidMessagesRevenue(
            tl_types::account::PaidMessagesRevenue { stars_amount: 100 },
        ),
    );
    let ch_rev = h
        .live
        .get_paid_messages_revenue(Some(h.channel_id), h.peer_id)
        .expect("channel monoforum getPaidMessagesRevenue succeeds");
    assert_eq!(ch_rev.stars_amount, 100);
    let dec_ch_rev =
        tl_fns::account::GetPaidMessagesRevenue::from_bytes(&h.fake.captured()[0].bytes)
            .expect("decode channel GetPaidMessagesRevenue");
    assert!(matches!(
        dec_ch_rev.parent_peer,
        Some(tl_enums::InputPeer::Channel(_))
    ));

    // 3. Remove fee & refund charged Stars in 1:1 DM (`refund_charged = true, require_payment = false`).
    h.fake.clear_captured();
    h.fake.enqueue_ok(
        tl_fns::account::ToggleNoPaidMessagesException::CONSTRUCTOR_ID,
        &true,
    );
    let removed = h
        .live
        .toggle_no_paid_messages_exception(None, h.peer_id, true, false)
        .expect("toggleNoPaidMessagesException remove fee succeeds");
    assert!(removed);
    let dec_toggle =
        tl_fns::account::ToggleNoPaidMessagesException::from_bytes(&h.fake.captured()[0].bytes)
            .expect("decode ToggleNoPaidMessagesException");
    assert!(dec_toggle.refund_charged);
    assert!(!dec_toggle.require_payment);
    assert!(dec_toggle.parent_peer.is_none());

    // Confirm cached 1:1 user `stars_per_message` was cleared (`window_peer_menu.cpp:4755`).
    let stars_after = h.live.read_session(|s| {
        let u = s
            .user(DataUserId::from_tl(h.peer_uid))
            .expect("peer user present");
        (
            u.stars_per_message,
            u.flags.contains(UserFlags::HAS_STARS_PER_MESSAGE),
        )
    });
    assert_eq!(stars_after, (0, false));

    // 4. Re-require payment in channel monoforum (`refund_charged = false, require_payment = true`).
    h.fake.clear_captured();
    h.fake.enqueue_ok(
        tl_fns::account::ToggleNoPaidMessagesException::CONSTRUCTOR_ID,
        &true,
    );
    let reenabled = h
        .live
        .toggle_no_paid_messages_exception(Some(h.channel_id), h.peer_id, false, true)
        .expect("toggleNoPaidMessagesException require payment succeeds");
    assert!(reenabled);
    let dec_ch_toggle =
        tl_fns::account::ToggleNoPaidMessagesException::from_bytes(&h.fake.captured()[0].bytes)
            .expect("decode channel ToggleNoPaidMessagesException");
    assert!(!dec_ch_toggle.refund_charged);
    assert!(dec_ch_toggle.require_payment);
    assert!(matches!(
        dec_ch_toggle.parent_peer,
        Some(tl_enums::InputPeer::Channel(_))
    ));
}

#[test]
fn get_requirements_to_contact_variants_batching_and_session_flags() {
    let h = make_wp60_harness();

    // Seed 105 synthetic users in session to exercise the 100-user batch boundary (`kPerRequest = 100`).
    let mut user_peers = Vec::with_capacity(105);
    h.live.write_session(|s| {
        for i in 0..105i64 {
            let uid = 730_000 + i;
            let mut u = User::new(DataUserId::from_tl(uid), false);
            u.first_name = format!("User{i}");
            u.access_hash = (uid * 13 + 5) as u64;
            s.upsert_user(u, PeerUpdateFlags::empty());
            user_peers.push(PeerId::from_user(uid as u64));
        }
    });

    // First batch of 100: [Empty, Premium, PaidMessages(50), ...Empty]
    let mut resp1 = Vec::with_capacity(100);
    resp1.push(tl_enums::RequirementToContact::Empty);
    resp1.push(tl_enums::RequirementToContact::Premium);
    resp1.push(tl_enums::RequirementToContact::PaidMessages(
        tl_types::RequirementToContactPaidMessages { stars_amount: 50 },
    ));
    for _ in 3..100 {
        resp1.push(tl_enums::RequirementToContact::Empty);
    }
    // Second batch of 5: intentionally return only 2 items to verify short-response Empty padding
    // matching `Telegram/SourceFiles/api/api_premium.cpp:457-460`.
    let resp2 = vec![
        tl_enums::RequirementToContact::PaidMessages(tl_types::RequirementToContactPaidMessages {
            stars_amount: 120,
        }),
        tl_enums::RequirementToContact::Premium,
    ];
    h.fake.enqueue_ok(
        tl_fns::users::GetRequirementsToContact::CONSTRUCTOR_ID,
        &resp1,
    );
    h.fake.enqueue_ok(
        tl_fns::users::GetRequirementsToContact::CONSTRUCTOR_ID,
        &resp2,
    );

    let reqs = h
        .live
        .get_requirements_to_contact(&user_peers)
        .expect("getRequirementsToContact succeeds");
    assert_eq!(reqs.len(), 105);
    assert_eq!(reqs[0].requirement, RequirementToContact::Empty);
    assert_eq!(reqs[1].requirement, RequirementToContact::Premium);
    assert_eq!(
        reqs[2].requirement,
        RequirementToContact::PaidMessages { stars_amount: 50 }
    );
    assert_eq!(
        reqs[100].requirement,
        RequirementToContact::PaidMessages { stars_amount: 120 }
    );
    assert_eq!(reqs[101].requirement, RequirementToContact::Premium);
    assert_eq!(reqs[102].requirement, RequirementToContact::Empty);
    assert_eq!(reqs[104].requirement, RequirementToContact::Empty);

    // Verify exact 100 + 5 batching on the wire.
    let captured = h.fake.captured();
    assert_eq!(captured.len(), 2);
    let b1 = tl_fns::users::GetRequirementsToContact::from_bytes(&captured[0].bytes)
        .expect("decode batch 1");
    let b2 = tl_fns::users::GetRequirementsToContact::from_bytes(&captured[1].bytes)
        .expect("decode batch 2");
    assert_eq!(b1.id.len(), 100);
    assert_eq!(b2.id.len(), 5);

    // Verify cached user flags in `Session` match `api_premium.cpp:446-455`.
    h.live.read_session(|s| {
        let u1 = s.user(DataUserId::from_tl(730_001)).expect("u1");
        assert!(u1.flags.contains(UserFlags::REQUIRES_PREMIUM_TO_WRITE));
        assert!(u1.flags.contains(UserFlags::HAS_REQUIRE_PREMIUM_TO_WRITE));
        assert!(
            u1.flags
                .contains(UserFlags::MESSAGE_MONEY_RESTRICTIONS_KNOWN)
        );
        assert_eq!(u1.stars_per_message, 0);

        let u2 = s.user(DataUserId::from_tl(730_002)).expect("u2");
        assert!(!u2.flags.contains(UserFlags::REQUIRES_PREMIUM_TO_WRITE));
        assert!(u2.flags.contains(UserFlags::HAS_STARS_PER_MESSAGE));
        assert!(
            u2.flags
                .contains(UserFlags::MESSAGE_MONEY_RESTRICTIONS_KNOWN)
        );
        assert_eq!(u2.stars_per_message, 50);
    });
}

// ============================================================================
// 4. tg-iv `messages.getRichMessage` Hydration & Serializer Conformance
// ============================================================================

#[test]
fn tg_iv_get_rich_message_hydration_and_round_trip_conformance() {
    let h = make_wp60_harness();

    // RichMessage with heading, formatted paragraph, blockquote, code, math, and an extra user.
    let extra_uid = 799_001i64;
    let extra_user = synthetic_user(extra_uid, "Rowan", "rowan_author", false);
    let rich_blocks = vec![
        tl_enums::PageBlock::Heading2(tl_types::PageBlockHeading2 {
            text: tl_enums::RichText::TextPlain(tl_types::TextPlain {
                text: "Protocol Spec".to_owned(),
            }),
        }),
        tl_enums::PageBlock::Paragraph(tl_types::PageBlockParagraph {
            text: tl_enums::RichText::TextConcat(tl_types::TextConcat {
                texts: vec![
                    tl_enums::RichText::TextPlain(tl_types::TextPlain {
                        text: "See ".to_owned(),
                    }),
                    tl_enums::RichText::TextUrl(tl_types::TextUrl {
                        text: Box::new(tl_enums::RichText::TextPlain(tl_types::TextPlain {
                            text: "reference".to_owned(),
                        })),
                        url: "https://example.org/spec".to_owned(),
                        webpage_id: 0,
                    }),
                ],
            }),
        }),
        tl_enums::PageBlock::Preformatted(tl_types::PageBlockPreformatted {
            text: tl_enums::RichText::TextPlain(tl_types::TextPlain {
                text: "let x = 42;".to_owned(),
            }),
            language: "rust".to_owned(),
        }),
        tl_enums::PageBlock::Math(tl_types::PageBlockMath {
            source: r"\int_0^1 x\,dx = \frac{1}{2}".to_owned(),
        }),
    ];
    let tl_rm = tl_enums::RichMessage::RichMessage(tl_types::RichMessage {
        rtl: false,
        part: false,
        blocks: rich_blocks,
        photos: Vec::new(),
        documents: Vec::new(),
    });
    h.fake.enqueue_ok(
        tl_fns::messages::GetRichMessage::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: vec![synthetic_message(
                321,
                h.peer_uid,
                h.peer_uid,
                false,
                1_750_000_500,
                "",
                None,
                Some(tl_rm),
            )],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(extra_user)],
            topics: Vec::new(),
        }),
    );

    let hydrated = fetch_rich_message_live(&h.live, h.peer_id, MsgId(321))
        .expect("getRichMessage succeeds")
        .expect("payload hydrated");
    assert_eq!(hydrated.page.blocks.len(), 4);
    assert_eq!(hydrated.page.blocks[0].kind, RichBlockKind::Heading);
    assert_eq!(hydrated.page.blocks[0].heading_level, 2);
    assert_eq!(hydrated.page.blocks[2].kind, RichBlockKind::Code);
    assert_eq!(hydrated.page.blocks[2].language, "rust");
    assert_eq!(hydrated.page.blocks[3].kind, RichBlockKind::Math);

    // Verify `users` from `messages.getRichMessage` were applied to `Session`.
    let loaded_extra = h
        .live
        .read_session(|s| s.user(DataUserId::from_tl(extra_uid)).is_some());
    assert!(
        loaded_extra,
        "users returned by messages.getRichMessage must hydrate Session"
    );

    // Verify cached lookup matches hydrated payload and round-trips through serializer.
    let cached = get_cached_rich_message(h.live.runtime().session(), h.peer_id, MsgId(321))
        .expect("cached rich payload");
    assert_eq!(cached.page, hydrated.page);

    let resolver = InMemoryMediaResolver::default();
    let ser =
        serialize_input_rich_message(&cached.page, SerializeRichMessageMode::Draft, &resolver);
    assert_eq!(ser.status, SerializeRichMessageStatus::Success);
    let irm = ser.value.expect("serialized InputRichMessage");
    let round_tripped = rich_page_from_tl_rich_message(
        &tl_types::RichMessage {
            rtl: irm.rtl,
            part: false,
            blocks: irm.blocks,
            photos: Vec::new(),
            documents: Vec::new(),
        },
        RichParseMode::Normal,
    );
    assert_eq!(round_tripped.blocks.len(), 4);
    assert_eq!(round_tripped.blocks[0].text.text.text, "Protocol Spec");
    assert_eq!(
        round_tripped.blocks[3].formula,
        r"\int_0^1 x\,dx = \frac{1}{2}"
    );
}

// ============================================================================
// 5. tg-tgw Live Widget Query, Poll, Edit & Incoming DM Conformance
// ============================================================================

#[test]
fn tg_tgw_live_widget_query_poll_edit_and_incoming_dm_conformance() {
    let mut h = make_wp60_harness();

    // Seed two messages sharing `widget_id = 88_001` in `Session`.
    let html = b"<title>Build Monitor</title><p>Status: green</p>";
    let payload = pack_payload_zlib(html);
    h.live.write_session(|s| {
        let data_peer = DataPeerId::from_user(DataUserId::from_tl(h.peer_uid));
        for mid in [501i64, 502i64] {
            let mut msg = Message::new_text(
                data_peer,
                DataMsgId(mid),
                Some(data_peer),
                now_unix_i32(),
                tg_data::TextWithEntities::plain("Widget"),
            );
            msg.content = MessageContent::Regular {
                text: tg_data::TextWithEntities::plain("Widget"),
                media: MessageMedia::Widget(Box::new(WidgetData {
                    widget_id: Some(88_001),
                    width: 320,
                    height: 180,
                    payload: payload.clone(),
                    is_copy: false,
                })),
            };
            s.insert_message(msg);
        }
    });

    // 1. Typed `messages.getBotWidgetQueryAnswer` with `SignData` & `SentTransaction` + errors.
    h.fake.enqueue_ok(
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID,
        &tl_enums::messages::BotWidgetQueryAnswer::BotWidgetQueryAnswer(
            tl_types::messages::BotWidgetQueryAnswer {
                result: Some(r#"{"ok":true}"#.to_owned()),
            },
        ),
    );
    let q_ok = widget_query_typed_live(
        &h.live,
        h.peer_id,
        MsgId(501),
        tl_enums::WidgetQueryType::Data(tl_types::WidgetQueryTypeData { with_wallet: true }),
        b"cmd=refresh",
    )
    .expect("widget query ok");
    assert_eq!(
        q_ok,
        WidgetRequestOutcome::ok(Some(r#"{"ok":true}"#.to_owned()))
    );

    h.fake.enqueue_err(
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 400,
            name: "WIDGET_RESPONSE_TIMEOUT".to_owned(),
            value: None,
        }),
    );
    assert_eq!(
        h.live
            .widget_query(h.peer_id, MsgId(501), b"slow", false)
            .expect("timeout mapped"),
        WidgetRequestOutcome::err(WidgetRequestFailure::Timeout)
    );

    h.fake.enqueue_err(
        tl_fns::messages::GetBotWidgetQueryAnswer::CONSTRUCTOR_ID,
        InvocationError::Rpc(RpcError {
            code: 420,
            name: "FLOOD_WAIT_10".to_owned(),
            value: Some(10),
        }),
    );
    assert_eq!(
        h.live
            .widget_query(h.peer_id, MsgId(501), b"burst", false)
            .expect("flood mapped"),
        WidgetRequestOutcome::err(WidgetRequestFailure::RateLimited)
    );

    // 2. Seed second carrier into `message_states`, then poll `messages.getWidgetPublicData`
    //    and confirm shared `widget_id = 88_001` updates both messages.
    let incoming_dm_batch = tl_enums::Updates::Combined(tl_types::UpdatesCombined {
        updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: synthetic_message(
                502,
                h.self_uid, // incoming DM where `peer_id == self_uid`, `from_id == peer_uid`
                h.peer_uid,
                false,
                now_unix_i32(),
                "",
                Some(tl_enums::MessageMedia::Widget(
                    tl_types::MessageMediaWidget {
                        is_copy: false,
                        can_query: true,
                        background_query: false,
                        width: 320,
                        height: 180,
                        payload: payload.clone(),
                        public_data: Some(r#"{"v":1}"#.to_owned()),
                        public_data_hash: Some(101),
                        chat_data: None,
                        widget_id: Some(88_001),
                    },
                )),
                None,
            ),
            pts: 101,
            pts_count: 1,
        })],
        users: Vec::new(),
        chats: Vec::new(),
        date: now_unix_i32(),
        seq_start: 1,
        seq: 2,
    });
    assert!(record_widget_updates_from_batch(
        h.live.runtime().session(),
        &incoming_dm_batch
    ));

    // Confirm incoming DM keyed under `h.peer_id` (NOT `self_uid`).
    {
        let st_arc = widgets_state_for_session(h.live.runtime().session());
        let st = st_arc.lock().expect("lock widget state");
        assert!(
            st.message_states.contains_key(&(h.peer_id, MsgId(502))),
            "incoming DM widget must be keyed under sender peer_id, not self_uid"
        );
    }

    h.fake.enqueue_ok(
        tl_fns::messages::GetWidgetPublicData::CONSTRUCTOR_ID,
        &tl_enums::messages::WidgetPublicData::WidgetPublicData(
            tl_types::messages::WidgetPublicData {
                hash: 202,
                data: r#"{"v":2}"#.to_owned(),
                period: 1, // clamped to 5000ms per tgw_state.cpp:24
            },
        ),
    );
    let poll = h
        .live
        .widget_poll_public_data(h.peer_id, MsgId(501), 101)
        .expect("poll public data");
    assert_eq!(
        poll,
        PublicDataPollResult::Updated {
            hash: 202,
            data: r#"{"v":2}"#.to_owned(),
            period_ms: 5_000,
        }
    );

    // Confirm sibling carrier `(h.peer_id, MsgId(502))` also received the shared public_data!
    {
        let st_arc = widgets_state_for_session(h.live.runtime().session());
        let st = st_arc.lock().expect("lock widget state");
        let (ws2, hash2, _) = st
            .message_states
            .get(&(h.peer_id, MsgId(502)))
            .expect("carrier 502 cached");
        assert_eq!(ws2.public_data.as_deref(), Some(br#"{"v":2}"#.as_slice()));
        assert_eq!(*hash2, Some(202));
    }

    // 3. `messages.editWidgetData` updates all carriers sharing `widget_id = 88_001`.
    h.fake
        .enqueue_ok(tl_fns::messages::EditWidgetData::CONSTRUCTOR_ID, &true);
    let shared_ok = widget_edit_shared_data_live(&h.live, 88_001, r#"{"v":3}"#, Some(30))
        .expect("editWidgetData ok");
    assert!(shared_ok);
    {
        let st_arc = widgets_state_for_session(h.live.runtime().session());
        let st = st_arc.lock().expect("lock widget state");
        let (ws1, _, _) = st
            .message_states
            .get(&(h.peer_id, MsgId(501)))
            .expect("carrier 501 cached");
        let (ws2, _, _) = st
            .message_states
            .get(&(h.peer_id, MsgId(502)))
            .expect("carrier 502 cached");
        assert_eq!(ws1.public_data.as_deref(), Some(br#"{"v":3}"#.as_slice()));
        assert_eq!(ws2.public_data.as_deref(), Some(br#"{"v":3}"#.as_slice()));
    }

    // 4. `messages.editMessageWidgetData` with `Updates::Combined` updates per-message chat_data.
    h.fake.enqueue_ok(
        tl_fns::messages::EditMessageWidgetData::CONSTRUCTOR_ID,
        &tl_enums::Updates::Combined(tl_types::UpdatesCombined {
            updates: vec![tl_enums::Update::MessageWidgetData(
                tl_types::UpdateMessageWidgetData {
                    peer: tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: h.peer_uid,
                    }),
                    msg_id: 501,
                    public_data: None,
                    public_data_hash: None,
                    chat_data: Some(r#"{"pinned":true}"#.to_owned()),
                    pts: 102,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq_start: 2,
            seq: 3,
        }),
    );
    h.live
        .widget_edit_data(h.peer_id, MsgId(501), None, Some(r#"{"pinned":true}"#))
        .expect("editMessageWidgetData succeeds");
    let st_arc = widgets_state_for_session(h.live.runtime().session());
    let st = st_arc.lock().expect("lock widget state");
    let (ws1, _, _) = st
        .message_states
        .get(&(h.peer_id, MsgId(501)))
        .expect("carrier 501 cached");
    assert_eq!(
        ws1.chat_data.as_deref(),
        Some(br#"{"pinned":true}"#.as_slice())
    );
    assert!(h.live.read_session(|s| {
        s.message(DataFullMsgId::new(
            DataPeerId::from_user(DataUserId::from_tl(h.peer_uid)),
            DataMsgId(501),
        ))
        .is_some()
    }));
}

fn incoming_widget_dm_batch(msg_id: i32, self_uid: i64, sender_uid: i64) -> tl_enums::Updates {
    tl_enums::Updates::UpdateShort(tl_types::UpdateShort {
        update: tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
            message: synthetic_message(
                msg_id,
                self_uid,
                sender_uid,
                false,
                now_unix_i32(),
                "",
                Some(tl_enums::MessageMedia::Widget(
                    tl_types::MessageMediaWidget {
                        is_copy: false,
                        can_query: true,
                        background_query: false,
                        width: 320,
                        height: 180,
                        payload: pack_payload_zlib(b"<p>poll</p>"),
                        public_data: Some(r#"{"v":1}"#.to_owned()),
                        public_data_hash: Some(7),
                        chat_data: None,
                        widget_id: None,
                    },
                )),
                None,
            ),
            pts: 1,
            pts_count: 1,
        }),
        date: now_unix_i32(),
    })
}

fn widget_keys_for(session: &Arc<tokio::sync::RwLock<tg_data::Session>>) -> Vec<(PeerId, MsgId)> {
    let st_arc = widgets_state_for_session(session);
    let st = st_arc.lock().expect("lock widget state");
    st.message_states.keys().copied().collect()
}

/// A background task holding the session write lock (update pump, dialog loader) must not make
/// an incoming DM's widget land under `self_uid`, i.e. in Saved Messages instead of the sender's
/// chat. CI hit this in `tg_tgw_live_widget_query_poll_edit_and_incoming_dm_conformance`.
#[test]
fn incoming_dm_widget_keyed_under_sender_while_session_is_write_locked() {
    let self_uid = 730_001i64;
    let sender_uid = 730_002i64;
    let session = Arc::new(tokio::sync::RwLock::new(tg_data::Session::new(
        DataUserId::from_tl(self_uid),
    )));
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let writer = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            let guard = session.blocking_write();
            locked_tx.send(()).expect("signal write lock held");
            std::thread::sleep(Duration::from_millis(150));
            drop(guard);
        })
    };
    locked_rx.recv().expect("writer holds the session lock");

    assert!(record_widget_updates_from_batch(
        &session,
        &incoming_widget_dm_batch(9_001, self_uid, sender_uid),
    ));
    writer.join().expect("writer thread");

    assert_eq!(
        widget_keys_for(&session),
        vec![(PeerId::from_user(sender_uid as u64), MsgId(9_001))],
        "incoming DM widget must be keyed under sender peer_id, not self_uid"
    );
}

/// `LiveSession::apply_updates_side_caches` passes the `self_uid` it already read, so recording
/// never waits on the session lock (here the caller itself holds the write guard).
#[test]
fn incoming_dm_widget_with_known_self_uid_never_locks_the_session() {
    let self_uid = 730_011i64;
    let sender_uid = 730_012i64;
    let session = Arc::new(tokio::sync::RwLock::new(tg_data::Session::new(
        DataUserId::from_tl(self_uid),
    )));
    let guard = session.blocking_write();
    assert!(record_widget_updates_from_batch_for_self(
        &session,
        &incoming_widget_dm_batch(9_002, self_uid, sender_uid),
        self_uid,
    ));
    drop(guard);
    assert_eq!(
        widget_keys_for(&session),
        vec![(PeerId::from_user(sender_uid as u64), MsgId(9_002))]
    );
}
