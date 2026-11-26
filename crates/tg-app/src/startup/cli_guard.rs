//! Command-line exits that run before any profile, lock or window is touched (TGB-3005).
//!
//! Two traps opened the user's real profile from automation:
//!
//! 1. Info flags. tdesktop has no `--version`; its launcher treats every unknown argument as a
//!    trailing URL or file path, so `rustgram --version` started the full app. Here `--version` /
//!    `-v` / `-V` print the version, `--help` / `-h` print the usage, and any other unknown
//!    option (`-x`, `--foo`) prints an error; all three exit before the working dir is used.
//! 2. Agent runs. A process started by a coding agent or automation (detected from explicit
//!    environment variables, see [`detect_agent`]) must name its own working dir with `-workdir`.
//!    Without it (or with a `-workdir` inside the default profile) the launch is refused with exit
//!    code 2, whatever `--demo` / `-testmode` say: both of them still write logs, crash state and
//!    shell settings into the default working dir and share its single-instance lock.
//!
//! The user's own launches (Explorer, Start menu, `tg://` links, autostart) carry none of the
//! agent variables and behave as before.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

use tg_core::launcher::{ParsedArgs, ResolvedWorkingDir, WorkDirEnv, resolve_working_dir};
use tg_core::version::APP_NAME;

use super::plan::help_text;

/// Explicit marker for agent and automation shells (`RUSTGRAM_AGENT=1`). Briefs and test
/// harnesses set it; any value other than empty, `0`, `false`, `no` or `off` counts.
pub const ENV_AGENT: &str = "RUSTGRAM_AGENT";

/// Variables that coding-agent hosts set in every shell they spawn. A non-empty value marks
/// the process as an agent run.
const AGENT_HOST_VARS: &[&str] = &[
    // Claude Code (every Bash / PowerShell tool shell).
    "CLAUDECODE",
    // T3 Code agent threads (Claude and Codex children alike).
    "T3CODE_THREAD_ID",
    // Cross-vendor convention for agent shells (`AI_AGENT=<tool>_<version>_agent`).
    "AI_AGENT",
];

/// Prefix of the variables the Codex CLI sets in the commands it runs (`CODEX_SANDBOX`,
/// `CODEX_SANDBOX_NETWORK_DISABLED`, `CODEX_THREAD_ID`, ...).
const CODEX_PREFIX: &str = "CODEX_";

/// `CODEX_*` variables a user may set globally (configuration, not a running agent).
const CODEX_USER_CONFIG_VARS: &[&str] = &["CODEX_HOME"];

/// tdesktop switches without a value (`core/launcher.cpp` `parseMap`) plus Rustgram's own.
const KNOWN_FLAGS: &[&str] = &[
    "-debug",
    "-testagent",
    "-testmode",
    "-autostart",
    "-fixprevious",
    "-cleanup",
    "-noupdate",
    "-tosettings",
    "-startintray",
    "-many",
    "-externalinterface",
    "-quit",
    "--start-main",
    "--live",
    "--demo",
];

/// Switches that consume the next argument as their value.
const KNOWN_VALUE_OPTIONS: &[&str] = &["-key", "-workdir", "-scale", "-sendpath", "--settings"];

const VERSION_FLAGS: &[&str] = &["--version", "-version", "-v", "-V"];
const HELP_FLAGS: &[&str] = &["--help", "-help", "-h", "-?", "/?"];

/// What the command line asks for before any startup work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliRequest {
    /// Start the app (or run `-cleanup` / `-quit`) as usual.
    Run,
    /// Print the version and exit.
    Version,
    /// Print the usage and exit.
    Help,
    /// An option Rustgram does not know: print an error and exit with code 2.
    UnknownOption(String),
}

/// Classifies `argv` (including `argv[0]`). Version and help flags win over unknown options;
/// values of one-value options and everything after `--` are never read as options.
#[must_use]
pub fn classify_cli<S: AsRef<str>>(argv: &[S]) -> CliRequest {
    let mut info = None;
    let mut unknown = None;
    let mut args = argv.iter().skip(1).map(AsRef::as_ref);
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if KNOWN_VALUE_OPTIONS.contains(&arg) {
            args.next();
            continue;
        }
        if info.is_none() {
            if VERSION_FLAGS.contains(&arg) {
                info = Some(CliRequest::Version);
                continue;
            }
            if HELP_FLAGS.contains(&arg) {
                info = Some(CliRequest::Help);
                continue;
            }
        }
        if unknown.is_none() && is_unknown_option(arg) {
            unknown = Some(CliRequest::UnknownOption(arg.to_owned()));
        }
    }
    info.or(unknown).unwrap_or(CliRequest::Run)
}

fn is_unknown_option(arg: &str) -> bool {
    arg.len() > 1
        && arg.starts_with('-')
        && !KNOWN_FLAGS.contains(&arg)
        && !VERSION_FLAGS.contains(&arg)
        && !HELP_FLAGS.contains(&arg)
}

/// Version string printed by `--version` (the release version the binary was built with).
#[must_use]
pub fn version_text() -> String {
    format!("Rustgram Desktop {}", env!("RUSTGRAM_BUILD_VERSION"))
}

/// The environment variable that marked this process as an agent or automation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSignal {
    /// Name of the variable, as found in the environment.
    pub variable: String,
}

/// Detects an agent or automation run from environment variables (`std::env::vars_os()`).
///
/// Only explicit markers count: [`ENV_AGENT`], the variables agent hosts set in their shells
/// (`CLAUDECODE`, `T3CODE_THREAD_ID`, `AI_AGENT`) and Codex's `CODEX_*` (except `CODEX_HOME`).
/// Names compare case-insensitively, as Windows environment names do.
#[must_use]
pub fn detect_agent<I, K, V>(vars: I) -> Option<AgentSignal>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    vars.into_iter().find_map(|(key, value)| {
        let key = key.as_ref().to_string_lossy();
        let value = value.as_ref().to_string_lossy();
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        let upper = key.to_ascii_uppercase();
        let marks_agent = if upper == ENV_AGENT {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        } else if AGENT_HOST_VARS.contains(&upper.as_str()) {
            true
        } else {
            upper.starts_with(CODEX_PREFIX) && !CODEX_USER_CONFIG_VARS.contains(&upper.as_str())
        };
        marks_agent.then(|| AgentSignal {
            variable: key.into_owned(),
        })
    })
}

/// Returns the refusal message when an agent run would open the default profile: no
/// `-workdir`, or a `-workdir` inside the default working dir or `%APPDATA%\Rustgram`.
#[must_use]
pub fn agent_profile_refusal(
    agent: Option<&AgentSignal>,
    resolved: &ResolvedWorkingDir,
    env: &WorkDirEnv,
) -> Option<String> {
    let agent = agent?;
    let inside_default = default_profile_dirs(env)
        .iter()
        .any(|dir| path_is_within(&resolved.path, dir));
    if resolved.is_custom && !inside_default {
        return None;
    }
    let reason = if resolved.is_custom {
        "-workdir points into the default profile"
    } else {
        "no -workdir was given"
    };
    Some(format!(
        "Rustgram: refusing to start. This is an agent or automation run ({} is set) and {reason}, \
         so it would open the user's own profile.\n\
         Pass -workdir <scratch dir> (add --demo for the offline demo, or -testmode with a copied \
         test-server workdir).\n\
         To read the version without starting the app: rustgram --version",
        agent.variable
    ))
}

fn default_profile_dirs(env: &WorkDirEnv) -> Vec<PathBuf> {
    let mut dirs = vec![resolve_working_dir(&ParsedArgs::default(), env).path];
    if let Some(app_data) = &env.app_data_dir {
        dirs.push(app_data.join(APP_NAME));
    }
    dirs
}

/// Component-wise, case-insensitive `path.starts_with(dir)` (Windows paths ignore case).
fn path_is_within(path: &Path, dir: &Path) -> bool {
    let fold = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
    fold(path).starts_with(fold(dir))
}

/// A launch that ends before startup: text to print and the process exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarlyExit {
    /// Process exit code (0 for version / help, 2 for errors).
    pub code: i32,
    /// Text to print.
    pub message: String,
    /// Print to stderr instead of stdout.
    pub to_stderr: bool,
}

impl EarlyExit {
    /// Prints the message, attaching to the parent console first (release builds have none).
    pub fn emit(&self) {
        super::attach_parent_console();
        if self.to_stderr {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{}", self.message);
            let _ = err.flush();
        } else {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", self.message);
            let _ = out.flush();
        }
    }
}

/// Decides whether this launch ends before any profile, lock or window work: info flags,
/// unknown options, and agent runs without their own `-workdir`. `resolved` is the working dir
/// the launch would use; `agent` comes from [`detect_agent`].
#[must_use]
pub fn early_exit<S: AsRef<str>>(
    argv: &[S],
    resolved: &ResolvedWorkingDir,
    env: &WorkDirEnv,
    agent: Option<&AgentSignal>,
) -> Option<EarlyExit> {
    match classify_cli(argv) {
        CliRequest::Version => Some(EarlyExit {
            code: 0,
            message: version_text(),
            to_stderr: false,
        }),
        CliRequest::Help => Some(EarlyExit {
            code: 0,
            message: format!("{}\n\n{}", version_text(), help_text()),
            to_stderr: false,
        }),
        CliRequest::UnknownOption(option) => Some(EarlyExit {
            code: 2,
            message: format!(
                "Rustgram: unknown option '{option}'. Nothing was started.\n\
                 Run 'rustgram --help' for the list of options."
            ),
            to_stderr: true,
        }),
        CliRequest::Run => agent_profile_refusal(agent, resolved, env).map(|message| EarlyExit {
            code: 2,
            message,
            to_stderr: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_core::launcher::WorkingDirOrigin;

    fn env(base: &Path) -> WorkDirEnv {
        WorkDirEnv {
            exe_dir: base.join("bin"),
            app_data_dir: Some(base.join("AppData").join("Roaming")),
            current_dir: base.join("cwd"),
        }
    }

    fn resolved_for(argv: &[&str], env: &WorkDirEnv) -> ResolvedWorkingDir {
        resolve_working_dir(&tg_core::launcher::parse_arguments(argv), env)
    }

    fn agent(var: &str) -> AgentSignal {
        AgentSignal {
            variable: var.to_owned(),
        }
    }

    #[test]
    fn info_flags_and_unknown_options_are_classified() {
        let cases: &[(&[&str], CliRequest)] = &[
            (&["rustgram.exe"], CliRequest::Run),
            (&["rustgram.exe", "--version"], CliRequest::Version),
            (&["rustgram.exe", "-v"], CliRequest::Version),
            (&["rustgram.exe", "-V"], CliRequest::Version),
            (&["rustgram.exe", "--help"], CliRequest::Help),
            (&["rustgram.exe", "-h"], CliRequest::Help),
            (&["rustgram.exe", "/?"], CliRequest::Help),
            (
                &["rustgram.exe", "--verison"],
                CliRequest::UnknownOption("--verison".to_owned()),
            ),
            (&["rustgram.exe", "-x", "--version"], CliRequest::Version),
            (
                &[
                    "rustgram.exe",
                    "-workdir",
                    "D:/scratch",
                    "--demo",
                    "-testmode",
                    "-many",
                ],
                CliRequest::Run,
            ),
            // One-value option values and everything after `--` are not options.
            (&["rustgram.exe", "-key", "-v"], CliRequest::Run),
            (&["rustgram.exe", "--settings", "--help"], CliRequest::Run),
            (&["rustgram.exe", "--", "-v", "--foo"], CliRequest::Run),
            (&["rustgram.exe", "tg://resolve?domain=x"], CliRequest::Run),
            (
                &["rustgram.exe", "C:/files/report.pdf", "-"],
                CliRequest::Run,
            ),
            (
                &[
                    "rustgram.exe",
                    "-noupdate",
                    "-autostart",
                    "-debug",
                    "-startintray",
                    "-tosettings",
                    "-quit",
                    "-cleanup",
                    "-fixprevious",
                    "-externalinterface",
                    "-testagent",
                    "-scale",
                    "150",
                    "-sendpath",
                    "a.txt",
                    "--live",
                    "--start-main",
                ],
                CliRequest::Run,
            ),
        ];
        for (argv, expected) in cases {
            assert_eq!(&classify_cli(argv), expected, "argv: {argv:?}");
        }
    }

    #[test]
    fn version_and_help_exit_before_the_guard_even_for_agents() {
        let dir = tempfile::TempDir::new().unwrap();
        let env = env(dir.path());
        let argv = ["rustgram.exe", "--version"];
        let resolved = resolved_for(&argv, &env);
        assert!(!resolved.is_custom);
        let exit = early_exit(&argv, &resolved, &env, Some(&agent("CLAUDECODE"))).unwrap();
        assert_eq!(exit.code, 0);
        assert!(!exit.to_stderr);
        assert_eq!(exit.message, version_text());
        assert!(exit.message.contains(env!("RUSTGRAM_BUILD_VERSION")));

        let help = early_exit(&["rustgram.exe", "-h"], &resolved, &env, None).unwrap();
        assert_eq!(help.code, 0);
        assert!(help.message.contains("-workdir <DIR>"));

        let unknown = early_exit(&["rustgram.exe", "--foo"], &resolved, &env, None).unwrap();
        assert_eq!(unknown.code, 2);
        assert!(unknown.to_stderr);
        assert!(unknown.message.contains("--foo"));
    }

    #[test]
    fn agent_without_own_workdir_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let env = env(dir.path());
        let default_dir = env.app_data_dir.clone().unwrap().join(APP_NAME);
        let refused: &[&[&str]] = &[
            &["rustgram.exe"],
            &["rustgram.exe", "--demo"],
            &["rustgram.exe", "-testmode"],
            &["rustgram.exe", "--live", "-testmode", "-many"],
            &["rustgram.exe", "-cleanup"],
            &["rustgram.exe", "-quit"],
            &["rustgram.exe", "tg://resolve?domain=x"],
        ];
        for argv in refused {
            let resolved = resolved_for(argv, &env);
            let exit = early_exit(argv, &resolved, &env, Some(&agent("RUSTGRAM_AGENT")))
                .unwrap_or_else(|| panic!("agent run {argv:?} was not refused"));
            assert_eq!(exit.code, 2, "{argv:?}");
            assert!(exit.to_stderr);
            assert!(exit.message.contains("RUSTGRAM_AGENT"));
            assert!(exit.message.contains("-workdir"));
        }

        // `-workdir` naming the default profile (any case, relative or nested) is refused too.
        let default_str = default_dir.to_string_lossy().into_owned();
        let upper = default_str.to_uppercase();
        let nested = default_dir.join("tdata").to_string_lossy().into_owned();
        for wd in [default_str.as_str(), upper.as_str(), nested.as_str()] {
            let argv = ["rustgram.exe", "--demo", "-workdir", wd];
            let resolved = resolved_for(&argv, &env);
            assert!(resolved.is_custom);
            let exit = early_exit(&argv, &resolved, &env, Some(&agent("CLAUDECODE")));
            assert_eq!(exit.map(|e| e.code), Some(2), "{wd}");
        }
    }

    #[test]
    fn agent_with_scratch_workdir_and_user_launches_start() {
        let dir = tempfile::TempDir::new().unwrap();
        let env = env(dir.path());
        let scratch = dir.path().join("scratch").to_string_lossy().into_owned();
        let argv = ["rustgram.exe", "--demo", "-workdir", scratch.as_str()];
        let resolved = resolved_for(&argv, &env);
        assert_eq!(resolved.origin, WorkingDirOrigin::CustomFlag);
        assert_eq!(
            early_exit(&argv, &resolved, &env, Some(&agent("CLAUDECODE"))),
            None
        );

        // The user's own launch (no agent variables) keeps the default profile.
        for argv in [
            &["rustgram.exe"][..],
            &["rustgram.exe", "-autostart"],
            &["rustgram.exe", "--", "tg://resolve?domain=x"],
        ] {
            let resolved = resolved_for(argv, &env);
            assert!(!resolved.is_custom);
            assert_eq!(early_exit(argv, &resolved, &env, None), None, "{argv:?}");
        }
    }

    #[test]
    fn agent_detection_uses_explicit_variables_only() {
        let detect = |vars: &[(&str, &str)]| detect_agent(vars.iter().copied());
        assert_eq!(detect(&[]), None);
        assert_eq!(
            detect(&[("PATH", "C:/Windows"), ("APPDATA", "x"), ("USERNAME", "u")]),
            None
        );
        assert_eq!(
            detect(&[("RUSTGRAM_AGENT", "1")]),
            Some(agent("RUSTGRAM_AGENT"))
        );
        assert_eq!(
            detect(&[("rustgram_agent", "yes")]),
            Some(agent("rustgram_agent"))
        );
        for off in ["0", "false", "No", "off", "", "  "] {
            assert_eq!(detect(&[("RUSTGRAM_AGENT", off)]), None, "{off:?}");
        }
        assert_eq!(detect(&[("CLAUDECODE", "1")]), Some(agent("CLAUDECODE")));
        assert_eq!(
            detect(&[("T3CODE_THREAD_ID", "thread:abc")]),
            Some(agent("T3CODE_THREAD_ID"))
        );
        assert_eq!(
            detect(&[("AI_AGENT", "claude-code_2_agent")]),
            Some(agent("AI_AGENT"))
        );
        assert_eq!(
            detect(&[("CODEX_SANDBOX_NETWORK_DISABLED", "1")]),
            Some(agent("CODEX_SANDBOX_NETWORK_DISABLED"))
        );
        // A user's global Codex configuration and T3's non-agent variables do not count.
        assert_eq!(detect(&[("CODEX_HOME", "C:/codex")]), None);
        assert_eq!(detect(&[("T3CODE_CLI_PATH", "C:/t3.cmd")]), None);
        assert_eq!(detect(&[("CLAUDECODE", "")]), None);
    }
}
