//! `tg-wallet`: Built-in TON Wallet, BOC codecs, Toncenter API client, TON Connect 2.0,
//! and local passcode-encrypted key vault for Rustgram.
//!
//! Port origin: `Telegram/SourceFiles/wallet/*` and `Telegram/SourceFiles/gram/*`.

pub mod address;
pub mod boc;
pub mod cell;
pub mod contract;
pub mod emulate;
pub mod jettons;
pub mod mnemonic;
pub mod mtproto_api;
pub mod phrase_shares;
pub mod toncenter;
pub mod tonconnect;
pub mod vault;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Number of nanoTON in 1 TON (`10^9`).
pub const NANO_PER_TON: u64 = 1_000_000_000;

/// Default minimum transfer amount in nanoTON (`kTransferMinNanosDefault` in `wallet_session.h`).
pub const TRANSFER_MIN_NANOS_DEFAULT: i64 = 100_000_000;

/// Maximum minimum-transfer threshold (`kTransferMinNanosMax` in `wallet_session.h`).
pub const TRANSFER_MIN_NANOS_MAX: i64 = 1_i64 << 53;

/// Default old wallet bot username (`AppConfig::oldWalletBotUsername` in `main_app_config.cpp`).
pub const DEFAULT_OLD_WALLET_BOT_USERNAME: &str = "wallet";

/// Unified error type for `tg-wallet`.
#[derive(Debug, Error)]
pub enum WalletError {
    /// Address could not be parsed or failed CRC16 verification.
    #[error("invalid TON address: {0}")]
    InvalidAddress(String),

    /// Cell builder or slice violated TVM bounds (1023 bits, 4 refs, etc.).
    #[error("cell error: {0}")]
    Cell(String),

    /// BOC serialization or deserialization failed.
    #[error("invalid BOC: {0}")]
    Boc(String),

    /// Mnemonic phrase is invalid or failed TON seed validation.
    #[error("invalid mnemonic: {0}")]
    Mnemonic(String),

    /// Wallet contract construction or signing failed.
    #[error("contract error: {0}")]
    Contract(String),

    /// Toncenter HTTP / RPC / JSON parsing error.
    #[error("toncenter error: {0}")]
    Toncenter(String),

    /// Native MTProto wallet / payments RPC error.
    #[error("MTProto wallet error: {0}")]
    Mtproto(String),

    /// TON Connect link, bridge, manifest, or NaCl crypto error.
    #[error("TON Connect error: {0}")]
    TonConnect(String),

    /// Vault encryption, decryption, passcode, or persistence error.
    #[error("vault error: {0}")]
    Vault(String),

    /// Amount parsing or validation error.
    #[error("invalid amount: {0}")]
    InvalidAmount(String),
}

/// Application-level feature flags and endpoint configuration matching tdesktop
/// (`AppConfig::walletAvailable`, `AppConfig::oldWalletBotUsername`, `Wallet::Available`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletAppConfig {
    /// Server/app config flag `wallet_available` (default `true` in `main_app_config.cpp`).
    pub wallet_available: bool,
    /// When `true`, Telegram support mode disables the built-in wallet (`wallet_panel.cpp:226`).
    pub support_mode: bool,
    /// Legacy wallet bot username intercepted by the built-in wallet (`old_wallet_bot_username`).
    pub old_wallet_bot_username: String,
    /// Minimum transfer amount in nanoTON.
    pub transfer_min_nanos: i64,
    /// Configured toncenter base URL (must be set from config; never hardcoded in tests).
    pub toncenter_base_url: String,
    /// Optional toncenter API key.
    pub toncenter_api_key: Option<String>,
    /// Configured TON Connect HTTP bridge base URL.
    pub tonconnect_bridge_url: String,
    /// Whether this wallet targets TON testnet (`-3`) rather than mainnet (`-239`).
    pub testnet: bool,
}

impl Default for WalletAppConfig {
    fn default() -> Self {
        Self {
            wallet_available: true,
            support_mode: false,
            old_wallet_bot_username: DEFAULT_OLD_WALLET_BOT_USERNAME.to_owned(),
            transfer_min_nanos: TRANSFER_MIN_NANOS_DEFAULT,
            toncenter_base_url: String::new(),
            toncenter_api_key: None,
            tonconnect_bridge_url: String::new(),
            testnet: false,
        }
    }
}

impl WalletAppConfig {
    /// Mirrors `Wallet::Available(session)` (`wallet_panel.cpp:225-229`).
    #[must_use]
    pub const fn is_available(&self) -> bool {
        !self.support_mode && self.wallet_available
    }

    /// Mirrors `Wallet::IsOldWalletBotUsername` (`wallet_panel.cpp:198-203`).
    #[must_use]
    pub fn is_old_wallet_bot_username(&self, username: &str) -> bool {
        !self.old_wallet_bot_username.is_empty()
            && self.old_wallet_bot_username.eq_ignore_ascii_case(username)
    }
}

/// Formats a nanoTON value as a human-readable decimal TON string (trimming trailing zeros).
#[must_use]
pub fn format_ton(nano: i64) -> String {
    let neg = nano < 0;
    let abs = nano.unsigned_abs();
    let whole = abs / NANO_PER_TON;
    let frac = abs % NANO_PER_TON;
    let prefix = if neg { "-" } else { "" };
    if frac == 0 {
        format!("{prefix}{whole}")
    } else {
        let frac_str = format!("{frac:09}");
        let trimmed = frac_str.trim_end_matches('0');
        format!("{prefix}{whole}.{trimmed}")
    }
}

/// Parses a decimal TON string (e.g. `"1.25"`, `"0.05"`, `"10"`) into nanoTON.
///
/// Mirrors `ParseLinkAmount` in `wallet_panel.cpp`: accepts `.` as decimal separator, rejects
/// commas, negative values, or more than 9 fractional digits.
pub fn parse_ton_amount(input: &str) -> Result<u64, WalletError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(WalletError::InvalidAmount("empty amount".to_owned()));
    }
    if trimmed.contains(',') || trimmed.starts_with('-') || trimmed.starts_with('+') {
        return Err(WalletError::InvalidAmount(format!(
            "unsupported amount syntax: {trimmed}"
        )));
    }
    let mut parts = trimmed.split('.');
    let whole_str = parts.next().unwrap_or("");
    let frac_str = parts.next();
    if parts.next().is_some() {
        return Err(WalletError::InvalidAmount(format!(
            "multiple decimal points: {trimmed}"
        )));
    }
    if whole_str.is_empty() || !whole_str.bytes().all(|b| b.is_ascii_digit()) {
        return Err(WalletError::InvalidAmount(format!(
            "invalid integer part: {trimmed}"
        )));
    }
    let whole: u64 = whole_str
        .parse()
        .map_err(|_| WalletError::InvalidAmount(format!("amount overflow: {trimmed}")))?;

    let frac: u64 = match frac_str {
        None => 0,
        Some(f) => {
            if f.is_empty() || f.len() > 9 || !f.bytes().all(|b| b.is_ascii_digit()) {
                return Err(WalletError::InvalidAmount(format!(
                    "invalid fractional part: {trimmed}"
                )));
            }
            let scale = 10_u64.pow((9 - f.len()) as u32);
            let parsed: u64 = f
                .parse()
                .map_err(|_| WalletError::InvalidAmount(format!("fraction overflow: {trimmed}")))?;
            parsed.checked_mul(scale).ok_or_else(|| {
                WalletError::InvalidAmount(format!("fraction overflow: {trimmed}"))
            })?
        }
    };

    whole
        .checked_mul(NANO_PER_TON)
        .and_then(|v| v.checked_add(frac))
        .ok_or_else(|| WalletError::InvalidAmount(format!("amount overflow: {trimmed}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_ton_amounts() {
        assert_eq!(format_ton(0), "0");
        assert_eq!(format_ton(1_000_000_000), "1");
        assert_eq!(format_ton(1_250_000_000), "1.25");
        assert_eq!(format_ton(1), "0.000000001");
        assert_eq!(format_ton(-500_000_000), "-0.5");

        assert_eq!(parse_ton_amount("0").unwrap(), 0);
        assert_eq!(parse_ton_amount("1.25").unwrap(), 1_250_000_000);
        assert_eq!(parse_ton_amount("0.000000001").unwrap(), 1);
        assert!(parse_ton_amount("1,5").is_err());
        assert!(parse_ton_amount("-1").is_err());
        assert!(parse_ton_amount("1.1234567890").is_err());
    }

    #[test]
    fn app_config_availability_gating() {
        let mut cfg = WalletAppConfig::default();
        assert!(cfg.is_available());
        assert!(cfg.is_old_wallet_bot_username("Wallet"));
        assert!(!cfg.is_old_wallet_bot_username("otherbot"));

        cfg.wallet_available = false;
        assert!(!cfg.is_available());

        cfg.wallet_available = true;
        cfg.support_mode = true;
        assert!(!cfg.is_available());
    }
}

#[cfg(test)]
mod fuzz_untrusted;
