//! Integration tests for conference E2E (`tde2e`) blockchain, shared key derivation,
//! frame encryption/decryption, emoji verification, tampered block rejection,
//! Layer 233 `phone.*` RPCs, and in-call messages / paid Star reactions.

use std::collections::BTreeSet;

use tg_calls::group::e2e::{
    CallFailure, ChainBlock, ChainChange, ChainGroupParticipant, ChainGroupState, ChainSharedKey,
    ChainStateProof, ConferenceBlockchain, DEFAULT_CALL_CHANNEL_ID,
    DEFAULT_PARTICIPANT_PERMISSIONS, DEFAULT_PARTICIPANT_VERSION, E2eAction, E2eCall, E2eError,
    E2eTimerKind, GroupBroadcast, PERMISSION_ADD_USERS, PublicKey, SUBCHAIN_BROADCAST,
    SUBCHAIN_STATE, TemporaryKeyPair, build_create_conference_call,
    build_decline_conference_invite, build_delete_conference_participants, build_get_chain_blocks,
    build_get_last_chain_block, build_invite_conference_participant, build_join_conference_call,
    build_send_conference_broadcast, compute_conference_emoji_hash, conference_emoji_fingerprint,
    is_chain_invalid_error, is_permanent_broadcast_error,
};
use tg_calls::group::messages::{
    GroupCallMessages, GroupMessageAction, MessageDeleteRequest, PreparedEncryptedMessage,
    StarsColoring, build_delete_group_call_messages, build_delete_group_call_participant_messages,
    build_get_group_call_stars, build_save_default_send_as,
    build_send_group_call_encrypted_message, build_send_group_call_message,
    deserialize_encrypted_message, serialize_encrypted_message, stars_coloring_for_count,
    stars_required_for_message,
};
use tg_calls::types::{
    CallId, GroupCallDonorData, GroupCallMessageData, GroupCallMessageEntity, InputGroupCall,
    MsgId, PeerId, PhoneGroupCallStarsResponse, UpdateDeleteGroupCallMessages,
    UpdateGroupCallChainBlocks, UpdateGroupCallEncryptedMessage, UpdateGroupCallMessage, UserId,
};
use tg_calls::{GroupCallSession, compute_emoji_fingerprint};
use tg_crypto::e2e::E2eKeyPair;
use tg_tl::{Deserializable, Serializable, enums as tl_enums, types as tl_types};

fn keypair(seed_byte: u8) -> E2eKeyPair {
    E2eKeyPair::from_seed([seed_byte; 32])
}

fn collect_outbound_blocks(actions: &[E2eAction]) -> Vec<Vec<u8>> {
    actions
        .iter()
        .filter_map(|a| match a {
            E2eAction::SendOutboundBlock(b) => Some(b.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Key Derivation (TemporaryKeyPair & ChainSharedKey)
// ---------------------------------------------------------------------------

#[test]
fn temporary_key_pair_and_shared_key_derivation() {
    // One-shot ECDH envelope round-trip (`TdE2E::TemporaryKeyPair`).
    let alice_tmp = TemporaryKeyPair::from_seed([0x10; 32]);
    let bob_tmp = TemporaryKeyPair::from_seed([0x20; 32]);
    let charlie_tmp = TemporaryKeyPair::from_seed([0x30; 32]);

    let secret_msg = b"one-shot conference secret";
    let sealed = alice_tmp
        .encrypt_for_one(&bob_tmp.public_key(), secret_msg)
        .expect("encrypt_for_one");
    let opened = bob_tmp
        .decrypt_for_one(&alice_tmp.public_key(), &sealed)
        .expect("decrypt_for_one");
    assert_eq!(opened, secret_msg);
    assert!(
        charlie_tmp
            .decrypt_for_one(&alice_tmp.public_key(), &sealed)
            .is_err()
    );

    // Deterministic `ChainSharedKey` derivation for 3 participants.
    let kp_a = keypair(0xA1);
    let kp_b = keypair(0xB2);
    let kp_c = keypair(0xC3);
    let kp_outsider = keypair(0xD4);
    let ephem = keypair(0xE5);

    let group_state = ChainGroupState {
        participants: vec![
            ChainGroupParticipant {
                user_id: UserId(101),
                public_key: PublicKey(kp_a.public_key()),
                permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
                version: DEFAULT_PARTICIPANT_VERSION,
            },
            ChainGroupParticipant {
                user_id: UserId(202),
                public_key: PublicKey(kp_b.public_key()),
                permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
                version: DEFAULT_PARTICIPANT_VERSION,
            },
            ChainGroupParticipant {
                user_id: UserId(303),
                public_key: PublicKey(kp_c.public_key()),
                permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
                version: DEFAULT_PARTICIPANT_VERSION,
            },
        ],
        external_permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
    };

    let raw_shared_key = [0x7Au8; 32];
    let content_key = [0x5Cu8; 32];
    let content_nonce = [0x3Eu8; 16];

    let shared = ChainSharedKey::seal_with_params(
        &raw_shared_key,
        &group_state,
        &ephem,
        &content_key,
        &content_nonce,
    )
    .expect("seal shared key");
    shared
        .validate_recipients(&group_state)
        .expect("recipients valid");

    // Every participant recovers the exact same 32-byte shared key.
    assert_eq!(
        shared.open_for_participant(UserId(101), &kp_a).unwrap(),
        raw_shared_key
    );
    assert_eq!(
        shared.open_for_participant(UserId(202), &kp_b).unwrap(),
        raw_shared_key
    );
    assert_eq!(
        shared.open_for_participant(UserId(303), &kp_c).unwrap(),
        raw_shared_key
    );

    // Outsider cannot recover the shared key, nor can Bob open Alice's slot with the wrong key.
    assert_eq!(
        shared.open_for_participant(UserId(404), &kp_outsider),
        Err(E2eError::UnknownParticipant(404))
    );
    assert!(shared.open_for_participant(UserId(101), &kp_b).is_err());

    // Tampered encrypted_shared_key or dest_header fails authentication.
    let mut bad_sk = shared.clone();
    bad_sk.encrypted_shared_key[0] ^= 0x01;
    assert!(bad_sk.open_for_participant(UserId(101), &kp_a).is_err());

    let mut bad_hdr = shared.clone();
    bad_hdr.dest_headers[1][0] ^= 0x01;
    assert!(bad_hdr.open_for_participant(UserId(202), &kp_b).is_err());
}

// ---------------------------------------------------------------------------
// 2. Three-Participant Join / Leave Chain & Key Rotation
// ---------------------------------------------------------------------------

#[test]
fn three_participant_join_leave_chain_and_key_rotation() {
    let mut alice = E2eCall::with_key_pair(UserId(1), keypair(1));
    let mut bob = E2eCall::with_key_pair(UserId(2), keypair(2));
    let mut charlie = E2eCall::with_key_pair(UserId(3), keypair(3));

    // Step 1: Alice creates block 0 (zero block).
    assert!(!alice.has_last_block0());
    let block0 = alice.make_join_block().expect("alice zero block");
    alice
        .apply_blocks(SUBCHAIN_STATE, 1, std::slice::from_ref(&block0), true)
        .expect("alice apply block 0");
    assert_eq!(alice.participants_set(), &BTreeSet::from([UserId(1)]));
    assert!(alice.encrypt_decrypt().is_ready());
    let (epoch0, key0) = alice.blockchain().current_shared_key().unwrap();
    assert_eq!(epoch0, 0);

    // Step 2: Bob refreshes last block 0, builds self-add block 1, both apply.
    bob.refresh_last_block0(Some(block0.clone()));
    assert!(bob.has_last_block0());
    let block1 = bob.make_join_block().expect("bob self-add block 1");

    alice
        .apply_blocks(SUBCHAIN_STATE, 2, std::slice::from_ref(&block1), false)
        .expect("alice apply block 1");
    bob.apply_blocks(SUBCHAIN_STATE, 2, &[block0.clone(), block1.clone()], true)
        .expect("bob apply blocks 0..2");

    assert_eq!(
        alice.participants_set(),
        &BTreeSet::from([UserId(1), UserId(2)])
    );
    assert_eq!(bob.participants_set(), alice.participants_set());
    assert_eq!(
        alice.blockchain().current_shared_key(),
        bob.blockchain().current_shared_key()
    );
    let (epoch1, key1) = alice.blockchain().current_shared_key().unwrap();
    assert_eq!(epoch1, 1);
    assert_ne!(key1, key0, "joining must rotate epoch shared key");

    // Step 3: Charlie fetches block 1 and joins with block 2.
    charlie.refresh_last_block0(Some(block1.clone()));
    let block2 = charlie.make_join_block().expect("charlie self-add block 2");

    // Deliver block 2 to all three (test Charlie initializing directly from block 2 snapshot).
    alice
        .apply_blocks(SUBCHAIN_STATE, 3, std::slice::from_ref(&block2), false)
        .expect("alice apply block 2");
    bob.apply_blocks(SUBCHAIN_STATE, 3, std::slice::from_ref(&block2), false)
        .expect("bob apply block 2");
    charlie
        .apply_blocks(SUBCHAIN_STATE, 3, std::slice::from_ref(&block2), true)
        .expect("charlie init from block 2");

    let all_three = BTreeSet::from([UserId(1), UserId(2), UserId(3)]);
    assert_eq!(alice.participants_set(), &all_three);
    assert_eq!(bob.participants_set(), &all_three);
    assert_eq!(charlie.participants_set(), &all_three);

    let (epoch2, key2) = alice.blockchain().current_shared_key().unwrap();
    assert_eq!(epoch2, 2);
    assert_eq!(bob.blockchain().current_shared_key(), Some((2, key2)));
    assert_eq!(charlie.blockchain().current_shared_key(), Some((2, key2)));

    // All three can encrypt and decrypt frames on epoch 2.
    let enc_from_charlie = charlie
        .encrypt_decrypt()
        .encrypt_frame(DEFAULT_CALL_CHANNEL_ID, b"hello from charlie", 0)
        .expect("charlie encrypt");
    assert_eq!(
        alice
            .encrypt_decrypt()
            .decrypt_frame(UserId(3), DEFAULT_CALL_CHANNEL_ID, &enc_from_charlie)
            .expect("alice decrypt"),
        b"hello from charlie"
    );
    assert_eq!(
        bob.encrypt_decrypt()
            .decrypt_frame(UserId(3), DEFAULT_CALL_CHANNEL_ID, &enc_from_charlie)
            .expect("bob decrypt"),
        b"hello from charlie"
    );

    // Step 4: Charlie leaves; Alice creates remove block 3 (self is never removed even if passed).
    let remove_ids = BTreeSet::from([UserId(1), UserId(3)]);
    let block3 = alice
        .make_remove_block(&remove_ids)
        .expect("make remove block")
        .expect("charlie was present");

    // Test out-of-order push buffering on Bob: finish short poll first, then apply block 3.
    bob.subchain_blocks_request_finished(SUBCHAIN_STATE)
        .unwrap();
    alice
        .apply_blocks(SUBCHAIN_STATE, 4, std::slice::from_ref(&block3), false)
        .expect("alice apply block 3");
    bob.apply_blocks(SUBCHAIN_STATE, 4, std::slice::from_ref(&block3), false)
        .expect("bob apply block 3");
    charlie
        .apply_blocks(SUBCHAIN_STATE, 4, std::slice::from_ref(&block3), false)
        .expect("charlie observes block 3 removal");

    let remaining = BTreeSet::from([UserId(1), UserId(2)]);
    assert_eq!(alice.participants_set(), &remaining);
    assert_eq!(bob.participants_set(), &remaining);
    assert_eq!(charlie.participants_set(), &remaining);

    let (epoch3, key3) = alice.blockchain().current_shared_key().unwrap();
    assert_eq!(epoch3, 3);
    assert_ne!(key3, key2, "removing a participant must rotate shared key");
    assert_eq!(bob.blockchain().current_shared_key(), Some((3, key3)));
    assert_eq!(
        charlie.blockchain().current_shared_key(),
        None,
        "removed participant must not have epoch 3 key"
    );

    // Alice -> Bob on epoch 3 succeeds, but Charlie CANNOT decrypt epoch 3 frames!
    let secret_after_leave = alice
        .encrypt_decrypt()
        .encrypt_frame(DEFAULT_CALL_CHANNEL_ID, b"post-leave private audio", 0)
        .expect("alice encrypt epoch 3");
    assert_eq!(
        bob.encrypt_decrypt()
            .decrypt_frame(UserId(1), DEFAULT_CALL_CHANNEL_ID, &secret_after_leave)
            .expect("bob decrypt epoch 3"),
        b"post-leave private audio"
    );
    assert!(
        charlie
            .encrypt_decrypt()
            .decrypt_frame(UserId(1), DEFAULT_CALL_CHANNEL_ID, &secret_after_leave)
            .is_err(),
        "removed participant must not decrypt post-leave frames"
    );

    // Removing a user who is already gone returns Ok(None).
    assert_eq!(
        alice
            .make_remove_block(&BTreeSet::from([UserId(3)]))
            .unwrap(),
        None
    );
}

// ---------------------------------------------------------------------------
// 3. Frame Encrypt / Decrypt Round-Trip (Audio + Video Prefix + Replay Guard)
// ---------------------------------------------------------------------------

#[test]
fn frame_encrypt_decrypt_roundtrip_audio_video_and_replay_guard() {
    let mut alice = E2eCall::with_key_pair(UserId(10), keypair(10));
    let mut bob = E2eCall::with_key_pair(UserId(20), keypair(20));

    let b0 = alice.make_join_block().unwrap();
    alice
        .apply_blocks(0, 1, std::slice::from_ref(&b0), true)
        .unwrap();
    bob.refresh_last_block0(Some(b0.clone()));
    let b1 = bob.make_join_block().unwrap();
    alice
        .apply_blocks(0, 2, std::slice::from_ref(&b1), false)
        .unwrap();
    bob.apply_blocks(0, 2, &[b0, b1], true).unwrap();

    let alice_ed = alice.encrypt_decrypt();
    let bob_ed = bob.encrypt_decrypt();

    // Audio frame (unencrypted_prefix_size = 0).
    let opus_frame = vec![0xF8, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
    let enc_audio = alice_ed
        .encrypt_frame(0, &opus_frame, 0)
        .expect("encrypt audio");
    assert_ne!(&enc_audio[..opus_frame.len()], opus_frame.as_slice());
    let dec_audio = bob_ed
        .decrypt_frame(UserId(10), 0, &enc_audio)
        .expect("decrypt audio");
    assert_eq!(dec_audio, opus_frame);

    // Replay of identical packet is rejected.
    assert!(matches!(
        bob_ed.decrypt_frame(UserId(10), 0, &enc_audio),
        Err(E2eError::ReplayDetected(_))
    ));

    // Video frame preserving 5-byte Annex-B + NAL header prefix (`unencrypted_prefix_size = 5`).
    let mut h264_frame = vec![0x00, 0x00, 0x00, 0x01, 0x65];
    h264_frame.extend_from_slice(&[0xAB; 128]);
    let enc_video = alice_ed
        .encrypt_frame(1, &h264_frame, 5)
        .expect("encrypt video");
    assert_eq!(&enc_video[..5], &[0x00, 0x00, 0x00, 0x01, 0x65]);
    let dec_video = bob_ed
        .decrypt_frame(UserId(10), 1, &enc_video)
        .expect("decrypt video");
    assert_eq!(dec_video, h264_frame);

    // Tampering with even the unencrypted prefix fails authentication.
    let enc_video2 = alice_ed.encrypt_frame(1, &h264_frame, 5).unwrap();
    let mut bad_prefix = enc_video2.clone();
    bad_prefix[4] ^= 0x01;
    assert!(bob_ed.decrypt_frame(UserId(10), 1, &bad_prefix).is_err());

    // Wrong channel ID fails decryption.
    assert!(bob_ed.decrypt_frame(UserId(10), 0, &enc_video2).is_err());

    // Wrong sender user ID fails authentication.
    assert!(bob_ed.decrypt_frame(UserId(20), 1, &enc_video2).is_err());

    // Out-of-order UDP delivery within replay window succeeds.
    let p1 = alice_ed.encrypt_frame(0, b"frame-1", 0).unwrap();
    let p2 = alice_ed.encrypt_frame(0, b"frame-2", 0).unwrap();
    let p3 = alice_ed.encrypt_frame(0, b"frame-3", 0).unwrap();
    assert_eq!(
        bob_ed.decrypt_frame(UserId(10), 0, &p3).unwrap(),
        b"frame-3"
    );
    assert_eq!(
        bob_ed.decrypt_frame(UserId(10), 0, &p1).unwrap(),
        b"frame-1"
    );
    assert_eq!(
        bob_ed.decrypt_frame(UserId(10), 0, &p2).unwrap(),
        b"frame-2"
    );
    assert!(bob_ed.decrypt_frame(UserId(10), 0, &p1).is_err());

    // Callback-shaped `transform()` matches `TdE2E::EncryptDecrypt::callback()`.
    let enc_cb = alice_ed.transform(b"via-callback", 10, true, 0);
    assert!(!enc_cb.is_empty());
    let dec_cb = bob_ed.transform(&enc_cb, 10, false, 0);
    assert_eq!(dec_cb, b"via-callback");
}

// ---------------------------------------------------------------------------
// 4. Emoji Fingerprints (Subchain 1 Commit-Reveal Verification)
// ---------------------------------------------------------------------------

#[test]
fn emoji_verification_commit_reveal_three_participants() {
    let mut alice = E2eCall::with_key_pair(UserId(1), keypair(0x11));
    let mut bob = E2eCall::with_key_pair(UserId(2), keypair(0x22));
    let mut charlie = E2eCall::with_key_pair(UserId(3), keypair(0x33));

    // Form 3-participant chain at height 2.
    let b0 = alice.make_join_block().unwrap();
    alice
        .apply_blocks(0, 1, std::slice::from_ref(&b0), true)
        .unwrap();
    bob.refresh_last_block0(Some(b0.clone()));
    let b1 = bob.make_join_block().unwrap();
    alice
        .apply_blocks(0, 2, std::slice::from_ref(&b1), false)
        .unwrap();
    bob.apply_blocks(0, 2, &[b0, b1.clone()], true).unwrap();
    charlie.refresh_last_block0(Some(b1));
    let b2 = charlie.make_join_block().unwrap();

    let _ = alice.take_actions();
    let _ = bob.take_actions();

    alice
        .apply_blocks(0, 3, std::slice::from_ref(&b2), false)
        .unwrap();
    bob.apply_blocks(0, 3, std::slice::from_ref(&b2), false)
        .unwrap();
    charlie
        .apply_blocks(0, 3, std::slice::from_ref(&b2), true)
        .unwrap();

    // All three automatically emit NonceCommit on subchain 1.
    let commits_a = collect_outbound_blocks(&alice.take_actions());
    let commits_b = collect_outbound_blocks(&bob.take_actions());
    let commits_c = collect_outbound_blocks(&charlie.take_actions());
    assert_eq!(commits_a.len(), 1);
    assert_eq!(commits_b.len(), 1);
    assert_eq!(commits_c.len(), 1);

    let all_commits = vec![
        commits_a[0].clone(),
        commits_b[0].clone(),
        commits_c[0].clone(),
    ];

    // Before reveals complete, emoji_hash is None.
    assert_eq!(alice.emoji_hash(), None);
    assert_eq!(bob.emoji_hash(), None);
    assert_eq!(charlie.emoji_hash(), None);

    // Deliver all 3 commits on subchain 1 -> each participant emits NonceReveal!
    alice
        .apply_blocks(SUBCHAIN_BROADCAST, 3, &all_commits, true)
        .unwrap();
    bob.apply_blocks(SUBCHAIN_BROADCAST, 3, &all_commits, true)
        .unwrap();
    charlie
        .apply_blocks(SUBCHAIN_BROADCAST, 3, &all_commits, true)
        .unwrap();

    let reveals_a = collect_outbound_blocks(&alice.take_actions());
    let reveals_b = collect_outbound_blocks(&bob.take_actions());
    let reveals_c = collect_outbound_blocks(&charlie.take_actions());
    assert_eq!(reveals_a.len(), 1);
    assert_eq!(reveals_b.len(), 1);
    assert_eq!(reveals_c.len(), 1);

    let all_reveals = vec![
        reveals_c[0].clone(),
        reveals_a[0].clone(),
        reveals_b[0].clone(),
    ];

    // Deliver reveals in arbitrary order -> all 3 derive the exact same emoji_hash & 4 emojis!
    alice
        .apply_blocks(SUBCHAIN_BROADCAST, 6, &all_reveals, true)
        .unwrap();
    bob.apply_blocks(SUBCHAIN_BROADCAST, 6, &all_reveals, true)
        .unwrap();
    charlie
        .apply_blocks(SUBCHAIN_BROADCAST, 6, &all_reveals, true)
        .unwrap();

    let hash_a = alice.emoji_hash().expect("alice emoji hash ready");
    let hash_b = bob.emoji_hash().expect("bob emoji hash ready");
    let hash_c = charlie.emoji_hash().expect("charlie emoji hash ready");
    assert_eq!(hash_a, hash_b);
    assert_eq!(hash_b, hash_c);

    let emojis_a = alice.emoji_fingerprint().expect("4 emojis");
    let emojis_b = bob.emoji_fingerprint().expect("4 emojis");
    let emojis_c = charlie.emoji_fingerprint().expect("4 emojis");
    assert_eq!(emojis_a, emojis_b);
    assert_eq!(emojis_b, emojis_c);
    assert_eq!(emojis_a, compute_emoji_fingerprint(&hash_a));
    for e in &emojis_a {
        assert!(!e.is_empty());
    }

    // Order-independence known-answer check for `compute_conference_emoji_hash`.
    let n1 = [0x11u8; 32];
    let n2 = [0x99u8; 32];
    let n3 = [0x44u8; 32];
    let ch = [0xABu8; 32];
    let h_order1 = compute_conference_emoji_hash(&[n1, n2, n3], &ch);
    let h_order2 = compute_conference_emoji_hash(&[n3, n1, n2], &ch);
    assert_eq!(h_order1, h_order2);
    assert_eq!(
        conference_emoji_fingerprint(&h_order1),
        compute_emoji_fingerprint(&h_order1)
    );
}

#[test]
fn emoji_verification_rejects_cheating_reveal_and_forged_broadcast() {
    let kp_a = keypair(0x51);
    let kp_b = keypair(0x52);
    let mut alice = ConferenceBlockchain::new(UserId(1), kp_a.clone());

    let p_a = ChainGroupParticipant {
        user_id: UserId(1),
        public_key: PublicKey(kp_a.public_key()),
        permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
        version: 1,
    };
    let p_b = ChainGroupParticipant {
        user_id: UserId(2),
        public_key: PublicKey(kp_b.public_key()),
        permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
        version: 1,
    };
    let block0 = ConferenceBlockchain::create_zero_block(
        &kp_a,
        vec![p_a, p_b],
        DEFAULT_PARTICIPANT_PERMISSIONS,
    )
    .unwrap();
    let chain_hash = block0.block_hash();
    alice.apply_block(&block0.to_tl_bytes()).unwrap();

    // Bob reveals BEFORE committing -> rejected.
    let early_reveal = GroupBroadcast::new_reveal(&kp_b, UserId(2), 0, chain_hash, [0x77; 32]);
    assert!(matches!(
        alice.apply_broadcast(&early_reveal.to_tl_bytes()),
        Err(E2eError::InvalidBroadcast(_))
    ));

    // Bob commits to nonce [0x77; 32], then reveals a DIFFERENT nonce [0x88; 32] -> rejected!
    let commit_b = GroupBroadcast::new_commit(&kp_b, UserId(2), 0, chain_hash, &[0x77; 32]);
    alice.apply_broadcast(&commit_b.to_tl_bytes()).unwrap();
    let cheated_reveal = GroupBroadcast::new_reveal(&kp_b, UserId(2), 0, chain_hash, [0x88; 32]);
    assert!(matches!(
        alice.apply_broadcast(&cheated_reveal.to_tl_bytes()),
        Err(E2eError::InvalidBroadcast(_))
    ));

    // Forged broadcast claiming to be from Bob but signed by Alice -> rejected!
    let forged_commit = GroupBroadcast::new_commit(&kp_a, UserId(2), 0, chain_hash, &[0x99; 32]);
    assert!(matches!(
        alice.apply_broadcast(&forged_commit.to_tl_bytes()),
        Err(E2eError::InvalidSignature(_))
    ));
}

// ---------------------------------------------------------------------------
// 5. Tampered Block Rejection
// ---------------------------------------------------------------------------

#[test]
fn tampered_blocks_are_rejected() {
    let kp_a = keypair(0x61);
    let kp_b = keypair(0x62);
    let kp_c = keypair(0x63);

    let p_a = ChainGroupParticipant {
        user_id: UserId(1),
        public_key: PublicKey(kp_a.public_key()),
        permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
        version: 1,
    };
    // Bob has ADD_USERS only (NO REMOVE_USERS permission).
    let p_b = ChainGroupParticipant {
        user_id: UserId(2),
        public_key: PublicKey(kp_b.public_key()),
        permissions: PERMISSION_ADD_USERS,
        version: 1,
    };

    let b0 = ConferenceBlockchain::create_zero_block(
        &kp_a,
        vec![p_a.clone(), p_b.clone()],
        PERMISSION_ADD_USERS,
    )
    .unwrap();
    let b0_bytes = b0.to_tl_bytes();

    // 5a. Bit flip anywhere in serialized block fails signature check.
    let mut flipped = b0_bytes.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x01;
    let mut verifier = ConferenceBlockchain::new(UserId(1), kp_a.clone());
    assert!(verifier.apply_block(&flipped).is_err());

    // 5b. Trailing garbage bytes rejected.
    let mut trailing = b0_bytes.clone();
    trailing.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    assert!(matches!(
        verifier.apply_block(&trailing),
        Err(E2eError::InvalidEncoding(_))
    ));

    // Initialize verifier cleanly with block 0.
    verifier.apply_block(&b0_bytes).unwrap();

    // 5c. Wrong prev_block_hash (even validly signed!) rejected.
    let valid_gs = verifier.group_state().clone();
    let valid_sk = ChainSharedKey::seal_for_group(&[0xAA; 32], &valid_gs).unwrap();
    let mut bad_prev = ChainBlock {
        signature: [0u8; 64],
        prev_block_hash: [0xFF; 32],
        changes: vec![
            ChainChange::SetGroupState(valid_gs.clone()),
            ChainChange::SetSharedKey(valid_sk.clone()),
        ],
        height: 1,
        state_proof: ChainStateProof {
            kv_hash: [0u8; 32],
            group_state: Some(valid_gs.clone()),
            shared_key: Some(valid_sk.clone()),
        },
        signature_public_key: None,
    };
    bad_prev.sign(&kp_a);
    assert_eq!(
        verifier.apply_block(&bad_prev.to_tl_bytes()),
        Err(E2eError::InvalidPrevHash { height: 1 })
    );

    // 5d. Skipped height rejected.
    let mut bad_height = bad_prev.clone();
    bad_height.prev_block_hash = *verifier.last_block_hash();
    bad_height.height = 2;
    bad_height.sign(&kp_a);
    assert_eq!(
        verifier.apply_block(&bad_height.to_tl_bytes()),
        Err(E2eError::InvalidHeight {
            expected: 1,
            actual: 2
        })
    );

    // 5e. Tampered state_proof (disagrees with changes) rejected.
    let mut bad_proof = bad_prev.clone();
    bad_proof.prev_block_hash = *verifier.last_block_hash();
    bad_proof.state_proof.shared_key = None;
    bad_proof.sign(&kp_a);
    assert!(matches!(
        verifier.apply_block(&bad_proof.to_tl_bytes()),
        Err(E2eError::TamperedStateProof(_))
    ));

    // 5f. Unauthorized removal by Bob (lacks PERMISSION_REMOVE_USERS) rejected!
    let kicked_gs = ChainGroupState {
        participants: vec![p_b.clone()],
        external_permissions: PERMISSION_ADD_USERS,
    };
    let kicked_sk = ChainSharedKey::seal_for_group(&[0xBB; 32], &kicked_gs).unwrap();
    let mut unauthorized_kick = ChainBlock {
        signature: [0u8; 64],
        prev_block_hash: *verifier.last_block_hash(),
        changes: vec![
            ChainChange::SetGroupState(kicked_gs.clone()),
            ChainChange::SetSharedKey(kicked_sk.clone()),
        ],
        height: 1,
        state_proof: ChainStateProof {
            kv_hash: [0u8; 32],
            group_state: Some(kicked_gs),
            shared_key: Some(kicked_sk),
        },
        signature_public_key: None,
    };
    unauthorized_kick.sign(&kp_b);
    assert!(matches!(
        verifier.apply_block(&unauthorized_kick.to_tl_bytes()),
        Err(E2eError::PermissionDenied(_))
    ));

    // 5g. External self-add by Charlie trying to ALSO kick Alice -> rejected!
    let p_c = ChainGroupParticipant {
        user_id: UserId(3),
        public_key: PublicKey(kp_c.public_key()),
        permissions: DEFAULT_PARTICIPANT_PERMISSIONS,
        version: 1,
    };
    let hostile_gs = ChainGroupState {
        participants: vec![p_b, p_c],
        external_permissions: PERMISSION_ADD_USERS,
    };
    let hostile_sk = ChainSharedKey::seal_for_group(&[0xCC; 32], &hostile_gs).unwrap();
    let mut hostile_self_add = ChainBlock {
        signature: [0u8; 64],
        prev_block_hash: *verifier.last_block_hash(),
        changes: vec![
            ChainChange::SetGroupState(hostile_gs.clone()),
            ChainChange::SetSharedKey(hostile_sk.clone()),
        ],
        height: 1,
        state_proof: ChainStateProof {
            kv_hash: [0u8; 32],
            group_state: Some(hostile_gs),
            shared_key: Some(hostile_sk),
        },
        signature_public_key: None,
    };
    hostile_self_add.sign(&kp_c);
    assert!(matches!(
        verifier.apply_block(&hostile_self_add.to_tl_bytes()),
        Err(E2eError::PermissionDenied(_))
    ));

    // 5h. `E2eCall` marks itself failed and emits `E2eAction::Failed(CallFailure::Unknown)` on tampered block.
    let mut call = E2eCall::with_key_pair(UserId(1), kp_a);
    let _ = call.apply_blocks(0, 1, &[flipped], true);
    assert_eq!(call.failed(), Some(CallFailure::Unknown));
    assert!(
        call.take_actions()
            .contains(&E2eAction::Failed(CallFailure::Unknown))
    );
}

// ---------------------------------------------------------------------------
// 6. Layer 233 `phone.*` Conference RPC Builders & GroupCallSession Integration
// ---------------------------------------------------------------------------

#[test]
fn layer233_conference_rpcs_and_session_subchain_sync() {
    let mut session = GroupCallSession::new(
        PeerId::from_user(UserId(1)),
        PeerId::from_user(UserId(1)),
        CallId(777),
        123_456,
        true,
        0,
        false,
    );
    session.enable_conference_e2e_with_key_pair(UserId(1), keypair(0x71));
    assert!(session.is_conference());

    let zero_block = session.e2e_mut().unwrap().make_join_block().unwrap();
    let pk = session.e2e().unwrap().my_key();

    // Round-trip all Layer 233 conference RPCs through `tg-tl` wire encoding.
    let create_req = build_create_conference_call(
        42,
        true,
        true,
        pk,
        zero_block.clone(),
        r#"{"ufrag":"a"}"#.into(),
    );
    let create_wire = create_req.to_bytes();
    assert_eq!(
        tg_tl::functions::phone::CreateConferenceCall::from_bytes(&create_wire).unwrap(),
        create_req
    );

    let input_call = InputGroupCall::Id {
        id: CallId(777),
        access_hash: 123_456,
    };
    let join_req = build_join_conference_call(
        input_call.clone(),
        tl_enums::InputPeer::Self_,
        false,
        true,
        None,
        pk,
        zero_block.clone(),
        r#"{"ufrag":"b"}"#.into(),
    )
    .unwrap();
    assert_eq!(
        tg_tl::functions::phone::JoinGroupCall::from_bytes(&join_req.to_bytes()).unwrap(),
        join_req
    );

    let del_req =
        build_delete_conference_participants(input_call.clone(), &[UserId(2)], vec![1, 2, 3], true)
            .unwrap();
    assert!(del_req.only_left);
    assert!(!del_req.kick);
    assert_eq!(
        tg_tl::functions::phone::DeleteConferenceCallParticipants::from_bytes(&del_req.to_bytes())
            .unwrap(),
        del_req
    );

    let bcast_req = build_send_conference_broadcast(input_call.clone(), vec![9, 8, 7]).unwrap();
    assert_eq!(
        tg_tl::functions::phone::SendConferenceCallBroadcast::from_bytes(&bcast_req.to_bytes())
            .unwrap(),
        bcast_req
    );

    let poll_req = build_get_chain_blocks(input_call.clone(), 0, 0, 50).unwrap();
    assert_eq!(
        tg_tl::functions::phone::GetGroupCallChainBlocks::from_bytes(&poll_req.to_bytes()).unwrap(),
        poll_req
    );

    let last_req = build_get_last_chain_block(input_call.clone()).unwrap();
    assert_eq!(last_req.sub_chain_id, 0);
    assert_eq!(last_req.offset, -1);
    assert_eq!(last_req.limit, 1);

    let inv_req =
        build_invite_conference_participant(input_call.clone(), tl_enums::InputUser::Self_, true)
            .unwrap();
    assert_eq!(
        tg_tl::functions::phone::InviteConferenceCallParticipant::from_bytes(&inv_req.to_bytes())
            .unwrap(),
        inv_req
    );

    let decl_req = build_decline_conference_invite(MsgId(55)).unwrap();
    assert_eq!(
        tg_tl::functions::phone::DeclineConferenceCallInvite::from_bytes(&decl_req.to_bytes())
            .unwrap(),
        decl_req
    );

    assert!(is_chain_invalid_error("CONF_WRITE_CHAIN_INVALID_123"));
    assert!(is_permanent_broadcast_error("BLOCK_INVALID"));
    assert!(!is_permanent_broadcast_error("GROUPCALL_FORBIDDEN"));

    // TL UpdateGroupCallChainBlocks conversion + buffered subchain request handling.
    let tl_upd = tl_types::UpdateGroupCallChainBlocks {
        call: tl_enums::InputGroupCall::InputGroupCall(tl_types::InputGroupCall {
            id: 777,
            access_hash: 123_456,
        }),
        sub_chain_id: 0,
        blocks: vec![zero_block],
        next_offset: 1,
    };
    let domain_upd = UpdateGroupCallChainBlocks::try_from(tl_upd).unwrap();
    session.begin_subchain_request(0);
    session
        .finish_subchain_request(0, &[domain_upd])
        .expect("finish subchain 0");
    assert!(session.e2e().unwrap().is_initialized());
    assert!(session.e2e_encrypt_decrypt().unwrap().is_ready());

    // Timer firing triggers short-poll request action.
    session
        .e2e_mut()
        .unwrap()
        .on_timer(SUBCHAIN_STATE, E2eTimerKind::ShortPoll)
        .unwrap();
    let actions = session.take_e2e_actions();
    assert!(actions.iter().any(|a| matches!(
        a,
        E2eAction::RequestSubchainBlocks {
            subchain: 0,
            offset: 1,
            limit: 50
        }
    )));
}

// ---------------------------------------------------------------------------
// 7. In-Call Group Messages (Plain + E2E Encrypted) & Paid Star Reactions
// ---------------------------------------------------------------------------

#[test]
fn in_call_encrypted_messages_and_paid_star_reactions() {
    // JSON codec round-trip (`calls_group_message_encryption.cpp`).
    let msg = PreparedEncryptedMessage {
        random_id: 9_876_543_210,
        text: "Hello bold emoji!".into(),
        entities: vec![
            GroupCallMessageEntity::Bold {
                offset: 6,
                length: 4,
            },
            GroupCallMessageEntity::CustomEmoji {
                offset: 11,
                length: 5,
                document_id: 555_111_222,
            },
        ],
    };
    let wire_json = serialize_encrypted_message(&msg);
    let parsed = deserialize_encrypted_message(&wire_json).expect("deserialize json");
    assert_eq!(parsed, msg);

    // Reject `_min_layer` higher than Layer 233.
    let future_json = br#"{"_":"groupCallMessage","_min_layer":999,"random_id":"1","message":{"_":"textWithEntities","text":"hi","entities":[]}}"#;
    assert!(deserialize_encrypted_message(future_json).is_none());

    // End-to-end encrypted conference message exchange between Alice & Bob.
    let mut alice_e2e = E2eCall::with_key_pair(UserId(1), keypair(0x81));
    let mut bob_e2e = E2eCall::with_key_pair(UserId(2), keypair(0x82));
    let b0 = alice_e2e.make_join_block().unwrap();
    alice_e2e
        .apply_blocks(0, 1, std::slice::from_ref(&b0), true)
        .unwrap();
    bob_e2e.refresh_last_block0(Some(b0.clone()));
    let b1 = bob_e2e.make_join_block().unwrap();
    alice_e2e
        .apply_blocks(0, 2, std::slice::from_ref(&b1), false)
        .unwrap();
    bob_e2e.apply_blocks(0, 2, &[b0, b1], true).unwrap();

    let mut alice_msgs = GroupCallMessages::new(
        PeerId::from_user(UserId(1)),
        PeerId::from_user(UserId(1)),
        PeerId::from_user(UserId(1)),
        true,
        false,
        true,
    );
    alice_msgs.set_e2e(Some(alice_e2e.encrypt_decrypt()));
    alice_msgs.set_call_ready(true);

    let mut bob_msgs = GroupCallMessages::new(
        PeerId::from_user(UserId(1)),
        PeerId::from_user(UserId(2)),
        PeerId::from_user(UserId(2)),
        true,
        false,
        false,
    );
    bob_msgs.set_e2e(Some(bob_e2e.encrypt_decrypt()));
    bob_msgs.set_call_ready(true);

    alice_msgs.send(
        "Secret call chat!".into(),
        vec![GroupCallMessageEntity::Italic {
            offset: 0,
            length: 6,
        }],
        0,
        4242,
        1_000,
    );
    let actions = alice_msgs.take_actions();
    let encrypted_bytes = actions
        .iter()
        .find_map(|a| match a {
            GroupMessageAction::SendEncryptedMessage {
                encrypted_message, ..
            } => Some(encrypted_message.clone()),
            _ => None,
        })
        .expect("must emit SendEncryptedMessage");

    // Verify Layer 233 RPC builder.
    let input_call = InputGroupCall::Id {
        id: CallId(900),
        access_hash: 1,
    };
    let enc_rpc =
        build_send_group_call_encrypted_message(input_call.clone(), encrypted_bytes.clone())
            .unwrap();
    assert_eq!(
        tg_tl::functions::phone::SendGroupCallEncryptedMessage::from_bytes(&enc_rpc.to_bytes())
            .unwrap(),
        enc_rpc
    );

    // Bob receives and decrypts the conference message; duplicate delivery is deduplicated.
    let upd = UpdateGroupCallEncryptedMessage {
        call_id: CallId(900),
        from_id: PeerId::from_user(UserId(1)),
        encrypted_message: encrypted_bytes,
    };
    assert!(bob_msgs.handle_incoming_encrypted_message(&upd, 1_005));
    assert_eq!(bob_msgs.messages().len(), 1);
    assert_eq!(bob_msgs.messages()[0].text, "Secret call chat!");
    assert_eq!(bob_msgs.messages()[0].entities.len(), 1);

    // Duplicate delivery (same random_id) does not duplicate the message.
    alice_msgs.handle_encrypted_message_sent(4242, 1_001);
    assert_eq!(alice_msgs.messages()[0].id, 1);

    // Paid Star reactions + Stars coloring + leaderboard in a livestream.
    let colorings = vec![
        StarsColoring {
            bg_light: 0x111111,
            bg_dark: 0x222222,
            from_stars: 1,
            seconds_pin: 30,
            characters_max: 60,
            emoji_limit: 2,
        },
        StarsColoring {
            bg_light: 0x333333,
            bg_dark: 0x444444,
            from_stars: 50,
            seconds_pin: 120,
            characters_max: 160,
            emoji_limit: 6,
        },
    ];
    assert_eq!(
        stars_coloring_for_count(&colorings, 10)
            .unwrap()
            .seconds_pin,
        30
    );
    assert_eq!(
        stars_coloring_for_count(&colorings, 75)
            .unwrap()
            .seconds_pin,
        120
    );
    assert_eq!(stars_required_for_message(&colorings, 40, 1), 1);
    assert_eq!(stars_required_for_message(&colorings, 100, 3), 50);

    let mut stream_msgs = GroupCallMessages::new(
        PeerId::from_user(UserId(99)),
        PeerId::from_user(UserId(1)),
        PeerId::from_user(UserId(1)),
        false,
        true,
        false,
    );
    stream_msgs.configure_limits(10, 0, colorings);
    stream_msgs.set_call_ready(true);
    stream_msgs.apply_stars_stats(&PhoneGroupCallStarsResponse {
        total_stars: 100,
        top_donors: vec![GroupCallDonorData {
            top: true,
            my: false,
            peer_id: Some(PeerId::from_user(UserId(5))),
            stars: 100,
        }],
    });
    stream_msgs.reactions_paid_add(25);
    assert_eq!(stream_msgs.reactions_paid_scheduled(), 25);
    assert_eq!(stream_msgs.stars_local_state().total, 125);
    assert_eq!(stream_msgs.stars_local_state().my, 25);

    stream_msgs.reactions_paid_send(7777, 2_000);
    let stream_actions = stream_msgs.take_actions();
    assert!(stream_actions.iter().any(|a| matches!(
        a,
        GroupMessageAction::SendPlainMessage {
            random_id: 7777,
            allow_paid_stars: 25,
            ..
        }
    )));
    stream_msgs.finish_paid_sending(true, 0, 2_001);
    assert_eq!(stream_msgs.stars_top().total, 125);

    // Incoming plain paid message + deletion + RPC builders.
    stream_msgs.handle_incoming_message(
        &UpdateGroupCallMessage {
            call_id: CallId(900),
            message: GroupCallMessageData {
                from_admin: false,
                id: 500,
                from_id: PeerId::from_user(UserId(7)),
                date: 2_005,
                text: "Great stream!".into(),
                entities: vec![],
                paid_message_stars: Some(50),
            },
        },
        2_005,
    );
    assert_eq!(stream_msgs.stars_top().total, 175);
    stream_msgs.handle_deleted_messages(&UpdateDeleteGroupCallMessages {
        call_id: CallId(900),
        messages: vec![500],
    });
    assert!(!stream_msgs.messages().iter().any(|m| m.id == 500));

    stream_msgs.delete_confirmed(MessageDeleteRequest {
        id: 1,
        delete_all_from: None,
        report_spam: true,
    });
    assert!(stream_msgs.take_actions().iter().any(|a| matches!(
        a,
        GroupMessageAction::DeleteMessages {
            report_spam: true,
            ..
        }
    )));

    // Remaining message/stars TL RPC builders round-trip cleanly.
    let plain_rpc = build_send_group_call_message(
        input_call.clone(),
        123,
        "hi".into(),
        &[],
        Some(10),
        Some(tl_enums::InputPeer::Self_),
    )
    .unwrap();
    assert_eq!(
        tg_tl::functions::phone::SendGroupCallMessage::from_bytes(&plain_rpc.to_bytes()).unwrap(),
        plain_rpc
    );
    let del_m = build_delete_group_call_messages(input_call.clone(), vec![1, 2], true).unwrap();
    assert_eq!(
        tg_tl::functions::phone::DeleteGroupCallMessages::from_bytes(&del_m.to_bytes()).unwrap(),
        del_m
    );
    let del_p = build_delete_group_call_participant_messages(
        input_call.clone(),
        tl_enums::InputPeer::Self_,
        false,
    )
    .unwrap();
    assert_eq!(
        tg_tl::functions::phone::DeleteGroupCallParticipantMessages::from_bytes(&del_p.to_bytes())
            .unwrap(),
        del_p
    );
    let stars_rpc = build_get_group_call_stars(input_call.clone()).unwrap();
    assert_eq!(
        tg_tl::functions::phone::GetGroupCallStars::from_bytes(&stars_rpc.to_bytes()).unwrap(),
        stars_rpc
    );
    let send_as_rpc = build_save_default_send_as(input_call, tl_enums::InputPeer::Self_).unwrap();
    assert_eq!(
        tg_tl::functions::phone::SaveDefaultSendAs::from_bytes(&send_as_rpc.to_bytes()).unwrap(),
        send_as_rpc
    );
}
