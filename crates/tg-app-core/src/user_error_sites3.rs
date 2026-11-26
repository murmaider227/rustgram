//! Failure handlers of the chat's lists and boxes, quick replies and payment links, each a port
//! of the tdesktop `.fail()` handler it cites (TGB-2977). Paths are under `Telegram/SourceFiles/`.

use tg_lang::tr;

use super::{ApiFailure, UserFacingError, error_type_toast, send_message, silent};

fn toast(text: String) -> UserFacingError {
    UserFacingError::Toast(text)
}

/// `messages.getMessageReactionsList` for the "who reacted" list
/// (`history/view/reactions/history_view_reactions_list.cpp:440-480`): no failure handler.
#[must_use]
pub fn reactions_list(raw: &str) -> UserFacingError {
    silent("messages.getMessageReactionsList", raw)
}

/// `messages.togglePaidReactionPrivacy` (`data/data_message_reactions.cpp:1815-1844`): the
/// handler only finishes the local send; nothing is shown.
#[must_use]
pub fn paid_reaction_privacy(raw: &str) -> UserFacingError {
    silent("messages.togglePaidReactionPrivacy", raw)
}

/// `messages.reportReaction` (`api/api_report.cpp:198-212`): no failure handler.
#[must_use]
pub fn report_reaction(raw: &str) -> UserFacingError {
    silent("messages.reportReaction", raw)
}

/// `messages.deleteParticipantReaction(s)` (`apiwrap.cpp:1570-1600`): no failure handler.
#[must_use]
pub fn delete_participant_reactions(raw: &str) -> UserFacingError {
    silent("messages.deleteParticipantReaction", raw)
}

/// `messages.getPollVotes` (`info/polls/info_polls_results_inner_widget.cpp:326-375`): the
/// handler only clears the request id.
#[must_use]
pub fn poll_votes(raw: &str) -> UserFacingError {
    silent("messages.getPollVotes", raw)
}

/// `messages.getPollResults` (`api/api_polls.cpp:465-478`): nothing is shown.
#[must_use]
pub fn reload_poll_results(raw: &str) -> UserFacingError {
    silent("messages.getPollResults", raw)
}

/// `stats.getPollStats` (`api/api_polls.cpp:520-535`, shown by
/// `history/view/media/menu/history_view_poll_menu.cpp:310-350`): the stats area's label shows
/// the error type, or "No votes" when there is none.
#[must_use]
pub fn poll_stats_label(raw: &str) -> String {
    ApiFailure::parse("stats.getPollStats", raw)
        .error_type()
        .map_or_else(tr::lng_polls_votes_none, str::to_owned)
}

/// `messages.search` inside a chat (`api/api_messages_search.cpp:143-153`): nothing is shown;
/// the search just ends.
#[must_use]
pub fn chat_search(raw: &str) -> UserFacingError {
    silent("messages.search", raw)
}

/// Adding a message to a quick-reply shortcut sends it (`messages.sendMessage` with
/// `quick_reply_shortcut`), so it fails like any send (`ApiWrap::sendMessageFail`).
#[must_use]
pub fn quick_reply_send(raw: &str) -> UserFacingError {
    send_message(raw, super::SendTarget::User)
}

/// A payments, Passport or gift link that failed to load.
///
/// * an invoice: `payments.getPaymentForm` ([`super::load_payment_screen`]);
/// * a Passport form: `FormController::formFail` (`passport/passport_form_controller.cpp:2623-2630`)
///   shows "Sorry, there was an error..." and the error type;
/// * a unique gift: `core/local_url_handlers.cpp:2393-2397`, `ShowGiftErrorToast` or
///   `"Error: " + type`;
/// * anything else: the error type.
#[must_use]
pub fn payments_link(kind: PaymentsLinkKind, raw: &str) -> UserFacingError {
    match kind {
        PaymentsLinkKind::Invoice => super::load_payment_screen(raw),
        PaymentsLinkKind::Passport => {
            let failure = ApiFailure::parse("account.getAuthorizationForm", raw);
            match failure.error_type() {
                Some(name) => toast(format!("{}\n{name}", tr::lng_passport_form_error())),
                None => UserFacingError::Silent,
            }
        }
        PaymentsLinkKind::UniqueGift => {
            let failure = ApiFailure::parse("payments.getUniqueStarGift", raw);
            match failure.error_type() {
                Some("STARGIFT_ALREADY_BURNED") => toast(tr::lng_gift_burned_message()),
                Some("STARGIFT_MESSAGE_INVALID") => toast(tr::lng_gift_send_message_restricted()),
                Some(name) => toast(format!("Error: {name}")),
                None => UserFacingError::Silent,
            }
        }
        PaymentsLinkKind::Other => error_type_toast("payments link", raw),
    }
}

/// Which payments link failed, for [`payments_link`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentsLinkKind {
    /// `tg://invoice`, `t.me/$slug`.
    Invoice,
    /// `tg://passport`, `tg://resolve?domain=telegrampassport`.
    Passport,
    /// `t.me/nft/<slug>`.
    UniqueGift,
    /// Another payments screen.
    Other,
}

/// Chat requests whose tdesktop counterparts have no failure UI: accepting a contact and
/// reporting spam from the contact bar (`history/view/history_view_contact_status.cpp:917`,
/// `967`), toggling translations (`data/data_peer.cpp:862`), pausing or disabling a Business
/// bot (`data/business/data_business_chatbots.cpp:161-212`, the peer settings are reloaded),
/// the default "send as" peer (`main/session/send_as_peers.cpp:94`), a saved reaction tag and
/// the default reaction (`data/data_message_reactions.cpp:512-516`, `582`), a to-do item toggle
/// (`api/api_todo_lists.cpp:193-201`), "block from replies" (`window/window_peer_menu.cpp:2847`)
/// and a message translation (`lang/translate_mtproto_provider.cpp:101-149`, `failAll`).
#[must_use]
pub fn chat_silent(site: &str, raw: &str) -> UserFacingError {
    silent(site, raw)
}

/// `messages.sendVote` (`api/api_polls.cpp:345-358`): only a vote restriction shows a toast
/// (`ShowVoteRestrictionToast`, not ported); anything else nothing.
#[must_use]
pub fn poll_vote(raw: &str) -> UserFacingError {
    silent("messages.sendVote", raw)
}

/// Editing a message from the compose field (`history/history_widget.cpp:5724-5746`): the same
/// decision as the quick-reply editor.
#[must_use]
pub fn edit_message(raw: &str) -> UserFacingError {
    super::edit_quick_reply_message(raw)
}

/// `messages.addPollAnswer` (`history/view/history_view_add_poll_option.cpp:469-485`,
/// `mapErrorToText`).
#[must_use]
pub fn add_poll_option(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("messages.addPollAnswer", raw);
    let Some(name) = failure.error_type() else {
        return UserFacingError::Silent;
    };
    toast(if name == "POLL_ANSWERS_TOO_MUCH" {
        tr::lng_polls_max_options_reached()
    } else if name == "POLL_ANSWER_DUPLICATE" {
        tr::lng_polls_add_option_duplicate()
    } else if name.starts_with("POLL") && name.contains("CLOSE") {
        tr::lng_polls_add_option_closed()
    } else {
        tr::lng_polls_add_option_error()
    })
}

/// Accepting a suggested birthday (`core/local_url_handlers.cpp:1065-1069`): a flood wait or
/// `"Error: " + type`.
#[must_use]
pub fn suggested_birthday(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("account.updateBirthday", raw);
    match failure.error_type() {
        Some(name) if name.starts_with("FLOOD_WAIT_") => toast(tr::lng_flood_error()),
        Some(name) => toast(format!("Error: {name}")),
        None => UserFacingError::Silent,
    }
}

/// `messages.editFactCheck` / `deleteFactCheck` (`data/components/factchecks.cpp:200-215`): the
/// error type in a toast.
#[must_use]
pub fn factcheck(raw: &str) -> UserFacingError {
    error_type_toast("messages.editFactCheck", raw)
}
