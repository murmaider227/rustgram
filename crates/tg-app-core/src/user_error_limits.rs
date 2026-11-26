//! The Premium limit boxes a failed request opens (`boxes/premium_limits_box.cpp`, TGB-2978):
//! title, the limit sentences with their bold counts, and the buttons of `SimpleLimitBox`
//! ("Increase Limit" + "Cancel" for a free account, "OK" for a Premium one).

use tg_lang::tr;
use tg_payments::premium::PremiumLimits;
use tg_ui::text::RichText;

/// Which limit box a failure opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitBoxKind {
    /// `ChannelsLimitBox` (`premium_limits_box.cpp:544-628`): `CHANNELS_TOO_MUCH`.
    Channels,
    /// `FilterChatsLimitBox` (`premium_limits_box.cpp:695-738`): `FILTER_INCLUDE_TOO_MUCH`.
    FilterChats,
    /// `ShareableFiltersLimitBox` (`premium_limits_box.cpp:826-868`): `CHATLISTS_TOO_MUCH`.
    ShareableFilters,
    /// `FilterLinksLimitBox` (`premium_limits_box.cpp:740-783`): `INVITES_TOO_MUCH`.
    FilterLinks,
}

/// What a limit box shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitBoxContent {
    /// Box title.
    pub title: String,
    /// Box text, with the bold counts of `tr::rich`.
    pub text: RichText,
    /// `LimitsPremiumRef(refAddition)` for the "Increase Limit" button; `None` for a Premium
    /// account, which gets a single "OK".
    pub increase_ref: Option<String>,
}

/// `Data::PremiumLimits` value for an app-config key: the override, else tdesktop's default.
fn limit(limits: &PremiumLimits, key: &str, fallback: i32) -> i32 {
    limits.overrides.get(key).copied().unwrap_or(fallback)
}

/// A `tr::rich` phrase as rich text: `**bold**` markers become bold spans.
#[must_use]
pub fn rich_from_marked(marked: &str) -> RichText {
    let mut rich = RichText::new();
    for (i, part) in marked.split("**").enumerate() {
        rich = if i % 2 == 1 {
            rich.push_bold(part)
        } else {
            rich.push_plain(part)
        };
    }
    rich
}

/// The title, text and buttons of `kind` for a free (`is_premium == false`) or Premium account
/// (`session->premiumPossible()` is assumed: the Premium purchase is not blocked).
#[must_use]
pub fn limit_box_content(kind: LimitBoxKind, is_premium: bool) -> LimitBoxContent {
    let limits = PremiumLimits::new(is_premium);
    let (title, first, second, ref_addition) = match kind {
        LimitBoxKind::Channels => {
            let current = if is_premium {
                limit(&limits, "channels_limit_premium", 1000)
            } else {
                limit(&limits, "channels_limit_default", 500)
            };
            let premium = limit(&limits, "channels_limit_premium", 1000);
            (
                tr::lng_channels_limit_title(),
                tr::lng_channels_limit1(current),
                // The channels box always has a second sentence.
                Some(if is_premium {
                    tr::lng_channels_limit2_final()
                } else {
                    tr::lng_channels_limit2(premium)
                }),
                "channels",
            )
        }
        LimitBoxKind::FilterChats => {
            let premium = limit(&limits, "dialog_filters_chats_limit_premium", 200);
            let current = if is_premium {
                premium
            } else {
                limit(&limits, "dialog_filters_chats_limit_default", 100)
            };
            (
                tr::lng_filter_chats_limit_title(),
                tr::lng_filter_chats_limit1(current),
                (!is_premium).then(|| tr::lng_filter_chats_limit2(premium)),
                "dialog_filters_chats",
            )
        }
        LimitBoxKind::ShareableFilters => {
            let premium = limit(&limits, "chatlists_joined_limit_premium", 20);
            let current = if is_premium {
                premium
            } else {
                limit(&limits, "chatlists_joined_limit_default", 2)
            };
            (
                tr::lng_filter_shared_limit_title(),
                tr::lng_filter_shared_limit1(current),
                (!is_premium).then(|| tr::lng_filter_shared_limit2(premium)),
                "chatlists_joined",
            )
        }
        LimitBoxKind::FilterLinks => {
            let premium = limit(&limits, "chatlist_invites_limit_premium", 20);
            let current = if is_premium {
                premium
            } else {
                limit(&limits, "chatlist_invites_limit_default", 3)
            };
            (
                tr::lng_filter_links_limit_title(),
                tr::lng_filter_links_limit1(current),
                (!is_premium).then(|| tr::lng_filter_links_limit2(premium)),
                "chatlist_invites",
            )
        }
    };
    let marked = match second {
        Some(second) => format!("{first} {second}"),
        None => first,
    };
    LimitBoxContent {
        title,
        text: rich_from_marked(&marked),
        increase_ref: (!is_premium).then(|| format!("double_limits__{ref_addition}")),
    }
}
