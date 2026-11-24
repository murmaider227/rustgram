//! Backend trait and domain models for the dialogs list, folders, archive, and search.
//!
//! Port origin: `Telegram/SourceFiles/dialogs/dialogs_entry.h`,
//! `Telegram/SourceFiles/dialogs/dialogs_main_list.h`,
//! `Telegram/SourceFiles/dialogs/dialogs_pinned_list.h`,
//! `Telegram/SourceFiles/data/data_chat_filters.h`,
//! and `Telegram/SourceFiles/dialogs/ui/chat_search_in.h`.

use tg_history::{DialogBadgesState, DialogChatTypeIcon, DialogSendState, LocalDateTime};

use crate::stand_in::types::{FilterId, MsgId, PeerId};

/// Peer classification used by dialogs rows, icons, and search filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogPeerKind {
    /// Direct human user chat.
    User,
    /// Telegram bot user.
    Bot,
    /// Basic group or supergroup.
    Group,
    /// Broadcast channel.
    Channel,
    /// Forum supergroup with topics.
    Forum,
}

impl DialogPeerKind {
    /// Maps this peer kind to the leading chat-type icon expected by `tg-history`.
    #[must_use]
    pub const fn chat_type_icon(self) -> DialogChatTypeIcon {
        match self {
            Self::User => DialogChatTypeIcon::None,
            Self::Bot => DialogChatTypeIcon::Bot,
            Self::Group => DialogChatTypeIcon::Group,
            Self::Channel => DialogChatTypeIcon::Channel,
            Self::Forum => DialogChatTypeIcon::Forum,
        }
    }
}

/// Built-in special conversation types with dedicated avatars and titles
/// (`dialogs_layout.cpp` `Flag::SavedMessages`, `Flag::RepliesMessages`, `Flag::VerifyCodes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SpecialRowKind {
    /// Ordinary user, group, channel, or bot conversation.
    #[default]
    None,
    /// Current user's "Saved Messages" cloud notes chat.
    SavedMessages,
    /// System "Replies" chat aggregating comment replies.
    Replies,
    /// System "Verification Codes" chat.
    VerificationCodes,
}

/// Send-action activity displayed in place of the preview text while a peer is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypingAction {
    /// "typing"
    Typing,
    /// "recording a voice message"
    RecordingVoice,
    /// "sending a photo"
    UploadingPhoto,
    /// "choosing a sticker"
    ChoosingSticker,
}

impl TypingAction {
    /// Returns the localized verb phrase for this action (`send_action_painter.cpp`).
    #[must_use]
    pub fn verb(self) -> String {
        match self {
            Self::Typing => tg_lang::tr::lng_typing(),
            Self::RecordingVoice => tg_lang::tr::lng_send_action_record_audio(),
            Self::UploadingPhoto => tg_lang::tr::lng_send_action_upload_photo(),
            Self::ChoosingSticker => tg_lang::tr::lng_send_action_choose_sticker(),
        }
    }
}

/// Live send-action state on a dialog row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypingState {
    /// Kind of live activity.
    pub action: TypingAction,
    /// Optional participant name in group chats (e.g. `"Sam"` -> `"Sam is typing"`).
    pub actor: Option<String>,
}

impl TypingState {
    /// Formats the localized base typing text without animated trailing dots.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.actor {
            Some(name) if !name.is_empty() => match self.action {
                TypingAction::Typing => tg_lang::tr::lng_user_typing(name),
                TypingAction::RecordingVoice => tg_lang::tr::lng_user_action_record_audio(name),
                TypingAction::UploadingPhoto => tg_lang::tr::lng_user_action_upload_photo(name),
                TypingAction::ChoosingSticker => tg_lang::tr::lng_user_action_choose_sticker(name),
            },
            _ => self.action.verb(),
        }
    }
}

/// Unsent draft attached to a conversation (`Data::Draft`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogDraft {
    /// Draft message body.
    pub text: String,
    /// Local timestamp of the draft edit.
    pub time: LocalDateTime,
    /// Whether the draft replies to a message.
    pub is_reply: bool,
    /// True if the draft is currently being synced to the server.
    pub saving: bool,
}

/// Compact folder tag chip rendered below the preview line (`ui/chat/chats_filter_tag.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderTagBadge {
    /// Folder identifier this tag corresponds to.
    pub filter_id: FilterId,
    /// Short folder label shown inside the pill.
    pub title: String,
    /// Palette accent index (`0..=6`, matching peer/folder color indices).
    pub color_index: u8,
}

/// Complete view model for a single row in the dialogs list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogItem {
    /// Unique peer identifier.
    pub peer_id: PeerId,
    /// Peer classification.
    pub kind: DialogPeerKind,
    /// Special system row marker (Saved Messages, Replies, etc.).
    pub special: SpecialRowKind,
    /// Display title of the peer or chat.
    pub title: String,
    /// Optional public `@username` without leading `@`.
    pub username: Option<String>,
    /// Peer color index (`0..7`) for avatar gradient.
    pub color_index: u8,
    /// Verified checkmark badge.
    pub verified: bool,
    /// Premium star badge.
    pub premium: bool,
    /// Online presence dot (users only).
    pub online: bool,
    /// Active group call badge (groups/channels).
    pub active_call: bool,
    /// Whether notifications are muted.
    pub muted: bool,
    /// Whether this chat is pinned in the currently queried list.
    pub pinned: bool,
    /// Position among pinned dialogs (`0` = top pinned), if pinned.
    pub pinned_order: Option<usize>,
    /// Whether this chat resides in the Archive folder.
    pub archived: bool,
    /// Folder filter IDs containing this dialog (excluding `FilterId::ALL`).
    pub folders: Vec<FilterId>,
    /// Optional folder tag pills rendered in `DialogRowStyle::TAGGED`.
    pub folder_tags: Vec<FolderTagBadge>,
    /// Latest message ID (`MsgId(0)` if empty/cleared).
    pub last_message_id: MsgId,
    /// Preview text of the latest message.
    pub last_message: String,
    /// Optional sender name prefix for group chats.
    pub last_sender: Option<String>,
    /// Local timestamp of the latest message.
    pub last_message_time: LocalDateTime,
    /// Delivery ticks / sending clock for outgoing last messages.
    pub send_state: DialogSendState,
    /// Optional unsent local/cloud draft.
    pub draft: Option<DialogDraft>,
    /// Optional live typing / recording indicator.
    pub typing: Option<TypingState>,
    /// Unread counter, unread mark, mention `@`, reaction heart, and poll badges.
    pub badges: DialogBadgesState,
    /// Whether this bot offers an inline "Open" right action button.
    pub has_bot_app_button: bool,
}

impl DialogItem {
    /// Returns the effective display timestamp (the newer of draft time and last message time),
    /// matching `dialogs_layout.cpp:1154-1158`.
    #[must_use]
    pub fn display_time(&self) -> LocalDateTime {
        if let Some(draft) = self.effective_draft()
            && draft.time.to_local_epoch_secs() > self.last_message_time.to_local_epoch_secs()
        {
            return draft.time;
        }
        self.last_message_time
    }

    /// Returns the draft only when it should be displayed (tdesktop suppresses draft display
    /// when there are unread messages in the dialog, `dialogs_layout.cpp:1143`).
    #[must_use]
    pub fn effective_draft(&self) -> Option<&DialogDraft> {
        let draft = self.draft.as_ref()?;
        if draft.text.is_empty() || self.badges.unread {
            None
        } else {
            Some(draft)
        }
    }

    /// Returns whether the pinned icon should be shown (only when badges are empty and pinned).
    #[must_use]
    pub const fn display_pinned_icon(&self) -> bool {
        self.pinned && self.badges.is_empty()
    }
}

/// Which dialog list is being queried (main folder, custom filter, or Archive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DialogListQuery {
    /// Main list or a specific chat folder (`FilterId::ALL` for All Chats).
    Filter(FilterId),
    /// Inside the opened Archive folder (`Data::Folder::kId = 1`).
    Archive,
}

/// Paginated slice returned by [`DialogsBackend::dialog_slice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogSlice {
    /// Dialogs in the requested `[offset, offset + limit)` window.
    pub entries: Vec<DialogItem>,
    /// Total count of dialogs in this list.
    pub total: usize,
    /// Number of pinned dialogs at the top of this list.
    pub pinned_count: usize,
    /// `true` if more dialogs exist beyond `offset + entries.len()`.
    pub has_more: bool,
}

/// Icon kind for a chat folder in the vertical sidebar (`ui/filter_icons.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FolderIconKind {
    /// All Chats icon.
    #[default]
    All,
    /// Unread messages folder.
    Unread,
    /// Unmuted chats folder.
    Unmuted,
    /// Personal / Private chats.
    Private,
    /// Groups folder.
    Groups,
    /// Broadcast channels folder.
    Channels,
    /// Bots folder.
    Bots,
    /// Work / briefcase folder.
    Work,
    /// Star / favorites folder.
    Star,
    /// Reading / book folder.
    Book,
    /// Games folder.
    Game,
    /// Cat folder.
    Cat,
    /// Money / finance folder.
    Money,
    /// Idea / lightbulb folder.
    Light,
    /// Like / thumbs-up folder.
    Like,
    /// Music / note folder.
    Note,
    /// Art / palette folder.
    Palette,
    /// Travel / globe folder.
    Travel,
    /// Sport folder.
    Sport,
    /// Study / education folder.
    Study,
    /// Airplane / flight folder.
    Airplane,
    /// Crown / VIP folder.
    Crown,
    /// Flower / nature folder.
    Flower,
    /// Home / family folder.
    Home,
    /// Love / heart folder.
    Love,
    /// Mask / theater folder.
    Mask,
    /// Party / celebration folder.
    Party,
    /// Trade / stocks folder.
    Trade,
    /// Setup / gear folder.
    Setup,
    /// Generic custom folder.
    Custom,
}

impl FolderIconKind {
    /// Compact 3-letter ASCII fallback label for text-only badges.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unread => "New",
            Self::Unmuted => "Snd",
            Self::Private => "Per",
            Self::Groups => "Grp",
            Self::Channels => "Chn",
            Self::Bots => "Bot",
            Self::Work => "Wrk",
            Self::Star => "Str",
            Self::Book => "Lib",
            Self::Game => "Fun",
            Self::Cat => "Cat",
            Self::Money => "Pay",
            Self::Light => "Ide",
            Self::Like => "Lik",
            Self::Note => "Mus",
            Self::Palette => "Art",
            Self::Travel => "Trv",
            Self::Sport => "Spt",
            Self::Study => "Edu",
            Self::Airplane => "Air",
            Self::Crown => "Vip",
            Self::Flower => "Flw",
            Self::Home => "Hom",
            Self::Love => "Lov",
            Self::Mask => "Msk",
            Self::Party => "Pty",
            Self::Trade => "Trd",
            Self::Setup => "Set",
            Self::Custom => "Fld",
        }
    }
}

impl From<crate::settings::backend::FolderIcon> for FolderIconKind {
    fn from(icon: crate::settings::backend::FolderIcon) -> Self {
        use crate::settings::backend::FolderIcon;
        match icon {
            FolderIcon::Custom => Self::Custom,
            FolderIcon::All => Self::All,
            FolderIcon::Unread => Self::Unread,
            FolderIcon::Unmuted => Self::Unmuted,
            FolderIcon::Bots => Self::Bots,
            FolderIcon::Channels => Self::Channels,
            FolderIcon::Groups => Self::Groups,
            FolderIcon::Private => Self::Private,
            FolderIcon::Work => Self::Work,
            FolderIcon::Star => Self::Star,
            FolderIcon::Book => Self::Book,
            FolderIcon::Game => Self::Game,
            FolderIcon::Cat => Self::Cat,
            FolderIcon::Money => Self::Money,
            FolderIcon::Light => Self::Light,
            FolderIcon::Like => Self::Like,
            FolderIcon::Note => Self::Note,
            FolderIcon::Palette => Self::Palette,
            FolderIcon::Travel => Self::Travel,
            FolderIcon::Sport => Self::Sport,
            FolderIcon::Study => Self::Study,
            FolderIcon::Airplane => Self::Airplane,
            FolderIcon::Crown => Self::Crown,
            FolderIcon::Flower => Self::Flower,
            FolderIcon::Home => Self::Home,
            FolderIcon::Love => Self::Love,
            FolderIcon::Mask => Self::Mask,
            FolderIcon::Party => Self::Party,
            FolderIcon::Trade => Self::Trade,
            FolderIcon::Setup => Self::Setup,
        }
    }
}

/// Unread counter state for a chat folder (`Dialogs::UnreadState`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderUnreadState {
    /// Total unread chats (or unread marks) in the folder.
    pub unread_chats: u32,
    /// How many of those unread chats are muted.
    pub muted_chats: u32,
}

impl FolderUnreadState {
    /// Returns the display count and whether the badge should use muted gray styling,
    /// matching `window_filters_menu.cpp:603-613`.
    #[must_use]
    pub const fn badge_display(self, include_muted: bool) -> (u32, bool) {
        let count = if include_muted {
            self.unread_chats
        } else {
            self.unread_chats.saturating_sub(self.muted_chats)
        };
        let muted = include_muted && count > 0 && count == self.muted_chats;
        (count, muted)
    }
}

/// Folder metadata shown in the vertical filters sidebar and horizontal tabs strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatFolder {
    /// Folder identifier (`FilterId::ALL` for "All Chats").
    pub id: FilterId,
    /// Display title (e.g. `"All Chats"`, `"Work"`).
    pub title: String,
    /// Vector icon kind.
    pub icon: FolderIconKind,
    /// Optional color index for folder tag chips (`0..7`).
    pub color_index: u8,
    /// Unread counts for this folder.
    pub unread: FolderUnreadState,
    /// Whether this folder is locked behind a Premium limit.
    pub locked: bool,
}

/// Summary metadata for the top-level Archive row (`Data::Folder`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSummary {
    /// Total number of archived chats.
    pub chat_count: usize,
    /// Unread chats/messages inside the Archive.
    pub unread_count: u32,
    /// Whether all unread items in the Archive are muted.
    pub all_muted: bool,
    /// Top archived chat titles shown in the expanded Archive preview line.
    pub preview_titles: Vec<String>,
    /// Whether the Archive row is currently collapsed to the 37px bar.
    pub collapsed: bool,
}

/// Active tab inside search mode (`Dialogs::ChatSearchTab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SearchTab {
    /// Default unified search (chats, global peers, messages).
    #[default]
    Chats,
    /// Channels & public posts search.
    Channels,
    /// Bots & mini apps search.
    Apps,
    /// Global public posts search.
    PublicPosts,
}

impl SearchTab {
    /// Localized display label for the search tab strip (`dialogs_widget.cpp`).
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Chats => tg_lang::tr::lng_recent_chats(),
            Self::Channels => tg_lang::tr::lng_recent_channels(),
            Self::Apps => tg_lang::tr::lng_recent_apps(),
            Self::PublicPosts => tg_lang::tr::lng_recent_posts(),
        }
    }

    /// All tabs in tdesktop display order.
    pub const ALL: [Self; 4] = [Self::Chats, Self::Channels, Self::Apps, Self::PublicPosts];
}

/// Filter applied to message search results (`Dialogs::ChatTypeFilter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChatTypeFilter {
    /// All chat types.
    #[default]
    All,
    /// Private user chats only.
    Private,
    /// Groups and supergroups only.
    Groups,
    /// Broadcast channels only.
    Channels,
}

impl ChatTypeFilter {
    /// Localized display label for the filter toggle in the "Found N messages" bar.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::All => tg_lang::tr::lng_search_filter_all(),
            Self::Private => tg_lang::tr::lng_search_filter_private(),
            Self::Groups => tg_lang::tr::lng_search_filter_group(),
            Self::Channels => tg_lang::tr::lng_search_filter_channel(),
        }
    }

    /// Cycles to the next chat type filter.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Private,
            Self::Private => Self::Groups,
            Self::Groups => Self::Channels,
            Self::Channels => Self::All,
        }
    }

    /// Returns whether `kind` passes this filter.
    #[must_use]
    pub const fn matches(self, kind: DialogPeerKind) -> bool {
        match self {
            Self::All => true,
            Self::Private => matches!(kind, DialogPeerKind::User | DialogPeerKind::Bot),
            Self::Groups => matches!(kind, DialogPeerKind::Group | DialogPeerKind::Forum),
            Self::Channels => matches!(kind, DialogPeerKind::Channel),
        }
    }
}

/// Query parameters for [`DialogsBackend::search_dialogs`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchRequest {
    /// Raw search text from the search box.
    pub query: String,
    /// Active search scope tab.
    pub tab: SearchTab,
    /// Chat type filter for message results.
    pub chat_type_filter: ChatTypeFilter,
    /// Optional peer restriction (e.g. searching inside one chat).
    pub in_peer: Option<PeerId>,
    /// True when searching inside the Archive folder.
    pub in_archive: bool,
    /// Optional tag filter (Saved Messages reaction/tag search).
    pub selected_tag: Option<String>,
    /// Pagination offset for message search results.
    pub messages_offset: usize,
    /// Maximum message results to return per page.
    pub messages_limit: usize,
}

/// Hashtag/cashtag autocomplete suggestion shown at the top of search results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashtagSuggestion {
    /// Tag text without the leading `#` or `$`.
    pub tag: String,
    /// Leading sigil (`'#'` or `'$'`).
    pub sigil: char,
    /// Number of matching messages for this tag.
    pub usage_count: u32,
}

/// Global directory search hit (`PeerSearchResult` in `dialogs_inner_widget.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPeerResult {
    /// Peer identifier.
    pub peer_id: PeerId,
    /// Peer classification.
    pub kind: DialogPeerKind,
    /// Display name / title.
    pub title: String,
    /// Public `@username` (without `@`).
    pub username: String,
    /// Subtitle (e.g. `"@rust_weekly, 18.4K subscribers"` or `"@helper_bot, bot"`).
    pub subtitle: String,
    /// Peer avatar color index.
    pub color_index: u8,
    /// Verified badge.
    pub verified: bool,
    /// Premium badge.
    pub premium: bool,
}

/// Single message match in dialogs search (`FakeRow` in `dialogs_layout.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSearchResult {
    /// Owning peer identifier.
    pub peer_id: PeerId,
    /// Peer classification.
    pub kind: DialogPeerKind,
    /// Special row kind (e.g. Saved Messages).
    pub special: SpecialRowKind,
    /// Peer display title.
    pub peer_title: String,
    /// Peer color index.
    pub color_index: u8,
    /// Matched message identifier.
    pub msg_id: MsgId,
    /// Optional sender prefix (for group chats).
    pub sender_name: Option<String>,
    /// Message text snippet containing the match.
    pub text: String,
    /// Message timestamp.
    pub time: LocalDateTime,
    /// Whether this result belongs to an archived chat.
    pub archived: bool,
}

/// Top (frequently contacted) peer item shown in the horizontal `TopPeersStrip`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopPeerItem {
    /// Peer identifier.
    pub peer_id: PeerId,
    /// Short first name shown under the avatar.
    pub short_name: String,
    /// Full display title for tooltip.
    pub full_title: String,
    /// Peer avatar color index.
    pub color_index: u8,
    /// Whether the user is currently online.
    pub online: bool,
    /// Unread badge counter (`0` = no badge).
    pub unread_count: u32,
    /// Whether unread notifications are muted.
    pub muted: bool,
}

/// Recent search peer entry shown in `Suggestions` when the search box is focused and empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentPeerItem {
    /// Peer identifier.
    pub peer_id: PeerId,
    /// Peer classification.
    pub kind: DialogPeerKind,
    /// Special row marker.
    pub special: SpecialRowKind,
    /// Display title.
    pub title: String,
    /// Status subtitle (e.g. `"online"`, `"last seen recently"`, `"240 members"`).
    pub status: String,
    /// Peer avatar color index.
    pub color_index: u8,
    /// Unread counter badge.
    pub unread_count: u32,
    /// Whether notifications are muted.
    pub muted: bool,
    /// Verified badge.
    pub verified: bool,
}

/// Search tag chip for Saved Messages tag search (`dialogs_search_tags.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTagChip {
    /// Tag identifier / key.
    pub id: String,
    /// Display glyph or short label (e.g. `"#todo"`, `"*Important"`).
    pub label: String,
    /// Number of tagged messages.
    pub count: u32,
}

/// Quota / Stars flood metadata returned by `channels.checkSearchPostsFlood` and
/// `channels.searchPosts` (`SearchPostsFlood` in `dialogs_search_posts.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PostsSearchFloodInfo {
    /// True if the current query can be executed without spending Stars.
    pub query_is_free: bool,
    /// Daily free query allowance.
    pub total_daily: i32,
    /// Remaining free queries for today.
    pub remains: i32,
    /// Optional Unix timestamp when free quota resets (`wait_till`).
    pub wait_till: Option<i32>,
    /// Telegram Stars cost for an extra paid search once free quota is exhausted.
    pub stars_amount: i64,
}

/// Combined search results grouped into tdesktop's exact sections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// Matching hashtag / cashtag completions.
    pub hashtags: Vec<HashtagSuggestion>,
    /// Matching local dialogs and contacts.
    pub local_chats: Vec<DialogItem>,
    /// Matching global public peers / channels / bots.
    pub global_peers: Vec<GlobalPeerResult>,
    /// Matching messages (paginated).
    pub messages: Vec<MessageSearchResult>,
    /// Total number of matching messages before pagination.
    pub total_messages: usize,
    /// `true` if more message results can be loaded.
    pub has_more_messages: bool,
    /// Optional public posts search quota/Stars flood info (`channels.searchPosts`).
    pub posts_flood: Option<PostsSearchFloodInfo>,
}

impl SearchResults {
    /// Returns `true` when all search sections are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hashtags.is_empty()
            && self.local_chats.is_empty()
            && self.global_peers.is_empty()
            && self.messages.is_empty()
    }
}

/// Data-access contract for the dialogs list, folders sidebar, archive, and search.
///
/// Implemented for [`crate::stand_in::backend::MockBackend`] and [`crate::dialogs::mock::MockDialogsStore`],
/// and later backed by `tg-api` + `tg-data` during integration.
pub trait DialogsBackend: Send {
    /// Photo ID of `peer_id`'s animated video avatar when its row should play it
    /// (`InnerWidget::validateVideoUserpic`: premium user with a video userpic, not self).
    fn dialog_video_userpic(&self, _peer_id: PeerId) -> Option<i64> {
        None
    }

    /// Decoded frames of `peer_id`'s video avatar `photo_id`, loading it on first request.
    fn video_userpic_frames(
        &self,
        _peer_id: PeerId,
        _photo_id: i64,
    ) -> crate::core_types::video_userpic::VideoUserpicFetch {
        crate::core_types::video_userpic::VideoUserpicFetch::Failed
    }

    /// Returns the reference "now" local date/time used for relative dialog date formatting.
    fn current_time(&self) -> LocalDateTime;

    /// Returns a paginated slice of dialogs for `list` (`Filter(id)` or `Archive`).
    fn dialog_slice(&self, list: DialogListQuery, offset: usize, limit: usize) -> DialogSlice;

    /// Looks up a single dialog item by `peer_id`.
    fn dialog_by_peer(&self, peer_id: PeerId) -> Option<DialogItem>;

    /// Returns all configured chat folders (always including `FilterId::ALL`).
    fn chat_folders(&self) -> Vec<ChatFolder>;

    /// Persists a reordered folder list (`FiltersMenu::applyReorder`).
    fn reorder_folders(&mut self, order: &[FilterId]);

    /// Adds a peer to a custom chat folder.
    fn add_peer_to_folder(&mut self, peer_id: PeerId, filter_id: FilterId);

    /// Removes a peer from a custom chat folder.
    fn remove_peer_from_folder(&mut self, peer_id: PeerId, filter_id: FilterId);

    /// Marks every chat in `filter_id` as read.
    fn mark_folder_read(&mut self, filter_id: FilterId);

    /// Returns summary metadata for the Archive row, or `None` if there are no archived chats.
    fn archive_summary(&self) -> Option<ArchiveSummary>;

    /// Returns whether the Archive row is currently collapsed to the compact 37px bar.
    fn archive_collapsed(&self) -> bool;

    /// Sets whether the Archive row is collapsed.
    fn set_archive_collapsed(&mut self, collapsed: bool);

    /// Moves `peer_id` into or out of the Archive folder; returns the new `archived` state.
    fn toggle_archive(&mut self, peer_id: PeerId) -> bool;

    /// Marks all archived chats as read.
    fn mark_archive_read(&mut self);

    /// Pins or unpins `peer_id` in `list`; returns the new `pinned` state.
    fn toggle_pin(&mut self, peer_id: PeerId, list: DialogListQuery) -> bool;

    /// Moves a pinned dialog from `from_index` to `to_index` within the pinned section of `list`.
    /// Returns `true` if the order changed.
    fn reorder_pinned(&mut self, list: DialogListQuery, from_index: usize, to_index: usize)
    -> bool;

    /// Toggles notification mute for `peer_id`; returns the new `muted` state.
    fn toggle_mute_dialog(&mut self, peer_id: PeerId) -> bool;

    /// Marks `peer_id` as read (clears unread count, unread mark, mentions, reactions).
    fn mark_dialog_read(&mut self, peer_id: PeerId);

    /// Marks `peer_id` as unread (sets unread mark when unread count is 0).
    fn mark_dialog_unread(&mut self, peer_id: PeerId);

    /// Clears message history preview for `peer_id` while keeping the dialog row.
    fn clear_history(&mut self, peer_id: PeerId);

    /// Deletes/leaves a dialog and removes it from the list.
    fn delete_dialog(&mut self, peer_id: PeerId);

    /// Performs local + global + message + hashtag search.
    fn search_dialogs(&self, request: &SearchRequest) -> SearchResults;

    /// Returns top (frequently contacted) peers for the search suggestions strip.
    fn top_peers(&self) -> Vec<TopPeerItem>;

    /// Removes a peer from the top peers strip.
    fn remove_top_peer(&mut self, peer_id: PeerId);

    /// Returns recently searched/opened peers shown in search suggestions.
    fn recent_search_peers(&self) -> Vec<RecentPeerItem>;

    /// Records a peer as recently searched/opened.
    fn add_recent_search_peer(&mut self, peer_id: PeerId);

    /// Removes one peer from recent searches.
    fn remove_recent_search_peer(&mut self, peer_id: PeerId);

    /// Clears all recent search peers.
    fn clear_recent_search_peers(&mut self);

    /// Returns saved/recent hashtags.
    fn recent_hashtags(&self) -> Vec<String>;

    /// Removes a hashtag from recent hashtags.
    fn remove_recent_hashtag(&mut self, tag: &str);

    /// Returns available search tags (for Saved Messages tag filtering).
    fn search_tags(&self) -> Vec<SearchTagChip>;

    /// Returns the Saved Messages peer identifier for `Ctrl+0` (`Command::ChatSelf`).
    fn saved_messages_peer_id(&self) -> PeerId;

    /// Returns community dialog grouping descriptors for collapsing member chats in the list
    /// (`dialogs_community_*`).
    fn community_groups(&self) -> Vec<crate::core_types::misc::CommunityDialogGroup> {
        Vec::new()
    }

    /// Toggles whether `community_id` is collapsed into one grouped row in the dialogs list
    /// (`communities.toggleCommunityCollapsedInDialogs`).
    fn toggle_community_collapsed(&mut self, _community_id: PeerId, _collapsed: bool) {}

    /// Returns whether colored folder name tags are enabled on dialog rows
    /// (`messages.dialogFilters.tags_enabled` / `messages.toggleDialogFilterTags`).
    fn folder_tags_enabled(&self) -> bool {
        false
    }

    /// Returns the latest server promo suggestions snapshot (`help.getPromoData`), if available.
    fn promo_data(&self) -> Option<crate::core_types::misc::PromoDataSnapshot> {
        None
    }

    /// Dismisses a server promo suggestion (`help.dismissSuggestion`).
    fn dismiss_promo_suggestion(
        &mut self,
        _peer_id: Option<PeerId>,
        _suggestion: &str,
    ) -> Result<bool, String> {
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // WP-66: Contact Birthdays, Top Peers Toggle, Statuses, Sponsored Peers & Hide Promo
    // -----------------------------------------------------------------------

    /// Fetches contact birthdays (`contacts.getBirthdays`,
    /// `data/components/promo_suggestions.cpp:284-346`).
    fn contact_birthdays(
        &self,
        _force: bool,
    ) -> Result<Vec<crate::core_types::dto::contacts_extras::ContactBirthdayEntry>, String> {
        Ok(Vec::new())
    }

    /// Enables or disables top-peers suggestions (`contacts.toggleTopPeers`,
    /// `data/components/top_peers.cpp:195-216`).
    fn toggle_top_peers(&mut self, _enabled: bool) -> Result<bool, String> {
        Err("Toggling top peers is not supported by this backend.".to_owned())
    }

    /// Returns whether top-peers suggestions are currently enabled.
    fn top_peers_enabled(&self) -> bool {
        true
    }

    /// Refreshes contact online/last-seen statuses (`contacts.getStatuses`,
    /// `apiwrap.cpp:2210-2229`).
    fn refresh_contact_statuses(
        &mut self,
    ) -> Result<Vec<crate::core_types::dto::contacts_extras::ContactStatusInfo>, String> {
        Ok(Vec::new())
    }

    /// Queries sponsored search peers for `query` (`contacts.getSponsoredPeers`,
    /// `api/api_peer_search.cpp:100-131`).
    fn sponsored_peers(
        &self,
        _query: &str,
    ) -> Result<Vec<crate::core_types::dto::contacts_extras::SponsoredPeerInfo>, String> {
        Ok(Vec::new())
    }

    /// Hides a promoted / PSA peer from the dialog list (`help.hidePromoData`,
    /// `window/window_peer_menu.cpp:525-530`).
    fn hide_promo_data(&mut self, _peer_id: PeerId) -> Result<bool, String> {
        Err("Hiding promo data is not supported by this backend.".to_owned())
    }
}
