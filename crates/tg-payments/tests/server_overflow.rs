//! Server values at the ends of the integer range saturate instead of overflowing
//! (TGB-3091, TGB-3092, TGB-3093). Normal values keep tdesktop's results.

use tg_data::{PeerId, UserId};
use tg_payments::credits::{CreditsAmount, CreditsType};
use tg_payments::premium::boosts_list_from_tl;
use tg_payments::star_gifts::star_gift_from_tl;
use tg_tl::{enums as e, types as t};

fn stars(amount: i64, nanos: i32) -> CreditsAmount {
    CreditsAmount::from_tl(&e::StarsAmount::StarsAmount(t::StarsAmount {
        amount,
        nanos,
    }))
}

#[test]
fn stars_amount_saturates_at_i64_ends() {
    let low = stars(i64::MIN, -1);
    assert_eq!((low.whole(), low.nano()), (i64::MIN, 999_999_999));
    let high = stars(i64::MAX, 1_000_000_000);
    assert_eq!((high.whole(), high.nano()), (i64::MAX, 0));
    let high = stars(i64::MAX, i32::MAX);
    assert_eq!(high.whole(), i64::MAX);
    assert!((0..1_000_000_000).contains(&high.nano()));
}

#[test]
fn stars_amount_normalizes_like_tdesktop() {
    let a = stars(5, -1);
    assert_eq!((a.whole(), a.nano()), (4, 999_999_999));
    let a = stars(-5, -1_500_000_000);
    assert_eq!((a.whole(), a.nano()), (-7, 500_000_000));
    let a = stars(2, i32::MIN);
    assert_eq!(
        a.whole() * 1_000_000_000 + a.nano(),
        2_000_000_000 + i64::from(i32::MIN)
    );
    let a = stars(1, 1_999_999_999);
    assert_eq!((a.whole(), a.nano()), (2, 999_999_999));
    let a = CreditsAmount::new(0, -1_000_000_000, CreditsType::Stars);
    assert_eq!((a.whole(), a.nano()), (-1, 0));
}

#[test]
fn credits_arithmetic_saturates() {
    let max = CreditsAmount::stars(i64::MAX);
    assert_eq!((max + CreditsAmount::stars(1)).whole(), i64::MAX);
    let min = CreditsAmount::stars(i64::MIN);
    assert_eq!((min - CreditsAmount::stars(1)).whole(), i64::MIN);
    assert_eq!((max * 2).whole(), i64::MAX);
    assert_eq!(min.abs().whole(), i64::MAX);
    assert_eq!(
        (-CreditsAmount::new(3, 250_000_000, CreditsType::Stars)).whole(),
        -4
    );
    let sum = CreditsAmount::new(1, 600_000_000, CreditsType::Stars)
        + CreditsAmount::new(2, 700_000_000, CreditsType::Stars);
    assert_eq!((sum.whole(), sum.nano()), (4, 300_000_000));
}

fn boost(date: i32, expires: i32) -> e::premium::BoostsList {
    e::premium::BoostsList::BoostsList(t::premium::BoostsList {
        count: 1,
        boosts: vec![e::Boost::Boost(t::Boost {
            gift: false,
            giveaway: false,
            unclaimed: false,
            id: String::new(),
            user_id: None,
            giveaway_msg_id: None,
            date,
            expires,
            used_gift_slug: None,
            multiplier: None,
            stars: None,
        })],
        next_offset: None,
        users: Vec::new(),
    })
}

fn boost_months(date: i32, expires: i32) -> i32 {
    let peer = PeerId::from_user(UserId(1));
    let slice = boosts_list_from_tl(&boost(date, expires), peer, false);
    slice.list[0].expires_after_months
}

#[test]
fn boost_months_do_not_overflow() {
    const MONTH: i64 = 30 * 86_400;
    assert_eq!(
        boost_months(1, i32::MIN),
        ((i32::MIN as i64 - 1) / MONTH) as i32
    );
    assert_eq!(
        boost_months(i32::MIN, i32::MAX),
        (u32::MAX as i64 / MONTH) as i32
    );
    let date = 1_700_000_000;
    assert_eq!(boost_months(date, date + 3 * 30 * 86_400), 3);
    assert_eq!(boost_months(date, date + 30 * 86_400 - 1), 0);
}

fn unique_gift(issued: i32, total: i32) -> e::StarGift {
    e::StarGift::Unique(t::StarGiftUnique {
        require_premium: false,
        resale_ton_only: false,
        theme_available: false,
        burned: false,
        crafted: false,
        id: 1,
        gift_id: 1,
        title: String::new(),
        slug: String::new(),
        num: 1,
        owner_id: None,
        owner_name: None,
        owner_address: None,
        attributes: Vec::new(),
        availability_issued: issued,
        availability_total: total,
        gift_address: None,
        resell_amount: None,
        released_by: None,
        value_amount: None,
        value_currency: None,
        value_usd_amount: None,
        theme_peer: None,
        peer_color: None,
        host_id: None,
        offer_min_stars: None,
        craft_chance_permille: None,
    })
}

#[test]
fn unique_gift_left_saturates() {
    assert_eq!(
        star_gift_from_tl(&unique_gift(i32::MIN, 0)).limited_left,
        i32::MAX
    );
    assert_eq!(
        star_gift_from_tl(&unique_gift(1, i32::MIN)).limited_left,
        i32::MIN
    );
    assert_eq!(star_gift_from_tl(&unique_gift(40, 100)).limited_left, 60);
}
