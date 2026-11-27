//! Demo [`PeerMenuCaps`]: the chat menu's rights read from the demo profile
//! ([`InfoBackend::peer_profile`]) the way the live session reads them from `tg-data`.

use tg_app_core::history::peer_menu::{PeerMenuCaps, PeerMenuKind};
use tg_app_core::stand_in::types::PeerId;
use tg_app_history::history::backend::HistoryBackend;
use tg_app_info::dialogs::backend::DialogsBackend;
use tg_app_info::info::backend::{InfoBackend, InfoPeerKind};
use tg_data::rights::ChatBannedRights;

/// [`PeerMenuCaps`] of a demo chat.
pub fn demo_peer_menu_caps<B>(backend: &B, peer_id: PeerId) -> Option<PeerMenuCaps>
where
    B: HistoryBackend + InfoBackend + DialogsBackend + ?Sized,
{
    let profile = InfoBackend::peer_profile(backend, peer_id)?;
    let info = HistoryBackend::peer_info(backend, peer_id)?;
    let (self_id, _) = HistoryBackend::self_peer(backend);
    let premium = InfoBackend::peer_profile(backend, self_id).is_some_and(|p| p.premium);
    let kind = match profile.kind {
        InfoPeerKind::User => PeerMenuKind::User,
        InfoPeerKind::Bot => PeerMenuKind::Bot,
        InfoPeerKind::SavedMessages => PeerMenuKind::SavedMessages,
        InfoPeerKind::Group => PeerMenuKind::BasicGroup,
        InfoPeerKind::Supergroup => PeerMenuKind::Megagroup,
        InfoPeerKind::Channel => PeerMenuKind::Broadcast,
    };
    let admin = profile.can_edit || profile.can_manage_admins || profile.can_manage_permissions;
    let group_polls = !profile
        .default_banned_rights
        .contains(ChatBannedRights::SEND_POLLS);
    let can_create_polls = match kind {
        PeerMenuKind::SavedMessages | PeerMenuKind::Bot => true,
        PeerMenuKind::User => false,
        PeerMenuKind::BasicGroup | PeerMenuKind::Megagroup => admin || group_polls,
        PeerMenuKind::Broadcast => admin,
    };
    let mute_until = DialogsBackend::dialog_mute_until(backend, peer_id);
    Some(PeerMenuCaps {
        peer_id,
        kind,
        blocked: profile.is_blocked,
        forum: profile.is_forum,
        can_create_topics: profile.is_forum,
        am_in: true,
        am_creator: profile.can_manage_admins,
        can_manage: admin
            || (kind == PeerMenuKind::Megagroup
                && !profile
                    .default_banned_rights
                    .contains(ChatBannedRights::CHANGE_INFO)),
        can_delete_messages: info.can_delete,
        public: profile.is_public,
        can_create_polls,
        can_create_todo_lists: premium && kind != PeerMenuKind::Broadcast && can_create_polls,
        can_export: !profile.no_forwards,
        can_post_stories: kind == PeerMenuKind::SavedMessages
            || (kind == PeerMenuKind::Broadcast && admin),
        can_edit_stories: kind == PeerMenuKind::Broadcast && admin,
        sharing_disabled: profile.no_forwards,
        discussion: (kind == PeerMenuKind::Broadcast)
            .then(|| profile.linked_discussion_peer.as_ref().map(|(id, _)| *id))
            .flatten(),
        direct_messages: kind == PeerMenuKind::Broadcast && admin,
        muted: mute_until.is_some() || profile.muted,
        mute_until,
        silent: DialogsBackend::dialog_notify_sound(backend, peer_id).none,
        ..PeerMenuCaps::default()
    })
}
