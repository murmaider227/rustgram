//! Media, attachment, reaction, and service-action TL -> domain conversions.
//!
//! Port origin: `data/data_media_types.cpp`, `data/data_photo.cpp`, `data/data_document.cpp`,
//! `data/data_web_page.cpp`, `data/data_poll.cpp`, `history/history_item.cpp`.

use tg_data::{
    Birthday, BotAppId, CallId, ChannelId, ChatId, ContactData, DiceData, DocumentData, DocumentId,
    DocumentKind, ExtendedMedia, GameData, GameId, GeoPoint, GiveawayData, GiveawayResultsData,
    InvoiceData, LiveLocationData, MediaWebPageFlags, MessageMedia, MessageReactionCount,
    MessageReactionsSummary, MsgId, PaidMediaData, PeerId, PhoneCallDiscardReason, PhotoData,
    PhotoId, PhotoSize, PollAnswer, PollAnswerVoters, PollData, PollId, PollMedia, PollResults,
    ReactionId, RecentReactor, ServiceAction, StarsAmount, StickerMeta, StoryMediaData,
    TextWithEntities, TodoCompletionData, TodoItemData, TodoListData, UserId, VenueData,
    VideoStreamData, WallPaperId, WebDocumentData, WebPageData, WebPageId, WidgetData,
};
use tg_tl::enums as tl_enums;
use tg_tl::types as tl_types;

use crate::convert::messages::{entities_from_tl, text_with_entities_from_tl};
use crate::convert::peers::peer_id_from_tl;

/// Converts a TL `GeoPoint` into `Option<tg_data::GeoPoint>`.
#[must_use]
pub const fn geo_point_from_tl(geo: &tl_enums::GeoPoint) -> Option<GeoPoint> {
    match geo {
        tl_enums::GeoPoint::Empty => None,
        tl_enums::GeoPoint::GeoPoint(g) => Some(GeoPoint {
            lat: g.lat,
            lon: g.long,
            accuracy_radius: g.accuracy_radius,
        }),
    }
}

/// Extracts a `PhotoId` from a TL `Photo` (`None` for `photoEmpty`).
#[must_use]
pub const fn photo_id_from_tl(photo: &tl_enums::Photo) -> Option<PhotoId> {
    match photo {
        tl_enums::Photo::Empty(_) => None,
        tl_enums::Photo::Photo(p) => Some(PhotoId::from_tl(p.id)),
    }
}

fn extract_photo_sizes(sizes: &[tl_enums::PhotoSize]) -> (Vec<PhotoSize>, Option<Vec<u8>>) {
    let mut out = Vec::with_capacity(sizes.len());
    let mut stripped = None;
    for sz in sizes {
        match sz {
            tl_enums::PhotoSize::Empty(_) | tl_enums::PhotoSize::PhotoPathSize(_) => {}
            tl_enums::PhotoSize::PhotoSize(s) => {
                out.push(PhotoSize {
                    kind: s.r#type.clone(),
                    width: s.w,
                    height: s.h,
                    size: i64::from(s.size),
                });
            }
            tl_enums::PhotoSize::PhotoCachedSize(s) => {
                out.push(PhotoSize {
                    kind: s.r#type.clone(),
                    width: s.w,
                    height: s.h,
                    size: s.bytes.len() as i64,
                });
            }
            tl_enums::PhotoSize::Progressive(s) => {
                let size = s.sizes.last().copied().unwrap_or(0);
                out.push(PhotoSize {
                    kind: s.r#type.clone(),
                    width: s.w,
                    height: s.h,
                    size: i64::from(size),
                });
            }
            tl_enums::PhotoSize::PhotoStrippedSize(s) => {
                stripped = Some(s.bytes.clone());
            }
        }
    }
    (out, stripped)
}

/// Converts a TL `Photo` into `Option<tg_data::PhotoData>` (`None` for `photoEmpty`).
#[must_use]
pub fn photo_from_tl(photo: &tl_enums::Photo) -> Option<PhotoData> {
    let tl_enums::Photo::Photo(p) = photo else {
        return None;
    };
    let (sizes, stripped_thumb) = extract_photo_sizes(&p.sizes);
    Some(PhotoData {
        id: PhotoId::from_tl(p.id),
        access_hash: p.access_hash as u64,
        file_reference: p.file_reference.clone(),
        date: p.date,
        dc_id: p.dc_id,
        sizes,
        stripped_thumb,
        has_stickers: p.has_stickers,
        spoiler: false,
        ttl_seconds: None,
        live_photo_video_id: None,
    })
}

/// Converts a TL `Document` into `Option<tg_data::DocumentData>` (`None` for `documentEmpty`).
#[must_use]
pub fn document_from_tl(doc: &tl_enums::Document) -> Option<DocumentData> {
    let tl_enums::Document::Document(d) = doc else {
        return None;
    };
    let (thumbs, stripped_thumb) = d
        .thumbs
        .as_deref()
        .map(extract_photo_sizes)
        .unwrap_or_default();

    let mut is_animated = false;
    let mut is_video = false;
    let mut is_round = false;
    let mut is_voice = false;
    let mut is_audio = false;
    let mut is_sticker = false;
    let mut file_name = String::new();
    let mut duration: Option<f64> = None;
    let mut width: Option<i32> = None;
    let mut height: Option<i32> = None;
    let mut audio_title: Option<String> = None;
    let mut audio_performer: Option<String> = None;
    let mut waveform: Option<Vec<u8>> = None;
    let mut sticker: Option<StickerMeta> = None;
    let mut supports_streaming = false;

    for attr in &d.attributes {
        match attr {
            tl_enums::DocumentAttribute::ImageSize(s) => {
                width = Some(s.w);
                height = Some(s.h);
            }
            tl_enums::DocumentAttribute::Animated => {
                is_animated = true;
            }
            tl_enums::DocumentAttribute::Sticker(s) => {
                is_sticker = true;
                let (set_id, set_access_hash) = match &s.stickerset {
                    tl_enums::InputStickerSet::ID(id) => (id.id as u64, id.access_hash as u64),
                    _ => (0, 0),
                };
                sticker = Some(StickerMeta {
                    alt: s.alt.clone(),
                    set_id,
                    set_access_hash,
                    is_mask: s.mask,
                });
            }
            tl_enums::DocumentAttribute::CustomEmoji(c) => {
                is_sticker = true;
                let (set_id, set_access_hash) = match &c.stickerset {
                    tl_enums::InputStickerSet::ID(id) => (id.id as u64, id.access_hash as u64),
                    _ => (0, 0),
                };
                sticker = Some(StickerMeta {
                    alt: c.alt.clone(),
                    set_id,
                    set_access_hash,
                    is_mask: false,
                });
            }
            tl_enums::DocumentAttribute::Video(v) => {
                is_video = true;
                is_round = v.round_message;
                supports_streaming = v.supports_streaming;
                duration = Some(v.duration);
                width = Some(v.w);
                height = Some(v.h);
            }
            tl_enums::DocumentAttribute::Audio(a) => {
                if a.voice {
                    is_voice = true;
                } else {
                    is_audio = true;
                }
                duration = Some(f64::from(a.duration));
                audio_title = a.title.clone();
                audio_performer = a.performer.clone();
                waveform = a.waveform.clone();
            }
            tl_enums::DocumentAttribute::Filename(f) => {
                file_name = f.file_name.clone();
            }
            tl_enums::DocumentAttribute::HasStickers => {}
        }
    }

    let kind = if is_sticker {
        if d.mime_type == "application/x-tgsticker" {
            DocumentKind::AnimatedSticker
        } else if d.mime_type == "video/webm" {
            DocumentKind::VideoSticker
        } else {
            DocumentKind::Sticker
        }
    } else if is_round {
        DocumentKind::RoundVideo
    } else if is_animated && (is_video || d.mime_type == "image/gif" || d.mime_type == "video/mp4")
    {
        DocumentKind::Gif
    } else if is_video {
        DocumentKind::Video
    } else if is_voice {
        DocumentKind::Voice
    } else if is_audio {
        DocumentKind::Audio
    } else {
        DocumentKind::File
    };

    Some(DocumentData {
        id: DocumentId::from_tl(d.id),
        access_hash: d.access_hash as u64,
        file_reference: d.file_reference.clone(),
        date: d.date,
        mime_type: d.mime_type.clone(),
        size: d.size,
        dc_id: d.dc_id,
        thumbs,
        stripped_thumb,
        kind,
        file_name,
        duration,
        width,
        height,
        audio_title,
        audio_performer,
        waveform,
        sticker,
        supports_streaming,
        spoiler: false,
        ttl_seconds: None,
        video_timestamp: None,
        video_cover: None,
    })
}

/// Converts a TL `WebPage` into `Option<tg_data::WebPageData>`.
#[must_use]
pub fn webpage_from_tl(wp: &tl_enums::WebPage) -> Option<WebPageData> {
    match wp {
        tl_enums::WebPage::Empty(_) | tl_enums::WebPage::NotModified(_) => None,
        tl_enums::WebPage::Pending(p) => Some(WebPageData {
            id: WebPageId::from_tl(p.id),
            url: p.url.clone().unwrap_or_default(),
            display_url: p.url.clone().unwrap_or_default(),
            kind: WebPageData::KIND_PENDING.to_owned(),
            ..WebPageData::default()
        }),
        tl_enums::WebPage::WebPage(w) => {
            let mut flags = MediaWebPageFlags::empty();
            if w.has_large_media {
                flags.insert(MediaWebPageFlags::FORCE_LARGE_MEDIA);
            }
            Some(WebPageData {
                id: WebPageId::from_tl(w.id),
                url: w.url.clone(),
                display_url: w.display_url.clone(),
                kind: w.r#type.clone().unwrap_or_default(),
                site_name: w.site_name.clone(),
                title: w.title.clone(),
                description: w.description.clone(),
                author: w.author.clone(),
                photo: w.photo.as_ref().and_then(photo_from_tl),
                document: w.document.as_ref().and_then(document_from_tl),
                photo_is_video_cover: w.video_cover_photo,
                flags,
            })
        }
    }
}

/// Converts a TL `Reaction` into `Option<tg_data::ReactionId>`.
#[must_use]
pub fn reaction_from_tl(reaction: &tl_enums::Reaction) -> Option<ReactionId> {
    match reaction {
        tl_enums::Reaction::Empty => None,
        tl_enums::Reaction::Emoji(e) => Some(ReactionId::Emoji(e.emoticon.clone())),
        tl_enums::Reaction::CustomEmoji(c) => {
            Some(ReactionId::Custom(DocumentId::from_tl(c.document_id)))
        }
        tl_enums::Reaction::Paid => Some(ReactionId::Paid),
    }
}

/// Converts a domain `tg_data::ReactionId` into a TL `Reaction` (`Data::ReactionToMTP`).
#[must_use]
pub fn reaction_to_tl(reaction: &ReactionId) -> tl_enums::Reaction {
    match reaction {
        ReactionId::Emoji(emoticon) => tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: emoticon.clone(),
        }),
        ReactionId::Custom(doc_id) => {
            tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
                document_id: doc_id.to_tl(),
            })
        }
        ReactionId::Paid => tl_enums::Reaction::Paid,
    }
}

/// Converts a TL `MessageReactions` into `tg_data::MessageReactionsSummary`.
#[must_use]
pub fn reactions_from_tl(reactions: &tl_enums::MessageReactions) -> MessageReactionsSummary {
    let tl_enums::MessageReactions::MessageReactions(r) = reactions;
    let results = r
        .results
        .iter()
        .filter_map(|rc| {
            let tl_enums::ReactionCount::ReactionCount(c) = rc;
            let id = reaction_from_tl(&c.reaction)?;
            Some(MessageReactionCount {
                id,
                count: c.count,
                chosen_order: c.chosen_order,
            })
        })
        .collect();
    let recent_reactors = r
        .recent_reactions
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|pr| {
            let tl_enums::MessagePeerReaction::MessagePeerReaction(p) = pr;
            let reaction = reaction_from_tl(&p.reaction)?;
            Some(RecentReactor {
                peer_id: peer_id_from_tl(&p.peer_id),
                reaction,
                date: p.date,
                unread: p.unread,
                my: p.my,
            })
        })
        .collect();
    MessageReactionsSummary {
        results,
        recent_reactors,
        can_see_list: r.can_see_list,
        reactions_as_tags: r.reactions_as_tags,
        min: r.min,
    }
}

/// Converts a TL `StarsAmount` (`starsAmount` or `starsTonAmount`) into `tg_data::StarsAmount`.
#[must_use]
pub const fn stars_amount_from_tl(amount: &tl_enums::StarsAmount) -> StarsAmount {
    match amount {
        tl_enums::StarsAmount::StarsAmount(s) => StarsAmount {
            amount: s.amount,
            nanos: s.nanos,
            is_ton: false,
        },
        tl_enums::StarsAmount::StarsTonAmount(t) => StarsAmount {
            amount: t.amount,
            nanos: 0,
            is_ton: true,
        },
    }
}

fn extended_media_from_tl(em: &tl_enums::MessageExtendedMedia) -> ExtendedMedia {
    match em {
        tl_enums::MessageExtendedMedia::Preview(p) => {
            let thumb = p.thumb.as_ref().and_then(|sz| match sz {
                tl_enums::PhotoSize::PhotoStrippedSize(s) => Some(s.bytes.clone()),
                tl_enums::PhotoSize::PhotoCachedSize(s) => Some(s.bytes.clone()),
                _ => None,
            });
            ExtendedMedia::Preview {
                width: p.w,
                height: p.h,
                thumb,
                duration: p.video_duration,
            }
        }
        tl_enums::MessageExtendedMedia::MessageExtendedMedia(m) => {
            ExtendedMedia::Media(Box::new(message_media_from_tl(&m.media)))
        }
    }
}

/// Converts a TL `WebDocument` (`Images::FromWebDocument`): its location and image size.
fn web_document_from_tl(doc: &tl_enums::WebDocument) -> WebDocumentData {
    let (url, access_hash, size, mime_type, attributes) = match doc {
        tl_enums::WebDocument::WebDocument(d) => (
            &d.url,
            Some(d.access_hash),
            d.size,
            &d.mime_type,
            &d.attributes,
        ),
        tl_enums::WebDocument::NoProxy(d) => (&d.url, None, d.size, &d.mime_type, &d.attributes),
    };
    let (width, height) = attributes
        .iter()
        .find_map(|attr| match attr {
            tl_enums::DocumentAttribute::ImageSize(s) => Some((s.w, s.h)),
            _ => None,
        })
        .unwrap_or((0, 0));
    WebDocumentData {
        url: url.clone(),
        access_hash,
        size,
        mime_type: mime_type.clone(),
        width,
        height,
    }
}

fn poll_answer_from_tl(answer: &tl_enums::PollAnswer) -> PollAnswer {
    match answer {
        tl_enums::PollAnswer::PollAnswer(a) => PollAnswer {
            text: text_with_entities_from_tl(&a.text),
            option: a.option.clone(),
        },
        tl_enums::PollAnswer::InputPollAnswer(a) => PollAnswer {
            text: text_with_entities_from_tl(&a.text),
            option: Vec::new(),
        },
    }
}

fn todo_item_from_tl(item: &tl_enums::TodoItem) -> TodoItemData {
    let tl_enums::TodoItem::TodoItem(it) = item;
    TodoItemData {
        id: it.id,
        title: text_with_entities_from_tl(&it.title),
    }
}

fn group_call_id_and_hash(call: &tl_enums::InputGroupCall) -> (CallId, u64) {
    match call {
        tl_enums::InputGroupCall::InputGroupCall(c) => {
            (CallId::from_tl(c.id), c.access_hash as u64)
        }
        tl_enums::InputGroupCall::Slug(_) | tl_enums::InputGroupCall::InviteMessage(_) => {
            (CallId::ZERO, 0)
        }
    }
}

/// Converts any TL `MessageMedia` (all 20 Layer 233 variants) into `tg_data::MessageMedia`.
#[must_use]
pub fn message_media_from_tl(media: &tl_enums::MessageMedia) -> MessageMedia {
    match media {
        tl_enums::MessageMedia::Empty => MessageMedia::Empty,
        tl_enums::MessageMedia::Unsupported => MessageMedia::Unsupported,
        tl_enums::MessageMedia::Photo(p) => {
            let mut data = p.photo.as_ref().and_then(photo_from_tl).unwrap_or_default();
            data.spoiler = p.spoiler;
            data.ttl_seconds = p.ttl_seconds;
            data.live_photo_video_id = p.video.as_ref().and_then(|v| match v {
                tl_enums::Document::Document(d) => Some(DocumentId::from_tl(d.id)),
                tl_enums::Document::Empty(_) => None,
            });
            MessageMedia::Photo(Box::new(data))
        }
        tl_enums::MessageMedia::Document(d) => {
            let mut data = d
                .document
                .as_ref()
                .and_then(document_from_tl)
                .unwrap_or_default();
            data.spoiler = d.spoiler;
            data.ttl_seconds = d.ttl_seconds;
            data.video_timestamp = d.video_timestamp;
            data.video_cover = d.video_cover.as_ref().and_then(photo_from_tl).map(Box::new);
            if d.round {
                data.kind = DocumentKind::RoundVideo;
            } else if d.voice {
                data.kind = DocumentKind::Voice;
            } else if d.video && data.kind == DocumentKind::File {
                data.kind = DocumentKind::Video;
            }
            MessageMedia::Document(Box::new(data))
        }
        tl_enums::MessageMedia::WebPage(w) => {
            // `CreateMedia` (`history/history_item.cpp`): `webPageEmpty` and
            // `webPageNotModified` create no media at all.
            let Some(mut data) = webpage_from_tl(&w.webpage) else {
                return MessageMedia::Empty;
            };
            data.flags
                .set(MediaWebPageFlags::FORCE_LARGE_MEDIA, w.force_large_media);
            data.flags
                .set(MediaWebPageFlags::FORCE_SMALL_MEDIA, w.force_small_media);
            data.flags.set(MediaWebPageFlags::MANUAL, w.manual);
            data.flags.set(MediaWebPageFlags::SAFE, w.safe);
            MessageMedia::WebPage(Box::new(data))
        }
        tl_enums::MessageMedia::Geo(g) => {
            MessageMedia::Location(geo_point_from_tl(&g.geo).unwrap_or_default())
        }
        tl_enums::MessageMedia::Contact(c) => MessageMedia::Contact(Box::new(ContactData {
            phone_number: c.phone_number.clone(),
            first_name: c.first_name.clone(),
            last_name: c.last_name.clone(),
            vcard: c.vcard.clone(),
            user_id: UserId::from_tl(c.user_id),
        })),
        tl_enums::MessageMedia::Venue(v) => MessageMedia::Venue(Box::new(VenueData {
            geo: geo_point_from_tl(&v.geo).unwrap_or_default(),
            title: v.title.clone(),
            address: v.address.clone(),
            provider: v.provider.clone(),
            venue_id: v.venue_id.clone(),
            venue_type: v.venue_type.clone(),
        })),
        tl_enums::MessageMedia::GeoLive(gl) => {
            MessageMedia::LiveLocation(Box::new(LiveLocationData {
                geo: geo_point_from_tl(&gl.geo).unwrap_or_default(),
                heading: gl.heading,
                period: gl.period,
                proximity_notification_radius: gl.proximity_notification_radius,
            }))
        }
        tl_enums::MessageMedia::Dice(d) => MessageMedia::Dice(DiceData {
            value: d.value,
            emoticon: d.emoticon.clone(),
        }),
        tl_enums::MessageMedia::Game(g) => {
            let tl_enums::Game::Game(game) = &g.game;
            MessageMedia::Game(Box::new(GameData {
                id: GameId::from_tl(game.id),
                access_hash: game.access_hash as u64,
                short_name: game.short_name.clone(),
                title: game.title.clone(),
                description: game.description.clone(),
                photo: photo_from_tl(&game.photo),
                document: game.document.as_ref().and_then(document_from_tl),
            }))
        }
        tl_enums::MessageMedia::Invoice(inv) => MessageMedia::Invoice(Box::new(InvoiceData {
            title: inv.title.clone(),
            description: inv.description.clone(),
            currency: inv.currency.clone(),
            total_amount: inv.total_amount,
            start_param: inv.start_param.clone(),
            receipt_msg_id: inv.receipt_msg_id.map(|id| MsgId(i64::from(id))),
            shipping_address_requested: inv.shipping_address_requested,
            test: inv.test,
            extended_media: inv
                .extended_media
                .as_deref()
                .map(|em| Box::new(extended_media_from_tl(em))),
            photo: inv.photo.as_ref().map(web_document_from_tl),
        })),
        tl_enums::MessageMedia::Poll(p) => {
            let tl_enums::Poll::Poll(poll) = &p.poll;
            let tl_enums::PollResults::PollResults(res) = p.results.as_ref();
            let results = PollResults {
                results: res
                    .results
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|v| {
                        let tl_enums::PollAnswerVoters::PollAnswerVoters(av) = v;
                        PollAnswerVoters {
                            option: av.option.clone(),
                            voters: av.voters.unwrap_or(0),
                            chosen: av.chosen,
                            correct: av.correct,
                        }
                    })
                    .collect(),
                total_voters: res.total_voters,
                recent_voters: res
                    .recent_voters
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(peer_id_from_tl)
                    .collect(),
                solution: res.solution.as_ref().map(|s| {
                    TextWithEntities::new(
                        s.clone(),
                        res.solution_entities
                            .as_deref()
                            .map(entities_from_tl)
                            .unwrap_or_default(),
                    )
                }),
            };
            MessageMedia::Poll(Box::new(PollData {
                id: PollId::from_tl(poll.id),
                question: text_with_entities_from_tl(&poll.question),
                answers: poll.answers.iter().map(poll_answer_from_tl).collect(),
                closed: poll.closed,
                public_voters: poll.public_voters,
                multiple_choice: poll.multiple_choice,
                quiz: poll.quiz,
                close_period: poll.close_period,
                close_date: poll.close_date,
                results,
                attached_media: p
                    .attached_media
                    .as_deref()
                    .and_then(|m| PollMedia::from_media(&message_media_from_tl(m)))
                    .map(Box::new),
                answer_media: poll
                    .answers
                    .iter()
                    .filter_map(|answer| match answer {
                        tl_enums::PollAnswer::PollAnswer(a) => {
                            let media = a.media.as_ref().map(message_media_from_tl)?;
                            Some((a.option.clone(), PollMedia::from_media(&media)?))
                        }
                        tl_enums::PollAnswer::InputPollAnswer(_) => None,
                    })
                    .collect(),
            }))
        }
        tl_enums::MessageMedia::Story(s) => MessageMedia::Story(StoryMediaData {
            peer_id: peer_id_from_tl(&s.peer),
            story_id: s.id,
            via_mention: s.via_mention,
        }),
        tl_enums::MessageMedia::Giveaway(g) => MessageMedia::Giveaway(Box::new(GiveawayData {
            channels: g.channels.iter().copied().map(ChannelId::from_tl).collect(),
            countries_iso2: g.countries_iso2.clone().unwrap_or_default(),
            prize_description: g.prize_description.clone(),
            quantity: g.quantity,
            months: g.months,
            stars: g.stars,
            until_date: g.until_date,
            only_new_subscribers: g.only_new_subscribers,
            winners_are_visible: g.winners_are_visible,
        })),
        tl_enums::MessageMedia::GiveawayResults(g) => {
            MessageMedia::GiveawayResults(Box::new(GiveawayResultsData {
                channel_id: ChannelId::from_tl(g.channel_id),
                additional_peers_count: g.additional_peers_count.unwrap_or(0),
                launch_msg_id: MsgId(i64::from(g.launch_msg_id)),
                winners_count: g.winners_count,
                unclaimed_count: g.unclaimed_count,
                winners: g.winners.iter().copied().map(UserId::from_tl).collect(),
                months: g.months,
                stars: g.stars,
                prize_description: g.prize_description.clone(),
                until_date: g.until_date,
                only_new_subscribers: g.only_new_subscribers,
                refunded: g.refunded,
            }))
        }
        tl_enums::MessageMedia::PaidMedia(pm) => MessageMedia::PaidMedia(Box::new(PaidMediaData {
            stars_amount: pm.stars_amount,
            extended_media: pm
                .extended_media
                .iter()
                .map(extended_media_from_tl)
                .collect(),
        })),
        tl_enums::MessageMedia::ToDo(td) => {
            let tl_enums::TodoList::TodoList(list) = &td.todo;
            let completions = td
                .completions
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|c| {
                    let tl_enums::TodoCompletion::TodoCompletion(tc) = c;
                    TodoCompletionData {
                        id: tc.id,
                        completed_by: peer_id_from_tl(&tc.completed_by),
                        date: tc.date,
                    }
                })
                .collect();
            MessageMedia::TodoList(Box::new(TodoListData {
                title: text_with_entities_from_tl(&list.title),
                items: list.list.iter().map(todo_item_from_tl).collect(),
                completions,
                others_can_append: list.others_can_append,
                others_can_complete: list.others_can_complete,
            }))
        }
        tl_enums::MessageMedia::VideoStream(vs) => {
            let (call_id, access_hash) = group_call_id_and_hash(&vs.call);
            MessageMedia::VideoStream(VideoStreamData {
                call_id,
                access_hash,
                rtmp_stream: vs.rtmp_stream,
            })
        }
        tl_enums::MessageMedia::Widget(w) => MessageMedia::Widget(Box::new(WidgetData {
            widget_id: w.widget_id,
            width: w.width,
            height: w.height,
            payload: w.payload.clone(),
            is_copy: w.is_copy,
        })),
    }
}

fn secure_value_type_name(t: &tl_enums::SecureValueType) -> String {
    match t {
        tl_enums::SecureValueType::PersonalDetails => "personal_details",
        tl_enums::SecureValueType::Passport => "passport",
        tl_enums::SecureValueType::DriverLicense => "driver_license",
        tl_enums::SecureValueType::IdentityCard => "identity_card",
        tl_enums::SecureValueType::InternalPassport => "internal_passport",
        tl_enums::SecureValueType::Address => "address",
        tl_enums::SecureValueType::UtilityBill => "utility_bill",
        tl_enums::SecureValueType::BankStatement => "bank_statement",
        tl_enums::SecureValueType::RentalAgreement => "rental_agreement",
        tl_enums::SecureValueType::PassportRegistration => "passport_registration",
        tl_enums::SecureValueType::TemporaryRegistration => "temporary_registration",
        tl_enums::SecureValueType::Phone => "phone",
        tl_enums::SecureValueType::Email => "email",
    }
    .to_owned()
}

fn star_gift_id_and_stars(gift: &tl_enums::StarGift) -> (u64, i64, String) {
    match gift {
        tl_enums::StarGift::StarGift(g) => (g.id as u64, g.stars, String::new()),
        tl_enums::StarGift::Unique(u) => (u.id as u64, 0, u.slug.clone()),
    }
}

/// Converts any TL `MessageAction` (all 72 Layer 233 constructors) into `tg_data::ServiceAction`.
#[must_use]
pub fn message_action_from_tl(action: &tl_enums::MessageAction) -> ServiceAction {
    match action {
        tl_enums::MessageAction::Empty => ServiceAction::Empty,
        tl_enums::MessageAction::ChatCreate(a) => ServiceAction::ChatCreate {
            title: a.title.clone(),
            users: a.users.iter().copied().map(UserId::from_tl).collect(),
        },
        tl_enums::MessageAction::ChatEditTitle(a) => ServiceAction::ChatEditTitle {
            title: a.title.clone(),
        },
        tl_enums::MessageAction::ChatEditPhoto(a) => ServiceAction::ChatEditPhoto {
            photo: photo_from_tl(&a.photo).map(Box::new),
        },
        tl_enums::MessageAction::ChatDeletePhoto => ServiceAction::ChatDeletePhoto,
        tl_enums::MessageAction::ChatAddUser(a) => ServiceAction::ChatAddUser {
            users: a.users.iter().copied().map(UserId::from_tl).collect(),
        },
        tl_enums::MessageAction::ChatDeleteUser(a) => ServiceAction::ChatDeleteUser {
            user_id: UserId::from_tl(a.user_id),
        },
        tl_enums::MessageAction::ChatJoinedByLink(a) => ServiceAction::ChatJoinedByLink {
            inviter_id: UserId::from_tl(a.inviter_id),
        },
        tl_enums::MessageAction::ChannelCreate(a) => ServiceAction::ChannelCreate {
            title: a.title.clone(),
        },
        tl_enums::MessageAction::ChatMigrateTo(a) => ServiceAction::ChatMigrateTo {
            channel_id: ChannelId::from_tl(a.channel_id),
        },
        tl_enums::MessageAction::ChannelMigrateFrom(a) => ServiceAction::ChannelMigrateFrom {
            title: a.title.clone(),
            chat_id: ChatId::from_tl(a.chat_id),
        },
        tl_enums::MessageAction::PinMessage => ServiceAction::PinMessage,
        tl_enums::MessageAction::HistoryClear => ServiceAction::HistoryClear,
        tl_enums::MessageAction::GameScore(a) => ServiceAction::GameScore {
            game_id: GameId::from_tl(a.game_id),
            score: a.score,
        },
        tl_enums::MessageAction::PaymentSentMe(a) => ServiceAction::PaymentSentMe {
            currency: a.currency.clone(),
            total_amount: a.total_amount,
            payload: a.payload.clone(),
            shipping_option_id: a.shipping_option_id.clone(),
            recurring_init: a.recurring_init,
            recurring_used: a.recurring_used,
            subscription_until_date: a.subscription_until_date,
        },
        tl_enums::MessageAction::PaymentSent(a) => ServiceAction::PaymentSent {
            currency: a.currency.clone(),
            total_amount: a.total_amount,
            invoice_slug: a.invoice_slug.clone(),
            recurring_init: a.recurring_init,
            recurring_used: a.recurring_used,
            subscription_until_date: a.subscription_until_date,
        },
        tl_enums::MessageAction::PhoneCall(a) => ServiceAction::PhoneCall {
            call_id: CallId::from_tl(a.call_id),
            video: a.video,
            reason: a.reason.as_ref().map(|r| match r {
                tl_enums::PhoneCallDiscardReason::Missed => PhoneCallDiscardReason::Missed,
                tl_enums::PhoneCallDiscardReason::Disconnect => PhoneCallDiscardReason::Disconnect,
                tl_enums::PhoneCallDiscardReason::Hangup => PhoneCallDiscardReason::Hangup,
                tl_enums::PhoneCallDiscardReason::Busy => PhoneCallDiscardReason::Busy,
                tl_enums::PhoneCallDiscardReason::MigrateConferenceCall(_) => {
                    PhoneCallDiscardReason::MigrateConferenceCall
                }
            }),
            duration: a.duration,
        },
        tl_enums::MessageAction::ScreenshotTaken => ServiceAction::ScreenshotTaken,
        tl_enums::MessageAction::CustomAction(a) => ServiceAction::CustomAction {
            message: a.message.clone(),
        },
        tl_enums::MessageAction::BotAllowed(a) => ServiceAction::BotAllowed {
            attach_menu: a.attach_menu,
            from_request: a.from_request,
            domain: a.domain.clone(),
            app_id: a.app.as_ref().and_then(|app| match app {
                tl_enums::BotApp::BotApp(b) => Some(BotAppId::from_tl(b.id)),
                tl_enums::BotApp::NotModified => None,
            }),
        },
        tl_enums::MessageAction::SecureValuesSentMe(a) => ServiceAction::SecureValuesSentMe {
            values_count: a.values.len(),
        },
        tl_enums::MessageAction::SecureValuesSent(a) => ServiceAction::SecureValuesSent {
            types: a.types.iter().map(secure_value_type_name).collect(),
        },
        tl_enums::MessageAction::ContactSignUp => ServiceAction::ContactSignUp,
        tl_enums::MessageAction::GeoProximityReached(a) => ServiceAction::GeoProximityReached {
            from_id: peer_id_from_tl(&a.from_id),
            to_id: peer_id_from_tl(&a.to_id),
            distance: a.distance,
        },
        tl_enums::MessageAction::GroupCall(a) => ServiceAction::GroupCall {
            call_id: group_call_id_and_hash(&a.call).0,
            duration: a.duration,
        },
        tl_enums::MessageAction::InviteToGroupCall(a) => ServiceAction::InviteToGroupCall {
            call_id: group_call_id_and_hash(&a.call).0,
            users: a.users.iter().copied().map(UserId::from_tl).collect(),
        },
        tl_enums::MessageAction::SetMessagesTTL(a) => ServiceAction::SetMessagesTtl {
            period: a.period,
            auto_setting_from: a.auto_setting_from.map(UserId::from_tl),
        },
        tl_enums::MessageAction::GroupCallScheduled(a) => ServiceAction::GroupCallScheduled {
            call_id: group_call_id_and_hash(&a.call).0,
            schedule_date: a.schedule_date,
        },
        tl_enums::MessageAction::SetChatTheme(a) => {
            let theme = match &a.theme {
                tl_enums::ChatTheme::ChatTheme(t) => t.emoticon.clone(),
                tl_enums::ChatTheme::UniqueGift(g) => match &g.gift {
                    tl_enums::StarGift::Unique(u) => u.slug.clone(),
                    tl_enums::StarGift::StarGift(s) => s.title.clone().unwrap_or_default(),
                },
            };
            ServiceAction::SetChatTheme { theme }
        }
        tl_enums::MessageAction::ChatJoinedByRequest => ServiceAction::ChatJoinedByRequest,
        tl_enums::MessageAction::WebViewDataSentMe(a) => ServiceAction::WebViewDataSentMe {
            text: a.text.clone(),
            data: a.data.clone(),
        },
        tl_enums::MessageAction::WebViewDataSent(a) => ServiceAction::WebViewDataSent {
            text: a.text.clone(),
        },
        tl_enums::MessageAction::GiftPremium(a) => ServiceAction::GiftPremium {
            currency: a.currency.clone(),
            amount: a.amount,
            days: a.days,
            crypto_currency: a.crypto_currency.clone(),
            crypto_amount: a.crypto_amount,
            message: a.message.as_ref().map(text_with_entities_from_tl),
        },
        tl_enums::MessageAction::TopicCreate(a) => ServiceAction::TopicCreate {
            title: a.title.clone(),
            icon_color: a.icon_color,
            icon_emoji_id: a.icon_emoji_id.map(DocumentId::from_tl),
            title_missing: a.title_missing,
        },
        tl_enums::MessageAction::TopicEdit(a) => ServiceAction::TopicEdit {
            title: a.title.clone(),
            icon_emoji_id: a.icon_emoji_id.map(DocumentId::from_tl),
            closed: a.closed,
            hidden: a.hidden,
        },
        tl_enums::MessageAction::SuggestProfilePhoto(a) => ServiceAction::SuggestProfilePhoto {
            photo: photo_from_tl(&a.photo).map(Box::new),
        },
        tl_enums::MessageAction::RequestedPeer(a) => ServiceAction::RequestedPeer {
            button_id: a.button_id,
            peers: a.peers.iter().map(peer_id_from_tl).collect(),
        },
        tl_enums::MessageAction::SetChatWallPaper(a) => {
            let wallpaper_id = match &a.wallpaper {
                tl_enums::WallPaper::WallPaper(w) => WallPaperId::from_tl(w.id),
                tl_enums::WallPaper::NoFile(w) => WallPaperId::from_tl(w.id),
            };
            ServiceAction::SetChatWallPaper {
                same: a.same,
                for_both: a.for_both,
                wallpaper_id,
            }
        }
        tl_enums::MessageAction::GiftCode(a) => ServiceAction::GiftCode {
            via_giveaway: a.via_giveaway,
            unclaimed: a.unclaimed,
            boost_peer: a.boost_peer.as_ref().map(peer_id_from_tl),
            days: a.days,
            slug: a.slug.clone(),
            currency: a.currency.clone(),
            amount: a.amount,
            message: a.message.as_ref().map(text_with_entities_from_tl),
        },
        tl_enums::MessageAction::GiveawayLaunch(a) => {
            ServiceAction::GiveawayLaunch { stars: a.stars }
        }
        tl_enums::MessageAction::GiveawayResults(a) => ServiceAction::GiveawayResults {
            stars: a.stars,
            winners_count: a.winners_count,
            unclaimed_count: a.unclaimed_count,
        },
        tl_enums::MessageAction::BoostApply(a) => ServiceAction::BoostApply { boosts: a.boosts },
        tl_enums::MessageAction::RequestedPeerSentMe(a) => ServiceAction::RequestedPeerSentMe {
            button_id: a.button_id,
            peers: a
                .peers
                .iter()
                .map(|rp| match rp {
                    tl_enums::RequestedPeer::User(u) => {
                        PeerId::from_user(UserId::from_tl(u.user_id))
                    }
                    tl_enums::RequestedPeer::Chat(c) => {
                        PeerId::from_chat(ChatId::from_tl(c.chat_id))
                    }
                    tl_enums::RequestedPeer::Channel(ch) => {
                        PeerId::from_channel(ChannelId::from_tl(ch.channel_id))
                    }
                })
                .collect(),
        },
        tl_enums::MessageAction::PaymentRefunded(a) => ServiceAction::PaymentRefunded {
            peer: peer_id_from_tl(&a.peer),
            currency: a.currency.clone(),
            total_amount: a.total_amount,
            payload: a.payload.clone(),
        },
        tl_enums::MessageAction::GiftStars(a) => ServiceAction::GiftStars {
            currency: a.currency.clone(),
            amount: a.amount,
            stars: a.stars,
            crypto_currency: a.crypto_currency.clone(),
            crypto_amount: a.crypto_amount,
            transaction_id: a.transaction_id.clone(),
        },
        tl_enums::MessageAction::PrizeStars(a) => ServiceAction::PrizeStars {
            unclaimed: a.unclaimed,
            stars: a.stars,
            transaction_id: a.transaction_id.clone(),
            boost_peer: peer_id_from_tl(&a.boost_peer),
            giveaway_msg_id: MsgId(i64::from(a.giveaway_msg_id)),
        },
        tl_enums::MessageAction::StarGift(a) => {
            let (gift_id, stars, _) = star_gift_id_and_stars(&a.gift);
            ServiceAction::StarGift {
                gift_id,
                stars,
                convert_stars: a.convert_stars,
                name_hidden: a.name_hidden,
                saved: a.saved,
                converted: a.converted,
                upgraded: a.upgraded,
                refunded: a.refunded,
                message: a.message.as_ref().map(text_with_entities_from_tl),
                from_id: a.from_id.as_ref().map(peer_id_from_tl),
                to_id: a.to_id.as_ref().map(peer_id_from_tl),
            }
        }
        tl_enums::MessageAction::StarGiftUnique(a) => {
            let (gift_id, _, slug) = star_gift_id_and_stars(&a.gift);
            ServiceAction::StarGiftUnique {
                gift_id,
                slug,
                upgrade: a.upgrade,
                transferred: a.transferred,
                saved: a.saved,
                refunded: a.refunded,
                transfer_stars: a.transfer_stars,
                resale_amount: a.resale_amount.as_ref().map(stars_amount_from_tl),
                from_id: a.from_id.as_ref().map(peer_id_from_tl),
                message: a.message.as_ref().map(text_with_entities_from_tl),
            }
        }
        tl_enums::MessageAction::PaidMessagesRefunded(a) => ServiceAction::PaidMessagesRefunded {
            count: a.count,
            stars: a.stars,
        },
        tl_enums::MessageAction::PaidMessagesPrice(a) => ServiceAction::PaidMessagesPrice {
            stars: a.stars,
            broadcast_messages_allowed: a.broadcast_messages_allowed,
        },
        tl_enums::MessageAction::ConferenceCall(a) => ServiceAction::ConferenceCall {
            call_id: CallId::from_tl(a.call_id),
            missed: a.missed,
            active: a.active,
            video: a.video,
            duration: a.duration,
            other_participants: a
                .other_participants
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(peer_id_from_tl)
                .collect(),
        },
        tl_enums::MessageAction::TodoCompletions(a) => ServiceAction::TodoCompletions {
            completed: a.completed.clone(),
            incompleted: a.incompleted.clone(),
        },
        tl_enums::MessageAction::TodoAppendTasks(a) => ServiceAction::TodoAppendTasks {
            list: a.list.iter().map(todo_item_from_tl).collect(),
        },
        tl_enums::MessageAction::SuggestedPostApproval(a) => ServiceAction::SuggestedPostApproval {
            rejected: a.rejected,
            balance_too_low: a.balance_too_low,
            reject_comment: a.reject_comment.clone(),
            schedule_date: a.schedule_date,
            price: a.price.as_ref().map(stars_amount_from_tl),
        },
        tl_enums::MessageAction::SuggestedPostSuccess(a) => ServiceAction::SuggestedPostSuccess {
            price: stars_amount_from_tl(&a.price),
        },
        tl_enums::MessageAction::SuggestedPostRefund(a) => ServiceAction::SuggestedPostRefund {
            payer_initiated: a.payer_initiated,
        },
        tl_enums::MessageAction::GiftTon(a) => ServiceAction::GiftTon {
            currency: a.currency.clone(),
            amount: a.amount,
            crypto_currency: a.crypto_currency.clone(),
            crypto_amount: a.crypto_amount,
            transaction_id: a.transaction_id.clone(),
        },
        tl_enums::MessageAction::SuggestBirthday(a) => {
            let tl_enums::Birthday::Birthday(bd) = &a.birthday;
            ServiceAction::SuggestBirthday {
                birthday: Birthday {
                    day: bd.day.clamp(0, 255) as u8,
                    month: bd.month.clamp(0, 255) as u8,
                    year: bd.year.unwrap_or(0).clamp(0, u16::MAX as i32) as u16,
                },
            }
        }
        tl_enums::MessageAction::StarGiftPurchaseOffer(a) => ServiceAction::StarGiftPurchaseOffer {
            gift_id: star_gift_id_and_stars(&a.gift).0,
            price: stars_amount_from_tl(&a.price),
            expires_at: a.expires_at,
            accepted: a.accepted,
            declined: a.declined,
        },
        tl_enums::MessageAction::StarGiftPurchaseOfferDeclined(a) => {
            ServiceAction::StarGiftPurchaseOfferDeclined {
                gift_id: star_gift_id_and_stars(&a.gift).0,
                price: stars_amount_from_tl(&a.price),
                expired: a.expired,
            }
        }
        tl_enums::MessageAction::NewCreatorPending(a) => ServiceAction::NewCreatorPending {
            new_creator_id: UserId::from_tl(a.new_creator_id),
        },
        tl_enums::MessageAction::ChangeCreator(a) => ServiceAction::ChangeCreator {
            new_creator_id: UserId::from_tl(a.new_creator_id),
        },
        tl_enums::MessageAction::NoForwardsToggle(a) => ServiceAction::NoForwardsToggle {
            prev_value: a.prev_value,
            new_value: a.new_value,
        },
        tl_enums::MessageAction::NoForwardsRequest(a) => ServiceAction::NoForwardsRequest {
            expired: a.expired,
            prev_value: a.prev_value,
            new_value: a.new_value,
        },
        tl_enums::MessageAction::PollAppendAnswer(a) => ServiceAction::PollAppendAnswer {
            answer: poll_answer_from_tl(&a.answer),
        },
        tl_enums::MessageAction::PollDeleteAnswer(a) => ServiceAction::PollDeleteAnswer {
            answer: poll_answer_from_tl(&a.answer),
        },
        tl_enums::MessageAction::ManagedBotCreated(a) => ServiceAction::ManagedBotCreated {
            bot_id: UserId::from_tl(a.bot_id),
        },
        tl_enums::MessageAction::ChangeCommunity(a) => ServiceAction::ChangeCommunity {
            community_id: a.community_id.map(ChannelId::from_tl),
        },
        tl_enums::MessageAction::ChatJoinedViaCommunity(a) => {
            ServiceAction::ChatJoinedViaCommunity {
                community_id: ChannelId::from_tl(a.community_id),
            }
        }
        tl_enums::MessageAction::GramTransfer(a) => ServiceAction::GramTransfer {
            amount: a.amount,
            peer_address: a.peer_address.clone(),
            transaction_id: a.transaction_id.clone(),
            comment: a.comment.clone(),
            comment_encrypted: a.comment_encrypted,
        },
        tl_enums::MessageAction::WalletTonConnectRequest(a) => {
            ServiceAction::WalletTonConnectRequest {
                session_id: a.session_id,
                expires: a.expires,
                accepted: a.accepted,
                declined: a.declined,
                topic: a.topic.clone(),
                trace_id: a.trace_id.clone(),
                dapp_name: a.dapp_name.clone(),
            }
        }
        tl_enums::MessageAction::NoPaidMessagesTokenUsed => ServiceAction::NoPaidMessagesTokenUsed,
    }
}
