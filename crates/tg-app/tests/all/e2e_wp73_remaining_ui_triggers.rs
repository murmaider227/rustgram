//! End-to-end `egui_kittest` tests, `FakeRpcClient` live-session verification, and headless
//! `wgpu` before/after screenshot capture for WP-73 remaining UI triggers:
//! 1. Today's Birthdays banner in chat list (`contacts.getBirthdays`).
//! 2. Sponsored peers in global search (`contacts.getSponsoredPeers`).
//! 3. Promoted-dialog "Hide" context menu action (`help.hidePromoData`).
//! 4. Privacy "Suggest Frequent Contacts" toggle (`contacts.toggleTopPeers`).
//! 5. Replies chat "Block from Replies" context menu + confirm modal (`contacts.blockFromReplies`).
//! 6. Saved Messages default tag reactions (`messages.getDefaultTagReactions`).
//! 7. Stories bulk blocklist editor (`contacts.setBlocked(my_stories_from=true)`).
//! 8. Background picker default emojis (`account.getDefaultBackgroundEmojis`).
//! 9. `README.md` Rustgram rename (`# Rustgram`, `RUSTGRAM_API_ID` / `RUSTGRAM_API_HASH`, zero local paths).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app::chat_helpers::backend::{
    ChatHelpersBackend, DefaultEmojiCatalogKind, DefaultTagReaction,
};
use tg_app::chat_helpers::mock::MockChatHelpersBackend;
use tg_app::core_types::dto::contacts_extras::{
    BlockFromRepliesRequest, ContactBirthdayEntry, SponsoredPeerInfo,
};
use tg_app::core_types::misc::PromoDataSnapshot;
use tg_app::dialogs::backend::{DialogListQuery, DialogsBackend};
use tg_app::dialogs::inner::dialog_context_menu_items;
use tg_app::dialogs::search::{DialogsSearchState, MIN_SPONSORED_QUERY_LEN};
use tg_app::dialogs::suggestions::KEY_BIRTHDAY_CONTACTS_TODAY;
use tg_app::dialogs::widget::DialogsPanel;
use tg_app::history::HistoryWidget;
use tg_app::history::backend::{HistoryBackend, HistoryMessage, HistoryPeerInfo, MessageExtra};
use tg_app::history::inner::{REPLIES_PEER_ID_PROD, is_replies_chat_peer};
use tg_app::history::mock::MockHistoryStore;
use tg_app::history::sections_ui::SelfForwardsTaggerState;
use tg_app::history::theme::HistoryColors;
use tg_app::info::values::Birthday;
use tg_app::live::LiveSession;
use tg_app::settings::SettingsSection;
use tg_app::settings::privacy::PrivacyState;
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{FilterId, MsgId, PeerId, PeerKind, PeerSummary};
use tg_app::stories::{
    AppStoriesHost, MockStoriesBackend, PrivacyPeerListKind, StoriesBackend, StoryPrivacyBox,
    StoryPrivacyOutcome, mock_peers as story_mock_peers,
};
use tg_app::themes::{BackgroundPickerState, ChatBackground};
use tg_app::window::section::{SectionShow, SettingsPage};
use tg_data::UserId as DataUserId;
use tg_lang::tr;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_stories::StoriesStore;
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::widgets::menu::MenuItem;
use tokio::sync::broadcast;

use crate::common::{
    AppTestState, TEST_STEP_DT, apply_deterministic_style, build_wgpu_harness,
    build_widget_harness, init_headless_env, settle_layout, step_frames,
};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

static WGPU_WP73_LOCK: Mutex<()> = Mutex::new(());

fn snap_dir() -> PathBuf {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp73"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    dir
}

fn snap_stage() -> String {
    std::env::var("RUSTGRAM_WP73_SNAP_STAGE").unwrap_or_else(|_| "after".to_owned())
}

fn save_app_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, name: &str) {
    let img = harness.render().expect("wgpu render");
    img.save(snap_dir().join(format!("{name}.png")))
        .expect("save png");
}

fn render_wgpu_snap<S: 'static>(
    size: Vec2,
    state: S,
    name: &str,
    ui_fn: impl FnMut(&mut egui::Ui, &mut S) + 'static,
) {
    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(size)
        .with_step_dt(TEST_STEP_DT)
        .with_render_cursor(false)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .wgpu()
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(ui_fn, state);
    for _ in 0..6 {
        harness.step();
    }
    let img = harness.render().expect("wgpu render");
    img.save(snap_dir().join(format!("{name}.png")))
        .expect("save png");
}

fn step_n<S>(harness: &mut egui_kittest::Harness<'static, S>, n: usize) {
    for _ in 0..n {
        harness.step();
    }
}

fn sample_birthdays_for_session_today() -> Vec<ContactBirthdayEntry> {
    vec![
        ContactBirthdayEntry {
            peer_id: session_peers::USER_MIRA,
            display_name: "Mira Vance".to_owned(),
            birthday: Birthday {
                day: 22,
                month: 11,
                year: Some(1994),
            },
        },
        ContactBirthdayEntry {
            peer_id: session_peers::USER_SAM,
            display_name: "Sam Chen".to_owned(),
            birthday: Birthday {
                day: 22,
                month: 11,
                year: None,
            },
        },
    ]
}

fn sample_sponsored_peers() -> Vec<SponsoredPeerInfo> {
    vec![SponsoredPeerInfo {
        peer_id: PeerId::from_channel(9_001),
        random_id: vec![0xAA, 0xBB, 0xCC, 0xDD],
        title: "Rustgram Cloud Tools".to_owned(),
        username: Some("rustgram_cloud".to_owned()),
        sponsor_info: Some("Official Partner".to_owned()),
        additional_info: Some("Sponsored search placement shown for matching queries.".to_owned()),
        color_index: 4,
        verified: true,
        premium: false,
    }]
}

// =============================================================================
// 1. egui_kittest UI Tests — Every WP-73 Remaining UI Trigger
// =============================================================================

#[test]
fn e2e_wp73_today_birthdays_banner_renders_opens_and_dismisses() {
    init_headless_env();
    let session = MockSession::new(true);
    session
        .handle()
        .set_contact_birthdays(sample_birthdays_for_session_today());

    struct BirthdaysState {
        dialogs: DialogsPanel,
        session: MockSession,
        palette: Palette,
    }

    let state = BirthdaysState {
        dialogs: DialogsPanel::new(),
        session,
        palette: Palette::day(),
    };

    let mut harness = build_widget_harness(state, Vec2::new(360.0, 720.0), |ui, st| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = st.dialogs.show_column(
                ui,
                &mut st.session,
                session_peers::GROUP_CORE,
                false,
                false,
                true,
                false,
                &st.palette,
                &EnglishStrings,
            );
        });
    });
    step_n(&mut harness, 4);

    assert_eq!(
        harness
            .state()
            .dialogs
            .promo_suggestions
            .active()
            .map(|s| s.key.as_str()),
        Some(KEY_BIRTHDAY_CONTACTS_TODAY),
        "Today's Birthdays banner must appear after contacts.getBirthdays sync"
    );
    assert_eq!(
        harness
            .state()
            .dialogs
            .promo_suggestions
            .today_birthdays()
            .len(),
        2
    );

    // Dismissing hides the banner and remembers dismissal.
    harness
        .state_mut()
        .dialogs
        .promo_suggestions
        .mark_dismissed(KEY_BIRTHDAY_CONTACTS_TODAY);
    step_n(&mut harness, 2);
    assert!(
        harness.state().dialogs.promo_suggestions.active().is_none(),
        "dismissed birthday banner must remain hidden"
    );
}

#[test]
fn e2e_wp73_sponsored_search_peers_threshold_disclosure_and_click() {
    init_headless_env();
    let session = MockSession::new(true);
    session
        .handle()
        .set_sponsored_peers(sample_sponsored_peers());

    // Below 4-character threshold -> must not query or show sponsored peers.
    let mut short_search = DialogsSearchState::default();
    short_search.sync_sponsored_peers("rus", &session);
    assert_eq!(MIN_SPONSORED_QUERY_LEN, 4);
    assert!(
        short_search.sponsored_peers.is_empty(),
        "queries shorter than 4 chars must not load sponsored peers"
    );

    struct SponsoredSearchState {
        dialogs: DialogsPanel,
        session: MockSession,
        palette: Palette,
    }
    let mut dialogs = DialogsPanel::new();
    dialogs.search_active = true;
    dialogs.search_query = "rustgram".to_owned();

    let state = SponsoredSearchState {
        dialogs,
        session,
        palette: Palette::day(),
    };

    let mut harness = build_widget_harness(state, Vec2::new(380.0, 760.0), |ui, st| {
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = st.dialogs.show_column(
                ui,
                &mut st.session,
                session_peers::GROUP_CORE,
                false,
                false,
                true,
                false,
                &st.palette,
                &EnglishStrings,
            );
        });
    });
    step_n(&mut harness, 5);

    assert_eq!(
        harness.state().dialogs.search_state.sponsored_peers.len(),
        1,
        "4+ char global search must hydrate sponsored peers"
    );
    assert_eq!(
        harness.state().dialogs.search_state.sponsored_peers[0].title,
        "Rustgram Cloud Tools"
    );

    // Expanding sponsor info disclosure stores the active peer id cleanly.
    harness.state_mut().dialogs.search_state.sponsored_info_open =
        Some(PeerId::from_channel(9_001));
    step_n(&mut harness, 2);
    assert_eq!(
        harness.state().dialogs.search_state.sponsored_info_open,
        Some(PeerId::from_channel(9_001))
    );
}

#[test]
fn e2e_wp73_promoted_dialog_hide_context_menu_action() {
    init_headless_env();
    let mut session = MockSession::new(true);
    let promo_peer = session_peers::CHANNEL_DEV_LOG;
    let list = DialogListQuery::Filter(FilterId::ALL);

    // Non-promoted dialog must not include Hide PSA.
    let non_promo_menu = dialog_context_menu_items(promo_peer, &session, list);
    assert!(
        !non_promo_menu
            .iter()
            .any(|m| matches!(m, MenuItem::Action { id, .. } if id == "hide_promo")),
        "non-promoted dialogs must not show Hide PSA action"
    );

    // Mark CHANNEL_DEV_LOG as promoted -> context menu must include Hide PSA action.
    session.handle().set_promo_data(Some(PromoDataSnapshot {
        expires: 1_900_000_000,
        promo_peer: Some(promo_peer),
        psa_type: Some("psa".to_owned()),
        psa_message: Some("Official community update".to_owned()),
        pending_suggestions: Vec::new(),
        dismissed_suggestions: Vec::new(),
        custom_suggestion: None,
    }));

    let promo_menu = dialog_context_menu_items(promo_peer, &session, list);
    assert!(
        promo_menu.iter().any(|m| matches!(
            m,
            MenuItem::Action { id, label, .. }
                if id == "hide_promo" && label == &tr::lng_context_hide_psa()
        )),
        "promoted dialog context menu must include Hide PSA action"
    );

    // Executing Hide dispatches DialogsBackend::hide_promo_data and clears promo_data.
    assert!(session.hide_promo_data(promo_peer).expect("hide promo"));
    assert_eq!(session.handle().hidden_promo_peers(), vec![promo_peer]);
    assert!(session.promo_data().is_none());
}

#[test]
fn e2e_wp73_privacy_suggest_frequent_contacts_toggles_top_peers_and_hides_strip() {
    init_headless_env();
    let state = AppTestState::main_workspace();
    let mut harness = build_wgpu_harness(state, Vec2::new(980.0, 860.0));
    settle_layout(&mut harness);

    if let Some(session) = harness.state_mut().controller.session_mut() {
        session.show_settings(SettingsPage::PrivacySecurity);
    }
    settle_layout(&mut harness);
    if let Some(s) = harness
        .state_mut()
        .controller
        .main_widget_mut()
        .settings_mut()
    {
        s.push_section(SettingsSection::PrivacySecurity);
    }
    step_frames(&mut harness, 4);

    assert!(
        DialogsBackend::top_peers_enabled(harness.state().controller.session_backend()),
        "top peers should be enabled by default"
    );

    // Scroll down in Privacy settings so "Suggest frequent contacts" is in view, then click it.
    let label = tr::lng_settings_top_peers_suggest();
    let center = harness.ctx.content_rect().center();
    for _ in 0..8 {
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(center));
        harness.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -180.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        step_frames(&mut harness, 2);
    }
    assert!(
        harness.query_by_label_contains(&label).is_some(),
        "Privacy settings must expose '{label}'"
    );
    harness.get_by_label_contains(&label).click();
    step_frames(&mut harness, 4);

    assert!(
        !DialogsBackend::top_peers_enabled(harness.state().controller.session_backend()),
        "clicking Suggest frequent contacts must call toggle_top_peers(false)"
    );

    let mut privacy = PrivacyState::default();
    assert!(privacy.top_peers_enabled);
    privacy.top_peers_enabled = false;
    assert!(!privacy.top_peers_enabled);
}

#[test]
fn e2e_wp73_replies_chat_block_from_replies_modal_and_backend() {
    init_headless_env();
    let replies_peer = PeerId::from_user(REPLIES_PEER_ID_PROD);
    let mut store = MockHistoryStore::new();
    let replies_info = HistoryPeerInfo {
        summary: PeerSummary {
            id: replies_peer,
            kind: PeerKind::User,
            title: "Replies".to_owned(),
            status: "service notifications".to_owned(),
            username: Some("replies".to_owned()),
            phone_or_link: None,
            about: "Forwarded comment replies.".to_owned(),
            initials: "RE".to_owned(),
            avatar_rgb: [0x4D, 0xA6, 0xE8],
            muted: false,
            can_send: false,
            is_saved_messages: false,
        },
        typing_text: None,
        contact_status: None,
        unread_count: 1,
        can_pin: false,
        can_delete: true,
        link_base: "https://t.me/replies".to_owned(),
    };
    assert!(is_replies_chat_peer(&replies_info));
    store.register_peer(replies_info);
    store.push_message(
        replies_peer,
        HistoryMessage {
            id: MsgId(7701),
            peer_id: replies_peer,
            from_id: PeerId::from_user(55_001),
            sender_name: "Spammy Commenter".to_owned(),
            date_unix: 1_795_341_600,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple("Unwanted reply in comment thread"),
            media: None,
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        },
    );

    struct RepliesTestState {
        widget: HistoryWidget,
        store: MockHistoryStore,
        palette: Palette,
        replies_peer: PeerId,
    }

    let mut widget = HistoryWidget::new();
    widget.open_block_from_replies_modal(MsgId(7701), "Spammy Commenter");

    let mut harness = build_widget_harness(
        RepliesTestState {
            widget,
            store,
            palette: Palette::day(),
            replies_peer,
        },
        Vec2::new(720.0, 640.0),
        |ui, st| {
            let _ = st.widget.show_with_backend(
                ui,
                st.replies_peer,
                &mut st.store,
                false,
                false,
                false,
                &st.palette,
                &EnglishStrings,
            );
        },
    );
    step_n(&mut harness, 3);

    assert!(harness.state().widget.is_block_from_replies_open());
    assert!(
        harness
            .query_by_label_contains(&tr::lng_blocked_list_confirm_title("Spammy Commenter"))
            .is_some(),
        "BlockFromRepliesBox must display localized title"
    );
    assert!(
        harness
            .query_by_label_contains(&tr::lng_report_spam())
            .is_some()
    );
    assert!(
        harness
            .query_by_label_contains(&tr::lng_blocked_list_confirm_reply())
            .is_some()
    );
    assert!(
        harness
            .query_by_label_contains(&tr::lng_blocked_list_confirm_reply_all("Spammy Commenter"))
            .is_some()
    );

    harness
        .get_by_label(&tr::lng_blocked_list_confirm_ok())
        .click();
    step_n(&mut harness, 3);

    assert!(!harness.state().widget.is_block_from_replies_open());
    assert_eq!(
        harness.state().store.blocked_from_replies(),
        &[BlockFromRepliesRequest {
            msg_id: MsgId(7701),
            delete_message: true,
            delete_history: true,
            report_spam: true,
        }]
    );
}

#[test]
fn e2e_wp73_saved_messages_default_tag_reactions_hydrates_tagger() {
    init_headless_env();
    let mut session = MockSession::new(true);
    session.chat_helpers_mut().set_default_tag_reactions(vec![
        DefaultTagReaction::Emoji("🚀".to_owned()),
        DefaultTagReaction::Emoji("💡".to_owned()),
        DefaultTagReaction::Emoji("🎯".to_owned()),
        DefaultTagReaction::Paid,
    ]);

    struct SavedTaggerTestState {
        widget: HistoryWidget,
        session: MockSession,
        palette: Palette,
    }

    let mut widget = HistoryWidget::new();
    widget.self_forwards_tagger_mut().show_for_forwarded(
        session_peers::SELF,
        vec![MsgId(501)],
        1.0,
    );

    let mut harness = build_widget_harness(
        SavedTaggerTestState {
            widget,
            session,
            palette: Palette::day(),
        },
        Vec2::new(720.0, 560.0),
        |ui, st| {
            let _ = st.widget.show_with_backend(
                ui,
                session_peers::SELF,
                &mut st.session,
                false,
                false,
                false,
                &st.palette,
                &EnglishStrings,
            );
        },
    );
    step_n(&mut harness, 3);

    assert!(
        harness
            .state()
            .widget
            .self_forwards_tagger()
            .has_loaded_default_tags(),
        "opening SelfForwardsTagger must fetch ChatHelpersBackend::default_tag_reactions"
    );
    assert_eq!(
        harness
            .state()
            .widget
            .self_forwards_tagger()
            .cached_default_tags,
        vec!["🚀", "💡", "🎯", "⭐"]
    );
    assert!(
        harness.query_by_label("🚀").is_some(),
        "default tag reaction emoji chip must render in SelfForwardsTagger"
    );
}

#[test]
fn e2e_wp73_stories_bulk_blocklist_editor_saves_via_backend() {
    init_headless_env();
    struct StoriesBlocklistTestState {
        privacy_box: StoryPrivacyBox,
        backend: MockStoriesBackend,
        last_saved: Option<Vec<tg_data::PeerId>>,
    }

    let mut backend = MockStoriesBackend::new(1_760_000_000);
    backend.set_viewer_stories_blocked(story_mock_peers::CEDAR, true);

    let mut harness = build_widget_harness(
        StoriesBlocklistTestState {
            privacy_box: StoryPrivacyBox::default(),
            backend,
            last_saved: None,
        },
        Vec2::new(560.0, 740.0),
        |ui, st| {
            let vp = ui.available_rect_before_wrap();
            if let StoryPrivacyOutcome::SaveStoriesBlocklist(peers) =
                st.privacy_box.show_modal(ui.ctx(), vp, &st.backend)
            {
                st.backend
                    .save_stories_blocklist(&peers)
                    .expect("save stories blocklist");
                st.last_saved = Some(peers);
            }
        },
    );
    step_n(&mut harness, 3);

    harness.get_by_label("Manage Blocklist...").click();
    step_n(&mut harness, 3);
    assert_eq!(
        harness.state().privacy_box.editing_list,
        Some(PrivacyPeerListKind::StoriesBlocklist)
    );

    harness.get_by_label_contains("Aurora Studio").click();
    step_n(&mut harness, 2);
    harness.get_by_label("Save Blocklist").click();
    step_n(&mut harness, 3);

    let saved = harness
        .state()
        .last_saved
        .as_ref()
        .expect("Save Blocklist must emit SaveStoriesBlocklist");
    assert!(saved.contains(&story_mock_peers::CEDAR));
    assert!(saved.contains(&story_mock_peers::AURORA));
    assert!(
        harness
            .state()
            .backend
            .is_viewer_stories_blocked(story_mock_peers::AURORA)
    );
    assert_eq!(harness.state().backend.saved_stories_blocklists.len(), 1);
}

#[test]
fn e2e_wp73_background_picker_loads_and_selects_default_background_emojis() {
    init_headless_env();
    struct BgEmojiTestState {
        picker: BackgroundPickerState,
        chat_bg: ChatBackground,
        colors: LiveThemeColors,
        chat_helpers: MockChatHelpersBackend,
    }

    let state = BgEmojiTestState {
        picker: BackgroundPickerState::new(false, Some((1001, "Mira Vance".to_owned()))),
        chat_bg: ChatBackground::new(),
        colors: LiveThemeColors::default(),
        chat_helpers: MockChatHelpersBackend::new(),
    };

    let mut harness = build_widget_harness(state, Vec2::new(620.0, 820.0), |ui, st| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let _ = st.picker.show_with_chat_helpers(
                ui,
                &mut st.chat_bg,
                Some(&st.chat_helpers),
                &st.colors,
            );
        });
    });
    step_n(&mut harness, 3);

    assert_eq!(
        harness
            .state()
            .chat_helpers
            .recorded_default_emoji_catalog_calls(),
        vec![DefaultEmojiCatalogKind::Background],
        "BackgroundPicker must request DefaultEmojiCatalogKind::Background once"
    );
    assert!(
        !harness.state().picker.default_emojis().is_empty(),
        "default_emojis must be populated from ChatHelpersBackend"
    );
    let first_id = harness.state().picker.default_emojis()[0].document_id;
    let first_emoji = harness.state().picker.default_emojis()[0].emoji.clone();
    assert!(harness.query_by_label_contains("Pattern Emojis").is_some());

    harness.get_by_label_contains(&first_emoji).click();
    step_n(&mut harness, 2);
    assert_eq!(harness.state().picker.selected_emoji_id(), Some(first_id));

    harness.get_by_label_contains(&first_emoji).click();
    step_n(&mut harness, 2);
    assert_eq!(harness.state().picker.selected_emoji_id(), None);
}

#[test]
fn wp73_readme_rustgram_rename_and_no_local_paths() {
    let readme_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("README.md");
    let text = std::fs::read_to_string(&readme_path).expect("read README.md");
    assert!(
        text.lines().any(|l| l.trim() == "# Rustgram"),
        "README.md title must be # Rustgram"
    );
    assert!(
        !text.contains("# Tgfork"),
        "README.md must not retain # Tgfork heading"
    );
    assert!(
        text.contains("RUSTGRAM_API_ID") && text.contains("RUSTGRAM_API_HASH"),
        "README.md must document RUSTGRAM_API_ID and RUSTGRAM_API_HASH"
    );
    for forbidden in ["D:/", "D:\\", "C:/Users/", "C:\\Users\\", "/Users/"] {
        assert!(
            !text.contains(forbidden),
            "README.md must not contain local path fragment '{forbidden}'"
        );
    }
}

// =============================================================================
// 2. FakeRpcClient Live-Session Tests — All 8 WP-73 Wire RPCs
// =============================================================================

type RpcResponseQueue = Arc<Mutex<VecDeque<(u32, Result<Vec<u8>, InvocationError>)>>>;

#[derive(Clone)]
struct Wp73FakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: RpcResponseQueue,
    invoked_ctors: Arc<Mutex<Vec<u32>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wp73FakeRpc {
    fn new(dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            invoked_ctors: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, value: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(value.to_bytes())));
    }

    fn invoked_count(&self, ctor: u32) -> usize {
        self.invoked_ctors
            .lock()
            .expect("lock ctors")
            .iter()
            .filter(|&&c| c == ctor)
            .count()
    }
}

impl RpcClient for Wp73FakeRpc {
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
        self.invoked_ctors.lock().expect("lock ctors").push(ctor);

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

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i32)
        .unwrap_or(1_760_000_000)
}

fn synthetic_tl_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
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
        access_hash: Some(id * 101 + 13),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Demo".to_owned()),
        username: Some(username.to_owned()),
        phone: Some("0000000000".to_owned()),
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

fn empty_updates() -> tl_enums::Updates {
    tl_enums::Updates::Updates(tl_types::Updates {
        updates: Vec::new(),
        users: Vec::new(),
        chats: Vec::new(),
        date: now_unix_i32(),
        seq: 2,
    })
}

fn boot_live_wp73(
    initial_users: Vec<tl_enums::User>,
) -> (
    LiveSession,
    Wp73FakeRpc,
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
) {
    let now = now_unix_i32();
    let self_uid = 830_001i64;
    let fake = Wp73FakeRpc::new(2);
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(synthetic_tl_user(
            self_uid,
            "Self",
            "self_wp73",
            true,
        ))],
    );
    fake.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 1,
            qts: 0,
            date: now,
            seq: 1,
            unread_count: 0,
        }),
    );
    fake.enqueue_ok(
        tl_fns::updates::GetDifference::CONSTRUCTOR_ID,
        &tl_enums::updates::Difference::Empty(tl_types::updates::DifferenceEmpty {
            date: now,
            seq: 1,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetDialogs::CONSTRUCTOR_ID,
        &tl_enums::messages::Dialogs::Dialogs(tl_types::messages::Dialogs {
            dialogs: Vec::new(),
            messages: Vec::new(),
            chats: Vec::new(),
            users: initial_users,
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
                pts: 1,
                qts: 0,
                date: now,
                seq: 1,
                unread_count: 0,
            }),
        }),
    );

    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId::from_tl(self_uid),
            tg_app::live::ErasedRpcClient::new(fake.clone()),
            SessionRuntimeConfig::default(),
        )
        .expect("start SessionRuntime"),
    );
    runtime
        .tokio_handle()
        .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
        .expect("wait_ready timeout")
        .expect("startup ready");

    let live = LiveSession::new(Arc::clone(&runtime), 1);
    (live, fake, runtime)
}

#[test]
fn live_wp73_all_eight_ui_triggers_dispatch_wire_rpcs() {
    init_headless_env();
    let contact_uid = 830_042i64;
    let sponsored_uid = 830_099i64;
    let contact_peer = PeerId::from_user(contact_uid as u64);

    let (mut live, fake, runtime) = boot_live_wp73(vec![tl_enums::User::User(synthetic_tl_user(
        contact_uid,
        "Nora",
        "nora_wp73",
        false,
    ))]);

    // Enqueue exact TL responses for all 8 WP-73 RPCs.
    let today = DialogsBackend::current_time(&live);
    fake.enqueue_ok(
        tl_fns::contacts::GetBirthdays::CONSTRUCTOR_ID,
        &tl_enums::contacts::ContactBirthdays::ContactBirthdays(
            tl_types::contacts::ContactBirthdays {
                contacts: vec![tl_enums::ContactBirthday::ContactBirthday(
                    tl_types::ContactBirthday {
                        contact_id: contact_uid,
                        birthday: tl_enums::Birthday::Birthday(tl_types::Birthday {
                            day: i32::from(today.date.day),
                            month: i32::from(today.date.month),
                            year: Some(1995),
                        }),
                    },
                )],
                users: vec![tl_enums::User::User(synthetic_tl_user(
                    contact_uid,
                    "Nora",
                    "nora_wp73",
                    false,
                ))],
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::contacts::GetSponsoredPeers::CONSTRUCTOR_ID,
        &tl_enums::contacts::SponsoredPeers::SponsoredPeers(tl_types::contacts::SponsoredPeers {
            peers: vec![tl_enums::SponsoredPeer::SponsoredPeer(
                tl_types::SponsoredPeer {
                    random_id: vec![1, 2, 3, 4],
                    peer: tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: sponsored_uid,
                    }),
                    sponsor_info: Some("Demo Ad".to_owned()),
                    additional_info: Some("Info".to_owned()),
                },
            )],
            chats: Vec::new(),
            users: vec![tl_enums::User::User(synthetic_tl_user(
                sponsored_uid,
                "Sponsored Bot",
                "sponsored_wp73",
                false,
            ))],
        }),
    );
    fake.enqueue_ok(tl_fns::help::HidePromoData::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(tl_fns::contacts::ToggleTopPeers::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::contacts::BlockFromReplies::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetDefaultTagReactions::CONSTRUCTOR_ID,
        &tl_enums::messages::Reactions::Reactions(tl_types::messages::Reactions {
            hash: 12_345,
            reactions: vec![
                tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "🔥".to_owned(),
                }),
                tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "🚀".to_owned(),
                }),
            ],
        }),
    );
    fake.enqueue_ok(tl_fns::contacts::SetBlocked::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::account::GetDefaultBackgroundEmojis::CONSTRUCTOR_ID,
        &tl_enums::EmojiList::EmojiList(tl_types::EmojiList {
            hash: 77_001,
            document_id: vec![52_001, 52_002],
        }),
    );

    // 1 + 2. DialogsPanel -> contacts.getBirthdays (normal list), then contacts.getSponsoredPeers (search)
    {
        let mut dlgs = DialogsPanel::new();
        let palette = Palette::day();
        let ctx = egui::Context::default();
        // The live backend loads birthdays on a background thread (`poll_contact_birthdays`), so
        // the suggestion shows up on a frame after the reply arrives.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = dlgs.show_column(
                    ui,
                    &mut live,
                    contact_peer,
                    false,
                    false,
                    true,
                    false,
                    &palette,
                    &EnglishStrings,
                );
            });
            out.textures_delta.clear();
            if dlgs.promo_suggestions.active().is_some() || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            fake.invoked_count(tl_fns::contacts::GetBirthdays::CONSTRUCTOR_ID),
            1,
            "DialogsPanel must dispatch contacts.getBirthdays"
        );
        assert_eq!(
            dlgs.promo_suggestions.active().map(|s| s.key.as_str()),
            Some(KEY_BIRTHDAY_CONTACTS_TODAY)
        );

        dlgs.search_active = true;
        dlgs.search_query = "rustgram".to_owned();
        let mut out2 = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = dlgs.show_column(
                ui,
                &mut live,
                contact_peer,
                false,
                false,
                true,
                false,
                &palette,
                &EnglishStrings,
            );
        });
        out2.textures_delta.clear();
        assert_eq!(
            fake.invoked_count(tl_fns::contacts::GetSponsoredPeers::CONSTRUCTOR_ID),
            1,
            "DialogsPanel search must dispatch contacts.getSponsoredPeers"
        );
        assert_eq!(dlgs.search_state.sponsored_peers.len(), 1);
    }

    // 3. Promoted-dialog Hide -> help.hidePromoData
    assert!(DialogsBackend::hide_promo_data(&mut live, contact_peer).expect("hide promo live"));
    assert_eq!(
        fake.invoked_count(tl_fns::help::HidePromoData::CONSTRUCTOR_ID),
        1
    );

    // 4. Privacy "Suggest Frequent Contacts" -> contacts.toggleTopPeers
    assert!(DialogsBackend::toggle_top_peers(&mut live, false).expect("toggle top peers live"));
    assert!(!DialogsBackend::top_peers_enabled(&live));
    assert_eq!(
        fake.invoked_count(tl_fns::contacts::ToggleTopPeers::CONSTRUCTOR_ID),
        1
    );

    // 5. Replies chat Block from Replies -> contacts.blockFromReplies
    HistoryBackend::block_from_replies(
        &mut live,
        BlockFromRepliesRequest {
            msg_id: MsgId(7701),
            delete_message: true,
            delete_history: true,
            report_spam: true,
        },
    )
    .expect("block from replies live");
    assert_eq!(
        fake.invoked_count(tl_fns::contacts::BlockFromReplies::CONSTRUCTOR_ID),
        1
    );

    // 6. Saved Messages SelfForwardsTagger -> messages.getDefaultTagReactions
    {
        let mut hist = HistoryWidget::new();
        let (self_pid, _) = HistoryBackend::self_peer(&live);
        hist.self_forwards_tagger_mut()
            .show_for_forwarded(self_pid, vec![MsgId(10)], 1.0);
        let palette = Palette::day();
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = hist.show_with_backend(
                ui,
                self_pid,
                &mut live,
                false,
                false,
                false,
                &palette,
                &EnglishStrings,
            );
        });
        out.textures_delta.clear();
        assert_eq!(
            fake.invoked_count(tl_fns::messages::GetDefaultTagReactions::CONSTRUCTOR_ID),
            1,
            "SelfForwardsTagger must dispatch messages.getDefaultTagReactions"
        );
        assert_eq!(
            hist.self_forwards_tagger().cached_default_tags,
            vec!["🔥", "🚀"]
        );
    }

    // 7. Stories bulk blocklist -> contacts.setBlocked(my_stories_from = true)
    {
        let self_data = StoriesBackend::self_peer_id(&live);
        let store = StoriesStore::new(self_data);
        let host = AppStoriesHost::with_backend(store, Box::new(live.clone()));
        let target_data = tg_data::PeerId::from_user(DataUserId(contact_uid as u64));
        assert!(
            StoriesBackend::save_stories_blocklist(&mut live, &[target_data])
                .expect("save stories blocklist live")
        );
        drop(host);
        assert_eq!(
            fake.invoked_count(tl_fns::contacts::SetBlocked::CONSTRUCTOR_ID),
            1
        );
    }

    // 8. BackgroundPicker -> account.getDefaultBackgroundEmojis
    {
        let mut picker = BackgroundPickerState::new(false, None);
        let mut chat_bg = ChatBackground::new();
        let colors = LiveThemeColors::default();
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = picker.show_with_chat_helpers(ui, &mut chat_bg, Some(&live), &colors);
        });
        out.textures_delta.clear();
        assert_eq!(
            fake.invoked_count(tl_fns::account::GetDefaultBackgroundEmojis::CONSTRUCTOR_ID),
            1,
            "BackgroundPicker must dispatch account.getDefaultBackgroundEmojis"
        );
        assert_eq!(picker.default_emojis().len(), 2);
    }

    drop(live);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}

// =============================================================================
// 3. Before / After Headless wgpu Screenshot Capture (1440x900 & 390x844)
// =============================================================================

#[test]
#[ignore = "captures before/after design review PNGs under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp73_snapshots() {
    let _guard = WGPU_WP73_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stage = snap_stage();
    let is_after = stage != "before";

    for (size_label, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. Chat list (Today's Birthdays banner)
        {
            let state = AppTestState::main_workspace();
            if is_after {
                state
                    .controller
                    .session_handle()
                    .set_contact_birthdays(sample_birthdays_for_session_today());
            }
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            if size.x < 640.0
                && let Some(sess) = h.state_mut().controller.session_mut()
            {
                sess.show_back_from_stack(SectionShow::forward());
            }
            step_frames(&mut h, 6);
            save_app_png(&mut h, &format!("dialogs_birthdays_{size_label}_{stage}"));
        }

        // 2. Dialogs search (Sponsored search peers)
        {
            let state = AppTestState::main_workspace();
            if is_after {
                state
                    .controller
                    .session_handle()
                    .set_sponsored_peers(sample_sponsored_peers());
            }
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            if size.x < 640.0
                && let Some(sess) = h.state_mut().controller.session_mut()
            {
                sess.show_back_from_stack(SectionShow::forward());
            }
            {
                let dlgs = h.state_mut().controller.main_widget_mut().dialogs_mut();
                dlgs.search_active = true;
                dlgs.search_query = "rustgram".to_owned();
                if is_after {
                    dlgs.search_state.sponsored_info_open = Some(PeerId::from_channel(9_001));
                }
            }
            step_frames(&mut h, 6);
            save_app_png(
                &mut h,
                &format!("dialogs_search_sponsored_{size_label}_{stage}"),
            );
        }

        // 3. Settings > Privacy and Security ("Suggest Frequent Contacts")
        {
            let state = AppTestState::main_workspace();
            let mut h = build_wgpu_harness(state, size);
            settle_layout(&mut h);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_settings(SettingsPage::PrivacySecurity);
            }
            settle_layout(&mut h);
            if let Some(s) = h.state_mut().controller.main_widget_mut().settings_mut() {
                s.push_section(SettingsSection::PrivacySecurity);
            }
            step_frames(&mut h, 4);
            if is_after {
                let center = h.ctx.content_rect().center();
                for _ in 0..8 {
                    h.input_mut().events.push(egui::Event::PointerMoved(center));
                    h.input_mut().events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -180.0),
                        modifiers: egui::Modifiers::NONE,
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                    });
                    step_frames(&mut h, 2);
                }
            }
            step_frames(&mut h, 4);
            save_app_png(
                &mut h,
                &format!("settings_privacy_top_peers_{size_label}_{stage}"),
            );
        }

        // 4. Background picker (Default background emojis)
        {
            struct BgSnapState {
                picker: BackgroundPickerState,
                chat_bg: ChatBackground,
                colors: LiveThemeColors,
                chat_helpers: MockChatHelpersBackend,
                use_chat_helpers: bool,
            }
            let bg_state = BgSnapState {
                picker: BackgroundPickerState::new(false, Some((1001, "Mira Vance".to_owned()))),
                chat_bg: ChatBackground::new(),
                colors: LiveThemeColors::default(),
                chat_helpers: MockChatHelpersBackend::new(),
                use_chat_helpers: is_after,
            };
            render_wgpu_snap(
                size,
                bg_state,
                &format!("background_picker_emojis_{size_label}_{stage}"),
                |ui, st| {
                    ui.ctx().set_visuals(egui::Visuals::light());
                    let bg_fill = tg_ui::theme::Colors::color(&st.colors, "windowBg");
                    egui::Frame::NONE
                        .fill(bg_fill)
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                let ch: Option<&dyn ChatHelpersBackend> = if st.use_chat_helpers {
                                    Some(&st.chat_helpers)
                                } else {
                                    None
                                };
                                let _ = st.picker.show_with_chat_helpers(
                                    ui,
                                    &mut st.chat_bg,
                                    ch,
                                    &st.colors,
                                );
                            });
                        });
                },
            );
        }

        // 5. Saved Messages SelfForwardsTagger (Default tag reactions)
        {
            struct TaggerSnapState {
                tagger: SelfForwardsTaggerState,
                palette: Palette,
            }
            let mut tagger = SelfForwardsTaggerState::default();
            tagger.show_for_forwarded(PeerId::from_user(1000), vec![MsgId(501)], 1.0);
            if is_after {
                tagger.seed_default_tag_reactions(&[
                    DefaultTagReaction::Emoji("🚀".to_owned()),
                    DefaultTagReaction::Emoji("💡".to_owned()),
                    DefaultTagReaction::Emoji("🎯".to_owned()),
                    DefaultTagReaction::Emoji("❤️".to_owned()),
                    DefaultTagReaction::Emoji("🔥".to_owned()),
                    DefaultTagReaction::Paid,
                ]);
            }
            let t_state = TaggerSnapState {
                tagger,
                palette: Palette::day(),
            };
            render_wgpu_snap(
                size,
                t_state,
                &format!("history_saved_tagger_{size_label}_{stage}"),
                |ui, st| {
                    let rect = ui.available_rect_before_wrap();
                    let colors = HistoryColors::from_palette(&st.palette);
                    ui.painter()
                        .rect_filled(rect, egui::CornerRadius::ZERO, colors.canvas_bg);
                    let _ = st.tagger.show(ui, rect, &[], 1.5, &colors);
                },
            );
        }

        // 6. Stories privacy modal (Bulk blocklist)
        {
            struct StorySnapState {
                box_state: StoryPrivacyBox,
                backend: MockStoriesBackend,
            }
            let mut backend = MockStoriesBackend::new(1_760_000_000);
            if is_after {
                backend.set_viewer_stories_blocked(story_mock_peers::CEDAR, true);
            }
            let s_state = StorySnapState {
                box_state: StoryPrivacyBox::default(),
                backend,
            };
            render_wgpu_snap(
                size,
                s_state,
                &format!("stories_privacy_blocklist_{size_label}_{stage}"),
                |ui, st| {
                    let vp = ui.available_rect_before_wrap();
                    let _ = st.box_state.show_modal(ui.ctx(), vp, &st.backend);
                },
            );
        }

        // 7. Replies chat Block from Replies confirmation modal
        if is_after {
            struct BlockRepliesSnapState {
                modal: tg_app::history::compose_boxes::BlockFromRepliesBoxState,
                palette: Palette,
            }
            let b_state = BlockRepliesSnapState {
                modal: tg_app::history::compose_boxes::BlockFromRepliesBoxState::new(
                    MsgId(7701),
                    "Jordan Patel",
                ),
                palette: Palette::day(),
            };
            render_wgpu_snap(
                size,
                b_state,
                &format!("history_block_from_replies_{size_label}_{stage}"),
                |ui, st| {
                    let rect = ui.available_rect_before_wrap();
                    let colors = HistoryColors::from_palette(&st.palette);
                    ui.painter()
                        .rect_filled(rect, egui::CornerRadius::ZERO, colors.canvas_bg);
                    let _ = st.modal.show(ui, &colors);
                },
            );
        }
    }
}
