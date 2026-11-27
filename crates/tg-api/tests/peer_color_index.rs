//! TGB-2880: the userpic colour index follows `PeerData::changeColor` — the server `color`
//! first, then the id-based fallback (`Data::DecideColorIndex`), also for collectible colours.

mod common;

use common::sample_tl_user;
use tg_api::convert::apply_tl_user;
use tg_data::{ColorCollectibleAccent, PeerId, Session, UserId, decide_color_index};
use tg_tl::{enums as tl_enums, types as tl_types};

const USER: i64 = 5_000_493_909;

fn color_after(color: Option<tl_enums::PeerColor>) -> (u8, bool) {
    let mut session = Session::new(UserId(1));
    let mut u = sample_tl_user(USER, 0x55, "Colour", false);
    u.color = color;
    let pid = apply_tl_user(&mut session, &tl_enums::User::User(u)).expect("user applied");
    let common = session.peer(pid).expect("peer").common();
    (common.color_index, common.color_index_cloud)
}

#[test]
fn colour_index_prefers_server_colour_then_id_fallback() {
    let fallback = decide_color_index(PeerId::from_user(UserId(USER as u64)));
    assert_eq!(fallback, 5, "5000493909 % 7");

    assert_eq!(color_after(None), (fallback, false));

    let server = tl_enums::PeerColor::PeerColor(tl_types::PeerColor {
        color: Some(2),
        background_emoji_id: None,
    });
    assert_eq!(color_after(Some(server)), (2, true));

    let no_index = tl_enums::PeerColor::PeerColor(tl_types::PeerColor {
        color: None,
        background_emoji_id: Some(7),
    });
    assert_eq!(color_after(Some(no_index)), (fallback, false));

    let collectible = tl_enums::PeerColor::Collectible(tl_types::PeerColorCollectible {
        collectible_id: 1,
        gift_emoji_id: 2,
        background_emoji_id: 3,
        accent_color: 0x00ff_8800,
        colors: vec![0x00ff_8800],
        dark_accent_color: None,
        dark_colors: None,
    });
    assert_eq!(
        color_after(Some(collectible)),
        (fallback, false),
        "a collectible colour has no index (`ColorIndexFromColor`)"
    );
}

/// TGB-3012: the name accent of a collectible colour is kept (`Data::ParseColorCollectible`)
/// and a plain colour drops it again (`PeerData::clearColorCollectible`).
#[test]
fn collectible_colour_keeps_its_name_accent() {
    let mut session = Session::new(UserId(1));
    let apply = |session: &mut Session, color: tl_enums::PeerColor| {
        let mut u = sample_tl_user(USER, 0x55, "Colour", false);
        u.color = Some(color);
        apply_tl_user(session, &tl_enums::User::User(u)).expect("user applied")
    };
    let collectible = |dark: Option<i32>| {
        tl_enums::PeerColor::Collectible(tl_types::PeerColorCollectible {
            collectible_id: 1,
            gift_emoji_id: 2,
            background_emoji_id: 3,
            accent_color: 0x00ff_8800,
            colors: vec![0x00ff_8800],
            dark_accent_color: dark,
            dark_colors: None,
        })
    };

    let pid = apply(&mut session, collectible(Some(0x0011_2233)));
    let accent = session.peer(pid).expect("peer").common().color_collectible;
    assert_eq!(
        accent,
        Some(ColorCollectibleAccent {
            accent: 0x00ff_8800,
            dark_accent: Some(0x0011_2233),
        })
    );
    assert_eq!(accent.map(|a| a.for_theme(true)), Some(0x0011_2233));

    // `MaybeColorFromSerialized(-1)` is no dark accent: night uses the day one.
    let pid = apply(&mut session, collectible(Some(-1)));
    let accent = session.peer(pid).expect("peer").common().color_collectible;
    assert_eq!(accent.map(|a| a.for_theme(true)), Some(0x00ff_8800));

    let pid = apply(
        &mut session,
        tl_enums::PeerColor::PeerColor(tl_types::PeerColor {
            color: Some(4),
            background_emoji_id: None,
        }),
    );
    let common = session.peer(pid).expect("peer").common();
    assert_eq!(common.color_collectible, None);
    assert_eq!(common.color_index, 4);
}
