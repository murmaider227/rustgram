//! Interactive CLI login, dialog/history viewer, and Saved Messages write test.
//!
//! Usage:
//!   RUSTGRAM_API_ID=12345 RUSTGRAM_API_HASH=... cargo run -p tg-session --example login -- \
//!     --workdir ./target/cli-tdata
//!
//! Write test (Saved Messages):
//!   cargo run -p tg-session --example login -- \
//!     --workdir ./target/cli-tdata --send "Hello from Rustgram!" --to-saved

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use qrcode::QrCode;
use qrcode::render::unicode;
use tg_api::{ApiError, LoadDirection, LoginErrorKind, LoginStep, RpcClient, SendOptions};
use tg_core::domain::Environment;
use tg_core::launcher::{WorkDirEnv, parse_arguments};
use tg_data::{MsgId, PeerId, TimeId};
use tg_session::runtime::format_message_preview;
use tg_session::{
    AccountManager, LoginFlow, SessionError, SessionRuntime, SessionRuntimeConfig,
    WorkingDirOpenOptions, load_api_credentials_from_env,
};

struct CliArgs {
    workdir: Option<PathBuf>,
    key: Option<String>,
    passcode: Option<String>,
    qr: bool,
    phone: bool,
    test_dc: bool,
    send_text: Option<String>,
    to_saved: bool,
    logout: bool,
    quiet: bool,
    inputs_dir: Option<PathBuf>,
    qr_svg: Option<PathBuf>,
    help: bool,
}

fn parse_cli() -> Result<CliArgs, String> {
    let mut args = std::env::args().skip(1);
    let mut out = CliArgs {
        workdir: None,
        key: None,
        passcode: None,
        qr: false,
        phone: false,
        test_dc: false,
        send_text: None,
        to_saved: false,
        logout: false,
        quiet: false,
        inputs_dir: None,
        qr_svg: None,
        help: false,
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workdir" | "-workdir" => {
                let val = args
                    .next()
                    .ok_or_else(|| format!("missing path after {arg}"))?;
                out.workdir = Some(PathBuf::from(val));
            }
            "--key" | "-key" => {
                let val = args
                    .next()
                    .ok_or_else(|| format!("missing value after {arg}"))?;
                out.key = Some(val);
            }
            "--passcode" => {
                let val = args
                    .next()
                    .ok_or_else(|| "missing value after --passcode".to_owned())?;
                out.passcode = Some(val);
            }
            "--qr" => out.qr = true,
            "--phone" => out.phone = true,
            "--test-dc" | "-testmode" | "--testmode" => out.test_dc = true,
            "--send" => {
                let val = args
                    .next()
                    .ok_or_else(|| "missing message text after --send".to_owned())?;
                out.send_text = Some(val);
            }
            "--to-saved" => out.to_saved = true,
            "--logout" => out.logout = true,
            "--quiet" => out.quiet = true,
            "--qr-svg" => {
                let val = args
                    .next()
                    .ok_or_else(|| "missing path after --qr-svg".to_owned())?;
                out.qr_svg = Some(PathBuf::from(val));
            }
            "--inputs-dir" => {
                let val = args
                    .next()
                    .ok_or_else(|| "missing path after --inputs-dir".to_owned())?;
                out.inputs_dir = Some(PathBuf::from(val));
            }
            "--help" | "-h" => out.help = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if out.send_text.is_some() && !out.to_saved {
        return Err("--send requires --to-saved".to_owned());
    }
    Ok(out)
}

fn print_usage() {
    println!(
        "Rustgram tg-session CLI login & session verifier\n\
         \n\
         ENVIRONMENT:\n\
         \tRUSTGRAM_API_ID    Telegram application api_id (integer)\n\
         \tRUSTGRAM_API_HASH  Telegram application api_hash (hex string)\n\
         \n\
         OPTIONS:\n\
         \t--workdir <DIR>  Working directory (parent of tdata)\n\
         \t--key <NAME>     Optional profile key suffix (default: data)\n\
         \t--passcode <PW>  Local tdata passcode (prompted if needed)\n\
         \t--phone          Use phone + code login directly\n\
         \t--qr             Use QR code login directly\n\
         \t--test-dc        Connect to Telegram test DCs\n\
         \t--send <TEXT>    Send TEXT (must be combined with --to-saved)\n\
         \t--to-saved       Target Saved Messages for --send\n\
         \t--logout         Log out the active account and clear tdata\n\
         \t--quiet          Print counts only (no chat names, message texts or user details)\n\
         \t--inputs-dir <D> Read prompted values from files in D (phone, code, password, ...)\n\
         \t--qr-svg <FILE>  Write the current login QR code to FILE (SVG) for display\n\
         \t-h, --help       Show this help"
    );
}

static INPUTS_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static QR_SVG: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Writes the QR code for `url` to the `--qr-svg` file atomically (temp file + rename).
fn write_qr_svg(path: &std::path::Path, url: &str) -> Result<(), String> {
    let code = QrCode::new(url.as_bytes()).map_err(|e| e.to_string())?;
    let svg = code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(320, 320)
        .quiet_zone(true)
        .build();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, svg).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn quiet() -> bool {
    QUIET.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reads the value for `key` from `<inputs-dir>/<key>` when `--inputs-dir` is set (the file is
/// deleted after reading), otherwise prompts on the terminal (hidden when `secret`).
fn ask(key: &str, prompt: &str, secret: bool) -> io::Result<String> {
    if let Some(Some(dir)) = INPUTS_DIR.get() {
        let path = dir.join(key);
        println!("WAITING_FOR_INPUT {key}");
        io::stdout().flush()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(20 * 60);
        loop {
            if let Ok(value) = std::fs::read_to_string(&path) {
                let _ = std::fs::remove_file(&path);
                println!("RECEIVED_INPUT {key}");
                return Ok(value.trim().to_owned());
            }
            if std::time::Instant::now() > deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out waiting for input '{key}'"),
                ));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    if secret {
        return rpassword::prompt_password(prompt);
    }
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

fn prompt_line(prompt: &str) -> io::Result<String> {
    ask("choice", prompt, false)
}

fn now_unix() -> TimeId {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().min(i32::MAX as u64) as TimeId)
}

fn render_qr_terminal(url: &str) -> Result<String, String> {
    let code = QrCode::new(url.as_bytes()).map_err(|e| e.to_string())?;
    let rendered = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    Ok(rendered)
}

fn format_timestamp(ts: TimeId) -> String {
    Timestamp::from_second(i64::from(ts))
        .map(|t| {
            t.to_zoned_datetime(TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| ts.to_string())
}

async fn run_phone_login(flow: &mut LoginFlow) -> Result<tg_tl::types::User, SessionError> {
    if let Ok(nearest) = flow.fetch_nearest_dc().await {
        println!(
            "Nearest DC: DC{} (country={})",
            nearest.nearest_dc, nearest.country
        );
    }

    loop {
        let phone = ask(
            "phone",
            "Phone number (international format, e.g. +1234567890): ",
            false,
        )?;
        match flow.submit_phone(&phone).await {
            Ok(_) => break,
            Err(SessionError::Api(ApiError::Login(kind))) => {
                eprintln!("Phone rejected ({kind:?}). Please try again.");
            }
            Err(err) => return Err(err),
        }
    }

    loop {
        match flow.step().clone() {
            LoginStep::EmailSetup { .. } => {
                let email = ask("email", "Login email address required: ", false)?;
                if let Err(err) = flow.submit_email_setup(&email).await {
                    eprintln!("Email error: {err}");
                }
            }
            LoginStep::EmailVerify {
                email_pattern,
                code_length,
                ..
            } => {
                let code = ask(
                    "email_code",
                    &format!("Verification code sent to {email_pattern} ({code_length} digits): "),
                    false,
                )?;
                if let Err(err) = flow.submit_code(&code).await {
                    eprintln!("Verification failed: {err}");
                }
            }
            LoginStep::CodeInput(state) => {
                let delivery = if state.code_by_telegram {
                    "sent via Telegram app"
                } else if let Some(url) = &state.fragment_url {
                    println!("Retrieve code from Fragment: {url}");
                    "via Fragment"
                } else if let Some(pat) = &state.email_pattern_login {
                    println!("Code sent to email {pat}");
                    "via email"
                } else {
                    "sent via SMS/call"
                };
                let prompt = if state.code_length > 0 {
                    format!("Login code ({delivery}, {} digits): ", state.code_length)
                } else {
                    format!("Login code ({delivery}): ")
                };
                let code = ask("code", &prompt, false)?;
                match flow.submit_code(&code).await {
                    Ok(_) => {}
                    Err(SessionError::Api(ApiError::Login(LoginErrorKind::PhoneCodeInvalid))) => {
                        eprintln!("Invalid code. Please try again.");
                    }
                    Err(err) => return Err(err),
                }
            }
            LoginStep::PasswordCheck { password_state, .. } => {
                complete_2fa_password(flow, &password_state.hint).await?;
            }
            LoginStep::SignUp { .. } => {
                let first = ask("first_name", "First name: ", false)?;
                let last = ask("last_name", "Last name (optional): ", false)?;
                flow.submit_sign_up(&first, &last).await?;
            }
            LoginStep::Authorized { user, .. } => return Ok(*user),
            other => {
                return Err(SessionError::Api(ApiError::InvalidState(format!(
                    "unexpected login step: {other:?}"
                ))));
            }
        }
    }
}

async fn run_qr_login(flow: &mut LoginFlow) -> Result<tg_tl::types::User, SessionError> {
    let mut updates_rx = flow.rpc().subscribe_updates();
    flow.start_qr_login(now_unix(), Vec::new()).await?;

    let mut last_url = String::new();
    loop {
        match flow.step().clone() {
            LoginStep::QrLogin(qr) => {
                if qr.qr_url != last_url {
                    last_url = qr.qr_url.clone();
                    if let Some(Some(svg_path)) = QR_SVG.get() {
                        match write_qr_svg(svg_path, &qr.qr_url) {
                            Ok(()) => println!("QR_READY"),
                            Err(e) => eprintln!("(QR file write failed: {e})"),
                        }
                    } else {
                        println!(
                            "\nScan this QR code in Telegram (Settings -> Devices -> Link Desktop Device):\n"
                        );
                        match render_qr_terminal(&qr.qr_url) {
                            Ok(art) => println!("{art}"),
                            Err(e) => eprintln!("(QR render failed: {e})"),
                        }
                        if !quiet() {
                            println!("URL: {}\n", qr.qr_url);
                        }
                    }
                }

                let wait_secs = qr.refresh_after_secs.clamp(2, 5);
                let timer = tokio::time::sleep(Duration::from_secs(wait_secs));
                tokio::pin!(timer);

                tokio::select! {
                    _ = &mut timer => {
                        flow.refresh_qr_token(now_unix()).await?;
                    }
                    maybe_upd = async {
                        match &mut updates_rx {
                            Some(rx) => rx.recv().await.ok(),
                            None => std::future::pending().await,
                        }
                    } => {
                        if let Some(upd) = maybe_upd {
                            let _ = flow.on_qr_updates(&upd, now_unix()).await?;
                        }
                    }
                }
            }
            LoginStep::PasswordCheck { password_state, .. } => {
                complete_2fa_password(flow, &password_state.hint).await?;
            }
            LoginStep::Authorized { user, .. } => return Ok(*user),
            other => {
                return Err(SessionError::Api(ApiError::InvalidState(format!(
                    "unexpected QR login step: {other:?}"
                ))));
            }
        }
    }
}

async fn complete_2fa_password(flow: &mut LoginFlow, hint: &str) -> Result<(), SessionError> {
    if !hint.is_empty() && !quiet() {
        println!("2FA Password hint: {hint}");
    }
    loop {
        let password = ask("password", "2FA Password (hidden): ", true)?;
        match flow.submit_password(&password).await {
            Ok(_) => return Ok(()),
            Err(SessionError::Api(ApiError::Login(LoginErrorKind::PasswordInvalid))) => {
                eprintln!("Incorrect 2FA password. Please try again.");
            }
            Err(err) => return Err(err),
        }
    }
}

enum CliExit {
    MissingCredentials(String),
    Other(String),
}

impl From<String> for CliExit {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}

fn run() -> Result<(), CliExit> {
    let cli = parse_cli().map_err(CliExit::Other)?;
    if cli.help {
        print_usage();
        return Ok(());
    }
    let _ = INPUTS_DIR.set(cli.inputs_dir.clone());
    let _ = QR_SVG.set(cli.qr_svg.clone());
    QUIET.store(cli.quiet, std::sync::atomic::Ordering::Relaxed);

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let credentials = load_api_credentials_from_env().map_err(|e| {
        CliExit::MissingCredentials(format!(
            "Missing or invalid Telegram API credentials: {e}\n\
             Set RUSTGRAM_API_ID and RUSTGRAM_API_HASH in your environment \
             (obtain them from https://my.telegram.org/apps) before running."
        ))
    })?;

    // Resolve working dir through tg-core's launcher rules.
    let mut core_argv = vec!["rustgram-cli".to_owned()];
    if let Some(wd) = &cli.workdir {
        core_argv.push("-workdir".to_owned());
        core_argv.push(wd.to_string_lossy().into_owned());
    }
    if let Some(key) = &cli.key {
        core_argv.push("-key".to_owned());
        core_argv.push(key.clone());
    }
    if cli.test_dc {
        core_argv.push("-testmode".to_owned());
    }
    let parsed = parse_arguments(&core_argv);
    let (open_opts, resolved) = WorkingDirOpenOptions::from_launch_args(
        &parsed,
        &WorkDirEnv::from_process(),
        credentials.clone(),
    );
    println!("Working directory: {}", resolved.path.display());

    let mut manager = AccountManager::open(open_opts).map_err(|e| e.to_string())?;
    let passcode = if let Some(p) = cli.passcode {
        p.into_bytes()
    } else if manager.needs_passcode().map_err(|e| e.to_string())? {
        ask("passcode", "Local tdata passcode: ", true)
            .map_err(|e| e.to_string())?
            .into_bytes()
    } else {
        Vec::new()
    };
    manager.unlock(&passcode).map_err(|e| e.to_string())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("failed to create Tokio runtime: {e}"))?;

    let mut active_index = manager
        .active_index()
        .ok_or_else(|| "no active account slot".to_owned())?;

    if cli.logout {
        let res = rt
            .block_on(manager.logout_account(active_index))
            .map_err(|e| e.to_string())?;
        println!(
            "Logged out slot {active_index} (remaining unconfirmed keys: {}).",
            res.remaining.len()
        );
        return Ok(());
    }

    if cli.test_dc
        && manager
            .account(active_index)
            .is_some_and(|a| !a.is_authorized() && a.environment != Environment::Test)
    {
        active_index = manager
            .add_account(Environment::Test)
            .map_err(|e| e.to_string())?;
    }

    let already_authed = manager
        .account(active_index)
        .is_some_and(|a| a.is_authorized());

    let (user_id, logged_in_user) = if already_authed {
        let (uid, main_dc, key_count, pending_destroy) = {
            let acc = manager
                .account(active_index)
                .ok_or_else(|| "missing account".to_owned())?;
            let uid = acc.user_id.ok_or_else(|| "missing user_id".to_owned())?;
            (
                uid,
                acc.main_dc_id(),
                acc.client.export_persistent_keys().len(),
                acc.keys_to_destroy.len(),
            )
        };
        if quiet() {
            println!("Resuming saved session: main_dc=DC{main_dc}, saved_keys={key_count}");
        } else {
            println!(
                "Resuming saved session: user_id={}, main_dc=DC{}, saved_keys={}",
                uid.0, main_dc, key_count
            );
        }
        if pending_destroy > 0 {
            let _ = rt.block_on(manager.destroy_pending_keys(active_index));
        }
        (uid, None)
    } else {
        let use_qr = if cli.qr {
            true
        } else if cli.phone {
            false
        } else {
            let choice =
                prompt_line("Choose login method: [1] Phone number  [2] QR code (default 1): ")
                    .map_err(|e| e.to_string())?;
            choice == "2" || choice.eq_ignore_ascii_case("qr")
        };

        let rpc = manager
            .account(active_index)
            .ok_or_else(|| "missing active account".to_owned())?
            .rpc
            .clone();
        let user = rt
            .block_on(async move {
                let mut flow = LoginFlow::new(rpc, credentials);
                if use_qr {
                    run_qr_login(&mut flow).await
                } else {
                    run_phone_login(&mut flow).await
                }
            })
            .map_err(|e| format!("login failed: {e}"))?;

        let auth = manager
            .complete_login(active_index, &user)
            .map_err(|e| format!("failed to save MTP authorization: {e}"))?;
        let display_name = format!(
            "{} {}",
            user.first_name.as_deref().unwrap_or(""),
            user.last_name.as_deref().unwrap_or("")
        )
        .trim()
        .to_owned();
        if quiet() {
            println!(
                "Logged in (main_dc=DC{}, saved_keys={}).",
                auth.main_dc_id,
                auth.keys.len()
            );
        } else {
            println!(
                "Logged in as '{}' (id={}, main_dc=DC{}, saved_keys={}).",
                display_name,
                auth.user_id,
                auth.main_dc_id,
                auth.keys.len()
            );
        }
        (tg_data::UserId::from_tl(user.id), Some(user))
    };

    // Start SessionRuntime on the same Tokio runtime driving the MTProto sessions.
    let rpc = manager
        .account(active_index)
        .ok_or_else(|| "missing account".to_owned())?
        .rpc
        .clone();
    let runtime = SessionRuntime::start_mtproto_on(
        user_id,
        rpc,
        rt.handle().clone(),
        SessionRuntimeConfig::default(),
    )
    .map_err(|e| format!("failed to start SessionRuntime: {e}"))?;

    if let Some(user) = &logged_in_user {
        runtime.seed_self_user(user);
    }

    println!("Syncing updates (getState + getDifference) and loading first 20 dialogs...");
    runtime
        .wait_ready_blocking()
        .map_err(|e| format!("session sync failed: {e}"))?;

    // Print first 20 dialogs with unread counts.
    let dialogs = runtime.dialog_summaries(20);
    if quiet() {
        let unread = dialogs
            .iter()
            .filter(|d| d.unread_count > 0 || d.unread_mark)
            .count();
        println!("dialogs_loaded={} unread_chats={unread}", dialogs.len());
    } else {
        println!("\n=== Top {} Dialogs ===", dialogs.len());
    }
    for (idx, dlg) in dialogs.iter().enumerate().filter(|_| !quiet()) {
        let unread_tag = if dlg.unread_count > 0 {
            format!(" [unread: {}]", dlg.unread_count)
        } else if dlg.unread_mark {
            " [unread]".to_owned()
        } else {
            String::new()
        };
        let muted_tag = if dlg.muted { " (muted)" } else { "" };
        println!(
            "{:>2}. {}{}{}  --  {}",
            idx + 1,
            dlg.title,
            unread_tag,
            muted_tag,
            dlg.top_message_preview
        );
    }

    // Load and print the last 5 messages of the first chat dialog.
    if let Some(first_peer) = dialogs.iter().find_map(|d| d.peer_id) {
        let first_title = dialogs
            .iter()
            .find(|d| d.peer_id == Some(first_peer))
            .map(|d| d.title.as_str())
            .unwrap_or("First Chat");
        let _ = runtime
            .open_history_blocking(first_peer, MsgId::ZERO, LoadDirection::Before)
            .map_err(|e| format!("failed to load history for '{first_title}': {e}"))?;
        let messages = runtime.last_messages(first_peer, 5);
        if quiet() {
            println!("first_chat_messages_loaded={}", messages.len());
        } else {
            println!(
                "\n=== Last {} Messages in '{}' ===",
                messages.len(),
                first_title
            );
        }
        let session_guard = runtime.session().blocking_read();
        for msg in messages.iter().filter(|_| !quiet()) {
            let sender_name = msg
                .from_id
                .and_then(|pid| session_guard.peer(pid))
                .map(|p| p.name().to_owned())
                .unwrap_or_else(|| {
                    if msg.is_outgoing() {
                        "Me".to_owned()
                    } else {
                        "Unknown".to_owned()
                    }
                });
            println!(
                "  [#{}] [{}] {}: {}",
                msg.id.0,
                format_timestamp(msg.date),
                sender_name,
                format_message_preview(msg)
            );
        }
    }

    // Optional write test: --send "<text>" --to-saved
    if let Some(text) = cli.send_text {
        let self_peer = PeerId::from_user(user_id);
        println!("\nSending message to Saved Messages...");
        let outcome = runtime
            .send_message_blocking(self_peer, text, SendOptions::default())
            .map_err(|e| format!("failed to send message to Saved Messages: {e}"))?;
        for sent in &outcome.messages {
            println!(
                "Sent to Saved Messages: random_id={}, state={:?}",
                sent.random_id, sent.state
            );
        }
    }

    // Persist any newly generated DC keys / config updates before exit.
    let saved = manager
        .save_account(active_index)
        .map_err(|e| format!("failed to save session on exit: {e}"))?;
    if quiet() {
        println!(
            "Session saved (main_dc=DC{}, keys={}).",
            saved.main_dc_id,
            saved.keys.len()
        );
    } else {
        println!(
            "\nSession saved (user_id={}, main_dc=DC{}, keys={}). Run again to resume without code.",
            saved.user_id,
            saved.main_dc_id,
            saved.keys.len()
        );
    }

    runtime.shutdown();
    manager.shutdown();
    drop(rt);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliExit::MissingCredentials(msg)) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
        Err(CliExit::Other(err)) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}
