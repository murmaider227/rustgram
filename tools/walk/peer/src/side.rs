//! One logged-in test account (A or B) opened from a scratch copy of its working directory.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tg_api::{ApiCredentials, InvocationError, RpcClient};
use tg_core::domain::Environment as CoreEnvironment;
use tg_session::{AccountManager, MtprotoRpc, WorkingDirOpenOptions};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::guard::TestDcGuard;

/// FLOOD_WAIT above this many seconds stops the run (owner rule).
pub const MAX_FLOOD_WAIT_SECS: u32 = 60;

pub struct Side {
    pub name: &'static str,
    pub rpc: MtprotoRpc,
    pub self_id: i64,
    pub premium: bool,
    rng: parking_lot::Mutex<u64>,
    // Keeps the account manager (and its connections) alive.
    _mgr: AccountManager,
    // Dropped last: the login stays locked while its connections live.
    _lock: crate::lock::LoginLock,
}

/// Loads tdesktop's public TEST-ONLY credentials at run time; never printed.
pub fn load_test_credentials() -> Result<ApiCredentials, String> {
    if let Ok(c) = tg_session::load_api_credentials_from_env() {
        return Ok(c);
    }
    let path = std::env::var("RUSTGRAM_API_CREDS_FILE")
        .or_else(|_| std::env::var("TDESKTOP_DIR").map(|d| format!("{d}/docs/api_credentials.md")))
        .map_err(|_| "set RUSTGRAM_API_CREDS_FILE or TDESKTOP_DIR".to_owned())?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("credentials doc: {e}"))?;
    let mut api_id = None;
    let mut api_hash = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("**api_id**:") {
            api_id = rest.trim().parse::<i32>().ok();
        } else if let Some(rest) = t.strip_prefix("**api_hash**:") {
            api_hash = Some(rest.trim().to_owned());
        }
    }
    match (api_id, api_hash) {
        (Some(api_id), Some(api_hash)) if !api_hash.is_empty() => {
            Ok(ApiCredentials { api_id, api_hash })
        }
        _ => Err("could not read the test credentials".into()),
    }
}

pub fn now() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i32::try_from(d.as_secs()).unwrap_or(i32::MAX))
        .unwrap_or(0)
}

impl Side {
    pub async fn open(
        name: &'static str,
        dir: &Path,
        creds: &ApiCredentials,
    ) -> Result<Self, String> {
        let lock = crate::lock::acquire(dir)?;
        let guard = Arc::new(TestDcGuard::new());
        let opts = WorkingDirOpenOptions::new(dir, creds.clone())
            .with_test_mode(true)
            .with_transport_factory(guard.clone());
        let mgr = AccountManager::open_and_unlock(opts, b"")
            .map_err(|e| format!("{name}: open working dir failed: {e}"))?;
        let slot = mgr
            .active_index()
            .ok_or(format!("{name}: no active account"))?;
        let acc = mgr
            .account(slot)
            .ok_or(format!("{name}: no account in slot"))?;
        if acc.environment != CoreEnvironment::Test {
            return Err(format!(
                "SAFETY: {name} is not a test-server account; refusing"
            ));
        }
        let self_id = i64::try_from(acc.user_id.ok_or(format!("{name}: not logged in"))?.0)
            .map_err(|_| format!("{name}: bad user id"))?;
        let rpc = acc.rpc.clone();
        let client = acc.client.clone();
        let tl_enums::Config::Config(cfg) = rpc
            .invoke(&tl_fns::help::GetConfig)
            .await
            .map_err(|e| format!("{name}: help.getConfig: {e}"))?;
        guard.register_config(&cfg)?;
        if !client.apply_tl_config(&cfg) {
            return Err(format!("{name}: test config rejected"));
        }
        let users = rpc
            .invoke(&tl_fns::users::GetUsers {
                id: vec![tl_enums::InputUser::Self_],
            })
            .await
            .map_err(|e| format!("{name}: users.getUsers: {e}"))?;
        let premium = users
            .iter()
            .any(|u| matches!(u, tl_enums::User::User(u) if u.premium));
        let seed = u64::try_from(self_id).unwrap_or(1) ^ (now() as u64).rotate_left(17);
        Ok(Self {
            name,
            rpc,
            self_id,
            premium,
            rng: parking_lot::Mutex::new(seed | 1),
            _mgr: mgr,
            _lock: lock,
        })
    }

    pub fn random_id(&self) -> i64 {
        let mut s = self.rng.lock();
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s as i64
    }

    /// Invokes `req`, waiting out FLOOD_WAIT up to the limit, retrying a few times.
    pub async fn call<R: RemoteCall + Send + Sync>(
        &self,
        what: &str,
        req: &R,
    ) -> Result<R::Return, String> {
        let mut tries = 0;
        loop {
            match self.rpc.invoke(req).await {
                Ok(v) => return Ok(v),
                Err(InvocationError::Rpc(r)) if r.flood_wait_seconds().is_some() => {
                    let secs = r.flood_wait_seconds().unwrap_or(0);
                    if secs > MAX_FLOOD_WAIT_SECS {
                        return Err(format!(
                            "FLOOD_WAIT_{secs} (>60 s) on {} {what}: stopping",
                            self.name
                        ));
                    }
                    tries += 1;
                    if tries > 4 {
                        return Err(format!("{} {what}: repeated FLOOD_WAIT", self.name));
                    }
                    eprintln!("[{}] {what}: FLOOD_WAIT {secs} s, waiting", self.name);
                    tokio::time::sleep(Duration::from_secs(u64::from(secs) + 1)).await;
                }
                Err(e) => return Err(format!("{} {what}: {e}", self.name)),
            }
        }
    }
}

/// Every message id an `Updates` answer reports, in order of appearance.
pub fn msg_ids(updates: &tl_enums::Updates) -> Vec<i32> {
    let mut ids = Vec::new();
    let mut one = |u: &tl_enums::Update| match u {
        tl_enums::Update::NewMessage(m) => ids.push(message_id(&m.message)),
        tl_enums::Update::NewChannelMessage(m) => ids.push(message_id(&m.message)),
        tl_enums::Update::NewScheduledMessage(m) => ids.push(message_id(&m.message)),
        _ => {}
    };
    match updates {
        tl_enums::Updates::UpdateShortSentMessage(s) => ids.push(s.id),
        tl_enums::Updates::UpdateShortMessage(s) => ids.push(s.id),
        tl_enums::Updates::UpdateShortChatMessage(s) => ids.push(s.id),
        tl_enums::Updates::UpdateShort(s) => one(&s.update),
        tl_enums::Updates::Updates(u) => u.updates.iter().for_each(one),
        tl_enums::Updates::Combined(c) => c.updates.iter().for_each(one),
        tl_enums::Updates::TooLong => {}
    }
    ids
}

pub fn message_id(m: &tl_enums::Message) -> i32 {
    match m {
        tl_enums::Message::Empty(m) => m.id,
        tl_enums::Message::Message(m) => m.id,
        tl_enums::Message::Service(m) => m.id,
    }
}

/// Chats carried by an `Updates` answer.
pub fn updates_chats(updates: &tl_enums::Updates) -> Vec<tl_enums::Chat> {
    match updates {
        tl_enums::Updates::Updates(u) => u.chats.clone(),
        tl_enums::Updates::Combined(c) => c.chats.clone(),
        _ => Vec::new(),
    }
}

pub fn twe(text: &str) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: text.to_owned(),
        entities: Vec::new(),
    })
}

/// UTF-16 length, the unit of message entity offsets.
pub fn u16len(s: &str) -> i32 {
    i32::try_from(s.encode_utf16().count()).unwrap_or(i32::MAX)
}
