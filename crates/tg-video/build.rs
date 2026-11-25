//! Build script for `tg-video`: compiles the C FFmpeg decode/encode shims and links against a
//! shared FFmpeg development build pointed to by `FFMPEG_DIR`.
//!
//! ## Expected FFmpeg version
//! - **Supported series:** FFmpeg 7.x or 8.x (GPL shared build; tested with FFmpeg 8.1.x).
//! - **Tested ABI major versions (FFmpeg 8.1.x):**
//!   `libavformat 62.x`, `libavcodec 62.x`, `libavutil 60.x`, `libswscale 9.x`, `libswresample 6.x`.
//! - **Minimum supported ABI major versions (FFmpeg 7.0+):**
//!   `libavformat >= 61`, `libavcodec >= 61`, `libavutil >= 59`, `libswscale >= 8`, `libswresample >= 5`.
//! - **Security note:** Always ship a current patched FFmpeg 7.x/8.x build. Because general
//!   shared builds enable hundreds of demuxers/decoders, `csrc/tg_ffmpeg_shim.c` and
//!   `csrc/tg_ffmpeg_encode.c` enforce `protocol_whitelist`, `format_whitelist`, and
//!   `codec_whitelist` to match Telegram Desktop's minimal FFmpeg build surface.

use std::env;
use std::path::PathBuf;

/// Minimum supported `LIBAVFORMAT_VERSION_MAJOR` (FFmpeg 7.0 = 61; FFmpeg 8.x = 62).
const MIN_LIBAVFORMAT_MAJOR: u32 = 61;
/// Minimum supported `LIBAVCODEC_VERSION_MAJOR` (FFmpeg 7.0 = 61; FFmpeg 8.x = 62).
const MIN_LIBAVCODEC_MAJOR: u32 = 61;

fn parse_version_major(header_path: &std::path::Path, macro_name: &str) -> Option<u32> {
    let text = std::fs::read_to_string(header_path).ok()?;
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("#define") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        if parts.next() == Some(macro_name) {
            return parts.next()?.parse::<u32>().ok();
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-changed=csrc/tg_ffmpeg_shim.h");
    println!("cargo:rerun-if-changed=csrc/tg_ffmpeg_shim.c");
    println!("cargo:rerun-if-changed=csrc/tg_ffmpeg_encode.c");

    let ffmpeg_dir = match env::var("FFMPEG_DIR") {
        Ok(val) if !val.trim().is_empty() => PathBuf::from(val),
        _ => {
            eprintln!();
            eprintln!("error: FFMPEG_DIR environment variable is not set.");
            eprintln!();
            eprintln!(
                "tg-video links against a shared FFmpeg development build (headers + .lib/.dll)."
            );
            eprintln!(
                "On Windows x64, download a BtbN `win64-gpl-shared` build (FFmpeg 7.x or 8.x),"
            );
            eprintln!("extract it to a local directory (e.g. C:\\ffmpeg), and set:");
            eprintln!("  PowerShell: $env:FFMPEG_DIR='C:\\ffmpeg'");
            eprintln!("              $env:Path=\"$env:FFMPEG_DIR\\bin;\" + $env:Path");
            eprintln!("  Git Bash:   export FFMPEG_DIR='/c/ffmpeg'");
            eprintln!("              export PATH=\"$FFMPEG_DIR/bin:$PATH\"");
            eprintln!();
            eprintln!("Expected layout under FFMPEG_DIR:");
            eprintln!("  <FFMPEG_DIR>/include/libavcodec/avcodec.h");
            eprintln!("  <FFMPEG_DIR>/lib/{{avformat,avcodec,avutil,swscale,swresample}}.lib");
            eprintln!(
                "  <FFMPEG_DIR>/bin/{{avformat-*,avcodec-*,avutil-*,swscale-*,swresample-*}}.dll"
            );
            eprintln!();
            eprintln!("See crates/tg-video/README.md for full setup instructions.");
            std::process::exit(1);
        }
    };

    let include_dir = ffmpeg_dir.join("include");
    let lib_dir = ffmpeg_dir.join("lib");
    let avcodec_h = include_dir.join("libavcodec").join("avcodec.h");

    if !avcodec_h.is_file() {
        eprintln!(
            "error: FFMPEG_DIR is set to {:?}, but {:?} was not found.",
            ffmpeg_dir, avcodec_h
        );
        eprintln!(
            "Make sure FFMPEG_DIR points to an FFmpeg *shared development* build containing include/, lib/, and bin/."
        );
        std::process::exit(1);
    }

    if !lib_dir.is_dir() {
        eprintln!(
            "error: FFMPEG_DIR is set to {:?}, but {:?} directory was not found.",
            ffmpeg_dir, lib_dir
        );
        std::process::exit(1);
    }

    if let Some(avformat_major) = parse_version_major(
        &include_dir.join("libavformat").join("version_major.h"),
        "LIBAVFORMAT_VERSION_MAJOR",
    ) && avformat_major < MIN_LIBAVFORMAT_MAJOR
    {
        eprintln!(
            "error: FFmpeg libavformat major version {avformat_major} is older than the minimum supported version {MIN_LIBAVFORMAT_MAJOR} (FFmpeg 7.0+ / 8.x required)."
        );
        std::process::exit(1);
    }
    if let Some(avcodec_major) = parse_version_major(
        &include_dir.join("libavcodec").join("version_major.h"),
        "LIBAVCODEC_VERSION_MAJOR",
    ) && avcodec_major < MIN_LIBAVCODEC_MAJOR
    {
        eprintln!(
            "error: FFmpeg libavcodec major version {avcodec_major} is older than the minimum supported version {MIN_LIBAVCODEC_MAJOR} (FFmpeg 7.0+ / 8.x required)."
        );
        std::process::exit(1);
    }

    cc::Build::new()
        .file("csrc/tg_ffmpeg_shim.c")
        .include("csrc")
        .include(&include_dir)
        .warnings(false)
        .compile("tg_ffmpeg_shim");

    cc::Build::new()
        .file("csrc/tg_ffmpeg_encode.c")
        .include("csrc")
        .include(&include_dir)
        .warnings(false)
        .compile("tg_ffmpeg_encode");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=avformat");
    println!("cargo:rustc-link-lib=avcodec");
    println!("cargo:rustc-link-lib=avutil");
    println!("cargo:rustc-link-lib=swscale");
    println!("cargo:rustc-link-lib=swresample");

    let bin_dir = ffmpeg_dir.join("bin");
    println!(
        "cargo:rustc-env=TG_VIDEO_FFMPEG_BIN_DIR={}",
        bin_dir.display()
    );
}
