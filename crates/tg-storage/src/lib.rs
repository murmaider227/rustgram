//! Byte-compatible Rust port of Telegram Desktop's local storage (`tdata`) and cache layers.
//!
//! Ported from `Telegram/SourceFiles/storage/` and `Telegram/lib_storage/storage/`:
//! - [`qdatastream`]: Big-endian `QDataStream::Qt_5_1` reader/writer
//! - [`tdf`]: `TDF$` container format, MD5 checksums, `ToFilePart` naming, AES-IGE local encryption
//! - [`domain`]: `key_datas` (legacy 3-blob and v2 Argon2id/Scrypt passcode wraps), account slots
//! - [`account`]: Per-account `map` (`lsk*` keys), drafts, cursors, locations, prefs, stickers,
//!   saved GIFs, trusted peers, export settings, bot storages, wallet engine records
//! - [`settings_scheme`]: All `dbi*` setting block IDs and parsers
//! - [`local`]: Global `settings`, themes, backgrounds, langpacks, recent languages, update manifest
//! - [`mtp_auth`]: MTProto authorization serialization (modern wide-ID and legacy 32-bit formats)
//! - [`peer_id`]: `PeerId` and `DraftKey` serialization
//! - [`serialize`]: `SerializedPeer`, `SerializedDocument`, `ImageLocation`, and `TextTag`
//! - [`encrypted_file`]: `TDEF` AES-256-CTR encrypted files (`lib_storage`)
//! - [`cache`]: `lib_storage` binlog cache DB (`cache::binlog`) and Rustgram SQLite media cache (`cache::media_cache`)
//! - [`write_manager`]: Async coalescing `WriteManager` and debounced per-account write timers
//! - [`cloud_blob`]: Staged cloud blob downloader, SHA-256 verifier, and safe atomic ZIP unpacker
//! - [`folder_archive`]: Deterministic folder archival and safe extraction with size/count caps
//! - [`streamed_downloader`]: Sparse range streamed file downloader with seek priority and resume

pub mod account;
pub mod cache;
pub mod cloud_blob;
pub mod domain;
pub mod encrypted_file;
pub mod error;
pub mod folder_archive;
pub mod local;
pub mod mtp_auth;
pub mod peer_id;
pub mod qdatastream;
pub mod serialize;
pub mod session_settings;
pub mod settings_scheme;
pub mod streamed_downloader;
pub mod tdf;
pub mod write_manager;

pub use account::{AccountMap, AccountStorage, LskKeyType};
pub use cloud_blob::{
    AsyncCloudBlobSource, BlobChunkFuture, BlobProgress, BlobState, CLOUD_LOCATION_USERNAME,
    CloudBlobDescriptor, CloudBlobError, CloudBlobLoader, CloudBlobLocation, CloudBlobResult,
    CloudBlobSource, HttpCloudBlobSource, MAX_CLOUD_BLOB_ZIP_SIZE, MAX_EXTRACTED_FILE_SIZE,
    UnpackOptions, UnpackReport, is_safe_archive_path, sha256_file, sha256_hex, state_description,
    unpack_blob, unpack_blob_bytes, verify_sha256,
};
pub use domain::{AccountsInfo, DomainStorage, KeyData, PasscodeVerification, PasscodeWrap};
pub use encrypted_file::{EncryptedFile, OpenMode};
pub use error::{Result, StorageError};
pub use folder_archive::{
    ARCHIVE_CHUNK_SIZE, ARCHIVE_ENTRY_OVERHEAD, ArchiveEntries, ArchiveEntry, ArchiveExtractResult,
    ArchiveLimits, ArchiveProgress, ArchiveWriteResult, ArchiveWriteStatus, FolderArchiveError,
    FolderArchiveResult, PreparedFileArchive, STALE_ARCHIVE_TIMEOUT, STORED_EXTENSIONS,
    archive_folder, archive_size_estimate, clear_stale_archive_files, extract_folder_archive,
    folder_files_for_sending, gather_archive_entries, is_already_compressed_name,
    prepare_files_archive, prepare_folder_archive, single_folder_path, write_archive,
    write_archive_to_path,
};
pub use local::{GlobalSettings, LocalStorage};
pub use mtp_auth::{MtpAuthKeyEntry, MtpAuthorization};
pub use peer_id::{DraftKey, PeerId};
pub use qdatastream::{
    DataStreamError, QDATASTREAM_NULL_LEN, QDataStreamReader, QDataStreamWriter, QtColor,
    QtDateTime,
};
pub use session_settings::{
    AutoDownloadOverride, AutoDownloadSource, AutoDownloadType, StoredAutoDownload,
    StoredReactionId, StoredSessionSettings, StoredThreadId, StoredUnreviewedAuth,
};
pub use settings_scheme::{DbiBlockId, LegacyProxyData, LegacyProxySettings, ReadSettingsContext};
pub use streamed_downloader::{
    ChunkFuture, ChunkProvider, DEFAULT_MAX_IN_FLIGHT_PARTS, DownloadProgress, PARTIAL_META_MAGIC,
    PARTIAL_META_VERSION, PartPriorityQueue, PartialFileMetadata, STREAMED_PART_SIZE,
    StreamedDownloadError, StreamedDownloadResult, StreamedDownloaderConfig,
    StreamedFileDownloader,
};
pub use tdf::{
    APP_VERSION, FileKey, FileReadDescriptor, FileWriteDescriptor, TDF_MAGIC, compose_data_string,
    compute_data_name_key, from_file_part, to_file_part,
};
pub use tg_crypto::{
    AuthKey, PasscodeKdf, PasscodeKdfKind, StorageCtrState, StorageEncryptionKey,
    StorageEncryptionKey as EncryptionKey,
};
pub use write_manager::{
    AccountWriteScheduler, AsyncWriteManager, DELAYED_WRITE_TIMEOUT, RemoveEntry,
    WRITE_SEARCH_SUGGESTIONS_DELAY, WriteEntry,
};
