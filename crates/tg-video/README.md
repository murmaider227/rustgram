# `tg-video`

Rust port of Telegram Desktop's video decoding, streaming reader/player, clip/sticker reader, and video thumbnail subsystem (`Telegram/SourceFiles/media/streaming/`, `Telegram/SourceFiles/ffmpeg/`, and `Telegram/SourceFiles/media/clip/`).

## FFmpeg Setup (Windows x64)

`tg-video` uses a thin bindgen-free C shim (`csrc/tg_ffmpeg_shim.c`) compiled with MSVC via `cc` and linked against a shared FFmpeg development build (FFmpeg 7.x or 8.x, GPL-3.0). No CMake or `libclang` installation is required.

1. Download the Windows x64 shared GPL build that CI and releases use, pinned in `.github/actions/setup-ffmpeg/action.yml`: BtbN [FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) release `autobuild-2026-11-27-12-42`, file `ffmpeg-N-129333-g7fdb4c2341-win64-gpl-shared.zip` (FFmpeg master `N-129333-g7fdb4c2341`, dav1d 1.5.5). Verify its SHA-256 against the value in that action. Release builds of FFmpeg 8.1 (`ffmpeg-n8.1-*`) still link, but they lack the October 2026 MOV and AAC-USAC memory-safety fixes (TGB-3146), so do not ship them.
2. Extract it to a local directory (e.g. `C:\ffmpeg`) so it has `include/`, `lib/`, and `bin/`.
3. Set `FFMPEG_DIR` before building, and add `<FFMPEG_DIR>\bin` to `PATH` when running binaries or tests:

### PowerShell
```powershell
$env:FFMPEG_DIR = 'C:\ffmpeg'
$env:Path = "$env:FFMPEG_DIR\bin;" + $env:Path
```

### Git Bash
```bash
export FFMPEG_DIR='/c/ffmpeg'
export PATH="$FFMPEG_DIR/bin:$PATH"
```

If `FFMPEG_DIR` is not set (or points to an exe-only FFmpeg build without `include/` and `lib/`), `build.rs` fails with an actionable error message.

## Running Tests and Example

```powershell
cargo test -p tg-video
cargo clippy -p tg-video --all-targets -- -D warnings
cargo fmt --check

# Decode N frames (default 10) from a video file into PNGs:
cargo run -p tg-video --example play -- path\to\video.mp4 out_dir 10
```

Integration tests that generate temporary H.264 MP4, rotated MP4, and VP9 WebM (with alpha) fixtures look for `FFMPEG_EXE`, `<FFMPEG_DIR>/bin/ffmpeg(.exe)`, or `ffmpeg` on `PATH` and skip gracefully if no `ffmpeg` executable is available.

## Architecture

- `ffmpeg` (`ffmpeg_utility.cpp`, `ffmpeg_frame_generator.cpp`): custom AVIO with `protocol_whitelist=""` security hardening, Windows hardware decoding selection (`D3D11VA` -> `DXVA2` -> `CUDA` -> software), `libvpx-vp9` alpha decoding, `swscale` RGBA/BGRA conversion, YUV420P/NV12 extraction, `swresample` audio PCM conversion, display-matrix / tag rotation, aspect-ratio correction, and `FrameGenerator`.
- `streaming::loader` (`media_streaming_loader*.cpp`): `RangeLoader` trait (implemented by `tg-api` via `upload.getFile`), `LocalFileLoader`, `MemoryRangeLoader`, `PriorityQueue`, and `SpeedEstimator`.
- `streaming::reader` (`media_streaming_reader.cpp`): sliced/partial file reader with 128 KB parts, 1 MB slices (configurable to tdesktop's 8 MB slice mode), 1 MB prefetch ahead, seek priority reset, LRU slice cache, complex/contiguous cache serialization, and `NotStreamable` header guard.
- `streaming::clock` (`media_streaming_video_track.cpp`): A/V sync clock (`SyncClock`), playback speed (0.5x–2.5x), external audio sync, and `FrameQueue` with late-frame dropping.
- `streaming::decoder` & `streaming::player` (`media_streaming_file.cpp`, `media_streaming_video_track.cpp`, `media_streaming_audio_track.cpp`, `media_streaming_player.cpp`): `Decoder` trait, `FfmpegStreamDecoder`, and `StreamingPlayer` with seeking, looping, speed, volume, and buffering state.
- `streaming::document` (`media_streaming_document.cpp`, `media_streaming_instance.cpp`): `StreamingDocument` and `StreamingInstance` with multi-instance priority arbitration and adaptive quality switching.
- `clip` (`media_clip_reader.cpp`, `media_clip_ffmpeg.cpp`, `media_clip_check_streaming.cpp`, `media_streaming_round_preview.cpp`): `ClipReader` for GIF/MP4 animations, round videos, and WebM/VP9 stickers with alpha; MP4 `moov`-within-128 KB streaming check; and antialiased circle / rounded-rectangle mask helpers.
- `thumbnail`: first-frame / seeked thumbnail extraction with rotation, aspect ratio correction, resize, opaque compositing, and PNG/JPEG encoding.
