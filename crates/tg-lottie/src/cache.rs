//! Compressed frame cache using YUV420 + 4-bit alpha, XOR frame deltas, and LZ4.
//!
//! Port of `lottie/details/lottie_cache.h`, `lottie/details/lottie_cache.cpp`,
//! `lottie/details/lottie_cache_frame_storage.h`, and
//! `lottie/details/lottie_cache_frame_storage.cpp`.

use lz4_flex::block::{compress, decompress};

use crate::common::{
    FrameBuffer, FrameRenderResult, FrameRequest, MAX_FRAME_RATE, MAX_FRAMES_COUNT, MAX_SIZE,
    NORMAL_FRAME_RATE, Size,
};

/// Maximum allowed serialized cache blob size (`kMaxCacheSize`, 10 MiB).
pub const MAX_CACHE_SIZE: usize = 10 * 1024 * 1024;

/// Size rounding enforced by the YUV420A4 cache (`Cache::sizeRounding()`).
pub const CACHE_SIZE_ROUNDING: u32 = 8;

/// Header byte size: 8 big-endian `i32`s (`encoder, w, h, orig_w, orig_h, fr, count, ready`).
const HEADER_SIZE: usize = 8 * 4;

/// Row alignment used in `EncodedStorage` (`kAlignStorage`).
const ALIGN_STORAGE: usize = 16;

/// Cache pixel/compression codec identifier (`Cache::Encoder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum CacheEncoder {
    /// YUV420 planar + 4-bit alpha plane, XOR frame deltas, LZ4 block compression.
    Yuv420A4Lz4 = 0,
}

fn y_line_size(width: usize) -> usize {
    width.div_ceil(ALIGN_STORAGE) * ALIGN_STORAGE
}

fn uv_line_size(width: usize) -> usize {
    (width / 2).div_ceil(ALIGN_STORAGE) * ALIGN_STORAGE
}

fn y_size(width: usize, height: usize) -> usize {
    y_line_size(width) * height
}

fn uv_size(width: usize, height: usize) -> usize {
    uv_line_size(width) * (height / 2)
}

fn a_size(width: usize, height: usize) -> usize {
    (width * height) / 2
}

/// Planar YUV420P + 4-bit packed alpha frame buffer (`Lottie::EncodedStorage`).
#[derive(Debug, Clone, Default)]
pub struct EncodedStorage {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl EncodedStorage {
    /// Allocates planar storage for even `width` and `height`.
    pub fn allocate(&mut self, width: u32, height: u32) {
        let w = (width as usize) & !1;
        let h = (height as usize) & !1;
        if y_size(w, h) != y_size(self.width, self.height)
            || uv_size(w, h) != uv_size(self.width, self.height)
            || a_size(w, h) != a_size(self.width, self.height)
        {
            self.width = w;
            self.height = h;
            let total = y_size(w, h) + 2 * uv_size(w, h) + a_size(w, h);
            self.data = vec![0u8; total];
        }
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Total planar byte size.
    #[must_use]
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Raw planar bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Mutable raw planar bytes.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    fn y_offset(&self) -> usize {
        0
    }

    fn u_offset(&self) -> usize {
        y_size(self.width, self.height)
    }

    fn v_offset(&self) -> usize {
        self.u_offset() + uv_size(self.width, self.height)
    }

    fn a_offset(&self) -> usize {
        self.u_offset() + 2 * uv_size(self.width, self.height)
    }
}

/// XORs `from` into `to` byte-for-byte (`Lottie::Xor`).
pub fn xor_storage(to: &mut EncodedStorage, from: &EncodedStorage) {
    let n = to.data.len().min(from.data.len());
    for i in 0..n {
        to.data[i] ^= from.data[i];
    }
}

/// Encodes a premultiplied RGBA `FrameBuffer` into `YUV420P + A4` (`Lottie::Encode`).
pub fn encode_frame(to: &mut EncodedStorage, from: &FrameBuffer) {
    let w = (from.width as usize) & !1;
    let h = (from.height as usize) & !1;
    to.allocate(w as u32, h as u32);
    if w == 0 || h == 0 {
        return;
    }

    let y_stride = y_line_size(w);
    let uv_stride = uv_line_size(w);
    let y_off = to.y_offset();
    let u_off = to.u_offset();
    let v_off = to.v_offset();
    let a_off = to.a_offset();
    let src_w = from.width as usize;

    // First unpremultiply pixels into straight RGB + A for accurate YUV conversion.
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let si = (y * src_w + x) * 4;
            let pr = from.data[si];
            let pg = from.data[si + 1];
            let pb = from.data[si + 2];
            let a = from.data[si + 3];
            let (r, g, b) = if a == 0 {
                (0, 0, 0)
            } else if a == 255 {
                (pr, pg, pb)
            } else {
                let af = u32::from(a);
                (
                    ((u32::from(pr) * 255 + af / 2) / af).min(255) as u8,
                    ((u32::from(pg) * 255 + af / 2) / af).min(255) as u8,
                    ((u32::from(pb) * 255 + af / 2) / af).min(255) as u8,
                )
            };
            let di = (y * w + x) * 3;
            rgb[di] = r;
            rgb[di + 1] = g;
            rgb[di + 2] = b;

            // BT.601 studio-swing Y: ((66*R + 129*G + 25*B + 128) >> 8) + 16
            let y_val =
                ((66 * i32::from(r) + 129 * i32::from(g) + 25 * i32::from(b) + 128) >> 8) + 16;
            to.data[y_off + y * y_stride + x] = y_val.clamp(0, 255) as u8;
        }
    }

    // Subsampled U and V planes (2x2 box average).
    for by in 0..(h / 2) {
        for bx in 0..(w / 2) {
            let mut r_sum = 0i32;
            let mut g_sum = 0i32;
            let mut b_sum = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let di = ((by * 2 + dy) * w + (bx * 2 + dx)) * 3;
                    r_sum += i32::from(rgb[di]);
                    g_sum += i32::from(rgb[di + 1]);
                    b_sum += i32::from(rgb[di + 2]);
                }
            }
            let r = (r_sum + 2) >> 2;
            let g = (g_sum + 2) >> 2;
            let b = (b_sum + 2) >> 2;
            let u_val = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let v_val = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
            to.data[u_off + by * uv_stride + bx] = u_val.clamp(0, 255) as u8;
            to.data[v_off + by * uv_stride + bx] = v_val.clamp(0, 255) as u8;
        }
    }

    // 4-bit alpha plane (2 pixels per byte: high nibble = even x, low nibble = odd x).
    let a_stride = w / 2;
    for y in 0..h {
        for bx in 0..a_stride {
            let x0 = bx * 2;
            let a0 = from.data[(y * src_w + x0) * 4 + 3];
            let a1 = from.data[(y * src_w + x0 + 1) * 4 + 3];
            to.data[a_off + y * a_stride + bx] = (a0 & 0xF0) | (a1 >> 4);
        }
    }
}

/// Decodes a `YUV420P + A4` `EncodedStorage` into a premultiplied RGBA `FrameBuffer` (`Lottie::Decode`).
pub fn decode_frame(to: &mut FrameBuffer, from: &EncodedStorage, size: Size) {
    to.ensure_storage(size);
    let w = (size.width as usize).min(from.width);
    let h = (size.height as usize).min(from.height);
    if w == 0 || h == 0 {
        return;
    }

    let y_stride = y_line_size(from.width);
    let uv_stride = uv_line_size(from.width);
    let y_off = from.y_offset();
    let u_off = from.u_offset();
    let v_off = from.v_offset();
    let a_off = from.a_offset();
    let a_stride = from.width / 2;
    let dst_w = to.width as usize;

    for y in 0..h {
        let u_row = u_off + (y / 2) * uv_stride;
        let v_row = v_off + (y / 2) * uv_stride;
        let y_row = y_off + y * y_stride;
        let a_row = a_off + y * a_stride;
        for x in 0..w {
            let c = i32::from(from.data[y_row + x]) - 16;
            let d = i32::from(from.data[u_row + x / 2]) - 128;
            let e = i32::from(from.data[v_row + x / 2]) - 128;

            let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u32;
            let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u32;
            let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u32;

            let packed_a = from.data[a_row + x / 2];
            let nib = if x % 2 == 0 {
                packed_a >> 4
            } else {
                packed_a & 0x0F
            };
            // Matches DecodeAlpha in lottie_cache_frame_storage.cpp: duplicate nibble into 8-bit alpha.
            let a = (nib << 4) | nib;
            let au = u32::from(a);

            let di = (y * dst_w + x) * 4;
            to.data[di] = ((r * au + 127) / 255) as u8;
            to.data[di + 1] = ((g * au + 127) / 255) as u8;
            to.data[di + 2] = ((b * au + 127) / 255) as u8;
            to.data[di + 3] = a;
        }
    }
}

/// Compresses raw planar storage into `[i32_le_len | payload]` (`CompressFromRaw`).
fn compress_from_raw(from: &EncodedStorage) -> Vec<u8> {
    let raw = from.as_bytes();
    let compressed = compress(raw);
    let payload: &[u8] = if compressed.len() >= raw.len() {
        raw
    } else {
        &compressed
    };
    let len = payload.len() as i32;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Compresses `frame`, also testing XOR delta against `previous`, and swaps `frame` into `previous`
/// (`CompressAndSwapFrame`).
fn compress_and_swap_frame(
    frame: &mut EncodedStorage,
    previous: &mut EncodedStorage,
    allow_xor: bool,
) -> Vec<u8> {
    let direct = compress_from_raw(frame);
    std::mem::swap(frame, previous);
    if !allow_xor {
        return direct;
    }

    xor_storage(frame, previous);
    let mut xored = compress_from_raw(frame);
    if xored.len() >= direct.len() {
        return direct;
    }

    // Negative length signals XOR delta with previous frame.
    let neg_len = -((xored.len() - 4) as i32);
    xored[0..4].copy_from_slice(&neg_len.to_le_bytes());
    xored
}

/// Decompresses a payload into `to` (`UncompressToRaw`).
fn uncompress_to_raw(to: &mut EncodedStorage, from: &[u8]) -> bool {
    let expected = to.size();
    if from.is_empty() || from.len() > expected {
        return false;
    }
    if from.len() == expected {
        to.as_bytes_mut().copy_from_slice(from);
        return true;
    }
    match decompress(from, expected) {
        Ok(decoded) if decoded.len() == expected => {
            to.as_bytes_mut().copy_from_slice(&decoded);
            true
        }
        _ => false,
    }
}

/// Per-reader streaming decode state for `Cache` (`Lottie::CacheReadContext`).
#[derive(Debug, Clone, Default)]
pub struct CacheReadContext {
    /// Scratch buffer for freshly decompressed frame/delta.
    pub uncompressed: EncodedStorage,
    /// Previous decoded planar frame (needed for XOR deltas).
    pub previous: EncodedStorage,
    /// Current byte offset inside `Cache`'s serialized blob.
    pub offset: usize,
    /// Next frame index expected at `offset`.
    pub offset_frame_index: u32,
}

impl CacheReadContext {
    /// Returns `true` if buffers have been prepared for reading.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.offset != 0
    }
}

type PutCallback = Box<dyn FnMut(Vec<u8>) + Send>;

/// Compressed animated frame cache (`Lottie::Cache`).
pub struct Cache {
    data: Vec<u8>,
    compressed_frames: Vec<Vec<u8>>,
    encode_total_size: usize,
    size: Size,
    original: Size,
    read_context: CacheReadContext,
    first_frame: Option<FrameBuffer>,
    frame_rate: u32,
    frames_count: u32,
    frames_ready: u32,
    frames_in_data: u32,
    encoder: CacheEncoder,
    put: Option<PutCallback>,
}

impl Cache {
    /// Opens an existing serialized cache blob (if valid for `request`) or starts an empty cache.
    pub fn new(data: Vec<u8>, request: FrameRequest, put: Option<PutCallback>) -> Self {
        let mut cache = Self {
            data,
            compressed_frames: Vec::new(),
            encode_total_size: 0,
            size: Size::default(),
            original: Size::default(),
            read_context: CacheReadContext::default(),
            first_frame: None,
            frame_rate: 0,
            frames_count: 0,
            frames_ready: 0,
            frames_in_data: 0,
            encoder: CacheEncoder::Yuv420A4Lz4,
            put,
        };
        if !cache.read_header(request) {
            cache.frames_ready = 0;
            cache.frames_in_data = 0;
            cache.data.clear();
        }
        cache
    }

    /// Initializes cache metadata for encoding frames from scratch.
    pub fn init(
        &mut self,
        original: Size,
        frame_rate: u32,
        frames_count: u32,
        request: FrameRequest,
    ) {
        self.size = if request.is_empty() {
            let r = self.size_rounding();
            Size::new(
                original.width.saturating_sub(original.width % r).max(r),
                original.height.saturating_sub(original.height % r).max(r),
            )
        } else {
            request.size(original, self.size_rounding())
        };
        self.original = original;
        self.frame_rate = frame_rate;
        self.frames_count = frames_count;
        self.frames_ready = 0;
        self.frames_in_data = 0;
        self.prepare_own_buffers();
    }

    /// Size rounding required by the YUV420A4 encoder (`8`).
    #[must_use]
    pub const fn size_rounding(&self) -> u32 {
        CACHE_SIZE_ROUNDING
    }

    /// Cached animation frame rate.
    #[must_use]
    pub const fn frame_rate(&self) -> u32 {
        self.frame_rate
    }

    /// Number of frames currently encoded in this cache.
    #[must_use]
    pub const fn frames_ready(&self) -> u32 {
        self.frames_ready
    }

    /// Total frames expected for this animation.
    #[must_use]
    pub const fn frames_count(&self) -> u32 {
        self.frames_count
    }

    /// Original unscaled composition size.
    #[must_use]
    pub const fn original_size(&self) -> Size {
        self.original
    }

    /// Cached frame dimensions.
    #[must_use]
    pub const fn cached_size(&self) -> Size {
        self.size
    }

    /// Takes the pre-decoded first frame if the cache was initialized from an existing blob.
    pub fn take_first_frame(&mut self) -> Option<FrameBuffer> {
        self.first_frame.take()
    }

    /// Allocates buffers on an external `CacheReadContext` for concurrent reading.
    pub fn prepare_buffers(&self, context: &mut CacheReadContext) {
        if self.size.is_empty() {
            return;
        }
        context.offset = HEADER_SIZE;
        context.offset_frame_index = 0;
        context
            .uncompressed
            .allocate(self.size.width, self.size.height);
        context.previous.allocate(self.size.width, self.size.height);
    }

    /// Synchronizes an external `CacheReadContext` with the cache's primary read context.
    pub fn keep_up_context(&self, context: &mut CacheReadContext) {
        if !context.ready() {
            self.prepare_buffers(context);
        }
        context.offset = self.read_context.offset;
        context.offset_frame_index = self.read_context.offset_frame_index;
        context.previous = self.read_context.previous.clone();
        context.uncompressed = self.read_context.uncompressed.clone();
    }

    /// Renders frame `index` from the cache into `to` using the cache's internal context.
    pub fn render_frame(
        &mut self,
        to: &mut FrameBuffer,
        request: FrameRequest,
        index: u32,
    ) -> FrameRenderResult {
        let mut ctx = std::mem::take(&mut self.read_context);
        let result = self.render_frame_with_context(&mut ctx, to, request, index);
        self.read_context = ctx;

        if result == FrameRenderResult::Ok {
            if index + 1 == self.frames_ready && self.data.len() > self.read_context.offset {
                self.data.truncate(self.read_context.offset);
            }
        } else if result == FrameRenderResult::BadCacheSize {
            self.frames_ready = 0;
            self.frames_in_data = 0;
            self.data.clear();
        }
        result
    }

    /// Renders frame `index` using a caller-supplied `CacheReadContext` (read-only on `self`).
    pub fn render_frame_with_context(
        &self,
        context: &mut CacheReadContext,
        to: &mut FrameBuffer,
        request: FrameRequest,
        index: u32,
    ) -> FrameRenderResult {
        if index >= self.frames_ready {
            return FrameRenderResult::NotReady;
        }
        if !request.is_empty() && request.size(self.original, self.size_rounding()) != self.size {
            return FrameRenderResult::BadCacheSize;
        }
        if !context.ready() {
            return FrameRenderResult::Failed;
        }
        if index == 0 {
            context.offset_frame_index = 0;
            context.offset = HEADER_SIZE;
        } else if index != context.offset_frame_index {
            // Sequential replay from frame 0 if caller seeks non-sequentially.
            context.offset_frame_index = 0;
            context.offset = HEADER_SIZE;
            for step in 0..index {
                let (ok, xored) = self.read_compressed_frame(context);
                if !ok || (xored && step == 0) {
                    return FrameRenderResult::Failed;
                }
                if xored {
                    xor_storage(&mut context.previous, &context.uncompressed);
                } else {
                    std::mem::swap(&mut context.uncompressed, &mut context.previous);
                }
            }
        }

        let (ok, xored) = self.read_compressed_frame(context);
        if !ok || (xored && index == 0) {
            return FrameRenderResult::Failed;
        }
        if xored {
            xor_storage(&mut context.previous, &context.uncompressed);
        } else {
            std::mem::swap(&mut context.uncompressed, &mut context.previous);
        }
        decode_frame(to, &context.previous, self.size);
        FrameRenderResult::Ok
    }

    /// Appends a freshly rendered frame at `index` into the cache, flushing when complete or full.
    pub fn append_frame(&mut self, frame: &FrameBuffer, request: FrameRequest, index: u32) {
        let req_size = if request.is_empty() {
            frame.size()
        } else {
            request.size(self.original, self.size_rounding())
        };
        if req_size != self.size {
            self.frames_ready = 0;
            self.frames_in_data = 0;
            self.data.clear();
        }
        if index != self.frames_ready {
            return;
        }
        if index == 0 {
            self.size = req_size;
            self.compressed_frames.clear();
            self.encode_total_size = 0;
            self.prepare_own_buffers();
        }
        if frame.size() != self.size || !self.read_context.ready() {
            return;
        }

        encode_frame(&mut self.read_context.uncompressed, frame);
        let compressed = compress_and_swap_frame(
            &mut self.read_context.uncompressed,
            &mut self.read_context.previous,
            index != 0,
        );

        let now_size = if self.data.is_empty() {
            HEADER_SIZE
        } else {
            self.data.len()
        } + self.encode_total_size;
        let total_size = now_size + compressed.len();
        if now_size <= MAX_CACHE_SIZE && total_size > MAX_CACHE_SIZE {
            self.finalize_encoding();
        }

        self.encode_total_size += compressed.len();
        self.read_context.offset_frame_index += 1;
        self.read_context.offset += compressed.len();
        self.compressed_frames.push(compressed);
        self.frames_ready += 1;
        if self.frames_ready == self.frames_count {
            self.finalize_encoding();
        }
    }

    /// Flushes any accumulated encoded frames into the serialized cache blob and invokes `put`.
    pub fn finalize_encoding(&mut self) {
        if self.compressed_frames.is_empty() {
            return;
        }
        let target_len = if self.data.is_empty() {
            HEADER_SIZE
        } else {
            self.data.len()
        } + self.encode_total_size;
        if self.data.is_empty() {
            self.data.reserve(target_len);
            self.write_header();
        } else {
            self.update_frames_ready_count();
        }
        for block in self.compressed_frames.drain(..) {
            self.data.extend_from_slice(&block);
        }
        self.encode_total_size = 0;
        self.frames_in_data = self.frames_ready;
        if self.data.len() <= MAX_CACHE_SIZE
            && let Some(ref mut put) = self.put
        {
            put(self.data.clone());
        }
    }

    /// Returns the current serialized cache bytes (call `finalize_encoding()` first if needed).
    #[must_use]
    pub fn serialized_bytes(&self) -> &[u8] {
        &self.data
    }

    fn prepare_own_buffers(&mut self) {
        if self.size.is_empty() {
            return;
        }
        self.read_context.offset = HEADER_SIZE;
        self.read_context.offset_frame_index = 0;
        self.read_context
            .uncompressed
            .allocate(self.size.width, self.size.height);
        self.read_context
            .previous
            .allocate(self.size.width, self.size.height);
    }

    fn read_header(&mut self, request: FrameRequest) -> bool {
        if self.data.len() < HEADER_SIZE {
            return false;
        }
        let read_i32 = |off: usize| -> i32 {
            i32::from_be_bytes([
                self.data[off],
                self.data[off + 1],
                self.data[off + 2],
                self.data[off + 3],
            ])
        };

        let encoder = read_i32(0);
        if encoder != CacheEncoder::Yuv420A4Lz4 as i32 {
            return false;
        }
        let w = read_i32(4);
        let h = read_i32(8);
        let orig_w = read_i32(12);
        let orig_h = read_i32(16);
        let frame_rate = read_i32(20);
        let frames_count = read_i32(24);
        let frames_ready = read_i32(28);

        if w <= 0
            || h <= 0
            || orig_w <= 0
            || orig_h <= 0
            || orig_w > MAX_SIZE as i32
            || orig_h > MAX_SIZE as i32
            || frame_rate <= 0
            || (frame_rate > NORMAL_FRAME_RATE as i32 && frame_rate != MAX_FRAME_RATE as i32)
            || frames_count <= 0
            || frames_count > MAX_FRAMES_COUNT as i32
            || frames_ready <= 0
            || frames_ready > frames_count
        {
            return false;
        }

        let size = Size::new(w as u32, h as u32);
        let original = Size::new(orig_w as u32, orig_h as u32);
        if !request.is_empty() && request.size(original, self.size_rounding()) != size {
            return false;
        }
        if !size.width.is_multiple_of(2) || !size.height.is_multiple_of(2) {
            return false;
        }

        self.encoder = CacheEncoder::Yuv420A4Lz4;
        self.size = size;
        self.original = original;
        self.frame_rate = frame_rate as u32;
        self.frames_count = frames_count as u32;
        self.frames_ready = frames_ready as u32;
        self.frames_in_data = frames_ready as u32;
        self.prepare_own_buffers();

        let mut first = FrameBuffer::new(size);
        if self.render_frame(&mut first, request, 0) == FrameRenderResult::Ok {
            self.first_frame = Some(first);
            true
        } else {
            false
        }
    }

    fn write_header(&mut self) {
        self.data.clear();
        let fields: [i32; 8] = [
            self.encoder as i32,
            self.size.width as i32,
            self.size.height as i32,
            self.original.width as i32,
            self.original.height as i32,
            self.frame_rate as i32,
            self.frames_count as i32,
            self.frames_ready as i32,
        ];
        for f in fields {
            self.data.extend_from_slice(&f.to_be_bytes());
        }
    }

    fn update_frames_ready_count(&mut self) {
        if self.data.len() >= HEADER_SIZE {
            let bytes = (self.frames_ready as i32).to_be_bytes();
            self.data[(HEADER_SIZE - 4)..HEADER_SIZE].copy_from_slice(&bytes);
        }
    }

    fn read_compressed_frame(&self, context: &mut CacheReadContext) -> (bool, bool) {
        let part: &[u8] = if context.offset_frame_index >= self.frames_in_data {
            let ready_idx = (context.offset_frame_index - self.frames_in_data) as usize;
            match self.compressed_frames.get(ready_idx) {
                Some(b) => b.as_slice(),
                None => return (false, false),
            }
        } else if self.data.len() > context.offset {
            &self.data[context.offset..]
        } else {
            return (false, false);
        };

        if part.len() < 4 {
            return (false, false);
        }
        let raw_len = i32::from_le_bytes([part[0], part[1], part[2], part[3]]);
        let xored = raw_len < 0;
        let len = match raw_len.checked_abs() {
            Some(v) if v >= 0 => v as usize,
            _ => return (false, false),
        };
        let payload = &part[4..];
        if len > payload.len() {
            return (false, false);
        }
        let ok = uncompress_to_raw(&mut context.uncompressed, &payload[..len]);
        if ok {
            context.offset += 4 + len;
            context.offset_frame_index += 1;
        }
        (ok, xored)
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.finalize_encoding();
    }
}
