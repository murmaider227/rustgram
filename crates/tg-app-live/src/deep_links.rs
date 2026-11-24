//! Live MTProto RPC handlers for deep links on [`LiveSession`].
//!
//! Port origin: `Telegram/SourceFiles/core/local_url_handlers.cpp`,
//! `Telegram/SourceFiles/api/api_chat_invite.cpp`,
//! `Telegram/SourceFiles/api/api_chat_filters.cpp`,
//! `Telegram/SourceFiles/api/api_confirm_phone.cpp`,
//! and `Telegram/SourceFiles/window/window_session_controller.cpp` (`showPeerByLink`).

use std::sync::Arc;
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, peer_id_from_tl};
use tg_data::{PeerId as DataPeerId, Session};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{from_data_peer, peer_display_title, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::session_api::{
    BankCardInfo, BankCardOpenUrl, ChatInvitePreview, ChatlistInvitePreview, CloudThemePreview,
    DeepLinkInfoResult, GiftCodePreview, StickerSetPreview, UniqueGiftPreview, WallPaperPreview,
};
use tg_app_core::stand_in::types::PeerId;

const DEEP_LINK_RPC_TIMEOUT: Duration = Duration::from_secs(10);

impl LiveSession {
    fn dl_read_session<R: Send>(&self, f: impl FnOnce(&Session) -> R + Send) -> R {
        let lock = self.runtime().session();
        if let Ok(guard) = lock.try_read() {
            return f(&guard);
        }
        let arc = Arc::clone(lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let guard = arc.read().await;
            f(&guard)
        })
    }

    fn dl_write_session<R: Send>(&self, f: impl FnOnce(&mut Session) -> R + Send) -> R {
        let lock = self.runtime().session();
        if let Ok(mut guard) = lock.try_write() {
            return f(&mut guard);
        }
        let arc = Arc::clone(lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let mut guard = arc.write().await;
            f(&mut guard)
        })
    }

    fn dl_invoke<R>(&self, req: R) -> Result<R::Return, String>
    where
        R: tg_tl::RemoteCall + Send + Sync + 'static,
        R::Return: Send + 'static,
    {
        let rpc = self.runtime().rpc().clone();
        block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(DEEP_LINK_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
        })
    }

    /// Resolves a username via local cache first, falling back to live `contacts.resolveUsername`.
    pub(crate) fn deep_link_resolve_username(
        &mut self,
        username: &str,
        referer: Option<&str>,
    ) -> Result<PeerId, String> {
        let cleaned = username.trim().trim_start_matches('@');
        if cleaned.is_empty() {
            return Err("Username is empty.".to_owned());
        }
        if let Some(local) = tg_app_core::session_api::AppSession::resolve_username(self, cleaned) {
            return Ok(local);
        }
        let resp = self.dl_invoke(tl_fns::contacts::ResolveUsername {
            username: cleaned.to_owned(),
            referer: referer.filter(|s| !s.is_empty()).map(ToOwned::to_owned),
        })?;
        let tl_enums::contacts::ResolvedPeer::ResolvedPeer(data) = resp;
        let peer_id = peer_id_from_tl(&data.peer);
        self.dl_write_session(|session| {
            apply_tl_users_and_chats(session, &data.users, &data.chats);
        });
        self.refresh_cached_views();
        self.bridge_state().notify_ui();
        Ok(from_data_peer(peer_id))
    }

    /// Resolves a phone number via live `contacts.resolvePhone`.
    pub(crate) fn deep_link_resolve_phone(&mut self, phone: &str) -> Result<PeerId, String> {
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return Err("Phone number is empty.".to_owned());
        }
        let resp = self.dl_invoke(tl_fns::contacts::ResolvePhone { phone: digits })?;
        let tl_enums::contacts::ResolvedPeer::ResolvedPeer(data) = resp;
        let peer_id = peer_id_from_tl(&data.peer);
        self.dl_write_session(|session| {
            apply_tl_users_and_chats(session, &data.users, &data.chats);
        });
        self.refresh_cached_views();
        self.bridge_state().notify_ui();
        Ok(from_data_peer(peer_id))
    }

    /// Inspects a chat invite hash via `messages.checkChatInvite`.
    pub(crate) fn deep_link_check_chat_invite(
        &mut self,
        hash: &str,
    ) -> Result<ChatInvitePreview, String> {
        let resp = self.dl_invoke(tl_fns::messages::CheckChatInvite {
            hash: hash.to_owned(),
        })?;
        match resp {
            tl_enums::ChatInvite::Already(already) => {
                let peer_id = self.apply_single_chat(&already.chat);
                let title = chat_title_from_tl(&already.chat);
                Ok(ChatInvitePreview::AlreadyJoined { peer_id, title })
            }
            tl_enums::ChatInvite::Peek(peek) => {
                let peer_id = self.apply_single_chat(&peek.chat);
                let title = chat_title_from_tl(&peek.chat);
                Ok(ChatInvitePreview::AlreadyJoined { peer_id, title })
            }
            tl_enums::ChatInvite::ChatInvite(inv) => {
                let participant_names = inv
                    .participants
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(user_display_name_from_tl)
                    .collect();
                Ok(ChatInvitePreview::Invite {
                    hash: hash.to_owned(),
                    title: inv.title,
                    about: inv.about,
                    participants_count: inv.participants_count,
                    is_channel: inv.channel,
                    is_broadcast: inv.broadcast,
                    is_megagroup: inv.megagroup,
                    request_needed: inv.request_needed,
                    verified: inv.verified,
                    color_index: (inv.color.unsigned_abs() % 8) as u8,
                    participant_names,
                })
            }
        }
    }

    /// Joins a chat or channel by invite hash via `messages.importChatInvite`.
    pub(crate) fn deep_link_import_chat_invite(&mut self, hash: &str) -> Result<PeerId, String> {
        let resp = self.dl_invoke(tl_fns::messages::ImportChatInvite {
            hash: hash.to_owned(),
        })?;
        match resp {
            tl_enums::messages::ChatInviteJoinResult::Ok(ok) => {
                let joined_peer = self.apply_updates_and_find_chat(&ok.updates);
                self.refresh_cached_views();
                self.bridge_state().notify_ui();
                joined_peer
                    .ok_or_else(|| "Joined chat, but server returned no chat peer.".to_owned())
            }
            tl_enums::messages::ChatInviteJoinResult::WebView(wv) => {
                self.dl_write_session(|session| {
                    apply_tl_users_and_chats(session, &wv.users, &[]);
                });
                Err(format!(
                    "Invite requires verification through bot {} before joining.",
                    wv.bot_id
                ))
            }
        }
    }

    /// Inspects a shareable folder link via `chatlists.checkChatlistInvite`.
    pub(crate) fn deep_link_check_chatlist_invite(
        &mut self,
        slug: &str,
    ) -> Result<ChatlistInvitePreview, String> {
        let resp = self.dl_invoke(tl_fns::chatlists::CheckChatlistInvite {
            slug: slug.to_owned(),
        })?;
        match resp {
            tl_enums::chatlists::ChatlistInvite::ChatlistInvite(inv) => {
                let tl_enums::TextWithEntities::TextWithEntities(t) = inv.title;
                let peers: Vec<PeerId> = inv
                    .peers
                    .iter()
                    .map(|p| from_data_peer(peer_id_from_tl(p)))
                    .collect();
                let peer_titles = self.dl_write_session(|session| {
                    apply_tl_users_and_chats(session, &inv.users, &inv.chats);
                    peers
                        .iter()
                        .map(|&pid| peer_display_title(session, to_data_peer(pid)))
                        .collect()
                });
                self.refresh_cached_views();
                Ok(ChatlistInvitePreview {
                    slug: slug.to_owned(),
                    title: t.text,
                    emoticon: inv.emoticon,
                    peers,
                    peer_titles,
                    already_filter_id: None,
                })
            }
            tl_enums::chatlists::ChatlistInvite::Already(already) => {
                let peers: Vec<PeerId> = already
                    .missing_peers
                    .iter()
                    .map(|p| from_data_peer(peer_id_from_tl(p)))
                    .collect();
                let peer_titles = self.dl_write_session(|session| {
                    apply_tl_users_and_chats(session, &already.users, &already.chats);
                    peers
                        .iter()
                        .map(|&pid| peer_display_title(session, to_data_peer(pid)))
                        .collect()
                });
                self.refresh_cached_views();
                Ok(ChatlistInvitePreview {
                    slug: slug.to_owned(),
                    title: format!("Folder #{}", already.filter_id),
                    emoticon: None,
                    peers,
                    peer_titles,
                    already_filter_id: Some(already.filter_id),
                })
            }
        }
    }

    /// Joins a shareable folder link via `chatlists.joinChatlistInvite`.
    pub(crate) fn deep_link_join_chatlist_invite(
        &mut self,
        slug: &str,
        peers: &[PeerId],
    ) -> Result<usize, String> {
        let input_peers: Vec<tl_enums::InputPeer> = self.dl_read_session(|session| {
            peers
                .iter()
                .map(|&pid| input_peer_for(session, to_data_peer(pid)))
                .filter(|p| !matches!(p, tl_enums::InputPeer::Empty))
                .collect()
        });
        let count = input_peers.len();
        let updates = self.dl_invoke(tl_fns::chatlists::JoinChatlistInvite {
            slug: slug.to_owned(),
            peers: input_peers,
        })?;
        let _ = self.apply_updates_and_find_chat(&updates);
        self.refresh_cached_views();
        self.bridge_state().notify_ui();
        Ok(count)
    }

    /// Fetches a sticker or custom-emoji set preview via `messages.getStickerSet` and ingests
    /// its documents into [`LiveSession`]'s sticker cache so Install/Remove and previews work.
    pub(crate) fn deep_link_resolve_sticker_set(
        &mut self,
        short_name: &str,
        is_emoji: bool,
    ) -> Result<StickerSetPreview, String> {
        let resp = self.dl_invoke(tl_fns::messages::GetStickerSet {
            stickerset: tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
                short_name: short_name.to_owned(),
            }),
            hash: 0,
        })?;
        match resp {
            tl_enums::messages::StickerSet::StickerSet(s) => {
                Ok(self.ingest_sticker_set_preview(&s, is_emoji))
            }
            tl_enums::messages::StickerSet::NotModified => Ok(StickerSetPreview {
                set_id: 0,
                access_hash: 0,
                title: short_name.to_owned(),
                short_name: short_name.to_owned(),
                count: 0,
                is_emoji,
                installed: false,
                archived: false,
                official: false,
                items: Vec::new(),
            }),
        }
    }

    /// Fetches a cloud theme preview via `account.getTheme`.
    pub(crate) fn deep_link_resolve_cloud_theme(
        &mut self,
        slug: &str,
    ) -> Result<CloudThemePreview, String> {
        let tl_enums::Theme::Theme(theme) = self.dl_invoke(tl_fns::account::GetTheme {
            format: "tdesktop".to_owned(),
            theme: tl_enums::InputTheme::Slug(tl_types::InputThemeSlug {
                slug: slug.to_owned(),
            }),
        })?;
        Ok(CloudThemePreview {
            title: theme.title,
            slug: theme.slug,
            installs_count: theme.installs_count.unwrap_or(0),
        })
    }

    /// Fetches a cloud wallpaper preview via `account.getWallPaper`.
    pub(crate) fn deep_link_resolve_wallpaper(
        &mut self,
        slug: &str,
    ) -> Result<WallPaperPreview, String> {
        let resp = self.dl_invoke(tl_fns::account::GetWallPaper {
            wallpaper: tl_enums::InputWallPaper::Slug(tl_types::InputWallPaperSlug {
                slug: slug.to_owned(),
            }),
        })?;
        match resp {
            tl_enums::WallPaper::WallPaper(wp) => Ok(WallPaperPreview {
                slug: wp.slug,
                is_pattern: wp.pattern,
                is_dark: wp.dark,
            }),
            tl_enums::WallPaper::NoFile(wp) => Ok(WallPaperPreview {
                slug: slug.to_owned(),
                is_pattern: false,
                is_dark: wp.dark,
            }),
        }
    }

    /// Requests a phone-confirmation code via `account.sendConfirmPhoneCode`.
    pub(crate) fn deep_link_send_confirm_phone_code(
        &mut self,
        hash: &str,
    ) -> Result<String, String> {
        let sent = self.dl_invoke(tl_fns::account::SendConfirmPhoneCode {
            hash: hash.to_owned(),
            settings: tl_enums::CodeSettings::CodeSettings(tl_types::CodeSettings {
                allow_flashcall: false,
                current_number: false,
                allow_app_hash: false,
                allow_missed_call: false,
                allow_firebase: false,
                unknown_number: false,
                logout_tokens: None,
                token: None,
                app_sandbox: None,
            }),
        })?;
        match sent {
            tl_enums::auth::SentCode::SentCode(s) => Ok(s.phone_code_hash),
            tl_enums::auth::SentCode::Success(_) => Ok("confirmed".to_owned()),
            tl_enums::auth::SentCode::PaymentRequired(_) => {
                Err("Phone confirmation requires payment verification.".to_owned())
            }
        }
    }

    /// Submits a phone-confirmation verification code via `account.confirmPhone`
    /// (`Api::ConfirmPhone::resolve` in `api_confirm_phone.cpp:117-143`).
    pub(crate) fn deep_link_confirm_phone(
        &mut self,
        phone_code_hash: &str,
        phone_code: &str,
    ) -> Result<bool, String> {
        let clean_hash = phone_code_hash.trim();
        if clean_hash.is_empty() {
            return Err("Phone confirmation hash cannot be empty.".to_owned());
        }
        let clean_code = phone_code.trim();
        if clean_code.is_empty() {
            return Err("PHONE_CODE_EMPTY".to_owned());
        }
        let confirmed = self.dl_invoke(tl_fns::account::ConfirmPhone {
            phone_code_hash: clean_hash.to_owned(),
            phone_code: clean_code.to_owned(),
        })?;
        if confirmed {
            self.bridge_state().notify_ui();
        }
        Ok(confirmed)
    }

    /// Checks a Premium gift code via `payments.checkGiftCode`.
    pub(crate) fn deep_link_check_gift_code(
        &mut self,
        slug: &str,
    ) -> Result<GiftCodePreview, String> {
        let tl_enums::payments::CheckedGiftCode::CheckedGiftCode(code) =
            self.dl_invoke(tl_fns::payments::CheckGiftCode {
                slug: slug.to_owned(),
            })?;
        let from_title = self.dl_write_session(|session| {
            apply_tl_users_and_chats(session, &code.users, &code.chats);
            code.from_id
                .as_ref()
                .map(|p| peer_display_title(session, peer_id_from_tl(p)))
        });
        Ok(GiftCodePreview {
            slug: slug.to_owned(),
            days: code.days,
            used_date: code.used_date,
            from_title,
        })
    }

    /// Redeems a Telegram Premium gift code via `payments.applyGiftCode`.
    pub(crate) fn deep_link_apply_gift_code(&mut self, slug: &str) -> Result<(), String> {
        let trimmed = slug.trim();
        if trimmed.is_empty() {
            return Err("Gift code slug is empty.".to_owned());
        }
        let updates = self.dl_invoke(tl_fns::payments::ApplyGiftCode {
            slug: trimmed.to_owned(),
        })?;
        let _ = self.apply_updates_and_find_chat(&updates);
        self.refresh_cached_views();
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Resolves issuing bank info for a clicked card number via `payments.getBankCardData`.
    pub(crate) fn deep_link_get_bank_card_data(
        &mut self,
        number: &str,
    ) -> Result<BankCardInfo, String> {
        let trimmed = number.trim();
        if trimmed.is_empty() {
            return Err("Bank card number is empty.".to_owned());
        }
        let tl_enums::payments::BankCardData::BankCardData(data) =
            self.dl_invoke(tl_fns::payments::GetBankCardData {
                number: trimmed.to_owned(),
            })?;
        let open_urls = data
            .open_urls
            .into_iter()
            .map(
                |tl_enums::BankCardOpenUrl::BankCardOpenUrl(item)| BankCardOpenUrl {
                    name: item.name,
                    url: item.url,
                },
            )
            .collect();
        Ok(BankCardInfo {
            number: trimmed.to_owned(),
            title: data.title,
            open_urls,
        })
    }

    /// Resolves a collectible unique Star Gift via `payments.getUniqueStarGift`.
    pub(crate) fn deep_link_resolve_unique_gift(
        &mut self,
        slug: &str,
    ) -> Result<UniqueGiftPreview, String> {
        let tl_enums::payments::UniqueStarGift::UniqueStarGift(resp) =
            self.dl_invoke(tl_fns::payments::GetUniqueStarGift {
                slug: slug.to_owned(),
            })?;
        self.dl_write_session(|session| {
            apply_tl_users_and_chats(session, &resp.users, &resp.chats);
        });
        let (title, num, owner_name) = match resp.gift {
            tl_enums::StarGift::Unique(u) => (u.title, u.num, u.owner_name),
            tl_enums::StarGift::StarGift(g) => {
                (g.title.unwrap_or_else(|| slug.to_owned()), 0, None)
            }
        };
        Ok(UniqueGiftPreview {
            slug: slug.to_owned(),
            title,
            num,
            owner_name,
        })
    }

    /// Resolves a `t.me/m/<slug>` business chat link via `account.resolveBusinessChatLink`.
    pub(crate) fn deep_link_resolve_business_chat_link(
        &mut self,
        slug: &str,
    ) -> Result<(PeerId, String), String> {
        let tl_enums::account::ResolvedBusinessChatLinks::ResolvedBusinessChatLinks(resp) = self
            .dl_invoke(tl_fns::account::ResolveBusinessChatLink {
                slug: slug.to_owned(),
            })?;
        let peer_id = peer_id_from_tl(&resp.peer);
        self.dl_write_session(|session| {
            apply_tl_users_and_chats(session, &resp.users, &resp.chats);
        });
        self.refresh_cached_views();
        self.bridge_state().notify_ui();
        Ok((from_data_peer(peer_id), resp.message))
    }

    /// Queries server-provided fallback copy for unknown `tg://<path>` links via `help.getDeepLinkInfo`.
    pub(crate) fn deep_link_request_info(
        &mut self,
        path: &str,
    ) -> Result<Option<DeepLinkInfoResult>, String> {
        let resp = self.dl_invoke(tl_fns::help::GetDeepLinkInfo {
            path: path.to_owned(),
        })?;
        match resp {
            tl_enums::help::DeepLinkInfo::Empty => Ok(None),
            tl_enums::help::DeepLinkInfo::DeepLinkInfo(info) => Ok(Some(DeepLinkInfoResult {
                message: info.message,
                update_app: info.update_app,
            })),
        }
    }

    /// Checks whether a private channel (`t.me/c/<id>`) is loaded in the local session.
    pub(crate) fn deep_link_resolve_private_channel(&self, raw_channel_id: u64) -> Option<PeerId> {
        let candidate = PeerId::from_channel(raw_channel_id);
        let data_peer = to_data_peer(candidate);
        self.dl_read_session(|session| session.peer(data_peer).map(|_| candidate))
    }

    fn apply_single_chat(&mut self, chat: &tl_enums::Chat) -> PeerId {
        let peer_id = chat_peer_id_from_tl(chat);
        self.dl_write_session(|session| {
            apply_tl_users_and_chats(session, &[], std::slice::from_ref(chat));
        });
        self.refresh_cached_views();
        self.bridge_state().notify_ui();
        from_data_peer(peer_id)
    }

    fn apply_updates_and_find_chat(&self, updates: &tl_enums::Updates) -> Option<PeerId> {
        let chats: &[tl_enums::Chat] = match updates {
            tl_enums::Updates::Updates(u) => &u.chats,
            tl_enums::Updates::Combined(u) => &u.chats,
            _ => &[],
        };
        let first_chat = chats
            .first()
            .map(|c| from_data_peer(chat_peer_id_from_tl(c)));
        let _ = self.apply_updates_to_session(updates);
        first_chat
    }
}

fn chat_peer_id_from_tl(chat: &tl_enums::Chat) -> DataPeerId {
    match chat {
        tl_enums::Chat::Empty(c) => DataPeerId::from_chat(tg_data::ChatId(c.id.unsigned_abs())),
        tl_enums::Chat::Chat(c) => DataPeerId::from_chat(tg_data::ChatId(c.id.unsigned_abs())),
        tl_enums::Chat::Forbidden(c) => DataPeerId::from_chat(tg_data::ChatId(c.id.unsigned_abs())),
        tl_enums::Chat::Channel(c) => {
            DataPeerId::from_channel(tg_data::ChannelId(c.id.unsigned_abs()))
        }
        tl_enums::Chat::ChannelForbidden(c) => {
            DataPeerId::from_channel(tg_data::ChannelId(c.id.unsigned_abs()))
        }
        tl_enums::Chat::Community(c) => {
            DataPeerId::from_channel(tg_data::ChannelId(c.id.unsigned_abs()))
        }
        tl_enums::Chat::CommunityForbidden(c) => {
            DataPeerId::from_channel(tg_data::ChannelId(c.id.unsigned_abs()))
        }
    }
}

fn chat_title_from_tl(chat: &tl_enums::Chat) -> String {
    match chat {
        tl_enums::Chat::Chat(c) => c.title.clone(),
        tl_enums::Chat::Forbidden(c) => c.title.clone(),
        tl_enums::Chat::Channel(c) => c.title.clone(),
        tl_enums::Chat::ChannelForbidden(c) => c.title.clone(),
        tl_enums::Chat::Community(c) => c.title.clone(),
        tl_enums::Chat::CommunityForbidden(c) => c.title.clone(),
        tl_enums::Chat::Empty(c) => format!("Chat #{}", c.id),
    }
}

fn user_display_name_from_tl(user: &tl_enums::User) -> Option<String> {
    let tl_enums::User::User(u) = user else {
        return None;
    };
    let first = u.first_name.as_deref().unwrap_or("").trim();
    let last = u.last_name.as_deref().unwrap_or("").trim();
    let full = format!("{first} {last}").trim().to_owned();
    if !full.is_empty() {
        Some(full)
    } else {
        u.username.clone()
    }
}
