//! Unit tests for WP-55 Payments, Stars & Star Gifts TL request builders, parsers,
//! filter flags, and hash helpers.

use tg_data::{PeerId, UserId};
use tg_payments::credits::{build_get_stars_gift_options, stars_gift_options_from_tl};
use tg_payments::star_gifts::{
    CanSendGiftResult, GIFT_WITHDRAW_TMP_PASSWORD_PERIOD_SECS, GiftCollection, PeerGiftsFilter,
    build_check_can_send_gift, build_delete_star_gift, build_get_saved_star_gift,
    build_get_saved_star_gifts_filtered, build_get_star_gift_withdrawal_url,
    build_toggle_chat_star_gift_notifications, build_withdraw_star_gift,
    check_can_send_gift_result_from_tl, compute_gift_collections_hash,
    is_tmp_password_refused_error, saved_star_gifts_from_tl, star_gift_withdrawal_url_from_tl,
};
use tg_tl::{Deserializable, Identifiable, Serializable, enums as tl_enums, types as tl_types};

#[test]
fn check_can_send_gift_builder_and_parsers() {
    let req = build_check_can_send_gift(987_654_321);
    let raw = req.to_bytes();
    assert_eq!(
        u32::from_le_bytes(raw[..4].try_into().expect("ctor")),
        tg_tl::functions::payments::CheckCanSendGift::CONSTRUCTOR_ID
    );
    let decoded = tg_tl::functions::payments::CheckCanSendGift::from_bytes(&raw).expect("decode");
    assert_eq!(decoded.gift_id, 987_654_321);

    assert_eq!(
        check_can_send_gift_result_from_tl(&tl_enums::payments::CheckCanSendGiftResult::Ok),
        CanSendGiftResult::Ok
    );
    let fail_tl = tl_enums::payments::CheckCanSendGiftResult::Fail(
        tl_types::payments::CheckCanSendGiftResultFail {
            reason: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: "Premium required to send this limited gift.".to_owned(),
                entities: Vec::new(),
            }),
        },
    );
    assert_eq!(
        check_can_send_gift_result_from_tl(&fail_tl),
        CanSendGiftResult::Fail {
            reason: "Premium required to send this limited gift.".to_owned()
        }
    );
}

#[test]
fn get_stars_gift_options_builder_and_parser() {
    let input_user = tl_enums::InputUser::InputUser(tl_types::InputUser {
        user_id: 42_000,
        access_hash: 99_111,
    });
    let req = build_get_stars_gift_options(Some(input_user.clone()));
    let bytes = req.to_bytes();
    let decoded =
        tg_tl::functions::payments::GetStarsGiftOptions::from_bytes(&bytes).expect("decode");
    assert_eq!(decoded.user_id, Some(input_user));

    let req_none = build_get_stars_gift_options(None);
    let decoded_none =
        tg_tl::functions::payments::GetStarsGiftOptions::from_bytes(&req_none.to_bytes())
            .expect("decode none");
    assert!(decoded_none.user_id.is_none());

    let parsed = stars_gift_options_from_tl(
        &[tl_enums::StarsGiftOption::StarsGiftOption(
            tl_types::StarsGiftOption {
                extended: true,
                stars: 250,
                store_product: Some("org.telegram.stars.gift.250".to_owned()),
                currency: "USD".to_owned(),
                amount: 499,
            },
        )],
        PeerId::from_user(UserId::from_tl(42_000)),
    );
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].credits, 250);
    assert_eq!(parsed[0].amount, 499);
    assert!(parsed[0].extended);
    assert_eq!(
        parsed[0].gift_bare_peer_id,
        PeerId::from_user(UserId::from_tl(42_000)).raw()
    );
}

#[test]
fn withdraw_and_withdrawal_url_and_delete_and_get_saved_and_toggle_notifications_builders() {
    assert_eq!(GIFT_WITHDRAW_TMP_PASSWORD_PERIOD_SECS, 600);
    assert!(is_tmp_password_refused_error("TMP_PASSWORD_INVALID"));
    assert!(is_tmp_password_refused_error(
        "RPC error 400: TMP_PASSWORD_INVALID"
    ));
    assert!(is_tmp_password_refused_error("PASSWORD_HASH_INVALID"));
    assert!(is_tmp_password_refused_error("SRP_PASSWORD_CHANGED"));
    assert!(!is_tmp_password_refused_error("FLOOD_WAIT_30"));

    let sg_user =
        tl_enums::InputSavedStarGift::User(tl_types::InputSavedStarGiftUser { msg_id: 88 });
    let withdraw_req = build_withdraw_star_gift(sg_user.clone(), vec![1, 2, 3, 4], true);
    let withdraw_dec =
        tg_tl::functions::payments::WithdrawStarGift::from_bytes(&withdraw_req.to_bytes())
            .expect("decode withdraw");
    assert!(withdraw_dec.keep_on_profile);
    assert_eq!(withdraw_dec.stargift, sg_user);
    assert_eq!(withdraw_dec.tmp_password, vec![1, 2, 3, 4]);

    let url_req = build_get_star_gift_withdrawal_url(sg_user.clone(), vec![9, 8, 7]);
    let url_dec =
        tg_tl::functions::payments::GetStarGiftWithdrawalUrl::from_bytes(&url_req.to_bytes())
            .expect("decode withdrawal url");
    assert_eq!(url_dec.stargift, sg_user);
    assert_eq!(url_dec.tmp_password, vec![9, 8, 7]);
    let url = star_gift_withdrawal_url_from_tl(
        &tl_enums::payments::StarGiftWithdrawalUrl::StarGiftWithdrawalUrl(
            tl_types::payments::StarGiftWithdrawalUrl {
                url: "https://fragment.com/gift/PlushPepe-1?token=abc".to_owned(),
            },
        ),
    );
    assert_eq!(url, "https://fragment.com/gift/PlushPepe-1?token=abc");

    let del_req = build_delete_star_gift(sg_user.clone());
    let del_dec = tg_tl::functions::payments::DeleteStarGift::from_bytes(&del_req.to_bytes())
        .expect("decode delete");
    assert_eq!(del_dec.stargift, sg_user);

    let get_one_req = build_get_saved_star_gift(vec![sg_user.clone()]);
    let get_one_dec =
        tg_tl::functions::payments::GetSavedStarGift::from_bytes(&get_one_req.to_bytes())
            .expect("decode getSavedStarGift");
    assert_eq!(get_one_dec.stargift, vec![sg_user]);

    let notify_on = build_toggle_chat_star_gift_notifications(tl_enums::InputPeer::Self_, true);
    let notify_on_dec = tg_tl::functions::payments::ToggleChatStarGiftNotifications::from_bytes(
        &notify_on.to_bytes(),
    )
    .expect("decode notify on");
    assert!(notify_on_dec.enabled);

    let notify_off = build_toggle_chat_star_gift_notifications(tl_enums::InputPeer::Self_, false);
    let notify_off_dec = tg_tl::functions::payments::ToggleChatStarGiftNotifications::from_bytes(
        &notify_off.to_bytes(),
    )
    .expect("decode notify off");
    assert!(!notify_off_dec.enabled);
}

#[test]
fn peer_gifts_filter_skip_limited_does_not_set_exclude_upgradable() {
    // Regression test for `info/peer_gifts/info_peer_gifts_widget.cpp:688-690`:
    // `skip_limited` maps ONLY to `exclude_unupgradable`.
    let filter = PeerGiftsFilter {
        skip_limited: true,
        skip_upgradable: false,
        ..PeerGiftsFilter::default()
    };
    let req = build_get_saved_star_gifts_filtered(tl_enums::InputPeer::Self_, &filter, "", 50);
    assert!(req.exclude_unupgradable);
    assert!(!req.exclude_upgradable);
}

#[test]
fn saved_star_gifts_slice_preserves_chat_notifications_enabled_and_collections_hash() {
    let tl_slice =
        tl_enums::payments::SavedStarGifts::SavedStarGifts(tl_types::payments::SavedStarGifts {
            count: 0,
            chat_notifications_enabled: Some(true),
            gifts: Vec::new(),
            next_offset: None,
            chats: Vec::new(),
            users: Vec::new(),
        });
    let parsed = saved_star_gifts_from_tl(&tl_slice, PeerId::ZERO, false);
    assert_eq!(parsed.chat_notifications_enabled, Some(true));

    // Known-answer test for `Api::CountHash` (`api/api_hash.h:36-70`).
    let cols = vec![
        GiftCollection {
            id: 1,
            count: 2,
            title: "Alpha".to_owned(),
            icon_document_id: None,
            hash: 111,
        },
        GiftCollection {
            id: 2,
            count: 3,
            title: "Beta".to_owned(),
            icon_document_id: None,
            hash: 222,
        },
    ];
    let mut expected: u64 = 0;
    for v in [111_u64, 222_u64] {
        expected ^= expected >> 21;
        expected ^= expected << 35;
        expected ^= expected >> 4;
        expected = expected.wrapping_add(v);
    }
    assert_eq!(compute_gift_collections_hash(&cols), expected as i64);
    assert_eq!(compute_gift_collections_hash(&[]), 0);
}
