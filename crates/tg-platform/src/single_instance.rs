//! Single-instance lock and activation IPC over Windows named mutex + named pipe.
//!
//! Port of `Telegram/SourceFiles/core/sandbox.cpp` (local socket single-instance protocol
//! and 7-bit escaping) and `Telegram/SourceFiles/platform/win/specific_win.cpp`
//! (`SingleInstanceLocalServerName`, `ActivateOtherProcess`, `WaitForProcessExit`).

#[cfg(windows)]
use std::sync::Arc;
use std::time::Duration;

use crate::error::Result;

/// Maximum IPC message buffer size accepted from a secondary instance (1 MiB),
/// matching `kLocalClientBufferLimit` in `core/sandbox.cpp`.
pub const MAX_IPC_BUFFER_BYTES: usize = 1024 * 1024;

/// Maximum length of a single decoded URL record (8192 chars), matching `core/sandbox.cpp`.
pub const MAX_URL_LENGTH: usize = 8192;

/// Payload sent by a secondary instance when activating the primary instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstanceActivation {
    /// `tg://`, `tonsite://`, or local file URLs passed on the command line.
    pub urls: Vec<String>,
    /// Additional raw command-line arguments forwarded to the primary instance.
    pub args: Vec<String>,
    /// Request to show / bring the primary window to the foreground (`CMD:show;`).
    pub show: bool,
    /// Request to quit the primary instance (`CMD:quit;`).
    pub quit: bool,
}

/// Response sent back by the primary instance (`RES:<pid>_<window_id>;`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceResponse {
    /// Process ID of the primary instance.
    pub process_id: u32,
    /// Native window handle (`HWND` as `u64`) to activate, or `0` for none/any.
    pub window_id: u64,
}

/// Result of attempting to acquire the single-instance lock.
pub enum AcquireResult {
    /// This process is the primary instance. Keep the guard alive for the app lifetime.
    Primary(SingleInstanceGuard),
    /// Another instance is already running and was notified via the named pipe.
    Secondary {
        /// Response returned by the primary instance, if received.
        response: Option<InstanceResponse>,
    },
}

/// Guard holding the single-instance named mutex and background named-pipe listener.
///
/// Dropping this guard shuts down the pipe listener thread cleanly and releases the mutex.
pub struct SingleInstanceGuard {
    #[cfg(windows)]
    inner: Option<win_ipc::GuardInner>,
}

impl std::fmt::Debug for SingleInstanceGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleInstanceGuard").finish()
    }
}

/// Build the canonical single-instance identifier for a given app name and workdir hash.
pub fn single_instance_name(app_name: &str, working_dir_hash: &str) -> String {
    format!("{app_name}-{working_dir_hash}")
}

/// Escape an arbitrary UTF-8 string into tdesktop's 7-bit IPC wire format
/// (`EscapeTo7bit` in `core/sandbox.cpp`): code units `< 32`, `> 127`, `%`, or `;`
/// are encoded as `%XXXX` (4 lowercase hex digits per UTF-16 code unit).
pub fn escape_to_7bit(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 2);
    for code in value.encode_utf16() {
        if !(32..=127).contains(&code) || code == b'%' as u16 || code == b';' as u16 {
            use std::fmt::Write;
            let _ = write!(out, "%{code:04x}");
        } else if let Some(ch) = char::from_u32(u32::from(code)) {
            out.push(ch);
        }
    }
    out
}

/// Decode a string previously encoded with [`escape_to_7bit`]
/// (`EscapeFrom7bit` in `core/sandbox.cpp`).
pub fn unescape_from_7bit(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut utf16 = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 4 < bytes.len()
            && let (Some(h1), Some(h2), Some(h3), Some(h4)) = (
                hex_val(bytes[i + 1]),
                hex_val(bytes[i + 2]),
                hex_val(bytes[i + 3]),
                hex_val(bytes[i + 4]),
            )
        {
            let unit =
                (u16::from(h1) << 12) | (u16::from(h2) << 8) | (u16::from(h3) << 4) | u16::from(h4);
            utf16.push(unit);
            i += 5;
            continue;
        }
        utf16.push(u16::from(bytes[i]));
        i += 1;
    }
    String::from_utf16_lossy(&utf16)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Encode an [`InstanceActivation`] into tdesktop's semicolon-delimited wire format.
pub fn encode_activation_commands(activation: &InstanceActivation) -> String {
    let mut out = String::new();
    for url in &activation.urls {
        out.push_str("OPEN:");
        out.push_str(&escape_to_7bit(url));
        out.push(';');
    }
    for arg in &activation.args {
        out.push_str("ARG:");
        out.push_str(&escape_to_7bit(arg));
        out.push(';');
    }
    if activation.quit {
        out.push_str("CMD:quit;");
    } else if activation.show || (activation.urls.is_empty() && activation.args.is_empty()) {
        out.push_str("CMD:show;");
    }
    out
}

fn is_local_file_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("file://")
        || (!url.contains("://")
            && (url.starts_with('\\') || url.starts_with('/') || url.get(1..2) == Some(":")))
}

/// Parse semicolon-delimited IPC commands received from a secondary instance.
///
/// Enforces tdesktop's smuggling protection (`core/sandbox.cpp` lines 633-658):
/// if a launch carries any non-file external URL, local file URLs in the same launch are dropped.
pub fn parse_activation_commands(data: &str) -> InstanceActivation {
    let mut raw_urls = Vec::new();
    let mut args = Vec::new();
    let mut show = false;
    let mut quit = false;
    let mut has_open = false;

    let records: Vec<&str> = data.split(';').filter(|r| !r.is_empty()).collect();
    for rec in &records {
        if rec.starts_with("OPEN:") {
            has_open = true;
            break;
        }
    }

    for rec in records {
        if let Some(rest) = rec.strip_prefix("OPEN:") {
            let decoded: String = unescape_from_7bit(rest)
                .chars()
                .take(MAX_URL_LENGTH)
                .collect();
            raw_urls.push(decoded);
        } else if let Some(rest) = rec.strip_prefix("ARG:") {
            let decoded: String = unescape_from_7bit(rest)
                .chars()
                .take(MAX_URL_LENGTH)
                .collect();
            args.push(decoded);
        } else if let Some(cmd) = rec.strip_prefix("CMD:") {
            if has_open && cmd != "quit" {
                continue;
            }
            match cmd {
                "show" => show = true,
                "quit" => quit = true,
                _ => {}
            }
        }
    }

    let has_external = raw_urls.iter().any(|u| !is_local_file_url(u));
    let urls = raw_urls
        .into_iter()
        .filter(|u| !(has_external && is_local_file_url(u)))
        .collect();

    InstanceActivation {
        urls,
        args,
        show,
        quit,
    }
}

/// Format an [`InstanceResponse`] in tdesktop's `RES:<pid>_<window_id>;` format.
pub fn encode_response(resp: InstanceResponse) -> String {
    format!("RES:{}_{};", resp.process_id, resp.window_id)
}

/// Parse a `RES:<pid>_<window_id>;` response from the primary instance.
pub fn parse_response(data: &str) -> Option<InstanceResponse> {
    let idx = data.find("RES:")?;
    let rest = &data[idx + 4..];
    let end = rest.find(';')?;
    let body = &rest[..end];
    let (pid_str, win_str) = body.split_once('_')?;
    Some(InstanceResponse {
        process_id: pid_str.parse().ok()?,
        window_id: win_str.parse().ok()?,
    })
}

/// Acquire the single-instance lock for `instance_name`.
///
/// - If no other instance holds the mutex, spawns a background named-pipe server that
///   delivers subsequent activations to `on_activate` and returns [`AcquireResult::Primary`].
/// - If another instance is already running, connects to its named pipe, sends
///   `activation_to_send`, waits for `RES:<pid>_<hwnd>;`, activates the primary window
///   if non-zero, and returns [`AcquireResult::Secondary`].
pub fn acquire_single_instance<F>(
    instance_name: &str,
    activation_to_send: &InstanceActivation,
    on_activate: F,
) -> Result<AcquireResult>
where
    F: Fn(InstanceActivation) -> InstanceResponse + Send + Sync + 'static,
{
    #[cfg(windows)]
    {
        win_ipc::acquire(instance_name, activation_to_send, Arc::new(on_activate))
    }
    #[cfg(not(windows))]
    {
        let _ = (instance_name, activation_to_send, on_activate);
        Ok(AcquireResult::Primary(SingleInstanceGuard {}))
    }
}

/// Activate a window belonging to another running process (`specific_win.cpp` `ActivateOtherProcess`).
///
/// If `window_id` is `0`, picks the highest-priority top-level window belonging to `process_id`
/// (matching `FindToActivate` using `GWLP_USERDATA` or any visible top-level window).
pub fn activate_other_process(process_id: u32, window_id: u64) -> bool {
    #[cfg(windows)]
    {
        win_ipc::activate_other_process(process_id, window_id)
    }
    #[cfg(not(windows))]
    {
        let _ = (process_id, window_id);
        false
    }
}

/// Wait up to `timeout` for a process with `process_id` to terminate
/// (`WaitForProcessExit` in `specific_win.cpp`).
pub fn wait_for_process_exit(process_id: u32, timeout: Duration) -> bool {
    #[cfg(windows)]
    {
        win_ipc::wait_for_process_exit(process_id, timeout)
    }
    #[cfg(not(windows))]
    {
        let _ = (process_id, timeout);
        false
    }
}

#[cfg(windows)]
mod win_ipc {
    use super::*;
    use crate::app_id::win_impl::to_wide;
    use crate::error::PlatformError;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER,
        ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
        GetLastError, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WAIT_EVENT, WAIT_OBJECT_0,
    };
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR,
        PSID, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ALL_ACCESS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, FlushFileBuffers, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    };
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, NAMED_PIPE_MODE,
        PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
        PIPE_WAIT, WaitNamedPipeW,
    };
    use windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, GetCurrentProcess, INFINITE, OpenProcess, OpenProcessToken,
        PROCESS_SYNCHRONIZE, ReleaseMutex, ResetEvent, SetEvent, WaitForMultipleObjects,
        WaitForSingleObject,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, EnumWindows, GWLP_USERDATA, GetWindowLongPtrW,
        GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
    };
    use windows::core::{BOOL, PCWSTR};

    type ActivateFn = Arc<dyn Fn(InstanceActivation) -> InstanceResponse + Send + Sync>;

    pub(crate) struct GuardInner {
        mutex: HANDLE,
        stop_event: HANDLE,
        stop: Arc<AtomicBool>,
        server_thread: Option<JoinHandle<()>>,
    }

    // SAFETY: HANDLEs (`mutex`, `stop_event`) are owned exclusively by GuardInner and closed only
    // in Drop after the server thread has been joined.
    unsafe impl Send for GuardInner {}
    unsafe impl Sync for GuardInner {}

    impl Drop for GuardInner {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if !self.stop_event.is_invalid() {
                // SAFETY: Signalling our manual-reset stop event wakes overlapped pipe waits
                // immediately without any connect/create race window.
                let _ = unsafe { SetEvent(self.stop_event) };
            }

            if let Some(handle) = self.server_thread.take() {
                let _ = handle.join();
            }
            if !self.stop_event.is_invalid() {
                // SAFETY: Server thread has exited; safe to close our owned stop event handle.
                let _ = unsafe { CloseHandle(self.stop_event) };
            }
            if !self.mutex.is_invalid() {
                // SAFETY: Releasing and closing our owned single-instance mutex handle.
                unsafe {
                    let _ = ReleaseMutex(self.mutex);
                    let _ = CloseHandle(self.mutex);
                }
            }
        }
    }

    impl Drop for SingleInstanceGuard {
        fn drop(&mut self) {
            self.inner.take();
        }
    }

    fn sanitize_name(name: &str) -> String {
        name.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn mutex_name(instance_name: &str) -> String {
        format!("Local\\RustgramMutex_{}", sanitize_name(instance_name))
    }

    fn pipe_name(instance_name: &str) -> String {
        format!("\\\\.\\pipe\\RustgramPipe_{}", sanitize_name(instance_name))
    }

    pub(crate) fn pipe_open_mode(first_instance: bool) -> FILE_FLAGS_AND_ATTRIBUTES {
        let base = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first_instance {
            base | FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            base
        }
    }

    pub(crate) fn pipe_mode() -> NAMED_PIPE_MODE {
        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS
    }

    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: Closing owned Win32 kernel handle.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// Owns the backing buffers for a Windows `SECURITY_DESCRIPTOR` with an explicit DACL
    /// granting access solely to the current process user's SID.
    pub(crate) struct CurrentUserSecurityDescriptor {
        _token_user_buf: Vec<u8>,
        _acl_buf: Vec<u32>,
        sd: SECURITY_DESCRIPTOR,
    }

    impl CurrentUserSecurityDescriptor {
        pub(crate) fn new() -> Result<Self> {
            let mut token = HANDLE::default();
            // SAFETY: Opening current process token with TOKEN_QUERY right.
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
                .map_err(|e| PlatformError::from_win("OpenProcessToken", e))?;
            let _token_guard = OwnedHandle(token);

            let mut needed: u32 = 0;
            // SAFETY: Querying required buffer length for TokenUser.
            let probe = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
            if probe.is_ok() || needed == 0 {
                return Err(PlatformError::Ipc(
                    "GetTokenInformation(TokenUser) returned unexpected length".into(),
                ));
            }
            // SAFETY: Checking Win32 error immediately after GetTokenInformation probe.
            let probe_err = unsafe { GetLastError() };
            if probe_err != ERROR_INSUFFICIENT_BUFFER {
                return Err(PlatformError::last_win32("GetTokenInformation(probe)"));
            }

            let mut token_user_buf = vec![0u8; needed as usize];
            // SAFETY: Filling caller-allocated buffer of exact queried size with TOKEN_USER.
            unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    Some(token_user_buf.as_mut_ptr().cast()),
                    needed,
                    &mut needed,
                )
            }
            .map_err(|e| PlatformError::from_win("GetTokenInformation(TokenUser)", e))?;

            // SAFETY: `token_user_buf` was populated by GetTokenInformation(TokenUser).
            let user_sid: PSID =
                unsafe { (*(token_user_buf.as_ptr().cast::<TOKEN_USER>())).User.Sid };
            if user_sid.0.is_null() {
                return Err(PlatformError::Ipc(
                    "current process token returned null user SID".into(),
                ));
            }

            // SAFETY: `user_sid` is a valid SID returned by GetTokenInformation.
            let sid_len = unsafe { GetLengthSid(user_sid) } as usize;
            let raw_acl_size =
                std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>() + sid_len
                    - std::mem::size_of::<u32>();
            let acl_words = raw_acl_size.div_ceil(std::mem::size_of::<u32>());
            let acl_byte_len = (acl_words * std::mem::size_of::<u32>()) as u32;
            let mut acl_buf = vec![0u32; acl_words];
            let acl_ptr = acl_buf.as_mut_ptr().cast::<ACL>();

            // SAFETY: `acl_buf` is 4-byte aligned and sized for ACL + single ACCESS_ALLOWED_ACE + SID.
            unsafe { InitializeAcl(acl_ptr, acl_byte_len, ACL_REVISION) }
                .map_err(|e| PlatformError::from_win("InitializeAcl", e))?;
            // SAFETY: Adding single ACCESS_ALLOWED_ACE for the current user's SID only.
            unsafe { AddAccessAllowedAce(acl_ptr, ACL_REVISION, FILE_ALL_ACCESS.0, user_sid) }
                .map_err(|e| PlatformError::from_win("AddAccessAllowedAce", e))?;

            let mut sd = SECURITY_DESCRIPTOR::default();
            let sd_ptr = PSECURITY_DESCRIPTOR((&mut sd as *mut SECURITY_DESCRIPTOR).cast());
            // SAFETY: Initializing stack-allocated SECURITY_DESCRIPTOR with revision 1.
            unsafe { InitializeSecurityDescriptor(sd_ptr, SECURITY_DESCRIPTOR_REVISION) }
                .map_err(|e| PlatformError::from_win("InitializeSecurityDescriptor", e))?;
            // SAFETY: Setting explicit DACL (bDaclPresent=true, bDaclDefaulted=false) pointing to
            // `acl_buf`, which stays pinned inside this struct for the lifetime of the descriptor.
            unsafe { SetSecurityDescriptorDacl(sd_ptr, true, Some(acl_ptr), false) }
                .map_err(|e| PlatformError::from_win("SetSecurityDescriptorDacl", e))?;

            Ok(Self {
                _token_user_buf: token_user_buf,
                _acl_buf: acl_buf,
                sd,
            })
        }

        pub(crate) fn security_attributes(&mut self) -> SECURITY_ATTRIBUTES {
            // Re-bind DACL pointer after struct move so it always points to our owned `_acl_buf`.
            let acl_ptr = self._acl_buf.as_mut_ptr().cast::<ACL>();
            let sd_ptr = PSECURITY_DESCRIPTOR((&mut self.sd as *mut SECURITY_DESCRIPTOR).cast());
            // SAFETY: Refreshing DACL pointer inside self.sd to point to pinned heap `_acl_buf`.
            let _ = unsafe { SetSecurityDescriptorDacl(sd_ptr, true, Some(acl_ptr), false) };
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: (&mut self.sd as *mut SECURITY_DESCRIPTOR).cast(),
                bInheritHandle: BOOL(0),
            }
        }
    }

    pub(crate) fn create_pipe_instance(pipe_w: &[u16], first_instance: bool) -> Result<HANDLE> {
        let mut user_sd = CurrentUserSecurityDescriptor::new()?;
        let sa = user_sd.security_attributes();
        // SAFETY: Valid null-terminated UTF-16 pipe path, hardened overlapped named pipe flags
        // (`FILE_FLAG_FIRST_PIPE_INSTANCE` on initial creation + `PIPE_REJECT_REMOTE_CLIENTS`),
        // and explicit current-user-only DACL in `sa` valid for the duration of `CreateNamedPipeW`.
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(pipe_w.as_ptr()),
                pipe_open_mode(first_instance),
                pipe_mode(),
                PIPE_UNLIMITED_INSTANCES,
                8192,
                8192,
                0,
                Some(&sa),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(PlatformError::last_win32("CreateNamedPipeW"))
        } else {
            Ok(handle)
        }
    }

    fn wait_overlapped(
        pipe: HANDLE,
        ov: &OVERLAPPED,
        io_event: HANDLE,
        stop_event: HANDLE,
        timeout_ms: u32,
    ) -> Option<u32> {
        let handles = [stop_event, io_event];
        // SAFETY: Waiting on valid manual-reset stop and I/O event handles.
        let wait = unsafe { WaitForMultipleObjects(&handles, false, timeout_ms) };
        if wait == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            let mut transferred: u32 = 0;
            // SAFETY: `io_event` signaled; read overlapped completion status.
            if unsafe { GetOverlappedResult(pipe, ov, &mut transferred, false) }.is_ok() {
                return Some(transferred);
            }
            return None;
        }
        // Stop requested or timed out: cancel pending I/O and wait for the kernel to release `ov`
        // before returning so the caller's stack-allocated `OVERLAPPED` and buffers remain valid.
        // SAFETY: `pipe` and `ov` are valid; `GetOverlappedResult(..., true)` drains cancellation.
        unsafe {
            let _ = CancelIoEx(pipe, Some(ov));
            let mut drained: u32 = 0;
            let _ = GetOverlappedResult(pipe, ov, &mut drained, true);
        }
        None
    }

    fn connect_pipe_or_stop(pipe: HANDLE, io_event: HANDLE, stop_event: HANDLE) -> bool {
        // SAFETY: Resetting our reusable manual-reset I/O event before starting overlapped connect.
        let _ = unsafe { ResetEvent(io_event) };
        let mut ov = OVERLAPPED {
            hEvent: io_event,
            ..Default::default()
        };
        // SAFETY: `pipe` was created with `FILE_FLAG_OVERLAPPED`; `ov` stays alive through wait.
        let res = unsafe { ConnectNamedPipe(pipe, Some(&mut ov)) };
        if res.is_ok() {
            return true;
        }
        // SAFETY: Reading Win32 error immediately after `ConnectNamedPipe`.
        let err = unsafe { GetLastError() };
        if err == ERROR_PIPE_CONNECTED {
            return true;
        }
        if err == ERROR_IO_PENDING {
            return wait_overlapped(pipe, &ov, io_event, stop_event, INFINITE).is_some();
        }
        false
    }

    pub(crate) fn acquire(
        instance_name: &str,
        activation_to_send: &InstanceActivation,
        on_activate: ActivateFn,
    ) -> Result<AcquireResult> {
        if instance_name.is_empty() {
            return Err(PlatformError::InvalidArgument(
                "instance_name cannot be empty".into(),
            ));
        }

        let m_name = mutex_name(instance_name);
        let p_path = pipe_name(instance_name);
        let m_wide = to_wide(&m_name);

        // SAFETY: Creating a named mutex in the current user session namespace (`Local\`).
        let mutex = unsafe { CreateMutexW(None, true, PCWSTR(m_wide.as_ptr())) }
            .map_err(|e| PlatformError::from_win("CreateMutexW", e))?;
        // SAFETY: Checking whether the mutex already existed immediately after CreateMutexW.
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

        if already_exists {
            // SAFETY: Closing our handle to the existing mutex; we are the secondary instance.
            let _ = unsafe { CloseHandle(mutex) };
            let response = send_to_primary(&p_path, activation_to_send)?;
            if let Some(resp) = response
                && resp.window_id != 0
            {
                // SAFETY: Allow primary process to take foreground focus.
                let _ = unsafe { AllowSetForegroundWindow(resp.process_id) };
                activate_other_process(resp.process_id, resp.window_id);
            }
            return Ok(AcquireResult::Secondary { response });
        }

        // Create the initial pipe instance synchronously before returning so any secondary
        // instance launched immediately afterwards finds the pipe listening, and enforce
        // `FILE_FLAG_FIRST_PIPE_INSTANCE` so a pre-existing squatting pipe fails closed.
        let pipe_w = to_wide(&p_path);
        let first_pipe = match create_pipe_instance(&pipe_w, true) {
            Ok(h) => h,
            Err(e) => {
                // SAFETY: Cleaning up mutex on pipe creation failure.
                unsafe {
                    let _ = ReleaseMutex(mutex);
                    let _ = CloseHandle(mutex);
                }
                return Err(e);
            }
        };

        // SAFETY: Creating manual-reset event used to wake and stop the pipe listener cleanly.
        let stop_event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
            Ok(ev) => ev,
            Err(e) => {
                // SAFETY: Cleaning up pipe and mutex on event creation failure.
                unsafe {
                    let _ = CloseHandle(first_pipe);
                    let _ = ReleaseMutex(mutex);
                    let _ = CloseHandle(mutex);
                }
                return Err(PlatformError::from_win("CreateEventW(stop)", e));
            }
        };

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let pipe_w_thread = pipe_w;
        let first_pipe_raw = first_pipe.0 as usize;
        let stop_event_raw = stop_event.0 as usize;

        let server_thread = std::thread::Builder::new()
            .name("tg-single-instance".into())
            .spawn(move || {
                let mut current_pipe = HANDLE(first_pipe_raw as *mut _);
                let stop_ev = HANDLE(stop_event_raw as *mut _);
                // SAFETY: Creating per-thread manual-reset event for overlapped pipe I/O.
                let Ok(io_ev) = (unsafe { CreateEventW(None, true, false, PCWSTR::null()) }) else {
                    let _ = unsafe { CloseHandle(current_pipe) };
                    return;
                };

                loop {
                    if stop_clone.load(Ordering::SeqCst) {
                        // SAFETY: Closing unused pipe handle on shutdown.
                        let _ = unsafe { CloseHandle(current_pipe) };
                        break;
                    }

                    let connected = connect_pipe_or_stop(current_pipe, io_ev, stop_ev);

                    if stop_clone.load(Ordering::SeqCst) {
                        // SAFETY: Disconnecting and closing owned pipe handle on shutdown.
                        unsafe {
                            let _ = DisconnectNamedPipe(current_pipe);
                            let _ = CloseHandle(current_pipe);
                        }
                        break;
                    }

                    if connected {
                        handle_client_pipe(current_pipe, io_ev, stop_ev, &on_activate);
                    }

                    // Disconnect the client and reuse the exact same primary pipe instance handle
                    // so there is never an unowned window where a squatter could recreate the pipe.
                    // SAFETY: Disconnecting the served client from our owned pipe handle.
                    let disconnect_ok = unsafe { DisconnectNamedPipe(current_pipe) }.is_ok();

                    if stop_clone.load(Ordering::SeqCst) {
                        // SAFETY: Closing owned pipe handle on shutdown.
                        let _ = unsafe { CloseHandle(current_pipe) };
                        break;
                    }

                    if !disconnect_ok {
                        // SAFETY: Closing broken pipe handle before attempting recreation.
                        let _ = unsafe { CloseHandle(current_pipe) };
                        match create_pipe_instance(&pipe_w_thread, false) {
                            Ok(next) => current_pipe = next,
                            Err(e) => {
                                tracing::error!("Failed to recreate single-instance pipe: {e}");
                                break;
                            }
                        }
                    }
                }

                // SAFETY: Closing per-thread overlapped I/O event on exit.
                let _ = unsafe { CloseHandle(io_ev) };
            })?;

        Ok(AcquireResult::Primary(SingleInstanceGuard {
            inner: Some(GuardInner {
                mutex,
                stop_event,
                stop,
                server_thread: Some(server_thread),
            }),
        }))
    }

    fn read_overlapped_chunk(
        pipe: HANDLE,
        buf: &mut [u8],
        io_event: HANDLE,
        stop_event: HANDLE,
    ) -> Option<usize> {
        // SAFETY: Resetting reusable manual-reset I/O event before `ReadFile`.
        let _ = unsafe { ResetEvent(io_event) };
        let mut ov = OVERLAPPED {
            hEvent: io_event,
            ..Default::default()
        };
        let mut bytes_read: u32 = 0;
        // SAFETY: `pipe` is overlapped; `buf` and `ov` stay alive until completion or cancel drain.
        let res = unsafe { ReadFile(pipe, Some(buf), Some(&mut bytes_read), Some(&mut ov)) };
        if res.is_ok() {
            return Some(bytes_read as usize);
        }
        // SAFETY: Reading Win32 error immediately after `ReadFile`.
        if unsafe { GetLastError() } == ERROR_IO_PENDING {
            return wait_overlapped(pipe, &ov, io_event, stop_event, 5_000).map(|n| n as usize);
        }
        None
    }

    fn write_overlapped_all(
        pipe: HANDLE,
        data: &[u8],
        io_event: HANDLE,
        stop_event: HANDLE,
    ) -> bool {
        let mut offset = 0usize;
        while offset < data.len() {
            // SAFETY: Resetting reusable manual-reset I/O event before `WriteFile`.
            let _ = unsafe { ResetEvent(io_event) };
            let mut ov = OVERLAPPED {
                hEvent: io_event,
                ..Default::default()
            };
            let mut written: u32 = 0;
            // SAFETY: `pipe` is overlapped; slice and `ov` stay alive until completion or cancel drain.
            let res = unsafe {
                WriteFile(
                    pipe,
                    Some(&data[offset..]),
                    Some(&mut written),
                    Some(&mut ov),
                )
            };
            let n = if res.is_ok() {
                written as usize
            } else if unsafe { GetLastError() } == ERROR_IO_PENDING {
                match wait_overlapped(pipe, &ov, io_event, stop_event, 5_000) {
                    Some(n) => n as usize,
                    None => return false,
                }
            } else {
                return false;
            };
            if n == 0 {
                return false;
            }
            offset += n;
        }
        true
    }

    fn handle_client_pipe(
        pipe: HANDLE,
        io_event: HANDLE,
        stop_event: HANDLE,
        on_activate: &ActivateFn,
    ) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];

        while let Some(n) = read_overlapped_chunk(pipe, &mut chunk, io_event, stop_event) {
            if n == 0 {
                break;
            }
            if buf.len() + n > MAX_IPC_BUFFER_BYTES {
                tracing::warn!("Single-instance IPC client buffer overflow");
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.last() == Some(&b';') {
                break;
            }
        }

        if buf.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&buf);
        let activation = parse_activation_commands(&text);
        let resp = on_activate(activation);
        let resp_str = encode_response(resp);
        if write_overlapped_all(pipe, resp_str.as_bytes(), io_event, stop_event) {
            // SAFETY: Flushing response bytes to the connected client before disconnecting.
            let _ = unsafe { FlushFileBuffers(pipe) };
        }
    }

    fn send_to_primary(
        pipe_path: &str,
        activation: &InstanceActivation,
    ) -> Result<Option<InstanceResponse>> {
        let pipe_w = to_wide(pipe_path);
        let handle = loop {
            // SAFETY: Opening existing named pipe hosted by the primary instance.
            let res = unsafe {
                CreateFileW(
                    PCWSTR(pipe_w.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            };
            match res {
                Ok(h) if !h.is_invalid() => break h,
                _ => {
                    // SAFETY: Reading last Win32 error immediately after CreateFileW.
                    let err = unsafe { GetLastError() };
                    if err == ERROR_PIPE_BUSY {
                        // SAFETY: Waiting up to 2 seconds for a pipe instance to become available.
                        if unsafe { WaitNamedPipeW(PCWSTR(pipe_w.as_ptr()), 2000) }.as_bool() {
                            continue;
                        }
                    }
                    return Err(PlatformError::Ipc(format!(
                        "could not connect to primary instance pipe '{pipe_path}' (error {})",
                        err.0
                    )));
                }
            }
        };

        let payload = encode_activation_commands(activation);
        let mut written: u32 = 0;
        // SAFETY: Writing encoded commands to the connected pipe.
        let write_res =
            unsafe { WriteFile(handle, Some(payload.as_bytes()), Some(&mut written), None) };
        if let Err(e) = write_res {
            // SAFETY: Closing client pipe handle on error.
            let _ = unsafe { CloseHandle(handle) };
            return Err(PlatformError::from_win("WriteFile(pipe)", e));
        }

        let mut resp_buf = [0u8; 256];
        let mut read_bytes: u32 = 0;
        // SAFETY: Reading `RES:<pid>_<hwnd>;` reply from primary instance.
        let _ = unsafe { ReadFile(handle, Some(&mut resp_buf), Some(&mut read_bytes), None) };
        // SAFETY: Closing owned client pipe handle.
        let _ = unsafe { CloseHandle(handle) };

        if read_bytes == 0 {
            return Ok(None);
        }
        let resp_str = String::from_utf8_lossy(&resp_buf[..read_bytes as usize]);
        Ok(parse_response(&resp_str))
    }

    struct FindWindowRequest {
        process_id: u32,
        window_id: u64,
        result: HWND,
        result_level: u32,
    }

    unsafe extern "system" fn find_to_activate(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` points to stack-allocated `FindWindowRequest` valid during EnumWindows.
        let req = unsafe { &mut *(lparam.0 as *mut FindWindowRequest) };
        let mut pid: u32 = 0;
        // SAFETY: Querying window owning process ID.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != req.process_id {
            return BOOL(1);
        }
        let wid = (hwnd.0 as usize as u64) & 0xFFFF_FFFF;
        if req.window_id != 0 && wid == req.window_id {
            req.result = hwnd;
            req.result_level = 3;
            return BOOL(0);
        }
        // SAFETY: Reading GWLP_USERDATA priority matching `specific_win.cpp` (1 or 2).
        let data = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as u32;
        if (data == 1 || data == 2) && data > req.result_level {
            req.result = hwnd;
            req.result_level = data;
        } else if req.result_level == 0 && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            req.result = hwnd;
        }
        BOOL(1)
    }

    pub(crate) fn activate_other_process(process_id: u32, window_id: u64) -> bool {
        let mut req = FindWindowRequest {
            process_id,
            window_id,
            result: HWND::default(),
            result_level: 0,
        };
        // SAFETY: EnumWindows invokes `find_to_activate` synchronously on this thread.
        let _ = unsafe {
            EnumWindows(
                Some(find_to_activate),
                LPARAM(&mut req as *mut FindWindowRequest as isize),
            )
        };
        if !req.result.0.is_null() {
            // SAFETY: Best-effort activation of found top-level window.
            unsafe {
                let _ = SetForegroundWindow(req.result);
                let _ = SetFocus(Some(req.result));
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn wait_for_process_exit(process_id: u32, timeout: Duration) -> bool {
        // SAFETY: Opening process handle with SYNCHRONIZE right only.
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, process_id) }) else {
            // SAFETY: ERROR_INVALID_PARAMETER means the process already exited.
            return unsafe { GetLastError() } == ERROR_INVALID_PARAMETER;
        };
        let ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        // SAFETY: Waiting on valid process handle, then closing it.
        let wait = unsafe { WaitForSingleObject(handle, ms) };
        let _ = unsafe { CloseHandle(handle) };
        wait == WAIT_OBJECT_0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_bit_escape_roundtrip() {
        let samples = [
            "tg://resolve?domain=rustgram&text=hello;world%20!",
            "tonsite://example.ton/path?a=1&b=привет",
            "line1\nline2\t🚀",
        ];
        for s in samples {
            let enc = escape_to_7bit(s);
            assert!(enc.bytes().all(|b| (32..=127).contains(&b) && b != b';'));
            assert_eq!(unescape_from_7bit(&enc), s);
        }
    }

    #[test]
    fn activation_command_encoding_and_smuggling_filter() {
        let act = InstanceActivation {
            urls: vec!["tg://resolve?domain=test".into()],
            args: vec!["-workdir".into(), "C:\\data".into()],
            show: false,
            quit: false,
        };
        let wire = encode_activation_commands(&act);
        let parsed = parse_activation_commands(&wire);
        assert_eq!(parsed.urls, vec!["tg://resolve?domain=test"]);
        assert_eq!(parsed.args, vec!["-workdir", "C:\\data"]);
        assert!(!parsed.quit);

        // Mixed external URL + local file drops the local file (sandbox.cpp security rule).
        let mixed = "OPEN:tg://resolve?domain=foo;OPEN:file:///C:/secret.txt;CMD:show;";
        let parsed_mixed = parse_activation_commands(mixed);
        assert_eq!(parsed_mixed.urls, vec!["tg://resolve?domain=foo"]);
        assert!(!parsed_mixed.show); // CMD:show ignored when OPEN: is present
    }

    #[test]
    fn response_encode_decode() {
        let r = InstanceResponse {
            process_id: 4242,
            window_id: 0x0012_3456,
        };
        let wire = encode_response(r);
        assert_eq!(wire, "RES:4242_1193046;");
        assert_eq!(parse_response(&wire), Some(r));
        assert_eq!(parse_response("invalid"), None);
    }

    #[cfg(windows)]
    #[test]
    fn single_instance_primary_and_secondary_pipe_ipc() {
        use std::sync::Mutex;

        let name = format!("test_inst_{}", std::process::id());
        let received = Arc::new(Mutex::new(Vec::<InstanceActivation>::new()));
        let recv_cb = Arc::clone(&received);

        let primary = acquire_single_instance(
            &name,
            &InstanceActivation {
                show: true,
                ..Default::default()
            },
            move |act| {
                recv_cb.lock().unwrap().push(act);
                InstanceResponse {
                    process_id: std::process::id(),
                    window_id: 0,
                }
            },
        )
        .unwrap();

        let _guard = match primary {
            AcquireResult::Primary(g) => g,
            AcquireResult::Secondary { .. } => panic!("expected primary instance"),
        };

        // Second acquisition with the same name must route through the named pipe.
        let secondary_payload = InstanceActivation {
            urls: vec!["tg://resolve?domain=rustgram".into()],
            args: vec!["--demo".into()],
            show: false,
            quit: false,
        };
        let second = acquire_single_instance(&name, &secondary_payload, |_| InstanceResponse {
            process_id: 0,
            window_id: 0,
        })
        .unwrap();

        match second {
            AcquireResult::Primary(_) => panic!("expected secondary instance"),
            AcquireResult::Secondary { response } => {
                let resp = response.expect("expected response from primary");
                assert_eq!(resp.process_id, std::process::id());
                assert_eq!(resp.window_id, 0);
            }
        }

        let got = received.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].urls, vec!["tg://resolve?domain=rustgram"]);
        assert_eq!(got[0].args, vec!["--demo"]);
    }

    #[cfg(windows)]
    #[test]
    fn d5_single_instance_pipe_enforces_first_instance_reject_remote_and_user_only_dacl() {
        use crate::app_id::win_impl::to_wide;
        use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, HANDLE};
        use windows::Win32::Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
            GetKernelObjectSecurity, GetSecurityDescriptorDacl, GetTokenInformation,
            PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
        };
        use windows::Win32::Storage::FileSystem::FILE_FLAG_FIRST_PIPE_INSTANCE;
        use windows::Win32::System::Pipes::PIPE_REJECT_REMOTE_CLIENTS;
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        use windows::core::BOOL;

        // 1. Verify open/pipe mode flags include FILE_FLAG_FIRST_PIPE_INSTANCE and PIPE_REJECT_REMOTE_CLIENTS.
        let open_flags = win_ipc::pipe_open_mode(true);
        assert_ne!(
            open_flags.0 & FILE_FLAG_FIRST_PIPE_INSTANCE.0,
            0,
            "first pipe instance must set FILE_FLAG_FIRST_PIPE_INSTANCE"
        );
        let mode_flags = win_ipc::pipe_mode();
        assert_ne!(
            mode_flags.0 & PIPE_REJECT_REMOTE_CLIENTS.0,
            0,
            "named pipe must set PIPE_REJECT_REMOTE_CLIENTS"
        );

        // 2. Create a pipe via `create_pipe_instance(..., true)` and verify:
        //    (a) a second `create_pipe_instance(..., true)` on the same name fails (squatter protection),
        //    (b) the live kernel object has an explicit DACL containing ONLY 1 ACE for the current user's SID.
        let pipe_path = format!(
            "\\\\.\\pipe\\RustgramD5SecTest_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let pipe_w = to_wide(&pipe_path);
        let h1 = win_ipc::create_pipe_instance(&pipe_w, true).expect("create hardened pipe");

        // Second first-instance creation on the same name MUST fail with ERROR_ACCESS_DENIED.
        let dup = win_ipc::create_pipe_instance(&pipe_w, true);
        assert!(
            dup.is_err(),
            "FILE_FLAG_FIRST_PIPE_INSTANCE must reject creating a duplicate instance on the same pipe name"
        );
        let last_err = unsafe { GetLastError() };
        assert_eq!(
            last_err, ERROR_ACCESS_DENIED,
            "expected ERROR_ACCESS_DENIED when squatting an existing pipe"
        );

        // Query the live pipe handle's DACL from the Windows kernel.
        let mut sd_needed: u32 = 0;
        let _ = unsafe {
            GetKernelObjectSecurity(h1, DACL_SECURITY_INFORMATION.0, None, 0, &mut sd_needed)
        };
        assert!(sd_needed > 0, "expected non-empty security descriptor");
        let mut sd_buf = vec![0u8; sd_needed as usize];
        unsafe {
            GetKernelObjectSecurity(
                h1,
                DACL_SECURITY_INFORMATION.0,
                Some(PSECURITY_DESCRIPTOR(sd_buf.as_mut_ptr().cast())),
                sd_needed,
                &mut sd_needed,
            )
        }
        .expect("GetKernelObjectSecurity");

        let mut dacl_present = BOOL(0);
        let mut dacl_defaulted = BOOL(0);
        let mut dacl_ptr: *mut ACL = std::ptr::null_mut();
        unsafe {
            GetSecurityDescriptorDacl(
                PSECURITY_DESCRIPTOR(sd_buf.as_mut_ptr().cast()),
                &mut dacl_present,
                &mut dacl_ptr,
                &mut dacl_defaulted,
            )
        }
        .expect("GetSecurityDescriptorDacl");
        assert!(dacl_present.as_bool(), "DACL must be present on pipe");
        assert!(
            !dacl_defaulted.as_bool(),
            "DACL must be explicit (not defaulted)"
        );
        assert!(
            !dacl_ptr.is_null(),
            "DACL must not be a NULL (allow-all) DACL"
        );

        let mut acl_info = ACL_SIZE_INFORMATION::default();
        unsafe {
            GetAclInformation(
                dacl_ptr,
                (&mut acl_info as *mut ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        }
        .expect("GetAclInformation");
        assert_eq!(
            acl_info.AceCount, 1,
            "pipe DACL must contain exactly 1 ACE (current user only), got {}",
            acl_info.AceCount
        );

        let mut ace_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        unsafe { GetAce(dacl_ptr, 0, &mut ace_ptr) }.expect("GetAce");
        let ace = unsafe { &*(ace_ptr.cast::<ACCESS_ALLOWED_ACE>()) };
        let ace_sid = PSID((&ace.SidStart as *const u32).cast_mut().cast());

        // Compare ACE SID against current process user's SID.
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .expect("OpenProcessToken");
        let mut tok_len: u32 = 0;
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut tok_len) };
        let mut tok_buf = vec![0u8; tok_len as usize];
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(tok_buf.as_mut_ptr().cast()),
                tok_len,
                &mut tok_len,
            )
        }
        .expect("GetTokenInformation");
        let current_sid = unsafe { (*(tok_buf.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        assert!(
            unsafe { EqualSid(ace_sid, current_sid) }.is_ok(),
            "pipe DACL ACE SID must equal the current user's SID"
        );

        let _ = unsafe { CloseHandle(token) };
        let _ = unsafe { CloseHandle(h1) };

        // 3. Also verify sequential activations through the reused single-instance pipe handle.
        let inst_name = format!("d5_seq_test_{}", std::process::id());
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_cb = Arc::clone(&seen);
        let primary =
            acquire_single_instance(&inst_name, &InstanceActivation::default(), move |act| {
                seen_cb.lock().unwrap().extend(act.urls);
                InstanceResponse {
                    process_id: std::process::id(),
                    window_id: 0,
                }
            })
            .expect("acquire primary");
        let _guard = match primary {
            AcquireResult::Primary(g) => g,
            AcquireResult::Secondary { .. } => panic!("expected primary"),
        };

        for idx in 1..=2 {
            let url = format!("tg://resolve?domain=client{idx}");
            let res = acquire_single_instance(
                &inst_name,
                &InstanceActivation {
                    urls: vec![url],
                    ..Default::default()
                },
                |_| InstanceResponse {
                    process_id: 0,
                    window_id: 0,
                },
            )
            .expect("secondary connect");
            assert!(matches!(
                res,
                AcquireResult::Secondary { response: Some(_) }
            ));
        }
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "tg://resolve?domain=client1".to_owned(),
                "tg://resolve?domain=client2".to_owned()
            ]
        );
    }
}
