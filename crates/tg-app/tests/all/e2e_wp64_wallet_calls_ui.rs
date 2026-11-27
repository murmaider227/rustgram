//! End-to-end `egui_kittest` tests and headless `wgpu` before/after screenshot capture for WP-64:
//! - Wallet Card Editor (`wallet.setCardStickers`) & Move-to-Gifts (`payments.{depositStarGift,displayStarGift}`)
//! - Wallet Cloud Backup toggle & Gasless fee indicator
//! - Group Call Invite Link Export (`phone.exportGroupCallInvite`), Invite Participants Picker (`phone.inviteToGroupCall`),
//!   Scheduled Group Call bar (`phone.startScheduledGroupCall` / `phone.toggleGroupCallStartSubscription`),
//!   and Recent Calls "Clear All" confirmation (`messages.deletePhoneCallHistory`)
//! - Poll Results Reload & Poll Stats (`messages.getPollResults`, `stats.getPollStats`)
//! - Export Takeout Init-Delay prompt & cancellation cleanup (`ApiError::parse_takeout_init_delay`, `finish_takeout_cancelled`)

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui::Vec2;
use egui_kittest::kittest::Queryable;
use tg_app::calls::backend::CallsBackend;
use tg_app::calls::group_panel::GroupCallPanelState;
use tg_app::calls::mock::MockCallsBackend;
use tg_app::calls::recent_calls::{RecentCallsAction, RecentCallsBox};
use tg_app::calls::theme::CallPalette;
use tg_app::export::manager::takeout_delay_info_from_error;
use tg_app::history::backend::{
    HistoryBackend, PollAttachment, PollOption, PollVoterEntry, PollVotesPage,
};
use tg_app::history::theme::HistoryColors;
use tg_app::info::reactions_list::{PollVotersAction, PollVotersWidget};
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::wallet::{WalletPanel, WalletScreen};
use tg_app_window::mock_session::MockSession;
use tg_export::stand_in::InputPeer;
use tg_export::{
    ApiError, Environment, ExportController, ExportSource, ExportState, ExportTypes,
    InMemoryExportSource, Settings,
};
use tg_lang::tr;
use tg_wallet::mtproto_api::{CardStickerCoordinates, CardStickerInput};
use tg_wallet::toncenter::NftKind;

use crate::common::{TEST_STEP_DT, apply_deterministic_style};

const DESKTOP_SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const MOBILE_SIZE: Vec2 = Vec2::new(390.0, 844.0);

static WGPU_WP64_LOCK: Mutex<()> = Mutex::new(());

fn snap_dir() -> PathBuf {
    let dir = std::env::var_os("RUSTGRAM_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rustgram-snap").join("wp64"));
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
        .renderer(crate::common::serialized_wgpu_renderer())
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

fn sample_poll_widget() -> PollVotersWidget {
    let poll = PollAttachment {
        question: "Which release focus should we prioritize next?".to_owned(),
        is_quiz: false,
        is_anonymous: false,
        multiple_choice: false,
        closed: false,
        options: vec![
            PollOption {
                text: "Wallet Card Customization".to_owned(),
                votes: 42,
                chosen: true,
                correct: false,
            },
            PollOption {
                text: "Scheduled Group Calls".to_owned(),
                votes: 31,
                chosen: false,
                correct: false,
            },
            PollOption {
                text: "Poll Statistics Charts".to_owned(),
                votes: 19,
                chosen: false,
                correct: false,
            },
        ],
        total_voters: 92,
        explanation: None,
    };
    let page = PollVotesPage {
        total_count: 92,
        voters: vec![
            PollVoterEntry {
                peer_id: PeerId::from_user(201),
                name: "Mira Solis".to_owned(),
                username: Some("mira_solis".to_owned()),
                color_index: 1,
                option_indices: vec![0],
                date_unix: 1_760_000_000,
            },
            PollVoterEntry {
                peer_id: PeerId::from_user(202),
                name: "Theo Vance".to_owned(),
                username: Some("theo_v".to_owned()),
                color_index: 3,
                option_indices: vec![1],
                date_unix: 1_760_001_200,
            },
            PollVoterEntry {
                peer_id: PeerId::from_user(203),
                name: "Nadia Chen".to_owned(),
                username: None,
                color_index: 5,
                option_indices: vec![2],
                date_unix: 1_760_002_400,
            },
        ],
        next_offset: None,
    };
    PollVotersWidget::new(PeerId::from_channel(3001), MsgId(77), poll, page)
}

fn sample_poll_stats_graph() -> tg_stats::StatisticalGraph {
    let session = MockSession::new(true);
    session
        .poll_stats(PeerId::from_channel(3001), MsgId(77), false)
        .expect("mock poll stats")
}

// =============================================================================
// Automated End-to-End `egui_kittest` Tests (WP-64)
// =============================================================================

#[test]
fn wp64_wallet_card_editor_adds_sticker_toggles_bg_and_persists() {
    let mut panel = WalletPanel::with_mock_demo();
    panel.select_screen(WalletScreen::Overview);

    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(720.0, 820.0))
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, p: &mut WalletPanel| {
                let palette = Palette::day();
                let _ = p.show(ui, &palette, true);
            },
            panel,
        );
    harness.step();

    // Open Card Editor from Wallet Overview.
    harness.get_by_label("Edit Card").click();
    harness.run_steps(3);
    assert_eq!(harness.state().active_screen(), WalletScreen::CardEditor);

    // Add the first collectible gift sticker.
    let add_lbl = tr::lng_wallet_card_add();
    harness
        .get_all_by_label(&add_lbl)
        .next()
        .expect("add sticker button")
        .click();
    harness.run_steps(2);
    assert_eq!(
        harness
            .state_mut()
            .card_editor_state_mut()
            .draft
            .stickers
            .len(),
        1
    );

    // Apply card backdrop color from the selected gift.
    let apply_color_lbl = tr::lng_wallet_card_apply_color();
    harness
        .get_all_by_label(&apply_color_lbl)
        .next()
        .expect("apply color button")
        .click();
    harness.run_steps(2);
    assert!(
        harness
            .state_mut()
            .card_editor_state_mut()
            .draft
            .background_address
            .is_some()
    );

    // Save card -> calls `WalletBackend::set_card_stickers` and returns to Overview.
    let save_lbl = tr::lng_settings_save();
    harness.get_by_label(&save_lbl).click();
    harness.run_steps(3);
    assert_eq!(harness.state().active_screen(), WalletScreen::Overview);
    let saved = harness.state_mut().backend_mut().wallet_card_state();
    assert_eq!(saved.stickers.len(), 1);
    assert!(saved.background_address.is_some());
}

#[test]
fn wp64_wallet_move_to_gifts_stops_at_confirmation_then_deposits() {
    let mut panel = WalletPanel::with_mock_demo();
    let gift_item = panel
        .backend_mut()
        .collectibles()
        .into_iter()
        .find(|n| matches!(n.kind, NftKind::TelegramGift))
        .expect("demo wallet has a TelegramGift collectible");
    let gift_addr = gift_item.address.clone();
    panel.select_screen(WalletScreen::Collectibles);
    panel.collectibles_state_mut().selected_nft_address = Some(gift_addr.clone());

    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(720.0, 820.0))
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, p: &mut WalletPanel| {
                let palette = Palette::day();
                let _ = p.show(ui, &palette, true);
            },
            panel,
        );
    harness.step();

    // Open Move to My Gifts sheet.
    let move_lbl = tr::lng_gift_transfer_to_gifts();
    harness.get_by_label(&move_lbl).click();
    harness.run_steps(2);
    assert!(
        harness
            .state_mut()
            .collectibles_state_mut()
            .move_to_gifts
            .is_open()
    );

    // Step 1 -> Step 2 (quote & verification ONLY — must NOT submit yet!).
    harness.get_by_label("Review Move to Gifts").click();
    harness.run_steps(2);
    assert!(
        harness
            .state_mut()
            .collectibles_state_mut()
            .move_to_gifts
            .pending_quote
            .is_some(),
        "quote must be prepared awaiting explicit confirmation"
    );
    assert!(
        harness
            .state_mut()
            .backend_mut()
            .collectibles()
            .iter()
            .any(|n| n.address == gift_addr),
        "gift must NOT be submitted before user presses confirmation"
    );

    // Step 2 -> Step 3: explicit user confirmation button press.
    let confirm_lbl = tr::lng_gift_transfer_to_gifts_button();
    harness.get_by_label(&confirm_lbl).click();
    harness.run_steps(2);
    let receipt = harness
        .state_mut()
        .collectibles_state_mut()
        .move_to_gifts
        .receipt
        .clone()
        .expect("move receipt after confirmation");
    assert!(receipt.shown_in_gifts);
    assert!(
        !harness
            .state_mut()
            .backend_mut()
            .collectibles()
            .iter()
            .any(|n| n.address == gift_addr),
        "gift removed from wallet collectibles after confirmed deposit"
    );
}

#[test]
fn wp64_wallet_cloud_backup_toggle_and_disable_confirmation() {
    let mut panel = WalletPanel::with_mock_demo();
    panel.select_screen(WalletScreen::Backup);

    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(720.0, 820.0))
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, p: &mut WalletPanel| {
                let palette = Palette::day();
                let _ = p.show(ui, &palette, true);
            },
            panel,
        );
    harness.step();
    assert!(
        !harness
            .state_mut()
            .backend_mut()
            .cloud_backup_status()
            .backup_enabled
    );

    // Enable encrypted cloud backup.
    let enable_lbl = tr::lng_wallet_backup_enable();
    harness.get_by_label(&enable_lbl).click();
    harness.run_steps(2);
    assert!(
        harness
            .state_mut()
            .backend_mut()
            .cloud_backup_status()
            .backup_enabled
    );

    // Clicking Disable opens confirmation step first — backup stays enabled until confirmed.
    let disable_lbl = tr::lng_wallet_backup_disable();
    harness.get_by_label(&disable_lbl).click();
    harness.run_steps(2);
    assert!(
        harness
            .state_mut()
            .backend_mut()
            .cloud_backup_status()
            .backup_enabled
    );
    assert!(
        harness
            .state_mut()
            .backup_state_mut()
            .confirm_disable_cloud_backup
    );

    // Explicit confirm disables cloud backup.
    harness.get_by_label("Confirm Disable").click();
    harness.run_steps(2);
    assert!(
        !harness
            .state_mut()
            .backend_mut()
            .cloud_backup_status()
            .backup_enabled
    );
}

#[test]
fn wp64_group_call_export_invite_schedule_and_clear_recent_calls() {
    // 1. Export invite link + Invite Members picker.
    {
        let mut backend = MockCallsBackend::new();
        backend.setup_demo_group_call(10_000);
        let mut panel = GroupCallPanelState::default();
        panel.settings_dialog.open();

        let mut harness = egui_kittest::HarnessBuilder::default()
            .with_size(Vec2::new(900.0, 760.0))
            .with_step_dt(TEST_STEP_DT)
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .build_ui_state(
                |ui, (b, p): &mut (MockCallsBackend, GroupCallPanelState)| {
                    let palette = CallPalette::tdesktop();
                    if let Some(snap) = b.active_group_call().cloned() {
                        p.settings_dialog.show(ui.ctx(), b, &snap, &palette, 10_000);
                        if p.settings_dialog.request_invite_box {
                            p.settings_dialog.request_invite_box = false;
                            p.invite_dialog.open();
                        }
                        p.invite_dialog.show(ui.ctx(), b, &snap, &palette);
                    }
                },
                (backend, panel),
            );
        harness.step();

        // Export invite link calls `export_group_call_invite`.
        harness.get_by_label("Export Link").click();
        harness.run_steps(2);
        let exported = harness
            .state()
            .1
            .settings_dialog
            .exported_link
            .clone()
            .expect("exported invite link");
        assert!(exported.contains("voicechat=spk_"));

        // Open Invite Members picker and invite QA Harness.
        let invite_trigger = tr::lng_group_call_invite();
        harness.get_by_label(&invite_trigger).click();
        harness.run_steps(2);
        assert!(harness.state().1.invite_dialog.open);

        harness.get_by_label("QA Harness").click();
        harness.run_steps(2);
        let invite_btn = tr::lng_group_call_invite_button();
        harness.get_by_label(&invite_btn).click();
        harness.run_steps(2);

        let members = &harness
            .state()
            .0
            .active_group_call()
            .expect("group call")
            .members;
        assert!(
            members
                .iter()
                .any(|m| m.peer.id == MockCallsBackend::PEER_QA_BOT && m.is_invited),
            "QA Harness should appear as invited in the group call"
        );
    }

    // 2. Scheduled Group Call countdown bar: Set Reminder + Start Now confirmation.
    {
        let mut backend = MockCallsBackend::new();
        let future_unix = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1_800_000_000, |d| d.as_secs())
            + 3_665) as i32;
        backend.setup_demo_scheduled_group_call(10_000, future_unix);
        let panel = GroupCallPanelState::default();

        let mut harness = egui_kittest::HarnessBuilder::default()
            .with_size(Vec2::new(900.0, 680.0))
            .with_step_dt(TEST_STEP_DT)
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .build_ui_state(
                |ui, (b, p): &mut (MockCallsBackend, GroupCallPanelState)| {
                    let palette = CallPalette::tdesktop();
                    let snap = b.active_group_call().expect("scheduled group").clone();
                    let rect = ui.available_rect_before_wrap();
                    p.paint_panel_contents(ui, rect, &snap, b, &palette, 10_000);
                    if p.start_scheduled_confirm_open {
                        // Drive confirm modal via `show_window` path by calling it directly when open.
                        p.show_window(ui.ctx(), b, &palette, 10_000);
                    }
                },
                (backend, panel),
            );
        harness.step();

        let rem_lbl = tr::lng_group_call_set_reminder();
        harness.get_by_label(&rem_lbl).click();
        harness.run_steps(2);
        assert!(harness.state().0.active_group_call_schedule_subscribed());

        let start_lbl = tr::lng_group_call_start_now();
        harness.get_by_label(&start_lbl).click();
        harness.run_steps(2);
        assert!(harness.state().1.start_scheduled_confirm_open);

        // Confirm starting the scheduled call now.
        harness
            .get_all_by_label(&start_lbl)
            .last()
            .expect("confirm start now")
            .click();
        harness.run_steps(2);
        assert!(
            harness
                .state()
                .0
                .active_group_call_schedule_date()
                .is_none()
        );
    }

    // 3. Recent Calls "Clear All" confirmation flow (`messages.deletePhoneCallHistory`).
    {
        let backend = MockCallsBackend::new();
        assert!(!backend.recent_calls().is_empty());
        let rbox = RecentCallsBox {
            open: true,
            ..Default::default()
        };

        let mut harness = egui_kittest::HarnessBuilder::default()
            .with_size(Vec2::new(600.0, 600.0))
            .with_step_dt(TEST_STEP_DT)
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .build_ui_state(
                |ui, (b, r): &mut (MockCallsBackend, RecentCallsBox)| {
                    let palette = CallPalette::tdesktop();
                    if let Some(RecentCallsAction::ClearAll { revoke }) =
                        r.show(ui.ctx(), b, &palette)
                    {
                        let _ = b.delete_phone_call_history(revoke);
                    }
                },
                (backend, rbox),
            );
        harness.step();

        let clear_all_lbl = tr::lng_call_box_clear_all();
        harness.get_by_label(&clear_all_lbl).click();
        harness.run_steps(2);
        assert!(harness.state().1.clear_confirm_open);
        assert!(
            !harness.state().0.recent_calls().is_empty(),
            "recent calls must NOT be cleared before confirmation"
        );

        let everyone_lbl = tr::lng_delete_for_everyone_check();
        harness.get_by_label(&everyone_lbl).click();
        harness.run_steps(1);

        let clear_confirm_lbl = tr::lng_call_box_clear_button();
        harness.get_by_label(&clear_confirm_lbl).click();
        harness.run_steps(2);
        assert!(harness.state().0.recent_calls().is_empty());
        assert_eq!(harness.state().0.cleared_history_calls(), &[true]);
    }
}

#[test]
fn wp64_poll_results_reload_and_poll_stats_chart_render() {
    struct PollModalState {
        session: MockSession,
        widget: PollVotersWidget,
        reload_count: usize,
        stats_count: usize,
    }
    let state = PollModalState {
        session: MockSession::new(true),
        widget: sample_poll_widget(),
        reload_count: 0,
        stats_count: 0,
    };

    let mut harness = egui_kittest::HarnessBuilder::default()
        .with_size(Vec2::new(640.0, 680.0))
        .with_step_dt(TEST_STEP_DT)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, st: &mut PollModalState| {
                let colors = HistoryColors::from_palette(&Palette::day());
                if let Some(act) = st.widget.show_modal(ui, &colors) {
                    match act {
                        PollVotersAction::ReloadResults => {
                            st.reload_count += 1;
                            if let Ok(Some(updated)) = st
                                .session
                                .reload_poll_results(st.widget.peer_id, st.widget.msg_id)
                            {
                                st.widget.apply_poll_results(updated);
                            }
                        }
                        PollVotersAction::LoadStats { dark } => {
                            st.stats_count += 1;
                            let graph = st
                                .session
                                .poll_stats(st.widget.peer_id, st.widget.msg_id, dark)
                                .expect("poll stats graph");
                            st.widget.apply_poll_stats(graph, dark);
                        }
                        _ => {}
                    }
                }
            },
            state,
        );
    harness.step();

    // Click Refresh -> triggers `PollVotersAction::ReloadResults`.
    harness.get_by_label("Refresh").click();
    harness.run_steps(2);
    assert_eq!(harness.state().reload_count, 1);

    // Switch to Poll Stats -> triggers `PollVotersAction::LoadStats` and renders `ChartWidgetState`.
    let view_stats_lbl = tr::lng_polls_view_stats();
    harness.get_by_label(&view_stats_lbl).click();
    harness.run_steps(3);
    assert_eq!(harness.state().stats_count, 1);
    assert!(harness.state().widget.show_stats);
    assert!(harness.state().widget.stats_chart.is_some());
}

#[test]
fn wp64_export_takeout_init_delay_and_cancel_finishes_takeout_session() {
    // 1. Takeout init-delay prompt formatting (`ApiError::parse_takeout_init_delay`).
    let err = ApiError::new(420, "TAKEOUT_INIT_DELAY_7200", "takeout delay");
    let delay =
        takeout_delay_info_from_error(&err, 1_760_000_000).expect("parse TAKEOUT_INIT_DELAY_7200");
    assert_eq!(delay.delay_seconds, 7200);
    assert_eq!(delay.hours, 2);
    assert!(delay.message.contains('2'));

    // 2. Cancelling an active export invokes `ExportSource::finish_export_cancelled`.
    struct CancelTrackingSource {
        inner: InMemoryExportSource,
        cancelled_finished: Arc<AtomicBool>,
    }

    impl ExportSource for CancelTrackingSource {
        async fn start_export(
            &mut self,
            settings: &Settings,
            stats: &tg_export::output::stats::Stats,
        ) -> Result<tg_export::StartInfo, ApiError> {
            self.inner.start_export(settings, stats).await
        }
        async fn request_dialogs_list(
            &mut self,
            on_progress: &mut (dyn FnMut(i32) -> bool + Send),
        ) -> Result<tg_export::data::DialogsInfo, ApiError> {
            self.inner.request_dialogs_list(on_progress).await
        }
        async fn request_personal_info(
            &mut self,
        ) -> Result<tg_export::data::PersonalInfo, ApiError> {
            self.inner.request_personal_info().await
        }
        async fn request_userpics(
            &mut self,
            on_event: &mut (
                     dyn FnMut(
                tg_export::StreamEvent<
                    tg_export::data::UserpicsInfo,
                    tg_export::data::UserpicsSlice,
                >,
            ) -> bool
                         + Send
                 ),
        ) -> Result<(), ApiError> {
            self.inner.request_userpics(on_event).await
        }
        async fn request_stories(
            &mut self,
            on_event: &mut (
                     dyn FnMut(
                tg_export::StreamEvent<tg_export::data::StoriesInfo, tg_export::data::StoriesSlice>,
            ) -> bool
                         + Send
                 ),
        ) -> Result<(), ApiError> {
            self.inner.request_stories(on_event).await
        }
        async fn request_profile_music(
            &mut self,
            on_event: &mut (
                     dyn FnMut(
                tg_export::StreamEvent<
                    tg_export::data::ProfileMusicInfo,
                    tg_export::data::ProfileMusicSlice,
                >,
            ) -> bool
                         + Send
                 ),
        ) -> Result<(), ApiError> {
            self.inner.request_profile_music(on_event).await
        }
        async fn request_contacts(&mut self) -> Result<tg_export::data::ContactsList, ApiError> {
            self.inner.request_contacts().await
        }
        async fn request_sessions(&mut self) -> Result<tg_export::data::SessionsList, ApiError> {
            self.inner.request_sessions().await
        }
        async fn request_other_data(
            &mut self,
            path: &str,
        ) -> Result<tg_export::data::File, ApiError> {
            self.inner.request_other_data(path).await
        }
        async fn request_messages(
            &mut self,
            info: &tg_export::data::DialogInfo,
            on_event: &mut (
                     dyn FnMut(
                tg_export::StreamEvent<tg_export::data::DialogInfo, tg_export::data::MessagesSlice>,
            ) -> bool
                         + Send
                 ),
        ) -> Result<(), ApiError> {
            self.inner.request_messages(info, on_event).await
        }
        async fn request_topic_messages(
            &mut self,
            peer_id: tg_data::PeerId,
            input_peer: &InputPeer,
            topic_root_id: i32,
            on_event: &mut (
                     dyn FnMut(tg_export::StreamEvent<i32, tg_export::data::MessagesSlice>) -> bool
                         + Send
                 ),
        ) -> Result<(), ApiError> {
            self.inner
                .request_topic_messages(peer_id, input_peer, topic_root_id, on_event)
                .await
        }
        async fn finish_export(&mut self) -> Result<(), ApiError> {
            self.inner.finish_export().await
        }
        async fn finish_export_cancelled(&mut self) -> Result<(), ApiError> {
            self.cancelled_finished.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn skip_file(&mut self, random_id: u64) {
            self.inner.skip_file(random_id);
        }
        fn cancel_export_fast(&mut self) {
            self.inner.cancel_export_fast();
        }
    }

    let flag = Arc::new(AtomicBool::new(false));
    let src = CancelTrackingSource {
        inner: InMemoryExportSource::default(),
        cancelled_finished: Arc::clone(&flag),
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let settings = Settings {
        types: ExportTypes::PERSONAL_INFO,
        path: tmp.path().to_string_lossy().into_owned(),
        ..Settings::default()
    };
    let (mut controller, handle) = ExportController::new(src, InputPeer::Empty);
    handle.cancel_fast();

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let final_state = rt.block_on(controller.run(&settings, &Environment::default()));
    assert!(
        matches!(final_state, ExportState::Cancelled),
        "expected Cancelled terminal state, got {final_state:?}"
    );
    assert!(
        flag.load(Ordering::SeqCst),
        "ExportController cancellation must call finish_export_cancelled"
    );
}

// =============================================================================
// Manual Headless `wgpu` Before & After Snapshot Captures (`RUSTGRAM_SNAPSHOT_DIR` / temp dir)
// =============================================================================

#[test]
#[ignore = "Manual WP-64 before-snapshot capture"]
fn capture_wp64_before_snapshots() {
    let _guard = WGPU_WP64_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (size, tag) in [(DESKTOP_SIZE, "1440x900"), (MOBILE_SIZE, "390x844")] {
        // 1. Wallet Overview
        {
            let mut panel = WalletPanel::with_mock_demo();
            panel.select_screen(WalletScreen::Overview);
            render_wgpu_snap(
                size,
                panel,
                &format!("wallet_overview_{tag}_before"),
                |ui, p| {
                    let palette = Palette::day();
                    let _ = p.show(ui, &palette, true);
                },
            );
        }

        // 2. Wallet Collectibles
        {
            let mut panel = WalletPanel::with_mock_demo();
            panel.select_screen(WalletScreen::Collectibles);
            render_wgpu_snap(
                size,
                panel,
                &format!("wallet_collectibles_{tag}_before"),
                |ui, p| {
                    let palette = Palette::day();
                    let _ = p.show(ui, &palette, true);
                },
            );
        }

        // 3. Wallet Backup
        {
            let mut panel = WalletPanel::with_mock_demo();
            panel.select_screen(WalletScreen::Backup);
            render_wgpu_snap(
                size,
                panel,
                &format!("wallet_backup_{tag}_before"),
                |ui, p| {
                    let palette = Palette::day();
                    let _ = p.show(ui, &palette, true);
                },
            );
        }

        // 4. Group Call Panel
        {
            let mut backend = MockCallsBackend::new();
            backend.setup_demo_group_call(10_000);
            let panel = GroupCallPanelState::default();
            render_wgpu_snap(
                size,
                (backend, panel),
                &format!("group_call_panel_{tag}_before"),
                |ui, (b, p)| {
                    let palette = CallPalette::tdesktop();
                    let snap = b.active_group_call().expect("active group").clone();
                    let rect = ui.available_rect_before_wrap();
                    p.paint_panel_contents(ui, rect, &snap, b, &palette, 10_000);
                },
            );
        }

        // 5. Group Call Settings
        {
            let mut backend = MockCallsBackend::new();
            backend.setup_demo_group_call(10_000);
            let mut panel = GroupCallPanelState::default();
            panel.settings_dialog.open();
            render_wgpu_snap(
                size,
                (backend, panel),
                &format!("group_call_settings_{tag}_before"),
                |ui, (b, p)| {
                    let palette = CallPalette::tdesktop();
                    let snap = b.active_group_call().expect("active group").clone();
                    p.settings_dialog.show(ui.ctx(), b, &snap, &palette, 10_000);
                },
            );
        }

        // 6. Recent Calls Box
        {
            let backend = MockCallsBackend::new();
            let box_ui = RecentCallsBox {
                open: true,
                ..Default::default()
            };
            render_wgpu_snap(
                size,
                (backend, box_ui),
                &format!("recent_calls_{tag}_before"),
                |ui, (b, rbox)| {
                    let palette = CallPalette::tdesktop();
                    let _ = rbox.show(ui.ctx(), b, &palette);
                },
            );
        }

        // 7. Poll Results Modal
        {
            let widget = sample_poll_widget();
            render_wgpu_snap(
                size,
                widget,
                &format!("poll_results_{tag}_before"),
                |ui, w| {
                    let colors = HistoryColors::from_palette(&Palette::day());
                    let _ = w.show_modal(ui, &colors);
                },
            );
        }
    }
}

#[test]
#[ignore = "Manual WP-64 after-snapshot capture"]
fn capture_wp64_after_snapshots() {
    let _guard = WGPU_WP64_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for (size, tag) in [(DESKTOP_SIZE, "1440x900"), (MOBILE_SIZE, "390x844")] {
        // 1. Wallet Overview (with sticker, custom backdrop, cloud backup enabled & gasless badge)
        {
            let mut panel = WalletPanel::with_mock_demo();
            let _ = panel.backend_mut().enable_cloud_backup();
            if let Some(gift) = panel
                .backend_mut()
                .collectibles()
                .into_iter()
                .find(|n| matches!(n.kind, NftKind::TelegramGift))
            {
                let _ = panel.backend_mut().set_card_stickers(
                    &[CardStickerInput {
                        coordinates: CardStickerCoordinates {
                            x: 78.0,
                            y: 28.0,
                            w: 22.0,
                            h: 22.0,
                            rotation: 0.0,
                            radius: None,
                        },
                        nft_address: gift.address.clone(),
                    }],
                    Some(&gift.address),
                );
            }
            panel.select_screen(WalletScreen::Overview);
            render_wgpu_snap(
                size,
                panel,
                &format!("wallet_overview_{tag}_after"),
                |ui, p| {
                    let palette = Palette::day();
                    let _ = p.show(ui, &palette, true);
                },
            );
        }

        // 2. Wallet Card Editor
        {
            let mut panel = WalletPanel::with_mock_demo();
            if let Some(gift) = panel
                .backend_mut()
                .collectibles()
                .into_iter()
                .find(|n| matches!(n.kind, NftKind::TelegramGift))
            {
                let _ = panel.backend_mut().set_card_stickers(
                    &[CardStickerInput {
                        coordinates: CardStickerCoordinates {
                            x: 72.0,
                            y: 35.0,
                            w: 22.0,
                            h: 22.0,
                            rotation: 0.0,
                            radius: None,
                        },
                        nft_address: gift.address.clone(),
                    }],
                    Some(&gift.address),
                );
            }
            let state_snapshot = panel.backend_mut().wallet_card_state();
            panel.card_editor_state_mut().draft = state_snapshot;
            panel.card_editor_state_mut().initialized = true;
            panel.select_screen(WalletScreen::CardEditor);
            render_wgpu_snap(
                size,
                panel,
                &format!("wallet_card_editor_{tag}_after"),
                |ui, p| {
                    let palette = Palette::day();
                    let _ = p.show(ui, &palette, true);
                },
            );
        }

        // 3. Wallet Collectibles (responsive 1-col @ 390px + Move to My Gifts button)
        {
            let mut panel = WalletPanel::with_mock_demo();
            if let Some(gift) = panel
                .backend_mut()
                .collectibles()
                .into_iter()
                .find(|n| matches!(n.kind, NftKind::TelegramGift))
            {
                panel.collectibles_state_mut().selected_nft_address = Some(gift.address);
            }
            panel.select_screen(WalletScreen::Collectibles);
            render_wgpu_snap(
                size,
                panel,
                &format!("wallet_collectibles_{tag}_after"),
                |ui, p| {
                    let palette = Palette::day();
                    let _ = p.show(ui, &palette, true);
                },
            );
        }

        // 4. Wallet Move-to-Gifts Confirmation Sheet
        {
            let mut panel = WalletPanel::with_mock_demo();
            if let Some(gift) = panel
                .backend_mut()
                .collectibles()
                .into_iter()
                .find(|n| matches!(n.kind, NftKind::TelegramGift))
            {
                let quote = panel
                    .backend_mut()
                    .prepare_move_to_gifts(&gift.address, "DeskCalendar-45754", false)
                    .ok();
                panel.collectibles_state_mut().move_to_gifts.open_for(gift);
                panel.collectibles_state_mut().move_to_gifts.pending_quote = quote;
            }
            panel.select_screen(WalletScreen::Collectibles);
            render_wgpu_snap(
                size,
                panel,
                &format!("wallet_move_to_gifts_{tag}_after"),
                |ui, p| {
                    let palette = Palette::day();
                    let _ = p.show(ui, &palette, true);
                },
            );
        }

        // 5. Wallet Backup (with Cloud Backup card enabled)
        {
            let mut panel = WalletPanel::with_mock_demo();
            let _ = panel.backend_mut().enable_cloud_backup();
            panel.select_screen(WalletScreen::Backup);
            render_wgpu_snap(
                size,
                panel,
                &format!("wallet_backup_{tag}_after"),
                |ui, p| {
                    let palette = Palette::day();
                    let _ = p.show(ui, &palette, true);
                },
            );
        }

        // 6. Group Call Panel (with Scheduled Group Call countdown bar + "+ Invite" button)
        {
            let mut backend = MockCallsBackend::new();
            let future_unix = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1_800_000_000, |d| d.as_secs())
                + 5_420) as i32;
            backend.setup_demo_scheduled_group_call(10_000, future_unix);
            let panel = GroupCallPanelState::default();
            render_wgpu_snap(
                size,
                (backend, panel),
                &format!("group_call_panel_{tag}_after"),
                |ui, (b, p)| {
                    let palette = CallPalette::tdesktop();
                    let snap = b.active_group_call().expect("active group").clone();
                    let rect = ui.available_rect_before_wrap();
                    p.paint_panel_contents(ui, rect, &snap, b, &palette, 10_000);
                },
            );
        }

        // 7. Group Call Settings (with Export Link + Invite Members)
        {
            let mut backend = MockCallsBackend::new();
            backend.setup_demo_group_call(10_000);
            let exported = backend.export_group_call_invite(true).ok();
            let mut panel = GroupCallPanelState::default();
            panel.settings_dialog.open();
            panel.settings_dialog.exported_link = exported;
            render_wgpu_snap(
                size,
                (backend, panel),
                &format!("group_call_settings_{tag}_after"),
                |ui, (b, p)| {
                    let palette = CallPalette::tdesktop();
                    let snap = b.active_group_call().expect("active group").clone();
                    p.settings_dialog.show(ui.ctx(), b, &snap, &palette, 10_000);
                },
            );
        }

        // 8. Group Call Invite Picker
        {
            let mut backend = MockCallsBackend::new();
            backend.setup_demo_group_call(10_000);
            let mut panel = GroupCallPanelState::default();
            panel.invite_dialog.open();
            panel
                .invite_dialog
                .selected_peers
                .push(MockCallsBackend::PEER_QA_BOT);
            render_wgpu_snap(
                size,
                (backend, panel),
                &format!("group_call_invite_{tag}_after"),
                |ui, (b, p)| {
                    let palette = CallPalette::tdesktop();
                    let snap = b.active_group_call().expect("active group").clone();
                    p.invite_dialog.show(ui.ctx(), b, &snap, &palette);
                },
            );
        }

        // 9. Recent Calls Box (with Clear All confirmation expanded)
        {
            let backend = MockCallsBackend::new();
            let box_ui = RecentCallsBox {
                open: true,
                clear_confirm_open: true,
                ..Default::default()
            };
            render_wgpu_snap(
                size,
                (backend, box_ui),
                &format!("recent_calls_{tag}_after"),
                |ui, (b, rbox)| {
                    let palette = CallPalette::tdesktop();
                    let _ = rbox.show(ui.ctx(), b, &palette);
                },
            );
        }

        // 10. Poll Results Modal (with Refresh + View Stats pill)
        {
            let widget = sample_poll_widget();
            render_wgpu_snap(
                size,
                widget,
                &format!("poll_results_{tag}_after"),
                |ui, w| {
                    let colors = HistoryColors::from_palette(&Palette::day());
                    let _ = w.show_modal(ui, &colors);
                },
            );
        }

        // 11. Poll Stats Modal (with ChartWidgetState rendered)
        {
            let mut widget = sample_poll_widget();
            widget.apply_poll_stats(sample_poll_stats_graph(), false);
            render_wgpu_snap(size, widget, &format!("poll_stats_{tag}_after"), |ui, w| {
                let colors = HistoryColors::from_palette(&Palette::day());
                let _ = w.show_modal(ui, &colors);
            });
        }
    }
}
