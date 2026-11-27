//! CLI example rendering a single frame of a Lottie `.json` or `.tgs` animation to PNG.
//!
//! Usage:
//! ```text
//! cargo run -p tg-lottie --example lottie_render -- in.json out.png frame [--size WxH]
//! ```

use std::env;
use std::fs;
use std::process;

use tg_lottie::{FrameBuffer, Instance, Size};

fn parse_size(arg: &str) -> Option<Size> {
    let (w, h) = arg.split_once('x').or_else(|| arg.split_once('X'))?;
    let width = w.parse::<u32>().ok()?;
    let height = h.parse::<u32>().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(Size::new(width, height))
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "Usage: {} <in.json|in.tgs> <out.png> <frame> [--size WxH]",
            args[0]
        );
        process::exit(2);
    }

    let in_path = &args[1];
    let out_path = &args[2];
    let frame: f64 = match args[3].parse::<f64>() {
        Ok(f) if f.is_finite() => f,
        _ => {
            eprintln!("Invalid frame number: {}", args[3]);
            process::exit(2);
        }
    };

    let mut size_override = None;
    let mut i = 4;
    while i < args.len() {
        if args[i] == "--size" && i + 1 < args.len() {
            size_override = parse_size(&args[i + 1]);
            if size_override.is_none() {
                eprintln!("Invalid --size argument: {}", args[i + 1]);
                process::exit(2);
            }
            i += 2;
        } else {
            eprintln!("Unknown argument: {}", args[i]);
            process::exit(2);
        }
    }

    let bytes = match fs::read(in_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to read {in_path}: {e}");
            process::exit(1);
        }
    };

    let instance = match Instance::from_bytes(&bytes, None) {
        Ok(inst) => inst,
        Err(e) => {
            eprintln!("Failed to load Lottie animation: {e}");
            process::exit(1);
        }
    };

    let target_size = size_override.unwrap_or_else(|| instance.size());
    let mut buf = FrameBuffer::new(target_size);
    instance.render_at_frame(&mut buf, frame);

    if let Err(e) = buf.save_png(out_path) {
        eprintln!("Failed to write {out_path}: {e}");
        process::exit(1);
    }
}
