//! TL (`tg-tl`) and domain (`tg-data`) conversion helpers for `tg-bots`.
//!
//! Port origin: `Telegram/SourceFiles/history/history_item_reply_markup.cpp`,
//! `Telegram/SourceFiles/api/api_bot.cpp`, `Telegram/SourceFiles/inline_bots/inline_bot_result.cpp`,
//! `Telegram/SourceFiles/inline_bots/bot_attach_web_view.cpp`,
//! `Telegram/SourceFiles/boxes/url_auth_box.cpp`, and `Telegram/SourceFiles/data/data_user.cpp`.

use tg_tl::{enums as tl_enums, types as tl_types};

use crate::callback::{CallbackAnswerInput, UrlAuthRequestData, UrlAuthResultInput};
use crate::commands::{
    BotAppColors, BotCommand, BotInfo, BotInfoUpdate, BotMenuButton, BotVerifierSettings,
};
use crate::inline::{
    InlineResult, InlineResultKind, InlineResultsPage, LocationPoint, MediaDimensions,
    SwitchPmButton,
};
use crate::markup::{
    AttachMenuPeerTypeInput, ButtonColor, ButtonKind, ButtonVisual, CreateBotRequest,
    InlineQueryPeerTypeInput, MarkupButton, PeerTypes, PollRequestRestriction, ReplyMarkupData,
    ReplyMarkupInput, RequestPeerQuery, RequestPeerRestriction, RequestPeerType,
    peer_types_from_attach_menu, peer_types_from_inline_query,
};
use crate::mini_app::{AttachMenuBot, GameData, GameHighScore, WebViewResultData};
use crate::protocol::{
    BotAppDescriptor, BotAppLookupResult, BotRecommendations, PopularAppBotsPage,
    RequestedWebViewButton,
};
use crate::stand_in::{
    BotAppId, ChatAdminRights, DocumentId, GameId, PeerId, PhotoId, Rgba, UserId,
};

// ============================================================================
// Admin Rights Conversions
// ============================================================================

/// Converts `tg_data::ChatAdminRights` into `tg_bots::stand_in::ChatAdminRights`,
/// mapping each flag explicitly (bit 19+ differ between `tg-data` and `tg-bots`).
#[must_use]
pub fn chat_admin_rights_from_data(rights: tg_data::ChatAdminRights) -> ChatAdminRights {
    let mut out = ChatAdminRights::empty();
    out.set(
        ChatAdminRights::CHANGE_INFO,
        rights.contains(tg_data::ChatAdminRights::CHANGE_INFO),
    );
    out.set(
        ChatAdminRights::POST_MESSAGES,
        rights.contains(tg_data::ChatAdminRights::POST_MESSAGES),
    );
    out.set(
        ChatAdminRights::EDIT_MESSAGES,
        rights.contains(tg_data::ChatAdminRights::EDIT_MESSAGES),
    );
    out.set(
        ChatAdminRights::DELETE_MESSAGES,
        rights.contains(tg_data::ChatAdminRights::DELETE_MESSAGES),
    );
    out.set(
        ChatAdminRights::BAN_USERS,
        rights.contains(tg_data::ChatAdminRights::BAN_USERS),
    );
    out.set(
        ChatAdminRights::INVITE_BY_LINK_OR_ADD,
        rights.contains(tg_data::ChatAdminRights::INVITE_USERS),
    );
    out.set(
        ChatAdminRights::PIN_MESSAGES,
        rights.contains(tg_data::ChatAdminRights::PIN_MESSAGES),
    );
    out.set(
        ChatAdminRights::ADD_ADMINS,
        rights.contains(tg_data::ChatAdminRights::ADD_ADMINS),
    );
    out.set(
        ChatAdminRights::ANONYMOUS,
        rights.contains(tg_data::ChatAdminRights::ANONYMOUS),
    );
    out.set(
        ChatAdminRights::MANAGE_CALL,
        rights.contains(tg_data::ChatAdminRights::MANAGE_CALL),
    );
    out.set(
        ChatAdminRights::OTHER,
        rights.contains(tg_data::ChatAdminRights::OTHER),
    );
    out.set(
        ChatAdminRights::MANAGE_TOPICS,
        rights.contains(tg_data::ChatAdminRights::MANAGE_TOPICS),
    );
    out.set(
        ChatAdminRights::POST_STORIES,
        rights.contains(tg_data::ChatAdminRights::POST_STORIES),
    );
    out.set(
        ChatAdminRights::EDIT_STORIES,
        rights.contains(tg_data::ChatAdminRights::EDIT_STORIES),
    );
    out.set(
        ChatAdminRights::DELETE_STORIES,
        rights.contains(tg_data::ChatAdminRights::DELETE_STORIES),
    );
    out.set(
        ChatAdminRights::MANAGE_DIRECT,
        rights.contains(tg_data::ChatAdminRights::MANAGE_DIRECT_MESSAGES),
    );
    out.set(
        ChatAdminRights::MANAGE_RANKS,
        rights.contains(tg_data::ChatAdminRights::MANAGE_RANKS),
    );
    out.set(
        ChatAdminRights::MANAGE_LINKED_PEERS,
        rights.contains(tg_data::ChatAdminRights::MANAGE_LINKED_PEERS),
    );
    out.set(
        ChatAdminRights::MANAGE_WELCOME_MESSAGES,
        rights.contains(tg_data::ChatAdminRights::MANAGE_WELCOME_MESSAGES),
    );
    out
}

/// Converts TL `ChatAdminRights` into `tg_bots::stand_in::ChatAdminRights`.
#[must_use]
pub fn chat_admin_rights_from_tl(rights: &tl_enums::ChatAdminRights) -> ChatAdminRights {
    let tl_enums::ChatAdminRights::ChatAdminRights(r) = rights;
    let mut out = ChatAdminRights::empty();
    out.set(ChatAdminRights::CHANGE_INFO, r.change_info);
    out.set(ChatAdminRights::POST_MESSAGES, r.post_messages);
    out.set(ChatAdminRights::EDIT_MESSAGES, r.edit_messages);
    out.set(ChatAdminRights::DELETE_MESSAGES, r.delete_messages);
    out.set(ChatAdminRights::BAN_USERS, r.ban_users);
    out.set(ChatAdminRights::INVITE_BY_LINK_OR_ADD, r.invite_users);
    out.set(ChatAdminRights::PIN_MESSAGES, r.pin_messages);
    out.set(ChatAdminRights::ADD_ADMINS, r.add_admins);
    out.set(ChatAdminRights::ANONYMOUS, r.anonymous);
    out.set(ChatAdminRights::MANAGE_CALL, r.manage_call);
    out.set(ChatAdminRights::OTHER, r.other);
    out.set(ChatAdminRights::MANAGE_TOPICS, r.manage_topics);
    out.set(ChatAdminRights::POST_STORIES, r.post_stories);
    out.set(ChatAdminRights::EDIT_STORIES, r.edit_stories);
    out.set(ChatAdminRights::DELETE_STORIES, r.delete_stories);
    out.set(ChatAdminRights::MANAGE_DIRECT, r.manage_direct_messages);
    out.set(ChatAdminRights::MANAGE_RANKS, r.manage_ranks);
    out.set(ChatAdminRights::MANAGE_LINKED_PEERS, r.manage_linked_peers);
    out.set(
        ChatAdminRights::MANAGE_WELCOME_MESSAGES,
        r.manage_welcome_messages,
    );
    out
}

// ============================================================================
// Reply Markup Conversions (`tg-data` -> `tg-bots` & `tg-tl` -> `tg-bots`)
// ============================================================================

fn button_color_from_data(c: tg_data::ButtonColor) -> ButtonColor {
    match c {
        tg_data::ButtonColor::Normal => ButtonColor::Normal,
        tg_data::ButtonColor::Primary => ButtonColor::Primary,
        tg_data::ButtonColor::Danger => ButtonColor::Danger,
        tg_data::ButtonColor::Success => ButtonColor::Success,
    }
}

fn button_visual_from_data(v: tg_data::ButtonVisual) -> ButtonVisual {
    ButtonVisual {
        icon_id: DocumentId(v.icon_id.0),
        color: button_color_from_data(v.color),
    }
}

fn peer_types_from_data(pt: tg_data::InlineQueryPeerTypes) -> PeerTypes {
    let mut out = PeerTypes::empty();
    out.set(
        PeerTypes::SAME_BOT,
        pt.contains(tg_data::InlineQueryPeerTypes::SAME_BOT),
    );
    out.set(
        PeerTypes::BOT,
        pt.contains(tg_data::InlineQueryPeerTypes::BOT),
    );
    out.set(
        PeerTypes::USER,
        pt.contains(tg_data::InlineQueryPeerTypes::USER),
    );
    out.set(
        PeerTypes::GROUP,
        pt.contains(tg_data::InlineQueryPeerTypes::GROUP),
    );
    out.set(
        PeerTypes::BROADCAST,
        pt.contains(tg_data::InlineQueryPeerTypes::BROADCAST),
    );
    out
}

fn request_peer_restriction_from_data(
    r: tg_data::RequestPeerRestriction,
) -> RequestPeerRestriction {
    match r {
        tg_data::RequestPeerRestriction::Any => RequestPeerRestriction::Any,
        tg_data::RequestPeerRestriction::Yes => RequestPeerRestriction::Yes,
        tg_data::RequestPeerRestriction::No => RequestPeerRestriction::No,
    }
}

fn request_peer_type_from_data(t: tg_data::RequestPeerType) -> RequestPeerType {
    match t {
        tg_data::RequestPeerType::User => RequestPeerType::User,
        tg_data::RequestPeerType::Group => RequestPeerType::Group,
        tg_data::RequestPeerType::Broadcast => RequestPeerType::Broadcast,
    }
}

fn poll_restriction_from_data(r: tg_data::PollRequestRestriction) -> PollRequestRestriction {
    match r {
        tg_data::PollRequestRestriction::Any => PollRequestRestriction::Any,
        tg_data::PollRequestRestriction::RegularOnly => PollRequestRestriction::RegularOnly,
        tg_data::PollRequestRestriction::QuizOnly => PollRequestRestriction::QuizOnly,
    }
}

fn request_peer_query_from_data(q: &tg_data::RequestPeerQuery) -> RequestPeerQuery {
    RequestPeerQuery {
        max_quantity: q.max_quantity,
        peer_type: request_peer_type_from_data(q.peer_type),
        user_is_bot: request_peer_restriction_from_data(q.user_is_bot),
        user_is_premium: request_peer_restriction_from_data(q.user_is_premium),
        group_is_forum: request_peer_restriction_from_data(q.group_is_forum),
        has_username: request_peer_restriction_from_data(q.has_username),
        am_creator: q.am_creator,
        is_bot_participant: q.is_bot_participant,
        my_rights: chat_admin_rights_from_data(q.my_rights),
        bot_rights: chat_admin_rights_from_data(q.bot_rights),
    }
}

fn markup_button_from_data(btn: &tg_data::KeyboardButton) -> MarkupButton {
    let kind = match &btn.kind {
        tg_data::KeyboardButtonKind::Default => ButtonKind::Default,
        tg_data::KeyboardButtonKind::RequestPhone => ButtonKind::RequestPhone,
        tg_data::KeyboardButtonKind::RequestLocation => ButtonKind::RequestLocation,
        tg_data::KeyboardButtonKind::RequestPoll { restriction } => ButtonKind::RequestPoll {
            restriction: poll_restriction_from_data(*restriction),
        },
        tg_data::KeyboardButtonKind::RequestPeer { button_id, query } => ButtonKind::RequestPeer {
            button_id: i64::from(*button_id),
            query: request_peer_query_from_data(query),
        },
        tg_data::KeyboardButtonKind::CreateBot {
            button_id,
            suggested_name,
            suggested_username,
        } => ButtonKind::CreateBot {
            button_id: i64::from(*button_id),
            request: CreateBotRequest {
                suggested_name: suggested_name.clone(),
                suggested_username: suggested_username.clone(),
            },
        },
        tg_data::KeyboardButtonKind::SimpleWebView { url } => {
            ButtonKind::SimpleWebView { url: url.clone() }
        }
        tg_data::KeyboardButtonKind::Url { url } => ButtonKind::Url { url: url.clone() },
        tg_data::KeyboardButtonKind::UrlAuth {
            url,
            forward_text,
            button_id,
        } => ButtonKind::Auth {
            url: url.clone(),
            forward_text: forward_text.clone(),
            button_id: i64::from(*button_id),
        },
        tg_data::KeyboardButtonKind::WebView { url } => ButtonKind::WebView { url: url.clone() },
        tg_data::KeyboardButtonKind::Callback { data } => {
            ButtonKind::Callback { data: data.clone() }
        }
        tg_data::KeyboardButtonKind::CallbackWithPassword { data } => {
            ButtonKind::CallbackWithPassword { data: data.clone() }
        }
        tg_data::KeyboardButtonKind::Game => ButtonKind::Game,
        tg_data::KeyboardButtonKind::Buy => ButtonKind::Buy,
        tg_data::KeyboardButtonKind::SwitchInline { query, peer_types } => {
            ButtonKind::SwitchInline {
                query: query.clone(),
                peer_types: peer_types_from_data(*peer_types),
            }
        }
        tg_data::KeyboardButtonKind::SwitchInlineSame { query } => ButtonKind::SwitchInlineSame {
            query: query.clone(),
        },
        tg_data::KeyboardButtonKind::UserProfile { user_id } => ButtonKind::UserProfile {
            user_id: UserId(user_id.0),
        },
        tg_data::KeyboardButtonKind::CopyText { text } => {
            ButtonKind::CopyText { text: text.clone() }
        }
        tg_data::KeyboardButtonKind::Disabled => ButtonKind::Disabled,
    };
    MarkupButton::with_visual(btn.text.clone(), button_visual_from_data(btn.visual), kind)
}

/// Converts a `tg_data::ReplyMarkup` into [`ReplyMarkupData`].
#[must_use]
pub fn reply_markup_from_data(markup: &tg_data::ReplyMarkup) -> ReplyMarkupData {
    let input = match markup {
        tg_data::ReplyMarkup::Inline { rows, force_reply } => ReplyMarkupInput::Inline {
            rows: rows
                .iter()
                .map(|row| row.iter().map(markup_button_from_data).collect())
                .collect(),
            force_reply: *force_reply,
        },
        tg_data::ReplyMarkup::Keyboard {
            rows,
            resize,
            single_use,
            selective,
            persistent,
            force_reply,
            placeholder,
        } => ReplyMarkupInput::Keyboard {
            rows: rows
                .iter()
                .map(|row| row.iter().map(markup_button_from_data).collect())
                .collect(),
            resize: *resize,
            single_use: *single_use,
            selective: *selective,
            persistent: *persistent,
            force_reply: *force_reply,
            placeholder: placeholder.clone(),
        },
        tg_data::ReplyMarkup::ForceReply {
            single_use,
            selective,
            placeholder,
        } => ReplyMarkupInput::ForceReply {
            single_use: *single_use,
            selective: *selective,
            placeholder: placeholder.clone(),
        },
        tg_data::ReplyMarkup::RemoveKeyboard { selective } => ReplyMarkupInput::RemoveKeyboard {
            selective: *selective,
        },
    };
    ReplyMarkupData::from_input(Some(input))
}

fn button_visual_from_tl(style: Option<&tl_enums::KeyboardButtonStyle>) -> ButtonVisual {
    let Some(tl_enums::KeyboardButtonStyle::KeyboardButtonStyle(s)) = style else {
        return ButtonVisual::default();
    };
    ButtonVisual {
        icon_id: DocumentId(s.icon.map_or(0, |v| v as u64)),
        color: ButtonColor::from_style_flags(s.bg_danger, s.bg_primary, s.bg_success),
    }
}

fn tl_inline_query_peer_types(types: &[tl_enums::InlineQueryPeerType]) -> PeerTypes {
    let mapped: Vec<InlineQueryPeerTypeInput> = types
        .iter()
        .map(|ty| match ty {
            tl_enums::InlineQueryPeerType::SameBotPM => InlineQueryPeerTypeInput::SameBotPm,
            tl_enums::InlineQueryPeerType::PM => InlineQueryPeerTypeInput::Pm,
            tl_enums::InlineQueryPeerType::Chat => InlineQueryPeerTypeInput::Chat,
            tl_enums::InlineQueryPeerType::Megagroup => InlineQueryPeerTypeInput::Megagroup,
            tl_enums::InlineQueryPeerType::Broadcast => InlineQueryPeerTypeInput::Broadcast,
            tl_enums::InlineQueryPeerType::BotPM => InlineQueryPeerTypeInput::BotPm,
        })
        .collect();
    peer_types_from_inline_query(&mapped)
}

fn tl_request_peer_query(req: &tl_types::ButtonTypeRequestPeer) -> RequestPeerQuery {
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
                query.my_rights = chat_admin_rights_from_tl(rights);
            }
            if let Some(ref rights) = c.bot_admin_rights {
                query.bot_rights = chat_admin_rights_from_tl(rights);
            }
        }
        tl_enums::RequestPeerType::Broadcast(b) => {
            query.peer_type = RequestPeerType::Broadcast;
            query.am_creator = b.creator;
            query.has_username = RequestPeerRestriction::from_option(b.has_username);
            if let Some(ref rights) = b.user_admin_rights {
                query.my_rights = chat_admin_rights_from_tl(rights);
            }
            if let Some(ref rights) = b.bot_admin_rights {
                query.bot_rights = chat_admin_rights_from_tl(rights);
            }
        }
        tl_enums::RequestPeerType::CreateBot(_) => {}
    }
    query
}

fn markup_button_from_tl_keyboard(btn: &tl_enums::KeyboardButton) -> Option<MarkupButton> {
    let tl_enums::KeyboardButton::KeyboardButton(b) = btn;
    let visual = button_visual_from_tl(b.style.as_ref());
    let kind = match &b.r#type {
        tl_enums::ButtonType::Default => ButtonKind::Default,
        tl_enums::ButtonType::RequestPhone => ButtonKind::RequestPhone,
        tl_enums::ButtonType::RequestGeoLocation => ButtonKind::RequestLocation,
        tl_enums::ButtonType::RequestPoll(p) => ButtonKind::RequestPoll {
            restriction: PollRequestRestriction::from_quiz_option(p.quiz),
        },
        tl_enums::ButtonType::RequestPeer(req) => match &req.peer_type {
            tl_enums::RequestPeerType::CreateBot(cb) => ButtonKind::CreateBot {
                button_id: i64::from(req.button_id),
                request: CreateBotRequest {
                    suggested_name: cb.suggested_name.clone().unwrap_or_default(),
                    suggested_username: cb.suggested_username.clone().unwrap_or_default(),
                },
            },
            _ => ButtonKind::RequestPeer {
                button_id: i64::from(req.button_id),
                query: tl_request_peer_query(req),
            },
        },
        tl_enums::ButtonType::InputButtonTypeRequestPeer(_) => return None,
        tl_enums::ButtonType::SimpleWebView(wv) => ButtonKind::SimpleWebView {
            url: wv.url.clone(),
        },
    };
    Some(MarkupButton::with_visual(b.text.clone(), visual, kind))
}

fn markup_button_from_tl_inline(btn: &tl_enums::KeyboardInlineButton) -> Option<MarkupButton> {
    let tl_enums::KeyboardInlineButton::KeyboardInlineButton(b) = btn;
    let visual = button_visual_from_tl(b.style.as_ref());
    let kind = match &b.r#type {
        tl_enums::InlineButtonType::Url(u) => ButtonKind::Url { url: u.url.clone() },
        tl_enums::InlineButtonType::UrlAuth(a) => ButtonKind::Auth {
            url: a.url.clone(),
            forward_text: a.fwd_text.clone().unwrap_or_default(),
            button_id: i64::from(a.button_id),
        },
        tl_enums::InlineButtonType::InputInlineButtonTypeUrlAuth(_) => return None,
        tl_enums::InlineButtonType::WebView(wv) => ButtonKind::WebView {
            url: wv.url.clone(),
        },
        tl_enums::InlineButtonType::Callback(cb) => {
            if cb.requires_password {
                ButtonKind::CallbackWithPassword {
                    data: cb.data.clone(),
                }
            } else {
                ButtonKind::Callback {
                    data: cb.data.clone(),
                }
            }
        }
        tl_enums::InlineButtonType::Game => ButtonKind::Game,
        tl_enums::InlineButtonType::Buy => ButtonKind::Buy,
        tl_enums::InlineButtonType::SwitchInline(sw) => {
            if sw.same_peer {
                ButtonKind::SwitchInlineSame {
                    query: sw.query.clone(),
                }
            } else {
                let peer_types = sw
                    .peer_types
                    .as_deref()
                    .map(tl_inline_query_peer_types)
                    .unwrap_or_default();
                ButtonKind::SwitchInline {
                    query: sw.query.clone(),
                    peer_types,
                }
            }
        }
        tl_enums::InlineButtonType::UserProfile(up) => ButtonKind::UserProfile {
            user_id: UserId(up.user_id as u64),
        },
        tl_enums::InlineButtonType::InputInlineButtonTypeUserProfile(_) => return None,
        tl_enums::InlineButtonType::Copy(c) => ButtonKind::CopyText {
            text: c.copy_text.clone(),
        },
        tl_enums::InlineButtonType::Disabled => ButtonKind::Disabled,
    };
    Some(MarkupButton::with_visual(b.text.clone(), visual, kind))
}

/// Converts a TL `ReplyMarkup` directly into [`ReplyMarkupData`].
#[must_use]
pub fn reply_markup_from_tl(markup: &tl_enums::ReplyMarkup) -> ReplyMarkupData {
    let input = match markup {
        tl_enums::ReplyMarkup::ReplyInlineMarkup(m) => ReplyMarkupInput::Inline {
            rows: m
                .rows
                .iter()
                .map(|row| {
                    let tl_enums::KeyboardInlineButtonRow::KeyboardInlineButtonRow(r) = row;
                    r.buttons
                        .iter()
                        .filter_map(markup_button_from_tl_inline)
                        .collect()
                })
                .collect(),
            force_reply: m.force_reply,
        },
        tl_enums::ReplyMarkup::ReplyKeyboardMarkup(m) => ReplyMarkupInput::Keyboard {
            rows: m
                .rows
                .iter()
                .map(|row| {
                    let tl_enums::KeyboardButtonRow::KeyboardButtonRow(r) = row;
                    r.buttons
                        .iter()
                        .filter_map(markup_button_from_tl_keyboard)
                        .collect()
                })
                .collect(),
            resize: m.resize,
            single_use: m.single_use,
            selective: m.selective,
            persistent: m.persistent,
            force_reply: m.force_reply,
            placeholder: m.placeholder.clone().unwrap_or_default(),
        },
        tl_enums::ReplyMarkup::ReplyKeyboardForceReply(m) => ReplyMarkupInput::ForceReply {
            single_use: m.single_use,
            selective: m.selective,
            placeholder: m.placeholder.clone().unwrap_or_default(),
        },
        tl_enums::ReplyMarkup::ReplyKeyboardHide(m) => ReplyMarkupInput::RemoveKeyboard {
            selective: m.selective,
        },
    };
    ReplyMarkupData::from_input(Some(input))
}

// ============================================================================
// Callback Answer & URL Auth Conversions
// ============================================================================

/// Converts TL `messages.BotCallbackAnswer` into [`CallbackAnswerInput`].
#[must_use]
pub fn callback_answer_from_tl(ans: &tl_enums::messages::BotCallbackAnswer) -> CallbackAnswerInput {
    let tl_enums::messages::BotCallbackAnswer::BotCallbackAnswer(a) = ans;
    CallbackAnswerInput {
        alert: a.alert,
        has_url: a.has_url,
        native_ui: a.native_ui,
        message: a.message.clone(),
        url: a.url.clone(),
        cache_time: a.cache_time,
    }
}

/// Converts TL `UrlAuthResult` into [`UrlAuthResultInput`].
#[must_use]
pub fn url_auth_result_from_tl(res: &tl_enums::UrlAuthResult) -> UrlAuthResultInput {
    match res {
        tl_enums::UrlAuthResult::Accepted(a) => UrlAuthResultInput::Accepted { url: a.url.clone() },
        tl_enums::UrlAuthResult::Default => UrlAuthResultInput::Default,
        tl_enums::UrlAuthResult::Request(r) => {
            let (bot_id, bot_name) = match &r.bot {
                tl_enums::User::User(u) => {
                    let first = u.first_name.as_deref().unwrap_or("");
                    let last = u.last_name.as_deref().unwrap_or("");
                    let name = if !first.is_empty() && !last.is_empty() {
                        format!("{first} {last}")
                    } else if !first.is_empty() {
                        first.to_owned()
                    } else {
                        u.username.clone().unwrap_or_default()
                    };
                    (UserId(u.id as u64), name)
                }
                tl_enums::User::Empty(e) => (UserId(e.id as u64), String::new()),
            };
            UrlAuthResultInput::Request(UrlAuthRequestData {
                domain: r.domain.clone(),
                bot_id,
                bot_name,
                request_write_access: r.request_write_access,
                request_phone_number: r.request_phone_number,
                match_codes_first: r.match_codes_first,
                is_app: r.is_app,
                verified_app_name: r.verified_app_name.clone(),
                user_id_hint: r.user_id_hint.map(|id| UserId(id as u64)),
                match_codes: r.match_codes.clone().unwrap_or_default(),
                browser: r.browser.clone(),
                platform: r.platform.clone(),
                ip: r.ip.clone(),
                region: r.region.clone(),
            })
        }
    }
}

// ============================================================================
// Inline Bot Results Conversions
// ============================================================================

fn extract_inline_message_meta(
    msg: &tl_enums::BotInlineMessage,
) -> (bool, Option<LocationPoint>, Option<ReplyMarkupData>) {
    let is_widget = matches!(msg, tl_enums::BotInlineMessage::Widget(_));
    let location = match msg {
        tl_enums::BotInlineMessage::MediaGeo(g) => geo_point_to_location(&g.geo),
        tl_enums::BotInlineMessage::MediaVenue(v) => geo_point_to_location(&v.geo),
        _ => None,
    };
    let tl_markup = match msg {
        tl_enums::BotInlineMessage::MediaAuto(m) => m.reply_markup.as_ref(),
        tl_enums::BotInlineMessage::Text(m) => m.reply_markup.as_ref(),
        tl_enums::BotInlineMessage::MediaGeo(m) => m.reply_markup.as_ref(),
        tl_enums::BotInlineMessage::MediaVenue(m) => m.reply_markup.as_ref(),
        tl_enums::BotInlineMessage::MediaContact(m) => m.reply_markup.as_ref(),
        tl_enums::BotInlineMessage::MediaInvoice(m) => m.reply_markup.as_ref(),
        tl_enums::BotInlineMessage::MediaWebPage(m) => m.reply_markup.as_ref(),
        tl_enums::BotInlineMessage::RichMessage(m) => m.reply_markup.as_ref(),
        tl_enums::BotInlineMessage::Widget(m) => m.reply_markup.as_ref(),
    };
    (is_widget, location, tl_markup.map(reply_markup_from_tl))
}

fn geo_point_to_location(geo: &tl_enums::GeoPoint) -> Option<LocationPoint> {
    match geo {
        tl_enums::GeoPoint::Empty => None,
        tl_enums::GeoPoint::GeoPoint(g) => Some(LocationPoint {
            lat: g.lat,
            lon: g.long,
            access_hash: g.access_hash,
        }),
    }
}

fn web_doc_url_and_meta(doc: &tl_enums::WebDocument) -> (String, MediaDimensions, Option<i32>) {
    let (url, attrs) = match doc {
        tl_enums::WebDocument::WebDocument(d) => (d.url.clone(), d.attributes.as_slice()),
        tl_enums::WebDocument::NoProxy(d) => (d.url.clone(), d.attributes.as_slice()),
    };
    let (dims, duration) = attrs_dimensions_and_duration(attrs);
    (url, dims, duration)
}

fn attrs_dimensions_and_duration(
    attrs: &[tl_enums::DocumentAttribute],
) -> (MediaDimensions, Option<i32>) {
    let mut dims = MediaDimensions::default();
    let mut duration = None;
    for attr in attrs {
        match attr {
            tl_enums::DocumentAttribute::ImageSize(s) => {
                dims = MediaDimensions {
                    width: s.w,
                    height: s.h,
                };
            }
            tl_enums::DocumentAttribute::Video(v) => {
                dims = MediaDimensions {
                    width: v.w,
                    height: v.h,
                };
                duration = Some(v.duration.round() as i32);
            }
            tl_enums::DocumentAttribute::Audio(a) => {
                duration = Some(a.duration);
            }
            _ => {}
        }
    }
    (dims, duration)
}

fn photo_dimensions(photo: &tl_enums::Photo) -> (Option<PhotoId>, MediaDimensions) {
    let tl_enums::Photo::Photo(p) = photo else {
        return (None, MediaDimensions::default());
    };
    let mut best = MediaDimensions::default();
    let mut best_area = 0i64;
    for sz in &p.sizes {
        let (w, h) = match sz {
            tl_enums::PhotoSize::PhotoSize(s) => (s.w, s.h),
            tl_enums::PhotoSize::PhotoCachedSize(s) => (s.w, s.h),
            tl_enums::PhotoSize::Progressive(s) => (s.w, s.h),
            _ => continue,
        };
        let area = i64::from(w) * i64::from(h);
        if area >= best_area {
            best_area = area;
            best = MediaDimensions {
                width: w,
                height: h,
            };
        }
    }
    (Some(PhotoId(p.id as u64)), best)
}

/// Converts a single TL `BotInlineResult` into an [`InlineResult`].
#[must_use]
pub fn bot_inline_result_from_tl(
    query_id: u64,
    item: &tl_enums::BotInlineResult,
) -> Option<InlineResult> {
    match item {
        tl_enums::BotInlineResult::BotInlineResult(r) => {
            let (is_widget, location, reply_markup) = extract_inline_message_meta(&r.send_message);
            let kind = InlineResultKind::from_tl_type(&r.r#type, is_widget)?;
            let (content_url, content_dims, content_dur) = r
                .content
                .as_ref()
                .map(web_doc_url_and_meta)
                .unwrap_or_else(|| (String::new(), MediaDimensions::default(), None));
            let (thumb_dims, thumb_dur) = r
                .thumb
                .as_ref()
                .map(|t| {
                    let (_, d, dur) = web_doc_url_and_meta(t);
                    (d, dur)
                })
                .unwrap_or((MediaDimensions::default(), None));
            let dimensions = if content_dims.width > 0 && content_dims.height > 0 {
                content_dims
            } else {
                thumb_dims
            };
            let result = InlineResult {
                query_id,
                id: r.id.clone(),
                kind,
                title: r.title.clone().unwrap_or_default(),
                description: r.description.clone().unwrap_or_default(),
                url: r.url.clone().unwrap_or_default(),
                content_url,
                photo_id: None,
                document_id: None,
                dimensions,
                has_thumbnail: r.thumb.is_some(),
                location,
                duration_secs: content_dur.or(thumb_dur),
                reply_markup,
            };
            Some(result)
        }
        tl_enums::BotInlineResult::BotInlineMediaResult(r) => {
            let (is_widget, location, reply_markup) = extract_inline_message_meta(&r.send_message);
            let kind = InlineResultKind::from_tl_type(&r.r#type, is_widget)?;
            let (photo_id, photo_dims) = r
                .photo
                .as_ref()
                .map(photo_dimensions)
                .unwrap_or((None, MediaDimensions::default()));
            let (document_id, doc_dims, duration_secs, has_doc_thumb) = match r.document.as_ref() {
                Some(tl_enums::Document::Document(d)) => {
                    let (dims, dur) = attrs_dimensions_and_duration(&d.attributes);
                    let has_thumb = d.thumbs.as_ref().is_some_and(|t| !t.is_empty());
                    (Some(DocumentId(d.id as u64)), dims, dur, has_thumb)
                }
                _ => (None, MediaDimensions::default(), None, false),
            };
            let dimensions = if doc_dims.width > 0 && doc_dims.height > 0 {
                doc_dims
            } else {
                photo_dims
            };
            let result = InlineResult {
                query_id,
                id: r.id.clone(),
                kind,
                title: r.title.clone().unwrap_or_default(),
                description: r.description.clone().unwrap_or_default(),
                url: String::new(),
                content_url: String::new(),
                photo_id,
                document_id,
                dimensions,
                has_thumbnail: photo_id.is_some() || has_doc_thumb,
                location,
                duration_secs,
                reply_markup,
            };
            result.is_valid().then_some(result)
        }
    }
}

/// Converts TL `messages.BotResults` into [`InlineResultsPage`].
#[must_use]
pub fn bot_results_from_tl(query: &str, res: &tl_enums::messages::BotResults) -> InlineResultsPage {
    let tl_enums::messages::BotResults::BotResults(r) = res;
    let query_id = r.query_id as u64;
    let switch_pm = if let Some(tl_enums::InlineBotSwitchPM::InlineBotSwitchPM(pm)) = &r.switch_pm {
        Some(SwitchPmButton::StartPm {
            text: pm.text.clone(),
            start_param: pm.start_param.clone(),
        })
    } else if let Some(tl_enums::InlineBotWebView::InlineBotWebView(wv)) = &r.switch_webview {
        Some(SwitchPmButton::WebView {
            text: wv.text.clone(),
            url: wv.url.clone(),
        })
    } else {
        None
    };

    let results = r
        .results
        .iter()
        .filter_map(|item| bot_inline_result_from_tl(query_id, item))
        .collect();

    InlineResultsPage {
        query: query.to_owned(),
        next_offset: r.next_offset.clone().filter(|s| !s.is_empty()),
        switch_pm,
        gallery: r.gallery,
        cache_time: r.cache_time,
        results,
    }
}

// ============================================================================
// Bot Commands, Menu Button, BotInfo, Attach Menu & Games Conversions
// ============================================================================

/// Converts TL `BotCommand` into [`BotCommand`].
#[must_use]
pub fn bot_command_from_tl(cmd: &tl_enums::BotCommand) -> BotCommand {
    let tl_enums::BotCommand::BotCommand(c) = cmd;
    BotCommand {
        command: c.command.clone(),
        description: c.description.clone(),
        ephemeral: c.ephemeral,
    }
}

/// Converts TL `BotMenuButton` into [`BotMenuButton`].
#[must_use]
pub fn bot_menu_button_from_tl(btn: &tl_enums::BotMenuButton) -> BotMenuButton {
    match btn {
        tl_enums::BotMenuButton::Default => BotMenuButton::Default,
        tl_enums::BotMenuButton::Commands => BotMenuButton::Commands,
        tl_enums::BotMenuButton::BotMenuButton(b) => BotMenuButton::WebApp {
            text: b.text.clone(),
            url: b.url.clone(),
        },
    }
}

/// Converts TL `BotAppSettings` into [`BotAppColors`].
#[must_use]
pub fn bot_app_colors_from_tl(settings: &tl_enums::BotAppSettings) -> BotAppColors {
    let tl_enums::BotAppSettings::BotAppSettings(s) = settings;
    BotAppColors {
        title_day: s.header_color.map(|c| Rgba::from_serialized_rgb(c as u32)),
        title_night: s
            .header_dark_color
            .map(|c| Rgba::from_serialized_rgb(c as u32)),
        body_day: s
            .background_color
            .map(|c| Rgba::from_serialized_rgb(c as u32)),
        body_night: s
            .background_dark_color
            .map(|c| Rgba::from_serialized_rgb(c as u32)),
    }
}

/// Converts TL `BotVerifierSettings` into [`BotVerifierSettings`].
#[must_use]
pub fn bot_verifier_settings_from_tl(
    settings: &tl_enums::BotVerifierSettings,
) -> BotVerifierSettings {
    let tl_enums::BotVerifierSettings::BotVerifierSettings(s) = settings;
    let custom_description = s
        .custom_description
        .as_ref()
        .map(|t| {
            let tl_enums::TextWithEntities::TextWithEntities(tw) = t;
            tw.text.clone()
        })
        .unwrap_or_default();
    BotVerifierSettings {
        icon_id: DocumentId(s.icon as u64),
        company: s.company.clone(),
        custom_description,
        can_modify_description: s.can_modify_custom_description,
    }
}

/// Converts TL `BotInfo` (from `UserFull.bot_info`) into [`BotInfoUpdate`].
#[must_use]
pub fn bot_info_update_from_tl(info: &tl_enums::BotInfo) -> BotInfoUpdate {
    let tl_enums::BotInfo::BotInfo(bi) = info;
    let description_photo_id = bi.description_photo.as_ref().and_then(|p| match p {
        tl_enums::Photo::Photo(p) => Some(PhotoId(p.id as u64)),
        tl_enums::Photo::Empty(_) => None,
    });
    let description_document_id = bi.description_document.as_ref().and_then(|d| match d {
        tl_enums::Document::Document(d) => Some(DocumentId(d.id as u64)),
        tl_enums::Document::Empty(_) => None,
    });
    let commands = bi
        .commands
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(bot_command_from_tl)
        .collect();
    let menu_button = bi
        .menu_button
        .as_ref()
        .map(bot_menu_button_from_tl)
        .unwrap_or_default();
    let app_colors = bi
        .app_settings
        .as_ref()
        .map(bot_app_colors_from_tl)
        .unwrap_or_default();
    let verifier_settings = bi
        .verifier_settings
        .as_ref()
        .map(bot_verifier_settings_from_tl);

    BotInfoUpdate {
        description: bi.description.clone().unwrap_or_default(),
        description_photo_id,
        description_document_id,
        commands,
        menu_button,
        privacy_policy_url: bi.privacy_policy_url.clone().unwrap_or_default(),
        app_colors,
        verifier_settings,
    }
}

/// Converts `tg_data::BotInfo` into `tg_bots::commands::BotInfo`.
#[must_use]
pub fn bot_info_from_data(data: &tg_data::BotInfo) -> BotInfo {
    let menu_button = if !data.menu_button_url.is_empty() && !data.menu_button_text.is_empty() {
        BotMenuButton::WebApp {
            text: data.menu_button_text.clone(),
            url: data.menu_button_url.clone(),
        }
    } else if !data.commands.is_empty() {
        BotMenuButton::Commands
    } else {
        BotMenuButton::Default
    };
    BotInfo {
        description: data.description.clone(),
        inline_placeholder: data.inline_placeholder.clone(),
        commands: data
            .commands
            .iter()
            .map(|c| BotCommand::new(c.command.clone(), c.description.clone()))
            .collect(),
        description_photo_id: data.photo_id.map(|p| PhotoId(p.0)),
        description_document_id: data.document_id.map(|d| DocumentId(d.0)),
        menu_button,
        privacy_policy_url: data.privacy_policy_url.clone(),
        app_colors: BotAppColors::default(),
        start_token: data.start_token.clone(),
        group_admin_rights: chat_admin_rights_from_data(data.group_admin_rights),
        channel_admin_rights: chat_admin_rights_from_data(data.channel_admin_rights),
        verifier_settings: None,
        version: data.version,
        description_version: 0,
        active_users: data.active_users,
        inited: data.inited,
        reads_all_history: data.reads_all_history,
        cant_join_groups: data.cant_join_groups,
        supports_attach_menu: data.supports_attach_menu,
        can_edit_information: data.can_edit_information,
        can_manage_emoji_status: false,
        supports_business: data.supports_business,
        has_main_app: data.has_main_app,
        user_creates_topics: data.user_creates_topics,
        can_manage_bots: data.can_manage_bots,
        supports_guest_chat: false,
    }
}

/// Converts TL `AttachMenuBot` into [`AttachMenuBot`].
///
/// Matches `ParseAttachBot` in `inline_bots/bot_attach_web_view.cpp`.
#[must_use]
pub fn attach_menu_bot_from_tl(bot: &tl_enums::AttachMenuBot) -> AttachMenuBot {
    let tl_enums::AttachMenuBot::AttachMenuBot(b) = bot;
    let peer_types_input: Vec<AttachMenuPeerTypeInput> = b
        .peer_types
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|pt| match pt {
            tl_enums::AttachMenuPeerType::SameBotPM => AttachMenuPeerTypeInput::SameBotPm,
            tl_enums::AttachMenuPeerType::BotPM => AttachMenuPeerTypeInput::BotPm,
            tl_enums::AttachMenuPeerType::PM => AttachMenuPeerTypeInput::Pm,
            tl_enums::AttachMenuPeerType::Chat => AttachMenuPeerTypeInput::Chat,
            tl_enums::AttachMenuPeerType::Broadcast => AttachMenuPeerTypeInput::Broadcast,
        })
        .collect();
    let icon_document_id = b
        .icons
        .iter()
        .find_map(|icon| {
            let tl_enums::AttachMenuBotIcon::AttachMenuBotIcon(i) = icon;
            if i.name == "default_static"
                && let tl_enums::Document::Document(doc) = &i.icon
            {
                Some(DocumentId(doc.id as u64))
            } else {
                None
            }
        })
        .or_else(|| {
            b.icons.iter().find_map(|icon| {
                let tl_enums::AttachMenuBotIcon::AttachMenuBotIcon(i) = icon;
                if let tl_enums::Document::Document(doc) = &i.icon {
                    Some(DocumentId(doc.id as u64))
                } else {
                    None
                }
            })
        });
    let uid = b.bot_id as u64;
    let data_peer = tg_data::PeerId::from_user(tg_data::UserId(uid));
    AttachMenuBot {
        bot_id: UserId(uid),
        bot_peer_id: PeerId(data_peer.0),
        icon_document_id,
        short_name: b.short_name.clone(),
        peer_types: peer_types_from_attach_menu(&peer_types_input),
        inactive: b.inactive,
        in_side_menu: b.show_in_side_menu,
        in_attach_menu: b.show_in_attach_menu,
        disclaimer_required: b.side_menu_disclaimer_needed,
        request_write_access: b.request_write_access,
    }
}

/// Converts TL `AttachMenuBots` into `Option<(hash, Vec<AttachMenuBot>)>`.
///
/// Returns `None` for `attachMenuBotsNotModified`.
#[must_use]
pub fn attach_menu_bots_from_tl(
    bots: &tl_enums::AttachMenuBots,
) -> Option<(i64, Vec<AttachMenuBot>)> {
    match bots {
        tl_enums::AttachMenuBots::NotModified => None,
        tl_enums::AttachMenuBots::AttachMenuBots(b) => {
            Some((b.hash, b.bots.iter().map(attach_menu_bot_from_tl).collect()))
        }
    }
}

/// Converts TL `WebViewResult` into [`WebViewResultData`].
#[must_use]
pub fn web_view_result_from_tl(res: &tl_enums::WebViewResult) -> WebViewResultData {
    let tl_enums::WebViewResult::Url(u) = res;
    WebViewResultData {
        url: u.url.clone(),
        query_id: u.query_id.map_or(0, |id| id as u64),
        fullscreen: u.fullscreen,
        fullsize: u.fullsize,
        same_origin: u.same_origin,
    }
}

/// Converts TL `messages.HighScores` into `Vec<GameHighScore>`.
#[must_use]
pub fn game_high_scores_from_tl(scores: &tl_enums::messages::HighScores) -> Vec<GameHighScore> {
    let tl_enums::messages::HighScores::HighScores(hs) = scores;
    hs.scores
        .iter()
        .map(|s| {
            let tl_enums::HighScore::HighScore(h) = s;
            GameHighScore {
                pos: h.pos,
                user_id: UserId(h.user_id as u64),
                score: h.score,
            }
        })
        .collect()
}

/// Converts `tg_data::GameData` into `tg_bots::mini_app::GameData`.
#[must_use]
pub fn game_data_from_data(data: &tg_data::GameData) -> GameData {
    GameData {
        id: GameId(data.id.0),
        access_hash: data.access_hash,
        short_name: data.short_name.clone(),
        title: data.title.clone(),
        description: data.description.clone(),
        photo_id: data.photo.as_ref().map(|p| PhotoId(p.id.0)),
        document_id: data.document.as_ref().map(|d| DocumentId(d.id.0)),
    }
}

// ============================================================================
// Bot App, Discovery & Requested WebView Button Conversions (WP-54)
// ============================================================================

/// Converts TL `messages.BotApp` (`messages.botApp#eb50adf5`) into [`BotAppLookupResult`].
///
/// Port of `Data::Session::processBotApp` (`data/data_session.cpp:4843-4862`) and
/// `WebViewInstance::resolveApp` (`inline_bots/bot_attach_web_view.cpp:1106-1124`).
#[must_use]
pub fn bot_app_from_tl(bot_id: UserId, res: &tl_enums::messages::BotApp) -> BotAppLookupResult {
    let tl_enums::messages::BotApp::BotApp(m) = res;
    let (app, not_modified) = match &m.app {
        tl_enums::BotApp::NotModified => (None, true),
        tl_enums::BotApp::BotApp(a) => {
            let photo_id = match &a.photo {
                tl_enums::Photo::Photo(p) => Some(PhotoId(p.id as u64)),
                tl_enums::Photo::Empty(_) => None,
            };
            let document_id = a.document.as_ref().and_then(|d| match d {
                tl_enums::Document::Document(doc) => Some(DocumentId(doc.id as u64)),
                tl_enums::Document::Empty(_) => None,
            });
            (
                Some(BotAppDescriptor {
                    id: BotAppId(a.id as u64),
                    access_hash: a.access_hash,
                    bot_id,
                    short_name: a.short_name.clone(),
                    title: a.title.clone(),
                    description: a.description.clone(),
                    photo_id,
                    document_id,
                    hash: a.hash,
                }),
                false,
            )
        }
    };
    BotAppLookupResult {
        app,
        not_modified,
        inactive: m.inactive,
        request_write_access: m.request_write_access,
        has_settings: m.has_settings,
    }
}

/// Converts TL `bots.PopularAppBots` (`bots.popularAppBots#1991b13b`) into [`PopularAppBotsPage`].
///
/// Port of `AttachWebView::loadPopularAppBots` (`inline_bots/bot_attach_web_view.cpp:2671-2680`):
/// filters out any non-bot user entries (`if (bot->isBot())`).
#[must_use]
pub fn popular_app_bots_from_tl(res: &tl_enums::bots::PopularAppBots) -> PopularAppBotsPage {
    let tl_enums::bots::PopularAppBots::PopularAppBots(p) = res;
    let bot_ids = p
        .users
        .iter()
        .filter_map(|u| match u {
            tl_enums::User::User(user) if user.bot => Some(UserId(user.id as u64)),
            _ => None,
        })
        .collect();
    PopularAppBotsPage {
        next_offset: p.next_offset.clone().filter(|s| !s.is_empty()),
        bot_ids,
    }
}

/// Converts TL `users.Users` returned by `bots.getBotRecommendations` into [`BotRecommendations`].
///
/// Port of `ParseSimilarBots` (`api/api_chat_participants.cpp:252-269`):
/// for `users.usersSlice`, `more_count = (count - users.len()).max(0)`.
#[must_use]
pub fn bot_recommendations_from_tl(res: &tl_enums::users::Users) -> BotRecommendations {
    let extract_id = |u: &tl_enums::User| match u {
        tl_enums::User::User(user) => UserId(user.id as u64),
        tl_enums::User::Empty(e) => UserId(e.id as u64),
    };
    match res {
        tl_enums::users::Users::Users(u) => BotRecommendations {
            bot_ids: u.users.iter().map(extract_id).collect(),
            more_count: 0,
        },
        tl_enums::users::Users::Slice(s) => {
            let bot_ids: Vec<UserId> = s.users.iter().map(extract_id).collect();
            let shown = i32::try_from(bot_ids.len()).unwrap_or(i32::MAX);
            let more_count = s.count.saturating_sub(shown).max(0);
            BotRecommendations {
                bot_ids,
                more_count,
            }
        }
    }
}

/// Converts TL `KeyboardButton` returned by `bots.getRequestedWebViewButton` into
/// [`RequestedWebViewButton`].
///
/// Port of `WebViewInstance::botRequestChat` (`inline_bots/bot_attach_web_view.cpp:2159-2224`).
#[must_use]
pub fn requested_webview_button_from_tl(btn: &tl_enums::KeyboardButton) -> RequestedWebViewButton {
    let tl_enums::KeyboardButton::KeyboardButton(b) = btn;
    match &b.r#type {
        tl_enums::ButtonType::RequestPeer(req) => match &req.peer_type {
            tl_enums::RequestPeerType::CreateBot(cb) => RequestedWebViewButton::CreateBot {
                button_id: req.button_id,
                request: CreateBotRequest {
                    suggested_name: cb.suggested_name.clone().unwrap_or_default(),
                    suggested_username: cb.suggested_username.clone().unwrap_or_default(),
                },
            },
            _ => RequestedWebViewButton::RequestPeer {
                button_id: req.button_id,
                query: tl_request_peer_query(req),
            },
        },
        _ => RequestedWebViewButton::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tl_inline_markup_converts_all_button_kinds() {
        let style = Some(tl_enums::KeyboardButtonStyle::KeyboardButtonStyle(
            tl_types::KeyboardButtonStyle {
                bg_primary: true,
                bg_danger: false,
                bg_success: false,
                icon: Some(9001),
            },
        ));
        let row1 = tl_enums::KeyboardInlineButtonRow::KeyboardInlineButtonRow(
            tl_types::KeyboardInlineButtonRow {
                buttons: vec![
                    tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                        tl_types::KeyboardInlineButton {
                            style: style.clone(),
                            text: "Visit".to_owned(),
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
                            text: "Confirm".to_owned(),
                            r#type: tl_enums::InlineButtonType::Callback(
                                tl_types::InlineButtonTypeCallback {
                                    requires_password: true,
                                    data: b"pay_1".to_vec(),
                                },
                            ),
                        },
                    ),
                    tl_enums::KeyboardInlineButton::KeyboardInlineButton(
                        tl_types::KeyboardInlineButton {
                            style: None,
                            text: "Share".to_owned(),
                            r#type: tl_enums::InlineButtonType::SwitchInline(
                                tl_types::InlineButtonTypeSwitchInline {
                                    same_peer: false,
                                    query: "cats".to_owned(),
                                    peer_types: Some(vec![
                                        tl_enums::InlineQueryPeerType::PM,
                                        tl_enums::InlineQueryPeerType::Megagroup,
                                    ]),
                                },
                            ),
                        },
                    ),
                ],
            },
        );
        let tl = tl_enums::ReplyMarkup::ReplyInlineMarkup(tl_types::ReplyInlineMarkup {
            force_reply: false,
            rows: vec![row1],
        });

        let parsed = reply_markup_from_tl(&tl);
        assert!(parsed.is_inline());
        assert!(
            parsed
                .flags
                .contains(crate::markup::ReplyMarkupFlags::HAS_SWITCH_INLINE)
        );
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].len(), 3);
        assert_eq!(parsed.rows[0][0].visual.color, ButtonColor::Primary);
        assert_eq!(parsed.rows[0][0].visual.icon_id, DocumentId(9001));
        assert!(matches!(
            &parsed.rows[0][1].kind,
            ButtonKind::CallbackWithPassword { data } if data == b"pay_1"
        ));
        assert!(matches!(
            &parsed.rows[0][2].kind,
            ButtonKind::SwitchInline { query, peer_types }
                if query == "cats" && *peer_types == (PeerTypes::USER | PeerTypes::GROUP)
        ));
    }

    #[test]
    fn tl_bot_results_converts_switch_pm_and_next_offset() {
        let tl = tl_enums::messages::BotResults::BotResults(tl_types::messages::BotResults {
            gallery: true,
            query_id: 777,
            next_offset: Some("page2".to_owned()),
            switch_pm: Some(tl_enums::InlineBotSwitchPM::InlineBotSwitchPM(
                tl_types::InlineBotSwitchPM {
                    text: "Sign in".to_owned(),
                    start_param: "auth".to_owned(),
                },
            )),
            switch_webview: None,
            results: vec![tl_enums::BotInlineResult::BotInlineResult(
                tl_types::BotInlineResult {
                    id: "res1".to_owned(),
                    r#type: "article".to_owned(),
                    title: Some("Hello".to_owned()),
                    description: Some("World".to_owned()),
                    url: Some("https://example.org/a".to_owned()),
                    thumb: None,
                    content: None,
                    send_message: tl_enums::BotInlineMessage::Text(
                        tl_types::BotInlineMessageText {
                            no_webpage: false,
                            invert_media: false,
                            message: "Sent text".to_owned(),
                            entities: None,
                            reply_markup: None,
                        },
                    ),
                },
            )],
            cache_time: 60,
            users: Vec::new(),
        });

        let page = bot_results_from_tl("hello", &tl);
        assert!(page.gallery);
        assert_eq!(page.next_offset.as_deref(), Some("page2"));
        assert_eq!(
            page.switch_pm,
            Some(SwitchPmButton::StartPm {
                text: "Sign in".to_owned(),
                start_param: "auth".to_owned(),
            })
        );
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].query_id, 777);
        assert_eq!(page.results[0].kind, InlineResultKind::Article);
    }

    #[test]
    fn data_markup_roundtrips_through_reply_markup_from_data() {
        let data_markup = tg_data::ReplyMarkup::Keyboard {
            rows: vec![vec![
                tg_data::KeyboardButton::new("Phone", tg_data::KeyboardButtonKind::RequestPhone),
                tg_data::KeyboardButton::new(
                    "Quiz",
                    tg_data::KeyboardButtonKind::RequestPoll {
                        restriction: tg_data::PollRequestRestriction::QuizOnly,
                    },
                ),
            ]],
            resize: true,
            single_use: true,
            selective: false,
            persistent: true,
            force_reply: false,
            placeholder: "Choose...".to_owned(),
        };
        let converted = reply_markup_from_data(&data_markup);
        assert!(!converted.is_inline());
        assert!(converted.is_resize());
        assert!(converted.is_single_use());
        assert!(converted.is_persistent());
        assert_eq!(converted.placeholder, "Choose...");
        assert_eq!(converted.rows[0].len(), 2);
        assert!(matches!(
            converted.rows[0][1].kind,
            ButtonKind::RequestPoll {
                restriction: PollRequestRestriction::QuizOnly
            }
        ));
    }

    #[test]
    fn tl_bot_app_popular_recommendations_and_requested_button_conversions() {
        let app_tl = tl_enums::messages::BotApp::BotApp(tl_types::messages::BotApp {
            inactive: true,
            request_write_access: true,
            has_settings: false,
            app: tl_enums::BotApp::BotApp(tl_types::BotApp {
                id: 500,
                access_hash: 99,
                short_name: "demo".to_owned(),
                title: "Demo App".to_owned(),
                description: "Desc".to_owned(),
                photo: tl_enums::Photo::Empty(tl_types::PhotoEmpty { id: 0 }),
                document: None,
                hash: 77,
            }),
        });
        let parsed = bot_app_from_tl(UserId(10), &app_tl);
        assert!(parsed.inactive && parsed.request_write_access && !parsed.not_modified);
        assert_eq!(parsed.app.as_ref().unwrap().id, BotAppId(500));

        let not_mod = tl_enums::messages::BotApp::BotApp(tl_types::messages::BotApp {
            inactive: false,
            request_write_access: false,
            has_settings: true,
            app: tl_enums::BotApp::NotModified,
        });
        let parsed_nm = bot_app_from_tl(UserId(10), &not_mod);
        assert!(parsed_nm.not_modified && parsed_nm.app.is_none());

        let recs = bot_recommendations_from_tl(&tl_enums::users::Users::Slice(
            tl_types::users::UsersSlice {
                count: 5,
                users: vec![tl_enums::User::Empty(tl_types::UserEmpty { id: 42 })],
            },
        ));
        assert_eq!(recs.bot_ids, vec![UserId(42)]);
        assert_eq!(recs.more_count, 4);

        let unsupported = requested_webview_button_from_tl(
            &tl_enums::KeyboardButton::KeyboardButton(tl_types::KeyboardButton {
                style: None,
                text: "Plain".to_owned(),
                r#type: tl_enums::ButtonType::Default,
            }),
        );
        assert_eq!(unsupported, RequestedWebViewButton::Unsupported);
    }
}
