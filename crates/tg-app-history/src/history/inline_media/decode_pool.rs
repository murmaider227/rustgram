//! Background decoding for chat media: full photos (scaled to display size), generated video
//! thumbnails, Lottie parsing and Lottie frame rasterisation run on a few shared worker threads;
//! the UI thread only uploads the finished pixels (within a per-frame budget).
//!
//! Same pattern as the sticker panel's Lottie workers (`stickers_list_widget.rs`): a job queue,
//! results sent back over a channel, a repaint requested when one lands. With background
//! decoding off ([`tg_emoji::background`]) every job runs inline, so tests and headless
//! screenshot harnesses see the same pixels on their first frame.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use egui::ColorImage;
use tg_lottie::{Composition, Size as LottieSize, render_composition};
use tg_video::{
    FrameData, Size as VideoSize, ThumbnailOptions, extract_thumbnail, extract_thumbnail_from_bytes,
};

use tg_app_core::stand_in::types::MsgId;
use tg_app_media::media::inline::InlineDocumentData;

/// `(sticker_id, size_bucket_px, step_idx)` of one rasterised Lottie frame.
pub(super) type LottieFrameKey = (u64, u32, u32);

/// Work for a decode worker.
pub(super) enum DecodeJob {
    /// Decode an encoded photo, scaled down to fit `max_side` (never up).
    Photo {
        msg_id: MsgId,
        revision: u64,
        bytes: Arc<[u8]>,
        max_side: u32,
    },
    /// Extract the first frame of a video as its thumbnail.
    VideoThumb {
        msg_id: MsgId,
        revision: u64,
        data: InlineDocumentData,
        max_side: u32,
        round_circle: bool,
    },
    /// Prepare the circle-masked thumbnail of a round video.
    RoundThumb {
        msg_id: MsgId,
        revision: u64,
        bytes: Arc<[u8]>,
        side_px: u32,
    },
    /// Parse Lottie JSON.
    LottieParse { sticker_id: u64, json: Arc<str> },
    /// Rasterise one Lottie frame.
    LottieFrame {
        key: LottieFrameKey,
        comp: Arc<Composition>,
        sample_frame: f64,
    },
}

/// A finished [`DecodeJob`].
pub(super) enum DecodeOutput {
    Photo {
        msg_id: MsgId,
        revision: u64,
        max_side: u32,
        /// Pixels plus `true` when the source was larger than `max_side` (so a bigger display
        /// size later needs a new decode).
        image: Option<(ColorImage, bool)>,
    },
    VideoThumb {
        msg_id: MsgId,
        revision: u64,
        image: Option<ColorImage>,
    },
    RoundThumb {
        msg_id: MsgId,
        revision: u64,
        image: Option<ColorImage>,
    },
    LottieParsed {
        sticker_id: u64,
        comp: Option<Arc<Composition>>,
    },
    LottieFrame {
        key: LottieFrameKey,
        image: ColorImage,
    },
}

/// Runs `job` on the calling thread (the workers call this too).
pub(super) fn run_job(job: DecodeJob) -> DecodeOutput {
    match job {
        DecodeJob::Photo {
            msg_id,
            revision,
            bytes,
            max_side,
        } => DecodeOutput::Photo {
            msg_id,
            revision,
            max_side,
            image: decode_photo_scaled(&bytes, max_side),
        },
        DecodeJob::VideoThumb {
            msg_id,
            revision,
            data,
            max_side,
            round_circle,
        } => DecodeOutput::VideoThumb {
            msg_id,
            revision,
            image: extract_video_thumb(data, max_side, round_circle),
        },
        DecodeJob::RoundThumb {
            msg_id,
            revision,
            bytes,
            side_px,
        } => DecodeOutput::RoundThumb {
            msg_id,
            revision,
            image: super::round_video::round_thumb_rgba(&bytes, side_px).map(|(side, rgba)| {
                ColorImage::from_rgba_unmultiplied([side as usize, side as usize], &rgba)
            }),
        },
        DecodeJob::LottieParse { sticker_id, json } => DecodeOutput::LottieParsed {
            sticker_id,
            comp: Composition::from_json(&json).ok().map(Arc::new),
        },
        DecodeJob::LottieFrame {
            key,
            comp,
            sample_frame,
        } => {
            let size = key.1;
            let fb = render_composition(&comp, sample_frame, LottieSize::new(size, size));
            DecodeOutput::LottieFrame {
                key,
                image: ColorImage::from_rgba_premultiplied(
                    [fb.width as usize, fb.height as usize],
                    &fb.data,
                ),
            }
        }
    }
}

/// Decodes `bytes` with the inline photo limits of [`super::decode_inline_photo_rgba`], scaled
/// with `tg_media`'s smooth downscale to fit a `max_side` square.
fn decode_photo_scaled(bytes: &[u8], max_side: u32) -> Option<(ColorImage, bool)> {
    let decoded = tg_media::image::read_image(tg_media::image::ReadArgs {
        content: bytes.to_vec(),
        max_size: Some((max_side, max_side)),
        ..Default::default()
    })
    .ok()?;
    let (w, h) = decoded.image.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let downscaled = decoded.scale < 1.0;
    let raw = decoded.image.into_raw();
    Some((
        ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &raw),
        downscaled,
    ))
}

fn extract_video_thumb(
    data: InlineDocumentData,
    max_side: u32,
    round_circle: bool,
) -> Option<ColorImage> {
    let opts = ThumbnailOptions {
        position_ms: 0,
        max_size: Some(VideoSize::new(max_side.max(64), max_side.max(64))),
        keep_alpha: round_circle,
        round_circle,
    };
    let frame = match data {
        InlineDocumentData::File(path) => extract_thumbnail(&path, opts),
        InlineDocumentData::Bytes(bytes) => extract_thumbnail_from_bytes(bytes, opts),
        // A file that is still downloading shows its document thumbnail instead.
        InlineDocumentData::Stream(_) => return None,
    }
    .ok()?;
    let FrameData::Rgba(rgba) = frame.data else {
        return None;
    };
    Some(ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        &rgba,
    ))
}

struct QueuedJob {
    job: DecodeJob,
    results: Sender<DecodeOutput>,
    ctx: egui::Context,
}

/// Process-wide worker pool. Jobs are taken newest first: the newest request is what the
/// viewport shows now.
struct Pool {
    queue: Mutex<VecDeque<QueuedJob>>,
    ready: Condvar,
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let pool: &'static Pool = Box::leak(Box::new(Pool {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        }));
        let workers = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 3));
        for idx in 0..workers {
            let _ = std::thread::Builder::new()
                .name(format!("tg-media-decode-{idx}"))
                .spawn(move || worker_loop(pool));
        }
        pool
    })
}

fn worker_loop(pool: &'static Pool) {
    loop {
        let next = {
            let mut queue = pool.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if let Some(job) = queue.pop_back() {
                    break job;
                }
                queue = pool
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let QueuedJob { job, results, ctx } = next;
        let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_job(job)));
        if let Ok(output) = output
            && results.send(output).is_ok()
        {
            ctx.request_repaint();
        }
    }
}

/// One runtime's connection to the decode pool.
pub(super) struct DecodeQueue {
    results_tx: Sender<DecodeOutput>,
    results_rx: Receiver<DecodeOutput>,
    /// Finished outputs not uploaded yet (the per-frame upload budget ran out).
    backlog: VecDeque<DecodeOutput>,
    background: Option<bool>,
    submitted: u64,
}

impl Default for DecodeQueue {
    fn default() -> Self {
        let (results_tx, results_rx) = mpsc::channel();
        Self {
            results_tx,
            results_rx,
            backlog: VecDeque::new(),
            background: None,
            submitted: 0,
        }
    }
}

impl DecodeQueue {
    /// Overrides the process-wide background-decode switch (`None` follows it).
    pub(super) fn set_background(&mut self, background: Option<bool>) {
        self.background = background;
    }

    pub(super) fn is_background(&self) -> bool {
        self.background
            .unwrap_or_else(tg_emoji::background::background_decode_enabled)
    }

    /// Number of jobs handed to the pool so far.
    pub(super) const fn submitted(&self) -> u64 {
        self.submitted
    }

    /// Runs `job` inline (returning its output) or hands it to the pool (returning `None`).
    pub(super) fn submit(&mut self, ctx: &egui::Context, job: DecodeJob) -> Option<DecodeOutput> {
        if !self.is_background() {
            return Some(run_job(job));
        }
        self.submitted += 1;
        let pool = pool();
        pool.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(QueuedJob {
                job,
                results: self.results_tx.clone(),
                ctx: ctx.clone(),
            });
        pool.ready.notify_one();
        None
    }

    /// Takes the next finished output, if any.
    pub(super) fn next_output(&mut self) -> Option<DecodeOutput> {
        if let Some(output) = self.backlog.pop_front() {
            return Some(output);
        }
        self.results_rx.try_recv().ok()
    }

    /// Puts back an output that did not fit this frame's upload budget.
    pub(super) fn defer(&mut self, output: DecodeOutput) {
        self.backlog.push_front(output);
    }
}
