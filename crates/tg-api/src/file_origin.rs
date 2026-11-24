//! File reference origin resolution, accumulation, and refresh-and-retry helpers.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/data_file_origin.h`
//! - `Telegram/SourceFiles/data/data_file_origin.cpp` (`FileReferenceAccumulator`, `GetFileReferences`)
//! - `Telegram/SourceFiles/apiwrap.cpp:2835-3100` (`ApiWrap::refreshFileReference`)
//! - `Telegram/SourceFiles/storage/download_manager_mtproto.cpp:457-476, 929-946`
//!   (`DownloadMtprotoTask::refreshFileReferenceFrom`, `normalPartFailed`).

use std::collections::HashMap;
use std::future::Future;

use tg_data::{PeerId, Session, UserId};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::convert::{input_channel_for, input_peer_for, input_user_for};
use crate::error::ApiError;
use crate::rpc::{InvocationError, RpcClient};

/// Origin metadata describing which Telegram RPC can refetch fresh `file_reference` bytes
/// when a file download or media call fails with `FILE_REFERENCE_EXPIRED` / `FILE_REFERENCE_INVALID`
/// (port of `Data::FileOrigin` in `data/data_file_origin.h`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FileOrigin {
    /// Regular private/group/channel message (`messages.getMessages` or `channels.getMessages`).
    Message {
        /// Peer owning the message (channel uses `channels.getMessages`, user/chat uses `messages.getMessages`).
        peer_id: PeerId,
        /// Server message ID.
        msg_id: i32,
    },
    /// Scheduled message (`messages.getScheduledMessages`).
    ScheduledMessage {
        /// Peer owning the scheduled message.
        peer_id: PeerId,
        /// Scheduled message ID.
        msg_id: i32,
    },
    /// User profile photo (`photos.getUserPhotos` with `offset=-1, max_id=photo_id, limit=1`).
    UserPhoto {
        /// Owner user ID.
        user_id: UserId,
        /// Photo ID.
        photo_id: u64,
    },
    /// Full user profile (`users.getFullUser`).
    FullUser {
        /// Target user ID.
        user_id: UserId,
    },
    /// Chat or channel photo (`messages.getFullChat` or `channels.getFullChannel`).
    PeerPhoto {
        /// Target chat or channel peer ID.
        peer_id: PeerId,
    },
    /// Installed/remote sticker set (`messages.getStickerSet`).
    StickerSet {
        /// Sticker set ID.
        set_id: u64,
        /// Sticker set access hash.
        access_hash: u64,
    },
    /// Cloud recent stickers or masks (`messages.getRecentStickers`).
    RecentStickers {
        /// `true` for mask/attached stickers, `false` for regular recent stickers.
        attached: bool,
    },
    /// Favorite stickers (`messages.getFavedStickers`).
    FavedStickers,
    /// Saved GIFs (`messages.getSavedGifs`).
    SavedGifs,
    /// Cloud wallpaper (`account.getWallPaper` by ID/access_hash or slug).
    Wallpaper {
        /// Wallpaper ID.
        paper_id: u64,
        /// Wallpaper access hash.
        access_hash: u64,
        /// Optional non-self owner user ID; when present and `slug` is non-empty, tdesktop queries
        /// by `inputWallPaperSlug` (`apiwrap.cpp:3071-3078`).
        owner_id: Option<UserId>,
        /// Optional wallpaper slug.
        slug: String,
    },
    /// Cloud theme (`account.getTheme`).
    Theme {
        /// Theme ID.
        theme_id: u64,
        /// Theme access hash.
        access_hash: u64,
        /// Theme format (e.g. `"tdesktop"`).
        format: String,
    },
    /// Saved notification ringtones (`account.getSavedRingtones`).
    Ringtones,
    /// Telegram Premium promo preview videos (`help.getPremiumPromo`).
    PremiumPreviews,
    /// Cached webpage / Instant View (`messages.getWebPage`).
    WebPage {
        /// Page URL.
        url: String,
    },
    /// Peer story (`stories.getStoriesByID`).
    Story {
        /// Peer owning the story.
        peer_id: PeerId,
        /// Story ID.
        story_id: i32,
    },
    /// User saved profile music (`users.getSavedMusicByID`).
    SavedMusic {
        /// User owning the saved music.
        user_id: UserId,
        /// Document ID.
        doc_id: u64,
        /// Document access hash.
        access_hash: u64,
        /// Current (stale) file reference bytes.
        file_reference: Vec<u8>,
    },
}

/// Refreshed `file_reference` bytes collected from origin RPC responses, keyed by photo/document ID
/// (port of `Data::UpdatedFileReferences` and `FileReferenceAccumulator` in `data/data_file_origin.cpp`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpdatedFileReferences {
    /// Fresh `file_reference` bytes keyed by photo ID.
    pub photos: HashMap<u64, Vec<u8>>,
    /// Fresh `file_reference` bytes keyed by document ID.
    pub documents: HashMap<u64, Vec<u8>>,
}

impl UpdatedFileReferences {
    /// Creates an empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if no references were collected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.photos.is_empty() && self.documents.is_empty()
    }

    /// Collects a photo's `file_reference` (`FileReferenceAccumulator::push(MTPPhoto)`).
    pub fn push_photo(&mut self, photo: &tl_enums::Photo) {
        if let tl_enums::Photo::Photo(p) = photo {
            self.photos.insert(p.id as u64, p.file_reference.clone());
        }
    }

    /// Collects a document's `file_reference` (`FileReferenceAccumulator::push(MTPDocument)`).
    pub fn push_document(&mut self, doc: &tl_enums::Document) {
        if let tl_enums::Document::Document(d) = doc {
            self.documents.insert(d.id as u64, d.file_reference.clone());
        }
    }

    /// Collects references from a `WallPaper` (`data_file_origin.cpp:51-56`).
    pub fn push_wallpaper(&mut self, wp: &tl_enums::WallPaper) {
        if let tl_enums::WallPaper::WallPaper(w) = wp {
            self.push_document(&w.document);
        }
    }

    /// Collects references from a `Theme` (`data_file_origin.cpp:57-59`).
    pub fn push_theme(&mut self, theme: &tl_enums::Theme) {
        let tl_enums::Theme::Theme(t) = theme;
        if let Some(doc) = &t.document {
            self.push_document(doc);
        }
    }

    /// Collects references nested inside a `Page` (`data_file_origin.cpp:42-45`).
    pub fn push_page(&mut self, page: &tl_enums::Page) {
        let tl_enums::Page::Page(p) = page;
        for photo in &p.photos {
            self.push_photo(photo);
        }
        for doc in &p.documents {
            self.push_document(doc);
        }
    }

    /// Collects references nested inside a `WebPage` (`data_file_origin.cpp:92-100`).
    pub fn push_webpage(&mut self, wp: &tl_enums::WebPage) {
        if let tl_enums::WebPage::WebPage(w) = wp {
            if let Some(doc) = &w.document {
                self.push_document(doc);
            }
            if let Some(photo) = &w.photo {
                self.push_photo(photo);
            }
            if let Some(page) = &w.cached_page {
                self.push_page(page);
            }
            if let Some(attrs) = &w.attributes {
                for attr in attrs {
                    match attr {
                        tl_enums::WebPageAttribute::Story(s) => {
                            if let Some(story) = &s.story {
                                self.push_story_item(story);
                            }
                        }
                        tl_enums::WebPageAttribute::Theme(t) => {
                            if let Some(docs) = &t.documents {
                                for doc in docs {
                                    self.push_document(doc);
                                }
                            }
                        }
                        tl_enums::WebPageAttribute::StickerSet(ss) => {
                            for doc in &ss.stickers {
                                self.push_document(doc);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Collects references nested inside a `Game` (`data_file_origin.cpp:101-106`).
    pub fn push_game(&mut self, game: &tl_enums::Game) {
        let tl_enums::Game::Game(g) = game;
        self.push_photo(&g.photo);
        if let Some(doc) = &g.document {
            self.push_document(doc);
        }
    }

    /// Collects references nested inside a `MessageMedia` (`data_file_origin.cpp:113-139`).
    pub fn push_message_media(&mut self, media: &tl_enums::MessageMedia) {
        match media {
            tl_enums::MessageMedia::Photo(p) => {
                if let Some(photo) = &p.photo {
                    self.push_photo(photo);
                }
            }
            tl_enums::MessageMedia::Document(d) => {
                if let Some(doc) = &d.document {
                    self.push_document(doc);
                }
                if let Some(cover) = &d.video_cover {
                    self.push_photo(cover);
                }
                if let Some(alts) = &d.alt_documents {
                    for alt in alts {
                        self.push_document(alt);
                    }
                }
            }
            tl_enums::MessageMedia::WebPage(w) => {
                self.push_webpage(&w.webpage);
            }
            tl_enums::MessageMedia::Game(g) => {
                self.push_game(&g.game);
            }
            tl_enums::MessageMedia::Invoice(inv) => {
                if let Some(boxed) = &inv.extended_media
                    && let tl_enums::MessageExtendedMedia::MessageExtendedMedia(ext) =
                        boxed.as_ref()
                {
                    self.push_message_media(&ext.media);
                }
            }
            tl_enums::MessageMedia::PaidMedia(pm) => {
                for item in &pm.extended_media {
                    if let tl_enums::MessageExtendedMedia::MessageExtendedMedia(ext) = item {
                        self.push_message_media(&ext.media);
                    }
                }
            }
            _ => {}
        }
    }

    /// Collects references from a single TL `Message` (`data_file_origin.cpp:159-177`).
    pub fn push_message(&mut self, msg: &tl_enums::Message) {
        match msg {
            tl_enums::Message::Message(m) => {
                if let Some(media) = &m.media {
                    self.push_message_media(media);
                }
                if let Some(tl_enums::MessageReplyHeader::MessageReplyHeader(rh)) = &m.reply_to
                    && let Some(rm) = &rh.reply_media
                {
                    self.push_message_media(rm);
                }
            }
            tl_enums::Message::Service(s) => match &s.action {
                tl_enums::MessageAction::ChatEditPhoto(a) => self.push_photo(&a.photo),
                tl_enums::MessageAction::SuggestProfilePhoto(a) => self.push_photo(&a.photo),
                tl_enums::MessageAction::SetChatWallPaper(a) => {
                    self.push_wallpaper(&a.wallpaper);
                }
                _ => {}
            },
            tl_enums::Message::Empty(_) => {}
        }
    }

    /// Collects references from a `StoryItem` (`data_file_origin.cpp:178-184`).
    pub fn push_story_item(&mut self, item: &tl_enums::StoryItem) {
        if let tl_enums::StoryItem::StoryItem(si) = item {
            self.push_message_media(&si.media);
        }
    }

    /// Collects references from `messages.Messages` (`data_file_origin.cpp:185-190`).
    pub fn push_messages(&mut self, resp: &tl_enums::messages::Messages) {
        let list = match resp {
            tl_enums::messages::Messages::Messages(m) => &m.messages,
            tl_enums::messages::Messages::Slice(m) => &m.messages,
            tl_enums::messages::Messages::ChannelMessages(m) => &m.messages,
            tl_enums::messages::Messages::NotModified(_) => return,
        };
        for m in list {
            self.push_message(m);
        }
    }

    /// Collects references from `photos.Photos` (`data_file_origin.cpp:196-200`).
    pub fn push_photos(&mut self, resp: &tl_enums::photos::Photos) {
        let list = match resp {
            tl_enums::photos::Photos::Photos(p) => &p.photos,
            tl_enums::photos::Photos::Slice(p) => &p.photos,
        };
        for photo in list {
            self.push_photo(photo);
        }
    }

    /// Collects references from `users.UserFull` (`data_file_origin.cpp:201-206`).
    pub fn push_user_full(&mut self, resp: &tl_enums::users::UserFull) {
        let tl_enums::users::UserFull::UserFull(uf) = resp;
        let tl_enums::UserFull::UserFull(full) = &uf.full_user;
        if let Some(p) = &full.personal_photo {
            self.push_photo(p);
        }
        if let Some(p) = &full.fallback_photo {
            self.push_photo(p);
        }
        if let Some(p) = &full.profile_photo {
            self.push_photo(p);
        }
    }

    /// Collects references from `messages.ChatFull` (`data_file_origin.cpp:207-218`).
    pub fn push_chat_full(&mut self, resp: &tl_enums::messages::ChatFull) {
        let tl_enums::messages::ChatFull::ChatFull(cf) = resp;
        match &cf.full_chat {
            tl_enums::ChatFull::ChatFull(c) => {
                if let Some(p) = &c.chat_photo {
                    self.push_photo(p);
                }
            }
            tl_enums::ChatFull::ChannelFull(c) => {
                self.push_photo(&c.chat_photo);
            }
            tl_enums::ChatFull::CommunityFull(_) => {}
        }
    }

    /// Collects references from `messages.StickerSet` (`data_file_origin.cpp:242-247`).
    pub fn push_sticker_set(&mut self, resp: &tl_enums::messages::StickerSet) {
        if let tl_enums::messages::StickerSet::StickerSet(ss) = resp {
            for doc in &ss.documents {
                self.push_document(doc);
            }
        }
    }

    /// Collects references from `messages.RecentStickers` (`data_file_origin.cpp:230-235`).
    pub fn push_recent_stickers(&mut self, resp: &tl_enums::messages::RecentStickers) {
        if let tl_enums::messages::RecentStickers::RecentStickers(r) = resp {
            for doc in &r.stickers {
                self.push_document(doc);
            }
        }
    }

    /// Collects references from `messages.FavedStickers` (`data_file_origin.cpp:236-241`).
    pub fn push_faved_stickers(&mut self, resp: &tl_enums::messages::FavedStickers) {
        if let tl_enums::messages::FavedStickers::FavedStickers(f) = resp {
            for doc in &f.stickers {
                self.push_document(doc);
            }
        }
    }

    /// Collects references from `messages.SavedGifs` (`data_file_origin.cpp:248-253`).
    pub fn push_saved_gifs(&mut self, resp: &tl_enums::messages::SavedGifs) {
        if let tl_enums::messages::SavedGifs::SavedGifs(g) = resp {
            for doc in &g.gifs {
                self.push_document(doc);
            }
        }
    }

    /// Collects references from `account.SavedRingtones` (`data_file_origin.cpp:267-272`).
    pub fn push_saved_ringtones(&mut self, resp: &tl_enums::account::SavedRingtones) {
        if let tl_enums::account::SavedRingtones::SavedRingtones(r) = resp {
            for doc in &r.ringtones {
                self.push_document(doc);
            }
        }
    }

    /// Collects references from `help.PremiumPromo` (`data_file_origin.cpp:273-275`).
    pub fn push_premium_promo(&mut self, resp: &tl_enums::help::PremiumPromo) {
        let tl_enums::help::PremiumPromo::PremiumPromo(p) = resp;
        for doc in &p.videos {
            self.push_document(doc);
        }
    }

    /// Collects references from `messages.WebPage` (`data_file_origin.cpp:276-278`).
    pub fn push_messages_webpage(&mut self, resp: &tl_enums::messages::WebPage) {
        let tl_enums::messages::WebPage::WebPage(wp) = resp;
        self.push_webpage(&wp.webpage);
    }

    /// Collects references from `stories.Stories` (`data_file_origin.cpp:279-281`).
    pub fn push_stories(&mut self, resp: &tl_enums::stories::Stories) {
        let tl_enums::stories::Stories::Stories(s) = resp;
        for story in &s.stories {
            self.push_story_item(story);
        }
    }

    /// Collects references from `users.SavedMusic` (`data_file_origin.cpp:282-287`).
    pub fn push_saved_music(&mut self, resp: &tl_enums::users::SavedMusic) {
        if let tl_enums::users::SavedMusic::SavedMusic(sm) = resp {
            for doc in &sm.documents {
                self.push_document(doc);
            }
        }
    }

    /// Looks up a fresh `file_reference` for a TL [`tl_enums::InputFileLocation`].
    #[must_use]
    pub fn lookup_tl_location(&self, loc: &tl_enums::InputFileLocation) -> Option<Vec<u8>> {
        match loc {
            tl_enums::InputFileLocation::InputPhotoFileLocation(p) => {
                self.photos.get(&(p.id as u64)).cloned()
            }
            tl_enums::InputFileLocation::InputDocumentFileLocation(d) => {
                self.documents.get(&(d.id as u64)).cloned()
            }
            tl_enums::InputFileLocation::InputPeerPhotoFileLocation(p) => {
                self.photos.get(&(p.photo_id as u64)).cloned()
            }
            _ => None,
        }
    }
}

/// Applies `new_ref` onto `loc` in place, returning `true` only if `new_ref` is non-empty and
/// actually differs from the previous `file_reference` (`DownloadMtprotoTask::refreshFileReferenceFrom`,
/// `storage/download_manager_mtproto.cpp:457-470`).
pub fn apply_file_reference(loc: &mut tl_enums::InputFileLocation, new_ref: Vec<u8>) -> bool {
    if new_ref.is_empty() {
        return false;
    }
    match loc {
        tl_enums::InputFileLocation::InputPhotoFileLocation(p) => {
            if p.file_reference == new_ref {
                false
            } else {
                p.file_reference = new_ref;
                true
            }
        }
        tl_enums::InputFileLocation::InputDocumentFileLocation(d) => {
            if d.file_reference == new_ref {
                false
            } else {
                d.file_reference = new_ref;
                true
            }
        }
        _ => false,
    }
}

/// Refetches `origin` over `rpc` (mirroring `ApiWrap::refreshFileReference` in
/// `Telegram/SourceFiles/apiwrap.cpp:2886-3100`) and collects all fresh `file_reference` bytes
/// via [`UpdatedFileReferences`].
pub async fn refresh_file_reference<C: RpcClient + ?Sized>(
    rpc: &C,
    session: &Session,
    origin: &FileOrigin,
) -> Result<UpdatedFileReferences, ApiError> {
    let mut updated = UpdatedFileReferences::new();

    match origin {
        FileOrigin::Message { peer_id, msg_id } => {
            let input_msg = vec![tl_enums::InputMessage::ID(tl_types::InputMessageID {
                id: *msg_id,
            })];
            let resp = if let Some(ch_id) = peer_id.to_channel() {
                let channel = input_channel_for(session, ch_id);
                if matches!(channel, tl_enums::InputChannel::Empty) {
                    return Err(ApiError::InvalidState(format!(
                        "channel {ch_id:?} not found in session for file reference refresh"
                    )));
                }
                rpc.invoke(&tl_fns::channels::GetMessages {
                    channel,
                    id: input_msg,
                })
                .await
                .map_err(Into::into)?
            } else {
                rpc.invoke(&tl_fns::messages::GetMessages { id: input_msg })
                    .await
                    .map_err(Into::into)?
            };
            updated.push_messages(&resp);
        }
        FileOrigin::ScheduledMessage { peer_id, msg_id } => {
            let peer = input_peer_for(session, *peer_id);
            let resp = rpc
                .invoke(&tl_fns::messages::GetScheduledMessages {
                    peer,
                    id: vec![*msg_id],
                })
                .await
                .map_err(Into::into)?;
            updated.push_messages(&resp);
        }
        FileOrigin::UserPhoto { user_id, photo_id } => {
            let user_id = input_user_for(session, *user_id);
            if matches!(user_id, tl_enums::InputUser::Empty) {
                return Err(ApiError::InvalidState(
                    "user not found in session for user photo file reference refresh".into(),
                ));
            }
            let resp = rpc
                .invoke(&tl_fns::photos::GetUserPhotos {
                    user_id,
                    offset: -1,
                    max_id: *photo_id as i64,
                    limit: 1,
                })
                .await
                .map_err(Into::into)?;
            updated.push_photos(&resp);
        }
        FileOrigin::FullUser { user_id } => {
            let id = input_user_for(session, *user_id);
            if matches!(id, tl_enums::InputUser::Empty) {
                return Err(ApiError::InvalidState(
                    "user not found in session for full user file reference refresh".into(),
                ));
            }
            let resp = rpc
                .invoke(&tl_fns::users::GetFullUser { id })
                .await
                .map_err(Into::into)?;
            updated.push_user_full(&resp);
        }
        FileOrigin::PeerPhoto { peer_id } => {
            if let Some(ch_id) = peer_id.to_channel() {
                let channel = input_channel_for(session, ch_id);
                if matches!(channel, tl_enums::InputChannel::Empty) {
                    return Err(ApiError::InvalidState(
                        "channel not found in session for peer photo file reference refresh".into(),
                    ));
                }
                let resp = rpc
                    .invoke(&tl_fns::channels::GetFullChannel { channel })
                    .await
                    .map_err(Into::into)?;
                updated.push_chat_full(&resp);
            } else if let Some(chat_id) = peer_id.to_chat() {
                let resp = rpc
                    .invoke(&tl_fns::messages::GetFullChat {
                        chat_id: chat_id.0 as i64,
                    })
                    .await
                    .map_err(Into::into)?;
                updated.push_chat_full(&resp);
            } else {
                return Err(ApiError::InvalidState(
                    "PeerPhoto origin requires a chat or channel peer".into(),
                ));
            }
        }
        FileOrigin::StickerSet {
            set_id,
            access_hash,
        } => {
            let resp = rpc
                .invoke(&tl_fns::messages::GetStickerSet {
                    stickerset: tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                        id: *set_id as i64,
                        access_hash: *access_hash as i64,
                    }),
                    hash: 0,
                })
                .await
                .map_err(Into::into)?;
            updated.push_sticker_set(&resp);
        }
        FileOrigin::RecentStickers { attached } => {
            let resp = rpc
                .invoke(&tl_fns::messages::GetRecentStickers {
                    attached: *attached,
                    hash: 0,
                })
                .await
                .map_err(Into::into)?;
            updated.push_recent_stickers(&resp);
        }
        FileOrigin::FavedStickers => {
            let resp = rpc
                .invoke(&tl_fns::messages::GetFavedStickers { hash: 0 })
                .await
                .map_err(Into::into)?;
            updated.push_faved_stickers(&resp);
        }
        FileOrigin::SavedGifs => {
            let resp = rpc
                .invoke(&tl_fns::messages::GetSavedGifs { hash: 0 })
                .await
                .map_err(Into::into)?;
            updated.push_saved_gifs(&resp);
        }
        FileOrigin::Wallpaper {
            paper_id,
            access_hash,
            owner_id,
            slug,
        } => {
            // Match `ApiWrap::refreshFileReference` (`apiwrap.cpp:3070-3078`):
            // when `owner_id` is set to another user and `slug` is non-empty, use
            // `inputWallPaperSlug`, otherwise `inputWallPaper(id, access_hash)`.
            let use_slug =
                owner_id.is_some_and(|oid| oid != session.self_user_id()) && !slug.is_empty();
            let wallpaper = if use_slug {
                tl_enums::InputWallPaper::Slug(tl_types::InputWallPaperSlug { slug: slug.clone() })
            } else {
                tl_enums::InputWallPaper::InputWallPaper(tl_types::InputWallPaper {
                    id: *paper_id as i64,
                    access_hash: *access_hash as i64,
                })
            };
            let resp = rpc
                .invoke(&tl_fns::account::GetWallPaper { wallpaper })
                .await
                .map_err(Into::into)?;
            updated.push_wallpaper(&resp);
        }
        FileOrigin::Theme {
            theme_id,
            access_hash,
            format,
        } => {
            let resp = rpc
                .invoke(&tl_fns::account::GetTheme {
                    format: format.clone(),
                    theme: tl_enums::InputTheme::InputTheme(tl_types::InputTheme {
                        id: *theme_id as i64,
                        access_hash: *access_hash as i64,
                    }),
                })
                .await
                .map_err(Into::into)?;
            updated.push_theme(&resp);
        }
        FileOrigin::Ringtones => {
            let resp = rpc
                .invoke(&tl_fns::account::GetSavedRingtones { hash: 0 })
                .await
                .map_err(Into::into)?;
            updated.push_saved_ringtones(&resp);
        }
        FileOrigin::PremiumPreviews => {
            let resp = rpc
                .invoke(&tl_fns::help::GetPremiumPromo)
                .await
                .map_err(Into::into)?;
            updated.push_premium_promo(&resp);
        }
        FileOrigin::WebPage { url } => {
            let resp = rpc
                .invoke(&tl_fns::messages::GetWebPage {
                    url: url.clone(),
                    hash: 0,
                })
                .await
                .map_err(Into::into)?;
            updated.push_messages_webpage(&resp);
        }
        FileOrigin::Story { peer_id, story_id } => {
            let peer = input_peer_for(session, *peer_id);
            let resp = rpc
                .invoke(&tl_fns::stories::GetStoriesByID {
                    peer,
                    id: vec![*story_id],
                })
                .await
                .map_err(Into::into)?;
            updated.push_stories(&resp);
        }
        FileOrigin::SavedMusic {
            user_id,
            doc_id,
            access_hash,
            file_reference,
        } => {
            let id = input_user_for(session, *user_id);
            if matches!(id, tl_enums::InputUser::Empty) {
                return Err(ApiError::InvalidState(
                    "user not found in session for saved music file reference refresh".into(),
                ));
            }
            let resp = rpc
                .invoke(&tl_fns::users::GetSavedMusicByID {
                    id,
                    documents: vec![tl_enums::InputDocument::InputDocument(
                        tl_types::InputDocument {
                            id: *doc_id as i64,
                            access_hash: *access_hash as i64,
                            file_reference: file_reference.clone(),
                        },
                    )],
                })
                .await
                .map_err(Into::into)?;
            updated.push_saved_music(&resp);
        }
    }

    Ok(updated)
}

/// Runs `op` with `location`, and if it fails with `400 FILE_REFERENCE_*` (`FILE_REFERENCE_EXPIRED`,
/// `FILE_REFERENCE_INVALID`, etc.), refetches `origin` via [`refresh_file_reference`], updates
/// `location` if a *different* non-empty `file_reference` was returned, and retries `op` **once**
/// (port of `DownloadMtprotoTask::normalPartFailed` + `refreshFileReferenceFrom`,
/// `storage/download_manager_mtproto.cpp:457-476, 929-946`).
pub async fn retry_once_on_file_reference_error<C, T, F, Fut>(
    rpc: &C,
    session: &Session,
    origin: &FileOrigin,
    location: &mut tl_enums::InputFileLocation,
    mut op: F,
) -> Result<T, ApiError>
where
    C: RpcClient + ?Sized,
    F: FnMut(&tl_enums::InputFileLocation) -> Fut,
    Fut: Future<Output = Result<T, InvocationError>>,
{
    match op(location).await {
        Ok(val) => Ok(val),
        Err(InvocationError::Rpc(ref rpc_err)) if rpc_err.is_file_reference_error() => {
            let orig_err = rpc_err.clone();
            let updated = refresh_file_reference(rpc, session, origin).await?;
            let Some(fresh) = updated.lookup_tl_location(location) else {
                return Err(ApiError::Invocation(InvocationError::Rpc(orig_err)));
            };
            if !apply_file_reference(location, fresh) {
                return Err(ApiError::Invocation(InvocationError::Rpc(orig_err)));
            }
            op(location).await.map_err(ApiError::Invocation)
        }
        Err(err) => Err(ApiError::Invocation(err)),
    }
}
