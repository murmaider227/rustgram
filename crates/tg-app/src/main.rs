//! `rustgram` desktop binary entry point.
//!
//! Launches the `eframe` shell with `CoreSettings::native_window_frame`-controlled
//! frameless titlebar (`tg-platform::frameless`), taskbar thumbnail controls, and
//! Windows platform hooks via [`tg_app::startup::run_from_cli`].
//!
//! Port origin: `Telegram/SourceFiles/main.cpp`.

#[global_allocator]
static GLOBAL_ALLOC: tg_app::startup::profiler::ProfilingAllocator =
    tg_app::startup::profiler::ProfilingAllocator;

fn main() -> anyhow::Result<()> {
    tg_app::startup::profiler::init_from_env();
    tg_app::startup::run_from_cli()
}
