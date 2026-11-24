//! Port of `data/data_peer.h`, `data/data_peer.cpp`, `data/data_user.h`,
//! `data/data_user.cpp`, `data/data_chat.h`, `data/data_chat.cpp`,
//! `data/data_channel.h`, `data/data_channel.cpp`, `data/data_user_names.h`,
//! `data/data_user_names.cpp`, and `data/data_message_reaction_id.h`.
//!
//! Implements the peer domain model (`User`, `Chat`, `Channel`, unified `Peer`),
//! flags, usernames, bot info, emoji status, color indices, restrictions,
//! `CanSendAnything`/`CanWrite` permission checks, slow mode, linked chats,
//! and basic-group -> megagroup migration handling.

use std::collections::{BTreeMap, BTreeSet};

use bitflags::bitflags;

use crate::id::{
    COLOR_INDEX_COUNT, ChannelId, ChatId, CollectibleId, DocumentId, MsgId, PeerId, PhotoId,
    TimeId, UNKNOWN_PHOTO_ID, UserId, decide_color_index,
};
use crate::lastseen::{
    LastseenStatus, OnlineStatusText, SET_ONLINE_AFTER_ACTIVITY, sort_by_online_status,
};
use crate::rights::{
    ChatAdminRights, ChatBannedRights, ChatRestrictionsInfo, RestrictionCheckResult,
    SendCheckContext, is_restricted_forever,
};
use crate::text::TextWithEntities;

/// Geographic coordinate pair (`Data::LocationPoint`).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct GeoPoint {
    /// Latitude in degrees.
    pub lat: f64,
    /// Longitude in degrees.
    pub lon: f64,
    /// Optional accuracy radius in meters.
    pub accuracy_radius: Option<i32>,
}

/// Physical location attached to a local megagroup (`ChannelLocation`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelLocation {
    /// Human-readable street address.
    pub address: String,
    /// Coordinates.
    pub point: GeoPoint,
}

/// Reaction identifier: standard emoji, custom emoji document, or paid stars reaction
/// (`Data::ReactionId`).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ReactionId {
    /// Standard Unicode emoji reaction (e.g. `"👍"`).
    Emoji(String),
    /// Custom animated/static emoji reaction backed by a document.
    Custom(DocumentId),
    /// Telegram Stars paid reaction (`ReactionId::Paid()`).
    Paid,
}

impl ReactionId {
    /// Returns `true` if this reaction is empty (invalid empty emoji string).
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Emoji(s) if s.is_empty())
    }

    /// Returns `true` if this is the paid Stars reaction.
    pub const fn is_paid(&self) -> bool {
        matches!(self, Self::Paid)
    }
}

/// Policy for reactions allowed in a chat or channel (`Data::AllowedReactionsType`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum AllowedReactionsType {
    /// All reactions including custom emoji.
    All,
    /// All default Unicode reactions.
    Default,
    /// Only an explicit allow-list (empty list means reactions disabled).
    #[default]
    Some,
}

/// Reaction configuration for a chat or channel (`Data::AllowedReactions`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct AllowedReactions {
    /// Explicit reactions when `kind == AllowedReactionsType::Some`.
    pub some: Vec<ReactionId>,
    /// Maximum unique reactions per message (`0` = use app config default).
    pub max_count: i32,
    /// Policy kind.
    pub kind: AllowedReactionsType,
    /// Whether paid Stars reactions are enabled.
    pub paid_enabled: bool,
}

/// Peer emoji status (`EmojiStatusId` + expiration timestamp).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct EmojiStatus {
    /// Custom emoji document ID (`DocumentId::ZERO` when unset).
    pub document_id: DocumentId,
    /// Optional collectible gift ID if this is a collectible emoji status.
    pub collectible_id: Option<CollectibleId>,
    /// Optional expiration Unix timestamp (`0` = no expiry).
    pub until: TimeId,
}

impl EmojiStatus {
    /// Returns `true` if an emoji status is set.
    pub const fn is_set(&self) -> bool {
        !self.document_id.is_zero() || self.collectible_id.is_some()
    }
}

/// Restriction/unavailability reason from the server (`Data::UnavailableReason`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct UnavailableReason {
    /// Platform tag (`"all"`, `"ios"`, `"ms"`, etc.).
    pub platform: String,
    /// Machine reason code (e.g. `"sensitive"`, `"terms"`, `"porno"`).
    pub reason: String,
    /// Localized explanation text shown to the user.
    pub text: String,
}

impl UnavailableReason {
    /// Returns `true` if this reason represents the `"sensitive"` content mark.
    pub fn is_sensitive(&self) -> bool {
        self.reason == "sensitive"
    }

    /// Computes the first non-sensitive, non-ignored unavailability message
    /// (`Data::UnavailableReason::Compute`).
    pub fn compute<'a>(reasons: &'a [Self], ignored_reasons: &[String]) -> Option<&'a str> {
        reasons
            .iter()
            .find(|r| !r.is_sensitive() && !ignored_reasons.iter().any(|skip| skip == &r.reason))
            .map(|r| r.text.as_str())
    }
}

/// Notification settings for a peer (`Data::PeerNotifySettings`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct PeerNotifySettings {
    /// Mute expiration timestamp (`None` = inherit default, `Some(0)` = unmuted,
    /// `Some(ts)` = muted until `ts`, `Some(i32::MAX)` = muted forever).
    pub mute_until: Option<TimeId>,
    /// Whether silent notifications are forced.
    pub silent: Option<bool>,
    /// Whether message previews are shown in notifications.
    pub show_previews: Option<bool>,
    /// Optional notification sound identifier.
    pub sound: Option<String>,
    /// Whether stories from this peer are muted.
    pub stories_muted: Option<bool>,
}

impl PeerNotifySettings {
    /// Returns `true` if muted at timestamp `now`.
    pub fn is_muted(&self, now: TimeId) -> bool {
        self.mute_until.is_some_and(|until| until > now)
    }
}

/// A single username entry (primary editable or collectible Fragment username)
/// (`Data::Username`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Username {
    /// Username without leading `@`.
    pub username: String,
    /// Whether the username is currently active.
    pub active: bool,
    /// Whether the username is the user/channel's editable (non-collectible) username.
    pub editable: bool,
}

/// Ordered active usernames with tracked editable index (`Data::UsernamesInfo`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct UsernamesInfo {
    usernames: Vec<String>,
    index_editable: Option<usize>,
}

impl UsernamesInfo {
    /// Sets or clears the single editable username (`UsernamesInfo::setUsername`).
    pub fn set_username(&mut self, username: impl Into<String>) {
        let username = username.into();
        match self.index_editable {
            None => {
                if !username.is_empty() {
                    self.index_editable = Some(self.usernames.len());
                    self.usernames.push(username);
                }
            }
            Some(idx) if username.is_empty() => {
                self.usernames.remove(idx);
                self.index_editable = None;
            }
            Some(idx) => {
                self.usernames[idx] = username;
            }
        }
    }

    /// Replaces the full active username list (`UsernamesInfo::setUsernames`).
    pub fn set_usernames(&mut self, list: &[Username]) {
        self.usernames.clear();
        self.index_editable = None;
        for entry in list {
            if entry.active {
                if entry.editable {
                    self.index_editable = Some(self.usernames.len());
                }
                self.usernames.push(entry.username.clone());
            }
        }
    }

    /// Returns the primary display username (first active), or `""` if none.
    pub fn username(&self) -> &str {
        self.usernames.first().map(String::as_str).unwrap_or("")
    }

    /// Returns the editable username, or `""` if none.
    pub fn editable_username(&self) -> &str {
        self.index_editable
            .and_then(|i| self.usernames.get(i))
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Returns all active usernames in server order.
    pub fn usernames(&self) -> &[String] {
        &self.usernames
    }

    /// Returns `true` if `username` matches the editable username.
    pub fn is_editable(&self, username: &str) -> bool {
        self.index_editable
            .and_then(|i| self.usernames.get(i))
            .is_some_and(|u| u == username)
    }
}

/// How completely a peer's data has been loaded (`PeerData::LoadedStatus`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum LoadedStatus {
    /// Placeholder created by ID only.
    #[default]
    Not,
    /// Loaded from a `min` constructor (lacks full access hash / details).
    Minimal,
    /// Loaded from a regular `user`/`chat`/`channel` constructor.
    Normal,
    /// Loaded including `userFull`/`chatFull`/`channelFull`.
    Full,
}

/// Whether a peer is blocked by the current account (`PeerData::BlockStatus`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum BlockStatus {
    /// Not yet fetched.
    #[default]
    Unknown,
    /// Peer is blocked.
    Blocked,
    /// Peer is not blocked.
    NotBlocked,
}

/// Active stories state ring on a peer avatar (`PeerData::StoriesState`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum StoriesState {
    /// Unknown.
    #[default]
    Unknown,
    /// No active stories.
    None,
    /// Has active stories, all read.
    HasRead,
    /// Has unread active stories.
    HasUnread,
    /// Has an active live video stream story.
    HasVideoStream,
}

/// Default tab opened on a peer's profile (`Data::ProfileTab`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ProfileTab {
    /// Default / unset.
    #[default]
    None,
    /// Channel posts.
    Posts,
    /// Star gifts.
    Gifts,
    /// Shared photos & videos.
    Media,
    /// Shared files.
    Files,
    /// Shared music.
    Music,
    /// Shared voice messages.
    Voice,
    /// Shared links.
    Links,
    /// Shared GIFs.
    Gifs,
}

/// Contact relationship state (`UserData::ContactStatus`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ContactStatus {
    /// Unknown.
    #[default]
    Unknown,
    /// User is in our contact list.
    Contact,
    /// User is not in our contact list.
    NotContact,
}

/// Voice/video calls availability (`UserData::CallsStatus`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum CallsStatus {
    /// Unknown.
    #[default]
    Unknown,
    /// Calls enabled.
    Enabled,
    /// Calls disabled.
    Disabled,
    /// Calls restricted by peer's privacy settings.
    Private,
}

/// Whether a group has bots (`Data::BotStatus`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum BotStatus {
    /// Participants not yet inspected.
    #[default]
    Unknown,
    /// Confirmed no bots in group.
    NoBots,
    /// At least one bot in group.
    HasBots,
}

/// User birthday (`Data::Birthday`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Birthday {
    /// Day of month (`1..=31`).
    pub day: u8,
    /// Month of year (`1..=12`).
    pub month: u8,
    /// Optional birth year (`0` if omitted).
    pub year: u16,
}

/// User Stars rating (`Data::StarsRating`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StarsRating {
    /// Current rating level.
    pub level: i32,
    /// Total stars score.
    pub stars: i32,
    /// Threshold for current level.
    pub this_level_stars: i32,
    /// Threshold for next level.
    pub next_level_stars: i32,
}

/// Third-party bot verification details (`Ui::BotVerifyDetails`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct BotVerifyDetails {
    /// Bot peer ID that issued verification.
    pub bot_id: UserId,
    /// Custom emoji icon document ID.
    pub icon_id: DocumentId,
    /// Explanation text shown in profile.
    pub description: String,
}

/// Bot slash command (`Data::BotCommand`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct BotCommand {
    /// Command name without leading `/`.
    pub command: String,
    /// Short help description.
    pub description: String,
}

/// Bot metadata attached to a `User` when `is_bot()` is true (`BotInfo`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BotInfo {
    /// Bot description shown before `/start`.
    pub description: String,
    /// Inline query placeholder text.
    pub inline_placeholder: String,
    /// Slash commands registered by the bot.
    pub commands: Vec<BotCommand>,
    /// Optional description photo ID.
    pub photo_id: Option<PhotoId>,
    /// Optional description animation/video document ID.
    pub document_id: Option<DocumentId>,
    /// Web-app menu button label.
    pub menu_button_text: String,
    /// Web-app menu button URL.
    pub menu_button_url: String,
    /// Privacy policy URL.
    pub privacy_policy_url: String,
    /// Start deep-link token.
    pub start_token: String,
    /// Suggested admin rights when adding bot to a group.
    pub group_admin_rights: ChatAdminRights,
    /// Suggested admin rights when adding bot to a channel.
    pub channel_admin_rights: ChatAdminRights,
    /// Bot info version (`< 0` marks non-bot).
    pub version: i32,
    /// Active monthly users count (shown as bot status text when > 0).
    pub active_users: i32,
    /// Whether full bot info has been loaded.
    pub inited: bool,
    /// Bot can read all group messages (privacy mode off).
    pub reads_all_history: bool,
    /// Bot cannot be added to groups.
    pub cant_join_groups: bool,
    /// Bot supports attachment menu.
    pub supports_attach_menu: bool,
    /// Current user can edit bot info.
    pub can_edit_information: bool,
    /// Bot supports Telegram Business connections.
    pub supports_business: bool,
    /// Bot has a main mini app.
    pub has_main_app: bool,
    /// Bot allows users to create forum topics.
    pub user_creates_topics: bool,
    /// Bot can manage child bots.
    pub can_manage_bots: bool,
}

bitflags! {
    /// Peer action bar flags (`PeerBarSetting` in `data_peer.h`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct PeerBarSettings: u32 {
        /// Show "Report Spam" button.
        const REPORT_SPAM             = 1 << 0;
        /// Show "Add Contact" button.
        const ADD_CONTACT             = 1 << 1;
        /// Show "Block Contact" button.
        const BLOCK_CONTACT           = 1 << 2;
        /// Show "Share Contact" button.
        const SHARE_CONTACT           = 1 << 3;
        /// Needs contacts exception to show phone.
        const NEED_CONTACTS_EXCEPTION = 1 << 4;
        /// Conversation was auto-archived.
        const AUTO_ARCHIVED           = 1 << 5;
        /// Chat joined via join request banner.
        const REQUEST_CHAT            = 1 << 6;
        /// Request chat is a broadcast channel.
        const REQUEST_CHAT_BROADCAST  = 1 << 7;
        /// Business bot connected to this chat.
        const HAS_BUSINESS_BOT        = 1 << 8;
        /// Connected business bot is paused.
        const BUSINESS_BOT_PAUSED     = 1 << 9;
        /// Connected business bot can reply.
        const BUSINESS_BOT_CAN_REPLY  = 1 << 10;
        /// Settings have not been loaded yet.
        const UNKNOWN                 = 1 << 11;
    }
}

bitflags! {
    /// Flags on `User` (`UserDataFlag` in `data_user.h`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct UserFlags: u32 {
        /// User is a contact.
        const CONTACT                         = 1 << 0;
        /// User is a mutual contact.
        const MUTUAL_CONTACT                  = 1 << 1;
        /// Deleted / inaccessible account.
        const DELETED                         = 1 << 2;
        /// Verified account.
        const VERIFIED                        = 1 << 3;
        /// Marked as scam.
        const SCAM                            = 1 << 4;
        /// Marked as fake.
        const FAKE                            = 1 << 5;
        /// Inline bot requests geolocation.
        const BOT_INLINE_GEO                  = 1 << 6;
        /// Blocked by current account.
        const BLOCKED                         = 1 << 7;
        /// Supports phone calls.
        const HAS_PHONE_CALLS                 = 1 << 8;
        /// Disallows calls due to privacy settings.
        const PHONE_CALLS_PRIVATE             = 1 << 9;
        /// Official Telegram support account.
        const SUPPORT                         = 1 << 10;
        /// Can pin messages in this DM.
        const CAN_PIN_MESSAGES                = 1 << 11;
        /// Ignore `min` user profile photo.
        const DISCARD_MIN_PHOTO               = 1 << 12;
        /// This user is the currently logged-in account (`Self`).
        const SELF                            = 1 << 13;
        /// User has Telegram Premium.
        const PREMIUM                         = 1 << 14;
        /// Unofficial client security warning.
        const UNOFFICIAL_SECURITY_RISK        = 1 << 15;
        /// Voice and video messages are forbidden by this user's privacy settings.
        const VOICE_MESSAGES_FORBIDDEN        = 1 << 16;
        /// Has a personal contact photo set by us.
        const PERSONAL_PHOTO                  = 1 << 17;
        /// Stories hidden in archive.
        const STORIES_HIDDEN                  = 1 << 18;
        /// Has active stories.
        const HAS_ACTIVE_STORIES              = 1 << 19;
        /// Has unread active stories.
        const HAS_UNREAD_STORIES              = 1 << 20;
        /// Writing to this user requires Telegram Premium.
        const REQUIRES_PREMIUM_TO_WRITE       = 1 << 21;
        /// User's privacy settings include require-premium-to-write.
        const HAS_REQUIRE_PREMIUM_TO_WRITE    = 1 << 22;
        /// User charges Telegram Stars per message.
        const HAS_STARS_PER_MESSAGE           = 1 << 23;
        /// Money/premium write restrictions have been checked.
        const MESSAGE_MONEY_RESTRICTIONS_KNOWN = 1 << 24;
        /// Read dates hidden by privacy settings.
        const READ_DATES_PRIVATE              = 1 << 25;
        /// Stories correspondent flag.
        const STORIES_CORRESPONDENT           = 1 << 26;
        /// Bot supports forum topics.
        const FORUM                           = 1 << 27;
        /// Has an active live video stream.
        const HAS_ACTIVE_VIDEO_STREAM         = 1 << 28;
        /// We enabled no-forwards protection in this DM.
        const NO_FORWARDS_MY_ENABLED          = 1 << 29;
        /// Peer enabled no-forwards protection in this DM.
        const NO_FORWARDS_PEER_ENABLED        = 1 << 30;
        /// User is in our close friends list.
        const CLOSE_FRIEND                    = 1 << 31;
    }
}

bitflags! {
    /// Flags on basic group `Chat` (`ChatDataFlag` in `data_chat.h`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct ChatFlags: u32 {
        /// Current account has left this group.
        const LEFT                 = 1 << 0;
        /// Current account is the group creator.
        const CREATOR              = 1 << 2;
        /// Group was deactivated (typically after migration to a megagroup).
        const DEACTIVATED          = 1 << 3;
        /// Group is forbidden / inaccessible.
        const FORBIDDEN            = 1 << 4;
        /// Group call is active.
        const CALL_ACTIVE          = 1 << 5;
        /// Group call has active participants.
        const CALL_NOT_EMPTY       = 1 << 6;
        /// Current account can set group username.
        const CAN_SET_USERNAME     = 1 << 7;
        /// Forwarding / saving content from this group is forbidden.
        const NO_FORWARDS          = 1 << 8;
        /// Group has welcome messages configured.
        const HAS_WELCOME_MESSAGES = 1 << 9;
    }
}

bitflags! {
    /// Flags on `Channel` (`ChannelDataFlag` in `data_channel.h`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct ChannelFlags: u64 {
        /// Current account has left (is not a member of) this channel.
        const LEFT                    = 1 << 0;
        /// Current account is the channel creator.
        const CREATOR                 = 1 << 1;
        /// Channel is forbidden / banned.
        const FORBIDDEN               = 1 << 2;
        /// Group call / live stream is active.
        const CALL_ACTIVE             = 1 << 3;
        /// Group call / live stream is non-empty.
        const CALL_NOT_EMPTY          = 1 << 4;
        /// Broadcast channel appends author signatures to posts.
        const SIGNATURES              = 1 << 5;
        /// Verified channel.
        const VERIFIED                = 1 << 6;
        /// Marked as scam.
        const SCAM                    = 1 << 7;
        /// Marked as fake.
        const FAKE                    = 1 << 8;
        /// Channel is a supergroup (megagroup).
        const MEGAGROUP               = 1 << 9;
        /// Channel is a broadcast channel.
        const BROADCAST               = 1 << 10;
        /// Channel is a gigagroup (broadcast-style huge group).
        const GIGAGROUP               = 1 << 11;
        /// Channel has a public username.
        const USERNAME                = 1 << 12;
        /// Channel has a public geolocation.
        const LOCATION                = 1 << 13;
        /// Current account can set the channel username.
        const CAN_SET_USERNAME        = 1 << 14;
        /// Current account can set the group sticker set.
        const CAN_SET_STICKERS        = 1 << 15;
        /// New members cannot see message history before joining.
        const PRE_HISTORY_HIDDEN      = 1 << 16;
        /// Current account can view participant list.
        const CAN_VIEW_PARTICIPANTS   = 1 << 17;
        /// Channel has a linked discussion group / broadcast channel.
        const HAS_LINK                = 1 << 18;
        /// Slow mode is enabled.
        const SLOWMODE_ENABLED        = 1 << 19;
        /// Forwarding / saving content is forbidden.
        const NO_FORWARDS             = 1 << 20;
        /// Users must join before writing (linked discussion group).
        const JOIN_TO_WRITE           = 1 << 21;
        /// Joining requires administrator approval.
        const REQUEST_TO_JOIN         = 1 << 22;
        /// Supergroup is a forum with topics.
        const FORUM                   = 1 << 23;
        /// Aggressive anti-spam is enabled.
        const ANTI_SPAM               = 1 << 24;
        /// Participant list is hidden from regular members.
        const PARTICIPANTS_HIDDEN     = 1 << 25;
        /// Stories hidden in archive.
        const STORIES_HIDDEN          = 1 << 26;
        /// Has active stories.
        const HAS_ACTIVE_STORIES      = 1 << 27;
        /// Has unread active stories.
        const HAS_UNREAD_STORIES      = 1 << 28;
        /// Current account can view channel statistics.
        const CAN_GET_STATISTICS      = 1 << 29;
        /// User prefers viewing forum as flat messages.
        const VIEW_AS_MESSAGES        = 1 << 30;
        /// Similar channels drawer expanded.
        const SIMILAR_EXPANDED        = 1 << 31;
        /// Can view ad revenue statistics.
        const CAN_VIEW_REVENUE        = 1 << 32;
        /// Paid media posting is allowed.
        const PAID_MEDIA_ALLOWED      = 1 << 33;
        /// Can view Stars revenue statistics.
        const CAN_VIEW_CREDITS_REVENUE = 1 << 34;
        /// Post signatures show author profiles.
        const SIGNATURE_PROFILES      = 1 << 35;
        /// Channel accepts Star gifts.
        const STARGIFTS_AVAILABLE     = 1 << 36;
        /// Paid messages feature available.
        const PAID_MESSAGES_AVAILABLE = 1 << 37;
        /// Auto-translation enabled for channel posts.
        const AUTO_TRANSLATION        = 1 << 38;
        /// Channel is a monoforum (channel direct messages).
        const MONOFORUM               = 1 << 39;
        /// Current account is an admin of this monoforum.
        const MONOFORUM_ADMIN         = 1 << 40;
        /// Monoforum direct messages are disabled.
        const MONOFORUM_DISABLED      = 1 << 41;
        /// Forum uses tabbed subsections UI.
        const FORUM_TABS              = 1 << 42;
        /// Channel charges Stars per message.
        const HAS_STARS_PER_MESSAGE   = 1 << 43;
        /// Stars-per-message setting has been loaded.
        const STARS_PER_MESSAGE_KNOWN = 1 << 44;
        /// Has active live video stream.
        const HAS_ACTIVE_VIDEO_STREAM = 1 << 45;
        /// Channel is a community hub.
        const COMMUNITY               = 1 << 46;
        /// Community hub is collapsed in dialogs list.
        const COMMUNITY_COLLAPSED     = 1 << 47;
        /// Group has welcome messages enabled.
        const HAS_WELCOME_MESSAGES    = 1 << 48;
    }
}

/// Common fields shared by every peer (`PeerData` base class fields).
#[derive(Clone, Debug, PartialEq)]
pub struct PeerCommon {
    /// Unified shifted peer ID.
    pub id: PeerId,
    /// Display name.
    pub name: String,
    /// Monotonically increasing version bumped on name changes.
    pub name_version: u16,
    /// Tokenized lowercase words for dialog search indexing.
    pub name_words: BTreeSet<String>,
    /// First characters of `name_words` for indexed-list lookup.
    pub name_first_letters: BTreeSet<char>,
    /// Peer bio / description (`about`).
    pub about: String,
    /// Profile photo ID (`UNKNOWN_PHOTO_ID` if unknown, `PhotoId::ZERO` if none).
    pub userpic_photo_id: PhotoId,
    /// Whether the profile photo has an animated video avatar.
    pub userpic_has_video: bool,
    /// Accent color index (`0..64`), initialized via `decide_color_index(id)`.
    pub color_index: u8,
    /// Whether `color_index` came from cloud peer color (vs default derived from ID).
    pub color_index_cloud: bool,
    /// Optional profile header color index (`0..64`).
    pub color_profile_index: Option<u8>,
    /// Background custom emoji ID for reply/quote accent.
    pub background_emoji_id: DocumentId,
    /// Background custom emoji ID for profile header.
    pub profile_background_emoji_id: DocumentId,
    /// Current emoji status.
    pub emoji_status: EmojiStatus,
    /// Notification settings.
    pub notify: PeerNotifySettings,
    /// Action bar flags.
    pub bar_settings: PeerBarSettings,
    /// Block status.
    pub block_status: BlockStatus,
    /// Loaded status.
    pub loaded_status: LoadedStatus,
    /// Auto-delete TTL period in seconds (`0` = disabled).
    pub messages_ttl: TimeId,
    /// Chat theme token (emoji or theme slug).
    pub theme_token: String,
    /// Main profile tab.
    pub main_profile_tab: ProfileTab,
    /// Server unavailability / restriction reasons.
    pub unavailable_reasons: Vec<UnavailableReason>,
    /// Whether the peer is marked as sensitive content.
    pub sensitive_content: bool,
}

impl PeerCommon {
    /// Creates default `PeerCommon` for `id`, deriving the default color index.
    pub fn new(id: PeerId) -> Self {
        Self {
            id,
            name: String::new(),
            name_version: 1,
            name_words: BTreeSet::new(),
            name_first_letters: BTreeSet::new(),
            about: String::new(),
            userpic_photo_id: UNKNOWN_PHOTO_ID,
            userpic_has_video: false,
            color_index: decide_color_index(id),
            color_index_cloud: false,
            color_profile_index: None,
            background_emoji_id: DocumentId::ZERO,
            profile_background_emoji_id: DocumentId::ZERO,
            emoji_status: EmojiStatus::default(),
            notify: PeerNotifySettings::default(),
            bar_settings: PeerBarSettings::UNKNOWN,
            block_status: BlockStatus::Unknown,
            loaded_status: LoadedStatus::Not,
            messages_ttl: 0,
            theme_token: String::new(),
            main_profile_tab: ProfileTab::None,
            unavailable_reasons: Vec::new(),
            sensitive_content: false,
        }
    }

    /// Returns `true` if at least minimal (`min` constructor) data has been loaded
    /// (`PeerData::isMinimalLoaded`).
    #[inline]
    pub const fn is_minimal_loaded(&self) -> bool {
        !matches!(self.loaded_status, LoadedStatus::Not)
    }

    /// Returns `true` if full non-`min` peer metadata has been loaded (`PeerData::isLoaded`).
    #[inline]
    pub const fn is_loaded(&self) -> bool {
        matches!(
            self.loaded_status,
            LoadedStatus::Normal | LoadedStatus::Full
        )
    }

    /// Returns `true` if `userFull`/`chatFull`/`channelFull` has been loaded (`PeerData::isFullLoaded`).
    #[inline]
    pub const fn is_full_loaded(&self) -> bool {
        matches!(self.loaded_status, LoadedStatus::Full)
    }

    /// Sets a cloud peer color index (`PeerData::changeColorIndex`).
    pub fn set_color_index(&mut self, index: u8) -> bool {
        let index = index % COLOR_INDEX_COUNT;
        if self.color_index_cloud && self.color_index == index {
            return false;
        }
        self.color_index_cloud = true;
        self.color_index = index;
        true
    }

    /// Resets peer color index back to the ID-derived default (`PeerData::clearColorIndex`).
    pub fn clear_color_index(&mut self) -> bool {
        if !self.color_index_cloud {
            return false;
        }
        self.color_index_cloud = false;
        self.color_index = decide_color_index(self.id);
        true
    }

    /// Sets profile color index (`PeerData::changeColorProfileIndex`).
    pub fn set_color_profile_index(&mut self, index: u8) -> bool {
        let index = index % COLOR_INDEX_COUNT;
        if self.color_profile_index == Some(index) {
            return false;
        }
        self.color_profile_index = Some(index);
        true
    }

    /// Clears profile color index.
    pub fn clear_color_profile_index(&mut self) -> bool {
        self.color_profile_index.take().is_some()
    }

    /// Updates `unavailable_reasons` and extracts the `sensitive` flag
    /// (`PeerData::setUnavailableReasons`).
    pub fn set_unavailable_reasons(&mut self, mut reasons: Vec<UnavailableReason>) -> bool {
        let mut sensitive = false;
        reasons.retain(|r| {
            if r.is_sensitive() {
                sensitive = true;
                false
            } else {
                true
            }
        });
        let changed = self.sensitive_content != sensitive || self.unavailable_reasons != reasons;
        self.sensitive_content = sensitive;
        self.unavailable_reasons = reasons;
        changed
    }

    /// Recomputes `name_words` and `name_first_letters` from `name` and extra tokens (usernames/phone).
    pub fn rebuild_name_index(&mut self, extra_tokens: &[&str]) {
        self.name_words.clear();
        self.name_first_letters.clear();
        for source in std::iter::once(self.name.as_str()).chain(extra_tokens.iter().copied()) {
            for word in split_search_words(source) {
                if let Some(ch) = word.chars().next() {
                    self.name_first_letters.insert(ch);
                }
                self.name_words.insert(word);
            }
        }
    }
}

/// Splits text into lowercase alphanumeric words for peer/dialog prefix indexing.
pub(crate) fn split_search_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                current.push(lower);
            }
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// User peer model (`UserData`).
#[derive(Clone, Debug, PartialEq)]
pub struct User {
    /// Common peer fields.
    pub common: PeerCommon,
    /// User flags.
    pub flags: UserFlags,
    /// Server access hash (`u64::MAX` marks legacy inaccessible/deleted account).
    pub access_hash: u64,
    /// First name.
    pub first_name: String,
    /// Last name.
    pub last_name: String,
    /// Display name or formatted phone fallback.
    pub name_or_phone: String,
    /// Phone number (if visible).
    pub phone: String,
    /// Active usernames (primary + collectible).
    pub usernames: UsernamesInfo,
    /// Last-seen / online status.
    pub lastseen: LastseenStatus,
    /// Contact relationship status.
    pub contact_status: ContactStatus,
    /// Calls status.
    pub calls_status: CallsStatus,
    /// Optional birthday.
    pub birthday: Option<Birthday>,
    /// Number of groups in common with current account.
    pub common_chats_count: i32,
    /// Number of gifts displayed on profile.
    pub peer_gifts_count: i32,
    /// Stars charged per incoming message (`0` if free).
    pub stars_per_message: i32,
    /// Stars rating level/progress.
    pub stars_rating: StarsRating,
    /// Optional linked personal channel.
    pub personal_channel_id: Option<ChannelId>,
    /// Optional pinned message ID in personal channel.
    pub personal_channel_message_id: MsgId,
    /// Optional linked community channel.
    pub linked_community_id: Option<ChannelId>,
    /// Manager user ID if this is a managed bot.
    pub bot_manager_id: Option<UserId>,
    /// Optional third-party bot verification details.
    pub bot_verify_details: Option<BotVerifyDetails>,
    /// Private contact note.
    pub note: TextWithEntities,
    /// Bot metadata when this user is a bot.
    pub bot_info: Option<BotInfo>,
}

impl User {
    /// Legacy sentinel access hash indicating an inaccessible/deleted user
    /// (`UserData::kInaccessibleAccessHashOld`).
    pub const INACCESSIBLE_ACCESS_HASH_OLD: u64 = 0xFFFF_FFFF_FFFF_FFFF;

    /// Creates a new `User` shell for `user_id`.
    pub fn new(user_id: UserId, is_self: bool) -> Self {
        let peer_id = PeerId::from_user(user_id);
        let flags = if is_self {
            UserFlags::SELF
        } else {
            UserFlags::empty()
        };
        Self {
            common: PeerCommon::new(peer_id),
            flags,
            access_hash: 0,
            first_name: String::new(),
            last_name: String::new(),
            name_or_phone: String::new(),
            phone: String::new(),
            usernames: UsernamesInfo::default(),
            lastseen: LastseenStatus::default(),
            contact_status: ContactStatus::Unknown,
            calls_status: CallsStatus::Unknown,
            birthday: None,
            common_chats_count: 0,
            peer_gifts_count: 0,
            stars_per_message: 0,
            stars_rating: StarsRating::default(),
            personal_channel_id: None,
            personal_channel_message_id: MsgId::ZERO,
            linked_community_id: None,
            bot_manager_id: None,
            bot_verify_details: None,
            note: TextWithEntities::default(),
            bot_info: None,
        }
    }

    /// Returns the bare `UserId`.
    #[inline]
    pub const fn user_id(&self) -> UserId {
        self.common.id.to_user_or_zero()
    }

    /// Sets access hash, marking `UserFlags::DELETED` if it equals `INACCESSIBLE_ACCESS_HASH_OLD`.
    pub fn set_access_hash(&mut self, access_hash: u64) {
        if access_hash == Self::INACCESSIBLE_ACCESS_HASH_OLD {
            self.access_hash = 0;
            self.flags.insert(UserFlags::DELETED);
        } else {
            self.access_hash = access_hash;
        }
    }

    /// Updates user name and rebuilds search index (`UserData::setName`).
    pub fn set_name(
        &mut self,
        first_name: impl Into<String>,
        last_name: impl Into<String>,
        name_or_phone: impl Into<String>,
        username: impl Into<String>,
    ) {
        let new_first = first_name.into();
        let new_last = last_name.into();
        let change_name = !new_first.is_empty() || !new_last.is_empty();
        if change_name && new_first.trim().is_empty() {
            self.first_name = new_last;
            self.last_name.clear();
        } else if change_name {
            self.first_name = new_first;
            self.last_name = new_last;
        }
        let full_name = if self.last_name.is_empty() {
            self.first_name.clone()
        } else if self.first_name.is_empty() {
            self.last_name.clone()
        } else {
            format!("{} {}", self.first_name, self.last_name)
        };
        let new_name_or_phone = name_or_phone.into();
        let new_username = username.into();
        if self.common.name != full_name {
            self.common.name = full_name;
            self.common.name_version = self.common.name_version.wrapping_add(1);
        }
        self.name_or_phone = new_name_or_phone;
        if self.usernames.editable_username() != new_username {
            self.usernames.set_username(new_username);
        }
        self.rebuild_name_index();
    }

    /// Rebuilds the peer's search index from full name, phone, and active usernames.
    pub fn rebuild_name_index(&mut self) {
        let mut extra: Vec<&str> = self
            .usernames
            .usernames()
            .iter()
            .map(String::as_str)
            .collect();
        if !self.phone.is_empty() {
            extra.push(self.phone.as_str());
        }
        self.common.rebuild_name_index(&extra);
    }

    /// Returns `true` if this user is a bot (`bot_info.is_some()`).
    #[inline]
    pub const fn is_bot(&self) -> bool {
        self.bot_info.is_some()
    }

    /// Returns `true` if this user is the currently logged-in account.
    #[inline]
    pub const fn is_self(&self) -> bool {
        self.flags.contains(UserFlags::SELF)
    }

    /// Returns `true` if the user is deleted / inaccessible (`UserData::isInaccessible`).
    #[inline]
    pub const fn is_inaccessible(&self) -> bool {
        self.flags.contains(UserFlags::DELETED)
    }

    /// Returns `true` if the user is in the current account's contact list.
    #[inline]
    pub const fn is_contact(&self) -> bool {
        matches!(self.contact_status, ContactStatus::Contact)
            || self.flags.contains(UserFlags::CONTACT)
    }

    /// Returns `true` if the user is verified.
    #[inline]
    pub const fn is_verified(&self) -> bool {
        self.flags.contains(UserFlags::VERIFIED)
    }

    /// Returns `true` if the user is marked scam.
    #[inline]
    pub const fn is_scam(&self) -> bool {
        self.flags.contains(UserFlags::SCAM)
    }

    /// Returns `true` if the user is marked fake.
    #[inline]
    pub const fn is_fake(&self) -> bool {
        self.flags.contains(UserFlags::FAKE)
    }

    /// Returns `true` if the user has Telegram Premium.
    #[inline]
    pub const fn is_premium(&self) -> bool {
        self.flags.contains(UserFlags::PREMIUM)
    }

    /// Returns `true` if this is official Telegram support.
    #[inline]
    pub const fn is_support(&self) -> bool {
        self.flags.contains(UserFlags::SUPPORT)
    }

    /// Returns `true` if this is a Telegram notifications user (`333000` or `777000`).
    #[inline]
    pub const fn is_notifications_user(&self) -> bool {
        self.common.id.0 == PeerId::LEGACY_NOTIFICATIONS.0
            || self.common.id.0 == PeerId::SERVICE_NOTIFICATIONS.0
    }

    /// Returns `true` if this is a service user (`id % 1000 == 0`).
    #[inline]
    pub const fn is_service_user(&self) -> bool {
        self.common.id.bare() != 0 && self.common.id.bare().is_multiple_of(1000)
    }

    /// Returns `true` if this is the Replies chat bot.
    #[inline]
    pub const fn is_replies_chat(&self, is_test_environment: bool) -> bool {
        let expected = if is_test_environment {
            PeerId::REPLIES_CHAT_TEST
        } else {
            PeerId::REPLIES_CHAT_PRODUCTION
        };
        self.common.id.0 == expected.0
    }

    /// Returns `true` if this is the verification codes user (`489000`).
    #[inline]
    pub const fn is_verify_codes(&self) -> bool {
        self.common.id.0 == PeerId::VERIFY_CODES.0
    }

    /// Returns `true` if this is `@spambot` (`PeerData::isFreezeAppealChat`).
    pub fn is_freeze_appeal_chat(&self) -> bool {
        self.usernames.username().eq_ignore_ascii_case("spambot")
    }

    /// Returns `true` if forwarding messages from this DM is allowed (`UserData::allowsForwarding`).
    #[inline]
    pub const fn allows_forwarding(&self) -> bool {
        !self.flags.contains(UserFlags::NO_FORWARDS_MY_ENABLED)
            && !self.flags.contains(UserFlags::NO_FORWARDS_PEER_ENABLED)
    }

    /// Returns `true` if writing to this user requires Telegram Premium.
    #[inline]
    pub const fn requires_premium_to_write(&self) -> bool {
        self.flags.contains(UserFlags::REQUIRES_PREMIUM_TO_WRITE)
    }

    /// Returns the short display name (`first_name` if non-empty, else `last_name`).
    pub fn short_name(&self) -> &str {
        if !self.first_name.is_empty() {
            &self.first_name
        } else if !self.last_name.is_empty() {
            &self.last_name
        } else {
            &self.common.name
        }
    }

    /// Records observed user activity as a 30-second pseudo-online status (`UserData::madeAction`).
    ///
    /// Returns `true` if `lastseen` changed.
    pub fn made_action(&mut self, when: TimeId) -> bool {
        if self.is_bot() || self.is_service_user() || when <= 0 {
            return false;
        }
        let till = self.lastseen.online_till();
        if till < when + 1 {
            let local = till == 0 || self.lastseen.is_local_online_value();
            let next = LastseenStatus::online_till_status(
                when.saturating_add(SET_ONLINE_AFTER_ACTIVITY),
                local,
                self.lastseen.is_hidden_by_me(),
            );
            if self.lastseen != next {
                self.lastseen = next;
                return true;
            }
        }
        false
    }

    /// Returns whether this user is currently considered online at `now` (`Data::OnlineTextActive`).
    #[inline]
    pub const fn is_online(&self, now: TimeId) -> bool {
        !self.is_service_user() && !self.is_bot() && self.lastseen.is_online(now)
    }

    /// Computes sort key for member lists sorted by online status (`Data::SortByOnlineValue`).
    #[inline]
    pub const fn sort_by_online_value(&self, now: TimeId) -> TimeId {
        if self.is_service_user() || self.is_bot() {
            -1
        } else {
            sort_by_online_status(self.lastseen, now)
        }
    }

    /// Formats the user's status string (special service/support/bot labels or last-seen text)
    /// (`Data::OnlineText` in `data_peer_values.cpp`).
    pub fn online_text(&self, now: TimeId, tz_offset_secs: i32) -> String {
        if let Some(special) = self.special_status_text() {
            return special;
        }
        OnlineStatusText::classify(self.lastseen, now, tz_offset_secs)
            .format_english(tz_offset_secs)
    }

    /// Formats the user's full status string (no relative minutes/hours) (`Data::OnlineTextFull`).
    pub fn online_text_full(&self, now: TimeId, tz_offset_secs: i32) -> String {
        if let Some(special) = self.special_status_text() {
            return special;
        }
        OnlineStatusText::classify_full(self.lastseen, now, tz_offset_secs)
            .format_english(tz_offset_secs)
    }

    fn special_status_text(&self) -> Option<String> {
        if self.is_notifications_user() {
            Some("service notifications".to_owned())
        } else if self.is_support() {
            Some("support".to_owned())
        } else if let Some(bot) = &self.bot_info {
            if bot.active_users > 0 {
                Some(format!("{} monthly users", bot.active_users))
            } else {
                Some(bot_status_text(self.usernames.username()).to_owned())
            }
        } else if self.is_service_user() {
            Some("support".to_owned())
        } else {
            None
        }
    }
}

/// Returns the bot subtitle matching username suffix rules in `Data::BotStatusText`.
fn bot_status_text(primary_username: &str) -> &'static str {
    let lower = primary_username.to_ascii_lowercase();
    if lower.ends_with("app") {
        "mini app"
    } else if lower.ends_with("agent") {
        "agent"
    } else if lower.ends_with("ai") {
        "AI"
    } else if lower.ends_with("game") {
        "game"
    } else {
        "bot"
    }
}

/// Result of checking a basic-chat participant update's version against the local chat version
/// (`ChatData::UpdateStatus` in `data/data_chat.h` / `ChatData::applyUpdateVersion` in
/// `data/data_chat.cpp:191-201`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ChatUpdateVersionStatus {
    /// Update version is older than the already-applied chat version; ignore it.
    TooOld,
    /// One or more intermediate versions were missed; participants invalidated, full chat needed.
    Skipped,
    /// Version is contiguous (`version <= current + 1`) and has been applied.
    Good,
}

/// Basic group chat peer model (`ChatData`).
#[derive(Clone, Debug, PartialEq)]
pub struct Chat {
    /// Common peer fields.
    pub common: PeerCommon,
    /// Chat flags.
    pub flags: ChatFlags,
    /// Member count.
    pub count: i32,
    /// Creation timestamp.
    pub date: TimeId,
    /// Creator user ID.
    pub creator: UserId,
    /// Participant update version.
    pub version: i32,
    /// Default banned rights for non-admin members.
    pub default_restrictions: ChatBannedRights,
    /// Current account's admin rights in this group.
    pub admin_rights: ChatAdminRights,
    /// Permanent invite link (if known).
    pub invite_link: String,
    /// Pending join request count.
    pub pending_requests_count: i32,
    /// Recent join requester user IDs.
    pub recent_requesters: Vec<UserId>,
    /// Allowed reactions configuration.
    pub allowed_reactions: AllowedReactions,
    /// Loaded participant user IDs.
    pub participants: BTreeSet<UserId>,
    /// Loaded admin user IDs.
    pub admins: BTreeSet<UserId>,
    /// Custom member rank titles.
    pub member_ranks: BTreeMap<UserId, String>,
    /// Bot presence status.
    pub bot_status: BotStatus,
    /// Target megagroup `ChannelId` if this basic group was migrated.
    pub migrated_to: Option<ChannelId>,
}

impl Chat {
    /// Creates a new `Chat` shell for `chat_id`.
    pub fn new(chat_id: ChatId) -> Self {
        Self {
            common: PeerCommon::new(PeerId::from_chat(chat_id)),
            flags: ChatFlags::empty(),
            count: 0,
            date: 0,
            creator: UserId::ZERO,
            version: 0,
            default_restrictions: ChatBannedRights::empty(),
            admin_rights: ChatAdminRights::empty(),
            invite_link: String::new(),
            pending_requests_count: 0,
            recent_requesters: Vec::new(),
            allowed_reactions: AllowedReactions::default(),
            participants: BTreeSet::new(),
            admins: BTreeSet::new(),
            member_ranks: BTreeMap::new(),
            bot_status: BotStatus::Unknown,
            migrated_to: None,
        }
    }

    /// Returns the bare `ChatId`.
    #[inline]
    pub const fn chat_id(&self) -> ChatId {
        self.common.id.to_chat_or_zero()
    }

    /// Sets the chat title and rebuilds the search index.
    pub fn set_name(&mut self, name: impl Into<String>) {
        let name = name.into();
        if !name.is_empty() && self.common.name != name {
            self.common.name = name;
            self.common.name_version = self.common.name_version.wrapping_add(1);
            self.common.rebuild_name_index(&[]);
        }
    }

    /// Returns `true` if this chat is forbidden / inaccessible.
    #[inline]
    pub const fn is_forbidden(&self) -> bool {
        self.flags.contains(ChatFlags::FORBIDDEN)
    }

    /// Returns `true` if the current account has left this chat.
    #[inline]
    pub const fn have_left(&self) -> bool {
        self.flags.contains(ChatFlags::LEFT)
    }

    /// Returns `true` if this chat is deactivated.
    #[inline]
    pub const fn is_deactivated(&self) -> bool {
        self.flags.contains(ChatFlags::DEACTIVATED)
    }

    /// Returns `true` if the current account is currently an active member (`ChatData::amIn`).
    #[inline]
    pub const fn am_in(&self) -> bool {
        !self.is_forbidden() && !self.is_deactivated() && !self.have_left()
    }

    /// Returns `true` if the current account is the group creator.
    #[inline]
    pub const fn am_creator(&self) -> bool {
        self.flags.contains(ChatFlags::CREATOR)
    }

    /// Returns `true` if the current account holds any admin right.
    #[inline]
    pub const fn has_admin_rights(&self) -> bool {
        !self.admin_rights.is_empty()
    }

    /// Returns `true` if this basic group has been migrated to a megagroup.
    #[inline]
    pub const fn is_migrated(&self) -> bool {
        self.migrated_to.is_some()
    }

    /// Returns `true` if forwarding content from this chat is allowed.
    #[inline]
    pub const fn allows_forwarding(&self) -> bool {
        !self.flags.contains(ChatFlags::NO_FORWARDS)
    }

    /// Returns `true` if current user can delete messages (`ChatData::canDeleteMessages`).
    #[inline]
    pub const fn can_delete_messages(&self) -> bool {
        self.am_creator() || self.admin_rights.contains(ChatAdminRights::DELETE_MESSAGES)
    }

    /// Returns `true` if current user can ban members (`ChatData::canBanMembers`).
    #[inline]
    pub const fn can_ban_members(&self) -> bool {
        self.am_creator() || self.admin_rights.contains(ChatAdminRights::BAN_USERS)
    }

    /// Returns `true` if current user can edit group permissions (`ChatData::canEditPermissions`).
    #[inline]
    pub const fn can_edit_permissions(&self) -> bool {
        self.am_in() && self.can_ban_members()
    }

    /// Returns `true` if current user can promote admins (`ChatData::canAddAdmins`).
    #[inline]
    pub const fn can_add_admins(&self) -> bool {
        self.am_in() && self.am_creator()
    }

    /// Returns `true` if any member can add participants (`ChatData::anyoneCanAddMembers`).
    #[inline]
    pub const fn anyone_can_add_members(&self) -> bool {
        !self
            .default_restrictions
            .contains(ChatBannedRights::INVITE_USERS)
    }

    /// Clears cached participant sets and resets bot presence status
    /// (`ChatData::invalidateParticipants` in `data/data_chat.cpp`).
    pub fn invalidate_participants(&mut self) {
        self.participants.clear();
        self.admins.clear();
        self.member_ranks.clear();
        self.bot_status = BotStatus::Unknown;
    }

    /// Validates and advances `self.version` for a participant update
    /// (`ChatData::applyUpdateVersion` in `data/data_chat.cpp:191-201`).
    pub fn apply_update_version(&mut self, version: i32) -> ChatUpdateVersionStatus {
        if self.version > version {
            ChatUpdateVersionStatus::TooOld
        } else if self.version + 1 < version {
            self.invalidate_participants();
            ChatUpdateVersionStatus::Skipped
        } else {
            self.version = version;
            ChatUpdateVersionStatus::Good
        }
    }
}

/// Extra state attached to a megagroup (supergroup) channel (`MegagroupInfo`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MegagroupInfo {
    /// Original basic group `ChatId` if migrated from a basic group.
    pub migrated_from: Option<ChatId>,
    /// Optional physical geolocation.
    pub location: Option<ChannelLocation>,
    /// Optional creator user ID (if known).
    pub creator: Option<UserId>,
    /// Loaded admin user IDs.
    pub admins: BTreeSet<UserId>,
    /// Custom member rank titles.
    pub member_ranks: BTreeMap<UserId, String>,
    /// Number of boosts applied by the current user.
    pub boosts_applied: i32,
    /// Number of boosts required to bypass default group restrictions.
    pub boosts_unrestrict: i32,
    /// Slow-mode delay in seconds (`0` = disabled).
    pub slowmode_seconds: i32,
    /// Timestamp of the current account's last message in this group.
    pub slowmode_last_message: TimeId,
}

impl MegagroupInfo {
    /// Returns `true` if boosts applied meet or exceed the unrestrict threshold
    /// (`ChannelData::unrestrictedByBoosts`).
    #[inline]
    pub const fn unrestricted_by_boosts(&self) -> bool {
        self.boosts_unrestrict > 0 && self.boosts_applied >= self.boosts_unrestrict
    }
}

/// Channel / supergroup peer model (`ChannelData`).
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// Common peer fields.
    pub common: PeerCommon,
    /// Channel flags (defaults to `ChannelFlags::FORBIDDEN`, matching C++).
    pub flags: ChannelFlags,
    /// Server access hash.
    pub access_hash: u64,
    /// Creation/join date.
    pub date: TimeId,
    /// Active usernames (primary + collectible).
    pub usernames: UsernamesInfo,
    /// Default banned rights for non-admin members.
    pub default_restrictions: ChatBannedRights,
    /// Current account's admin rights.
    pub admin_rights: ChatAdminRights,
    /// Current account's personal restrictions and expiry.
    pub restrictions: ChatRestrictionsInfo,
    /// Member count (`-1` when unknown).
    pub members_count: i32,
    /// Admin count.
    pub admins_count: i32,
    /// Restricted member count.
    pub restricted_count: i32,
    /// Kicked member count.
    pub kicked_count: i32,
    /// Pending join requests count.
    pub pending_requests_count: i32,
    /// Recent join requester user IDs.
    pub recent_requesters: Vec<UserId>,
    /// Minimum available message ID in history (`available_min_id`).
    pub available_min_id: MsgId,
    /// Boost level hint.
    pub level_hint: i32,
    /// Stars charged per message (`0` = free).
    pub stars_per_message: i32,
    /// Number of gifts displayed on channel profile.
    pub peer_gifts_count: i32,
    /// Paid subscription expiry timestamp (`0` = none).
    pub subscription_until_date: TimeId,
    /// Permanent invite link (if known).
    pub invite_link: String,
    /// Linked discussion group (for broadcast) or broadcast channel (for discussion group).
    pub discussion_link: Option<ChannelId>,
    /// Whether `discussion_link` has been resolved from `channelFull`.
    pub discussion_link_known: bool,
    /// Linked monoforum / broadcast channel pair.
    pub monoforum_link: Option<ChannelId>,
    /// Linked community channel ID.
    pub linked_community_id: Option<ChannelId>,
    /// Allowed reactions configuration.
    pub allowed_reactions: AllowedReactions,
    /// Optional third-party bot verification details.
    pub bot_verify_details: Option<BotVerifyDetails>,
    /// Megagroup-specific details (populated when `ChannelFlags::MEGAGROUP` is set).
    pub mg_info: Option<MegagroupInfo>,
}

impl Channel {
    /// Creates a new `Channel` shell for `channel_id`.
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            common: PeerCommon::new(PeerId::from_channel(channel_id)),
            flags: ChannelFlags::FORBIDDEN,
            access_hash: 0,
            date: 0,
            usernames: UsernamesInfo::default(),
            default_restrictions: ChatBannedRights::empty(),
            admin_rights: ChatAdminRights::empty(),
            restrictions: ChatRestrictionsInfo::default(),
            members_count: -1,
            admins_count: 1,
            restricted_count: 0,
            kicked_count: 0,
            pending_requests_count: 0,
            recent_requesters: Vec::new(),
            available_min_id: MsgId::ZERO,
            level_hint: 0,
            stars_per_message: 0,
            peer_gifts_count: 0,
            subscription_until_date: 0,
            invite_link: String::new(),
            discussion_link: None,
            discussion_link_known: false,
            monoforum_link: None,
            linked_community_id: None,
            allowed_reactions: AllowedReactions::default(),
            bot_verify_details: None,
            mg_info: None,
        }
    }

    /// Returns the bare `ChannelId`.
    #[inline]
    pub const fn channel_id(&self) -> ChannelId {
        self.common.id.to_channel_or_zero()
    }

    /// Sets channel title and editable username, updating `ChannelFlags::USERNAME`
    /// and the search index (`ChannelData::setName`).
    pub fn set_name(&mut self, name: impl Into<String>, username: impl Into<String>) {
        let name = name.into();
        let username = username.into();
        if !name.is_empty() && self.common.name != name {
            self.common.name = name;
            self.common.name_version = self.common.name_version.wrapping_add(1);
        }
        if self.usernames.editable_username() != username {
            self.usernames.set_username(username);
            self.flags.set(
                ChannelFlags::USERNAME,
                !self.usernames.username().is_empty(),
            );
        }
        self.rebuild_name_index();
    }

    /// Replaces active usernames and syncs `ChannelFlags::USERNAME`.
    pub fn set_usernames(&mut self, list: &[Username]) {
        self.usernames.set_usernames(list);
        self.flags.set(
            ChannelFlags::USERNAME,
            !self.usernames.username().is_empty(),
        );
        self.rebuild_name_index();
    }

    /// Rebuilds the search index from title and active usernames.
    pub fn rebuild_name_index(&mut self) {
        let extra: Vec<&str> = self
            .usernames
            .usernames()
            .iter()
            .map(String::as_str)
            .collect();
        self.common.rebuild_name_index(&extra);
    }

    /// Sets channel flags, ensuring `mg_info` exists when `MEGAGROUP` is enabled.
    pub fn set_flags(&mut self, flags: ChannelFlags) {
        self.flags = flags;
        if self.is_megagroup() && self.mg_info.is_none() {
            self.mg_info = Some(MegagroupInfo::default());
        }
    }

    /// Ensures `mg_info` is allocated and returns a mutable reference to it.
    pub fn ensure_mg_info(&mut self) -> &mut MegagroupInfo {
        self.mg_info.get_or_insert_with(MegagroupInfo::default)
    }

    /// Returns `true` if the current account has left this channel.
    #[inline]
    pub const fn have_left(&self) -> bool {
        self.flags.contains(ChannelFlags::LEFT)
    }

    /// Returns `true` if this channel is forbidden.
    #[inline]
    pub const fn is_forbidden(&self) -> bool {
        self.flags.contains(ChannelFlags::FORBIDDEN)
    }

    /// Returns `true` if this channel is a community hub.
    #[inline]
    pub const fn is_community(&self) -> bool {
        self.flags.contains(ChannelFlags::COMMUNITY)
    }

    /// Returns `true` if the current account is an active member (`ChannelData::amIn`).
    #[inline]
    pub const fn am_in(&self) -> bool {
        !self.is_forbidden() && !self.have_left() && !self.is_community()
    }

    /// Returns `true` if this channel is a supergroup (megagroup).
    #[inline]
    pub const fn is_megagroup(&self) -> bool {
        self.flags.contains(ChannelFlags::MEGAGROUP)
    }

    /// Returns `true` if this channel is a broadcast channel.
    #[inline]
    pub const fn is_broadcast(&self) -> bool {
        self.flags.contains(ChannelFlags::BROADCAST)
    }

    /// Returns `true` if this channel is a gigagroup.
    #[inline]
    pub const fn is_gigagroup(&self) -> bool {
        self.flags.contains(ChannelFlags::GIGAGROUP)
    }

    /// Returns `true` if this channel is a forum.
    #[inline]
    pub const fn is_forum(&self) -> bool {
        self.flags.contains(ChannelFlags::FORUM)
    }

    /// Returns `true` if this channel is a monoforum (channel direct messages).
    #[inline]
    pub const fn is_monoforum(&self) -> bool {
        self.flags.contains(ChannelFlags::MONOFORUM)
    }

    /// Returns `true` if monoforum direct messages are disabled.
    #[inline]
    pub const fn monoforum_disabled(&self) -> bool {
        self.flags.contains(ChannelFlags::MONOFORUM_DISABLED)
    }

    /// Returns `true` if the channel has a public username or geolocation (`ChannelData::isPublic`).
    #[inline]
    pub const fn is_public(&self) -> bool {
        self.flags
            .intersects(ChannelFlags::USERNAME.union(ChannelFlags::LOCATION))
    }

    /// Returns `true` if the current account is the channel creator.
    #[inline]
    pub const fn am_creator(&self) -> bool {
        self.flags.contains(ChannelFlags::CREATOR)
    }

    /// Returns `true` if verified.
    #[inline]
    pub const fn is_verified(&self) -> bool {
        self.flags.contains(ChannelFlags::VERIFIED)
    }

    /// Returns `true` if marked scam.
    #[inline]
    pub const fn is_scam(&self) -> bool {
        self.flags.contains(ChannelFlags::SCAM)
    }

    /// Returns `true` if marked fake.
    #[inline]
    pub const fn is_fake(&self) -> bool {
        self.flags.contains(ChannelFlags::FAKE)
    }

    /// Returns `true` if the current account holds any admin right.
    #[inline]
    pub const fn has_admin_rights(&self) -> bool {
        !self.admin_rights.is_empty()
    }

    /// Returns `true` if forwarding content is allowed.
    #[inline]
    pub const fn allows_forwarding(&self) -> bool {
        !self.flags.contains(ChannelFlags::NO_FORWARDS)
    }

    /// Returns `true` if default group restrictions are lifted via boosts.
    #[inline]
    pub fn unrestricted_by_boosts(&self) -> bool {
        self.mg_info
            .as_ref()
            .is_some_and(MegagroupInfo::unrestricted_by_boosts)
    }

    /// Returns `true` if current account has personal restrictions active at `now`.
    #[inline]
    pub const fn has_restrictions_at(&self, now: TimeId) -> bool {
        !self.restrictions.flags.is_empty()
            && (is_restricted_forever(self.restrictions.until) || self.restrictions.until > now)
    }

    /// Returns `true` if current account can post messages (`ChannelData::canPostMessages`).
    #[inline]
    pub const fn can_post_messages(&self) -> bool {
        self.am_creator() || self.admin_rights.contains(ChatAdminRights::POST_MESSAGES)
    }

    /// Returns `true` if current account can edit messages (`ChannelData::canEditMessages`).
    #[inline]
    pub const fn can_edit_messages(&self) -> bool {
        self.am_creator() || self.admin_rights.contains(ChatAdminRights::EDIT_MESSAGES)
    }

    /// Returns `true` if current account can delete messages (`ChannelData::canDeleteMessages`).
    #[inline]
    pub const fn can_delete_messages(&self) -> bool {
        self.am_creator() || self.admin_rights.contains(ChatAdminRights::DELETE_MESSAGES)
    }

    /// Returns `true` if current account can ban members (`ChannelData::canBanMembers`).
    #[inline]
    pub const fn can_ban_members(&self) -> bool {
        self.am_creator() || self.admin_rights.contains(ChatAdminRights::BAN_USERS)
    }

    /// Returns `true` if current account can post stories (`ChannelData::canPostStories`).
    #[inline]
    pub const fn can_post_stories(&self) -> bool {
        self.am_creator() || self.admin_rights.contains(ChatAdminRights::POST_STORIES)
    }

    /// Returns `true` if current account can edit stories (`ChannelData::canEditStories`).
    #[inline]
    pub const fn can_edit_stories(&self) -> bool {
        !self.is_monoforum()
            && (self.am_creator() || self.admin_rights.contains(ChatAdminRights::EDIT_STORIES))
    }

    /// Returns `true` if current account can delete stories (`ChannelData::canDeleteStories`).
    #[inline]
    pub const fn can_delete_stories(&self) -> bool {
        self.am_creator() || self.admin_rights.contains(ChatAdminRights::DELETE_STORIES)
    }

    /// Returns `true` if current account can post paid media (`ChannelData::canPostPaidMedia`).
    #[inline]
    pub const fn can_post_paid_media(&self) -> bool {
        self.can_post_messages() && self.flags.contains(ChannelFlags::PAID_MEDIA_ALLOWED)
    }

    /// Returns `true` if current account can access monoforum direct messages.
    #[inline]
    pub const fn can_access_monoforum(&self) -> bool {
        self.am_creator()
            || self
                .admin_rights
                .contains(ChatAdminRights::MANAGE_DIRECT_MESSAGES)
    }

    /// Returns `true` if slow mode applies to the current account (`PeerData::slowmodeApplied`).
    #[inline]
    pub const fn slowmode_applied(&self) -> bool {
        !self.am_creator()
            && !self.has_admin_rights()
            && self.flags.contains(ChannelFlags::SLOWMODE_ENABLED)
    }

    /// Returns the configured slow-mode interval in seconds (`0` if none).
    #[inline]
    pub fn slowmode_seconds(&self) -> i32 {
        self.mg_info.as_ref().map_or(0, |mg| mg.slowmode_seconds)
    }

    /// Sets slow-mode interval in seconds and updates `ChannelFlags::SLOWMODE_ENABLED`.
    pub fn set_slowmode_seconds(&mut self, seconds: i32) {
        self.flags.set(ChannelFlags::SLOWMODE_ENABLED, seconds > 0);
        if seconds > 0 || self.mg_info.is_some() {
            self.ensure_mg_info().slowmode_seconds = seconds;
        }
    }

    /// Records the timestamp of our last sent message for slow-mode countdown
    /// (`ChannelData::growSlowmodeLastMessage`).
    pub fn grow_slowmode_last_message(&mut self, when: TimeId) {
        let mg = self.ensure_mg_info();
        if mg.slowmode_last_message < when {
            mg.slowmode_last_message = when;
        }
    }

    /// Returns remaining slow-mode cooldown seconds at `now` (`PeerData::slowmodeSecondsLeft`).
    pub fn slowmode_seconds_left(&self, now: TimeId) -> i32 {
        let Some(mg) = &self.mg_info else {
            return 0;
        };
        if mg.slowmode_seconds > 0 && mg.slowmode_last_message > 0 {
            let elapsed = (now as i64) - (mg.slowmode_last_message as i64);
            ((mg.slowmode_seconds as i64) - elapsed).max(0) as i32
        } else {
            0
        }
    }

    /// Returns the original `ChatId` if this megagroup was migrated from a basic group.
    pub fn migrated_from(&self) -> Option<ChatId> {
        self.mg_info.as_ref().and_then(|mg| mg.migrated_from)
    }

    /// Returns member count clamped to at least 1 (`ChannelData::membersCount`).
    #[inline]
    pub fn members_count_clamped(&self) -> i32 {
        self.members_count.max(1)
    }

    /// Returns `true` if `members_count` has been fetched (`>= 0`).
    #[inline]
    pub const fn members_count_known(&self) -> bool {
        self.members_count >= 0
    }
}

/// Unified Telegram peer (`PeerData` polymorphic hierarchy in C++).
#[derive(Clone, Debug, PartialEq)]
pub enum Peer {
    /// Private user or bot (`UserData`).
    User(Box<User>),
    /// Basic group chat (`ChatData`).
    Chat(Box<Chat>),
    /// Broadcast channel or supergroup/megagroup (`ChannelData`).
    Channel(Box<Channel>),
}

impl Peer {
    /// Returns the peer's `PeerId`.
    #[inline]
    pub const fn id(&self) -> PeerId {
        self.common().id
    }

    /// Returns shared `PeerCommon` fields.
    #[inline]
    pub const fn common(&self) -> &PeerCommon {
        match self {
            Self::User(u) => &u.common,
            Self::Chat(c) => &c.common,
            Self::Channel(ch) => &ch.common,
        }
    }

    /// Returns mutable shared `PeerCommon` fields.
    #[inline]
    pub fn common_mut(&mut self) -> &mut PeerCommon {
        match self {
            Self::User(u) => &mut u.common,
            Self::Chat(c) => &mut c.common,
            Self::Channel(ch) => &mut ch.common,
        }
    }

    /// Returns `Some(&User)` if this peer is a user.
    #[inline]
    pub fn as_user(&self) -> Option<&User> {
        match self {
            Self::User(u) => Some(u),
            _ => None,
        }
    }

    /// Returns `Some(&mut User)` if this peer is a user.
    #[inline]
    pub fn as_user_mut(&mut self) -> Option<&mut User> {
        match self {
            Self::User(u) => Some(u),
            _ => None,
        }
    }

    /// Returns `Some(&Chat)` if this peer is a basic group chat.
    #[inline]
    pub fn as_chat(&self) -> Option<&Chat> {
        match self {
            Self::Chat(c) => Some(c),
            _ => None,
        }
    }

    /// Returns `Some(&mut Chat)` if this peer is a basic group chat.
    #[inline]
    pub fn as_chat_mut(&mut self) -> Option<&mut Chat> {
        match self {
            Self::Chat(c) => Some(c),
            _ => None,
        }
    }

    /// Returns `Some(&Channel)` if this peer is a channel or supergroup.
    #[inline]
    pub fn as_channel(&self) -> Option<&Channel> {
        match self {
            Self::Channel(ch) => Some(ch),
            _ => None,
        }
    }

    /// Returns `Some(&mut Channel)` if this peer is a channel or supergroup.
    #[inline]
    pub fn as_channel_mut(&mut self) -> Option<&mut Channel> {
        match self {
            Self::Channel(ch) => Some(ch),
            _ => None,
        }
    }

    /// Returns the display name (`PeerData::name`).
    pub fn name(&self) -> &str {
        &self.common().name
    }

    /// Returns the short display name (`PeerData::shortName`).
    pub fn short_name(&self) -> &str {
        match self {
            Self::User(u) => u.short_name(),
            Self::Chat(c) => &c.common.name,
            Self::Channel(ch) => &ch.common.name,
        }
    }

    /// Returns the primary active username, or `""` if none.
    pub fn username(&self) -> &str {
        match self {
            Self::User(u) => u.usernames.username(),
            Self::Channel(ch) => ch.usernames.username(),
            Self::Chat(_) => "",
        }
    }

    /// Returns all active usernames.
    pub fn usernames(&self) -> &[String] {
        match self {
            Self::User(u) => u.usernames.usernames(),
            Self::Channel(ch) => ch.usernames.usernames(),
            Self::Chat(_) => &[],
        }
    }

    /// Updates blocked status (`PeerData::setIsBlocked` in `data/data_peer.cpp:2113-2130`),
    /// keeping `UserFlags::BLOCKED` in sync for users. Returns `true` if status changed.
    pub fn set_is_blocked(&mut self, blocked: bool) -> bool {
        let status = if blocked {
            BlockStatus::Blocked
        } else {
            BlockStatus::NotBlocked
        };
        if self.common().block_status == status {
            return false;
        }
        self.common_mut().block_status = status;
        if let Self::User(u) = self {
            u.flags.set(UserFlags::BLOCKED, blocked);
        }
        true
    }

    /// Returns `true` if this peer is a bot.
    #[inline]
    pub fn is_bot(&self) -> bool {
        self.as_user().is_some_and(User::is_bot)
    }

    /// Returns `true` if this peer is the logged-in user (`Self`).
    #[inline]
    pub fn is_self(&self) -> bool {
        self.as_user().is_some_and(User::is_self)
    }

    /// Returns `true` if this peer is verified.
    #[inline]
    pub fn is_verified(&self) -> bool {
        match self {
            Self::User(u) => u.is_verified(),
            Self::Channel(ch) => ch.is_verified(),
            Self::Chat(_) => false,
        }
    }

    /// Returns `true` if this peer is marked scam.
    #[inline]
    pub fn is_scam(&self) -> bool {
        match self {
            Self::User(u) => u.is_scam(),
            Self::Channel(ch) => ch.is_scam(),
            Self::Chat(_) => false,
        }
    }

    /// Returns `true` if this peer is marked fake.
    #[inline]
    pub fn is_fake(&self) -> bool {
        match self {
            Self::User(u) => u.is_fake(),
            Self::Channel(ch) => ch.is_fake(),
            Self::Chat(_) => false,
        }
    }

    /// Returns `true` if this peer is a Telegram Premium user.
    #[inline]
    pub fn is_premium(&self) -> bool {
        self.as_user().is_some_and(User::is_premium)
    }

    /// Returns `true` if this peer is a megagroup (supergroup).
    #[inline]
    pub fn is_megagroup(&self) -> bool {
        self.as_channel().is_some_and(Channel::is_megagroup)
    }

    /// Returns `true` if this peer is a broadcast channel.
    #[inline]
    pub fn is_broadcast(&self) -> bool {
        self.as_channel().is_some_and(Channel::is_broadcast)
    }

    /// Returns `true` if this peer is a forum (supergroup or bot).
    #[inline]
    pub fn is_forum(&self) -> bool {
        match self {
            Self::User(u) => u.is_bot() && u.flags.contains(UserFlags::FORUM),
            Self::Channel(ch) => ch.is_forum(),
            Self::Chat(_) => false,
        }
    }

    /// Returns `true` if this peer is a monoforum.
    #[inline]
    pub fn is_monoforum(&self) -> bool {
        self.as_channel().is_some_and(Channel::is_monoforum)
    }

    /// Returns `true` if this peer is a gigagroup.
    #[inline]
    pub fn is_gigagroup(&self) -> bool {
        self.as_channel().is_some_and(Channel::is_gigagroup)
    }

    /// Returns `true` if this peer is `@spambot`.
    #[inline]
    pub fn is_freeze_appeal_chat(&self) -> bool {
        self.username().eq_ignore_ascii_case("spambot")
    }

    /// Returns `true` if forwarding messages from this peer is allowed (`PeerData::allowsForwarding`).
    #[inline]
    pub fn allows_forwarding(&self) -> bool {
        match self {
            Self::User(u) => u.allows_forwarding(),
            Self::Chat(c) => c.allows_forwarding(),
            Self::Channel(ch) => ch.allows_forwarding(),
        }
    }

    /// Checks whether the current user is restricted from performing `right`
    /// (`PeerData::amRestricted` in `data_peer.cpp`).
    pub fn am_restricted(
        &self,
        right: ChatBannedRights,
        ctx: SendCheckContext,
    ) -> RestrictionCheckResult {
        fn allow_by_admin_rights(right: ChatBannedRights, admin_rights: ChatAdminRights) -> bool {
            if right == ChatBannedRights::INVITE_USERS {
                admin_rights.contains(ChatAdminRights::INVITE_USERS)
            } else if right == ChatBannedRights::CHANGE_INFO {
                admin_rights.contains(ChatAdminRights::CHANGE_INFO)
            } else if right == ChatBannedRights::MANAGE_TOPICS {
                admin_rights.contains(ChatAdminRights::MANAGE_TOPICS)
            } else if right == ChatBannedRights::PIN_MESSAGES {
                admin_rights.contains(ChatAdminRights::PIN_MESSAGES)
            } else {
                !admin_rights.is_empty()
            }
        }

        match self {
            Self::User(user) => {
                if user.requires_premium_to_write() && !ctx.account_premium {
                    return RestrictionCheckResult::Explicit;
                }
                if right == ChatBannedRights::SEND_VOICES
                    || right == ChatBannedRights::SEND_ROUNDVIDEOS
                {
                    if user.flags.contains(UserFlags::VOICE_MESSAGES_FORBIDDEN) {
                        RestrictionCheckResult::Explicit
                    } else {
                        RestrictionCheckResult::Allowed
                    }
                } else if right == ChatBannedRights::PIN_MESSAGES {
                    if user.flags.contains(UserFlags::CAN_PIN_MESSAGES) {
                        RestrictionCheckResult::Allowed
                    } else {
                        RestrictionCheckResult::Explicit
                    }
                } else {
                    RestrictionCheckResult::Allowed
                }
            }
            Self::Channel(channel) => {
                if channel.monoforum_disabled() {
                    return RestrictionCheckResult::WithEveryone;
                }
                let default_restrictions = channel.default_restrictions
                    | if channel.is_public() {
                        ChatBannedRights::PIN_MESSAGES | ChatBannedRights::CHANGE_INFO
                    } else {
                        ChatBannedRights::empty()
                    };
                if channel.am_creator() || allow_by_admin_rights(right, channel.admin_rights) {
                    RestrictionCheckResult::Allowed
                } else if default_restrictions.intersects(right)
                    && !channel.unrestricted_by_boosts()
                {
                    RestrictionCheckResult::WithEveryone
                } else if channel.restrictions.flags.intersects(right) {
                    RestrictionCheckResult::Explicit
                } else {
                    RestrictionCheckResult::Allowed
                }
            }
            Self::Chat(chat) => {
                if chat.am_creator() || allow_by_admin_rights(right, chat.admin_rights) {
                    RestrictionCheckResult::Allowed
                } else if chat.default_restrictions.intersects(right) {
                    RestrictionCheckResult::WithEveryone
                } else {
                    RestrictionCheckResult::Allowed
                }
            }
        }
    }

    /// Returns `true` if the current user can send at least one of `rights` to this peer
    /// (`Data::CanSendAnyOf` in `data_chat_participant_status.cpp`).
    pub fn can_send_any_of(
        &self,
        rights: ChatBannedRights,
        forbid_in_forums: bool,
        ctx: SendCheckContext,
    ) -> bool {
        if ctx.account_frozen && !self.is_freeze_appeal_chat() {
            return false;
        }
        match self {
            Self::User(user) => {
                if user.is_inaccessible()
                    || user.is_replies_chat(ctx.is_test_environment)
                    || user.is_verify_codes()
                {
                    return false;
                }
                if user.requires_premium_to_write() && !ctx.account_premium {
                    return false;
                }
                let special = ChatBannedRights::SEND_VOICES
                    | ChatBannedRights::SEND_ROUNDVIDEOS
                    | ChatBannedRights::SEND_POLLS;
                if !(rights & !special).is_empty() {
                    return true;
                }
                for right in [
                    ChatBannedRights::SEND_VOICES,
                    ChatBannedRights::SEND_ROUNDVIDEOS,
                    ChatBannedRights::SEND_POLLS,
                ] {
                    if rights.contains(right) && self.am_restricted(right, ctx).is_allowed() {
                        return true;
                    }
                }
                false
            }
            Self::Chat(chat) => {
                if !chat.am_in() {
                    return false;
                }
                chat.am_creator()
                    || chat.has_admin_rights()
                    || !(rights & !chat.default_restrictions).is_empty()
            }
            Self::Channel(channel) => {
                if channel.monoforum_disabled() {
                    return false;
                }
                let allowed = channel.am_in()
                    || (channel.flags.contains(ChannelFlags::HAS_LINK)
                        && !channel.flags.contains(ChannelFlags::JOIN_TO_WRITE))
                    || channel.is_monoforum();
                if !allowed || (forbid_in_forums && channel.is_forum()) {
                    return false;
                }
                let restricted = channel.restrictions.flags
                    | if channel.unrestricted_by_boosts() {
                        ChatBannedRights::empty()
                    } else {
                        channel.default_restrictions
                    };
                channel.can_post_messages()
                    || (!channel.is_broadcast()
                        && (channel.has_admin_rights() || !(rights & !restricted).is_empty()))
            }
        }
    }

    /// Returns `true` if the current account can send the specific media/message `right`
    /// (`Data::CanSend`).
    #[inline]
    pub fn can_send(
        &self,
        right: ChatBannedRights,
        forbid_in_forums: bool,
        ctx: SendCheckContext,
    ) -> bool {
        self.can_send_any_of(right, forbid_in_forums, ctx)
    }

    /// Returns `true` if the current account can send plain text messages (`Data::CanSendTexts`).
    #[inline]
    pub fn can_send_texts(&self, forbid_in_forums: bool, ctx: SendCheckContext) -> bool {
        self.can_send(ChatBannedRights::SEND_PLAIN, forbid_in_forums, ctx)
    }

    /// Returns `true` if the current account can send any kind of message (`Data::CanSendAnything`).
    #[inline]
    pub fn can_send_anything(&self, forbid_in_forums: bool, ctx: SendCheckContext) -> bool {
        self.can_send_any_of(ChatBannedRights::ALL_SEND, forbid_in_forums, ctx)
    }

    /// Convenience alias for `can_send_anything(true, ctx)` (matching `CanWrite` checks).
    #[inline]
    pub fn can_write(&self, ctx: SendCheckContext) -> bool {
        self.can_send_anything(true, ctx)
    }

    /// Returns `true` if at least minimal (`min`) data is loaded (`PeerData::isMinimalLoaded`).
    #[inline]
    pub fn is_minimal_loaded(&self) -> bool {
        self.common().is_minimal_loaded()
    }

    /// Returns `true` if non-`min` peer metadata is loaded (`PeerData::isLoaded`).
    #[inline]
    pub fn is_loaded(&self) -> bool {
        self.common().is_loaded()
    }

    /// Returns `true` if `*Full` peer info is loaded (`PeerData::isFullLoaded`).
    #[inline]
    pub fn is_full_loaded(&self) -> bool {
        self.common().is_full_loaded()
    }

    /// Returns `true` if the current account can pin messages (`PeerData::canPinMessages`).
    pub fn can_pin_messages(&self, ctx: SendCheckContext) -> bool {
        match self {
            Self::User(_) => self
                .am_restricted(ChatBannedRights::PIN_MESSAGES, ctx)
                .is_allowed(),
            Self::Chat(chat) => {
                chat.am_in()
                    && self
                        .am_restricted(ChatBannedRights::PIN_MESSAGES, ctx)
                        .is_allowed()
            }
            Self::Channel(channel) => {
                if channel.is_megagroup() {
                    self.am_restricted(ChatBannedRights::PIN_MESSAGES, ctx)
                        .is_allowed()
                } else {
                    channel.am_creator()
                        || channel
                            .admin_rights
                            .contains(ChatAdminRights::EDIT_MESSAGES)
                }
            }
        }
    }

    /// Returns `true` if the current account posts anonymously in this peer (`PeerData::amAnonymous`).
    pub fn am_anonymous(&self) -> bool {
        match self {
            Self::Channel(ch) => {
                if ch.is_broadcast() {
                    !ch.flags.contains(ChannelFlags::SIGNATURE_PROFILES)
                } else {
                    ch.admin_rights.contains(ChatAdminRights::ANONYMOUS)
                }
            }
            _ => false,
        }
    }

    /// Returns `true` if slow mode applies to the current account in this peer.
    pub fn slowmode_applied(&self) -> bool {
        self.as_channel().is_some_and(Channel::slowmode_applied)
    }

    /// Returns remaining slow-mode cooldown seconds at `now`.
    pub fn slowmode_seconds_left(&self, now: TimeId) -> i32 {
        self.as_channel()
            .map_or(0, |ch| ch.slowmode_seconds_left(now))
    }

    /// Returns `true` if the current account can manage group calls (`PeerData::canManageGroupCall`).
    pub fn can_manage_group_call(&self) -> bool {
        match self {
            Self::User(u) => u.is_self(),
            Self::Chat(c) => {
                c.am_creator() || c.admin_rights.contains(ChatAdminRights::MANAGE_CALL)
            }
            Self::Channel(ch) => {
                !ch.is_monoforum()
                    && (ch.am_creator() || ch.admin_rights.contains(ChatAdminRights::MANAGE_CALL))
            }
        }
    }

    /// Returns `true` if the current account can create forum topics (`PeerData::canCreateTopics`).
    pub fn can_create_topics(&self, ctx: SendCheckContext) -> bool {
        match self {
            Self::User(u) => u.is_bot() && u.flags.contains(UserFlags::FORUM),
            Self::Channel(ch) => {
                ch.is_forum()
                    && self
                        .am_restricted(ChatBannedRights::MANAGE_TOPICS, ctx)
                        .is_allowed()
            }
            Self::Chat(_) => false,
        }
    }

    /// Returns `true` if the current account can manage forum topics (`PeerData::canManageTopics`).
    pub fn can_manage_topics(&self) -> bool {
        match self {
            Self::User(u) => u.is_bot() && u.flags.contains(UserFlags::FORUM),
            Self::Channel(ch) => {
                ch.is_forum()
                    && (ch.am_creator() || ch.admin_rights.contains(ChatAdminRights::MANAGE_TOPICS))
            }
            Self::Chat(_) => false,
        }
    }
}
