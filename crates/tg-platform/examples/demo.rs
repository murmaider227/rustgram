//! Interactive demo for `tg-platform`: prints Windows system info, creates a tray icon
//! with an unread counter & context menu, and sends a native WinRT toast notification.
//!
//! Run with:
//! `cargo run -p tg-platform --example demo`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use tg_platform::app_id::{
    DEFAULT_APP_USER_MODEL_ID, ShortcutConfig, current_executable_path, set_current_process_app_id,
    start_menu_programs_dir, validate_or_create_shortcut_in,
};
use tg_platform::notifications::{
    ContextId, NotificationId, ToastContent, ToastManager, query_notification_suppression,
};
use tg_platform::spellcheck::SpellChecker;
use tg_platform::system_info::{
    default_mail_app, device_model_pretty, idle_time, is_dark_mode, is_dark_taskbar, monitors_dpi,
    power_status, system_accent_color, system_country, system_language, system_version_pretty,
};
use tg_platform::tray::{TrayConfig, TrayEvent, TrayIcon, TrayMenuItem, default_app_icon};

const CMD_SHOW_TOAST: u32 = 1;
const CMD_INC_UNREAD: u32 = 2;
const CMD_TOGGLE_MUTED: u32 = 3;
const CMD_CLEAR_UNREAD: u32 = 4;
const CMD_QUIT: u32 = 5;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Rustgram tg-platform demo ===");
    println!("OS version:      {}", system_version_pretty());
    println!("Device model:    {}", device_model_pretty());
    println!(
        "Language/Region: {} / {}",
        system_language(),
        system_country()
    );
    println!(
        "Dark mode:       {:?} (taskbar: {:?})",
        is_dark_mode(),
        is_dark_taskbar()
    );
    println!("Accent color:    {:?}", system_accent_color());
    println!("Idle time:       {:?}", idle_time());
    println!("Power status:    {:?}", power_status());
    println!("Default mail:    {:?}", default_mail_app());
    for mon in monitors_dpi() {
        println!(
            "Monitor {}: {}x{} @ {} DPI ({:.0}% scale, primary={})",
            mon.device_name,
            mon.right - mon.left,
            mon.bottom - mon.top,
            mon.dpi_x,
            mon.scale_factor * 100.0,
            mon.is_primary
        );
    }

    if let Ok(spell) = SpellChecker::new() {
        println!(
            "Spellchecker active languages: {:?}",
            spell.active_languages()
        );
        println!(
            "Spellcheck 'hello'={}, 'helooo'={} -> suggestions={:?}",
            spell.check_word("hello"),
            spell.check_word("helooo"),
            spell.suggestions("helooo", 5)
        );
    }

    // Set AppUserModelID and ensure a Start-menu shortcut exists so Windows displays toasts.
    let aumid = DEFAULT_APP_USER_MODEL_ID;
    if let Err(e) = set_current_process_app_id(aumid) {
        eprintln!("Warning: SetCurrentProcessExplicitAppUserModelID failed: {e}");
    }
    if let (Ok(exe), Ok(programs)) = (current_executable_path(), start_menu_programs_dir()) {
        let work_dir = exe
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let cfg = ShortcutConfig::for_executable(exe, work_dir);
        match validate_or_create_shortcut_in(&programs, &cfg) {
            Ok(p) => println!("Start-menu shortcut ready at: {}", p.display()),
            Err(e) => eprintln!("Warning: could not prepare Start-menu shortcut: {e}"),
        }
    }

    println!(
        "Notification suppression state: {:?}",
        query_notification_suppression(aumid)
    );

    // Save a temporary PNG icon to use as the toast avatar image.
    let avatar_path = std::env::temp_dir().join("rustgram_demo_avatar.png");
    let avatar_png = default_app_icon(64, false, true).encode_png()?;
    let _ = std::fs::write(&avatar_path, avatar_png);

    let toast_manager = Arc::new(ToastManager::new(aumid, |ev| {
        println!("[Toast activated] {ev:?}");
    })?);

    let next_msg_id = Arc::new(AtomicU32::new(1));
    let show_sample_toast = {
        let mgr = Arc::clone(&toast_manager);
        let msg_counter = Arc::clone(&next_msg_id);
        let avatar = avatar_path.clone();
        move || {
            let msg_id = msg_counter.fetch_add(1, Ordering::SeqCst) as i32;
            let mut content = ToastContent::simple(
                NotificationId {
                    context: ContextId {
                        session_id: 1,
                        peer_id: 42,
                        topic_root_id: 0,
                        monoforum_peer_id: 0,
                    },
                    msg_id,
                },
                "Rustgram Demo",
                format!("Native Windows toast #{msg_id} — try replying or clicking Mark as read!"),
            );
            content.subtitle = Some("Demo Chat".to_string());
            content.image_path = Some(avatar.clone());
            match mgr.show(&content) {
                Ok(()) => println!("Shown toast #{msg_id}"),
                Err(e) => eprintln!("Failed to show toast: {e}"),
            }
        }
    };

    let running = Arc::new(AtomicBool::new(true));
    let unread = Arc::new(AtomicU32::new(3));
    let muted = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<TrayEvent>();

    let tray = TrayIcon::new(
        TrayConfig {
            app_name: "Rustgram Demo".to_string(),
            unread_count: 3,
            muted: false,
            menu: vec![
                TrayMenuItem::action(CMD_SHOW_TOAST, "Show Toast Notification"),
                TrayMenuItem::action(CMD_INC_UNREAD, "Increment Unread Count"),
                TrayMenuItem::action(CMD_TOGGLE_MUTED, "Toggle Muted Badge Color"),
                TrayMenuItem::action(CMD_CLEAR_UNREAD, "Clear Unread Count"),
                TrayMenuItem::Separator,
                TrayMenuItem::action(CMD_QUIT, "Quit"),
            ],
            ..Default::default()
        },
        move |ev| {
            let _ = tx.send(ev);
        },
    )?;

    println!("\nTray icon is live! Showing an initial toast notification...");
    println!("Right-click the tray icon for options, or press Ctrl+C / choose Quit to exit.");
    show_sample_toast();

    while running.load(Ordering::SeqCst) {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
            println!("[Tray event] {ev:?}");
            match ev {
                TrayEvent::LeftClick | TrayEvent::DoubleClick => {
                    show_sample_toast();
                }
                TrayEvent::MenuItem(CMD_SHOW_TOAST) => show_sample_toast(),
                TrayEvent::MenuItem(CMD_INC_UNREAD) => {
                    let count = unread.fetch_add(1, Ordering::SeqCst) + 1;
                    let is_muted = muted.load(Ordering::SeqCst);
                    let _ = tray.set_unread(count, is_muted);
                    println!("Unread badge updated -> {count} (muted={is_muted})");
                }
                TrayEvent::MenuItem(CMD_TOGGLE_MUTED) => {
                    let is_muted = !muted.fetch_xor(true, Ordering::SeqCst);
                    let count = unread.load(Ordering::SeqCst);
                    let _ = tray.set_unread(count, is_muted);
                    println!("Muted state toggled -> {is_muted} (count={count})");
                }
                TrayEvent::MenuItem(CMD_CLEAR_UNREAD) => {
                    unread.store(0, Ordering::SeqCst);
                    let _ = tray.set_unread(0, false);
                    toast_manager.clear_all();
                    println!("Cleared unread badge and active toasts");
                }
                TrayEvent::MenuItem(CMD_QUIT) => {
                    running.store(false, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }

    toast_manager.clear_all();
    let _ = std::fs::remove_file(&avatar_path);
    println!("Exiting demo cleanly.");
    Ok(())
}
