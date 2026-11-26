//! Input script for the frame benchmark. One command per line, `#` starts a comment.
//! Coordinates are logical points inside the window.
//!
//! ```text
//! size <w> <h>                 resize the window's inner area
//! wait <ms>                    keep drawing frames for <ms>
//! section <name>               label the following frames (and sample stacks in `sample` mode)
//! end                          stop labelling
//! move <x> <y>                 move the pointer (repeated every frame so the real cursor cannot win)
//! click <x> <y>                press on one frame, release on the next
//! rclick <x> <y>               the same with the secondary button (context menus)
//! text <text>                  type <text> (one text event, the rest of the line)
//! wheel <up|down> <lines_per_s> <ms>  wheel notches at the pointer for <ms>
//! key <name>                   press and release a key (egui key name, e.g. Escape)
//! shot <path>                  save a 24-bit BMP of the window (the next step waits two
//!                              frames, so its input is not in the picture)
//! log <text>                   write an event line to the trace
//! quit                         flush the trace and close the window
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use egui::{Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, RawInput, Vec2};

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Size(f32, f32),
    Wait(u64),
    Section(String),
    End,
    Move(f32, f32),
    Click(f32, f32),
    RightClick(f32, f32),
    Text(String),
    Wheel { dir: f32, rate: f32, ms: u64 },
    Key(egui::Key),
    Shot(PathBuf),
    Log(String),
    Quit,
}

/// What one frame of the script produced besides raw input events.
#[derive(Default)]
pub struct StepOutput {
    pub sections: Vec<String>,
    pub events: Vec<String>,
    pub commands: Vec<egui::ViewportCommand>,
    pub quit: bool,
}

/// A parsed script and its progress.
pub struct Script {
    steps: Vec<Step>,
    index: usize,
    step_started: Option<Instant>,
    notches_sent: u32,
    release_at: Option<(Pos2, PointerButton)>,
    pointer: Option<Pos2>,
    /// Frames without new steps after a `shot`: the window is captured after the frame that
    /// asked for it, and input sent meanwhile would be in the picture.
    hold_frames: u8,
}

fn parse_f32(s: Option<&str>, line: usize) -> Result<f32, String> {
    s.and_then(|v| v.parse::<f32>().ok())
        .ok_or_else(|| format!("line {line}: expected a number"))
}

fn parse_u64(s: Option<&str>, line: usize) -> Result<u64, String> {
    s.and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| format!("line {line}: expected an integer"))
}

impl Script {
    /// Parses the script text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut steps = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line_no = i + 1;
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let cmd = parts.next().unwrap_or("");
            let rest = || line[cmd.len()..].trim().to_owned();
            let step = match cmd {
                "size" => Step::Size(
                    parse_f32(parts.next(), line_no)?,
                    parse_f32(parts.next(), line_no)?,
                ),
                "wait" => Step::Wait(parse_u64(parts.next(), line_no)?),
                "section" => Step::Section(rest()),
                "end" => Step::End,
                "move" => Step::Move(
                    parse_f32(parts.next(), line_no)?,
                    parse_f32(parts.next(), line_no)?,
                ),
                "click" => Step::Click(
                    parse_f32(parts.next(), line_no)?,
                    parse_f32(parts.next(), line_no)?,
                ),
                "rclick" => Step::RightClick(
                    parse_f32(parts.next(), line_no)?,
                    parse_f32(parts.next(), line_no)?,
                ),
                "text" => Step::Text(rest()),
                "wheel" => {
                    let dir = match parts.next() {
                        Some("up") => 1.0,
                        Some("down") => -1.0,
                        _ => return Err(format!("line {line_no}: wheel needs up or down")),
                    };
                    Step::Wheel {
                        dir,
                        rate: parse_f32(parts.next(), line_no)?,
                        ms: parse_u64(parts.next(), line_no)?,
                    }
                }
                "key" => {
                    let name = parts.next().unwrap_or("");
                    Step::Key(
                        egui::Key::from_name(name)
                            .ok_or_else(|| format!("line {line_no}: unknown key {name}"))?,
                    )
                }
                "shot" => Step::Shot(PathBuf::from(rest())),
                "log" => Step::Log(rest()),
                "quit" => Step::Quit,
                other => return Err(format!("line {line_no}: unknown command {other}")),
            };
            steps.push(step);
        }
        Ok(Self {
            steps,
            index: 0,
            step_started: None,
            notches_sent: 0,
            release_at: None,
            hold_frames: 0,
            pointer: None,
        })
    }

    fn next_step(&mut self) {
        self.index += 1;
        self.step_started = None;
        self.notches_sent = 0;
    }

    /// Runs every step that is due on this frame and appends its input to `raw`.
    pub fn advance(&mut self, now: Instant, raw: &mut RawInput, out: &mut StepOutput) {
        if let Some(pos) = self.pointer {
            raw.events.push(Event::PointerMoved(pos));
        }
        if let Some((pos, button)) = self.release_at.take() {
            raw.events.push(pointer_button(pos, button, false));
            // The release frame ends the click; the next step starts on the following frame.
            return;
        }
        if self.hold_frames > 0 {
            self.hold_frames -= 1;
            return;
        }
        while let Some(step) = self.steps.get(self.index).cloned() {
            let started = *self.step_started.get_or_insert(now);
            let elapsed_ms = now.saturating_duration_since(started).as_secs_f64() * 1000.0;
            match step {
                Step::Size(w, h) => {
                    out.commands
                        .push(egui::ViewportCommand::InnerSize(Vec2::new(w, h)));
                }
                Step::Wait(ms) => {
                    if elapsed_ms < ms as f64 {
                        return;
                    }
                }
                Step::Section(name) => out.sections.push(name),
                Step::End => out.sections.push(String::new()),
                Step::Move(x, y) => {
                    let pos = Pos2::new(x, y);
                    self.pointer = Some(pos);
                    raw.events.push(Event::PointerMoved(pos));
                }
                Step::Click(x, y) | Step::RightClick(x, y) => {
                    let button = if matches!(step, Step::RightClick(..)) {
                        PointerButton::Secondary
                    } else {
                        PointerButton::Primary
                    };
                    let pos = Pos2::new(x, y);
                    self.pointer = Some(pos);
                    raw.events.push(Event::PointerMoved(pos));
                    raw.events.push(pointer_button(pos, button, true));
                    self.release_at = Some((pos, button));
                    self.next_step();
                    return;
                }
                Step::Text(text) => raw.events.push(Event::Text(text)),
                Step::Wheel { dir, rate, ms } => {
                    let window = elapsed_ms.min(ms as f64);
                    let due = (window / 1000.0 * f64::from(rate)).floor() as u32 + 1;
                    let n = due.saturating_sub(self.notches_sent);
                    if n > 0 {
                        self.notches_sent += n;
                        raw.events.push(Event::MouseWheel {
                            unit: MouseWheelUnit::Line,
                            delta: Vec2::new(0.0, dir * n as f32),
                            phase: egui::TouchPhase::Move,
                            source: egui::MouseWheelSource::Wheel,
                            modifiers: Modifiers::NONE,
                        });
                    }
                    if elapsed_ms < ms as f64 {
                        return;
                    }
                }
                Step::Key(key) => {
                    for pressed in [true, false] {
                        raw.events.push(Event::Key {
                            key,
                            physical_key: Some(key),
                            pressed,
                            repeat: false,
                            modifiers: Modifiers::NONE,
                        });
                    }
                }
                Step::Shot(path) => {
                    out.commands.push(egui::ViewportCommand::Screenshot(
                        egui::ScreenshotCallback::new(move |image| {
                            let _ = write_bmp(&path, &image);
                        }),
                    ));
                    self.hold_frames = 2;
                    self.next_step();
                    return;
                }
                Step::Log(text) => out.events.push(text),
                Step::Quit => {
                    out.quit = true;
                    self.next_step();
                    return;
                }
            }
            self.next_step();
        }
    }
}

fn pointer_button(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// Writes a bottom-up 24-bit BMP (no extra image crate in the binary).
fn write_bmp(path: &std::path::Path, image: &Arc<egui::ColorImage>) -> std::io::Result<()> {
    let [w, h] = image.size;
    let row = (w * 3).div_ceil(4) * 4;
    let data_len = row * h;
    let file_len = 54 + data_len;
    let mut buf = Vec::with_capacity(file_len);
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&(file_len as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&54u32.to_le_bytes());
    buf.extend_from_slice(&40u32.to_le_bytes());
    buf.extend_from_slice(&(w as i32).to_le_bytes());
    buf.extend_from_slice(&(h as i32).to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&24u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&(data_len as u32).to_le_bytes());
    buf.extend_from_slice(&[0u8; 16]);
    for y in (0..h).rev() {
        let start = buf.len();
        for x in 0..w {
            let c = image.pixels[y * w + x];
            buf.extend_from_slice(&[c.b(), c.g(), c.r()]);
        }
        buf.resize(start + row, 0);
    }
    std::fs::write(path, buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_command() {
        let s = Script::parse(
            "size 1200 800\nwait 10 # c\nsection a b\nmove 1 2\nclick 3 4\nwheel down 20 500\n\
             key Escape\nshot x.bmp\nlog hi\nend\nquit\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.steps.len(), 11);
        assert_eq!(s.steps[2], Step::Section("a b".to_owned()));
        assert!(Script::parse("wheel left 1 1").is_err());
        assert!(Script::parse("bogus").is_err());
    }

    #[test]
    fn wheel_spreads_notches_over_time() {
        let mut s = Script::parse("move 10 10\nwheel down 10 1000\nlog done")
            .unwrap_or_else(|e| panic!("{e}"));
        let t0 = Instant::now();
        let mut total = 0.0;
        let mut out = StepOutput::default();
        for ms in [0u64, 250, 600, 1000, 1100] {
            let mut raw = RawInput::default();
            s.advance(
                t0 + std::time::Duration::from_millis(ms),
                &mut raw,
                &mut out,
            );
            for e in raw.events {
                if let Event::MouseWheel { delta, .. } = e {
                    total += delta.y;
                }
            }
        }
        assert_eq!(total, -11.0);
        assert_eq!(out.events, vec!["done".to_owned()]);
    }

    /// TGB-2948: a `click` right after a `shot` was sent in the frame the window was captured
    /// after, so the picture showed the click's frame.
    #[test]
    fn a_shot_holds_the_next_step_for_two_frames() {
        let mut s = Script::parse(
            "shot x.bmp
click 5 6",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let t0 = Instant::now();
        let mut out = StepOutput::default();
        let pressed = |raw: &RawInput| {
            raw.events
                .iter()
                .any(|e| matches!(e, Event::PointerButton { pressed: true, .. }))
        };
        for frame in 0..3 {
            let mut raw = RawInput::default();
            s.advance(t0, &mut raw, &mut out);
            assert!(!pressed(&raw), "frame {frame} sent the click");
        }
        assert_eq!(out.commands.len(), 1);
        let mut raw = RawInput::default();
        s.advance(t0, &mut raw, &mut out);
        assert!(pressed(&raw));
    }

    #[test]
    fn click_releases_on_next_frame() {
        let mut s = Script::parse("click 5 6\nlog after").unwrap_or_else(|e| panic!("{e}"));
        let t0 = Instant::now();
        let mut out = StepOutput::default();
        let mut raw = RawInput::default();
        s.advance(t0, &mut raw, &mut out);
        assert!(
            raw.events
                .iter()
                .any(|e| matches!(e, Event::PointerButton { pressed: true, .. }))
        );
        let mut raw = RawInput::default();
        s.advance(t0, &mut raw, &mut out);
        assert!(
            raw.events
                .iter()
                .any(|e| matches!(e, Event::PointerButton { pressed: false, .. }))
        );
        assert!(out.events.is_empty());
        let mut raw = RawInput::default();
        s.advance(t0, &mut raw, &mut out);
        assert_eq!(out.events, vec!["after".to_owned()]);
    }
}
