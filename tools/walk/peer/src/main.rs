//! Scenario-walk helper for two Telegram TEST-server accounts (A and B).
//!
//! ```text
//! tg-walk-peer fixture    --a <login A> --b <login B> --state <state.json> --media <dir>
//! tg-walk-peer serve      --a <login A> --b <login B> --state <state.json> --media <dir>
//! tg-walk-peer mint       --approver <login> --out <empty dir>
//! tg-walk-peer newaccount --out <empty dir> --state <state.json> [--name <first name>]
//! tg-walk-peer retire     --b <login B> --state <old state.json>
//! tg-walk-peer solo-fixture|solo-serve --main <login> --who a|b --state <state.json>
//! tg-walk-peer approveqr  --approver <login> --grey <raw 8-bit file> --width W --height H
//! ```
//!
//! One login directory = one running process: every command locks the login dirs it opens
//! (`login.lock`) and refuses a dir another process holds. Never copy a login dir and run the
//! copies: the server drops a key that two processes use at once (AUTH_KEY_DUPLICATED). Make a
//! new login with `mint` instead.
//!
//! Accounts are opened with a transport guard that refuses every non-test data center.
//! Credentials are tdesktop's public test-only pair, read at run time (`TDESKTOP_DIR` or
//! `RUSTGRAM_API_CREDS_FILE`) and never printed. Nothing that identifies the accounts is
//! printed; the state file (ids, access hashes, message ids, the test number of a new account)
//! stays where `--state` points, outside the repo.

mod fixture;
mod guard;
mod lock;
mod newlogin;
mod ops;
mod qr;
mod retire;
mod serve;
mod side;
mod solo;
mod world;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

use side::{Side, load_test_credentials};
use world::World;

const USAGE: &str = "usage: tg-walk-peer fixture|serve --a DIR --b DIR --state FILE --media DIR \
                     | mint --approver DIR --out DIR | newaccount --out DIR --state FILE [--name NAME] \
                     | retire --b DIR --state FILE";

pub(crate) fn decode_b64url(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.trim().trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

struct Args {
    mode: String,
    kv: HashMap<String, String>,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut it = std::env::args().skip(1);
        let mode = it.next().ok_or(USAGE)?;
        let mut kv = HashMap::new();
        while let Some(k) = it.next() {
            let key = k
                .strip_prefix("--")
                .ok_or(format!("unexpected {k}; {USAGE}"))?;
            let v = it.next().ok_or(format!("{k} needs a value"))?;
            let low = v.to_lowercase().replace('\\', "/");
            if low.contains("appdata/roaming") {
                return Err("SAFETY: refusing a profile under AppData/Roaming".into());
            }
            kv.insert(key.to_owned(), v);
        }
        Ok(Self { mode, kv })
    }

    fn path(&self, k: &str) -> Result<PathBuf, String> {
        self.kv
            .get(k)
            .map(PathBuf::from)
            .ok_or(format!("--{k} is required; {USAGE}"))
    }
}

async fn run(args: Args) -> Result<(), String> {
    match args.mode.as_str() {
        "mint" => newlogin::mint(&args.path("approver")?, &args.path("out")?).await,
        "newaccount" => {
            let name = args.kv.get("name").map_or("Walk Tester", String::as_str);
            newlogin::new_account(&args.path("out")?, &args.path("state")?, name).await
        }
        "retire" => retire::run(&args.path("b")?, &args.path("state")?).await,
        "solo-fixture" | "solo-serve" => {
            let who = args.kv.get("who").map_or("b", String::as_str);
            let (main, state) = (args.path("main")?, args.path("state")?);
            if args.mode == "solo-fixture" {
                solo::fixture(&main, who, &state).await
            } else {
                solo::serve(&main, who, &state).await
            }
        }
        "approveqr" => {
            let dim = |k: &str| -> Result<usize, String> {
                args.kv
                    .get(k)
                    .and_then(|v| v.parse().ok())
                    .ok_or(format!("--{k} is required"))
            };
            qr::approve(
                &args.path("approver")?,
                &args.path("grey")?,
                dim("width")?,
                dim("height")?,
            )
            .await
        }
        "fixture" | "serve" => {
            let creds = load_test_credentials()?;
            let a = Side::open("a", &args.path("a")?, &creds).await?;
            let b = Side::open("b", &args.path("b")?, &creds).await?;
            if a.self_id == b.self_id {
                return Err("--a and --b are the same account".into());
            }
            let state_path = args.path("state")?;
            let state = World::load_state(&state_path);
            let mut w = World {
                a,
                b,
                state,
                state_path,
            };
            let media = args.path("media")?;
            if args.mode == "fixture" {
                fixture::run(&mut w, &media).await
            } else {
                w.ensure_user_hashes().await?;
                serve::run(&mut w, &media).await
            }
        }
        m => Err(format!("unknown mode {m}; {USAGE}")),
    }
}

fn main() -> ExitCode {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("tokio: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("walk peer failed: {e}");
            ExitCode::FAILURE
        }
    }
}
