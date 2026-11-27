//! Wave 6 (TGB-3064): bugs found in `tg-media` and fuzz-style tests for parsers of untrusted bytes.

use tg_media::cache::{CacheSettings, IMAGE_CACHE_TAG, LocalFileCache, StorageKey};
use tg_media::image::inline::{expand_inline_bytes, expand_path_inline_bytes, parse_svg_path};
use tg_media::image::jpeg::{
    jpeg_frame_info, parse_exif_app1_orientation, parse_tiff_orientation,
    read_jpeg_exif_orientation,
};

/// Deterministic xorshift generator, so failures reproduce without a seed file.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % (n as u64)) as usize
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

/// Mutations of `seed`: truncations, single-byte flips, random spans, and pure noise.
fn mutations(seed: &[u8], rng: &mut Rng, count: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::with_capacity(count + seed.len());
    for cut in 0..seed.len() {
        out.push(seed[..cut].to_vec());
    }
    for _ in 0..count {
        let mut m = seed.to_vec();
        match rng.below(4) {
            0 if !m.is_empty() => {
                let i = rng.below(m.len());
                m[i] = rng.next() as u8;
            }
            1 if !m.is_empty() => {
                let i = rng.below(m.len());
                for b in m.iter_mut().skip(i).take(8) {
                    *b = 0xFF;
                }
            }
            2 => {
                let len = rng.below(64);
                m = rng.bytes(len);
            }
            _ => {
                let at = rng.below(m.len() + 1);
                let len = rng.below(16);
                let extra = rng.bytes(len);
                m.splice(at..at, extra);
            }
        }
        out.push(m);
    }
    out
}

#[test]
fn storage_key_from_hex_rejects_non_ascii_without_panicking() {
    // 15 ASCII digits, then a 2-byte character across byte 16: 32 bytes, 31 chars.
    let stem = format!("{}é{}", "0".repeat(15), "1".repeat(15));
    assert_eq!(stem.len(), 32);
    assert_eq!(StorageKey::from_hex(&stem), None);
    // `from_str_radix` accepts a sign; a cache file name never has one.
    assert_eq!(
        StorageKey::from_hex("+0000000000000010000000000000001"),
        None
    );
    let key = StorageKey::new(0x0102, 0x0A0B_0C0D_0E0F_1011);
    assert_eq!(StorageKey::from_hex(&key.to_hex()), Some(key));
    assert_eq!(
        StorageKey::from_hex(&key.to_hex().to_uppercase()),
        Some(key)
    );
}

#[test]
fn cache_open_skips_stray_and_unreadable_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let good = StorageKey::new(7, 9);
    {
        let mut cache = LocalFileCache::open(CacheSettings::new(dir.path())).expect("open");
        cache.put(good, IMAGE_CACHE_TAG, b"payload").expect("put");
    }
    // A stray file whose 32-byte stem is not ASCII.
    let stray = format!("{}é{}.bin", "0".repeat(15), "1".repeat(15));
    std::fs::write(dir.path().join(stray), b"\x01junk").expect("write stray");
    // A directory with a valid key name: it cannot be opened as a file.
    let dir_key = StorageKey::new(1, 2);
    std::fs::create_dir(dir.path().join(format!("{}.bin", dir_key.to_hex()))).expect("mkdir");

    let mut cache = LocalFileCache::open(CacheSettings::new(dir.path())).expect("reopen");
    assert_eq!(cache.get(good).expect("get"), Some(b"payload".to_vec()));
    assert!(!cache.contains(dir_key));
}

#[test]
fn cache_get_returns_payload_without_tag_byte() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cache = LocalFileCache::open(CacheSettings::new(dir.path())).expect("open");
    let key = StorageKey::new(3, 4);
    let payload: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
    cache.put(key, IMAGE_CACHE_TAG, &payload).expect("put");
    assert_eq!(cache.get(key).expect("get"), Some(payload));
    cache.put(key, IMAGE_CACHE_TAG, &[]).expect("put empty");
    assert_eq!(cache.get(key).expect("get empty"), Some(Vec::new()));
}

fn sample_jpeg() -> Vec<u8> {
    let tiff_le: &[u8] = &[
        b'I', b'I', 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, // header, IFD0 at 8
        0x01, 0x00, // one entry
        0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00,
        0x00, // orientation 6
        0x00, 0x00, 0x00, 0x00, // next IFD
    ];
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(tiff_le);
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
    jpeg.extend_from_slice(&u16::try_from(app1.len() + 2).expect("len").to_be_bytes());
    jpeg.extend_from_slice(&app1);
    // SOF0: length 11, precision 8, 16x32, 3 components (truncated component table is fine).
    jpeg.extend_from_slice(&[
        0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x20, 0x00, 0x10, 0x03, 0, 0, 0,
    ]);
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    jpeg
}

#[test]
fn jpeg_parsers_survive_mutated_input() {
    let seed = sample_jpeg();
    let info = jpeg_frame_info(&seed).expect("sample parses");
    assert_eq!((info.width, info.height, info.components), (16, 32, 3));
    assert_eq!(read_jpeg_exif_orientation(&seed) as u16, 6);

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for input in mutations(&seed, &mut rng, 4000) {
        let _ = jpeg_frame_info(&input);
        let _ = read_jpeg_exif_orientation(&input);
        let _ = parse_exif_app1_orientation(&input);
        let _ = parse_tiff_orientation(&input);
        if input.len() > 12 {
            let _ = parse_tiff_orientation(&input[12..]);
        }
    }
}

#[test]
fn stripped_thumbnail_and_path_parsers_survive_mutated_input() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let thumb_seed = [0x01, 0x28, 0x28, 0x00, 0x11, 0x22, 0x33, 0xFF, 0xD9];
    for input in mutations(&thumb_seed, &mut rng, 2000) {
        if let Some(expanded) = expand_inline_bytes(&input) {
            assert!(expanded.len() > input.len());
        }
        // The full decode goes through `image`; it must fail cleanly, never panic.
        let _ = tg_media::image::inline::from_inline_bytes(&input);
    }
    let path_seed = [0xC0, 0x41, 0x42, 0x10, 0x20, 0xC5, 0x30, 0x7A, 0xFF, 0x00];
    for input in mutations(&path_seed, &mut rng, 4000) {
        let expanded = expand_path_inline_bytes(&input);
        let _ = parse_svg_path(&expanded);
    }
}
