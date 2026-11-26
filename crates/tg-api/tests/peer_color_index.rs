//! TGB-2880: the userpic colour index follows `PeerData::changeColor` — the server `color`
//! first, then the id-based fallback (`Data::DecideColorIndex`), also for collectible colours.

mod common;

use common::sample_tl_user;
use tg_api::convert::apply_tl_user;
use tg_data::{PeerId, Session, UserId, decide_color_index};
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
