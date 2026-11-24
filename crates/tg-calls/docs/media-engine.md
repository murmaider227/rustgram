# Rustgram Media Engine Evaluation & Concrete Plan

This document evaluates options for implementing the real-time media engine underneath
`tg-calls` (1:1 voice/video calls and group calls / livestreams) in Rust on Windows x64
first (`x86_64-pc-windows-msvc`), without requiring CMake or C++ `tgcalls` wrapping.

---

## 1. Requirements From the Telegram Calls Ecosystem

A full Telegram calls media engine must cover two distinct media pipelines:

1. **1:1 Calls (`ControllerDescriptor` from `tg-calls`)**:
   - **Transport**: ICE (STUN + TURN + Telegram UDP/TCP reflectors with 16-byte `peer_tag`),
     DTLS-SRTP (in `tgcalls` v5.0–v11.0 / `InstanceV2Impl`), optional SOCKS5 UDP proxy.
   - **Signaling**: End-to-end encrypted JSON/binary signaling messages exchanged over MTProto
     via `phone.sendSignalingData` and `updatePhoneCallSignalingData` using the 256-byte DH
     `auth_key` (implemented in `tg_calls::signaling`).
   - **Media**: Opus 48 kHz audio (FEC, DTX, adaptive bitrate, jitter buffer, AEC/NS/AGC),
     VP8/VP9/H.264/AV1 video (optional phase 2), audio/video state & low-battery signaling,
     signal-bar quality estimation (0–4 bars from RTT + packet loss).
   - **Compatibility target**: Interoperate cleanly with official Telegram Desktop / iOS /
     Android clients via WebRTC-based `tgcalls` v5.0+ (`InstanceV2Impl` / `InstanceV2ReferenceImpl`).
     Legacy custom UDP protocol `2.4.4` (`libtgvoip` / `InstanceImpl`) is deprecated on modern
     clients and can be omitted once `5.0.0+` is supported.

2. **Group Calls / Voice Chats / Livestreams (`GroupCallSession` from `tg-calls`)**:
   - **Interactive RTC Mode**: WebRTC DTLS-SRTP connection to Telegram's custom SFU using the
     JSON join payload (`ssrc`, `ufrag`, `pwd`, `fingerprints`, `ssrc-groups`) and
     `updateGroupCallConnection` response (`transport`, `audio`, `video`, `candidates`).
     Audio-level callbacks (`AudioLevelSample`) feed directly into
     `GroupCallSession::audio_levels_updated`.
   - **Broadcast / RTMP Stream Mode**: Chunked MPEG-TS / Ogg-Opus / H.264 segments fetched
     over MTProto (`upload.getFile` with `inputGroupCallStream` on `stream_dc_id`) rather than
     WebRTC, decoded and synchronized against server timestamps.
   - **Conference E2E Calls (`TdE2E`)**: Frame-level SFrame/custom transform hook
     (`e2eEncryptDecrypt`) applied to encoded Opus/video payloads prior to SRTP packetization.

---

## 2. WebRTC Stack Evaluation: `str0m` vs `webrtc-rs` vs `libwebrtc` Bindings

| Criterion | `str0m` (Pure Rust, Sans-IO) | `webrtc-rs` (Pure Rust, Async) | `libwebrtc` C++ bindings (`livekit/webrtc-sys`) |
|---|---|---|---|
| **Build on Windows MSVC (no CMake)** | Pure Rust (`cargo build`, zero C/C++ build tools) | Pure Rust (`cargo build`, zero C/C++ build tools) | Very heavy: prebuilt ~500 MB Google WebRTC binaries or GN/Ninja/Clang toolchain |
| **Architecture fit with `tg-calls`** | **Best fit**: Sans-IO (`Rtc` driven by packets + timestamps), matches `tg-calls` cleanly | Tokio task-per-component; higher internal lock/channel overhead | Callback-driven C++ threads + custom ADM/VideoSink FFI glue |
| **Telegram SFU Custom JSON (non-SDP)** | **Excellent**: Direct SDP/ICE/DTLS/SSRC/payload-type API without fighting an opinionated PeerConnection state machine | Moderate: expects standard SDP offer/answer; requires synthesizing SDP from Telegram's custom JSON | Moderate: `tgcalls` itself has ~5k LOC of `SdpBuilder` glue to translate Telegram JSON <-> libwebrtc SDP |
| **ICE / STUN / TURN / DTLS-SRTP** | Full ICE, STUN, DTLS (`dimpl`/`openssl`/`rustls`), SRTP, TWCC, NACK, PLI/FIR, simulcast | Full ICE, STUN, TURN, DTLS, SRTP, SCTP | Reference implementation (gold standard BWE, jitter buffer, AEC3) |
| **TURN & Telegram Reflectors** | Needs small custom TURN/reflector UDP shim around socket I/O (easy because Sans-IO!) | Built-in `turn` crate; custom reflector needs transport wrapper | Built-in TURN; Telegram reflectors implemented in `tgcalls` via custom `PacketSocketFactory` |
| **Audio/Video Codecs & DSP** | Transport only — bring your own Opus, jitter buffer, AEC/NS/AGC, video codecs | Transport only — bring your own codecs & DSP | Bundled Opus, VP8/VP9/H.264/AV1, NetEq jitter buffer, WebRTC AEC3/NS/AGC |
| **Binary Size & Licensing** | Small (~2–4 MB), MIT/Apache-2.0 (GPL-3.0 compatible) | Medium (~5–8 MB), MIT/Apache-2.0 (GPL-3.0 compatible) | Huge (+30–60 MB binary), BSD-3-Clause |

### Recommendation

**Adopt `str0m` (primary) with a thin custom UDP socket loop (`tokio::net::UdpSocket`)** for
Phase 1–3 (1:1 voice calls + group voice chats + video), keeping the media engine behind a
small `MediaEngine` trait in a dedicated `tg-calls-media` crate so `libwebrtc` remains a
clean drop-in alternative only if mobile/desktop video parity or AEC3 performance later
justifies prebuilt C++ binaries.

Why `str0m` beats `webrtc-rs` here:
1. Telegram's SFU and `tgcalls` v5+ signaling do **not** exchange standard WebRTC SDP over the
   wire — they exchange compact JSON (`ufrag`, `pwd`, `fingerprints`, `candidates`,
   `payload-types`, `rtp-hdrexts`, `ssrc-groups`). `str0m`'s Sans-IO API makes synthesizing
   internal media lines and injecting custom reflector/TURN UDP sockets dramatically simpler.
2. Zero CMake / GN / Ninja requirement; compiles fast on `x86_64-pc-windows-msvc` using the
   existing Cargo toolchain.
3. Easy deterministic integration testing: two `str0m` endpoints can be wired together in
   memory in unit tests without touching sockets.

---

## 3. Audio Codec (Opus) & Voice DSP

- **Opus**:
  - Telegram calls use **Opus at 48 kHz**, 20 ms frames (960 samples/channel), mono for voice
    (stereo optional for music/screen audio), in-band FEC (`useinbandfec=1`), DTX, and
    adaptive bitrate (typically 16–32 kbps for 1:1 voice, 32 kbps for group calls).
  - Crate options on Windows MSVC without CMake:
    - `opus-static-sys` / `audiopus_sys` with `cc` crate compilation (C99 sources compiled via
      MSVC `cl.exe` — works with a standard MSVC Cargo setup since `cc` needs no CMake).
    - Pure-Rust decoder/encoder fallback: `mousiki` / `opus-codec` (still maturing — benchmark
      carefully before relying on pure-Rust Opus for production encoding).
  - **Plan**: Compile libopus (BSD-3-Clause, GPL-compatible) via a minimal `cc`-based `-sys`
    crate (no CMake), exposing a safe `OpusEncoder` / `OpusDecoder` wrapper in `tg-calls-media`.
- **Voice Processing (AEC / NS / AGC + VAD)**:
  - **Noise Suppression + VAD**: `nnnoiseless` (pure Rust port of RNNoise, 48 kHz / 10 ms
    frames, outputs both denoised audio and a voice-activity probability that directly drives
    `AudioLevelSample { level, voice }` and `GroupCallSession::audio_levels_updated`).
  - **Acoustic Echo Cancellation (AEC)**: `aec3` (pure Rust port of WebRTC AEC3, 48 kHz) or
    `speexdsp` via `cc`. Start with `nnnoiseless` + `aec3` so everything stays pure Rust.
  - **Jitter Buffer**: Implement a 48 kHz adaptive NetEq-lite jitter buffer (target delay
    40–200 ms, Opus PLC on missing packets, Opus FEC recovery on single-packet loss,
    clock-drift smoothing via 1-sample interpolation/drop).

---

## 4. Audio I/O with `cpal` on Windows

- **Crate**: `cpal` (WASAPI backend on Windows 10/11 x64, pure Rust via `windows` bindings,
  zero CMake).
- **Capture & Playback Architecture**:
  - Dedicated real-time audio callback threads communicating with the Tokio media loop via
    lock-free SPSC ring buffers (`ringbuf`, zero heap allocations in audio callbacks).
  - Resampling: Windows WASAPI shared mode frequently runs at 44.1 kHz or 48 kHz float32.
    Use `rubato` (pure Rust sinc/FFT resampler) to convert capture/playback to Opus's native
    48 kHz f32 mono/stereo.
  - **Device Selection & Hot-Swap**: Mirror tdesktop's `Webrtc::DeviceResolver` (`default`
    sentinel vs explicit WASAPI device ID) and expose `set_audio_input_device` /
    `set_audio_output_device` on the media engine trait.
  - **System Audio Capture (Screen Sharing)**: WASAPI loopback capture (`AUDCLNT_STREAMFLAGS_LOOPBACK`)
    is supported directly by `cpal` / `windows` crate on Windows 10/11, replacing
    `Webrtc::SystemAudioCapture`.

---

## 5. Reflectors, P2P, STUN/TURN, and Proxy Support

`tg_calls::protocol::collect_endpoints_and_servers` already converts TL `PhoneConnection`
entries into the exact structures `tgcalls` expects:

1. **WebRTC STUN / TURN (`phoneConnectionWebrtc`)**:
   - Standard RFC 5389 STUN binding requests (for server-reflexive candidates when
     `enable_p2p == true`) and RFC 5766 TURN allocations using `username`/`password`.
2. **Telegram Reflectors (`phoneConnection`)**:
   - In `tgcalls` v5+ (`InstanceV2Impl`), reflectors are exposed as TURN servers with
     `login = "reflector"` and `password = hex(peer_tag)` (16-byte tag -> 32 lowercase hex
     chars), over UDP or TCP (`is_tcp`).
   - Note that Telegram reflectors speak a Telegram-specific TURN-compatible relay framing
     (special peer-tag header / TURN channel data). Implement a dedicated
     `ReflectorSocket` adapter in `tg-calls-media` that handles both standard RFC 5766 TURN
     and Telegram's reflector packet format.
3. **Privacy (`enable_p2p = false`)**:
   - When `ControllerDescriptor.enable_p2p` is `false`, suppress host and `srflx` ICE
     candidates and gather only `relay` candidates so the local IP is never disclosed to the
     remote peer.
4. **SOCKS5 Proxy**:
   - When proxy-for-calls is enabled, route UDP packets through RFC 1928 SOCKS5 `UDP ASSOCIATE`
     (falling back to TCP reflectors if UDP associate is unavailable).

---

## 6. The `tgcalls` Signaling Protocol

For 1:1 calls (`tgcalls` v5.0+ / `InstanceV2Impl`):

1. **Outer Transport**:
   - Media engine emits opaque signaling blobs -> `Call::emit_signaling_data` ->
     `phone.sendSignalingData`.
   - Remote blobs arrive via `updatePhoneCallSignalingData` -> `Call::handle_signaling_data`
     -> `CallAction::ReceiveSignalingData`.
2. **Encryption Layer (`tg_calls::signaling`)**:
   - Every blob is encrypted with the 256-byte DH `auth_key` via MTProto 2.0 AES-256-IGE
     (`msg_key` + `derive_aes_key_iv`, directional offset `x = 128 + (0|8)`).
   - *Action item*: Once `TelegramMessenger/tgcalls` (`tgcalls/EncryptedConnection.cpp` and
     `tgcalls/v2/Signaling.cpp`) is checked out, verify the exact inner sequence header and
     JSON message types against a live test capture between two test instances.
3. **Inner Signaling JSON Messages (`tgcalls` v5+ `InstanceV2Impl`)**:
   - `initialSetup` / `negotiateChannels`: ICE `ufrag`/`pwd`, DTLS `fingerprints`, supported
     audio/video codecs, RTP header extensions, SSRC assignments.
   - `candidates`: trickle ICE candidates (`sdpString`, `sdpMid`, `sdpMLineIndex`).
   - `mediaState`: remote `audioState` (`active`/`muted`) and `videoState`
     (`inactive`/`paused`/`active`) -> feeds `Call::update_remote_media_state`.
   - `remoteBatteryLevelIsLow` -> feeds `Call::update_remote_battery_state`.
   - `videoParameters` / `videoRotation`: aspect ratio and rotation updates.

For group calls (`GroupCallSession`):
- Local join payload JSON (`GroupJoinPayload`) is generated by `str0m` (random non-zero
  audio SSRC, ICE `ufrag`/`pwd`, DTLS SHA-256 fingerprint, optional `ssrc-groups`) and
  delivered to `GroupCallSession::on_join_payload_ready`.
- Server SFU response JSON (`updateGroupCallConnection`) is parsed via
  `parse_join_response` into either `ParsedJoinResponse::Rtc` (fed to `str0m` to configure
  remote ICE/DTLS/codecs) or `ParsedJoinResponse::BroadcastStream` (switches engine to
  chunked MTProto `upload.getFile` stream playback).

---

## 7. Concrete Implementation Plan

### Crate Boundary
Keep `tg-calls` pure protocol & state machines (zero OS audio/network dependencies), and
create a sibling crate `tg-calls-media` implementing:

```rust
pub trait CallMediaEngine: Send {
    fn start(&mut self, descriptor: ControllerDescriptor);
    fn receive_signaling_data(&mut self, data: Vec<u8>);
    fn set_muted(&mut self, muted: bool);
    fn set_audio_devices(&mut self, input_id: &str, output_id: &str);
    fn poll_event(&mut self) -> Option<MediaEngineEvent>;
    fn stop(&mut self);
}
```

### Phased Rollout

1. **Phase 1 — Loopback & Protocol Interop Verification (1–2 weeks)**
   - Pin and inspect `TelegramMessenger/tgcalls` (`EncryptedConnection.cpp`,
     `v2/Signaling.cpp`, `group/GroupInstanceCustomImpl.cpp`) to lock down every byte of
     signaling framing and exact `DEFAULT_LIBRARY_VERSIONS` / `MAX_LAYER`.
   - Add golden test vectors captured from two `tgcalls` instances talking locally into
     `tg-calls/src/signaling.rs`.

2. **Phase 2 — 1:1 Voice Calls (`tg-calls-media` MVP, 2–3 weeks)**
   - Integrate `str0m` + Tokio UDP socket loop + RFC 5389 STUN + RFC 5766 TURN + Telegram
     reflector adapter.
   - Wire `cpal` WASAPI capture/playback + `rubato` 48 kHz resampling + `libopus` (`cc`
     build) + `nnnoiseless` NS/VAD + `aec3` echo cancellation + adaptive jitter buffer.
   - Implement `tgcalls` v5+ JSON signaling translator (`negotiateChannels`, `candidates`,
     `mediaState`) over `tg_calls::signaling::SignalingCipher`.
   - Map transport stats (RTT, packet loss) to 0–4 signal bars -> `Call::handle_controller_bar_count_change`.

3. **Phase 3 — Group Voice Chats (2 weeks)**
   - Implement `GroupMediaEngine` over `str0m` consuming `GroupJoinPayload` /
     `RtcJoinResponse` (multi-SSRC Opus mixing, per-SSRC volume scaling via
     `GroupCallAction::SetInstanceVolume`, SSRC removal, active speaker audio levels ->
     `GroupCallSession::audio_levels_updated`).
   - Add `StreamPlayer` for `InstanceMode::Stream` (fetching chunks via `upload.getFile`
     `inputGroupCallStream` and decoding Opus/audio tracks).

4. **Phase 4 — Video & Screen Sharing (3–4 weeks)**
   - Camera capture on Windows via `windows` crate Media Foundation (`IMFSourceReader`) or
     `nokhwa`, screen capture via Windows Graphics Capture API (`Windows.Graphics.Capture`).
   - Software/hardware VP8/H.264 encode/decode (`openh264` / Media Foundation H.264 MFT /
     `dav1d`) fed through `str0m` simulcast layers matching `compute_requested_video_channels`.
   - Frame-level E2E encryption hook for Telegram conference calls (`TdE2E`).
