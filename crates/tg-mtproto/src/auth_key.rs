//! Port of `mtproto/details/mtproto_dc_key_creator.cpp`,
//! `mtproto/details/mtproto_dc_key_binder.cpp`, `mtproto/details/mtproto_bound_key_creator.cpp`,
//! and `mtproto/details/mtproto_dcenter.cpp`.
//!
//! Implements the 3-roundtrip MTProto DH auth key exchange (both permanent and PFS temporary
//! keys), `auth.bindTempAuthKey` inner encryption & response verification, and per-DC `Dcenter`
//! key coordination.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use rand::RngCore;
use tg_crypto::{
    AuthKey, Side, const_time_eq, dh, encrypt_data_v1, factorize_bytes, sha1, try_aes_ige_decrypt,
    try_aes_ige_encrypt,
};
use tg_tl::functions::auth::BindTempAuthKey;
use tg_tl::mtproto::{enums as mtp_enums, functions as mtp_functions, types as mtp_types};
use tg_tl::{BOOL_TRUE_ID, Cursor, Deserializable, Identifiable, Serializable};
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::dc_options::{DcId, DcOptions, DcType};
use crate::error::InvocationError;
use crate::message::{decode_unencrypted_packet, encode_unencrypted_packet};
use crate::time::MtprotoClock;
use crate::transport_adapter::PacketConnection;

/// Default PFS temporary key lifetime in seconds (`kTemporaryExpiresIn = 86400`, 24 hours).
pub const TEMPORARY_EXPIRES_IN_SECS: i32 = 86_400;
/// Extra expiry margin added when binding temporary keys (`kBindKeyAdditionalExpiresTimeout = 30`).
pub const BIND_KEY_ADDITIONAL_EXPIRES_TIMEOUT_SECS: i32 = 30;
/// Minimum age before a persistent key may be declared forgotten on `ENCRYPTED_MESSAGE_INVALID`.
pub const KEY_OLD_ENOUGH_FOR_DESTROY: Duration = Duration::from_secs(60);
/// Maximum DH `dh_gen_retry` attempts (`mtproto_dc_key_creator.cpp`: 5 retries).
const MAX_DH_RETRIES: u32 = 5;

/// Origin/kind of an [`ManagedAuthKey`] (`AuthKey::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKeyType {
    /// Loaded from persistent storage (`tdata`).
    ReadFromFile,
    /// Newly generated persistent key.
    Generated,
    /// Ephemeral PFS temporary key.
    Temporary,
}

/// Auth key paired with its DC metadata and expiry (`MTP::AuthKey`).
#[derive(Debug, Clone)]
pub struct ManagedAuthKey {
    /// Underlying 2048-bit key material.
    pub key: AuthKey,
    /// Key origin/kind.
    pub key_type: AuthKeyType,
    /// Bare DC ID that owns this key.
    pub dc_id: DcId,
    /// Local monotonic creation timestamp (used for `destroyOldEnoughPersistentKey`).
    pub created_at: Instant,
    /// Unix timestamp when a temporary key expires (`None` for persistent keys).
    pub expires_at: Option<i32>,
}

impl ManagedAuthKey {
    /// Creates a new `ManagedAuthKey`.
    pub fn new(key: AuthKey, key_type: AuthKeyType, dc_id: DcId) -> Self {
        Self {
            key,
            key_type,
            dc_id,
            created_at: Instant::now(),
            expires_at: None,
        }
    }

    /// 64-bit key identifier.
    pub fn key_id(&self) -> u64 {
        self.key.key_id_u64()
    }
}

/// Request specification for [`create_dc_keys`] (`MTP::details::DcKeyRequest`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DcKeyRequest {
    /// Whether a new persistent auth key must be created first.
    pub persistent_needed: bool,
    /// Lifetime in seconds for the temporary key (`0` = create only persistent key, no PFS temp key).
    pub temporary_expires_in: i32,
}

/// Output of [`create_dc_keys`] (`MTP::details::DcKeyResult`).
#[derive(Debug, Clone)]
pub struct DcKeyResult {
    /// Newly created persistent key, if requested.
    pub persistent_key: Option<ManagedAuthKey>,
    /// Initial server salt derived during persistent key creation.
    pub persistent_server_salt: u64,
    /// Newly created temporary key, if requested.
    pub temporary_key: Option<ManagedAuthKey>,
    /// Initial server salt derived during temporary key creation.
    pub temporary_server_salt: u64,
}

/// Runs a single MTProto DH handshake on `conn` to create either a persistent (`expires_in == 0`)
/// or temporary (`expires_in > 0`) auth key (port of `DcKeyCreator`).
///
/// Returns `(ManagedAuthKey, initial_server_salt)`.
pub async fn create_single_auth_key(
    conn: &mut dyn PacketConnection,
    dc_id: DcId,
    protocol_dc_id: i16,
    dc_options: &DcOptions,
    clock: &MtprotoClock,
    expires_in: i32,
) -> Result<(ManagedAuthKey, u64), InvocationError> {
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);

    // Step 1: Send req_pq_multi.
    let req_pq = mtp_functions::ReqPqMulti { nonce }.to_bytes();
    let pkt1 = encode_unencrypted_packet(clock.next_msg_id(), &req_pq);
    conn.send(&pkt1).await?;

    // Receive resPQ.
    let resp1 = conn.recv().await?;
    let (_, body1) = decode_unencrypted_packet(&resp1)?;
    let mtp_enums::ResPQ::ResPQ(res_pq) = mtp_enums::ResPQ::from_bytes(body1)?;
    if res_pq.nonce != nonce {
        return Err(InvocationError::Protocol(
            "nonce mismatch in resPQ".to_string(),
        ));
    }

    let fingerprints: Vec<u64> = res_pq
        .server_public_key_fingerprints
        .iter()
        .map(|&fp| fp as u64)
        .collect();
    let rsa_key = dc_options
        .get_dc_rsa_key(dc_id, &fingerprints)
        .ok_or_else(|| {
            InvocationError::Protocol(format!(
                "no matching RSA public key for DC {dc_id} (server sent {} fingerprints)",
                fingerprints.len()
            ))
        })?;

    let (p, q) = factorize_bytes(&res_pq.pq)?;

    let mut new_nonce = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut new_nonce);

    let pq_inner_bytes = if expires_in == 0 {
        mtp_enums::PQInnerData::Dc(mtp_types::PQInnerDataDc {
            pq: res_pq.pq.clone(),
            p: p.clone(),
            q: q.clone(),
            nonce,
            server_nonce: res_pq.server_nonce,
            new_nonce,
            dc: protocol_dc_id as i32,
        })
        .to_bytes()
    } else {
        mtp_enums::PQInnerData::TempDc(mtp_types::PQInnerDataTempDc {
            pq: res_pq.pq.clone(),
            p: p.clone(),
            q: q.clone(),
            nonce,
            server_nonce: res_pq.server_nonce,
            new_nonce,
            dc: protocol_dc_id as i32,
            expires_in,
        })
        .to_bytes()
    };

    let encrypted_data = rsa_key.encrypt_hashed(&pq_inner_bytes)?.to_vec();

    // Step 2: Send req_DH_params.
    let req_dh = mtp_functions::ReqDHParams {
        nonce,
        server_nonce: res_pq.server_nonce,
        p,
        q,
        public_key_fingerprint: rsa_key.fingerprint() as i64,
        encrypted_data,
    }
    .to_bytes();
    let pkt2 = encode_unencrypted_packet(clock.next_msg_id(), &req_dh);
    conn.send(&pkt2).await?;

    // Receive server_DH_params.
    let resp2 = conn.recv().await?;
    let (_, body2) = decode_unencrypted_packet(&resp2)?;
    let server_dh = mtp_enums::ServerDHParams::from_bytes(body2)?;

    let encrypted_answer = match server_dh {
        mtp_enums::ServerDHParams::Ok(ok) => {
            if ok.nonce != nonce || ok.server_nonce != res_pq.server_nonce {
                return Err(InvocationError::Protocol(
                    "nonce/server_nonce mismatch in server_DH_params_ok".to_string(),
                ));
            }
            ok.encrypted_answer
        }
        mtp_enums::ServerDHParams::Fail(fail) => {
            if fail.nonce != nonce || fail.server_nonce != res_pq.server_nonce {
                return Err(InvocationError::Protocol(
                    "nonce mismatch in server_DH_params_fail".to_string(),
                ));
            }
            let expected = nonce_digest(&new_nonce);
            if !const_time_eq(&fail.new_nonce_hash, &expected) {
                return Err(InvocationError::Protocol(
                    "new_nonce_hash mismatch in server_DH_params_fail".to_string(),
                ));
            }
            return Err(InvocationError::Protocol(
                "server returned server_DH_params_fail".to_string(),
            ));
        }
    };

    if encrypted_answer.len() < 24 || !encrypted_answer.len().is_multiple_of(16) {
        return Err(InvocationError::Protocol(format!(
            "invalid encrypted_answer length: {}",
            encrypted_answer.len()
        )));
    }

    // Derive tmp_aes_key and tmp_aes_iv.
    let (tmp_aes_key, tmp_aes_iv) = derive_tmp_aes_key_iv(&new_nonce, &res_pq.server_nonce);
    let decrypted = try_aes_ige_decrypt(&encrypted_answer, &tmp_aes_key, &tmp_aes_iv)?;
    if decrypted.len() < 24 {
        return Err(InvocationError::Protocol(
            "decrypted server_DH_inner_data too short".to_string(),
        ));
    }

    let mut cur = Cursor::new(&decrypted[20..]);
    let mtp_enums::ServerDHInnerData::ServerDHInnerData(dh_inner) =
        mtp_enums::ServerDHInnerData::deserialize(&mut cur)?;
    let consumed = cur.position();
    let expected_sha = sha1(&[&decrypted[20..20 + consumed]]);
    if !const_time_eq(&decrypted[0..20], &expected_sha) {
        return Err(InvocationError::Protocol(
            "SHA-1 mismatch in server_DH_inner_data".to_string(),
        ));
    }
    if dh_inner.nonce != nonce || dh_inner.server_nonce != res_pq.server_nonce {
        return Err(InvocationError::Protocol(
            "nonce mismatch in server_DH_inner_data".to_string(),
        ));
    }

    clock.update_server_time(dh_inner.server_time, false);

    if !dh::is_prime_and_good(&dh_inner.dh_prime, dh_inner.g) {
        return Err(InvocationError::Protocol(
            "server sent invalid DH prime or generator".to_string(),
        ));
    }

    // Step 3: Compute g_b and auth_key, send set_client_DH_params (with up to 5 retries).
    let mut retry_id: i64 = 0;
    for attempt in 1..=MAX_DH_RETRIES {
        let mut random_seed = [0u8; 256];
        rand::thread_rng().fill_bytes(&mut random_seed);
        let mod_exp = dh::create_mod_exp(dh_inner.g, &dh_inner.dh_prime, &random_seed)?;
        let computed =
            dh::create_auth_key(&dh_inner.g_a, &mod_exp.random_power, &dh_inner.dh_prime)?;
        let auth_key = AuthKey::from_bytes(&computed)?;
        let aux_hash = auth_key.aux_hash();

        let client_dh_inner =
            mtp_enums::ClientDHInnerData::ClientDHInnerData(mtp_types::ClientDHInnerData {
                nonce,
                server_nonce: res_pq.server_nonce,
                retry_id,
                g_b: mod_exp.modexp.clone(),
            })
            .to_bytes();

        let encrypted_client_dh =
            encrypt_client_dh_inner(&client_dh_inner, &tmp_aes_key, &tmp_aes_iv)?;
        let set_client = mtp_functions::SetClientDHParams {
            nonce,
            server_nonce: res_pq.server_nonce,
            encrypted_data: encrypted_client_dh,
        }
        .to_bytes();
        let pkt3 = encode_unencrypted_packet(clock.next_msg_id(), &set_client);
        conn.send(&pkt3).await?;

        let resp3 = conn.recv().await?;
        let (_, body3) = decode_unencrypted_packet(&resp3)?;
        let answer = mtp_enums::SetClientDHParamsAnswer::from_bytes(body3)?;

        match answer {
            mtp_enums::SetClientDHParamsAnswer::DhGenOk(ok) => {
                if ok.nonce != nonce || ok.server_nonce != res_pq.server_nonce {
                    return Err(InvocationError::Protocol(
                        "nonce mismatch in dh_gen_ok".to_string(),
                    ));
                }
                let expected = dh::dh_new_nonce_hash(&new_nonce, 1, &aux_hash);
                if !const_time_eq(&ok.new_nonce_hash1, &expected) {
                    return Err(InvocationError::Protocol(
                        "new_nonce_hash1 mismatch in dh_gen_ok".to_string(),
                    ));
                }
                let salt1 = u64::from_le_bytes(new_nonce[0..8].try_into().unwrap_or([0; 8]));
                let salt2 =
                    u64::from_le_bytes(res_pq.server_nonce[0..8].try_into().unwrap_or([0; 8]));
                let initial_salt = salt1 ^ salt2;
                let key_type = if expires_in == 0 {
                    AuthKeyType::Generated
                } else {
                    AuthKeyType::Temporary
                };
                let mut managed = ManagedAuthKey::new(auth_key, key_type, dc_id);
                if expires_in > 0 {
                    managed.expires_at = Some(
                        clock.now_secs() + expires_in + BIND_KEY_ADDITIONAL_EXPIRES_TIMEOUT_SECS,
                    );
                }
                debug!(
                    dc_id,
                    expires_in,
                    key_id = format_args!("0x{:016x}", managed.key_id()),
                    "created MTProto auth key"
                );
                return Ok((managed, initial_salt));
            }
            mtp_enums::SetClientDHParamsAnswer::DhGenRetry(retry) => {
                if retry.nonce != nonce || retry.server_nonce != res_pq.server_nonce {
                    return Err(InvocationError::Protocol(
                        "nonce mismatch in dh_gen_retry".to_string(),
                    ));
                }
                let expected = dh::dh_new_nonce_hash(&new_nonce, 2, &aux_hash);
                if !const_time_eq(&retry.new_nonce_hash2, &expected) {
                    return Err(InvocationError::Protocol(
                        "new_nonce_hash2 mismatch in dh_gen_retry".to_string(),
                    ));
                }
                warn!(dc_id, attempt, "server returned dh_gen_retry");
                retry_id = i64::from_le_bytes(aux_hash);
            }
            mtp_enums::SetClientDHParamsAnswer::DhGenFail(fail) => {
                if fail.nonce != nonce || fail.server_nonce != res_pq.server_nonce {
                    return Err(InvocationError::Protocol(
                        "nonce mismatch in dh_gen_fail".to_string(),
                    ));
                }
                let expected = dh::dh_new_nonce_hash(&new_nonce, 3, &aux_hash);
                if !const_time_eq(&fail.new_nonce_hash3, &expected) {
                    return Err(InvocationError::Protocol(
                        "new_nonce_hash3 mismatch in dh_gen_fail".to_string(),
                    ));
                }
                return Err(InvocationError::Protocol(
                    "server returned dh_gen_fail".to_string(),
                ));
            }
        }
    }

    Err(InvocationError::Protocol(format!(
        "auth key creation exceeded {MAX_DH_RETRIES} dh_gen_retry attempts"
    )))
}

/// Runs persistent and/or temporary key creation sequentially on `conn` (`DcKeyCreator::done`).
pub async fn create_dc_keys(
    conn: &mut dyn PacketConnection,
    dc_id: DcId,
    protocol_dc_id: i16,
    dc_options: &DcOptions,
    clock: &MtprotoClock,
    request: DcKeyRequest,
) -> Result<DcKeyResult, InvocationError> {
    let mut result = DcKeyResult {
        persistent_key: None,
        persistent_server_salt: 0,
        temporary_key: None,
        temporary_server_salt: 0,
    };
    if request.persistent_needed {
        let (key, salt) =
            create_single_auth_key(conn, dc_id, protocol_dc_id, dc_options, clock, 0).await?;
        result.persistent_key = Some(key);
        result.persistent_server_salt = salt;
    }
    if request.temporary_expires_in > 0 {
        let (key, salt) = create_single_auth_key(
            conn,
            dc_id,
            protocol_dc_id,
            dc_options,
            clock,
            request.temporary_expires_in,
        )
        .await?;
        result.temporary_key = Some(key);
        result.temporary_server_salt = salt;
    }
    Ok(result)
}

/// Derives `(tmp_aes_key, tmp_aes_iv)` from `(new_nonce, server_nonce)`.
pub fn derive_tmp_aes_key_iv(
    new_nonce: &[u8; 32],
    server_nonce: &[u8; 16],
) -> ([u8; 32], [u8; 32]) {
    dh::dh_server_aes_key_iv(new_nonce, server_nonce)
}

fn encrypt_client_dh_inner(
    inner: &[u8],
    aes_key: &[u8; 32],
    aes_iv: &[u8; 32],
) -> Result<Vec<u8>, InvocationError> {
    let hash = sha1(&[inner]);
    let unpadded = 20 + inner.len();
    let rem = unpadded % 16;
    let padded_len = if rem == 0 {
        unpadded
    } else {
        unpadded + (16 - rem)
    };
    let mut buf = vec![0u8; padded_len];
    buf[0..20].copy_from_slice(&hash);
    buf[20..unpadded].copy_from_slice(inner);
    if padded_len > unpadded {
        rand::thread_rng().fill_bytes(&mut buf[unpadded..]);
    }
    Ok(try_aes_ige_encrypt(&buf, aes_key, aes_iv)?)
}

fn nonce_digest(data: &[u8]) -> [u8; 16] {
    let h = sha1(&[data]);
    let mut out = [0u8; 16];
    out.copy_from_slice(&h[4..20]);
    out
}

// ============================================================================
// PFS Temporary Key Binder (`mtproto_dc_key_binder.cpp`)
// ============================================================================

/// Outcome of handling `auth.bindTempAuthKey` response (`DcKeyBindState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DcKeyBindState {
    /// Binding succeeded (`boolTrue`).
    Success,
    /// Server reported `400 ENCRYPTED_MESSAGE_INVALID`: persistent key is forgotten on server.
    DefinitelyDestroyed,
    /// Temporary / other failure; retry binding.
    Failed,
}

/// Prepares `auth.bindTempAuthKey` payload binding `temporary_key` to `persistent_key`
/// (`DcKeyBinder::prepareRequest`).
///
/// The returned `(msg_id, serialized_body)` MUST be sent with `msg_id` as the outer message ID.
pub fn prepare_bind_temp_key_request(
    persistent_key: &ManagedAuthKey,
    temporary_key: &ManagedAuthKey,
    session_id: u64,
    clock: &MtprotoClock,
) -> Result<(u64, Vec<u8>), InvocationError> {
    let expires_at = temporary_key.expires_at.ok_or_else(|| {
        InvocationError::Protocol("temporary key has no expires_at timestamp".to_string())
    })?;
    let mut nonce_bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = i64::from_le_bytes(nonce_bytes);
    let msg_id = clock.next_msg_id();

    let inner = mtp_enums::BindAuthKeyInner::BindAuthKeyInner(mtp_types::BindAuthKeyInner {
        nonce,
        temp_auth_key_id: temporary_key.key_id() as i64,
        perm_auth_key_id: persistent_key.key_id() as i64,
        temp_session_id: session_id as i64,
        expires_at,
    })
    .to_bytes();

    // Inner envelope: random salt (8) + random session_id (8) + real msg_id (8) + seq_no=0 (4) + len (4) + body.
    let mut plain = vec![0u8; 32 + inner.len()];
    rand::thread_rng().fill_bytes(&mut plain[0..16]);
    plain[16..24].copy_from_slice(&msg_id.to_le_bytes());
    plain[24..28].copy_from_slice(&0u32.to_le_bytes());
    plain[28..32].copy_from_slice(&(inner.len() as u32).to_le_bytes());
    plain[32..].copy_from_slice(&inner);

    let encrypted_message = encrypt_data_v1(&plain, &persistent_key.key, Side::Client);
    let req = BindTempAuthKey {
        perm_auth_key_id: persistent_key.key_id() as i64,
        nonce,
        expires_at,
        encrypted_message,
    }
    .to_bytes();

    Ok((msg_id, req))
}

/// Evaluates the RPC result body of `auth.bindTempAuthKey` (`DcKeyBinder::handleResponse`).
pub fn handle_bind_temp_key_response(response_body: &[u8]) -> DcKeyBindState {
    if response_body.len() < 4 {
        return DcKeyBindState::Failed;
    }
    let ctor = u32::from_le_bytes([
        response_body[0],
        response_body[1],
        response_body[2],
        response_body[3],
    ]);
    if ctor == BOOL_TRUE_ID {
        return DcKeyBindState::Success;
    }
    if ctor == mtp_types::RpcError::CONSTRUCTOR_ID
        && let Ok(mtp_enums::RpcError::RpcError(err)) =
            mtp_enums::RpcError::from_bytes(response_body)
        && err.error_code == 400
        && err.error_message == "ENCRYPTED_MESSAGE_INVALID"
    {
        return DcKeyBindState::DefinitelyDestroyed;
    }
    DcKeyBindState::Failed
}

// ============================================================================
// Per-DC Key Registry (`mtproto_dcenter.cpp`)
// ============================================================================

/// Slot index for temporary keys within a [`Dcenter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporaryKeyType {
    /// Regular DC temporary key.
    Regular,
    /// Media cluster temporary key.
    MediaCluster,
}

impl TemporaryKeyType {
    /// Maps a [`DcType`] to its temporary key slot (`TemporaryKeyTypeByDcType`).
    pub const fn from_dc_type(dc_type: DcType) -> Self {
        match dc_type {
            DcType::MediaCluster => Self::MediaCluster,
            _ => Self::Regular,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Regular => 0,
            Self::MediaCluster => 1,
        }
    }
}

/// Key creation lock ownership (`MTP::details::CreatingKeyType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatingKeyType {
    /// Another session is already creating the key, or a key already exists.
    None,
    /// Caller owns creating the persistent key (and regular temp key).
    Persistent,
    /// Caller owns creating the regular temporary key.
    TemporaryRegular,
    /// Caller owns creating the media-cluster temporary key.
    TemporaryMediaCluster,
}

#[derive(Debug)]
struct DcenterInner {
    dc_id: DcId,
    persistent_key: Option<ManagedAuthKey>,
    temporary_keys: [Option<ManagedAuthKey>; 2],
    creating: [bool; 2],
    connection_inited: bool,
    reinit_generation: u64,
}

/// Thread-safe per-DC auth key and connection-init state (`MTP::details::Dcenter`).
#[derive(Debug, Clone)]
pub struct Dcenter {
    inner: Arc<RwLock<DcenterInner>>,
    /// Bumped whenever a key-creation slot is released, so sessions waiting for another
    /// session's key creation wake up instead of polling (`Dcenter` key-changed signal).
    key_creation_released: Arc<watch::Sender<u64>>,
}

impl Dcenter {
    /// Creates a new `Dcenter` with an optional pre-existing persistent key.
    pub fn new(dc_id: DcId, persistent_key: Option<ManagedAuthKey>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(DcenterInner {
                dc_id,
                persistent_key,
                temporary_keys: [None, None],
                creating: [false, false],
                connection_inited: false,
                reinit_generation: 0,
            })),
            key_creation_released: Arc::new(watch::Sender::new(0)),
        }
    }

    /// Subscribes to key-creation releases (success, failure or a key wipe). Subscribe before
    /// checking the keys so a release between the check and the wait is not missed.
    pub fn subscribe_key_creation(&self) -> watch::Receiver<u64> {
        self.key_creation_released.subscribe()
    }

    fn notify_key_creation_released(&self) {
        self.key_creation_released
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Bare DC ID.
    pub fn id(&self) -> DcId {
        self.inner.read().dc_id
    }

    /// Returns the persistent key, if any.
    pub fn persistent_key(&self) -> Option<ManagedAuthKey> {
        self.inner.read().persistent_key.clone()
    }

    /// Sets the persistent key directly (e.g. when importing saved keys or skipping PFS).
    pub fn set_persistent_key(&self, key: Option<ManagedAuthKey>) {
        let mut guard = self.inner.write();
        guard.persistent_key = key;
        guard.connection_inited = false;
    }

    /// Returns the temporary key for `key_type`, if present and not expired.
    pub fn temporary_key(&self, key_type: TemporaryKeyType) -> Option<ManagedAuthKey> {
        self.inner.read().temporary_keys[key_type.index()].clone()
    }

    /// Returns whether `initConnection` has succeeded on this DC.
    pub fn connection_inited(&self) -> bool {
        self.inner.read().connection_inited
    }

    /// Marks this DC as having completed (or needing) `initConnection`.
    pub fn set_connection_inited(&self, inited: bool) {
        self.inner.write().connection_inited = inited;
    }

    /// Re-initialization generation (`SessionData::reInitGeneration`, tdesktop 3f11c74).
    pub fn reinit_generation(&self) -> u64 {
        self.inner.read().reinit_generation
    }

    /// Starts a new re-initialization (`advanceReInitGeneration`) and clears
    /// `connection_inited`: answers to requests sent on connections opened before this call
    /// no longer mark the DC as inited.
    pub fn begin_reinit(&self) {
        let mut guard = self.inner.write();
        guard.reinit_generation = guard.reinit_generation.wrapping_add(1);
        guard.connection_inited = false;
    }

    /// Marks the DC inited after a good answer on a connection opened at `generation`, unless a
    /// re-initialization started since (`notifyConnectionInited` with 3f11c74's check).
    pub fn notify_connection_inited(&self, generation: u64) -> bool {
        let mut guard = self.inner.write();
        if guard.reinit_generation != generation {
            return false;
        }
        guard.connection_inited = true;
        true
    }

    /// Destroys a matching temporary key (called on `-404` or `AUTH_KEY_PERM_EMPTY`).
    pub fn destroy_temporary_key(&self, key_id: u64) -> bool {
        let mut guard = self.inner.write();
        for slot in &mut guard.temporary_keys {
            if let Some(k) = slot
                && k.key_id() == key_id
            {
                *slot = None;
                guard.connection_inited = false;
                return true;
            }
        }
        false
    }

    /// Destroys a confirmed-forgotten persistent key and all temporary keys bound to it.
    pub fn destroy_confirmed_forgotten_key(&self, key_id: u64) -> bool {
        let mut guard = self.inner.write();
        match &guard.persistent_key {
            Some(k) if k.key_id() == key_id => {
                guard.persistent_key = None;
                guard.temporary_keys = [None, None];
                guard.connection_inited = false;
                true
            }
            _ => false,
        }
    }

    /// Destroys all persistent and temporary keys for this DC and clears `connection_inited`
    /// (used when the server rejects a duplicated key with `AUTH_KEY_DUPLICATED` so a fresh
    /// DH key exchange runs on reconnect, mirroring `Dcenter::destroyConfirmedForgottenKey` in
    /// `mtproto/details/mtproto_dcenter.cpp`).
    pub fn destroy_all_keys(&self) {
        let mut guard = self.inner.write();
        guard.persistent_key = None;
        guard.temporary_keys = [None, None];
        guard.creating = [false, false];
        guard.connection_inited = false;
        drop(guard);
        self.notify_key_creation_released();
    }

    /// Attempts to claim single-flight key creation for `dc_type` (`Dcenter::acquireKeyCreation`).
    pub fn acquire_key_creation(&self, dc_type: DcType) -> CreatingKeyType {
        let mut guard = self.inner.write();
        let key_type = TemporaryKeyType::from_dc_type(dc_type);
        let idx = key_type.index();
        if guard.temporary_keys[idx].is_some() {
            return CreatingKeyType::None;
        }
        let reg_idx = TemporaryKeyType::Regular.index();
        if key_type == TemporaryKeyType::MediaCluster && guard.temporary_keys[reg_idx].is_some() {
            if guard.creating[idx] {
                return CreatingKeyType::None;
            }
            guard.creating[idx] = true;
            return CreatingKeyType::TemporaryMediaCluster;
        }
        if guard.creating[reg_idx] {
            return CreatingKeyType::None;
        }
        guard.creating[reg_idx] = true;
        if dc_type != DcType::Cdn && guard.persistent_key.is_none() {
            CreatingKeyType::Persistent
        } else {
            CreatingKeyType::TemporaryRegular
        }
    }

    /// Releases key creation on success, storing the bound temporary key and (if created)
    /// persistent key (`Dcenter::releaseKeyCreationOnDone`).
    pub fn release_key_creation_on_done(
        &self,
        creating: CreatingKeyType,
        temporary_key: ManagedAuthKey,
        persistent_key_used_for_bind: Option<ManagedAuthKey>,
    ) -> bool {
        let stored = self.store_created_key(creating, temporary_key, persistent_key_used_for_bind);
        if creating != CreatingKeyType::None {
            self.notify_key_creation_released();
        }
        stored
    }

    fn store_created_key(
        &self,
        creating: CreatingKeyType,
        temporary_key: ManagedAuthKey,
        persistent_key_used_for_bind: Option<ManagedAuthKey>,
    ) -> bool {
        let mut guard = self.inner.write();
        let idx = match creating {
            CreatingKeyType::Persistent | CreatingKeyType::TemporaryRegular => 0,
            CreatingKeyType::TemporaryMediaCluster => 1,
            CreatingKeyType::None => return false,
        };
        if creating == CreatingKeyType::Persistent {
            guard.persistent_key = persistent_key_used_for_bind;
        } else if let Some(used) = persistent_key_used_for_bind {
            let current_id = guard.persistent_key.as_ref().map(ManagedAuthKey::key_id);
            if current_id != Some(used.key_id()) {
                guard.creating[idx] = false;
                return false;
            }
        }
        guard.temporary_keys[idx] = Some(temporary_key);
        guard.creating[idx] = false;
        guard.connection_inited = false;
        true
    }

    /// Releases key creation slot after failure (`Dcenter::releaseKeyCreationOnFail`).
    pub fn release_key_creation_on_fail(&self, creating: CreatingKeyType) {
        let slot = match creating {
            CreatingKeyType::Persistent | CreatingKeyType::TemporaryRegular => 0,
            CreatingKeyType::TemporaryMediaCluster => 1,
            CreatingKeyType::None => return,
        };
        self.inner.write().creating[slot] = false;
        self.notify_key_creation_released();
    }
}
