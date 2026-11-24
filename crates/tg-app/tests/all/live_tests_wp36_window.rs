//! WP-36 live-window reachability tests driven through the real `LiveActiveWorkspace` window
//! against the fake RPC client of `tests_window_wiring`:
//! - Folder multi-link manager (`chatlists.{get,edit,delete}ExportedInvite`)
//! - Folder color tags (`messages.toggleDialogFilterTags`) + live `DialogsBackend::folder_tags_enabled`
//! - Custom folder icon picker (`FilterIconPanel` -> `messages.updateDialogFilter` + sidebar icon)
//! - Forum topic icon picker (`messages.getStickerSet(EmojiDefaultTopicIcons)`) + `SubsectionTabsWidget`
//! - Peer member rank / tag editor (`MembersSection` -> `BoxKind::EditPeerTag` ->
//!   `messages.editChatParticipantRank`)

use std::time::Duration;

use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use tg_data::{Channel, ChannelFlags, ChannelId, PeerUpdateFlags};
use tg_tl::{Identifiable, enums as tl_enums, functions as tl_fns, types as tl_types};

use super::live_tests_window_wiring::{LiveTestEnv, build_live_harness};
use tg_app::dialogs::backend::{DialogsBackend, FolderIconKind};
use tg_app::history::sections_model::SubsectionTabsMode;
use tg_app::history::sections_ui::HistorySectionKind;
use tg_app::info::backend::InfoBackend;
use tg_app::settings::SettingsSection;
use tg_app::stand_in::types::{MsgId, PeerId};
use tg_app::window::section::{SectionShow, SettingsPage};

fn settle_harness(
    harness: &mut egui_kittest::Harness<'_, tg_app::live::host::LiveActiveWorkspace>,
) {
    for _ in 0..40 {
        harness.step();
        if !harness.state().controller().is_animating() {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    harness.step();
}

#[test]
fn wp36_live_window_folder_links_manager_tags_and_custom_icon() {
    let env = LiveTestEnv::boot(736_001, 736_002, 736_003);
    let alice = env.alice_peer;
    let fake = env.fake.clone();
    let runtime = env.runtime;
    let mut ws = env.workspace;
    ws.controller_mut()
        .session_mut()
        .expect("session")
        .show_peer_history(alice, SectionShow::clear_stack(), MsgId::SHOW_AT_END);

    fake.enqueue_ok(
        tl_fns::messages::ToggleDialogFilterTags::CONSTRUCTOR_ID,
        &true,
    );
    fake.enqueue_ok(
        tl_fns::chatlists::GetExportedInvites::CONSTRUCTOR_ID,
        &tl_enums::chatlists::ExportedInvites::ExportedInvites(
            tl_types::chatlists::ExportedInvites {
                invites: vec![tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(
                    tl_types::ExportedChatlistInvite {
                        title: "Team Link".to_owned(),
                        url: "https://t.me/addlist/team_slug".to_owned(),
                        peers: vec![tl_enums::Peer::User(tl_types::PeerUser {
                            user_id: 736_002,
                        })],
                    },
                )],
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::chatlists::EditExportedInvite::CONSTRUCTOR_ID,
        &tl_enums::ExportedChatlistInvite::ExportedChatlistInvite(
            tl_types::ExportedChatlistInvite {
                title: "Team Link".to_owned(),
                url: "https://t.me/addlist/team_slug".to_owned(),
                peers: vec![tl_enums::Peer::User(tl_types::PeerUser {
                    user_id: 736_002,
                })],
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::chatlists::DeleteExportedInvite::CONSTRUCTOR_ID,
        &true,
    );
    fake.enqueue_ok(tl_fns::messages::UpdateDialogFilter::CONSTRUCTOR_ID, &true);

    let settings_backend = ws.controller().session_backend().create_settings_backend();
    let embedded = ws.controller().theme_manager().embedded_type();
    let accent = ws.controller().theme_manager().accent();
    ws.controller_mut().main_widget_mut().open_settings_section(
        SettingsSection::Folders,
        settings_backend,
        embedded,
        accent,
    );
    ws.controller_mut()
        .session_mut()
        .expect("session")
        .show_settings(SettingsPage::Main);

    let mut harness = build_live_harness(ws);
    settle_harness(&mut harness);

    // 1. Folder tags toggle -> `messages.toggleDialogFilterTags(enabled = true)` + live dialogs sync.
    assert!(
        !DialogsBackend::folder_tags_enabled(harness.state().controller().session_backend()),
        "folder tags start disabled"
    );
    harness.get_by_label("Show Folder Tags").click();
    harness.step();
    harness.step();
    assert_eq!(
        fake.invoked_count(tl_fns::messages::ToggleDialogFilterTags::CONSTRUCTOR_ID),
        1,
        "the Show Folder Tags row must call messages.toggleDialogFilterTags"
    );
    assert!(
        DialogsBackend::folder_tags_enabled(harness.state().controller().session_backend()),
        "DialogsBackend::folder_tags_enabled must reflect toggled state live"
    );

    // 2. Folder row "Manage Invite Links" -> multi-link manager loads `chatlists.getExportedInvites`.
    harness.get_by_label("Manage Invite Links: Work").click();
    for _ in 0..5 {
        harness.step();
    }
    assert_eq!(
        fake.invoked_count(tl_fns::chatlists::GetExportedInvites::CONSTRUCTOR_ID),
        1
    );
    let loaded = harness
        .state()
        .controller()
        .main_widget()
        .settings()
        .and_then(|s| s.folders.links_box.as_ref())
        .map(|b| (b.filter_id, b.links.clone()));
    let (filter_id, links) = loaded.expect("folder links manager must be open");
    assert_eq!(filter_id, 2);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].slug, "team_slug");
    assert!(harness.query_by_label("Team Link").is_some());

    // 3. Edit -> Save -> `chatlists.editExportedInvite(slug = "team_slug")`.
    harness
        .get_all_by_label("Edit")
        .last()
        .expect("link Edit button")
        .click();
    for _ in 0..5 {
        harness.step();
    }
    harness.get_by_label("Save").click();
    for _ in 0..5 {
        harness.step();
    }
    assert_eq!(
        fake.invoked_count(tl_fns::chatlists::EditExportedInvite::CONSTRUCTOR_ID),
        1,
        "saving edited link must call chatlists.editExportedInvite"
    );

    // 4. Delete -> Confirm Delete -> `chatlists.deleteExportedInvite(slug = "team_slug")`.
    harness.get_by_label("Delete").click();
    for _ in 0..4 {
        harness.step();
    }
    harness.get_by_label("Confirm Delete").click();
    for _ in 0..4 {
        harness.step();
    }
    assert_eq!(
        fake.invoked_count(tl_fns::chatlists::DeleteExportedInvite::CONSTRUCTOR_ID),
        1
    );
    let remaining = harness
        .state()
        .controller()
        .main_widget()
        .settings()
        .and_then(|s| s.folders.links_box.as_ref())
        .map(|b| b.links.len());
    assert_eq!(remaining, Some(0));

    // Escape closes the manager.
    harness.key_press_modifiers(Modifiers::NONE, Key::Escape);
    for _ in 0..4 {
        harness.step();
    }
    assert!(
        harness
            .state()
            .controller()
            .main_widget()
            .settings()
            .is_some_and(|s| s.folders.links_box.is_none())
    );

    // 5. Custom folder icon picker (`FilterIconPanel`) -> `messages.updateDialogFilter` + sidebar icon.
    harness.get_by_label("Edit Folder: Work").click();
    for _ in 0..4 {
        harness.step();
    }
    harness.get_by_label("Choose Folder Icon (Folder)").click();
    for _ in 0..4 {
        harness.step();
    }
    harness.get_by_label("Cat").click();
    for _ in 0..4 {
        harness.step();
    }
    harness.get_by_label("Save").click();
    for _ in 0..4 {
        harness.step();
    }
    assert_eq!(
        fake.invoked_count(tl_fns::messages::UpdateDialogFilter::CONSTRUCTOR_ID),
        1,
        "saving folder after choosing custom icon must call messages.updateDialogFilter"
    );
    let sidebar_work_icon =
        DialogsBackend::chat_folders(harness.state().controller().session_backend())
            .into_iter()
            .find(|f| f.id.0 == 2)
            .map(|f| f.icon);
    assert_eq!(
        sidebar_work_icon,
        Some(FolderIconKind::Cat),
        "custom FilterIconPanel selection must propagate to the live folders sidebar"
    );

    drop(harness);
    LiveTestEnv::shutdown_runtime(runtime);
}

#[test]
fn wp36_live_window_forum_icons_subsection_tabs_and_peer_tags() {
    let env = LiveTestEnv::boot(736_101, 736_102, 736_103);
    let alice = env.alice_peer;
    let fake = env.fake.clone();
    let runtime = env.runtime;
    let forum_cid = 736_200_i64;
    let forum_peer = PeerId::from_channel(forum_cid as u64);

    runtime.tokio_handle().block_on(async {
        let mut s = runtime.session().write().await;
        let mut forum = Channel::new(ChannelId(forum_cid as u64));
        forum.common.name = "Dev Forum".to_owned();
        forum.access_hash = 555_200;
        forum.set_flags(ChannelFlags::MEGAGROUP | ChannelFlags::FORUM | ChannelFlags::CREATOR);
        forum.members_count = 5;
        s.upsert_channel(forum, PeerUpdateFlags::NAME);
    });

    // 1. Forum topics list + default topic icons (`messages.getForumTopics`, `messages.getStickerSet`).
    fake.enqueue_ok(
        tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID,
        &tl_enums::messages::ForumTopics::ForumTopics(tl_types::messages::ForumTopics {
            order_by_create_date: false,
            count: 2,
            topics: vec![
                tl_enums::ForumTopic::ForumTopic(tl_types::ForumTopic {
                    my: false,
                    closed: false,
                    pinned: true,
                    short: false,
                    hidden: false,
                    title_missing: false,
                    id: 1,
                    date: 1_795_341_600,
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: forum_cid,
                    }),
                    title: "General".to_owned(),
                    icon_color: 0x6FB9F0,
                    icon_emoji_id: None,
                    top_message: 1,
                    read_inbox_max_id: 1,
                    read_outbox_max_id: 1,
                    unread_count: 0,
                    unread_mentions_count: 0,
                    unread_reactions_count: 0,
                    unread_poll_votes_count: 0,
                    from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 736_101 }),
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
                    draft: None,
                }),
                tl_enums::ForumTopic::ForumTopic(tl_types::ForumTopic {
                    my: false,
                    closed: false,
                    pinned: false,
                    short: false,
                    hidden: false,
                    title_missing: false,
                    id: 42,
                    date: 1_795_341_700,
                    peer: tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: forum_cid,
                    }),
                    title: "Design".to_owned(),
                    icon_color: 0xCB86DB,
                    icon_emoji_id: None,
                    top_message: 42,
                    read_inbox_max_id: 42,
                    read_outbox_max_id: 42,
                    unread_count: 0,
                    unread_mentions_count: 0,
                    unread_reactions_count: 0,
                    unread_poll_votes_count: 0,
                    from_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 736_102 }),
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
                    draft: None,
                }),
            ],
            messages: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
            pts: 1,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID,
        &tl_enums::messages::StickerSet::StickerSet(tl_types::messages::StickerSet {
            set: tl_enums::StickerSet::StickerSet(tl_types::StickerSet {
                archived: false,
                official: true,
                masks: false,
                emojis: true,
                text_color: false,
                channel_emoji_status: false,
                creator: false,
                installed_date: None,
                id: 99_100,
                access_hash: 123,
                title: "Default Topic Icons".to_owned(),
                short_name: "topic_icons".to_owned(),
                thumbs: None,
                thumb_dc_id: None,
                thumb_version: None,
                thumb_document_id: None,
                count: 1,
                hash: 1,
            }),
            packs: Vec::new(),
            keywords: Vec::new(),
            documents: vec![tl_enums::Document::Document(tl_types::Document {
                id: 5309984423003823246,
                access_hash: 101,
                file_reference: vec![1, 2, 3],
                date: 1_795_341_600,
                mime_type: "application/x-tgsticker".to_owned(),
                size: 512,
                thumbs: None,
                video_thumbs: None,
                dc_id: 2,
                attributes: vec![tl_enums::DocumentAttribute::CustomEmoji(
                    tl_types::DocumentAttributeCustomEmoji {
                        free: true,
                        text_color: false,
                        alt: "💡".to_owned(),
                        stickerset: tl_enums::InputStickerSet::EmojiDefaultTopicIcons,
                    },
                )],
            })],
        }),
    );
    // Replies fetch when opening topic 42:
    fake.enqueue_ok(
        tl_fns::messages::GetReplies::CONSTRUCTOR_ID,
        &tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
            messages: Vec::new(),
            topics: Vec::new(),
            chats: Vec::new(),
            users: Vec::new(),
        }),
    );

    let mut ws = env.workspace;
    ws.controller_mut()
        .session_mut()
        .expect("session")
        .show_peer_history(forum_peer, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    ws.controller_mut()
        .main_widget_mut()
        .history_mut()
        .open_section(HistorySectionKind::ForumTopics {
            peer_id: forum_peer,
        });

    let mut harness = build_live_harness(ws);
    settle_harness(&mut harness);
    assert_eq!(
        fake.invoked_count(tl_fns::messages::GetForumTopics::CONSTRUCTOR_ID),
        1
    );

    // Open "+ Topic" -> fetches `messages.getStickerSet(EmojiDefaultTopicIcons)` and shows topic icon picker.
    harness.get_by_label("+ Topic").click();
    harness.step();
    harness.step();
    assert_eq!(
        fake.invoked_count(tl_fns::messages::GetStickerSet::CONSTRUCTOR_ID),
        1,
        "+ Topic must load default forum topic icons via messages.getStickerSet"
    );
    harness.get_by_label("Icons").click();
    harness.step();
    harness.step();
    assert!(
        harness.query_by_label("Topic icon 💡").is_some(),
        "forum topic icon picker must render fetched custom icon"
    );

    // Open "Design" topic -> SubsectionTabsWidget renders and switches Top -> Left on click.
    harness.get_by_label("Design").click();
    harness.step();
    harness.step();
    harness
        .get_by_label("Subsection Tabs Layout (Tabs on Top)")
        .click();
    harness.step();
    harness.step();
    assert!(
        harness
            .query_by_label("Subsection Tabs Layout (Tabs on Left)")
            .is_some(),
        "clicking Subsection Tabs layout toggle must switch Top -> Left"
    );

    // 2. Peer member rank / tag editor reachable via live `InfoWrapWidget` -> `BoxKind::EditPeerTag`.
    fake.enqueue_ok(
        tl_fns::channels::GetParticipants::CONSTRUCTOR_ID,
        &tl_enums::channels::ChannelParticipants::ChannelParticipants(
            tl_types::channels::ChannelParticipants {
                count: 1,
                participants: vec![tl_enums::ChannelParticipant::ChannelParticipant(
                    tl_types::ChannelParticipant {
                        user_id: 736_102,
                        date: 1_795_341_600,
                        subscription_until_date: None,
                        rank: Some("Helper".to_owned()),
                    },
                )],
                chats: Vec::new(),
                users: Vec::new(),
            },
        ),
    );
    fake.enqueue_ok(
        tl_fns::messages::EditChatParticipantRank::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_800,
            seq: 2,
        }),
    );

    // Fetch participants (populates `mg_info.member_ranks` -> `InfoBackend::member_rank`).
    let members = InfoBackend::peer_members(
        harness.state().controller().session_backend(),
        forum_peer,
        "",
    );
    assert_eq!(members.len(), 1);
    assert_eq!(
        InfoBackend::member_rank(
            harness.state().controller().session_backend(),
            forum_peer,
            alice,
        )
        .as_deref(),
        Some("Helper"),
        "server-returned participant rank must surface via InfoBackend::member_rank"
    );

    // Open Info pane on forum peer, right-click Alice to open `EditPeerTag`, and save.
    harness
        .state_mut()
        .controller_mut()
        .session_mut()
        .expect("session")
        .show_peer_info(forum_peer, SectionShow::forward());
    settle_harness(&mut harness);
    harness
        .get_by_label("Member: Alice Tester")
        .click_secondary();
    harness.step();
    harness.step();
    assert!(
        harness.query_by_label("Save").is_some(),
        "right-clicking member row must push EditPeerTagBoxState modal"
    );
    harness.get_by_label("Save").click();
    harness.step();
    harness.step();
    assert_eq!(
        fake.invoked_count(tl_fns::messages::EditChatParticipantRank::CONSTRUCTOR_ID),
        1,
        "saving EditPeerTag modal must invoke messages.editChatParticipantRank"
    );
    assert_eq!(SubsectionTabsMode::Top.next(), SubsectionTabsMode::Left);

    drop(harness);
    LiveTestEnv::shutdown_runtime(runtime);
}

#[test]
fn wp36_live_window_self_forwards_tagger() {
    let env = LiveTestEnv::boot(736_301, 736_302, 736_303);
    let self_peer = env.self_peer;
    let alice = env.alice_peer;
    let fake = env.fake.clone();
    let runtime = env.runtime;

    fake.enqueue_ok(
        tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID,
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
                    id: 501,
                    from_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: 736_301,
                    })),
                    from_boosts_applied: None,
                    from_rank: None,
                    peer_id: tl_enums::Peer::User(tl_types::PeerUser { user_id: 736_301 }),
                    saved_peer_id: Some(tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: 736_302,
                    })),
                    fwd_from: None,
                    via_bot_id: None,
                    via_business_bot_id: None,
                    guestchat_via_from: None,
                    reply_to: None,
                    date: 1_795_341_900,
                    message: "Hello in live chat".to_owned(),
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
                    effect: None,
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
            date: 1_795_341_900,
            seq: 1,
        }),
    );
    fake.enqueue_ok(
        tl_fns::messages::SendReaction::CONSTRUCTOR_ID,
        &tl_enums::Updates::Updates(tl_types::Updates {
            updates: Vec::new(),
            users: Vec::new(),
            chats: Vec::new(),
            date: 1_795_341_901,
            seq: 2,
        }),
    );

    let mut ws = env.workspace;
    ws.controller_mut()
        .session_mut()
        .expect("session")
        .show_peer_history(self_peer, SectionShow::clear_stack(), MsgId::SHOW_AT_END);
    ws.controller_mut()
        .main_widget_mut()
        .history_mut()
        .compose_mut()
        .start_forward(alice, vec![MsgId(1)], "Alice: Hello".to_owned());

    let mut harness = build_live_harness(ws);
    settle_harness(&mut harness);

    // Submit forward into Saved Messages via Send button or Enter.
    harness.key_press_modifiers(Modifiers::NONE, Key::Enter);
    harness.step();
    harness.step();
    assert_eq!(
        fake.invoked_count(tl_fns::messages::ForwardMessages::CONSTRUCTOR_ID),
        1,
        "submitting forward in Saved Messages must invoke messages.forwardMessages"
    );

    // Inline Self-Forwards Tagger overlay appears with quick reaction tag buttons.
    assert!(
        harness.query_by_label("🔥").is_some(),
        "inline SelfForwardsTagger must appear after forwarding to Saved Messages"
    );
    harness.get_by_label("🔥").click();
    harness.step();
    harness.step();
    assert_eq!(
        fake.invoked_count(tl_fns::messages::SendReaction::CONSTRUCTOR_ID),
        1,
        "clicking a tag in SelfForwardsTagger must invoke messages.sendReaction"
    );

    drop(harness);
    LiveTestEnv::shutdown_runtime(runtime);
}
