//! Formatting helpers for call timers, state labels, PTT delays, and recent call dates.
//!
//! Port of:
//! - `Telegram/SourceFiles/ui/text/format_values.cpp` (`FormatDurationText`)
//! - `Telegram/SourceFiles/calls/calls_panel.cpp` (`Panel::updateStatusText`, `startDurationUpdateTimer`)
//! - `Telegram/SourceFiles/calls/group/calls_group_settings.cpp` (`DelayByIndex`, `FormatDelay`)
//! - `Telegram/SourceFiles/calls/group/calls_group_members_row.cpp` (`StatusPercentString`)
//! - `Telegram/SourceFiles/calls/calls_box_controller.cpp` (`Row::refreshStatus`)

use tg_calls::CallState;
use tg_lang::tr;

use crate::calls::strings;

/// Formats a non-negative duration in seconds identically to `Ui::FormatDurationText`
/// (`ui/text/format_values.cpp:135-138`): `MM:SS` under 1 hour, `H:MM:SS` at 1 hour and above.
#[must_use]
pub fn format_duration_text(duration_secs: i64) -> String {
    let clamped = duration_secs.max(0);
    let hours = clamped / 3600;
    let minutes = (clamped % 3600) / 60;
    let seconds = clamped % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Computes milliseconds until the next whole-second boundary (+5 ms safety margin),
/// matching `Panel::startDurationUpdateTimer` (`calls_panel.cpp:1948-1951`).
#[must_use]
pub const fn ms_until_next_second(current_duration_ms: u64) -> u64 {
    (1000 - (current_duration_ms % 1000)) + 5
}

/// Returns the localized status line for a 1:1 call state (`Panel::updateStatusText`,
/// `calls_panel.cpp:1879-1918`).
#[must_use]
pub fn call_status_text(
    state: CallState,
    duration_ms: u64,
    is_conference_invite: bool,
    rating_ended_duration_secs: Option<i64>,
) -> String {
    if let Some(ended_secs) = rating_ended_duration_secs
        && ended_secs > 0
    {
        return format_duration_text(ended_secs);
    }
    match state {
        CallState::Starting
        | CallState::WaitingInit
        | CallState::WaitingInitAck
        | CallState::MigrationHangingUp => tr::lng_call_status_connecting(),
        CallState::Established => format_duration_text((duration_ms / 1000) as i64),
        CallState::FailedHangingUp | CallState::Failed => tr::lng_call_status_failed(),
        CallState::HangingUp => tr::lng_call_status_hanging(),
        CallState::Ended | CallState::EndedByOtherDevice => tr::lng_call_status_ended(),
        CallState::ExchangingKeys => tr::lng_call_status_exchanging(),
        CallState::Waiting => tr::lng_call_status_waiting(),
        CallState::Requesting => tr::lng_call_status_requesting(),
        CallState::WaitingIncoming => {
            if is_conference_invite {
                tr::lng_call_status_group_invite()
            } else {
                tr::lng_call_status_incoming()
            }
        }
        CallState::Ringing => tr::lng_call_status_ringing(),
        CallState::Busy => tr::lng_call_status_busy(),
        CallState::WaitingUserConfirmation => strings::START_CALL_CONFIRM.to_owned(),
    }
}

/// Returns the hover tooltip text for the 4-emoji verification fingerprint pill
/// (`tr::lng_call_fingerprint_tooltip`).
#[must_use]
pub fn emoji_fingerprint_tooltip(peer_name: &str) -> String {
    tr::lng_call_fingerprint_tooltip(peer_name)
}

/// Number of discrete Push-to-Talk release delay steps (`kDelaysCount = 201` -> `0..=2000 ms`).
pub const PTT_DELAY_STEPS: usize = 201;

/// Maps a Push-to-Talk slider index (`0..PTT_DELAY_STEPS`) to milliseconds (`DelayByIndex`,
/// `calls_group_settings.cpp:121-123`).
#[must_use]
pub const fn ptt_delay_by_index(index: usize) -> u64 {
    let clamped = if index >= PTT_DELAY_STEPS {
        PTT_DELAY_STEPS - 1
    } else {
        index
    };
    (clamped as u64) * 10
}

/// Formats Push-to-Talk release delay identically to `FormatDelay`
/// (`calls_group_settings.cpp:125-135`, `tr::lng_group_call_ptt_delay_ms` / `_s`).
#[must_use]
pub fn format_ptt_delay(delay_ms: u64) -> String {
    if delay_ms < 1000 {
        tr::lng_group_call_ptt_delay_ms(&delay_ms.to_string())
    } else {
        tr::lng_group_call_ptt_delay_s(&format!("{:.2}", (delay_ms as f64) / 1000.0))
    }
}

/// Formats a participant's volume (`1..=20000`, where `10000` is 100%) as a percentage string
/// (`StatusPercentString`, `calls_group_members_row.cpp:60-62`).
#[must_use]
pub fn format_volume_percent(volume: i32) -> String {
    let ratio = (volume.clamp(0, tg_calls::group::MAX_VOLUME) as f32)
        / (tg_calls::group::MAX_VOLUME as f32);
    let pct = (ratio * 200.0).round() as i32;
    format!("{pct}%")
}

/// Formats a recent call row status via `tr::lng_call_box_status_*` and `tr::lng_call_duration_info`
/// (`calls_box_controller.cpp:437-459`).
#[must_use]
pub fn format_recent_call_status(
    days_ago: u32,
    hour: u8,
    minute: u8,
    grouped_count: usize,
    duration_secs: Option<i64>,
) -> String {
    let clock = format!("{hour:02}:{minute:02}");
    let time_part = match days_ago {
        0 => tr::lng_call_box_status_today(&clock),
        1 => tr::lng_call_box_status_yesterday(&clock),
        d => tr::lng_call_box_status_date(&tr::lng_days(i64::from(d)), &clock),
    };
    let with_duration = match duration_secs {
        Some(secs) if secs > 0 => {
            tr::lng_call_duration_info(&time_part, &format_duration_text(secs))
        }
        _ => time_part,
    };
    if grouped_count > 1 {
        tr::lng_call_box_status_group(&grouped_count.to_string(), &with_duration)
    } else {
        with_duration
    }
}
