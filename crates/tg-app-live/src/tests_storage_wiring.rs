//! Fake-RPC integration tests for WP-59: wiring `tg-storage` (`CloudBlob`, `FolderArchive`,
//! `StreamedFileDownloader`) into live emoji sets, Hunspell dictionaries, and seekable streamed
//! media playback (`LiveSession`, `TelegramRangeLoader`, `TelegramMediaInvoker`,
//! `LiveAudioDataSource`).

use std::collections::VecDeque;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_audio::{AudioDataSource, AudioMsgId, MediaKind};
use tg_crypto::sha256_bytes;
use tg_data::{
    DocumentData, DocumentId, DocumentKind, FullMsgId, Message, MessageContent, MessageMedia,
    MsgId as DataMsgId, PeerId as DataPeerId, TextWithEntities as DataText, UserId,
};
use tg_media::transfer::{DOWNLOAD_PART_SIZE, FileOrigin, validate_get_file_request};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_storage::{
    ArchiveLimits, ArchiveWriteStatus, AsyncCloudBlobSource, CloudBlobLoader, CloudBlobLocation,
    PartialFileMetadata, UnpackOptions, extract_folder_archive,
};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_video::RangeLoader;
use tokio::sync::broadcast;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::cloud_blob_source::TelegramCloudBlobSource;
use crate::dice_pack::{
    EMOJI_CLOUD_LOCATION_USERNAME, EmojiSetState, EmojiSetsManager, archive_emoji_set_folder,
    clear_stale_emoji_set_archives,
};
use crate::media::{LiveAudioDataSource, TelegramRangeLoader, document_to_location};
use crate::session::LiveSession;
use crate::wp40_polish::RpcDictionaryCloudSource;
use tg_app_core::core_types::dto::dictionaries::{
    DICTIONARIES_MANIFEST_CHANNEL, DictionaryCloudSource, HunspellDictionary, LANG_ID_SERBIAN,
    build_dictionary_zip_bytes, is_dictionary_downloaded,
};
use tg_app_core::core_types::erased_rpc::ErasedRpcClient;
use tg_app_core::media::backend::{MediaBackend, MediaItemId};

type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
type DynamicHandler =
    Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

#[derive(Clone)]
struct FakeRpcClient {
    main_dc: DcId,
    responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    dynamic: Arc<Mutex<Option<DynamicHandler>>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
    delay_offset_zero: Arc<Mutex<Option<Duration>>>,
}

impl FakeRpcClient {
    fn new(main_dc: DcId) -> Self {
        let (updates_tx, _) = broadcast::channel(64);
        Self {
            main_dc,
            responses: Arc::new(Mutex::new(VecDeque::new())),
            dynamic: Arc::new(Mutex::new(None)),
            updates_tx,
            delay_offset_zero: Arc::new(Mutex::new(None)),
        }
    }

    fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
        self.responses
            .lock()
            .expect("lock responses")
            .push_back((expected_ctor, Ok(resp.to_bytes())));
    }

    fn set_dynamic_handler(&self, handler: DynamicHandler) {
        *self.dynamic.lock().expect("lock dynamic") = Some(handler);
    }

    fn set_delay_offset_zero(&self, delay: Duration) {
        *self.delay_offset_zero.lock().expect("lock delay") = Some(delay);
    }
}

impl RpcClient for FakeRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(self.main_dc, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        let delay = *self.delay_offset_zero.lock().expect("lock delay");
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID
            && let Some(delay) = delay
            && let Ok(parsed) = tl_fns::upload::GetFile::from_bytes(&bytes)
            && parsed.offset == 0
        {
            let handler = self.dynamic.lock().expect("lock dynamic").clone();
            if let Some(handler) = handler {
                // Signal start before async sleep so cancellation wakes cleanly on abort.
                let _ = handler(ctor, &bytes);
            }
            tokio::time::sleep(delay).await;
        }

        if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
            && let Some(res) = handler(ctor, &bytes)
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
        let raw = match next {
            Some(Ok(b)) => b,
            Some(Err(e)) => return Err(e),
            None => {
                return Err(InvocationError::Rpc(tg_api::rpc::RpcError {
                    code: 500,
                    name: format!("UNMOCKED_CTOR_{ctor:#010x}"),
                    value: None,
                }));
            }
        };
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        self.main_dc
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for &(name, data) in entries {
            writer.start_file(name, opts).expect("start zip entry");
            writer.write_all(data).expect("write zip entry");
        }
        writer.finish().expect("finish zip");
    }
    cursor.into_inner()
}

fn build_symlink_zip(link_name: &str, target: &str) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer
            .start_file("config.json", opts)
            .expect("start config.json");
        writer.write_all(br#"{"v":1}"#).expect("write config.json");
        writer
            .add_symlink(link_name, target, opts)
            .expect("add symlink");
        writer.finish().expect("finish symlink zip");
    }
    cursor.into_inner()
}

fn make_channel(id: i64, access_hash: i64, username: &str) -> tl_enums::Chat {
    tl_enums::Chat::Channel(tl_types::Channel {
        creator: false,
        left: false,
        broadcast: true,
        verified: true,
        megagroup: false,
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
        in_news: false,
        forum_sidebar: false,
        id,
        access_hash: Some(access_hash),
        title: format!("Channel {username}"),
        username: Some(username.to_owned()),
        photo: tl_enums::ChatPhoto::Empty,
        date: 1_740_000_000,
        restriction_reason: None,
        admin_rights: None,
        banned_rights: None,
        default_banned_rights: None,
        participants_count: None,
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
    })
}

fn make_doc_message(channel_id: i64, post_id: i32, doc_id: i64, size: i64) -> tl_enums::Message {
    tl_enums::Message::Message(tl_types::Message {
        out: false,
        mentioned: false,
        media_unread: false,
        silent: false,
        post: true,
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
        id: post_id,
        from_id: None,
        from_boosts_applied: None,
        from_rank: None,
        peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id }),
        saved_peer_id: None,
        fwd_from: None,
        via_bot_id: None,
        via_business_bot_id: None,
        reply_to: None,
        date: 1_740_000_000,
        message: String::new(),
        media: Some(tl_enums::MessageMedia::Document(
            tl_types::MessageMediaDocument {
                nopremium: false,
                spoiler: false,
                video: false,
                round: false,
                voice: false,
                document: Some(tl_enums::Document::Document(tl_types::Document {
                    id: doc_id,
                    access_hash: doc_id * 17,
                    file_reference: vec![9, 8, 7],
                    date: 1_740_000_000,
                    mime_type: "application/zip".to_owned(),
                    size,
                    thumbs: None,
                    video_thumbs: None,
                    dc_id: 2,
                    attributes: vec![tl_enums::DocumentAttribute::Filename(
                        tl_types::DocumentAttributeFilename {
                            file_name: format!("blob_{post_id}.zip"),
                        },
                    )],
                })),
                alt_documents: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: None,
            },
        )),
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

fn enqueue_cloud_blob_download(
    fake: &FakeRpcClient,
    channel_id: i64,
    username: &str,
    post_id: i32,
    doc_id: i64,
    zip_bytes: &[u8],
    include_resolve: bool,
) {
    if include_resolve {
        let resolved =
            tl_enums::contacts::ResolvedPeer::ResolvedPeer(tl_types::contacts::ResolvedPeer {
                peer: tl_enums::Peer::Channel(tl_types::PeerChannel { channel_id }),
                chats: vec![make_channel(channel_id, channel_id * 3, username)],
                users: Vec::new(),
            });
        fake.enqueue_ok(tl_fns::contacts::ResolveUsername::CONSTRUCTOR_ID, &resolved);
    }

    let msgs = tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
        inexact: false,
        pts: 10,
        count: 1,
        offset_id_offset: None,
        messages: vec![make_doc_message(
            channel_id,
            post_id,
            doc_id,
            zip_bytes.len() as i64,
        )],
        topics: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    });
    fake.enqueue_ok(tl_fns::channels::GetMessages::CONSTRUCTOR_ID, &msgs);

    let file_resp = tl_enums::upload::File::File(tl_types::upload::File {
        r#type: tl_enums::storage::FileType::FileUnknown,
        mtime: 1_740_000_000,
        bytes: zip_bytes.to_vec(),
    });
    fake.enqueue_ok(tl_fns::upload::GetFile::CONSTRUCTOR_ID, &file_resp);
}

struct LiveStorageTestHarness {
    _rt: tokio::runtime::Runtime,
    fake: FakeRpcClient,
    runtime: Arc<SessionRuntime<ErasedRpcClient>>,
    live: LiveSession,
    tmp: TempDir,
}

impl LiveStorageTestHarness {
    fn new() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let fake = FakeRpcClient::new(2);
        let erased = ErasedRpcClient::new(fake.clone());
        let runtime = Arc::new(
            SessionRuntime::start_on(
                UserId(42_000),
                erased,
                rt.handle().clone(),
                SessionRuntimeConfig::default(),
            )
            .expect("start SessionRuntime"),
        );
        let tmp = TempDir::new().expect("tempdir");
        let live = LiveSession::new(Arc::clone(&runtime), 59_001);
        live.media_cache()
            .reopen_at(tmp.path().join("media_cache"))
            .expect("reopen media cache in tempdir");
        Self {
            _rt: rt,
            fake,
            runtime,
            live,
            tmp,
        }
    }
}

// =============================================================================
// 1. Emoji sets: CloudBlob SHA-256 verification, atomic swap, rollback, FolderArchive
// =============================================================================

#[test]
fn wp59_emoji_sets_cloud_blob_atomic_swap_sha256_and_zip_slip_rollback() {
    let h = LiveStorageTestHarness::new();
    let sets_root = h.tmp.path().join("emoji_sets");
    let mut mgr = EmojiSetsManager::new(sets_root.clone());

    // Pre-populate prior active set_2 directory so we can verify atomic replacement & rollback.
    let set2_dir = mgr.set_data_path(2);
    fs::create_dir_all(&set2_dir).expect("create prior set_2");
    fs::write(set2_dir.join("config.json"), br#"{"version":0,"old":true}"#).expect("write old");
    fs::write(set2_dir.join("legacy_only.webp"), b"LEGACY").expect("write legacy");

    // 1a. Valid archive with matching SHA-256 -> atomic directory swap replaces old files cleanly.
    let valid_zip = build_zip(&[
        ("config.json", br#"{"version":2,"set":"twemoji"}"#),
        ("emoji_0.webp", b"RIFF_TWEMOJI_V2"),
    ]);
    let valid_sha = sha256_bytes(&valid_zip);
    enqueue_cloud_blob_download(
        &h.fake,
        500_002,
        EMOJI_CLOUD_LOCATION_USERNAME,
        3224,
        700_001,
        &valid_zip,
        true,
    );

    let report = mgr
        .load_and_switch_to_verified(&h.live, 2, Some(valid_sha), true)
        .expect("valid SHA-256 emoji set download should succeed");
    assert_eq!(report.sha256, valid_sha);
    assert_eq!(report.extracted_files.len(), 2);
    assert_eq!(mgr.current_set_id(), 2);
    assert_eq!(mgr.state_for(2), EmojiSetState::Active);
    assert_eq!(
        fs::read(set2_dir.join("config.json")).expect("read new config"),
        br#"{"version":2,"set":"twemoji"}"#
    );
    assert_eq!(
        fs::read(set2_dir.join("emoji_0.webp")).expect("read new sprite"),
        b"RIFF_TWEMOJI_V2"
    );
    assert!(
        !set2_dir.join("legacy_only.webp").exists(),
        "atomic directory swap must replace old files"
    );

    // 1b. Corrupted SHA-256 -> fails closed BEFORE unpacking and preserves prior set_2 untouched.
    let tampered_zip = build_zip(&[
        ("config.json", br#"{"version":999,"tampered":true}"#),
        ("emoji_0.webp", b"TAMPERED"),
    ]);
    let mut wrong_sha = sha256_bytes(&tampered_zip);
    wrong_sha[0] ^= 0xFF;
    enqueue_cloud_blob_download(
        &h.fake,
        500_002,
        EMOJI_CLOUD_LOCATION_USERNAME,
        3224,
        700_002,
        &tampered_zip,
        true,
    );

    let err = mgr
        .load_and_switch_to_verified(&h.live, 2, Some(wrong_sha), true)
        .expect_err("corrupted SHA-256 must fail closed");
    assert!(
        err.contains("SHA-256 mismatch"),
        "expected SHA-256 error, got: {err}"
    );
    assert!(matches!(mgr.state_for(2), EmojiSetState::Failed(_)));
    assert_eq!(
        fs::read(set2_dir.join("config.json")).expect("prior config preserved"),
        br#"{"version":2,"set":"twemoji"}"#
    );
    assert_eq!(
        fs::read(set2_dir.join("emoji_0.webp")).expect("prior sprite preserved"),
        b"RIFF_TWEMOJI_V2"
    );

    // 1c. Zip-slip entry (`../evil.txt`) alongside valid `config.json` -> fails closed, no escape,
    //     and preserves prior set_2 directory intact.
    let slip_zip = build_zip(&[
        ("config.json", br#"{"version":3,"evil":true}"#),
        ("../evil.txt", b"PWNED"),
    ]);
    let slip_sha = sha256_bytes(&slip_zip);
    enqueue_cloud_blob_download(
        &h.fake,
        500_002,
        EMOJI_CLOUD_LOCATION_USERNAME,
        3224,
        700_003,
        &slip_zip,
        true,
    );

    let err = mgr
        .load_and_switch_to_verified(&h.live, 2, Some(slip_sha), true)
        .expect_err("zip-slip archive must fail closed");
    assert!(
        err.contains("unsafe archive entry path"),
        "expected unsafe path error, got: {err}"
    );
    assert!(
        !sets_root.join("evil.txt").exists(),
        "zip-slip must not escape"
    );
    assert_eq!(
        fs::read(set2_dir.join("config.json")).expect("prior config intact after zip-slip"),
        br#"{"version":2,"set":"twemoji"}"#
    );

    // 1d. Symlink entry -> fails closed, prior set_2 intact.
    let symlink_zip = build_symlink_zip("emoji_link.webp", "/etc/passwd");
    enqueue_cloud_blob_download(
        &h.fake,
        500_002,
        EMOJI_CLOUD_LOCATION_USERNAME,
        3224,
        700_004,
        &symlink_zip,
        true,
    );
    let err = mgr
        .load_and_switch_to_verified(&h.live, 2, None, true)
        .expect_err("symlink entry must fail closed");
    assert!(
        err.contains("symlink"),
        "expected symlink rejection, got: {err}"
    );
    assert_eq!(
        fs::read(set2_dir.join("config.json")).expect("prior config intact after symlink"),
        br#"{"version":2,"set":"twemoji"}"#
    );

    // 1e. FolderArchive round-trip + stale archive cleanup helper.
    let backup_zip = h.tmp.path().join("set2_backup.zip");
    let write_res = archive_emoji_set_folder(&set2_dir, &backup_zip).expect("archive set_2");
    assert_eq!(write_res.status, ArchiveWriteStatus::Done);
    assert!(backup_zip.is_file());
    let restore_dir = h.tmp.path().join("set2_restored");
    let extract_res =
        extract_folder_archive(&backup_zip, &restore_dir, &ArchiveLimits::default(), |_| {
            true
        })
        .expect("extract backup");
    assert_eq!(extract_res.files_extracted, 2);
    assert_eq!(
        fs::read(restore_dir.join("set_2/config.json")).expect("restored config"),
        br#"{"version":2,"set":"twemoji"}"#
    );
    // Fresh files are not deleted by stale cleanup.
    assert_eq!(clear_stale_emoji_set_archives(mgr.storage_root()), 0);
}

// =============================================================================
// 2. Hunspell Dictionaries: RpcDictionaryCloudSource + CloudBlob atomic swap & rollback
// =============================================================================

#[test]
fn wp59_dictionaries_cloud_blob_atomic_swap_sha256_zip_slip_and_serbian_rename() {
    let h = LiveStorageTestHarness::new();
    let dicts_root = h.tmp.path().join("dictionaries");
    let en_dir = dicts_root.join("en_US");
    fs::create_dir_all(&en_dir).expect("create prior en_US");
    fs::write(en_dir.join("en_US.dic"), b"1\noldword\n").expect("write old dic");
    fs::write(en_dir.join("en_US.aff"), b"SET UTF-8\n").expect("write old aff");
    fs::write(en_dir.join("obsolete.dic"), b"1\nobsolete\n").expect("write obsolete");

    let mut source =
        RpcDictionaryCloudSource::new(h.fake.clone(), h.runtime.tokio_handle().clone());

    // 2a. Valid dictionary archive with expected SHA-256 -> atomic swap replaces prior dir cleanly.
    let valid_dict_zip = build_dictionary_zip_bytes(
        "en_US",
        "3\nhello/S\nworld\ntelegram\n",
        "SET UTF-8\nSFX S Y 1\nSFX S 0 s .\n",
    );
    let valid_sha = sha256_bytes(&valid_dict_zip);
    enqueue_cloud_blob_download(
        &h.fake,
        777_001,
        DICTIONARIES_MANIFEST_CHANNEL,
        2950,
        800_001,
        &valid_dict_zip,
        true,
    );

    let (installed_dir, report) = source
        .download_and_unpack_dictionary_verified(
            DICTIONARIES_MANIFEST_CHANNEL,
            2950,
            valid_dict_zip.len() as u64,
            &dicts_root,
            31,
            Some("en_US"),
            Some(valid_sha),
        )
        .expect("valid dictionary cloud blob download");
    assert_eq!(installed_dir, en_dir);
    assert_eq!(report.sha256, valid_sha);
    assert!(is_dictionary_downloaded(&dicts_root, 31, Some("en_US")));
    assert!(
        !en_dir.join("obsolete.dic").exists(),
        "atomic directory swap must remove obsolete files"
    );
    let dict = HunspellDictionary::load_from_dir(&en_dir, "en_US").expect("load en_US");
    assert!(dict.check_word("hello"));
    assert!(dict.check_word("hellos"));
    assert!(!dict.check_word("oldword"));

    // 2b. Corrupted SHA-256 -> fails closed, prior en_US dictionary completely preserved.
    let corrupt_zip = build_dictionary_zip_bytes("en_US", "1\ncorrupted\n", "SET UTF-8\n");
    let mut bad_sha = sha256_bytes(&corrupt_zip);
    bad_sha[31] ^= 0x01;
    // Note: channel `@tdhbcfiles` is already cached in `source.blob_source`.
    enqueue_cloud_blob_download(
        &h.fake,
        777_001,
        DICTIONARIES_MANIFEST_CHANNEL,
        2951,
        800_002,
        &corrupt_zip,
        false,
    );

    let err = source
        .download_and_unpack_dictionary_verified(
            DICTIONARIES_MANIFEST_CHANNEL,
            2951,
            corrupt_zip.len() as u64,
            &dicts_root,
            31,
            Some("en_US"),
            Some(bad_sha),
        )
        .expect_err("corrupted dictionary SHA-256 must fail closed");
    assert!(
        err.contains("SHA-256 mismatch"),
        "expected SHA-256 error, got: {err}"
    );
    let preserved = HunspellDictionary::load_from_dir(&en_dir, "en_US").expect("still intact");
    assert!(preserved.check_word("hello"));
    assert!(!preserved.check_word("corrupted"));

    // 2c. Zip-slip entry (`../evil.dic`) -> fails closed, no escape, prior en_US intact.
    let slip_dict_zip = build_zip(&[
        ("en_US.dic", b"1\npoison\n"),
        ("en_US.aff", b"SET UTF-8\n"),
        ("../evil.dic", b"1\nescaped\n"),
    ]);
    enqueue_cloud_blob_download(
        &h.fake,
        777_001,
        DICTIONARIES_MANIFEST_CHANNEL,
        2952,
        800_003,
        &slip_dict_zip,
        false,
    );
    let err = source
        .download_and_unpack_dictionary(
            DICTIONARIES_MANIFEST_CHANNEL,
            2952,
            slip_dict_zip.len() as u64,
            &dicts_root,
            31,
            Some("en_US"),
            None,
        )
        .expect_err("zip-slip dictionary must fail closed");
    assert!(
        err.contains("unsafe archive entry path"),
        "expected unsafe path error, got: {err}"
    );
    assert!(!dicts_root.join("evil.dic").exists());
    let preserved_after_slip =
        HunspellDictionary::load_from_dir(&en_dir, "en_US").expect("still intact");
    assert!(preserved_after_slip.check_word("hello"));
    assert!(!preserved_after_slip.check_word("poison"));

    // 2d. Serbian rename quirk (`sr_Cyrl_RS.{dic,aff}` -> `sr_RS.{dic,aff}`) through CloudBlobLoader.
    let sr_zip = build_dictionary_zip_bytes("sr_Cyrl_RS", "1\nздраво\n", "SET UTF-8\n");
    let sr_sha = sha256_bytes(&sr_zip);
    enqueue_cloud_blob_download(
        &h.fake,
        777_001,
        DICTIONARIES_MANIFEST_CHANNEL,
        2953,
        800_004,
        &sr_zip,
        false,
    );
    let sr_dir = source
        .download_and_unpack_dictionary(
            DICTIONARIES_MANIFEST_CHANNEL,
            2953,
            sr_zip.len() as u64,
            &dicts_root,
            LANG_ID_SERBIAN,
            Some("sr_RS"),
            Some(sr_sha),
        )
        .expect("serbian dictionary cloud blob download");
    assert!(sr_dir.join("sr_RS.dic").is_file());
    assert!(sr_dir.join("sr_RS.aff").is_file());
    let sr_dict = HunspellDictionary::load_from_dir(&sr_dir, "sr_RS").expect("load sr_RS");
    assert!(sr_dict.check_word("здраво"));

    // 2e. AsyncCloudBlobSource path via CloudBlobLoader::download_and_unpack_async.
    let async_target = h.tmp.path().join("async_blob");
    let async_zip = build_zip(&[
        ("en_US.dic", b"1\nasyncword\n"),
        ("en_US.aff", b"SET UTF-8\n"),
    ]);
    let async_sha = sha256_bytes(&async_zip);
    enqueue_cloud_blob_download(
        &h.fake,
        777_001,
        DICTIONARIES_MANIFEST_CHANNEL,
        2954,
        800_005,
        &async_zip,
        true,
    );
    let async_src = Arc::new(TelegramCloudBlobSource::new(
        h.fake.clone(),
        h.runtime.tokio_handle().clone(),
    ));
    let async_loader = CloudBlobLoader::new(
        31,
        CloudBlobLocation::new(DICTIONARIES_MANIFEST_CHANNEL, 2954),
        &async_target,
        async_zip.len() as u64,
        UnpackOptions {
            expected_sha256: Some(async_sha),
            ..UnpackOptions::default()
        },
    );
    // Smoke-check trait object compatibility on `AsyncCloudBlobSource`.
    let _: Arc<dyn AsyncCloudBlobSource> = async_src.clone();
    let async_report = h
        .runtime
        .tokio_handle()
        .block_on(async_loader.download_and_unpack_async(
            async_src,
            DOWNLOAD_PART_SIZE as usize,
            |_| true,
        ))
        .expect("async cloud blob unpack");
    assert_eq!(async_report.sha256, async_sha);
    assert!(async_target.join("en_US.dic").is_file());
}

// =============================================================================
// 3. Streamed Media Playback: out-of-order seeks, stale chunk cancellation,
//    resumable .part/.part.meta persistence across reopen, FILE_REFERENCE refresh
// =============================================================================

fn seed_video_message(
    h: &LiveStorageTestHarness,
    peer_id: DataPeerId,
    msg_id: i32,
    doc_id: u64,
    file_ref: Vec<u8>,
    size: usize,
) -> MediaItemId {
    let doc = DocumentData {
        id: DocumentId(doc_id),
        access_hash: doc_id.wrapping_mul(31),
        file_reference: file_ref,
        dc_id: 2,
        size: size as i64,
        mime_type: "video/mp4".to_owned(),
        file_name: format!("stream_{doc_id}.mp4"),
        kind: DocumentKind::Video,
        ..DocumentData::default()
    };
    let mut msg = Message::new_text(
        peer_id,
        DataMsgId(i64::from(msg_id)),
        Some(peer_id),
        1_740_000_100,
        DataText::plain("streamed video"),
    );
    msg.content = MessageContent::Regular {
        text: DataText::plain("streamed video"),
        media: MessageMedia::Document(Box::new(doc)),
    };
    h.runtime.tokio_handle().block_on(async {
        let mut s = h.runtime.session().write().await;
        s.insert_message(msg);
    });
    let siblings = h.live.siblings_in_chat(peer_id.0, MediaItemId(0));
    siblings
        .into_iter()
        .find(|item| item.msg_id == msg_id)
        .expect("seeded media item")
        .id
}

#[test]
fn wp59_live_streamed_media_out_of_order_seeks_and_resumable_partial_metadata() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize; // 128 KiB
    // 3 full parts + 40 KiB tail = 424 KiB.
    let total_size = part_size * 3 + 40 * 1024;
    let payload: Vec<u8> = (0..total_size)
        .map(|i| ((i.wrapping_mul(37) ^ (i >> 8) ^ 0x5A) & 0xFF) as u8)
        .collect();
    let payload_rpc = Arc::new(payload.clone());
    let recorded_requests: Arc<Mutex<Vec<(u64, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_rpc = Arc::clone(&recorded_requests);

    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode upload.getFile");
        let offset = u64::try_from(req.offset).expect("non-negative offset");
        let limit = u32::try_from(req.limit).expect("non-negative limit");
        // Every upload.getFile request must satisfy the file-transfer conformance alignment rules.
        validate_get_file_request(offset, limit, req.precise)
            .expect("upload.getFile request must be spec-aligned");
        recorded_rpc.lock().unwrap().push((offset, limit));

        let start = offset as usize;
        let end = start.saturating_add(limit as usize).min(payload_rpc.len());
        let bytes = if start < payload_rpc.len() {
            payload_rpc[start..end].to_vec()
        } else {
            Vec::new()
        };
        let resp = tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileMp4,
            mtime: 1_740_000_100,
            bytes,
        });
        Some(Ok(resp.to_bytes()))
    }));

    let peer = DataPeerId::from_user(UserId(77_001));
    let item_id = seed_video_message(&h, peer, 101, 901_001, vec![1, 2, 3], total_size);

    // 1. Open LiveSession's RangeLoader and perform out-of-order range seeks (tail first, then middle).
    let loader1 = h
        .live
        .open_range_loader(item_id)
        .expect("open_range_loader #1");
    assert_eq!(loader1.size(), total_size as u64);

    // Seek to trailing partial part (part 3, spanning unaligned offset inside tail).
    let tail_offset = (part_size * 3 + 5_000) as u64;
    let tail_len = 12_345usize;
    let tail_slice = loader1
        .load_range(tail_offset, tail_len)
        .expect("seek tail range");
    assert_eq!(
        tail_slice.as_slice(),
        &payload[tail_offset as usize..tail_offset as usize + tail_len]
    );

    // Seek backwards across part 1 -> part 2 boundary (unaligned cross-boundary read).
    let cross_offset = (part_size * 2 - 4_096) as u64;
    let cross_len = 10_000usize;
    let cross_slice = loader1
        .load_range(cross_offset, cross_len)
        .expect("seek cross-boundary range");
    assert_eq!(
        cross_slice.as_slice(),
        &payload[cross_offset as usize..cross_offset as usize + cross_len]
    );

    let reqs_first_session: Vec<(u64, u32)> = recorded_requests.lock().unwrap().clone();
    assert_eq!(reqs_first_session.len(), 3, "should fetch parts 3, 1, 2");
    assert!(
        reqs_first_session
            .iter()
            .all(|&(_, lim)| lim == DOWNLOAD_PART_SIZE),
        "all upload.getFile limits must be 128 KiB"
    );

    // Stop & drop first loader: `.part` and `.part.meta` must remain persisted in tempdir.
    loader1.stop();
    drop(loader1);

    // The file name is a keyed hash of the cache key and the sidecar is encrypted (SEC3-B3).
    let streamed_dir = h.live.media_cache().root_dir().join("streamed");
    let cache_key = tg_storage::cache::media_cache::lookup_cache_key_for_path(&streamed_dir)
        .expect("cache key registered for the streamed dir");
    let stem = tg_storage::cache::media_cache::keyed_stream_file_stem(&cache_key, 901_001u64, 2u64);
    let target_base = streamed_dir.join(format!("{stem}.bin"));
    let part_file = PathBuf::from(format!("{}.part", target_base.display()));
    let meta_file = PathBuf::from(format!("{}.part.meta", target_base.display()));
    assert!(part_file.is_file(), "sparse .part file must persist");
    assert!(meta_file.is_file(), "resumable .part.meta must persist");

    let persisted_meta = PartialFileMetadata::load_encrypted(&meta_file, &cache_key)
        .expect("load persisted metadata");
    assert_eq!(persisted_meta.total_size, total_size as u64);
    assert!(
        !persisted_meta.is_part_saved(0),
        "part 0 was never requested"
    );
    assert!(persisted_meta.is_part_saved(1), "part 1 must be saved");
    assert!(persisted_meta.is_part_saved(2), "part 2 must be saved");
    assert!(persisted_meta.is_part_saved(3), "part 3 must be saved");

    // 2. Reopen a brand-new RangeLoader via LiveSession against the same tempdir cache:
    //    reading previously fetched parts 1, 2, 3 must issue ZERO new upload.getFile RPCs!
    recorded_requests.lock().unwrap().clear();
    let loader2 = h
        .live
        .open_range_loader(item_id)
        .expect("open_range_loader #2 (resumed)");

    let resumed_tail = loader2
        .load_range(tail_offset, tail_len)
        .expect("resumed tail read");
    assert_eq!(resumed_tail, tail_slice);
    let resumed_cross = loader2
        .load_range(cross_offset, cross_len)
        .expect("resumed cross read");
    assert_eq!(resumed_cross, cross_slice);
    assert!(
        recorded_requests.lock().unwrap().is_empty(),
        "resumed reads from persisted .part must issue zero RPCs"
    );

    // Reading uncached part 0 fetches ONLY part 0.
    let head_slice = loader2.load_range(0, 8_192).expect("read head");
    assert_eq!(head_slice.as_slice(), &payload[..8_192]);
    assert_eq!(
        *recorded_requests.lock().unwrap(),
        vec![(0, DOWNLOAD_PART_SIZE)]
    );
    loader2.stop();
}

#[test]
fn wp59_streamed_media_cancels_stale_prefetch_chunks_on_seek() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let fake = FakeRpcClient::new(2);
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let total_size = part_size * 4;
    let payload: Vec<u8> = (0..total_size)
        .map(|i| ((i.wrapping_mul(19) + 11) & 0xFF) as u8)
        .collect();
    let payload_rpc = Arc::new(payload.clone());

    let part0_started = Arc::new(AtomicUsize::new(0));
    let part0_started_rpc = Arc::clone(&part0_started);

    // Fake RPC handler that delays part 0 asynchronously (simulating slow sequential prefetch)
    // while serving part 3 immediately so a seek to part 3 preempts and cancels the stale part 0.
    fake.set_delay_offset_zero(Duration::from_secs(5));
    fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode upload.getFile");
        let offset = u64::try_from(req.offset).expect("offset");
        let limit = u32::try_from(req.limit).expect("limit");
        validate_get_file_request(offset, limit, req.precise).expect("aligned request");

        if offset == 0 {
            part0_started_rpc.fetch_add(1, Ordering::SeqCst);
        }

        let start = offset as usize;
        let end = start.saturating_add(limit as usize).min(payload_rpc.len());
        let resp = tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileMp4,
            mtime: 1_740_000_200,
            bytes: payload_rpc[start..end].to_vec(),
        });
        Some(Ok(resp.to_bytes()))
    }));

    let tmp = TempDir::new().expect("tempdir");
    let doc = DocumentData {
        id: DocumentId(902_001),
        access_hash: 55_666,
        file_reference: vec![5, 6, 7],
        dc_id: 2,
        size: total_size as i64,
        mime_type: "video/mp4".to_owned(),
        file_name: "seek_cancel.mp4".to_owned(),
        kind: DocumentKind::Video,
        ..DocumentData::default()
    };
    let loader = TelegramRangeLoader::new(
        fake.clone(),
        rt.handle().clone(),
        document_to_location(&doc),
    )
    .with_storage_dir(tmp.path().join("streamed"));

    // Start low-priority background prefetch of part 0.
    loader.prefetch(0, part_size);
    let deadline = Instant::now() + Duration::from_secs(2);
    while part0_started.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        part0_started.load(Ordering::SeqCst) >= 1,
        "prefetched part 0 should be in flight"
    );

    // Seek directly to part 3 -> StreamedFileDownloader bumps priority epoch and cancels stale
    // background in-flight part 0, calling ChunkProvider::cancel_chunk(0).
    let seek_offset = (part_size * 3 + 1_024) as u64;
    let seek_len = 4_096usize;
    let slice = loader
        .load_range(seek_offset, seek_len)
        .expect("seek to part 3");
    assert_eq!(
        slice.as_slice(),
        &payload[seek_offset as usize..seek_offset as usize + seek_len]
    );

    let cancelled = loader.cancelled_chunks();
    assert!(
        cancelled.contains(&0),
        "stale prefetched chunk at offset 0 must be cancelled on seek; got {cancelled:?}"
    );
    loader.stop();
}

#[test]
fn wp59_streamed_media_refreshes_expired_file_reference_via_media_invoker() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let total_size = part_size + 16_384;
    let payload: Vec<u8> = (0..total_size)
        .map(|i| ((i * 13 + 3) & 0xFF) as u8)
        .collect();
    let payload_rpc = Arc::new(payload.clone());

    let peer = DataPeerId::from_user(UserId(77_002));
    let doc_id = 903_001u64;
    let old_ref = vec![0xAA, 0x01];
    let fresh_ref = vec![0xBB, 0x02];
    let item_id = seed_video_message(&h, peer, 202, doc_id, old_ref.clone(), total_size);

    let expected_fresh = fresh_ref.clone();
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let refresh_count_rpc = Arc::clone(&refresh_count);

    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode getFile");
            let tl_enums::InputFileLocation::InputDocumentFileLocation(ref loc) = req.location
            else {
                return None;
            };
            if loc.file_reference != expected_fresh {
                return Some(Err(tg_api::rpc::InvocationError::Rpc(
                    tg_api::rpc::RpcError {
                        code: 400,
                        name: "FILE_REFERENCE_EXPIRED".to_owned(),
                        value: None,
                    },
                )));
            }
            let start = req.offset as usize;
            let end = start
                .saturating_add(req.limit as usize)
                .min(payload_rpc.len());
            let resp = tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FileMp4,
                mtime: 1_740_000_300,
                bytes: payload_rpc[start..end].to_vec(),
            });
            return Some(Ok(resp.to_bytes()));
        }
        if ctor == tl_fns::messages::GetMessages::CONSTRUCTOR_ID {
            refresh_count_rpc.fetch_add(1, Ordering::SeqCst);
            let mut msg = make_doc_message(0, 202, doc_id as i64, total_size as i64);
            if let tl_enums::Message::Message(ref mut inner) = msg {
                inner.peer_id = tl_enums::Peer::User(tl_types::PeerUser { user_id: 77_002 });
                if let Some(tl_enums::MessageMedia::Document(ref mut md)) = inner.media
                    && let Some(tl_enums::Document::Document(ref mut d)) = md.document
                {
                    d.file_reference = expected_fresh.clone();
                }
            }
            let resp = tl_enums::messages::Messages::Messages(tl_types::messages::Messages {
                messages: vec![msg],
                topics: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            });
            return Some(Ok(resp.to_bytes()));
        }
        None
    }));

    let loader = h
        .live
        .open_range_loader(item_id)
        .expect("open_range_loader with refresh");
    let slice = loader
        .load_range(part_size as u64 - 512, 2_048)
        .expect("range read succeeds after FILE_REFERENCE_EXPIRED refresh");
    assert_eq!(
        slice.as_slice(),
        &payload[part_size - 512..part_size - 512 + 2_048]
    );
    assert_eq!(
        refresh_count.load(Ordering::SeqCst),
        1,
        "should refresh expired file_reference once and reuse it for subsequent chunks"
    );
    loader.stop();
}

#[test]
fn wp59_live_audio_data_source_streamed_range_and_resumable_open() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let total_size = part_size + 12_000;
    let payload: Vec<u8> = (0..total_size)
        .map(|i| ((i.wrapping_mul(41) + 5) & 0xFF) as u8)
        .collect();
    let payload_rpc = Arc::new(payload.clone());
    let rpc_calls = Arc::new(AtomicUsize::new(0));
    let rpc_calls_clone = Arc::clone(&rpc_calls);

    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return None;
        }
        rpc_calls_clone.fetch_add(1, Ordering::SeqCst);
        let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode getFile");
        let start = req.offset as usize;
        let end = start
            .saturating_add(req.limit as usize)
            .min(payload_rpc.len());
        let resp = tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileMp3,
            mtime: 1_740_000_400,
            bytes: payload_rpc[start..end].to_vec(),
        });
        Some(Ok(resp.to_bytes()))
    }));

    let peer = DataPeerId::from_user(UserId(77_003));
    let doc_id = DocumentId(904_001);
    let doc = DocumentData {
        id: doc_id,
        access_hash: 444_555,
        file_reference: vec![7, 7, 7],
        dc_id: 2,
        size: total_size as i64,
        mime_type: "audio/mpeg".to_owned(),
        file_name: "track.mp3".to_owned(),
        kind: DocumentKind::Audio,
        ..DocumentData::default()
    };
    let mut msg = Message::new_text(
        peer,
        DataMsgId(303),
        Some(peer),
        1_740_000_400,
        DataText::plain("audio track"),
    );
    msg.content = MessageContent::Regular {
        text: DataText::plain("audio track"),
        media: MessageMedia::Document(Box::new(doc)),
    };
    h.runtime.tokio_handle().block_on(async {
        h.runtime.session().write().await.insert_message(msg);
    });

    let audio_cache = h.tmp.path().join("audio_streamed");
    let source = LiveAudioDataSource::new(
        h.fake.clone(),
        h.runtime.tokio_handle().clone(),
        Arc::clone(h.runtime.session()),
    )
    .with_storage_dir(&audio_cache);

    let track_id = AudioMsgId::new(
        doc_id,
        FullMsgId::new(peer, DataMsgId(303)),
        MediaKind::AudioFile,
        60_000,
        0,
    );

    // 1. Seekable range read for the tail part persists part 1 on disk.
    let range_loader = source
        .open_range_loader(&track_id)
        .expect("open audio range loader");
    let tail = range_loader
        .load_range(part_size as u64, 4_096)
        .expect("audio tail seek");
    assert_eq!(tail.as_slice(), &payload[part_size..part_size + 4_096]);
    assert_eq!(rpc_calls.load(Ordering::SeqCst), 1);
    drop(range_loader);

    // 2. Full AudioDataSource::open resumes from `.part` (fetching ONLY missing part 0 -> 1 RPC).
    let mut input = source.open(&track_id).expect("open audio input");
    let mut read_bytes = Vec::new();
    input
        .reader
        .read_to_end(&mut read_bytes)
        .expect("read audio input");
    assert_eq!(read_bytes, payload);
    assert_eq!(input.hint_ext.as_deref(), Some("mp3"));
    assert_eq!(
        rpc_calls.load(Ordering::SeqCst),
        2,
        "full open should resume persisted part 1 and only fetch part 0"
    );

    // 3. Re-opening finalized audio file uses zero additional RPCs.
    let mut input2 = source.open(&track_id).expect("open finalized audio input");
    let mut read_bytes2 = Vec::new();
    input2
        .reader
        .read_to_end(&mut read_bytes2)
        .expect("read finalized audio input");
    assert_eq!(read_bytes2, payload);
    assert_eq!(rpc_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn wp59_direct_range_loader_with_origin_and_sha256_finalization() {
    let h = LiveStorageTestHarness::new();
    let part_size = DOWNLOAD_PART_SIZE as usize;
    let total_size = part_size * 2;
    let payload: Vec<u8> = (0..total_size).map(|i| ((i * 23) & 0xFF) as u8).collect();
    let expected_sha = sha256_bytes(&payload);
    let payload_rpc = Arc::new(payload.clone());

    h.fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
        if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            return None;
        }
        let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode getFile");
        let start = req.offset as usize;
        let end = start
            .saturating_add(req.limit as usize)
            .min(payload_rpc.len());
        let resp = tl_enums::upload::File::File(tl_types::upload::File {
            r#type: tl_enums::storage::FileType::FileMp4,
            mtime: 1_740_000_500,
            bytes: payload_rpc[start..end].to_vec(),
        });
        Some(Ok(resp.to_bytes()))
    }));

    let doc = DocumentData {
        id: DocumentId(905_001),
        access_hash: 111_222,
        file_reference: vec![1, 1],
        dc_id: 2,
        size: total_size as i64,
        mime_type: "video/mp4".to_owned(),
        file_name: "final.mp4".to_owned(),
        kind: DocumentKind::Video,
        ..DocumentData::default()
    };
    let loader = TelegramRangeLoader::new(
        h.fake.clone(),
        h.runtime.tokio_handle().clone(),
        document_to_location(&doc),
    )
    .with_refresh_origin(Arc::clone(h.runtime.session()), FileOrigin::default())
    .with_storage_dir(h.tmp.path().join("final_stream"))
    .with_expected_sha256(expected_sha);

    let finalized_path = loader.download_to_file().expect("finalize with SHA-256");
    assert!(finalized_path.is_file());
    // The finalized file is encrypted on disk; its content is read back through the loader.
    assert_ne!(fs::read(&finalized_path).expect("read finalized"), payload);
    assert_eq!(
        loader
            .load_range(0, payload.len())
            .expect("read finalized content"),
        payload
    );
    assert!(loader.progress().expect("progress").finished);
}

#[path = "tests_sec3b.rs"]
mod sec3b;
