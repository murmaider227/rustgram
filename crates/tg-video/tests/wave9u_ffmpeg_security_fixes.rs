//! TGB-3146: corrupt MP4 and AAC-USAC inputs that reach the FFmpeg code fixed upstream in
//! October / November 2026 (tdesktop 7.4.6 backports them as desktop-app/patches ffmpeg/0019-0024).
//!
//! - MOV demuxer, `mov_read_trun()` / `update_frag_index()` (FFmpeg 0260f625, 5647739d, 5e8f97c4):
//!   fragmented MP4 whose sample table lists more samples than chunks, fragments whose `sidx`
//!   references land before the current `moof` (so fragments are inserted ahead of the current
//!   one), and truncated `trun` boxes.
//! - AAC-USAC decoder (FFmpeg 694934cc, 7442df24, 1436f560): a stream that switches between two
//!   sample descriptions mid-stream, the second with a USAC element list that fails to parse part
//!   way, and SBR configurations fed garbage access units.
//!
//! Each input must end in an error or a clean EOF, never a crash or an endless loop. With the
//! unpatched FFmpeg these are heap / stack out-of-bounds accesses that do not reliably crash
//! without AddressSanitizer, so the proof that the shipped FFmpeg has the fixes is its build id
//! (see `linked_ffmpeg_reports_its_build_id`) checked by `.github/actions/setup-ffmpeg`.

use tg_video::{
    DecodedMediaFrame, FfmpegMedia, FfmpegOpenOptions, OutputPixelFormat, ffmpeg_build_id,
    ffmpeg_versions,
};

/// Deterministic xorshift64* generator, so every run feeds the same samples.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform value in `lo..=hi`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| (self.next() >> 56) as u8).collect()
    }
}

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8 + payload.len()).expect("test box fits in u32");
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = ((u32::from(version) << 24) | flags).to_be_bytes().to_vec();
    body.extend_from_slice(payload);
    mp4_box(kind, &body)
}

fn be32(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// MSB-first bit writer for AudioSpecificConfig / UsacConfig.
#[derive(Default)]
struct Bits {
    bits: Vec<bool>,
}

impl Bits {
    fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            self.bits.push((value >> i) & 1 == 1);
        }
    }

    /// ISO/IEC 23003-3 `escapedValue(n1, n2, n3)`.
    fn escaped(&mut self, value: u32, n1: u32, n2: u32, n3: u32) {
        let lim1 = (1 << n1) - 1;
        if value < lim1 {
            self.put(value, n1);
            return;
        }
        self.put(lim1, n1);
        let rest = value - lim1;
        let lim2 = (1 << n2) - 1;
        if rest < lim2 {
            self.put(rest, n2);
            return;
        }
        self.put(lim2, n2);
        self.put(rest - lim2, n3);
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bits
            .chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0u8, |acc, (i, &b)| acc | (u8::from(b) << (7 - i)))
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum UsacElement {
    Sce,
    Ext,
}

/// USAC AudioSpecificConfig (AOT 42, 48 kHz, mono). `sbr_index` is `coreSbrFrameLengthIndex`
/// (1 = no SBR, 3 = 2:1 SBR). The extension element at `fail_at` declares a config longer than
/// the descriptor, so the element list fails to parse at that element.
fn usac_asc(sbr_index: u32, elements: &[UsacElement], fail_at: Option<usize>) -> Vec<u8> {
    let mut b = Bits::default();
    b.put(31, 5);
    b.put(42 - 32, 6);
    b.put(3, 4); // samplingFrequencyIndex: 48 kHz
    b.put(1, 4); // channelConfiguration: mono
    b.put(3, 5); // usacSamplingFrequencyIndex
    b.put(sbr_index, 3);
    b.put(1, 5); // channelConfigurationIndex: mono
    let count = u32::try_from(elements.len()).expect("few elements");
    b.escaped(count - 1, 4, 8, 16);
    for (i, element) in elements.iter().enumerate() {
        match element {
            UsacElement::Sce => {
                b.put(0, 2);
                b.put(0, 1); // tw_mdct
                b.put(1, 1); // noiseFilling
                if sbr_index >= 2 {
                    b.put(0, 3); // harmonicSBR, bs_interTes, bs_pvc
                    b.put(5, 4); // dflt_start_freq
                    b.put(9, 4); // dflt_stop_freq
                    b.put(0, 2); // dflt_header_extra1/2
                }
            }
            UsacElement::Ext => {
                let failing = fail_at == Some(i);
                b.put(3, 2);
                b.escaped(if failing { 2 } else { 0 }, 4, 8, 16); // usacExtElementType
                b.escaped(if failing { 4000 } else { 0 }, 4, 8, 16); // config length
                b.put(0, 2); // default length present, payload frag
            }
        }
    }
    b.put(0, 1); // usacConfigExtensionPresent
    b.into_bytes()
}

fn esds(asc: &[u8]) -> Vec<u8> {
    let asc_len = u8::try_from(asc.len()).expect("short ASC");
    let mut dsi = vec![0x05, asc_len];
    dsi.extend_from_slice(asc);
    let mut dcd = vec![0x04, 13 + u8::try_from(dsi.len()).expect("short DSI")];
    dcd.extend_from_slice(&[0x40, 0x15, 0, 0, 0, 0, 1, 0xf4, 0, 0, 1, 0xf4, 0]);
    dcd.extend_from_slice(&dsi);
    let sl = [0x06, 0x01, 0x02];
    let mut es = vec![
        0x03,
        3 + u8::try_from(dcd.len() + sl.len()).expect("short ES"),
    ];
    es.extend_from_slice(&[0, 1, 0]);
    es.extend_from_slice(&dcd);
    es.extend_from_slice(&sl);
    full_box(b"esds", 0, 0, &es)
}

fn mp4a(asc: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 6];
    p.extend_from_slice(&1u16.to_be_bytes()); // data reference index
    p.extend_from_slice(&[0u8; 8]);
    p.extend_from_slice(&1u16.to_be_bytes()); // channels
    p.extend_from_slice(&16u16.to_be_bytes()); // sample size
    p.extend_from_slice(&[0u8; 4]);
    p.extend_from_slice(&(48_000u32 << 16).to_be_bytes());
    p.extend_from_slice(&esds(asc));
    mp4_box(b"mp4a", &p)
}

const IDENTITY_MATRIX: [u32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

/// `moov` with one fragmented audio track (id 1). The sample table declares `sample_count`
/// samples but only `chunk_count` one-sample chunks, so the index built from it is shorter than
/// the time-to-sample table.
fn moov(stsd_entries: &[Vec<u8>], sample_count: u32, chunk_count: u32, with_stts: bool) -> Vec<u8> {
    let mut stsd_body = be32(&[u32::try_from(stsd_entries.len()).expect("few entries")]);
    for entry in stsd_entries {
        stsd_body.extend_from_slice(entry);
    }
    let mut stbl = full_box(b"stsd", 0, 0, &stsd_body);
    if with_stts {
        stbl.extend(full_box(b"stts", 0, 0, &be32(&[1, sample_count, 1024])));
    }
    let stsc = if chunk_count > 0 {
        be32(&[1, 1, 1, 1])
    } else {
        be32(&[0])
    };
    stbl.extend(full_box(b"stsc", 0, 0, &stsc));
    stbl.extend(full_box(b"stsz", 0, 0, &be32(&[4, sample_count])));
    let mut stco = be32(&[chunk_count]);
    stco.extend(be32(&vec![0; chunk_count as usize]));
    stbl.extend(full_box(b"stco", 0, 0, &stco));

    let dref = full_box(
        b"dref",
        0,
        0,
        &[be32(&[1]), full_box(b"url ", 0, 1, &[])].concat(),
    );
    let minf = mp4_box(
        b"minf",
        &[
            full_box(b"smhd", 0, 0, &[0; 4]),
            mp4_box(b"dinf", &dref),
            mp4_box(b"stbl", &stbl),
        ]
        .concat(),
    );
    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(b"soun");
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.extend_from_slice(b"snd\0");
    let mut mdhd = be32(&[0, 0, 48_000, 0]);
    mdhd.extend_from_slice(&[0x55, 0xc4, 0, 0]);
    let mdia = mp4_box(
        b"mdia",
        &[
            full_box(b"mdhd", 0, 0, &mdhd),
            full_box(b"hdlr", 0, 0, &hdlr),
            minf,
        ]
        .concat(),
    );
    let mut tkhd = be32(&[0, 0, 1, 0, 0, 0, 0]);
    tkhd.extend_from_slice(&[0, 0, 0, 0, 0x01, 0x00, 0, 0]);
    tkhd.extend(be32(&IDENTITY_MATRIX));
    tkhd.extend(be32(&[0, 0]));
    let trak = mp4_box(b"trak", &[full_box(b"tkhd", 0, 7, &tkhd), mdia].concat());
    let mut mvhd = be32(&[0, 0, 1000, 0, 0x10000]);
    mvhd.extend_from_slice(&[0x01, 0x00]);
    mvhd.extend_from_slice(&[0u8; 10]);
    mvhd.extend(be32(&IDENTITY_MATRIX));
    mvhd.extend_from_slice(&[0u8; 24]);
    mvhd.extend(be32(&[2]));
    let trex = full_box(b"trex", 0, 0, &be32(&[1, 1, 1024, 4, 0]));
    mp4_box(
        b"moov",
        &[
            full_box(b"mvhd", 0, 0, &mvhd),
            trak,
            mp4_box(b"mvex", &trex),
        ]
        .concat(),
    )
}

/// `sidx` v1 whose 64-bit `first_offset` is negative, so its fragment references land before
/// the box itself (inserting fragment index items ahead of the current `moof`).
fn sidx_negative(back: u64, items: u16) -> Vec<u8> {
    let mut p = be32(&[1, 48_000]);
    p.extend_from_slice(&0u64.to_be_bytes());
    p.extend_from_slice(&back.wrapping_neg().to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&items.to_be_bytes());
    for _ in 0..items {
        p.extend(be32(&[1, 1024, 0x9000_0000]));
    }
    full_box(b"sidx", 1, 0, &p)
}

const TFHD_STSD_ID: u32 = 0x00_0002;
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;
const TRUN_DATA_OFFSET: u32 = 0x00_0001;
const TRUN_SAMPLE_DURATION: u32 = 0x00_0100;
const TRUN_SAMPLE_SIZE: u32 = 0x00_0200;

fn tfhd(stsd_id: u32) -> Vec<u8> {
    full_box(
        b"tfhd",
        0,
        TFHD_DEFAULT_BASE_IS_MOOF | TFHD_STSD_ID,
        &be32(&[1, stsd_id]),
    )
}

fn tfdt(time: u64) -> Vec<u8> {
    full_box(b"tfdt", 1, 0, &time.to_be_bytes())
}

/// One corrupt fragmented MP4 for the MOV `trun` / fragment index paths.
fn corrupt_fragmented_mp4(rng: &mut Rng) -> Vec<u8> {
    let aac_lc_48k_mono = [0x11, 0x90];
    let sample_count = [8, 64, 512, 4096][rng.range(0, 3) as usize];
    let chunk_count = rng.range(0, 2) as u32;
    let with_stts = rng.chance(80);
    let mut out = mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso6mp41");
    out.extend(moov(
        &[mp4a(&aac_lc_48k_mono)],
        sample_count,
        chunk_count,
        with_stts,
    ));
    if rng.chance(50) {
        let back = rng.range(1, out.len() as u64);
        out.extend(sidx_negative(back, rng.range(1, 4) as u16));
    }
    let fragments = rng.range(2, 6) as u32;
    for seq in 1..=fragments {
        let entries = rng.range(1, 16) as u32;
        let mut traf = Vec::new();
        if rng.chance(60) {
            let back = rng.range(1, out.len() as u64 + 64);
            traf.extend(sidx_negative(back, rng.range(1, 4) as u16));
        }
        traf.extend(tfhd(1));
        traf.extend(tfdt(rng.range(0, 1 << 20)));
        let mut trun_body = be32(&[entries]);
        for _ in 0..entries {
            trun_body.extend(be32(&[1024, 4]));
        }
        let mut trun = full_box(
            b"trun",
            0,
            TRUN_SAMPLE_DURATION | TRUN_SAMPLE_SIZE,
            &trun_body,
        );
        if seq == fragments && rng.chance(30) {
            trun.truncate(trun.len() - [4, 9][rng.range(0, 1) as usize]);
        }
        traf.extend(trun);
        out.extend(mp4_box(
            b"moof",
            &[
                full_box(b"mfhd", 0, 0, &be32(&[seq])),
                mp4_box(b"traf", &traf),
            ]
            .concat(),
        ));
        out.extend(mp4_box(b"mdat", &rng.bytes(4 * entries as usize)));
    }
    out
}

/// One fragmented MP4 with AAC-USAC audio: two sample descriptions, the first a plain USAC
/// config, the second one whose element list fails to parse; fragments alternate between them
/// so the decoder gets the broken config as new extradata mid-stream. Access units are garbage
/// behind an independency flag and the frequency-domain core mode.
fn corrupt_usac_mp4(rng: &mut Rng) -> Vec<u8> {
    let sbr_index = [1, 3][rng.range(0, 1) as usize];
    let good = usac_asc(sbr_index, &[UsacElement::Sce], None);
    let bad_count = rng.range(2, 6) as usize;
    let mut bad_list = vec![UsacElement::Sce];
    bad_list.resize(bad_count, UsacElement::Ext);
    let fail_at = rng.range(1, bad_count as u64 - 1) as usize;
    let bad = usac_asc(sbr_index, &bad_list, Some(fail_at));

    let mut out = mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso6mp41");
    out.extend(moov(&[mp4a(&good), mp4a(&bad)], 0, 0, false));
    for (seq, stsd_id) in [1u32, 2, 1, 2, 1].into_iter().enumerate() {
        let frames: Vec<Vec<u8>> = (0..rng.range(2, 8))
            .map(|_| {
                // usacIndependencyFlag = 1, core_mode = 0 (FD), then garbage.
                let mut frame = vec![0x80 | (rng.next() >> 58) as u8];
                let len = rng.range(3, 60) as usize;
                frame.extend(rng.bytes(len));
                frame
            })
            .collect();
        let mfhd = full_box(b"mfhd", 0, 0, &be32(&[seq as u32 + 1]));
        let traf_head = [tfhd(stsd_id), tfdt(seq as u64 * 8192)].concat();
        let sizes: Vec<u8> = frames
            .iter()
            .flat_map(|f| be32(&[1024, f.len() as u32]))
            .collect();
        let trun_len = 8 + 4 + 4 + 4 + sizes.len();
        let moof_len = 8 + mfhd.len() + 8 + traf_head.len() + trun_len;
        let mut trun_body = be32(&[frames.len() as u32, moof_len as u32 + 8]);
        trun_body.extend(sizes);
        let trun = full_box(
            b"trun",
            0,
            TRUN_DATA_OFFSET | TRUN_SAMPLE_DURATION | TRUN_SAMPLE_SIZE,
            &trun_body,
        );
        out.extend(mp4_box(
            b"moof",
            &[mfhd, mp4_box(b"traf", &[traf_head, trun].concat())].concat(),
        ));
        out.extend(mp4_box(b"mdat", &frames.concat()));
    }
    out
}

/// Opens and decodes `bytes` to the end. Returns how the decode ended and how many audio frames
/// came out; panics only if the decode loop does not terminate. A crash inside FFmpeg aborts the
/// whole test binary.
fn decode_to_end(bytes: Vec<u8>) -> (&'static str, u32) {
    let options = FfmpegOpenOptions {
        open_video: false,
        open_audio: true,
        thread_count: -1,
        ..FfmpegOpenOptions::default()
    };
    let Ok(mut media) = FfmpegMedia::open_bytes(bytes, options) else {
        return ("open error", 0);
    };
    let mut audio_frames = 0;
    for _ in 0..10_000 {
        match media.decode_next(false, true, OutputPixelFormat::Rgba, None, false, 48_000, 2) {
            Ok(Some(DecodedMediaFrame::Audio(_))) => audio_frames += 1,
            Ok(Some(DecodedMediaFrame::Video(_))) => {}
            Ok(None) => return ("eof", audio_frames),
            Err(_) => return ("decode error", audio_frames),
        }
    }
    panic!("decode loop did not terminate on a corrupt sample");
}

#[test]
fn linked_ffmpeg_reports_its_build_id() {
    let id = ffmpeg_build_id();
    let v = ffmpeg_versions();
    println!(
        "FFmpeg build {id}; libavformat {}.{}.{}, libavcodec {}.{}.{}",
        v.avformat >> 16,
        (v.avformat >> 8) & 0xff,
        v.avformat & 0xff,
        v.avcodec >> 16,
        (v.avcodec >> 8) & 0xff,
        v.avcodec & 0xff,
    );
    assert!(
        !id.is_empty(),
        "av_version_info() must name the FFmpeg build"
    );
}

#[test]
fn corrupt_fragmented_mp4_ends_in_error_not_crash() {
    let mut outcomes = std::collections::BTreeMap::<&str, u32>::new();
    for seed in 0..48 {
        let sample = corrupt_fragmented_mp4(&mut Rng::new(seed));
        *outcomes.entry(decode_to_end(sample).0).or_default() += 1;
    }
    println!("corrupt fragmented MP4 outcomes: {outcomes:?}");
    assert_eq!(outcomes.values().sum::<u32>(), 48);
}

#[test]
fn usac_config_switch_to_broken_element_list_ends_in_error_not_crash() {
    let mut outcomes = std::collections::BTreeMap::<&str, u32>::new();
    for seed in 0..48 {
        let sample = corrupt_usac_mp4(&mut Rng::new(1000 + seed));
        *outcomes.entry(decode_to_end(sample).0).or_default() += 1;
    }
    println!("corrupt USAC outcomes: {outcomes:?}");
    assert_eq!(outcomes.values().sum::<u32>(), 48);
}

#[test]
fn usac_config_with_failing_element_decodes_no_audio() {
    let bad = usac_asc(
        1,
        &[UsacElement::Sce, UsacElement::Ext, UsacElement::Ext],
        Some(1),
    );
    let mut out = mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso6mp41");
    out.extend(moov(&[mp4a(&bad)], 0, 0, false));
    let (outcome, audio_frames) = decode_to_end(out);
    println!("broken USAC config: {outcome}, {audio_frames} audio frames");
    assert_eq!(
        audio_frames, 0,
        "a USAC config that fails to parse must not decode audio"
    );
}
