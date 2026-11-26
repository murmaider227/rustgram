//! Failure handlers of the history, bots, settings, media, info, payments, calls and Business
//! screens, each a port of the tdesktop `.fail()` handler it cites (TGB-2957). Paths are under
//! `Telegram/SourceFiles/`.

use tg_lang::tr;

use super::{ApiFailure, LimitBoxKind, UserFacingError, error_type_toast, silent};

fn toast(text: String) -> UserFacingError {
    UserFacingError::Toast(text)
}

fn inform(text: String) -> UserFacingError {
    UserFacingError::Inform { title: None, text }
}

/// `MTP::IsFloodError` → `lng_flood_error`, `406` → nothing, anything else the error type: the
/// `ShowFreeToast` / share-theme handlers (`info/profile/info_profile_qr_link.cpp:85-91`,
/// `info/profile/info_profile_share_theme.cpp:26-32`).
fn flood_or_error_type(failure: &ApiFailure) -> UserFacingError {
    match failure.rpc() {
        Some(rpc) if rpc.is_flood_error() => toast(tr::lng_flood_error()),
        Some(rpc) if !rpc.is_ignored() => toast(rpc.name.clone()),
        _ => UserFacingError::Silent,
    }
}

/// The location for a "Share location" bot button is unavailable
/// (`api/api_bot.cpp:379-383`): an inform box.
#[must_use]
pub fn location_unavailable(raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse("geolocation", raw);
    inform(tr::lng_bot_share_location_unavailable())
}

/// Pressing a bot callback button (`api/api_bot.cpp:157-172`): no error is shown.
#[must_use]
pub fn bot_callback(raw: &str) -> UserFacingError {
    silent("messages.getBotCallbackAnswer", raw)
}

/// `messages.acceptUrlAuth` from a login-URL button (`boxes/url_auth_box.cpp:428-430`): no
/// error; the URL opens without authorization.
#[must_use]
pub fn accept_url_auth_button(raw: &str) -> UserFacingError {
    silent("messages.acceptUrlAuth", raw)
}

/// `messages.acceptUrlAuth` from a link (`boxes/url_auth_box.cpp:567-585`): a toast naming the
/// domain (title + text).
#[must_use]
pub fn accept_url_auth_link(raw: &str, domain: &str) -> UserFacingError {
    let failure = ApiFailure::parse("messages.acceptUrlAuth", raw);
    if failure.is("URL_EXPIRED") {
        return toast(tr::lng_url_auth_phone_toast_bad_expired());
    }
    toast(format!(
        "{}\n{}",
        tr::lng_url_auth_phone_toast_bad_title(),
        tr::lng_url_auth_phone_toast_bad(domain)
    ))
}

/// `messages.transcribeAudio` (`api/api_transcribes.cpp:219-231`): the failure is written into
/// the voice bubble ([`transcribe_bubble_text`]), with no toast or box.
#[must_use]
pub fn transcribe_audio(raw: &str) -> UserFacingError {
    silent("messages.transcribeAudio", raw)
}

/// The italic text a voice or round-video bubble shows after a failed transcription
/// (`history/view/media/history_view_document.cpp:533-538`): `MSG_VOICE_TOO_LONG` has its own
/// text, any other failure "Failed".
#[must_use]
pub fn transcribe_bubble_text(too_long: bool) -> String {
    if too_long {
        tr::lng_audio_transcribe_long()
    } else {
        tr::lng_attach_failed()
    }
}

/// `messages.getInlineBotResults` (`inline_bots/inline_results_widget.cpp:476-480`): nothing.
#[must_use]
pub fn inline_query(raw: &str) -> UserFacingError {
    silent("messages.getInlineBotResults", raw)
}

/// Theme and wallpaper requests whose tdesktop handlers show nothing: the cloud catalogs,
/// installing a theme, uploading/installing/resetting/removing a wallpaper, the chat theme,
/// gift themes (`data/data_cloud_themes.cpp:298-303`, `504-506`, `533-535`, `642-645`,
/// `boxes/background_box.cpp:336-371`, `boxes/background_preview_box.cpp:630-678`).
#[must_use]
pub fn theme_sync(site: &str, raw: &str) -> UserFacingError {
    silent(site, raw)
}

/// Saving an edited cloud theme (`window/themes/window_theme_editor_box.cpp:918-939`). Title and
/// link errors mark the fields in tdesktop; here they show the error type.
#[must_use]
pub fn save_cloud_theme(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("account.createTheme/updateTheme", raw);
    if failure.is("THEME_SLUG_OCCUPIED") {
        toast(tr::lng_create_channel_link_occupied())
    } else if failure.is("THEME_FILE_INVALID") {
        UserFacingError::Silent
    } else {
        error_type_toast("account.createTheme/updateTheme", raw)
    }
}

/// `messages.setShareTheme`, `contacts.exportContactToken`, `contacts.exportNoPaidMessagesToken`.
#[must_use]
pub fn share_theme_or_qr(site: &str, raw: &str) -> UserFacingError {
    flood_or_error_type(&ApiFailure::parse(site, raw))
}

/// `chatlists.exportChatlistInvite` (`boxes/filters/edit_filter_box.cpp:1076-1089`): the
/// shared-folders, folder-links and channels limit boxes.
#[must_use]
pub fn export_chatlist_invite(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("chatlists.exportChatlistInvite", raw);
    match failure.error_type() {
        Some("USER_CHANNELS_TOO_MUCH") => toast(tr::lng_filters_link_group_admin_error()),
        Some("CHATLISTS_TOO_MUCH") => UserFacingError::LimitBox(LimitBoxKind::ShareableFilters),
        Some("INVITES_TOO_MUCH") => UserFacingError::LimitBox(LimitBoxKind::FilterLinks),
        Some("CHANNELS_TOO_MUCH") => UserFacingError::LimitBox(LimitBoxKind::Channels),
        _ => UserFacingError::Silent,
    }
}

/// `messages.toggleDialogFilterTags` (`data/data_chat_filters.cpp:483-488`): no message.
#[must_use]
pub fn toggle_folder_tags(raw: &str) -> UserFacingError {
    silent("messages.toggleDialogFilterTags", raw)
}

/// An emoji set download (`storage/storage_cloud_blob.cpp:96-97`, `160`): "Failed".
#[must_use]
pub fn emoji_set_download(raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse("emoji set download", raw);
    toast(tr::lng_attach_failed())
}

/// `messages.getPreparedInlineMessage` (`inline_bots/bot_attach_web_view.cpp:2139-2141`): only
/// the Mini App hears about it.
#[must_use]
pub fn prepared_message(raw: &str) -> UserFacingError {
    silent("messages.getPreparedInlineMessage", raw)
}

/// `messages.requestWebView` / `requestMainWebView`
/// (`inline_bots/bot_attach_web_view.cpp:1271-1277`): the error type.
#[must_use]
pub fn open_mini_app(raw: &str) -> UserFacingError {
    error_type_toast("messages.requestWebView", raw)
}

/// `payments.getBankCardData` (`core/bank_card_click_handler.cpp:71-73`): the menu shows
/// "Failed".
#[must_use]
pub fn bank_card_data(raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse("payments.getBankCardData", raw);
    toast(tr::lng_attach_failed())
}

/// `account.confirmPhone` (`api/api_confirm_phone.cpp:129-141`): the error line under the code
/// field.
#[must_use]
pub fn confirm_phone(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("account.confirmPhone", raw);
    toast(if failure.is_flood() {
        tr::lng_flood_error()
    } else if failure.is("PHONE_CODE_EMPTY") || failure.is("PHONE_CODE_INVALID") {
        tr::lng_bad_code()
    } else {
        tg_lang::hardcoded::server_error().to_owned()
    })
}

/// `payments.sendPaymentForm` (`payments/payments_checkout_process.cpp:565-592`).
#[must_use]
pub fn send_payment_form(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("payments.sendPaymentForm", raw);
    let Some(id) = failure.error_type() else {
        return UserFacingError::Silent;
    };
    toast(match id {
        "INVOICE_ALREADY_PAID" => tr::lng_payments_already_paid(),
        "PAYMENT_FAILED" => tr::lng_payments_payment_failed(),
        "BOT_PRECHECKOUT_FAILED" => tr::lng_payments_precheckout_failed(),
        "BOT_PRECHECKOUT_TIMEOUT" => tr::lng_payments_precheckout_timeout(),
        "REQUESTED_INFO_INVALID"
        | "SHIPPING_OPTION_INVALID"
        | "PAYMENT_CREDENTIALS_INVALID"
        | "PAYMENT_CREDENTIALS_ID_INVALID" => format!(
            "{}\nError: {id}. Your card has not been billed.",
            tr::lng_payments_payment_failed()
        ),
        _ => format!("Error: {id}"),
    })
}

/// `payments.validateRequestedInfo` (`payments/payments_checkout_process.cpp:489-535`). Field
/// errors highlight fields in tdesktop; here they show `Error: <id>`.
#[must_use]
pub fn validate_requested_info(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("payments.validateRequestedInfo", raw);
    match failure.error_type() {
        Some("SHIPPING_BOT_TIMEOUT") => toast("Error: Bot Timeout!".to_owned()),
        Some("SHIPPING_NOT_AVAILABLE") => toast(tr::lng_payments_shipping_not_available()),
        Some(id) => toast(format!("Error: {id}")),
        None => UserFacingError::Silent,
    }
}

/// `payments.getPaymentForm` and the other payment screens' loads
/// (`payments/payments_checkout_process.cpp:478-487`).
#[must_use]
pub fn load_payment_screen(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("payments.getPaymentForm", raw);
    match failure.error_type() {
        Some("INVOICE_ALREADY_PAID") => toast(tr::lng_payments_already_paid()),
        Some(id) => toast(format!("Error: {id}")),
        None => UserFacingError::Silent,
    }
}

/// A 1:1 call failed: `error_type` is the RPC error name (`Call::handleRequestError`,
/// `calls/calls_call.cpp:1660-1679`) or a controller error (`handleControllerError`,
/// `1681-1698`). tdesktop shows an inform box; `user` is the other side's name.
#[must_use]
pub fn call_error(error_type: &str, user: &str) -> UserFacingError {
    tracing::debug!(target: "user_error", error_type, "call failed");
    match error_type {
        "USER_PRIVACY_RESTRICTED" => inform(tr::lng_call_error_not_available(user)),
        "PARTICIPANT_VERSION_OUTDATED" => inform(tr::lng_call_error_outdated(user)),
        "CALL_PROTOCOL_LAYER_INVALID" | "ERROR_INCOMPATIBLE" => {
            inform(tg_lang::hardcoded::call_error_incompatible().replace("{user}", user))
        }
        "ERROR_AUDIO_IO" => inform(tr::lng_call_error_audio_io()),
        // Other controller errors show nothing.
        name if name.starts_with("ERROR_") || name.is_empty() => UserFacingError::Silent,
        name => {
            // A request error is an RPC name; anything else is a local failure text.
            if name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                inform(name.to_owned())
            } else {
                UserFacingError::Silent
            }
        }
    }
}

/// Group-call requests with no failure UI in tdesktop: `messages.getDhConfig`,
/// `phone.createGroupCall`, `phone.createConferenceCall`, in-call messages, title and recording
/// edits (`calls/calls_instance.cpp:535-541`, `calls/group/calls_group_call.cpp:1320-1325`,
/// `1772-1777`, `2955-2995`, `calls/group/calls_group_messages.cpp:484-499`).
#[must_use]
pub fn call_silent(site: &str, raw: &str) -> UserFacingError {
    silent(site, raw)
}

/// `phone.joinGroupCall` (`calls/group/calls_group_call.cpp:1863-1886`, `joinFail`).
#[must_use]
pub fn join_group_call(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("phone.joinGroupCall", raw);
    match failure.error_type() {
        Some("CONF_WRITE_CHAIN_INVALID" | "GROUPCALL_SSRC_DUPLICATE_MUCH") => {
            UserFacingError::Silent
        }
        Some("GROUPCALL_FORBIDDEN" | "GROUPCALL_INVALID") => {
            toast(tr::lng_confcall_not_accessible())
        }
        Some(name) => toast(name.to_owned()),
        None => UserFacingError::Silent,
    }
}

/// `account.updateBusinessGreetingMessage` / `updateBusinessAwayMessage`
/// (`settings/business/settings_greeting.cpp:271-277`, `settings_away_message.cpp:353-358`).
#[must_use]
pub fn business_auto_message(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("account.updateBusinessGreeting/AwayMessage", raw);
    match failure.error_type() {
        Some("BUSINESS_RECIPIENTS_EMPTY") => toast(tr::lng_greeting_recipients_empty()),
        Some("SHORTCUT_INVALID") | None => UserFacingError::Silent,
        Some(name) => toast(name.to_owned()),
    }
}

/// `account.updateConnectedBot` (`data/business/data_business_chatbots.cpp:120-126`).
#[must_use]
pub fn business_chatbot(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("account.updateConnectedBot", raw);
    match failure.error_type() {
        Some("BUSINESS_RECIPIENTS_EMPTY") => toast(tr::lng_greeting_recipients_empty()),
        Some("BOT_BUSINESS_MISSING") => toast(tr::lng_chatbots_not_supported()),
        _ => UserFacingError::Silent,
    }
}

/// `account.updateBusinessLocation` / `updateBusinessIntro` and the chat-link saves: tdesktop's
/// fail handlers are empty (`settings/business/settings_location.cpp:281-285`,
/// `settings_chat_intro.cpp:633-638`, `api/api_chat_links.cpp`). Rustgram's own local check
/// texts (no RPC error inside) still show, since tdesktop prevents those inputs in the fields.
#[must_use]
pub fn business_silent(site: &str, raw: &str) -> UserFacingError {
    if ApiFailure::parse(site, raw).rpc().is_some() {
        UserFacingError::Silent
    } else {
        toast(raw.to_owned())
    }
}

/// `messages.editQuickReplyShortcut` (`data/business/data_shortcut_messages.cpp:561-572`).
#[must_use]
pub fn rename_quick_reply(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("messages.editQuickReplyShortcut", raw);
    match failure.error_type() {
        Some("SHORTCUT_OCCUPIED") => toast(tr::lng_replies_error_occupied()),
        Some("SHORTCUT_ID_INVALID") | None => UserFacingError::Silent,
        Some(name) => toast(name.to_owned()),
    }
}

/// Editing a quick-reply message (`settings/business/settings_shortcut_messages.cpp:1292-1307`).
#[must_use]
pub fn edit_quick_reply_message(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("messages.editMessage", raw);
    match failure.error_type() {
        Some("MESSAGE_NOT_MODIFIED" | "MESSAGE_EMPTY") | None => UserFacingError::Silent,
        Some(_) => toast(tr::lng_edit_error()),
    }
}
