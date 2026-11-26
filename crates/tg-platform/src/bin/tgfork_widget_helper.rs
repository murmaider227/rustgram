//! Out-of-process sandboxed widget helper executable (`rustgram-widget-helper.exe`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/_other/widget_helper.cpp`
//! - `Telegram/SourceFiles/_other/widget_helper_win.cpp`
//! - `Telegram/SourceFiles/_other/widget_helper_serve.cpp`
//!
//! Spawned by `tg_platform::widget_sandbox::Win32WidgetSandboxLauncher` with the decimal
//! inherited named-pipe handle as `argv[1]`. Adopts the client byte pipe, drops its startup
//! impersonation token into the Low-IL AppContainer lockdown token, verifies sandbox Job Object
//! limits, and serves the length-prefixed framed IPC loop until stopped or disconnected.
//! Release builds use the GUI subsystem so the helper never opens a console window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

use tg_platform::widget_sandbox::{
    DEFAULT_MAX_MESSAGE_BYTES, JobLimitsConfig, adopt_inherited_widget_channel,
    lockdown_widget_helper, serve_widget_helper_channel,
};

const MAX_REASON_BYTES: usize = 4096;

fn exit_with_error(reason: &str) -> ExitCode {
    let truncated = if reason.len() > MAX_REASON_BYTES {
        &reason[..MAX_REASON_BYTES]
    } else {
        reason
    };
    eprintln!("Widget helper: {truncated}");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.len() > 2 {
        return exit_with_error("unexpected arguments");
    }
    let skip_lockdown = args.get(1).is_some_and(|flag| flag == "--no-lockdown");
    if args.len() == 2 && !skip_lockdown {
        return exit_with_error("unexpected helper option");
    }

    let channel = match adopt_inherited_widget_channel(&args[0], DEFAULT_MAX_MESSAGE_BYTES) {
        Ok(ch) => ch,
        Err(err) => return exit_with_error(&err.to_string()),
    };

    if !skip_lockdown && let Err(err) = lockdown_widget_helper(&JobLimitsConfig::default()) {
        return exit_with_error(&err.to_string());
    }

    match serve_widget_helper_channel(channel.as_ref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => exit_with_error(&err.to_string()),
    }
}
