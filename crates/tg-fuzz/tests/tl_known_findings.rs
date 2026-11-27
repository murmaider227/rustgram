//! Minimal hand-built inputs for the findings of the structured TL fuzzer (TGB-3086).
//!
//! Each test is the smallest server object found to break a converter, built by hand from the
//! fuzzer's report. A test is ignored while the owning package's issue is open (it panics); the
//! owner removes the `#[ignore]` with the fix, and the matching entry of `KNOWN` in
//! `tl_converters.rs`. TGB-3087 and TGB-3089..3093 are fixed (saturating math).

use tg_tl::{enums as e, types as t};

const NOW: i32 = 1_700_000_000;

fn notify() -> e::PeerNotifySettings {
    e::PeerNotifySettings::PeerNotifySettings(t::PeerNotifySettings {
        show_previews: None,
        silent: None,
        mute_until: None,
        ios_sound: None,
        android_sound: None,
        other_sound: None,
        stories_muted: None,
        stories_hide_sender: None,
        stories_ios_sound: None,
        stories_android_sound: None,
        stories_other_sound: None,
    })
}

fn dialog(user_id: i64, unread_count: i32) -> e::Dialog {
    e::Dialog::Dialog(t::Dialog {
        pinned: false,
        unread_mark: false,
        view_forum_as_messages: false,
        peer: e::Peer::User(t::PeerUser { user_id }),
        top_message: 1,
        read_inbox_max_id: 0,
        read_outbox_max_id: 0,
        unread_count,
        unread_mentions_count: 0,
        unread_reactions_count: 0,
        unread_poll_votes_count: 0,
        notify_settings: notify(),
        pts: None,
        draft: None,
        folder_id: None,
        ttl_period: None,
    })
}

fn archive_folder(muted: i32, unmuted: i32, muted_peers: i32, unmuted_peers: i32) -> e::Dialog {
    e::Dialog::Folder(t::DialogFolder {
        pinned: false,
        folder: e::Folder::Folder(t::Folder {
            autofill_new_broadcasts: false,
            autofill_public_groups: false,
            autofill_new_correspondents: false,
            id: 1,
            title: String::new(),
            photo: None,
        }),
        peer: e::Peer::User(t::PeerUser { user_id: 1 }),
        top_message: 1,
        unread_muted_peers_count: muted_peers,
        unread_unmuted_peers_count: unmuted_peers,
        unread_muted_messages_count: muted,
        unread_unmuted_messages_count: unmuted,
    })
}

fn apply(dialogs: Vec<e::Dialog>) {
    let resp = e::messages::Dialogs::Dialogs(t::messages::Dialogs {
        dialogs,
        messages: Vec::new(),
        chats: Vec::new(),
        users: Vec::new(),
    });
    let mut session = tg_data::Session::new(tg_data::UserId(1));
    tg_api::convert::apply_tl_dialogs_response(&mut session, &resp, NOW);
}

/// TGB-3087: `dialogs.rs:913`, archive unread messages `i32::MAX + 1`.
#[test]
fn archive_folder_unread_messages_sum() {
    apply(vec![archive_folder(1, i32::MAX, 0, 0)]);
}

/// TGB-3087: `dialogs.rs:914`, archive unread peers `i32::MAX + 1`.
#[test]
fn archive_folder_unread_peers_sum() {
    apply(vec![archive_folder(0, 0, 1, i32::MAX)]);
}

/// TGB-3089: `dialogs.rs:202`, two dialogs whose unread counts sum past `i32::MAX`.
#[test]
fn dialogs_unread_sum() {
    apply(vec![dialog(2, i32::MAX), dialog(3, 1)]);
}

/// TGB-3090: `tg-bots/src/tl_convert.rs:1197`, `usersSlice` count `i32::MIN` with one user.
#[test]
fn bot_recommendations_slice_count() {
    let users = e::users::Users::Slice(t::users::UsersSlice {
        count: i32::MIN,
        users: vec![e::User::Empty(t::UserEmpty { id: 1 })],
    });
    let _ = tg_bots::tl_convert::bot_recommendations_from_tl(&users);
}

/// TGB-3091: `tg-payments/src/credits.rs:195`, `starsAmount` with `amount = i64::MIN` and a
/// negative `nanos`.
#[test]
fn stars_amount_min_with_negative_nanos() {
    let amount = e::StarsAmount::StarsAmount(t::StarsAmount {
        amount: i64::MIN,
        nanos: -1,
    });
    let _ = tg_payments::credits::CreditsAmount::from_tl(&amount);
}

/// TGB-3091: `tg-payments/src/credits.rs:199`, `starsAmount` with `amount = i64::MAX` and
/// `nanos` of a whole star or more.
#[test]
fn stars_amount_max_with_whole_star_nanos() {
    let amount = e::StarsAmount::StarsAmount(t::StarsAmount {
        amount: i64::MAX,
        nanos: 1_000_000_000,
    });
    let _ = tg_payments::credits::CreditsAmount::from_tl(&amount);
}

/// TGB-3092: `tg-payments/src/premium.rs:772`, a boost whose `expires - date` overflows.
#[test]
fn boost_expires_minus_date() {
    let list = e::premium::BoostsList::BoostsList(t::premium::BoostsList {
        count: 1,
        boosts: vec![e::Boost::Boost(t::Boost {
            gift: false,
            giveaway: false,
            unclaimed: false,
            id: String::new(),
            user_id: None,
            giveaway_msg_id: None,
            date: 1,
            expires: i32::MIN,
            used_gift_slug: None,
            multiplier: None,
            stars: None,
        })],
        next_offset: None,
        users: Vec::new(),
    });
    let peer = tg_data::PeerId::from_user(tg_data::UserId(1));
    let _ = tg_payments::premium::boosts_list_from_tl(&list, peer, false);
}

/// TGB-3093: `tg-payments/src/star_gifts.rs:984`, a unique gift with
/// `availability_issued = i32::MIN`.
#[test]
fn unique_gift_availability_left() {
    let gift = e::StarGift::Unique(t::StarGiftUnique {
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
        availability_issued: i32::MIN,
        availability_total: 0,
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
    });
    let _ = tg_payments::star_gifts::star_gift_from_tl(&gift);
}
