//! Dialogs community grouping (`dialogs_community_chats_list`, `dialogs_community_rows_view`,
//! `dialogs_community_requestable_list`) and collapsed community row aggregation.
//!
//! Port origin:
//! - `Telegram/SourceFiles/dialogs/dialogs_community_chats_list.h` / `.cpp`
//! - `Telegram/SourceFiles/dialogs/dialogs_community_rows_view.h` / `.cpp`
//! - `Telegram/SourceFiles/dialogs/dialogs_community_requestable_list.h` / `.cpp`
//! - `Telegram/SourceFiles/data/data_community.cpp`

use std::collections::HashSet;

use egui::{Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_history::{DialogBadgesState, DialogSendState, LocalDateTime};
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::stand_in::Colors;
use tg_ui::userpic::EmptyUserpic;

use crate::boxes::community::CommunityLinkedPeer;
use crate::dialogs::backend::{DialogItem, DialogPeerKind, SpecialRowKind};
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Category of chats rendered by [`CommunityChatsListWidget`] (`Dialogs::CommunityChatsKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommunityChatsKind {
    /// Member chats the current user has already joined, ordered by recent activity.
    Joined,
    /// Viewable linked groups/channels the user has not joined yet.
    Discoverable,
    /// Bots linked to the community.
    Bots,
}

pub use tg_app_core::core_types::misc::CommunityDialogGroup;

/// Output row produced by [`group_dialogs_with_communities`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupedDialogRow {
    /// Ordinary individual dialog row.
    Dialog(DialogItem),
    /// Collapsed community hub row aggregating its member dialogs into one entry.
    CollapsedCommunity {
        /// Community peer ID.
        community_id: PeerId,
        /// Synthesized [`DialogItem`] carrying aggregated unread counts, newest timestamp,
        /// and preview of member chat titles (matching `Data::CommunityInfo::validateListEntryCache`).
        row: DialogItem,
        /// Member dialogs currently folded inside this community row.
        member_peers: Vec<PeerId>,
    },
}

impl GroupedDialogRow {
    /// Returns the primary [`DialogItem`] to paint for this row.
    #[must_use]
    pub const fn as_dialog_item(&self) -> &DialogItem {
        match self {
            Self::Dialog(item) => item,
            Self::CollapsedCommunity { row, .. } => row,
        }
    }

    /// Returns `Some(community_id)` if this row is a collapsed community group.
    #[must_use]
    pub const fn collapsed_community_id(&self) -> Option<PeerId> {
        match self {
            Self::Dialog(_) => None,
            Self::CollapsedCommunity { community_id, .. } => Some(*community_id),
        }
    }
}

/// Groups a flat dialogs slice with collapsed communities, matching `Data::CommunityInfo`
/// in `data_community.cpp`:
///
/// - For each community with `collapsed_in_dialogs == true` that has at least one member dialog
///   in `entries`, individual member rows are hidden and replaced by a single
///   [`GroupedDialogRow::CollapsedCommunity`] positioned at the newest member's slot,
///   with aggregated unread/mention/reaction counters and a comma-separated member-chat preview.
/// - Communities with `collapsed_in_dialogs == false` leave their member dialogs ungrouped.
#[must_use]
pub fn group_dialogs_with_communities(
    entries: &[DialogItem],
    communities: &[CommunityDialogGroup],
) -> Vec<GroupedDialogRow> {
    let collapsed_groups: Vec<&CommunityDialogGroup> = communities
        .iter()
        .filter(|c| c.collapsed_in_dialogs && !c.member_peer_ids.is_empty())
        .collect();
    if collapsed_groups.is_empty() {
        return entries
            .iter()
            .cloned()
            .map(GroupedDialogRow::Dialog)
            .collect();
    }

    let mut hidden_members: HashSet<PeerId> = HashSet::new();
    let mut emitted_communities: HashSet<PeerId> = HashSet::new();
    for g in &collapsed_groups {
        for &pid in &g.member_peer_ids {
            hidden_members.insert(pid);
        }
    }

    let mut output = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Some(group) = collapsed_groups
            .iter()
            .find(|g| g.member_peer_ids.contains(&entry.peer_id))
        {
            if emitted_communities.insert(group.community_id) {
                let members: Vec<&DialogItem> = entries
                    .iter()
                    .filter(|e| group.member_peer_ids.contains(&e.peer_id))
                    .collect();
                let aggregated = synthesize_community_dialog_item(group, &members);
                let member_peers = members.iter().map(|m| m.peer_id).collect();
                output.push(GroupedDialogRow::CollapsedCommunity {
                    community_id: group.community_id,
                    row: aggregated,
                    member_peers,
                });
            }
            continue;
        }
        if !hidden_members.contains(&entry.peer_id) {
            output.push(GroupedDialogRow::Dialog(entry.clone()));
        }
    }
    output
}

/// Synthesizes a single aggregated [`DialogItem`] for a collapsed community row,
/// matching `Data::CommunityInfo::validateListEntryCache` and `oneUnreadStateChanged`.
#[must_use]
pub fn synthesize_community_dialog_item(
    group: &CommunityDialogGroup,
    members: &[&DialogItem],
) -> DialogItem {
    let mut unread_count = 0i32;
    let mut unread_muted = true;
    let mut unread_mentions = false;
    let mut unread_reactions = false;
    let mut newest_time = LocalDateTime::new(2026, 1, 1, 0, 0, 0);
    let mut has_time = false;
    let mut titles = Vec::new();

    for m in members {
        if m.badges.unread_counter > 0 {
            unread_count = unread_count.saturating_add(m.badges.unread_counter);
            if !m.muted {
                unread_muted = false;
            }
        }
        if m.badges.mention {
            unread_mentions = true;
        }
        if m.badges.reaction {
            unread_reactions = true;
        }
        let t = m.display_time();
        if !has_time || t.to_local_epoch_secs() > newest_time.to_local_epoch_secs() {
            newest_time = t;
            has_time = true;
        }
        if titles.len() < 4 {
            titles.push(m.title.as_str());
        }
    }

    let preview = if titles.is_empty() {
        format!("{} chats", group.member_peer_ids.len())
    } else {
        titles.join(", ")
    };

    DialogItem {
        peer_id: group.community_id,
        kind: DialogPeerKind::Forum,
        special: SpecialRowKind::None,
        title: group.title.clone(),
        username: None,
        color_index: group.color_index,
        verified: false,
        premium: false,
        emoji_status_id: None,
        online: false,
        active_call: false,
        muted: unread_count > 0 && unread_muted,
        pinned: group.pinned,
        pinned_order: None,
        archived: false,
        folders: Vec::new(),
        folder_tags: Vec::new(),
        last_message_id: MsgId(0),
        last_message: preview,
        last_sender: Some(format!("{} chats", members.len())),
        last_message_time: newest_time,
        send_state: DialogSendState::None,
        draft: None,
        typing: None,
        badges: DialogBadgesState {
            unread_counter: unread_count,
            unread: unread_count > 0,
            unread_muted: unread_count > 0 && unread_muted,
            mention: unread_mentions,
            mention_muted: false,
            reaction: unread_reactions,
            reaction_muted: false,
            poll: false,
            poll_muted: false,
        },
        is_contact: false,
        media_preview: None,
        has_bot_app_button: false,
    }
}

/// Actions emitted by [`CommunityChatsListWidget::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommunityChatsListAction {
    /// User clicked a chat or bot row.
    OpenPeer(PeerId),
    /// User clicked a hidden peer that is not accessible.
    HiddenInaccessible,
    /// Admin clicked "Remove" on a linked peer row.
    RemovePeer(PeerId),
}

/// Renders a list of community member/discoverable/bot rows
/// (`Dialogs::CommunityChatsList` + `Dialogs::CommunityRowsView`).
pub struct CommunityChatsListWidget;

impl CommunityChatsListWidget {
    /// Renders `peers` with avatars, member/subscriber subtitle or last message preview,
    /// unread badges, and optional admin Remove action.
    pub fn show(
        ui: &mut Ui,
        salt: &str,
        kind: CommunityChatsKind,
        peers: &[&CommunityLinkedPeer],
        can_manage: bool,
        colors: &dyn Colors,
    ) -> Option<CommunityChatsListAction> {
        let mut action = None;
        for peer in peers {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let row_w =
                    (ui.available_width() - if can_manage { 72.0 } else { 12.0 }).max(120.0);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(row_w, 50.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(6),
                        colors.color("dialogsBgOver"),
                    );
                }

                let av_rect = Rect::from_min_size(
                    Pos2::new(rect.left() + 6.0, rect.center().y - 19.0),
                    Vec2::splat(38.0),
                );
                EmptyUserpic::for_peer(peer.peer_id.bare(), &peer.title, colors)
                    .paint_circle_or_photo(ui.painter(), av_rect, peer.peer_id.0);

                let tx = av_rect.right() + 10.0;
                ui.painter().text(
                    Pos2::new(tx, rect.top() + 15.0),
                    Align2::LEFT_CENTER,
                    &peer.title,
                    FontId::new(13.5, family_semibold()),
                    colors.color("windowBoldFg"),
                );

                let subtitle = match kind {
                    CommunityChatsKind::Joined => {
                        if !peer.last_message.is_empty() {
                            peer.last_message.clone()
                        } else if peer.members_count > 0 {
                            let noun = if peer.is_channel {
                                "subscribers"
                            } else {
                                "members"
                            };
                            format!("{} {noun}", peer.members_count)
                        } else {
                            "Joined".to_owned()
                        }
                    }
                    CommunityChatsKind::Discoverable => {
                        if peer.members_count > 0 {
                            let noun = if peer.is_channel {
                                "subscribers"
                            } else {
                                "members"
                            };
                            format!("{} {noun}", peer.members_count)
                        } else if peer.is_channel {
                            "Channel".to_owned()
                        } else {
                            "Group".to_owned()
                        }
                    }
                    CommunityChatsKind::Bots => {
                        if peer.is_hidden() {
                            "bot · hidden".to_owned()
                        } else if let Some(ref u) = peer.username {
                            format!("@{u}")
                        } else {
                            "bot".to_owned()
                        }
                    }
                };
                ui.painter().text(
                    Pos2::new(tx, rect.top() + 34.0),
                    Align2::LEFT_CENTER,
                    &subtitle,
                    FontId::new(12.0, family_regular()),
                    colors.color("windowSubTextFg"),
                );

                if kind == CommunityChatsKind::Joined && peer.unread_count > 0 {
                    let badge = peer.unread_count.to_string();
                    let bg = if peer.muted {
                        colors.color("dialogsUnreadBgMuted")
                    } else {
                        colors.color("dialogsUnreadBg")
                    };
                    let b_rect = Rect::from_min_size(
                        Pos2::new(rect.right() - 30.0, rect.center().y - 10.0),
                        Vec2::new(24.0, 20.0),
                    );
                    ui.painter().rect_filled(b_rect, CornerRadius::same(10), bg);
                    ui.painter().text(
                        b_rect.center(),
                        Align2::CENTER_CENTER,
                        &badge,
                        FontId::new(11.0, family_semibold()),
                        colors.color("dialogsUnreadFg"),
                    );
                }

                if resp.clicked() {
                    if peer.is_hidden() && !peer.joined && !peer.can_view_history {
                        action = Some(CommunityChatsListAction::HiddenInaccessible);
                    } else {
                        action = Some(CommunityChatsListAction::OpenPeer(peer.peer_id));
                    }
                }

                if can_manage {
                    let id = ui.make_persistent_id((salt, "remove", peer.peer_id.0));
                    ui.push_id(id, |ui| {
                        if ui.small_button("Remove").clicked() {
                            action = Some(CommunityChatsListAction::RemovePeer(peer.peer_id));
                        }
                    });
                }
            });
        }
        action
    }
}
