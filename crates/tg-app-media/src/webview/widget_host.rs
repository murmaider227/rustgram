//! Active HTML widget host coordinating `WidgetStateMachine`, `SandboxPolicy`,
//! `WebViewWidgetHost`, `WidgetSandboxLauncher`, and `WidgetMeter`.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_runtime.cpp` and
//! `Telegram/SourceFiles/platform/win/widget_sandbox_win.cpp`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tg_platform::widget_sandbox::{
    WidgetHelperCompletion, WidgetHelperProcess, WidgetSandboxConfig, WidgetSandboxLauncher,
    Win32WidgetSandboxLauncher, resolve_widget_helper_path_from_current_exe,
};
use tg_tgw::{
    BubbleThemeColors, HeightChange, HostToRendererMsg, MeterConfig, MeterVerdict,
    RendererToHostMsg, ResizeAnchor, RuntimeState, SandboxPolicy, WebViewWidgetHost, WidgetMeter,
    WidgetStateMachine, WidgetTheme, build_bubble_theme, decode_renderer_message,
    encode_host_message,
};

use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Active widget host coordinating [`WidgetStateMachine`], [`SandboxPolicy`], an optional
/// embedded [`WebViewWidgetHost`], an OS-sandboxed [`WidgetHelperProcess`], and a [`WidgetMeter`]
/// watchdog for a chat widget message.
pub struct LiveWidgetHost {
    /// Owning peer ID.
    pub peer_id: PeerId,
    /// Owning message ID.
    pub msg_id: MsgId,
    /// Lifecycle state machine.
    pub state_machine: WidgetStateMachine,
    /// Active OS + CSP sandbox policy.
    pub policy: SandboxPolicy,
    /// Active theme tokens.
    pub theme: WidgetTheme,
    /// Embedded WebView2 host when created and running.
    webview: Option<WebViewWidgetHost>,
    /// OS sandbox launcher for out-of-process widget helper isolation (`Win32WidgetSandboxLauncher`
    /// by default, injectable with `FakeWidgetSandboxLauncher` in tests).
    sandbox_launcher: Arc<dyn WidgetSandboxLauncher>,
    /// Configured path to `rustgram-widget-helper.exe` (forwarded from startup settings).
    helper_executable: Option<PathBuf>,
    /// Running sandboxed widget helper process and its framed IPC channel, if launched.
    sandbox_helper: Option<Box<dyn WidgetHelperProcess>>,
    /// Per-widget frame-time, CPU-budget, and memory watchdog (`tg-tgw`).
    pub meter: WidgetMeter,
    /// Last received bridge messages for inspection/processing.
    pub inbox: Arc<Mutex<Vec<String>>>,
}

impl LiveWidgetHost {
    /// Creates a new [`LiveWidgetHost`] in [`RuntimeState::Idle`] with a validated sandbox policy,
    /// [`Win32WidgetSandboxLauncher`], and default [`WidgetMeter`] budgets.
    #[must_use]
    pub fn new(peer_id: PeerId, msg_id: MsgId, html: &[u8], dark: bool) -> Self {
        let theme = if dark {
            build_bubble_theme(&BubbleThemeColors::dark())
        } else {
            build_bubble_theme(&BubbleThemeColors::light())
        };
        let mut sm = WidgetStateMachine::new(html);
        sm.set_visible(true);
        sm.mark_painted();
        sm.set_theme(theme.clone());
        Self {
            peer_id,
            msg_id,
            state_machine: sm,
            policy: SandboxPolicy::for_current_target(),
            theme,
            webview: None,
            sandbox_launcher: Arc::new(Win32WidgetSandboxLauncher::new()),
            helper_executable: None,
            sandbox_helper: None,
            meter: WidgetMeter::new(),
            inbox: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Overrides the [`WidgetSandboxLauncher`] used by [`Self::launch_sandbox_helper`] (e.g.
    /// `FakeWidgetSandboxLauncher` in tests).
    #[must_use]
    pub fn with_sandbox_launcher(mut self, launcher: Arc<dyn WidgetSandboxLauncher>) -> Self {
        self.sandbox_launcher = launcher;
        self
    }

    /// Configures the path to `rustgram-widget-helper.exe` used by
    /// [`Self::launch_configured_sandbox_helper`].
    #[must_use]
    pub fn with_helper_executable(mut self, path: PathBuf) -> Self {
        self.helper_executable = Some(path);
        self
    }

    /// Sets the path to `rustgram-widget-helper.exe` (typically resolved next to `rustgram.exe`
    /// during application startup).
    pub fn set_helper_executable(&mut self, path: PathBuf) {
        self.helper_executable = Some(path);
    }

    /// Returns the explicitly configured helper executable path, if any.
    #[must_use]
    pub fn helper_executable(&self) -> Option<&Path> {
        self.helper_executable.as_deref()
    }

    /// Returns the effective `rustgram-widget-helper.exe` path: the startup-configured path if set,
    /// otherwise resolved next to the current executable.
    #[must_use]
    pub fn resolved_helper_executable(&self) -> PathBuf {
        self.helper_executable
            .clone()
            .unwrap_or_else(resolve_widget_helper_path_from_current_exe)
    }

    /// Launches the sandboxed widget helper using [`Self::resolved_helper_executable`] and default
    /// [`WidgetSandboxConfig`] limits.
    pub fn launch_configured_sandbox_helper(&mut self) -> Result<u32, String> {
        let config = WidgetSandboxConfig::for_executable(self.resolved_helper_executable());
        self.launch_sandbox_helper(&config)
    }

    /// Replaces the active [`WidgetSandboxLauncher`].
    pub fn set_sandbox_launcher(&mut self, launcher: Arc<dyn WidgetSandboxLauncher>) {
        self.sandbox_launcher = launcher;
    }

    /// Replaces the active [`WidgetMeter`] configuration.
    pub fn set_meter_config(&mut self, config: MeterConfig) {
        self.meter = WidgetMeter::with_config(config);
    }

    /// Returns `true` if the configured widget helper sandbox launcher is supported on this host.
    #[must_use]
    pub fn is_sandbox_supported(&self) -> bool {
        self.sandbox_launcher.is_supported()
    }

    /// Returns a human-readable explanation if the widget helper sandbox is unsupported.
    #[must_use]
    pub fn sandbox_unsupported_reason(&self) -> Option<String> {
        self.sandbox_launcher.unsupported_reason()
    }

    /// Returns the PID of the running sandboxed widget helper process, if any.
    #[must_use]
    pub fn sandbox_pid(&self) -> Option<u32> {
        self.sandbox_helper.as_ref().map(|p| p.pid())
    }

    /// Launches an out-of-process sandboxed widget helper using the configured
    /// [`WidgetSandboxLauncher`], driving [`WidgetStateMachine`] through
    /// `Waiting -> Constructing -> Live` (or `Failed` on error).
    pub fn launch_sandbox_helper(&mut self, config: &WidgetSandboxConfig) -> Result<u32, String> {
        if let Some(prev) = self.sandbox_helper.take() {
            let _ = prev.terminate();
        }
        let gen_id = self
            .state_machine
            .request_start()
            .map_err(|e| e.to_string())?;
        self.state_machine
            .begin_construct(gen_id)
            .map_err(|e| e.to_string())?;

        if !self.sandbox_launcher.is_supported() {
            let reason = self
                .sandbox_launcher
                .unsupported_reason()
                .unwrap_or_else(|| "widget helper sandbox is not supported".to_owned());
            self.state_machine.fail(&reason);
            return Err(reason);
        }

        match self.sandbox_launcher.launch(config) {
            Ok(helper) => {
                let pid = helper.pid();
                self.sandbox_helper = Some(helper);
                let _ = self.state_machine.mark_live(gen_id);
                Ok(pid)
            }
            Err(err) => {
                let msg = err.to_string();
                self.state_machine.fail(&msg);
                Err(msg)
            }
        }
    }

    /// Sends one length-prefixed IPC frame to the running sandboxed widget helper process.
    pub fn send_sandbox_frame(&self, payload: &[u8]) -> Result<(), String> {
        let helper = self
            .sandbox_helper
            .as_ref()
            .ok_or_else(|| "no sandboxed widget helper is running".to_owned())?;
        helper
            .channel()
            .send_frame(payload)
            .map_err(|e| e.to_string())
    }

    /// Receives the next length-prefixed IPC frame from the running sandboxed widget helper process.
    pub fn recv_sandbox_frame(&self) -> Result<Vec<u8>, String> {
        let helper = self
            .sandbox_helper
            .as_ref()
            .ok_or_else(|| "no sandboxed widget helper is running".to_owned())?;
        helper.channel().recv_frame().map_err(|e| e.to_string())
    }

    /// Terminates any running sandboxed widget helper process and closes its IPC channel.
    pub fn terminate_sandbox_helper(&mut self) -> Result<Option<WidgetHelperCompletion>, String> {
        let Some(helper) = self.sandbox_helper.take() else {
            return Ok(None);
        };
        helper.terminate().map(Some).map_err(|e| e.to_string())
    }

    /// Records a frame duration in [`Self::meter`] and enforces [`MeterVerdict::Suspend`] by
    /// terminating the sandbox helper / closing the webview and failing the state machine.
    pub fn record_frame_and_enforce(&mut self, now_secs: f64, frame_secs: f64) -> MeterVerdict {
        let verdict = self.meter.record_frame(now_secs, frame_secs);
        self.apply_meter_verdict(&verdict);
        verdict
    }

    /// Updates tracked memory usage in [`Self::meter`] and enforces [`MeterVerdict::Suspend`].
    pub fn update_memory_and_enforce(&mut self, bytes: usize, now_secs: f64) -> MeterVerdict {
        let verdict = self.meter.update_memory(bytes, now_secs);
        self.apply_meter_verdict(&verdict);
        verdict
    }

    /// Evaluates the current [`MeterVerdict`] at `now_secs`.
    #[must_use]
    pub fn meter_verdict(&self, now_secs: f64) -> MeterVerdict {
        self.meter.verdict(now_secs)
    }

    fn apply_meter_verdict(&mut self, verdict: &MeterVerdict) {
        if let MeterVerdict::Suspend { detail, .. } = verdict {
            if let Some(helper) = self.sandbox_helper.take() {
                if let Some(ctrl) = verdict.to_stop_control()
                    && let Ok(frame) = ctrl.encode()
                {
                    let _ = helper.channel().send_frame(&frame);
                }
                let _ = helper.terminate();
            }
            if let Some(wv) = self.webview.take() {
                wv.close();
            }
            self.state_machine.fail(detail.clone());
        }
    }

    /// Launches the embedded WebView2 sandbox host if the runtime is installed, driving
    /// [`WidgetStateMachine`] through `Waiting -> Constructing -> Live` (or `Failed` on error).
    pub fn launch_webview(&mut self, html: &str) -> Result<(), String> {
        let gen_id = self
            .state_machine
            .request_start()
            .map_err(|e| e.to_string())?;
        self.state_machine
            .begin_construct(gen_id)
            .map_err(|e| e.to_string())?;

        if !WebViewWidgetHost::availability().is_supported() {
            self.state_machine
                .fail("WebView2 runtime is not available on this machine");
            return Err("WebView2 runtime is not available on this machine".to_owned());
        }

        let inbox = Arc::clone(&self.inbox);
        match WebViewWidgetHost::create(None, html, &self.theme, self.policy.clone(), move |msg| {
            if let Ok(mut guard) = inbox.lock() {
                guard.push(msg);
            }
        }) {
            Ok(host) => {
                self.webview = Some(host);
                let _ = self.state_machine.mark_live(gen_id);
                Ok(())
            }
            Err(err) => {
                self.state_machine.fail(err.to_string());
                Err(err.to_string())
            }
        }
    }

    /// Records a height change reported by the widget.
    pub fn on_resize(&mut self, height: i32) {
        self.state_machine
            .record_height(height, HeightChange::Own, ResizeAnchor::Top);
    }

    /// Drains and decodes all queued [`RendererToHostMsg`] bridge messages from WebView2.
    pub fn drain_messages(&mut self) -> Vec<RendererToHostMsg> {
        let raw: Vec<String> = self
            .inbox
            .lock()
            .map(|mut g| g.drain(..).collect())
            .unwrap_or_default();
        raw.into_iter()
            .filter_map(|line| decode_renderer_message(&line).ok())
            .collect()
    }

    /// Encodes and posts a [`HostToRendererMsg`] into the active sandboxed WebView2 page.
    pub fn send_host_message(&self, msg: &HostToRendererMsg) -> Result<(), String> {
        let json = encode_host_message(msg).map_err(|e| e.to_string())?;
        if let Some(ref wv) = self.webview {
            wv.post_host_json(&json).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> RuntimeState {
        self.state_machine.state()
    }

    /// Closes the embedded WebView2 host and terminates any running sandboxed helper process.
    pub fn close(&mut self) {
        if let Some(helper) = self.sandbox_helper.take() {
            let _ = helper.terminate();
        }
        if let Some(wv) = self.webview.take() {
            wv.close();
        }
        self.state_machine.release(false);
    }
}
