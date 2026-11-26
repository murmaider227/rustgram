//! Star Gift card (`Info::PeerGifts::GiftButton`, `info/peer_gifts/info_peer_gifts_common.cpp`)
//! shared by the "Send a Gift" box and the profile gifts list.
//!
//! A card is a white rounded rectangle (`giftBoxGiftRadius`) with a soft shadow on the
//! `boxDividerBg` box background, the gift's own animated sticker (`starGift.sticker`, played
//! once and then resting on its first frame), an optional rotated corner ribbon
//! (`ValidateRotatedBadge`: "limited", "sold out", "1 of N", ...) and a price pill
//! (`creditsBg3` at 12% with `creditsFg` text and the Stars icon).

use std::collections::HashMap;
use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::{Arc, Mutex};

use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Response, Sense, Shape, Stroke,
    TextureHandle, Ui, Vec2, WidgetInfo,
};
use tg_app_core::chat_helpers::backend::StickerMedia;
use tg_app_core::payments::backend::ReceivedGiftItem;
use tg_app_core::payments::gift_stickers::gift_sticker_media;
use tg_ui::custom_emoji::{CustomEmojiAsset, CustomEmojiPaintContext, CustomEmojiRenderer};
use tg_ui::fonts::{family_semibold, font_id_from_style};
use tg_ui::tg_style::st;

/// `boxWideWidth`.
pub const BOX_WIDE_WIDTH: f32 = 364.0;
/// `giftBoxPadding` (left, top, right, bottom).
pub const GIFT_BOX_PADDING: [f32; 4] = [11.0, 4.0, 11.0, 24.0];
/// `giftBoxGiftSkip`.
pub const GIFT_BOX_GIFT_SKIP: Vec2 = Vec2::new(10.0, 8.0);
/// `giftBoxGiftHeight`.
pub const GIFT_BOX_GIFT_HEIGHT: f32 = 164.0;
/// `giftBoxGiftSmall`.
pub const GIFT_BOX_GIFT_SMALL: f32 = 108.0;
/// `giftBoxGiftRadius`.
pub const GIFT_BOX_GIFT_RADIUS: f32 = 12.0;
/// `giftBoxStickerSize`.
pub const GIFT_BOX_STICKER_SIZE: f32 = 80.0;
/// `giftBoxStickerStarTop`.
const STICKER_STAR_TOP: f32 = 24.0;
/// `giftBoxStickerUniqueTop`.
const STICKER_UNIQUE_TOP: f32 = 32.0;
/// `giftBoxSmallStickerTop`.
const STICKER_SMALL_TOP: f32 = 16.0;
/// `giftBoxButtonBottom`.
const BUTTON_BOTTOM: f32 = 12.0;
/// `giftBoxButtonBottomSmall`.
const BUTTON_BOTTOM_SMALL: f32 = 4.0;
/// `giftBoxButtonPadding` (horizontal, vertical).
const BUTTON_PADDING: Vec2 = Vec2::new(8.0, 4.0);
/// Gifts per row in the gift box grid (`kGiftsPerRow`).
pub const GIFTS_PER_ROW: usize = 3;

fn day_palette() -> &'static tg_ui::theme::DayPalette {
    static PALETTE: std::sync::OnceLock<tg_ui::theme::DayPalette> = std::sync::OnceLock::new();
    PALETTE.get_or_init(tg_ui::theme::DayPalette::new)
}

/// Card content for a gift on a profile (`GiftButtonMode::Minimal`): small card, sender
/// userpic, no price, and a "1 of N" / "#number" ribbon for limited and unique gifts.
#[must_use]
pub fn profile_gift_card_spec(item: &ReceivedGiftItem) -> GiftCardSpec {
    let info = &item.saved.info;
    let unique = info.unique.as_ref();
    let count = info.limited_count;
    let badge = (count > 0 || item.saved.pinned || unique.is_some()).then(|| {
        if let Some(u) = unique {
            GiftBadge {
                text: format!("#{}", format_gift_price(i64::from(u.number))),
                bg: rgb(u.backdrop.edge_color),
                fg: Color32::WHITE,
            }
        } else {
            let text = if count == 1 {
                tg_lang::tr::lng_gift_limited_of_one()
            } else {
                let amount = if count % 1000 != 0 && count < 10_000 {
                    format_gift_price(i64::from(count))
                } else {
                    format!("{}K", count / 1000)
                };
                tg_lang::tr::lng_gift_limited_of_count(&amount)
            };
            GiftBadge {
                text,
                bg: Color32::from_rgb(0x16, 0x8a, 0xcd),
                fg: Color32::WHITE,
            }
        }
    });
    let userpic = unique.is_none().then(|| GiftUserpic {
        peer_id: if item.saved.anonymous {
            0
        } else {
            item.saved.from_id.0.unsigned_abs()
        },
        name: item.sender_name.clone(),
    });
    GiftCardSpec {
        sticker_document_id: info.sticker_document_id.0,
        price: GiftPrice::None,
        badge,
        backdrop: unique.map(|u| GiftBackdrop {
            center: rgb(u.backdrop.center_color),
            edge: rgb(u.backdrop.edge_color),
        }),
        premium_border: false,
        small: true,
        userpic,
        hidden: item.saved.hidden,
        label: if item.title.trim().is_empty() {
            format!("{} ⭐", info.stars)
        } else {
            item.title.clone()
        },
    }
}

fn rgb(c: tg_payments::star_gifts::RgbColor) -> Color32 {
    crate::payments::ui_helpers::rgb_to_color32(c)
}

/// Day-palette colors used by gift cards that the stand-in [`Palette`] does not carry.
pub mod colors {
    use egui::Color32;

    /// `creditsBg1`.
    pub const CREDITS_BG1: Color32 = Color32::from_rgb(0xff, 0xb2, 0x22);
    /// `creditsBg2`.
    pub const CREDITS_BG2: Color32 = Color32::from_rgb(0xff, 0xd9, 0x51);
    /// `creditsBg3`.
    pub const CREDITS_BG3: Color32 = Color32::from_rgb(0xf0, 0xb4, 0x00);
    /// `creditsFg`.
    pub const CREDITS_FG: Color32 = Color32::from_rgb(0xba, 0x70, 0x00);
    /// `boxTextFgGood`.
    pub const BOX_TEXT_FG_GOOD: Color32 = Color32::from_rgb(0x4a, 0xb4, 0x4a);
    /// `attentionButtonFg`.
    pub const ATTENTION_BUTTON_FG: Color32 = Color32::from_rgb(0xd1, 0x4e, 0x4e);
    /// `premiumButtonBg1`.
    pub const PREMIUM_BUTTON_BG1: Color32 = Color32::from_rgb(0x55, 0xa5, 0xff);
    /// `premiumButtonBg2`.
    pub const PREMIUM_BUTTON_BG2: Color32 = Color32::from_rgb(0xa7, 0x67, 0xff);
    /// `premiumButtonBg3`.
    pub const PREMIUM_BUTTON_BG3: Color32 = Color32::from_rgb(0xdb, 0x5c, 0x9d);
    /// `premiumIconBg1`.
    pub const PREMIUM_ICON_BG1: Color32 = Color32::from_rgb(0xf3, 0x89, 0x26);
    /// `premiumIconBg2`.
    pub const PREMIUM_ICON_BG2: Color32 = Color32::from_rgb(0xe4, 0x44, 0x56);
    /// `premiumIconBg3`.
    pub const PREMIUM_ICON_BG3: Color32 = Color32::from_rgb(0x4a, 0xcd, 0x43);
}

/// Size of one gift card in the gift box grid (`Delegate::buttonSize`).
#[must_use]
pub fn gift_box_card_size(small: bool) -> Vec2 {
    let available = BOX_WIDE_WIDTH - GIFT_BOX_PADDING[0] - GIFT_BOX_PADDING[2];
    let width = ((available - 2.0 * GIFT_BOX_GIFT_SKIP.x) / GIFTS_PER_ROW as f32).floor();
    Vec2::new(
        width,
        if small {
            GIFT_BOX_GIFT_SMALL
        } else {
            GIFT_BOX_GIFT_HEIGHT
        },
    )
}

/// Rects of `count` cards laid out like `MakeGiftsList`: `GIFTS_PER_ROW` per row, centered in
/// `width`, starting at `top`.
#[must_use]
pub fn gift_grid_rects(left: f32, top: f32, width: f32, count: usize, small: bool) -> Vec<Rect> {
    let single = gift_box_card_size(small);
    let row_w =
        single.x * GIFTS_PER_ROW as f32 + GIFT_BOX_GIFT_SKIP.x * (GIFTS_PER_ROW as f32 - 1.0);
    let x0 = left + ((width - row_w) / 2.0).max(0.0).floor();
    (0..count)
        .map(|i| {
            let col = (i % GIFTS_PER_ROW) as f32;
            let row = (i / GIFTS_PER_ROW) as f32;
            Rect::from_min_size(
                Pos2::new(
                    x0 + col * (single.x + GIFT_BOX_GIFT_SKIP.x),
                    top + row * (single.y + GIFT_BOX_GIFT_SKIP.y),
                ),
                single,
            )
        })
        .collect()
}

/// Height of a grid with `count` cards (without outer padding).
#[must_use]
pub fn gift_grid_height(count: usize, small: bool) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let rows = count.div_ceil(GIFTS_PER_ROW) as f32;
    let single = gift_box_card_size(small);
    rows * single.y + (rows - 1.0) * GIFT_BOX_GIFT_SKIP.y
}

/// Corner ribbon (`GiftBadge`).
#[derive(Debug, Clone, PartialEq)]
pub struct GiftBadge {
    /// Ribbon text (`lng_gift_stars_limited`, `lng_gift_stars_sold_out`, ...).
    pub text: String,
    /// Ribbon fill.
    pub bg: Color32,
    /// Ribbon text color.
    pub fg: Color32,
}

/// Price pill content.
#[derive(Debug, Clone, PartialEq)]
pub enum GiftPrice {
    /// No pill (small profile cards of non-resale gifts).
    None,
    /// Stars icon (`_delegate->star()`) followed by the formatted amount (e.g. `"1,200+"`).
    Stars(String),
    /// Plain text pill (e.g. a unique gift's transfer label).
    Text(String),
}

/// Unique collectible card background (`UniqueGiftBackdrop` center/edge colors).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GiftBackdrop {
    /// Radial gradient center color.
    pub center: Color32,
    /// Radial gradient edge color.
    pub edge: Color32,
}

/// Sender shown on a received gift card (`MakeUserpicThumbnail` / hidden author).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiftUserpic {
    /// Sender peer id (`0` for an anonymous sender).
    pub peer_id: u64,
    /// Sender display name.
    pub name: String,
}

/// Everything a card needs to paint (`GiftDescriptor` reduced to what is drawn).
#[derive(Debug, Clone, PartialEq)]
pub struct GiftCardSpec {
    /// `starGift.sticker` (or the unique model sticker) document id; `0` when unknown.
    pub sticker_document_id: u64,
    /// Price pill.
    pub price: GiftPrice,
    /// Corner ribbon.
    pub badge: Option<GiftBadge>,
    /// Unique collectible background.
    pub backdrop: Option<GiftBackdrop>,
    /// `requirePremium` / auction gifts get a `creditsFg` border.
    pub premium_border: bool,
    /// Small card (`giftBoxGiftSmall`, profile list).
    pub small: bool,
    /// Sender userpic in the top-left corner (`giftBoxUserpicSize`), profile list only.
    pub userpic: Option<GiftUserpic>,
    /// Hidden from the profile (`StickerPremiumMark` hidden mark over the sticker).
    pub hidden: bool,
    /// Accessibility label (never painted).
    pub label: String,
}

/// Formats a Stars count like `GiftButton::setDescriptor`'s `format` lambda.
#[must_use]
pub fn format_gift_price(number: i64) -> String {
    if number >= 1_000_000 {
        let value = number as f64 / 1_000_000.0;
        if value >= 100.0 || (value.fract() * 10.0).round() == 0.0 {
            format!("{}M", value.floor())
        } else {
            format!("{:.1}M", (value * 10.0).floor() / 10.0)
        }
    } else {
        let digits = number.max(0).to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3);
        for (i, ch) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(ch);
        }
        out
    }
}

/// Paints one gift card into `rect` and returns its click response.
pub fn gift_card(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    spec: &GiftCardSpec,
    card_bg: Color32,
) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, &spec.label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter_at(rect.expand(4.0));
    let radius = CornerRadius::same(GIFT_BOX_GIFT_RADIUS as u8);

    // Background (`GenerateGiftCardBackground`): `windowShadowFg` at 30%, blurred, shifted
    // down by `radius / 12`, under a `boxBg` rounded rect.
    let shadow = rect.translate(Vec2::new(0.0, GIFT_BOX_GIFT_RADIUS / 12.0));
    for (grow, alpha) in [(2.0, 10u8), (1.0, 18), (0.0, 26)] {
        painter.rect_filled(
            shadow.expand(grow),
            CornerRadius::same((GIFT_BOX_GIFT_RADIUS + grow) as u8),
            Color32::from_black_alpha(alpha),
        );
    }
    painter.rect_filled(rect, radius, card_bg);
    if let Some(backdrop) = spec.backdrop {
        paint_radial_backdrop(&painter, rect, backdrop);
    } else if spec.premium_border {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(2.0, colors::CREDITS_FG),
            egui::StrokeKind::Inside,
        );
    }

    // Sticker.
    let sticker_top = if spec.small {
        STICKER_SMALL_TOP
    } else if spec.backdrop.is_some() {
        STICKER_UNIQUE_TOP
    } else {
        STICKER_STAR_TOP
    };
    let sticker_rect = Rect::from_min_size(
        Pos2::new(
            rect.center().x - GIFT_BOX_STICKER_SIZE / 2.0,
            rect.top() + sticker_top,
        ),
        Vec2::splat(GIFT_BOX_STICKER_SIZE),
    );
    paint_gift_sticker(ui, &painter, sticker_rect, spec.sticker_document_id);
    if spec.hidden {
        // Hidden mark (`giftBoxHiddenMark`): a dark round badge over the sticker center.
        let mark = Rect::from_center_size(sticker_rect.center(), Vec2::splat(28.0));
        painter.circle_filled(mark.center(), 14.0, Color32::from_black_alpha(110));
        tg_ui::icons::paint_style_icon(
            &painter,
            mark,
            st::gift_box_hidden_mark,
            &tg_ui::tg_style::DefaultPaletteResolver,
            Some(Color32::WHITE),
        );
    }

    // Sender userpic (`giftBoxUserpicSize` at `giftBoxUserpicSkip`).
    if let Some(ref userpic) = spec.userpic {
        let r = Rect::from_min_size(rect.min + Vec2::splat(2.0), Vec2::splat(24.0));
        if userpic.peer_id == 0 {
            painter.circle_filled(r.center(), 12.0, Color32::from_gray(0xb5));
        } else {
            tg_ui::userpic::EmptyUserpic::for_peer(userpic.peer_id, &userpic.name, day_palette())
                .paint_circle(&painter, r);
        }
    }

    // Corner ribbon.
    if let Some(ref badge) = spec.badge {
        paint_rotated_badge(ui, rect, badge);
    }

    // Price pill.
    paint_price_pill(ui, &painter, rect, spec);

    resp
}

fn paint_radial_backdrop(painter: &egui::Painter, rect: Rect, backdrop: GiftBackdrop) {
    // Concentric rounded rects approximate `paintUniqueBackgroundGradient`'s radial fill.
    let steps = 12;
    painter.rect_filled(
        rect,
        CornerRadius::same(GIFT_BOX_GIFT_RADIUS as u8),
        backdrop.edge,
    );
    let center = Pos2::new(rect.center().x, rect.top() + rect.height() * 0.4);
    let max_r = rect.width().max(rect.height()) * 0.55;
    for i in (0..steps).rev() {
        let t = i as f32 / steps as f32;
        let color = lerp_color(backdrop.center, backdrop.edge, t);
        painter.with_clip_rect(rect.shrink(1.0)).circle_filled(
            center,
            max_r * (t + 1.0 / steps as f32),
            color,
        );
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// Paints the ribbon like `ValidateRotatedBadge(badge, QMargins(), false)` placed at the
/// card's top-right corner and clipped to the card.
fn paint_rotated_badge(ui: &Ui, card: Rect, badge: &GiftBadge) {
    let font = FontId::new(10.0, family_semibold());
    let galley = ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap(badge.text.clone(), font, badge.fg));
    let twidth = galley.size().x;
    let height = galley.size().y;
    let size = twidth + height * 2.0;
    let skip = (twidth / std::f32::consts::SQRT_2).ceil();
    let origin = Pos2::new(card.right() + 1.0 - size, card.top() - 1.0);
    let text_pos = origin + Vec2::new(size - skip, 0.0);
    let along = Vec2::new(FRAC_1_SQRT_2, FRAC_1_SQRT_2);
    let across = Vec2::new(-FRAC_1_SQRT_2, FRAC_1_SQRT_2);
    let start = text_pos - along * (5.0 * twidth);
    let end = text_pos + along * (7.0 * twidth);
    let painter = ui.painter_at(card);
    painter.add(Shape::convex_polygon(
        vec![start, end, end + across * height, start + across * height],
        badge.bg,
        Stroke::NONE,
    ));
    painter.add(
        egui::epaint::TextShape::new(text_pos, galley, badge.fg)
            .with_angle(std::f32::consts::FRAC_PI_4),
    );
}

fn paint_price_pill(ui: &Ui, painter: &egui::Painter, card: Rect, spec: &GiftCardSpec) {
    let font = font_id_from_style(st::semibold_font);
    let (text, with_star) = match spec.price {
        GiftPrice::None => return,
        GiftPrice::Stars(ref text) => (text.clone(), true),
        GiftPrice::Text(ref text) => (text.clone(), false),
    };
    let unique = spec.backdrop.is_some();
    let text_color = if unique {
        Color32::WHITE
    } else {
        colors::CREDITS_FG
    };
    let galley = ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap(text, font.clone(), text_color));
    let line_h = ui.ctx().fonts_mut(|f| f.row_height(&font));
    let star_w = if with_star { line_h + 2.0 } else { 0.0 };
    let inner_w = star_w + galley.size().x;
    let pill = Rect::from_min_size(
        Pos2::new(
            card.center().x - (inner_w + BUTTON_PADDING.x * 2.0) / 2.0,
            card.bottom()
                - if spec.small {
                    BUTTON_BOTTOM_SMALL
                } else {
                    BUTTON_BOTTOM
                }
                - (line_h + BUTTON_PADDING.y * 2.0),
        ),
        Vec2::new(
            inner_w + BUTTON_PADDING.x * 2.0,
            line_h + BUTTON_PADDING.y * 2.0,
        ),
    );
    let fill = if unique {
        Color32::from_white_alpha(51)
    } else {
        // `creditsBg3` painted at 12% opacity.
        Color32::from_rgba_unmultiplied(
            colors::CREDITS_BG3.r(),
            colors::CREDITS_BG3.g(),
            colors::CREDITS_BG3.b(),
            31,
        )
    };
    painter.rect_filled(pill, CornerRadius::same((pill.height() / 2.0) as u8), fill);
    let mut x = pill.left() + BUTTON_PADDING.x;
    if with_star {
        let star = Rect::from_min_size(
            Pos2::new(x, pill.center().y - line_h / 2.0),
            Vec2::splat(line_h),
        );
        paint_credits_star(ui.ctx(), painter, star.shrink(1.0));
        x += star_w;
    }
    painter.galley(
        Pos2::new(x, pill.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
}

const PREMIUM_STAR_SVG: &str = include_str!("assets/premium_star.svg");

/// Colorizes the Premium star SVG (`icons/settings/star.svg`) with a linear gradient.
fn colorized_star_svg(stops: &[(f32, Color32)]) -> String {
    let stops_xml: String = stops
        .iter()
        .map(|(offset, c)| {
            format!(
                "<stop offset='{offset}' stop-color='#{:02x}{:02x}{:02x}'/>",
                c.r(),
                c.g(),
                c.b()
            )
        })
        .collect();
    let gradient = format!(
        "<linearGradient id='Gradient2' x1='0' x2='1' y1='1' y2='0'>{stops_xml}</linearGradient>"
    );
    // Swap the fill first: the inserted stops may themselves contain `#fff...`.
    PREMIUM_STAR_SVG
        .replace("#fff", "url(#Gradient2)")
        .replace("gradientPlaceholder", &gradient)
}

fn rasterize_svg(svg: &str, px: u32) -> Option<egui::ColorImage> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let scale = (px as f32 / size.width().max(1.0)).min(px as f32 / size.height().max(1.0));
    let dx = (px as f32 - size.width() * scale) * 0.5;
    let dy = (px as f32 - size.height() * scale) * 0.5;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
        &mut pixmap.as_mut(),
    );
    Some(egui::ColorImage::from_rgba_premultiplied(
        [px as usize, px as usize],
        pixmap.data(),
    ))
}

/// Paints the Premium star shape filled with `stops` into `rect` (cached per size and stops).
pub fn paint_gradient_star(
    ctx: &egui::Context,
    painter: &egui::Painter,
    rect: Rect,
    stops: &[(f32, Color32)],
) {
    let px = (rect.width().max(rect.height()) * ctx.pixels_per_point())
        .round()
        .clamp(4.0, 512.0) as u32;
    let key = stops.iter().fold(u64::from(px), |acc, (o, c)| {
        acc.wrapping_mul(1_000_003)
            ^ u64::from(u32::from_le_bytes(c.to_array()))
            ^ u64::from(o.to_bits())
    });
    let id = Id::unique(("tg_gift_card_star", key));
    let texture = match ctx.data(|d| d.get_temp::<Option<TextureHandle>>(id)) {
        Some(cached) => cached,
        None => {
            let rendered = rasterize_svg(&colorized_star_svg(stops), px).map(|image| {
                ctx.load_texture(
                    format!("tg_gift_card_star_{key}"),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            ctx.data_mut(|d| d.insert_temp(id, rendered.clone()));
            rendered
        }
    };
    if let Some(texture) = texture {
        let side = rect.width().min(rect.height());
        painter.image(
            texture.id(),
            Rect::from_center_size(rect.center(), Vec2::splat(side)),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// Paints the gold Stars icon (`CreditsIconGradientStops`).
pub fn paint_credits_star(ctx: &egui::Context, painter: &egui::Painter, rect: Rect) {
    paint_gradient_star(
        ctx,
        painter,
        rect,
        &[(0.0, colors::CREDITS_BG1), (1.0, colors::CREDITS_BG2)],
    );
}

/// Per-context sticker renderer and playback state for gift cards.
#[derive(Default)]
struct GiftStickers {
    renderer: CustomEmojiRenderer,
    /// Document id -> time (seconds) its first frame was shown.
    started: HashMap<u64, f64>,
    /// Document id -> full animation length in seconds (known after the first frame).
    duration: HashMap<u64, f64>,
    /// Documents whose media could not be decoded (never re-parsed every frame).
    failed: std::collections::HashSet<u64>,
}

fn gift_stickers(ctx: &egui::Context) -> Arc<Mutex<GiftStickers>> {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(Id::unique("tg_gift_card_stickers"), || {
            Arc::new(Mutex::new(GiftStickers::default()))
        })
        .clone()
    })
}

fn asset_for(media: StickerMedia) -> CustomEmojiAsset {
    match media {
        StickerMedia::LottieJson(json) => CustomEmojiAsset::lottie(json),
        StickerMedia::LottieTgs(bytes) => CustomEmojiAsset::tgs(bytes),
        StickerMedia::StaticRgba {
            width,
            height,
            rgba,
        } => CustomEmojiAsset::static_rgba(width, height, rgba),
    }
}

/// Paints the gift sticker: plays once from the first time it is shown, then rests on frame 0
/// (`GiftButtonPlayback::Once` -> `Rest`). Nothing is drawn until the document is downloaded.
fn paint_gift_sticker(ui: &Ui, painter: &egui::Painter, rect: Rect, document_id: u64) {
    if document_id == 0 {
        return;
    }
    let ctx = ui.ctx().clone();
    let key = document_id.to_string();
    let state = gift_stickers(&ctx);
    let Ok(mut guard) = state.lock() else {
        return;
    };
    if !guard.renderer.contains(&key) {
        if guard.failed.contains(&document_id) {
            return;
        }
        let Some(media) = gift_sticker_media(document_id) else {
            return;
        };
        let kind = match media {
            StickerMedia::LottieJson(_) => "lottie-json",
            StickerMedia::LottieTgs(_) => "tgs",
            StickerMedia::StaticRgba { .. } => "static",
        };
        if !guard
            .renderer
            .register_asset(key.clone(), &asset_for(media))
        {
            tracing::debug!(document_id, kind, "gift sticker media could not be decoded");
            guard.failed.insert(document_id);
            return;
        }
    }
    let now = ctx.input(|i| i.time);
    let started = *guard.started.entry(document_id).or_insert(now);
    let elapsed = (now - started).max(0.0);
    let done = guard
        .duration
        .get(&document_id)
        .is_some_and(|total| elapsed >= *total);
    let paint_ctx = CustomEmojiPaintContext {
        time_secs: elapsed,
        paused: done,
        force_first_frame: done,
        force_last_frame: false,
        text_color: Color32::WHITE,
    };
    if !guard.duration.contains_key(&document_id) {
        let px = (rect.width() * ctx.pixels_per_point()).round() as u32;
        if let Some(frame) = guard.renderer.frame_for(&key, px, paint_ctx) {
            let total = f64::from(frame.duration_ms) * frame.frame_count as f64 / 1000.0;
            guard.duration.insert(document_id, total.max(0.0));
        }
    }
    guard.renderer.paint(&ctx, painter, &key, rect, paint_ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_size_matches_tdesktop_box_metrics() {
        // (364 - 11 - 11 - 2 * 10) / 3 = 107.33 -> 107 x 164.
        assert_eq!(gift_box_card_size(false), Vec2::new(107.0, 164.0));
        assert_eq!(gift_box_card_size(true), Vec2::new(107.0, 108.0));
    }

    #[test]
    fn grid_places_three_cards_per_row_with_tdesktop_skips() {
        let rects = gift_grid_rects(0.0, 0.0, BOX_WIDE_WIDTH, 5, false);
        assert_eq!(rects.len(), 5);
        assert_eq!(rects[0].min, Pos2::new(11.0, 0.0));
        assert_eq!(rects[1].min.x - rects[0].max.x, GIFT_BOX_GIFT_SKIP.x);
        assert_eq!(rects[3].min.y - rects[0].max.y, GIFT_BOX_GIFT_SKIP.y);
        assert_eq!(rects[3].min.x, rects[0].min.x);
        assert_eq!(gift_grid_height(5, false), 164.0 * 2.0 + 8.0);
        assert_eq!(gift_grid_height(0, false), 0.0);
    }

    #[test]
    fn price_format_matches_count_decimal_and_short_millions() {
        assert_eq!(format_gift_price(15), "15");
        assert_eq!(format_gift_price(2_500), "2,500");
        assert_eq!(format_gift_price(250_000), "250,000");
        assert_eq!(format_gift_price(1_000_000), "1M");
        assert_eq!(format_gift_price(1_500_000), "1.5M");
    }

    #[test]
    fn white_star_svg_keeps_white_stops() {
        let svg = colorized_star_svg(&[(0.0, Color32::WHITE), (1.0, Color32::WHITE)]);
        assert!(svg.contains("stop-color='#ffffff'"), "{svg}");
        assert!(!svg.contains("url(#Gradient2)fff"));
    }
}
