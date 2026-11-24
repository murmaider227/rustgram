//! Decodes N video frames from a media file into PNG images using `tg-video`.
//!
//! Usage:
//!   cargo run -p tg-video --example play -- <file.mp4> <out_dir> [max_frames]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use tg_video::{
    FfmpegStreamDecoder, OutputPixelFormat, PlaybackMode, PlaybackOptions, StreamingPlayer,
    ThumbnailEncoding, encode_thumbnail,
};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <input_video> <out_dir> [max_frames=10]", args[0]);
        return ExitCode::from(2);
    }

    let input_path = PathBuf::from(&args[1]);
    let out_dir = PathBuf::from(&args[2]);
    let max_frames: usize = args
        .get(3)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10);

    if let Err(err) = fs::create_dir_all(&out_dir) {
        eprintln!("Failed to create output directory {:?}: {err}", out_dir);
        return ExitCode::FAILURE;
    }

    let options = PlaybackOptions {
        mode: PlaybackMode::Video,
        output_format: OutputPixelFormat::Rgba,
        apply_rotation: true,
        ..PlaybackOptions::default()
    };

    let decoder = match FfmpegStreamDecoder::from_file(&input_path, options.clone()) {
        Ok(d) => d,
        Err(err) => {
            eprintln!("Failed to open {:?}: {err}", input_path);
            return ExitCode::FAILURE;
        }
    };

    let mut player = match StreamingPlayer::new(decoder, options) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("Failed to initialize player: {err}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(video) = &player.info().video {
        println!(
            "Opened {:?}: {}x{} (codec={}, fps={:.2}, duration={}ms, rotation={}deg, alpha={})",
            input_path,
            video.size.width,
            video.size.height,
            video.codec_name,
            video.fps,
            video.state.duration_ms,
            video.rotation.degrees(),
            video.has_alpha,
        );
    }

    let mut written = 0usize;
    while written < max_frames {
        match player.next_video_frame() {
            Ok(Some(frame)) => {
                let png = match encode_thumbnail(&frame, ThumbnailEncoding::Png) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        eprintln!("Failed to encode frame {written}: {err}");
                        return ExitCode::FAILURE;
                    }
                };
                let out_path = out_dir.join(format!("frame_{written:04}.png"));
                if let Err(err) = fs::write(&out_path, png) {
                    eprintln!("Failed to write {:?}: {err}", out_path);
                    return ExitCode::FAILURE;
                }
                println!(
                    "Wrote {} ({}x{}, pts={}ms, alpha={})",
                    out_path.display(),
                    frame.width,
                    frame.height,
                    frame.pts_ms,
                    frame.has_alpha,
                );
                written += 1;
            }
            Ok(None) => break,
            Err(err) => {
                eprintln!("Decode error at frame {written}: {err}");
                return ExitCode::FAILURE;
            }
        }
    }

    println!("Decoded {written} frame(s) to {}", out_dir.display());
    ExitCode::SUCCESS
}
