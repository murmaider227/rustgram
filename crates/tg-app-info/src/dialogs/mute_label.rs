//! "Unmute" context-menu label with the time left until the mute ends ("Unmute\t2h").
//!
//! Port origin: `PeerMenuAddMuteSubmenuAction` (`Telegram/SourceFiles/window/window_peer_menu.cpp`)
//! and `Ui::FormatMuteForTiny` (`Telegram/SourceFiles/ui/text/format_values.cpp`).

/// `Ui::FormatMuteForTiny`: the short "time left" suffix ("5m", "2h", "3d", "1w", "2m", "1y"),
/// empty for a minute or less and for anything past a year (muted forever).
#[must_use]
pub fn format_mute_for_tiny(sec: f64) -> String {
    let count = |unit: f64| (sec / unit).round() as i64;
    if sec <= 60.0 {
        String::new()
    } else if sec <= 60.0 * 59.0 {
        tg_lang::tr::lng_minutes_tiny(count(60.0))
    } else if sec <= 3600.0 * 23.0 {
        tg_lang::tr::lng_hours_tiny(count(3600.0))
    } else if sec <= 86400.0 * 6.0 {
        tg_lang::tr::lng_days_tiny(count(86400.0))
    } else if sec <= (86400.0 * 7.0) * 3.0 {
        tg_lang::tr::lng_weeks_tiny(count(86400.0 * 7.0))
    } else if sec <= (86400.0 * 31.0) * 11.0 {
        tg_lang::tr::lng_months_tiny(count(86400.0 * 31.0))
    } else if sec <= 86400.0 * 366.0 {
        tg_lang::tr::lng_years_tiny(count(86400.0 * 365.0))
    } else {
        String::new()
    }
}

/// `lng_context_unmute + '\t' + FormatMuteForTiny(muteUntil - now)`: the menu draws the part
/// after the tab right-aligned like a shortcut. `mute_until` is `None` when the end is unknown.
#[must_use]
pub fn unmute_menu_label(mute_until: Option<i32>, now_unix: i64) -> String {
    let left = mute_until.map_or(0, |until| i64::from(until) - now_unix);
    format!(
        "{}\t{}",
        tg_lang::tr::lng_context_unmute(),
        format_mute_for_tiny(left as f64)
    )
}

/// Current Unix time in seconds (`base::unixtime::now`).
#[must_use]
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_mute_suffix_matches_format_mute_for_tiny() {
        assert_eq!(format_mute_for_tiny(30.0), "");
        assert_eq!(format_mute_for_tiny(60.0), "");
        assert_eq!(format_mute_for_tiny(61.0), "1m");
        assert_eq!(format_mute_for_tiny(45.0 * 60.0), "45m");
        assert_eq!(format_mute_for_tiny(60.0 * 59.0 + 1.0), "1h");
        assert_eq!(format_mute_for_tiny(2.0 * 3600.0 - 30.0), "2h");
        assert_eq!(format_mute_for_tiny(23.0 * 3600.0 + 1.0), "1d");
        assert_eq!(format_mute_for_tiny(3.0 * 86400.0), "3d");
        assert_eq!(format_mute_for_tiny(7.0 * 86400.0), "1w");
        assert_eq!(format_mute_for_tiny(60.0 * 86400.0), "2m");
        assert_eq!(format_mute_for_tiny(365.0 * 86400.0), "1y");
        // Muted forever (`i32::MAX`) is far past a year: no suffix.
        assert_eq!(format_mute_for_tiny(f64::from(i32::MAX) - 1.8e9), "");
    }

    #[test]
    fn unmute_label_carries_the_time_left_after_a_tab() {
        let now = 1_800_000_000;
        assert_eq!(
            unmute_menu_label(Some((now + 2 * 3600) as i32), now),
            format!("{}\t2h", tg_lang::tr::lng_context_unmute())
        );
        assert_eq!(
            unmute_menu_label(Some(i32::MAX), now),
            format!("{}\t", tg_lang::tr::lng_context_unmute())
        );
        assert_eq!(
            unmute_menu_label(None, now),
            format!("{}\t", tg_lang::tr::lng_context_unmute())
        );
    }
}
