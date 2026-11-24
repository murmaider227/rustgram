//! Port of `core/sandbox.h` and the single-instance IPC glue of `core/sandbox.cpp`.
//!
//! Defines the single-instance wire protocol (`CMD:show;`, `CMD:quit;`, `OPEN:<7bit>;`,
//! `RES:<pid>_<window_id>;`), 7-bit `%XXXX` escaping, mixed-URL/local-file smuggling protection,
//! and the [`SingleInstanceTransport`] trait implemented by `tg-platform` (Windows named pipes).

use thiserror::Error;

use crate::launcher::MAX_ARG_CHARS;

/// Maximum buffered IPC bytes per client connection (`kLocalClientBufferLimit = 1 MiB`).
pub const LOCAL_CLIENT_BUFFER_LIMIT: usize = 1024 * 1024;

/// IPC timeout during `-cleanup` shutdown (`kCleanupIpcTimeout = 10s`).
pub const CLEANUP_IPC_TIMEOUT_MS: u64 = 10_000;

/// Maximum wait for running instance exit during `-cleanup` (`kCleanupQuitTimeout = 30s`).
pub const CLEANUP_QUIT_TIMEOUT_MS: u64 = 30_000;

/// Errors returned by single-instance IPC operations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SingleInstanceError {
    /// No running primary instance was found at the IPC endpoint.
    #[error("no running instance found")]
    ServerNotFound,
    /// Client exceeded `LOCAL_CLIENT_BUFFER_LIMIT`.
    #[error("single-instance client buffer overflow")]
    BufferOverflow,
    /// Peer process was rejected by platform security check (different user).
    #[error("untrusted IPC peer")]
    UntrustedPeer,
    /// Malformed response received from primary instance.
    #[error("malformed single-instance response: {0}")]
    MalformedResponse(String),
    /// Platform transport failure (e.g. named pipe error).
    #[error("single-instance transport error: {0}")]
    Transport(String),
}

/// Command sent from a secondary launch to the primary instance (`sandbox.cpp:392-406`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SingleInstanceCommand {
    /// Activate and raise the main window (`CMD:show;`).
    Show,
    /// Request the running instance to quit (`CMD:quit;`).
    Quit,
    /// Forward a URL or local file path (`OPEN:<escaped>;`).
    OpenUrl(String),
    /// Forward a Wayland/XDG activation token (`XDG_ACTIVATION_TOKEN:<base64>;`).
    XdgActivationToken(String),
    /// External control command (`CTRL:<payload>;`).
    Control(String),
}

/// Response returned by the primary instance (`RES:<pid>_<window_id>;`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SingleInstanceResponse {
    /// OS process ID of the primary instance.
    pub process_id: u64,
    /// Native window handle/ID to activate (`0` if activation not required).
    pub window_id: u64,
}

impl SingleInstanceResponse {
    /// Formats as `RES:<pid>_<window_id>;`.
    #[must_use]
    pub fn encode(self) -> String {
        format!("RES:{}_{};", self.process_id, self.window_id)
    }

    /// Parses the first `RES:<pid>_<window_id>;` record from `data`.
    pub fn decode(data: &str) -> Result<Self, SingleInstanceError> {
        let Some(start) = data.find("RES:") else {
            return Err(SingleInstanceError::MalformedResponse(data.to_owned()));
        };
        let rest = &data[start + 4..];
        let Some(end) = rest.find(';') else {
            return Err(SingleInstanceError::MalformedResponse(data.to_owned()));
        };
        let body = &rest[..end];
        let Some((pid_str, wid_str)) = body.split_once('_') else {
            return Err(SingleInstanceError::MalformedResponse(data.to_owned()));
        };
        let process_id = pid_str
            .parse::<u64>()
            .map_err(|_| SingleInstanceError::MalformedResponse(data.to_owned()))?;
        let window_id = wid_str
            .parse::<u64>()
            .map_err(|_| SingleInstanceError::MalformedResponse(data.to_owned()))?;
        Ok(Self {
            process_id,
            window_id,
        })
    }
}

/// Incoming parsed batch of commands from a secondary instance.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IncomingBatch {
    /// Commands received (`CMD:*` / `CTRL:*` skipped when `OPEN:` is present, per `sandbox.cpp:609`).
    pub commands: Vec<SingleInstanceCommand>,
    /// Filtered `OPEN:` URLs/paths (local `file://` URLs dropped if an external URL was mixed in).
    pub open_urls: Vec<String>,
    /// Whether any local file URL was dropped due to mixed external-URL smuggling protection.
    pub dropped_local_files: usize,
}

/// Escapes a string into tdesktop's 7-bit `%XXXX` UTF-16 encoding (`EscapeTo7bit` in `sandbox.cpp:72-91`).
#[must_use]
pub fn escape_to_7bit(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(value.len() * 2);
    for unit in value.encode_utf16() {
        if !(32..=127).contains(&unit) || unit == u16::from(b'%') || unit == u16::from(b';') {
            out.push('%');
            out.push(HEX[((unit >> 12) & 0x0F) as usize] as char);
            out.push(HEX[((unit >> 8) & 0x0F) as usize] as char);
            out.push(HEX[((unit >> 4) & 0x0F) as usize] as char);
            out.push(HEX[(unit & 0x0F) as usize] as char);
        } else {
            out.push(unit as u8 as char);
        }
    }
    out
}

/// Decodes a string encoded with [`escape_to_7bit`] (`EscapeFrom7bit` in `sandbox.cpp:93-110`).
#[must_use]
pub fn escape_from_7bit(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 4 < bytes.len() {
            let code = (hex_val(bytes[i + 1]) << 12)
                | (hex_val(bytes[i + 2]) << 8)
                | (hex_val(bytes[i + 3]) << 4)
                | hex_val(bytes[i + 4]);
            units.push(code);
            i += 5;
        } else {
            units.push(u16::from(bytes[i]));
            i += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

fn hex_val(b: u8) -> u16 {
    let lower = b.to_ascii_lowercase();
    if lower >= b'a' {
        u16::from(lower.wrapping_sub(b'a').wrapping_add(10)) & 0x000F
    } else {
        u16::from(lower.wrapping_sub(b'0')) & 0x000F
    }
}

/// Encodes a slice of [`SingleInstanceCommand`]s into the wire payload (`sandbox.cpp:392-408`).
#[must_use]
pub fn encode_commands(commands: &[SingleInstanceCommand]) -> String {
    let mut out = String::new();
    for cmd in commands {
        match cmd {
            SingleInstanceCommand::Show => out.push_str("CMD:show;"),
            SingleInstanceCommand::Quit => out.push_str("CMD:quit;"),
            SingleInstanceCommand::OpenUrl(url) => {
                out.push_str("OPEN:");
                out.push_str(&escape_to_7bit(url));
                out.push(';');
            }
            SingleInstanceCommand::XdgActivationToken(tok) => {
                out.push_str("XDG_ACTIVATION_TOKEN:");
                out.push_str(tok);
                out.push(';');
            }
            SingleInstanceCommand::Control(payload) => {
                out.push_str("CTRL:");
                out.push_str(payload);
                out.push(';');
            }
        }
    }
    out
}

/// Parses complete `;`-delimited records from `payload`, enforcing tdesktop's mixed-URL
/// smuggling protection (`sandbox.cpp:588-665`).
#[must_use]
pub fn parse_command_batch(payload: &str) -> IncomingBatch {
    let mut batch = IncomingBatch::default();
    let records: Vec<&str> = payload.split(';').filter(|s| !s.is_empty()).collect();

    let has_open = records.iter().any(|r| r.starts_with("OPEN:"));
    let mut raw_urls = Vec::new();

    for rec in records {
        if let Some(rest) = rec.strip_prefix("CMD:") {
            if has_open {
                continue;
            }
            match rest {
                "show" => batch.commands.push(SingleInstanceCommand::Show),
                "quit" => batch.commands.push(SingleInstanceCommand::Quit),
                _ => {}
            }
        } else if let Some(rest) = rec.strip_prefix("OPEN:") {
            let decoded: String = escape_from_7bit(rest).chars().take(MAX_ARG_CHARS).collect();
            raw_urls.push(decoded);
        } else if let Some(rest) = rec.strip_prefix("XDG_ACTIVATION_TOKEN:") {
            batch
                .commands
                .push(SingleInstanceCommand::XdgActivationToken(rest.to_owned()));
        } else if let Some(rest) = rec.strip_prefix("CTRL:")
            && !has_open
        {
            batch
                .commands
                .push(SingleInstanceCommand::Control(rest.to_owned()));
        }
    }

    // Smuggling protection (`sandbox.cpp:633-658`): if any URL is non-local, drop local file URLs.
    let has_external = raw_urls.iter().any(|u| !is_local_file_url(u));
    for url in raw_urls {
        if has_external && is_local_file_url(&url) {
            batch.dropped_local_files += 1;
            continue;
        }
        batch.open_urls.push(url);
    }

    batch
}

fn is_local_file_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("file://")
}

/// Derives the platform IPC server name from the working directory hash and app GUID
/// (`Platform::SingleInstanceLocalServerName` in `specific_win.cpp:391-393`).
#[must_use]
pub fn single_instance_server_name(working_dir_hash: &str, app_id: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("Global\\{working_dir_hash}-{app_id}")
    } else {
        format!("{working_dir_hash}-{app_id}")
    }
}

/// Outcome of the startup single-instance handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SingleInstanceStartup {
    /// We are the primary instance and are now listening on the IPC server.
    PrimaryInstance,
    /// Another instance is already running; commands were forwarded and this process should exit.
    ForwardedToExisting(SingleInstanceResponse),
}

/// Platform single-instance IPC trait (`tg-platform` implements this using Windows named pipes).
pub trait SingleInstanceTransport {
    /// Returns the IPC endpoint / named pipe identifier.
    fn server_name(&self) -> &str;

    /// Attempts to connect to an already-running primary instance and deliver `commands`.
    ///
    /// Returns `Ok(Some(response))` if an existing instance handled the commands, or
    /// `Ok(None)` if no running instance was found.
    fn try_forward(
        &self,
        commands: &[SingleInstanceCommand],
    ) -> Result<Option<SingleInstanceResponse>, SingleInstanceError>;

    /// Starts listening for secondary instances on [`Self::server_name`].
    fn start_server(&mut self) -> Result<(), SingleInstanceError>;

    /// Polls any pending incoming batches sent by secondary instances.
    fn poll_incoming(&mut self) -> Result<Vec<IncomingBatch>, SingleInstanceError>;

    /// Stops listening and releases the IPC endpoint.
    fn stop_server(&mut self);
}

/// High-level single-instance coordinator used by the application during startup (`Core::Sandbox`).
#[derive(Debug)]
pub struct SingleInstanceCoordinator<T: SingleInstanceTransport> {
    transport: T,
    listening: bool,
}

impl<T: SingleInstanceTransport> SingleInstanceCoordinator<T> {
    /// Creates a new coordinator wrapping `transport`.
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            listening: false,
        }
    }

    /// Borrows the underlying transport.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Mutably borrows the underlying transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Returns `true` if this instance currently owns the IPC server.
    #[must_use]
    pub fn is_listening(&self) -> bool {
        self.listening
    }

    /// Performs the tdesktop single-instance startup check (`Sandbox::start` / `socketConnected` / `socketError`):
    ///
    /// 1. Builds the startup command list from `start_urls` and `quit`.
    /// 2. Unless `allow_many` is true, tries forwarding to an existing instance.
    /// 3. If no existing instance answers, starts listening as the primary instance.
    pub fn acquire_or_forward(
        &mut self,
        start_urls: &[String],
        quit: bool,
        allow_many: bool,
    ) -> Result<SingleInstanceStartup, SingleInstanceError> {
        let mut commands = Vec::new();
        for url in start_urls {
            commands.push(SingleInstanceCommand::OpenUrl(url.clone()));
        }
        if quit {
            commands.push(SingleInstanceCommand::Quit);
        } else if start_urls.is_empty() {
            commands.push(SingleInstanceCommand::Show);
        }

        if !allow_many && let Some(resp) = self.transport.try_forward(&commands)? {
            return Ok(SingleInstanceStartup::ForwardedToExisting(resp));
        }

        self.transport.start_server()?;
        self.listening = true;
        Ok(SingleInstanceStartup::PrimaryInstance)
    }

    /// Polls incoming command batches from secondary instances.
    pub fn poll_incoming(&mut self) -> Result<Vec<IncomingBatch>, SingleInstanceError> {
        if !self.listening {
            return Ok(Vec::new());
        }
        self.transport.poll_incoming()
    }

    /// Shuts down the IPC server cleanly.
    pub fn stop(&mut self) {
        if self.listening {
            self.transport.stop_server();
            self.listening = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_bit_escape_round_trip() {
        let input = "tg://resolve?domain=test;pct=50%&emoji=😀\n";
        let escaped = escape_to_7bit(input);
        assert!(!escaped.contains(';'));
        assert!(escaped.is_ascii());
        assert_eq!(escape_from_7bit(&escaped), input);
    }

    #[test]
    fn command_encoding_response_and_smuggling_protection() {
        let encoded = encode_commands(&[
            SingleInstanceCommand::OpenUrl("tg://resolve?domain=foo".to_owned()),
            SingleInstanceCommand::OpenUrl("file:///C:/secret.txt".to_owned()),
            SingleInstanceCommand::Show,
        ]);
        let batch = parse_command_batch(&encoded);
        // CMD:show ignored when OPEN: present; file:// dropped when external URL present.
        assert!(batch.commands.is_empty());
        assert_eq!(batch.open_urls, vec!["tg://resolve?domain=foo".to_owned()]);
        assert_eq!(batch.dropped_local_files, 1);

        let resp = SingleInstanceResponse {
            process_id: 4242,
            window_id: 987_654,
        };
        let wire = resp.encode();
        assert_eq!(wire, "RES:4242_987654;");
        assert_eq!(SingleInstanceResponse::decode(&wire).unwrap(), resp);
    }

    #[test]
    fn coordinator_forwards_or_starts_primary() {
        struct MockTransport {
            name: String,
            existing: Option<SingleInstanceResponse>,
            started: bool,
        }
        impl SingleInstanceTransport for MockTransport {
            fn server_name(&self) -> &str {
                &self.name
            }
            fn try_forward(
                &self,
                _commands: &[SingleInstanceCommand],
            ) -> Result<Option<SingleInstanceResponse>, SingleInstanceError> {
                Ok(self.existing)
            }
            fn start_server(&mut self) -> Result<(), SingleInstanceError> {
                self.started = true;
                Ok(())
            }
            fn poll_incoming(&mut self) -> Result<Vec<IncomingBatch>, SingleInstanceError> {
                Ok(Vec::new())
            }
            fn stop_server(&mut self) {
                self.started = false;
            }
        }

        let resp = SingleInstanceResponse {
            process_id: 100,
            window_id: 200,
        };
        let mut secondary = SingleInstanceCoordinator::new(MockTransport {
            name: single_instance_server_name("deadbeef", "guid"),
            existing: Some(resp),
            started: false,
        });
        assert_eq!(
            secondary.acquire_or_forward(&[], false, false).unwrap(),
            SingleInstanceStartup::ForwardedToExisting(resp)
        );
        assert!(!secondary.is_listening());

        let mut primary = SingleInstanceCoordinator::new(MockTransport {
            name: single_instance_server_name("deadbeef", "guid"),
            existing: None,
            started: false,
        });
        assert_eq!(
            primary.acquire_or_forward(&[], false, false).unwrap(),
            SingleInstanceStartup::PrimaryInstance
        );
        assert!(primary.is_listening());
        primary.stop();
        assert!(!primary.is_listening());
    }
}
