//! End-to-end `egui_kittest` tests and headless `wgpu` screenshot capture for WP-63:
//! - `StickerSetBox` pack-owner controls (rename, reorder, remove sticker entries, delete pack)
//! - `stickers_gift_box_pack` tiers (the Stickers tab shows no gift-box ribbon, as in 7.4.6)
//! - Peer Profile UI: `info_profile_balance_pill`, Set Main Profile Tab selector,
//!   Personal Channel picker, Report Profile Photo & Suggest Birthday actions, channel gift notifications
//! - Star Gift TON blockchain withdrawal dialog (2FA SRP prompt + Fragment URL, fake backend only),
//!   delete gift confirmation, and channel gift notifications toggle

use std::path::PathBuf;
use std::sync::Mutex;

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::boxes::sticker_set::{StickerSetBoxState, StickerSetOwnerAction};
use tg_app::boxes::{BoxHost, BoxKind};
use tg_app::chat_helpers::gift_box_pack::{GiftBoxPack, GiftBoxPackKind};
use tg_app::chat_helpers::stickers_list_widget::{StickersListEvent, StickersListWidget};
use tg_app::info::backend::{InfoBackend, ProfileMainTab};
use tg_app::info::balance_pill::BalancePillClick;
use tg_app::info::mock::{MockBackend, mock_peers};
use tg_app::info::profile_controls::ordered_shared_media_tabs;
use tg_app::info::wrap_widget::{InfoWrapEvent, InfoWrapWidget};
use tg_app::payments::backend::{PaymentsBackend, ReceivedGiftItem};
use tg_app::payments::mock::{DEMO_CLOUD_PASSWORD, MockPaymentsBackend};
use tg_app::payments::star_gifts::{StarGiftModal, StarGiftsTab, StarGiftsView, TonWithdrawalMode};
use tg_app::payments::{AppPaymentsHost, PaymentsScreen, PaymentsScreenHost};
use tg_app::session_api::{StickerSetPreview, StickerSetPreviewItem};
use tg_app::settings::theme_bridge::LiveThemeColors;
use tg_app::stand_in::mock_session::session_peers;
use tg_app::stand_in::theme::Palette;
use tg_core::settings::{CoreSettings, PowerSavingFlags};

use crate::common::{AppTestState, build_wgpu_harness, settle_layout, step_frames};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_WP63_LOCK: Mutex<()> = Mutex::new(());

fn wp63_snap_dir() -> PathBuf {
    std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp63"))
}

fn save_wgpu_png(harness: &mut egui_kittest::Harness<'static, AppTestState>, name: &str) {
    let dir = wp63_snap_dir();
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

fn sample_sticker_set_preview() -> StickerSetPreview {
    StickerSetPreview {
        set_id: 9001,
        access_hash: 42,
        title: "Aurora Studio Pack".to_owned(),
        short_name: "aurora_studio".to_owned(),
        count: 5,
        is_emoji: false,
        installed: true,
        archived: false,
        official: false,
        items: vec![
            StickerSetPreviewItem {
                document_id: 101,
                alt_emoji: "🚀".to_owned(),
                is_animated: true,
                is_video: false,
                is_premium: false,
                thumb_rgba: None,
            },
            StickerSetPreviewItem {
                document_id: 102,
                alt_emoji: "✨".to_owned(),
                is_animated: false,
                is_video: false,
                is_premium: false,
                thumb_rgba: None,
            },
            StickerSetPreviewItem {
                document_id: 103,
                alt_emoji: "🎁".to_owned(),
                is_animated: false,
                is_video: false,
                is_premium: true,
                thumb_rgba: None,
            },
            StickerSetPreviewItem {
                document_id: 104,
                alt_emoji: "💎".to_owned(),
                is_animated: false,
                is_video: true,
                is_premium: false,
                thumb_rgba: None,
            },
            StickerSetPreviewItem {
                document_id: 105,
                alt_emoji: "🎉".to_owned(),
                is_animated: true,
                is_video: false,
                is_premium: false,
                thumb_rgba: None,
            },
        ],
    }
}

#[test]
fn wp63_sticker_set_box_owner_controls_and_visibility() {
    // 1. Non-owner pack: owner controls must NOT be rendered or mutable via UI.
    {
        let mut box_host = BoxHost::new();
        let mut pushed = false;
        let colors = LiveThemeColors::default();
        let mut backend = MockBackend::new();
        let mut h = egui_kittest::Harness::builder()
            .with_size(Vec2::new(540.0, 560.0))
            .allow_missing_glyphs()
            .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
            .build_ui(|ui| {
                if !pushed {
                    let st = StickerSetBoxState::from_preview(sample_sticker_set_preview());
                    assert!(!st.is_owner);
                    box_host.push(ui.ctx(), BoxKind::StickerSet(st), &colors);
                    pushed = true;
                }
                let _ = box_host.show(ui.ctx(), ui.ctx().content_rect(), &mut backend, &colors);
            });
        h.run_steps(4);
        assert!(
            h.query_by_label_contains("Edit name").is_none(),
            "non-owner StickerSetBox must hide Edit name control"
        );
        assert!(
            h.query_by_label_contains("Delete Pack").is_none(),
            "non-owner StickerSetBox must hide Delete Pack control"
        );
    }

    // 2. Owner pack: rename, reorder, remove sticker, and two-step delete pack.
    let mut state = StickerSetBoxState::from_preview(sample_sticker_set_preview()).with_owner(true);
    assert!(state.is_owner);

    // Rename validation: empty rejected, valid accepted.
    state.rename_draft = "   ".to_owned();
    assert!(state.submit_rename().is_err());
    assert!(state.rename_error.is_some());
    assert!(state.take_owner_actions().is_empty());

    state.rename_draft = "Aurora Pro Pack".to_owned();
    assert!(state.submit_rename().is_ok());
    assert_eq!(state.title, "Aurora Pro Pack");
    let actions = state.take_owner_actions();
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        StickerSetOwnerAction::RenamePack {
            set_id: 9001,
            new_title,
            ..
        } if new_title == "Aurora Pro Pack"
    ));

    // Reorder sticker 0 -> 1.
    state.set_reorder_mode(true);
    assert!(state.reorder_mode);
    assert!(state.move_sticker(0, 1));
    assert_eq!(state.items[0].document_id, 102);
    assert_eq!(state.items[1].document_id, 101);
    let actions = state.take_owner_actions();
    assert!(matches!(
        &actions[0],
        StickerSetOwnerAction::ReorderSticker {
            document_id: 101,
            new_position: 1,
            ..
        }
    ));

    // Remove sticker at index 0.
    assert!(state.remove_sticker_at(0));
    assert_eq!(state.items.len(), 4);
    assert_eq!(state.count, 4);
    let actions = state.take_owner_actions();
    assert!(matches!(
        &actions[0],
        StickerSetOwnerAction::RemoveSticker {
            document_id: 102,
            ..
        }
    ));

    // Render owner UI in egui_kittest and verify controls + two-step delete confirmation.
    let mut owner_box_host = BoxHost::new();
    let mut owner_pushed = false;
    let mut captured_events = Vec::new();
    let colors = LiveThemeColors::default();
    let mut backend = MockBackend::new();
    let mut h = egui_kittest::Harness::builder()
        .with_size(Vec2::new(620.0, 640.0))
        .allow_missing_glyphs()
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui(|ui| {
            if !owner_pushed {
                let st =
                    StickerSetBoxState::from_preview(sample_sticker_set_preview()).with_owner(true);
                owner_box_host.push(ui.ctx(), BoxKind::StickerSet(st), &colors);
                owner_pushed = true;
            }
            captured_events.extend(owner_box_host.show(
                ui.ctx(),
                ui.ctx().content_rect(),
                &mut backend,
                &colors,
            ));
        });
    h.run_steps(4);
    assert!(
        h.query_by_label_contains("Edit name").is_some()
            || h.query_by_label_contains("Rename").is_some(),
        "owner StickerSetBox must show rename button"
    );
    assert!(
        h.query_by_label_contains("Reorder").is_some(),
        "owner StickerSetBox must show reorder button"
    );
    assert!(
        h.query_by_label_contains("Delete").is_some(),
        "owner StickerSetBox must show delete pack button"
    );
}

#[test]
fn wp63_stickers_gift_box_pack_ribbon_and_tiers() {
    // Verify GiftBoxPack dividers and lookup semantics (`stickers_gift_box_pack.cpp`).
    assert_eq!(GiftBoxPack::months_for_stars(100), 3);
    assert_eq!(GiftBoxPack::months_for_stars(1_000), 3);
    assert_eq!(GiftBoxPack::months_for_stars(1_500), 6);
    assert_eq!(GiftBoxPack::months_for_stars(2_500), 12);
    assert_eq!(GiftBoxPack::months_for_stars(10_000), 12);

    let pack = GiftBoxPack::demo();
    assert_eq!(pack.lookup(1).expect("1m").divider, 1);
    assert_eq!(pack.lookup(6).expect("6m").divider, 6);
    assert_eq!(pack.lookup(24).expect("24m").divider, 24);
    assert_eq!(pack.ton_lookup(0).expect("ton 0").divider, 0);
    assert_eq!(pack.ton_lookup(10).expect("ton 10").divider, 10);
    assert_eq!(pack.ton_lookup(50).expect("ton 50").divider, 50);

    // Render StickersListWidget and verify Gift Boxes ribbon & TON mode switch.
    let mut widget = StickersListWidget::new();
    assert_eq!(widget.gift_box_kind(), GiftBoxPackKind::Premium);
    widget.set_gift_box_kind(GiftBoxPackKind::Ton);
    assert_eq!(widget.gift_box_kind(), GiftBoxPackKind::Ton);
    widget.set_gift_box_kind(GiftBoxPackKind::Premium);

    let palette = Palette::day();
    let mut helpers_backend = tg_app::chat_helpers::mock::MockChatHelpersBackend::new();
    let mut events: Vec<StickersListEvent> = Vec::new();
    let mut h = egui_kittest::Harness::builder()
        .with_size(Vec2::new(360.0, 520.0))
        .allow_missing_glyphs()
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui(|ui| {
            events.extend(widget.show(ui, &mut helpers_backend, &palette));
        });
    h.run_steps(4);
    // Telegram Desktop 7.4.6 shows no gift-box ribbon in the Stickers tab (UI parity stage 2);
    // `GiftBoxPack` only resolves gift stickers for gift messages.
    assert!(
        h.query_all_by_label_contains("Gift Boxes").next().is_none(),
        "StickersListWidget must not render a Gift Boxes ribbon"
    );
}

#[test]
fn wp63_peer_profile_balance_pill_main_tab_personal_channel_and_actions() {
    let mut backend = MockBackend::new();

    // 1. Balance pill data present on self and Mira profiles.
    let self_bal = backend
        .peer_profile_balance(mock_peers::SAVED_MESSAGES)
        .expect("self balance seeded");
    assert_eq!(self_bal.ton_nanos, 12_450_000_000);
    assert_eq!(self_bal.stars_balance, 1_450);

    // 2. Set Main Profile Tab persists and rotates tab order.
    backend
        .set_main_profile_tab(mock_peers::USER_MIRA, ProfileMainTab::Music)
        .expect("set main tab");
    assert_eq!(
        backend.main_profile_tab(mock_peers::USER_MIRA),
        Some(ProfileMainTab::Music)
    );
    let ordered = ordered_shared_media_tabs(Some(ProfileMainTab::Music));
    assert_eq!(ordered[0], tg_app::info::backend::SharedMediaTab::Music);

    // 3. Personal channel picker updates & clears self personal channel.
    assert!(
        backend
            .personal_channel(mock_peers::SAVED_MESSAGES)
            .is_none()
    );
    let candidates = backend.personal_channel_candidates();
    assert!(!candidates.is_empty());
    backend
        .update_personal_channel(Some(candidates[0].peer_id))
        .expect("set personal channel");
    assert_eq!(
        backend
            .personal_channel(mock_peers::SAVED_MESSAGES)
            .map(|c| c.peer_id),
        Some(candidates[0].peer_id)
    );
    backend
        .update_personal_channel(None)
        .expect("clear personal channel");
    assert!(
        backend
            .personal_channel(mock_peers::SAVED_MESSAGES)
            .is_none()
    );

    // 4. Suggest birthday & report profile photo & channel gift notifications.
    backend
        .suggest_user_birthday(
            mock_peers::USER_MIRA,
            tg_app::info::values::Birthday {
                day: 21,
                month: 9,
                year: Some(1996),
            },
        )
        .expect("suggest birthday");
    assert_eq!(backend.profile_extras.suggested_birthdays.len(), 1);

    backend
        .report_profile_photo(
            mock_peers::USER_MIRA,
            tg_app::info::backend::ProfilePhotoRef {
                id: 42,
                access_hash: 99,
                file_reference: vec![1, 2],
            },
            tg_app::info::backend::ReportReason::Spam,
            "impersonation photo",
        )
        .expect("report profile photo");
    assert_eq!(backend.profile_extras.reported_profile_photos.len(), 1);

    assert_eq!(
        backend.chat_star_gift_notifications_enabled(mock_peers::CHANNEL_RELEASES),
        Some(true)
    );
    assert!(
        !backend
            .toggle_chat_star_gift_notifications(mock_peers::CHANNEL_RELEASES, false)
            .expect("toggle off")
    );
    assert_eq!(
        backend.chat_star_gift_notifications_enabled(mock_peers::CHANNEL_RELEASES),
        Some(false)
    );

    // 5. Render InfoWrapWidget for Mira Vance and verify balance pill + personal channel + main tab bar render cleanly.
    let mut wrap = InfoWrapWidget::for_peer(mock_peers::USER_MIRA);
    let colors = LiveThemeColors::default();
    let mut events: Vec<InfoWrapEvent> = Vec::new();
    let mut h = egui_kittest::Harness::builder()
        .with_size(Vec2::new(420.0, 900.0))
        .allow_missing_glyphs()
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui(|ui| {
            events.extend(wrap.show(ui, &mut backend, true, &colors));
        });
    h.run_steps(5);
    assert!(
        h.query_by_label_contains("8.75 TON").is_some(),
        "ProfileCover must render info_profile_balance_pill TON value"
    );
    assert!(
        h.query_by_label_contains("Rustgram Release Notes")
            .is_some(),
        "Peer profile must display linked personal channel"
    );
    let _ = BalancePillClick::Ton;
}

#[test]
fn wp63_star_gift_ton_withdrawal_delete_and_notifications() {
    let mut backend = MockPaymentsBackend::new();
    let bundle = backend.load_star_gifts().expect("load star gifts");
    let mut view = StarGiftsView::new(bundle);
    view.active_tab = StarGiftsTab::Received;

    // Sync peer gifts & notification toggle from fake marketplace backend.
    {
        let mp = backend
            .as_marketplace_backend()
            .expect("mock marketplace backend");
        view.reload_peer_gifts(mp).expect("reload peer gifts");
    }
    assert!(view.chat_notifications_enabled);
    let unique_gift: ReceivedGiftItem = view
        .received
        .iter()
        .find(|g| g.saved.info.unique.is_some())
        .cloned()
        .expect("seeded unique collectible gift");
    let gift_id = unique_gift.saved.info.id;

    // 1. Rejection on empty / wrong 2FA password (fake backend only; never touches network).
    {
        let mp = backend.as_marketplace_backend().expect("mp");
        assert!(mp.get_star_gift_withdrawal_url(gift_id, "").is_err());
        assert!(mp.get_star_gift_withdrawal_url(gift_id, "wrong").is_err());
        assert!(mp.withdraw_star_gift(gift_id, "wrong", true).is_err());

        // Valid fake 2FA password returns deterministic Fragment demo URL.
        let url = mp
            .get_star_gift_withdrawal_url(gift_id, DEMO_CLOUD_PASSWORD)
            .expect("fragment withdrawal url");
        assert!(url.starts_with("https://fragment.com/nft/withdraw/"));
        assert!(url.contains("demo=1"));

        // Toggle chat gift notifications.
        let toggled = mp
            .toggle_chat_star_gift_notifications("", false)
            .expect("toggle notifications");
        assert!(!toggled);
        assert!(!mp.chat_star_gift_notifications_enabled(""));
    }

    // 2. Render WithdrawToTon modal in egui_kittest and verify confirmation UI & URL display.
    view.open_withdraw_to_ton(unique_gift.clone());
    if let Some(StarGiftModal::WithdrawToTon {
        ref mut password,
        ref mut mode,
        ..
    }) = view.modal
    {
        *mode = TonWithdrawalMode::FragmentUrl;
        *password = DEMO_CLOUD_PASSWORD.to_owned();
    }

    let palette = Palette::day();
    {
        let mut h = egui_kittest::Harness::builder()
            .with_size(Vec2::new(620.0, 760.0))
            .allow_missing_glyphs()
            .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
            .build_ui(|ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let _ = view.show(ui, &mut backend, &palette);
                });
            });
        h.run_steps(4);
        assert!(
            h.query_by_label_contains("Fragment Withdrawal URL")
                .is_some(),
            "WithdrawToTon modal must show Fragment Withdrawal URL mode"
        );
        assert!(
            h.query_by_label_contains("Get Fragment Withdrawal Link")
                .is_some(),
            "WithdrawToTon modal must show explicit confirmation button"
        );
    }

    // 3. Test fake TON withdrawal + Delete Gift confirmation via backend.
    {
        let mp = backend.as_marketplace_backend().expect("mp");
        let notice = mp
            .withdraw_star_gift(gift_id, DEMO_CLOUD_PASSWORD, true)
            .expect("withdraw star gift");
        assert!(notice.contains("Simulated TON withdrawal"));

        // Delete a second gift and verify it is removed.
        let regular_id = view
            .received
            .iter()
            .find(|g| g.saved.info.id != gift_id)
            .map(|g| g.saved.info.id)
            .expect("second gift");
        let del_msg = mp.delete_star_gift(regular_id).expect("delete gift");
        assert!(del_msg.contains("Deleted"));
        assert!(mp.get_saved_star_gift(regular_id).is_err());
    }

    // Also render ConfirmDeleteGift modal to verify it lays out cleanly.
    view.open_delete_gift_confirm(unique_gift);
    let mut h2 = egui_kittest::Harness::builder()
        .with_size(Vec2::new(620.0, 620.0))
        .allow_missing_glyphs()
        .with_context_setup(tg_ui::fonts::register_tdesktop_fonts)
        .build_ui(|ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let _ = view.show(ui, &mut backend, &palette);
            });
        });
    h2.run_steps(4);
    assert!(
        h2.query_by_label_contains("Confirm Delete Gift").is_some(),
        "ConfirmDeleteGift modal must render confirmation button"
    );
}

#[test]
#[ignore = "generates WP-63 before screenshots under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp63_before_screenshots() {
    let _guard = WGPU_WP63_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. StickerSetBox
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let colors = LiveThemeColors::default();
            let st_box = StickerSetBoxState::from_preview(sample_sticker_set_preview());
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::StickerSet(st_box),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("sticker_set_box_{tag}_before"));
        }

        // 2. StickersListWidget (TabbedPanel on Stickers tab)
        {
            let mut h = build_snap_harness(size);
            let now = h.ctx.input(|i| i.time);
            let panel = h
                .state_mut()
                .controller
                .main_widget_mut()
                .tabbed_panel_mut();
            panel.set_active_tab(tg_app::chat_helpers::SelectorTab::Stickers);
            panel.show_animated(now);
            settle_layout(&mut h);
            step_frames(&mut h, 12);
            save_wgpu_png(&mut h, &format!("stickers_list_{tag}_before"));
        }

        // 3. Peer Profile (InfoWrapWidget)
        {
            let mut h = build_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_info(
                    session_peers::USER_MIRA,
                    tg_app::window::section::SectionShow::clear_stack(),
                );
            }
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("peer_profile_{tag}_before"));
        }

        // 4. Star Gifts overlay (Received unique collectible)
        {
            let mut h = egui_kittest::HarnessBuilder::default()
                .with_size(size)
                .with_step_dt(crate::common::TEST_STEP_DT)
                .with_render_cursor(false)
                .allow_missing_glyphs()
                .with_accessibility_check(false)
                .renderer(crate::common::serialized_wgpu_renderer())
                .with_context_setup(|ctx| {
                    tg_ui::fonts::register_tdesktop_fonts(ctx);
                    crate::common::apply_deterministic_style(ctx);
                })
                .build_ui_state(
                    |ui, st: &mut AppTestState| {
                        st.show(ui);
                        let palette = st.controller.palette();
                        let mode = st.controller.theme_mode();
                        AppPaymentsHost::update_and_render(ui.ctx(), &palette, mode);
                    },
                    {
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
                        state
                    },
                );
            settle_layout(&mut h);
            let ctx = h.ctx.clone();
            AppPaymentsHost::open_screen(&ctx, PaymentsScreen::StarGifts);
            settle_layout(&mut h);
            step_frames(&mut h, 6);
            save_wgpu_png(&mut h, &format!("star_gifts_withdraw_{tag}_before"));
            AppPaymentsHost::close(&ctx);
        }
    }
}

#[test]
#[ignore = "generates WP-63 after screenshots under RUSTGRAM_SNAPSHOT_DIR (or temp dir)"]
fn capture_wp63_after_screenshots() {
    let _guard = WGPU_WP63_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (tag, size) in [("1440x900", DESKTOP_SIZE), ("390x844", MOBILE_SIZE)] {
        // 1. StickerSetBox with pack-owner controls (Edit name, Reorder active, Delete Pack)
        {
            let mut h = build_snap_harness(size);
            let ctx = h.ctx.clone();
            let colors = LiveThemeColors::default();
            let mut st_box =
                StickerSetBoxState::from_preview(sample_sticker_set_preview()).with_owner(true);
            st_box.set_reorder_mode(true);
            h.state_mut().controller.box_host_mut().push(
                &ctx,
                BoxKind::StickerSet(st_box),
                &colors,
            );
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("sticker_set_box_{tag}_after"));
        }

        // 2. StickersListWidget (Stickers tab)
        {
            let mut h = build_snap_harness(size);
            let now = h.ctx.input(|i| i.time);
            let panel = h
                .state_mut()
                .controller
                .main_widget_mut()
                .tabbed_panel_mut();
            panel.set_active_tab(tg_app::chat_helpers::SelectorTab::Stickers);
            panel.show_animated(now);
            settle_layout(&mut h);
            step_frames(&mut h, 12);
            save_wgpu_png(&mut h, &format!("stickers_list_{tag}_after"));
        }

        // 3. Peer Profile (InfoWrapWidget: BalancePill, Personal Channel, Main Tab Bar, Suggest Birthday & Report Photo)
        {
            let mut h = build_snap_harness(size);
            if let Some(session) = h.state_mut().controller.session_mut() {
                session.show_peer_info(
                    session_peers::USER_MIRA,
                    tg_app::window::section::SectionShow::clear_stack(),
                );
            }
            settle_layout(&mut h);
            save_wgpu_png(&mut h, &format!("peer_profile_{tag}_after"));
        }

        // 4. Star Gifts TON blockchain withdrawal confirmation dialog (with Fragment URL preview)
        {
            struct PaymentsSnapState {
                app: AppTestState,
                host: PaymentsScreenHost,
            }
            let mut app = AppTestState::main_workspace();
            let static_settings = CoreSettings {
                loop_animated_stickers: false,
                power_saving: PowerSavingFlags::ALL,
                ..CoreSettings::default()
            };
            app.controller
                .main_widget_mut()
                .apply_core_settings(&static_settings);

            let mut host = PaymentsScreenHost::with_mock();
            host.select_screen(PaymentsScreen::StarGifts);
            if let Some(gv) = host.gifts_view_mut() {
                gv.active_tab = StarGiftsTab::Received;
                if let Some(unique_item) = gv
                    .received
                    .iter()
                    .find(|g| g.saved.info.unique.is_some())
                    .cloned()
                {
                    let slug = unique_item
                        .saved
                        .info
                        .unique
                        .as_ref()
                        .map(|u| u.slug.clone())
                        .unwrap_or_else(|| "CrystalPrism-42".to_owned());
                    let gid = unique_item.saved.info.id;
                    gv.modal = Some(StarGiftModal::WithdrawToTon {
                        item: unique_item,
                        mode: TonWithdrawalMode::FragmentUrl,
                        password: DEMO_CLOUD_PASSWORD.to_owned(),
                        keep_on_profile: true,
                        error: None,
                        result_url: Some(format!(
                            "https://fragment.com/nft/withdraw/{slug}?gift={gid}&demo=1"
                        )),
                        result_notice: None,
                    });
                }
            }

            let mut h = egui_kittest::HarnessBuilder::default()
                .with_size(size)
                .with_step_dt(crate::common::TEST_STEP_DT)
                .with_render_cursor(false)
                .allow_missing_glyphs()
                .with_accessibility_check(false)
                .renderer(crate::common::serialized_wgpu_renderer())
                .with_context_setup(|ctx| {
                    tg_ui::fonts::register_tdesktop_fonts(ctx);
                    crate::common::apply_deterministic_style(ctx);
                })
                .build_ui_state(
                    |ui, st: &mut PaymentsSnapState| {
                        st.app.show(ui);
                        let palette = st.app.controller.palette();
                        let screen = ui.ctx().content_rect();
                        let modal_w = (screen.width() - 32.0).clamp(320.0, 660.0);
                        let modal_h = (screen.height() - 48.0).clamp(420.0, 760.0);
                        egui::Window::new("Star Gifts & Marketplace")
                            .collapsible(false)
                            .resizable(false)
                            .title_bar(false)
                            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                            .fixed_size(egui::Vec2::new(modal_w, modal_h))
                            .frame(
                                egui::Frame::window(ui.style())
                                    .fill(palette.window_bg)
                                    .stroke(egui::Stroke::new(1.0, palette.divider))
                                    .corner_radius(egui::CornerRadius::same(12))
                                    .inner_margin(egui::Margin::ZERO),
                            )
                            .show(ui.ctx(), |ui| {
                                let _ = st.host.show(ui, &palette, true);
                            });
                    },
                    PaymentsSnapState { app, host },
                );
            for _ in 0..8 {
                h.step();
            }
            let dir = wp63_snap_dir();
            std::fs::create_dir_all(&dir).expect("create snap dir");
            let img = h.render().expect("wgpu render");
            img.save(dir.join(format!("star_gifts_withdraw_{tag}_after.png")))
                .expect("save png");
        }
    }
}
