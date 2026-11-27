//! `AuthKey` computes `auth_key_id` once at construction (TGB-3074). Known answers are
//! SHA-1 bytes 12..20 of the key, computed independently with Python's `hashlib`.

use tg_crypto::{AuthKey, Side, decrypt_data_v2, encrypt_data_v2};

fn patterned() -> [u8; 256] {
    let mut data = [0u8; 256];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(29).wrapping_add(11);
    }
    data
}

#[test]
fn key_id_known_answers_for_new_and_from_bytes() {
    let key = AuthKey::new(patterned());
    assert_eq!(
        key.key_id(),
        [0xe4, 0x18, 0x2d, 0x73, 0x2c, 0xa4, 0x29, 0xf3]
    );
    assert_eq!(key.key_id_u64(), 17_521_716_336_079_476_964);
    assert_eq!(key.data(), &patterned());

    let short = AuthKey::from_bytes(&[0xab, 0xcd]).expect("short key");
    assert_eq!(
        short.key_id(),
        [0x8d, 0x77, 0xb2, 0x58, 0xf1, 0xa7, 0x62, 0x6f]
    );
    assert_eq!(short, AuthKey::new(*short.data()));
}

#[test]
fn cloned_key_keeps_its_id_and_encrypts_the_same() {
    let key = AuthKey::new(patterned());
    let clone = key.clone();
    assert_eq!(clone.key_id(), key.key_id());
    assert_eq!(clone, key);

    let wire = encrypt_data_v2(b"0123456789abcdef0123456789abcdef", &key, Side::Client);
    assert_eq!(&wire[..8], &key.key_id());
    let plain = decrypt_data_v2(&wire, &clone, Side::Client).expect("decrypt with clone");
    assert!(plain.starts_with(b"0123456789abcdef0123456789abcdef"));

    // A packet for another key is rejected by its auth_key_id.
    let mut other = patterned();
    other[0] ^= 1;
    assert!(decrypt_data_v2(&wire, &AuthKey::new(other), Side::Client).is_err());
}
