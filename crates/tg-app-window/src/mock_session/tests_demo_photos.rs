//! TGB-3021: the demo chat photos (album tiles, the channel post photo) come with their full
//! pictures, so the history widget shows real images instead of blurred thumbnails.

use std::sync::Arc;

use tg_app_core::core_types::dto::inline_media::PhotoLoadState;
use tg_app_core::stand_in::types::MsgId;
use tg_app_history::history::backend::HistoryBackend;

use super::{MockSession, session_peers};

fn ready_bytes(
    session: &MockSession,
    peer: tg_app_core::stand_in::types::PeerId,
    id: i32,
) -> Arc<[u8]> {
    let desc = session
        .inline_media_descriptor(peer, MsgId(id))
        .unwrap_or_else(|| panic!("demo photo {id} has no descriptor"));
    match desc.photo_load {
        Some(PhotoLoadState::Ready { bytes }) => bytes,
        other => panic!("demo photo {id} is not downloaded: {other:?}"),
    }
}

#[test]
fn demo_photos_and_album_tiles_have_their_full_pictures() {
    let session = MockSession::new(true);
    let cases = [
        (session_peers::GROUP_CORE, 102),
        (session_peers::GROUP_CORE, 1061),
        (session_peers::GROUP_CORE, 1062),
        (session_peers::GROUP_CORE, 1063),
        (session_peers::USER_MIRA, 201),
        (session_peers::CHANNEL_DEV_LOG, 301),
    ];
    for (peer, id) in cases {
        let bytes = ready_bytes(&session, peer, id);
        let image = image::load_from_memory(&bytes)
            .unwrap_or_else(|e| panic!("demo photo {id} does not decode: {e}"));
        assert!(
            image.width() >= 320 && image.height() >= 320,
            "demo photo {id} is a full picture, not a thumbnail ({}x{})",
            image.width(),
            image.height()
        );
        // Asked again (every chat reload), the same bytes come back, so decoded pixels stay.
        assert!(Arc::ptr_eq(&bytes, &ready_bytes(&session, peer, id)));
    }
}

#[test]
fn album_message_itself_and_unmapped_messages_have_no_photo() {
    let session = MockSession::new(true);
    // The album message paints its tiles under their own ids.
    assert!(
        session
            .inline_media_descriptor(session_peers::GROUP_CORE, MsgId(106))
            .is_none()
    );
    assert!(
        session
            .inline_media_descriptor(session_peers::GROUP_CORE, MsgId(101))
            .is_none()
    );
}
