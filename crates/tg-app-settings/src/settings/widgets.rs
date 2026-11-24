//! Reusable Telegram Desktop Settings UI rows, headers, icons, and dividers built on `tg-ui`.
//!
//! Port origin: `Telegram/SourceFiles/settings/settings_common.cpp`,
//! `Telegram/SourceFiles/settings/settings.style`, and
//! `Telegram/SourceFiles/ui/vertical_list.cpp`.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Response, Role, Sense, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo,
};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::{CheckStyle, CheckboxStyle, Colors, FlatLabelStyle, RadioStyle, ToggleStyle};
use tg_ui::tg_style::{Icon, st};
use tg_ui::theme::{ColorsResolver, resolve_color_ref};
use tg_ui::widgets::checkbox::{Checkbox, Radio, Toggle, paint_toggle_view};
use tg_ui::widgets::labels::{FlatLabel, divider_bar};

/// Height of the settings top navigation bar (`st::info_top_bar_height`, `54px`).
pub const SETTINGS_TOP_BAR_HEIGHT: f32 = 54.0;
/// Standard horizontal padding for settings rows (`st::settings_button.padding.left`, `22px`).
pub const SETTINGS_ROW_PAD_X: f32 = 22.0;
/// Standard settings row height (`44px`).
pub const SETTINGS_ROW_HEIGHT: f32 = 44.0;

/// Returns the scaled settings top bar height from `st::info_top_bar_height`.
#[must_use]
pub fn settings_top_bar_height() -> f32 {
    st::info_top_bar_height.scaled() as f32
}

/// Returns the scaled horizontal padding for settings rows from `st::settings_button_no_icon`.
#[must_use]
pub fn settings_row_pad_x() -> f32 {
    st::settings_button_no_icon.padding.left.scaled() as f32
}

/// Returns the scaled icon badge left offset from `st::settings_button.icon_left`.
#[must_use]
fn settings_icon_left() -> f32 {
    st::settings_button.icon_left.scaled().max(18) as f32
}

/// Returns the scaled text left offset for rows with an icon (`st::settings_button.padding.left`).
#[must_use]
fn settings_text_left_with_icon() -> f32 {
    st::settings_button.padding.left.scaled().max(58) as f32
}

/// Colored badge icon descriptor matching `Settings::IconDescriptor` (`settings_common.cpp` + `colors.palette`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsIconKind {
    Account,
    Notifications,
    Privacy,
    Chat,
    Folders,
    Advanced,
    Calls,
    Power,
    Language,
    Scale,
    Premium,
    Stars,
    Gift,
    Affiliate,
    Business,
    Help,
    Debug,
    Sessions,
    Blocked,
    Lock,
    Key,
    Timer,
    Network,
    Storage,
    Download,
    Export,
    Speaker,
    Mic,
    Camera,
    Brush,
    Sticker,
    Phone,
    Username,
    Birthday,
    Logout,
}

impl SettingsIconKind {
    /// Background fill color from tdesktop's `settingsIconBg1..8` palette tokens
    /// matching `settings_main.cpp` and `settings_common.cpp`.
    #[must_use]
    pub fn bg_color(self, colors: &dyn Colors) -> Color32 {
        let key = match self {
            // Red (`#f06964`): Notifications, Blocked, Logout, Mic
            Self::Notifications | Self::Blocked | Self::Logout | Self::Mic => "settingsIconBg1",
            // Green (`#6dc534`): Privacy, Local Passcode, Calls, Speaker, Phone
            Self::Privacy | Self::Lock | Self::Calls | Self::Speaker | Self::Phone => {
                "settingsIconBg2"
            }
            // Amber/Orange (`#ed9f20`): Chat, Power/Battery, Auto-Delete Timer, Brush, Sticker, 2FA Key, Stars
            Self::Chat
            | Self::Power
            | Self::Timer
            | Self::Brush
            | Self::Sticker
            | Self::Key
            | Self::Stars => "settingsIconBg3",
            // Sky Blue (`#56b3f5`): Account, Folders, Devices/Sessions, Storage, Download, Username, Business
            Self::Account
            | Self::Folders
            | Self::Sessions
            | Self::Storage
            | Self::Download
            | Self::Username
            | Self::Business => "settingsIconBg4",
            // Indigo (`#7595ff`): Scale, Network, Camera, Debug, Affiliate
            Self::Scale | Self::Network | Self::Camera | Self::Debug | Self::Affiliate => {
                "settingsIconBg5"
            }
            // Purple (`#b580e2`): Advanced, Language, Birthday, Gift
            Self::Advanced | Self::Language | Self::Birthday | Self::Gift => "settingsIconBg6",
            // Coral Orange (`#f2925b`): Help/FAQ, Export
            Self::Help | Self::Export => "settingsIconBg8",
            // Telegram Premium accent
            Self::Premium => "premiumButtonBg2",
        };
        let c = colors.color(key);
        if c.a() == 0 {
            colors.color("windowBgActive")
        } else {
            c
        }
    }

    /// Real `tg_style::Icon` from `menu_icons.style` / `settings.style`.
    #[must_use]
    pub fn style_icon(self) -> Icon {
        match self {
            Self::Account => st::menu_icon_profile,
            Self::Notifications => st::menu_icon_notifications,
            Self::Privacy | Self::Lock => st::menu_icon_lock,
            Self::Chat => st::menu_icon_chat_bubble,
            Self::Folders => st::menu_icon_show_in_folder,
            Self::Advanced => st::menu_icon_manage,
            Self::Calls => st::menu_icon_calls_receive,
            Self::Power => st::menu_icon_power_usage,
            Self::Language => st::menu_icon_translate,
            Self::Scale => st::settings_icon_interface_scale,
            Self::Premium | Self::Stars => st::menu_icon_premium,
            Self::Gift => st::menu_icon_gift_premium,
            Self::Affiliate => st::menu_icon_link,
            Self::Business => st::menu_icon_manage,
            Self::Help => st::menu_icon_faq,
            Self::Debug => st::menu_icon_experimental,
            Self::Sessions => st::menu_icon_devices,
            Self::Blocked => st::menu_icon_block,
            Self::Key => st::menu_icon_keys_legend,
            Self::Timer => st::menu_icon_ttl,
            Self::Network => st::menu_icon_network,
            Self::Storage => st::menu_icon_storage,
            Self::Download => st::menu_icon_download,
            Self::Export => st::menu_icon_export,
            Self::Speaker => st::menu_icon_sound_on,
            Self::Mic => st::menu_icon_unmute,
            Self::Camera => st::menu_icon_video_chat,
            Self::Brush => st::menu_icon_font,
            Self::Sticker => st::menu_icon_stickers,
            Self::Phone => st::menu_icon_phone,
            Self::Username => st::menu_icon_username,
            Self::Birthday => st::menu_icon_gift_premium,
            Self::Logout => st::menu_icon_leave_attention,
        }
    }
}

/// Paints a Telegram Desktop settings icon badge: colored rounded square (`st::settings_icon_radius`)
/// with the real `tg-style` icon tinted in `settingsIconFg` (`white`) through `tg-ui`'s icon cache.
pub fn paint_settings_icon(
    painter: &egui::Painter,
    rect: Rect,
    kind: SettingsIconKind,
    colors: &dyn Colors,
) {
    let radius = st::settings_icon_radius.scaled().clamp(1, 255) as u8;
    let bg = kind.bg_color(colors);
    painter.rect_filled(rect, CornerRadius::same(radius), bg);

    let fg = {
        let token = colors.color("settingsIconFg");
        if token.a() == 0 {
            Color32::WHITE
        } else {
            token
        }
    };
    let resolver = ColorsResolver(colors);
    let inner = rect.shrink(3.0);
    paint_style_icon(painter, inner, kind.style_icon(), &resolver, Some(fg));
}

/// Paints a `tg_style::Icon` centered in `rect` with an optional tint color override.
pub fn paint_tinted_icon(
    painter: &egui::Painter,
    rect: Rect,
    icon: Icon,
    colors: &dyn Colors,
    tint: Option<Color32>,
) -> bool {
    let resolver = ColorsResolver(colors);
    paint_style_icon(painter, rect, icon, &resolver, tint)
}

/// Renders the `st::info_top_bar_height` top bar with a real `st::info_top_bar_back` icon button,
/// section title (`st::info_top_bar_title`), and optional right action label.
///
/// Returns `(back_clicked, right_action_clicked)`.
pub fn settings_top_bar(
    ui: &mut Ui,
    title: &str,
    right_action: Option<&str>,
    colors: &dyn Colors,
) -> (bool, bool) {
    let mut back_clicked = false;
    let mut right_clicked = false;
    let rect = ui.max_rect();
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.color("windowBg"));

    let bar_h = settings_top_bar_height();
    let top_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), bar_h));
    ui.scope_builder(egui::UiBuilder::new().max_rect(top_rect), |ui| {
        ui.horizontal_centered(|ui| {
            ui.add_space(6.0);
            let back_w = st::info_top_bar_back.width.scaled().max(36) as f32;
            let back_h = st::info_top_bar_back.height.scaled().max(36) as f32;
            let (back_rect, back_resp) =
                ui.allocate_exact_size(Vec2::new(back_w, back_h.min(bar_h - 4.0)), Sense::click());
            back_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, tr::lng_go_back()));
            if back_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().circle_filled(
                    back_rect.center(),
                    (back_rect.width().min(back_rect.height()) * 0.44).max(16.0),
                    colors.color("windowBgOver"),
                );
            }
            let back_icon = if back_resp.hovered() {
                st::info_top_bar_back_icon_over
            } else {
                st::info_top_bar_back_icon
            };
            paint_tinted_icon(ui.painter(), back_rect, back_icon, colors, None);
            if back_resp.on_hover_text(tr::lng_go_back()).clicked() {
                back_clicked = true;
            }

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(title)
                    .font(font_id_from_style(st::info_top_bar_title.style.font))
                    .color(resolve_color_ref(st::info_top_bar_title.text_fg, colors)),
            );

            if let Some(label) = right_action {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(st::settings_button_right_skip.scaled().max(14) as f32);
                    let resp = ui.add(
                        egui::Button::new(
                            egui::RichText::new(label)
                                .font(font_id_from_style(st::semibold_font))
                                .color(colors.color("windowActiveTextFg")),
                        )
                        .frame(false),
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        right_clicked = true;
                    }
                });
            }
        });
    });

    ui.painter().line_segment(
        [
            Pos2::new(top_rect.left(), top_rect.bottom()),
            Pos2::new(top_rect.right(), top_rect.bottom()),
        ],
        Stroke::new(1.0, colors.color("shadowFg")),
    );

    (back_clicked, right_clicked)
}

/// Renders a colored subsection header (`Ui::AddSubsectionTitle`, `st::default_subsection_title`).
pub fn subsection_title(ui: &mut Ui, title: &str, colors: &dyn Colors) {
    let pad = st::default_subsection_title_padding;
    ui.add_space(pad.top.scaled().max(8) as f32);
    ui.horizontal(|ui| {
        ui.add_space(pad.left.scaled().max(settings_row_pad_x() as i32) as f32);
        ui.label(
            egui::RichText::new(title)
                .font(font_id_from_style(st::default_subsection_title.style.font))
                .color(resolve_color_ref(
                    st::default_subsection_title.text_fg,
                    colors,
                )),
        );
    });
    ui.add_space(pad.bottom.scaled().max(4) as f32);
}

/// Renders a gray divider band (`Ui::AddDivider`) with optional muted explanatory footer (`Ui::AddDividerText`).
pub fn settings_divider(ui: &mut Ui, description: Option<&str>, colors: &dyn Colors) {
    let vskip = (st::default_vertical_list_skip.scaled() as f32).clamp(4.0, 10.0);
    ui.add_space(vskip);
    if let Some(desc) = description {
        let mut style = FlatLabelStyle::box_label(colors);
        style.text_fg = colors.color("windowSubTextFg");
        style.style.font.size = (st::normal_font.scaled_size() as f32 - 1.0).max(11.5);

        let pad = st::default_box_divider_label_padding;
        let pad_x = pad.left.scaled().max(settings_row_pad_x() as i32) as f32;
        let pad_y = pad.top.scaled().max(8) as i8;
        let width = ui.available_width();
        let bg_color = colors.color("boxDividerBg");
        let sh_color = colors.color("shadowFg");

        let frame = egui::Frame::NONE
            .fill(bg_color)
            .inner_margin(egui::Margin::symmetric(pad_x as i8, pad_y));
        let resp = frame.show(ui, |ui| {
            ui.set_min_width((width - pad_x * 2.0).max(120.0));
            FlatLabel::plain(desc, style)
                .id_salt(desc)
                .max_width((width - pad_x * 2.0).max(120.0))
                .show(ui);
        });
        let r = resp.response.rect;
        ui.painter().line_segment(
            [Pos2::new(r.left(), r.top()), Pos2::new(r.right(), r.top())],
            Stroke::new(1.0, sh_color),
        );
        ui.painter().line_segment(
            [
                Pos2::new(r.left(), r.bottom()),
                Pos2::new(r.right(), r.bottom()),
            ],
            Stroke::new(1.0, sh_color),
        );
    } else {
        divider_bar(ui, 10.0, colors);
    }
    ui.add_space(4.0);
}

/// Minimum horizontal clearance kept between a row's left text column and its right-aligned
/// action label / chevron so long titles truncate with an ellipsis before touching them.
const SETTINGS_ROW_TEXT_RIGHT_GAP: f32 = 12.0;

fn layout_elided_single_line(
    painter: &egui::Painter,
    text: &str,
    font_id: egui::FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: max_width.max(1.0),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        },
        ..Default::default()
    };
    job.append(
        text,
        0.0,
        TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
    painter.layout_job(job)
}

/// Full-width clickable settings row (`Ui::SettingsButton` with optional icon, right value, subtitle, or red attention style).
#[allow(clippy::too_many_arguments)]
pub fn settings_row(
    ui: &mut Ui,
    title: &str,
    subtitle: Option<&str>,
    right_label: Option<&str>,
    icon: Option<SettingsIconKind>,
    attention: bool,
    show_chevron: bool,
    colors: &dyn Colors,
) -> Response {
    let w = ui.available_width().max(200.0);
    let pad_x = settings_row_pad_x();
    let base_h = (st::settings_button.height.scaled() as f32).max(SETTINGS_ROW_HEIGHT);
    let h = if subtitle.is_some() {
        base_h + 12.0
    } else {
        base_h
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let a11y_label = match right_label {
        Some(val) if !val.is_empty() => format!("{title}, {val}"),
        _ => title.to_owned(),
    };
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, a11y_label.clone()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        let over_bg = if attention {
            resolve_color_ref(st::settings_attention_button.text_bg_over, colors)
        } else {
            resolve_color_ref(st::settings_button.text_bg_over, colors)
        };
        ui.painter().rect_filled(rect, CornerRadius::ZERO, over_bg);
    }

    let text_x = if let Some(kind) = icon {
        let icon_size = 28.0_f32;
        let icon_rect = Rect::from_min_size(
            Pos2::new(
                rect.left() + settings_icon_left(),
                rect.center().y - icon_size * 0.5,
            ),
            Vec2::splat(icon_size),
        );
        paint_settings_icon(ui.painter(), icon_rect, kind, colors);
        rect.left() + settings_text_left_with_icon()
    } else {
        rect.left() + pad_x
    };

    // Reserve right-side chrome (chevron + right-aligned label) first so the left title and
    // subtitle truncate cleanly with an ellipsis and maintain a consistent clearance gap.
    let mut right_x =
        rect.right() - st::settings_button_right_skip.scaled().max(pad_x as i32) as f32;
    if show_chevron {
        let chev_rect =
            Rect::from_center_size(Pos2::new(right_x - 4.0, rect.center().y), Vec2::splat(16.0));
        paint_tinted_icon(
            ui.painter(),
            chev_rect,
            st::menu_icon_submenu_arrow,
            colors,
            Some(colors.color("windowSubTextFg")),
        );
        right_x -= 18.0;
    }

    let right_fg = colors.color("windowActiveTextFg");
    let right_galley = right_label.filter(|v| !v.is_empty()).map(|val| {
        ui.painter().layout_no_wrap(
            val.to_owned(),
            font_id_from_style(st::normal_font),
            right_fg,
        )
    });
    let right_content_left = match right_galley.as_ref() {
        Some(g) => right_x - g.size().x,
        None => right_x,
    };
    if let Some(g) = right_galley {
        ui.painter().galley(
            Pos2::new(right_content_left, rect.center().y - g.size().y * 0.5),
            g,
            right_fg,
        );
    }

    let text_max_w = (right_content_left - SETTINGS_ROW_TEXT_RIGHT_GAP - text_x).max(24.0);
    let text_clip = Rect::from_min_max(
        Pos2::new(text_x, rect.top()),
        Pos2::new(
            (text_x + text_max_w).min(right_content_left - SETTINGS_ROW_TEXT_RIGHT_GAP * 0.5),
            rect.bottom(),
        ),
    );
    let text_painter = ui.painter().with_clip_rect(text_clip);

    let title_fg = if attention {
        resolve_color_ref(st::settings_attention_button.text_fg, colors)
    } else {
        resolve_color_ref(st::settings_button.text_fg, colors)
    };
    let title_font = font_id_from_style(st::settings_button.style.font);
    let title_galley = layout_elided_single_line(
        ui.painter(),
        title,
        title_font.clone(),
        title_fg,
        text_max_w,
    );
    let mut was_truncated = title_galley.elided;

    if let Some(sub) = subtitle {
        let sub_font = egui::FontId::new(
            (title_font.size - 1.5).max(11.5),
            tg_ui::fonts::family_regular(),
        );
        let sub_fg = colors.color("windowSubTextFg");
        let sub_galley = layout_elided_single_line(ui.painter(), sub, sub_font, sub_fg, text_max_w);
        was_truncated |= sub_galley.elided;

        text_painter.galley(Pos2::new(text_x, rect.top() + 10.0), title_galley, title_fg);
        text_painter.galley(Pos2::new(text_x, rect.top() + 30.0), sub_galley, sub_fg);
    } else {
        let title_y = rect.center().y - title_galley.size().y * 0.5;
        text_painter.galley(Pos2::new(text_x, title_y), title_galley, title_fg);
    }

    if was_truncated {
        let full_tip = match subtitle {
            Some(sub) if !sub.is_empty() => format!("{title}\n{sub}"),
            _ => title.to_owned(),
        };
        resp.on_hover_text(full_tip)
    } else {
        resp
    }
}

/// Full-width settings row with a `tg-ui` toggle switch on the right (`settingsButton` + `toggleOn`).
pub fn settings_toggle_row(
    ui: &mut Ui,
    title: &str,
    subtitle: Option<&str>,
    icon: Option<SettingsIconKind>,
    checked: &mut bool,
    enabled: bool,
    colors: &dyn Colors,
) -> bool {
    let w = ui.available_width().max(200.0);
    let pad_x = settings_row_pad_x();
    let base_h = (st::settings_button.height.scaled() as f32).max(SETTINGS_ROW_HEIGHT);
    let h = if subtitle.is_some() {
        base_h + 12.0
    } else {
        base_h
    };
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), sense);
    let mut changed = false;
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            rect,
            CornerRadius::ZERO,
            resolve_color_ref(st::settings_button.text_bg_over, colors),
        );
    }
    if enabled && resp.clicked() {
        *checked = !*checked;
        changed = true;
    }
    resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, enabled, *checked, title));

    let text_x = if let Some(kind) = icon {
        let icon_size = 28.0_f32;
        let icon_rect = Rect::from_min_size(
            Pos2::new(
                rect.left() + settings_icon_left(),
                rect.center().y - icon_size * 0.5,
            ),
            Vec2::splat(icon_size),
        );
        paint_settings_icon(ui.painter(), icon_rect, kind, colors);
        rect.left() + settings_text_left_with_icon()
    } else {
        rect.left() + pad_x
    };

    let fg = if enabled {
        resolve_color_ref(st::settings_button.text_fg, colors)
    } else {
        colors.color("windowSubTextFg")
    };
    let title_font = font_id_from_style(st::settings_button.style.font);
    let sub_font = egui::FontId::new(
        (title_font.size - 1.5).max(11.5),
        tg_ui::fonts::family_regular(),
    );

    if let Some(sub) = subtitle {
        ui.painter().text(
            Pos2::new(text_x, rect.top() + 10.0),
            Align2::LEFT_TOP,
            title,
            title_font,
            fg,
        );
        ui.painter().text(
            Pos2::new(text_x, rect.top() + 30.0),
            Align2::LEFT_TOP,
            sub,
            sub_font,
            colors.color("windowSubTextFg"),
        );
    } else {
        ui.painter().text(
            Pos2::new(text_x, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            title_font,
            fg,
        );
    }

    let toggle_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&toggle_st);
    let right_skip = st::settings_button_right_skip.scaled().max(pad_x as i32) as f32;
    let toggle_rect = Rect::from_min_size(
        Pos2::new(
            rect.right() - right_skip - tsize.x,
            rect.center().y - tsize.y * 0.5,
        ),
        tsize,
    );
    let toggled_val = if *checked { 1.0 } else { 0.0 };
    paint_toggle_view(
        ui.painter(),
        toggle_rect,
        &toggle_st,
        toggled_val,
        if enabled { 1.0 } else { 0.45 },
    );

    changed
}

/// Split settings row: left side opens details, right side toggles a switch (`SetupSplitToggle`).
///
/// Returns `(row_clicked, toggle_changed)`.
pub fn settings_split_toggle_row(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    icon: Option<SettingsIconKind>,
    checked: &mut bool,
    colors: &dyn Colors,
) -> (bool, bool) {
    let w = ui.available_width().max(220.0);
    let pad_x = settings_row_pad_x();
    let h = 56.0;
    let toggle_zone_w = 68.0;
    let (full_rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());

    let left_rect = Rect::from_min_max(
        full_rect.min,
        Pos2::new(full_rect.right() - toggle_zone_w, full_rect.bottom()),
    );
    let right_rect = Rect::from_min_max(
        Pos2::new(full_rect.right() - toggle_zone_w, full_rect.top()),
        full_rect.max,
    );

    let left_resp = ui.interact(
        left_rect,
        ui.scope_id().with((title, "left")),
        Sense::click(),
    );
    left_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, title));
    let right_resp = ui.interact(
        right_rect,
        ui.scope_id().with((title, "right")),
        Sense::click(),
    );

    let over_bg = resolve_color_ref(st::settings_button.text_bg_over, colors);
    if left_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(left_rect, CornerRadius::ZERO, over_bg);
    }
    if right_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(right_rect, CornerRadius::ZERO, over_bg);
    }

    let text_x = if let Some(kind) = icon {
        let icon_size = 28.0_f32;
        let icon_rect = Rect::from_min_size(
            Pos2::new(
                full_rect.left() + settings_icon_left(),
                full_rect.center().y - icon_size * 0.5,
            ),
            Vec2::splat(icon_size),
        );
        paint_settings_icon(ui.painter(), icon_rect, kind, colors);
        full_rect.left() + settings_text_left_with_icon()
    } else {
        full_rect.left() + pad_x
    };

    let title_font = font_id_from_style(st::settings_button.style.font);
    let sub_font = egui::FontId::new(
        (title_font.size - 1.5).max(11.5),
        tg_ui::fonts::family_regular(),
    );

    ui.painter().text(
        Pos2::new(text_x, full_rect.top() + 10.0),
        Align2::LEFT_TOP,
        title,
        title_font,
        resolve_color_ref(st::settings_button.text_fg, colors),
    );
    ui.painter().text(
        Pos2::new(text_x, full_rect.top() + 30.0),
        Align2::LEFT_TOP,
        subtitle,
        sub_font,
        colors.color("windowSubTextFg"),
    );

    // Subtle vertical divider line between main button and toggle.
    ui.painter().line_segment(
        [
            Pos2::new(right_rect.left(), full_rect.top() + 14.0),
            Pos2::new(right_rect.left(), full_rect.bottom() - 14.0),
        ],
        Stroke::new(1.0, colors.color("shadowFg")),
    );

    let mut toggled_changed = false;
    if right_resp.clicked() {
        *checked = !*checked;
        toggled_changed = true;
    }
    right_resp.widget_info(|| {
        WidgetInfo::selected(Role::CheckBox, true, *checked, format!("Toggle {title}"))
    });

    let toggle_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&toggle_st);
    let toggle_rect = Rect::from_center_size(right_rect.center(), tsize);
    paint_toggle_view(
        ui.painter(),
        toggle_rect,
        &toggle_st,
        if *checked { 1.0 } else { 0.0 },
        1.0,
    );

    (left_resp.clicked(), toggled_changed)
}

/// Indented `tg-ui` checkbox row matching `st::settings_checkbox_padding`.
pub fn settings_checkbox_row(
    ui: &mut Ui,
    checked: &mut bool,
    label: &str,
    enabled: bool,
    colors: &dyn Colors,
) -> Response {
    let pad_left = st::settings_checkbox_padding
        .left
        .scaled()
        .max(settings_row_pad_x() as i32) as f32;
    ui.horizontal(|ui| {
        ui.add_space(pad_left);
        Checkbox::new(
            checked,
            label,
            CheckboxStyle::default_checkbox(colors),
            CheckStyle::default_check(colors),
        )
        .enabled(enabled)
        .show(ui)
    })
    .inner
}

/// Indented `tg-ui` radio button row matching `st::settings_privacy_option`.
pub fn settings_radio_row<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: &mut T,
    value: T,
    label: &str,
    enabled: bool,
    colors: &dyn Colors,
) -> Response {
    let pad_left = st::settings_checkbox_padding
        .left
        .scaled()
        .max(settings_row_pad_x() as i32) as f32;
    ui.horizontal(|ui| {
        ui.add_space(pad_left);
        Radio::new(
            current,
            value,
            label,
            CheckboxStyle::default_checkbox(colors),
            RadioStyle::default_radio(colors),
        )
        .enabled(enabled)
        .show(ui)
    })
    .inner
}

/// Renders a rounded card box (e.g. for current session, preview bubbles, or info banners).
pub fn settings_card(ui: &mut Ui, colors: &dyn Colors, add_contents: impl FnOnce(&mut Ui)) {
    let pad_x = settings_row_pad_x();
    let radius = st::box_radius.scaled().clamp(4, 16) as u8;
    let outer_w = (ui.available_width() - pad_x * 2.0).max(160.0);
    let inner_w = (outer_w - 24.0).max(120.0);
    egui::Frame::NONE
        .outer_margin(egui::Margin::symmetric(pad_x as i8, 0))
        .fill(colors.color("windowBgOver"))
        .stroke(Stroke::new(1.0, colors.color("shadowFg")))
        .corner_radius(CornerRadius::same(radius))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(inner_w);
            ui.set_max_width(inner_w);
            add_contents(ui);
        });
}

/// Paints a small rounded pill tag (e.g. `"BETA"`, `"RTL"`, `"TEST"`).
pub fn paint_badge_pill(ui: &mut Ui, text: &str, bg: Color32, fg: Color32) {
    let font = egui::FontId::new(10.0, tg_ui::fonts::family_semibold());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), fg);
    let size = Vec2::new(galley.size().x + 10.0, 18.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(9), bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(9),
        Stroke::NONE,
        StrokeKind::Inside,
    );
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, text, font, fg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::theme_bridge::LiveThemeColors;

    #[test]
    fn settings_row_truncates_long_title_with_ellipsis_and_gap_at_390px() {
        let ctx = egui::Context::default();
        tg_ui::fonts::register_tdesktop_fonts(&ctx);
        let colors = LiveThemeColors::default();
        let long_title = "Windows Hello (Demo Workstation Primary Passkey)";
        let right_label = "Delete";

        let mut narrow_elided = false;
        let mut narrow_gap = 0.0_f32;
        let mut narrow_rendered = String::new();
        let mut wide_elided = true;

        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(390.0, 200.0))),
                ..Default::default()
            },
            |ui| {
                ui.set_max_width(390.0);
                let _ = settings_row(
                    ui,
                    long_title,
                    Some("Last used Feb 14, 2025 02:26"),
                    Some(right_label),
                    Some(SettingsIconKind::Key),
                    false,
                    true,
                    &colors,
                );
                // Re-measure with the exact same layout metrics used inside `settings_row`.
                let pad_x = settings_row_pad_x();
                let text_x = settings_text_left_with_icon();
                let right_x =
                    390.0 - st::settings_button_right_skip.scaled().max(pad_x as i32) as f32 - 18.0;
                let right_g = ui.painter().layout_no_wrap(
                    right_label.to_owned(),
                    font_id_from_style(st::normal_font),
                    colors.color("windowActiveTextFg"),
                );
                let right_left = right_x - right_g.size().x;
                let max_w = (right_left - SETTINGS_ROW_TEXT_RIGHT_GAP - text_x).max(24.0);
                let title_g = layout_elided_single_line(
                    ui.painter(),
                    long_title,
                    font_id_from_style(st::settings_button.style.font),
                    colors.color("windowFg"),
                    max_w,
                );
                narrow_elided = title_g.elided;
                narrow_gap = right_left - (text_x + title_g.size().x);
                narrow_rendered = title_g.text().to_owned();

                // Desktop width (1440px) has ample space and must not elide.
                let wide_right_x = 1440.0
                    - st::settings_button_right_skip.scaled().max(pad_x as i32) as f32
                    - 18.0;
                let wide_max_w =
                    (wide_right_x - right_g.size().x - SETTINGS_ROW_TEXT_RIGHT_GAP - text_x)
                        .max(24.0);
                let wide_g = layout_elided_single_line(
                    ui.painter(),
                    long_title,
                    font_id_from_style(st::settings_button.style.font),
                    colors.color("windowFg"),
                    wide_max_w,
                );
                wide_elided = wide_g.elided;
            },
        );
        out.textures_delta.clear();

        assert!(
            narrow_elided,
            "long passkey title must elide with an ellipsis at 390px (rendered={narrow_rendered:?}, gap={narrow_gap:.1})"
        );
        assert!(
            narrow_gap >= SETTINGS_ROW_TEXT_RIGHT_GAP - 1.0,
            "must preserve at least {SETTINGS_ROW_TEXT_RIGHT_GAP}px gap before right-aligned Delete (got {narrow_gap:.1}px)"
        );
        assert!(
            !wide_elided,
            "passkey title should render in full at 1440px width"
        );
    }
}
