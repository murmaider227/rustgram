//! The cache actor: reads never wait behind queued writes, hits are served from memory, the
//! paint path gets `Pending` instead of a disk read, and results of a replaced store are dropped.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{PhotoData, PhotoId, PhotoSize};
use tg_storage::AuthKey;
use tg_storage::cache::media_cache::derive_media_cache_key;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use super::store::Priority;
use super::*;

/// Answers every `upload.getFile` with `payload` and counts the calls.
#[derive(Clone)]
struct CountingRpc {
    payload: Arc<Vec<u8>>,
    get_file_calls: Arc<AtomicUsize>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    main_dc: Arc<Mutex<DcId>>,
}

impl CountingRpc {
    fn new(payload: Vec<u8>) -> Self {
        let (updates_tx, _) = broadcast::channel(4);
        Self {
            payload: Arc::new(payload),
            get_file_calls: Arc::new(AtomicUsize::new(0)),
            updates_tx,
            main_dc: Arc::new(Mutex::new(2)),
        }
    }
}

impl RpcClient for CountingRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc_id(), req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return Err(InvocationError::Dropped);
        }
        self.get_file_calls.fetch_add(1, Ordering::SeqCst);
        let raw = tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileJpeg,
            mtime: 0,
            bytes: self.payload.as_ref().clone(),
        })
        .to_bytes();
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        self.main_dc.lock().map_or(2, |dc| *dc)
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        if let Ok(mut dc) = self.main_dc.lock() {
            *dc = dc_id;
        }
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn keyed_cache(dir: &Path) -> LiveMediaCache {
    let key = derive_media_cache_key(&AuthKey::new([0x5Au8; 256]), "data");
    LiveMediaCache::open_with_key(dir, key).expect("open cache")
}

fn sample_photo(id: u64, size: usize) -> PhotoData {
    PhotoData {
        id: PhotoId(id),
        access_hash: 7,
        file_reference: vec![1],
        dc_id: 2,
        sizes: vec![PhotoSize {
            kind: "x".to_owned(),
            width: 800,
            height: 600,
            size: size as i64,
        }],
        stripped_thumb: Some(vec![1, 2, 3]),
        ..PhotoData::default()
    }
}

fn photo_key(photo: &PhotoData) -> String {
    photo_cache_key(2, photo.id.0, "x")
}

fn wait_for(mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn stored_and_reread_blobs_are_served_from_memory() {
    let dir = TempDir::new().expect("tempdir");
    let cache = keyed_cache(dir.path());
    cache
        .put_blob("photo:2:1:x", MediaCacheTag::Photo, b"PHOTO_ONE")
        .expect("put");
    assert_eq!(
        cache.get_blob("photo:2:1:x").as_deref(),
        Some(&b"PHOTO_ONE"[..])
    );
    assert_eq!(
        cache.counters().disk_reads(),
        0,
        "a just-stored blob needs no disk read"
    );

    // A cold handle on the same folder reads from disk once, then from memory.
    drop(cache);
    let cold = keyed_cache(dir.path());
    for _ in 0..5 {
        assert_eq!(
            cold.get_blob("photo:2:1:x").as_deref(),
            Some(&b"PHOTO_ONE"[..])
        );
    }
    assert_eq!(cold.counters().disk_reads(), 1);
    assert_eq!(cold.counters().memory_hits(), 4);

    // Known misses are remembered too, until the key is stored.
    assert!(cold.get_blob("photo:2:404:x").is_none());
    assert!(cold.get_blob("photo:2:404:x").is_none());
    assert_eq!(cold.counters().disk_reads(), 2);
    cold.put_blob("photo:2:404:x", MediaCacheTag::Photo, b"LATE")
        .expect("put late");
    assert_eq!(
        cold.get_blob("photo:2:404:x").as_deref(),
        Some(&b"LATE"[..])
    );
}

#[test]
fn a_read_jumps_ahead_of_queued_writes() {
    let dir = TempDir::new().expect("tempdir");
    let writer = keyed_cache(dir.path());
    writer
        .put_blob("photo:2:9:x", MediaCacheTag::Photo, b"ON_DISK")
        .expect("put");
    drop(writer);
    let cache = keyed_cache(dir.path());

    // Two slow writes: the first one is running when the read arrives, the second is queued.
    cache.actor.post(Priority::Write, |_| {
        std::thread::sleep(Duration::from_millis(250))
    });
    std::thread::sleep(Duration::from_millis(30));
    cache.actor.post(Priority::Write, |_| {
        std::thread::sleep(Duration::from_millis(600))
    });

    let started = Instant::now();
    assert_eq!(
        cache.get_blob("photo:2:9:x").as_deref(),
        Some(&b"ON_DISK"[..])
    );
    let waited = started.elapsed();
    println!("cache read behind a running + a queued write: waited {waited:?} (FIFO: ~820 ms)");
    assert!(
        waited < Duration::from_millis(600),
        "the read must not wait for the queued write, waited {waited:?}"
    );
}

#[test]
fn photo_descriptor_hit_is_pending_then_ready_without_a_download() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    let dir = TempDir::new().expect("tempdir");
    let photo = sample_photo(42, 9);
    let writer = keyed_cache(dir.path());
    writer
        .put_blob(&photo_key(&photo), MediaCacheTag::Photo, b"JPEG_BYTES")
        .expect("put");
    drop(writer);

    let cache = keyed_cache(dir.path());
    let rpc = CountingRpc::new(b"NETWORK".to_vec());
    let bridge = Arc::new(LiveBridgeState::new());
    let generation = bridge.generation();
    let media = MessageMedia::Photo(Box::new(photo));

    let first = cache
        .descriptor_for_media(&media, true, &rpc, runtime.handle(), &bridge)
        .expect("descriptor");
    assert!(matches!(first.photo_load, Some(PhotoLoadState::Pending)));
    assert_eq!(
        cache.counters().blocking_reads(),
        0,
        "the paint path never waits"
    );

    assert!(
        wait_for(|| bridge.generation() > generation),
        "UI must be notified"
    );
    let ready = cache
        .descriptor_for_media(&media, true, &rpc, runtime.handle(), &bridge)
        .expect("descriptor");
    match ready.photo_load {
        Some(PhotoLoadState::Ready { bytes }) => assert_eq!(&*bytes, b"JPEG_BYTES"),
        other => panic!("expected Ready, got {other:?}"),
    }
    assert_eq!(rpc.get_file_calls.load(Ordering::SeqCst), 0);
    assert_eq!(cache.counters().disk_reads(), 1);
}

#[test]
fn photo_descriptor_miss_starts_one_download_and_serves_it_from_memory() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    let dir = TempDir::new().expect("tempdir");
    let cache = keyed_cache(dir.path());
    let payload = b"DOWNLOADED_JPEG".to_vec();
    let photo = sample_photo(77, payload.len());
    let rpc = CountingRpc::new(payload.clone());
    let bridge = Arc::new(LiveBridgeState::new());
    let media = MessageMedia::Photo(Box::new(photo.clone()));

    // Many repaints while the read and the download run.
    let mut seen_loading = false;
    assert!(wait_for(|| {
        let desc = cache
            .descriptor_for_media(&media, true, &rpc, runtime.handle(), &bridge)
            .expect("descriptor");
        match desc.photo_load {
            Some(PhotoLoadState::Loading { .. }) => {
                seen_loading = true;
                false
            }
            Some(PhotoLoadState::Ready { bytes }) => &*bytes == payload.as_slice(),
            _ => false,
        }
    }));
    assert!(seen_loading || rpc.get_file_calls.load(Ordering::SeqCst) == 1);
    assert_eq!(rpc.get_file_calls.load(Ordering::SeqCst), 1, "one download");
    assert_eq!(
        cache.counters().disk_reads(),
        1,
        "one disk lookup (the miss)"
    );

    // The encrypted write lands on disk in the background.
    assert!(wait_for(|| cache.lock().writing.is_empty()));
    let cold = keyed_cache(dir.path());
    assert_eq!(
        cold.get_blob(&photo_key(&photo)).as_deref(),
        Some(payload.as_slice())
    );
}

#[test]
fn results_of_a_replaced_store_are_never_served() {
    let first_dir = TempDir::new().expect("tempdir a");
    let second_dir = TempDir::new().expect("tempdir b");
    let cache = keyed_cache(first_dir.path());
    cache
        .put_blob("photo:2:5:x", MediaCacheTag::Photo, b"ACCOUNT_A")
        .expect("put a");
    assert!(cache.get_blob("photo:2:5:x").is_some());

    let other_key = derive_media_cache_key(&AuthKey::new([0x11u8; 256]), "data");
    cache
        .reopen_at_with_key(second_dir.path(), other_key)
        .expect("reopen");
    assert!(
        cache.get_blob("photo:2:5:x").is_none(),
        "memory of the previous account must be dropped on reopen"
    );

    // A download that finishes for the old store is neither served nor written.
    let old_store = 0;
    cache.finish_download(
        old_store,
        "photo:2:6:x".to_owned(),
        MediaCacheTag::Photo,
        None,
        b"LATE_A".to_vec(),
    );
    assert!(cache.get_blob("photo:2:6:x").is_none());
    assert_eq!(cache.stats().photos_bytes, 0);
}
