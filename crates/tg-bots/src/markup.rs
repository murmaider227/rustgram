//! Port of `history/history_item_reply_markup.h`, `history/history_item_reply_markup.cpp`,
//! and `chat_helpers/bot_keyboard.cpp` reply-keyboard state derivation.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

use crate::deep_links::is_mini_app_url;
use crate::stand_in::{ChatAdminRights, DocumentId, FullMsgId, PeerId, PeerKind, UserId};

bitflags! {
    /// Reply markup flags, matching `ReplyMarkupFlag` in `history_item_reply_markup.h:32-46`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct ReplyMarkupFlags: u32 {
        /// Remove / hide reply keyboard (`replyKeyboardHide`).
        const NONE                    = 1 << 0;
        /// Force reply (`replyKeyboardForceReply` or `force_reply` flag).
        const FORCE_REPLY             = 1 << 1;
        /// Optimization flag: markup contains a `SwitchInline` button.
        const HAS_SWITCH_INLINE       = 1 << 2;
        /// Inline keyboard attached to the message (`replyInlineMarkup`).
        const INLINE                  = 1 << 3;
        /// Resize reply keyboard vertically to fit buttons (`resize`).
        const RESIZE                  = 1 << 4;
        /// Hide reply keyboard after a single press (`single_use`).
        const SINGLE_USE              = 1 << 5;
        /// Apply markup selectively to mentioned / replied-to users (`selective`).
        const SELECTIVE               = 1 << 6;
        /// No reply markup is present (`IsNull`).
        const IS_NULL                 = 1 << 7;
        /// Optimization flag: markup has exactly one row with one `Buy` button.
        const ONLY_BUY_BUTTON         = 1 << 8;
        /// Always show the reply keyboard when regular keyboard is hidden (`persistent`).
        const PERSISTENT              = 1 << 9;
        /// Suggested post decline marker.
        const SUGGESTION_DECLINE      = 1 << 10;
        /// Suggested post accept marker.
        const SUGGESTION_ACCEPT       = 1 << 11;
        /// Suggested post separator marker.
        const SUGGESTION_SEPARATOR    = 1 << 12;
    }
}

bitflags! {
    /// Peer types allowed for switch-inline or attach-menu peer selection.
    ///
    /// Matches `InlineBots::PeerType` in `inline_bots/bot_attach_web_view.h:59-66`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct PeerTypes: u8 {
        /// The same bot's own private chat.
        const SAME_BOT  = 0x01;
        /// Other bots' private chats.
        const BOT       = 0x02;
        /// Regular users' private chats.
        const USER      = 0x04;
        /// Basic groups and supergroups.
        const GROUP     = 0x08;
        /// Broadcast channels.
        const BROADCAST = 0x10;
    }
}

/// Plain representation of TL `InlineQueryPeerType` constructors for `tg-api` conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InlineQueryPeerTypeInput {
    /// `inlineQueryPeerTypeSameBotPM`
    SameBotPm,
    /// `inlineQueryPeerTypePM`
    Pm,
    /// `inlineQueryPeerTypeChat`
    Chat,
    /// `inlineQueryPeerTypeMegagroup`
    Megagroup,
    /// `inlineQueryPeerTypeBroadcast`
    Broadcast,
    /// `inlineQueryPeerTypeBotPM`
    BotPm,
}

/// Converts a slice of TL `InlineQueryPeerType` values into [`PeerTypes`].
///
/// Port of `PeerTypesFromMTP` in `history_item_reply_markup.cpp:279-299`.
/// Note that tdesktop maps `SameBotPM` to empty flags for switch-inline recipient pickers.
pub fn peer_types_from_inline_query(types: &[InlineQueryPeerTypeInput]) -> PeerTypes {
    let mut result = PeerTypes::empty();
    for ty in types {
        match ty {
            InlineQueryPeerTypeInput::Pm => result |= PeerTypes::USER,
            InlineQueryPeerTypeInput::Chat | InlineQueryPeerTypeInput::Megagroup => {
                result |= PeerTypes::GROUP
            }
            InlineQueryPeerTypeInput::Broadcast => result |= PeerTypes::BROADCAST,
            InlineQueryPeerTypeInput::BotPm => result |= PeerTypes::BOT,
            InlineQueryPeerTypeInput::SameBotPm => {}
        }
    }
    result
}

/// Plain representation of TL `AttachMenuPeerType` constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttachMenuPeerTypeInput {
    /// `attachMenuPeerTypeSameBotPM`
    SameBotPm,
    /// `attachMenuPeerTypeBotPM`
    BotPm,
    /// `attachMenuPeerTypePM`
    Pm,
    /// `attachMenuPeerTypeChat`
    Chat,
    /// `attachMenuPeerTypeBroadcast`
    Broadcast,
}

/// Converts a slice of TL `AttachMenuPeerType` values into [`PeerTypes`].
///
/// Port of `ResolvePeerTypes` in `inline_bots/bot_attach_web_view.cpp:191-208`.
pub fn peer_types_from_attach_menu(types: &[AttachMenuPeerTypeInput]) -> PeerTypes {
    let mut result = PeerTypes::empty();
    for ty in types {
        match ty {
            AttachMenuPeerTypeInput::SameBotPm => result |= PeerTypes::SAME_BOT,
            AttachMenuPeerTypeInput::BotPm => result |= PeerTypes::BOT,
            AttachMenuPeerTypeInput::Pm => result |= PeerTypes::USER,
            AttachMenuPeerTypeInput::Chat => result |= PeerTypes::GROUP,
            AttachMenuPeerTypeInput::Broadcast => result |= PeerTypes::BROADCAST,
        }
    }
    result
}

/// Parses a space-separated `choose=` link parameter (`"users bots groups channels"`).
///
/// Port of `InlineBots::ParseChooseTypes` in `inline_bots/bot_attach_web_view.cpp:938-952`.
pub fn parse_choose_types(choose: &str) -> PeerTypes {
    let mut result = PeerTypes::empty();
    for entry in choose.split(' ') {
        match entry {
            "users" => result |= PeerTypes::USER,
            "bots" => result |= PeerTypes::BOT,
            "groups" => result |= PeerTypes::GROUP,
            "channels" => result |= PeerTypes::BROADCAST,
            _ => {}
        }
    }
    result
}

/// Converts WebView JS `switchInlineQuery` chat type names (`["users", "bots", "groups", "channels"]`).
///
/// Port of `PeerTypesFromNames` in `inline_bots/bot_attach_web_view.cpp:242-258`.
pub fn peer_types_from_names<S: AsRef<str>>(names: &[S]) -> PeerTypes {
    let mut result = PeerTypes::empty();
    for name in names {
        match name.as_ref() {
            "users" => result |= PeerTypes::USER,
            "bots" => result |= PeerTypes::BOT,
            "groups" => result |= PeerTypes::GROUP,
            "channels" => result |= PeerTypes::BROADCAST,
            _ => {}
        }
    }
    result
}

/// Checks whether a peer matches a [`PeerTypes`] mask with respect to `bot_peer_id`.
///
/// Port of `InlineBots::PeerMatchesTypes` in `inline_bots/bot_attach_web_view.cpp:922-936`.
pub fn peer_matches_types(
    peer_id: PeerId,
    peer_kind: PeerKind,
    bot_peer_id: PeerId,
    types: PeerTypes,
) -> bool {
    match peer_kind {
        PeerKind::User => {
            if peer_id == bot_peer_id {
                types.contains(PeerTypes::SAME_BOT)
            } else {
                types.contains(PeerTypes::USER)
            }
        }
        PeerKind::Bot => {
            if peer_id == bot_peer_id {
                types.contains(PeerTypes::SAME_BOT)
            } else {
                types.contains(PeerTypes::BOT)
            }
        }
        PeerKind::Broadcast => types.contains(PeerTypes::BROADCAST),
        PeerKind::Group => types.contains(PeerTypes::GROUP),
    }
}

/// Peer category requested by a `RequestPeer` keyboard button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RequestPeerType {
    /// Request user(s).
    #[default]
    User,
    /// Request basic group(s) or supergroup(s).
    Group,
    /// Request broadcast channel(s).
    Broadcast,
}

/// Tri-state filter criteria for `RequestPeerQuery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RequestPeerRestriction {
    /// No restriction (TL field omitted).
    #[default]
    Any,
    /// Must satisfy the predicate (TL field `boolTrue`).
    Yes,
    /// Must NOT satisfy the predicate (TL field `boolFalse`).
    No,
}

impl RequestPeerRestriction {
    /// Converts an optional boolean into a tri-state [`RequestPeerRestriction`].
    pub const fn from_option(value: Option<bool>) -> Self {
        match value {
            None => Self::Any,
            Some(true) => Self::Yes,
            Some(false) => Self::No,
        }
    }

    /// Evaluates whether `actual` satisfies this restriction.
    pub const fn matches(self, actual: bool) -> bool {
        match self {
            Self::Any => true,
            Self::Yes => actual,
            Self::No => !actual,
        }
    }
}

/// Criteria for a `buttonTypeRequestPeer` keyboard button.
///
/// Port of `RequestPeerQuery` in `history_item_reply_markup.h:50-72`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct RequestPeerQuery {
    /// Maximum number of peers the user may select (`max_quantity`).
    pub max_quantity: i32,
    /// Target peer category.
    pub peer_type: RequestPeerType,
    /// For users: whether the user must/must not be a bot.
    pub user_is_bot: RequestPeerRestriction,
    /// For users: whether the user must/must not have Telegram Premium.
    pub user_is_premium: RequestPeerRestriction,
    /// For groups: whether the group must/must not be a forum.
    pub group_is_forum: RequestPeerRestriction,
    /// For groups/channels: whether the peer must/must not have a public username.
    pub has_username: RequestPeerRestriction,
    /// Whether the current user must be the creator of the group/channel.
    pub am_creator: bool,
    /// For groups: whether the requesting bot must already be a participant.
    pub is_bot_participant: bool,
    /// Minimum admin rights required for the current user.
    pub my_rights: ChatAdminRights,
    /// Minimum admin rights required for the bot.
    pub bot_rights: ChatAdminRights,
}

/// Candidate peer properties used to evaluate [`RequestPeerQuery::matches_candidate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestPeerCandidate {
    /// Candidate peer category.
    pub peer_type: RequestPeerType,
    /// Whether candidate user is a bot.
    pub is_bot: bool,
    /// Whether candidate user is Premium.
    pub is_premium: bool,
    /// Whether candidate group is a forum.
    pub is_forum: bool,
    /// Whether candidate peer has a username.
    pub has_username: bool,
    /// Whether the current user is the creator.
    pub am_creator: bool,
    /// Whether the requesting bot is already a participant.
    pub is_bot_participant: bool,
    /// Current user's admin rights in this group/channel.
    pub my_rights: ChatAdminRights,
    /// Requesting bot's admin rights in this group/channel.
    pub bot_rights: ChatAdminRights,
}

impl RequestPeerQuery {
    /// Returns `true` if `candidate` satisfies every criterion in this query.
    pub fn matches_candidate(&self, candidate: &RequestPeerCandidate) -> bool {
        if candidate.peer_type != self.peer_type {
            return false;
        }
        match self.peer_type {
            RequestPeerType::User => {
                self.user_is_bot.matches(candidate.is_bot)
                    && self.user_is_premium.matches(candidate.is_premium)
            }
            RequestPeerType::Group => {
                (!self.am_creator || candidate.am_creator)
                    && (!self.is_bot_participant || candidate.is_bot_participant)
                    && self.group_is_forum.matches(candidate.is_forum)
                    && self.has_username.matches(candidate.has_username)
                    && candidate.my_rights.contains(self.my_rights)
                    && candidate.bot_rights.contains(self.bot_rights)
            }
            RequestPeerType::Broadcast => {
                (!self.am_creator || candidate.am_creator)
                    && self.has_username.matches(candidate.has_username)
                    && candidate.my_rights.contains(self.my_rights)
                    && candidate.bot_rights.contains(self.bot_rights)
            }
        }
    }
}

/// Payload for a `CreateBot` (`requestPeerTypeCreateBot`) button.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct CreateBotRequest {
    /// Optional suggested display name for the managed bot.
    pub suggested_name: String,
    /// Optional suggested username for the managed bot.
    pub suggested_username: String,
}

/// Poll type restriction for `RequestPoll` buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PollRequestRestriction {
    /// User may create either a regular poll or a quiz.
    #[default]
    Any,
    /// Only regular (non-quiz) polls allowed (`quiz = false`).
    RegularOnly,
    /// Only quiz polls allowed (`quiz = true`).
    QuizOnly,
}

impl PollRequestRestriction {
    /// Converts optional TL `quiz: Bool` into [`PollRequestRestriction`].
    pub const fn from_quiz_option(quiz: Option<bool>) -> Self {
        match quiz {
            None => Self::Any,
            Some(false) => Self::RegularOnly,
            Some(true) => Self::QuizOnly,
        }
    }
}

/// Tag enum mirroring `HistoryMessageMarkupButton::Type` in `history_item_reply_markup.h:81-107`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ButtonType {
    /// Plain reply keyboard button (sends its text as a message).
    Default,
    /// Opens an external or Telegram URL.
    Url,
    /// Sends a callback query with opaque bytes.
    Callback,
    /// Sends a callback query guarded by 2FA cloud password.
    CallbackWithPassword,
    /// Requests the user's phone number contact.
    RequestPhone,
    /// Requests the user's geolocation.
    RequestLocation,
    /// Prompts the user to create and send a poll.
    RequestPoll,
    /// Prompts the user to select peer(s) matching [`RequestPeerQuery`].
    RequestPeer,
    /// Switches to inline mode in a chosen chat.
    SwitchInline,
    /// Switches to inline mode in the current chat.
    SwitchInlineSame,
    /// Launches an HTML5 bot game via callback query.
    Game,
    /// Opens payment checkout flow.
    Buy,
    /// URL login / OAuth authorization button (`inlineButtonTypeUrlAuth`).
    Auth,
    /// Opens a user's profile (`inlineButtonTypeUserProfile`).
    UserProfile,
    /// Opens a Mini App WebView (`inlineButtonTypeWebView`).
    WebView,
    /// Opens a simple WebView from reply keyboard (`buttonTypeSimpleWebView`).
    SimpleWebView,
    /// Copies text to the clipboard (`inlineButtonTypeCopy`).
    CopyText,
    /// Non-interactive disabled button.
    Disabled,
    /// Suggested post decline action button.
    SuggestDecline,
    /// Suggested post accept action button.
    SuggestAccept,
    /// Suggested post edit/change action button.
    SuggestChange,
    /// Creates a managed bot (`requestPeerTypeCreateBot`).
    CreateBot,
}

/// Corner icon shown on inline keyboard buttons.
///
/// Matches `HistoryMessageMarkupButton::TypeIcon` in `history_item_reply_markup.h:109-116`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ButtonTypeIcon {
    /// No corner icon.
    #[default]
    None,
    /// External URL arrow icon (`inline_button_url`).
    Url,
    /// Payment card icon (`inline_button_card`).
    Payment,
    /// Switch inline / PM icon (`inline_button_switch`).
    SwitchPm,
    /// WebView / Mini App icon (`inline_button_web`).
    Webview,
    /// Copy-to-clipboard icon (`inline_button_copy`).
    Copy,
}

impl ButtonType {
    /// Returns `true` if activating this button starts an asynchronous server request
    /// and displays a loading indicator.
    ///
    /// Port of `HistoryMessageMarkupButton::LoadsOnActivate` (`history_item_reply_markup.cpp:346-350`).
    pub const fn loads_on_activate(self) -> bool {
        matches!(
            self,
            Self::Callback | Self::CallbackWithPassword | Self::Game
        )
    }

    /// Returns the corner icon associated with this button type.
    ///
    /// Port of `HistoryMessageMarkupButton::IconOfType` (`history_item_reply_markup.cpp:27-50,352-357`).
    pub const fn icon(self) -> ButtonTypeIcon {
        match self {
            Self::Url | Self::Auth => ButtonTypeIcon::Url,
            Self::SwitchInline | Self::SwitchInlineSame => ButtonTypeIcon::SwitchPm,
            Self::Buy => ButtonTypeIcon::Payment,
            Self::WebView | Self::SimpleWebView => ButtonTypeIcon::Webview,
            Self::CopyText => ButtonTypeIcon::Copy,
            Self::Default
            | Self::Callback
            | Self::CallbackWithPassword
            | Self::RequestPhone
            | Self::RequestLocation
            | Self::RequestPoll
            | Self::RequestPeer
            | Self::Game
            | Self::UserProfile
            | Self::Disabled
            | Self::SuggestDecline
            | Self::SuggestAccept
            | Self::SuggestChange
            | Self::CreateBot => ButtonTypeIcon::None,
        }
    }
}

/// Semantic button color style (`MTPKeyboardButtonStyle` / `MTPRichButtonStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ButtonColor {
    /// Standard theme button background.
    #[default]
    Normal,
    /// Primary accent background (`bg_primary`).
    Primary,
    /// Destructive / danger background (`bg_danger`).
    Danger,
    /// Positive / success background (`bg_success`).
    Success,
}

impl ButtonColor {
    /// Resolves color precedence matching `ParseVisual` in `history_item_reply_markup.cpp:76-82`:
    /// danger > primary > success > normal.
    pub const fn from_style_flags(bg_danger: bool, bg_primary: bool, bg_success: bool) -> Self {
        if bg_danger {
            Self::Danger
        } else if bg_primary {
            Self::Primary
        } else if bg_success {
            Self::Success
        } else {
            Self::Normal
        }
    }
}

/// Visual customization attached to a keyboard button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ButtonVisual {
    /// Optional custom emoji icon id (`0` if none).
    pub icon_id: DocumentId,
    /// Semantic button color.
    pub color: ButtonColor,
}

/// Typed button payload covering every reply-keyboard and inline-keyboard button kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ButtonKind {
    /// Regular reply button (sends its text).
    Default,
    /// URL button.
    Url {
        /// Target URL.
        url: String,
    },
    /// Callback query button.
    Callback {
        /// Opaque callback data bytes.
        data: Vec<u8>,
    },
    /// Callback query button requiring 2FA password verification.
    CallbackWithPassword {
        /// Opaque callback data bytes.
        data: Vec<u8>,
    },
    /// Request phone contact button.
    RequestPhone,
    /// Request geolocation button.
    RequestLocation,
    /// Request poll creation button.
    RequestPoll {
        /// Optional quiz/regular restriction.
        restriction: PollRequestRestriction,
    },
    /// Request peer selection button.
    RequestPeer {
        /// Unique button id echoed back to `messages.sendBotRequestedPeer`.
        button_id: i64,
        /// Peer selection criteria.
        query: RequestPeerQuery,
    },
    /// Switch inline query in a chosen chat.
    SwitchInline {
        /// Pre-filled inline query text.
        query: String,
        /// Optional peer-type filter for the recipient chooser.
        peer_types: PeerTypes,
    },
    /// Switch inline query in the current chat.
    SwitchInlineSame {
        /// Pre-filled inline query text.
        query: String,
    },
    /// Play bot game button.
    Game,
    /// Buy / invoice payment button.
    Buy,
    /// URL login / OAuth authorization button.
    Auth {
        /// Target URL to authenticate against.
        url: String,
        /// Replacement text shown when the message is forwarded.
        forward_text: String,
        /// Server button id used in `messages.requestUrlAuth` / `acceptUrlAuth`.
        button_id: i64,
    },
    /// Opens a specific user's profile.
    UserProfile {
        /// Target user id.
        user_id: UserId,
    },
    /// Inline WebView (Mini App) button.
    WebView {
        /// WebView URL.
        url: String,
    },
    /// Reply-keyboard simple WebView button.
    SimpleWebView {
        /// WebView URL.
        url: String,
    },
    /// Copy-text inline button.
    CopyText {
        /// Text copied to the clipboard.
        text: String,
    },
    /// Disabled non-interactive button.
    Disabled,
    /// Suggested post decline button.
    SuggestDecline,
    /// Suggested post accept button.
    SuggestAccept,
    /// Suggested post edit button.
    SuggestChange,
    /// Create managed bot button.
    CreateBot {
        /// Server button id echoed to `messages.sendBotRequestedPeer`.
        button_id: i64,
        /// Suggested name/username for the new bot.
        request: CreateBotRequest,
    },
}

impl ButtonKind {
    /// Returns the discriminant [`ButtonType`].
    pub const fn button_type(&self) -> ButtonType {
        match self {
            Self::Default => ButtonType::Default,
            Self::Url { .. } => ButtonType::Url,
            Self::Callback { .. } => ButtonType::Callback,
            Self::CallbackWithPassword { .. } => ButtonType::CallbackWithPassword,
            Self::RequestPhone => ButtonType::RequestPhone,
            Self::RequestLocation => ButtonType::RequestLocation,
            Self::RequestPoll { .. } => ButtonType::RequestPoll,
            Self::RequestPeer { .. } => ButtonType::RequestPeer,
            Self::SwitchInline { .. } => ButtonType::SwitchInline,
            Self::SwitchInlineSame { .. } => ButtonType::SwitchInlineSame,
            Self::Game => ButtonType::Game,
            Self::Buy => ButtonType::Buy,
            Self::Auth { .. } => ButtonType::Auth,
            Self::UserProfile { .. } => ButtonType::UserProfile,
            Self::WebView { .. } => ButtonType::WebView,
            Self::SimpleWebView { .. } => ButtonType::SimpleWebView,
            Self::CopyText { .. } => ButtonType::CopyText,
            Self::Disabled => ButtonType::Disabled,
            Self::SuggestDecline => ButtonType::SuggestDecline,
            Self::SuggestAccept => ButtonType::SuggestAccept,
            Self::SuggestChange => ButtonType::SuggestChange,
            Self::CreateBot { .. } => ButtonType::CreateBot,
        }
    }

    /// Returns the effective icon type used during layout (`Url` buttons pointing to a
    /// Mini App URL display the `WebView` icon, matching `history_item_components.cpp:944-947`).
    pub fn effective_icon_type(&self) -> ButtonType {
        match self {
            Self::Url { url } if is_mini_app_url(url) => ButtonType::WebView,
            other => other.button_type(),
        }
    }
}

/// A single button in a reply or inline keyboard.
///
/// Port of `HistoryMessageMarkupButton` in `history_item_reply_markup.h:80-168`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MarkupButton {
    /// Label displayed on the button.
    pub text: String,
    /// Visual customization (icon + color).
    pub visual: ButtonVisual,
    /// Button kind and payload.
    pub kind: ButtonKind,
}

impl MarkupButton {
    /// Creates a button with default visual styling.
    pub fn new(text: impl Into<String>, kind: ButtonKind) -> Self {
        Self {
            text: text.into(),
            visual: ButtonVisual::default(),
            kind,
        }
    }

    /// Creates a button with explicit visual styling.
    pub fn with_visual(text: impl Into<String>, visual: ButtonVisual, kind: ButtonKind) -> Self {
        Self {
            text: text.into(),
            visual,
            kind,
        }
    }

    /// Returns the button's [`ButtonType`].
    pub const fn button_type(&self) -> ButtonType {
        self.kind.button_type()
    }

    /// Returns the effective [`ButtonType`] for corner icon selection and min-width sizing.
    pub fn effective_icon_type(&self) -> ButtonType {
        self.kind.effective_icon_type()
    }

    /// Returns `true` if activating this button shows a loading indicator.
    pub const fn loads_on_activate(&self) -> bool {
        self.button_type().loads_on_activate()
    }

    /// Returns the corner icon for this button.
    pub fn corner_icon(&self) -> ButtonTypeIcon {
        self.effective_icon_type().icon()
    }

    /// Computes a stable lookup key for rich-page embedded bot buttons, matching
    /// `HistoryMessageMarkupButton::RichPageButtonKey` (`history_item_reply_markup.cpp:359-368`).
    pub fn rich_page_key(&self) -> Vec<u8> {
        let (button_id, peer_types, payload): (i64, u8, &[u8]) = match &self.kind {
            ButtonKind::Url { url }
            | ButtonKind::WebView { url }
            | ButtonKind::SimpleWebView { url } => (0, 0, url.as_bytes()),
            ButtonKind::Callback { data } | ButtonKind::CallbackWithPassword { data } => {
                (0, 0, data.as_slice())
            }
            ButtonKind::SwitchInline { query, peer_types } => {
                (0, peer_types.bits(), query.as_bytes())
            }
            ButtonKind::SwitchInlineSame { query } => (0, 0, query.as_bytes()),
            ButtonKind::Auth { url, button_id, .. } => (*button_id, 0, url.as_bytes()),
            ButtonKind::CopyText { text } => (0, 0, text.as_bytes()),
            ButtonKind::RequestPeer { button_id, .. } | ButtonKind::CreateBot { button_id, .. } => {
                (*button_id, 0, &[])
            }
            _ => (0, 0, &[]),
        };
        let mut key =
            format!("{};{};{};", self.button_type() as u8, button_id, peer_types).into_bytes();
        key.extend_from_slice(payload);
        key
    }
}

/// Plain input mirroring the 4 TL `ReplyMarkup` constructors (`api.tl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplyMarkupInput {
    /// `replyKeyboardMarkup` — custom reply keyboard below the compose box.
    Keyboard {
        /// Rows of buttons.
        rows: Vec<Vec<MarkupButton>>,
        /// Resize vertically to fit content.
        resize: bool,
        /// Hide keyboard after one use.
        single_use: bool,
        /// Show only to specific users.
        selective: bool,
        /// Keep keyboard visible when system keyboard is hidden.
        persistent: bool,
        /// Also force reply.
        force_reply: bool,
        /// Input field placeholder text.
        placeholder: String,
    },
    /// `replyInlineMarkup` — inline keyboard attached beneath a message bubble.
    Inline {
        /// Rows of inline buttons.
        rows: Vec<Vec<MarkupButton>>,
        /// Optional force_reply flag.
        force_reply: bool,
    },
    /// `replyKeyboardHide` — removes the active reply keyboard.
    RemoveKeyboard {
        /// Remove only for specific users.
        selective: bool,
    },
    /// `replyKeyboardForceReply` — forces the client to display a reply interface.
    ForceReply {
        /// Single-use flag.
        single_use: bool,
        /// Selective flag.
        selective: bool,
        /// Input field placeholder text.
        placeholder: String,
    },
}

/// Parsed reply markup attached to a message.
///
/// Port of `HistoryMessageMarkupData` in `history_item_reply_markup.h:181-199`
/// and `history_item_reply_markup.cpp:409-522`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMarkupData {
    /// Non-empty rows of buttons (empty rows are stripped on construction).
    pub rows: Vec<Vec<MarkupButton>>,
    /// Markup flags.
    pub flags: ReplyMarkupFlags,
    /// Optional compose input placeholder text.
    pub placeholder: String,
}

impl Default for ReplyMarkupData {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            flags: ReplyMarkupFlags::IS_NULL,
            placeholder: String::new(),
        }
    }
}

impl ReplyMarkupData {
    /// Builds [`ReplyMarkupData`] from an optional [`ReplyMarkupInput`],
    /// matching `HistoryMessageMarkupData::HistoryMessageMarkupData`.
    pub fn from_input(input: Option<ReplyMarkupInput>) -> Self {
        let Some(input) = input else {
            return Self::default();
        };
        let mut data = Self {
            rows: Vec::new(),
            flags: ReplyMarkupFlags::empty(),
            placeholder: String::new(),
        };
        match input {
            ReplyMarkupInput::Keyboard {
                rows,
                resize,
                single_use,
                selective,
                persistent,
                force_reply,
                placeholder,
            } => {
                if resize {
                    data.flags |= ReplyMarkupFlags::RESIZE;
                }
                if selective {
                    data.flags |= ReplyMarkupFlags::SELECTIVE;
                }
                if single_use {
                    data.flags |= ReplyMarkupFlags::SINGLE_USE;
                }
                if persistent {
                    data.flags |= ReplyMarkupFlags::PERSISTENT;
                }
                if force_reply {
                    data.flags |= ReplyMarkupFlags::FORCE_REPLY;
                }
                data.placeholder = placeholder;
                data.fill_rows(rows);
            }
            ReplyMarkupInput::Inline { rows, force_reply } => {
                data.flags = ReplyMarkupFlags::INLINE;
                if force_reply {
                    data.flags |= ReplyMarkupFlags::FORCE_REPLY;
                }
                data.fill_rows(rows);
            }
            ReplyMarkupInput::RemoveKeyboard { selective } => {
                data.flags = ReplyMarkupFlags::NONE;
                if selective {
                    data.flags |= ReplyMarkupFlags::SELECTIVE;
                }
            }
            ReplyMarkupInput::ForceReply {
                single_use,
                selective,
                placeholder,
            } => {
                data.flags = ReplyMarkupFlags::FORCE_REPLY;
                if selective {
                    data.flags |= ReplyMarkupFlags::SELECTIVE;
                }
                if single_use {
                    data.flags |= ReplyMarkupFlags::SINGLE_USE;
                }
                data.placeholder = placeholder;
            }
        }
        data
    }

    fn fill_rows(&mut self, input_rows: Vec<Vec<MarkupButton>>) {
        self.rows.clear();
        for row in input_rows {
            if row.is_empty() {
                continue;
            }
            for button in &row {
                if button.button_type() == ButtonType::SwitchInline {
                    self.flags |= ReplyMarkupFlags::HAS_SWITCH_INLINE;
                }
            }
            self.rows.push(row);
        }
        if self.rows.len() == 1
            && self.rows[0].len() == 1
            && self.rows[0][0].button_type() == ButtonType::Buy
        {
            self.flags |= ReplyMarkupFlags::ONLY_BUY_BUTTON;
        }
    }

    /// Creates a forwarded copy of this markup, matching
    /// `HistoryMessageMarkupData::fillForwardedData` (`history_item_reply_markup.cpp:476-508`).
    ///
    /// - `SwitchInlineSame` is converted to `SwitchInline` (empty `peer_types`).
    /// - `Auth` buttons with non-empty `forward_text` replace their visible `text`
    ///   with `forward_text` and clear `forward_text`.
    pub fn forwarded(&self) -> Self {
        if self.is_null() {
            return Self::default();
        }
        let mut rows = Vec::with_capacity(self.rows.len());
        let mut flags = self.flags;
        for row in &self.rows {
            let mut new_row = Vec::with_capacity(row.len());
            for button in row {
                let (new_text, new_kind) = match &button.kind {
                    ButtonKind::SwitchInlineSame { query } => {
                        flags |= ReplyMarkupFlags::HAS_SWITCH_INLINE;
                        (
                            button.text.clone(),
                            ButtonKind::SwitchInline {
                                query: query.clone(),
                                peer_types: PeerTypes::empty(),
                            },
                        )
                    }
                    ButtonKind::Auth {
                        url,
                        forward_text,
                        button_id,
                    } => {
                        let text = if forward_text.is_empty() {
                            button.text.clone()
                        } else {
                            forward_text.clone()
                        };
                        (
                            text,
                            ButtonKind::Auth {
                                url: url.clone(),
                                forward_text: String::new(),
                                button_id: *button_id,
                            },
                        )
                    }
                    other => (button.text.clone(), other.clone()),
                };
                new_row.push(MarkupButton {
                    text: new_text,
                    visual: button.visual,
                    kind: new_kind,
                });
            }
            if !new_row.is_empty() {
                rows.push(new_row);
            }
        }
        Self {
            rows,
            flags,
            placeholder: self.placeholder.clone(),
        }
    }

    /// Returns `true` when no markup is present (`IS_NULL`).
    pub const fn is_null(&self) -> bool {
        self.flags.contains(ReplyMarkupFlags::IS_NULL)
    }

    /// Returns `true` when rows, placeholder, and all non-`IS_NULL` flags are empty.
    pub fn is_trivial(&self) -> bool {
        self.rows.is_empty()
            && self.placeholder.is_empty()
            && (self.flags - ReplyMarkupFlags::IS_NULL).is_empty()
    }

    /// Returns `true` if this is an inline keyboard.
    pub const fn is_inline(&self) -> bool {
        self.flags.contains(ReplyMarkupFlags::INLINE)
    }

    /// Returns `true` if this markup explicitly hides/removes a reply keyboard.
    pub const fn is_remove_keyboard(&self) -> bool {
        self.flags.contains(ReplyMarkupFlags::NONE)
    }

    /// Returns `true` if `FORCE_REPLY` is set.
    pub const fn is_force_reply(&self) -> bool {
        self.flags.contains(ReplyMarkupFlags::FORCE_REPLY)
    }

    /// Returns `true` if `RESIZE` is set.
    pub const fn is_resize(&self) -> bool {
        self.flags.contains(ReplyMarkupFlags::RESIZE)
    }

    /// Returns `true` if `SINGLE_USE` is set.
    pub const fn is_single_use(&self) -> bool {
        self.flags.contains(ReplyMarkupFlags::SINGLE_USE)
    }

    /// Returns `true` if `SELECTIVE` is set.
    pub const fn is_selective(&self) -> bool {
        self.flags.contains(ReplyMarkupFlags::SELECTIVE)
    }

    /// Returns `true` if `PERSISTENT` is set.
    pub const fn is_persistent(&self) -> bool {
        self.flags.contains(ReplyMarkupFlags::PERSISTENT)
    }

    /// Returns a reference to the button at `(row, column)`, if in bounds.
    pub fn button(&self, row: usize, column: usize) -> Option<&MarkupButton> {
        self.rows.get(row).and_then(|r| r.get(column))
    }

    /// Returns `true` if this markup defines a reply keyboard panel (non-inline, non-null).
    pub const fn defines_reply_keyboard(&self) -> bool {
        !self.is_null() && !self.is_inline()
    }
}

/// Active compose-area reply keyboard state derived from the latest reply-keyboard message.
///
/// Port of `BotKeyboard::updateMarkup` state rules in `chat_helpers/bot_keyboard.cpp:335-377`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplyKeyboardState {
    /// Message owning the current reply keyboard, if any.
    pub for_msg_id: Option<FullMsgId>,
    /// Whether the keyboard has visible non-inline rows to render.
    pub has_visible_rows: bool,
    /// Whether `force_reply` is active.
    pub force_reply: bool,
    /// When `true`, keyboard expands to `max_outer_height` (i.e. `!RESIZE`).
    pub maximize_size: bool,
    /// When `true`, keyboard hides after one activation.
    pub single_use: bool,
    /// When `true`, keyboard persists when the regular keyboard is hidden.
    pub persistent: bool,
    /// Whether the markup is selective.
    pub selective: bool,
    /// Input field placeholder string.
    pub placeholder: String,
}

impl ReplyKeyboardState {
    /// Updates state for a new message's [`ReplyMarkupData`].
    /// Returns `true` if the active keyboard state changed.
    pub fn update_markup(
        &mut self,
        msg_id: Option<FullMsgId>,
        markup: Option<&ReplyMarkupData>,
        force: bool,
    ) -> bool {
        let Some(msg_id) = msg_id else {
            return self.clear();
        };
        let Some(markup) = markup else {
            return self.clear();
        };
        if !markup.defines_reply_keyboard() {
            return self.clear();
        }
        if self.for_msg_id == Some(msg_id) && !force {
            return false;
        }
        if markup.is_remove_keyboard() {
            let had = self.for_msg_id.is_some();
            *self = Self::default();
            return had;
        }

        let has_visible_rows = !markup.rows.is_empty() && !markup.is_inline();
        let force_reply = markup.is_force_reply();
        self.for_msg_id = Some(msg_id);
        self.has_visible_rows = has_visible_rows;
        self.force_reply = force_reply;
        self.maximize_size = !markup.is_resize();
        self.single_use = (force_reply && !has_visible_rows) || markup.is_single_use();
        self.persistent = markup.is_persistent();
        self.selective = markup.is_selective();
        self.placeholder = markup.placeholder.clone();
        true
    }

    /// Clears the reply keyboard state (e.g. when the owning message is removed
    /// or a single-use keyboard is dismissed). Returns `true` if state was non-empty.
    pub fn clear(&mut self) -> bool {
        if self.for_msg_id.is_some() {
            *self = Self::default();
            true
        } else {
            false
        }
    }

    /// Hides the keyboard if it belongs to `msg_id` and is single-use.
    pub fn hide_if_single_use(&mut self, msg_id: FullMsgId) -> bool {
        if self.for_msg_id == Some(msg_id) && self.single_use {
            self.clear()
        } else {
            false
        }
    }
}
