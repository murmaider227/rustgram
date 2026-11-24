//! WP-33 integration tests:
//! - Live cloud theme save from `ThemeEditorState` (`upload.saveFilePart` + `account.uploadTheme`
//!   + `account.createTheme` / `account.updateTheme`);
//! - `ChooseChatThemeState` hydration from live `account.getChatThemes` + hash caching;
//! - Live `messages.getAvailableEffects` catalog, `upload.getFile` animation fetch, `messages.sendMessage`
//!   with `effect`, and `tg-lottie` playback on send/receive (without `"placeholder"` UI labels);
//! - `StoryModalOutcome::ToggleMockPremium` hidden/disabled in live mode (`StealthModeBox`);
//! - Business Premium Subscribe CTA navigating to `PaymentsScreen::Premium` (read-only preview,
//!   never auto-purchasing);
//! - Renamed `PaymentsBackend::{topup_stars, send_star_gift, apply_received_gift_action}` live RPCs.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_payments::credits::CreditTopupOption;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_stories::{StealthMode, StoriesStore, StoryViews};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::theme::ThemeManager;
use tokio::sync::broadcast;

use tg_app::business::{BusinessScreen, BusinessScreenHost};
use tg_app::history::backend::{HistoryBackend, HistoryMessage, SendMessageExtended};
use tg_app::history::compose::ComposeControls;
use tg_app::history::compose_extras::MessageEffectsPickerState;
use tg_app::history::take_new_effect_messages;
use tg_app::history::theme::HistoryColors;
use tg_app::live::session::LiveSession;
use tg_app::payments::backend::{PaymentsBackend, ReceivedGiftAction};
use tg_app::payments::{AppPaymentsHost, PaymentsScreen};
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::stories::backend::StoriesBackend;
use tg_app::stories::boxes::{StoryModalBox, StoryModalOutcome, show_story_modal};
use tg_app::stories::mock::MockStoriesBackend;
use tg_app::themes::{ChatBackground, ChooseChatThemeState, ThemeEditorState, ThemesStudio};

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);
type CapturedRequests = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

#[derive(Clone)]
struct RecordingFakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    captured_requests: CapturedRequests,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl RecordingFakeRpc {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            captured_requests: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }

    fn captured_for(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.captured_requests
            .lock()
            .expect("lock captured")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }

    fn invoked_count(&self, ctor: u32) -> usize {
        self.captured_for(ctor).len()
    }
}

impl RpcClient for RecordingFakeRpc {
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
        self.captured_requests
            .lock()
            .expect("lock captured")
            .push((ctor, bytes));

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
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
    first_name: &str,
    username: &str,
    is_self: bool,
    premium: bool,
) -> tl_types::User {
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
        premium,
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
        access_hash: Some(id * 17 + 99),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: (!username.is_empty()).then(|| username.to_owned()),
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
    }
}

struct TestLiveHarness {
    _rt: tokio::runtime::Runtime,
    rpc: RecordingFakeRpc,
    live: LiveSession,
    peer_id: PeerId,
}

fn build_live_harness(self_premium: bool) -> TestLiveHarness {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let rpc = RecordingFakeRpc::new(2);
    let runtime = Arc::new(
        SessionRuntime::start_on(
            DataUserId::from_tl(10_001),
            tg_app::live::ErasedRpcClient::new(rpc.clone()),
            rt.handle().clone(),
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

    let self_user = synthetic_user(10_001, "Casey", "casey_demo", true, self_premium);
    let peer_user = synthetic_user(20_002, "Jordan", "jordan_demo", false, false);
    rt.block_on(async {
        let mut session = runtime.session().write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut session,
            &[
                tl_enums::User::User(self_user),
                tl_enums::User::User(peer_user),
            ],
            &[],
        );
    });

    let live = LiveSession::new(runtime, 1);
    TestLiveHarness {
        _rt: rt,
        rpc,
        live,
        peer_id: PeerId::from_user(20_002),
    }
}

fn sample_tl_document(id: i64, mime_type: &str, size: i64) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: id * 13 + 7,
        file_reference: vec![1, 2, 3, 4],
        date: now_unix_i32(),
        mime_type: mime_type.to_owned(),
        size,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: Vec::new(),
    })
}

fn sample_tl_theme(id: i64, slug: &str, title: &str, emoticon: Option<&str>) -> tl_enums::Theme {
    tl_enums::Theme::Theme(tl_types::Theme {
        creator: true,
        default: false,
        for_chat: emoticon.is_some(),
        id,
        access_hash: id * 31 + 5,
        slug: slug.to_owned(),
        title: title.to_owned(),
        document: Some(sample_tl_document(
            9000 + id,
            "application/x-tgtheme-tdesktop",
            512,
        )),
        settings: Some(vec![tl_enums::ThemeSettings::ThemeSettings(
            tl_types::ThemeSettings {
                message_colors_animated: false,
                base_theme: tl_enums::BaseTheme::Classic,
                accent_color: 0x0040_A7E3,
                outbox_accent_color: Some(0x002B_78E4),
                message_colors: Some(vec![0x0033_99CC]),
                wallpaper: None,
            },
        )]),
        emoticon: emoticon.map(str::to_owned),
        installs_count: Some(12),
    })
}

#[test]
fn theme_editor_cloud_save_invokes_upload_create_and_update_theme() {
    let h = build_live_harness(false);
    let backend = h.live.themes_backend();

    // Enqueue upload + create responses.
    h.rpc
        .enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    h.rpc.enqueue_ok(
        tl_fns::account::UploadTheme::CONSTRUCTOR_ID,
        &sample_tl_document(501, "application/x-tgtheme-tdesktop", 1024),
    );
    h.rpc.enqueue_ok(
        tl_fns::account::CreateTheme::CONSTRUCTOR_ID,
        &sample_tl_theme(7001, "aurora_theme", "Aurora Live", None),
    );

    let mut mgr = ThemeManager::new();
    let mut bg = ChatBackground::new();
    let mut editor = ThemeEditorState::from_theme_manager(&mgr);
    editor.open_cloud_save_dialog(&mgr, 42);
    editor.cloud_save.title = "Aurora Live".to_owned();
    editor.cloud_save.slug = "aurora_theme".to_owned();

    // Click "Save Cloud Theme" inside `ThemeEditorState::show_with_backend` via egui_kittest.
    let saved_cell = Arc::new(Mutex::new(None));
    let saved_out = Arc::clone(&saved_cell);
    let be_for_ui = Arc::clone(&backend);
    let mut harness = Harness::builder()
        .with_size(Vec2::new(620.0, 640.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui(move |ui| {
            let out = editor.show_with_backend(ui, &mut mgr, &mut bg, Some(be_for_ui.as_ref()));
            if let Some(rec) = out.saved_cloud_theme {
                *saved_out.lock().expect("lock saved") = Some(rec);
            }
        });
    harness.run();
    harness
        .get_by_role_and_label(egui::Role::Button, "Save Cloud Theme")
        .click();
    harness.run();

    let created = saved_cell
        .lock()
        .expect("lock saved")
        .clone()
        .expect("cloud theme created via UI button");
    assert_eq!(created.theme.id, 7001);
    assert_eq!(created.theme.slug, "aurora_theme");
    assert_eq!(
        h.rpc
            .invoked_count(tl_fns::account::UploadTheme::CONSTRUCTOR_ID),
        1
    );
    assert_eq!(
        h.rpc
            .invoked_count(tl_fns::account::CreateTheme::CONSTRUCTOR_ID),
        1
    );

    // Decode exact `account.createTheme` TL request.
    let raw_create = &h
        .rpc
        .captured_for(tl_fns::account::CreateTheme::CONSTRUCTOR_ID)[0];
    let decoded = tl_fns::account::CreateTheme::from_bytes(raw_create).expect("decode CreateTheme");
    assert_eq!(decoded.slug, "aurora_theme");
    assert_eq!(decoded.title, "Aurora Live");
    assert!(decoded.document.is_some());

    // Now test updating an existing cloud theme via `ThemeEditorState::submit_cloud_save`.
    h.rpc
        .enqueue_ok(tl_fns::upload::SaveFilePart::CONSTRUCTOR_ID, &true);
    h.rpc.enqueue_ok(
        tl_fns::account::UploadTheme::CONSTRUCTOR_ID,
        &sample_tl_document(502, "application/x-tgtheme-tdesktop", 1024),
    );
    h.rpc.enqueue_ok(
        tl_fns::account::UpdateTheme::CONSTRUCTOR_ID,
        &sample_tl_theme(7001, "aurora_theme", "Aurora Live v2", None),
    );

    let mgr2 = ThemeManager::new();
    let mut bg2 = ChatBackground::new();
    let mut editor2 = ThemeEditorState::from_theme_manager(&mgr2);
    editor2.set_cloud(created.theme.clone());
    editor2.open_cloud_save_dialog(&mgr2, 42);
    editor2.cloud_save.title = "Aurora Live v2".to_owned();
    editor2.cloud_save.slug = "aurora_theme".to_owned();
    let updated = editor2
        .submit_cloud_save(&mgr2, &mut bg2, backend.as_ref())
        .expect("update cloud theme");
    assert_eq!(updated.theme.title, "Aurora Live v2");
    assert_eq!(
        h.rpc
            .invoked_count(tl_fns::account::UpdateTheme::CONSTRUCTOR_ID),
        1
    );
}

#[test]
fn chat_theme_picker_hydrates_from_live_get_chat_themes() {
    let h = build_live_harness(false);
    let backend = h.live.themes_backend();

    let server_themes = tl_enums::account::Themes::Themes(tl_types::account::Themes {
        hash: 77_123,
        themes: vec![
            sample_tl_theme(101, "neon_wave", "Neon Wave", Some("🌊")),
            sample_tl_theme(102, "amber_dusk", "Amber Dusk", Some("🌅")),
        ],
    });
    h.rpc.enqueue_ok(
        tl_fns::account::GetChatThemes::CONSTRUCTOR_ID,
        &server_themes,
    );

    let mut picker = ChooseChatThemeState::new(h.peer_id.0, "Jordan");
    assert!(!picker.cloud_synced);
    assert_eq!(picker.themes().len(), 7); // offline fallback presets before hydration

    let count = picker
        .hydrate_from_backend(backend.as_ref())
        .expect("hydrate from account.getChatThemes");
    assert_eq!(count, 2);
    assert!(picker.cloud_synced);
    assert_eq!(picker.cloud_hash(), 77_123);
    assert_eq!(picker.themes()[0].token, "🌊");
    assert_eq!(picker.themes()[0].name, "Neon Wave");
    assert_eq!(picker.themes()[1].token, "🌅");

    // Second hydration passes cached hash and handles `ThemesNotModified`.
    h.rpc.enqueue_ok(
        tl_fns::account::GetChatThemes::CONSTRUCTOR_ID,
        &tl_enums::account::Themes::NotModified,
    );
    let count2 = picker
        .hydrate_from_backend(backend.as_ref())
        .expect("not modified refresh");
    assert_eq!(count2, 2);
    let captured = h
        .rpc
        .captured_for(tl_fns::account::GetChatThemes::CONSTRUCTOR_ID);
    assert_eq!(captured.len(), 2);
    let second_req =
        tl_fns::account::GetChatThemes::from_bytes(&captured[1]).expect("decode GetChatThemes");
    assert_eq!(second_req.hash, 77_123);

    // Also verify `ThemesStudio::refresh_cloud_catalogs` hydrates `chat_theme_picker` from live session.
    h.rpc.enqueue_ok(
        tl_fns::account::GetThemes::CONSTRUCTOR_ID,
        &tl_enums::account::Themes::NotModified,
    );
    h.rpc.enqueue_ok(
        tl_fns::account::GetChatThemes::CONSTRUCTOR_ID,
        &server_themes,
    );
    h.rpc.enqueue_ok(
        tl_fns::account::GetWallPapers::CONSTRUCTOR_ID,
        &tl_enums::account::WallPapers::NotModified,
    );
    let mut studio = ThemesStudio::new();
    studio.set_backend(backend);
    assert!(!studio.chat_theme_picker.cloud_synced);
    studio
        .refresh_cloud_catalogs()
        .expect("refresh cloud catalogs");
    assert!(studio.chat_theme_picker.cloud_synced);
    assert_eq!(studio.chat_theme_picker.themes().len(), 2);
    assert_eq!(studio.chat_theme_picker.themes()[0].name, "Neon Wave");
}

#[test]
fn live_available_effects_catalog_send_and_lottie_playback() {
    let mut h = build_live_harness(true);

    let live_lottie_json = r#"{"v":"5.5.2","fr":30,"ip":0,"op":30,"w":64,"h":64,"layers":[]}"#;
    let effects_resp = tl_enums::messages::AvailableEffects::AvailableEffects(
        tl_types::messages::AvailableEffects {
            hash: 99_441,
            effects: vec![
                tl_enums::AvailableEffect::AvailableEffect(tl_types::AvailableEffect {
                    premium_required: false,
                    id: 5104841245755180586,
                    emoticon: "🔥".to_owned(),
                    static_icon_id: None,
                    effect_sticker_id: 8001,
                    effect_animation_id: Some(8002),
                }),
                tl_enums::AvailableEffect::AvailableEffect(tl_types::AvailableEffect {
                    premium_required: true,
                    id: 99001122334455,
                    emoticon: "🎆".to_owned(),
                    static_icon_id: None,
                    effect_sticker_id: 8003,
                    effect_animation_id: Some(8004),
                }),
            ],
            documents: vec![
                sample_tl_document(
                    8002,
                    "application/x-tgsticker",
                    live_lottie_json.len() as i64,
                ),
                sample_tl_document(
                    8004,
                    "application/x-tgsticker",
                    live_lottie_json.len() as i64,
                ),
            ],
        },
    );

    h.rpc.enqueue_ok(
        tl_fns::messages::GetAvailableEffects::CONSTRUCTOR_ID,
        &effects_resp,
    );

    let catalog = h.live.available_effects();
    assert_eq!(catalog.len(), 2);
    assert_eq!(catalog[0].emoji, "🔥");
    assert!(!catalog[0].premium_required);
    assert_eq!(catalog[1].id, 99001122334455);
    assert_eq!(catalog[1].emoji, "🎆");
    assert!(catalog[1].premium_required);

    // Subsequent refresh sends cached hash and accepts NotModified.
    h.rpc.enqueue_ok(
        tl_fns::messages::GetAvailableEffects::CONSTRUCTOR_ID,
        &tl_enums::messages::AvailableEffects::NotModified,
    );
    let cached = h
        .live
        .refresh_available_effects()
        .expect("refresh NotModified");
    assert_eq!(cached.len(), 2);
    let reqs = h
        .rpc
        .captured_for(tl_fns::messages::GetAvailableEffects::CONSTRUCTOR_ID);
    assert_eq!(reqs.len(), 2);
    let req2 = tl_fns::messages::GetAvailableEffects::from_bytes(&reqs[1])
        .expect("decode GetAvailableEffects");
    assert_eq!(req2.hash, 99_441);

    // Download effect animation via `upload.getFile` and verify Lottie JSON retrieval.
    h.rpc.enqueue_ok(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileUnknown,
            mtime: now_unix_i32(),
            bytes: live_lottie_json.as_bytes().to_vec(),
        }),
    );
    let fetched_json = h
        .live
        .effect_lottie_json(99001122334455)
        .expect("fetched effect Lottie JSON");
    assert!(fetched_json.contains("\"op\":30"));

    // Send a message with the live effect ID and verify `messages.sendMessage` transmits `effect`.
    h.rpc.enqueue_ok(
        tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![tl_enums::Update::NewMessage(tl_types::UpdateNewMessage {
                message: tl_enums::Message::Message(tl_types::Message {
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
                    id: 4201,
                    from_id: Some(tl_enums::Peer::User(tl_types::PeerUser { user_id: 10_001 })),
                    from_boosts_applied: None,
                    from_rank: None,
                    peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 20_002 }),
                    saved_peer_id: None,
                    fwd_from: None,
                    via_bot_id: None,
                    via_business_bot_id: None,
                    guestchat_via_from: None,
                    reply_to: None,
                    date: now_unix_i32(),
                    message: "Celebration!".to_owned(),
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
                    quick_reply_shortcut_id: None,
                    effect: Some(99001122334455),
                    factcheck: None,
                    report_delivery_until_date: None,
                    paid_message_stars: None,
                    suggested_post: None,
                    schedule_repeat_period: None,
                    summary_from_language: None,
                    rich_message: None,
                }),
                pts: 10,
                pts_count: 1,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: now_unix_i32(),
            seq: 1,
        }),
    );
    let sent = h
        .live
        .send_message_extended(
            h.peer_id,
            SendMessageExtended {
                text: TextWithEntities::simple("Celebration!"),
                effect_id: Some(99001122334455),
                ..SendMessageExtended::default()
            },
        )
        .expect("send message with live effect");
    assert_eq!(sent.extra.effect_id, Some(99001122334455));
    let send_raw = &h
        .rpc
        .captured_for(tl_fns::messages::SendMessage::CONSTRUCTOR_ID)[0];
    let send_tl = tl_fns::messages::SendMessage::from_bytes(send_raw).expect("decode SendMessage");
    assert_eq!(send_tl.effect, Some(99001122334455));

    // Verify UI picker renders live catalog (without "placeholder" text) and triggers Lottie playback.
    let mut picker = MessageEffectsPickerState::default();
    picker.sync_catalog(catalog);
    picker.open = true;
    picker.trigger_effect(sent.id, 99001122334455, Some(fetched_json), 1.0);
    assert_eq!(picker.active_effects.len(), 1);

    let colors = HistoryColors::from_palette(&Palette::day());
    let mut harness = Harness::builder()
        .with_size(Vec2::new(480.0, 360.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui(move |ui| {
            picker.show_popup(ui, Pos2::new(320.0, 260.0), &colors);
            picker.paint_active_effects(
                ui,
                Rect::from_min_size(Pos2::ZERO, Vec2::new(480.0, 360.0)),
                1.4,
            );
        });
    harness.run_steps(2);
    assert!(harness.query_by_label_contains("Message Effects").is_some());
    assert!(harness.query_by_label_contains("placeholder").is_none());

    // Verify ComposeControls uses live tooltip without "(placeholder)".
    let mut compose = ComposeControls::new();
    let palette = Palette::day();
    let compose_colors = HistoryColors::from_palette(&palette);
    let mut c_harness = Harness::builder()
        .with_size(Vec2::new(620.0, 120.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui(move |ui| {
            let rect = ui.max_rect();
            let _ = compose.show_extended(
                ui,
                rect,
                h.peer_id,
                true,
                false,
                "Write a message...",
                &compose_colors,
                &h.live,
                0,
                &[],
                true,
            );
        });
    c_harness.run();
    assert!(c_harness.query_by_label_contains("placeholder").is_none());
}

#[test]
fn stealth_mode_box_hides_toggle_mock_premium_in_live_mode() {
    let h = build_live_harness(false);
    assert!(!h.live.supports_mock_premium_toggle());
    assert!(!h.live.account_premium());

    // Clicking "UNLOCK WITH TELEGRAM PREMIUM" in live mode MUST emit `OpenPremium`, never `ToggleMockPremium`.
    let live_outcome = Arc::new(Mutex::new(StoryModalOutcome::None));
    let out_clone = Arc::clone(&live_outcome);
    let mut modal = StoryModalBox::StealthMode {
        pending_open_peer: None,
    };
    let views = StoryViews::default();
    let views_live = views.clone();
    let live_for_ui = h.live.clone();
    let mut harness = Harness::builder()
        .with_size(Vec2::new(560.0, 520.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui(move |ui| {
            let o = show_story_modal(
                ui.ctx(),
                ui.max_rect(),
                &mut modal,
                &views_live,
                StealthMode::default(),
                &live_for_ui,
                1_760_000_000,
            );
            if o != StoryModalOutcome::None {
                *out_clone.lock().expect("lock outcome") = o;
            }
        });
    harness.run();
    harness
        .get_by_label_contains("UNLOCK WITH TELEGRAM PREMIUM")
        .click();
    harness.run();

    assert_eq!(
        *live_outcome.lock().expect("lock outcome"),
        StoryModalOutcome::OpenPremium
    );
    assert!(
        !h.live.account_premium(),
        "live session premium status must never be toggled by mock controls"
    );

    // Offline mock backend keeps `supports_mock_premium_toggle() == true` and preserves `ToggleMockPremium`.
    let mut mock_backend = MockStoriesBackend::new(1_760_000_000);
    mock_backend.set_account_premium(false);
    assert!(mock_backend.supports_mock_premium_toggle());
    let mock_outcome = Arc::new(Mutex::new(StoryModalOutcome::None));
    let mock_out_clone = Arc::clone(&mock_outcome);
    let mut mock_modal = StoryModalBox::StealthMode {
        pending_open_peer: None,
    };
    let mut mock_harness = Harness::builder()
        .with_size(Vec2::new(560.0, 520.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui(move |ui| {
            let o = show_story_modal(
                ui.ctx(),
                ui.max_rect(),
                &mut mock_modal,
                &views,
                StealthMode::default(),
                &mock_backend,
                1_760_000_000,
            );
            if o != StoryModalOutcome::None {
                *mock_out_clone.lock().expect("lock mock outcome") = o;
            }
        });
    mock_harness.run();
    mock_harness
        .get_by_label_contains("UNLOCK WITH TELEGRAM PREMIUM")
        .click();
    mock_harness.run();
    assert_eq!(
        *mock_outcome.lock().expect("lock mock outcome"),
        StoryModalOutcome::ToggleMockPremium
    );
}

#[test]
fn business_premium_subscribe_cta_opens_payments_screen_premium() {
    let mut host = BusinessScreenHost::with_mock();
    host.select_screen(BusinessScreen::Premium);
    let snap = host.snapshot().clone();
    let cta_label = host.premium_view_mut().subscribe_label(&snap);
    assert!(
        !cta_label.to_ascii_lowercase().contains("placeholder"),
        "Subscribe CTA must not contain placeholder text: {cta_label}"
    );

    let palette = Palette::day();
    let nav_triggered = Arc::new(Mutex::new(false));
    let nav_flag = Arc::clone(&nav_triggered);
    let mut harness = Harness::builder()
        .with_size(Vec2::new(560.0, 720.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui(move |ui| {
            let _ = host.show_at(ui, &palette, true, 0);
            if host.take_open_payments_premium() {
                *nav_flag.lock().expect("lock nav") = true;
                AppPaymentsHost::open_screen(ui.ctx(), PaymentsScreen::Premium);
            }
        });
    harness.run();
    harness.get_by_label_contains("Telegram Premium •").click();
    harness.run();

    assert!(
        *nav_triggered.lock().expect("lock nav"),
        "Subscribe CTA should request navigation to PaymentsScreen::Premium"
    );
    assert!(AppPaymentsHost::is_open(&harness.ctx));
    assert_eq!(
        AppPaymentsHost::active_screen(&harness.ctx),
        Some(PaymentsScreen::Premium)
    );
}

#[test]
fn payments_backend_renamed_methods_invoke_live_rpcs() {
    let mut h = build_live_harness(false);

    // 1. `topup_stars` invokes `payments.getPaymentForm` (read-only invoice form preview).
    h.rpc.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::Stars(tl_types::payments::PaymentFormStars {
            form_id: 9012,
            bot_id: 777_000,
            title: "Stars Top-Up".to_owned(),
            description: "100 Stars".to_owned(),
            photo: None,
            invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
                test: true,
                name_requested: false,
                phone_requested: false,
                email_requested: false,
                shipping_address_requested: false,
                flexible: false,
                phone_to_provider: false,
                email_to_provider: false,
                recurring: false,
                currency: "XTR".to_owned(),
                prices: Vec::new(),
                max_tip_amount: None,
                suggested_tip_amounts: None,
                terms_url: None,
                subscription_period: None,
            }),
            users: Vec::new(),
        }),
    );
    let msg = h
        .live
        .topup_stars(&CreditTopupOption {
            credits: 100,
            currency: "USD".to_owned(),
            amount: 199,
            product: String::new(),
            extended: false,
            gift_bare_peer_id: 0,
        })
        .expect("topup_stars form preview");
    assert!(msg.contains("9012"));
    assert_eq!(
        h.rpc
            .invoked_count(tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID),
        1
    );

    // 2. `send_star_gift` resolves recipient + invokes `payments.getPaymentForm` + `payments.sendStarsForm`.
    let peer_user = synthetic_user(20_002, "Jordan", "jordan_demo", false, false);
    h.rpc.enqueue_ok(
        tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID,
        &tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
            peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 20_002 }),
            chats: Vec::new(),
            users: vec![tl_enums::User::User(peer_user)],
        }),
    );
    h.rpc.enqueue_ok(
        tl_fns::payments::GetPaymentForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentForm::StarGift(tl_types::payments::PaymentFormStarGift {
            form_id: 9013,
            invoice: tl_enums::Invoice::Invoice(tl_types::Invoice {
                test: true,
                name_requested: false,
                phone_requested: false,
                email_requested: false,
                shipping_address_requested: false,
                flexible: false,
                phone_to_provider: false,
                email_to_provider: false,
                recurring: false,
                currency: "XTR".to_owned(),
                prices: Vec::new(),
                max_tip_amount: None,
                suggested_tip_amounts: None,
                terms_url: None,
                subscription_period: None,
            }),
        }),
    );
    h.rpc.enqueue_ok(
        tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID,
        &tl_enums::payments::PaymentResult::PaymentResult(tl_types::payments::PaymentResult {
            updates: tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: now_unix_i32(),
                seq: 1,
            }),
        }),
    );
    let gift_msg = h
        .live
        .send_star_gift(5501, "@jordan_demo", "Happy birthday!", false, false)
        .expect("send_star_gift");
    assert!(gift_msg.contains("5501"));
    assert_eq!(
        h.rpc
            .invoked_count(tl_fns::payments::SendStarsForm::CONSTRUCTOR_ID),
        1
    );

    // 3. `apply_received_gift_action` returns clear error when gift isn't cached yet.
    let err = h
        .live
        .apply_received_gift_action(99_999, ReceivedGiftAction::ConvertToStars)
        .expect_err("unknown gift ID returns error");
    assert!(err.contains("99999"));
}

fn sample_photo_story(id: i32, now: i32) -> tl_enums::StoryItem {
    tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
        pinned: false,
        public: true,
        close_friends: false,
        min: false,
        noforwards: false,
        edited: false,
        contacts: false,
        selected_contacts: false,
        out: false,
        id,
        date: now,
        from_id: None,
        fwd_from: None,
        expire_date: now + 86_400,
        caption: Some("Harbor at dusk".to_owned()),
        entities: None,
        media: Box::new(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
            spoiler: false,
            live_photo: false,
            photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                has_stickers: false,
                id: i64::from(id) * 10,
                access_hash: i64::from(id) * 19,
                file_reference: vec![1, 2, 3],
                date: now,
                sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                    r#type: "y".to_owned(),
                    w: 360,
                    h: 640,
                    size: 2048,
                })],
                video_sizes: None,
                dc_id: 2,
            })),
            ttl_seconds: None,
            video: None,
        })),
        media_areas: None,
        privacy: None,
        views: None,
        sent_reaction: None,
        albums: None,
        music: None,
    })
}

#[test]
fn live_story_viewer_stealth_unlock_requests_premium_screen() {
    let h = build_live_harness(false);
    let now = now_unix_i32();
    let story_peer = DataPeerId::from_user(DataUserId(20_002));
    let mut store = StoriesStore::new(DataPeerId::from_user(DataUserId(10_001)));
    let peer_stories = tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
        peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: 20_002 }),
        max_read_id: None,
        stories: vec![sample_photo_story(41, now)],
    });
    store.apply_tl_peer_stories(&peer_stories, tg_stories::ParseSource::DirectRequest, now);
    assert!(
        store.source(story_peer).is_some(),
        "peer stories must create a source"
    );

    let mut host = tg_app::stories::AppStoriesHost::with_backend(store, Box::new(h.live.clone()));
    host.handle_strip_actions(
        vec![tg_app::stories::StoriesStripAction::OpenStealth {
            peer_id: story_peer,
            list: tg_stories::StorySourcesList::NotHidden,
        }],
        now,
        0,
    );
    assert!(host.viewer().is_open(), "live story viewer should open");

    let host = std::rc::Rc::new(std::cell::RefCell::new(host));
    let host_ui = std::rc::Rc::clone(&host);
    let palette = Palette::day();
    let mut harness = Harness::builder()
        .with_size(Vec2::new(900.0, 900.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui(move |ui| {
            host_ui.borrow_mut().update_and_render(ui.ctx(), &palette);
        });
    harness.run_steps(2);
    harness
        .get_by_label_contains("UNLOCK WITH TELEGRAM PREMIUM")
        .click();
    harness.run_steps(2);

    let mut host = host.borrow_mut();
    assert!(
        host.take_open_premium_request(),
        "live Stealth Mode unlock must request the Telegram Premium screen"
    );
    assert!(
        !host.take_open_premium_request(),
        "request is consumed once"
    );
    assert!(
        !h.live.account_premium(),
        "live session premium status must never be toggled by mock controls"
    );
}

fn effect_test_message(peer_id: PeerId, id: i32, effect_id: Option<u64>) -> HistoryMessage {
    let extra = tg_app::history::backend::MessageExtra {
        effect_id,
        ..Default::default()
    };
    HistoryMessage {
        id: MsgId(id),
        peer_id,
        from_id: peer_id,
        sender_name: "Jordan".to_owned(),
        date_unix: 1_760_000_000,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple("hi"),
        media: None,
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra,
    }
}

#[test]
fn message_effects_play_only_for_new_messages_never_for_loaded_history() {
    let peer = PeerId::from_user(20_002);
    let mut marks = HashMap::new();
    let mut msgs = vec![
        effect_test_message(peer, 10, Some(5_104_841_245_755_180_586)),
        effect_test_message(peer, 11, None),
    ];

    // First sight of the peer (history shown on open) only seeds the watermark.
    assert!(take_new_effect_messages(&mut marks, peer, &msgs).is_empty());
    assert_eq!(marks.get(&peer), Some(&MsgId(11)));

    // Older history loaded by scrolling up never replays effects.
    msgs.insert(0, effect_test_message(peer, 3, Some(99_001_122_334_455)));
    assert!(take_new_effect_messages(&mut marks, peer, &msgs).is_empty());
    assert_eq!(marks.get(&peer), Some(&MsgId(11)));

    // A newly arrived message with an effect plays exactly once.
    msgs.push(effect_test_message(peer, 12, Some(99_001_122_334_455)));
    msgs.push(effect_test_message(peer, 13, None));
    assert_eq!(
        take_new_effect_messages(&mut marks, peer, &msgs),
        vec![(MsgId(12), 99_001_122_334_455)]
    );
    assert!(take_new_effect_messages(&mut marks, peer, &msgs).is_empty());
    assert_eq!(marks.get(&peer), Some(&MsgId(13)));

    // An empty chat has nothing to seed yet.
    let other = PeerId::from_user(30_003);
    assert!(take_new_effect_messages(&mut marks, other, &[]).is_empty());
    assert!(!marks.contains_key(&other));
}
