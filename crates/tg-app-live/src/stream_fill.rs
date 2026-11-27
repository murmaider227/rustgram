//! TGB-3115: a voice or round video message that played in full from a stream counts as
//! loaded. tdesktop's streaming reader marks the document loaded once every slice is in its
//! cache (`Streaming::Document` -> `DocumentData::setLoadedInMediaCache`), so the status line
//! drops the size and the next play reads the file.
//!
//! Here the loader that serves the stream notes the parts it served; once every part has been
//! served, the file is read back from the streamed store (no network: the parts are there) and
//! handed to `fill`, which puts it into the media cache under the document's key.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tg_video::{CacheKey, PART_SIZE, RangeLoader};

type Fill = Box<dyn FnOnce(Vec<u8>) + Send>;

/// A [`RangeLoader`] that hands the whole file to `fill` once a stream has read all of it.
pub(crate) struct FillingLoader {
    inner: Arc<dyn RangeLoader>,
    served: Mutex<Vec<bool>>,
    complete: AtomicBool,
    fill: Mutex<Option<Fill>>,
}

impl FillingLoader {
    pub(crate) fn new(
        inner: Arc<dyn RangeLoader>,
        fill: impl FnOnce(Vec<u8>) + Send + 'static,
    ) -> Self {
        let parts = usize::try_from(inner.size().div_ceil(PART_SIZE)).unwrap_or(0);
        Self {
            inner,
            served: Mutex::new(vec![false; parts]),
            complete: AtomicBool::new(false),
            fill: Mutex::new(Some(Box::new(fill))),
        }
    }

    /// Marks the parts `[offset, offset + len)` covers whole; the last one ends at the file end.
    fn note_served(&self, offset: u64, len: usize) {
        if self.complete.load(Ordering::Acquire) || len == 0 {
            return;
        }
        let size = self.inner.size();
        let end = offset.saturating_add(len as u64).min(size);
        let mut served = self.served.lock().unwrap_or_else(PoisonError::into_inner);
        let mut part = offset.div_ceil(PART_SIZE);
        while part * PART_SIZE < end {
            let part_end = ((part + 1) * PART_SIZE).min(size);
            if part_end > end {
                break;
            }
            if let Some(slot) = usize::try_from(part).ok().and_then(|i| served.get_mut(i)) {
                *slot = true;
            }
            part += 1;
        }
        if served.is_empty() || !served.iter().all(|s| *s) {
            return;
        }
        drop(served);
        if self.complete.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(fill) = self
            .fill
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let inner = Arc::clone(&self.inner);
        // Reading the file back can wait for the store; the decoder thread that served the
        // last part goes on meanwhile.
        let spawned = std::thread::Builder::new()
            .name("tg-stream-fill".to_owned())
            .spawn(move || {
                if let Some(bytes) = read_all(inner.as_ref()) {
                    fill(bytes);
                }
            });
        if let Err(err) = spawned {
            tracing::debug!("streamed file not kept: {err}");
        }
    }
}

fn read_all(loader: &dyn RangeLoader) -> Option<Vec<u8>> {
    let size = loader.size();
    let mut bytes = Vec::with_capacity(usize::try_from(size).ok()?);
    while (bytes.len() as u64) < size {
        let chunk = loader
            .load_range(bytes.len() as u64, PART_SIZE as usize)
            .map_err(|err| tracing::debug!("streamed file not kept: {err}"))
            .ok()?;
        if chunk.is_empty() {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    (bytes.len() as u64 == size).then_some(bytes)
}

impl RangeLoader for FillingLoader {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn base_cache_key(&self) -> CacheKey {
        self.inner.base_cache_key()
    }

    fn load_range(&self, offset: u64, len: usize) -> tg_video::Result<Vec<u8>> {
        let bytes = self.inner.load_range(offset, len)?;
        self.note_served(offset, bytes.len());
        Ok(bytes)
    }

    fn prefetch(&self, offset: u64, len: usize) {
        self.inner.prefetch(offset, len);
    }

    fn cancel(&self, offset: u64) {
        self.inner.cancel(offset);
    }

    fn reset_priorities(&self) {
        self.inner.reset_priorities();
    }

    fn set_priority(&self, priority: i32) {
        self.inner.set_priority(priority);
    }

    fn stop(&self) {
        self.inner.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use tg_video::MemoryRangeLoader;

    use super::*;

    fn file(parts: u64) -> Vec<u8> {
        (0..PART_SIZE * parts - 1234)
            .map(|i| (i % 251) as u8)
            .collect()
    }

    fn filling(bytes: Vec<u8>) -> (FillingLoader, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();
        let inner = Arc::new(MemoryRangeLoader::new(bytes).expect("loader"));
        let loader = FillingLoader::new(inner, move |bytes| {
            let _ = tx.send(bytes);
        });
        (loader, rx)
    }

    #[test]
    fn a_file_read_in_full_is_handed_over_once() {
        let bytes = file(3);
        let (loader, filled) = filling(bytes.clone());
        for part in [2, 0] {
            loader
                .load_range(part * PART_SIZE, PART_SIZE as usize)
                .expect("part");
        }
        assert!(filled.recv_timeout(Duration::from_millis(100)).is_err());
        loader
            .load_range(PART_SIZE, PART_SIZE as usize)
            .expect("part");
        let kept = filled.recv_timeout(Duration::from_secs(10)).expect("kept");
        assert_eq!(kept, bytes);
        loader.load_range(0, PART_SIZE as usize).expect("again");
        assert!(
            filled.recv_timeout(Duration::from_millis(100)).is_err(),
            "once"
        );
    }

    #[test]
    fn a_file_read_in_part_is_not_kept() {
        let (loader, filled) = filling(file(3));
        loader.load_range(0, PART_SIZE as usize).expect("part");
        // A short read inside the second part covers no part whole.
        loader.load_range(PART_SIZE + 10, 100).expect("short");
        loader
            .load_range(2 * PART_SIZE, PART_SIZE as usize)
            .expect("part");
        assert!(filled.recv_timeout(Duration::from_millis(200)).is_err());
    }
}
