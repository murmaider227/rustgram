//! Bag-of-Cells (BOC) serialization, deserialization, and tdesktop `gram_boc` inspection helpers.
//!
//! Port origin: `Telegram/SourceFiles/gram/gram_boc.h`, `Telegram/SourceFiles/gram/gram_boc.cpp`,
//! and `Telegram/SourceFiles/gram/tests/gram_boc_tests.cpp`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rand_core::{OsRng, RngCore};

use crate::WalletError;
use crate::cell::Cell;

/// Standard TON BOC magic prefix (`0xB5EE9C72`).
pub const BOC_MAGIC: u32 = 0xB5EE_9C72;
/// Generic BOC magic with index (`0x68FF65F3`).
pub const BOC_MAGIC_INDEXED: u32 = 0x68FF_65F3;
/// Generic BOC magic with index + CRC32C (`0xACC3A728`).
pub const BOC_MAGIC_INDEXED_CRC32C: u32 = 0xACC3_A728;
/// Maximum number of cells accepted by [`deserialize_boc`] (prevents untrusted memory/CPU DoS).
pub const MAX_BOC_CELLS: usize = 65_536;

const SIGNATURE_BYTES: usize = 64;
const REQUEST_BYTES: usize = SIGNATURE_BYTES + 4 + 4 + 4 + 4 + 32;
const REQUEST_REFS: usize = 1;
const EXTERNAL_ROTATION_OPCODE: u32 = 0xFBBA_99C8;
const INTERNAL_ROTATION_OPCODE: u32 = 0xFBBA_99C7;
const MAX_INSPECT_CELLS: usize = 64;
const MAX_INSPECT_BYTES: usize = 64 * 1024;
const NFT_TRANSFER_OPCODE: u32 = 0x5FCC_3D14;
const NFT_TRANSFER_OWNER_BITS: usize = 32 + 64 + 2 + 1 + 8 + 256;

/// Options controlling [`serialize_boc_with_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BocSerializeOptions {
    /// Include cell byte offset index table (`has_idx`).
    pub has_idx: bool,
    /// Append little-endian CRC32C checksum (`has_crc32c`).
    pub has_crc32c: bool,
}

impl Default for BocSerializeOptions {
    fn default() -> Self {
        Self {
            has_idx: false,
            has_crc32c: true,
        }
    }
}

/// Computes Castagnoli CRC32C (`0x82F63B78`), matching `Crc32c` in `gram_boc.cpp:172-183`.
#[must_use]
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0x82F6_3B78;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

fn bytes_needed_for(value: usize) -> usize {
    if value < 0x100 {
        1
    } else if value < 0x1_0000 {
        2
    } else if value < 0x100_0000 {
        3
    } else {
        4
    }
}

fn write_be_uint(out: &mut Vec<u8>, value: u64, size: usize) {
    for shift in (0..size).rev() {
        out.push(((value >> (shift * 8)) & 0xFF) as u8);
    }
}

fn read_be_uint(data: &[u8], offset: &mut usize, size: usize) -> Result<u64, WalletError> {
    if size == 0 || size > 8 || data.len().saturating_sub(*offset) < size {
        return Err(WalletError::Boc(
            "unexpected EOF reading integer".to_owned(),
        ));
    }
    let mut val = 0_u64;
    for i in 0..size {
        val = (val << 8) | u64::from(data[*offset + i]);
    }
    *offset += size;
    Ok(val)
}

/// Serializes a single root [`Cell`] into a TON Bag-of-Cells byte vector (with CRC32C by default).
pub fn serialize_boc(root: &Cell) -> Result<Vec<u8>, WalletError> {
    serialize_boc_with_options(root, BocSerializeOptions::default())
}

/// Serializes a single root [`Cell`] into standard padded base64 BOC.
pub fn serialize_boc_base64(root: &Cell) -> Result<String, WalletError> {
    Ok(STANDARD.encode(serialize_boc(root)?))
}

/// Serializes a single root [`Cell`] into a TON BOC with explicit header flags.
pub fn serialize_boc_with_options(
    root: &Cell,
    opts: BocSerializeOptions,
) -> Result<Vec<u8>, WalletError> {
    // Iterative topological post-order deduplication by cell representation hash, then reverse so
    // parents strictly precede their children (required by TON BOC spec) without call-stack depth
    // proportional to cell depth.
    let mut visited: HashSet<[u8; 32]> = HashSet::new();
    let mut post_order: Vec<Arc<Cell>> = Vec::new();
    let root_arc = Arc::new(root.clone());
    let mut stack: Vec<(Arc<Cell>, bool)> = vec![(root_arc, false)];
    while let Some((cell, expanded)) = stack.pop() {
        if expanded {
            post_order.push(cell);
            continue;
        }
        if !visited.insert(cell.hash()) {
            continue;
        }
        stack.push((Arc::clone(&cell), true));
        for r in cell.refs().iter().rev() {
            if !visited.contains(&r.hash()) {
                stack.push((Arc::clone(r), false));
            }
        }
    }
    post_order.reverse();

    let mut index_by_hash: HashMap<[u8; 32], usize> = HashMap::with_capacity(post_order.len());
    for (idx, cell) in post_order.iter().enumerate() {
        index_by_hash.insert(cell.hash(), idx);
    }

    let cell_count = post_order.len();
    let ref_size = bytes_needed_for(cell_count);

    // Encode all cells and collect cumulative end offsets for the optional index table.
    let mut cells_data = Vec::new();
    let mut offsets = Vec::with_capacity(cell_count);
    for cell in &post_order {
        cells_data.push(cell.d1());
        cells_data.push(cell.d2());
        cells_data.extend_from_slice(&cell.padded_data());
        for child in cell.refs() {
            let child_idx = *index_by_hash
                .get(&child.hash())
                .ok_or_else(|| WalletError::Boc("missing child index".to_owned()))?;
            write_be_uint(&mut cells_data, child_idx as u64, ref_size);
        }
        offsets.push(cells_data.len());
    }

    let offset_size = bytes_needed_for(cells_data.len());
    let flags: u8 =
        (u8::from(opts.has_idx) << 7) | (u8::from(opts.has_crc32c) << 6) | (ref_size as u8 & 0x07);

    let mut out = Vec::new();
    out.extend_from_slice(&BOC_MAGIC.to_be_bytes());
    out.push(flags);
    out.push(offset_size as u8);
    write_be_uint(&mut out, cell_count as u64, ref_size);
    write_be_uint(&mut out, 1, ref_size); // roots = 1
    write_be_uint(&mut out, 0, ref_size); // absent = 0
    write_be_uint(&mut out, cells_data.len() as u64, offset_size);
    write_be_uint(&mut out, 0, ref_size); // root_list[0] = 0

    if opts.has_idx {
        for off in offsets {
            write_be_uint(&mut out, off as u64, offset_size);
        }
    }
    out.extend_from_slice(&cells_data);

    if opts.has_crc32c {
        let crc = crc32c(&out);
        out.extend_from_slice(&crc.to_le_bytes());
    }
    Ok(out)
}

/// Deserializes a TON Bag-of-Cells byte slice and returns its root cells.
pub fn deserialize_boc(data: &[u8]) -> Result<Vec<Arc<Cell>>, WalletError> {
    if data.len() < 10 {
        return Err(WalletError::Boc("BOC too short".to_owned()));
    }
    let mut offset = 0;
    let magic = read_be_uint(data, &mut offset, 4)? as u32;

    let (
        has_idx,
        has_crc,
        _has_cache_bits,
        ref_size,
        offset_size,
        cells_count,
        roots_count,
        absent_count,
        total_cells_size,
        root_indices,
    ) = match magic {
        BOC_MAGIC => {
            let flags = read_be_uint(data, &mut offset, 1)? as u8;
            let has_idx = (flags & 0x80) != 0;
            let has_crc = (flags & 0x40) != 0;
            let has_cache_bits = (flags & 0x20) != 0;
            let ref_size = (flags & 0x07) as usize;
            if ref_size == 0 || ref_size > 4 {
                return Err(WalletError::Boc(format!("invalid ref_size {ref_size}")));
            }
            let offset_size = read_be_uint(data, &mut offset, 1)? as usize;
            if offset_size == 0 || offset_size > 8 {
                return Err(WalletError::Boc(format!(
                    "invalid offset_size {offset_size}"
                )));
            }
            let cells = usize::try_from(read_be_uint(data, &mut offset, ref_size)?)
                .map_err(|_| WalletError::Boc("BOC cells_count overflow".to_owned()))?;
            let roots = usize::try_from(read_be_uint(data, &mut offset, ref_size)?)
                .map_err(|_| WalletError::Boc("BOC roots_count overflow".to_owned()))?;
            let absent = usize::try_from(read_be_uint(data, &mut offset, ref_size)?)
                .map_err(|_| WalletError::Boc("BOC absent_count overflow".to_owned()))?;
            let total_size = usize::try_from(read_be_uint(data, &mut offset, offset_size)?)
                .map_err(|_| WalletError::Boc("BOC total_cells_size overflow".to_owned()))?;
            if absent != 0 {
                return Err(WalletError::Boc("absent cells not supported".to_owned()));
            }
            if cells == 0 || roots == 0 || roots > cells {
                return Err(WalletError::Boc(
                    "BOC has invalid cells or roots count".to_owned(),
                ));
            }
            if cells > MAX_BOC_CELLS {
                return Err(WalletError::Boc(format!(
                    "BOC cells_count {cells} exceeds limit {MAX_BOC_CELLS}"
                )));
            }
            let remaining = data.len().saturating_sub(offset);
            let roots_bytes = roots
                .checked_mul(ref_size)
                .ok_or_else(|| WalletError::Boc("BOC roots table size overflow".to_owned()))?;
            let min_cells_bytes = cells
                .checked_mul(2)
                .ok_or_else(|| WalletError::Boc("BOC cells_count overflow".to_owned()))?;
            let min_required = roots_bytes
                .checked_add(min_cells_bytes)
                .ok_or_else(|| WalletError::Boc("BOC header size overflow".to_owned()))?;
            if remaining < min_required {
                return Err(WalletError::Boc(
                    "BOC counts exceed remaining input bytes".to_owned(),
                ));
            }
            let mut root_list = Vec::with_capacity(roots);
            for _ in 0..roots {
                let idx = usize::try_from(read_be_uint(data, &mut offset, ref_size)?)
                    .map_err(|_| WalletError::Boc("BOC root index overflow".to_owned()))?;
                root_list.push(idx);
            }
            (
                has_idx,
                has_crc,
                has_cache_bits,
                ref_size,
                offset_size,
                cells,
                roots,
                absent,
                total_size,
                root_list,
            )
        }
        BOC_MAGIC_INDEXED | BOC_MAGIC_INDEXED_CRC32C => {
            let ref_size = read_be_uint(data, &mut offset, 1)? as usize;
            if ref_size == 0 || ref_size > 4 {
                return Err(WalletError::Boc(format!("invalid ref_size {ref_size}")));
            }
            let offset_size = read_be_uint(data, &mut offset, 1)? as usize;
            if offset_size == 0 || offset_size > 8 {
                return Err(WalletError::Boc(format!(
                    "invalid offset_size {offset_size}"
                )));
            }
            let cells = usize::try_from(read_be_uint(data, &mut offset, ref_size)?)
                .map_err(|_| WalletError::Boc("BOC cells_count overflow".to_owned()))?;
            let roots = usize::try_from(read_be_uint(data, &mut offset, ref_size)?)
                .map_err(|_| WalletError::Boc("BOC roots_count overflow".to_owned()))?;
            let absent = usize::try_from(read_be_uint(data, &mut offset, ref_size)?)
                .map_err(|_| WalletError::Boc("BOC absent_count overflow".to_owned()))?;
            let total_size = usize::try_from(read_be_uint(data, &mut offset, offset_size)?)
                .map_err(|_| WalletError::Boc("BOC total_cells_size overflow".to_owned()))?;
            if absent != 0 {
                return Err(WalletError::Boc("absent cells not supported".to_owned()));
            }
            if cells == 0 || roots == 0 || roots > cells {
                return Err(WalletError::Boc(
                    "BOC has invalid cells or roots count".to_owned(),
                ));
            }
            if cells > MAX_BOC_CELLS {
                return Err(WalletError::Boc(format!(
                    "BOC cells_count {cells} exceeds limit {MAX_BOC_CELLS}"
                )));
            }
            let remaining = data.len().saturating_sub(offset);
            let min_per_cell = offset_size
                .checked_add(2)
                .ok_or_else(|| WalletError::Boc("BOC cell stride overflow".to_owned()))?;
            let min_required = cells
                .checked_mul(min_per_cell)
                .ok_or_else(|| WalletError::Boc("BOC cells_count overflow".to_owned()))?;
            if remaining < min_required {
                return Err(WalletError::Boc(
                    "BOC counts exceed remaining input bytes".to_owned(),
                ));
            }
            let root_list = (0..roots).collect();
            (
                true,
                magic == BOC_MAGIC_INDEXED_CRC32C,
                false,
                ref_size,
                offset_size,
                cells,
                roots,
                absent,
                total_size,
                root_list,
            )
        }
        _ => {
            return Err(WalletError::Boc(format!("unknown BOC magic 0x{magic:08x}")));
        }
    };

    if absent_count != 0 {
        return Err(WalletError::Boc("absent cells not supported".to_owned()));
    }
    if cells_count == 0 || roots_count == 0 || roots_count > cells_count {
        return Err(WalletError::Boc("BOC has no cells or roots".to_owned()));
    }
    for &r in &root_indices {
        if r >= cells_count {
            return Err(WalletError::Boc("root index out of bounds".to_owned()));
        }
    }

    if has_idx {
        let index_bytes = cells_count
            .checked_mul(offset_size)
            .ok_or_else(|| WalletError::Boc("index size overflow".to_owned()))?;
        if data.len().saturating_sub(offset) < index_bytes {
            return Err(WalletError::Boc("truncated index table".to_owned()));
        }
        offset = offset
            .checked_add(index_bytes)
            .ok_or_else(|| WalletError::Boc("index offset overflow".to_owned()))?;
    }

    let crc_tail = if has_crc { 4_usize } else { 0_usize };
    let expected_tail = total_cells_size
        .checked_add(crc_tail)
        .ok_or_else(|| WalletError::Boc("BOC payload size overflow".to_owned()))?;
    if data.len().saturating_sub(offset) != expected_tail {
        return Err(WalletError::Boc(format!(
            "BOC payload length mismatch: expected {}, got {}",
            expected_tail,
            data.len().saturating_sub(offset)
        )));
    }

    let min_cells_bytes = cells_count
        .checked_mul(2)
        .ok_or_else(|| WalletError::Boc("BOC cells_count overflow".to_owned()))?;
    if min_cells_bytes > total_cells_size {
        return Err(WalletError::Boc(
            "BOC cells_count exceeds total_cells_size".to_owned(),
        ));
    }

    if has_crc {
        let crc_pos = data.len() - 4;
        let stored = u32::from_le_bytes([
            data[crc_pos],
            data[crc_pos + 1],
            data[crc_pos + 2],
            data[crc_pos + 3],
        ]);
        let actual = crc32c(&data[..crc_pos]);
        if stored != actual {
            return Err(WalletError::Boc(format!(
                "BOC CRC32C mismatch: expected 0x{actual:08x}, got 0x{stored:08x}"
            )));
        }
    }

    struct RawCell {
        data: Vec<u8>,
        bit_len: usize,
        refs: Vec<usize>,
        exotic: bool,
    }

    let cells_end = offset
        .checked_add(total_cells_size)
        .ok_or_else(|| WalletError::Boc("BOC cells_end overflow".to_owned()))?;
    let mut raw_cells = Vec::with_capacity(cells_count);
    for i in 0..cells_count {
        if cells_end.saturating_sub(offset) < 2 {
            return Err(WalletError::Boc("truncated cell descriptors".to_owned()));
        }
        let d1 = data[offset];
        let d2 = data[offset + 1];
        offset += 2;

        let refs_count = (d1 & 0x07) as usize;
        let exotic = (d1 & 0x08) != 0;
        let has_hashes = (d1 & 0x10) != 0;
        let level_mask = d1 >> 5;
        if refs_count > 4 {
            return Err(WalletError::Boc("cell ref count > 4".to_owned()));
        }

        if has_hashes {
            let hashes_count = (level_mask.count_ones() as usize) + 1;
            let skip = hashes_count * (32 + 2);
            if cells_end.saturating_sub(offset) < skip {
                return Err(WalletError::Boc("truncated stored hashes".to_owned()));
            }
            offset += skip;
        }

        let data_size = ((d2 >> 1) + (d2 & 1)) as usize;
        let whole_bytes = (d2 & 1) == 0;
        if cells_end.saturating_sub(offset) < data_size {
            return Err(WalletError::Boc("truncated cell data".to_owned()));
        }
        let cell_bytes = &data[offset..offset + data_size];
        offset += data_size;

        let (clean_bytes, bit_len) = if whole_bytes {
            (cell_bytes.to_vec(), data_size * 8)
        } else {
            let last = *cell_bytes
                .last()
                .ok_or_else(|| WalletError::Boc("empty padded cell".to_owned()))?;
            if (last & 0x7F) == 0 {
                return Err(WalletError::Boc("invalid completion tag byte".to_owned()));
            }
            let trailing_zeros = last.trailing_zeros() as usize;
            let bit_len = data_size * 8 - 1 - trailing_zeros;
            let mut clean = cell_bytes.to_vec();
            let clear_mask = !((1_u8 << (trailing_zeros + 1)) - 1);
            if let Some(b) = clean.last_mut() {
                *b &= clear_mask;
            }
            (clean, bit_len)
        };

        let refs_bytes = refs_count
            .checked_mul(ref_size)
            .ok_or_else(|| WalletError::Boc("cell refs size overflow".to_owned()))?;
        if cells_end.saturating_sub(offset) < refs_bytes {
            return Err(WalletError::Boc("truncated cell references".to_owned()));
        }
        let mut refs = Vec::with_capacity(refs_count);
        for _ in 0..refs_count {
            let r = usize::try_from(read_be_uint(data, &mut offset, ref_size)?)
                .map_err(|_| WalletError::Boc("cell ref index overflow".to_owned()))?;
            if r <= i || r >= cells_count {
                return Err(WalletError::Boc(
                    "invalid cell reference index (cycle or out of order)".to_owned(),
                ));
            }
            refs.push(r);
        }

        raw_cells.push(RawCell {
            data: clean_bytes,
            bit_len,
            refs,
            exotic,
        });
    }

    if offset != cells_end {
        return Err(WalletError::Boc(
            "trailing bytes inside cells section".to_owned(),
        ));
    }

    // Build cells bottom-up (since every ref index strictly exceeds its parent index).
    let mut built: Vec<Option<Arc<Cell>>> = vec![None; cells_count];
    for i in (0..cells_count).rev() {
        let raw = &raw_cells[i];
        let mut child_arcs = Vec::with_capacity(raw.refs.len());
        for &r in &raw.refs {
            let child = built[r]
                .as_ref()
                .ok_or_else(|| WalletError::Boc("unbuilt child reference".to_owned()))?;
            child_arcs.push(Arc::clone(child));
        }
        let cell = Cell::new(raw.data.clone(), raw.bit_len, child_arcs, raw.exotic)?;
        built[i] = Some(Arc::new(cell));
    }

    let mut roots = Vec::with_capacity(root_indices.len());
    for idx in root_indices {
        let root = built[idx]
            .as_ref()
            .ok_or_else(|| WalletError::Boc("missing root cell".to_owned()))?;
        roots.push(Arc::clone(root));
    }
    Ok(roots)
}

/// Deserializes a single-root base64-encoded BOC.
pub fn deserialize_boc_base64(boc_base64: &str) -> Result<Arc<Cell>, WalletError> {
    let bytes = STANDARD
        .decode(boc_base64.trim())
        .map_err(|e| WalletError::Boc(format!("invalid base64: {e}")))?;
    let mut roots = deserialize_boc(&bytes)?;
    if roots.len() != 1 {
        return Err(WalletError::Boc(format!(
            "expected 1 root cell, got {}",
            roots.len()
        )));
    }
    Ok(roots.remove(0))
}

// ============================================================================
// Direct port of `Telegram/SourceFiles/gram/gram_boc.cpp`
// ============================================================================

struct InspectCell {
    data_from: usize,
    data_size: usize,
    refs_from: usize,
    refs: usize,
    whole_bytes: bool,
}

struct ParsedInspectBoc {
    data: Vec<u8>,
    cells: Vec<InspectCell>,
    ref_size: usize,
    root: usize,
    has_crc: bool,
}

fn read_u32_be(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn parse_inspect_boc(boc_base64: &str) -> Option<ParsedInspectBoc> {
    if boc_base64.is_empty() {
        return None;
    }
    let data = STANDARD.decode(boc_base64.as_bytes()).ok()?;
    if data.is_empty() || data.len() > MAX_INSPECT_BYTES {
        return None;
    }
    let mut offset = 0;
    let magic = read_be_uint(&data, &mut offset, 4).ok()? as u32;
    let flags = read_be_uint(&data, &mut offset, 1).ok()? as u8;
    if magic != BOC_MAGIC {
        return None;
    }
    let has_index = (flags & 0x80) != 0;
    let has_crc = (flags & 0x40) != 0;
    let has_cache_bits = (flags & 0x20) != 0;
    let ref_size = (flags & 0x07) as usize;
    if has_cache_bits || ref_size == 0 || ref_size > 4 {
        return None;
    }
    let offset_size = read_be_uint(&data, &mut offset, 1).ok()? as usize;
    if offset_size == 0 || offset_size > 8 {
        return None;
    }
    let cells_count = read_be_uint(&data, &mut offset, ref_size).ok()? as usize;
    let roots = read_be_uint(&data, &mut offset, ref_size).ok()? as usize;
    let absent = read_be_uint(&data, &mut offset, ref_size).ok()? as usize;
    let size = read_be_uint(&data, &mut offset, offset_size).ok()? as usize;
    if absent != 0
        || roots != 1
        || cells_count == 0
        || cells_count > MAX_INSPECT_CELLS
        || size > MAX_INSPECT_BYTES
    {
        return None;
    }
    let root = read_be_uint(&data, &mut offset, ref_size).ok()? as usize;
    let listed = if has_index {
        cells_count.checked_mul(offset_size)?
    } else {
        0
    };
    if data.len().saturating_sub(offset) < listed {
        return None;
    }
    offset += listed;
    let tail = if has_crc { 4 } else { 0 };
    if data.len().saturating_sub(offset) != size + tail {
        return None;
    }
    let till = offset + size;
    let mut cells = Vec::with_capacity(cells_count);
    for _ in 0..cells_count {
        if till.saturating_sub(offset) < 2 {
            return None;
        }
        let first = data[offset];
        let second = data[offset + 1];
        offset += 2;
        let refs = (first & 0x07) as usize;
        let data_size = ((second >> 1) + (second & 0x01)) as usize;
        if (first & 0xF8) != 0 || refs > 4 || till.saturating_sub(offset) < data_size {
            return None;
        }
        let cell = InspectCell {
            data_from: offset,
            data_size,
            refs_from: offset + data_size,
            refs,
            whole_bytes: (second & 0x01) == 0,
        };
        offset += data_size;
        let refs_bytes = refs * ref_size;
        if till.saturating_sub(offset) < refs_bytes {
            return None;
        }
        offset += refs_bytes;
        cells.push(cell);
    }
    if offset != till {
        return None;
    }
    Some(ParsedInspectBoc {
        data,
        cells,
        ref_size,
        root,
        has_crc,
    })
}

fn inspect_cell_bits(data: &[u8], cell: &InspectCell) -> Option<usize> {
    if cell.whole_bytes {
        return Some(cell.data_size * 8);
    }
    let last = *data.get(cell.data_from + cell.data_size.checked_sub(1)?)?;
    if (last & 0x7F) == 0 {
        return None;
    }
    Some(cell.data_size * 8 - 1 - last.trailing_zeros() as usize)
}

fn strictly_valid_inspect(parsed: &ParsedInspectBoc) -> bool {
    let count = parsed.cells.len();
    if parsed.root >= count {
        return false;
    }
    if parsed.has_crc {
        let till = parsed.data.len() - 4;
        let stored = u32::from_le_bytes([
            parsed.data[till],
            parsed.data[till + 1],
            parsed.data[till + 2],
            parsed.data[till + 3],
        ]);
        if stored != crc32c(&parsed.data[..till]) {
            return false;
        }
    }
    for (i, cell) in parsed.cells.iter().enumerate() {
        if inspect_cell_bits(&parsed.data, cell).is_none() {
            return false;
        }
        let mut off = cell.refs_from;
        for _ in 0..cell.refs {
            let Ok(r) = read_be_uint(&parsed.data, &mut off, parsed.ref_size) else {
                return false;
            };
            if r <= i as u64 || r >= count as u64 {
                return false;
            }
        }
    }
    true
}

/// Replaces the request signature of a Wallet rev00 key-rotation message with random bytes so a
/// fee-emulation quote can never be broadcast (`Gram::BreakRotationSignature` in `gram_boc.cpp`).
#[must_use]
pub fn break_rotation_signature(boc_base64: &str) -> Option<String> {
    let mut parsed = parse_inspect_boc(boc_base64)?;
    let mut found: Option<usize> = None;
    for (i, cell) in parsed.cells.iter().enumerate() {
        if !cell.whole_bytes || cell.refs != REQUEST_REFS || cell.data_size != REQUEST_BYTES {
            continue;
        }
        let opcode = read_u32_be(&parsed.data, cell.data_from + SIGNATURE_BYTES);
        if opcode != EXTERNAL_ROTATION_OPCODE && opcode != INTERNAL_ROTATION_OPCODE {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(i);
    }
    let idx = found?;
    let sig_start = parsed.cells[idx].data_from;
    OsRng.fill_bytes(&mut parsed.data[sig_start..sig_start + SIGNATURE_BYTES]);
    if parsed.has_crc {
        let till = parsed.data.len() - 4;
        let crc = crc32c(&parsed.data[..till]).to_le_bytes();
        parsed.data[till..].copy_from_slice(&crc);
    }
    Some(STANDARD.encode(&parsed.data))
}

/// Extracts a zero-opcode UTF-8 text comment (including snake continuation cells) from a BOC
/// (`Gram::TextCommentFromBoc` in `gram_boc.cpp`).
#[must_use]
pub fn text_comment_from_boc(boc_base64: &str) -> Option<String> {
    let parsed = parse_inspect_boc(boc_base64)?;
    let count = parsed.cells.len();
    let mut visited = vec![false; count];
    let mut bytes = Vec::new();
    let mut index = parsed.root;

    for step in 0..MAX_INSPECT_CELLS {
        if index >= count || visited[index] {
            return None;
        }
        visited[index] = true;
        let cell = &parsed.cells[index];
        if !cell.whole_bytes || cell.refs > 1 {
            return None;
        }
        let mut from = cell.data_from;
        let mut size = cell.data_size;
        if step == 0 {
            if size < 4 || read_u32_be(&parsed.data, from) != 0 {
                return None;
            }
            from += 4;
            size -= 4;
        }
        bytes.extend_from_slice(&parsed.data[from..from + size]);
        if cell.refs == 0 {
            return String::from_utf8(bytes).ok();
        }
        let mut off = cell.refs_from;
        let next = read_be_uint(&parsed.data, &mut off, parsed.ref_size).ok()? as usize;
        if next >= count {
            return None;
        }
        index = next;
    }
    None
}

/// Extracts the new owner's raw TON address (`workchain:64-hex`) from an NFT transfer payload BOC
/// (`Gram::NftTransferNewOwner` in `gram_boc.cpp`).
#[must_use]
pub fn nft_transfer_new_owner(boc_base64: &str) -> Option<String> {
    let parsed = parse_inspect_boc(boc_base64)?;
    if !strictly_valid_inspect(&parsed) {
        return None;
    }
    let root = &parsed.cells[parsed.root];
    let bits = inspect_cell_bits(&parsed.data, root)?;
    if bits < NFT_TRANSFER_OWNER_BITS {
        return None;
    }
    let mut pos = 0_usize;
    let read_bits = |pos: &mut usize, count: usize| -> Option<u64> {
        if count == 0 || count > 64 || bits.saturating_sub(*pos) < count {
            return None;
        }
        let mut acc = 0_u64;
        for _ in 0..count {
            let byte = parsed.data[root.data_from + (*pos / 8)];
            let bit = u64::from((byte >> (7 - (*pos % 8))) & 1);
            acc = (acc << 1) | bit;
            *pos += 1;
        }
        Some(acc)
    };

    if read_bits(&mut pos, 32)? != u64::from(NFT_TRANSFER_OPCODE) {
        return None;
    }
    let _query_id = read_bits(&mut pos, 64)?;
    if read_bits(&mut pos, 2)? != 0b10 || read_bits(&mut pos, 1)? != 0 {
        return None;
    }
    let wc = (read_bits(&mut pos, 8)? as u8) as i8;
    let mut hash = String::with_capacity(64);
    for _ in 0..4 {
        let part = read_bits(&mut pos, 64)?;
        hash.push_str(&format!("{part:016x}"));
    }
    Some(format!("{wc}:{hash}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CellBuilder;

    // Official vectors from `Telegram/SourceFiles/gram/tests/gram_boc_tests.cpp`.
    const K_EXTERNAL: &str = "te6ccgEBAwEA2wABRYgAZ4xQk4U3opHAuJR05zBkP9ox03AUWUCLZzGptmq5eCIMAQHgb3k9gvcbRTBHo0i1Tf6H7+uEc/sWCnBvso+EvWulcg3Sln/+t8K1TbV3ZY0GMfu9SzGLZbD2mX2i7watvgPTB/u6mch//3/9cQAAAAECAwTCrGFc4SyQlfUmZksQauBUlmtXR6JkHwvHYR18E+fdLQIAgMaXjUHo2b8/kzmLv2VlyZq9CkIyoIsmPjPooy9ulySPlJzma5y5fVXX2hgk+42rJJWOyIv9orOu0TaS7uFk8Ac=";
    const K_EXTERNAL_SIG_AT: usize = 51;
    const K_STATE_INIT: &str = "te6ccgECBQEAASMAA0eIAGeMUJOFN6KRwLiUdOcwZD/aMdNwFFlAi2cxqbZquXgiEbABAgMAMP8AIJgh10mDCLnyQN+Ahfgz0O0eIO1T2QBSAAAAAAB//3/9sVgYwu51mjANtmKYj5sLSE5BNQcQddZ7R4cEAwsqFRQB4IJOhqLIPT6ZAVSqau5TiNrwViO18a5BfcbDaD7kwgRBs9qp20QMmdor2Eq49U+WG51zil92UuLZeqEeur18hwj7upnIf/9//XEAAAAAAAAADZsMkWTuztpaoK5jxR0a4idQWpWkORvJ/mlZTNwu4yoEAIBibQcxgA3vUtUu9Z0gckIggtUepLQeyuRyq8CwTKsH+YviQW5yhCnSUky2x7ssTWQ8FztE7UtBqc6G6SSvQJMH";
    const K_STATE_INIT_SIG_AT: usize = 124;
    const K_INTERNAL: &str = "te6ccgEBAwEA6AABYEIAGeMUJOFN6KRwLiUdOcwZD/aMdNwFFlAi2cxqbZquXgiAAAAAAAAAAAAAAAAAAQEB4GjrM3YMvhbU9Hg0G1vQpJVrg5iC886zd6ZNwSAmZRCh2fClR/+rNimR/GY/gmH80A5VJ+8DDRO9q+CrtyIitA77upnHf/9//XEAAAABAgMEk6mZyJrraLyLwzObhObSS7c/VCofehNrPY77xbJQoDUCAIBWCljSrxoHWqV8hxqGgA1c/Ns5vGKNvWAKMFPRbs91WEA37cX7cP9JJc8cEsjbVg5Ue4we4AJjn4VCaU01HWQE";
    const K_INTERNAL_SIG_AT: usize = 64;
    const K_COMMENT: &str = "te6cckEBAQEADAAAFAAAAABIZWxsbyGVgYQo";
    const K_DEPLOY: &str = "te6cckEBBAEAOgACATQCAQAAART/APSkE/S88sgLAwBI0wHQ0wMBcbCRW+D6QDBwgBDIywVYzxYh+gLLagHPFsmAQPsAlxCarA==";
    const K_HASHED: &str = "te6cckEBAwEARQARGAAAAABIZWxsbyEhIQIAZAAAAAAAAAAAAAAAAAAAAAAAAAARIjNEUVVFUllJRCEBACCAgYKDhIWGh4iJiouMjY6PAADqDvOl";

    #[test]
    fn boc_roundtrip_preserves_cell_hash() {
        let comment_root = deserialize_boc_base64(K_COMMENT).unwrap();
        let re_encoded = serialize_boc_base64(&comment_root).unwrap();
        let roundtripped = deserialize_boc_base64(&re_encoded).unwrap();
        assert_eq!(comment_root.hash(), roundtripped.hash());
        assert_eq!(re_encoded, K_COMMENT);

        let deploy_root = deserialize_boc_base64(K_DEPLOY).unwrap();
        let deploy_re = serialize_boc_base64(&deploy_root).unwrap();
        let deploy_rt = deserialize_boc_base64(&deploy_re).unwrap();
        assert_eq!(deploy_root.hash(), deploy_rt.hash());
    }

    #[test]
    fn gram_boc_break_rotation_signature_vectors() {
        for (original, sig_at) in [
            (K_EXTERNAL, K_EXTERNAL_SIG_AT),
            (K_STATE_INIT, K_STATE_INIT_SIG_AT),
            (K_INTERNAL, K_INTERNAL_SIG_AT),
        ] {
            let broken = break_rotation_signature(original).unwrap();
            let was = STANDARD.decode(original).unwrap();
            let now = STANDARD.decode(&broken).unwrap();
            assert_eq!(was.len(), now.len());
            let mut changed = 0;
            for i in 0..was.len() {
                if was[i] != now[i] {
                    assert!(
                        i >= sig_at && i < sig_at + SIGNATURE_BYTES,
                        "byte {i} changed outside signature"
                    );
                    changed += 1;
                }
            }
            assert!(changed >= SIGNATURE_BYTES / 2);
        }

        // Checksummed rotation keeps a valid CRC32C.
        let mut bytes = STANDARD.decode(K_EXTERNAL).unwrap();
        bytes[4] |= 0x40;
        let crc = crc32c(&bytes);
        bytes.extend_from_slice(&crc.to_le_bytes());
        let broken = break_rotation_signature(&STANDARD.encode(&bytes)).unwrap();
        let now = STANDARD.decode(&broken).unwrap();
        let till = now.len() - 4;
        let stored = u32::from_le_bytes([now[till], now[till + 1], now[till + 2], now[till + 3]]);
        assert_eq!(stored, crc32c(&now[..till]));

        // Non-rotation rejected.
        assert!(break_rotation_signature("").is_none());
        assert!(break_rotation_signature("not base64").is_none());
        assert!(break_rotation_signature(K_COMMENT).is_none());
    }

    #[test]
    fn gram_boc_text_comment_vectors() {
        assert_eq!(text_comment_from_boc(K_COMMENT).as_deref(), Some("Hello!"));
        assert!(text_comment_from_boc("").is_none());
        assert!(text_comment_from_boc(K_DEPLOY).is_none());
        assert!(text_comment_from_boc(K_HASHED).is_none());

        // Snake comment from gram_boc_tests.cpp:142-148 ("abcdef").
        let snake = hex::decode("b5ee9c7201010201000f00010e00000000616263010006646566").unwrap();
        assert_eq!(
            text_comment_from_boc(&STANDARD.encode(snake)).as_deref(),
            Some("abcdef")
        );

        // Long comment built with CellBuilder round-trips through text_comment_from_boc.
        let long_text = "TON comment ".repeat(20);
        let mut b = CellBuilder::new();
        b.store_text_comment(&long_text).unwrap();
        let cell = b.end_cell().unwrap();
        let boc = serialize_boc_base64(&cell).unwrap();
        assert_eq!(
            text_comment_from_boc(&boc).as_deref(),
            Some(long_text.as_str())
        );
    }

    #[test]
    fn gram_boc_nft_transfer_owner_vectors() {
        use crate::address::TonAddress;

        let owner_hex = "0123456789abcdeffedcba987654321013579bdf02468aceeca86420fdb97531";
        let owner_bytes: [u8; 32] = hex::decode(owner_hex).unwrap().try_into().unwrap();
        for wc in [0_i8, -1_i8] {
            let mut b = CellBuilder::new();
            b.store_uint(u64::from(NFT_TRANSFER_OPCODE), 32).unwrap();
            b.store_uint(0x0102_0304_0506_0708, 64).unwrap();
            b.store_address(&TonAddress::new(wc, owner_bytes)).unwrap();
            b.store_address(&TonAddress::new(0, [0x5A; 32])).unwrap();
            b.store_bit(false).unwrap();
            b.store_coins(1).unwrap();
            b.store_bit(false).unwrap();
            let cell = b.end_cell().unwrap();
            let boc = serialize_boc_base64(&cell).unwrap();
            assert_eq!(
                nft_transfer_new_owner(&boc).as_deref(),
                Some(format!("{wc}:{owner_hex}").as_str())
            );
        }
        assert!(nft_transfer_new_owner(K_COMMENT).is_none());
    }

    fn build_raw_boc(cells_count: usize, ref_size: usize, cells_data: &[u8]) -> Vec<u8> {
        let offset_size = bytes_needed_for(cells_data.len().max(1));
        let flags = ref_size as u8 & 0x07; // no idx, no crc
        let mut out = Vec::new();
        out.extend_from_slice(&BOC_MAGIC.to_be_bytes());
        out.push(flags);
        out.push(offset_size as u8);
        write_be_uint(&mut out, cells_count as u64, ref_size);
        write_be_uint(&mut out, 1, ref_size); // roots = 1
        write_be_uint(&mut out, 0, ref_size); // absent = 0
        write_be_uint(&mut out, cells_data.len() as u64, offset_size);
        write_be_uint(&mut out, 0, ref_size); // root_list[0] = 0
        out.extend_from_slice(cells_data);
        out
    }

    #[test]
    fn sec2c_w1_diamond_chain_40_cells_hashes_in_milliseconds() {
        // 40-cell diamond DAG: each cell i < 39 has 4 refs all pointing to cell i + 1 (4^39 paths).
        let cells_count = 40_usize;
        let mut cells_data = Vec::new();
        for i in 0..cells_count {
            if i + 1 < cells_count {
                cells_data.push(0x04); // d1: 4 refs, ordinary, level 0
                cells_data.push(0x00); // d2: 0 bits
                for _ in 0..4 {
                    cells_data.push((i + 1) as u8);
                }
            } else {
                cells_data.push(0x00); // leaf: 0 refs
                cells_data.push(0x00); // 0 bits
            }
        }
        let raw_boc = build_raw_boc(cells_count, 1, &cells_data);
        assert!(raw_boc.len() < 300, "compact ~250-byte diamond BOC");

        let started = std::time::Instant::now();
        let roots = deserialize_boc(&raw_boc).expect("40-cell diamond BOC must deserialize");
        let root = &roots[0];
        let h1 = root.hash();
        let depth = root.depth();
        let lm = root.level_mask();
        let re_encoded = serialize_boc(root).expect("re-serialize diamond BOC");
        let rt = deserialize_boc(&re_encoded).expect("roundtrip diamond BOC");
        let elapsed = started.elapsed();

        assert_eq!(depth, 39);
        assert_eq!(lm, 0);
        assert_eq!(rt[0].hash(), h1);
        assert_eq!(*root, rt[0]);
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "40-cell diamond BOC took too long: {elapsed:?}"
        );
    }

    #[test]
    fn sec2c_w1_2000_deep_chain_rejected_without_stack_overflow() {
        // 2000-cell linear chain: cell i -> cell i + 1 (depth 1999 > MAX_CELL_DEPTH = 1024).
        let cells_count = 2000_usize;
        let mut cells_data = Vec::with_capacity(cells_count * 4);
        for i in 0..cells_count {
            if i + 1 < cells_count {
                cells_data.push(0x01); // d1: 1 ref
                cells_data.push(0x00); // d2: 0 bits
                write_be_uint(&mut cells_data, (i + 1) as u64, 2);
            } else {
                cells_data.push(0x00);
                cells_data.push(0x00);
            }
        }
        let raw_boc = build_raw_boc(cells_count, 2, &cells_data);
        let err = deserialize_boc(&raw_boc)
            .expect_err("2000-deep BOC chain must be rejected cleanly without stack overflow");
        assert!(
            err.to_string().contains("depth"),
            "expected depth rejection error, got: {err}"
        );

        // Also verify cell-count cap rejects BOCs exceeding MAX_BOC_CELLS before allocation.
        let over_cap = MAX_BOC_CELLS + 1;
        let fake_cells = vec![0_u8; over_cap * 2];
        let raw_over = build_raw_boc(over_cap, 4, &fake_cells);
        let cap_err = deserialize_boc(&raw_over)
            .expect_err("BOC with cells_count > MAX_BOC_CELLS must be rejected");
        assert!(
            cap_err.to_string().contains("exceeds limit"),
            "expected cell count cap error, got: {cap_err}"
        );
    }
}
