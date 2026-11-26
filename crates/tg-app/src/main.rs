//! `rustgram` desktop binary entry point.
//!
//! Launches the `eframe` shell with `CoreSettings::native_window_frame`-controlled
//! frameless titlebar (`tg-platform::frameless`), taskbar thumbnail controls, and
//! Windows platform hooks via [`tg_app::startup::run_from_cli`].
//!
//! Port origin: `Telegram/SourceFiles/main.cpp`.
//!
//! Release builds use the Windows GUI subsystem (like tdesktop's `WinMain` build), so no console
//! window opens next to the app; logs go to `log.txt` / `DebugLogs` in the working directory.
//! Debug builds keep the console for `tracing` output.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[global_allocator]
static GLOBAL_ALLOC: tg_app::startup::profiler::ProfilingAllocator =
    tg_app::startup::profiler::ProfilingAllocator;

fn main() -> anyhow::Result<()> {
    tg_app::startup::profiler::init_from_env();
    tg_app::startup::frame_trace::init_from_env();
    tg_ui::fonts::enable_system_monospace();
    tg_app_history::history::inline_media::enable_background_decode();
    tg_app::startup::run_from_cli()
}
