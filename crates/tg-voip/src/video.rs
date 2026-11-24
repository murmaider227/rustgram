//! Video capture, pixel conversion, rotation/aspect handling, and H.264 codec traits/factories.
//!
//! Reference: `TelegramMessenger/tgcalls` (`VideoCaptureInterface`, `VideoSinkInterface`,
//! `InstanceV2Impl` video pipeline) and `crates/tg-calls/docs/media-engine.md` §7 Phase 4.
//!
//! # Platform Implementation (Windows 10/11 x64)
//! - **Camera capture**: Windows Media Foundation (`MFEnumDeviceSources` + `IMFSourceReader`)
//!   in [`crate::video_mf`].
//! - **Screen capture**: DXGI Desktop Duplication (`IDXGIOutput1::DuplicateOutput` + D3D11
//!   staging readback) in [`crate::video_mf`].
//! - **H.264 encode/decode**: Windows Media Foundation Transforms (`IMFTransform` via `MFTEnumEx`),
//!   preferring hardware MFTs and transparently falling back to Windows' built-in software
//!   Microsoft H.264 Encoder/Decoder MFTs — zero CMake, zero OpenH264 binary downloads.
//! - **VP8**: Omitted because no pure-Rust/MSVC-cc VP8 encoder exists without CMake/NASM.
//! - **UI output**: Decoded remote frames are exposed as RGBA (`RgbaVideoFrame` /
//!   `VideoFrame::to_rgba_rotated`) with remote rotation (`0`/`90`/`180`/`270`) and display
//!   aspect ratio ready for `tg-app`.

use crate::error::{VoipError, VoipResult};

/// Pixel layout of a raw captured or decoded video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPixelFormat {
    /// Planar YUV 4:2:0 (I420).
    I420,
    /// Semi-planar YUV 4:2:0 (NV12, native Windows Media Foundation format).
    Nv12,
    /// Packed 32-bit BGRA (native Windows DXGI / Graphics Capture format).
    Bgra,
    /// Packed 32-bit RGBA (UI presentation format for `tg-app` / `egui`).
    Rgba,
}

/// Raw video frame passed between capture, codec, and UI sink.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Clockwise rotation in degrees (`0`, `90`, `180`, `270`).
    pub rotation: u16,
    /// Capture or presentation timestamp in microseconds.
    pub timestamp_us: u64,
    /// Pixel buffer format.
    pub format: VideoPixelFormat,
    /// Contiguous raw pixel bytes.
    pub data: Vec<u8>,
}

/// Decoded RGBA video frame with rotation already applied, ready for `tg-app` texture upload.
#[derive(Debug, Clone, PartialEq)]
pub struct RgbaVideoFrame {
    /// Display width in pixels (post-rotation).
    pub width: u32,
    /// Display height in pixels (post-rotation).
    pub height: u32,
    /// Source rotation metadata in degrees (`0`, `90`, `180`, `270`) that was applied.
    pub source_rotation: u16,
    /// Display aspect ratio (`width as f32 / height as f32`).
    pub aspect_ratio: f32,
    /// Presentation timestamp in microseconds.
    pub timestamp_us: u64,
    /// Tightly packed RGBA8 pixel buffer (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
}

impl VideoFrame {
    /// Display dimensions after applying `rotation` (`90` and `270` swap width/height).
    #[must_use]
    pub const fn display_dimensions(&self) -> (u32, u32) {
        match normalize_rotation(self.rotation) {
            90 | 270 => (self.height, self.width),
            _ => (self.width, self.height),
        }
    }

    /// Effective display aspect ratio (`display_width / display_height`), or `1.0` if degenerate.
    #[must_use]
    pub fn aspect_ratio(&self) -> f32 {
        let (w, h) = self.display_dimensions();
        if h == 0 { 1.0 } else { w as f32 / h as f32 }
    }

    /// Convert this frame's pixel buffer into tightly packed RGBA8 bytes (without rotating).
    pub fn to_rgba_bytes(&self) -> VoipResult<Vec<u8>> {
        let w = self.width as usize;
        let h = self.height as usize;
        match self.format {
            VideoPixelFormat::Rgba => {
                let expected = w.saturating_mul(h).saturating_mul(4);
                if self.data.len() < expected {
                    return Err(VoipError::Video(format!(
                        "RGBA frame buffer too small: {} < {expected}",
                        self.data.len()
                    )));
                }
                Ok(self.data[..expected].to_vec())
            }
            VideoPixelFormat::Bgra => bgra_to_rgba(&self.data, self.width, self.height),
            VideoPixelFormat::Nv12 => nv12_to_rgba(&self.data, self.width, self.height),
            VideoPixelFormat::I420 => i420_to_rgba(&self.data, self.width, self.height),
        }
    }

    /// Convert to an [`RgbaVideoFrame`] with `self.rotation` applied for direct `tg-app` rendering.
    pub fn to_rgba_rotated(&self) -> VoipResult<RgbaVideoFrame> {
        let rgba_unrotated = self.to_rgba_bytes()?;
        let rot = normalize_rotation(self.rotation);
        let (out_w, out_h, rgba) = rotate_rgba(&rgba_unrotated, self.width, self.height, rot);
        let aspect_ratio = if out_h == 0 {
            1.0
        } else {
            out_w as f32 / out_h as f32
        };
        Ok(RgbaVideoFrame {
            width: out_w,
            height: out_h,
            source_rotation: rot,
            aspect_ratio,
            timestamp_us: self.timestamp_us,
            rgba,
        })
    }

    /// Convert this frame into tightly packed NV12 bytes (native Windows MF H.264 encoder input).
    pub fn to_nv12_bytes(&self) -> VoipResult<Vec<u8>> {
        match self.format {
            VideoPixelFormat::Nv12 => {
                let expected = nv12_buffer_len(self.width, self.height);
                if self.data.len() < expected {
                    return Err(VoipError::Video(format!(
                        "NV12 frame buffer too small: {} < {expected}",
                        self.data.len()
                    )));
                }
                Ok(self.data[..expected].to_vec())
            }
            VideoPixelFormat::I420 => i420_to_nv12(&self.data, self.width, self.height),
            VideoPixelFormat::Rgba => rgba_to_nv12(&self.data, self.width, self.height),
            VideoPixelFormat::Bgra => bgra_to_nv12(&self.data, self.width, self.height),
        }
    }
}

/// Normalize any degree value into one of `0`, `90`, `180`, `270`.
#[must_use]
pub const fn normalize_rotation(deg: u16) -> u16 {
    match deg % 360 {
        45..=134 => 90,
        135..=224 => 180,
        225..=314 => 270,
        _ => 0,
    }
}

/// Required byte size of a tightly packed NV12 or I420 4:2:0 frame.
#[must_use]
pub const fn nv12_buffer_len(width: u32, height: u32) -> usize {
    let w = width as usize;
    let h = height as usize;
    w * h + (w * h) / 2
}

/// Convert packed BGRA8 to packed RGBA8.
pub fn bgra_to_rgba(bgra: &[u8], width: u32, height: u32) -> VoipResult<Vec<u8>> {
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if bgra.len() < expected {
        return Err(VoipError::Video(format!(
            "BGRA buffer too small: {} < {expected}",
            bgra.len()
        )));
    }
    let mut out = vec![0u8; expected];
    let (dst_px, _) = out.as_chunks_mut::<4>();
    let (src_px, _) = bgra[..expected].as_chunks::<4>();
    for (dst, src) in dst_px.iter_mut().zip(src_px.iter()) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = src[3];
    }
    Ok(out)
}

/// Convert semi-planar NV12 (BT.601 limited range) to packed RGBA8.
pub fn nv12_to_rgba(nv12: &[u8], width: u32, height: u32) -> VoipResult<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    let y_len = w.saturating_mul(h);
    let uv_len = w.saturating_mul(h / 2);
    if nv12.len() < y_len + uv_len {
        return Err(VoipError::Video(format!(
            "NV12 buffer too small: {} < {}",
            nv12.len(),
            y_len + uv_len
        )));
    }
    let y_plane = &nv12[..y_len];
    let uv_plane = &nv12[y_len..y_len + uv_len];
    let mut rgba = vec![0u8; y_len * 4];

    for row in 0..h {
        let uv_row = (row / 2) * w;
        let y_row = row * w;
        for col in 0..w {
            let y = i32::from(y_plane[y_row + col]);
            let uv_idx = uv_row + (col & !1);
            let u = i32::from(uv_plane.get(uv_idx).copied().unwrap_or(128)) - 128;
            let v = i32::from(uv_plane.get(uv_idx + 1).copied().unwrap_or(128)) - 128;

            let c = (y - 16).max(0);
            let r = ((298 * c + 409 * v + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * u + 128) >> 8).clamp(0, 255) as u8;

            let dst = (y_row + col) * 4;
            rgba[dst] = r;
            rgba[dst + 1] = g;
            rgba[dst + 2] = b;
            rgba[dst + 3] = 255;
        }
    }
    Ok(rgba)
}

/// Convert planar I420 (YUV 4:2:0) to packed RGBA8.
pub fn i420_to_rgba(i420: &[u8], width: u32, height: u32) -> VoipResult<Vec<u8>> {
    let nv12 = i420_to_nv12(i420, width, height)?;
    nv12_to_rgba(&nv12, width, height)
}

/// Convert planar I420 to semi-planar NV12.
pub fn i420_to_nv12(i420: &[u8], width: u32, height: u32) -> VoipResult<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    let y_len = w * h;
    let cw = w / 2;
    let ch = h / 2;
    let c_len = cw * ch;
    if i420.len() < y_len + c_len * 2 {
        return Err(VoipError::Video(format!(
            "I420 buffer too small: {} < {}",
            i420.len(),
            y_len + c_len * 2
        )));
    }
    let mut nv12 = vec![0u8; y_len + c_len * 2];
    nv12[..y_len].copy_from_slice(&i420[..y_len]);
    let u_plane = &i420[y_len..y_len + c_len];
    let v_plane = &i420[y_len + c_len..y_len + c_len * 2];
    let uv_dst = &mut nv12[y_len..];
    for i in 0..c_len {
        uv_dst[i * 2] = u_plane[i];
        uv_dst[i * 2 + 1] = v_plane[i];
    }
    Ok(nv12)
}

/// Convert packed RGBA8 to semi-planar NV12 (BT.601 limited range).
pub fn rgba_to_nv12(rgba: &[u8], width: u32, height: u32) -> VoipResult<Vec<u8>> {
    rgb_like_to_nv12(rgba, width, height, false)
}

/// Convert packed BGRA8 to semi-planar NV12 (BT.601 limited range).
pub fn bgra_to_nv12(bgra: &[u8], width: u32, height: u32) -> VoipResult<Vec<u8>> {
    rgb_like_to_nv12(bgra, width, height, true)
}

fn rgb_like_to_nv12(src: &[u8], width: u32, height: u32, is_bgra: bool) -> VoipResult<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    let expected = w.saturating_mul(h).saturating_mul(4);
    if src.len() < expected {
        return Err(VoipError::Video(format!(
            "RGBA/BGRA buffer too small: {} < {expected}",
            src.len()
        )));
    }
    let y_len = w * h;
    let uv_len = w * (h / 2);
    let mut nv12 = vec![128u8; y_len + uv_len];

    for row in 0..h {
        for col in 0..w {
            let p = (row * w + col) * 4;
            let (r, g, b) = if is_bgra {
                (
                    i32::from(src[p + 2]),
                    i32::from(src[p + 1]),
                    i32::from(src[p]),
                )
            } else {
                (
                    i32::from(src[p]),
                    i32::from(src[p + 1]),
                    i32::from(src[p + 2]),
                )
            };
            let y = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(16, 235) as u8;
            nv12[row * w + col] = y;
        }
    }

    for row in (0..(h & !1)).step_by(2) {
        let uv_row = y_len + (row / 2) * w;
        for col in (0..(w & !1)).step_by(2) {
            let mut r_sum = 0i32;
            let mut g_sum = 0i32;
            let mut b_sum = 0i32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = ((row + dy) * w + (col + dx)) * 4;
                    if is_bgra {
                        r_sum += i32::from(src[p + 2]);
                        g_sum += i32::from(src[p + 1]);
                        b_sum += i32::from(src[p]);
                    } else {
                        r_sum += i32::from(src[p]);
                        g_sum += i32::from(src[p + 1]);
                        b_sum += i32::from(src[p + 2]);
                    }
                }
            }
            let r = r_sum >> 2;
            let g = g_sum >> 2;
            let b = b_sum >> 2;
            let u = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(16, 240) as u8;
            let v = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(16, 240) as u8;
            nv12[uv_row + col] = u;
            nv12[uv_row + col + 1] = v;
        }
    }
    Ok(nv12)
}

/// Resize an NV12 buffer using nearest-neighbor / box sampling for dynamic resolution adaptation.
pub fn resize_nv12(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> VoipResult<Vec<u8>> {
    if src_w == dst_w && src_h == dst_h {
        let len = nv12_buffer_len(src_w, src_h);
        if src.len() < len {
            return Err(VoipError::Video("NV12 source buffer truncated".into()));
        }
        return Ok(src[..len].to_vec());
    }
    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Err(VoipError::Video(
            "zero video dimension in resize_nv12".into(),
        ));
    }
    let src_y_len = sw * sh;
    if src.len() < src_y_len + sw * (sh / 2) {
        return Err(VoipError::Video("NV12 source buffer truncated".into()));
    }

    let dst_y_len = dw * dh;
    let mut dst = vec![128u8; dst_y_len + dw * (dh / 2)];
    for y in 0..dh {
        let sy = (y * sh) / dh;
        for x in 0..dw {
            let sx = (x * sw) / dw;
            dst[y * dw + x] = src[sy * sw + sx];
        }
    }

    let src_ch = sh / 2;
    let src_cw = sw / 2;
    let dst_ch = dh / 2;
    let dst_cw = dw / 2;
    for cy in 0..dst_ch {
        let scy = (cy * src_ch) / dst_ch.max(1);
        for cx in 0..dst_cw {
            let scx = (cx * src_cw) / dst_cw.max(1);
            let s_idx = src_y_len + scy * sw + scx * 2;
            let d_idx = dst_y_len + cy * dw + cx * 2;
            dst[d_idx] = src[s_idx];
            dst[d_idx + 1] = src[s_idx + 1];
        }
    }
    Ok(dst)
}

/// Rotate packed RGBA8 pixels clockwise by `0`, `90`, `180`, or `270` degrees.
#[must_use]
pub fn rotate_rgba(rgba: &[u8], width: u32, height: u32, rotation: u16) -> (u32, u32, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let rot = normalize_rotation(rotation);
    if rot == 0 || w == 0 || h == 0 {
        return (width, height, rgba.to_vec());
    }

    let (out_w, out_h) = match rot {
        90 | 270 => (h, w),
        _ => (w, h),
    };
    let mut out = vec![0u8; out_w * out_h * 4];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match rot {
                90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                270 => (y, w - 1 - x),
                _ => (x, y),
            };
            let s = (y * w + x) * 4;
            let d = (dy * out_w + dx) * 4;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (out_w as u32, out_h as u32, out)
}

/// Video codec identifier advertised in `tgcalls` `NegotiateChannels`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecKind {
    /// VP8 (RFC 7741).
    Vp8,
    /// VP9.
    Vp9,
    /// H.264 / AVC (RFC 6184).
    H264,
    /// AV1.
    Av1,
}

/// Metadata for an enumerated camera capture device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraDeviceInfo {
    /// OS-stable camera identifier (Windows MF symbolic link).
    pub id: String,
    /// Human-readable camera name.
    pub name: String,
}

/// Metadata for an enumerated desktop display available for screen sharing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayDeviceInfo {
    /// OS-stable display identifier (e.g. `\\.\DISPLAY1`).
    pub id: String,
    /// Human-readable display label.
    pub name: String,
    /// Display width in pixels.
    pub width: u32,
    /// Display height in pixels.
    pub height: u32,
    /// Desktop left coordinate.
    pub left: i32,
    /// Desktop top coordinate.
    pub top: i32,
}

/// Configuration for opening a camera capture source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraCaptureConfig {
    /// Target width in pixels (default 1280).
    pub width: u32,
    /// Target height in pixels (default 720).
    pub height: u32,
    /// Target frame rate in frames per second (default 30).
    pub fps: u32,
}

impl Default for CameraCaptureConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
        }
    }
}

/// Configuration for opening a desktop screen capture source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCaptureConfig {
    /// Target display ID (`""` or `"default"` selects the primary display).
    pub display_id: String,
    /// Target capture frame rate (default 15 fps).
    pub fps: u32,
}

impl Default for ScreenCaptureConfig {
    fn default() -> Self {
        Self {
            display_id: String::new(),
            fps: 15,
        }
    }
}

/// Configuration for creating an H.264 video encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoEncoderConfig {
    /// Coded width in pixels (must be even).
    pub width: u32,
    /// Coded height in pixels (must be even).
    pub height: u32,
    /// Target frame rate in fps.
    pub fps: u32,
    /// Initial target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Keyframe interval in frames (default `fps * 2`).
    pub gop_size: u32,
}

impl Default for VideoEncoderConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            fps: 30,
            bitrate_bps: 750_000,
            gop_size: 60,
        }
    }
}

/// Trait implemented by platform camera/screen capture sources.
pub trait VideoCaptureSource: Send {
    /// Pull the latest captured frame if one is ready.
    fn next_frame(&mut self) -> VoipResult<Option<VideoFrame>>;
    /// Stop capturing and release the camera/display handle.
    fn stop(&mut self);
}

/// Trait implemented by video encoders (Windows Media Foundation H.264 MFT).
pub trait VideoEncoder: Send {
    /// Codec implemented by this encoder.
    fn codec(&self) -> VideoCodecKind;
    /// Encode a raw [`VideoFrame`] into an Annex-B bitstream unit (may be empty while priming).
    fn encode(&mut self, frame: &VideoFrame, force_keyframe: bool) -> VoipResult<Vec<u8>>;
    /// Adjust target video bitrate in bits per second.
    fn set_bitrate_bps(&mut self, bitrate_bps: u32);
    /// Reconfigure output resolution, frame rate, and bitrate (default no-op for fixed encoders).
    fn reconfigure(
        &mut self,
        _width: u32,
        _height: u32,
        _fps: u32,
        bitrate_bps: u32,
    ) -> VoipResult<()> {
        self.set_bitrate_bps(bitrate_bps);
        Ok(())
    }
    /// Flush any buffered compressed frames at stream end.
    fn flush(&mut self) -> VoipResult<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
}

/// Trait implemented by video decoders (Windows Media Foundation H.264 MFT).
pub trait VideoDecoder: Send {
    /// Codec implemented by this decoder.
    fn codec(&self) -> VideoCodecKind;
    /// Decode an Annex-B bitstream unit into a raw [`VideoFrame`].
    fn decode(&mut self, packet: &[u8]) -> VoipResult<Option<VideoFrame>>;
    /// Flush any buffered decoded frames.
    fn flush(&mut self) -> VoipResult<Vec<VideoFrame>> {
        Ok(Vec::new())
    }
}

/// Synthetic moving color-bar video capture source for deterministic tests and headless runs.
pub struct SyntheticVideoSource {
    width: u32,
    height: u32,
    fps: u32,
    frame_index: u64,
    stopped: bool,
}

impl SyntheticVideoSource {
    /// Create a synthetic NV12 test pattern generator.
    #[must_use]
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        Self {
            width: (width & !1).max(16),
            height: (height & !1).max(16),
            fps: fps.max(1),
            frame_index: 0,
            stopped: false,
        }
    }

    /// Generate one deterministic synthetic NV12 frame at `index`.
    #[must_use]
    pub fn generate_nv12_frame(width: u32, height: u32, index: u64, fps: u32) -> VideoFrame {
        let w = (width & !1).max(16) as usize;
        let h = (height & !1).max(16) as usize;
        let y_len = w * h;
        let uv_len = w * (h / 2);
        let mut data = vec![128u8; y_len + uv_len];

        let shift = (index as usize * 8) % w.max(1);
        for row in 0..h {
            for col in 0..w {
                let x = (col + shift) % w;
                let bar = (x * 8) / w;
                let luma = match bar {
                    0 => 220u8,
                    1 => 190,
                    2 => 160,
                    3 => 130,
                    4 => 100,
                    5 => 75,
                    6 => 50,
                    _ => 30,
                };
                data[row * w + col] = luma;
            }
        }
        for cy in 0..(h / 2) {
            for cx in 0..(w / 2) {
                let x = ((cx * 2) + shift) % w;
                let bar = (x * 8) / w;
                let (u, v) = match bar {
                    0 => (128u8, 128u8),
                    1 => (44, 142),
                    2 => (156, 44),
                    3 => (72, 58),
                    4 => (184, 198),
                    5 => (100, 212),
                    6 => (212, 114),
                    _ => (128, 128),
                };
                let idx = y_len + cy * w + cx * 2;
                data[idx] = u;
                data[idx + 1] = v;
            }
        }

        VideoFrame {
            width: w as u32,
            height: h as u32,
            rotation: 0,
            timestamp_us: (index * 1_000_000) / u64::from(fps.max(1)),
            format: VideoPixelFormat::Nv12,
            data,
        }
    }
}

impl VideoCaptureSource for SyntheticVideoSource {
    fn next_frame(&mut self) -> VoipResult<Option<VideoFrame>> {
        if self.stopped {
            return Ok(None);
        }
        let frame = Self::generate_nv12_frame(self.width, self.height, self.frame_index, self.fps);
        self.frame_index += 1;
        Ok(Some(frame))
    }

    fn stop(&mut self) {
        self.stopped = true;
    }
}

/// Enumerate available video capture cameras (Windows Media Foundation `MFEnumDeviceSources`).
pub fn list_cameras() -> VoipResult<Vec<CameraDeviceInfo>> {
    #[cfg(windows)]
    {
        crate::video_mf::mf_list_cameras()
    }
    #[cfg(not(windows))]
    {
        Err(VoipError::Video(
            "camera enumeration is only implemented on Windows (Media Foundation)".into(),
        ))
    }
}

/// Enumerate available desktop displays for screen sharing (Windows DXGI outputs).
pub fn list_displays() -> VoipResult<Vec<DisplayDeviceInfo>> {
    #[cfg(windows)]
    {
        crate::video_mf::dxgi_list_displays()
    }
    #[cfg(not(windows))]
    {
        Err(VoipError::Video(
            "display enumeration is only implemented on Windows (DXGI)".into(),
        ))
    }
}

/// Open a camera capture source by `device_id` (`""` or `"default"` selects the first camera).
pub fn open_camera(
    device_id: &str,
    config: CameraCaptureConfig,
) -> VoipResult<Box<dyn VideoCaptureSource>> {
    #[cfg(windows)]
    {
        let cam = crate::video_mf::MfCameraCapture::open(device_id, config)?;
        Ok(Box::new(cam))
    }
    #[cfg(not(windows))]
    {
        let _ = (device_id, config);
        Err(VoipError::Video(
            "camera capture is only implemented on Windows (Media Foundation)".into(),
        ))
    }
}

/// Open a desktop screen capture source via DXGI Desktop Duplication.
pub fn open_screen_capture(config: ScreenCaptureConfig) -> VoipResult<Box<dyn VideoCaptureSource>> {
    #[cfg(windows)]
    {
        let cap = crate::video_mf::DxgiScreenCapture::open(&config.display_id)?;
        Ok(Box::new(cap))
    }
    #[cfg(not(windows))]
    {
        let _ = config;
        Err(VoipError::Video(
            "screen capture is only implemented on Windows (DXGI Desktop Duplication)".into(),
        ))
    }
}

/// Create an H.264 encoder (Windows Media Foundation hardware MFT with software MFT fallback).
pub fn create_h264_encoder(config: VideoEncoderConfig) -> VoipResult<Box<dyn VideoEncoder>> {
    #[cfg(windows)]
    {
        let enc = crate::video_mf::MfH264Encoder::new(config)?;
        Ok(Box::new(enc))
    }
    #[cfg(not(windows))]
    {
        let _ = config;
        Err(VoipError::Video(
            "H.264 encoding via Media Foundation is only available on Windows".into(),
        ))
    }
}

/// Create an H.264 decoder (Windows Media Foundation hardware MFT with software MFT fallback).
pub fn create_h264_decoder() -> VoipResult<Box<dyn VideoDecoder>> {
    #[cfg(windows)]
    {
        let dec = crate::video_mf::MfH264Decoder::new()?;
        Ok(Box::new(dec))
    }
    #[cfg(not(windows))]
    {
        Err(VoipError::Video(
            "H.264 decoding via Media Foundation is only available on Windows".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_conversion_and_rotation_roundtrip() {
        let frame = SyntheticVideoSource::generate_nv12_frame(64, 48, 0, 30);
        let rgba = frame.to_rgba_rotated().unwrap();
        assert_eq!(rgba.width, 64);
        assert_eq!(rgba.height, 48);
        assert_eq!(rgba.rgba.len(), 64 * 48 * 4);
        assert!((rgba.aspect_ratio - (64.0 / 48.0)).abs() < 1e-4);

        // Rotate 90 degrees -> swaps dimensions and aspect ratio.
        let mut rotated_frame = frame.clone();
        rotated_frame.rotation = 90;
        let rot90 = rotated_frame.to_rgba_rotated().unwrap();
        assert_eq!(rot90.width, 48);
        assert_eq!(rot90.height, 64);
        assert_eq!(rot90.source_rotation, 90);
        assert!((rot90.aspect_ratio - (48.0 / 64.0)).abs() < 1e-4);

        // RGBA -> NV12 -> resize preserves expected buffer lengths.
        let nv12 = rgba_to_nv12(&rgba.rgba, rgba.width, rgba.height).unwrap();
        assert_eq!(nv12.len(), nv12_buffer_len(64, 48));
        let resized = resize_nv12(&nv12, 64, 48, 32, 24).unwrap();
        assert_eq!(resized.len(), nv12_buffer_len(32, 24));
    }
}
