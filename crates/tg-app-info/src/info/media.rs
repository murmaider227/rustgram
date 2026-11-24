//! Shared media provider and list/grid widget (Media, Files, Links, Voice, Music, GIFs).
//!
//! Port origin: `Telegram/SourceFiles/info/media/info_media_list_widget.cpp`,
//! `Telegram/SourceFiles/info/media/info_media_provider.cpp`,
//! and `Telegram/SourceFiles/info/media/info_media_list_section.cpp`.

use std::collections::HashSet;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, family_semibold, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::{ColorsResolver, resolve_color_ref};

use crate::info::backend::{InfoBackend, MediaItem, ProfileMainTab, SharedMediaTab};
use crate::info::profile_controls::shared_media_to_profile_main_tab;
use crate::info::strings;
use crate::info::values::{format_duration, format_file_size};
use tg_app_core::stand_in::types::PeerId;

/// Grouped slice of shared media items under one month/date header (`Info::Media::ListSection`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDateSection<'a> {
    /// Section header label (e.g. `"November 2026"`).
    pub header: &'a str,
    /// Items belonging to this section.
    pub items: Vec<&'a MediaItem>,
}

/// Provider organizing shared media items into date sections (`Info::Media::Provider`).
pub struct MediaProvider;

impl MediaProvider {
    /// Fetches filtered shared media items from `backend`.
    #[must_use]
    pub fn load(
        backend: &dyn InfoBackend,
        peer_id: PeerId,
        tab: SharedMediaTab,
        query: &str,
    ) -> Vec<MediaItem> {
        backend.shared_media(peer_id, tab, query)
    }

    /// Groups an ordered list of [`MediaItem`]s by `date_group` while preserving order.
    #[must_use]
    pub fn group_by_date<'a>(items: &'a [MediaItem]) -> Vec<MediaDateSection<'a>> {
        let mut sections: Vec<MediaDateSection<'a>> = Vec::new();
        for item in items {
            let header = item.date_group();
            if let Some(last) = sections.last_mut()
                && last.header == header
            {
                last.items.push(item);
                continue;
            }
            sections.push(MediaDateSection {
                header,
                items: vec![item],
            });
        }
        sections
    }
}

/// Events emitted by [`MediaListWidget::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaListAction {
    /// Clicked an item to open/preview/play it.
    OpenItem(i32),
    /// Toggled multi-selection state for an item.
    ToggleSelect(i32),
}

/// Renders the shared media tab strip and content grid/list (`Info::Media::ListWidget`).
#[derive(Debug, Default)]
pub struct MediaListWidget {
    /// Active audio/voice item currently playing in mock playback preview.
    playing_item_id: Option<i32>,
}

impl MediaListWidget {
    /// Creates a new [`MediaListWidget`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Renders the 6-tab shared media strip with real `tg_style::st::info_icon_media_*` icons
    /// and localized tab labels.
    pub fn show_tabs(
        ui: &mut Ui,
        id_salt: &str,
        active_tab: &mut SharedMediaTab,
        colors: &dyn Colors,
    ) {
        let _ = Self::show_tabs_with_main_tab(
            ui,
            id_salt,
            active_tab,
            &SharedMediaTab::ALL,
            None,
            false,
            colors,
        );
    }

    /// Renders the 6-tab shared media strip in `order` with optional Main Profile Tab indicator
    /// and right-click `"Set as Main Tab"` context menu (`info_profile_tabs_host.cpp`).
    ///
    /// Returns `Some(ProfileMainTab)` when the user selects `"Set as Main Tab"` from a tab's
    /// context menu.
    #[allow(clippy::too_many_arguments)]
    pub fn show_tabs_with_main_tab(
        ui: &mut Ui,
        id_salt: &str,
        active_tab: &mut SharedMediaTab,
        order: &[SharedMediaTab; 6],
        main_tab: Option<ProfileMainTab>,
        can_set_main: bool,
        colors: &dyn Colors,
    ) -> Option<ProfileMainTab> {
        let mut set_main_request = None;
        let width = ui.available_width().max(200.0);
        let tab_h = 48.0;
        let (strip_rect, _) = ui.allocate_exact_size(Vec2::new(width, tab_h), Sense::hover());
        ui.painter()
            .rect_filled(strip_rect, CornerRadius::ZERO, colors.color("windowBg"));

        let count = order.len() as f32;
        let tab_w = width / count;
        let resolver = ColorsResolver(colors);
        let active_fg = colors.color("windowActiveTextFg");
        let inactive_fg = colors.color("windowSubTextFg");
        let active_bar = colors.color("windowBgActive");

        for (i, tab) in order.iter().copied().enumerate() {
            let is_active = *active_tab == tab;
            let profile_tab = shared_media_to_profile_main_tab(tab);
            let is_main = main_tab == Some(profile_tab);
            let tab_rect = Rect::from_min_size(
                Pos2::new(strip_rect.min.x + i as f32 * tab_w, strip_rect.min.y),
                Vec2::new(tab_w, tab_h),
            );
            let resp = ui.interact(
                tab_rect,
                ui.make_persistent_id((id_salt, "media_tab", tab.index())),
                Sense::click(),
            );
            let acc_label = if is_main {
                format!("{} (Main Tab)", tab.localized_section_title())
            } else {
                tab.localized_section_title()
            };
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Tab, true, is_active, &acc_label)
            });
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().rect_filled(
                    tab_rect,
                    CornerRadius::ZERO,
                    colors.color("windowBgOver"),
                );
            }

            let fg = if is_active { active_fg } else { inactive_fg };
            let icon = tab_icon(tab);
            let icon_rect = Rect::from_center_size(
                Pos2::new(tab_rect.center().x, tab_rect.min.y + 16.0),
                Vec2::splat(18.0),
            );
            paint_style_icon(ui.painter(), icon_rect, *icon, &resolver, Some(fg));

            if is_main {
                ui.painter().circle_filled(
                    Pos2::new(icon_rect.right() + 3.0, icon_rect.top() + 2.0),
                    2.5,
                    active_bar,
                );
            }

            let short_label = tab_short_label(tab);
            let font = if is_active {
                FontId::new(11.0, family_semibold())
            } else {
                FontId::new(11.0, family_regular())
            };
            ui.painter().text(
                Pos2::new(tab_rect.center().x, tab_rect.min.y + 34.0),
                Align2::CENTER_CENTER,
                short_label,
                font,
                fg,
            );

            if is_active {
                let bar_w = (tab_w - 14.0).max(20.0);
                let bar_rect = Rect::from_min_size(
                    Pos2::new(tab_rect.center().x - bar_w * 0.5, tab_rect.max.y - 3.0),
                    Vec2::new(bar_w, 3.0),
                );
                ui.painter()
                    .rect_filled(bar_rect, CornerRadius::same(1), active_bar);
            }

            if can_set_main && !is_main {
                resp.context_menu(|ui| {
                    if ui.button(tr::lng_profile_tab_set_as_main()).clicked() {
                        set_main_request = Some(profile_tab);
                        ui.close();
                    }
                });
            }

            let resp = resp.on_hover_text(acc_label);
            if resp.clicked() {
                *active_tab = tab;
            }
        }

        // Bottom hairline divider
        ui.painter().line_segment(
            [
                Pos2::new(strip_rect.left(), strip_rect.bottom()),
                Pos2::new(strip_rect.right(), strip_rect.bottom()),
            ],
            Stroke::new(1.0, colors.color("shadowFg")),
        );

        set_main_request
    }

    /// Renders the shared media content for `tab` and `items`.
    pub fn show_items(
        &mut self,
        ui: &mut Ui,
        tab: SharedMediaTab,
        items: &[MediaItem],
        selected_ids: &HashSet<i32>,
        colors: &dyn Colors,
    ) -> Option<MediaListAction> {
        let width = ui.available_width().max(200.0);
        if items.is_empty() {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 96.0), Sense::hover());
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!(
                    "{} {} {}",
                    strings::MEDIA_EMPTY_PREFIX,
                    tab.localized_section_title().to_lowercase(),
                    strings::MEDIA_EMPTY_SUFFIX
                ),
                font_id_from_style(st::normal_font),
                colors.color("windowSubTextFg"),
            );
            return None;
        }

        let sections = MediaProvider::group_by_date(items);
        let mut action = None;
        let hdr_height = (st::info_media_header_height.0 as f32).max(32.0);
        let hdr_font = font_id_from_style(st::info_media_header_style.font);
        let hdr_fg = resolve_color_ref(st::info_media_header_fg, colors);
        let left_pad = (st::info_media_left.0 as f32).max(16.0);

        for section in sections {
            // Section month header (`st::info_media_header_style` / `st::info_media_header_fg`).
            let (hdr, _) = ui.allocate_exact_size(Vec2::new(width, hdr_height), Sense::hover());
            ui.painter().text(
                Pos2::new(hdr.min.x + left_pad, hdr.center().y),
                Align2::LEFT_CENTER,
                section.header,
                hdr_font.clone(),
                hdr_fg,
            );

            match tab {
                SharedMediaTab::PhotosVideos => {
                    if let Some(a) =
                        render_photo_video_grid(ui, &section.items, selected_ids, colors)
                    {
                        action = Some(a);
                    }
                }
                SharedMediaTab::Gifs => {
                    if let Some(a) = render_gif_grid(ui, &section.items, selected_ids, colors) {
                        action = Some(a);
                    }
                }
                SharedMediaTab::Files => {
                    for item in section.items {
                        if let Some(a) = render_file_row(ui, item, selected_ids, colors) {
                            action = Some(a);
                        }
                    }
                }
                SharedMediaTab::Links => {
                    for item in section.items {
                        if let Some(a) = render_link_row(ui, item, selected_ids, colors) {
                            action = Some(a);
                        }
                    }
                }
                SharedMediaTab::Voice => {
                    for item in section.items {
                        if let Some(a) = self.render_voice_row(ui, item, selected_ids, colors) {
                            action = Some(a);
                        }
                    }
                }
                SharedMediaTab::Music => {
                    for item in section.items {
                        if let Some(a) = self.render_music_row(ui, item, selected_ids, colors) {
                            action = Some(a);
                        }
                    }
                }
            }
        }

        action
    }

    fn render_voice_row(
        &mut self,
        ui: &mut Ui,
        item: &MediaItem,
        selected_ids: &HashSet<i32>,
        colors: &dyn Colors,
    ) -> Option<MediaListAction> {
        let MediaItem::Voice {
            id,
            sender,
            duration_secs,
            waveform,
            listened,
            date_label,
            ..
        } = item
        else {
            return None;
        };

        let width = ui.available_width().max(200.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 58.0), Sense::click());
        let selected = selected_ids.contains(id);
        if selected {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, colors.color("lightButtonBgOver"));
        } else if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
        }

        let playing = self.playing_item_id == Some(*id);
        let play_c = Pos2::new(rect.min.x + 36.0, rect.center().y);
        ui.painter()
            .circle_filled(play_c, 19.0, colors.color("windowBgActive"));
        paint_play_or_pause(ui.painter(), play_c, playing, colors, Color32::WHITE);

        let left_x = rect.min.x + 66.0;
        ui.painter().text(
            Pos2::new(left_x, rect.min.y + 8.0),
            Align2::LEFT_TOP,
            sender,
            FontId::new(13.0, family_semibold()),
            colors.color("windowBoldFg"),
        );

        // Waveform bars
        let bar_y_base = rect.min.y + 44.0;
        let bar_w = 3.0;
        let bar_gap = 2.0;
        let max_bars =
            (((rect.max.x - left_x - 86.0) / (bar_w + bar_gap)).floor() as usize).clamp(8, 32);
        for (i, &amp) in waveform.iter().take(max_bars).enumerate() {
            let h = ((f32::from(amp) / 31.0) * 16.0).max(2.5);
            let x = left_x + i as f32 * (bar_w + bar_gap);
            let bar_color = if playing && i < max_bars / 2 || !listened {
                colors.color("windowBgActive")
            } else {
                colors.color("sliderBgInactive")
            };
            ui.painter().rect_filled(
                Rect::from_min_size(Pos2::new(x, bar_y_base - h), Vec2::new(bar_w, h)),
                CornerRadius::same(1),
                bar_color,
            );
        }

        ui.painter().text(
            Pos2::new(rect.max.x - 16.0, rect.min.y + 10.0),
            Align2::RIGHT_TOP,
            format_duration(*duration_secs),
            FontId::new(12.0, family_regular()),
            colors.color("windowActiveTextFg"),
        );
        ui.painter().text(
            Pos2::new(rect.max.x - 16.0, rect.min.y + 29.0),
            Align2::RIGHT_TOP,
            date_label,
            FontId::new(11.0, family_regular()),
            colors.color("windowSubTextFg"),
        );

        if resp.secondary_clicked() {
            return Some(MediaListAction::ToggleSelect(*id));
        }
        if resp.clicked() {
            self.playing_item_id = if playing { None } else { Some(*id) };
            return Some(MediaListAction::OpenItem(*id));
        }
        None
    }

    fn render_music_row(
        &mut self,
        ui: &mut Ui,
        item: &MediaItem,
        selected_ids: &HashSet<i32>,
        colors: &dyn Colors,
    ) -> Option<MediaListAction> {
        let MediaItem::Music {
            id,
            title,
            performer,
            duration_secs,
            size_bytes,
            ..
        } = item
        else {
            return None;
        };

        let width = ui.available_width().max(200.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 56.0), Sense::click());
        let selected = selected_ids.contains(id);
        if selected {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, colors.color("lightButtonBgOver"));
        } else if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
        }

        let playing = self.playing_item_id == Some(*id);
        let play_c = Pos2::new(rect.min.x + 36.0, rect.center().y);
        ui.painter()
            .circle_filled(play_c, 19.0, colors.color("windowBgActive"));
        paint_play_or_pause(ui.painter(), play_c, playing, colors, Color32::WHITE);

        let left_x = rect.min.x + 66.0;
        ui.painter().text(
            Pos2::new(left_x, rect.min.y + 10.0),
            Align2::LEFT_TOP,
            title,
            font_id_from_style(st::semibold_font),
            colors.color("windowBoldFg"),
        );
        let subtitle = format!(
            "{performer} · {} · {}",
            format_duration(*duration_secs),
            format_file_size(*size_bytes)
        );
        ui.painter().text(
            Pos2::new(left_x, rect.min.y + 30.0),
            Align2::LEFT_TOP,
            subtitle,
            FontId::new(12.0, family_regular()),
            colors.color("windowSubTextFg"),
        );

        if resp.secondary_clicked() {
            return Some(MediaListAction::ToggleSelect(*id));
        }
        if resp.clicked() {
            self.playing_item_id = if playing { None } else { Some(*id) };
            return Some(MediaListAction::OpenItem(*id));
        }
        None
    }
}

fn tab_icon(tab: SharedMediaTab) -> &'static Icon {
    match tab {
        SharedMediaTab::PhotosVideos => &st::info_icon_media_photo,
        SharedMediaTab::Files => &st::info_icon_media_file,
        SharedMediaTab::Links => &st::info_icon_media_link,
        SharedMediaTab::Voice => &st::info_icon_media_voice,
        SharedMediaTab::Music => &st::info_icon_media_audio,
        SharedMediaTab::Gifs => &st::info_icon_media_gif,
    }
}

fn tab_short_label(tab: SharedMediaTab) -> String {
    match tab {
        SharedMediaTab::PhotosVideos => tr::lng_media_type_media(),
        SharedMediaTab::Files => tr::lng_media_type_files(),
        SharedMediaTab::Links => tr::lng_info_link_label(),
        SharedMediaTab::Voice => tr::lng_media_type_audios()
            .split_whitespace()
            .next()
            .unwrap_or("Voice")
            .to_owned(),
        SharedMediaTab::Music => tr::lng_rights_chat_music(),
        SharedMediaTab::Gifs => tr::lng_media_type_gifs(),
    }
}

fn render_photo_video_grid(
    ui: &mut Ui,
    items: &[&MediaItem],
    selected_ids: &HashSet<i32>,
    colors: &dyn Colors,
) -> Option<MediaListAction> {
    let mut action = None;
    let width = ui.available_width().max(180.0);
    let pad = (st::info_media_left.0 as f32).max(12.0);
    let gap = (st::info_media_skip.0 as f32).max(4.0);
    let cols = 3usize;
    let tile_size = ((width - pad * 2.0 - gap * (cols - 1) as f32) / cols as f32).max(48.0);
    let resolver = ColorsResolver(colors);

    for row in items.chunks(cols) {
        let (row_rect, _) =
            ui.allocate_exact_size(Vec2::new(width, tile_size + gap), Sense::hover());
        for (col_idx, item) in row.iter().enumerate() {
            let x = row_rect.min.x + pad + col_idx as f32 * (tile_size + gap);
            let tile_rect =
                Rect::from_min_size(Pos2::new(x, row_rect.min.y), Vec2::splat(tile_size));
            let id = item.id();
            let resp = ui.interact(
                tile_rect,
                ui.make_persistent_id(("media_grid_tile", id)),
                Sense::click(),
            );
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }

            let (rgb, duration, caption) = match item {
                MediaItem::Photo {
                    preview_rgb,
                    caption,
                    ..
                } => (*preview_rgb, None, caption.as_str()),
                MediaItem::Video {
                    preview_rgb,
                    duration_secs,
                    caption,
                    ..
                } => (*preview_rgb, Some(*duration_secs), caption.as_str()),
                _ => ([0x80, 0x90, 0xA0], None, ""),
            };
            let base = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            let dark = Color32::from_rgb(
                rgb[0].saturating_sub(36),
                rgb[1].saturating_sub(36),
                rgb[2].saturating_sub(36),
            );
            ui.painter()
                .rect_filled(tile_rect, CornerRadius::same(4), base);
            let lower = Rect::from_min_max(
                Pos2::new(tile_rect.min.x, tile_rect.center().y + tile_size * 0.12),
                tile_rect.max,
            );
            ui.painter().rect_filled(lower, CornerRadius::same(4), dark);

            if let Some(secs) = duration {
                let badge_rect = Rect::from_min_size(
                    Pos2::new(tile_rect.min.x + 5.0, tile_rect.min.y + 5.0),
                    Vec2::new(40.0, 17.0),
                );
                ui.painter().rect_filled(
                    badge_rect,
                    CornerRadius::same(4),
                    Color32::from_rgba_unmultiplied(0, 0, 0, 145),
                );
                ui.painter().text(
                    badge_rect.center(),
                    Align2::CENTER_CENTER,
                    format_duration(secs),
                    FontId::new(10.0, family_semibold()),
                    Color32::WHITE,
                );
                let play_rect = Rect::from_center_size(tile_rect.center(), Vec2::splat(22.0));
                paint_style_icon(
                    ui.painter(),
                    play_rect,
                    st::history_file_thumb_play,
                    &resolver,
                    Some(Color32::from_rgba_unmultiplied(255, 255, 255, 235)),
                );
            }

            // Selection indicator badge in top-right corner
            let is_selected = selected_ids.contains(&id);
            if is_selected || resp.hovered() {
                let check_c = Pos2::new(tile_rect.max.x - 14.0, tile_rect.min.y + 14.0);
                let bg = if is_selected {
                    colors.color("overviewCheckBgActive")
                } else {
                    colors.color("overviewCheckBg")
                };
                ui.painter().circle_filled(check_c, 9.0, bg);
                ui.painter()
                    .circle_stroke(check_c, 9.0, Stroke::new(1.5, Color32::WHITE));
                if is_selected {
                    let chk_rect = Rect::from_center_size(check_c, Vec2::splat(13.0));
                    paint_style_icon(
                        ui.painter(),
                        chk_rect,
                        st::default_checkbox_icon,
                        &resolver,
                        Some(Color32::WHITE),
                    );
                }
            }

            let resp = if !caption.is_empty() {
                resp.on_hover_text(caption)
            } else {
                resp
            };

            if resp.secondary_clicked() || (resp.clicked() && !selected_ids.is_empty()) {
                action = Some(MediaListAction::ToggleSelect(id));
            } else if resp.clicked() {
                action = Some(MediaListAction::OpenItem(id));
            }
        }
    }
    action
}

fn render_gif_grid(
    ui: &mut Ui,
    items: &[&MediaItem],
    selected_ids: &HashSet<i32>,
    colors: &dyn Colors,
) -> Option<MediaListAction> {
    let mut action = None;
    let width = ui.available_width().max(180.0);
    let pad = (st::info_media_left.0 as f32).max(12.0);
    let gap = 6.0;
    let cols = 2usize;
    let tile_w = ((width - pad * 2.0 - gap) / cols as f32).max(70.0);
    let tile_h = (tile_w * 0.68).max(58.0);

    for row in items.chunks(cols) {
        let (row_rect, _) = ui.allocate_exact_size(Vec2::new(width, tile_h + gap), Sense::hover());
        for (col_idx, item) in row.iter().enumerate() {
            let MediaItem::Gif {
                id,
                label,
                preview_rgb,
                ..
            } = item
            else {
                continue;
            };
            let x = row_rect.min.x + pad + col_idx as f32 * (tile_w + gap);
            let tile_rect =
                Rect::from_min_size(Pos2::new(x, row_rect.min.y), Vec2::new(tile_w, tile_h));
            let resp = ui.interact(
                tile_rect,
                ui.make_persistent_id(("media_gif_tile", *id)),
                Sense::click(),
            );
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let bg = Color32::from_rgb(preview_rgb[0], preview_rgb[1], preview_rgb[2]);
            ui.painter()
                .rect_filled(tile_rect, CornerRadius::same(6), bg);
            let pill = Rect::from_min_size(
                Pos2::new(tile_rect.min.x + 6.0, tile_rect.min.y + 6.0),
                Vec2::new(28.0, 16.0),
            );
            ui.painter().rect_filled(
                pill,
                CornerRadius::same(4),
                Color32::from_rgba_unmultiplied(0, 0, 0, 140),
            );
            ui.painter().text(
                pill.center(),
                Align2::CENTER_CENTER,
                strings::MEDIA_GIF_BADGE,
                FontId::new(9.5, family_semibold()),
                Color32::WHITE,
            );
            ui.painter().text(
                Pos2::new(tile_rect.center().x, tile_rect.max.y - 14.0),
                Align2::CENTER_CENTER,
                label,
                FontId::new(11.5, family_regular()),
                Color32::WHITE,
            );
            if selected_ids.contains(id) {
                ui.painter().rect_stroke(
                    tile_rect,
                    CornerRadius::same(6),
                    Stroke::new(2.5, colors.color("windowBgActive")),
                    egui::StrokeKind::Inside,
                );
            }
            if resp.secondary_clicked() {
                action = Some(MediaListAction::ToggleSelect(*id));
            } else if resp.clicked() {
                action = Some(MediaListAction::OpenItem(*id));
            }
        }
    }
    action
}

fn render_file_row(
    ui: &mut Ui,
    item: &MediaItem,
    selected_ids: &HashSet<i32>,
    colors: &dyn Colors,
) -> Option<MediaListAction> {
    let MediaItem::File {
        id,
        filename,
        size_bytes,
        ext,
        date_label,
        ..
    } = item
    else {
        return None;
    };
    let width = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 56.0), Sense::click());
    if selected_ids.contains(id) {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("lightButtonBgOver"));
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }

    let badge_rect = Rect::from_min_size(
        Pos2::new(rect.min.x + 16.0, rect.center().y - 19.0),
        Vec2::splat(38.0),
    );
    let badge_bg = match ext.as_str() {
        "PDF" => colors.color("settingsIconBg1"),
        "ZIP" => colors.color("settingsIconBg3"),
        "THM" => colors.color("settingsIconBg6"),
        _ => colors.color("windowBgActive"),
    };
    ui.painter()
        .rect_filled(badge_rect, CornerRadius::same(6), badge_bg);
    ui.painter().text(
        badge_rect.center(),
        Align2::CENTER_CENTER,
        ext,
        FontId::new(11.0, family_semibold()),
        Color32::WHITE,
    );

    let text_x = badge_rect.max.x + 12.0;
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 10.0),
        Align2::LEFT_TOP,
        filename,
        font_id_from_style(st::semibold_font),
        colors.color("windowBoldFg"),
    );
    let meta = format!("{} · {date_label}", format_file_size(*size_bytes));
    ui.painter().text(
        Pos2::new(text_x, rect.min.y + 30.0),
        Align2::LEFT_TOP,
        meta,
        FontId::new(12.0, family_regular()),
        colors.color("windowSubTextFg"),
    );

    if resp.secondary_clicked() {
        Some(MediaListAction::ToggleSelect(*id))
    } else if resp.clicked() {
        Some(MediaListAction::OpenItem(*id))
    } else {
        None
    }
}

fn render_link_row(
    ui: &mut Ui,
    item: &MediaItem,
    selected_ids: &HashSet<i32>,
    colors: &dyn Colors,
) -> Option<MediaListAction> {
    let MediaItem::Link {
        id,
        url,
        title,
        description,
        domain,
        ..
    } = item
    else {
        return None;
    };
    let width = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 66.0), Sense::click());
    if selected_ids.contains(id) {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("lightButtonBgOver"));
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
    }

    let tile_rect = Rect::from_min_size(
        Pos2::new(rect.min.x + 16.0, rect.min.y + 12.0),
        Vec2::splat(38.0),
    );
    ui.painter().rect_filled(
        tile_rect,
        CornerRadius::same(6),
        colors.color("windowBgOver"),
    );
    let initial = domain
        .chars()
        .next()
        .unwrap_or('L')
        .to_uppercase()
        .to_string();
    ui.painter().text(
        tile_rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::new(16.0, family_semibold()),
        colors.color("windowActiveTextFg"),
    );

    let text_x = tile_rect.max.x + 12.0;
    let clip = Rect::from_min_max(
        Pos2::new(text_x, rect.min.y),
        Pos2::new(rect.max.x - 14.0, rect.max.y),
    );
    let p = ui.painter().with_clip_rect(clip);
    p.text(
        Pos2::new(text_x, rect.min.y + 7.0),
        Align2::LEFT_TOP,
        title,
        FontId::new(13.0, family_semibold()),
        colors.color("windowBoldFg"),
    );
    p.text(
        Pos2::new(text_x, rect.min.y + 25.0),
        Align2::LEFT_TOP,
        description,
        FontId::new(11.5, family_regular()),
        colors.color("windowSubTextFg"),
    );
    p.text(
        Pos2::new(text_x, rect.min.y + 43.0),
        Align2::LEFT_TOP,
        url,
        FontId::new(11.5, family_regular()),
        colors.color("windowActiveTextFg"),
    );

    if resp.secondary_clicked() {
        Some(MediaListAction::ToggleSelect(*id))
    } else if resp.clicked() {
        Some(MediaListAction::OpenItem(*id))
    } else {
        None
    }
}

fn paint_play_or_pause(
    painter: &egui::Painter,
    center: Pos2,
    playing: bool,
    colors: &dyn Colors,
    color: Color32,
) {
    let icon = if playing {
        &st::history_file_in_pause
    } else {
        &st::history_file_in_play
    };
    let rect = Rect::from_center_size(center, Vec2::splat(16.0));
    paint_style_icon(painter, rect, *icon, &ColorsResolver(colors), Some(color));
}
