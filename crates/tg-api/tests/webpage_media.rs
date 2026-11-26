//! `messageMediaWebPage` -> domain media, as `CreateMedia` in tdesktop's
//! `history/history_item.cpp` maps it: an empty or not-modified page is no media at all, a
//! pending page is kept (so `updateWebPage` can fill it) but marked as pending.

use tg_api::convert::media::message_media_from_tl;
use tg_data::MessageMedia;
use tg_tl::{enums as tl_enums, types as tl_types};

fn web_page_media(webpage: tl_enums::WebPage) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::WebPage(tl_types::MessageMediaWebPage {
        force_large_media: false,
        force_small_media: false,
        manual: false,
        safe: false,
        webpage,
    })
}

#[test]
fn empty_web_page_creates_no_media() {
    let media = web_page_media(tl_enums::WebPage::Empty(tl_types::WebPageEmpty {
        id: 0,
        url: Some("https://t.me/c/1234567890/42".into()),
    }));
    assert_eq!(message_media_from_tl(&media), MessageMedia::Empty);
}

#[test]
fn not_modified_web_page_creates_no_media() {
    let media = web_page_media(tl_enums::WebPage::NotModified(
        tl_types::WebPageNotModified {
            cached_page_views: None,
        },
    ));
    assert_eq!(message_media_from_tl(&media), MessageMedia::Empty);
}

#[test]
fn pending_web_page_is_kept_and_marked_pending() {
    let media = web_page_media(tl_enums::WebPage::Pending(tl_types::WebPagePending {
        id: 555,
        url: Some("https://example.org".into()),
        date: 1_700_000_000,
    }));
    let MessageMedia::WebPage(page) = message_media_from_tl(&media) else {
        panic!("a pending page stays a web page media");
    };
    assert!(page.is_pending());
    assert_eq!(page.id.0, 555);
}
