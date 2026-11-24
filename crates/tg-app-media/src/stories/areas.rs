//! Interactive story media areas renderer at rotated coordinates.
//!
//! Port origin: `Telegram/SourceFiles/media/stories/media_stories_reactions.cpp`
//! (`ReactionView`, `WeatherView`, `ClipToContentRect`) and
//! `Telegram/SourceFiles/media/stories/media_stories_controller.cpp` (`rebuildActiveAreas`, `lookupAreaHandler`).

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use tg_data::ReactionId;
use tg_stories::{
    ChannelPost, Story, StoryArea, StoryLocation, SuggestedReaction, UrlArea, WeatherArea,
    rotate_point,
};

use crate::stories::layout::{
    argb_u32_to_color32, choose_weather_fg, format_compact_count, format_temperature,
    project_area_rect, rotated_rounded_rect_points,
};

/// Renders all interactive story areas (`locations`, `url_areas`, `weather_areas`,
/// `suggested_reactions`, `channel_posts`) clipped to `content_rect` at their rotated coordinates.
pub fn paint_story_areas(
    painter: &Painter,
    story: &Story,
    content_rect: Rect,
    weather_in_celsius: bool,
    hovered_point_norm: Option<(f64, f64)>,
) {
    let clipped = painter.with_clip_rect(content_rect);

    for post in &story.channel_posts {
        let hovered =
            hovered_point_norm.is_some_and(|(nx, ny)| post.area.hit_test_normalized(nx, ny));
        paint_channel_post_area(&clipped, post, content_rect, hovered);
    }

    for loc in &story.locations {
        let hovered =
            hovered_point_norm.is_some_and(|(nx, ny)| loc.area.hit_test_normalized(nx, ny));
        paint_location_area(&clipped, loc, content_rect, hovered);
    }

    for url in &story.url_areas {
        let hovered =
            hovered_point_norm.is_some_and(|(nx, ny)| url.area.hit_test_normalized(nx, ny));
        paint_url_area(&clipped, url, content_rect, hovered);
    }

    for weather in &story.weather_areas {
        let hovered =
            hovered_point_norm.is_some_and(|(nx, ny)| weather.area.hit_test_normalized(nx, ny));
        paint_weather_area(&clipped, weather, content_rect, weather_in_celsius, hovered);
    }

    for reaction in &story.suggested_reactions {
        let hovered =
            hovered_point_norm.is_some_and(|(nx, ny)| reaction.area.hit_test_normalized(nx, ny));
        let chosen = story.sent_reaction.as_ref() == Some(&reaction.reaction);
        paint_suggested_reaction_area(&clipped, reaction, content_rect, chosen, hovered);
    }
}

fn paint_rotated_pill(
    painter: &Painter,
    area: StoryArea,
    content_rect: Rect,
    fill: Color32,
    border: Stroke,
    hovered: bool,
) -> Rect {
    let (mut rect, radius) = project_area_rect(area, content_rect);
    if hovered {
        rect = rect.expand(1.5);
    }
    let pts = rotated_rounded_rect_points(rect, radius, area.rotation, 5);
    // Soft drop shadow offset slightly downward.
    let shadow_pts: Vec<Pos2> = pts.iter().map(|p| *p + Vec2::new(0.0, 2.5)).collect();
    painter.add(Shape::convex_polygon(
        shadow_pts,
        Color32::from_rgba_unmultiplied(0, 0, 0, 72),
        Stroke::NONE,
    ));
    painter.add(Shape::convex_polygon(pts, fill, border));
    rect
}

fn paint_location_area(painter: &Painter, loc: &StoryLocation, content_rect: Rect, hovered: bool) {
    let fill = if hovered {
        Color32::from_rgba_unmultiplied(255, 255, 255, 245)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 226)
    };
    let rect = paint_rotated_pill(
        painter,
        loc.area,
        content_rect,
        fill,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 180)),
        hovered,
    );
    let label = if !loc.title.is_empty() {
        format!("\u{1F4CD} {}", loc.title)
    } else if !loc.city.is_empty() {
        format!("\u{1F4CD} {}", loc.city)
    } else {
        format!("\u{1F4CD} {:.3}, {:.3}", loc.point.lat, loc.point.lon)
    };
    let font_size = (rect.height() * 0.42).clamp(10.5, 15.0);
    paint_rotated_text(
        painter,
        rect.center(),
        loc.area.rotation,
        &label,
        FontId::proportional(font_size),
        Color32::from_rgb(0x1D, 0x29, 0x39),
    );
}

fn paint_url_area(painter: &Painter, url: &UrlArea, content_rect: Rect, hovered: bool) {
    let is_star_gift = url.url.starts_with("tg://nft");
    let fill = if is_star_gift {
        Color32::from_rgba_unmultiplied(0x5B, 0x2C, 0x83, if hovered { 240 } else { 218 })
    } else {
        Color32::from_rgba_unmultiplied(0x18, 0x24, 0x33, if hovered { 232 } else { 202 })
    };
    let rect = paint_rotated_pill(
        painter,
        url.area,
        content_rect,
        fill,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(110, 198, 255, 170)),
        hovered,
    );
    let short_url = format_url_badge(&url.url);
    let font_size = (rect.height() * 0.42).clamp(10.0, 14.0);
    paint_rotated_text(
        painter,
        rect.center(),
        url.area.rotation,
        &short_url,
        FontId::proportional(font_size),
        Color32::from_rgb(0x90, 0xD8, 0xFF),
    );
}

fn paint_weather_area(
    painter: &Painter,
    weather: &WeatherArea,
    content_rect: Rect,
    celsius: bool,
    hovered: bool,
) {
    let bg = argb_u32_to_color32(weather.color);
    let fg = choose_weather_fg(bg);
    let rect = paint_rotated_pill(
        painter,
        weather.area,
        content_rect,
        bg,
        Stroke::new(
            if hovered { 1.5 } else { 1.0 },
            Color32::from_rgba_unmultiplied(255, 255, 255, 120),
        ),
        hovered,
    );
    let temp = format_temperature(weather.millicelsius, celsius);
    let text = if weather.emoji.is_empty() {
        temp
    } else {
        format!("{} {temp}", weather.emoji)
    };
    let font_size = (rect.height() * 0.48).clamp(11.0, 16.0);
    paint_rotated_text(
        painter,
        rect.center(),
        weather.area.rotation,
        &text,
        FontId::proportional(font_size),
        fg,
    );
}

fn paint_suggested_reaction_area(
    painter: &Painter,
    suggested: &SuggestedReaction,
    content_rect: Rect,
    chosen: bool,
    hovered: bool,
) {
    let (mut rect, _radius) = project_area_rect(suggested.area, content_rect);
    if hovered {
        rect = rect.expand(2.0);
    }
    let center = rect.center();
    let bubble_r = (rect.width().min(rect.height()) * 0.46).max(16.0);

    let (fill, fg, stroke_color) = if chosen {
        (
            Color32::from_rgba_unmultiplied(0x3B, 0xA8, 0xF5, 240),
            Color32::WHITE,
            Color32::WHITE,
        )
    } else if suggested.dark {
        (
            Color32::from_rgba_unmultiplied(0x1E, 0x24, 0x2B, 224),
            Color32::WHITE,
            Color32::from_rgba_unmultiplied(255, 255, 255, 64),
        )
    } else {
        (
            Color32::from_rgba_unmultiplied(255, 255, 255, 236),
            Color32::from_rgb(0x18, 0x20, 0x2A),
            Color32::from_rgba_unmultiplied(0, 0, 0, 45),
        )
    };

    // Draw thought-bubble tail circles matching `kSuggestedTailBigOffset` (0.464) &
    // `kSuggestedTailSmallOffset` (0.697) in `media_stories_reactions.cpp:56-61`.
    let dir = if suggested.flipped { -1.0_f64 } else { 1.0_f64 };
    let ox = f64::from(center.x);
    let oy = f64::from(center.y);
    let big_raw = (
        ox + dir * f64::from(bubble_r) * 0.78,
        oy + f64::from(bubble_r) * 0.72,
    );
    let small_raw = (
        ox + dir * f64::from(bubble_r) * 1.12,
        oy + f64::from(bubble_r) * 1.02,
    );
    let (bx, by) = rotate_point(big_raw.0, big_raw.1, ox, oy, suggested.area.rotation);
    let (sx, sy) = rotate_point(small_raw.0, small_raw.1, ox, oy, suggested.area.rotation);

    painter.circle_filled(
        Pos2::new(center.x, center.y + 2.5),
        bubble_r,
        Color32::from_rgba_unmultiplied(0, 0, 0, 65),
    );
    painter.circle_filled(Pos2::new(bx as f32, by as f32), bubble_r * 0.264, fill);
    painter.circle_filled(Pos2::new(sx as f32, sy as f32), bubble_r * 0.110, fill);
    painter.circle(center, bubble_r, fill, Stroke::new(1.2, stroke_color));

    let emoji_label = reaction_display_text(&suggested.reaction);
    let has_count = suggested.count > 0;
    let emoji_y = if has_count {
        center.y - bubble_r * 0.18
    } else {
        center.y
    };
    let emoji_size = (bubble_r * if has_count { 0.78 } else { 0.95 }).clamp(14.0, 28.0);
    painter.text(
        Pos2::new(center.x, emoji_y),
        Align2::CENTER_CENTER,
        emoji_label,
        FontId::proportional(emoji_size),
        fg,
    );
    if has_count {
        let count_text = format_compact_count(suggested.count);
        painter.text(
            Pos2::new(center.x, center.y + bubble_r * 0.46),
            Align2::CENTER_CENTER,
            count_text,
            FontId::proportional((bubble_r * 0.36).clamp(9.5, 13.0)),
            fg,
        );
    }
}

fn paint_channel_post_area(
    painter: &Painter,
    post: &ChannelPost,
    content_rect: Rect,
    hovered: bool,
) {
    let fill = Color32::from_rgba_unmultiplied(0x16, 0x1F, 0x2B, if hovered { 220 } else { 188 });
    let rect = paint_rotated_pill(
        painter,
        post.area,
        content_rect,
        fill,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 90)),
        hovered,
    );
    let label = format!("Channel Post #{}", post.item_id.msg.0);
    paint_rotated_text(
        painter,
        rect.center(),
        post.area.rotation,
        &label,
        FontId::proportional((rect.height() * 0.30).clamp(11.0, 14.0)),
        Color32::WHITE,
    );
}

fn paint_rotated_text(
    painter: &Painter,
    center: Pos2,
    angle_deg: f64,
    text: &str,
    font: FontId,
    color: Color32,
) {
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size();
    let angle_rad = (angle_deg as f32).to_radians();
    // Rotate the top-left anchor around `center` so the rotated TextShape stays centered.
    let half = size * 0.5;
    let (sin_a, cos_a) = angle_rad.sin_cos();
    let rx = cos_a * (-half.x) - sin_a * (-half.y);
    let ry = sin_a * (-half.x) + cos_a * (-half.y);
    let anchor = Pos2::new(center.x + rx, center.y + ry);
    let mut shape = egui::epaint::TextShape::new(anchor, galley, color);
    shape.angle = angle_rad;
    painter.add(Shape::Text(shape));
}

/// Formats a URL area into a clean compact pill label.
#[must_use]
pub fn format_url_badge(url: &str) -> String {
    if let Some(slug) = url.strip_prefix("tg://nft?slug=") {
        return format!("\u{2B50} Gift: {slug}");
    }
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let short = if stripped.chars().count() > 28 {
        let head: String = stripped.chars().take(27).collect();
        format!("{head}…")
    } else {
        stripped.to_owned()
    };
    format!("\u{1F517} {short}")
}

/// Returns a human-readable glyph/label for a [`ReactionId`].
#[must_use]
pub fn reaction_display_text(reaction: &ReactionId) -> String {
    match reaction {
        ReactionId::Emoji(e) => e.clone(),
        ReactionId::Custom(id) => format!("#{id}"),
        ReactionId::Paid => "\u{2B50}".to_owned(),
    }
}
