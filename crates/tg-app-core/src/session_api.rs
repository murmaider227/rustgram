//! Unified object-safe session abstraction ([`AppSession`]) shared by `WindowController`,
//! `MainWidget`, and all desktop UI layers across demo mode ([`MockSession`]) and live
//! Telegram mode ([`LiveSession`]).
//!
//! Port origin: `Telegram/SourceFiles/main/main_session.h`,
//! `Telegram/SourceFiles/window/window_session_controller.cpp`, and
//! `Telegram/SourceFiles/core/application.cpp`.

use tg_audio::AudioMsgId;

use crate::bots::backend::BotsBackend;
use crate::business::backend::BusinessBackend;
use crate::calls::backend::CallsBackend;
use crate::chat_helpers::backend::ChatHelpersBackend;
use crate::core_types::session_ids::{PendingChatOpen, session_peers};
use crate::dialogs::backend::DialogsBackend;
use crate::history::backend::{HistoryBackend, HistoryMessage};
use crate::info::backend::InfoBackend;
use crate::media::backend::{MediaBackend, MediaItemId};
use crate::notifications::backend::NotifyBackend;
use crate::payments::backend::{PassportBackend, PaymentsBackend};
use crate::settings::backend::SettingsBackend;
use crate::stand_in::backend::Backend;
use crate::stand_in::types::{AuthUser, MsgId, PeerId};
use crate::statistics::backend::StatsBackend;
use crate::stories::backend::StoriesBackend;
use crate::webview::backend::WebViewBackend;

/// Result of checking a chat invite link (`messages.checkChatInvite`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatInvitePreview {
    /// Current user is already a member (or peeking) of this chat/channel.
    AlreadyJoined {
        /// Resolved chat/channel peer ID.
        peer_id: PeerId,
        /// Display title of the chat/channel.
        title: String,
    },
    /// Join preview metadata for an unjoined chat/channel.
    Invite {
        /// Invite hash.
        hash: String,
        /// Group or channel title.
        title: String,
        /// Optional description (`about`).
        about: Option<String>,
        /// Participant / subscriber count.
        participants_count: i32,
        /// `true` if channel/supergroup.
        is_channel: bool,
        /// `true` if broadcast channel.
        is_broadcast: bool,
        /// `true` if megagroup.
        is_megagroup: bool,
        /// `true` if admin approval is required.
        request_needed: bool,
        /// `true` if verified.
        verified: bool,
        /// Avatar color palette index.
        color_index: u8,
        /// Preview names of participants returned by server.
        participant_names: Vec<String>,
    },
}

/// Preview metadata returned by `chatlists.checkChatlistInvite`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatlistInvitePreview {
    /// Shareable folder slug.
    pub slug: String,
    /// Folder display title.
    pub title: String,
    /// Optional folder emoticon.
    pub emoticon: Option<String>,
    /// Peers included in (or missing from) the folder.
    pub peers: Vec<PeerId>,
    /// Display titles corresponding to `peers`.
    pub peer_titles: Vec<String>,
    /// Existing local filter ID if this folder is already added.
    pub already_filter_id: Option<i32>,
}

/// One sticker or custom-emoji entry shown in [`crate::boxes::sticker_set::StickerSetBoxState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerSetPreviewItem {
    /// Document identifier.
    pub document_id: u64,
    /// Representative Unicode emoji associated with this sticker/emoji.
    pub alt_emoji: String,
    /// `true` for `.tgs` Lottie animated stickers.
    pub is_animated: bool,
    /// `true` for `.webm` video stickers.
    pub is_video: bool,
    /// `true` when Telegram Premium is required to use/send this item.
    pub is_premium: bool,
    /// Optional decoded RGBA8 thumbnail `(width, height, rgba_bytes)` for immediate grid preview.
    pub thumb_rgba: Option<(u32, u32, Vec<u8>)>,
}

/// Preview metadata returned by `messages.getStickerSet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerSetPreview {
    /// Numeric sticker set ID (`StickerSet.id`).
    pub set_id: u64,
    /// Sticker set access hash.
    pub access_hash: i64,
    /// Sticker or custom-emoji set title.
    pub title: String,
    /// Short name slug.
    pub short_name: String,
    /// Number of stickers / emojis in the pack.
    pub count: i32,
    /// `true` if this pack contains custom emojis.
    pub is_emoji: bool,
    /// `true` if already installed.
    pub installed: bool,
    /// `true` if currently archived.
    pub archived: bool,
    /// `true` if this is an official Telegram pack.
    pub official: bool,
    /// Preview entries displayed in `StickerSetBox`'s grid.
    pub items: Vec<StickerSetPreviewItem>,
}

/// Preview metadata returned by `account.getTheme`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudThemePreview {
    /// Theme title.
    pub title: String,
    /// Theme slug.
    pub slug: String,
    /// Total install count.
    pub installs_count: i32,
}

/// Preview metadata returned by `account.getWallPaper`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallPaperPreview {
    /// Wallpaper slug.
    pub slug: String,
    /// Whether wallpaper is a pattern overlay.
    pub is_pattern: bool,
    /// Whether wallpaper is dark.
    pub is_dark: bool,
}

/// Preview metadata returned by `payments.checkGiftCode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiftCodePreview {
    /// Gift code slug.
    pub slug: String,
    /// Subscription duration in days.
    pub days: i32,
    /// Unix timestamp when used, if already redeemed.
    pub used_date: Option<i32>,
    /// Optional sender chat/user title.
    pub from_title: Option<String>,
}

/// Preview metadata returned by `payments.getUniqueStarGift`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniqueGiftPreview {
    /// Collectible gift slug.
    pub slug: String,
    /// Gift title.
    pub title: String,
    /// Collectible serial number.
    pub num: i32,
    /// Optional owner name.
    pub owner_name: Option<String>,
}

/// Server-supplied fallback info returned by `help.getDeepLinkInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLinkInfoResult {
    /// Server-provided explanatory text.
    pub message: String,
    /// `true` when the server recommends updating the application.
    pub update_app: bool,
}

/// One external bank-app/web link returned by `payments.getBankCardData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BankCardOpenUrl {
    /// Bank or payment provider display name.
    pub name: String,
    /// External URL to open in browser/app.
    pub url: String,
}

/// Issuing bank metadata returned by `payments.getBankCardData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BankCardInfo {
    /// Bank card number that was queried.
    pub number: String,
    /// Bank title (e.g. `"Sample Bank"`).
    pub title: String,
    /// Quick-open bank URLs.
    pub open_urls: Vec<BankCardOpenUrl>,
}

/// Unified object-safe session contract consumed by [`crate::window::controller::WindowController`]
/// and [`crate::window::main_widget::MainWidget`].
///
/// Both [`MockSession`] (demo mode) and [`LiveSession`] (live MTProto mode) implement this trait
/// so the exact same window shell, navigation stack, modal boxes, settings, statistics, media
/// viewer, calls, stories, business, payments, and chat helpers run in both modes.
pub trait AppSession:
    std::any::Any
    + Backend
    + DialogsBackend
    + HistoryBackend
    + InfoBackend
    + SettingsBackend
    + MediaBackend
    + ChatHelpersBackend
    + NotifyBackend
    + StatsBackend
    + StoriesBackend
    + CallsBackend
    + WebViewBackend
    + BotsBackend
    + BusinessBackend
    + PaymentsBackend
    + PassportBackend
{
    /// Returns `&dyn Any` for optional downcasting to concrete session implementations.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Returns `&mut dyn Any` for optional mutable downcasting.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// Synchronizes cached user / borrowed view state before rendering a frame.
    fn sync_cached_user(&mut self) {}

    /// Signs in immediately using an authorized [`AuthUser`] produced by the local demo intro.
    fn sign_in_as(&mut self, _user: AuthUser) {}

    /// Resolves a username (`@handle` or `handle`) to its [`PeerId`].
    fn resolve_username(&self, username: &str) -> Option<PeerId>;

    /// Returns `true` when `peer_id` is a forum supergroup with topics enabled.
    fn is_forum_peer(&self, peer_id: PeerId) -> bool;

    /// Resolves a message `(peer_id, msg_id)` and optional album item index to a [`MediaItemId`].
    fn resolve_media_item_id(
        &self,
        peer_id: PeerId,
        msg_id: MsgId,
        album_index: Option<usize>,
    ) -> Option<MediaItemId>;

    /// Resolves a playable [`AudioMsgId`] for a specific message, if available.
    fn resolve_audio_msg_id(&self, _peer_id: PeerId, _msg_id: MsgId) -> Option<AudioMsgId> {
        None
    }

    /// Returns a sample playable voice note [`AudioMsgId`] (used as fallback in demo mode).
    fn sample_voice_id(&self) -> Option<AudioMsgId> {
        None
    }

    /// Returns a sample playable music track [`AudioMsgId`] (used as fallback in demo mode).
    fn sample_audio_id(&self) -> Option<AudioMsgId> {
        None
    }

    /// Queues an explicit Mini App / HTML5 Game launch URL for `bot_id`.
    fn set_pending_mini_app_url(&mut self, bot_id: PeerId, url: Option<String>);

    /// Takes any queued Mini App / HTML5 Game launch URL for `bot_id`.
    fn take_pending_mini_app_url(&mut self, bot_id: PeerId) -> Option<String>;

    /// Returns `true` if `bot_id` has already been confirmed by the user for opening Mini Apps
    /// without showing the consent box again (`Storage::Account::isPeerTrustedOpenWebView`).
    fn is_mini_app_bot_trusted(&self, _bot_id: PeerId) -> bool {
        false
    }

    /// Marks `bot_id` as trusted for opening Mini Apps and schedules encrypted persistence in
    /// per-account `StoredSessionSettings` (`Storage::Account::markPeerTrustedOpenWebView`).
    fn trust_mini_app_bot(&mut self, _bot_id: PeerId) {}

    /// Drains any pending chat navigation requests emitted by notifications or the media viewer.
    fn take_pending_chat_opens(&self) -> Vec<PendingChatOpen>;

    /// Sets the active peer targeted by [`StatsBackend`] screens.
    fn set_stats_target_peer(&self, peer_id: PeerId);

    /// Sends a helper message (sticker or GIF fallback) from `chat_helpers` into `peer_id`.
    fn send_helper_message(
        &mut self,
        peer_id: PeerId,
        text: String,
    ) -> Result<HistoryMessage, String>;

    /// Sends an existing sticker document into `peer_id` (`messages.sendMedia(InputMediaDocument)`
    /// in live mode; local sticker message in mock mode).
    fn send_sticker(
        &mut self,
        peer_id: PeerId,
        document_id: tg_chat::DocumentId,
        reply_to: Option<MsgId>,
        silent: bool,
    ) -> Result<HistoryMessage, String> {
        let _ = (reply_to, silent);
        let label = ChatHelpersBackend::sticker_document(self, document_id).map_or_else(
            || format!("Sticker #{}", document_id.0),
            |d| format!("{} [Sticker]", d.alt_emoji),
        );
        self.send_helper_message(peer_id, label)
    }

    /// Sends an existing GIF document into `peer_id` (`messages.sendMedia(InputMediaDocument)`
    /// in live mode; local GIF message in mock mode).
    fn send_gif(
        &mut self,
        peer_id: PeerId,
        document_id: tg_chat::DocumentId,
        title: &str,
        reply_to: Option<MsgId>,
        silent: bool,
    ) -> Result<HistoryMessage, String> {
        let _ = (document_id, reply_to, silent);
        self.send_helper_message(peer_id, format!("[GIF] {title}"))
    }

    /// Resolves `peer_id` into `(InputPeer, display_title)` for single-chat export.
    fn resolve_export_peer(
        &self,
        peer_id: PeerId,
    ) -> Option<(tg_export::stand_in::InputPeer, String)> {
        let profile = InfoBackend::peer_profile(self, peer_id)?;
        let bare = peer_id.bare() as i64;
        let input = match profile.kind {
            crate::info::backend::InfoPeerKind::SavedMessages => {
                tg_export::stand_in::InputPeer::Self_
            }
            crate::info::backend::InfoPeerKind::User | crate::info::backend::InfoPeerKind::Bot => {
                tg_export::stand_in::InputPeer::User(tg_tl::types::InputPeerUser {
                    user_id: bare,
                    access_hash: 0,
                })
            }
            crate::info::backend::InfoPeerKind::Group => {
                tg_export::stand_in::InputPeer::Chat(tg_tl::types::InputPeerChat { chat_id: bare })
            }
            crate::info::backend::InfoPeerKind::Supergroup
            | crate::info::backend::InfoPeerKind::Channel => {
                tg_export::stand_in::InputPeer::Channel(tg_tl::types::InputPeerChannel {
                    channel_id: bare,
                    access_hash: 0,
                })
            }
        };
        let title = if profile.title.trim().is_empty() {
            format!("Chat {}", peer_id.0)
        } else {
            profile.title
        };
        Some((input, title))
    }

    /// Installs a sticker or custom-emoji set from [`crate::boxes::sticker_set::StickerSetBoxState`].
    fn install_sticker_set_preview(
        &mut self,
        set_id: u64,
        _short_name: &str,
    ) -> Result<bool, String> {
        Ok(ChatHelpersBackend::install_sticker_set(
            self,
            tg_chat::StickerSetId(set_id),
        ))
    }

    /// Uninstalls a sticker or custom-emoji set from [`crate::boxes::sticker_set::StickerSetBoxState`].
    fn remove_sticker_set_preview(
        &mut self,
        set_id: u64,
        _short_name: &str,
    ) -> Result<bool, String> {
        Ok(ChatHelpersBackend::remove_sticker_set(
            self,
            tg_chat::StickerSetId(set_id),
        ))
    }

    /// Renames an owned sticker or custom-emoji pack (`stickers.renameStickerSet`,
    /// tdesktop `boxes/sticker_set_box.cpp:347-358`) and returns the refreshed preview when live.
    fn rename_sticker_set_preview(
        &mut self,
        _set_id: u64,
        _short_name: &str,
        _new_title: &str,
    ) -> Result<Option<StickerSetPreview>, String> {
        Ok(None)
    }

    /// Moves a sticker inside an owned pack to `new_position` (`stickers.changeStickerPosition`,
    /// tdesktop `boxes/sticker_set_box.cpp:1592-1602`) and returns the refreshed preview when live.
    fn change_sticker_position_preview(
        &mut self,
        _set_id: u64,
        _short_name: &str,
        _document_id: u64,
        _new_position: usize,
    ) -> Result<Option<StickerSetPreview>, String> {
        Ok(None)
    }

    /// Removes a sticker from an owned pack (`stickers.removeStickerFromSet`,
    /// tdesktop `boxes/sticker_set_box.cpp:1939-1949`) and returns the refreshed preview when live.
    fn remove_sticker_from_set_preview(
        &mut self,
        _set_id: u64,
        _short_name: &str,
        _document_id: u64,
    ) -> Result<Option<StickerSetPreview>, String> {
        Ok(None)
    }

    /// Permanently deletes an owned sticker or custom-emoji pack (`stickers.deleteStickerSet`,
    /// tdesktop `boxes/sticker_set_box.cpp:922` / `api/api_stickers_creator.cpp:369-376`).
    fn delete_sticker_set_preview(
        &mut self,
        _set_id: u64,
        _short_name: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Creates a boxed [`SettingsBackend`] sharing this session's state.
    fn create_settings_backend(&self) -> Box<dyn SettingsBackend>;

    /// Creates a boxed [`StatsBackend`] sharing this session's state.
    fn create_stats_backend(&self) -> Box<dyn StatsBackend>;

    /// Creates a boxed [`NotifyBackend`] sharing this session's state.
    fn create_notify_backend(&self) -> Box<dyn NotifyBackend>;

    /// Creates a boxed [`WebViewBackend`] sharing this session's state.
    fn create_webview_backend(&self) -> Box<dyn WebViewBackend>;

    /// Creates a boxed [`CallsBackend`] sharing this session's state.
    fn create_calls_backend(&self) -> Box<dyn CallsBackend>;

    /// Creates a boxed [`StoriesBackend`] sharing this session's state.
    fn create_stories_backend(&self) -> Box<dyn StoriesBackend>;

    /// Creates a boxed [`BusinessBackend`] sharing this session's state.
    fn create_business_backend(&self) -> Box<dyn BusinessBackend>;

    /// Creates a boxed [`PaymentsBackend`] sharing this session's state.
    fn create_payments_backend(&self) -> Box<dyn PaymentsBackend>;

    /// Creates a boxed [`PassportBackend`] sharing this session's state.
    fn create_passport_backend(&self) -> Box<dyn PassportBackend>;

    /// Returns `true` when backed by a live Telegram MTProto session rather than local mock data.
    fn is_live_session(&self) -> bool {
        false
    }

    /// Called once when `peer_id` becomes the chat shown in the history column, before the
    /// history view asks for its data, so a live session can send the chat's requests together
    /// (tdesktop sends them asynchronously from `HistoryWidget::showHistory`).
    fn prepare_open_chat(&self, _peer_id: PeerId) {}

    /// Resolves a username (`@handle` or `handle`), hitting `contacts.resolveUsername` in live mode
    /// when not yet present in the local cache.
    fn resolve_username_remote(
        &mut self,
        username: &str,
        _referer: Option<&str>,
    ) -> Result<PeerId, String> {
        self.resolve_username(username)
            .ok_or_else(|| format!("Username @{username} was not found."))
    }

    /// Resolves a phone number (`contacts.resolvePhone` in live mode).
    fn resolve_phone_remote(&mut self, phone: &str) -> Result<PeerId, String> {
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        InfoBackend::all_peers(self)
            .into_iter()
            .find(|p| {
                let peer_digits: String = p
                    .phone
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect();
                !digits.is_empty() && peer_digits == digits
            })
            .map(|p| p.id)
            .ok_or_else(|| format!("No user found for phone +{digits}."))
    }

    /// Resolves a private channel ID (`t.me/c/<id>`) if loaded in the session.
    fn resolve_private_channel_peer(&self, raw_channel_id: u64) -> Option<PeerId> {
        let candidate = PeerId::from_channel(raw_channel_id);
        InfoBackend::peer_profile(self, candidate).map(|p| p.id)
    }

    /// Inspects a chat invite link (`messages.checkChatInvite`).
    fn check_chat_invite(&mut self, hash: &str) -> Result<ChatInvitePreview, String> {
        Ok(ChatInvitePreview::Invite {
            hash: hash.to_owned(),
            title: format!("Invite Group ({hash})"),
            about: Some("Preview of invited Telegram group.".to_owned()),
            participants_count: 12,
            is_channel: false,
            is_broadcast: false,
            is_megagroup: true,
            request_needed: false,
            verified: false,
            color_index: 2,
            participant_names: vec!["Mira Vance".to_owned(), "Theo Reed".to_owned()],
        })
    }

    /// Joins a chat/channel by invite hash (`messages.importChatInvite`).
    fn import_chat_invite(&mut self, _hash: &str) -> Result<PeerId, String> {
        Ok(session_peers::GROUP_CORE)
    }

    /// Inspects a shareable chat folder link (`chatlists.checkChatlistInvite`).
    fn check_chatlist_invite(&mut self, slug: &str) -> Result<ChatlistInvitePreview, String> {
        Ok(ChatlistInvitePreview {
            slug: slug.to_owned(),
            title: format!("Shared Folder ({slug})"),
            emoticon: Some("📁".to_owned()),
            peers: vec![session_peers::GROUP_CORE],
            peer_titles: vec!["Rustgram Core".to_owned()],
            already_filter_id: None,
        })
    }

    /// Joins a shareable chat folder link (`chatlists.joinChatlistInvite`).
    fn join_chatlist_invite(&mut self, _slug: &str, peers: &[PeerId]) -> Result<usize, String> {
        Ok(peers.len())
    }

    /// Fetches the pack of a sticker by its set id and access hash (`messages.getStickerSet`
    /// with `inputStickerSetID`), as `StickerSetBox` does for a clicked sticker.
    fn resolve_sticker_set_by_id(
        &mut self,
        _set_id: u64,
        _access_hash: u64,
    ) -> Result<StickerSetPreview, String> {
        Err("STICKERSET_INVALID".to_owned())
    }

    /// Fetches sticker or custom-emoji pack metadata (`messages.getStickerSet`).
    fn resolve_sticker_set(
        &mut self,
        short_name: &str,
        is_emoji: bool,
    ) -> Result<StickerSetPreview, String> {
        let sample_emojis = if is_emoji {
            ["✨", "🔥", "💎", "🚀", "🎯", "⚡", "🎉", "💡"]
        } else {
            ["😀", "😎", "🥳", "🐱", "🌟", "🔥", "🎉", "👍"]
        };
        let items = sample_emojis
            .iter()
            .enumerate()
            .map(|(i, emoji)| StickerSetPreviewItem {
                document_id: 900_000 + (i as u64) + 1,
                alt_emoji: (*emoji).to_owned(),
                is_animated: i % 3 == 0,
                is_video: false,
                is_premium: false,
                thumb_rgba: None,
            })
            .collect();
        Ok(StickerSetPreview {
            set_id: 0,
            access_hash: 0,
            title: short_name.to_owned(),
            short_name: short_name.to_owned(),
            count: sample_emojis.len() as i32,
            is_emoji,
            installed: false,
            archived: false,
            official: false,
            items,
        })
    }

    /// Fetches cloud theme metadata (`account.getTheme`).
    fn resolve_cloud_theme(&mut self, slug: &str) -> Result<CloudThemePreview, String> {
        Ok(CloudThemePreview {
            title: slug.to_owned(),
            slug: slug.to_owned(),
            installs_count: 1,
        })
    }

    /// Fetches cloud wallpaper metadata (`account.getWallPaper`).
    fn resolve_wallpaper(&mut self, slug: &str) -> Result<WallPaperPreview, String> {
        Ok(WallPaperPreview {
            slug: slug.to_owned(),
            is_pattern: false,
            is_dark: false,
        })
    }

    /// Requests a phone confirmation code (`account.sendConfirmPhoneCode`).
    fn send_confirm_phone_code(&mut self, _hash: &str) -> Result<String, String> {
        Ok("mock_confirm_hash".to_owned())
    }

    /// Confirms a phone number ownership code (`account.confirmPhone`, `api_confirm_phone.cpp`).
    fn confirm_phone_code(
        &mut self,
        _phone_code_hash: &str,
        _phone_code: &str,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// Checks a Telegram Premium gift code (`payments.checkGiftCode`).
    fn check_gift_code(&mut self, slug: &str) -> Result<GiftCodePreview, String> {
        Ok(GiftCodePreview {
            slug: slug.to_owned(),
            days: 90,
            used_date: None,
            from_title: None,
        })
    }

    /// Redeems a Telegram Premium gift code (`payments.applyGiftCode`).
    fn apply_gift_code(&mut self, slug: &str) -> Result<(), String> {
        if slug.trim().is_empty() {
            return Err("Gift code slug is empty.".to_owned());
        }
        Ok(())
    }

    /// Resolves issuing bank info for a clicked card number (`payments.getBankCardData`).
    fn get_bank_card_data(&mut self, number: &str) -> Result<BankCardInfo, String> {
        let trimmed = number.trim();
        if trimmed.is_empty() {
            return Err("Bank card number is empty.".to_owned());
        }
        Ok(BankCardInfo {
            number: trimmed.to_owned(),
            title: "Sample Test Bank".to_owned(),
            open_urls: vec![BankCardOpenUrl {
                name: "Sample Bank Online".to_owned(),
                url: "https://example.invalid/bank-card".to_owned(),
            }],
        })
    }

    /// Fetches unique collectible Star Gift metadata (`payments.getUniqueStarGift`).
    fn resolve_unique_gift(&mut self, slug: &str) -> Result<UniqueGiftPreview, String> {
        Ok(UniqueGiftPreview {
            slug: slug.to_owned(),
            title: slug.to_owned(),
            num: 1,
            owner_name: None,
        })
    }

    /// Resolves a business chat link (`account.resolveBusinessChatLink`).
    fn resolve_business_chat_link(&mut self, slug: &str) -> Result<(PeerId, String), String> {
        Ok((session_peers::USER_MIRA, format!("Business link: {slug}")))
    }

    /// Queries server fallback copy for an unknown `tg://` path (`help.getDeepLinkInfo`).
    fn request_deep_link_info(
        &mut self,
        _path: &str,
    ) -> Result<Option<DeepLinkInfoResult>, String> {
        Ok(None)
    }
}
