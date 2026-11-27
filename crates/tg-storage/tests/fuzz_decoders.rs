//! Mutation fuzzing of the byte-level decoders that read local files (TGB-3060).
//!
//! Every decoder must return an error (or a value) for any input; none may panic. Inputs are
//! random byte strings plus seeds mutated with bit flips and "interesting" 32-bit values (lengths
//! and counts such as `0x7FFF_FFFF`) dropped at random offsets, which is where length-prefixed
//! readers usually break. The generator is a fixed-seed LCG so failures reproduce.

use std::panic::{AssertUnwindSafe, catch_unwind};

use tg_storage::qdatastream::{QDataStreamReader, QDataStreamWriter};
use tg_storage::serialize::deserialize_tags;
use tg_storage::serialize::document::SerializedDocument;
use tg_storage::serialize::image_location::{DownloadLocation, ImageLocation, StorageFileLocation};
use tg_storage::serialize::peer::{SerializedPeer, deserialize_self_user, peek_user_phone};
use tg_storage::session_settings::{StoredAutoDownload, StoredSessionSettings};
use tg_storage::settings_scheme::{ReadSettingsContext, read_all_settings, read_setting};
use tg_storage::streamed_downloader::PartialFileMetadata;
use tg_storage::tdf::decode_tdf_bytes;
use tg_storage::{AccountMap, AccountsInfo, MtpAuthorization};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

const INTERESTING: [u32; 10] = [
    0,
    1,
    0x7F,
    0xFF,
    0x8000,
    0xFFFF,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFE,
    0xFFFF_FFFF,
];

fn mutate(rng: &mut Lcg, seed: &[u8]) -> Vec<u8> {
    let mut out = seed.to_vec();
    for _ in 0..=rng.below(4) {
        match rng.below(5) {
            0 if !out.is_empty() => {
                let i = rng.below(out.len());
                out[i] ^= 1 << rng.below(8);
            }
            1 if out.len() >= 4 => {
                let i = rng.below(out.len() - 3);
                let v = INTERESTING[rng.below(INTERESTING.len())];
                out[i..i + 4].copy_from_slice(&v.to_be_bytes());
            }
            2 if !out.is_empty() => {
                let cut = rng.below(out.len());
                out.truncate(cut);
            }
            3 => {
                let i = rng.below(out.len() + 1);
                let v = INTERESTING[rng.below(INTERESTING.len())];
                out.splice(i..i, v.to_be_bytes());
            }
            _ => {
                let n = rng.below(16);
                for _ in 0..n {
                    out.push(rng.next() as u8);
                }
            }
        }
    }
    out
}

fn random_bytes(rng: &mut Lcg, max_len: usize) -> Vec<u8> {
    let len = rng.below(max_len);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        if rng.below(8) == 0 && out.len() + 4 <= len {
            out.extend_from_slice(&INTERESTING[rng.below(INTERESTING.len())].to_be_bytes());
        } else {
            out.push(rng.next() as u8);
        }
    }
    out
}

/// Runs `decode` on `iterations` inputs derived from `seeds`, failing with the first input that
/// panics.
fn fuzz(name: &str, seeds: &[Vec<u8>], iterations: usize, decode: impl Fn(&[u8])) {
    let mut rng = Lcg(0x5EED_0000 ^ name.len() as u64);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failure = None;
    for i in 0..iterations {
        let input = if seeds.is_empty() || i % 3 == 0 {
            random_bytes(&mut rng, 160)
        } else {
            mutate(&mut rng, &seeds[i % seeds.len()])
        };
        if catch_unwind(AssertUnwindSafe(|| decode(&input))).is_err() {
            failure = Some(input);
            break;
        }
    }
    std::panic::set_hook(previous_hook);
    if let Some(input) = failure {
        panic!("{name} panicked on input {input:02x?}");
    }
}

const VERSIONS: [i32; 4] = [0, 9_015, 2_000_000, 6_000_000];

#[test]
fn tdf_container_decoder_never_panics() {
    let seed = tg_storage::tdf::encode_tdf_bytes(6_000_000, b"payload").unwrap_or_default();
    fuzz("tdf", &[seed], 20_000, |bytes| {
        let _ = decode_tdf_bytes(bytes, 6_000_000);
    });
}

#[test]
fn settings_blocks_never_panic() {
    let mut rng = Lcg(42);
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failure = None;
    'outer: for block_id in 0..256u32 {
        for &version in &VERSIONS {
            for _ in 0..60 {
                let payload = random_bytes(&mut rng, 96);
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let mut context = ReadSettingsContext::default();
                    let mut stream = QDataStreamReader::new(&payload);
                    let _ = read_setting(block_id, &mut stream, version, &mut context);
                }));
                if result.is_err() {
                    failure = Some((block_id, version, payload));
                    break 'outer;
                }
            }
        }
    }
    std::panic::set_hook(previous_hook);
    if let Some((block_id, version, payload)) = failure {
        panic!("read_setting({block_id}, v{version}) panicked on {payload:02x?}");
    }
    fuzz("read_all_settings", &[], 20_000, |bytes| {
        let mut context = ReadSettingsContext::default();
        let _ = read_all_settings(bytes, 2_000_000, &mut context);
    });
}

#[test]
fn peer_and_document_readers_never_panic() {
    let mut w = QDataStreamWriter::new();
    w.write_u64(7);
    w.write_u64(u64::MAX);
    w.write_qstring("first");
    w.write_qstring("last");
    w.write_qstring("");
    w.write_qstring("user");
    w.write_qstring("");
    let seed = w.into_bytes();
    for &version in &VERSIONS {
        fuzz("read_peer", std::slice::from_ref(&seed), 6_000, |bytes| {
            let mut r = QDataStreamReader::new(bytes);
            let _ = SerializedPeer::read_from_stream(version, &mut r);
        });
        fuzz("self_user", std::slice::from_ref(&seed), 3_000, |bytes| {
            let _ = deserialize_self_user(version, bytes);
            let _ = peek_user_phone(version, bytes);
        });
        fuzz("read_document", &[], 6_000, |bytes| {
            let mut r = QDataStreamReader::new(bytes);
            let _ = SerializedDocument::read_from_stream(version, &mut r);
        });
        fuzz("read_image_location", &[], 6_000, |bytes| {
            let mut r = QDataStreamReader::new(bytes);
            let _ = ImageLocation::read_from_stream(version, &mut r);
        });
    }
}

#[test]
fn location_decoders_never_panic() {
    fuzz("storage_file_location", &[], 20_000, |bytes| {
        let _ = StorageFileLocation::deserialize(bytes);
    });
    fuzz("download_location", &[], 20_000, |bytes| {
        let _ = DownloadLocation::deserialize(bytes);
    });
    fuzz("image_location", &[], 20_000, |bytes| {
        let _ = ImageLocation::deserialize(bytes);
    });
    fuzz("tags", &[], 20_000, |bytes| {
        let _ = deserialize_tags(bytes, 64);
    });
}

#[test]
fn account_and_session_decoders_never_panic() {
    let map_seed = AccountMap::default().serialize();
    fuzz("account_map", &[map_seed], 20_000, |bytes| {
        let _ = AccountMap::deserialize(bytes);
    });
    fuzz("accounts_info", &[], 20_000, |bytes| {
        let _ = AccountsInfo::deserialize(bytes);
    });
    fuzz("mtp_authorization", &[], 20_000, |bytes| {
        let _ = MtpAuthorization::deserialize(bytes);
    });
    let session_seed = StoredSessionSettings::default().serialize();
    fuzz("session_settings", &[session_seed], 20_000, |bytes| {
        let _ = StoredSessionSettings::deserialize(bytes);
    });
    fuzz("auto_download", &[], 20_000, |bytes| {
        let _ = StoredAutoDownload::deserialize(bytes);
    });
    fuzz("partial_download_meta", &[], 20_000, |bytes| {
        let _ = PartialFileMetadata::decode(bytes);
    });
}
