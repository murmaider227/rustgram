//! Generic `FakeRpcClient` (implementing `tg_api::RpcClient` over real `tg-tl`
//! wire serialization), server-side SRP-6a verifier, and TL test fixtures.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use num_bigint::BigUint;
use tg_api::messaging::RandomIdSource;
use tg_api::rpc::{DcId, InvocationError, RpcClient, RpcError};
use tg_crypto::dh::GOOD_PRIME;
use tg_crypto::hash::sha256;
use tg_crypto::srp::{self, PasswordKdfAlgoModPow};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, types as tl_types,
};
use tokio::sync::broadcast;

type RpcHandler = Box<dyn Fn(DcId, &[u8]) -> Result<Vec<u8>, InvocationError> + Send>;
type RpcResponseQueue = VecDeque<Result<Vec<u8>, InvocationError>>;

/// Deterministic test `RandomIdSource` yielding sequential non-zero `i64` values.
pub struct SeqRandomIdSource {
    next: i64,
}

impl SeqRandomIdSource {
    pub const fn new(start: i64) -> Self {
        Self { next: start }
    }
}

impl RandomIdSource for SeqRandomIdSource {
    fn next_random_id(&mut self) -> i64 {
        let v = self.next;
        self.next += 1;
        v
    }
}

/// Recorded raw TL RPC invocation captured by [`FakeRpcClient`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRpc {
    pub dc_id: DcId,
    pub constructor_id: u32,
    pub request_bytes: Vec<u8>,
}

impl RecordedRpc {
    /// Decodes this call as a concrete TL function struct if its constructor ID matches.
    pub fn decode<R: Identifiable + Deserializable>(&self) -> Option<R> {
        if self.constructor_id == R::CONSTRUCTOR_ID {
            R::from_bytes(&self.request_bytes).ok()
        } else {
            None
        }
    }
}

/// Programmable fake [`RpcClient`] exercising real `tg-tl` wire serialization and
/// deserialization on every call, plus a live `Updates` broadcast channel.
pub struct FakeRpcClient {
    main_dc: AtomicI32,
    dc_switches: Mutex<Vec<DcId>>,
    calls: Mutex<Vec<RecordedRpc>>,
    queues: Mutex<HashMap<u32, RpcResponseQueue>>,
    handlers: Mutex<HashMap<u32, RpcHandler>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl FakeRpcClient {
    pub fn new(initial_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(128);
        Self {
            main_dc: AtomicI32::new(initial_dc),
            dc_switches: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            queues: Mutex::new(HashMap::new()),
            handlers: Mutex::new(HashMap::new()),
            updates_tx,
        }
    }

    /// Enqueues a serialized TL success response for RPC function `R`.
    pub fn enqueue_ok<R>(&self, response: &R::Return)
    where
        R: Identifiable + RemoteCall,
        R::Return: Serializable,
    {
        self.queues
            .lock()
            .expect("lock poisoned")
            .entry(R::CONSTRUCTOR_ID)
            .or_default()
            .push_back(Ok(response.to_bytes()));
    }

    /// Enqueues an `InvocationError` response for RPC function `R`.
    pub fn enqueue_err<R: Identifiable>(&self, err: InvocationError) {
        self.queues
            .lock()
            .expect("lock poisoned")
            .entry(R::CONSTRUCTOR_ID)
            .or_default()
            .push_back(Err(err));
    }

    /// Registers a dynamic handler for RPC function `R` (used when queue for `R` is empty).
    pub fn set_handler<R, F>(&self, handler: F)
    where
        R: Identifiable + Deserializable + RemoteCall,
        R::Return: Serializable,
        F: Fn(DcId, R) -> Result<R::Return, InvocationError> + Send + 'static,
    {
        let wrapped: RpcHandler = Box::new(move |dc_id, bytes| {
            let req =
                R::from_bytes(bytes).map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            let resp = handler(dc_id, req)?;
            Ok(resp.to_bytes())
        });
        self.handlers
            .lock()
            .expect("lock poisoned")
            .insert(R::CONSTRUCTOR_ID, wrapped);
    }

    /// Returns all recorded RPC calls in order.
    pub fn recorded_calls(&self) -> Vec<RecordedRpc> {
        self.calls.lock().expect("lock poisoned").clone()
    }

    /// Returns all recorded calls matching TL function `R`, decoded from wire bytes.
    pub fn calls_of<R: Identifiable + Deserializable>(&self) -> Vec<(DcId, R)> {
        self.calls
            .lock()
            .expect("lock poisoned")
            .iter()
            .filter_map(|c| c.decode::<R>().map(|r| (c.dc_id, r)))
            .collect()
    }

    /// Returns all DC switches performed via `set_main_dc_id`.
    pub fn dc_switches(&self) -> Vec<DcId> {
        self.dc_switches.lock().expect("lock poisoned").clone()
    }

    /// Pushes an incoming `tg_tl::enums::Updates` onto the broadcast stream.
    pub fn push_update(&self, updates: tl_enums::Updates) {
        let _ = self.updates_tx.send(updates);
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    fn main_dc_id(&self) -> DcId {
        self.main_dc.load(Ordering::SeqCst)
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        self.main_dc.store(dc_id, Ordering::SeqCst);
        self.dc_switches.lock().expect("lock poisoned").push(dc_id);
    }

    async fn invoke<R: RemoteCall + Send + Sync>(
        &self,
        request: &R,
    ) -> Result<R::Return, InvocationError> {
        self.invoke_in_dc(self.main_dc_id(), request).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        request: &R,
    ) -> Result<R::Return, InvocationError> {
        let request_bytes = request.to_bytes();
        if request_bytes.len() < 4 {
            return Err(InvocationError::Transport(
                "serialized TL request shorter than 4 bytes".into(),
            ));
        }
        let constructor_id = u32::from_le_bytes([
            request_bytes[0],
            request_bytes[1],
            request_bytes[2],
            request_bytes[3],
        ]);
        self.calls.lock().expect("lock poisoned").push(RecordedRpc {
            dc_id,
            constructor_id,
            request_bytes: request_bytes.clone(),
        });

        let queued = self
            .queues
            .lock()
            .expect("lock poisoned")
            .get_mut(&constructor_id)
            .and_then(VecDeque::pop_front);

        let resp_bytes = match queued {
            Some(res) => res?,
            None => {
                let handlers = self.handlers.lock().expect("lock poisoned");
                if let Some(handler) = handlers.get(&constructor_id) {
                    handler(dc_id, &request_bytes)?
                } else {
                    let name = tg_tl::from_constructor_id(constructor_id).unwrap_or("unknown");
                    return Err(InvocationError::Transport(format!(
                        "no queued fake response for {name} (0x{constructor_id:08x}) on DC {dc_id}"
                    )));
                }
            }
        };

        R::Return::from_bytes(&resp_bytes)
            .map_err(|e| InvocationError::Deserialize(format!("fake response decode: {e}")))
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

/// Convenience helper for building an `InvocationError::Rpc`.
pub fn rpc_err(code: i32, name: &str) -> InvocationError {
    InvocationError::Rpc(RpcError::new(code, name))
}

// ---------------------------------------------------------------------------
// Real server-side SRP-6a fixture for 2FA login verification
// ---------------------------------------------------------------------------

/// Simulated Telegram SRP-6a server holding real safe-prime parameters,
/// verifier `v = g^x mod p`, ephemeral `b`, and public `B = (k*v + g^b) mod p`.
pub struct SrpTestServer {
    pub algo: PasswordKdfAlgoModPow,
    pub srp_id: i64,
    pub v: BigUint,
    pub b_secret: BigUint,
    pub srp_b: [u8; 256],
}

impl SrpTestServer {
    /// Builds a real SRP-6a challenge for `password` using Telegram's 2048-bit `GOOD_PRIME` (`g = 3`).
    pub fn new(password: &str, srp_id: i64) -> Self {
        let algo = PasswordKdfAlgoModPow {
            salt1: b"test_salt_one_12345678".to_vec(),
            salt2: b"test_salt_two_87654321".to_vec(),
            g: 3,
            p: GOOD_PRIME.to_vec(),
        };
        let v_bytes = srp::compute_digest(&algo, password.as_bytes()).expect("valid GOOD_PRIME");
        let v = BigUint::from_bytes_be(&v_bytes);
        let p = BigUint::from_bytes_be(&algo.p);
        let g = BigUint::from(algo.g as u32);

        // Deterministic 256-byte server ephemeral secret `b`.
        let mut b_raw = [0u8; 256];
        for (i, byte) in b_raw.iter_mut().enumerate() {
            *byte = ((i.wrapping_mul(37).wrapping_add(19)) & 0xFF) as u8;
        }
        b_raw[0] |= 0x40;
        let b_secret = BigUint::from_bytes_be(&b_raw);

        let p_256 = pad_256(&algo.p);
        let g_256 = pad_256(&g.to_bytes_be());
        let k = BigUint::from_bytes_be(&sha256(&[&p_256, &g_256]));
        let g_b = g.modpow(&b_secret, &p);
        let b_pub = ((&k * &v) + &g_b) % &p;
        let srp_b = pad_256(&b_pub.to_bytes_be());

        Self {
            algo,
            srp_id,
            v,
            b_secret,
            srp_b,
        }
    }

    /// Returns a TL `account.Password` advertising this SRP challenge.
    pub fn tl_account_password(
        &self,
        hint: Option<&str>,
        has_recovery: bool,
    ) -> tl_types::account::Password {
        tl_types::account::Password {
            has_recovery,
            has_secure_values: false,
            has_password: true,
            current_algo: Some(
                tl_enums::PasswordKdfAlgo::SHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow(
                    tl_types::PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow {
                        salt1: self.algo.salt1.clone(),
                        salt2: self.algo.salt2.clone(),
                        g: self.algo.g,
                        p: self.algo.p.clone(),
                    },
                ),
            ),
            srp_b: Some(self.srp_b.to_vec()),
            srp_id: Some(self.srp_id),
            hint: hint.map(str::to_owned),
            email_unconfirmed_pattern: None,
            new_algo: tl_enums::PasswordKdfAlgo::Unknown,
            new_secure_algo: tl_enums::SecurePasswordKdfAlgo::Unknown,
            secure_random: vec![1, 2, 3, 4],
            pending_reset_date: None,
            login_email_pattern: None,
        }
    }

    /// Verifies a client's TL `inputCheckPasswordSRP` (`srp_id`, `A`, `M1`) using server-side SRP-6a.
    pub fn verify_check(&self, check: &tl_types::InputCheckPasswordSRP) -> bool {
        if check.srp_id != self.srp_id || check.a.len() > 256 || check.m1.len() != 32 {
            return false;
        }
        let p = BigUint::from_bytes_be(&self.algo.p);
        let g = BigUint::from(self.algo.g as u32);
        let a_pub = BigUint::from_bytes_be(&check.a);
        if a_pub == BigUint::from(0u32) || a_pub >= p {
            return false;
        }
        let a_256 = pad_256(&check.a);
        let u = BigUint::from_bytes_be(&sha256(&[&a_256, &self.srp_b]));
        if u == BigUint::from(0u32) {
            return false;
        }
        // Server computes S = (A * v^u)^b mod p.
        let v_u = self.v.modpow(&u, &p);
        let base = (&a_pub * &v_u) % &p;
        let s = base.modpow(&self.b_secret, &p);
        let s_256 = pad_256(&s.to_bytes_be());
        let k_session = sha256(&[&s_256]);

        let p_256 = pad_256(&self.algo.p);
        let g_256 = pad_256(&g.to_bytes_be());
        let h_p = sha256(&[&p_256]);
        let h_g = sha256(&[&g_256]);
        let mut h_xor = [0u8; 32];
        for i in 0..32 {
            h_xor[i] = h_p[i] ^ h_g[i];
        }
        let h_s1 = sha256(&[&self.algo.salt1]);
        let h_s2 = sha256(&[&self.algo.salt2]);
        let expected_m1 = sha256(&[&h_xor, &h_s1, &h_s2, &a_256, &self.srp_b, &k_session]);

        check.m1.as_slice() == expected_m1.as_slice()
    }
}

fn pad_256(bytes: &[u8]) -> [u8; 256] {
    let mut out = [0u8; 256];
    let start = 256 - bytes.len();
    out[start..].copy_from_slice(bytes);
    out
}

// ---------------------------------------------------------------------------
// TL object builders for clean tests (synthetic names only)
// ---------------------------------------------------------------------------

pub fn sample_tl_user(
    id: i64,
    access_hash: i64,
    first_name: &str,
    is_self: bool,
) -> tl_types::User {
    tl_types::User {
        self_: is_self,
        contact: !is_self,
        mutual_contact: false,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: true,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_forum_sidebar: false,
        bot_guard: false,
        bot_guestchat: false,
        id,
        access_hash: Some(access_hash),
        first_name: Some(first_name.to_owned()),
        last_name: Some("Tester".to_owned()),
        username: Some(format!("user_{id}")),
        phone: Some(format!("999000{id}")),
        photo: None,
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    }
}

pub fn sample_tl_self_user() -> tl_types::User {
    sample_tl_user(123_456, 999, "Test", true)
}

pub fn sample_tl_auth(user: tl_types::User) -> tl_enums::auth::Authorization {
    tl_enums::auth::Authorization::Authorization(tl_types::auth::Authorization {
        setup_password_required: false,
        otherwise_relogin_days: None,
        tmp_sessions: None,
        future_auth_token: None,
        user: tl_enums::User::User(user),
    })
}

pub fn sample_tl_message(
    peer: tl_enums::Peer,
    from: Option<tl_enums::Peer>,
    id: i32,
    date: i32,
    text: &str,
) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: false,
        from_scheduled: false,
        legacy: false,
        edit_hide: false,
        pinned: false,
        noforwards: false,
        invert_media: false,
        offline: false,
        video_processing_pending: false,
        paid_suggested_post_stars: false,
        paid_suggested_post_ton: false,
        guestchat_via_from: None,
        id,
        from_id: from,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: peer,
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date,
        message: text.to_owned(),
        media: None,
        reply_markup: None,
        entities: None,
        views: None,
        forwards: None,
        replies: None,
        edit_date: None,
        post_author: None,
        grouped_id: None,
        reactions: None,
        restriction_reason: None,
        ttl_period: None,
        quick_reply_shortcut_id: None,
        effect: None,
        factcheck: None,
        report_delivery_until_date: None,
        paid_message_stars: None,
        suggested_post: None,
        schedule_repeat_period: None,
        summary_from_language: None,
        rich_message: None,
    })
}

pub fn sample_tl_dialog(peer: tl_enums::Peer, top_message: i32) -> tl_enums::Dialog {
    tl_enums::Dialog::Dialog(tl_types::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer,
        top_message,
        read_inbox_max_id: top_message,
        read_outbox_max_id: top_message,
        unread_count: 0,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: tl_enums::PeerNotifySettings::PeerNotifySettings(
            tl_types::PeerNotifySettings {
                show_previews: None,
                silent: None,
                mute_until: None,
                ios_sound: None,
                android_sound: None,
                other_sound: None,
                stories_muted: None,
                stories_hide_sender: None,
                stories_ios_sound: None,
                stories_android_sound: None,
                stories_other_sound: None,
            },
        ),
        pts: None,
        draft: None,
        folder_id: Some(0),
        ttl_period: None,
    })
}

pub fn sample_tl_channel(
    id: i64,
    access_hash: i64,
    title: &str,
    megagroup: bool,
) -> tl_types::Channel {
    tl_types::Channel {
        creator: false,
        left: false,
        broadcast: !megagroup,
        verified: false,
        megagroup,
        restricted: false,
        signatures: false,
        min: false,
        scam: false,
        has_link: false,
        has_geo: false,
        slowmode_enabled: false,
        call_active: false,
        call_not_empty: false,
        fake: false,
        gigagroup: false,
        noforwards: false,
        join_to_send: false,
        join_request: false,
        forum: false,
        stories_hidden: false,
        stories_hidden_min: false,
        stories_unavailable: true,
        signature_profiles: false,
        autotranslation: false,
        broadcast_messages_allowed: false,
        monoforum: false,
        forum_tabs: false,
        forum_sidebar: false,
        in_news: false,
        id,
        access_hash: Some(access_hash),
        title: title.to_owned(),
        username: Some(format!("ch_{id}")),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_700_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: Some(42),
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        emoji_status: None,
        level: None,
        subscription_until_date: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_monoforum_id: None,
        linked_community_id: None,
        pinned_msg_id: None,
    }
}
