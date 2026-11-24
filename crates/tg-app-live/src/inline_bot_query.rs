//! Live `HistoryBackend::query_inline_bot` implementation via `contacts.resolveUsername` and
//! `messages.getInlineBotResults`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/inline_bots/inline_results_widget.cpp` (`sendRequest`)
//! - `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp` (inline bot lookup)

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for, peer_id_from_tl};
use tg_app_core::history::sections_model::{InlineBotResult, InlineBotResultKind};
use tg_data::{Peer, PeerId as DataPeerId, UserId as DataUserId};
use tg_tl::{enums as tl_enums, functions as tl_fns};
use tracing::warn;

use crate::bridge::LiveBridgeState;
use crate::convert::known_peer_ids;
use crate::session::LiveSession;

struct ResolvedBotsCache {
    owner: Weak<LiveBridgeState>,
    by_username: HashMap<String, DataUserId>,
}

fn resolved_bots_cache() -> &'static Mutex<HashMap<usize, ResolvedBotsCache>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, ResolvedBotsCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolves `@bot_username` (local session cache first, then `contacts.resolveUsername`) and
/// executes `messages.getInlineBotResults` for `query`, mapping the server's `messages.BotResults`
/// into UI [`InlineBotResult`] rows for the history composer popup.
pub(crate) fn query_inline_bot(
    live: &LiveSession,
    bot_username: &str,
    query: &str,
) -> Vec<InlineBotResult> {
    let uname = bot_username.trim().trim_start_matches('@');
    if uname.is_empty() {
        return Vec::new();
    }

    let Some(bot_input_user) = resolve_bot_input_user(live, uname) else {
        return Vec::new();
    };

    let peer = live.read_session(|session| {
        let self_peer = input_peer_for(session, session.self_peer_id());
        if matches!(self_peer, tl_enums::InputPeer::Empty) {
            tl_enums::InputPeer::Self_
        } else {
            self_peer
        }
    });

    let resp = match live.invoke_rpc(tl_fns::messages::GetInlineBotResults {
        bot: bot_input_user,
        peer,
        geo_point: None,
        query: query.to_owned(),
        offset: String::new(),
    }) {
        Ok(resp) => resp,
        Err(err) => {
            warn!(bot = uname, error = %err, "messages.getInlineBotResults failed");
            return Vec::new();
        }
    };

    let tl_enums::messages::BotResults::BotResults(br) = resp;
    if !br.users.is_empty() {
        live.write_session(|session| {
            apply_tl_users_and_chats(session, &br.users, &[]);
        });
    }

    br.results
        .iter()
        .map(|res| map_tl_inline_result(uname, res))
        .collect()
}

fn resolve_bot_input_user(live: &LiveSession, uname: &str) -> Option<tl_enums::InputUser> {
    let lower = uname.to_ascii_lowercase();
    let session_key = Arc::as_ptr(live.bridge_state()) as usize;

    // 1. Check per-session resolved-inline-bot cache (covers bots not in the dialog list).
    let cached_uid = {
        let mut map = resolved_bots_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.retain(|_, entry| entry.owner.strong_count() > 0);
        map.get(&session_key)
            .and_then(|entry| entry.by_username.get(&lower).copied())
    };
    if let Some(uid) = cached_uid {
        let input = live.read_session(|session| input_user_for(session, uid));
        if !matches!(input, tl_enums::InputUser::Empty) {
            return Some(input);
        }
    }

    // 2. Check bots already present in session dialogs.
    let cached = live.read_session(|session| {
        for pid in known_peer_ids(session) {
            if let Some(user) = session.peer(pid).and_then(Peer::as_user)
                && user.is_bot()
                && user.usernames.username().eq_ignore_ascii_case(uname)
            {
                let input = input_user_for(session, user.user_id());
                if !matches!(input, tl_enums::InputUser::Empty) {
                    return Some((user.user_id(), input));
                }
            }
        }
        None
    });
    if let Some((uid, input)) = cached {
        let mut map = resolved_bots_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(session_key).or_insert_with(|| ResolvedBotsCache {
            owner: Arc::downgrade(live.bridge_state()),
            by_username: HashMap::new(),
        });
        entry.by_username.insert(lower, uid);
        return Some(input);
    }

    // 3. Resolve remotely via `contacts.resolveUsername` and cache the resolved bot user.
    let resolved = match live.invoke_rpc(tl_fns::contacts::ResolveUsername {
        username: uname.to_owned(),
        referer: None,
    }) {
        Ok(tl_enums::contacts::ResolvedPeer::ResolvedPeer(rp)) => rp,
        Err(err) => {
            warn!(bot = uname, error = %err, "contacts.resolveUsername for inline bot failed");
            return None;
        }
    };

    live.write_session(|session| {
        apply_tl_users_and_chats(session, &resolved.users, &resolved.chats);
    });

    let bot_peer: DataPeerId = peer_id_from_tl(&resolved.peer);
    let bot_uid: DataUserId = bot_peer.to_user()?;
    let input = live.read_session(|session| input_user_for(session, bot_uid));
    if matches!(input, tl_enums::InputUser::Empty) {
        None
    } else {
        let mut map = resolved_bots_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(session_key).or_insert_with(|| ResolvedBotsCache {
            owner: Arc::downgrade(live.bridge_state()),
            by_username: HashMap::new(),
        });
        entry.by_username.insert(lower, bot_uid);
        Some(input)
    }
}

fn map_tl_inline_result(bot_username: &str, res: &tl_enums::BotInlineResult) -> InlineBotResult {
    let (id, kind_str, raw_title, raw_desc, raw_url, send_message) = match res {
        tl_enums::BotInlineResult::BotInlineResult(r) => (
            r.id.as_str(),
            r.r#type.as_str(),
            r.title.as_deref().unwrap_or_default(),
            r.description.as_deref().unwrap_or_default(),
            r.url.as_deref().unwrap_or_default(),
            &r.send_message,
        ),
        tl_enums::BotInlineResult::BotInlineMediaResult(r) => (
            r.id.as_str(),
            r.r#type.as_str(),
            r.title.as_deref().unwrap_or_default(),
            r.description.as_deref().unwrap_or_default(),
            "",
            &r.send_message,
        ),
    };

    let kind = match kind_str.to_ascii_lowercase().as_str() {
        "gif" | "mpeg4_gif" => InlineBotResultKind::Gif,
        "photo" | "sticker" | "video" => InlineBotResultKind::Photo,
        _ => InlineBotResultKind::Article,
    };

    let send_text = extract_send_text(send_message, raw_title, raw_url, id);
    let title = if !raw_title.is_empty() {
        raw_title.to_owned()
    } else if !send_text.is_empty() {
        send_text.chars().take(64).collect()
    } else {
        id.to_owned()
    };
    let description = if !raw_desc.is_empty() {
        raw_desc.to_owned()
    } else if !raw_url.is_empty() {
        raw_url.to_owned()
    } else {
        match kind {
            InlineBotResultKind::Gif => "Animated GIF".to_owned(),
            InlineBotResultKind::Photo => "Photo".to_owned(),
            InlineBotResultKind::Article => format!("@{bot_username}"),
        }
    };

    InlineBotResult {
        id: id.to_owned(),
        bot_username: bot_username.to_owned(),
        title,
        description,
        send_text,
        thumb_rgb: swatch_for_result(kind, id),
        kind,
    }
}

fn extract_send_text(
    msg: &tl_enums::BotInlineMessage,
    fallback_title: &str,
    fallback_url: &str,
    fallback_id: &str,
) -> String {
    let text = match msg {
        tl_enums::BotInlineMessage::Text(m) => m.message.clone(),
        tl_enums::BotInlineMessage::MediaAuto(m) => m.message.clone(),
        tl_enums::BotInlineMessage::MediaGeo(m) => match &m.geo {
            tl_enums::GeoPoint::GeoPoint(g) => format!("Location: {:.5}, {:.5}", g.lat, g.long),
            tl_enums::GeoPoint::Empty => "Location".to_owned(),
        },
        tl_enums::BotInlineMessage::MediaVenue(m) => {
            if m.address.is_empty() {
                m.title.clone()
            } else {
                format!("{} — {}", m.title, m.address)
            }
        }
        tl_enums::BotInlineMessage::MediaContact(m) => {
            let name = format!("{} {}", m.first_name, m.last_name)
                .trim()
                .to_owned();
            if m.phone_number.is_empty() {
                name
            } else {
                format!("{name} ({})", m.phone_number)
            }
        }
        tl_enums::BotInlineMessage::MediaInvoice(m) => m.title.clone(),
        tl_enums::BotInlineMessage::MediaWebPage(m) => {
            if !m.message.is_empty() {
                m.message.clone()
            } else {
                m.url.clone()
            }
        }
        tl_enums::BotInlineMessage::RichMessage(_) => "Rich message".to_owned(),
        tl_enums::BotInlineMessage::Widget(_) => "Interactive widget".to_owned(),
    };

    if !text.is_empty() {
        text
    } else if !fallback_title.is_empty() {
        fallback_title.to_owned()
    } else if !fallback_url.is_empty() {
        fallback_url.to_owned()
    } else {
        fallback_id.to_owned()
    }
}

fn swatch_for_result(kind: InlineBotResultKind, id: &str) -> [u8; 3] {
    let hash = id
        .bytes()
        .fold(0_u8, |acc, b| acc.wrapping_mul(31).wrapping_add(b));
    match kind {
        InlineBotResultKind::Gif => match hash % 3 {
            0 => [0x3B, 0x7A, 0xA8],
            1 => [0x58, 0x9C, 0x62],
            _ => [0x92, 0x5E, 0xB8],
        },
        InlineBotResultKind::Photo => match hash % 3 {
            0 => [0x4A, 0x8F, 0xC4],
            1 => [0xC9, 0x7D, 0x4E],
            _ => [0x4D, 0x99, 0x89],
        },
        InlineBotResultKind::Article => [0x4B, 0x7B, 0xA2],
    }
}
