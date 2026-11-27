//! [`std::io::Read`] + [`std::io::Seek`] over a [`StreamingReader`], for decoders outside FFmpeg
//! (the voice message player in `tg-audio`) that read a file while it downloads (TGB-3000).

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::Result;
use crate::ffmpeg::CustomIoSource;
use crate::streaming::loader::RangeLoader;
use crate::streaming::reader::StreamingReader;

/// A blocking reader of a file served by a [`RangeLoader`]: a read waits for the parts it needs
/// (the sliced reader prefetches ahead of it). Call it from a background thread only.
pub struct RangeReadSeek {
    // `Read` takes `&mut self`, so the lock is never contended; it only makes the reader `Sync`.
    reader: Mutex<StreamingReader>,
    size: u64,
}

impl RangeReadSeek {
    /// Reads the file `loader` serves, without a slice cache.
    ///
    /// # Errors
    /// The loader reports an empty file.
    pub fn new(loader: Arc<dyn RangeLoader>) -> Result<Self> {
        let size = loader.size();
        Ok(Self {
            reader: Mutex::new(StreamingReader::new(loader, None)?),
            size,
        })
    }

    /// The file size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    fn inner(&mut self) -> &mut StreamingReader {
        self.reader
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Read for RangeReadSeek {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner().read_bytes(buf)
    }
}

impl Seek for RangeReadSeek {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner().seek_to(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::loader::{MemoryRangeLoader, PART_SIZE};

    #[test]
    fn reads_and_seeks_across_parts_like_the_file() {
        let size = (PART_SIZE * 2 + 1234) as usize;
        let file: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let loader = Arc::new(MemoryRangeLoader::new(file.clone()).expect("loader"));
        let mut reader = RangeReadSeek::new(loader).expect("reader");
        assert_eq!(reader.size(), size as u64);

        let mut all = Vec::new();
        reader.read_to_end(&mut all).expect("read");
        assert_eq!(all, file);

        let tail = reader.seek(SeekFrom::End(-10)).expect("seek");
        assert_eq!(tail, size as u64 - 10);
        let mut last = [0u8; 10];
        reader.read_exact(&mut last).expect("tail");
        assert_eq!(&last[..], &file[size - 10..]);

        reader
            .seek(SeekFrom::Start(PART_SIZE - 3))
            .expect("seek across a part boundary");
        let mut across = [0u8; 6];
        reader.read_exact(&mut across).expect("across");
        let at = PART_SIZE as usize - 3;
        assert_eq!(&across[..], &file[at..at + 6]);

        assert!(reader.seek(SeekFrom::Start(size as u64 + 1)).is_err());
    }
}
