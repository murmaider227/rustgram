//! TON wallet contract state init (`V3R2`, `V4R2`, `V5R1`) and transfer message building/signing.
//!
//! Port origin: wallet contract specs used by `Telegram/SourceFiles/wallet/` (`v3r2`, `v4r2`,
//! `v5r1`) and official TON `@ton/ton` contract bytecode & signing specifications.

use std::sync::Arc;

use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::WalletError;
use crate::address::TonAddress;
use crate::boc::{deserialize_boc, deserialize_boc_base64, serialize_boc_base64};
use crate::cell::{Cell, CellBuilder};
use crate::mnemonic::TonKeypair;

/// Default `subwallet_id` for V3R2 and V4R2 wallets on basechain (`workchain = 0`).
pub const DEFAULT_SUBWALLET_ID_V3_V4: u32 = 698_983_191;
/// Default `wallet_id` for V5R1 on mainnet (`networkGlobalId = -239`, workchain 0, subwallet 0).
pub const DEFAULT_WALLET_ID_V5R1_MAINNET: i32 = 2_147_483_409;
/// Default `wallet_id` for V5R1 on testnet (`networkGlobalId = -3`, workchain 0, subwallet 0).
pub const DEFAULT_WALLET_ID_V5R1_TESTNET: i32 = 2_147_483_645;

/// Default send mode: pay transfer fees separately (`1`) + ignore action errors (`2`) = `3`.
pub const DEFAULT_SEND_MODE: u8 = 3;
/// Opcode for Wallet V5R1 signed external transfer (`"sign"` = `0x7369676E`).
pub const V5R1_AUTH_SIGNED_EXTERNAL: u32 = 0x7369_676E;
/// OutAction `action_send_msg` tag (`0x0ec3c86d`).
pub const ACTION_SEND_MSG_TAG: u32 = 0x0EC3_C86D;

/// Official compiled TVM code BOC (base64) for Wallet V3R2.
/// Known SHA-256 cell hash: `84dafa449f98a6987789ba232358072bc0f76dc4524002a5d0918b9a75d2d599`.
pub const WALLET_V3R2_CODE_BOC_BASE64: &str = "te6cckEBAQEAcQAA3v8AIN0gggFMl7ohggEznLqxn3Gw7UTQ0x/THzHXC//jBOCk8mCDCNcYINMf0x/TH/gjE7vyY+1E0NMf0x/T/9FRMrryoVFEuvKiBPkBVBBV+RDyo/gAkyDXSpbTB9QC+wDo0QGkyMsfyx/L/8ntVBC9ba0=";

/// Official compiled TVM code BOC (base64) for Wallet V4R2.
/// Known SHA-256 cell hash: `feb5ff6820e2ff0d9483e7e0d62c817d846789fb4ae580c878866d959dabd5c0`.
pub const WALLET_V4R2_CODE_BOC_BASE64: &str = "te6ccgECFAEAAtQAART/APSkE/S88sgLAQIBIAIDAgFIBAUE+PKDCNcYINMf0x/THwL4I7vyZO1E0NMf0x/T//QE0VFDuvKhUVG68qIF+QFUEGT5EPKj+AAkpMjLH1JAyx9SMMv/UhD0AMntVPgPAdMHIcAAn2xRkyDXSpbTB9QC+wDoMOAhwAHjACHAAuMAAcADkTDjDQOkyMsfEssfy/8QERITAubQAdDTAyFxsJJfBOAi10nBIJJfBOAC0x8hghBwbHVnvSKCEGRzdHK9sJJfBeAD+kAwIPpEAcjKB8v/ydDtRNCBAUDXIfQEMFyBAQj0Cm+hMbOSXwfgBdM/yCWCEHBsdWe6kjgw4w0DghBkc3RyupJfBuMNBgcCASAICQB4AfoA9AQw+CdvIjBQCqEhvvLgUIIQcGx1Z4MesXCAGFAEywUmzxZY+gIZ9ADLaRfLH1Jgyz8gyYBA+wAGAIpQBIEBCPRZMO1E0IEBQNcgyAHPFvQAye1UAXKwjiOCEGRzdHKDHrFwgBhQBcsFUAPPFiP6AhPLassfyz/JgED7AJJfA+ICASAKCwBZvSQrb2omhAgKBrkPoCGEcNQICEekk30pkQzmkD6f+YN4EoAbeBAUiYcVnzGEAgFYDA0AEbjJftRNDXCx+AA9sp37UTQgQFA1yH0BDACyMoHy//J0AGBAQj0Cm+hMYAIBIA4PABmtznaiaEAga5Drhf/AABmvHfaiaEAQa5DrhY/AAG7SB/oA1NQi+QAFyMoHFcv/ydB3dIAYyMsFywIizxZQBfoCFMtrEszMyXP7AMhAFIEBCPRR8qcCAHCBAQjXGPoA0z/IVCBHgQEI9FHyp4IQbm90ZXB0gBjIywXLAlAGzxZQBPoCFMtqEssfyz/Jc/sAAgBsgQEI1xj6ANM/MFIkgQEI9Fnyp4IQZHN0cnB0gBjIywXLAlAFzxZQA/oCE8tqyx8Syz/Jc/sAAAr0AMntVA==";

/// Official compiled TVM code BOC (hex) for Wallet V5R1 (W5).
/// Known SHA-256 cell hash: `20834b7b72b112147e1b2fb457b84e74d1a30f04f737d4f62a668e9552d2b72f`.
pub const WALLET_V5R1_CODE_BOC_HEX: &str = "b5ee9c7241021401000281000114ff00f4a413f4bcf2c80b01020120020d020148030402dcd020d749c120915b8f6320d70b1f2082106578746ebd21821073696e74bdb0925f03e082106578746eba8eb48020d72101d074d721fa4030fa44f828fa443058bd915be0ed44d0810141d721f4058307f40e6fa1319130e18040d721707fdb3ce03120d749810280b99130e070e2100f020120050c020120060902016e07080019adce76a2684020eb90eb85ffc00019af1df6a2684010eb90eb858fc00201480a0b0017b325fb51341c75c875c2c7e00011b262fb513435c280200019be5f0f6a2684080a0eb90fa02c0102f20e011e20d70b1f82107369676ebaf2e08a7f0f01e68ef0eda2edfb218308d722028308d723208020d721d31fd31fd31fed44d0d200d31f20d31fd3ffd70a000af90140ccf9109a28945f0adb31e1f2c087df02b35007b0f2d0845125baf2e0855036baf2e086f823bbf2d0882292f800de01a47fc8ca00cb1f01cf16c9ed542092f80fde70db3cd81003f6eda2edfb02f404216e926c218e4c0221d73930709421c700b38e2d01d72820761e436c20d749c008f2e09320d74ac002f2e09320d71d06c712c2005230b0f2d089d74cd7393001a4e86c128407bbf2e093d74ac000f2e093ed55e2d20001c000915be0ebd72c08142091709601d72c081c12e25210b1e30f20d74a111213009601fa4001fa44f828fa443058baf2e091ed44d0810141d718f405049d7fc8ca0040048307f453f2e08b8e14038307f45bf2e08c22d70a00216e01b3b0f2d090e2c85003cf1612f400c9ed54007230d72c08248e2d21f2e092d200ed44d0d2005113baf2d08f54503091319c01810140d721d70a00f2e08ee2c8ca0058cf16c9ed5493f2c08de20010935bdb31e1d74cd0b4d6c35e";

/// Supported TON wallet smart-contract versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum WalletVersion {
    /// Wallet V3 revision 2 (`wallet_v3r2`).
    V3R2,
    /// Wallet V4 revision 2 (`wallet_v4r2`).
    V4R2,
    /// Wallet V5 revision 1 / W5 (`wallet_v5r1`).
    #[default]
    V5R1,
}

impl WalletVersion {
    /// Human-readable label shown in the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::V3R2 => "v3r2",
            Self::V4R2 => "v4r2",
            Self::V5R1 => "v5r1 (W5)",
        }
    }

    /// Maximum outgoing internal messages allowed in one external transfer.
    #[must_use]
    pub const fn max_messages(self) -> usize {
        match self {
            Self::V3R2 | Self::V4R2 => 4,
            Self::V5R1 => 255,
        }
    }

    /// Returns the official compiled TVM code [`Cell`] for this wallet version.
    pub fn code_cell(self) -> Result<Arc<Cell>, WalletError> {
        match self {
            Self::V3R2 => deserialize_boc_base64(WALLET_V3R2_CODE_BOC_BASE64),
            Self::V4R2 => deserialize_boc_base64(WALLET_V4R2_CODE_BOC_BASE64),
            Self::V5R1 => {
                let bytes = hex::decode(WALLET_V5R1_CODE_BOC_HEX)
                    .map_err(|e| WalletError::Contract(format!("v5r1 hex decode: {e}")))?;
                let mut roots = deserialize_boc(&bytes)?;
                roots
                    .drain(..)
                    .next()
                    .ok_or_else(|| WalletError::Contract("v5r1 code BOC empty".to_owned()))
            }
        }
    }
}

/// Outgoing internal message specification for a wallet transfer.
#[derive(Debug, Clone)]
pub struct TransferMessage {
    /// Destination TON address.
    pub destination: TonAddress,
    /// Transfer value in nanoTON.
    pub amount_nano: u64,
    /// Whether the internal message is bounceable.
    pub bounce: bool,
    /// TON send mode flags (default [`DEFAULT_SEND_MODE`] = 3).
    pub send_mode: u8,
    /// Optional message body cell (e.g. text comment or smart contract call).
    pub body: Option<Arc<Cell>>,
    /// Optional StateInit cell when deploying a target contract.
    pub state_init: Option<Arc<Cell>>,
}

impl TransferMessage {
    /// Creates a plain TON transfer with optional UTF-8 text comment.
    pub fn simple(
        destination: TonAddress,
        amount_nano: u64,
        bounce: bool,
        comment: Option<&str>,
    ) -> Result<Self, WalletError> {
        let body = match comment {
            Some(text) if !text.is_empty() => {
                let mut b = CellBuilder::new();
                b.store_text_comment(text)?;
                Some(Arc::new(b.end_cell()?))
            }
            _ => None,
        };
        Ok(Self {
            destination,
            amount_nano,
            bounce,
            send_mode: DEFAULT_SEND_MODE,
            body,
            state_init: None,
        })
    }

    /// Encodes this transfer as a TVM `MessageRelaxed` internal message cell.
    pub fn to_internal_cell(&self) -> Result<Arc<Cell>, WalletError> {
        let mut b = CellBuilder::new();
        // int_msg_info$0 ihr_disabled:1 bounce:1 bounced:0 src:addr_none$00
        b.store_bit(false)?; // int_msg_info
        b.store_bit(true)?; // ihr_disabled
        b.store_bit(self.bounce)?;
        b.store_bit(false)?; // bounced
        b.store_addr_none()?; // src = addr_none
        b.store_address(&self.destination)?;
        b.store_coins(u128::from(self.amount_nano))?;
        b.store_bit(false)?; // extra_currencies = empty dict
        b.store_coins(0)?; // ihr_fee
        b.store_coins(0)?; // fwd_fee
        b.store_uint(0, 64)?; // created_lt
        b.store_uint(0, 32)?; // created_at

        // init:(Maybe (Either StateInit ^StateInit))
        match &self.state_init {
            Some(init) => {
                b.store_bit(true)?; // Just
                b.store_bit(true)?; // Right (^StateInit)
                b.store_ref(Arc::clone(init))?;
            }
            None => {
                b.store_bit(false)?; // Nothing
            }
        }

        // body:(Either X ^X)
        match &self.body {
            Some(body) => {
                // Store inline when it fits cleanly without overflowing bits or refs.
                if b.remaining_bits() > body.bit_len() && b.ref_count() + body.refs().len() <= 4 {
                    b.store_bit(false)?; // Left (inline)
                    let mut slice = body.begin_parse();
                    for _ in 0..body.bit_len() {
                        b.store_bit(slice.load_bit()?)?;
                    }
                    for r in body.refs() {
                        b.store_ref(Arc::clone(r))?;
                    }
                } else {
                    b.store_bit(true)?; // Right (^Cell)
                    b.store_ref(Arc::clone(body))?;
                }
            }
            None => {
                b.store_bit(false)?; // Left (empty inline body)
            }
        }

        Ok(Arc::new(b.end_cell()?))
    }
}

/// Constructed and signed external-in wallet transfer ready for `sendBoc`.
#[derive(Debug, Clone)]
pub struct SignedTransfer {
    /// Complete external-in message cell (`ext_in_msg_info` + optional `StateInit` + signed body).
    pub external_message: Arc<Cell>,
    /// Signed wallet body cell.
    pub body: Arc<Cell>,
    /// 64-byte Ed25519 signature over the signing payload cell hash.
    pub signature: [u8; 64],
    /// SHA-256 hash of the external-in message cell.
    pub message_hash: [u8; 32],
    /// Base64-encoded BOC ready to submit to `toncenter` `sendBoc`.
    pub boc_base64: String,
}

/// Configured TON wallet contract capable of deriving its `StateInit`, address, and signed transfers.
#[derive(Debug, Clone)]
pub struct WalletContract {
    /// Contract version (`V3R2`, `V4R2`, or `V5R1`).
    pub version: WalletVersion,
    /// Workchain (`0` for basechain).
    pub workchain: i8,
    /// Wallet/subwallet identifier (32-bit).
    pub wallet_id: i32,
    /// 32-byte Ed25519 public key.
    pub public_key: [u8; 32],
    /// Whether this wallet targets testnet.
    pub testnet: bool,
}

impl WalletContract {
    /// Creates a wallet contract with default `wallet_id` for `version` and `testnet`.
    #[must_use]
    pub const fn new(
        version: WalletVersion,
        workchain: i8,
        public_key: [u8; 32],
        testnet: bool,
    ) -> Self {
        let wallet_id = match version {
            WalletVersion::V3R2 | WalletVersion::V4R2 => {
                (DEFAULT_SUBWALLET_ID_V3_V4 as i32).wrapping_add(workchain as i32)
            }
            WalletVersion::V5R1 => {
                if testnet {
                    DEFAULT_WALLET_ID_V5R1_TESTNET
                } else {
                    DEFAULT_WALLET_ID_V5R1_MAINNET
                }
            }
        };
        Self {
            version,
            workchain,
            wallet_id,
            public_key,
            testnet,
        }
    }

    /// Builds the initial persistent data cell (`c4`) for this wallet contract.
    pub fn init_data_cell(&self) -> Result<Arc<Cell>, WalletError> {
        let mut b = CellBuilder::new();
        match self.version {
            WalletVersion::V3R2 => {
                b.store_uint(0, 32)?; // seqno
                b.store_uint(self.wallet_id as u32 as u64, 32)?;
                b.store_bytes(&self.public_key)?;
            }
            WalletVersion::V4R2 => {
                b.store_uint(0, 32)?; // seqno
                b.store_uint(self.wallet_id as u32 as u64, 32)?;
                b.store_bytes(&self.public_key)?;
                b.store_bit(false)?; // empty plugins dict
            }
            WalletVersion::V5R1 => {
                b.store_bit(true)?; // is_signature_allowed
                b.store_uint(0, 32)?; // seqno
                b.store_int(i64::from(self.wallet_id), 32)?;
                b.store_bytes(&self.public_key)?;
                b.store_bit(false)?; // empty extensions dict
            }
        }
        Ok(Arc::new(b.end_cell()?))
    }

    /// Builds the TVM `StateInit` cell (`split_depth:0 special:0 code:Just data:Just library:0`).
    pub fn state_init_cell(&self) -> Result<Arc<Cell>, WalletError> {
        let code = self.version.code_cell()?;
        let data = self.init_data_cell()?;
        let mut b = CellBuilder::new();
        b.store_bit(false)?; // split_depth = None
        b.store_bit(false)?; // special = None
        b.store_maybe_ref(Some(code))?;
        b.store_maybe_ref(Some(data))?;
        b.store_bit(false)?; // library = None
        Ok(Arc::new(b.end_cell()?))
    }

    /// Computes the deterministic [`TonAddress`] (`workchain : state_init.hash()`).
    pub fn address(&self) -> Result<TonAddress, WalletError> {
        let init = self.state_init_cell()?;
        Ok(TonAddress::new(self.workchain, init.hash()))
    }

    /// Builds the unsigned signing payload cell for a batch of outgoing transfers.
    pub fn build_signing_payload(
        &self,
        seqno: u32,
        valid_until: u32,
        messages: &[TransferMessage],
    ) -> Result<CellBuilder, WalletError> {
        if messages.is_empty() {
            return Err(WalletError::Contract(
                "transfer must contain at least one message".to_owned(),
            ));
        }
        if messages.len() > self.version.max_messages() {
            return Err(WalletError::Contract(format!(
                "{} supports at most {} messages per transfer",
                self.version.label(),
                self.version.max_messages()
            )));
        }

        let mut signing = CellBuilder::new();
        match self.version {
            WalletVersion::V3R2 => {
                signing.store_uint(self.wallet_id as u32 as u64, 32)?;
                signing.store_uint(u64::from(valid_until), 32)?;
                signing.store_uint(u64::from(seqno), 32)?;
                for msg in messages {
                    signing.store_uint(u64::from(msg.send_mode), 8)?;
                    signing.store_ref(msg.to_internal_cell()?)?;
                }
            }
            WalletVersion::V4R2 => {
                signing.store_uint(self.wallet_id as u32 as u64, 32)?;
                signing.store_uint(u64::from(valid_until), 32)?;
                signing.store_uint(u64::from(seqno), 32)?;
                signing.store_uint(0, 8)?; // op = 0 (simple send)
                for msg in messages {
                    signing.store_uint(u64::from(msg.send_mode), 8)?;
                    signing.store_ref(msg.to_internal_cell()?)?;
                }
            }
            WalletVersion::V5R1 => {
                signing.store_uint(u64::from(V5R1_AUTH_SIGNED_EXTERNAL), 32)?;
                signing.store_int(i64::from(self.wallet_id), 32)?;
                signing.store_uint(u64::from(valid_until), 32)?;
                signing.store_uint(u64::from(seqno), 32)?;
                // OutList is a linked list of cells built from empty leaf upward:
                // out_list_empty = empty cell;
                // out_list{n+1} = prev:^OutList action_send_msg(0x0ec3c86d, mode:u8, msg:^Cell).
                let mut list = Arc::new(Cell::empty());
                for msg in messages {
                    // External signed W5 requires IGNORE_ERRORS (bit 1 = 2).
                    let mode = msg.send_mode | 2;
                    let mut node = CellBuilder::new();
                    node.store_ref(list)?;
                    node.store_uint(u64::from(ACTION_SEND_MSG_TAG), 32)?;
                    node.store_uint(u64::from(mode), 8)?;
                    node.store_ref(msg.to_internal_cell()?)?;
                    list = Arc::new(node.end_cell()?);
                }
                signing.store_maybe_ref(Some(list))?;
                signing.store_bit(false)?; // has_other_actions = 0
            }
        }

        Ok(signing)
    }

    /// Wraps a wallet signing payload and 64-byte signature buffer into an external-in BOC message.
    ///
    /// Automatically attaches `StateInit` when `seqno == 0` (initial wallet deployment).
    fn wrap_external_transfer(
        &self,
        seqno: u32,
        signing: CellBuilder,
        sig_bytes: [u8; 64],
    ) -> Result<SignedTransfer, WalletError> {
        // V3R2 and V4R2 pack signature at the front; V5R1 packs signature at the tail.
        let mut body_builder = CellBuilder::new();
        match self.version {
            WalletVersion::V3R2 | WalletVersion::V4R2 => {
                body_builder.store_bytes(&sig_bytes)?;
                body_builder.store_builder(&signing)?;
            }
            WalletVersion::V5R1 => {
                body_builder.store_builder(&signing)?;
                body_builder.store_bytes(&sig_bytes)?;
            }
        }
        let body = Arc::new(body_builder.end_cell()?);

        let dest = self.address()?;
        let state_init = if seqno == 0 {
            Some(self.state_init_cell()?)
        } else {
            None
        };

        let mut ext = CellBuilder::new();
        // ext_in_msg_info$10 src:addr_none$00 dest:MsgAddressInt import_fee:Coins(0)
        ext.store_uint(0b10, 2)?;
        ext.store_addr_none()?;
        ext.store_address(&dest)?;
        ext.store_coins(0)?;
        // init:(Maybe (Either StateInit ^StateInit))
        match state_init {
            Some(init) => {
                ext.store_bit(true)?; // Just
                ext.store_bit(true)?; // Right (^StateInit)
                ext.store_ref(init)?;
            }
            None => {
                ext.store_bit(false)?;
            }
        }
        // body:(Either X ^X) -> Right (^Cell)
        ext.store_bit(true)?;
        ext.store_ref(Arc::clone(&body))?;

        let external_message = Arc::new(ext.end_cell()?);
        let message_hash = external_message.hash();
        let boc_base64 = serialize_boc_base64(&external_message)?;

        Ok(SignedTransfer {
            external_message,
            body,
            signature: sig_bytes,
            message_hash,
            boc_base64,
        })
    }

    /// Builds an unsigned external-in BOC with an all-zero dummy signature (`[0u8; 64]`) for
    /// `ignore_chksig = true` fee estimation and pre-approval transaction emulation ONLY.
    ///
    /// Never accesses or uses any real wallet private key, so the returned BOC cannot verify
    /// under `self.public_key` and is strictly unbroadcastable on-chain.
    pub fn create_unsigned_emulation_transfer(
        &self,
        seqno: u32,
        valid_until: u32,
        messages: &[TransferMessage],
    ) -> Result<SignedTransfer, WalletError> {
        let signing = self.build_signing_payload(seqno, valid_until, messages)?;
        self.wrap_external_transfer(seqno, signing, [0_u8; 64])
    }

    /// Signs a transfer with `keypair` and wraps it into an external-in BOC message.
    ///
    /// Automatically attaches `StateInit` when `seqno == 0` (initial wallet deployment).
    pub fn create_signed_transfer(
        &self,
        keypair: &TonKeypair,
        seqno: u32,
        valid_until: u32,
        messages: &[TransferMessage],
    ) -> Result<SignedTransfer, WalletError> {
        if keypair.public_key_bytes() != self.public_key {
            return Err(WalletError::Contract(
                "signing keypair does not match wallet public key".to_owned(),
            ));
        }

        let signing = self.build_signing_payload(seqno, valid_until, messages)?;
        let signing_hash = signing.clone().end_cell()?.hash();
        let sig = keypair.signing_key().sign(&signing_hash);
        self.wrap_external_transfer(seqno, signing, sig.to_bytes())
    }

    /// Verifies that a [`SignedTransfer`]'s Ed25519 signature matches `self.public_key` and
    /// covers the exact signing payload hash.
    pub fn verify_signed_transfer(
        &self,
        signed: &SignedTransfer,
        seqno: u32,
        valid_until: u32,
        messages: &[TransferMessage],
    ) -> Result<(), WalletError> {
        let vk = VerifyingKey::from_bytes(&self.public_key)
            .map_err(|e| WalletError::Contract(format!("invalid public key: {e}")))?;
        let signing = self.build_signing_payload(seqno, valid_until, messages)?;
        let hash = signing.end_cell()?.hash();
        let sig = Signature::from_bytes(&signed.signature);
        vk.verify(&hash, &sig)
            .map_err(|e| WalletError::Contract(format!("signature verification failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boc::{deserialize_boc_base64, text_comment_from_boc};
    use crate::mnemonic::mnemonic_to_keypair;

    #[test]
    fn official_wallet_contract_code_hashes() {
        assert_eq!(
            hex::encode(WalletVersion::V3R2.code_cell().unwrap().hash()),
            "84dafa449f98a6987789ba232358072bc0f76dc4524002a5d0918b9a75d2d599"
        );
        assert_eq!(
            hex::encode(WalletVersion::V4R2.code_cell().unwrap().hash()),
            "feb5ff6820e2ff0d9483e7e0d62c817d846789fb4ae580c878866d959dabd5c0"
        );
        assert_eq!(
            hex::encode(WalletVersion::V5R1.code_cell().unwrap().hash()),
            "20834b7b72b112147e1b2fb457b84e74d1a30f04f737d4f62a668e9552d2b72f"
        );
    }

    #[test]
    fn transfer_signing_and_verification_all_versions() {
        // Synthetic test vector mnemonic from public TON spec (no real funds).
        let words: Vec<String> = [
            "hospital", "stove", "relief", "fringe", "tongue", "always", "charge", "angry", "urge",
            "sentence", "again", "match", "nerve", "inquiry", "senior", "coconut", "label",
            "tumble", "carry", "category", "beauty", "bean", "road", "solution",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        let kp = mnemonic_to_keypair(&words, "").unwrap();
        let recipient = TonAddress::new(0, [0x22; 32]);
        let msg =
            TransferMessage::simple(recipient, 250_000_000, false, Some("Test note")).unwrap();

        for version in [
            WalletVersion::V3R2,
            WalletVersion::V4R2,
            WalletVersion::V5R1,
        ] {
            let contract = WalletContract::new(version, 0, kp.public_key_bytes(), false);
            let addr = contract.address().unwrap();
            assert_eq!(addr.workchain, 0);

            // Seqno 0 attaches StateInit; seqno 5 omits it.
            for seqno in [0_u32, 5_u32] {
                let signed = contract
                    .create_signed_transfer(&kp, seqno, 1_800_000_000, std::slice::from_ref(&msg))
                    .unwrap();
                contract
                    .verify_signed_transfer(
                        &signed,
                        seqno,
                        1_800_000_000,
                        std::slice::from_ref(&msg),
                    )
                    .unwrap();

                // Round-trip BOC deserialization preserves exact external message hash.
                let decoded = deserialize_boc_base64(&signed.boc_base64).unwrap();
                assert_eq!(decoded.hash(), signed.message_hash);

                // Wrong seqno fails verification.
                assert!(
                    contract
                        .verify_signed_transfer(
                            &signed,
                            seqno + 1,
                            1_800_000_000,
                            std::slice::from_ref(&msg)
                        )
                        .is_err()
                );
            }
        }

        // Verify comment body decodes cleanly.
        let body_boc = serialize_boc_base64(msg.body.as_ref().unwrap()).unwrap();
        assert_eq!(
            text_comment_from_boc(&body_boc).as_deref(),
            Some("Test note")
        );
    }
}
