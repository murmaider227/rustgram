//! "Send a Gift" box (`GiftBox` in `boxes/star_gift_box.cpp`): recipient userpic, the Stars
//! gifts block (subtitle, about text, `MakeGiftsPriceTabs` tabs) and the 3-column gift grid
//! (`MakeGiftsList`), on the `boxDividerBg` background with a "Back" box button.

use std::sync::OnceLock;

use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Ui, Vec2, WidgetInfo,
};
use tg_app_core::payments::backend::CatalogGiftItem;
use tg_app_core::stand_in::theme::Palette;
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::theme::DayPalette;
use tg_ui::userpic::EmptyUserpic;

use crate::payments::gift_card::{
    BOX_WIDE_WIDTH, GIFT_BOX_PADDING, GiftBadge, GiftCardSpec, GiftPrice, colors,
    format_gift_price, gift_card, gift_grid_height, gift_grid_rects,
};

/// `defaultVerticalListSkip`.
const LIST_SKIP: f32 = 6.0;
/// `premiumGiftsUserpicButton.photoSize`.
const USERPIC_SIZE: f32 = 66.0;
/// `giftBoxSubtitleMargin` / `giftBoxAboutMargin` / `giftBoxTabsMargin` (vertical).
const BLOCK_MARGIN_V: f32 = 4.0;
/// `giftBoxSubtitleMargin` horizontal.
const SUBTITLE_MARGIN_H: f32 = 22.0;
/// `giftBoxAboutMargin` / `giftBoxTabsMargin` horizontal.
const ABOUT_MARGIN_H: f32 = 20.0;
/// `giftBoxTabPadding`.
const TAB_PADDING: Vec2 = Vec2::new(8.0, 2.0);
/// `giftBoxTabSkip`.
const TAB_SKIP: f32 = 2.0;
/// `windowBgRipple` (`giftBoxTabBgActive`).
const WINDOW_BG_RIPPLE: Color32 = Color32::from_rgb(0xe5, 0xe5, 0xe5);
/// `defaultBox.buttonPadding` + `buttonHeight`: the box button bar height.
const BUTTON_BAR_HEIGHT: f32 = 10.0 + 34.0 + 10.0;

/// Gift box tab (`kPriceTabAll` / `kPriceTabMy` / `kPriceTabCollectibles`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GiftBoxTab {
    /// "All Gifts".
    #[default]
    All,
    /// "My Gifts" (owned unique gifts, never for yourself).
    My,
    /// "Collectibles" (resale listings and upgradable limited gifts).
    Collectibles,
}

impl GiftBoxTab {
    fn label(self) -> String {
        match self {
            Self::All => tr::lng_gift_stars_tabs_all(),
            Self::My => tr::lng_gift_stars_tabs_my(),
            Self::Collectibles => tr::lng_gift_stars_tabs_collectibles(),
        }
    }
}

/// One card in the grid: a catalog gift, or its resale entry (`GiftTypeStars::resale`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GiftBoxEntry {
    /// Index into the catalog.
    pub catalog_index: usize,
    /// Resale marketplace entry for this gift.
    pub resale: bool,
}

/// What the user did in the box this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GiftBoxOutcome {
    /// Clicked card.
    pub clicked: Option<GiftBoxEntry>,
    /// "Back" pressed.
    pub back: bool,
}

/// Gift box view state.
#[derive(Debug, Clone, Default)]
pub struct GiftBoxState {
    /// Selected tab.
    pub tab: GiftBoxTab,
}

fn is_sold_out(item: &CatalogGiftItem) -> bool {
    item.gift.limited_count > 0 && item.gift.limited_left == 0
}

/// Builds the ordered entry list like `GiftsStars`: each gift, followed by its resale entry when
/// `resellCount > 0`, with sold-out non-resale gifts moved to the end (stable).
#[must_use]
pub fn gift_box_all_entries(catalog: &[CatalogGiftItem]) -> Vec<GiftBoxEntry> {
    let mut list = Vec::with_capacity(catalog.len());
    for (catalog_index, item) in catalog.iter().enumerate() {
        list.push(GiftBoxEntry {
            catalog_index,
            resale: false,
        });
        if item.gift.resell_count > 0 {
            list.push(GiftBoxEntry {
                catalog_index,
                resale: true,
            });
        }
    }
    list.sort_by_key(|e| !e.resale && catalog[e.catalog_index].gift.sold_out);
    list
}

/// Tabs shown above the grid (`MakeGiftsPriceTabs`): "All Gifts", then "Collectibles" when any
/// gift is on resale or is a limited upgradable gift. "My Gifts" is never shown for yourself.
#[must_use]
pub fn gift_box_tabs(
    catalog: &[CatalogGiftItem],
    for_self: bool,
    has_my_unique: bool,
) -> Vec<GiftBoxTab> {
    let mut tabs = vec![GiftBoxTab::All];
    if has_my_unique && !for_self && !catalog.is_empty() {
        tabs.push(GiftBoxTab::My);
    }
    let has_collectibles = catalog.iter().any(|item| {
        item.gift.resell_count > 0 || (item.gift.limited_count > 0 && item.gift.upgradable)
    });
    if has_collectibles {
        tabs.push(GiftBoxTab::Collectibles);
    }
    tabs
}

/// Entries shown for `tab` (`MakeStarsGifts` filter).
#[must_use]
pub fn gift_box_entries(catalog: &[CatalogGiftItem], tab: GiftBoxTab) -> Vec<GiftBoxEntry> {
    let all = gift_box_all_entries(catalog);
    match tab {
        GiftBoxTab::My => Vec::new(),
        GiftBoxTab::All => {
            let resale_ids: Vec<u64> = all
                .iter()
                .filter(|e| e.resale)
                .map(|e| catalog[e.catalog_index].gift.id)
                .collect();
            all.into_iter()
                .filter(|e| {
                    let item = &catalog[e.catalog_index];
                    e.resale || !(is_sold_out(item) && resale_ids.contains(&item.gift.id))
                })
                .collect()
        }
        GiftBoxTab::Collectibles => all
            .into_iter()
            .filter(|e| {
                let item = &catalog[e.catalog_index];
                e.resale
                    || (item.gift.limited_count > 0 && item.gift.upgradable && !is_sold_out(item))
            })
            .collect(),
    }
}

/// Card content for a catalog entry (`GiftButton::setDescriptor` + badge in `paint`).
#[must_use]
pub fn gift_box_card_spec(item: &CatalogGiftItem, resale: bool, now_unix: i64) -> GiftCardSpec {
    let gift = &item.gift;
    let sold_out = is_sold_out(item);
    let upcoming_auction =
        gift.auction_start_date > 0 && i64::from(gift.auction_start_date) > now_unix;
    let price = if resale {
        let mut text = format_gift_price(gift.stars_resell_min);
        if gift.resell_count > 1 {
            text.push('+');
        }
        GiftPrice::Stars(text)
    } else if gift.is_auction() {
        GiftPrice::Text(if gift.sold_out || upcoming_auction {
            tr::lng_gift_stars_auction_view()
        } else {
            tr::lng_gift_stars_auction_join()
        })
    } else {
        GiftPrice::Stars(format_gift_price(gift.stars))
    };
    let your_left = if gift.per_user_total > 0 {
        if gift.per_user_remains > 0 {
            tr::lng_gift_stars_your_left(i64::from(gift.per_user_remains))
        } else {
            tr::lng_gift_stars_your_finished()
        }
    } else {
        String::new()
    };
    let badge = (gift.limited_count > 0).then(|| {
        let (text, bg) = if resale {
            (tr::lng_gift_stars_resale(), colors::BOX_TEXT_FG_GOOD)
        } else if sold_out {
            (tr::lng_gift_stars_sold_out(), colors::ATTENTION_BUTTON_FG)
        } else if gift.is_auction() {
            (
                if upcoming_auction {
                    tr::lng_gift_stars_auction_soon()
                } else {
                    tr::lng_gift_stars_auction()
                },
                colors::CREDITS_FG,
            )
        } else if gift.require_premium {
            (tr::lng_gift_stars_premium(), colors::CREDITS_FG)
        } else if your_left.is_empty() {
            (tr::lng_gift_stars_limited(), ACTIVE_TEXT_FG)
        } else {
            (your_left.clone(), ACTIVE_TEXT_FG)
        };
        GiftBadge {
            text,
            bg,
            fg: Color32::WHITE,
        }
    });
    GiftCardSpec {
        sticker_document_id: gift.sticker_document_id.0,
        price,
        badge,
        backdrop: None,
        premium_border: !resale && (gift.require_premium || gift.is_auction()),
        small: false,
        userpic: None,
        hidden: false,
        label: gift_label(item),
    }
}

/// `windowActiveTextFg` (day).
const ACTIVE_TEXT_FG: Color32 = Color32::from_rgb(0x16, 0x8a, 0xcd);

/// Accessibility label of a card: the gift title when the server sends one, else its price.
#[must_use]
pub fn gift_label(item: &CatalogGiftItem) -> String {
    if item.title.trim().is_empty() {
        format!("{} ⭐", item.gift.stars)
    } else {
        item.title.clone()
    }
}

fn day_palette() -> &'static DayPalette {
    static PALETTE: OnceLock<DayPalette> = OnceLock::new();
    PALETTE.get_or_init(DayPalette::new)
}

/// Lays out `**bold**` markup into a centered, wrapped galley.
fn marked_galley(
    ui: &Ui,
    text: &str,
    size: f32,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    for (i, part) in text.split("**").enumerate() {
        if part.is_empty() {
            continue;
        }
        let family = if i % 2 == 1 {
            family_semibold()
        } else {
            family_regular()
        };
        job.append(
            part,
            0.0,
            egui::TextFormat {
                font_id: FontId::new(size, family),
                color,
                ..Default::default()
            },
        );
    }
    job.wrap.max_width = width;
    job.halign = egui::Align::Center;
    ui.ctx().fonts_mut(|f| f.layout_job(job))
}

/// Renders the gift box body into the current `ui` (full width) and returns the outcome.
#[allow(clippy::too_many_arguments)]
pub fn show_gift_box(
    ui: &mut Ui,
    state: &mut GiftBoxState,
    catalog: &[CatalogGiftItem],
    recipient_name: &str,
    recipient_peer_id: u64,
    for_self: bool,
    now_unix: i64,
    palette: &Palette,
) -> GiftBoxOutcome {
    let mut outcome = GiftBoxOutcome::default();
    let full = ui.max_rect();
    let width = full.width();
    let divider_bg = palette.window_bg_over;
    ui.painter()
        .rect_filled(full, CornerRadius::ZERO, divider_bg);

    let tabs = gift_box_tabs(catalog, for_self, false);
    if !tabs.contains(&state.tab) {
        state.tab = GiftBoxTab::All;
    }
    let entries = gift_box_entries(catalog, state.tab);

    let content_rect = Rect::from_min_max(
        full.min,
        Pos2::new(full.right(), full.bottom() - BUTTON_BAR_HEIGHT),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    egui::ScrollArea::vertical()
        .id_salt("tg_gift_box_scroll")
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            let left = ui.max_rect().left();
            let width = ui.available_width();
            let mut y = ui.max_rect().top();

            // Userpic (`premiumGiftsUserpicButton`), then two list skips and the mini stars.
            y += LIST_SKIP * 5.0;
            let userpic = Rect::from_min_size(
                Pos2::new(left + (width - USERPIC_SIZE) / 2.0, y),
                Vec2::splat(USERPIC_SIZE),
            );
            EmptyUserpic::for_peer(recipient_peer_id, recipient_name, day_palette())
                .paint_circle(ui.painter(), userpic);
            y += USERPIC_SIZE + LIST_SKIP * 4.0;

            // Block subtitle (`giftBoxSubtitle`, `boxTitleFont`).
            y += BLOCK_MARGIN_V;
            let subtitle = if for_self {
                tr::lng_gift_self_title()
            } else {
                tr::lng_gift_stars_subtitle()
            };
            let subtitle_galley = ui.ctx().fonts_mut(|f| {
                f.layout(
                    subtitle,
                    FontId::new(16.0, family_semibold()),
                    palette.window_bold_fg,
                    width - 2.0 * SUBTITLE_MARGIN_H,
                )
            });
            ui.painter().galley(
                Pos2::new(left + (width - subtitle_galley.size().x) / 2.0, y),
                subtitle_galley.clone(),
                palette.window_bold_fg,
            );
            y += subtitle_galley.size().y + BLOCK_MARGIN_V;

            // About (`giftBoxAbout`).
            y += BLOCK_MARGIN_V;
            let about = if for_self {
                tr::lng_gift_self_about()
            } else {
                tr::lng_gift_stars_about(
                    &format!("**{recipient_name}**"),
                    &tr::lng_gift_stars_link(),
                )
            };
            let about_galley = marked_galley(
                ui,
                &about,
                13.0,
                palette.window_fg,
                width - 2.0 * ABOUT_MARGIN_H,
            );
            ui.painter().galley(
                Pos2::new(left + width / 2.0, y),
                about_galley.clone(),
                palette.window_fg,
            );
            y += about_galley.size().y + BLOCK_MARGIN_V;

            // Tabs (`MakeGiftsPriceTabs`).
            y += BLOCK_MARGIN_V;
            let font = FontId::new(13.0, family_semibold());
            let galleys: Vec<_> = tabs
                .iter()
                .map(|tab| {
                    let active = *tab == state.tab;
                    let fg = if active {
                        palette.window_bold_fg
                    } else {
                        palette.window_sub_text_fg
                    };
                    (
                        *tab,
                        ui.ctx()
                            .fonts_mut(|f| f.layout_no_wrap(tab.label(), font.clone(), fg)),
                        fg,
                    )
                })
                .collect();
            let tab_h = galleys
                .first()
                .map_or(0.0, |(_, g, _)| g.size().y + TAB_PADDING.y * 2.0);
            let full_w: f32 = galleys
                .iter()
                .map(|(_, g, _)| g.size().x + TAB_PADDING.x * 2.0)
                .sum::<f32>()
                + TAB_SKIP * (galleys.len().saturating_sub(1)) as f32;
            let mut x = left + ((width - full_w) / 2.0).max(ABOUT_MARGIN_H);
            for (tab, galley, fg) in galleys {
                let rect = Rect::from_min_size(
                    Pos2::new(x, y),
                    Vec2::new(galley.size().x + TAB_PADDING.x * 2.0, tab_h),
                );
                let resp = ui.interact(
                    rect,
                    Id::unique(("tg_gift_box_tab", tab as u8)),
                    Sense::click(),
                );
                let label = tab.label();
                resp.widget_info(|| {
                    WidgetInfo::selected(egui::Role::Tab, true, tab == state.tab, &label)
                });
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if tab == state.tab {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same((tab_h / 2.0) as u8),
                        if palette.window_bg.r() < 128 {
                            palette.window_bg_over
                        } else {
                            WINDOW_BG_RIPPLE
                        },
                    );
                }
                ui.painter().galley(rect.min + TAB_PADDING, galley, fg);
                if resp.clicked() {
                    state.tab = tab;
                }
                x = rect.right() + TAB_SKIP;
            }
            y += tab_h + BLOCK_MARGIN_V;

            // Gift grid (`MakeGiftsList`, `giftBoxPadding`).
            y += GIFT_BOX_PADDING[1];
            let grid_left = left + (width - BOX_WIDE_WIDTH).max(0.0) / 2.0;
            let grid_w = width.min(BOX_WIDE_WIDTH);
            let rects = gift_grid_rects(grid_left, y, grid_w, entries.len(), false);
            for (entry, rect) in entries.iter().zip(rects) {
                let Some(item) = catalog.get(entry.catalog_index) else {
                    continue;
                };
                let spec = gift_box_card_spec(item, entry.resale, now_unix);
                let id = Id::unique(("tg_gift_box_card", item.gift.id, entry.resale));
                if gift_card(ui, rect, id, &spec, palette.window_bg).clicked() {
                    outcome.clicked = Some(*entry);
                }
            }
            y += gift_grid_height(entries.len(), false) + GIFT_BOX_PADDING[3];

            let used = y - ui.max_rect().top();
            ui.allocate_space(Vec2::new(width, used.max(0.0)));
        });

    // Box button bar: "Back" (`lng_create_group_back`), right-aligned light button.
    let bar = Rect::from_min_max(
        Pos2::new(full.left(), full.bottom() - BUTTON_BAR_HEIGHT),
        full.max,
    );
    ui.painter()
        .rect_filled(bar, CornerRadius::ZERO, palette.window_bg);
    let label = tr::lng_create_group_back();
    let galley = ui.ctx().fonts_mut(|f| {
        f.layout_no_wrap(
            label.clone(),
            FontId::new(14.0, family_semibold()),
            palette.window_active_text_fg,
        )
    });
    let button = Rect::from_min_size(
        Pos2::new(
            bar.right() - 10.0 - galley.size().x - 2.0 * 14.0,
            bar.top() + 10.0,
        ),
        Vec2::new(galley.size().x + 2.0 * 14.0, 34.0),
    );
    let resp = ui.interact(button, Id::unique("tg_gift_box_back"), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, &label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(button, CornerRadius::same(6), palette.window_bg_over);
    }
    ui.painter().galley(
        Pos2::new(
            button.center().x - galley.size().x / 2.0,
            button.center().y - galley.size().y / 2.0,
        ),
        galley,
        palette.window_active_text_fg,
    );
    outcome.back = resp.clicked();
    let _ = width;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_payments::star_gifts::StarGift;

    fn gift(id: u64, stars: i64) -> CatalogGiftItem {
        CatalogGiftItem {
            gift: StarGift {
                id,
                stars,
                ..StarGift::default()
            },
            title: String::new(),
            artwork_kind: "star".to_owned(),
        }
    }

    #[test]
    fn sold_out_gifts_go_last_and_resale_entries_follow_their_gift() {
        let mut sold = gift(1, 50);
        sold.gift.limited_count = 100;
        sold.gift.limited_left = 0;
        sold.gift.sold_out = true;
        let mut resale = gift(2, 25);
        resale.gift.limited_count = 10;
        resale.gift.limited_left = 3;
        resale.gift.resell_count = 4;
        resale.gift.stars_resell_min = 900;
        let catalog = vec![sold, resale, gift(3, 15)];
        let all = gift_box_all_entries(&catalog);
        let order: Vec<(usize, bool)> = all.iter().map(|e| (e.catalog_index, e.resale)).collect();
        assert_eq!(order, vec![(1, false), (1, true), (2, false), (0, false)]);
    }

    #[test]
    fn tabs_match_make_gifts_price_tabs() {
        let catalog = vec![gift(1, 15)];
        assert_eq!(gift_box_tabs(&catalog, true, false), vec![GiftBoxTab::All]);
        let mut limited = gift(2, 50);
        limited.gift.limited_count = 10;
        limited.gift.limited_left = 5;
        limited.gift.upgradable = true;
        let catalog = vec![gift(1, 15), limited];
        assert_eq!(
            gift_box_tabs(&catalog, true, true),
            vec![GiftBoxTab::All, GiftBoxTab::Collectibles]
        );
        assert_eq!(
            gift_box_tabs(&catalog, false, true),
            vec![GiftBoxTab::All, GiftBoxTab::My, GiftBoxTab::Collectibles]
        );
        let collectibles = gift_box_entries(&catalog, GiftBoxTab::Collectibles);
        assert_eq!(collectibles.len(), 1);
        assert_eq!(collectibles[0].catalog_index, 1);
    }

    #[test]
    fn card_spec_badges_follow_gift_button_paint() {
        let plain = gift(1, 15);
        let spec = gift_box_card_spec(&plain, false, 0);
        assert_eq!(spec.badge, None);
        assert_eq!(spec.price, GiftPrice::Stars("15".to_owned()));
        assert!(spec.label.contains("15"), "no 'Star Gift #id' label");

        let mut limited = gift(2, 50);
        limited.gift.limited_count = 1000;
        limited.gift.limited_left = 10;
        let spec = gift_box_card_spec(&limited, false, 0);
        let badge = spec.badge.expect("limited badge");
        assert_eq!(badge.text, tr::lng_gift_stars_limited());
        assert_eq!(badge.bg, ACTIVE_TEXT_FG);

        limited.gift.limited_left = 0;
        let badge = gift_box_card_spec(&limited, false, 0)
            .badge
            .expect("sold out badge");
        assert_eq!(badge.text, tr::lng_gift_stars_sold_out());
        assert_eq!(badge.bg, colors::ATTENTION_BUTTON_FG);

        let mut premium = gift(3, 75);
        premium.gift.limited_count = 500;
        premium.gift.limited_left = 100;
        premium.gift.require_premium = true;
        let spec = gift_box_card_spec(&premium, false, 0);
        assert!(spec.premium_border);
        assert_eq!(
            spec.badge.map(|b| b.text),
            Some(tr::lng_gift_stars_premium())
        );

        let mut resale = gift(4, 25);
        resale.gift.limited_count = 10;
        resale.gift.resell_count = 3;
        resale.gift.stars_resell_min = 1200;
        let spec = gift_box_card_spec(&resale, true, 0);
        assert_eq!(spec.price, GiftPrice::Stars("1,200+".to_owned()));
        assert_eq!(
            spec.badge.map(|b| b.text),
            Some(tr::lng_gift_stars_resale())
        );
    }
}
