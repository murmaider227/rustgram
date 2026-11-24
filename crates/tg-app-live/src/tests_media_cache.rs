//! Unit and integration tests for WP-02: live inline media rendering, disk cache integration
//! (`tg-storage`'s `MediaCacheIndex`), CDN redirect downloads with AES-256-CTR + SHA-256 hash
//! checks, `.tgs` sticker decompression, `MessageMedia` conversions, and `SettingsBackend`
//! local storage wiring.
//!
//! All network tests use an in-process `FakeRpcClient` with synthetic data only.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{
    ChannelId, ContactData, DiceData, DocumentData, DocumentId, DocumentKind, ExtendedMedia,
    GameData, GameId, GeoPoint, GiveawayData, GiveawayResultsData, InvoiceData, LiveLocationData,
    MediaWebPageFlags, Message, MessageContent, MessageFlags, MessageMedia, MsgId as DataMsgId,
    PaidMediaData, PeerId as DataPeerId, PhotoData, PhotoId, PhotoSize, PollAnswer,
    PollAnswerVoters, PollData, PollId, PollResults, Session, StickerMeta, StoryMediaData,
    TextWithEntities as DataText, UserId as DataUserId, VenueData, WebPageData, WebPageId,
};
use tg_media::transfer::{CDN_HASH_CHUNK_SIZE, DOWNLOAD_PART_SIZE, apply_cdn_aes_ctr};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_storage::cache::media_cache::MediaCacheTag;
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tokio::sync::broadcast;

use crate::LiveSession;
use crate::convert::message_to_history_message;
use crate::media::{document_thumb_location, download_file_bytes_async, photo_to_location};
use crate::media_cache::{
    DEFAULT_CACHE_SIZE_LIMIT_BYTES, DEFAULT_CACHE_TIME_LIMIT_DAYS, LiveMediaCache,
    decompress_tgs_to_lottie_json, has_persisted_index,
};
use tg_app_core::core_types::dto::inline_media::PhotoLoadState;
use tg_app_core::core_types::misc::InlineDocumentData;
use tg_app_core::history::backend::{HistoryBackend, HistoryMedia};
use tg_app_core::settings::backend::SettingsBackend;
use tg_app_core::stand_in::types::{MsgId, PeerId};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

// =============================================================================
// FakeRpcClient & Synthetic Helpers
// =============================================================================

type RpcDynamicHandler =
    Arc<dyn Fn(DcId, u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

type QueuedRpcResp = (u32, Result<Vec<u8>, InvocationError>);

#[derive(Clone)]
struct MediaFakeRpcClient {
    main_dc: Arc<Mutex<DcId>>,
    recorded: Arc<Mutex<Vec<(DcId, u32)>>>,
    responses: Arc<Mutex<VecDeque<QueuedRpcResp>>>,
    dynamic: Arc<Mutex<Option<RpcDynamicHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl MediaFakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc: Arc::new(Mutex::new(main_dc)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::new())),
            dynamic: Arc::new(Mutex::new(None)),
            updates_tx,
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn set_dynamic_handler(&self, handler: RpcDynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
    }
}

impl RpcClient for MediaFakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let dc = self.main_dc_id();
        self.invoke_in_dc(dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        self.recorded
            .lock()
            .expect("lock recorded")
            .push((dc_id, ctor));

        if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
            && let Some(res) = handler(dc_id, ctor, &bytes)
        {
            let raw = res?;
            return R::Return::from_bytes(&raw)
                .map_err(|e| InvocationError::Deserialize(e.to_string()));
        }

        let next = {
            let mut q = self.responses.lock().expect("lock responses");
            if let Some(pos) = q.iter().position(|(c, _)| *c == ctor) {
                q.remove(pos).map(|(_, r)| r)
            } else {
                q.pop_front().map(|(_, r)| r)
            }
        };

        let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        *self.main_dc.lock().expect("lock main_dc")
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        *self.main_dc.lock().expect("lock main_dc") = dc_id;
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn synthetic_user(id: i64, first_name: &str, username: &str, is_self: bool) -> tl_types::User {
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
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id,
        access_hash: Some(id * 100 + 3),
        first_name: Some(first_name.to_owned()),
        last_name: None,
        username: Some(username.to_owned()),
        phone: None,
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

fn start_live_session() -> (TempDir, MediaFakeRpcClient, LiveSession, PeerId) {
    let dir = TempDir::new().expect("tempdir");
    let fake = MediaFakeRpcClient::new(2);
    let self_uid = 700_001i64;
    let peer_uid = 700_002i64;
    fake.enqueue_ok(
        tl_fns::users::GetUsers::CONSTRUCTOR_ID,
        &vec![tl_enums::User::User(synthetic_user(
            self_uid,
            "UnitSelf",
            "unit_self",
            true,
        ))],
    );
    fake.enqueue_ok(
        tl_fns::updates::GetState::CONSTRUCTOR_ID,
        &tl_enums::updates::State::State(tl_types::updates::State {
            pts: 10,
            qts: 0,
            date: 1_750_000_000,
            seq: 1,
            unread_count: 0,
        }),
    );

    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: true,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId(self_uid as u64),
            crate::ErasedRpcClient::new(fake.clone()),
            cfg,
        )
        .expect("start SessionRuntime"),
    );
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !runtime.is_ready() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let live = LiveSession::new(Arc::clone(&runtime), 42);
    live.set_media_cache_dir(dir.path().join("media_cache"))
        .expect("reopen media cache in TempDir");

    // Seed peer into Session directly.
    let session_arc = Arc::clone(runtime.session());
    runtime.tokio_handle().block_on(async move {
        let mut guard = session_arc.write().await;
        tg_api::convert::apply_tl_users_and_chats(
            &mut guard,
            &[tl_enums::User::User(synthetic_user(
                peer_uid,
                "MediaPeer",
                "media_peer",
                false,
            ))],
            &[],
        );
    });

    (
        dir,
        fake,
        live,
        PeerId(DataPeerId::from_user(DataUserId(peer_uid as u64)).0),
    )
}

fn insert_message_into_live(live: &LiveSession, msg: Message) {
    let session_arc = Arc::clone(live.runtime().session());
    live.runtime().tokio_handle().block_on(async move {
        let mut guard = session_arc.write().await;
        guard.insert_message(msg);
    });
}

fn make_regular_msg(peer: PeerId, id: i32, media: MessageMedia, flags: MessageFlags) -> Message {
    let data_peer = DataPeerId(peer.0);
    let mut msg = Message::new_text(
        data_peer,
        DataMsgId(i64::from(id)),
        Some(data_peer),
        1_750_000_100,
        DataText::plain(String::new()),
    );
    msg.flags = flags;
    msg.content = MessageContent::Regular {
        text: DataText::plain(String::new()),
        media,
    };
    msg
}

/// Minimal pure-Rust RFC 1952 gzip encoder using stored (uncompressed) DEFLATE blocks.
/// Produces valid `.tgs` bytes accepted by `tg_lottie::unpack_gzip` (`flate2::GzDecoder`).
fn gzip_stored_bytes(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= u16::MAX as usize);
    let mut out = Vec::with_capacity(payload.len() + 23);
    // Gzip 10-byte header: ID1=0x1f, ID2=0x8b, CM=8 (DEFLATE), FLG=0, MTIME=0, XFL=0, OS=255
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff]);
    // Single final stored DEFLATE block: BFINAL=1, BTYPE=00 -> byte 0x01
    out.push(0x01);
    let len = payload.len() as u16;
    let nlen = !len;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&nlen.to_le_bytes());
    out.extend_from_slice(payload);
    // Standard IEEE 802.3 CRC-32
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in payload {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc = !crc;
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out
}

fn minimal_valid_lottie_json() -> &'static str {
    r#"{"v":"5.5.2","fr":60,"ip":0,"op":60,"w":512,"h":512,"layers":[]}"#
}

fn wait_until(pred: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    pred()
}

// =============================================================================
// 1. CDN Redirect, AES-256-CTR, ReuploadNeeded, GetCdnFileHashes & Hash Check
// =============================================================================

#[test]
fn cdn_redirect_reupload_and_delayed_hashes_decrypt_and_verify() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    // 200 KiB payload spanning two 128 KiB CDN hash chunks (128 KiB + 72 KiB).
    let total_len = (CDN_HASH_CHUNK_SIZE as usize) + 72 * 1024;
    let plaintext: Vec<u8> = (0..total_len)
        .map(|i| ((i * 31 + 7) & 0xFF) as u8)
        .collect();
    let key = [0x42u8; 32];
    let iv = [0x19u8; 16];
    let file_token = b"cdn_token_alpha".to_vec();
    let req_token = b"reup_req_token_1".to_vec();
    let home_dc = 2;
    let cdn_dc = 203;

    let hash_chunk_0 = tg_crypto::sha256(&[&plaintext[..CDN_HASH_CHUNK_SIZE as usize]]).to_vec();
    let hash_chunk_1 = tg_crypto::sha256(&[&plaintext[CDN_HASH_CHUNK_SIZE as usize..]]).to_vec();

    let reupload_triggered = Arc::new(AtomicUsize::new(0));
    let get_file_count = Arc::new(AtomicUsize::new(0));
    let get_cdn_count = Arc::new(AtomicUsize::new(0));
    let reupload_count = Arc::new(AtomicUsize::new(0));
    let get_hashes_count = Arc::new(AtomicUsize::new(0));

    let fake = MediaFakeRpcClient::new(home_dc);
    {
        let plaintext = plaintext.clone();
        let file_token = file_token.clone();
        let req_token = req_token.clone();
        let hash_chunk_0 = hash_chunk_0.clone();
        let hash_chunk_1 = hash_chunk_1.clone();
        let reupload_triggered = Arc::clone(&reupload_triggered);
        let get_file_count = Arc::clone(&get_file_count);
        let get_cdn_count = Arc::clone(&get_cdn_count);
        let reupload_count = Arc::clone(&reupload_count);
        let get_hashes_count = Arc::clone(&get_hashes_count);

        fake.set_dynamic_handler(Arc::new(move |dc, ctor, bytes| {
            if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                assert_eq!(dc, home_dc);
                get_file_count.fetch_add(1, Ordering::SeqCst);
                // Return CDN redirect containing ONLY hash for offset 0 so chunk 1 must call getCdnFileHashes.
                let redir =
                    tl_enums::upload::File::CdnRedirect(tl_types::upload::FileCdnRedirect {
                        dc_id: cdn_dc,
                        file_token: file_token.clone(),
                        encryption_key: key.to_vec(),
                        encryption_iv: iv.to_vec(),
                        file_hashes: vec![tl_enums::FileHash::FileHash(tl_types::FileHash {
                            offset: 0,
                            limit: CDN_HASH_CHUNK_SIZE as i32,
                            hash: hash_chunk_0.clone(),
                        })],
                    });
                return Some(Ok(redir.to_bytes()));
            }
            if ctor == tl_fns::upload::GetCdnFile::CONSTRUCTOR_ID {
                assert_eq!(dc, cdn_dc);
                get_cdn_count.fetch_add(1, Ordering::SeqCst);
                // Parse offset & limit from serialized GetCdnFile (skip 4-byte ctor + TL bytes file_token)
                let mut cur = tg_tl::Cursor::new(&bytes[4..]);
                let parsed_token = Vec::<u8>::deserialize(&mut cur).expect("token");
                let offset = i64::deserialize(&mut cur).expect("offset") as usize;
                let limit = i32::deserialize(&mut cur).expect("limit") as usize;
                assert_eq!(parsed_token, file_token);

                if offset == 0 && reupload_triggered.fetch_add(1, Ordering::SeqCst) == 0 {
                    let resp = tl_enums::upload::CdnFile::ReuploadNeeded(
                        tl_types::upload::CdnFileReuploadNeeded {
                            request_token: req_token.clone(),
                        },
                    );
                    return Some(Ok(resp.to_bytes()));
                }

                let end = (offset + limit).min(plaintext.len());
                let mut enc = plaintext[offset..end].to_vec();
                apply_cdn_aes_ctr(&mut enc, &key, &iv, offset as u64).expect("encrypt cdn");
                let resp =
                    tl_enums::upload::CdnFile::CdnFile(tl_types::upload::CdnFile { bytes: enc });
                return Some(Ok(resp.to_bytes()));
            }
            if ctor == tl_fns::upload::ReuploadCdnFile::CONSTRUCTOR_ID {
                assert_eq!(dc, home_dc);
                reupload_count.fetch_add(1, Ordering::SeqCst);
                let resp: Vec<tl_enums::FileHash> = Vec::new();
                return Some(Ok(resp.to_bytes()));
            }
            if ctor == tl_fns::upload::GetCdnFileHashes::CONSTRUCTOR_ID {
                assert_eq!(dc, home_dc);
                get_hashes_count.fetch_add(1, Ordering::SeqCst);
                let resp = vec![tl_enums::FileHash::FileHash(tl_types::FileHash {
                    offset: i64::from(CDN_HASH_CHUNK_SIZE),
                    limit: (plaintext.len() - CDN_HASH_CHUNK_SIZE as usize) as i32,
                    hash: hash_chunk_1.clone(),
                })];
                return Some(Ok(resp.to_bytes()));
            }
            None
        }));
    }

    let loc = tl_enums::InputFileLocation::InputDocumentFileLocation(
        tl_types::InputDocumentFileLocation {
            id: 9001,
            access_hash: 12345,
            file_reference: vec![1, 2, 3],
            thumb_size: String::new(),
        },
    );

    let downloaded = rt
        .block_on(download_file_bytes_async(
            &fake,
            home_dc,
            loc,
            Some(plaintext.len() as u64),
        ))
        .expect("CDN download should succeed");

    assert_eq!(downloaded, plaintext);
    assert_eq!(get_file_count.load(Ordering::SeqCst), 1);
    assert_eq!(reupload_count.load(Ordering::SeqCst), 1);
    assert_eq!(get_hashes_count.load(Ordering::SeqCst), 1);
    assert_eq!(get_cdn_count.load(Ordering::SeqCst), 3); // 1 reuploadNeeded + 2 data chunks
}

#[test]
fn cdn_hash_mismatch_rejects_corrupted_payload() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    let plaintext = vec![0xABu8; 16 * 1024];
    let key = [0x11u8; 32];
    let iv = [0x22u8; 16];
    let good_hash = tg_crypto::sha256(&[&plaintext]).to_vec();

    let fake = MediaFakeRpcClient::new(2);
    fake.set_dynamic_handler(Arc::new(move |_dc, ctor, _bytes| {
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let redir = tl_enums::upload::File::CdnRedirect(tl_types::upload::FileCdnRedirect {
                dc_id: 203,
                file_token: b"tok".to_vec(),
                encryption_key: key.to_vec(),
                encryption_iv: iv.to_vec(),
                file_hashes: vec![tl_enums::FileHash::FileHash(tl_types::FileHash {
                    offset: 0,
                    limit: plaintext.len() as i32,
                    hash: good_hash.clone(),
                })],
            });
            return Some(Ok(redir.to_bytes()));
        }
        if ctor == tl_fns::upload::GetCdnFile::CONSTRUCTOR_ID {
            let mut enc = plaintext.clone();
            apply_cdn_aes_ctr(&mut enc, &key, &iv, 0).expect("enc");
            enc[100] ^= 0xFF; // Corrupt one byte in transit!
            return Some(Ok(tl_enums::upload::CdnFile::CdnFile(
                tl_types::upload::CdnFile { bytes: enc },
            )
            .to_bytes()));
        }
        None
    }));

    let loc =
        tl_enums::InputFileLocation::InputPhotoFileLocation(tl_types::InputPhotoFileLocation {
            id: 404,
            access_hash: 1,
            file_reference: vec![],
            thumb_size: "x".to_owned(),
        });

    let err = rt
        .block_on(download_file_bytes_async(&fake, 2, loc, Some(16 * 1024)))
        .expect_err("corrupted CDN chunk must fail SHA-256 verification");
    assert!(
        err.to_string().contains("hash mismatch"),
        "unexpected error: {err}"
    );
}

#[test]
fn cdn_token_invalid_falls_back_to_home_dc() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    let payload = b"direct fallback photo payload bytes".to_vec();
    let get_file_calls = Arc::new(AtomicUsize::new(0));
    let fake = MediaFakeRpcClient::new(2);
    {
        let payload = payload.clone();
        let get_file_calls = Arc::clone(&get_file_calls);
        fake.set_dynamic_handler(Arc::new(move |_dc, ctor, _bytes| {
            if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                let n = get_file_calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    let redir =
                        tl_enums::upload::File::CdnRedirect(tl_types::upload::FileCdnRedirect {
                            dc_id: 203,
                            file_token: b"expired_tok".to_vec(),
                            encryption_key: vec![0x33u8; 32],
                            encryption_iv: vec![0x44u8; 16],
                            file_hashes: Vec::new(),
                        });
                    return Some(Ok(redir.to_bytes()));
                }
                return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FileJpeg,
                    mtime: 0,
                    bytes: payload.clone(),
                })
                .to_bytes()));
            }
            if ctor == tl_fns::upload::GetCdnFile::CONSTRUCTOR_ID {
                return Some(Err(InvocationError::Rpc(tg_api::RpcError {
                    code: 400,
                    name: "FILE_TOKEN_INVALID".to_owned(),
                    value: None,
                })));
            }
            None
        }));
    }

    let loc =
        tl_enums::InputFileLocation::InputPhotoFileLocation(tl_types::InputPhotoFileLocation {
            id: 505,
            access_hash: 2,
            file_reference: vec![],
            thumb_size: "m".to_owned(),
        });

    let got = rt
        .block_on(download_file_bytes_async(
            &fake,
            2,
            loc,
            Some(payload.len() as u64),
        ))
        .expect("home DC fallback must succeed");
    assert_eq!(got, payload);
    assert_eq!(get_file_calls.load(Ordering::SeqCst), 2);
}

// =============================================================================
// 2. LiveSession::inline_media_descriptor + Async Downloads + Disk Cache
// =============================================================================

#[test]
fn live_inline_photo_downloads_async_caches_and_serves_without_redownload() {
    let (_dir, fake, live, peer_id) = start_live_session();
    let photo_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4, 5, 6, 7, 8];
    let get_file_calls = Arc::new(AtomicUsize::new(0));
    {
        let photo_bytes = photo_bytes.clone();
        let get_file_calls = Arc::clone(&get_file_calls);
        fake.set_dynamic_handler(Arc::new(move |_dc, ctor, _bytes| {
            if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                get_file_calls.fetch_add(1, Ordering::SeqCst);
                return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FileJpeg,
                    mtime: 1_750_000_000,
                    bytes: photo_bytes.clone(),
                })
                .to_bytes()));
            }
            None
        }));
    }

    let photo = PhotoData {
        id: PhotoId(11_001),
        access_hash: 99,
        file_reference: vec![9, 8],
        date: 1_750_000_100,
        dc_id: 2,
        sizes: vec![
            PhotoSize {
                kind: "i".to_owned(),
                width: 40,
                height: 30,
                size: 120,
            },
            PhotoSize {
                kind: "x".to_owned(),
                width: 800,
                height: 600,
                size: photo_bytes.len() as i64,
            },
        ],
        stripped_thumb: Some(vec![1, 2, 3]),
        spoiler: true,
        ..PhotoData::default()
    };
    // Verify photo_to_location skips stripped "i" and picks "x".
    let loc = photo_to_location(&photo);
    match &loc.location {
        tl_enums::InputFileLocation::InputPhotoFileLocation(p) => assert_eq!(p.thumb_size, "x"),
        other => panic!("unexpected location: {other:?}"),
    }

    let msg = make_regular_msg(
        peer_id,
        101,
        MessageMedia::Photo(Box::new(photo)),
        MessageFlags::empty(),
    );
    insert_message_into_live(&live, msg);

    let initial_gen = live.bridge_state().generation();
    let desc1 = live
        .inline_media_descriptor(peer_id, MsgId(101))
        .expect("photo should produce descriptor");
    assert!(desc1.has_spoiler);
    assert!(
        matches!(
            desc1.photo_load,
            Some(PhotoLoadState::Loading { .. } | PhotoLoadState::Ready { .. })
        ),
        "first call must be Loading or Ready"
    );

    // Wait for async download to finish and notify UI.
    assert!(
        wait_until(|| {
            matches!(
                live.inline_media_descriptor(peer_id, MsgId(101))
                    .and_then(|d| d.photo_load),
                Some(PhotoLoadState::Ready { .. })
            )
        }),
        "photo download did not transition to Ready in time"
    );
    assert!(live.bridge_state().generation() > initial_gen);

    let desc_ready = live
        .inline_media_descriptor(peer_id, MsgId(101))
        .expect("ready descriptor");
    match desc_ready.photo_load {
        Some(PhotoLoadState::Ready { bytes }) => assert_eq!(bytes, photo_bytes),
        other => panic!("expected Ready photo bytes, got {other:?}"),
    }
    // Calling again hits MediaCacheIndex without issuing a second upload.getFile RPC.
    assert_eq!(get_file_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        live.local_storage_stats().photos_bytes,
        photo_bytes.len() as i64
    );
}

#[test]
fn live_inline_tgs_sticker_decompresses_into_lottie_json_and_video_document_thumbs_load() {
    let (_dir, fake, live, peer_id) = start_live_session();
    let expected_json = minimal_valid_lottie_json();
    let tgs_bytes = gzip_stored_bytes(expected_json.as_bytes());
    let video_thumb_bytes = b"JPEG_VIDEO_THUMB_BYTES".to_vec();
    let doc_thumb_bytes = b"JPEG_DOC_THUMB_BYTES".to_vec();
    let round_bytes = b"MP4_ROUND_VIDEO_BYTES".to_vec();
    let voice_bytes = b"OGG_OPUS_VOICE_BYTES".to_vec();

    // Verify helper directly first.
    let unpacked = decompress_tgs_to_lottie_json(&tgs_bytes).expect("decompress tgs");
    assert_eq!(unpacked, expected_json);
    assert!(tg_lottie::Composition::from_json(&unpacked).is_ok());

    {
        let tgs_bytes = tgs_bytes.clone();
        let video_thumb_bytes = video_thumb_bytes.clone();
        let doc_thumb_bytes = doc_thumb_bytes.clone();
        let round_bytes = round_bytes.clone();
        let voice_bytes = voice_bytes.clone();
        fake.set_dynamic_handler(Arc::new(move |_dc, ctor, bytes| {
            if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                let mut cur = tg_tl::Cursor::new(&bytes[4..]);
                let _flags = i32::deserialize(&mut cur).expect("flags");
                let loc = tl_enums::InputFileLocation::deserialize(&mut cur).expect("loc");
                let payload = match loc {
                    tl_enums::InputFileLocation::InputDocumentFileLocation(d) => match d.id {
                        20_001 => tgs_bytes.clone(),
                        20_002 => {
                            assert_eq!(d.thumb_size, "m");
                            video_thumb_bytes.clone()
                        }
                        20_003 => {
                            assert_eq!(d.thumb_size, "s");
                            doc_thumb_bytes.clone()
                        }
                        20_004 => round_bytes.clone(),
                        20_005 => voice_bytes.clone(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FileUnknown,
                    mtime: 0,
                    bytes: payload,
                })
                .to_bytes()));
            }
            None
        }));
    }

    // 1. Animated .tgs sticker
    let tgs_doc = DocumentData {
        id: DocumentId(20_001),
        access_hash: 1,
        dc_id: 2,
        mime_type: "application/x-tgsticker".to_owned(),
        size: tgs_bytes.len() as i64,
        kind: DocumentKind::AnimatedSticker,
        width: Some(512),
        height: Some(512),
        sticker: Some(StickerMeta {
            alt: "✨".to_owned(),
            set_id: 1,
            set_access_hash: 2,
            is_mask: false,
        }),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            201,
            MessageMedia::Document(Box::new(tgs_doc)),
            MessageFlags::empty(),
        ),
    );

    // 2. Video with thumbnail (huge body size so only thumb downloads!)
    let video_doc = DocumentData {
        id: DocumentId(20_002),
        access_hash: 2,
        dc_id: 2,
        mime_type: "video/mp4".to_owned(),
        size: 150 * 1024 * 1024,
        kind: DocumentKind::Video,
        width: Some(1280),
        height: Some(720),
        duration: Some(45.0),
        thumbs: vec![PhotoSize {
            kind: "m".to_owned(),
            width: 320,
            height: 180,
            size: video_thumb_bytes.len() as i64,
        }],
        ..DocumentData::default()
    };
    assert!(document_thumb_location(&video_doc).is_some());
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            202,
            MessageMedia::Document(Box::new(video_doc)),
            MessageFlags::empty(),
        ),
    );

    // 3. Document with thumbnail
    let file_doc = DocumentData {
        id: DocumentId(20_003),
        access_hash: 3,
        dc_id: 2,
        mime_type: "application/pdf".to_owned(),
        file_name: "spec.pdf".to_owned(),
        size: 4_000_000,
        kind: DocumentKind::File,
        thumbs: vec![PhotoSize {
            kind: "s".to_owned(),
            width: 90,
            height: 90,
            size: doc_thumb_bytes.len() as i64,
        }],
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            203,
            MessageMedia::Document(Box::new(file_doc)),
            MessageFlags::empty(),
        ),
    );

    // 4. Round video
    let round_doc = DocumentData {
        id: DocumentId(20_004),
        access_hash: 4,
        dc_id: 2,
        mime_type: "video/mp4".to_owned(),
        size: round_bytes.len() as i64,
        kind: DocumentKind::RoundVideo,
        width: Some(240),
        height: Some(240),
        duration: Some(12.0),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            204,
            MessageMedia::Document(Box::new(round_doc)),
            MessageFlags::empty(),
        ),
    );

    // 5. Voice note (unread flag set)
    let voice_doc = DocumentData {
        id: DocumentId(20_005),
        access_hash: 5,
        dc_id: 2,
        mime_type: "audio/ogg".to_owned(),
        size: voice_bytes.len() as i64,
        kind: DocumentKind::Voice,
        duration: Some(6.0),
        waveform: Some(vec![0x1F, 0xAA, 0x55]),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            205,
            MessageMedia::Document(Box::new(voice_doc)),
            MessageFlags::MEDIA_IS_UNREAD,
        ),
    );

    // Trigger inline descriptors for all 5 messages.
    for mid in [201, 202, 203, 204, 205] {
        let _ = live.inline_media_descriptor(peer_id, MsgId(mid));
    }

    // Wait for all 5 background downloads to land in MediaCache.
    assert!(wait_until(|| {
        let d_vid = live.inline_media_descriptor(peer_id, MsgId(202));
        let d_doc = live.inline_media_descriptor(peer_id, MsgId(203));
        let d_rnd = live.inline_media_descriptor(peer_id, MsgId(204));
        let d_voi = live.inline_media_descriptor(peer_id, MsgId(205));
        let slice = live.load_slice_around(peer_id, MsgId::SHOW_AT_END, 20, 20);
        let sticker_ready = slice.messages.iter().any(|m| {
            m.id == MsgId(201)
                && matches!(&m.media, Some(HistoryMedia::Sticker(s)) if !s.lottie_json.is_empty())
        });
        sticker_ready
            && matches!(
                d_vid.and_then(|d| d.photo_load),
                Some(PhotoLoadState::Ready { .. })
            )
            && matches!(
                d_doc.and_then(|d| d.photo_load),
                Some(PhotoLoadState::Ready { .. })
            )
            && d_rnd.and_then(|d| d.video_data).is_some()
            && d_voi.and_then(|d| d.audio_bytes).is_some()
    }));

    // Verify exact contents for each media type:
    let slice = live.load_slice_around(peer_id, MsgId::SHOW_AT_END, 20, 20);
    let sticker_msg = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(201))
        .expect("sticker msg");
    match &sticker_msg.media {
        Some(HistoryMedia::Sticker(st)) => {
            assert_eq!(st.lottie_json, expected_json);
            assert_eq!(st.emoji, "✨");
        }
        other => panic!("expected Sticker, got {other:?}"),
    }

    let voice_msg = slice
        .messages
        .iter()
        .find(|m| m.id == MsgId(205))
        .expect("voice msg");
    match &voice_msg.media {
        Some(HistoryMedia::Voice(v)) => assert!(v.is_unread),
        other => panic!("expected Voice, got {other:?}"),
    }

    let d_vid = live
        .inline_media_descriptor(peer_id, MsgId(202))
        .expect("vid desc");
    match d_vid.photo_load {
        Some(PhotoLoadState::Ready { bytes }) => assert_eq!(bytes, video_thumb_bytes),
        other => panic!("expected video thumb Ready, got {other:?}"),
    }

    let d_doc = live
        .inline_media_descriptor(peer_id, MsgId(203))
        .expect("doc desc");
    match d_doc.photo_load {
        Some(PhotoLoadState::Ready { bytes }) => assert_eq!(bytes, doc_thumb_bytes),
        other => panic!("expected doc thumb Ready, got {other:?}"),
    }

    let d_rnd = live
        .inline_media_descriptor(peer_id, MsgId(204))
        .expect("round desc");
    match d_rnd.video_data {
        Some(InlineDocumentData::Bytes(bytes)) => assert_eq!(bytes, round_bytes),
        other => panic!("expected round video Bytes, got {other:?}"),
    }
    assert_eq!(d_rnd.audio_bytes.as_deref(), Some(round_bytes.as_slice()));

    let d_voi = live
        .inline_media_descriptor(peer_id, MsgId(205))
        .expect("voice desc");
    assert_eq!(d_voi.audio_bytes.as_deref(), Some(voice_bytes.as_slice()));
    assert_eq!(d_voi.audio_ext.as_deref(), Some("ogg"));
}

// =============================================================================
// 3. SettingsBackend Local Storage Stats, Limits & Clear (Real MediaCache)
// =============================================================================

#[test]
fn settings_local_storage_stats_limits_prune_and_clear_use_real_media_cache() {
    let (dir, _fake, mut live, _peer_id) = start_live_session();
    let cache_dir = dir.path().join("media_cache");
    assert!(has_persisted_index(&cache_dir));

    // Verify tdesktop defaults from storage_cache_types.h (1 GiB, 31 days).
    let initial = live.local_storage_stats();
    assert_eq!(initial.total_bytes(), 0);
    assert_eq!(initial.size_limit_bytes, DEFAULT_CACHE_SIZE_LIMIT_BYTES);
    assert_eq!(initial.time_limit_days, DEFAULT_CACHE_TIME_LIMIT_DAYS);

    // Populate real blobs across categories.
    live.media_cache()
        .put_blob("photo:2:1:x", MediaCacheTag::Photo, &vec![1u8; 400])
        .expect("put photo");
    live.media_cache()
        .put_blob("doc:2:2", MediaCacheTag::Video, &vec![2u8; 600])
        .expect("put video");
    live.media_cache()
        .put_blob("doc:2:3", MediaCacheTag::Document, &vec![3u8; 500])
        .expect("put doc");
    live.media_cache()
        .put_blob("doc:2:4", MediaCacheTag::Sticker, &vec![4u8; 300])
        .expect("put sticker");

    let stats = live.local_storage_stats();
    assert_eq!(stats.photos_bytes, 400);
    assert_eq!(stats.videos_bytes, 600);
    assert_eq!(stats.files_bytes, 500);
    assert_eq!(stats.stickers_bytes, 300);
    assert_eq!(stats.total_bytes(), 1800);

    // Verify persistence across reopen of LiveMediaCache on the same directory.
    let reopened = LiveMediaCache::open(&cache_dir).expect("reopen cache");
    let reopened_stats = reopened.stats();
    assert_eq!(reopened_stats.photos_bytes, 400);
    assert_eq!(reopened_stats.videos_bytes, 600);
    assert_eq!(reopened_stats.files_bytes, 500);
    assert_eq!(reopened_stats.stickers_bytes, 300);
    assert_eq!(reopened_stats.total_bytes(), 1800);

    // Setting a tight size limit immediately prunes MediaCacheIndex via LRU.
    live.set_local_storage_limits(1000, 7);
    let after_limit = live.local_storage_stats();
    assert_eq!(after_limit.size_limit_bytes, 1000);
    assert_eq!(after_limit.time_limit_days, 7);
    assert!(
        after_limit.total_bytes() <= 1000,
        "expected total <= 1000 after limit prune, got {}",
        after_limit.total_bytes()
    );
    assert!(after_limit.total_bytes() > 0);

    // Clearing local storage deletes all cached blobs and returns freed byte count.
    let expected_freed = after_limit.total_bytes();
    let freed = live.clear_local_storage();
    assert_eq!(freed, expected_freed);
    let after_clear = live.local_storage_stats();
    assert_eq!(after_clear.total_bytes(), 0);
    assert_eq!(after_clear.photos_bytes, 0);
    assert_eq!(after_clear.videos_bytes, 0);
    assert_eq!(after_clear.files_bytes, 0);
    assert_eq!(after_clear.stickers_bytes, 0);
}

// =============================================================================
// 4. All Remaining MessageMedia Variant Conversions in live/convert.rs
// =============================================================================

#[test]
fn convert_all_message_media_variants() {
    let mut session = Session::new(DataUserId(1));
    let peer = DataPeerId::from_user(DataUserId(2));
    let bot_uid = DataUserId(99);
    let mut bot_tl = synthetic_user(99, "ArcadeBot", "arcade_bot", false);
    bot_tl.bot = true;
    tg_api::convert::apply_tl_users_and_chats(
        &mut session,
        &[
            tl_enums::User::User(synthetic_user(2, "Sam", "sam_user", false)),
            tl_enums::User::User(bot_tl),
        ],
        &[],
    );

    let mk = |id: i64, media: MessageMedia, flags: MessageFlags| {
        let mut m = Message::new_text(
            peer,
            DataMsgId(id),
            Some(peer),
            1_750_000_000,
            DataText::plain(String::new()),
        );
        m.flags = flags;
        m.content = MessageContent::Regular {
            text: DataText::plain(String::new()),
            media,
        };
        m
    };

    // 1. PaidMedia (locked preview -> Photo + LinkPreviewData)
    let paid_locked = mk(
        1,
        MessageMedia::PaidMedia(Box::new(PaidMediaData {
            stars_amount: 50,
            extended_media: vec![ExtendedMedia::Preview {
                width: Some(800),
                height: Some(600),
                thumb: Some(vec![1, 2, 3]),
                duration: None,
            }],
        })),
        MessageFlags::empty(),
    );
    let hm = message_to_history_message(&session, &paid_locked);
    assert!(matches!(hm.media, Some(HistoryMedia::Photo(ref p)) if p.file_name.contains("50")));
    assert_eq!(
        hm.extra
            .link_preview
            .as_ref()
            .map(|lp| lp.site_name.as_str()),
        Some("Paid Media")
    );

    // PaidMedia (multi-item -> Album)
    let paid_album = mk(
        2,
        MessageMedia::PaidMedia(Box::new(PaidMediaData {
            stars_amount: 100,
            extended_media: vec![
                ExtendedMedia::Preview {
                    width: Some(640),
                    height: Some(480),
                    thumb: None,
                    duration: None,
                },
                ExtendedMedia::Preview {
                    width: Some(640),
                    height: Some(360),
                    thumb: None,
                    duration: Some(15),
                },
            ],
        })),
        MessageFlags::empty(),
    );
    let hm = message_to_history_message(&session, &paid_album);
    assert!(matches!(hm.media, Some(HistoryMedia::Album(ref items)) if items.len() == 2));

    // 2. Giveaway
    let giveaway = mk(
        3,
        MessageMedia::Giveaway(Box::new(GiveawayData {
            channels: vec![ChannelId(12345)],
            countries_iso2: vec!["US".to_owned(), "DE".to_owned()],
            prize_description: Some("Bonus gift".to_owned()),
            quantity: 5,
            months: Some(3),
            stars: None,
            until_date: 1_760_000_000,
            only_new_subscribers: true,
            winners_are_visible: true,
        })),
        MessageFlags::empty(),
    );
    let hm = message_to_history_message(&session, &giveaway);
    assert!(hm.extra.link_preview.is_none());
    let Some(HistoryMedia::Giveaway(ref gw)) = hm.media else {
        panic!("expected HistoryMedia::Giveaway, got {:?}", hm.media);
    };
    assert_eq!(gw.quantity, 5);
    assert_eq!(gw.months, Some(3));
    assert_eq!(gw.prize_description.as_deref(), Some("Bonus gift"));
    assert!(gw.only_new_subscribers);
    assert!(gw.winners_are_visible);
    assert_eq!(gw.countries_iso2, vec!["US".to_owned(), "DE".to_owned()]);

    // 3. GiveawayResults
    let results = mk(
        4,
        MessageMedia::GiveawayResults(Box::new(GiveawayResultsData {
            channel_id: ChannelId(12345),
            additional_peers_count: 1,
            launch_msg_id: DataMsgId(3),
            winners_count: 2,
            unclaimed_count: 1,
            winners: vec![DataUserId(2)],
            months: None,
            stars: Some(500),
            prize_description: None,
            until_date: 1_760_000_000,
            only_new_subscribers: false,
            refunded: false,
        })),
        MessageFlags::empty(),
    );
    let hm = message_to_history_message(&session, &results);
    assert!(hm.extra.link_preview.is_none());
    let Some(HistoryMedia::GiveawayResults(ref gwr)) = hm.media else {
        panic!("expected HistoryMedia::GiveawayResults, got {:?}", hm.media);
    };
    assert_eq!(gwr.winners_count, 2);
    assert_eq!(gwr.stars, Some(500));
    assert_eq!(gwr.unclaimed_count, 1);
    assert!(gwr.winners.iter().any(|(_, name)| name.contains("Sam")));

    // 4. Invoice (USD & XTR Stars)
    let inv = mk(
        5,
        MessageMedia::Invoice(Box::new(InvoiceData {
            title: "Pro Plan".to_owned(),
            description: "Monthly license".to_owned(),
            currency: "USD".to_owned(),
            total_amount: 999,
            start_param: "pro_slug".to_owned(),
            receipt_msg_id: None,
            shipping_address_requested: false,
            test: false,
            extended_media: None,
        })),
        MessageFlags::empty(),
    );
    let hm = message_to_history_message(&session, &inv);
    let lp = hm.extra.link_preview.as_ref().expect("invoice preview");
    assert_eq!(lp.site_name, "Invoice");
    assert!(lp.title.contains("USD 9.99"));
    assert_eq!(lp.url, "tg://invoice?slug=pro_slug");
    assert!(hm.text.text.contains("Pro Plan"));

    let inv_stars = mk(
        6,
        MessageMedia::Invoice(Box::new(InvoiceData {
            title: "Sticker Pack".to_owned(),
            description: String::new(),
            currency: "XTR".to_owned(),
            total_amount: 250,
            start_param: String::new(),
            receipt_msg_id: Some(DataMsgId(99)),
            shipping_address_requested: false,
            test: false,
            extended_media: None,
        })),
        MessageFlags::empty(),
    );
    let hm = message_to_history_message(&session, &inv_stars);
    let lp = hm.extra.link_preview.as_ref().expect("receipt preview");
    assert_eq!(lp.site_name, "Payment Receipt");
    assert!(lp.title.contains("250 ⭐"));

    // 5. Story
    let story = mk(
        7,
        MessageMedia::Story(StoryMediaData {
            peer_id: peer,
            story_id: 77,
            via_mention: true,
        }),
        MessageFlags::empty(),
    );
    let hm = message_to_history_message(&session, &story);
    assert!(hm.extra.link_preview.is_none());
    let Some(HistoryMedia::Story(ref st)) = hm.media else {
        panic!("expected HistoryMedia::Story, got {:?}", hm.media);
    };
    assert!(st.via_mention);
    assert!(st.owner_name.contains("Sam"));
    assert_eq!(st.story_id, 77);

    // 6. Dice
    let dice = mk(
        8,
        MessageMedia::Dice(DiceData {
            value: 6,
            emoticon: "🎯".to_owned(),
        }),
        MessageFlags::empty(),
    );
    let hm = message_to_history_message(&session, &dice);
    match hm.media {
        Some(HistoryMedia::Sticker(st)) => {
            assert_eq!(st.emoji, "🎯");
            assert!(st.set_title.contains("Bullseye"));
        }
        other => panic!("expected Dice as Sticker, got {other:?}"),
    }

    // 7. Geo / Venue / GeoLive
    let geo = mk(
        9,
        MessageMedia::Location(GeoPoint {
            lat: 47.6062,
            lon: -122.3321,
            accuracy_radius: None,
        }),
        MessageFlags::empty(),
    );
    assert!(matches!(
        message_to_history_message(&session, &geo).media,
        Some(HistoryMedia::Location(ref l)) if l.venue_title.is_none() && l.live_period_secs.is_none()
    ));

    let venue = mk(
        10,
        MessageMedia::Venue(Box::new(VenueData {
            geo: GeoPoint {
                lat: 47.61,
                lon: -122.33,
                accuracy_radius: None,
            },
            title: "Central Library".to_owned(),
            address: "1000 Fourth Ave".to_owned(),
            provider: "foursquare".to_owned(),
            venue_id: "v1".to_owned(),
            venue_type: "library".to_owned(),
        })),
        MessageFlags::empty(),
    );
    assert!(matches!(
        message_to_history_message(&session, &venue).media,
        Some(HistoryMedia::Location(ref l)) if l.venue_title.as_deref() == Some("Central Library")
    ));

    let live_loc = mk(
        11,
        MessageMedia::LiveLocation(Box::new(LiveLocationData {
            geo: GeoPoint {
                lat: 47.62,
                lon: -122.34,
                accuracy_radius: Some(15),
            },
            heading: Some(90),
            period: 900,
            proximity_notification_radius: None,
        })),
        MessageFlags::empty(),
    );
    assert!(matches!(
        message_to_history_message(&session, &live_loc).media,
        Some(HistoryMedia::Location(ref l)) if l.live_period_secs == Some(900)
    ));

    // 8. Contact
    let contact = mk(
        12,
        MessageMedia::Contact(Box::new(ContactData {
            phone_number: "+10005550100".to_owned(),
            first_name: "Taylor".to_owned(),
            last_name: "Quinn".to_owned(),
            vcard: String::new(),
            user_id: DataUserId(2),
        })),
        MessageFlags::empty(),
    );
    assert!(matches!(
        message_to_history_message(&session, &contact).media,
        Some(HistoryMedia::Contact(ref c)) if c.first_name == "Taylor" && c.user_id.is_some()
    ));

    // 9. Poll
    let poll = mk(
        13,
        MessageMedia::Poll(Box::new(PollData {
            id: PollId(555),
            question: DataText::plain("Best language?"),
            answers: vec![
                PollAnswer {
                    text: DataText::plain("Rust"),
                    option: vec![0],
                },
                PollAnswer {
                    text: DataText::plain("C++"),
                    option: vec![1],
                },
            ],
            closed: false,
            public_voters: true,
            multiple_choice: false,
            quiz: true,
            close_period: None,
            close_date: None,
            results: PollResults {
                results: vec![PollAnswerVoters {
                    option: vec![0],
                    voters: 10,
                    chosen: true,
                    correct: true,
                }],
                total_voters: Some(10),
                recent_voters: Vec::new(),
                solution: Some(DataText::plain("Memory safety!")),
            },
        })),
        MessageFlags::empty(),
    );
    assert!(matches!(
        message_to_history_message(&session, &poll).media,
        Some(HistoryMedia::Poll(ref p)) if p.is_quiz && p.total_voters == 10 && p.explanation.as_deref() == Some("Memory safety!")
    ));

    // 10. WebPage with INVERT_MEDIA -> above_text = true
    let webpage = mk(
        14,
        MessageMedia::WebPage(Box::new(WebPageData {
            id: WebPageId(888),
            url: "https://example.org/docs".to_owned(),
            display_url: "example.org/docs".to_owned(),
            kind: "article".to_owned(),
            site_name: Some("Example".to_owned()),
            title: Some("Reference Guide".to_owned()),
            description: Some("Full specification".to_owned()),
            photo: None,
            document: None,
            flags: MediaWebPageFlags::FORCE_LARGE_MEDIA,
        })),
        MessageFlags::INVERT_MEDIA,
    );
    let hm = message_to_history_message(&session, &webpage);
    let lp = hm.extra.link_preview.expect("webpage preview");
    assert!(lp.large_media);
    assert!(lp.above_text);
    assert_eq!(lp.site_name, "Example");

    // 11. Game
    let mut game = mk(
        15,
        MessageMedia::Game(Box::new(GameData {
            id: GameId(777),
            access_hash: 42,
            short_name: "star_dash".to_owned(),
            title: "Star Dash".to_owned(),
            description: "Retro space runner".to_owned(),
            photo: Some(PhotoData {
                id: PhotoId(33),
                sizes: vec![PhotoSize {
                    kind: "x".to_owned(),
                    width: 640,
                    height: 360,
                    size: 12000,
                }],
                stripped_thumb: Some(vec![1, 5, 5]),
                ..PhotoData::default()
            }),
            document: None,
        })),
        MessageFlags::empty(),
    );
    game.via_bot_id = Some(bot_uid);
    let hm = message_to_history_message(&session, &game);
    match hm.media {
        Some(HistoryMedia::Game(g)) => {
            assert_eq!(g.game.title, "Star Dash");
            assert_eq!(g.game.short_name, "star_dash");
            assert_eq!(g.bot_username, "arcade_bot");
            assert_eq!(g.media_width, 640);
            assert_eq!(g.media_height, 360);
            assert!(!g.is_animation);
            assert_eq!(g.stripped_thumb.as_deref(), Some(&[1, 5, 5][..]));
        }
        other => panic!("expected Game, got {other:?}"),
    }
}

#[test]
fn cdn_part_size_constant_matches_tdesktop() {
    assert_eq!(DOWNLOAD_PART_SIZE, 128 * 1024);
    assert_eq!(CDN_HASH_CHUNK_SIZE, 128 * 1024);
}

#[test]
fn cdn_reupload_token_invalid_falls_back_to_home_dc() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    let payload = b"fallback after reupload REQUEST_TOKEN_INVALID".to_vec();
    let get_file_calls = Arc::new(AtomicUsize::new(0));
    let fake = MediaFakeRpcClient::new(2);
    {
        let payload = payload.clone();
        let get_file_calls = Arc::clone(&get_file_calls);
        fake.set_dynamic_handler(Arc::new(move |_dc, ctor, _bytes| {
            if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                let n = get_file_calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    let redir =
                        tl_enums::upload::File::CdnRedirect(tl_types::upload::FileCdnRedirect {
                            dc_id: 203,
                            file_token: b"tok_reup".to_vec(),
                            encryption_key: vec![0x55u8; 32],
                            encryption_iv: vec![0x66u8; 16],
                            file_hashes: Vec::new(),
                        });
                    return Some(Ok(redir.to_bytes()));
                }
                return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FileUnknown,
                    mtime: 0,
                    bytes: payload.clone(),
                })
                .to_bytes()));
            }
            if ctor == tl_fns::upload::GetCdnFile::CONSTRUCTOR_ID {
                return Some(Ok(tl_enums::upload::CdnFile::ReuploadNeeded(
                    tl_types::upload::CdnFileReuploadNeeded {
                        request_token: b"stale_req_tok".to_vec(),
                    },
                )
                .to_bytes()));
            }
            if ctor == tl_fns::upload::ReuploadCdnFile::CONSTRUCTOR_ID {
                return Some(Err(InvocationError::Rpc(tg_api::RpcError {
                    code: 400,
                    name: "REQUEST_TOKEN_INVALID".to_owned(),
                    value: None,
                })));
            }
            None
        }));
    }

    let loc = tl_enums::InputFileLocation::InputDocumentFileLocation(
        tl_types::InputDocumentFileLocation {
            id: 606,
            access_hash: 3,
            file_reference: vec![],
            thumb_size: String::new(),
        },
    );

    let got = rt
        .block_on(download_file_bytes_async(
            &fake,
            2,
            loc,
            Some(payload.len() as u64),
        ))
        .expect("reupload REQUEST_TOKEN_INVALID must fall back to home DC");
    assert_eq!(got, payload);
    assert_eq!(get_file_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn cdn_oversized_chunk_is_rejected() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    let fake = MediaFakeRpcClient::new(2);
    fake.set_dynamic_handler(Arc::new(move |_dc, ctor, _bytes| {
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let redir = tl_enums::upload::File::CdnRedirect(tl_types::upload::FileCdnRedirect {
                dc_id: 203,
                file_token: b"tok_big".to_vec(),
                encryption_key: vec![0x11u8; 32],
                encryption_iv: vec![0x22u8; 16],
                file_hashes: Vec::new(),
            });
            return Some(Ok(redir.to_bytes()));
        }
        if ctor == tl_fns::upload::GetCdnFile::CONSTRUCTOR_ID {
            let oversized = vec![0u8; (DOWNLOAD_PART_SIZE as usize) + 16];
            return Some(Ok(tl_enums::upload::CdnFile::CdnFile(
                tl_types::upload::CdnFile { bytes: oversized },
            )
            .to_bytes()));
        }
        None
    }));

    let loc =
        tl_enums::InputFileLocation::InputPhotoFileLocation(tl_types::InputPhotoFileLocation {
            id: 707,
            access_hash: 4,
            file_reference: vec![],
            thumb_size: "y".to_owned(),
        });

    let err = rt
        .block_on(download_file_bytes_async(&fake, 2, loc, None))
        .expect_err("oversized CDN chunk must be rejected");
    assert!(
        err.to_string().contains("exceeds part size"),
        "unexpected error: {err}"
    );
}

#[test]
fn live_inline_gif_video_sticker_static_sticker_audio_and_reopened_tgs_disk_cache() {
    let (dir, fake, live, peer_id) = start_live_session();
    let gif_bytes = b"MP4_GIF_ANIMATION_BYTES".to_vec();
    let webm_bytes = b"WEBM_VIDEO_STICKER_BYTES".to_vec();
    let webp_bytes = b"WEBP_STATIC_STICKER_BYTES".to_vec();
    let expected_json = minimal_valid_lottie_json();
    let tgs_bytes = gzip_stored_bytes(expected_json.as_bytes());

    {
        let gif_bytes = gif_bytes.clone();
        let webm_bytes = webm_bytes.clone();
        let webp_bytes = webp_bytes.clone();
        let tgs_bytes = tgs_bytes.clone();
        fake.set_dynamic_handler(Arc::new(move |_dc, ctor, bytes| {
            if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                let mut cur = tg_tl::Cursor::new(&bytes[4..]);
                let _flags = i32::deserialize(&mut cur).expect("flags");
                let loc = tl_enums::InputFileLocation::deserialize(&mut cur).expect("loc");
                let payload = match loc {
                    tl_enums::InputFileLocation::InputDocumentFileLocation(d) => match d.id {
                        30_001 => gif_bytes.clone(),
                        30_002 => webm_bytes.clone(),
                        30_003 => webp_bytes.clone(),
                        30_004 => tgs_bytes.clone(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                return Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                    r#type: tl_enums::storage::FileType::FileUnknown,
                    mtime: 0,
                    bytes: payload,
                })
                .to_bytes()));
            }
            None
        }));
    }

    // GIF
    let gif_doc = DocumentData {
        id: DocumentId(30_001),
        access_hash: 1,
        dc_id: 2,
        mime_type: "video/mp4".to_owned(),
        size: gif_bytes.len() as i64,
        kind: DocumentKind::Gif,
        width: Some(320),
        height: Some(240),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            301,
            MessageMedia::Document(Box::new(gif_doc)),
            MessageFlags::empty(),
        ),
    );

    // WebM VideoSticker
    let webm_doc = DocumentData {
        id: DocumentId(30_002),
        access_hash: 2,
        dc_id: 2,
        mime_type: "video/webm".to_owned(),
        size: webm_bytes.len() as i64,
        kind: DocumentKind::VideoSticker,
        width: Some(512),
        height: Some(512),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            302,
            MessageMedia::Document(Box::new(webm_doc)),
            MessageFlags::empty(),
        ),
    );

    // Static WebP Sticker
    let webp_doc = DocumentData {
        id: DocumentId(30_003),
        access_hash: 3,
        dc_id: 2,
        mime_type: "image/webp".to_owned(),
        size: webp_bytes.len() as i64,
        kind: DocumentKind::Sticker,
        width: Some(512),
        height: Some(512),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            303,
            MessageMedia::Document(Box::new(webp_doc)),
            MessageFlags::empty(),
        ),
    );

    // Animated .tgs sticker (to verify persistent disk reopen decompression)
    let tgs_doc = DocumentData {
        id: DocumentId(30_004),
        access_hash: 4,
        dc_id: 2,
        mime_type: "application/x-tgsticker".to_owned(),
        size: tgs_bytes.len() as i64,
        kind: DocumentKind::AnimatedSticker,
        width: Some(512),
        height: Some(512),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            304,
            MessageMedia::Document(Box::new(tgs_doc.clone())),
            MessageFlags::empty(),
        ),
    );

    // Audio track metadata
    let audio_doc = DocumentData {
        id: DocumentId(30_005),
        access_hash: 5,
        dc_id: 2,
        mime_type: "audio/mpeg".to_owned(),
        size: 1_500_000,
        kind: DocumentKind::Audio,
        duration: Some(187.4),
        audio_title: Some("Nocturne".to_owned()),
        audio_performer: Some("Synth Ensemble".to_owned()),
        ..DocumentData::default()
    };
    insert_message_into_live(
        &live,
        make_regular_msg(
            peer_id,
            305,
            MessageMedia::Document(Box::new(audio_doc)),
            MessageFlags::empty(),
        ),
    );

    for mid in [301, 302, 303, 304, 305] {
        let _ = live.inline_media_descriptor(peer_id, MsgId(mid));
    }

    assert!(wait_until(|| {
        let d_gif = live.inline_media_descriptor(peer_id, MsgId(301));
        let d_webm = live.inline_media_descriptor(peer_id, MsgId(302));
        let d_webp = live.inline_media_descriptor(peer_id, MsgId(303));
        let tgs_ready = live
            .media_cache()
            .cached_lottie_json_for_doc(&tgs_doc)
            .is_some();
        d_gif.and_then(|d| d.video_data).is_some()
            && d_webm.and_then(|d| d.video_data).is_some()
            && matches!(
                d_webp.and_then(|d| d.photo_load),
                Some(PhotoLoadState::Ready { .. })
            )
            && tgs_ready
    }));

    let d_gif = live
        .inline_media_descriptor(peer_id, MsgId(301))
        .expect("gif desc");
    assert!(matches!(d_gif.video_data, Some(InlineDocumentData::Bytes(b)) if b == gif_bytes));

    let d_webm = live
        .inline_media_descriptor(peer_id, MsgId(302))
        .expect("webm desc");
    assert!(d_webm.is_video_sticker);
    assert!(matches!(d_webm.video_data, Some(InlineDocumentData::Bytes(b)) if b == webm_bytes));

    let d_webp = live
        .inline_media_descriptor(peer_id, MsgId(303))
        .expect("webp desc");
    assert!(
        matches!(d_webp.photo_load, Some(PhotoLoadState::Ready { bytes }) if bytes == webp_bytes)
    );

    let d_audio = live
        .inline_media_descriptor(peer_id, MsgId(305))
        .expect("audio desc");
    assert_eq!(d_audio.music_title.as_deref(), Some("Nocturne"));
    assert_eq!(d_audio.music_performer.as_deref(), Some("Synth Ensemble"));
    assert_eq!(d_audio.music_duration_secs, Some(187));

    // Reopen MediaCache from disk with cold in-memory Lottie map and confirm .tgs reloads from disk.
    let cold_cache = LiveMediaCache::open(dir.path().join("media_cache")).expect("reopen cache");
    assert_eq!(
        cold_cache.cached_lottie_json_for_doc(&tgs_doc).as_deref(),
        Some(expected_json)
    );
}
