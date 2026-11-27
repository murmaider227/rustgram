//! Where an inline download fetches an expired `file_reference` again (`Data::FileOrigin`).
//!
//! tdesktop's `FileLoader` answers `FILE_REFERENCE_EXPIRED` / `FILE_REFERENCE_INVALID` by asking
//! the server for the file's origin again (`Api::RefreshFileReference`: the message through
//! `messages.getMessages` / `channels.getMessages`) and repeats the request with the new
//! reference. Without an origin the download fails with that error, and the media cache retries
//! with the same stale reference after its cooldown, for ever.

use std::sync::Arc;

use tg_api::RpcClient;
use tg_data::{Message, Session};
use tg_media::transfer::FileOrigin;
use tg_tl::enums as tl_enums;
use tokio::sync::RwLock;

use crate::media::{
    download_file_bytes_with_progress_async, download_file_bytes_with_refresh_async,
};

/// The live session and the origin of one file.
#[derive(Clone)]
pub struct RefreshOrigin {
    session: Arc<RwLock<Session>>,
    origin: FileOrigin,
}

impl RefreshOrigin {
    /// The origin of a file attached to `msg` (`Data::FileOriginMessage`).
    #[must_use]
    pub fn message(session: &Arc<RwLock<Session>>, msg: &Message) -> Self {
        Self {
            session: Arc::clone(session),
            origin: FileOrigin {
                peer_id: Some(msg.peer_id.0),
                msg_id: Some(msg.id.0 as i32),
                sticker_set_id: None,
            },
        }
    }
}

/// Downloads `location`, refreshing its `file_reference` from `refresh` when the server says
/// it expired.
pub(super) async fn download<C: RpcClient + Clone + 'static>(
    rpc: &C,
    refresh: Option<RefreshOrigin>,
    dc_id: i32,
    location: tl_enums::InputFileLocation,
    expected_size: Option<u64>,
    on_progress: impl FnMut(u64, u64),
) -> anyhow::Result<Vec<u8>> {
    match refresh {
        Some(RefreshOrigin { session, origin }) => {
            download_file_bytes_with_refresh_async(
                rpc,
                &session,
                origin,
                dc_id,
                location,
                expected_size,
                on_progress,
            )
            .await
        }
        None => {
            download_file_bytes_with_progress_async(
                rpc,
                dc_id,
                location,
                expected_size,
                on_progress,
            )
            .await
        }
    }
}
