//! Chat-list preview, preview sender and service-message text, as Telegram Desktop builds them.
//!
//! Port origin: `HistoryItem::toPreview` and `HistoryItem::setServiceMessageWhen*`
//! (`history/history_item.cpp`), `Media*::notificationText` / `toPreview`
//! (`data/data_media_types.cpp`) and `Ui::Text::FormatSongNameFor`
//! (`ui/text/format_song_document_name.cpp`).

use tg_app_core::dialogs::backend::{DialogMediaPreview, DialogMediaPreviewKind};
use tg_data::{
    DocumentData, DocumentKind, ForwardInfo, Message, MessageContent, MessageMedia, Peer,
    PeerId as DataPeerId, ServiceAction, Session,
};
use tg_lang::tr;

/// `kPinnedMessageTextLimit` (`history/history_item.cpp`).
const PINNED_MESSAGE_TEXT_LIMIT: usize = 16;

/// Returns a peer's own name, never the "Saved Messages" label (`PeerData::name`).
#[must_use]
pub fn peer_real_name(session: &Session, peer_id: DataPeerId) -> String {
    match session.peer(peer_id) {
        Some(peer) if !peer.name().is_empty() => peer.name().to_owned(),
        _ => format!("Peer {}", peer_id.bare()),
    }
}

fn peer_short_name(session: &Session, peer_id: DataPeerId) -> String {
    match session.peer(peer_id) {
        Some(peer) if !peer.short_name().is_empty() => peer.short_name().to_owned(),
        _ => peer_real_name(session, peer_id),
    }
}

/// `fromSender` in `HistoryItem::toPreview`: "You" for the current account, else the short name.
fn from_sender(session: &Session, sender: DataPeerId) -> String {
    if sender == session.self_peer_id() {
        tr::lng_from_you()
    } else {
        peer_short_name(session, sender)
    }
}

/// `HistoryMessageForwarded::forwardOfForward`.
const fn forward_of_forward(fwd: &ForwardInfo) -> bool {
    fwd.saved_from_id.is_some() || fwd.saved_from_name.is_some()
}

/// `fromForwarded` in `HistoryItem::toPreview`.
fn from_forwarded(session: &Session, msg: &Message) -> Option<String> {
    let fwd = msg.forward_from.as_ref()?;
    let (sender, hidden_name) = if forward_of_forward(fwd) {
        (fwd.saved_from_id, fwd.saved_from_name.as_ref())
    } else {
        (fwd.from_id, fwd.from_name.as_ref())
    };
    match sender {
        Some(pid) => Some(from_sender(session, pid)),
        None => hidden_name.cloned(),
    }
}

fn is_broadcast_peer(session: &Session, peer_id: DataPeerId) -> bool {
    matches!(session.peer(peer_id), Some(Peer::Channel(ch)) if ch.is_broadcast())
}

/// `HistoryItem::isPostHidingAuthor`: a channel post without author profiles.
fn is_post_hiding_author(session: &Session, msg: &Message) -> bool {
    if !msg.is_post() {
        return false;
    }
    !matches!(
        session.peer(msg.peer_id),
        Some(Peer::Channel(ch)) if ch.flags.contains(tg_data::ChannelFlags::SIGNATURE_PROFILES)
    )
}

/// Sender part of a chat-list or search preview (`HistoryItem::toPreview`, `sender`), shown as
/// "{from}:" before the text. `None` for one-to-one chats, channel posts and service messages.
#[must_use]
pub fn preview_sender(session: &Session, msg: &Message) -> Option<String> {
    if matches!(
        msg.content,
        MessageContent::Service(_) | MessageContent::Empty
    ) || is_post_hiding_author(session, msg)
    {
        return None;
    }
    let peer_id = msg.peer_id;
    if !peer_id.is_user() {
        return match msg.from_id {
            Some(from) => Some(from_sender(session, from)),
            None => from_forwarded(session, msg),
        };
    }
    if peer_id == session.self_peer_id()
        || session
            .peer(peer_id)
            .and_then(Peer::as_user)
            .is_some_and(tg_data::User::is_verify_codes)
    {
        return from_forwarded(session, msg);
    }
    None
}

/// `Dialogs::Ui::DialogsPreviewText`: one line, no line breaks.
pub(crate) fn one_line(text: &str) -> String {
    text.trim()
        .split(['\r', '\n'])
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `WithCaptionNotificationText`: the media type alone, the caption alone when the row shows a
/// mini image, or "{media}, {caption}".
fn with_caption(attach_type: &str, caption: &str, has_mini_images: bool) -> String {
    if caption.is_empty() {
        attach_type.to_owned()
    } else if has_mini_images {
        caption.to_owned()
    } else {
        tr::lng_dialogs_text_media(&tr::lng_dialogs_text_media_wrapped(attach_type), caption)
    }
}

/// `Ui::Text::FormatSongNameFor(...).string()`.
fn song_name(doc: &DocumentData) -> String {
    let title = doc.audio_title.as_deref().unwrap_or("");
    let performer = doc.audio_performer.as_deref().unwrap_or("");
    let unknown = "Unknown Track";
    let (title, performer) = if title.is_empty() && performer.is_empty() {
        let name = if doc.file_name.is_empty() {
            unknown
        } else {
            doc.file_name.as_str()
        };
        (name, "")
    } else if performer.is_empty() {
        (title, "")
    } else {
        (if title.is_empty() { unknown } else { title }, performer)
    };
    if title.is_empty() || performer.is_empty() {
        format!("{performer}{title}")
    } else {
        format!("{performer} \u{2013} {title}")
    }
}

pub(crate) const fn is_sticker(kind: DocumentKind) -> bool {
    matches!(
        kind,
        DocumentKind::Sticker | DocumentKind::AnimatedSticker | DocumentKind::VideoSticker
    )
}

/// `TryFilePreview`: documents that get a mini image in the chat list.
fn document_has_mini_image(doc: &DocumentData) -> bool {
    (!doc.thumbs.is_empty() || doc.stripped_thumb.is_some())
        && !is_sticker(doc.kind)
        && doc.kind != DocumentKind::Audio
}

fn sticker_text(doc: &DocumentData) -> String {
    match doc.sticker.as_ref().map(|s| s.alt.as_str()) {
        Some(alt) if !alt.is_empty() => tr::lng_in_dlg_sticker_emoji(alt),
        _ => tr::lng_in_dlg_sticker(),
    }
}

/// `MediaFile::toPreview` type part.
fn document_type_text(doc: &DocumentData) -> String {
    let ttl = doc.ttl_seconds.is_some_and(|t| t > 0);
    match doc.kind {
        DocumentKind::RoundVideo => {
            if ttl {
                tr::lng_in_dlg_video_message_ttl()
            } else {
                tr::lng_in_dlg_video_message()
            }
        }
        DocumentKind::Gif => "GIF".to_owned(),
        DocumentKind::Video => {
            if ttl {
                tr::lng_in_dlg_video_ttl()
            } else {
                tr::lng_in_dlg_video()
            }
        }
        DocumentKind::Voice => {
            if ttl {
                tr::lng_in_dlg_voice_message_ttl()
            } else {
                tr::lng_in_dlg_audio()
            }
        }
        DocumentKind::Audio => {
            if doc.audio_title.is_some()
                || doc.audio_performer.is_some()
                || !doc.file_name.is_empty()
            {
                song_name(doc)
            } else {
                tr::lng_in_dlg_audio_file()
            }
        }
        _ if !doc.file_name.is_empty() => song_name(doc),
        _ => tr::lng_in_dlg_file(),
    }
}

/// Chat-list preview text of `msg` (`HistoryItem::toPreview(...).text`), without the sender.
#[must_use]
pub fn message_preview_text(session: &Session, msg: &Message) -> String {
    match &msg.content {
        MessageContent::Empty => String::new(),
        MessageContent::Service(action) => service_action_text(session, msg, action),
        MessageContent::Regular { text, media } => {
            let caption = one_line(&text.text);
            regular_preview(media, &caption, true)
        }
    }
}

/// Structured chat-list media/service preview metadata for `msg` (`HistoryItem::toPreview`),
/// driving leading preview icons/thumbnails and blue `dialogsTextFgService` tinting in
/// `dialogs/layout.rs`.
#[must_use]
pub fn message_media_preview(msg: &Message) -> Option<DialogMediaPreview> {
    match &msg.content {
        MessageContent::Empty => None,
        MessageContent::Service(action) if matches!(**action, ServiceAction::HistoryClear) => None,
        MessageContent::Service(_) => Some(DialogMediaPreview {
            kind: DialogMediaPreviewKind::Service,
            stripped_thumb: None,
            use_service_color: true,
            has_caption: false,
        }),
        MessageContent::Regular { text, media } => {
            let caption_non_empty = !one_line(&text.text).is_empty();
            match media {
                MessageMedia::Empty | MessageMedia::WebPage(_) | MessageMedia::Unsupported => None,
                MessageMedia::Photo(p) => {
                    let ttl = p.ttl_seconds.is_some_and(|t| t > 0);
                    let has_caption = caption_non_empty && !ttl;
                    Some(DialogMediaPreview {
                        kind: DialogMediaPreviewKind::Photo,
                        stripped_thumb: (!ttl).then(|| p.stripped_thumb.clone()).flatten(),
                        use_service_color: !has_caption,
                        has_caption,
                    })
                }
                MessageMedia::Document(doc) => {
                    if is_sticker(doc.kind) {
                        return Some(DialogMediaPreview {
                            kind: DialogMediaPreviewKind::Sticker,
                            stripped_thumb: None,
                            use_service_color: true,
                            has_caption: false,
                        });
                    }
                    let ttl = doc.ttl_seconds.is_some_and(|t| t > 0);
                    let has_caption = caption_non_empty && !ttl;
                    let kind = match doc.kind {
                        DocumentKind::RoundVideo => DialogMediaPreviewKind::RoundVideo,
                        DocumentKind::Gif => DialogMediaPreviewKind::Gif,
                        DocumentKind::Video => DialogMediaPreviewKind::Video,
                        DocumentKind::Voice => DialogMediaPreviewKind::Voice,
                        DocumentKind::Audio => DialogMediaPreviewKind::Audio,
                        _ => DialogMediaPreviewKind::File,
                    };
                    let stripped_thumb = (!ttl && document_has_mini_image(doc))
                        .then(|| doc.stripped_thumb.clone())
                        .flatten();
                    Some(DialogMediaPreview {
                        kind,
                        stripped_thumb,
                        use_service_color: !has_caption,
                        has_caption,
                    })
                }
                MessageMedia::Contact(_) => Some(DialogMediaPreview {
                    kind: DialogMediaPreviewKind::Contact,
                    stripped_thumb: None,
                    use_service_color: true,
                    has_caption: false,
                }),
                MessageMedia::Location(_) | MessageMedia::LiveLocation(_) => {
                    Some(DialogMediaPreview {
                        kind: DialogMediaPreviewKind::Location,
                        stripped_thumb: None,
                        use_service_color: true,
                        has_caption: false,
                    })
                }
                MessageMedia::Venue(v) => {
                    let has_title = !v.title.trim().is_empty();
                    Some(DialogMediaPreview {
                        kind: DialogMediaPreviewKind::Location,
                        stripped_thumb: None,
                        use_service_color: !has_title,
                        has_caption: has_title,
                    })
                }
                MessageMedia::Poll(_) => Some(DialogMediaPreview {
                    kind: DialogMediaPreviewKind::Poll,
                    stripped_thumb: None,
                    // `WithCaptionNotificationText(type, {})` colorizes the whole type part.
                    use_service_color: true,
                    has_caption: false,
                }),
                MessageMedia::TodoList(_) => Some(DialogMediaPreview {
                    kind: DialogMediaPreviewKind::TodoList,
                    stripped_thumb: None,
                    // `WithCaptionNotificationText(type, {})` colorizes the whole type part.
                    use_service_color: true,
                    has_caption: false,
                }),
                MessageMedia::Dice(_) => Some(DialogMediaPreview {
                    kind: DialogMediaPreviewKind::Dice,
                    stripped_thumb: None,
                    use_service_color: false,
                    has_caption: false,
                }),
                MessageMedia::Game(_) => Some(DialogMediaPreview {
                    kind: DialogMediaPreviewKind::Game,
                    stripped_thumb: None,
                    use_service_color: false,
                    has_caption: false,
                }),
                MessageMedia::Invoice(_) => Some(DialogMediaPreview {
                    kind: DialogMediaPreviewKind::Invoice,
                    stripped_thumb: None,
                    use_service_color: !caption_non_empty,
                    has_caption: caption_non_empty,
                }),
                MessageMedia::Story(_) => Some(DialogMediaPreview {
                    kind: DialogMediaPreviewKind::Story,
                    stripped_thumb: None,
                    use_service_color: true,
                    has_caption: false,
                }),
                _ => None,
            }
        }
    }
}

/// `Media*::toPreview(...).text`. `mini_images` is `ToPreviewOptions::generateImages`: the
/// chat list draws mini thumbnails and then shows a caption alone; a reply header does not
/// (`HistoryItem::inReplyText`) and shows "{media}, {caption}".
pub(crate) fn regular_preview(media: &MessageMedia, caption: &str, mini_images: bool) -> String {
    match media {
        MessageMedia::Empty | MessageMedia::WebPage(_) | MessageMedia::Unsupported => {
            caption.to_owned()
        }
        MessageMedia::Photo(p) => {
            let ttl = p.ttl_seconds.is_some_and(|t| t > 0);
            let kind = if ttl {
                tr::lng_in_dlg_photo_ttl()
            } else {
                tr::lng_in_dlg_photo()
            };
            let caption = if ttl { "" } else { caption };
            with_caption(&kind, caption, mini_images)
        }
        MessageMedia::Document(doc) => {
            if is_sticker(doc.kind) {
                return sticker_text(doc);
            }
            let ttl = doc.ttl_seconds.is_some_and(|t| t > 0);
            let caption = if ttl { "" } else { caption };
            with_caption(
                &document_type_text(doc),
                caption,
                mini_images && document_has_mini_image(doc),
            )
        }
        MessageMedia::Contact(_) => tr::lng_in_dlg_contact(),
        MessageMedia::Location(_) => with_caption(&tr::lng_maps_point(), "", false),
        MessageMedia::LiveLocation(_) => with_caption(&tr::lng_live_location(), "", false),
        MessageMedia::Venue(v) => with_caption(&tr::lng_maps_point(), &v.title, false),
        MessageMedia::Poll(p) => format!("\u{1F4CA} {}", one_line(&p.question.text)),
        MessageMedia::TodoList(t) => format!("\u{2611}\u{FE0F} {}", one_line(&t.title.text)),
        MessageMedia::Dice(d) => d.emoticon.clone(),
        MessageMedia::Game(g) => format!("\u{1F3AE} {}", g.title),
        MessageMedia::Invoice(i) => {
            if caption.is_empty() {
                i.title.clone()
            } else {
                caption.to_owned()
            }
        }
        MessageMedia::Story(_) => tr::lng_in_dlg_story(),
        _ => caption.to_owned(),
    }
}

fn first_user_name(session: &Session, user: tg_data::UserId) -> String {
    peer_real_name(session, DataPeerId::from_user(user))
}

/// `lng_action_add_users_*` list composition (`prepareChatAddUserText`).
fn join_user_names(names: &[String]) -> String {
    let mut iter = names.iter();
    let Some(first) = iter.next() else {
        return String::new();
    };
    let mut acc = first.clone();
    let count = names.len();
    for (i, name) in iter.enumerate() {
        acc = if i + 2 == count {
            tr::lng_action_add_users_and_last(&acc, name)
        } else {
            tr::lng_action_add_users_and_one(&acc, name)
        };
    }
    acc
}

/// `HistoryItem::preparePinnedText` (pinned target looked up through the reply header).
fn pinned_text(session: &Session, msg: &Message, from: &str) -> String {
    let target = msg.reply_to.as_ref().and_then(|r| {
        let id = r.reply_to_msg_id?;
        let peer = r.reply_to_peer_id.unwrap_or(msg.peer_id);
        Some((peer, id))
    });
    let Some((peer, id)) = target else {
        return tr::lng_action_pinned_deleted(from);
    };
    let Some(pinned) = session.message(tg_data::FullMsgId::new(peer, id)) else {
        return tr::lng_action_pinned_media(from, &tr::lng_contacts_loading());
    };
    let media_text = match &pinned.content {
        MessageContent::Regular { media, .. } => pinned_media_text(media),
        _ => String::new(),
    };
    if !media_text.is_empty() {
        return tr::lng_action_pinned_media(from, &media_text);
    }
    let original = match &pinned.content {
        MessageContent::Regular { text, .. } => text.text.clone(),
        _ => String::new(),
    };
    let chars: Vec<char> = original.chars().collect();
    let text = if chars.len() > PINNED_MESSAGE_TEXT_LIMIT + 5 {
        let mut cut: String = chars[..PINNED_MESSAGE_TEXT_LIMIT].iter().collect();
        cut.push('\u{2026}');
        cut
    } else {
        original
    };
    tr::lng_action_pinned_message(from, &text)
}

/// `Media*::pinnedTextSubstring`.
fn pinned_media_text(media: &MessageMedia) -> String {
    match media {
        MessageMedia::Photo(_) => tr::lng_action_pinned_media_photo(),
        MessageMedia::Document(doc) => match doc.kind {
            DocumentKind::Sticker | DocumentKind::AnimatedSticker | DocumentKind::VideoSticker => {
                match doc.sticker.as_ref().map(|s| s.alt.as_str()) {
                    Some(alt) if !alt.is_empty() => tr::lng_action_pinned_media_emoji_sticker(alt),
                    _ => tr::lng_action_pinned_media_sticker(),
                }
            }
            DocumentKind::RoundVideo => tr::lng_action_pinned_media_video_message(),
            DocumentKind::Gif => tr::lng_action_pinned_media_gif(),
            DocumentKind::Video => tr::lng_action_pinned_media_video(),
            DocumentKind::Voice => tr::lng_action_pinned_media_voice(),
            DocumentKind::Audio => tr::lng_action_pinned_media_audio(),
            _ => tr::lng_action_pinned_media_file(),
        },
        MessageMedia::Contact(_) => tr::lng_action_pinned_media_contact(),
        MessageMedia::Location(_) | MessageMedia::LiveLocation(_) | MessageMedia::Venue(_) => {
            tr::lng_action_pinned_media_location()
        }
        MessageMedia::Game(g) => tr::lng_action_pinned_media_game(&g.title),
        MessageMedia::Story(_) => tr::lng_action_pinned_media_story(),
        MessageMedia::Poll(p) => format!("\u{AB}{}\u{BB}", p.question.text),
        MessageMedia::Dice(d) => format!("\u{AB}{}\u{BB}", d.emoticon),
        _ => String::new(),
    }
}

/// tdesktop's service-message text for the actions it renders the same way in history and in
/// the chat list (`HistoryItem::setServiceMessageWhen*` / `prepare*` lambdas). `None` for actions
/// whose text needs data this port does not keep (those keep their existing history rendering).
#[must_use]
pub fn known_service_text(
    session: &Session,
    msg: &Message,
    action: &ServiceAction,
) -> Option<String> {
    let from_id = msg.from_id.unwrap_or(msg.peer_id);
    let from = peer_real_name(session, from_id);
    let is_post = msg.is_post() || is_broadcast_peer(session, msg.peer_id);
    let text = match action {
        ServiceAction::ChatCreate { title, .. } => tr::lng_action_created_chat(&from, title),
        ServiceAction::ChannelCreate { title } => {
            if is_post {
                tr::lng_action_created_channel()
            } else {
                tr::lng_action_created_chat(&from, title)
            }
        }
        ServiceAction::ChatEditTitle { title } => {
            if is_post {
                tr::lng_action_changed_title_channel(title)
            } else {
                tr::lng_action_changed_title(&from, title)
            }
        }
        ServiceAction::ChatEditPhoto { .. } => {
            if is_post {
                tr::lng_action_changed_photo_channel()
            } else {
                tr::lng_action_changed_photo(&from)
            }
        }
        ServiceAction::ChatDeletePhoto => {
            if is_post {
                tr::lng_action_removed_photo_channel()
            } else {
                tr::lng_action_removed_photo(&from)
            }
        }
        ServiceAction::ChatAddUser { users } => match users.as_slice() {
            [single] if DataPeerId::from_user(*single) == from_id => {
                tr::lng_action_user_joined(&from)
            }
            [single] => tr::lng_action_add_user(&from, &first_user_name(session, *single)),
            many => {
                let names: Vec<String> =
                    many.iter().map(|u| first_user_name(session, *u)).collect();
                tr::lng_action_add_users_many(&from, &join_user_names(&names))
            }
        },
        ServiceAction::ChatDeleteUser { user_id } => {
            if DataPeerId::from_user(*user_id) == from_id {
                tr::lng_action_user_left(&from)
            } else {
                tr::lng_action_kick_user(&from, &first_user_name(session, *user_id))
            }
        }
        ServiceAction::ChatJoinedByLink { .. } => tr::lng_action_user_joined_by_link(&from),
        ServiceAction::ChatJoinedByRequest => tr::lng_action_user_joined_by_request(&from),
        ServiceAction::PinMessage => pinned_text(session, msg, &from),
        ServiceAction::ContactSignUp => tr::lng_action_user_registered(&from),
        ServiceAction::CustomAction { message } => message.clone(),
        ServiceAction::ScreenshotTaken => {
            if msg.is_outgoing() {
                tr::lng_action_you_took_screenshot()
            } else {
                tr::lng_action_took_screenshot(&from)
            }
        }
        ServiceAction::BotAllowed {
            attach_menu,
            from_request,
            domain,
            app_id,
        } => {
            if *attach_menu {
                tr::lng_action_attach_menu_bot_allowed()
            } else if *from_request {
                tr::lng_action_webapp_bot_allowed()
            } else if app_id.is_some() {
                tr::lng_action_bot_allowed_from_app("App")
            } else {
                tr::lng_action_bot_allowed_from_domain(domain.as_deref().unwrap_or(""))
            }
        }
        ServiceAction::PhoneCall {
            video,
            reason,
            duration,
            ..
        } => call_text(msg, *video, reason.as_ref(), *duration),
        // `PrepareEmptyText<MTPDmessageActionHistoryClear>` (`history_item.cpp`).
        ServiceAction::HistoryClear => String::new(),
        _ => return None,
    };
    Some(text)
}

/// `MediaCall::notificationText` (`MediaCall::Text` + `lng_call_type_and_duration`).
fn call_text(
    msg: &Message,
    video: bool,
    reason: Option<&tg_data::PhoneCallDiscardReason>,
    duration: Option<i32>,
) -> String {
    use tg_data::PhoneCallDiscardReason as Reason;
    let missed = matches!(reason, Some(Reason::Missed));
    let busy = matches!(reason, Some(Reason::Busy));
    let kind = if msg.is_outgoing() {
        match (missed, video) {
            (true, true) => tr::lng_call_video_cancelled(),
            (true, false) => tr::lng_call_cancelled(),
            (false, true) => tr::lng_call_video_outgoing(),
            (false, false) => tr::lng_call_outgoing(),
        }
    } else if missed {
        if video {
            tr::lng_call_video_missed()
        } else {
            tr::lng_call_missed()
        }
    } else if busy {
        if video {
            tr::lng_call_video_declined()
        } else {
            tr::lng_call_declined()
        }
    } else if video {
        tr::lng_call_video_incoming()
    } else {
        tr::lng_call_incoming()
    };
    match duration {
        Some(secs) if secs > 0 => {
            tr::lng_call_type_and_duration(&kind, &format_duration_words(i64::from(secs)))
        }
        _ => kind,
    }
}

/// `Ui::FormatDurationWords`.
fn format_duration_words(duration: i64) -> String {
    if duration > 59 {
        let minutes = tr::lng_duration_minsec_minutes(duration / 60);
        let seconds = tr::lng_duration_minsec_seconds(duration % 60);
        tr::lng_duration_minutes_seconds(&minutes, &seconds)
    } else {
        tr::lng_seconds(duration)
    }
}

/// Service text for the chat list: tdesktop's text where known, otherwise the same text the
/// history pill shows.
#[must_use]
pub fn service_action_text(session: &Session, msg: &Message, action: &ServiceAction) -> String {
    if let Some(text) = known_service_text(session, msg, action) {
        return text;
    }
    let from = peer_real_name(session, msg.from_id.unwrap_or(msg.peer_id));
    crate::convert::service_action_to_ui(action, session).display_text(&from)
}

/// `Message::hasOutLayout` for Saved Messages: the current account's own notes and messages it
/// forwarded from itself are drawn as outgoing (`history_view_message.cpp`).
#[must_use]
pub fn has_saved_messages_out_layout(session: &Session, msg: &Message) -> bool {
    let self_id = session.self_peer_id();
    if msg.peer_id != self_id {
        return false;
    }
    let Some(fwd) = msg.forward_from.as_ref() else {
        return true;
    };
    if forward_of_forward(fwd) {
        fwd.saved_from_id == Some(self_id) || fwd.saved_out
    } else {
        fwd.from_id == Some(self_id)
    }
}
