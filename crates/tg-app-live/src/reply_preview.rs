//! What a reply header shows of the replied-to message: its one-line text and media thumbnail.
//!
//! Port origin: `HistoryItem::inReplyText` (`history/history_item.cpp`), the `toPreview` and
//! `hasReplyPreview` methods of `Media*` (`data/data_media_types.cpp`) and
//! `Data::ReplyPreview` (`data/data_reply_preview.cpp`).

use tg_app_core::history::backend::ReplyThumb;
use tg_data::{
    DocumentData, DocumentKind, Message, MessageContent, MessageMedia, PhotoData, Session,
    WebDocumentData,
};

use crate::preview_text::{
    message_media_preview, one_line, peer_real_name, regular_preview, service_action_text,
};

/// One-line text of a replied-to message (`HistoryItem::inReplyText`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplyPreviewText {
    /// The text: a caption, "Photo", "Photo, caption", a service action...
    pub text: String,
    /// Leading bytes of `text` that tdesktop colours (`Ui::Text::Colorized`): the media type
    /// part, or the whole of a service action.
    pub colored_len: usize,
}

/// Text a reply header shows for `msg`.
///
/// A regular message gives `toPreview({ .hideSender = true, .generateImages = false })`: with
/// no mini images a media message reads "{media}" or "{media}, {caption}" and the media part
/// is coloured. A service message gives its notification text without the author's name,
/// coloured as a whole.
#[must_use]
pub fn reply_preview_text(session: &Session, msg: &Message) -> ReplyPreviewText {
    match &msg.content {
        MessageContent::Empty => ReplyPreviewText::default(),
        MessageContent::Service(action) => {
            let full = service_action_text(session, msg, action);
            let name = peer_real_name(session, msg.from_id.unwrap_or(msg.peer_id));
            let trimmed = full.trim();
            let text = trimmed
                .strip_prefix(name.as_str())
                .map_or(trimmed, str::trim)
                .to_owned();
            ReplyPreviewText {
                colored_len: text.len(),
                text,
            }
        }
        MessageContent::Regular { text, media } => {
            let caption = one_line(&text.text);
            let full = regular_preview(media, &caption, false);
            let colored_len = match message_media_preview(msg) {
                None => 0,
                Some(preview) if preview.use_service_color => full.len(),
                // "{media}, {caption}": `lng_dialogs_text_media_wrapped` ("{media},") is the
                // coloured part.
                Some(_) if !caption.is_empty() && full.len() > caption.len() => full
                    .strip_suffix(caption.as_str())
                    .map_or(0, |media_part| media_part.trim_end().len()),
                Some(_) => 0,
            };
            ReplyPreviewText {
                text: full,
                colored_len,
            }
        }
    }
}

/// The image file behind a reply header's preview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReplyImage<'a> {
    /// `PhotoData::getReplyPreview`: a photo, a video's cover, a story or poll photo.
    Photo(&'a PhotoData),
    /// `DocumentData::getReplyPreview`: the document's thumbnail.
    Document(&'a DocumentData),
    /// An invoice's product photo, a web file (`Data::Session::photoFromWeb`).
    Web(&'a WebDocumentData),
}

/// An owned [`ReplyImage`], kept while its thumbnail is requested.
#[derive(Debug, Clone, PartialEq)]
pub enum OwnedReplyImage {
    /// See [`ReplyImage::Photo`].
    Photo(Box<PhotoData>),
    /// See [`ReplyImage::Document`].
    Document(Box<DocumentData>),
    /// See [`ReplyImage::Web`].
    Web(WebDocumentData),
}

impl ReplyImage<'_> {
    /// Clones the image's data.
    #[must_use]
    pub fn to_owned_image(self) -> OwnedReplyImage {
        match self {
            Self::Photo(photo) => OwnedReplyImage::Photo(Box::new(photo.clone())),
            Self::Document(doc) => OwnedReplyImage::Document(Box::new(doc.clone())),
            Self::Web(web) => OwnedReplyImage::Web(web.clone()),
        }
    }

    fn stripped_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Photo(photo) => photo.stripped_thumb.as_deref(),
            Self::Document(doc) => doc.stripped_thumb.as_deref(),
            Self::Web(_) => None,
        }
        .filter(|bytes| !bytes.is_empty())
    }
}

/// The image of a reply header's preview and how it is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplyPreviewSource<'a> {
    /// The image.
    pub image: ReplyImage<'a>,
    /// Shown blurred (`getReplyPreview(..., spoiler = true)`).
    pub spoiler: bool,
    /// With the animated spoiler particles (`Reply::_spoiler`).
    pub spoiler_animation: bool,
}

impl ReplyPreviewSource<'_> {
    /// Whether the thumbnail file is downloaded: media under a spoiler keeps its blurred inline
    /// thumbnail and loads the file only when it has none
    /// (`ReplyPreview::image`: `inlineThumbnailBytes.isEmpty() || !spoiler`).
    #[must_use]
    pub fn may_load(&self) -> bool {
        !self.spoiler || self.image.stripped_bytes().is_none()
    }

    /// The header's thumbnail description.
    #[must_use]
    pub fn thumb(&self) -> ReplyThumb {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match self.image {
            ReplyImage::Photo(photo) => (0u8, photo.id.0).hash(&mut hasher),
            ReplyImage::Document(doc) => (1u8, doc.id.0).hash(&mut hasher),
            ReplyImage::Web(web) => (2u8, &web.url).hash(&mut hasher),
        }
        self.spoiler.hash(&mut hasher);
        ReplyThumb {
            stripped: self.image.stripped_bytes().map(<[u8]>::to_vec),
            round: matches!(self.image, ReplyImage::Document(doc) if doc.kind == DocumentKind::RoundVideo),
            spoiler: self.spoiler,
            spoiler_animation: self.spoiler_animation,
            key: hasher.finish(),
        }
    }
}

/// What a reply points at, as far as it is known (`HistoryMessageReply`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplyTargets<'a> {
    /// The replied-to message (`resolvedMessage`).
    pub message: Option<&'a Message>,
    /// The media of the replied-to story (`resolvedStory`).
    pub story: Option<&'a MessageMedia>,
}

fn regular_media(msg: &Message) -> Option<&MessageMedia> {
    match &msg.content {
        MessageContent::Regular { media, .. } if !matches!(media, MessageMedia::Empty) => {
            Some(media)
        }
        _ => None,
    }
}

/// `DocumentData::hasThumbnail()`: a thumbnail that can be downloaded.
fn has_thumbnail(doc: &DocumentData) -> bool {
    crate::media::document_thumb_location(doc).is_some()
}

/// `!PhotoData::isNull()`.
const fn photo_exists(photo: &PhotoData) -> bool {
    photo.id.0 != 0
}

/// A web document Telegram can fetch for the client. tdesktop also loads
/// `webDocumentNoProxy` straight from its URL; that is not done here, so such an invoice photo
/// has no preview.
const fn web_exists(web: &WebDocumentData) -> bool {
    web.access_hash.is_some()
}

/// `Media::hasSpoilerForPreview`: under a spoiler, or self-destructing.
fn has_spoiler_for_preview(media: &MessageMedia) -> bool {
    let ttl = |ttl: Option<i32>| ttl.is_some_and(|secs| secs != 0);
    match media {
        MessageMedia::Photo(photo) => photo.spoiler || ttl(photo.ttl_seconds),
        MessageMedia::Document(doc) => doc.spoiler || ttl(doc.ttl_seconds),
        _ => false,
    }
}

/// `Media::hasReplyPreview` (`Story::hasReplyPreview` for a story's media).
fn media_has_preview(media: &MessageMedia) -> bool {
    match media {
        MessageMedia::Photo(photo) => photo_exists(photo),
        MessageMedia::Document(doc) => has_thumbnail(doc),
        MessageMedia::WebPage(page) => match (&page.document, &page.photo) {
            (Some(doc), _) => has_thumbnail(doc) && doc.kind != DocumentKind::WallPaper,
            (None, Some(photo)) => photo_exists(photo),
            (None, None) => false,
        },
        MessageMedia::Game(game) => match (&game.document, &game.photo) {
            (Some(doc), _) => has_thumbnail(doc),
            (None, Some(photo)) => photo_exists(photo),
            (None, None) => false,
        },
        MessageMedia::Invoice(invoice) => invoice.photo.as_ref().is_some_and(web_exists),
        _ => false,
    }
}

/// `Media::replyPreview`. With `own` the media belongs to the message it is asked for, so a
/// video shows its cover (`LookupVideoCover` finds one only for the item's own document or
/// its web page's document).
fn media_image(media: &MessageMedia, own: bool) -> Option<ReplyImage<'_>> {
    fn document(doc: &DocumentData) -> Option<&DocumentData> {
        has_thumbnail(doc).then_some(doc)
    }
    match media {
        MessageMedia::Photo(photo) => photo_exists(photo).then_some(ReplyImage::Photo(photo)),
        MessageMedia::Document(doc) => match doc.video_cover.as_deref().filter(|_| own) {
            Some(cover) => Some(ReplyImage::Photo(cover)),
            None => document(doc).map(ReplyImage::Document),
        },
        MessageMedia::WebPage(page) => match (&page.document, &page.photo) {
            (Some(_), Some(cover)) if own && page.photo_is_video_cover => {
                Some(ReplyImage::Photo(cover))
            }
            (Some(doc), _) => (doc.kind != DocumentKind::WallPaper)
                .then(|| document(doc))
                .flatten()
                .map(ReplyImage::Document),
            (None, Some(photo)) => photo_exists(photo).then_some(ReplyImage::Photo(photo)),
            (None, None) => None,
        },
        MessageMedia::Game(game) => match (&game.document, &game.photo) {
            (Some(doc), _) => document(doc).map(ReplyImage::Document),
            (None, Some(photo)) => photo_exists(photo).then_some(ReplyImage::Photo(photo)),
            (None, None) => None,
        },
        MessageMedia::Invoice(invoice) => invoice
            .photo
            .as_ref()
            .filter(|web| web_exists(web))
            .map(ReplyImage::Web),
        _ => None,
    }
}

/// Preview of the reply header of `holder`: `None` when the header has none
/// (`Reply::_hasPreview` is false), `Some(None)` when the 32x32 place is kept but there is no
/// image for it, else the image.
///
/// Port of the `hasPreview` rule in `Reply::update` and of the `image` chain in
/// `Reply::paint`: the media of the replied-to poll answer (or of the poll), then the media of
/// the replied-to message, then of the story, then the media sent along with a reply to a
/// message from another chat (`externalMedia`).
#[must_use]
pub fn reply_preview_source<'a>(
    holder: &'a Message,
    targets: ReplyTargets<'a>,
) -> Option<Option<ReplyPreviewSource<'a>>> {
    let reply = holder.reply_to.as_ref()?;
    let media = targets.message.and_then(regular_media);
    let external = reply.reply_media.as_deref();
    let poll_media = match media {
        Some(MessageMedia::Poll(poll)) => {
            match reply.poll_option.as_deref().filter(|o| !o.is_empty()) {
                Some(option) => poll.media_of_answer(option),
                None => poll.attached_media.as_deref(),
            }
        }
        _ => None,
    };
    let has_preview = targets.story.is_some_and(media_has_preview)
        || media.is_some_and(media_has_preview)
        || external.is_some_and(media_has_preview)
        || poll_media.is_some_and(|m| m.photo.is_some() || m.document.is_some());
    if !has_preview {
        return None;
    }
    let plain = |image| ReplyPreviewSource {
        image,
        spoiler: false,
        spoiler_animation: false,
    };
    if let Some(poll_media) = poll_media {
        // `getReplyPreview(message)`: the spoiler flag is the poll message's, never set.
        if let Some(photo) = &poll_media.photo {
            return Some(Some(plain(ReplyImage::Photo(photo))));
        }
        if let Some(doc) = &poll_media.document {
            return Some(has_thumbnail(doc).then(|| plain(ReplyImage::Document(doc))));
        }
    }
    if let Some(media) = media {
        let spoiler = has_spoiler_for_preview(media);
        return Some(media_image(media, true).map(|image| ReplyPreviewSource {
            image,
            spoiler,
            spoiler_animation: spoiler && media_has_preview(media),
        }));
    }
    if let Some(story) = targets.story {
        // `Story::replyPreview` asks without a spoiler and without the video cover.
        return Some(media_image(story, false).map(plain));
    }
    // `externalMedia->replyPreview()` asks with the holder as the media's parent, so the
    // spoiler flag is the one of the holder's own media and no particles are drawn.
    let spoiler = regular_media(holder).is_some_and(has_spoiler_for_preview);
    Some(external.and_then(|media| {
        media_image(media, false).map(|image| ReplyPreviewSource {
            image,
            spoiler,
            spoiler_animation: false,
        })
    }))
}

/// [`reply_preview_source`] as the header's thumbnail description.
#[must_use]
pub fn reply_thumb(holder: &Message, targets: ReplyTargets<'_>) -> Option<Option<ReplyThumb>> {
    reply_preview_source(holder, targets).map(|source| source.map(|s| s.thumb()))
}

/// The image whose thumbnail file the reply header of `holder` downloads, `None` when it
/// shows none or keeps the blurred inline thumbnail.
#[must_use]
pub fn reply_image_to_load(holder: &Message, targets: ReplyTargets<'_>) -> Option<OwnedReplyImage> {
    reply_preview_source(holder, targets)
        .flatten()
        .filter(ReplyPreviewSource::may_load)
        .map(|source| source.image.to_owned_image())
}
