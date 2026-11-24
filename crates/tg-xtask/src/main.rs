//! CLI entrypoint for `cargo run -p tg-xtask -- <command>`.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;

use tg_updater::version::{UpdateChannel, Version};
use tg_xtask::XtaskError;
use tg_xtask::dist::{
    DistOptions, find_workspace_root, read_workspace_version, resolve_target_dir, run_dist,
};
use tg_xtask::icon::render_svg_to_ico;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("tg-xtask error: {err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), XtaskError> {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        print_usage();
        return Ok(());
    };

    match cmd {
        "dist" => run_dist_cli(&args[1..]),
        "icon" => run_icon_cli(&args[1..]),
        "keygen" => run_keygen_cli(),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => Err(XtaskError::InvalidArgument(format!(
            "unknown subcommand '{other}' (expected dist, icon, or keygen)"
        ))),
    }
}

fn print_usage() {
    println!(
        "\
tg-xtask — Rustgram build, icon, and Windows packaging helper

USAGE:
  cargo run -p tg-xtask -- <SUBCOMMAND> [OPTIONS]

SUBCOMMANDS:
  dist     Build release binaries, generate rustgram.ico, stage portable Rustgram/ folder
           with FFmpeg DLLs and licenses, create portable zip, optionally pack signed
           .tupdate, and run ISCC.exe if installed.
    --version <SEMVER>       Override release version (default: workspace version)
    --channel <stable|beta>  Update channel (default: auto from version)
    --out-dir <DIR>          Output directory (default: <CARGO_TARGET_DIR>/dist)
    --release-dir <DIR>      Directory containing built executables
    --ffmpeg-bin <DIR>       Directory containing FFmpeg runtime DLLs
    --skip-build             Reuse already-built binaries
    --signing-key <FILE>     Path to 32-byte raw or 64-char hex Ed25519 signing seed
    --iscc <PATH>            Explicit path to Inno Setup ISCC.exe
    --require-iscc           Fail if ISCC.exe is not installed

  icon     Render assets/icon.svg into a multi-size Windows .ico file.
    --svg <PATH>             Input SVG path (default: <workspace>/assets/icon.svg)
    --out <PATH>             Output .ico path (default: <CARGO_TARGET_DIR>/dist/rustgram.ico)

  keygen   Generate a new Ed25519 release signing keypair (prints public key for
           RUSTGRAM_UPDATE_PUBKEY build env var and private seed for GitHub Actions secret)."
    );
}

fn run_dist_cli(args: &[String]) -> Result<(), XtaskError> {
    let workspace_root = find_workspace_root()?;
    let mut version_arg: Option<String> = None;
    let mut channel_arg: Option<String> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut release_dir: Option<PathBuf> = None;
    let mut ffmpeg_bin_dir: Option<PathBuf> = None;
    let mut skip_build = false;
    let mut signing_key_path: Option<PathBuf> = None;
    let mut iscc_path: Option<PathBuf> = None;
    let mut require_iscc = false;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                i += 1;
                version_arg = Some(next_value(args, i, "--version")?);
            }
            "--channel" => {
                i += 1;
                channel_arg = Some(next_value(args, i, "--channel")?);
            }
            "--out-dir" => {
                i += 1;
                out_dir = Some(PathBuf::from(next_value(args, i, "--out-dir")?));
            }
            "--release-dir" => {
                i += 1;
                release_dir = Some(PathBuf::from(next_value(args, i, "--release-dir")?));
            }
            "--ffmpeg-bin" => {
                i += 1;
                ffmpeg_bin_dir = Some(PathBuf::from(next_value(args, i, "--ffmpeg-bin")?));
            }
            "--skip-build" => skip_build = true,
            "--signing-key" => {
                i += 1;
                signing_key_path = Some(PathBuf::from(next_value(args, i, "--signing-key")?));
            }
            "--iscc" => {
                i += 1;
                iscc_path = Some(PathBuf::from(next_value(args, i, "--iscc")?));
            }
            "--require-iscc" => require_iscc = true,
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            other => {
                return Err(XtaskError::InvalidArgument(format!(
                    "unknown dist option '{other}'"
                )));
            }
        }
        i += 1;
    }

    let version = match version_arg {
        Some(v) => Version::parse(&v)?,
        None => read_workspace_version(&workspace_root)?,
    };

    let channel = match channel_arg.as_deref() {
        Some("stable") => UpdateChannel::Stable,
        Some("beta") => UpdateChannel::Beta,
        Some(other) => {
            return Err(XtaskError::InvalidArgument(format!(
                "invalid channel '{other}' (expected 'stable' or 'beta')"
            )));
        }
        None => {
            if version.is_prerelease() {
                UpdateChannel::Beta
            } else {
                UpdateChannel::Stable
            }
        }
    };

    let out_dir = out_dir.unwrap_or_else(|| resolve_target_dir(&workspace_root).join("dist"));

    let output = run_dist(&DistOptions {
        workspace_root,
        version,
        channel,
        out_dir,
        release_dir,
        ffmpeg_bin_dir,
        skip_build,
        signing_key_path,
        iscc_path,
        require_iscc,
    })?;

    println!("tg-xtask: dist completed:");
    println!("  icon:         {}", output.icon_path.display());
    println!("  portable dir: {}", output.portable_dir.display());
    println!("  portable zip: {}", output.portable_zip.display());
    if let Some(ref inst) = output.installer_exe {
        println!("  installer:    {}", inst.display());
    }
    if let Some(ref upd) = output.update_package {
        println!("  update pkg:   {}", upd.display());
    }
    println!("  checksums:    {}", output.checksums_path.display());

    Ok(())
}

fn run_icon_cli(args: &[String]) -> Result<(), XtaskError> {
    let workspace_root = find_workspace_root()?;
    let mut svg_path = workspace_root.join("assets").join("icon.svg");
    let mut out_path = resolve_target_dir(&workspace_root)
        .join("dist")
        .join("rustgram.ico");

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--svg" => {
                i += 1;
                svg_path = PathBuf::from(next_value(args, i, "--svg")?);
            }
            "--out" => {
                i += 1;
                out_path = PathBuf::from(next_value(args, i, "--out")?);
            }
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            other => {
                return Err(XtaskError::InvalidArgument(format!(
                    "unknown icon option '{other}'"
                )));
            }
        }
        i += 1;
    }

    render_svg_to_ico(&svg_path, &out_path)?;
    println!("tg-xtask: generated {}", out_path.display());
    Ok(())
}

fn run_keygen_cli() -> Result<(), XtaskError> {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let pub_bytes = verifying_key.to_bytes();
    let seed_hex = hex::encode(signing_key.to_bytes());
    let pub_hex = hex::encode(pub_bytes);

    println!("=== Rustgram Ed25519 Release Signing Keypair ===");
    println!();
    println!("1) Public key (pass at release build time via env var RUSTGRAM_UPDATE_PUBKEY):");
    println!("   Hex: {pub_hex}");
    println!("   PowerShell: $env:RUSTGRAM_UPDATE_PUBKEY='{pub_hex}'");
    println!("   Bash:       export RUSTGRAM_UPDATE_PUBKEY={pub_hex}");
    print!("   Rust bytes: [");
    for (idx, b) in pub_bytes.iter().enumerate() {
        if idx > 0 {
            print!(", ");
        }
        print!("0x{b:02X}");
    }
    println!("]");
    println!();
    println!(
        "2) Private key seed (NEVER commit — store in GitHub secret RUSTGRAM_UPDATER_SIGNING_KEY):"
    );
    println!("   {seed_hex}");
    Ok(())
}

fn next_value(args: &[String], idx: usize, flag: &str) -> Result<String, XtaskError> {
    args.get(idx)
        .cloned()
        .ok_or_else(|| XtaskError::InvalidArgument(format!("missing value for {flag}")))
}
