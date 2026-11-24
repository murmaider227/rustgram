//! Decoupled backend trait for Telegram Business and Telegram Premium screens.
//!
//! Port origin: `Telegram/SourceFiles/data/business/` (`BusinessInfo`, `Chatbots`,
//! `ShortcutMessages`), `api/api_chat_links.h`, `data/data_emoji_statuses.h`, and
//! `api/api_premium.h`.

use crate::business::model::{
    ActiveEmojiStatus, AwaySettings, BotSearchResult, BusinessChatLink, BusinessLocation,
    BusinessOverviewSnapshot, BusinessShortcutId, ChatIntro, ChatbotsSettings, GreetingSettings,
    QuickReplyAttachment, QuickReplyMessage, QuickReplyShortcut, WorkingHours,
};

/// Backend interface powering all Telegram Business and Premium UI screens.
///
/// Production code can back this with `tg-api` MTProto RPCs (`account.updateBusinessWorkHours`,
/// `account.updateBusinessLocation`, `account.updateBusinessIntro`,
/// `account.updateBusinessGreetingMessage`, `account.updateBusinessAwayMessage`,
/// `account.updateConnectedBot`, `account.createBusinessChatLink`, `messages.getQuickReplies`,
/// `account.updateEmojiStatus`); offline builds and tests use [`crate::business::mock::MockBusinessBackend`].
pub trait BusinessBackend: Send {
    /// Loads the full Telegram Business & Premium snapshot.
    fn load_overview(&self) -> Result<BusinessOverviewSnapshot, String>;

    /// Toggles simulated Telegram Premium membership state (for testing Free vs Premium flows).
    fn set_premium_enabled(&mut self, enabled: bool) -> Result<(), String>;

    /// Toggles the "Hide Sponsored Messages" switch (`Api::SponsoredToggle` in `settings_business.cpp`).
    fn set_sponsored_disabled(&mut self, disabled: bool) -> Result<(), String>;

    /// Saves business opening hours (`account.updateBusinessWorkHours`).
    fn save_working_hours(&mut self, hours: WorkingHours) -> Result<(), String>;

    /// Saves business location address and optional coordinates (`account.updateBusinessLocation`).
    fn save_location(&mut self, location: BusinessLocation) -> Result<(), String>;

    /// Saves Start Page / Chat Intro (`account.updateBusinessIntro`).
    fn save_chat_intro(&mut self, intro: ChatIntro) -> Result<(), String>;

    /// Saves Greeting Message settings (`account.updateBusinessGreetingMessage`).
    fn save_greeting_settings(&mut self, settings: GreetingSettings) -> Result<(), String>;

    /// Saves Away Message settings (`account.updateBusinessAwayMessage`).
    fn save_away_settings(&mut self, settings: AwaySettings) -> Result<(), String>;

    /// Searches bots by `@username` or `t.me/...` link (`settings_chatbots.cpp`).
    fn search_business_bots(&self, query: &str) -> BotSearchResult;

    /// Saves connected chatbot settings (`account.updateConnectedBot`).
    fn save_chatbots(&mut self, settings: ChatbotsSettings) -> Result<(), String>;

    /// Creates or retrieves a quick-reply shortcut by name (`ShortcutMessages::emplaceShortcut`).
    fn emplace_shortcut(&mut self, name: &str) -> Result<QuickReplyShortcut, String>;

    /// Renames an existing quick-reply shortcut (`ShortcutMessages::editShortcut`).
    fn rename_shortcut(
        &mut self,
        shortcut_id: BusinessShortcutId,
        new_name: &str,
    ) -> Result<QuickReplyShortcut, String>;

    /// Deletes a quick-reply shortcut and all its messages (`ShortcutMessages::removeShortcut`).
    fn remove_shortcut(&mut self, shortcut_id: BusinessShortcutId) -> Result<(), String>;

    /// Appends a message to a quick-reply shortcut (enforcing [`crate::business::model::SHORTCUT_MESSAGES_LIMIT`]).
    fn add_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        text: &str,
        attachment: QuickReplyAttachment,
    ) -> Result<QuickReplyMessage, String>;

    /// Edits the text of an existing message inside a shortcut.
    fn edit_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        message_id: i32,
        new_text: &str,
    ) -> Result<QuickReplyMessage, String>;

    /// Deletes a message inside a quick-reply shortcut.
    fn delete_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        message_id: i32,
    ) -> Result<(), String>;

    /// Creates a new business chat link (`account.createBusinessChatLink`).
    fn create_chat_link(&mut self, title: &str, message: &str) -> Result<BusinessChatLink, String>;

    /// Edits an existing business chat link (`account.editBusinessChatLink`).
    fn edit_chat_link(
        &mut self,
        link: &str,
        title: &str,
        message: &str,
    ) -> Result<BusinessChatLink, String>;

    /// Deletes a business chat link (`account.deleteBusinessChatLink`).
    fn delete_chat_link(&mut self, link: &str) -> Result<(), String>;

    /// Updates the user's active emoji status (`account.updateEmojiStatus`).
    fn set_emoji_status(&mut self, status: ActiveEmojiStatus) -> Result<(), String>;

    /// Leaves selected inactive channels from `ChannelsLimitBox`.
    fn leave_inactive_channels(&mut self, channel_ids: &[i64]) -> Result<usize, String>;

    /// Revokes a public link from `PublicLinksLimitBox`.
    fn revoke_public_link(&mut self, channel_id: i64) -> Result<(), String>;
}
