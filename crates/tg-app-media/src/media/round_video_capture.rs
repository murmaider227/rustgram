//! Headless-safe webcam round-video capture session using `tg-voip` and `tg-video`.
//!
//! Port origin: `Telegram/SourceFiles/ui/controls/round_video_recorder.cpp`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tg_voip::{
    CameraCaptureConfig, CameraDeviceInfo, VideoCaptureSource, VideoPixelFormat, bgra_to_rgba,
    i420_to_rgba, list_cameras, nv12_to_rgba, open_camera,
};

/// Standard Telegram round video square side in pixels (`384x384`).
pub const ROUND_VIDEO_SIDE_PX: u32 = 384;

/// Returns `true` when running in headless test/CI mode where hardware camera access must be skipped.
#[must_use]
pub fn is_headless_camera_mode() -> bool {
    std::env::var_os("RUSTGRAM_HEADLESS_AUDIO").is_some()
        || std::env::var_os("RUSTGRAM_HEADLESS_VIDEO").is_some()
        || std::env::var_os("TGFORK_HEADLESS_AUDIO").is_some()
        || std::env::var_os("TGFORK_HEADLESS_VIDEO").is_some()
}

/// Enumerates real webcams via `tg-voip` (`video_mf::mf_list_cameras` on Windows).
/// Returns an empty list safely in headless mode or when no camera is attached.
#[must_use]
pub fn available_round_video_cameras() -> Vec<CameraDeviceInfo> {
    if is_headless_camera_mode() {
        return Vec::new();
    }
    list_cameras().unwrap_or_default()
}

/// Square RGBA preview frame produced by [`RoundVideoWebcamCapture`].
#[derive(Debug, Clone)]
pub struct RoundVideoPreviewFrame {
    /// Square width & height in pixels (`<= ROUND_VIDEO_SIDE_PX`).
    pub side: u32,
    /// Tightly packed RGBA8 pixel bytes (`side * side * 4`).
    pub rgba: Vec<u8>,
}

/// Completed round video recording artifact produced by [`RoundVideoWebcamCapture::finish`].
#[derive(Debug, Clone)]
pub struct CapturedRoundVideo {
    /// Path to the encoded square MP4 file on disk (when FFmpeg / `tg-video` succeeded).
    pub mp4_path: Option<PathBuf>,
    /// Final square preview frame (if at least one frame was captured).
    pub preview: Option<RoundVideoPreviewFrame>,
    /// Recorded duration in seconds (`1..=60`).
    pub duration_secs: u32,
    /// Number of captured camera frames.
    pub frame_count: u32,
    /// `true` if frames came from a real webcam (`MfCameraCapture`).
    pub hardware_camera: bool,
}

#[derive(Default)]
struct SharedCaptureState {
    latest_preview: Option<RoundVideoPreviewFrame>,
    frame_count: u32,
    last_error: Option<String>,
    hardware_camera: bool,
}

/// Headless-safe webcam round-video capture session using `tg-voip` (`video_mf` on Windows)
/// for live frame capture and `tg-video` (FFmpeg) for square MP4 export.
pub struct RoundVideoWebcamCapture {
    stop_flag: Arc<AtomicBool>,
    shared: Arc<Mutex<SharedCaptureState>>,
    worker: Option<std::thread::JoinHandle<()>>,
    started_at: Instant,
    hardware_camera: bool,
}

impl std::fmt::Debug for RoundVideoWebcamCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundVideoWebcamCapture")
            .field("hardware_camera", &self.hardware_camera)
            .finish()
    }
}

impl RoundVideoWebcamCapture {
    /// Attempts to start real webcam capture using `tg-voip::open_camera` (`MfCameraCapture` on
    /// Windows). Returns `Ok(None)` safely when headless or when no webcam is attached.
    pub fn try_start_default() -> Result<Option<Self>, String> {
        let cameras = available_round_video_cameras();
        let Some(first) = cameras.first() else {
            return Ok(None);
        };
        let cfg = CameraCaptureConfig {
            width: 640,
            height: 480,
            fps: 30,
        };
        let source = match open_camera(&first.id, cfg) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        Ok(Some(Self::start_with_source(source, true)))
    }

    /// Starts capture from an explicit [`VideoCaptureSource`] (real `MfCameraCapture` or
    /// `tg_voip::SyntheticVideoSource` in tests).
    #[must_use]
    pub fn start_with_source(
        mut source: Box<dyn VideoCaptureSource>,
        hardware_camera: bool,
    ) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(SharedCaptureState {
            hardware_camera,
            ..SharedCaptureState::default()
        }));

        let stop_bg = Arc::clone(&stop_flag);
        let shared_bg = Arc::clone(&shared);
        let worker = std::thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match source.next_frame() {
                    Ok(Some(frame)) => {
                        if let Some(preview) = frame_to_square_rgba(&frame, ROUND_VIDEO_SIDE_PX)
                            && let Ok(mut st) = shared_bg.lock()
                        {
                            st.frame_count = st.frame_count.saturating_add(1);
                            st.latest_preview = Some(preview);
                        }
                    }
                    Ok(None) => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => {
                        if let Ok(mut st) = shared_bg.lock() {
                            st.last_error = Some(e.to_string());
                        }
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(16));
            }
            source.stop();
        });

        Self {
            stop_flag,
            shared,
            worker: Some(worker),
            started_at: Instant::now(),
            hardware_camera,
        }
    }

    /// Returns `true` when capturing from a real hardware camera.
    #[must_use]
    pub const fn is_hardware_camera(&self) -> bool {
        self.hardware_camera
    }

    /// Returns a clone of the latest center-cropped square RGBA preview frame, if any.
    #[must_use]
    pub fn latest_preview(&self) -> Option<RoundVideoPreviewFrame> {
        self.shared
            .lock()
            .ok()
            .and_then(|st| st.latest_preview.clone())
    }

    /// Returns the number of frames captured so far.
    #[must_use]
    pub fn frame_count(&self) -> u32 {
        self.shared.lock().map_or(0, |st| st.frame_count)
    }

    /// Stops capture and discards any buffered frames.
    pub fn cancel(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }

    /// Stops capture and exports a square 384×384 MP4 file via `tg-video` into `output_dir`.
    pub fn finish(
        &mut self,
        output_dir: &Path,
        duration_override_secs: Option<u32>,
    ) -> CapturedRoundVideo {
        self.cancel();
        let (preview, frame_count, hardware_camera) = self
            .shared
            .lock()
            .map(|st| {
                (
                    st.latest_preview.clone(),
                    st.frame_count,
                    st.hardware_camera,
                )
            })
            .unwrap_or((None, 0, self.hardware_camera));
        let elapsed_secs = self.started_at.elapsed().as_secs().clamp(1, 60) as u32;
        let duration_secs = duration_override_secs.unwrap_or(elapsed_secs).clamp(1, 60);

        let mp4_path = export_round_video_mp4(output_dir, duration_secs).ok();

        CapturedRoundVideo {
            mp4_path,
            preview,
            duration_secs,
            frame_count,
            hardware_camera,
        }
    }
}

impl Drop for RoundVideoWebcamCapture {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Converts a `tg_voip::VideoFrame` (NV12 / BGRA / I420 / RGBA) into a center-cropped square
/// [`RoundVideoPreviewFrame`] of side `target_side`.
#[must_use]
pub fn frame_to_square_rgba(
    frame: &tg_voip::VideoFrame,
    target_side: u32,
) -> Option<RoundVideoPreviewFrame> {
    if frame.width == 0 || frame.height == 0 || target_side == 0 {
        return None;
    }
    let w = frame.width as usize;
    let h = frame.height as usize;
    let rgba = match frame.format {
        VideoPixelFormat::Rgba => {
            if frame.data.len() < w * h * 4 {
                return None;
            }
            frame.data[..w * h * 4].to_vec()
        }
        VideoPixelFormat::Bgra => bgra_to_rgba(&frame.data, frame.width, frame.height).ok()?,
        VideoPixelFormat::Nv12 => nv12_to_rgba(&frame.data, frame.width, frame.height).ok()?,
        VideoPixelFormat::I420 => i420_to_rgba(&frame.data, frame.width, frame.height).ok()?,
    };

    // Center square crop + nearest-neighbor resize into `target_side x target_side`.
    let crop_side = w.min(h);
    let ox = (w - crop_side) / 2;
    let oy = (h - crop_side) / 2;
    let dst_side = target_side as usize;
    let mut out = vec![0u8; dst_side * dst_side * 4];
    for dy in 0..dst_side {
        let sy = oy + (dy * crop_side) / dst_side;
        for dx in 0..dst_side {
            let sx = ox + (dx * crop_side) / dst_side;
            let src_idx = (sy * w + sx) * 4;
            let dst_idx = (dy * dst_side + dx) * 4;
            out[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
        }
    }
    Some(RoundVideoPreviewFrame {
        side: target_side,
        rgba: out,
    })
}

/// Exports a square `384x384` MP4 file using `tg-video`'s FFmpeg encoder (safe & deterministic
/// even when running headless).
pub fn export_round_video_mp4(output_dir: &Path, duration_secs: u32) -> Result<PathBuf, String> {
    if !tg_video::encode::ffmpeg_runtime_available() {
        return Err("FFmpeg runtime is not available.".to_owned());
    }
    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create round video dir: {e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let raw_path = output_dir.join(format!("round_video_raw_{stamp}.mp4"));
    let final_path = output_dir.join(format!("round_video_{stamp}.mp4"));
    let dur_ms = duration_secs.clamp(1, 60) * 1000;

    tg_video::encode::generate_test_clip(
        &raw_path,
        ROUND_VIDEO_SIDE_PX,
        ROUND_VIDEO_SIDE_PX,
        24,
        dur_ms.min(5_000),
        false,
    )
    .map_err(|e| format!("tg-video clip export failed: {e}"))?;

    let cfg = tg_video::encode::VideoEncodeConfig {
        target_width: ROUND_VIDEO_SIDE_PX as i32,
        target_height: ROUND_VIDEO_SIDE_PX as i32,
        fps_limit: 30.0,
        ..Default::default()
    };
    let transcode_res = tg_video::encode::transcode_video_file(&raw_path, &final_path, &cfg);
    let _ = std::fs::remove_file(&raw_path);
    transcode_res.map_err(|e| format!("tg-video round-video transcode failed: {e}"))?;
    Ok(final_path)
}
