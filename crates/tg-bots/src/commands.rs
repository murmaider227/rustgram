//! Port of bot commands, scoped command sets, menu buttons, and bot info model from:
//! - `data/data_peer_bot_command.h`
//! - `data/data_peer_bot_commands.h` / `.cpp`
//! - `data/data_user.h` / `.cpp` (`BotInfo`, `setBotInfo`)
//! - `data/data_peer.cpp` (`ApplyBotMenuButton`)
//! - `chat_helpers/bot_command.h` / `.cpp` (`WrapCommandInChat`)
//! - `inline_bots/bot_attach_web_view.cpp` (`botOpenPrivacyPolicy`)

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::stand_in::{ChatAdminRights, DocumentId, PeerId, PhotoId, Rgba, UserId};

/// A single slash command advertised by a bot (`Data::BotCommand`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BotCommand {
    /// Command identifier without the leading `/` (e.g. `"start"`, `"help"`).
    pub command: String,
    /// Human-readable description shown in the command menu.
    pub description: String,
    /// Whether this command is ephemeral (`data_peer_bot_command.h:15`).
    pub ephemeral: bool,
}

impl BotCommand {
    /// Creates a non-ephemeral [`BotCommand`].
    pub fn new(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            ephemeral: false,
        }
    }
}

/// Command visibility scope mirroring all 7 TL `BotCommandScope` constructors (`schema/api.tl:1500-1506`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub enum BotCommandScope {
    /// `botCommandScopeDefault` — fallback scope for all chats.
    #[default]
    Default,
    /// `botCommandScopeUsers` — all private chats with users.
    Users,
    /// `botCommandScopeChats` — all group and supergroup chats.
    Chats,
    /// `botCommandScopeChatAdmins` — all group and supergroup administrators.
    ChatAdmins,
    /// `botCommandScopePeer` — a specific chat or user.
    Peer(PeerId),
    /// `botCommandScopePeerAdmins` — administrators of a specific group/supergroup.
    PeerAdmins(PeerId),
    /// `botCommandScopePeerUser` — a specific member of a specific group/supergroup.
    PeerUser {
        /// Target group/supergroup peer id.
        peer: PeerId,
        /// Target user id inside that chat.
        user: UserId,
    },
}

/// Chat context used to resolve the highest-precedence [`BotCommandScope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandResolutionContext {
    /// Peer id of the active chat.
    pub peer_id: PeerId,
    /// `true` for a private 1-on-1 chat, `false` for a group/supergroup.
    pub is_private_chat: bool,
    /// `true` if the current user is an administrator of the group/supergroup.
    pub is_admin: bool,
    /// Current user's id.
    pub self_user_id: UserId,
}

/// Stores bot commands per `(BotCommandScope, lang_code)` and resolves the most specific
/// command list matching a chat context (following Telegram's Bot API scope precedence).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedBotCommands {
    entries: BTreeMap<(BotCommandScope, String), Vec<BotCommand>>,
}

impl ScopedBotCommands {
    /// Creates an empty scoped command store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets commands for `(scope, lang_code)` (`bots.setBotCommands`).
    pub fn set(
        &mut self,
        scope: BotCommandScope,
        lang_code: impl Into<String>,
        commands: Vec<BotCommand>,
    ) {
        let lang = lang_code.into().to_ascii_lowercase();
        if commands.is_empty() {
            self.entries.remove(&(scope, lang));
        } else {
            self.entries.insert((scope, lang), commands);
        }
    }

    /// Removes commands for `(scope, lang_code)` (`bots.resetBotCommands`).
    pub fn reset(&mut self, scope: BotCommandScope, lang_code: &str) -> bool {
        self.entries
            .remove(&(scope, lang_code.to_ascii_lowercase()))
            .is_some()
    }

    /// Gets the exact command list stored for `(scope, lang_code)`, if any (`bots.getBotCommands`).
    pub fn get_exact(&self, scope: BotCommandScope, lang_code: &str) -> Option<&[BotCommand]> {
        self.entries
            .get(&(scope, lang_code.to_ascii_lowercase()))
            .map(Vec::as_slice)
    }

    /// Resolves commands for `ctx` and `lang_code` using Telegram's scope specificity order:
    /// - Private chat: `Peer(peer)` -> `Users` -> `Default`
    /// - Group admin: `PeerUser(peer, self)` -> `PeerAdmins(peer)` -> `Peer(peer)` -> `ChatAdmins` -> `Chats` -> `Default`
    /// - Group member: `PeerUser(peer, self)` -> `Peer(peer)` -> `Chats` -> `Default`
    ///
    /// For each scope, tries the exact `lang_code` first and falls back to `""` (default language).
    pub fn resolve(&self, ctx: &CommandResolutionContext, lang_code: &str) -> &[BotCommand] {
        let lang = lang_code.to_ascii_lowercase();
        let candidates: Vec<BotCommandScope> = if ctx.is_private_chat {
            vec![
                BotCommandScope::Peer(ctx.peer_id),
                BotCommandScope::Users,
                BotCommandScope::Default,
            ]
        } else if ctx.is_admin {
            vec![
                BotCommandScope::PeerUser {
                    peer: ctx.peer_id,
                    user: ctx.self_user_id,
                },
                BotCommandScope::PeerAdmins(ctx.peer_id),
                BotCommandScope::Peer(ctx.peer_id),
                BotCommandScope::ChatAdmins,
                BotCommandScope::Chats,
                BotCommandScope::Default,
            ]
        } else {
            vec![
                BotCommandScope::PeerUser {
                    peer: ctx.peer_id,
                    user: ctx.self_user_id,
                },
                BotCommandScope::Peer(ctx.peer_id),
                BotCommandScope::Chats,
                BotCommandScope::Default,
            ]
        };

        for scope in candidates {
            if !lang.is_empty()
                && let Some(cmds) = self.entries.get(&(scope, lang.clone()))
            {
                return cmds.as_slice();
            }
            if let Some(cmds) = self.entries.get(&(scope, String::new())) {
                return cmds.as_slice();
            }
        }
        &[]
    }
}

/// Group bot-presence status matching `Data::BotStatus` (`data/data_peer_bot_commands.h:14-18`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[repr(i32)]
pub enum BotStatus {
    /// Chat is known to contain no bots (`-1`).
    NoBots = -1,
    /// Bot presence not yet loaded (`0`).
    #[default]
    Unknown = 0,
    /// Chat has one or more bots (`2`).
    HasBots = 2,
}

/// Per-bot command list entry (`Data::BotCommands`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCommandsEntry {
    /// Bot user id.
    pub user_id: UserId,
    /// Commands advertised by this bot in the chat.
    pub commands: Vec<BotCommand>,
}

/// Map of bot commands present in a group chat (`Data::ChatBotCommands`).
///
/// Port of `data/data_peer_bot_commands.cpp:12-33`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatBotCommands {
    by_bot: BTreeMap<UserId, Vec<BotCommand>>,
}

impl ChatBotCommands {
    /// Creates an empty group bot-command registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates commands from a fresh list of [`BotCommandsEntry`], returning `true` if changed.
    ///
    /// Matches `ChatBotCommands::update` (`data/data_peer_bot_commands.cpp:12-33`):
    /// an empty input clears the map; non-empty entries insert/update per bot and remove
    /// bots whose command list became empty.
    pub fn update(&mut self, list: Vec<BotCommandsEntry>) -> bool {
        if list.is_empty() {
            let changed = !self.by_bot.is_empty();
            self.by_bot.clear();
            return changed;
        }
        let mut changed = false;
        for entry in list {
            if entry.commands.is_empty() {
                changed |= self.by_bot.remove(&entry.user_id).is_some();
            } else {
                let current = self.by_bot.entry(entry.user_id).or_default();
                if *current != entry.commands {
                    *current = entry.commands;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Returns commands registered for `bot_id`, if any.
    pub fn commands_for(&self, bot_id: UserId) -> Option<&[BotCommand]> {
        self.by_bot.get(&bot_id).map(Vec::as_slice)
    }

    /// Iterates over all `(bot_id, commands)` pairs in the chat.
    pub fn iter(&self) -> impl Iterator<Item = (UserId, &[BotCommand])> {
        self.by_bot.iter().map(|(&id, cmds)| (id, cmds.as_slice()))
    }

    /// Returns `true` if no bots have commands registered in this chat.
    pub fn is_empty(&self) -> bool {
        self.by_bot.is_empty()
    }
}

/// Appends `@bot_username` to a slash command when sent inside a group that has bots.
///
/// Exact port of `Bot::WrapCommandInChat` (`chat_helpers/bot_command.cpp:32-47`):
/// appends `@<username>` iff `bot_username` is non-empty, `peer_bot_status != BotStatus::NoBots`,
/// and `command` does not already contain `@` at byte index `>= 2`.
pub fn wrap_command_in_chat(
    peer_bot_status: BotStatus,
    command: &str,
    bot_username: &str,
) -> String {
    if bot_username.is_empty() || peer_bot_status == BotStatus::NoBots {
        return command.to_string();
    }
    let has_at_after_prefix = command.find('@').is_some_and(|idx| idx >= 2);
    if has_at_after_prefix {
        command.to_string()
    } else {
        format!("{command}@{bot_username}")
    }
}

/// Bot menu button configuration (`BotMenuButton` in `schema/api.tl:1573-1575`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BotMenuButton {
    /// `botMenuButtonDefault` — default client behavior (shows commands if any exist).
    #[default]
    Default,
    /// `botMenuButtonCommands` — explicitly shows the bot's slash-command menu.
    Commands,
    /// `botMenuButton` — opens a Mini App WebView with the given label and URL.
    WebApp {
        /// Button label.
        text: String,
        /// WebView URL.
        url: String,
    },
}

impl BotMenuButton {
    /// Returns `true` if this button launches a Web App with non-empty text and URL.
    pub fn is_web_app(&self) -> bool {
        matches!(self, Self::WebApp { text, url } if !text.is_empty() && !url.is_empty())
    }
}

/// Optional bot-configured Mini App header/background colors (`MTPBotAppSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BotAppColors {
    /// Day-mode title bar background color.
    pub title_day: Option<Rgba>,
    /// Night-mode title bar background color.
    pub title_night: Option<Rgba>,
    /// Day-mode body background color.
    pub body_day: Option<Rgba>,
    /// Night-mode body background color.
    pub body_night: Option<Rgba>,
}

/// Third-party verifier settings attached to a bot (`BotVerifierSettings` in `data_user.h:45-58`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BotVerifierSettings {
    /// Custom emoji icon document id (`0` if unset).
    pub icon_id: DocumentId,
    /// Verifying organization name.
    pub company: String,
    /// Optional custom description text.
    pub custom_description: String,
    /// Whether the bot may edit its custom verification description.
    pub can_modify_description: bool,
}

/// Bot metadata attached to a bot user (`BotInfo` in `data/data_user.h:60-125`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BotInfo {
    /// "What can this bot do?" description text.
    pub description: String,
    /// Placeholder shown in the compose input after `@bot ` for inline bots.
    pub inline_placeholder: String,
    /// Slash commands returned in `BotInfo`.
    pub commands: Vec<BotCommand>,
    /// Description banner photo id, if any.
    pub description_photo_id: Option<PhotoId>,
    /// Description banner animation/document id, if any.
    pub description_document_id: Option<DocumentId>,
    /// Menu button beside the compose input.
    pub menu_button: BotMenuButton,
    /// Bot privacy policy URL (`privacy_policy_url`).
    pub privacy_policy_url: String,
    /// Mini App header/body theme overrides.
    pub app_colors: BotAppColors,
    /// Pending `/start` token (e.g. from deep link or `switch_pm`).
    pub start_token: String,
    /// Suggested default admin rights when adding the bot to a group.
    pub group_admin_rights: ChatAdminRights,
    /// Suggested default admin rights when adding the bot to a channel.
    pub channel_admin_rights: ChatAdminRights,
    /// Optional third-party verifier settings.
    pub verifier_settings: Option<BotVerifierSettings>,
    /// Bot info version counter.
    pub version: i32,
    /// Incremented whenever description / description photo / description document changes.
    pub description_version: i32,
    /// Monthly active user count.
    pub active_users: i32,
    /// Whether full `BotInfo` has been initialized from the server.
    pub inited: bool,
    /// Bot can read all group messages (privacy mode disabled).
    pub reads_all_history: bool,
    /// Bot cannot be added to groups.
    pub cant_join_groups: bool,
    /// Bot supports the attachment menu (`bot_attach_menu`).
    pub supports_attach_menu: bool,
    /// Current user can edit this bot's info.
    pub can_edit_information: bool,
    /// Bot has permission to manage the user's emoji status.
    pub can_manage_emoji_status: bool,
    /// Bot supports Telegram Business integration.
    pub supports_business: bool,
    /// Bot has a Main Mini App (`bot_has_main_app`).
    pub has_main_app: bool,
    /// Users can create forum topics with this bot.
    pub user_creates_topics: bool,
    /// Bot can manage child bots (`can_manage_bots`).
    pub can_manage_bots: bool,
    /// Bot supports guest chat.
    pub supports_guest_chat: bool,
}

/// Update payload applied to [`BotInfo`] (plain mirror of `MTPDbotInfo`, `data_user.cpp:476-562`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BotInfoUpdate {
    /// Updated description text.
    pub description: String,
    /// Updated description photo id.
    pub description_photo_id: Option<PhotoId>,
    /// Updated description document id.
    pub description_document_id: Option<DocumentId>,
    /// Updated command list.
    pub commands: Vec<BotCommand>,
    /// Updated menu button.
    pub menu_button: BotMenuButton,
    /// Updated privacy policy URL.
    pub privacy_policy_url: String,
    /// Updated app color overrides.
    pub app_colors: BotAppColors,
    /// Updated verifier settings.
    pub verifier_settings: Option<BotVerifierSettings>,
}

/// Result of applying a [`BotInfoUpdate`] to [`BotInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotInfoApplyResult {
    /// Whether description / photo / document changed (incremented `description_version`).
    pub description_changed: bool,
    /// Whether commands, menu button, privacy policy, or verifier settings changed.
    pub metadata_changed: bool,
}

impl BotInfo {
    /// Applies a [`BotInfoUpdate`] in place, matching `UserData::setBotInfo`
    /// (`data/data_user.cpp:476-562`) and `Data::ApplyBotMenuButton` (`data/data_peer.cpp:164-183`).
    pub fn apply_update(&mut self, update: BotInfoUpdate) -> BotInfoApplyResult {
        let mut description_changed = false;
        if self.description != update.description {
            self.description = update.description;
            self.description_version += 1;
            description_changed = true;
        }
        if self.description_photo_id != update.description_photo_id {
            self.description_photo_id = update.description_photo_id;
            self.description_version += 1;
            description_changed = true;
        }
        if self.description_document_id != update.description_document_id {
            self.description_document_id = update.description_document_id;
            self.description_version += 1;
            description_changed = true;
        }

        let changed_commands = self.commands != update.commands;
        self.commands = update.commands;

        let changed_button = self.apply_menu_button(update.menu_button);
        self.inited = true;

        let changed_privacy = self.privacy_policy_url != update.privacy_policy_url;
        self.privacy_policy_url = update.privacy_policy_url;

        self.app_colors = update.app_colors;

        let changed_verifier = self.verifier_settings != update.verifier_settings;
        self.verifier_settings = update.verifier_settings;

        BotInfoApplyResult {
            description_changed,
            metadata_changed: changed_commands
                || changed_button
                || changed_privacy
                || changed_verifier,
        }
    }

    /// Updates `menu_button`, returning `true` if it changed (`Data::ApplyBotMenuButton`).
    pub fn apply_menu_button(&mut self, button: BotMenuButton) -> bool {
        if self.menu_button != button {
            self.menu_button = button;
            true
        } else {
            false
        }
    }

    /// Returns `true` if this bot supports inline queries (non-empty `inline_placeholder`).
    pub fn supports_inline(&self) -> bool {
        !self.inline_placeholder.is_empty()
    }
}

/// Resolution action when opening a bot's privacy policy (`WebViewInstance::botOpenPrivacyPolicy`,
/// `inline_bots/bot_attach_web_view.cpp:2394-2449`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrivacyPolicyAction {
    /// Open the bot's explicit `privacy_policy_url`.
    OpenUrl(String),
    /// Send `/privacy` (preserving the bot's exact casing) because the bot defines that command.
    SendPrivacyCommand(String),
    /// Open Telegram's default bot privacy policy page.
    OpenDefaultTelegramPolicy,
}

/// Resolves how to open a bot's privacy policy.
///
/// Exact precedence from `bot_attach_web_view.cpp:2394-2449`:
/// 1. Non-empty `bot_info.privacy_policy_url`
/// 2. Case-insensitive `"privacy"` command in `bot_info.commands` -> `"/<original>"`
/// 3. Fallback to Telegram's default privacy policy URL.
pub fn resolve_privacy_policy_action(bot_info: &BotInfo) -> PrivacyPolicyAction {
    if !bot_info.privacy_policy_url.is_empty() {
        return PrivacyPolicyAction::OpenUrl(bot_info.privacy_policy_url.clone());
    }
    for cmd in &bot_info.commands {
        if cmd.command.eq_ignore_ascii_case("privacy") {
            return PrivacyPolicyAction::SendPrivacyCommand(format!("/{}", cmd.command));
        }
    }
    PrivacyPolicyAction::OpenDefaultTelegramPolicy
}
