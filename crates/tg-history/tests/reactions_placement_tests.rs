//! TGB-3182: where a message's reactions go (`Message::embedReactionsInBubble` =
//! `usesMessageInfoLayout`, `history_view_message.cpp:5540, 7263-7275`). A bubble that ends with
//! media drawing its own time (a photo without a caption) keeps the reactions below the bubble on
//! the chat background; a text message or a file keeps them inside.

use tg_history::consts::MEDIA_IN_BUBBLE_SKIP;
use tg_history::*;

fn info() -> BottomInfoInput {
    BottomInfoInput {
        author_edited_date_width: 30,
        ..Default::default()
    }
}

/// A photo in a group: the sender name above it, no caption.
fn photo_message() -> MessageInput {
    MessageInput {
        from_name_width: Some(60),
        media: Some(MediaInput::Photo(PhotoInput {
            dimensions: Size::new(640, 960),
            has_bubble: true,
            ..Default::default()
        })),
        bottom_info: info(),
        reaction_button_widths: vec![48],
        reactions_in_bubble: true,
        ..Default::default()
    }
}

#[test]
fn a_photo_without_caption_keeps_its_reactions_below_the_bubble() {
    let fonts = FontMetrics::default();
    let l = layout_message(&photo_message(), 800, &fonts);
    assert!(l.has_bubble);
    assert!(!l.reactions_in_bubble);
    let bubble = l.geometry_without_outside_reactions();
    assert_eq!(
        l.reactions_rect.y,
        bubble.y + bubble.height + MEDIA_IN_BUBBLE_SKIP,
        "the reactions start mediaInBubbleSkip below the bubble"
    );
    assert_eq!(
        bubble.y + bubble.height,
        l.media_rect.y + l.media_rect.height,
        "the bubble ends with the photo"
    );
    assert_eq!(
        l.reaction_buttons[0].x, l.geometry.x,
        "left-aligned with the bubble"
    );
}

#[test]
fn a_photo_with_a_caption_keeps_its_reactions_inside() {
    let fonts = FontMetrics::default();
    let mut msg = photo_message();
    msg.text_max_width = 120;
    msg.text_min_height = 18;
    msg.text_wrapped_height = 18;
    msg.text_real_width = 120;
    let l = layout_message(&msg, 800, &fonts);
    assert!(l.reactions_in_bubble);
    assert_eq!(l.geometry_without_outside_reactions(), l.geometry);
    assert!(l.reactions_rect.y + l.reactions_rect.height < l.geometry.y + l.geometry.height);
}

#[test]
fn text_and_file_messages_keep_their_reactions_inside() {
    let fonts = FontMetrics::default();
    let text = MessageInput {
        text_max_width: 120,
        text_min_height: 18,
        text_wrapped_height: 18,
        text_real_width: 120,
        bottom_info: info(),
        reaction_button_widths: vec![48],
        reactions_in_bubble: true,
        ..Default::default()
    };
    assert!(layout_message(&text, 800, &fonts).reactions_in_bubble);
    let file = MessageInput {
        media: Some(MediaInput::Document(DocumentInput {
            name_width: 120,
            status_width: 60,
            max_status_width: 60,
            ..Default::default()
        })),
        bottom_info: info(),
        reaction_button_widths: vec![48],
        reactions_in_bubble: true,
        ..Default::default()
    };
    assert!(
        layout_message(&file, 800, &fonts).reactions_in_bubble,
        "Document::customInfoLayout is false"
    );
}
