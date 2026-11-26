//! TGB-3021: every album tile asks the backend for its own picture (tiles are painted under
//! their own ids), and a tile that is still downloading is re-polled until it is ready.

use egui::{CentralPanel, Context, Frame, RawInput};
use tg_text::TextWithEntities;

use crate::history::HistoryWidget;
use crate::history::backend::{AlbumItem, HistoryMedia, HistoryMessage, MessageExtra};
use crate::history::inline_media::{InlineMediaDescriptor, PhotoLoadState};
use crate::history::mock::{MockHistoryStore, synthetic_stripped_thumb};
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};

fn frame(
    ctx: &Context,
    widget: &mut HistoryWidget,
    store: &mut MockHistoryStore,
    peer: PeerId,
    time: f64,
) {
    let palette = Palette::day();
    let input = RawInput {
        time: Some(time),
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let _ = widget.show_with_backend(
                ui,
                peer,
                store,
                true,
                false,
                false,
                &palette,
                &EnglishStrings,
            );
        });
    });
    out.textures_delta.clear();
}

fn tile(msg_id: MsgId, seed: u8) -> AlbumItem {
    AlbumItem {
        msg_id,
        width: 1280,
        height: 800,
        stripped_thumb: Some(synthetic_stripped_thumb(seed)),
        fallback_rgb: ([0xE0, 0x85, 0x52], [0x26, 0x3D, 0x6B]),
        video_duration_secs: None,
        label: format!("tile {}", msg_id.0),
    }
}

fn ready(byte: u8) -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        photo_load: Some(PhotoLoadState::Ready {
            bytes: vec![byte; 4].into(),
        }),
        ..InlineMediaDescriptor::default()
    }
}

fn photo_state(widget: &mut HistoryWidget, id: MsgId) -> Option<PhotoLoadState> {
    widget
        .inner_mut()
        .media_runtime_mut()
        .descriptor(id)
        .and_then(|d| d.photo_load.clone())
}

#[test]
fn album_tiles_get_their_own_pictures_and_downloading_tiles_are_repolled() {
    let mut store = MockHistoryStore::new();
    let peer = PeerId::from_chat(2001);
    let album_id = MsgId(9_800);
    let tiles = [MsgId(9_801), MsgId(9_802), MsgId(9_803)];
    store.push_message(
        peer,
        HistoryMessage {
            id: album_id,
            peer_id: peer,
            from_id: PeerId::from_user(1_001),
            sender_name: "Mira Vance".to_owned(),
            date_unix: 1_795_341_600,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple("Album"),
            media: Some(HistoryMedia::Album(vec![
                tile(tiles[0], 0x21),
                tile(tiles[1], 0x45),
                tile(tiles[2], 0x63),
            ])),
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
        },
    );
    store.set_media_descriptor(peer, tiles[0], ready(1));
    store.set_media_descriptor(peer, tiles[1], ready(2));
    store.set_media_descriptor(
        peer,
        tiles[2],
        InlineMediaDescriptor {
            photo_load: Some(PhotoLoadState::Loading {
                progress: Some(0.2),
                bytes_loaded: 20_000,
                total_bytes: 100_000,
            }),
            ..InlineMediaDescriptor::default()
        },
    );

    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    frame(&ctx, &mut widget, &mut store, peer, 0.0);
    frame(&ctx, &mut widget, &mut store, peer, 0.05);
    for id in &tiles[..2] {
        assert!(
            matches!(
                photo_state(&mut widget, *id),
                Some(PhotoLoadState::Ready { .. })
            ),
            "album tile {} must get its downloaded picture",
            id.0
        );
    }
    assert!(matches!(
        photo_state(&mut widget, tiles[2]),
        Some(PhotoLoadState::Loading { .. })
    ));

    // The last tile finishes downloading in the background.
    store.set_media_descriptor(peer, tiles[2], ready(3));
    frame(&ctx, &mut widget, &mut store, peer, 1.0);
    assert!(
        matches!(
            photo_state(&mut widget, tiles[2]),
            Some(PhotoLoadState::Ready { .. })
        ),
        "a downloading album tile must be re-polled until it is ready"
    );
}
