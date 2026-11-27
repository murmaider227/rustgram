//! Message, entity, reply, forward, and short-update conversions.
//!
//! Port origin: `data/data_session.cpp` (`processMessage`), `history/history_item.cpp`,
//! `api/api_text_entities.cpp`, `api/api_updates.cpp` (`DataIsLoaded`).

use tg_data::{
    BusinessShortcutId, ButtonColor, ButtonVisual, ChatId, DocumentId, EffectId, EntityKind,
    FormattedDateFlags, ForwardInfo, FullMsgId, FullStoryId, InlineQueryPeerTypes, KeyboardButton,
    KeyboardButtonKind, Message, MessageContent, MessageFlags, MessageMedia, MsgId, PeerId,
    PollRequestRestriction, ReplyInfo, ReplyMarkup, RequestPeerQuery, RequestPeerRestriction,
    RequestPeerType, Session, TextEntity, TextWithEntities, UserId,
};
use tg_tl::{enums as tl_enums, types as tl_types};

use crate::convert::media::{message_action_from_tl, message_media_from_tl, reactions_from_tl};
use crate::convert::peers::{
    admin_rights_from_tl, input_peer_for, input_user_for, peer_id_from_tl,
};

/// Converts a slice of TL `MessageEntity` items into `tg_data::TextEntity` values
/// (all 25 Layer 233 constructors), skipping `internal:` URLs per
/// `Api::EntitiesFromMTP` (`api/api_text_entities.cpp:146-148`).
#[must_use]
pub fn entities_from_tl(entities: &[tl_enums::MessageEntity]) -> Vec<TextEntity> {
    entities_from_tl_with_self(entities, UserId::ZERO)
}

/// Converts TL `MessageEntity` items into `tg_data::TextEntity` values, resolving
/// `InputUser::Self_` in `InputMessageEntityMentionName` to `self_id`
/// (`api/api_text_entities.cpp:176-182`, `Api::EntitiesFromMTP`).
///
/// Server bounds are checked here, once: entities with a negative offset or a non-positive
/// length are dropped and the rest end at `i32::MAX` at most, so `offset + length` never
/// overflows. Prefer [`entities_from_tl_for_text`] when the text is at hand.
#[must_use]
pub fn entities_from_tl_with_self(
    entities: &[tl_enums::MessageEntity],
    self_id: UserId,
) -> Vec<TextEntity> {
    entities_within(entities, self_id, i32::MAX)
}

/// [`entities_from_tl_with_self`] with every entity also cut to the UTF-16 length of `text`,
/// as tdesktop's consumers do when they `mid()` the text: afterwards
/// `0 <= offset < offset + length <= text length` holds for each entity, in server order.
#[must_use]
pub fn entities_from_tl_for_text(
    text: &str,
    entities: &[tl_enums::MessageEntity],
    self_id: UserId,
) -> Vec<TextEntity> {
    let text_len = i32::try_from(tg_text::utf16_len(text)).unwrap_or(i32::MAX);
    entities_within(entities, self_id, text_len)
}

fn entities_within(
    entities: &[tl_enums::MessageEntity],
    self_id: UserId,
    text_utf16_len: i32,
) -> Vec<TextEntity> {
    entities
        .iter()
        .filter_map(|e| entity_from_tl(e, self_id)?.clamped_to_text(text_utf16_len))
        .collect()
}

fn is_internal_url(url: &str) -> bool {
    url.len() >= 9 && url.as_bytes()[..9].eq_ignore_ascii_case(b"internal:")
}

fn entity_from_tl(entity: &tl_enums::MessageEntity, self_id: UserId) -> Option<TextEntity> {
    Some(match entity {
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
        tl_enums::MessageEntity::TextUrl(e) => {
            // Skip `internal:` URLs on receive (`api/api_text_entities.cpp:146-148`).
            if is_internal_url(&e.url) {
                return None;
            }
            TextEntity::new(
                e.offset,
                e.length,
                EntityKind::TextUrl { url: e.url.clone() },
            )
        }
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
                tl_enums::InputUser::Self_ => self_id,
                tl_enums::InputUser::Empty => UserId::ZERO,
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
                document_id: DocumentId::from_tl(e.document_id),
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
            flags.set(FormattedDateFlags::RELATIVE, e.relative);
            flags.set(FormattedDateFlags::SHORT_TIME, e.short_time);
            flags.set(FormattedDateFlags::LONG_TIME, e.long_time);
            flags.set(FormattedDateFlags::SHORT_DATE, e.short_date);
            flags.set(FormattedDateFlags::LONG_DATE, e.long_date);
            flags.set(FormattedDateFlags::DAY_OF_WEEK, e.day_of_week);
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
    })
}

/// Canonical nesting priority for sorting outgoing entities in the order Telegram expects
/// (outer block containers first, then inline styles, then leaf entities).
fn entity_sort_priority(kind: &EntityKind) -> u8 {
    match kind {
        EntityKind::Blockquote { .. } => 0,
        EntityKind::Pre { .. } => 1,
        EntityKind::TextUrl { .. } | EntityKind::MentionName { .. } => 2,
        EntityKind::Bold => 3,
        EntityKind::Italic => 4,
        EntityKind::Underline => 5,
        EntityKind::StrikeOut => 6,
        EntityKind::Spoiler => 7,
        EntityKind::Code => 8,
        EntityKind::CustomEmoji { .. } => 9,
        EntityKind::FormattedDate { .. } => 10,
        _ => 11,
    }
}

/// Converts domain `TextEntity` items to outgoing TL `MessageEntity` items using `session`
/// to resolve `InputUser` access hashes for `MentionName` entities (`ConvertOption::WithLocal`,
/// `api/api_text_entities.cpp:292-436`, `Api::EntitiesToMTP`).
#[must_use]
pub fn entities_to_tl(entities: &[TextEntity], session: &Session) -> Vec<tl_enums::MessageEntity> {
    entities_to_tl_inner(entities, session, false)
}

/// Converts domain `TextEntity` items for sending (`ConvertOption::SkipLocal`,
/// `api/api_text_entities.cpp:300-315`, `Api::EntitiesToMTP`): omits server-parsed local
/// entities (`Url`, `Email`, `Phone`, `BankCard`, `TonAddress`, `Hashtag`, `Cashtag`,
/// `Mention`, `BotCommand`), drops invalid zero-length / zero-ID / `internal:` entities,
/// and sorts entities canonically (`offset` ascending, `length` descending, outer-before-inner).
#[must_use]
pub fn entities_to_tl_for_sending(
    entities: &[TextEntity],
    session: &Session,
) -> Vec<tl_enums::MessageEntity> {
    let mut sorted: Vec<&TextEntity> = entities.iter().collect();
    sorted.sort_by(|a, b| {
        a.offset
            .cmp(&b.offset)
            .then_with(|| b.length.cmp(&a.length))
            .then_with(|| entity_sort_priority(&a.kind).cmp(&entity_sort_priority(&b.kind)))
    });
    sorted
        .into_iter()
        .filter_map(|e| entity_to_tl(e, session, true))
        .collect()
}

fn entities_to_tl_inner(
    entities: &[TextEntity],
    session: &Session,
    skip_local: bool,
) -> Vec<tl_enums::MessageEntity> {
    entities
        .iter()
        .filter_map(|e| entity_to_tl(e, session, skip_local))
        .collect()
}

fn entity_to_tl(
    entity: &TextEntity,
    session: &Session,
    skip_local: bool,
) -> Option<tl_enums::MessageEntity> {
    let offset = entity.offset;
    let length = entity.length;
    // Skip zero/negative length entities (`api/api_text_entities.cpp:297-299`).
    if length <= 0 || offset < 0 {
        return None;
    }
    if skip_local
        && matches!(
            entity.kind,
            EntityKind::Url
                | EntityKind::Email
                | EntityKind::Phone
                | EntityKind::BankCard
                | EntityKind::TonAddress
                | EntityKind::Hashtag
                | EntityKind::Cashtag
                | EntityKind::Mention
                | EntityKind::BotCommand
        )
    {
        return None;
    }
    Some(match &entity.kind {
        EntityKind::Mention => {
            tl_enums::MessageEntity::Mention(tl_types::MessageEntityMention { offset, length })
        }
        EntityKind::Hashtag => {
            tl_enums::MessageEntity::Hashtag(tl_types::MessageEntityHashtag { offset, length })
        }
        EntityKind::BotCommand => {
            tl_enums::MessageEntity::BotCommand(tl_types::MessageEntityBotCommand {
                offset,
                length,
            })
        }
        EntityKind::Url => {
            tl_enums::MessageEntity::Url(tl_types::MessageEntityUrl { offset, length })
        }
        EntityKind::Email => {
            tl_enums::MessageEntity::Email(tl_types::MessageEntityEmail { offset, length })
        }
        EntityKind::Bold => {
            tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold { offset, length })
        }
        EntityKind::Italic => {
            tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic { offset, length })
        }
        EntityKind::Code => {
            tl_enums::MessageEntity::Code(tl_types::MessageEntityCode { offset, length })
        }
        EntityKind::Pre { language } => tl_enums::MessageEntity::Pre(tl_types::MessageEntityPre {
            offset,
            length,
            language: language.clone(),
        }),
        EntityKind::TextUrl { url } => {
            // Skip `internal:` URLs on send (`api/api_text_entities.cpp:324-332`).
            if url.is_empty() || is_internal_url(url) {
                return None;
            }
            tl_enums::MessageEntity::TextUrl(tl_types::MessageEntityTextUrl {
                offset,
                length,
                url: url.clone(),
            })
        }
        EntityKind::MentionName { user_id } => {
            // Skip invalid/zero mention user IDs (`api/api_text_entities.cpp:50-52`).
            if user_id.is_zero() {
                return None;
            }
            let input_user = input_user_for(session, *user_id);
            if matches!(input_user, tl_enums::InputUser::Empty) {
                return None;
            }
            tl_enums::MessageEntity::InputMessageEntityMentionName(
                tl_types::InputMessageEntityMentionName {
                    offset,
                    length,
                    user_id: input_user,
                },
            )
        }
        EntityKind::Phone => {
            tl_enums::MessageEntity::Phone(tl_types::MessageEntityPhone { offset, length })
        }
        EntityKind::Cashtag => {
            tl_enums::MessageEntity::Cashtag(tl_types::MessageEntityCashtag { offset, length })
        }
        EntityKind::Underline => {
            tl_enums::MessageEntity::Underline(tl_types::MessageEntityUnderline { offset, length })
        }
        EntityKind::StrikeOut => {
            tl_enums::MessageEntity::Strike(tl_types::MessageEntityStrike { offset, length })
        }
        EntityKind::BankCard => {
            tl_enums::MessageEntity::BankCard(tl_types::MessageEntityBankCard { offset, length })
        }
        EntityKind::Spoiler => {
            tl_enums::MessageEntity::Spoiler(tl_types::MessageEntitySpoiler { offset, length })
        }
        EntityKind::CustomEmoji { document_id, .. } => {
            // Skip zero custom emoji document IDs (`api/api_text_entities.cpp:34-37`).
            if document_id.0 == 0 {
                return None;
            }
            tl_enums::MessageEntity::CustomEmoji(tl_types::MessageEntityCustomEmoji {
                offset,
                length,
                document_id: document_id.to_tl(),
            })
        }
        EntityKind::Blockquote { collapsed } => {
            tl_enums::MessageEntity::Blockquote(tl_types::MessageEntityBlockquote {
                collapsed: *collapsed,
                offset,
                length,
            })
        }
        EntityKind::FormattedDate { date, flags } => {
            // Skip zero formatted date (`api/api_text_entities.cpp:416`).
            if *date == 0 {
                return None;
            }
            tl_enums::MessageEntity::FormattedDate(tl_types::MessageEntityFormattedDate {
                relative: flags.contains(FormattedDateFlags::RELATIVE),
                short_time: flags.contains(FormattedDateFlags::SHORT_TIME),
                long_time: flags.contains(FormattedDateFlags::LONG_TIME),
                short_date: flags.contains(FormattedDateFlags::SHORT_DATE),
                long_date: flags.contains(FormattedDateFlags::LONG_DATE),
                day_of_week: flags.contains(FormattedDateFlags::DAY_OF_WEEK),
                offset,
                length,
                date: *date,
            })
        }
        EntityKind::DiffInsert => {
            tl_enums::MessageEntity::DiffInsert(tl_types::MessageEntityDiffInsert {
                offset,
                length,
            })
        }
        EntityKind::DiffReplace { old_text } => {
            tl_enums::MessageEntity::DiffReplace(tl_types::MessageEntityDiffReplace {
                offset,
                length,
                old_text: old_text.clone(),
            })
        }
        EntityKind::DiffDelete => {
            tl_enums::MessageEntity::DiffDelete(tl_types::MessageEntityDiffDelete {
                offset,
                length,
            })
        }
        EntityKind::TonAddress => {
            tl_enums::MessageEntity::TonAddress(tl_types::MessageEntityTonAddress {
                offset,
                length,
            })
        }
        // Local UI-only or unknown entities are never transmitted on the wire.
        EntityKind::Unknown
        | EntityKind::Semibold
        | EntityKind::MediaTimestamp { .. }
        | EntityKind::Colorized
        | EntityKind::Subscript
        | EntityKind::Superscript
        | EntityKind::Marked => return None,
    })
}

/// Converts a TL `TextWithEntities` into `tg_data::TextWithEntities`.
#[must_use]
pub fn text_with_entities_from_tl(text: &tl_enums::TextWithEntities) -> TextWithEntities {
    let tl_enums::TextWithEntities::TextWithEntities(t) = text;
    TextWithEntities::new(
        t.text.clone(),
        entities_from_tl_for_text(&t.text, &t.entities, UserId::ZERO),
    )
}

/// Converts a `tg_data::TextWithEntities` into a TL `TextWithEntities`.
#[must_use]
pub fn text_with_entities_to_tl(
    text: &TextWithEntities,
    session: &Session,
) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: text.text.clone(),
        entities: entities_to_tl(&text.entities, session),
    })
}

/// Converts a TL `MessageFwdHeader` into `tg_data::ForwardInfo`.
#[must_use]
pub fn forward_info_from_tl(fwd: &tl_enums::MessageFwdHeader) -> ForwardInfo {
    let tl_enums::MessageFwdHeader::MessageFwdHeader(f) = fwd;
    ForwardInfo {
        from_id: f.from_id.as_ref().map(peer_id_from_tl),
        from_name: f.from_name.clone(),
        date: f.date,
        channel_post: f.channel_post.map(|id| MsgId(i64::from(id))),
        post_author: f.post_author.clone(),
        saved_from_peer: f.saved_from_peer.as_ref().map(peer_id_from_tl),
        saved_from_msg_id: f.saved_from_msg_id.map(|id| MsgId(i64::from(id))),
        saved_from_id: f.saved_from_id.as_ref().map(peer_id_from_tl),
        saved_from_name: f.saved_from_name.clone(),
        saved_date: f.saved_date,
        saved_out: f.saved_out,
        psa_type: f.psa_type.clone(),
        imported: f.imported,
    }
}

/// Converts a TL `MessageReplyHeader` into `tg_data::ReplyInfo`.
#[must_use]
pub fn reply_info_from_tl(reply: &tl_enums::MessageReplyHeader) -> ReplyInfo {
    match reply {
        tl_enums::MessageReplyHeader::MessageReplyHeader(r) => ReplyInfo {
            reply_to_msg_id: r.reply_to_msg_id.map(|id| MsgId(i64::from(id))),
            reply_to_peer_id: r.reply_to_peer_id.as_ref().map(peer_id_from_tl),
            reply_to_story: None,
            top_msg_id: r.reply_to_top_id.map(|id| MsgId(i64::from(id))),
            monoforum_peer_id: None,
            forum_topic: r.forum_topic,
            quote_text: r.quote_text.as_ref().map(|q| {
                TextWithEntities::new(
                    q.clone(),
                    r.quote_entities
                        .as_deref()
                        .map(entities_from_tl)
                        .unwrap_or_default(),
                )
            }),
            quote_offset: r.quote_offset,
            quote: r.quote,
            reply_from: r
                .reply_from
                .as_ref()
                .map(|f| Box::new(forward_info_from_tl(f))),
            todo_item_id: r.todo_item_id,
            poll_option: r.poll_option.clone(),
            reply_media: r
                .reply_media
                .as_ref()
                .map(message_media_from_tl)
                .filter(|media| !matches!(media, tg_data::MessageMedia::Empty))
                .map(Box::new),
        },
        tl_enums::MessageReplyHeader::MessageReplyStoryHeader(s) => ReplyInfo {
            reply_to_story: Some(FullStoryId::new(peer_id_from_tl(&s.peer), s.story_id)),
            ..ReplyInfo::default()
        },
    }
}

/// Builds a TL `InputReplyTo` from a `tg_data::ReplyInfo` using `session` for access hashes.
#[must_use]
pub fn input_reply_to_from_reply_info(
    reply: &ReplyInfo,
    session: &Session,
) -> Option<tl_enums::InputReplyTo> {
    if let Some(story) = reply.reply_to_story {
        return Some(tl_enums::InputReplyTo::Story(tl_types::InputReplyToStory {
            peer: input_peer_for(session, story.peer),
            story_id: story.story,
        }));
    }
    if let Some(msg_id) = reply.reply_to_msg_id {
        // Match `ReplyToForMTP` (`data/data_histories.cpp:87-100`):
        // only include `quote_text`, `quote_entities` (with `SkipLocal`), and `quote_offset`
        // when `quote_text` is non-empty.
        let non_empty_quote = reply.quote_text.as_ref().filter(|q| !q.text.is_empty());
        let quote_text = non_empty_quote.map(|q| q.text.clone());
        let quote_entities = non_empty_quote
            .map(|q| entities_to_tl_for_sending(&q.entities, session))
            .filter(|v| !v.is_empty());
        let quote_offset = if quote_text.is_some() {
            reply.quote_offset
        } else {
            None
        };
        return Some(tl_enums::InputReplyTo::Message(
            tl_types::InputReplyToMessage {
                reply_to_msg_id: msg_id.as_i32().unwrap_or(0),
                top_msg_id: reply.top_msg_id.and_then(MsgId::as_i32),
                reply_to_peer_id: reply.reply_to_peer_id.map(|p| input_peer_for(session, p)),
                quote_text,
                quote_entities,
                quote_offset,
                monoforum_peer_id: reply.monoforum_peer_id.map(|p| input_peer_for(session, p)),
                todo_item_id: reply.todo_item_id,
                poll_option: reply.poll_option.clone(),
                quote_rich: None,
            },
        ));
    }
    if let Some(mono_peer) = reply.monoforum_peer_id {
        return Some(tl_enums::InputReplyTo::MonoForum(
            tl_types::InputReplyToMonoForum {
                monoforum_peer_id: input_peer_for(session, mono_peer),
            },
        ));
    }
    if let Some(top_msg_id) = reply.top_msg_id {
        return Some(tl_enums::InputReplyTo::Message(
            tl_types::InputReplyToMessage {
                reply_to_msg_id: top_msg_id.as_i32().unwrap_or(0),
                top_msg_id: top_msg_id.as_i32(),
                reply_to_peer_id: None,
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                monoforum_peer_id: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            },
        ));
    }
    None
}

fn button_visual_from_tl(style: Option<&tl_enums::KeyboardButtonStyle>) -> ButtonVisual {
    let Some(tl_enums::KeyboardButtonStyle::KeyboardButtonStyle(s)) = style else {
        return ButtonVisual::default();
    };
    ButtonVisual {
        icon_id: s.icon.map(DocumentId::from_tl).unwrap_or(DocumentId::ZERO),
        color: ButtonColor::from_style_flags(s.bg_danger, s.bg_primary, s.bg_success),
    }
}

/// Converts a slice of TL `InlineQueryPeerType` values into [`InlineQueryPeerTypes`].
///
/// Port of `PeerTypesFromMTP` in `history/history_item_reply_markup.cpp:279-299`.
#[must_use]
pub fn inline_query_peer_types_from_tl(
    types: &[tl_enums::InlineQueryPeerType],
) -> InlineQueryPeerTypes {
    let mut result = InlineQueryPeerTypes::empty();
    for ty in types {
        match ty {
            tl_enums::InlineQueryPeerType::PM => result |= InlineQueryPeerTypes::USER,
            tl_enums::InlineQueryPeerType::Chat | tl_enums::InlineQueryPeerType::Megagroup => {
                result |= InlineQueryPeerTypes::GROUP;
            }
            tl_enums::InlineQueryPeerType::Broadcast => result |= InlineQueryPeerTypes::BROADCAST,
            tl_enums::InlineQueryPeerType::BotPM => result |= InlineQueryPeerTypes::BOT,
            tl_enums::InlineQueryPeerType::SameBotPM => {}
        }
    }
    result
}

fn request_peer_query_from_tl(req: &tl_types::ButtonTypeRequestPeer) -> RequestPeerQuery {
    let mut query = RequestPeerQuery {
        max_quantity: req.max_quantity,
        ..RequestPeerQuery::default()
    };
    match &req.peer_type {
        tl_enums::RequestPeerType::User(u) => {
            query.peer_type = RequestPeerType::User;
            query.user_is_bot = RequestPeerRestriction::from_option(u.bot);
            query.user_is_premium = RequestPeerRestriction::from_option(u.premium);
        }
        tl_enums::RequestPeerType::Chat(c) => {
            query.peer_type = RequestPeerType::Group;
            query.am_creator = c.creator;
            query.is_bot_participant = c.bot_participant;
            query.group_is_forum = RequestPeerRestriction::from_option(c.forum);
            query.has_username = RequestPeerRestriction::from_option(c.has_username);
            if let Some(ref rights) = c.user_admin_rights {
                query.my_rights = admin_rights_from_tl(rights);
            }
            if let Some(ref rights) = c.bot_admin_rights {
                query.bot_rights = admin_rights_from_tl(rights);
            }
        }
        tl_enums::RequestPeerType::Broadcast(b) => {
            query.peer_type = RequestPeerType::Broadcast;
            query.am_creator = b.creator;
            query.has_username = RequestPeerRestriction::from_option(b.has_username);
            if let Some(ref rights) = b.user_admin_rights {
                query.my_rights = admin_rights_from_tl(rights);
            }
            if let Some(ref rights) = b.bot_admin_rights {
                query.bot_rights = admin_rights_from_tl(rights);
            }
        }
        tl_enums::RequestPeerType::CreateBot(_) => {}
    }
    query
}

/// Converts a TL `KeyboardButton` (reply keyboard button, layer 233) into `tg_data::KeyboardButton`.
///
/// Port of `ParseButton(const MTPKeyboardButton &)` in `history/history_item_reply_markup.cpp`.
#[must_use]
pub fn keyboard_button_from_tl(btn: &tl_enums::KeyboardButton) -> Option<KeyboardButton> {
    let tl_enums::KeyboardButton::KeyboardButton(b) = btn;
    let visual = button_visual_from_tl(b.style.as_ref());
    let kind = match &b.r#type {
        tl_enums::ButtonType::Default => KeyboardButtonKind::Default,
        tl_enums::ButtonType::RequestPhone => KeyboardButtonKind::RequestPhone,
        tl_enums::ButtonType::RequestGeoLocation => KeyboardButtonKind::RequestLocation,
        tl_enums::ButtonType::RequestPoll(p) => KeyboardButtonKind::RequestPoll {
            restriction: PollRequestRestriction::from_quiz_option(p.quiz),
        },
        tl_enums::ButtonType::RequestPeer(req) => match &req.peer_type {
            tl_enums::RequestPeerType::CreateBot(cb) => KeyboardButtonKind::CreateBot {
                button_id: req.button_id,
                suggested_name: cb.suggested_name.clone().unwrap_or_default(),
                suggested_username: cb.suggested_username.clone().unwrap_or_default(),
            },
            _ => KeyboardButtonKind::RequestPeer {
                button_id: req.button_id,
                query: request_peer_query_from_tl(req),
            },
        },
        tl_enums::ButtonType::InputButtonTypeRequestPeer(_) => return None,
        tl_enums::ButtonType::SimpleWebView(wv) => KeyboardButtonKind::SimpleWebView {
            url: wv.url.clone(),
        },
    };
    Some(KeyboardButton {
        text: b.text.clone(),
        visual,
        kind,
    })
}

/// Converts a TL `KeyboardInlineButton` (inline keyboard button, layer 233) into `tg_data::KeyboardButton`.
///
/// Port of `ParseInlineButton` in `history/history_item_reply_markup.cpp`.
#[must_use]
pub fn inline_button_from_tl(btn: &tl_enums::KeyboardInlineButton) -> Option<KeyboardButton> {
    let tl_enums::KeyboardInlineButton::KeyboardInlineButton(b) = btn;
    let visual = button_visual_from_tl(b.style.as_ref());
    let kind = match &b.r#type {
        tl_enums::InlineButtonType::Url(u) => KeyboardButtonKind::Url { url: u.url.clone() },
        tl_enums::InlineButtonType::UrlAuth(a) => KeyboardButtonKind::UrlAuth {
            url: a.url.clone(),
            forward_text: a.fwd_text.clone().unwrap_or_default(),
            button_id: a.button_id,
        },
        tl_enums::InlineButtonType::InputInlineButtonTypeUrlAuth(_) => return None,
        tl_enums::InlineButtonType::WebView(wv) => KeyboardButtonKind::WebView {
            url: wv.url.clone(),
        },
        tl_enums::InlineButtonType::Callback(cb) => {
            if cb.requires_password {
                KeyboardButtonKind::CallbackWithPassword {
                    data: cb.data.clone(),
                }
            } else {
                KeyboardButtonKind::Callback {
                    data: cb.data.clone(),
                }
            }
        }
        tl_enums::InlineButtonType::Game => KeyboardButtonKind::Game,
        tl_enums::InlineButtonType::Buy => KeyboardButtonKind::Buy,
        tl_enums::InlineButtonType::SwitchInline(sw) => {
            if sw.same_peer {
                KeyboardButtonKind::SwitchInlineSame {
                    query: sw.query.clone(),
                }
            } else {
                let peer_types = sw
                    .peer_types
                    .as_deref()
                    .map(inline_query_peer_types_from_tl)
                    .unwrap_or_default();
                KeyboardButtonKind::SwitchInline {
                    query: sw.query.clone(),
                    peer_types,
                }
            }
        }
        tl_enums::InlineButtonType::UserProfile(up) => KeyboardButtonKind::UserProfile {
            user_id: UserId::from_tl(up.user_id),
        },
        tl_enums::InlineButtonType::InputInlineButtonTypeUserProfile(_) => return None,
        tl_enums::InlineButtonType::Copy(c) => KeyboardButtonKind::CopyText {
            text: c.copy_text.clone(),
        },
        tl_enums::InlineButtonType::Disabled => KeyboardButtonKind::Disabled,
    };
    Some(KeyboardButton {
        text: b.text.clone(),
        visual,
        kind,
    })
}

/// Converts a TL `ReplyMarkup` (all 4 constructors and all layer 233 button kinds)
/// into `tg_data::ReplyMarkup`.
///
/// Port of `HistoryMessageMarkupData::HistoryMessageMarkupData` in
/// `history/history_item_reply_markup.cpp`.
#[must_use]
pub fn reply_markup_from_tl(markup: &tl_enums::ReplyMarkup) -> ReplyMarkup {
    match markup {
        tl_enums::ReplyMarkup::ReplyInlineMarkup(m) => {
            let rows = m
                .rows
                .iter()
                .filter_map(|row| {
                    let tl_enums::KeyboardInlineButtonRow::KeyboardInlineButtonRow(r) = row;
                    let buttons: Vec<KeyboardButton> =
                        r.buttons.iter().filter_map(inline_button_from_tl).collect();
                    (!buttons.is_empty()).then_some(buttons)
                })
                .collect();
            ReplyMarkup::Inline {
                rows,
                force_reply: m.force_reply,
            }
        }
        tl_enums::ReplyMarkup::ReplyKeyboardMarkup(m) => {
            let rows = m
                .rows
                .iter()
                .filter_map(|row| {
                    let tl_enums::KeyboardButtonRow::KeyboardButtonRow(r) = row;
                    let buttons: Vec<KeyboardButton> = r
                        .buttons
                        .iter()
                        .filter_map(keyboard_button_from_tl)
                        .collect();
                    (!buttons.is_empty()).then_some(buttons)
                })
                .collect();
            ReplyMarkup::Keyboard {
                rows,
                resize: m.resize,
                single_use: m.single_use,
                selective: m.selective,
                persistent: m.persistent,
                force_reply: m.force_reply,
                placeholder: m.placeholder.clone().unwrap_or_default(),
            }
        }
        tl_enums::ReplyMarkup::ReplyKeyboardForceReply(m) => ReplyMarkup::ForceReply {
            single_use: m.single_use,
            selective: m.selective,
            placeholder: m.placeholder.clone().unwrap_or_default(),
        },
        tl_enums::ReplyMarkup::ReplyKeyboardHide(m) => ReplyMarkup::RemoveKeyboard {
            selective: m.selective,
        },
    }
}

/// Converts a TL `Message` (`message`, `messageService`, `messageEmpty`) into a `tg_data::Message`.
#[must_use]
pub fn message_from_tl(tl_msg: &tl_enums::Message, self_id: UserId) -> Option<Message> {
    match tl_msg {
        tl_enums::Message::Empty(e) => {
            let peer_id = e.peer_id.as_ref().map(peer_id_from_tl)?;
            Some(Message {
                id: MsgId(i64::from(e.id)),
                peer_id,
                from_id: None,
                date: 0,
                edit_date: None,
                flags: MessageFlags::empty(),
                content: MessageContent::Empty,
                reply_to: None,
                forward_from: None,
                views: None,
                forwards: None,
                replies_count: None,
                reactions: None,
                grouped_id: None,
                via_bot_id: None,
                via_business_bot_id: None,
                post_author: None,
                from_rank: None,
                ttl_period: None,
                schedule_repeat_period: None,
                effect_id: None,
                shortcut_id: None,
                paid_message_stars: None,
                reply_markup: None,
            })
        }
        tl_enums::Message::Message(m) => {
            let peer_id = peer_id_from_tl(&m.peer_id);
            let from_id = m.from_id.as_ref().map(peer_id_from_tl).or_else(|| {
                if m.out && !m.post {
                    Some(PeerId::from_user(self_id))
                } else if peer_id.is_user() && !m.post {
                    Some(peer_id)
                } else {
                    None
                }
            });

            let reply_markup = m.reply_markup.as_ref().map(reply_markup_from_tl);

            let mut flags = MessageFlags::HISTORY_ENTRY;
            flags.set(MessageFlags::OUTGOING, m.out);
            flags.set(MessageFlags::MENTIONS_ME, m.mentioned);
            flags.set(MessageFlags::MEDIA_IS_UNREAD, m.media_unread);
            flags.set(MessageFlags::SILENT, m.silent);
            flags.set(MessageFlags::POST, m.post);
            flags.set(MessageFlags::IS_OR_WAS_SCHEDULED, m.from_scheduled);
            flags.set(MessageFlags::LEGACY, m.legacy);
            flags.set(MessageFlags::HIDE_EDITED, m.edit_hide);
            flags.set(MessageFlags::PINNED, m.pinned);
            flags.set(MessageFlags::NO_FORWARDS, m.noforwards);
            flags.set(MessageFlags::INVERT_MEDIA, m.invert_media);
            flags.set(
                MessageFlags::STARS_PAID_SUGGESTED,
                m.paid_suggested_post_stars,
            );
            flags.set(MessageFlags::TON_PAID_SUGGESTED, m.paid_suggested_post_ton);
            flags.set(MessageFlags::HAS_FROM_ID, from_id.is_some());
            flags.set(MessageFlags::HAS_POST_AUTHOR, m.post_author.is_some());
            flags.set(MessageFlags::HAS_VIEWS, m.views.is_some());
            flags.set(MessageFlags::HAS_REPLY_INFO, m.replies.is_some());
            flags.set(MessageFlags::HAS_REPLY_MARKUP, reply_markup.is_some());
            flags.set(
                MessageFlags::HAS_SWITCH_INLINE_BUTTON,
                reply_markup
                    .as_ref()
                    .is_some_and(ReplyMarkup::has_switch_inline),
            );
            flags.set(MessageFlags::FROM_INLINE_BOT, m.via_bot_id.is_some());
            flags.set(
                MessageFlags::SHORTCUT_MESSAGE,
                m.quick_reply_shortcut_id.is_some(),
            );
            flags.set(
                MessageFlags::HAS_RESTRICTIONS,
                m.restriction_reason.as_ref().is_some_and(|r| !r.is_empty()),
            );
            flags.set(
                MessageFlags::GUEST_CHAT_VIA_FROM,
                m.guestchat_via_from.is_some(),
            );
            flags.set(
                MessageFlags::CAN_BE_SUMMARIZED,
                m.summary_from_language.is_some(),
            );

            let entities = m
                .entities
                .as_deref()
                .map(|e| entities_from_tl_for_text(&m.message, e, self_id))
                .unwrap_or_default();
            if entities
                .iter()
                .any(|e| matches!(e.kind, EntityKind::Url | EntityKind::TextUrl { .. }))
            {
                flags.insert(MessageFlags::HAS_TEXT_LINKS);
            }
            let text = TextWithEntities::new(m.message.clone(), entities);
            let media = m
                .media
                .as_ref()
                .map(message_media_from_tl)
                .unwrap_or(MessageMedia::Empty);

            let reactions = m.reactions.as_ref().map(reactions_from_tl);
            if let Some(ref rx) = reactions {
                flags.set(MessageFlags::CAN_VIEW_REACTIONS, rx.can_see_list);
                flags.set(MessageFlags::REACTIONS_ARE_TAGS, rx.reactions_as_tags);
                if rx.recent_reactors.iter().any(|r| r.unread) {
                    flags.insert(MessageFlags::HAS_UNREAD_REACTION);
                }
            }

            let replies_count = m.replies.as_ref().map(|r| {
                let tl_enums::MessageReplies::MessageReplies(mr) = r;
                mr.replies
            });

            Some(Message {
                id: MsgId(i64::from(m.id)),
                peer_id,
                from_id,
                date: m.date,
                edit_date: m.edit_date,
                flags,
                content: MessageContent::Regular { text, media },
                reply_to: m.reply_to.as_ref().map(reply_info_from_tl),
                forward_from: m.fwd_from.as_ref().map(forward_info_from_tl),
                views: m.views,
                forwards: m.forwards,
                replies_count,
                reactions,
                grouped_id: m.grouped_id.map(|g| g as u64),
                via_bot_id: m.via_bot_id.map(UserId::from_tl),
                via_business_bot_id: m.via_business_bot_id.map(UserId::from_tl),
                post_author: m.post_author.clone(),
                from_rank: m.from_rank.clone().filter(|r| !r.is_empty()),
                ttl_period: m.ttl_period,
                schedule_repeat_period: m.schedule_repeat_period,
                effect_id: m.effect.map(EffectId::from_tl),
                shortcut_id: m.quick_reply_shortcut_id as Option<BusinessShortcutId>,
                paid_message_stars: m.paid_message_stars,
                reply_markup,
            })
        }
        tl_enums::Message::Service(s) => {
            let peer_id = peer_id_from_tl(&s.peer_id);
            let from_id = s.from_id.as_ref().map(peer_id_from_tl).or_else(|| {
                if s.out && !s.post {
                    Some(PeerId::from_user(self_id))
                } else {
                    None
                }
            });
            let action = message_action_from_tl(&s.action);
            let mut msg =
                Message::new_service(peer_id, MsgId(i64::from(s.id)), from_id, s.date, action);
            msg.flags.set(MessageFlags::OUTGOING, s.out);
            msg.flags.set(MessageFlags::MENTIONS_ME, s.mentioned);
            msg.flags.set(MessageFlags::MEDIA_IS_UNREAD, s.media_unread);
            msg.flags.set(MessageFlags::SILENT, s.silent);
            msg.flags.set(MessageFlags::POST, s.post);
            msg.flags.set(MessageFlags::LEGACY, s.legacy);
            msg.flags
                .set(MessageFlags::REACTIONS_ALLOWED, s.reactions_are_possible);
            msg.reply_to = s.reply_to.as_ref().map(reply_info_from_tl);
            msg.reactions = s.reactions.as_ref().map(reactions_from_tl);
            msg.ttl_period = s.ttl_period;
            Some(msg)
        }
    }
}

/// Converts and inserts a TL `Message` into `session`, returning its `FullMsgId`.
pub fn apply_tl_message(session: &mut Session, tl_msg: &tl_enums::Message) -> Option<FullMsgId> {
    let msg = message_from_tl(tl_msg, session.self_user_id())?;
    let id = msg.full_id();
    session.insert_message(msg);
    Some(id)
}

/// Converts and stores a TL `Message` that came with a page which is not the chat's own bottom
/// (topic / comment pages, search results, unread-mention pages, reference refreshes):
/// `NewMessageType::Existing`, see [`Session::insert_existing_message`].
pub fn apply_tl_existing_message(
    session: &mut Session,
    tl_msg: &tl_enums::Message,
) -> Option<FullMsgId> {
    let msg = message_from_tl(tl_msg, session.self_user_id())?;
    let id = msg.full_id();
    session.insert_existing_message(msg);
    Some(id)
}

fn peer_data_is_loaded(session: &Session, peer_id: PeerId) -> bool {
    peer_id.is_zero() || session.peer_loaded(peer_id)
}

fn mention_users_data_is_loaded(session: &Session, entities: &[tl_enums::MessageEntity]) -> bool {
    for entity in entities {
        match entity {
            tl_enums::MessageEntity::MentionName(m) => {
                if !session.user_loaded(UserId::from_tl(m.user_id)) {
                    return false;
                }
            }
            tl_enums::MessageEntity::InputMessageEntityMentionName(m) => {
                if let tl_enums::InputUser::InputUser(u) = &m.user_id
                    && !session.user_loaded(UserId::from_tl(u.user_id))
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn forwarded_info_data_is_loaded(session: &Session, fwd: &tl_enums::MessageFwdHeader) -> bool {
    let tl_enums::MessageFwdHeader::MessageFwdHeader(data) = fwd;
    data.from_id
        .as_ref()
        .is_none_or(|p| peer_data_is_loaded(session, peer_id_from_tl(p)))
        && data
            .saved_from_peer
            .as_ref()
            .is_none_or(|p| peer_data_is_loaded(session, peer_id_from_tl(p)))
        && data
            .saved_from_id
            .as_ref()
            .is_none_or(|p| peer_data_is_loaded(session, peer_id_from_tl(p)))
}

fn reply_data_is_loaded(session: &Session, reply: &tl_enums::MessageReplyHeader) -> bool {
    match reply {
        tl_enums::MessageReplyHeader::MessageReplyHeader(data) => {
            data.reply_to_peer_id
                .as_ref()
                .is_none_or(|p| peer_data_is_loaded(session, peer_id_from_tl(p)))
                && data
                    .reply_from
                    .as_ref()
                    .is_none_or(|f| forwarded_info_data_is_loaded(session, f))
                && data
                    .quote_entities
                    .as_deref()
                    .is_none_or(|e| mention_users_data_is_loaded(session, e))
        }
        tl_enums::MessageReplyHeader::MessageReplyStoryHeader(data) => {
            peer_data_is_loaded(session, peer_id_from_tl(&data.peer))
        }
    }
}

/// Checks whether the owning peer of a TL `Message` is loaded with non-`min` metadata
/// (`api_updates.cpp`: `updateNewMessage` peer check).
#[must_use]
pub fn message_peer_loaded(session: &Session, tl_msg: &tl_enums::Message) -> bool {
    let peer_id = match tl_msg {
        tl_enums::Message::Message(m) => peer_id_from_tl(&m.peer_id),
        tl_enums::Message::Service(s) => peer_id_from_tl(&s.peer_id),
        tl_enums::Message::Empty(e) => match &e.peer_id {
            Some(p) => peer_id_from_tl(p),
            None => PeerId::ZERO,
        },
    };
    peer_data_is_loaded(session, peer_id)
}

/// Checks whether all peers referenced by an `UpdateShortMessage` are already loaded
/// with non-`min` metadata in `session` (`api_updates.cpp`: `DataIsLoaded(MTPDupdateShortMessage)`).
///
/// If this returns `false` (including when a peer is only loaded from a `min` constructor),
/// tdesktop triggers `updates.getDifference` so the server supplies the full `users` / `chats`.
#[must_use]
pub fn short_message_data_loaded(session: &Session, short: &tl_types::UpdateShortMessage) -> bool {
    session.user_loaded(UserId::from_tl(short.user_id))
        && short
            .fwd_from
            .as_ref()
            .is_none_or(|f| forwarded_info_data_is_loaded(session, f))
        && short
            .via_bot_id
            .is_none_or(|id| session.user_loaded(UserId::from_tl(id)))
        && short
            .reply_to
            .as_ref()
            .is_none_or(|r| reply_data_is_loaded(session, r))
        && short
            .entities
            .as_deref()
            .is_none_or(|e| mention_users_data_is_loaded(session, e))
}

/// Checks whether all peers referenced by an `UpdateShortChatMessage` are already loaded
/// with non-`min` metadata in `session` (`api_updates.cpp`: `DataIsLoaded(MTPDupdateShortChatMessage)`).
#[must_use]
pub fn short_chat_message_data_loaded(
    session: &Session,
    short: &tl_types::UpdateShortChatMessage,
) -> bool {
    session.chat_loaded(ChatId::from_tl(short.chat_id))
        && session.user_loaded(UserId::from_tl(short.from_id))
        && short
            .fwd_from
            .as_ref()
            .is_none_or(|f| forwarded_info_data_is_loaded(session, f))
        && short
            .via_bot_id
            .is_none_or(|id| session.user_loaded(UserId::from_tl(id)))
        && short
            .reply_to
            .as_ref()
            .is_none_or(|r| reply_data_is_loaded(session, r))
        && short
            .entities
            .as_deref()
            .is_none_or(|e| mention_users_data_is_loaded(session, e))
}

/// Converts a TL `UpdateShortMessage` into a domain `tg_data::Message`.
#[must_use]
pub fn message_from_short_message(
    short: &tl_types::UpdateShortMessage,
    self_id: UserId,
) -> Message {
    let peer_id = PeerId::from_user(UserId::from_tl(short.user_id));
    let from_id = if short.out {
        PeerId::from_user(self_id)
    } else {
        peer_id
    };
    let entities = short
        .entities
        .as_deref()
        .map(|e| entities_from_tl_for_text(&short.message, e, self_id))
        .unwrap_or_default();
    let mut msg = Message::new_text(
        peer_id,
        MsgId(i64::from(short.id)),
        Some(from_id),
        short.date,
        TextWithEntities::new(short.message.clone(), entities),
    );
    msg.flags.set(MessageFlags::OUTGOING, short.out);
    msg.flags.set(MessageFlags::MENTIONS_ME, short.mentioned);
    msg.flags
        .set(MessageFlags::MEDIA_IS_UNREAD, short.media_unread);
    msg.flags.set(MessageFlags::SILENT, short.silent);
    msg.flags
        .set(MessageFlags::FROM_INLINE_BOT, short.via_bot_id.is_some());
    msg.forward_from = short.fwd_from.as_ref().map(forward_info_from_tl);
    msg.reply_to = short.reply_to.as_ref().map(reply_info_from_tl);
    msg.via_bot_id = short.via_bot_id.map(UserId::from_tl);
    msg.ttl_period = short.ttl_period;
    msg
}

/// Converts a TL `UpdateShortChatMessage` into a domain `tg_data::Message`.
#[must_use]
pub fn message_from_short_chat_message(
    short: &tl_types::UpdateShortChatMessage,
    self_id: UserId,
) -> Message {
    let peer_id = PeerId::from_chat(ChatId::from_tl(short.chat_id));
    let from_id = PeerId::from_user(UserId::from_tl(short.from_id));
    let entities = short
        .entities
        .as_deref()
        .map(|e| entities_from_tl_for_text(&short.message, e, self_id))
        .unwrap_or_default();
    let mut msg = Message::new_text(
        peer_id,
        MsgId(i64::from(short.id)),
        Some(from_id),
        short.date,
        TextWithEntities::new(short.message.clone(), entities),
    );
    msg.flags.set(MessageFlags::OUTGOING, short.out);
    msg.flags.set(MessageFlags::MENTIONS_ME, short.mentioned);
    msg.flags
        .set(MessageFlags::MEDIA_IS_UNREAD, short.media_unread);
    msg.flags.set(MessageFlags::SILENT, short.silent);
    msg.flags
        .set(MessageFlags::FROM_INLINE_BOT, short.via_bot_id.is_some());
    msg.forward_from = short.fwd_from.as_ref().map(forward_info_from_tl);
    msg.reply_to = short.reply_to.as_ref().map(reply_info_from_tl);
    msg.via_bot_id = short.via_bot_id.map(UserId::from_tl);
    msg.ttl_period = short.ttl_period;
    msg
}

#[cfg(test)]
mod reply_markup_tests {
    use super::*;

    #[test]
    fn converts_all_reply_markup_variants_and_layer_233_buttons() {
        let style = Some(tl_enums::KeyboardButtonStyle::KeyboardButtonStyle(
            tl_types::KeyboardButtonStyle {
                bg_primary: false,
                bg_danger: true,
                bg_success: true, // danger wins over success
                icon: Some(4242),
            },
        ));

        let inline_tl = tl_enums::ReplyMarkup::ReplyInlineMarkup(tl_types::ReplyInlineMarkup {
            force_reply: true,
            rows: vec![tl_enums::KeyboardInlineButtonRow::KeyboardInlineButtonRow(
                tl_types::KeyboardInlineButtonRow {
                    buttons: vec![
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: style.clone(),
                                text: "URL".to_owned(),
                                r#type: tl_enums::InlineButtonType::Url(
                                    tl_types::InlineButtonTypeUrl {
                                        url: "https://example.org".to_owned(),
                                    },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "Auth".to_owned(),
                                r#type: tl_enums::InlineButtonType::UrlAuth(
                                    tl_types::InlineButtonTypeUrlAuth {
                                        fwd_text: Some("Open".to_owned()),
                                        url: "https://auth.example.org".to_owned(),
                                        button_id: 77,
                                    },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "App".to_owned(),
                                r#type: tl_enums::InlineButtonType::WebView(
                                    tl_types::InlineButtonTypeWebView {
                                        url: "https://app.example.org".to_owned(),
                                    },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "CB".to_owned(),
                                r#type: tl_enums::InlineButtonType::Callback(
                                    tl_types::InlineButtonTypeCallback {
                                        requires_password: false,
                                        data: b"cb1".to_vec(),
                                    },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "CB2FA".to_owned(),
                                r#type: tl_enums::InlineButtonType::Callback(
                                    tl_types::InlineButtonTypeCallback {
                                        requires_password: true,
                                        data: b"cb2".to_vec(),
                                    },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "Play".to_owned(),
                                r#type: tl_enums::InlineButtonType::Game,
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "Buy".to_owned(),
                                r#type: tl_enums::InlineButtonType::Buy,
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "Switch".to_owned(),
                                r#type: tl_enums::InlineButtonType::SwitchInline(
                                    tl_types::InlineButtonTypeSwitchInline {
                                        same_peer: false,
                                        query: "q".to_owned(),
                                        peer_types: Some(vec![
                                            tl_enums::InlineQueryPeerType::PM,
                                            tl_enums::InlineQueryPeerType::Broadcast,
                                        ]),
                                    },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "SwitchSame".to_owned(),
                                r#type: tl_enums::InlineButtonType::SwitchInline(
                                    tl_types::InlineButtonTypeSwitchInline {
                                        same_peer: true,
                                        query: "here".to_owned(),
                                        peer_types: None,
                                    },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "Profile".to_owned(),
                                r#type: tl_enums::InlineButtonType::UserProfile(
                                    tl_types::InlineButtonTypeUserProfile { user_id: 12345 },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "Copy".to_owned(),
                                r#type: tl_enums::InlineButtonType::Copy(
                                    tl_types::InlineButtonTypeCopy {
                                        copy_text: "CODE-1".to_owned(),
                                    },
                                ),
                            },
                        ),
                        tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                            tl_types::KeyboardInlineButton {
                                style: None,
                                text: "Off".to_owned(),
                                r#type: tl_enums::InlineButtonType::Disabled,
                            },
                        ),
                    ],
                },
            )],
        });

        let parsed = reply_markup_from_tl(&inline_tl);
        assert!(parsed.is_inline());
        assert!(parsed.has_switch_inline());
        let ReplyMarkup::Inline { rows, force_reply } = parsed else {
            panic!("expected inline markup");
        };
        assert!(force_reply);
        assert_eq!(rows[0].len(), 12);
        assert_eq!(rows[0][0].visual.color, ButtonColor::Danger);
        assert_eq!(rows[0][0].visual.icon_id, DocumentId(4242));
        assert!(matches!(
            &rows[0][7].kind,
            KeyboardButtonKind::SwitchInline { query, peer_types }
                if query == "q"
                    && *peer_types == (InlineQueryPeerTypes::USER | InlineQueryPeerTypes::BROADCAST)
        ));
        assert!(matches!(
            &rows[0][9].kind,
            KeyboardButtonKind::UserProfile { user_id } if *user_id == UserId(12345)
        ));
        assert!(matches!(
            &rows[0][10].kind,
            KeyboardButtonKind::CopyText { text } if text == "CODE-1"
        ));

        let reply_kb_tl =
            tl_enums::ReplyMarkup::ReplyKeyboardMarkup(tl_types::ReplyKeyboardMarkup {
                resize: true,
                single_use: true,
                selective: false,
                persistent: true,
                force_reply: false,
                rows: vec![tl_enums::KeyboardButtonRow::KeyboardButtonRow(
                    tl_types::KeyboardButtonRow {
                        buttons: vec![
                            tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
                                style: None,
                                text: "Hi".to_owned(),
                                r#type: tl_enums::ButtonType::Default,
                            }),
                            tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
                                style: None,
                                text: "Phone".to_owned(),
                                r#type: tl_enums::ButtonType::RequestPhone,
                            }),
                            tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
                                style: None,
                                text: "Loc".to_owned(),
                                r#type: tl_enums::ButtonType::RequestGeoLocation,
                            }),
                            tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
                                style: None,
                                text: "Poll".to_owned(),
                                r#type: tl_enums::ButtonType::RequestPoll(
                                    tl_types::ButtonTypeRequestPoll { quiz: Some(true) },
                                ),
                            }),
                            tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
                                style: None,
                                text: "Pick Group".to_owned(),
                                r#type: tl_enums::ButtonType::RequestPeer(
                                    tl_types::ButtonTypeRequestPeer {
                                        button_id: 9,
                                        peer_type: tl_enums::RequestPeerType::Chat(
                                            tl_types::RequestPeerTypeChat {
                                                creator: true,
                                                bot_participant: false,
                                                has_username: Some(true),
                                                forum: Some(false),
                                                user_admin_rights: None,
                                                bot_admin_rights: None,
                                            },
                                        ),
                                        max_quantity: 2,
                                    },
                                ),
                            }),
                            tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
                                style: None,
                                text: "New Bot".to_owned(),
                                r#type: tl_enums::ButtonType::RequestPeer(
                                    tl_types::ButtonTypeRequestPeer {
                                        button_id: 10,
                                        peer_type: tl_enums::RequestPeerType::CreateBot(
                                            tl_types::RequestPeerTypeCreateBot {
                                                bot_managed: true,
                                                suggested_name: Some("Helper".to_owned()),
                                                suggested_username: Some("helper_bot".to_owned()),
                                            },
                                        ),
                                        max_quantity: 1,
                                    },
                                ),
                            }),
                            tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
                                style: None,
                                text: "Mini".to_owned(),
                                r#type: tl_enums::ButtonType::SimpleWebView(
                                    tl_types::ButtonTypeSimpleWebView {
                                        url: "https://mini.example.org".to_owned(),
                                    },
                                ),
                            }),
                        ],
                    },
                )],
                placeholder: Some("Type here".to_owned()),
            });

        let ReplyMarkup::Keyboard {
            rows: kb_rows,
            resize,
            single_use,
            persistent,
            placeholder,
            ..
        } = reply_markup_from_tl(&reply_kb_tl)
        else {
            panic!("expected reply keyboard");
        };
        assert!(resize && single_use && persistent);
        assert_eq!(placeholder, "Type here");
        assert_eq!(kb_rows[0].len(), 7);
        assert!(matches!(
            &kb_rows[0][4].kind,
            KeyboardButtonKind::RequestPeer { button_id: 9, query }
                if query.peer_type == RequestPeerType::Group
                    && query.am_creator
                    && query.max_quantity == 2
                    && query.has_username == RequestPeerRestriction::Yes
                    && query.group_is_forum == RequestPeerRestriction::No
        ));
        assert!(matches!(
            &kb_rows[0][5].kind,
            KeyboardButtonKind::CreateBot {
                button_id: 10,
                suggested_name,
                suggested_username
            } if suggested_name == "Helper" && suggested_username == "helper_bot"
        ));

        assert_eq!(
            reply_markup_from_tl(&tl_enums::ReplyMarkup::ReplyKeyboardForceReply(
                tl_types::ReplyKeyboardForceReply {
                    single_use: true,
                    selective: true,
                    placeholder: Some("Reply!".to_owned()),
                }
            )),
            ReplyMarkup::ForceReply {
                single_use: true,
                selective: true,
                placeholder: "Reply!".to_owned(),
            }
        );

        assert_eq!(
            reply_markup_from_tl(&tl_enums::ReplyMarkup::ReplyKeyboardHide(
                tl_types::ReplyKeyboardHide { selective: true }
            )),
            ReplyMarkup::RemoveKeyboard { selective: true }
        );
    }
}
