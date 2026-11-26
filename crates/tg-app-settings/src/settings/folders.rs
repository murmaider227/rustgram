//! Chat Folders (dialog filters) settings section, `EditFilterBox`, `FilterIconPanel`,
//! folder color tags toggle, and shareable multi-link invite manager integration.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_folders.cpp`,
//! `Telegram/SourceFiles/boxes/filters/edit_filter_box.cpp`,
//! `Telegram/SourceFiles/ui/filter_icon_panel.cpp`,
//! and `Telegram/SourceFiles/boxes/filters/edit_filter_links.cpp`.

use egui::{Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use tg_core::CoreSettings;
use tg_core::settings::ChatsFiltersTabsMode;
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::{Icon, st};
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use super::folder_links::{
    FolderLinksAction, FolderLinksBoxState, folder_tag_color32, paint_folder_color_swatch,
};

use super::backend::{
    ChatFolder, FOLDER_TAG_COLORS, FolderIcon, MAX_FOLDER_TITLE_LEN, SettingsBackend,
};
use super::strings;
use super::widgets::{
    paint_tinted_icon, settings_card, settings_checkbox_row, settings_divider, settings_radio_row,
    settings_row_pad_x, settings_toggle_row, subsection_title,
};

/// Number of icons per row in `Ui::FilterIconPanel` (`kIconsPerRow = 6`).
pub const FILTER_ICON_PANEL_COLUMNS: usize = 6;

/// Persistent UI state for the Chat Folders section.
#[derive(Debug, Clone, Default)]
pub struct FoldersState {
    /// Draft folder currently being created or edited (`EditFilterBox`).
    pub editing: Option<ChatFolder>,
    /// Validation error shown in `EditFilterBox`.
    pub editor_error: Option<String>,
    /// Whether the custom folder icon picker panel (`Ui::FilterIconPanel`) is open.
    pub icon_panel_open: bool,
    /// Active multi-link invite manager dialog (`EditFilterLinksBox`).
    pub links_box: Option<FolderLinksBoxState>,
    /// Last deleted folder held for quick Undo restoration.
    pub last_deleted: Option<ChatFolder>,
    /// Most recently exported shareable folder link `(folder_id, url)`.
    pub last_exported_link: Option<(i32, String)>,
}

/// Output emitted by [`render_folders_section`].
#[derive(Debug, Clone, Default)]
pub struct FoldersOutput {
    /// Vertical sidebar vs horizontal tabs layout toggled.
    pub toggled_folders_sidebar: bool,
    /// Optional toast message.
    pub toast: Option<String>,
}

/// Maps a [`FolderIcon`] to its real Telegram Desktop `tg_style::Icon` from `ui/filter_icons.style`.
#[must_use]
pub fn folder_style_icon(icon: FolderIcon) -> Icon {
    match icon {
        FolderIcon::Custom => st::folders_custom,
        FolderIcon::All => st::folders_all,
        FolderIcon::Unread => st::folders_unread,
        FolderIcon::Unmuted => st::folders_unmuted,
        FolderIcon::Bots => st::folders_bots,
        FolderIcon::Channels => st::folders_channels,
        FolderIcon::Groups => st::folders_groups,
        FolderIcon::Private => st::folders_private,
        FolderIcon::Work => st::folders_work,
        FolderIcon::Star => st::folders_favorite,
        FolderIcon::Book => st::folders_book,
        FolderIcon::Game => st::folders_game,
        FolderIcon::Cat => st::folders_cat,
        FolderIcon::Money => st::folders_money,
        FolderIcon::Light => st::folders_light,
        FolderIcon::Like => st::folders_like,
        FolderIcon::Note => st::folders_note,
        FolderIcon::Palette => st::folders_palette,
        FolderIcon::Travel => st::folders_travel,
        FolderIcon::Sport => st::folders_sport,
        FolderIcon::Study => st::folders_study,
        FolderIcon::Airplane => st::folders_airplane,
        FolderIcon::Crown => st::folders_crown,
        FolderIcon::Flower => st::folders_flower,
        FolderIcon::Home => st::folders_home,
        FolderIcon::Love => st::folders_love,
        FolderIcon::Mask => st::folders_mask,
        FolderIcon::Party => st::folders_party,
        FolderIcon::Trade => st::folders_trade,
        FolderIcon::Setup => st::folders_setup,
    }
}

/// Renders the Chat Folders (`settings_folders.cpp`) section.
pub fn render_folders_section(
    ui: &mut Ui,
    state: &mut FoldersState,
    core: &mut CoreSettings,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> FoldersOutput {
    let mut out = FoldersOutput::default();
    let pad_x = settings_row_pad_x();
    let folders = backend.folders();

    // Intro banner + Create Folder button.
    ui.add_space(10.0);
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(strings::FOLDERS_ORGANIZE_TITLE)
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(tr::lng_filters_about())
                .font(font_id_from_style(st::normal_font))
                .color(colors.color("windowSubTextFg")),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if RoundButton::new(
                &tr::lng_filters_create(),
                RoundButtonStyle::default_active(colors),
            )
            .show(ui)
            .clicked()
            {
                state.editing = Some(ChatFolder::default());
                state.editor_error = None;
            }
            if let Some(deleted) = state.last_deleted.clone() {
                ui.add_space(8.0);
                let restore_lbl = format!("{} '{}'", strings::STICKERS_RESTORE_BTN, deleted.title);
                if RoundButton::new(&restore_lbl, RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    let mut restored = deleted;
                    restored.id = 0;
                    if backend.save_folder(restored).is_ok() {
                        state.last_deleted = None;
                        out.toast = Some(strings::FOLDERS_RESTORED_TOAST.to_owned());
                    }
                }
            }
        });
    });

    // Inline EditFilterBox when creating or editing a folder.
    if state.editing.is_some() {
        ui.add_space(8.0);
        render_edit_filter_box(ui, state, backend, colors, &mut out);
    }

    settings_divider(ui, None, colors);

    // My Folders list with reorder & delete controls.
    subsection_title(ui, strings::FOLDERS_MY_FOLDERS, colors);
    if folders.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(pad_x);
            ui.label(
                egui::RichText::new(strings::FOLDERS_EMPTY)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        });
    } else {
        let mut reorder_swap: Option<(usize, usize)> = None;
        let mut delete_target: Option<ChatFolder> = None;
        let mut export_target: Option<ChatFolder> = None;
        let mut links_target: Option<ChatFolder> = None;
        let len = folders.len();

        for (idx, folder) in folders.iter().enumerate() {
            let w = ui.available_width().max(240.0);
            let h = 56.0;
            let (rect, row_hover) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
            if row_hover.hovered() {
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBgOver"));
            }

            // Real tdesktop folder icon inside rounded colored tile (reflecting tag color if set).
            let icon_rect = Rect::from_min_size(
                Pos2::new(rect.left() + 18.0, rect.center().y - 16.0),
                Vec2::splat(32.0),
            );
            let tile_bg = folder
                .color_index
                .map(folder_tag_color32)
                .unwrap_or_else(|| colors.color("windowBgActive"));
            ui.painter().rect_filled(
                icon_rect,
                CornerRadius::same(st::settings_icon_radius.scaled().clamp(4, 16) as u8),
                tile_bg,
            );
            paint_tinted_icon(
                ui.painter(),
                icon_rect.shrink(4.0),
                folder_style_icon(folder.icon),
                colors,
                Some(Color32::WHITE),
            );

            // Title & summary.
            let text_x = icon_rect.right() + 12.0;
            ui.painter().text(
                Pos2::new(text_x, rect.top() + 10.0),
                Align2::LEFT_TOP,
                &folder.title,
                font_id_from_style(st::semibold_font),
                colors.color("windowBoldFg"),
            );
            ui.painter().text(
                Pos2::new(text_x, rect.top() + 30.0),
                Align2::LEFT_TOP,
                folder.summary(),
                egui::FontId::new(12.0, tg_ui::fonts::family_regular()),
                colors.color("windowSubTextFg"),
            );

            // Inline icon action buttons on the right (Links / Export Invite Link / Up / Down / Delete).
            let mut btn_x = rect.right() - pad_x;
            let del_rect = Rect::from_min_size(
                Pos2::new(btn_x - 28.0, rect.center().y - 14.0),
                Vec2::splat(28.0),
            );
            let del_resp = ui.interact(
                del_rect,
                ui.scope_id().with(("folder_del", folder.id)),
                Sense::click(),
            );
            if del_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().circle_filled(
                    del_rect.center(),
                    14.0,
                    colors.color("attentionButtonBgOver"),
                );
            }
            paint_tinted_icon(
                ui.painter(),
                del_rect.shrink(4.0),
                st::menu_icon_delete_attention,
                colors,
                Some(colors.color("attentionButtonFg")),
            );
            if del_resp
                .on_hover_text(tr::lng_filters_remove_yes())
                .clicked()
            {
                delete_target = Some(folder.clone());
            }
            btn_x -= 34.0;

            let links_rect = Rect::from_min_size(
                Pos2::new(btn_x - 26.0, rect.center().y - 13.0),
                Vec2::splat(26.0),
            );
            let links_resp = ui.interact(
                links_rect,
                ui.scope_id().with(("folder_links", folder.id)),
                Sense::click(),
            );
            let links_a11y = format!("Manage Invite Links: {}", folder.title);
            links_resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, links_a11y.clone())
            });
            if links_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().circle_filled(
                    links_rect.center(),
                    13.0,
                    colors.color("lightButtonBgOver"),
                );
            }
            paint_tinted_icon(
                ui.painter(),
                links_rect.shrink(4.0),
                st::menu_icon_manage,
                colors,
                Some(colors.color("windowActiveTextFg")),
            );
            if links_resp.on_hover_text("Manage Invite Links").clicked() {
                links_target = Some(folder.clone());
            }
            btn_x -= 30.0;

            let exp_rect = Rect::from_min_size(
                Pos2::new(btn_x - 26.0, rect.center().y - 13.0),
                Vec2::splat(26.0),
            );
            let exp_resp = ui.interact(
                exp_rect,
                ui.scope_id().with(("folder_export_link", folder.id)),
                Sense::click(),
            );
            let exp_a11y = format!("Export Invite Link: {}", folder.title);
            exp_resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, exp_a11y.clone())
            });
            if exp_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .circle_filled(exp_rect.center(), 13.0, colors.color("windowBgOver"));
            }
            paint_tinted_icon(
                ui.painter(),
                exp_rect.shrink(3.0),
                st::menu_icon_link,
                colors,
                Some(colors.color("windowActiveTextFg")),
            );
            if exp_resp.on_hover_text("Export Invite Link").clicked() {
                export_target = Some(folder.clone());
            }
            btn_x -= 30.0;

            if idx + 1 < len {
                let dn_rect = Rect::from_min_size(
                    Pos2::new(btn_x - 24.0, rect.center().y - 12.0),
                    Vec2::splat(24.0),
                );
                let dn_resp = ui.interact(
                    dn_rect,
                    ui.scope_id().with(("folder_dn", folder.id)),
                    Sense::click(),
                );
                if dn_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                paint_tinted_icon(
                    ui.painter(),
                    dn_rect.shrink(3.0),
                    st::menu_icon_expand,
                    colors,
                    Some(colors.color("windowSubTextFg")),
                );
                if dn_resp.clicked() {
                    reorder_swap = Some((idx, idx + 1));
                }
                btn_x -= 28.0;
            }
            if idx > 0 {
                let up_rect = Rect::from_min_size(
                    Pos2::new(btn_x - 24.0, rect.center().y - 12.0),
                    Vec2::splat(24.0),
                );
                let up_resp = ui.interact(
                    up_rect,
                    ui.scope_id().with(("folder_up", folder.id)),
                    Sense::click(),
                );
                if up_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                paint_tinted_icon(
                    ui.painter(),
                    up_rect.shrink(3.0),
                    st::menu_icon_collapse,
                    colors,
                    Some(colors.color("windowSubTextFg")),
                );
                if up_resp.clicked() {
                    reorder_swap = Some((idx, idx - 1));
                }
                btn_x -= 28.0;
            }

            // Clicking the non-button row body opens EditFilterBox without stealing clicks from inline action buttons.
            let body_rect = Rect::from_min_max(
                rect.min,
                Pos2::new(btn_x.max(rect.left() + 64.0), rect.bottom()),
            );
            let body_resp = ui.interact(
                body_rect,
                ui.scope_id().with(("folder_body", folder.id)),
                Sense::click(),
            );
            let body_a11y = format!("Edit Folder: {}", folder.title);
            body_resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, body_a11y.clone())
            });
            if body_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if body_resp.clicked()
                && delete_target.is_none()
                && reorder_swap.is_none()
                && export_target.is_none()
                && links_target.is_none()
            {
                state.editing = Some(folder.clone());
                state.editor_error = None;
                state.icon_panel_open = false;
            }
        }

        if let Some(target) = export_target {
            match backend.export_folder_invite_link(
                target.id,
                &target.title,
                &target.included_peers,
            ) {
                Ok(url) => {
                    ui.ctx().copy_text(url.clone());
                    state.last_exported_link = Some((target.id, url.clone()));
                    out.toast = Some(format!("Folder invite link copied: {url}"));
                }
                Err(err) => {
                    out.toast = tg_app_core::user_error::export_chatlist_invite(&err).into_text();
                }
            }
        }

        if let Some((_, ref url)) = state.last_exported_link {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                ui.label(
                    egui::RichText::new(format!("Invite link: {url}"))
                        .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                        .color(colors.color("windowActiveTextFg")),
                );
            });
        }

        if let Some(target) = links_target {
            state.links_box = Some(FolderLinksBoxState::new(
                target.id,
                target.title,
                target.shareable,
                target.included_peers,
            ));
        }

        if let Some((a, b)) = reorder_swap {
            let mut ids: Vec<i32> = folders.iter().map(|f| f.id).collect();
            ids.swap(a, b);
            backend.reorder_folders(&ids);
        }
        if let Some(target) = delete_target
            && backend.delete_folder(target.id)
        {
            state.last_deleted = Some(target.clone());
            if state.editing.as_ref().is_some_and(|e| e.id == target.id) {
                state.editing = None;
            }
            out.toast = Some(format!(
                "{}: '{}'",
                tr::lng_filters_remove_yes(),
                target.title
            ));
        }
    }

    // Recommended / Suggested Folders.
    let suggested = backend.suggested_folders();
    if !suggested.is_empty() {
        settings_divider(ui, None, colors);
        subsection_title(ui, &tr::lng_filters_recommended(), colors);
        let mut accept_idx = None;
        let add_lbl = tr::lng_filters_recommended_add();
        for (idx, item) in suggested.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add_space(pad_x);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(&item.folder.title)
                            .font(font_id_from_style(st::semibold_font))
                            .color(colors.color("windowBoldFg")),
                    );
                    ui.label(
                        egui::RichText::new(&item.description)
                            .font(egui::FontId::new(12.0, tg_ui::fonts::family_regular()))
                            .color(colors.color("windowSubTextFg")),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(pad_x);
                    if RoundButton::new(&add_lbl, RoundButtonStyle::default_active(colors))
                        .full_radius(true)
                        .show(ui)
                        .clicked()
                    {
                        accept_idx = Some(idx);
                    }
                });
            });
            ui.add_space(6.0);
        }
        if let Some(idx) = accept_idx
            && let Some(added) = backend.accept_suggested_folder(idx)
        {
            out.toast = Some(format!("{add_lbl}: '{}'", added.title));
        }
    }

    settings_divider(ui, None, colors);

    // Folder color tags toggle (`messages.toggleDialogFilterTags`).
    let mut tags_enabled = backend.folder_tags_enabled();
    if settings_toggle_row(
        ui,
        "Show Folder Tags",
        Some("Display colored folder name tags on chats in your chat list"),
        None,
        &mut tags_enabled,
        true,
        colors,
    ) {
        match backend.set_folder_tags_enabled(tags_enabled) {
            Ok(_) => {
                out.toast = Some(if tags_enabled {
                    "Folder tags enabled".to_owned()
                } else {
                    "Folder tags disabled".to_owned()
                });
            }
            Err(err) => {
                out.toast = tg_app_core::user_error::toggle_folder_tags(&err).into_text();
            }
        }
    }

    settings_divider(ui, None, colors);

    // Folder Tabs layout preferences (`CoreSettings::chat_filters_horizontal` & `chat_filters_tabs_mode`).
    subsection_title(ui, &tr::lng_filters_view_subtitle(), colors);
    let mut top_bar = core.chat_filters_horizontal;
    if settings_toggle_row(
        ui,
        strings::FOLDERS_TABS_ON_TOP,
        Some(strings::FOLDERS_TABS_ON_TOP_SUBTITLE),
        None,
        &mut top_bar,
        true,
        colors,
    ) {
        core.chat_filters_horizontal = top_bar;
        out.toggled_folders_sidebar = true;
    }

    ui.add_space(4.0);
    settings_radio_row(
        ui,
        &mut core.chat_filters_tabs_mode,
        ChatsFiltersTabsMode::Default,
        &tr::lng_filters_tabs_default(),
        true,
        colors,
    );
    settings_radio_row(
        ui,
        &mut core.chat_filters_tabs_mode,
        ChatsFiltersTabsMode::TextAndIcons,
        &tr::lng_filters_tabs_text_icons(),
        true,
        colors,
    );
    settings_radio_row(
        ui,
        &mut core.chat_filters_tabs_mode,
        ChatsFiltersTabsMode::IconsOnly,
        &tr::lng_filters_tabs_icons(),
        true,
        colors,
    );
    settings_radio_row(
        ui,
        &mut core.chat_filters_tabs_mode,
        ChatsFiltersTabsMode::TextOnly,
        &tr::lng_filters_tabs_text(),
        true,
        colors,
    );

    // Multi-link invite manager modal overlay (`boxes/folder_links.rs`).
    let mut close_links = false;
    if let Some(ref mut links_box) = state.links_box
        && let Some(act) = links_box.show_modal(ui, backend, colors)
    {
        match act {
            FolderLinksAction::Close => close_links = true,
            FolderLinksAction::Toast(msg) => out.toast = Some(msg),
        }
    }
    if close_links {
        state.links_box = None;
    }

    ui.add_space(16.0);
    out
}

fn render_edit_filter_box(
    ui: &mut Ui,
    state: &mut FoldersState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    out: &mut FoldersOutput,
) {
    let contacts = backend.contacts();
    let mut close = false;
    let mut save_candidate: Option<ChatFolder> = None;

    if let Some(draft) = &mut state.editing {
        settings_card(ui, colors, |ui| {
            let heading = if draft.id == 0 {
                tr::lng_filters_new()
            } else {
                tr::lng_filters_edit()
            };
            ui.label(
                egui::RichText::new(heading)
                    .font(font_id_from_style(st::box_title_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                // Current icon button that opens/toggles `FilterIconPanel` (`ui/filter_icon_panel.cpp`).
                let (icon_btn_rect, icon_btn_resp) =
                    ui.allocate_exact_size(Vec2::splat(36.0), Sense::click());
                icon_btn_resp.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::Role::Button,
                        true,
                        format!("Choose Folder Icon ({})", draft.icon.label()),
                    )
                });
                if icon_btn_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let tile_bg = if state.icon_panel_open {
                    colors.color("windowBgActive")
                } else if icon_btn_resp.hovered() {
                    colors.color("windowBgOver")
                } else {
                    colors.color("lightButtonBgOver")
                };
                ui.painter()
                    .rect_filled(icon_btn_rect, CornerRadius::same(8), tile_bg);
                let icon_tint = if state.icon_panel_open {
                    Color32::WHITE
                } else {
                    colors.color("windowActiveTextFg")
                };
                paint_tinted_icon(
                    ui.painter(),
                    icon_btn_rect.shrink(6.0),
                    folder_style_icon(draft.icon),
                    colors,
                    Some(icon_tint),
                );
                if icon_btn_resp.on_hover_text("Choose Folder Icon").clicked() {
                    state.icon_panel_open = !state.icon_panel_open;
                }

                ui.add_space(6.0);
                InputField::single_line(
                    "folder_edit_title",
                    &mut draft.title,
                    &tr::lng_filters_new_name(),
                    InputFieldStyle::default_input(colors),
                )
                .max_length(MAX_FOLDER_TITLE_LEN)
                .show(ui);
            });

            // `Ui::FilterIconPanel` (6-column x 5-row custom folder icon grid).
            if state.icon_panel_open {
                ui.add_space(6.0);
                if let Some(chosen) = render_filter_icon_panel(ui, draft.icon, colors) {
                    draft.icon = chosen;
                    state.icon_panel_open = false;
                }
            }

            // Folder Tag Color swatch row (`0..7` + none).
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Tag Color")
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            ui.horizontal(|ui| {
                let (none_rect, none_resp) =
                    ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                if none_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                paint_folder_color_swatch(
                    ui.painter(),
                    none_rect,
                    None,
                    draft.color_index.is_none(),
                    colors.color("windowSubTextFg"),
                );
                if none_resp.on_hover_text("No tag color").clicked() {
                    draft.color_index = None;
                }
                for &(idx, _hex, name) in &FOLDER_TAG_COLORS {
                    let (sw_rect, sw_resp) =
                        ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                    if sw_resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    paint_folder_color_swatch(
                        ui.painter(),
                        sw_rect,
                        Some(folder_tag_color32(idx)),
                        draft.color_index == Some(idx),
                        colors.color("windowBoldFg"),
                    );
                    if sw_resp.on_hover_text(name).clicked() {
                        draft.color_index = Some(idx);
                    }
                }
            });

            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(tr::lng_filters_include())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            settings_checkbox_row(
                ui,
                &mut draft.include_contacts,
                &tr::lng_filters_type_contacts(),
                true,
                colors,
            );
            settings_checkbox_row(
                ui,
                &mut draft.include_non_contacts,
                &tr::lng_filters_type_non_contacts(),
                true,
                colors,
            );
            settings_checkbox_row(
                ui,
                &mut draft.include_groups,
                &tr::lng_filters_type_groups(),
                true,
                colors,
            );
            settings_checkbox_row(
                ui,
                &mut draft.include_channels,
                &tr::lng_filters_type_channels(),
                true,
                colors,
            );
            settings_checkbox_row(
                ui,
                &mut draft.include_bots,
                &tr::lng_filters_type_bots(),
                true,
                colors,
            );

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(tr::lng_filters_exclude())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            settings_checkbox_row(
                ui,
                &mut draft.exclude_muted,
                &tr::lng_filters_type_no_muted(),
                true,
                colors,
            );
            settings_checkbox_row(
                ui,
                &mut draft.exclude_read,
                &tr::lng_filters_type_no_read(),
                true,
                colors,
            );
            settings_checkbox_row(
                ui,
                &mut draft.exclude_archived,
                &tr::lng_filters_type_no_archived(),
                true,
                colors,
            );

            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(strings::FOLDERS_INCLUDED_CHATS_SPECIFIC)
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            ui.horizontal_wrapped(|ui| {
                for peer in &contacts {
                    let mut included = draft.included_peers.contains(&peer.id);
                    if ui.checkbox(&mut included, &peer.name).changed() {
                        if included {
                            draft.included_peers.push(peer.id);
                            draft.excluded_peers.retain(|&id| id != peer.id);
                        } else {
                            draft.included_peers.retain(|&id| id != peer.id);
                        }
                    }
                }
            });

            if let Some(err) = &state.editor_error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(err)
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("boxTextFgError")),
                );
            }

            ui.add_space(10.0);
            let mut export_current = false;
            let mut open_links_for_draft: Option<ChatFolder> = None;
            ui.horizontal(|ui| {
                let save_btn_lbl = if draft.id == 0 {
                    tr::lng_filters_create_button()
                } else {
                    tr::lng_settings_save()
                };
                if RoundButton::new(&save_btn_lbl, RoundButtonStyle::default_active(colors))
                    .show(ui)
                    .clicked()
                {
                    save_candidate = Some(draft.clone());
                }
                if draft.id > 0 {
                    ui.add_space(8.0);
                    if RoundButton::new(
                        "Export Invite Link",
                        RoundButtonStyle::default_light(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        export_current = true;
                    }
                    ui.add_space(8.0);
                    if RoundButton::new(
                        "Manage Invite Links",
                        RoundButtonStyle::default_light(colors),
                    )
                    .show(ui)
                    .clicked()
                    {
                        open_links_for_draft = Some(draft.clone());
                    }
                }
                ui.add_space(8.0);
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    close = true;
                }
            });
            if export_current {
                match backend.export_folder_invite_link(
                    draft.id,
                    &draft.title,
                    &draft.included_peers,
                ) {
                    Ok(url) => {
                        ui.ctx().copy_text(url.clone());
                        state.last_exported_link = Some((draft.id, url.clone()));
                        out.toast = Some(format!("Folder invite link copied: {url}"));
                    }
                    Err(err) => {
                        state.editor_error = Some(err);
                    }
                }
            }
            if let Some(target) = open_links_for_draft {
                state.links_box = Some(FolderLinksBoxState::new(
                    target.id,
                    target.title,
                    target.shareable,
                    target.included_peers,
                ));
            }
        });
    }

    if let Some(candidate) = save_candidate {
        match backend.save_folder(candidate) {
            Ok(saved) => {
                out.toast = Some(format!("{}: '{}'", tr::lng_settings_save(), saved.title));
                close = true;
            }
            Err(err) => state.editor_error = Some(err),
        }
    }

    if close {
        state.editing = None;
        state.editor_error = None;
        state.icon_panel_open = false;
    }
}

/// Renders Telegram Desktop's `Ui::FilterIconPanel` (6-column x 5-row custom folder icon grid).
///
/// Port origin: `Telegram/SourceFiles/ui/filter_icon_panel.cpp`.
pub fn render_filter_icon_panel(
    ui: &mut Ui,
    current: FolderIcon,
    colors: &dyn Colors,
) -> Option<FolderIcon> {
    let mut chosen = None;
    egui::Frame::NONE
        .fill(colors.color("dialogsBg"))
        .stroke(Stroke::new(1.0, colors.color("shadowFg")))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("Choose an Icon")
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(6.0);
            for row in FolderIcon::ALL.chunks(FILTER_ICON_PANEL_COLUMNS) {
                ui.horizontal(|ui| {
                    for &icon in row {
                        let selected = current == icon;
                        let (tile, resp) =
                            ui.allocate_exact_size(Vec2::splat(36.0), Sense::click());
                        resp.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::Role::Button,
                                true,
                                selected,
                                icon.label(),
                            )
                        });
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        let bg = if selected {
                            colors.color("windowBgActive")
                        } else if resp.hovered() {
                            colors.color("dialogsBgOver")
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.painter().rect_filled(
                            tile,
                            CornerRadius::same(
                                st::settings_icon_radius.scaled().clamp(4, 12) as u8,
                            ),
                            bg,
                        );
                        let tint = if selected {
                            Color32::WHITE
                        } else if resp.hovered() {
                            colors.color("windowBoldFg")
                        } else {
                            colors.color("windowSubTextFg")
                        };
                        paint_tinted_icon(
                            ui.painter(),
                            tile.shrink(6.0),
                            folder_style_icon(icon),
                            colors,
                            Some(tint),
                        );
                        if resp.on_hover_text(icon.label()).clicked() {
                            chosen = Some(icon);
                        }
                    }
                });
            }
        });
    chosen
}
