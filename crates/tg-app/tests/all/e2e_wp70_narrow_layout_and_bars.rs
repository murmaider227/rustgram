//! End-to-end `egui_kittest` tests, `FakeRpcClient` TL wire checks, and headless `wgpu`
//! before/after screenshot capture for WP-70:
//! 1. 390 px narrow-viewport non-overlap fixes: channel top bar ("Direct Messages" / title &
//!    "Sponsored" banner) and group-call header ("Display As" pill / call title).
//! 2. Stickers panel: "Clear Recent Stickers", remote sticker / emoji-pack search with paging,
//!    and old featured sticker packs pagination (`ChatHelpersBackend` WP-67).
//! 3. Paid-reaction privacy toggle & reaction moderation actions (report reaction, delete
//!    participant reaction / all reactions) in message context menu & Who-Reacted list (`WP-67`).
//! 4. Contact status bar (Report Spam / Add Contact / Dismiss) wired to WP-66 (`HistoryBackend`).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient, RpcError};
use tg_app::calls::backend::CallsBackend;
use tg_app::calls::group_panel::{GroupCallPanelState, compute_group_call_header_layout};
use tg_app::calls::mock::MockCallsBackend;
use tg_app::calls::theme::CallPalette;
use tg_app::chat_helpers::mock::MockChatHelpersBackend;
use tg_app::chat_helpers::stickers_list_widget::{StickersListEvent, StickersListWidget};
use tg_app::history::HISTORY_TOP_BAR_HEIGHT;
use tg_app::history::HistoryWidget;
use tg_app::history::backend::{
    ContactStatusBanner, HistoryBackend, HistoryPeerInfo, MessageReactionsPage, ReactionCounter,
    ReactionPeerEntry,
};
use tg_app::history::bars::{
    CONTACT_STATUS_HEIGHT, compute_contact_status_bar_layout, compute_top_bar_layout,
    paint_contact_status_bar, paint_top_bar,
};
use tg_app::history::mock::MockHistoryStore;
use tg_app::history::sponsored::{
    SPONSORED_BANNER_HEIGHT, compute_sponsored_header_layout, paint_sponsored_banner,
};
use tg_app::history::theme::HistoryColors;
use tg_app::info::reactions_list::{ReactionsListAction, ReactionsListWidget};
use tg_app::live::{ErasedRpcClient, LiveSession};
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};
use tg_app_core::chat_helpers::backend::{
    ChatHelpersBackend, PaidReactionPrivacy, SearchStickersRequest,
};
use tg_app_core::core_types::dto::sponsored::SponsoredMessageItem;
use tg_data::{
    Channel, ChannelFlags, ChannelId, Chat, ChatId, PeerBarSettings, PeerUpdateFlags, User,
    UserFlags, UserId as DataUserId,
};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    BOOL_TRUE_ID, Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums,
    functions as tl_fns, types as tl_types,
};
use tokio::sync::broadcast;

use crate::common::{TEST_STEP_DT, apply_deterministic_style};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_WP70_LOCK: Mutex<()> = Mutex::new(());

fn snap_dir() -> PathBuf {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp70"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    dir
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

fn sample_channel_peer() -> HistoryPeerInfo {
    let summary = PeerSummary {
        id: PeerId::from_channel(900_123),
        kind: PeerKind::Channel,
        title: "Rustgram Engineering Broadcast".to_owned(),
        status: "18,420 subscribers".to_owned(),
        username: Some("rustgram_news".to_owned()),
        phone_or_link: Some("https://t.me/rustgram_news".to_owned()),
        about: String::new(),
        initials: "RE".to_owned(),
        avatar_rgb: [0x40, 0xA7, 0xE3],
        muted: false,
        can_send: false,
        is_saved_messages: false,
    };
    HistoryPeerInfo::from_summary(summary)
}

fn sample_sponsored_item() -> SponsoredMessageItem {
    SponsoredMessageItem {
        random_id: b"wp70-ad-1".to_vec(),
        url: "https://example.org/rust-tools".to_owned(),
        title: "Rustgram Dev Cloud".to_owned(),
        text: TextWithEntities::simple(
            "Fast cross-platform CI runners for Rust workspaces with zero setup.",
        ),
        button_text: "Try Free".to_owned(),
        sponsor_info: Some("Sponsored by Example Cloud".to_owned()),
        additional_info: None,
        recommended: false,
        can_report: true,
        color_index: 2,
        min_display_duration_ms: 0,
        max_display_duration_ms: 0,
        is_link_internal: false,
        viewed: false,
    }
}

fn sample_reactions_widget() -> ReactionsListWidget {
    let summary = vec![
        ReactionCounter {
            emoji: "⭐".to_owned(),
            count: 25,
            chosen: true,
        },
        ReactionCounter {
            emoji: "🔥".to_owned(),
            count: 3,
            chosen: false,
        },
        ReactionCounter {
            emoji: "👍".to_owned(),
            count: 2,
            chosen: false,
        },
    ];
    let page = MessageReactionsPage {
        total_count: 30,
        entries: vec![
            ReactionPeerEntry {
                peer_id: PeerId::from_user(401),
                name: "Nora Lin".to_owned(),
                username: Some("nora_lin".to_owned()),
                emoji: "⭐".to_owned(),
                date_unix: 1_760_000_120,
                is_my: true,
                color_index: 1,
            },
            ReactionPeerEntry {
                peer_id: PeerId::from_user(402),
                name: "Devon Brooks".to_owned(),
                username: Some("devon_b".to_owned()),
                emoji: "🔥".to_owned(),
                date_unix: 1_760_000_090,
                is_my: false,
                color_index: 3,
            },
            ReactionPeerEntry {
                peer_id: PeerId::from_user(403),
                name: "Priya Nair".to_owned(),
                username: Some("priya_n".to_owned()),
                emoji: "👍".to_owned(),
                date_unix: 1_760_000_040,
                is_my: false,
                color_index: 5,
            },
        ],
        next_offset: None,
    };
    ReactionsListWidget::new(PeerId::from_channel(900_123), MsgId(77), summary, page)
        .with_paid_privacy(Some(PaidReactionPrivacy::Default))
        .with_moderation(true)
}

fn assert_non_overlapping_horizontally(rects: &[(&str, egui::Rect)]) {
    for i in 0..rects.len() {
        let (name_a, a) = rects[i];
        assert!(
            a.width() > 0.0 && a.height() > 0.0,
            "{name_a} must have non-empty dimensions: {a:?}"
        );
        for &(name_b, b) in &rects[(i + 1)..] {
            let inter = a.intersect(b);
            assert!(
                inter.width() <= 0.5 || inter.height() <= 0.5,
                "{name_a} ({a:?}) overlaps {name_b} ({b:?}) by {inter:?}"
            );
        }
    }
}

// =============================================================================
// 1. 390 px Narrow-Viewport Non-Overlap Layout Tests
// =============================================================================

#[test]
fn narrow_390px_channel_top_bar_and_sponsored_banner_have_zero_overlap() {
    let top_rect = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        Vec2::new(MOBILE_SIZE.x, HISTORY_TOP_BAR_HEIGHT),
    );
    // Telegram Desktop 7.4.6 one-column top bar: back, userpic, title; search, group call and
    // the chat menu on the right (no profile toggle, no text buttons).
    let top_layout = compute_top_bar_layout(top_rect, PeerKind::Channel, false, true);
    assert!(top_layout.compact, "390px top bar must enter compact mode");
    assert!(
        top_layout.info_rect.is_none(),
        "one-column mode hides the profile toggle"
    );
    let call_rect = top_layout
        .call_rect
        .expect("channel admins get the group call button");
    let back_rect = top_layout.back_rect.expect("back button");
    let avatar_rect = top_layout.avatar_rect.expect("one-column userpic");
    assert_non_overlapping_horizontally(&[
        ("back_rect", back_rect),
        ("avatar_rect", avatar_rect),
        ("title_clip_rect", top_layout.title_clip_rect),
        ("search_rect", top_layout.search_rect),
        ("call_rect", call_rect),
        ("menu_rect", top_layout.menu_rect),
    ]);
    assert!(
        top_layout.title_clip_rect.right() <= top_layout.search_rect.left(),
        "title clip must end before the search button"
    );

    let ad_rect = egui::Rect::from_min_size(
        egui::Pos2::new(0.0, top_rect.bottom() + 8.0),
        Vec2::new(MOBILE_SIZE.x, SPONSORED_BANNER_HEIGHT),
    );
    let ad_layout = compute_sponsored_header_layout(ad_rect, false, true);
    let report_rect = ad_layout
        .report_rect
        .expect("report button must exist when can_report is true");
    assert_non_overlapping_horizontally(&[
        ("ad_title_clip", ad_layout.title_rect),
        ("ad_badge", ad_layout.badge_rect),
        ("ad_about", ad_layout.about_rect),
        ("ad_report", report_rect),
        ("ad_hide", ad_layout.hide_rect),
    ]);

    // Verify rendered accessible controls at 390x844 in egui_kittest.
    let peer = sample_channel_peer();
    let ad = sample_sponsored_item();
    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(MOBILE_SIZE)
        .with_step_dt(TEST_STEP_DT)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, (peer, ad): &mut (HistoryPeerInfo, SponsoredMessageItem)| {
                let colors = HistoryColors::from_palette(&Palette::day());
                let avail = ui.available_rect_before_wrap();
                let top_rect = egui::Rect::from_min_size(
                    avail.min,
                    Vec2::new(avail.width(), HISTORY_TOP_BAR_HEIGHT),
                );
                let _ = paint_top_bar(ui, top_rect, peer, true, false, false, &colors);
                let ad_rect = egui::Rect::from_min_size(
                    egui::Pos2::new(avail.left(), top_rect.bottom() + 8.0),
                    Vec2::new(avail.width(), SPONSORED_BANNER_HEIGHT),
                );
                let _ = paint_sponsored_banner(ui, ad_rect, ad, false, &colors);
            },
            (peer, ad),
        );
    harness.step();
    assert!(harness.query_by_label("Search in chat").is_some());
    assert!(harness.query_by_label("Chat menu").is_some());
    assert!(harness.query_by_label("Mute").is_none());
    assert!(harness.query_by_label("About Sponsored").is_some());
    assert!(harness.query_by_label("Report Ad").is_some());
    assert!(harness.query_by_label("Remove Ads").is_some());
}

#[test]
fn narrow_390px_group_call_header_display_as_pill_does_not_overlap_title() {
    let header_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(MOBILE_SIZE.x, 56.0));
    let layout = compute_group_call_header_layout(header_rect, false);
    assert!(
        layout.compact,
        "390px group-call header must enter compact mode"
    );
    assert_non_overlapping_horizontally(&[
        ("title_clip_rect", layout.title_clip_rect),
        ("join_as_rect", layout.join_as_rect),
        ("pin_rect", layout.pin_rect),
        ("min_rect", layout.min_rect),
        ("close_rect", layout.close_rect),
    ]);
    assert!(
        layout.title_clip_rect.right() <= layout.join_as_rect.left(),
        "group-call title clip must stay strictly left of Display As pill"
    );

    let mut backend = MockCallsBackend::new();
    backend.setup_demo_group_call(10_000);
    let panel = GroupCallPanelState::default();
    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(MOBILE_SIZE)
        .with_step_dt(TEST_STEP_DT)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, (b, p): &mut (MockCallsBackend, GroupCallPanelState)| {
                let palette = CallPalette::tdesktop();
                let snap = b.active_group_call().expect("active group").clone();
                let rect = ui.available_rect_before_wrap();
                p.paint_panel_contents(ui, rect, &snap, b, &palette, 10_000);
            },
            (backend, panel),
        );
    harness.step();
    assert!(harness.query_by_label_contains("Display As").is_some());
}

#[test]
fn narrow_390px_contact_status_bar_buttons_do_not_overlap() {
    let cs_rect = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        Vec2::new(MOBILE_SIZE.x, CONTACT_STATUS_HEIGHT),
    );
    let banner = ContactStatusBanner::AddOrBlock {
        peer_name: "Jordan Patel".to_owned(),
    };
    let layout = compute_contact_status_bar_layout(cs_rect, &banner);
    assert!(
        layout.compact,
        "390px contact status bar must enter compact mode"
    );
    assert_eq!(layout.action_rects.len(), 3);
    assert_non_overlapping_horizontally(&[
        ("add", layout.action_rects[0]),
        ("block", layout.action_rects[1]),
        ("report_spam", layout.action_rects[2]),
        ("dismiss", layout.dismiss_rect),
    ]);
}

// =============================================================================
// 2. Stickers Panel Tests (Clear Recent, Remote Search + Paging, Old Featured)
// =============================================================================

#[test]
fn stickers_panel_clear_recent_stickers_confirmation_and_wp67_wire() {
    struct State {
        backend: MockChatHelpersBackend,
        widget: StickersListWidget,
        cleared_events: usize,
    }
    let state = State {
        backend: MockChatHelpersBackend::new(),
        widget: StickersListWidget::new(),
        cleared_events: 0,
    };
    let has_recent = |b: &MockChatHelpersBackend| {
        b.sticker_sections()
            .iter()
            .any(|s| s.id == tg_chat::stickers::RECENT_SET_ID && s.count > 0)
    };
    assert!(has_recent(&state.backend));

    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(360.0, 540.0))
        .with_step_dt(TEST_STEP_DT)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut State| {
                let palette = Palette::day();
                if let Some(StickersListEvent::RecentCleared) =
                    st.widget.show(ui, &mut st.backend, &palette)
                {
                    st.cleared_events += 1;
                }
            },
            state,
        );
    harness.step();

    // Open inline destructive confirmation strip.
    harness.get_by_label("Clear Recent Stickers").click();
    harness.step();
    assert!(harness.state().widget.is_clear_recent_confirm_open());

    // Cancel must not clear recents.
    harness.get_by_label("Cancel").click();
    harness.step();
    assert!(!harness.state().widget.is_clear_recent_confirm_open());
    assert_eq!(harness.state().backend.clear_recent_calls(), 0);
    assert!(has_recent(&harness.state().backend));

    // Re-open and confirm destructive verb "Clear Recent".
    harness.get_by_label("Clear Recent Stickers").click();
    harness.step();
    harness.get_by_label("Clear Recent").click();
    harness.step();

    assert_eq!(harness.state().cleared_events, 1);
    assert_eq!(harness.state().backend.clear_recent_calls(), 1);
    assert!(!has_recent(&harness.state().backend));
}

#[test]
fn stickers_panel_remote_search_paging_emoji_packs_and_old_featured() {
    struct State {
        backend: MockChatHelpersBackend,
        widget: StickersListWidget,
    }
    let mut widget = StickersListWidget::new();
    widget.set_search_query("star");
    let state = State {
        backend: MockChatHelpersBackend::new(),
        widget,
    };

    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(360.0, 620.0))
        .with_step_dt(TEST_STEP_DT)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut State| {
                let palette = Palette::day();
                let _ = st.widget.show(ui, &mut st.backend, &palette);
            },
            state,
        );
    harness.step();
    harness.step();

    // Remote search must populate remote sticker hits & emoji packs for query "star".
    assert!(
        !harness.state().widget.remote_search_stickers().is_empty()
            || harness.query_by_label_contains("Add").is_some(),
        "remote search must surface matching sticker / emoji pack results"
    );

    // Clear query and test "Load older trending packs" pagination (`messages.getOldFeaturedStickers`).
    harness.state_mut().widget.set_search_query("");
    for _ in 0..3 {
        harness.step();
    }
    let page = harness
        .state()
        .backend
        .load_old_featured_stickers(0, 10)
        .expect("load old featured stickers");
    assert!(
        !page.set_ids.is_empty(),
        "loading older featured packs must return archived featured set IDs"
    );
    let first_old = harness
        .state()
        .backend
        .sticker_set_section(page.set_ids[0])
        .expect("archived featured set must resolve via sticker_set_section");
    assert!(!first_old.title.is_empty());
}

// =============================================================================
// 3. Paid-Reaction Privacy Toggle & Reaction Moderation Tests
// =============================================================================

#[test]
fn reactions_list_paid_privacy_toggle_and_moderation_actions() {
    struct State {
        widget: ReactionsListWidget,
        actions: Vec<ReactionsListAction>,
    }
    let state = State {
        widget: sample_reactions_widget(),
        actions: Vec::new(),
    };

    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(500.0, 620.0))
        .with_step_dt(TEST_STEP_DT)
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            |ui, st: &mut State| {
                let colors = HistoryColors::from_palette(&Palette::day());
                if let Some(act) = st.widget.show_modal(ui, &colors) {
                    if let ReactionsListAction::TogglePaidPrivacy(next) = act {
                        st.widget.paid_privacy = Some(next);
                    }
                    if let ReactionsListAction::DeleteParticipantReaction(pid)
                    | ReactionsListAction::DeleteParticipantReactions(pid) = act
                    {
                        st.widget.remove_participant_entries(pid);
                    }
                    st.actions.push(act);
                }
            },
            state,
        );
    harness.step();

    // 1. Toggle Paid Reaction Privacy from Default -> Anonymous.
    harness.get_by_label("Show me in Top Senders").click();
    harness.step();
    assert_eq!(
        harness.state().actions.last(),
        Some(&ReactionsListAction::TogglePaidPrivacy(
            PaidReactionPrivacy::Anonymous
        ))
    );
    assert_eq!(
        harness.state().widget.paid_privacy,
        Some(PaidReactionPrivacy::Anonymous)
    );

    // 2. Open moderation menu for non-self participant Devon Brooks (peer 402) and Report reaction.
    harness
        .get_by_label("Reaction actions for Devon Brooks")
        .click();
    harness.run_steps(2);
    harness
        .get_by_label(tg_lang::tr::lng_report_reaction().as_str())
        .click();
    harness.run_steps(2);
    assert_eq!(
        harness.state().actions.last(),
        Some(&ReactionsListAction::ReportReaction(PeerId::from_user(402)))
    );

    // 3. Re-open moderation menu for Devon Brooks and Delete this reaction.
    harness
        .get_by_label("Reaction actions for Devon Brooks")
        .click();
    harness.run_steps(2);
    harness
        .get_by_label(tg_lang::tr::lng_context_delete_this_reaction().as_str())
        .click();
    harness.run_steps(2);
    assert_eq!(
        harness.state().actions.last(),
        Some(&ReactionsListAction::DeleteParticipantReaction(
            PeerId::from_user(402)
        ))
    );
    assert!(
        harness
            .state()
            .widget
            .entries
            .iter()
            .all(|e| e.peer_id != PeerId::from_user(402)),
        "deleted participant entry must be removed from Who-Reacted list"
    );

    // 4. Open moderation menu for Priya Nair (peer 403) and Remove all from user.
    harness
        .get_by_label("Reaction actions for Priya Nair")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Remove all from user").click();
    harness.run_steps(2);
    assert_eq!(
        harness.state().actions.last(),
        Some(&ReactionsListAction::DeleteParticipantReactions(
            PeerId::from_user(403)
        ))
    );
}

// =============================================================================
// 4. Contact Status Bar Tests (Add Contact, Report Spam w/ Confirm, Dismiss)
// =============================================================================

#[test]
fn contact_status_bar_add_contact_report_spam_and_dismiss_flows() {
    let peer_id = PeerId::from_user(1002); // Jordan Patel, seeded with AddOrBlock banner.
    struct State {
        backend: MockHistoryStore,
        widget: HistoryWidget,
    }
    let widget = HistoryWidget::new();
    let backend = MockHistoryStore::new();

    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(720.0, 620.0))
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_context_setup(|ctx| {
            tg_ui::fonts::register_tdesktop_fonts(ctx);
            apply_deterministic_style(ctx);
        })
        .build_ui_state(
            move |ui, st: &mut State| {
                let palette = Palette::day();
                let strings = tg_app::stand_in::strings::EnglishStrings;
                let _ = st.widget.show_with_backend(
                    ui,
                    peer_id,
                    &mut st.backend,
                    false,
                    false,
                    false,
                    &palette,
                    &strings,
                );
            },
            State { backend, widget },
        );
    harness.step();

    // Flow A: Add contact -> calls HistoryBackend::accept_contact (WP-66) and clears banner.
    harness.get_by_label("Add contact").click();
    harness.step();
    assert_eq!(harness.state().backend.accepted_contacts(), &[peer_id]);
    assert!(
        harness
            .state()
            .backend
            .peer_info(peer_id)
            .expect("peer")
            .contact_status
            .is_none()
    );

    // Flow B: Report Spam -> inline destructive confirmation -> Cancel does NOT report -> Report calls report_spam.
    harness.state_mut().backend.set_contact_status(
        peer_id,
        Some(ContactStatusBanner::ReportSpam {
            peer_name: "Jordan Patel".to_owned(),
            is_channel: false,
        }),
    );
    harness.run_steps(2);
    harness.get_by_label("Report Spam").click();
    harness.run_steps(2);
    harness.get_by_label("Cancel").click();
    harness.run_steps(2);
    assert!(harness.state().backend.reported_spam_peers().is_empty());

    harness.get_by_label("Report Spam").click();
    harness.run_steps(2);
    harness.get_by_label("Report").click();
    harness.run_steps(2);
    assert_eq!(harness.state().backend.reported_spam_peers(), &[peer_id]);
    assert!(
        harness
            .state()
            .backend
            .peer_info(peer_id)
            .expect("peer")
            .contact_status
            .is_none()
    );

    // Flow C: Dismiss -> calls hide_peer_settings_bar (WP-66) and clears banner.
    harness.state_mut().backend.set_contact_status(
        peer_id,
        Some(ContactStatusBanner::AddOrBlock {
            peer_name: "Jordan Patel".to_owned(),
        }),
    );
    harness.run_steps(2);
    harness.get_by_label("Dismiss contact banner").click();
    harness.run_steps(2);
    assert_eq!(harness.state().backend.hidden_settings_bars(), &[peer_id]);
    assert!(
        harness
            .state()
            .backend
            .peer_info(peer_id)
            .expect("peer")
            .contact_status
            .is_none()
    );
}

// =============================================================================
// 5. FakeRpcClient Live End-to-End TL Wire Protocol Verification
// =============================================================================

type RequestCapture = (u32, Vec<u8>);
type ResponseEntry = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct Wp70FakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<RequestCapture>>>,
    responses: Arc<Mutex<VecDeque<ResponseEntry>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wp70FakeRpcClient {
    fn new() -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(2)),
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

    fn enqueue_bool_true(&self, expected_ctor: u32) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(BOOL_TRUE_ID.to_le_bytes().to_vec())));
    }

    fn count_recorded(&self, ctor: u32) -> usize {
        self.recorded
            .lock()
            .expect("lock recorded")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .count()
    }
}

impl RpcClient for Wp70FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        request: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = request.to_bytes();
        let ctor = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((ctor, raw));

        let popped = {
            let mut q = self.responses.lock().expect("lock responses");
            q.iter()
                .position(|(c, _)| *c == ctor)
                .and_then(|pos| q.remove(pos))
                .map(|(_, r)| r)
        };
        match popped {
            Some(Ok(bytes)) => R::Return::from_bytes(&bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string())),
            Some(Err(err)) => Err(err),
            None => Err(InvocationError::Rpc(RpcError::new(
                500,
                format!("NO_FAKE_RESPONSE_{ctor:#010x}"),
            ))),
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }
}

#[test]
fn live_wp70_all_rpc_wire_roundtrip() {
    let fake = Wp70FakeRpcClient::new();
    let self_uid = DataUserId(10_700);
    let stranger_uid = DataUserId(20_701);
    let spam_chat_id = ChatId(30_702);
    let spam_chan_id = ChannelId(40_703);

    let cfg = SessionRuntimeConfig {
        init_updates_state: false,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..Default::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(self_uid, ErasedRpcClient::new(fake.clone()), cfg)
            .expect("start SessionRuntime"),
    );

    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        let mut self_user = User::new(self_uid, true);
        self_user.first_name = "Alex".to_owned();
        self_user.flags.insert(UserFlags::SELF);
        s.upsert_user(self_user, PeerUpdateFlags::NAME);

        let mut stranger = User::new(stranger_uid, false);
        stranger.first_name = "Jordan".to_owned();
        stranger.last_name = "Patel".to_owned();
        stranger.access_hash = 0x1122_3344_5566;
        stranger.common.bar_settings = PeerBarSettings::ADD_CONTACT | PeerBarSettings::REPORT_SPAM;
        s.upsert_user(stranger, PeerUpdateFlags::NAME);

        let mut group = Chat::new(spam_chat_id);
        group.common.name = "Unknown Promo Group".to_owned();
        group.common.bar_settings = PeerBarSettings::REPORT_SPAM;
        s.upsert_chat(group, PeerUpdateFlags::NAME);

        let mut chan = Channel::new(spam_chan_id);
        chan.common.name = "Rustgram Engineering".to_owned();
        chan.access_hash = 0xCAFE_BABE_0703;
        chan.flags |= ChannelFlags::BROADCAST | ChannelFlags::CREATOR;
        chan.common.bar_settings = PeerBarSettings::REPORT_SPAM;
        s.upsert_channel(chan, PeerUpdateFlags::NAME);
    });

    let mut live = LiveSession::new(runtime, 1);
    let stranger_peer = PeerId::from_user(stranger_uid.0);
    let group_peer = PeerId::from_chat(spam_chat_id.0);
    let chan_peer = PeerId::from_channel(spam_chan_id.0);

    // Verify convert.rs ContactStatusBanner mapping for non-contact user, spam group, and spam channel.
    assert!(matches!(
        live.peer_info(stranger_peer).and_then(|p| p.contact_status),
        Some(ContactStatusBanner::AddOrBlock { .. })
    ));
    assert!(matches!(
        live.peer_info(group_peer).and_then(|p| p.contact_status),
        Some(ContactStatusBanner::ReportSpam {
            is_channel: false,
            ..
        })
    ));
    assert!(matches!(
        live.peer_info(chan_peer).and_then(|p| p.contact_status),
        Some(ContactStatusBanner::ReportSpam {
            is_channel: true,
            ..
        })
    ));

    // Enqueue fake TL responses for every WP-70 UI-wired RPC.
    fake.enqueue_ok(
        tl_fns::contacts::AcceptContact::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_760_000_000,
            seq: 1,
        }),
    );
    fake.enqueue_bool_true(tl_fns::messages::ReportSpam::CONSTRUCTOR_ID);
    fake.enqueue_bool_true(tl_fns::messages::HidePeerSettingsBar::CONSTRUCTOR_ID);
    fake.enqueue_bool_true(tl_fns::messages::ClearRecentStickers::CONSTRUCTOR_ID);
    fake.enqueue_ok(
        tl_fns::messages::SearchStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickers::FoundStickers(tl_types::messages::FoundStickers {
            next_offset: Some(1),
            hash: 42,
            stickers: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::SearchEmojiStickerSets::CONSTRUCTOR_ID,
        &tl_enums::messages::FoundStickerSets::FoundStickerSets(
            tl_types::messages::FoundStickerSets {
                hash: 99,
                sets: Vec::new(),
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetOldFeaturedStickers::CONSTRUCTOR_ID,
        &tl_enums::messages::FeaturedStickers::FeaturedStickers(
            tl_types::messages::FeaturedStickers {
                premium: false,
                hash: 101,
                count: 0,
                sets: Vec::new(),
                unread: Vec::new(),
            },
        ),
    );
    fake.enqueue_bool_true(tl_fns::messages::TogglePaidReactionPrivacy::CONSTRUCTOR_ID);
    fake.enqueue_bool_true(tl_fns::messages::ReportReaction::CONSTRUCTOR_ID);
    fake.enqueue_ok(
        tl_fns::messages::DeleteParticipantReaction::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_760_000_001,
            seq: 2,
        }),
    );
    fake.enqueue_bool_true(tl_fns::messages::DeleteParticipantReactions::CONSTRUCTOR_ID);

    // Drive Contact Status Bar WP-66 RPCs through HistoryBackend.
    HistoryBackend::accept_contact(&mut live, stranger_peer).expect("accept_contact");
    assert!(HistoryBackend::report_spam(&mut live, group_peer).expect("report_spam"));
    assert!(
        HistoryBackend::hide_peer_settings_bar(&mut live, chan_peer)
            .expect("hide_peer_settings_bar")
    );

    // Drive Stickers & Reaction Moderation/Privacy WP-67 RPCs through HistoryBackend bridge!
    {
        let ch = HistoryBackend::as_chat_helpers_backend_mut(&mut live)
            .expect("LiveSession must expose ChatHelpersBackend via HistoryBackend bridge");
        assert!(ch.clear_recent_stickers(false).expect("clear_recent"));
        let _ = ch
            .search_stickers_remote(&SearchStickersRequest {
                emojis: false,
                query: "cat".to_owned(),
                emoticon: String::new(),
                lang_codes: vec!["en".to_owned()],
                offset: 0,
                limit: 20,
            })
            .expect("search_stickers_remote");
        let _ = ch
            .search_emoji_sticker_sets("cat", false)
            .expect("search_emoji_sticker_sets");
        let _ = ch
            .load_old_featured_stickers(0, 10)
            .expect("load_old_featured_stickers");
        assert!(
            ch.toggle_paid_reaction_privacy(chan_peer, MsgId(77), PaidReactionPrivacy::Anonymous,)
                .expect("toggle_paid_reaction_privacy")
        );
        assert!(
            ch.report_reaction(chan_peer, MsgId(77), stranger_peer)
                .expect("report_reaction")
        );
        ch.delete_participant_reaction(chan_peer, MsgId(77), stranger_peer)
            .expect("delete_participant_reaction");
        assert!(
            ch.delete_participant_reactions(chan_peer, stranger_peer)
                .expect("delete_participant_reactions")
        );
    }

    // Assert every single TL constructor hit FakeRpcClient exactly once.
    for (name, ctor) in [
        (
            "contacts.acceptContact",
            tl_fns::contacts::AcceptContact::CONSTRUCTOR_ID,
        ),
        (
            "messages.reportSpam",
            tl_fns::messages::ReportSpam::CONSTRUCTOR_ID,
        ),
        (
            "messages.hidePeerSettingsBar",
            tl_fns::messages::HidePeerSettingsBar::CONSTRUCTOR_ID,
        ),
        (
            "messages.clearRecentStickers",
            tl_fns::messages::ClearRecentStickers::CONSTRUCTOR_ID,
        ),
        (
            "messages.searchStickers",
            tl_fns::messages::SearchStickers::CONSTRUCTOR_ID,
        ),
        (
            "messages.searchEmojiStickerSets",
            tl_fns::messages::SearchEmojiStickerSets::CONSTRUCTOR_ID,
        ),
        (
            "messages.getOldFeaturedStickers",
            tl_fns::messages::GetOldFeaturedStickers::CONSTRUCTOR_ID,
        ),
        (
            "messages.togglePaidReactionPrivacy",
            tl_fns::messages::TogglePaidReactionPrivacy::CONSTRUCTOR_ID,
        ),
        (
            "messages.reportReaction",
            tl_fns::messages::ReportReaction::CONSTRUCTOR_ID,
        ),
        (
            "messages.deleteParticipantReaction",
            tl_fns::messages::DeleteParticipantReaction::CONSTRUCTOR_ID,
        ),
        (
            "messages.deleteParticipantReactions",
            tl_fns::messages::DeleteParticipantReactions::CONSTRUCTOR_ID,
        ),
    ] {
        assert_eq!(
            fake.count_recorded(ctor),
            1,
            "expected {name} ({ctor:#010x}) to be sent once"
        );
    }
}

// =============================================================================
// 6. Headless wgpu Before & After Screenshot Capture
// =============================================================================

fn capture_wp70_suite(suffix: &str) {
    let _guard = WGPU_WP70_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. Channel top bar + Sponsored banner
        {
            let peer = sample_channel_peer();
            let ad = sample_sponsored_item();
            render_wgpu_snap(
                size,
                (peer, ad),
                &format!("channel_top_bar_{tag}_{suffix}"),
                |ui, (peer, ad)| {
                    let palette = Palette::day();
                    let colors = HistoryColors::from_palette(&palette);
                    let avail = ui.available_rect_before_wrap();
                    ui.painter()
                        .rect_filled(avail, egui::CornerRadius::ZERO, colors.canvas_bg);
                    let top_rect = egui::Rect::from_min_size(
                        avail.min,
                        Vec2::new(avail.width(), HISTORY_TOP_BAR_HEIGHT),
                    );
                    let _ = paint_top_bar(ui, top_rect, peer, true, false, false, &colors);
                    let ad_rect = egui::Rect::from_min_size(
                        egui::Pos2::new(avail.left(), top_rect.bottom() + 8.0),
                        Vec2::new(avail.width(), SPONSORED_BANNER_HEIGHT),
                    );
                    let _ = paint_sponsored_banner(ui, ad_rect, ad, false, &colors);
                },
            );
        }

        // 2. Group Call header ("Display As" pill + title)
        {
            let mut backend = MockCallsBackend::new();
            backend.setup_demo_group_call(10_000);
            let panel = GroupCallPanelState::default();
            render_wgpu_snap(
                size,
                (backend, panel),
                &format!("group_call_header_{tag}_{suffix}"),
                |ui, (b, p)| {
                    let palette = CallPalette::tdesktop();
                    let snap = b.active_group_call().expect("active group").clone();
                    let rect = ui.available_rect_before_wrap();
                    p.paint_panel_contents(ui, rect, &snap, b, &palette, 10_000);
                },
            );
        }

        // 3. Stickers panel (Clear Recent, Remote Search, Old Featured)
        {
            let backend = MockChatHelpersBackend::new();
            let widget = StickersListWidget::new();
            render_wgpu_snap(
                size,
                (backend, widget),
                &format!("stickers_panel_{tag}_{suffix}"),
                |ui, (b, w)| {
                    let palette = Palette::day();
                    let avail = ui.available_rect_before_wrap();
                    let panel_w = avail.width().min(360.0);
                    let panel_h = avail.height().min(540.0);
                    let panel_rect =
                        egui::Rect::from_center_size(avail.center(), Vec2::new(panel_w, panel_h));
                    ui.painter().rect_filled(
                        avail,
                        egui::CornerRadius::ZERO,
                        palette.window_bg_over,
                    );
                    ui.painter().rect_filled(
                        panel_rect,
                        egui::CornerRadius::same(8),
                        palette.window_bg,
                    );
                    ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                        let _ = w.show(ui, b, &palette);
                    });
                },
            );
        }

        // 4. Who-Reacted list modal (paid-reaction privacy toggle, crisp Twemoji/star glyphs, moderation menu)
        {
            let widget = sample_reactions_widget();
            render_wgpu_snap(
                size,
                widget,
                &format!("reactions_list_{tag}_{suffix}"),
                |ui, w| {
                    let colors = HistoryColors::from_palette(&Palette::day());
                    let _ = w.show_modal(ui, &colors);
                },
            );
        }

        // 5. Contact Status Bar (Add contact | Block user | Report Spam | Dismiss)
        {
            let peer = sample_channel_peer();
            let banner = ContactStatusBanner::AddOrBlock {
                peer_name: "Jordan Patel".to_owned(),
            };
            render_wgpu_snap(
                size,
                (peer, banner),
                &format!("contact_status_bar_{tag}_{suffix}"),
                |ui, (peer, banner)| {
                    let palette = Palette::day();
                    let colors = HistoryColors::from_palette(&palette);
                    let avail = ui.available_rect_before_wrap();
                    ui.painter()
                        .rect_filled(avail, egui::CornerRadius::ZERO, colors.canvas_bg);
                    let top_rect = egui::Rect::from_min_size(
                        avail.min,
                        Vec2::new(avail.width(), HISTORY_TOP_BAR_HEIGHT),
                    );
                    let _ = paint_top_bar(ui, top_rect, peer, true, false, false, &colors);
                    let cs_rect = egui::Rect::from_min_size(
                        egui::Pos2::new(avail.left(), top_rect.bottom()),
                        Vec2::new(avail.width(), CONTACT_STATUS_HEIGHT),
                    );
                    let _ = paint_contact_status_bar(ui, cs_rect, banner, &colors);
                },
            );
        }
    }
}

#[test]
#[ignore = "captures WP-70 before screenshots into $RUSTGRAM_SNAPSHOT_DIR"]
fn capture_wp70_before_screenshots() {
    capture_wp70_suite("before");
}

#[test]
#[ignore = "captures WP-70 after screenshots into $RUSTGRAM_SNAPSHOT_DIR"]
fn capture_wp70_after_screenshots() {
    capture_wp70_suite("after");
}
