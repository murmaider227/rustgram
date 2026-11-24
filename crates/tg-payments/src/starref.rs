//! Telegram Stars Affiliate Program (StarRef) models, validation, formatting,
//! and TL conversions/builders.
//!
//! Port of `info/bot/starref/info_bot_starref_common.h/.cpp`,
//! `info/bot/starref/info_bot_starref_setup_widget.cpp`,
//! and `info/bot/starref/info_bot_starref_join_widget.cpp`.

use tg_data::{TimeId, UserId};
use tg_tl::{enums as tl_enums, functions as tl_fn};

use crate::credits::CreditsAmount;

/// Minimum allowed commission permille fallback (`starref_min_commission_permille`, 0.1%).
pub const STARREF_COMMISSION_MIN_FALLBACK: i32 = 1;
/// Maximum allowed commission permille fallback (`starref_max_commission_permille`, 90%).
pub const STARREF_COMMISSION_MAX_FALLBACK: i32 = 900;
/// Default commission permille in setup UI (`kCommissionDefault`, 20%).
pub const STARREF_COMMISSION_DEFAULT: i32 = 200;
/// Default program duration in months (`kDurationDefault`, 12 months).
pub const STARREF_DURATION_DEFAULT_MONTHS: i32 = 12;
/// Sentinel value representing an unlimited ("Forever") duration (`0` on the wire).
pub const STARREF_DURATION_FOREVER: i32 = 0;
/// Page size for connected/suggested affiliate bot queries (`kPerPage`).
pub const STARREF_PER_PAGE: i32 = 50;

/// Affiliate program parameters advertised by a bot (`StarRefProgram`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StarRefProgram {
    /// Bot user ID offering the affiliate program.
    pub bot_id: UserId,
    /// Commission rate in permille (`1..=900`, where `200` = `20%`). `0` means ended.
    pub commission_permille: i32,
    /// Attribution duration in months (`0` = forever).
    pub duration_months: i32,
    /// Scheduled program termination Unix timestamp (`0` if active/not scheduled).
    pub end_date: TimeId,
    /// Estimated daily revenue per referred user.
    pub daily_revenue_per_user: CreditsAmount,
}

impl StarRefProgram {
    /// Returns `true` if the program is currently active and not scheduled to end.
    pub const fn is_active(&self) -> bool {
        self.commission_permille > 0 && self.end_date == 0
    }

    /// Formats the commission percentage (`FormatCommission` in `info_bot_starref_common.cpp`).
    pub fn commission_text(&self) -> String {
        format_commission(self.commission_permille)
    }

    /// Formats the program duration (`FormatProgramDuration`).
    pub fn duration_text(&self) -> String {
        format_program_duration(self.duration_months)
    }
}

/// Validation error when creating or updating a bot's affiliate program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StarRefValidationError {
    /// Commission permille outside `[min, max]`.
    CommissionOutOfRange {
        /// Provided value.
        value: i32,
        /// Minimum allowed.
        min: i32,
        /// Maximum allowed.
        max: i32,
    },
    /// Active affiliate programs cannot reduce their commission rate.
    CommissionDecreased {
        /// Existing active commission permille.
        previous: i32,
        /// Attempted lower commission permille.
        requested: i32,
    },
    /// Active affiliate programs cannot shorten their attribution duration.
    DurationShortened {
        /// Existing active duration in months (`0` = forever).
        previous_months: i32,
        /// Attempted shorter duration in months.
        requested_months: i32,
    },
}

/// Validates updating a bot's affiliate program against tdesktop's rules
/// (`info_bot_starref_setup_widget.cpp`: active programs forbid lowering commission
/// or shortening duration; `commission_permille == 0` terminates the program).
pub fn validate_star_ref_update(
    previous: Option<&StarRefProgram>,
    commission_permille: i32,
    duration_months: i32,
    min_commission: i32,
    max_commission: i32,
) -> Result<(), StarRefValidationError> {
    // Terminating an active program is always allowed with commission == 0.
    if commission_permille == 0 {
        return Ok(());
    }
    if commission_permille < min_commission || commission_permille > max_commission {
        return Err(StarRefValidationError::CommissionOutOfRange {
            value: commission_permille,
            min: min_commission,
            max: max_commission,
        });
    }
    if let Some(prev) = previous
        && prev.is_active()
    {
        if commission_permille < prev.commission_permille {
            return Err(StarRefValidationError::CommissionDecreased {
                previous: prev.commission_permille,
                requested: commission_permille,
            });
        }
        let prev_rank = if prev.duration_months == 0 {
            i32::MAX
        } else {
            prev.duration_months
        };
        let req_rank = if duration_months == 0 {
            i32::MAX
        } else {
            duration_months
        };
        if req_rank < prev_rank {
            return Err(StarRefValidationError::DurationShortened {
                previous_months: prev.duration_months,
                requested_months: duration_months,
            });
        }
    }
    Ok(())
}

/// An affiliate program the user/channel has joined (`ConnectedBotStarRef`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectedStarRefBot {
    /// Affiliate program bot ID.
    pub bot_id: UserId,
    /// Referral URL (`https://t.me/<bot>?start=_tgr_...`).
    pub url: String,
    /// Connection Unix timestamp.
    pub date: TimeId,
    /// Locked commission permille at join time.
    pub commission_permille: i32,
    /// Locked duration in months (`0` = forever).
    pub duration_months: i32,
    /// Referred participants count.
    pub participants: i64,
    /// Total Stars earned through this link.
    pub revenue: i64,
    /// Whether the affiliate link has been revoked.
    pub revoked: bool,
}

/// Paginated list of connected affiliate bots (`payments.ConnectedStarRefBots`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectedStarRefBotsSlice {
    /// Total connected bots count.
    pub count: i32,
    /// Entries in this page.
    pub bots: Vec<ConnectedStarRefBot>,
}

/// Paginated list of suggested affiliate programs (`payments.SuggestedStarRefBots`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SuggestedStarRefBotsSlice {
    /// Total suggested programs count.
    pub count: i32,
    /// Programs in this page.
    pub programs: Vec<StarRefProgram>,
    /// Next pagination offset (`""` if last page).
    pub next_offset: String,
}

/// Sort order for `payments.getSuggestedStarRefBots` (`SuggestedSort` in `info_bot_starref_join_widget.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuggestedStarRefSort {
    /// Highest commission × daily revenue per user (default).
    #[default]
    Profitability,
    /// Highest total Stars revenue (`order_by_revenue = true`).
    Revenue,
    /// Newest programs first (`order_by_date = true`).
    Date,
}

/// Formats a permille commission as a clean percentage string (e.g. `200` -> `"20%"`, `125` -> `"12.5%"`).
pub fn format_commission(commission_permille: i32) -> String {
    let clamped = commission_permille.max(0);
    if clamped % 10 == 0 {
        format!("{}%", clamped / 10)
    } else {
        format!("{}.{}%", clamped / 10, clamped % 10)
    }
}

/// Formats an affiliate program duration (`FormatProgramDuration` in `info_bot_starref_common.cpp`).
pub fn format_program_duration(duration_months: i32) -> String {
    if duration_months <= 0 {
        "Forever".to_owned()
    } else if duration_months < 12 {
        if duration_months == 1 {
            "1 month".to_owned()
        } else {
            format!("{duration_months} months")
        }
    } else {
        let years = duration_months / 12;
        if years == 1 {
            "1 year".to_owned()
        } else {
            format!("{years} years")
        }
    }
}

// ============================================================================
// TL Conversions & Request Builders
// ============================================================================

/// Converts a TL `StarRefProgram` into [`StarRefProgram`].
pub fn star_ref_program_from_tl(tl: &tl_enums::StarRefProgram) -> StarRefProgram {
    let tl_enums::StarRefProgram::StarRefProgram(p) = tl;
    StarRefProgram {
        bot_id: UserId::from_tl(p.bot_id),
        commission_permille: p.commission_permille,
        duration_months: p.duration_months.unwrap_or(0),
        end_date: p.end_date.unwrap_or(0),
        daily_revenue_per_user: p
            .daily_revenue_per_user
            .as_ref()
            .map(CreditsAmount::from_tl)
            .unwrap_or(CreditsAmount::ZERO_STARS),
    }
}

/// Converts a TL `ConnectedBotStarRef` into [`ConnectedStarRefBot`].
pub fn connected_star_ref_bot_from_tl(tl: &tl_enums::ConnectedBotStarRef) -> ConnectedStarRefBot {
    let tl_enums::ConnectedBotStarRef::ConnectedBotStarRef(b) = tl;
    ConnectedStarRefBot {
        bot_id: UserId::from_tl(b.bot_id),
        url: b.url.clone(),
        date: b.date,
        commission_permille: b.commission_permille,
        duration_months: b.duration_months.unwrap_or(0),
        participants: b.participants,
        revenue: b.revenue,
        revoked: b.revoked,
    }
}

/// Converts a TL `payments.ConnectedStarRefBots` into [`ConnectedStarRefBotsSlice`].
pub fn connected_star_ref_bots_from_tl(
    tl: &tl_enums::payments::ConnectedStarRefBots,
) -> ConnectedStarRefBotsSlice {
    let tl_enums::payments::ConnectedStarRefBots::ConnectedStarRefBots(d) = tl;
    ConnectedStarRefBotsSlice {
        count: d.count,
        bots: d
            .connected_bots
            .iter()
            .map(connected_star_ref_bot_from_tl)
            .collect(),
    }
}

/// Converts a TL `payments.SuggestedStarRefBots` into [`SuggestedStarRefBotsSlice`].
pub fn suggested_star_ref_bots_from_tl(
    tl: &tl_enums::payments::SuggestedStarRefBots,
) -> SuggestedStarRefBotsSlice {
    let tl_enums::payments::SuggestedStarRefBots::SuggestedStarRefBots(d) = tl;
    SuggestedStarRefBotsSlice {
        count: d.count,
        programs: d
            .suggested_bots
            .iter()
            .map(star_ref_program_from_tl)
            .collect(),
        next_offset: d.next_offset.clone().unwrap_or_default(),
    }
}

/// Builds `bots.updateStarRefProgram` (`UpdateProgram` in `info_bot_starref_common.cpp:984`).
pub fn build_update_star_ref_program(
    bot: tl_enums::InputUser,
    commission_permille: i32,
    duration_months: i32,
) -> tl_fn::bots::UpdateStarRefProgram {
    tl_fn::bots::UpdateStarRefProgram {
        bot,
        commission_permille,
        duration_months: (commission_permille > 0 && duration_months > 0)
            .then_some(duration_months),
    }
}

/// Builds `bots.updateStarRefProgram` with `commission_permille = 0` to end the program.
pub fn build_finish_star_ref_program(
    bot: tl_enums::InputUser,
) -> tl_fn::bots::UpdateStarRefProgram {
    build_update_star_ref_program(bot, 0, 0)
}

/// Builds `payments.getConnectedStarRefBots`.
pub fn build_get_connected_star_ref_bots(
    peer: tl_enums::InputPeer,
    offset: Option<(TimeId, String)>,
    limit: i32,
) -> tl_fn::payments::GetConnectedStarRefBots {
    let (offset_date, offset_link) = match offset {
        Some((d, link)) => (Some(d), Some(link)),
        None => (None, None),
    };
    tl_fn::payments::GetConnectedStarRefBots {
        peer,
        offset_date,
        offset_link,
        limit: if limit > 0 { limit } else { STARREF_PER_PAGE },
    }
}

/// Builds `payments.getConnectedStarRefBot`.
pub fn build_get_connected_star_ref_bot(
    peer: tl_enums::InputPeer,
    bot: tl_enums::InputUser,
) -> tl_fn::payments::GetConnectedStarRefBot {
    tl_fn::payments::GetConnectedStarRefBot { peer, bot }
}

/// Builds `payments.getSuggestedStarRefBots`.
pub fn build_get_suggested_star_ref_bots(
    peer: tl_enums::InputPeer,
    sort: SuggestedStarRefSort,
    offset: impl Into<String>,
    limit: i32,
) -> tl_fn::payments::GetSuggestedStarRefBots {
    tl_fn::payments::GetSuggestedStarRefBots {
        order_by_revenue: sort == SuggestedStarRefSort::Revenue,
        order_by_date: sort == SuggestedStarRefSort::Date,
        peer,
        offset: offset.into(),
        limit: if limit > 0 { limit } else { STARREF_PER_PAGE },
    }
}

/// Builds `payments.connectStarRefBot`.
pub fn build_connect_star_ref_bot(
    peer: tl_enums::InputPeer,
    bot: tl_enums::InputUser,
) -> tl_fn::payments::ConnectStarRefBot {
    tl_fn::payments::ConnectStarRefBot { peer, bot }
}

/// Builds `payments.editConnectedStarRefBot`.
pub fn build_edit_connected_star_ref_bot(
    peer: tl_enums::InputPeer,
    link: impl Into<String>,
    revoked: bool,
) -> tl_fn::payments::EditConnectedStarRefBot {
    tl_fn::payments::EditConnectedStarRefBot {
        revoked,
        peer,
        link: link.into(),
    }
}
