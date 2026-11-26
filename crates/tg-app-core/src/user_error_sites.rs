//! Per-request failure handlers of the window, deep links and media viewer, each a port of the
//! tdesktop `.fail()` handler it cites (TGB-2957). Paths are under `Telegram/SourceFiles/`.

use tg_lang::tr;

use super::{
    ApiFailure, LimitBoxKind, UserFacingError, channels_limit_box, error_type_toast, plain, silent,
    spambot_inform,
};

fn toast(text: String) -> UserFacingError {
    UserFacingError::Toast(text)
}

fn inform(text: String) -> UserFacingError {
    UserFacingError::Inform { title: None, text }
}

/// The bare type for an RPC error unless `MTP::IgnoreError` (`406`); nothing otherwise.
fn error_type_unless_ignored(failure: &ApiFailure) -> UserFacingError {
    match failure.rpc() {
        Some(rpc) if !rpc.is_ignored() => toast(rpc.name.clone()),
        _ => UserFacingError::Silent,
    }
}

/// Opening a photo or file in the media viewer or an external app (`upload.getFile`, then a
/// local launch). tdesktop has no error UI for either (`data/data_document.cpp:1352-1380`,
/// `platform/win/file_utilities_win.cpp:298-301`).
#[must_use]
pub fn open_media(raw: &str) -> UserFacingError {
    silent("open media", raw)
}

/// `messages.getAttachedStickers` (`api/api_attached_stickers.cpp:65-70`): any error is an
/// inform box "Sticker set not found."
#[must_use]
pub fn attached_stickers(raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse("messages.getAttachedStickers", raw);
    inform(tr::lng_stickers_not_found())
}

/// Forwarding from the media viewer (`boxes/share_box.cpp:2081-2105`): two known errors get a
/// toast, anything else nothing. `peer_name` is the chat the forward went to.
#[must_use]
pub fn forward_media(raw: &str, peer_name: &str) -> UserFacingError {
    let failure = ApiFailure::parse("messages.forwardMessages", raw);
    match failure.error_type() {
        // tdesktop's literal, untranslated text.
        Some(name) if name.starts_with("ALLOW_PAYMENT_REQUIRED_") => {
            toast("Payment requirements changed. Please, try again.".to_owned())
        }
        Some("VOICE_MESSAGES_FORBIDDEN") => {
            toast(tr::lng_restricted_send_voice_messages(peer_name))
        }
        _ => UserFacingError::Silent,
    }
}

/// Saving a confirmed proxy: a local settings write with no failure path in tdesktop
/// (`boxes/connection_box.cpp:2497-2512`).
#[must_use]
pub fn apply_proxy(raw: &str) -> UserFacingError {
    silent("apply proxy", raw)
}

/// `chatlists.joinChatlistInvite` (`ShowImportError`, `api/api_chat_filters.cpp:662-685`): the
/// channels, folder-chats and shared-folders limit boxes, or a toast.
#[must_use]
pub fn join_chatlist(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("chatlists.joinChatlistInvite", raw);
    match failure.error_type() {
        Some("CHANNELS_TOO_MUCH") => channels_limit_box(),
        Some("FILTER_INCLUDE_TOO_MUCH") => UserFacingError::LimitBox(LimitBoxKind::FilterChats),
        Some("CHATLISTS_TOO_MUCH") => UserFacingError::LimitBox(LimitBoxKind::ShareableFilters),
        Some("INVITE_SLUG_EXPIRED") => toast(tr::lng_group_invite_bad_link()),
        Some(name) if name.starts_with("FLOOD_WAIT_") => toast(tr::lng_flood_error()),
        Some(name) => toast(name.to_owned()),
        None => UserFacingError::Silent,
    }
}

/// `chatlists.checkChatlistInvite` (`api/api_chat_filters.cpp:752-754`): a `400` is a toast.
#[must_use]
pub fn check_chatlist_invite(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("chatlists.checkChatlistInvite", raw);
    if failure.code() == Some(400) {
        toast(tr::lng_group_invite_bad_link())
    } else {
        UserFacingError::Silent
    }
}

/// Switching the language (`lang/lang_cloud_manager.cpp:241-249`, `465-469`): an unsupported
/// code is an inform box, anything else nothing.
#[must_use]
pub fn switch_language(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("langpack.getLanguage", raw);
    if failure.is("LANG_CODE_NOT_SUPPORTED") {
        inform(tr::lng_language_not_found())
    } else {
        UserFacingError::Silent
    }
}

/// `payments.applyGiftCode` (`boxes/gift_premium_box.cpp:958-975`): an active Premium
/// subscription gets the "already have Premium" toast with its end date
/// ([`already_premium_toast`]), anything else the error type.
#[must_use]
pub fn apply_gift_code(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("payments.applyGiftCode", raw);
    match failure.error_type() {
        Some(name) if name.starts_with("PREMIUM_SUB_ACTIVE_UNTIL_") => already_premium_toast(
            name["PREMIUM_SUB_ACTIVE_UNTIL_".len()..]
                .parse()
                .unwrap_or_default(),
        ),
        Some(name) => toast(name.to_owned()),
        None => UserFacingError::Silent,
    }
}

/// `ShowAlreadyPremiumToast` (`boxes/gift_premium_box.cpp:798-829`): the title, then "You can
/// activate this link after {date} or send the link to a friend." with `langDateTime(date)`.
/// The toast here is plain text: "send the link" is not a link yet (it opens the recipient
/// chooser in tdesktop).
#[must_use]
pub fn already_premium_toast(until_unix: i32) -> UserFacingError {
    let at = tg_history::datetime::LocalDateTime::from_unix_local(i64::from(until_unix.max(0)));
    let date = format!(
        "{:02}.{:02}.{:02}",
        at.date.day,
        at.date.month,
        at.date.year.rem_euclid(100)
    );
    let time = tg_history::datetime::format_time_short(at);
    let about = tr::lng_gift_link_already_about(
        &tr::lng_mediaview_date_time(&date, &time),
        &tr::lng_gift_link_already_link(),
    );
    toast(format!(
        "{}\n{}",
        tr::lng_gift_link_already_title(),
        plain(&about)
    ))
}

/// `payments.checkGiftCode` (`boxes/gift_premium_box.cpp:1086-1090`): any error is the
/// "link expired" toast.
#[must_use]
pub fn check_gift_code(raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse("payments.checkGiftCode", raw);
    toast(tr::lng_gift_link_expired())
}

/// `messages.getStickerSet` / `messages.installStickerSet` from the sticker set box
/// (`boxes/sticker_set_box.cpp:834-846`): any error is an inform box "Sticker set not found."
#[must_use]
pub fn sticker_set(raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse("messages.getStickerSet", raw);
    inform(tr::lng_stickers_not_found())
}

/// `messages.uninstallStickerSet` has no failure handler
/// (`chat_helpers/stickers_list_widget.cpp:4510-4519`).
#[must_use]
pub fn uninstall_sticker_set(raw: &str) -> UserFacingError {
    silent("messages.uninstallStickerSet", raw)
}

/// Owner edits of a sticker set: rename, reorder, remove a sticker, delete the set
/// (`boxes/sticker_set_box.cpp:360-362`, `1611-1613`, `1953-1956`, `930-932`): the error type.
#[must_use]
pub fn sticker_set_owner_edit(raw: &str) -> UserFacingError {
    error_type_toast("stickers.* owner edit", raw)
}

/// `aicompose.saveTone` (`boxes/create_ai_tone_box.cpp:633-637`, `717-749`): the saved-styles
/// limit gets its own toast; other errors the type unless `406`. Removing a saved style has no
/// failure handler (`create_ai_tone_box.cpp:696-699`).
#[must_use]
pub fn save_ai_tone(raw: &str, unsave: bool, is_premium: bool) -> UserFacingError {
    let failure = ApiFailure::parse("aicompose.saveTone", raw);
    if unsave {
        return UserFacingError::Silent;
    }
    if failure.is("TONES_SAVED_TOO_MANY") {
        // `aicompose_tone_saved_limit_default` / `_premium` app config defaults.
        let (default_limit, premium_limit) = (5, 20);
        return toast(plain(&if is_premium {
            tr::lng_ai_compose_tone_saved_limit_final(premium_limit)
        } else {
            tr::lng_ai_compose_tone_saved_limit(
                default_limit,
                &tr::lng_ai_compose_tone_saved_limit_link(),
                &premium_limit.to_string(),
            )
        }));
    }
    error_type_unless_ignored(&failure)
}

/// `aicompose.getTone` from a `t.me/addstyle` link (`core/local_url_handlers.cpp:280-314`).
#[must_use]
pub fn resolve_ai_tone(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("aicompose.getTone", raw);
    if failure.is("AICOMPOSE_TONE_SLUG_INVALID") {
        toast(tr::lng_ai_compose_tone_invalid())
    } else {
        error_type_unless_ignored(&failure)
    }
}

/// `account.getTheme` (`data/data_cloud_themes.cpp:353-362`).
#[must_use]
pub fn resolve_cloud_theme(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("account.getTheme", raw);
    if failure.is("THEME_FORMAT_INVALID") {
        inform(tr::lng_theme_no_desktop())
    } else {
        UserFacingError::Silent
    }
}

/// `account.getWallPaper` (`boxes/background_preview_box.cpp:1156-1167`): any error is an
/// inform box.
#[must_use]
pub fn resolve_wallpaper(raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse("account.getWallPaper", raw);
    inform(tr::lng_background_bad_link())
}

/// `contacts.importNoPaidMessagesToken` (`window/window_no_paid_token.cpp:40-55`).
#[must_use]
pub fn import_no_paid_token(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("contacts.importNoPaidMessagesToken", raw);
    match failure.error_type() {
        Some("NO_PAID_MESSAGES_TOKEN_INVALID") => toast(tr::lng_no_paid_token_invalid()),
        Some("USER_ID_INVALID") => toast(tr::lng_no_paid_token_user_invalid()),
        _ if failure.is_flood() => toast(tr::lng_flood_error()),
        _ => error_type_unless_ignored(&failure),
    }
}

/// `contacts.resolveUsername` behind a link (`window/window_session_controller.cpp:591-600`):
/// an expired affiliate link and a `400` show text, anything else nothing.
#[must_use]
pub fn resolve_username(raw: &str, username: &str) -> UserFacingError {
    let failure = ApiFailure::parse("contacts.resolveUsername", raw);
    if failure.is("STARREF_EXPIRED") {
        toast(tr::lng_star_ref_stopped())
    } else if failure.code() == Some(400) {
        inform(tr::lng_username_not_found(username))
    } else {
        UserFacingError::Silent
    }
}

/// A private channel link the account cannot open (`window_session_controller.cpp:632-658`).
#[must_use]
pub fn private_channel_unavailable(raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse("private channel link", raw);
    toast(tr::lng_error_post_link_invalid())
}

/// `account.resolveBusinessChatLink` (`window/window_session_controller.cpp:565-569`).
#[must_use]
pub fn resolve_business_chat_link(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("account.resolveBusinessChatLink", raw);
    if failure.code() == Some(400) {
        toast(tr::lng_confirm_phone_link_invalid())
    } else {
        UserFacingError::Silent
    }
}

/// `messages.requestUrlAuth` for a `tg://oauth` link (`boxes/url_auth_box.cpp:346-353`): an
/// expired link is a toast; anything else shows no error.
#[must_use]
pub fn request_url_auth(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("messages.requestUrlAuth", raw);
    if failure.is("URL_EXPIRED") {
        toast(tr::lng_url_auth_phone_toast_bad_expired())
    } else {
        UserFacingError::Silent
    }
}

/// A request whose tdesktop counterpart has no failure handler at all (Business overview loads,
/// the bot-manager username lookup of `core/deep_links/deep_links_new.cpp:48-113`).
#[must_use]
pub fn no_handler(site: &str, raw: &str) -> UserFacingError {
    silent(site, raw)
}

/// Adding an account past the limit: tdesktop shows `AccountsLimitBox` before trying
/// (`settings/sections/settings_information.cpp:1027-1029`, `boxes/premium_limits_box.cpp`).
#[must_use]
pub fn accounts_limit(limit: usize) -> UserFacingError {
    UserFacingError::Inform {
        title: Some(tr::lng_accounts_limit_title()),
        text: plain(&format!(
            "{} {}",
            tr::lng_accounts_limit1(limit),
            tr::lng_accounts_limit2()
        )),
    }
}

/// What kind of chat a message went to, for the texts of [`send_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendTarget {
    /// A one-to-one chat.
    User,
    /// A basic group or supergroup.
    Group,
    /// A broadcast channel.
    Channel,
}

/// `ApiWrap::sendMessageFail` (`apiwrap.cpp:561-663`) for a message, sticker or GIF send.
/// The flood texts link "Learn more" to @SpamBot, `PREMIUM_ACCOUNT_REQUIRED` opens the Premium
/// screen. `BALANCE_TOO_LOW` opens the TON top-up box only for a dice stake, which Rustgram
/// cannot send; otherwise it is the error type, as in tdesktop.
#[must_use]
pub fn send_message(raw: &str, target: SendTarget) -> UserFacingError {
    let failure = ApiFailure::parse("messages.sendMedia", raw);
    let Some(name) = failure.error_type() else {
        return UserFacingError::Silent;
    };
    match name {
        "PEER_FLOOD" => spambot_inform(tr::lng_cant_send_to_not_contact),
        "USER_BANNED_IN_CHANNEL" => spambot_inform(tr::lng_error_public_groups_denied),
        "PREMIUM_ACCOUNT_REQUIRED" => UserFacingError::PremiumScreen {
            referral: "premium_stickers".to_owned(),
        },
        "SCHEDULE_STATUS_PRIVATE" => inform(tr::lng_cant_do_this()),
        "CHAT_FORWARDS_RESTRICTED" => toast(match target {
            SendTarget::Channel => tr::lng_error_noforwards_channel(),
            SendTarget::User => tr::lng_error_noforwards_user(),
            SendTarget::Group => tr::lng_error_noforwards_group(),
        }),
        "SCHEDULE_TOO_MUCH" => toast(tr::lng_error_schedule_limit()),
        _ if name.starts_with("SLOWMODE_WAIT_") || name.starts_with("FILE_REFERENCE_") => {
            UserFacingError::Silent
        }
        _ if name.starts_with("ALLOW_PAYMENT_REQUIRED_") => {
            toast("Payment requirements changed. Please, try again.".to_owned())
        }
        _ => toast(name.to_owned()),
    }
}
