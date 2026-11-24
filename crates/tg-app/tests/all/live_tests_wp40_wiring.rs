//! WP-40 wiring tests: each feature is reached from the live window path, against fakes only.
//!
//! - Platform translation is tried before `messages.translateText` when enabled.
//! - Settings -> Advanced -> Manage dictionaries uses the session's cloud source.
//! - The "Restore windows?" card restores and saves separate chat windows.
//! - Dialog rows play video avatars loaded through `users.getFullUser` + `upload.getFile`.
//! - The BotEarn affiliate button opens the Stars Affiliate screen for that bot.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use tg_data::{
    Message, MsgId as DataMsgId, PeerId as DataPeerId, PeerUpdateFlags, PhotoId,
    TextWithEntities as DataText, User, UserFlags, UserId as DataUserId,
};
use tg_platform::translate_win::{FakeWinTranslateBridge, TranslateProvider, WinTranslateProvider};
use tg_tl::{Identifiable, enums as tl_enums, functions as tl_fns, types as tl_types};

use super::live_tests_wp40_polish::make_live_session;
use tg_app::dialogs::backend::DialogsBackend;
use tg_app::dialogs::video_userpic::{
    DialogVideoUserpicCache, VideoUserpicFetch, VideoUserpicFrame, VideoUserpicState,
    paint_row_video_userpic,
};
use tg_app::history::backend::HistoryBackend;
use tg_app::live::LiveSession;
use tg_app::live::convert::now_unix_i32;
use tg_app::live::video_userpic::set_video_userpic_decoder_live;
use tg_app::payments::{PaymentsScreen, PaymentsScreenHost};
use tg_app::settings::backend::SettingsBackend;
use tg_app::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};
use tg_app::startup::restore_windows::{
    RestoreWindowsAction, RestoreWindowsPolicy, SavedWindowEntry, SavedWindowGeometry,
    SavedWindowsSnapshot,
};
use tg_app::window::chat_preview::{
    RestoreWindowsHost, SavedChatWindow, SavedWindowsManager, WindowShellExtras,
};
use tg_app::window::main_widget::MainWidgetAction;

const FRIEND_UID: u64 = 501;
const FRIEND_UID_TL: i64 = 501;

fn seed_user(live: &LiveSession, uid: u64, premium: bool, video_photo: Option<u64>) {
    let session = live.runtime().session();
    live.runtime().tokio_handle().block_on(async {
        let mut s = session.write().await;
        let mut user = User::new(DataUserId(uid), false);
        user.first_name = "Sample".to_owned();
        user.last_name = format!("Friend{uid}");
        user.access_hash = 9_000 + uid;
        if premium {
            user.flags.insert(UserFlags::PREMIUM);
        }
        if let Some(photo) = video_photo {
            user.common.userpic_photo_id = PhotoId(photo);
            user.common.userpic_has_video = true;
        }
        s.upsert_user(user, PeerUpdateFlags::empty());
    });
}

fn seed_text_message(live: &LiveSession, uid: u64, msg_id: i32, text: &str) {
    let session = live.runtime().session();
    let text = text.to_owned();
    live.runtime().tokio_handle().block_on(async move {
        let mut s = session.write().await;
        let peer = DataPeerId::from_user(DataUserId(uid));
        s.insert_message(Message::new_text(
            peer,
            DataMsgId(i64::from(msg_id)),
            Some(peer),
            now_unix_i32() - 30,
            DataText::plain(text),
        ));
    });
}

fn fake_translator(available: bool) -> Arc<dyn TranslateProvider> {
    let mut bridge =
        FakeWinTranslateBridge::new(available, vec!["de".to_owned()]).with_fail_unknown(true);
    bridge.insert_translation("de", "Good morning", "Guten Morgen");
    Arc::new(WinTranslateProvider::with_bridge(bridge))
}

fn translate_result(text: &str) -> tl_enums::messages::TranslatedText {
    tl_enums::messages::TranslatedText::TranslateResult(tl_types::messages::TranslateResult {
        result: vec![tl_enums::TextWithEntities::TextWithEntities(
            tl_types::TextWithEntities {
                text: text.to_owned(),
                entities: Vec::new(),
            },
        )],
    })
}

// =============================================================================
// 1. Platform translation before `messages.translateText`
// =============================================================================

#[test]
fn wp40_wiring_platform_translation_runs_before_server_translation() {
    let (rpc, mut live) = make_live_session();
    seed_user(&live, FRIEND_UID, false, None);
    seed_text_message(&live, FRIEND_UID, 10, "Good morning");
    let peer = PeerId::from_user(FRIEND_UID);

    // Enabled + available provider: no RPC at all.
    live.set_platform_translator_live(Some(fake_translator(true)));
    live.set_platform_translation_live(true);
    let out = live
        .translate_message(peer, MsgId(10), "de")
        .expect("platform translation");
    assert_eq!(out, "Guten Morgen");
    assert!(
        rpc.recorded().is_empty(),
        "platform path must not call Telegram"
    );

    // Provider unavailable on this host: falls back to `messages.translateText`.
    live.set_platform_translator_live(Some(fake_translator(false)));
    rpc.push_response(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &translate_result("Guten Morgen (server)"),
    );
    let out = live
        .translate_message(peer, MsgId(10), "de")
        .expect("server translation");
    assert_eq!(out, "Guten Morgen (server)");
    assert_eq!(rpc.recorded().len(), 1);

    // Setting off (the live host pushes `CoreSettings::use_platform_translation`): server again.
    live.set_platform_translator_live(Some(fake_translator(true)));
    live.set_platform_translation_live(false);
    rpc.push_response(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &translate_result("Server only"),
    );
    let out = live
        .translate_message(peer, MsgId(10), "de")
        .expect("server translation");
    assert_eq!(out, "Server only");
    assert_eq!(rpc.recorded().len(), 2);

    // Batch: platform translates what it can, the server gets the rest in one call.
    seed_text_message(&live, FRIEND_UID, 11, "Unknown phrase");
    live.set_platform_translation_live(true);
    rpc.push_response(
        tl_fns::messages::TranslateText::CONSTRUCTOR_ID,
        &translate_result("Unbekannter Satz"),
    );
    let pairs = live
        .translate_messages_batch(peer, &[MsgId(10), MsgId(11)], "de")
        .expect("batch translation");
    assert_eq!(
        pairs,
        vec![
            (MsgId(10), "Guten Morgen".to_owned()),
            (MsgId(11), "Unbekannter Satz".to_owned()),
        ]
    );
    assert_eq!(rpc.recorded().len(), 3);
}

// =============================================================================
// 2. Manage dictionaries uses the session's cloud source
// =============================================================================

#[test]
fn wp40_wiring_live_session_provides_cloud_dictionary_source() {
    let (rpc, live) = make_live_session();
    let mut source = live
        .dictionary_cloud_source()
        .expect("live session provides a cloud dictionary source");
    // The manifest load resolves the dictionary channel first; the fake rejects it.
    let err = source.fetch_manifest().expect_err("unmocked resolve fails");
    assert!(err.contains("resolve"), "{err}");
    let ctors: Vec<u32> = rpc.recorded().iter().map(|(c, _)| *c).collect();
    assert_eq!(
        ctors,
        vec![tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID]
    );
}

// =============================================================================
// 3. Restore windows offer + persistence
// =============================================================================

fn saved_snapshot(policy: RestoreWindowsPolicy) -> SavedWindowsSnapshot {
    SavedWindowsSnapshot {
        policy,
        windows: vec![
            SavedWindowEntry {
                id: 1,
                title: "Design Review".to_owned(),
                account_index: 0,
                peer_id: Some(PeerId::from_user(FRIEND_UID).0),
                position: SavedWindowGeometry {
                    w: 600,
                    h: 700,
                    ..SavedWindowGeometry::default()
                },
            },
            SavedWindowEntry {
                id: 2,
                title: "Release Notes".to_owned(),
                account_index: 0,
                peer_id: Some(PeerId::from_channel(77).0),
                position: SavedWindowGeometry::default(),
            },
        ],
    }
}

#[test]
fn wp40_wiring_restore_windows_policies_and_persistence() {
    // Ask: card pending, nothing reopened until the user answers.
    let dir = tempfile::tempdir().expect("tempdir");
    saved_snapshot(RestoreWindowsPolicy::Ask)
        .save_to(dir.path())
        .expect("save");
    let mut saved = SavedWindowsManager::new();
    let mut host = RestoreWindowsHost::default();
    host.install(dir.path(), &mut saved);
    assert!(host.has_pending_offer());
    assert!(saved.open_windows().is_empty());

    // "Always": reopens both windows and stores the policy.
    let toast = host.choose(RestoreWindowsAction::Always, &mut saved);
    assert!(toast.is_some());
    assert_eq!(saved.open_windows().len(), 2);
    assert_eq!(saved.open_windows()[0].width, 600.0);
    assert_eq!(
        SavedWindowsSnapshot::load_from(dir.path()).policy,
        RestoreWindowsPolicy::Always
    );

    // Closing a window rewrites the saved set.
    assert!(saved.close_window(PeerId::from_channel(77)));
    host.save_if_changed(&saved);
    let on_disk = SavedWindowsSnapshot::load_from(dir.path());
    assert_eq!(on_disk.policy, RestoreWindowsPolicy::Always);
    assert_eq!(on_disk.windows.len(), 1);
    assert_eq!(on_disk.windows[0].title, "Design Review");

    // Next launch with `Always`: restored without a card.
    let mut saved2 = SavedWindowsManager::new();
    let mut host2 = RestoreWindowsHost::default();
    host2.install(dir.path(), &mut saved2);
    assert!(!host2.has_pending_offer());
    assert_eq!(saved2.open_windows().len(), 1);

    // `Never`: nothing restored.
    let dir3 = tempfile::tempdir().expect("tempdir");
    saved_snapshot(RestoreWindowsPolicy::Never)
        .save_to(dir3.path())
        .expect("save");
    let mut saved3 = SavedWindowsManager::new();
    let mut host3 = RestoreWindowsHost::default();
    host3.install(dir3.path(), &mut saved3);
    assert!(!host3.has_pending_offer());
    assert!(saved3.open_windows().is_empty());
    assert_eq!(host3.policy(), RestoreWindowsPolicy::Never);
}

#[test]
fn wp40_wiring_restore_windows_card_clicks_restore_and_chat_open_dismisses() {
    let dir = tempfile::tempdir().expect("tempdir");
    saved_snapshot(RestoreWindowsPolicy::Ask)
        .save_to(dir.path())
        .expect("save");

    let mut extras = WindowShellExtras::new();
    let restore = &mut extras.restore;
    restore.install(dir.path(), &mut extras.saved_windows);
    assert!(extras.restore.has_pending_offer());

    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::Vec2::new(900.0, 600.0))
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui_state(
            |ui, extras: &mut WindowShellExtras| {
                let e = extras;
                let _ = e.restore.show(ui.ctx(), &mut e.saved_windows, false);
            },
            extras,
        );
    harness.run();
    harness
        .get_by_label(&tg_lang::tr::lng_restore_windows_restore())
        .click();
    harness.run();
    let state = harness.state();
    assert!(!state.restore.has_pending_offer());
    assert_eq!(state.saved_windows.open_windows().len(), 2);
    // "Restore" (once) keeps the stored policy.
    assert_eq!(
        SavedWindowsSnapshot::load_from(dir.path()).policy,
        RestoreWindowsPolicy::Ask
    );

    // Opening a chat while the card is up dismisses it (`ShowRestoreOffer` activeChatValue).
    let dir2 = tempfile::tempdir().expect("tempdir");
    saved_snapshot(RestoreWindowsPolicy::Ask)
        .save_to(dir2.path())
        .expect("save");
    let mut saved = SavedWindowsManager::new();
    let mut host = RestoreWindowsHost::default();
    host.install(dir2.path(), &mut saved);
    let ctx = egui::Context::default();
    ctx.run_ui(egui::RawInput::default(), |ui| {
        let _ = host.show(ui.ctx(), &mut saved, true);
    })
    .textures_delta
    .clear();
    assert!(!host.has_pending_offer());
    assert!(saved.open_windows().is_empty());
}

#[test]
fn wp40_wiring_window_controller_installs_restore_windows() {
    let dir = tempfile::tempdir().expect("tempdir");
    saved_snapshot(RestoreWindowsPolicy::Always)
        .save_to(dir.path())
        .expect("save");
    let mut ctrl = tg_app::window::controller::WindowController::with_session(
        tg_app::stand_in::mock_session::MockSession::new(true),
        tg_app::stand_in::settings::ShellSettings::default(),
        dir.path().join("shell.json"),
    );
    ctrl.install_restore_windows(dir.path());
    let extras = ctrl.shell_extras_mut();
    assert!(extras.restore.is_installed());
    assert_eq!(extras.saved_windows.open_windows().len(), 2);

    // Opening another separate window is saved for the next launch.
    extras
        .saved_windows
        .open_window(SavedChatWindow::new(PeerId::from_user(9), "Sample Notes"));
    let e = ctrl.shell_extras_mut();
    e.restore.save_if_changed(&e.saved_windows);
    assert_eq!(SavedWindowsSnapshot::load_from(dir.path()).windows.len(), 3);
}

// =============================================================================
// 4. Video avatars in dialog rows
// =============================================================================

fn user_full_with_video_photo(uid: i64, photo_id: i64) -> tl_enums::users::UserFull {
    let photo = tl_enums::Photo::Photo(tl_types::Photo {
        has_stickers: false,
        id: photo_id,
        access_hash: photo_id * 3,
        file_reference: vec![7, 7],
        date: 1_750_000_000,
        sizes: Vec::new(),
        video_sizes: Some(vec![
            tl_enums::VideoSize::VideoSize(tl_types::VideoSize {
                r#type: "v".to_owned(),
                w: 800,
                h: 800,
                size: 4096,
                video_start_ts: None,
            }),
            tl_enums::VideoSize::VideoSize(tl_types::VideoSize {
                r#type: "u".to_owned(),
                w: 160,
                h: 160,
                size: 16,
                video_start_ts: None,
            }),
        ]),
        dc_id: 2,
    });
    user_full(uid, Some(photo))
}

fn fake_decoder(bytes: &[u8]) -> Result<Vec<VideoUserpicFrame>, String> {
    if bytes.is_empty() {
        return Err("empty".to_owned());
    }
    let frame = |v: u8| VideoUserpicFrame::new(8, 8, vec![v; 8 * 8 * 4], 50);
    Ok([frame(10), frame(200)].into_iter().flatten().collect())
}

fn wait_for_frames(live: &LiveSession, peer: PeerId, photo_id: i64) -> VideoUserpicFetch {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let fetch = live.video_userpic_frames(peer, photo_id);
        if fetch != VideoUserpicFetch::Pending || Instant::now() > deadline {
            return fetch;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn wp40_wiring_dialog_rows_load_and_paint_video_userpics() {
    let (rpc, live) = make_live_session();
    let photo_id: i64 = 4_242;
    seed_user(&live, FRIEND_UID, true, Some(photo_id as u64));
    seed_user(&live, 502, false, Some(77));
    seed_user(&live, 503, true, None);
    set_video_userpic_decoder_live(&live, fake_decoder);

    // Only premium users with a known video avatar animate (`validateVideoUserpic`).
    let friend = PeerId::from_user(FRIEND_UID);
    assert_eq!(live.dialog_video_userpic(friend), Some(photo_id));
    assert_eq!(live.dialog_video_userpic(PeerId::from_user(502)), None);
    assert_eq!(live.dialog_video_userpic(PeerId::from_user(503)), None);
    assert_eq!(live.dialog_video_userpic(PeerId::from_user(100)), None);

    rpc.push_response(
        tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
        &user_full_with_video_photo(FRIEND_UID_TL, photo_id),
    );
    rpc.push_response(
        tl_fns::upload::GetFile::CONSTRUCTOR_ID,
        &tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileMp4,
            mtime: 1_750_000_030,
            bytes: vec![1; 16],
        }),
    );

    // Paint through the dialogs-row path: the first frame starts the load.
    let mut cache = DialogVideoUserpicCache::new();
    let ctx = egui::Context::default();
    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(42.0, 42.0));
    let mut painted = false;
    ctx.run_ui(egui::RawInput::default(), |ui| {
        painted = paint_row_video_userpic(ui, &mut cache, &live, friend, rect);
    })
    .textures_delta
    .clear();
    assert!(!painted);
    assert_eq!(
        cache.get(friend).map(|v| v.state()),
        Some(VideoUserpicState::Loading)
    );

    let fetch = wait_for_frames(&live, friend, photo_id);
    let VideoUserpicFetch::Ready(frames) = fetch else {
        panic!("video userpic did not load: {fetch:?}");
    };
    assert_eq!(frames.len(), 2);

    // Requests: users.getFullUser then upload.getFile on the small `u` video size.
    let recorded = rpc.recorded();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].0, tl_fns::users::GetFullUser::CONSTRUCTOR_ID);
    assert_eq!(recorded[1].0, tl_fns::upload::GetFile::CONSTRUCTOR_ID);
    let thumb = b"\x01u";
    assert!(
        recorded[1].1.windows(thumb.len()).any(|w| w == thumb),
        "upload.getFile must request thumb_size \"u\""
    );

    // Hand the frames to the row cache and paint a video frame over the static userpic.
    cache.entry_mut(friend).set_decoded_frames(frames);
    ctx.run_ui(egui::RawInput::default(), |ui| {
        painted = paint_row_video_userpic(ui, &mut cache, &live, friend, rect);
    })
    .textures_delta
    .clear();
    assert!(painted);

    // A peer that stops qualifying is dropped from the cache.
    ctx.run_ui(egui::RawInput::default(), |ui| {
        let _ = paint_row_video_userpic(ui, &mut cache, &live, PeerId::from_user(502), rect);
    })
    .textures_delta
    .clear();
    assert!(cache.get(PeerId::from_user(502)).is_none());
}

#[test]
fn wp40_wiring_video_userpic_failure_keeps_static_userpic() {
    let (rpc, live) = make_live_session();
    seed_user(&live, FRIEND_UID, true, Some(99));
    set_video_userpic_decoder_live(&live, fake_decoder);
    // users.getFullUser returns a different photo: the load fails, no download is attempted.
    rpc.push_response(
        tl_fns::users::GetFullUser::CONSTRUCTOR_ID,
        &user_full_with_video_photo(FRIEND_UID_TL, 12345),
    );
    let fetch = wait_for_frames(&live, PeerId::from_user(FRIEND_UID), 99);
    assert_eq!(fetch, VideoUserpicFetch::Failed);
    assert_eq!(rpc.recorded().len(), 1);
}

// =============================================================================
// 5. BotEarn affiliate button -> Stars Affiliate screen for that bot
// =============================================================================

#[test]
fn wp40_wiring_bot_earn_affiliate_opens_affiliate_screen_for_bot() {
    let bot = PeerSummary {
        id: PeerId::from_user(700),
        kind: PeerKind::User,
        title: "Sample Shop Bot".to_owned(),
        status: "bot".to_owned(),
        username: Some("sample_shop_bot".to_owned()),
        phone_or_link: None,
        about: String::new(),
        initials: "SS".to_owned(),
        avatar_rgb: [10, 20, 30],
        muted: false,
        can_send: true,
        is_saved_messages: false,
    };
    let action = tg_app::window::main_widget::bot_earn_affiliate_action(Some(&bot));
    let MainWidgetAction::OpenPayments { screen, slug } = action else {
        panic!("expected OpenPayments");
    };
    assert_eq!(screen, PaymentsScreen::Affiliate);
    assert_eq!(slug.as_deref(), Some("sample_shop_bot"));

    // The payments host opens the Affiliate screen scoped to that bot.
    let mut payments = PaymentsScreenHost::with_mock();
    payments
        .open_deep_link_request(PaymentsScreen::Affiliate, slug.as_deref())
        .expect("open affiliate");
    assert_eq!(payments.active_screen(), PaymentsScreen::Affiliate);
    assert_eq!(
        payments.starref_view().map(|v| v.peer_name.as_str()),
        Some("sample_shop_bot")
    );

    // Without a username the title is used; with no peer the user's own programs open.
    let mut no_username = bot.clone();
    no_username.username = None;
    let MainWidgetAction::OpenPayments { slug, .. } =
        tg_app::window::main_widget::bot_earn_affiliate_action(Some(&no_username))
    else {
        panic!("expected OpenPayments");
    };
    assert_eq!(slug.as_deref(), Some("Sample Shop Bot"));
    let MainWidgetAction::OpenPayments { slug, .. } =
        tg_app::window::main_widget::bot_earn_affiliate_action(None)
    else {
        panic!("expected OpenPayments");
    };
    assert_eq!(slug, None);
}

fn user_full(uid: i64, profile_photo: Option<tl_enums::Photo>) -> tl_enums::users::UserFull {
    tl_enums::users::UserFull::UserFull(tl_types::users::UserFull {
        full_user: tl_enums::UserFull::UserFull(tl_types::UserFull {
            blocked: false,
            phone_calls_available: false,
            phone_calls_private: false,
            can_pin_message: false,
            has_scheduled: false,
            video_calls_available: false,
            voice_messages_forbidden: false,
            translations_disabled: false,
            stories_pinned_available: false,
            blocked_my_stories_from: false,
            wallpaper_overridden: false,
            contact_require_premium: false,
            read_dates_private: false,
            sponsored_enabled: false,
            can_view_revenue: false,
            bot_can_manage_emoji_status: false,
            display_gifts_button: false,
            noforwards_my_enabled: false,
            noforwards_peer_enabled: false,
            unofficial_security_risk: false,
            id: uid,
            about: None,
            settings: tl_enums::PeerSettings::PeerSettings(tl_types::PeerSettings {
                report_spam: false,
                add_contact: false,
                block_contact: false,
                share_contact: false,
                need_contacts_exception: false,
                report_geo: false,
                autoarchived: false,
                invite_members: false,
                request_chat_broadcast: false,
                business_bot_paused: false,
                business_bot_can_reply: false,
                geo_distance: None,
                request_chat_title: None,
                request_chat_date: None,
                business_bot_id: None,
                business_bot_manage_url: None,
                charge_paid_message_stars: None,
                registration_month: None,
                phone_country: None,
                name_change_date: None,
                photo_change_date: None,
            }),
            personal_photo: None,
            profile_photo,
            fallback_photo: None,
            notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
                tl_types::PeerNotifySettings {
                    show_previews: None,
                    silent: None,
                    mute_until: None,
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
            bot_info: None,
            pinned_msg_id: None,
            common_chats_count: 0,
            folder_id: None,
            ttl_period: None,
            theme: None,
            private_forward_name: None,
            bot_group_admin_rights: None,
            bot_broadcast_admin_rights: None,
            wallpaper: None,
            stories: None,
            business_work_hours: None,
            business_location: None,
            business_greeting_message: None,
            business_away_message: None,
            business_intro: None,
            birthday: None,
            personal_channel_id: None,
            personal_channel_message: None,
            stargifts_count: None,
            starref_program: None,
            bot_verification: None,
            send_paid_messages_stars: None,
            disallowed_gifts: None,
            stars_rating: None,
            stars_my_pending_rating: None,
            stars_my_pending_rating_date: None,
            main_tab: None,
            saved_music: None,
            note: None,
            bot_manager_id: None,
            wallet_card: None,
            share_theme: None,
        }),
        chats: Vec::new(),
        users: Vec::new(),
    })
}
