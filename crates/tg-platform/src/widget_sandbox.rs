//! Sandboxed widget helper IPC channel and Win32 AppContainer / Job-Object launcher.
//!
//! Port of:
//! - `Telegram/SourceFiles/platform/win/widget_sandbox_win.h`
//! - `Telegram/SourceFiles/platform/win/widget_sandbox_win.cpp`
//! - `Telegram/SourceFiles/platform/win/widget_channel_win.h`
//! - `Telegram/SourceFiles/platform/win/widget_channel_win.cpp`
//! - `Telegram/SourceFiles/platform/win/widget_helper_win.cpp`
//! - `Telegram/SourceFiles/_other/widget_helper_win.cpp`
//!
//! Provides:
//! - [`WidgetHelperChannel`] length-prefixed framed IPC trait (`u32` little-endian length prefix +
//!   payload) with strict maximum message-size enforcement and clean disconnect reporting;
//! - [`WidgetByteStream`] + [`FramedWidgetChannel`] mirroring `twidget::SandboxRemoteStream`;
//! - [`FakeWidgetChannel`] in-memory duplex pair for deterministic IPC tests;
//! - [`WidgetSandboxLauncher`] & [`WidgetHelperProcess`] traits with [`Win32WidgetSandboxLauncher`]
//!   (owner-DACL overlapped named pipe, restricted Low-IL AppContainer tokens, Job object limits &
//!   UI restrictions, process creation mitigations) and [`FakeWidgetSandboxLauncher`].

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::error::{PlatformError, Result};

/// File name of the companion sandboxed widget helper binary located next to `rustgram.exe`.
pub const WIDGET_HELPER_EXE_NAME: &str = if cfg!(windows) {
    "rustgram-widget-helper.exe"
} else {
    "rustgram-widget-helper"
};

/// AppContainer profile name for the sandboxed widget helper (`kAppContainerName`).
pub const APP_CONTAINER_NAME: &str = "telegram.desktop.widget-helper";

/// Security attribute identifying a less-privileged AppContainer (`kLessPrivilegedName`).
pub const LESS_PRIVILEGED_APP_CONTAINER_ATTR: &str = "WIN://NOALLAPPPKG";

/// Named pipe send/receive buffer size (64 KiB, `kBufferSize` in `widget_channel_win.cpp:21`).
pub const PIPE_BUFFER_SIZE: u32 = 64 * 1024;

/// Length of the little-endian `u32` frame header preceding each payload.
pub const FRAME_LENGTH_PREFIX_BYTES: usize = 4;

/// Default maximum framed message size (32 MiB, matching `HostedWire::kMaxMessageBytes`).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

/// Minimum Windows 10 build required for Less-Privileged AppContainer sandboxing (Windows 10 1809).
pub const MIN_WINDOWS_BUILD_FOR_SANDBOX: u32 = 17763;

/// `SeChangeNotifyPrivilege` LUID low part (`kChangeNotifyPrivilege` in `widget_sandbox_win.cpp:53`).
pub const SE_CHANGE_NOTIFY_LUID_LOW: u32 = 23;

// Job object extended limit flags (`kJobLimits` in `widget_sandbox_win.cpp:55-57`).
/// `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`.
pub const JOB_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
/// `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`.
pub const JOB_LIMIT_DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x0000_0400;
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
pub const JOB_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
/// Default widget helper Job extended limit bitmask.
pub const DEFAULT_JOB_LIMIT_FLAGS: u32 =
    JOB_LIMIT_ACTIVE_PROCESS | JOB_LIMIT_KILL_ON_JOB_CLOSE | JOB_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;

/// Default maximum active processes allowed inside the widget helper Job (`kJobActiveProcesses`).
pub const DEFAULT_JOB_ACTIVE_PROCESS_LIMIT: u32 = 1;

// Job object basic UI restriction flags (`kJobUiRestrictions` in `widget_sandbox_win.cpp:59-66`).
/// `JOB_OBJECT_UILIMIT_HANDLES`.
pub const JOB_UILIMIT_HANDLES: u32 = 0x0000_0001;
/// `JOB_OBJECT_UILIMIT_READCLIPBOARD`.
pub const JOB_UILIMIT_READCLIPBOARD: u32 = 0x0000_0002;
/// `JOB_OBJECT_UILIMIT_WRITECLIPBOARD`.
pub const JOB_UILIMIT_WRITECLIPBOARD: u32 = 0x0000_0004;
/// `JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS`.
pub const JOB_UILIMIT_SYSTEMPARAMETERS: u32 = 0x0000_0008;
/// `JOB_OBJECT_UILIMIT_DISPLAYSETTINGS`.
pub const JOB_UILIMIT_DISPLAYSETTINGS: u32 = 0x0000_0010;
/// `JOB_OBJECT_UILIMIT_GLOBALATOMS`.
pub const JOB_UILIMIT_GLOBALATOMS: u32 = 0x0000_0020;
/// `JOB_OBJECT_UILIMIT_DESKTOP`.
pub const JOB_UILIMIT_DESKTOP: u32 = 0x0000_0040;
/// `JOB_OBJECT_UILIMIT_EXITWINDOWS`.
pub const JOB_UILIMIT_EXITWINDOWS: u32 = 0x0000_0080;
/// Default widget helper Job UI restriction bitmask (all 8 restrictions enabled).
pub const DEFAULT_JOB_UI_RESTRICTIONS: u32 = JOB_UILIMIT_HANDLES
    | JOB_UILIMIT_READCLIPBOARD
    | JOB_UILIMIT_WRITECLIPBOARD
    | JOB_UILIMIT_SYSTEMPARAMETERS
    | JOB_UILIMIT_DISPLAYSETTINGS
    | JOB_UILIMIT_GLOBALATOMS
    | JOB_UILIMIT_DESKTOP
    | JOB_UILIMIT_EXITWINDOWS;

// Process creation mitigation policy bitmasks (`ComputePolicies` in `widget_sandbox_win.cpp:557-650`).
/// `PROCESS_CREATION_MITIGATION_POLICY_DEP_ENABLE`.
pub const MITIGATION_DEP_ENABLE: u64 = 0x0000_0000_0000_0001;
/// `PROCESS_CREATION_MITIGATION_POLICY_FORCE_RELOCATE_IMAGES_ALWAYS_ON`.
pub const MITIGATION_FORCE_RELOCATE_IMAGES_ALWAYS_ON: u64 = 0x0000_0000_0000_0100;
/// `PROCESS_CREATION_MITIGATION_POLICY_BOTTOM_UP_ASLR_ALWAYS_ON`.
pub const MITIGATION_BOTTOM_UP_ASLR_ALWAYS_ON: u64 = 0x0000_0000_0001_0000;
/// `PROCESS_CREATION_MITIGATION_POLICY_HIGH_ENTROPY_ASLR_ALWAYS_ON`.
pub const MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON: u64 = 0x0000_0000_0010_0000;
/// `PROCESS_CREATION_MITIGATION_POLICY_STRICT_HANDLE_CHECKS_ALWAYS_ON`.
pub const MITIGATION_STRICT_HANDLE_CHECKS_ALWAYS_ON: u64 = 0x0000_0000_0100_0000;
/// `PROCESS_CREATION_MITIGATION_POLICY_WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON`.
pub const MITIGATION_WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON: u64 = 0x0000_0000_1000_0000;
/// `PROCESS_CREATION_MITIGATION_POLICY_EXTENSION_POINT_DISABLE_ALWAYS_ON`.
pub const MITIGATION_EXTENSION_POINT_DISABLE_ALWAYS_ON: u64 = 0x0000_0001_0000_0000;
/// `PROCESS_CREATION_MITIGATION_POLICY_PROHIBIT_DYNAMIC_CODE_ALWAYS_ON`.
pub const MITIGATION_PROHIBIT_DYNAMIC_CODE_ALWAYS_ON: u64 = 0x0000_0010_0000_0000;
/// `PROCESS_CREATION_MITIGATION_POLICY_BLOCK_NON_MICROSOFT_BINARIES_ALWAYS_ON`.
pub const MITIGATION_BLOCK_NON_MICROSOFT_BINARIES_ALWAYS_ON: u64 = 0x0000_1000_0000_0000;
/// `PROCESS_CREATION_MITIGATION_POLICY_FONT_DISABLE_ALWAYS_ON`.
pub const MITIGATION_FONT_DISABLE_ALWAYS_ON: u64 = 0x0001_0000_0000_0000;
/// `PROCESS_CREATION_MITIGATION_POLICY_IMAGE_LOAD_PREFER_SYSTEM32_ALWAYS_ON`.
pub const MITIGATION_IMAGE_LOAD_PREFER_SYSTEM32_ALWAYS_ON: u64 = 1u64 << 60;
/// `PROCESS_CREATION_MITIGATION_POLICY2_CET_USER_SHADOW_STACKS_ALWAYS_ON`.
pub const MITIGATION2_CET_USER_SHADOW_STACKS_ALWAYS_ON: u64 = 1u64 << 28;
/// `PROCESS_CREATION_CHILD_PROCESS_RESTRICTED`.
pub const CHILD_PROCESS_RESTRICTED: u32 = 0x01;
/// `PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT`.
pub const ALL_APPLICATION_PACKAGES_OPT_OUT: u32 = 0x01;

// ============================================================================
// Channel Error & Framed IPC Helpers
// ============================================================================

/// Error returned by [`WidgetHelperChannel`] and [`WidgetByteStream`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// The remote peer closed or dropped its end of the channel cleanly or unexpectedly.
    PeerDisconnected,
    /// This local channel endpoint has already been shut down.
    Closed,
    /// A sent or received frame exceeded the configured maximum message size.
    MessageTooLarge {
        /// Actual message size in bytes.
        size: usize,
        /// Configured maximum permitted size in bytes.
        max: usize,
    },
    /// Stream reached EOF while reading a frame length header or payload body.
    TruncatedFrame {
        /// Expected byte count.
        expected: usize,
        /// Actual bytes received before EOF.
        actual: usize,
    },
    /// Underlying OS or stream I/O failure.
    Io(String),
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PeerDisconnected => f.write_str("widget helper peer disconnected"),
            Self::Closed => f.write_str("widget helper channel is closed"),
            Self::MessageTooLarge { size, max } => {
                write!(
                    f,
                    "widget helper message size {size} exceeds limit of {max} bytes"
                )
            }
            Self::TruncatedFrame { expected, actual } => {
                write!(
                    f,
                    "truncated widget helper frame: expected {expected} bytes, got {actual}"
                )
            }
            Self::Io(msg) => write!(f, "widget helper channel I/O error: {msg}"),
        }
    }
}

impl std::error::Error for ChannelError {}

impl From<ChannelError> for PlatformError {
    fn from(err: ChannelError) -> Self {
        Self::WidgetSandbox(err.to_string())
    }
}

/// Encodes `payload` into a length-prefixed wire frame (`[len: u32 LE][payload]`).
pub fn encode_length_prefixed_frame(
    payload: &[u8],
    max_message_bytes: usize,
) -> std::result::Result<Vec<u8>, ChannelError> {
    if payload.len() > max_message_bytes {
        return Err(ChannelError::MessageTooLarge {
            size: payload.len(),
            max: max_message_bytes,
        });
    }
    let len_u32 = u32::try_from(payload.len()).map_err(|_| ChannelError::MessageTooLarge {
        size: payload.len(),
        max: max_message_bytes,
    })?;
    let mut frame = Vec::with_capacity(FRAME_LENGTH_PREFIX_BYTES + payload.len());
    frame.extend_from_slice(&len_u32.to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Decodes and validates a 4-byte little-endian frame length header against `max_message_bytes`.
pub fn decode_frame_length(
    header: [u8; FRAME_LENGTH_PREFIX_BYTES],
    max_message_bytes: usize,
) -> std::result::Result<usize, ChannelError> {
    let size = u32::from_le_bytes(header) as usize;
    if size > max_message_bytes {
        return Err(ChannelError::MessageTooLarge {
            size,
            max: max_message_bytes,
        });
    }
    Ok(size)
}

/// Writes one length-prefixed frame to any [`std::io::Write`] sink.
pub fn write_framed_to<W: Write>(
    writer: &mut W,
    payload: &[u8],
    max_message_bytes: usize,
) -> std::result::Result<(), ChannelError> {
    let frame = encode_length_prefixed_frame(payload, max_message_bytes)?;
    writer
        .write_all(&frame)
        .map_err(|e| ChannelError::Io(e.to_string()))
}

/// Reads one length-prefixed frame from any [`std::io::Read`] source.
pub fn read_framed_from<R: Read>(
    reader: &mut R,
    max_message_bytes: usize,
) -> std::result::Result<Vec<u8>, ChannelError> {
    let mut header = [0u8; FRAME_LENGTH_PREFIX_BYTES];
    let mut read_header = 0;
    while read_header < FRAME_LENGTH_PREFIX_BYTES {
        match reader.read(&mut header[read_header..]) {
            Ok(0) => {
                if read_header == 0 {
                    return Err(ChannelError::PeerDisconnected);
                }
                return Err(ChannelError::TruncatedFrame {
                    expected: FRAME_LENGTH_PREFIX_BYTES,
                    actual: read_header,
                });
            }
            Ok(n) => read_header += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ChannelError::Io(e.to_string())),
        }
    }
    let payload_len = decode_frame_length(header, max_message_bytes)?;
    let mut payload = vec![0u8; payload_len];
    let mut read_body = 0;
    while read_body < payload_len {
        match reader.read(&mut payload[read_body..]) {
            Ok(0) => {
                return Err(ChannelError::TruncatedFrame {
                    expected: payload_len,
                    actual: read_body,
                });
            }
            Ok(n) => read_body += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ChannelError::Io(e.to_string())),
        }
    }
    Ok(payload)
}

/// Low-level duplex byte stream between host and widget helper (`twidget::SandboxRemoteStream` in
/// `widget_channel_win.h:28-50`).
pub trait WidgetByteStream: Send + Sync {
    /// Reads up to `out.len()` bytes; returns `Ok(0)` on peer EOF.
    fn read(&self, out: &mut [u8]) -> std::result::Result<usize, ChannelError>;

    /// Writes all bytes in `bytes` or returns a [`ChannelError`].
    fn write_all(&self, bytes: &[u8]) -> std::result::Result<(), ChannelError>;

    /// Cancels pending I/O and closes this stream endpoint.
    fn shutdown(&self);

    /// Returns `true` after [`Self::shutdown`] has been called.
    fn is_closed(&self) -> bool;
}

/// Length-prefixed framed IPC channel between host and sandboxed widget helper.
pub trait WidgetHelperChannel: Send + Sync {
    /// Sends a single length-prefixed payload frame.
    fn send_frame(&self, payload: &[u8]) -> std::result::Result<(), ChannelError>;

    /// Receives the next length-prefixed payload frame from the peer.
    fn recv_frame(&self) -> std::result::Result<Vec<u8>, ChannelError>;

    /// Maximum allowed payload size in bytes for both send and receive.
    fn max_message_bytes(&self) -> usize;

    /// Shuts down this endpoint and wakes any blocked peer/local operations.
    fn shutdown(&self);

    /// Returns `true` if this endpoint has been shut down.
    fn is_closed(&self) -> bool;
}

/// Length-prefixed [`WidgetHelperChannel`] wrapper around any [`WidgetByteStream`].
pub struct FramedWidgetChannel<S: WidgetByteStream> {
    stream: S,
    max_message_bytes: usize,
    read_lock: Mutex<()>,
    write_lock: Mutex<()>,
}

impl<S: WidgetByteStream> FramedWidgetChannel<S> {
    /// Wraps `stream` with a length-prefixed framing layer enforcing `max_message_bytes`.
    #[must_use]
    pub fn new(stream: S, max_message_bytes: usize) -> Self {
        Self {
            stream,
            max_message_bytes,
            read_lock: Mutex::new(()),
            write_lock: Mutex::new(()),
        }
    }

    /// Borrows the underlying byte stream.
    #[must_use]
    pub fn stream(&self) -> &S {
        &self.stream
    }
}

impl<S: WidgetByteStream> WidgetHelperChannel for FramedWidgetChannel<S> {
    fn send_frame(&self, payload: &[u8]) -> std::result::Result<(), ChannelError> {
        if self.stream.is_closed() {
            return Err(ChannelError::Closed);
        }
        let frame = encode_length_prefixed_frame(payload, self.max_message_bytes)?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| ChannelError::Io("write lock poisoned".into()))?;
        self.stream.write_all(&frame)
    }

    fn recv_frame(&self) -> std::result::Result<Vec<u8>, ChannelError> {
        if self.stream.is_closed() {
            return Err(ChannelError::Closed);
        }
        let _guard = self
            .read_lock
            .lock()
            .map_err(|_| ChannelError::Io("read lock poisoned".into()))?;
        let mut header = [0u8; FRAME_LENGTH_PREFIX_BYTES];
        let mut header_read = 0;
        while header_read < FRAME_LENGTH_PREFIX_BYTES {
            let n = self.stream.read(&mut header[header_read..])?;
            if n == 0 {
                if header_read == 0 {
                    return Err(if self.stream.is_closed() {
                        ChannelError::Closed
                    } else {
                        ChannelError::PeerDisconnected
                    });
                }
                return Err(ChannelError::TruncatedFrame {
                    expected: FRAME_LENGTH_PREFIX_BYTES,
                    actual: header_read,
                });
            }
            header_read += n;
        }

        let payload_len = decode_frame_length(header, self.max_message_bytes)?;
        let mut payload = vec![0u8; payload_len];
        let mut body_read = 0;
        while body_read < payload_len {
            let n = self.stream.read(&mut payload[body_read..])?;
            if n == 0 {
                return Err(ChannelError::TruncatedFrame {
                    expected: payload_len,
                    actual: body_read,
                });
            }
            body_read += n;
        }
        Ok(payload)
    }

    fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    fn shutdown(&self) {
        self.stream.shutdown();
    }

    fn is_closed(&self) -> bool {
        self.stream.is_closed()
    }
}

// ============================================================================
// In-Memory FakeWidgetChannel Pair
// ============================================================================

struct DirectionalPipe {
    bytes: VecDeque<u8>,
    writer_closed: bool,
    reader_closed: bool,
}

impl DirectionalPipe {
    fn new() -> Self {
        Self {
            bytes: VecDeque::new(),
            writer_closed: false,
            reader_closed: false,
        }
    }
}

struct PipeSync {
    state: Mutex<DirectionalPipe>,
    cv: Condvar,
}

impl PipeSync {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(DirectionalPipe::new()),
            cv: Condvar::new(),
        })
    }
}

/// In-memory duplex framed IPC endpoint implementing [`WidgetHelperChannel`].
///
/// Created in connected pairs via [`FakeWidgetChannel::pair`]. Internally streams real
/// length-prefixed byte frames so framing, size caps, truncation, and disconnect behavior match
/// the Win32 named pipe transport without touching OS pipes.
pub struct FakeWidgetChannel {
    outbound: Arc<PipeSync>,
    inbound: Arc<PipeSync>,
    max_message_bytes: usize,
    closed: AtomicBool,
    read_lock: Mutex<()>,
    write_lock: Mutex<()>,
}

impl FakeWidgetChannel {
    /// Creates a connected pair `(host_end, helper_end)` enforcing `max_message_bytes`.
    #[must_use]
    pub fn pair(max_message_bytes: usize) -> (Self, Self) {
        let a_to_b = PipeSync::new();
        let b_to_a = PipeSync::new();
        let a = Self {
            outbound: Arc::clone(&a_to_b),
            inbound: Arc::clone(&b_to_a),
            max_message_bytes,
            closed: AtomicBool::new(false),
            read_lock: Mutex::new(()),
            write_lock: Mutex::new(()),
        };
        let b = Self {
            outbound: b_to_a,
            inbound: a_to_b,
            max_message_bytes,
            closed: AtomicBool::new(false),
            read_lock: Mutex::new(()),
            write_lock: Mutex::new(()),
        };
        (a, b)
    }

    /// Creates a connected pair using [`DEFAULT_MAX_MESSAGE_BYTES`] (32 MiB).
    #[must_use]
    pub fn pair_default() -> (Self, Self) {
        Self::pair(DEFAULT_MAX_MESSAGE_BYTES)
    }

    /// Injects raw bytes directly onto the outbound byte stream, bypassing frame encoding.
    ///
    /// Useful in tests to simulate a malicious or crashing peer sending an oversized length prefix
    /// or truncated frame payload.
    pub fn inject_raw_bytes(&self, raw: &[u8]) -> std::result::Result<(), ChannelError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ChannelError::Closed);
        }
        let mut guard = self
            .outbound
            .state
            .lock()
            .map_err(|_| ChannelError::Io("pipe lock poisoned".into()))?;
        if guard.reader_closed {
            return Err(ChannelError::PeerDisconnected);
        }
        guard.bytes.extend(raw);
        self.outbound.cv.notify_all();
        Ok(())
    }

    /// Non-blocking receive attempt: returns `Ok(None)` when no bytes are buffered yet and the
    /// peer is still connected.
    pub fn try_recv_frame(&self) -> std::result::Result<Option<Vec<u8>>, ChannelError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ChannelError::Closed);
        }
        let _read_guard = self
            .read_lock
            .lock()
            .map_err(|_| ChannelError::Io("read lock poisoned".into()))?;
        let mut guard = self
            .inbound
            .state
            .lock()
            .map_err(|_| ChannelError::Io("pipe lock poisoned".into()))?;
        if guard.bytes.is_empty() {
            if guard.writer_closed {
                return Err(ChannelError::PeerDisconnected);
            }
            return Ok(None);
        }
        if guard.bytes.len() < FRAME_LENGTH_PREFIX_BYTES {
            if guard.writer_closed {
                let actual = guard.bytes.len();
                guard.bytes.clear();
                return Err(ChannelError::TruncatedFrame {
                    expected: FRAME_LENGTH_PREFIX_BYTES,
                    actual,
                });
            }
            return Ok(None);
        }
        let header = [
            guard.bytes[0],
            guard.bytes[1],
            guard.bytes[2],
            guard.bytes[3],
        ];
        let payload_len = decode_frame_length(header, self.max_message_bytes)?;
        let total = FRAME_LENGTH_PREFIX_BYTES.checked_add(payload_len).ok_or(
            ChannelError::MessageTooLarge {
                size: payload_len,
                max: self.max_message_bytes,
            },
        )?;
        if guard.bytes.len() < total {
            if guard.writer_closed {
                let actual = guard.bytes.len() - FRAME_LENGTH_PREFIX_BYTES;
                guard.bytes.clear();
                return Err(ChannelError::TruncatedFrame {
                    expected: payload_len,
                    actual,
                });
            }
            return Ok(None);
        }
        for _ in 0..FRAME_LENGTH_PREFIX_BYTES {
            let _ = guard.bytes.pop_front();
        }
        let mut payload = Vec::with_capacity(payload_len);
        for _ in 0..payload_len {
            if let Some(b) = guard.bytes.pop_front() {
                payload.push(b);
            }
        }
        Ok(Some(payload))
    }

    fn read_exact_blocking(
        &self,
        buf: &mut [u8],
        is_header: bool,
    ) -> std::result::Result<(), ChannelError> {
        let mut filled = 0;
        let mut guard = self
            .inbound
            .state
            .lock()
            .map_err(|_| ChannelError::Io("pipe lock poisoned".into()))?;
        while filled < buf.len() {
            while let Some(byte) = guard.bytes.pop_front() {
                buf[filled] = byte;
                filled += 1;
                if filled == buf.len() {
                    return Ok(());
                }
            }
            if self.closed.load(Ordering::SeqCst) || guard.reader_closed {
                return Err(ChannelError::Closed);
            }
            if guard.writer_closed {
                if is_header && filled == 0 {
                    return Err(ChannelError::PeerDisconnected);
                }
                return Err(ChannelError::TruncatedFrame {
                    expected: buf.len(),
                    actual: filled,
                });
            }
            guard = self
                .inbound
                .cv
                .wait(guard)
                .map_err(|_| ChannelError::Io("pipe condvar poisoned".into()))?;
        }
        Ok(())
    }

    fn mark_closed(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut out) = self.outbound.state.lock() {
            out.writer_closed = true;
            self.outbound.cv.notify_all();
        }
        if let Ok(mut inp) = self.inbound.state.lock() {
            inp.reader_closed = true;
            self.inbound.cv.notify_all();
        }
    }
}

impl Drop for FakeWidgetChannel {
    fn drop(&mut self) {
        self.mark_closed();
    }
}

impl WidgetHelperChannel for FakeWidgetChannel {
    fn send_frame(&self, payload: &[u8]) -> std::result::Result<(), ChannelError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ChannelError::Closed);
        }
        let frame = encode_length_prefixed_frame(payload, self.max_message_bytes)?;
        let _write_guard = self
            .write_lock
            .lock()
            .map_err(|_| ChannelError::Io("write lock poisoned".into()))?;
        let mut guard = self
            .outbound
            .state
            .lock()
            .map_err(|_| ChannelError::Io("pipe lock poisoned".into()))?;
        if guard.reader_closed {
            return Err(ChannelError::PeerDisconnected);
        }
        guard.bytes.extend(frame);
        self.outbound.cv.notify_all();
        Ok(())
    }

    fn recv_frame(&self) -> std::result::Result<Vec<u8>, ChannelError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(ChannelError::Closed);
        }
        let _read_guard = self
            .read_lock
            .lock()
            .map_err(|_| ChannelError::Io("read lock poisoned".into()))?;
        let mut header = [0u8; FRAME_LENGTH_PREFIX_BYTES];
        self.read_exact_blocking(&mut header, true)?;
        let payload_len = decode_frame_length(header, self.max_message_bytes)?;
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            self.read_exact_blocking(&mut payload, false)?;
        }
        Ok(payload)
    }

    fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    fn shutdown(&self) {
        self.mark_closed();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

// ============================================================================
// Sandbox Policy, Job Verification, Quoting & Handle Parsing
// ============================================================================

/// AppContainer privilege tier (`Platform::WidgetHelper::AppContainerLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppContainerLevel {
    /// Regular AppContainer (includes `ALL APPLICATION PACKAGES` SID).
    Regular,
    /// Less-privileged AppContainer (`WIN://NOALLAPPPKG`, excludes `ALL APPLICATION PACKAGES`).
    LessPrivileged,
}

/// Process mitigation expectation verified inside the helper (`Platform::WidgetHelper::Mitigation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MitigationExpectation {
    /// `PROCESS_MITIGATION_POLICY` numeric identifier.
    pub policy_id: u32,
    /// Struct size in bytes.
    pub size_bytes: u32,
    /// Required policy flag bits.
    pub required_flags: u32,
    /// Forbidden policy flag bits.
    pub forbidden_flags: u32,
}

/// Process creation mitigation policy set (`Platform::WidgetHelper::Policies`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicies {
    /// Two 64-bit mitigation policy attribute words passed via `PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY`.
    pub creation: [u64; 2],
    /// Effective byte size of `creation` (`8` when word 1 is 0, else `16`).
    pub creation_size: usize,
    /// Value for `PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY`.
    pub child_process: u32,
    /// Value for `PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY`.
    pub all_application_packages: u32,
    /// Per-policy expectations checked by `VerifyMitigations` inside the helper.
    pub mitigations: Vec<MitigationExpectation>,
}

/// Computes the exact Windows sandbox mitigation policies matching `ComputePolicies()` in
/// `widget_sandbox_win.cpp:557-650`.
#[must_use]
pub fn compute_sandbox_policies(is_64_bit: bool, cet_available: bool) -> SandboxPolicies {
    let mut words = [0u64; 2];
    let mut mitigations = Vec::with_capacity(11);

    if !is_64_bit {
        words[0] |= MITIGATION_DEP_ENABLE;
        mitigations.push(MitigationExpectation {
            policy_id: 0, // ProcessDEPPolicy
            size_bytes: 8,
            required_flags: 0x0000_0003, // Enable | DisableAtlThunkEmulation
            forbidden_flags: 0,
        });
    }

    // ProcessASLRPolicy (1)
    let mut aslr_req = 0x0000_0003; // EnableBottomUpRandomization | EnableForceRelocateImages
    words[0] |= MITIGATION_BOTTOM_UP_ASLR_ALWAYS_ON | MITIGATION_FORCE_RELOCATE_IMAGES_ALWAYS_ON;
    if is_64_bit {
        aslr_req |= 0x0000_0004; // EnableHighEntropy
        words[0] |= MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON;
    }
    mitigations.push(MitigationExpectation {
        policy_id: 1,
        size_bytes: 4,
        required_flags: aslr_req,
        forbidden_flags: 0,
    });

    // ProcessDynamicCodePolicy (2)
    words[0] |= MITIGATION_PROHIBIT_DYNAMIC_CODE_ALWAYS_ON;
    mitigations.push(MitigationExpectation {
        policy_id: 2,
        size_bytes: 4,
        required_flags: 0x0000_0001,  // ProhibitDynamicCode
        forbidden_flags: 0x0000_0006, // AllowThreadOptOut | AllowRemoteDowngrade
    });

    // ProcessStrictHandleCheckPolicy (3)
    words[0] |= MITIGATION_STRICT_HANDLE_CHECKS_ALWAYS_ON;
    mitigations.push(MitigationExpectation {
        policy_id: 3,
        size_bytes: 4,
        required_flags: 0x0000_0003,
        forbidden_flags: 0,
    });

    // ProcessSystemCallDisablePolicy (4)
    words[0] |= MITIGATION_WIN32K_SYSTEM_CALL_DISABLE_ALWAYS_ON;
    mitigations.push(MitigationExpectation {
        policy_id: 4,
        size_bytes: 4,
        required_flags: 0x0000_0001,
        forbidden_flags: 0,
    });

    // ProcessExtensionPointDisablePolicy (6)
    words[0] |= MITIGATION_EXTENSION_POINT_DISABLE_ALWAYS_ON;
    mitigations.push(MitigationExpectation {
        policy_id: 6,
        size_bytes: 4,
        required_flags: 0x0000_0001,
        forbidden_flags: 0,
    });

    // ProcessSignaturePolicy (8)
    words[0] |= MITIGATION_BLOCK_NON_MICROSOFT_BINARIES_ALWAYS_ON;
    mitigations.push(MitigationExpectation {
        policy_id: 8,
        size_bytes: 4,
        required_flags: 0x0000_0001, // MicrosoftSignedOnly
        forbidden_flags: 0,
    });

    // ProcessFontDisablePolicy (9)
    words[0] |= MITIGATION_FONT_DISABLE_ALWAYS_ON;
    mitigations.push(MitigationExpectation {
        policy_id: 9,
        size_bytes: 4,
        required_flags: 0x0000_0001, // DisableNonSystemFonts
        forbidden_flags: 0,
    });

    // ProcessImageLoadPolicy (10)
    words[0] |= MITIGATION_IMAGE_LOAD_PREFER_SYSTEM32_ALWAYS_ON;
    mitigations.push(MitigationExpectation {
        policy_id: 10,
        size_bytes: 4,
        required_flags: 0x0000_0004, // PreferSystem32Images
        forbidden_flags: 0,
    });

    // ProcessChildProcessPolicy (13)
    mitigations.push(MitigationExpectation {
        policy_id: 13,
        size_bytes: 4,
        required_flags: 0x0000_0001,  // NoChildProcessCreation
        forbidden_flags: 0x0000_0004, // AllowSecureProcessCreation
    });

    // ProcessUserShadowStackPolicy (16)
    if is_64_bit && cet_available {
        words[1] |= MITIGATION2_CET_USER_SHADOW_STACKS_ALWAYS_ON;
        mitigations.push(MitigationExpectation {
            policy_id: 16,
            size_bytes: 4,
            required_flags: 0x0000_0001,
            forbidden_flags: 0,
        });
    }

    let creation_size = if words[1] != 0 { 16 } else { 8 };
    SandboxPolicies {
        creation: words,
        creation_size,
        child_process: CHILD_PROCESS_RESTRICTED,
        all_application_packages: ALL_APPLICATION_PACKAGES_OPT_OUT,
        mitigations,
    }
}

/// Job Object limit & UI restriction configuration (`widget_sandbox_win.cpp:55-66`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobLimitsConfig {
    /// Extended limit flags (`JOB_OBJECT_LIMIT_*`).
    pub limit_flags: u32,
    /// Maximum concurrent active processes inside the job (`1`).
    pub active_process_limit: u32,
    /// Basic UI restriction flags (`JOB_OBJECT_UILIMIT_*`).
    pub ui_restrictions: u32,
}

impl Default for JobLimitsConfig {
    fn default() -> Self {
        Self {
            limit_flags: DEFAULT_JOB_LIMIT_FLAGS,
            active_process_limit: DEFAULT_JOB_ACTIVE_PROCESS_LIMIT,
            ui_restrictions: DEFAULT_JOB_UI_RESTRICTIONS,
        }
    }
}

/// Verifies observed Job Object limits against `expected` (`VerifyJob` in
/// `widget_sandbox_win.cpp:972-991`).
pub fn verify_job_limits(
    in_job: bool,
    limit_flags: u32,
    active_process_limit: u32,
    ui_restrictions: u32,
    expected: &JobLimitsConfig,
) -> std::result::Result<(), String> {
    if !in_job {
        return Err("the process is not in a job".to_owned());
    }
    if (limit_flags & expected.limit_flags) != expected.limit_flags
        || active_process_limit != expected.active_process_limit
    {
        return Err(format!(
            "the job's limits are 0x{limit_flags:x} for {active_process_limit} processes"
        ));
    }
    if (ui_restrictions & expected.ui_restrictions) != expected.ui_restrictions {
        return Err(format!(
            "the job's UI restrictions are 0x{ui_restrictions:x}"
        ));
    }
    Ok(())
}

/// Formats the per-launch private named pipe path (`PipeName` in `widget_helper_win.cpp:66-72`).
#[must_use]
pub fn format_widget_pipe_name(pid: u32, rand_hi: u64, rand_lo: u64) -> String {
    format!(r"\\.\pipe\telegram-widget-helper-{pid}-{rand_hi:016x}{rand_lo:016x}")
}

/// Quotes one command-line argument according to Windows `CommandLineToArgvW` rules
/// (`QuotedArgument` in `widget_helper_win.cpp:88-108`).
#[must_use]
pub fn quote_windows_argument(argument: &str) -> String {
    if !argument.is_empty()
        && !argument
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0B' | '"'))
    {
        return argument.to_owned();
    }
    let mut result = String::with_capacity(argument.len() + 2);
    result.push('"');
    let mut slashes = 0usize;
    for ch in argument.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        let repeat = if ch == '"' { slashes * 2 + 1 } else { slashes };
        for _ in 0..repeat {
            result.push('\\');
        }
        slashes = 0;
        result.push(ch);
    }
    for _ in 0..(slashes * 2) {
        result.push('\\');
    }
    result.push('"');
    result
}

/// Parses an inherited decimal pipe handle argument passed to the widget helper, rejecting
/// empty/leading-zero strings and Windows pseudo-handle sentinels (`ParseHandle` in
/// `_other/widget_helper_win.cpp:24-47`).
#[must_use]
pub fn parse_inherited_handle_token(token: &str) -> Option<usize> {
    if token.is_empty() || token.starts_with('0') {
        return None;
    }
    let value = token.parse::<usize>().ok()?;
    // Reject pseudo-handles: INVALID_HANDLE_VALUE (-1), GetCurrentThread (-2),
    // GetCurrentProcessToken (-4), GetCurrentThreadToken (-5),
    // GetCurrentThreadEffectiveToken (-6).
    let sentinels: [usize; 5] = [
        usize::MAX,
        usize::MAX - 1,
        usize::MAX - 3,
        usize::MAX - 4,
        usize::MAX - 5,
    ];
    if value == 0 || sentinels.contains(&value) {
        return None;
    }
    Some(value)
}

/// Resolves the companion `rustgram-widget-helper.exe` path placed next to `main_exe`.
#[must_use]
pub fn resolve_widget_helper_path(main_exe: &Path) -> PathBuf {
    match main_exe.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(WIDGET_HELPER_EXE_NAME),
        _ => PathBuf::from(WIDGET_HELPER_EXE_NAME),
    }
}

/// Resolves `rustgram-widget-helper.exe` next to `exe_path` when it has a parent directory,
/// otherwise falling back to `fallback_dir`.
#[must_use]
pub fn resolve_widget_helper_path_with_fallback(
    exe_path: Option<&Path>,
    fallback_dir: &Path,
) -> PathBuf {
    if let Some(exe) = exe_path
        && let Some(parent) = exe.parent()
        && !parent.as_os_str().is_empty()
    {
        return parent.join(WIDGET_HELPER_EXE_NAME);
    }
    fallback_dir.join(WIDGET_HELPER_EXE_NAME)
}

/// Resolves `rustgram-widget-helper.exe` next to the currently running executable.
#[must_use]
pub fn resolve_widget_helper_path_from_current_exe() -> PathBuf {
    match std::env::current_exe() {
        Ok(exe) => resolve_widget_helper_path(&exe),
        Err(_) => PathBuf::from(WIDGET_HELPER_EXE_NAME),
    }
}

/// Adopts the inherited client named-pipe handle passed on the helper command line
/// (`WidgetHelper::Adopt` in `_other/widget_helper_win.cpp:51-61`).
pub fn adopt_inherited_widget_channel(
    token: &str,
    max_message_bytes: usize,
) -> Result<Box<dyn WidgetHelperChannel>> {
    let raw_handle = parse_inherited_handle_token(token).ok_or_else(|| {
        PlatformError::WidgetSandbox(format!("invalid inherited pipe handle token '{token}'"))
    })?;
    #[cfg(windows)]
    {
        win32_impl::adopt_inherited_channel(raw_handle, max_message_bytes)
    }
    #[cfg(not(windows))]
    {
        let _ = (raw_handle, max_message_bytes);
        Err(PlatformError::Unsupported)
    }
}

/// Drops the startup thread impersonation token and verifies sandbox Job Object limits inside
/// the helper process (`WidgetHelper::Lockdown` in `_other/widget_helper_win.cpp:63-65`).
pub fn lockdown_widget_helper(expected_job: &JobLimitsConfig) -> Result<()> {
    #[cfg(windows)]
    {
        win32_impl::lockdown_helper(expected_job)
    }
    #[cfg(not(windows))]
    {
        let _ = expected_job;
        Ok(())
    }
}

/// Runs the framed IPC serve loop inside `rustgram-widget-helper.exe` (or in-process over
/// [`FakeWidgetChannel`] in tests).
///
/// Speaks the length-prefixed wire protocol shared with `LiveWidgetHost` and `tg-tgw`:
/// - Binary control frames starting with `0xFF`:
///   - `Open` (`[0xFF, 1, ..]`) -> replies with `Content` (`[0xFF, 64]`) and `Loaded` (`[0xFF, 65]`).
///   - `Stopped` (`[0xFF, 66, ..]`) -> terminates the serve loop cleanly.
/// - UTF-8 JSON bridge frames (`{"type":"ready"|"frame"|"frozen"|"stopped",...}`):
///   - `"ready"` -> replies with `{"type":"ready"}`.
///   - `"frame"` -> replies with `{"type":"ops","count":0}`.
///   - `"frozen"` / `"stopped"` -> terminates the serve loop cleanly.
/// - Plain probe frames (`b"ping"` -> `b"pong"`, or echoes other non-empty frames).
pub fn serve_widget_helper_channel(channel: &dyn WidgetHelperChannel) -> Result<()> {
    const WIRE_TAG: u8 = 0xFF;
    const KIND_OPEN: u8 = 1;
    const KIND_CONTENT: u8 = 64;
    const KIND_LOADED: u8 = 65;
    const KIND_STOPPED: u8 = 66;

    loop {
        let frame = match channel.recv_frame() {
            Ok(bytes) => bytes,
            Err(ChannelError::PeerDisconnected | ChannelError::Closed) => return Ok(()),
            Err(err) => return Err(PlatformError::from(err)),
        };

        if frame.len() >= 2 && frame[0] == WIRE_TAG {
            match frame[1] {
                KIND_OPEN => {
                    channel.send_frame(&[WIRE_TAG, KIND_CONTENT])?;
                    channel.send_frame(&[WIRE_TAG, KIND_LOADED])?;
                }
                KIND_STOPPED => return Ok(()),
                _ => {}
            }
            continue;
        }

        if let Ok(text) = std::str::from_utf8(&frame)
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(text)
            && let Some(kind) = val.get("type").and_then(serde_json::Value::as_str)
        {
            match kind {
                "ready" => channel.send_frame(br#"{"type":"ready"}"#)?,
                "frame" => channel.send_frame(br#"{"type":"ops","count":0}"#)?,
                "frozen" | "stopped" => return Ok(()),
                _ => channel.send_frame(br#"{"type":"ready"}"#)?,
            }
            continue;
        }

        if frame == b"ping" {
            channel.send_frame(b"pong")?;
        } else if !frame.is_empty() {
            channel.send_frame(&frame)?;
        }
    }
}

// ============================================================================
// Launcher & Helper Process Traits + Fake Implementation
// ============================================================================

/// Configuration for spawning a sandboxed widget helper process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetSandboxConfig {
    /// Path to the widget helper executable.
    pub executable: PathBuf,
    /// Extra command-line arguments appended after the inherited pipe handle.
    pub arguments: Vec<String>,
    /// AppContainer name used to derive the package SID.
    pub app_container_name: String,
    /// Target AppContainer privilege tier.
    pub app_container_level: AppContainerLevel,
    /// Job Object extended limits and UI restrictions.
    pub job_limits: JobLimitsConfig,
    /// Maximum IPC message size in bytes.
    pub max_message_bytes: usize,
}

impl WidgetSandboxConfig {
    /// Creates a default sandbox configuration for `executable`.
    #[must_use]
    pub fn for_executable(executable: PathBuf) -> Self {
        Self {
            executable,
            arguments: Vec::new(),
            app_container_name: APP_CONTAINER_NAME.to_owned(),
            app_container_level: AppContainerLevel::LessPrivileged,
            job_limits: JobLimitsConfig::default(),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        }
    }
}

/// Process exit/teardown summary (`Platform::WidgetHelper::Completion`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetHelperCompletion {
    /// Observed process exit code, if reaped.
    pub exit_code: Option<u32>,
    /// `true` if the child process exited and was reaped.
    pub reaped: bool,
    /// `true` if the job object had to be forcefully terminated.
    pub terminated: bool,
    /// Diagnostic detail string (e.g. `"exit 0"`).
    pub detail: String,
}

/// Running sandboxed widget helper process handle.
pub trait WidgetHelperProcess: Send + Sync {
    /// OS or synthetic process identifier.
    fn pid(&self) -> u32;

    /// Framed IPC channel connected to this widget helper.
    fn channel(&self) -> &dyn WidgetHelperChannel;

    /// Waits up to `timeout` for the helper to exit cleanly.
    fn wait_exit(&self, timeout: Duration) -> Result<Option<WidgetHelperCompletion>>;

    /// Shuts down the IPC channel and forcefully terminates the helper's Job Object.
    fn terminate(&self) -> Result<WidgetHelperCompletion>;
}

/// Abstraction over launching sandboxed widget helper processes.
pub trait WidgetSandboxLauncher: Send + Sync {
    /// Returns `true` when OS-level widget helper sandboxing is available.
    fn is_supported(&self) -> bool;

    /// Returns a human-readable explanation if sandboxing is unsupported.
    fn unsupported_reason(&self) -> Option<String>;

    /// Launches a new sandboxed widget helper process and returns its handle + IPC channel.
    fn launch(&self, config: &WidgetSandboxConfig) -> Result<Box<dyn WidgetHelperProcess>>;
}

/// In-memory fake [`WidgetHelperProcess`] backed by a [`FakeWidgetChannel`].
pub struct FakeWidgetHelperProcess {
    pid: u32,
    channel: FakeWidgetChannel,
    exit_code: Mutex<Option<u32>>,
    terminated: AtomicBool,
}

impl FakeWidgetHelperProcess {
    /// Marks this fake process as having exited with `code`.
    pub fn mark_exited(&self, code: u32) {
        if let Ok(mut guard) = self.exit_code.lock() {
            *guard = Some(code);
        }
    }
}

impl WidgetHelperProcess for FakeWidgetHelperProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn channel(&self) -> &dyn WidgetHelperChannel {
        &self.channel
    }

    fn wait_exit(&self, _timeout: Duration) -> Result<Option<WidgetHelperCompletion>> {
        let code = self
            .exit_code
            .lock()
            .map_err(|_| PlatformError::WidgetSandbox("exit lock poisoned".into()))?;
        Ok(code.map(|c| WidgetHelperCompletion {
            exit_code: Some(c),
            reaped: true,
            terminated: self.terminated.load(Ordering::SeqCst),
            detail: format!("exit {c}"),
        }))
    }

    fn terminate(&self) -> Result<WidgetHelperCompletion> {
        self.channel.shutdown();
        self.terminated.store(true, Ordering::SeqCst);
        let mut guard = self
            .exit_code
            .lock()
            .map_err(|_| PlatformError::WidgetSandbox("exit lock poisoned".into()))?;
        let code = *guard.get_or_insert(1067); // ERROR_PROCESS_ABORTED
        Ok(WidgetHelperCompletion {
            exit_code: Some(code),
            reaped: true,
            terminated: true,
            detail: format!("exit {code}, terminated"),
        })
    }
}

#[derive(Default)]
struct FakeLauncherState {
    supported: bool,
    unsupported_reason: Option<String>,
    launch_error: Option<String>,
    launched_configs: Vec<(u32, WidgetSandboxConfig)>,
    peer_channels: HashMap<u32, FakeWidgetChannel>,
}

/// Deterministic in-memory [`WidgetSandboxLauncher`] for tests.
///
/// Spawns no OS processes, creates no real named pipes or Job Objects, and hands back the
/// peer side of each launched [`FakeWidgetChannel`] via [`Self::take_peer_channel`].
#[derive(Clone)]
pub struct FakeWidgetSandboxLauncher {
    inner: Arc<Mutex<FakeLauncherState>>,
    next_pid: Arc<AtomicU32>,
}

impl Default for FakeWidgetSandboxLauncher {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeWidgetSandboxLauncher {
    /// Creates a supported fake sandbox launcher starting synthetic PIDs at `10_000`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeLauncherState {
                supported: true,
                unsupported_reason: None,
                launch_error: None,
                launched_configs: Vec::new(),
                peer_channels: HashMap::new(),
            })),
            next_pid: Arc::new(AtomicU32::new(10_000)),
        }
    }

    /// Creates a fake launcher that reports sandboxing as unsupported with `reason`.
    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeLauncherState {
                supported: false,
                unsupported_reason: Some(reason.into()),
                launch_error: None,
                launched_configs: Vec::new(),
                peer_channels: HashMap::new(),
            })),
            next_pid: Arc::new(AtomicU32::new(10_000)),
        }
    }

    /// Injects an error to be returned by subsequent [`WidgetSandboxLauncher::launch`] calls.
    pub fn set_launch_error(&self, error: Option<String>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.launch_error = error;
        }
    }

    /// Takes the helper-side [`FakeWidgetChannel`] for synthetic process `pid`.
    #[must_use]
    pub fn take_peer_channel(&self, pid: u32) -> Option<FakeWidgetChannel> {
        self.inner.lock().ok()?.peer_channels.remove(&pid)
    }

    /// Returns all `(pid, WidgetSandboxConfig)` launches recorded so far.
    #[must_use]
    pub fn launched_configs(&self) -> Vec<(u32, WidgetSandboxConfig)> {
        self.inner
            .lock()
            .map(|g| g.launched_configs.clone())
            .unwrap_or_default()
    }
}

impl WidgetSandboxLauncher for FakeWidgetSandboxLauncher {
    fn is_supported(&self) -> bool {
        self.inner.lock().map(|g| g.supported).unwrap_or(false)
    }

    fn unsupported_reason(&self) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.unsupported_reason.clone())
    }

    fn launch(&self, config: &WidgetSandboxConfig) -> Result<Box<dyn WidgetHelperProcess>> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| PlatformError::WidgetSandbox("fake launcher lock poisoned".into()))?;
        if !guard.supported {
            return Err(PlatformError::WidgetSandbox(
                guard
                    .unsupported_reason
                    .clone()
                    .unwrap_or_else(|| "widget helper sandbox unsupported".to_owned()),
            ));
        }
        if let Some(ref err) = guard.launch_error {
            return Err(PlatformError::WidgetSandbox(err.clone()));
        }
        let pid = self.next_pid.fetch_add(1, Ordering::SeqCst);
        let (host_channel, helper_channel) = FakeWidgetChannel::pair(config.max_message_bytes);
        guard.launched_configs.push((pid, config.clone()));
        guard.peer_channels.insert(pid, helper_channel);
        Ok(Box::new(FakeWidgetHelperProcess {
            pid,
            channel: host_channel,
            exit_code: Mutex::new(None),
            terminated: AtomicBool::new(false),
        }))
    }
}

// ============================================================================
// Production Win32 AppContainer / Job-Object Launcher & Named Pipe Stream
// ============================================================================

/// Production Windows sandboxed widget helper launcher using owner-DACL overlapped named pipes,
/// restricted Low-IL AppContainer tokens, Job Object limits/UI restrictions, and process creation
/// mitigation policies (`widget_sandbox_win.cpp`, `widget_channel_win.cpp`, `widget_helper_win.cpp`).
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32WidgetSandboxLauncher;

impl Win32WidgetSandboxLauncher {
    /// Creates a new [`Win32WidgetSandboxLauncher`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl WidgetSandboxLauncher for Win32WidgetSandboxLauncher {
    fn is_supported(&self) -> bool {
        self.unsupported_reason().is_none()
    }

    fn unsupported_reason(&self) -> Option<String> {
        #[cfg(windows)]
        {
            let info = crate::system_info::windows_version();
            if info.major > 10 || (info.major == 10 && info.build >= MIN_WINDOWS_BUILD_FOR_SANDBOX)
            {
                None
            } else {
                Some("the widget helper needs Windows 10 1809 or later".to_owned())
            }
        }
        #[cfg(not(windows))]
        {
            Some("the widget helper sandbox is only available on Windows".to_owned())
        }
    }

    fn launch(&self, config: &WidgetSandboxConfig) -> Result<Box<dyn WidgetHelperProcess>> {
        #[cfg(windows)]
        {
            if let Some(reason) = self.unsupported_reason() {
                return Err(PlatformError::WidgetSandbox(reason));
            }
            win32_impl::launch_sandboxed_helper(config)
        }
        #[cfg(not(windows))]
        {
            let _ = config;
            Err(PlatformError::Unsupported)
        }
    }
}

#[cfg(windows)]
#[allow(dead_code)]
mod win32_impl {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use windows::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_IO_PENDING, ERROR_NO_TOKEN,
        ERROR_PIPE_CONNECTED, ERROR_PROCESS_ABORTED, FreeLibrary, GENERIC_READ, GENERIC_WRITE,
        GetLastError, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree,
        SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, CreateRestrictedToken, CreateWellKnownSid,
        DISABLE_MAX_PRIVILEGE, DuplicateTokenEx, FreeSid, GetLengthSid, GetTokenInformation,
        InitializeAcl, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SECURITY_IMPERSONATION_LEVEL, SID_AND_ATTRIBUTES,
        SetSecurityDescriptorControl, SetSecurityDescriptorDacl, SetTokenInformation,
        TOKEN_ADJUST_DEFAULT, TOKEN_ALL_ACCESS, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
        TOKEN_GROUPS, TOKEN_IMPERSONATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
        TokenGroups, TokenIntegrityLevel, TokenUser, WinLowLabelSid, WinNullSid,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ALL_ACCESS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        FILE_SHARE_MODE, FILE_TYPE_PIPE, GetFileType, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
        WriteFile,
    };
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows::Win32::System::JobObjects::{
        CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOB_OBJECT_UILIMIT, JOBOBJECT_BASIC_UI_RESTRICTIONS,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject,
    };
    use windows::Win32::System::LibraryLoader::{
        GetModuleHandleW, GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
        SetDefaultDllDirectories,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeInfo, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_SERVER_END, PIPE_TYPE_BYTE, PIPE_TYPE_MESSAGE, PIPE_WAIT,
    };
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows::Win32::System::SystemServices::{
        SE_GROUP_ENABLED, SE_GROUP_INTEGRITY, SE_GROUP_USE_FOR_DENY_ONLY,
        SECURITY_DESCRIPTOR_REVISION,
    };
    use windows::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_SUSPENDED, CreateEventW, CreateProcessAsUserW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
        GetCurrentThread, GetExitCodeProcess, InitializeProcThreadAttributeList,
        LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken, OpenThreadToken,
        PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
        PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
        PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, ResetEvent, ResumeThread,
        STARTF_FORCEOFFFEEDBACK, STARTUPINFOEXW, SetEvent, SetThreadToken,
        UpdateProcThreadAttribute, WaitForMultipleObjects, WaitForSingleObject,
    };
    use windows_core::{BOOL, HRESULT, PCSTR, PCWSTR, PWSTR, w};

    use super::{
        ChannelError, FramedWidgetChannel, JobLimitsConfig, PIPE_BUFFER_SIZE, WidgetByteStream,
        WidgetHelperChannel, WidgetHelperCompletion, WidgetHelperProcess, WidgetSandboxConfig,
        compute_sandbox_policies, format_widget_pipe_name, quote_windows_argument,
        verify_job_limits,
    };
    use crate::app_id::win_impl::to_wide;
    use crate::error::{PlatformError, Result};

    const SECURITY_MAX_SID_SIZE: usize = 68;
    const ACCESS_ALLOWED_ACE_HEADER_SIZE: usize = 12;

    /// RAII wrapper around a Win32 `HANDLE`.
    pub(super) struct OwnedHandle(HANDLE);

    // SAFETY: Win32 kernel handles (pipes, events, jobs, tokens, processes) are process-wide and
    // safe to transfer or synchronize across threads.
    unsafe impl Send for OwnedHandle {}
    // SAFETY: Kernel handles are referenced only through thread-safe Win32 APIs.
    unsafe impl Sync for OwnedHandle {}

    impl OwnedHandle {
        fn new(handle: HANDLE) -> Option<Self> {
            if handle.is_invalid() || handle.0.is_null() {
                None
            } else {
                Some(Self(handle))
            }
        }

        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() && !self.0.0.is_null() {
                // SAFETY: Closing our owned Win32 kernel handle once.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    struct WinPipeHandles {
        pipe: OwnedHandle,
        stop: OwnedHandle,
        reading: OwnedHandle,
        writing: OwnedHandle,
    }

    /// Overlapped Win32 named pipe stream (`WinStream` in `widget_channel_win.cpp`).
    pub(super) struct WinPipeStream {
        handles: WinPipeHandles,
        stopped: AtomicBool,
    }

    impl WinPipeStream {
        fn transfer<F>(&self, event: HANDLE, issue: F) -> std::result::Result<u32, ChannelError>
        where
            F: FnOnce(*mut OVERLAPPED) -> windows_core::Result<()>,
        {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(ChannelError::Closed);
            }
            // SAFETY: Resetting our manual-reset event before issuing overlapped I/O.
            unsafe { ResetEvent(event) }.map_err(|e| ChannelError::Io(e.to_string()))?;

            let mut overlapped = OVERLAPPED {
                hEvent: event,
                ..Default::default()
            };

            if let Err(err) = issue(&mut overlapped) {
                // SAFETY: Reading thread-local Win32 error code immediately after failed I/O.
                let last = unsafe { GetLastError() };
                if last != ERROR_IO_PENDING {
                    return Err(ChannelError::PeerDisconnected);
                }
                let wait_handles = [self.handles.stop.raw(), event];
                // SAFETY: Waiting on valid event handles owned by `self`.
                let waited = unsafe { WaitForMultipleObjects(&wait_handles, false, u32::MAX) };
                if waited.0 != WAIT_OBJECT_0.0 + 1 {
                    // SAFETY: Cancelling our overlapped operation on stop or wait failure.
                    let _ = unsafe { CancelIoEx(self.handles.pipe.raw(), Some(&overlapped)) };
                }
                let _ = err;
            }

            let mut transferred = 0u32;
            // SAFETY: Retrieving the completed byte count for `overlapped`.
            let res = unsafe {
                GetOverlappedResult(self.handles.pipe.raw(), &overlapped, &mut transferred, true)
            };
            if res.is_err() || self.stopped.load(Ordering::SeqCst) {
                return Err(if self.stopped.load(Ordering::SeqCst) {
                    ChannelError::Closed
                } else {
                    ChannelError::PeerDisconnected
                });
            }
            Ok(transferred)
        }
    }

    impl WidgetByteStream for WinPipeStream {
        fn read(&self, out: &mut [u8]) -> std::result::Result<usize, ChannelError> {
            if out.is_empty() {
                return Ok(0);
            }
            let chunk_len = out.len().min(u32::MAX as usize);
            let done = self.transfer(self.handles.reading.raw(), |ov| {
                // SAFETY: `out[..chunk_len]` remains valid until `GetOverlappedResult` completes
                // inside `transfer`.
                unsafe {
                    ReadFile(
                        self.handles.pipe.raw(),
                        Some(&mut out[..chunk_len]),
                        None,
                        Some(ov),
                    )
                }
            })?;
            Ok(done as usize)
        }

        fn write_all(&self, mut bytes: &[u8]) -> std::result::Result<(), ChannelError> {
            while !bytes.is_empty() {
                let chunk_len = bytes.len().min(u32::MAX as usize);
                let chunk = &bytes[..chunk_len];
                let done = self.transfer(self.handles.writing.raw(), |ov| {
                    // SAFETY: `chunk` remains valid until `GetOverlappedResult` completes inside
                    // `transfer`.
                    unsafe { WriteFile(self.handles.pipe.raw(), Some(chunk), None, Some(ov)) }
                })?;
                if done == 0 {
                    return Err(ChannelError::PeerDisconnected);
                }
                bytes = &bytes[done as usize..];
            }
            if self.stopped.load(Ordering::SeqCst) {
                Err(ChannelError::Closed)
            } else {
                Ok(())
            }
        }

        fn shutdown(&self) {
            self.stopped.store(true, Ordering::SeqCst);
            // SAFETY: Signaling our owned stop event wakes any blocked `WaitForMultipleObjects`.
            let _ = unsafe { SetEvent(self.handles.stop.raw()) };
        }

        fn is_closed(&self) -> bool {
            self.stopped.load(Ordering::SeqCst)
        }
    }

    fn create_manual_event() -> Result<OwnedHandle> {
        // SAFETY: Creating an unnamed manual-reset initially-unsignaled Win32 event.
        let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|e| PlatformError::from_win("CreateEventW", e))?;
        OwnedHandle::new(h)
            .ok_or_else(|| PlatformError::WidgetSandbox("CreateEventW returned null".into()))
    }

    fn read_token_bytes(
        token: HANDLE,
        class: windows::Win32::Security::TOKEN_INFORMATION_CLASS,
    ) -> Result<Vec<u8>> {
        let mut size = 0u32;
        // SAFETY: Probing required buffer length for `GetTokenInformation`.
        let _ = unsafe { GetTokenInformation(token, class, None, 0, &mut size) };
        if size == 0 {
            return Err(PlatformError::last_win32("GetTokenInformation(size)"));
        }
        let mut buf = vec![0u8; size as usize];
        // SAFETY: `buf` is sized to `size` bytes as reported by the probe call.
        unsafe {
            GetTokenInformation(token, class, Some(buf.as_mut_ptr().cast()), size, &mut size)
        }
        .map_err(|e| PlatformError::from_win("GetTokenInformation", e))?;
        Ok(buf)
    }

    fn well_known_sid(kind: windows::Win32::Security::WELL_KNOWN_SID_TYPE) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; SECURITY_MAX_SID_SIZE];
        let mut size = buf.len() as u32;
        // SAFETY: `buf` is at least `SECURITY_MAX_SID_SIZE` bytes.
        unsafe { CreateWellKnownSid(kind, None, Some(PSID(buf.as_mut_ptr().cast())), &mut size) }
            .map_err(|e| PlatformError::from_win("CreateWellKnownSid", e))?;
        buf.truncate(size as usize);
        Ok(buf)
    }

    /// Creates an owner-DACL duplex overlapped named pipe pair (`CreateWinChannel` in
    /// `widget_channel_win.cpp:201-229`).
    fn create_win_channel(name: &str) -> Result<(WinPipeStream, OwnedHandle)> {
        let stop = create_manual_event()?;
        let reading = create_manual_event()?;
        let writing = create_manual_event()?;

        let mut proc_token = HANDLE::default();
        // SAFETY: Opening the current process token for `TOKEN_QUERY`.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut proc_token) }
            .map_err(|e| PlatformError::from_win("OpenProcessToken", e))?;
        let proc_token = OwnedHandle::new(proc_token)
            .ok_or_else(|| PlatformError::WidgetSandbox("OpenProcessToken returned null".into()))?;
        let user_bytes = read_token_bytes(proc_token.raw(), TokenUser)?;
        // SAFETY: `TokenUser` starts with `TOKEN_USER`.
        let user_sid = unsafe { (*(user_bytes.as_ptr().cast::<TOKEN_USER>())).User.Sid };

        let acl_size =
            std::mem::size_of::<ACL>() + ACCESS_ALLOWED_ACE_HEADER_SIZE + SECURITY_MAX_SID_SIZE;
        let mut acl_buf = vec![0u8; acl_size];
        let acl_ptr = acl_buf.as_mut_ptr().cast::<ACL>();
        let mut sd = SECURITY_DESCRIPTOR::default();
        let psd = PSECURITY_DESCRIPTOR((&mut sd as *mut SECURITY_DESCRIPTOR).cast());

        // SAFETY: Initializing an owner-only protected DACL in stack/heap buffers kept alive
        // across `CreateNamedPipeW`.
        unsafe {
            InitializeAcl(acl_ptr, acl_size as u32, ACL_REVISION)
                .map_err(|e| PlatformError::from_win("InitializeAcl", e))?;
            AddAccessAllowedAce(acl_ptr, ACL_REVISION, FILE_ALL_ACCESS.0, user_sid)
                .map_err(|e| PlatformError::from_win("AddAccessAllowedAce", e))?;
            InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION)
                .map_err(|e| PlatformError::from_win("InitializeSecurityDescriptor", e))?;
            SetSecurityDescriptorDacl(psd, true, Some(acl_ptr), false)
                .map_err(|e| PlatformError::from_win("SetSecurityDescriptorDacl", e))?;
            SetSecurityDescriptorControl(psd, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
                .map_err(|e| PlatformError::from_win("SetSecurityDescriptorControl", e))?;
        }

        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd.0,
            bInheritHandle: BOOL(0),
        };
        let wide_name = to_wide(name);
        // SAFETY: `wide_name` is null-terminated UTF-16 and `sa` points to live `sd`/`acl_buf`.
        let server_raw = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_name.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                0,
                Some(&sa),
            )
        };
        let pipe = OwnedHandle::new(server_raw)
            .ok_or_else(|| PlatformError::last_win32("CreateNamedPipeW"))?;

        // SAFETY: Opening the client end of the private named pipe we just created.
        let client_raw = unsafe {
            CreateFileW(
                PCWSTR(wide_name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| PlatformError::from_win("CreateFileW", e))?;
        let child = OwnedHandle::new(client_raw).ok_or_else(|| {
            PlatformError::WidgetSandbox("CreateFileW returned invalid handle".into())
        })?;

        let mut ov = OVERLAPPED {
            hEvent: reading.raw(),
            ..Default::default()
        };
        // SAFETY: Completing server-side connection after our client opened the pipe.
        let connect_res = unsafe { ConnectNamedPipe(pipe.raw(), Some(&mut ov)) };
        if connect_res.is_ok() {
            return Err(PlatformError::WidgetSandbox(
                "ConnectNamedPipe accepted an unexpected client".into(),
            ));
        }
        // SAFETY: Inspecting thread-local error immediately after `ConnectNamedPipe`.
        let err = unsafe { GetLastError() };
        if err != ERROR_PIPE_CONNECTED {
            if err == ERROR_IO_PENDING {
                // SAFETY: Cancelling pending connect before returning failure.
                let _ = unsafe { CancelIoEx(pipe.raw(), Some(&ov)) };
                let mut done = 0u32;
                let _ = unsafe { GetOverlappedResult(pipe.raw(), &ov, &mut done, true) };
            }
            return Err(PlatformError::Win32 {
                context: "ConnectNamedPipe",
                code: err.0,
                message: format!("ConnectNamedPipe failed with error {}", err.0),
            });
        }

        let stream = WinPipeStream {
            handles: WinPipeHandles {
                pipe,
                stop,
                reading,
                writing,
            },
            stopped: AtomicBool::new(false),
        };
        Ok((stream, child))
    }

    /// Adopts an inherited client named pipe handle inside the helper (`AdoptWinChannel` in
    /// `widget_channel_win.cpp:231-245`).
    pub(super) fn adopt_win_channel(pipe: OwnedHandle) -> Result<WinPipeStream> {
        let raw = pipe.raw();
        let mut flags = NAMED_PIPE_MODE::default();
        // SAFETY: Validating the inherited handle is a byte-mode client named pipe and clearing
        // its inheritance flag.
        let valid = unsafe {
            GetFileType(raw) == FILE_TYPE_PIPE
                && GetNamedPipeInfo(raw, Some(&mut flags), None, None, None).is_ok()
                && (flags.0 & (PIPE_SERVER_END.0 | PIPE_TYPE_MESSAGE.0)) == 0
                && SetHandleInformation(raw, HANDLE_FLAG_INHERIT.0, Default::default()).is_ok()
        };
        if !valid {
            return Err(PlatformError::WidgetSandbox(
                "inherited handle is not a valid client byte pipe".into(),
            ));
        }
        let stop = create_manual_event()?;
        let reading = create_manual_event()?;
        let writing = create_manual_event()?;
        Ok(WinPipeStream {
            handles: WinPipeHandles {
                pipe,
                stop,
                reading,
                writing,
            },
            stopped: AtomicBool::new(false),
        })
    }

    /// Helper-process entry wrapper: restricts DLL search order to System32, adopts the
    /// inherited client byte pipe, and returns a framed [`WidgetHelperChannel`].
    pub(super) fn adopt_inherited_channel(
        raw_handle: usize,
        max_message_bytes: usize,
    ) -> Result<Box<dyn WidgetHelperChannel>> {
        // SAFETY: Restricting default DLL search directories to System32 before loading any
        // optional modules inside the sandboxed helper process.
        unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .map_err(|e| PlatformError::from_win("SetDefaultDllDirectories", e))?;
        let owned = OwnedHandle::new(HANDLE(raw_handle as *mut c_void))
            .ok_or_else(|| PlatformError::WidgetSandbox("invalid inherited pipe handle".into()))?;
        let stream = adopt_win_channel(owned)?;
        Ok(Box::new(FramedWidgetChannel::new(
            stream,
            max_message_bytes,
        )))
    }

    /// Drops the initial thread impersonation token and verifies the helper's sandbox Job Object
    /// and token invariants (`LockDownHelper` in `widget_sandbox_win.cpp`).
    pub(super) fn lockdown_helper(expected_job: &JobLimitsConfig) -> Result<()> {
        // SAFETY: Reverting the current thread to the process's locked-down primary AppContainer
        // token by clearing its startup impersonation token.
        unsafe { SetThreadToken(None, None) }
            .map_err(|e| PlatformError::from_win("SetThreadToken(revert)", e))?;
        verify_no_thread_token().map_err(PlatformError::WidgetSandbox)?;
        verify_current_job(expected_job).map_err(PlatformError::WidgetSandbox)?;
        Ok(())
    }

    /// Creates a configured Job Object (`CreateJob` in `widget_sandbox_win.cpp:888-942`).
    fn create_sandbox_job(
        config: &JobLimitsConfig,
    ) -> Result<(OwnedHandle, PROCESS_CREATION_FLAGS)> {
        let mut in_job = BOOL(0);
        // SAFETY: Querying whether the current host process is already inside a Job Object.
        unsafe { IsProcessInJob(GetCurrentProcess(), None, &mut in_job) }
            .map_err(|e| PlatformError::from_win("IsProcessInJob", e))?;

        let mut creation_flags = PROCESS_CREATION_FLAGS(0);
        if in_job.as_bool() {
            let mut own = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            // SAFETY: Querying extended limit information for the current job.
            unsafe {
                QueryInformationJobObject(
                    None,
                    JobObjectExtendedLimitInformation,
                    (&mut own as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    None,
                )
            }
            .map_err(|e| PlatformError::from_win("QueryInformationJobObject", e))?;
            let flags = own.BasicLimitInformation.LimitFlags;
            if flags.contains(JOB_OBJECT_LIMIT_BREAKAWAY_OK)
                && !flags.contains(JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK)
            {
                creation_flags = CREATE_BREAKAWAY_FROM_JOB;
            }
        }

        // SAFETY: Creating an unnamed Job Object.
        let job_raw = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|e| PlatformError::from_win("CreateJobObjectW", e))?;
        let job = OwnedHandle::new(job_raw)
            .ok_or_else(|| PlatformError::WidgetSandbox("CreateJobObjectW returned null".into()))?;

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT(config.limit_flags);
        limits.BasicLimitInformation.ActiveProcessLimit = config.active_process_limit;
        let restrictions = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT(config.ui_restrictions),
        };

        // SAFETY: Applying extended limits and basic UI restrictions to the newly created job.
        unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|e| PlatformError::from_win("SetInformationJobObject(limits)", e))?;
            SetInformationJobObject(
                job.raw(),
                JobObjectBasicUIRestrictions,
                (&restrictions as *const JOBOBJECT_BASIC_UI_RESTRICTIONS).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            )
            .map_err(|e| PlatformError::from_win("SetInformationJobObject(ui)", e))?;
        }
        Ok((job, creation_flags))
    }

    /// Queries and verifies that the current process is inside a sandbox Job Object (`VerifyJob`).
    pub(super) fn verify_current_job(
        expected: &JobLimitsConfig,
    ) -> std::result::Result<(), String> {
        let mut in_job = BOOL(0);
        // SAFETY: Querying job membership for the current process.
        unsafe { IsProcessInJob(GetCurrentProcess(), None, &mut in_job) }
            .map_err(|e| format!("IsProcessInJob failed: {e}"))?;
        if !in_job.as_bool() {
            return verify_job_limits(false, 0, 0, 0, expected);
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let mut ui = JOBOBJECT_BASIC_UI_RESTRICTIONS::default();
        // SAFETY: Querying current job limits and UI restrictions.
        unsafe {
            QueryInformationJobObject(
                None,
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                None,
            )
            .map_err(|e| format!("QueryInformationJobObject(limits) failed: {e}"))?;
            QueryInformationJobObject(
                None,
                JobObjectBasicUIRestrictions,
                (&mut ui as *mut JOBOBJECT_BASIC_UI_RESTRICTIONS).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
                None,
            )
            .map_err(|e| format!("QueryInformationJobObject(ui) failed: {e}"))?;
        }
        verify_job_limits(
            true,
            limits.BasicLimitInformation.LimitFlags.0,
            limits.BasicLimitInformation.ActiveProcessLimit,
            ui.UIRestrictionsClass.0,
            expected,
        )
    }

    /// Verifies the calling thread has no impersonation token (`VerifyNoThreadToken`).
    pub(super) fn verify_no_thread_token() -> std::result::Result<(), String> {
        let mut token = HANDLE::default();
        // SAFETY: Checking whether the current thread holds an impersonation token.
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token) }.is_ok() {
            let _guard = OwnedHandle::new(token);
            return Err("the thread still has a token".to_owned());
        }
        // SAFETY: Reading thread-local error immediately after `OpenThreadToken`.
        let err = unsafe { GetLastError() };
        if err == ERROR_NO_TOKEN {
            Ok(())
        } else {
            Err(format!("OpenThreadToken failed: error {}", err.0))
        }
    }

    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: *mut c_void,
        object_name: *mut c_void,
        attributes: u32,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }

    type FnDeriveAppContainerSid = unsafe extern "system" fn(PCWSTR, *mut PSID) -> HRESULT;
    type FnNtCreateLowBoxToken = unsafe extern "system" fn(
        *mut HANDLE,
        HANDLE,
        u32,
        *mut ObjectAttributes,
        PSID,
        u32,
        *const SID_AND_ATTRIBUTES,
        u32,
        *const HANDLE,
    ) -> i32;

    fn derive_app_container_sid(name: &str) -> Result<Vec<u8>> {
        // SAFETY: Loading `userenv.dll` strictly from System32.
        let module =
            unsafe { LoadLibraryExW(w!("userenv.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(|e| PlatformError::from_win("LoadLibraryExW(userenv.dll)", e))?;
        struct LibGuard(windows::Win32::Foundation::HMODULE);
        impl Drop for LibGuard {
            fn drop(&mut self) {
                // SAFETY: Releasing our own library handle once.
                let _ = unsafe { FreeLibrary(self.0) };
            }
        }
        let _guard = LibGuard(module);

        // SAFETY: Resolving `DeriveAppContainerSidFromAppContainerName`.
        let proc = unsafe {
            GetProcAddress(
                module,
                PCSTR(c"DeriveAppContainerSidFromAppContainerName".as_ptr().cast()),
            )
        }
        .ok_or_else(|| {
            PlatformError::WidgetSandbox(
                "DeriveAppContainerSidFromAppContainerName is not available".into(),
            )
        })?;
        // SAFETY: Signature matches `<userenv.h>`.
        let derive: FnDeriveAppContainerSid = unsafe { std::mem::transmute(proc) };
        let wide = to_wide(name);
        let mut sid = PSID::default();
        // SAFETY: `wide` is null-terminated UTF-16 and `sid` receives a heap SID freed with `FreeSid`.
        let hr = unsafe { derive(PCWSTR(wide.as_ptr()), &mut sid) };
        if hr.is_err() || sid.0.is_null() {
            return Err(PlatformError::Win32 {
                context: "DeriveAppContainerSidFromAppContainerName",
                code: hr.0 as u32,
                message: format!("failed with HRESULT 0x{:08x}", hr.0 as u32),
            });
        }
        // SAFETY: `sid` is a valid Windows SID; copying its bytes before freeing.
        let len = unsafe { GetLengthSid(sid) } as usize;
        let bytes = unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), len) }.to_vec();
        // SAFETY: Releasing the SID allocated by `DeriveAppContainerSidFromAppContainerName`.
        let _ = unsafe { FreeSid(sid) };
        Ok(bytes)
    }

    fn create_low_box_token(base: HANDLE, package_sid: &[u8]) -> Result<OwnedHandle> {
        // SAFETY: `ntdll.dll` is always mapped in every Windows process.
        let ntdll = unsafe { GetModuleHandleW(w!("ntdll.dll")) }
            .map_err(|e| PlatformError::from_win("GetModuleHandleW(ntdll.dll)", e))?;
        // SAFETY: Resolving `NtCreateLowBoxToken` from `ntdll.dll`.
        let proc = unsafe { GetProcAddress(ntdll, PCSTR(c"NtCreateLowBoxToken".as_ptr().cast())) }
            .ok_or_else(|| {
                PlatformError::WidgetSandbox("NtCreateLowBoxToken is not available".into())
            })?;
        // SAFETY: Signature matches Windows NT `NtCreateLowBoxToken`.
        let create_low_box: FnNtCreateLowBoxToken = unsafe { std::mem::transmute(proc) };

        let mut attrs = ObjectAttributes {
            length: std::mem::size_of::<ObjectAttributes>() as u32,
            root_directory: std::ptr::null_mut(),
            object_name: std::ptr::null_mut(),
            attributes: 0,
            security_descriptor: std::ptr::null_mut(),
            security_quality_of_service: std::ptr::null_mut(),
        };
        let mut token = HANDLE::default();
        // SAFETY: Invoking `NtCreateLowBoxToken` with zero capabilities and a valid package SID.
        let status = unsafe {
            create_low_box(
                &mut token,
                base,
                TOKEN_ALL_ACCESS.0,
                &mut attrs,
                PSID(package_sid.as_ptr() as *mut c_void),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
            )
        };
        if status < 0 {
            return Err(PlatformError::Win32 {
                context: "NtCreateLowBoxToken",
                code: status as u32,
                message: format!("NTSTATUS 0x{:08x}", status as u32),
            });
        }
        OwnedHandle::new(token)
            .ok_or_else(|| PlatformError::WidgetSandbox("NtCreateLowBoxToken returned null".into()))
    }

    fn create_low_restricted_token(
        source: HANDLE,
        disable: &[SID_AND_ATTRIBUTES],
        restricting: &[SID_AND_ATTRIBUTES],
        low_sid: &[u8],
    ) -> Result<OwnedHandle> {
        let mut raw = HANDLE::default();
        let disable_opt = (!disable.is_empty()).then_some(disable);
        let restrict_opt = (!restricting.is_empty()).then_some(restricting);
        // SAFETY: Creating a restricted token from `source` with `DISABLE_MAX_PRIVILEGE`.
        unsafe {
            CreateRestrictedToken(
                source,
                DISABLE_MAX_PRIVILEGE,
                disable_opt,
                None,
                restrict_opt,
                &mut raw,
            )
        }
        .map_err(|e| PlatformError::from_win("CreateRestrictedToken", e))?;
        let token = OwnedHandle::new(raw).ok_or_else(|| {
            PlatformError::WidgetSandbox("CreateRestrictedToken returned null".into())
        })?;

        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: PSID(low_sid.as_ptr() as *mut c_void),
                Attributes: SE_GROUP_INTEGRITY as u32,
            },
        };
        // SAFETY: Setting Low integrity level on our newly created restricted token.
        unsafe {
            SetTokenInformation(
                token.raw(),
                TokenIntegrityLevel,
                (&label as *const TOKEN_MANDATORY_LABEL).cast(),
                (std::mem::size_of::<TOKEN_MANDATORY_LABEL>() + low_sid.len()) as u32,
            )
        }
        .map_err(|e| PlatformError::from_win("SetTokenInformation(LowIL)", e))?;
        Ok(token)
    }

    fn create_sandbox_tokens(app_container_name: &str) -> Result<(OwnedHandle, OwnedHandle)> {
        let mut source_raw = HANDLE::default();
        // SAFETY: Opening the current process token to derive lockdown and initial tokens.
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
                &mut source_raw,
            )
        }
        .map_err(|e| PlatformError::from_win("OpenProcessToken", e))?;
        let source = OwnedHandle::new(source_raw)
            .ok_or_else(|| PlatformError::WidgetSandbox("OpenProcessToken returned null".into()))?;

        let user_bytes = read_token_bytes(source.raw(), TokenUser)?;
        let groups_bytes = read_token_bytes(source.raw(), TokenGroups)?;
        let low_sid = well_known_sid(WinLowLabelSid)?;
        let null_sid = well_known_sid(WinNullSid)?;
        let package_sid = derive_app_container_sid(app_container_name)?;

        // SAFETY: `user_bytes` holds `TOKEN_USER`; `groups_bytes` holds `TOKEN_GROUPS`.
        let user_sid = unsafe { (*(user_bytes.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        let groups_ptr = groups_bytes.as_ptr().cast::<TOKEN_GROUPS>();
        let group_slice = unsafe {
            std::slice::from_raw_parts(
                (*groups_ptr).Groups.as_ptr(),
                (*groups_ptr).GroupCount as usize,
            )
        };

        let mut disable = vec![SID_AND_ATTRIBUTES {
            Sid: user_sid,
            Attributes: 0,
        }];
        let mut restricting = vec![SID_AND_ATTRIBUTES {
            Sid: user_sid,
            Attributes: 0,
        }];
        for g in group_slice {
            if (g.Attributes & (SE_GROUP_INTEGRITY as u32)) == 0 {
                disable.push(SID_AND_ATTRIBUTES {
                    Sid: g.Sid,
                    Attributes: 0,
                });
            }
            if (g.Attributes & (SE_GROUP_ENABLED as u32)) != 0
                && (g.Attributes
                    & ((SE_GROUP_USE_FOR_DENY_ONLY as u32) | (SE_GROUP_INTEGRITY as u32)))
                    == 0
            {
                restricting.push(SID_AND_ATTRIBUTES {
                    Sid: g.Sid,
                    Attributes: 0,
                });
            }
        }
        let null_only = [SID_AND_ATTRIBUTES {
            Sid: PSID(null_sid.as_ptr() as *mut c_void),
            Attributes: 0,
        }];

        let lockdown = create_low_restricted_token(source.raw(), &disable, &null_only, &low_sid)?;
        let restricted = create_low_restricted_token(source.raw(), &[], &restricting, &low_sid)?;
        let boxed_lockdown = create_low_box_token(lockdown.raw(), &package_sid)?;
        let boxed_restricted = create_low_box_token(restricted.raw(), &package_sid)?;

        let mut initial_raw = HANDLE::default();
        // SAFETY: Duplicating `boxed_restricted` as an impersonation token for helper startup.
        unsafe {
            DuplicateTokenEx(
                boxed_restricted.raw(),
                TOKEN_IMPERSONATE | TOKEN_QUERY,
                None,
                SECURITY_IMPERSONATION_LEVEL(2), // SecurityImpersonation
                windows::Win32::Security::TokenImpersonation,
                &mut initial_raw,
            )
        }
        .map_err(|e| PlatformError::from_win("DuplicateTokenEx", e))?;
        let initial = OwnedHandle::new(initial_raw)
            .ok_or_else(|| PlatformError::WidgetSandbox("DuplicateTokenEx returned null".into()))?;

        Ok((boxed_lockdown, initial))
    }

    struct Win32WidgetHelperProcess {
        pid: u32,
        process: OwnedHandle,
        job: OwnedHandle,
        channel: FramedWidgetChannel<WinPipeStream>,
    }

    impl WidgetHelperProcess for Win32WidgetHelperProcess {
        fn pid(&self) -> u32 {
            self.pid
        }

        fn channel(&self) -> &dyn WidgetHelperChannel {
            &self.channel
        }

        fn wait_exit(&self, timeout: Duration) -> Result<Option<WidgetHelperCompletion>> {
            let ms = timeout.as_millis().min((u32::MAX - 1) as u128) as u32;
            // SAFETY: Waiting on our owned child process handle.
            let waited = unsafe { WaitForSingleObject(self.process.raw(), ms) };
            if waited == WAIT_OBJECT_0 {
                let mut code = 0u32;
                // SAFETY: Reading exit code of the exited child process.
                unsafe { GetExitCodeProcess(self.process.raw(), &mut code) }
                    .map_err(|e| PlatformError::from_win("GetExitCodeProcess", e))?;
                return Ok(Some(WidgetHelperCompletion {
                    exit_code: Some(code),
                    reaped: true,
                    terminated: false,
                    detail: format!("exit {code}"),
                }));
            }
            if waited == WAIT_TIMEOUT {
                return Ok(None);
            }
            Err(PlatformError::last_win32("WaitForSingleObject"))
        }

        fn terminate(&self) -> Result<WidgetHelperCompletion> {
            self.channel.shutdown();
            // SAFETY: Terminating the sandbox Job Object kills the helper and any child.
            let _ = unsafe { TerminateJobObject(self.job.raw(), ERROR_PROCESS_ABORTED.0) };
            let _ = unsafe { WaitForSingleObject(self.process.raw(), 1000) };
            let mut code = ERROR_PROCESS_ABORTED.0;
            // SAFETY: Reading exit code after job termination.
            let _ = unsafe { GetExitCodeProcess(self.process.raw(), &mut code) };
            Ok(WidgetHelperCompletion {
                exit_code: Some(code),
                reaped: true,
                terminated: true,
                detail: format!("exit {code}, terminated"),
            })
        }
    }

    pub(super) fn launch_sandboxed_helper(
        config: &WidgetSandboxConfig,
    ) -> Result<Box<dyn WidgetHelperProcess>> {
        let mut rand_bytes = [0u8; 16];
        getrandom::getrandom(&mut rand_bytes)
            .map_err(|e| PlatformError::WidgetSandbox(format!("RNG failed: {e}")))?;
        let rand_hi = u64::from_le_bytes(rand_bytes[..8].try_into().unwrap_or([0; 8]));
        let rand_lo = u64::from_le_bytes(rand_bytes[8..].try_into().unwrap_or([0; 8]));
        let pipe_name = format_widget_pipe_name(std::process::id(), rand_hi, rand_lo);

        let (stream, child_pipe) = create_win_channel(&pipe_name)?;
        let (lockdown_token, initial_token) = create_sandbox_tokens(&config.app_container_name)?;
        let (job, job_creation_flags) = create_sandbox_job(&config.job_limits)?;

        let mut inheritable_pipe_raw = HANDLE::default();
        // SAFETY: Duplicating the client pipe handle as inheritable for `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                child_pipe.raw(),
                GetCurrentProcess(),
                &mut inheritable_pipe_raw,
                0,
                true,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .map_err(|e| PlatformError::from_win("DuplicateHandle", e))?;
        let inheritable_pipe = OwnedHandle::new(inheritable_pipe_raw)
            .ok_or_else(|| PlatformError::WidgetSandbox("DuplicateHandle returned null".into()))?;

        let policies = compute_sandbox_policies(cfg!(target_pointer_width = "64"), false);
        let mut inherited_handles = [inheritable_pipe.raw()];
        let mut jobs = [job.raw()];
        let mitigations = policies.creation;
        let child_process = policies.child_process;
        let all_app_packages = policies.all_application_packages;

        let attr_count = 5u32;
        let mut list_size = 0usize;
        // SAFETY: Probing required attribute list buffer size.
        let _ =
            unsafe { InitializeProcThreadAttributeList(None, attr_count, None, &mut list_size) };
        if list_size == 0 {
            return Err(PlatformError::last_win32(
                "InitializeProcThreadAttributeList",
            ));
        }
        let mut list_buf = vec![0u8; list_size];
        let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(list_buf.as_mut_ptr().cast());
        // SAFETY: Initializing the attribute list inside `list_buf`.
        unsafe {
            InitializeProcThreadAttributeList(Some(attr_list), attr_count, None, &mut list_size)
        }
        .map_err(|e| PlatformError::from_win("InitializeProcThreadAttributeList", e))?;

        struct AttrListGuard(LPPROC_THREAD_ATTRIBUTE_LIST);
        impl Drop for AttrListGuard {
            fn drop(&mut self) {
                // SAFETY: Releasing attribute list initialized above.
                unsafe { DeleteProcThreadAttributeList(self.0) };
            }
        }
        let _attr_guard = AttrListGuard(attr_list);

        // SAFETY: All pointers remain live until after `CreateProcessAsUserW` returns.
        unsafe {
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(inherited_handles.as_mut_ptr().cast()),
                std::mem::size_of_val(&inherited_handles),
                None,
                None,
            )
            .map_err(|e| PlatformError::from_win("UpdateProcThreadAttribute(HANDLE_LIST)", e))?;
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY as usize,
                Some(mitigations.as_ptr().cast()),
                policies.creation_size,
                None,
                None,
            )
            .map_err(|e| PlatformError::from_win("UpdateProcThreadAttribute(MITIGATION)", e))?;
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY as usize,
                Some((&child_process as *const u32).cast()),
                std::mem::size_of::<u32>(),
                None,
                None,
            )
            .map_err(|e| PlatformError::from_win("UpdateProcThreadAttribute(CHILD_PROCESS)", e))?;
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY as usize,
                Some((&all_app_packages as *const u32).cast()),
                std::mem::size_of::<u32>(),
                None,
                None,
            )
            .map_err(|e| {
                PlatformError::from_win("UpdateProcThreadAttribute(ALL_APP_PACKAGES)", e)
            })?;
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some(jobs.as_mut_ptr().cast()),
                std::mem::size_of_val(&jobs),
                None,
                None,
            )
            .map_err(|e| PlatformError::from_win("UpdateProcThreadAttribute(JOB_LIST)", e))?;
        }

        let mut sysdir = vec![0u16; 1024];
        // SAFETY: Querying Windows System32 directory for neutral helper working directory.
        let sysdir_len = unsafe { GetSystemDirectoryW(Some(&mut sysdir)) } as usize;
        if sysdir_len == 0 || sysdir_len >= sysdir.len() {
            return Err(PlatformError::last_win32("GetSystemDirectoryW"));
        }
        sysdir.truncate(sysdir_len);
        sysdir.push(0);

        let exe_str = config.executable.to_string_lossy();
        let exe_wide = to_wide(exe_str.as_ref());
        let mut cmd = format!("\"{}\" {}", exe_str, inheritable_pipe.raw().0 as usize);
        for arg in &config.arguments {
            cmd.push(' ');
            cmd.push_str(&quote_windows_argument(arg));
        }
        let mut cmd_wide = to_wide(&cmd);

        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_FORCEOFFFEEDBACK;
        startup.lpAttributeList = attr_list;

        let mut proc_info = PROCESS_INFORMATION::default();
        // SAFETY: Spawning suspended sandboxed helper under lockdown token + job + mitigations.
        unsafe {
            CreateProcessAsUserW(
                Some(lockdown_token.raw()),
                PCWSTR(exe_wide.as_ptr()),
                Some(PWSTR(cmd_wide.as_mut_ptr())),
                None,
                None,
                true,
                job_creation_flags | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
                None,
                PCWSTR(sysdir.as_ptr()),
                &startup.StartupInfo,
                &mut proc_info,
            )
        }
        .map_err(|e| PlatformError::from_win("CreateProcessAsUserW", e))?;

        let process = OwnedHandle::new(proc_info.hProcess).ok_or_else(|| {
            PlatformError::WidgetSandbox("CreateProcessAsUserW returned null process".into())
        })?;
        let thread = OwnedHandle::new(proc_info.hThread).ok_or_else(|| {
            PlatformError::WidgetSandbox("CreateProcessAsUserW returned null thread".into())
        })?;

        // Attach the initial impersonation token so the helper can load its DLLs before lockdown.
        let thread_raw = thread.raw();
        // SAFETY: Setting the initial impersonation token on the suspended helper thread.
        if let Err(err) = unsafe { SetThreadToken(Some(&thread_raw), Some(initial_token.raw())) } {
            let _ = unsafe { TerminateJobObject(job.raw(), ERROR_PROCESS_ABORTED.0) };
            return Err(PlatformError::from_win("SetThreadToken", err));
        }

        // SAFETY: Resuming the suspended helper main thread.
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            let _ = unsafe { TerminateJobObject(job.raw(), ERROR_PROCESS_ABORTED.0) };
            return Err(PlatformError::last_win32("ResumeThread"));
        }

        // Silence unused helper import if not referenced directly in this path.
        let _ = (INVALID_HANDLE_VALUE, LocalFree);

        Ok(Box::new(Win32WidgetHelperProcess {
            pid: proc_info.dwProcessId,
            process,
            job,
            channel: FramedWidgetChannel::new(stream, config.max_message_bytes),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_widget_channel_round_trips_frames_and_enforces_limit() {
        let (host, helper) = FakeWidgetChannel::pair(64);
        host.send_frame(b"hello widget").unwrap();
        assert_eq!(helper.recv_frame().unwrap(), b"hello widget");

        helper.send_frame(b"{\"type\":\"ready\"}").unwrap();
        assert_eq!(host.recv_frame().unwrap(), b"{\"type\":\"ready\"}");

        // Empty payload is a valid 0-byte frame
        host.send_frame(b"").unwrap();
        assert_eq!(helper.recv_frame().unwrap(), b"");

        // Oversized payload is rejected on send
        let too_big = vec![0xAA; 65];
        assert_eq!(
            host.send_frame(&too_big),
            Err(ChannelError::MessageTooLarge { size: 65, max: 64 })
        );

        // Oversized length header from peer is rejected on receive
        helper.inject_raw_bytes(&128u32.to_le_bytes()).unwrap();
        assert_eq!(
            host.recv_frame(),
            Err(ChannelError::MessageTooLarge { size: 128, max: 64 })
        );
    }

    #[test]
    fn fake_widget_channel_reports_peer_disconnect_and_truncation() {
        let (host, helper) = FakeWidgetChannel::pair(1024);
        helper.shutdown();
        assert_eq!(host.recv_frame(), Err(ChannelError::PeerDisconnected));
        assert_eq!(
            host.send_frame(b"after disconnect"),
            Err(ChannelError::PeerDisconnected)
        );

        // Truncated payload before peer drops
        let (host2, helper2) = FakeWidgetChannel::pair(1024);
        let mut partial = Vec::new();
        partial.extend_from_slice(&10u32.to_le_bytes());
        partial.extend_from_slice(b"1234");
        helper2.inject_raw_bytes(&partial).unwrap();
        drop(helper2);
        assert_eq!(
            host2.recv_frame(),
            Err(ChannelError::TruncatedFrame {
                expected: 10,
                actual: 4
            })
        );
    }

    #[test]
    fn policy_job_and_quoting_helpers_match_tdesktop() {
        let policies_x64 = compute_sandbox_policies(true, true);
        assert_eq!(policies_x64.creation_size, 16);
        assert_ne!(
            policies_x64.creation[0] & MITIGATION_HIGH_ENTROPY_ASLR_ALWAYS_ON,
            0
        );
        assert_ne!(
            policies_x64.creation[1] & MITIGATION2_CET_USER_SHADOW_STACKS_ALWAYS_ON,
            0
        );
        assert_eq!(policies_x64.child_process, CHILD_PROCESS_RESTRICTED);
        assert_eq!(
            policies_x64.all_application_packages,
            ALL_APPLICATION_PACKAGES_OPT_OUT
        );

        let expected_job = JobLimitsConfig::default();
        assert!(
            verify_job_limits(
                true,
                DEFAULT_JOB_LIMIT_FLAGS,
                1,
                DEFAULT_JOB_UI_RESTRICTIONS,
                &expected_job
            )
            .is_ok()
        );
        assert!(verify_job_limits(false, 0, 0, 0, &expected_job).is_err());

        assert_eq!(quote_windows_argument("simple"), "simple");
        assert_eq!(quote_windows_argument(""), "\"\"");
        assert_eq!(quote_windows_argument("with space"), "\"with space\"");
        assert_eq!(quote_windows_argument(r#"a"b"#), r#""a\"b""#);
        assert_eq!(
            quote_windows_argument(r"C:\Dir With Space\"),
            r#""C:\Dir With Space\\""#
        );

        assert_eq!(
            format_widget_pipe_name(4242, 0x1, 0x2),
            r"\\.\pipe\telegram-widget-helper-4242-00000000000000010000000000000002"
        );
        assert_eq!(parse_inherited_handle_token("1234"), Some(1234));
        assert_eq!(parse_inherited_handle_token("0"), None);
        assert_eq!(parse_inherited_handle_token("0123"), None);
        assert_eq!(parse_inherited_handle_token(&usize::MAX.to_string()), None);
    }

    #[test]
    fn resolve_widget_helper_path_and_serve_framed_ipc_loop() {
        let resolved =
            resolve_widget_helper_path(Path::new(r"C:\Program Files\Rustgram\rustgram.exe"));
        assert_eq!(
            resolved,
            PathBuf::from(r"C:\Program Files\Rustgram").join(WIDGET_HELPER_EXE_NAME)
        );
        let fallback = resolve_widget_helper_path_with_fallback(
            Some(Path::new("rustgram.exe")),
            Path::new(r"D:\Install\Rustgram"),
        );
        assert_eq!(
            fallback,
            PathBuf::from(r"D:\Install\Rustgram").join(WIDGET_HELPER_EXE_NAME)
        );

        let (host, helper) = FakeWidgetChannel::pair_default();
        let worker = std::thread::spawn(move || serve_widget_helper_channel(&helper));

        // Open control frame ([0xFF, 1, dpr]) -> Content ([0xFF, 64]) + Loaded ([0xFF, 65])
        let mut open = vec![0xFF, 1];
        open.extend_from_slice(&2.0_f64.to_le_bytes());
        host.send_frame(&open).expect("send open");
        assert_eq!(host.recv_frame().expect("recv content"), vec![0xFF, 64]);
        assert_eq!(host.recv_frame().expect("recv loaded"), vec![0xFF, 65]);

        // JSON bridge ready + frame probes
        host.send_frame(br#"{"type":"ready"}"#).expect("send ready");
        assert_eq!(
            host.recv_frame().expect("recv ready"),
            br#"{"type":"ready"}"#
        );
        host.send_frame(br#"{"type":"frame","time":16.0}"#)
            .expect("send frame");
        assert_eq!(
            host.recv_frame().expect("recv ops"),
            br#"{"type":"ops","count":0}"#
        );

        // Plain ping -> pong
        host.send_frame(b"ping").expect("send ping");
        assert_eq!(host.recv_frame().expect("recv pong"), b"pong");

        // Stopped control frame terminates helper cleanly.
        host.send_frame(&[0xFF, 66, 0, 0, 0, 0])
            .expect("send stopped");
        worker
            .join()
            .expect("helper thread join")
            .expect("helper exited cleanly");
    }
}
