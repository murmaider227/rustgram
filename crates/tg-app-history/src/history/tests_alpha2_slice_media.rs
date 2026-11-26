//! Live bug (alpha.2): photos stayed blurred until opened. A live chat opens with only the
//! messages the session already has (often just the last one); the rest comes through the
//! older / newer preloads, which never asked the backend for inline media, so those photos were
//! never downloaded. Every slice that enters the list must request (and so start) its media.

use tg_text::TextWithEntities;

use crate::history::backend::{
    HistoryBackend, HistoryMedia, HistoryMessage, MessageExtra, PhotoAttachment,
};
use crate::history::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use crate::history::inner::HistoryInnerWidget;
use crate::history::mock::{MockHistoryStore, synthetic_stripped_thumb};
use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind};

fn message(peer: PeerId, id: i32, photo: bool) -> HistoryMessage {
    HistoryMessage {
        id: MsgId(id),
        peer_id: peer,
        from_id: PeerId::from_user(1001),
        sender_name: "Mira Vance".to_owned(),
        date_unix: 1_795_341_600 + id,
        outgoing: false,
        is_post: false,
        text: TextWithEntities::simple(if photo { "" } else { "text" }),
        media: photo.then(|| {
            HistoryMedia::Photo(PhotoAttachment {
                width: 640,
                height: 400,
                stripped_thumb: Some(synthetic_stripped_thumb(0x31)),
                fallback_rgb: ([0x5E, 0x72, 0xE4], [0x82, 0x5E, 0xE4]),
                file_name: "landscape.jpg".to_owned(),
                size_bytes: 120_000,
            })
        }),
        service: None,
        reply: None,
        forward: None,
        reactions: Vec::new(),
        views: None,
        edited: false,
        is_read: true,
        is_sending: false,
        is_pinned: false,
        extra: MessageExtra::default(),
    }
}

fn loading() -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        photo_load: Some(PhotoLoadState::Loading {
            progress: None,
            bytes_loaded: 0,
            total_bytes: 120_000,
        }),
        ..InlineMediaDescriptor::default()
    }
}

fn store_with_photos(peer: PeerId, photo_ids: &[i32], text_ids: &[i32]) -> MockHistoryStore {
    let mut store = MockHistoryStore::new();
    let mut ids: Vec<(i32, bool)> = photo_ids
        .iter()
        .map(|&i| (i, true))
        .chain(text_ids.iter().map(|&i| (i, false)))
        .collect();
    ids.sort_unstable();
    for (id, photo) in ids {
        store.push_message(peer, message(peer, id, photo));
        if photo {
            store.set_media_descriptor(peer, MsgId(id), loading());
        }
    }
    store
}

fn has_descriptor(inner: &HistoryInnerWidget, id: i32) -> bool {
    inner.media_runtime().descriptor(MsgId(id)).is_some()
}

#[test]
fn photos_from_the_older_preload_request_their_media() {
    let peer = PeerId::from_chat(4101);
    let store = store_with_photos(peer, &[1, 2, 3], &[4]);
    let mut inner = HistoryInnerWidget::new();
    // The chat opened with just the last message, as a live session does right after login.
    let newest = store.load_slice_around(peer, MsgId::SHOW_AT_END, 0, 0);
    assert_eq!(newest.messages.len(), 1);
    inner.apply_slice(peer, newest, PeerKind::Group, true);

    inner.load_older_slice(&store, PeerKind::Group);
    assert_eq!(inner.messages().len(), 4);
    for id in [1, 2, 3] {
        assert!(
            has_descriptor(&inner, id),
            "photo {id} loaded by the older preload must request its media"
        );
    }
}

#[test]
fn photos_from_the_newer_preload_and_jumps_request_their_media() {
    let peer = PeerId::from_chat(4102);
    let store = store_with_photos(peer, &[2, 5, 9], &[1, 3, 4, 6, 7, 8]);
    let mut inner = HistoryInnerWidget::new();
    let oldest = store.load_slice_around(peer, MsgId(1), 0, 0);
    inner.apply_slice(peer, oldest, PeerKind::Group, false);

    inner.load_newer_slice(&store, PeerKind::Group);
    for id in [2, 5, 9] {
        assert!(
            has_descriptor(&inner, id),
            "photo {id} from the newer preload"
        );
    }

    let mut jumped = HistoryInnerWidget::new();
    jumped.apply_slice(
        peer,
        store.load_slice_around(peer, MsgId(1), 0, 0),
        PeerKind::Group,
        false,
    );
    jumped.jump_to_message(MsgId(9), &store, PeerKind::Group, 0);
    assert!(has_descriptor(&jumped, 9), "photo reached by a jump");
}
