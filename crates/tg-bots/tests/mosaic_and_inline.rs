//! Known-answer tests for `MosaicLayout` (exact port of `layout/layout_mosaic.cpp`)
//! and inline bot results, gallery vs list decisions, query parsing, debounce, and pagination.

use tg_bots::inline::{
    GetInlineBotResultsRequest, INLINE_BOT_REQUEST_DELAY_MS, InlineLayoutKind, InlineQueryAction,
    InlineQueryController, InlineResult, InlineResultKind, InlineResultsPage, MediaDimensions,
    ParsedInlineQuery, SwitchPmButton, choose_layout_kind, compute_item_dimensions,
    layout_inline_results, parse_inline_bot_query, prevent_inline_bot_query, style,
};
use tg_bots::keyboard_layout::Rect;
use tg_bots::mosaic::{
    INLINE_ITEMS_MAX_PER_ROW, MATRIX_ROW_SHIFT, Margins, MosaicLayout, Position, SimpleMosaicItem,
    index_to_position, position_to_index,
};
use tg_bots::stand_in::{DocumentId, PeerId, PhotoId, UserId};

fn make_result(id: &str, kind: InlineResultKind, w: i32, h: i32) -> InlineResult {
    InlineResult {
        query_id: 777,
        id: id.to_string(),
        kind,
        title: format!("Title {id}"),
        description: format!("Desc {id}"),
        url: String::new(),
        content_url: String::new(),
        photo_id: matches!(kind, InlineResultKind::Photo).then_some(PhotoId(1)),
        document_id: kind.requires_document().then_some(DocumentId(2)),
        dimensions: MediaDimensions {
            width: w,
            height: h,
        },
        has_thumbnail: false,
        location: None,
        duration_secs: None,
        reply_markup: None,
    }
}

#[test]
fn mosaic_position_encoding_known_answers() {
    assert_eq!(MATRIX_ROW_SHIFT, 40_000);
    assert_eq!(position_to_index(0, 0), 0);
    assert_eq!(position_to_index(3, 4), 120_004);
    assert_eq!(index_to_position(120_004), Position { row: 3, column: 4 });
    assert_eq!(
        index_to_position(-1),
        Position {
            row: -1,
            column: -1
        }
    );
}

#[test]
fn mosaic_layout_known_answer_big_width_and_min_clamp() {
    // Reproduce exact `layout/layout_mosaic.cpp` arithmetic:
    // big_width = 320, full_width = 320, right_skip = 3, padding = 0, min_item_width = 48.
    // Row 0 items: max_widths = [50, 200, 150] (all height 99).
    // add_item(0): sumWidth = 50 (< 320).
    // add_item(1): sumWidth = 50 + 200 + 3 = 253 (< 320).
    // add_item(2): sumWidth = 253 + 150 + 3 = 406 (>= 320, finalized on next item or force).
    // layoutRow on [50, 200, 150] with row.max_width = 406, availableWidth = 320:
    // Sorted indices by max_width: [0 (50), 2 (150), 1 (200)].
    // Step 0 (index=0, maxw=50):
    //   w = (50 * 320) / 406 = 16000 / 406 = 39 -> clamped to actualWidth = 48!
    //   availableWidth = 320 - 48 = 272; desiredWidth = 406 - 50 = 356; (index == 0 -> no skip sub).
    // Step 1 (index=2, maxw=150):
    //   w = (150 * 272) / 356 = 40800 / 356 = 114 -> actualWidth = 114.
    //   availableWidth = 272 - 114 = 158; desiredWidth = 356 - 150 = 206.
    //   index > 0 -> availableWidth = 155, desiredWidth = 203.
    // Step 2 (index=1, maxw=200):
    //   w = (200 * 155) / 203 = 31000 / 203 = 152 -> actualWidth = 152.
    //   availableWidth = 3; desiredWidth = 3; index > 0 -> availableWidth = 0, desiredWidth = 0.
    // Total row pixel span = 48 + 3 + 152 + 3 + 114 = 320!
    let mut mosaic = MosaicLayout::new(320);
    mosaic.set_full_width(320);
    mosaic.set_right_skip(3);
    mosaic.add_items(vec![
        SimpleMosaicItem::new(0, 50, 99),
        SimpleMosaicItem::new(1, 200, 99),
        SimpleMosaicItem::new(2, 150, 99),
        // Item 3 triggers row 0 finalization and lands in row 1 (not full/big -> keeps natural max_width = 100).
        SimpleMosaicItem::new(3, 100, 99),
    ]);

    assert_eq!(mosaic.rows_count(), 2);
    assert_eq!(mosaic.rows()[0].max_width, 406);
    assert_eq!(mosaic.rows()[0].items[0].width, 48);
    assert_eq!(mosaic.rows()[0].items[1].width, 152);
    assert_eq!(mosaic.rows()[0].items[2].width, 114);
    assert_eq!(mosaic.rows()[0].height, 99);

    // Row 1 is not full (< 5) and not big (100 < 320), so row.max_width == 0 and item keeps 100px.
    assert_eq!(mosaic.rows()[1].max_width, 0);
    assert_eq!(mosaic.rows()[1].items[0].width, 100);
    assert_eq!(mosaic.rows()[1].items[0].position, position_to_index(1, 0));

    // Exact rectangles & find_by_point checks.
    assert_eq!(
        mosaic.find_rect(position_to_index(0, 0)),
        Some(Rect::new(0, 0, 48, 99))
    );
    assert_eq!(
        mosaic.find_rect(position_to_index(0, 1)),
        Some(Rect::new(51, 0, 152, 99))
    );
    assert_eq!(
        mosaic.find_rect(position_to_index(0, 2)),
        Some(Rect::new(206, 0, 114, 99))
    );
    assert_eq!(
        mosaic.find_rect(position_to_index(1, 0)),
        Some(Rect::new(0, 99, 100, 99))
    );

    let hit = mosaic.find_by_point(60, 20);
    assert_eq!(hit.index, position_to_index(0, 1));
    assert!(hit.exact);
    assert_eq!((hit.relative_x, hit.relative_y), (9, 20));

    // Point past the right edge of row 1 clamps to col 0 with exact = false.
    let miss = mosaic.find_by_point(150, 110);
    assert_eq!(miss.index, position_to_index(1, 0));
    assert!(!miss.exact);
}

#[test]
fn mosaic_layout_max_five_items_per_row_and_validate_existing() {
    // 6 small items of width 48 with big_width = 1000:
    // Row 0 must finalize at INLINE_ITEMS_MAX_PER_ROW (5) items, putting item 5 in row 1.
    let mut mosaic = MosaicLayout::new(1000);
    mosaic.set_full_width(300);
    mosaic.set_right_skip(3);
    mosaic.set_padding(Margins::new(10, 5, 10, 5));
    let items: Vec<_> = (0..6).map(|i| SimpleMosaicItem::new(i, 48, 64)).collect();
    mosaic.add_items(items);

    assert_eq!(mosaic.rows_count(), 2);
    assert_eq!(mosaic.rows()[0].items.len(), INLINE_ITEMS_MAX_PER_ROW);
    assert_eq!(mosaic.rows()[1].items.len(), 1);
    // Total height = top(5) + 64 + 64 + bottom(5) = 138.
    assert_eq!(mosaic.count_desired_height(300), 138);

    // Pagination append: 8 items where first 6 match -> last row (row 1) is discarded so it can be refilled!
    // Wait: `validate_existing_rows` keeps row 0 (5 items) and drops row 1 because row 1 might not be full.
    let until = mosaic.validate_existing_rows(|item, idx| item.id != idx, 8);
    assert_eq!(until, 5);
    assert_eq!(mosaic.rows_count(), 1);

    // Shrink to 3 items: keeps first 3 of row 0 and re-layouts row 0.
    let kept = mosaic.validate_existing_rows(|item, idx| item.id != idx, 3);
    assert_eq!(kept, 3);
    assert_eq!(mosaic.rows_count(), 1);
    assert_eq!(mosaic.rows()[0].items.len(), 3);
}

#[test]
fn inline_gallery_vs_list_decision_and_dimensions() {
    let photo = make_result("p1", InlineResultKind::Photo, 200, 100);
    let gif = make_result("g1", InlineResultKind::Gif, 100, 200);
    let article = make_result("a1", InlineResultKind::Article, 0, 0);
    let voice = make_result("v1", InlineResultKind::Voice, 0, 0);
    let sticker = make_result("s1", InlineResultKind::Sticker, 512, 512);

    // Default mode (gallery = None)
    assert_eq!(
        choose_layout_kind(&photo, false, None),
        InlineLayoutKind::Photo
    );
    assert_eq!(choose_layout_kind(&gif, false, None), InlineLayoutKind::Gif);
    assert_eq!(
        choose_layout_kind(&article, true, None),
        InlineLayoutKind::Article { with_thumb: true }
    );
    assert_eq!(
        choose_layout_kind(&voice, false, None),
        InlineLayoutKind::File
    );
    assert_eq!(
        choose_layout_kind(&sticker, false, None),
        InlineLayoutKind::Sticker
    );

    // Forced list mode (gallery = Some(false)): Photo/Gif/Sticker become Article.
    assert_eq!(
        choose_layout_kind(&photo, true, Some(false)),
        InlineLayoutKind::Article { with_thumb: true }
    );
    assert_eq!(
        choose_layout_kind(&gif, false, Some(false)),
        InlineLayoutKind::Article { with_thumb: false }
    );

    // Forced gallery mode (gallery = Some(true)): Article/Voice/Video/Game become Thumbnail.
    assert_eq!(
        choose_layout_kind(&article, false, Some(true)),
        InlineLayoutKind::Thumbnail
    );
    assert_eq!(
        choose_layout_kind(&voice, false, Some(true)),
        InlineLayoutKind::Thumbnail
    );

    // Exact dimension calculations:
    // Photo 200x100 -> w = 200 * 96 / 100 = 192, h = 96 + 3 = 99.
    let photo_dims = compute_item_dimensions(&photo, InlineLayoutKind::Photo, 1, 0);
    assert_eq!(photo_dims.max_width, 192);
    assert_eq!(
        photo_dims.min_height,
        style::INLINE_MEDIA_HEIGHT + style::INLINE_RESULTS_SKIP
    );

    // Gif 100x200 -> w = 100 * 96 / 200 = 48 (exact min width), h = 99.
    let gif_dims = compute_item_dimensions(&gif, InlineLayoutKind::Gif, 1, 0);
    assert_eq!(gif_dims.max_width, 48);
    assert_eq!(gif_dims.min_height, 99);

    // Gif with 0x0 dimensions -> max_width = 0 (skipped in Inner::layoutPrepareInlineResult).
    let zero_gif = make_result("g0", InlineResultKind::Gif, 0, 0);
    assert_eq!(
        compute_item_dimensions(&zero_gif, InlineLayoutKind::Gif, 1, 0).max_width,
        0
    );

    // Full gallery layout with switch_pm button offset.
    let out = layout_inline_results(
        &[photo, gif],
        true,
        style::DEFAULT_MOSAIC_BIG_WIDTH + style::INLINE_RESULTS_LEFT,
        style::DEFAULT_MOSAIC_BIG_WIDTH,
        Some(34),
    );
    assert_eq!(out.items.len(), 2);
    // Top starts at stickerPanPadding(11) + switchPm(34) + skip(3) = 48.
    assert_eq!(out.items[0].2.y, 48);
    assert_eq!(out.items[1].2.y, 48);
    assert_eq!(out.items[0].2.x, style::INLINE_RESULTS_LEFT);
}

#[test]
fn inline_query_parsing_debounce_pagination_and_switch_pm() {
    // ParseInlineBotQuery known answers.
    assert_eq!(
        parse_inline_bot_query("@gif cats", None),
        Some(ParsedInlineQuery {
            username: "gif".into(),
            query: "cats".into(),
        })
    );
    // Without trailing space, only accepted if username ends with "bot".
    assert_eq!(parse_inline_bot_query("@gif", None), None);
    assert_eq!(
        parse_inline_bot_query("@myinlinebot", None),
        Some(ParsedInlineQuery {
            username: "myinlinebot".into(),
            query: String::new(),
        })
    );
    // Formatted tag overlapping username rejects inline query.
    assert_eq!(parse_inline_bot_query("@gif cats", Some(2)), None);
    assert!(parse_inline_bot_query("@gif cats", Some(5)).is_some());
    assert_eq!(prevent_inline_bot_query("@gif hi"), " @gif hi");
    assert_eq!(prevent_inline_bot_query("hello"), "hello");

    // InlineQueryController debounce (400 ms) + pagination + cache hit.
    let mut ctrl = InlineQueryController::new();
    let bot = UserId(55);
    let peer = PeerId(99);

    let act = ctrl.on_query_changed(bot, peer, "dogs", 1_000);
    assert_eq!(
        act,
        InlineQueryAction::ScheduleDebounce {
            delay_ms: INLINE_BOT_REQUEST_DELAY_MS,
            cancel_in_flight: false,
        }
    );
    assert_eq!(ctrl.debounce_deadline_ms(), Some(1_400));
    assert_eq!(ctrl.on_timer_tick(1_399), None);

    let req1 = ctrl.on_timer_tick(1_400).unwrap();
    assert_eq!(
        req1,
        GetInlineBotResultsRequest {
            bot_id: bot,
            peer_id: peer,
            query: "dogs".into(),
            offset: String::new(),
        }
    );
    assert!(ctrl.is_requesting());

    // First page arrives with next_offset = "page2" and switch_pm.
    let added1 = ctrl.on_results_received(InlineResultsPage {
        query: "dogs".into(),
        next_offset: Some("page2".into()),
        switch_pm: Some(SwitchPmButton::StartPm {
            text: "Configure bot".into(),
            start_param: "setup".into(),
        }),
        gallery: true,
        cache_time: 60,
        results: vec![make_result("1", InlineResultKind::Photo, 120, 120)],
    });
    assert_eq!(added1, 1);
    assert!(!ctrl.is_requesting());
    assert_eq!(
        ctrl.current_entry().unwrap().switch_pm,
        Some(SwitchPmButton::StartPm {
            text: "Configure bot".into(),
            start_param: "setup".into(),
        })
    );

    // Scrolling to bottom requests offset = "page2".
    let req2 = ctrl.on_scroll_bottom().unwrap();
    assert_eq!(req2.offset, "page2");

    // Second page returns 0 valid results -> next_offset is cleared automatically.
    let mut invalid_photo = make_result("bad", InlineResultKind::Photo, 100, 100);
    invalid_photo.photo_id = None;
    let added2 = ctrl.on_results_received(InlineResultsPage {
        query: "dogs".into(),
        next_offset: Some("page3".into()),
        switch_pm: None,
        gallery: true,
        cache_time: 60,
        results: vec![invalid_photo],
    });
    assert_eq!(added2, 0);
    assert_eq!(ctrl.on_scroll_bottom(), None);

    // Switching query and returning to "dogs" serves directly from cache without debounce.
    let _ = ctrl.on_query_changed(bot, peer, "cats", 2_000);
    let cached = ctrl.on_query_changed(bot, peer, "dogs", 2_100);
    assert_eq!(
        cached,
        InlineQueryAction::ShowCached {
            cancel_in_flight: false,
        }
    );
    assert_eq!(ctrl.active_query(), "dogs");
    assert_eq!(ctrl.current_entry().unwrap().results.len(), 1);
}
