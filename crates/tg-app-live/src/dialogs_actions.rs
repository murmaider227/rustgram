//! Live dialogs actions for the Mute submenu (timed mute, sound on/off, per-chat tone),
//! block confirmation (report spam + block + delete chat), conversation deletion, and bot restart.
//!
//! Port origin:
//! - `Telegram/SourceFiles/menu/menu_mute.cpp` (`ThreadDescriptor`, `FillMuteMenu`)
//! - `Telegram/SourceFiles/data/notify/data_peer_notify_settings.cpp`
//! - `Telegram/SourceFiles/data/notify/data_notify_settings.cpp`
//! - `Telegram/SourceFiles/window/window_peer_menu.cpp` (`PeerMenuBlockUserBox`,
//!   `PeerMenuUnblockUserWithBotRestart`)
//! - `Telegram/SourceFiles/apiwrap.cpp` (`ApiWrap::deleteConversation`)

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, Weak};

use tg_app_core::notifications::settings::NotifySound;
use tg_app_core::stand_in::types::PeerId;
use tg_crypto::random_u64;
use tg_data::{BlockStatus, MsgId as DataMsgId, PeerBarSettings, PeerNotifySettings};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::warn;

use crate::bridge::LiveBridgeState;
use crate::convert::{history_messages_ordered, now_unix_i32, to_data_peer};
use crate::session::LiveSession;

#[derive(Default)]
struct DialogNotifyExtras {
    /// Last non-silent sound configured for a peer, so disabling sound (`None`) and re-enabling
    /// restores the custom ringtone or local sound (`menu/menu_mute.cpp:328-333`).
    previous_sounds: HashMap<PeerId, NotifySound>,
}

struct RegistryEntry {
    owner: Weak<LiveBridgeState>,
    state: DialogNotifyExtras,
}

fn registry() -> &'static Mutex<HashMap<usize, RegistryEntry>> {
    static REG: OnceLock<Mutex<HashMap<usize, RegistryEntry>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_notify_extras<R>(live: &LiveSession, f: impl FnOnce(&mut DialogNotifyExtras) -> R) -> R {
    let bridge = live.bridge.state();
    let key = std::sync::Arc::as_ptr(bridge) as usize;
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = reg.entry(key).or_insert_with(|| RegistryEntry {
        owner: std::sync::Arc::downgrade(bridge),
        state: DialogNotifyExtras::default(),
    });
    f(&mut entry.state)
}

/// Converts a `tg_data::PeerNotifySettings.sound` stored token into `tg_app_core::notifications::settings::NotifySound`.
#[must_use]
pub(crate) fn data_sound_to_ui(raw: Option<&str>) -> NotifySound {
    match raw {
        None | Some("") | Some("default") => NotifySound::default_sound(),
        Some("none") => NotifySound::silent(),
        Some(s) => {
            if let Some(id_str) = s.strip_prefix("ringtone:")
                && let Ok(id) = id_str.parse::<u64>()
                && id != 0
            {
                return NotifySound::ringtone(id);
            }
            if let Some((title, data)) = s.split_once('|') {
                NotifySound::local(title, data)
            } else {
                NotifySound::local(s, s)
            }
        }
    }
}

/// Converts a UI [`NotifySound`] into TL `NotificationSound` and the canonical `tg_data` token.
#[must_use]
pub(crate) fn ui_sound_to_tl_and_token(
    sound: &NotifySound,
) -> (tl_enums::NotificationSound, String) {
    if sound.none {
        (tl_enums::NotificationSound::None, "none".to_owned())
    } else if sound.id != 0 {
        (
            tl_enums::NotificationSound::Ringtone(tl_types::NotificationSoundRingtone {
                id: sound.id as i64,
            }),
            format!("ringtone:{}", sound.id),
        )
    } else if !sound.title.is_empty() || !sound.data.is_empty() {
        let title = if sound.title.is_empty() {
            sound.data.clone()
        } else {
            sound.title.clone()
        };
        let data = if sound.data.is_empty() {
            sound.title.clone()
        } else {
            sound.data.clone()
        };
        let token = if title == data {
            data.clone()
        } else {
            format!("{title}|{data}")
        };
        (
            tl_enums::NotificationSound::Local(tl_types::NotificationSoundLocal { title, data }),
            token,
        )
    } else {
        (tl_enums::NotificationSound::Default, "default".to_owned())
    }
}

fn build_input_notify_settings(
    existing: &PeerNotifySettings,
    mute_until_override: Option<i32>,
    sound_override: Option<&NotifySound>,
) -> tl_enums::InputPeerNotifySettings {
    let mute_until = mute_until_override.or(existing.mute_until);
    let sound = if let Some(s) = sound_override {
        Some(ui_sound_to_tl_and_token(s).0)
    } else {
        existing
            .sound
            .as_deref()
            .map(|raw| ui_sound_to_tl_and_token(&data_sound_to_ui(Some(raw))).0)
    };
    tl_enums::InputPeerNotifySettings::InputPeerNotifySettings(tl_types::InputPeerNotifySettings {
        show_previews: existing.show_previews,
        silent: existing.silent,
        mute_until,
        sound,
        stories_muted: existing.stories_muted,
        stories_hide_sender: None,
        stories_sound: None,
    })
}

/// Computes `mute_until` Unix timestamp from `seconds` and `now_unix`, matching
/// `MuteMenu::ThreadDescriptor` (`menu/menu_mute.cpp:238-247`) and `MuteValue::until`:
/// - `seconds <= 0` -> `0` (unmute)
/// - `seconds == i32::MAX` -> `i32::MAX` (forever)
/// - `0 < seconds < i32::MAX` -> `min(now_unix + seconds, i32::MAX)`
#[must_use]
pub fn compute_mute_until(now_unix: i32, seconds: i32) -> i32 {
    if seconds <= 0 {
        0
    } else if seconds == i32::MAX {
        i32::MAX
    } else {
        i64::from(now_unix)
            .saturating_add(i64::from(seconds))
            .clamp(0, i64::from(i32::MAX)) as i32
    }
}

/// Mutes or unmutes `peer_id` for `seconds` via `account.updateNotifySettings`.
pub(crate) fn mute_dialog_for(
    live: &LiveSession,
    peer_id: PeerId,
    seconds: i32,
) -> Result<bool, String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for account.updateNotifySettings.".to_owned());
    };
    let data_peer = to_data_peer(peer_id);
    let now = now_unix_i32();
    let mute_until = compute_mute_until(now, seconds);
    let existing = live.read_session(|s| {
        s.peer(data_peer)
            .map(|p| p.common().notify.clone())
            .unwrap_or_default()
    });
    let settings = build_input_notify_settings(&existing, Some(mute_until), None);
    let req = tl_fns::account::UpdateNotifySettings {
        peer: tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
            peer: input_peer,
        }),
        settings,
    };
    let _ = live.invoke_rpc(req)?;

    let is_muted = mute_until > now;
    live.write_session(|s| {
        if let Some(peer) = s.peer_mut(data_peer) {
            peer.common_mut().notify.mute_until = Some(mute_until);
        }
        s.set_history_muted(data_peer, is_muted);
    });
    if let Ok(mut c) = live.cache.lock() {
        c.notify_settings_version = c.notify_settings_version.wrapping_add(1);
    }
    live.bridge.state().notify_ui();
    Ok(is_muted)
}

/// Returns the active mute expiration timestamp for `peer_id` (`None` when unmuted).
#[must_use]
pub(crate) fn dialog_mute_until(live: &LiveSession, peer_id: PeerId) -> Option<i32> {
    let data_peer = to_data_peer(peer_id);
    let now = now_unix_i32();
    live.read_session(|s| {
        let peer_until = s
            .peer(data_peer)
            .and_then(|p| p.common().notify.mute_until)
            .filter(|&until| until > now);
        if peer_until.is_some() {
            peer_until
        } else if s.history(data_peer).is_some_and(|h| h.muted) {
            Some(i32::MAX)
        } else {
            None
        }
    })
}

/// Returns the current per-chat [`NotifySound`] for `peer_id`.
#[must_use]
pub(crate) fn dialog_notify_sound(live: &LiveSession, peer_id: PeerId) -> NotifySound {
    let data_peer = to_data_peer(peer_id);
    live.read_session(|s| {
        let raw = s
            .peer(data_peer)
            .and_then(|p| p.common().notify.sound.as_deref());
        data_sound_to_ui(raw)
    })
}

/// Sets a per-chat notification tone (`Default`, `Silent`, local sound, or cloud ringtone) via
/// `account.updateNotifySettings` (`menu/menu_mute.cpp:235-237`).
pub(crate) fn set_dialog_notify_sound(
    live: &LiveSession,
    peer_id: PeerId,
    sound: NotifySound,
) -> Result<(), String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for account.updateNotifySettings.".to_owned());
    };
    let data_peer = to_data_peer(peer_id);
    let existing = live.read_session(|s| {
        s.peer(data_peer)
            .map(|p| p.common().notify.clone())
            .unwrap_or_default()
    });
    let (_, token) = ui_sound_to_tl_and_token(&sound);
    let settings = build_input_notify_settings(&existing, None, Some(&sound));
    let req = tl_fns::account::UpdateNotifySettings {
        peer: tl_enums::InputNotifyPeer::InputNotifyPeer(tl_types::InputNotifyPeer {
            peer: input_peer,
        }),
        settings,
    };
    let _ = live.invoke_rpc(req)?;

    if !sound.none {
        with_notify_extras(live, |st| {
            st.previous_sounds.insert(peer_id, sound);
        });
    } else {
        let prev = data_sound_to_ui(existing.sound.as_deref());
        if !prev.none {
            with_notify_extras(live, |st| {
                st.previous_sounds.entry(peer_id).or_insert(prev);
            });
        }
    }

    live.write_session(|s| {
        if let Some(peer) = s.peer_mut(data_peer) {
            peer.common_mut().notify.sound = Some(token);
        }
    });
    if let Ok(mut c) = live.cache.lock() {
        c.notify_settings_version = c.notify_settings_version.wrapping_add(1);
    }
    live.bridge.state().notify_ui();
    Ok(())
}

/// Toggles per-chat notification sound on (`enabled = true`) or off (`enabled = false`, silent),
/// restoring the previous non-silent tone when re-enabled (`menu/menu_mute.cpp:325-339`).
pub(crate) fn set_dialog_sound_enabled(
    live: &LiveSession,
    peer_id: PeerId,
    enabled: bool,
) -> Result<bool, String> {
    let target = if enabled {
        with_notify_extras(live, |st| {
            st.previous_sounds
                .get(&peer_id)
                .cloned()
                .filter(|s| !s.none)
                .unwrap_or_else(NotifySound::default_sound)
        })
    } else {
        let current = dialog_notify_sound(live, peer_id);
        if !current.none {
            with_notify_extras(live, |st| {
                st.previous_sounds.insert(peer_id, current);
            });
        }
        NotifySound::silent()
    };
    set_dialog_notify_sound(live, peer_id, target)?;
    Ok(enabled)
}

/// Returns `true` when `peer_id` has `PeerBarSettings::REPORT_SPAM` set
/// (`window_peer_menu.cpp:2770-2773`).
#[must_use]
pub(crate) fn dialog_suggests_report_spam(live: &LiveSession, peer_id: PeerId) -> bool {
    let data_peer = to_data_peer(peer_id);
    live.read_session(|s| {
        s.peer(data_peer).is_some_and(|p| {
            p.common()
                .bar_settings
                .contains(PeerBarSettings::REPORT_SPAM)
        })
    })
}

/// Deletes/leaves a conversation (`ApiWrap::deleteConversation`, `window_peer_menu.cpp:2864`):
/// - Supergroup/channel: `channels.leaveChannel` (updates applied via `SessionRuntime`).
/// - Basic group: `messages.deleteChatUser(self)` if still in group, followed by paginated
///   `messages.deleteHistory(just_clear: false, revoke)`.
/// - DM user/bot: paginated `messages.deleteHistory(just_clear: false, revoke)` with
///   `AffectedHistory` PTS fed into `SessionRuntime`.
pub(crate) fn delete_chat(live: &LiveSession, peer_id: PeerId, revoke: bool) -> Result<(), String> {
    let data_peer = to_data_peer(peer_id);
    let (max_id, basic_chat_leave) = live.read_session(|s| {
        let max_id = s
            .history(data_peer)
            .and_then(|h| h.top_message_id.or_else(|| h.loaded_ranges.max_id()))
            .map(|m| m.0 as i32)
            .unwrap_or(0);
        let chat_leave = data_peer.to_chat().and_then(|cid| {
            s.chat(cid)
                .is_some_and(|c| !c.have_left())
                .then_some(cid.to_tl())
        });
        (max_id, chat_leave)
    });

    if let Some(ch) = live.input_channel(peer_id) {
        let updates = live.invoke_rpc(tl_fns::channels::LeaveChannel { channel: ch })?;
        let _ = live.apply_updates_to_session(&updates);
    } else if let Some(input_peer) = live.input_peer(peer_id) {
        if let Some(chat_id) = basic_chat_leave {
            let updates = live.invoke_rpc(tl_fns::messages::DeleteChatUser {
                revoke_history: revoke,
                chat_id,
                user_id: tl_enums::InputUser::Self_,
            })?;
            let _ = live.apply_updates_to_session(&updates);
        }
        for _ in 0..tg_api::DELETE_HISTORY_RETRY_LIMIT {
            let tl_enums::messages::AffectedHistory::AffectedHistory(aff) =
                live.invoke_rpc(tl_fns::messages::DeleteHistory {
                    just_clear: false,
                    revoke,
                    peer: input_peer.clone(),
                    max_id,
                    min_date: None,
                    max_date: None,
                })?;
            let _ = live.apply_affected_history(Some(peer_id), aff.pts, aff.pts_count);
            if aff.offset <= 0 {
                break;
            }
        }
    } else {
        return Err("Peer not found in session for delete_chat.".to_owned());
    }

    let ids: Vec<DataMsgId> = live.read_session(|s| {
        history_messages_ordered(s, data_peer)
            .into_iter()
            .map(|m| m.id)
            .collect()
    });
    live.write_session(|s| {
        if !ids.is_empty() {
            s.delete_messages(data_peer, &ids);
        }
        s.set_history_unread_count(data_peer, 0);
        s.set_history_unread_mark(data_peer, false);
        if let Some(h) = s.history_mut(data_peer) {
            h.in_chat_list = false;
        }
        s.sync_dialog_lists(data_peer);
    });
    live.bridge.state().notify_ui();
    Ok(())
}

/// Blocks `peer_id` (`contacts.block`) and optionally reports spam (`messages.reportSpam`)
/// and/or deletes the chat (`delete_chat`) — matching `Window::PeerMenuBlockUserBox`
/// (`window/window_peer_menu.cpp:2855-2868`).
pub(crate) fn report_spam_and_block(
    live: &LiveSession,
    peer_id: PeerId,
    report_spam: bool,
    should_delete_chat: bool,
) -> Result<(), String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for contacts.block.".to_owned());
    };
    let data_peer = to_data_peer(peer_id);

    // 1. Block peer (`contacts.block`).
    let _ = live.invoke_rpc(tl_fns::contacts::Block {
        my_stories_from: false,
        id: input_peer.clone(),
    })?;
    live.write_session(|s| {
        if let Some(peer) = s.peer_mut(data_peer) {
            peer.common_mut().block_status = BlockStatus::Blocked;
        }
    });
    if let Ok(mut c) = live.cache.lock() {
        c.blocked_loaded = false;
    }

    // 2. Optionally report spam (`messages.reportSpam`).
    if report_spam {
        let _ = live.invoke_rpc(tl_fns::messages::ReportSpam { peer: input_peer })?;
        live.write_session(|s| {
            if let Some(peer) = s.peer_mut(data_peer) {
                peer.common_mut().bar_settings = PeerBarSettings::empty();
            }
        });
    }

    // 3. Optionally delete conversation (`ApiWrap::deleteConversation(peer, false)`).
    if should_delete_chat {
        delete_chat(live, peer_id, false)?;
    } else {
        live.bridge.state().notify_ui();
    }

    Ok(())
}

/// Unblocks `peer_id` (`contacts.unblock`) and, when `peer_id` is a non-support bot, sends
/// `/start` (`PeerMenuUnblockUserWithBotRestart`, `window/window_peer_menu.cpp:2879-2887`).
pub(crate) fn unblock_and_restart_bot(live: &LiveSession, peer_id: PeerId) -> Result<(), String> {
    let Some(input_peer) = live.input_peer(peer_id) else {
        return Err("Peer not found in session for contacts.unblock.".to_owned());
    };
    let data_peer = to_data_peer(peer_id);

    let _ = live.invoke_rpc(tl_fns::contacts::Unblock {
        my_stories_from: false,
        id: input_peer.clone(),
    })?;
    let is_restartable_bot = live.write_session(|s| {
        if let Some(peer) = s.peer_mut(data_peer) {
            peer.common_mut().block_status = BlockStatus::NotBlocked;
        }
        s.peer(data_peer)
            .and_then(|p| p.as_user())
            .is_some_and(|u| u.is_bot() && !u.is_support())
    });
    if let Ok(mut c) = live.cache.lock() {
        c.blocked_loaded = false;
    }

    if is_restartable_bot {
        let req = tl_fns::messages::SendMessage {
            no_webpage: false,
            silent: false,
            background: false,
            clear_draft: false,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: false,
            allow_paid_floodskip: false,
            peer: input_peer,
            reply_to: None,
            message: "/start".to_owned(),
            random_id: random_u64() as i64,
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            allow_paid_stars: None,
            suggested_post: None,
            rich_message: None,
        };
        match live.invoke_rpc(req) {
            Ok(updates) => {
                let _ = live.apply_updates_to_session(&updates);
            }
            Err(err) => {
                warn!(peer = peer_id.0, error = %err, "failed to send /start after unblocking bot");
                return Err(err);
            }
        }
    }

    live.bridge.state().notify_ui();
    Ok(())
}
