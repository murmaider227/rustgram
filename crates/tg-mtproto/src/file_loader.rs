//! Port of `mtproto/dedicated_file_loader.cpp`: chunked file download coordination over
//! dedicated shifted DC sessions (`updater_dc_id` / `download_dc_id`).

use std::collections::VecDeque;
use std::time::Duration;

use crate::dc_options::{DcId, ShiftedDcId, updater_dc_id};
use crate::error::InvocationError;

/// Default chunk size (`DedicatedLoader::kChunkSize = 128 * 1024`).
pub const DEDICATED_CHUNK_SIZE: usize = 128 * 1024;
/// Maximum downloadable file size (`AbstractDedicatedLoader::kMaxFileSize = 512 * 1024 * 1024`).
pub const DEDICATED_MAX_FILE_SIZE: u64 = 512 * 1024 * 1024;
/// Maximum concurrent in-flight chunk requests (`DedicatedLoader::kRequestsCount = 4`).
pub const DEDICATED_CONCURRENT_REQUESTS: usize = 4;
/// Stagger delay between launching consecutive chunk requests (`kNextRequestDelay = 100ms`).
pub const DEDICATED_NEXT_REQUEST_DELAY: Duration = Duration::from_millis(100);

/// Progress snapshot reported by [`DedicatedFileDownloader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes written sequentially so far.
    pub already: u64,
    /// Total file size in bytes.
    pub total: u64,
}

#[derive(Debug)]
struct InFlightChunk {
    offset: u64,
    bytes: Option<Vec<u8>>,
}

/// Ordered chunk scheduler & resume validator porting `DedicatedLoader` / `AbstractDedicatedLoader`.
///
/// Manages in-order assembly of out-of-order chunk responses from up to
/// [`DEDICATED_CONCURRENT_REQUESTS`] concurrent `upload.getFile` calls on a dedicated shifted DC.
#[derive(Debug)]
pub struct DedicatedFileDownloader {
    dc_id: DcId,
    shifted_dc_id: ShiftedDcId,
    total_size: u64,
    chunk_size: usize,
    written_bytes: u64,
    next_offset: u64,
    in_flight: VecDeque<InFlightChunk>,
}

impl DedicatedFileDownloader {
    /// Creates a new downloader targeting `updater_dc_id(dc_id)` starting from `already_size`
    /// (aligned down to a multiple of `chunk_size`, matching `AbstractDedicatedLoader::validateOutput`).
    pub fn new(
        dc_id: DcId,
        total_size: u64,
        already_size: u64,
        chunk_size: usize,
    ) -> Result<Self, InvocationError> {
        if total_size == 0 || total_size > DEDICATED_MAX_FILE_SIZE || chunk_size == 0 {
            return Err(InvocationError::Protocol(format!(
                "invalid dedicated download size={total_size}, chunk={chunk_size}"
            )));
        }
        let aligned = if already_size < chunk_size as u64 || already_size > total_size {
            0
        } else {
            already_size - (already_size % chunk_size as u64)
        };
        Ok(Self {
            dc_id,
            shifted_dc_id: updater_dc_id(dc_id),
            total_size,
            chunk_size,
            written_bytes: aligned,
            next_offset: aligned,
            in_flight: VecDeque::new(),
        })
    }

    /// Bare DC ID hosting the file.
    pub const fn dc_id(&self) -> DcId {
        self.dc_id
    }

    /// Dedicated shifted DC ID to send chunk requests to.
    pub const fn shifted_dc_id(&self) -> ShiftedDcId {
        self.shifted_dc_id
    }

    /// Current download progress.
    pub const fn progress(&self) -> DownloadProgress {
        DownloadProgress {
            already: self.written_bytes,
            total: self.total_size,
        }
    }

    /// Returns `true` when all bytes have been delivered.
    pub const fn is_complete(&self) -> bool {
        self.written_bytes >= self.total_size
    }

    /// Claims the next `(offset, limit)` chunk to request, or `None` if at concurrency cap or EOF.
    pub fn next_chunk_request(&mut self) -> Option<(u64, usize)> {
        if self.in_flight.len() >= DEDICATED_CONCURRENT_REQUESTS
            || self.next_offset >= self.total_size
        {
            return None;
        }
        let offset = self.next_offset;
        self.next_offset = self.next_offset.saturating_add(self.chunk_size as u64);
        self.in_flight.push_back(InFlightChunk {
            offset,
            bytes: None,
        });
        Some((offset, self.chunk_size))
    }

    /// Records the received bytes for `offset` and drains all newly contiguous chunks from the
    /// head of the queue in exact file order (`DedicatedLoader::gotPart`).
    pub fn on_chunk_received(
        &mut self,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Vec<Vec<u8>>, InvocationError> {
        if data.is_empty() {
            return Err(InvocationError::Protocol(
                "empty chunk received in dedicated file loader".to_string(),
            ));
        }
        // Match `DownloadMtprotoTask::partLoaded` (`storage/download_manager_mtproto.cpp:922`) and
        // `DedicatedLoader::gotPart` (`mtproto/dedicated_file_loader.cpp:360-387`): reject chunks
        // larger than the requested limit or short non-final chunks that would leave holes.
        if data.len() > self.chunk_size {
            return Err(InvocationError::Protocol(format!(
                "dedicated chunk at offset {offset} size {} exceeds chunk_size {}",
                data.len(),
                self.chunk_size
            )));
        }
        if data.len() < self.chunk_size
            && offset.saturating_add(data.len() as u64) != self.total_size
        {
            return Err(InvocationError::Protocol(format!(
                "short non-final chunk at offset {offset}: got {} bytes, expected {} (total {})",
                data.len(),
                self.chunk_size,
                self.total_size
            )));
        }
        let entry = self
            .in_flight
            .iter_mut()
            .find(|c| c.offset == offset)
            .ok_or_else(|| {
                InvocationError::Protocol(format!("unexpected chunk offset {offset}"))
            })?;
        if entry.bytes.is_some() {
            return Err(InvocationError::Protocol(format!(
                "duplicate chunk received for offset {offset}"
            )));
        }
        entry.bytes = Some(data);

        let mut ready = Vec::new();
        while self
            .in_flight
            .front()
            .is_some_and(|front| front.bytes.is_some())
        {
            if let Some(chunk) = self.in_flight.pop_front()
                && let Some(bytes) = chunk.bytes
            {
                let next_written = self.written_bytes.saturating_add(bytes.len() as u64);
                if next_written > DEDICATED_MAX_FILE_SIZE {
                    return Err(InvocationError::Protocol(
                        "dedicated file download exceeds maximum size".to_string(),
                    ));
                }
                self.written_bytes = next_written;
                ready.push(bytes);
            }
        }
        Ok(ready)
    }
}
