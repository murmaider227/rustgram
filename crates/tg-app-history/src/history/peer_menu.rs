//! The chat top bar's "three dots" menu (`TopBarWidget::showPeerMenu` ->
//! `Window::FillDialogsEntryMenu` with `PeerMenuSource::History`), a port of
//! `Filler::fillHistoryActions` in `window/window_peer_menu.cpp`.
//!
//! Row order, labels, icons and conditions follow the `Filler::add*` functions it calls; the
//! menu is drawn with `st::popupMenuExpandedSeparator` (`TopBarWidget::createMenu`), so its
//! separators are the full-width band.

use tg_app_core::history::backend::{HistoryBackend, HistoryPeerInfo};
use tg_app_core::history::peer_menu::{PeerMenuCaps, PeerMenuKind};
use tg_app_core::stand_in::types::PeerId;
use tg_app_info::dialogs::entry_menu::EntryMenuRow;
use tg_app_info::dialogs::inner::{
    MUTE_FOR_ACTION, MUTE_FOREVER_ACTION, MUTE_SELECT_TONE_ACTION, MUTE_SOUND_TOGGLE_ACTION,
    mute_submenu_rows,
};
use tg_app_info::dialogs::mute_label::{unix_now, unmute_menu_label};
use tg_lang::tr;
use tg_ui::tg_style::st;

/// Window state the menu depends on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerMenuContext {
    /// The third column shows this chat's info (`isThreeColumn()` with
    /// `thirdSectionInfoEnabled()`): `Filler::addInfo` adds nothing then.
    pub info_in_third_column: bool,
    /// `Adaptive::isOneColumn()` (`Filler::addViewAsTopics`).
    pub one_column: bool,
    /// Current Unix time, for the "Unmute\t2h" hint.
    pub now: i64,
}

/// What a row of the menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerMenuCommand {
    /// "Unmute" (`notifySettings->update(thread, { .unmute = true })`).
    Unmute,
    /// Mute submenu: "Mute forever".
    MuteForever,
    /// Mute submenu: "Mute for..." (`PickMuteBox`).
    MuteFor,
    /// Mute submenu: "Select tone" (`RingtonesBox`).
    SelectTone,
    /// Mute submenu: "Disable sound" / "Enable sound".
    ToggleSound,
    /// "Create topic" (`NewForumTopicBox`).
    CreateTopic,
    /// "View profile" / "View group info" / "View channel info" (`showPeerInfo`).
    ViewInfo,
    /// "View as topics" (`showForum`).
    ViewAsTopics,
    /// "Manage group" / "Manage channel" / "Manage bot" (`showEditPeerBox`).
    Manage,
    /// "Post story" (`Media::Stories::PostStory`).
    PostStory,
    /// "Story drafts" (`ShowStoryDraftsBox`).
    StoryDrafts,
    /// "Stories archive" (`Info::Stories::Make(..., ArchiveId())`).
    StoryArchive,
    /// "Boost group" / "Boost channel" (`resolveBoostState`).
    Boost,
    /// "Create poll" (`PeerMenuCreatePoll`).
    CreatePoll,
    /// "Create checklist" (`PeerMenuCreateTodoList`).
    CreateTodoList,
    /// "Change wallpaper" (`toggleChooseChatTheme`).
    ChangeWallpaper,
    /// "Disable sharing" / "Enable sharing" (`messages.toggleNoForwards`).
    ToggleNoForwards,
    /// "View discussion" (`showPeerHistory(discussionLink)`).
    ViewDiscussion(PeerId),
    /// "Direct messages" (`showPeerHistory(broadcastMonoforum)`).
    DirectMessages,
    /// "Export chat history" (`PeerMenuExportChat`).
    Export,
    /// "Translate" (`saveTranslationDisabled(false)`).
    Translate,
    /// "Report" (`ShowReportMessageBox`).
    Report,
    /// "Clear history" (`ClearHistoryHandler`).
    ClearHistory,
    /// "Delete chat" / "Delete and leave" (`DeleteAndLeaveHandler`).
    DeleteChat,
    /// "Leave group" / "Leave channel" (`DeleteAndLeaveHandler`).
    LeaveChat,
}

const UNMUTE: &str = "peer_menu:unmute";
const CREATE_TOPIC: &str = "peer_menu:create_topic";
const VIEW_INFO: &str = "peer_menu:view_info";
const VIEW_AS_TOPICS: &str = "peer_menu:view_as_topics";
const MANAGE: &str = "peer_menu:manage";
const POST_STORY: &str = "peer_menu:post_story";
const STORY_DRAFTS: &str = "peer_menu:story_drafts";
const STORY_ARCHIVE: &str = "peer_menu:story_archive";
const BOOST: &str = "peer_menu:boost";
const CREATE_POLL: &str = "peer_menu:create_poll";
const CREATE_TODO: &str = "peer_menu:create_todo";
const WALLPAPER: &str = "peer_menu:wallpaper";
const NO_FORWARDS: &str = "peer_menu:no_forwards";
const VIEW_DISCUSSION: &str = "peer_menu:view_discussion";
const DIRECT_MESSAGES: &str = "peer_menu:direct_messages";
const EXPORT: &str = "peer_menu:export";
const TRANSLATE: &str = "peer_menu:translate";
const REPORT: &str = "peer_menu:report";
const CLEAR_HISTORY: &str = "peer_menu:clear_history";
const DELETE_CHAT: &str = "peer_menu:delete_chat";
const LEAVE_CHAT: &str = "peer_menu:leave_chat";

/// Maps a triggered row id of [`history_peer_menu_rows`] back to its command.
#[must_use]
pub fn peer_menu_command(id: &str, caps: &PeerMenuCaps) -> Option<PeerMenuCommand> {
    Some(match id {
        UNMUTE => PeerMenuCommand::Unmute,
        MUTE_FOREVER_ACTION => PeerMenuCommand::MuteForever,
        MUTE_FOR_ACTION => PeerMenuCommand::MuteFor,
        MUTE_SELECT_TONE_ACTION => PeerMenuCommand::SelectTone,
        MUTE_SOUND_TOGGLE_ACTION => PeerMenuCommand::ToggleSound,
        CREATE_TOPIC => PeerMenuCommand::CreateTopic,
        VIEW_INFO => PeerMenuCommand::ViewInfo,
        VIEW_AS_TOPICS => PeerMenuCommand::ViewAsTopics,
        MANAGE => PeerMenuCommand::Manage,
        POST_STORY => PeerMenuCommand::PostStory,
        STORY_DRAFTS => PeerMenuCommand::StoryDrafts,
        STORY_ARCHIVE => PeerMenuCommand::StoryArchive,
        BOOST => PeerMenuCommand::Boost,
        CREATE_POLL => PeerMenuCommand::CreatePoll,
        CREATE_TODO => PeerMenuCommand::CreateTodoList,
        WALLPAPER => PeerMenuCommand::ChangeWallpaper,
        NO_FORWARDS => PeerMenuCommand::ToggleNoForwards,
        VIEW_DISCUSSION => PeerMenuCommand::ViewDiscussion(caps.discussion?),
        DIRECT_MESSAGES => PeerMenuCommand::DirectMessages,
        EXPORT => PeerMenuCommand::Export,
        TRANSLATE => PeerMenuCommand::Translate,
        REPORT => PeerMenuCommand::Report,
        CLEAR_HISTORY => PeerMenuCommand::ClearHistory,
        DELETE_CHAT => PeerMenuCommand::DeleteChat,
        LEAVE_CHAT => PeerMenuCommand::LeaveChat,
        _ => return None,
    })
}

/// `Filler::skipCreateActions`: no poll / checklist rows in Replies, in a channel the account
/// has not joined, in a bot chat before /start, or in a blocked user's chat.
fn skip_create_actions(caps: &PeerMenuCaps) -> bool {
    caps.replies_or_verify_codes
        || (caps.kind.is_channel() && !caps.am_in)
        || (caps.kind == PeerMenuKind::Bot && caps.bot_start_pending)
        || (caps.kind.is_user() && caps.blocked)
}

/// The rows of the chat top bar menu (`Filler::fillHistoryActions`).
#[must_use]
pub fn history_peer_menu_rows(caps: &PeerMenuCaps, ctx: PeerMenuContext) -> Vec<EntryMenuRow> {
    let mut rows = Vec::new();
    let kind = caps.kind;
    let action = |id: &str, label: String, icon| EntryMenuRow::action(id, label, Some(icon));

    // `addToggleMuteSubmenu(true)`: never for Saved Messages or a forum; a separator after.
    if kind != PeerMenuKind::SavedMessages && !caps.forum {
        if caps.muted {
            rows.push(action(
                UNMUTE,
                unmute_menu_label(caps.mute_until, ctx.now),
                st::menu_icon_unmute,
            ));
        } else {
            rows.push(EntryMenuRow::submenu(
                tr::lng_context_mute(),
                Some(if caps.silent {
                    st::menu_icon_silent
                } else {
                    st::menu_icon_mute
                }),
                mute_submenu_rows(caps.silent),
            ));
        }
        rows.push(EntryMenuRow::ExpandedSeparator);
    }

    // `addCreateTopic`: `canCreateTopics() && !isBot()`, a separator after.
    if caps.can_create_topics && kind != PeerMenuKind::Bot {
        rows.push(action(
            CREATE_TOPIC,
            tr::lng_forum_create_topic(),
            st::menu_icon_discussion,
        ));
        rows.push(EntryMenuRow::ExpandedSeparator);
    }

    // `addInfo`: not for self, Replies or Verification Codes, nor while the third column
    // already shows this chat's info.
    if kind != PeerMenuKind::SavedMessages
        && !caps.replies_or_verify_codes
        && !ctx.info_in_third_column
    {
        let (label, icon) = match kind {
            PeerMenuKind::BasicGroup | PeerMenuKind::Megagroup => {
                (tr::lng_context_view_group(), st::menu_icon_info)
            }
            PeerMenuKind::User | PeerMenuKind::Bot | PeerMenuKind::SavedMessages => {
                (tr::lng_context_view_profile(), st::menu_icon_profile)
            }
            PeerMenuKind::Broadcast => (tr::lng_context_view_channel(), st::menu_icon_info),
        };
        rows.push(action(VIEW_INFO, label, icon));
    }

    // `addViewAsTopics`: a forum supergroup without tabs, in one-column mode.
    if caps.forum && kind.is_channel() && !caps.forum_tabs && ctx.one_column {
        rows.push(action(
            VIEW_AS_TOPICS,
            tr::lng_forum_view_as_topics(),
            st::menu_icon_as_topics,
        ));
    }

    // `addManageChat` (`EditPeerInfoBox::Available`).
    if caps.can_manage {
        let label = match kind {
            PeerMenuKind::User | PeerMenuKind::Bot | PeerMenuKind::SavedMessages => {
                tr::lng_manage_bot_title()
            }
            PeerMenuKind::BasicGroup | PeerMenuKind::Megagroup => tr::lng_manage_group_title(),
            PeerMenuKind::Broadcast => tr::lng_manage_channel_title(),
        };
        rows.push(action(MANAGE, label, st::menu_icon_manage));
    }

    // `addPostStory`, `addStoryDrafts` (self only), `addStoryArchive` (`canEditStories`).
    if caps.can_post_stories {
        rows.push(action(
            POST_STORY,
            tr::lng_story_post_button(),
            st::menu_icon_story_post,
        ));
    }
    if kind == PeerMenuKind::SavedMessages {
        rows.push(action(
            STORY_DRAFTS,
            tr::lng_story_drafts_button(),
            st::menu_icon_stories_saved_section,
        ));
    }
    if kind.is_channel() && caps.can_edit_stories {
        rows.push(action(
            STORY_ARCHIVE,
            tr::lng_stories_archive_button(),
            st::menu_icon_stories_archive_section,
        ));
    }

    // `addBoostChat`: every channel except a monoforum.
    if kind.is_channel() && !caps.monoforum {
        let label = if kind == PeerMenuKind::Megagroup {
            tr::lng_boost_group_button()
        } else {
            tr::lng_boost_channel_button()
        };
        rows.push(action(BOOST, label, st::menu_icon_boosts));
    }

    // `addCreatePoll`, `addCreateTodoList`.
    if !skip_create_actions(caps) {
        if caps.can_create_polls {
            rows.push(action(
                CREATE_POLL,
                tr::lng_polls_create(),
                st::menu_icon_create_poll,
            ));
        }
        if caps.can_create_todo_lists {
            rows.push(action(
                CREATE_TODO,
                tr::lng_todo_create(),
                st::menu_icon_create_todo_list,
            ));
        }
    }

    // `addThemeEdit`: users (bots and self included) that can be written to.
    if kind.is_user()
        && !caps.replies_or_verify_codes
        && !caps.inaccessible
        && !caps.premium_required_to_write
    {
        rows.push(action(
            WALLPAPER,
            tr::lng_chat_theme_wallpaper(),
            st::menu_icon_change_colors,
        ));
    }

    // `addToggleNoForwards`: 1:1 chats with a person.
    if kind == PeerMenuKind::User && !caps.inaccessible && !caps.service_user {
        let (label, icon) = if caps.sharing_disabled {
            (tr::lng_enable_sharing(), st::menu_icon_share_on)
        } else {
            (tr::lng_disable_sharing(), st::menu_icon_share_off)
        };
        rows.push(action(NO_FORWARDS, label, icon));
    }

    // `addViewDiscussion`, `addDirectMessages`: broadcast channels.
    if kind == PeerMenuKind::Broadcast {
        if caps.discussion.is_some() {
            rows.push(action(
                VIEW_DISCUSSION,
                tr::lng_profile_view_discussion(),
                st::menu_icon_discussion,
            ));
        }
        if caps.direct_messages {
            rows.push(action(
                DIRECT_MESSAGES,
                tr::lng_profile_direct_messages(),
                st::menu_icon_chat_discuss,
            ));
        }
    }

    // `addExportChat`.
    if caps.can_export {
        rows.push(action(
            EXPORT,
            tr::lng_profile_export_chat(),
            st::menu_icon_export,
        ));
    }

    // `addTranslate`.
    if caps.translate_offered {
        rows.push(action(
            TRANSLATE,
            tr::lng_context_translate(),
            st::menu_icon_translate,
        ));
    }

    // `addReport`: groups and channels the account did not create.
    if (kind.is_group() || kind.is_channel()) && !caps.am_creator {
        rows.push(action(
            REPORT,
            tr::lng_profile_report(),
            st::menu_icon_report,
        ));
    }

    // `addClearHistory`: not a monoforum; a channel only when joined and either the account
    // may delete messages or it is a private, topic-less group.
    let clear = !caps.monoforum
        && (!kind.is_channel()
            || (caps.am_in
                && (caps.can_delete_messages || (kind.is_group() && !caps.public && !caps.forum))));
    if clear {
        rows.push(action(
            CLEAR_HISTORY,
            tr::lng_profile_clear_history(),
            st::menu_icon_clear,
        ));
    }

    // `addDeleteChat`: users ("Delete chat") and basic groups ("Delete and leave").
    if !kind.is_channel() {
        let label = if kind.is_user() {
            tr::lng_profile_delete_conversation()
        } else {
            tr::lng_profile_clear_and_exit()
        };
        rows.push(EntryMenuRow::attention(
            DELETE_CHAT,
            label,
            Some(st::menu_icon_delete_attention),
        ));
    }

    // `addLeaveChat`: joined supergroups and channels.
    if kind.is_channel() && caps.am_in {
        let label = if kind == PeerMenuKind::Megagroup {
            tr::lng_profile_leave_group()
        } else {
            tr::lng_profile_leave_channel()
        };
        rows.push(EntryMenuRow::attention(
            LEAVE_CHAT,
            label,
            Some(st::menu_icon_leave_attention),
        ));
    }

    rows
}

/// Whether Rustgram has the box or section a row opens. Rows without one are left out rather
/// than shown dead: "Create topic" (`NewForumTopicBox`), "View as topics", "Post story",
/// "Story drafts", "Stories archive", "Change wallpaper" (`toggleChooseChatTheme`) and
/// "Translate".
#[must_use]
pub const fn peer_menu_command_available(command: PeerMenuCommand) -> bool {
    !matches!(
        command,
        PeerMenuCommand::CreateTopic
            | PeerMenuCommand::ViewAsTopics
            | PeerMenuCommand::PostStory
            | PeerMenuCommand::StoryDrafts
            | PeerMenuCommand::StoryArchive
            | PeerMenuCommand::ChangeWallpaper
            | PeerMenuCommand::Translate
    )
}

/// [`history_peer_menu_rows`] without the rows [`peer_menu_command_available`] rejects, and
/// without the separators that leaves at the start, at the end or doubled.
#[must_use]
pub fn available_peer_menu_rows(caps: &PeerMenuCaps, ctx: PeerMenuContext) -> Vec<EntryMenuRow> {
    let mut out: Vec<EntryMenuRow> = Vec::new();
    for row in history_peer_menu_rows(caps, ctx) {
        match &row {
            EntryMenuRow::Item(item) => {
                let available = !item.submenu.is_empty()
                    || peer_menu_command(&item.id, caps).is_some_and(peer_menu_command_available);
                if available {
                    out.push(row);
                }
            }
            EntryMenuRow::Separator | EntryMenuRow::ExpandedSeparator => {
                if out
                    .last()
                    .is_some_and(|last| matches!(last, EntryMenuRow::Item(_)))
                {
                    out.push(row);
                }
            }
        }
    }
    while out
        .last()
        .is_some_and(|last| !matches!(last, EntryMenuRow::Item(_)))
    {
        out.pop();
    }
    out
}

/// The chat top bar menu of `peer` as the history widget shows it: [`PeerMenuCaps`] from
/// `backend`, or guessed from `peer` when the backend reads no data layer.
#[must_use]
pub fn host_peer_menu_rows(
    backend: &dyn HistoryBackend,
    peer: &HistoryPeerInfo,
    info_in_third_column: bool,
) -> Vec<EntryMenuRow> {
    let caps = backend
        .peer_menu_caps(peer.summary.id)
        .unwrap_or_else(|| PeerMenuCaps::from_peer_info(peer));
    available_peer_menu_rows(
        &caps,
        PeerMenuContext {
            info_in_third_column,
            one_column: false,
            now: unix_now(),
        },
    )
}
