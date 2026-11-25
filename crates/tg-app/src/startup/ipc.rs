//! Single-instance named-pipe IPC bridge and wire message encoding.
//!
//! Port origin: `Telegram/SourceFiles/core/sandbox.cpp` (`socketConnected`, `readClients`,
//! `stopRunningInstance`) and `Telegram/SourceFiles/platform/win/specific_win.cpp`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tg_core::launcher::{ParsedArgs, is_url_argument};
use tg_core::sandbox::CLEANUP_QUIT_TIMEOUT_MS;
use tg_platform::single_instance::{
    AcquireResult, InstanceActivation, InstanceResponse, SingleInstanceGuard,
    acquire_single_instance, encode_activation_commands, encode_response,
    parse_activation_commands, parse_response, wait_for_process_exit,
};

pub use crate::core_types::startup_types::IncomingActivation;

/// Thread-safe queue connecting the background named-pipe listener thread to `WindowController`.
#[derive(Clone, Default)]
pub struct ActivationQueue {
    inner: Arc<Mutex<VecDeque<IncomingActivation>>>,
    repaint_ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl std::fmt::Debug for ActivationQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.inner.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("ActivationQueue")
            .field("pending", &len)
            .finish()
    }
}

impl ActivationQueue {
    /// Creates an empty [`ActivationQueue`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the active `egui::Context` so incoming named-pipe messages wake the UI loop
    /// immediately even when the window is minimized or hidden in the system tray.
    pub fn set_repaint_ctx(&self, ctx: egui::Context) {
        if let Ok(mut guard) = self.repaint_ctx.lock() {
            *guard = Some(ctx);
        }
    }

    /// Enqueues an incoming activation for the UI thread to process on the next frame.
    pub fn push(&self, activation: IncomingActivation) {
        if activation.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.inner.lock() {
            guard.push_back(activation);
        }
        if let Ok(guard) = self.repaint_ctx.lock()
            && let Some(ctx) = guard.as_ref()
        {
            ctx.request_repaint();
        }
    }

    /// Drains all pending activations in FIFO order.
    #[must_use]
    pub fn drain(&self) -> Vec<IncomingActivation> {
        self.inner
            .lock()
            .map(|mut g| g.drain(..).collect())
            .unwrap_or_default()
    }
}

/// Outcome of attempting to acquire or forward to a single instance.
#[derive(Debug)]
pub enum SingleInstanceOutcome {
    /// This process is the primary instance. Hold `_guard` until shutdown.
    Primary {
        /// RAII guard owning the Windows named mutex and named-pipe listener thread.
        guard: SingleInstanceGuard,
        /// Queue receiving activations forwarded by subsequent launches.
        queue: ActivationQueue,
    },
    /// Another instance is already running; arguments were forwarded over the named pipe.
    ForwardedToExisting {
        /// Response received from the primary instance, if any.
        response: Option<InstanceResponse>,
    },
}

/// Converts a local filesystem path into a canonical `file:///` URI for single-instance
/// `OPEN:` wire encoding, matching `core/sandbox.cpp` (`QUrl::fromLocalFile`).
#[must_use]
pub fn path_to_file_url(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    if raw.to_ascii_lowercase().starts_with("file://") {
        return raw;
    }
    if raw.starts_with('/') {
        format!("file://{raw}")
    } else {
        format!("file:///{raw}")
    }
}

/// Decodes a `file://` URI back into a local [`PathBuf`].
#[must_use]
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url
        .strip_prefix("file:///")
        .or_else(|| url.strip_prefix("FILE:///"))
        .or_else(|| url.strip_prefix("file://"))
        .or_else(|| url.strip_prefix("FILE://"))?;

    if rest.is_empty() {
        return None;
    }
    // On Windows, `file:///C:/foo` -> `C:/foo`; UNC `file://server/share` -> `\\server\share`.
    let decoded = if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
        rest.to_owned()
    } else if url.to_ascii_lowercase().starts_with("file:///") {
        format!("/{rest}")
    } else {
        format!("\\\\{}", rest.replace('/', "\\"))
    };
    Some(PathBuf::from(decoded))
}

/// Builds a [`tg_platform::single_instance::InstanceActivation`] wire payload from [`ParsedArgs`].
///
/// External URLs (`tg://`, `tonsite://`, etc.) and `-sendpath` local files (encoded as `file:///`)
/// are placed into `OPEN:` records. When `-quit` is set, emits `CMD:quit;`; otherwise emits
/// `CMD:show;` when no URLs/files are present.
#[must_use]
pub fn build_activation_from_args(args: &ParsedArgs) -> InstanceActivation {
    let mut urls = Vec::with_capacity(args.start_urls.len() + args.send_paths.len());
    for url in &args.start_urls {
        urls.push(url.clone());
    }
    for path in &args.send_paths {
        urls.push(path_to_file_url(path));
    }
    InstanceActivation {
        show: !args.quit && urls.is_empty(),
        quit: args.quit,
        urls,
        args: Vec::new(),
    }
}

/// Encodes an [`InstanceActivation`] to the semicolon-delimited named-pipe wire string.
#[must_use]
pub fn encode_pipe_message(activation: &InstanceActivation) -> String {
    encode_activation_commands(activation)
}

/// Decodes a named-pipe wire payload into a typed [`IncomingActivation`], separating external
/// URLs (`tg://`, `tonsite://`, etc.) from local `-sendpath` files while enforcing tdesktop's
/// mixed-URL smuggling protection (`sandbox.cpp:633-658`).
#[must_use]
pub fn decode_pipe_message(wire: &str) -> IncomingActivation {
    let parsed = parse_activation_commands(wire);
    activation_to_incoming(parsed)
}

/// Converts a raw [`InstanceActivation`] (already smuggling-filtered by `tg-platform`) into an
/// [`IncomingActivation`] with split `urls` vs `send_paths`.
#[must_use]
pub fn activation_to_incoming(activation: InstanceActivation) -> IncomingActivation {
    let mut urls = Vec::new();
    let mut send_paths = Vec::new();

    for entry in activation.urls {
        if let Some(path) = file_url_to_path(&entry) {
            send_paths.push(path);
        } else if is_url_argument(&entry) {
            urls.push(entry);
        } else if !entry.is_empty() {
            send_paths.push(PathBuf::from(entry));
        }
    }
    // Only absolute local paths: `..\secret.txt` or `\\host\share\x` never reach the ShareBox
    // (the controller also requires the file to exist before showing it).
    send_paths.retain(|p| tg_platform::url_policy::is_local_absolute_path(p));

    let show =
        activation.show || (!activation.quit && (!urls.is_empty() || !send_paths.is_empty()));
    IncomingActivation {
        urls,
        send_paths,
        show,
        quit: activation.quit,
    }
}

/// Encodes a primary-instance `RES:<pid>_<window_id>;` reply.
#[must_use]
pub fn encode_pipe_response(resp: InstanceResponse) -> String {
    encode_response(resp)
}

/// Decodes a `RES:<pid>_<window_id>;` reply from the primary instance.
#[must_use]
pub fn decode_pipe_response(wire: &str) -> Option<InstanceResponse> {
    parse_response(wire)
}

/// Acquires the Windows single-instance named mutex & pipe server for `instance_name`,
/// or forwards `activation` to an existing primary instance.
pub fn acquire_or_forward_single_instance(
    instance_name: &str,
    activation: &InstanceActivation,
) -> tg_platform::Result<SingleInstanceOutcome> {
    let queue = ActivationQueue::new();
    let queue_cb = queue.clone();
    let res = acquire_single_instance(instance_name, activation, move |incoming| {
        queue_cb.push(activation_to_incoming(incoming));
        InstanceResponse {
            process_id: std::process::id(),
            window_id: 0,
        }
    })?;

    match res {
        AcquireResult::Primary(guard) => Ok(SingleInstanceOutcome::Primary { guard, queue }),
        AcquireResult::Secondary { response } => {
            Ok(SingleInstanceOutcome::ForwardedToExisting { response })
        }
    }
}

/// Sends `CMD:quit;` to any running instance for `instance_name` and waits up to
/// [`CLEANUP_QUIT_TIMEOUT_MS`] for it to exit (`Sandbox::stopRunningInstance` in `sandbox.cpp`).
///
/// Returns `Ok(true)` if a running instance was asked to quit and exited cleanly,
/// `Ok(false)` if no running instance was found.
pub fn stop_running_instance(instance_name: &str) -> tg_platform::Result<bool> {
    let quit_act = InstanceActivation {
        urls: Vec::new(),
        args: Vec::new(),
        show: false,
        quit: true,
    };
    let res = acquire_single_instance(instance_name, &quit_act, |_| InstanceResponse {
        process_id: std::process::id(),
        window_id: 0,
    })?;
    match res {
        AcquireResult::Primary(guard) => {
            // No instance was running; drop guard immediately.
            drop(guard);
            Ok(false)
        }
        AcquireResult::Secondary { response } => {
            if let Some(resp) = response {
                let exited = wait_for_process_exit(
                    resp.process_id,
                    Duration::from_millis(CLEANUP_QUIT_TIMEOUT_MS),
                );
                if !exited {
                    tracing::warn!(
                        pid = resp.process_id,
                        "Running Rustgram instance did not exit within cleanup timeout"
                    );
                }
            }
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests_sec2a {
    use super::*;

    #[test]
    fn l11_relative_unc_and_device_send_paths_are_dropped() {
        // Only file entries (a mixed URL + file activation drops the files anyway).
        let activation = InstanceActivation {
            show: false,
            quit: false,
            urls: vec![
                r"..\secret.txt".to_owned(),
                "secret.txt".to_owned(),
                r"\\attacker\share\x.txt".to_owned(),
                "file://attacker/share/y.txt".to_owned(),
                r"\\?\C:\Windows\win.ini".to_owned(),
                "file:///C:/Users/test/photo.png".to_owned(),
            ],
            args: vec![],
        };
        let decoded = activation_to_incoming(activation);
        assert!(decoded.urls.is_empty());
        assert_eq!(
            decoded.send_paths,
            vec![PathBuf::from("C:/Users/test/photo.png")]
        );
    }
}
