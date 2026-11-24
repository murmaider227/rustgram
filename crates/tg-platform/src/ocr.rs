//! Native Windows OCR text recognition via `Windows.Media.Ocr`.
//!
//! Port of `Telegram/SourceFiles/platform/platform_text_recognition.h` and
//! `Telegram/SourceFiles/platform/win/text_recognition_win.h`, implemented on top of the
//! WinRT `Windows.Media.Ocr.OcrEngine` API (Windows 10+).
//!
//! All WinRT async operations execute inside a dedicated MTA worker thread with a bounded
//! timeout so calling [`recognize_rgba8`] from an STA UI thread never deadlocks or freezes the UI.

use crate::error::{PlatformError, Result};

/// Bounding rectangle in image pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OcrRect {
    /// Left X coordinate in pixels.
    pub x: f32,
    /// Top Y coordinate in pixels.
    pub y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,
}

impl OcrRect {
    /// Constructs a new [`OcrRect`].
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Right edge X coordinate (`x + width`).
    #[must_use]
    pub fn right(self) -> f32 {
        self.x + self.width
    }

    /// Bottom edge Y coordinate (`y + height`).
    #[must_use]
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    /// Computes the union bounding box of `self` and `other`.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        let min_x = self.x.min(other.x);
        let min_y = self.y.min(other.y);
        let max_x = self.right().max(other.right());
        let max_y = self.bottom().max(other.bottom());
        Self::new(
            min_x,
            min_y,
            (max_x - min_x).max(0.0),
            (max_y - min_y).max(0.0),
        )
    }
}

/// Single recognized word with its pixel bounding box.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    /// Recognized word text.
    pub text: String,
    /// Word bounding box in source image pixel coordinates.
    pub rect: OcrRect,
}

/// Single recognized text line containing words and a combined bounding box.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    /// Full line text reported by `OcrLine::Text`.
    pub text: String,
    /// Combined bounding box enclosing all words on this line.
    pub rect: OcrRect,
    /// Individual word boxes within this line.
    pub words: Vec<OcrWord>,
}

/// Complete OCR recognition result for an image (`Platform::TextRecognition::Result`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OcrResult {
    /// Recognized text lines with bounding boxes.
    pub lines: Vec<OcrLine>,
    /// Combined recognized text joined across lines.
    pub text: String,
    /// Optional detected clockwise text rotation angle in degrees.
    pub text_angle: Option<f64>,
    /// Width of the coordinate space in pixels.
    pub coordinate_width: u32,
    /// Height of the coordinate space in pixels.
    pub coordinate_height: u32,
    /// BCP-47 language tag used by the recognizer (e.g. `"en-US"`).
    pub language_tag: String,
}

impl OcrResult {
    /// Returns `true` when at least one non-empty text line was recognized.
    #[must_use]
    pub fn has_text(&self) -> bool {
        !self.lines.is_empty() && !self.text.trim().is_empty()
    }
}

/// Returns `true` if Windows OCR is supported and at least one recognizer language pack is installed.
#[must_use]
pub fn is_available() -> bool {
    #[cfg(windows)]
    {
        if !crate::system_info::windows_version().is_windows_10_or_greater() {
            return false;
        }
        !available_languages().is_empty()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Lists BCP-47 language tags installed for `Windows.Media.Ocr` (e.g. `["en-US"]`).
///
/// Returns an empty list on older Windows, non-Windows hosts, or if no OCR packs are installed.
#[must_use]
pub fn available_languages() -> Vec<String> {
    #[cfg(windows)]
    {
        if !crate::system_info::windows_version().is_windows_10_or_greater() {
            return Vec::new();
        }
        win_ocr::available_languages()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Recognizes text from an unmultiplied `RGBA8` pixel buffer (`width * height * 4` bytes).
///
/// If `language_tag` is `Some(tag)` (e.g. `"en-US"`), requests that specific OCR language;
/// otherwise uses the user's profile languages and falls back to the first available OCR language.
pub fn recognize_rgba8(
    rgba: &[u8],
    width: u32,
    height: u32,
    language_tag: Option<&str>,
) -> Result<OcrResult> {
    if width == 0 || height == 0 {
        return Err(PlatformError::InvalidArgument(
            "OCR image dimensions must be non-zero".into(),
        ));
    }
    let expected_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| PlatformError::InvalidArgument("OCR image dimensions overflow".into()))?;
    if rgba.len() != expected_len {
        return Err(PlatformError::InvalidArgument(format!(
            "OCR buffer length {} does not match {width}x{height}x4 ({expected_len})",
            rgba.len()
        )));
    }

    #[cfg(windows)]
    {
        if !crate::system_info::windows_version().is_windows_10_or_greater() {
            return Err(PlatformError::Unsupported);
        }
        win_ocr::recognize_rgba8(rgba, width, height, language_tag)
    }
    #[cfg(not(windows))]
    {
        let _ = language_tag;
        Err(PlatformError::Unsupported)
    }
}

/// Converts unmultiplied RGBA8 pixels into premultiplied BGRA8 bytes suitable for `SoftwareBitmap`.
#[must_use]
pub fn rgba8_to_premultiplied_bgra8(rgba: &[u8]) -> Vec<u8> {
    let mut bgra = Vec::with_capacity(rgba.len());
    let (chunks, _) = rgba.as_chunks::<4>();
    for &[r, g, b, a] in chunks {
        if a == 255 {
            bgra.extend_from_slice(&[b, g, r, 255]);
        } else if a == 0 {
            bgra.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let af = u16::from(a);
            let pb = ((u16::from(b) * af + 127) / 255) as u8;
            let pg = ((u16::from(g) * af + 127) / 255) as u8;
            let pr = ((u16::from(r) * af + 127) / 255) as u8;
            bgra.extend_from_slice(&[pb, pg, pr, a]);
        }
    }
    bgra
}

#[cfg(windows)]
mod win_ocr {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use windows::Globalization::Language;
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::{OcrEngine, OcrResult as WinOcrResult};
    use windows::Storage::Streams::DataWriter;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows::core::HSTRING;

    use super::{OcrLine, OcrRect, OcrResult, OcrWord, rgba8_to_premultiplied_bgra8};
    use crate::app_id::win_impl::ensure_process_mta;
    use crate::error::{PlatformError, Result};

    /// Maximum time to wait for `OcrEngine::AvailableRecognizerLanguages`.
    const LANGUAGES_TIMEOUT: Duration = Duration::from_secs(10);
    /// Maximum time to wait for a single `OcrEngine::RecognizeAsync` call so a stuck WinRT
    /// operation can never freeze the caller or UI thread.
    const RECOGNIZE_TIMEOUT: Duration = Duration::from_secs(30);

    struct MtaComGuard(bool);

    impl MtaComGuard {
        fn init() -> Self {
            // Keep the process-wide MTA pinned alive so `windows-core`'s static `FactoryCache`
            // never holds dangling pointers after this short-lived worker calls `CoUninitialize`.
            ensure_process_mta();
            // SAFETY: Initializing a multi-threaded COM apartment on a short-lived worker thread
            // so WinRT async calls never deadlock on a caller's STA message pump.
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            Self(hr.is_ok())
        }
    }

    impl Drop for MtaComGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Paired with a successful `CoInitializeEx` on the same thread.
                // Because `ensure_process_mta()` keeps MTA usage >= 1, this detaches only the
                // worker thread without tearing down the process MTA or unloading WinRT servers.
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Runs `work` on a named MTA worker thread and waits at most `timeout` for the result.
    ///
    /// Returns an error if the thread fails to spawn, panics, or does not finish within `timeout`,
    /// avoiding any unbounded `.join()` that could freeze the UI.
    pub(super) fn run_on_mta_with_timeout<T, F>(
        thread_name: &str,
        timeout: Duration,
        work: F,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        ensure_process_mta();
        let (tx, rx) = mpsc::sync_channel::<Result<T>>(1);
        std::thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                // Execute all WinRT work inside an inner scope so every WinRT/COM object is
                // dropped strictly before `MtaComGuard::drop` calls `CoUninitialize()`.
                let result = {
                    let _com = MtaComGuard::init();
                    work()
                };
                let _ = tx.send(result);
            })
            .map_err(|e| PlatformError::Ocr(format!("failed to spawn {thread_name}: {e}")))?;

        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(PlatformError::Ocr(format!(
                "Windows OCR timed out after {}ms",
                timeout.as_millis()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(PlatformError::Ocr("OCR worker thread panicked".into()))
            }
        }
    }

    pub(super) fn available_languages() -> Vec<String> {
        run_on_mta_with_timeout("tg-ocr-languages", LANGUAGES_TIMEOUT, || {
            Ok(available_languages_in_mta())
        })
        .unwrap_or_default()
    }

    fn available_languages_in_mta() -> Vec<String> {
        let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let count = langs.Size().unwrap_or(0);
        for idx in 0..count {
            if let Ok(lang) = langs.GetAt(idx)
                && let Ok(tag) = lang.LanguageTag()
            {
                let s = tag.to_string_lossy();
                if !s.is_empty() {
                    out.push(s);
                }
            }
        }
        out
    }

    pub(super) fn recognize_rgba8(
        rgba: &[u8],
        width: u32,
        height: u32,
        language_tag: Option<&str>,
    ) -> Result<OcrResult> {
        recognize_rgba8_with_timeout(rgba, width, height, language_tag, RECOGNIZE_TIMEOUT)
    }

    pub(super) fn recognize_rgba8_with_timeout(
        rgba: &[u8],
        width: u32,
        height: u32,
        language_tag: Option<&str>,
        timeout: Duration,
    ) -> Result<OcrResult> {
        let bgra = rgba8_to_premultiplied_bgra8(rgba);
        let lang_owned = language_tag.map(str::to_owned);

        run_on_mta_with_timeout("tg-ocr-recognize", timeout, move || {
            recognize_in_mta(&bgra, width, height, lang_owned.as_deref(), timeout)
        })
    }

    fn recognize_in_mta(
        bgra: &[u8],
        width: u32,
        height: u32,
        language_tag: Option<&str>,
        timeout: Duration,
    ) -> Result<OcrResult> {
        let deadline = Instant::now() + timeout;

        let max_dim = OcrEngine::MaxImageDimension().unwrap_or(10_000);
        if width > max_dim || height > max_dim {
            return Err(PlatformError::Ocr(format!(
                "image {width}x{height} exceeds Windows OCR max dimension {max_dim}"
            )));
        }

        let engine = create_engine(language_tag)?;
        let active_lang = engine
            .RecognizerLanguage()
            .and_then(|l| l.LanguageTag())
            .map(|t| t.to_string_lossy())
            .unwrap_or_default();

        let writer =
            DataWriter::new().map_err(|e| PlatformError::from_win("DataWriter::new", e))?;
        writer
            .WriteBytes(bgra)
            .map_err(|e| PlatformError::from_win("DataWriter::WriteBytes", e))?;
        let buffer = writer
            .DetachBuffer()
            .map_err(|e| PlatformError::from_win("DataWriter::DetachBuffer", e))?;

        let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width as i32,
            height as i32,
            BitmapAlphaMode::Premultiplied,
        )
        .map_err(|e| PlatformError::from_win("SoftwareBitmap::CreateCopyWithAlphaFromBuffer", e))?;

        let async_op = engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| PlatformError::from_win("OcrEngine::RecognizeAsync", e))?;

        // Use `when` + `sync_channel::recv_timeout` rather than `IAsyncOperation::join()`.
        // `windows-future`'s `join()` waits with `INFINITE` inside `Waiter::drop` (even if
        // `SetCompleted` fails), which can deadlock indefinitely on a stalled WinRT operation.
        let (done_tx, done_rx) = mpsc::sync_channel::<Result<OcrResult>>(1);
        let active_lang_cb = active_lang;
        async_op
            .when(move |win_res| {
                let converted = win_res
                    .map_err(|e| PlatformError::from_win("OcrEngine::RecognizeAsync", e))
                    .map(|r| extract_ocr_result(&r, width, height, active_lang_cb));
                let _ = done_tx.send(converted);
            })
            .map_err(|e| PlatformError::from_win("OcrEngine::RecognizeAsync::when", e))?;

        let remaining = deadline.saturating_duration_since(Instant::now());
        match done_rx.recv_timeout(remaining) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = async_op.Cancel();
                Err(PlatformError::Ocr(format!(
                    "Windows OCR recognition timed out after {}ms",
                    timeout.as_millis()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(PlatformError::Ocr(
                "Windows OCR completion handler dropped without delivering a result".into(),
            )),
        }
    }

    fn extract_ocr_result(
        win_result: &WinOcrResult,
        width: u32,
        height: u32,
        language_tag: String,
    ) -> OcrResult {
        let text_angle = win_result.TextAngle().ok().and_then(|r| r.Value().ok());
        let mut lines = Vec::new();
        let mut combined_lines = Vec::new();

        if let Ok(win_lines) = win_result.Lines() {
            let line_count = win_lines.Size().unwrap_or(0);
            for li in 0..line_count {
                let Ok(line) = win_lines.GetAt(li) else {
                    continue;
                };
                let line_text = line.Text().map(|t| t.to_string_lossy()).unwrap_or_default();
                let mut words = Vec::new();
                let mut line_rect: Option<OcrRect> = None;

                if let Ok(win_words) = line.Words() {
                    let word_count = win_words.Size().unwrap_or(0);
                    for wi in 0..word_count {
                        let Ok(word) = win_words.GetAt(wi) else {
                            continue;
                        };
                        let wtext = word.Text().map(|t| t.to_string_lossy()).unwrap_or_default();
                        let wrect = word
                            .BoundingRect()
                            .map(|r| OcrRect::new(r.X, r.Y, r.Width, r.Height))
                            .unwrap_or(OcrRect::new(0.0, 0.0, 0.0, 0.0));
                        line_rect = Some(match line_rect {
                            Some(acc) => acc.union(wrect),
                            None => wrect,
                        });
                        words.push(OcrWord {
                            text: wtext,
                            rect: wrect,
                        });
                    }
                }

                if !line_text.is_empty() {
                    combined_lines.push(line_text.clone());
                }
                lines.push(OcrLine {
                    text: line_text,
                    rect: line_rect.unwrap_or(OcrRect::new(0.0, 0.0, 0.0, 0.0)),
                    words,
                });
            }
        }

        let text = if combined_lines.is_empty() {
            win_result
                .Text()
                .map(|t| t.to_string_lossy())
                .unwrap_or_default()
        } else {
            combined_lines.join("\n")
        };

        OcrResult {
            lines,
            text,
            text_angle,
            coordinate_width: width,
            coordinate_height: height,
            language_tag,
        }
    }

    fn create_engine(language_tag: Option<&str>) -> Result<OcrEngine> {
        if let Some(tag) = language_tag {
            let lang = Language::CreateLanguage(&HSTRING::from(tag))
                .map_err(|e| PlatformError::from_win("Language::CreateLanguage", e))?;
            if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&lang) {
                return Ok(engine);
            }
            return Err(PlatformError::Ocr(format!(
                "Windows OCR language '{tag}' is not installed"
            )));
        }

        if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
            return Ok(engine);
        }

        if let Ok(langs) = OcrEngine::AvailableRecognizerLanguages()
            && langs.Size().unwrap_or(0) > 0
            && let Ok(first) = langs.GetAt(0)
            && let Ok(engine) = OcrEngine::TryCreateFromLanguage(&first)
        {
            return Ok(engine);
        }

        Err(PlatformError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_to_premultiplied_bgra_swaps_channels_and_premultiplies() {
        // Opaque red (255, 0, 0, 255) -> BGRA (0, 0, 255, 255)
        // Half-alpha green (0, 200, 0, 128) -> BGRA (0, ~100, 0, 128)
        let input = [255, 0, 0, 255, 0, 200, 0, 128, 10, 20, 30, 0];
        let out = rgba8_to_premultiplied_bgra8(&input);
        assert_eq!(&out[0..4], &[0, 0, 255, 255]);
        assert_eq!(out[4], 0);
        assert!((i16::from(out[5]) - 100).abs() <= 1);
        assert_eq!(out[6], 0);
        assert_eq!(out[7], 128);
        assert_eq!(&out[8..12], &[0, 0, 0, 0]);
    }

    #[test]
    fn recognize_rejects_invalid_dimensions_and_buffer_sizes() {
        assert!(recognize_rgba8(&[], 0, 10, None).is_err());
        assert!(recognize_rgba8(&[0u8; 8], 10, 10, None).is_err());
    }

    /// Minimal clean 5×7 bitmap glyphs for uppercase letters and digits used to synthesize
    /// crisp high-contrast test images for headless Windows OCR verification.
    fn glyph_5x7(ch: char) -> [u8; 7] {
        match ch {
            'H' => [
                0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
            ],
            'E' => [
                0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
            ],
            'L' => [
                0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
            ],
            'O' => [
                0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
            ],
            'W' => [
                0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
            ],
            'R' => [
                0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
            ],
            'D' => [
                0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
            ],
            ' ' => [0; 7],
            _ => [0; 7],
        }
    }

    fn render_test_banner(text: &str, scale: usize) -> (Vec<u8>, u32, u32) {
        let margin = scale * 6;
        let glyph_w = 5 * scale;
        let glyph_h = 7 * scale;
        let advance = 7 * scale;
        let width = margin * 2 + text.chars().count() * advance;
        let height = margin * 2 + glyph_h;

        // Crisp white background, black text.
        let mut rgba = vec![255u8; width * height * 4];
        for (idx, ch) in text.chars().enumerate() {
            let rows = glyph_5x7(ch);
            let x0 = margin + idx * advance;
            let y0 = margin;
            for (ry, &bits) in rows.iter().enumerate() {
                for rx in 0..5 {
                    if (bits & (1 << (4 - rx))) != 0 {
                        for sy in 0..scale {
                            for sx in 0..scale {
                                let px = x0 + rx * scale + sx;
                                let py = y0 + ry * scale + sy;
                                let off = (py * width + px) * 4;
                                rgba[off] = 0;
                                rgba[off + 1] = 0;
                                rgba[off + 2] = 0;
                                rgba[off + 3] = 255;
                            }
                        }
                    }
                }
            }
        }
        let _ = glyph_w;
        (rgba, width as u32, height as u32)
    }

    #[test]
    fn ocr_recognizes_generated_image_when_english_pack_available() {
        let langs = available_languages();
        let Some(en_tag) = langs
            .iter()
            .find(|tag| tag.to_ascii_lowercase().starts_with("en"))
            .cloned()
        else {
            // Gracefully skip when no English OCR language pack is installed on the host.
            eprintln!("skipping Windows OCR test: no English OCR language installed ({langs:?})");
            return;
        };

        let (rgba, width, height) = render_test_banner("HELLO WORLD", 8);
        let result = recognize_rgba8(&rgba, width, height, Some(&en_tag))
            .expect("Windows.Media.Ocr recognition should succeed");

        let upper = result.text.to_ascii_uppercase();
        assert!(
            upper.contains("HELLO") || upper.contains("WORLD"),
            "expected OCR text to contain HELLO or WORLD, got: {:?} (lines: {:?})",
            result.text,
            result.lines
        );
        assert_eq!(result.coordinate_width, width);
        assert_eq!(result.coordinate_height, height);
        assert!(!result.lines.is_empty());
        for line in &result.lines {
            assert!(line.rect.width > 0.0 && line.rect.height > 0.0);
        }

        // Calling OCR again (including from a thread that enters and leaves an STA apartment)
        // must remain safe and must not crash or hang on cached WinRT factories.
        #[cfg(windows)]
        {
            let _sta = crate::app_id::win_impl::ComGuard::init_sta();
            let langs_again = available_languages();
            assert!(!langs_again.is_empty());
            let second = recognize_rgba8(&rgba, width, height, Some(&en_tag))
                .expect("second Windows.Media.Ocr recognition from STA thread should succeed");
            assert!(second.has_text());
        }
    }

    #[cfg(windows)]
    #[test]
    fn ocr_worker_timeout_returns_error_without_hanging() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let (release_tx, release_rx) = mpsc::channel::<()>();
        let started = Instant::now();
        let err = win_ocr::run_on_mta_with_timeout(
            "tg-ocr-timeout-test",
            Duration::from_millis(50),
            move || {
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
                Ok(())
            },
        )
        .expect_err("stalled worker must return a timeout error");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout wait took too long: {:?}",
            started.elapsed()
        );
        assert!(
            matches!(err, PlatformError::Ocr(ref msg) if msg.contains("timed out")),
            "expected OCR timeout error, got: {err:?}"
        );
        let _ = release_tx.send(());
    }
}
