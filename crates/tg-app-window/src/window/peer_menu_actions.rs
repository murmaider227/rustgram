//! The window side of the chat top bar menu (`Filler::fillHistoryActions` in
//! `window/window_peer_menu.cpp`): the rows that open a box or another section. The history
//! widget runs the rest itself (mute toggles, poll, checklist, sharing, direct messages).

use tg_app_core::session_api::AppSession;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_history::history::peer_menu::PeerMenuCommand;
use tg_app_info::info::InfoWrapEvent;

use crate::window::main_widget::MainWidgetAction;
use crate::window::section::SectionShow;
use crate::window::session_controller::SessionController;

/// Runs `command` of `peer_id`'s chat menu: boxes go out as [`MainWidgetAction::InfoEvent`]
/// (the controller opens the same boxes as the profile), navigation happens on `session`.
pub fn run_peer_menu_command(
    peer_id: PeerId,
    command: PeerMenuCommand,
    session: &mut SessionController,
    session_backend: &mut dyn AppSession,
    actions: &mut Vec<MainWidgetAction>,
) {
    let event = match command {
        // `Filler::addInfo` -> `SessionController::showPeerInfo`.
        PeerMenuCommand::ViewInfo => {
            session.show_peer_info_layer(peer_id);
            None
        }
        // `Filler::addManageChat` -> `showEditPeerBox` (`EditPeerInfoBox`; a bot's own box).
        PeerMenuCommand::Manage => Some(
            if tg_app_core::info::backend::InfoBackend::peer_profile(session_backend, peer_id)
                .is_some_and(|p| p.kind == tg_app_info::info::InfoPeerKind::Bot)
            {
                InfoWrapEvent::OpenEditBotInfo(peer_id)
            } else {
                InfoWrapEvent::OpenEditPeer(peer_id)
            },
        ),
        // `Filler::addBoostChat` -> `resolveBoostState`: Rustgram's boost screen, as the
        // `t.me/boost` link opens it.
        PeerMenuCommand::Boost => Some(InfoWrapEvent::OpenBoosts(peer_id)),
        PeerMenuCommand::Export => Some(InfoWrapEvent::ExportChatHistory(peer_id)),
        PeerMenuCommand::Report => Some(InfoWrapEvent::OpenReport(peer_id)),
        PeerMenuCommand::ClearHistory => Some(InfoWrapEvent::OpenClearHistory(peer_id)),
        // `DeleteAndLeaveHandler` -> `DeleteChatBox` ("Delete chat", "Delete and leave",
        // "Leave group", "Leave channel").
        PeerMenuCommand::DeleteChat | PeerMenuCommand::LeaveChat => {
            Some(InfoWrapEvent::OpenDelete {
                peer_id,
                message_ids: Vec::new(),
            })
        }
        PeerMenuCommand::MuteFor => Some(InfoWrapEvent::OpenMuteFor(peer_id)),
        PeerMenuCommand::SelectTone => Some(InfoWrapEvent::OpenSelectTone(peer_id)),
        // `FillMuteMenu`: "Disable sound" / "Enable sound" applies at once.
        PeerMenuCommand::ToggleSound => {
            use tg_app_core::dialogs::backend::DialogsBackend;
            let silent = DialogsBackend::dialog_notify_sound(session_backend, peer_id).none;
            DialogsBackend::set_dialog_sound_enabled(session_backend, peer_id, silent)
                .err()
                .map(InfoWrapEvent::Toast)
        }
        // `Filler::addViewDiscussion` -> `showPeerHistory(discussionLink)`.
        PeerMenuCommand::ViewDiscussion(chat) => {
            session.show_peer_history(chat, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
            None
        }
        PeerMenuCommand::Unmute
        | PeerMenuCommand::MuteForever
        | PeerMenuCommand::CreateTopic
        | PeerMenuCommand::ViewAsTopics
        | PeerMenuCommand::PostStory
        | PeerMenuCommand::StoryDrafts
        | PeerMenuCommand::StoryArchive
        | PeerMenuCommand::CreatePoll
        | PeerMenuCommand::CreateTodoList
        | PeerMenuCommand::ChangeWallpaper
        | PeerMenuCommand::ToggleNoForwards
        | PeerMenuCommand::DirectMessages
        | PeerMenuCommand::Translate => None,
    };
    if let Some(event) = event {
        actions.push(MainWidgetAction::InfoEvent(event));
    }
}
