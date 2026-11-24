//! `tg-tl`: Telegram TL runtime and generated Layer 233 API + MTProto schemas.
//!
//! Port of `Telegram/lib_tl` (`tl_basic_types.h`, `tl_boxed.h`) and
//! `Telegram/SourceFiles/mtproto/core_types.h` + generated `scheme.h`/`scheme.cpp`.

pub mod cursor;
pub mod error;
pub mod manual;
pub mod traits;

pub use cursor::Cursor;
pub use error::DeserializeError;
pub use traits::{
    BOOL_FALSE_ID, BOOL_TRUE_ID, BareVec, Boxed, Deserializable, Identifiable, RawObject,
    RemoteCall, Serializable, VECTOR_ID, deserialize_bare_vec, serialize_bare_vec, write_tl_bytes,
};

// Generated Telegram API schema (`LAYER`, `types`, `enums`, `functions`).
include!(concat!(env!("OUT_DIR"), "/api.rs"));

/// Generated MTProto schema (`schema/mtproto.tl`) plus manual framing types
/// (`RpcResult`, `CoreMessage`, `MsgContainer`, `GzipPacked`).
pub mod mtproto {
    pub use crate::manual::{
        CoreMessage, DEFAULT_MAX_UNPACK_BYTES, GzipPacked, MsgContainer, RpcResult,
    };

    include!(concat!(env!("OUT_DIR"), "/mtproto.rs"));
}

// Generated constructor ID debug table (`from_constructor_id`).
include!(concat!(env!("OUT_DIR"), "/constructor_ids.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    // Exhaustive generated round-trip tests for every constructor and function in both schemas.
    include!(concat!(env!("OUT_DIR"), "/roundtrip_api.rs"));
    include!(concat!(env!("OUT_DIR"), "/roundtrip_mtproto.rs"));

    #[test]
    fn layer_constant_is_233() {
        assert_eq!(LAYER, 233);
    }

    #[test]
    fn known_answer_req_pq_multi() {
        let nonce: [u8; 16] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ];
        let req = mtproto::functions::ReqPqMulti { nonce };
        assert_eq!(mtproto::functions::ReqPqMulti::CONSTRUCTOR_ID, 0xbe7e_8ef1);
        assert_eq!(req.name(), "req_pq_multi");

        let expected: Vec<u8> = [
            // 0xbe7e8ef1 little-endian
            0xf1, 0x8e, 0x7e, 0xbe, // nonce (16 bytes)
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ]
        .to_vec();

        let encoded = req.to_bytes();
        assert_eq!(encoded, expected);
        let decoded = mtproto::functions::ReqPqMulti::from_bytes(&encoded).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn known_answer_input_peer_user() {
        let peer = types::InputPeerUser {
            user_id: 0x0102_0304_0506_0708,
            access_hash: -1,
        };
        assert_eq!(types::InputPeerUser::CONSTRUCTOR_ID, 0xdde8_a54c);

        // Bare serialization has NO constructor ID.
        let bare_expected: Vec<u8> = [
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // user_id LE
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // access_hash = -1 LE
        ]
        .to_vec();
        assert_eq!(peer.to_bytes(), bare_expected);
        assert_eq!(
            types::InputPeerUser::from_bytes(&bare_expected).unwrap(),
            peer
        );

        // Boxed serialization via enums::InputPeer prepends 0xdde8a54c LE.
        let boxed = enums::InputPeer::User(peer.clone());
        let mut boxed_expected = vec![0x4c, 0xa5, 0xe8, 0xdd];
        boxed_expected.extend_from_slice(&bare_expected);
        assert_eq!(boxed.to_bytes(), boxed_expected);
        assert_eq!(
            enums::InputPeer::from_bytes(&boxed_expected).unwrap(),
            boxed
        );

        // Boxed<types::InputPeerUser> produces the exact same bytes.
        assert_eq!(Boxed(peer.clone()).to_bytes(), boxed_expected);
        assert_eq!(
            Boxed::<types::InputPeerUser>::from_bytes(&boxed_expected).unwrap(),
            Boxed(peer)
        );
    }

    #[test]
    fn known_answer_vector_of_longs() {
        let v: Vec<i64> = vec![1, 0x1122_3344_5566_7788, -2];
        let expected: Vec<u8> = [
            0x15, 0xc4, 0xb5, 0x1c, // vector#1cb5c415 LE
            0x03, 0x00, 0x00, 0x00, // len = 3
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // 1
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // 0x1122334455667788
            0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // -2
        ]
        .to_vec();
        assert_eq!(v.to_bytes(), expected);
        assert_eq!(Vec::<i64>::from_bytes(&expected).unwrap(), v);

        // BareVec omits the 0x1cb5c415 constructor ID.
        assert_eq!(BareVec(v.clone()).to_bytes(), expected[4..]);
        assert_eq!(BareVec::<i64>::from_bytes(&expected[4..]).unwrap().0, v);
    }

    #[test]
    fn known_answer_strings_and_padding() {
        // Empty string: [0, 0, 0, 0]
        assert_eq!(String::new().to_bytes(), vec![0, 0, 0, 0]);
        assert_eq!(String::from_bytes(&[0, 0, 0, 0]).unwrap(), "");

        // 1 byte: [1, 'A', 0, 0]
        assert_eq!("A".to_string().to_bytes(), vec![1, b'A', 0, 0]);
        // 2 bytes: [2, 'A', 'B', 0]
        assert_eq!("AB".to_string().to_bytes(), vec![2, b'A', b'B', 0]);
        // 3 bytes: [3, 'A', 'B', 'C'] (exact multiple of 4, no padding)
        assert_eq!("ABC".to_string().to_bytes(), vec![3, b'A', b'B', b'C']);
        // 4 bytes: [4, 'A', 'B', 'C', 'D', 0, 0, 0]
        assert_eq!(
            "ABCD".to_string().to_bytes(),
            vec![4, b'A', b'B', b'C', b'D', 0, 0, 0]
        );

        // 253 bytes (largest short-form length): 1 + 253 = 254 -> +2 zero pad = 256 bytes.
        let s253 = "x".repeat(253);
        let b253 = s253.to_bytes();
        assert_eq!(b253.len(), 256);
        assert_eq!(b253[0], 253);
        assert_eq!(&b253[1..254], s253.as_bytes());
        assert_eq!(&b253[254..256], &[0, 0]);
        assert_eq!(String::from_bytes(&b253).unwrap(), s253);

        // 254 bytes (smallest long-form length): [0xfe, 0xfe, 0x00, 0x00] + 254 bytes + 2 pad = 260.
        let s254 = "y".repeat(254);
        let b254 = s254.to_bytes();
        assert_eq!(b254.len(), 260);
        assert_eq!(&b254[0..4], &[0xfe, 0xfe, 0x00, 0x00]);
        assert_eq!(&b254[4..258], s254.as_bytes());
        assert_eq!(&b254[258..260], &[0, 0]);
        assert_eq!(String::from_bytes(&b254).unwrap(), s254);

        // 300 bytes (0x012c): [0xfe, 0x2c, 0x01, 0x00] + 300 bytes + 0 pad = 304.
        let s300 = "z".repeat(300);
        let b300 = s300.to_bytes();
        assert_eq!(b300.len(), 304);
        assert_eq!(&b300[0..4], &[0xfe, 0x2c, 0x01, 0x00]);
        assert_eq!(&b300[4..304], s300.as_bytes());
        assert_eq!(String::from_bytes(&b300).unwrap(), s300);
    }

    #[test]
    fn non_utf8_string_deserializes_lossily() {
        // 3 bytes: valid 'a', invalid 0xff, valid 'b'
        let raw = [3u8, b'a', 0xff, b'b'];
        let decoded = String::from_bytes(&raw).unwrap();
        assert_eq!(decoded, "a\u{FFFD}b");
        // Vec<u8> preserves exact bytes without loss.
        assert_eq!(Vec::<u8>::from_bytes(&raw).unwrap(), vec![b'a', 0xff, b'b']);
    }

    #[test]
    fn boxed_bool_roundtrip() {
        assert_eq!(true.to_bytes(), BOOL_TRUE_ID.to_le_bytes());
        assert_eq!(false.to_bytes(), BOOL_FALSE_ID.to_le_bytes());
        assert!(bool::from_bytes(&BOOL_TRUE_ID.to_le_bytes()).unwrap());
        assert!(!bool::from_bytes(&BOOL_FALSE_ID.to_le_bytes()).unwrap());
        assert_eq!(
            bool::from_bytes(&0x1234_5678u32.to_le_bytes()),
            Err(DeserializeError::UnexpectedConstructor { id: 0x1234_5678 })
        );
    }

    #[test]
    fn flags_and_flags2_and_shared_bits_behaviour() {
        // Test multi-flag `User` (has both `flags:#` and `flags2:#`, plus shared bit `flags.14`:
        // `bot:flags.14?true` and `bot_info_version:flags.14?int`).
        let u = types::User {
            self_: true, // flags.10
            contact: false,
            mutual_contact: false,
            deleted: false,
            bot: true, // flags.14 (shared with bot_info_version)
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
            premium: true, // flags.28
            attach_menu_enabled: false,
            bot_can_edit: false,
            close_friend: true, // flags2.2
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
            id: 777,
            access_hash: Some(999),              // flags.0
            first_name: Some("Ada".to_string()), // flags.1
            last_name: None,
            username: None,
            phone: None,
            photo: None,
            status: None,
            bot_info_version: Some(3), // flags.14 (shared with bot)
            restriction_reason: None,
            bot_inline_placeholder: None,
            lang_code: None,
            emoji_status: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            bot_active_users: Some(50), // flags2.12
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_community_id: None,
        };

        let bytes = u.to_bytes();
        // First 4 bytes = flags: bits 0, 1, 10, 14, 28
        let expected_flags: u32 = (1 << 0) | (1 << 1) | (1 << 10) | (1 << 14) | (1 << 28);
        let actual_flags = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(actual_flags, expected_flags);

        // Next 4 bytes = flags2: bits 2, 12
        let expected_flags2: u32 = (1 << 2) | (1 << 12);
        let actual_flags2 = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(actual_flags2, expected_flags2);

        let decoded = types::User::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, u);

        // Shared-bit safety: if only one of two Option fields sharing bit 0 in
        // `inputGroupCallStream` is `Some`, bit 0 is NOT set so wire output stays valid.
        let half = types::InputGroupCallStream {
            call: enums::InputGroupCall::InputGroupCall(types::InputGroupCall {
                id: 1,
                access_hash: 2,
            }),
            time_ms: 100,
            scale: 1,
            video_channel: Some(5),
            video_quality: None,
        };
        let half_bytes = half.to_bytes();
        assert_eq!(u32::from_le_bytes(half_bytes[0..4].try_into().unwrap()), 0);
        let half_decoded = types::InputGroupCallStream::from_bytes(&half_bytes).unwrap();
        assert_eq!(half_decoded.video_channel, None);
        assert_eq!(half_decoded.video_quality, None);
    }

    #[test]
    fn generic_remote_call_nesting_and_return_type() {
        fn assert_return_is_config<R: RemoteCall<Return = enums::Config>>(_: &R) {}

        let call = functions::InvokeWithLayer {
            layer: LAYER,
            query: functions::InitConnection {
                api_id: 12345,
                device_model: "PC".to_string(),
                system_version: "Windows".to_string(),
                app_version: "0.1.0".to_string(),
                system_lang_code: "en".to_string(),
                lang_pack: "tdesktop".to_string(),
                lang_code: "en".to_string(),
                proxy: None,
                params: None,
                query: functions::help::GetConfig,
            },
        };
        assert_return_is_config(&call);

        let bytes = call.to_bytes();
        // Starts with invokeWithLayer#da9b0d0d, layer=233, initConnection#c1cd5ea9
        assert_eq!(&bytes[0..4], &0xda9b_0d0du32.to_le_bytes());
        assert_eq!(&bytes[4..8], &233i32.to_le_bytes());
        assert_eq!(&bytes[8..12], &0xc1cd_5ea9u32.to_le_bytes());

        type NestedCall =
            functions::InvokeWithLayer<functions::InitConnection<functions::help::GetConfig>>;
        let decoded = NestedCall::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, call);
    }

    #[test]
    fn bare_and_boxed_future_salt_work_together() {
        let salt = mtproto::types::FutureSalt {
            valid_since: 1000,
            valid_until: 2000,
            salt: 0x0102_0304_0506_0708,
        };
        // Boxed FutureSalt has 4-byte constructor ID + 16 bytes = 20 bytes.
        let boxed_salt = mtproto::enums::FutureSalt::FutureSalt(salt.clone());
        assert_eq!(boxed_salt.to_bytes().len(), 20);
        assert_eq!(
            mtproto::enums::FutureSalt::from_bytes(&boxed_salt.to_bytes()).unwrap(),
            boxed_salt
        );

        // FutureSalts uses `salts:vector<future_salt>` (bare vector of bare future_salt).
        let salts = mtproto::types::FutureSalts {
            req_msg_id: 42,
            now: 1500,
            salts: vec![salt],
        };
        let bytes = salts.to_bytes();
        // 8 (req_msg_id) + 4 (now) + 4 (bare vec len) + 16 (bare future_salt) = 32 bytes.
        assert_eq!(bytes.len(), 32);
        assert_eq!(
            mtproto::types::FutureSalts::from_bytes(&bytes).unwrap(),
            salts
        );
    }

    #[test]
    fn manual_mtproto_framing_and_gzip() {
        // RpcResult
        let inner_pong = mtproto::enums::Pong::Pong(mtproto::types::Pong {
            msg_id: 10,
            ping_id: 20,
        });
        let rpc = Boxed(mtproto::RpcResult {
            req_msg_id: 10,
            result: RawObject(inner_pong.to_bytes()),
        });
        let rpc_bytes = rpc.to_bytes();
        assert_eq!(&rpc_bytes[0..4], &0xf35c_6d01u32.to_le_bytes());
        let rpc_dec = Boxed::<mtproto::RpcResult>::from_bytes(&rpc_bytes).unwrap();
        assert_eq!(rpc_dec, rpc);
        assert_eq!(
            rpc_dec
                .0
                .result
                .deserialize_as::<mtproto::enums::Pong>()
                .unwrap(),
            inner_pong
        );

        // MsgContainer with 2 messages
        let container = Boxed(mtproto::MsgContainer {
            messages: vec![
                mtproto::CoreMessage {
                    msg_id: 100,
                    seqno: 1,
                    body: RawObject(inner_pong.to_bytes()),
                },
                mtproto::CoreMessage {
                    msg_id: 104,
                    seqno: 2,
                    body: RawObject(rpc_bytes.clone()),
                },
            ],
        });
        let c_bytes = container.to_bytes();
        assert_eq!(&c_bytes[0..4], &0x73f1_f8dcu32.to_le_bytes());
        let c_dec = Boxed::<mtproto::MsgContainer>::from_bytes(&c_bytes).unwrap();
        assert_eq!(c_dec, container);

        // GzipPacked
        let payload = b"hello repeated payload ".repeat(50);
        let packed = mtproto::GzipPacked::pack(&payload).unwrap();
        assert!(packed.packed_data.len() < payload.len());
        let wire = Boxed(packed.clone()).to_bytes();
        assert_eq!(&wire[0..4], &0x3072_cfa1u32.to_le_bytes());
        let unpacked_obj = Boxed::<mtproto::GzipPacked>::from_bytes(&wire).unwrap().0;
        assert_eq!(unpacked_obj.unpack().unwrap(), payload);
        // Limit enforcement against zip bombs
        assert!(matches!(
            unpacked_obj.unpack_with_limit(16),
            Err(DeserializeError::GzipError(_))
        ));
    }

    #[test]
    fn constructor_id_debug_table() {
        assert_eq!(from_constructor_id(0xbe7e_8ef1), Some("req_pq_multi"));
        assert_eq!(from_constructor_id(0xdde8_a54c), Some("inputPeerUser"));
        assert_eq!(from_constructor_id(0xda9b_0d0d), Some("invokeWithLayer"));
        assert_eq!(from_constructor_id(0xf35c_6d01), Some("rpc_result"));
        assert_eq!(from_constructor_id(0x73f1_f8dc), Some("msg_container"));
        assert_eq!(from_constructor_id(0x3072_cfa1), Some("gzip_packed"));
        assert_eq!(from_constructor_id(0x1cb5_c415), Some("vector"));
        assert_eq!(from_constructor_id(0x9972_75b5), Some("boolTrue"));
        assert_eq!(from_constructor_id(0xbc79_9737), Some("boolFalse"));
        assert_eq!(from_constructor_id(0x0000_0000), None);
    }

    #[test]
    fn deserialization_error_cases() {
        assert!(matches!(
            i32::from_bytes(&[1, 2]),
            Err(DeserializeError::UnexpectedEof {
                needed: 4,
                remaining: 2
            })
        ));
        assert_eq!(
            Vec::<u8>::from_bytes(&[0xff, 0, 0, 0]),
            Err(DeserializeError::InvalidStringLength { prefix: 0xff })
        );
        // Negative vector length
        let mut neg_vec = VECTOR_ID.to_le_bytes().to_vec();
        neg_vec.extend_from_slice(&(-5i32).to_le_bytes());
        assert_eq!(
            Vec::<i32>::from_bytes(&neg_vec),
            Err(DeserializeError::NegativeLength(-5))
        );
        // Malicious huge vector length on tiny buffer does NOT OOM, returns EOF
        let mut huge_vec = VECTOR_ID.to_le_bytes().to_vec();
        huge_vec.extend_from_slice(&i32::MAX.to_le_bytes());
        assert!(matches!(
            Vec::<i32>::from_bytes(&huge_vec),
            Err(DeserializeError::UnexpectedEof { .. })
        ));
        // Unknown constructor ID
        assert_eq!(
            enums::InputPeer::from_bytes(&0xdead_beefu32.to_le_bytes()),
            Err(DeserializeError::UnexpectedConstructor { id: 0xdead_beef })
        );
    }
}
