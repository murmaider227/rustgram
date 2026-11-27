//! TGB-3183: the chat top bar menu follows `Filler::fillHistoryActions`
//! (`window/window_peer_menu.cpp`) for every peer type, with icons and the expanded separator.

use egui::{Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use egui_kittest::kittest::Queryable;
use tg_app_core::history::peer_menu::{PeerMenuCaps, PeerMenuKind};
use tg_app_core::stand_in::types::{PeerId, PeerKind, PeerSummary};
use tg_app_info::dialogs::entry_menu::EntryMenuRow;
use tg_lang::tr;
use tg_ui::tg_style::st;

use crate::history::backend::HistoryPeerInfo;
use crate::history::bars::paint_top_bar;
use crate::history::peer_menu::{
    PeerMenuCommand, PeerMenuContext, available_peer_menu_rows, history_peer_menu_rows,
    peer_menu_command,
};
use crate::history::theme::HistoryColors;

const SEP: &str = "---";

fn labels(rows: &[EntryMenuRow]) -> Vec<String> {
    rows.iter()
        .map(|row| row.label().map_or_else(|| SEP.to_owned(), str::to_owned))
        .collect()
}

fn ctx() -> PeerMenuContext {
    PeerMenuContext {
        info_in_third_column: true,
        one_column: false,
        now: 1_000,
    }
}

fn caps(kind: PeerMenuKind) -> PeerMenuCaps {
    PeerMenuCaps {
        peer_id: PeerId::from_channel(77),
        kind,
        am_in: true,
        ..PeerMenuCaps::default()
    }
}

/// The owner's group: a muted supergroup the account is a member of, with the info shown in the
/// third column, Premium, and a private group (so "Clear history" is offered).
fn owner_group() -> PeerMenuCaps {
    PeerMenuCaps {
        muted: true,
        mute_until: Some(i32::MAX),
        can_manage: true,
        can_create_polls: true,
        can_create_todo_lists: true,
        can_export: true,
        ..caps(PeerMenuKind::Megagroup)
    }
}

#[test]
fn supergroup_member_menu_matches_telegram_desktop() {
    let rows = history_peer_menu_rows(&owner_group(), ctx());
    assert_eq!(
        labels(&rows),
        [
            tr::lng_context_unmute(),
            SEP.to_owned(),
            tr::lng_manage_group_title(),
            tr::lng_boost_group_button(),
            tr::lng_polls_create(),
            tr::lng_todo_create(),
            tr::lng_profile_export_chat(),
            tr::lng_profile_report(),
            tr::lng_profile_clear_history(),
            tr::lng_profile_leave_group(),
        ]
    );
    // `popupMenuExpandedSeparator`: the band after the mute row.
    assert!(matches!(rows[1], EntryMenuRow::ExpandedSeparator));
    let icon_of = |i: usize| match &rows[i] {
        EntryMenuRow::Item(item) => item.icon,
        _ => None,
    };
    assert_eq!(icon_of(0), Some(st::menu_icon_unmute));
    assert_eq!(icon_of(2), Some(st::menu_icon_manage));
    assert_eq!(icon_of(3), Some(st::menu_icon_boosts));
    assert_eq!(icon_of(4), Some(st::menu_icon_create_poll));
    assert_eq!(icon_of(5), Some(st::menu_icon_create_todo_list));
    assert_eq!(icon_of(6), Some(st::menu_icon_export));
    assert_eq!(icon_of(7), Some(st::menu_icon_report));
    assert_eq!(icon_of(8), Some(st::menu_icon_clear));
    assert_eq!(icon_of(9), Some(st::menu_icon_leave_attention));
    assert!(matches!(&rows[9], EntryMenuRow::Item(item) if item.attention));
    // Every row maps back to its command.
    let caps = owner_group();
    let commands: Vec<_> = rows
        .iter()
        .filter_map(EntryMenuRow::id)
        .filter_map(|id| peer_menu_command(id, &caps))
        .collect();
    assert_eq!(
        commands,
        [
            PeerMenuCommand::Unmute,
            PeerMenuCommand::Manage,
            PeerMenuCommand::Boost,
            PeerMenuCommand::CreatePoll,
            PeerMenuCommand::CreateTodoList,
            PeerMenuCommand::Export,
            PeerMenuCommand::Report,
            PeerMenuCommand::ClearHistory,
            PeerMenuCommand::LeaveChat,
        ]
    );
}

#[test]
fn info_row_appears_without_the_third_column() {
    let rows = history_peer_menu_rows(
        &owner_group(),
        PeerMenuContext {
            info_in_third_column: false,
            ..ctx()
        },
    );
    assert_eq!(labels(&rows)[2], tr::lng_context_view_group());
}

#[test]
fn unmuted_chat_has_the_mute_submenu() {
    let rows = history_peer_menu_rows(&caps(PeerMenuKind::Megagroup), ctx());
    let EntryMenuRow::Item(mute) = &rows[0] else {
        panic!("mute row first");
    };
    assert_eq!(mute.label, tr::lng_context_mute());
    assert_eq!(mute.icon, Some(st::menu_icon_mute));
    assert_eq!(mute.submenu.len(), 4, "FillMuteMenu rows");
}

#[test]
fn supergroup_owner_and_public_group_without_delete_rights() {
    let owner = PeerMenuCaps {
        am_creator: true,
        can_delete_messages: true,
        ..owner_group()
    };
    let l = labels(&history_peer_menu_rows(&owner, ctx()));
    assert!(
        !l.contains(&tr::lng_profile_report()),
        "creators cannot report"
    );
    assert!(l.contains(&tr::lng_profile_clear_history()));

    let public = PeerMenuCaps {
        public: true,
        ..owner_group()
    };
    let l = labels(&history_peer_menu_rows(&public, ctx()));
    assert!(!l.contains(&tr::lng_profile_clear_history()));
}

#[test]
fn basic_group_member_and_creator() {
    let member = PeerMenuCaps {
        can_create_polls: true,
        ..caps(PeerMenuKind::BasicGroup)
    };
    assert_eq!(
        labels(&history_peer_menu_rows(&member, ctx())),
        [
            tr::lng_context_mute(),
            SEP.to_owned(),
            tr::lng_polls_create(),
            tr::lng_profile_report(),
            tr::lng_profile_clear_history(),
            tr::lng_profile_clear_and_exit(),
        ]
    );
    let creator = PeerMenuCaps {
        am_creator: true,
        can_manage: true,
        ..member
    };
    let l = labels(&history_peer_menu_rows(&creator, ctx()));
    assert!(l.contains(&tr::lng_manage_group_title()));
    assert!(!l.contains(&tr::lng_profile_report()));
    assert!(!l.contains(&tr::lng_profile_leave_group()));
}

#[test]
fn channel_subscriber_admin_and_left() {
    let subscriber = PeerMenuCaps {
        can_export: true,
        ..caps(PeerMenuKind::Broadcast)
    };
    assert_eq!(
        labels(&history_peer_menu_rows(&subscriber, ctx())),
        [
            tr::lng_context_mute(),
            SEP.to_owned(),
            tr::lng_boost_channel_button(),
            tr::lng_profile_export_chat(),
            tr::lng_profile_report(),
            tr::lng_profile_leave_channel(),
        ]
    );
    let admin = PeerMenuCaps {
        can_manage: true,
        can_create_polls: true,
        can_delete_messages: true,
        discussion: Some(PeerId::from_channel(78)),
        direct_messages: true,
        ..subscriber.clone()
    };
    assert_eq!(
        labels(&history_peer_menu_rows(&admin, ctx())),
        [
            tr::lng_context_mute(),
            SEP.to_owned(),
            tr::lng_manage_channel_title(),
            tr::lng_boost_channel_button(),
            tr::lng_polls_create(),
            tr::lng_profile_view_discussion(),
            tr::lng_profile_direct_messages(),
            tr::lng_profile_export_chat(),
            tr::lng_profile_report(),
            tr::lng_profile_clear_history(),
            tr::lng_profile_leave_channel(),
        ]
    );
    let left = PeerMenuCaps {
        am_in: false,
        can_create_polls: true,
        ..subscriber
    };
    let l = labels(&history_peer_menu_rows(&left, ctx()));
    assert!(!l.contains(&tr::lng_profile_leave_channel()));
    assert!(!l.contains(&tr::lng_polls_create()), "skipCreateActions");
}

#[test]
fn user_bot_and_saved_messages() {
    let user = PeerMenuCaps {
        can_export: true,
        ..caps(PeerMenuKind::User)
    };
    assert_eq!(
        labels(&history_peer_menu_rows(&user, ctx())),
        [
            tr::lng_context_mute(),
            SEP.to_owned(),
            tr::lng_chat_theme_wallpaper(),
            tr::lng_disable_sharing(),
            tr::lng_profile_export_chat(),
            tr::lng_profile_clear_history(),
            tr::lng_profile_delete_conversation(),
        ]
    );
    // Rustgram has no chat theme box yet: the row is left out, not shown dead.
    assert_eq!(
        labels(&available_peer_menu_rows(&user, ctx()))[2],
        tr::lng_disable_sharing()
    );

    let bot = PeerMenuCaps {
        can_create_polls: true,
        ..caps(PeerMenuKind::Bot)
    };
    let l = labels(&history_peer_menu_rows(&bot, ctx()));
    assert!(l.contains(&tr::lng_polls_create()));
    assert!(!l.contains(&tr::lng_disable_sharing()));
    let pending = PeerMenuCaps {
        bot_start_pending: true,
        ..bot
    };
    let l = labels(&history_peer_menu_rows(&pending, ctx()));
    assert!(!l.contains(&tr::lng_polls_create()), "bot not started");

    let saved = PeerMenuCaps {
        can_create_polls: true,
        can_post_stories: true,
        ..caps(PeerMenuKind::SavedMessages)
    };
    assert_eq!(
        labels(&available_peer_menu_rows(&saved, ctx())),
        [
            tr::lng_polls_create(),
            tr::lng_profile_clear_history(),
            tr::lng_profile_delete_conversation(),
        ]
    );
}

#[test]
fn forum_has_no_mute_row_and_no_dangling_separator() {
    let forum = PeerMenuCaps {
        forum: true,
        can_create_topics: true,
        ..owner_group()
    };
    let all = labels(&history_peer_menu_rows(&forum, ctx()));
    assert_eq!(all[0], tr::lng_forum_create_topic());
    assert_eq!(all[1], SEP);
    let shown = labels(&available_peer_menu_rows(&forum, ctx()));
    assert_eq!(shown[0], tr::lng_manage_group_title());
    assert!(!shown.iter().any(|l| l == SEP));
}

fn group_peer_info() -> HistoryPeerInfo {
    HistoryPeerInfo::from_summary(PeerSummary {
        id: PeerId::from_channel(91),
        kind: PeerKind::Group,
        title: "Weekend Hikers".to_owned(),
        status: "1 204 members".to_owned(),
        username: None,
        phone_or_link: None,
        about: String::new(),
        initials: "WH".to_owned(),
        avatar_rgb: [0x6C, 0x9F, 0xE0],
        muted: true,
        can_send: true,
        is_saved_messages: false,
    })
}

/// Through the real top bar widget: before TGB-3183 a group's menu held one row, "Unmute".
#[test]
fn group_top_bar_menu_opens_with_the_filler_rows() {
    let peer = group_peer_info();
    let colors = HistoryColors::from_palette(&tg_app_core::stand_in::theme::Palette::day());
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(900.0, 600.0))
        .with_step_dt(0.05)
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .build_ui(move |ui| {
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 54.0));
            let _ = paint_top_bar(ui, rect, &peer, false, false, false, &colors);
        });
    harness.run_steps(3);
    harness.get_by_label(&tr::lng_chat_menu()).click();
    harness.run_steps(5);
    for label in [
        tr::lng_context_view_group(),
        tr::lng_boost_group_button(),
        tr::lng_polls_create(),
        tr::lng_profile_report(),
        tr::lng_profile_clear_history(),
        tr::lng_profile_leave_group(),
    ] {
        assert!(
            harness.query_by_label_contains(&label).is_some(),
            "menu row {label:?} missing"
        );
    }
}
