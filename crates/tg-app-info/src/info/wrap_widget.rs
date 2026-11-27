//! Stateful Info/Profile container with sub-section stack, top bar, profile cover,
//! details/actions, members list, common groups, and shared media tabs.
//!
//! Port origin: `Telegram/SourceFiles/info/info_wrap_widget.h`,
//! `Telegram/SourceFiles/info/info_wrap_widget.cpp`,
//! `Telegram/SourceFiles/info/info_controller.h`,
//! and `Telegram/SourceFiles/info/profile/info_profile_widget.cpp`.

use std::collections::HashSet;

use egui::{CornerRadius, Rect, ScrollArea, Ui, Vec2};
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
use crate::info::cover::{CoverAction, CoverButton, ProfileCover, cover_buttons};
use crate::info::downloads::{DownloadsAction, DownloadsSection};
use crate::info::global_media::{GLOBAL_MEDIA_PAGE_SIZE, GlobalMediaAction, GlobalMediaSection};
use crate::info::media::{MediaListAction, MediaListWidget, MediaProvider};
use crate::info::members::{CommonGroupsSection, MembersAction, MembersSection};
use crate::info::mute_menu::{MuteMenuCommand, ProfileMuteMenu};
use crate::info::profile_controls::{
    ContactNoteDraft, ContactNoteOutcome, ContactPhotoDraft, ContactPhotoOutcome,
    DeleteContactDraft, DeleteContactOutcome, DeleteOwnPhotoDraft, DeleteOwnPhotoOutcome,
    PersonalChannelAction, PersonalChannelSection, ReportProfilePhotoDraft,
    ReportProfilePhotoOutcome, SuggestBirthdayDraft, SuggestBirthdayOutcome,
    can_set_main_profile_tab, is_valid_profile_photo_ref, ordered_shared_media_tabs,
    profile_main_tab_label, show_contact_note_card, show_contact_photo_card,
    show_delete_contact_card, show_delete_own_photo_card, show_main_profile_tab_bar,
    show_report_profile_photo_card, show_suggest_birthday_card,
};
use crate::info::profile_tabs::{
    PeerGiftsAction, PeerGiftsSection, ProfileStoriesAction, ProfileStoriesSection,
    SavedMusicAction, SavedMusicSection,
};
use crate::info::similar_channels::{SimilarChannelsAction, SimilarChannelsSection};
use crate::info::strings;
use crate::info::top_bar::{INFO_TOP_BAR_HEIGHT, InfoTopBar, TopBarAction, TopBarConfig};
use tg_app_core::chat_helpers::backend::AttachedStickersMedia;
use tg_app_core::core_types::dto::contacts_extras::ContactProfilePhotoMode;
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
    /// Start an outgoing 1:1 call from the profile Call button
    /// (`Core::App().calls().startOutgoingCall`).
    StartCall {
        /// User to call.
        peer_id: PeerId,
        /// `true` for a video call.
        video: bool,
    },
    /// Open the Send Gift box for this peer from the profile Gift button (`Ui::ShowStarGiftBox`).
    OpenSendGift(PeerId),
    /// Open the chat history for `PeerId`.
    OpenHistory(PeerId),
    /// Show an informational toast message.
    Toast(String),
    /// The Mute button (or its menu's "Mute forever / Unmute") toggled the chat's
    /// notifications. tdesktop only flips the button (`MuteMenu::ToggleMuteForever`); no toast.
    NotificationsToggled {
        /// The chat.
        peer_id: PeerId,
        /// Whether it is muted now.
        muted: bool,
    },
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
    /// Open the "Block {name}" box (`Window::PeerMenuBlockUserBox`).
    OpenBlockUser {
        /// The user to block.
        peer_id: PeerId,
        /// The user's short name.
        name: String,
    },
    /// Unblock a bot and send it /start (`PeerMenuUnblockUserWithBotRestart`), then open its chat.
    RestartBot(PeerId),
    /// Open "Mute notifications for..." for a chat (`PickMuteBox`).
    OpenMuteFor(PeerId),
    /// Start or join the group's video chat / the channel's live stream
    /// (`SessionController::startOrJoinGroupCall`).
    JoinGroupCall(PeerId),
    /// Open "Notification Sound" for a chat (`RingtonesBox`).
    OpenSelectTone(PeerId),
    /// Open the "Clear history" confirmation box (`ClearHistoryHandler`).
    OpenClearHistory(PeerId),
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
    /// Open Attached Stickers modal for a photo/document (`messages.getAttachedStickers`).
    OpenAttachedStickers {
        /// Target media selector.
        media: AttachedStickersMedia,
        /// Human-readable media label (e.g. `"Mira Vance's Profile Photo"`).
        media_label: String,
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
    contact_note_draft: Option<ContactNoteDraft>,
    delete_contact_draft: Option<DeleteContactDraft>,
    delete_own_photo_draft: Option<DeleteOwnPhotoDraft>,
    contact_photo_draft: Option<ContactPhotoDraft>,
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
    /// Optional explicit real [`ProfilePhotoRef`] override for tests (until WP-71 lands
    /// `InfoBackend::active_profile_photo_ref` on `master`).
    active_photo_ref_override: Option<(PeerId, ProfilePhotoRef)>,
    /// The backend's answer for the profile last shown, so the cover (painted every frame)
    /// asks the backend once per peer; the live backend may need an RPC to answer.
    active_photo_ref_cache: std::cell::RefCell<Option<(PeerId, Option<ProfilePhotoRef>)>>,
    /// The Mute button's right-click menu (`MuteMenu::SetupMuteMenu`).
    mute_menu: ProfileMuteMenu,
    /// Whether the shown peer's notification sound is off (`NotifySound::none`), as the host
    /// last reported it with [`Self::set_notify_sound_silent`].
    notify_silent: Option<(PeerId, bool)>,
    /// "Disable sound" / "Enable sound" picked in the mute menu, for the host to apply.
    sound_toggle_request: Option<(PeerId, bool)>,
    /// Shown as `Info::LayerWidget` (`Wrap::Layer`): the top bar has the x, and a back arrow
    /// only for a sub-section.
    layer_wrap: bool,
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
            contact_note_draft: None,
            delete_contact_draft: None,
            delete_own_photo_draft: None,
            contact_photo_draft: None,
            show_inline_media: false,
            show_inline_members: true,
            birthday_effect: None,
            birthday_effect_peer: None,
            birthday_today_override: None,
            chat_effects_paused: false,
            active_photo_ref_override: None,
            active_photo_ref_cache: std::cell::RefCell::new(None),
            mute_menu: ProfileMuteMenu::default(),
            notify_silent: None,
            sound_toggle_request: None,
            layer_wrap: false,
        }
    }

    /// Shows the next frames as a layer over the window (`Wrap::Layer`) or as a column.
    pub fn set_layer_wrap(&mut self, layer: bool) {
        self.layer_wrap = layer;
    }

    /// Tells the profile whether `peer_id`'s notification sound is off, so the mute menu shows
    /// "Enable sound" instead of "Disable sound" (`FillMuteMenu`).
    pub fn set_notify_sound_silent(&mut self, peer_id: PeerId, silent: bool) {
        self.notify_silent = Some((peer_id, silent));
    }

    /// Takes the pending "Disable sound" / "Enable sound" request from the mute menu:
    /// `(peer, enable)`, to apply with `DialogsBackend::set_dialog_sound_enabled`.
    pub fn take_sound_toggle_request(&mut self) -> Option<(PeerId, bool)> {
        self.sound_toggle_request.take()
    }

    /// Whether the profile Mute button's menu is open.
    #[must_use]
    pub const fn is_mute_menu_open(&self) -> bool {
        self.mute_menu.is_open()
    }

    fn show_mute_menu(
        &mut self,
        ui: &Ui,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
        events: &mut Vec<InfoWrapEvent>,
    ) {
        let InfoSection::Profile { peer_id, .. } = self.current else {
            self.mute_menu.close();
            return;
        };
        if !self.mute_menu.is_open() {
            return;
        }
        let Some(peer) = backend.peer_profile(peer_id) else {
            self.mute_menu.close();
            return;
        };
        let silent = self
            .notify_silent
            .filter(|(pid, _)| *pid == peer_id)
            .is_some_and(|(_, silent)| silent);
        let Some(command) = self.mute_menu.show(
            ui.ctx(),
            ui.scope_id().with(("info_profile_mute_menu", peer_id.0)),
            peer.muted,
            silent,
            colors,
        ) else {
            return;
        };
        match command {
            MuteMenuCommand::SelectTone => events.push(InfoWrapEvent::OpenSelectTone(peer_id)),
            MuteMenuCommand::MuteFor => events.push(InfoWrapEvent::OpenMuteFor(peer_id)),
            MuteMenuCommand::SetSoundEnabled { enable } => {
                self.notify_silent = Some((peer_id, !enable));
                self.sound_toggle_request = Some((peer_id, enable));
            }
            MuteMenuCommand::ToggleMuteForever => self.handle_profile_action(
                ProfileAction::ToggleNotifications(peer_id),
                &peer,
                backend,
                events,
            ),
        }
    }

    /// Sets or clears an explicit active [`ProfilePhotoRef`] override for `peer_id` (used in tests).
    pub fn set_active_profile_photo_ref_override(
        &mut self,
        peer_id: PeerId,
        photo: Option<ProfilePhotoRef>,
    ) {
        self.active_photo_ref_override = photo.map(|p| (peer_id, p));
        self.active_photo_ref_cache.replace(None);
    }

    /// Resolves the real active [`ProfilePhotoRef`] for `peer_id`, rejecting placeholder refs
    /// (`id == 0` or `access_hash == 0`).
    ///
    /// Asks `InfoBackend::active_profile_photo_ref` once per peer and caches the answer; with no
    /// real ref, Delete / Report / Attached Stickers stay disabled and send nothing.
    fn resolve_active_profile_photo_ref(
        &self,
        backend: &dyn InfoBackend,
        peer_id: PeerId,
    ) -> Option<ProfilePhotoRef> {
        if let Some((pid, ref photo)) = self.active_photo_ref_override
            && pid == peer_id
        {
            return is_valid_profile_photo_ref(photo).then(|| photo.clone());
        }
        if let Some((pid, ref cached)) = *self.active_photo_ref_cache.borrow()
            && pid == peer_id
        {
            return cached.clone();
        }
        let resolved = backend
            .active_profile_photo_ref(peer_id)
            .filter(is_valid_profile_photo_ref);
        self.active_photo_ref_cache
            .replace(Some((peer_id, resolved.clone())));
        resolved
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

    /// Updates the active inline Contact Note draft text (used by headless UI tests).
    pub fn set_contact_note_draft_for_test(&mut self, text: impl Into<String>) {
        if let Some(ref mut draft) = self.contact_note_draft {
            draft.text = text.into();
        }
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
            self.contact_note_draft = None;
            self.delete_contact_draft = None;
            self.delete_own_photo_draft = None;
            self.contact_photo_draft = None;
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

        // 1. Top bar (`54px`); on the profile page it floats over the cover.
        let has_photo_ref = self
            .resolve_active_profile_photo_ref(&*backend, peer_id)
            .is_some();
        let (title, subtitle, show_search, show_edit, menu_items) =
            build_top_bar_meta(self.current, peer.as_ref(), has_photo_ref);
        // `WrapWidget::setupTopBarMenuToggle`: the profile's ⋮ menu only exists outside the
        // side column or when the Info stack has history.
        let menu_items = if matches!(self.current, InfoSection::Profile { .. })
            && in_third_column
            && self.back_stack.is_empty()
        {
            Vec::new()
        } else {
            with_tdesktop_menu_icons(menu_items)
        };
        let profile_overlay = matches!(self.current, InfoSection::Profile { .. })
            && self.selected_media_ids.is_empty()
            && !self.top_bar.is_search_open();
        let top_cfg = TopBarConfig {
            title: &title,
            subtitle: subtitle.as_deref(),
            show_back: !self.back_stack.is_empty() || (!in_third_column && !self.layer_wrap),
            show_close: in_third_column || self.layer_wrap,
            show_search: show_search && !profile_overlay,
            show_edit: show_edit && !profile_overlay,
            menu_items,
            selected_count: self.selected_media_ids.len(),
            overlay: profile_overlay,
        };
        let top_rect = Rect::from_min_size(
            ui.cursor().min,
            Vec2::new(ui.available_width().max(180.0), INFO_TOP_BAR_HEIGHT),
        );

        let top_act = if profile_overlay {
            None
        } else {
            self.top_bar.show(ui, "info_wrap_topbar", &top_cfg, colors)
        };
        if let Some(act) = top_act {
            self.handle_top_bar_action(act, peer_id, peer.as_ref(), backend, &mut events);
        }

        let mut events = self.show_body(ui, backend, in_third_column, colors, peer, events);
        self.show_mute_menu(ui, backend, colors, &mut events);

        if profile_overlay {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(top_rect));
            let peer = backend.peer_profile(peer_id);
            if let Some(act) = self
                .top_bar
                .show(&mut child, "info_wrap_topbar", &top_cfg, colors)
            {
                self.handle_top_bar_action(act, peer_id, peer.as_ref(), backend, &mut events);
            }
            return events;
        }
        events
    }

    fn handle_top_bar_action(
        &mut self,
        act: TopBarAction,
        peer_id: PeerId,
        peer: Option<&PeerProfile>,
        backend: &mut dyn InfoBackend,
        events: &mut Vec<InfoWrapEvent>,
    ) {
        {
            match act {
                TopBarAction::Back => {
                    if !self.go_back() {
                        events.push(InfoWrapEvent::CloseOrBack);
                    }
                }
                TopBarAction::Close => events.push(InfoWrapEvent::CloseOrBack),
                TopBarAction::Edit => {
                    if peer.is_some_and(|p| p.kind == InfoPeerKind::Bot) {
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
                    self.handle_menu_command(&cmd, peer, backend, events);
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &mut dyn InfoBackend,
        in_third_column: bool,
        colors: &dyn Colors,
        peer: Option<PeerProfile>,
        mut events: Vec<InfoWrapEvent>,
    ) -> Vec<InfoWrapEvent> {
        let peer_id = self.current.peer_id();

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
                    // The cover and the `VerticalBlocks` under it: no egui spacing between rows,
                    // only tdesktop's skips (`infoProfileSkip`).
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let profile_balance = backend.peer_profile_balance(peer_id);
                    let active_photo_ref =
                        self.resolve_active_profile_photo_ref(&*backend, peer_id);
                    let buttons = cover_buttons(&peer, in_third_column);
                    if let Some(cover_act) = ProfileCover::show_with_buttons(
                        ui,
                        &peer,
                        colors,
                        self.birthday_effect.as_mut(),
                        profile_balance.as_ref(),
                        active_photo_ref.is_some(),
                        &buttons,
                    ) {
                        match cover_act {
                            CoverAction::Button(button) => {
                                let act = match button {
                                    CoverButton::Message => {
                                        Some(ProfileAction::SendMessage(peer_id))
                                    }
                                    CoverButton::Mute => {
                                        Some(ProfileAction::ToggleNotifications(peer_id))
                                    }
                                    CoverButton::Call => {
                                        // `Core::App().calls().startOutgoingCall(user, {})`.
                                        events.push(InfoWrapEvent::StartCall {
                                            peer_id,
                                            video: false,
                                        });
                                        None
                                    }
                                    CoverButton::Manage => Some(ProfileAction::OpenEditPeer),
                                    CoverButton::VideoChat { .. } => {
                                        // `controller->startOrJoinGroupCall(peer)`.
                                        events.push(InfoWrapEvent::JoinGroupCall(peer_id));
                                        None
                                    }
                                    CoverButton::Gift => {
                                        // `Ui::ShowStarGiftBox(controller, peer)`.
                                        events.push(InfoWrapEvent::OpenSendGift(peer_id));
                                        None
                                    }
                                    CoverButton::Leave => Some(ProfileAction::OpenDeleteOrLeave),
                                };
                                if let Some(act) = act {
                                    self.handle_profile_action(act, &peer, backend, &mut events);
                                }
                            }
                            CoverAction::MuteMenu { anchor } => {
                                self.mute_menu.open_at(ui.ctx(), anchor, colors);
                            }
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
                                self.clear_inline_drafts();
                                if let Some(photo) = active_photo_ref.clone() {
                                    self.report_photo_draft = Some(ReportProfilePhotoDraft::new(
                                        peer_id,
                                        &peer.title,
                                        photo,
                                    ));
                                } else {
                                    events.push(InfoWrapEvent::Toast(
                                        "Profile photo metadata is unavailable.".to_owned(),
                                    ));
                                }
                            }
                            CoverAction::DeleteOwnProfilePhoto => {
                                self.clear_inline_drafts();
                                if let Some(photo) = active_photo_ref.clone() {
                                    self.delete_own_photo_draft =
                                        Some(DeleteOwnPhotoDraft { photo, error: None });
                                } else {
                                    events.push(InfoWrapEvent::Toast(
                                        "Profile photo metadata is unavailable.".to_owned(),
                                    ));
                                }
                            }
                            CoverAction::SuggestContactPhoto => {
                                self.clear_inline_drafts();
                                self.contact_photo_draft = Some(ContactPhotoDraft::new(
                                    peer_id,
                                    &peer.title,
                                    ContactProfilePhotoMode::Suggest,
                                ));
                            }
                            CoverAction::ViewAttachedStickers => {
                                if let Some(photo) = active_photo_ref.clone() {
                                    events.push(InfoWrapEvent::OpenAttachedStickers {
                                        media: AttachedStickersMedia::Photo {
                                            id: photo.id,
                                            access_hash: photo.access_hash,
                                            file_reference: photo.file_reference,
                                        },
                                        media_label: format!("{}'s Profile Photo", peer.title),
                                    });
                                } else {
                                    events.push(InfoWrapEvent::Toast(
                                        "Profile photo metadata is unavailable.".to_owned(),
                                    ));
                                }
                            }
                        }
                    }

                    // Cached private Contact Note summary row (`info_profile_actions.cpp:804-895`)
                    if peer.kind == InfoPeerKind::User
                        && let Some(note) = backend.contact_note(peer_id)
                        && !note.text.trim().is_empty()
                    {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(14, 4))
                            .show(ui, |ui| {
                                egui::Frame::NONE
                                    .fill(colors.color("windowBgOver"))
                                    .corner_radius(CornerRadius::same(8))
                                    .inner_margin(egui::Margin::same(10))
                                    .show(ui, |ui| {
                                        ui.set_min_width((ui.available_width() - 4.0).max(160.0));
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new("Note · Private")
                                                    .font(egui::FontId::new(
                                                        11.5,
                                                        tg_ui::fonts::family_semibold(),
                                                    ))
                                                    .color(colors.color("windowActiveTextFg")),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if ui.small_button("Edit Note").clicked() {
                                                        self.clear_inline_drafts();
                                                        self.contact_note_draft =
                                                            Some(ContactNoteDraft::new(
                                                                peer_id,
                                                                &peer.title,
                                                                Some(&note.text),
                                                            ));
                                                    }
                                                },
                                            );
                                        });
                                        ui.add_space(2.0);
                                        ui.label(
                                            egui::RichText::new(&note.text)
                                                .font(font_id_from_style(st::normal_font))
                                                .color(colors.color("windowFg")),
                                        );
                                    });
                            });
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
                                if !is_valid_profile_photo_ref(&draft.photo) {
                                    events.push(InfoWrapEvent::Toast(
                                        "Profile photo metadata is unavailable.".to_owned(),
                                    ));
                                } else {
                                    match backend.report_profile_photo(
                                        draft.peer_id,
                                        draft.photo.clone(),
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
                            }
                            Some(ReportProfilePhotoOutcome::Cancel) => {}
                            None => self.report_photo_draft = Some(draft),
                        }
                    }

                    // Inline Contact Note editor (`contacts.updateContactNote`)
                    if let Some(mut draft) = self.contact_note_draft.take() {
                        match show_contact_note_card(ui, &mut draft, colors) {
                            Some(ContactNoteOutcome::Save(text)) => {
                                let payload = tg_text::TextWithEntities::simple(text.clone());
                                match backend.update_contact_note(draft.peer_id, payload) {
                                    Ok(_) => {
                                        events.push(InfoWrapEvent::Toast(if text.is_empty() {
                                            "Contact note deleted".to_owned()
                                        } else {
                                            "Contact note saved".to_owned()
                                        }))
                                    }
                                    Err(err) => {
                                        draft.error = Some(err);
                                        self.contact_note_draft = Some(draft);
                                    }
                                }
                            }
                            Some(ContactNoteOutcome::Delete) => {
                                match backend.update_contact_note(
                                    draft.peer_id,
                                    tg_text::TextWithEntities::default(),
                                ) {
                                    Ok(_) => events.push(InfoWrapEvent::Toast(
                                        "Contact note deleted".to_owned(),
                                    )),
                                    Err(err) => {
                                        draft.error = Some(err);
                                        self.contact_note_draft = Some(draft);
                                    }
                                }
                            }
                            Some(ContactNoteOutcome::Cancel) => {}
                            None => self.contact_note_draft = Some(draft),
                        }
                    }

                    // Inline Delete Contact confirmation (`contacts.deleteContacts`)
                    if let Some(mut draft) = self.delete_contact_draft.take() {
                        match show_delete_contact_card(ui, &draft, colors) {
                            Some(DeleteContactOutcome::Confirm) => {
                                match backend.delete_contact(draft.peer_id) {
                                    Ok(()) => events.push(InfoWrapEvent::Toast(format!(
                                        "{} removed from contacts",
                                        draft.peer_title
                                    ))),
                                    Err(err) => {
                                        draft.error = Some(err);
                                        self.delete_contact_draft = Some(draft);
                                    }
                                }
                            }
                            Some(DeleteContactOutcome::Cancel) => {}
                            None => self.delete_contact_draft = Some(draft),
                        }
                    }

                    // Inline Delete Own Profile Photo confirmation (`photos.deletePhotos`)
                    if let Some(mut draft) = self.delete_own_photo_draft.take() {
                        match show_delete_own_photo_card(ui, &draft, colors) {
                            Some(DeleteOwnPhotoOutcome::Confirm) => {
                                if !is_valid_profile_photo_ref(&draft.photo) {
                                    events.push(InfoWrapEvent::Toast(
                                        "Profile photo metadata is unavailable.".to_owned(),
                                    ));
                                } else {
                                    match backend.delete_profile_photos(&[draft.photo.clone()]) {
                                        Ok(_) => events.push(InfoWrapEvent::Toast(
                                            "Profile photo deleted".to_owned(),
                                        )),
                                        Err(err) => {
                                            draft.error = Some(err);
                                            self.delete_own_photo_draft = Some(draft);
                                        }
                                    }
                                }
                            }
                            Some(DeleteOwnPhotoOutcome::Cancel) => {}
                            None => self.delete_own_photo_draft = Some(draft),
                        }
                    }

                    // Inline Suggest / Set Personal Contact Photo card (`photos.uploadContactProfilePhoto`)
                    if let Some(mut draft) = self.contact_photo_draft.take() {
                        match show_contact_photo_card(ui, &mut draft, colors) {
                            Some(ContactPhotoOutcome::Upload(req)) => {
                                let is_suggest = req.mode == ContactProfilePhotoMode::Suggest;
                                match backend.upload_contact_profile_photo(draft.peer_id, req) {
                                    Ok(_) => events.push(InfoWrapEvent::Toast(if is_suggest {
                                        format!("Suggested profile photo to {}", draft.peer_title)
                                    } else {
                                        format!(
                                            "Personal profile photo set for {}",
                                            draft.peer_title
                                        )
                                    })),
                                    Err(err) => {
                                        draft.error = Some(err);
                                        self.contact_photo_draft = Some(draft);
                                    }
                                }
                            }
                            Some(ContactPhotoOutcome::ClearPersonal) => {
                                match backend.clear_contact_profile_photo(draft.peer_id) {
                                    Ok(_) => events.push(InfoWrapEvent::Toast(format!(
                                        "Personal photo cleared for {}",
                                        draft.peer_title
                                    ))),
                                    Err(err) => {
                                        draft.error = Some(err);
                                        self.contact_photo_draft = Some(draft);
                                    }
                                }
                            }
                            Some(ContactPhotoOutcome::Cancel) => {}
                            None => self.contact_photo_draft = Some(draft),
                        }
                    }

                    let blocks_top = ui.cursor().min.y;
                    if let Some(act) = ProfileActionsView::show_with_today_and_photo_ref(
                        ui,
                        &peer,
                        in_third_column,
                        (today_year, today_month, today_day),
                        active_photo_ref.is_some(),
                        colors,
                    ) {
                        self.handle_profile_action(act, &peer, backend, &mut events);
                    }

                    // `SetupSharedMediaClassic` (side column): right after the details block.
                    if !self.show_inline_media {
                        let mut media_act = None;
                        let any = peer.media_counts.photos_videos
                            + peer.media_counts.files
                            + peer.media_counts.links
                            + peer.media_counts.voice
                            + peer.media_counts.music
                            + peer.media_counts.gifs
                            > 0
                            || (peer.media_counts.common_groups > 0
                                && matches!(peer.kind, InfoPeerKind::User | InfoPeerKind::Bot));
                        if any {
                            block_separator(ui, blocks_top, colors);
                            ProfileActionsView::show_media_counts(
                                ui,
                                &peer,
                                colors,
                                &mut media_act,
                            );
                        }
                        if let Some(act) = media_act {
                            self.handle_profile_action(act, &peer, backend, &mut events);
                        }
                    }

                    // Inline members list on group profiles (`InnerWidget::makeMembersPart`).
                    if self.show_inline_members && peer.kind.is_group() {
                        block_separator(ui, blocks_top, colors);
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
                        let thumbs_backend: &dyn InfoBackend = &*backend;
                        if let Some(m_ev) = self.media_list.show_items_with_thumbs(
                            ui,
                            media_tab,
                            &items,
                            &self.selected_media_ids,
                            &|msg_id| thumbs_backend.shared_media_thumb(peer_id, msg_id),
                            colors,
                        ) {
                            self.handle_media_event(m_ev, &mut events);
                        }
                    }

                    if let Some(act) = ProfileActionsView::show_footer_actions_with_photo_ref(
                        ui,
                        &peer,
                        active_photo_ref.is_some(),
                        colors,
                    ) {
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
                    let thumbs_backend: &dyn InfoBackend = &*backend;
                    if let Some(m_ev) = self.media_list.show_items_with_thumbs(
                        ui,
                        tab,
                        &items,
                        &self.selected_media_ids,
                        &|msg_id| thumbs_backend.shared_media_thumb(peer_id, msg_id),
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
                events.push(InfoWrapEvent::NotificationsToggled {
                    peer_id: pid,
                    muted,
                });
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
            // `ActionsFiller::addBlockAction`: unblocking a bot restarts it, blocking a bot needs
            // no confirmation, blocking a user asks with `PeerMenuBlockUserBox`.
            ProfileAction::ToggleBlock(pid) => {
                let bot = peer.kind == InfoPeerKind::Bot;
                if peer.is_blocked && bot {
                    events.push(InfoWrapEvent::RestartBot(pid));
                } else if peer.is_blocked || bot {
                    let blocked = backend.toggle_block_user(pid);
                    events.push(InfoWrapEvent::Toast(if blocked {
                        tr::lng_new_contact_block_done(&peer.title)
                    } else {
                        format!("{} unblocked", peer.title)
                    }));
                } else {
                    events.push(InfoWrapEvent::OpenBlockUser {
                        peer_id: pid,
                        name: short_name(&peer.title),
                    });
                }
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
                self.clear_inline_drafts();
                self.suggest_birthday_draft = Some(SuggestBirthdayDraft::new(peer.id, &peer.title));
            }
            ProfileAction::ReportProfilePhoto => {
                self.clear_inline_drafts();
                if let Some(photo) = self.resolve_active_profile_photo_ref(&*backend, peer.id) {
                    self.report_photo_draft =
                        Some(ReportProfilePhotoDraft::new(peer.id, &peer.title, photo));
                } else {
                    events.push(InfoWrapEvent::Toast(
                        "Profile photo metadata is unavailable.".to_owned(),
                    ));
                }
            }
            ProfileAction::EditContactNote => {
                self.clear_inline_drafts();
                let existing = backend.contact_note(peer.id);
                self.contact_note_draft = Some(ContactNoteDraft::new(
                    peer.id,
                    &peer.title,
                    existing.as_ref().map(|n| n.text.as_str()),
                ));
            }
            ProfileAction::DeleteContact => {
                self.clear_inline_drafts();
                self.delete_contact_draft = Some(DeleteContactDraft {
                    peer_id: peer.id,
                    peer_title: peer.title.clone(),
                    error: None,
                });
            }
            ProfileAction::DeleteOwnProfilePhoto => {
                self.clear_inline_drafts();
                if let Some(photo) = self.resolve_active_profile_photo_ref(&*backend, peer.id) {
                    self.delete_own_photo_draft = Some(DeleteOwnPhotoDraft { photo, error: None });
                } else {
                    events.push(InfoWrapEvent::Toast(
                        "Profile photo metadata is unavailable.".to_owned(),
                    ));
                }
            }
            ProfileAction::SuggestContactPhoto => {
                self.clear_inline_drafts();
                self.contact_photo_draft = Some(ContactPhotoDraft::new(
                    peer.id,
                    &peer.title,
                    ContactProfilePhotoMode::Suggest,
                ));
            }
            ProfileAction::ViewAttachedStickers => {
                if let Some(photo) = self.resolve_active_profile_photo_ref(&*backend, peer.id) {
                    events.push(InfoWrapEvent::OpenAttachedStickers {
                        media: AttachedStickersMedia::Photo {
                            id: photo.id,
                            access_hash: photo.access_hash,
                            file_reference: photo.file_reference,
                        },
                        media_label: format!("{}'s Profile Photo", peer.title),
                    });
                } else {
                    events.push(InfoWrapEvent::Toast(
                        "Profile photo metadata is unavailable.".to_owned(),
                    ));
                }
            }
        }
    }

    fn clear_inline_drafts(&mut self) {
        self.suggest_birthday_draft = None;
        self.report_photo_draft = None;
        self.contact_note_draft = None;
        self.delete_contact_draft = None;
        self.delete_own_photo_draft = None;
        self.contact_photo_draft = None;
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
                events.push(InfoWrapEvent::NotificationsToggled {
                    peer_id: peer.id,
                    muted,
                });
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
                self.suggest_birthday_draft = None;
                if let Some(photo) = self.resolve_active_profile_photo_ref(&*backend, peer.id) {
                    self.report_photo_draft =
                        Some(ReportProfilePhotoDraft::new(peer.id, &peer.title, photo));
                } else {
                    events.push(InfoWrapEvent::Toast(
                        "Profile photo metadata is unavailable.".to_owned(),
                    ));
                }
            }
            "report" => events.push(InfoWrapEvent::OpenReport(peer.id)),
            "profile:stories" => {
                self.handle_profile_action(ProfileAction::OpenStories, peer, backend, events);
            }
            "profile:gifts" => {
                self.handle_profile_action(ProfileAction::OpenPeerGifts, peer, backend, events);
            }
            "profile:saved_music" => {
                self.handle_profile_action(ProfileAction::OpenSavedMusic, peer, backend, events);
            }
            "profile:contact_note" => {
                self.handle_profile_action(ProfileAction::EditContactNote, peer, backend, events);
            }
            "profile:suggest_photo" => self.handle_profile_action(
                ProfileAction::SuggestContactPhoto,
                peer,
                backend,
                events,
            ),
            "profile:delete_contact" => {
                self.handle_profile_action(ProfileAction::DeleteContact, peer, backend, events);
            }
            "profile:delete_own_photo" => self.handle_profile_action(
                ProfileAction::DeleteOwnProfilePhoto,
                peer,
                backend,
                events,
            ),
            "profile:open_app" => self.handle_profile_action(
                ProfileAction::OpenBotMiniApp(peer.id),
                peer,
                backend,
                events,
            ),
            "profile:statistics" => self.handle_profile_action(
                ProfileAction::OpenStatistics(peer.id),
                peer,
                backend,
                events,
            ),
            "profile:boosts" => self.handle_profile_action(
                ProfileAction::OpenBoosts(peer.id),
                peer,
                backend,
                events,
            ),
            "profile:permissions" => {
                self.handle_profile_action(ProfileAction::OpenPermissions, peer, backend, events);
            }
            "profile:admins" => {
                self.handle_profile_action(ProfileAction::OpenAdministrators, peer, backend, events)
            }
            "profile:similar" => self.handle_profile_action(
                ProfileAction::OpenSimilarChannels,
                peer,
                backend,
                events,
            ),
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

/// tdesktop `menu_icons.style` icon for a profile ⋮ menu action id (`window_peer_menu.cpp`).
fn profile_menu_style_icon(id: &str) -> Option<tg_ui::tg_style::Icon> {
    use tg_ui::tg_style::st;
    Some(match id {
        "edit_peer" | "edit_bot_info" | "profile:contact_note" => st::menu_icon_edit,
        "edit_username" => st::menu_icon_username,
        "share_peer" => st::menu_icon_share,
        "auto_delete" => st::menu_icon_timer,
        "create_group" => st::menu_icon_groups,
        "create_channel" => st::menu_icon_channel,
        "downloads" => st::menu_icon_download,
        "export_chat_history" => st::menu_icon_export,
        "import_chat_history" => st::menu_icon_import_theme,
        "global_media" => st::menu_icon_search,
        "language" => st::menu_icon_translate,
        "about" | "terms" | "profile:similar" => st::menu_icon_info,
        "qr_code" => st::menu_icon_qr_code,
        "emoji_userpic" => st::menu_icon_emoji,
        "profile:stories" => st::menu_icon_stories_saved_section,
        "profile:gifts" => st::menu_icon_gift_premium,
        "profile:saved_music" => st::menu_icon_file,
        "profile:suggest_photo" => st::menu_icon_photo_suggest,
        "suggest_birthday" => st::menu_icon_photo_set,
        "profile:open_app" => st::menu_icon_bot,
        "verify_peers" => st::menu_icon_signed,
        "profile:permissions" => st::menu_icon_permissions,
        "profile:admins" => st::menu_icon_admin,
        "profile:statistics" => st::menu_icon_stats,
        "profile:boosts" => st::menu_icon_boosts,
        "report" | "report_profile_photo" => st::menu_icon_report,
        "profile:delete_contact" | "profile:delete_own_photo" | "delete_leave" => {
            st::menu_icon_delete
        }
        _ => return None,
    })
}

/// Swaps the generic [`IconKind`] glyphs of the profile ⋮ menu for tdesktop's vector menu icons.
fn with_tdesktop_menu_icons(items: Vec<MenuItem>) -> Vec<MenuItem> {
    items
        .into_iter()
        .map(|item| {
            let icon = match &item {
                MenuItem::Action { id, .. } => profile_menu_style_icon(id),
                MenuItem::Toggle { .. } | MenuItem::Separator => None,
            };
            match icon {
                Some(icon) => item.with_style_icon(icon),
                None => item,
            }
        })
        .collect()
}

fn build_top_bar_meta(
    section: InfoSection,
    peer: Option<&PeerProfile>,
    has_photo_ref: bool,
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
            push_profile_extra_items(&mut items, peer, has_photo_ref);
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

/// Profile rows that the 7.4.6 page does not list (they live in tdesktop's peer menu),
/// kept reachable from the ⋮ menu under their previous labels.
fn push_profile_extra_items(items: &mut Vec<MenuItem>, peer: &PeerProfile, has_photo_ref: bool) {
    items.push(MenuItem::Separator);
    let user_like = matches!(peer.kind, InfoPeerKind::User | InfoPeerKind::SavedMessages);
    if user_like || peer.kind == InfoPeerKind::Channel {
        items.push(
            MenuItem::action("profile:stories", "Stories & Albums").with_icon(IconKind::Star),
        );
        items.push(MenuItem::action("profile:gifts", "Gifts").with_icon(IconKind::Star));
    }
    if user_like {
        items
            .push(MenuItem::action("profile:saved_music", "Saved Music").with_icon(IconKind::Star));
    }
    if peer.kind != InfoPeerKind::SavedMessages && peer.primary_username().is_none() {
        items.push(MenuItem::action("qr_code", "QR Code").with_icon(IconKind::Info));
    }
    if peer.can_edit {
        items.push(MenuItem::action("emoji_userpic", "Emoji Avatar").with_icon(IconKind::Edit));
    }
    if peer.kind == InfoPeerKind::User {
        items.push(
            MenuItem::action("suggest_birthday", tr::lng_suggest_birthday())
                .with_icon(IconKind::Star),
        );
        items.push(
            MenuItem::action("profile:contact_note", "Contact Note").with_icon(IconKind::Edit),
        );
        items.push(
            MenuItem::action("profile:suggest_photo", tr::lng_profile_suggest_photo())
                .with_icon(IconKind::Edit),
        );
        let mut report_photo = MenuItem::action("report_profile_photo", "Report Profile Photo")
            .with_icon(IconKind::Info);
        if !has_photo_ref {
            report_photo = report_photo.disabled();
        }
        items.push(report_photo);
        if peer.is_contact {
            items.push(
                MenuItem::action("profile:delete_contact", tr::lng_info_delete_contact())
                    .with_icon(IconKind::Trash)
                    .attention(),
            );
        }
    }
    if peer.kind == InfoPeerKind::SavedMessages {
        let mut delete_photo = MenuItem::action("profile:delete_own_photo", "Delete Profile Photo")
            .with_icon(IconKind::Trash)
            .attention();
        if !has_photo_ref {
            delete_photo = delete_photo.disabled();
        }
        items.push(delete_photo);
    }
    if peer.kind == InfoPeerKind::Bot {
        if peer.has_mini_app {
            items.push(
                MenuItem::action("profile:open_app", tr::lng_profile_open_app())
                    .with_icon(IconKind::Star),
            );
        }
        items.push(
            MenuItem::action("profile:statistics", tr::lng_bot_earn_title())
                .with_icon(IconKind::Info),
        );
        items.push(MenuItem::action("verify_peers", "Verify Peers").with_icon(IconKind::Check));
    }
    if peer.kind.is_group() || peer.kind.is_channel() {
        if peer.can_manage_permissions && peer.kind.is_group() {
            items.push(
                MenuItem::action("profile:permissions", tr::lng_manage_peer_permissions())
                    .with_icon(IconKind::Lock),
            );
        }
        if peer.can_manage_admins {
            items.push(
                MenuItem::action("profile:admins", tr::lng_manage_peer_administrators())
                    .with_icon(IconKind::Star),
            );
        }
        items.push(
            MenuItem::action("profile:statistics", tr::lng_stats_title()).with_icon(IconKind::Info),
        );
        items.push(
            MenuItem::action("profile:boosts", tr::lng_boosts_title()).with_icon(IconKind::Star),
        );
        if peer.kind.is_channel() {
            items.push(
                MenuItem::action("profile:similar", tr::lng_similar_channels_title())
                    .with_icon(IconKind::Info),
            );
        }
    }
}

/// `UserData::shortName()`: the first name (the title up to its first space).
/// `VerticalBlocks` separator before a profile block: `infoProfileSkip`, the divider bar and
/// `infoProfileSkip` again when a block is already shown above (the cursor moved past
/// `blocks_top`), otherwise only the leading `infoProfileSkip`.
fn block_separator(ui: &mut Ui, blocks_top: f32, colors: &dyn Colors) {
    let skip = crate::info::actions::INFO_SECTION_SKIP;
    if ui.cursor().min.y > blocks_top + 0.5 {
        ui.add_space(skip);
        divider_bar(ui, 8.0, colors);
    }
    ui.add_space(skip);
}

fn short_name(title: &str) -> String {
    title.split_whitespace().next().unwrap_or(title).to_owned()
}
