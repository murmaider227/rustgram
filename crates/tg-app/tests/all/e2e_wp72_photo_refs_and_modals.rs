//! End-to-end `egui_kittest` tests, `FakeRpcClient` live-session verification, and headless
//! `wgpu` before/after screenshot capture for WP-72:
//! 1. Media Viewer `"Attached Stickers"` action for photos/videos with stickers/masks
//!    (`crates/tg-app-media/src/media/overlay.rs` + `WindowController::open_attached_stickers_modal`).
//! 2. `EditInviteLinksBoxState` at `390 x 844`: `"+ Create a New Link"` and `"Delete All Revoked"`
//!    pills keep positive clearance (`>= 10 px`) from the modal scroll-area right edge / scrollbar.
//! 3. `PeerQrBox` free-messages token export (`contacts.exportNoPaidMessagesToken`) and
//!    `EmojiUserpicBox` server default emoji catalog (`ChatHelpersBackend::default_emoji_catalog`).
//! 4. `InfoWrapWidget` real profile-photo ref resolution (`DeleteOwnProfilePhoto`,
//!    `ReportProfilePhoto`, `ViewAttachedStickers` — disabled when no real ref is known, and
//!    passing exact `(id, access_hash, file_reference)` when known; never `peer_id.bare()` / `0`).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::{Pos2, Vec2};
use egui_kittest::kittest::Queryable;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_app::boxes::emoji_userpic::EmojiUserpicBoxState;
use tg_app::boxes::invite_links::EditInviteLinksBoxState;
use tg_app::boxes::peer_qr::{FreeMessagesCodeMode, PeerQrBoxState};
use tg_app::boxes::{BoxEvent, BoxHost, BoxKind};
use tg_app::chat_helpers::backend::{
    AttachedStickersMedia, ChatHelpersBackend, DefaultEmojiCatalogKind,
};
use tg_app::info::backend::{InfoBackend, ProfilePhotoRef};
use tg_app::info::mock::{MockBackend, mock_peers};
use tg_app::info::{InfoColors, InfoWrapEvent, InfoWrapWidget};
use tg_app::live::LiveSession;
use tg_app::media::backend::{MediaItem, MediaItemId, MediaItemKind};
use tg_app::media::mock::{
    MOCK_CHAT_PEER_ID, MOCK_PHOTO_LANDSCAPE_ID, MOCK_PHOTO_PORTRAIT_ID, MockMediaBackend,
};
use tg_app::media::overlay::{MediaOverlayWidget, OverlayEvent};
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::theme::ThemeMode;
use tg_app::stand_in::types::PeerId;
use tg_app::window::section::SectionShow;
use tg_chat::DocumentId;
use tg_core::settings::{CoreSettings, PowerSavingFlags};
use tg_data::{
    Message as DataMessage, MessageContent as DataMessageContent, MessageMedia as DataMessageMedia,
    MsgId as DataMsgId, PeerId as DataPeerId, PhotoData, PhotoId, PhotoSize,
    Session as DataSession, UserId as DataUserId,
};
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

static WGPU_WP72_LOCK: Mutex<()> = Mutex::new(());

fn save_wgpu_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, name: &str) {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp72"));
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
    step_frames(&mut harness, 20);
    harness
}

fn click_pos(harness: &mut egui_kittest::Harness<'static, AppTestState>, pos: Pos2) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    settle_layout(harness);
}

fn step_n<S>(harness: &mut egui_kittest::Harness<'static, S>, count: usize) {
    for _ in 0..count {
        harness.step();
    }
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
    build_widget_harness(initial, size, |ui, st| {
        let viewport = ui.ctx().content_rect();
        let evs = st
            .host
            .show(ui.ctx(), viewport, &mut st.session, &st.colors);
        st.events.extend(evs);
    })
}

struct InfoTestState {
    widget: InfoWrapWidget,
    backend: MockBackend,
    colors: InfoColors,
    events: Vec<InfoWrapEvent>,
}

impl InfoTestState {
    fn for_peer(peer_id: PeerId) -> Self {
        init_headless_env();
        tg_app::clock::init_test_clock();
        Self {
            widget: InfoWrapWidget::for_peer(peer_id),
            backend: MockBackend::new(),
            colors: InfoColors::for_mode(ThemeMode::Day),
            events: Vec::new(),
        }
    }
}

fn build_info_harness(
    initial: InfoTestState,
    size: Vec2,
) -> egui_kittest::Harness<'static, InfoTestState> {
    build_widget_harness(initial, size, |ui, st| {
        let evs = st.widget.show(ui, &mut st.backend, false, &st.colors);
        st.events.extend(evs);
    })
}

// =============================================================================
// 1. Media Viewer "Attached Stickers" (Photo & Video) + WindowController modal
// =============================================================================

#[test]
fn wp72_media_viewer_attached_stickers_photo_and_video_kittest() {
    let media_mock = MockMediaBackend::new();

    // A. Full WindowController E2E: photo WITH attached_stickers_media opens AttachedStickers modal.
    {
        let mut h = build_snap_harness(Vec2::new(1024.0, 768.0));
        h.state_mut()
            .controller
            .media_overlay_mut()
            .open_item(
                &media_mock,
                MOCK_CHAT_PEER_ID,
                MOCK_PHOTO_LANDSCAPE_ID,
                1_000,
            )
            .expect("open mock photo with attached stickers");
        step_frames(&mut h, 6);
        h.get_by_label("More").click();
        step_frames(&mut h, 6);

        let attached_lbl = tr::lng_context_attached_stickers();
        assert!(
            h.query_by_label(&attached_lbl).is_some(),
            "Media Viewer More menu must show '{}' when item has attached stickers",
            attached_lbl
        );
        h.get_by_label(&attached_lbl).click();
        step_frames(&mut h, 6);

        assert!(
            !h.state().controller.media_overlay().is_open(),
            "Clicking Attached Stickers must close the fullscreen overlay"
        );
        assert!(
            matches!(
                h.state().controller.box_host().top_kind(),
                Some(BoxKind::AttachedStickers(_))
            ),
            "WindowController must route OverlayEvent::AttachedStickers into AttachedStickersBox"
        );
    }

    // B. Photo WITHOUT attached stickers: "Attached Stickers" must NOT appear in More menu.
    {
        let mut h = build_snap_harness(Vec2::new(1024.0, 768.0));
        h.state_mut()
            .controller
            .media_overlay_mut()
            .open_item(
                &media_mock,
                MOCK_CHAT_PEER_ID,
                MOCK_PHOTO_PORTRAIT_ID,
                1_000,
            )
            .expect("open mock photo without stickers");
        step_frames(&mut h, 6);
        h.get_by_label("More").click();
        step_frames(&mut h, 6);
        let attached_lbl = tr::lng_context_attached_stickers();
        assert!(
            h.query_by_label(&attached_lbl).is_none(),
            "Media Viewer More menu must hide '{}' when item has no attached stickers",
            attached_lbl
        );
    }

    // C. Video item WITH real AttachedStickersMedia::Document ref emits the exact Document ref.
    {
        struct SingleVideoBackend {
            item: MediaItem,
        }
        impl tg_app::media::backend::MediaBackend for SingleVideoBackend {
            fn get_item(&self, id: MediaItemId) -> Option<MediaItem> {
                (id == self.item.id).then(|| self.item.clone())
            }
            fn siblings_in_chat(&self, _peer_id: i64, _item_id: MediaItemId) -> Vec<MediaItem> {
                vec![self.item.clone()]
            }
            fn load_bytes(&self, _id: MediaItemId) -> anyhow::Result<Vec<u8>> {
                anyhow::bail!("no photo bytes for video")
            }
            fn open_range_loader(
                &self,
                _id: MediaItemId,
            ) -> anyhow::Result<Arc<dyn tg_video::RangeLoader>> {
                anyhow::bail!("synthetic video without ffmpeg stream")
            }
            fn save_to_disk(
                &self,
                _id: MediaItemId,
                dest: &std::path::Path,
            ) -> anyhow::Result<PathBuf> {
                Ok(dest.to_path_buf())
            }
            fn copy_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
                Ok(())
            }
            fn forward_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
                Ok(())
            }
            fn delete_item(&self, _id: MediaItemId) -> anyhow::Result<()> {
                Ok(())
            }
            fn show_in_chat(&self, _peer_id: i64, _msg_id: i32) {}
            fn open_in_external_app(&self, _id: MediaItemId) -> anyhow::Result<PathBuf> {
                Ok(PathBuf::from("clip.mp4"))
            }
            fn audio_track_metadata(
                &self,
                _id: tg_audio::AudioMsgId,
            ) -> Option<tg_app::media::backend::AudioTrackMetadata> {
                None
            }
            fn audio_data_source(&self) -> Arc<dyn tg_audio::AudioDataSource> {
                MockMediaBackend::new().audio_data_source()
            }
            fn playlist_provider(&self) -> Arc<dyn tg_audio::PlaylistProvider> {
                MockMediaBackend::new().playlist_provider()
            }
        }

        let expected_ref = AttachedStickersMedia::Document {
            id: 990_010,
            access_hash: 0x7777_0010,
            file_reference: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };
        let video_backend = SingleVideoBackend {
            item: MediaItem {
                id: MediaItemId(990_010),
                peer_id: MOCK_CHAT_PEER_ID,
                msg_id: 2042,
                kind: MediaItemKind::Video,
                width: 1280,
                height: 720,
                duration_ms: 4_000,
                file_size: 1024,
                file_name: "masked_clip.mp4".to_owned(),
                mime_type: "video/mp4".to_owned(),
                sender_name: "Mira Vance".to_owned(),
                date_text: "today at 20:00".to_owned(),
                caption: TextWithEntities::default(),
                rotation_degrees: 0,
                has_alpha: false,
                allows_forward: true,
                allows_delete: false,
                allows_save: true,
                local_path: None,
                grouped_id: None,
                attached_stickers_media: Some(expected_ref.clone()),
            },
        };

        let mut overlay = MediaOverlayWidget::new();
        overlay
            .open_item(
                &video_backend,
                MOCK_CHAT_PEER_ID,
                MediaItemId(990_010),
                1_000,
            )
            .expect("open synthetic video item");
        let mut h = build_widget_harness(
            (overlay, video_backend, Vec::<OverlayEvent>::new()),
            Vec2::new(960.0, 720.0),
            |ui, (ov, bk, evs)| {
                let vp = ui.ctx().content_rect();
                evs.extend(ov.show(ui.ctx(), vp, bk, 1_500));
            },
        );
        step_n(&mut h, 3);
        h.get_by_label("More").click();
        step_n(&mut h, 3);
        h.get_by_label(&tr::lng_context_attached_stickers()).click();
        step_n(&mut h, 3);

        let attached_ev = h.state().2.iter().find_map(|e| match e {
            OverlayEvent::AttachedStickers { media, media_label } => {
                Some((media.clone(), media_label.clone()))
            }
            _ => None,
        });
        assert_eq!(
            attached_ev,
            Some((expected_ref, "masked_clip.mp4".to_owned())),
            "Video item Attached Stickers must emit its exact Document ref"
        );
    }
}

// =============================================================================
// 2. Invite Links at 390 x 844: "+ Create a New Link" & "Delete All Revoked" clearance
// =============================================================================

#[test]
fn wp72_invite_links_390px_scrollbar_clearance_kittest() {
    let mut h = build_box_harness(BoxTestState::new(), MOBILE_SIZE);
    let ctx = h.ctx.clone();
    {
        let st = h.state_mut();
        st.host.push(
            &ctx,
            BoxKind::EditInviteLinks(EditInviteLinksBoxState::new(
                session_peers::GROUP_CORE,
                "Rustgram Core Port",
            )),
            &st.colors,
        );
    }
    step_n(&mut h, 10);

    // At 390 px, BoxModal width is clamped to `viewport.width() - 24 = 366`, centered at x=195,
    // so box right = 378 and scroll area right edge = 378 - content_padding.right (22) = 356.
    // Close button / modal box layout gives an exact upper bound for the scroll area right edge.
    let viewport_w = MOBILE_SIZE.x;
    let clamped_box_w = (viewport_w - 24.0).min(384.0 + 32.0);
    let box_right = viewport_w * 0.5 + clamped_box_w * 0.5;
    let scroll_right = box_right - 22.0; // BoxStyle::default_box content_padding.right = 22.0

    let create_rect = h.get_by_label("+ Create a New Link").rect();
    let delete_revoked_rect = h.get_by_label("Delete All Revoked").rect();

    let create_clearance = scroll_right - create_rect.max.x;
    let delete_clearance = scroll_right - delete_revoked_rect.max.x;
    assert!(
        create_clearance >= 10.0,
        "'+ Create a New Link' must have >= 10 px right scrollbar clearance at 390px, got {create_clearance:.1} px (pill right={}, scroll right={})",
        create_rect.max.x,
        scroll_right
    );
    assert!(
        delete_clearance >= 10.0,
        "'Delete All Revoked' must have >= 10 px right scrollbar clearance at 390px, got {delete_clearance:.1} px (pill right={}, scroll right={})",
        delete_revoked_rect.max.x,
        scroll_right
    );
}

// =============================================================================
// 3. PeerQrBox `contacts.exportNoPaidMessagesToken` & EmojiUserpicBox default catalog
// =============================================================================

#[test]
fn wp72_peer_qr_export_no_paid_messages_token_kittest() {
    let mut h = build_box_harness(BoxTestState::new(), Vec2::new(560.0, 760.0));
    let ctx = h.ctx.clone();
    {
        let st = h.state_mut();
        let peer = st
            .session
            .peer_profile(session_peers::USER_MIRA)
            .expect("mira profile");
        let qr = PeerQrBoxState::from_profile_with_backend(&peer, &st.session);
        assert_eq!(qr.free_code_mode, FreeMessagesCodeMode::Fee);
        assert_eq!(qr.link, "https://t.me/mira_vance");
        st.host.push(&ctx, BoxKind::PeerQr(qr), &st.colors);
    }
    step_n(&mut h, 8);

    // 1. Click "Allow Free Messages" -> exports temporary fee-waiver token (`revoke=false, temporary=true`)
    h.get_by_label("Allow Free Messages").click();
    step_n(&mut h, 6);
    {
        let Some(BoxKind::PeerQr(qr)) = h.state().host.top_kind() else {
            panic!("PeerQrBox should stay open");
        };
        assert_eq!(qr.free_code_mode, FreeMessagesCodeMode::Temporary);
        assert!(
            qr.link.contains("free=free_active_tmp"),
            "Expected temporary free-message link, got {}",
            qr.link
        );
        assert!(qr.matrix.is_some());
    }

    // 2. Click "Disable timer" -> exports permanent fee-waiver token (`revoke=false, temporary=false`)
    h.get_by_label(&tr::lng_no_paid_token_menu_disable_timer())
        .click();
    step_n(&mut h, 6);
    {
        let Some(BoxKind::PeerQr(qr)) = h.state().host.top_kind() else {
            panic!("PeerQrBox should stay open");
        };
        assert_eq!(qr.free_code_mode, FreeMessagesCodeMode::Permanent);
        assert!(
            qr.link.contains("free=free_active_perm"),
            "Expected permanent free-message link, got {}",
            qr.link
        );
    }

    // 3. Click "Reset Code" -> revokes & mints replacement (`revoke=true, temporary=false`)
    h.get_by_label(&tr::lng_no_paid_token_menu_reset()).click();
    step_n(&mut h, 6);
    {
        let Some(BoxKind::PeerQr(qr)) = h.state().host.top_kind() else {
            panic!("PeerQrBox should stay open");
        };
        assert_eq!(qr.free_code_mode, FreeMessagesCodeMode::Permanent);
        assert!(
            qr.link.contains("free=free_revoked_perm"),
            "Expected revoked/reset permanent free-message link, got {}",
            qr.link
        );
    }

    // 4. Click "Restore message fee" -> returns to original plain profile QR link
    h.get_by_label(&tr::lng_no_paid_token_menu_restore())
        .click();
    step_n(&mut h, 6);
    {
        let Some(BoxKind::PeerQr(qr)) = h.state().host.top_kind() else {
            panic!("PeerQrBox should stay open");
        };
        assert_eq!(qr.free_code_mode, FreeMessagesCodeMode::Fee);
        assert_eq!(qr.link, "https://t.me/mira_vance");
    }
}

#[test]
fn wp72_emoji_userpic_default_emoji_catalog_kittest() {
    let session = MockSession::new(true);

    // 1. User profile -> ProfilePhoto catalog
    let user_box = EmojiUserpicBoxState::with_default_catalog(
        session_peers::SELF,
        "Taylor Kim",
        DefaultEmojiCatalogKind::ProfilePhoto,
        &session,
    );
    assert!(user_box.catalog_loaded);
    assert_eq!(user_box.catalog_kind, DefaultEmojiCatalogKind::ProfilePhoto);
    assert!(
        !user_box.suggested_emojis.is_empty(),
        "ProfilePhoto catalog must populate suggested_emojis"
    );
    let second_profile_id = user_box.suggested_emojis[1].emoji_id;
    let second_profile_label = user_box.suggested_emojis[1].label.clone();

    // 2. Group -> GroupPhoto catalog returns distinct group default emojis
    let group_box = EmojiUserpicBoxState::with_default_catalog(
        session_peers::GROUP_CORE,
        "Rustgram Core Port",
        DefaultEmojiCatalogKind::GroupPhoto,
        &session,
    );
    assert!(group_box.catalog_loaded);
    assert_eq!(group_box.catalog_kind, DefaultEmojiCatalogKind::GroupPhoto);
    assert!(!group_box.suggested_emojis.is_empty());
    assert_ne!(
        user_box.suggested_emojis[0].emoji_id, group_box.suggested_emojis[0].emoji_id,
        "ProfilePhoto and GroupPhoto catalogs must query distinct server catalogs"
    );

    // 3. Render in BoxHost, click second suggested emoji tile, and submit -> uploads exact doc ID.
    let mut h = build_box_harness(BoxTestState::new(), Vec2::new(540.0, 760.0));
    let ctx = h.ctx.clone();
    {
        let st = h.state_mut();
        st.host
            .push(&ctx, BoxKind::EmojiUserpic(user_box), &st.colors);
    }
    step_n(&mut h, 8);
    h.get_by_label(&second_profile_label).click();
    step_n(&mut h, 4);
    {
        let Some(BoxKind::EmojiUserpic(b)) = h.state().host.top_kind() else {
            panic!("EmojiUserpicBox must be open");
        };
        assert_eq!(b.emoji_id, second_profile_id);
        let req = b.build_request().expect("valid request");
        assert_eq!(req.emoji_id, second_profile_id);
    }
    h.get_by_label("Set Photo").click();
    step_n(&mut h, 6);
    assert!(
        h.state()
            .events
            .iter()
            .any(|ev| matches!(ev, BoxEvent::PeerUpdated(pid) if *pid == session_peers::SELF)),
        "Submitting EmojiUserpicBox must emit PeerUpdated"
    );
}

// =============================================================================
// 4. InfoWrapWidget real ProfilePhotoRef wiring & disabled guard when None
// =============================================================================

#[test]
fn wp72_profile_photo_actions_guard_and_exact_ref_passthrough_kittest() {
    // A. Without a real ProfilePhotoRef (default until WP-71 lands on master):
    //    Delete / Report actions are disabled and emit zero bogus `peer_id.bare()` RPCs.
    {
        let mut self_h = build_info_harness(
            InfoTestState::for_peer(mock_peers::SAVED_MESSAGES),
            Vec2::new(420.0, 1400.0),
        );
        step_n(&mut self_h, 6);
        click_profile_menu_item(&mut self_h, "Delete Profile Photo");
        step_n(&mut self_h, 5);
        assert!(
            self_h.query_by_label("Confirm Delete Photo").is_none(),
            "Delete Profile Photo must stay disabled when no real ProfilePhotoRef is loaded"
        );
        assert!(
            self_h
                .state()
                .backend
                .peer_mgmt
                .deleted_profile_photos
                .is_empty(),
            "Zero photos.deletePhotos RPCs may fire without a real ProfilePhotoRef"
        );

        // Even an invalid placeholder ref (`access_hash == 0`) must be rejected.
        self_h
            .state_mut()
            .widget
            .set_active_profile_photo_ref_override(
                mock_peers::SAVED_MESSAGES,
                Some(ProfilePhotoRef {
                    id: mock_peers::SAVED_MESSAGES.bare() as i64,
                    access_hash: 0,
                    file_reference: Vec::new(),
                }),
            );
        step_n(&mut self_h, 4);
        click_profile_menu_item(&mut self_h, "Delete Profile Photo");
        step_n(&mut self_h, 4);
        assert!(
            self_h.query_by_label("Confirm Delete Photo").is_none(),
            "Placeholder ref with access_hash == 0 must still keep Delete Profile Photo disabled"
        );
    }

    // B. With a real ProfilePhotoRef: DeleteOwnProfilePhoto passes exact `(id, access_hash, file_reference)`.
    let real_self_photo = ProfilePhotoRef {
        id: 77_001,
        access_hash: 0x1234_5678_9ABC,
        file_reference: vec![9, 8, 7],
    };
    {
        let mut self_h = build_info_harness(
            InfoTestState::for_peer(mock_peers::SAVED_MESSAGES),
            Vec2::new(420.0, 1400.0),
        );
        self_h
            .state_mut()
            .widget
            .set_active_profile_photo_ref_override(
                mock_peers::SAVED_MESSAGES,
                Some(real_self_photo.clone()),
            );
        step_n(&mut self_h, 6);
        click_profile_menu_item(&mut self_h, "Delete Profile Photo");
        step_n(&mut self_h, 4);
        self_h.get_by_label("Confirm Delete Photo").click();
        step_n(&mut self_h, 5);
        assert_eq!(
            self_h.state().backend.peer_mgmt.deleted_profile_photos,
            vec![77_001],
            "DeleteOwnProfilePhoto must send the exact real photo.id (never peer_id.bare())"
        );
    }

    // C. ReportProfilePhoto: disabled when None, and passes exact ProfilePhotoRef when present.
    let real_mira_photo = ProfilePhotoRef {
        id: 88_002,
        access_hash: 0x0FED_CBA9_8765,
        file_reference: vec![1, 3, 5, 7],
    };
    {
        let mut mira_h = build_info_harness(
            InfoTestState::for_peer(mock_peers::USER_MIRA),
            Vec2::new(420.0, 1800.0),
        );
        step_n(&mut mira_h, 6);
        click_profile_menu_item(&mut mira_h, "Report Profile Photo");
        step_n(&mut mira_h, 4);
        assert!(
            mira_h
                .state()
                .backend
                .profile_extras
                .reported_profile_photos
                .is_empty(),
            "Report Profile Photo must be disabled without a real ref"
        );

        // Now provide a real ProfilePhotoRef and submit report.
        mira_h
            .state_mut()
            .widget
            .set_active_profile_photo_ref_override(
                mock_peers::USER_MIRA,
                Some(real_mira_photo.clone()),
            );
        step_n(&mut mira_h, 4);
        click_profile_menu_item(&mut mira_h, "Report Profile Photo");
        step_n(&mut mira_h, 4);
        mira_h.get_by_label("Submit Photo Report").click();
        step_n(&mut mira_h, 5);
        let reported = &mira_h
            .state()
            .backend
            .profile_extras
            .reported_profile_photos;
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0].0, mock_peers::USER_MIRA);
        assert_eq!(reported[0].1, real_mira_photo);
    }

    // D. Cover context menu "Attached Stickers" emits exact real AttachedStickersMedia::Photo.
    {
        let mut mira_h = build_info_harness(
            InfoTestState::for_peer(mock_peers::USER_MIRA),
            Vec2::new(420.0, 900.0),
        );
        mira_h
            .state_mut()
            .widget
            .set_active_profile_photo_ref_override(
                mock_peers::USER_MIRA,
                Some(real_mira_photo.clone()),
            );
        step_n(&mut mira_h, 6);
        // Right-click avatar center: the 7.4.6 cover centers the 80 px photo 24 px below the
        // top of the panel (CentralPanel margin 8 -> x = 210, y = 8 + 24 + 40).
        let av_pos = Pos2::new(210.0, 72.0);
        mira_h
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(av_pos));
        mira_h.input_mut().events.push(egui::Event::PointerButton {
            pos: av_pos,
            button: egui::PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        mira_h.step();
        mira_h.input_mut().events.push(egui::Event::PointerButton {
            pos: av_pos,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        step_n(&mut mira_h, 4);
        mira_h
            .get_by_label(&tr::lng_stickers_attached_sets())
            .click();
        step_n(&mut mira_h, 4);
        let emitted = mira_h.state().events.iter().find_map(|ev| match ev {
            InfoWrapEvent::OpenAttachedStickers { media, .. } => Some(media.clone()),
            _ => None,
        });
        assert_eq!(
            emitted,
            Some(AttachedStickersMedia::Photo {
                id: real_mira_photo.id,
                access_hash: real_mira_photo.access_hash,
                file_reference: real_mira_photo.file_reference,
            }),
            "Cover Attached Stickers must emit the exact real ProfilePhotoRef"
        );
    }
}

// =============================================================================
// 5. FakeRpcClient live-session end-to-end verification
// =============================================================================

fn now_unix_i32() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i32)
        .unwrap_or(1_760_000_000)
}

type QueuedResp = (u32, Result<Vec<u8>, InvocationError>);
type RecordedReq = (u32, Vec<u8>);

#[derive(Clone)]
struct Wp72FakeRpc {
    main_dc: Arc<Mutex<DcId>>,
    responses: Arc<Mutex<VecDeque<QueuedResp>>>,
    invoked: Arc<Mutex<Vec<RecordedReq>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl Wp72FakeRpc {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(32);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            invoked: Arc::new(Mutex::new(Vec::new())),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((ctor, Ok(resp.to_bytes())));
    }

    fn requests_for(&self, ctor: u32) -> Vec<Vec<u8>> {
        self.invoked
            .lock()
            .expect("lock invoked")
            .iter()
            .filter(|(c, _)| *c == ctor)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

impl RpcClient for Wp72FakeRpc {
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
        self.invoked
            .lock()
            .expect("lock invoked")
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

fn synthetic_self_user(id: i64) -> tl_types::User {
    tl_types::User {
        self_: true,
        contact: false,
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
        premium: true,
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
        access_hash: Some(id * 31 + 7),
        first_name: Some("Self".to_owned()),
        last_name: None,
        username: Some("self_wp72".to_owned()),
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

fn boot_live_wp72() -> (
    LiveSession,
    Wp72FakeRpc,
    Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
) {
    let now = now_unix_i32();
    let self_uid = 820_001i64;
    let fake = Wp72FakeRpc::new(2);
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(synthetic_self_user(self_uid))],
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
            users: Vec::new(),
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
fn wp72_live_fake_rpc_attached_stickers_no_paid_token_and_default_emojis() {
    let (live, fake, runtime) = boot_live_wp72();
    let now = now_unix_i32();

    // 1. Live `message_to_viewer_media_item` preserves `has_stickers` real photo ref and sends
    //    exact `messages.getAttachedStickers(InputStickeredMediaPhoto)` on the wire.
    let session = DataSession::new(DataUserId::from_tl(820_001));
    let peer_820002 = DataPeerId::from_user(DataUserId::from_tl(820_002));
    let mut photo_msg = DataMessage::new_text(
        peer_820002,
        DataMsgId(55),
        Some(peer_820002),
        now,
        tg_data::TextWithEntities::default(),
    );
    photo_msg.content = DataMessageContent::Regular {
        text: tg_data::TextWithEntities::default(),
        media: DataMessageMedia::Photo(Box::new(PhotoData {
            id: PhotoId(444_555),
            access_hash: 0x0011_2233_4455_6677,
            file_reference: vec![0xCA, 0xFE, 0xBA, 0xBE],
            date: now,
            dc_id: 2,
            sizes: vec![PhotoSize {
                kind: "y".to_owned(),
                width: 800,
                height: 600,
                size: 12_345,
            }],
            stripped_thumb: None,
            has_stickers: true,
            spoiler: false,
            ttl_seconds: None,
            live_photo_video_id: None,
        })),
    };
    let viewer_item = tg_app::live::convert::message_to_viewer_media_item(&session, &photo_msg)
        .expect("viewer media item");
    let media_ref = viewer_item
        .attached_stickers_media
        .clone()
        .expect("has_stickers photo must carry real AttachedStickersMedia");
    assert_eq!(
        media_ref,
        AttachedStickersMedia::Photo {
            id: 444_555,
            access_hash: 0x0011_2233_4455_6677,
            file_reference: vec![0xCA, 0xFE, 0xBA, 0xBE],
        }
    );

    fake.enqueue_ok(
        tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID,
        &vec![tl_enums::StickerSetCovered::StickerSetCovered(
            tl_types::StickerSetCovered {
                set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                    archived: false,
                    official: false,
                    masks: true,
                    emojis: false,
                    text_color: false,
                    channel_emoji_status: false,
                    creator: false,
                    installed_date: None,
                    id: 9501,
                    access_hash: 99,
                    title: "Viewer Masks".to_owned(),
                    short_name: "viewer_masks".to_owned(),
                    thumbs: None,
                    thumb_dc_id: None,
                    thumb_version: None,
                    thumb_document_id: None,
                    count: 8,
                    hash: 1,
                }),
                cover: tl_enums::Document::Empty(tl_types::DocumentEmpty { id: 601 }),
            },
        )],
    );
    let sets = ChatHelpersBackend::get_attached_stickers(&live, &media_ref)
        .expect("live get_attached_stickers");
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].short_name, "viewer_masks");
    let attached_reqs = fake.requests_for(tl_fns::messages::GetAttachedStickers::CONSTRUCTOR_ID);
    assert_eq!(attached_reqs.len(), 1);
    let expected_wire = tl_fns::messages::GetAttachedStickers {
        media: tl_enums::InputStickeredMedia::Photo(tl_types::InputStickeredMediaPhoto {
            id: tl_enums::InputPhoto::InputPhoto(tl_types::InputPhoto {
                id: 444_555,
                access_hash: 0x0011_2233_4455_6677,
                file_reference: vec![0xCA, 0xFE, 0xBA, 0xBE],
            }),
        }),
    }
    .to_bytes();
    assert_eq!(attached_reqs[0], expected_wire);

    // 2. Live `contacts.exportNoPaidMessagesToken` round-trips into `PeerQrBoxState`.
    fake.enqueue_ok(
        tl_fns::contacts::ExportNoPaidMessagesToken::CONSTRUCTOR_ID,
        &tl_enums::ExportedNoPaidMessagesToken::ExportedNoPaidMessagesToken(
            tl_types::ExportedNoPaidMessagesToken {
                token: "live_waiver_tmp".to_owned(),
                url: "https://t.me/self_wp72?free=live_waiver_tmp".to_owned(),
                expires: Some(now + 1800),
            },
        ),
    );
    let exported = InfoBackend::export_no_paid_messages_token(&live, false, true)
        .expect("live export_no_paid_messages_token");
    assert_eq!(exported.url, "https://t.me/self_wp72?free=live_waiver_tmp");
    let mut qr = PeerQrBoxState::for_link(
        PeerId::from_user(820_001),
        "Self",
        "@self_wp72",
        "https://t.me/self_wp72",
        0,
    );
    let _ = qr.apply_no_paid_messages_token(&exported, false, true);
    assert_eq!(qr.free_code_mode, FreeMessagesCodeMode::Temporary);
    assert_eq!(qr.link, "https://t.me/self_wp72?free=live_waiver_tmp");
    assert_eq!(
        fake.requests_for(tl_fns::contacts::ExportNoPaidMessagesToken::CONSTRUCTOR_ID)
            .len(),
        1
    );

    // 3. Live `account.getDefaultProfilePhotoEmojis` & `account.getDefaultGroupPhotoEmojis`
    //    populate `EmojiUserpicBoxState::suggested_emojis`.
    fake.enqueue_ok(
        tl_fns::account::GetDefaultProfilePhotoEmojis::CONSTRUCTOR_ID,
        &tl_enums::EmojiList::EmojiList(tl_types::EmojiList {
            hash: 111,
            document_id: vec![500_001, 500_002, 500_003],
        }),
    );
    fake.enqueue_ok(
        tl_fns::account::GetDefaultGroupPhotoEmojis::CONSTRUCTOR_ID,
        &tl_enums::EmojiList::EmojiList(tl_types::EmojiList {
            hash: 222,
            document_id: vec![600_001, 600_002],
        }),
    );
    let prof_box = EmojiUserpicBoxState::with_default_catalog(
        PeerId::from_user(820_001),
        "Self",
        DefaultEmojiCatalogKind::ProfilePhoto,
        &live,
    );
    assert_eq!(
        prof_box
            .suggested_emojis
            .iter()
            .map(|e| DocumentId(e.emoji_id as u64))
            .collect::<Vec<_>>(),
        vec![
            DocumentId(500_001),
            DocumentId(500_002),
            DocumentId(500_003)
        ]
    );
    let grp_box = EmojiUserpicBoxState::with_default_catalog(
        PeerId::from_channel(900_001),
        "Team",
        DefaultEmojiCatalogKind::GroupPhoto,
        &live,
    );
    assert_eq!(
        grp_box
            .suggested_emojis
            .iter()
            .map(|e| DocumentId(e.emoji_id as u64))
            .collect::<Vec<_>>(),
        vec![DocumentId(600_001), DocumentId(600_002)]
    );
    assert_eq!(
        fake.requests_for(tl_fns::account::GetDefaultProfilePhotoEmojis::CONSTRUCTOR_ID)
            .len(),
        1
    );
    assert_eq!(
        fake.requests_for(tl_fns::account::GetDefaultGroupPhotoEmojis::CONSTRUCTOR_ID)
            .len(),
        1
    );

    drop(live);
    if let Ok(rt) = Arc::try_unwrap(runtime) {
        rt.shutdown();
    }
}

// =============================================================================
// 6. Headless wgpu Before & After Screenshot Generators (1440x900 & 390x844)
// =============================================================================

#[test]
#[ignore = "captures WP-72 before screenshots under $RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp72_before_screenshots() {
    let _guard = WGPU_WP72_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mock = MockBackend::new();
    let media_mock = MockMediaBackend::new();
    let colors = LiveThemeColors::default();

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. Media Viewer with More menu open
        {
            let mut h = build_snap_harness(size);
            h.state_mut()
                .controller
                .media_overlay_mut()
                .open_item(
                    &media_mock,
                    MOCK_CHAT_PEER_ID,
                    MOCK_PHOTO_LANDSCAPE_ID,
                    1_000,
                )
                .expect("open mock photo");
            settle_layout(&mut h);
            let more_center = Pos2::new(size.x - 16.0 - 19.0, size.y - 27.0);
            click_pos(&mut h, more_center);
            save_wgpu_png(&mut h, &format!("media_viewer_{tag}_before"));
        }

        // 2. EditInviteLinksBox
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

        // 3. PeerQrBox
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let peer = mock
                .peer_profile(mock_peers::USER_MIRA)
                .expect("user profile");
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::PeerQr(PeerQrBoxState::from_profile(&peer)),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("peer_qr_{tag}_before"));
        }

        // 4. EmojiUserpicBox
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EmojiUserpic(EmojiUserpicBoxState::new(session_peers::SELF, "Taylor Kim")),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("emoji_userpic_{tag}_before"));
        }

        // 5. Profile Info Pane (photo actions)
        {
            let mut h = build_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_info(session_peers::USER_MIRA, SectionShow::clear_stack());
            }
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("profile_photo_actions_{tag}_before"));
        }
    }
}

#[test]
#[ignore = "captures WP-72 after screenshots under $RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp72_after_screenshots() {
    let _guard = WGPU_WP72_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mock = MockBackend::new();
    let session = MockSession::new(true);
    let media_mock = MockMediaBackend::new();
    let colors = LiveThemeColors::default();

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. Media Viewer with More menu open showing "Attached Stickers" and clipped 390px footer
        {
            let mut h = build_snap_harness(size);
            h.state_mut()
                .controller
                .media_overlay_mut()
                .open_item(
                    &media_mock,
                    MOCK_CHAT_PEER_ID,
                    MOCK_PHOTO_LANDSCAPE_ID,
                    1_000,
                )
                .expect("open mock photo");
            settle_layout(&mut h);
            let more_center = Pos2::new(size.x - 16.0 - 19.0, size.y - 27.0);
            click_pos(&mut h, more_center);
            save_wgpu_png(&mut h, &format!("media_viewer_{tag}_after"));
        }

        // 2. EditInviteLinksBox with right scrollbar gutter on pills and rows
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
            save_wgpu_png(&mut h, &format!("invite_links_{tag}_after"));
        }

        // 3. PeerQrBox with active fee-waiver QR controls (`contacts.exportNoPaidMessagesToken`)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let peer = mock
                .peer_profile(mock_peers::USER_MIRA)
                .expect("user profile");
            let mut qr = PeerQrBoxState::from_profile_with_backend(&peer, &session);
            if let Ok(info) = session.export_no_paid_messages_token(false, true) {
                let _ = qr.apply_no_paid_messages_token(&info, false, true);
            }
            h.state_mut()
                .controller
                .box_host_mut()
                .push(&ctx, BoxKind::PeerQr(qr), &colors);
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("peer_qr_{tag}_after"));
        }

        // 4. EmojiUserpicBox with server default emoji catalog + crisp Noto Emoji rendering
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let box_state = EmojiUserpicBoxState::with_default_catalog(
                session_peers::SELF,
                "Taylor Kim",
                DefaultEmojiCatalogKind::ProfilePhoto,
                &session,
            );
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::EmojiUserpic(box_state),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("emoji_userpic_{tag}_after"));
        }

        // 5. Profile Info Pane (photo actions disabled cleanly when no real ref is loaded)
        {
            let mut h = build_snap_harness(size);
            if let Some(sess) = h.state_mut().controller.session_mut() {
                sess.show_peer_info(session_peers::USER_MIRA, SectionShow::clear_stack());
            }
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("profile_photo_actions_{tag}_after"));
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
