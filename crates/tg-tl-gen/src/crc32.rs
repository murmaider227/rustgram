//! IEEE 802.3 CRC32 and TL constructor ID computation.
//!
//! Port of the CRC32 constructor-ID normalization in `lib_tl/tl/generate_tl.py`
//! and the TL specification (<https://core.telegram.org/mtproto/TL-abstract-types>).

use crate::ast::{Definition, FlagInner, ParamType, TypeRef};

/// Precomputed IEEE 802.3 CRC32 lookup table (polynomial `0xEDB88320`).
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut c = i;
        let mut j = 0;
        while j < 8 {
            if c & 1 != 0 {
                c = 0xEDB8_8320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
            j += 1;
        }
        table[i as usize] = c;
        i += 1;
    }
    table
};

/// Computes the standard IEEE 802.3 CRC32 checksum of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = CRC32_TABLE[idx] ^ (crc >> 8);
    }
    !crc
}

/// Formats a [`TypeRef`] in TL CRC-normalized form (angle brackets replaced by spaces).
fn format_type_ref_for_crc(ty: &TypeRef, out: &mut String, bytes_as_string: bool) {
    if ty.generic_ref {
        out.push('!');
    }
    if let Some(ref ns) = ty.namespace {
        out.push_str(ns);
        out.push('.');
    }
    if bytes_as_string && ty.namespace.is_none() && ty.name == "bytes" {
        out.push_str("string");
    } else {
        out.push_str(&ty.name);
    }
    if let Some(ref arg) = ty.generic_arg {
        out.push(' ');
        // Note: Telegram's TL compiler and `lib_tl/tl/generate_tl.py` replace
        // `:bytes ` and `?bytes ` with `string`, leaving `Vector<bytes>` as `Vector bytes`.
        format_type_ref_for_crc(arg, out, false);
    }
}

/// Produces the normalized TL definition string used for CRC32 constructor-ID computation.
///
/// Normalization rules (matching Telegram's TL spec and `lib_tl/tl/generate_tl.py`):
/// 1. Omit `#hexid` and trailing `;`.
/// 2. Omit conditional `:flag.N?true` parameters.
/// 3. Strip `{` and `}` around generic parameters (`{X:Type}` -> `X:Type`).
/// 4. Replace `<` and `>` with spaces and collapse repeated spaces.
/// 5. Replace top-level parameter `bytes` type with its TL synonym `string`.
pub fn normalize_definition_for_crc(def: &Definition) -> String {
    let mut out = String::new();
    if let Some(ref ns) = def.namespace {
        out.push_str(ns);
        out.push('.');
    }
    out.push_str(&def.name);

    for gp in &def.generic_params {
        out.push(' ');
        out.push_str(gp);
        out.push_str(":Type");
    }

    for param in &def.params {
        match &param.ty {
            ParamType::Flags => {
                out.push(' ');
                out.push_str(&param.name);
                out.push_str(":#");
            }
            ParamType::Normal(ty) => {
                out.push(' ');
                out.push_str(&param.name);
                out.push(':');
                format_type_ref_for_crc(ty, &mut out, true);
            }
            ParamType::Flag {
                flag_field,
                bit,
                inner,
            } => match inner {
                FlagInner::True => {
                    // Omitted from CRC32 input per TL specification.
                }
                FlagInner::Type(ty) => {
                    out.push(' ');
                    out.push_str(&param.name);
                    out.push(':');
                    out.push_str(flag_field);
                    out.push('.');
                    out.push_str(&bit.to_string());
                    out.push('?');
                    format_type_ref_for_crc(ty, &mut out, true);
                }
            },
        }
    }

    out.push_str(" = ");
    format_type_ref_for_crc(&def.result_type, &mut out, false);
    out
}

/// Computes the CRC32 constructor ID of a parsed [`Definition`].
pub fn compute_constructor_id(def: &Definition) -> u32 {
    let normalized = normalize_definition_for_crc(def);
    crc32(normalized.as_bytes())
}
