//! Multi-account section of the system tray context menu.
//!
//! Port of `Telegram/SourceFiles/tray_accounts_menu.h` and `tray_accounts_menu.cpp`.

use tg_platform::tray::TrayMenuItem;

/// Maximum characters displayed for an account name before appending `…` (`kMaxLength = 30`
/// in `tray_accounts_menu.cpp:56`).
pub const TRAY_ACCOUNT_MAX_NAME_LEN: usize = 30;

/// First Win32 tray menu command ID reserved for account switching entries.
pub const TRAY_ACCOUNT_MENU_ID_BASE: u32 = 1000;

/// Last Win32 tray menu command ID (inclusive) reserved for account switching entries.
pub const TRAY_ACCOUNT_MENU_ID_MAX: u32 = 1099;

/// Summary of one signed-in account rendered in the system tray context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayAccountEntry {
    /// Account slot index in `AccountManager` / `Domain` (`0`, `1`, ...).
    pub slot_index: i32,
    /// Display name (user full name or `@username`).
    pub display_name: String,
    /// Whether this account is currently active in the main window.
    pub is_active: bool,
    /// Unread badge count for this account (respecting its mute/count settings).
    pub unread_count: u32,
    /// Whether all unread items for this account are muted.
    pub unread_muted: bool,
}

/// Maps an account slot index (`0..=99`) to its deterministic tray menu command ID.
#[must_use]
pub fn account_menu_id(slot_index: i32) -> Option<u32> {
    let idx = u32::try_from(slot_index).ok()?;
    let id = TRAY_ACCOUNT_MENU_ID_BASE.checked_add(idx)?;
    (id <= TRAY_ACCOUNT_MENU_ID_MAX).then_some(id)
}

/// Maps a tray menu command ID back to its account slot index, if it belongs to the accounts section.
#[must_use]
pub fn account_slot_from_menu_id(id: u32) -> Option<i32> {
    if (TRAY_ACCOUNT_MENU_ID_BASE..=TRAY_ACCOUNT_MENU_ID_MAX).contains(&id) {
        i32::try_from(id - TRAY_ACCOUNT_MENU_ID_BASE).ok()
    } else {
        None
    }
}

/// Formats one tray account menu label (`tray_accounts_menu.cpp:63-66`), truncating long names
/// to [`TRAY_ACCOUNT_MAX_NAME_LEN`] characters with an ellipsis and showing unread counts.
#[must_use]
pub fn format_tray_account_label(entry: &TrayAccountEntry) -> String {
    let trimmed = entry.display_name.trim();
    let base = if trimmed.chars().count() > TRAY_ACCOUNT_MAX_NAME_LEN {
        let mut s: String = trimmed.chars().take(TRAY_ACCOUNT_MAX_NAME_LEN).collect();
        s.push('…');
        s
    } else if trimmed.is_empty() {
        format!("Account {}", entry.slot_index + 1)
    } else {
        trimmed.to_owned()
    };

    if entry.unread_count > 0 {
        format!("{base} ({})", entry.unread_count)
    } else {
        base
    }
}

/// Appends the multi-account switcher entries to `items` (`Core::TrayAccountsMenu::Fill`).
///
/// Mirrors tdesktop exactly: hidden while passcode-locked or when `accounts.len() <= 1`;
/// otherwise inserts a separator followed by one item per signed-in account.
pub fn fill_tray_accounts_menu(
    items: &mut Vec<TrayMenuItem>,
    accounts: &[TrayAccountEntry],
    passcode_locked: bool,
) {
    if passcode_locked || accounts.len() <= 1 {
        return;
    }

    items.push(TrayMenuItem::Separator);
    for entry in accounts {
        let Some(id) = account_menu_id(entry.slot_index) else {
            continue;
        };
        items.push(TrayMenuItem::Action {
            id,
            label: format_tray_account_label(entry),
            enabled: true,
            checked: entry.is_active,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::settings::{
        AccountUnreadEntry, UnreadBadgeResult, UnreadBadgeState,
        aggregate_account_badges_per_account,
    };

    #[test]
    fn accounts_menu_hidden_for_single_account_or_passcode_lock() {
        let single = vec![TrayAccountEntry {
            slot_index: 0,
            display_name: "Alice".to_owned(),
            is_active: true,
            unread_count: 2,
            unread_muted: false,
        }];
        let mut items = Vec::new();
        fill_tray_accounts_menu(&mut items, &single, false);
        assert!(items.is_empty());

        let two = vec![
            single[0].clone(),
            TrayAccountEntry {
                slot_index: 1,
                display_name: "Beta".to_owned(),
                is_active: false,
                unread_count: 5,
                unread_muted: true,
            },
        ];
        fill_tray_accounts_menu(&mut items, &two, true);
        assert!(items.is_empty(), "must be hidden when passcode locked");

        fill_tray_accounts_menu(&mut items, &two, false);
        assert_eq!(items.len(), 3); // Separator + 2 accounts
        assert_eq!(items[0], TrayMenuItem::Separator);
        assert_eq!(
            items[1],
            TrayMenuItem::Action {
                id: account_menu_id(0).unwrap(),
                label: "Alice (2)".to_owned(),
                enabled: true,
                checked: true,
            }
        );
        assert_eq!(
            items[2],
            TrayMenuItem::Action {
                id: account_menu_id(1).unwrap(),
                label: "Beta (5)".to_owned(),
                enabled: true,
                checked: false,
            }
        );
        assert_eq!(
            account_slot_from_menu_id(account_menu_id(1).unwrap()),
            Some(1)
        );
        assert_eq!(account_slot_from_menu_id(5), None);
    }

    #[test]
    fn tray_account_label_truncates_at_30_chars_with_ellipsis() {
        let long_name = "A".repeat(40);
        let entry = TrayAccountEntry {
            slot_index: 0,
            display_name: long_name,
            is_active: false,
            unread_count: 0,
            unread_muted: false,
        };
        let label = format_tray_account_label(&entry);
        assert_eq!(label.chars().count(), TRAY_ACCOUNT_MAX_NAME_LEN + 1);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn aggregate_account_badges_respects_per_account_mute_and_count_modes() {
        let acct0_muted_only = AccountUnreadEntry {
            state: UnreadBadgeState {
                messages: 4,
                messages_muted: 4,
                chats: 2,
                chats_muted: 2,
                ..UnreadBadgeState::default()
            },
            include_muted_counter: true,
            count_unread_messages: true,
        };
        let acct1_unmuted_chats = AccountUnreadEntry {
            state: UnreadBadgeState {
                messages: 6,
                messages_muted: 0,
                chats: 2,
                chats_muted: 0,
                ..UnreadBadgeState::default()
            },
            include_muted_counter: false,
            count_unread_messages: false,
        };

        // acct0 contributes 4 muted msgs; acct1 contributes 2 unmuted chats -> total 6, unmuted!
        assert_eq!(
            aggregate_account_badges_per_account([acct0_muted_only, acct1_unmuted_chats]),
            UnreadBadgeResult {
                count: 6,
                muted: false,
            }
        );

        // Two all-muted accounts with include_muted_counter=true -> muted gray badge!
        assert_eq!(
            aggregate_account_badges_per_account([acct0_muted_only, acct0_muted_only]),
            UnreadBadgeResult {
                count: 8,
                muted: true,
            }
        );

        // Excluding muted on acct0 drops its contribution to 0.
        let mut acct0_no_muted = acct0_muted_only;
        acct0_no_muted.include_muted_counter = false;
        assert_eq!(
            aggregate_account_badges_per_account([acct0_no_muted]),
            UnreadBadgeResult {
                count: 0,
                muted: false,
            }
        );
    }
}
