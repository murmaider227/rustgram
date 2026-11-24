//! Send options, silent send, scheduled messages, send-when-online, reminders,
//! send context menu item building, and keyboard shortcut availability.
//!
//! Port of:
//! - `Telegram/SourceFiles/menu/menu_send.h` and `menu_send.cpp`
//! - `Telegram/SourceFiles/menu/menu_send_details.h`
//! - `Telegram/SourceFiles/history/history_widget.cpp` (`sendMenuDetails`)
//! - `Telegram/SourceFiles/history/view/history_view_schedule_box.cpp` (`CanScheduleUntilOnline`, `DefaultScheduleTime`)
//! - `Telegram/SourceFiles/api/api_common.h` and `api_common.cpp` (`SendOptions`, `kScheduledUntilOnlineTimestamp`)

use tg_data::{BusinessShortcutId, EffectId, PeerId, TimeId};

use crate::types::UserInfo;

/// Special `schedule_date` value meaning "send when the recipient comes online".
///
/// Exact constant `Api::kScheduledUntilOnlineTimestamp` (`0x7FFFFFFE`) in `api/api_common.h`.
pub const SCHEDULED_UNTIL_ONLINE_TIMESTAMP: TimeId = 0x7FFF_FFFE;

/// Default schedule dialog offset in seconds (+10 minutes),
/// matching `HistoryView::DefaultScheduleTime()` (`now + 600`).
pub const DEFAULT_SCHEDULE_OFFSET_SECS: i32 = 600;

/// Options attached to an outgoing message send request, matching `Api::SendOptions`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SendOptions {
    /// Paid media / comment star price (`0` when free).
    pub price: u64,
    /// Optional peer ID to send as (channel/anonymous admin).
    pub send_as: Option<PeerId>,
    /// Scheduled Unix timestamp (`0` for immediate, `SCHEDULED_UNTIL_ONLINE_TIMESTAMP` for online).
    pub scheduled: TimeId,
    /// Repeat interval in seconds for recurring scheduled messages (`0` if one-shot).
    pub schedule_repeat_period: TimeId,
    /// Business quick-reply shortcut ID (`0` if none).
    pub shortcut_id: BusinessShortcutId,
    /// Message effect ID (`0` if none).
    pub effect_id: EffectId,
    /// Send silently without notification sound.
    pub silent: bool,
    /// Place caption above media (`invert_media`).
    pub invert_caption: bool,
    /// Hide inline bot attribution.
    pub hide_via_bot: bool,
    /// Send media with spoiler cover.
    pub media_spoiler: bool,
    /// Self-destruct TTL in seconds (`0` for normal, `0x7FFFFFFF` for view-once).
    pub ttl_seconds: i32,
}

/// Constructs [`SendOptions`] for "Send When Online", matching `Api::DefaultSendWhenOnlineOptions`.
pub fn default_send_when_online_options(ctrl_pressed: bool) -> SendOptions {
    SendOptions {
        scheduled: SCHEDULED_UNTIL_ONLINE_TIMESTAMP,
        silent: ctrl_pressed,
        ..SendOptions::default()
    }
}

/// Computes the default initial schedule timestamp (`now + 600`), matching `DefaultScheduleTime()`.
pub const fn default_schedule_time(now: TimeId) -> TimeId {
    now + DEFAULT_SCHEDULE_OFFSET_SECS
}

/// Send menu mode determined for a chat/peer, matching `SendMenu::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SendMenuType {
    /// No send menu available (e.g. no peer, or replying to an ephemeral bot message).
    #[default]
    Disabled,
    /// Only "Send Without Sound" is allowed (e.g. paid messages with starsPerMessage > 0, or inside schedule box).
    SilentOnly,
    /// Both "Send Without Sound" and "Schedule Message" are allowed (groups, channels, bots, hidden-lastseen users).
    Scheduled,
    /// "Send Without Sound", "Schedule Message", and "Send When Online" (1-on-1 non-bot user with visible lastseen).
    ScheduledToUser,
    /// "Set a Reminder" only (Saved Messages / self chat — no silent option).
    Reminder,
    /// Special menu for editing paid livestream comment star price.
    EditCommentPrice,
}

/// Media spoiler toggle state in the send menu, matching `SendMenu::SpoilerState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpoilerState {
    /// Spoiler toggle not applicable.
    #[default]
    None,
    /// Spoiler is currently enabled (menu shows checked item -> triggers `SpoilerOff`).
    Enabled,
    /// Spoiler is possible but disabled (menu shows unchecked item -> triggers `SpoilerOn`).
    Possible,
}

/// Media caption placement state in the send menu, matching `SendMenu::CaptionState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptionState {
    /// Caption movement not applicable.
    #[default]
    None,
    /// Caption is currently below media (menu offers "Move Caption Up").
    Below,
    /// Caption is currently above media (menu offers "Move Caption Down").
    Above,
}

/// Photo compression quality state in the send menu, matching `SendMenu::PhotoQualityState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PhotoQualityState {
    /// Photo quality toggle not applicable.
    #[default]
    None,
    /// Standard quality (1280px; menu offers unchecked "Send in High Quality").
    Standard,
    /// High quality (2560px; menu offers checked "Send in High Quality").
    High,
}

/// Video cover state in the send menu, matching `SendMenu::CoverState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoverState {
    /// Video cover editing not applicable.
    #[default]
    None,
    /// Can add a video cover (menu shows "Edit Cover" only).
    Add,
    /// Already has a custom cover (menu shows "Edit Cover" and "Remove Cover").
    Has,
}

/// Full specification of options shown in a send context menu, matching `SendMenu::Details`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SendMenuDetails {
    /// Primary send menu type.
    pub menu_type: SendMenuType,
    /// Target peer ID.
    pub peer_id: Option<PeerId>,
    /// Optional forum topic root message ID.
    pub topic_root_id: i64,
    /// Media spoiler state.
    pub spoiler: SpoilerState,
    /// Caption position state.
    pub caption: CaptionState,
    /// Photo quality toggle state.
    pub photo_quality: PhotoQualityState,
    /// Video cover state.
    pub cover: CoverState,
    /// Paid media / livestream comment star price (`Some(0)` -> "Make Paid", `Some(n)` -> "Change Price").
    pub price: Option<u64>,
    /// Minimum star price for livestream comment editing.
    pub comment_price_min: Option<u64>,
    /// Whether message effects selector is attached (true for 1-on-1 user chats).
    pub effect_allowed: bool,
    /// Whether "Reply to Forwarded Message" action is shown.
    pub reply_to_forward: bool,
}

/// Target peer classification for determining [`SendMenuDetails`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendTargetPeer<'a> {
    /// 1-on-1 user chat (including Saved Messages and bots).
    User(&'a UserInfo),
    /// Basic group chat.
    BasicChat {
        /// Peer ID.
        peer_id: PeerId,
    },
    /// Megagroup (supergroup) or broadcast channel.
    Channel {
        /// Peer ID.
        peer_id: PeerId,
        /// Paid messages star requirement (0 when free).
        stars_per_message: u64,
    },
}

/// Returns `true` if "Send When Online" is supported for `peer`.
///
/// Exact port of `HistoryView::CanScheduleUntilOnline` in
/// `Telegram/SourceFiles/history/view/history_view_schedule_box.cpp`:
/// user chat, not self, not bot, lastseen not hidden, `stars_per_message == 0`, not notifications user.
pub fn can_schedule_until_online(peer: &SendTargetPeer<'_>) -> bool {
    match peer {
        SendTargetPeer::User(user) => {
            !user.is_self
                && !user.is_bot
                && !user.lastseen.is_hidden()
                && user.stars_per_message == 0
                && !user.is_notifications_user
        }
        SendTargetPeer::BasicChat { .. } | SendTargetPeer::Channel { .. } => false,
    }
}

/// Computes the [`SendMenuDetails`] for a chat peer.
///
/// Direct port of `HistoryWidget::sendMenuDetails()` in
/// `Telegram/SourceFiles/history/history_widget.cpp:6197-6215`.
pub fn send_menu_details_for_peer(
    peer: Option<&SendTargetPeer<'_>>,
    is_ephemeral_bot_reply: bool,
) -> SendMenuDetails {
    let Some(peer) = peer else {
        return SendMenuDetails {
            menu_type: SendMenuType::Disabled,
            ..SendMenuDetails::default()
        };
    };

    let (peer_id, stars_per_message, is_self, is_user) = match peer {
        SendTargetPeer::User(u) => (
            PeerId::from_user(u.id),
            u.stars_per_message,
            u.is_self,
            true,
        ),
        SendTargetPeer::BasicChat { peer_id } => (*peer_id, 0, false, false),
        SendTargetPeer::Channel {
            peer_id,
            stars_per_message,
        } => (*peer_id, *stars_per_message, false, false),
    };

    let menu_type = if is_ephemeral_bot_reply {
        SendMenuType::Disabled
    } else if stars_per_message > 0 {
        SendMenuType::SilentOnly
    } else if is_self {
        SendMenuType::Reminder
    } else if can_schedule_until_online(peer) {
        SendMenuType::ScheduledToUser
    } else {
        SendMenuType::Scheduled
    };

    SendMenuDetails {
        menu_type,
        peer_id: Some(peer_id),
        effect_allowed: is_user,
        ..SendMenuDetails::default()
    }
}

/// Action kind triggered by a send menu item, matching `SendMenu::ActionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMenuActionType {
    /// Send immediately (with `SendOptions { silent: true }` or `scheduled: UNTIL_ONLINE`).
    Send,
    /// Open the schedule / reminder date-time picker.
    Schedule,
    /// Enable media spoiler.
    SpoilerOn,
    /// Disable media spoiler.
    SpoilerOff,
    /// Move caption above media.
    CaptionUp,
    /// Move caption below media.
    CaptionDown,
    /// Enable high quality photo compression (2560px).
    PhotoQualityOn,
    /// Disable high quality photo compression (1280px).
    PhotoQualityOff,
    /// Open video cover editor.
    EditCover,
    /// Clear custom video cover.
    RemoveCover,
    /// Set, change, or remove paid media / livestream star price.
    ChangePrice,
    /// Reply to a forwarded message.
    ReplyToForward,
}

/// Logical entry in a built send context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendMenuItem {
    /// Send without sound (`SendOptions { silent: true }`).
    SendSilent,
    /// Schedule message (or "Set a Reminder" when `is_reminder` is true).
    Schedule {
        /// `true` in Saved Messages (`SendMenuType::Reminder`).
        is_reminder: bool,
    },
    /// Send when the user comes online (`SCHEDULED_UNTIL_ONLINE_TIMESTAMP`).
    SendWhenOnline,
    /// Reply to forwarded message.
    ReplyToForward,
    /// Visual separator between send actions and media option actions.
    Separator,
    /// Toggle high-quality photo sending (`checked == true` when currently `High`).
    PhotoQuality {
        /// Current checked state.
        checked: bool,
        /// Action triggered on click.
        action: SendMenuActionType,
    },
    /// Toggle spoiler effect (`checked == true` when currently `Enabled`).
    Spoiler {
        /// Current checked state.
        checked: bool,
        /// Action triggered on click.
        action: SendMenuActionType,
    },
    /// Move caption above or below media.
    CaptionPosition {
        /// Action triggered on click (`CaptionDown` if currently above, else `CaptionUp`).
        action: SendMenuActionType,
    },
    /// Edit video cover.
    EditCover,
    /// Clear custom video cover.
    RemoveCover,
    /// Change or set paid price (`has_existing_price` distinguishes "Change Price" vs "Make Paid").
    ChangePrice {
        /// `true` when `*price > 0`.
        has_existing_price: bool,
    },
    /// Edit paid livestream comment stars.
    EditCommentStars,
    /// Remove extra livestream comment stars (resetting to `comment_price_min`).
    RemoveCommentStars {
        /// Reset price value (`comment_price_min.unwrap_or(0)`).
        reset_to: u64,
    },
}

/// Result of building a send menu, matching `SendMenu::FillMenuResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltSendMenu {
    /// Menu would be empty; caller should skip showing it.
    Skipped,
    /// Menu prepared with ordered items (and whether effects panel may be attached).
    Prepared {
        /// Ordered menu items.
        items: Vec<SendMenuItem>,
        /// Whether message effects selector should be attached.
        attach_effects: bool,
    },
}

/// Builds the exact list of send context menu items for `details`.
///
/// Direct port of `SendMenu::FillSendMenu` and `FillEditCommentPriceMenu` in
/// `Telegram/SourceFiles/menu/menu_send.cpp:679-852`.
pub fn build_send_menu(details: &SendMenuDetails) -> BuiltSendMenu {
    let kind = details.menu_type;
    let sending = kind != SendMenuType::Disabled;
    let has_media_options = details.spoiler != SpoilerState::None
        || details.caption != CaptionState::None
        || details.photo_quality != PhotoQualityState::None
        || details.cover != CoverState::None
        || details.price.is_some();

    if !sending && !has_media_options {
        return BuiltSendMenu::Skipped;
    }

    if kind == SendMenuType::EditCommentPrice {
        let mut items = vec![SendMenuItem::EditCommentStars];
        if details.price.unwrap_or(0) > details.comment_price_min.unwrap_or(0) {
            items.push(SendMenuItem::RemoveCommentStars {
                reset_to: details.comment_price_min.unwrap_or(0),
            });
        }
        return BuiltSendMenu::Prepared {
            items,
            attach_effects: false,
        };
    }

    let mut items = Vec::new();

    if sending && kind != SendMenuType::Reminder {
        items.push(SendMenuItem::SendSilent);
    }
    if sending && kind != SendMenuType::SilentOnly {
        items.push(SendMenuItem::Schedule {
            is_reminder: kind == SendMenuType::Reminder,
        });
    }
    if sending && kind == SendMenuType::ScheduledToUser {
        items.push(SendMenuItem::SendWhenOnline);
    }
    if sending && details.reply_to_forward {
        items.push(SendMenuItem::ReplyToForward);
    }

    if sending && has_media_options {
        items.push(SendMenuItem::Separator);
    }

    if details.photo_quality != PhotoQualityState::None {
        let high = details.photo_quality == PhotoQualityState::High;
        items.push(SendMenuItem::PhotoQuality {
            checked: high,
            action: if high {
                SendMenuActionType::PhotoQualityOff
            } else {
                SendMenuActionType::PhotoQualityOn
            },
        });
    }
    if details.spoiler != SpoilerState::None {
        let spoilered = details.spoiler == SpoilerState::Enabled;
        items.push(SendMenuItem::Spoiler {
            checked: spoilered,
            action: if spoilered {
                SendMenuActionType::SpoilerOff
            } else {
                SendMenuActionType::SpoilerOn
            },
        });
    }
    if details.caption != CaptionState::None {
        let above = details.caption == CaptionState::Above;
        items.push(SendMenuItem::CaptionPosition {
            action: if above {
                SendMenuActionType::CaptionDown
            } else {
                SendMenuActionType::CaptionUp
            },
        });
    }
    if details.cover != CoverState::None {
        items.push(SendMenuItem::EditCover);
        if details.cover == CoverState::Has {
            items.push(SendMenuItem::RemoveCover);
        }
    }
    if let Some(price) = details.price {
        items.push(SendMenuItem::ChangePrice {
            has_existing_price: price > 0,
        });
    }

    BuiltSendMenu::Prepared {
        items,
        attach_effects: details.effect_allowed,
    }
}

/// Keyboard shortcut commands handled by `SendMenu::SetupMenuAndShortcuts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendShortcut {
    /// Send message silently (without notification).
    SendSilentMessage,
    /// Open schedule/reminder picker.
    ScheduleMessage,
    /// Send normally.
    JustSendMessage,
}

/// Returns whether `shortcut` is enabled for `menu_type`,
/// matching `SetupMenuAndShortcuts` in `menu/menu_send.cpp:891-924`.
pub const fn is_send_shortcut_allowed(menu_type: SendMenuType, shortcut: SendShortcut) -> bool {
    if matches!(menu_type, SendMenuType::Disabled) {
        return false;
    }
    match shortcut {
        SendShortcut::SendSilentMessage => !matches!(menu_type, SendMenuType::Reminder),
        SendShortcut::ScheduleMessage => !matches!(menu_type, SendMenuType::SilentOnly),
        SendShortcut::JustSendMessage => true,
    }
}

/// Computes the child send menu type shown on the "Schedule" button inside the Schedule Box,
/// matching `ScheduleBox` (`history_view_schedule_box.cpp:395-401`).
pub const fn schedule_box_child_menu_type(parent_type: SendMenuType) -> SendMenuType {
    match parent_type {
        SendMenuType::Disabled => SendMenuType::Disabled,
        _ => SendMenuType::SilentOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_data::{LastseenStatus, UserId};

    fn make_user(id: u64) -> UserInfo {
        UserInfo {
            id: UserId(id),
            lastseen: LastseenStatus::online_till_status(1_700_000_000, false, false),
            ..UserInfo::default()
        }
    }

    #[test]
    fn send_menu_type_for_each_chat_kind() {
        // 1. Saved Messages (self) -> Reminder only (no silent, effects allowed)
        let mut self_user = make_user(1);
        self_user.is_self = true;
        let details = send_menu_details_for_peer(Some(&SendTargetPeer::User(&self_user)), false);
        assert_eq!(details.menu_type, SendMenuType::Reminder);
        assert!(details.effect_allowed);
        assert_eq!(
            build_send_menu(&details),
            BuiltSendMenu::Prepared {
                items: vec![SendMenuItem::Schedule { is_reminder: true }],
                attach_effects: true,
            }
        );
        assert!(!is_send_shortcut_allowed(
            details.menu_type,
            SendShortcut::SendSilentMessage
        ));
        assert!(is_send_shortcut_allowed(
            details.menu_type,
            SendShortcut::ScheduleMessage
        ));

        // 2. Regular user with visible lastseen -> ScheduledToUser (silent + schedule + when online)
        let friend = make_user(2);
        let details = send_menu_details_for_peer(Some(&SendTargetPeer::User(&friend)), false);
        assert_eq!(details.menu_type, SendMenuType::ScheduledToUser);
        assert_eq!(
            build_send_menu(&details),
            BuiltSendMenu::Prepared {
                items: vec![
                    SendMenuItem::SendSilent,
                    SendMenuItem::Schedule { is_reminder: false },
                    SendMenuItem::SendWhenOnline,
                ],
                attach_effects: true,
            }
        );

        // 3. User with hidden lastseen -> Scheduled (no "Send When Online")
        let mut hidden_user = make_user(3);
        hidden_user.lastseen = LastseenStatus::recently(false);
        let details = send_menu_details_for_peer(Some(&SendTargetPeer::User(&hidden_user)), false);
        assert_eq!(details.menu_type, SendMenuType::Scheduled);

        // 4. Bot -> Scheduled (no "Send When Online")
        let mut bot = make_user(4);
        bot.is_bot = true;
        let details = send_menu_details_for_peer(Some(&SendTargetPeer::User(&bot)), false);
        assert_eq!(details.menu_type, SendMenuType::Scheduled);

        // 5. Notifications user (777000) -> Scheduled (no "Send When Online")
        let mut notif = make_user(777_000);
        notif.is_notifications_user = true;
        let details = send_menu_details_for_peer(Some(&SendTargetPeer::User(&notif)), false);
        assert_eq!(details.menu_type, SendMenuType::Scheduled);

        // 6. Paid messages user (stars_per_message > 0) -> SilentOnly
        let mut paid_user = make_user(5);
        paid_user.stars_per_message = 50;
        let details = send_menu_details_for_peer(Some(&SendTargetPeer::User(&paid_user)), false);
        assert_eq!(details.menu_type, SendMenuType::SilentOnly);
        assert!(!is_send_shortcut_allowed(
            details.menu_type,
            SendShortcut::ScheduleMessage
        ));
        assert!(is_send_shortcut_allowed(
            details.menu_type,
            SendShortcut::SendSilentMessage
        ));

        // 7. Group / Channel -> Scheduled, effect_allowed = false
        let details = send_menu_details_for_peer(
            Some(&SendTargetPeer::Channel {
                peer_id: PeerId(-100123),
                stars_per_message: 0,
            }),
            false,
        );
        assert_eq!(details.menu_type, SendMenuType::Scheduled);
        assert!(!details.effect_allowed);

        // 8. Ephemeral bot reply or no peer -> Disabled -> Skipped
        let details = send_menu_details_for_peer(Some(&SendTargetPeer::User(&friend)), true);
        assert_eq!(details.menu_type, SendMenuType::Disabled);
        assert_eq!(build_send_menu(&details), BuiltSendMenu::Skipped);
        assert_eq!(
            send_menu_details_for_peer(None, false).menu_type,
            SendMenuType::Disabled
        );
    }

    #[test]
    fn send_menu_media_options_separator_and_comment_price() {
        let details = SendMenuDetails {
            menu_type: SendMenuType::Scheduled,
            photo_quality: PhotoQualityState::High,
            spoiler: SpoilerState::Possible,
            caption: CaptionState::Below,
            cover: CoverState::Has,
            price: Some(100),
            reply_to_forward: true,
            ..SendMenuDetails::default()
        };
        assert_eq!(
            build_send_menu(&details),
            BuiltSendMenu::Prepared {
                items: vec![
                    SendMenuItem::SendSilent,
                    SendMenuItem::Schedule { is_reminder: false },
                    SendMenuItem::ReplyToForward,
                    SendMenuItem::Separator,
                    SendMenuItem::PhotoQuality {
                        checked: true,
                        action: SendMenuActionType::PhotoQualityOff,
                    },
                    SendMenuItem::Spoiler {
                        checked: false,
                        action: SendMenuActionType::SpoilerOn,
                    },
                    SendMenuItem::CaptionPosition {
                        action: SendMenuActionType::CaptionUp,
                    },
                    SendMenuItem::EditCover,
                    SendMenuItem::RemoveCover,
                    SendMenuItem::ChangePrice {
                        has_existing_price: true,
                    },
                ],
                attach_effects: false,
            }
        );

        // Disabled sending with media options only: no leading separator
        let media_only = SendMenuDetails {
            menu_type: SendMenuType::Disabled,
            spoiler: SpoilerState::Enabled,
            ..SendMenuDetails::default()
        };
        assert_eq!(
            build_send_menu(&media_only),
            BuiltSendMenu::Prepared {
                items: vec![SendMenuItem::Spoiler {
                    checked: true,
                    action: SendMenuActionType::SpoilerOff,
                }],
                attach_effects: false,
            }
        );

        // EditCommentPrice mode
        let comment_price = SendMenuDetails {
            menu_type: SendMenuType::EditCommentPrice,
            price: Some(50),
            comment_price_min: Some(10),
            ..SendMenuDetails::default()
        };
        assert_eq!(
            build_send_menu(&comment_price),
            BuiltSendMenu::Prepared {
                items: vec![
                    SendMenuItem::EditCommentStars,
                    SendMenuItem::RemoveCommentStars { reset_to: 10 },
                ],
                attach_effects: false,
            }
        );

        // Default send-when-online options & schedule time
        let online = default_send_when_online_options(true);
        assert_eq!(online.scheduled, SCHEDULED_UNTIL_ONLINE_TIMESTAMP);
        assert!(online.silent);
        assert_eq!(default_schedule_time(1000), 1600);
        assert_eq!(
            schedule_box_child_menu_type(SendMenuType::ScheduledToUser),
            SendMenuType::SilentOnly
        );
    }
}
