//! Real-network smoke test harness against Telegram's **official TEST data centers only**.
//!
//! Safety guarantees:
//! - Refuses any non-test DC address or any `help.getConfig` response without `test_mode = true`.
//! - Never embeds API credentials: reads `RUSTGRAM_API_ID` / `RUSTGRAM_API_HASH` (or legacy
//!   `TGFORK_API_ID` / `TGFORK_API_HASH`) from the environment, or parses `docs/api_credentials.md`
//!   from `TDESKTOP_DIR` / `RUSTGRAM_API_CREDS_FILE` at runtime without printing them.
//! - Uses only throwaway Telegram test phone numbers `99966XYYYY` (`X` in `1..=3`) with fake names
//!   `"Rustgram Test A"` / `"Rustgram Test B"`, logs out both accounts, and deletes temp `tdata`.
//!
//! Run:
//!   cargo run -p tg-session --example test_dc_smoke

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use tg_api::convert::{
    apply_tl_users_and_chats, input_channel_for, input_peer_for, input_user_for,
};
use tg_api::{
    ApiCredentials, ApiError, AppConfigSync, AuthorizationsManager, ChannelParticipantsFilter,
    ContactsManager, DeleteHistoryOutcome, EmojiStatusesSync, ForwardItem, ForwardOptions,
    InvocationError, LoginErrorKind, LoginStep, MessageSender, ReactionsSync, RpcClient,
    SendOptions, SendProgressKind, StickersSyncManager, StickersType,
};
use tg_core::domain::Environment as CoreEnvironment;
use tg_crypto::sha256;
use tg_data::{
    ChannelId, ChatId, EntityKind, FullMsgId, MessageMedia, MsgId, PeerId, Session, TypingAction,
    UserId,
};
use tg_media::error::MediaError;
use tg_media::transfer::download::{CancelToken, DownloadRequest, download_file};
use tg_media::transfer::upload::{UploadConfig, UploadedInputFile, upload_bytes};
use tg_media::transfer::{
    BoxFuture as MediaBoxFuture, CdnFileHash, CdnRedirect, FileOrigin as MediaFileOrigin,
    GetCdnFileRequest, GetCdnFileResponse, GetFileRequest, GetFileResponse, GetWebFileRequest,
    GetWebFileResponse, MediaInvoker, SaveBigFilePartRequest, SaveFilePartRequest,
    TransferLocation, validate_get_cdn_file_request, validate_get_file_request,
};
use tg_mtproto::dc_options::{
    DcId, DcOptions, DcType, Environment as MtpEnvironment, download_dc_id, upload_dc_id,
};
use tg_mtproto::transport::{Error as TransportError, ProxyData};
use tg_mtproto::transport_adapter::BoxFuture as MtpBoxFuture;
use tg_mtproto::{
    ClientConfig, PacketConnection, TgTransportFactory, TransportFactory, build_race_candidates,
};
use tg_session::{
    AccountManager, LoginFlow, MtprotoRpc, SessionRuntime, SessionRuntimeConfig,
    WorkingDirOpenOptions, load_api_credentials_from_env,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::sync::Notify;

/// Explicit denylist of Telegram production DC IPs that must NEVER be dialed by this harness.
const PRODUCTION_DC_IPS: &[&str] = &[
    "149.154.175.50",
    "149.154.167.51",
    "149.154.167.50",
    "95.161.76.100",
    "149.154.175.100",
    "149.154.167.91",
    "149.154.167.92",
    "149.154.171.5",
    "2001:0b28:f23d:f001:0000:0000:0000:000a",
    "2001:067c:04e8:f002:0000:0000:0000:000a",
    "2001:0b28:f23d:f003:0000:0000:0000:000a",
    "2001:067c:04e8:f004:0000:0000:0000:000a",
    "2001:0b28:f23f:f005:0000:0000:0000:000a",
];

/// Official Telegram built-in test DC endpoints (`mtproto_dc_options.cpp::kBuiltInDcsTest`).
const INITIAL_TEST_DC_IPS: &[&str] = &[
    "149.154.175.10",
    "149.154.167.40",
    "149.154.175.117",
    "2001:0b28:f23d:f001:0000:0000:0000:000e",
    "2001:067c:04e8:f002:0000:0000:0000:000e",
    "2001:0b28:f23d:f003:0000:0000:0000:000e",
];

const MAX_FLOOD_WAIT_SECS: u32 = 60;
const STEP_WAIT_TIMEOUT: Duration = Duration::from_secs(25);

fn is_production_ip(ip: &str) -> bool {
    let trimmed = ip.trim();
    PRODUCTION_DC_IPS
        .iter()
        .any(|p| p.eq_ignore_ascii_case(trimmed))
}

fn assert_test_phone(phone: &str) {
    let bytes = phone.as_bytes();
    assert!(
        bytes.len() == 10
            && bytes.starts_with(b"99966")
            && (b'1'..=b'3').contains(&bytes[5])
            && bytes[6..].iter().all(u8::is_ascii_digit),
        "SAFETY VIOLATION: refusing non-test phone number"
    );
}

/// Loads Telegram Desktop's public TEST-ONLY credentials at runtime from env vars or
/// `<TDESKTOP_DIR>/docs/api_credentials.md` (or `RUSTGRAM_API_CREDS_FILE`). Never prints them.
fn load_test_api_credentials() -> Result<ApiCredentials, String> {
    if let Ok(creds) = load_api_credentials_from_env() {
        return Ok(creds);
    }
    let path = std::env::var("RUSTGRAM_API_CREDS_FILE")
        .or_else(|_| std::env::var("TGFORK_API_CREDS_FILE"))
        .or_else(|_| {
            std::env::var("TDESKTOP_DIR").map(|dir| format!("{dir}/docs/api_credentials.md"))
        })
        .map_err(|_| {
            "set RUSTGRAM_API_ID/RUSTGRAM_API_HASH, RUSTGRAM_API_CREDS_FILE, or TDESKTOP_DIR"
                .to_owned()
        })?;
    let text = std::fs::read_to_string(Path::new(&path))
        .map_err(|e| format!("failed to read test credentials doc at {path}: {e}"))?;
    let mut api_id: Option<i32> = None;
    let mut api_hash: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("**api_id**:") {
            api_id = rest.trim().parse::<i32>().ok().filter(|&v| v > 0);
        } else if let Some(rest) = trimmed.strip_prefix("**api_hash**:") {
            let h = rest.trim().to_owned();
            if !h.is_empty() {
                api_hash = Some(h);
            }
        }
    }
    match (api_id, api_hash) {
        (Some(api_id), Some(api_hash)) => Ok(ApiCredentials { api_id, api_hash }),
        _ => Err(format!(
            "could not parse test api_id/api_hash from {path} (and RUSTGRAM_API_ID/RUSTGRAM_API_HASH unset)"
        )),
    }
}

/// Wrapping [`PacketConnection`] that can be abruptly dropped on demand to simulate a real TCP
/// disconnect mid-session.
struct DroppablePacketConnection {
    inner: Option<Box<dyn PacketConnection>>,
    drop_gen: u64,
    current_gen: Arc<AtomicU64>,
    drop_notify: Arc<Notify>,
    tag: String,
}

impl DroppablePacketConnection {
    fn is_dropped(&self) -> bool {
        self.inner.is_none() || self.current_gen.load(Ordering::Acquire) != self.drop_gen
    }
}

impl PacketConnection for DroppablePacketConnection {
    fn send<'a>(&'a mut self, payload: &'a [u8]) -> MtpBoxFuture<'a, Result<(), TransportError>> {
        Box::pin(async move {
            if self.is_dropped() {
                self.inner = None;
                return Err(TransportError::Disconnected);
            }
            let Some(inner) = self.inner.as_mut() else {
                return Err(TransportError::Disconnected);
            };
            let notified = self.drop_notify.notified();
            tokio::select! {
                res = inner.send(payload) => res,
                _ = notified => {
                    self.inner = None;
                    Err(TransportError::Disconnected)
                }
            }
        })
    }

    fn recv(&mut self) -> MtpBoxFuture<'_, Result<Vec<u8>, TransportError>> {
        Box::pin(async move {
            if self.is_dropped() {
                self.inner = None;
                return Err(TransportError::Disconnected);
            }
            let Some(inner) = self.inner.as_mut() else {
                return Err(TransportError::Disconnected);
            };
            let notified = self.drop_notify.notified();
            tokio::select! {
                res = inner.recv() => res,
                _ = notified => {
                    self.inner = None;
                    Err(TransportError::Disconnected)
                }
            }
        })
    }

    fn close(&mut self) -> MtpBoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(mut inner) = self.inner.take() {
                inner.close().await;
            }
        })
    }

    fn using_http_wait(&self) -> bool {
        self.inner.as_ref().is_some_and(|c| c.using_http_wait())
    }

    fn need_http_wait(&self) -> bool {
        self.inner.as_ref().is_some_and(|c| c.need_http_wait())
    }

    fn transport_tag(&self) -> &str {
        &self.tag
    }
}

/// Transport factory wrapper enforcing that EVERY dialed address is an official Telegram test DC
/// and providing a real TCP connection drop trigger for reconnect testing.
struct TestDcGuardTransportFactory {
    inner: TgTransportFactory,
    allowed_ips: RwLock<HashSet<String>>,
    allowed_endpoints: RwLock<HashSet<(String, u16)>>,
    drop_gen: Arc<AtomicU64>,
    drop_notify: Arc<Notify>,
    connect_count: AtomicUsize,
    block_connects: AtomicBool,
}

impl TestDcGuardTransportFactory {
    fn new() -> Self {
        let mut allowed_ips = HashSet::new();
        let mut allowed_endpoints = HashSet::new();
        for &ip in INITIAL_TEST_DC_IPS {
            allowed_ips.insert(ip.to_owned());
            allowed_endpoints.insert((ip.to_owned(), 443));
            allowed_endpoints.insert((ip.to_owned(), 80));
        }
        Self {
            inner: TgTransportFactory::default(),
            allowed_ips: RwLock::new(allowed_ips),
            allowed_endpoints: RwLock::new(allowed_endpoints),
            drop_gen: Arc::new(AtomicU64::new(0)),
            drop_notify: Arc::new(Notify::new()),
            connect_count: AtomicUsize::new(0),
            block_connects: AtomicBool::new(false),
        }
    }

    fn validate_and_register_config(&self, cfg: &tl_types::Config) {
        assert!(
            cfg.test_mode,
            "SAFETY VIOLATION: help.getConfig returned test_mode=false! Aborting immediately."
        );
        let mut ips = self.allowed_ips.write();
        let mut eps = self.allowed_endpoints.write();
        for opt_enum in &cfg.dc_options {
            let tl_enums::DcOption::DcOption(opt) = opt_enum;
            assert!(
                !is_production_ip(&opt.ip_address),
                "SAFETY VIOLATION: help.getConfig contained production DC IP {}!",
                opt.ip_address
            );
            if opt.port > 0 && opt.port <= i32::from(u16::MAX) {
                ips.insert(opt.ip_address.clone());
                eps.insert((opt.ip_address.clone(), opt.port as u16));
            }
        }
    }

    fn assert_endpoint_allowed(&self, ip: &str, port: u16) {
        assert!(
            !is_production_ip(ip),
            "SAFETY VIOLATION: attempted connection to production DC IP {ip}:{port}!"
        );
        let ips = self.allowed_ips.read();
        let eps = self.allowed_endpoints.read();
        assert!(
            ips.contains(ip) && (port == 443 || port == 80 || eps.contains(&(ip.to_owned(), port))),
            "SAFETY VIOLATION: attempted connection to unapproved endpoint {ip}:{port}!"
        );
    }

    fn force_drop_active_connections(&self) {
        self.drop_gen.fetch_add(1, Ordering::AcqRel);
        self.drop_notify.notify_waiters();
    }

    fn set_block_connects(&self, blocked: bool) {
        self.block_connects.store(blocked, Ordering::Release);
    }

    fn connect_count(&self) -> usize {
        self.connect_count.load(Ordering::Acquire)
    }

    fn wrap_conn(&self, conn: Box<dyn PacketConnection>) -> Box<dyn PacketConnection> {
        let tag = conn.transport_tag().to_owned();
        let drop_gen = self.drop_gen.load(Ordering::Acquire);
        self.connect_count.fetch_add(1, Ordering::AcqRel);
        Box::new(DroppablePacketConnection {
            inner: Some(conn),
            drop_gen,
            current_gen: Arc::clone(&self.drop_gen),
            drop_notify: Arc::clone(&self.drop_notify),
            tag,
        })
    }
}

impl TransportFactory for TestDcGuardTransportFactory {
    fn connect<'a>(
        &'a self,
        ip: &'a str,
        port: u16,
        protocol_dc_id: i16,
        secret: &'a [u8],
    ) -> MtpBoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        Box::pin(async move {
            self.assert_endpoint_allowed(ip, port);
            assert!(
                protocol_dc_id.abs() >= 10_000,
                "SAFETY VIOLATION: non-test protocol_dc_id {protocol_dc_id}"
            );
            if self.block_connects.load(Ordering::Acquire) {
                return Err(TransportError::Disconnected);
            }
            let conn = self.inner.connect(ip, port, protocol_dc_id, secret).await?;
            Ok(self.wrap_conn(conn))
        })
    }

    fn connect_dc<'a>(
        &'a self,
        bare_dc: DcId,
        dc_type: DcType,
        dc_options: &'a DcOptions,
        config: &'a ClientConfig,
        proxy: &'a ProxyData,
    ) -> MtpBoxFuture<'a, Result<Box<dyn PacketConnection>, TransportError>> {
        Box::pin(async move {
            assert!(
                dc_options.is_test_mode() && config.environment == MtpEnvironment::Test,
                "SAFETY VIOLATION: connect_dc called outside test mode!"
            );
            let candidates = build_race_candidates(bare_dc, dc_type, dc_options, config, proxy);
            for cand in &candidates {
                match &cand.endpoint {
                    tg_transport::RaceEndpoint::Tcp {
                        address,
                        port,
                        config: c,
                    } => {
                        self.assert_endpoint_allowed(address, *port);
                        assert!(
                            c.dc_id.abs() >= 10_000,
                            "SAFETY VIOLATION: non-test protocol_dc_id {}",
                            c.dc_id
                        );
                    }
                    tg_transport::RaceEndpoint::Http { address, config: c } => {
                        self.assert_endpoint_allowed(address, c.port);
                    }
                }
            }
            if self.block_connects.load(Ordering::Acquire) {
                return Err(TransportError::Disconnected);
            }
            let conn = self
                .inner
                .connect_dc(bare_dc, dc_type, dc_options, config, proxy)
                .await?;
            Ok(self.wrap_conn(conn))
        })
    }
}

/// Real `tg_media::transfer::MediaInvoker` over `MtprotoRpc` using shifted upload/download DCs.
struct LiveMediaInvoker {
    rpc: MtprotoRpc,
}

impl LiveMediaInvoker {
    fn new(rpc: MtprotoRpc) -> Self {
        Self { rpc }
    }
}

fn map_inv_err(err: InvocationError) -> MediaError {
    match err {
        InvocationError::Rpc(r) => MediaError::Rpc {
            code: r.code,
            message: r.name,
        },
        other => MediaError::Rpc {
            code: 500,
            message: other.to_string(),
        },
    }
}

fn transfer_loc_to_tl(
    loc: &TransferLocation,
) -> tg_media::error::Result<tl_enums::InputFileLocation> {
    match loc {
        TransferLocation::Document {
            id,
            access_hash,
            file_reference,
            thumb_size,
        } => Ok(tl_enums::InputFileLocation::InputDocumentFileLocation(
            tl_types::InputDocumentFileLocation {
                id: *id as i64,
                access_hash: *access_hash as i64,
                file_reference: file_reference.clone(),
                thumb_size: thumb_size.clone(),
            },
        )),
        TransferLocation::Photo {
            id,
            access_hash,
            file_reference,
            thumb_size,
        } => Ok(tl_enums::InputFileLocation::InputPhotoFileLocation(
            tl_types::InputPhotoFileLocation {
                id: *id as i64,
                access_hash: *access_hash as i64,
                file_reference: file_reference.clone(),
                thumb_size: thumb_size.clone(),
            },
        )),
        other => Err(MediaError::InvalidTransferParam(format!(
            "unsupported location in smoke test: {other:?}"
        ))),
    }
}

fn map_tl_hashes(hashes: Vec<tl_enums::FileHash>) -> Vec<CdnFileHash> {
    hashes
        .into_iter()
        .map(|h| {
            let tl_enums::FileHash::FileHash(fh) = h;
            CdnFileHash {
                offset: fh.offset as u64,
                limit: fh.limit as u32,
                hash: fh.hash,
            }
        })
        .collect()
}

impl MediaInvoker for LiveMediaInvoker {
    fn get_file(
        &self,
        dc_id: i32,
        session_index: usize,
        req: GetFileRequest,
    ) -> MediaBoxFuture<'_, tg_media::error::Result<GetFileResponse>> {
        Box::pin(async move {
            validate_get_file_request(req.offset, req.limit, req.precise)?;
            let shifted = download_dc_id(dc_id, session_index as i32);
            let tl_req = tl_fns::upload::GetFile {
                precise: req.precise,
                cdn_supported: req.cdn_supported,
                location: transfer_loc_to_tl(&req.location)?,
                offset: req.offset as i64,
                limit: req.limit as i32,
            };
            let resp = self
                .rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_inv_err)?;
            match resp {
                tl_enums::upload::File::File(f) => Ok(GetFileResponse::File { bytes: f.bytes }),
                tl_enums::upload::File::CdnRedirect(r) => {
                    Ok(GetFileResponse::CdnRedirect(CdnRedirect {
                        dc_id: r.dc_id,
                        file_token: r.file_token,
                        encryption_key: r.encryption_key,
                        encryption_iv: r.encryption_iv,
                        file_hashes: map_tl_hashes(r.file_hashes),
                    }))
                }
            }
        })
    }

    fn get_web_file(
        &self,
        _dc_id: i32,
        _session_index: usize,
        _req: GetWebFileRequest,
    ) -> MediaBoxFuture<'_, tg_media::error::Result<GetWebFileResponse>> {
        Box::pin(async move {
            Err(MediaError::InvalidTransferParam(
                "get_web_file unused in smoke test".into(),
            ))
        })
    }

    fn get_cdn_file(
        &self,
        cdn_dc_id: i32,
        session_index: usize,
        req: GetCdnFileRequest,
    ) -> MediaBoxFuture<'_, tg_media::error::Result<GetCdnFileResponse>> {
        Box::pin(async move {
            validate_get_cdn_file_request(req.offset, req.limit)?;
            let shifted = download_dc_id(cdn_dc_id, session_index as i32);
            let tl_req = tl_fns::upload::GetCdnFile {
                file_token: req.file_token,
                offset: req.offset as i64,
                limit: req.limit as i32,
            };
            let resp = self
                .rpc
                .invoke_in_dc(shifted, &tl_req)
                .await
                .map_err(map_inv_err)?;
            match resp {
                tl_enums::upload::CdnFile::CdnFile(f) => Ok(GetCdnFileResponse::Bytes(f.bytes)),
                tl_enums::upload::CdnFile::ReuploadNeeded(r) => {
                    Ok(GetCdnFileResponse::ReuploadNeeded {
                        request_token: r.request_token,
                    })
                }
            }
        })
    }

    fn get_cdn_file_hashes(
        &self,
        dc_id: i32,
        session_index: usize,
        file_token: Vec<u8>,
        offset: u64,
    ) -> MediaBoxFuture<'_, tg_media::error::Result<Vec<CdnFileHash>>> {
        Box::pin(async move {
            let shifted = download_dc_id(dc_id, session_index as i32);
            let hashes = self
                .rpc
                .invoke_in_dc(
                    shifted,
                    &tl_fns::upload::GetCdnFileHashes {
                        file_token,
                        offset: offset as i64,
                    },
                )
                .await
                .map_err(map_inv_err)?;
            Ok(map_tl_hashes(hashes))
        })
    }

    fn reupload_cdn_file(
        &self,
        dc_id: i32,
        session_index: usize,
        file_token: Vec<u8>,
        request_token: Vec<u8>,
    ) -> MediaBoxFuture<'_, tg_media::error::Result<Vec<CdnFileHash>>> {
        Box::pin(async move {
            let shifted = download_dc_id(dc_id, session_index as i32);
            let hashes = self
                .rpc
                .invoke_in_dc(
                    shifted,
                    &tl_fns::upload::ReuploadCdnFile {
                        file_token,
                        request_token,
                    },
                )
                .await
                .map_err(map_inv_err)?;
            Ok(map_tl_hashes(hashes))
        })
    }

    fn refresh_file_reference(
        &self,
        _origin: MediaFileOrigin,
        _location: TransferLocation,
    ) -> MediaBoxFuture<'_, tg_media::error::Result<Vec<u8>>> {
        Box::pin(async move {
            Err(MediaError::Rpc {
                code: 400,
                message: "FILE_REFERENCE_EXPIRED".into(),
            })
        })
    }

    fn save_file_part(
        &self,
        session_index: usize,
        req: SaveFilePartRequest,
    ) -> MediaBoxFuture<'_, tg_media::error::Result<bool>> {
        Box::pin(async move {
            let shifted = upload_dc_id(session_index as i32);
            self.rpc
                .invoke_in_dc(
                    shifted,
                    &tl_fns::upload::SaveFilePart {
                        file_id: req.file_id,
                        file_part: req.file_part as i32,
                        bytes: req.bytes,
                    },
                )
                .await
                .map_err(map_inv_err)
        })
    }

    fn save_big_file_part(
        &self,
        session_index: usize,
        req: SaveBigFilePartRequest,
    ) -> MediaBoxFuture<'_, tg_media::error::Result<bool>> {
        Box::pin(async move {
            let shifted = upload_dc_id(session_index as i32);
            self.rpc
                .invoke_in_dc(
                    shifted,
                    &tl_fns::upload::SaveBigFilePart {
                        file_id: req.file_id,
                        file_part: req.file_part as i32,
                        file_total_parts: req.file_total_parts as i32,
                        bytes: req.bytes,
                    },
                )
                .await
                .map_err(map_inv_err)
        })
    }
}

/// Simple deterministic PRNG for generating test phone suffixes and file payloads.
struct SimpleRng(u64);

impl SimpleRng {
    fn from_time(salt: u64) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x1234_5678_9abc_def0, |d| d.as_nanos() as u64);
        let mut s = nanos ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        if s == 0 {
            s = 0xDEAD_BEEF_CAFE_BABE;
        }
        Self(s)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_test_phone(&mut self, dc: i32) -> String {
        assert!((1..=3).contains(&dc), "test DC must be 1..=3");
        let suffix = self.next_u64() % 10_000;
        let phone = format!("99966{dc}{suffix:04}");
        assert_test_phone(&phone);
        phone
    }
}

/// Generates a valid 32x32 RGBA PNG image using uncompressed zlib stored blocks.
fn generate_test_png(width: u32, height: u32) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                if crc & 1 != 0 {
                    crc = (crc >> 1) ^ 0xEDB8_8320;
                } else {
                    crc >>= 1;
                }
            }
        }
        !crc
    }

    fn adler32(data: &[u8]) -> u32 {
        let mut s1: u32 = 1;
        let mut s2: u32 = 0;
        for &b in data {
            s1 = (s1 + u32::from(b)) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        (s2 << 16) | s1
    }

    fn push_chunk(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(tag);
        out.extend_from_slice(payload);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    let mut raw_scanlines = Vec::with_capacity((height as usize) * (1 + (width as usize) * 3));
    for y in 0..height {
        raw_scanlines.push(0u8); // Filter type 0 (None)
        for x in 0..width {
            raw_scanlines.push(((x * 7 + y * 3) & 0xFF) as u8);
            raw_scanlines.push(((x * 5 + y * 11) & 0xFF) as u8);
            raw_scanlines.push(((x * 13 + y * 2) & 0xFF) as u8);
        }
    }

    // Wrap raw scanlines in valid zlib stream with stored (uncompressed) DEFLATE blocks.
    let mut zlib = Vec::with_capacity(raw_scanlines.len() + 32);
    zlib.push(0x78);
    zlib.push(0x01);
    let mut offset = 0usize;
    while offset < raw_scanlines.len() {
        let chunk_len = (raw_scanlines.len() - offset).min(65535);
        let is_last = offset + chunk_len == raw_scanlines.len();
        zlib.push(if is_last { 0x01 } else { 0x00 });
        let len16 = chunk_len as u16;
        zlib.extend_from_slice(&len16.to_le_bytes());
        zlib.extend_from_slice(&(!len16).to_le_bytes());
        zlib.extend_from_slice(&raw_scanlines[offset..offset + chunk_len]);
        offset += chunk_len;
    }
    zlib.extend_from_slice(&adler32(&raw_scanlines).to_be_bytes());

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // color type: truecolor RGB
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    push_chunk(&mut png, b"IHDR", &ihdr);
    push_chunk(&mut png, b"IDAT", &zlib);
    push_chunk(&mut png, b"IEND", &[]);
    png
}

/// Generates a deterministic ~3 MiB payload (`3 * 1024 * 1024 + 137` bytes).
fn generate_big_payload() -> Vec<u8> {
    let size = 3 * 1024 * 1024 + 137;
    let mut buf = vec![0u8; size];
    let mut rng = SimpleRng(0x5467_666F_726B_334D);
    for chunk in buf.chunks_mut(8) {
        let v = rng.next_u64().to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&v[..n]);
    }
    buf
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha256(&[bytes]);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn extract_flood_wait_secs(err: &tg_session::SessionError) -> Option<u32> {
    match err {
        tg_session::SessionError::Api(ApiError::Login(LoginErrorKind::FloodWait { seconds })) => {
            Some(*seconds)
        }
        tg_session::SessionError::Api(ApiError::Invocation(InvocationError::Rpc(r))) => {
            r.flood_wait_seconds()
        }
        _ => None,
    }
}

/// Signs up or signs in a throwaway test account on `target_dc` (`1..=3`), guaranteeing
/// `PHONE_MIGRATE_X` when `target_dc` differs from the initial DC (2).
#[allow(clippy::too_many_arguments)]
async fn login_test_account(
    label: &str,
    first_name: &str,
    last_name: &str,
    target_dc: i32,
    manager: &mut AccountManager,
    slot: i32,
    creds: &ApiCredentials,
    rng: &mut SimpleRng,
) -> Result<(tl_types::User, String), String> {
    let code = target_dc.to_string().repeat(5);
    let rpc = manager
        .account(slot)
        .ok_or_else(|| format!("{label}: missing account slot {slot}"))?
        .rpc
        .clone();

    let mut last_err = String::new();
    for number_attempt in 0..3 {
        let phone = rng.next_test_phone(target_dc);
        assert_test_phone(&phone);

        let mut flow = LoginFlow::new(rpc.clone(), creds.clone());
        let mut send_ok = false;
        for retry in 0..3 {
            match flow.submit_phone(&phone).await {
                Ok(_) => {
                    send_ok = true;
                    break;
                }
                Err(err) => {
                    eprintln!(
                        "  [{label}] auth.sendCode attempt {number_attempt}/{retry} err: {err}"
                    );
                    if let Some(secs) = extract_flood_wait_secs(&err) {
                        if secs > MAX_FLOOD_WAIT_SECS {
                            return Err(format!(
                                "{label}: FLOOD_WAIT_{secs} (>60s) on auth.sendCode; stopping"
                            ));
                        }
                        last_err = format!("FLOOD_WAIT_{secs} on attempt {number_attempt}/{retry}");
                        tokio::time::sleep(Duration::from_secs(u64::from(secs) + 1)).await;
                        break; // pick another random YYYY
                    }
                    last_err = format!("auth.sendCode failed (retry {retry}): {err}");
                }
            }
        }
        if !send_ok {
            continue;
        }

        // Now complete code -> signIn / signUp.
        let need_other_number = loop {
            match flow.step().clone() {
                LoginStep::CodeInput(state) => {
                    eprintln!(
                        "  [{label}] CodeInput: code_length={}, hash_len={}, by_tg={}, has_email={}, has_fragment={}, next_type={:?}",
                        state.code_length,
                        state.phone_hash.len(),
                        state.code_by_telegram,
                        state.email_pattern_login.is_some(),
                        state.fragment_url.is_some(),
                        state.next_type,
                    );
                    let effective_code = if state.code_length > 0 {
                        target_dc.to_string().repeat(state.code_length as usize)
                    } else {
                        code.clone()
                    };
                    match flow.submit_code(&effective_code).await {
                        Ok(_) => {}
                        Err(err) => {
                            eprintln!(
                                "  [{label}] auth.signIn(dc={target_dc}, len={}) attempt {number_attempt} err: {err}",
                                effective_code.len()
                            );
                            if let Some(secs) = extract_flood_wait_secs(&err) {
                                if secs > MAX_FLOOD_WAIT_SECS {
                                    return Err(format!(
                                        "{label}: FLOOD_WAIT_{secs} (>60s) on auth.signIn; stopping"
                                    ));
                                }
                                last_err = format!("FLOOD_WAIT_{secs} on auth.signIn");
                                tokio::time::sleep(Duration::from_secs(u64::from(secs) + 1)).await;
                                break true;
                            }
                            last_err = format!("auth.signIn failed: {err}");
                            break true;
                        }
                    }
                }
                LoginStep::SignUp { .. } => {
                    match flow.submit_sign_up(first_name, last_name).await {
                        Ok(_) => {}
                        Err(err) => {
                            eprintln!(
                                "  [{label}] auth.signUp attempt {number_attempt} err: {err}"
                            );
                            if let Some(secs) = extract_flood_wait_secs(&err)
                                && secs > MAX_FLOOD_WAIT_SECS
                            {
                                return Err(format!(
                                    "{label}: FLOOD_WAIT_{secs} (>60s) on auth.signUp; stopping"
                                ));
                            }
                            last_err = format!("auth.signUp failed: {err}");
                            break true;
                        }
                    }
                }
                LoginStep::PasswordCheck { .. } => {
                    eprintln!(
                        "  [{label}] attempt {number_attempt}: 2FA enabled on this random test number, switching..."
                    );
                    last_err = "test number has 2FA enabled; switching to fresh test number".into();
                    break true;
                }
                LoginStep::EmailSetup { .. } | LoginStep::EmailVerify { .. } => {
                    eprintln!(
                        "  [{label}] attempt {number_attempt}: email setup required on this random test number, switching..."
                    );
                    last_err =
                        "test number requires email setup; switching to fresh test number".into();
                    break true;
                }
                LoginStep::Authorized { user, .. } => {
                    let _ = manager
                        .complete_login(slot, &user)
                        .map_err(|e| format!("{label}: complete_login failed: {e}"))?;
                    println!(
                        "  [{label}] Logged in on test DC{} (user_id={})",
                        rpc.main_dc_id(),
                        user.id
                    );
                    return Ok((*user, phone));
                }
                other => {
                    last_err = format!("unexpected login step {other:?}");
                    break true;
                }
            }
        };
        if !need_other_number {
            break;
        }
    }

    Err(format!(
        "{label}: could not sign in after trying test numbers: {last_err}"
    ))
}

async fn wait_until<F, T>(timeout: Duration, mut pred: F) -> Result<T, String>
where
    F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<T>> + Send>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(val) = pred().await {
            return Ok(val);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out after {}s", timeout.as_secs()));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn extract_chat_or_channel_from_updates(
    updates: &tl_enums::Updates,
    session: &mut Session,
) -> (Option<ChatId>, Option<ChannelId>) {
    let mut chat_id = None;
    let mut channel_id = None;
    let chats = match updates {
        tl_enums::Updates::Updates(u) => {
            apply_tl_users_and_chats(session, &u.users, &u.chats);
            &u.chats[..]
        }
        tl_enums::Updates::Combined(u) => {
            apply_tl_users_and_chats(session, &u.users, &u.chats);
            &u.chats[..]
        }
        _ => &[],
    };
    for c in chats {
        match c {
            tl_enums::Chat::Chat(ch) => chat_id = Some(ChatId::from_tl(ch.id)),
            tl_enums::Chat::Channel(ch) => channel_id = Some(ChannelId::from_tl(ch.id)),
            _ => {}
        }
    }
    (chat_id, channel_id)
}

fn peer_messages(session: &Session, peer_id: PeerId) -> Vec<tg_data::Message> {
    let Some(history) = session.history(peer_id) else {
        return Vec::new();
    };
    let mut ids: Vec<MsgId> = history
        .loaded_ranges
        .slices()
        .iter()
        .flat_map(|slice| slice.messages.iter().copied())
        .collect();
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .filter_map(|msg_id| session.message(FullMsgId::new(peer_id, msg_id)).cloned())
        .collect()
}

fn extract_first_sent_msg_id(outcome: &tg_api::SendMessageOutcome) -> Option<MsgId> {
    outcome.messages.first().and_then(|m| match m.state {
        tg_api::PendingMessageState::Sent { server_id } => Some(server_id),
        _ => None,
    })
}

fn uploaded_to_tl_input_file(uploaded: &UploadedInputFile) -> tl_enums::InputFile {
    match uploaded {
        UploadedInputFile::Small {
            id,
            parts,
            name,
            md5_checksum,
        } => tl_enums::InputFile::InputFile(tl_types::InputFile {
            id: *id,
            parts: *parts as i32,
            name: name.clone(),
            md5_checksum: md5_checksum.clone(),
        }),
        UploadedInputFile::Big { id, parts, name } => {
            tl_enums::InputFile::Big(tl_types::InputFileBig {
                id: *id,
                parts: *parts as i32,
                name: name.clone(),
            })
        }
    }
}

/// Runs the full real-network smoke test suite against Telegram's official TEST DCs.
pub async fn run_smoke() -> Result<(), String> {
    let creds = load_test_api_credentials()?;
    println!("[init] Loaded TEST-ONLY API credentials from runtime source.");

    let dir_a = tempfile::TempDir::new().map_err(|e| format!("tempdir A: {e}"))?;
    let dir_b = tempfile::TempDir::new().map_err(|e| format!("tempdir B: {e}"))?;
    let guard_a = Arc::new(TestDcGuardTransportFactory::new());
    let guard_b = Arc::new(TestDcGuardTransportFactory::new());

    let opts_a = WorkingDirOpenOptions::new(dir_a.path(), creds.clone())
        .with_test_mode(true)
        .with_transport_factory(guard_a.clone());
    let opts_b = WorkingDirOpenOptions::new(dir_b.path(), creds.clone())
        .with_test_mode(true)
        .with_transport_factory(guard_b.clone());

    let mut mgr_a = AccountManager::open_and_unlock(opts_a, b"")
        .map_err(|e| format!("open AccountManager A: {e}"))?;
    let mut mgr_b = AccountManager::open_and_unlock(opts_b, b"")
        .map_err(|e| format!("open AccountManager B: {e}"))?;

    let slot_a = mgr_a.active_index().ok_or("no active slot A")?;
    let slot_b = mgr_b.active_index().ok_or("no active slot B")?;

    assert_eq!(
        mgr_a.account(slot_a).map(|a| a.environment),
        Some(CoreEnvironment::Test),
        "SAFETY VIOLATION: Account A not in Test environment"
    );
    assert_eq!(
        mgr_b.account(slot_b).map(|a| a.environment),
        Some(CoreEnvironment::Test),
        "SAFETY VIOLATION: Account B not in Test environment"
    );

    let handle = tokio::runtime::Handle::current();
    let mut rng = SimpleRng::from_time(0xA1B2_C3D4);

    // Track runtimes for guaranteed cleanup on exit.
    let mut rt_a: Option<SessionRuntime<MtprotoRpc>> = None;
    let mut rt_b: Option<SessionRuntime<MtprotoRpc>> = None;

    let res: Result<(), String> = async {
        // =====================================================================
        // Step 0: Real-network anonymous protocol verification on Test DCs 1, 2, 3
        //         (DH + PFS + help.getConfig test_mode=true + nearestDc + langpack +
        //          auth.exportLoginToken + forced TCP disconnect & reconnect recovery).
        // =====================================================================
        {
            let rpc_b_init = mgr_b.account(slot_b).unwrap().rpc.clone();
            let client_b_init = mgr_b.account(slot_b).unwrap().client.clone();
            for dc in [2, 1, 3] {
                let tl_enums::Config::Config(cfg) = rpc_b_init
                    .invoke_in_dc(dc, &tl_fns::help::GetConfig)
                    .await
                    .map_err(|e| format!("step 0: anon help.getConfig on DC{dc} failed: {e}"))?;
                guard_b.validate_and_register_config(&cfg);
                assert!(
                    cfg.this_dc == dc,
                    "step 0: expected this_dc={dc}, got {}",
                    cfg.this_dc
                );
                assert!(
                    client_b_init.apply_tl_config(&cfg),
                    "step 0: apply_tl_config rejected test config on DC{dc}"
                );
                println!(
                    "[step 0] Test DC{dc} help.getConfig OK: test_mode={}, this_dc={}, dc_options={}",
                    cfg.test_mode,
                    cfg.this_dc,
                    cfg.dc_options.len()
                );
            }

            let tl_enums::NearestDc::NearestDc(nearest) = rpc_b_init
                .invoke(&tl_fns::help::GetNearestDc)
                .await
                .map_err(|e| format!("step 0: help.getNearestDc failed: {e}"))?;
            println!(
                "[step 0] help.getNearestDc OK: country={}, this_dc={}, nearest_dc={}",
                nearest.country, nearest.this_dc, nearest.nearest_dc
            );

            let langs = rpc_b_init
                .invoke(&tl_fns::langpack::GetLanguages {
                    lang_pack: "tdesktop".to_owned(),
                })
                .await
                .map_err(|e| format!("step 0: langpack.getLanguages failed: {e}"))?;
            println!("[step 0] langpack.getLanguages OK: {} languages", langs.len());

            let qr_token = rpc_b_init
                .invoke(&tl_fns::auth::ExportLoginToken {
                    api_id: creds.api_id,
                    api_hash: creds.api_hash.clone(),
                    except_ids: Vec::new(),
                })
                .await
                .map_err(|e| format!("step 0: auth.exportLoginToken failed: {e}"))?;
            match qr_token {
                tl_enums::auth::LoginToken::LoginToken(tok) => {
                    println!(
                        "[step 0] auth.exportLoginToken OK: token_len={}, expires={}",
                        tok.token.len(),
                        tok.expires
                    );
                }
                other => {
                    println!("[step 0] auth.exportLoginToken returned {other:?}");
                }
            }

            // Forced TCP disconnect + reconnect + ping + RPC recovery on live Test DC2.
            let connects_before = guard_b.connect_count();
            guard_b.force_drop_active_connections();
            tokio::time::sleep(Duration::from_millis(300)).await;
            rpc_b_init
                .ping()
                .await
                .map_err(|e| format!("step 0: post-drop ping failed: {e}"))?;
            let tl_enums::Config::Config(cfg_after) = rpc_b_init
                .invoke(&tl_fns::help::GetConfig)
                .await
                .map_err(|e| format!("step 0: post-drop help.getConfig failed: {e}"))?;
            guard_b.validate_and_register_config(&cfg_after);
            let connects_after = guard_b.connect_count();
            assert!(
                connects_after > connects_before,
                "step 0: expected TCP reconnect after forced drop ({connects_before} -> {connects_after})"
            );
            println!(
                "[step 0] Forced TCP drop + reconnect + ping + RPC recovery OK (connects {connects_before} -> {connects_after})"
            );
        }

        // =====================================================================
        // Step 1: Sign up / log in A (start on DC 1, target DC 3 -> forces PHONE_MIGRATE_3)
        //         and B (target test DC 3).
        // =====================================================================
        println!("[step 1] Signing in Account A (start DC 1 -> target test DC 3, triggers PHONE_MIGRATE_3) and Account B (target test DC 3)...");
        mgr_a.account(slot_a).unwrap().client.set_main_dc_id(1);
        let initial_dc_a = mgr_a.account(slot_a).unwrap().main_dc_id();
        let (user_a, _phone_a) = login_test_account(
            "A",
            "Rustgram",
            "Test A",
            3,
            &mut mgr_a,
            slot_a,
            &creds,
            &mut rng,
        )
        .await?;
        let migrated_dc_a = mgr_a.account(slot_a).unwrap().main_dc_id();
        println!("  [step 1] Account A DC transition: DC{initial_dc_a} -> DC{migrated_dc_a}");
        if migrated_dc_a != 3 {
            return Err(format!(
                "step 1: expected Account A to migrate to test DC 3, got DC{migrated_dc_a}"
            ));
        }

        let (user_b, phone_b) = login_test_account(
            "B",
            "Rustgram",
            "Test B",
            3,
            &mut mgr_b,
            slot_b,
            &creds,
            &mut rng,
        )
        .await?;
        println!(
            "  [step 1] PASS: both accounts authorized (A=DC{}, B=DC{})",
            mgr_a.account(slot_a).unwrap().main_dc_id(),
            mgr_b.account(slot_b).unwrap().main_dc_id()
        );

        let rpc_a = mgr_a.account(slot_a).unwrap().rpc.clone();
        let rpc_b = mgr_b.account(slot_b).unwrap().rpc.clone();
        let client_a = mgr_a.account(slot_a).unwrap().client.clone();
        let client_b = mgr_b.account(slot_b).unwrap().client.clone();

        // =====================================================================
        // Step 2: help.getConfig (assert test_mode=true), start SessionRuntimes
        //         (dialogs + updates pump), contacts, A adds B.
        // =====================================================================
        println!("[step 2] Loading help.getConfig, dialogs, contacts, and importing B on A...");
        let tl_enums::Config::Config(cfg_a) = rpc_a
            .invoke(&tl_fns::help::GetConfig)
            .await
            .map_err(|e| format!("step 2: A help.getConfig failed: {e}"))?;
        guard_a.validate_and_register_config(&cfg_a);
        assert!(
            client_a.apply_tl_config(&cfg_a),
            "step 2: client_a.apply_tl_config rejected test config"
        );

        let tl_enums::Config::Config(cfg_b) = rpc_b
            .invoke(&tl_fns::help::GetConfig)
            .await
            .map_err(|e| format!("step 2: B help.getConfig failed: {e}"))?;
        guard_b.validate_and_register_config(&cfg_b);
        assert!(
            client_b.apply_tl_config(&cfg_b),
            "step 2: client_b.apply_tl_config rejected test config"
        );
        println!(
            "  [step 2] help.getConfig verified test_mode=true (A dc_options={}, B dc_options={})",
            cfg_a.dc_options.len(),
            cfg_b.dc_options.len()
        );

        let runtime_a = SessionRuntime::start_mtproto_on(
            UserId::from_tl(user_a.id),
            rpc_a.clone(),
            handle.clone(),
            SessionRuntimeConfig::default(),
        )
        .map_err(|e| format!("step 2: start runtime A: {e}"))?;
        runtime_a.seed_self_user_async(&user_a).await;
        rt_a = Some(runtime_a);

        let runtime_b = SessionRuntime::start_mtproto_on(
            UserId::from_tl(user_b.id),
            rpc_b.clone(),
            handle.clone(),
            SessionRuntimeConfig::default(),
        )
        .map_err(|e| format!("step 2: start runtime B: {e}"))?;
        runtime_b.seed_self_user_async(&user_b).await;
        rt_b = Some(runtime_b);

        let ra = rt_a.as_ref().unwrap();
        let rb = rt_b.as_ref().unwrap();
        ra.wait_ready()
            .await
            .map_err(|e| format!("step 2: runtime A wait_ready failed: {e}"))?;
        rb.wait_ready()
            .await
            .map_err(|e| format!("step 2: runtime B wait_ready failed: {e}"))?;

        let mut contacts_a = ContactsManager::new();
        {
            let mut sa = ra.session().write().await;
            let _ = contacts_a
                .fetch_contacts(&rpc_a, &mut sa)
                .await
                .map_err(|e| format!("step 2: A fetch_contacts failed: {e}"))?;
        }
        let mut contacts_b = ContactsManager::new();
        {
            let mut sb = rb.session().write().await;
            let _ = contacts_b
                .fetch_contacts(&rpc_b, &mut sb)
                .await
                .map_err(|e| format!("step 2: B fetch_contacts failed: {e}"))?;
        }

        // A imports B via contacts.importContacts with B's test phone number.
        let tl_enums::contacts::ImportedContacts::ImportedContacts(imported) = rpc_a
            .invoke(&tl_fns::contacts::ImportContacts {
                contacts: vec![tl_enums::InputContact::InputPhoneContact(
                    tl_types::InputPhoneContact {
                        client_id: 1,
                        phone: format!("+{phone_b}"),
                        first_name: "Rustgram".to_owned(),
                        last_name: "Test B".to_owned(),
                        note: None,
                    },
                )],
            })
            .await
            .map_err(|e| format!("step 2: contacts.importContacts failed: {e}"))?;
        {
            let mut sa = ra.session().write().await;
            apply_tl_users_and_chats(&mut sa, &imported.users, &[]);
        }

        let peer_b = PeerId::from_user(UserId::from_tl(user_b.id));
        let peer_a = PeerId::from_user(UserId::from_tl(user_a.id));

        // Verify A has B's access_hash (if importContacts returned empty, fall back to username).
        let a_has_b = {
            let sa = ra.session().read().await;
            sa.user(UserId::from_tl(user_b.id))
                .is_some_and(|u| u.access_hash != 0)
        };
        if !a_has_b {
            let uname = format!("rustgramtestb{}", rng.next_u64() % 1_000_000);
            let _ = rpc_b
                .invoke(&tl_fns::account::UpdateUsername {
                    username: uname.clone(),
                })
                .await
                .map_err(|e| format!("step 2: B updateUsername fallback failed: {e}"))?;
            let mut sa = ra.session().write().await;
            let resolved = tg_api::resolve_username(&rpc_a, &mut sa, &uname, None)
                .await
                .map_err(|e| format!("step 2: A resolve_username fallback failed: {e}"))?;
            if resolved.peer_id != peer_b {
                return Err(format!(
                    "step 2: resolved peer {:?} != expected {:?}",
                    resolved.peer_id, peer_b
                ));
            }
        }
        println!("  [step 2] PASS: config, dialogs, contacts, and peer resolution succeeded.");

        // =====================================================================
        // Step 3: Direct messaging (formatted text, live pump on B, edit, reply,
        //         forward, react, delete for both).
        // =====================================================================
        println!("[step 3] Testing direct messages (format, live pump, edit, reply, forward, react, delete)...");
        let msg_text = "Hello bold italic code!";
        let entities = vec![
            tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
                offset: 6,
                length: 4,
            }),
            tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic {
                offset: 11,
                length: 6,
            }),
            tl_enums::MessageEntity::Code(tl_types::MessageEntityCode {
                offset: 18,
                length: 4,
            }),
        ];
        let send1 = ra
            .send_message_with_entities(peer_b, msg_text, entities, None, SendOptions::default())
            .await
            .map_err(|e| format!("step 3: A send_message failed: {e}"))?;
        let msg1_id = extract_first_sent_msg_id(&send1)
            .ok_or("step 3: missing server msg_id for A's sent message")?;

        // Wait for B to receive msg1 via live update pump (no getHistory!).
        let b_msg1_id = wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(rb.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                for msg in peer_messages(&s, peer_a) {
                    if let Some(t) = msg.text()
                        && t.text == msg_text
                    {
                        return Some((msg.id, t.entities.len()));
                    }
                }
                None
            })
        })
        .await
        .map_err(|e| format!("step 3: B did not receive formatted message via live pump: {e}"))?;
        if b_msg1_id.1 < 3 {
            return Err(format!(
                "step 3: B received message with {} entities, expected >= 3",
                b_msg1_id.1
            ));
        }
        println!(
            "  [step 3] Live formatted message delivered (A id={}, B id={}, entities={})",
            msg1_id.0, b_msg1_id.0.0, b_msg1_id.1
        );

        // Edit message on A -> verify B receives edit live.
        let edited_text = "Hello EDITED bold text!";
        let input_peer_b = {
            let sa = ra.session().read().await;
            input_peer_for(&sa, peer_b)
        };
        let sender_a = MessageSender::default();
        let edit_updates = sender_a
            .edit_message(
                &rpc_a,
                tl_fns::messages::EditMessage {
                    no_webpage: false,
                    invert_media: false,
                    peer: input_peer_b.clone(),
                    id: msg1_id.0 as i32,
                    message: Some(edited_text.to_owned()),
                    media: None,
                    reply_markup: None,
                    entities: Some(vec![tl_enums::MessageEntity::Bold(
                        tl_types::MessageEntityBold {
                            offset: 6,
                            length: 6,
                        },
                    )]),
                    schedule_date: None,
                    schedule_repeat_period: None,
                    quick_reply_shortcut_id: None,
                    rich_message: None,
                },
            )
            .await
            .map_err(|e| format!("step 3: A edit_message failed: {e}"))?;
        let _ = edit_updates;

        wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(rb.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                let m = s.message(FullMsgId::new(peer_a, b_msg1_id.0))?;
                m.text().is_some_and(|t| t.text == edited_text).then_some(())
            })
        })
        .await
        .map_err(|e| format!("step 3: B did not receive live edit update: {e}"))?;
        println!("  [step 3] Live edit delivered.");

        // B replies to A's message -> verify A receives reply live.
        let reply_text = "Reply from B";
        let reply_hdr = tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
            reply_to_msg_id: b_msg1_id.0.0 as i32,
            top_msg_id: None,
            reply_to_peer_id: None,
            quote_text: None,
            quote_entities: None,
            quote_offset: None,
            monoforum_peer_id: None,
            todo_item_id: None,
            poll_option: None,
            quote_rich: None,
        });
        let send_reply = rb
            .send_message_with_entities(
                peer_a,
                reply_text,
                Vec::new(),
                Some(reply_hdr),
                SendOptions::default(),
            )
            .await
            .map_err(|e| format!("step 3: B reply failed: {e}"))?;
        let _b_reply_id = extract_first_sent_msg_id(&send_reply)
            .ok_or("step 3: missing server msg_id for B's reply")?;

        let a_reply_id = wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(ra.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                for msg in peer_messages(&s, peer_b) {
                    if msg.text().is_some_and(|t| t.text == reply_text) && msg.reply_to.is_some() {
                        return Some(msg.id);
                    }
                }
                None
            })
        })
        .await
        .map_err(|e| format!("step 3: A did not receive B's reply live: {e}"))?;
        println!("  [step 3] Live reply delivered (A id={}).", a_reply_id.0);

        // A forwards B's reply to Saved Messages (self).
        let mut fwd_sender = MessageSender::default();
        let fwd_out = fwd_sender
            .forward_messages(
                &rpc_a,
                &[ForwardItem {
                    from_peer: input_peer_b.clone(),
                    msg_id: a_reply_id,
                }],
                tl_enums::InputPeer::Self_,
                None,
                ForwardOptions::PreserveInfo,
                SendOptions::default(),
            )
            .await
            .map_err(|e| format!("step 3: A forward_messages failed: {e:?}"))?;
        if fwd_out.updates.is_empty() {
            return Err("step 3: forward_messages returned no updates".into());
        }
        println!("  [step 3] Forward to Saved Messages succeeded.");

        // B reacts to A's message with thumbs-up -> verify A receives reaction update.
        let input_peer_a = {
            let sb = rb.session().read().await;
            input_peer_for(&sb, peer_a)
        };
        let _ = rpc_b
            .invoke(&tl_fns::messages::SendReaction {
                big: false,
                add_to_recent: false,
                peer: input_peer_a.clone(),
                msg_id: b_msg1_id.0.0 as i32,
                reaction: Some(vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "\u{1F44D}".to_owned(),
                })]),
            })
            .await
            .map_err(|e| format!("step 3: B sendReaction failed: {e}"))?;

        wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(ra.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                let m = s.message(FullMsgId::new(peer_b, msg1_id))?;
                m.reactions
                    .as_ref()
                    .is_some_and(|r| !r.results.is_empty())
                    .then_some(())
            })
        })
        .await
        .map_err(|e| format!("step 3: A did not receive reaction update live: {e}"))?;
        println!("  [step 3] Live reaction delivered.");

        // A deletes msg1 for both (`revoke: true`) -> verify B removes it live.
        let affected = sender_a
            .delete_messages(&rpc_a, input_peer_b.clone(), vec![msg1_id], true)
            .await
            .map_err(|e| format!("step 3: A delete_messages(revoke=true) failed: {e}"))?;
        let _ = ra
            .apply_affected_history(Some(peer_b), affected.pts, affected.pts_count)
            .await;

        wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(rb.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                s.message(FullMsgId::new(peer_a, b_msg1_id.0))
                    .is_none()
                    .then_some(())
            })
        })
        .await
        .map_err(|e| format!("step 3: B did not receive live delete(revoke=true): {e}"))?;
        println!("  [step 3] PASS: direct messaging full cycle succeeded.");

        // =====================================================================
        // Step 4: Group & Channel (create, post, B joins, read receipts & unread).
        // =====================================================================
        println!("[step 4] Creating small group and channel, posting, joining, checking unread & read receipts...");
        let input_user_b = {
            let sa = ra.session().read().await;
            input_user_for(&sa, UserId::from_tl(user_b.id))
        };
        let tl_enums::messages::InvitedUsers::InvitedUsers(created_group) = rpc_a
            .invoke(&tl_fns::messages::CreateChat {
                users: vec![input_user_b],
                title: "Rustgram Test Group".to_owned(),
                ttl_period: None,
            })
            .await
            .map_err(|e| format!("step 4: messages.createChat failed: {e}"))?;
        let group_chat_id = {
            let mut sa = ra.session().write().await;
            extract_chat_or_channel_from_updates(&created_group.updates, &mut sa)
                .0
                .ok_or("step 4: createChat did not return a ChatId")?
        };
        let group_peer = PeerId::from_chat(group_chat_id);

        let group_post = "Group smoke post from A";
        let grp_send = ra
            .send_message(group_peer, group_post, SendOptions::default())
            .await
            .map_err(|e| format!("step 4: A group post failed: {e}"))?;
        let grp_msg_id = extract_first_sent_msg_id(&grp_send)
            .ok_or("step 4: missing group msg_id")?;

        // Wait for B to receive group message via live pump and check unread > 0.
        let b_grp_msg_id = wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(rb.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                let hist = s.history(group_peer)?;
                for msg in peer_messages(&s, group_peer) {
                    if msg.text().is_some_and(|t| t.text == group_post) {
                        return Some((msg.id, hist.unread_count.unwrap_or(0)));
                    }
                }
                None
            })
        })
        .await
        .map_err(|e| format!("step 4: B did not receive group post live: {e}"))?;
        println!(
            "  [step 4] Group post received on B (msg_id={}, unread_count={})",
            b_grp_msg_id.0.0, b_grp_msg_id.1
        );

        // B marks group read -> verify unread clears on B and read receipt reaches A.
        let _ = rb
            .mark_read(group_peer, b_grp_msg_id.0, true)
            .await
            .map_err(|e| format!("step 4: B mark_read(group) failed: {e}"))?;
        wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(ra.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                let hist = s.history(group_peer)?;
                hist.outbox_read_before
                    .is_some_and(|r| r.0 > grp_msg_id.0)
                    .then_some(())
            })
        })
        .await
        .map_err(|e| format!("step 4: A did not receive group read receipt: {e}"))?;
        let b_grp_unread_after = {
            let sb = rb.session().read().await;
            sb.history(group_peer).and_then(|h| h.unread_count).unwrap_or(0)
        };
        if b_grp_unread_after != 0 {
            return Err(format!(
                "step 4: B group unread_count after mark_read is {b_grp_unread_after}, expected 0"
            ));
        }
        println!("  [step 4] Group read receipt & unread counter verified.");

        // Channel: A creates broadcast channel, exports invite link, B joins, A posts, B reads.
        let chan_updates = rpc_a
            .invoke(&tl_fns::channels::CreateChannel {
                broadcast: true,
                megagroup: false,
                for_import: false,
                forum: false,
                title: "Rustgram Test Channel".to_owned(),
                about: "Automated test-DC smoke channel".to_owned(),
                geo_point: None,
                address: None,
                ttl_period: None,
            })
            .await
            .map_err(|e| format!("step 4: channels.createChannel failed: {e}"))?;
        let channel_id = {
            let mut sa = ra.session().write().await;
            extract_chat_or_channel_from_updates(&chan_updates, &mut sa)
                .1
                .ok_or("step 4: createChannel did not return ChannelId")?
        };
        let channel_peer = PeerId::from_channel(channel_id);
        let input_channel_peer = {
            let sa = ra.session().read().await;
            input_peer_for(&sa, channel_peer)
        };
        let exported = rpc_a
            .invoke(&tl_fns::messages::ExportChatInvite {
                legacy_revoke_permanent: false,
                request_needed: false,
                peer: input_channel_peer,
                expire_date: None,
                usage_limit: None,
                title: Some("Smoke Invite".to_owned()),
                subscription_pricing: None,
            })
            .await
            .map_err(|e| format!("step 4: exportChatInvite failed: {e}"))?;
        let invite_link = match exported {
            tl_enums::ExportedChatInvite::ChatInviteExported(inv) => inv.link,
            other => return Err(format!("step 4: unexpected invite variant: {other:?}")),
        };
        let invite_hash = invite_link
            .rsplit('/')
            .next()
            .map(|s| s.trim_start_matches('+'))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("step 4: could not extract hash from '{invite_link}'"))?
            .to_owned();

        let join_res = rpc_b
            .invoke(&tl_fns::messages::ImportChatInvite { hash: invite_hash })
            .await
            .map_err(|e| format!("step 4: B importChatInvite failed: {e}"))?;
        match join_res {
            tl_enums::messages::ChatInviteJoinResult::Ok(ok) => {
                let mut sb = rb.session().write().await;
                let _ = extract_chat_or_channel_from_updates(&ok.updates, &mut sb);
            }
            other => return Err(format!("step 4: unexpected join result: {other:?}")),
        }
        // Mark channel active on B so channel updates / getChannelDifference stay hot.
        let _ = rb.set_active_channel(channel_id, true).await;
        let _ = rb
            .open_history(channel_peer, MsgId::ZERO, tg_api::LoadDirection::Before)
            .await
            .map_err(|e| format!("step 4: B open_history(channel) failed: {e}"))?;

        let chan_post = "Channel broadcast post from A";
        let _ = ra
            .send_message(channel_peer, chan_post, SendOptions::default())
            .await
            .map_err(|e| format!("step 4: A channel post failed: {e}"))?;

        let b_chan_msg = wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(rb.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                for msg in peer_messages(&s, channel_peer) {
                    if msg.text().is_some_and(|t| t.text == chan_post) {
                        return Some(msg.id);
                    }
                }
                None
            })
        })
        .await
        .map_err(|e| format!("step 4: B did not receive channel post live: {e}"))?;
        let _ = rb
            .mark_read(channel_peer, b_chan_msg, true)
            .await
            .map_err(|e| format!("step 4: B mark_read(channel) failed: {e}"))?;
        println!("  [step 4] PASS: group + channel + join + read receipts succeeded.");

        // =====================================================================
        // Step 5: Upload small generated image & ~3 MB file (big-file path),
        //         B downloads both and SHA-256 hashes match.
        // =====================================================================
        println!("[step 5] Uploading small PNG image + ~3 MB big file and verifying B's downloads...");
        let invoker_a = LiveMediaInvoker::new(rpc_a.clone());
        let invoker_b = LiveMediaInvoker::new(rpc_b.clone());
        let cancel = CancelToken::new();

        // 5A. Small generated PNG image (small-file path: upload.saveFilePart).
        let png_bytes = generate_test_png(32, 32);
        let png_sha = sha256_hex(&png_bytes);
        let uploaded_png = upload_bytes(
            &invoker_a,
            "rustgram_smoke.png",
            &png_bytes,
            UploadConfig::default(),
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 5A: upload small PNG failed: {e}"))?;
        assert!(
            matches!(uploaded_png, UploadedInputFile::Small { .. }),
            "step 5A: expected small-file upload path"
        );
        let png_caption = "smoke-small-png";
        let _ = rpc_a
            .invoke(&tl_fns::messages::SendMedia {
                silent: false,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: input_peer_b.clone(),
                reply_to: None,
                media: tl_enums::InputMedia::UploadedDocument(
                    tl_types::InputMediaUploadedDocument {
                        nosound_video: false,
                        force_file: true,
                        spoiler: false,
                        file: uploaded_to_tl_input_file(&uploaded_png),
                        thumb: None,
                        mime_type: "image/png".to_owned(),
                        attributes: vec![tl_enums::DocumentAttribute::Filename(
                            tl_types::DocumentAttributeFilename {
                                file_name: "rustgram_smoke.png".to_owned(),
                            },
                        )],
                        stickers: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                ),
                message: png_caption.to_owned(),
                random_id: rng.next_u64() as i64,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            })
            .await
            .map_err(|e| format!("step 5A: sendMedia(small PNG) failed: {e}"))?;

        let png_doc = wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(rb.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                for msg in peer_messages(&s, peer_a) {
                    if msg.text().is_some_and(|t| t.text == png_caption)
                        && let Some(MessageMedia::Document(doc)) = msg.media()
                    {
                        return Some(doc.clone());
                    }
                }
                None
            })
        })
        .await
        .map_err(|e| format!("step 5A: B did not receive small PNG message live: {e}"))?;

        let dl_png = download_file(
            &invoker_b,
            DownloadRequest::new(
                png_doc.dc_id,
                TransferLocation::Document {
                    id: png_doc.id.0,
                    access_hash: png_doc.access_hash,
                    file_reference: png_doc.file_reference.clone(),
                    thumb_size: String::new(),
                },
                png_doc.size as u64,
            ),
            None,
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 5A: B download small PNG failed: {e}"))?;
        let dl_png_sha = sha256_hex(&dl_png);
        if dl_png_sha != png_sha {
            return Err(format!(
                "step 5A: small PNG SHA-256 mismatch (sent={png_sha}, got={dl_png_sha})"
            ));
        }
        println!(
            "  [step 5A] Small PNG uploaded ({} bytes) & downloaded by B with matching SHA-256.",
            png_bytes.len()
        );

        // 5B. ~3 MB generated file forced through big-file path (`upload.saveBigFilePart` + `inputFileBig`).
        let big_bytes = generate_big_payload();
        let big_sha = sha256_hex(&big_bytes);
        let big_cfg = UploadConfig {
            big_file_threshold: 1024 * 1024, // force saveBigFilePart for ~3 MB file
            max_sessions: 4,
            ..UploadConfig::default()
        };
        let uploaded_big = upload_bytes(
            &invoker_a,
            "rustgram_smoke_3mb.bin",
            &big_bytes,
            big_cfg,
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 5B: upload 3 MB big file failed: {e}"))?;
        assert!(
            matches!(uploaded_big, UploadedInputFile::Big { .. }),
            "step 5B: expected big-file upload path"
        );
        let big_caption = "smoke-big-3mb";
        let _ = rpc_a
            .invoke(&tl_fns::messages::SendMedia {
                silent: false,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: input_peer_b.clone(),
                reply_to: None,
                media: tl_enums::InputMedia::UploadedDocument(
                    tl_types::InputMediaUploadedDocument {
                        nosound_video: false,
                        force_file: true,
                        spoiler: false,
                        file: uploaded_to_tl_input_file(&uploaded_big),
                        thumb: None,
                        mime_type: "application/octet-stream".to_owned(),
                        attributes: vec![tl_enums::DocumentAttribute::Filename(
                            tl_types::DocumentAttributeFilename {
                                file_name: "rustgram_smoke_3mb.bin".to_owned(),
                            },
                        )],
                        stickers: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                ),
                message: big_caption.to_owned(),
                random_id: rng.next_u64() as i64,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            })
            .await
            .map_err(|e| format!("step 5B: sendMedia(3 MB file) failed: {e}"))?;

        let big_doc = wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(rb.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                for msg in peer_messages(&s, peer_a) {
                    if msg.text().is_some_and(|t| t.text == big_caption)
                        && let Some(MessageMedia::Document(doc)) = msg.media()
                    {
                        return Some(doc.clone());
                    }
                }
                None
            })
        })
        .await
        .map_err(|e| format!("step 5B: B did not receive 3 MB file message live: {e}"))?;

        let dl_big = download_file(
            &invoker_b,
            DownloadRequest::new(
                big_doc.dc_id,
                TransferLocation::Document {
                    id: big_doc.id.0,
                    access_hash: big_doc.access_hash,
                    file_reference: big_doc.file_reference.clone(),
                    thumb_size: String::new(),
                },
                big_doc.size as u64,
            ),
            None,
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 5B: B download 3 MB file failed: {e}"))?;
        let dl_big_sha = sha256_hex(&dl_big);
        if dl_big_sha != big_sha {
            return Err(format!(
                "step 5B: 3 MB file SHA-256 mismatch (sent={big_sha}, got={dl_big_sha})"
            ));
        }
        println!(
            "  [step 5B] PASS: ~3 MB big file ({} bytes) uploaded & downloaded by B with matching SHA-256.",
            big_bytes.len()
        );

        // =====================================================================
        // Step 6: Force TCP drop on B, send messages from A while disconnected,
        //         reconnect and verify getDifference recovers all without loss.
        // =====================================================================
        println!("[step 6] Forcing TCP drop on Account B and verifying reconnect + getDifference...");
        let connects_before = guard_b.connect_count();
        guard_b.set_block_connects(true);
        guard_b.force_drop_active_connections();
        tokio::time::sleep(Duration::from_millis(500)).await;

        let gap_msgs = [
            "reconnect-gap-msg-1",
            "reconnect-gap-msg-2",
            "reconnect-gap-msg-3",
        ];
        for text in &gap_msgs {
            let _ = ra
                .send_message(peer_b, *text, SendOptions::default())
                .await
                .map_err(|e| format!("step 6: A send '{text}' failed: {e}"))?;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Allow B to reconnect and trigger difference recovery.
        guard_b.set_block_connects(false);
        rb.on_reconnect()
            .await
            .map_err(|e| format!("step 6: B on_reconnect failed: {e}"))?;

        wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(rb.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                let msgs = peer_messages(&s, peer_a);
                let all_present = gap_msgs.iter().all(|expected| {
                    msgs.iter()
                        .any(|m| m.text().is_some_and(|t| t.text == *expected))
                });
                all_present.then_some(())
            })
        })
        .await
        .map_err(|e| format!("step 6: B missed messages after forced TCP reconnect: {e}"))?;

        let connects_after = guard_b.connect_count();
        if connects_after <= connects_before {
            return Err(format!(
                "step 6: expected reconnect count to increase (before={connects_before}, after={connects_after})"
            ));
        }
        println!(
            "  [step 6] PASS: TCP drop + reconnect ({connects_before} -> {connects_after}) recovered all 3 messages via getDifference."
        );

        Ok(())
    }
    .await;

    // =========================================================================
    // Step 7: Always log out both accounts and wipe temporary session dirs.
    // =========================================================================
    println!("[step 7] Logging out both test accounts and removing temporary tdata...");
    if let Some(r) = rt_a.take() {
        r.shutdown();
    }
    if let Some(r) = rt_b.take() {
        r.shutdown();
    }
    guard_a.set_block_connects(false);
    guard_b.set_block_connects(false);
    let logout_a = mgr_a.logout_account(slot_a).await;
    let logout_b = mgr_b.logout_account(slot_b).await;
    mgr_a.shutdown();
    mgr_b.shutdown();
    drop(dir_a);
    drop(dir_b);

    match (&logout_a, &logout_b) {
        (Ok(la), Ok(lb)) => {
            println!(
                "  [step 7] PASS: logged out A (remaining keys={}) and B (remaining keys={}); temp tdata deleted.",
                la.remaining.len(),
                lb.remaining.len()
            );
        }
        _ => {
            eprintln!("  [step 7] WARN: logout_a={logout_a:?}, logout_b={logout_b:?}");
        }
    }

    res?;
    logout_a.map_err(|e| format!("step 7: logout A failed: {e}"))?;
    logout_b.map_err(|e| format!("step 7: logout B failed: {e}"))?;
    println!("\nALL TEST-DC SMOKE STEPS PASSED!");
    Ok(())
}

/// Extracts every server message ID from a TL `Updates` batch (used to track created messages
/// for guaranteed cleanup).
fn extract_msg_ids_from_updates(updates: &tl_enums::Updates) -> Vec<MsgId> {
    let mut ids = Vec::new();
    let mut push_from_update = |u: &tl_enums::Update| match u {
        tl_enums::Update::MessageID(m) => ids.push(MsgId(i64::from(m.id))),
        tl_enums::Update::NewMessage(m) => {
            if let Some(id) = tl_message_id(&m.message) {
                ids.push(id);
            }
        }
        tl_enums::Update::NewChannelMessage(m) => {
            if let Some(id) = tl_message_id(&m.message) {
                ids.push(id);
            }
        }
        _ => {}
    };
    match updates {
        tl_enums::Updates::UpdateShortSentMessage(s) => ids.push(MsgId(i64::from(s.id))),
        tl_enums::Updates::UpdateShortMessage(s) => ids.push(MsgId(i64::from(s.id))),
        tl_enums::Updates::UpdateShortChatMessage(s) => ids.push(MsgId(i64::from(s.id))),
        tl_enums::Updates::UpdateShort(s) => push_from_update(&s.update),
        tl_enums::Updates::Updates(u) => {
            for item in &u.updates {
                push_from_update(item);
            }
        }
        tl_enums::Updates::Combined(c) => {
            for item in &c.updates {
                push_from_update(item);
            }
        }
        tl_enums::Updates::TooLong => {}
    }
    ids.sort();
    ids.dedup();
    ids
}

fn tl_message_id(msg: &tl_enums::Message) -> Option<MsgId> {
    match msg {
        tl_enums::Message::Empty(m) => Some(MsgId(i64::from(m.id))),
        tl_enums::Message::Message(m) => Some(MsgId(i64::from(m.id))),
        tl_enums::Message::Service(m) => Some(MsgId(i64::from(m.id))),
    }
}

fn check_flood_wait_inv(step: &str, op: &str, err: &InvocationError) -> String {
    if let InvocationError::Rpc(r) = err
        && let Some(secs) = r.flood_wait_seconds()
        && secs > MAX_FLOOD_WAIT_SECS
    {
        return format!("{step}: {op} hit FLOOD_WAIT_{secs} (>60s); stopped");
    }
    format!("{step}: {op} failed: {err}")
}

fn check_flood_wait_api(step: &str, op: &str, err: &ApiError) -> String {
    if let ApiError::Invocation(inv) = err {
        return check_flood_wait_inv(step, op, inv);
    }
    format!("{step}: {op} failed: {err}")
}

fn now_unix_secs() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i32)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[derive(Default)]
struct SingleAccountCleanup {
    saved_msg_ids: Vec<MsgId>,
    channels: Vec<(ChannelId, i64, String)>,
}

impl SingleAccountCleanup {
    fn track_saved(&mut self, id: MsgId) {
        if id.0 > 0 && !self.saved_msg_ids.contains(&id) {
            self.saved_msg_ids.push(id);
        }
    }

    fn track_channel(&mut self, id: ChannelId, access_hash: i64, label: &str) {
        if !self.channels.iter().any(|(c, _, _)| *c == id) {
            self.channels.push((id, access_hash, label.to_owned()));
        }
    }

    fn untrack_channel(&mut self, id: ChannelId) {
        self.channels.retain(|(c, _, _)| *c != id);
    }

    fn untrack_saved(&mut self, ids: &[MsgId]) {
        self.saved_msg_ids.retain(|id| !ids.contains(id));
    }
}

/// Runs the single-account logged-in smoke test suite (`--session <dir>`) against Telegram's
/// official TEST DCs using a copied `tdata` working directory.
///
/// Safety guarantees:
/// - Never logs out or deletes the account, never modifies profile, never messages anyone else.
/// - Always deletes every message created in Saved Messages and every created group/channel.
pub async fn run_single_account_smoke(session_dir: PathBuf) -> Result<(), String> {
    let creds = load_test_api_credentials()?;
    println!(
        "[init] Loaded TEST-ONLY API credentials from runtime source; opening session copy at {}",
        session_dir.display()
    );

    let guard = Arc::new(TestDcGuardTransportFactory::new());
    let opts = WorkingDirOpenOptions::new(&session_dir, creds.clone())
        .with_test_mode(true)
        .with_transport_factory(guard.clone());

    let mut mgr = AccountManager::open_and_unlock(opts, b"")
        .map_err(|e| format!("step 1: open AccountManager failed: {e}"))?;
    let slot = mgr
        .active_index()
        .ok_or("step 1: no active account slot in session dir")?;
    let acc = mgr
        .account(slot)
        .ok_or("step 1: active slot missing LiveAccount")?;

    assert_eq!(
        acc.environment,
        CoreEnvironment::Test,
        "SAFETY VIOLATION: loaded session is NOT in Environment::Test!"
    );
    let self_uid = acc
        .user_id
        .ok_or("step 1: loaded session is not authorized (user_id is None)")?;
    let self_peer = PeerId::from_user(self_uid);
    let rpc = acc.rpc.clone();
    let client = acc.client.clone();
    println!(
        "[step 1] Unlocked authorized test-DC session (main_dc={}, persistent_keys={})",
        acc.main_dc_id(),
        client.export_persistent_keys().len()
    );

    // Always verify help.getConfig test_mode=true FIRST before any other RPC.
    let tl_enums::Config::Config(cfg) = rpc
        .invoke(&tl_fns::help::GetConfig)
        .await
        .map_err(|e| check_flood_wait_inv("step 1", "help.getConfig", &e))?;
    guard.validate_and_register_config(&cfg);
    assert!(
        client.apply_tl_config(&cfg),
        "step 1: client.apply_tl_config rejected test config"
    );
    println!(
        "  [step 1] help.getConfig OK: test_mode={}, this_dc={}, dc_options={}",
        cfg.test_mode,
        cfg.this_dc,
        cfg.dc_options.len()
    );

    let handle = tokio::runtime::Handle::current();
    let runtime = SessionRuntime::start_mtproto_on(
        self_uid,
        rpc.clone(),
        handle.clone(),
        SessionRuntimeConfig::default(),
    )
    .map_err(|e| format!("step 1: start SessionRuntime failed: {e}"))?;
    if let Some(ref u) = acc.self_user {
        runtime.seed_self_user_async(u).await;
    }

    let mut cleanup = SingleAccountCleanup::default();
    let mut step_results: Vec<(&'static str, Result<String, String>)> = Vec::new();
    let mut rng = SimpleRng::from_time(0x51A7_E001);

    // =========================================================================
    // Step 1: updates.getState, load dialogs (paging), contacts, help.getConfig,
    //         account.getAuthorizations.
    // =========================================================================
    let step1_res: Result<String, String> = async {
        // Explicit updates.getState probe in addition to SessionRuntime startup.
        let tl_enums::updates::State::State(raw_state) = rpc
            .invoke(&tl_fns::updates::GetState)
            .await
            .map_err(|e| check_flood_wait_inv("step 1", "updates.getState", &e))?;
        println!(
            "  [step 1] updates.getState OK: pts={}, qts={}, date={}, seq={}, unread={}",
            raw_state.pts, raw_state.qts, raw_state.date, raw_state.seq, raw_state.unread_count
        );

        runtime
            .wait_ready()
            .await
            .map_err(|e| format!("step 1: runtime wait_ready failed: {e}"))?;

        let persisted = runtime
            .persisted_updates_state()
            .await
            .map_err(|e| format!("step 1: persisted_updates_state failed: {e}"))?;
        if persisted.pts <= 0 || persisted.date <= 0 {
            return Err(format!(
                "step 1: invalid runtime updates state after startup: {persisted:?}"
            ));
        }

        // Page through main dialogs (page 1 loaded during wait_ready; continue until complete).
        let mut extra_pages = 0usize;
        for _ in 0..10 {
            match runtime.load_dialogs_page(0).await {
                Ok(Some(page)) => {
                    extra_pages += 1;
                    if page.folder_done {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(format!("step 1: load_dialogs_page failed: {e}")),
            }
        }
        let dialog_count = runtime.dialog_summaries_async(200).await.len();
        println!(
            "  [step 1] Dialogs paging OK: initial + {extra_pages} extra page(s), {dialog_count} dialogs in Session"
        );

        // Load contacts.
        let mut contacts = ContactsManager::new();
        let contacts_res = {
            let mut s = runtime.session().write().await;
            contacts
                .fetch_contacts(&rpc, &mut s)
                .await
                .map_err(|e| check_flood_wait_api("step 1", "contacts.getContacts", &e))?
        };
        let contacts_res2 = {
            let mut s = runtime.session().write().await;
            contacts
                .fetch_contacts(&rpc, &mut s)
                .await
                .map_err(|e| check_flood_wait_api("step 1", "contacts.getContacts#2", &e))?
        };
        println!(
            "  [step 1] Contacts OK: first={contacts_res:?}, second={contacts_res2:?}"
        );

        // Load account.getAuthorizations via AuthorizationsManager (do not log personal data).
        let mut auths_mgr = AuthorizationsManager::new();
        let (auths_count, has_current) = {
            let auths = auths_mgr
                .reload(&rpc, now_unix_secs())
                .await
                .map_err(|e| check_flood_wait_api("step 1", "account.getAuthorizations", &e))?;
            (auths.len(), auths.iter().any(|a| a.hash == 0))
        };
        if auths_count == 0 || !has_current {
            return Err(format!(
                "step 1: account.getAuthorizations returned {auths_count} entries, has_current={has_current}"
            ));
        }
        println!(
            "  [step 1] account.getAuthorizations OK: count={auths_count}, has_current={has_current}, ttl_days={}",
            auths_mgr.ttl_days()
        );

        Ok(format!(
            "pts={}, dialogs={dialog_count}, extra_pages={extra_pages}, auths={auths_count}",
            persisted.pts
        ))
    }
    .await;
    step_results.push((
        "Step 1 (session/state/dialogs/contacts/config/auths)",
        step1_res,
    ));

    // =========================================================================
    // Step 2: Saved Messages: send formatted text, edit, reply, forward to itself,
    //         react, pin/unpin, delete. Check live update pump delivers each change.
    // =========================================================================
    let step2_res: Result<String, String> = async {
        println!("[step 2] Testing Saved Messages (format, edit, reply, forward, react, pin/unpin, delete)...");
        let mut changes_rx = runtime.subscribe_changes();
        let mut raw_push_rx = rpc.subscribe_updates();

        // 2A. Send text with formatting entities (Bold, Italic, Code).
        let msg1_text = "Saved bold italic code smoke";
        let entities = vec![
            tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
                offset: 6,
                length: 4,
            }),
            tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic {
                offset: 11,
                length: 6,
            }),
            tl_enums::MessageEntity::Code(tl_types::MessageEntityCode {
                offset: 18,
                length: 4,
            }),
        ];
        let send1 = runtime
            .send_message_with_entities(self_peer, msg1_text, entities, None, SendOptions::default())
            .await
            .map_err(|e| format!("step 2: send formatted message failed: {e}"))?;
        let msg1_id = extract_first_sent_msg_id(&send1)
            .ok_or("step 2: missing server msg_id for formatted message")?;
        cleanup.track_saved(msg1_id);
        for upd in &send1.updates {
            for id in extract_msg_ids_from_updates(upd) {
                cleanup.track_saved(id);
            }
        }

        // Confirm Session holds formatted text + 3 entities.
        {
            let s = runtime.session().read().await;
            let m = s
                .message(FullMsgId::new(self_peer, msg1_id))
                .ok_or("step 2: sent formatted message not found in Session")?;
            let t = m.text().ok_or("step 2: sent message missing text")?;
            if t.text != msg1_text || t.entities.len() < 3 {
                return Err(format!(
                    "step 2: sent message mismatch text={:?} entities={}",
                    t.text,
                    t.entities.len()
                ));
            }
        }
        println!("  [step 2A] Sent formatted message id={} (3 entities verified)", msg1_id.0);

        // 2B. Edit it via SessionRuntime::edit_message (Api::EditMessage -> applyUpdates).
        let edited_text = "Saved EDITED bold smoke";
        let _ = runtime
            .edit_message(tl_fns::messages::EditMessage {
                no_webpage: false,
                invert_media: false,
                peer: tl_enums::InputPeer::Self_,
                id: msg1_id.0 as i32,
                message: Some(edited_text.to_owned()),
                media: None,
                reply_markup: None,
                entities: Some(vec![tl_enums::MessageEntity::Bold(
                    tl_types::MessageEntityBold {
                        offset: 6,
                        length: 6,
                    },
                )]),
                schedule_date: None,
                schedule_repeat_period: None,
                quick_reply_shortcut_id: None,
                rich_message: None,
            })
            .await
            .map_err(|e| format!("step 2B: runtime.edit_message failed: {e}"))?;

        let edit_live = wait_until(Duration::from_secs(4), || {
            let session_arc = Arc::clone(runtime.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                let m = s.message(FullMsgId::new(self_peer, msg1_id))?;
                m.text()
                    .is_some_and(|t| t.text == edited_text && t.entities.len() == 1)
                    .then_some(())
            })
        })
        .await;
        if edit_live.is_err() {
            return Err("step 2B: editMessage change not reflected in Session after apply_updates".into());
        }
        println!("  [step 2B] Edit delivered to Session via UpdatesHandler OK");

        // 2C. Reply to it.
        let reply_text = "Saved reply smoke";
        let reply_hdr = tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
            reply_to_msg_id: msg1_id.0 as i32,
            top_msg_id: None,
            reply_to_peer_id: None,
            quote_text: None,
            quote_entities: None,
            quote_offset: None,
            monoforum_peer_id: None,
            todo_item_id: None,
            poll_option: None,
            quote_rich: None,
        });
        let send_reply = runtime
            .send_message_with_entities(
                self_peer,
                reply_text,
                Vec::new(),
                Some(reply_hdr),
                SendOptions::default(),
            )
            .await
            .map_err(|e| format!("step 2C: send reply failed: {e}"))?;
        let reply_id = extract_first_sent_msg_id(&send_reply)
            .ok_or("step 2C: missing server msg_id for reply")?;
        cleanup.track_saved(reply_id);
        {
            let s = runtime.session().read().await;
            let m = s
                .message(FullMsgId::new(self_peer, reply_id))
                .ok_or("step 2C: reply not found in Session")?;
            if m.reply_to.is_none() {
                return Err("step 2C: reply message missing reply_to header in Session".into());
            }
        }
        println!("  [step 2C] Reply sent id={} (reply_to verified)", reply_id.0);

        // 2D. Forward reply to Saved Messages (itself).
        let mut fwd_sender = MessageSender::default();
        let fwd_out = fwd_sender
            .forward_messages(
                &rpc,
                &[ForwardItem {
                    from_peer: tl_enums::InputPeer::Self_,
                    msg_id: reply_id,
                }],
                tl_enums::InputPeer::Self_,
                None,
                ForwardOptions::PreserveInfo,
                SendOptions::default(),
            )
            .await
            .map_err(|e| format!("step 2D: forward_messages failed: {e:?}"))?;
        let mut fwd_ids = Vec::new();
        for upd in fwd_out.updates {
            for id in extract_msg_ids_from_updates(&upd) {
                cleanup.track_saved(id);
                fwd_ids.push(id);
            }
            runtime
                .apply_updates(upd)
                .await
                .map_err(|e| format!("step 2D: runtime.apply_updates(forward) failed: {e}"))?;
        }
        let fwd_id = *fwd_ids
            .first()
            .ok_or("step 2D: forward_messages returned no server msg_id")?;
        let fwd_live = wait_until(Duration::from_secs(4), || {
            let session_arc = Arc::clone(runtime.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                s.message(FullMsgId::new(self_peer, fwd_id)).map(|_| ())
            })
        })
        .await;
        if fwd_live.is_err() {
            return Err(format!(
                "step 2D: forwarded message id={} not reflected in Session",
                fwd_id.0
            ));
        }
        println!("  [step 2D] Forwarded id={} delivered to Session via UpdatesHandler OK", fwd_id.0);

        // 2E. React to msg1 (note: Saved Messages reactions are Premium-only tags on Telegram;
        //     non-Premium accounts receive 403 PREMIUM_ACCOUNT_REQUIRED, so we verify live
        //     reactions in the megagroup in Step 3 as well).
        let mut saved_react_note = "OK";
        match rpc
            .invoke(&tl_fns::messages::SendReaction {
                big: false,
                add_to_recent: false,
                peer: tl_enums::InputPeer::Self_,
                msg_id: msg1_id.0 as i32,
                reaction: Some(vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                    emoticon: "\u{1F44D}".to_owned(),
                })]),
            })
            .await
        {
            Ok(react_upd) => {
                runtime
                    .apply_updates(react_upd)
                    .await
                    .map_err(|e| format!("step 2E: runtime.apply_updates(react) failed: {e}"))?;
                let react_live = wait_until(Duration::from_secs(4), || {
                    let session_arc = Arc::clone(runtime.session());
                    Box::pin(async move {
                        let s = session_arc.read().await;
                        let m = s.message(FullMsgId::new(self_peer, msg1_id))?;
                        m.reactions
                            .as_ref()
                            .is_some_and(|r| !r.results.is_empty())
                            .then_some(())
                    })
                })
                .await;
                if react_live.is_err() {
                    return Err(
                        "step 2E: reaction not reflected in Session after apply_updates".into(),
                    );
                }
                println!("  [step 2E] Saved Messages reaction delivered to Session OK");
            }
            Err(InvocationError::Rpc(ref r)) if r.name == "PREMIUM_ACCOUNT_REQUIRED" => {
                saved_react_note = "403 PREMIUM_ACCOUNT_REQUIRED (Saved Messages tags require Premium; megagroup reaction tested in step 3)";
                println!(
                    "  [step 2E] Saved Messages sendReaction returned 403 PREMIUM_ACCOUNT_REQUIRED (expected for non-Premium account); continuing to pin/unpin/delete"
                );
            }
            Err(e) => {
                return Err(check_flood_wait_inv("step 2E", "messages.sendReaction", &e));
            }
        }

        // 2F. Pin then unpin msg1.
        let pin_upd = rpc
            .invoke(&tl_fns::messages::UpdatePinnedMessage {
                silent: true,
                unpin: false,
                pm_oneside: false,
                peer: tl_enums::InputPeer::Self_,
                id: msg1_id.0 as i32,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 2F", "messages.updatePinnedMessage(pin)", &e))?;
        runtime
            .apply_updates(pin_upd)
            .await
            .map_err(|e| format!("step 2F: runtime.apply_updates(pin) failed: {e}"))?;
        let pin_live = wait_until(Duration::from_secs(4), || {
            let session_arc = Arc::clone(runtime.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                let m = s.message(FullMsgId::new(self_peer, msg1_id))?;
                m.is_pinned().then_some(())
            })
        })
        .await;
        if pin_live.is_err() {
            return Err("step 2F: pin not reflected in Session after apply_updates".into());
        }

        let unpin_upd = rpc
            .invoke(&tl_fns::messages::UpdatePinnedMessage {
                silent: true,
                unpin: true,
                pm_oneside: false,
                peer: tl_enums::InputPeer::Self_,
                id: msg1_id.0 as i32,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 2F", "messages.updatePinnedMessage(unpin)", &e))?;
        runtime
            .apply_updates(unpin_upd)
            .await
            .map_err(|e| format!("step 2F: runtime.apply_updates(unpin) failed: {e}"))?;
        let unpin_live = wait_until(Duration::from_secs(4), || {
            let session_arc = Arc::clone(runtime.session());
            Box::pin(async move {
                let s = session_arc.read().await;
                let m = s.message(FullMsgId::new(self_peer, msg1_id))?;
                (!m.is_pinned()).then_some(())
            })
        })
        .await;
        if unpin_live.is_err() {
            return Err("step 2F: unpin not reflected in Session after apply_updates".into());
        }
        println!("  [step 2F] Pin & unpin delivered to Session via UpdatesHandler OK");

        // 2G. Delete msg1, reply_id, fwd_id via SessionRuntime::delete_messages.
        let to_del = vec![msg1_id, reply_id, fwd_id];
        let _affected = runtime
            .delete_messages(self_peer, to_del.clone(), true)
            .await
            .map_err(|e| format!("step 2G: runtime.delete_messages failed: {e}"))?;
        cleanup.untrack_saved(&to_del);

        let del_live = wait_until(Duration::from_secs(4), || {
            let session_arc = Arc::clone(runtime.session());
            let ids = to_del.clone();
            Box::pin(async move {
                let s = session_arc.read().await;
                ids.iter()
                    .all(|&id| s.message(FullMsgId::new(self_peer, id)).is_none())
                    .then_some(())
            })
        })
        .await;
        if del_live.is_err() {
            return Err("step 2G: deleted messages still present in Session".into());
        }
        println!("  [step 2G] Delete delivered to Session via delete_messages OK");

        // Drain diagnostics from change/raw channels.
        let mut change_count = 0usize;
        while changes_rx.try_recv().is_ok() {
            change_count += 1;
        }
        let mut push_count = 0usize;
        if let Some(ref mut rx) = raw_push_rx {
            while rx.try_recv().is_ok() {
                push_count += 1;
            }
        }
        Ok(format!(
            "send/edit/reply/fwd/pin/unpin/del OK, saved_react={saved_react_note} (changes={change_count}, raw_pushes={push_count})"
        ))
    }
    .await;
    step_results.push(("Step 2 (Saved Messages + live pump)", step2_res));

    // =========================================================================
    // Step 3: Create supergroup (megagroup) and broadcast channel with only this
    //         account; post, edit, delete; read history; check unread counters and
    //         updates.getChannelDifference; delete both.
    // =========================================================================
    let step3_res: Result<String, String> = async {
        println!("[step 3] Testing supergroup (megagroup) and broadcast channel...");
        for (label, is_megagroup, is_broadcast) in
            [("megagroup", true, false), ("broadcast", false, true)]
        {
            let created = rpc
                .invoke(&tl_fns::channels::CreateChannel {
                    broadcast: is_broadcast,
                    megagroup: is_megagroup,
                    for_import: false,
                    forum: false,
                    title: format!("Rustgram Smoke {label}"),
                    about: "Temporary automated test".to_owned(),
                    geo_point: None,
                    address: None,
                    ttl_period: None,
                })
                .await
                .map_err(|e| {
                    check_flood_wait_inv("step 3", &format!("createChannel({label})"), &e)
                })?;

            let (ch_id, ch_access_hash) = {
                let mut s = runtime.session().write().await;
                let ch_id = extract_chat_or_channel_from_updates(&created, &mut s)
                    .1
                    .ok_or_else(|| {
                        format!("step 3: createChannel({label}) returned no ChannelId")
                    })?;
                let hash = s.channel(ch_id).map_or(0, |c| c.access_hash as i64);
                (ch_id, hash)
            };
            cleanup.track_channel(ch_id, ch_access_hash, label);
            let _ = runtime.apply_updates(created).await;
            let ch_peer = PeerId::from_channel(ch_id);
            let _ = runtime.set_active_channel(ch_id, true).await;
            println!("  [step 3/{label}] Created channel id={}", ch_id.0);

            // Post 2 messages.
            let post1 = runtime
                .send_message(ch_peer, format!("{label} post 1"), SendOptions::default())
                .await
                .map_err(|e| format!("step 3/{label}: post 1 failed: {e}"))?;
            let post1_id = extract_first_sent_msg_id(&post1)
                .ok_or_else(|| format!("step 3/{label}: missing msg_id for post 1"))?;
            let post2 = runtime
                .send_message(ch_peer, format!("{label} post 2"), SendOptions::default())
                .await
                .map_err(|e| format!("step 3/{label}: post 2 failed: {e}"))?;
            let post2_id = extract_first_sent_msg_id(&post2)
                .ok_or_else(|| format!("step 3/{label}: missing msg_id for post 2"))?;
            println!(
                "  [step 3/{label}] Posted msg_ids={}, {}",
                post1_id.0, post2_id.0
            );

            // Edit post 1 via SessionRuntime::edit_message (handles 500 RPC_CALL_FAIL ->
            // retry -> MESSAGE_NOT_MODIFIED gracefully and syncs Session).
            let input_ch_peer = {
                let s = runtime.session().read().await;
                input_peer_for(&s, ch_peer)
            };
            let edited_text = format!("{label} post 1 EDITED");
            let edit_res = runtime
                .edit_message(tl_fns::messages::EditMessage {
                    no_webpage: false,
                    invert_media: false,
                    peer: input_ch_peer.clone(),
                    id: post1_id.0 as i32,
                    message: Some(edited_text.clone()),
                    media: None,
                    reply_markup: None,
                    entities: None,
                    schedule_date: None,
                    schedule_repeat_period: None,
                    quick_reply_shortcut_id: None,
                    rich_message: None,
                })
                .await
                .map_err(|e| format!("step 3/{label}: runtime.edit_message failed: {e}"))?;
            println!(
                "  [step 3/{label}] Edited post1 id={} OK (direct_updates={})",
                post1_id.0,
                edit_res.is_some()
            );

            // Also verify live reaction delivery via UpdatesHandler on post1 (works on non-Premium).
            let react_upd = rpc
                .invoke(&tl_fns::messages::SendReaction {
                    big: false,
                    add_to_recent: false,
                    peer: input_ch_peer.clone(),
                    msg_id: post1_id.0 as i32,
                    reaction: Some(vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "\u{1F44D}".to_owned(),
                    })]),
                })
                .await
                .map_err(|e| {
                    check_flood_wait_inv("step 3", &format!("sendReaction({label})"), &e)
                })?;
            runtime
                .apply_updates(react_upd)
                .await
                .map_err(|e| format!("step 3/{label}: apply_updates(react) failed: {e}"))?;
            {
                let s = runtime.session().read().await;
                let has_react = s
                    .message(FullMsgId::new(ch_peer, post1_id))
                    .and_then(|m| m.reactions.as_ref())
                    .is_some_and(|r| !r.results.is_empty());
                if !has_react {
                    return Err(format!(
                        "step 3/{label}: reaction not reflected in Session after apply_updates"
                    ));
                }
            }
            println!(
                "  [step 3/{label}] Reaction on post1 id={} verified in Session OK",
                post1_id.0
            );

            // Read history via SessionRuntime::open_history (also initializes channel pts).
            let slice = runtime
                .open_history(ch_peer, MsgId::ZERO, tg_api::LoadDirection::Before)
                .await
                .map_err(|e| format!("step 3/{label}: open_history failed: {e}"))?
                .ok_or_else(|| format!("step 3/{label}: open_history returned None"))?;
            let hist_pts = slice.channel_pts.unwrap_or(1);
            let saw_edited = {
                let s = runtime.session().read().await;
                s.message(FullMsgId::new(ch_peer, post1_id))
                    .and_then(|m| m.text())
                    .is_some_and(|t| t.text == edited_text)
            };
            if !saw_edited {
                return Err(format!(
                    "step 3/{label}: open_history did not show edited text"
                ));
            }

            // Mark read & check unread counter.
            let _ = runtime
                .mark_read(ch_peer, post2_id, true)
                .await
                .map_err(|e| format!("step 3/{label}: mark_read failed: {e}"))?;
            let unread_after = {
                let s = runtime.session().read().await;
                s.history(ch_peer).and_then(|h| h.unread_count).unwrap_or(0)
            };
            if unread_after != 0 {
                return Err(format!(
                    "step 3/{label}: unread_count after mark_read is {unread_after}, expected 0"
                ));
            }

            // Check updates.getChannelDifference starting from pts=1 (and from hist_pts).
            let input_ch = {
                let s = runtime.session().read().await;
                input_channel_for(&s, ch_id)
            };
            let diff1 = rpc
                .invoke(&tl_fns::updates::GetChannelDifference {
                    force: false,
                    channel: input_ch.clone(),
                    filter: tl_enums::ChannelMessagesFilter::Empty,
                    pts: 1,
                    limit: 50,
                })
                .await
                .map_err(|e| {
                    check_flood_wait_inv(
                        "step 3",
                        &format!("getChannelDifference({label}, pts=1)"),
                        &e,
                    )
                })?;
            let diff2 = rpc
                .invoke(&tl_fns::updates::GetChannelDifference {
                    force: false,
                    channel: input_ch.clone(),
                    filter: tl_enums::ChannelMessagesFilter::Empty,
                    pts: hist_pts.max(1),
                    limit: 50,
                })
                .await
                .map_err(|e| {
                    check_flood_wait_inv(
                        "step 3",
                        &format!("getChannelDifference({label}, pts={hist_pts})"),
                        &e,
                    )
                })?;
            println!(
                "  [step 3/{label}] getChannelDifference OK: from_1={:?}, from_{hist_pts}={:?}",
                std::mem::discriminant(&diff1),
                std::mem::discriminant(&diff2)
            );

            // Delete both messages via SessionRuntime::delete_messages (channels.deleteMessages).
            let _del_aff = runtime
                .delete_messages(ch_peer, vec![post1_id, post2_id], true)
                .await
                .map_err(|e| format!("step 3/{label}: delete_messages failed: {e}"))?;
            {
                let s = runtime.session().read().await;
                if s.message(FullMsgId::new(ch_peer, post1_id)).is_some()
                    || s.message(FullMsgId::new(ch_peer, post2_id)).is_some()
                {
                    return Err(format!("step 3/{label}: deleted messages still in Session"));
                }
            }

            // Delete channel via channels.deleteChannel and apply its updates so Session marks it forbidden.
            let _ = runtime.set_active_channel(ch_id, false).await;
            let del_ch_upd = rpc
                .invoke(&tl_fns::channels::DeleteChannel { channel: input_ch })
                .await
                .map_err(|e| {
                    check_flood_wait_inv("step 3", &format!("deleteChannel({label})"), &e)
                })?;
            let _ = runtime.apply_updates(del_ch_upd).await;
            cleanup.untrack_channel(ch_id);
            println!(
                "  [step 3/{label}] Deleted messages and channel id={}",
                ch_id.0
            );
        }
        Ok(
            "megagroup + broadcast create/post/edit/history/read/getChannelDifference/delete OK"
                .into(),
        )
    }
    .await;
    step_results.push(("Step 3 (megagroup & broadcast channel)", step3_res));

    // =========================================================================
    // Step 4: Upload small generated PNG (sendMedia photo) & ~3 MB generated file
    //         (big-file path, upload.saveBigFilePart) to Saved Messages, download
    //         both back, compare SHA-256; keep 3 MB doc for Step 5B, then delete all.
    // =========================================================================
    let invoker = LiveMediaInvoker::new(rpc.clone());
    let cancel = CancelToken::new();
    let mut big_doc_for_step5: Option<(tg_data::DocumentData, String, MsgId)> = None;

    let step4_res: Result<String, String> = async {
        println!(
            "[step 4] Uploading small PNG (photo + doc) and ~3 MB big file to Saved Messages..."
        );
        // 4A. Small generated PNG via sendMedia(InputMedia::UploadedPhoto).
        let png_bytes = generate_test_png(64, 64);
        let png_sha = sha256_hex(&png_bytes);
        let uploaded_png = upload_bytes(
            &invoker,
            "rustgram_smoke_photo.png",
            &png_bytes,
            UploadConfig::default(),
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 4A: upload small PNG failed: {e}"))?;
        assert!(
            matches!(uploaded_png, UploadedInputFile::Small { .. }),
            "step 4A: expected small-file upload path"
        );
        let photo_upd = rpc
            .invoke(&tl_fns::messages::SendMedia {
                silent: true,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: tl_enums::InputPeer::Self_,
                reply_to: None,
                media: tl_enums::InputMedia::UploadedPhoto(tl_types::InputMediaUploadedPhoto {
                    spoiler: false,
                    live_photo: false,
                    file: uploaded_to_tl_input_file(&uploaded_png),
                    stickers: None,
                    ttl_seconds: None,
                    video: None,
                }),
                message: "smoke-photo-png".to_owned(),
                random_id: rng.next_u64() as i64,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 4A", "sendMedia(photo)", &e))?;
        let photo_msg_ids = extract_msg_ids_from_updates(&photo_upd);
        for &id in &photo_msg_ids {
            cleanup.track_saved(id);
        }
        let photo_msg_id = *photo_msg_ids
            .first()
            .ok_or("step 4A: sendMedia(photo) returned no msg_id")?;
        let _ = runtime.apply_updates(photo_upd).await;
        // Refresh Saved Messages history if needed so Session has the photo metadata.
        let _ = runtime
            .open_history(self_peer, MsgId::ZERO, tg_api::LoadDirection::Before)
            .await
            .map_err(|e| format!("step 4A: open_history(self) failed: {e}"))?;
        let photo_data = {
            let s = runtime.session().read().await;
            let m = s
                .message(FullMsgId::new(self_peer, photo_msg_id))
                .ok_or("step 4A: photo message not in Session")?;
            match m.media() {
                Some(MessageMedia::Photo(p)) => p.clone(),
                other => {
                    return Err(format!(
                        "step 4A: expected MessageMedia::Photo, got {other:?}"
                    ));
                }
            }
        };
        let best_size = photo_data
            .sizes
            .iter()
            .filter(|sz| !sz.kind.is_empty() && sz.kind != "i")
            .max_by_key(|sz| (i64::from(sz.width) * i64::from(sz.height), sz.size))
            .ok_or("step 4A: photo has no downloadable PhotoSize")?;
        let dl_photo = download_file(
            &invoker,
            DownloadRequest::new(
                photo_data.dc_id,
                TransferLocation::Photo {
                    id: photo_data.id.0,
                    access_hash: photo_data.access_hash,
                    file_reference: photo_data.file_reference.clone(),
                    thumb_size: best_size.kind.clone(),
                },
                best_size.size.max(0) as u64,
            ),
            None,
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 4A: download photo failed: {e}"))?;
        if dl_photo.len() < 16 || dl_photo[0..2] != [0xFF, 0xD8] {
            return Err(format!(
                "step 4A: downloaded photo ({} bytes) is not a valid JPEG",
                dl_photo.len()
            ));
        }
        println!(
            "  [step 4A] Photo uploaded ({} B PNG) & downloaded back ({} B JPEG, size='{}')",
            png_bytes.len(),
            dl_photo.len(),
            best_size.kind
        );

        // 4A-doc: Also send the same small PNG as UploadedDocument to verify exact SHA-256 on small-file path.
        let uploaded_png_doc = upload_bytes(
            &invoker,
            "rustgram_smoke_small.png",
            &png_bytes,
            UploadConfig::default(),
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 4A-doc: upload small PNG doc failed: {e}"))?;
        let png_doc_upd = rpc
            .invoke(&tl_fns::messages::SendMedia {
                silent: true,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: tl_enums::InputPeer::Self_,
                reply_to: None,
                media: tl_enums::InputMedia::UploadedDocument(
                    tl_types::InputMediaUploadedDocument {
                        nosound_video: false,
                        force_file: true,
                        spoiler: false,
                        file: uploaded_to_tl_input_file(&uploaded_png_doc),
                        thumb: None,
                        mime_type: "image/png".to_owned(),
                        attributes: vec![tl_enums::DocumentAttribute::Filename(
                            tl_types::DocumentAttributeFilename {
                                file_name: "rustgram_smoke_small.png".to_owned(),
                            },
                        )],
                        stickers: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                ),
                message: "smoke-small-png-doc".to_owned(),
                random_id: rng.next_u64() as i64,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 4A-doc", "sendMedia(small doc)", &e))?;
        let png_doc_ids = extract_msg_ids_from_updates(&png_doc_upd);
        for &id in &png_doc_ids {
            cleanup.track_saved(id);
        }
        let png_doc_msg_id = *png_doc_ids.first().ok_or("step 4A-doc: missing msg_id")?;
        let _ = runtime.apply_updates(png_doc_upd).await;
        let _ = runtime
            .open_history(self_peer, MsgId::ZERO, tg_api::LoadDirection::Before)
            .await
            .map_err(|e| format!("step 4A-doc: open_history failed: {e}"))?;
        let small_doc = {
            let s = runtime.session().read().await;
            let m = s
                .message(FullMsgId::new(self_peer, png_doc_msg_id))
                .ok_or("step 4A-doc: doc msg not in Session")?;
            match m.media() {
                Some(MessageMedia::Document(d)) => d.clone(),
                other => return Err(format!("step 4A-doc: expected Document, got {other:?}")),
            }
        };
        let dl_png_doc = download_file(
            &invoker,
            DownloadRequest::new(
                small_doc.dc_id,
                TransferLocation::Document {
                    id: small_doc.id.0,
                    access_hash: small_doc.access_hash,
                    file_reference: small_doc.file_reference.clone(),
                    thumb_size: String::new(),
                },
                small_doc.size as u64,
            ),
            None,
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 4A-doc: download small PNG doc failed: {e}"))?;
        if sha256_hex(&dl_png_doc) != png_sha {
            return Err("step 4A-doc: small PNG document SHA-256 mismatch".into());
        }
        println!("  [step 4A-doc] Small PNG doc SHA-256 exact match OK");

        // 4B. ~3 MB generated file through big-file path (`upload.saveBigFilePart`).
        let big_bytes = generate_big_payload();
        let big_sha = sha256_hex(&big_bytes);
        let big_cfg = UploadConfig {
            big_file_threshold: 1024 * 1024,
            max_sessions: 4,
            ..UploadConfig::default()
        };
        let uploaded_big = upload_bytes(
            &invoker,
            "rustgram_smoke_3mb.bin",
            &big_bytes,
            big_cfg,
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 4B: upload 3 MB big file failed: {e}"))?;
        assert!(
            matches!(uploaded_big, UploadedInputFile::Big { .. }),
            "step 4B: expected big-file upload path (saveBigFilePart)"
        );
        let big_upd = rpc
            .invoke(&tl_fns::messages::SendMedia {
                silent: true,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: tl_enums::InputPeer::Self_,
                reply_to: None,
                media: tl_enums::InputMedia::UploadedDocument(
                    tl_types::InputMediaUploadedDocument {
                        nosound_video: false,
                        force_file: true,
                        spoiler: false,
                        file: uploaded_to_tl_input_file(&uploaded_big),
                        thumb: None,
                        mime_type: "application/octet-stream".to_owned(),
                        attributes: vec![tl_enums::DocumentAttribute::Filename(
                            tl_types::DocumentAttributeFilename {
                                file_name: "rustgram_smoke_3mb.bin".to_owned(),
                            },
                        )],
                        stickers: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                ),
                message: "smoke-big-3mb".to_owned(),
                random_id: rng.next_u64() as i64,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 4B", "sendMedia(3 MB file)", &e))?;
        let big_ids = extract_msg_ids_from_updates(&big_upd);
        for &id in &big_ids {
            cleanup.track_saved(id);
        }
        let big_msg_id = *big_ids.first().ok_or("step 4B: missing msg_id")?;
        let _ = runtime.apply_updates(big_upd).await;
        let _ = runtime
            .open_history(self_peer, MsgId::ZERO, tg_api::LoadDirection::Before)
            .await
            .map_err(|e| format!("step 4B: open_history failed: {e}"))?;
        let big_doc = {
            let s = runtime.session().read().await;
            let m = s
                .message(FullMsgId::new(self_peer, big_msg_id))
                .ok_or("step 4B: big doc msg not in Session")?;
            match m.media() {
                Some(MessageMedia::Document(d)) => d.clone(),
                other => return Err(format!("step 4B: expected Document, got {other:?}")),
            }
        };
        let dl_big = download_file(
            &invoker,
            DownloadRequest::new(
                big_doc.dc_id,
                TransferLocation::Document {
                    id: big_doc.id.0,
                    access_hash: big_doc.access_hash,
                    file_reference: big_doc.file_reference.clone(),
                    thumb_size: String::new(),
                },
                big_doc.size as u64,
            ),
            None,
            &cancel,
            |_| {},
        )
        .await
        .map_err(|e| format!("step 4B: download 3 MB file failed: {e}"))?;
        let dl_big_sha = sha256_hex(&dl_big);
        if dl_big_sha != big_sha {
            return Err(format!(
                "step 4B: 3 MB SHA-256 mismatch (sent={big_sha}, got={dl_big_sha})"
            ));
        }
        println!(
            "  [step 4B] 3 MB big file ({} B) uploaded & downloaded with exact SHA-256 match!",
            big_bytes.len()
        );
        big_doc_for_step5 = Some((*big_doc, big_sha, big_msg_id));

        // Delete the photo and small PNG doc now; big_msg_id will be deleted right after Step 5B.
        let del_now = vec![photo_msg_id, png_doc_msg_id];
        let sender = MessageSender::default();
        let aff = sender
            .delete_messages(&rpc, tl_enums::InputPeer::Self_, del_now.clone(), true)
            .await
            .map_err(|e| check_flood_wait_api("step 4", "delete photo/small doc", &e))?;
        let _ = runtime
            .apply_affected_history(None, aff.pts, aff.pts_count)
            .await;
        cleanup.untrack_saved(&del_now);

        Ok("photo(JPEG) + small PNG doc(SHA-256) + 3 MB big file(SHA-256) OK".into())
    }
    .await;
    step_results.push(("Step 4 (photo + 3 MB upload/download/SHA-256)", step4_res));

    // =========================================================================
    // Step 5: Drop TCP connection while idle and while a download runs; confirm
    //         getDifference recovers and nothing is lost or duplicated.
    // =========================================================================
    let step5_res: Result<String, String> = async {
        println!("[step 5] Testing TCP drop while idle + getDifference, and TCP drop mid-download...");
        // 5A. Drop TCP while idle, create an unseen message via raw RPC so runtime must
        //     recover it via updates.getDifference, verify present exactly once.
        let idle_connects_before = guard.connect_count();
        guard.force_drop_active_connections();
        tokio::time::sleep(Duration::from_millis(400)).await;
        rpc.ping()
            .await
            .map_err(|e| check_flood_wait_inv("step 5A", "post-drop ping", &e))?;
        let idle_connects_after = guard.connect_count();
        if idle_connects_after <= idle_connects_before {
            return Err(format!(
                "step 5A: expected connect_count to increase after idle drop ({idle_connects_before} -> {idle_connects_after})"
            ));
        }

        // Send a message bypassing SessionRuntime, then force on_reconnect() -> getDifference.
        let gap_text = format!("idle-drop-diff-recovery-{}", rng.next_u64() % 1_000_000);
        let raw_send = rpc
            .invoke(&tl_fns::messages::SendMessage {
                no_webpage: false,
                silent: true,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: tl_enums::InputPeer::Self_,
                reply_to: None,
                message: gap_text.clone(),
                random_id: rng.next_u64() as i64,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
                rich_message: None,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 5A", "raw sendMessage for gap", &e))?;
        let gap_ids = extract_msg_ids_from_updates(&raw_send);
        for &id in &gap_ids {
            cleanup.track_saved(id);
        }
        let gap_msg_id = *gap_ids.first().ok_or("step 5A: raw send returned no msg_id")?;

        // Drop again while idle and run on_reconnect() to trigger getDifference.
        guard.force_drop_active_connections();
        tokio::time::sleep(Duration::from_millis(300)).await;
        runtime
            .on_reconnect()
            .await
            .map_err(|e| format!("step 5A: on_reconnect(getDifference) failed: {e}"))?;

        wait_until(STEP_WAIT_TIMEOUT, || {
            let session_arc = Arc::clone(runtime.session());
            let expected = gap_text.clone();
            Box::pin(async move {
                let s = session_arc.read().await;
                let matches = peer_messages(&s, self_peer)
                    .into_iter()
                    .filter(|m| m.text().is_some_and(|t| t.text == expected))
                    .count();
                (matches > 0).then_some(matches)
            })
        })
        .await
        .and_then(|count| {
            if count == 1 {
                Ok(())
            } else {
                Err(format!("expected 1 copy of gap message, found {count}"))
            }
        })
        .map_err(|e| format!("step 5A: getDifference recovery check failed: {e}"))?;

        // Delete the gap message.
        let sender = MessageSender::default();
        let aff = sender
            .delete_messages(&rpc, tl_enums::InputPeer::Self_, vec![gap_msg_id], true)
            .await
            .map_err(|e| check_flood_wait_api("step 5A", "delete gap msg", &e))?;
        let _ = runtime
            .apply_affected_history(None, aff.pts, aff.pts_count)
            .await;
        cleanup.untrack_saved(&[gap_msg_id]);
        println!(
            "  [step 5A] Idle TCP drop + reconnect ({idle_connects_before} -> {idle_connects_after}) + getDifference recovered 1/1 message with zero duplicates."
        );

        // 5B. Drop TCP connection WHILE a ~3 MB download is running.
        let (big_doc, big_sha, big_msg_id) = big_doc_for_step5
            .take()
            .ok_or("step 5B: skipped because step 4B did not produce 3 MB document")?;
        let dl_connects_before = guard.connect_count();
        let dropped_mid_dl = Arc::new(AtomicBool::new(false));
        let drop_flag = Arc::clone(&dropped_mid_dl);
        let guard_for_cb = Arc::clone(&guard);
        let dl_after_drop = download_file(
            &invoker,
            DownloadRequest::new(
                big_doc.dc_id,
                TransferLocation::Document {
                    id: big_doc.id.0,
                    access_hash: big_doc.access_hash,
                    file_reference: big_doc.file_reference.clone(),
                    thumb_size: String::new(),
                },
                big_doc.size as u64,
            ),
            None,
            &cancel,
            move |prog| {
                if prog.downloaded >= 256 * 1024 && !drop_flag.swap(true, Ordering::AcqRel) {
                    println!(
                        "  [step 5B] Triggering forced TCP drop mid-download at {}/{} bytes...",
                        prog.downloaded, prog.total
                    );
                    guard_for_cb.force_drop_active_connections();
                }
            },
        )
        .await
        .map_err(|e| format!("step 5B: download failed after mid-download TCP drop: {e}"))?;

        let dl_connects_after = guard.connect_count();
        if !dropped_mid_dl.load(Ordering::Acquire) {
            return Err("step 5B: mid-download drop callback never fired".into());
        }
        if dl_connects_after <= dl_connects_before {
            return Err(format!(
                "step 5B: expected connect_count increase after mid-download drop ({dl_connects_before} -> {dl_connects_after})"
            ));
        }
        let dl_sha = sha256_hex(&dl_after_drop);
        if dl_sha != big_sha {
            return Err(format!(
                "step 5B: post-drop downloaded SHA-256 mismatch (expected={big_sha}, got={dl_sha})"
            ));
        }

        // Delete the 3 MB message from Saved Messages now.
        let aff_big = sender
            .delete_messages(&rpc, tl_enums::InputPeer::Self_, vec![big_msg_id], true)
            .await
            .map_err(|e| check_flood_wait_api("step 5B", "delete 3 MB msg", &e))?;
        let _ = runtime
            .apply_affected_history(None, aff_big.pts, aff_big.pts_count)
            .await;
        cleanup.untrack_saved(&[big_msg_id]);
        println!(
            "  [step 5B] Mid-download TCP drop recovered ({dl_connects_before} -> {dl_connects_after}), exact SHA-256 match!"
        );

        Ok(format!(
            "idle drop + getDifference OK; mid-download drop ({dl_connects_before}->{dl_connects_after}) SHA-256 OK"
        ))
    }
    .await;
    step_results.push(("Step 5 (TCP drop idle & mid-download)", step5_res));

    // =========================================================================
    // Step 6: Sticker / emoji / reaction lists with hashes (messages.getAllStickers,
    //         getAvailableReactions, help.getAppConfig) twice -> second call must
    //         get NotModified.
    // =========================================================================
    let step6_res: Result<String, String> = async {
        println!("[step 6] Testing hash-cached lists (getAllStickers, getEmojiStickers, getAvailableReactions, help.getAppConfig) twice -> NotModified...");
        let now_ms = now_unix_ms();
        let now_secs = (now_ms / 1000) as i32;

        // 6A. messages.getAllStickers via StickersSyncManager hash + raw second call check.
        let mut stickers_mgr = StickersSyncManager::new();
        let first_stickers = rpc
            .invoke(&tl_fns::messages::GetAllStickers { hash: 0 })
            .await
            .map_err(|e| check_flood_wait_inv("step 6A", "messages.getAllStickers#1", &e))?;
        let (stickers_server_hash, stickers_count) = match first_stickers {
            tl_enums::messages::AllStickers::AllStickers(data) => {
                let _ = stickers_mgr.apply_all_stickers(
                    StickersType::Stickers,
                    &data.sets,
                    data.hash,
                    now_secs,
                );
                (data.hash, data.sets.len())
            }
            tl_enums::messages::AllStickers::NotModified => {
                return Err("step 6A: getAllStickers(hash=0) unexpectedly returned NotModified".into());
            }
        };
        let computed_stickers_hash = stickers_mgr.count_stickers_hash(true) as i64;
        println!(
            "  [step 6A] getAllStickers#1: sets={stickers_count}, server_hash={stickers_server_hash}, computed_hash={computed_stickers_hash}"
        );
        let second_stickers = rpc
            .invoke(&tl_fns::messages::GetAllStickers {
                hash: computed_stickers_hash,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 6A", "messages.getAllStickers#2", &e))?;
        if !matches!(second_stickers, tl_enums::messages::AllStickers::NotModified) {
            return Err(format!(
                "step 6A: getAllStickers#2 with computed hash {computed_stickers_hash} (server_hash={stickers_server_hash}) did NOT return NotModified"
            ));
        }
        println!("  [step 6A] getAllStickers#2 returned NotModified OK");

        // 6B. messages.getEmojiStickers + messages.getEmojiGroups twice -> NotModified.
        let first_emoji = rpc
            .invoke(&tl_fns::messages::GetEmojiStickers { hash: 0 })
            .await
            .map_err(|e| check_flood_wait_inv("step 6B", "messages.getEmojiStickers#1", &e))?;
        let emoji_server_hash = match first_emoji {
            tl_enums::messages::AllStickers::AllStickers(data) => {
                let _ = stickers_mgr.apply_all_stickers(
                    StickersType::Emoji,
                    &data.sets,
                    data.hash,
                    now_secs,
                );
                data.hash
            }
            tl_enums::messages::AllStickers::NotModified => {
                return Err("step 6B: getEmojiStickers(hash=0) returned NotModified".into());
            }
        };
        let computed_emoji_hash = stickers_mgr.count_custom_emoji_hash(true) as i64;
        let second_emoji = rpc
            .invoke(&tl_fns::messages::GetEmojiStickers {
                hash: computed_emoji_hash,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 6B", "messages.getEmojiStickers#2", &e))?;
        if !matches!(second_emoji, tl_enums::messages::AllStickers::NotModified) {
            return Err(format!(
                "step 6B: getEmojiStickers#2 with computed hash {computed_emoji_hash} (server_hash={emoji_server_hash}) did NOT return NotModified"
            ));
        }
        let mut emoji_sync = EmojiStatusesSync::default();
        emoji_sync
            .refresh_emoji_groups(&rpc)
            .await
            .map_err(|e| check_flood_wait_api("step 6B", "messages.getEmojiGroups#1", &e))?;
        let second_groups = rpc
            .invoke(&tl_fns::messages::GetEmojiGroups {
                hash: emoji_sync.emoji_groups.hash,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 6B", "messages.getEmojiGroups#2", &e))?;
        if !matches!(second_groups, tl_enums::messages::EmojiGroups::NotModified) {
            return Err(format!(
                "step 6B: getEmojiGroups#2 with hash {} did NOT return NotModified",
                emoji_sync.emoji_groups.hash
            ));
        }
        println!("  [step 6B] getEmojiStickers#2 + getEmojiGroups#2 returned NotModified OK");

        // 6C. messages.getAvailableReactions via ReactionsSync + raw NotModified verification.
        let mut reactions_sync = ReactionsSync::default();
        let _ = reactions_sync
            .refresh_available(&rpc, now_ms, true)
            .await
            .map_err(|e| check_flood_wait_api("step 6C", "messages.getAvailableReactions#1", &e))?;
        let second_reactions = rpc
            .invoke(&tl_fns::messages::GetAvailableReactions {
                hash: reactions_sync.available_hash,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 6C", "messages.getAvailableReactions#2", &e))?;
        if !matches!(
            second_reactions,
            tl_enums::messages::AvailableReactions::NotModified
        ) {
            return Err(format!(
                "step 6C: getAvailableReactions#2 with hash {} did NOT return NotModified",
                reactions_sync.available_hash
            ));
        }
        println!(
            "  [step 6C] getAvailableReactions#2 returned NotModified OK (count={}, hash={})",
            reactions_sync.available.len(),
            reactions_sync.available_hash
        );

        // 6D. help.getAppConfig via AppConfigSync + raw NotModified verification.
        let mut app_cfg_sync = AppConfigSync::default();
        let _ = app_cfg_sync
            .refresh(&rpc, now_ms, true)
            .await
            .map_err(|e| check_flood_wait_api("step 6D", "help.getAppConfig#1", &e))?;
        let second_app_cfg = rpc
            .invoke(&tl_fns::help::GetAppConfig {
                hash: app_cfg_sync.hash,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 6D", "help.getAppConfig#2", &e))?;
        if !matches!(second_app_cfg, tl_enums::help::AppConfig::NotModified) {
            return Err(format!(
                "step 6D: help.getAppConfig#2 with hash {} did NOT return NotModified",
                app_cfg_sync.hash
            ));
        }
        println!(
            "  [step 6D] help.getAppConfig#2 returned NotModified OK (keys={}, hash={})",
            app_cfg_sync.values.len(),
            app_cfg_sync.hash
        );

        Ok(format!(
            "All NotModified OK (stickers={}, reactions={}, app_config_keys={})",
            stickers_count,
            reactions_sync.available.len(),
            app_cfg_sync.values.len()
        ))
    }
    .await;
    step_results.push(("Step 6 (hash-cached NotModified lists)", step6_res));

    // =========================================================================
    // Guaranteed cleanup: delete any remaining Saved Messages & channels created
    // by this run, and shut down cleanly WITHOUT logging out the account.
    // =========================================================================
    println!("[cleanup] Ensuring all created Saved Messages and channels are deleted...");
    guard.set_block_connects(false);
    let mut cleanup_errors = Vec::new();
    if !cleanup.saved_msg_ids.is_empty() {
        let ids: Vec<i32> = cleanup
            .saved_msg_ids
            .iter()
            .filter_map(|m| m.as_i32())
            .collect();
        println!(
            "  [cleanup] Deleting {} remaining Saved Messages: {ids:?}",
            ids.len()
        );
        match rpc
            .invoke(&tl_fns::messages::DeleteMessages {
                revoke: true,
                id: ids.clone(),
            })
            .await
        {
            Ok(_) => {
                println!("  [cleanup] Deleted remaining Saved Messages OK.");
                cleanup.saved_msg_ids.clear();
            }
            Err(e) => {
                cleanup_errors.push(format!("failed to delete Saved Messages {ids:?}: {e}"));
            }
        }
    }
    for (ch_id, access_hash, label) in std::mem::take(&mut cleanup.channels) {
        println!(
            "  [cleanup] Deleting remaining {label} channel id={}",
            ch_id.0
        );
        let input_ch = tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
            channel_id: ch_id.0 as i64,
            access_hash,
        });
        match rpc
            .invoke(&tl_fns::channels::DeleteChannel { channel: input_ch })
            .await
        {
            Ok(_) => println!("  [cleanup] Deleted {label} channel id={} OK.", ch_id.0),
            Err(e) => cleanup_errors.push(format!("failed to delete {label} {}: {e}", ch_id.0)),
        }
    }

    runtime.shutdown_async().await;
    mgr.shutdown();

    println!("\n=== SINGLE-ACCOUNT TEST-DC SMOKE SUMMARY ===");
    let mut failures = Vec::new();
    for (name, res) in &step_results {
        match res {
            Ok(detail) => println!("  PASS | {name}: {detail}"),
            Err(err) => {
                println!("  FAIL | {name}: {err}");
                failures.push(format!("{name}: {err}"));
            }
        }
    }
    if cleanup_errors.is_empty() {
        println!(
            "  CLEANUP | PASS: all created Saved Messages, supergroup, and channel deleted (account NOT logged out)."
        );
    } else {
        for ce in &cleanup_errors {
            println!("  CLEANUP | ERROR: {ce}");
            failures.push(format!("cleanup: {ce}"));
        }
    }

    if failures.is_empty() {
        println!("\nALL SINGLE-ACCOUNT TEST-DC SMOKE STEPS PASSED!");
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn contains_update_matching<F>(updates: &tl_enums::Updates, mut pred: F) -> bool
where
    F: FnMut(&tl_enums::Update) -> bool,
{
    match updates {
        tl_enums::Updates::UpdateShort(s) => pred(&s.update),
        tl_enums::Updates::Updates(u) => u.updates.iter().any(&mut pred),
        tl_enums::Updates::Combined(c) => c.updates.iter().any(&mut pred),
        _ => false,
    }
}

fn describe_typing_updates(updates: &tl_enums::Updates) -> Vec<&'static str> {
    let mut found = Vec::new();
    let mut inspect = |u: &tl_enums::Update| match u {
        tl_enums::Update::UserTyping(_) => found.push("updateUserTyping"),
        tl_enums::Update::ChatUserTyping(_) => found.push("updateChatUserTyping"),
        tl_enums::Update::ChannelUserTyping(_) => found.push("updateChannelUserTyping"),
        _ => {}
    };
    match updates {
        tl_enums::Updates::UpdateShort(s) => inspect(&s.update),
        tl_enums::Updates::Updates(u) => {
            for item in &u.updates {
                inspect(item);
            }
        }
        tl_enums::Updates::Combined(c) => {
            for item in &c.updates {
                inspect(item);
            }
        }
        _ => {}
    }
    found
}

/// Runs the two-account logged-in smoke test suite (`--session-a <dir> --session-b <dir>`)
/// against Telegram's official TEST DCs using copied `tdata` working directories.
///
/// Safety guarantees:
/// - Refuses production DC addresses and any `help.getConfig` without `test_mode = true`.
/// - Never modifies either profile, never logs out or deletes either account, never touches
///   pre-existing chats, and never logs phone numbers, personal names, or API keys.
/// - Always deletes the A<->B private chat history (with `revoke: true`) and the created group.
pub async fn run_two_account_smoke(
    session_dir_a: PathBuf,
    session_dir_b: PathBuf,
) -> Result<(), String> {
    let creds = load_test_api_credentials()?;
    println!(
        "[init] Loaded TEST-ONLY API credentials from runtime source; opening session copies A={} B={}",
        session_dir_a.display(),
        session_dir_b.display()
    );

    let guard_a = Arc::new(TestDcGuardTransportFactory::new());
    let guard_b = Arc::new(TestDcGuardTransportFactory::new());

    let opts_a = WorkingDirOpenOptions::new(&session_dir_a, creds.clone())
        .with_test_mode(true)
        .with_transport_factory(guard_a.clone());
    let opts_b = WorkingDirOpenOptions::new(&session_dir_b, creds.clone())
        .with_test_mode(true)
        .with_transport_factory(guard_b.clone());

    let mut mgr_a = AccountManager::open_and_unlock(opts_a, b"")
        .map_err(|e| format!("step 1: open AccountManager A failed: {e}"))?;
    let mut mgr_b = AccountManager::open_and_unlock(opts_b, b"")
        .map_err(|e| format!("step 1: open AccountManager B failed: {e}"))?;

    let slot_a = mgr_a
        .active_index()
        .ok_or("step 1: no active account slot in session A dir")?;
    let slot_b = mgr_b
        .active_index()
        .ok_or("step 1: no active account slot in session B dir")?;
    let acc_a = mgr_a
        .account(slot_a)
        .ok_or("step 1: active slot A missing LiveAccount")?;
    let acc_b = mgr_b
        .account(slot_b)
        .ok_or("step 1: active slot B missing LiveAccount")?;

    assert_eq!(
        acc_a.environment,
        CoreEnvironment::Test,
        "SAFETY VIOLATION: session A is NOT in Environment::Test!"
    );
    assert_eq!(
        acc_b.environment,
        CoreEnvironment::Test,
        "SAFETY VIOLATION: session B is NOT in Environment::Test!"
    );

    let self_uid_a = acc_a
        .user_id
        .ok_or("step 1: session A is not authorized (user_id is None)")?;
    let self_uid_b = acc_b
        .user_id
        .ok_or("step 1: session B is not authorized (user_id is None)")?;
    if self_uid_a == self_uid_b {
        return Err("step 1: session A and session B belong to the same account!".into());
    }

    let rpc_a = acc_a.rpc.clone();
    let rpc_b = acc_b.rpc.clone();
    let client_a = acc_a.client.clone();
    let client_b = acc_b.client.clone();
    println!(
        "[step 1] Unlocked authorized test-DC sessions (A main_dc={}, B main_dc={})",
        acc_a.main_dc_id(),
        acc_b.main_dc_id()
    );

    // Always verify help.getConfig test_mode=true FIRST on BOTH accounts before any other RPC.
    let tl_enums::Config::Config(cfg_a) = rpc_a
        .invoke(&tl_fns::help::GetConfig)
        .await
        .map_err(|e| check_flood_wait_inv("step 1", "A help.getConfig", &e))?;
    guard_a.validate_and_register_config(&cfg_a);
    assert!(
        client_a.apply_tl_config(&cfg_a),
        "step 1: client_a.apply_tl_config rejected test config"
    );

    let tl_enums::Config::Config(cfg_b) = rpc_b
        .invoke(&tl_fns::help::GetConfig)
        .await
        .map_err(|e| check_flood_wait_inv("step 1", "B help.getConfig", &e))?;
    guard_b.validate_and_register_config(&cfg_b);
    assert!(
        client_b.apply_tl_config(&cfg_b),
        "step 1: client_b.apply_tl_config rejected test config"
    );
    println!(
        "  [step 1] help.getConfig verified test_mode=true (A dc={}, opts={}; B dc={}, opts={})",
        cfg_a.this_dc,
        cfg_a.dc_options.len(),
        cfg_b.this_dc,
        cfg_b.dc_options.len()
    );

    // Fetch self users via users.getUsers([inputUserSelf]) on each side (no phone numbers logged).
    let users_a = rpc_a
        .invoke(&tl_fns::users::GetUsers {
            id: vec![tl_enums::InputUser::Self_],
        })
        .await
        .map_err(|e| check_flood_wait_inv("step 1", "A users.getUsers(Self)", &e))?;
    let tl_user_a = users_a
        .into_iter()
        .find_map(|u| match u {
            tl_enums::User::User(usr) => Some(usr),
            tl_enums::User::Empty(_) => None,
        })
        .ok_or("step 1: A users.getUsers(Self) returned no User")?;

    let users_b = rpc_b
        .invoke(&tl_fns::users::GetUsers {
            id: vec![tl_enums::InputUser::Self_],
        })
        .await
        .map_err(|e| check_flood_wait_inv("step 1", "B users.getUsers(Self)", &e))?;
    let tl_user_b = users_b
        .into_iter()
        .find_map(|u| match u {
            tl_enums::User::User(usr) => Some(usr),
            tl_enums::User::Empty(_) => None,
        })
        .ok_or("step 1: B users.getUsers(Self) returned no User")?;

    let uid_a = UserId::from_tl(tl_user_a.id);
    let uid_b = UserId::from_tl(tl_user_b.id);
    let peer_a = PeerId::from_user(uid_a);
    let peer_b = PeerId::from_user(uid_b);
    println!(
        "  [step 1] users.getUsers(Self) OK: A uid={}, B uid={}",
        uid_a.0, uid_b.0
    );

    let handle = tokio::runtime::Handle::current();
    let rt_a = SessionRuntime::start_mtproto_on(
        uid_a,
        rpc_a.clone(),
        handle.clone(),
        SessionRuntimeConfig::default(),
    )
    .map_err(|e| format!("step 1: start SessionRuntime A failed: {e}"))?;
    rt_a.seed_self_user_async(&tl_user_a).await;

    let rt_b = SessionRuntime::start_mtproto_on(
        uid_b,
        rpc_b.clone(),
        handle.clone(),
        SessionRuntimeConfig::default(),
    )
    .map_err(|e| format!("step 1: start SessionRuntime B failed: {e}"))?;
    rt_b.seed_self_user_async(&tl_user_b).await;

    let mut step_results: Vec<(&'static str, Result<String, String>)> = Vec::new();
    let mut rng = SimpleRng::from_time(0x2A66_C0DE);

    // Cleanup state tracked from the moment any chat/group is created.
    let mut created_group: Option<(ChannelId, i64)> = None;
    let mut private_chat_ready = false;

    // Shared state passed across steps.
    let mut group_peer_opt: Option<PeerId> = None;
    let mut step2_b_reply_id_on_a: Option<MsgId> = None;
    let mut step2_b_reply_id_on_b: Option<MsgId> = None;
    let mut step3_msg_id_on_a: Option<MsgId> = None;
    let mut step3_msg_id_on_b: Option<MsgId> = None;

    // =========================================================================
    // Step 1: Connect both, create shared supergroup, invite link, B joins,
    //         discover each other's User + access_hash from group members,
    //         and open private chat A<->B.
    // =========================================================================
    let step1_res: Result<String, String> = async {
        rt_a.wait_ready()
            .await
            .map_err(|e| format!("step 1: runtime A wait_ready failed: {e}"))?;
        rt_b.wait_ready()
            .await
            .map_err(|e| format!("step 1: runtime B wait_ready failed: {e}"))?;

        // Mark both sessions online so SendProgressManager sees active presence.
        let _ = rt_a.update_presence(true, 0).await;
        let _ = rt_b.update_presence(true, 0).await;

        // A creates a supergroup (megagroup).
        let created = rpc_a
            .invoke(&tl_fns::channels::CreateChannel {
                broadcast: false,
                megagroup: true,
                for_import: false,
                forum: false,
                title: "Rustgram Two-Account Smoke".to_owned(),
                about: "Temporary automated test supergroup".to_owned(),
                geo_point: None,
                address: None,
                ttl_period: None,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 1", "A channels.createChannel", &e))?;

        let (group_ch_id, group_access_hash) = {
            let mut sa = rt_a.session().write().await;
            let ch_id = extract_chat_or_channel_from_updates(&created, &mut sa)
                .1
                .ok_or("step 1: createChannel did not return ChannelId")?;
            let hash = sa.channel(ch_id).map_or(0, |c| c.access_hash as i64);
            (ch_id, hash)
        };
        created_group = Some((group_ch_id, group_access_hash));
        let _ = rt_a.apply_updates(created).await;
        let group_peer = PeerId::from_channel(group_ch_id);
        group_peer_opt = Some(group_peer);
        let _ = rt_a.set_active_channel(group_ch_id, true).await;
        println!("  [step 1] A created supergroup channel_id={}", group_ch_id.0);

        // A exports an invite link.
        let input_group_a = {
            let sa = rt_a.session().read().await;
            input_peer_for(&sa, group_peer)
        };
        let exported = rpc_a
            .invoke(&tl_fns::messages::ExportChatInvite {
                legacy_revoke_permanent: false,
                request_needed: false,
                peer: input_group_a,
                expire_date: None,
                usage_limit: None,
                title: None,
                subscription_pricing: None,
            })
            .await
            .map_err(|e| check_flood_wait_inv("step 1", "A messages.exportChatInvite", &e))?;
        let invite_link = match exported {
            tl_enums::ExportedChatInvite::ChatInviteExported(inv) => inv.link,
            other => return Err(format!("step 1: unexpected invite variant: {other:?}")),
        };
        let invite_hash = invite_link
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(|s| s.trim_start_matches('+'))
            .filter(|s| !s.is_empty())
            .ok_or("step 1: could not parse invite hash from exported link")?
            .to_owned();
        println!("  [step 1] Exported supergroup invite link (hash_len={})", invite_hash.len());

        // B joins via messages.importChatInvite (wait briefly for Test DC backend to propagate
        // the brand-new invite link; retry with 2s spacing on transient INVITE_HASH_EXPIRED).
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let join_res = {
            let mut last_res = None;
            for attempt in 0..4 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_millis(2500)).await;
                }
                match rpc_b
                    .invoke(&tl_fns::messages::ImportChatInvite {
                        hash: invite_hash.clone(),
                    })
                    .await
                {
                    Ok(res) => {
                        last_res = Some(Ok(res));
                        break;
                    }
                    Err(InvocationError::Rpc(ref r))
                        if r.name == "INVITE_HASH_EXPIRED" && attempt < 5 =>
                    {
                        println!(
                            "  [step 1] B importChatInvite attempt {attempt} got INVITE_HASH_EXPIRED (propagation delay), retrying..."
                        );
                        last_res = Some(Err(InvocationError::Rpc(r.clone())));
                    }
                    Err(e) => {
                        last_res = Some(Err(e));
                        break;
                    }
                }
            }
            last_res
                .expect("retry loop ran at least once")
                .map_err(|e| check_flood_wait_inv("step 1", "B messages.importChatInvite", &e))?
        };
        match join_res {
            tl_enums::messages::ChatInviteJoinResult::Ok(ok) => {
                {
                    let mut sb = rt_b.session().write().await;
                    let _ = extract_chat_or_channel_from_updates(&ok.updates, &mut sb);
                }
                let _ = rt_b.apply_updates(ok.updates).await;
            }
            other => return Err(format!("step 1: unexpected importChatInvite result: {other:?}")),
        }
        let _ = rt_b.set_active_channel(group_ch_id, true).await;
        println!("  [step 1] B joined supergroup via messages.importChatInvite OK");

        // Both sides fetch group participants to obtain each other's User + access_hash.
        // Retry briefly if Telegram's participant index hasn't reflected the brand-new join yet.
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let part_a = {
                let mut sa = rt_a.session().write().await;
                tg_api::channels::get_participants(
                    &rpc_a,
                    &mut sa,
                    group_ch_id,
                    &ChannelParticipantsFilter::Recent,
                    0,
                    50,
                    0,
                )
                .await
                .map_err(|e| check_flood_wait_api("step 1", "A channels.getParticipants", &e))?
            };
            let part_b = {
                let mut sb = rt_b.session().write().await;
                tg_api::channels::get_participants(
                    &rpc_b,
                    &mut sb,
                    group_ch_id,
                    &ChannelParticipantsFilter::Recent,
                    0,
                    50,
                    0,
                )
                .await
                .map_err(|e| check_flood_wait_api("step 1", "B channels.getParticipants", &e))?
            };
            let part_a_ids: Vec<_> = match part_a {
                tg_api::ChannelParticipantsPage::Participants { participants, .. } => participants
                    .iter()
                    .map(|p| (p.peer_id.0, p.kind))
                    .collect(),
                tg_api::ChannelParticipantsPage::NotModified => Vec::new(),
            };
            let part_b_ids: Vec<_> = match part_b {
                tg_api::ChannelParticipantsPage::Participants { participants, .. } => participants
                    .iter()
                    .map(|p| (p.peer_id.0, p.kind))
                    .collect(),
                tg_api::ChannelParticipantsPage::NotModified => Vec::new(),
            };
            let a_has_b = {
                let sa = rt_a.session().read().await;
                sa.user(uid_b).is_some_and(|u| u.access_hash != 0)
            };
            let b_has_a = {
                let sb = rt_b.session().read().await;
                sb.user(uid_a).is_some_and(|u| u.access_hash != 0)
            };
            println!(
                "  [step 1] channels.getParticipants attempt {attempt}: A={part_a_ids:?} (has_B={a_has_b}), B={part_b_ids:?} (has_A={b_has_a})"
            );
            if a_has_b && b_has_a {
                break;
            }
        }

        // Open group history on both sides so channel PTS watermarks are initialized (and any
        // ChatJoinedByLink service message users are applied).
        let _ = rt_a
            .open_history(group_peer, MsgId::ZERO, tg_api::LoadDirection::Before)
            .await
            .map_err(|e| format!("step 1: A open_history(group) failed: {e}"))?;
        let _ = rt_b
            .open_history(group_peer, MsgId::ZERO, tg_api::LoadDirection::Before)
            .await
            .map_err(|e| format!("step 1: B open_history(group) failed: {e}"))?;

        // If A still lacks B's access_hash (e.g. if getParticipants only listed admins/creator),
        // have B send a brief hello in the shared group and fetch via open_history/getParticipant.
        let a_has_b_after = {
            let sa = rt_a.session().read().await;
            sa.user(uid_b).is_some_and(|u| u.access_hash != 0)
        };
        if !a_has_b_after {
            let _ = rt_b
                .send_message(group_peer, "hello from B", SendOptions::default())
                .await
                .map_err(|e| format!("step 1: B group hello failed: {e}"))?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = rt_a
                .open_history(group_peer, MsgId::ZERO, tg_api::LoadDirection::Before)
                .await
                .map_err(|e| format!("step 1: A open_history(group#2) failed: {e}"))?;
            let sa_user_b = {
                let sa = rt_a.session().read().await;
                sa.user(uid_b).map(|u| (u.access_hash, u.common.loaded_status))
            };
            println!("  [step 1] After B group hello, A user_b state={sa_user_b:?}");
        }

        // Verify both sides resolved each other's InputPeerUser with non-zero access_hash.
        let ip_b_on_a = {
            let sa = rt_a.session().read().await;
            input_peer_for(&sa, peer_b)
        };
        let ip_a_on_b = {
            let sb = rt_b.session().read().await;
            input_peer_for(&sb, peer_a)
        };
        match (&ip_b_on_a, &ip_a_on_b) {
            (tl_enums::InputPeer::User(ub), tl_enums::InputPeer::User(ua))
                if ub.access_hash != 0 && ua.access_hash != 0 => {}
            _ => {
                return Err(format!(
                    "step 1: missing InputPeerUser with access_hash (A->B={ip_b_on_a:?}, B->A={ip_a_on_b:?})"
                ));
            }
        }

        // Open private chat A<->B on both sides to confirm InputPeerUser works.
        let _ = rt_a
            .open_history(peer_b, MsgId::ZERO, tg_api::LoadDirection::Before)
            .await
            .map_err(|e| format!("step 1: A open_history(peer_b) failed: {e}"))?;
        let _ = rt_b
            .open_history(peer_a, MsgId::ZERO, tg_api::LoadDirection::Before)
            .await
            .map_err(|e| format!("step 1: B open_history(peer_a) failed: {e}"))?;
        private_chat_ready = true;

        Ok(format!(
            "connected A(uid={}) & B(uid={}), supergroup ch_id={}, InputPeerUser resolved both ways",
            uid_a.0, uid_b.0, group_ch_id.0
        ))
    }
    .await;
    let step1_ok = step1_res.is_ok();
    step_results.push((
        "Step 1 (connect, supergroup invite/join, A<->B private chat)",
        step1_res,
    ));

    // =========================================================================
    // Step 2: A sends B formatted text; B receives live (no polling) with
    //         correct entities; B replies; A receives live; edit & delete on
    //         one side show up live on the other.
    // =========================================================================
    let step2_res: Result<String, String> = if !step1_ok {
        Err("skipped because Step 1 failed".into())
    } else {
        async {
            println!("[step 2] Testing live formatted text, reply, edit, and delete across A<->B...");
            // 2A. A sends formatted text (Bold, Italic, Code) -> B receives LIVE.
            let msg_text = "Hello bold italic code!";
            let entities = vec![
                tl_enums::MessageEntity::Bold(tl_types::MessageEntityBold {
                    offset: 6,
                    length: 4,
                }),
                tl_enums::MessageEntity::Italic(tl_types::MessageEntityItalic {
                    offset: 11,
                    length: 6,
                }),
                tl_enums::MessageEntity::Code(tl_types::MessageEntityCode {
                    offset: 18,
                    length: 4,
                }),
            ];
            let send1 = rt_a
                .send_message_with_entities(peer_b, msg_text, entities, None, SendOptions::default())
                .await
                .map_err(|e| format!("step 2A: A send_message_with_entities failed: {e}"))?;
            let a_msg1_id = extract_first_sent_msg_id(&send1)
                .ok_or("step 2A: missing server msg_id for A's formatted message")?;

            let (b_msg1_id, b_entities) = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    for msg in peer_messages(&s, peer_a) {
                        if let Some(t) = msg.text()
                            && t.text == msg_text
                        {
                            return Some((msg.id, t.entities.clone()));
                        }
                    }
                    None
                })
            })
            .await
            .map_err(|e| format!("step 2A: B did not receive formatted message live: {e}"))?;

            let has_bold = b_entities
                .iter()
                .any(|e| e.kind == EntityKind::Bold && e.offset == 6 && e.length == 4);
            let has_italic = b_entities
                .iter()
                .any(|e| e.kind == EntityKind::Italic && e.offset == 11 && e.length == 6);
            let has_code = b_entities
                .iter()
                .any(|e| e.kind == EntityKind::Code && e.offset == 18 && e.length == 4);
            if !(has_bold && has_italic && has_code) {
                return Err(format!(
                    "step 2A: B received wrong entities: {b_entities:?} (expected Bold(6,4), Italic(11,6), Code(18,4))"
                ));
            }
            println!(
                "  [step 2A] Live formatted text delivered A(id={}) -> B(id={}) with exact Bold/Italic/Code entities",
                a_msg1_id.0, b_msg1_id.0
            );

            // 2B. B replies to A's message -> A receives reply LIVE.
            let reply_text = "Live reply from B";
            let reply_hdr = tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
                reply_to_msg_id: b_msg1_id.0 as i32,
                top_msg_id: None,
                reply_to_peer_id: None,
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                monoforum_peer_id: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            });
            let send_reply = rt_b
                .send_message_with_entities(
                    peer_a,
                    reply_text,
                    Vec::new(),
                    Some(reply_hdr),
                    SendOptions::default(),
                )
                .await
                .map_err(|e| format!("step 2B: B reply failed: {e}"))?;
            let b_reply_id = extract_first_sent_msg_id(&send_reply)
                .ok_or("step 2B: missing server msg_id for B's reply")?;

            let a_reply_id = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_a.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    for msg in peer_messages(&s, peer_b) {
                        if msg.text().is_some_and(|t| t.text == reply_text)
                            && msg.reply_to.is_some()
                        {
                            return Some(msg.id);
                        }
                    }
                    None
                })
            })
            .await
            .map_err(|e| format!("step 2B: A did not receive B's reply live: {e}"))?;
            step2_b_reply_id_on_a = Some(a_reply_id);
            step2_b_reply_id_on_b = Some(b_reply_id);
            println!(
                "  [step 2B] Live reply delivered B(id={}) -> A(id={}) with reply_to header",
                b_reply_id.0, a_reply_id.0
            );

            // 2C. A edits msg1 -> B sees edit LIVE with updated text + entity.
            let edited_text = "Hello EDITED bold text!";
            let input_peer_b = {
                let sa = rt_a.session().read().await;
                input_peer_for(&sa, peer_b)
            };
            let _ = rt_a
                .edit_message(tl_fns::messages::EditMessage {
                    no_webpage: false,
                    invert_media: false,
                    peer: input_peer_b,
                    id: a_msg1_id.0 as i32,
                    message: Some(edited_text.to_owned()),
                    media: None,
                    reply_markup: None,
                    entities: Some(vec![tl_enums::MessageEntity::Bold(
                        tl_types::MessageEntityBold {
                            offset: 6,
                            length: 6,
                        },
                    )]),
                    schedule_date: None,
                    schedule_repeat_period: None,
                    quick_reply_shortcut_id: None,
                    rich_message: None,
                })
                .await
                .map_err(|e| format!("step 2C: A edit_message failed: {e}"))?;

            wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    let m = s.message(FullMsgId::new(peer_a, b_msg1_id))?;
                    let t = m.text()?;
                    let ok_entity = t
                        .entities
                        .iter()
                        .any(|e| e.kind == EntityKind::Bold && e.offset == 6 && e.length == 6);
                    (t.text == edited_text && ok_entity).then_some(())
                })
            })
            .await
            .map_err(|e| format!("step 2C: B did not receive live edit update: {e}"))?;
            println!("  [step 2C] Live edit delivered A -> B with updated text + Bold(6,6)");

            // 2D. A deletes msg1 (revoke=true) -> B sees deletion LIVE.
            let _ = rt_a
                .delete_messages(peer_b, vec![a_msg1_id], true)
                .await
                .map_err(|e| format!("step 2D: A delete_messages(revoke=true) failed: {e}"))?;

            wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    s.message(FullMsgId::new(peer_a, b_msg1_id))
                        .is_none()
                        .then_some(())
                })
            })
            .await
            .map_err(|e| format!("step 2D: B did not receive live delete(revoke=true): {e}"))?;
            println!("  [step 2D] Live delete(revoke=true) delivered A -> B");

            Ok(format!(
                "formatted(3 entities) + reply(A={},B={}) + edit + delete(revoke=true) all live OK",
                a_reply_id.0, b_reply_id.0
            ))
        }
        .await
    };
    step_results.push((
        "Step 2 (live formatted text, reply, edit, delete)",
        step2_res,
    ));

    // =========================================================================
    // Step 3: Read receipts: B reads -> A sees outbox read
    //         (updateReadHistoryOutbox, messages.getOutboxReadDate where
    //         allowed); unread counters on both sides.
    // =========================================================================
    let step3_res: Result<String, String> = if !step1_ok {
        Err("skipped because Step 1 failed".into())
    } else {
        async {
            println!("[step 3] Testing read receipts, updateReadHistoryOutbox, getOutboxReadDate, and unread counters on both sides...");

            // If B's stranger chat bar (`report_spam=true`) is active on the A<->B chat,
            // Telegram suppresses outgoing read receipts from B to A (`MESSAGE_NOT_READ_YET`).
            // Dismiss the chat-local peer settings bar on A<->B only (does not alter profiles,
            // contacts, or global privacy).
            {
                let ip_a = {
                    let sb = rt_b.session().read().await;
                    input_peer_for(&sb, peer_a)
                };
                if let Ok(tl_enums::messages::PeerSettings::PeerSettings(psb)) =
                    rpc_b.invoke(&tl_fns::messages::GetPeerSettings { peer: ip_a.clone() }).await
                {
                    let tl_enums::PeerSettings::PeerSettings(s) = psb.settings;
                    if s.report_spam {
                        let _ = rpc_b
                            .invoke(&tl_fns::messages::HidePeerSettingsBar { peer: ip_a })
                            .await;
                    }
                }
            }

            // 3A. Verify A's unread counter from B's Step 2 reply, then A marks read -> 0,
            //     and check whether B sees outbox read for B's reply!
            let a_unread_before = {
                let sa = rt_a.session().read().await;
                sa.history(peer_b).and_then(|h| h.unread_count).unwrap_or(0)
            };
            let a_reply_to_read = match step2_b_reply_id_on_a {
                Some(id) => id,
                None => {
                    let s = rt_b
                        .send_message(peer_a, "unread-check-b-to-a", SendOptions::default())
                        .await
                        .map_err(|e| format!("step 3A: fallback B send failed: {e}"))?;
                    let _ = s;
                    wait_until(STEP_WAIT_TIMEOUT, || {
                        let session_arc = Arc::clone(rt_a.session());
                        Box::pin(async move {
                            let s = session_arc.read().await;
                            peer_messages(&s, peer_b)
                                .into_iter()
                                .find(|m| m.text().is_some_and(|t| t.text == "unread-check-b-to-a"))
                                .map(|m| m.id)
                        })
                    })
                    .await
                    .map_err(|e| format!("step 3A: A did not receive fallback B msg: {e}"))?
                }
            };
            let a_unread_check = {
                let sa = rt_a.session().read().await;
                sa.history(peer_b).and_then(|h| h.unread_count).unwrap_or(0)
            };
            if a_unread_check == 0 {
                return Err(format!(
                    "step 3A: expected A unread_count > 0 before mark_read (got before={a_unread_before}, check={a_unread_check})"
                ));
            }
            let _ = rt_a
                .mark_read(peer_b, a_reply_to_read, true)
                .await
                .map_err(|e| format!("step 3A: A mark_read failed: {e}"))?;
            let a_unread_after = {
                let sa = rt_a.session().read().await;
                sa.history(peer_b).and_then(|h| h.unread_count).unwrap_or(0)
            };
            if a_unread_after != 0 {
                return Err(format!(
                    "step 3A: A unread_count after mark_read is {a_unread_after}, expected 0"
                ));
            }
            tokio::time::sleep(Duration::from_millis(600)).await;
            let b_outbox_after_3a = {
                let sb = rt_b.session().read().await;
                sb.history(peer_a).and_then(|h| h.outbox_read_before)
            };
            let b_read_date_3a = if let Some(b_rid) = step2_b_reply_id_on_b {
                let ip_a = {
                    let sb = rt_b.session().read().await;
                    input_peer_for(&sb, peer_a)
                };
                Some(
                    rpc_b
                        .invoke(&tl_fns::messages::GetOutboxReadDate {
                            peer: ip_a,
                            msg_id: b_rid.0 as i32,
                        })
                        .await,
                )
            } else {
                None
            };
            println!(
                "  [step 3A] A unread counter verified: {a_unread_check} -> {a_unread_after}; B outbox_read_before={b_outbox_after_3a:?}, B getOutboxReadDate={b_read_date_3a:?}"
            );

            // 3B. A sends a fresh message to B; B's unread increments; B marks read ->
            //     B unread becomes 0 AND A receives updateReadHistoryOutbox live.
            let mut raw_a_rx = rpc_a.subscribe_updates();
            let read_text = "Read receipt & reaction target message";
            let send_r = rt_a
                .send_message(peer_b, read_text, SendOptions::default())
                .await
                .map_err(|e| format!("step 3B: A send_message failed: {e}"))?;
            let a_read_msg_id = extract_first_sent_msg_id(&send_r)
                .ok_or("step 3B: missing server msg_id for A's message")?;
            step3_msg_id_on_a = Some(a_read_msg_id);

            let (b_read_msg_id, b_unread_before) = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    let hist = s.history(peer_a)?;
                    let msg = peer_messages(&s, peer_a)
                        .into_iter()
                        .find(|m| m.text().is_some_and(|t| t.text == read_text))?;
                    let unread = hist.unread_count.unwrap_or(0);
                    (unread > 0).then_some((msg.id, unread))
                })
            })
            .await
            .map_err(|e| format!("step 3B: B did not receive message with unread > 0: {e}"))?;
            step3_msg_id_on_b = Some(b_read_msg_id);

            let flushed_b = rt_b
                .mark_read(peer_a, b_read_msg_id, true)
                .await
                .map_err(|e| format!("step 3B: B mark_read failed: {e}"))?;
            println!(
                "  [step 3B] B mark_read(peer_a, max_id={}) flushed={flushed_b:?}",
                b_read_msg_id.0
            );
            let b_unread_after = {
                let sb = rt_b.session().read().await;
                sb.history(peer_a).and_then(|h| h.unread_count).unwrap_or(0)
            };
            if b_unread_after != 0 {
                return Err(format!(
                    "step 3B: B unread_count after mark_read is {b_unread_after}, expected 0"
                ));
            }
            println!(
                "  [step 3B] B unread counter verified: {b_unread_before} -> {b_unread_after}"
            );

            // Wait for A's Session outbox_read_before to advance past a_read_msg_id.
            let a_outbox_read = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_a.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    let hist = s.history(peer_b)?;
                    hist.outbox_read_before
                        .filter(|r| r.0 > a_read_msg_id.0)
                })
            })
            .await
            .map_err(|e| format!("step 3B: A did not see outbox read receipt live: {e}"))?;

            // Confirm raw `updateReadHistoryOutbox` arrived on A's live MTProto push stream.
            let mut saw_raw_outbox_update = false;
            if let Some(ref mut rx) = raw_a_rx {
                while let Ok(upd) = rx.try_recv() {
                    if contains_update_matching(&upd, |u| {
                        matches!(u, tl_enums::Update::ReadHistoryOutbox(_))
                    }) {
                        saw_raw_outbox_update = true;
                    }
                }
            }
            println!(
                "  [step 3B] A outbox_read_before={} (> msg_id={}), saw_raw_updateReadHistoryOutbox={saw_raw_outbox_update}",
                a_outbox_read.0, a_read_msg_id.0
            );

            // 3C. messages.getOutboxReadDate (where allowed by privacy settings).
            let input_peer_b = {
                let sa = rt_a.session().read().await;
                input_peer_for(&sa, peer_b)
            };
            let read_date_note = match rpc_a
                .invoke(&tl_fns::messages::GetOutboxReadDate {
                    peer: input_peer_b,
                    msg_id: a_read_msg_id.0 as i32,
                })
                .await
            {
                Ok(tl_enums::OutboxReadDate::OutboxReadDate(d)) => {
                    if d.date <= 0 {
                        return Err(format!(
                            "step 3C: messages.getOutboxReadDate returned non-positive date {}",
                            d.date
                        ));
                    }
                    format!("date={}", d.date)
                }
                Err(InvocationError::Rpc(r))
                    if matches!(
                        r.name.as_str(),
                        "YOUR_PRIVACY_RESTRICTED" | "USER_PRIVACY_RESTRICTED"
                    ) =>
                {
                    format!("privacy-restricted ({} {})", r.code, r.name)
                }
                Err(e) => {
                    return Err(check_flood_wait_inv(
                        "step 3C",
                        "messages.getOutboxReadDate",
                        &e,
                    ));
                }
            };
            println!("  [step 3C] messages.getOutboxReadDate: {read_date_note}");

            Ok(format!(
                "A unread {a_unread_check}->0, B unread {b_unread_before}->0, outbox_read_before={} (raw_update={saw_raw_outbox_update}), getOutboxReadDate={read_date_note}",
                a_outbox_read.0
            ))
        }
        .await
    };
    step_results.push((
        "Step 3 (read receipts, outbox read date, unread counters)",
        step3_res,
    ));

    // =========================================================================
    // Step 4: Typing: A sends messages.setTyping -> B gets updateUserTyping;
    //         also in the group (updateChannelUserTyping / updateChatUserTyping).
    // =========================================================================
    let step4_res: Result<String, String> = if !step1_ok {
        Err("skipped because Step 1 failed".into())
    } else {
        async {
            println!("[step 4] Testing live typing indicators (DM updateUserTyping + group typing)...");
            let group_peer = group_peer_opt.ok_or("step 4: missing group_peer")?;
            let mut raw_b_rx = rpc_b.subscribe_updates();

            // 4A. Private chat typing A -> B.
            let sent_dm = rt_a
                .send_typing(peer_b, None, SendProgressKind::Typing, 0)
                .await
                .map_err(|e| format!("step 4A: A send_typing(DM) failed: {e}"))?;
            if !sent_dm {
                let lastseen_dbg = {
                    let sa = rt_a.session().read().await;
                    sa.user(uid_b).map(|u| u.lastseen)
                };
                return Err(format!(
                    "step 4A: A send_typing(DM) was skipped locally by SendProgressManager (peer B lastseen={lastseen_dbg:?})"
                ));
            }

            wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    s.active_typing(peer_a)
                        .into_iter()
                        .any(|(from, _, act)| from == peer_a && act == TypingAction::Typing)
                        .then_some(())
                })
            })
            .await
            .map_err(|e| format!("step 4A: B did not receive DM typing update live: {e}"))?;

            let mut dm_raw_kinds = Vec::new();
            if let Some(ref mut rx) = raw_b_rx {
                while let Ok(upd) = rx.try_recv() {
                    dm_raw_kinds.extend(describe_typing_updates(&upd));
                }
            }
            println!("  [step 4A] DM typing received live on B (raw={dm_raw_kinds:?})");

            // 4B. Group typing A -> B in the shared supergroup.
            let sent_grp = rt_a
                .send_typing(group_peer, None, SendProgressKind::Typing, 0)
                .await
                .map_err(|e| format!("step 4B: A send_typing(group) failed: {e}"))?;
            if !sent_grp {
                return Err(
                    "step 4B: A send_typing(group) was skipped locally by SendProgressManager"
                        .into(),
                );
            }

            wait_until(Duration::from_secs(10), || {
                let session_arc = Arc::clone(rt_b.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    s.active_typing(group_peer)
                        .into_iter()
                        .any(|(from, _, act)| from == peer_a && act == TypingAction::Typing)
                        .then_some(())
                })
            })
            .await
            .map_err(|e| format!("step 4B: B did not receive group typing update live: {e}"))?;

            let mut grp_raw_kinds = Vec::new();
            if let Some(ref mut rx) = raw_b_rx {
                while let Ok(upd) = rx.try_recv() {
                    grp_raw_kinds.extend(describe_typing_updates(&upd));
                }
            }
            println!("  [step 4B] Group typing received live on B (raw={grp_raw_kinds:?})");

            Ok(format!(
                "DM typing live OK (raw={dm_raw_kinds:?}); group typing live OK (raw={grp_raw_kinds:?})"
            ))
        }
        .await
    };
    step_results.push(("Step 4 (typing in DM and group)", step4_res));

    // =========================================================================
    // Step 5: Reactions from B on A's message reach A; forward from A's chat
    //         to the group; reply in the group.
    // =========================================================================
    let step5_res: Result<String, String> = if !step1_ok {
        Err("skipped because Step 1 failed".into())
    } else {
        async {
            println!("[step 5] Testing reactions B->A, forward DM->group, and reply in group...");
            let group_peer = group_peer_opt.ok_or("step 5: missing group_peer")?;
            let (a_target_id, b_target_id) = match (step3_msg_id_on_a, step3_msg_id_on_b) {
                (Some(a), Some(b)) => (a, b),
                _ => {
                    let text = "Step 5 target msg";
                    let out = rt_a
                        .send_message(peer_b, text, SendOptions::default())
                        .await
                        .map_err(|e| format!("step 5: fallback send failed: {e}"))?;
                    let a_id = extract_first_sent_msg_id(&out)
                        .ok_or("step 5: missing fallback a_id")?;
                    let b_id = wait_until(STEP_WAIT_TIMEOUT, || {
                        let session_arc = Arc::clone(rt_b.session());
                        Box::pin(async move {
                            let s = session_arc.read().await;
                            peer_messages(&s, peer_a)
                                .into_iter()
                                .find(|m| m.text().is_some_and(|t| t.text == text))
                                .map(|m| m.id)
                        })
                    })
                    .await
                    .map_err(|e| format!("step 5: B missed fallback msg: {e}"))?;
                    (a_id, b_id)
                }
            };

            // 5A. B reacts with thumbs-up -> A receives live reaction update.
            let input_peer_a_on_b = {
                let sb = rt_b.session().read().await;
                input_peer_for(&sb, peer_a)
            };
            let react_upd = rpc_b
                .invoke(&tl_fns::messages::SendReaction {
                    big: false,
                    add_to_recent: false,
                    peer: input_peer_a_on_b,
                    msg_id: b_target_id.0 as i32,
                    reaction: Some(vec![tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "\u{1F44D}".to_owned(),
                    })]),
                })
                .await
                .map_err(|e| check_flood_wait_inv("step 5A", "B messages.sendReaction", &e))?;
            let _ = rt_b.apply_updates(react_upd).await;

            wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_a.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    let m = s.message(FullMsgId::new(peer_b, a_target_id))?;
                    m.reactions
                        .as_ref()
                        .is_some_and(|r| !r.results.is_empty())
                        .then_some(())
                })
            })
            .await
            .map_err(|e| format!("step 5A: A did not receive live reaction update from B: {e}"))?;
            println!("  [step 5A] Live reaction from B reached A OK");

            // 5B. A forwards message from A<->B private chat to the shared group.
            let (input_peer_b_on_a, input_group_on_a, target_text) = {
                let sa = rt_a.session().read().await;
                let text = sa
                    .message(FullMsgId::new(peer_b, a_target_id))
                    .and_then(|m| m.text().map(|t| t.text.clone()))
                    .unwrap_or_default();
                (
                    input_peer_for(&sa, peer_b),
                    input_peer_for(&sa, group_peer),
                    text,
                )
            };
            let mut fwd_sender = MessageSender::default();
            let fwd_out = fwd_sender
                .forward_messages(
                    &rpc_a,
                    &[ForwardItem {
                        from_peer: input_peer_b_on_a,
                        msg_id: a_target_id,
                    }],
                    input_group_on_a,
                    None,
                    ForwardOptions::PreserveInfo,
                    SendOptions::default(),
                )
                .await
                .map_err(|e| format!("step 5B: A forward_messages to group failed: {e:?}"))?;
            for upd in fwd_out.updates {
                let _ = rt_a.apply_updates(upd).await;
            }

            let b_fwd_msg_id = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                let expected = target_text.clone();
                Box::pin(async move {
                    let s = session_arc.read().await;
                    for msg in peer_messages(&s, group_peer) {
                        if msg.text().is_some_and(|t| t.text == expected)
                            && msg.forward_from.is_some()
                        {
                            return Some(msg.id);
                        }
                    }
                    None
                })
            })
            .await
            .map_err(|e| format!("step 5B: B did not receive forwarded message in group live: {e}"))?;
            println!(
                "  [step 5B] Forward from A<->B chat to group delivered live to B (group msg_id={})",
                b_fwd_msg_id.0
            );

            // 5C. B replies to the forwarded message in the group -> A receives reply live.
            let grp_reply_text = "Group reply from B to forwarded msg";
            let grp_reply_hdr = tl_enums::InputReplyTo::Message(tl_types::InputReplyToMessage {
                reply_to_msg_id: b_fwd_msg_id.0 as i32,
                top_msg_id: None,
                reply_to_peer_id: None,
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                monoforum_peer_id: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            });
            let grp_reply_out = rt_b
                .send_message_with_entities(
                    group_peer,
                    grp_reply_text,
                    Vec::new(),
                    Some(grp_reply_hdr),
                    SendOptions::default(),
                )
                .await
                .map_err(|e| format!("step 5C: B reply in group failed: {e}"))?;
            let b_grp_reply_id = extract_first_sent_msg_id(&grp_reply_out)
                .ok_or("step 5C: missing server msg_id for B's group reply")?;

            let a_grp_reply_id = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_a.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    for msg in peer_messages(&s, group_peer) {
                        if msg.text().is_some_and(|t| t.text == grp_reply_text)
                            && msg.reply_to.is_some()
                        {
                            return Some(msg.id);
                        }
                    }
                    None
                })
            })
            .await
            .map_err(|e| format!("step 5C: A did not receive B's group reply live: {e}"))?;
            println!(
                "  [step 5C] Group reply delivered live B(id={}) -> A(id={}) OK",
                b_grp_reply_id.0, a_grp_reply_id.0
            );

            Ok(format!(
                "reaction B->A OK; forward DM->group (msg_id={}) OK; group reply (msg_id={}) OK",
                b_fwd_msg_id.0, a_grp_reply_id.0
            ))
        }
        .await
    };
    step_results.push((
        "Step 5 (reactions, forward to group, reply in group)",
        step5_res,
    ));

    // =========================================================================
    // Step 6: A sends B a ~3 MB file and a photo; B downloads both and
    //         SHA-256 matches.
    // =========================================================================
    let step6_res: Result<String, String> = if !step1_ok {
        Err("skipped because Step 1 failed".into())
    } else {
        async {
            println!("[step 6] Uploading ~3 MB file + photo from A to B and verifying B's downloads & SHA-256...");
            let invoker_a = LiveMediaInvoker::new(rpc_a.clone());
            let invoker_b = LiveMediaInvoker::new(rpc_b.clone());
            let cancel = CancelToken::new();
            let input_peer_b = {
                let sa = rt_a.session().read().await;
                input_peer_for(&sa, peer_b)
            };

            // 6A. ~3 MB file via big-file upload path (`upload.saveBigFilePart`).
            let big_bytes = generate_big_payload();
            let big_sha = sha256_hex(&big_bytes);
            let big_cfg = UploadConfig {
                big_file_threshold: 1024 * 1024,
                max_sessions: 4,
                ..UploadConfig::default()
            };
            let uploaded_big = upload_bytes(
                &invoker_a,
                "rustgram_two_acct_3mb.bin",
                &big_bytes,
                big_cfg,
                &cancel,
                |_| {},
            )
            .await
            .map_err(|e| format!("step 6A: upload 3 MB file failed: {e}"))?;
            assert!(
                matches!(uploaded_big, UploadedInputFile::Big { .. }),
                "step 6A: expected UploadedInputFile::Big"
            );
            let big_caption = "two-acct-3mb-file";
            let big_upd = rpc_a
                .invoke(&tl_fns::messages::SendMedia {
                    silent: false,
                    background: false,
                    clear_draft: false,
                    noforwards: false,
                    update_stickersets_order: false,
                    invert_media: false,
                    allow_paid_floodskip: false,
                    peer: input_peer_b.clone(),
                    reply_to: None,
                    media: tl_enums::InputMedia::UploadedDocument(
                        tl_types::InputMediaUploadedDocument {
                            nosound_video: false,
                            force_file: true,
                            spoiler: false,
                            file: uploaded_to_tl_input_file(&uploaded_big),
                            thumb: None,
                            mime_type: "application/octet-stream".to_owned(),
                            attributes: vec![tl_enums::DocumentAttribute::Filename(
                                tl_types::DocumentAttributeFilename {
                                    file_name: "rustgram_two_acct_3mb.bin".to_owned(),
                                },
                            )],
                            stickers: None,
                            video_cover: None,
                            video_timestamp: None,
                            ttl_seconds: None,
                        },
                    ),
                    message: big_caption.to_owned(),
                    random_id: rng.next_u64() as i64,
                    reply_markup: None,
                    entities: None,
                    schedule_date: None,
                    schedule_repeat_period: None,
                    send_as: None,
                    quick_reply_shortcut: None,
                    effect: None,
                    allow_paid_stars: None,
                    suggested_post: None,
                })
                .await
                .map_err(|e| check_flood_wait_inv("step 6A", "A sendMedia(3 MB file)", &e))?;
            let _ = rt_a.apply_updates(big_upd).await;

            let big_doc_b = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    for msg in peer_messages(&s, peer_a) {
                        if msg.text().is_some_and(|t| t.text == big_caption)
                            && let Some(MessageMedia::Document(doc)) = msg.media()
                        {
                            return Some(doc.clone());
                        }
                    }
                    None
                })
            })
            .await
            .map_err(|e| format!("step 6A: B did not receive 3 MB file message live: {e}"))?;

            let dl_big = download_file(
                &invoker_b,
                DownloadRequest::new(
                    big_doc_b.dc_id,
                    TransferLocation::Document {
                        id: big_doc_b.id.0,
                        access_hash: big_doc_b.access_hash,
                        file_reference: big_doc_b.file_reference.clone(),
                        thumb_size: String::new(),
                    },
                    big_doc_b.size as u64,
                ),
                None,
                &cancel,
                |_| {},
            )
            .await
            .map_err(|e| format!("step 6A: B download 3 MB file failed: {e}"))?;
            let dl_big_sha = sha256_hex(&dl_big);
            if dl_big_sha != big_sha {
                return Err(format!(
                    "step 6A: 3 MB file SHA-256 mismatch (sent={big_sha}, got={dl_big_sha})"
                ));
            }
            println!(
                "  [step 6A] 3 MB file ({} B) received live & downloaded by B with exact SHA-256 match!",
                big_bytes.len()
            );

            // 6B. Photo (`InputMedia::UploadedPhoto`): A sends PNG photo, B receives live,
            //     B downloads largest PhotoSize, verify valid JPEG AND SHA-256 matches A's
            //     downloaded copy of the server-stored photo.
            let png_bytes = generate_test_png(64, 64);
            let uploaded_photo = upload_bytes(
                &invoker_a,
                "rustgram_two_acct_photo.png",
                &png_bytes,
                UploadConfig::default(),
                &cancel,
                |_| {},
            )
            .await
            .map_err(|e| format!("step 6B: upload photo failed: {e}"))?;
            assert!(
                matches!(uploaded_photo, UploadedInputFile::Small { .. }),
                "step 6B: expected UploadedInputFile::Small"
            );
            let photo_caption = "two-acct-photo";
            let photo_upd = rpc_a
                .invoke(&tl_fns::messages::SendMedia {
                    silent: false,
                    background: false,
                    clear_draft: false,
                    noforwards: false,
                    update_stickersets_order: false,
                    invert_media: false,
                    allow_paid_floodskip: false,
                    peer: input_peer_b,
                    reply_to: None,
                    media: tl_enums::InputMedia::UploadedPhoto(
                        tl_types::InputMediaUploadedPhoto {
                            spoiler: false,
                            live_photo: false,
                            file: uploaded_to_tl_input_file(&uploaded_photo),
                            stickers: None,
                            ttl_seconds: None,
                            video: None,
                        },
                    ),
                    message: photo_caption.to_owned(),
                    random_id: rng.next_u64() as i64,
                    reply_markup: None,
                    entities: None,
                    schedule_date: None,
                    schedule_repeat_period: None,
                    send_as: None,
                    quick_reply_shortcut: None,
                    effect: None,
                    allow_paid_stars: None,
                    suggested_post: None,
                })
                .await
                .map_err(|e| check_flood_wait_inv("step 6B", "A sendMedia(photo)", &e))?;
            let a_photo_msg_id = extract_msg_ids_from_updates(&photo_upd)
                .into_iter()
                .next()
                .ok_or("step 6B: sendMedia(photo) returned no msg_id")?;
            let _ = rt_a.apply_updates(photo_upd).await;

            let photo_b = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                Box::pin(async move {
                    let s = session_arc.read().await;
                    for msg in peer_messages(&s, peer_a) {
                        if msg.text().is_some_and(|t| t.text == photo_caption)
                            && let Some(MessageMedia::Photo(p)) = msg.media()
                        {
                            return Some(p.clone());
                        }
                    }
                    None
                })
            })
            .await
            .map_err(|e| format!("step 6B: B did not receive photo message live: {e}"))?;

            let best_size_b = photo_b
                .sizes
                .iter()
                .filter(|sz| !sz.kind.is_empty() && sz.kind != "i")
                .max_by_key(|sz| (i64::from(sz.width) * i64::from(sz.height), sz.size))
                .ok_or("step 6B: photo on B has no downloadable PhotoSize")?;
            let dl_photo_b = download_file(
                &invoker_b,
                DownloadRequest::new(
                    photo_b.dc_id,
                    TransferLocation::Photo {
                        id: photo_b.id.0,
                        access_hash: photo_b.access_hash,
                        file_reference: photo_b.file_reference.clone(),
                        thumb_size: best_size_b.kind.clone(),
                    },
                    best_size_b.size.max(0) as u64,
                ),
                None,
                &cancel,
                |_| {},
            )
            .await
            .map_err(|e| format!("step 6B: B download photo failed: {e}"))?;
            if dl_photo_b.len() < 16 || dl_photo_b[0..2] != [0xFF, 0xD8] {
                return Err(format!(
                    "step 6B: B downloaded photo ({} B) is not a valid JPEG",
                    dl_photo_b.len()
                ));
            }

            // Download the same photo size from A's session copy and assert identical SHA-256.
            let photo_a = {
                let sa = rt_a.session().read().await;
                sa.message(FullMsgId::new(peer_b, a_photo_msg_id))
                    .and_then(|m| match m.media() {
                        Some(MessageMedia::Photo(p)) => Some(p.clone()),
                        _ => None,
                    })
                    .ok_or("step 6B: photo message missing on A Session after apply_updates")?
            };
            let dl_photo_a = download_file(
                &invoker_a,
                DownloadRequest::new(
                    photo_a.dc_id,
                    TransferLocation::Photo {
                        id: photo_a.id.0,
                        access_hash: photo_a.access_hash,
                        file_reference: photo_a.file_reference.clone(),
                        thumb_size: best_size_b.kind.clone(),
                    },
                    best_size_b.size.max(0) as u64,
                ),
                None,
                &cancel,
                |_| {},
            )
            .await
            .map_err(|e| format!("step 6B: A download photo failed: {e}"))?;
            let sha_photo_a = sha256_hex(&dl_photo_a);
            let sha_photo_b = sha256_hex(&dl_photo_b);
            if sha_photo_a != sha_photo_b {
                return Err(format!(
                    "step 6B: photo SHA-256 mismatch between A and B (A={sha_photo_a}, B={sha_photo_b})"
                ));
            }
            println!(
                "  [step 6B] Photo received live & downloaded by B ({} B JPEG, size='{}'), SHA-256 matches A's copy!",
                dl_photo_b.len(),
                best_size_b.kind
            );

            Ok(format!(
                "3 MB file ({} B) SHA-256 exact match; photo ({} B JPEG, size='{}') SHA-256 match across A&B",
                big_bytes.len(),
                dl_photo_b.len(),
                best_size_b.kind
            ))
        }
        .await
    };
    step_results.push((
        "Step 6 (3 MB file + photo upload/download/SHA-256)",
        step6_res,
    ));

    // =========================================================================
    // Step 7: B drops its TCP connection while A sends 5 messages; after
    //         reconnect B has all 5, in order, no duplicates (getDifference).
    // =========================================================================
    let step7_res: Result<String, String> = if !step1_ok {
        Err("skipped because Step 1 failed".into())
    } else {
        async {
            println!("[step 7] Dropping B's TCP connection while A sends 5 messages; verifying reconnect + getDifference...");
            let connects_before = guard_b.connect_count();
            guard_b.set_block_connects(true);
            guard_b.force_drop_active_connections();
            tokio::time::sleep(Duration::from_millis(500)).await;

            let nonce = rng.next_u64() % 1_000_000;
            let gap_msgs: Vec<String> = (1..=5)
                .map(|i| format!("two-acct-gap-{nonce}-msg-{i}"))
                .collect();
            for text in &gap_msgs {
                let _ = rt_a
                    .send_message(peer_b, text.clone(), SendOptions::default())
                    .await
                    .map_err(|e| format!("step 7: A send '{text}' failed: {e}"))?;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Confirm B did NOT receive the messages while disconnected.
            let received_while_down = {
                let sb = rt_b.session().read().await;
                let msgs = peer_messages(&sb, peer_a);
                gap_msgs
                    .iter()
                    .filter(|expected| {
                        msgs.iter()
                            .any(|m| m.text().is_some_and(|t| t.text == **expected))
                    })
                    .count()
            };
            if received_while_down > 0 {
                return Err(format!(
                    "step 7: B unexpectedly received {received_while_down}/5 messages while TCP was dropped"
                ));
            }

            // Re-allow connects and trigger reconnect -> getDifference on B.
            guard_b.set_block_connects(false);
            rt_b.on_reconnect()
                .await
                .map_err(|e| format!("step 7: B on_reconnect failed: {e}"))?;

            let recovered_ids = wait_until(STEP_WAIT_TIMEOUT, || {
                let session_arc = Arc::clone(rt_b.session());
                let expected = gap_msgs.clone();
                Box::pin(async move {
                    let s = session_arc.read().await;
                    let msgs = peer_messages(&s, peer_a);
                    let mut ids = Vec::with_capacity(expected.len());
                    for exp in &expected {
                        let matches: Vec<MsgId> = msgs
                            .iter()
                            .filter(|m| m.text().is_some_and(|t| t.text == *exp))
                            .map(|m| m.id)
                            .collect();
                        if matches.is_empty() {
                            return None;
                        }
                        ids.push((exp.clone(), matches));
                    }
                    Some(ids)
                })
            })
            .await
            .map_err(|e| format!("step 7: B did not recover all 5 messages via getDifference: {e}"))?;

            let connects_after = guard_b.connect_count();
            if connects_after <= connects_before {
                return Err(format!(
                    "step 7: expected B connect_count to increase ({connects_before} -> {connects_after})"
                ));
            }

            // Check zero duplicates and strictly increasing MsgId order.
            let mut ordered_ids = Vec::with_capacity(5);
            for (text, matches) in &recovered_ids {
                if matches.len() != 1 {
                    return Err(format!(
                        "step 7: duplicate count for '{text}': found {} copies ({matches:?})",
                        matches.len()
                    ));
                }
                ordered_ids.push(matches[0]);
            }
            if !ordered_ids.windows(2).all(|w| w[0] < w[1]) {
                return Err(format!(
                    "step 7: recovered 5 messages out of order: {ordered_ids:?}"
                ));
            }
            println!(
                "  [step 7] TCP drop + reconnect ({connects_before} -> {connects_after}) recovered 5/5 messages in order {:?}, 0 duplicates!",
                ordered_ids.iter().map(|m| m.0).collect::<Vec<_>>()
            );

            Ok(format!(
                "connects {connects_before}->{connects_after}, 5/5 in order {:?}, 0 duplicates",
                ordered_ids.iter().map(|m| m.0).collect::<Vec<_>>()
            ))
        }
        .await
    };
    step_results.push((
        "Step 7 (TCP drop on B + 5 msgs + getDifference recovery)",
        step7_res,
    ));

    // =========================================================================
    // Step 8: Clean up unconditionally: delete private-chat history for both
    //         sides (messages.deleteHistory with revoke) and delete the group.
    //         Never log out or delete either account.
    // =========================================================================
    println!(
        "[step 8] Cleaning up A<->B private chat history (revoke=true) and shared supergroup..."
    );
    guard_a.set_block_connects(false);
    guard_b.set_block_connects(false);
    let mut cleanup_errors = Vec::new();
    let mut cleanup_notes = Vec::new();

    if private_chat_ready {
        let input_peer_b_on_a = {
            let sa = rt_a.session().read().await;
            input_peer_for(&sa, peer_b)
        };
        let input_peer_a_on_b = {
            let sb = rt_b.session().read().await;
            input_peer_for(&sb, peer_a)
        };
        let sender = MessageSender::default();
        match sender
            .delete_history(
                &rpc_a,
                tl_fns::messages::DeleteHistory {
                    just_clear: false,
                    revoke: true,
                    peer: input_peer_b_on_a.clone(),
                    max_id: 0,
                    min_date: None,
                    max_date: None,
                },
            )
            .await
        {
            Ok(DeleteHistoryOutcome::Affected(pages)) => {
                for p in &pages {
                    let _ = rt_a.apply_affected_history(None, p.pts, p.pts_count).await;
                }
                println!(
                    "  [step 8] A messages.deleteHistory(revoke=true) succeeded ({} page(s))",
                    pages.len()
                );
                cleanup_notes.push(format!(
                    "A deleteHistory(revoke=true, pages={})",
                    pages.len()
                ));
            }
            Ok(other) => {
                cleanup_notes.push(format!("A deleteHistory returned {other:?}"));
            }
            Err(e) => {
                cleanup_errors.push(format!("A messages.deleteHistory(revoke=true) failed: {e}"));
            }
        }

        // Verify both A and B private chat histories are empty on the server; if B still has
        // any residual history, run deleteHistory on B as well.
        let check_hist_count = |resp: tl_enums::messages::Messages| -> usize {
            match resp {
                tl_enums::messages::Messages::Messages(m) => m.messages.len(),
                tl_enums::messages::Messages::Slice(m) => m.messages.len(),
                tl_enums::messages::Messages::ChannelMessages(m) => m.messages.len(),
                tl_enums::messages::Messages::NotModified(_) => 0,
            }
        };
        let hist_req = |peer: tl_enums::InputPeer| tl_fns::messages::GetHistory {
            peer,
            offset_id: 0,
            offset_date: 0,
            add_offset: 0,
            limit: 10,
            max_id: 0,
            min_id: 0,
            hash: 0,
        };
        let mut b_remaining = match rpc_b.invoke(&hist_req(input_peer_a_on_b.clone())).await {
            Ok(r) => check_hist_count(r),
            Err(e) => {
                cleanup_errors.push(format!("B verify getHistory failed: {e}"));
                0
            }
        };
        if b_remaining > 0 {
            match sender
                .delete_history(
                    &rpc_b,
                    tl_fns::messages::DeleteHistory {
                        just_clear: false,
                        revoke: true,
                        peer: input_peer_a_on_b.clone(),
                        max_id: 0,
                        min_date: None,
                        max_date: None,
                    },
                )
                .await
            {
                Ok(_) => {
                    b_remaining = rpc_b
                        .invoke(&hist_req(input_peer_a_on_b))
                        .await
                        .map(check_hist_count)
                        .unwrap_or(b_remaining);
                }
                Err(e) => {
                    cleanup_errors
                        .push(format!("B messages.deleteHistory(revoke=true) failed: {e}"));
                }
            }
        }
        let a_remaining = match rpc_a.invoke(&hist_req(input_peer_b_on_a)).await {
            Ok(r) => check_hist_count(r),
            Err(e) => {
                cleanup_errors.push(format!("A verify getHistory failed: {e}"));
                0
            }
        };
        if a_remaining != 0 || b_remaining != 0 {
            cleanup_errors.push(format!(
                "private chat not empty after deleteHistory (A remaining={a_remaining}, B remaining={b_remaining})"
            ));
        } else {
            println!("  [step 8] Verified private chat A<->B is empty on both A and B (0 msgs).");
            cleanup_notes.push("A&B private chat verified empty".to_owned());
        }
    }

    if let Some((ch_id, access_hash)) = created_group.take() {
        let _ = rt_a.set_active_channel(ch_id, false).await;
        let _ = rt_b.set_active_channel(ch_id, false).await;
        let input_ch = tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
            channel_id: ch_id.0 as i64,
            access_hash,
        });
        match rpc_a
            .invoke(&tl_fns::channels::DeleteChannel { channel: input_ch })
            .await
        {
            Ok(upd) => {
                let _ = rt_a.apply_updates(upd).await;
                println!(
                    "  [step 8] Deleted shared supergroup channel_id={} OK.",
                    ch_id.0
                );
                cleanup_notes.push(format!("deleted supergroup ch_id={}", ch_id.0));
            }
            Err(e) => {
                cleanup_errors.push(format!("channels.deleteChannel({}) failed: {e}", ch_id.0));
            }
        }
    }

    let step8_res = if cleanup_errors.is_empty() {
        Ok(cleanup_notes.join("; "))
    } else {
        Err(cleanup_errors.join("; "))
    };
    step_results.push(("Step 8 (cleanup private chat + supergroup)", step8_res));

    rt_a.shutdown_async().await;
    rt_b.shutdown_async().await;
    mgr_a.shutdown();
    mgr_b.shutdown();

    println!("\n=== TWO-ACCOUNT TEST-DC SMOKE SUMMARY ===");
    let mut failures = Vec::new();
    for (name, res) in &step_results {
        match res {
            Ok(detail) => println!("  PASS | {name}: {detail}"),
            Err(err) => {
                println!("  FAIL | {name}: {err}");
                failures.push(format!("{name}: {err}"));
            }
        }
    }

    if failures.is_empty() {
        println!("\nALL TWO-ACCOUNT TEST-DC SMOKE STEPS PASSED!");
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let mut args = std::env::args().skip(1);
    let mut session_dir: Option<PathBuf> = None;
    let mut session_dir_a: Option<PathBuf> = None;
    let mut session_dir_b: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        if arg == "--session" {
            match args.next() {
                Some(val) => session_dir = Some(PathBuf::from(val)),
                None => {
                    eprintln!("--session requires a directory path");
                    return ExitCode::FAILURE;
                }
            }
        } else if let Some(val) = arg.strip_prefix("--session=") {
            session_dir = Some(PathBuf::from(val));
        } else if arg == "--session-a" {
            match args.next() {
                Some(val) => session_dir_a = Some(PathBuf::from(val)),
                None => {
                    eprintln!("--session-a requires a directory path");
                    return ExitCode::FAILURE;
                }
            }
        } else if let Some(val) = arg.strip_prefix("--session-a=") {
            session_dir_a = Some(PathBuf::from(val));
        } else if arg == "--session-b" {
            match args.next() {
                Some(val) => session_dir_b = Some(PathBuf::from(val)),
                None => {
                    eprintln!("--session-b requires a directory path");
                    return ExitCode::FAILURE;
                }
            }
        } else if let Some(val) = arg.strip_prefix("--session-b=") {
            session_dir_b = Some(PathBuf::from(val));
        } else {
            eprintln!(
                "unknown argument: {arg} (usage: test_dc_smoke [--session <dir> | --session-a <dir> --session-b <dir>])"
            );
            return ExitCode::FAILURE;
        }
    }

    if session_dir.is_some() && (session_dir_a.is_some() || session_dir_b.is_some()) {
        eprintln!("cannot combine --session with --session-a/--session-b");
        return ExitCode::FAILURE;
    }
    if session_dir_a.is_some() != session_dir_b.is_some() {
        eprintln!("both --session-a and --session-b must be provided together");
        return ExitCode::FAILURE;
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("failed to build tokio runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    let result = match (session_dir, session_dir_a, session_dir_b) {
        (Some(dir), None, None) => rt.block_on(run_single_account_smoke(dir)),
        (None, Some(dir_a), Some(dir_b)) => rt.block_on(run_two_account_smoke(dir_a, dir_b)),
        (None, None, None) => rt.block_on(run_smoke()),
        _ => unreachable!(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("TEST-DC SMOKE FAILED:\n{err}");
            ExitCode::FAILURE
        }
    }
}
