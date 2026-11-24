//! Live sending of saved profile music tracks (`MusicAttachBox`) as real audio documents
//! via `messages.sendMedia` (1 track) or `messages.sendMultiMedia` (2+ tracks) with
//! `InputMediaDocument` constructed from [`SavedMusicTrack`].
//!
//! Port origin: `Telegram/SourceFiles/boxes/music_attach_box.cpp` and
//! `Telegram/SourceFiles/api/api_sending.cpp` (`SendExistingDocument`).

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::input_peer_for;
use tg_data::FullMsgId as DataFullMsgId;
use tg_text::TextWithEntities;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::LiveSession;
use crate::convert::{message_to_history_message, now_unix_i32, to_data_peer};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::upload::SendMediaOptions;
use tg_app_core::history::backend::{
    DocumentAttachment, HistoryMedia, HistoryMessage, MessageExtra,
};
use tg_app_core::info::backend::SavedMusicTrack;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const MUSIC_SEND_RPC_TIMEOUT: Duration = Duration::from_secs(30);

static NEXT_MUSIC_RANDOM_ID: AtomicI64 = AtomicI64::new(0x4D55_5349_4300_0001);

fn next_music_random_id() -> i64 {
    NEXT_MUSIC_RANDOM_ID.fetch_add(1, Ordering::Relaxed)
}

fn input_document_from_track(track: &SavedMusicTrack) -> tl_enums::InputDocument {
    tl_enums::InputDocument::InputDocument(tl_types::InputDocument {
        id: track.document_id,
        access_hash: track.access_hash,
        file_reference: track.file_reference.clone(),
    })
}

fn audio_attachment_from_track(track: &SavedMusicTrack) -> HistoryMedia {
    let base = if track.performer.trim().is_empty() {
        track.title.trim().to_owned()
    } else if track.title.trim().is_empty() {
        track.performer.trim().to_owned()
    } else {
        format!("{} - {}", track.performer.trim(), track.title.trim())
    };
    let file_name = if base.is_empty() {
        format!("track_{}.mp3", track.document_id)
    } else {
        format!("{base}.mp3")
    };
    HistoryMedia::Document(DocumentAttachment {
        file_name,
        size_bytes: track.size_bytes,
        mime_type: "audio/mpeg".to_owned(),
        thumb_rgb: None,
    })
}

/// Sends one or more [`SavedMusicTrack`] audio documents to `peer_id`.
///
/// - 1 track -> `messages.sendMedia` with `InputMediaDocument`
/// - 2+ tracks -> `messages.sendMultiMedia` with `InputSingleMedia` wrapping `InputMediaDocument`
pub fn send_saved_music_live(
    live: &mut LiveSession,
    peer_id: PeerId,
    tracks: &[SavedMusicTrack],
) -> Result<Vec<HistoryMessage>, String> {
    if tracks.is_empty() {
        return Err("Select at least one track to send.".to_owned());
    }

    if tracks.len() == 1 {
        let track = &tracks[0];
        let doc = input_document_from_track(track);
        let mut msg =
            live.send_existing_document(peer_id, doc, false, SendMediaOptions::default())?;
        if msg.media.is_none() {
            msg.media = Some(audio_attachment_from_track(track));
        }
        return Ok(vec![msg]);
    }

    let data_peer = to_data_peer(peer_id);
    let session_arc = live.runtime().session().clone();
    let input_peer = block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = session_arc.read().await;
        input_peer_for(&guard, data_peer)
    });
    if matches!(input_peer, tl_enums::InputPeer::Empty) {
        return Err("Peer not found in session.".to_owned());
    }

    let multi_media: Vec<tl_enums::InputSingleMedia> = tracks
        .iter()
        .map(|track| {
            tl_enums::InputSingleMedia::InputSingleMedia(tl_types::InputSingleMedia {
                media: tl_enums::InputMedia::Document(tl_types::InputMediaDocument {
                    spoiler: false,
                    id: input_document_from_track(track),
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                    query: None,
                }),
                random_id: next_music_random_id(),
                message: String::new(),
                entities: None,
            })
        })
        .collect();

    let req = tl_fns::messages::SendMultiMedia {
        silent: false,
        background: false,
        clear_draft: false,
        noforwards: false,
        update_stickersets_order: false,
        invert_media: false,
        allow_paid_floodskip: false,
        peer: input_peer,
        reply_to: None,
        multi_media,
        schedule_date: None,
        send_as: None,
        quick_reply_shortcut: None,
        effect: None,
        allow_paid_stars: None,
    };

    let rpc = live.runtime().rpc().clone();
    let updates = block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            MUSIC_SEND_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })?;

    let new_ids = live.apply_updates_to_session(&updates);
    live.bridge_state().notify_ui();

    let session_arc = live.runtime().session().clone();
    let (self_peer, self_name, loaded) =
        block_on_runtime(live.runtime().tokio_handle(), async move {
            let guard = session_arc.read().await;
            let self_dp = guard.self_peer_id();
            let self_name = guard
                .peer(self_dp)
                .map(|p| p.name().to_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "You".to_owned());
            let mut msgs = Vec::with_capacity(new_ids.len());
            for mid in &new_ids {
                if let Some(m) = guard.message(DataFullMsgId::new(data_peer, *mid)) {
                    msgs.push(message_to_history_message(&guard, m));
                }
            }
            (PeerId(self_dp.0), self_name, msgs)
        });

    if !loaded.is_empty() {
        return Ok(loaded);
    }

    let now = now_unix_i32();
    Ok(tracks
        .iter()
        .enumerate()
        .map(|(idx, track)| HistoryMessage {
            id: MsgId(idx as i32 + 1),
            peer_id,
            from_id: self_peer,
            sender_name: self_name.clone(),
            date_unix: now,
            outgoing: true,
            is_post: false,
            text: TextWithEntities::default(),
            media: Some(audio_attachment_from_track(track)),
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra::default(),
        })
        .collect())
}
