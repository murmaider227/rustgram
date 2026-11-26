//! TGB-2959: the TL fields a reply header's media preview needs reach `tg_data`: a video's
//! cover, the "photo is the video's cover" flag of a web page, the media sent along with a
//! reply to another chat's message, media attached to a poll and to its answers, and an
//! invoice's product photo.

use tg_api::convert::{message_media_from_tl, reply_info_from_tl, webpage_from_tl};
use tg_data::{MessageMedia, PhotoId};
use tg_tl::{enums as tl_enums, types as tl_types};

fn photo(id: i64) -> tl_enums::Photo {
    tl_enums::Photo::Photo(tl_types::Photo {
        has_stickers: false,
        id,
        access_hash: 4242,
        file_reference: vec![1, 2, 3],
        date: 1_700_000_000,
        sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
            r#type: "m".into(),
            w: 320,
            h: 240,
            size: 12_000,
        })],
        video_sizes: None,
        dc_id: 2,
    })
}

fn video(id: i64) -> tl_enums::Document {
    tl_enums::Document::Document(tl_types::Document {
        id,
        access_hash: 9876,
        file_reference: vec![9, 8],
        date: 1_700_000_100,
        mime_type: "video/mp4".to_owned(),
        size: 1_048_576,
        thumbs: None,
        video_thumbs: None,
        dc_id: 2,
        attributes: Vec::new(),
    })
}

fn photo_media(id: i64) -> tl_enums::MessageMedia {
    tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
        spoiler: false,
        live_photo: false,
        photo: Some(photo(id)),
        ttl_seconds: None,
        video: None,
    })
}

fn text(text: &str) -> tl_enums::TextWithEntities {
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: text.to_owned(),
        entities: Vec::new(),
    })
}

#[test]
fn a_video_keeps_its_cover() {
    let media = message_media_from_tl(&tl_enums::MessageMedia::Document(
        tl_types::MessageMediaDocument {
            nopremium: false,
            spoiler: false,
            video: true,
            round: false,
            voice: false,
            document: Some(video(5)),
            alt_documents: None,
            video_cover: Some(photo(77)),
            video_timestamp: None,
            ttl_seconds: None,
        },
    ));
    let MessageMedia::Document(doc) = media else {
        panic!("expected a document, got {media:?}");
    };
    assert_eq!(doc.video_cover.map(|cover| cover.id), Some(PhotoId(77)));
}

#[test]
fn a_web_page_keeps_the_cover_flag() {
    let page = |video_cover_photo| {
        webpage_from_tl(&tl_enums::WebPage::WebPage(tl_types::WebPage {
            has_large_media: false,
            video_cover_photo,
            id: 777,
            url: "https://example.org/clip".into(),
            display_url: "example.org/clip".into(),
            hash: 55,
            r#type: Some("video".into()),
            site_name: None,
            title: None,
            description: None,
            photo: Some(photo(78)),
            embed_url: None,
            embed_type: None,
            embed_width: None,
            embed_height: None,
            duration: None,
            author: None,
            document: Some(video(6)),
            cached_page: None,
            attributes: None,
        }))
        .expect("web page")
    };
    assert!(page(true).photo_is_video_cover);
    assert!(!page(false).photo_is_video_cover);
}

#[test]
fn a_reply_header_keeps_the_media_of_the_other_chat() {
    let header = |reply_media| {
        reply_info_from_tl(&tl_enums::MessageReplyHeader::MessageReplyHeader(
            tl_types::MessageReplyHeader {
                reply_to_scheduled: false,
                forum_topic: false,
                quote: false,
                reply_to_ephemeral: false,
                reply_to_msg_id: Some(4000),
                reply_to_peer_id: None,
                reply_from: None,
                reply_media,
                reply_to_top_id: None,
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                todo_item_id: None,
                poll_option: None,
                quote_rich: None,
            },
        ))
    };
    let with_media = header(Some(photo_media(79)));
    assert!(matches!(
        with_media.reply_media.as_deref(),
        Some(MessageMedia::Photo(photo)) if photo.id == PhotoId(79)
    ));
    assert!(header(None).reply_media.is_none());
    assert!(
        header(Some(tl_enums::MessageMedia::Empty))
            .reply_media
            .is_none(),
        "messageMediaEmpty is no media"
    );
}

#[test]
fn a_poll_keeps_its_attached_media_and_its_answers_media() {
    let answer = |option: u8, media| {
        tl_enums::PollAnswer::PollAnswer(tl_types::PollAnswer {
            text: text("answer"),
            option: vec![option],
            media,
            added_by: None,
            date: None,
        })
    };
    let media = message_media_from_tl(&tl_enums::MessageMedia::Poll(tl_types::MessageMediaPoll {
        poll: tl_enums::Poll::Poll(tl_types::Poll {
            id: 9,
            closed: false,
            public_voters: false,
            multiple_choice: false,
            quiz: false,
            open_answers: false,
            revoting_disabled: false,
            shuffle_answers: false,
            hide_results_until_close: false,
            creator: false,
            subscribers_only: false,
            question: text("Which one?"),
            answers: vec![answer(0, None), answer(1, Some(photo_media(81)))],
            close_period: None,
            close_date: None,
            countries_iso2: None,
            hash: 0,
        }),
        results: Box::new(tl_enums::PollResults::PollResults(tl_types::PollResults {
            min: false,
            has_unread_votes: false,
            can_view_stats: false,
            results: None,
            total_voters: None,
            recent_voters: None,
            solution: None,
            solution_entities: None,
            solution_media: None,
        })),
        attached_media: Some(Box::new(photo_media(80))),
    }));
    let MessageMedia::Poll(poll) = media else {
        panic!("expected a poll, got {media:?}");
    };
    let photo_id = |media: Option<&tg_data::PollMedia>| {
        media.and_then(|m| m.photo.as_ref()).map(|photo| photo.id)
    };
    assert_eq!(photo_id(poll.attached_media.as_deref()), Some(PhotoId(80)));
    assert_eq!(photo_id(poll.media_of_answer(&[1])), Some(PhotoId(81)));
    assert!(poll.media_of_answer(&[0]).is_none());
    assert!(poll.has_answer(&[0]) && !poll.has_answer(&[7]));
}

#[test]
fn an_invoice_keeps_its_product_photo() {
    let invoice = |photo| {
        let media = message_media_from_tl(&tl_enums::MessageMedia::Invoice(
            tl_types::MessageMediaInvoice {
                shipping_address_requested: false,
                test: true,
                title: "Pro Plan".to_owned(),
                description: "Monthly".to_owned(),
                photo,
                receipt_msg_id: None,
                currency: "USD".to_owned(),
                total_amount: 999,
                start_param: "pro".to_owned(),
                extended_media: None,
            },
        ));
        let MessageMedia::Invoice(invoice) = media else {
            panic!("expected an invoice, got {media:?}");
        };
        invoice.photo
    };
    let size = vec![tl_enums::DocumentAttribute::ImageSize(
        tl_types::DocumentAttributeImageSize { w: 640, h: 480 },
    )];
    let proxied = invoice(Some(tl_enums::WebDocument::WebDocument(
        tl_types::WebDocument {
            url: "https://shop.example/item.jpg".to_owned(),
            access_hash: 55,
            size: 20_000,
            mime_type: "image/jpeg".to_owned(),
            attributes: size.clone(),
        },
    )))
    .expect("photo");
    assert_eq!(proxied.url, "https://shop.example/item.jpg");
    assert_eq!(proxied.access_hash, Some(55));
    assert_eq!((proxied.width, proxied.height), (640, 480));

    let direct = invoice(Some(tl_enums::WebDocument::NoProxy(
        tl_types::WebDocumentNoProxy {
            url: "https://shop.example/item.jpg".to_owned(),
            size: 20_000,
            mime_type: "image/jpeg".to_owned(),
            attributes: size,
        },
    )))
    .expect("photo");
    assert_eq!(direct.access_hash, None);
    assert!(invoice(None).is_none());
}
