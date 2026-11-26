//! Diagnostics for TGB-2908: a message sent to Saved Messages showed on one side for about a
//! second and then moved to the other. Logs (INFO, target `saved_side`, so it reaches `log.txt`)
//! the shape of each self-chat message as the server sends it and when it replaces the local
//! pending copy. Never logs text, names or phone numbers.

use tg_data::{MsgId, PeerId, Session};
use tg_tl::enums as tl_enums;

/// Log target shared by every Saved Messages side diagnostic line.
pub const TARGET: &str = "saved_side";

/// Kind of a message author relative to the account: `self`, `other` or `none`.
#[must_use]
pub fn from_kind(from: Option<PeerId>, self_peer: PeerId) -> &'static str {
    match from {
        None => "none",
        Some(p) if p == self_peer => "self",
        Some(_) => "other",
    }
}

fn tl_peer_is_self(peer: &tl_enums::Peer, session: &Session) -> bool {
    matches!(peer, tl_enums::Peer::User(u) if u.user_id as u64 == session.self_user_id().0)
}

/// Logs the server's version of a Saved Messages message (`stage = "server"`).
pub fn log_server_message(session: &Session, msg: &tl_enums::Message, source: &'static str) {
    let tl_enums::Message::Message(m) = msg else {
        return;
    };
    if !tl_peer_is_self(&m.peer_id, session) {
        return;
    }
    let from = match &m.from_id {
        None => "none",
        Some(p) if tl_peer_is_self(p, session) => "self",
        Some(_) => "other",
    };
    tracing::info!(
        target: TARGET,
        stage = "server",
        source,
        msg_id = m.id,
        out = m.out,
        from,
        saved_peer = m.saved_peer_id.is_some(),
        fwd = m.fwd_from.is_some(),
        "saved messages: server message"
    );
}

/// Logs `updateMessageID` re-keying a Saved Messages pending message (`stage = "resolved"`).
pub fn log_resolved(
    local: Option<tg_data::FullMsgId>,
    session: &Session,
    random_id: i64,
    server_id: MsgId,
) {
    let Some(local) = local else {
        return;
    };
    if local.peer != session.self_peer_id() {
        return;
    }
    tracing::info!(
        target: TARGET,
        stage = "resolved",
        random_id,
        local_id = local.msg.0,
        server_id = server_id.0,
        "saved messages: pending message replaced by the server id"
    );
}
