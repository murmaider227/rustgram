//! Stateful Info/Profile container with sub-section stack, top bar, profile cover,
//! details/actions, members list, common groups, and shared media tabs.
//!
//! Port origin: `Telegram/SourceFiles/info/info_wrap_widget.h`,
//! `Telegram/SourceFiles/info/info_wrap_widget.cpp`,
//! `Telegram/SourceFiles/info/info_controller.h`,
//! and `Telegram/SourceFiles/info/profile/info_profile_widget.cpp`.

use std::collections::HashSet;

use egui::{CornerRadius, ScrollArea, Ui};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::IconKind;
use tg_ui::widgets::labels::divider_bar;
use tg_ui::widgets::menu::MenuItem;

use crate::info::actions::{ProfileAction, ProfileActionsView};
use crate::info::backend::{
    InfoBackend, InfoPeerKind, PeerProfile, ProfilePhotoRef, SharedMediaTab,
};
use crate::info::balance_pill::BalancePillClick;
use crate::info::birthday_effect::{
    ProfileBirthdayEffect, birthday_age, current_utc_ymd, is_birthday_today,
};
use crate::info::community::{
    CommunityInfoAction, CommunityInfoSection, CommunityRequestsSection,
    CommunityRequestsSectionAction,
};
use crate::info::cover::{CoverAction, ProfileCover};
use crate::info::downloads::{DownloadsAction, DownloadsSection};
use crate::info::global_media::{GLOBAL_MEDIA_PAGE_SIZE, GlobalMediaAction, GlobalMediaSection};
use crate::info::media::{MediaListAction, MediaListWidget, MediaProvider};
use crate::info::members::{CommonGroupsSection, MembersAction, MembersSection};
use crate::info::profile_controls::{
    PersonalChannelAction, PersonalChannelSection, ReportProfilePhotoDraft,
    ReportProfilePhotoOutcome, SuggestBirthdayDraft, SuggestBirthdayOutcome,
    can_set_main_profile_tab, ordered_shared_media_tabs, profile_main_tab_label,
    show_main_profile_tab_bar, show_report_profile_photo_card, show_suggest_birthday_card,
};
use crate::info::profile_tabs::{
    PeerGiftsAction, PeerGiftsSection, ProfileStoriesAction, ProfileStoriesSection,
    SavedMusicAction, SavedMusicSection,
};
use crate::info::similar_channels::{SimilarChannelsAction, SimilarChannelsSection};
use crate::info::strings;
use crate::info::top_bar::{InfoTopBar, TopBarAction, TopBarConfig};
use tg_app_core::stand_in::types::PeerId;

/// Active view inside [`InfoWrapWidget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoSection {
    /// Main peer profile screen (cover + details + actions + inline members or shared media tabs).
    Profile {
        /// Target peer identifier.
        peer_id: PeerId,
        /// Selected inline shared-media tab shown below profile actions.
        media_tab: SharedMediaTab,
    },
    /// Dedicated full-height shared media browser for a peer.
    SharedMedia {
        /// Target peer identifier.
        peer_id: PeerId,
        /// Active shared media tab.
        tab: SharedMediaTab,
    },
    /// Dedicated full-height searchable members list for a group/channel.
    Members {
        /// Target group/channel identifier.
        peer_id: PeerId,
    },
    /// Dedicated Groups in Common list for a user.
    CommonGroups {
        /// Target user identifier.
        peer_id: PeerId,
    },
    /// Similar Channels recommendations tab (`channels.getChannelRecommendations`).
    SimilarChannels {
        /// Target channel identifier.
        peer_id: PeerId,
    },
    /// Downloads Manager tab (`info/downloads/`).
    Downloads {
        /// Owning/context peer identifier.
        peer_id: PeerId,
    },
    /// Global Media tab aggregating shared media across all chats (`info/global_media.rs`).
    GlobalMedia {
        /// Context peer identifier (used when navigating back to profile).
        peer_id: PeerId,
        /// Active global media category tab.
        tab: SharedMediaTab,
    },
    /// Profile Stories & Albums tab (`info/stories/*`).
    Stories {
        /// Target peer identifier.
        peer_id: PeerId,
        /// `false` = pinned stories, `true` = archived stories.
        archive: bool,
    },
    /// Peer Star Gifts & Collections tab (`info/peer_gifts/*`).
    PeerGifts {
        /// Target peer identifier.
        peer_id: PeerId,
    },
    /// Saved Profile Music tab (`info/saved/info_saved_music`).
    SavedMusic {
        /// Target user peer identifier.
        peer_id: PeerId,
    },
    /// Community overview tab (`info/community/info_community_widget.cpp`).
    Community {
        /// Community peer identifier.
        peer_id: PeerId,
    },
    /// Community pending peer link requests tab
    /// (`info/community_requests/info_community_requests_widget.cpp`).
    CommunityRequests {
        /// Community peer identifier.
        peer_id: PeerId,
    },
}

impl InfoSection {
    /// Returns the peer identifier owned by this section.
    #[must_use]
    pub const fn peer_id(self) -> PeerId {
        match self {
            Self::Profile { peer_id, .. }
            | Self::SharedMedia { peer_id, .. }
            | Self::Members { peer_id }
            | Self::CommonGroups { peer_id }
            | Self::SimilarChannels { peer_id }
            | Self::Downloads { peer_id }
            | Self::GlobalMedia { peer_id, .. }
            | Self::Stories { peer_id, .. }
            | Self::PeerGifts { peer_id }
            | Self::SavedMusic { peer_id } => peer_id,
            Self::Community { peer_id } | Self::CommunityRequests { peer_id } => peer_id,
        }
    }
}

/// High-level events emitted from [`InfoWrapWidget`] to the enclosing shell or [`crate::boxes::BoxHost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoWrapEvent {
    /// Close the Info column or pop the outer navigation stack.
    CloseOrBack,
    /// Open the chat history for `PeerId`.
    OpenHistory(PeerId),
    /// Show an informational toast message.
    Toast(String),
    /// Open Edit Peer Info modal box.
    OpenEditPeer(PeerId),
    /// Open Edit Username modal box.
    OpenEditUsername(PeerId),
    /// Open Group Permissions modal box.
    OpenPermissions(PeerId),
    /// Open Administrators list or edit specific admin rights modal box.
    OpenAdminRights {
        /// Group or channel id.
        peer_id: PeerId,
        /// Specific member to edit (if `None`, opens admins list/picker).
        user_id: Option<PeerId>,
    },
    /// Open Edit Member Tag / Rank modal box (`EditTagControl` / `messages.editChatParticipantRank`).
    OpenEditMemberTag {
        /// Group or supergroup id.
        peer_id: PeerId,
        /// Member whose custom rank/tag is edited.
        user_id: PeerId,
    },
    /// Open Add Members peer picker modal box.
    OpenAddMembers(PeerId),
    /// Open Add Contact modal box.
    OpenAddContact,
    /// Open Share / Forward modal box.
    OpenShare {
        /// Message IDs to share/forward.
        message_ids: Vec<i32>,
        /// Optional shareable link text.
        share_link: Option<String>,
    },
    /// Open Report modal box.
    OpenReport(PeerId),
    /// Open Delete Messages / Leave Peer confirmation box.
    OpenDelete {
        /// Target peer id.
        peer_id: PeerId,
        /// Selected message IDs (empty = delete/leave entire chat).
        message_ids: Vec<i32>,
    },
    /// Open Photo Crop modal box for updating peer avatar.
    OpenPhotoCrop(PeerId),
    /// Open Auto-Delete Timer box.
    OpenAutoDelete(PeerId),
    /// Open Linked Discussion Group picker box.
    OpenLinkedDiscussionPicker(PeerId),
    /// Open Create Group wizard.
    OpenCreateGroup,
    /// Open Create Channel wizard.
    OpenCreateChannel,
    /// Open About Rustgram box.
    OpenAbout,
    /// Open Language picker box.
    OpenLanguage,
    /// Open Terms of Service box.
    OpenTerms,
    /// Open Statistics (channel, supergroup, or bot stars revenue) for `PeerId`.
    OpenStatistics(PeerId),
    /// Open Boosts screen for a channel or supergroup.
    OpenBoosts(PeerId),
    /// Open a Bot Mini App for `PeerId`.
    OpenBotMiniApp(PeerId),
    /// Open single-chat data export (`AppExportHost::open_single_chat_export`) for `PeerId`.
    ExportChatHistory(PeerId),
    /// Open Chat History Import wizard (`ChatImportBoxState` at `SelectFile`) for `PeerId`.
    ImportChatHistory(PeerId),
    /// Open Edit Managed Bot Info modal (`EditBotInfoBox`) for a bot peer.
    OpenEditBotInfo(PeerId),
    /// Open a shared media item in the media viewer or link opener.
    OpenMedia {
        /// Peer owning the shared media.
        peer_id: PeerId,
        /// Shared media item / message identifier.
        item_id: i32,
        /// Active shared media tab when clicked.
        tab: SharedMediaTab,
    },
    /// Open Profile QR Card modal (`PeerQrBox`).
    OpenPeerQr(PeerId),
    /// Open Emoji Userpic Builder modal (`info/userpic/*`).
    OpenEmojiUserpic(PeerId),
    /// Open Per-Chat Allowed Reactions modal (`EditPeerReactionsBox`).
    OpenPeerReactions(PeerId),
    /// Open Bot Custom Verification modal (`VerifyPeersBox`).
    OpenVerifyPeers(PeerId),
    /// Open a story selected from the Profile Stories / Albums tab.
    OpenProfileStory {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Selected story ID.
        story_id: i32,
    },
    /// Open Fragment `CollectibleInfoBox` (`fragment.getCollectibleInfo`) for a peer's
    /// collectible username or anonymous phone number.
    OpenCollectibleInfo {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Collectible entity (username without `@` or phone with `+`).
        entity: String,
    },
    /// User clicked the TON or Stars `info_profile_balance_pill` (`info_profile_balance_pill.cpp`).
    OpenBalanceDetails {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Whether TON or Stars pill was clicked.
        kind: BalancePillClick,
    },
}

/// Stateful host for the Info/Profile section (`Info::WrapWidget`).
#[derive(Debug)]
pub struct InfoWrapWidget {
    current: InfoSection,
    back_stack: Vec<InfoSection>,
    top_bar: InfoTopBar,
    members_search: String,
    downloads_search: String,
    global_media_search: String,
    global_media_limit: usize,
    media_list: MediaListWidget,
    selected_media_ids: HashSet<i32>,
    selected_global_media: HashSet<(PeerId, i32)>,
    selected_story_album: Option<i32>,
    peer_gifts_filter: tg_payments::star_gifts::PeerGiftsFilter,
    selected_gift_id: Option<u64>,
    saved_music_search: String,
    playing_music_doc_id: Option<i64>,
    personal_channel_picker_open: bool,
    suggest_birthday_draft: Option<SuggestBirthdayDraft>,
    report_photo_draft: Option<ReportProfilePhotoDraft>,
    /// Show inline shared media tabs below profile actions (used for user/channel/saved profiles).
    pub show_inline_media: bool,
    /// Show inline members block on group profiles.
    pub show_inline_members: bool,
    /// Active single-shot profile birthday confetti animation (`info_profile_birthday_effect.cpp`).
    birthday_effect: Option<ProfileBirthdayEffect>,
    /// Peer ID for which the birthday effect has already been triggered in this view.
    birthday_effect_peer: Option<PeerId>,
    /// Optional deterministic `(year, month, day)` override for birthday matching in tests.
    birthday_today_override: Option<(u16, u8, u8)>,
    /// True when `PowerSavingFlags::CHAT_EFFECTS` / reduced motion disables birthday confetti.
    pub chat_effects_paused: bool,
}

impl InfoWrapWidget {
    /// Creates an [`InfoWrapWidget`] opened to `peer_id`'s profile.
    #[must_use]
    pub fn for_peer(peer_id: PeerId) -> Self {
        Self {
            current: InfoSection::Profile {
                peer_id,
                media_tab: SharedMediaTab::PhotosVideos,
            },
            back_stack: Vec::new(),
            top_bar: InfoTopBar::new(),
            members_search: String::new(),
            downloads_search: String::new(),
            global_media_search: String::new(),
            global_media_limit: GLOBAL_MEDIA_PAGE_SIZE,
            media_list: MediaListWidget::new(),
            selected_media_ids: HashSet::new(),
            selected_global_media: HashSet::new(),
            selected_story_album: None,
            peer_gifts_filter: tg_payments::star_gifts::PeerGiftsFilter::default(),
            selected_gift_id: None,
            saved_music_search: String::new(),
            playing_music_doc_id: None,
            personal_channel_picker_open: false,
            suggest_birthday_draft: None,
            report_photo_draft: None,
            show_inline_media: true,
            show_inline_members: true,
            birthday_effect: None,
            birthday_effect_peer: None,
            birthday_today_override: None,
            chat_effects_paused: false,
        }
    }

    /// Overrides the calendar `(year, month, day)` used to detect whether a peer's birthday is today
    /// (for deterministic headless tests and snapshots).
    pub fn set_birthday_today_override(&mut self, ymd: Option<(u16, u8, u8)>) {
        self.birthday_today_override = ymd;
        self.birthday_effect_peer = None;
        self.birthday_effect = None;
    }

    /// Returns `true` if a profile birthday confetti burst is currently playing.
    #[must_use]
    pub fn is_birthday_effect_active(&self) -> bool {
        self.birthday_effect
            .as_ref()
            .is_some_and(ProfileBirthdayEffect::is_active)
    }

    /// Returns the number of confetti animation frames rendered for the current profile visit.
    #[must_use]
    pub fn birthday_effect_frames_rendered(&self) -> u32 {
        self.birthday_effect
            .as_ref()
            .map_or(0, ProfileBirthdayEffect::frames_rendered)
    }

    /// Navigates to the Global Media tab aggregating media across all chats.
    pub fn open_global_media(&mut self, tab: SharedMediaTab) {
        let peer_id = self.current.peer_id();
        self.global_media_limit = GLOBAL_MEDIA_PAGE_SIZE;
        self.selected_global_media.clear();
        self.push_section(InfoSection::GlobalMedia { peer_id, tab });
    }

    /// Navigates to the Similar Channels tab for the current peer.
    pub fn open_similar_channels(&mut self) {
        let peer_id = self.current.peer_id();
        self.push_section(InfoSection::SimilarChannels { peer_id });
    }

    /// Navigates to the Downloads Manager tab.
    pub fn open_downloads(&mut self) {
        let peer_id = self.current.peer_id();
        self.push_section(InfoSection::Downloads { peer_id });
    }

    /// Navigates to the Profile Stories / Albums tab for the current peer.
    pub fn open_stories(&mut self, archive: bool) {
        let peer_id = self.current.peer_id();
        self.selected_story_album = None;
        self.push_section(InfoSection::Stories { peer_id, archive });
    }

    /// Navigates to the Peer Star Gifts tab for the current peer.
    pub fn open_peer_gifts(&mut self) {
        let peer_id = self.current.peer_id();
        self.selected_gift_id = None;
        self.push_section(InfoSection::PeerGifts { peer_id });
    }

    /// Navigates to the Saved Profile Music tab for the current peer.
    pub fn open_saved_music(&mut self) {
        let peer_id = self.current.peer_id();
        self.saved_music_search.clear();
        self.push_section(InfoSection::SavedMusic { peer_id });
    }
    /// Navigates to the Community overview tab (`info/community/`).
    pub fn open_community(&mut self, community_id: PeerId) {
        self.push_section(InfoSection::Community {
            peer_id: community_id,
        });
    }

    /// Navigates to the Community Pending Link Requests tab (`info/community_requests/`).
    pub fn open_community_requests(&mut self, community_id: PeerId) {
        self.push_section(InfoSection::CommunityRequests {
            peer_id: community_id,
        });
    }
    /// Opens the Downloads Manager as the root section (from Main Menu or `Ctrl+J`).
    pub fn show_downloads_root(&mut self, peer_id: PeerId) {
        if !matches!(self.current, InfoSection::Downloads { .. }) {
            self.back_stack.clear();
            self.current = InfoSection::Downloads { peer_id };
            self.top_bar.close_search();
            self.selected_media_ids.clear();
        }
    }

    /// Returns the currently displayed [`InfoSection`].
    #[must_use]
    pub const fn current_section(&self) -> InfoSection {
        self.current
    }

    /// Returns the number of entries in the internal Info back stack.
    #[must_use]
    pub fn back_stack_len(&self) -> usize {
        self.back_stack.len()
    }

    /// Switches to a new root peer profile, clearing any sub-section history if the peer changed
    /// or if the widget was previously showing the standalone Downloads Manager.
    pub fn set_peer(&mut self, peer_id: PeerId) {
        if self.current.peer_id() != peer_id
            || matches!(self.current, InfoSection::Downloads { .. })
        {
            self.back_stack.clear();
            self.current = InfoSection::Profile {
                peer_id,
                media_tab: SharedMediaTab::PhotosVideos,
            };
            self.top_bar.close_search();
            self.members_search.clear();
            self.selected_media_ids.clear();
            self.personal_channel_picker_open = false;
            self.suggest_birthday_draft = None;
            self.report_photo_draft = None;
        }
    }

    /// Sets the active shared media tab on the current section.
    pub fn set_media_tab(&mut self, tab: SharedMediaTab) {
        match &mut self.current {
            InfoSection::Profile { media_tab, .. } => *media_tab = tab,
            InfoSection::SharedMedia { tab: active, .. } => *active = tab,
            _ => {}
        }
    }

    /// Pushes a sub-section (e.g. Shared Media, Members, Common Groups, or another user's profile).
    pub fn push_section(&mut self, next: InfoSection) {
        if self.current != next {
            self.back_stack.push(self.current);
            self.current = next;
            self.top_bar.close_search();
            self.selected_media_ids.clear();
        }
    }

    /// Pops one entry from the internal Info stack; returns `false` if already at root.
    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.back_stack.pop() {
            self.current = prev;
            self.top_bar.close_search();
            self.selected_media_ids.clear();
            true
        } else {
            false
        }
    }

    /// Returns the currently selected media item IDs.
    #[must_use]
    pub fn selected_media_ids(&self) -> &HashSet<i32> {
        &self.selected_media_ids
    }

    /// Renders the entire Info/Profile widget inside `ui`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        in_third_column: bool,
        colors: &dyn Colors,
    ) -> Vec<InfoWrapEvent> {
        let mut events = Vec::new();
        let rect = ui.max_rect();
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBg"));

        let peer_id = self.current.peer_id();
        let peer = backend.peer_profile(peer_id);

        // 1. Top bar (`54px`).
        let (title, subtitle, show_search, show_edit, menu_items) =
            build_top_bar_meta(self.current, peer.as_ref());
        let top_cfg = TopBarConfig {
            title: &title,
            subtitle: subtitle.as_deref(),
            show_back: !self.back_stack.is_empty() || !in_third_column,
            show_close: in_third_column,
            show_search,
            show_edit,
            menu_items,
            selected_count: self.selected_media_ids.len(),
        };

        if let Some(act) = self.top_bar.show(ui, "info_wrap_topbar", &top_cfg, colors) {
            match act {
                TopBarAction::Back => {
                    if !self.go_back() {
                        events.push(InfoWrapEvent::CloseOrBack);
                    }
                }
                TopBarAction::Close => events.push(InfoWrapEvent::CloseOrBack),
                TopBarAction::Edit => {
                    if peer.as_ref().is_some_and(|p| p.kind == InfoPeerKind::Bot) {
                        events.push(InfoWrapEvent::OpenEditBotInfo(peer_id));
                    } else {
                        events.push(InfoWrapEvent::OpenEditPeer(peer_id));
                    }
                }
                TopBarAction::CancelSelection => self.selected_media_ids.clear(),
                TopBarAction::ForwardSelected => {
                    let ids: Vec<i32> = self.selected_media_ids.iter().copied().collect();
                    events.push(InfoWrapEvent::OpenShare {
                        message_ids: ids,
                        share_link: None,
                    });
                }
                TopBarAction::DeleteSelected => {
                    let ids: Vec<i32> = self.selected_media_ids.iter().copied().collect();
                    events.push(InfoWrapEvent::OpenDelete {
                        peer_id,
                        message_ids: ids,
                    });
                }
                TopBarAction::MenuCommand(cmd) => {
                    self.handle_menu_command(&cmd, peer.as_ref(), backend, &mut events);
                }
            }
        }

        // 2. Scrollable body.
        // Community sections may refer to a community channel ID that is tracked in
        // `backend.community_details(peer_id)` rather than `backend.peer_profile(peer_id)`.
        if matches!(
            self.current,
            InfoSection::Community { .. } | InfoSection::CommunityRequests { .. }
        ) {
            ScrollArea::vertical()
                .id_salt(("tg_info_wrap_comm_scroll", peer_id.0, self.back_stack.len()))
                .auto_shrink([false, false])
                .show(ui, |ui| match self.current {
                    InfoSection::Community { peer_id: cid } => {
                        if let Some(details) = backend.community_details(cid) {
                            if let Some(act) =
                                CommunityInfoSection::show(ui, &details, backend, colors)
                            {
                                match act {
                                    CommunityInfoAction::OpenPeer(pid) => {
                                        events.push(InfoWrapEvent::OpenHistory(pid));
                                    }
                                    CommunityInfoAction::OpenRequests(cid) => {
                                        self.push_section(InfoSection::CommunityRequests {
                                            peer_id: cid,
                                        });
                                    }
                                    CommunityInfoAction::OpenManage(cid)
                                    | CommunityInfoAction::OpenAddChat(cid) => {
                                        events.push(InfoWrapEvent::OpenEditPeer(cid));
                                    }
                                    CommunityInfoAction::CollapsedToggled(collapsed) => {
                                        events.push(InfoWrapEvent::Toast(if collapsed {
                                            "Community chats grouped into one row".to_owned()
                                        } else {
                                            "Community chats shown individually".to_owned()
                                        }));
                                    }
                                    CommunityInfoAction::PeerRemoved(_) => {
                                        events.push(InfoWrapEvent::Toast(
                                            "Chat removed from community".to_owned(),
                                        ));
                                    }
                                    CommunityInfoAction::Toast(msg) => {
                                        events.push(InfoWrapEvent::Toast(msg));
                                    }
                                }
                            }
                        } else {
                            ui.label("Community is unavailable.");
                        }
                    }
                    InfoSection::CommunityRequests { peer_id: cid } => {
                        let slice = backend
                            .community_peer_link_requests(cid, "", 50)
                            .unwrap_or_default();
                        if let Some(act) = CommunityRequestsSection::show(ui, &slice, colors) {
                            match act {
                                CommunityRequestsSectionAction::Approve(pid) => {
                                    match backend.toggle_community_peer_link_request_approval(
                                        cid, pid, false,
                                    ) {
                                        Ok(()) => events.push(InfoWrapEvent::Toast(
                                            "Link request approved".to_owned(),
                                        )),
                                        Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                    }
                                }
                                CommunityRequestsSectionAction::Reject(pid) => {
                                    match backend
                                        .toggle_community_peer_link_request_approval(cid, pid, true)
                                    {
                                        Ok(()) => events.push(InfoWrapEvent::Toast(
                                            "Link request rejected".to_owned(),
                                        )),
                                        Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                    }
                                }
                                CommunityRequestsSectionAction::ApproveAll => {
                                    match backend
                                        .toggle_all_community_peer_link_request_approval(cid, false)
                                    {
                                        Ok(()) => events.push(InfoWrapEvent::Toast(
                                            "Approved all link requests".to_owned(),
                                        )),
                                        Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                    }
                                }
                                CommunityRequestsSectionAction::RejectAll => {
                                    match backend
                                        .toggle_all_community_peer_link_request_approval(cid, true)
                                    {
                                        Ok(()) => events.push(InfoWrapEvent::Toast(
                                            "Rejected all link requests".to_owned(),
                                        )),
                                        Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                });
            return events;
        }
        let top_query = self.top_bar.search_query().to_owned();
        if matches!(self.current, InfoSection::Downloads { .. }) {
            ScrollArea::vertical()
                .id_salt(("tg_info_wrap_downloads_scroll", self.back_stack.len()))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let effective_q = if !top_query.is_empty() {
                        top_query.as_str()
                    } else {
                        self.downloads_search.as_str()
                    };
                    let entries = backend.downloads_list(effective_q);
                    if let Some(dl_act) =
                        DownloadsSection::show(ui, &entries, &mut self.downloads_search, colors)
                    {
                        match dl_act {
                            DownloadsAction::TogglePause(id) => {
                                if let Err(err) = backend.toggle_download_pause(id) {
                                    events.push(InfoWrapEvent::Toast(err));
                                }
                            }
                            DownloadsAction::Delete {
                                download_id,
                                delete_file,
                            } => {
                                match backend.cancel_or_delete_download(download_id, delete_file) {
                                    Ok(()) => events
                                        .push(InfoWrapEvent::Toast("Download removed".to_owned())),
                                    Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                }
                            }
                            DownloadsAction::OpenSourceChat(pid) => {
                                events.push(InfoWrapEvent::OpenHistory(pid));
                            }
                            DownloadsAction::ClearCompleted => {
                                if let Ok(n) = backend.clear_completed_downloads() {
                                    events.push(InfoWrapEvent::Toast(format!(
                                        "Cleared {n} completed download(s)"
                                    )));
                                }
                            }
                            DownloadsAction::DeleteAll => {
                                if let Ok(n) = backend.delete_all_downloads() {
                                    events.push(InfoWrapEvent::Toast(format!(
                                        "Deleted {n} download(s)"
                                    )));
                                }
                            }
                        }
                    }
                });
            return events;
        }

        let Some(peer) = peer else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(strings::PEER_UNAVAILABLE)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
            return events;
        };

        ScrollArea::vertical()
            .id_salt(("tg_info_wrap_scroll", peer.id.0, self.back_stack.len()))
            .auto_shrink([false, false])
            .show(ui, |ui| match self.current {
                InfoSection::Profile {
                    peer_id,
                    mut media_tab,
                } => {
                    let (today_year, today_month, today_day) =
                        self.birthday_today_override.unwrap_or_else(current_utc_ymd);
                    if self.birthday_effect_peer != Some(peer_id) {
                        self.birthday_effect_peer = Some(peer_id);
                        self.birthday_effect = peer.birthday.and_then(|bday| {
                            if is_birthday_today(bday, today_month, today_day) {
                                Some(ProfileBirthdayEffect::start(
                                    peer_id.0 as u64,
                                    birthday_age(bday, today_year),
                                    self.chat_effects_paused,
                                ))
                            } else {
                                None
                            }
                        });
                    }
                    let now_secs = ui.ctx().input(|i| i.time);
                    if let Some(ref mut eff) = self.birthday_effect {
                        eff.tick(now_secs, self.chat_effects_paused);
                    }
                    let profile_balance = backend.peer_profile_balance(peer_id);
                    if let Some(cover_act) = ProfileCover::show_with_extras(
                        ui,
                        &peer,
                        colors,
                        self.birthday_effect.as_mut(),
                        profile_balance.as_ref(),
                    ) {
                        match cover_act {
                            CoverAction::AvatarClicked => {
                                if peer.can_edit {
                                    events.push(InfoWrapEvent::OpenPhotoCrop(peer_id));
                                } else {
                                    events.push(InfoWrapEvent::Toast(format!(
                                        "Viewing {}'s profile photo",
                                        peer.title
                                    )));
                                }
                            }
                            CoverAction::BalancePillClicked(kind) => {
                                events.push(InfoWrapEvent::OpenBalanceDetails { peer_id, kind });
                            }
                            CoverAction::ReportProfilePhoto => {
                                self.report_photo_draft =
                                    Some(ReportProfilePhotoDraft::new(peer_id, &peer.title));
                                self.suggest_birthday_draft = None;
                            }
                        }
                    }

                    // Personal Channel row & inline picker (`account.updatePersonalChannel`)
                    let current_personal_ch = backend.personal_channel(peer_id);
                    let personal_ch_candidates = if self.personal_channel_picker_open {
                        backend.personal_channel_candidates()
                    } else {
                        Vec::new()
                    };
                    if let Some(ch_act) = PersonalChannelSection::show(
                        ui,
                        &peer,
                        current_personal_ch.as_ref(),
                        &personal_ch_candidates,
                        &mut self.personal_channel_picker_open,
                        colors,
                    ) {
                        match ch_act {
                            PersonalChannelAction::OpenChannel(cid) => {
                                if backend.peer_profile(cid).is_some() {
                                    self.push_section(InfoSection::Profile {
                                        peer_id: cid,
                                        media_tab: SharedMediaTab::PhotosVideos,
                                    });
                                } else {
                                    events.push(InfoWrapEvent::OpenHistory(cid));
                                }
                            }
                            PersonalChannelAction::SelectChannel(cid) => {
                                match backend.update_personal_channel(Some(cid)) {
                                    Ok(()) => events.push(InfoWrapEvent::Toast(
                                        tr::lng_settings_channel_saved(),
                                    )),
                                    Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                }
                            }
                            PersonalChannelAction::RemoveChannel => {
                                match backend.update_personal_channel(None) {
                                    Ok(()) => events.push(InfoWrapEvent::Toast(
                                        tr::lng_settings_channel_removed(),
                                    )),
                                    Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                }
                            }
                        }
                    }

                    // Inline Suggest Date of Birth card (`users.suggestBirthday`)
                    if let Some(mut draft) = self.suggest_birthday_draft.take() {
                        match show_suggest_birthday_card(ui, &mut draft, colors) {
                            Some(SuggestBirthdayOutcome::Submit(bday)) => {
                                match backend.suggest_user_birthday(draft.peer_id, bday) {
                                    Ok(()) => events.push(InfoWrapEvent::Toast(format!(
                                        "Suggested birthday to {}",
                                        draft.peer_title
                                    ))),
                                    Err(err) => {
                                        draft.error = Some(err);
                                        self.suggest_birthday_draft = Some(draft);
                                    }
                                }
                            }
                            Some(SuggestBirthdayOutcome::Cancel) => {}
                            None => self.suggest_birthday_draft = Some(draft),
                        }
                    }

                    // Inline Report Profile Photo card (`account.reportProfilePhoto`)
                    if let Some(mut draft) = self.report_photo_draft.take() {
                        match show_report_profile_photo_card(ui, &mut draft, colors) {
                            Some(ReportProfilePhotoOutcome::Submit { reason, comment }) => {
                                let photo = ProfilePhotoRef {
                                    id: peer_id.bare() as i64,
                                    access_hash: 0,
                                    file_reference: Vec::new(),
                                };
                                match backend.report_profile_photo(
                                    draft.peer_id,
                                    photo,
                                    reason,
                                    &comment,
                                ) {
                                    Ok(()) => events.push(InfoWrapEvent::Toast(
                                        "Profile photo reported".to_owned(),
                                    )),
                                    Err(err) => {
                                        draft.error = Some(err);
                                        self.report_photo_draft = Some(draft);
                                    }
                                }
                            }
                            Some(ReportProfilePhotoOutcome::Cancel) => {}
                            None => self.report_photo_draft = Some(draft),
                        }
                    }

                    if let Some(act) = ProfileActionsView::show_with_today(
                        ui,
                        &peer,
                        in_third_column,
                        (today_year, today_month, today_day),
                        colors,
                    ) {
                        self.handle_profile_action(act, &peer, backend, &mut events);
                    }

                    // Inline members list on group profiles
                    if self.show_inline_members && peer.kind.is_group() {
                        ui.add_space(6.0);
                        divider_bar(ui, 8.0, colors);
                        ui.add_space(4.0);
                        let effective_q = if !top_query.is_empty() {
                            top_query.as_str()
                        } else {
                            self.members_search.as_str()
                        };
                        let members = backend.peer_members(peer_id, effective_q);
                        if let Some(m_act) = MembersSection::show_with_ranks(
                            ui,
                            "inline_profile",
                            peer.members_count,
                            &members,
                            &mut self.members_search,
                            true,
                            peer.can_add_members,
                            peer.can_manage_admins,
                            colors,
                            |uid| backend.member_rank(peer_id, uid),
                        ) {
                            self.handle_members_action(m_act, peer_id, backend, &mut events);
                        }
                    }

                    // Inline shared media tabs strip + Set Main Profile Tab (`info_profile_tabs_host.cpp`)
                    if self.show_inline_media {
                        ui.add_space(6.0);
                        divider_bar(ui, 8.0, colors);
                        let current_main = backend.main_profile_tab(peer_id);
                        if let Some(chosen_main) =
                            show_main_profile_tab_bar(ui, &peer, current_main, colors)
                        {
                            match backend.set_main_profile_tab(peer_id, chosen_main) {
                                Ok(()) => events.push(InfoWrapEvent::Toast(format!(
                                    "Main profile tab set to {}",
                                    profile_main_tab_label(chosen_main)
                                ))),
                                Err(err) => events.push(InfoWrapEvent::Toast(err)),
                            }
                        }
                        let current_main = backend.main_profile_tab(peer_id);
                        let ordered_tabs = ordered_shared_media_tabs(current_main);
                        let can_set_media_main = can_set_main_profile_tab(
                            &peer,
                            crate::info::backend::ProfileMainTab::Media,
                        );
                        if let Some(ctx_main) = MediaListWidget::show_tabs_with_main_tab(
                            ui,
                            "inline_media_tabs",
                            &mut media_tab,
                            &ordered_tabs,
                            current_main,
                            can_set_media_main,
                            colors,
                        ) {
                            match backend.set_main_profile_tab(peer_id, ctx_main) {
                                Ok(()) => events.push(InfoWrapEvent::Toast(format!(
                                    "Main profile tab set to {}",
                                    profile_main_tab_label(ctx_main)
                                ))),
                                Err(err) => events.push(InfoWrapEvent::Toast(err)),
                            }
                        }
                        if let InfoSection::Profile {
                            media_tab: stored, ..
                        } = &mut self.current
                        {
                            *stored = media_tab;
                        }
                        let items = MediaProvider::load(backend, peer_id, media_tab, &top_query);
                        if let Some(m_ev) = self.media_list.show_items(
                            ui,
                            media_tab,
                            &items,
                            &self.selected_media_ids,
                            colors,
                        ) {
                            self.handle_media_event(m_ev, &mut events);
                        }
                    }

                    if let Some(act) = ProfileActionsView::show_footer_actions(ui, &peer, colors) {
                        self.handle_profile_action(act, &peer, backend, &mut events);
                    }
                    ui.add_space(16.0);
                }
                InfoSection::SharedMedia { peer_id, mut tab } => {
                    MediaListWidget::show_tabs(ui, "sub_media_tabs", &mut tab, colors);
                    if let InfoSection::SharedMedia { tab: stored, .. } = &mut self.current {
                        *stored = tab;
                    }
                    let items = MediaProvider::load(backend, peer_id, tab, &top_query);
                    if let Some(m_ev) = self.media_list.show_items(
                        ui,
                        tab,
                        &items,
                        &self.selected_media_ids,
                        colors,
                    ) {
                        self.handle_media_event(m_ev, &mut events);
                    }
                    ui.add_space(16.0);
                }
                InfoSection::Members { peer_id } => {
                    let effective_q = if !top_query.is_empty() {
                        top_query.as_str()
                    } else {
                        self.members_search.as_str()
                    };
                    let members = backend.peer_members(peer_id, effective_q);
                    if let Some(m_act) = MembersSection::show_with_ranks(
                        ui,
                        "full_members",
                        peer.members_count,
                        &members,
                        &mut self.members_search,
                        true,
                        peer.can_add_members,
                        peer.can_manage_admins,
                        colors,
                        |uid| backend.member_rank(peer_id, uid),
                    ) {
                        self.handle_members_action(m_act, peer_id, backend, &mut events);
                    }
                    ui.add_space(16.0);
                }
                InfoSection::CommonGroups { peer_id } => {
                    let groups = backend.common_groups(peer_id);
                    if let Some(gid) = CommonGroupsSection::show(ui, &groups, colors) {
                        self.push_section(InfoSection::Profile {
                            peer_id: gid,
                            media_tab: SharedMediaTab::PhotosVideos,
                        });
                    }
                }
                InfoSection::SimilarChannels { peer_id } => {
                    let similar = backend.similar_channels(peer_id).unwrap_or_default();
                    if let Some(sim_act) = SimilarChannelsSection::show(ui, &similar, colors) {
                        match sim_act {
                            SimilarChannelsAction::OpenChannel(cid) => {
                                if backend.peer_profile(cid).is_some() {
                                    self.push_section(InfoSection::Profile {
                                        peer_id: cid,
                                        media_tab: SharedMediaTab::PhotosVideos,
                                    });
                                } else {
                                    events.push(InfoWrapEvent::OpenHistory(cid));
                                }
                            }
                            SimilarChannelsAction::RequestPremiumUnlock => {
                                events.push(InfoWrapEvent::Toast(
                                    "Telegram Premium unlocks up to 100 similar channels."
                                        .to_owned(),
                                ));
                            }
                        }
                    }
                }
                InfoSection::Downloads { .. } => {
                    let effective_q = if !top_query.is_empty() {
                        top_query.as_str()
                    } else {
                        self.downloads_search.as_str()
                    };
                    let entries = backend.downloads_list(effective_q);
                    if let Some(dl_act) =
                        DownloadsSection::show(ui, &entries, &mut self.downloads_search, colors)
                    {
                        match dl_act {
                            DownloadsAction::TogglePause(id) => {
                                if let Err(err) = backend.toggle_download_pause(id) {
                                    events.push(InfoWrapEvent::Toast(err));
                                }
                            }
                            DownloadsAction::Delete {
                                download_id,
                                delete_file,
                            } => {
                                match backend.cancel_or_delete_download(download_id, delete_file) {
                                    Ok(()) => events
                                        .push(InfoWrapEvent::Toast("Download removed".to_owned())),
                                    Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                }
                            }
                            DownloadsAction::OpenSourceChat(pid) => {
                                events.push(InfoWrapEvent::OpenHistory(pid));
                            }
                            DownloadsAction::ClearCompleted => {
                                if let Ok(n) = backend.clear_completed_downloads() {
                                    events.push(InfoWrapEvent::Toast(format!(
                                        "Cleared {n} completed download(s)"
                                    )));
                                }
                            }
                            DownloadsAction::DeleteAll => {
                                if let Ok(n) = backend.delete_all_downloads() {
                                    events.push(InfoWrapEvent::Toast(format!(
                                        "Deleted {n} download(s)"
                                    )));
                                }
                            }
                        }
                    }
                }
                InfoSection::GlobalMedia { mut tab, .. } => {
                    let effective_q = if !top_query.is_empty() {
                        top_query.as_str()
                    } else {
                        self.global_media_search.as_str()
                    };
                    let page = backend.global_media(tab, effective_q, self.global_media_limit);
                    if let Some(gm_act) = GlobalMediaSection::show(
                        ui,
                        &mut tab,
                        &mut self.global_media_search,
                        &page,
                        &self.selected_global_media,
                        colors,
                    ) {
                        match gm_act {
                            GlobalMediaAction::TabChanged(new_tab) => {
                                if let InfoSection::GlobalMedia { tab: stored, .. } =
                                    &mut self.current
                                {
                                    *stored = new_tab;
                                }
                                self.global_media_limit = GLOBAL_MEDIA_PAGE_SIZE;
                                self.selected_global_media.clear();
                            }
                            GlobalMediaAction::OpenMedia {
                                peer_id: src_peer,
                                item_id,
                                tab: item_tab,
                            } => {
                                events.push(InfoWrapEvent::OpenMedia {
                                    peer_id: src_peer,
                                    item_id,
                                    tab: item_tab,
                                });
                            }
                            GlobalMediaAction::OpenSourceChat {
                                peer_id: src_peer, ..
                            } => {
                                events.push(InfoWrapEvent::OpenHistory(src_peer));
                            }
                            GlobalMediaAction::ToggleSelect {
                                peer_id: src_peer,
                                item_id,
                            } => {
                                let key = (src_peer, item_id);
                                if !self.selected_global_media.remove(&key) {
                                    self.selected_global_media.insert(key);
                                }
                            }
                            GlobalMediaAction::LoadMore => {
                                self.global_media_limit = self
                                    .global_media_limit
                                    .saturating_add(GLOBAL_MEDIA_PAGE_SIZE);
                            }
                        }
                    }
                }
                InfoSection::Stories { peer_id, archive } => {
                    let albums = backend.peer_story_albums(peer_id).unwrap_or_default();
                    let page = if archive {
                        backend
                            .peer_stories_archive(peer_id, 0, 50)
                            .unwrap_or_default()
                    } else if let Some(aid) = self.selected_story_album {
                        backend
                            .peer_album_stories(peer_id, aid, 0, 50)
                            .unwrap_or_default()
                    } else {
                        backend
                            .peer_pinned_stories(peer_id, 0, 50)
                            .unwrap_or_default()
                    };
                    let can_archive = peer.can_edit || peer.kind == InfoPeerKind::SavedMessages;
                    if let Some(st_act) = ProfileStoriesSection::show(
                        ui,
                        peer_id,
                        archive,
                        can_archive,
                        self.selected_story_album,
                        &albums,
                        &page,
                        colors,
                    ) {
                        match st_act {
                            ProfileStoriesAction::SwitchArchive(next_archive) => {
                                self.selected_story_album = None;
                                self.current = InfoSection::Stories {
                                    peer_id,
                                    archive: next_archive,
                                };
                            }
                            ProfileStoriesAction::SelectAlbum(aid) => {
                                self.selected_story_album = aid;
                            }
                            ProfileStoriesAction::OpenStory { peer_id, story_id } => {
                                events.push(InfoWrapEvent::OpenProfileStory { peer_id, story_id });
                            }
                        }
                    }
                    ui.add_space(16.0);
                }
                InfoSection::PeerGifts { peer_id } => {
                    let page = backend
                        .peer_saved_gifts(peer_id, &self.peer_gifts_filter)
                        .unwrap_or_default();
                    let can_manage = peer.can_edit || peer.kind == InfoPeerKind::SavedMessages;
                    let notify_enabled = backend.chat_star_gift_notifications_enabled(peer_id);
                    if let Some(gift_act) = PeerGiftsSection::show_with_notifications(
                        ui,
                        peer_id,
                        can_manage,
                        notify_enabled,
                        &mut self.peer_gifts_filter,
                        &mut self.selected_gift_id,
                        &page,
                        colors,
                    ) {
                        match gift_act {
                            PeerGiftsAction::ToggleVisibility {
                                msg_id,
                                saved_id,
                                unsave,
                            } => match backend
                                .toggle_peer_gift_saved(peer_id, msg_id, saved_id, unsave)
                            {
                                Ok(()) => events.push(InfoWrapEvent::Toast(if unsave {
                                    "Gift hidden from profile".to_owned()
                                } else {
                                    "Gift displayed on profile".to_owned()
                                })),
                                Err(err) => events.push(InfoWrapEvent::Toast(err)),
                            },
                            PeerGiftsAction::ToggleNotifications(enabled) => {
                                match backend.toggle_chat_star_gift_notifications(peer_id, enabled)
                                {
                                    Ok(true) => events.push(InfoWrapEvent::Toast(
                                        tr::lng_peer_gifts_notify_enabled(),
                                    )),
                                    Ok(false) => events.push(InfoWrapEvent::Toast(
                                        "Channel gift notifications disabled".to_owned(),
                                    )),
                                    Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                }
                            }
                        }
                    }
                    ui.add_space(16.0);
                }
                InfoSection::SavedMusic { peer_id } => {
                    let tracks = backend.peer_saved_music(peer_id, 0, 50).unwrap_or_default();
                    let can_manage = peer.can_edit || peer.kind == InfoPeerKind::SavedMessages;
                    if let Some(m_act) = SavedMusicSection::show(
                        ui,
                        &tracks,
                        &mut self.saved_music_search,
                        self.playing_music_doc_id,
                        can_manage,
                        colors,
                    ) {
                        match m_act {
                            SavedMusicAction::TogglePlay(doc_id) => {
                                if self.playing_music_doc_id == Some(doc_id) {
                                    self.playing_music_doc_id = None;
                                } else {
                                    self.playing_music_doc_id = Some(doc_id);
                                }
                            }
                            SavedMusicAction::ToggleSaveProfile { track, unsave } => {
                                match backend.save_profile_music(&track, unsave) {
                                    Ok(()) => events.push(InfoWrapEvent::Toast(if unsave {
                                        format!("Removed \"{}\" from profile music", track.title)
                                    } else {
                                        format!("Saved \"{}\" to profile music", track.title)
                                    })),
                                    Err(err) => events.push(InfoWrapEvent::Toast(err)),
                                }
                            }
                        }
                    }
                    ui.add_space(16.0);
                }
                InfoSection::Community { .. } | InfoSection::CommunityRequests { .. } => {}
            });

        events
    }

    fn handle_profile_action(
        &mut self,
        act: ProfileAction,
        peer: &PeerProfile,
        backend: &mut dyn InfoBackend,
        events: &mut Vec<InfoWrapEvent>,
    ) {
        match act {
            ProfileAction::SendMessage(pid) => events.push(InfoWrapEvent::OpenHistory(pid)),
            ProfileAction::ToggleNotifications(pid) => {
                let muted = backend.toggle_notifications(pid);
                events.push(InfoWrapEvent::Toast(if muted {
                    strings::TOAST_NOTIFICATIONS_MUTED.to_owned()
                } else {
                    strings::TOAST_NOTIFICATIONS_ENABLED.to_owned()
                }));
            }
            ProfileAction::CopyValue { label, value } => {
                events.push(InfoWrapEvent::Toast(format!("{label} copied: {value}")));
            }
            ProfileAction::OpenSharedMedia(tab) => {
                self.push_section(InfoSection::SharedMedia {
                    peer_id: peer.id,
                    tab,
                });
            }
            ProfileAction::OpenMembers => {
                self.push_section(InfoSection::Members { peer_id: peer.id });
            }
            ProfileAction::OpenCommonGroups => {
                self.push_section(InfoSection::CommonGroups { peer_id: peer.id });
            }
            ProfileAction::OpenSimilarChannels => {
                self.push_section(InfoSection::SimilarChannels { peer_id: peer.id });
            }
            ProfileAction::OpenStories => {
                self.selected_story_album = None;
                self.push_section(InfoSection::Stories {
                    peer_id: peer.id,
                    archive: false,
                });
            }
            ProfileAction::OpenPeerGifts => {
                self.selected_gift_id = None;
                self.push_section(InfoSection::PeerGifts { peer_id: peer.id });
            }
            ProfileAction::OpenSavedMusic => {
                self.saved_music_search.clear();
                self.push_section(InfoSection::SavedMusic { peer_id: peer.id });
            }
            ProfileAction::OpenQrCode => events.push(InfoWrapEvent::OpenPeerQr(peer.id)),
            ProfileAction::OpenEmojiUserpic => {
                events.push(InfoWrapEvent::OpenEmojiUserpic(peer.id))
            }
            ProfileAction::OpenVerifyPeers => events.push(InfoWrapEvent::OpenVerifyPeers(peer.id)),
            ProfileAction::OpenEditBotInfo => events.push(InfoWrapEvent::OpenEditBotInfo(peer.id)),
            ProfileAction::OpenEditPeer => events.push(InfoWrapEvent::OpenEditPeer(peer.id)),
            ProfileAction::OpenEditUsername => {
                events.push(InfoWrapEvent::OpenEditUsername(peer.id))
            }
            ProfileAction::OpenPermissions => events.push(InfoWrapEvent::OpenPermissions(peer.id)),
            ProfileAction::OpenAdministrators => events.push(InfoWrapEvent::OpenAdminRights {
                peer_id: peer.id,
                user_id: None,
            }),
            ProfileAction::OpenAddMembers => events.push(InfoWrapEvent::OpenAddMembers(peer.id)),
            ProfileAction::OpenAddContact => events.push(InfoWrapEvent::OpenAddContact),
            ProfileAction::OpenSharePeer => events.push(InfoWrapEvent::OpenShare {
                message_ids: Vec::new(),
                share_link: peer.shareable_link(),
            }),
            ProfileAction::OpenLinkedDiscussion(Some(linked_id)) => {
                self.push_section(InfoSection::Profile {
                    peer_id: linked_id,
                    media_tab: SharedMediaTab::PhotosVideos,
                });
            }
            ProfileAction::OpenLinkedDiscussion(None) => {
                events.push(InfoWrapEvent::OpenLinkedDiscussionPicker(peer.id));
            }
            ProfileAction::OpenAutoDelete => events.push(InfoWrapEvent::OpenAutoDelete(peer.id)),
            ProfileAction::OpenBotMiniApp(pid) => {
                events.push(InfoWrapEvent::OpenBotMiniApp(pid));
            }
            ProfileAction::OpenStatistics(pid) => {
                events.push(InfoWrapEvent::OpenStatistics(pid));
            }
            ProfileAction::OpenBoosts(pid) => {
                events.push(InfoWrapEvent::OpenBoosts(pid));
            }
            ProfileAction::ToggleBlock(pid) => {
                let blocked = backend.toggle_block_user(pid);
                events.push(InfoWrapEvent::Toast(if blocked {
                    format!("{} blocked", peer.title)
                } else {
                    format!("{} unblocked", peer.title)
                }));
            }
            ProfileAction::OpenReport => events.push(InfoWrapEvent::OpenReport(peer.id)),
            ProfileAction::OpenDeleteOrLeave => events.push(InfoWrapEvent::OpenDelete {
                peer_id: peer.id,
                message_ids: Vec::new(),
            }),
            ProfileAction::OpenCollectibleInfo { peer_id, entity } => {
                events.push(InfoWrapEvent::OpenCollectibleInfo { peer_id, entity });
            }
            ProfileAction::SuggestBirthday => {
                self.suggest_birthday_draft = Some(SuggestBirthdayDraft::new(peer.id, &peer.title));
                self.report_photo_draft = None;
            }
            ProfileAction::ReportProfilePhoto => {
                self.report_photo_draft = Some(ReportProfilePhotoDraft::new(peer.id, &peer.title));
                self.suggest_birthday_draft = None;
            }
        }
    }

    fn handle_members_action(
        &mut self,
        act: MembersAction,
        group_id: PeerId,
        backend: &mut dyn InfoBackend,
        events: &mut Vec<InfoWrapEvent>,
    ) {
        match act {
            MembersAction::OpenMemberProfile(uid) => {
                self.push_section(InfoSection::Profile {
                    peer_id: uid,
                    media_tab: SharedMediaTab::PhotosVideos,
                });
            }
            MembersAction::AddMembers => events.push(InfoWrapEvent::OpenAddMembers(group_id)),
            MembersAction::EditAdminRights(uid) => events.push(InfoWrapEvent::OpenAdminRights {
                peer_id: group_id,
                user_id: Some(uid),
            }),
            MembersAction::EditMemberTag(uid) => events.push(InfoWrapEvent::OpenEditMemberTag {
                peer_id: group_id,
                user_id: uid,
            }),
            MembersAction::RemoveMember(uid) => match backend.remove_member(group_id, uid) {
                Ok(()) => events.push(InfoWrapEvent::Toast(
                    strings::TOAST_MEMBER_REMOVED.to_owned(),
                )),
                Err(err) => events.push(InfoWrapEvent::Toast(err)),
            },
        }
    }

    fn handle_media_event(&mut self, ev: MediaListAction, events: &mut Vec<InfoWrapEvent>) {
        let tab = match self.current {
            InfoSection::Profile { media_tab, .. } => media_tab,
            InfoSection::SharedMedia { tab, .. } => tab,
            _ => SharedMediaTab::PhotosVideos,
        };
        match ev {
            MediaListAction::OpenItem(id) => {
                events.push(InfoWrapEvent::OpenMedia {
                    peer_id: self.current.peer_id(),
                    item_id: id,
                    tab,
                });
            }
            MediaListAction::ToggleSelect(id) => {
                if !self.selected_media_ids.remove(&id) {
                    self.selected_media_ids.insert(id);
                }
            }
        }
    }

    fn handle_menu_command(
        &mut self,
        cmd: &str,
        peer: Option<&PeerProfile>,
        backend: &mut dyn InfoBackend,
        events: &mut Vec<InfoWrapEvent>,
    ) {
        let Some(peer) = peer else {
            return;
        };
        match cmd {
            "edit_peer" => events.push(InfoWrapEvent::OpenEditPeer(peer.id)),
            "edit_bot_info" => events.push(InfoWrapEvent::OpenEditBotInfo(peer.id)),
            "edit_username" => events.push(InfoWrapEvent::OpenEditUsername(peer.id)),
            "qr_code" => events.push(InfoWrapEvent::OpenPeerQr(peer.id)),
            "emoji_userpic" => events.push(InfoWrapEvent::OpenEmojiUserpic(peer.id)),
            "peer_reactions" => events.push(InfoWrapEvent::OpenPeerReactions(peer.id)),
            "verify_peers" => events.push(InfoWrapEvent::OpenVerifyPeers(peer.id)),
            "share_peer" => events.push(InfoWrapEvent::OpenShare {
                message_ids: Vec::new(),
                share_link: peer.shareable_link(),
            }),
            "auto_delete" => events.push(InfoWrapEvent::OpenAutoDelete(peer.id)),
            "mute_toggle" => {
                let muted = backend.toggle_notifications(peer.id);
                events.push(InfoWrapEvent::Toast(if muted {
                    strings::TOAST_NOTIFICATIONS_MUTED.to_owned()
                } else {
                    strings::TOAST_NOTIFICATIONS_ENABLED.to_owned()
                }));
            }
            "create_group" => events.push(InfoWrapEvent::OpenCreateGroup),
            "create_channel" => events.push(InfoWrapEvent::OpenCreateChannel),
            "export_chat_history" => events.push(InfoWrapEvent::ExportChatHistory(peer.id)),
            "import_chat_history" => events.push(InfoWrapEvent::ImportChatHistory(peer.id)),
            "community" => {
                if let Some(comm) = backend.community_for_peer(peer.id) {
                    self.push_section(InfoSection::Community { peer_id: comm.id });
                } else {
                    events.push(InfoWrapEvent::Toast(
                        "No linked community for this chat.".to_owned(),
                    ));
                }
            }
            "downloads" => self.push_section(InfoSection::Downloads { peer_id: peer.id }),
            "global_media" => self.open_global_media(SharedMediaTab::PhotosVideos),
            "suggest_birthday" => {
                self.suggest_birthday_draft = Some(SuggestBirthdayDraft::new(peer.id, &peer.title));
                self.report_photo_draft = None;
            }
            "report_profile_photo" => {
                self.report_photo_draft = Some(ReportProfilePhotoDraft::new(peer.id, &peer.title));
                self.suggest_birthday_draft = None;
            }
            "report" => events.push(InfoWrapEvent::OpenReport(peer.id)),
            "delete_leave" => events.push(InfoWrapEvent::OpenDelete {
                peer_id: peer.id,
                message_ids: Vec::new(),
            }),
            "language" => events.push(InfoWrapEvent::OpenLanguage),
            "about" => events.push(InfoWrapEvent::OpenAbout),
            "terms" => events.push(InfoWrapEvent::OpenTerms),
            _ => {}
        }
    }
}

fn build_top_bar_meta(
    section: InfoSection,
    peer: Option<&PeerProfile>,
) -> (String, Option<String>, bool, bool, Vec<MenuItem>) {
    match section {
        InfoSection::Community { .. } => {
            return (
                peer.map_or_else(|| "Community".to_owned(), |p| p.title.clone()),
                None,
                false,
                false,
                Vec::new(),
            );
        }
        InfoSection::CommunityRequests { .. } => {
            return (
                "Pending Link Requests".to_owned(),
                peer.map(|p| p.title.clone()),
                false,
                false,
                Vec::new(),
            );
        }
        _ => {}
    }
    let Some(peer) = peer else {
        return (
            strings::FALLBACK_INFO_HEADING.to_owned(),
            None,
            false,
            false,
            Vec::new(),
        );
    };

    match section {
        InfoSection::Profile { .. } => {
            let mut items = Vec::new();
            if peer.can_edit {
                if peer.kind == InfoPeerKind::Bot {
                    items.push(
                        MenuItem::action("edit_bot_info", "Edit Bot Info")
                            .with_icon(IconKind::Edit),
                    );
                } else {
                    items.push(
                        MenuItem::action("edit_peer", strings::MENU_EDIT_INFO)
                            .with_icon(IconKind::Edit),
                    );
                }
            }
            if matches!(
                peer.kind,
                InfoPeerKind::User | InfoPeerKind::SavedMessages | InfoPeerKind::Channel
            ) && peer.can_edit
            {
                items.push(
                    MenuItem::action("edit_username", strings::MENU_EDIT_USERNAME)
                        .with_icon(IconKind::Edit),
                );
            }
            items.push(
                MenuItem::action("share_peer", strings::MENU_SHARE).with_icon(IconKind::Send),
            );
            if peer.kind != InfoPeerKind::SavedMessages {
                items.push(MenuItem::Toggle {
                    id: "mute_toggle".to_owned(),
                    label: strings::MENU_MUTE_NOTIFICATIONS.to_owned(),
                    checked: peer.muted,
                    icon: Some(IconKind::Settings),
                });
                items.push(
                    MenuItem::action("auto_delete", tr::lng_manage_messages_ttl_title())
                        .with_icon(IconKind::Lock),
                );
            }
            items.push(MenuItem::Separator);
            items.push(
                MenuItem::action("create_group", tr::lng_create_group_title())
                    .with_icon(IconKind::Plus),
            );
            items.push(
                MenuItem::action("create_channel", tr::lng_create_channel_title())
                    .with_icon(IconKind::Plus),
            );
            items.push(
                MenuItem::action("export_chat_history", "Export chat history")
                    .with_icon(IconKind::Settings),
            );
            items.push(
                MenuItem::action("import_chat_history", "Import Chat History")
                    .with_icon(IconKind::Attach),
            );
            items.push(
                MenuItem::action("downloads", tr::lng_downloads_section())
                    .with_icon(IconKind::Settings),
            );
            items
                .push(MenuItem::action("global_media", "Global Media").with_icon(IconKind::Search));
            items.push(
                MenuItem::action("language", tr::lng_languages()).with_icon(IconKind::Settings),
            );
            items.push(
                MenuItem::action("about", strings::MENU_ABOUT_TGFORK).with_icon(IconKind::Info),
            );
            items.push(MenuItem::action("terms", tr::lng_terms_header()).with_icon(IconKind::Info));
            if peer.kind != InfoPeerKind::SavedMessages {
                items.push(MenuItem::Separator);
                items.push(
                    MenuItem::action("report", tr::lng_profile_report())
                        .with_icon(IconKind::Info)
                        .attention(),
                );
                items.push(
                    MenuItem::action(
                        "delete_leave",
                        if peer.kind.is_group() {
                            tr::lng_profile_leave_group()
                        } else if peer.kind.is_channel() {
                            tr::lng_profile_leave_channel()
                        } else {
                            tr::lng_profile_delete_conversation()
                        },
                    )
                    .with_icon(IconKind::Trash)
                    .attention(),
                );
            }
            (
                peer.kind.localized_heading(),
                None,
                peer.kind.is_group(),
                peer.can_edit,
                items,
            )
        }
        InfoSection::SharedMedia { tab, .. } => (
            tab.localized_section_title(),
            Some(peer.title.clone()),
            true,
            false,
            Vec::new(),
        ),
        InfoSection::Members { .. } => (
            tr::lng_profile_participants_section(),
            Some(peer.status.clone()),
            true,
            false,
            Vec::new(),
        ),
        InfoSection::CommonGroups { .. } => (
            tr::lng_profile_common_groups_section(),
            Some(peer.title.clone()),
            false,
            false,
            Vec::new(),
        ),
        InfoSection::SimilarChannels { .. } => (
            tr::lng_similar_channels_title(),
            Some(peer.title.clone()),
            false,
            false,
            Vec::new(),
        ),
        InfoSection::Downloads { .. } => {
            (tr::lng_downloads_section(), None, true, false, Vec::new())
        }
        InfoSection::GlobalMedia { tab, .. } => (
            "Global Media".to_owned(),
            Some(tab.localized_section_title()),
            true,
            false,
            Vec::new(),
        ),
        InfoSection::Stories { archive, .. } => (
            if archive {
                "Stories Archive".to_owned()
            } else {
                "Stories".to_owned()
            },
            Some(peer.title.clone()),
            false,
            false,
            Vec::new(),
        ),
        InfoSection::PeerGifts { .. } => (
            "Gifts".to_owned(),
            Some(peer.title.clone()),
            false,
            false,
            Vec::new(),
        ),
        InfoSection::SavedMusic { .. } => (
            "Saved Music".to_owned(),
            Some(peer.title.clone()),
            false,
            false,
            Vec::new(),
        ),
        InfoSection::Community { .. } | InfoSection::CommunityRequests { .. } => {
            ("Community".to_owned(), None, false, false, Vec::new())
        }
    }
}
