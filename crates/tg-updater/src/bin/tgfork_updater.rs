//! Standalone `rustgram-updater` helper binary.
//!
//! Port of `Telegram/SourceFiles/_other/updater_win.cpp`.
//! Waits for the main `rustgram.exe` process to exit, copies staged files from
//! `<working dir>/tupdates/temp` into the installation directory (with automatic rollback on
//! failure), cleans up `tupdates`, updates the Windows uninstall registry version, and relaunches
//! `rustgram.exe` with `-noupdate` plus forwarded flags (`-workdir`, `-key`, `-debug`,
//! `-autostart`, `-startintray`).
//! Release builds use the GUI subsystem (like tdesktop's `Updater.exe`), so applying an update
//! never flashes a console window; `-debug` writes its log to a file instead.
//! The working dir is `-workdir`, else the current directory (as in tdesktop, which the app sets to
//! its working dir), else the app's default working dir when only that one holds a staged update.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use tg_updater::apply::{
    SystemAppLauncher, SystemProcessWaiter, parse_args, resolve_working_dir, run_updater,
};

fn main() -> ExitCode {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    if raw_args
        .iter()
        .any(|a| a == "-h" || a == "--help" || a.eq_ignore_ascii_case("-help"))
    {
        println!(
            "rustgram-updater — Rustgram staged update applier\n\
             Usage: rustgram-updater [-update] [-finishprocess <PID> <CREATED>] \
             [-workdir <DIR>] [-writeprotected <DIR>] [-exename <NAME>] \
             [-key <NAME>] [-autostart] [-debug] [-startintray] [-justupdate]"
        );
        return ExitCode::SUCCESS;
    }

    let parsed = parse_args(&raw_args);

    let exe_dir = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));

    let current_dir = env::current_dir().unwrap_or_else(|_| exe_dir.clone());
    let app_data = env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let (working_dir, working_dir_source) = resolve_working_dir(
        parsed.workdir.as_deref(),
        &current_dir,
        &exe_dir,
        app_data.as_deref(),
    );

    let mut debug_log = if parsed.debug {
        open_debug_log(&working_dir)
    } else {
        None
    };
    write_debug_log(
        debug_log.as_mut(),
        &format!(
            "rustgram-updater started: workdir='{}' ({working_dir_source:?}), current_dir='{}', install_dir='{}', need_update={}",
            working_dir.display(),
            current_dir.display(),
            parsed
                .write_protected_dir
                .as_deref()
                .unwrap_or(&exe_dir)
                .display(),
            parsed.need_update
        ),
    );

    let waiter = SystemProcessWaiter;
    let launcher = SystemAppLauncher;

    match run_updater(&parsed, &working_dir, &exe_dir, &waiter, &launcher) {
        Ok(outcome) => {
            write_debug_log(
                debug_log.as_mut(),
                &format!(
                    "rustgram-updater finished: updated={}, files_replaced={}, relaunched={}, version={:?}",
                    outcome.updated,
                    outcome.files_replaced,
                    outcome.relaunched,
                    outcome.installed_version
                ),
            );
            if parsed.need_update && !outcome.updated {
                eprintln!(
                    "rustgram-updater: no staged update found in {}",
                    working_dir.display()
                );
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(err) => {
            write_debug_log(
                debug_log.as_mut(),
                &format!("rustgram-updater failed: {err}"),
            );
            eprintln!("rustgram-updater: update failed: {err}");
            ExitCode::from(1)
        }
    }
}

fn open_debug_log(working_dir: &Path) -> Option<File> {
    let dir = working_dir.join("DebugLogs");
    fs::create_dir_all(&dir).ok()?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{secs}_upd.txt"));
    File::create(path).ok()
}

fn write_debug_log(file: Option<&mut File>, line: &str) {
    if let Some(f) = file {
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
}
