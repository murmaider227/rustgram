//! End-to-end synthetic `tdata` byte-compatibility and known-answer tests.
//!
//! Never accesses any real user `tdata`; operates exclusively inside `tempfile::TempDir`.

use std::fs;

use tempfile::TempDir;
use tg_crypto::{
    AuthKey, LOCAL_ENCRYPT_SALT_SIZE, PasscodeKdf, PasscodeKdfKind, aes_decrypt_local,
    create_legacy_local_key, create_local_key, md5, sha1,
};
use tg_storage::cache::binlog::{BinlogCache, CacheKey};
use tg_storage::domain::{
    AccountsInfo, DomainStorage, KEY_DATA_FORMAT_VERSION, KEY_DATA_MAGIC, read_key_data,
};
use tg_storage::local::{LocalStorage, read_legacy_settings_stream};
use tg_storage::qdatastream::{QDataStreamReader, QDataStreamWriter};
use tg_storage::serialize::peer::{
    SerializedPeer, SerializedPeerKind, SerializedUser, deserialize_self_user, peek_user_phone,
    serialize_self_user,
};
use tg_storage::settings_scheme::{DbiBlockId, ReadSettingsContext};
use tg_storage::tdf::{
    APP_VERSION, FileWriteDescriptor, TDF_MAGIC, compose_data_string, compute_data_name_key,
    decode_tdf_bytes, decrypt_local, prepare_encrypted, to_file_part,
};
use tg_storage::{AccountStorage, MtpAuthKeyEntry, MtpAuthorization, PeerId};

#[test]
fn synthetic_tdata_exact_on_disk_wire_layout() {
    let tmp = TempDir::new().expect("tempdir");
    let tdata = tmp.path().join("tdata");

    // 1. Initialize global settings (`tdata/settingss`)
    let mut local = LocalStorage::open_or_init(&tdata).expect("init local");
    local.settings_mut().auto_start = true;
    local.settings_mut().scale_percent = 125;
    local.settings_mut().night_mode = false;
    local.write_settings().expect("write settings");

    // Inspect raw bytes of `tdata/settingss` directly against tdesktop's TDF$ spec
    let raw_settings = fs::read(tdata.join("settingss")).expect("read raw settingss");
    assert_eq!(&raw_settings[0..4], &TDF_MAGIC);
    assert_eq!(&raw_settings[4..8], &APP_VERSION.to_le_bytes());

    let settings_payload = &raw_settings[8..raw_settings.len() - 16];
    let settings_md5 = &raw_settings[raw_settings.len() - 16..];
    let expected_settings_md5 = md5(&[
        settings_payload,
        &(settings_payload.len() as i32).to_le_bytes(),
        &APP_VERSION.to_le_bytes(),
        b"TDF$",
    ]);
    assert_eq!(settings_md5, expected_settings_md5);

    // Decrypt `settingss` by hand using CreateLegacyLocalKey("", salt)
    let mut sr = QDataStreamReader::new(settings_payload);
    let settings_salt = sr.read_byte_array().expect("salt");
    assert_eq!(settings_salt.len(), LOCAL_ENCRYPT_SALT_SIZE);
    let settings_enc = sr.read_byte_array().expect("enc");
    assert!(sr.at_end());
    let derived_settings_key = create_legacy_local_key(b"", &settings_salt);
    let decrypted_settings =
        decrypt_local(&settings_enc, &derived_settings_key).expect("hand decrypt settings");
    // First block written by Local::writeSettings() is dbiAutoStart (0x06) = 1
    assert_eq!(
        &decrypted_settings[0..8],
        &[0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01]
    );

    // 2. Create domain (`tdata/key_datas`) with deterministic local key
    let local_key = AuthKey::new([0x4Bu8; 256]);
    let mut domain = DomainStorage::create_with_local_key(
        &tdata,
        "data",
        local_key.clone(),
        AccountsInfo {
            indices: vec![0, 1],
            active: 0,
        },
    )
    .expect("create domain");

    // Inspect `key_datas` raw TDF + v2 trailer structure
    let raw_key_data = fs::read(tdata.join("key_datas")).expect("read raw key_datas");
    let decoded_kd = decode_tdf_bytes(&raw_key_data, APP_VERSION).expect("decode key_datas");
    let kd = read_key_data(&decoded_kd.data).expect("parse key_data");
    assert!(!kd.legacy);
    assert_eq!(kd.open_salt.len(), 32);
    assert_eq!(kd.open_key_encrypted.len(), 16 + 272); // 16-byte key128 + ceil16(4+256)

    // Verify 4th blob in `key_datas` starts with magic 0x4B443200 ("KD2\0") and version 2
    let mut kdr = QDataStreamReader::new(&decoded_kd.data);
    let _b1 = kdr.read_byte_array().expect("b1");
    let _b2 = kdr.read_byte_array().expect("b2");
    let _b3 = kdr.read_byte_array().expect("b3");
    let trailer = kdr.read_byte_array().expect("trailer");
    assert!(kdr.at_end());
    assert_eq!(&trailer[0..4], &KEY_DATA_MAGIC.to_be_bytes());
    assert_eq!(&trailer[4..8], &KEY_DATA_FORMAT_VERSION.to_be_bytes());

    // Verify open wrap decrypts with CreateLocalKey("", open_salt)
    let open_wrap_key = create_local_key(b"", &kd.open_salt);
    let unwrapped_local = decrypt_local(&kd.open_key_encrypted, &open_wrap_key).expect("unwrap");
    assert_eq!(unwrapped_local.as_slice(), &local_key.data);

    // 3. Populate account 0 ("data" -> D877F783D5D3EF8C) and account 1 ("data#2")
    let mut acc0 = AccountStorage::open(&tdata, "data", 0, local_key.clone()).expect("open acc0");
    let self_peer = SerializedPeer {
        id: PeerId::from_user(7_000_000_001),
        userpic_photo_id: 0,
        userpic_location: Default::default(),
        userpic_has_video: false,
        kind: SerializedPeerKind::User(SerializedUser {
            first_name: "Synthetic".into(),
            last_name: "User".into(),
            phone: "0000000000".into(),
            username: "synthetic_test".into(),
            access_hash: 0x1234_5678,
            flags: 0,
            bot_inline_placeholder: String::new(),
            lastseen: 0,
            is_contact: false,
            bot_info_version: -1,
            supports_guest_chat: false,
        }),
    };
    acc0.map_mut().self_serialized = serialize_self_user(&self_peer, "Synthetic bio");
    acc0.write_map().expect("write acc0 map");

    // Verify raw `D877F783D5D3EF8C/maps` begins with two 0xFFFFFFFF null QByteArrays
    let raw_map = fs::read(tdata.join("D877F783D5D3EF8C").join("maps")).expect("raw maps");
    let decoded_map = decode_tdf_bytes(&raw_map, APP_VERSION).expect("decode maps");
    assert_eq!(
        &decoded_map.data[0..8],
        &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
    );

    // Write MTP authorization for account 0 -> `tdata/D877F783D5D3EF8Cs`
    let auth0 = MtpAuthorization {
        user_id: 7_000_000_001,
        main_dc_id: 2,
        keys: vec![
            MtpAuthKeyEntry {
                dc_id: 2,
                key: AuthKey::new([0x21u8; 256]),
            },
            MtpAuthKeyEntry {
                dc_id: 4,
                key: AuthKey::new([0x41u8; 256]),
            },
        ],
        keys_to_destroy: vec![],
    };
    acc0.write_mtp_data(&auth0).expect("write mtp0");
    assert!(tdata.join("D877F783D5D3EF8Cs").exists());

    // Account 1 uses "data#2"
    let acc1_part = to_file_part(compute_data_name_key(&compose_data_string("data", 1)));
    let acc1 = AccountStorage::open(&tdata, "data", 1, local_key.clone()).expect("open acc1");
    let auth1 = MtpAuthorization {
        user_id: 7_000_000_002,
        main_dc_id: 4,
        keys: vec![MtpAuthKeyEntry {
            dc_id: 4,
            key: AuthKey::new([0x42u8; 256]),
        }],
        keys_to_destroy: vec![],
    };
    acc1.write_mtp_data(&auth1).expect("write mtp1");
    acc1.write_map().expect("write acc1 map");
    assert!(tdata.join(format!("{acc1_part}s")).exists());
    assert!(tdata.join(&acc1_part).join("maps").exists());

    // 4. Write an entry into account 0's lib_storage binlog cache (`user_data/cache`)
    {
        let mut cache = BinlogCache::open(acc0.cache_path(), acc0.cache_key(), true)
            .expect("open binlog cache");
        cache
            .put(CacheKey::new(0x0102, 999), b"cached-jpeg-bytes", 1)
            .expect("cache put");
    }

    // 5. Arm a modern Argon2id passcode on the domain, reopen everything from scratch
    let fast_kdf = PasscodeKdf {
        kind: PasscodeKdfKind::Argon2id,
        memory: 64,
        time: 1,
        parallel: 1,
    };
    domain
        .set_passcode(b"synth-pass-123", fast_kdf, true)
        .expect("set passcode");

    let reopened_domain =
        DomainStorage::open(&tdata, "data", b"synth-pass-123").expect("reopen domain");
    assert_eq!(reopened_domain.accounts().indices, vec![0, 1]);
    assert_eq!(reopened_domain.accounts().active, 0);

    let reopened_acc0 =
        AccountStorage::open(&tdata, "data", 0, reopened_domain.local_key().clone())
            .expect("reopen acc0");
    assert_eq!(reopened_acc0.read_mtp_data().expect("mtp0"), Some(auth0));
    assert_eq!(
        peek_user_phone(APP_VERSION, &reopened_acc0.map().self_serialized).expect("peek"),
        Some("0000000000".into())
    );
    let (decoded_self, about) =
        deserialize_self_user(APP_VERSION, &reopened_acc0.map().self_serialized)
            .expect("self user");
    assert_eq!(decoded_self, self_peer);
    assert_eq!(about, "Synthetic bio");

    let reopened_acc1 =
        AccountStorage::open(&tdata, "data", 1, reopened_domain.local_key().clone())
            .expect("reopen acc1");
    assert_eq!(reopened_acc1.read_mtp_data().expect("mtp1"), Some(auth1));

    let reopened_cache =
        BinlogCache::open(reopened_acc0.cache_path(), reopened_acc0.cache_key(), true)
            .expect("reopen cache");
    assert_eq!(
        reopened_cache
            .get(CacheKey::new(0x0102, 999))
            .expect("cache get")
            .as_deref(),
        Some(&b"cached-jpeg-bytes"[..])
    );
}

#[test]
fn hand_verified_prepare_encrypted_and_decrypt_local_wire_format() {
    let key = AuthKey::new([0x5Au8; 256]);
    let payload = b"0123456789AB"; // 12 bytes -> 4 + 12 = 16 bytes (no random padding needed!)

    let encrypted = prepare_encrypted(payload, &key).expect("encrypt");
    // Exact size: 16 (key128) + 16 (1 AES block) = 32 bytes
    assert_eq!(encrypted.len(), 32);

    // Independently verify plaintext = LE(16u32) || payload
    let mut expected_pt = Vec::with_capacity(16);
    expected_pt.extend_from_slice(&16u32.to_le_bytes());
    expected_pt.extend_from_slice(payload);

    let expected_sha1 = sha1(&[&expected_pt]);
    assert_eq!(&encrypted[0..16], &expected_sha1[0..16]);

    let mut key128 = [0u8; 16];
    key128.copy_from_slice(&encrypted[0..16]);
    let decrypted_block = aes_decrypt_local(&encrypted[16..], &key, &key128).expect("raw decrypt");
    assert_eq!(decrypted_block, expected_pt);

    let roundtrip = decrypt_local(&encrypted, &key).expect("decrypt_local");
    assert_eq!(roundtrip, payload);
}

#[test]
fn legacy_single_account_map_with_passcode_opens_correctly() {
    let tmp = TempDir::new().expect("tempdir");
    let tdata = tmp.path().join("tdata");
    let acc_dir = tdata.join(to_file_part(compute_data_name_key("data")));
    fs::create_dir_all(&acc_dir).expect("mkdir");

    let salt = [0x7Eu8; LOCAL_ENCRYPT_SALT_SIZE];
    let passcode = b"legacy-pass";
    let passcode_key = create_legacy_local_key(passcode, &salt);
    let local_key = AuthKey::new([0xC3u8; 256]);
    let legacy_key_enc = prepare_encrypted(&local_key.data, &passcode_key).expect("wrap");

    let mut map_payload = QDataStreamWriter::new();
    map_payload.write_u32(tg_storage::LskKeyType::Locations as u32);
    map_payload.write_u64(0x1122_3344_5566_7788);
    let map_enc = prepare_encrypted(map_payload.as_bytes(), &local_key).expect("map enc");

    let mut desc = FileWriteDescriptor::new("map", &acc_dir);
    desc.write_data(&salt);
    desc.write_data(&legacy_key_enc);
    desc.write_data(&map_enc);
    desc.finish().expect("write legacy map");

    // Wrong passcode fails
    assert!(AccountStorage::open_legacy(&tdata, "data", b"wrong").is_err());

    // Correct passcode recovers local_key and map
    let acc = AccountStorage::open_legacy(&tdata, "data", passcode).expect("open legacy");
    assert_eq!(acc.local_key(), &local_key);
    assert_eq!(acc.map().locations_key, 0x1122_3344_5566_7788);
}

#[test]
fn legacy_pre_tdf_inline_encrypted_settings_stream() {
    let salt = [0x22u8; LOCAL_ENCRYPT_SALT_SIZE];
    let old_key = create_legacy_local_key(b"", &salt);

    // Build inner encrypted block with dbiUser + dbiKey
    let mut inner = QDataStreamWriter::new();
    inner.write_u32(DbiBlockId::User as u32);
    inner.write_i32(4242);
    inner.write_u32(2);
    inner.write_u32(DbiBlockId::Key as u32);
    inner.write_i32(2);
    inner.write_raw(&[0x99u8; 256]);
    let enc_block = prepare_encrypted(inner.as_bytes(), &old_key).expect("enc");

    let mut outer = QDataStreamWriter::new();
    outer.write_u32(DbiBlockId::Version as u32);
    outer.write_i32(9000);
    outer.write_u32(DbiBlockId::EncryptedWithSalt as u32);
    outer.write_byte_array(&salt);
    outer.write_byte_array(&enc_block);

    let mut parsed_key = None;
    let mut ctx = ReadSettingsContext::default();
    read_legacy_settings_stream(outer.as_bytes(), &mut parsed_key, &mut ctx).expect("read legacy");

    assert_eq!(ctx.mtp_legacy_user_id, 4242);
    assert_eq!(ctx.mtp_legacy_main_dc_id, 2);
    assert_eq!(ctx.mtp_legacy_keys.len(), 1);
    assert_eq!(&ctx.mtp_legacy_keys[0].key.data, &[0x99u8; 256]);
}
