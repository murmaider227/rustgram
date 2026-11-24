//! Offline deterministic TL Layer 233 conformance tests for `tg-tl`.
//!
//! Independently parses `schema/api.tl` and `schema/mtproto.tl` directly from disk,
//! verifies every constructor/function ID against an independent IEEE 802.3 CRC32 of the
//! normalized TL line (matching tdesktop's `codegen_scheme.py` / `generate_tl.py`),
//! audits and asserts the exact set of schema constructors skipped by the generator,
//! and tests `gzip_packed`, boxed/bare vectors, `flags`/`flags2`/`true` flags/shared bits,
//! and `bytes`/`string` 4-byte padding at every length boundary.

use std::collections::BTreeSet;

use tg_tl::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, BareVec, Boxed, Deserializable, DeserializeError, Identifiable,
    LAYER, RawObject, Serializable, VECTOR_ID, enums, from_constructor_id, functions, mtproto,
    types,
};

const API_TL: &str = include_str!("../../../schema/api.tl");
const MTPROTO_TL: &str = include_str!("../../../schema/mtproto.tl");

/// Independent IEEE 802.3 CRC32 (bit-by-bit, polynomial 0xEDB88320).
fn spec_crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Independent TL line normalizer matching `Telegram/SourceFiles/codegen/scheme/codegen_scheme.py`
/// and `Telegram/lib_tl/tl/generate_tl.py`.
fn normalize_tl_line_tdesktop(raw_line: &str) -> (String, Option<u32>, String) {
    let line = raw_line.trim().trim_end_matches(';').trim();
    let (lhs, rhs) = line.split_once('=').expect("TL line must contain '='");
    let mut tokens = lhs.split_whitespace();
    let head = tokens.next().expect("TL line must have constructor name");
    let (name, explicit_id) = match head.split_once('#') {
        Some((n, hex)) => (n.to_string(), Some(u32::from_str_radix(hex, 16).unwrap())),
        None => (head.to_string(), None),
    };

    let mut norm_parts = vec![name.clone()];
    for tok in tokens {
        // Generic parameter `{X:Type}` -> `X:Type`
        if tok.starts_with('{') && tok.ends_with('}') {
            norm_parts.push(tok[1..tok.len() - 1].to_string());
            continue;
        }
        // Conditional `name:flags.N?true` is omitted from CRC32 input.
        if let Some((_, ty)) = tok.split_once(':')
            && let Some((_, inner)) = ty.split_once('?')
            && inner == "true"
        {
            continue;
        }
        // Replace `:bytes` or `?bytes` (top-level synonym, not inside `Vector<bytes>`) with `string`,
        // and replace `<` / `>` with spaces.
        let replaced_synonym = if let Some((p, ty)) = tok.split_once(':') {
            if ty == "bytes" {
                format!("{p}:string")
            } else if let Some((cond, inner)) = ty.split_once('?') {
                if inner == "bytes" {
                    format!("{p}:{cond}?string")
                } else {
                    tok.to_string()
                }
            } else {
                tok.to_string()
            }
        } else {
            tok.to_string()
        };
        let no_angles = replaced_synonym.replace(['<', '>'], " ");
        for sub in no_angles.split_whitespace() {
            norm_parts.push(sub.to_string());
        }
    }

    norm_parts.push("=".to_string());
    let rhs_no_angles = rhs.replace(['<', '>'], " ");
    for sub in rhs_no_angles.split_whitespace() {
        norm_parts.push(sub.to_string());
    }

    let normalized = norm_parts.join(" ");
    (name, explicit_id, normalized)
}

/// Built-in core lines skipped by both tdesktop's `codegen_scheme.py` and `tg-tl-gen`.
const SKIP_LINES: &[&str] = &[
    "int ? = Int;",
    "long ? = Long;",
    "double ? = Double;",
    "string ? = String;",
    "vector {t:Type} # [ t ] = Vector t;",
    "vector#1cb5c415 {t:Type} # [ t ] = Vector t;",
    "int128 4*[ int ] = Int128;",
    "int256 8*[ int ] = Int256;",
];

/// Historical MTProto schema constructor IDs exempted in tdesktop's `codegen_scheme.py:52-58`.
const TDESKTOP_CRC_EXCEPTIONS: &[(&str, u32)] = &[
    ("ipPortSecret", 0x3798_2646),
    ("accessPointRule", 0x4679_b65f),
    ("help.configSimple", 0x5a59_2a6c),
];

#[test]
fn conformance_all_schema_constructor_ids_match_normalized_crc32_and_audit_skipped() {
    assert_eq!(LAYER, 233);

    let mut api_active = 0usize;
    let mut api_skipped_builtins: Vec<String> = Vec::new();
    let mut api_bool_ctors: Vec<String> = Vec::new();
    let mut api_seen_ids = BTreeSet::new();

    for raw in API_TL.lines() {
        let stripped = raw.split("//").next().unwrap_or("").trim();
        if stripped.is_empty() || stripped == "---types---" || stripped == "---functions---" {
            continue;
        }
        if SKIP_LINES.contains(&stripped) {
            api_skipped_builtins.push(stripped.to_string());
            continue;
        }
        let (name, explicit_id, normalized) = normalize_tl_line_tdesktop(stripped);
        let explicit =
            explicit_id.unwrap_or_else(|| panic!("api.tl entry `{name}` missing explicit #id"));
        let crc = spec_crc32(normalized.as_bytes());
        assert_eq!(
            crc, explicit,
            "api.tl CRC32 mismatch for `{name}`: normalized=`{normalized}`"
        );
        // Every active constructor/function in api.tl must appear in `from_constructor_id`.
        assert_eq!(
            from_constructor_id(explicit),
            Some(name.as_str()),
            "missing api.tl constructor `{name}#{explicit:08x}` from generated id table"
        );
        assert!(
            api_seen_ids.insert(explicit),
            "duplicate id 0x{explicit:08x} in api.tl"
        );
        if stripped.ends_with("= Bool;") && (name == "boolFalse" || name == "boolTrue") {
            api_bool_ctors.push(name);
        }
        api_active += 1;
    }

    assert!(api_active > 2500);
    // Exact audit of skipped declarations in api.tl:
    // 1. `vector#1cb5c415 {t:Type} # [ t ] = Vector t;` (handled generically by Vec<T> / VECTOR_ID)
    // 2. `boolFalse#bc799737 = Bool;` and `boolTrue#997275b5 = Bool;` (mapped natively to Rust `bool`)
    assert_eq!(
        api_skipped_builtins,
        vec!["vector#1cb5c415 {t:Type} # [ t ] = Vector t;"]
    );
    assert_eq!(api_bool_ctors, vec!["boolFalse", "boolTrue"]);

    let mut mtp_active = 0usize;
    let mut mtp_skipped_builtins: Vec<String> = Vec::new();
    let mut mtp_commented_manual: Vec<String> = Vec::new();

    for raw in MTPROTO_TL.lines() {
        let trimmed = raw.trim();
        if let Some(commented) = trimmed.strip_prefix("//") {
            let c = commented.trim();
            if c.contains("= RpcResult;")
                || c.contains("= Message;")
                || c.contains("= MessageContainer;")
                || c.contains("= MessageCopy;")
                || c.contains("= Object;")
            {
                mtp_commented_manual.push(c.to_string());
            }
        }
        let stripped = raw.split("//").next().unwrap_or("").trim();
        if stripped.is_empty() || stripped == "---types---" || stripped == "---functions---" {
            continue;
        }
        if SKIP_LINES.contains(&stripped) {
            mtp_skipped_builtins.push(stripped.to_string());
            continue;
        }
        let (name, explicit_id, normalized) = normalize_tl_line_tdesktop(stripped);
        let crc = spec_crc32(normalized.as_bytes());
        let expected_id = match explicit_id {
            Some(explicit) => {
                if !TDESKTOP_CRC_EXCEPTIONS
                    .iter()
                    .any(|&(n, id)| n == name && id == explicit)
                {
                    assert_eq!(
                        crc, explicit,
                        "mtproto.tl CRC32 mismatch for `{name}`: normalized=`{normalized}`"
                    );
                }
                explicit
            }
            None => crc,
        };
        assert_eq!(
            from_constructor_id(expected_id),
            Some(name.as_str()),
            "missing mtproto.tl constructor `{name}#{expected_id:08x}` from generated id table"
        );
        mtp_active += 1;
    }

    assert!(mtp_active >= 60);
    // Exact audit of skipped built-in core lines in mtproto.tl (same 7 lines as codegen_scheme.py):
    assert_eq!(
        mtp_skipped_builtins,
        vec![
            "int ? = Int;",
            "long ? = Long;",
            "double ? = Double;",
            "string ? = String;",
            "vector {t:Type} # [ t ] = Vector t;",
            "int128 4*[ int ] = Int128;",
            "int256 8*[ int ] = Int256;",
        ]
    );
    // Commented-out manual MTProto framing lines: 4 implemented in manual.rs + 1 unused (msg_copy).
    assert_eq!(mtp_commented_manual.len(), 5);
    assert_eq!(
        from_constructor_id(mtproto::RpcResult::CONSTRUCTOR_ID),
        Some("rpc_result")
    );
    assert_eq!(
        from_constructor_id(mtproto::MsgContainer::CONSTRUCTOR_ID),
        Some("msg_container")
    );
    assert_eq!(
        from_constructor_id(mtproto::GzipPacked::CONSTRUCTOR_ID),
        Some("gzip_packed")
    );
    assert_eq!(from_constructor_id(0xe060_46b2), Some("msg_copy"));
    assert_eq!(from_constructor_id(VECTOR_ID), Some("vector"));
    assert_eq!(from_constructor_id(BOOL_TRUE_ID), Some("boolTrue"));
    assert_eq!(from_constructor_id(BOOL_FALSE_ID), Some("boolFalse"));
}

#[test]
fn conformance_bytes_and_string_4byte_padding_all_boundaries() {
    // Test EVERY length from 0..=265 plus large 16-bit and 17-bit lengths (65536, 70000).
    let mut lengths: Vec<usize> = (0..=265).collect();
    lengths.extend_from_slice(&[511, 512, 513, 1024, 65535, 65536, 65537, 70000]);

    for len in lengths {
        let payload: Vec<u8> = (0..len).map(|i| b'a' + (i % 26) as u8).collect();
        let encoded = payload.to_bytes();

        // Total wire length MUST be a multiple of 4.
        assert_eq!(encoded.len() % 4, 0, "unaligned TL bytes for len={len}");

        if len <= 253 {
            // Short form: [len: u8] || payload || zero_pad(0..3)
            let pad = (4 - ((1 + len) % 4)) % 4;
            assert_eq!(encoded.len(), 1 + len + pad);
            assert_eq!(encoded[0], len as u8);
            assert_eq!(&encoded[1..1 + len], &payload[..]);
            assert!(
                encoded[1 + len..].iter().all(|&b| b == 0),
                "padding bytes must be zero for len={len}"
            );
        } else {
            // Long form: [0xFE, len_lo, len_mid, len_hi] || payload || zero_pad(0..3)
            let pad = (4 - (len % 4)) % 4;
            assert_eq!(encoded.len(), 4 + len + pad);
            assert_eq!(encoded[0], 0xFE);
            assert_eq!(encoded[1], (len & 0xFF) as u8);
            assert_eq!(encoded[2], ((len >> 8) & 0xFF) as u8);
            assert_eq!(encoded[3], ((len >> 16) & 0xFF) as u8);
            assert_eq!(&encoded[4..4 + len], &payload[..]);
            assert!(
                encoded[4 + len..].iter().all(|&b| b == 0),
                "padding bytes must be zero for len={len}"
            );
        }

        // Round-trip as both Vec<u8> and String.
        assert_eq!(Vec::<u8>::from_bytes(&encoded).unwrap(), payload);
        let s = String::from_utf8(payload).unwrap();
        assert_eq!(s.to_bytes(), encoded);
        assert_eq!(String::from_bytes(&encoded).unwrap(), s);
    }

    // Prefix 0xFF MUST be rejected.
    assert_eq!(
        Vec::<u8>::from_bytes(&[0xFF, 0, 0, 0]),
        Err(DeserializeError::InvalidStringLength { prefix: 0xFF })
    );
}

#[test]
fn conformance_vectors_boxed_bare_nested_and_gzip_packed() {
    // 1. Boxed Vector<bytes> vs Vec<u8> bytes (ensures Vec<Vec<u8>> writes 0x1cb5c415).
    let blobs: Vec<Vec<u8>> = vec![b"hi".to_vec(), b"world!".to_vec()];
    let wire = blobs.to_bytes();
    assert_eq!(&wire[0..4], &VECTOR_ID.to_le_bytes());
    assert_eq!(&wire[4..8], &2u32.to_le_bytes());
    assert_eq!(&wire[8..12], &[2, b'h', b'i', 0]);
    assert_eq!(&wire[12..20], &[6, b'w', b'o', b'r', b'l', b'd', b'!', 0]);
    assert_eq!(Vec::<Vec<u8>>::from_bytes(&wire).unwrap(), blobs);

    // 2. Bare vector<future_salt> inside future_salts#ae500895 (no 0x1cb5c415, bare elements).
    let fs = mtproto::enums::FutureSalts::FutureSalts(mtproto::types::FutureSalts {
        req_msg_id: 0x1111_2222_3333_4444,
        now: 1_700_000_000,
        salts: vec![
            mtproto::types::FutureSalt {
                valid_since: 1_700_000_000,
                valid_until: 1_700_001_800,
                salt: 0x1020_3040_5060_7080,
            },
            mtproto::types::FutureSalt {
                valid_since: 1_700_001_800,
                valid_until: 1_700_003_600,
                salt: -1,
            },
        ],
    });
    let fs_wire = fs.to_bytes();
    // 4 (ctor) + 8 (req_msg_id) + 4 (now) + 4 (bare vec len) + 2 * 16 (bare future_salt) = 52 bytes.
    assert_eq!(fs_wire.len(), 52);
    assert_eq!(&fs_wire[0..4], &0xae50_0895u32.to_le_bytes());
    assert_eq!(&fs_wire[16..20], &2u32.to_le_bytes()); // bare vector count, NO VECTOR_ID!
    assert_eq!(
        mtproto::enums::FutureSalts::from_bytes(&fs_wire).unwrap(),
        fs
    );

    // 3. Nested boxed Vector<Vector<TlsBlock>> inside tlsBlockPermutation & bare vector<TlsBlock> inside tlsClientHello.
    let perm = mtproto::enums::TlsBlock::Permutation(mtproto::types::TlsBlockPermutation {
        entries: vec![
            vec![mtproto::enums::TlsBlock::Domain],
            vec![
                mtproto::enums::TlsBlock::Zero(mtproto::types::TlsBlockZero { length: 32 }),
                mtproto::enums::TlsBlock::Padding,
            ],
        ],
    });
    let hello = mtproto::enums::TlsClientHello::TlsClientHello(mtproto::types::TlsClientHello {
        blocks: vec![
            mtproto::enums::TlsBlock::String(mtproto::types::TlsBlockString {
                data: b"\x16\x03\x01".to_vec(),
            }),
            perm,
        ],
    });
    let hello_wire = hello.to_bytes();
    // Bytes 4..8 are bare vector count = 2 (no VECTOR_ID).
    assert_eq!(&hello_wire[4..8], &2u32.to_le_bytes());
    assert_eq!(
        mtproto::enums::TlsClientHello::from_bytes(&hello_wire).unwrap(),
        hello
    );

    // 4. BareVec<i32> omits VECTOR_ID.
    let bv = BareVec(vec![10i32, -20, 30]);
    let bv_wire = bv.to_bytes();
    assert_eq!(bv_wire.len(), 16);
    assert_eq!(BareVec::<i32>::from_bytes(&bv_wire).unwrap(), bv);

    // 5. gzip_packed#3072cfa1 wraps any TL object, validates magic bytes 1f 8b, and enforces limits.
    let inner_wire = fs_wire;
    let gz = mtproto::GzipPacked::pack(&inner_wire).unwrap();
    assert_eq!(&gz.packed_data[0..2], &[0x1F, 0x8B]); // RFC 1952 gzip header magic
    let gz_wire = Boxed(gz).to_bytes();
    assert_eq!(&gz_wire[0..4], &0x3072_cfa1u32.to_le_bytes());
    let decoded_gz = Boxed::<mtproto::GzipPacked>::from_bytes(&gz_wire)
        .unwrap()
        .0;
    let unpacked = decoded_gz.unpack().unwrap();
    assert_eq!(unpacked, inner_wire);
    assert_eq!(
        mtproto::enums::FutureSalts::from_bytes(&unpacked).unwrap(),
        fs
    );
    assert!(decoded_gz.unpack_with_limit(4).is_err());
}

#[test]
fn conformance_flags_flags2_true_flags_and_conditional_fields() {
    // 1. `true` flags consume 0 payload bytes (encoded solely in the `flags:#` bitmask).
    //    `inputPeerNotifySettings#cacb6ae2` has flags:#, show_previews:flags.0?Bool, silent:flags.1?Bool, ...
    //    `account.finishTakeoutSession#1d2652ee flags:# success:flags.0?true = Bool;`
    let finish_false = functions::account::FinishTakeoutSession { success: false };
    assert_eq!(
        finish_false.to_bytes(),
        [0xee, 0x52, 0x26, 0x1d, 0x00, 0x00, 0x00, 0x00] // ctor + flags=0 (8 bytes total!)
    );
    let finish_true = functions::account::FinishTakeoutSession { success: true };
    assert_eq!(
        finish_true.to_bytes(),
        [0xee, 0x52, 0x26, 0x1d, 0x01, 0x00, 0x00, 0x00] // ctor + flags=1 (still 8 bytes total!)
    );
    assert_eq!(
        functions::account::FinishTakeoutSession::from_bytes(&finish_true.to_bytes()).unwrap(),
        finish_true
    );

    // 2. Dual bitmask (`flags:#` AND `flags2:#`) on `types::User`:
    //    - flags.0 (access_hash), flags.10 (self_:true), flags.14 (shared bot:true + bot_info_version:int),
    //      flags.28 (premium:true)
    //    - flags2.2 (close_friend:true), flags2.12 (bot_active_users:int)
    let user = types::User {
        self_: true,
        contact: false,
        mutual_contact: false,
        deleted: false,
        bot: true,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: true,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: true,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id: 42,
        access_hash: Some(0x1234_5678),
        first_name: None,
        last_name: None,
        username: Some("alice".to_string()), // flags.3
        phone: None,
        photo: None,
        status: None,
        bot_info_version: Some(7),
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: Some(123),
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    };

    let wire = user.to_bytes();
    // Exact byte-by-byte layout check:
    // [0..4]   flags  = (1<<0) | (1<<3) | (1<<10) | (1<<14) | (1<<28)
    // [4..8]   flags2 = (1<<2) | (1<<12)
    // [8..16]  id = 42i64
    // [16..24] access_hash = 0x12345678i64
    // [24..32] username = [5, 'a', 'l', 'i', 'c', 'e', 0, 0]
    // [32..36] bot_info_version = 7i32
    // [36..40] bot_active_users = 123i32
    assert_eq!(wire.len(), 40);
    let expected_flags: u32 = (1 << 0) | (1 << 3) | (1 << 10) | (1 << 14) | (1 << 28);
    let expected_flags2: u32 = (1 << 2) | (1 << 12);
    assert_eq!(&wire[0..4], &expected_flags.to_le_bytes());
    assert_eq!(&wire[4..8], &expected_flags2.to_le_bytes());
    assert_eq!(&wire[8..16], &42i64.to_le_bytes());
    assert_eq!(&wire[16..24], &0x1234_5678i64.to_le_bytes());
    assert_eq!(&wire[24..32], &[5, b'a', b'l', b'i', b'c', b'e', 0, 0]);
    assert_eq!(&wire[32..36], &7i32.to_le_bytes());
    assert_eq!(&wire[36..40], &123i32.to_le_bytes());

    let decoded = types::User::from_bytes(&wire).unwrap();
    assert_eq!(decoded, user);

    // 3. Verify `invokeWithLayer(initConnection(..., query))` with both `proxy` (flags.0) and `params` (flags.1) set.
    let wrapped = functions::InvokeWithLayer {
        layer: LAYER,
        query: functions::InitConnection {
            api_id: 99999,
            device_model: "Linux".to_string(),
            system_version: "6.8".to_string(),
            app_version: "0.1.0".to_string(),
            system_lang_code: "en".to_string(),
            lang_pack: "tdesktop".to_string(),
            lang_code: "en".to_string(),
            proxy: Some(enums::InputClientProxy::InputClientProxy(
                types::InputClientProxy {
                    address: "127.0.0.1".to_string(),
                    port: 443,
                },
            )),
            params: Some(enums::JSONValue::JsonObject(types::JsonObject {
                value: vec![enums::JSONObjectValue::JsonObjectValue(
                    types::JsonObjectValue {
                        key: "tz_offset".to_string(),
                        value: enums::JSONValue::JsonNumber(types::JsonNumber { value: 3600.0 }),
                    },
                )],
            })),
            query: RawObject(functions::help::GetConfig.to_bytes()),
        },
    };
    let wrapped_wire = wrapped.to_bytes();
    // Constructor IDs and initConnection flags == 0x03 (bits 0 and 1).
    assert_eq!(&wrapped_wire[0..4], &0xda9b_0d0du32.to_le_bytes());
    assert_eq!(&wrapped_wire[4..8], &233i32.to_le_bytes());
    assert_eq!(&wrapped_wire[8..12], &0xc1cd_5ea9u32.to_le_bytes());
    assert_eq!(&wrapped_wire[12..16], &0x0000_0003u32.to_le_bytes());
    let decoded_wrapped =
        functions::InvokeWithLayer::<functions::InitConnection<RawObject>>::from_bytes(
            &wrapped_wire,
        )
        .unwrap();
    assert_eq!(decoded_wrapped, wrapped);
}
