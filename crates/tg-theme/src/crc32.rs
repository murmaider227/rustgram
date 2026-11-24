//! CRC32 hashing matching `lib_base/base/crc32hash.cpp`.

use std::sync::OnceLock;

const POLY: u32 = 0x04c1_1db7;

fn reflect(mut val: u32, bits: u32) -> u32 {
    let mut result = 0u32;
    for i in 1..=bits {
        if (val & 1) != 0 {
            result |= 1u32 << (bits - i);
        }
        val >>= 1;
    }
    result
}

fn table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut entry = reflect(i as u32, 8) << 24;
            for _ in 0..8 {
                entry = (entry << 1) ^ if (entry & (1u32 << 31)) != 0 { POLY } else { 0 };
            }
            *slot = reflect(entry, 32);
        }
        t
    })
}

/// Computes the signed 32-bit CRC32 checksum identical to tdesktop's `base::crc32`.
#[must_use]
pub fn crc32(data: &[u8]) -> i32 {
    let tbl = table();
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc = (crc >> 8) ^ tbl[((crc & 0xff) as usize) ^ (byte as usize)];
    }
    (crc ^ 0xffff_ffffu32) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_check_value() {
        // IEEE 802.3 CRC32("123456789") == 0xCBF43926
        assert_eq!(crc32(b"123456789") as u32, 0xcbf4_3926);
        assert_eq!(crc32(b"") as u32, 0);
    }
}
