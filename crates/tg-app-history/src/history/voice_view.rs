//! Voice message bubble: waveform bars and the status line under them.
//!
//! Port origin: `PaintWaveform` and the voice part of `Document::draw` / `updateStatusText`
//! (`Telegram/SourceFiles/history/view/media/history_view_document.cpp`),
//! `File::setStatusSize` (`history_view_file.cpp`) and `Ui::FormatDurationAndSizeText`,
//! `FormatDurationText`, `FormatPlayedText`, `FormatSizeText`
//! (`Telegram/SourceFiles/ui/text/format_values.cpp`).
//!
//! Internal design differs from tdesktop: `PaintWaveform` rescales the waveform on every
//! paint; here the bars of a message are computed once per waveform and width and kept in
//! [`WaveformBarsCache`], so a frame only fills rectangles.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Vec2};
use tg_history::consts::{MSG_WAVEFORM_BAR, MSG_WAVEFORM_MAX, MSG_WAVEFORM_MIN, MSG_WAVEFORM_SKIP};
use tg_media::audio::waveform_decode_5bit;

use tg_app_core::stand_in::types::MsgId;

/// `Media::Player::kWaveformSamplesCount`: bars of a voice message without a waveform.
const WAVEFORM_SAMPLES_COUNT: usize = 100;
/// `st::lineWidth`.
const LINE_WIDTH: f32 = 1.0;
/// `st::mediaUnreadSkip`: gap between the status text and the unread dot.
pub const MEDIA_UNREAD_SKIP: f32 = 5.0;
/// `st::mediaUnreadTop`: unread dot offset below the status line top.
pub const MEDIA_UNREAD_TOP: f32 = 6.0;
/// `st::mediaUnreadSize`: unread dot diameter.
pub const MEDIA_UNREAD_SIZE: f32 = 7.0;
/// Messages whose bars are kept before the cache starts over.
const BARS_CACHE_CAP: usize = 512;

/// One waveform bar, relative to the waveform's left / top (`p.translate(nameleft, top)`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveBar {
    /// `barLeft`.
    pub left: f32,
    /// `barTop = st::lineWidth + (st::msgWaveformMax - barValue) / 2.`.
    pub top: f32,
    /// `barHeight = st::msgWaveformMin + barValue`.
    pub height: f32,
}

/// Bars of a voice waveform resampled to `available_width` (`PaintWaveform`).
///
/// `waveform` holds decoded `0..=31` samples (`documentWaveformDecode`). An empty one is drawn
/// as [`WAVEFORM_SAMPLES_COUNT`] minimum-height bars, like a `VoiceData` without a waveform.
/// Bar heights are scaled by the loudest sample (`VoiceData::wavemax`), so the loudest bar is
/// nearly `st::msgWaveformMax` tall however quiet the recording is.
#[must_use]
pub fn waveform_bars(waveform: &[u8], available_width: i32) -> Vec<WaveBar> {
    let has_waveform = !waveform.is_empty();
    let wf_size = if has_waveform {
        waveform.len() as i32
    } else {
        WAVEFORM_SAMPLES_COUNT as i32
    };
    let bar_step = MSG_WAVEFORM_BAR + MSG_WAVEFORM_SKIP;
    let bar_count = (available_width / bar_step).min(wf_size);
    if bar_count <= 0 {
        return Vec::new();
    }
    let wave_max = waveform.iter().copied().max().map_or(0, i32::from);
    let bar_norm_value = wave_max + 1;
    let max_delta = MSG_WAVEFORM_MAX - MSG_WAVEFORM_MIN;

    let mut bars = Vec::with_capacity(bar_count as usize);
    let (mut bar_left, mut sum, mut max_value) = (0i32, 0i32, 0i32);
    for i in 0..wf_size {
        let value = waveform.get(i as usize).copied().map_or(0, i32::from);
        if sum + bar_count < wf_size {
            max_value = max_value.max(value);
            sum += bar_count;
            continue;
        }
        sum = sum + bar_count - wf_size;
        if sum < (bar_count + 1) / 2 {
            max_value = max_value.max(value);
        }
        let bar_value = (max_value * max_delta + bar_norm_value / 2) / bar_norm_value;
        bars.push(WaveBar {
            left: bar_left as f32,
            top: LINE_WIDTH + (MSG_WAVEFORM_MAX - bar_value) as f32 / 2.0,
            height: (MSG_WAVEFORM_MIN + bar_value) as f32,
        });
        bar_left += bar_step;
        max_value = if sum < (bar_count + 1) / 2 { 0 } else { value };
    }
    bars
}

/// Width of the waveform area for a voice bubble whose name column is `name_width` wide
/// (`namewidth + st::msgWaveformSkip` in `Document::draw`).
#[must_use]
pub const fn waveform_available_width(name_width: i32) -> i32 {
    name_width + MSG_WAVEFORM_SKIP
}

/// Played share of the waveform before any colour is chosen (`progress` in `Document::draw`):
/// an incoming voice message nobody listened to yet is drawn fully active.
#[must_use]
pub fn waveform_progress(outgoing: bool, has_playback: bool, unread: bool, playback: f32) -> f32 {
    if !outgoing && !has_playback && unread {
        1.0
    } else if has_playback {
        playback.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

struct CachedBars {
    waveform_5bit: Vec<u8>,
    available_width: i32,
    bars: Rc<[WaveBar]>,
}

/// Waveform bars per message, recomputed only when the waveform bytes or the width change.
#[derive(Default)]
pub struct WaveformBarsCache {
    entries: HashMap<MsgId, CachedBars>,
    computed: u64,
}

impl std::fmt::Debug for WaveformBarsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaveformBarsCache")
            .field("entries", &self.entries.len())
            .field("computed", &self.computed)
            .finish()
    }
}

impl Clone for WaveformBarsCache {
    /// A clone starts empty: the bars are derived data.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl WaveformBarsCache {
    /// Bars of `msg_id`'s 5-bit packed waveform at `available_width`.
    pub fn bars(
        &mut self,
        msg_id: MsgId,
        waveform_5bit: &[u8],
        available_width: i32,
    ) -> Rc<[WaveBar]> {
        if let Some(hit) = self.entries.get(&msg_id)
            && hit.available_width == available_width
            && hit.waveform_5bit == waveform_5bit
        {
            return Rc::clone(&hit.bars);
        }
        if self.entries.len() >= BARS_CACHE_CAP {
            self.entries.clear();
        }
        self.computed += 1;
        let decoded = waveform_decode_5bit(waveform_5bit);
        let bars: Rc<[WaveBar]> = waveform_bars(&decoded, available_width).into();
        self.entries.insert(
            msg_id,
            CachedBars {
                waveform_5bit: waveform_5bit.to_vec(),
                available_width,
                bars: Rc::clone(&bars),
            },
        );
        bars
    }

    /// How many times bars were computed (not served from the cache).
    #[must_use]
    pub const fn computed(&self) -> u64 {
        self.computed
    }
}

/// Fills the bars at `origin`: the part left of `available_width * progress` in `active`, the
/// rest in `inactive`, a bar the boundary crosses split in two (`PaintWaveform`).
pub fn paint_waveform_bars(
    painter: &Painter,
    origin: Pos2,
    bars: &[WaveBar],
    available_width: i32,
    progress: f32,
    active: Color32,
    inactive: Color32,
) {
    let active_width = (available_width as f32 * progress).round();
    let bar_width = MSG_WAVEFORM_BAR as f32;
    let fill = |left: f32, bar: &WaveBar, width: f32, color: Color32| {
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(origin.x + left, origin.y + bar.top),
                Vec2::new(width, bar.height),
            ),
            CornerRadius::ZERO,
            color,
        );
    };
    for bar in bars {
        if bar.left < active_width && bar.left + bar_width > active_width {
            let left_width = active_width - bar.left;
            fill(bar.left, bar, left_width, active);
            fill(active_width, bar, bar_width - left_width, inactive);
        } else if bar.left >= active_width {
            fill(bar.left, bar, bar_width, inactive);
        } else {
            fill(bar.left, bar, bar_width, active);
        }
    }
}

/// What the status line of a voice message describes (`File::_statusSize`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceStatus {
    /// `Ui::FileStatusSizeReady`: not loaded, "00:04, 15.9 KB".
    Ready,
    /// `Ui::FileStatusSizeLoaded`: loaded and not playing, "00:04".
    Loaded,
    /// Playing or paused, "00:01 / 00:04".
    Played {
        /// Whole seconds played (`state.position / state.frequency`).
        position_secs: u32,
    },
}

/// `FormatTenths`.
fn format_tenths(tenths: u64, decimal_point: char) -> String {
    format!("{}{decimal_point}{}", tenths / 10, tenths % 10)
}

/// `Ui::FormatSizeText` with the locale's decimal point.
#[must_use]
pub fn format_size_text(size_bytes: u64, decimal_point: char) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if size_bytes >= MB {
        format!(
            "{} {}",
            format_tenths(size_bytes * 10 / MB, decimal_point),
            tg_lang::tr::lng_size_megabytes()
        )
    } else if size_bytes >= KB {
        format!(
            "{} {}",
            format_tenths(size_bytes * 10 / KB, decimal_point),
            tg_lang::tr::lng_size_kilobytes()
        )
    } else {
        format!("{size_bytes} {}", tg_lang::tr::lng_size_bytes())
    }
}

/// Status line of a voice message (`File::setStatusSize` with a duration).
///
/// A voice message whose size is not known (`size_bytes == 0`, e.g. a quick reply preview)
/// shows its duration alone instead of "00:04, 0 B".
#[must_use]
pub fn voice_status_text(
    status: VoiceStatus,
    duration_secs: u32,
    size_bytes: u64,
    decimal_point: char,
) -> String {
    let duration = tg_media::audio::format_duration_text(i64::from(duration_secs));
    match status {
        VoiceStatus::Ready if size_bytes > 0 => tg_lang::tr::lng_duration_and_size(
            &duration,
            &format_size_text(size_bytes, decimal_point),
        ),
        VoiceStatus::Ready | VoiceStatus::Loaded => duration,
        VoiceStatus::Played { position_secs } => tg_lang::tr::lng_duration_played(
            &tg_media::audio::format_duration_text(i64::from(position_secs)),
            &duration,
        ),
    }
}

/// `QLocale().decimalPoint()`: the decimal separator of the user's regional format.
#[must_use]
pub fn locale_decimal_point() -> char {
    static POINT: OnceLock<char> = OnceLock::new();
    *POINT.get_or_init(tg_platform::system_info::system_decimal_point)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_media::audio::waveform_encode_5bit;

    /// `PaintWaveform` transcribed literally from `history_view_document.cpp`, as the
    /// reference the port is checked against.
    fn reference_bars(
        wf: Option<&[u8]>,
        wavemax: i32,
        available_width: i32,
    ) -> Vec<(i32, f64, i32)> {
        let wf_size = wf.map_or(100, |w| w.len() as i32);
        let bar_width = 2;
        let bar_count = (available_width / (bar_width + 1)).min(wf_size);
        let bar_norm_value = (if wf.is_some() { wavemax } else { 0 }) + 1;
        let max_delta = 17 - 3;
        let mut out = Vec::new();
        let (mut bar_left, mut sum, mut max_value) = (0, 0, 0);
        for i in 0..wf_size {
            let value = wf.map_or(0, |w| i32::from(w[i as usize]));
            if sum + bar_count < wf_size {
                max_value = max_value.max(value);
                sum += bar_count;
                continue;
            }
            sum = sum + bar_count - wf_size;
            if sum < (bar_count + 1) / 2 {
                max_value = max_value.max(value);
            }
            let bar_value = ((max_value * max_delta) + (bar_norm_value / 2)) / bar_norm_value;
            out.push((
                bar_left,
                1.0 + f64::from(17 - bar_value) / 2.0,
                3 + bar_value,
            ));
            bar_left += bar_width + 1;
            max_value = if sum < (bar_count + 1) / 2 { 0 } else { value };
        }
        out
    }

    fn sample_waveform(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| ((i * 7 + (i / 3) * 5) % 13) as u8)
            .collect()
    }

    #[test]
    fn bars_match_the_reference_for_every_width() {
        let wf = sample_waveform(100);
        let wavemax = i32::from(*wf.iter().max().expect("non-empty"));
        for width in [0, 2, 3, 48, 149, 200, 243, 300, 301, 400] {
            let got = waveform_bars(&wf, width);
            let want = reference_bars(Some(&wf), wavemax, width);
            assert_eq!(got.len(), want.len(), "width {width}");
            for (bar, (left, top, height)) in got.iter().zip(want) {
                assert_eq!(bar.left, left as f32, "width {width}");
                assert!((f64::from(bar.top) - top).abs() < 1e-6, "width {width}");
                assert_eq!(bar.height, height as f32, "width {width}");
            }
        }
    }

    #[test]
    fn a_quiet_recording_is_scaled_by_its_loudest_sample() {
        // Loudest sample 12 of 31: tdesktop scales by `wavemax + 1`, so that bar is 16 px
        // of the 17 px line. Scaling by 31 (the old painter) gave 8 px.
        let wf: Vec<u8> = (0..100u8).map(|i| i % 13).collect();
        let bars = waveform_bars(&wf, 300);
        assert_eq!(bars.len(), 100);
        let tallest = bars.iter().map(|b| b.height).fold(0.0, f32::max);
        assert_eq!(
            tallest, 16.0,
            "(12 * 14 + 6) / 13 = 13, plus the 3 px minimum"
        );
        let loud = bars.iter().find(|b| b.height == tallest).expect("a bar");
        assert_eq!(loud.top, 1.0 + (17.0 - 13.0) / 2.0);
        // Bars are centred on one line, not bottom-aligned.
        let quiet = bars.iter().find(|b| b.height == 3.0).expect("a silent bar");
        assert_eq!(quiet.top, 1.0 + 17.0 / 2.0);
    }

    #[test]
    fn bar_count_is_limited_by_width_and_by_samples() {
        let wf = sample_waveform(100);
        assert_eq!(waveform_bars(&wf, 150).len(), 50);
        assert_eq!(waveform_bars(&wf, 1000).len(), 100);
        assert_eq!(waveform_bars(&wf, 2).len(), 0);
        let bars = waveform_bars(&wf, 150);
        assert_eq!(bars[1].left - bars[0].left, 3.0);
    }

    #[test]
    fn a_missing_waveform_draws_a_hundred_minimum_bars() {
        let bars = waveform_bars(&[], 400);
        assert_eq!(bars.len(), 100);
        assert!(bars.iter().all(|b| b.height == 3.0 && b.top == 9.5));
        assert_eq!(
            bars.len(),
            reference_bars(None, 0, 400).len(),
            "same count as tdesktop"
        );
    }

    #[test]
    fn unread_incoming_voice_is_fully_active() {
        assert_eq!(waveform_progress(false, false, true, 0.0), 1.0);
        assert_eq!(waveform_progress(true, false, true, 0.0), 0.0);
        assert_eq!(waveform_progress(false, false, false, 0.0), 0.0);
        assert_eq!(waveform_progress(false, true, true, 0.25), 0.25);
    }

    #[test]
    fn the_cache_computes_bars_once_per_waveform_and_width() {
        let packed = waveform_encode_5bit(&sample_waveform(100));
        let mut cache = WaveformBarsCache::default();
        let first = cache.bars(MsgId(1), &packed, 300);
        let again = cache.bars(MsgId(1), &packed, 300);
        assert!(Rc::ptr_eq(&first, &again));
        assert_eq!(cache.computed(), 1);
        let narrower = cache.bars(MsgId(1), &packed, 150);
        assert_eq!(narrower.len(), 50);
        assert_eq!(cache.computed(), 2);
        let other = waveform_encode_5bit(&sample_waveform(64));
        assert_eq!(cache.bars(MsgId(1), &other, 150).len(), 50);
        assert_eq!(cache.computed(), 3);
    }

    #[test]
    fn status_reads_duration_and_size_until_loaded() {
        // 15.9 KB: 16_300 * 10 / 1024 = 159 tenths.
        assert_eq!(
            voice_status_text(VoiceStatus::Ready, 4, 16_300, ','),
            "00:04, 15,9 KB"
        );
        assert_eq!(
            voice_status_text(VoiceStatus::Ready, 4, 16_300, '.'),
            "00:04, 15.9 KB"
        );
        assert_eq!(
            voice_status_text(VoiceStatus::Loaded, 4, 16_300, '.'),
            "00:04"
        );
        assert_eq!(
            voice_status_text(VoiceStatus::Played { position_secs: 1 }, 64, 16_300, '.'),
            "00:01 / 01:04"
        );
        assert_eq!(voice_status_text(VoiceStatus::Ready, 4, 0, '.'), "00:04");
    }

    #[test]
    fn sizes_use_tdesktop_units_and_truncated_tenths() {
        assert_eq!(format_size_text(512, '.'), "512 B");
        assert_eq!(format_size_text(1024, '.'), "1.0 KB");
        assert_eq!(format_size_text(7_500, '.'), "7.3 KB");
        assert_eq!(format_size_text(3 * 1024 * 1024 + 600_000, ','), "3,5 MB");
    }
}
