//! Global Media tab section (`Info::GlobalMedia`) aggregating shared photos, videos, files,
//! links, voice messages, music, and GIFs across all chats via `messages.searchGlobal`.
//!
//! Port origin: `Telegram/SourceFiles/info/global_media/info_global_media_widget.cpp`,
//! `Telegram/SourceFiles/info/global_media/info_global_media_inner_widget.cpp`,
//! and `Telegram/SourceFiles/info/global_media/info_global_media_provider.cpp`.

use std::collections::HashSet;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Role, Sense, Stroke, Ui, Vec2,
    WidgetInfo,
};
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::stand_in::{Colors, InputFieldStyle};
use tg_ui::widgets::input_field::InputField;

use crate::info::backend::{MediaItem, SharedMediaTab};
use crate::info::media::MediaListWidget;
use crate::info::values::{format_duration, format_file_size};
use tg_app_core::stand_in::types::{MsgId, PeerId};

pub use tg_app_core::core_types::dto::global_media::*;

/// Renders the Global Media Info section (`Info::GlobalMedia::Widget`).
pub struct GlobalMediaSection;

impl GlobalMediaSection {
    /// Renders the 6-tab media filter strip, search box, date-grouped global media entries,
    /// and optional pagination button.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        active_tab: &mut SharedMediaTab,
        search_query: &mut String,
        page: &GlobalMediaPage,
        selected: &HashSet<(PeerId, i32)>,
        colors: &dyn Colors,
    ) -> Option<GlobalMediaAction> {
        let mut action = None;

        // 1. 6-tab media type bar (`Media | Files | Links | Voice | Music | GIFs`).
        let prev_tab = *active_tab;
        MediaListWidget::show_tabs(ui, "global_media_tabs", active_tab, colors);
        if *active_tab != prev_tab {
            action = Some(GlobalMediaAction::TabChanged(*active_tab));
        }

        // 2. Search input across all chats.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let input_w = (ui.available_width() - 14.0).max(120.0);
            ui.allocate_ui(Vec2::new(input_w, 34.0), |ui| {
                let hint = format!("Search all {}", active_tab.label().to_lowercase());
                InputField::single_line(
                    "info_global_media_search",
                    search_query,
                    &hint,
                    InputFieldStyle::default_input(colors),
                )
                .show(ui);
            });
        });

        // 3. Summary bar.
        if !page.entries.is_empty() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let total_display = page.total_count.max(page.entries.len());
                ui.label(
                    egui::RichText::new(format!("{} item(s) across all chats", total_display))
                        .font(FontId::new(11.5, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                );
            });
        }

        if page.entries.is_empty() {
            ui.add_space(36.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(format!("No global {}", active_tab.label().to_lowercase()))
                        .font(FontId::new(14.0, family_semibold()))
                        .color(colors.color("windowBoldFg")),
                );
                ui.add_space(4.0);
                let hint = if search_query.trim().is_empty() {
                    "Shared media from all your chats will appear here.".to_owned()
                } else {
                    format!("No matches for \"{}\".", search_query.trim())
                };
                ui.label(
                    egui::RichText::new(hint)
                        .font(FontId::new(12.5, family_regular()))
                        .color(colors.color("windowSubTextFg")),
                );
            });
            return action;
        }

        // 4. Date-grouped entries (grid for Photos/Videos/GIFs, rows for Files/Links/Voice/Music).
        let is_grid = matches!(
            *active_tab,
            SharedMediaTab::PhotosVideos | SharedMediaTab::Gifs
        );
        let mut current_group: Option<&str> = None;
        let mut group_buf: Vec<&GlobalMediaEntry> = Vec::new();

        for entry in &page.entries {
            let group = entry.item.date_group();
            if current_group != Some(group) {
                if let Some(prev) = current_group {
                    if let Some(ev) =
                        render_group(ui, prev, &group_buf, *active_tab, is_grid, selected, colors)
                    {
                        action = Some(ev);
                    }
                    group_buf.clear();
                }
                current_group = Some(group);
            }
            group_buf.push(entry);
        }
        if let Some(last_group) = current_group
            && !group_buf.is_empty()
            && let Some(ev) = render_group(
                ui,
                last_group,
                &group_buf,
                *active_tab,
                is_grid,
                selected,
                colors,
            )
        {
            action = Some(ev);
        }

        // 5. Pagination footer.
        if page.has_more {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                let btn = ui.add(
                    egui::Button::new(
                        egui::RichText::new("Load more media")
                            .font(FontId::new(12.5, family_semibold()))
                            .color(colors.color("windowActiveTextFg")),
                    )
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::new(1.0, colors.color("shadowFg")))
                    .corner_radius(CornerRadius::same(6)),
                );
                if btn.clicked() {
                    action = Some(GlobalMediaAction::LoadMore);
                }
            });
            ui.add_space(8.0);
        }

        action
    }
}

fn render_group(
    ui: &mut Ui,
    header: &str,
    entries: &[&GlobalMediaEntry],
    tab: SharedMediaTab,
    is_grid: bool,
    selected: &HashSet<(PeerId, i32)>,
    colors: &dyn Colors,
) -> Option<GlobalMediaAction> {
    let mut action = None;

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(
            egui::RichText::new(header)
                .font(FontId::new(12.5, family_semibold()))
                .color(colors.color("windowSubTextFg")),
        );
    });
    ui.add_space(6.0);

    if is_grid {
        let avail = (ui.available_width() - 28.0).max(160.0);
        let cols = 3usize;
        let gap = 4.0;
        let tile = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).max(56.0);

        for chunk in entries.chunks(cols) {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.spacing_mut().item_spacing.x = gap;
                for entry in chunk {
                    if let Some(ev) = render_grid_tile(ui, entry, tab, tile, selected, colors) {
                        action = Some(ev);
                    }
                }
            });
            ui.add_space(gap);
        }
    } else {
        for entry in entries {
            if let Some(ev) = render_list_row(ui, entry, tab, selected, colors) {
                action = Some(ev);
            }
        }
    }

    action
}

fn render_grid_tile(
    ui: &mut Ui,
    entry: &GlobalMediaEntry,
    tab: SharedMediaTab,
    side: f32,
    selected: &HashSet<(PeerId, i32)>,
    colors: &dyn Colors,
) -> Option<GlobalMediaAction> {
    let key = entry.full_key();
    let is_sel = selected.contains(&key);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    resp.widget_info(|| {
        WidgetInfo::labeled(
            Role::Button,
            true,
            format!("{} from {}", tab.label(), entry.peer_title),
        )
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let (rgb, badge) = match &entry.item {
        MediaItem::Photo { preview_rgb, .. } => (*preview_rgb, None),
        MediaItem::Video {
            preview_rgb,
            duration_secs,
            ..
        } => (*preview_rgb, Some(format_duration(*duration_secs))),
        MediaItem::Gif { preview_rgb, .. } => (*preview_rgb, Some("GIF".to_owned())),
        _ => ([0x4E, 0x7A, 0xA8], None),
    };
    let fill = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);

    // Top-left duration / GIF badge.
    if let Some(tag) = badge {
        let pill = Rect::from_min_size(
            Pos2::new(rect.left() + 4.0, rect.top() + 4.0),
            Vec2::new(38.0, 16.0),
        );
        ui.painter().rect_filled(
            pill,
            CornerRadius::same(4),
            Color32::from_rgba_unmultiplied(0, 0, 0, 145),
        );
        ui.painter().text(
            pill.center(),
            Align2::CENTER_CENTER,
            tag,
            FontId::new(10.0, family_semibold()),
            Color32::WHITE,
        );
    }

    // Bottom source-chat caption strip.
    let strip = Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - 18.0), rect.max);
    ui.painter().rect_filled(
        strip,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: 4,
            se: 4,
        },
        Color32::from_rgba_unmultiplied(0, 0, 0, 130),
    );
    let short_peer: String = entry.peer_title.chars().take(12).collect();
    ui.painter().text(
        Pos2::new(strip.left() + 4.0, strip.center().y),
        Align2::LEFT_CENTER,
        short_peer,
        FontId::new(9.5, family_regular()),
        Color32::WHITE,
    );

    if is_sel {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(2.5, colors.color("windowBgActive")),
            egui::StrokeKind::Inside,
        );
    }

    if resp.secondary_clicked() {
        return Some(GlobalMediaAction::ToggleSelect {
            peer_id: entry.peer_id,
            item_id: entry.item.id(),
        });
    }
    if resp.clicked() {
        return Some(GlobalMediaAction::OpenMedia {
            peer_id: entry.peer_id,
            item_id: entry.item.id(),
            tab,
        });
    }
    None
}

fn render_list_row(
    ui: &mut Ui,
    entry: &GlobalMediaEntry,
    tab: SharedMediaTab,
    selected: &HashSet<(PeerId, i32)>,
    colors: &dyn Colors,
) -> Option<GlobalMediaAction> {
    let key = entry.full_key();
    let is_sel = selected.contains(&key);
    let width = ui.available_width().max(180.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 58.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let bg = if is_sel {
        Color32::from_rgba_unmultiplied(
            colors.color("windowBgActive").r(),
            colors.color("windowBgActive").g(),
            colors.color("windowBgActive").b(),
            28,
        )
    } else if resp.hovered() {
        colors.color("windowBgOver")
    } else {
        colors.color("windowBg")
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, bg);

    let (badge, title, subtitle) = match &entry.item {
        MediaItem::File {
            filename,
            size_bytes,
            ext,
            date_label,
            ..
        } => (
            ext.clone(),
            filename.clone(),
            format!(
                "{} · {} · {}",
                format_file_size(*size_bytes),
                date_label,
                entry.peer_title
            ),
        ),
        MediaItem::Link {
            title, domain, url, ..
        } => (
            "URL".to_owned(),
            if title.is_empty() {
                url.clone()
            } else {
                title.clone()
            },
            format!("{domain} · {}", entry.peer_title),
        ),
        MediaItem::Voice {
            sender,
            duration_secs,
            date_label,
            ..
        } => (
            "▶".to_owned(),
            sender.clone(),
            format!(
                "{} · {} · {}",
                format_duration(*duration_secs),
                date_label,
                entry.peer_title
            ),
        ),
        MediaItem::Music {
            title,
            performer,
            duration_secs,
            size_bytes,
            ..
        } => (
            "♪".to_owned(),
            title.clone(),
            format!(
                "{performer} · {} · {} · {}",
                format_duration(*duration_secs),
                format_file_size(*size_bytes),
                entry.peer_title
            ),
        ),
        MediaItem::Photo { caption, .. } => (
            "IMG".to_owned(),
            if caption.is_empty() {
                "Photo".to_owned()
            } else {
                caption.clone()
            },
            entry.peer_title.clone(),
        ),
        MediaItem::Video {
            caption,
            duration_secs,
            ..
        } => (
            "VID".to_owned(),
            if caption.is_empty() {
                "Video".to_owned()
            } else {
                caption.clone()
            },
            format!("{} · {}", format_duration(*duration_secs), entry.peer_title),
        ),
        MediaItem::Gif { label, .. } => ("GIF".to_owned(), label.clone(), entry.peer_title.clone()),
    };

    // Left badge circle / rounded square.
    let icon_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 14.0, rect.top() + 9.0),
        Vec2::splat(40.0),
    );
    ui.painter().rect_filled(
        icon_rect,
        CornerRadius::same(10),
        colors.color("windowBgActive"),
    );
    match &entry.item {
        MediaItem::Voice { .. } => {
            tg_app_core::ui_icons::paint_triangle(
                ui.painter(),
                icon_rect.center() + Vec2::new(1.5, 0.0),
                16.0,
                tg_app_core::ui_icons::ArrowDir::Right,
                Color32::WHITE,
            );
        }
        MediaItem::Music { .. } => {
            tg_app_core::ui_icons::paint_style_tinted(
                ui.painter(),
                Rect::from_center_size(icon_rect.center(), Vec2::splat(20.0)),
                tg_ui::tg_style::st::menu_icon_sound_select,
                Color32::WHITE,
            );
        }
        _ => {
            ui.painter().text(
                icon_rect.center(),
                Align2::CENTER_CENTER,
                badge,
                FontId::new(11.0, family_semibold()),
                Color32::WHITE,
            );
        }
    }

    let text_x = icon_rect.right() + 12.0;
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 12.0),
        Align2::LEFT_TOP,
        title,
        FontId::new(13.5, family_semibold()),
        colors.color("windowBoldFg"),
    );
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 32.0),
        Align2::LEFT_TOP,
        subtitle,
        FontId::new(11.5, family_regular()),
        colors.color("windowSubTextFg"),
    );

    // Right-side "Jump" pill to open the source chat message directly.
    let jump_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 56.0, rect.center().y - 12.0),
        Vec2::new(44.0, 24.0),
    );
    let jump_resp = ui.interact(
        jump_rect,
        ui.make_persistent_id(("gm_jump", entry.peer_id.0, entry.item.id())),
        Sense::click(),
    );
    if jump_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            jump_rect,
            CornerRadius::same(6),
            colors.color("windowBgActive"),
        );
        ui.painter().text(
            jump_rect.center(),
            Align2::CENTER_CENTER,
            "Chat",
            FontId::new(11.0, family_semibold()),
            Color32::WHITE,
        );
    } else {
        ui.painter().text(
            jump_rect.center(),
            Align2::CENTER_CENTER,
            "Chat",
            FontId::new(11.0, family_regular()),
            colors.color("windowActiveTextFg"),
        );
    }
    if jump_resp.clicked() {
        return Some(GlobalMediaAction::OpenSourceChat {
            peer_id: entry.peer_id,
            msg_id: MsgId(entry.item.id()),
        });
    }

    if resp.secondary_clicked() {
        return Some(GlobalMediaAction::ToggleSelect {
            peer_id: entry.peer_id,
            item_id: entry.item.id(),
        });
    }
    if resp.clicked() {
        return Some(GlobalMediaAction::OpenMedia {
            peer_id: entry.peer_id,
            item_id: entry.item.id(),
            tab,
        });
    }
    None
}
