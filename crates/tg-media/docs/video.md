# Video & Audio Playback Integration Plan (`tg-media`)

This document outlines how to integrate FFmpeg into `tg-media` for video playback, animated GIF/MP4/WebM sticker decoding, voice/audio decoding, and streaming playback — mirroring Telegram Desktop's architecture (`Telegram/SourceFiles/ffmpeg/*` and `Telegram/SourceFiles/media/streaming/*`).

## 1. FFmpeg Distribution Requirements

A static/exe-only FFmpeg installation (`ffmpeg.exe`, `ffplay.exe`, `ffprobe.exe` without `include/`, `lib/`, or `bin/*.dll`) cannot be linked via FFI (`ffmpeg-next` / `rsmpeg` / raw `libav*` bindings) as-is because it lacks headers and `.lib`/`.dll` files. To enable in-process FFmpeg decoding on `x86_64-pc-windows-msvc`, we need a **shared dev build** (e.g., `gyan.dev` `ffmpeg-release-full-shared` or `BtbN/FFmpeg-Builds` `win64-gpl-shared` matching FFmpeg 7.x or 8.x, which ships `include/`, `lib/*.lib` MSVC import libraries, and `bin/*.dll`, with codecs H.264, HEVC, VP8/VP9, AV1, Opus, Vorbis, AAC, MP3, WebP, GIF, and `d3d11va`/`dxva2`/`nvdec`).

## 2. Two-Phase Integration Strategy

### Phase A (Immediate / Zero-FFI-Setup Fallback via `ffmpeg` on `PATH`)
Useful during early bring-up before shipping shared DLLs:
- **Metadata & thumbnail extraction:** Spawn `ffprobe -v quiet -print_format json -show_format -show_streams` on cached files to extract duration, rotation metadata, dimensions, and codecs; extract first-frame / poster thumbnails via `ffmpeg -i <file> -frames:v 1 -f image2pipe -vcodec png pipe:1`.
- **Limitations:** Cannot drive Telegram's custom streaming `AVIOContext` or frame-accurate seekable zero-copy playback for inline GIFs/MP4s/round videos/WebM stickers without unacceptable process-spawn overhead.

### Phase B (Production In-Process Integration — Mirrors tdesktop)
Ship FFmpeg 7.x/8.x LGPL/GPLv3 shared DLLs (`avformat-*.dll`, `avcodec-*.dll`, `avutil-*.dll`, `swscale-*.dll`, `swresample-*.dll`) alongside `rustgram.exe` and bind them in `tg-media` behind an optional Cargo feature `ffmpeg` (disabled by default so pure-Rust builds/tests remain fast and dependency-free).

#### Crate Choice for MSVC
- Prefer thin `libloading`-based dynamic loading or a minimal C shim pointed at `FFMPEG_DIR=<path-to-ffmpeg-shared>` (with `include/` and `lib/` MSVC `.lib` import libs).
- Avoid crates requiring CMake or `pkg-config` on Windows (`docs/AGENT_BRIEF.md` notes CMake is not on `PATH`). Using `bindgen` with `LIBCLANG_PATH` or hand-written minimal `extern "C"` declarations against FFmpeg 7.x ABI avoids CMake completely and keeps build times tiny.

## 3. Porting tdesktop's FFmpeg Architecture

Map each tdesktop file in `Telegram/SourceFiles/ffmpeg/` directly into `tg-media::video`:

| tdesktop Reference | Planned `tg-media` Module | Responsibility |
|---|---|---|
| `ffmpeg/ffmpeg_utility.h/.cpp` | `video/ffi.rs`, `video/context.rs` | Safe RAII wrappers (`Packet`, `Frame`, `IoContext`, `FormatContext`, `CodecContext`, `SwsScaler`, `SwrResampler`), `AvError`, PTS/timebase conversions (`pts_to_time_ms`, `pts_to_time_ceil_ms`, `time_to_pts`), rotation metadata (`displaymatrix`), SAR aspect correction, 64-byte row alignment (`kAlignImageBy = 64`). |
| `ffmpeg/ffmpeg_bytes_io_wrap.h` | `video/io.rs` | In-memory byte-slice `AVIOContext` (`read_packet`, `seek`) for small cached animations/WebM stickers. |
| `media/streaming/media_streaming_file.cpp` | `video/stream_io.rs` | Custom `AVIOContext` backed by `tg_media::transfer::streamed::StreamedFileDownloaderState` + `StreamingPriorityQueue` (128 KiB parts, on-demand random access for MP4 `moov` atom at end of file and seeking). |
| `ffmpeg/ffmpeg_frame_generator.h/.cpp` | `video/frame_generator.rs` | Pull-based frame decoder for animated WebM stickers, GIF/MP4 loops, and video avatars (`count()`, `rate()`, `render_next()`, `jump_to_start()`). |
| `media/streaming/media_streaming_player.cpp` | `video/player.rs` | Dedicated decode thread (video + audio streams), bounded frame ring buffer, A/V clock synchronization (audio master clock via `cpal`/`wasapi` or monotonic wall clock for muted loops), pause/resume/seek/loop/speed control. |

## 4. Critical Security & Correctness Invariants from tdesktop

1. **Custom-IO protocol restriction (`FFmpeg::RestrictToCustomIO`):**
   Before `avformat_open_input()`, always set `protocol_whitelist` to `""` (empty) and disable external resource resolution on the `AVFormatContext`. All bytes MUST come exclusively from `tg-media`'s custom `AVIOContext` callbacks (`kAVBlockSize = 4096`) so malicious playlist/DASH/HLS payloads cannot leak the user's IP or read arbitrary local files.
2. **Maximum frame area & memory safety (`MaxPixelsForAreaLimit`):**
   Cap decoded frame pixel area (matching `Images::kReadMaxArea` / `CodecDescriptor::videoMaxArea`) and enforce `AVCodecContext::max_pixels` before opening the decoder to defend against decompression bombs.
3. **Alpha channel handling (WebM VP9/VP8 stickers with alpha):**
   Detect `AV_PIX_FMT_YUVA420P` (and WebM `alpha_mode=1` side data), decode using `libvpx`/`libvpx-vp9` (not hardware decoders that drop alpha), and convert to premultiplied RGBA via `libswscale`.
4. **Hardware acceleration on Windows:**
   Support opt-in `d3d11va` decoding for H.264/HEVC/VP9/AV1 with automatic fallback to software decoding on format/driver failure or when alpha is present.
5. **GUI integration with `egui` / `wgpu` (`tg-app`):**
   Decode frames off the UI thread into reusable `Vec<u8>` RGBA buffers (or `wgpu` textures uploaded via `egui_wgpu`), returning `VideoFrame { pts_ms, duration_ms, width, height, rgba: Arc<[u8]> }` over a bounded `tokio::sync::watch` / channel so the UI thread never blocks on decoding or scaling.

## 5. Audio Output Pairing

- Decode audio streams (`Opus` voice messages, `AAC`/`MP3`/`FLAC` audio files, video soundtracks) through `libavcodec` + `libswresample` into interleaved `f32` PCM (48 kHz stereo).
- Feed decoded PCM chunks into an audio output backend (`cpal` on Windows WASAPI) while exposing `playback_position_ms()` for:
  - Video A/V sync (`VideoPlayer`)
  - Voice message waveform progress bars (`tg_media::audio::VoiceDuration::active_bars`)
  - Waveform generation for locally recorded/imported audio (`tg_media::audio::convert_waveform_from_pcm_i16`).
