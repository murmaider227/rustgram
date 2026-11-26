//! Images Telegram fetches from the web for the client (`WebDocument`), such as an invoice's
//! product photo, loaded with `upload.getWebFile`.
//!
//! Port origin: `Data::Session::photoFromWeb` with `Images::FromWebDocument`
//! (`data/data_session.cpp`, `ui/image/image_location_factory.cpp`) and the web file loader
//! (`storage/file_download_mtproto.cpp`).
//!
//! Internal design differs from tdesktop: there the web file becomes a `PhotoData` with its own
//! loader object. Here one small table per session, keyed by URL, holds the bytes or the
//! running request; a lookup never blocks and a failed request is asked again after a pause.
//! Only `webDocument` (with an access hash) is fetched; `webDocumentNoProxy`, which tdesktop
//! loads straight from its URL, is not.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use tg_api::RpcClient;
use tg_app_core::info::backend::SharedMediaThumb;
use tg_data::WebDocumentData;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tracing::debug;

use crate::bridge::LiveBridgeState;
use crate::session::LiveSession;

/// Chunk size of one `upload.getWebFile` call.
const CHUNK_LIMIT: i32 = 128 * 1024;
/// Largest web image kept (a product photo is a few hundred KiB).
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Images kept per session before the table starts over.
const MAX_ENTRIES: usize = 128;
/// Pause before a failed image is asked for again.
const RETRY_AFTER: Duration = Duration::from_secs(30);

enum Entry {
    Loading,
    Ready(Arc<[u8]>),
    Failed(Instant),
}

struct Table {
    owner: Weak<LiveBridgeState>,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

fn table_for(bridge: &Arc<LiveBridgeState>) -> Arc<Mutex<HashMap<String, Entry>>> {
    static TABLES: OnceLock<Mutex<HashMap<usize, Table>>> = OnceLock::new();
    let key = Arc::as_ptr(bridge) as usize;
    let owner = Arc::downgrade(bridge);
    let mut tables = TABLES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    tables.retain(|_, table| table.owner.strong_count() > 0);
    if let Some(table) = tables.get(&key)
        && table.owner.ptr_eq(&owner)
    {
        return Arc::clone(&table.entries);
    }
    let entries = Arc::new(Mutex::new(HashMap::new()));
    tables.insert(
        key,
        Table {
            owner,
            entries: Arc::clone(&entries),
        },
    );
    entries
}

/// Downloads `web` with `upload.getWebFile`.
pub async fn fetch_web_document<C: RpcClient>(
    rpc: &C,
    web: &WebDocumentData,
) -> Result<Vec<u8>, String> {
    let access_hash = web
        .access_hash
        .ok_or_else(|| "web document without an access hash".to_owned())?;
    let mut out = Vec::new();
    loop {
        let req = tl_fns::upload::GetWebFile {
            location: tl_enums::InputWebFileLocation::InputWebFileLocation(
                tl_types::InputWebFileLocation {
                    url: web.url.clone(),
                    access_hash,
                },
            ),
            offset: i32::try_from(out.len()).map_err(|_| "web file too large".to_owned())?,
            limit: CHUNK_LIMIT,
        };
        let resp = rpc.invoke(&req).await.map_err(|e| {
            let err: tg_api::InvocationError = e.into();
            format!("upload.getWebFile failed: {err}")
        })?;
        let tl_enums::upload::WebFile::WebFile(file) = resp;
        let chunk = file.bytes.len();
        if out.len().saturating_add(chunk) > MAX_BYTES {
            return Err("web file exceeds the size limit".to_owned());
        }
        out.extend_from_slice(&file.bytes);
        let expected = usize::try_from(file.size).unwrap_or(0);
        if chunk == 0 || chunk < CHUNK_LIMIT as usize || (expected > 0 && out.len() >= expected) {
            break;
        }
    }
    if out.is_empty() {
        return Err("empty web file".to_owned());
    }
    Ok(out)
}

/// The bytes of `web`, or `Loading` while they are fetched in the background (the UI is
/// notified when they arrive).
pub fn fetch(live: &LiveSession, web: &WebDocumentData) -> SharedMediaThumb {
    if web.access_hash.is_none() {
        return SharedMediaThumb::Unavailable;
    }
    let entries = table_for(live.bridge_state());
    {
        let mut guard = entries.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get(&web.url) {
            Some(Entry::Ready(bytes)) => {
                return SharedMediaThumb::Ready {
                    bytes: bytes.to_vec(),
                };
            }
            Some(Entry::Loading) => {
                return SharedMediaThumb::Loading {
                    stripped_thumb: None,
                };
            }
            Some(Entry::Failed(at)) if at.elapsed() < RETRY_AFTER => {
                return SharedMediaThumb::Unavailable;
            }
            Some(Entry::Failed(_)) | None => {}
        }
        if guard.len() >= MAX_ENTRIES {
            guard.retain(|_, entry| matches!(entry, Entry::Loading));
        }
        guard.insert(web.url.clone(), Entry::Loading);
    }
    let rpc = live.runtime().rpc().clone();
    let bridge = Arc::downgrade(live.bridge_state());
    let web = web.clone();
    live.runtime().tokio_handle().spawn(async move {
        let result = fetch_web_document(&rpc, &web).await;
        let entry = match result {
            Ok(bytes) => Entry::Ready(bytes.into()),
            Err(error) => {
                debug!(%error, "web image download failed");
                Entry::Failed(Instant::now())
            }
        };
        entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(web.url, entry);
        if let Some(bridge) = bridge.upgrade() {
            bridge.notify_ui();
        }
    });
    SharedMediaThumb::Loading {
        stripped_thumb: None,
    }
}
