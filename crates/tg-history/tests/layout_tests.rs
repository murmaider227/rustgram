//! Known-answer and behavioural tests for `tg-history`.

use tg_history::consts::{
    ATTACH_MESSAGE_TO_PREVIOUS_SECONDS_DELTA, DIALOGS_IMPORTANT_BAR_HEIGHT, DIALOGS_ROW_HEIGHT,
    HISTORY_GROUP_SKIP, HISTORY_GROUP_WIDTH_MAX, HISTORY_GROUP_WIDTH_MIN, MAX_MEDIA_SIZE,
    MAX_SIGNATURE_SIZE, MAX_VIDEO_MESSAGE_SIZE, MSG_MARGIN, MSG_MARGIN_TOP_ATTACHED, MSG_MAX_WIDTH,
    MSG_PADDING,
};
use tg_history::*;

// ===========================================================================
// 1. Grouped media layout (ui/grouped_layout.cpp & history_view_media_grouped.cpp)
// ===========================================================================

#[test]
fn grouped_layout_empty_and_single() {
    let empty = layout_media_group(
        &[],
        HISTORY_GROUP_WIDTH_MAX,
        HISTORY_GROUP_WIDTH_MIN,
        HISTORY_GROUP_SKIP,
    );
    assert!(empty.is_empty());

    let one = layout_media_group(
        &[Size::new(800, 600)],
        HISTORY_GROUP_WIDTH_MAX,
        HISTORY_GROUP_WIDTH_MIN,
        HISTORY_GROUP_SKIP,
    );
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].geometry, Rect::new(0, 0, 430, 322));
    assert_eq!(one[0].sides, RectParts::ALL_SIDES);
}

#[test]
fn grouped_layout_two_wide_top_bottom() {
    // Two 16:9 photos: proportions="ww", avgRatio = (1 + 16/9 + 16/9)/2 = 2.2778 > 1.4, diff = 0 < 0.2.
    let res = layout_media_group(&[Size::new(1600, 900), Size::new(1600, 900)], 430, 100, 4);
    assert_eq!(res.len(), 2);
    assert_eq!(res[0].geometry, Rect::new(0, 0, 430, 213));
    assert_eq!(
        res[0].sides,
        RectParts::LEFT | RectParts::TOP | RectParts::RIGHT
    );
    assert_eq!(res[1].geometry, Rect::new(0, 217, 430, 213));
    assert_eq!(
        res[1].sides,
        RectParts::LEFT | RectParts::BOTTOM | RectParts::RIGHT
    );
}

#[test]
fn grouped_layout_two_square_left_right_equal() {
    let res = layout_media_group(&[Size::new(500, 500), Size::new(500, 500)], 430, 100, 4);
    assert_eq!(res.len(), 2);
    assert_eq!(res[0].geometry, Rect::new(0, 0, 213, 213));
    assert_eq!(
        res[0].sides,
        RectParts::TOP | RectParts::LEFT | RectParts::BOTTOM
    );
    assert_eq!(res[1].geometry, Rect::new(217, 0, 213, 213));
    assert_eq!(
        res[1].sides,
        RectParts::TOP | RectParts::RIGHT | RectParts::BOTTOM
    );
}

#[test]
fn grouped_layout_two_mixed_left_right() {
    // Narrow + wide: proportions="nw" -> layoutTwoLeftRight()
    let res = layout_media_group(&[Size::new(600, 1000), Size::new(1000, 600)], 430, 100, 4);
    assert_eq!(res.len(), 2);
    assert_eq!(res[0].geometry, Rect::new(0, 0, 150, 166));
    assert_eq!(
        res[0].sides,
        RectParts::TOP | RectParts::LEFT | RectParts::BOTTOM
    );
    assert_eq!(res[1].geometry, Rect::new(154, 0, 276, 166));
    assert_eq!(
        res[1].sides,
        RectParts::TOP | RectParts::RIGHT | RectParts::BOTTOM
    );
}

#[test]
fn grouped_layout_three_left_and_other() {
    let res = layout_media_group(
        &[
            Size::new(600, 1000),
            Size::new(800, 600),
            Size::new(800, 600),
        ],
        430,
        100,
        4,
    );
    assert_eq!(res.len(), 3);
    assert_eq!(res[0].geometry, Rect::new(0, 0, 213, 430));
    assert_eq!(
        res[0].sides,
        RectParts::TOP | RectParts::LEFT | RectParts::BOTTOM
    );
    assert_eq!(res[1].geometry, Rect::new(217, 0, 213, 213));
    assert_eq!(res[1].sides, RectParts::TOP | RectParts::RIGHT);
    assert_eq!(res[2].geometry, Rect::new(217, 217, 213, 213));
    assert_eq!(res[2].sides, RectParts::BOTTOM | RectParts::RIGHT);
}

#[test]
fn grouped_layout_three_top_and_other() {
    let res = layout_media_group(
        &[
            Size::new(1600, 900),
            Size::new(1000, 1000),
            Size::new(1000, 1000),
        ],
        430,
        100,
        4,
    );
    assert_eq!(res.len(), 3);
    assert_eq!(res[0].geometry, Rect::new(0, 0, 430, 242));
    assert_eq!(
        res[0].sides,
        RectParts::LEFT | RectParts::TOP | RectParts::RIGHT
    );
    assert_eq!(res[1].geometry, Rect::new(0, 246, 213, 184));
    assert_eq!(res[1].sides, RectParts::BOTTOM | RectParts::LEFT);
    assert_eq!(res[2].geometry, Rect::new(217, 246, 213, 184));
    assert_eq!(res[2].sides, RectParts::BOTTOM | RectParts::RIGHT);
}

#[test]
fn grouped_layout_four_top_and_other() {
    let res = layout_media_group(
        &[
            Size::new(1600, 900),
            Size::new(1000, 1000),
            Size::new(1000, 1000),
            Size::new(1000, 1000),
        ],
        430,
        100,
        4,
    );
    assert_eq!(res.len(), 4);
    assert_eq!(res[0].geometry, Rect::new(0, 0, 430, 242));
    assert_eq!(
        res[0].sides,
        RectParts::LEFT | RectParts::TOP | RectParts::RIGHT
    );
    assert_eq!(res[1].geometry, Rect::new(0, 246, 141, 141));
    assert_eq!(res[1].sides, RectParts::BOTTOM | RectParts::LEFT);
    assert_eq!(res[2].geometry, Rect::new(145, 246, 140, 141));
    assert_eq!(res[2].sides, RectParts::BOTTOM);
    assert_eq!(res[3].geometry, Rect::new(289, 246, 141, 141));
    assert_eq!(res[3].sides, RectParts::RIGHT | RectParts::BOTTOM);
}

#[test]
fn grouped_layout_four_left_and_other() {
    let res = layout_media_group(
        &[
            Size::new(600, 1000),
            Size::new(1000, 1000),
            Size::new(1000, 1000),
            Size::new(1000, 1000),
        ],
        430,
        100,
        4,
    );
    assert_eq!(res.len(), 4);
    assert_eq!(res[0].geometry, Rect::new(0, 0, 256, 430));
    assert_eq!(
        res[0].sides,
        RectParts::TOP | RectParts::LEFT | RectParts::BOTTOM
    );
    assert_eq!(res[1].geometry, Rect::new(260, 0, 141, 141));
    assert_eq!(res[1].sides, RectParts::TOP | RectParts::RIGHT);
    assert_eq!(res[2].geometry, Rect::new(260, 145, 141, 141));
    assert_eq!(res[2].sides, RectParts::RIGHT);
    assert_eq!(res[3].geometry, Rect::new(260, 290, 141, 140));
    assert_eq!(res[3].sides, RectParts::BOTTOM | RectParts::RIGHT);
}

#[test]
fn grouped_layout_five_items_complex_scoring() {
    let sizes = [Size::new(100, 100); 5];
    let res = layout_media_group(&sizes, 430, 100, 4);
    assert_eq!(res.len(), 5);
    // Optimal partition is [2, 3]: row 0 has 2 items of height 213, row 1 has 3 items of height 141.
    assert_eq!(res[0].geometry, Rect::new(0, 0, 213, 213));
    assert_eq!(res[0].sides, RectParts::TOP | RectParts::LEFT);
    assert_eq!(res[1].geometry, Rect::new(217, 0, 213, 213));
    assert_eq!(res[1].sides, RectParts::TOP | RectParts::RIGHT);
    assert_eq!(res[2].geometry, Rect::new(0, 217, 141, 141));
    assert_eq!(res[2].sides, RectParts::BOTTOM | RectParts::LEFT);
    assert_eq!(res[3].geometry, Rect::new(145, 217, 141, 141));
    assert_eq!(res[3].sides, RectParts::BOTTOM);
    assert_eq!(res[4].geometry, Rect::new(290, 217, 140, 141));
    assert_eq!(res[4].sides, RectParts::BOTTOM | RectParts::RIGHT);
}

#[test]
fn grouped_layout_panorama_triggers_complex_layouter() {
    // ratio = 2.5 > 2.0 forces ComplexLayouter even for count == 2.
    let res = layout_media_group(&[Size::new(1000, 400), Size::new(1000, 400)], 430, 100, 4);
    assert_eq!(res.len(), 2);
    assert_eq!(res[0].geometry, Rect::new(0, 0, 430, 172));
    assert_eq!(
        res[0].sides,
        RectParts::TOP | RectParts::LEFT | RectParts::RIGHT
    );
    assert_eq!(res[1].geometry, Rect::new(0, 176, 430, 172));
    assert_eq!(
        res[1].sides,
        RectParts::BOTTOM | RectParts::LEFT | RectParts::RIGHT
    );
}

#[test]
fn grouped_layout_ten_items_and_rescaling() {
    let fonts = FontMetrics::default();
    let input = GroupedMediaInput {
        mode: GroupedMode::Grid,
        part_sizes: vec![Size::new(800, 600); 10],
        is_bubble_top: true,
        is_bubble_bottom: true,
        last_item_has_text: false,
        column_extra_bottom_info: false,
        hosted_instant_view: false,
    };
    let mut layout = GroupedMediaLayout::compute_optimal(&input, &fonts);
    assert_eq!(layout.parts.len(), 10);
    assert_eq!(layout.optimal_size.width, 430);
    assert!(layout.optimal_size.height > 0);

    // Rescale down to half width (215px) and verify outer right edge aligns cleanly.
    let resized = layout.resize_to_width(215, &input, None, &fonts);
    assert_eq!(resized.width, 215);
    assert!(resized.height > 0 && resized.height < layout.optimal_size.height);
    let max_right = layout
        .parts
        .iter()
        .map(|p| p.geometry.right())
        .max()
        .unwrap_or(0);
    assert_eq!(max_right, 215);

    // Corner rounding restriction: interior part loses all corners.
    let rounded = apply_rounding_sides(BubbleRounding::ALL_LARGE, RectParts::TOP | RectParts::LEFT);
    assert_eq!(rounded.top_left, BubbleCornerRounding::Large);
    assert_eq!(rounded.top_right, BubbleCornerRounding::None);
    assert_eq!(rounded.bottom_left, BubbleCornerRounding::None);
    assert_eq!(rounded.bottom_right, BubbleCornerRounding::None);
}

#[test]
fn grouped_playlist_column_and_selection_intervals() {
    let fonts = FontMetrics::default();
    let input = GroupedMediaInput {
        mode: GroupedMode::Column,
        part_sizes: vec![Size::new(280, 54), Size::new(300, 54), Size::new(268, 54)],
        is_bubble_top: true,
        is_bubble_bottom: true,
        last_item_has_text: false,
        column_extra_bottom_info: false,
        hosted_instant_view: false,
    };
    let layout = GroupedMediaLayout::compute_optimal(&input, &fonts);
    assert_eq!(layout.optimal_size.width, 300);
    // Padding top = (8 - 5) - 0 = 3, bottom = (8 - 5) + 0 = 3 -> total height = 162 + 6 = 168.
    assert_eq!(layout.padding, Margins::new(0, 3, 0, 3));
    assert_eq!(layout.optimal_size.height, 168);

    // Select items 0 and 1 via subgroup bitmask -> merged into single band extending to top=0.
    let mut sel = add_group_item_selection(TextSelection::EMPTY, 0);
    sel = add_group_item_selection(sel, 1);
    let intervals = layout.bubble_selection_intervals(sel);
    assert_eq!(intervals.len(), 1);
    assert_eq!(intervals[0].top, 0);
    assert_eq!(intervals[0].height, 111); // 3 + 54 + 54
}

// ===========================================================================
// 2. Attach-to-previous / next grouping & date separator rules
// ===========================================================================

fn make_meta(from: i64, unix_secs: i32) -> AttachMetadata {
    AttachMetadata {
        date: unix_secs,
        date_time: LocalDateTime::from_unix_utc(i64::from(unix_secs)),
        from: PeerId(from),
        display_from: Some(PeerId(from)),
        ..Default::default()
    }
}

#[test]
fn attach_to_previous_same_author_time_window() {
    let base = 1_700_000_000; // 2023-11-14 22:13:20 UTC
    let a = make_meta(10, base);
    let b_within = make_meta(10, base + ATTACH_MESSAGE_TO_PREVIOUS_SECONDS_DELTA - 1);
    let c_at_limit = make_meta(10, base + ATTACH_MESSAGE_TO_PREVIOUS_SECONDS_DELTA);
    let d_other = make_meta(20, base + 5);

    assert!(compute_is_attach_to_previous(&a, &b_within, false));
    // Strictly `< 900`: delta == 900 does NOT attach.
    assert!(!compute_is_attach_to_previous(&a, &c_at_limit, false));
    // Different author does NOT attach.
    assert!(!compute_is_attach_to_previous(&a, &d_other, false));
    // Top separator (date badge / unread bar) blocks attachment.
    assert!(!compute_is_attach_to_previous(&a, &b_within, true));
}

#[test]
fn attach_to_previous_special_rules_markup_service_saved_megagroup() {
    let base = 1_700_000_000;

    // Inline reply markup on previous message prevents attachment.
    let mut prev_kb = make_meta(10, base);
    prev_kb.has_visible_inline_markup = true;
    let cur = make_meta(10, base + 10);
    assert!(!compute_is_attach_to_previous(&prev_kb, &cur, false));

    // Service message never attaches.
    let mut svc = make_meta(10, base);
    svc.is_service = true;
    assert!(!compute_is_attach_to_previous(&svc, &cur, false));

    // Different forum topics never attach.
    let mut t1 = make_meta(10, base);
    t1.topic_root_id = MsgId(100);
    let mut t2 = make_meta(10, base + 10);
    t2.topic_root_id = MsgId(200);
    assert!(!compute_is_attach_to_previous(&t1, &t2, false));

    // Saved Messages with hidden forwarded sender checks hidden_sender_key equality.
    let mut s1 = make_meta(1, base);
    s1.is_saved_or_special_chat = true;
    s1.is_forwarded = true;
    s1.display_from = None;
    s1.hidden_sender_key = Some("Alice".into());
    let mut s2 = s1.clone();
    s2.date = base + 30;
    assert!(compute_is_attach_to_previous(&s1, &s2, false));
    s2.hidden_sender_key = Some("Bob".into());
    assert!(!compute_is_attach_to_previous(&s1, &s2, false));

    // Megagroup anonymous admins attach only if anonymous rank matches.
    let mut mg1 = make_meta(500, base);
    mg1.author_is_megagroup = true;
    mg1.anonymous_rank = "Founder".into();
    let mut mg2 = mg1.clone();
    mg2.date = base + 15;
    assert!(compute_is_attach_to_previous(&mg1, &mg2, false));
    mg2.anonymous_rank = "Mod".into();
    assert!(!compute_is_attach_to_previous(&mg1, &mg2, false));
}

#[test]
fn date_separator_same_day_and_sequence_states() {
    // 2026-06-15 23:55:00 UTC -> 2026-06-15 23:59:00 UTC -> 2026-06-16 00:01:00 UTC
    let t1 = LocalDateTime::new(2026, 6, 15, 23, 55, 0).to_local_epoch_secs() as i32;
    let t2 = LocalDateTime::new(2026, 6, 15, 23, 59, 0).to_local_epoch_secs() as i32;
    let t3 = LocalDateTime::new(2026, 6, 16, 0, 1, 0).to_local_epoch_secs() as i32;

    let m1 = make_meta(42, t1);
    let m2 = make_meta(42, t2);
    let m3 = make_meta(42, t3);

    assert!(is_same_day(m1.date_time.date, m2.date_time.date));
    assert!(!is_same_day(m2.date_time.date, m3.date_time.date));

    let states = compute_sequence_attach_states(&[m1, m2, m3], None);
    // First item displays date, attaches to next (m2).
    assert!(states[0].display_date);
    assert!(!states[0].attached_to_previous);
    assert!(states[0].attached_to_next);
    // Second item is same day, attaches to previous, NOT to m3 because m3 crosses midnight.
    assert!(!states[1].display_date);
    assert!(states[1].attached_to_previous);
    assert!(!states[1].attached_to_next);
    // Third item crosses midnight -> displays date -> cannot attach to m2 even though only 120s apart!
    assert!(states[2].display_date);
    assert!(!states[2].attached_to_previous);
}

#[test]
fn bubble_corner_rounding_flags() {
    // Standalone incoming message -> Tail on bottom-left, Large elsewhere.
    let single_in = count_message_rounding(false, false, false, false);
    assert_eq!(single_in.top_left, BubbleCornerRounding::Large);
    assert_eq!(single_in.top_right, BubbleCornerRounding::Large);
    assert_eq!(single_in.bottom_left, BubbleCornerRounding::Tail);
    assert_eq!(single_in.bottom_right, BubbleCornerRounding::Large);

    // Middle outgoing attached message -> Small on right corners, Large on left.
    let mid_out = count_message_rounding(true, true, true, false);
    assert_eq!(mid_out.top_left, BubbleCornerRounding::Large);
    assert_eq!(mid_out.top_right, BubbleCornerRounding::Small);
    assert_eq!(mid_out.bottom_left, BubbleCornerRounding::Large);
    assert_eq!(mid_out.bottom_right, BubbleCornerRounding::Small);

    // Inline keyboard forces bottom corners to Small.
    let with_kb = count_bubble_rounding(single_in, true);
    assert_eq!(with_kb.bottom_left, BubbleCornerRounding::Small);
    assert_eq!(with_kb.bottom_right, BubbleCornerRounding::Small);
}

// ===========================================================================
// 3. Message, service message, and media sizing
// ===========================================================================

#[test]
fn photo_sticker_and_round_video_sizing() {
    // CountDesiredMediaSize clamps 2000x1000 into 430x430 -> 430x215.
    let desired = count_desired_media_size(Size::new(2000, 1000));
    assert_eq!(desired, Size::new(MAX_MEDIA_SIZE, 215));

    // CountPhotoMediaSize clamps tall photo height to newWidth as well.
    let tall = count_photo_media_size(Size::new(200, 400), 300, 430);
    assert_eq!(tall, Size::new(150, 300));

    // Sticker max size = 224x224, animated emoji = 112x112.
    let st_size = count_sticker_optimal_size(&StickerInput {
        kind: StickerKind::Regular,
        dimensions: Size::new(512, 512),
        custom_size: 0,
        option_sticker_size: None,
    });
    assert_eq!(st_size, Size::new(224, 224));
    let emoji_size = count_sticker_optimal_size(&StickerInput {
        kind: StickerKind::AnimatedEmoji,
        dimensions: Size::new(512, 512),
        custom_size: 0,
        option_sticker_size: None,
    });
    assert_eq!(emoji_size, Size::new(112, 112));

    // Round video message caps at maxVideoMessageSize = 240.
    let mut wmax = MSG_MAX_WIDTH;
    let round = count_gif_thumb_size(GifKind::VideoMessage, Size::new(384, 384), &mut wmax, false);
    assert_eq!(
        round,
        Size::new(MAX_VIDEO_MESSAGE_SIZE, MAX_VIDEO_MESSAGE_SIZE)
    );
}

#[test]
fn message_and_service_layout_basics() {
    let fonts = FontMetrics::default();
    let msg = MessageInput {
        is_out: true,
        text_max_width: 180,
        text_min_height: 18,
        text_wrapped_height: 18,
        bottom_info: BottomInfoInput {
            author_edited_date_width: 36,
            out_layout: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let layout = layout_message(&msg, 600, &fonts);
    assert!(layout.has_bubble);
    assert!(layout.has_right_layout);
    assert_eq!(layout.margin_top, MSG_MARGIN.top);
    // Outgoing message in Default mode is right-aligned near viewport right edge.
    assert_eq!(layout.geometry.right(), 600 - MSG_MARGIN.left);
    assert!(layout.geometry.width <= MSG_MAX_WIDTH);
    assert_eq!(layout.bottom_info_display, InfoDisplayType::Default);

    // Attached message uses MSG_MARGIN_TOP_ATTACHED (0px).
    let mut attached_msg = msg.clone();
    attached_msg.attach.attached_to_previous = true;
    attached_msg.attach.bubble_attached_to_previous = true;
    let attached_layout = layout_message(&attached_msg, 600, &fonts);
    assert_eq!(attached_layout.margin_top, MSG_MARGIN_TOP_ATTACHED);
    assert_eq!(
        attached_layout.message_rounding.top_right,
        BubbleCornerRounding::Small
    );

    // Signed author exceeding MAX_SIGNATURE_SIZE (144px) sets author_elided.
    let sig = BottomInfoLayout::compute(
        &BottomInfoInput {
            author_width: 120,
            after_author_width: 40,
            ..Default::default()
        },
        300,
        &fonts,
    );
    assert!(sig.author_elided);
    assert_eq!(sig.optimal_size.width, MAX_SIGNATURE_SIZE);

    // Service message centering.
    let svc = layout_service_message(
        &ServiceInput {
            text_max_width: 100,
            text_min_height: 18,
            ..Default::default()
        },
        600,
        &fonts,
    );
    // Bubble width = 100 + 24 = 124; centered in 600 -> x = (600 - 124) / 2 = 238.
    assert_eq!(svc.text_bubble_rect.width, 124);
    assert_eq!(svc.text_bubble_rect.x, 238);
    assert_eq!(MSG_PADDING.horizontal(), 22);
}

// ===========================================================================
// 4. History list: visible range, hit-testing, scroll anchoring, preload, highlight
// ===========================================================================

fn make_list_item(id: i32, ts: i32, height: i32) -> HistoryListItem {
    let fid = FullMsgId::new(PeerId(1), MsgId(i64::from(id)));
    HistoryListItem::new(
        fid,
        MessagePosition {
            full_id: fid,
            date: ts,
        },
        make_meta(1, ts),
        height,
    )
}

#[test]
fn history_list_visible_range_and_hit_testing() {
    let mut list = HistoryList::new();
    // 10 items, each 100px tall -> total items_height = 1000px.
    let items: Vec<_> = (0..10)
        .map(|i| make_list_item(i + 1, 1_700_000_000 + i * 60, 100))
        .collect();
    list.set_items(items);
    assert_eq!(list.items_height(), 1000);

    // Viewport [250, 550) should intersect items 2 (200..300), 3 (300..400), 4 (400..500), 5 (500..600).
    let vis = list.visible_items_top_to_bottom(250, 550);
    let indices: Vec<usize> = vis.iter().map(|v| v.index).collect();
    assert_eq!(indices, vec![2, 3, 4, 5]);
    assert_eq!(vis[0].top, 200);
    assert_eq!(vis[0].bottom, 300);

    // Bottom-to-top returns exact reverse.
    let vis_rev = list.visible_items_bottom_to_top(250, 550);
    let rev_indices: Vec<usize> = vis_rev.iter().map(|v| v.index).collect();
    assert_eq!(rev_indices, vec![5, 4, 3, 2]);

    // Hit-testing by Y.
    assert_eq!(list.find_item_index_by_y(0), Some(0));
    assert_eq!(list.find_item_index_by_y(99), Some(0));
    assert_eq!(list.find_item_index_by_y(100), Some(1));
    assert_eq!(list.find_item_index_by_y(999), Some(9));
    assert_eq!(list.find_item_index_by_y(5000), Some(9)); // clamped
    assert_eq!(list.strict_find_item_by_y(-1), None);
    assert_eq!(list.strict_find_item_by_y(1000), None);
    assert_eq!(list.strict_find_item_by_y(450), Some(4));
}

#[test]
fn history_list_scroll_anchoring_on_prepend() {
    let mut list = HistoryList::new();
    let initial: Vec<_> = (10..20)
        .map(|i| make_list_item(i, 1_700_000_000 + i * 60, 80))
        .collect();
    list.set_items(initial);

    // User is scrolled so visible_top = 260 (item index 3 starts at y=240, shift = +20).
    list.save_scroll_state(260, 660);
    assert_eq!(list.scroll_top_state.shift, 20);
    assert_eq!(list.scroll_top_state.item.full_id.msg, MsgId(13));

    // Prepend 5 older items of height 100 each (+500px above).
    let older: Vec<_> = (5..10)
        .map(|i| make_list_item(i, 1_700_000_000 + i * 60, 100))
        .collect();
    list.prepend_older(older);

    // Restored scroll top must move down by exactly 500px so item 13 stays at the exact same pixel.
    let restored = list.restore_scroll_top();
    assert_eq!(restored, Some(760));
    assert!(!is_message_position_valid(list.scroll_top_state.item));
}

#[test]
fn history_list_preload_triggers_and_highlighter() {
    let mut list = HistoryList::new();
    let items: Vec<_> = (0..40)
        .map(|i| make_list_item(i + 1, 1_700_000_000 + i * 60, 100))
        .collect();
    list.set_items(items);
    list.loaded_at_top = false;
    list.loaded_at_bottom = false;
    list.ids_limit = 40;
    list.around_index = Some(20);

    // Viewport height = 400 -> preload_before = 2 * 400 = 800.
    // Near top (visible_top = 300 < 800) triggers preload_top.
    let near_top = list.check_preload(300, 700);
    assert!(near_top.preload_top);
    assert!(!near_top.preload_bottom);
    assert_eq!(near_top.around_index, Some(3));

    // Middle of list (visible_top = 1800, visible_bottom = 2200) triggers neither.
    let middle = list.check_preload(1800, 2200);
    assert!(!middle.preload_top);
    assert!(!middle.preload_bottom);
    assert_eq!(middle.around_index, None);

    // Near bottom (total = 4008, visible_bottom = 3500 -> remaining 508 < 800) triggers preload_bottom.
    let near_bottom = list.check_preload(3100, 3500);
    assert!(!near_bottom.preload_top);
    assert!(near_bottom.preload_bottom);
    assert_eq!(near_bottom.around_index, Some(35));

    // Jump-to-message highlighter timeline.
    let mut hl = ElementHighlighter::new();
    let target_id = FullMsgId::new(PeerId(1), MsgId(15));
    hl.highlight(HighlightTarget::whole_message(target_id), 1000);
    assert!((hl.state_for(target_id, 1250).opacity - 0.5).abs() < 1e-6);
    assert!((hl.state_for(target_id, 1500).opacity - 1.0).abs() < 1e-6);
    assert!((hl.state_for(target_id, 3000).opacity - 0.5).abs() < 1e-6);
    assert!(hl.is_animating(4499));
    assert!(!hl.is_animating(4500));
    hl.tick(4500);
    assert_eq!(hl.latest_highlighted_msg_id(), MsgId(15));
}

#[test]
fn cross_message_text_selection() {
    let mut sel = HistorySelection::new();
    let id1 = FullMsgId::new(PeerId(1), MsgId(1));
    let id2 = FullMsgId::new(PeerId(1), MsgId(2));
    let id3 = FullMsgId::new(PeerId(1), MsgId(3));

    // Single-message drag selects [from, to) symbols.
    sel.start_drag(ListSelectionEndpoint {
        item_id: id1,
        item_index: 0,
        text: FlatSelectionEndpoint::new(2, false),
    });
    sel.update_drag(ListSelectionEndpoint {
        item_id: id1,
        item_index: 0,
        text: FlatSelectionEndpoint::new(5, true), // offset 6
    });
    assert_eq!(sel.selection_for_item(0, id1), TextSelection::new(2, 6));
    assert_eq!(sel.selection_for_item(1, id2), TextSelection::EMPTY);

    // Dragging across into item 2 transitions to whole-item selection for 0..=2.
    sel.update_drag(ListSelectionEndpoint {
        item_id: id3,
        item_index: 2,
        text: FlatSelectionEndpoint::new(4, false),
    });
    assert_eq!(sel.selection_for_item(0, id1), FULL_SELECTION);
    assert_eq!(sel.selection_for_item(1, id2), FULL_SELECTION);
    assert_eq!(sel.selection_for_item(2, id3), FULL_SELECTION);
    // Partial projection also works across items.
    assert_eq!(
        sel.selection_for_item_partial(0, 10),
        TextSelection::new(2, 10)
    );
    assert_eq!(
        sel.selection_for_item_partial(1, 8),
        TextSelection::new(0, 8)
    );
    assert_eq!(
        sel.selection_for_item_partial(2, 12),
        TextSelection::new(0, 4)
    );
}

// ===========================================================================
// 5. Dialogs row layout & date formatting
// ===========================================================================

#[test]
fn dialogs_date_formatting_rules() {
    // Reference "now": Sunday 2026-11-22 10:00:00
    let now = LocalDateTime::new(2026, 11, 22, 10, 0, 0);

    // Same calendar day -> time format (tdesktop H:mm, no leading zero on hour).
    let same_day = LocalDateTime::new(2026, 11, 22, 8, 5, 0);
    assert_eq!(classify_dialogs_date(same_day, now), DialogsDateKind::Time);
    assert_eq!(format_dialogs_date(same_day, now, true), "8:05");
    assert_eq!(format_dialogs_date(same_day, now, false), "8:05 AM");

    let early_morning = LocalDateTime::new(2026, 11, 22, 3, 7, 0);
    assert_eq!(format_dialogs_date(early_morning, now, true), "3:07");

    // Yesterday late evening (11 hours ago, < 20h) -> STILL time format (exact tdesktop rule!).
    let yesterday_recent = LocalDateTime::new(2026, 11, 21, 23, 0, 0);
    assert_eq!(
        classify_dialogs_date(yesterday_recent, now),
        DialogsDateKind::Time
    );
    assert_eq!(format_dialogs_date(yesterday_recent, now, true), "23:00");

    // Yesterday morning (25 hours ago, >= 20h, 1 day ago < 7) -> Weekday ("Sat").
    let yesterday_old = LocalDateTime::new(2026, 11, 21, 9, 0, 0);
    assert_eq!(
        classify_dialogs_date(yesterday_old, now),
        DialogsDateKind::Weekday
    );
    assert_eq!(format_dialogs_date(yesterday_old, now, true), "Sat");

    // 6 days ago (Monday 2026-11-16) -> Weekday ("Mon").
    let six_days_ago = LocalDateTime::new(2026, 11, 16, 12, 0, 0);
    assert_eq!(
        classify_dialogs_date(six_days_ago, now),
        DialogsDateKind::Weekday
    );
    assert_eq!(format_dialogs_date(six_days_ago, now, true), "Mon");

    // 7 days ago (Sunday 2026-11-15) -> Date ("15.11.26").
    let seven_days_ago = LocalDateTime::new(2026, 11, 15, 12, 0, 0);
    assert_eq!(
        classify_dialogs_date(seven_days_ago, now),
        DialogsDateKind::Date
    );
    assert_eq!(format_dialogs_date(seven_days_ago, now, true), "15.11.26");
}

#[test]
fn dialogs_row_layout_normal_compact_badges_and_elision() {
    let fonts = FontMetrics::default();

    // Normal row with unread counter + mention badge + online dot + date + sent checks.
    let input = DialogRowInput {
        style: DialogRowStyle::DEFAULT,
        mode: DialogRowMode::Normal,
        width: 300,
        chat_type_icon: DialogChatTypeIcon::Group,
        name_text_width: 220,
        date_text_width: 34,
        send_state: DialogSendState::Received,
        badges: DialogBadgesState {
            unread: true,
            unread_counter: 12,
            mention: true,
            ..Default::default()
        },
        unread_counter_text_width: 14,
        display_pinned_icon: true, // hidden while unread badge is shown
        avatar_badge: AvatarCornerBadge::Online,
        ..Default::default()
    };
    let layout = layout_dialog_row(&input, &fonts);
    assert_eq!(layout.row_rect.height, DIALOGS_ROW_HEIGHT);
    assert_eq!(layout.photo_rect, Rect::new(10, 8, 46, 46));
    assert!(layout.avatar_badge_rect.is_some());
    assert!(layout.unread_badge_rect.is_some());
    assert!(layout.mention_badge_rect.is_some());
    assert!(layout.pinned_icon_rect.is_none()); // suppressed by unread badge
    assert!(layout.name_elided);
    // Mention badge sits to the left of unread badge.
    let unread_r = layout.unread_badge_rect.unwrap();
    let mention_r = layout.mention_badge_rect.unwrap();
    assert!(mention_r.right() < unread_r.left());

    // Typing indicator replaces message preview rect.
    let mut typing_in = input.clone();
    typing_in.is_typing = true;
    typing_in.badges = DialogBadgesState::default();
    let typing_layout = layout_dialog_row(&typing_in, &fonts);
    assert!(typing_layout.typing_rect.is_some());
    assert!(typing_layout.message_rect.is_none());
    assert!(typing_layout.pinned_icon_rect.is_some()); // visible when badges are empty

    // Narrow / compact mode: online dot hidden when unread badges sit over avatar.
    let mut narrow_in = input.clone();
    narrow_in.mode = DialogRowMode::Narrow;
    narrow_in.width = 66;
    let narrow_layout = layout_dialog_row(&narrow_in, &fonts);
    assert!(narrow_layout.avatar_badge_rect.is_none());
    assert!(narrow_layout.unread_badge_rect.is_some());
    assert!(narrow_layout.message_rect.is_none());

    // Collapsed bar height = 37px.
    let mut collapsed_in = input;
    collapsed_in.mode = DialogRowMode::Collapsed;
    let collapsed = layout_dialog_row(&collapsed_in, &fonts);
    assert_eq!(collapsed.row_rect.height, DIALOGS_IMPORTANT_BAR_HEIGHT);

    // Unread counter formatting (exact port of ui/unread_counter_format.cpp).
    assert_eq!(format_unread_counter(1500, true, false), "1500");
    assert_eq!(format_unread_counter(1500, true, true), "99+");
    assert_eq!(format_unread_counter(1_500_000, false, true), "99999+");
    assert_eq!(format_unread_counter_short(2500), "2K");

    // Text elision at fixed 8px/char.
    let elided = elide_text_right("Hello Telegram World", 80, |s| {
        (s.chars().count() as i32) * 8
    });
    assert_eq!(elided, "Hello Tel\u{2026}");
}

#[test]
fn all_media_types_sizing_rules() {
    let fonts = FontMetrics::default();

    // Document (plain file, thumbed, voice with waveform + transcribe).
    let file_opt = count_document_optimal_size(&DocumentInput {
        kind: DocumentKind::File,
        name_width: 150,
        max_status_width: 80,
        status_width: 80,
        skip_block_width: 48,
        bottom_info_first_line_width: 48,
        ..Default::default()
    });
    assert_eq!(file_opt.width, 268); // msgFileMinWidth
    assert_eq!(file_opt.height, 8 + 44 + 8); // 60px

    let voice_opt = count_document_optimal_size(&DocumentInput {
        kind: DocumentKind::Voice,
        max_status_width: 40,
        status_width: 40,
        has_transcribe_button: true,
        transcribe_text_height: 36,
        ..Default::default()
    });
    // Waveform 100*(2+1)=300 + pad(22) + thumb(44) + skip(11) + transcribe(28+10) = 415
    assert_eq!(voice_opt.width, 415);
    assert_eq!(voice_opt.height, 60 + 36 + 8);
    assert_eq!(waveform_bar_count(90, 100), 30);

    // WebPage article vs pending.
    assert_eq!(
        count_web_page_optimal_size(
            &WebPageInput {
                pending_or_failed: true,
                ..Default::default()
            },
            &fonts
        ),
        Size::ZERO
    );
    let wp = count_web_page_optimal_size(
        &WebPageInput {
            as_article: true,
            site_name_width: 80,
            title_width: 160,
            title_lines: 1,
            description_width: 200,
            description_height: 36,
            article_photo_size: Size::new(400, 400),
            open_button_width: 90,
            is_bubble_bottom: true,
            ..Default::default()
        },
        &fonts,
    );
    assert!(wp.width > 200);
    assert!(wp.height > 72);

    // Poll with 2 options + bottom button.
    let poll = count_poll_optimal_size(
        &PollInput {
            question_width: 180,
            question_height: 18,
            answers: vec![
                PollAnswerInput {
                    text_width: 100,
                    text_height: 18,
                },
                PollAnswerInput {
                    text_width: 120,
                    text_height: 18,
                },
            ],
            has_bottom_button: true,
            ..Default::default()
        },
        &fonts,
    );
    assert_eq!(poll.width, 268);
    assert!(poll.height > 150);

    // Contact card.
    let contact = count_contact_optimal_size(
        &ContactInput {
            name_width: 110,
            phone_width: 100,
            button_widths: vec![90],
            is_bubble_bottom: true,
            ..Default::default()
        },
        &fonts,
    );
    assert!(contact.width >= 160);
    assert!(contact.height >= 42 + 36);

    // Location default 320x240, venue adds title & description lines.
    let geo = count_location_optimal_size(&LocationInput::default(), &fonts);
    assert_eq!(geo, Size::new(320, 240));
    let venue = count_location_optimal_size(
        &LocationInput {
            has_bubble: true,
            is_bubble_bottom: true,
            title_height: 18,
            description_height: 36,
            ..Default::default()
        },
        &fonts,
    );
    assert_eq!(venue.width, 320);
    assert_eq!(venue.height, 240 + 18 + 36 + 5 + 8);

    // Call card (bubble top vs not bubble top).
    let call_top = count_call_optimal_size(&CallInput {
        text_width: 100,
        status_width: 80,
        is_bubble_top: true,
        ..Default::default()
    });
    assert_eq!(call_top, Size::new(240, 56));
    let call_below = count_call_optimal_size(&CallInput {
        text_width: 100,
        status_width: 80,
        is_bubble_top: false,
        ..Default::default()
    });
    assert_eq!(call_below, Size::new(240, 50));
}

#[test]
fn message_headers_media_overlay_and_service_line_smoothing() {
    let fonts = FontMetrics::default();

    // Message with sender name, forwarded header (wrapping to 2 lines), reply preview,
    // comments button, inline keyboard, and reactions inside bubble.
    let msg = MessageInput {
        is_out: false,
        has_from_photo: true,
        from_name_width: Some(120),
        forwarded_width: Some(500), // forces 2 lines inside 408px inner width
        reply: Some(ReplyInput {
            name_width: 90,
            text_width: 150,
            has_preview: true,
            ..Default::default()
        }),
        text_max_width: 240,
        text_min_height: 18,
        text_wrapped_height: 36,
        bottom_info: BottomInfoInput {
            author_edited_date_width: 36,
            views_text_width: 24,
            pinned: true,
            ..Default::default()
        },
        reactions_size: Size::new(80, 24),
        reactions_in_bubble: true,
        has_comments_button: true,
        comments_text_width: 70,
        inline_keyboard: Some(Size::new(260, 74)),
        ..Default::default()
    };
    let layout = layout_message(&msg, 640, &fonts);
    assert_eq!(layout.header_heights.from_name_or_via, 18);
    assert_eq!(layout.header_heights.forwarded, 36);
    assert!(layout.header_heights.reply >= 40);
    assert_eq!(layout.comments_rect.height, 40);
    assert_eq!(layout.keyboard_rect.height, 74);
    assert_eq!(layout.reactions_rect.height, 24);
    assert_eq!(
        layout.bubble_rounding.bottom_left,
        BubbleCornerRounding::Small
    );
    assert_eq!(
        layout.bubble_rounding.bottom_right,
        BubbleCornerRounding::Small
    );

    // Unwrapped sticker displays bottom info with InfoDisplayType::Background.
    let sticker_msg = MessageInput {
        media: Some(MediaInput::Sticker(StickerInput {
            kind: StickerKind::Regular,
            dimensions: Size::new(512, 512),
            custom_size: 0,
            option_sticker_size: None,
        })),
        bottom_info: BottomInfoInput {
            author_edited_date_width: 36,
            ..Default::default()
        },
        ..Default::default()
    };
    let sticker_layout = layout_message(&sticker_msg, 600, &fonts);
    assert!(!sticker_layout.has_bubble);
    assert!(sticker_layout.unwrapped);
    assert_eq!(
        sticker_layout.bottom_info_display,
        InfoDisplayType::Background
    );
    assert_eq!(sticker_layout.media_rect.size(), Size::new(224, 224));

    // Photo without caption uses InfoDisplayType::Image overlay.
    let photo_msg = MessageInput {
        media: Some(MediaInput::Photo(PhotoInput {
            dimensions: Size::new(800, 600),
            ..Default::default()
        })),
        bottom_info: BottomInfoInput {
            author_edited_date_width: 36,
            ..Default::default()
        },
        ..Default::default()
    };
    let photo_layout = layout_message(&photo_msg, 600, &fonts);
    assert_eq!(photo_layout.bottom_info_display, InfoDisplayType::Image);

    // Service message line width smoothing: lines differing by less than minDelta equalize.
    let smoothed = count_service_line_widths(&[200, 195, 100], &fonts);
    assert_eq!(smoothed[0], 200);
    assert_eq!(smoothed[1], 200);
    assert_eq!(smoothed[2], 100);

    // Wide chat date centering clamps to wide_chat_width() = 542.
    assert_eq!(wide_chat_width(), 542);
    let date_normal = count_service_date_rect(80, 0, 1000, false, &fonts);
    let date_wide = count_service_date_rect(80, 0, 1000, true, &fonts);
    assert!(date_normal.x > date_wide.x);
}

#[test]
fn history_list_sticky_userpics_collapse_gaps_and_jump_to_unread() {
    let mut list = HistoryList::new();
    let base = 1_700_000_000;
    let mut items: Vec<_> = (0..4)
        .map(|i| {
            let mut it = make_list_item(i + 1, base + i * 30, 100);
            it.has_from_photo = true;
            it
        })
        .collect();
    items[2].has_unread_bar = true;
    list.set_items(items);
    list.set_unread_bar(Some(2));

    // Items 0 & 1 attach; unread bar at index 2 breaks attachment; items 2 & 3 attach.
    assert!(list.items()[0].attach_state.attached_to_next);
    assert!(!list.items()[1].attach_state.attached_to_next);
    assert!(!list.items()[2].attach_state.attached_to_previous);
    assert!(list.items()[2].attach_state.attached_to_next);

    // Jump to UnreadMessagePosition targets item 2's top + unread bar margin_top (9px).
    let unread_scroll = list.scroll_top_for_position(MessagePosition::UNREAD, 0, 300);
    assert_eq!(unread_scroll, Some(200 + unread_bar_margin_top()));

    // Sticky userpics: when viewport cuts through attached group (0..=1) at bottom=150,
    // item 1 gets a sticky userpic clamped above viewport bottom.
    let pics = list.enumerate_userpics(0, 150);
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].item_index, 1);

    // Collapse gap shifts visual positions below abs_y.
    list.set_collapse_gaps(vec![CollapseGap {
        abs_y: 200,
        height: 50,
    }]);
    let vis = list.visible_items_top_to_bottom(240, 360);
    assert_eq!(vis.len(), 2);
    assert_eq!(vis[0].index, 2);
    assert_eq!(vis[0].top, 250);
    assert_eq!(vis[1].index, 3);
    assert_eq!(vis[1].top, 350);

    // Highlighter queue & quote-part timeline.
    let mut hl = ElementHighlighter::new();
    let id_a = FullMsgId::new(PeerId(1), MsgId(1));
    let id_b = FullMsgId::new(PeerId(1), MsgId(2));
    hl.enqueue(
        HighlightTarget {
            item_id: id_a,
            range: TextSelection::new(0, 5),
            poll_option: Vec::new(),
        },
        0,
    );
    hl.enqueue(HighlightTarget::whole_message(id_b), 100);
    // At t=1100ms, quote highlight is halfway through its 200ms collapse (1000..1200ms).
    let st_a = hl.state_for(id_a, 1100);
    assert!((st_a.opacity - 1.0).abs() < 1e-6);
    assert!((st_a.collapsion - 0.5).abs() < 1e-6);
    // Total duration for text part = 3*500 + 200 + 3000 = 4700ms -> advances queue to id_b.
    hl.tick(4700);
    assert!(hl.is_animating(4800));
    assert!(hl.state_for(id_b, 4950).opacity > 0.0);
}
