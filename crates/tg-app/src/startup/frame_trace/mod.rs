//! Env-gated frame-time trace and scripted input for `tools/bench/frame_bench.py` (TGB-2893).
//!
//! Off unless `RUSTGRAM_FRAME_TRACE` is set. When off, every hook is one relaxed atomic load and
//! nothing is allocated, sampled or written.
//!
//! - `RUSTGRAM_FRAME_TRACE=1`: one line per frame in `RUSTGRAM_FRAME_TRACE_OUT` (default
//!   `frame_trace.tsv` in the current directory):
//!   `F <index> <start_us> <ui_us> <cpu_us> <section>`, where `ui_us` is the time spent in
//!   `App::ui` and `cpu_us` is eframe's whole-frame CPU time (input, UI, tessellation, GL
//!   painting; the vsync wait in `swap_buffers` is excluded). Script events are `E <t_us> <text>`.
//!   Lines are written whole (see [`out`]); `trace_start` names the process id.
//! - `RUSTGRAM_FRAME_TRACE=sample`: also samples the UI thread's call stack every millisecond while
//!   a script section runs. Raw return addresses go to `<out>.stacks`, the loaded modules to
//!   `<out>.modules`; the benchmark symbolizes them offline against the PDB.
//! - `RUSTGRAM_FRAME_SCRIPT=<file>`: feeds the input script described in [`script`] into egui
//!   (wheel, pointer, keys), so runs repeat exactly and never touch the OS cursor or keyboard.

mod out;
mod sampler;
mod script;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);
static STATE: OnceLock<Mutex<TraceState>> = OnceLock::new();

struct PendingFrame {
    index: u64,
    start_us: u64,
    ui_us: u64,
    section: String,
}

struct TraceState {
    epoch: Instant,
    out: out::TraceOut,
    frame_index: u64,
    pending: Option<PendingFrame>,
    section: String,
    sampling_active: Arc<AtomicBool>,
    sampler: Option<sampler::Sampler>,
    script: Option<script::Script>,
    commands: Vec<egui::ViewportCommand>,
    finished: bool,
}

impl TraceState {
    fn now_us(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    fn event(&mut self, text: &str) {
        let t = self.now_us();
        self.out.line(format_args!("E\t{t}\t{text}"));
    }

    fn set_section(&mut self, name: &str) {
        self.section = name.to_owned();
        self.sampling_active
            .store(!self.section.is_empty(), Ordering::Relaxed);
        let text = if name.is_empty() {
            "section_end".to_owned()
        } else {
            format!("section_start {name}")
        };
        self.event(&text);
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.sampling_active.store(false, Ordering::Relaxed);
        if let Some(frame) = self.pending.take() {
            write_frame(&mut self.out, &frame, None);
        }
        self.event("trace_end");
        self.out.flush();
        if let Some(sampler) = self.sampler.take() {
            sampler.stop();
        }
    }
}

fn write_frame(out: &mut out::TraceOut, frame: &PendingFrame, cpu_us: Option<u64>) {
    let cpu = cpu_us.map_or_else(|| "-".to_owned(), |v| v.to_string());
    out.line(format_args!(
        "F\t{}\t{}\t{}\t{}\t{}",
        frame.index, frame.start_us, frame.ui_us, cpu, frame.section
    ));
}

fn env_flag() -> Option<String> {
    std::env::var("RUSTGRAM_FRAME_TRACE")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v != "0" && v != "false")
}

/// Starts the trace when `RUSTGRAM_FRAME_TRACE` is set. Call once from `main`, on the UI thread
/// (the stack sampler targets the calling thread).
pub fn init_from_env() {
    let Some(mode) = env_flag() else {
        return;
    };
    let out_path = std::env::var_os("RUSTGRAM_FRAME_TRACE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("frame_trace.tsv"));
    let out = match out::TraceOut::create(&out_path) {
        Ok(out) => out,
        Err(err) => {
            tracing::warn!(%err, "frame trace: cannot create the output file");
            return;
        }
    };
    let epoch = Instant::now();
    let sampling_active = Arc::new(AtomicBool::new(false));
    let sampler = if mode == "sample" {
        match sampler::Sampler::start(&out_path, epoch, Arc::clone(&sampling_active)) {
            Ok(s) => Some(s),
            Err(err) => {
                tracing::warn!(%err, "frame trace: stack sampler unavailable");
                None
            }
        }
    } else {
        None
    };
    let script = std::env::var_os("RUSTGRAM_FRAME_SCRIPT").and_then(|path| {
        match std::fs::read_to_string(&path) {
            Ok(text) => match script::Script::parse(&text) {
                Ok(s) => Some(s),
                Err(err) => {
                    tracing::warn!(%err, "frame trace: bad script");
                    None
                }
            },
            Err(err) => {
                tracing::warn!(%err, "frame trace: cannot read the script");
                None
            }
        }
    });
    let mut state = TraceState {
        epoch,
        out,
        frame_index: 0,
        pending: None,
        section: String::new(),
        sampling_active,
        sampler,
        script,
        commands: Vec::new(),
        finished: false,
    };
    state.event(&format!(
        "trace_start mode={mode} pid={}",
        std::process::id()
    ));
    if STATE.set(Mutex::new(state)).is_ok() {
        ENABLED.store(true, Ordering::Release);
    }
}

/// Whether the frame trace is running.
#[inline]
#[must_use]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn with_state<R>(f: impl FnOnce(&mut TraceState) -> R) -> Option<R> {
    let state = STATE.get()?;
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Some(f(&mut guard))
}

/// Measures one `App::ui` call; dropping it records the UI time of the frame.
pub struct FrameGuard {
    started: Instant,
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        let ui_us = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        with_state(|s| {
            if let Some(frame) = s.pending.as_mut() {
                frame.ui_us = ui_us;
            }
        });
    }
}

/// Call first thing in `App::ui` with `frame.info().cpu_usage` (the previous frame's CPU time).
#[must_use]
pub fn begin_frame(ctx: &egui::Context, prev_cpu_seconds: Option<f32>) -> Option<FrameGuard> {
    if !is_enabled() {
        return None;
    }
    let started = Instant::now();
    with_state(|s| {
        let start_us = s.now_us();
        if let Some(prev) = s.pending.take() {
            let cpu_us = prev_cpu_seconds.map(|c| (f64::from(c) * 1_000_000.0).round() as u64);
            write_frame(&mut s.out, &prev, cpu_us);
        }
        s.pending = Some(PendingFrame {
            index: s.frame_index,
            start_us,
            ui_us: 0,
            section: s.section.clone(),
        });
        s.frame_index += 1;
        for cmd in s.commands.drain(..) {
            ctx.send_viewport_cmd(cmd);
        }
        if s.script.is_some() && !s.finished {
            ctx.request_repaint();
        }
    });
    Some(FrameGuard { started })
}

/// Feeds the input script into this frame's raw input (`eframe::App::raw_input_hook`).
pub fn raw_input_hook(_ctx: &egui::Context, raw_input: &mut egui::RawInput) {
    if !is_enabled() {
        return;
    }
    with_state(|s| {
        let Some(mut script) = s.script.take() else {
            return;
        };
        let now = Instant::now();
        let mut out = script::StepOutput::default();
        script.advance(now, raw_input, &mut out);
        for name in out.sections {
            s.set_section(&name);
        }
        for text in out.events {
            s.event(&text);
        }
        s.commands.extend(out.commands);
        if out.quit {
            s.finish();
            s.commands.push(egui::ViewportCommand::Close);
        }
        s.script = Some(script);
    });
}

/// Flushes the trace and stops the sampler (`eframe::App::on_exit`).
pub fn shutdown() {
    if !is_enabled() {
        return;
    }
    with_state(TraceState::finish);
}
