use tg_calls::dh::{
    DH_GOOD_PRIME as GOOD_PRIME, DhConfig, RANDOM_POWER_SIZE, finish_dh_key_exchange,
    generate_mod_exp_first_with_rng,
};
use tg_calls::emoji_fingerprint::{
    EMOJI_COUNT, compute_call_emoji_fingerprint, compute_emoji_fingerprint,
    compute_emoji_fingerprint_indices, compute_emoji_fingerprint_slice, compute_emoji_index,
    emoji_by_index,
};

#[test]
fn emoji_table_has_exact_333_valid_entries_matching_tdesktop() {
    assert_eq!(EMOJI_COUNT, 333);

    for i in 0..EMOJI_COUNT {
        let emoji = emoji_by_index(i).expect("every entry in Data/Offsets must be valid UTF-16");
        assert!(!emoji.is_empty(), "emoji at index {i} is empty");
    }
    assert!(emoji_by_index(EMOJI_COUNT).is_none());

    // Spot-check boundary and multi-code-unit entries directly against calls_emoji_fingerprint.cpp:
    // Index 0: 0xd83d, 0xde09 -> U+1F609 (😉)
    assert_eq!(emoji_by_index(0).as_deref(), Some("\u{1F609}"));
    // Index 1: 0xd83d, 0xde0d -> U+1F60D (😍)
    assert_eq!(emoji_by_index(1).as_deref(), Some("\u{1F60D}"));
    // Index 43: offset 86..87 = 0x270c -> U+270C (✌) (single UTF-16 code unit!)
    assert_eq!(emoji_by_index(43).as_deref(), Some("\u{270C}"));
    // Index 44: offset 87..88 = 0x270b -> U+270B (✋)
    assert_eq!(emoji_by_index(44).as_deref(), Some("\u{270B}"));
    // Index 301: offset 578..582 = 0xd83c, 0xddef, 0xd83c, 0xddf5 -> 🇯🇵 (4 UTF-16 units)
    assert_eq!(emoji_by_index(301).as_deref(), Some("\u{1F1EF}\u{1F1F5}"));
    // Index 311: offset 618..620 = 0x0031, 0x20e3 -> 1️⃣
    assert_eq!(emoji_by_index(311).as_deref(), Some("1\u{20E3}"));
    // Last index 332: offset 656..658 = 0xd83d, 0xdd37 -> U+1F537 (🔷)
    assert_eq!(emoji_by_index(332).as_deref(), Some("\u{1F537}"));
}

#[test]
fn emoji_fingerprint_known_answer_vectors_from_cpp_algorithm() {
    // Vector 1: all-zero 32-byte SHA-256 -> [0, 0, 0, 0] -> 4x 😉
    let zeros = [0u8; 32];
    assert_eq!(compute_emoji_fingerprint_indices(&zeros), [0, 0, 0, 0]);
    assert_eq!(
        compute_emoji_fingerprint(&zeros),
        ["\u{1F609}", "\u{1F609}", "\u{1F609}", "\u{1F609}"]
    );

    // Vector 2: MSB masking check (& 0x7F on byte 0 of each 8-byte chunk).
    assert_eq!(
        compute_emoji_index(&[0x80, 0, 0, 0, 0, 0, 0, 0]),
        compute_emoji_index(&[0x00, 0, 0, 0, 0, 0, 0, 0])
    );
    assert_eq!(
        compute_emoji_index(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
        0x7FFF_FFFF_FFFF_FFFF
    );

    // Vector 3: explicit known chunks:
    // chunk 0 = 0x8000_0000_0000_0001 -> masked = 1 -> index 1 (😍)
    // chunk 1 = 0x0000_0000_0000_014D (333) -> 333 % 333 = 0 -> index 0 (😉)
    // chunk 2 = 0x0000_0000_0000_014C (332) -> 332 % 333 = 332 -> index 332 (🔷)
    // chunk 3 = 0xFFFF_FFFF_FFFF_FFFF -> 0x7FFF_FFFF_FFFF_FFFF % 333 = 141
    let expected_idx3 = (0x7FFF_FFFF_FFFF_FFFFu64 % 333) as usize;
    let mut vec3 = [0u8; 32];
    vec3[0..8].copy_from_slice(&0x8000_0000_0000_0001u64.to_be_bytes());
    vec3[8..16].copy_from_slice(&333u64.to_be_bytes());
    vec3[16..24].copy_from_slice(&332u64.to_be_bytes());
    vec3[24..32].copy_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_be_bytes());

    let indices = compute_emoji_fingerprint_indices(&vec3);
    assert_eq!(indices, [1, 0, 332, expected_idx3]);

    let emojis = compute_emoji_fingerprint(&vec3);
    assert_eq!(emojis[0], "\u{1F60D}");
    assert_eq!(emojis[1], "\u{1F609}");
    assert_eq!(emojis[2], "\u{1F537}");
    assert_eq!(emojis[3], emoji_by_index(expected_idx3).unwrap());

    // Slice helper rejects non-multiples of 8 bytes.
    assert!(compute_emoji_fingerprint_slice(&[0u8; 7]).is_none());
    assert_eq!(
        compute_emoji_fingerprint_slice(&vec3).unwrap(),
        emojis.to_vec()
    );
}

#[test]
fn call_emoji_fingerprint_matches_across_dh_peers() {
    let config = DhConfig {
        version: 1,
        g: 3,
        p: GOOD_PRIME.to_vec(),
    };
    let a = generate_mod_exp_first_with_rng(&config, &[0xA1; RANDOM_POWER_SIZE], |b| b.fill(0x1A))
        .unwrap();
    let b = generate_mod_exp_first_with_rng(&config, &[0xB2; RANDOM_POWER_SIZE], |b| b.fill(0x2B))
        .unwrap();

    let (key_a, _) = finish_dh_key_exchange(&config, &b.modexp, &a.random_power).unwrap();
    let (key_b, _) = finish_dh_key_exchange(&config, &a.modexp, &b.random_power).unwrap();

    let emoji_a = compute_call_emoji_fingerprint(&key_a, &a.modexp);
    let emoji_b = compute_call_emoji_fingerprint(&key_b, &a.modexp);
    assert_eq!(emoji_a, emoji_b);
    assert_eq!(emoji_a.len(), 4);
}
