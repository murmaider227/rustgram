//! TON address parsing, formatting (raw and user-friendly CRC16-CCITT), and transfer links.
//!
//! Port origin: `Telegram/SourceFiles/wallet/wallet_address.h`,
//! `Telegram/SourceFiles/wallet/wallet_address.cpp`.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::WalletError;

/// Tag byte for a bounceable mainnet user-friendly address (`EQ...`).
pub const TAG_BOUNCEABLE: u8 = 0x11;
/// Tag byte for a non-bounceable mainnet user-friendly address (`UQ...`).
pub const TAG_NON_BOUNCEABLE: u8 = 0x51;
/// Bit flag added to the tag byte for testnet addresses (`kQ...` / `0Q...`).
pub const TAG_TESTNET_FLAG: u8 = 0x80;

/// Canonical TON standard smart-contract address (`workchain:hash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TonAddress {
    /// Workchain ID (`0` = basechain, `-1` = masterchain).
    pub workchain: i8,
    /// 256-bit account ID (StateInit cell hash).
    pub hash: [u8; 32],
}

/// Address format metadata preserved when parsing a user input string (`ParsedAddress`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedAddress {
    /// Canonical address.
    pub address: TonAddress,
    /// Whether the input was in user-friendly base64 form (`true`) or raw `wc:hex` form (`false`).
    pub friendly: bool,
    /// Whether the address had the bounceable flag set (defaults to `true` for raw addresses).
    pub bounceable: bool,
    /// Whether the address had the testnet-only flag set.
    pub testnet: bool,
}

/// Parsed `ton://transfer/<address>?...` link (`Wallet::TransferLink`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferLink {
    /// Destination address.
    pub address: ParsedAddress,
    /// Optional transfer amount in nanoTON.
    pub amount_nano: Option<u64>,
    /// Optional plain UTF-8 transfer comment (`text=...`).
    pub comment: Option<String>,
    /// Optional expiration UNIX timestamp in seconds (`exp=...`).
    pub expires_at: Option<u64>,
}

/// Computes TON's CRC16-CCITT (XMODEM: polynomial `0x1021`, initial value `0x0000`).
#[must_use]
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

impl TonAddress {
    /// Constructs a new [`TonAddress`] from workchain and 32-byte account hash.
    #[must_use]
    pub const fn new(workchain: i8, hash: [u8; 32]) -> Self {
        Self { workchain, hash }
    }

    /// Parses either a raw (`0:<64-hex>`) or user-friendly (48-char base64/base64url) TON address.
    pub fn parse(text: &str) -> Result<ParsedAddress, WalletError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(WalletError::InvalidAddress("empty address".to_owned()));
        }
        if let Some((wc_str, hex_str)) = trimmed.split_once(':') {
            let workchain: i8 = wc_str
                .parse()
                .map_err(|_| WalletError::InvalidAddress(format!("invalid workchain: {wc_str}")))?;
            if hex_str.len() != 64 {
                return Err(WalletError::InvalidAddress(format!(
                    "expected 64 hex chars, got {}",
                    hex_str.len()
                )));
            }
            let decoded = hex::decode(hex_str)
                .map_err(|e| WalletError::InvalidAddress(format!("invalid hex hash: {e}")))?;
            let mut hash = [0_u8; 32];
            hash.copy_from_slice(&decoded);
            return Ok(ParsedAddress {
                address: Self { workchain, hash },
                friendly: false,
                bounceable: true,
                testnet: false,
            });
        }

        // User-friendly 48-character base64 or base64url (36 bytes = 1 tag + 1 wc + 32 hash + 2 crc16).
        if trimmed.len() != 48 {
            return Err(WalletError::InvalidAddress(format!(
                "expected 48-char user-friendly address or raw wc:hex, got length {}",
                trimmed.len()
            )));
        }

        let bytes = if trimmed.contains('-') || trimmed.contains('_') {
            URL_SAFE.decode(trimmed)
        } else {
            STANDARD.decode(trimmed)
        }
        .map_err(|e| WalletError::InvalidAddress(format!("invalid base64: {e}")))?;

        if bytes.len() != 36 {
            return Err(WalletError::InvalidAddress(format!(
                "decoded address must be 36 bytes, got {}",
                bytes.len()
            )));
        }

        let expected_crc = crc16_ccitt(&bytes[..34]);
        let actual_crc = u16::from_be_bytes([bytes[34], bytes[35]]);
        if expected_crc != actual_crc {
            return Err(WalletError::InvalidAddress(format!(
                "CRC16 mismatch: expected 0x{expected_crc:04x}, got 0x{actual_crc:04x}"
            )));
        }

        let mut tag = bytes[0];
        let testnet = (tag & TAG_TESTNET_FLAG) != 0;
        tag &= !TAG_TESTNET_FLAG;
        let bounceable = match tag {
            TAG_BOUNCEABLE => true,
            TAG_NON_BOUNCEABLE => false,
            _ => {
                return Err(WalletError::InvalidAddress(format!(
                    "unsupported address tag byte 0x{:02x}",
                    bytes[0]
                )));
            }
        };

        let workchain = bytes[1] as i8;
        let mut hash = [0_u8; 32];
        hash.copy_from_slice(&bytes[2..34]);

        Ok(ParsedAddress {
            address: Self { workchain, hash },
            friendly: true,
            bounceable,
            testnet,
        })
    }

    /// Formats as canonical lowercase raw address `<workchain>:<64-hex>`.
    #[must_use]
    pub fn to_raw(&self) -> String {
        format!("{}:{}", self.workchain, hex::encode(self.hash))
    }

    /// Formats as a 48-char user-friendly address (URL-safe base64 by default, matching TON standard).
    #[must_use]
    pub fn to_friendly(&self, bounceable: bool, testnet: bool) -> String {
        self.to_friendly_with_encoding(bounceable, testnet, true)
    }

    /// Formats as a 48-char user-friendly address with either URL-safe (`-_`) or standard (`+/`) base64.
    #[must_use]
    pub fn to_friendly_with_encoding(
        &self,
        bounceable: bool,
        testnet: bool,
        url_safe: bool,
    ) -> String {
        let mut buf = [0_u8; 36];
        let base_tag = if bounceable {
            TAG_BOUNCEABLE
        } else {
            TAG_NON_BOUNCEABLE
        };
        buf[0] = if testnet {
            base_tag | TAG_TESTNET_FLAG
        } else {
            base_tag
        };
        buf[1] = self.workchain as u8;
        buf[2..34].copy_from_slice(&self.hash);
        let crc = crc16_ccitt(&buf[..34]).to_be_bytes();
        buf[34] = crc[0];
        buf[35] = crc[1];
        if url_safe {
            URL_SAFE.encode(buf)
        } else {
            STANDARD.encode(buf)
        }
    }
}

impl std::fmt::Display for TonAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_friendly(false, false))
    }
}

/// Converts any valid TON address string to canonical raw form (`Wallet::CanonicalAddress`).
pub fn canonical_address(text: &str) -> Option<String> {
    TonAddress::parse(text).ok().map(|p| p.address.to_raw())
}

/// Formats a raw or friendly TON address into user-friendly form (`Wallet::FormatFriendly`).
pub fn format_friendly(text: &str, bounceable: bool, testnet: bool) -> Option<String> {
    TonAddress::parse(text)
        .ok()
        .map(|p| p.address.to_friendly(bounceable, testnet))
}

/// Returns whether a transfer link expiration timestamp has elapsed (`Wallet::TransferLinkExpired`).
#[must_use]
pub const fn transfer_link_expired(expires_at: Option<u64>, now_unix: u64) -> bool {
    match expires_at {
        Some(exp) => now_unix >= exp,
        None => false,
    }
}

/// Parses a `ton://transfer/<address>?amount=<nano>&text=<comment>&exp=<ts>` URI.
///
/// Mirrors `Wallet::ParseTransferLink` (`wallet_address.cpp:73-111`): rejects jetton or
/// binary-BOC transfer links for simple TON transfers.
pub fn parse_transfer_link(raw_url: &str) -> Result<TransferLink, WalletError> {
    let url = Url::parse(raw_url.trim())
        .map_err(|e| WalletError::InvalidAddress(format!("invalid transfer URI: {e}")))?;
    if !url.scheme().eq_ignore_ascii_case("ton") {
        return Err(WalletError::InvalidAddress(
            "expected ton:// scheme".to_owned(),
        ));
    }
    if url.host_str() != Some("transfer") {
        return Err(WalletError::InvalidAddress(
            "expected ton://transfer/ host".to_owned(),
        ));
    }
    let addr_str = url.path().trim_start_matches('/');
    let address = TonAddress::parse(addr_str)?;

    let mut amount_nano = None;
    let mut comment = None;
    let mut expires_at = None;

    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "jetton" | "bin" | "init" => {
                return Err(WalletError::InvalidAddress(format!(
                    "unsupported transfer link parameter: {k}"
                )));
            }
            "amount" => {
                let parsed: u64 = v
                    .parse()
                    .map_err(|_| WalletError::InvalidAmount(format!("invalid link amount: {v}")))?;
                amount_nano = Some(parsed);
            }
            "text" => {
                comment = Some(v.into_owned());
            }
            "exp" => {
                let parsed: u64 = v.parse().map_err(|_| {
                    WalletError::InvalidAddress(format!("invalid exp timestamp: {v}"))
                })?;
                expires_at = Some(parsed);
            }
            _ => {}
        }
    }

    Ok(TransferLink {
        address,
        amount_nano,
        comment,
        expires_at,
    })
}

/// Builds a `ton://transfer/<friendly-address>` payment URI for QR codes.
#[must_use]
pub fn build_transfer_link(
    address: &TonAddress,
    bounceable: bool,
    testnet: bool,
    amount_nano: Option<u64>,
    comment: Option<&str>,
) -> String {
    let friendly = address.to_friendly(bounceable, testnet);
    let mut url = format!("ton://transfer/{friendly}");
    let mut sep = '?';
    if let Some(amount) = amount_nano
        && amount > 0
    {
        url.push(sep);
        url.push_str(&format!("amount={amount}"));
        sep = '&';
    }
    if let Some(text) = comment
        && !text.is_empty()
    {
        url.push(sep);
        url.push_str("text=");
        url.push_str(&url::form_urlencoded::byte_serialize(text.as_bytes()).collect::<String>());
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_answer_address_roundtrip_from_tdesktop_fixtures() {
        // Fixtures from Telegram/SourceFiles/gram/tests/fixtures/api-dns-records-*.json
        let cases = [
            (
                "0:9da971af38d2f03abdf308d5f91636a97e5a2b07a66c39d71d7cbae3b032eddc",
                "UQCdqXGvONLwOr3zCNX5FjapflorB6ZsOdcdfLrjsDLt3AF4",
                false,
                false,
            ),
            (
                "0:471f04d4ef9a52ec55d17daffff8c52d222c28d184874069029444eee06125f1",
                "EQBHHwTU75pS7FXRfa__-MUtIiwo0YSHQGkClETu4GEl8df2",
                true,
                false,
            ),
            (
                "0:8b704249e018faa59bdc356f463a7a34fc0201dbbfa7441a387be83e9fffbfc4",
                "UQCLcEJJ4Bj6pZvcNW9GOno0_AIB27-nRBo4e-g-n_-_xGUY",
                false,
                false,
            ),
            (
                "0:91257b3e0eb645719c94e8961a2cb15c551a11ccd4969cb4b4c0bfde6a671a08",
                "UQCRJXs-DrZFcZyU6JYaLLFcVRoRzNSWnLS0wL_eamcaCLXS",
                false,
                false,
            ),
        ];

        for (raw, friendly, bounceable, testnet) in cases {
            let parsed_friendly = TonAddress::parse(friendly).unwrap();
            assert!(parsed_friendly.friendly);
            assert_eq!(parsed_friendly.bounceable, bounceable);
            assert_eq!(parsed_friendly.testnet, testnet);
            assert_eq!(parsed_friendly.address.to_raw(), raw);
            assert_eq!(
                parsed_friendly.address.to_friendly(bounceable, testnet),
                friendly
            );

            let parsed_raw = TonAddress::parse(raw).unwrap();
            assert!(!parsed_raw.friendly);
            assert_eq!(parsed_raw.address, parsed_friendly.address);
        }
    }

    #[test]
    fn masterchain_and_testnet_flags_and_crc_rejection() {
        let addr = TonAddress::new(-1, [0xAB; 32]);
        let eq_test = addr.to_friendly(true, true);
        assert!(eq_test.starts_with("kf") || eq_test.starts_with("kQ") || eq_test.starts_with("k"));
        let parsed = TonAddress::parse(&eq_test).unwrap();
        assert_eq!(parsed.address, addr);
        assert!(parsed.bounceable);
        assert!(parsed.testnet);

        // Corrupt last char -> CRC16 mismatch.
        let mut corrupted = eq_test.into_bytes();
        corrupted[47] = if corrupted[47] == b'A' { b'B' } else { b'A' };
        let corrupted = String::from_utf8(corrupted).unwrap();
        assert!(TonAddress::parse(&corrupted).is_err());
    }

    #[test]
    fn transfer_link_build_and_parse() {
        let addr = TonAddress::new(0, [0x42; 32]);
        let uri = build_transfer_link(&addr, false, false, Some(1_500_000_000), Some("Hello TON"));
        let parsed = parse_transfer_link(&uri).unwrap();
        assert_eq!(parsed.address.address, addr);
        assert!(!parsed.address.bounceable);
        assert_eq!(parsed.amount_nano, Some(1_500_000_000));
        assert_eq!(parsed.comment.as_deref(), Some("Hello TON"));
        assert!(!transfer_link_expired(Some(200), 100));
        assert!(transfer_link_expired(Some(100), 100));

        // Reject unsupported jetton links (matching wallet_address.cpp:78).
        let jetton_uri = format!("{uri}&jetton=EQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAM9c");
        assert!(parse_transfer_link(&jetton_uri).is_err());
    }
}
