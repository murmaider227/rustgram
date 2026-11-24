//! TON TVM Cell, [`CellBuilder`], [`CellSlice`], and canonical representation hash (SHA-256).
//!
//! Implements the TON TVM cell specification: up to 1023 bits and 4 child references per cell,
//! descriptor bytes `d1`/`d2`, bit-padded completion tag, recursive depth and representation
//! hash, VarUInt16 `Coins`, `MsgAddressInt`, and snake-encoded text comments.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::WalletError;
use crate::address::TonAddress;

/// Maximum number of data bits in a single TVM cell.
pub const MAX_CELL_BITS: usize = 1023;
/// Maximum number of child references in a single TVM cell.
pub const MAX_CELL_REFS: usize = 4;

/// An immutable TVM cell containing up to 1023 bits and up to 4 references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Bit data packed MSB-first (unused trailing bits in the last byte are zeroed).
    data: Vec<u8>,
    /// Number of valid bits in `data` (`0..=1023`).
    bit_len: usize,
    /// Child cell references (`0..=4`).
    refs: Vec<Arc<Cell>>,
    /// Whether this cell is an exotic cell (pruned branch, library, merkle proof/update).
    exotic: bool,
}

impl Cell {
    /// Creates an empty ordinary cell (0 bits, 0 refs).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            data: Vec::new(),
            bit_len: 0,
            refs: Vec::new(),
            exotic: false,
        }
    }

    /// Creates a validated cell from raw MSB-packed bytes, bit count, references, and exotic flag.
    pub fn new(
        mut data: Vec<u8>,
        bit_len: usize,
        refs: Vec<Arc<Cell>>,
        exotic: bool,
    ) -> Result<Self, WalletError> {
        if bit_len > MAX_CELL_BITS {
            return Err(WalletError::Cell(format!(
                "cell bit length {bit_len} exceeds {MAX_CELL_BITS}"
            )));
        }
        if refs.len() > MAX_CELL_REFS {
            return Err(WalletError::Cell(format!(
                "cell reference count {} exceeds {MAX_CELL_REFS}",
                refs.len()
            )));
        }
        let needed_bytes = bit_len.div_ceil(8);
        if data.len() < needed_bytes {
            return Err(WalletError::Cell(format!(
                "data buffer too short ({}) for {bit_len} bits",
                data.len()
            )));
        }
        data.truncate(needed_bytes);
        let rem = bit_len % 8;
        if rem != 0
            && let Some(last) = data.last_mut()
        {
            let mask = !((1_u8 << (8 - rem)) - 1);
            *last &= mask;
        }
        Ok(Self {
            data,
            bit_len,
            refs,
            exotic,
        })
    }

    /// Raw MSB-packed bit storage (zero-padded in any unused trailing bits).
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Number of valid bits in this cell.
    #[must_use]
    pub const fn bit_len(&self) -> usize {
        self.bit_len
    }

    /// Child references of this cell.
    #[must_use]
    pub fn refs(&self) -> &[Arc<Cell>] {
        &self.refs
    }

    /// Whether this is an exotic cell.
    #[must_use]
    pub const fn is_exotic(&self) -> bool {
        self.exotic
    }

    /// Level mask of an ordinary cell (bitwise OR of child level masks; `0` for standard cells).
    #[must_use]
    pub fn level_mask(&self) -> u8 {
        if self.exotic {
            // For pruned branch (type 1), level mask is stored in byte 1; otherwise 0.
            if self.bit_len >= 16 && self.data.first().copied() == Some(1) {
                return self.data[1];
            }
            return 0;
        }
        self.refs.iter().fold(0_u8, |acc, r| acc | r.level_mask())
    }

    /// First descriptor byte `d1 = refs_count + 8 * is_exotic + 32 * level_mask`.
    #[must_use]
    pub fn d1(&self) -> u8 {
        (self.refs.len() as u8) | (u8::from(self.exotic) << 3) | (self.level_mask() << 5)
    }

    /// Second descriptor byte `d2 = floor(bit_len / 8) + ceil(bit_len / 8)`.
    #[must_use]
    pub const fn d2(&self) -> u8 {
        ((self.bit_len / 8) + self.bit_len.div_ceil(8)) as u8
    }

    /// Returns the bit-padded payload bytes used for both BOC encoding and cell hashing.
    ///
    /// When `bit_len` is not a multiple of 8, a single `1` completion bit followed by `0` bits is
    /// appended to fill the final byte.
    #[must_use]
    pub fn padded_data(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        let rem = self.bit_len % 8;
        if rem != 0
            && let Some(last) = out.last_mut()
        {
            *last |= 1_u8 << (7 - rem);
        }
        out
    }

    /// Maximum depth of the cell subtree (`0` for a leaf cell, `1 + max(child.depth())` otherwise).
    #[must_use]
    pub fn depth(&self) -> u16 {
        self.refs
            .iter()
            .map(|r| r.depth().saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    /// Computes the canonical 256-bit TVM representation hash (`SHA-256`) of this cell.
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update([self.d1(), self.d2()]);
        hasher.update(self.padded_data());
        for child in &self.refs {
            hasher.update(child.depth().to_be_bytes());
        }
        for child in &self.refs {
            hasher.update(child.hash());
        }
        let digest = hasher.finalize();
        let mut out = [0_u8; 32];
        out.copy_from_slice(&digest);
        out
    }

    /// Opens a [`CellSlice`] cursor at bit 0, ref 0 for reading this cell.
    #[must_use]
    pub fn begin_parse(&self) -> CellSlice<'_> {
        CellSlice {
            cell: self,
            bit_pos: 0,
            ref_pos: 0,
        }
    }
}

/// Mutable builder for constructing a TVM [`Cell`].
#[derive(Debug, Clone, Default)]
pub struct CellBuilder {
    data: Vec<u8>,
    bit_len: usize,
    refs: Vec<Arc<Cell>>,
    exotic: bool,
}

impl CellBuilder {
    /// Creates an empty [`CellBuilder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of bits written so far.
    #[must_use]
    pub const fn bit_len(&self) -> usize {
        self.bit_len
    }

    /// Number of remaining free bits (`1023 - bit_len`).
    #[must_use]
    pub const fn remaining_bits(&self) -> usize {
        MAX_CELL_BITS - self.bit_len
    }

    /// Number of child references stored so far.
    #[must_use]
    pub fn ref_count(&self) -> usize {
        self.refs.len()
    }

    /// Stores a single bit (`true` = `1`, `false` = `0`).
    pub fn store_bit(&mut self, bit: bool) -> Result<&mut Self, WalletError> {
        if self.bit_len >= MAX_CELL_BITS {
            return Err(WalletError::Cell("cell bit overflow".to_owned()));
        }
        let byte_idx = self.bit_len / 8;
        let bit_offset = 7 - (self.bit_len % 8);
        if byte_idx >= self.data.len() {
            self.data.push(0);
        }
        if bit {
            self.data[byte_idx] |= 1_u8 << bit_offset;
        }
        self.bit_len += 1;
        Ok(self)
    }

    /// Stores the lowest `bits` bits of `value` in big-endian bit order (`0..=64`).
    pub fn store_uint(&mut self, value: u64, bits: usize) -> Result<&mut Self, WalletError> {
        if bits > 64 {
            return Err(WalletError::Cell(format!(
                "store_uint bits {bits} exceeds 64"
            )));
        }
        if self.bit_len + bits > MAX_CELL_BITS {
            return Err(WalletError::Cell("cell bit overflow".to_owned()));
        }
        for i in (0..bits).rev() {
            let bit = ((value >> i) & 1) != 0;
            self.store_bit(bit)?;
        }
        Ok(self)
    }

    /// Stores a signed two's-complement integer of `bits` width (`1..=64`).
    pub fn store_int(&mut self, value: i64, bits: usize) -> Result<&mut Self, WalletError> {
        if bits == 0 || bits > 64 {
            return Err(WalletError::Cell(format!(
                "store_int bits {bits} out of range 1..=64"
            )));
        }
        self.store_uint(value as u64, bits)
    }

    /// Stores a slice of whole bytes MSB-first.
    pub fn store_bytes(&mut self, bytes: &[u8]) -> Result<&mut Self, WalletError> {
        if self.bit_len + bytes.len() * 8 > MAX_CELL_BITS {
            return Err(WalletError::Cell("cell bit overflow".to_owned()));
        }
        if self.bit_len.is_multiple_of(8) {
            self.data.extend_from_slice(bytes);
            self.bit_len += bytes.len() * 8;
        } else {
            for &b in bytes {
                self.store_uint(u64::from(b), 8)?;
            }
        }
        Ok(self)
    }

    /// Stores all bits and references from another [`CellBuilder`].
    pub fn store_builder(&mut self, other: &Self) -> Result<&mut Self, WalletError> {
        if self.bit_len + other.bit_len > MAX_CELL_BITS {
            return Err(WalletError::Cell("cell bit overflow".to_owned()));
        }
        if self.refs.len() + other.refs.len() > MAX_CELL_REFS {
            return Err(WalletError::Cell("cell ref overflow".to_owned()));
        }
        for i in 0..other.bit_len {
            let b = (other.data[i / 8] >> (7 - (i % 8))) & 1;
            self.store_bit(b != 0)?;
        }
        for r in &other.refs {
            self.refs.push(Arc::clone(r));
        }
        Ok(self)
    }

    /// Appends a child cell reference.
    pub fn store_ref(&mut self, cell: Arc<Cell>) -> Result<&mut Self, WalletError> {
        if self.refs.len() >= MAX_CELL_REFS {
            return Err(WalletError::Cell("cell ref overflow".to_owned()));
        }
        self.refs.push(cell);
        Ok(self)
    }

    /// Stores a `Maybe ^Cell` (`0` if `None`, or `1` + reference if `Some`).
    pub fn store_maybe_ref(&mut self, cell: Option<Arc<Cell>>) -> Result<&mut Self, WalletError> {
        match cell {
            Some(c) => {
                self.store_bit(true)?;
                self.store_ref(c)
            }
            None => self.store_bit(false),
        }
    }

    /// Stores a `VarUInt 16` TON `Coins` value (4-bit byte length `0..=15` + big-endian bytes).
    pub fn store_coins(&mut self, value: u128) -> Result<&mut Self, WalletError> {
        if value == 0 {
            return self.store_uint(0, 4);
        }
        let bytes = value.to_be_bytes();
        let first_nonzero = bytes.iter().position(|&b| b != 0).unwrap_or(15);
        let len = 16 - first_nonzero;
        if len > 15 {
            return Err(WalletError::Cell("coins value exceeds 120 bits".to_owned()));
        }
        self.store_uint(len as u64, 4)?;
        self.store_bytes(&bytes[first_nonzero..])
    }

    /// Stores `addr_none$00`.
    pub fn store_addr_none(&mut self) -> Result<&mut Self, WalletError> {
        self.store_uint(0, 2)
    }

    /// Stores `addr_std$10 anycast:(Maybe Anycast)=0 workchain_id:int8 address:bits256`.
    pub fn store_address(&mut self, addr: &TonAddress) -> Result<&mut Self, WalletError> {
        self.store_uint(0b10, 2)?;
        self.store_bit(false)?; // no anycast
        self.store_int(i64::from(addr.workchain), 8)?;
        self.store_bytes(&addr.hash)
    }

    /// Stores a zero-opcode (`0x00000000`) UTF-8 text comment, chaining into snake cells if long.
    pub fn store_text_comment(&mut self, text: &str) -> Result<&mut Self, WalletError> {
        self.store_uint(0, 32)?;
        self.store_snake_bytes(text.as_bytes())
    }

    /// Stores arbitrarily long byte slices using TON's snake-cell chain format.
    pub fn store_snake_bytes(&mut self, bytes: &[u8]) -> Result<&mut Self, WalletError> {
        let max_here = self.remaining_bits() / 8;
        if bytes.len() <= max_here {
            return self.store_bytes(bytes);
        }
        self.store_bytes(&bytes[..max_here])?;
        let mut next = Self::new();
        next.store_snake_bytes(&bytes[max_here..])?;
        self.store_ref(Arc::new(next.end_cell()?))
    }

    /// Finalizes the builder into an immutable [`Cell`].
    pub fn end_cell(self) -> Result<Cell, WalletError> {
        Cell::new(self.data, self.bit_len, self.refs, self.exotic)
    }
}

/// Read cursor over a [`Cell`]'s bits and references.
#[derive(Debug, Clone)]
pub struct CellSlice<'a> {
    cell: &'a Cell,
    bit_pos: usize,
    ref_pos: usize,
}

impl<'a> CellSlice<'a> {
    /// Remaining unread bits in this slice.
    #[must_use]
    pub const fn remaining_bits(&self) -> usize {
        self.cell.bit_len.saturating_sub(self.bit_pos)
    }

    /// Remaining unread references in this slice.
    #[must_use]
    pub fn remaining_refs(&self) -> usize {
        self.cell.refs.len().saturating_sub(self.ref_pos)
    }

    /// Reads one bit.
    pub fn load_bit(&mut self) -> Result<bool, WalletError> {
        if self.bit_pos >= self.cell.bit_len {
            return Err(WalletError::Cell("unexpected end of cell bits".to_owned()));
        }
        let byte = self.cell.data[self.bit_pos / 8];
        let bit = ((byte >> (7 - (self.bit_pos % 8))) & 1) != 0;
        self.bit_pos += 1;
        Ok(bit)
    }

    /// Reads an unsigned integer of `bits` width (`0..=64`).
    pub fn load_uint(&mut self, bits: usize) -> Result<u64, WalletError> {
        if bits > 64 {
            return Err(WalletError::Cell(format!(
                "load_uint bits {bits} exceeds 64"
            )));
        }
        if self.remaining_bits() < bits {
            return Err(WalletError::Cell("unexpected end of cell bits".to_owned()));
        }
        let mut acc = 0_u64;
        for _ in 0..bits {
            acc = (acc << 1) | u64::from(self.load_bit()?);
        }
        Ok(acc)
    }

    /// Reads a signed two's-complement integer of `bits` width (`1..=64`).
    pub fn load_int(&mut self, bits: usize) -> Result<i64, WalletError> {
        if bits == 0 || bits > 64 {
            return Err(WalletError::Cell(format!(
                "load_int bits {bits} out of range 1..=64"
            )));
        }
        let raw = self.load_uint(bits)?;
        if bits == 64 {
            Ok(raw as i64)
        } else {
            let sign_bit = 1_u64 << (bits - 1);
            if (raw & sign_bit) != 0 {
                Ok((raw | (!0_u64 << bits)) as i64)
            } else {
                Ok(raw as i64)
            }
        }
    }

    /// Reads `len` whole bytes.
    pub fn load_bytes(&mut self, len: usize) -> Result<Vec<u8>, WalletError> {
        if self.remaining_bits() < len * 8 {
            return Err(WalletError::Cell("unexpected end of cell bits".to_owned()));
        }
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(self.load_uint(8)? as u8);
        }
        Ok(out)
    }

    /// Reads a `VarUInt 16` TON `Coins` value.
    pub fn load_coins(&mut self) -> Result<u128, WalletError> {
        let len = self.load_uint(4)? as usize;
        if len == 0 {
            return Ok(0);
        }
        let bytes = self.load_bytes(len)?;
        let mut acc = 0_u128;
        for b in bytes {
            acc = (acc << 8) | u128::from(b);
        }
        Ok(acc)
    }

    /// Reads a `MsgAddress`: returns `Ok(None)` for `addr_none$00` or `Ok(Some(addr))` for
    /// standard `addr_std$10` without anycast.
    pub fn load_address(&mut self) -> Result<Option<TonAddress>, WalletError> {
        let tag = self.load_uint(2)?;
        match tag {
            0b00 => Ok(None),
            0b10 => {
                let anycast = self.load_bit()?;
                if anycast {
                    return Err(WalletError::Cell(
                        "anycast address not supported".to_owned(),
                    ));
                }
                let workchain = self.load_int(8)? as i8;
                let hash_bytes = self.load_bytes(32)?;
                let mut hash = [0_u8; 32];
                hash.copy_from_slice(&hash_bytes);
                Ok(Some(TonAddress::new(workchain, hash)))
            }
            _ => Err(WalletError::Cell(format!(
                "unsupported MsgAddress tag 0b{tag:02b}"
            ))),
        }
    }

    /// Reads the next child reference.
    pub fn load_ref(&mut self) -> Result<&'a Arc<Cell>, WalletError> {
        let r = self
            .cell
            .refs
            .get(self.ref_pos)
            .ok_or_else(|| WalletError::Cell("unexpected end of cell refs".to_owned()))?;
        self.ref_pos += 1;
        Ok(r)
    }

    /// Reads a `Maybe ^Cell`.
    pub fn load_maybe_ref(&mut self) -> Result<Option<&'a Arc<Cell>>, WalletError> {
        if self.load_bit()? {
            Ok(Some(self.load_ref()?))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cell_known_hash() {
        // Universal TON spec vector: SHA-256([0x00, 0x00])
        let empty = Cell::empty();
        assert_eq!(empty.d1(), 0);
        assert_eq!(empty.d2(), 0);
        assert_eq!(
            hex::encode(empty.hash()),
            "96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7"
        );
    }

    #[test]
    fn builder_and_slice_roundtrip() {
        let addr = TonAddress::new(-1, [0x77; 32]);
        let mut b = CellBuilder::new();
        b.store_bit(true).unwrap();
        b.store_uint(0x1234, 16).unwrap();
        b.store_int(-42, 8).unwrap();
        b.store_coins(1_500_000_000).unwrap();
        b.store_address(&addr).unwrap();
        let child = Arc::new(Cell::empty());
        b.store_ref(child).unwrap();
        let cell = b.end_cell().unwrap();
        assert_eq!(cell.depth(), 1);

        let mut s = cell.begin_parse();
        assert!(s.load_bit().unwrap());
        assert_eq!(s.load_uint(16).unwrap(), 0x1234);
        assert_eq!(s.load_int(8).unwrap(), -42);
        assert_eq!(s.load_coins().unwrap(), 1_500_000_000);
        assert_eq!(s.load_address().unwrap(), Some(addr));
        assert_eq!(s.load_ref().unwrap().bit_len(), 0);
        assert_eq!(s.remaining_bits(), 0);
        assert_eq!(s.remaining_refs(), 0);
    }
}
