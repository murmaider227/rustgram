//! Pure channel direct messages (monoforum) & suggested-post DTOs.

use crate::history::sections_model::ScheduleDateTime;
use crate::stand_in::types::{MsgId, PeerId};

/// Nanotons per 1 TON (`Ui::kNanosInOne`).
pub const NANOS_PER_TON: i64 = 1_000_000_000;
/// Default channel creator share for Stars suggested posts in permille (`850` = `85%`).
pub const SUGGESTED_POST_COMMISSION_STARS: i32 = 850;
/// Default channel creator share for TON suggested posts in permille (`850` = `85%`).
pub const SUGGESTED_POST_COMMISSION_TON: i32 = 850;
/// Default minimum delay in seconds before a scheduled suggested post (`300` = 5 min).
pub const SUGGESTED_POST_DELAY_MIN_SECS: i32 = 300;
/// Default maximum delay in seconds for a scheduled suggested post (`2_678_400` = 31 days).
pub const SUGGESTED_POST_DELAY_MAX_SECS: i32 = 2_678_400;
/// Default minimum Stars offer for a suggested post.
pub const SUGGESTED_POST_STARS_MIN: i64 = 5;
/// Default maximum Stars offer for a suggested post.
pub const SUGGESTED_POST_STARS_MAX: i64 = 100_000;
/// Default minimum TON offer in nanotons (`0.01 TON`).
pub const SUGGESTED_POST_NANOTON_MIN: i64 = 10_000_000;
/// Default maximum TON offer in nanotons (`10_000 TON`).
pub const SUGGESTED_POST_NANOTON_MAX: i64 = 10_000_000_000_000;

/// Currency used for a suggested post offer (`CreditsType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuggestedCurrency {
    /// Telegram Stars (`starsAmount`).
    #[default]
    Stars,
    /// Toncoin (`starsTonAmount`, in nanotons).
    Ton,
}

/// Price offered for a suggested post (`CreditsAmount` / `StarsAmount`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SuggestedPrice {
    /// Currency (`Stars` or `Ton`).
    pub currency: SuggestedCurrency,
    /// Whole units (Stars or TON).
    pub whole: i64,
    /// Fractional billionths (`nanos` in `0..1_000_000_000`).
    pub nanos: i32,
}

impl SuggestedPrice {
    /// Zero / free offer.
    #[must_use]
    pub const fn free() -> Self {
        Self {
            currency: SuggestedCurrency::Stars,
            whole: 0,
            nanos: 0,
        }
    }

    /// Creates a whole Telegram Stars price.
    #[must_use]
    pub const fn stars(amount: i64) -> Self {
        Self {
            currency: SuggestedCurrency::Stars,
            whole: amount,
            nanos: 0,
        }
    }

    /// Creates a TON price from nanotons (`starsTonAmount.amount`).
    #[must_use]
    pub const fn ton_nanos(total_nanos: i64) -> Self {
        Self {
            currency: SuggestedCurrency::Ton,
            whole: total_nanos / NANOS_PER_TON,
            nanos: (total_nanos % NANOS_PER_TON) as i32,
        }
    }

    /// Returns `true` if this offer is free (`whole == 0 && nanos == 0`).
    #[inline]
    #[must_use]
    pub const fn is_free(&self) -> bool {
        self.whole == 0 && self.nanos == 0
    }

    /// Returns total nanotons (for TON) or total nanos (for Stars).
    #[inline]
    #[must_use]
    pub const fn total_nanos(&self) -> i64 {
        self.whole
            .saturating_mul(NANOS_PER_TON)
            .saturating_add(self.nanos as i64)
    }

    /// Computes creator payout after Telegram's commission (`HistoryView::PriceAfterCommission`).
    #[must_use]
    pub fn after_commission(&self, commission_permille: i32) -> Self {
        let mul = f64::from(commission_permille.clamp(0, 1000)) / 1000.0;
        match self.currency {
            SuggestedCurrency::Stars => {
                let exact = (self.whole as f64) * mul;
                Self::stars(exact.floor() as i64)
            }
            SuggestedCurrency::Ton => {
                let exact_nanos = ((self.total_nanos() as f64) * mul).round() as i64;
                Self::ton_nanos(exact_nanos)
            }
        }
    }

    /// Formats the commission percentage (`HistoryView::FormatAfterCommissionPercent`).
    #[must_use]
    pub fn format_commission_percent(commission_permille: i32) -> String {
        let tenths = commission_permille.clamp(0, 1000);
        if tenths % 10 == 0 {
            format!("{}%", tenths / 10)
        } else {
            format!("{:.1}%", f64::from(tenths) / 10.0)
        }
    }

    /// Formats a human-readable amount string (`"Free"`, `"250 Stars"`, `"1.5 TON"`).
    #[must_use]
    pub fn format_label(&self) -> String {
        if self.is_free() {
            return "Free".to_owned();
        }
        match self.currency {
            SuggestedCurrency::Stars => format!("⭐ {} Stars", self.whole),
            SuggestedCurrency::Ton => {
                if self.nanos == 0 {
                    format!("💎 {} TON", self.whole)
                } else {
                    let frac = format!("{:09}", self.nanos.unsigned_abs());
                    let trimmed = frac.trim_end_matches('0');
                    format!("💎 {}.{} TON", self.whole, trimmed)
                }
            }
        }
    }

    /// Parses a user-entered TON decimal string (e.g. `"1.25"`) into nanotons.
    #[must_use]
    pub fn parse_ton_nanos(raw: &str) -> Option<i64> {
        let s = raw.trim().replace(',', ".");
        if s.is_empty() {
            return Some(0);
        }
        let mut parts = s.split('.');
        let whole_str = parts.next()?;
        let frac_str = parts.next();
        if parts.next().is_some() {
            return None;
        }
        let whole: i64 = if whole_str.is_empty() {
            0
        } else {
            whole_str.parse().ok()?
        };
        if whole < 0 {
            return None;
        }
        let nanos: i64 = match frac_str {
            None | Some("") => 0,
            Some(frac) => {
                if frac.len() > 9 || !frac.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                let padded = format!("{frac:0<9}");
                padded.parse().ok()?
            }
        };
        whole
            .checked_mul(NANOS_PER_TON)
            .and_then(|w| w.checked_add(nanos))
    }
}

/// Lifecycle status of a suggested post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestedPostStatus {
    /// Awaiting approval or decline.
    Pending,
    /// Approved by the recipient/channel admin.
    Accepted,
    /// Declined.
    Rejected,
}

/// Suggested post metadata attached to a channel DM (monoforum) message (`SuggestedPost`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuggestedPostData {
    /// Optional offered price (`None` = free post suggestion).
    pub price: Option<SuggestedPrice>,
    /// Optional requested publication Unix timestamp (`None` = any time).
    pub schedule_date: Option<i32>,
    /// True if approved (`suggestedPost.accepted`).
    pub accepted: bool,
    /// True if rejected (`suggestedPost.rejected`).
    pub rejected: bool,
    /// True if paid with Stars (`message.paid_suggested_post_stars`).
    pub paid_stars: bool,
    /// True if paid with TON (`message.paid_suggested_post_ton`).
    pub paid_ton: bool,
}

impl SuggestedPostData {
    /// Returns the lifecycle status (`Pending` / `Accepted` / `Rejected`).
    #[must_use]
    pub const fn status(&self) -> SuggestedPostStatus {
        if self.accepted {
            SuggestedPostStatus::Accepted
        } else if self.rejected {
            SuggestedPostStatus::Rejected
        } else {
            SuggestedPostStatus::Pending
        }
    }

    /// Returns `true` if still awaiting a decision.
    #[inline]
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        !self.accepted && !self.rejected
    }
}

/// Operating mode of the suggested post options modal (`HistoryView::SuggestMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestMode {
    /// Creating a new suggested post offer.
    New,
    /// Proposing changes to an existing suggested post.
    Change,
    /// Picking publication time while accepting a suggested post without a scheduled date.
    Publish,
}

/// Editable suggest-post parameters attached to an outbound message (`SuggestOptions`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuggestOptions {
    /// Optional Stars or TON offer (`None` = free).
    pub price: Option<SuggestedPrice>,
    /// Optional requested publish Unix timestamp (`None` = any time).
    pub schedule_date: Option<i32>,
}

/// Decision passed to `messages.toggleSuggestedPostApproval`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestApprovalDecision {
    /// Approve the suggested post (optionally setting `schedule_date`).
    Approve {
        /// Optional scheduled publication timestamp.
        schedule_date: Option<i32>,
    },
    /// Reject the suggested post with an optional rejection comment.
    Reject {
        /// Optional explanation shown to the proposer.
        comment: Option<String>,
    },
}

/// Channel Direct Messages (monoforum) per-user sublist (`Data::SavedSublist` / `monoForumDialog`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonoforumSublist {
    /// Owning monoforum channel peer id.
    pub parent_peer_id: PeerId,
    /// Sender / conversation partner peer id (`monoForumDialog.peer`).
    pub sublist_peer_id: PeerId,
    /// Display title of `sublist_peer_id`.
    pub title: String,
    /// Deterministic avatar color.
    pub avatar_rgb: [u8; 3],
    /// Top message id in this sublist.
    pub top_message_id: MsgId,
    /// Preview snippet of the latest message.
    pub last_preview: String,
    /// Unix timestamp of the latest message.
    pub last_date_unix: i32,
    /// Highest read incoming message id (`read_inbox_max_id`).
    pub read_inbox_max_id: MsgId,
    /// Highest read outgoing message id (`read_outbox_max_id`).
    pub read_outbox_max_id: MsgId,
    /// Unread message count (`unread_count`).
    pub unread_count: u32,
    /// Unread reactions count (`unread_reactions_count`).
    pub unread_reactions_count: u32,
    /// Explicit unread badge mark (`unread_mark`).
    pub unread_mark: bool,
    /// True if this user is exempt from paid-message fees (`nopaid_messages_exception`).
    pub nopaid_messages_exception: bool,
    /// Optional local/cloud draft text for this sublist.
    pub draft_text: Option<String>,
}

impl MonoforumSublist {
    /// Applies a read-till update to this monoforum sublist (`SavedSublist::apply`).
    pub fn apply_read_till(&mut self, read_till_id: MsgId, is_out: bool) {
        if is_out {
            if read_till_id.0 > self.read_outbox_max_id.0 {
                self.read_outbox_max_id = read_till_id;
            }
        } else if read_till_id.0 >= self.read_inbox_max_id.0 {
            self.read_inbox_max_id = read_till_id;
            if read_till_id.0 >= self.top_message_id.0 {
                self.unread_count = 0;
                self.unread_mark = false;
            }
        }
    }
}

/// Returns `true` when an admin in a flat monoforum view must reply to a specific user message
/// before sending (`ChatWidget::monoforumReplyRequired` in `history_view_chat_section_monoforum.cpp`).
#[must_use]
pub const fn monoforum_reply_required(
    is_monoforum_admin: bool,
    monoforum_disabled: bool,
    is_editing: bool,
    replying_to_same_peer: bool,
) -> bool {
    !is_editing && is_monoforum_admin && !monoforum_disabled && !replying_to_same_peer
}

/// Formats a `messageActionSuggestedPostApproval` service message (`history_view_suggest_decision.cpp`).
#[must_use]
pub fn format_suggested_post_approval_service(
    actor_name: &str,
    rejected: bool,
    balance_too_low: bool,
    reject_comment: Option<&str>,
    schedule_date: Option<i32>,
    price_label: Option<&str>,
) -> String {
    if balance_too_low {
        return "Suggested post failed: insufficient balance".to_owned();
    }
    if rejected {
        return match reject_comment.filter(|c| !c.trim().is_empty()) {
            Some(reason) => format!("{actor_name} declined the suggested post: \"{reason}\""),
            None => format!("{actor_name} declined the suggested post"),
        };
    }
    let mut parts = vec![format!("{actor_name} approved the suggested post")];
    if let Some(price) = price_label.filter(|p| !p.is_empty()) {
        parts.push(format!("for {price}"));
    }
    if let Some(ts) = schedule_date.filter(|&d| d > 0) {
        let dt = ScheduleDateTime::from_unix_utc(i64::from(ts));
        parts.push(format!(
            "({:04}-{:02}-{:02} {:02}:{:02})",
            dt.year, dt.month, dt.day, dt.hour, dt.minute
        ));
    }
    parts.join(" ")
}

/// Formats a `messageActionSuggestedPostSuccess` service message.
#[must_use]
pub fn format_suggested_post_success_service(price_label: &str) -> String {
    format!("Suggested post published — paid {price_label}")
}

/// Formats a `messageActionSuggestedPostRefund` service message.
#[must_use]
pub fn format_suggested_post_refund_service(payer_initiated: bool) -> String {
    if payer_initiated {
        "Suggested post payment was refunded (cancelled by sender)".to_owned()
    } else {
        "Suggested post payment was refunded".to_owned()
    }
}

/// Formats an optional publish Unix timestamp (`"Any time"` or `"YYYY-MM-DD HH:MM"`).
#[must_use]
pub fn format_suggest_schedule_date(schedule_date: Option<i32>) -> String {
    match schedule_date.filter(|&ts| ts > 0) {
        Some(ts) => {
            let dt = ScheduleDateTime::from_unix_utc(i64::from(ts));
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                dt.year, dt.month, dt.day, dt.hour, dt.minute
            )
        }
        None => "Any time".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Suggested post decision card inside a bubble (`history_view_suggest_decision.cpp`)
// ---------------------------------------------------------------------------
