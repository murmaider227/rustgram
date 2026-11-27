//! Measures video playback pacing the way the media viewer drives [`StreamingPlayer`]:
//! one `step_video` per UI frame (~16.7 ms), a fixed amount of paint work per frame, and an
//! optional network model (round trip + bandwidth, with prefetch) in front of the file.
//!
//! A fake audio sink consumes the decoded audio in real time and serves as the audio clock;
//! the drift is `presented video pts - audio clock` at the moment the frame is presented.
//!
//! Usage:
//!   cargo run --release -p tg-video --example pacing_probe -- <file> [seconds=20]
//!     [--net <rtt_ms>:<kbytes_per_s>] [--busy <threads>] [--paint <ms>] [--csv <out.csv>]

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tg_video::streaming::{LocalFileLoader, RangeLoader};
use tg_video::{
    Decoder, FfmpegStreamDecoder, FramePopOutcome, OutputPixelFormat, PlaybackMode,
    PlaybackOptions, SliceConfig, StreamingPlayer, ThreadedDecoder,
};

/// Network model: a part becomes readable `rtt` after its request, limited by `bandwidth`.
struct NetLoader {
    inner: LocalFileLoader,
    start: Instant,
    rtt_ms: f64,
    bytes_per_ms: f64,
    state: Mutex<NetState>,
}

#[derive(Default)]
struct NetState {
    ready_at: HashMap<u64, f64>,
    link_free_at: f64,
}

impl NetLoader {
    fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn schedule(&self, st: &mut NetState, offset: u64, len: usize) -> f64 {
        if let Some(at) = st.ready_at.get(&offset) {
            return *at;
        }
        let now = self.now_ms();
        let transfer = len as f64 / self.bytes_per_ms;
        let at = (now + self.rtt_ms).max(st.link_free_at + transfer);
        st.link_free_at = at;
        st.ready_at.insert(offset, at);
        at
    }
}

impl RangeLoader for NetLoader {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn load_range(&self, offset: u64, len: usize) -> tg_video::error::Result<Vec<u8>> {
        let at = {
            let mut st = self.state.lock().map_err(|_| {
                tg_video::error::VideoError::Streaming(tg_video::StreamingError::LoadFailed)
            })?;
            self.schedule(&mut st, offset, len)
        };
        let wait = at - self.now_ms();
        if wait > 0.0 {
            thread::sleep(Duration::from_secs_f64(wait / 1000.0));
        }
        self.inner.load_range(offset, len)
    }

    fn prefetch(&self, offset: u64, len: usize) {
        if let Ok(mut st) = self.state.lock() {
            self.schedule(&mut st, offset, len);
        }
    }

    fn cancel(&self, offset: u64) {
        if let Ok(mut st) = self.state.lock() {
            let now = self.now_ms();
            if st.ready_at.get(&offset).is_some_and(|at| *at > now) {
                st.ready_at.remove(&offset);
            }
        }
    }
}

struct Args {
    file: PathBuf,
    seconds: f64,
    net: Option<(f64, f64)>,
    busy: usize,
    paint_ms: f64,
    csv: Option<PathBuf>,
    threaded: bool,
}

fn parse_args() -> Option<Args> {
    let mut it = env::args().skip(1);
    let file = PathBuf::from(it.next()?);
    let mut args = Args {
        file,
        seconds: 20.0,
        net: None,
        busy: 0,
        paint_ms: 3.0,
        csv: None,
        threaded: false,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--net" => {
                let v = it.next()?;
                let (rtt, kbs) = v.split_once(':')?;
                args.net = Some((rtt.parse().ok()?, kbs.parse().ok()?));
            }
            "--busy" => args.busy = it.next()?.parse().ok()?,
            "--paint" => args.paint_ms = it.next()?.parse().ok()?,
            "--csv" => args.csv = Some(PathBuf::from(it.next()?)),
            "--threaded" => args.threaded = true,
            other => args.seconds = other.parse().ok()?,
        }
    }
    Some(args)
}

fn spin_for(ms: f64) {
    let until = Instant::now() + Duration::from_secs_f64(ms / 1000.0);
    let mut x = 0u64;
    while Instant::now() < until {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        std::hint::black_box(x);
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() -> ExitCode {
    let Some(args) = parse_args() else {
        eprintln!("usage: pacing_probe <file> [seconds] [--net rtt:kBps] [--busy n] [--paint ms]");
        return ExitCode::from(2);
    };
    let stop = Arc::new(AtomicBool::new(false));
    let busy: Vec<_> = (0..args.busy)
        .map(|_| {
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    spin_for(5.0);
                }
            })
        })
        .collect();

    let file_loader = match LocalFileLoader::open(&args.file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("open failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let loader: Arc<dyn RangeLoader> = match args.net {
        Some((rtt, kbs)) => Arc::new(NetLoader {
            inner: file_loader,
            start: Instant::now(),
            rtt_ms: rtt,
            bytes_per_ms: kbs * 1024.0 / 1000.0,
            state: Mutex::new(NetState::default()),
        }),
        None => Arc::new(file_loader),
    };
    let opts = PlaybackOptions {
        mode: PlaybackMode::Both,
        output_format: OutputPixelFormat::Rgba,
        apply_rotation: true,
        ..PlaybackOptions::default()
    };
    let t_open = Instant::now();
    let decoder =
        FfmpegStreamDecoder::from_range_loader(loader, None, SliceConfig::default(), opts.clone());
    let player: tg_video::Result<StreamingPlayer<Box<dyn Decoder>>> = decoder.and_then(|d| {
        let d: Box<dyn Decoder> = if args.threaded {
            Box::new(ThreadedDecoder::spawn(d)?)
        } else {
            Box::new(d)
        };
        StreamingPlayer::new(d, opts)
    });
    let mut player = match player {
        Ok(p) => p,
        Err(e) => {
            eprintln!("player failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let open_ms = t_open.elapsed().as_secs_f64() * 1000.0;
    let fps = player.info().video.as_ref().map_or(0.0, |v| v.fps);
    let has_audio = player.info().audio.is_some();

    let base = Instant::now();
    let now_ms = || base.elapsed().as_secs_f64() * 1000.0;
    let world = |t: f64| t.round() as i64;

    let mut presents: Vec<(f64, i64)> = Vec::new();
    let mut step_ms: Vec<f64> = Vec::new();
    let mut audio_samples_played: f64 = 0.0;
    let mut audio_first_pts: Option<i64> = None;
    let mut audio_buffer_frames: usize = 0;
    let mut audio_queue_peak: usize = 0;
    let mut audio_rate = 48_000.0;
    let mut last_sink_ms: Option<f64> = None;
    let mut audio_clock_at_present: Vec<Option<f64>> = Vec::new();
    let frame_period = 1000.0 / 60.0;

    player.start(0);
    let end = args.seconds * 1000.0;
    let mut next_tick = 0.0;
    while now_ms() < end {
        let t0 = now_ms();
        let outcome = player.step_video(world(t0));
        let t1 = now_ms();
        step_ms.push(t1 - t0);

        // Fake audio sink: pull decoded audio into a sink buffer, play it out in real time.
        while let Some(a) = player.pop_audio_frame() {
            audio_first_pts.get_or_insert(a.pts_ms);
            audio_rate = f64::from(a.sample_rate.max(1));
            audio_buffer_frames += a.samples.len() / usize::from(a.channels.max(1));
        }
        audio_queue_peak = audio_queue_peak.max(audio_buffer_frames);
        if let Some(prev) = last_sink_ms {
            let want = (t1 - prev) / 1000.0 * audio_rate;
            let take = want.min(audio_buffer_frames as f64);
            audio_samples_played += take;
            audio_buffer_frames -= take as usize;
        }
        if audio_first_pts.is_some() {
            last_sink_ms = Some(t1);
        }

        match outcome {
            Ok(FramePopOutcome::Ready { frame, .. }) => {
                // The viewer copies the RGBA buffer into an egui ColorImage.
                if let tg_video::FrameData::Rgba(bytes) = &frame.data {
                    std::hint::black_box(bytes.clone());
                }
                presents.push((t1, frame.pts_ms));
                audio_clock_at_present.push(
                    audio_first_pts.map(|p| p as f64 + audio_samples_played / audio_rate * 1000.0),
                );
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("step failed: {e}");
                break;
            }
        }
        if player.stage() == tg_video::PlayerStage::Finished {
            break;
        }
        spin_for(args.paint_ms);
        next_tick += frame_period;
        let now = now_ms();
        if next_tick < now {
            next_tick = now;
        } else {
            thread::sleep(Duration::from_secs_f64((next_tick - now) / 1000.0));
        }
    }
    stop.store(true, Ordering::Relaxed);
    for b in busy {
        let _ = b.join();
    }

    if presents.len() < 2 {
        eprintln!("not enough frames presented");
        return ExitCode::FAILURE;
    }
    let frame_ms = if fps > 0.0 { 1000.0 / fps } else { 33.3 };
    let (w0, p0) = presents[0];
    let mut jitter = Vec::new();
    let mut freezes = 0usize;
    let mut longest_gap: f64 = 0.0;
    for w in presents.windows(2) {
        let dw = w[1].0 - w[0].0;
        let dp = (w[1].1 - w[0].1) as f64;
        jitter.push(dw - dp);
        longest_gap = longest_gap.max(dw);
        if dw > dp + 2.0 * frame_period {
            freezes += 1;
        }
    }
    let n = jitter.len() as f64;
    let mean = jitter.iter().sum::<f64>() / n;
    let sd = (jitter.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n).sqrt();
    let mut abs_j: Vec<f64> = jitter.iter().map(|d| d.abs()).collect();
    abs_j.sort_by(f64::total_cmp);
    let wall_drift: Vec<f64> = presents
        .iter()
        .map(|(w, p)| (w - w0) - (*p - p0) as f64)
        .collect();
    let av_drift: Vec<f64> = presents
        .iter()
        .zip(&audio_clock_at_present)
        .filter_map(|((_, p), a)| a.map(|a| *p as f64 - a))
        .collect();
    let mut steps = step_ms.clone();
    steps.sort_by(f64::total_cmp);
    let span_pts = (presents[presents.len() - 1].1 - p0) as f64;
    let expected = (span_pts / frame_ms).round() + 1.0;

    let name = args
        .file
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    println!(
        "file={name} fps={fps:.2} audio={has_audio} net={:?} busy={} threaded={} open_ms={open_ms:.0}",
        args.net, args.busy, args.threaded
    );
    println!(
        "presented={} expected={expected:.0} dropped_by_queue={} freezes(>2 ticks late)={freezes} longest_gap_ms={longest_gap:.0}",
        presents.len(),
        player.dropped_frames()
    );
    println!(
        "interval_jitter_ms: sd={sd:.2} p50={:.1} p95={:.1} p99={:.1} max={:.1}",
        percentile(&abs_j, 0.5),
        percentile(&abs_j, 0.95),
        percentile(&abs_j, 0.99),
        percentile(&abs_j, 1.0)
    );
    println!(
        "ui_step_ms: p50={:.2} p95={:.2} p99={:.2} max={:.1}",
        percentile(&steps, 0.5),
        percentile(&steps, 0.95),
        percentile(&steps, 0.99),
        percentile(&steps, 1.0)
    );
    let last = |v: &[f64]| v.last().copied().unwrap_or(0.0);
    let maxabs = |v: &[f64]| v.iter().fold(0.0f64, |m, d| m.max(d.abs()));
    println!(
        "video_vs_wall_ms: end={:.0} max={:.0}",
        last(&wall_drift),
        maxabs(&wall_drift)
    );
    if !av_drift.is_empty() {
        println!(
            "av_drift_ms (video pts - audio clock): start={:.0} end={:.0} max_abs={:.0} audio_sink_peak_ms={:.0}",
            av_drift[0],
            last(&av_drift),
            maxabs(&av_drift),
            audio_queue_peak as f64 / audio_rate * 1000.0
        );
    }
    if let Some(csv) = args.csv {
        let mut s = String::from("wall_ms,pts_ms,audio_clock_ms\n");
        for ((w, p), a) in presents.iter().zip(&audio_clock_at_present) {
            s.push_str(&format!(
                "{w:.2},{p},{}\n",
                a.map_or_else(String::new, |a| format!("{a:.1}"))
            ));
        }
        let _ = fs::write(csv, s);
    }
    ExitCode::SUCCESS
}
