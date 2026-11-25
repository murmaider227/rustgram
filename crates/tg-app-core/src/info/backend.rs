//! Data access trait and view models for Info/Profile screens and Peer/Utility boxes.
//!
//! Port origin: `Telegram/SourceFiles/info/info_controller.h`,
//! `Telegram/SourceFiles/info/media/info_media_provider.h`,
//! `Telegram/SourceFiles/boxes/peers/edit_peer_info_box.h`,
//! and `Telegram/SourceFiles/boxes/report_messages_box.h`.

use tg_data::rights::{ChatAdminRights, ChatBannedRights};

use crate::info::values::{Birthday, UsernameAvailability};
use crate::stand_in::types::PeerId;

/// Detailed peer category shown by the Info/Profile screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InfoPeerKind {
    /// Regular person user account.
    User,
    /// Legacy basic group chat.
    Group,
    /// Supergroup (megagroup).
    Supergroup,
    /// Broadcast channel.
    Channel,
    /// Automated Telegram bot.
    Bot,
    /// The current user's Saved Messages cloud chat.
    SavedMessages,
}

impl InfoPeerKind {
    /// Returns `true` if this peer is a group or supergroup.
    #[must_use]
    pub const fn is_group(self) -> bool {
        matches!(self, Self::Group | Self::Supergroup)
    }

    /// Returns `true` if this peer is a broadcast channel.
    #[must_use]
    pub const fn is_channel(self) -> bool {
        matches!(self, Self::Channel)
    }

    /// Returns the top bar section heading matching tdesktop (`Info::TopBar`).
    #[must_use]
    pub const fn info_heading(self) -> &'static str {
        match self {
            Self::User => "User Info",
            Self::Group | Self::Supergroup => "Group Info",
            Self::Channel => "Channel Info",
            Self::Bot => "Bot Info",
            Self::SavedMessages => "Saved Messages",
        }
    }

    /// Returns the localized top bar section heading via `tg-lang`.
    #[must_use]
    pub fn localized_heading(self) -> String {
        match self {
            Self::SavedMessages => tg_lang::tr::lng_saved_messages(),
            _ => self.info_heading().to_owned(),
        }
    }
}

/// Primary or collectible username attached to a peer (`Data::UsernamesInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerUsername {
    /// Handle without leading `@`.
    pub username: String,
    /// Whether the user can edit this username directly (`false` for collectible NFT handles).
    pub is_editable: bool,
    /// Whether this username is currently active.
    pub is_active: bool,
}

/// Role and permissions of a group/channel member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberRole {
    /// Group or channel creator/owner.
    Owner {
        /// Optional custom rank title shown next to the badge.
        custom_title: Option<String>,
    },
    /// Promoted administrator.
    Admin {
        /// Optional custom rank title.
        custom_title: Option<String>,
        /// Granted admin bitflags (`tg_data::ChatAdminRights`).
        rights: ChatAdminRights,
    },
    /// Regular participant.
    Member,
    /// Member with explicit restrictions.
    Restricted {
        /// Forbidden action flags (`tg_data::ChatBannedRights`).
        rights: ChatBannedRights,
        /// Expiry timestamp (`0` or `i32::MAX` for forever).
        until: i32,
    },
}

/// Member role fallback badges (preserving `MemberRole::badge_label()` contract).
pub const ROLE_OWNER: &str = "owner";
/// Member role fallback badge for group administrators.
pub const ROLE_ADMIN: &str = "admin";
/// Member role fallback badge for restricted members.
pub const ROLE_RESTRICTED: &str = "restricted";

/// Maximum length of a peer member rank / custom tag (`kRankLimit = 16` in `edit_tag_control.cpp`).
pub const MAX_PEER_TAG_RANK_LEN: usize = 16;

/// Strips emoji sequences, collapses newlines to single spaces, trims, and clamps to
/// [`MAX_PEER_TAG_RANK_LEN`] (`16`) Unicode scalars — matching
/// `TextUtilities::RemoveEmoji(TextUtilities::SingleLine(...))` in `edit_tag_control.cpp`.
#[must_use]
pub fn sanitize_peer_tag_rank(raw: &str) -> String {
    let mut no_emoji = String::with_capacity(raw.len());
    let mut pos = 0usize;
    while pos < raw.len() {
        if let Some((_, matched_bytes)) = tg_text::emoji::find_emoji(raw, pos) {
            pos += matched_bytes;
            continue;
        }
        let Some(ch) = raw[pos..].chars().next() else {
            break;
        };
        if ch == '\n' || ch == '\r' || ch == '\t' {
            no_emoji.push(' ');
        } else if !is_emoji_fallback_scalar(ch) {
            no_emoji.push(ch);
        }
        pos += ch.len_utf8();
    }
    no_emoji
        .trim()
        .chars()
        .take(MAX_PEER_TAG_RANK_LEN)
        .collect()
}

/// Returns `true` if `ch` is a fallback emoji scalar excluded from peer rank tags.
#[must_use]
pub fn is_emoji_fallback_scalar(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0xFE0E..=0xFE0F | 0x200D
    )
}

impl MemberRole {
    /// Returns the badge text displayed on the right side of a member row (`info_profile_members.cpp`).
    #[must_use]
    pub fn badge_label(&self) -> Option<&str> {
        match self {
            Self::Owner { custom_title } => Some(custom_title.as_deref().unwrap_or(ROLE_OWNER)),
            Self::Admin { custom_title, .. } => Some(custom_title.as_deref().unwrap_or(ROLE_ADMIN)),
            Self::Restricted { .. } => Some(ROLE_RESTRICTED),
            Self::Member => None,
        }
    }

    /// Returns `true` if this member is an owner or administrator.
    #[must_use]
    pub const fn is_admin_or_owner(&self) -> bool {
        matches!(self, Self::Owner { .. } | Self::Admin { .. })
    }

    /// Returns the member's admin rights, or empty rights for non-admins.
    #[must_use]
    pub fn admin_rights(&self) -> ChatAdminRights {
        match self {
            Self::Owner { .. } => ChatAdminRights::all(),
            Self::Admin { rights, .. } => *rights,
            Self::Member | Self::Restricted { .. } => ChatAdminRights::empty(),
        }
    }

    /// Returns the member's custom title, if any.
    #[must_use]
    pub fn custom_title(&self) -> &str {
        match self {
            Self::Owner { custom_title } | Self::Admin { custom_title, .. } => {
                custom_title.as_deref().unwrap_or_default()
            }
            Self::Member | Self::Restricted { .. } => "",
        }
    }
}

/// Single member row shown in a group/channel profile or admins list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberEntry {
    /// Member's peer identifier.
    pub peer_id: PeerId,
    /// Display name.
    pub name: String,
    /// Optional handle without `@`.
    pub username: Option<String>,
    /// Presence line (`"online"`, `"last seen recently"`, `"bot"`).
    pub status_text: String,
    /// Whether the member is currently online.
    pub is_online: bool,
    /// Whether the member is a bot.
    pub is_bot: bool,
    /// Role / admin badge / rights.
    pub role: MemberRole,
    /// Deterministic avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Group shared in common with a user (`info_common_groups_widget.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonGroupEntry {
    /// Group peer identifier.
    pub peer_id: PeerId,
    /// Group title.
    pub title: String,
    /// Total member count.
    pub members_count: u32,
    /// Avatar palette color index.
    pub color_index: u8,
}

/// Shared media category tabs (`Storage::SharedMediaType` / `Info::Media::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SharedMediaTab {
    /// Photos and videos grid.
    #[default]
    PhotosVideos,
    /// Shared documents / files.
    Files,
    /// Shared web links.
    Links,
    /// Voice & round video messages.
    Voice,
    /// Shared audio / music tracks.
    Music,
    /// Saved / shared GIF animations.
    Gifs,
}

impl SharedMediaTab {
    /// All shared media tabs in Telegram Desktop display order.
    pub const ALL: [Self; 6] = [
        Self::PhotosVideos,
        Self::Files,
        Self::Links,
        Self::Voice,
        Self::Music,
        Self::Gifs,
    ];

    /// Short tab bar label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PhotosVideos => "Media",
            Self::Files => "Files",
            Self::Links => "Links",
            Self::Voice => "Voice",
            Self::Music => "Music",
            Self::Gifs => "GIFs",
        }
    }

    /// Full section title when opened as a dedicated info sub-section.
    #[must_use]
    pub const fn section_title(self) -> &'static str {
        match self {
            Self::PhotosVideos => "Shared Media",
            Self::Files => "Shared Files",
            Self::Links => "Shared Links",
            Self::Voice => "Voice Messages",
            Self::Music => "Shared Music",
            Self::Gifs => "Shared GIFs",
        }
    }

    /// Localized section title via `tg-lang`.
    #[must_use]
    pub fn localized_section_title(self) -> String {
        match self {
            Self::PhotosVideos => tg_lang::tr::lng_profile_shared_media(),
            Self::Files => tg_lang::tr::lng_media_type_files(),
            Self::Links => tg_lang::tr::lng_media_type_links(),
            Self::Voice => tg_lang::tr::lng_media_type_audios(),
            Self::Music => tg_lang::tr::lng_media_type_songs(),
            Self::Gifs => tg_lang::tr::lng_media_type_gifs(),
        }
    }

    /// Zero-based index inside [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::PhotosVideos => 0,
            Self::Files => 1,
            Self::Links => 2,
            Self::Voice => 3,
            Self::Music => 4,
            Self::Gifs => 5,
        }
    }

    /// Converts an index back into a [`SharedMediaTab`].
    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            0 => Self::PhotosVideos,
            1 => Self::Files,
            2 => Self::Links,
            3 => Self::Voice,
            4 => Self::Music,
            _ => Self::Gifs,
        }
    }
}

/// Summary counts displayed on profile action buttons and shared media tabs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SharedMediaCounts {
    /// Photos & videos count.
    pub photos_videos: u32,
    /// Shared files count.
    pub files: u32,
    /// Shared links count.
    pub links: u32,
    /// Voice messages count.
    pub voice: u32,
    /// Music tracks count.
    pub music: u32,
    /// Shared GIFs count.
    pub gifs: u32,
    /// Groups in common (for users).
    pub common_groups: u32,
}

impl SharedMediaCounts {
    /// Returns the count for a specific [`SharedMediaTab`].
    #[must_use]
    pub const fn for_tab(&self, tab: SharedMediaTab) -> u32 {
        match tab {
            SharedMediaTab::PhotosVideos => self.photos_videos,
            SharedMediaTab::Files => self.files,
            SharedMediaTab::Links => self.links,
            SharedMediaTab::Voice => self.voice,
            SharedMediaTab::Music => self.music,
            SharedMediaTab::Gifs => self.gifs,
        }
    }
}

/// Individual shared media entry rendered by [`crate::info::media::MediaListWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaItem {
    /// Photo tile in the Media grid.
    Photo {
        /// Unique item message id.
        id: i32,
        /// Month/date section heading (e.g. `"November 2026"`).
        date_group: String,
        /// Original pixel width.
        width: u32,
        /// Original pixel height.
        height: u32,
        /// Optional caption / search text.
        caption: String,
        /// Accent preview RGB for synthetic thumbnail rendering.
        preview_rgb: [u8; 3],
    },
    /// Video tile in the Media grid.
    Video {
        /// Unique item message id.
        id: i32,
        /// Month/date section heading.
        date_group: String,
        /// Video length in seconds.
        duration_secs: u32,
        /// Original pixel width.
        width: u32,
        /// Original pixel height.
        height: u32,
        /// Optional caption.
        caption: String,
        /// Accent preview RGB for synthetic thumbnail rendering.
        preview_rgb: [u8; 3],
    },
    /// Shared document/file row.
    File {
        /// Unique item message id.
        id: i32,
        /// Month/date section heading.
        date_group: String,
        /// File name including extension.
        filename: String,
        /// File size in bytes.
        size_bytes: u64,
        /// Short uppercase extension badge (e.g. `"PDF"`, `"ZIP"`).
        ext: String,
        /// Formatted upload date/time.
        date_label: String,
    },
    /// Shared web link row.
    Link {
        /// Unique item message id.
        id: i32,
        /// Month/date section heading.
        date_group: String,
        /// Full URL.
        url: String,
        /// Link preview title.
        title: String,
        /// Short snippet.
        description: String,
        /// Domain host (e.g. `"docs.rs"`).
        domain: String,
    },
    /// Shared voice message row with waveform bars.
    Voice {
        /// Unique item message id.
        id: i32,
        /// Month/date section heading.
        date_group: String,
        /// Sender display name.
        sender: String,
        /// Duration in seconds.
        duration_secs: u32,
        /// Normalized waveform bar heights (`0..=31`).
        waveform: Vec<u8>,
        /// Whether already played.
        listened: bool,
        /// Formatted timestamp.
        date_label: String,
    },
    /// Shared audio/music track row.
    Music {
        /// Unique item message id.
        id: i32,
        /// Month/date section heading.
        date_group: String,
        /// Track title.
        title: String,
        /// Artist / performer name.
        performer: String,
        /// Track duration in seconds.
        duration_secs: u32,
        /// File size in bytes.
        size_bytes: u64,
    },
    /// Shared GIF tile.
    Gif {
        /// Unique item message id.
        id: i32,
        /// Month/date section heading.
        date_group: String,
        /// Short descriptor label.
        label: String,
        /// Duration in seconds.
        duration_secs: u32,
        /// Accent preview RGB.
        preview_rgb: [u8; 3],
    },
}

impl MediaItem {
    /// Returns the item's message identifier.
    #[must_use]
    pub const fn id(&self) -> i32 {
        match *self {
            Self::Photo { id, .. }
            | Self::Video { id, .. }
            | Self::File { id, .. }
            | Self::Link { id, .. }
            | Self::Voice { id, .. }
            | Self::Music { id, .. }
            | Self::Gif { id, .. } => id,
        }
    }

    /// Returns the month/date header string for section grouping.
    #[must_use]
    pub fn date_group(&self) -> &str {
        match self {
            Self::Photo { date_group, .. }
            | Self::Video { date_group, .. }
            | Self::File { date_group, .. }
            | Self::Link { date_group, .. }
            | Self::Voice { date_group, .. }
            | Self::Music { date_group, .. }
            | Self::Gif { date_group, .. } => date_group,
        }
    }

    /// Returns `true` if this item matches a case-insensitive search query.
    #[must_use]
    pub fn matches_query(&self, query_lower: &str) -> bool {
        if query_lower.is_empty() {
            return true;
        }
        match self {
            Self::Photo {
                caption,
                date_group,
                ..
            }
            | Self::Video {
                caption,
                date_group,
                ..
            } => {
                caption.to_lowercase().contains(query_lower)
                    || date_group.to_lowercase().contains(query_lower)
            }
            Self::File { filename, ext, .. } => {
                filename.to_lowercase().contains(query_lower)
                    || ext.to_lowercase().contains(query_lower)
            }
            Self::Link {
                url,
                title,
                description,
                domain,
                ..
            } => {
                url.to_lowercase().contains(query_lower)
                    || title.to_lowercase().contains(query_lower)
                    || description.to_lowercase().contains(query_lower)
                    || domain.to_lowercase().contains(query_lower)
            }
            Self::Voice {
                sender, date_label, ..
            } => {
                sender.to_lowercase().contains(query_lower)
                    || date_label.to_lowercase().contains(query_lower)
            }
            Self::Music {
                title, performer, ..
            } => {
                title.to_lowercase().contains(query_lower)
                    || performer.to_lowercase().contains(query_lower)
            }
            Self::Gif { label, .. } => label.to_lowercase().contains(query_lower),
        }
    }
}

/// Thumbnail fetch state for a shared-media grid tile (photo, video, or GIF)
/// (`Info::Media::Photo` / `Info::Media::Video` in `info_media_list_widget.cpp`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SharedMediaThumb {
    /// No thumbnail metadata or bytes are available (render synthetic `preview_rgb` fallback).
    #[default]
    Unavailable,
    /// Full thumbnail download is in flight (or only a stripped preview exists); optional
    /// Telegram stripped JPEG bytes (`photoStrippedSize`, starts with `0x01`) may be rendered
    /// blurred while loading.
    Loading {
        /// Optional stripped JPEG thumbnail bytes for immediate low-res preview.
        stripped_thumb: Option<Vec<u8>>,
    },
    /// Downloaded thumbnail image bytes (JPEG/PNG/WebP) ready to decode and display.
    Ready {
        /// Encoded image bytes.
        bytes: Vec<u8>,
    },
}

/// Group call / live stream state on a peer profile (`PeerData::groupCall` +
/// `PeerData::canManageGroupCall`, `info_profile_top_bar.cpp:1405-1420`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileGroupCallState {
    /// `true` when the group or channel currently has an active voice/video chat or live stream
    /// (`ChatFlags::CALL_ACTIVE` / `ChannelFlags::CALL_ACTIVE`, or loaded `InputGroupCall`).
    pub active: bool,
    /// `true` when the active call currently has participants (`CALL_NOT_EMPTY`).
    pub has_participants: bool,
    /// Active server group-call identifier `(call_id, access_hash)` when loaded from
    /// `ChatFull.call` / `ChannelFull.call` or `updateGroupCall`.
    pub call_id: Option<(i64, u64)>,
    /// `true` when the current account has permission to start or manage a group call /
    /// live stream in this group or channel (`PeerData::canManageGroupCall`).
    pub can_manage: bool,
}

impl ProfileGroupCallState {
    /// Returns `true` when the profile top bar should show the Video chat / Live stream button
    /// (`peer->groupCall() || (!peer->isUser() && peer->canManageGroupCall())`).
    #[must_use]
    pub const fn show_button(&self) -> bool {
        self.active || self.can_manage
    }
}

/// Complete peer profile model rendered by the Info/Profile screen and Edit Peer boxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerProfile {
    /// Unique peer identifier.
    pub id: PeerId,
    /// Classification (User / Group / Supergroup / Channel / Bot / SavedMessages).
    pub kind: InfoPeerKind,
    /// Display name or chat title.
    pub title: String,
    /// Presence or member/subscriber subtitle.
    pub status: String,
    /// Whether presence text should be tinted with accent blue (`online`).
    pub is_online: bool,
    /// Verified check badge.
    pub verified: bool,
    /// Premium star badge.
    pub premium: bool,
    /// Deterministic color index (`0..7`) for `EmptyUserpic`.
    pub color_index: u8,
    /// Optional phone number (users).
    pub phone: Option<String>,
    /// Primary and collectible usernames.
    pub usernames: Vec<PeerUsername>,
    /// User bio or group/channel description.
    pub bio: String,
    /// Optional user birthday.
    pub birthday: Option<Birthday>,
    /// Optional private invite link (`https://t.me/+...`).
    pub invite_link: Option<String>,
    /// Notifications muted flag.
    pub muted: bool,
    /// Whether this user is in the contacts list.
    pub is_contact: bool,
    /// Whether this user/bot is blocked.
    pub is_blocked: bool,
    /// Whether the current account can edit this peer's info.
    pub can_edit: bool,
    /// Whether the current account can invite/add members.
    pub can_add_members: bool,
    /// Whether the current account can manage administrators.
    pub can_manage_admins: bool,
    /// Whether the current account can edit default permissions.
    pub can_manage_permissions: bool,
    /// Whether the group/channel is public (has a public username).
    pub is_public: bool,
    /// Whether forwarding/saving content is restricted (`noforwards`).
    pub no_forwards: bool,
    /// Whether the group has forum topics enabled.
    pub is_forum: bool,
    /// Total members or subscribers count.
    pub members_count: u32,
    /// Online members count (groups).
    pub online_count: u32,
    /// Number of administrators.
    pub admins_count: u32,
    /// Group default banned rights (`tg_data::ChatBannedRights`).
    pub default_banned_rights: ChatBannedRights,
    /// Group slow-mode delay in seconds (`0` = off).
    pub slowmode_seconds: u32,
    /// Auto-delete messages TTL in seconds (`0` = off).
    pub auto_delete_ttl_seconds: u32,
    /// Linked discussion group `(peer_id, title)` for broadcast channels (or linked channel for groups).
    pub linked_discussion_peer: Option<(PeerId, String)>,
    /// Number of registered slash commands (bots).
    pub bot_commands_count: usize,
    /// Whether a bot can read all group messages.
    pub bot_reads_all_messages: bool,
    /// Whether a bot offers a Mini App.
    pub has_mini_app: bool,
    /// Shared media and common groups counts.
    pub media_counts: SharedMediaCounts,
    /// Active group call / live stream status and start permission (for the Video chat button).
    pub group_call: ProfileGroupCallState,
}

impl PeerProfile {
    /// Returns the primary active username without leading `@`, if any.
    #[must_use]
    pub fn primary_username(&self) -> Option<&str> {
        self.usernames
            .iter()
            .find(|u| u.is_active)
            .map(|u| u.username.as_str())
    }

    /// Returns the editable username without leading `@`, if any.
    #[must_use]
    pub fn editable_username(&self) -> Option<&str> {
        self.usernames
            .iter()
            .find(|u| u.is_editable)
            .map(|u| u.username.as_str())
    }

    /// Returns the best shareable link (`https://t.me/<username>` or private invite link).
    #[must_use]
    pub fn shareable_link(&self) -> Option<String> {
        if let Some(u) = self.primary_username() {
            Some(crate::info::values::format_username_link(u))
        } else {
            self.invite_link.clone()
        }
    }
}

/// Request payload for creating a new group (`GroupInfoBox` + `AddParticipantsBox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateGroupRequest {
    /// Group title (1..=128 chars).
    pub title: String,
    /// Initial member peer IDs to invite.
    pub member_ids: Vec<PeerId>,
    /// Auto-delete messages TTL in seconds (`0` = off).
    pub auto_delete_ttl_seconds: u32,
    /// Whether a custom cropped photo was attached.
    pub has_custom_photo: bool,
}

/// Request payload for creating a new broadcast channel (`GroupInfoBox` + `SetupChannelBox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateChannelRequest {
    /// Channel title (1..=128 chars).
    pub title: String,
    /// Optional channel description (0..=255 chars).
    pub description: String,
    /// `true` for public channel with username; `false` for private invite-link channel.
    pub is_public: bool,
    /// Public username (required when `is_public == true`).
    pub username: Option<String>,
    /// Restrict saving/forwarding content.
    pub no_forwards: bool,
    /// Whether a custom cropped photo was attached.
    pub has_custom_photo: bool,
}

/// Request payload for updating an existing peer's metadata (`EditPeerInfoBox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePeerInfoRequest {
    /// Updated title / display name.
    pub title: String,
    /// Updated bio / description.
    pub description: String,
    /// Updated public/private flag.
    pub is_public: bool,
    /// Updated public username (empty string clears).
    pub username: Option<String>,
    /// Updated restrict saving content toggle.
    pub no_forwards: bool,
}

/// Report reason options matching `Ui::ReportReason` in `report_box_graphics.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportReason {
    /// Unsolicited spam.
    Spam,
    /// Violence or threats.
    Violence,
    /// Adult / pornographic content.
    Pornography,
    /// Child abuse.
    ChildAbuse,
    /// Copyright infringement.
    Copyright,
    /// Illegal drugs.
    IllegalDrugs,
    /// Personal details / doxxing.
    PersonalDetails,
    /// Impersonation / fake account.
    Fake,
    /// Other reason with custom explanation.
    Other,
}

impl ReportReason {
    /// All standard report reasons in display order.
    pub const ALL: [Self; 9] = [
        Self::Spam,
        Self::Violence,
        Self::Pornography,
        Self::ChildAbuse,
        Self::Copyright,
        Self::IllegalDrugs,
        Self::PersonalDetails,
        Self::Fake,
        Self::Other,
    ];

    /// Human-readable label shown in `ReportBox`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Spam => "Spam",
            Self::Violence => "Violence",
            Self::Pornography => "Pornography",
            Self::ChildAbuse => "Child Abuse",
            Self::Copyright => "Copyright",
            Self::IllegalDrugs => "Illegal Drugs",
            Self::PersonalDetails => "Personal Details",
            Self::Fake => "Fake Account",
            Self::Other => "Other",
        }
    }

    /// Localized report reason label via `tg-lang`.
    #[must_use]
    pub fn localized_label(self) -> String {
        use tg_lang::tr;
        match self {
            Self::Spam => tr::lng_report_reason_spam(),
            Self::Violence => tr::lng_report_reason_violence(),
            Self::Pornography => tr::lng_report_reason_pornography(),
            Self::ChildAbuse => tr::lng_report_reason_child_abuse(),
            Self::Copyright => tr::lng_report_reason_copyright(),
            Self::IllegalDrugs => tr::lng_report_reason_illegal_drugs(),
            Self::PersonalDetails => tr::lng_report_reason_personal_details(),
            Self::Fake => tr::lng_report_reason_fake(),
            Self::Other => tr::lng_report_reason_other(),
        }
    }
}

/// Available UI language option (`Lang::Language` in `language_box.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageEntry {
    /// ISO / Telegram lang pack code (e.g. `"en"`, `"de"`, `"es"`).
    pub id: String,
    /// Language name in English (e.g. `"German"`).
    pub name: String,
    /// Language name in its own script (e.g. `"Deutsch"`).
    pub native_name: String,
    /// Whether this is an official language pack.
    pub official: bool,
    /// Approximate translation completeness percentage (`0..=100`).
    pub progress_percent: u8,
}

/// Terms of Service document presented by [`crate::boxes::terms::TermsOfServiceBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermsOfServiceData {
    /// Unique ToS revision identifier.
    pub id: String,
    /// Modal heading.
    pub title: String,
    /// Body paragraphs.
    pub body: String,
    /// Optional minimum age confirmation requirement (e.g. `Some(16)`).
    pub min_age_confirm: Option<u8>,
    /// Whether the user has already accepted this revision.
    pub accepted: bool,
}

/// Exported chat invite link metadata (`Api::InviteLink` / `chatInviteExported`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteLinkInfo {
    /// Full invite URL (e.g. `"https://t.me/+AbCdEf123456"`).
    pub link: String,
    /// Optional admin-visible label (up to 32 chars).
    pub title: String,
    /// Administrator who created this link.
    pub admin_id: PeerId,
    /// Display name of the creating administrator.
    pub admin_name: String,
    /// Creation timestamp (Unix seconds).
    pub date: i32,
    /// Optional start date when expiry countdown began (Unix seconds).
    pub start_date: Option<i32>,
    /// Optional expiration timestamp (Unix seconds, `None` = never expires).
    pub expire_date: Option<i32>,
    /// Optional maximum number of joins (`None` = unlimited).
    pub usage_limit: Option<i32>,
    /// Number of users who have already joined via this link.
    pub usage: i32,
    /// Number of pending join requests submitted through this link.
    pub requested: i32,
    /// Whether this is the admin's primary permanent invite link.
    pub permanent: bool,
    /// Whether this link has been revoked.
    pub revoked: bool,
    /// Whether joining via this link requires admin approval.
    pub request_needed: bool,
}

/// Parameters for creating (`messages.exportChatInvite`) or editing
/// (`messages.editExportedChatInvite`) an invite link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InviteLinkRequest {
    /// Optional label (max 32 chars).
    pub title: String,
    /// Optional Unix timestamp when the link expires (`None` = no expiry).
    pub expire_date: Option<i32>,
    /// Optional usage limit (`None` = unlimited; ignored when `request_needed == true`).
    pub usage_limit: Option<i32>,
    /// Require admin approval before users enter the chat.
    pub request_needed: bool,
    /// Legacy flag to revoke the permanent invite link on `exportChatInvite`.
    pub legacy_revoke_permanent: bool,
}

/// A user who joined via an invite link or submitted a join request (`chatInviteImporter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteImporterInfo {
    /// Importer/requester user peer identifier.
    pub user_id: PeerId,
    /// Display name.
    pub name: String,
    /// Optional username without leading `@`.
    pub username: Option<String>,
    /// Optional user bio snippet returned with join requests.
    pub about: Option<String>,
    /// Timestamp when the user joined or requested to join (Unix seconds).
    pub date: i32,
    /// `true` if this entry is a pending join request (`requested: true`).
    pub requested: bool,
    /// `true` if the user joined via a shareable folder chatlist link.
    pub via_chatlist: bool,
    /// Optional admin who approved the join request.
    pub approved_by: Option<PeerId>,
    /// Deterministic avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Paginated invite links response (`messages.ExportedChatInvites`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InviteLinksList {
    /// Total count reported by the server.
    pub count: i32,
    /// Invite links in this slice.
    pub links: Vec<InviteLinkInfo>,
}

/// Paginated invite importers / pending join requests response (`messages.ChatInviteImporters`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InviteImportersList {
    /// Total count reported by the server.
    pub count: i32,
    /// Importer or requester entries.
    pub importers: Vec<InviteImporterInfo>,
}

/// Summary for the in-chat join-requests top bar (`Ui::RequestsBarContent`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JoinRequestsSummary {
    /// Number of pending join requests.
    pub count: u32,
    /// Up to 3 recent requester `(peer_id, short_name, color_index)` tuples.
    pub recent_requesters: Vec<(PeerId, String, u8)>,
}

/// Current accent and profile color configuration for a peer (`PeerColor`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerColorSettings {
    /// Accent color index for replies/names (`0..64`, `None` = peer default).
    pub color_index: Option<u8>,
    /// Custom emoji document ID tiled on reply/quote background (`0` = none).
    pub background_emoji_id: u64,
    /// Profile header accent color index (`0..64`, `None` = default/unset).
    pub profile_color_index: Option<u8>,
    /// Custom emoji document ID tiled on profile header background (`0` = none).
    pub profile_background_emoji_id: u64,
}

/// Request payload for `account.updateColor` or `channels.updateColor`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdatePeerColorRequest {
    /// `false` = reply/name accent color; `true` = profile header color.
    pub for_profile: bool,
    /// Selected color index (`None` resets profile color to default).
    pub color_index: Option<u8>,
    /// Custom emoji document ID for background pattern (`0` = none).
    pub background_emoji_id: u64,
}

/// Structured error returned by ownership transfer pre-checks and execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipTransferError {
    /// 2FA cloud password must be enabled before transferring ownership (`PASSWORD_MISSING`).
    PasswordMissing,
    /// 2FA password was enabled too recently (`PASSWORD_TOO_FRESH_<N>`).
    PasswordTooFresh {
        /// Remaining cooldown seconds, if provided by server.
        retry_after_secs: u32,
    },
    /// Current session was authorized too recently (`SESSION_TOO_FRESH_<N>`).
    SessionTooFresh {
        /// Remaining cooldown seconds, if provided by server.
        retry_after_secs: u32,
    },
    /// Entered 2FA cloud password did not match (`PASSWORD_HASH_INVALID`).
    WrongPassword,
    /// Any other server or validation error.
    ServerError(String),
}

impl OwnershipTransferError {
    /// Returns a human-readable explanation matching tdesktop's `PrePasswordErrorBox`
    /// and `ChannelOwnershipTransfer::sendRequest` error mapping.
    #[must_use]
    pub fn display_message(&self) -> String {
        match self {
            Self::PasswordMissing => {
                "You need to enable Two-Step Verification (2FA) in Settings before transferring ownership."
                    .to_owned()
            }
            Self::PasswordTooFresh { .. } => {
                "You recently enabled Two-Step Verification. Please wait 7 days before transferring ownership."
                    .to_owned()
            }
            Self::SessionTooFresh { .. } => {
                "This session was signed in too recently. Please wait 24 hours before transferring ownership."
                    .to_owned()
            }
            Self::WrongPassword => "Incorrect Two-Step Verification password.".to_owned(),
            Self::ServerError(msg) => msg.clone(),
        }
    }
}

/// Result of pre-checking whether the current account can transfer chat ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipTransferCheck {
    /// Pre-checks passed; user should confirm and enter their 2FA password.
    Ready {
        /// Optional 2FA password hint from `account.getPassword`.
        password_hint: String,
    },
    /// Account/session is not eligible to transfer ownership yet.
    Blocked {
        /// Structured reason.
        reason: OwnershipTransferError,
        /// Human-readable message.
        message: String,
    },
}

/// Recommended similar channel entry (`channels.getChannelRecommendations`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimilarChannelEntry {
    /// Channel peer identifier.
    pub peer_id: PeerId,
    /// Channel display title.
    pub title: String,
    /// Optional public handle without `@`.
    pub username: Option<String>,
    /// Subscriber count.
    pub subscribers_count: u32,
    /// Verified badge flag.
    pub verified: bool,
    /// Avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Result of `channels.getChannelRecommendations`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimilarChannelsResult {
    /// Recommended channels.
    pub channels: Vec<SimilarChannelEntry>,
    /// Total recommendations available (`count` in `messages.chatsSlice`).
    pub total_count: u32,
    /// `true` if additional recommendations are gated behind Telegram Premium.
    pub has_more_for_premium: bool,
}

/// State of a file entry in the Info Downloads Manager (`info/downloads/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStatus {
    /// Actively downloading.
    Downloading {
        /// Bytes received so far.
        received_bytes: u64,
        /// Total expected bytes.
        total_bytes: u64,
    },
    /// Download paused by the user.
    Paused {
        /// Bytes received before pausing.
        received_bytes: u64,
        /// Total expected bytes.
        total_bytes: u64,
    },
    /// Completed and saved to disk/cache.
    Completed,
    /// Failed with an error message.
    Failed {
        /// Failure summary.
        reason: String,
    },
}

/// Single file entry displayed in the Info Downloads Manager (`Data::DownloadManager`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadEntry {
    /// Stable download entry identifier.
    pub id: u64,
    /// Source chat peer identifier.
    pub peer_id: PeerId,
    /// Source chat display title.
    pub peer_title: String,
    /// Source message id.
    pub msg_id: i32,
    /// File name including extension.
    pub file_name: String,
    /// MIME type string (e.g. `"application/pdf"`, `"video/mp4"`).
    pub mime_type: String,
    /// Total file size in bytes.
    pub total_bytes: u64,
    /// Optional local file path once downloaded.
    pub local_path: Option<String>,
    /// Download start timestamp (Unix seconds).
    pub started_at: i32,
    /// Date group header (e.g. `"Today"`, `"November 2026"`).
    pub date_group: String,
    /// Current transfer status.
    pub status: DownloadStatus,
}

impl DownloadEntry {
    /// Returns `true` if this entry matches a case-insensitive search query.
    #[must_use]
    pub fn matches_query(&self, query_lower: &str) -> bool {
        if query_lower.is_empty() {
            return true;
        }
        self.file_name.to_lowercase().contains(query_lower)
            || self.peer_title.to_lowercase().contains(query_lower)
            || self.mime_type.to_lowercase().contains(query_lower)
    }

    /// Returns a short uppercase file extension label (e.g. `"PDF"`, `"MP4"`).
    #[must_use]
    pub fn ext_badge(&self) -> String {
        self.file_name
            .rsplit('.')
            .next()
            .filter(|e| !e.is_empty() && e.len() <= 5 && *e != self.file_name)
            .map(|e| e.to_ascii_uppercase())
            .unwrap_or_else(|| "FILE".to_owned())
    }
}

/// Data backend contract for all Info/Profile screens and Peer/Utility boxes.
///
/// Implemented by [`crate::info::mock::MockBackend`] for standalone development, tests,
/// and screenshots; integration swaps in an adapter over `tg-data` + `tg-api`.
pub trait InfoBackend: Send {
    /// Returns the full profile for `peer_id`, if present.
    fn peer_profile(&self, peer_id: PeerId) -> Option<PeerProfile>;

    /// Returns all known peers (for share, forward, and linked discussion pickers).
    fn all_peers(&self) -> Vec<PeerProfile>;

    /// Returns user contacts eligible for group creation and add-member pickers.
    fn contacts(&self) -> Vec<PeerProfile>;

    /// Returns members of a group or channel filtered by case-insensitive `query`.
    fn peer_members(&self, peer_id: PeerId, query: &str) -> Vec<MemberEntry>;

    /// Returns groups shared in common with `user_id`.
    fn common_groups(&self, user_id: PeerId) -> Vec<CommonGroupEntry>;

    /// Returns shared media items for `peer_id` in `tab`, filtered by `query`.
    fn shared_media(&self, peer_id: PeerId, tab: SharedMediaTab, query: &str) -> Vec<MediaItem>;

    /// Loads or returns the cached thumbnail image for a shared-media item (`msg_id` in `peer_id`).
    ///
    /// Live backends serve cached bytes immediately (`SharedMediaThumb::Ready`) or start an
    /// asynchronous `upload.getFile` thumbnail download without blocking the UI thread, returning
    /// `SharedMediaThumb::Loading { stripped_thumb }` while the download is in flight
    /// (`info_media_list_widget.cpp`).
    fn shared_media_thumb(&self, _peer_id: PeerId, _msg_id: i32) -> SharedMediaThumb {
        SharedMediaThumb::Unavailable
    }

    /// Toggles notification mute state for `peer_id` and returns the new `muted` flag.
    fn toggle_notifications(&mut self, peer_id: PeerId) -> bool;

    /// Toggles block status for a user/bot and returns the new `is_blocked` flag.
    fn toggle_block_user(&mut self, peer_id: PeerId) -> bool;

    /// Adds a new contact and returns its newly created [`PeerId`].
    fn add_contact(
        &mut self,
        first_name: &str,
        last_name: &str,
        phone: &str,
    ) -> Result<PeerId, String>;

    /// Creates a new group chat and returns its [`PeerId`].
    fn create_group(&mut self, req: CreateGroupRequest) -> Result<PeerId, String>;

    /// Creates a new broadcast channel and returns its [`PeerId`].
    fn create_channel(&mut self, req: CreateChannelRequest) -> Result<PeerId, String>;

    /// Saves edits to a peer's title, description, type, and username.
    fn update_peer_info(
        &mut self,
        peer_id: PeerId,
        req: UpdatePeerInfoRequest,
    ) -> Result<(), String>;

    /// Saves group default permissions (`ChatBannedRights`) and slow-mode delay.
    fn update_default_permissions(
        &mut self,
        peer_id: PeerId,
        rights: ChatBannedRights,
        slowmode_seconds: u32,
    ) -> Result<(), String>;

    /// Promotes or updates an administrator's rights (`ChatAdminRights`) and custom title.
    fn update_admin_rights(
        &mut self,
        peer_id: PeerId,
        user_id: PeerId,
        rights: ChatAdminRights,
        custom_title: String,
    ) -> Result<(), String>;

    /// Adds one or more contacts into a group and returns how many new members were added.
    fn add_members(&mut self, peer_id: PeerId, member_ids: &[PeerId]) -> Result<usize, String>;

    /// Removes a member from a group.
    fn remove_member(&mut self, peer_id: PeerId, user_id: PeerId) -> Result<(), String>;

    /// Returns the custom rank/tag assigned to `user_id` in `peer_id`, if any.
    fn member_rank(&self, _peer_id: PeerId, _user_id: PeerId) -> Option<String> {
        None
    }

    /// Edits a group/supergroup participant's custom rank/tag (`messages.editChatParticipantRank`).
    fn edit_member_rank(
        &mut self,
        _peer_id: PeerId,
        _user_id: PeerId,
        rank: &str,
    ) -> Result<String, String> {
        Ok(sanitize_peer_tag_rank(rank))
    }

    /// Updates auto-delete timer TTL (in seconds, `0` = Off) for `peer_id`.
    fn set_auto_delete_ttl(&mut self, peer_id: PeerId, ttl_seconds: u32) -> Result<(), String>;

    /// Links or unlinks a discussion group for a broadcast channel.
    fn set_linked_discussion(
        &mut self,
        channel_id: PeerId,
        group_id: Option<PeerId>,
    ) -> Result<(), String>;

    /// Validates and checks availability of `username` (ignoring `exclude_peer`'s current handle).
    fn check_username(&self, username: &str, exclude_peer: Option<PeerId>) -> UsernameAvailability;

    /// Updates the editable public username for `peer_id`.
    fn update_username(&mut self, peer_id: PeerId, username: &str) -> Result<(), String>;

    /// Forwards `message_ids` with an optional `comment` to `target_peers`.
    fn forward_messages(
        &mut self,
        target_peers: &[PeerId],
        message_ids: &[i32],
        comment: &str,
    ) -> Result<usize, String>;

    /// Deletes `message_ids` from `peer_id` (optionally revoking for everyone).
    fn delete_messages(
        &mut self,
        peer_id: PeerId,
        message_ids: &[i32],
        revoke_for_everyone: bool,
    ) -> Result<usize, String>;

    /// Submits a peer/message abuse report.
    fn submit_report(
        &mut self,
        peer_id: PeerId,
        reason: ReportReason,
        comment: &str,
    ) -> Result<(), String>;

    /// Returns all available UI languages.
    fn languages(&self) -> Vec<LanguageEntry>;

    /// Returns the currently active language code (e.g. `"en"`).
    fn active_language(&self) -> String;

    /// Switches the active UI language.
    fn set_language(&mut self, lang_code: &str) -> Result<(), String>;

    /// Returns the active Terms of Service data.
    fn terms_of_service(&self) -> TermsOfServiceData;

    /// Records user acceptance of a Terms of Service revision.
    fn accept_terms(&mut self, tos_id: &str) -> Result<(), String>;

    // -------------------------------------------------------------------------
    // Peer Management Gaps (WP-10): Invite Links, Join Requests, Peer Colors,
    // Ownership Transfer, Similar Channels, and Downloads Manager.
    // -------------------------------------------------------------------------

    /// Fetches active (`revoked = false`) or revoked (`revoked = true`) invite links
    /// (`messages.getExportedChatInvites`).
    fn exported_chat_invites(
        &self,
        _peer_id: PeerId,
        _revoked: bool,
    ) -> Result<InviteLinksList, String> {
        Ok(InviteLinksList::default())
    }

    /// Creates a new invite link (`messages.exportChatInvite`).
    fn export_chat_invite(
        &mut self,
        _peer_id: PeerId,
        _req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        Err("Creating invite links is not supported by this backend.".to_owned())
    }

    /// Edits an existing invite link's label, expiry, usage limit, or approval flag
    /// (`messages.editExportedChatInvite`).
    fn edit_exported_chat_invite(
        &mut self,
        _peer_id: PeerId,
        _link: &str,
        _req: InviteLinkRequest,
    ) -> Result<InviteLinkInfo, String> {
        Err("Editing invite links is not supported by this backend.".to_owned())
    }

    /// Revokes an invite link (`messages.editExportedChatInvite` with `revoked = true`).
    /// Returns `(revoked_link, optional_new_permanent_link)`.
    fn revoke_exported_chat_invite(
        &mut self,
        _peer_id: PeerId,
        _link: &str,
    ) -> Result<(InviteLinkInfo, Option<InviteLinkInfo>), String> {
        Err("Revoking invite links is not supported by this backend.".to_owned())
    }

    /// Permanently deletes a revoked invite link (`messages.deleteExportedChatInvite`).
    fn delete_exported_chat_invite(&mut self, _peer_id: PeerId, _link: &str) -> Result<(), String> {
        Err("Deleting invite links is not supported by this backend.".to_owned())
    }

    /// Permanently deletes all revoked invite links for `peer_id`
    /// (`messages.deleteRevokedExportedChatInvites`).
    fn delete_revoked_exported_chat_invites(&mut self, _peer_id: PeerId) -> Result<(), String> {
        Err("Deleting revoked invite links is not supported by this backend.".to_owned())
    }

    /// Fetches users who joined via `link` or pending join requests (`requested = true`)
    /// (`messages.getChatInviteImporters`).
    fn chat_invite_importers(
        &self,
        _peer_id: PeerId,
        _link: Option<&str>,
        _requested: bool,
        _query: &str,
    ) -> Result<InviteImportersList, String> {
        Ok(InviteImportersList::default())
    }

    /// Approves (`approved = true`) or dismisses (`approved = false`) a single join request
    /// (`messages.hideChatJoinRequest`).
    fn hide_chat_join_request(
        &mut self,
        _peer_id: PeerId,
        _user_id: PeerId,
        _approved: bool,
    ) -> Result<(), String> {
        Err("Join request approval is not supported by this backend.".to_owned())
    }

    /// Approves or dismisses all pending join requests (`messages.hideAllChatJoinRequests`),
    /// returning how many requests were resolved.
    fn hide_all_chat_join_requests(
        &mut self,
        _peer_id: PeerId,
        _link: Option<&str>,
        _approved: bool,
    ) -> Result<usize, String> {
        Err("Bulk join request actions are not supported by this backend.".to_owned())
    }

    /// Returns a compact summary of pending join requests for `peer_id`.
    fn join_requests_summary(&self, _peer_id: PeerId) -> JoinRequestsSummary {
        JoinRequestsSummary::default()
    }

    /// Returns the current reply/name and profile color settings for `peer_id`.
    fn peer_color_settings(&self, peer_id: PeerId) -> PeerColorSettings {
        let idx = self.peer_profile(peer_id).map(|p| p.color_index);
        PeerColorSettings {
            color_index: idx,
            ..PeerColorSettings::default()
        }
    }

    /// Updates accent or profile color and background emoji (`account.updateColor` /
    /// `channels.updateColor`).
    fn update_peer_color(
        &mut self,
        _peer_id: PeerId,
        _req: UpdatePeerColorRequest,
    ) -> Result<(), String> {
        Err("Peer color editing is not supported by this backend.".to_owned())
    }

    /// Pre-checks 2FA and session age before transferring group/channel ownership.
    fn check_ownership_transfer(&self, _peer_id: PeerId) -> Result<OwnershipTransferCheck, String> {
        Ok(OwnershipTransferCheck::Ready {
            password_hint: String::new(),
        })
    }

    /// Transfers ownership of `peer_id` to `new_owner_id` after verifying the 2FA SRP password
    /// (`messages.editChatCreator`).
    fn transfer_ownership(
        &mut self,
        _peer_id: PeerId,
        _new_owner_id: PeerId,
        _password: &str,
    ) -> Result<(), OwnershipTransferError> {
        Err(OwnershipTransferError::ServerError(
            "Ownership transfer is not supported by this backend.".to_owned(),
        ))
    }

    /// Fetches similar recommended channels for `peer_id` (`channels.getChannelRecommendations`).
    fn similar_channels(&self, _peer_id: PeerId) -> Result<SimilarChannelsResult, String> {
        Ok(SimilarChannelsResult::default())
    }

    /// Lists entries in the Downloads Manager filtered by `query`.
    fn downloads_list(&self, _query: &str) -> Vec<DownloadEntry> {
        Vec::new()
    }

    /// Toggles pause/resume on an active or paused download.
    fn toggle_download_pause(&mut self, _download_id: u64) -> Result<(), String> {
        Err("Downloads manager is not supported by this backend.".to_owned())
    }

    /// Cancels an active download or removes a finished entry (optionally deleting the local file).
    fn cancel_or_delete_download(
        &mut self,
        _download_id: u64,
        _delete_file: bool,
    ) -> Result<(), String> {
        Err("Downloads manager is not supported by this backend.".to_owned())
    }

    /// Removes all completed entries from the Downloads list and returns the count cleared.
    fn clear_completed_downloads(&mut self) -> Result<usize, String> {
        Ok(0)
    }

    /// Removes all download entries and returns the count cleared.
    fn delete_all_downloads(&mut self) -> Result<usize, String> {
        Ok(0)
    }

    // -------------------------------------------------------------------------
    // Chat History Import (WP-30): WhatsApp / LINE / KakaoTalk archives.
    // -------------------------------------------------------------------------

    /// Inspects the leading lines of an exported chat log (`messages.checkHistoryImport`).
    fn check_history_import(
        &self,
        import_head: &str,
    ) -> Result<crate::core_types::dto::chat_import::HistoryImportParsedInfo, String> {
        if import_head.trim().is_empty() {
            return Err("Chat history header is empty.".to_owned());
        }
        Ok(
            crate::core_types::dto::chat_import::HistoryImportParsedInfo {
                pm: true,
                group: true,
                title: None,
            },
        )
    }

    /// Checks whether `peer_id` can receive imported messages (`messages.checkHistoryImportPeer`).
    fn check_history_import_peer(&self, peer_id: PeerId) -> Result<String, String> {
        let title = self
            .peer_profile(peer_id)
            .map(|p| p.title)
            .unwrap_or_else(|| "this chat".to_owned());
        Ok(format!(
            "Messages and media from the export archive will be imported into {title}."
        ))
    }

    /// Uploads and starts a chat history import (`initHistoryImport`, `uploadImportedMedia`,
    /// `startHistoryImport`).
    fn run_history_import(
        &mut self,
        peer_id: PeerId,
        archive: &crate::core_types::dto::chat_import::ParsedImportArchive,
    ) -> Result<crate::core_types::dto::chat_import::ChatImportSummary, String> {
        Ok(crate::core_types::dto::chat_import::ChatImportSummary {
            import_id: 1,
            peer_id,
            uploaded_media_count: archive.media_files.len(),
            skipped_media_count: 0,
            started: true,
        })
    }
    /// Loads global shared media across all chats (`Info::GlobalMedia` / `messages.searchGlobal`).
    fn global_media(
        &self,
        _tab: SharedMediaTab,
        _query: &str,
        _limit: usize,
    ) -> crate::core_types::dto::global_media::GlobalMediaPage {
        crate::core_types::dto::global_media::GlobalMediaPage::default()
    }
    // Profile Extras & Peer Management (WP-26): Profile Stories/Albums,
    // Peer Gifts, Saved Music, Emoji Userpic, Peer Reactions, Collectible
    // Usernames toggle/reorder, and Bot Custom Verification.
    // -------------------------------------------------------------------------

    /// Fetches pinned stories for `peer_id` (`stories.getPinnedStories`).
    fn peer_pinned_stories(
        &self,
        _peer_id: PeerId,
        _offset_id: i32,
        _limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        Ok(ProfileStoriesPage::default())
    }

    /// Fetches archived stories for `peer_id` (`stories.getStoriesArchive`).
    fn peer_stories_archive(
        &self,
        _peer_id: PeerId,
        _offset_id: i32,
        _limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        Ok(ProfileStoriesPage::default())
    }

    /// Fetches story albums for `peer_id` (`stories.getAlbums`).
    fn peer_story_albums(&self, _peer_id: PeerId) -> Result<Vec<ProfileStoryAlbum>, String> {
        Ok(Vec::new())
    }

    /// Fetches stories inside `album_id` for `peer_id` (`stories.getAlbumStories`).
    fn peer_album_stories(
        &self,
        _peer_id: PeerId,
        _album_id: i32,
        _offset_id: i32,
        _limit: i32,
    ) -> Result<ProfileStoriesPage, String> {
        Ok(ProfileStoriesPage::default())
    }

    /// Fetches saved Star Gifts and collections for `peer_id`
    /// (`payments.getSavedStarGifts` + `payments.getStarGiftCollections`).
    fn peer_saved_gifts(
        &self,
        _peer_id: PeerId,
        _filter: &tg_payments::star_gifts::PeerGiftsFilter,
    ) -> Result<ProfilePeerGiftsPage, String> {
        Ok(ProfilePeerGiftsPage::default())
    }

    /// Shows (`unsave = false`) or hides (`unsave = true`) a saved gift on a peer's profile
    /// (`payments.saveStarGift`).
    fn toggle_peer_gift_saved(
        &mut self,
        _peer_id: PeerId,
        _msg_id: i32,
        _saved_id: u64,
        _unsave: bool,
    ) -> Result<(), String> {
        Err("Star gift visibility updates are not supported by this backend.".to_owned())
    }

    /// Fetches saved profile music tracks for `peer_id` (`users.getSavedMusic`).
    fn peer_saved_music(
        &self,
        _peer_id: PeerId,
        _offset: i32,
        _limit: i32,
    ) -> Result<Vec<SavedMusicTrack>, String> {
        Ok(Vec::new())
    }

    /// Saves (`unsave = false`) or removes (`unsave = true`) a track on the user's profile
    /// (`account.saveMusic`).
    fn save_profile_music(
        &mut self,
        _track: &SavedMusicTrack,
        _unsave: bool,
    ) -> Result<(), String> {
        Err("Saved profile music updates are not supported by this backend.".to_owned())
    }

    /// Uploads an emoji + gradient avatar (`photos.uploadProfilePhoto` / `channels.editPhoto`
    /// with `videoSizeEmojiMarkup`).
    fn upload_emoji_userpic(
        &mut self,
        _peer_id: PeerId,
        _req: EmojiUserpicRequest,
    ) -> Result<(), String> {
        Err("Emoji userpic builder is not supported by this backend.".to_owned())
    }

    /// Returns the allowed reactions configuration for `peer_id`.
    fn peer_available_reactions(&self, _peer_id: PeerId) -> PeerReactionsConfig {
        PeerReactionsConfig::default()
    }

    /// Saves allowed reactions for `peer_id` (`messages.setChatAvailableReactions`).
    fn set_peer_available_reactions(
        &mut self,
        _peer_id: PeerId,
        _config: PeerReactionsConfig,
    ) -> Result<(), String> {
        Err("Editing peer reactions is not supported by this backend.".to_owned())
    }

    /// Activates or deactivates a collectible username (`channels.toggleUsername` /
    /// `account.toggleUsername` / `bots.toggleUsername`).
    fn toggle_peer_username(
        &mut self,
        _peer_id: PeerId,
        _username: &str,
        _active: bool,
    ) -> Result<(), String> {
        Err("Toggling collectible usernames is not supported by this backend.".to_owned())
    }

    /// Reorders active usernames (`channels.reorderUsernames` /
    /// `account.reorderUsernames` / `bots.reorderUsernames`).
    fn reorder_peer_usernames(
        &mut self,
        _peer_id: PeerId,
        _order: &[String],
    ) -> Result<(), String> {
        Err("Reordering usernames is not supported by this backend.".to_owned())
    }

    /// Returns the current bot custom verification status for `peer_id` under verifier `bot_id`.
    fn peer_verification_status(&self, bot_id: PeerId, peer_id: PeerId) -> PeerVerificationStatus {
        PeerVerificationStatus {
            bot_id,
            peer_id,
            verified: false,
            custom_description: String::new(),
        }
    }

    /// Grants or revokes third-party bot verification (`bots.setCustomVerification`).
    fn set_peer_custom_verification(
        &mut self,
        _bot_id: PeerId,
        _peer_id: PeerId,
        _enabled: bool,
        _custom_description: Option<&str>,
    ) -> Result<(), String> {
        Err("Bot custom verification is not supported by this backend.".to_owned())
    }

    // -------------------------------------------------------------------------
    // WP-27: Communities, Managed Bots, ModerateMessagesBox, Welcome Messages,
    // and Channel Anti-Spam.
    // -------------------------------------------------------------------------

    /// Lists all joined communities (`communities.getJoinedCommunities`).
    fn joined_communities(
        &self,
    ) -> Result<Vec<crate::core_types::dto::community::CommunityDetails>, String> {
        Ok(Vec::new())
    }

    /// Returns details for `community_id` (`channels.getFullChannel` -> `CommunityFull`).
    fn community_details(
        &self,
        _community_id: PeerId,
    ) -> Option<crate::core_types::dto::community::CommunityDetails> {
        None
    }

    /// Returns the community linked to `peer_id` (or `peer_id` itself if it is a community).
    fn community_for_peer(
        &self,
        _peer_id: PeerId,
    ) -> Option<crate::core_types::dto::community::CommunityDetails> {
        None
    }

    /// Creates a new community (`communities.create`) and returns its channel peer ID.
    fn create_community(
        &mut self,
        _req: crate::core_types::dto::community::CreateCommunityRequest,
    ) -> Result<PeerId, String> {
        Err("Creating communities is not supported by this backend.".to_owned())
    }

    /// Links, changes visibility of, or removes a peer in `community_id`
    /// (`communities.togglePeerLink`).
    fn toggle_community_peer_link(
        &mut self,
        _community_id: PeerId,
        _peer_id: PeerId,
        _action: crate::core_types::dto::community::CommunityPeerLinkAction,
    ) -> Result<(), String> {
        Err("Community peer links are not supported by this backend.".to_owned())
    }

    /// Toggles whether `community_id` is collapsed into one row in the dialogs list
    /// (`communities.toggleCommunityCollapsedInDialogs`).
    fn toggle_community_collapsed_in_dialogs(
        &mut self,
        _community_id: PeerId,
        _collapsed: bool,
    ) -> Result<(), String> {
        Err("Community collapse toggle is not supported by this backend.".to_owned())
    }

    /// Fetches pending peer link requests for `community_id`
    /// (`communities.getPeerLinkRequests`).
    fn community_peer_link_requests(
        &self,
        _community_id: PeerId,
        _offset: &str,
        _limit: i32,
    ) -> Result<crate::core_types::dto::community::CommunityPeerRequestsSlice, String> {
        Ok(crate::core_types::dto::community::CommunityPeerRequestsSlice::default())
    }

    /// Approves (`reject = false`) or rejects (`reject = true`) a single peer link request
    /// (`communities.togglePeerLinkRequestApproval`).
    fn toggle_community_peer_link_request_approval(
        &mut self,
        _community_id: PeerId,
        _peer_id: PeerId,
        _reject: bool,
    ) -> Result<(), String> {
        Err("Community link request approval is not supported by this backend.".to_owned())
    }

    /// Approves or rejects all pending peer link requests for `community_id`
    /// (`communities.toggleAllPeerLinkRequestApproval`).
    fn toggle_all_community_peer_link_request_approval(
        &mut self,
        _community_id: PeerId,
        _reject: bool,
    ) -> Result<(), String> {
        Err("Bulk community link request approval is not supported by this backend.".to_owned())
    }

    /// Bans (`unban = false`) or unbans (`unban = true`) a participant across `community_id`
    /// (`communities.toggleParticipantBanned`).
    fn toggle_community_participant_banned(
        &mut self,
        _community_id: PeerId,
        _participant_id: PeerId,
        _unban: bool,
    ) -> Result<(), String> {
        Err("Community bans are not supported by this backend.".to_owned())
    }

    /// Lists community chats created or joined by `participant_id`
    /// (`communities.getParticipantJoinedChats`).
    fn community_participant_joined_chats(
        &self,
        _community_id: PeerId,
        _participant_id: PeerId,
    ) -> Result<crate::core_types::dto::community::CommunityParticipantJoinedChats, String> {
        Ok(crate::core_types::dto::community::CommunityParticipantJoinedChats::default())
    }

    /// Updates community title and "who can add chats" policy (`ManageCommunityBox`).
    fn update_community_settings(
        &mut self,
        _community_id: PeerId,
        _title: &str,
        _only_admins_can_add: bool,
    ) -> Result<(), String> {
        Err("Community settings are not supported by this backend.".to_owned())
    }

    /// Deletes a community (`channels.deleteChannel`).
    fn delete_community(&mut self, _community_id: PeerId) -> Result<(), String> {
        Err("Deleting communities is not supported by this backend.".to_owned())
    }

    /// Checks whether a managed bot username is available (`bots.checkUsername`).
    fn check_bot_username(&self, _username: &str) -> Result<bool, String> {
        Ok(true)
    }

    /// Creates a managed bot (`bots.createBot`) and returns its user peer ID.
    fn create_managed_bot(
        &mut self,
        _manager_id: PeerId,
        _name: &str,
        _username: &str,
        _via_deeplink: bool,
    ) -> Result<PeerId, String> {
        Err("Managed bot creation is not supported by this backend.".to_owned())
    }

    /// Fetches localized bot info texts (`bots.getBotInfo`).
    fn get_bot_info(
        &self,
        _bot_id: PeerId,
        _lang_code: &str,
    ) -> Result<crate::core_types::dto::managed_bot::BotInfoTexts, String> {
        Ok(crate::core_types::dto::managed_bot::BotInfoTexts::default())
    }

    /// Updates localized bot info texts (`bots.setBotInfo`).
    fn set_bot_info(
        &mut self,
        _bot_id: PeerId,
        _lang_code: &str,
        _change: &crate::core_types::dto::managed_bot::BotInfoChange,
    ) -> Result<(), String> {
        Err("Editing bot info is not supported by this backend.".to_owned())
    }

    /// Translates bot info fields into `lang_code` (`messages.translateText`).
    fn translate_bot_info_texts(
        &self,
        _texts: &[String],
        _lang_code: &str,
    ) -> Result<Vec<String>, String> {
        Err("Bot info translation is not supported by this backend.".to_owned())
    }

    /// Lists admined/managed bots (`bots.getAdminedBots`).
    fn admined_bots(&self) -> Result<Vec<PeerId>, String> {
        Ok(Vec::new())
    }

    /// Executes `ModerateMessagesBox` group moderation actions (delete / report / delete all / ban).
    fn moderate_messages(
        &mut self,
        _req: crate::core_types::dto::moderate_messages::ModerateMessagesRequest,
    ) -> Result<crate::core_types::dto::moderate_messages::ModerateMessagesResult, String> {
        Err("Message moderation is not supported by this backend.".to_owned())
    }

    /// Fetches welcome message templates for `peer_id` (`ephemeral.getWelcomeMessages`).
    fn welcome_messages(
        &self,
        _peer_id: PeerId,
    ) -> Result<Vec<crate::core_types::dto::welcome_messages::WelcomeMessageItem>, String> {
        Ok(Vec::new())
    }

    /// Creates a new welcome message template (`ephemeral.sendMessage` with `welcome = true`).
    fn send_welcome_message(
        &mut self,
        _peer_id: PeerId,
        _text: &str,
    ) -> Result<crate::core_types::dto::welcome_messages::WelcomeMessageItem, String> {
        Err("Welcome messages are not supported by this backend.".to_owned())
    }

    /// Edits an existing welcome message template (`ephemeral.editMessage` with `welcome = true`).
    fn edit_welcome_message(
        &mut self,
        _peer_id: PeerId,
        _id: i32,
        _text: &str,
    ) -> Result<crate::core_types::dto::welcome_messages::WelcomeMessageItem, String> {
        Err("Welcome messages are not supported by this backend.".to_owned())
    }

    /// Deletes a single welcome message template (`ephemeral.deleteWelcomeMessage`).
    fn delete_welcome_message(&mut self, _peer_id: PeerId, _id: i32) -> Result<(), String> {
        Err("Welcome messages are not supported by this backend.".to_owned())
    }

    /// Deletes all welcome message templates for `peer_id` (`ephemeral.deleteAllWelcomeMessages`).
    fn delete_all_welcome_messages(&mut self, _peer_id: PeerId) -> Result<(), String> {
        Err("Welcome messages are not supported by this backend.".to_owned())
    }

    /// Returns whether aggressive anti-spam (`ChannelFlags::ANTI_SPAM`) is enabled for `peer_id`.
    fn channel_anti_spam_enabled(&self, _peer_id: PeerId) -> bool {
        false
    }

    /// Toggles aggressive anti-spam on a supergroup/channel (`channels.toggleAntiSpam`).
    fn toggle_channel_anti_spam(&mut self, _peer_id: PeerId, _enabled: bool) -> Result<(), String> {
        Err("Channel anti-spam toggle is not supported by this backend.".to_owned())
    }

    /// Reports a false-positive anti-spam deletion (`channels.reportAntiSpamFalsePositive`).
    fn report_antispam_false_positive(
        &mut self,
        _peer_id: PeerId,
        _msg_id: i32,
    ) -> Result<(), String> {
        Err("Anti-spam false positive reporting is not supported by this backend.".to_owned())
    }

    /// Resolves Fragment collectible info (`fragment.getCollectibleInfo`) for a collectible
    /// username or anonymous phone number owned by `peer_id`.
    fn get_collectible_info(
        &self,
        _peer_id: PeerId,
        _entity: &str,
    ) -> Result<crate::core_types::dto::collectible_info::CollectibleInfoData, String> {
        Err("Collectible info lookup is not supported by this backend.".to_owned())
    }

    // -------------------------------------------------------------------------
    // WP-60: Paid-message fee revenue, fee-exception toggling & contact requirements
    // -------------------------------------------------------------------------

    /// Fetches Stars paid-message revenue earned from `user_id` (optionally scoped to a channel
    /// direct-messages `parent_peer`) via `account.getPaidMessagesRevenue`.
    fn get_paid_messages_revenue(
        &self,
        _parent_peer: Option<PeerId>,
        _user_id: PeerId,
    ) -> Result<PaidMessagesRevenue, String> {
        Err("Paid-message revenue lookup is not supported by this backend.".to_owned())
    }

    /// Adds or removes a paid-message fee exception for `user_id` via
    /// `account.toggleNoPaidMessagesException`.
    fn toggle_no_paid_messages_exception(
        &mut self,
        _parent_peer: Option<PeerId>,
        _user_id: PeerId,
        _refund_charged: bool,
        _require_payment: bool,
    ) -> Result<bool, String> {
        Err("Paid-message fee exception toggle is not supported by this backend.".to_owned())
    }

    /// Fetches contact restrictions (none, Premium required, or Stars fee per message) for
    /// `user_ids` via `users.getRequirementsToContact`.
    fn get_requirements_to_contact(
        &self,
        _user_ids: &[PeerId],
    ) -> Result<Vec<UserContactRequirement>, String> {
        Err("Contact requirement lookup is not supported by this backend.".to_owned())
    }
    /// Sets the default/main profile tab for the signed-in account (`account.setMainProfileTab`)
    /// or a broadcast channel (`channels.setMainProfileTab`), matching
    /// `Info::Profile::TabsHost::setMainTab` (`info_profile_tabs_host.cpp`).
    fn set_main_profile_tab(
        &mut self,
        _peer_id: PeerId,
        _tab: ProfileMainTab,
    ) -> Result<(), String> {
        Err("Setting main profile tab is not supported by this backend.".to_owned())
    }

    /// Returns the cached main profile tab for `peer_id`, if known.
    fn main_profile_tab(&self, _peer_id: PeerId) -> Option<ProfileMainTab> {
        None
    }

    /// Suggests a birthday to a contact (`users.suggestBirthday`, `local_url_handlers.cpp`).
    fn suggest_user_birthday(
        &mut self,
        _peer_id: PeerId,
        _birthday: Birthday,
    ) -> Result<(), String> {
        Err("Suggesting birthdays is not supported by this backend.".to_owned())
    }

    /// Reports a peer's profile photo (`account.reportProfilePhoto`, `api_report.cpp`).
    fn report_profile_photo(
        &mut self,
        _peer_id: PeerId,
        _photo: ProfilePhotoRef,
        _reason: ReportReason,
        _comment: &str,
    ) -> Result<(), String> {
        Err("Reporting profile photos is not supported by this backend.".to_owned())
    }
    /// Returns the personal channel linked on `peer_id`'s profile, if any.
    fn personal_channel(&self, _peer_id: PeerId) -> Option<PersonalChannelOption> {
        None
    }

    /// Returns the user's own public channels eligible as a personal channel.
    fn personal_channel_candidates(&self) -> Vec<PersonalChannelOption> {
        Vec::new()
    }

    /// Updates or clears (`None`) the signed-in account's personal channel
    /// (`account.updatePersonalChannel`).
    fn update_personal_channel(&mut self, _channel_id: Option<PeerId>) -> Result<(), String> {
        Err("Updating personal channel is not supported by this backend.".to_owned())
    }

    /// Returns public TON / Stars balance summary displayed by `info_profile_balance_pill`, if any.
    fn peer_profile_balance(&self, _peer_id: PeerId) -> Option<ProfileBalanceInfo> {
        None
    }

    /// Returns whether channel Star Gift notifications are enabled for `peer_id`
    /// (`chat_notifications_enabled` in `info_peer_gifts_widget.cpp`).
    fn chat_star_gift_notifications_enabled(&self, _peer_id: PeerId) -> Option<bool> {
        None
    }

    /// Enables or disables channel Star Gift notifications (`payments.toggleChatStarGiftNotifications`).
    fn toggle_chat_star_gift_notifications(
        &mut self,
        _peer_id: PeerId,
        _enabled: bool,
    ) -> Result<bool, String> {
        Err("Toggling channel Star Gift notifications is not supported by this backend.".to_owned())
    }

    // -------------------------------------------------------------------------
    // WP-65: Channel & Supergroup Management Protocol Completion
    // (`13 channels.*` + `messages.deleteChat` + `messages.getAdminsWithInvites`)
    // -------------------------------------------------------------------------

    /// Returns cached channel/supergroup management flags and settings for `peer_id`.
    fn channel_management_state(&self, _peer_id: PeerId) -> Option<ChannelManagementState> {
        None
    }

    /// Toggles whether new members can view chat history before joining
    /// (`channels.togglePreHistoryHidden`, `edit_peer_info_box.cpp:2817-2847`).
    fn toggle_pre_history_hidden(&mut self, _peer_id: PeerId, _hidden: bool) -> Result<(), String> {
        Err("Toggling pre-history visibility is not supported by this backend.".to_owned())
    }

    /// Fetches groups eligible to be linked as a channel's discussion group
    /// (`channels.getGroupsForDiscussion`, `edit_peer_info_box.cpp:1044-1064`).
    fn groups_for_discussion(&self) -> Result<Vec<DiscussionGroupCandidate>, String> {
        Ok(Vec::new())
    }

    /// Upgrades a supergroup to a broadcast-style gigagroup
    /// (`channels.convertToGigagroup`, `edit_peer_permissions_box.cpp:1364-1375`).
    fn convert_to_gigagroup(&mut self, _peer_id: PeerId) -> Result<(), String> {
        Err("Converting to a gigagroup is not supported by this backend.".to_owned())
    }

    /// Deactivates all collectible usernames on `peer_id`
    /// (`channels.deactivateAllUsernames`, `edit_peer_info_box.cpp:2490-2501`).
    fn deactivate_all_channel_usernames(&mut self, _peer_id: PeerId) -> Result<(), String> {
        Err("Deactivating all channel usernames is not supported by this backend.".to_owned())
    }

    /// Enables or disables forum topics mode and tabbed subsection layout on a supergroup
    /// (`channels.toggleForum`, `edit_peer_info_box.cpp:2872-2891`).
    fn toggle_channel_forum(
        &mut self,
        _peer_id: PeerId,
        _enabled: bool,
        _tabs: bool,
    ) -> Result<(), String> {
        Err("Toggling channel forum mode is not supported by this backend.".to_owned())
    }

    /// Hides or reveals the members list in a supergroup
    /// (`channels.toggleParticipantsHidden`, `edit_members_visible.cpp:71-77`).
    fn toggle_participants_hidden(
        &mut self,
        _peer_id: PeerId,
        _hidden: bool,
    ) -> Result<(), String> {
        Err("Toggling hidden participants is not supported by this backend.".to_owned())
    }

    /// Toggles viewing a forum supergroup as a flat message stream
    /// (`channels.toggleViewForumAsMessages`, `data_session.cpp:6014-6027`).
    fn toggle_view_forum_as_messages(
        &mut self,
        _peer_id: PeerId,
        _as_messages: bool,
    ) -> Result<(), String> {
        Err("Toggling forum view-as-messages is not supported by this backend.".to_owned())
    }

    /// Sets how many boosts exempt a member from default group restrictions (`0` disables)
    /// (`channels.setBoostsToUnblockRestrictions`, `edit_peer_info_box.cpp:357-386`).
    fn set_boosts_to_unblock_restrictions(
        &mut self,
        _peer_id: PeerId,
        _boosts: u32,
    ) -> Result<(), String> {
        Err("Setting boosts to unblock restrictions is not supported by this backend.".to_owned())
    }

    /// Assigns or clears (`ChannelStickerSetRef::Empty`) the group's official sticker pack
    /// (`channels.setStickers`, `apiwrap.cpp:3158-3169`).
    fn set_channel_sticker_set(
        &mut self,
        _peer_id: PeerId,
        _set: ChannelStickerSetRef,
    ) -> Result<(), String> {
        Err("Setting channel sticker set is not supported by this backend.".to_owned())
    }

    /// Assigns or clears (`ChannelStickerSetRef::Empty`) the group's official custom emoji pack
    /// (`channels.setEmojiStickers`, `apiwrap.cpp:3171-3185`).
    fn set_channel_emoji_sticker_set(
        &mut self,
        _peer_id: PeerId,
        _set: ChannelStickerSetRef,
    ) -> Result<(), String> {
        Err("Setting channel emoji sticker set is not supported by this backend.".to_owned())
    }

    /// Updates Stars-per-message pricing and (for broadcast channels) direct-messages availability
    /// (`channels.updatePaidMessagesPrice`, `edit_peer_info_box.cpp:314-355`).
    fn update_channel_paid_messages_price(
        &mut self,
        _peer_id: PeerId,
        _broadcast_messages_allowed: bool,
        _stars_per_message: i64,
    ) -> Result<(), String> {
        Err("Updating paid messages price is not supported by this backend.".to_owned())
    }

    /// Enables or disables channel post auto-translation
    /// (`channels.toggleAutotranslation`, `edit_peer_info_box.cpp:2893-2914`).
    fn toggle_channel_autotranslation(
        &mut self,
        _peer_id: PeerId,
        _enabled: bool,
    ) -> Result<(), String> {
        Err("Toggling channel autotranslation is not supported by this backend.".to_owned())
    }

    /// Permanently deletes a basic group created by the current account
    /// (`messages.deleteChat`, `data_histories.cpp:875-897`).
    fn delete_basic_chat(&mut self, _peer_id: PeerId) -> Result<(), String> {
        Err("Deleting basic groups is not supported by this backend.".to_owned())
    }

    /// Lists administrators who have created invite links for `peer_id`
    /// (`messages.getAdminsWithInvites`, `edit_peer_invite_links.cpp:804-822`).
    fn admins_with_invites(&self, _peer_id: PeerId) -> Result<Vec<AdminWithInvitesEntry>, String> {
        Ok(Vec::new())
    }
    // WP-66: Contacts, Contact Tokens, Fee-Waiver Tokens, Notes & Profile Photos
    // -------------------------------------------------------------------------

    /// Deletes one or more contacts (`contacts.deleteContacts`,
    /// `boxes/contacts_box.cpp`, `boxes/peers/edit_contact_box.cpp`).
    fn delete_contacts(&mut self, _peer_ids: &[PeerId]) -> Result<(), String> {
        Err("Deleting contacts is not supported by this backend.".to_owned())
    }

    /// Convenience helper deleting a single contact via [`Self::delete_contacts`].
    fn delete_contact(&mut self, peer_id: PeerId) -> Result<(), String> {
        self.delete_contacts(&[peer_id])
    }

    /// Exports a temporary contact QR / deep-link token (`contacts.exportContactToken`,
    /// `info/profile/info_profile_qr_link.cpp`).
    fn export_contact_token(
        &self,
    ) -> Result<crate::core_types::dto::contacts_extras::ExportedContactTokenInfo, String> {
        Err("Exporting contact tokens is not supported by this backend.".to_owned())
    }

    /// Resolves a temporary contact token (`contacts.importContactToken`,
    /// `window/window_session_controller_contact.cpp`).
    fn import_contact_token(&mut self, _token: &str) -> Result<PeerId, String> {
        Err("Importing contact tokens is not supported by this backend.".to_owned())
    }

    /// Exports a paid-message fee-waiver token (`contacts.exportNoPaidMessagesToken`,
    /// `api/api_premium.cpp`).
    fn export_no_paid_messages_token(
        &self,
        _revoke: bool,
        _temporary: bool,
    ) -> Result<crate::core_types::dto::contacts_extras::ExportedNoPaidMessagesTokenInfo, String>
    {
        Err("Exporting no-paid-messages tokens is not supported by this backend.".to_owned())
    }

    /// Redeems a paid-message fee-waiver token for `peer_id`
    /// (`contacts.importNoPaidMessagesToken`, `api/api_premium.cpp`).
    fn import_no_paid_messages_token(
        &mut self,
        _peer_id: PeerId,
        _token: &str,
    ) -> Result<crate::core_types::dto::contacts_extras::NoPaidMessagesTokenImportResult, String>
    {
        Err("Importing no-paid-messages tokens is not supported by this backend.".to_owned())
    }

    /// Returns the cached private contact note for `peer_id`, if any.
    fn contact_note(&self, _peer_id: PeerId) -> Option<tg_text::TextWithEntities> {
        None
    }

    /// Updates or clears (when empty) the private contact note for `peer_id`
    /// (`contacts.updateContactNote`, `info/profile/info_profile_actions.cpp`).
    fn update_contact_note(
        &mut self,
        _peer_id: PeerId,
        _note: tg_text::TextWithEntities,
    ) -> Result<bool, String> {
        Err("Updating contact notes is not supported by this backend.".to_owned())
    }

    /// Returns the real server [`ProfilePhotoRef`] (`id`, `access_hash`, `file_reference`)
    /// for `peer_id`'s currently active profile photo, resolved from cached `UserFull` /
    /// `ChatFull` / `ChannelFull` metadata or `photos.getUserPhotos`.
    ///
    /// Returns `None` when the peer has no active profile photo or no valid server photo
    /// reference is known — implementations MUST NEVER fabricate an `InputPhoto` from
    /// `peer_id` or `access_hash == 0` (`api/api_peer_photo.cpp`).
    fn active_profile_photo_ref(&self, _peer_id: PeerId) -> Option<ProfilePhotoRef> {
        None
    }

    /// Deletes one or more of the signed-in account's own profile photos (`photos.deletePhotos`,
    /// `api/api_peer_photo.cpp`).
    fn delete_profile_photos(&mut self, _photos: &[ProfilePhotoRef]) -> Result<Vec<i64>, String> {
        Err("Deleting profile photos is not supported by this backend.".to_owned())
    }

    /// Uploads, suggests, or clears a personal contact profile photo
    /// (`photos.uploadContactProfilePhoto`, `api/api_peer_photo.cpp`).
    fn upload_contact_profile_photo(
        &mut self,
        _peer_id: PeerId,
        _request: crate::core_types::dto::contacts_extras::ContactProfilePhotoUpload,
    ) -> Result<i64, String> {
        Err("Uploading contact profile photos is not supported by this backend.".to_owned())
    }

    /// Clears the personal contact profile photo for `peer_id` (`PeerPhoto::clearPersonal`).
    fn clear_contact_profile_photo(&mut self, peer_id: PeerId) -> Result<i64, String> {
        self.upload_contact_profile_photo(
            peer_id,
            crate::core_types::dto::contacts_extras::ContactProfilePhotoUpload::clear_personal(),
        )
    }
}

/// Primary profile tab selection matching `Data::ProfileTab` (`data_peer.h:124-134`)
/// and TL `ProfileTab` constructors (`schema/api.tl:2115-2122`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileMainTab {
    /// Posts / Stories tab (`profileTabPosts`).
    Posts,
    /// Star Gifts tab (`profileTabGifts`).
    Gifts,
    /// Shared Media tab (`profileTabMedia`).
    Media,
    /// Shared Files tab (`profileTabFiles`).
    Files,
    /// Shared Music tab (`profileTabMusic`).
    Music,
    /// Shared Voice messages tab (`profileTabVoice`).
    Voice,
    /// Shared Links tab (`profileTabLinks`).
    Links,
    /// Shared GIFs tab (`profileTabGifs`).
    Gifs,
}

/// Reference to a server profile photo (`InputPhoto`) for `photos.deletePhotos`,
/// `account.reportProfilePhoto`, and `messages.getAttachedStickers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePhotoRef {
    /// Server photo ID.
    pub id: i64,
    /// Photo access hash.
    pub access_hash: i64,
    /// File reference token.
    pub file_reference: Vec<u8>,
}

/// Candidate or active personal channel displayed on a user's profile (`account.updatePersonalChannel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalChannelOption {
    /// Channel peer identifier.
    pub peer_id: PeerId,
    /// Channel display title.
    pub title: String,
    /// Optional public `@username` without leading `@`.
    pub username: Option<String>,
    /// Subscriber count.
    pub subscribers_count: u32,
}

/// TON and Telegram Stars balance summary displayed by `Info::Profile::BalancePill`
/// (`info_profile_balance_pill.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProfileBalanceInfo {
    /// TON balance in nanotons (`1 TON = 1_000_000_000`).
    pub ton_nanos: i64,
    /// Telegram Stars balance (whole Stars; `0` hides the Stars pill).
    pub stars_balance: i64,
}

/// Summary card for a story displayed in a peer profile Stories / Albums tab
/// (`info/stories/info_stories_inner_widget.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileStoryItem {
    /// Story identifier within `peer_id`.
    pub id: i32,
    /// Owning peer identifier.
    pub peer_id: PeerId,
    /// Publication timestamp (Unix seconds).
    pub date: i32,
    /// Optional caption snippet.
    pub caption: String,
    /// Whether this story is pinned on the profile.
    pub pinned: bool,
    /// Whether this story is a video clip (shows duration badge).
    pub is_video: bool,
    /// Video duration in seconds (`0` for photo stories).
    pub duration_secs: u32,
    /// Total view count (if visible).
    pub views_count: u32,
    /// Album IDs this story belongs to.
    pub album_ids: Vec<i32>,
    /// Synthetic preview RGB tint for thumbnail rendering.
    pub preview_rgb: [u8; 3],
}

/// Story album metadata returned by `stories.getAlbums`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileStoryAlbum {
    /// Server-assigned album identifier.
    pub album_id: i32,
    /// Album display title.
    pub title: String,
    /// Number of stories in this album (when known).
    pub count: u32,
}

/// Paginated slice of profile stories (`stories.Stories`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileStoriesPage {
    /// Total story count reported by the server.
    pub total_count: i32,
    /// Stories in this slice.
    pub stories: Vec<ProfileStoryItem>,
    /// IDs pinned to the top of the profile grid.
    pub pinned_to_top: Vec<i32>,
}

/// Saved Star Gifts and collections for a peer profile (`info/peer_gifts/*`).
#[derive(Debug, Clone, Default)]
pub struct ProfilePeerGiftsPage {
    /// Total saved gift count for the peer.
    pub total_count: i32,
    /// Filtered received gifts (reusing `payments::backend::ReceivedGiftItem` / `tg-payments`).
    pub gifts: Vec<crate::payments::backend::ReceivedGiftItem>,
    /// Peer's gift collections (`tg_payments::star_gifts::GiftCollection`).
    pub collections: Vec<tg_payments::star_gifts::GiftCollection>,
}

/// Single audio track saved on a user's profile (`users.getSavedMusic` / `account.saveMusic`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMusicTrack {
    /// TL document identifier.
    pub document_id: i64,
    /// TL document access hash.
    pub access_hash: i64,
    /// TL document file reference bytes.
    pub file_reference: Vec<u8>,
    /// Track title.
    pub title: String,
    /// Artist / performer name.
    pub performer: String,
    /// Track duration in seconds.
    pub duration_secs: u32,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Whether this track is currently saved to the profile.
    pub saved_on_profile: bool,
}

/// Request payload for uploading an emoji + gradient userpic (`videoSizeEmojiMarkup`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiUserpicRequest {
    /// Selected custom emoji / sticker document ID (or deterministic preset ID).
    pub emoji_id: i64,
    /// Unicode emoji glyph used for preview and fallback rendering.
    pub emoji_glyph: String,
    /// 1 to 4 RGB colors (`0xRRGGBB`) for the gradient background (`kMaxColors = 4`).
    pub background_colors: Vec<u32>,
}

/// Allowed reactions mode matching TL `ChatReactions` (`edit_peer_reactions.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAvailableReactionsMode {
    /// Reactions disabled (`chatReactionsNone`).
    None,
    /// All standard reactions enabled (`chatReactionsAll`).
    All {
        /// Whether custom emoji reactions are also permitted (`allow_custom`).
        allow_custom: bool,
    },
    /// Only selected standard/custom reactions enabled (`chatReactionsSome`).
    Some {
        /// Standard Unicode emoji emoticons (e.g. `"👍"`, `"❤️"`).
        emojis: Vec<String>,
        /// Custom emoji document IDs.
        custom_emoji_ids: Vec<i64>,
    },
}

/// Full peer reactions configuration edited by `EditPeerReactionsBox`
/// and saved via `messages.setChatAvailableReactions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerReactionsConfig {
    /// Active `ChatReactions` mode.
    pub mode: ChatAvailableReactionsMode,
    /// Maximum unique reactions per message (`1..=11`, `None` = server default).
    pub reactions_limit: Option<i32>,
    /// Whether paid Star reactions are enabled for this channel.
    pub paid_enabled: bool,
    /// Catalog of selectable standard reactions shown in `EditPeerReactionsBox`.
    pub available_standard: Vec<String>,
}

impl Default for PeerReactionsConfig {
    fn default() -> Self {
        Self {
            mode: ChatAvailableReactionsMode::All { allow_custom: true },
            reactions_limit: Some(11),
            paid_enabled: false,
            available_standard: default_standard_reactions(),
        }
    }
}

/// Standard Telegram reactions list matching `edit_peer_reactions.cpp`.
#[must_use]
pub fn default_standard_reactions() -> Vec<String> {
    [
        "👍", "👎", "❤", "🔥", "🥰", "👏", "😁", "🤔", "🤯", "😱", "🤬", "😢", "🎉", "🤩", "🙏",
        "👌", "🕊", "🤡", "😍", "💯", "⚡", "🏆", "💔", "👀",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// Custom bot verification status for a peer (`bots.setCustomVerification`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerVerificationStatus {
    /// Verifier bot peer ID.
    pub bot_id: PeerId,
    /// Target peer being verified.
    pub peer_id: PeerId,
    /// Whether the peer is currently verified by `bot_id`.
    pub verified: bool,
    /// Optional custom description shown below the badge (max 70 chars).
    pub custom_description: String,
}

/// Paid-message fee revenue returned by `account.getPaidMessagesRevenue`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaidMessagesRevenue {
    /// Total Telegram Stars charged to the user for paid messages (`stars_amount`).
    pub stars_amount: i64,
}

/// Requirement needed to message a user (`RequirementToContact` in `users.getRequirementsToContact`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequirementToContact {
    /// No special requirement (`requirementToContactEmpty`).
    #[default]
    Empty,
    /// Sender must hold an active Telegram Premium subscription (`requirementToContactPremium`).
    Premium,
    /// Sender must pay `stars_amount` Telegram Stars per message (`requirementToContactPaidMessages`).
    PaidMessages {
        /// Cost per message in Telegram Stars.
        stars_amount: i64,
    },
}

/// Contact restriction pair returned for one user by `users.getRequirementsToContact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserContactRequirement {
    /// Target user peer ID.
    pub user_id: PeerId,
    /// Contact requirement returned by the server (missing trailing items default to `Empty`).
    pub requirement: RequirementToContact,
}

/// Sticker or custom-emoji pack reference for `channels.setStickers` / `channels.setEmojiStickers`
/// (`Data::InputStickerSet` in `data/stickers/data_stickers_set.cpp:228-234`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ChannelStickerSetRef {
    /// Clears the channel's assigned pack (`inputStickerSetEmpty`).
    #[default]
    Empty,
    /// References a pack by `(id, access_hash)` (`inputStickerSetID`).
    Id {
        /// Server sticker set ID.
        id: i64,
        /// Server sticker set access hash.
        access_hash: i64,
    },
    /// References a pack by short name (`inputStickerSetShortName`).
    ShortName(String),
}

/// Cached channel/supergroup management flags and settings snapshot (`WP-65` / `WP-69`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelManagementState {
    /// Whether pre-history is hidden from newly joined members (`ChannelFlags::PRE_HISTORY_HIDDEN`).
    pub pre_history_hidden: bool,
    /// Whether the supergroup is a forum (`ChannelFlags::FORUM`).
    pub is_forum: bool,
    /// Whether the forum uses tabbed subsection view (`ChannelFlags::FORUM_TABS`).
    pub forum_tabs: bool,
    /// Whether the user prefers viewing this forum as a flat message stream (`ChannelFlags::VIEW_AS_MESSAGES`).
    pub view_forum_as_messages: bool,
    /// Whether the member list is hidden from regular members (`ChannelFlags::PARTICIPANTS_HIDDEN`).
    pub participants_hidden: bool,
    /// Whether the supergroup has been converted to a gigagroup (`ChannelFlags::GIGAGROUP`).
    pub is_gigagroup: bool,
    /// Number of boosts needed to bypass default group restrictions (`0` = disabled).
    pub boosts_unrestrict: u32,
    /// Number of boosts applied by the current user.
    pub boosts_applied: u32,
    /// Stars charged per message (`0` = free).
    pub stars_per_message: i64,
    /// Whether direct messages are enabled for a broadcast channel (`Channel::broadcast_messages_allowed`).
    pub broadcast_messages_allowed: bool,
    /// Whether channel auto-translation is enabled (`ChannelFlags::AUTO_TRANSLATION`).
    pub autotranslation: bool,
    /// Whether the current user can set group sticker/emoji packs (`ChannelFlags::CAN_SET_STICKERS`).
    pub can_set_stickers: bool,
    /// Assigned group sticker set, if set in this session.
    pub sticker_set: Option<ChannelStickerSetRef>,
    /// Assigned group emoji sticker set, if set in this session.
    pub emoji_set: Option<ChannelStickerSetRef>,
}

/// Group candidate eligible to be linked as a channel discussion group
/// (`channels.getGroupsForDiscussion`, `edit_peer_info_box.cpp:1044-1064`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscussionGroupCandidate {
    /// Candidate group/supergroup peer ID.
    pub peer_id: PeerId,
    /// Group display title.
    pub title: String,
    /// Public `@username` without leading `@`, if any.
    pub username: Option<String>,
    /// Member count (clamped to `>= 0`).
    pub members_count: u32,
    /// Whether this candidate is already a supergroup (vs a basic group that will migrate on link).
    pub is_megagroup: bool,
    /// Whether the candidate has a public username or geolocation.
    pub is_public: bool,
}

/// Summary row of an admin who has created invite links (`messages.getAdminsWithInvites`,
/// `boxes/peers/edit_peer_invite_links.cpp:804-822`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminWithInvitesEntry {
    /// Administrator user peer ID.
    pub admin_id: PeerId,
    /// Display name resolved from `Session` (fallback `"User #<id>"`).
    pub name: String,
    /// Optional public username without leading `@`.
    pub username: Option<String>,
    /// Whether this administrator is the logged-in account (`user->isSelf()`).
    pub is_self: bool,
    /// Number of active invite links created by this admin.
    pub invites_count: u32,
    /// Number of revoked invite links created by this admin.
    pub revoked_invites_count: u32,
}
