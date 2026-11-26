//! TGB-3019: a correctly signed `.tupdate` whose ZIP payload holds a path-traversal, absolute or
//! symlink entry is refused by `unpack_verified_package`, which writes nothing outside
//! `tupdates/temp` and leaves no `ready` marker. The signature is still checked first: the
//! payloads below are re-signed with the test key, exactly as a stolen release key would allow.

use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use tg_updater::UpdateError;
use tg_updater::package::{
    compute_signing_input, pack_update, parse_envelope, unpack_verified_package, verify_package,
};
use tg_updater::version::{Target, UpdateChannel, Version};

const SIGNATURE_LEN: usize = 64;
const PAYLOAD_LEN_FIELD: usize = 4;

/// Signs `payload` as a valid `TFUP` package by taking the header of a regular package and
/// re-signing header + payload.
fn signed_package_with_payload(payload: &[u8], key: &SigningKey) -> Vec<u8> {
    let template = pack_update(
        UpdateChannel::Stable,
        Target::WIN_X64,
        &Version::parse("0.2.0").expect("version"),
        1_750_000_000,
        [("rustgram.exe", b"template".as_slice())],
        key,
    )
    .expect("template package");
    let template_payload_len = parse_envelope(&template)
        .expect("template envelope")
        .payload
        .len();
    let header_len = template.len() - template_payload_len - SIGNATURE_LEN - PAYLOAD_LEN_FIELD;
    let header = &template[..header_len];

    let signature = key.sign(&compute_signing_input(header, payload));
    let mut out = header.to_vec();
    out.extend_from_slice(&signature.to_bytes());
    out.extend_from_slice(&u32::try_from(payload.len()).expect("small").to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn zip_payload(build: impl FnOnce(&mut ZipWriter<&mut Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut cursor);
        writer
            .start_file("rustgram.exe", SimpleFileOptions::default())
            .expect("start");
        writer.write_all(b"new-binary").expect("write");
        build(&mut writer);
        writer.finish().expect("finish");
    }
    cursor.into_inner()
}

fn unpack(payload: &[u8], working_dir: &Path) -> Result<std::path::PathBuf, UpdateError> {
    let key = SigningKey::generate(&mut OsRng);
    let package = signed_package_with_payload(payload, &key);
    let verified = verify_package(
        &package,
        &key.verifying_key(),
        &Version::parse("0.1.0").expect("version"),
        false,
        Target::WIN_X64,
    )
    .expect("re-signed package verifies");
    unpack_verified_package(&verified, working_dir)
}

fn assert_refused_and_clean(result: Result<std::path::PathBuf, UpdateError>, root: &TempDir) {
    let err = result.expect_err("malicious entry must be refused");
    assert!(matches!(err, UpdateError::UnsafePath(_)), "got {err:?}");
    let temp = root.path().join("work").join("tupdates").join("temp");
    assert!(
        !temp.join("ready").exists(),
        "no ready marker after a refusal"
    );
    let entries: Vec<_> = fs::read_dir(root.path())
        .expect("read root")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(
        entries,
        vec!["work"],
        "nothing written beside the working dir"
    );
}

#[test]
fn template_resign_round_trips_a_benign_payload() {
    let root = TempDir::new().expect("tempdir");
    let working = root.path().join("work");
    let staged = unpack(&zip_payload(|_| {}), &working).expect("benign payload unpacks");
    assert_eq!(
        fs::read(staged.join("rustgram.exe")).expect("staged"),
        b"new-binary"
    );
    assert!(staged.join("ready").exists());
}

#[test]
fn parent_dir_traversal_entry_is_refused() {
    let root = TempDir::new().expect("tempdir");
    let payload = zip_payload(|w| {
        w.start_file("../../evil.dll", SimpleFileOptions::default())
            .expect("start");
        w.write_all(b"evil").expect("write");
    });
    assert_refused_and_clean(unpack(&payload, &root.path().join("work")), &root);
}

#[test]
fn absolute_path_entries_are_refused() {
    for name in ["/evil.dll", "C:/evil.dll", "\\\\server\\share\\evil.dll"] {
        let root = TempDir::new().expect("tempdir");
        let payload = zip_payload(|w| {
            w.start_file(name, SimpleFileOptions::default())
                .expect("start");
            w.write_all(b"evil").expect("write");
        });
        assert_refused_and_clean(unpack(&payload, &root.path().join("work")), &root);
    }
}

/// Fails on the code before TGB-3019: the symlink entry was written as a plain file holding the
/// link target and the update was marked ready.
#[test]
fn symlink_entry_is_refused() {
    let root = TempDir::new().expect("tempdir");
    let payload = zip_payload(|w| {
        w.add_symlink("Updater.exe", "../../outside", SimpleFileOptions::default())
            .expect("symlink");
    });
    assert_refused_and_clean(unpack(&payload, &root.path().join("work")), &root);
}
