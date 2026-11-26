//! What the user sees after a request fails (TGB-2957).
//!
//! Backend traits return `Result<_, String>`, so an RPC failure reaches the UI as text such as
//! `"RPC error 400: CHANNELS_TOO_MUCH"`. tdesktop never shows that text. Each request's
//! `.fail()` handler shows a translated message for the error names it knows, the bare error
//! type (`showToast(error.type())`), or nothing. The functions here make the same decision per
//! call site and cite the tdesktop handler they port. Every failure is logged in full.
//!
//! Internal design: the error stays a string across the backend traits (changing ~200 trait
//! signatures would touch every backend and mock); [`ApiFailure::parse`] reads the RPC error
//! back from its stable `RPC error <code>: <NAME>` display form
//! ([`tg_api::RpcError::find_in_text`]).

use tg_api::RpcError;
use tg_lang::tr;

/// What a call site shows for a failed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserFacingError {
    /// A toast with this text.
    Toast(String),
    /// An inform box (one "OK"/"Close" button) with this text (`Ui::MakeInformBox`).
    Inform {
        /// Box title, if the tdesktop box has one.
        title: Option<String>,
        /// Box text.
        text: String,
    },
    /// An inform box whose text has links (`Ui::MakeInformBox` with a `TextWithEntities`), e.g.
    /// the "more info" link to @SpamBot.
    InformRich {
        /// Box title, if the tdesktop box has one.
        title: Option<String>,
        /// Box text with its link spans.
        text: tg_ui::text::RichText,
    },
    /// A Premium limit box (`boxes/premium_limits_box.cpp`); its text depends on the account's
    /// Premium status, which the window knows ([`limit_box_content`]).
    LimitBox(LimitBoxKind),
    /// The handler opens the Premium screen (`Settings::ShowPremium(session, ref)`).
    PremiumScreen {
        /// The `ref` tdesktop passes, e.g. `premium_stickers`.
        referral: String,
    },
    /// Nothing visible; the error is only logged.
    Silent,
}

impl UserFacingError {
    /// The visible text (toast or box body), if any.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Toast(text) | Self::Inform { text, .. } => Some(text),
            Self::InformRich { text, .. } => Some(&text.text),
            Self::LimitBox(_) | Self::PremiumScreen { .. } | Self::Silent => None,
        }
    }

    /// The visible text for a surface that can only show a toast or a label.
    #[must_use]
    pub fn into_text(self) -> Option<String> {
        match self {
            Self::Toast(text) | Self::Inform { text, .. } => Some(text),
            Self::InformRich { text, .. } => Some(text.text),
            Self::LimitBox(kind) => Some(limit_box_content(kind, false).text.text),
            Self::PremiumScreen { .. } | Self::Silent => None,
        }
    }

    /// `true` when the error needs the window (a box or a screen), not only a toast or label.
    #[must_use]
    pub const fn needs_window(&self) -> bool {
        matches!(
            self,
            Self::Inform { .. }
                | Self::InformRich { .. }
                | Self::LimitBox(_)
                | Self::PremiumScreen { .. }
        )
    }
}

/// A failed request as the UI received it: the raw text plus the RPC error inside it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFailure {
    rpc: Option<RpcError>,
}

impl ApiFailure {
    /// Reads the RPC error out of `raw` and logs `raw` under `site`.
    #[must_use]
    pub fn parse(site: &str, raw: &str) -> Self {
        tracing::debug!(target: "user_error", site, error = raw, "request failed");
        Self {
            rpc: RpcError::find_in_text(raw),
        }
    }

    /// The RPC error, if the failure was one.
    #[must_use]
    pub const fn rpc(&self) -> Option<&RpcError> {
        self.rpc.as_ref()
    }

    /// `error.type()`: the RPC error name, e.g. `STICKERSET_INVALID`.
    #[must_use]
    pub fn error_type(&self) -> Option<&str> {
        self.rpc.as_ref().map(|e| e.name.as_str())
    }

    /// `error.code()`.
    #[must_use]
    pub fn code(&self) -> Option<i32> {
        self.rpc.as_ref().map(|e| e.code)
    }

    /// `MTP::IsFloodError`.
    #[must_use]
    pub fn is_flood(&self) -> bool {
        self.rpc.as_ref().is_some_and(RpcError::is_flood_error)
    }

    /// `type == name`.
    #[must_use]
    pub fn is(&self, name: &str) -> bool {
        self.error_type() == Some(name)
    }
}

fn toast(text: String) -> UserFacingError {
    UserFacingError::Toast(text)
}

/// The most common tdesktop handler, `showToast(error.type())`: the bare error name for an
/// RPC error. A local or transport failure shows nothing (tdesktop retries those requests and
/// never reaches `.fail()`).
#[must_use]
pub fn error_type_toast(site: &str, raw: &str) -> UserFacingError {
    ApiFailure::parse(site, raw)
        .error_type()
        .map_or(UserFacingError::Silent, |name| toast(name.to_owned()))
}

/// A request whose tdesktop `.fail()` handler shows nothing (or that has no handler).
#[must_use]
pub fn silent(site: &str, raw: &str) -> UserFacingError {
    let _ = ApiFailure::parse(site, raw);
    UserFacingError::Silent
}

/// Last line of defence in every toast sink: a text that still carries a raw RPC error (a call
/// site not yet mapped above) shows only the bare error type, tdesktop's most common handler
/// (`showToast(error.type())`), instead of `Could not X: RPC error 400: NAME`. A `406` shows
/// nothing (`MTP::IgnoreError`). Any other text passes unchanged.
#[must_use]
pub fn visible_toast_text(text: String) -> Option<String> {
    let Some(rpc) = RpcError::find_in_text(&text) else {
        return Some(text);
    };
    tracing::warn!(target: "user_error", error = %text, "unmapped request failure reached a toast");
    (!rpc.is_ignored()).then_some(rpc.name)
}

/// Removes tdesktop's `**bold**` markers from a `tr::rich` phrase shown as plain text.
fn plain(rich: &str) -> String {
    rich.replace("**", "")
}

/// `text` with the first occurrence of `label` made a link to `url` (`tr::link(label, url)`
/// substituted into a phrase, `tr::marked`).
#[must_use]
pub fn text_with_link(text: &str, label: &str, url: &str) -> tg_ui::text::RichText {
    let mut rich = tg_ui::text::RichText::plain(text);
    if !label.is_empty()
        && let Some(start) = text.find(label)
    {
        rich.add_span(
            start,
            start + label.len(),
            tg_ui::text::TextEntityKind::Link(url.to_owned()),
        );
    }
    rich
}

/// `session->createInternalLinkFull("spambot")`: the "more info" target of the flood texts.
pub const SPAMBOT_LINK: &str = "https://t.me/spambot";

/// `PeerFloodErrorText` (`boxes/add_contact_box.cpp:153-166`) and the banned-in-public-groups
/// text of `ApiWrap::sendMessageFail`: `phrase` with its "more info" part linking @SpamBot.
#[must_use]
pub fn spambot_inform(phrase: impl FnOnce(&str) -> String) -> UserFacingError {
    let more_info = tr::lng_cant_more_info();
    UserFacingError::InformRich {
        title: None,
        text: text_with_link(&phrase(&more_info), &more_info, SPAMBOT_LINK),
    }
}

/// `ChannelsLimitBox` (`boxes/premium_limits_box.cpp:544-628`).
#[must_use]
pub const fn channels_limit_box() -> UserFacingError {
    UserFacingError::LimitBox(LimitBoxKind::Channels)
}

#[path = "user_error_limits.rs"]
mod limits;
pub use limits::*;

/// `SubmitChatInvite` fail (`api/api_chat_invite.cpp:166-191`): `messages.importChatInvite`
/// after the user confirmed joining from an invite link.
#[must_use]
pub fn import_chat_invite(raw: &str, is_group: bool) -> UserFacingError {
    let failure = ApiFailure::parse("messages.importChatInvite", raw);
    if failure.is("CHANNELS_TOO_MUCH") {
        return channels_limit_box();
    }
    toast(if failure.is("INVITE_REQUEST_SENT") {
        if is_group {
            tr::lng_group_request_sent()
        } else {
            tr::lng_group_request_sent_channel()
        }
    } else if failure.is("USERS_TOO_MUCH") {
        tr::lng_group_invite_no_room()
    } else {
        tr::lng_group_invite_bad_link()
    })
}

/// `CheckChatInvite` fail (`api/api_chat_invite.cpp:743-757`): `messages.checkChatInvite` when
/// an invite link opens. A flood wait and a `400` show inform boxes; anything else nothing.
#[must_use]
pub fn check_chat_invite(raw: &str) -> UserFacingError {
    let failure = ApiFailure::parse("messages.checkChatInvite", raw);
    if failure.is_flood() {
        UserFacingError::Inform {
            title: None,
            text: tr::lng_flood_error(),
        }
    } else if failure.code() == Some(400) {
        UserFacingError::Inform {
            title: None,
            text: tr::lng_group_invite_bad_link(),
        }
    } else {
        UserFacingError::Silent
    }
}

#[path = "user_error_sites.rs"]
mod sites;
pub use sites::*;
#[path = "user_error_sites2.rs"]
mod sites2;
pub use sites2::*;
#[path = "user_error_sites3.rs"]
mod sites3;
pub use sites3::*;

#[cfg(test)]
#[path = "user_error_tests.rs"]
#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "user_error_w4_tests.rs"]
mod w4_tests;
