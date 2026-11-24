//! Integration tests for WP-44:
//! - `cloud_blob`: staged download (local HTTP server & fake async RPC), SHA-256 verification,
//!   safe ZIP extraction (rejecting zip-slip, absolute paths, symlinks), atomic directory swap.
//! - `folder_archive`: deterministic folder archival & extraction round-trip, file-count & byte
//!   budgets, progress/cancellation, and stale archive cleanup.
//! - `streamed_downloader`: out-of-order range seeks, in-flight offset deduplication, seek-priority
//!   cancellation, resumable partial-file metadata across reopen, and final byte equality.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;
use tg_crypto::sha256_bytes;
use tg_storage::cache::binlog::CacheKey;
use tg_storage::{
    ArchiveLimits, ArchiveWriteStatus, AsyncCloudBlobSource, BlobChunkFuture, BlobProgress,
    BlobState, CLOUD_LOCATION_USERNAME, ChunkFuture, ChunkProvider, CloudBlobError,
    CloudBlobLoader, CloudBlobLocation, FolderArchiveError, HttpCloudBlobSource, PartPriorityQueue,
    PartialFileMetadata, STALE_ARCHIVE_TIMEOUT, StreamedDownloadError, StreamedDownloaderConfig,
    StreamedFileDownloader, UnpackOptions, archive_folder, archive_size_estimate,
    clear_stale_archive_files, extract_folder_archive, folder_files_for_sending,
    gather_archive_entries, is_already_compressed_name, is_safe_archive_path,
    prepare_files_archive, prepare_folder_archive, single_folder_path, state_description,
    unpack_blob, unpack_blob_bytes, write_archive_to_path,
};
use tokio::sync::Notify;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

// ---------------------------------------------------------------------------
// Synthetic ZIP helpers
// ---------------------------------------------------------------------------

fn build_zip(entries: &[(&str, &[u8], Option<u32>)]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut cursor);
        for &(name, data, unix_mode) in entries {
            let mut opts =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            if let Some(mode) = unix_mode {
                opts = opts.unix_permissions(mode);
            }
            if name.ends_with('/') {
                writer.add_directory(name, opts).unwrap();
            } else {
                writer.start_file(name, opts).unwrap();
                writer.write_all(data).unwrap();
            }
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn spawn_local_http_server(
    routes: HashMap<String, Vec<u8>>,
    max_requests: usize,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1 ephemeral port");
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    let handle = thread::spawn(move || {
        for _ in 0..max_requests {
            let (mut stream, _) = match listener.accept() {
                Ok(conn) => conn,
                Err(_) => break,
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut req_line = String::new();
            let _ = reader.read_line(&mut req_line);
            loop {
                let mut header = String::new();
                let n = reader.read_line(&mut header).unwrap_or(0);
                if n == 0 || header == "\r\n" || header == "\n" {
                    break;
                }
            }
            let path = req_line.split_whitespace().nth(1).unwrap_or("/");
            if let Some(body) = routes.get(path) {
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body);
            } else {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
            let _ = stream.flush();
        }
    });

    (base_url, handle)
}

// ===========================================================================
// 1. Cloud Blob Downloader & Unpacker tests
// ===========================================================================

#[test]
fn cloud_blob_unpack_valid_archive_and_atomic_swap() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("emoji_pack");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("legacy.txt"), b"old version content").unwrap();

    let zip_bytes = build_zip(&[
        ("manifest.json", br#"{"version":2}"#, Some(0o644)),
        ("sprites/", &[], Some(0o755)),
        ("sprites/01.webp", b"WEBP_PAYLOAD_1", Some(0o644)),
        ("sprites/ignore.tmp", b"SKIP_ME", Some(0o644)),
    ]);
    let expected = sha256_bytes(&zip_bytes);

    let archive_path = tmp.path().join("blob.zip");
    fs::write(&archive_path, &zip_bytes).unwrap();

    let options = UnpackOptions {
        expected_sha256: Some(expected),
        ..UnpackOptions::default()
    };
    let report = unpack_blob(&archive_path, &target, &options, |name| {
        !name.ends_with(".tmp")
    })
    .expect("valid archive should unpack");

    assert_eq!(report.target_dir, target);
    assert_eq!(report.sha256, expected);
    assert_eq!(
        report.extracted_files,
        vec!["manifest.json".to_string(), "sprites/01.webp".to_string()]
    );
    assert!(!target.join("legacy.txt").exists(), "old files replaced");
    assert_eq!(
        fs::read(target.join("manifest.json")).unwrap(),
        br#"{"version":2}"#
    );
    assert_eq!(
        fs::read(target.join("sprites/01.webp")).unwrap(),
        b"WEBP_PAYLOAD_1"
    );
    assert!(!target.join("sprites/ignore.tmp").exists());

    // Confirm no leftover staging or backup sibling dirs remain.
    let siblings: Vec<String> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !siblings
            .iter()
            .any(|s| s.contains(".staging-") || s.contains(".backup-")),
        "unexpected leftover staging/backup dirs: {siblings:?}"
    );
}

#[test]
fn cloud_blob_fails_closed_on_sha256_mismatch_leaving_prior_dir_untouched() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("active_pack");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("sentinel.txt"), b"prior active data").unwrap();

    let zip_bytes = build_zip(&[("new.txt", b"should not appear", Some(0o644))]);
    let mut wrong_sha = sha256_bytes(&zip_bytes);
    wrong_sha[0] ^= 0xFF;

    let options = UnpackOptions {
        expected_sha256: Some(wrong_sha),
        ..UnpackOptions::default()
    };
    let err = unpack_blob_bytes(&zip_bytes, &target, &options, |_| true)
        .expect_err("SHA-256 mismatch must fail closed");
    assert!(matches!(err, CloudBlobError::Sha256Mismatch { .. }));

    // Prior active directory must be completely intact.
    assert_eq!(
        fs::read(target.join("sentinel.txt")).unwrap(),
        b"prior active data"
    );
    assert!(!target.join("new.txt").exists());
}

#[test]
fn cloud_blob_fails_closed_on_zip_slip_absolute_backslash_reserved_and_symlink() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("active_blob");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep.txt"), b"must stay untouched").unwrap();

    let bad_path_cases: &[(&str, &str)] = &[
        ("zip_slip_parent", "../evil.txt"),
        ("zip_slip_nested", "ok/../../evil.txt"),
        ("absolute_posix", "/etc/passwd"),
        ("backslash", r"sub\evil.txt"),
        ("windows_drive", "C:evil.txt"),
        ("windows_reserved", "sub/CON.txt"),
    ];

    for &(label, bad_name) in bad_path_cases {
        let zip_bytes = build_zip(&[
            ("good.txt", b"benign", Some(0o644)),
            (bad_name, b"pwned", Some(0o644)),
        ]);
        // Even if check_name would skip the malicious entry, unpacker MUST still fail closed.
        let err = unpack_blob_bytes(&zip_bytes, &target, &UnpackOptions::default(), |n| {
            n == "good.txt"
        })
        .expect_err("malicious archive path must fail closed");
        assert!(
            matches!(err, CloudBlobError::UnsafePath(_)),
            "case {label} ({bad_name}) expected UnsafePath, got {err:?}"
        );
        assert!(
            !tmp.path().join("evil.txt").exists(),
            "case {label} escaped"
        );
        assert_eq!(
            fs::read(target.join("keep.txt")).unwrap(),
            b"must stay untouched",
            "case {label} modified prior active directory"
        );
        assert!(!target.join("good.txt").exists());
    }

    // Symlink entry (written via ZipWriter::add_symlink with POSIX S_IFLNK = 0o120000).
    let symlink_zip = {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            writer
                .start_file("good.txt", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"ok").unwrap();
            writer
                .add_symlink("link_out", "/etc/hosts", SimpleFileOptions::default())
                .unwrap();
            writer.finish().unwrap();
        }
        cursor.into_inner()
    };
    let err = unpack_blob_bytes(&symlink_zip, &target, &UnpackOptions::default(), |_| true)
        .expect_err("symlink entry must be rejected");
    assert!(matches!(err, CloudBlobError::SymlinkRejected(_)));
    assert_eq!(
        fs::read(target.join("keep.txt")).unwrap(),
        b"must stay untouched"
    );

    assert!(is_safe_archive_path("sprites/emoji_01.webp"));
    assert!(!is_safe_archive_path("../evil"));
    assert!(!is_safe_archive_path("/abs/file"));
    assert!(!is_safe_archive_path("AUX.dat"));
}

#[test]
fn cloud_blob_enforces_size_and_count_limits() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("limited_blob");

    // 1. Archive size cap
    let zip_bytes = build_zip(&[("a.bin", &[0xAB; 4096], Some(0o644))]);
    let opts = UnpackOptions {
        max_archive_size: 32,
        ..UnpackOptions::default()
    };
    let err = unpack_blob_bytes(&zip_bytes, &target, &opts, |_| true).unwrap_err();
    assert!(matches!(err, CloudBlobError::ArchiveTooLarge { .. }));

    // 2. Single-file uncompressed cap (catches zip bombs)
    let bomb_like = build_zip(&[("big.txt", &vec![b'A'; 100_000], Some(0o644))]);
    let opts = UnpackOptions {
        max_file_size: 4096,
        ..UnpackOptions::default()
    };
    let err = unpack_blob_bytes(&bomb_like, &target, &opts, |_| true).unwrap_err();
    assert!(matches!(err, CloudBlobError::FileSizeLimitExceeded { .. }));

    // 3. Total uncompressed cap across multiple small files
    let multi = build_zip(&[
        ("a.txt", &vec![b'x'; 2000], Some(0o644)),
        ("b.txt", &vec![b'y'; 2000], Some(0o644)),
    ]);
    let opts = UnpackOptions {
        max_file_size: 3000,
        max_total_uncompressed_size: 3500,
        ..UnpackOptions::default()
    };
    let err = unpack_blob_bytes(&multi, &target, &opts, |_| true).unwrap_err();
    assert!(matches!(err, CloudBlobError::TotalSizeLimitExceeded { .. }));

    // 4. Max file count cap
    let opts = UnpackOptions {
        max_file_count: 1,
        ..UnpackOptions::default()
    };
    let err = unpack_blob_bytes(&multi, &target, &opts, |_| true).unwrap_err();
    assert!(matches!(err, CloudBlobError::FileCountLimitExceeded { .. }));

    assert!(!target.exists());
}

#[test]
fn cloud_blob_staged_download_via_local_fake_http_server() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("http_blob");

    let valid_zip = build_zip(&[("theme/data.txt", b"hello from 127.0.0.1", Some(0o644))]);
    let valid_sha = sha256_bytes(&valid_zip);

    let mut tampered_zip = valid_zip.clone();
    tampered_zip[0] ^= 0x01;

    let mut routes = HashMap::new();
    routes.insert(format!("/{CLOUD_LOCATION_USERNAME}/42"), valid_zip);
    routes.insert(format!("/{CLOUD_LOCATION_USERNAME}/43"), tampered_zip);
    let (base_url, server_handle) = spawn_local_http_server(routes, 2);

    let source = HttpCloudBlobSource::new(&base_url);

    // First download: valid archive -> Ready and unpacked.
    let loader_ok = CloudBlobLoader::new(
        1,
        CloudBlobLocation::official(42),
        &target,
        1024,
        UnpackOptions {
            expected_sha256: Some(valid_sha),
            ..UnpackOptions::default()
        },
    );
    let report = loader_ok
        .download_and_unpack_sync(&source, |_| true)
        .expect("HTTP staged download should succeed");
    assert_eq!(loader_ok.id(), 1);
    assert_eq!(loader_ok.current_state(), BlobState::Ready);
    assert_eq!(report.sha256, valid_sha);
    assert_eq!(
        fs::read(target.join("theme/data.txt")).unwrap(),
        b"hello from 127.0.0.1"
    );

    // Second download: tampered archive against same target -> Failed, prior content untouched.
    let loader_bad = CloudBlobLoader::new(
        2,
        CloudBlobLocation::official(43),
        &target,
        1024,
        UnpackOptions {
            expected_sha256: Some(valid_sha),
            ..UnpackOptions::default()
        },
    );
    let err = loader_bad
        .download_and_unpack_sync(&source, |_| true)
        .expect_err("tampered archive must fail SHA-256");
    assert!(matches!(err, CloudBlobError::Sha256Mismatch { .. }));
    assert!(matches!(loader_bad.current_state(), BlobState::Failed(_)));
    assert_eq!(
        fs::read(target.join("theme/data.txt")).unwrap(),
        b"hello from 127.0.0.1"
    );

    server_handle.join().unwrap();
}

struct FakeAsyncRpcBlobSource {
    blobs: HashMap<(String, i32), Vec<u8>>,
}

impl AsyncCloudBlobSource for FakeAsyncRpcBlobSource {
    fn download_chunk(
        &self,
        location: &CloudBlobLocation,
        offset: u64,
        limit: usize,
    ) -> BlobChunkFuture<'_> {
        let key = (location.username.clone(), location.post_id);
        let data = self.blobs.get(&key).cloned();
        Box::pin(async move {
            let bytes = data.ok_or_else(|| {
                CloudBlobError::DownloadFailed(format!("unknown blob post {}", key.1))
            })?;
            let start = offset as usize;
            if start >= bytes.len() {
                return Ok(Vec::new());
            }
            let end = (start + limit).min(bytes.len());
            Ok(bytes[start..end].to_vec())
        })
    }
}

#[tokio::test]
async fn cloud_blob_staged_download_via_fake_async_rpc_and_state_descriptions() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("rpc_blob");

    let zip_bytes = build_zip(&[("pack/info.txt", &vec![b'Z'; 5000], Some(0o644))]);
    let sha = sha256_bytes(&zip_bytes);

    let mut blobs = HashMap::new();
    blobs.insert((CLOUD_LOCATION_USERNAME.to_string(), 99), zip_bytes.clone());
    let rpc = Arc::new(FakeAsyncRpcBlobSource { blobs });

    let loader = CloudBlobLoader::new(
        7,
        CloudBlobLocation::official(99),
        &target,
        zip_bytes.len() as u64,
        UnpackOptions {
            expected_sha256: Some(sha),
            ..UnpackOptions::default()
        },
    );
    let report = loader
        .download_and_unpack_async(rpc, 1024, |_| true)
        .await
        .expect("async fake RPC download should succeed");
    assert_eq!(report.extracted_files, vec!["pack/info.txt".to_string()]);
    assert_eq!(loader.current_state(), BlobState::Ready);
    assert_eq!(fs::read(target.join("pack/info.txt")).unwrap().len(), 5000);

    // State descriptions
    assert!(state_description(&BlobState::Available { size: 2048 }, "Using").contains("2.0 KiB"));
    assert_eq!(
        state_description(&BlobState::Active, "Active Set"),
        "Active Set"
    );
    assert_eq!(state_description(&BlobState::Ready, "Active Set"), "Ready");
    let loading = BlobState::Loading(BlobProgress {
        already: 512,
        size: 1024,
    });
    assert!(state_description(&loading, "Active").contains("50%"));
}

// ===========================================================================
// 2. Folder Archive tests
// ===========================================================================

fn snapshot_tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    let mut map = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                map.insert(format!("{rel}/"), None);
                stack.push(path);
            } else if meta.is_file() {
                map.insert(rel, Some(fs::read(&path).unwrap()));
            }
        }
    }
    map
}

#[test]
fn folder_archive_round_trips_nested_tree_deterministically() {
    let tmp = TempDir::new().unwrap();
    let src_root = tmp.path().join("project_bundle");
    fs::create_dir_all(src_root.join("docs/nested")).unwrap();
    fs::create_dir_all(src_root.join("assets")).unwrap();
    fs::create_dir_all(src_root.join("empty_dir")).unwrap();

    fs::write(src_root.join("README.md"), b"# Deterministic Bundle\n").unwrap();
    fs::write(
        src_root.join("docs/nested/notes.txt"),
        "repeated compressible content ".repeat(200),
    )
    .unwrap();
    fs::write(
        src_root.join("assets/logo.png"),
        b"\x89PNG\r\n\x1a\nFAKEPNG",
    )
    .unwrap();
    fs::write(src_root.join(".hidden"), b"hidden-config").unwrap();

    let zip_a = tmp.path().join("out_a.zip");
    let zip_b = tmp.path().join("out_b.zip");
    let limits = ArchiveLimits::default();

    let mut progress_points = Vec::new();
    let res_a = archive_folder(&src_root, &zip_a, &limits, |p| {
        progress_points.push(p.fraction);
        true
    })
    .expect("first archive must succeed");
    assert_eq!(res_a.status, ArchiveWriteStatus::Done);
    assert_eq!(res_a.path.as_deref(), Some(zip_a.as_path()));
    assert!(!progress_points.is_empty());
    assert!(
        progress_points
            .windows(2)
            .all(|w| w[0] <= w[1] + f64::EPSILON),
        "progress should be monotonic: {progress_points:?}"
    );
    assert!((progress_points.last().copied().unwrap() - 1.0).abs() < 1e-9);

    let res_b =
        archive_folder(&src_root, &zip_b, &limits, |_| true).expect("second archive must succeed");
    assert_eq!(res_b.status, ArchiveWriteStatus::Done);

    // Deterministic output: both ZIP files must be byte-for-byte identical.
    let bytes_a = fs::read(&zip_a).unwrap();
    let bytes_b = fs::read(&zip_b).unwrap();
    assert_eq!(
        bytes_a, bytes_b,
        "deterministic folder archive must match byte-for-byte"
    );

    // Verify compression method selection (Stored for .png, Deflated for .txt).
    {
        let mut reader = ZipArchive::new(Cursor::new(&bytes_a)).unwrap();
        let png = reader.by_name("project_bundle/assets/logo.png").unwrap();
        assert_eq!(png.compression(), CompressionMethod::Stored);
        drop(png);
        let txt = reader
            .by_name("project_bundle/docs/nested/notes.txt")
            .unwrap();
        assert_eq!(txt.compression(), CompressionMethod::Deflated);
    }

    // Round-trip extraction and compare full nested tree including empty directory.
    let extracted_parent = tmp.path().join("extracted");
    let extract_report =
        extract_folder_archive(&zip_a, &extracted_parent, &limits, |_| true).unwrap();
    assert_eq!(extract_report.files_extracted, 4);
    assert!(extract_report.dirs_created >= 4);

    let extracted_root = extracted_parent.join("project_bundle");
    assert_eq!(snapshot_tree(&src_root), snapshot_tree(&extracted_root));
}

#[test]
fn folder_archive_enforces_file_count_byte_budgets_and_cancellation() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("budget_src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("f1.bin"), vec![1u8; 4000]).unwrap();
    fs::write(src.join("f2.bin"), vec![2u8; 4000]).unwrap();
    fs::write(src.join("f3.bin"), vec![3u8; 4000]).unwrap();

    let job = prepare_folder_archive(&src);
    assert_eq!(job.display_name, "budget_src.zip");

    // 1. Max file count cap during gather
    let err = gather_archive_entries(
        &job,
        &ArchiveLimits {
            max_files: 2,
            ..ArchiveLimits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        FolderArchiveError::FileCountLimitExceeded { .. }
    ));

    // 2. Max total uncompressed byte budget during gather
    let err = gather_archive_entries(
        &job,
        &ArchiveLimits {
            max_total_uncompressed_bytes: 7000,
            ..ArchiveLimits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, FolderArchiveError::ByteBudgetExceeded { .. }));

    // 3. Max single-file byte limit during gather
    let err = gather_archive_entries(
        &job,
        &ArchiveLimits {
            max_single_file_bytes: 3000,
            ..ArchiveLimits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, FolderArchiveError::SingleFileTooLarge { .. }));

    // 4. Max compressed archive size limit during write -> TooLarge, no output file left behind
    let entries = gather_archive_entries(&job, &ArchiveLimits::default()).unwrap();
    assert!(archive_size_estimate(&entries) > 12_000);
    let out_too_large = tmp.path().join("too_large.zip");
    let res = write_archive_to_path(
        &entries,
        &out_too_large,
        &ArchiveLimits {
            max_archive_bytes: 100,
            ..ArchiveLimits::default()
        },
        |_| true,
    )
    .unwrap();
    assert_eq!(res.status, ArchiveWriteStatus::TooLarge);
    assert!(res.path.is_none());
    assert!(!out_too_large.exists());

    // 5. Cancellation via progress callback -> Cancelled, no output file left behind
    let out_cancelled = tmp.path().join("cancelled.zip");
    let res = write_archive_to_path(&entries, &out_cancelled, &ArchiveLimits::default(), |_| {
        false
    })
    .unwrap();
    assert_eq!(res.status, ArchiveWriteStatus::Cancelled);
    assert!(!out_cancelled.exists());

    // 6. Write valid archive, then enforce file count & byte budgets during extraction
    let valid_zip = tmp.path().join("valid.zip");
    let ok =
        write_archive_to_path(&entries, &valid_zip, &ArchiveLimits::default(), |_| true).unwrap();
    assert_eq!(ok.status, ArchiveWriteStatus::Done);

    let err = extract_folder_archive(
        &valid_zip,
        &tmp.path().join("ext_count"),
        &ArchiveLimits {
            max_files: 2,
            ..ArchiveLimits::default()
        },
        |_| true,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        FolderArchiveError::FileCountLimitExceeded { .. }
    ));

    let err = extract_folder_archive(
        &valid_zip,
        &tmp.path().join("ext_bytes"),
        &ArchiveLimits {
            max_total_uncompressed_bytes: 7000,
            ..ArchiveLimits::default()
        },
        |_| true,
    )
    .unwrap_err();
    assert!(matches!(err, FolderArchiveError::ByteBudgetExceeded { .. }));
}

#[test]
fn folder_archive_helpers_dedupe_and_stale_cleanup() {
    let tmp = TempDir::new().unwrap();
    let dir_a = tmp.path().join("dir_a");
    let dir_b = tmp.path().join("dir_b");
    let sub_a = dir_a.join("sub");
    fs::create_dir_all(&sub_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();

    let f1 = dir_a.join("report.txt");
    let f2 = dir_b.join("report.txt");
    let f3 = sub_a.join("REPORT.TXT");
    fs::write(&f1, b"first").unwrap();
    fs::write(&f2, b"second").unwrap();
    fs::write(&f3, b"third").unwrap();

    assert_eq!(
        single_folder_path(std::slice::from_ref(&dir_a)),
        Some(dir_a.clone())
    );
    assert_eq!(single_folder_path(std::slice::from_ref(&f1)), None);
    assert_eq!(single_folder_path(&[dir_a.clone(), dir_b.clone()]), None);

    let send_files = folder_files_for_sending(&dir_a).unwrap();
    assert_eq!(send_files, vec![f1.clone(), f3.clone()]);

    let files_job = prepare_files_archive(&[f1, f2, f3], &[]);
    let entries = gather_archive_entries(&files_job, &ArchiveLimits::default()).unwrap();
    let names: Vec<&str> = entries.list.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["REPORT (3).TXT", "report (2).txt", "report.txt"]
    );

    assert!(is_already_compressed_name("video/clip.MP4"));
    assert!(is_already_compressed_name("photo.webp"));
    assert!(!is_already_compressed_name("notes.txt"));
    assert!(!is_already_compressed_name(".gitignore"));

    // Stale archive cleanup
    let archive_tmp = tmp.path().join("tdarchive");
    fs::create_dir_all(&archive_tmp).unwrap();
    fs::write(archive_tmp.join("old.zip"), b"stale").unwrap();
    assert_eq!(
        clear_stale_archive_files(&archive_tmp, STALE_ARCHIVE_TIMEOUT).unwrap(),
        0
    );
    // Small sleep or zero duration cutoff removes existing file.
    thread::sleep(Duration::from_millis(15));
    assert_eq!(
        clear_stale_archive_files(&archive_tmp, Duration::from_millis(1)).unwrap(),
        1
    );
    assert!(!archive_tmp.join("old.zip").exists());

    // Zip-slip and symlink rejection on folder archive extract
    let evil_zip = tmp.path().join("evil.zip");
    fs::write(
        &evil_zip,
        build_zip(&[("../escape.txt", b"nope", Some(0o644))]),
    )
    .unwrap();
    let err = extract_folder_archive(
        &evil_zip,
        &tmp.path().join("safe_dest"),
        &ArchiveLimits::default(),
        |_| true,
    )
    .unwrap_err();
    assert!(matches!(err, FolderArchiveError::UnsafePath(_)));

    let symlink_zip = tmp.path().join("symlink.zip");
    {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            writer
                .add_symlink("escape_link", "/etc/hosts", SimpleFileOptions::default())
                .unwrap();
            writer.finish().unwrap();
        }
        fs::write(&symlink_zip, cursor.into_inner()).unwrap();
    }
    let err = extract_folder_archive(
        &symlink_zip,
        &tmp.path().join("safe_dest_symlink"),
        &ArchiveLimits::default(),
        |_| true,
    )
    .unwrap_err();
    assert!(matches!(err, FolderArchiveError::SymlinkRejected(_)));
}

// ===========================================================================
// 3. Streamed File Downloader tests
// ===========================================================================

struct FakeChunkProvider {
    data: Vec<u8>,
    fetch_counts: Mutex<HashMap<u64, usize>>,
    cancelled_offsets: Mutex<Vec<u64>>,
    /// Optional set of offsets that pause on `gate` before returning.
    gated_offsets: Mutex<Vec<u64>>,
    gate_open: AtomicBool,
    gate_notify: Notify,
    /// Optional notification fired whenever a gated offset begins fetching.
    started_gated: Notify,
    started_gated_count: AtomicUsize,
    /// Optional corruption hook: return truncated chunk for this offset.
    truncate_offset: Mutex<Option<u64>>,
}

impl FakeChunkProvider {
    fn new(data: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            data,
            fetch_counts: Mutex::new(HashMap::new()),
            cancelled_offsets: Mutex::new(Vec::new()),
            gated_offsets: Mutex::new(Vec::new()),
            gate_open: AtomicBool::new(true),
            gate_notify: Notify::new(),
            started_gated: Notify::new(),
            started_gated_count: AtomicUsize::new(0),
            truncate_offset: Mutex::new(None),
        })
    }

    fn gate_offsets(&self, offsets: &[u64]) {
        self.gate_open.store(false, Ordering::SeqCst);
        *self.gated_offsets.lock().unwrap() = offsets.to_vec();
    }

    fn open_gate(&self) {
        self.gate_open.store(true, Ordering::SeqCst);
        self.gate_notify.notify_waiters();
    }

    fn fetch_count(&self, offset: u64) -> usize {
        self.fetch_counts
            .lock()
            .unwrap()
            .get(&offset)
            .copied()
            .unwrap_or(0)
    }

    fn total_fetches(&self) -> usize {
        self.fetch_counts.lock().unwrap().values().sum()
    }

    fn cancelled(&self) -> Vec<u64> {
        self.cancelled_offsets.lock().unwrap().clone()
    }
}

impl ChunkProvider for FakeChunkProvider {
    fn fetch_chunk(&self, offset: u64, length: usize) -> ChunkFuture<'_> {
        Box::pin(async move {
            {
                let mut counts = self.fetch_counts.lock().unwrap();
                *counts.entry(offset).or_insert(0) += 1;
            }

            let is_gated = self.gated_offsets.lock().unwrap().contains(&offset);
            if is_gated {
                self.started_gated_count.fetch_add(1, Ordering::SeqCst);
                self.started_gated.notify_waiters();
                while !self.gate_open.load(Ordering::SeqCst) {
                    self.gate_notify.notified().await;
                }
            }

            let start = offset as usize;
            let end = (start + length).min(self.data.len());
            if start > self.data.len() {
                return Err(StreamedDownloadError::ProviderFailed {
                    offset,
                    message: "offset past EOF".into(),
                });
            }
            let mut slice = self.data[start..end].to_vec();
            if *self.truncate_offset.lock().unwrap() == Some(offset) && !slice.is_empty() {
                slice.pop();
            }
            Ok(slice)
        })
    }

    fn cancel_chunk(&self, offset: u64) {
        self.cancelled_offsets.lock().unwrap().push(offset);
    }
}

fn synthetic_payload(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| ((i.wrapping_mul(31).wrapping_add(17)) & 0xFF) as u8)
        .collect()
}

#[test]
fn streamed_priority_queue_orders_seeks_ahead_of_older_requests() {
    let mut q = PartPriorityQueue::new();
    assert!(q.add(0));
    assert!(q.add(4096));
    assert!(!q.add(4096)); // duplicate at same priority
    assert!(q.add(8192));

    // Seek bumps priority epoch; newly added offsets jump ahead.
    q.reset_priorities();
    assert!(q.add(65536));
    assert!(q.add(4096)); // elevated from epoch 0 to epoch 1

    assert_eq!(q.take(), Some(4096));
    assert_eq!(q.take(), Some(65536));
    assert_eq!(q.take_in_range(0, 8192), vec![0]);
    assert_eq!(q.take(), Some(8192));
    assert!(q.is_empty());
}

#[tokio::test]
async fn streamed_downloader_out_of_order_seeks_and_final_byte_equality() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("video.mp4");
    let part_size = 4096usize;
    let payload = synthetic_payload(12 * part_size + 1337);
    let expected_sha = sha256_bytes(&payload);

    let provider = FakeChunkProvider::new(payload.clone());
    let mut cfg = StreamedDownloaderConfig::new(1001, payload.len() as u64, &target);
    cfg.cache_key = CacheKey::new(0x1122, 0x3344);
    cfg.part_size = part_size;
    cfg.max_in_flight = 4;
    cfg.expected_sha256 = Some(expected_sha);

    let dl = StreamedFileDownloader::open(cfg, Arc::clone(&provider)).unwrap();

    // Out-of-order seek #1: tail spanning parts 11 and 12 (short tail part)
    let tail_offset = 11 * (part_size as u64) + 500;
    let tail_len = payload.len() - tail_offset as usize;
    let tail = dl.read_range(tail_offset, tail_len).await.unwrap();
    assert_eq!(tail, payload[tail_offset as usize..]);

    // Out-of-order seek #2: middle unaligned slice across parts 4..6
    let mid_offset = 4 * (part_size as u64) + 123;
    let mid_len = 2 * part_size + 77;
    let mid = dl.read_range(mid_offset, mid_len).await.unwrap();
    assert_eq!(
        mid,
        payload[mid_offset as usize..mid_offset as usize + mid_len]
    );

    // `read_loaded_part` returns saved parts without provider calls and None for unsaved parts.
    assert!(dl.read_loaded_part(0).unwrap().is_none());
    let part11 = dl
        .read_loaded_part(11 * part_size as u64)
        .unwrap()
        .expect("part 11 must be cached on disk");
    assert_eq!(part11, payload[11 * part_size..12 * part_size]);

    // Now finish downloading the entire file.
    let final_path = dl.download_all().await.unwrap();
    assert_eq!(final_path, target);
    assert!(dl.progress().finished);
    assert!(!dl.part_path().exists(), ".part should be renamed away");
    assert!(!dl.meta_path().exists(), ".part.meta should be removed");

    // Verify exact final byte equality and that NO part was fetched more than once.
    assert_eq!(fs::read(&target).unwrap(), payload);
    let total_parts = payload.len().div_ceil(part_size);
    for idx in 0..total_parts {
        assert_eq!(
            provider.fetch_count((idx * part_size) as u64),
            1,
            "part {idx} should be fetched exactly once"
        );
    }
    assert_eq!(provider.total_fetches(), total_parts);
}

#[tokio::test]
async fn streamed_downloader_deduplicates_concurrent_in_flight_offsets() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("dedupe.bin");
    let part_size = 4096usize;
    let payload = synthetic_payload(8 * part_size);

    let provider = FakeChunkProvider::new(payload.clone());
    let target_offset = 3 * (part_size as u64);
    provider.gate_offsets(&[target_offset]);

    let mut cfg = StreamedDownloaderConfig::new(2002, payload.len() as u64, &target);
    cfg.part_size = part_size;
    cfg.max_in_flight = 4;

    let dl = StreamedFileDownloader::open(cfg, Arc::clone(&provider)).unwrap();

    // Launch 5 concurrent readers requesting overlapping slices of part 3 while it is gated.
    let mut handles = Vec::new();
    for i in 0..5usize {
        let dl_clone = dl.clone();
        let start = target_offset + (i as u64) * 100;
        let len = 512usize;
        handles.push(tokio::spawn(async move {
            (
                start as usize,
                len,
                dl_clone.read_range(start, len).await.unwrap(),
            )
        }));
    }

    // Wait until the single in-flight fetch for part 3 reaches the gate, then let other tasks
    // enqueue as waiters before releasing the gate.
    while provider.started_gated_count.load(Ordering::SeqCst) == 0 {
        provider.started_gated.notified().await;
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    provider.open_gate();

    for handle in handles {
        let (start, len, bytes) = handle.await.unwrap();
        assert_eq!(bytes, payload[start..start + len]);
    }

    // Crucial assertion: all 5 concurrent requests shared ONE provider fetch!
    assert_eq!(
        provider.fetch_count(target_offset),
        1,
        "concurrent in-flight offset must be deduplicated"
    );
}

#[tokio::test]
async fn streamed_downloader_seek_priority_cancels_background_prefetch() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("seek_preempt.bin");
    let part_size = 4096usize;
    let payload = synthetic_payload(10 * part_size);

    let provider = FakeChunkProvider::new(payload.clone());
    // Parts 0 and 1 are gated (simulating slow sequential background download filling all slots).
    provider.gate_offsets(&[0, part_size as u64]);

    let mut cfg = StreamedDownloaderConfig::new(3003, payload.len() as u64, &target);
    cfg.part_size = part_size;
    cfg.max_in_flight = 2; // Both slots will be occupied by gated parts 0 & 1.
    cfg.expected_sha256 = Some(sha256_bytes(&payload));

    let dl = StreamedFileDownloader::open(cfg, Arc::clone(&provider)).unwrap();

    // Start background full download.
    let bg_dl = dl.clone();
    let bg_task = tokio::spawn(async move { bg_dl.download_all().await });

    // Wait until both background slots (0 and 4096) are in flight and blocked on the gate.
    while provider.started_gated_count.load(Ordering::SeqCst) < 2 {
        provider.started_gated.notified().await;
    }

    // Seek to part 8 while the gate for parts 0 & 1 is STILL CLOSED!
    // Seek-priority preemption must cancel the slow background parts so part 8 returns immediately.
    let seek_offset = 8 * (part_size as u64);
    let sought = tokio::time::timeout(
        Duration::from_secs(2),
        dl.read_range(seek_offset, part_size),
    )
    .await
    .expect("seek must preempt background prefetch rather than blocking")
    .unwrap();
    assert_eq!(
        sought,
        payload[seek_offset as usize..seek_offset as usize + part_size]
    );

    let cancelled = provider.cancelled();
    assert!(
        cancelled.contains(&0) && cancelled.contains(&(part_size as u64)),
        "expected background parts 0 and {part_size} to be cancelled on seek, got {cancelled:?}"
    );

    // Release gate so background `download_all` finishes the re-queued parts and verifies SHA-256.
    provider.open_gate();
    let final_path = bg_task.await.unwrap().unwrap();
    assert_eq!(final_path, target);
    assert_eq!(fs::read(&target).unwrap(), payload);
}

#[tokio::test]
async fn streamed_downloader_persists_partial_progress_across_reopen() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("resumable.bin");
    let part_size = 4096usize;
    let payload = synthetic_payload(10 * part_size + 511);
    let expected_sha = sha256_bytes(&payload);

    // Session 1: download 3 out-of-order parts, then drop downloader without finishing.
    let (part_path, meta_path) = {
        let provider1 = FakeChunkProvider::new(payload.clone());
        let mut cfg1 = StreamedDownloaderConfig::new(4004, payload.len() as u64, &target);
        cfg1.cache_key = CacheKey::new(7, 42);
        cfg1.part_size = part_size;
        cfg1.max_in_flight = 4;
        cfg1.expected_sha256 = Some(expected_sha);

        let dl1 = StreamedFileDownloader::open(cfg1, Arc::clone(&provider1)).unwrap();
        let _ = dl1.read_range(0, part_size).await.unwrap();
        let _ = dl1
            .read_range(5 * part_size as u64, part_size)
            .await
            .unwrap();
        let _ = dl1.read_range(10 * part_size as u64, 511).await.unwrap();

        assert_eq!(dl1.progress().saved_parts, 3);
        assert_eq!(provider1.total_fetches(), 3);
        (dl1.part_path().to_path_buf(), dl1.meta_path().to_path_buf())
    };

    // Inspect persisted metadata on disk before reopening.
    assert!(part_path.is_file());
    assert!(meta_path.is_file());
    let loaded_meta = PartialFileMetadata::load(&meta_path).unwrap();
    assert_eq!(loaded_meta.object_id, 4004);
    assert_eq!(loaded_meta.saved_parts_count(), 3);
    assert!(loaded_meta.is_part_saved(0));
    assert!(loaded_meta.is_part_saved(5));
    assert!(loaded_meta.is_part_saved(10));
    assert!(!loaded_meta.is_part_saved(1));

    // Session 2: reopen with a brand-new provider and complete the download.
    let provider2 = FakeChunkProvider::new(payload.clone());
    let mut cfg2 = StreamedDownloaderConfig::new(4004, payload.len() as u64, &target);
    cfg2.cache_key = CacheKey::new(7, 42);
    cfg2.part_size = part_size;
    cfg2.max_in_flight = 4;
    cfg2.expected_sha256 = Some(expected_sha);

    let dl2 = StreamedFileDownloader::open(cfg2, Arc::clone(&provider2)).unwrap();
    assert_eq!(dl2.progress().saved_parts, 3);

    // Already-saved parts read directly from disk with zero provider calls.
    assert_eq!(
        dl2.read_loaded_part(5 * part_size as u64).unwrap().unwrap(),
        payload[5 * part_size..6 * part_size]
    );
    assert_eq!(provider2.total_fetches(), 0);

    // Finish remaining 8 parts.
    let final_path = dl2.download_all().await.unwrap();
    assert_eq!(final_path, target);
    assert_eq!(fs::read(&target).unwrap(), payload);

    // Provider 2 must NOT have been asked for parts 0, 5, or 10.
    assert_eq!(provider2.fetch_count(0), 0);
    assert_eq!(provider2.fetch_count(5 * part_size as u64), 0);
    assert_eq!(provider2.fetch_count(10 * part_size as u64), 0);
    assert_eq!(provider2.total_fetches(), 8);
}

#[tokio::test]
async fn streamed_downloader_fails_closed_on_sha256_mismatch_and_bad_chunk_length() {
    let tmp = TempDir::new().unwrap();
    let part_size = 4096usize;
    let payload = synthetic_payload(3 * part_size);

    // 1. Final SHA-256 mismatch
    let target_bad_sha = tmp.path().join("bad_sha.bin");
    let mut wrong_sha = sha256_bytes(&payload);
    wrong_sha[31] ^= 0x01;
    let provider = FakeChunkProvider::new(payload.clone());
    let mut cfg = StreamedDownloaderConfig::new(5005, payload.len() as u64, &target_bad_sha);
    cfg.part_size = part_size;
    cfg.expected_sha256 = Some(wrong_sha);

    let dl = StreamedFileDownloader::open(cfg, provider).unwrap();
    let err = dl.download_all().await.unwrap_err();
    assert!(matches!(err, StreamedDownloadError::Sha256Mismatch { .. }));
    assert!(
        !target_bad_sha.exists(),
        "corrupt file must not be promoted"
    );

    // 2. Truncated chunk length from provider
    let target_bad_len = tmp.path().join("bad_len.bin");
    let bad_provider = FakeChunkProvider::new(payload.clone());
    *bad_provider.truncate_offset.lock().unwrap() = Some(part_size as u64);
    let mut cfg2 = StreamedDownloaderConfig::new(5006, payload.len() as u64, &target_bad_len);
    cfg2.part_size = part_size;
    let dl2 = StreamedFileDownloader::open(cfg2, bad_provider).unwrap();
    let err2 = dl2.download_all().await.unwrap_err();
    assert!(
        matches!(
            err2,
            StreamedDownloadError::ProviderFailed { .. }
                | StreamedDownloadError::InvalidChunkLength { .. }
        ),
        "expected failure on truncated chunk, got {err2:?}"
    );
    assert!(!target_bad_len.exists());
}
