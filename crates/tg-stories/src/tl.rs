//! TL schema conversions and RPC request builders for Telegram Stories.
//!
//! Converts between generated `tg_tl` Layer 233 types (`StoryItem`, `PeerStories`,
//! `stories::AllStories`, `stories::Stories`, `stories::StoryViewsList`,
//! `stories::StoryReactionsList`, `stories::StoryViews`, `stories::Albums`,
//! `MediaArea`, `StoriesStealthMode`, `RecentStory`, `Update::Story`,
//! `Update::ReadStories`, `Update::StoriesStealthMode`) and `tg-stories` / `tg-data`
//! domain types.
//!
//! Port of TL parsing/serialization logic in `data/data_story.cpp` and `data/data_stories.cpp`.

use std::collections::{BTreeMap, BTreeSet};

use tg_data::message::PhotoSize;
use tg_data::peer::GeoPoint;
use tg_data::{
    CallId, ChannelId, ChatId, DocumentData, DocumentKind, EntityKind, FormattedDateFlags,
    FullMsgId, MsgId, PeerId, PhotoData, PhotoId, ReactionId, StoryId, TextEntity,
    TextWithEntities, TimeId, UserId,
};
use tg_tl::{enums as tl_enums, functions as tl_fn, types as tl_types};

use crate::story::{
    ChannelPost, RECENT_VIEWERS_MAX, StealthMode, Story, StoryAlbum, StoryArea, StoryIdDates,
    StoryLocation, StoryMedia, StoryPeerContext, StoryPrivacy, StoryRepostInfo, StoryVideoStream,
    StoryView, StoryViews, StoryViewsCounts, SuggestedReaction, UrlArea, WeatherArea,
    strip_story_links,
};

// ---------------------------------------------------------------------------
// Basic TL conversions (Peer, Reaction, Entity, GeoPoint, Photo, Document)
// ---------------------------------------------------------------------------

/// Converts a TL `Peer` enum into a `tg_data::PeerId`.
pub fn peer_id_from_tl(peer: &tl_enums::Peer) -> PeerId {
    match peer {
        tl_enums::Peer::User(u) => PeerId::from_user(UserId::from_tl(u.user_id)),
        tl_enums::Peer::Chat(c) => PeerId::from_chat(ChatId::from_tl(c.chat_id)),
        tl_enums::Peer::Channel(ch) => PeerId::from_channel(ChannelId::from_tl(ch.channel_id)),
    }
}

/// Converts a TL `Reaction` into `Option<ReactionId>` (`None` for `reactionEmpty`).
pub fn reaction_from_tl(reaction: &tl_enums::Reaction) -> Option<ReactionId> {
    match reaction {
        tl_enums::Reaction::Empty => None,
        tl_enums::Reaction::Emoji(e) => {
            if e.emoticon.is_empty() {
                None
            } else {
                Some(ReactionId::Emoji(e.emoticon.clone()))
            }
        }
        tl_enums::Reaction::CustomEmoji(c) => Some(ReactionId::Custom(
            tg_data::DocumentId::from_tl(c.document_id),
        )),
        tl_enums::Reaction::Paid => Some(ReactionId::Paid),
    }
}

/// Converts an `Option<&ReactionId>` back into a TL `Reaction`.
pub fn reaction_to_tl(reaction: Option<&ReactionId>) -> tl_enums::Reaction {
    match reaction {
        None => tl_enums::Reaction::Empty,
        Some(ReactionId::Emoji(e)) if e.is_empty() => tl_enums::Reaction::Empty,
        Some(ReactionId::Emoji(emoticon)) => tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
            emoticon: emoticon.clone(),
        }),
        Some(ReactionId::Custom(doc_id)) => {
            tl_enums::Reaction::CustomEmoji(tl_types::ReactionCustomEmoji {
                document_id: doc_id.to_tl(),
            })
        }
        Some(ReactionId::Paid) => tl_enums::Reaction::Paid,
    }
}

/// Converts a TL `GeoPoint` into `Option<GeoPoint>` (`None` for `geoPointEmpty`).
pub fn geo_point_from_tl(geo: &tl_enums::GeoPoint) -> Option<GeoPoint> {
    match geo {
        tl_enums::GeoPoint::Empty => None,
        tl_enums::GeoPoint::GeoPoint(p) => Some(GeoPoint {
            lat: p.lat,
            lon: p.long,
            accuracy_radius: p.accuracy_radius,
        }),
    }
}

/// Converts a slice of TL `MessageEntity` values into domain `Vec<TextEntity>`.
pub fn entities_from_tl(entities: &[tl_enums::MessageEntity]) -> Vec<TextEntity> {
    entities.iter().map(entity_from_tl).collect()
}

/// Converts a single TL `MessageEntity` into a `TextEntity`.
pub fn entity_from_tl(entity: &tl_enums::MessageEntity) -> TextEntity {
    match entity {
        tl_enums::MessageEntity::Unknown(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::Unknown)
        }
        tl_enums::MessageEntity::Mention(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::Mention)
        }
        tl_enums::MessageEntity::Hashtag(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::Hashtag)
        }
        tl_enums::MessageEntity::BotCommand(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::BotCommand)
        }
        tl_enums::MessageEntity::Url(e) => TextEntity::new(e.offset, e.length, EntityKind::Url),
        tl_enums::MessageEntity::Email(e) => TextEntity::new(e.offset, e.length, EntityKind::Email),
        tl_enums::MessageEntity::Bold(e) => TextEntity::new(e.offset, e.length, EntityKind::Bold),
        tl_enums::MessageEntity::Italic(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::Italic)
        }
        tl_enums::MessageEntity::Code(e) => TextEntity::new(e.offset, e.length, EntityKind::Code),
        tl_enums::MessageEntity::Pre(e) => TextEntity::new(
            e.offset,
            e.length,
            EntityKind::Pre {
                language: e.language.clone(),
            },
        ),
        tl_enums::MessageEntity::TextUrl(e) => TextEntity::new(
            e.offset,
            e.length,
            EntityKind::TextUrl { url: e.url.clone() },
        ),
        tl_enums::MessageEntity::MentionName(e) => TextEntity::new(
            e.offset,
            e.length,
            EntityKind::MentionName {
                user_id: UserId::from_tl(e.user_id),
            },
        ),
        tl_enums::MessageEntity::InputMessageEntityMentionName(e) => {
            let user_id = match &e.user_id {
                tl_enums::InputUser::InputUser(u) => UserId::from_tl(u.user_id),
                tl_enums::InputUser::FromMessage(u) => UserId::from_tl(u.user_id),
                _ => UserId::ZERO,
            };
            TextEntity::new(e.offset, e.length, EntityKind::MentionName { user_id })
        }
        tl_enums::MessageEntity::Phone(e) => TextEntity::new(e.offset, e.length, EntityKind::Phone),
        tl_enums::MessageEntity::Cashtag(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::Cashtag)
        }
        tl_enums::MessageEntity::Underline(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::Underline)
        }
        tl_enums::MessageEntity::Strike(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::StrikeOut)
        }
        tl_enums::MessageEntity::BankCard(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::BankCard)
        }
        tl_enums::MessageEntity::Spoiler(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::Spoiler)
        }
        tl_enums::MessageEntity::CustomEmoji(e) => TextEntity::new(
            e.offset,
            e.length,
            EntityKind::CustomEmoji {
                document_id: tg_data::DocumentId::from_tl(e.document_id),
                force_static: false,
            },
        ),
        tl_enums::MessageEntity::Blockquote(e) => TextEntity::new(
            e.offset,
            e.length,
            EntityKind::Blockquote {
                collapsed: e.collapsed,
            },
        ),
        tl_enums::MessageEntity::FormattedDate(e) => {
            let mut flags = FormattedDateFlags::empty();
            if e.relative {
                flags |= FormattedDateFlags::RELATIVE;
            }
            if e.short_time {
                flags |= FormattedDateFlags::SHORT_TIME;
            }
            if e.long_time {
                flags |= FormattedDateFlags::LONG_TIME;
            }
            if e.short_date {
                flags |= FormattedDateFlags::SHORT_DATE;
            }
            if e.long_date {
                flags |= FormattedDateFlags::LONG_DATE;
            }
            if e.day_of_week {
                flags |= FormattedDateFlags::DAY_OF_WEEK;
            }
            TextEntity::new(
                e.offset,
                e.length,
                EntityKind::FormattedDate {
                    date: e.date,
                    flags,
                },
            )
        }
        tl_enums::MessageEntity::DiffInsert(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::DiffInsert)
        }
        tl_enums::MessageEntity::DiffReplace(e) => TextEntity::new(
            e.offset,
            e.length,
            EntityKind::DiffReplace {
                old_text: e.old_text.clone(),
            },
        ),
        tl_enums::MessageEntity::DiffDelete(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::DiffDelete)
        }
        tl_enums::MessageEntity::TonAddress(e) => {
            TextEntity::new(e.offset, e.length, EntityKind::TonAddress)
        }
    }
}

/// Converts a TL `Photo` into `Option<PhotoData>` (`None` for `photoEmpty`).
pub fn photo_from_tl(photo: &tl_enums::Photo) -> Option<PhotoData> {
    match photo {
        tl_enums::Photo::Empty(_) => None,
        tl_enums::Photo::Photo(p) => {
            let sizes = p
                .sizes
                .iter()
                .filter_map(|s| match s {
                    tl_enums::PhotoSize::Empty(_) => None,
                    tl_enums::PhotoSize::PhotoSize(sz) => Some(PhotoSize {
                        kind: sz.r#type.clone(),
                        width: sz.w,
                        height: sz.h,
                        size: i64::from(sz.size),
                    }),
                    tl_enums::PhotoSize::PhotoCachedSize(sz) => Some(PhotoSize {
                        kind: sz.r#type.clone(),
                        width: sz.w,
                        height: sz.h,
                        size: sz.bytes.len() as i64,
                    }),
                    tl_enums::PhotoSize::PhotoStrippedSize(sz) => Some(PhotoSize {
                        kind: sz.r#type.clone(),
                        width: 0,
                        height: 0,
                        size: sz.bytes.len() as i64,
                    }),
                    tl_enums::PhotoSize::Progressive(sz) => Some(PhotoSize {
                        kind: sz.r#type.clone(),
                        width: sz.w,
                        height: sz.h,
                        size: i64::from(sz.sizes.last().copied().unwrap_or(0)),
                    }),
                    tl_enums::PhotoSize::PhotoPathSize(sz) => Some(PhotoSize {
                        kind: sz.r#type.clone(),
                        width: 0,
                        height: 0,
                        size: sz.bytes.len() as i64,
                    }),
                })
                .collect();
            Some(PhotoData {
                id: PhotoId::from_tl(p.id),
                access_hash: p.access_hash as u64,
                file_reference: p.file_reference.clone(),
                date: p.date,
                dc_id: p.dc_id,
                sizes,
                has_stickers: p.has_stickers,
                spoiler: false,
                ttl_seconds: None,
                live_photo_video_id: None,
                ..PhotoData::default()
            })
        }
    }
}

/// Converts a TL `Document` into `Option<(DocumentData, bool /* silent_video */)>`.
pub fn document_from_tl(doc: &tl_enums::Document) -> Option<(DocumentData, bool)> {
    match doc {
        tl_enums::Document::Empty(_) => None,
        tl_enums::Document::Document(d) => {
            let mut kind = DocumentKind::File;
            let mut file_name = String::new();
            let mut duration = None;
            let mut width = None;
            let mut height = None;
            let mut audio_title = None;
            let mut audio_performer = None;
            let mut waveform = None;
            let mut supports_streaming = false;
            let mut silent_video = false;
            let mut is_animated = false;

            for attr in &d.attributes {
                match attr {
                    tl_enums::DocumentAttribute::Video(v) => {
                        width = Some(v.w);
                        height = Some(v.h);
                        duration = Some(v.duration);
                        supports_streaming = v.supports_streaming;
                        silent_video = v.nosound;
                        if v.round_message {
                            kind = DocumentKind::RoundVideo;
                        } else if !is_animated {
                            kind = DocumentKind::Video;
                        }
                    }
                    tl_enums::DocumentAttribute::Animated => {
                        is_animated = true;
                        kind = DocumentKind::Gif;
                        silent_video = true;
                    }
                    tl_enums::DocumentAttribute::Filename(f) => {
                        file_name = f.file_name.clone();
                    }
                    tl_enums::DocumentAttribute::Audio(a) => {
                        duration = Some(f64::from(a.duration));
                        audio_title = a.title.clone();
                        audio_performer = a.performer.clone();
                        waveform = a.waveform.clone();
                        kind = if a.voice {
                            DocumentKind::Voice
                        } else {
                            DocumentKind::Audio
                        };
                    }
                    tl_enums::DocumentAttribute::ImageSize(sz) => {
                        width = Some(sz.w);
                        height = Some(sz.h);
                    }
                    tl_enums::DocumentAttribute::Sticker(_) => {
                        kind = DocumentKind::Sticker;
                    }
                    _ => {}
                }
            }

            if is_animated && matches!(kind, DocumentKind::Video | DocumentKind::File) {
                kind = DocumentKind::Gif;
            }

            Some((
                DocumentData {
                    id: tg_data::DocumentId::from_tl(d.id),
                    access_hash: d.access_hash as u64,
                    file_reference: d.file_reference.clone(),
                    date: d.date,
                    mime_type: d.mime_type.clone(),
                    size: d.size,
                    dc_id: d.dc_id,
                    kind,
                    file_name,
                    duration,
                    width,
                    height,
                    audio_title,
                    audio_performer,
                    waveform,
                    sticker: None,
                    supports_streaming,
                    spoiler: false,
                    ttl_seconds: None,
                    video_timestamp: None,
                    ..DocumentData::default()
                },
                silent_video,
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Story Media & Areas conversions
// ---------------------------------------------------------------------------

/// Parses a TL `MessageMedia` into a `StoryMedia` (`ParseMedia` in `data_stories.cpp`).
///
/// Returns `None` if the media type is invalid/unusable for a story (e.g. empty photo,
/// non-video document, or unrelated message media). Returns `Some(StoryMedia::Unsupported)`
/// for `messageMediaUnsupported`.
pub fn story_media_from_tl(
    peer_id: PeerId,
    media: &tl_enums::MessageMedia,
    existing_stream: Option<&StoryVideoStream>,
) -> Option<StoryMedia> {
    match media {
        tl_enums::MessageMedia::Photo(data) => {
            let mut photo = data.photo.as_ref().and_then(photo_from_tl)?;
            photo.spoiler = data.spoiler;
            photo.ttl_seconds = data.ttl_seconds;
            if let Some(vid) = &data.video
                && let Some((doc, _)) = document_from_tl(vid)
            {
                photo.live_photo_video_id = Some(doc.id);
            }
            Some(StoryMedia::Photo(photo))
        }
        tl_enums::MessageMedia::Document(data) => {
            let (mut doc, silent) = data.document.as_ref().and_then(document_from_tl)?;
            doc.spoiler = data.spoiler;
            doc.ttl_seconds = data.ttl_seconds;
            doc.video_timestamp = data.video_timestamp;
            // Matches C++: result->isGifv() || result->isVideoFile()
            let is_video_or_gif = matches!(doc.kind, DocumentKind::Video | DocumentKind::Gif)
                || data.video
                || doc.mime_type.starts_with("video/");
            if is_video_or_gif {
                Some(StoryMedia::Video {
                    document: doc,
                    silent,
                })
            } else {
                None
            }
        }
        tl_enums::MessageMedia::VideoStream(data) => match &data.call {
            tl_enums::InputGroupCall::InputGroupCall(call) => {
                let call_id = CallId::from_tl(call.id);
                let stream = match existing_stream {
                    Some(prev) if prev.peer_id == peer_id && prev.call_id == call_id => {
                        let mut cloned = prev.clone();
                        cloned.access_hash = call.access_hash;
                        cloned.rtmp_stream = data.rtmp_stream;
                        cloned
                    }
                    _ => StoryVideoStream {
                        peer_id,
                        call_id,
                        access_hash: call.access_hash,
                        rtmp_stream: data.rtmp_stream,
                        ..StoryVideoStream::default()
                    },
                };
                Some(StoryMedia::VideoStream(stream))
            }
            _ => Some(StoryMedia::Unsupported),
        },
        tl_enums::MessageMedia::Unsupported => Some(StoryMedia::Unsupported),
        _ => None,
    }
}

/// Converts a TL `MediaAreaCoordinates` into a `StoryArea` (`Data::ParseArea`).
pub fn story_area_from_tl(coords: &tl_enums::MediaAreaCoordinates) -> StoryArea {
    match coords {
        tl_enums::MediaAreaCoordinates::MediaAreaCoordinates(c) => {
            StoryArea::from_tl_coords(c.x, c.y, c.w, c.h, c.rotation, c.radius)
        }
    }
}

/// Converts a `StoryArea` into a TL `MediaAreaCoordinates` (`Data::SerializeArea`).
pub fn story_area_to_tl(area: StoryArea) -> tl_enums::MediaAreaCoordinates {
    let (x, y, w, h, rotation, radius) = area.to_tl_coords();
    tl_enums::MediaAreaCoordinates::MediaAreaCoordinates(tl_types::MediaAreaCoordinates {
        x,
        y,
        w,
        h,
        rotation,
        radius,
    })
}

/// Parsed collection of all interactive media areas on a story.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedMediaAreas {
    /// Location and venue areas.
    pub locations: Vec<StoryLocation>,
    /// Suggested reaction areas (with counts populated from `reactions_counts`).
    pub suggested_reactions: Vec<SuggestedReaction>,
    /// Channel post repost areas.
    pub channel_posts: Vec<ChannelPost>,
    /// URL and Star Gift (`tg://nft?slug=...`) areas.
    pub url_areas: Vec<UrlArea>,
    /// Weather areas.
    pub weather_areas: Vec<WeatherArea>,
}

/// Parses a slice of TL `MediaArea` into typed area lists (`data_story.cpp:91-225, 845-864`).
pub fn parse_media_areas(
    areas: &[tl_enums::MediaArea],
    reactions_counts: &BTreeMap<ReactionId, i32>,
) -> ParsedMediaAreas {
    let mut out = ParsedMediaAreas::default();
    for area in areas {
        match area {
            tl_enums::MediaArea::Venue(v) => {
                if let Some(point) = geo_point_from_tl(&v.geo) {
                    out.locations.push(StoryLocation {
                        area: story_area_from_tl(&v.coordinates),
                        point,
                        title: v.title.clone(),
                        address: v.address.clone(),
                        provider: v.provider.clone(),
                        venue_id: v.venue_id.clone(),
                        venue_type: v.venue_type.clone(),
                        ..StoryLocation::default()
                    });
                }
            }
            tl_enums::MediaArea::GeoPoint(g) => {
                if let Some(point) = geo_point_from_tl(&g.geo) {
                    let (country_iso2, state, city, street) = match &g.address {
                        Some(tl_enums::GeoPointAddress::GeoPointAddress(addr)) => (
                            addr.country_iso2.clone(),
                            addr.state.clone().unwrap_or_default(),
                            addr.city.clone().unwrap_or_default(),
                            addr.street.clone().unwrap_or_default(),
                        ),
                        None => (String::new(), String::new(), String::new(), String::new()),
                    };
                    out.locations.push(StoryLocation {
                        area: story_area_from_tl(&g.coordinates),
                        point,
                        country_iso2,
                        state,
                        city,
                        street,
                        ..StoryLocation::default()
                    });
                }
            }
            tl_enums::MediaArea::SuggestedReaction(r) => {
                if let Some(reaction) = reaction_from_tl(&r.reaction) {
                    let count = reactions_counts.get(&reaction).copied().unwrap_or(0);
                    out.suggested_reactions.push(SuggestedReaction {
                        area: story_area_from_tl(&r.coordinates),
                        reaction,
                        count,
                        flipped: r.flipped,
                        dark: r.dark,
                    });
                }
            }
            tl_enums::MediaArea::ChannelPost(cp) => {
                out.channel_posts.push(ChannelPost {
                    area: story_area_from_tl(&cp.coordinates),
                    item_id: FullMsgId::new(
                        PeerId::from_channel(ChannelId::from_tl(cp.channel_id)),
                        MsgId::from(cp.msg_id),
                    ),
                });
            }
            tl_enums::MediaArea::Url(u) => {
                out.url_areas.push(UrlArea {
                    area: story_area_from_tl(&u.coordinates),
                    url: u.url.clone(),
                });
            }
            tl_enums::MediaArea::StarGift(g) => {
                out.url_areas.push(UrlArea {
                    area: story_area_from_tl(&g.coordinates),
                    url: format!("tg://nft?slug={}", g.slug),
                });
            }
            tl_enums::MediaArea::Weather(w) => {
                let millicelsius = (1000.0 * w.temperature_c.clamp(-274.0, 1_000_000.0)) as i32;
                out.weather_areas.push(WeatherArea {
                    area: story_area_from_tl(&w.coordinates),
                    emoji: w.emoji.clone(),
                    color: w.color as u32,
                    millicelsius,
                });
            }
            // Input variants are unexpected from server; silently ignore (C++ logs API error).
            tl_enums::MediaArea::InputMediaAreaVenue(_)
            | tl_enums::MediaArea::InputMediaAreaChannelPost(_) => {}
        }
    }
    out
}

/// Parses TL `PrivacyRule` vector into `(allowed_peers, disallowed_peers)` (`ParsePrivacyPeers`).
///
/// Chat participant IDs are encoded as `PeerId::from_chat`; callers with a peer registry can
/// re-map migrated channels if needed via `parse_privacy_rules_with`.
pub fn parse_privacy_rules(rules: &[tl_enums::PrivacyRule]) -> (Vec<PeerId>, Vec<PeerId>) {
    parse_privacy_rules_with(rules, |chat_id| {
        Some(PeerId::from_chat(ChatId::from_tl(chat_id)))
    })
}

/// Parses TL `PrivacyRule` vector with a custom resolver for chat/channel bare IDs.
pub fn parse_privacy_rules_with(
    rules: &[tl_enums::PrivacyRule],
    mut resolve_chat_or_channel: impl FnMut(i64) -> Option<PeerId>,
) -> (Vec<PeerId>, Vec<PeerId>) {
    let mut allowed = Vec::new();
    let mut disallowed = Vec::new();
    for rule in rules {
        match rule {
            tl_enums::PrivacyRule::PrivacyValueAllowUsers(r) => {
                allowed.extend(
                    r.users
                        .iter()
                        .map(|&uid| PeerId::from_user(UserId::from_tl(uid))),
                );
            }
            tl_enums::PrivacyRule::PrivacyValueAllowChatParticipants(r) => {
                for &cid in &r.chats {
                    if let Some(pid) = resolve_chat_or_channel(cid) {
                        allowed.push(pid);
                    }
                }
            }
            tl_enums::PrivacyRule::PrivacyValueDisallowUsers(r) => {
                disallowed.extend(
                    r.users
                        .iter()
                        .map(|&uid| PeerId::from_user(UserId::from_tl(uid))),
                );
            }
            tl_enums::PrivacyRule::PrivacyValueDisallowChatParticipants(r) => {
                for &cid in &r.chats {
                    if let Some(pid) = resolve_chat_or_channel(cid) {
                        disallowed.push(pid);
                    }
                }
            }
            _ => {}
        }
    }
    (allowed, disallowed)
}

// ---------------------------------------------------------------------------
// StoryViews, StoryItem, PeerStories, AllStories, StealthMode, Albums, Updates
// ---------------------------------------------------------------------------

/// Parses TL `StoryViews` counts (`Story::parseViewsCounts` in `data_story.cpp`).
pub fn story_views_counts_from_tl(
    views: &tl_enums::StoryViews,
    mine: Option<&ReactionId>,
    is_channel: bool,
) -> StoryViewsCounts {
    let tl_enums::StoryViews::StoryViews(data) = views;
    let mut result = StoryViewsCounts {
        views: data.views_count,
        forwards: data.forwards_count.unwrap_or(0),
        reactions: data.reactions_count.unwrap_or(0),
        reactions_counts: BTreeMap::new(),
        recent_viewers: Vec::new(),
    };
    if let Some(list) = &data.recent_viewers {
        result.recent_viewers = list
            .iter()
            .take(RECENT_VIEWERS_MAX)
            .map(|&uid| PeerId::from_user(UserId::from_tl(uid)))
            .collect();
    }
    let mut total = 0i32;
    if is_channel && let Some(list) = &data.reactions {
        for rc in list {
            let tl_enums::ReactionCount::ReactionCount(rc_data) = rc;
            if let Some(id) = reaction_from_tl(&rc_data.reaction) {
                result.reactions_counts.insert(id, rc_data.count);
                total = total.saturating_add(rc_data.count);
            }
        }
    }
    if let Some(my_id) = mine
        && !my_id.is_empty()
    {
        let entry = result.reactions_counts.entry(my_id.clone()).or_insert(0);
        if *entry == 0 {
            *entry = 1;
            total = total.saturating_add(1);
        }
    }
    if result.reactions < total {
        result.reactions = total;
    }
    result
}

/// Outcome of parsing a TL `StoryItem` (`storyItem` / `storyItemSkipped` / `storyItemDeleted`).
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedStoryItem {
    /// Full `storyItem` parsed into a domain `Story`.
    Full(Box<Story>),
    /// Compact `storyItemSkipped` containing ID, creation date, expiry date, and live flag.
    Skipped(StoryIdDates),
    /// Tombstone `storyItemDeleted`.
    Deleted(StoryId),
}

impl ParsedStoryItem {
    /// Returns the `StoryId` carried by any variant.
    #[inline]
    pub const fn story_id(&self) -> StoryId {
        match self {
            Self::Full(s) => s.id,
            Self::Skipped(d) => d.id,
            Self::Deleted(id) => *id,
        }
    }
}

/// Parses a TL `StoryItem` into `Option<ParsedStoryItem>` (`Stories::parseAndApply` + `Story::applyFields`).
///
/// Returns `None` if a `storyItem` has unsupported/unusable media (caller should treat as deleted,
/// matching `data_stories.cpp:659-663`).
pub fn story_item_from_tl(
    peer_id: PeerId,
    peer_ctx: StoryPeerContext,
    item: &tl_enums::StoryItem,
    now: TimeId,
    existing: Option<&Story>,
) -> Option<ParsedStoryItem> {
    match item {
        tl_enums::StoryItem::Deleted(d) => Some(ParsedStoryItem::Deleted(d.id)),
        tl_enums::StoryItem::Skipped(s) => Some(ParsedStoryItem::Skipped(StoryIdDates {
            id: s.id,
            date: s.date,
            expires: s.expire_date,
            video_stream: s.live,
        })),
        tl_enums::StoryItem::StoryItem(data) => {
            let existing_stream = existing.and_then(|s| s.media.as_video_stream());
            let media = story_media_from_tl(peer_id, &data.media, existing_stream)?;

            let sent_reaction = if data.min {
                existing.and_then(|s| s.sent_reaction.clone())
            } else {
                data.sent_reaction.as_ref().and_then(reaction_from_tl)
            };

            let privacy = if data.public {
                StoryPrivacy::Public
            } else if data.close_friends {
                StoryPrivacy::CloseFriends
            } else if data.contacts {
                StoryPrivacy::Contacts
            } else if data.selected_contacts {
                StoryPrivacy::SelectedContacts
            } else {
                StoryPrivacy::Other
            };

            let out = if data.min {
                existing.is_some_and(|s| s.out)
            } else {
                data.out
            };

            let (privacy_allowed, privacy_disallowed) = if let Some(rules) = &data.privacy {
                parse_privacy_rules(rules)
            } else if data.min {
                existing
                    .map(|s| (s.privacy_allowed.clone(), s.privacy_disallowed.clone()))
                    .unwrap_or_default()
            } else {
                (Vec::new(), Vec::new())
            };

            let raw_caption = TextWithEntities {
                text: data.caption.clone().unwrap_or_default(),
                entities: data
                    .entities
                    .as_deref()
                    .map(entities_from_tl)
                    .unwrap_or_default(),
            };
            let caption = if peer_ctx.is_user && !peer_ctx.is_verified && !peer_ctx.is_premium {
                strip_story_links(raw_caption)
            } else {
                raw_caption
            };

            let (counts, views_known) = if let Some(v) = &data.views {
                (
                    story_views_counts_from_tl(v, sent_reaction.as_ref(), peer_ctx.is_channel),
                    true,
                )
            } else if let Some(prev) = existing {
                let mut map = BTreeMap::new();
                for sr in &prev.suggested_reactions {
                    if sr.count != 0 {
                        map.insert(sr.reaction.clone(), sr.count);
                    }
                }
                (
                    // Preserve `prev.views.views` (not `prev.views.total`) so `total = views + forwards`
                    // below does not double-count `forwards` on repeated `min` updates
                    // (`Story::applyFields` in `data/data_story.cpp:814-839`).
                    StoryViewsCounts {
                        views: prev.views.views,
                        forwards: prev.views.forwards,
                        reactions: prev.views.reactions,
                        reactions_counts: map,
                        recent_viewers: prev.recent_viewers.clone(),
                    },
                    prev.views.known,
                )
            } else {
                (StoryViewsCounts::default(), false)
            };

            let areas = data
                .media_areas
                .as_deref()
                .map(|a| parse_media_areas(a, &counts.reactions_counts))
                .unwrap_or_default();

            let repost = match &data.fwd_from {
                Some(tl_enums::StoryFwdHeader::StoryFwdHeader(fwd)) => StoryRepostInfo {
                    source_peer: fwd.from.as_ref().map(peer_id_from_tl),
                    source_name: fwd.from_name.clone().unwrap_or_default(),
                    source_id: fwd.story_id.unwrap_or(0),
                    modified: fwd.modified,
                },
                None => StoryRepostInfo::default(),
            };

            let from_peer = data.from_id.as_ref().map(peer_id_from_tl);
            let album_ids: BTreeSet<i32> = data
                .albums
                .as_deref()
                .map(|ids| ids.iter().copied().collect())
                .unwrap_or_default();
            let music = data
                .music
                .as_ref()
                .and_then(document_from_tl)
                .map(|(doc, _)| doc);

            let total_interactions = counts.views.saturating_add(counts.forwards);
            let views = StoryViews {
                list: Vec::new(),
                next_offset: String::new(),
                reactions: counts.reactions,
                forwards: counts.forwards,
                views: counts.views,
                total: total_interactions,
                known: views_known,
            };
            let channel_reactions = StoryViews {
                total: counts.reactions.saturating_add(counts.forwards),
                ..StoryViews::default()
            };

            Some(ParsedStoryItem::Full(Box::new(Story {
                id: data.id,
                peer_id,
                date: data.date,
                expires: data.expire_date,
                last_update_time: now,
                media,
                caption,
                out,
                in_profile: data.pinned,
                pinned_to_top: existing.is_some_and(|s| s.pinned_to_top),
                privacy,
                privacy_allowed,
                privacy_disallowed,
                no_forwards: data.noforwards,
                edited: data.edited,
                from_peer,
                repost,
                music,
                album_ids,
                sent_reaction,
                recent_viewers: counts.recent_viewers,
                views,
                channel_reactions,
                locations: areas.locations,
                suggested_reactions: areas.suggested_reactions,
                channel_posts: areas.channel_posts,
                url_areas: areas.url_areas,
                weather_areas: areas.weather_areas,
            })))
        }
    }
}

/// Converts TL `StoriesStealthMode` into `StealthMode`.
pub fn stealth_mode_from_tl(mode: &tl_enums::StoriesStealthMode) -> StealthMode {
    let tl_enums::StoriesStealthMode::StoriesStealthMode(data) = mode;
    StealthMode {
        enabled_till: data.active_until_date.unwrap_or(0),
        cooldown_till: data.cooldown_until_date.unwrap_or(0),
    }
}

/// Parsed `RecentStory` returned by `stories.getPeerMaxIDs` or `User`/`Channel` `stories_max_id`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct RecentStoryInfo {
    /// Highest active story ID (`0` if none).
    pub max_id: StoryId,
    /// Whether the peer currently has a live video stream story (`live`).
    pub live: bool,
}

/// Converts TL `RecentStory` into `RecentStoryInfo`.
pub fn recent_story_from_tl(recent: &tl_enums::RecentStory) -> RecentStoryInfo {
    let tl_enums::RecentStory::RecentStory(data) = recent;
    RecentStoryInfo {
        max_id: data.max_id.unwrap_or(0),
        live: data.live,
    }
}

/// Converts TL `StoryAlbum` into domain `StoryAlbum`.
pub fn story_album_from_tl(album: &tl_enums::StoryAlbum) -> StoryAlbum {
    let tl_enums::StoryAlbum::StoryAlbum(data) = album;
    StoryAlbum {
        id: data.album_id,
        title: data.title.clone(),
        icon_photo: data.icon_photo.as_ref().and_then(photo_from_tl),
        icon_video: data
            .icon_video
            .as_ref()
            .and_then(document_from_tl)
            .map(|(doc, _)| doc),
    }
}

/// Parsed `PeerStories` container.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedPeerStories {
    /// Owner peer ID.
    pub peer_id: PeerId,
    /// Server-reported `max_read_id` (`0` if omitted).
    pub max_read_id: StoryId,
    /// Raw story items (resolved or skipped/deleted).
    pub items: Vec<ParsedStoryItem>,
    /// Whether any `storyItem` in this peer's list contains a live video stream.
    pub has_video_stream: bool,
}

/// Parses TL `PeerStories` using `peer_ctx_lookup` to supply per-peer context.
pub fn peer_stories_from_tl(
    peer_stories: &tl_enums::PeerStories,
    now: TimeId,
    peer_ctx_lookup: impl FnMut(PeerId) -> StoryPeerContext,
) -> ParsedPeerStories {
    peer_stories_from_tl_with_existing(peer_stories, now, peer_ctx_lookup, |_, _| None)
}

/// Parses TL `PeerStories` while preserving fields omitted on `min` `StoryItem` payloads
/// by consulting `existing_lookup` (`Stories::parseAndApply` in `data/data_stories.cpp:557-586`
/// and `Story::applyFields` in `data/data_story.cpp:787-839`).
pub fn peer_stories_from_tl_with_existing<'a>(
    peer_stories: &tl_enums::PeerStories,
    now: TimeId,
    mut peer_ctx_lookup: impl FnMut(PeerId) -> StoryPeerContext,
    mut existing_lookup: impl FnMut(PeerId, StoryId) -> Option<&'a Story>,
) -> ParsedPeerStories {
    let tl_enums::PeerStories::PeerStories(data) = peer_stories;
    let peer_id = peer_id_from_tl(&data.peer);
    let ctx = peer_ctx_lookup(peer_id);
    let mut items = Vec::with_capacity(data.stories.len());
    let mut has_video_stream = false;
    for raw in &data.stories {
        if let tl_enums::StoryItem::StoryItem(si) = raw
            && matches!(*si.media, tl_enums::MessageMedia::VideoStream(_))
        {
            has_video_stream = true;
        }
        if let tl_enums::StoryItem::Skipped(sk) = raw
            && sk.live
        {
            has_video_stream = true;
        }
        let existing = match raw {
            tl_enums::StoryItem::StoryItem(si) => existing_lookup(peer_id, si.id),
            _ => None,
        };
        if let Some(parsed) = story_item_from_tl(peer_id, ctx, raw, now, existing) {
            items.push(parsed);
        } else if let tl_enums::StoryItem::StoryItem(si) = raw {
            items.push(ParsedStoryItem::Deleted(si.id));
        }
    }
    ParsedPeerStories {
        peer_id,
        max_read_id: data.max_read_id.unwrap_or(0),
        items,
        has_video_stream,
    }
}

/// Parsed result of `stories.getAllStories`.
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedAllStories {
    /// `stories.allStoriesNotModified`.
    NotModified {
        /// Pagination/sync state token.
        state: String,
        /// Updated stealth mode state.
        stealth_mode: StealthMode,
    },
    /// `stories.allStories`.
    All {
        /// Whether more pages remain (`has_more`).
        has_more: bool,
        /// Total peer sources count reported by server.
        count: i32,
        /// State token for next page / delta refresh.
        state: String,
        /// Per-peer story bundles.
        peer_stories: Vec<ParsedPeerStories>,
        /// Updated stealth mode state.
        stealth_mode: StealthMode,
    },
}

/// Parses TL `stories.AllStories`.
pub fn all_stories_from_tl(
    all: &tl_enums::stories::AllStories,
    now: TimeId,
    peer_ctx_lookup: impl FnMut(PeerId) -> StoryPeerContext,
) -> ParsedAllStories {
    all_stories_from_tl_with_existing(all, now, peer_ctx_lookup, |_, _| None)
}

/// Parses TL `stories.AllStories` while preserving existing story fields for `min` items
/// (`Stories::parseAndApply` in `data/data_stories.cpp:557-586`).
pub fn all_stories_from_tl_with_existing<'a>(
    all: &tl_enums::stories::AllStories,
    now: TimeId,
    mut peer_ctx_lookup: impl FnMut(PeerId) -> StoryPeerContext,
    mut existing_lookup: impl FnMut(PeerId, StoryId) -> Option<&'a Story>,
) -> ParsedAllStories {
    match all {
        tl_enums::stories::AllStories::NotModified(nm) => ParsedAllStories::NotModified {
            state: nm.state.clone(),
            stealth_mode: stealth_mode_from_tl(&nm.stealth_mode),
        },
        tl_enums::stories::AllStories::AllStories(data) => {
            let peer_stories = data
                .peer_stories
                .iter()
                .map(|ps| {
                    peer_stories_from_tl_with_existing(
                        ps,
                        now,
                        &mut peer_ctx_lookup,
                        &mut existing_lookup,
                    )
                })
                .collect();
            ParsedAllStories::All {
                has_more: data.has_more,
                count: data.count,
                state: data.state.clone(),
                peer_stories,
                stealth_mode: stealth_mode_from_tl(&data.stealth_mode),
            }
        }
    }
}

/// Parsed result of `stories.getPinnedStories` / `getStoriesArchive` / `getAlbumStories` / `getStoriesByID`.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedStoriesPage {
    /// Total count reported by server.
    pub count: i32,
    /// Parsed story items.
    pub items: Vec<ParsedStoryItem>,
    /// Story IDs pinned to top of profile (for `getPinnedStories`).
    pub pinned_to_top: Vec<StoryId>,
}

/// Parses TL `stories.Stories`.
pub fn stories_page_from_tl(
    peer_id: PeerId,
    peer_ctx: StoryPeerContext,
    stories: &tl_enums::stories::Stories,
    now: TimeId,
) -> ParsedStoriesPage {
    stories_page_from_tl_with_existing(peer_id, peer_ctx, stories, now, |_, _| None)
}

/// Parses TL `stories.Stories` while preserving existing story fields for `min` items
/// (`Stories::parseAndApply` in `data/data_stories.cpp:557-586`).
pub fn stories_page_from_tl_with_existing<'a>(
    peer_id: PeerId,
    peer_ctx: StoryPeerContext,
    stories: &tl_enums::stories::Stories,
    now: TimeId,
    mut existing_lookup: impl FnMut(PeerId, StoryId) -> Option<&'a Story>,
) -> ParsedStoriesPage {
    let tl_enums::stories::Stories::Stories(data) = stories;
    let mut items = Vec::with_capacity(data.stories.len());
    for raw in &data.stories {
        let existing = match raw {
            tl_enums::StoryItem::StoryItem(si) => existing_lookup(peer_id, si.id),
            _ => None,
        };
        if let Some(parsed) = story_item_from_tl(peer_id, peer_ctx, raw, now, existing) {
            items.push(parsed);
        } else if let tl_enums::StoryItem::StoryItem(si) = raw {
            items.push(ParsedStoryItem::Deleted(si.id));
        }
    }
    ParsedStoriesPage {
        count: data.count,
        items,
        pinned_to_top: data.pinned_to_top.clone().unwrap_or_default(),
    }
}

/// Parsed slice of `stories.StoryViewsList` or `stories.StoryReactionsList`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedViewsSlice {
    /// Paginated `StoryViews` slice ready to pass to `Story::apply_views_slice` /
    /// `Story::apply_channel_reactions_slice`.
    pub slice: StoryViews,
    /// Any embedded public repost stories encountered in the slice (peer_id + parsed item).
    pub repost_stories: Vec<(PeerId, ParsedStoryItem)>,
}

fn message_peer_and_id(msg: &tl_enums::Message) -> Option<(PeerId, MsgId, TimeId)> {
    match msg {
        tl_enums::Message::Message(m) => {
            Some((peer_id_from_tl(&m.peer_id), MsgId::from(m.id), m.date))
        }
        tl_enums::Message::Service(m) => {
            Some((peer_id_from_tl(&m.peer_id), MsgId::from(m.id), m.date))
        }
        tl_enums::Message::Empty(_) => None,
    }
}

/// Parses TL `stories.StoryViewsList` (`Stories::sendViewsSliceRequest`).
pub fn story_views_list_from_tl(
    list: &tl_enums::stories::StoryViewsList,
    now: TimeId,
    mut peer_ctx_lookup: impl FnMut(PeerId) -> StoryPeerContext,
) -> ParsedViewsSlice {
    let tl_enums::stories::StoryViewsList::StoryViewsList(data) = list;
    let mut slice = StoryViews {
        list: Vec::with_capacity(data.views.len()),
        next_offset: data.next_offset.clone().unwrap_or_default(),
        reactions: data.reactions_count,
        forwards: data.forwards_count,
        views: data.views_count,
        total: data.count,
        known: true,
    };
    let mut repost_stories = Vec::new();
    for view in &data.views {
        match view {
            tl_enums::StoryView::StoryView(v) => {
                slice.list.push(StoryView {
                    peer_id: PeerId::from_user(UserId::from_tl(v.user_id)),
                    reaction: v.reaction.as_ref().and_then(reaction_from_tl),
                    repost_id: 0,
                    forward_id: MsgId::ZERO,
                    date: v.date,
                    blocked: v.blocked,
                    blocked_my_stories_from: v.blocked_my_stories_from,
                });
            }
            tl_enums::StoryView::PublicRepost(r) => {
                let repost_peer = peer_id_from_tl(&r.peer_id);
                let ctx = peer_ctx_lookup(repost_peer);
                if let Some(parsed) = story_item_from_tl(repost_peer, ctx, &r.story, now, None) {
                    let repost_id = parsed.story_id();
                    let date = match &parsed {
                        ParsedStoryItem::Full(s) => s.date,
                        ParsedStoryItem::Skipped(d) => d.date,
                        ParsedStoryItem::Deleted(_) => 0,
                    };
                    repost_stories.push((repost_peer, parsed));
                    if repost_id != 0 {
                        slice.list.push(StoryView {
                            peer_id: repost_peer,
                            reaction: None,
                            repost_id,
                            forward_id: MsgId::ZERO,
                            date,
                            blocked: r.blocked,
                            blocked_my_stories_from: r.blocked_my_stories_from,
                        });
                    }
                }
            }
            tl_enums::StoryView::PublicForward(f) => {
                if let Some((fwd_peer, fwd_id, date)) = message_peer_and_id(&f.message) {
                    slice.list.push(StoryView {
                        peer_id: fwd_peer,
                        reaction: None,
                        repost_id: 0,
                        forward_id: fwd_id,
                        date,
                        blocked: f.blocked,
                        blocked_my_stories_from: f.blocked_my_stories_from,
                    });
                }
            }
        }
    }
    ParsedViewsSlice {
        slice,
        repost_stories,
    }
}

/// Parses TL `stories.StoryReactionsList` (`Stories::loadReactionsSlice`).
pub fn story_reactions_list_from_tl(
    list: &tl_enums::stories::StoryReactionsList,
    now: TimeId,
    mut peer_ctx_lookup: impl FnMut(PeerId) -> StoryPeerContext,
) -> ParsedViewsSlice {
    let tl_enums::stories::StoryReactionsList::StoryReactionsList(data) = list;
    let mut slice = StoryViews {
        list: Vec::with_capacity(data.reactions.len()),
        next_offset: data.next_offset.clone().unwrap_or_default(),
        reactions: data.count,
        forwards: 0,
        views: 0,
        total: data.count,
        known: true,
    };
    let mut repost_stories = Vec::new();
    for item in &data.reactions {
        match item {
            tl_enums::StoryReaction::StoryReaction(r) => {
                slice.list.push(StoryView {
                    peer_id: peer_id_from_tl(&r.peer_id),
                    reaction: reaction_from_tl(&r.reaction),
                    repost_id: 0,
                    forward_id: MsgId::ZERO,
                    date: r.date,
                    blocked: false,
                    blocked_my_stories_from: false,
                });
            }
            tl_enums::StoryReaction::PublicRepost(r) => {
                let repost_peer = peer_id_from_tl(&r.peer_id);
                let ctx = peer_ctx_lookup(repost_peer);
                if let Some(parsed) = story_item_from_tl(repost_peer, ctx, &r.story, now, None) {
                    let repost_id = parsed.story_id();
                    let date = match &parsed {
                        ParsedStoryItem::Full(s) => s.date,
                        ParsedStoryItem::Skipped(d) => d.date,
                        ParsedStoryItem::Deleted(_) => 0,
                    };
                    repost_stories.push((repost_peer, parsed));
                    if repost_id != 0 {
                        slice.list.push(StoryView {
                            peer_id: repost_peer,
                            reaction: None,
                            repost_id,
                            forward_id: MsgId::ZERO,
                            date,
                            blocked: false,
                            blocked_my_stories_from: false,
                        });
                    }
                }
            }
            tl_enums::StoryReaction::PublicForward(f) => {
                if let Some((fwd_peer, fwd_id, date)) = message_peer_and_id(&f.message) {
                    slice.list.push(StoryView {
                        peer_id: fwd_peer,
                        reaction: None,
                        repost_id: 0,
                        forward_id: fwd_id,
                        date,
                        blocked: false,
                        blocked_my_stories_from: false,
                    });
                }
            }
        }
    }
    ParsedViewsSlice {
        slice,
        repost_stories,
    }
}

/// Parsed story-related Telegram `Update` variants.
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedStoryUpdate {
    /// `updateStory`: a peer posted, edited, or deleted a story.
    Story {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Parsed story item.
        item: ParsedStoryItem,
    },
    /// `updateReadStories`: stories up to `max_id` were marked read for `peer_id`.
    ReadStories {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Maximum read story ID.
        max_id: StoryId,
    },
    /// `updateStoriesStealthMode`: stealth mode active/cooldown timestamps updated.
    StealthMode(StealthMode),
    /// `updateStoryID`: server assigned `id` for an outgoing story with `random_id`.
    StoryId {
        /// Assigned server story ID.
        id: StoryId,
        /// Client random ID.
        random_id: i64,
    },
}

/// Extracts a story-related update from a TL `Update` (`updateStory`, `updateReadStories`,
/// `updateStoriesStealthMode`, `updateStoryID`).
pub fn story_update_from_tl(
    update: &tl_enums::Update,
    now: TimeId,
    peer_ctx_lookup: impl FnMut(PeerId) -> StoryPeerContext,
) -> Option<ParsedStoryUpdate> {
    story_update_from_tl_with_existing(update, now, peer_ctx_lookup, |_, _| None)
}

/// Extracts a story-related update while preserving fields omitted on `min` `StoryItem`
/// updates (`Stories::apply(MTPDupdateStory)` in `data/data_stories.cpp:588-619`).
pub fn story_update_from_tl_with_existing<'a>(
    update: &tl_enums::Update,
    now: TimeId,
    mut peer_ctx_lookup: impl FnMut(PeerId) -> StoryPeerContext,
    mut existing_lookup: impl FnMut(PeerId, StoryId) -> Option<&'a Story>,
) -> Option<ParsedStoryUpdate> {
    match update {
        tl_enums::Update::Story(u) => {
            let peer_id = peer_id_from_tl(&u.peer);
            let ctx = peer_ctx_lookup(peer_id);
            let existing = match &u.story {
                tl_enums::StoryItem::StoryItem(si) => existing_lookup(peer_id, si.id),
                _ => None,
            };
            let item = match story_item_from_tl(peer_id, ctx, &u.story, now, existing) {
                Some(parsed) => parsed,
                None => match &u.story {
                    tl_enums::StoryItem::StoryItem(si) => ParsedStoryItem::Deleted(si.id),
                    tl_enums::StoryItem::Skipped(sk) => ParsedStoryItem::Deleted(sk.id),
                    tl_enums::StoryItem::Deleted(d) => ParsedStoryItem::Deleted(d.id),
                },
            };
            Some(ParsedStoryUpdate::Story { peer_id, item })
        }
        tl_enums::Update::ReadStories(u) => Some(ParsedStoryUpdate::ReadStories {
            peer_id: peer_id_from_tl(&u.peer),
            max_id: u.max_id,
        }),
        tl_enums::Update::StoriesStealthMode(u) => Some(ParsedStoryUpdate::StealthMode(
            stealth_mode_from_tl(&u.stealth_mode),
        )),
        tl_enums::Update::StoryID(u) => Some(ParsedStoryUpdate::StoryId {
            id: u.id,
            random_id: u.random_id,
        }),
        _ => None,
    }
}

/// Parsed `stories.Albums` response.
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedAlbums {
    /// `stories.albumsNotModified`.
    NotModified,
    /// `stories.albums`.
    Albums {
        /// Pagination/cache hash.
        hash: i64,
        /// Parsed albums.
        albums: Vec<StoryAlbum>,
    },
}

/// Parses TL `stories.Albums`.
pub fn albums_from_tl(albums: &tl_enums::stories::Albums) -> ParsedAlbums {
    match albums {
        tl_enums::stories::Albums::NotModified => ParsedAlbums::NotModified,
        tl_enums::stories::Albums::Albums(data) => ParsedAlbums::Albums {
            hash: data.hash,
            albums: data.albums.iter().map(story_album_from_tl).collect(),
        },
    }
}

// ---------------------------------------------------------------------------
// Outbound TL Request Builders
// ---------------------------------------------------------------------------

/// Builds `stories.getAllStories` (`Stories::loadMore` in `data_stories.cpp`).
pub fn build_get_all_stories(hidden: bool, state: Option<&str>) -> tl_fn::stories::GetAllStories {
    let has_state = state.is_some_and(|s| !s.is_empty());
    tl_fn::stories::GetAllStories {
        next: has_state,
        hidden,
        state: has_state.then(|| state.unwrap_or_default().to_string()),
    }
}

/// Builds `stories.getPeerStories`.
pub fn build_get_peer_stories(peer: tl_enums::InputPeer) -> tl_fn::stories::GetPeerStories {
    tl_fn::stories::GetPeerStories { peer }
}

/// Builds `stories.getStoriesByID`.
pub fn build_get_stories_by_id(
    peer: tl_enums::InputPeer,
    ids: Vec<StoryId>,
) -> tl_fn::stories::GetStoriesByID {
    tl_fn::stories::GetStoriesByID { peer, id: ids }
}

/// Builds `stories.readStories`.
pub fn build_read_stories(
    peer: tl_enums::InputPeer,
    max_id: StoryId,
) -> tl_fn::stories::ReadStories {
    tl_fn::stories::ReadStories { peer, max_id }
}

/// Builds `stories.incrementStoryViews`.
pub fn build_increment_story_views(
    peer: tl_enums::InputPeer,
    ids: Vec<StoryId>,
) -> tl_fn::stories::IncrementStoryViews {
    tl_fn::stories::IncrementStoryViews { peer, id: ids }
}

/// Builds `stories.getPeerMaxIDs`.
pub fn build_get_peer_max_ids(peers: Vec<tl_enums::InputPeer>) -> tl_fn::stories::GetPeerMaxIDs {
    tl_fn::stories::GetPeerMaxIDs { id: peers }
}

/// Builds `stories.getAllReadPeerStories`.
pub fn build_get_all_read_peer_stories() -> tl_fn::stories::GetAllReadPeerStories {
    tl_fn::stories::GetAllReadPeerStories
}

/// Builds `stories.activateStealthMode` with both `past` and `future` enabled
/// (matching `Stories::activateStealthMode` in `data_stories.cpp:1107`).
pub fn build_activate_stealth_mode(
    past: bool,
    future: bool,
) -> tl_fn::stories::ActivateStealthMode {
    tl_fn::stories::ActivateStealthMode { past, future }
}

/// Builds `stories.sendReaction`.
pub fn build_send_reaction(
    peer: tl_enums::InputPeer,
    story_id: StoryId,
    reaction: Option<&ReactionId>,
    add_to_recent: bool,
) -> tl_fn::stories::SendReaction {
    tl_fn::stories::SendReaction {
        add_to_recent,
        peer,
        story_id,
        reaction: reaction_to_tl(reaction),
    }
}

/// Builds `stories.deleteStories`.
pub fn build_delete_stories(
    peer: tl_enums::InputPeer,
    ids: Vec<StoryId>,
) -> tl_fn::stories::DeleteStories {
    tl_fn::stories::DeleteStories { peer, id: ids }
}

/// Builds `stories.togglePinned` (save to profile / archive).
pub fn build_toggle_pinned(
    peer: tl_enums::InputPeer,
    ids: Vec<StoryId>,
    pinned: bool,
) -> tl_fn::stories::TogglePinned {
    tl_fn::stories::TogglePinned {
        peer,
        id: ids,
        pinned,
    }
}

/// Builds `stories.togglePinnedToTop` (pin stories to top of profile grid).
pub fn build_toggle_pinned_to_top(
    peer: tl_enums::InputPeer,
    ids: Vec<StoryId>,
) -> tl_fn::stories::TogglePinnedToTop {
    tl_fn::stories::TogglePinnedToTop { peer, id: ids }
}

/// Builds `stories.togglePeerStoriesHidden`.
pub fn build_toggle_peer_stories_hidden(
    peer: tl_enums::InputPeer,
    hidden: bool,
) -> tl_fn::stories::TogglePeerStoriesHidden {
    tl_fn::stories::TogglePeerStoriesHidden { peer, hidden }
}

/// Builds `stories.getStoryViewsList` (`reactions_first: true` matching C++).
pub fn build_get_story_views_list(
    peer: tl_enums::InputPeer,
    id: StoryId,
    offset: String,
    limit: i32,
) -> tl_fn::stories::GetStoryViewsList {
    tl_fn::stories::GetStoryViewsList {
        just_contacts: false,
        reactions_first: true,
        forwards_first: false,
        peer,
        q: None,
        id,
        offset,
        limit,
    }
}

/// Builds `stories.getStoryReactionsList`.
pub fn build_get_story_reactions_list(
    peer: tl_enums::InputPeer,
    id: StoryId,
    offset: Option<String>,
    limit: i32,
) -> tl_fn::stories::GetStoryReactionsList {
    let offset = offset.filter(|s| !s.is_empty());
    tl_fn::stories::GetStoryReactionsList {
        forwards_first: false,
        peer,
        id,
        reaction: None,
        offset,
        limit,
    }
}

/// Builds `stories.getStoriesViews`.
pub fn build_get_stories_views(
    peer: tl_enums::InputPeer,
    ids: Vec<StoryId>,
) -> tl_fn::stories::GetStoriesViews {
    tl_fn::stories::GetStoriesViews { peer, id: ids }
}

/// Builds `stories.getPinnedStories`.
pub fn build_get_pinned_stories(
    peer: tl_enums::InputPeer,
    offset_id: StoryId,
    limit: i32,
) -> tl_fn::stories::GetPinnedStories {
    tl_fn::stories::GetPinnedStories {
        peer,
        offset_id,
        limit,
    }
}

/// Builds `stories.getStoriesArchive`.
pub fn build_get_stories_archive(
    peer: tl_enums::InputPeer,
    offset_id: StoryId,
    limit: i32,
) -> tl_fn::stories::GetStoriesArchive {
    tl_fn::stories::GetStoriesArchive {
        peer,
        offset_id,
        limit,
    }
}

/// Builds `stories.getAlbums`.
pub fn build_get_albums(peer: tl_enums::InputPeer, hash: i64) -> tl_fn::stories::GetAlbums {
    tl_fn::stories::GetAlbums { peer, hash }
}

/// Builds `stories.getAlbumStories`.
pub fn build_get_album_stories(
    peer: tl_enums::InputPeer,
    album_id: i32,
    offset: i32,
    limit: i32,
) -> tl_fn::stories::GetAlbumStories {
    tl_fn::stories::GetAlbumStories {
        peer,
        album_id,
        offset,
        limit,
    }
}

/// Builds `stories.createAlbum`.
pub fn build_create_album(
    peer: tl_enums::InputPeer,
    title: String,
    stories: Vec<StoryId>,
) -> tl_fn::stories::CreateAlbum {
    tl_fn::stories::CreateAlbum {
        peer,
        title,
        stories,
    }
}

/// Builds `stories.updateAlbum` to rename an album.
pub fn build_rename_album(
    peer: tl_enums::InputPeer,
    album_id: i32,
    title: String,
) -> tl_fn::stories::UpdateAlbum {
    tl_fn::stories::UpdateAlbum {
        peer,
        album_id,
        title: Some(title),
        delete_stories: None,
        add_stories: None,
        order: None,
    }
}

/// Builds `stories.updateAlbum` to reorder stories in an album.
pub fn build_reorder_album(
    peer: tl_enums::InputPeer,
    album_id: i32,
    order: Vec<StoryId>,
) -> tl_fn::stories::UpdateAlbum {
    tl_fn::stories::UpdateAlbum {
        peer,
        album_id,
        title: None,
        delete_stories: None,
        add_stories: None,
        order: Some(order),
    }
}

/// Builds `stories.deleteAlbum`.
pub fn build_delete_album(peer: tl_enums::InputPeer, album_id: i32) -> tl_fn::stories::DeleteAlbum {
    tl_fn::stories::DeleteAlbum { peer, album_id }
}

/// Builds `stories.exportStoryLink`.
pub fn build_export_story_link(
    peer: tl_enums::InputPeer,
    id: StoryId,
) -> tl_fn::stories::ExportStoryLink {
    tl_fn::stories::ExportStoryLink { peer, id }
}

/// Builds `stories.report`.
pub fn build_report_story(
    peer: tl_enums::InputPeer,
    ids: Vec<StoryId>,
    option: Vec<u8>,
    message: String,
) -> tl_fn::stories::Report {
    tl_fn::stories::Report {
        peer,
        id: ids,
        option,
        message,
    }
}

/// Filter options for `stories.getStoryViewsList` (`Stories::sendViewsSliceRequest`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoryViewsListFilter {
    /// Return only views from contacts (`just_contacts` flag).
    pub just_contacts: bool,
    /// Sort viewers with reactions first (`reactions_first` flag).
    pub reactions_first: bool,
    /// Sort public forwards/reposts first (`forwards_first` flag).
    pub forwards_first: bool,
    /// Optional search query (`q` flag). Empty string is omitted on the wire.
    pub query: Option<String>,
}

/// Builds `stories.getStoryViewsList` with full Layer 233 filter flags
/// (`just_contacts`, `reactions_first`, `forwards_first`, `q`).
pub fn build_get_story_views_list_filtered(
    peer: tl_enums::InputPeer,
    id: StoryId,
    filter: StoryViewsListFilter,
    offset: String,
    limit: i32,
) -> tl_fn::stories::GetStoryViewsList {
    let q = filter.query.filter(|s| !s.is_empty());
    tl_fn::stories::GetStoryViewsList {
        just_contacts: filter.just_contacts,
        reactions_first: filter.reactions_first,
        forwards_first: filter.forwards_first,
        peer,
        q,
        id,
        offset,
        limit,
    }
}

/// Converts a domain `GeoPoint` into a TL `GeoPoint` (`Data::LocationPoint::toMTP`).
pub fn geo_point_to_tl(point: GeoPoint) -> tl_enums::GeoPoint {
    tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
        long: point.lon,
        lat: point.lat,
        access_hash: 0,
        accuracy_radius: point.accuracy_radius,
    })
}

/// Base story privacy visibility option for `stories.sendStory` / `stories.editStory`
/// (`UserPrivacy::Option` in `api/api_stories.cpp:252-317`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StoryPrivacyOption {
    /// Visible to everyone (`inputPrivacyValueAllowAll`).
    Everyone,
    /// Visible to contacts (`inputPrivacyValueAllowContacts`).
    Contacts,
    /// Visible only to close friends (`inputPrivacyValueAllowCloseFriends`).
    CloseFriends,
    /// Visible only to explicitly allowed users/chats (selected contacts).
    Nobody,
}

/// Story privacy configuration matching `StoryPrivacyToInput` (`api/api_stories.cpp:252-317`).
#[derive(Clone, Debug, PartialEq)]
pub struct StoryPrivacyRuleInput {
    /// Base audience option.
    pub option: StoryPrivacyOption,
    /// Explicitly allowed users (used when `option == StoryPrivacyOption::Nobody`).
    pub always_users: Vec<tl_enums::InputUser>,
    /// Explicitly allowed bare chat/channel IDs (used when `option == StoryPrivacyOption::Nobody`).
    pub always_chats: Vec<i64>,
    /// Explicitly disallowed users (used when `option` is `Everyone` or `Contacts`).
    pub never_users: Vec<tl_enums::InputUser>,
    /// Explicitly disallowed bare chat/channel IDs (used when `option` is `Everyone` or `Contacts`).
    pub never_chats: Vec<i64>,
}

/// Serializes a [`StoryPrivacyRuleInput`] into TL `Vector<InputPrivacyRule>`, matching
/// `StoryPrivacyToInput` in `api/api_stories.cpp:252-317`.
///
/// Notable tdesktop rules preserved verbatim:
/// - `CloseFriends` emits only `inputPrivacyValueAllowCloseFriends` (no `never`/`always` rules).
/// - `Nobody` with empty `always_users` and `always_chats` falls back to allowing `self_user`
///   so Telegram accepts a "nobody else" private story.
pub fn story_privacy_to_tl(
    rule: &StoryPrivacyRuleInput,
    self_user: tl_enums::InputUser,
) -> Vec<tl_enums::InputPrivacyRule> {
    let mut result = Vec::new();
    match rule.option {
        StoryPrivacyOption::Everyone => {
            result.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll);
        }
        StoryPrivacyOption::Contacts => {
            result.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowContacts);
        }
        StoryPrivacyOption::CloseFriends => {
            result.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowCloseFriends);
        }
        StoryPrivacyOption::Nobody => {}
    }
    if rule.option == StoryPrivacyOption::Nobody {
        let mut users = rule.always_users.clone();
        if users.is_empty() && rule.always_chats.is_empty() {
            users.push(self_user);
        }
        if !users.is_empty() {
            result.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowUsers(
                tl_types::InputPrivacyValueAllowUsers { users },
            ));
        }
        if !rule.always_chats.is_empty() {
            result.push(
                tl_enums::InputPrivacyRule::InputPrivacyValueAllowChatParticipants(
                    tl_types::InputPrivacyValueAllowChatParticipants {
                        chats: rule.always_chats.clone(),
                    },
                ),
            );
        }
    } else if rule.option != StoryPrivacyOption::CloseFriends {
        if !rule.never_users.is_empty() {
            result.push(tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(
                tl_types::InputPrivacyValueDisallowUsers {
                    users: rule.never_users.clone(),
                },
            ));
        }
        if !rule.never_chats.is_empty() {
            result.push(
                tl_enums::InputPrivacyRule::InputPrivacyValueDisallowChatParticipants(
                    tl_types::InputPrivacyValueDisallowChatParticipants {
                        chats: rule.never_chats.clone(),
                    },
                ),
            );
        }
    }
    result
}

/// Interactive media areas attached to a story being sent or edited (`MediaAreasToMTP`
/// in `api/api_stories.cpp:189-231`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoryMediaAreasInput {
    /// Suggested reaction areas.
    pub reactions: Vec<SuggestedReaction>,
    /// URL areas.
    pub urls: Vec<UrlArea>,
    /// Location / venue areas.
    pub locations: Vec<StoryLocation>,
    /// Weather areas.
    pub weather: Vec<WeatherArea>,
    /// Channel post repost areas paired with resolved `InputChannel`.
    pub channel_posts: Vec<(ChannelPost, tl_enums::InputChannel)>,
}

/// Converts [`StoryMediaAreasInput`] into TL `Vec<MediaArea>`, preserving exact order and
/// flag semantics of `MediaAreasToMTP` / `LocationToMTP` in `api/api_stories.cpp:145-231`.
pub fn media_areas_to_tl(input: &StoryMediaAreasInput) -> Vec<tl_enums::MediaArea> {
    let mut result = Vec::with_capacity(
        input.reactions.len()
            + input.urls.len()
            + input.locations.len()
            + input.weather.len()
            + input.channel_posts.len(),
    );
    for reaction in &input.reactions {
        result.push(tl_enums::MediaArea::SuggestedReaction(
            tl_types::MediaAreaSuggestedReaction {
                dark: reaction.dark,
                flipped: reaction.flipped,
                coordinates: story_area_to_tl(reaction.area),
                reaction: reaction_to_tl(Some(&reaction.reaction)),
            },
        ));
    }
    for url in &input.urls {
        result.push(tl_enums::MediaArea::Url(tl_types::MediaAreaUrl {
            coordinates: story_area_to_tl(url.area),
            url: url.url.clone(),
        }));
    }
    for loc in &input.locations {
        if !loc.venue_id.is_empty() {
            result.push(tl_enums::MediaArea::Venue(tl_types::MediaAreaVenue {
                coordinates: story_area_to_tl(loc.area),
                geo: geo_point_to_tl(loc.point),
                title: loc.title.clone(),
                address: loc.address.clone(),
                provider: loc.provider.clone(),
                venue_id: loc.venue_id.clone(),
                venue_type: loc.venue_type.clone(),
            }));
        } else {
            let has_address = !loc.country_iso2.is_empty() || !loc.city.is_empty();
            let address = has_address.then(|| {
                tl_enums::GeoPointAddress::GeoPointAddress(tl_types::GeoPointAddress {
                    country_iso2: loc.country_iso2.clone(),
                    state: (!loc.state.is_empty()).then(|| loc.state.clone()),
                    city: (!loc.city.is_empty()).then(|| loc.city.clone()),
                    street: (!loc.street.is_empty()).then(|| loc.street.clone()),
                })
            });
            result.push(tl_enums::MediaArea::GeoPoint(tl_types::MediaAreaGeoPoint {
                coordinates: story_area_to_tl(loc.area),
                geo: geo_point_to_tl(loc.point),
                address,
            }));
        }
    }
    for weather in &input.weather {
        result.push(tl_enums::MediaArea::Weather(tl_types::MediaAreaWeather {
            coordinates: story_area_to_tl(weather.area),
            emoji: weather.emoji.clone(),
            temperature_c: f64::from(weather.millicelsius) / 1000.0,
            color: weather.color as i32,
        }));
    }
    for (post, channel) in &input.channel_posts {
        result.push(tl_enums::MediaArea::InputMediaAreaChannelPost(
            tl_types::InputMediaAreaChannelPost {
                coordinates: story_area_to_tl(post.area),
                channel: channel.clone(),
                msg_id: post.item_id.msg.0 as i32,
            },
        ));
    }
    result
}

/// Repost source metadata for `stories.sendStory` (`SendStoryRepost` in `api/api_stories.cpp`).
#[derive(Clone, Debug, PartialEq)]
pub struct StoryRepostInput {
    /// Peer owning the reposted story.
    pub from_peer: tl_enums::InputPeer,
    /// Story ID on `from_peer`.
    pub from_story_id: StoryId,
    /// Whether the media was modified relative to the original story (`fwd_modified`).
    pub modified: bool,
}

/// Parameters for building `stories.sendStory` (`Api::Stories::send` in `api/api_stories.cpp:1169-1198`).
#[derive(Clone, Debug, PartialEq)]
pub struct SendStoryInput {
    /// Target peer (self or channel).
    pub peer: tl_enums::InputPeer,
    /// Story photo or video media.
    pub media: tl_enums::InputMedia,
    /// Interactive media areas (omitted on wire when empty).
    pub media_areas: Vec<tl_enums::MediaArea>,
    /// Optional caption text (empty string omitted on wire).
    pub caption: String,
    /// Optional caption entities (empty vec omitted on wire).
    pub entities: Vec<tl_enums::MessageEntity>,
    /// Audience privacy rules.
    pub privacy_rules: Vec<tl_enums::InputPrivacyRule>,
    /// Client-generated deduplication random ID.
    pub random_id: i64,
    /// Story lifetime in seconds (e.g., 21600, 43200, 86400, 172800). Always set on wire when `Some`.
    pub period: Option<i32>,
    /// Save story to profile after expiry (`pinned`).
    pub pinned: bool,
    /// Disallow forwarding/screenshotting (`noforwards`).
    pub noforwards: bool,
    /// Optional repost origin (`fwd_from_id`, `fwd_from_story`, `fwd_modified`).
    pub repost: Option<StoryRepostInput>,
    /// Custom album IDs to attach this story to (empty vec omitted on wire).
    pub albums: Vec<i32>,
    /// Optional background music track document.
    pub music: Option<tl_enums::InputDocument>,
}

/// Builds `stories.sendStory` matching `Api::Stories::send` (`api/api_stories.cpp:1169-1198`).
pub fn build_send_story(input: SendStoryInput) -> tl_fn::stories::SendStory {
    let (fwd_modified, fwd_from_id, fwd_from_story) = match input.repost {
        Some(r) => (r.modified, Some(r.from_peer), Some(r.from_story_id)),
        None => (false, None, None),
    };
    tl_fn::stories::SendStory {
        pinned: input.pinned,
        noforwards: input.noforwards,
        fwd_modified,
        peer: input.peer,
        media: input.media,
        media_areas: (!input.media_areas.is_empty()).then_some(input.media_areas),
        caption: (!input.caption.is_empty()).then_some(input.caption),
        entities: (!input.entities.is_empty()).then_some(input.entities),
        privacy_rules: input.privacy_rules,
        random_id: input.random_id,
        period: input.period,
        fwd_from_id,
        fwd_from_story,
        albums: (!input.albums.is_empty()).then_some(input.albums),
        music: input.music,
    }
}

/// Parameters for building `stories.editStory` (`Api::Stories::send` in `api/api_stories.cpp:1148-1163`).
#[derive(Clone, Debug, PartialEq)]
pub struct EditStoryInput {
    /// Target peer (self or channel).
    pub peer: tl_enums::InputPeer,
    /// Existing story ID to edit.
    pub id: StoryId,
    /// Replacement media (`None` keeps existing media).
    pub media: Option<tl_enums::InputMedia>,
    /// Replacement media areas (`None` keeps existing; if `media` is `Some` and `media_areas` is `None`,
    /// an empty vec is sent matching `api/api_stories.cpp:1150`).
    pub media_areas: Option<Vec<tl_enums::MediaArea>>,
    /// Replacement `(caption, entities)`. In Layer 233 `caption` and `entities` share `flags.1`,
    /// so both are sent together when `Some` and omitted together when `None`.
    pub caption: Option<(String, Vec<tl_enums::MessageEntity>)>,
    /// Replacement privacy rules (`None` keeps existing).
    pub privacy_rules: Option<Vec<tl_enums::InputPrivacyRule>>,
    /// Replacement background music (`None` keeps existing).
    pub music: Option<tl_enums::InputDocument>,
}

/// Builds `stories.editStory` matching `Api::Stories::send` (`api/api_stories.cpp:1148-1163`).
pub fn build_edit_story(input: EditStoryInput) -> tl_fn::stories::EditStory {
    let media_areas = match (input.media.is_some(), input.media_areas) {
        (_, Some(areas)) => Some(areas),
        (true, None) => Some(Vec::new()),
        (false, None) => None,
    };
    let (caption, entities) = match input.caption {
        Some((text, ents)) => (Some(text), Some(ents)),
        None => (None, None),
    };
    tl_fn::stories::EditStory {
        peer: input.peer,
        id: input.id,
        media: input.media,
        media_areas,
        caption,
        entities,
        privacy_rules: input.privacy_rules,
        music: input.music,
    }
}

/// Builds `stories.updateAlbum` to add and/or remove stories from a custom album.
pub fn build_update_album_stories(
    peer: tl_enums::InputPeer,
    album_id: i32,
    add_stories: Vec<StoryId>,
    delete_stories: Vec<StoryId>,
) -> tl_fn::stories::UpdateAlbum {
    tl_fn::stories::UpdateAlbum {
        peer,
        album_id,
        title: None,
        delete_stories: (!delete_stories.is_empty()).then_some(delete_stories),
        add_stories: (!add_stories.is_empty()).then_some(add_stories),
        order: None,
    }
}

/// Builds `stories.reorderAlbums` to reorder a peer's custom story albums.
pub fn build_reorder_albums(
    peer: tl_enums::InputPeer,
    order: Vec<i32>,
) -> tl_fn::stories::ReorderAlbums {
    tl_fn::stories::ReorderAlbums { peer, order }
}

/// Builds `stories.canSendStory`.
pub fn build_can_send_story(peer: tl_enums::InputPeer) -> tl_fn::stories::CanSendStory {
    tl_fn::stories::CanSendStory { peer }
}

/// Builds `stories.getChatsToSend`.
pub fn build_get_chats_to_send() -> tl_fn::stories::GetChatsToSend {
    tl_fn::stories::GetChatsToSend
}
