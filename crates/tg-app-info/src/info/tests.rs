//! Unit tests for `info/` and `boxes/` logic plus `#[ignore]` GPU screenshot capture.

use tg_data::rights::{ChatAdminRights, ChatBannedRights};
use tg_editor::{CropPreset, CropType, PointF, Rect as EdRect, SizeF};

use crate::boxes::about::{AboutBoxState, GPL3_LICENSE_URL, TGFORK_VERSION, UPSTREAM_TDESKTOP_URL};
use crate::boxes::add_contact::AddContactBoxState;
use crate::boxes::confirm::{ConfirmBoxState, ConfirmTag};
use crate::boxes::create_peer::{
    ChannelVisibility, CreateChannelBoxState, CreateChannelStep, CreateGroupBoxState,
    CreateGroupStep,
};
use crate::boxes::delete_messages::DeleteMessagesBoxState;
use crate::boxes::edit_peer::{
    ALL_MEDIA_RIGHTS, AutoDeleteTimerBoxState, EditAdminRightsBoxState, EditPeerInfoBoxState,
    EditPermissionsBoxState, STICKERS_AND_GIFS_RIGHTS, count_allowed_permissions,
    is_permission_allowed, set_permission_allowed,
};
use crate::boxes::language::LanguageBoxState;
use crate::boxes::peer_list::{PeerListBoxState, PeerListPurpose, PeerSelectionMode};
use crate::boxes::photo_crop::PhotoCropBoxState;
use crate::boxes::report::{ReportBoxState, ReportStep};
use crate::boxes::share::ShareBoxState;
use crate::boxes::terms::TermsOfServiceBoxState;
use crate::boxes::username::UsernameBoxState;
use crate::info::backend::{InfoBackend, InfoPeerKind, MemberRole, ReportReason, SharedMediaTab};
use crate::info::media::MediaProvider;
use crate::info::mock::{MockBackend, mock_peers};
use crate::info::top_bar::InfoTopBar;
use crate::info::values::{
    AUTO_DELETE_TTL_PRESETS, Birthday, SLOWMODE_DELAYS_SECONDS, UsernameAvailability,
    format_auto_delete_ttl, format_birthday, format_duration, format_file_size,
    format_members_status, format_phone, format_slowmode, format_subscribers_status,
    is_valid_phone, slowmode_delay_by_index, slowmode_index_for_seconds, validate_username_syntax,
};
use crate::info::wrap_widget::{InfoSection, InfoWrapWidget};

#[test]
fn username_syntax_and_availability_rules() {
    assert_eq!(validate_username_syntax(""), UsernameAvailability::Empty);
    assert_eq!(validate_username_syntax("   "), UsernameAvailability::Empty);
    assert_eq!(
        validate_username_syntax("abcd"),
        UsernameAvailability::TooShort
    );
    assert_eq!(
        validate_username_syntax("a".repeat(33).as_str()),
        UsernameAvailability::TooLong
    );
    assert_eq!(
        validate_username_syntax("bad-char"),
        UsernameAvailability::BadSymbols
    );
    assert_eq!(
        validate_username_syntax("9starts_digit"),
        UsernameAvailability::InvalidFormat
    );
    assert_eq!(
        validate_username_syntax("_leading"),
        UsernameAvailability::InvalidFormat
    );
    assert_eq!(
        validate_username_syntax("trailing_"),
        UsernameAvailability::InvalidFormat
    );
    assert_eq!(
        validate_username_syntax("double__under"),
        UsernameAvailability::InvalidFormat
    );
    assert_eq!(
        validate_username_syntax("@valid_handle_2026"),
        UsernameAvailability::Available
    );

    let backend = MockBackend::new();
    // Existing handle on USER_MIRA is occupied for others, available for USER_MIRA itself.
    assert_eq!(
        backend.check_username("mira_vance", None),
        UsernameAvailability::Occupied
    );
    assert_eq!(
        backend.check_username("mira_vance", Some(mock_peers::USER_MIRA)),
        UsernameAvailability::Available
    );
    assert_eq!(
        backend.check_username("telegram", None),
        UsernameAvailability::Occupied
    );
    assert_eq!(
        backend.check_username("brand_new_handle", None),
        UsernameAvailability::Available
    );
}

#[test]
fn phone_and_value_formatters_match_tdesktop() {
    assert!(is_valid_phone("+999 661 0142"));
    assert!(is_valid_phone("333"));
    assert!(is_valid_phone("42"));
    assert!(is_valid_phone("4242"));
    assert!(is_valid_phone("42777"));
    assert!(!is_valid_phone("12345"));
    assert!(!is_valid_phone(""));

    assert_eq!(format_phone("9996610142"), "+999 661 01 42");
    assert_eq!(format_phone("+999 661 0142"), "+999 661 0142");

    let bday_full = Birthday {
        day: 14,
        month: 5,
        year: Some(1994),
    };
    assert_eq!(
        format_birthday(&bday_full, Some(2026)),
        "May 14, 1994 (32 years old)"
    );
    let bday_no_year = Birthday {
        day: 3,
        month: 11,
        year: None,
    };
    assert_eq!(format_birthday(&bday_no_year, Some(2026)), "November 3");

    assert_eq!(format_members_status(9, 4), "9 members, 4 online");
    assert_eq!(format_members_status(1, 1), "1 member");
    assert_eq!(format_subscribers_status(4_280), "4,280 subscribers");
    assert_eq!(format_duration(47), "0:47");
    assert_eq!(format_duration(3725), "1:02:05");
    assert_eq!(format_file_size(512), "512 B");
    assert_eq!(format_file_size(2 * 1024 * 1024), "2.0 MB");

    assert_eq!(SLOWMODE_DELAYS_SECONDS.len(), 8);
    assert_eq!(slowmode_delay_by_index(0), 0);
    assert_eq!(slowmode_delay_by_index(7), 3600);
    assert_eq!(slowmode_index_for_seconds(10), 2);
    assert_eq!(format_slowmode(0), "Off");
    assert_eq!(format_slowmode(30), "30s");
    assert_eq!(format_slowmode(300), "5m");
    assert_eq!(format_slowmode(3600), "1h");

    assert_eq!(AUTO_DELETE_TTL_PRESETS.len(), 17);
    assert_eq!(format_auto_delete_ttl(0), "Off");
    assert_eq!(format_auto_delete_ttl(86_400), "1 day");
    assert_eq!(format_auto_delete_ttl(86_400 * 7), "1 week");
    assert_eq!(format_auto_delete_ttl(86_400 * 31), "1 month");
    assert_eq!(format_auto_delete_ttl(86_400 * 365), "1 year");
}

#[test]
fn mock_backend_covers_all_peer_kinds_and_media_sections() {
    let mut backend = MockBackend::new();

    let user = backend.peer_profile(mock_peers::USER_MIRA).unwrap();
    assert_eq!(user.kind, InfoPeerKind::User);
    assert_eq!(user.usernames.len(), 2);
    assert!(user.birthday.is_some());
    assert!(!backend.common_groups(mock_peers::USER_MIRA).is_empty());

    let group = backend.peer_profile(mock_peers::GROUP_CORE).unwrap();
    assert_eq!(group.kind, InfoPeerKind::Supergroup);
    let members = backend.peer_members(mock_peers::GROUP_CORE, "");
    assert!(members.len() >= 8);
    assert!(matches!(members[0].role, MemberRole::Owner { .. }));
    let filtered = backend.peer_members(mock_peers::GROUP_CORE, "protocol");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "Kaelen Orlov");

    let channel = backend.peer_profile(mock_peers::CHANNEL_RELEASES).unwrap();
    assert_eq!(channel.kind, InfoPeerKind::Channel);
    assert!(channel.linked_discussion_peer.is_some());

    let bot = backend.peer_profile(mock_peers::BOT_BUILD).unwrap();
    assert_eq!(bot.kind, InfoPeerKind::Bot);
    assert!(bot.has_mini_app);

    let saved = backend.peer_profile(mock_peers::SAVED_MESSAGES).unwrap();
    assert_eq!(saved.kind, InfoPeerKind::SavedMessages);

    // Notifications toggle persists
    let muted = backend.toggle_notifications(mock_peers::USER_MIRA);
    assert!(muted);
    assert!(backend.peer_profile(mock_peers::USER_MIRA).unwrap().muted);

    // Media provider groups items by month and filters by query
    for tab in SharedMediaTab::ALL {
        let items = MediaProvider::load(&backend, mock_peers::USER_MIRA, tab, "");
        assert!(!items.is_empty(), "expected mock items for {tab:?}");
        let grouped = MediaProvider::group_by_date(&items);
        assert!(!grouped.is_empty());
    }
    let pdf_files = MediaProvider::load(
        &backend,
        mock_peers::USER_MIRA,
        SharedMediaTab::Files,
        "pdf",
    );
    assert_eq!(pdf_files.len(), 1);
}

#[test]
fn permissions_bitflag_inversion_and_admin_rights_editing() {
    let mut banned = ChatBannedRights::empty();
    assert!(is_permission_allowed(banned, ChatBannedRights::SEND_PLAIN));
    assert!(is_permission_allowed(banned, ALL_MEDIA_RIGHTS));

    set_permission_allowed(&mut banned, ChatBannedRights::SEND_PHOTOS, false);
    assert!(banned.contains(ChatBannedRights::SEND_PHOTOS));
    assert!(!is_permission_allowed(
        banned,
        ChatBannedRights::SEND_PHOTOS
    ));
    assert!(!is_permission_allowed(banned, ALL_MEDIA_RIGHTS));

    set_permission_allowed(&mut banned, STICKERS_AND_GIFS_RIGHTS, false);
    assert!(banned.contains(ChatBannedRights::SEND_STICKERS));
    assert!(banned.contains(ChatBannedRights::SEND_GIFS));
    assert!(banned.contains(ChatBannedRights::SEND_GAMES));
    assert!(banned.contains(ChatBannedRights::SEND_INLINE));

    let (allowed, total) = count_allowed_permissions(banned, true);
    assert_eq!(total, 15);
    assert_eq!(allowed, 13);

    let mut backend = MockBackend::new();
    let group = backend.peer_profile(mock_peers::GROUP_CORE).unwrap();
    let mut perm_box = EditPermissionsBoxState::from_profile(&group);
    perm_box.banned_rights = banned;
    perm_box.slowmode_index = 4; // 60s
    assert_eq!(perm_box.slowmode_seconds(), 60);
    perm_box.submit(&mut backend).unwrap();

    let updated = backend.peer_profile(mock_peers::GROUP_CORE).unwrap();
    assert_eq!(updated.default_banned_rights, banned);
    assert_eq!(updated.slowmode_seconds, 60);

    // Promote a regular member to admin with custom title, then dismiss
    let nolan = backend
        .peer_members(mock_peers::GROUP_CORE, "Nolan")
        .into_iter()
        .next()
        .unwrap();
    let mut admin_box = EditAdminRightsBoxState::from_member(&updated, &nolan);
    admin_box.rights = ChatAdminRights::DELETE_MESSAGES | ChatAdminRights::PIN_MESSAGES;
    admin_box.custom_title = "media".to_owned();
    admin_box.submit(&mut backend).unwrap();

    let promoted = backend
        .peer_members(mock_peers::GROUP_CORE, "Nolan")
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(promoted.role.badge_label(), Some("media"));
    assert_eq!(
        promoted.role.admin_rights(),
        ChatAdminRights::DELETE_MESSAGES | ChatAdminRights::PIN_MESSAGES
    );

    admin_box.dismiss_admin(&mut backend).unwrap();
    let demoted = backend
        .peer_members(mock_peers::GROUP_CORE, "Nolan")
        .into_iter()
        .next()
        .unwrap();
    assert!(!demoted.role.is_admin_or_owner());
}

#[test]
fn create_group_channel_add_contact_and_edit_peer_flows() {
    let mut backend = MockBackend::new();

    // 1. Add Contact validation + creation
    let mut add = AddContactBoxState::new();
    assert!(add.submit(&mut backend).is_err());
    add.first_name = "Caleb".to_owned();
    add.last_name = "Mercer".to_owned();
    add.phone = "123".to_owned();
    assert!(add.submit(&mut backend).is_err());
    add.phone = "+999 661 0955".to_owned();
    let new_contact_id = add.submit(&mut backend).unwrap();
    assert_eq!(
        backend.peer_profile(new_contact_id).unwrap().title,
        "Caleb Mercer"
    );

    // 2. Create Group 2-step flow
    let mut cg = CreateGroupBoxState::new();
    assert_eq!(cg.step, CreateGroupStep::Details);
    assert!(cg.advance_to_members().is_err());
    cg.name = "Graphics Squad".to_owned();
    cg.auto_delete_preset_idx = 7; // 1 week
    cg.advance_to_members().unwrap();
    assert_eq!(cg.step, CreateGroupStep::AddMembers);
    cg.member_picker.toggle_peer(mock_peers::USER_MIRA);
    cg.member_picker.toggle_peer(mock_peers::USER_TESSA);
    let gid = cg.submit(&mut backend).unwrap();
    let created_group = backend.peer_profile(gid).unwrap();
    assert_eq!(created_group.title, "Graphics Squad");
    assert_eq!(created_group.members_count, 3); // owner + 2 invited
    assert_eq!(created_group.auto_delete_ttl_seconds, 86_400 * 7);

    // 3. Create Channel 2-step flow (Public with username validation)
    let mut cc = CreateChannelBoxState::new();
    assert!(cc.advance_to_type().is_err());
    cc.name = "Shader Notes".to_owned();
    cc.description = "Daily rendering benchmarks".to_owned();
    cc.advance_to_type().unwrap();
    assert_eq!(cc.step, CreateChannelStep::ChannelType);
    cc.visibility = ChannelVisibility::Public;
    cc.username = "mira_vance".to_owned(); // occupied
    assert!(cc.submit(&mut backend).is_err());
    cc.username = "shader_notes_rs".to_owned();
    cc.no_forwards = true;
    let cid = cc.submit(&mut backend).unwrap();
    let created_ch = backend.peer_profile(cid).unwrap();
    assert_eq!(created_ch.primary_username(), Some("shader_notes_rs"));
    assert!(created_ch.no_forwards);

    // 4. Edit Peer Info + Auto-Delete + Linked Discussion
    let mut edit = EditPeerInfoBoxState::from_profile(&created_ch);
    edit.title = "Shader Notes v2".to_owned();
    edit.submit(&mut backend).unwrap();
    assert_eq!(backend.peer_profile(cid).unwrap().title, "Shader Notes v2");

    let ttl_box = AutoDeleteTimerBoxState::new(cid, 86_400 * 3);
    ttl_box.submit(&mut backend).unwrap();
    assert_eq!(
        backend.peer_profile(cid).unwrap().auto_delete_ttl_seconds,
        86_400 * 3
    );

    backend.set_linked_discussion(cid, Some(gid)).unwrap();
    assert_eq!(
        backend
            .peer_profile(cid)
            .unwrap()
            .linked_discussion_peer
            .unwrap()
            .0,
        gid
    );
}

#[test]
fn peer_list_share_report_delete_username_language_about_tos_and_crop() {
    let mut backend = MockBackend::new();

    // PeerList single vs multi & search filtering
    let contacts = backend.contacts();
    let mut picker = PeerListBoxState::new(
        "Pick",
        "OK",
        PeerSelectionMode::Single,
        PeerListPurpose::Generic,
    );
    picker.toggle_peer(mock_peers::USER_MIRA);
    picker.toggle_peer(mock_peers::USER_KAELEN);
    assert_eq!(picker.selected, vec![mock_peers::USER_KAELEN]);
    picker.search_query = "sora".to_owned();
    assert_eq!(picker.filter_candidates(&contacts).len(), 1);

    // Share box
    let mut share = ShareBoxState::new(
        vec![101, 102],
        Some("https://t.me/rustgram_core".to_owned()),
    );
    assert!(share.submit(&mut backend).is_err());
    share.toggle_target(mock_peers::USER_MIRA);
    share.toggle_target(mock_peers::GROUP_CORE);
    share.comment = "Check this out".to_owned();
    assert_eq!(share.submit(&mut backend).unwrap(), 2);
    assert_eq!(backend.forward_records.len(), 1);

    // Report box
    let mut rep = ReportBoxState::new(mock_peers::CHANNEL_DIGEST, "Rust GUI Digest");
    rep.reason = ReportReason::Other;
    rep.next_step();
    assert_eq!(rep.step, ReportStep::Details);
    assert!(rep.submit(&mut backend).is_err()); // Other requires comment
    rep.comment = "Synthetic test report".to_owned();
    rep.submit(&mut backend).unwrap();
    assert_eq!(backend.submitted_reports.len(), 1);

    // Delete messages box
    let group = backend.peer_profile(mock_peers::GROUP_CORE).unwrap();
    let mut del_leave = DeleteMessagesBoxState::for_delete_or_leave(&group, false);
    assert_eq!(del_leave.confirm_label(), "Leave");
    del_leave.revoke_for_everyone = true;
    assert_eq!(del_leave.confirm_label(), "Delete");
    let del_msgs = DeleteMessagesBoxState::for_messages(&group, vec![1, 2, 3]);
    assert!(del_msgs.prompt_text().contains("3 selected messages"));
    assert_eq!(del_msgs.submit(&mut backend).unwrap(), 3);

    // Username box
    let user = backend.peer_profile(mock_peers::USER_MIRA).unwrap();
    let mut ubox = UsernameBoxState::from_profile(&user);
    assert_eq!(ubox.collectibles.len(), 1);
    ubox.draft = "mira_rust_dev".to_owned();
    ubox.submit(&mut backend).unwrap();
    assert_eq!(
        backend
            .peer_profile(mock_peers::USER_MIRA)
            .unwrap()
            .editable_username(),
        Some("mira_rust_dev")
    );

    // Photo crop box backed by tg-editor
    let mut crop_box = PhotoCropBoxState::sample_profile_photo("Avatar", [0x40, 0xA7, 0xE3]);
    crop_box.controller.apply_transform(
        EdRect::new(0, 0, 320, 260),
        PointF::new(20.0, 20.0),
        0,
        false,
        SizeF::new(280.0, 210.0),
    );
    crop_box.rotate_90_cw();
    assert_eq!(crop_box.modifications.angle, 90);
    crop_box.toggle_flip();
    assert!(crop_box.modifications.flipped);
    crop_box.set_preset(CropPreset::Square);
    assert_eq!(crop_box.modifications.crop_type, CropType::Ellipse);
    let final_img = crop_box.finalize().unwrap();
    assert!(final_img.width() >= 640 && final_img.height() >= 640);

    // Language box
    let langs = backend.languages();
    let mut lang_box = LanguageBoxState::new(backend.active_language());
    lang_box.search_query = "deutsch".to_owned();
    assert_eq!(lang_box.filter_languages(&langs).len(), 1);
    lang_box.selected_lang_id = "de".to_owned();
    lang_box.apply(&mut backend).unwrap();
    assert_eq!(backend.active_language(), "de");

    // About box constants
    assert!(!TGFORK_VERSION.is_empty());
    assert!(AboutBoxState::version_line().contains(TGFORK_VERSION));
    assert!(UPSTREAM_TDESKTOP_URL.contains("telegramdesktop/tdesktop"));
    assert!(GPL3_LICENSE_URL.contains("gpl-3.0"));

    // Terms of Service box
    let mut tos_box = TermsOfServiceBoxState::new(backend.terms_of_service());
    assert!(tos_box.accept(&mut backend).is_err()); // min age unchecked
    tos_box.age_confirmed = true;
    tos_box.accept(&mut backend).unwrap();
    assert!(backend.terms_of_service().accepted);

    // Confirm box modal descriptor
    let cbox = ConfirmBoxState::new(
        "Delete?",
        "Confirm action",
        "Delete",
        ConfirmTag::Custom("x".to_owned()),
    )
    .attention(true)
    .with_checkbox("Also block", false);
    assert_eq!(cbox.to_modal("c1").buttons.len(), 2);

    // InfoWrapWidget navigation stack & TopBar Escape handling
    let mut wrap = InfoWrapWidget::for_peer(mock_peers::USER_MIRA);
    assert_eq!(wrap.back_stack_len(), 0);
    wrap.push_section(InfoSection::CommonGroups {
        peer_id: mock_peers::USER_MIRA,
    });
    assert_eq!(wrap.back_stack_len(), 1);
    assert!(wrap.go_back());
    assert_eq!(wrap.back_stack_len(), 0);

    let mut top = InfoTopBar::new();
    top.open_search();
    assert!(top.is_search_open());
    assert!(top.handle_escape());
    assert!(!top.is_search_open());
}

/// Captures GPU-rendered screenshots of:
/// 1. User profile (`crates/tg-app/docs/info_user.png`)
/// 2. Group profile with members (`crates/tg-app/docs/info_group.png`)
/// 3. Create Group wizard modal over workspace (`crates/tg-app/docs/info_create_group.png`)
///
/// Run with:
/// `cargo test -p tg-app capture_info_screenshots -- --ignored --nocapture`
#[test]
#[ignore]
fn capture_info_screenshots() {
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use eframe::egui;
    use egui::{CornerRadius, Pos2, Rect, Stroke, Vec2};
    use tg_ui::apply_tdesktop_visuals;
    use tg_ui::icons::invalidate_context_icon_cache;
    use tg_ui::stand_in::Colors;

    use crate::boxes::{BoxHost, BoxKind};
    use crate::info::colors::InfoColors;
    use crate::info::mock::{MockBackend, mock_peers};
    use crate::info::wrap_widget::InfoWrapWidget;

    #[derive(Clone, Copy)]
    enum Scene {
        UserProfile,
        UserProfileNight,
        GroupProfile,
        CreateGroupStep1,
        CreateGroupStep2,
    }

    struct CaptureApp {
        colors: InfoColors,
        backend: MockBackend,
        info_wrap: InfoWrapWidget,
        boxes: BoxHost,
        scenes: Vec<(Scene, PathBuf)>,
        current_idx: usize,
        scene_frames: u32,
        scene_started_at: std::time::Instant,
        requested_idx: Option<usize>,
        completed_count: Arc<AtomicUsize>,
        scene_initialized: bool,
        theme_gen: u64,
    }

    impl CaptureApp {
        fn setup_scene(&mut self, ctx: &egui::Context, scene: Scene) {
            self.backend = MockBackend::new();
            self.boxes = BoxHost::new();
            self.colors = match scene {
                Scene::UserProfileNight => InfoColors::night(),
                _ => InfoColors::day(),
            };
            self.theme_gen += 1;
            invalidate_context_icon_cache(ctx, self.theme_gen);
            apply_tdesktop_visuals(ctx, &self.colors);
            match scene {
                Scene::UserProfile | Scene::UserProfileNight => {
                    self.info_wrap = InfoWrapWidget::for_peer(mock_peers::USER_MIRA);
                    self.info_wrap.show_inline_media = true;
                }
                Scene::GroupProfile => {
                    self.info_wrap = InfoWrapWidget::for_peer(mock_peers::GROUP_CORE);
                    self.info_wrap.show_inline_members = true;
                    self.info_wrap.show_inline_media = false;
                }
                Scene::CreateGroupStep1 => {
                    self.info_wrap = InfoWrapWidget::for_peer(mock_peers::GROUP_CORE);
                    self.info_wrap.show_inline_members = true;
                    self.info_wrap.show_inline_media = false;
                    let mut cg = CreateGroupBoxState::new();
                    cg.name = "Rustgram Graphics Team".to_owned();
                    cg.auto_delete_preset_idx = 7;
                    self.boxes.push(ctx, BoxKind::CreateGroup(cg), &self.colors);
                }
                Scene::CreateGroupStep2 => {
                    self.info_wrap = InfoWrapWidget::for_peer(mock_peers::GROUP_CORE);
                    self.info_wrap.show_inline_members = true;
                    self.info_wrap.show_inline_media = false;
                    let mut cg = CreateGroupBoxState::new();
                    cg.name = "Rustgram Graphics Team".to_owned();
                    let _ = cg.advance_to_members();
                    cg.member_picker.toggle_peer(mock_peers::USER_MIRA);
                    cg.member_picker.toggle_peer(mock_peers::USER_KAELEN);
                    cg.member_picker.toggle_peer(mock_peers::USER_TESSA);
                    self.boxes.push(ctx, BoxKind::CreateGroup(cg), &self.colors);
                }
            }
            self.scene_frames = 0;
            self.scene_started_at = std::time::Instant::now();
            self.scene_initialized = true;
        }
    }

    impl eframe::App for CaptureApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            let done = self.completed_count.load(Ordering::Acquire);
            if done > self.current_idx {
                self.current_idx = done;
                self.scene_initialized = false;
                self.requested_idx = None;
            }
            if self.current_idx >= self.scenes.len() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }

            let (scene, path) = self.scenes[self.current_idx].clone();
            if !self.scene_initialized {
                self.setup_scene(&ctx, scene);
            }

            let viewport = ctx.viewport_rect();
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(self.colors.color("windowBgOver")))
                .show(ui, |ui| {
                    let full = ui.max_rect();
                    // Left simulated chat pane + right 400px Info column
                    let info_w = match scene {
                        Scene::UserProfile | Scene::UserProfileNight | Scene::GroupProfile => {
                            420.0_f32.min(full.width())
                        }
                        Scene::CreateGroupStep1 | Scene::CreateGroupStep2 => 360.0,
                    };
                    if matches!(scene, Scene::CreateGroupStep1 | Scene::CreateGroupStep2) {
                        let chat_rect = Rect::from_min_size(
                            full.min,
                            Vec2::new((full.width() - info_w).max(200.0), full.height()),
                        );
                        ui.painter().rect_filled(
                            chat_rect,
                            CornerRadius::ZERO,
                            egui::Color32::from_rgb(0xDB, 0xE7, 0xF0),
                        );
                        ui.painter().line_segment(
                            [
                                Pos2::new(chat_rect.max.x, chat_rect.min.y),
                                Pos2::new(chat_rect.max.x, chat_rect.max.y),
                            ],
                            Stroke::new(1.0, self.colors.color("shadowFg")),
                        );
                        let info_rect =
                            Rect::from_min_max(Pos2::new(chat_rect.max.x, full.min.y), full.max);
                        let mut info_ui = ui.new_child(egui::UiBuilder::new().max_rect(info_rect));
                        let _ = self.info_wrap.show(
                            &mut info_ui,
                            &mut self.backend,
                            true,
                            &self.colors,
                        );
                    } else {
                        let left = full.center().x - info_w * 0.5;
                        let card = Rect::from_min_size(
                            Pos2::new(left, full.min.y),
                            Vec2::new(info_w, full.height()),
                        );
                        let mut info_ui = ui.new_child(egui::UiBuilder::new().max_rect(card));
                        let _ = self.info_wrap.show(
                            &mut info_ui,
                            &mut self.backend,
                            true,
                            &self.colors,
                        );
                    }
                });

            let _ = self
                .boxes
                .show(&ctx, viewport, &mut self.backend, &self.colors);

            self.scene_frames += 1;
            let anim_done =
                self.scene_started_at.elapsed() >= std::time::Duration::from_millis(360);
            if self.requested_idx != Some(self.current_idx) && self.scene_frames >= 10 && anim_done
            {
                self.requested_idx = Some(self.current_idx);
                let completed = Arc::clone(&self.completed_count);
                let target_idx = self.current_idx + 1;
                let ctx_cb = ctx.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::ScreenshotCallback::new(move |img| {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let [w, h] = img.size;
                        let mut raw = Vec::with_capacity(w * h * 4);
                        for px in &img.pixels {
                            let [r, g, b, a] = px.to_srgba_unmultiplied();
                            raw.extend_from_slice(&[r, g, b, a]);
                        }
                        if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                            let _ = buf.save(&path);
                        }
                        completed.store(target_idx, Ordering::Release);
                        ctx_cb.request_repaint();
                    }),
                ));
            } else {
                ctx.request_repaint();
            }
        }
    }

    let docs_dir = tg_app_core::core_types::paths::workspace_app_docs_dir();
    let scenes = vec![
        (Scene::UserProfile, docs_dir.join("info_user.png")),
        (
            Scene::UserProfileNight,
            docs_dir.join("info_user_night.png"),
        ),
        (Scene::GroupProfile, docs_dir.join("info_group.png")),
        (
            Scene::CreateGroupStep1,
            docs_dir.join("info_create_group.png"),
        ),
        (
            Scene::CreateGroupStep2,
            docs_dir.join("info_create_group_members.png"),
        ),
    ];

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustgram Info & Boxes Screenshot Capture")
            .with_inner_size([860.0, 920.0]),
        #[cfg(windows)]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };

    let res = eframe::run_native(
        "Rustgram Info Capture",
        options,
        Box::new(move |cc| {
            let colors = InfoColors::day();
            apply_tdesktop_visuals(&cc.egui_ctx, &colors);
            Ok(Box::new(CaptureApp {
                colors,
                backend: MockBackend::new(),
                info_wrap: InfoWrapWidget::for_peer(mock_peers::USER_MIRA),
                boxes: BoxHost::new(),
                scenes,
                current_idx: 0,
                scene_frames: 0,
                scene_started_at: std::time::Instant::now(),
                requested_idx: None,
                completed_count: Arc::new(AtomicUsize::new(0)),
                scene_initialized: false,
                theme_gen: 0,
            }))
        }),
    );
    assert!(res.is_ok(), "eframe capture failed: {res:?}");
}
