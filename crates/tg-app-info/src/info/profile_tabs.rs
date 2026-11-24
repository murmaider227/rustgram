//! Extended Profile tabs for `InfoSection`:
//! - Profile Stories / Albums (`info/stories/*`, `stories.getPinnedStories` / `getStoriesArchive` / albums)
//! - Peer Gifts (`info/peer_gifts/*`, reusing `tg-payments` `star_gifts` models & `payments/ui_helpers` artwork)
//! - Saved Music on Profile (`info/saved/info_saved_music*`, `users.getSavedMusic` / `account.saveMusic`)
//!
//! Port origin: `Telegram/SourceFiles/info/stories/info_stories_widget.cpp`,
//! `Telegram/SourceFiles/info/stories/info_stories_inner_widget.cpp`,
//! `Telegram/SourceFiles/info/stories/info_stories_albums.cpp`,
//! `Telegram/SourceFiles/info/peer_gifts/info_peer_gifts_widget.cpp`,
//! `Telegram/SourceFiles/info/peer_gifts/info_peer_gifts_collections.cpp`,
//! and `Telegram/SourceFiles/info/saved/info_saved_music_widget.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
use tg_payments::star_gifts::{PeerGiftsFilter, SavedStarGiftId};
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::input_field::InputField;

use crate::info::backend::{
    ProfilePeerGiftsPage, ProfileStoriesPage, ProfileStoryAlbum, SavedMusicTrack,
};
pub use crate::info::birthday_effect::{
    BIRTHDAY_EFFECT_DURATION_SECS, CONFETTI_PALETTE, ProfileBirthdayEffect, birthday_age,
    current_utc_ymd, is_birthday_today,
};
use crate::info::values::{format_duration, format_file_size};
use crate::payments::backend::ReceivedGiftItem;
use crate::payments::credits::format_stars_count;
use crate::payments::ui_helpers::{
    PREMIUM_PURPLE, STAR_GOLD, format_unix_date, paint_gift_artwork, paint_star_emblem,
    rgb_to_color32,
};
use tg_app_core::stand_in::types::PeerId;

// =============================================================================
// 1. Profile Stories / Albums (`info/stories/*`)
// =============================================================================

/// User-triggered actions emitted by [`ProfileStoriesSection::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileStoriesAction {
    /// Switch between Pinned Stories (`archive = false`) and Stories Archive (`archive = true`).
    SwitchArchive(bool),
    /// Filter by an album ID (`None` = All Stories).
    SelectAlbum(Option<i32>),
    /// Open a story in the story viewer.
    OpenStory {
        /// Owning peer ID.
        peer_id: PeerId,
        /// Selected story ID.
        story_id: i32,
    },
}

/// Renders the Profile Stories & Albums tab (`info/stories/*`).
pub struct ProfileStoriesSection;

impl ProfileStoriesSection {
    /// Draws the Pinned/Archive toggle, album chips, and 3-column story grid.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        peer_id: PeerId,
        archive: bool,
        can_view_archive: bool,
        selected_album: Option<i32>,
        albums: &[ProfileStoryAlbum],
        page: &ProfileStoriesPage,
        colors: &dyn Colors,
    ) -> Option<ProfileStoriesAction> {
        let mut action = None;
        let width = ui.available_width().max(220.0);

        // Mode tabs (Pinned vs Archive) when allowed
        if can_view_archive {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                if tab_pill(ui, "Pinned Stories", !archive, colors) {
                    action = Some(ProfileStoriesAction::SwitchArchive(false));
                }
                ui.add_space(6.0);
                if tab_pill(ui, "Stories Archive", archive, colors) {
                    action = Some(ProfileStoriesAction::SwitchArchive(true));
                }
            });
        }

        // Story Album chips (`info_stories_albums.cpp`)
        if !albums.is_empty() && !archive {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(12.0);
                if tab_pill(ui, "All Stories", selected_album.is_none(), colors) {
                    action = Some(ProfileStoriesAction::SelectAlbum(None));
                }
                for album in albums {
                    let lbl = if album.count > 0 {
                        format!("{} ({})", album.title, album.count)
                    } else {
                        album.title.clone()
                    };
                    if tab_pill(ui, &lbl, selected_album == Some(album.album_id), colors) {
                        action = Some(ProfileStoriesAction::SelectAlbum(Some(album.album_id)));
                    }
                }
            });
        }

        ui.add_space(10.0);

        if page.stories.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(if archive {
                        "No archived stories yet."
                    } else if selected_album.is_some() {
                        "This album has no stories yet."
                    } else {
                        "No pinned stories on this profile."
                    })
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
                );
            });
            return action;
        }

        // 3-column story thumbnail grid (`info_stories_inner_widget.cpp`)
        let pad_x = 12.0;
        let gap = 6.0;
        let cols = 3usize;
        let cell_w = ((width - pad_x * 2.0 - gap * (cols as f32 - 1.0)) / cols as f32).max(68.0);
        let cell_h = (cell_w * 1.42).round();

        for row_items in page.stories.chunks(cols) {
            let (row_rect, _) =
                ui.allocate_exact_size(Vec2::new(width, cell_h + gap), Sense::hover());
            for (c_idx, story) in row_items.iter().enumerate() {
                let x = row_rect.left() + pad_x + c_idx as f32 * (cell_w + gap);
                let tile =
                    Rect::from_min_size(Pos2::new(x, row_rect.top()), Vec2::new(cell_w, cell_h));
                let resp = ui.interact(
                    tile,
                    ui.make_persistent_id(("profile_story_tile", peer_id.0, story.id)),
                    Sense::click(),
                );
                let accessible = if story.caption.is_empty() {
                    format!("Story #{}", story.id)
                } else {
                    format!("Story #{}: {}", story.id, story.caption)
                };
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &accessible));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }

                let tint = Color32::from_rgb(
                    story.preview_rgb[0],
                    story.preview_rgb[1],
                    story.preview_rgb[2],
                );
                ui.painter().rect_filled(tile, CornerRadius::same(8), tint);
                if resp.hovered() {
                    ui.painter().rect_stroke(
                        tile,
                        CornerRadius::same(8),
                        Stroke::new(2.0, colors.color("windowActiveTextFg")),
                        egui::StrokeKind::Inside,
                    );
                }

                // Top-left pinned badge
                if story.pinned {
                    let badge = Rect::from_min_size(
                        Pos2::new(tile.left() + 5.0, tile.top() + 5.0),
                        Vec2::new(38.0, 16.0),
                    );
                    ui.painter().rect_filled(
                        badge,
                        CornerRadius::same(4),
                        Color32::from_rgba_unmultiplied(0, 0, 0, 130),
                    );
                    ui.painter().text(
                        badge.center(),
                        Align2::CENTER_CENTER,
                        "PIN",
                        FontId::new(9.5, family_semibold()),
                        Color32::WHITE,
                    );
                }

                // Top-right video duration pill
                if story.is_video && story.duration_secs > 0 {
                    let dur = format_duration(story.duration_secs);
                    let badge = Rect::from_min_size(
                        Pos2::new(tile.right() - 40.0, tile.top() + 5.0),
                        Vec2::new(35.0, 16.0),
                    );
                    ui.painter().rect_filled(
                        badge,
                        CornerRadius::same(4),
                        Color32::from_rgba_unmultiplied(0, 0, 0, 145),
                    );
                    ui.painter().text(
                        badge.center(),
                        Align2::CENTER_CENTER,
                        dur,
                        FontId::new(9.5, family_semibold()),
                        Color32::WHITE,
                    );
                }

                // Bottom caption & views scrim
                let scrim =
                    Rect::from_min_max(Pos2::new(tile.left(), tile.bottom() - 36.0), tile.max);
                ui.painter().rect_filled(
                    scrim,
                    CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: 8,
                        se: 8,
                    },
                    Color32::from_rgba_unmultiplied(0, 0, 0, 128),
                );
                let clip = ui.painter().with_clip_rect(scrim.shrink(4.0));
                if !story.caption.is_empty() {
                    clip.text(
                        Pos2::new(scrim.left() + 5.0, scrim.top() + 4.0),
                        Align2::LEFT_TOP,
                        &story.caption,
                        FontId::new(10.5, family_semibold()),
                        Color32::WHITE,
                    );
                }
                clip.text(
                    Pos2::new(scrim.left() + 5.0, scrim.bottom() - 4.0),
                    Align2::LEFT_BOTTOM,
                    format!("{} views", story.views_count),
                    FontId::new(9.5, family_regular()),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 210),
                );

                if resp.clicked() {
                    action = Some(ProfileStoriesAction::OpenStory {
                        peer_id,
                        story_id: story.id,
                    });
                }
            }
        }

        action
    }
}

// =============================================================================
// 2. Peer Gifts (`info/peer_gifts/*`, reusing tg-payments & payments/ui_helpers)
// =============================================================================

/// User-triggered actions emitted by [`PeerGiftsSection::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerGiftsAction {
    /// Toggle whether a saved gift is hidden (`unsave = true`) or displayed (`unsave = false`).
    ToggleVisibility {
        /// Message ID (user gift) or `0`.
        msg_id: i32,
        /// Saved ID (channel gift) or gift ID.
        saved_id: u64,
        /// New hidden/unsaved state.
        unsave: bool,
    },
    /// Toggle channel Star Gift notifications (`payments.toggleChatStarGiftNotifications`,
    /// `info_peer_gifts_widget.cpp:2639-2666`).
    ToggleNotifications(bool),
}

/// Renders the Profile Peer Gifts tab (`info/peer_gifts/*`), reusing `tg_payments::star_gifts`
/// models and `payments::ui_helpers::paint_gift_artwork`.
pub struct PeerGiftsSection;

impl PeerGiftsSection {
    /// Renders collection chips, filter checkboxes, 3-column Star Gifts grid, and selected gift detail bar.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        peer_id: PeerId,
        can_manage: bool,
        filter: &mut PeerGiftsFilter,
        selected_gift_id: &mut Option<u64>,
        page: &ProfilePeerGiftsPage,
        colors: &dyn Colors,
    ) -> Option<PeerGiftsAction> {
        Self::show_with_notifications(
            ui,
            peer_id,
            can_manage,
            None,
            filter,
            selected_gift_id,
            page,
            colors,
        )
    }

    /// Renders Peer Gifts with an optional channel gift notifications checkbox
    /// (`setupNotifyCheckbox` in `info_peer_gifts_widget.cpp:2639-2666`).
    #[allow(clippy::too_many_arguments)]
    pub fn show_with_notifications(
        ui: &mut Ui,
        peer_id: PeerId,
        can_manage: bool,
        notify_enabled: Option<bool>,
        filter: &mut PeerGiftsFilter,
        selected_gift_id: &mut Option<u64>,
        page: &ProfilePeerGiftsPage,
        colors: &dyn Colors,
    ) -> Option<PeerGiftsAction> {
        let mut action = None;
        let width = ui.available_width().max(220.0);

        // Channel Star Gift notifications bar (`setupNotifyCheckbox`, `info_peer_gifts_widget.cpp:2639`).
        if let Some(mut notify) = notify_enabled {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let lbl = tg_lang::tr::lng_peer_gifts_notify();
                if ui.checkbox(&mut notify, lbl).changed() {
                    action = Some(PeerGiftsAction::ToggleNotifications(notify));
                }
            });
        }

        // 1. Gift Collections strip (`info_peer_gifts_collections.cpp`)
        if !page.collections.is_empty() {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_space(12.0);
                if tab_pill(
                    ui,
                    &format!("All Gifts ({})", page.total_count),
                    filter.collection_id.is_none(),
                    colors,
                ) {
                    filter.collection_id = None;
                }
                for col in &page.collections {
                    let lbl = format!("{} ({})", col.title, col.count);
                    if tab_pill(ui, &lbl, filter.collection_id == Some(col.id), colors) {
                        filter.collection_id = Some(col.id);
                    }
                }
            });
        }

        // 2. `PeerGiftsFilter` controls (sort by value / hide unlimited / hide hidden)
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_space(12.0);
            ui.checkbox(&mut filter.sort_by_value, "Sort by Value");
            ui.checkbox(&mut filter.skip_unlimited, "Hide Unlimited");
            ui.checkbox(&mut filter.skip_unsaved, "Displayed Only");
        });

        ui.add_space(8.0);

        if page.gifts.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new("No Star Gifts match the current filter.")
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowSubTextFg")),
                );
            });
            return None;
        }

        // 3. 3-column gift cards grid (`info_peer_gifts_common.cpp`)
        let pad_x = 12.0;
        let gap = 8.0;
        let cols = 3usize;
        let cell_w = ((width - pad_x * 2.0 - gap * (cols as f32 - 1.0)) / cols as f32).max(76.0);
        let cell_h = 114.0;

        for row_items in page.gifts.chunks(cols) {
            let (row_rect, _) =
                ui.allocate_exact_size(Vec2::new(width, cell_h + gap), Sense::hover());
            for (c_idx, item) in row_items.iter().enumerate() {
                let x = row_rect.left() + pad_x + c_idx as f32 * (cell_w + gap);
                let card =
                    Rect::from_min_size(Pos2::new(x, row_rect.top()), Vec2::new(cell_w, cell_h));
                let is_selected = *selected_gift_id == Some(item.saved.info.id);
                let resp = ui.interact(
                    card,
                    ui.make_persistent_id(("peer_gift_card", peer_id.0, item.saved.info.id)),
                    Sense::click(),
                );
                resp.widget_info(|| {
                    WidgetInfo::selected(Role::Button, true, is_selected, &item.title)
                });
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }

                let (card_bg, accent) = gift_card_colors(item, colors);
                ui.painter()
                    .rect_filled(card, CornerRadius::same(10), card_bg);
                if is_selected || resp.hovered() {
                    ui.painter().rect_stroke(
                        card,
                        CornerRadius::same(10),
                        Stroke::new(
                            if is_selected { 2.0 } else { 1.0 },
                            colors.color("windowActiveTextFg"),
                        ),
                        egui::StrokeKind::Inside,
                    );
                }

                // Artwork emblem (reusing `payments::ui_helpers::paint_gift_artwork`)
                let art_rect = Rect::from_center_size(
                    Pos2::new(card.center().x, card.top() + 40.0),
                    Vec2::splat(48.0),
                );
                paint_gift_artwork(ui.painter(), art_rect, &item.artwork_kind, accent);

                // Status badge (Pinned / Hidden / Unique #N)
                if let Some(ref unique) = item.saved.info.unique {
                    let badge_txt = format!("#{}", unique.number);
                    ui.painter().text(
                        Pos2::new(card.right() - 6.0, card.top() + 6.0),
                        Align2::RIGHT_TOP,
                        badge_txt,
                        FontId::new(9.5, family_semibold()),
                        Color32::WHITE,
                    );
                } else if item.saved.hidden {
                    ui.painter().text(
                        Pos2::new(card.right() - 6.0, card.top() + 6.0),
                        Align2::RIGHT_TOP,
                        "Hidden",
                        FontId::new(9.5, family_semibold()),
                        colors.color("windowSubTextFg"),
                    );
                }

                if item.saved.pinned {
                    ui.painter().text(
                        Pos2::new(card.left() + 6.0, card.top() + 6.0),
                        Align2::LEFT_TOP,
                        "PIN",
                        FontId::new(9.0, family_semibold()),
                        if item.saved.info.unique.is_some() {
                            Color32::WHITE
                        } else {
                            PREMIUM_PURPLE
                        },
                    );
                }

                // Title & Star value footer
                let title_fg = if item.saved.info.unique.is_some() {
                    Color32::WHITE
                } else {
                    colors.color("windowFg")
                };
                let clip = ui.painter().with_clip_rect(card.shrink(4.0));
                clip.text(
                    Pos2::new(card.center().x, card.bottom() - 32.0),
                    Align2::CENTER_TOP,
                    &item.title,
                    FontId::new(11.0, family_semibold()),
                    title_fg,
                );
                let star_y = card.bottom() - 12.0;
                paint_star_emblem(
                    ui.painter(),
                    Pos2::new(card.center().x - 24.0, star_y),
                    5.5,
                    STAR_GOLD,
                );
                clip.text(
                    Pos2::new(card.center().x - 14.0, star_y),
                    Align2::LEFT_CENTER,
                    format_stars_count(item.saved.info.stars),
                    FontId::new(10.5, family_semibold()),
                    STAR_GOLD,
                );

                if resp.clicked() {
                    *selected_gift_id = if is_selected {
                        None
                    } else {
                        Some(item.saved.info.id)
                    };
                }
            }
        }

        // 4. Inline selected gift detail summary card
        if let Some(sel_id) = *selected_gift_id
            && let Some(item) = page.gifts.iter().find(|g| g.saved.info.id == sel_id)
        {
            ui.add_space(8.0);
            egui::Frame::NONE
                .fill(colors.color("windowBgOver"))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_width((width - 48.0).max(160.0));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&item.title)
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowFg")),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format_unix_date(item.saved.date))
                                    .font(FontId::new(11.0, family_regular()))
                                    .color(colors.color("windowSubTextFg")),
                            );
                        });
                    });
                    ui.label(
                        egui::RichText::new(format!(
                            "From: {} · Lifecycle: {:?}",
                            item.sender_name,
                            item.saved.lifecycle_state()
                        ))
                        .font(FontId::new(11.5, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                    );
                    if !item.saved.message.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("\"{}\"", item.saved.message))
                                .font(font_id_from_style(st::normal_font))
                                .color(colors.color("windowFg")),
                        );
                    }
                    if can_manage {
                        ui.add_space(8.0);
                        let toggle_lbl = if item.saved.hidden {
                            "Show on Profile"
                        } else {
                            "Hide from Profile"
                        };
                        if ui.button(toggle_lbl).clicked() {
                            let (msg_id, saved_id) = match item.saved.manage_id {
                                Some(SavedStarGiftId::User(mid)) => {
                                    (mid.0 as i32, item.saved.info.id)
                                }
                                Some(SavedStarGiftId::Chat { saved_id, .. }) => (0, saved_id),
                                _ => (0, item.saved.info.id),
                            };
                            action = Some(PeerGiftsAction::ToggleVisibility {
                                msg_id,
                                saved_id,
                                unsave: !item.saved.hidden,
                            });
                        }
                    }
                });
        }

        action
    }
}

fn gift_card_colors(item: &ReceivedGiftItem, colors: &dyn Colors) -> (Color32, Color32) {
    if let Some(ref unique) = item.saved.info.unique {
        return (
            rgb_to_color32(unique.backdrop.center_color),
            rgb_to_color32(unique.backdrop.pattern_color),
        );
    }
    (colors.color("windowBgOver"), STAR_GOLD)
}

// =============================================================================
// 3. Saved Music on Profile (`info/saved/info_saved_music*`)
// =============================================================================

/// User-triggered actions emitted by [`SavedMusicSection::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavedMusicAction {
    /// Start or pause playback of the selected track.
    TogglePlay(i64),
    /// Save (`unsave = false`) or remove (`unsave = true`) a track via `account.saveMusic`.
    ToggleSaveProfile {
        /// Track payload.
        track: SavedMusicTrack,
        /// `true` to remove from profile, `false` to save.
        unsave: bool,
    },
}

/// Renders the Saved Music profile tab (`info/saved/info_saved_music_widget.cpp`).
pub struct SavedMusicSection;

impl SavedMusicSection {
    /// Renders the search bar and list of saved profile audio tracks.
    pub fn show(
        ui: &mut Ui,
        tracks: &[SavedMusicTrack],
        search_query: &mut String,
        playing_doc_id: Option<i64>,
        can_manage: bool,
        colors: &dyn Colors,
    ) -> Option<SavedMusicAction> {
        let mut action = None;
        let width = ui.available_width().max(220.0);

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.allocate_ui(Vec2::new((width - 24.0).max(160.0), 34.0), |ui| {
                InputField::single_line(
                    "saved_music_search",
                    search_query,
                    "Search saved music",
                    InputFieldStyle::default_input(colors),
                )
                .show(ui);
            });
        });
        ui.add_space(8.0);

        let q = search_query.trim().to_lowercase();
        let filtered: Vec<&SavedMusicTrack> = tracks
            .iter()
            .filter(|t| {
                q.is_empty()
                    || t.title.to_lowercase().contains(&q)
                    || t.performer.to_lowercase().contains(&q)
            })
            .collect();

        if filtered.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new(if tracks.is_empty() {
                        "No saved music on this profile yet."
                    } else {
                        "No matching tracks found."
                    })
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
                );
            });
            return None;
        }

        for track in filtered {
            let is_playing = playing_doc_id == Some(track.document_id);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::click());
            resp.widget_info(|| {
                WidgetInfo::labeled(
                    Role::Button,
                    true,
                    format!("{} — {}", track.title, track.performer),
                )
            });
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
            }

            // Circular play/pause button
            let play_center = Pos2::new(rect.left() + 30.0, rect.center().y);
            ui.painter()
                .circle_filled(play_center, 18.0, colors.color("windowBgActive"));
            if is_playing {
                tg_app_core::ui_icons::paint_pause(ui.painter(), play_center, 13.0, Color32::WHITE);
            } else {
                tg_app_core::ui_icons::paint_triangle(
                    ui.painter(),
                    play_center + Vec2::new(1.5, 0.0),
                    13.0,
                    tg_app_core::ui_icons::ArrowDir::Right,
                    Color32::WHITE,
                );
            }

            // Title & performer/duration/size subtitle
            let text_x = rect.left() + 58.0;
            let right_reserve = if can_manage { 86.0 } else { 14.0 };
            let clip = Rect::from_min_max(
                Pos2::new(text_x, rect.top()),
                Pos2::new(
                    (rect.right() - right_reserve).max(text_x + 40.0),
                    rect.bottom(),
                ),
            );
            let p = ui.painter().with_clip_rect(clip);
            p.text(
                Pos2::new(text_x, rect.top() + 9.0),
                Align2::LEFT_TOP,
                &track.title,
                font_id_from_style(st::semibold_font),
                if is_playing {
                    colors.color("windowActiveTextFg")
                } else {
                    colors.color("windowFg")
                },
            );
            let sub = format!(
                "{} · {} · {}",
                track.performer,
                format_duration(track.duration_secs),
                format_file_size(track.size_bytes)
            );
            p.text(
                Pos2::new(text_x, rect.top() + 29.0),
                Align2::LEFT_TOP,
                sub,
                FontId::new(11.5, family_regular()),
                colors.color("windowSubTextFg"),
            );

            // Optional Save / Remove pill on the right
            let mut action_btn_clicked = false;
            if can_manage {
                let btn_rect = Rect::from_center_size(
                    Pos2::new(rect.right() - 44.0, rect.center().y),
                    Vec2::new(68.0, 26.0),
                );
                let btn_lbl = if track.saved_on_profile {
                    "Remove"
                } else {
                    "Save"
                };
                let btn_resp = ui.interact(
                    btn_rect,
                    ui.make_persistent_id(("saved_music_toggle", track.document_id)),
                    Sense::click(),
                );
                btn_resp.widget_info(|| {
                    WidgetInfo::labeled(Role::Button, true, format!("{btn_lbl} {}", track.title))
                });
                if btn_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().rect_filled(
                    btn_rect,
                    CornerRadius::same(13),
                    colors.color("windowBgOver"),
                );
                ui.painter().text(
                    btn_rect.center(),
                    Align2::CENTER_CENTER,
                    btn_lbl,
                    FontId::new(11.0, family_semibold()),
                    if track.saved_on_profile {
                        colors.color("attentionButtonFg")
                    } else {
                        colors.color("windowActiveTextFg")
                    },
                );
                if btn_resp.clicked() {
                    action_btn_clicked = true;
                    action = Some(SavedMusicAction::ToggleSaveProfile {
                        track: track.clone(),
                        unsave: track.saved_on_profile,
                    });
                }
            }

            if resp.clicked() && !action_btn_clicked {
                action = Some(SavedMusicAction::TogglePlay(track.document_id));
            }
        }

        action
    }
}

fn tab_pill(ui: &mut Ui, label: &str, active: bool, colors: &dyn Colors) -> bool {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::new(12.0, family_semibold()),
        if active {
            Color32::WHITE
        } else {
            colors.color("windowFg")
        },
    );
    let size = Vec2::new(galley.size().x + 20.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, active, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let bg = if active {
        colors.color("windowBgActive")
    } else if resp.hovered() {
        colors.color("windowBgOver")
    } else {
        Color32::from_rgba_unmultiplied(128, 128, 128, 28)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        Color32::WHITE,
    );
    resp.clicked()
}
