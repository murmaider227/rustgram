//! End-to-end `egui_kittest` tests, `FakeRpcClient` live-session verification, and headless
//! `wgpu` before/after screenshot capture for WP-69:
//! - `EditPeer` channel / supergroup management controls (pre-history visibility, forum &
//!   tabs & view-as-messages, hide members, discussion group picker, gigagroup conversion,
//!   boosts to unrestrict, paid-messages price & channel DM, channel autotranslation, group
//!   stickers/emoji, deactivate all collectible usernames, legacy basic group delete)
//! - `EditInviteLinksBox`: Other Admins / Admins with Invites section (`messages.getAdminsWithInvites`)
//! - `PeerQrBox`: Temporary Contact QR token export (`contacts.exportContactToken`)
//! - `InfoWrapWidget`: Delete Contact confirmation, Contact Note editor, Delete Own Photo /
//!   Suggest & Set Contact Photo actions
//! - `AttachedStickersBox`: Attached sticker/mask/emoji sets modal (`messages.getAttachedStickers`)
//! - `tg://contact?token=...&free=...` (`DeepLink::ContactToken`) live resolution via
//!   `contacts.importContactToken` / `contacts.importNoPaidMessagesToken`

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::{Pos2, Rect, Vec2};
use egui_kittest::kittest::Queryable;
use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app::boxes::attached_stickers::{
    AttachedStickersBoxState, AttachedStickersOutcome, classify_attached_stickers,
    resolve_attached_stickers_box,
};
use tg_app::boxes::confirm::ConfirmTag;
use tg_app::boxes::delete_messages::DeleteMessagesBoxState;
use tg_app::boxes::edit_peer::EditPeerInfoBoxState;
use tg_app::boxes::edit_peer_extras::HistoryVisibility;
use tg_app::boxes::invite_links::EditInviteLinksBoxState;
use tg_app::boxes::peer_list::PeerListPurpose;
use tg_app::boxes::peer_qr::PeerQrBoxState;
use tg_app::boxes::sticker_set::StickerSetBoxState;
use tg_app::boxes::{BoxEvent, BoxHost, BoxKind};
use tg_app::chat_helpers::backend::{
    AttachedStickerSetSummary, AttachedStickersMedia, ChatHelpersBackend,
};
use tg_app::core_types::dto::contacts_extras::ContactProfilePhotoUpload;
use tg_app::info::backend::{ChannelStickerSetRef, InfoBackend, InfoPeerKind, ProfilePhotoRef};
use tg_app::info::mock::{MockBackend, mock_peers};
use tg_app::info::{InfoColors, InfoWrapEvent, InfoWrapWidget};
use tg_app::live::LiveSession;
use tg_app::session_api::{StickerSetPreview, StickerSetPreviewItem};
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::settings::ShellSettings;
use tg_app::stand_in::theme::ThemeMode;
use tg_app::stand_in::types::PeerId;
use tg_app::window::controller::WindowController;
use tg_app::window::section::SectionShow;
use tg_chat::{DocumentId, StickerSetId};
use tg_core::settings::{CoreSettings, PowerSavingFlags};
use tg_data::UserId as DataUserId;
use tg_lang::tr;
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_text::TextWithEntities;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::common::{
    AppTestState, build_wgpu_harness, build_widget_harness, init_headless_env, settle_layout,
    step_frames,
};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

static WGPU_WP69_LOCK: Mutex<()> = Mutex::new(());

fn save_wgpu_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, name: &str) {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp69"));
    std::fs::create_dir_all(&dir).expect("create snap dir");
    let img = harness.render().expect("wgpu render");
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("save png");
}

fn build_snap_harness(size: Vec2) -> egui_kittest::Harness<'static, AppTestState> {
    let mut state = AppTestState::main_workspace();
    let static_settings = CoreSettings {
        loop_animated_stickers: false,
        power_saving: PowerSavingFlags::ALL,
        ..CoreSettings::default()
    };
    state
        .controller
        .main_widget_mut()
        .apply_core_settings(&static_settings);
    let mut harness = build_wgpu_harness(state, size);
    settle_layout(&mut harness);
    step_frames(&mut harness, 35);
    harness
        .state_mut()
        .controller
        .main_widget_mut()
        .history_mut()
        .request_scroll_to_bottom(session_peers::GROUP_CORE);
    settle_layout(&mut harness);
    harness
}

fn sample_attached_preview() -> StickerSetPreview {
    StickerSetPreview {
        set_id: 9201,
        access_hash: 77,
        title: "Attached Masks Pack".to_owned(),
        short_name: "attached_masks".to_owned(),
        count: 3,
        is_emoji: false,
        installed: false,
        archived: false,
        official: false,
        items: vec![
            StickerSetPreviewItem {
                document_id: 501,
                alt_emoji: "😎".to_owned(),
                is_animated: false,
                is_video: false,
                is_premium: false,
                thumb_rgba: None,
            },
            StickerSetPreviewItem {
                document_id: 502,
                alt_emoji: "🎭".to_owned(),
                is_animated: true,
                is_video: false,
                is_premium: false,
                thumb_rgba: None,
            },
            StickerSetPreviewItem {
                document_id: 503,
                alt_emoji: "✨".to_owned(),
                is_animated: false,
                is_video: false,
                is_premium: true,
                thumb_rgba: None,
            },
        ],
    }
}

fn sample_attached_sets() -> Vec<AttachedStickerSetSummary> {
    vec![
        AttachedStickerSetSummary {
            set_id: StickerSetId(9201),
            access_hash: 77,
            title: "Aurora Masks".to_owned(),
            short_name: "aurora_masks".to_owned(),
            count: 18,
            is_emoji: false,
            is_masks: true,
            cover_doc_ids: vec![DocumentId(501)],
        },
        AttachedStickerSetSummary {
            set_id: StickerSetId(9202),
            access_hash: 88,
            title: "Signal Reactions".to_owned(),
            short_name: "signal_reactions".to_owned(),
            count: 24,
            is_emoji: true,
            is_masks: false,
            cover_doc_ids: vec![DocumentId(502)],
        },
        AttachedStickerSetSummary {
            set_id: StickerSetId(9203),
            access_hash: 99,
            title: "Nebula Stickers".to_owned(),
            short_name: "nebula_stickers".to_owned(),
            count: 32,
            is_emoji: false,
            is_masks: false,
            cover_doc_ids: vec![DocumentId(503)],
        },
    ]
}

// =============================================================================
// Shared BoxHost & InfoWrap test harnesses
// =============================================================================

struct BoxTestState {
    host: BoxHost,
    session: MockSession,
    colors: InfoColors,
    events: Vec<BoxEvent>,
}

impl BoxTestState {
    fn new() -> Self {
        init_headless_env();
        tg_app::clock::init_test_clock();
        Self {
            host: BoxHost::new(),
            session: MockSession::new(true),
            colors: InfoColors::for_mode(ThemeMode::Day),
            events: Vec::new(),
        }
    }
}

fn build_box_harness(
    initial: BoxTestState,
    size: Vec2,
) -> egui_kittest::Harness<'static, BoxTestState> {
    build_widget_harness(initial, size, |ui, state| {
        let ctx = ui.ctx().clone();
        let bounds = Rect::from_min_size(Pos2::ZERO, ctx.content_rect().size());
        let evs = state
            .host
            .show(&ctx, bounds, &mut state.session, &state.colors);
        state.events.extend(evs);
    })
}

struct InfoTestState {
    wrap: InfoWrapWidget,
    session: MockSession,
    colors: InfoColors,
    events: Vec<InfoWrapEvent>,
}

impl InfoTestState {
    fn for_peer(peer_id: PeerId) -> Self {
        init_headless_env();
        tg_app::clock::init_test_clock();
        let mut wrap = InfoWrapWidget::for_peer(peer_id);
        wrap.show_inline_media = false;
        Self {
            wrap,
            session: MockSession::new(true),
            colors: InfoColors::for_mode(ThemeMode::Day),
            events: Vec::new(),
        }
    }
}

fn build_info_harness(
    initial: InfoTestState,
    size: Vec2,
) -> egui_kittest::Harness<'static, InfoTestState> {
    build_widget_harness(initial, size, |ui, state| {
        egui::CentralPanel::default().show(ui, |ui| {
            let evs = state
                .wrap
                .show(ui, &mut state.session, false, &state.colors);
            state.events.extend(evs);
        });
    })
}

fn step_n<S>(harness: &mut egui_kittest::Harness<'static, S>, n: usize) {
    for _ in 0..n {
        harness.step();
    }
}

/// Scrolls the centered modal's `ScrollArea` until `label` sits comfortably inside the visible
/// viewport, then clicks it. Prevents `egui_kittest` from clicking below-the-fold coordinates
/// that land on the modal backdrop.
fn click_in_scrollable_modal<S>(harness: &mut egui_kittest::Harness<'static, S>, label: &str) {
    let center = harness.ctx.content_rect().center();
    let max_visible_y = center.y + 110.0;
    let min_visible_y = center.y - 110.0;

    for _ in 0..24 {
        let node = harness.get_by_label(label);
        let y = node.rect().center().y;
        if y >= min_visible_y && y <= max_visible_y {
            break;
        }
        let delta_y = (center.y - y).clamp(-140.0, 140.0);
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(center));
        harness.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, delta_y),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        step_n(harness, 3);
    }
    harness.get_by_label(label).click();
}

// =============================================================================
// 1. egui_kittest UI Tests — Every WP-69 Control & Modal
// =============================================================================

#[test]
fn e2e_wp69_edit_peer_group_management_controls() {
    let state = BoxTestState::new();
    // Use GROUP_UI_LAB (standalone supergroup with no linked channel so Pre-History Visibility
    // is shown when Private, matching tdesktop's edit_peer_info_box.cpp rule).
    let group_id = session_peers::GROUP_UI_LAB;

    // Seed an active collectible username on GROUP_UI_LAB so the deactivate-all button is visible.
    state
        .session
        .handle()
        .seed_collectible_username(group_id, "ui_lab_collectible");

    let mut harness = build_box_harness(state, Vec2::new(960.0, 960.0));
    harness.step();

    // Push EditPeerInfoBox for GROUP_UI_LAB (Private so History Visibility row appears).
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        let peer = st.session.peer_profile(group_id).expect("group profile");
        let mut edit = EditPeerInfoBoxState::from_profile(&peer);
        edit.visibility = tg_app::boxes::create_peer::ChannelVisibility::Private;
        st.host.push(&ctx, BoxKind::EditPeerInfo(edit), &st.colors);
    }
    step_n(&mut harness, 6);

    // 1. Pre-History Visibility sub-box (`channels.togglePreHistoryHidden`)
    click_in_scrollable_modal(&mut harness, &tr::lng_manage_history_visibility_title());
    step_n(&mut harness, 6);
    {
        let top = harness.state().host.top_kind().expect("top box");
        assert!(matches!(top, BoxKind::EditHistoryVisibility(_)));
    }
    if let Some(BoxKind::EditHistoryVisibility(hv)) = harness.state_mut().host.top_kind_mut() {
        hv.visibility = HistoryVisibility::Hidden;
    }
    step_n(&mut harness, 3);
    harness
        .get_all_by_label(&tr::lng_settings_save())
        .last()
        .expect("top Save")
        .click();
    step_n(&mut harness, 6);
    assert!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .pre_history_hidden,
        "saving Hidden must persist pre_history_hidden=true"
    );

    // 2. Forum Topics + Tabs + View as Messages (`channels.toggleForum` / `toggleViewForumAsMessages`)
    click_in_scrollable_modal(&mut harness, &tr::lng_forum_topics_switch());
    step_n(&mut harness, 6);
    assert!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .is_forum
    );

    click_in_scrollable_modal(&mut harness, "Show Topics as Tabs");
    step_n(&mut harness, 5);
    assert!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .forum_tabs
    );

    click_in_scrollable_modal(&mut harness, &tr::lng_forum_view_as_messages());
    step_n(&mut harness, 5);
    assert!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .view_forum_as_messages
    );

    // 3. Hide Members (`channels.toggleParticipantsHidden`)
    click_in_scrollable_modal(&mut harness, &tr::lng_profile_hide_participants());
    step_n(&mut harness, 5);
    assert!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .participants_hidden
    );

    // 4. Boosts to Unrestrict (`channels.setBoostsToUnblockRestrictions`)
    let before_boosts = harness
        .state()
        .session
        .channel_management_state(group_id)
        .expect("mgmt")
        .boosts_unrestrict;
    click_in_scrollable_modal(&mut harness, "+");
    step_n(&mut harness, 4);
    assert_eq!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .boosts_unrestrict,
        before_boosts + 1
    );
    click_in_scrollable_modal(&mut harness, "−");
    step_n(&mut harness, 4);
    assert_eq!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .boosts_unrestrict,
        before_boosts
    );

    // 5. Group Stickers & Custom Emoji Pack (`channels.setStickers` / `setEmojiStickers`)
    click_in_scrollable_modal(&mut harness, &tr::lng_group_stickers());
    step_n(&mut harness, 5);
    if let Some(BoxKind::EditGroupStickers(gs)) = harness.state_mut().host.top_kind_mut() {
        gs.sticker_short_name = "team_stickers_v2".to_owned();
        gs.emoji_short_name = "team_emojis_v2".to_owned();
    } else {
        panic!("expected EditGroupStickers sub-box");
    }
    step_n(&mut harness, 3);
    harness
        .get_all_by_label(&tr::lng_settings_save())
        .last()
        .expect("top Save")
        .click();
    step_n(&mut harness, 6);
    {
        let mgmt = harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt");
        assert_eq!(
            mgmt.sticker_set,
            Some(ChannelStickerSetRef::ShortName(
                "team_stickers_v2".to_owned()
            ))
        );
        assert_eq!(
            mgmt.emoji_set,
            Some(ChannelStickerSetRef::ShortName("team_emojis_v2".to_owned()))
        );
    }

    // 6. Paid Messages Price (`channels.updatePaidMessagesPrice`)
    if let Some(BoxKind::EditPeerInfo(edit)) = harness.state_mut().host.top_kind_mut() {
        edit.paid_stars_draft = "25".to_owned();
    }
    step_n(&mut harness, 3);
    click_in_scrollable_modal(&mut harness, "Apply Price");
    step_n(&mut harness, 5);
    assert_eq!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .stars_per_message,
        25
    );

    // 7. Convert to Broadcast Group (`channels.convertToGigagroup`)
    click_in_scrollable_modal(&mut harness, &tr::lng_rights_gigagroup_convert());
    step_n(&mut harness, 5);
    assert!(matches!(
        harness.state().host.top_kind(),
        Some(BoxKind::Confirm(c)) if c.tag == ConfirmTag::ConvertToGigagroup(group_id)
    ));
    harness
        .get_by_label(&tr::lng_gigagroup_convert_sure())
        .click();
    step_n(&mut harness, 5);
    assert!(
        harness
            .state()
            .session
            .channel_management_state(group_id)
            .expect("mgmt")
            .is_gigagroup
    );

    // 8. Deactivate All Collectible Usernames (`channels.deactivateAllUsernames`)
    click_in_scrollable_modal(&mut harness, "Deactivate All Collectible Usernames (1)");
    step_n(&mut harness, 5);
    assert!(matches!(
        harness.state().host.top_kind(),
        Some(BoxKind::Confirm(c)) if c.tag == ConfirmTag::DeactivateAllUsernames(group_id)
    ));
    harness.get_by_label("Deactivate All").click();
    step_n(&mut harness, 5);
    let after_usernames = harness
        .state()
        .session
        .peer_profile(group_id)
        .expect("group")
        .usernames;
    assert!(
        after_usernames
            .iter()
            .filter(|u| !u.is_editable)
            .all(|u| !u.is_active),
        "all collectible usernames must be deactivated"
    );

    // 9. Legacy Basic Group Delete (`messages.deleteChat`) via DeleteMessagesBoxState
    let mut legacy_profile = harness
        .state()
        .session
        .peer_profile(group_id)
        .expect("group profile")
        .clone();
    legacy_profile.id = PeerId::from_chat(4242);
    legacy_profile.kind = InfoPeerKind::Group;
    legacy_profile.title = "Legacy Study Group".to_owned();
    let mut del_box = DeleteMessagesBoxState::for_delete_or_leave(&legacy_profile, false);
    del_box.revoke_for_everyone = true;
    let deleted = del_box
        .submit(&mut harness.state_mut().session)
        .expect("delete legacy basic group");
    assert_eq!(deleted, 0);
}

#[test]
fn e2e_wp69_edit_peer_channel_discussion_autotranslate_and_dm() {
    let state = BoxTestState::new();
    let channel_id = session_peers::CHANNEL_DEV_LOG;
    let mut harness = build_box_harness(state, Vec2::new(960.0, 960.0));
    harness.step();

    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        let peer = st
            .session
            .peer_profile(channel_id)
            .expect("channel profile");
        st.host.push(
            &ctx,
            BoxKind::EditPeerInfo(EditPeerInfoBoxState::from_profile(&peer)),
            &st.colors,
        );
    }
    step_n(&mut harness, 6);

    // 1. Discussion Group Picker (`channels.getGroupsForDiscussion` + `set_linked_discussion`)
    let candidates = harness
        .state()
        .session
        .groups_for_discussion()
        .expect("groups_for_discussion");
    assert!(
        !candidates.is_empty(),
        "discussion group picker must receive candidates from groups_for_discussion()"
    );
    click_in_scrollable_modal(&mut harness, &tr::lng_manage_discussion_group());
    step_n(&mut harness, 5);
    {
        let top = harness.state().host.top_kind().expect("peer list");
        assert!(matches!(
            top,
            BoxKind::PeerList(pl) if matches!(pl.purpose, PeerListPurpose::LinkDiscussion { .. })
        ));
    }
    let chosen_group = candidates[0].peer_id;
    if let Some(BoxKind::PeerList(pl)) = harness.state_mut().host.top_kind_mut() {
        pl.selected = vec![chosen_group];
    }
    step_n(&mut harness, 3);
    harness.get_by_label(&tr::lng_settings_save()).click();
    step_n(&mut harness, 5);
    assert_eq!(
        harness
            .state()
            .session
            .peer_profile(channel_id)
            .expect("channel")
            .linked_discussion_peer
            .map(|(id, _)| id),
        Some(chosen_group)
    );

    // 2. Channel Autotranslation (`channels.toggleAutotranslation`)
    let before_auto = harness
        .state()
        .session
        .channel_management_state(channel_id)
        .expect("mgmt")
        .autotranslation;
    click_in_scrollable_modal(&mut harness, &tr::lng_edit_autotranslate());
    step_n(&mut harness, 5);
    assert_eq!(
        harness
            .state()
            .session
            .channel_management_state(channel_id)
            .expect("mgmt")
            .autotranslation,
        !before_auto
    );

    // 3. Direct Messages to Channel & Stars Price (`channels.updatePaidMessagesPrice`)
    let before_dm = harness
        .state()
        .session
        .channel_management_state(channel_id)
        .expect("mgmt")
        .broadcast_messages_allowed;
    click_in_scrollable_modal(&mut harness, "Allow Direct Messages to Channel");
    step_n(&mut harness, 5);
    assert_eq!(
        harness
            .state()
            .session
            .channel_management_state(channel_id)
            .expect("mgmt")
            .broadcast_messages_allowed,
        !before_dm
    );

    if let Some(BoxKind::EditPeerInfo(edit)) = harness.state_mut().host.top_kind_mut() {
        edit.paid_stars_draft = "15".to_owned();
    }
    step_n(&mut harness, 3);
    click_in_scrollable_modal(&mut harness, "Apply Price");
    step_n(&mut harness, 5);
    assert_eq!(
        harness
            .state()
            .session
            .channel_management_state(channel_id)
            .expect("mgmt")
            .stars_per_message,
        15
    );
}

#[test]
fn e2e_wp69_invite_links_other_admins_and_peer_qr_contact_token() {
    let state = BoxTestState::new();
    let group_id = session_peers::GROUP_CORE;
    let mut harness = build_box_harness(state, Vec2::new(900.0, 760.0));
    harness.step();

    // 1. EditInviteLinksBox loads & renders Other Admins (`messages.getAdminsWithInvites`)
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host.push(
            &ctx,
            BoxKind::EditInviteLinks(EditInviteLinksBoxState::new(group_id, "Rustgram Core Port")),
            &st.colors,
        );
    }
    step_n(&mut harness, 6);
    {
        let Some(BoxKind::EditInviteLinks(il)) = harness.state().host.top_kind() else {
            panic!("expected EditInviteLinks box");
        };
        assert!(
            il.admins_with_invites.len() >= 2,
            "Other Admins section must be populated via messages.getAdminsWithInvites"
        );
        assert!(
            il.admins_with_invites.iter().all(|a| !a.is_self),
            "Other Admins list must filter out self"
        );
    }

    // Pop invite links and push PeerQrBox for a private user without username
    {
        let st = harness.state_mut();
        st.host.pop(&ctx, &st.colors);
        let mut peer = st
            .session
            .peer_profile(session_peers::USER_MIRA)
            .expect("user profile");
        peer.usernames.clear();
        peer.invite_link = None;
        let qr = PeerQrBoxState::from_profile_with_backend(&peer, &st.session);
        assert!(qr.is_contact_token);
        assert!(qr.link.starts_with("https://t.me/contact/"));
        assert!(qr.token_expires_unix.is_some());
        st.host.push(&ctx, BoxKind::PeerQr(qr), &st.colors);
    }
    step_n(&mut harness, 6);

    let first_url = match harness.state().host.top_kind() {
        Some(BoxKind::PeerQr(qr)) => qr.link.clone(),
        _ => panic!("expected PeerQr box"),
    };
    harness.get_by_label("Refresh Contact QR").click();
    step_n(&mut harness, 5);
    let refreshed_url = match harness.state().host.top_kind() {
        Some(BoxKind::PeerQr(qr)) => qr.link.clone(),
        _ => panic!("expected PeerQr box"),
    };
    assert_ne!(
        first_url, refreshed_url,
        "clicking Refresh Contact QR must call contacts.exportContactToken and rotate the URL"
    );
}

#[test]
fn e2e_wp69_profile_contact_note_delete_contact_and_photos() {
    let mira = session_peers::USER_MIRA;
    let mut harness = build_info_harness(InfoTestState::for_peer(mira), Vec2::new(420.0, 1400.0));
    step_n(&mut harness, 6);

    // 1. Contact Note view -> edit -> save -> delete (`contacts.updateContactNote`)
    assert!(harness.state().session.contact_note(mira).is_some());
    click_profile_menu_item(&mut harness, "Contact Note");
    step_n(&mut harness, 4);
    // Update the inline editor text and click "Save Note"
    harness
        .state_mut()
        .wrap
        .set_contact_note_draft_for_test("Prefers async reviews on Tuesdays.");
    step_n(&mut harness, 3);
    harness.get_by_label("Save Note").click();
    step_n(&mut harness, 5);
    assert_eq!(
        harness.state().session.contact_note(mira).map(|n| n.text),
        Some("Prefers async reviews on Tuesdays.".to_owned())
    );

    click_profile_menu_item(&mut harness, "Contact Note");
    step_n(&mut harness, 4);
    harness.get_by_label("Delete Note").click();
    step_n(&mut harness, 5);
    assert!(
        harness.state().session.contact_note(mira).is_none(),
        "Delete Note must clear the note via contacts.updateContactNote"
    );

    // 2. Suggest & Set Personal Contact Photo (`photos.uploadContactProfilePhoto`)
    click_profile_menu_item(&mut harness, "Suggest Profile Photo");
    step_n(&mut harness, 4);
    harness.get_by_label("Send Photo Suggestion").click();
    step_n(&mut harness, 4);
    assert!(harness.state().events.iter().any(|ev| matches!(
        ev,
        InfoWrapEvent::Toast(msg) if msg.contains("Suggested profile photo")
    )));

    click_profile_menu_item(&mut harness, "Suggest Profile Photo");
    step_n(&mut harness, 4);
    harness
        .get_by_label(&tr::lng_profile_set_photo_for())
        .click();
    step_n(&mut harness, 4);
    harness.get_by_label("Set Personal Photo").click();
    step_n(&mut harness, 4);
    assert!(harness.state().events.iter().any(|ev| matches!(
        ev,
        InfoWrapEvent::Toast(msg) if msg.contains("Personal profile photo set")
    )));

    // Reset personal photo
    click_profile_menu_item(&mut harness, "Suggest Profile Photo");
    step_n(&mut harness, 4);
    harness.get_by_label("Reset Personal Photo").click();
    step_n(&mut harness, 4);
    assert!(harness.state().events.iter().any(|ev| matches!(
        ev,
        InfoWrapEvent::Toast(msg) if msg.contains("Personal photo cleared")
    )));

    // 3. Delete Contact confirmation (`contacts.deleteContacts`)
    assert!(
        harness
            .state()
            .session
            .peer_profile(mira)
            .expect("mira")
            .is_contact
    );
    click_profile_menu_item(&mut harness, &tr::lng_info_delete_contact());
    step_n(&mut harness, 4);
    harness.get_by_label("Confirm Delete Contact").click();
    step_n(&mut harness, 5);
    assert!(
        !harness
            .state()
            .session
            .peer_profile(mira)
            .expect("mira")
            .is_contact,
        "Confirm Delete Contact must invoke contacts.deleteContacts and update is_contact"
    );

    // 4. Delete Own Profile Photo (`photos.deletePhotos`) on Saved Messages (own profile):
    //    WP-72 requires a real server `ProfilePhotoRef` (non-zero `id` and `access_hash`) before
    //    enabling the action and sending `photos.deletePhotos`.
    let mut self_harness = build_info_harness(
        InfoTestState::for_peer(session_peers::SELF),
        Vec2::new(420.0, 1400.0),
    );
    self_harness
        .state_mut()
        .wrap
        .set_active_profile_photo_ref_override(
            session_peers::SELF,
            Some(ProfilePhotoRef {
                id: 9_100_001,
                access_hash: 0x1122_3344_5566,
                file_reference: vec![0xAA, 0xBB],
            }),
        );
    step_n(&mut self_harness, 6);
    click_profile_menu_item(&mut self_harness, "Delete Profile Photo");
    step_n(&mut self_harness, 4);
    self_harness.get_by_label("Confirm Delete Photo").click();
    step_n(&mut self_harness, 5);
    assert!(self_harness.state().events.iter().any(|ev| matches!(
        ev,
        InfoWrapEvent::Toast(msg) if msg.contains("Profile photo deleted")
    )));
}

#[test]
fn e2e_wp69_attached_stickers_modal_zero_one_and_multi() {
    let session = MockSession::new(true);

    // 0 sets -> NotFound
    let zero_media = AttachedStickersMedia::Photo {
        id: 0,
        access_hash: 0,
        file_reference: Vec::new(),
    };
    let zero_res = resolve_attached_stickers_box(&zero_media, "Empty Photo", &session, |_| None)
        .expect("resolve zero");
    assert!(matches!(zero_res, AttachedStickersOutcome::NotFound));

    // 1 set -> SingleSet directly
    let single = sample_attached_sets().into_iter().take(1).collect();
    let single_res = classify_attached_stickers(single, "Single Mask Photo", |_| None);
    assert!(matches!(single_res, AttachedStickersOutcome::SingleSet(_)));

    // >1 sets -> MultipleSets chooser modal
    let multi_media = AttachedStickersMedia::Photo {
        id: 777,
        access_hash: 123,
        file_reference: Vec::new(),
    };
    let multi_res =
        resolve_attached_stickers_box(&multi_media, "Profile Photo", &session, |_| None)
            .expect("resolve multi");
    let AttachedStickersOutcome::MultipleSets(list_box) = multi_res else {
        panic!("expected MultipleSets outcome for mock photo id=777");
    };
    assert!(list_box.sets.len() >= 2);

    // Render AttachedStickersBox in BoxHost and click a pack row
    let mut harness = build_box_harness(BoxTestState::new(), Vec2::new(900.0, 720.0));
    let ctx = harness.ctx.clone();
    {
        let st = harness.state_mut();
        st.host
            .push(&ctx, BoxKind::AttachedStickers(list_box), &st.colors);
    }
    step_n(&mut harness, 6);
    harness
        .get_by_label("Aurora Masks (Masks, 18 items)")
        .click();
    step_n(&mut harness, 4);
    assert!(
        harness.state().events.iter().any(|ev| matches!(
            ev,
            BoxEvent::OpenAttachedStickerSet { short_name, .. } if short_name == "aurora_masks"
        )),
        "clicking an attached set row must emit BoxEvent::OpenAttachedStickerSet"
    );
}

#[test]
fn e2e_wp69_contact_token_deep_link_mock_and_errors() {
    init_headless_env();
    tg_app::clock::init_test_clock();
    let temp = TempDir::new().expect("tempdir");
    let mut ctrl = WindowController::with_session(
        MockSession::new(true),
        ShellSettings::default(),
        temp.path().join("settings.json"),
    );

    // 1. Valid contact token + free fee-waiver token opens chat history (NOT AddContactBoxState!)
    ctrl.open_link("tg://contact?token=abc123&free=waiver77");
    assert!(
        !matches!(ctrl.box_host().top_kind(), Some(BoxKind::AddContact(_))),
        "tg://contact?token= must NOT fall back to blank AddContactBoxState"
    );
    assert_eq!(
        ctrl.session().expect("session").active_peer(),
        session_peers::USER_MIRA
    );
    assert!(
        ctrl.layer_stack()
            .toast_text()
            .is_some_and(|t| t.contains("Paid message fee waived")),
        "expected fee-waiver confirmation toast"
    );

    // 2. Expired token -> lng_contact_link_invalid toast
    ctrl.open_link("tg://contact?token=expired");
    assert_eq!(
        ctrl.layer_stack().toast_text(),
        Some(tr::lng_contact_link_invalid().as_str())
    );

    // 3. Flood-limited token -> lng_flood_error toast
    ctrl.open_link("tg://contact?token=flood");
    assert_eq!(
        ctrl.layer_stack().toast_text(),
        Some(tr::lng_flood_error().as_str())
    );
}

// =============================================================================
// 2. FakeRpcClient Live-Session Tests — Exact TL Constructor Verification
// =============================================================================

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i32::MAX as u64) as i32)
        .unwrap_or(1_760_000_000)
}

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct Wp69FakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    invoked_ctors: Arc<Mutex<Vec<u32>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wp69FakeRpc {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            invoked_ctors: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }

    fn enqueue_err(&self, ctor: u32, err: InvocationError) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Err(err)));
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

impl RpcClient for Wp69FakeRpc {
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

fn synthetic_user(
    id: i64,
    first_name: &str,
    username: &str,
    is_self: bool,
    paid_stars: Option<i64>,
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
        access_hash: Some(id * 100 + 7),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Tester".to_owned()),
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
        send_paid_messages_stars: paid_stars,
        linked_community_id: None,
    }
}

fn synthetic_megagroup(id: i64, title: &str) -> tl_types::Channel {
    tl_types::Channel {
        creator: true,
        left: false,
        broadcast: false,
        verified: false,
        megagroup: true,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: false,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(id * 19 + 5),
        title: title.to_owned(),
        username: Some("wp69_megagroup".to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: now_unix_i32(),
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(250),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
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

fn boot_live_wp69(
    initial_users: Vec<tl_enums::User>,
    initial_chats: Vec<tl_enums::Chat>,
) -> (
    LiveSession,
    Wp69FakeRpc,
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
) {
    let now = now_unix_i32();
    let self_uid = 810_001i64;
    let fake = Wp69FakeRpc::new(2);
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(synthetic_user(
            self_uid,
            "Self",
            "self_wp69",
            true,
            None,
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
            chats: initial_chats,
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
fn live_wp69_contact_token_deep_link_dispatches_import_and_fee_waiver_rpcs() {
    let (live, fake, runtime) = boot_live_wp69(Vec::new(), Vec::new());
    let temp = TempDir::new().expect("tempdir");
    let mut ctrl = WindowController::with_session(
        live,
        ShellSettings::default(),
        temp.path().join("settings.json"),
    );

    let resolved_uid = 810_042i64;
    let priced_user = synthetic_user(resolved_uid, "Lyra", "lyra_tok", false, Some(50));
    let waived_user = synthetic_user(resolved_uid, "Lyra", "lyra_tok", false, None);

    fake.enqueue_ok(
        tl_fns::contacts::ImportContactToken::CONSTRUCTOR_ID,
        &tl_enums::User::User(priced_user),
    );
    fake.enqueue_ok(
        tl_fns::contacts::ImportNoPaidMessagesToken::CONSTRUCTOR_ID,
        &tl_enums::User::User(waived_user),
    );

    ctrl.open_link("tg://contact?token=tok_live_42&free=waiver_99");

    assert_eq!(
        fake.invoked_count(tl_fns::contacts::ImportContactToken::CONSTRUCTOR_ID),
        1,
        "must dispatch contacts.importContactToken"
    );
    assert_eq!(
        fake.invoked_count(tl_fns::contacts::ImportNoPaidMessagesToken::CONSTRUCTOR_ID),
        1,
        "must dispatch contacts.importNoPaidMessagesToken when &free= is present"
    );
    assert_eq!(
        ctrl.session().expect("session").active_peer(),
        PeerId::from_user(resolved_uid as u64)
    );
    assert!(
        !matches!(ctrl.box_host().top_kind(), Some(BoxKind::AddContact(_))),
        "must not open blank AddContactBoxState"
    );
    assert!(
        ctrl.layer_stack()
            .toast_text()
            .is_some_and(|t| t.contains("Paid message fee waived")),
        "expected fee-waiver toast"
    );

    // Also test RPC error mapping for FLOOD_WAIT
    fake.enqueue_err(
        tl_fns::contacts::ImportContactToken::CONSTRUCTOR_ID,
        InvocationError::Rpc(tg_api::RpcError {
            code: 420,
            name: "FLOOD_WAIT_30".to_owned(),
            value: Some(30),
        }),
    );
    ctrl.open_link("tg://contact?token=rate_limited");
    assert_eq!(
        ctrl.layer_stack().toast_text(),
        Some(tr::lng_flood_error().as_str())
    );

    drop(ctrl);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}

#[test]
fn live_wp69_channel_contacts_and_attached_stickers_rpcs() {
    let contact_uid = 810_077i64;
    let group_cid = 910_055i64;
    let (mut live, fake, runtime) = boot_live_wp69(
        vec![tl_enums::User::User(synthetic_user(
            contact_uid,
            "Soren",
            "soren_live",
            false,
            None,
        ))],
        vec![tl_enums::Chat::Channel(synthetic_megagroup(
            group_cid,
            "WP69 Live Supergroup",
        ))],
    );
    let group_peer = PeerId::from_channel(group_cid as u64);
    let contact_peer = PeerId::from_user(contact_uid as u64);

    // Enqueue exact TL responses for every WP-69 RPC path
    fake.enqueue_ok(
        tl_fns::channels::TogglePreHistoryHidden::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::channels::ToggleForum::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::channels::ToggleViewForumAsMessages::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::channels::ToggleParticipantsHidden::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::channels::ToggleAutotranslation::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::channels::SetBoostsToUnblockRestrictions::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::channels::UpdatePaidMessagesPrice::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::channels::GetGroupsForDiscussion::CONSTRUCTOR_ID,
        &tl_enums::messages::Chats::Chats(tl_types::messages::Chats {
            chats: vec![tl_enums::Chat::Channel(synthetic_megagroup(
                group_cid,
                "WP69 Live Supergroup",
            ))],
        }),
    );
    fake.enqueue_ok(tl_fns::channels::SetStickers::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::channels::ConvertToGigagroup::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::channels::DeactivateAllUsernames::CONSTRUCTOR_ID,
        &true,
    );
    fake.enqueue_ok(
        tl_fns::messages::GetAdminsWithInvites::CONSTRUCTOR_ID,
        &tl_enums::messages::ChatAdminsWithInvites::ChatAdminsWithInvites(
            tl_types::messages::ChatAdminsWithInvites {
                admins: vec![tl_enums::ChatAdminWithInvites::ChatAdminWithInvites(
                    tl_types::ChatAdminWithInvites {
                        admin_id: contact_uid,
                        invites_count: 3,
                        revoked_invites_count: 1,
                    },
                )],
                users: vec![tl_enums::User::User(synthetic_user(
                    contact_uid,
                    "Soren",
                    "soren_live",
                    false,
                    None,
                ))],
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::contacts::ExportContactToken::CONSTRUCTOR_ID,
        &tl_enums::ExportedContactToken::ExportedContactToken(tl_types::ExportedContactToken {
            url: "https://t.me/contact/live_qr_token_01".to_owned(),
            expires: now_unix_i32() + 900,
        }),
    );
    fake.enqueue_ok(tl_fns::contacts::UpdateContactNote::CONSTRUCTOR_ID, &true);
    fake.enqueue_ok(
        tl_fns::contacts::DeleteContacts::CONSTRUCTOR_ID,
        &empty_updates(),
    );
    fake.enqueue_ok(
        tl_fns::photos::DeletePhotos::CONSTRUCTOR_ID,
        &vec![99_001i64],
    );
    fake.enqueue_ok(
        tl_fns::photos::UploadContactProfilePhoto::CONSTRUCTOR_ID,
        &tl_enums::photos::Photo::Photo(tl_types::photos::Photo {
            photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
            users: Vec::new(),
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID,
        &vec![
            tl_enums::StickerSetCovered::StickerSetCovered(tl_types::StickerSetCovered {
                set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                    archived: false,
                    official: false,
                    masks: true,
                    emojis: false,
                    text_color: false,
                    channel_emoji_status: false,
                    creator: false,
                    installed_date: None,
                    id: 9201,
                    access_hash: 77,
                    title: "Live Masks".to_owned(),
                    short_name: "live_masks".to_owned(),
                    thumbs: None,
                    thumb_dc_id: None,
                    thumb_version: None,
                    thumb_document_id: None,
                    count: 10,
                    hash: 1,
                }),
                cover: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 501 }),
            }),
            tl_enums::StickerSetCovered::StickerSetCovered(tl_types::StickerSetCovered {
                set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                    archived: false,
                    official: false,
                    masks: false,
                    emojis: true,
                    text_color: false,
                    channel_emoji_status: false,
                    creator: false,
                    installed_date: None,
                    id: 9202,
                    access_hash: 88,
                    title: "Live Emojis".to_owned(),
                    short_name: "live_emojis".to_owned(),
                    thumbs: None,
                    thumb_dc_id: None,
                    thumb_version: None,
                    thumb_document_id: None,
                    count: 15,
                    hash: 2,
                }),
                cover: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 502 }),
            }),
        ],
    );

    // Invoke all WP-69 backend calls on LiveSession
    InfoBackend::toggle_pre_history_hidden(&mut live, group_peer, true).expect("prehistory");
    InfoBackend::toggle_channel_forum(&mut live, group_peer, true, true).expect("forum");
    InfoBackend::toggle_view_forum_as_messages(&mut live, group_peer, true).expect("view msgs");
    InfoBackend::toggle_participants_hidden(&mut live, group_peer, true).expect("hide members");
    InfoBackend::toggle_channel_autotranslation(&mut live, group_peer, true)
        .expect("autotranslate");
    InfoBackend::set_boosts_to_unblock_restrictions(&mut live, group_peer, 3).expect("boosts");
    InfoBackend::update_channel_paid_messages_price(&mut live, group_peer, true, 20)
        .expect("paid price");
    let groups = InfoBackend::groups_for_discussion(&live).expect("discussion groups");
    assert_eq!(groups.len(), 1);
    InfoBackend::set_channel_sticker_set(
        &mut live,
        group_peer,
        ChannelStickerSetRef::ShortName("live_group_pack".to_owned()),
    )
    .expect("set stickers");
    InfoBackend::convert_to_gigagroup(&mut live, group_peer).expect("gigagroup");
    InfoBackend::deactivate_all_channel_usernames(&mut live, group_peer).expect("deactivate");
    let admins = InfoBackend::admins_with_invites(&live, group_peer).expect("admins with invites");
    assert_eq!(admins.len(), 1);
    let qr_token = InfoBackend::export_contact_token(&live).expect("export contact token");
    assert_eq!(qr_token.url, "https://t.me/contact/live_qr_token_01");
    InfoBackend::update_contact_note(
        &mut live,
        contact_peer,
        TextWithEntities::simple("Live test note"),
    )
    .expect("update contact note");
    InfoBackend::delete_contact(&mut live, contact_peer).expect("delete contact");
    let deleted_photos = InfoBackend::delete_profile_photos(
        &mut live,
        &[ProfilePhotoRef {
            id: 99_001,
            access_hash: 11,
            file_reference: Vec::new(),
        }],
    )
    .expect("delete profile photos");
    assert_eq!(deleted_photos, vec![99_001]);
    let _ = InfoBackend::upload_contact_profile_photo(
        &mut live,
        contact_peer,
        ContactProfilePhotoUpload::clear_personal(),
    )
    .expect("clear personal photo");
    let attached = ChatHelpersBackend::get_attached_stickers(
        &live,
        &AttachedStickersMedia::Photo {
            id: 5001,
            access_hash: 99,
            file_reference: Vec::new(),
        },
    )
    .expect("get attached stickers");
    assert_eq!(attached.len(), 2);

    // Assert every exact TL constructor was sent over RpcClient
    for (name, ctor) in [
        (
            "channels.togglePreHistoryHidden",
            tl_fns::channels::TogglePreHistoryHidden::CONSTRUCTOR_ID,
        ),
        (
            "channels.toggleForum",
            tl_fns::channels::ToggleForum::CONSTRUCTOR_ID,
        ),
        (
            "channels.toggleViewForumAsMessages",
            tl_fns::channels::ToggleViewForumAsMessages::CONSTRUCTOR_ID,
        ),
        (
            "channels.toggleParticipantsHidden",
            tl_fns::channels::ToggleParticipantsHidden::CONSTRUCTOR_ID,
        ),
        (
            "channels.toggleAutotranslation",
            tl_fns::channels::ToggleAutotranslation::CONSTRUCTOR_ID,
        ),
        (
            "channels.setBoostsToUnblockRestrictions",
            tl_fns::channels::SetBoostsToUnblockRestrictions::CONSTRUCTOR_ID,
        ),
        (
            "channels.updatePaidMessagesPrice",
            tl_fns::channels::UpdatePaidMessagesPrice::CONSTRUCTOR_ID,
        ),
        (
            "channels.getGroupsForDiscussion",
            tl_fns::channels::GetGroupsForDiscussion::CONSTRUCTOR_ID,
        ),
        (
            "channels.setStickers",
            tl_fns::channels::SetStickers::CONSTRUCTOR_ID,
        ),
        (
            "channels.convertToGigagroup",
            tl_fns::channels::ConvertToGigagroup::CONSTRUCTOR_ID,
        ),
        (
            "channels.deactivateAllUsernames",
            tl_fns::channels::DeactivateAllUsernames::CONSTRUCTOR_ID,
        ),
        (
            "messages.getAdminsWithInvites",
            tl_fns::messages::GetAdminsWithInvites::CONSTRUCTOR_ID,
        ),
        (
            "contacts.exportContactToken",
            tl_fns::contacts::ExportContactToken::CONSTRUCTOR_ID,
        ),
        (
            "contacts.updateContactNote",
            tl_fns::contacts::UpdateContactNote::CONSTRUCTOR_ID,
        ),
        (
            "contacts.deleteContacts",
            tl_fns::contacts::DeleteContacts::CONSTRUCTOR_ID,
        ),
        (
            "photos.deletePhotos",
            tl_fns::photos::DeletePhotos::CONSTRUCTOR_ID,
        ),
        (
            "photos.uploadContactProfilePhoto",
            tl_fns::photos::UploadContactProfilePhoto::CONSTRUCTOR_ID,
        ),
        (
            "messages.getAttachedStickers",
            tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID,
        ),
    ] {
        assert_eq!(
            fake.invoked_count(ctor),
            1,
            "expected {name} to be invoked exactly once"
        );
    }

    drop(live);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}

// =============================================================================
// 3. Headless wgpu Before & After Screenshot Generators (1440x900 & 390x844)
// =============================================================================

#[test]
#[ignore = "generates WP-69 before screenshots under $RUSTGRAM_SNAPSHOT_DIR"]
fn capture_wp69_before_screenshots() {
    let _guard = WGPU_WP69_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mock = MockBackend::new();
    let colors = LiveThemeColors::default();

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. EditPeerInfoBox for supergroup
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let peer = mock
                .peer_profile(mock_peers::GROUP_CORE)
                .expect("group profile");
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EditPeerInfo(EditPeerInfoBoxState::from_profile(&peer)),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("edit_peer_group_{tag}_before"));
        }

        // 2. EditPeerInfoBox for broadcast channel
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let peer = mock
                .peer_profile(mock_peers::CHANNEL_RELEASES)
                .expect("channel profile");
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EditPeerInfo(EditPeerInfoBoxState::from_profile(&peer)),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("edit_peer_channel_{tag}_before"));
        }

        // 3. EditInviteLinksBox
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EditInviteLinks(EditInviteLinksBoxState::new(
                    session_peers::GROUP_CORE,
                    "Rustgram Core Port",
                )),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("invite_links_{tag}_before"));
        }

        // 4. PeerQrBox (private user without username)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let mut peer = mock
                .peer_profile(mock_peers::USER_MIRA)
                .expect("user profile");
            peer.usernames.clear();
            peer.invite_link = None;
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::PeerQr(PeerQrBoxState::from_profile(&peer)),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("peer_qr_{tag}_before"));
        }

        // 5. Peer Profile (Contact)
        {
            let mut h = build_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_info(session_peers::USER_MIRA, SectionShow::clear_stack());
            }
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("peer_profile_contact_{tag}_before"));
        }

        // 6. Attached Stickers (before: single StickerSetBox fallback)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::StickerSet(StickerSetBoxState::from_preview(sample_attached_preview())),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("attached_stickers_{tag}_before"));
        }
    }
}

fn scroll_center_down(harness: &mut egui_kittest::Harness<'static, AppTestState>, steps: usize) {
    let center = harness.ctx.content_rect().center();
    for _ in 0..steps {
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(center));
        harness.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -110.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        settle_layout(harness);
    }
}

#[test]
#[ignore = "generates WP-69 after screenshots under $RUSTGRAM_SNAPSHOT_DIR"]
fn capture_wp69_after_screenshots() {
    let _guard = WGPU_WP69_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mock = MockBackend::new();
    let colors = LiveThemeColors::default();

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. EditPeerInfoBox for supergroup (scrolled to showcase WP-69 management controls)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let peer = mock
                .peer_profile(mock_peers::GROUP_CORE)
                .expect("group profile");
            let mut edit = EditPeerInfoBoxState::from_profile(&peer);
            edit.visibility = tg_app::boxes::create_peer::ChannelVisibility::Private;
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EditPeerInfo(edit),
                &colors,
            );
            settle_layout(&mut h);
            scroll_center_down(&mut h, 6);
            save_wgpu_png(&mut h, &format!("edit_peer_group_{tag}_after"));
        }

        // 2. EditPeerInfoBox for broadcast channel (scrolled to discussion, autotranslate, paid messages, DM)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let peer = mock
                .peer_profile(mock_peers::CHANNEL_RELEASES)
                .expect("channel profile");
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EditPeerInfo(EditPeerInfoBoxState::from_profile(&peer)),
                &colors,
            );
            settle_layout(&mut h);
            scroll_center_down(&mut h, 5);
            save_wgpu_png(&mut h, &format!("edit_peer_channel_{tag}_after"));
        }

        // 3. EditInviteLinksBox (with Other Admins / Admins with Invites section visible)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EditInviteLinks(EditInviteLinksBoxState::new(
                    session_peers::GROUP_CORE,
                    "Rustgram Core Port",
                )),
                &colors,
            );
            settle_layout(&mut h);
            scroll_center_down(&mut h, 2);
            save_wgpu_png(&mut h, &format!("invite_links_{tag}_after"));
        }

        // 4. PeerQrBox (private user without username -> temporary contact QR token export)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let mut peer = mock
                .peer_profile(mock_peers::USER_MIRA)
                .expect("user profile");
            peer.usernames.clear();
            peer.invite_link = None;
            let qr = PeerQrBoxState::from_profile_with_backend(&peer, &mock);
            h.state_mut()
                .controller
                .box_host_mut()
                .push(&ctx, BoxKind::PeerQr(qr), &colors);
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("peer_qr_{tag}_after"));
        }

        // 5. Peer Profile (Contact with Contact Note banner + Suggest Photo + Delete Contact)
        {
            let mut h = build_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_info(session_peers::USER_MIRA, SectionShow::clear_stack());
            }
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("peer_profile_contact_{tag}_after"));
        }

        // 6. AttachedStickersBox (multi-pack chooser modal from messages.getAttachedStickers)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::AttachedStickers(AttachedStickersBoxState::new(
                    "Profile Photo",
                    sample_attached_sets(),
                )),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("attached_stickers_{tag}_after"));
        }
    }
}

/// UI parity (Package D): rows the 7.4.6 profile no longer lists live in the `⋮` menu.
fn click_profile_menu_item<S>(harness: &mut egui_kittest::Harness<'static, S>, label: &str) {
    harness.get_by_label("More actions").click();
    for _ in 0..3 {
        harness.step();
    }
    harness.get_by_label(label).click();
}
