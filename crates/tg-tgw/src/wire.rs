//! Hosted binary control wire protocol and JSON bridge framing between host and sandboxed
//! widget renderer.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_hosted_wire.h`, `tgw_renderer_rules.h`,
//! `tgw_renderer_platform.cpp`, and `tgw_hosted_glue.cpp`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::bridge::WidgetRequestOutcome;
use crate::error::{Result, TgwError};
use crate::model::{ContentSafeArea, WidgetTheme};

/// Control frame lead byte (`HostedWire::kTag` in `tgw_hosted_wire.h:40`).
pub const WIRE_TAG: u8 = 0xFF;

/// Control frame header length (`kTag` + `Kind`, `tgw_hosted_wire.h:41`).
pub const WIRE_HEADER_BYTES: usize = 2;

/// 64-bit little-endian number size (`tgw_hosted_wire.h:42`).
pub const WIRE_NUMBER_BYTES: usize = 8;

/// 32-bit little-endian length field size (`tgw_hosted_wire.h:43`).
pub const WIRE_LENGTH_BYTES: usize = 4;

/// Maximum message size on the hosted channel (32 MiB, `tgw_hosted_wire.h:44`).
pub const MAX_MESSAGE_BYTES: usize = 32 << 20;

/// Maximum display list commands in one frame (`tgw_hosted_wire.h:45`).
pub const MAX_LIST_COMMANDS: usize = 1 << 16;

/// Maximum canvas ops in one batch (`tgw_hosted_wire.h:46`).
pub const MAX_CANVAS_RUN_OPS: usize = 1 << 13;

/// Maximum stop reason byte length (`tgw_hosted_wire.h:47`).
pub const MAX_STOP_REASON_BYTES: usize = 4096;

/// Maximum cursor CSS name length (`tgw_hosted_wire.h:48`).
pub const MAX_CURSOR_BYTES: usize = 256;

/// Overhead of a `Mirrored` frame (`header + way + len`, `tgw_hosted_wire.h:56-58`).
pub const MIRRORED_OVERHEAD_BYTES: usize = WIRE_HEADER_BYTES + 1 + WIRE_LENGTH_BYTES;

/// Maximum text bytes inside a `Mirrored` frame (`tgw_hosted_wire.h:59-60`).
pub const MAX_MIRRORED_TEXT_BYTES: usize = MAX_MESSAGE_BYTES - MIRRORED_OVERHEAD_BYTES;

/// Maximum serialized frame message size (8 MiB, `tgw_renderer_platform.cpp:27`).
pub const MAX_FRAME_BYTES: usize = 8 << 20;

/// Maximum renderer report text bytes (256 KiB, `tgw_renderer_platform.cpp:28`).
pub const MAX_REPORTS_BYTES: usize = 256 << 10;

/// Maximum estimated decoded memory budget per message (128 MiB, `tgw_renderer_platform.cpp:29`).
pub const MAX_DECODE_BYTES: usize = 128 << 20;

/// Special query ID used for post-load render acknowledgement (`tgw_renderer_rules.h:42`).
pub const LOAD_ACK_QUERY_ID: i64 = -1;

/// Local query ID used for host-internal queries (`tgw_renderer_rules.h:40`).
pub const LOCAL_QUERY_ID: i64 = 0;

/// Canonical ready JSON message (`tgw_renderer_rules.h:27`).
pub const READY_JSON_MESSAGE: &str = r#"{"type":"ready"}"#;

/// Capability caps for the remote renderer wire (`HostedWire::EmbeddingCaps` in `tgw_hosted_wire.h:72-78`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingCaps {
    /// Maximum wire message size in bytes.
    pub max_message_bytes: usize,
    /// Maximum display-list commands per frame.
    pub max_list_commands: usize,
    /// Maximum canvas operations per batch.
    pub max_canvas_ops: usize,
}

impl Default for EmbeddingCaps {
    fn default() -> Self {
        Self {
            max_message_bytes: MAX_MESSAGE_BYTES,
            max_list_commands: MAX_LIST_COMMANDS,
            max_canvas_ops: MAX_CANVAS_RUN_OPS,
        }
    }
}

impl EmbeddingCaps {
    /// Validates a wire message byte length against `max_message_bytes`.
    pub fn validate_message(&self, bytes: usize) -> Result<()> {
        if bytes > self.max_message_bytes {
            return Err(TgwError::Wire(format!(
                "message size {bytes} exceeds cap {}",
                self.max_message_bytes
            )));
        }
        Ok(())
    }

    /// Validates a display-list command count against `max_list_commands`.
    pub fn validate_commands(&self, count: usize) -> Result<()> {
        if count > self.max_list_commands {
            return Err(TgwError::Wire(format!(
                "display-list command count {count} exceeds cap {}",
                self.max_list_commands
            )));
        }
        Ok(())
    }

    /// Validates a canvas operation batch count against `max_canvas_ops`.
    pub fn validate_canvas_ops(&self, count: usize) -> Result<()> {
        if count > self.max_canvas_ops {
            return Err(TgwError::Wire(format!(
                "canvas op count {count} exceeds cap {}",
                self.max_canvas_ops
            )));
        }
        Ok(())
    }
}

/// Control frame discriminant (`HostedWire::Kind` in `tgw_hosted_wire.h:22-38`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ControlKind {
    // Host -> Helper (1..=7)
    /// Opens a renderer session with the given device pixel ratio.
    Open = 1,
    /// Delivers held context-menu events for a mouse press serial.
    MenuDeliver = 2,
    /// Delivers held touch context-menu events on long-press.
    FingerHold = 3,
    /// Releases held touch events on finger release.
    FingerRelease = 4,
    /// Tap control: mirror diagnostics.
    Mirror = 5,
    /// Tap control: mirror render-work diagnostics.
    MirrorWork = 6,
    /// Refills per-frame canvas font parse/request quotas.
    CanvasRefill = 7,

    // Helper -> Host (64..=70)
    /// First non-empty draw ops observed.
    Content = 64,
    /// Both content and load-ack observed.
    Loaded = 65,
    /// Renderer stopped with a diagnostic reason string.
    Stopped = 66,
    /// Mirrored diagnostic record.
    Mirrored = 67,
    /// Cursor style change correlated with a pointer reply counter.
    Cursor = 68,
    /// Canvas font allowance exceeded.
    CanvasRefused = 69,
    /// Primary click event observed for a press serial.
    Clicked = 70,
}

impl ControlKind {
    /// Parses a discriminant byte into a known [`ControlKind`].
    #[must_use]
    pub const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Open),
            2 => Some(Self::MenuDeliver),
            3 => Some(Self::FingerHold),
            4 => Some(Self::FingerRelease),
            5 => Some(Self::Mirror),
            6 => Some(Self::MirrorWork),
            7 => Some(Self::CanvasRefill),
            64 => Some(Self::Content),
            65 => Some(Self::Loaded),
            66 => Some(Self::Stopped),
            67 => Some(Self::Mirrored),
            68 => Some(Self::Cursor),
            69 => Some(Self::CanvasRefused),
            70 => Some(Self::Clicked),
            _ => None,
        }
    }

    /// Returns `true` if this control kind is sent from host to helper (`ToHelper`).
    #[must_use]
    pub const fn is_to_helper(self) -> bool {
        matches!(
            self,
            Self::Open
                | Self::MenuDeliver
                | Self::FingerHold
                | Self::FingerRelease
                | Self::Mirror
                | Self::MirrorWork
                | Self::CanvasRefill
        )
    }

    /// Returns `true` if this control kind is sent from helper to host (`FromHelper`).
    #[must_use]
    pub const fn is_from_helper(self) -> bool {
        matches!(
            self,
            Self::Content
                | Self::Loaded
                | Self::Stopped
                | Self::Mirrored
                | Self::Cursor
                | Self::CanvasRefused
                | Self::Clicked
        )
    }

    /// Returns `true` if this is a diagnostic tap control (`IsTapControl`).
    #[must_use]
    pub const fn is_tap_control(self) -> bool {
        matches!(self, Self::Mirror | Self::MirrorWork)
    }
}

/// Direction tag for `Mirrored` diagnostic frames (`tgw_hosted_wire.h:50-54`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MirroredWay {
    /// Built display/DOM snapshot (`'b'`).
    Built = b'b',
    /// Host message (`'h'`).
    Host = b'h',
    /// Renderer message (`'r'`).
    Renderer = b'r',
    /// Theme application (`'t'`).
    Theme = b't',
    /// Render work trace (`'w'`).
    Work = b'w',
}

impl MirroredWay {
    /// Converts a byte to a [`MirroredWay`].
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'b' => Some(Self::Built),
            b'h' => Some(Self::Host),
            b'r' => Some(Self::Renderer),
            b't' => Some(Self::Theme),
            b'w' => Some(Self::Work),
            _ => None,
        }
    }
}

/// Typed hosted binary control frame (`tgw_hosted_wire.h`).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMessage {
    /// `Kind::Open`: finite device pixel ratio encoded as IEEE-754 f64 LE bits.
    Open(f64),
    /// `Kind::MenuDeliver`: mouse press serial.
    MenuDeliver(u64),
    /// `Kind::FingerHold`: touch press serial.
    FingerHold(u64),
    /// `Kind::FingerRelease`: touch press serial.
    FingerRelease(u64),
    /// `Kind::Mirror`.
    Mirror,
    /// `Kind::MirrorWork`.
    MirrorWork,
    /// `Kind::CanvasRefill`.
    CanvasRefill,
    /// `Kind::Content`.
    Content,
    /// `Kind::Loaded`.
    Loaded,
    /// `Kind::Stopped`: UTF-8 stop reason (truncated to [`MAX_STOP_REASON_BYTES`]).
    Stopped(String),
    /// `Kind::Mirrored`: diagnostic record.
    Mirrored {
        /// Record stream tag.
        way: MirroredWay,
        /// UTF-8 record text.
        text: String,
    },
    /// `Kind::Cursor`: pointer reply counter and CSS cursor name.
    Cursor {
        /// Number of pointer replies processed before this cursor change.
        pointers: u64,
        /// CSS cursor name (e.g. `"pointer"` or `"auto"`).
        cursor: String,
    },
    /// `Kind::CanvasRefused`.
    CanvasRefused,
    /// `Kind::Clicked`: mouse press serial that produced a DOM click.
    Clicked(u64),
}

/// Returns `true` if `frame` starts with [`WIRE_TAG`] (`HostedWire::IsControl`).
#[must_use]
pub fn is_control_frame(frame: &[u8]) -> bool {
    frame.first().copied() == Some(WIRE_TAG)
}

/// Extracts the [`ControlKind`] of a control frame, if valid (`HostedWire::KindOf`).
#[must_use]
pub fn control_kind_of(frame: &[u8]) -> Option<ControlKind> {
    if frame.len() < WIRE_HEADER_BYTES || !is_control_frame(frame) {
        return None;
    }
    ControlKind::from_u8(frame[1])
}

/// Returns `true` if `frame` is an exact empty-body control frame of `kind`
/// (`HostedWire::IsExactly` in `tgw_hosted_wire.h:129-131`).
#[must_use]
pub fn is_exactly(frame: &[u8], kind: ControlKind) -> bool {
    frame.len() == WIRE_HEADER_BYTES && control_kind_of(frame) == Some(kind)
}

/// Decodes a press-serial control frame (`MenuDeliver`, `FingerHold`, or `FingerRelease`)
/// (`HostedWire::DecodePress` in `tgw_hosted_wire.h:236-245`).
pub fn decode_press(frame: &[u8]) -> Result<(ControlKind, u64)> {
    match ControlMessage::decode(frame)? {
        ControlMessage::MenuDeliver(press) => Ok((ControlKind::MenuDeliver, press)),
        ControlMessage::FingerHold(press) => Ok((ControlKind::FingerHold, press)),
        ControlMessage::FingerRelease(press) => Ok((ControlKind::FingerRelease, press)),
        other => Err(TgwError::Wire(format!(
            "expected press control frame, got {:?}",
            other.kind()
        ))),
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn read_u64_le(slice: &[u8]) -> Result<u64> {
    let arr: [u8; 8] = slice
        .get(..8)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| TgwError::Wire("truncated 64-bit integer".to_owned()))?;
    Ok(u64::from_le_bytes(arr))
}

fn read_u32_le(slice: &[u8]) -> Result<u32> {
    let arr: [u8; 4] = slice
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| TgwError::Wire("truncated 32-bit length".to_owned()))?;
    Ok(u32::from_le_bytes(arr))
}

fn decode_len_prefixed_text(body: &[u8], limit: usize) -> Result<String> {
    if body.len() < WIRE_LENGTH_BYTES {
        return Err(TgwError::Wire("missing text length field".to_owned()));
    }
    let len = read_u32_le(body)? as usize;
    let rest = &body[WIRE_LENGTH_BYTES..];
    if len > limit || len != rest.len() {
        return Err(TgwError::Wire(format!(
            "text length {len} exceeds limit {limit} or mismatches body {}",
            rest.len()
        )));
    }
    let text = std::str::from_utf8(rest)
        .map_err(|e| TgwError::Wire(format!("invalid UTF-8 in control frame: {e}")))?;
    Ok(text.to_owned())
}

impl ControlMessage {
    /// Returns the [`ControlKind`] discriminant of this message.
    #[must_use]
    pub const fn kind(&self) -> ControlKind {
        match self {
            Self::Open(_) => ControlKind::Open,
            Self::MenuDeliver(_) => ControlKind::MenuDeliver,
            Self::FingerHold(_) => ControlKind::FingerHold,
            Self::FingerRelease(_) => ControlKind::FingerRelease,
            Self::Mirror => ControlKind::Mirror,
            Self::MirrorWork => ControlKind::MirrorWork,
            Self::CanvasRefill => ControlKind::CanvasRefill,
            Self::Content => ControlKind::Content,
            Self::Loaded => ControlKind::Loaded,
            Self::Stopped(_) => ControlKind::Stopped,
            Self::Mirrored { .. } => ControlKind::Mirrored,
            Self::Cursor { .. } => ControlKind::Cursor,
            Self::CanvasRefused => ControlKind::CanvasRefused,
            Self::Clicked(_) => ControlKind::Clicked,
        }
    }

    /// Encodes this control message into its exact tdesktop binary wire representation.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        out.push(WIRE_TAG);
        out.push(self.kind() as u8);

        match self {
            Self::Open(dpr) => {
                if !dpr.is_finite() {
                    return Err(TgwError::Wire(
                        "Open device pixel ratio must be finite".to_owned(),
                    ));
                }
                out.extend_from_slice(&dpr.to_bits().to_le_bytes());
            }
            Self::MenuDeliver(press)
            | Self::FingerHold(press)
            | Self::FingerRelease(press)
            | Self::Clicked(press) => {
                out.extend_from_slice(&press.to_le_bytes());
            }
            Self::Mirror
            | Self::MirrorWork
            | Self::CanvasRefill
            | Self::Content
            | Self::Loaded
            | Self::CanvasRefused => {}
            Self::Stopped(reason) => {
                let text = truncate_utf8(reason, MAX_STOP_REASON_BYTES);
                out.extend_from_slice(&(text.len() as u32).to_le_bytes());
                out.extend_from_slice(text.as_bytes());
            }
            Self::Mirrored { way, text } => {
                if text.len() > MAX_MIRRORED_TEXT_BYTES {
                    return Err(TgwError::Wire(format!(
                        "Mirrored text {} exceeds max {MAX_MIRRORED_TEXT_BYTES}",
                        text.len()
                    )));
                }
                out.push(*way as u8);
                out.extend_from_slice(&(text.len() as u32).to_le_bytes());
                out.extend_from_slice(text.as_bytes());
            }
            Self::Cursor { pointers, cursor } => {
                let text = truncate_utf8(cursor, MAX_CURSOR_BYTES);
                out.extend_from_slice(&pointers.to_le_bytes());
                out.extend_from_slice(&(text.len() as u32).to_le_bytes());
                out.extend_from_slice(text.as_bytes());
            }
        }
        Ok(out)
    }

    /// Decodes a binary control frame, validating exact sizes, finite floats, and UTF-8.
    pub fn decode(frame: &[u8]) -> Result<Self> {
        let kind = control_kind_of(frame)
            .ok_or_else(|| TgwError::Wire("invalid control frame tag or kind".to_owned()))?;
        let body = &frame[WIRE_HEADER_BYTES..];

        match kind {
            ControlKind::Open => {
                if body.len() != WIRE_NUMBER_BYTES {
                    return Err(TgwError::Wire("Open frame has invalid length".to_owned()));
                }
                let dpr = f64::from_bits(read_u64_le(body)?);
                if !dpr.is_finite() {
                    return Err(TgwError::Wire(
                        "Open frame has non-finite device pixel ratio".to_owned(),
                    ));
                }
                Ok(Self::Open(dpr))
            }
            ControlKind::MenuDeliver => {
                if body.len() != WIRE_NUMBER_BYTES {
                    return Err(TgwError::Wire(
                        "MenuDeliver frame has invalid length".to_owned(),
                    ));
                }
                Ok(Self::MenuDeliver(read_u64_le(body)?))
            }
            ControlKind::FingerHold => {
                if body.len() != WIRE_NUMBER_BYTES {
                    return Err(TgwError::Wire(
                        "FingerHold frame has invalid length".to_owned(),
                    ));
                }
                Ok(Self::FingerHold(read_u64_le(body)?))
            }
            ControlKind::FingerRelease => {
                if body.len() != WIRE_NUMBER_BYTES {
                    return Err(TgwError::Wire(
                        "FingerRelease frame has invalid length".to_owned(),
                    ));
                }
                Ok(Self::FingerRelease(read_u64_le(body)?))
            }
            ControlKind::Clicked => {
                if body.len() != WIRE_NUMBER_BYTES {
                    return Err(TgwError::Wire(
                        "Clicked frame has invalid length".to_owned(),
                    ));
                }
                Ok(Self::Clicked(read_u64_le(body)?))
            }
            ControlKind::Mirror
            | ControlKind::MirrorWork
            | ControlKind::CanvasRefill
            | ControlKind::Content
            | ControlKind::Loaded
            | ControlKind::CanvasRefused => {
                if !body.is_empty() {
                    return Err(TgwError::Wire(format!(
                        "{kind:?} frame must have empty body, got {} bytes",
                        body.len()
                    )));
                }
                Ok(match kind {
                    ControlKind::Mirror => Self::Mirror,
                    ControlKind::MirrorWork => Self::MirrorWork,
                    ControlKind::CanvasRefill => Self::CanvasRefill,
                    ControlKind::Content => Self::Content,
                    ControlKind::Loaded => Self::Loaded,
                    ControlKind::CanvasRefused => Self::CanvasRefused,
                    _ => unreachable!(),
                })
            }
            ControlKind::Stopped => {
                let text = decode_len_prefixed_text(body, MAX_STOP_REASON_BYTES)?;
                Ok(Self::Stopped(text))
            }
            ControlKind::Mirrored => {
                if frame.len() < MIRRORED_OVERHEAD_BYTES || frame.len() > MAX_MESSAGE_BYTES {
                    return Err(TgwError::Wire(
                        "Mirrored frame outside size bounds".to_owned(),
                    ));
                }
                let way = MirroredWay::from_byte(body[0]).ok_or_else(|| {
                    TgwError::Wire(format!("invalid Mirrored way 0x{:02x}", body[0]))
                })?;
                let text = decode_len_prefixed_text(&body[1..], MAX_MIRRORED_TEXT_BYTES)?;
                Ok(Self::Mirrored { way, text })
            }
            ControlKind::Cursor => {
                let fixed = WIRE_NUMBER_BYTES + WIRE_LENGTH_BYTES;
                if body.len() < fixed || body.len() > fixed + MAX_CURSOR_BYTES {
                    return Err(TgwError::Wire(
                        "Cursor frame outside size bounds".to_owned(),
                    ));
                }
                let pointers = read_u64_le(&body[..WIRE_NUMBER_BYTES])?;
                let cursor =
                    decode_len_prefixed_text(&body[WIRE_NUMBER_BYTES..], MAX_CURSOR_BYTES)?;
                Ok(Self::Cursor { pointers, cursor })
            }
        }
    }
}

// =============================================================================
// JSON Host <-> Sandboxed Renderer Bridge Protocol
// =============================================================================

/// Messages sent from the Rustgram host into the sandboxed HTML widget renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostToRendererMsg {
    /// Initial readiness handshake.
    Ready,
    /// Updates theme tokens (`dark` flag + full [`WidgetTheme`]).
    Theme {
        /// Whether dark mode is active.
        dark: bool,
        /// Theme parameters and tokens.
        theme: Box<WidgetTheme>,
    },
    /// Pushes updated server state blobs (`chat_data`, `public_data`).
    Data {
        /// Per-chat data string, if set.
        chat_data: Option<String>,
        /// Shared public data string, if set.
        public_data: Option<String>,
    },
    /// Updates the overlaid timestamp safe-area insets.
    SafeArea {
        /// Safe area insets in CSS pixels.
        area: ContentSafeArea,
    },
    /// Updates viewport visibility.
    Visibility {
        /// Whether the widget card is visible on screen.
        visible: bool,
    },
    /// Updates width / max-height layout constraints.
    Resize {
        /// Card width in CSS pixels.
        width: f64,
        /// Maximum allowed height in CSS pixels.
        max_height: f64,
        /// Device pixel ratio.
        dpr: f64,
    },
    /// Delivers the outcome of a bot query (`messages.getBotWidgetQueryAnswer`).
    QueryAnswer {
        /// Client-assigned request id from [`RendererToHostMsg::BotQuery`].
        request_id: u64,
        /// Query outcome (result string or failure kind).
        outcome: WidgetRequestOutcome,
    },
    /// Animation frame tick (`FrameMessage` in `tgw_renderer_rules.h:143-148`).
    Frame {
        /// Monotonic clock time in milliseconds.
        time: f64,
    },
    /// Pauses / freezes the widget runtime (`tgw_hosted_glue.cpp:470`).
    Frozen,
    /// Requests the renderer to schedule a frame.
    RequestFrame,
    /// Executes a local or load-ack query (`qid == -1` for load ack).
    Query {
        /// Query identifier (`-1` for load ack, `0` for local viewport query).
        qid: i64,
        /// Query name (e.g. `"viewport"` or `"elementFromPoint"`).
        q: String,
    },
}

/// Messages emitted from the sandboxed HTML widget renderer to the Rustgram host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RendererToHostMsg {
    /// Renderer booted and ready to receive host messages.
    Ready,
    /// Reports a new content height in CSS pixels (`KeptBeforeReady` / `applySize`).
    Size {
        /// Content height in CSS pixels.
        height: f64,
    },
    /// Reports a draw-ops batch (`DrawsContent`).
    Ops {
        /// Number of non-empty draw ops produced.
        count: usize,
    },
    /// Requests opening an external or Telegram link (`Bridge::openLink`).
    OpenLink {
        /// Target URL.
        url: String,
        /// `true` for `openTelegramLink`, `false` for `openLink`.
        telegram: bool,
    },
    /// Requests copying text to the system clipboard (`Bridge::copyText`).
    CopyText {
        /// UTF-8 text to copy.
        text: String,
    },
    /// Issues a bot widget query (`messages.getBotWidgetQueryAnswer`).
    BotQuery {
        /// Client-assigned correlation id.
        request_id: u64,
        /// Opaque UTF-8 query payload bytes.
        data: String,
        /// Whether to attach user's wallet card (`with_wallet`).
        wallet: bool,
    },
    /// Persists widget local storage area (`StorageHost::commit`).
    StorageCommit {
        /// Storage area name (e.g. `"local"`).
        area: String,
        /// Key-value entries to persist.
        values: BTreeMap<String, String>,
    },
    /// Toggles watching shared public data (`hostWatch`).
    WatchChange {
        /// Whether the widget is actively watching shared state.
        on: bool,
    },
    /// Reports hover cursor style (`"pointer"` or `"auto"`).
    CursorChange {
        /// CSS cursor string.
        cursor: String,
    },
    /// Reports selected text inside the widget.
    SelectionChange {
        /// Selected text (empty when selection cleared).
        text: String,
    },
    /// Emits a console/diagnostic line for the widget log box.
    Diagnostic {
        /// Diagnostic category (e.g. `"canvas-limit"`, `"canvas-font-limit"`).
        kind: String,
        /// Human-readable detail.
        detail: String,
    },
    /// Reports that the renderer stopped with `reason`.
    Stopped {
        /// Human-readable stop reason.
        reason: String,
    },
}

/// Encodes a [`HostToRendererMsg`] to JSON, enforcing [`MAX_MESSAGE_BYTES`].
pub fn encode_host_message(msg: &HostToRendererMsg) -> Result<String> {
    let json = serde_json::to_string(msg)
        .map_err(|e| TgwError::Bridge(format!("serialize host message: {e}")))?;
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(TgwError::Bridge(format!(
            "host message size {} exceeds cap {MAX_MESSAGE_BYTES}",
            json.len()
        )));
    }
    Ok(json)
}

/// Decodes a [`HostToRendererMsg`] from JSON, enforcing [`MAX_MESSAGE_BYTES`].
pub fn decode_host_message(json: &str) -> Result<HostToRendererMsg> {
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(TgwError::Bridge(format!(
            "host message size {} exceeds cap {MAX_MESSAGE_BYTES}",
            json.len()
        )));
    }
    serde_json::from_str(json).map_err(|e| TgwError::Bridge(format!("parse host message: {e}")))
}

/// Encodes a [`RendererToHostMsg`] to JSON, enforcing [`MAX_MESSAGE_BYTES`].
pub fn encode_renderer_message(msg: &RendererToHostMsg) -> Result<String> {
    let json = serde_json::to_string(msg)
        .map_err(|e| TgwError::Bridge(format!("serialize renderer message: {e}")))?;
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(TgwError::Bridge(format!(
            "renderer message size {} exceeds cap {MAX_MESSAGE_BYTES}",
            json.len()
        )));
    }
    Ok(json)
}

/// Decodes a [`RendererToHostMsg`] from JSON, enforcing [`MAX_MESSAGE_BYTES`], finite heights,
/// and bounded diagnostic text.
pub fn decode_renderer_message(json: &str) -> Result<RendererToHostMsg> {
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(TgwError::Bridge(format!(
            "renderer message size {} exceeds cap {MAX_MESSAGE_BYTES}",
            json.len()
        )));
    }
    let msg: RendererToHostMsg = serde_json::from_str(json)
        .map_err(|e| TgwError::Bridge(format!("parse renderer message: {e}")))?;
    if let RendererToHostMsg::Size { height } = &msg
        && (!height.is_finite() || *height < 0.0)
    {
        return Err(TgwError::Bridge(format!(
            "invalid widget size height: {height}"
        )));
    }
    Ok(msg)
}

/// Tracks initial content + load-ack readiness transitions (`AdvanceLoad` in `tgw_renderer_rules.h:179-196`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LoadTracker {
    /// Whether any non-empty draw ops have been observed.
    pub has_content: bool,
    /// Whether the post-load query (`qid == -1`) has been acknowledged.
    pub acked: bool,
    /// Whether both `has_content` and `acked` have been satisfied.
    pub loaded: bool,
}

/// Transition edges reported by [`LoadTracker`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LoadChange {
    /// `true` on the first transition to `has_content`.
    pub content: bool,
    /// `true` on the first transition to `loaded`.
    pub loaded: bool,
}

impl LoadTracker {
    /// Advances load state upon observing a non-empty draw batch (`DrawsContent`).
    pub fn on_draw_ops(&mut self, op_count: usize) -> LoadChange {
        let mut change = LoadChange::default();
        if op_count > 0 {
            change.content = !self.has_content;
            self.has_content = true;
        }
        if self.acked && self.has_content && !self.loaded {
            self.loaded = true;
            change.loaded = true;
        }
        change
    }

    /// Advances load state upon observing a query message (`qid == LOAD_ACK_QUERY_ID`).
    pub fn on_query(&mut self, qid: i64) -> LoadChange {
        let mut change = LoadChange::default();
        if qid == LOAD_ACK_QUERY_ID {
            self.acked = true;
        }
        if self.acked && self.has_content && !self.loaded {
            self.loaded = true;
            change.loaded = true;
        }
        change
    }
}
