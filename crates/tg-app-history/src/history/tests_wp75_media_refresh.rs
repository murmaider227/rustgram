//! WP-75 stage-3: a photo that finishes downloading replaces its blurred thumbnail without
//! reopening the chat (the descriptor used to be read only in `load_peer`).

use egui::{CentralPanel, Context, Frame, RawInput};
use tg_text::TextWithEntities;

use crate::history::HistoryWidget;
use crate::history::backend::{HistoryMedia, HistoryMessage, MessageExtra, PhotoAttachment};
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

#[test]
fn downloading_photo_switches_to_ready_without_reopening_the_chat() {
    let mut store = MockHistoryStore::new();
    let peer = PeerId::from_chat(2001);
    let msg_id = MsgId(9_751);
    store.push_message(
        peer,
        HistoryMessage {
            id: msg_id,
            peer_id: peer,
            from_id: PeerId::from_user(1001),
            sender_name: "Mira Vance".to_owned(),
            date_unix: 1_795_341_600,
            outgoing: false,
            is_post: false,
            text: TextWithEntities::simple(""),
            media: Some(HistoryMedia::Photo(PhotoAttachment {
                width: 640,
                height: 400,
                stripped_thumb: Some(synthetic_stripped_thumb(0x31)),
                fallback_rgb: ([0x5E, 0x72, 0xE4], [0x82, 0x5E, 0xE4]),
                file_name: "landscape.jpg".to_owned(),
                size_bytes: 120_000,
            })),
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
    store.set_media_descriptor(
        peer,
        msg_id,
        InlineMediaDescriptor {
            photo_load: Some(PhotoLoadState::Loading {
                progress: Some(0.1),
                bytes_loaded: 12_000,
                total_bytes: 120_000,
            }),
            ..InlineMediaDescriptor::default()
        },
    );

    let ctx = Context::default();
    let mut widget = HistoryWidget::new();
    frame(&ctx, &mut widget, &mut store, peer, 0.0);
    frame(&ctx, &mut widget, &mut store, peer, 0.05);
    let state = |w: &mut HistoryWidget| {
        w.inner_mut()
            .media_runtime_mut()
            .descriptor(msg_id)
            .and_then(|d| d.photo_load.clone())
    };
    assert!(matches!(
        state(&mut widget),
        Some(PhotoLoadState::Loading { .. })
    ));

    // The download finishes in the background.
    store.set_media_descriptor(
        peer,
        msg_id,
        InlineMediaDescriptor {
            photo_load: Some(PhotoLoadState::Ready {
                bytes: vec![1, 2, 3].into(),
            }),
            ..InlineMediaDescriptor::default()
        },
    );
    frame(&ctx, &mut widget, &mut store, peer, 1.0);
    assert!(
        matches!(state(&mut widget), Some(PhotoLoadState::Ready { .. })),
        "a finished download must replace the blurred thumbnail on the next poll"
    );
}
