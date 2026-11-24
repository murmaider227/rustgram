//! MTProxy fake-TLS (`0xEE` secret) ClientHello generation, ServerHello HMAC verification,
//! and TLS 1.3 Application Data record framing.
//!
//! Port of `mtproto/details/mtproto_tls_socket.h/.cpp`.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use rand::RngCore;
use rand::seq::SliceRandom;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::error::Error;

/// Number of GREASE bytes generated per ClientHello (`kMaxGrease`).
pub const MAX_GREASE: usize = 8;

/// Maximum generated ClientHello size in bytes (`kClientHelloLimit`).
pub const CLIENT_HELLO_LIMIT: usize = 2048;

/// Size of the ClientHello / ServerHello random digest (`kHelloDigestLength`).
pub const HELLO_DIGEST_LENGTH: usize = 32;

/// Maximum accepted ServerHello total size (`kMaxServerHelloLength`).
pub const MAX_SERVER_HELLO_LENGTH: usize = 65536;

/// Byte offset of the 32-byte ServerHello random digest (`kServerHelloDigestPosition`).
pub const SERVER_HELLO_DIGEST_POSITION: usize = 11;

/// Maximum application-data payload per outgoing TLS record (`kClientPartSize`).
pub const CLIENT_PART_SIZE: usize = 2878;

/// Expected 3-byte prefix of ServerHello record 1 (`kServerHelloPart1`).
pub const SERVER_HELLO_PART1: &[u8; 3] = b"\x16\x03\x03";

/// Expected 9-byte ChangeCipherSpec + ApplicationData header in ServerHello (`kServerHelloPart3`).
pub const SERVER_HELLO_PART3: &[u8; 9] = b"\x14\x03\x03\x00\x01\x01\x17\x03\x03";

/// Expected 3-byte header of incoming Application Data records (`kServerHeader`).
pub const SERVER_HEADER: &[u8; 3] = b"\x17\x03\x03";

/// 6-byte ChangeCipherSpec prefix sent once before the first outgoing Application Data record
/// (`kClientPrefix`).
pub const CLIENT_PREFIX: &[u8; 6] = b"\x14\x03\x03\x00\x01\x01";

/// 3-byte header of outgoing Application Data records (`kClientHeader`).
pub const CLIENT_HEADER: &[u8; 3] = b"\x17\x03\x03";

/// Template block matching `MTPTlsBlock` from `mtproto/scheme/mtproto.tl`.
#[derive(Debug, Clone)]
pub enum TlsBlock {
    /// Fixed literal byte sequence (`tlsBlockString`).
    String(&'static [u8]),
    /// `length` zero bytes (`tlsBlockZero`). The first 32-byte zero block marks the digest slot.
    Zero(usize),
    /// 2-byte GREASE value selected from `greases[seed]` repeated twice (`tlsBlockGrease`).
    Grease(usize),
    /// `length` cryptographically random bytes (`tlsBlockRandom`).
    Random(usize),
    /// SNI domain bytes from the `0xEE` secret (`tlsBlockDomain`).
    Domain,
    /// Fresh 32-byte X25519 public key (`tlsBlockPublicKey`).
    PublicKey,
    /// Length-prefixed (2-byte big-endian) nested block sequence (`tlsBlockScope`).
    Scope(Vec<TlsBlock>),
    /// Randomly shuffled list of nested block sequences (`tlsBlockPermutation`).
    Permutation(Vec<Vec<TlsBlock>>),
    /// ML-KEM-768 encapsulation key share bytes (`tlsBlockM`: 384 * 3 + 32 = 1184 bytes).
    MlKem,
    /// Encrypted ClientHello (ECH) random payload (`tlsBlockE`: 144, 176, 208, or 240 bytes).
    EchPayload,
    /// Padding extension (`0x0015`) bringing ClientHello to at least 517 bytes if < 513 (`tlsBlockPadding`).
    Padding,
}

/// Generated fake-TLS ClientHello bytes and the 32-byte ClientHello random digest
/// (which is needed to verify the ServerHello HMAC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHello {
    /// Complete TLS ClientHello record bytes ready to write on the wire.
    pub data: Vec<u8>,
    /// The 32-byte digest written at offset 11..43 (after timestamp XOR).
    pub digest: [u8; HELLO_DIGEST_LENGTH],
}

/// Build the default fake-TLS ClientHello template matching `PrepareClientHelloRules()`
/// in `mtproto/details/mtproto_tls_socket.cpp`.
#[must_use]
pub fn default_client_hello_rules() -> Vec<TlsBlock> {
    use TlsBlock::*;

    let extensions_permutation: Vec<Vec<TlsBlock>> = vec![
        // 1. SNI (0x0000)
        vec![
            String(b"\x00\x00"),
            Scope(vec![Scope(vec![String(b"\x00"), Scope(vec![Domain])])]),
        ],
        // 2. status_request (0x0005)
        vec![String(b"\x00\x05\x00\x05\x01\x00\x00\x00\x00")],
        // 3. supported_groups (0x000a) with GREASE + X25519MLKEM768 + X25519 + secp256r1 + secp384r1
        vec![
            String(b"\x00\x0a\x00\x0c\x00\x0a"),
            Grease(4),
            String(b"\x11\xec\x00\x1d\x00\x17\x00\x18"),
        ],
        // 4. ec_point_formats (0x000b)
        vec![String(b"\x00\x0b\x00\x02\x01\x00")],
        // 5. signature_algorithms (0x000d)
        vec![String(
            b"\x00\x0d\x00\x18\x00\x16\x09\x04\x09\x05\x09\x06\x04\x03\x08\x04\x04\x01\x05\x03\x08\x05\x05\x01\x08\x06\x06\x01",
        )],
        // 6. ALPN (0x0010): h2, http/1.1
        vec![String(
            b"\x00\x10\x00\x0e\x00\x0c\x02\x68\x32\x08\x68\x74\x74\x70\x2f\x31\x2e\x31",
        )],
        // 7. signed_certificate_timestamp (0x0012)
        vec![String(b"\x00\x12\x00\x00")],
        // 8. extended_master_secret (0x0017)
        vec![String(b"\x00\x17\x00\x00")],
        // 9. compress_certificate (0x001b)
        vec![String(b"\x00\x1b\x00\x03\x02\x00\x02")],
        // 10. session_ticket (0x0023)
        vec![String(b"\x00\x23\x00\x00")],
        // 11. supported_versions (0x002b): GREASE, TLS 1.3, TLS 1.2
        vec![
            String(b"\x00\x2b\x00\x07\x06"),
            Grease(6),
            String(b"\x03\x04\x03\x03"),
        ],
        // 12. psk_key_exchange_modes (0x002d)
        vec![String(b"\x00\x2d\x00\x02\x01\x01")],
        // 13. key_share (0x0033): GREASE + X25519MLKEM768 + X25519
        vec![
            String(b"\x00\x33\x04\xef\x04\xed"),
            Grease(4),
            String(b"\x00\x01\x00\x11\xec\x04\xc0"),
            MlKem,
            PublicKey,
            String(b"\x00\x1d\x00\x20"),
            PublicKey,
        ],
        // 14. application_settings / ALPS (0x44cd)
        vec![String(b"\x44\xcd\x00\x05\x00\x03\x02\x68\x32")],
        // 15. encrypted_client_hello / ECH (0xfe0d)
        vec![
            String(b"\xfe\x0d"),
            Scope(vec![
                String(b"\x00\x00\x01\x00\x01"),
                Random(1),
                String(b"\x00\x20"),
                PublicKey,
                Scope(vec![EchPayload]),
            ]),
        ],
        // 16. renegotiation_info (0xff01)
        vec![String(b"\xff\x01\x00\x01\x00")],
    ];

    vec![
        String(b"\x16\x03\x01"),
        Scope(vec![
            String(b"\x01\x00"),
            Scope(vec![
                String(b"\x03\x03"),
                Zero(32),
                String(b"\x20"),
                Random(32),
                String(b"\x00\x20"),
                Grease(0),
                String(
                    b"\x13\x01\x13\x02\x13\x03\xc0\x2b\xc0\x2f\xc0\x2c\xc0\x30\xcc\xa9\xcc\xa8\xc0\x13\xc0\x14\x00\x9c\x00\x9d\x00\x2f\x00\x35\x01\x00",
                ),
                Scope(vec![
                    Grease(2),
                    String(b"\x00\x00"),
                    Permutation(extensions_permutation),
                    Grease(3),
                    String(b"\x00\x01\x00"),
                    Padding,
                ]),
            ]),
        ]),
    ]
}

/// Generate 8 GREASE bytes following `PrepareGreases()` in `mtproto_tls_socket.cpp`.
///
/// Each byte has the form `(x & 0xF0) + 0x0A`, and adjacent pairs `(2i, 2i+1)` are
/// guaranteed distinct.
#[must_use]
pub fn prepare_greases<R: RngCore>(rng: &mut R) -> [u8; MAX_GREASE] {
    let mut result = [0u8; MAX_GREASE];
    rng.fill_bytes(&mut result);
    for b in &mut result {
        *b = (*b & 0xF0) + 0x0A;
    }
    for pair in result.as_chunks_mut::<2>().0 {
        if pair[0] == pair[1] {
            pair[1] ^= 0x10;
        }
    }
    result
}

/// Generate a valid 32-byte X25519 public key (`GeneratePublicKey()`).
#[must_use]
pub fn generate_x25519_public_key<R: RngCore + rand::CryptoRng>(rng: &mut R) -> [u8; 32] {
    let secret = StaticSecret::random_from_rng(rng);
    PublicKey::from(&secret).to_bytes()
}

/// Generate a fake-TLS ClientHello for `domain` and 16-byte `key` using the current UTC time.
pub fn generate_client_hello(domain: &[u8], key: &[u8; 16]) -> Result<ClientHello, Error> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i32;
    let rules = default_client_hello_rules();
    let mut rng = rand::thread_rng();
    generate_client_hello_with_rules(&rules, domain, key, now, &mut rng)
}

/// Generate a fake-TLS ClientHello using explicit `rules`, `timestamp_secs`, and `rng`.
pub fn generate_client_hello_with_rules<R: RngCore + rand::CryptoRng>(
    rules: &[TlsBlock],
    domain: &[u8],
    key: &[u8; 16],
    timestamp_secs: i32,
    rng: &mut R,
) -> Result<ClientHello, Error> {
    let greases = prepare_greases(rng);
    let mut part = HelloPart::new(domain, &greases);
    part.write_blocks(rules, rng)?;
    part.finalize(key, timestamp_secs)
}

struct HelloPart<'a> {
    domain: &'a [u8],
    greases: &'a [u8; MAX_GREASE],
    result: Vec<u8>,
    digest_position: Option<usize>,
}

impl<'a> HelloPart<'a> {
    fn new(domain: &'a [u8], greases: &'a [u8; MAX_GREASE]) -> Self {
        Self {
            domain,
            greases,
            result: Vec::with_capacity(CLIENT_HELLO_LIMIT),
            digest_position: None,
        }
    }

    fn ensure_capacity(&self, additional: usize) -> Result<(), Error> {
        if additional == 0 || self.result.len().saturating_add(additional) > CLIENT_HELLO_LIMIT {
            return Err(Error::Tls(format!(
                "ClientHello size limit exceeded (current {}, adding {})",
                self.result.len(),
                additional
            )));
        }
        Ok(())
    }

    fn write_blocks<R: RngCore + rand::CryptoRng>(
        &mut self,
        blocks: &[TlsBlock],
        rng: &mut R,
    ) -> Result<(), Error> {
        for block in blocks {
            self.write_block(block, rng)?;
        }
        Ok(())
    }

    fn write_block<R: RngCore + rand::CryptoRng>(
        &mut self,
        block: &TlsBlock,
        rng: &mut R,
    ) -> Result<(), Error> {
        match block {
            TlsBlock::String(bytes) => {
                self.ensure_capacity(bytes.len())?;
                self.result.extend_from_slice(bytes);
            }
            TlsBlock::Zero(length) => {
                let already = self.result.len();
                self.ensure_capacity(*length)?;
                if *length == HELLO_DIGEST_LENGTH && self.digest_position.is_none() {
                    self.digest_position = Some(already);
                }
                self.result.resize(already + *length, 0);
            }
            TlsBlock::Grease(seed) => {
                let &g = self
                    .greases
                    .get(*seed)
                    .ok_or_else(|| Error::Tls(format!("invalid GREASE seed {seed}")))?;
                self.ensure_capacity(2)?;
                self.result.push(g);
                self.result.push(g);
            }
            TlsBlock::Random(length) => {
                let start = self.result.len();
                self.ensure_capacity(*length)?;
                self.result.resize(start + *length, 0);
                rng.fill_bytes(&mut self.result[start..]);
            }
            TlsBlock::Domain => {
                self.ensure_capacity(self.domain.len())?;
                self.result.extend_from_slice(self.domain);
            }
            TlsBlock::PublicKey => {
                let key = generate_x25519_public_key(rng);
                self.ensure_capacity(key.len())?;
                self.result.extend_from_slice(&key);
            }
            TlsBlock::Scope(entries) => {
                let len_offset = self.result.len();
                self.ensure_capacity(2)?;
                self.result.extend_from_slice(&[0, 0]);
                let content_start = self.result.len();
                self.write_blocks(entries, rng)?;
                let content_len = self.result.len() - content_start;
                let len_u16 = u16::try_from(content_len)
                    .map_err(|_| Error::Tls("TLS scope length exceeds u16".into()))?;
                self.result[len_offset..len_offset + 2].copy_from_slice(&len_u16.to_be_bytes());
            }
            TlsBlock::Permutation(entries) => {
                let mut rendered = Vec::with_capacity(entries.len());
                for inner in entries {
                    let mut sub = HelloPart::new(self.domain, self.greases);
                    sub.write_blocks(inner, rng)?;
                    rendered.push(sub.result);
                }
                rendered.shuffle(rng);
                for item in rendered {
                    self.ensure_capacity(item.len())?;
                    self.result.extend_from_slice(&item);
                }
            }
            TlsBlock::MlKem => {
                // Port of Generator::Part::writeBlock(MTPDtlsBlockM):
                // 384 pairs of mod-3329 12-bit integers packed into 3 bytes each + 32 random bytes.
                const ELEMENTS: usize = 384;
                const ADDED: usize = 32;
                let total = ELEMENTS * 3 + ADDED;
                let start = self.result.len();
                self.ensure_capacity(total)?;
                let mut rand_buf = vec![0u8; ELEMENTS * 8];
                rng.fill_bytes(&mut rand_buf);
                for chunk in rand_buf.as_chunks::<8>().0 {
                    let a = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) % 3329;
                    let b = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) % 3329;
                    self.result.push((a & 0xFF) as u8);
                    self.result.push(((a >> 8) + ((b & 0x0F) << 4)) as u8);
                    self.result.push((b >> 4) as u8);
                }
                self.result.resize(start + total, 0);
                rng.fill_bytes(&mut self.result[start + ELEMENTS * 3..]);
            }
            TlsBlock::EchPayload => {
                const LENGTHS: [usize; 4] = [144, 176, 208, 240];
                let idx = (rng.next_u32() as usize) % LENGTHS.len();
                self.write_block(&TlsBlock::Random(LENGTHS[idx]), rng)?;
            }
            TlsBlock::Padding => {
                let length = self.result.len();
                if length < 513 {
                    let pad = 513 - length;
                    self.write_block(&TlsBlock::String(b"\x00\x15"), rng)?;
                    self.write_block(&TlsBlock::Scope(vec![TlsBlock::Zero(pad)]), rng)?;
                }
            }
        }
        Ok(())
    }

    fn finalize(mut self, key: &[u8; 16], timestamp_secs: i32) -> Result<ClientHello, Error> {
        let digest_pos = self
            .digest_position
            .ok_or_else(|| Error::Tls("ClientHello template has no 32-byte digest slot".into()))?;

        // HMAC-SHA256 over the entire ClientHello with the 32-byte digest slot zeroed.
        let mut mac =
            Hmac::<Sha256>::new_from_slice(key).expect("HMAC-SHA256 accepts 16-byte keys");
        mac.update(&self.result);
        let hmac_bytes = mac.finalize().into_bytes();
        self.result[digest_pos..digest_pos + HELLO_DIGEST_LENGTH].copy_from_slice(&hmac_bytes);

        // XOR the last 4 bytes of the digest with little-endian unix timestamp.
        let ts_offset = digest_pos + HELLO_DIGEST_LENGTH - 4;
        let current = i32::from_le_bytes([
            self.result[ts_offset],
            self.result[ts_offset + 1],
            self.result[ts_offset + 2],
            self.result[ts_offset + 3],
        ]);
        let mixed = current ^ timestamp_secs;
        self.result[ts_offset..ts_offset + 4].copy_from_slice(&mixed.to_le_bytes());

        let mut digest = [0u8; HELLO_DIGEST_LENGTH];
        digest.copy_from_slice(&self.result[digest_pos..digest_pos + HELLO_DIGEST_LENGTH]);

        Ok(ClientHello {
            data: self.result,
            digest,
        })
    }
}

/// Verify a complete ServerHello message against the client's 32-byte digest and 16-byte secret key.
///
/// Port of `TlsSocket::checkHelloParts12`, `checkHelloParts34`, and `checkHelloDigest`.
pub fn verify_server_hello(
    server_hello: &[u8],
    client_digest: &[u8; HELLO_DIGEST_LENGTH],
    key: &[u8; 16],
) -> Result<(), Error> {
    let parts1_size = SERVER_HELLO_PART1.len() + 2;
    if server_hello.len() < parts1_size || server_hello.len() > MAX_SERVER_HELLO_LENGTH {
        return Err(Error::Tls("bad ServerHello length".into()));
    }
    if &server_hello[..SERVER_HELLO_PART1.len()] != SERVER_HELLO_PART1 {
        return Err(Error::Tls("bad ServerHello part 1 header".into()));
    }
    let part2_size = u16::from_be_bytes([server_hello[3], server_hello[4]]) as usize;
    let parts123_size = parts1_size + part2_size + SERVER_HELLO_PART3.len() + 2;
    if parts123_size > server_hello.len() || parts123_size > MAX_SERVER_HELLO_LENGTH {
        return Err(Error::Tls("bad ServerHello part 2/3 size".into()));
    }
    let part3_start = parts1_size + part2_size;
    if &server_hello[part3_start..part3_start + SERVER_HELLO_PART3.len()] != SERVER_HELLO_PART3 {
        return Err(Error::Tls("bad ServerHello part 3 header".into()));
    }
    let part4_len_offset = part3_start + SERVER_HELLO_PART3.len();
    let part4_size = u16::from_be_bytes([
        server_hello[part4_len_offset],
        server_hello[part4_len_offset + 1],
    ]) as usize;
    let full_size = parts123_size + part4_size;
    if full_size != server_hello.len() || full_size > MAX_SERVER_HELLO_LENGTH {
        return Err(Error::Tls("bad ServerHello total size".into()));
    }
    if full_size < SERVER_HELLO_DIGEST_POSITION + HELLO_DIGEST_LENGTH {
        return Err(Error::Tls("ServerHello too short for digest".into()));
    }

    let server_digest = &server_hello
        [SERVER_HELLO_DIGEST_POSITION..SERVER_HELLO_DIGEST_POSITION + HELLO_DIGEST_LENGTH];
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC-SHA256 accepts 16-byte keys");
    mac.update(client_digest);
    mac.update(&server_hello[..SERVER_HELLO_DIGEST_POSITION]);
    mac.update(&[0u8; HELLO_DIGEST_LENGTH]);
    mac.update(&server_hello[SERVER_HELLO_DIGEST_POSITION + HELLO_DIGEST_LENGTH..]);
    let expected = mac.finalize().into_bytes();

    if bool::from(server_digest.ct_eq(&expected)) {
        Ok(())
    } else {
        Err(Error::Tls("ServerHello HMAC digest mismatch".into()))
    }
}

/// Async fake-TLS stream wrapping an underlying byte stream (`TlsSocket` port).
///
/// Performs the fake-TLS ClientHello/ServerHello handshake during [`Self::connect`],
/// then frames outgoing writes into TLS Application Data records (<= 2878 bytes each,
/// with ChangeCipherSpec prepended before the first write) and unframes incoming records.
pub struct TlsStream<S> {
    inner: S,
    sent_client_prefix: bool,
    read_buf: Vec<u8>,
    read_pos: usize,
    pending_record_remaining: usize,
}

impl<S: AsyncRead + AsyncWrite + Unpin> TlsStream<S> {
    /// Perform the fake-TLS handshake over `inner` using `domain` and 16-byte `key`.
    pub async fn connect(mut inner: S, domain: &[u8], key: &[u8; 16]) -> Result<Self, Error> {
        let hello = generate_client_hello(domain, key)?;
        inner.write_all(&hello.data).await?;
        inner.flush().await?;

        // Read ServerHello header (part 1: 3 bytes + 2-byte length).
        let mut sh = Vec::with_capacity(256);
        let parts1_size = SERVER_HELLO_PART1.len() + 2;
        read_exact_append(&mut inner, &mut sh, parts1_size).await?;
        if &sh[..SERVER_HELLO_PART1.len()] != SERVER_HELLO_PART1 {
            return Err(Error::Tls("bad ServerHello part 1 header".into()));
        }
        let part2_size = u16::from_be_bytes([sh[3], sh[4]]) as usize;
        let parts123_size = parts1_size + part2_size + SERVER_HELLO_PART3.len() + 2;
        if parts123_size > MAX_SERVER_HELLO_LENGTH {
            return Err(Error::Tls("ServerHello parts 1..3 exceed limit".into()));
        }
        read_exact_append(&mut inner, &mut sh, parts123_size - parts1_size).await?;

        let part3_start = parts1_size + part2_size;
        if &sh[part3_start..part3_start + SERVER_HELLO_PART3.len()] != SERVER_HELLO_PART3 {
            return Err(Error::Tls("bad ServerHello part 3 header".into()));
        }
        let part4_len_offset = part3_start + SERVER_HELLO_PART3.len();
        let part4_size =
            u16::from_be_bytes([sh[part4_len_offset], sh[part4_len_offset + 1]]) as usize;
        let full_size = parts123_size + part4_size;
        if full_size > MAX_SERVER_HELLO_LENGTH {
            return Err(Error::Tls("ServerHello total size exceeds limit".into()));
        }
        read_exact_append(&mut inner, &mut sh, part4_size).await?;

        verify_server_hello(&sh, &hello.digest, key)?;

        Ok(Self {
            inner,
            sent_client_prefix: false,
            read_buf: Vec::new(),
            read_pos: 0,
            pending_record_remaining: 0,
        })
    }

    /// Write `prefix` (e.g. the 64-byte obfuscated2 header) followed by `data`, framed into
    /// fake-TLS Application Data records of at most [`CLIENT_PART_SIZE`] (2878) bytes.
    ///
    /// Port of `TlsSocket::write` (`mtproto/details/mtproto_tls_socket.cpp:902-929`).
    pub async fn write_framed(&mut self, mut prefix: &[u8], mut data: &[u8]) -> Result<(), Error> {
        if data.is_empty() && prefix.is_empty() {
            return Ok(());
        }
        let mut wire = Vec::with_capacity(
            prefix.len() + data.len() + 16 + ((prefix.len() + data.len()) / CLIENT_PART_SIZE) * 5,
        );
        if !self.sent_client_prefix {
            self.sent_client_prefix = true;
            wire.extend_from_slice(CLIENT_PREFIX);
        }
        while !prefix.is_empty() || !data.is_empty() {
            let max_from_data = CLIENT_PART_SIZE.saturating_sub(prefix.len());
            let take_data = data.len().min(max_from_data);
            let record_len = prefix.len() + take_data;
            wire.extend_from_slice(CLIENT_HEADER);
            wire.extend_from_slice(&(record_len as u16).to_be_bytes());
            if !prefix.is_empty() {
                wire.extend_from_slice(prefix);
                prefix = &[];
            }
            if take_data > 0 {
                wire.extend_from_slice(&data[..take_data]);
                data = &data[take_data..];
            }
        }
        self.inner.write_all(&wire).await?;
        self.inner.flush().await?;
        Ok(())
    }

    /// Read application-data payload bytes from incoming TLS records into `buf`.
    ///
    /// Returns `Ok(0)` only on clean EOF at a record boundary.
    pub async fn read_payload(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        // First drain any already-buffered record bytes.
        if self.read_pos < self.read_buf.len() {
            let avail = self.read_buf.len() - self.read_pos;
            let n = avail.min(buf.len());
            buf[..n].copy_from_slice(&self.read_buf[self.read_pos..self.read_pos + n]);
            self.read_pos += n;
            if self.read_pos == self.read_buf.len() {
                self.read_buf.clear();
                self.read_pos = 0;
            }
            return Ok(n);
        }

        // If we are mid-record, read directly from inner up to pending_record_remaining.
        if self.pending_record_remaining > 0 {
            let limit = buf.len().min(self.pending_record_remaining);
            let n = self.inner.read(&mut buf[..limit]).await?;
            if n == 0 {
                return Err(Error::Disconnected);
            }
            self.pending_record_remaining -= n;
            return Ok(n);
        }

        // Loop until we find a non-empty Application Data record (skipping 0-length records).
        loop {
            let mut header = [0u8; 5];
            let mut filled = 0;
            while filled < 5 {
                let n = self.inner.read(&mut header[filled..]).await?;
                if n == 0 {
                    if filled == 0 {
                        return Ok(0);
                    }
                    return Err(Error::Disconnected);
                }
                filled += n;
            }
            if &header[..3] != SERVER_HEADER {
                return Err(Error::Tls(format!(
                    "bad incoming TLS record header: {:02x?}",
                    &header[..3]
                )));
            }
            let record_len = u16::from_be_bytes([header[3], header[4]]) as usize;
            if record_len == 0 {
                continue;
            }
            self.pending_record_remaining = record_len;
            let limit = buf.len().min(self.pending_record_remaining);
            let n = self.inner.read(&mut buf[..limit]).await?;
            if n == 0 {
                return Err(Error::Disconnected);
            }
            self.pending_record_remaining -= n;
            return Ok(n);
        }
    }

    /// Borrow the underlying transport stream.
    #[must_use]
    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    /// Consume the wrapper and return the underlying stream.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

async fn read_exact_append<S: AsyncRead + Unpin>(
    stream: &mut S,
    dst: &mut Vec<u8>,
    count: usize,
) -> Result<(), Error> {
    if count == 0 {
        return Ok(());
    }
    let start = dst.len();
    dst.resize(start + count, 0);
    stream.read_exact(&mut dst[start..]).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Disconnected
        } else {
            Error::Io(e)
        }
    })?;
    Ok(())
}
