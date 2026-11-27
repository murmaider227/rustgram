//! Logins of TEST-server accounts for the walk and for other agents.
//!
//! One authorization key may only ever be used by ONE running process: the server drops a key
//! that two processes use at once (AUTH_KEY_DUPLICATED / AUTH_KEY_DROP_DUPLICATE), which logged a
//! shared test account out on 2026-11-27. So nobody copies a login directory and runs the copies;
//! every client gets its own login, minted here:
//!
//! - `mint`: a new session of the same account in an empty dir, through a QR login that an existing
//!   "approver" session confirms (`auth.acceptLoginToken`). The approver dir is locked while used.
//! - `new_account`: a fresh throwaway account on the test servers with a random 99966XYYYY number
//!   (the test servers' fixed code is X five times). The number goes only into the state file.
//!
//! Nothing about the accounts is printed.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tg_api::LoginStep;
use tg_core::domain::Environment as CoreEnvironment;
use tg_session::{AccountManager, LoginFlow, WorkingDirOpenOptions};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::guard::TestDcGuard;
use crate::side::{Side, load_test_credentials, now};

fn token_of(url: &str) -> Option<Vec<u8>> {
    let t = url.split("token=").nth(1)?;
    crate::decode_b64url(t.split('&').next()?)
}

/// Opens an EMPTY dir as a new test-server account slot (locked for this process).
fn new_slot(
    out_dir: &Path,
    creds: &tg_api::ApiCredentials,
) -> Result<(AccountManager, i32, crate::lock::LoginLock), String> {
    if out_dir.join("tdata").exists() {
        return Err(format!(
            "{} already holds a login; mint into an empty dir",
            out_dir.display()
        ));
    }
    let lock = crate::lock::acquire(out_dir)?;
    let guard = Arc::new(TestDcGuard::new());
    let opts = WorkingDirOpenOptions::new(out_dir, creds.clone())
        .with_test_mode(true)
        .with_transport_factory(guard);
    let mut mgr = AccountManager::open_and_unlock(opts, b"")
        .map_err(|e| format!("open new working dir: {e}"))?;
    let mut index = mgr.active_index().ok_or("no account slot")?;
    let needs_slot = mgr
        .account(index)
        .is_some_and(|a| a.is_authorized() || a.environment != CoreEnvironment::Test);
    if needs_slot {
        index = mgr
            .add_account(CoreEnvironment::Test)
            .map_err(|e| format!("add test account slot: {e}"))?;
    }
    if mgr.account(index).map(|a| a.environment) != Some(CoreEnvironment::Test) {
        return Err("SAFETY: the new slot is not a test-server slot".into());
    }
    Ok((mgr, index, lock))
}

pub async fn mint(approver_dir: &Path, out_dir: &Path) -> Result<(), String> {
    let creds = load_test_credentials()?;
    let approver = Side::open("approver", approver_dir, &creds).await?;
    let (mut mgr, index, _lock) = new_slot(out_dir, &creds)?;
    let rpc = mgr.account(index).ok_or("no new account")?.rpc.clone();
    let mut flow = LoginFlow::new(rpc, creds);
    flow.start_qr_login(now(), Vec::new())
        .await
        .map_err(|e| format!("start QR login: {e}"))?;
    let mut approved = false;
    for _ in 0..15 {
        match flow.step().clone() {
            LoginStep::QrLogin(qr) => {
                if !approved {
                    let token = token_of(&qr.qr_url).ok_or("no token in the QR url")?;
                    approver
                        .call(
                            "acceptLoginToken",
                            &tl_fns::auth::AcceptLoginToken { token },
                        )
                        .await?;
                    approved = true;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                flow.refresh_qr_token(now())
                    .await
                    .map_err(|e| format!("refresh QR token: {e}"))?;
            }
            LoginStep::Authorized { user, .. } => {
                if user.id != approver.self_id {
                    return Err("the new session is a different account".into());
                }
                mgr.complete_login(index, &user)
                    .map_err(|e| format!("save the new session: {e}"))?;
                println!("new login saved");
                return Ok(());
            }
            LoginStep::PasswordCheck { .. } => {
                return Err("the account has a 2FA password; not supported".into());
            }
            _ => return Err("unexpected login step".into()),
        }
    }
    Err("the QR login was not confirmed in time".into())
}

fn random_test_phone(seed: u64) -> String {
    let dc = 1 + (seed % 3);
    let rest = (seed / 3) % 10_000;
    format!("99966{dc}{rest:04}")
}

fn is_test_phone(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() == 10
        && p.starts_with("99966")
        && (b'1'..=b'3').contains(&b[5])
        && b.iter().all(u8::is_ascii_digit)
}

/// Registers a new throwaway account with a free random test number. A number that already
/// belongs to an account (someone else's test account) is logged out at once and skipped.
pub async fn new_account(
    out_dir: &Path,
    state_path: &Path,
    first_name: &str,
) -> Result<(), String> {
    let creds = load_test_credentials()?;
    let mut seed = (std::process::id() as u64) ^ (now() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for attempt in 0..8 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let phone = random_test_phone(seed >> 20);
        if !is_test_phone(&phone) {
            return Err("SAFETY: generated a non-test number".into());
        }
        let code: String = std::iter::repeat_n(phone.as_bytes()[5] as char, 5).collect();
        let attempt_dir = out_dir.join(format!("attempt{attempt}"));
        let (mut mgr, index, lock) = new_slot(&attempt_dir, &creds)?;
        let rpc = mgr.account(index).ok_or("no new account")?.rpc.clone();
        // Like the test_dc_smoke harness: start on the number's own DC.
        let dc = i32::from(phone.as_bytes()[5] - b'0');
        if let Some(acc) = mgr.account(index) {
            acc.client.set_main_dc_id(dc);
        }
        let user = match raw_sign_in(&rpc, &mgr, index, &creds, &phone, &code, first_name).await {
            Ok(RawLogin::New(user)) => user,
            Ok(RawLogin::Existing) => {
                eprintln!(
                    "[newaccount] attempt {attempt}: number already has an account; logging out and skipping"
                );
                let _ = mgr.logout_account(index).await;
                drop((mgr, lock));
                let _ = std::fs::remove_dir_all(&attempt_dir);
                continue;
            }
            Err(msg) => {
                eprintln!("[newaccount] attempt {attempt}: refused ({})", short(&msg));
                if let Some(secs) = flood_secs(&msg)
                    && secs > crate::side::MAX_FLOOD_WAIT_SECS
                {
                    return Err(format!("FLOOD_WAIT_{secs} (>60 s): stopping"));
                }
                drop((mgr, lock));
                let _ = std::fs::remove_dir_all(&attempt_dir);
                continue;
            }
        };
        mgr.complete_login(index, &user)
            .map_err(|e| format!("save the new account: {e}"))?;
        drop((mgr, lock));
        // Move the login into place: <out>/tdata.
        std::fs::rename(attempt_dir.join("tdata"), out_dir.join("tdata"))
            .map_err(|e| format!("move login: {e}"))?;
        let _ = std::fs::remove_dir_all(&attempt_dir);
        let mut state = crate::world::World::load_state(&state_path.to_path_buf());
        state["a_phone"] = serde_json::json!(phone);
        state["a_created"] = serde_json::json!(now());
        let text = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        std::fs::write(state_path, text).map_err(|e| format!("write state: {e}"))?;
        println!("new account saved (attempt {attempt})");
        return Ok(());
    }
    Err("no free test number after 8 attempts".into())
}

fn short(s: &str) -> String {
    s.chars().take(120).collect()
}

fn flood_secs(msg: &str) -> Option<u32> {
    let i = msg.find("FLOOD_WAIT_")?;
    msg[i + 11..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

async fn rpc_sign_up(
    rpc: &tg_session::MtprotoRpc,
    phone: &str,
    phone_hash: &str,
    first_name: &str,
) -> Result<tl_types::User, String> {
    use tg_api::RpcClient;
    let got = rpc
        .invoke(&tl_fns::auth::SignUp {
            no_joined_notifications: true,
            phone_number: phone.to_owned(),
            phone_code_hash: phone_hash.to_owned(),
            first_name: first_name.to_owned(),
            last_name: String::new(),
        })
        .await
        .map_err(|e| format!("auth.signUp: {e}"))?;
    match got {
        tl_enums::auth::Authorization::Authorization(a) => match a.user {
            tl_enums::User::User(u) => Ok(u),
            tl_enums::User::Empty(_) => Err("auth.signUp returned no user".into()),
        },
        _ => Err("auth.signUp asked for another step".into()),
    }
}

#[allow(clippy::large_enum_variant)] // built once per login attempt
enum RawLogin {
    New(tl_types::User),
    Existing,
}

/// auth.sendCode + auth.signIn (+ auth.signUp for a free number) on the number's own DC,
/// following PHONE_MIGRATE once.
async fn raw_sign_in(
    rpc: &tg_session::MtprotoRpc,
    mgr: &AccountManager,
    index: i32,
    creds: &tg_api::ApiCredentials,
    phone: &str,
    code: &str,
    first_name: &str,
) -> Result<RawLogin, String> {
    use tg_api::{InvocationError, RpcClient};
    let send = tl_fns::auth::SendCode {
        phone_number: phone.to_owned(),
        api_id: creds.api_id,
        api_hash: creds.api_hash.clone(),
        settings: tl_enums::CodeSettings::CodeSettings(tl_types::CodeSettings {
            allow_flashcall: false,
            current_number: false,
            allow_app_hash: false,
            allow_missed_call: false,
            allow_firebase: false,
            unknown_number: false,
            logout_tokens: None,
            token: None,
            app_sandbox: None,
        }),
    };
    let mut sent = rpc.invoke(&send).await;
    if let Err(InvocationError::Rpc(r)) = &sent
        && r.name.starts_with("PHONE_MIGRATE")
        && let Some(dc) = r.value
    {
        if let Some(acc) = mgr.account(index) {
            acc.client.set_main_dc_id(dc as i32);
        }
        sent = rpc.invoke(&send).await;
    }
    let sent = sent.map_err(|e| format!("auth.sendCode: {e}"))?;
    let hash = match sent {
        tl_enums::auth::SentCode::SentCode(s) => {
            let ty = format!("{:?}", s.r#type);
            eprintln!(
                "[newaccount] code sent as {} (next {:?})",
                ty.split(['(', '{']).next().unwrap_or("?").trim(),
                s.next_type
            );
            s.phone_code_hash
        }
        _ => return Err("auth.sendCode: unexpected answer".into()),
    };
    let got = rpc
        .invoke(&tl_fns::auth::SignIn {
            phone_number: phone.to_owned(),
            phone_code_hash: hash.clone(),
            phone_code: Some(code.to_owned()),
            email_verification: None,
        })
        .await
        .map_err(|e| format!("auth.signIn: {e}"))?;
    match got {
        tl_enums::auth::Authorization::Authorization(_) => Ok(RawLogin::Existing),
        tl_enums::auth::Authorization::SignUpRequired(_) => Ok(RawLogin::New(
            rpc_sign_up(rpc, phone, &hash, first_name).await?,
        )),
    }
}
