//! TL -> export data model conversions.
//!
//! Port of the TL parsing functions in `Telegram/SourceFiles/export/data/export_data_types.cpp`:
//! `ParsePeerId`, `ParseText`, `ParseTextWithEntities`, `ParseMaxImage`, `ParsePhoto`,
//! `ParseDocument`, `ParseMedia`, `ParseServiceAction`, `ParseReactions`, `ParseButtonRows`,
//! `ParseRichMessage`, `ParseMessage`, `ParseMessagesSlice`, `ParseUser`, `ParseChat`,
//! `ParsePeersLists`, `ParsePersonalInfo`, `ParseContactsList`, `AppendTopPeers`,
//! `ParseSessionsList`, `ParseWebSessionsList`, `ParseUserpicsSlice`, `ParseStoriesSlice`,
//! `ParseDialogsInfo`, `ParseLeftChannelsInfo`, `SingleMessageBefore`, `SingleMessageAfter`.

use std::collections::{BTreeMap, HashMap};

use tg_tl::{enums as tl_e, types as tl_t};

use crate::data::{
    Chat, ContactInfo, ContactsList, DialogInfo, DialogType, DialogsInfo, Document, File,
    FileLocation, Game, GeoPoint, GiveawayResults, GiveawayStart, HistoryMessageMarkupButton,
    Image, InlineButtonAction, InlineButtonActionType, InlineButtonPeerType, Invoice,
    MarkupButtonType, Media, MediaContent, Message, MessagesSlice, PaidMedia, Peer, PersonalInfo,
    PhoneCallState, Photo, Poll, PollAnswer, Reaction, ReactionRecent, ReactionType, RichBlock,
    RichBlockKind, RichButtonAlignment, RichButtonPayload, RichButtonStyle, RichCaption,
    RichChannel, RichChannelSource, RichListItem, RichListItemContent, RichListKind, RichMapPoint,
    RichMapPointSource, RichMessage, RichOrderedList, RichQuoteContent, RichRelatedArticle,
    RichTableAlignment, RichTableCell, RichTableRow, RichTableVerticalAlignment, RichTaskState,
    RichText, RichTextType, SecureValueType, ServiceAction, Session, SessionsList, SharedContact,
    StoriesSlice, Story, TextEntityInput, TextPart, TextPartType, TodoList, TodoListItem, TopPeer,
    UnsupportedMedia, User, UserpicsSlice, Venue, WebSession, dialog_type_from_chat,
    dialog_type_from_user, format_money_amount, parse_text_entities, peer_color_index,
    peer_color_index_peer, prepare_photo_file_name, prepare_story_file_name,
};
use crate::settings::{ExportTypes, Settings};
use crate::stand_in::{
    BareId, Birthday, ChannelId, ChatId, CreditsAmount, CreditsType, InputFileLocation, InputPeer,
    InputUser, PeerId, TimeId, UserId,
};

const REPLIES_USER_ID: u64 = 1_271_266_957;
const VERIFY_CODES_USER_ID: u64 = 489_000;

/// Converts a TL `Peer` into a `PeerId` (`ParsePeerId`).
pub fn parse_peer_id(peer: &tl_e::Peer) -> PeerId {
    match peer {
        tl_e::Peer::User(u) => PeerId::from_user(UserId(u.user_id as u64)),
        tl_e::Peer::Chat(c) => PeerId::from_chat(ChatId(c.chat_id as u64)),
        tl_e::Peer::Channel(c) => PeerId::from_channel(ChannelId(c.channel_id as u64)),
    }
}

/// Converts a TL `StarsAmount` into `CreditsAmount` (`CreditsAmountFromTL`).
pub fn parse_credits_amount(amount: &tl_e::StarsAmount) -> CreditsAmount {
    match amount {
        tl_e::StarsAmount::StarsAmount(s) => {
            CreditsAmount::new(s.amount, i64::from(s.nanos), CreditsType::Stars)
        }
        tl_e::StarsAmount::StarsTonAmount(t) => CreditsAmount::new(0, t.amount, CreditsType::Ton),
    }
}

/// Converts a TL `Birthday` into export `Birthday`.
pub fn parse_birthday(birthday: &tl_e::Birthday) -> Birthday {
    match birthday {
        tl_e::Birthday::Birthday(b) => Birthday::new(b.day, b.month, b.year.unwrap_or(0)),
    }
}

/// Converts TL `MessageEntity` slice + text into `Vec<TextPart>` (`ParseText`).
pub fn parse_text_tl(text: &str, entities: &[tl_e::MessageEntity]) -> Vec<TextPart> {
    let converted: Vec<TextEntityInput> = entities.iter().filter_map(convert_entity).collect();
    parse_text_entities(text, &converted)
}

/// Converts TL `TextWithEntities` into `Vec<TextPart>` (`ParseTextWithEntities`).
pub fn parse_text_with_entities(data: &tl_e::TextWithEntities) -> Vec<TextPart> {
    match data {
        tl_e::TextWithEntities::TextWithEntities(t) => parse_text_tl(&t.text, &t.entities),
    }
}

fn convert_entity(entity: &tl_e::MessageEntity) -> Option<TextEntityInput> {
    let (kind, offset, length, additional) = match entity {
        tl_e::MessageEntity::Unknown(e) => {
            (TextPartType::Unknown, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::Mention(e) => {
            (TextPartType::Mention, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::Hashtag(e) => {
            (TextPartType::Hashtag, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::BotCommand(e) => {
            (TextPartType::BotCommand, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::Url(e) => (TextPartType::Url, e.offset, e.length, String::new()),
        tl_e::MessageEntity::Email(e) => (TextPartType::Email, e.offset, e.length, String::new()),
        tl_e::MessageEntity::Bold(e) => (TextPartType::Bold, e.offset, e.length, String::new()),
        tl_e::MessageEntity::Italic(e) => (TextPartType::Italic, e.offset, e.length, String::new()),
        tl_e::MessageEntity::Code(e) => (TextPartType::Code, e.offset, e.length, String::new()),
        tl_e::MessageEntity::Pre(e) => (TextPartType::Pre, e.offset, e.length, e.language.clone()),
        tl_e::MessageEntity::TextUrl(e) => {
            (TextPartType::TextUrl, e.offset, e.length, e.url.clone())
        }
        tl_e::MessageEntity::MentionName(e) => (
            TextPartType::MentionName,
            e.offset,
            e.length,
            e.user_id.to_string(),
        ),
        tl_e::MessageEntity::InputMessageEntityMentionName(_) => return None,
        tl_e::MessageEntity::Phone(e) => (TextPartType::Phone, e.offset, e.length, String::new()),
        tl_e::MessageEntity::Cashtag(e) => {
            (TextPartType::Cashtag, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::Underline(e) => {
            (TextPartType::Underline, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::Strike(e) => (TextPartType::Strike, e.offset, e.length, String::new()),
        tl_e::MessageEntity::BankCard(e) => {
            (TextPartType::BankCard, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::TonAddress(e) => {
            (TextPartType::TonAddress, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::Spoiler(e) => {
            (TextPartType::Spoiler, e.offset, e.length, String::new())
        }
        tl_e::MessageEntity::CustomEmoji(e) => (
            TextPartType::CustomEmoji,
            e.offset,
            e.length,
            (e.document_id as u64).to_string(),
        ),
        tl_e::MessageEntity::Blockquote(e) => (
            TextPartType::Blockquote,
            e.offset,
            e.length,
            if e.collapsed {
                "1".to_string()
            } else {
                String::new()
            },
        ),
        tl_e::MessageEntity::FormattedDate(_)
        | tl_e::MessageEntity::DiffInsert(_)
        | tl_e::MessageEntity::DiffReplace(_)
        | tl_e::MessageEntity::DiffDelete(_) => return None,
    };
    Some(TextEntityInput {
        kind,
        offset,
        length,
        additional,
    })
}

/// Picks the largest downloadable photo size from `sizes` (`ParseMaxImage`).
pub fn parse_max_image(
    sizes: &[tl_e::PhotoSize],
    id: i64,
    access_hash: i64,
    file_reference: &[u8],
    dc_id: i32,
) -> Image {
    let mut best = Image::default();
    let mut best_area: i64 = 0;

    for size in sizes {
        let candidate = match size {
            tl_e::PhotoSize::Empty(_)
            | tl_e::PhotoSize::PhotoStrippedSize(_)
            | tl_e::PhotoSize::PhotoPathSize(_) => None,
            tl_e::PhotoSize::PhotoSize(s) => {
                Some((s.w, s.h, i64::from(s.size), Vec::new(), s.r#type.clone()))
            }
            tl_e::PhotoSize::PhotoCachedSize(s) => Some((
                s.w,
                s.h,
                s.bytes.len() as i64,
                s.bytes.clone(),
                s.r#type.clone(),
            )),
            tl_e::PhotoSize::Progressive(s) => {
                let total = s.sizes.last().copied().unwrap_or(0);
                Some((s.w, s.h, i64::from(total), Vec::new(), s.r#type.clone()))
            }
        };
        if let Some((w, h, byte_size, content, thumb_size)) = candidate {
            let area = i64::from(w) * i64::from(h);
            if area >= best_area {
                best_area = area;
                best = Image {
                    width: w,
                    height: h,
                    file: File {
                        location: FileLocation {
                            dc_id,
                            data: Some(InputFileLocation::InputPhotoFileLocation(
                                tl_t::InputPhotoFileLocation {
                                    id,
                                    access_hash,
                                    file_reference: file_reference.to_vec(),
                                    thumb_size,
                                },
                            )),
                        },
                        size: byte_size,
                        content,
                        ..Default::default()
                    },
                };
            }
        }
    }
    best
}

/// Parses a TL `Photo` into export `Photo` (`ParsePhoto`).
pub fn parse_photo(photo: &tl_e::Photo, spoilered: bool) -> Photo {
    match photo {
        tl_e::Photo::Empty(_) => Photo::default(),
        tl_e::Photo::Photo(p) => Photo {
            id: p.id as u64,
            date: p.date,
            spoilered,
            image: parse_max_image(&p.sizes, p.id, p.access_hash, &p.file_reference, p.dc_id),
        },
    }
}

/// Picks the best document thumbnail (`ParseDocumentThumb`).
pub fn parse_document_thumb(
    thumbs: &[tl_e::PhotoSize],
    id: i64,
    access_hash: i64,
    file_reference: &[u8],
    dc_id: i32,
) -> Image {
    let mut best = Image::default();
    let mut best_area: i64 = 0;

    for thumb in thumbs {
        let candidate = match thumb {
            tl_e::PhotoSize::Empty(_)
            | tl_e::PhotoSize::PhotoStrippedSize(_)
            | tl_e::PhotoSize::PhotoPathSize(_) => None,
            tl_e::PhotoSize::PhotoSize(s) => {
                Some((s.w, s.h, i64::from(s.size), Vec::new(), s.r#type.clone()))
            }
            tl_e::PhotoSize::PhotoCachedSize(s) => Some((
                s.w,
                s.h,
                s.bytes.len() as i64,
                s.bytes.clone(),
                s.r#type.clone(),
            )),
            tl_e::PhotoSize::Progressive(s) => {
                let total = s.sizes.last().copied().unwrap_or(0);
                Some((s.w, s.h, i64::from(total), Vec::new(), s.r#type.clone()))
            }
        };
        if let Some((w, h, byte_size, content, thumb_size)) = candidate {
            let area = i64::from(w) * i64::from(h);
            if area >= best_area {
                best_area = area;
                best = Image {
                    width: w,
                    height: h,
                    file: File {
                        location: FileLocation {
                            dc_id,
                            data: Some(InputFileLocation::InputDocumentFileLocation(
                                tl_t::InputDocumentFileLocation {
                                    id,
                                    access_hash,
                                    file_reference: file_reference.to_vec(),
                                    thumb_size,
                                },
                            )),
                        },
                        size: byte_size,
                        content,
                        ..Default::default()
                    },
                };
            }
        }
    }
    best
}

/// Parses a TL `Document` into export `Document` (`ParseDocument`).
pub fn parse_document(doc: &tl_e::Document, spoilered: bool) -> Document {
    let tl_e::Document::Document(d) = doc else {
        return Document::default();
    };
    let mut result = Document {
        id: d.id as u64,
        date: d.date,
        file: File {
            location: FileLocation {
                dc_id: d.dc_id,
                data: Some(InputFileLocation::InputDocumentFileLocation(
                    tl_t::InputDocumentFileLocation {
                        id: d.id,
                        access_hash: d.access_hash,
                        file_reference: d.file_reference.clone(),
                        thumb_size: String::new(),
                    },
                )),
            },
            size: d.size,
            ..Default::default()
        },
        thumb: d
            .thumbs
            .as_deref()
            .map(|t| parse_document_thumb(t, d.id, d.access_hash, &d.file_reference, d.dc_id))
            .unwrap_or_default(),
        mime: d.mime_type.clone(),
        spoilered,
        ..Default::default()
    };

    for attr in &d.attributes {
        match attr {
            tl_e::DocumentAttribute::ImageSize(a) => {
                result.width = a.w;
                result.height = a.h;
            }
            tl_e::DocumentAttribute::Animated => {
                result.is_animated = true;
            }
            tl_e::DocumentAttribute::Sticker(a) => {
                result.is_sticker = true;
                result.sticker_emoji = a.alt.clone();
            }
            tl_e::DocumentAttribute::CustomEmoji(a) => {
                result.is_sticker = true;
                result.sticker_emoji = a.alt.clone();
            }
            tl_e::DocumentAttribute::Video(a) => {
                result.width = a.w;
                result.height = a.h;
                result.duration = a.duration as i32;
                if a.round_message {
                    result.is_video_message = true;
                } else {
                    result.is_video_file = true;
                }
            }
            tl_e::DocumentAttribute::Audio(a) => {
                result.duration = a.duration;
                if a.voice {
                    result.is_voice_message = true;
                } else {
                    result.is_audio_file = true;
                    result.song_title = a.title.clone().unwrap_or_default();
                    result.song_performer = a.performer.clone().unwrap_or_default();
                }
            }
            tl_e::DocumentAttribute::Filename(a) => {
                result.name = a.file_name.clone();
            }
            tl_e::DocumentAttribute::HasStickers => {}
        }
    }

    if result.mime == "audio/ogg" && !result.is_voice_message && !result.is_audio_file {
        result.is_voice_message = true;
    }
    if result.is_animated && result.is_video_file {
        result.is_video_file = false;
    }
    result
}

/// Parses a TL `GeoPoint` into export `GeoPoint` (`ParseGeoPoint`).
pub fn parse_geo_point(geo: &tl_e::GeoPoint) -> GeoPoint {
    match geo {
        tl_e::GeoPoint::Empty => GeoPoint::default(),
        tl_e::GeoPoint::GeoPoint(g) => GeoPoint {
            latitude: g.lat,
            longitude: g.long,
            valid: true,
        },
    }
}

/// Parses a TL `MessageMedia` into export `Media` (`ParseMedia`).
///
/// Matches `export_data_types.cpp:2357-2361`: `MessageMediaDice` and `MessageMediaStory`
/// leave `content = MediaContent::None` (stories are exported via `StoriesSlice`).
pub fn parse_media(media: &tl_e::MessageMedia, relative_path: &str, date: TimeId) -> Media {
    let mut result = Media::default();
    match media {
        tl_e::MessageMedia::Empty
        | tl_e::MessageMedia::WebPage(_)
        | tl_e::MessageMedia::Dice(_)
        | tl_e::MessageMedia::Story(_)
        | tl_e::MessageMedia::VideoStream(_)
        | tl_e::MessageMedia::Widget(_) => {}
        tl_e::MessageMedia::Unsupported => {
            result.content = MediaContent::Unsupported(UnsupportedMedia);
        }
        tl_e::MessageMedia::Photo(p) => {
            result.ttl = p.ttl_seconds.unwrap_or(0);
            let photo = p
                .photo
                .as_ref()
                .map(|ph| parse_photo(ph, p.spoiler))
                .unwrap_or_default();
            result.content = MediaContent::Photo(photo);
        }
        tl_e::MessageMedia::Document(d) => {
            result.ttl = d.ttl_seconds.unwrap_or(0);
            let mut doc = d
                .document
                .as_ref()
                .map(|doc| parse_document(doc, d.spoiler))
                .unwrap_or_default();
            if d.round {
                doc.is_video_message = true;
                doc.is_video_file = false;
            }
            if d.voice {
                doc.is_voice_message = true;
                doc.is_audio_file = false;
            }
            result.content = MediaContent::Document(Box::new(doc));
        }
        tl_e::MessageMedia::Contact(c) => {
            let mut vcard = File::default();
            if !c.vcard.is_empty() {
                vcard.content = c.vcard.as_bytes().to_vec();
                vcard.size = vcard.content.len() as i64;
                vcard.suggested_path = format!("{relative_path}contacts/contact_{date}.vcard");
            }
            let user_id = UserId(c.user_id as u64);
            result.content = MediaContent::SharedContact(SharedContact {
                info: ContactInfo {
                    user_id,
                    first_name: c.first_name.clone(),
                    last_name: c.last_name.clone(),
                    phone_number: c.phone_number.clone(),
                    date,
                    color_index: peer_color_index(user_id.0),
                },
                vcard,
            });
        }
        tl_e::MessageMedia::Geo(g) => {
            result.content = MediaContent::GeoPoint(parse_geo_point(&g.geo));
        }
        tl_e::MessageMedia::GeoLive(g) => {
            result.ttl = g.period;
            result.content = MediaContent::GeoPoint(parse_geo_point(&g.geo));
        }
        tl_e::MessageMedia::Venue(v) => {
            result.content = MediaContent::Venue(Venue {
                point: parse_geo_point(&v.geo),
                title: v.title.clone(),
                address: v.address.clone(),
            });
        }
        tl_e::MessageMedia::Game(g) => {
            let tl_e::Game::Game(game) = &g.game;
            result.content = MediaContent::Game(Game {
                id: game.id as u64,
                bot_id: UserId(0),
                short_name: game.short_name.clone(),
                title: game.title.clone(),
                description: game.description.clone(),
            });
        }
        tl_e::MessageMedia::Invoice(inv) => {
            result.content = MediaContent::Invoice(Invoice {
                title: inv.title.clone(),
                description: inv.description.clone(),
                currency: inv.currency.clone(),
                amount: inv.total_amount as u64,
                receipt_msg_id: inv.receipt_msg_id.unwrap_or(0),
            });
        }
        tl_e::MessageMedia::Poll(p) => {
            let tl_e::Poll::Poll(poll) = &p.poll;
            let tl_e::PollResults::PollResults(results) = p.results.as_ref();
            let mut votes_map: HashMap<&[u8], (i32, bool)> = HashMap::new();
            if let Some(ref res_list) = results.results {
                for r in res_list {
                    let tl_e::PollAnswerVoters::PollAnswerVoters(v) = r;
                    votes_map.insert(v.option.as_slice(), (v.voters.unwrap_or(0), v.chosen));
                }
            }
            let answers = poll
                .answers
                .iter()
                .filter_map(|a| {
                    let tl_e::PollAnswer::PollAnswer(ans) = a else {
                        return None;
                    };
                    let (votes, my) = votes_map
                        .get(ans.option.as_slice())
                        .copied()
                        .unwrap_or((0, false));
                    Some(PollAnswer {
                        text: parse_text_with_entities(&ans.text),
                        option: ans.option.clone(),
                        votes,
                        my,
                    })
                })
                .collect();
            result.content = MediaContent::Poll(Poll {
                id: poll.id as u64,
                question: parse_text_with_entities(&poll.question),
                answers,
                total_votes: results.total_voters.unwrap_or(0),
                closed: poll.closed,
            });
        }
        tl_e::MessageMedia::ToDo(t) => {
            let tl_e::TodoList::TodoList(todo) = &t.todo;
            let items = todo
                .list
                .iter()
                .map(|item| {
                    let tl_e::TodoItem::TodoItem(ti) = item;
                    TodoListItem {
                        text: parse_text_with_entities(&ti.title),
                        id: ti.id,
                    }
                })
                .collect();
            result.content = MediaContent::TodoList(TodoList {
                title: parse_text_with_entities(&todo.title),
                items,
                others_can_append: todo.others_can_append,
                others_can_complete: todo.others_can_complete,
            });
        }
        tl_e::MessageMedia::Giveaway(g) => {
            result.content = MediaContent::GiveawayStart(GiveawayStart {
                channels: g.channels.iter().map(|&c| ChannelId(c as u64)).collect(),
                countries: g.countries_iso2.clone().unwrap_or_default(),
                additional_prize: g.prize_description.clone().unwrap_or_default(),
                until_date: g.until_date,
                quantity: g.quantity,
                months: g.months.unwrap_or(0),
                credits: g.stars.unwrap_or(0) as u64,
                all: !g.only_new_subscribers,
            });
        }
        tl_e::MessageMedia::GiveawayResults(g) => {
            result.content = MediaContent::GiveawayResults(GiveawayResults {
                channel: ChannelId(g.channel_id as u64),
                winners: g
                    .winners
                    .iter()
                    .map(|&w| PeerId::from_user(UserId(w as u64)))
                    .collect(),
                additional_prize: g.prize_description.clone().unwrap_or_default(),
                launch_id: g.launch_msg_id,
                additional_peers_count: g.additional_peers_count.unwrap_or(0),
                winners_count: g.winners_count,
                unclaimed_count: g.unclaimed_count,
                months: g.months.unwrap_or(0),
                credits: g.stars.unwrap_or(0) as u64,
                until_date: g.until_date,
                all: !g.only_new_subscribers,
                refunded: g.refunded,
            });
        }
        tl_e::MessageMedia::PaidMedia(pm) => {
            result.content = MediaContent::PaidMedia(PaidMedia {
                stars: pm.stars_amount as u64,
                extended: pm
                    .extended_media
                    .iter()
                    .map(|em| match em {
                        tl_e::MessageExtendedMedia::MessageExtendedMedia(inner) => {
                            Some(parse_media(&inner.media, relative_path, date))
                        }
                        tl_e::MessageExtendedMedia::Preview(_) => None,
                    })
                    .collect(),
            });
        }
    }
    result
}

/// Parses a TL `MessageAction` into export `ServiceAction` (`ParseServiceAction`).
pub fn parse_service_action(action: &tl_e::MessageAction, self_id: PeerId) -> ServiceAction {
    match action {
        tl_e::MessageAction::Empty => ServiceAction::None,
        tl_e::MessageAction::ChatCreate(a) => ServiceAction::ChatCreate {
            title: a.title.clone(),
            user_ids: a.users.iter().map(|&u| UserId(u as u64)).collect(),
        },
        tl_e::MessageAction::ChatEditTitle(a) => ServiceAction::ChatEditTitle {
            title: a.title.clone(),
        },
        tl_e::MessageAction::ChatEditPhoto(a) => ServiceAction::ChatEditPhoto {
            photo: parse_photo(&a.photo, false),
        },
        tl_e::MessageAction::ChatDeletePhoto => ServiceAction::ChatDeletePhoto,
        tl_e::MessageAction::ChatAddUser(a) => ServiceAction::ChatAddUser {
            user_ids: a.users.iter().map(|&u| UserId(u as u64)).collect(),
        },
        tl_e::MessageAction::ChatDeleteUser(a) => ServiceAction::ChatDeleteUser {
            user_id: UserId(a.user_id as u64),
        },
        tl_e::MessageAction::ChatJoinedByLink(a) => ServiceAction::ChatJoinedByLink {
            inviter_id: UserId(a.inviter_id as u64),
        },
        tl_e::MessageAction::ChannelCreate(a) => ServiceAction::ChannelCreate {
            title: a.title.clone(),
        },
        tl_e::MessageAction::ChatMigrateTo(a) => ServiceAction::ChatMigrateTo {
            channel_id: ChannelId(a.channel_id as u64),
        },
        tl_e::MessageAction::ChannelMigrateFrom(a) => ServiceAction::ChannelMigrateFrom {
            title: a.title.clone(),
            chat_id: ChatId(a.chat_id as u64),
        },
        tl_e::MessageAction::PinMessage => ServiceAction::PinMessage,
        tl_e::MessageAction::HistoryClear => ServiceAction::HistoryClear,
        tl_e::MessageAction::GameScore(a) => ServiceAction::GameScore {
            game_id: a.game_id as u64,
            score: a.score,
        },
        tl_e::MessageAction::PaymentSent(a) => ServiceAction::PaymentSent {
            currency: a.currency.clone(),
            amount: a.total_amount as u64,
            recurring_init: a.recurring_init,
            recurring_used: a.recurring_used,
        },
        tl_e::MessageAction::PaymentSentMe(a) => ServiceAction::PaymentSent {
            currency: a.currency.clone(),
            amount: a.total_amount as u64,
            recurring_init: a.recurring_init,
            recurring_used: a.recurring_used,
        },
        tl_e::MessageAction::PhoneCall(a) => {
            let state = match a.reason.as_ref() {
                Some(tl_e::PhoneCallDiscardReason::Missed) => PhoneCallState::Missed,
                Some(tl_e::PhoneCallDiscardReason::Disconnect) => PhoneCallState::Disconnect,
                Some(tl_e::PhoneCallDiscardReason::Hangup) => PhoneCallState::Hangup,
                Some(tl_e::PhoneCallDiscardReason::Busy) => PhoneCallState::Busy,
                Some(tl_e::PhoneCallDiscardReason::MigrateConferenceCall(_)) => {
                    PhoneCallState::MigrateConferenceCall
                }
                None => PhoneCallState::Hangup,
            };
            ServiceAction::PhoneCall {
                conference_id: 0,
                duration: a.duration.unwrap_or(0),
                state,
            }
        }
        tl_e::MessageAction::ConferenceCall(a) => {
            let state = if a.active {
                PhoneCallState::Active
            } else if a.missed {
                PhoneCallState::Missed
            } else if a.duration.is_some() {
                PhoneCallState::Hangup
            } else {
                PhoneCallState::Invitation
            };
            ServiceAction::PhoneCall {
                conference_id: a.call_id as u64,
                duration: a.duration.unwrap_or(0),
                state,
            }
        }
        tl_e::MessageAction::ScreenshotTaken => ServiceAction::ScreenshotTaken,
        tl_e::MessageAction::CustomAction(a) => ServiceAction::CustomAction {
            message: a.message.clone(),
        },
        tl_e::MessageAction::BotAllowed(a) => {
            let (app_id, app) = match a.app.as_ref() {
                Some(tl_e::BotApp::BotApp(ba)) => (ba.id as u64, ba.title.clone()),
                _ => (0, String::new()),
            };
            ServiceAction::BotAllowed {
                app_id,
                app,
                domain: a.domain.clone().unwrap_or_default(),
                attach_menu: a.attach_menu,
                from_request: a.from_request,
            }
        }
        tl_e::MessageAction::SecureValuesSent(a) => ServiceAction::SecureValuesSent {
            types: a.types.iter().map(convert_secure_value_type).collect(),
        },
        tl_e::MessageAction::SecureValuesSentMe(_) => ServiceAction::None,
        tl_e::MessageAction::ContactSignUp => ServiceAction::ContactSignUp,
        tl_e::MessageAction::GeoProximityReached(a) => {
            let from_id = parse_peer_id(&a.from_id);
            let to_id = parse_peer_id(&a.to_id);
            ServiceAction::GeoProximityReached {
                from_id,
                to_id,
                distance: a.distance,
                from_self: from_id == self_id && !self_id.is_zero(),
                to_self: to_id == self_id && !self_id.is_zero(),
            }
        }
        tl_e::MessageAction::GroupCall(a) => ServiceAction::GroupCall {
            duration: a.duration.unwrap_or(0),
        },
        tl_e::MessageAction::InviteToGroupCall(a) => ServiceAction::InviteToGroupCall {
            user_ids: a.users.iter().map(|&u| UserId(u as u64)).collect(),
        },
        tl_e::MessageAction::SetMessagesTTL(a) => {
            ServiceAction::SetMessagesTtl { period: a.period }
        }
        tl_e::MessageAction::GroupCallScheduled(a) => ServiceAction::GroupCallScheduled {
            date: a.schedule_date,
        },
        tl_e::MessageAction::SetChatTheme(a) => {
            let emoji = match &a.theme {
                tl_e::ChatTheme::ChatTheme(t) => t.emoticon.clone(),
                tl_e::ChatTheme::UniqueGift(_) => String::new(),
            };
            ServiceAction::SetChatTheme { emoji }
        }
        tl_e::MessageAction::ChatJoinedByRequest => ServiceAction::ChatJoinedByRequest,
        tl_e::MessageAction::ChatJoinedViaCommunity(a) => ServiceAction::ChatJoinedViaCommunity {
            community_id: ChannelId(a.community_id as u64),
        },
        tl_e::MessageAction::WebViewDataSent(a) => ServiceAction::WebViewDataSent {
            text: a.text.clone(),
        },
        tl_e::MessageAction::WebViewDataSentMe(a) => ServiceAction::WebViewDataSent {
            text: a.text.clone(),
        },
        tl_e::MessageAction::GiftPremium(a) => ServiceAction::GiftPremium {
            cost: format_money_amount(a.amount, &a.currency),
            days: a.days,
        },
        tl_e::MessageAction::TopicCreate(a) => ServiceAction::TopicCreate {
            title: a.title.clone(),
        },
        tl_e::MessageAction::TopicEdit(a) => ServiceAction::TopicEdit {
            title: a.title.clone().unwrap_or_default(),
            icon_emoji_id: a.icon_emoji_id.map(|id| id as u64),
        },
        tl_e::MessageAction::SuggestProfilePhoto(a) => ServiceAction::SuggestProfilePhoto {
            photo: parse_photo(&a.photo, false),
        },
        tl_e::MessageAction::RequestedPeer(a) => ServiceAction::RequestedPeer {
            peers: a.peers.iter().map(parse_peer_id).collect(),
            button_id: a.button_id,
        },
        tl_e::MessageAction::RequestedPeerSentMe(a) => {
            let peers = a
                .peers
                .iter()
                .map(|rp| match rp {
                    tl_e::RequestedPeer::User(u) => PeerId::from_user(UserId(u.user_id as u64)),
                    tl_e::RequestedPeer::Chat(c) => PeerId::from_chat(ChatId(c.chat_id as u64)),
                    tl_e::RequestedPeer::Channel(c) => {
                        PeerId::from_channel(ChannelId(c.channel_id as u64))
                    }
                })
                .collect();
            ServiceAction::RequestedPeer {
                peers,
                button_id: a.button_id,
            }
        }
        tl_e::MessageAction::SetChatWallPaper(a) => ServiceAction::SetChatWallPaper {
            same: a.same,
            both: a.for_both,
        },
        tl_e::MessageAction::GiftCode(a) => ServiceAction::GiftCode {
            code: a.slug.clone(),
            boost_peer_id: a
                .boost_peer
                .as_ref()
                .map(parse_peer_id)
                .unwrap_or(PeerId::ZERO),
            days: a.days,
            via_giveaway: a.via_giveaway,
            unclaimed: a.unclaimed,
        },
        tl_e::MessageAction::GiveawayLaunch(_) => ServiceAction::GiveawayLaunch,
        tl_e::MessageAction::GiveawayResults(a) => ServiceAction::GiveawayResults {
            winners: a.winners_count,
            unclaimed: a.unclaimed_count,
            credits: a.stars,
        },
        tl_e::MessageAction::BoostApply(a) => ServiceAction::BoostApply { boosts: a.boosts },
        tl_e::MessageAction::PaymentRefunded(a) => ServiceAction::PaymentRefunded {
            peer_id: parse_peer_id(&a.peer),
            currency: a.currency.clone(),
            amount: a.total_amount as u64,
            transaction_id: match &a.charge {
                tl_e::PaymentCharge::PaymentCharge(c) => c.id.clone(),
            },
        },
        tl_e::MessageAction::GiftStars(a) => ServiceAction::GiftCredits {
            cost: format_money_amount(a.amount, &a.currency),
            amount: CreditsAmount::from_whole(a.stars, CreditsType::Stars),
        },
        tl_e::MessageAction::GiftTon(a) => ServiceAction::GiftCredits {
            cost: format_money_amount(a.amount, &a.currency),
            amount: CreditsAmount::new(0, a.crypto_amount, CreditsType::Ton),
        },
        tl_e::MessageAction::PrizeStars(a) => ServiceAction::PrizeStars {
            peer_id: parse_peer_id(&a.boost_peer),
            amount: a.stars as u64,
            transaction_id: a.transaction_id.clone(),
            giveaway_msg_id: a.giveaway_msg_id,
            is_unclaimed: a.unclaimed,
        },
        tl_e::MessageAction::StarGift(a) => {
            let (gift_id, stars, limited) = match &a.gift {
                tl_e::StarGift::StarGift(g) => (g.id as u64, g.stars, g.limited),
                tl_e::StarGift::Unique(g) => (g.id as u64, 0, true),
            };
            ServiceAction::StarGift {
                gift_id,
                stars,
                text: a
                    .message
                    .as_ref()
                    .map(parse_text_with_entities)
                    .unwrap_or_default(),
                anonymous: a.name_hidden,
                limited,
                offer_price: CreditsAmount::default(),
                offer_expire_at: 0,
                offer: false,
                offer_accepted: false,
                offer_declined: false,
                offer_expired: false,
            }
        }
        tl_e::MessageAction::StarGiftUnique(a) => {
            let (gift_id, stars) = match &a.gift {
                tl_e::StarGift::StarGift(g) => (g.id as u64, g.stars),
                tl_e::StarGift::Unique(g) => (g.id as u64, a.transfer_stars.unwrap_or(0)),
            };
            ServiceAction::StarGift {
                gift_id,
                stars,
                text: a
                    .message
                    .as_ref()
                    .map(parse_text_with_entities)
                    .unwrap_or_default(),
                anonymous: a.name_hidden,
                limited: true,
                offer_price: CreditsAmount::default(),
                offer_expire_at: 0,
                offer: false,
                offer_accepted: false,
                offer_declined: false,
                offer_expired: false,
            }
        }
        tl_e::MessageAction::StarGiftPurchaseOffer(a) => {
            let gift_id = match &a.gift {
                tl_e::StarGift::StarGift(g) => g.id as u64,
                tl_e::StarGift::Unique(g) => g.id as u64,
            };
            ServiceAction::StarGift {
                gift_id,
                stars: 0,
                text: Vec::new(),
                anonymous: false,
                limited: true,
                offer_price: parse_credits_amount(&a.price),
                offer_expire_at: a.expires_at,
                offer: true,
                offer_accepted: a.accepted,
                offer_declined: a.declined,
                offer_expired: false,
            }
        }
        tl_e::MessageAction::StarGiftPurchaseOfferDeclined(a) => {
            let gift_id = match &a.gift {
                tl_e::StarGift::StarGift(g) => g.id as u64,
                tl_e::StarGift::Unique(g) => g.id as u64,
            };
            ServiceAction::StarGift {
                gift_id,
                stars: 0,
                text: Vec::new(),
                anonymous: false,
                limited: true,
                offer_price: parse_credits_amount(&a.price),
                offer_expire_at: 0,
                offer: true,
                offer_accepted: false,
                offer_declined: true,
                offer_expired: a.expired,
            }
        }
        tl_e::MessageAction::PaidMessagesRefunded(a) => ServiceAction::PaidMessagesRefunded {
            messages: a.count,
            stars: a.stars,
        },
        tl_e::MessageAction::PaidMessagesPrice(a) => ServiceAction::PaidMessagesPrice {
            stars: a.stars as i32,
            broadcast_allowed: a.broadcast_messages_allowed,
        },
        tl_e::MessageAction::TodoCompletions(a) => ServiceAction::TodoCompletions {
            completed: a.completed.clone(),
            incompleted: a.incompleted.clone(),
        },
        tl_e::MessageAction::TodoAppendTasks(a) => ServiceAction::TodoAppendTasks {
            items: a
                .list
                .iter()
                .map(|i| {
                    let tl_e::TodoItem::TodoItem(ti) = i;
                    TodoListItem {
                        text: parse_text_with_entities(&ti.title),
                        id: ti.id,
                    }
                })
                .collect(),
        },
        tl_e::MessageAction::PollAppendAnswer(a) => ServiceAction::PollAppendAnswer {
            option: match &a.answer {
                tl_e::PollAnswer::PollAnswer(ans) => match &ans.text {
                    tl_e::TextWithEntities::TextWithEntities(t) => t.text.clone(),
                },
                tl_e::PollAnswer::InputPollAnswer(ans) => match &ans.text {
                    tl_e::TextWithEntities::TextWithEntities(t) => t.text.clone(),
                },
            },
        },
        tl_e::MessageAction::PollDeleteAnswer(a) => ServiceAction::PollDeleteAnswer {
            option: match &a.answer {
                tl_e::PollAnswer::PollAnswer(ans) => match &ans.text {
                    tl_e::TextWithEntities::TextWithEntities(t) => t.text.clone(),
                },
                tl_e::PollAnswer::InputPollAnswer(ans) => match &ans.text {
                    tl_e::TextWithEntities::TextWithEntities(t) => t.text.clone(),
                },
            },
        },
        tl_e::MessageAction::SuggestedPostApproval(a) => ServiceAction::SuggestedPostApproval {
            reject_comment: a.reject_comment.clone().unwrap_or_default(),
            schedule_date: a.schedule_date.unwrap_or(0),
            price: a
                .price
                .as_ref()
                .map(parse_credits_amount)
                .unwrap_or_default(),
            rejected: a.rejected,
            balance_too_low: a.balance_too_low,
        },
        tl_e::MessageAction::SuggestedPostSuccess(a) => ServiceAction::SuggestedPostSuccess {
            price: parse_credits_amount(&a.price),
        },
        tl_e::MessageAction::SuggestedPostRefund(a) => ServiceAction::SuggestedPostRefund {
            payer_initiated: a.payer_initiated,
        },
        tl_e::MessageAction::SuggestBirthday(a) => ServiceAction::SuggestBirthday {
            birthday: parse_birthday(&a.birthday),
        },
        tl_e::MessageAction::NoForwardsToggle(a) => ServiceAction::NoForwardsToggle {
            new_value: a.new_value,
        },
        tl_e::MessageAction::NoForwardsRequest(a) => ServiceAction::NoForwardsRequest {
            expired: a.expired,
            new_value: a.new_value,
        },
        tl_e::MessageAction::NewCreatorPending(a) => ServiceAction::NewCreatorPending {
            new_creator_id: UserId(a.new_creator_id as u64),
        },
        tl_e::MessageAction::ChangeCreator(a) => ServiceAction::ChangeCreator {
            new_creator_id: UserId(a.new_creator_id as u64),
        },
        tl_e::MessageAction::ManagedBotCreated(a) => ServiceAction::ManagedBotCreated {
            bot_id: UserId(a.bot_id as u64),
        },
        tl_e::MessageAction::GramTransfer(a) => ServiceAction::GramTransfer {
            amount: a.amount,
            peer_address: a.peer_address.clone(),
            transaction_id: a.transaction_id.clone(),
            comment: a.comment.clone().unwrap_or_default(),
            comment_encrypted: a.comment_encrypted,
        },
        tl_e::MessageAction::WalletTonConnectRequest(a) => ServiceAction::WalletTonConnectRequest {
            session_id: a.session_id as u64,
            expires: a.expires,
            topic: a.topic.clone().unwrap_or_default(),
            trace_id: a.trace_id.clone().unwrap_or_default(),
            accepted: a.accepted,
            declined: a.declined,
        },
        tl_e::MessageAction::NoPaidMessagesTokenUsed => ServiceAction::NoPaidMessagesTokenUsed,
        tl_e::MessageAction::ChangeCommunity(_) => ServiceAction::None,
    }
}

fn convert_secure_value_type(t: &tl_e::SecureValueType) -> SecureValueType {
    match t {
        tl_e::SecureValueType::PersonalDetails => SecureValueType::PersonalDetails,
        tl_e::SecureValueType::Passport => SecureValueType::Passport,
        tl_e::SecureValueType::DriverLicense => SecureValueType::DriverLicense,
        tl_e::SecureValueType::IdentityCard => SecureValueType::IdentityCard,
        tl_e::SecureValueType::InternalPassport => SecureValueType::InternalPassport,
        tl_e::SecureValueType::Address => SecureValueType::Address,
        tl_e::SecureValueType::UtilityBill => SecureValueType::UtilityBill,
        tl_e::SecureValueType::BankStatement => SecureValueType::BankStatement,
        tl_e::SecureValueType::RentalAgreement => SecureValueType::RentalAgreement,
        tl_e::SecureValueType::PassportRegistration => SecureValueType::PassportRegistration,
        tl_e::SecureValueType::TemporaryRegistration => SecureValueType::TemporaryRegistration,
        tl_e::SecureValueType::Phone => SecureValueType::Phone,
        tl_e::SecureValueType::Email => SecureValueType::Email,
    }
}

/// Parses TL `MessageReactions` into `Vec<Reaction>` (`ParseReactions`).
pub fn parse_reactions(reactions: &tl_e::MessageReactions) -> Vec<Reaction> {
    let tl_e::MessageReactions::MessageReactions(r) = reactions;
    let mut list = Vec::with_capacity(r.results.len());
    for item in &r.results {
        let tl_e::ReactionCount::ReactionCount(rc) = item;
        let (kind, emoji, document_id) = match &rc.reaction {
            tl_e::Reaction::Empty => (ReactionType::Empty, String::new(), String::new()),
            tl_e::Reaction::Emoji(e) => (ReactionType::Emoji, e.emoticon.clone(), String::new()),
            tl_e::Reaction::CustomEmoji(c) => (
                ReactionType::CustomEmoji,
                String::new(),
                (c.document_id as u64).to_string(),
            ),
            tl_e::Reaction::Paid => (ReactionType::Paid, String::new(), String::new()),
        };
        let mut recent = Vec::new();
        if let Some(ref recent_list) = r.recent_reactions {
            for rec in recent_list {
                let tl_e::MessagePeerReaction::MessagePeerReaction(pr) = rec;
                if pr.reaction == rc.reaction {
                    recent.push(ReactionRecent {
                        peer_id: parse_peer_id(&pr.peer_id),
                        date: pr.date,
                    });
                }
            }
        }
        list.push(Reaction {
            kind,
            emoji,
            document_id,
            count: rc.count.max(0) as u32,
            recent,
        });
    }
    list
}

/// Parses TL `ReplyMarkup` inline buttons into `Vec<Vec<HistoryMessageMarkupButton>>` (`ParseButtonRows`).
pub fn parse_button_rows(markup: &tl_e::ReplyMarkup) -> Vec<Vec<HistoryMessageMarkupButton>> {
    let tl_e::ReplyMarkup::ReplyInlineMarkup(inline) = markup else {
        return Vec::new();
    };
    let mut rows = Vec::with_capacity(inline.rows.len());
    for row in &inline.rows {
        let tl_e::KeyboardInlineButtonRow::KeyboardInlineButtonRow(r) = row;
        let mut parsed_row = Vec::with_capacity(r.buttons.len());
        for btn in &r.buttons {
            let tl_e::KeyboardInlineButton::KeyboardInlineButton(b) = btn;
            let (kind, data, forward_text) = match &b.r#type {
                tl_e::InlineButtonType::Url(u) => (
                    MarkupButtonType::Url,
                    u.url.as_bytes().to_vec(),
                    String::new(),
                ),
                tl_e::InlineButtonType::UrlAuth(u) => (
                    MarkupButtonType::Auth,
                    u.url.as_bytes().to_vec(),
                    u.fwd_text.clone().unwrap_or_default(),
                ),
                tl_e::InlineButtonType::InputInlineButtonTypeUrlAuth(u) => (
                    MarkupButtonType::Auth,
                    u.url.as_bytes().to_vec(),
                    u.fwd_text.clone().unwrap_or_default(),
                ),
                tl_e::InlineButtonType::WebView(w) => (
                    MarkupButtonType::WebView,
                    w.url.as_bytes().to_vec(),
                    String::new(),
                ),
                tl_e::InlineButtonType::Callback(c) => (
                    if c.requires_password {
                        MarkupButtonType::CallbackWithPassword
                    } else {
                        MarkupButtonType::Callback
                    },
                    c.data.clone(),
                    String::new(),
                ),
                tl_e::InlineButtonType::Game => (MarkupButtonType::Game, Vec::new(), String::new()),
                tl_e::InlineButtonType::Buy => (MarkupButtonType::Buy, Vec::new(), String::new()),
                tl_e::InlineButtonType::SwitchInline(s) => (
                    if s.same_peer {
                        MarkupButtonType::SwitchInlineSame
                    } else {
                        MarkupButtonType::SwitchInline
                    },
                    s.query.as_bytes().to_vec(),
                    String::new(),
                ),
                tl_e::InlineButtonType::UserProfile(u) => (
                    MarkupButtonType::UserProfile,
                    u.user_id.to_string().into_bytes(),
                    String::new(),
                ),
                tl_e::InlineButtonType::InputInlineButtonTypeUserProfile(_) => {
                    (MarkupButtonType::UserProfile, Vec::new(), String::new())
                }
                tl_e::InlineButtonType::Copy(c) => (
                    MarkupButtonType::CopyText,
                    c.copy_text.as_bytes().to_vec(),
                    String::new(),
                ),
                tl_e::InlineButtonType::Disabled => {
                    (MarkupButtonType::Disabled, Vec::new(), String::new())
                }
            };
            parsed_row.push(HistoryMessageMarkupButton {
                kind,
                text: b.text.clone(),
                data,
                forward_text,
                button_id: match &b.r#type {
                    tl_e::InlineButtonType::UrlAuth(u) => i64::from(u.button_id),
                    _ => 0,
                },
            });
        }
        if !parsed_row.is_empty() {
            rows.push(parsed_row);
        }
    }
    rows
}

fn parse_inline_button_action(t: &tl_e::InlineButtonType) -> InlineButtonAction {
    match t {
        tl_e::InlineButtonType::Url(u) => InlineButtonAction {
            kind: InlineButtonActionType::Url,
            url: u.url.clone(),
            ..Default::default()
        },
        tl_e::InlineButtonType::UrlAuth(u) => InlineButtonAction {
            kind: InlineButtonActionType::Auth,
            url: u.url.clone(),
            forward_text: u.fwd_text.clone(),
            button_id: u.button_id,
            ..Default::default()
        },
        tl_e::InlineButtonType::InputInlineButtonTypeUrlAuth(u) => InlineButtonAction {
            kind: InlineButtonActionType::Auth,
            url: u.url.clone(),
            forward_text: u.fwd_text.clone(),
            ..Default::default()
        },
        tl_e::InlineButtonType::WebView(w) => InlineButtonAction {
            kind: InlineButtonActionType::WebView,
            url: w.url.clone(),
            ..Default::default()
        },
        tl_e::InlineButtonType::Callback(c) => InlineButtonAction {
            kind: if c.requires_password {
                InlineButtonActionType::CallbackWithPassword
            } else {
                InlineButtonActionType::Callback
            },
            callback_data: c.data.clone(),
            requires_password: c.requires_password,
            ..Default::default()
        },
        tl_e::InlineButtonType::Game => InlineButtonAction {
            kind: InlineButtonActionType::Game,
            ..Default::default()
        },
        tl_e::InlineButtonType::Buy => InlineButtonAction {
            kind: InlineButtonActionType::Buy,
            ..Default::default()
        },
        tl_e::InlineButtonType::SwitchInline(s) => InlineButtonAction {
            kind: if s.same_peer {
                InlineButtonActionType::SwitchInlineSame
            } else {
                InlineButtonActionType::SwitchInline
            },
            query: s.query.clone(),
            same_peer: s.same_peer,
            peer_types: s.peer_types.as_ref().map(|pts| {
                pts.iter()
                    .map(|pt| match pt {
                        tl_e::InlineQueryPeerType::SameBotPM => InlineButtonPeerType::SameBotPm,
                        tl_e::InlineQueryPeerType::PM => InlineButtonPeerType::Pm,
                        tl_e::InlineQueryPeerType::Chat => InlineButtonPeerType::Chat,
                        tl_e::InlineQueryPeerType::Megagroup => InlineButtonPeerType::Megagroup,
                        tl_e::InlineQueryPeerType::Broadcast => InlineButtonPeerType::Broadcast,
                        tl_e::InlineQueryPeerType::BotPM => InlineButtonPeerType::BotPm,
                    })
                    .collect()
            }),
            ..Default::default()
        },
        tl_e::InlineButtonType::UserProfile(u) => InlineButtonAction {
            kind: InlineButtonActionType::UserProfile,
            user_id: u.user_id as u64,
            ..Default::default()
        },
        tl_e::InlineButtonType::InputInlineButtonTypeUserProfile(_) => InlineButtonAction {
            kind: InlineButtonActionType::UserProfile,
            ..Default::default()
        },
        tl_e::InlineButtonType::Copy(c) => InlineButtonAction {
            kind: InlineButtonActionType::CopyText,
            copy_text: c.copy_text.clone(),
            ..Default::default()
        },
        tl_e::InlineButtonType::Disabled => InlineButtonAction {
            kind: InlineButtonActionType::Disabled,
            ..Default::default()
        },
    }
}

fn parse_rich_button_style(style: Option<&tl_e::RichButtonStyle>) -> Option<RichButtonStyle> {
    match style {
        Some(tl_e::RichButtonStyle::RichButtonStyle(s)) => {
            if s.bg_primary {
                Some(RichButtonStyle::Primary)
            } else if s.bg_danger {
                Some(RichButtonStyle::Danger)
            } else if s.bg_success {
                Some(RichButtonStyle::Success)
            } else if s.link {
                Some(RichButtonStyle::Link)
            } else {
                Some(RichButtonStyle::Default)
            }
        }
        None => None,
    }
}

/// Parses a TL `RichText` tree into export `RichText` (`ParseRichText`).
pub fn parse_rich_text(text: &tl_e::RichText) -> RichText {
    match text {
        tl_e::RichText::TextEmpty => RichText {
            kind: RichTextType::Empty,
            ..Default::default()
        },
        tl_e::RichText::TextPlain(t) => RichText {
            kind: RichTextType::Plain,
            text: t.text.clone(),
            ..Default::default()
        },
        tl_e::RichText::TextBold(t) => RichText {
            kind: RichTextType::Bold,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextItalic(t) => RichText {
            kind: RichTextType::Italic,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextUnderline(t) => RichText {
            kind: RichTextType::Underline,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextStrike(t) => RichText {
            kind: RichTextType::Strike,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextFixed(t) => RichText {
            kind: RichTextType::Fixed,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextUrl(t) => RichText {
            kind: RichTextType::Url,
            children: vec![parse_rich_text(&t.text)],
            data: t.url.clone(),
            id: t.webpage_id as u64,
            ..Default::default()
        },
        tl_e::RichText::TextEmail(t) => RichText {
            kind: RichTextType::Email,
            children: vec![parse_rich_text(&t.text)],
            data: t.email.clone(),
            ..Default::default()
        },
        tl_e::RichText::TextConcat(t) => RichText {
            kind: RichTextType::Concat,
            children: t.texts.iter().map(parse_rich_text).collect(),
            ..Default::default()
        },
        tl_e::RichText::TextSubscript(t) => RichText {
            kind: RichTextType::Subscript,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextSuperscript(t) => RichText {
            kind: RichTextType::Superscript,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextMarked(t) => RichText {
            kind: RichTextType::Marked,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextPhone(t) => RichText {
            kind: RichTextType::Phone,
            children: vec![parse_rich_text(&t.text)],
            data: t.phone.clone(),
            ..Default::default()
        },
        tl_e::RichText::TextImage(t) => RichText {
            kind: RichTextType::InlineImage,
            id: t.document_id as u64,
            width: t.w,
            height: t.h,
            ..Default::default()
        },
        tl_e::RichText::TextAnchor(t) => RichText {
            kind: RichTextType::Anchor,
            children: vec![parse_rich_text(&t.text)],
            data: t.name.clone(),
            ..Default::default()
        },
        tl_e::RichText::TextMath(t) => RichText {
            kind: RichTextType::Math,
            data: t.source.clone(),
            ..Default::default()
        },
        tl_e::RichText::TextCustomEmoji(t) => RichText {
            kind: RichTextType::CustomEmoji,
            text: t.alt.clone(),
            custom_emoji_data: (t.document_id as u64).to_string(),
            id: t.document_id as u64,
            ..Default::default()
        },
        tl_e::RichText::TextSpoiler(t) => RichText {
            kind: RichTextType::Spoiler,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextMention(t) => RichText {
            kind: RichTextType::Mention,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextHashtag(t) => RichText {
            kind: RichTextType::Hashtag,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextBotCommand(t) => RichText {
            kind: RichTextType::BotCommand,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextCashtag(t) => RichText {
            kind: RichTextType::Cashtag,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextAutoUrl(t) => RichText {
            kind: RichTextType::AutoUrl,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextAutoEmail(t) => RichText {
            kind: RichTextType::AutoEmail,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextAutoPhone(t) => RichText {
            kind: RichTextType::AutoPhone,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextBankCard(t) => RichText {
            kind: RichTextType::BankCard,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextTonAddress(t) => RichText {
            kind: RichTextType::TonAddress,
            children: vec![parse_rich_text(&t.text)],
            ..Default::default()
        },
        tl_e::RichText::TextMentionName(t) => RichText {
            kind: RichTextType::MentionName,
            children: vec![parse_rich_text(&t.text)],
            id: t.user_id as u64,
            ..Default::default()
        },
        tl_e::RichText::TextDate(t) => RichText {
            kind: RichTextType::FormattedDate,
            children: vec![parse_rich_text(&t.text)],
            date: t.date,
            relative: t.relative,
            short_time: t.short_time,
            long_time: t.long_time,
            short_date: t.short_date,
            long_date: t.long_date,
            day_of_week: t.day_of_week,
            ..Default::default()
        },
        tl_e::RichText::TextDiff(t) => RichText {
            kind: RichTextType::Diff,
            children: vec![parse_rich_text(&t.text)],
            old_children: vec![parse_rich_text(&t.old_text)],
            unsupported: true,
            ..Default::default()
        },
        tl_e::RichText::TextButton(t) => RichText {
            kind: RichTextType::Button,
            children: vec![parse_rich_text(&t.text)],
            button: Some(Box::new(RichButtonPayload {
                action: parse_inline_button_action(&t.r#type),
                style: parse_rich_button_style(t.style.as_ref()),
            })),
            ..Default::default()
        },
    }
}

fn parse_page_caption(caption: &tl_e::PageCaption) -> RichCaption {
    let tl_e::PageCaption::PageCaption(c) = caption;
    RichCaption {
        text: parse_rich_text(&c.text),
        credit: parse_rich_text(&c.credit),
    }
}

fn parse_rich_channel(chat: &tl_e::Chat) -> RichChannel {
    match chat {
        tl_e::Chat::Empty(c) => RichChannel {
            source: RichChannelSource::ChatEmpty,
            id: c.id as u64,
            ..Default::default()
        },
        tl_e::Chat::Chat(c) => RichChannel {
            source: RichChannelSource::Chat,
            id: c.id as u64,
            title: Some(c.title.clone()),
            ..Default::default()
        },
        tl_e::Chat::Forbidden(c) => RichChannel {
            source: RichChannelSource::ChatForbidden,
            id: c.id as u64,
            title: Some(c.title.clone()),
            ..Default::default()
        },
        tl_e::Chat::Channel(c) => RichChannel {
            source: RichChannelSource::Channel,
            id: c.id as u64,
            title: Some(c.title.clone()),
            username: c.username.clone(),
            access_hash: c.access_hash,
            broadcast: c.broadcast,
            megagroup: c.megagroup,
            monoforum: c.monoforum,
        },
        tl_e::Chat::ChannelForbidden(c) => RichChannel {
            source: RichChannelSource::ChannelForbidden,
            id: c.id as u64,
            title: Some(c.title.clone()),
            username: None,
            access_hash: Some(c.access_hash),
            broadcast: c.broadcast,
            megagroup: c.megagroup,
            monoforum: c.monoforum,
        },
        tl_e::Chat::Community(_) => RichChannel {
            source: RichChannelSource::Community,
            ..Default::default()
        },
        tl_e::Chat::CommunityForbidden(_) => RichChannel {
            source: RichChannelSource::CommunityForbidden,
            ..Default::default()
        },
    }
}

fn parse_rich_block(block: &tl_e::PageBlock) -> RichBlock {
    match block {
        tl_e::PageBlock::Unsupported => RichBlock {
            kind: RichBlockKind::Unsupported,
            unsupported: true,
            ..Default::default()
        },
        tl_e::PageBlock::Title(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 1,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Subtitle(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 2,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::AuthorDate(b) => RichBlock {
            kind: RichBlockKind::AuthorDate,
            text: parse_rich_text(&b.author),
            date: b.published_date,
            ..Default::default()
        },
        tl_e::PageBlock::Header(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 3,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Subheader(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 4,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Kicker(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 5,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Paragraph(b) => RichBlock {
            kind: RichBlockKind::Paragraph,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Preformatted(b) => RichBlock {
            kind: RichBlockKind::Code,
            text: parse_rich_text(&b.text),
            language: b.language.clone(),
            ..Default::default()
        },
        tl_e::PageBlock::Footer(b) => RichBlock {
            kind: RichBlockKind::Footer,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Divider => RichBlock {
            kind: RichBlockKind::Divider,
            ..Default::default()
        },
        tl_e::PageBlock::Anchor(b) => RichBlock {
            kind: RichBlockKind::Anchor,
            name: b.name.clone(),
            ..Default::default()
        },
        tl_e::PageBlock::List(b) => RichBlock {
            kind: RichBlockKind::List,
            list_kind: RichListKind::Bullet,
            list_items: b
                .items
                .iter()
                .map(|it| match it {
                    tl_e::PageListItem::Text(t) => RichListItem {
                        content: RichListItemContent::Text,
                        task_state: if t.checkbox {
                            if t.checked {
                                RichTaskState::Checked
                            } else {
                                RichTaskState::Unchecked
                            }
                        } else {
                            RichTaskState::None
                        },
                        text: Some(parse_rich_text(&t.text)),
                        ..Default::default()
                    },
                    tl_e::PageListItem::Blocks(bl) => RichListItem {
                        content: RichListItemContent::Blocks,
                        task_state: if bl.checkbox {
                            if bl.checked {
                                RichTaskState::Checked
                            } else {
                                RichTaskState::Unchecked
                            }
                        } else {
                            RichTaskState::None
                        },
                        blocks: bl.blocks.iter().map(parse_rich_block).collect(),
                        ..Default::default()
                    },
                })
                .collect(),
            ..Default::default()
        },
        tl_e::PageBlock::OrderedList(b) => RichBlock {
            kind: RichBlockKind::List,
            list_kind: RichListKind::Ordered,
            ordered_list: RichOrderedList {
                list_type: b.r#type.clone(),
                start: b.start,
                reversed: b.reversed,
            },
            list_items: b
                .items
                .iter()
                .map(|it| match it {
                    tl_e::PageListOrderedItem::Text(t) => RichListItem {
                        content: RichListItemContent::Text,
                        task_state: if t.checkbox {
                            if t.checked {
                                RichTaskState::Checked
                            } else {
                                RichTaskState::Unchecked
                            }
                        } else {
                            RichTaskState::None
                        },
                        num: t.num.clone(),
                        value: t.value,
                        item_type: t.r#type.clone(),
                        text: Some(parse_rich_text(&t.text)),
                        ..Default::default()
                    },
                    tl_e::PageListOrderedItem::Blocks(bl) => RichListItem {
                        content: RichListItemContent::Blocks,
                        task_state: if bl.checkbox {
                            if bl.checked {
                                RichTaskState::Checked
                            } else {
                                RichTaskState::Unchecked
                            }
                        } else {
                            RichTaskState::None
                        },
                        num: bl.num.clone(),
                        value: bl.value,
                        item_type: bl.r#type.clone(),
                        blocks: bl.blocks.iter().map(parse_rich_block).collect(),
                        ..Default::default()
                    },
                })
                .collect(),
            ..Default::default()
        },
        tl_e::PageBlock::Blockquote(b) => RichBlock {
            kind: RichBlockKind::Quote,
            quote_content: RichQuoteContent::Text,
            text: parse_rich_text(&b.text),
            quote_caption: parse_rich_text(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::BlockquoteBlocks(b) => RichBlock {
            kind: RichBlockKind::Quote,
            quote_content: RichQuoteContent::Blocks,
            blocks: b.blocks.iter().map(parse_rich_block).collect(),
            quote_caption: parse_rich_text(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::Pullquote(b) => RichBlock {
            kind: RichBlockKind::Quote,
            quote_content: RichQuoteContent::Text,
            pullquote: true,
            text: parse_rich_text(&b.text),
            quote_caption: parse_rich_text(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::Photo(b) => RichBlock {
            kind: RichBlockKind::Photo,
            spoiler: b.spoiler,
            photo_id: b.photo_id as u64,
            caption: parse_page_caption(&b.caption),
            optional_url: b.url.clone(),
            optional_webpage_id: b.webpage_id.map(|w| w as u64),
            ..Default::default()
        },
        tl_e::PageBlock::Video(b) => RichBlock {
            kind: RichBlockKind::Video,
            autoplay: b.autoplay,
            is_loop: b.r#loop,
            spoiler: b.spoiler,
            document_id: b.video_id as u64,
            caption: parse_page_caption(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::Cover(b) => RichBlock {
            kind: RichBlockKind::Cover,
            blocks: vec![parse_rich_block(&b.cover)],
            ..Default::default()
        },
        tl_e::PageBlock::Embed(b) => RichBlock {
            kind: RichBlockKind::Embed,
            full_width: b.full_width,
            allow_scrolling: b.allow_scrolling,
            optional_url: b.url.clone(),
            html: b.html.clone(),
            poster_photo_id: b.poster_photo_id.map(|id| id as u64),
            width: b.w,
            height: b.h,
            caption: parse_page_caption(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::EmbedPost(b) => RichBlock {
            kind: RichBlockKind::EmbedPost,
            url: b.url.clone(),
            webpage_id: b.webpage_id as u64,
            author_photo_id: b.author_photo_id as u64,
            author: b.author.clone(),
            date: b.date,
            blocks: b.blocks.iter().map(parse_rich_block).collect(),
            caption: parse_page_caption(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::Collage(b) => RichBlock {
            kind: RichBlockKind::Collage,
            blocks: b.items.iter().map(parse_rich_block).collect(),
            caption: parse_page_caption(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::Slideshow(b) => RichBlock {
            kind: RichBlockKind::Slideshow,
            blocks: b.items.iter().map(parse_rich_block).collect(),
            caption: parse_page_caption(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::Channel(b) => RichBlock {
            kind: RichBlockKind::Channel,
            channel: parse_rich_channel(&b.channel),
            ..Default::default()
        },
        tl_e::PageBlock::Audio(b) => RichBlock {
            kind: RichBlockKind::Audio,
            document_id: b.audio_id as u64,
            caption: parse_page_caption(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::Document(b) => RichBlock {
            kind: RichBlockKind::File,
            document_id: b.document_id as u64,
            caption: parse_page_caption(&b.caption),
            ..Default::default()
        },
        tl_e::PageBlock::Table(b) => RichBlock {
            kind: RichBlockKind::Table,
            bordered: b.bordered,
            striped: b.striped,
            compact: b.compact,
            text: parse_rich_text(&b.title),
            table_rows: b
                .rows
                .iter()
                .map(|row| {
                    let tl_e::PageTableRow::PageTableRow(r) = row;
                    RichTableRow {
                        cells: r
                            .cells
                            .iter()
                            .map(|cell| {
                                let tl_e::PageTableCell::PageTableCell(c) = cell;
                                RichTableCell {
                                    header: c.header,
                                    alignment: if c.align_center {
                                        RichTableAlignment::Center
                                    } else if c.align_right {
                                        RichTableAlignment::Right
                                    } else {
                                        RichTableAlignment::Left
                                    },
                                    vertical_alignment: if c.valign_middle {
                                        RichTableVerticalAlignment::Middle
                                    } else if c.valign_bottom {
                                        RichTableVerticalAlignment::Bottom
                                    } else {
                                        RichTableVerticalAlignment::Top
                                    },
                                    text: c.text.as_ref().map(parse_rich_text),
                                    colspan: c.colspan,
                                    rowspan: c.rowspan,
                                }
                            })
                            .collect(),
                    }
                })
                .collect(),
            ..Default::default()
        },
        tl_e::PageBlock::Details(b) => RichBlock {
            kind: RichBlockKind::Details,
            open: b.open,
            blocks: b.blocks.iter().map(parse_rich_block).collect(),
            text: parse_rich_text(&b.title),
            ..Default::default()
        },
        tl_e::PageBlock::RelatedArticles(b) => RichBlock {
            kind: RichBlockKind::RelatedArticles,
            text: parse_rich_text(&b.title),
            related_articles: b
                .articles
                .iter()
                .map(|art| {
                    let tl_e::PageRelatedArticle::PageRelatedArticle(a) = art;
                    RichRelatedArticle {
                        url: a.url.clone(),
                        webpage_id: a.webpage_id as u64,
                        title: a.title.clone(),
                        description: a.description.clone(),
                        photo_id: a.photo_id.map(|id| id as u64),
                        author: a.author.clone(),
                        published_date: a.published_date,
                    }
                })
                .collect(),
            ..Default::default()
        },
        tl_e::PageBlock::Map(b) => {
            let point = match &b.geo {
                tl_e::GeoPoint::Empty => RichMapPoint {
                    source: RichMapPointSource::GeoPointEmpty,
                    ..Default::default()
                },
                tl_e::GeoPoint::GeoPoint(g) => RichMapPoint {
                    source: RichMapPointSource::GeoPoint,
                    latitude: g.lat,
                    longitude: g.long,
                    access_hash: Some(g.access_hash),
                    accuracy_radius: g.accuracy_radius,
                },
            };
            RichBlock {
                kind: RichBlockKind::Map,
                map_point: point,
                zoom: b.zoom,
                map_width: b.w,
                map_height: b.h,
                caption: parse_page_caption(&b.caption),
                ..Default::default()
            }
        }
        tl_e::PageBlock::InputPageBlockMap(b) => {
            let point = match &b.geo {
                tl_e::InputGeoPoint::Empty => RichMapPoint {
                    source: RichMapPointSource::InputGeoPointEmpty,
                    ..Default::default()
                },
                tl_e::InputGeoPoint::InputGeoPoint(g) => RichMapPoint {
                    source: RichMapPointSource::InputGeoPoint,
                    latitude: g.lat,
                    longitude: g.long,
                    access_hash: None,
                    accuracy_radius: g.accuracy_radius,
                },
            };
            RichBlock {
                kind: RichBlockKind::InputMap,
                map_point: point,
                zoom: b.zoom,
                map_width: b.w,
                map_height: b.h,
                caption: parse_page_caption(&b.caption),
                ..Default::default()
            }
        }
        tl_e::PageBlock::Heading1(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 1,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Heading2(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 2,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Heading3(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 3,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Heading4(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 4,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Heading5(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 5,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Heading6(b) => RichBlock {
            kind: RichBlockKind::Heading,
            heading_level: 6,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::Math(b) => RichBlock {
            kind: RichBlockKind::Math,
            formula: b.source.clone(),
            ..Default::default()
        },
        tl_e::PageBlock::Thinking(b) => RichBlock {
            kind: RichBlockKind::Thinking,
            text: parse_rich_text(&b.text),
            ..Default::default()
        },
        tl_e::PageBlock::ButtonRow(b) => {
            let button_alignment = if b.align_left {
                RichButtonAlignment::Left
            } else if b.align_center {
                RichButtonAlignment::Center
            } else if b.align_right {
                RichButtonAlignment::Right
            } else {
                RichButtonAlignment::Stretch
            };
            RichBlock {
                kind: RichBlockKind::ButtonRow,
                button_alignment,
                buttons: b
                    .buttons
                    .iter()
                    .map(|btn| {
                        let tl_e::PageButton::PageButton(pb) = btn;
                        RichText {
                            kind: RichTextType::Button,
                            children: vec![parse_rich_text(&pb.text)],
                            button: Some(Box::new(RichButtonPayload {
                                action: parse_inline_button_action(&pb.r#type),
                                style: parse_rich_button_style(pb.style.as_ref()),
                            })),
                            ..Default::default()
                        }
                    })
                    .collect(),
                ..Default::default()
            }
        }
    }
}

/// Parses a TL `RichMessage` into export `RichMessage` (`ParseRichMessage`).
pub fn parse_rich_message(rich: &tl_e::RichMessage) -> RichMessage {
    let tl_e::RichMessage::RichMessage(rm) = rich;
    let mut photos = BTreeMap::new();
    for p in &rm.photos {
        let parsed = parse_photo(p, false);
        if parsed.id != 0 {
            photos.insert(parsed.id, parsed);
        }
    }
    let mut documents = BTreeMap::new();
    for d in &rm.documents {
        let parsed = parse_document(d, false);
        if parsed.id != 0 {
            documents.insert(parsed.id, parsed);
        }
    }
    RichMessage {
        blocks: rm.blocks.iter().map(parse_rich_block).collect(),
        photos,
        documents,
        rtl: rm.rtl,
        part: rm.part,
    }
}

/// Parses a TL `Message` into export `Message` (`ParseMessage`).
pub fn parse_message(msg: &tl_e::Message, self_id: PeerId, relative_path: &str) -> Message {
    let mut result = Message {
        self_id,
        ..Default::default()
    };
    match msg {
        tl_e::Message::Empty(m) => {
            result.id = m.id;
            if let Some(ref p) = m.peer_id {
                result.peer_id = parse_peer_id(p);
            }
        }
        tl_e::Message::Service(m) => {
            result.id = m.id;
            result.out = m.out;
            result.date = m.date;
            result.peer_id = parse_peer_id(&m.peer_id);
            result.from_id = m
                .from_id
                .as_ref()
                .map(parse_peer_id)
                .unwrap_or_else(|| if m.post { result.peer_id } else { PeerId::ZERO });
            if let Some(ref reply) = m.reply_to {
                apply_reply_header(&mut result, reply);
            }
            result.action = parse_service_action(&m.action, self_id);
            if let Some(ref reactions) = m.reactions {
                result.reactions = parse_reactions(reactions);
            }
        }
        tl_e::Message::Message(m) => {
            result.id = m.id;
            result.out = m.out;
            result.date = m.date;
            result.edited = m.edit_date.unwrap_or(0);
            result.peer_id = parse_peer_id(&m.peer_id);
            result.from_id = m
                .from_id
                .as_ref()
                .map(parse_peer_id)
                .unwrap_or(result.peer_id);
            result.signature = m.post_author.clone().unwrap_or_default();
            if let Some(bot_id) = m.via_bot_id {
                result.via_bot_id = UserId(bot_id as u64);
            }
            if let Some(ref reply) = m.reply_to {
                apply_reply_header(&mut result, reply);
            }
            if let Some(ref fwd) = m.fwd_from {
                let tl_e::MessageFwdHeader::MessageFwdHeader(f) = fwd;
                result.forwarded = true;
                result.forwarded_date = f.date;
                if let Some(ref from_id) = f.from_id {
                    result.forwarded_from_id = parse_peer_id(from_id);
                }
                if let Some(ref name) = f.from_name {
                    result.forwarded_from_name = name.clone();
                }
                if let Some(ref saved_peer) = f.saved_from_peer {
                    result.saved_from_chat_id = parse_peer_id(saved_peer);
                }
                result.show_forwarded_as_original = result.peer_id == self_id
                    && (!result.forwarded_from_id.is_zero()
                        || !result.forwarded_from_name.is_empty());
            }
            result.text = parse_text_tl(&m.message, m.entities.as_deref().unwrap_or(&[]));
            if let Some(ref media) = m.media {
                result.media = parse_media(media, relative_path, m.date);
                if let MediaContent::Game(ref mut game) = result.media.content {
                    if !result.via_bot_id.is_zero() {
                        game.bot_id = result.via_bot_id;
                    } else if result.from_id.is_user() {
                        game.bot_id = result.from_id.to_user_or_zero();
                    }
                }
            }
            if let Some(ref markup) = m.reply_markup {
                result.inline_button_rows = parse_button_rows(markup);
            }
            if let Some(ref reactions) = m.reactions {
                result.reactions = parse_reactions(reactions);
            }
            if let Some(ref rich) = m.rich_message {
                result.rich_message = Some(parse_rich_message(rich));
            }
        }
    }
    result
}

fn apply_reply_header(result: &mut Message, reply: &tl_e::MessageReplyHeader) {
    match reply {
        tl_e::MessageReplyHeader::MessageReplyHeader(r) => {
            result.reply_to_msg_id = r.reply_to_msg_id.unwrap_or(0);
            if let Some(ref peer) = r.reply_to_peer_id {
                result.reply_to_peer_id = parse_peer_id(peer);
            }
        }
        tl_e::MessageReplyHeader::MessageReplyStoryHeader(_) => {}
    }
}

/// Parses a TL `User` into export `User` (`ParseUser`).
pub fn parse_user(user: &tl_e::User) -> User {
    match user {
        tl_e::User::Empty(u) => {
            let bare_id = u.id as BareId;
            let color_index = peer_color_index(bare_id);
            User {
                bare_id,
                info: ContactInfo {
                    user_id: UserId(bare_id),
                    color_index,
                    ..Default::default()
                },
                color_index,
                ..Default::default()
            }
        }
        tl_e::User::User(u) => {
            let bare_id = u.id as BareId;
            let color_index = peer_color_index(bare_id);
            let username = u
                .username
                .clone()
                .or_else(|| {
                    u.usernames.as_ref().and_then(|list| {
                        list.iter().find_map(|un| match un {
                            tl_e::Username::Username(item) if item.active => {
                                Some(item.username.clone())
                            }
                            _ => None,
                        })
                    })
                })
                .unwrap_or_default();
            let input = if u.self_ {
                InputUser::Self_
            } else if let Some(access_hash) = u.access_hash {
                InputUser::InputUser(tl_t::InputUser {
                    user_id: u.id,
                    access_hash,
                })
            } else {
                InputUser::Empty
            };
            User {
                bare_id,
                info: ContactInfo {
                    user_id: UserId(bare_id),
                    first_name: u.first_name.clone().unwrap_or_default(),
                    last_name: u.last_name.clone().unwrap_or_default(),
                    phone_number: u.phone.clone().unwrap_or_default(),
                    date: 0,
                    color_index,
                },
                username,
                color_index,
                is_bot: u.bot,
                is_self: u.self_,
                is_replies: bare_id == REPLIES_USER_ID,
                is_verify_codes: bare_id == VERIFY_CODES_USER_ID,
                input,
            }
        }
    }
}

/// Parses a TL `Chat` into export `Chat` (`ParseChat`).
pub fn parse_chat(chat: &tl_e::Chat) -> Chat {
    match chat {
        tl_e::Chat::Empty(c) => {
            let bare_id = c.id as BareId;
            Chat {
                bare_id,
                color_index: peer_color_index(bare_id),
                input: InputPeer::Chat(tl_t::InputPeerChat { chat_id: c.id }),
                ..Default::default()
            }
        }
        tl_e::Chat::Chat(c) => {
            let bare_id = c.id as BareId;
            let migrated_to_channel_id = match c.migrated_to.as_ref() {
                Some(tl_e::InputChannel::InputChannel(ic)) => ChannelId(ic.channel_id as u64),
                _ => ChannelId(0),
            };
            Chat {
                bare_id,
                migrated_to_channel_id,
                title: c.title.clone(),
                color_index: peer_color_index(bare_id),
                input: InputPeer::Chat(tl_t::InputPeerChat { chat_id: c.id }),
                ..Default::default()
            }
        }
        tl_e::Chat::Forbidden(c) => {
            let bare_id = c.id as BareId;
            Chat {
                bare_id,
                title: c.title.clone(),
                color_index: peer_color_index(bare_id),
                input: InputPeer::Chat(tl_t::InputPeerChat { chat_id: c.id }),
                ..Default::default()
            }
        }
        tl_e::Chat::Channel(c) => {
            let bare_id = c.id as BareId;
            let username = c
                .username
                .clone()
                .or_else(|| {
                    c.usernames.as_ref().and_then(|list| {
                        list.iter().find_map(|un| match un {
                            tl_e::Username::Username(item) if item.active => {
                                Some(item.username.clone())
                            }
                            _ => None,
                        })
                    })
                })
                .unwrap_or_default();
            let input = if let Some(access_hash) = c.access_hash {
                InputPeer::Channel(tl_t::InputPeerChannel {
                    channel_id: c.id,
                    access_hash,
                })
            } else {
                InputPeer::Empty
            };
            let has_monoforum_admin_rights = c.creator
                || c.admin_rights.as_ref().is_some_and(|r| {
                    let tl_e::ChatAdminRights::ChatAdminRights(ar) = r;
                    ar.manage_direct_messages
                });
            Chat {
                bare_id,
                title: c.title.clone(),
                username,
                color_index: peer_color_index(bare_id),
                is_monoforum: c.monoforum,
                is_broadcast: c.broadcast,
                is_supergroup: c.megagroup || (!c.broadcast),
                is_monoforum_admin: c.monoforum && has_monoforum_admin_rights,
                has_monoforum_admin_rights,
                monoforum_link_id: c.linked_monoforum_id.map(|id| id as BareId).unwrap_or(0),
                input,
                ..Default::default()
            }
        }
        tl_e::Chat::ChannelForbidden(c) => {
            let bare_id = c.id as BareId;
            Chat {
                bare_id,
                title: c.title.clone(),
                color_index: peer_color_index(bare_id),
                is_monoforum: c.monoforum,
                is_broadcast: c.broadcast,
                is_supergroup: c.megagroup || (!c.broadcast),
                input: InputPeer::Channel(tl_t::InputPeerChannel {
                    channel_id: c.id,
                    access_hash: c.access_hash,
                }),
                ..Default::default()
            }
        }
        tl_e::Chat::Community(_) | tl_e::Chat::CommunityForbidden(_) => Chat::default(),
    }
}

/// Parses TL `users` and `chats` vectors into a `PeerId -> Peer` map (`ParsePeersLists`).
pub fn parse_peers_lists(users: &[tl_e::User], chats: &[tl_e::Chat]) -> BTreeMap<PeerId, Peer> {
    let mut map = BTreeMap::new();
    for u in users {
        let parsed = parse_user(u);
        let id = parsed.id();
        if !id.is_zero() {
            map.insert(id, Peer::User(parsed));
        }
    }
    let mut parsed_chats: Vec<Chat> = chats.iter().map(parse_chat).collect();
    let broadcast_info: HashMap<BareId, (bool, InputPeer, bool)> = parsed_chats
        .iter()
        .filter(|c| c.is_broadcast && c.monoforum_link_id != 0)
        .map(|c| {
            (
                c.monoforum_link_id,
                (
                    c.has_monoforum_admin_rights,
                    c.input.clone(),
                    !c.username.is_empty(),
                ),
            )
        })
        .collect();
    for c in &mut parsed_chats {
        if c.is_monoforum
            && let Some((admin_rights, broadcast_input, is_public)) = broadcast_info.get(&c.bare_id)
        {
            if *admin_rights {
                c.is_monoforum_admin = true;
                c.monoforum_broadcast_input = broadcast_input.clone();
            }
            c.is_monoforum_of_public_broadcast = *is_public;
        }
        let id = c.id();
        if !id.is_zero() {
            map.insert(id, Peer::Chat(c.clone()));
        }
    }
    map
}

/// Parses a TL `messages::Messages` response into chronological `MessagesSlice` (`ParseMessagesSlice`).
pub fn parse_messages_slice(
    users: &[tl_e::User],
    chats: &[tl_e::Chat],
    messages: &[tl_e::Message],
    self_id: PeerId,
    relative_path: &str,
) -> MessagesSlice {
    let peers = parse_peers_lists(users, chats);
    let mut list: Vec<Message> = messages
        .iter()
        .filter(|m| !matches!(m, tl_e::Message::Empty(_)))
        .map(|m| parse_message(m, self_id, relative_path))
        .collect();
    // Telegram returns history newest-first; export expects chronological oldest-first order.
    list.sort_by_key(|m| m.id);
    MessagesSlice { list, peers }
}

/// Parses `users.UserFull` into export `PersonalInfo` (`ParsePersonalInfo`).
pub fn parse_personal_info(data: &tl_e::users::UserFull) -> PersonalInfo {
    let tl_e::users::UserFull::UserFull(uf) = data;
    let tl_e::UserFull::UserFull(full) = &uf.full_user;
    let user = uf
        .users
        .iter()
        .map(parse_user)
        .find(|u| u.bare_id == full.id as BareId)
        .or_else(|| uf.users.first().map(parse_user))
        .unwrap_or_default();
    PersonalInfo {
        user,
        bio: full.about.clone().unwrap_or_default(),
    }
}

/// Parses `Vec<SavedContact>` into export `ContactsList` (`ParseContactsList`).
pub fn parse_contacts_list(contacts: &[tl_e::SavedContact]) -> ContactsList {
    let list = contacts
        .iter()
        .map(|c| {
            let tl_e::SavedContact::SavedPhoneContact(sc) = c;
            ContactInfo {
                user_id: UserId(0),
                first_name: sc.first_name.clone(),
                last_name: sc.last_name.clone(),
                phone_number: sc.phone.clone(),
                date: sc.date,
                color_index: peer_color_index(0),
            }
        })
        .collect();
    ContactsList {
        list,
        ..Default::default()
    }
}

/// Appends a `contacts.TopPeers` page into `ContactsList` (`AppendTopPeers`).
pub fn append_top_peers(to: &mut ContactsList, data: &tl_e::contacts::TopPeers) -> bool {
    let tl_e::contacts::TopPeers::TopPeers(tp) = data else {
        return false;
    };
    let peers_map = parse_peers_lists(&tp.users, &tp.chats);
    let mut appended = false;
    for category_peers in &tp.categories {
        let tl_e::TopPeerCategoryPeers::TopPeerCategoryPeers(cp) = category_peers;
        let target = match cp.category {
            tl_e::TopPeerCategory::Correspondents => &mut to.correspondents,
            tl_e::TopPeerCategory::BotsInline => &mut to.inline_bots,
            tl_e::TopPeerCategory::PhoneCalls => &mut to.phone_calls,
            _ => continue,
        };
        for top in &cp.peers {
            let tl_e::TopPeer::TopPeer(t) = top;
            let peer_id = parse_peer_id(&t.peer);
            if let Some(peer) = peers_map.get(&peer_id) {
                target.push(TopPeer {
                    peer: peer.clone(),
                    rating: t.rating,
                });
                appended = true;
            }
        }
    }
    appended
}

/// Parses `account.Authorizations` into `Vec<Session>` (`ParseSessionsList`).
pub fn parse_sessions_list(data: &tl_e::account::Authorizations) -> Vec<Session> {
    let tl_e::account::Authorizations::Authorizations(auths) = data;
    auths
        .authorizations
        .iter()
        .map(|a| {
            let tl_e::Authorization::Authorization(auth) = a;
            Session {
                application_id: auth.api_id,
                application_name: auth.app_name.clone(),
                application_version: auth.app_version.clone(),
                device_model: auth.device_model.clone(),
                platform: auth.platform.clone(),
                system_version: auth.system_version.clone(),
                ip: auth.ip.clone(),
                country: auth.country.clone(),
                region: auth.region.clone(),
                created: auth.date_created,
                last_active: auth.date_active,
            }
        })
        .collect()
}

/// Parses `account.WebAuthorizations` into `Vec<WebSession>` (`ParseWebSessionsList`).
pub fn parse_web_sessions_list(data: &tl_e::account::WebAuthorizations) -> Vec<WebSession> {
    let tl_e::account::WebAuthorizations::WebAuthorizations(auths) = data;
    let peers = parse_peers_lists(&auths.users, &[]);
    auths
        .authorizations
        .iter()
        .map(|a| {
            let tl_e::WebAuthorization::WebAuthorization(auth) = a;
            let bot_username = peers
                .get(&PeerId::from_user(UserId(auth.bot_id as u64)))
                .and_then(Peer::as_user)
                .map(|u| u.username.clone())
                .unwrap_or_default();
            WebSession {
                bot_username,
                domain: auth.domain.clone(),
                browser: auth.browser.clone(),
                platform: auth.platform.clone(),
                ip: auth.ip.clone(),
                region: auth.region.clone(),
                created: auth.date_created,
                last_active: auth.date_active,
            }
        })
        .collect()
}

/// Combines parsed client sessions and web sessions into `SessionsList`.
pub fn build_sessions_list(
    auths: &tl_e::account::Authorizations,
    web_auths: &tl_e::account::WebAuthorizations,
) -> SessionsList {
    SessionsList {
        list: parse_sessions_list(auths),
        web_list: parse_web_sessions_list(web_auths),
    }
}

/// Parses a slice of `photos::Photos` into `UserpicsSlice` (`ParseUserpicsSlice`).
pub fn parse_userpics_slice(
    photos: &[tl_e::Photo],
    base_index: usize,
    utc_offset_secs: i32,
) -> UserpicsSlice {
    let mut list = Vec::with_capacity(photos.len());
    for (idx, p) in photos.iter().enumerate() {
        let mut parsed = parse_photo(p, false);
        if parsed.id != 0 {
            parsed.image.file.suggested_path = format!(
                "profile_pictures/{}",
                prepare_photo_file_name(
                    (base_index + idx + 1) as i32,
                    parsed.date,
                    utc_offset_secs
                )
            );
            list.push(parsed);
        }
    }
    UserpicsSlice { list }
}

/// Parses `stories::Stories` into `StoriesSlice` (`ParseStoriesSlice`).
pub fn parse_stories_slice(
    stories: &[tl_e::StoryItem],
    base_index: usize,
    utc_offset_secs: i32,
) -> StoriesSlice {
    let mut list = Vec::new();
    let mut skipped = 0_i32;
    let mut last_id = 0_i32;
    for (idx, item) in stories.iter().enumerate() {
        let tl_e::StoryItem::StoryItem(st) = item else {
            last_id = match item {
                tl_e::StoryItem::Deleted(d) => d.id,
                tl_e::StoryItem::Skipped(s) => s.id,
                tl_e::StoryItem::StoryItem(x) => x.id,
            };
            skipped += 1;
            continue;
        };
        last_id = st.id;
        let caption = st
            .caption
            .as_deref()
            .map(|c| parse_text_tl(c, st.entities.as_deref().unwrap_or(&[])))
            .unwrap_or_default();
        let story_num = (base_index + idx + 1) as i32;
        let media = match st.media.as_ref() {
            tl_e::MessageMedia::Photo(p) => {
                let mut photo = p
                    .photo
                    .as_ref()
                    .map(|ph| parse_photo(ph, p.spoiler))
                    .unwrap_or_default();
                photo.image.file.suggested_path = format!(
                    "stories/{}",
                    prepare_story_file_name(story_num, st.date, ".jpg", utc_offset_secs)
                );
                Media {
                    content: MediaContent::Photo(photo),
                    ttl: p.ttl_seconds.unwrap_or(0),
                }
            }
            tl_e::MessageMedia::Document(d) => {
                let mut doc = d
                    .document
                    .as_ref()
                    .map(|doc| parse_document(doc, d.spoiler))
                    .unwrap_or_default();
                let main_name =
                    prepare_story_file_name(story_num, st.date, ".mp4", utc_offset_secs);
                doc.file.suggested_path = format!("stories/{main_name}");
                if doc.thumb.file.location.is_valid() {
                    doc.thumb.file.suggested_path = format!("stories/{main_name}_thumb.jpg");
                }
                Media {
                    content: MediaContent::Document(Box::new(doc)),
                    ttl: d.ttl_seconds.unwrap_or(0),
                }
            }
            _ => Media::default(),
        };
        list.push(Story {
            id: st.id,
            date: st.date,
            expires: st.expire_date,
            pinned: st.pinned,
            caption,
            media,
        });
    }
    StoriesSlice {
        list,
        last_id,
        skipped,
    }
}

/// Parses a `messages::Dialogs` page into `DialogInfo` entries for a given takeout split
/// (`ParseDialogsInfo`).
pub fn parse_dialogs_info(
    dialogs: &[tl_e::Dialog],
    messages: &[tl_e::Message],
    users: &[tl_e::User],
    chats: &[tl_e::Chat],
    split_index: i32,
) -> Vec<DialogInfo> {
    let peers = parse_peers_lists(users, chats);
    let mut msg_dates: HashMap<(PeerId, i32), TimeId> = HashMap::new();
    for m in messages {
        match m {
            tl_e::Message::Message(msg) => {
                msg_dates.insert((parse_peer_id(&msg.peer_id), msg.id), msg.date);
            }
            tl_e::Message::Service(msg) => {
                msg_dates.insert((parse_peer_id(&msg.peer_id), msg.id), msg.date);
            }
            tl_e::Message::Empty(_) => {}
        }
    }
    let mut result = Vec::new();
    for d in dialogs {
        let tl_e::Dialog::Dialog(dlg) = d else {
            continue;
        };
        let peer_id = parse_peer_id(&dlg.peer);
        let Some(peer) = peers.get(&peer_id) else {
            continue;
        };
        let top_message_date = msg_dates
            .get(&(peer_id, dlg.top_message))
            .copied()
            .unwrap_or(0);
        let mut info = dialog_info_from_peer(peer, dlg.top_message, top_message_date);
        info.splits.push(split_index);
        info.messages_count_per_split.push(0);
        result.push(info);
    }
    result
}

/// Builds a `DialogInfo` from a parsed `Peer`.
pub fn dialog_info_from_peer(
    peer: &Peer,
    top_message_id: i32,
    top_message_date: TimeId,
) -> DialogInfo {
    match peer {
        Peer::User(u) => DialogInfo {
            kind: dialog_type_from_user(u),
            name: u.info.first_name.clone(),
            last_name: u.info.last_name.clone(),
            input: peer.input(),
            top_message_id,
            top_message_date,
            peer_id: u.id(),
            color_index: u.color_index,
            ..Default::default()
        },
        Peer::Chat(c) => DialogInfo {
            kind: dialog_type_from_chat(c),
            name: c.title.clone(),
            last_name: String::new(),
            input: c.input.clone(),
            top_message_id,
            top_message_date,
            peer_id: c.id(),
            color_index: c.color_index,
            migrated_to_channel_id: c.migrated_to_channel_id,
            monoforum_broadcast_input: c.monoforum_broadcast_input.clone(),
            is_monoforum: c.is_monoforum,
            ..Default::default()
        },
    }
}

/// Builds a `DialogsInfo` from a single-peer `User` lookup (`ParseDialogsInfo` for single user).
pub fn parse_single_user_dialogs_info(user: &tl_e::User, splits_count: i32) -> DialogsInfo {
    let parsed = parse_user(user);
    let mut info = dialog_info_from_peer(&Peer::User(parsed), 0, 0);
    for split in 0..splits_count.max(1) {
        info.splits.push(split);
        info.messages_count_per_split.push(0);
    }
    DialogsInfo {
        chats: vec![info],
        left: Vec::new(),
    }
}

/// Builds a `DialogsInfo` from a single-peer `Chat` lookup (`ParseDialogsInfo` for single chat/channel).
pub fn parse_single_chat_dialogs_info(chat: &tl_e::Chat, splits_count: i32) -> DialogsInfo {
    let parsed = parse_chat(chat);
    let mut info = dialog_info_from_peer(&Peer::Chat(parsed), 0, 0);
    for split in 0..splits_count.max(1) {
        info.splits.push(split);
        info.messages_count_per_split.push(0);
    }
    DialogsInfo {
        chats: vec![info],
        left: Vec::new(),
    }
}

/// Parses `channels.getLeftChannels` response (`messages::Chats`) into left-channel `DialogInfo`s
/// (`ParseLeftChannelsInfo`).
pub fn parse_left_channels_info(chats: &[tl_e::Chat]) -> Vec<DialogInfo> {
    chats
        .iter()
        .filter_map(|c| {
            let parsed = parse_chat(c);
            if parsed.bare_id == 0 {
                return None;
            }
            let mut info = dialog_info_from_peer(&Peer::Chat(parsed), 0, 0);
            info.is_left_channel = true;
            info.only_my_messages = true;
            info.splits.push(-1);
            info.messages_count_per_split.push(0);
            Some(info)
        })
        .collect()
}

/// Returns whether a dialog of type `kind` is enabled by `settings.types` (`IsDialogIncluded`).
pub fn is_dialog_included(kind: DialogType, settings: &Settings) -> bool {
    if settings.only_single_peer() {
        return true;
    }
    let flag = match kind {
        DialogType::SelfChat
        | DialogType::Replies
        | DialogType::VerifyCodes
        | DialogType::Personal
        | DialogType::Unknown => ExportTypes::PERSONAL_CHATS,
        DialogType::Bot => ExportTypes::BOT_CHATS,
        DialogType::PrivateGroup | DialogType::PrivateSupergroup => ExportTypes::PRIVATE_GROUPS,
        DialogType::PrivateChannel => ExportTypes::PRIVATE_CHANNELS,
        DialogType::PublicSupergroup => ExportTypes::PUBLIC_GROUPS,
        DialogType::PublicChannel => ExportTypes::PUBLIC_CHANNELS,
    };
    settings.types.contains(flag)
}

/// Returns `true` if a `messages::Messages` response contains a single message strictly before `date`
/// (`SingleMessageBefore`).
pub fn single_message_before(messages: &[tl_e::Message], date: TimeId) -> bool {
    messages.first().is_some_and(|m| match m {
        tl_e::Message::Message(msg) => msg.date < date,
        tl_e::Message::Service(msg) => msg.date < date,
        tl_e::Message::Empty(_) => false,
    })
}

/// Returns `true` if a `messages::Messages` response contains a single message on or after `date`
/// (`SingleMessageAfter`).
pub fn single_message_after(messages: &[tl_e::Message], date: TimeId) -> bool {
    messages.first().is_some_and(|m| match m {
        tl_e::Message::Message(msg) => msg.date >= date,
        tl_e::Message::Service(msg) => msg.date >= date,
        tl_e::Message::Empty(_) => false,
    })
}

/// Helper to build a `DialogInfo` color index from a `PeerId`.
pub fn dialog_color_index(peer_id: PeerId) -> u8 {
    peer_color_index_peer(peer_id)
}
