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
/// Settings row height with an icon (`st::settings_button`: padding 10 + 10 around the 20 px line).
pub const SETTINGS_ROW_HEIGHT: f32 = 40.0;
/// Settings row height without an icon (`st::settings_button_no_icon`: padding 10 + 8).
pub const SETTINGS_ROW_HEIGHT_NO_ICON: f32 = 38.0;
/// Height of a two-line settings row (title + status label, e.g. `SetupSplitToggle`).
pub const SETTINGS_ROW_HEIGHT_TWO_LINE: f32 = 48.0;
/// Size of a `menu_icons.style` outline icon painted at the left of a settings row.
const SETTINGS_ICON_SIZE: f32 = 24.0;

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

/// Settings row icon matching `Settings::IconDescriptor` in `settings_common.cpp`.
///
/// tdesktop 7.4 paints these as monochrome `menu_icons.style` outline icons in their own
/// `menuIconColor`; only Premium and Stars keep their colored glyphs.
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
    /// Two-Step Verification (`menuIcon2SV`).
    TwoStep,
    /// Connected websites (`menuIconIpAddress`).
    IpAddress,
    /// Passkeys (`menuIconPermissions`).
    Passkeys,
    /// Flash the taskbar icon (`menuIconDockBounce`).
    DockBounce,
    /// Contact joined Telegram (`menuIconInvite`).
    Invite,
    /// Pinned messages (`menuIconPin`).
    Pin,
    /// Groups (`menuIconGroups`).
    Groups,
    /// Channels (`menuIconChannel`).
    Channel,
    /// Reactions (`menuIconGroupReactions`).
    Reactions,
    /// Telegram Features (`menuIconEmojiObjects`).
    Features,
    /// Ask a Question (`menuIconDiscussion`).
    Discussion,
    /// Night mode / auto-night (`menuIconNightMode`).
    NightMode,
    /// Your name color (`menuIconChangeColors`).
    NameColor,
    /// Chat folder (`menuIconShowInFolder`).
    Folder,
    /// Manage local storage (`menuIconStorage`).
    LocalStorage,
}

impl SettingsIconKind {
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
            Self::Scale => st::menu_icon_show_in_chat,
            Self::Premium | Self::Stars => st::menu_icon_premium,
            Self::Gift => st::menu_icon_gift_premium,
            Self::Affiliate => st::menu_icon_link,
            Self::Business => st::menu_icon_shop,
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
            Self::TwoStep => st::menu_icon2_sv,
            Self::IpAddress => st::menu_icon_ip_address,
            Self::Passkeys => st::menu_icon_permissions,
            Self::DockBounce => st::menu_icon_dock_bounce,
            Self::Invite => st::menu_icon_invite,
            Self::Pin => st::menu_icon_pin,
            Self::Groups => st::menu_icon_groups,
            Self::Channel => st::menu_icon_channel,
            Self::Reactions => st::menu_icon_group_reactions,
            Self::Features => st::menu_icon_emoji_objects,
            Self::Discussion => st::menu_icon_discussion,
            Self::NightMode => st::menu_icon_night_mode,
            Self::NameColor => st::menu_icon_change_colors,
            Self::Folder => st::menu_icon_show_in_folder,
            Self::LocalStorage => st::menu_icon_storage,
        }
    }

    /// Tint for the few icons tdesktop paints in color (Premium star, Stars coin); `None`
    /// keeps the icon's own `menuIconColor`.
    #[must_use]
    pub fn tint(self, colors: &dyn Colors) -> Option<Color32> {
        let token = match self {
            Self::Premium => "premiumButtonBg2",
            Self::Stars => "creditsBg1",
            _ => return None,
        };
        let c = colors.color(token);
        (c.a() != 0).then_some(c)
    }
}

/// Paints a Telegram Desktop settings row icon: the monochrome `menu_icons.style` outline glyph
/// in its own `menuIconColor` (Premium / Stars in their accent colors), no badge background.
pub fn paint_settings_icon(
    painter: &egui::Painter,
    rect: Rect,
    kind: SettingsIconKind,
    colors: &dyn Colors,
) {
    if let Some(stops) = premium_star_stops(kind, colors)
        && paint_premium_star(painter, rect, &stops)
    {
        return;
    }
    let resolver = ColorsResolver(colors);
    paint_style_icon(
        painter,
        rect,
        kind.style_icon(),
        &resolver,
        kind.tint(colors),
    );
}

/// tdesktop's Premium star (`icons/settings/star.svg`, GPL-3.0) that `AddPremiumStar` paints
/// on the Premium and Stars rows instead of a menu icon.
const PREMIUM_STAR_SVG: &str = include_str!("assets/premium_star.svg");

/// Gradient stops of `AddPremiumStar`: `Ui::Premium::ButtonGradientStops` for Premium,
/// `CreditsIconGradientStops` for Stars. `None` for every other row icon.
fn premium_star_stops(kind: SettingsIconKind, colors: &dyn Colors) -> Option<Vec<(f32, Color32)>> {
    let stops = match kind {
        SettingsIconKind::Premium => vec![
            (0.0, colors.color("premiumButtonBg1")),
            (0.6, colors.color("premiumButtonBg2")),
            (1.0, colors.color("premiumButtonBg3")),
        ],
        SettingsIconKind::Stars => vec![
            (0.0, colors.color("creditsBg1")),
            (1.0, colors.color("creditsBg2")),
        ],
        _ => return None,
    };
    stops.iter().all(|(_, c)| c.a() != 0).then_some(stops)
}

/// `Ui::Premium::ColorizedSvg`: the star with its white fill replaced by a bottom-left to
/// top-right linear gradient over `stops`.
fn colorized_premium_star_svg(stops: &[(f32, Color32)]) -> String {
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
    PREMIUM_STAR_SVG
        .replace("gradientPlaceholder", &gradient)
        .replace("#fff", "url(#Gradient2)")
}

/// Paints the gradient Premium star badge: `settingsButtonNoIcon.style.font->ascent` tall,
/// centered on the 24 px menu icon slot. Returns `false` if the SVG could not be rendered.
fn paint_premium_star(painter: &egui::Painter, icon_rect: Rect, stops: &[(f32, Color32)]) -> bool {
    let ctx = painter.ctx();
    let ppp = ctx.pixels_per_point();
    let font = font_id_from_style(st::settings_button_no_icon.style.font);
    let side = ctx
        .fonts_mut(|f| f.row_height(&font) * 0.78)
        .round()
        .max(8.0);
    let px = (side * ppp).round().max(1.0) as u32;
    let key = stops.iter().fold(u64::from(px), |acc, (o, c)| {
        acc.wrapping_mul(1_000_003)
            ^ u64::from(u32::from_le_bytes(c.to_array()))
            ^ u64::from(o.to_bits())
    });
    let id = egui::Id::unique(("tg_settings_premium_star", key));
    let texture = match ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        Some(cached) => cached,
        None => {
            let rendered = rasterize_svg(&colorized_premium_star_svg(stops), px).map(|image| {
                ctx.load_texture(
                    format!("tg_settings_premium_star_{key}"),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            ctx.data_mut(|d| d.insert_temp(id, rendered.clone()));
            rendered
        }
    };
    let Some(texture) = texture else {
        return false;
    };
    let rect = Rect::from_center_size(icon_rect.center(), Vec2::splat(side));
    painter.image(
        texture.id(),
        rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
    true
}

/// Renders `svg` into a `px`×`px` image, keeping the aspect ratio and centering it.
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

/// Rect of the row icon at `st::settings_button.icon_left`, centered vertically.
fn settings_icon_rect(row: Rect) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            row.left() + settings_icon_left(),
            row.center().y - SETTINGS_ICON_SIZE * 0.5,
        ),
        Vec2::splat(SETTINGS_ICON_SIZE),
    )
}

/// Row height for a single-line settings row (`settings_button` vs `settings_button_no_icon`).
fn single_line_row_height(has_icon: bool) -> f32 {
    if has_icon {
        SETTINGS_ROW_HEIGHT
    } else {
        SETTINGS_ROW_HEIGHT_NO_ICON
    }
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
                // Main Settings header (`settings_main.cpp` `fillTopBarMenu`): search icon and
                // the ⋮ menu whose main entry is "Edit profile" (`right_action`).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    let menu_resp = top_bar_icon_button(
                        ui,
                        st::info_top_bar_menu.icon,
                        st::info_top_bar_menu.icon_over,
                        label,
                        bar_h,
                        colors,
                    );
                    if menu_resp.clicked() {
                        right_clicked = true;
                    }
                    let search_label = tr::lng_dlg_filter();
                    let search_resp = top_bar_icon_button(
                        ui,
                        st::info_top_bar_search.icon,
                        st::info_top_bar_search.icon_over,
                        &search_label,
                        bar_h,
                        colors,
                    );
                    if search_resp.clicked() {
                        let open = super::main_page::settings_search_open(ui.ctx());
                        super::main_page::set_settings_search_open(ui.ctx(), !open);
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

/// 40 px header icon button (`st::info_top_bar_search` / `st::info_top_bar_menu`).
fn top_bar_icon_button(
    ui: &mut Ui,
    icon: Icon,
    icon_over: Icon,
    label: &str,
    bar_h: f32,
    colors: &dyn Colors,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(40.0, bar_h.min(40.0)), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let hovered = resp.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(rect.center(), 18.0, colors.color("windowBgOver"));
    }
    paint_tinted_icon(
        ui.painter(),
        rect,
        if hovered { icon_over } else { icon },
        colors,
        None,
    );
    resp.on_hover_text(label)
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

/// tdesktop's `Ui::AddDivider` band (`BoxContentDivider`, `boxDividerHeight` = 8 px). Callers
/// add the `defaultVerticalListSkip` (6 px) around it, as `SectionBuilder::addSkip` does.
pub fn settings_section_divider(ui: &mut Ui, colors: &dyn Colors) -> Rect {
    let height = st::box_divider_height.scaled().max(1) as f32;
    divider_bar(ui, height, colors)
}

/// `defaultVerticalListSkip`: the skip `SectionBuilder::addSkip` puts around a divider.
pub const SETTINGS_LIST_SKIP: f32 = 6.0;

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
///
/// `_show_chevron` is kept for call-site compatibility: tdesktop 7.4 draws no trailing chevrons
/// on settings rows.
#[allow(clippy::too_many_arguments)]
pub fn settings_row(
    ui: &mut Ui,
    title: &str,
    subtitle: Option<&str>,
    right_label: Option<&str>,
    icon: Option<SettingsIconKind>,
    attention: bool,
    _show_chevron: bool,
    colors: &dyn Colors,
) -> Response {
    let w = ui.available_width().max(200.0);
    let pad_x = settings_row_pad_x();
    let h = if subtitle.is_some() {
        SETTINGS_ROW_HEIGHT_TWO_LINE
    } else {
        single_line_row_height(icon.is_some())
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
        paint_settings_icon(ui.painter(), settings_icon_rect(rect), kind, colors);
        rect.left() + settings_text_left_with_icon()
    } else {
        rect.left() + pad_x
    };

    // Reserve the right-aligned label first so the left title and subtitle truncate cleanly
    // with an ellipsis and keep a consistent clearance gap.
    let right_x = rect.right() - st::settings_button_right_skip.scaled().max(pad_x as i32) as f32;

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

        text_painter.galley(Pos2::new(text_x, rect.top() + 4.0), title_galley, title_fg);
        text_painter.galley(Pos2::new(text_x, rect.top() + 25.0), sub_galley, sub_fg);
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
///
/// Wraps both title and optional subtitle strictly left of the toggle switch (growing the row
/// height dynamically like tdesktop's multi-line `SettingsButton`), preserving a consistent
/// [`SETTINGS_ROW_TEXT_RIGHT_GAP`] clearance so text never runs under the toggle at 390 px.
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
    let base_h = single_line_row_height(icon.is_some());

    let toggle_st = ToggleStyle::default_toggle(colors);
    let tsize = Toggle::toggle_size(&toggle_st);
    let right_skip = st::settings_button_right_skip.scaled().max(pad_x as i32) as f32;
    let text_offset_x = if icon.is_some() {
        settings_text_left_with_icon()
    } else {
        pad_x
    };
    let text_max_w =
        (w - right_skip - tsize.x - SETTINGS_ROW_TEXT_RIGHT_GAP - text_offset_x).max(48.0);

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
    let sub_fg = colors.color("windowSubTextFg");

    let title_galley = ui
        .painter()
        .layout(title.to_owned(), title_font, fg, text_max_w);
    let sub_galley = subtitle.filter(|s| !s.is_empty()).map(|sub| {
        ui.painter()
            .layout(sub.to_owned(), sub_font, sub_fg, text_max_w)
    });

    let pad_v = 10.0_f32;
    let sub_gap = 3.0_f32;
    let h = if let Some(ref sg) = sub_galley {
        (pad_v + title_galley.size().y + sub_gap + sg.size().y + pad_v)
            .max(SETTINGS_ROW_HEIGHT_TWO_LINE)
    } else {
        (pad_v + title_galley.size().y + pad_v).max(base_h)
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
    if enabled && resp.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(4),
            Stroke::new(1.5, colors.color("windowActiveTextFg")),
            StrokeKind::Inside,
        );
    }
    if enabled && resp.clicked() {
        *checked = !*checked;
        changed = true;
    }
    // The accessible name stays the row title (what users and tests look the row up by);
    // the subtitle is description text, not part of the name.
    resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, enabled, *checked, title));

    let text_x = if let Some(kind) = icon {
        paint_settings_icon(ui.painter(), settings_icon_rect(rect), kind, colors);
        rect.left() + settings_text_left_with_icon()
    } else {
        rect.left() + pad_x
    };

    let toggle_left = rect.right() - right_skip - tsize.x;
    let text_clip = Rect::from_min_max(
        Pos2::new(text_x, rect.top()),
        Pos2::new(
            (text_x + text_max_w).min(toggle_left - SETTINGS_ROW_TEXT_RIGHT_GAP * 0.5),
            rect.bottom(),
        ),
    );
    let text_painter = ui.painter().with_clip_rect(text_clip);

    if let Some(sg) = sub_galley {
        let content_h = title_galley.size().y + sub_gap + sg.size().y;
        let top_y = (rect.center().y - content_h * 0.5).max(rect.top() + pad_v * 0.8);
        let sub_y = top_y + title_galley.size().y + sub_gap;
        text_painter.galley(Pos2::new(text_x, top_y), title_galley, fg);
        text_painter.galley(Pos2::new(text_x, sub_y), sg, sub_fg);
    } else {
        let title_y = rect.center().y - title_galley.size().y * 0.5;
        text_painter.galley(Pos2::new(text_x, title_y), title_galley, fg);
    }

    let toggle_rect = Rect::from_min_size(
        Pos2::new(toggle_left, rect.center().y - tsize.y * 0.5),
        tsize,
    );
    let anim_secs = if ui.style().animation_time <= 0.0 {
        0.0
    } else {
        (toggle_st.duration_ms as f32 / 1000.0).min(0.20)
    };
    let toggled_val = f64::from(
        ui.ctx()
            .animate_bool_with_time(resp.id, *checked, anim_secs),
    );
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
/// Wraps title and subtitle strictly left of the vertical divider and right-hand toggle zone so
/// text never collides with the toggle on narrow (390 px) viewports.
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
    let toggle_zone_w = 68.0;
    let text_offset_x = if icon.is_some() {
        settings_text_left_with_icon()
    } else {
        pad_x
    };
    let text_max_w = (w - toggle_zone_w - SETTINGS_ROW_TEXT_RIGHT_GAP - text_offset_x).max(48.0);

    let title_fg = resolve_color_ref(st::settings_button.text_fg, colors);
    let sub_fg = colors.color("windowSubTextFg");
    let title_font = font_id_from_style(st::settings_button.style.font);
    let sub_font = egui::FontId::new(
        (title_font.size - 1.5).max(11.5),
        tg_ui::fonts::family_regular(),
    );
    let title_galley = ui
        .painter()
        .layout(title.to_owned(), title_font, title_fg, text_max_w);
    let sub_galley = if subtitle.is_empty() {
        None
    } else {
        Some(
            ui.painter()
                .layout(subtitle.to_owned(), sub_font, sub_fg, text_max_w),
        )
    };

    let pad_v = 6.0_f32;
    let sub_gap = 1.0_f32;
    let h = if let Some(ref sg) = sub_galley {
        (pad_v + title_galley.size().y + sub_gap + sg.size().y + pad_v)
            .max(SETTINGS_ROW_HEIGHT_TWO_LINE)
    } else {
        (pad_v + title_galley.size().y + pad_v).max(SETTINGS_ROW_HEIGHT)
    };

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
    if left_resp.has_focus() {
        ui.painter().rect_stroke(
            left_rect.shrink(1.0),
            CornerRadius::same(4),
            Stroke::new(1.5, colors.color("windowActiveTextFg")),
            StrokeKind::Inside,
        );
    }
    if right_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(right_rect, CornerRadius::ZERO, over_bg);
    }
    if right_resp.has_focus() {
        ui.painter().rect_stroke(
            right_rect.shrink(1.0),
            CornerRadius::same(4),
            Stroke::new(1.5, colors.color("windowActiveTextFg")),
            StrokeKind::Inside,
        );
    }

    let text_x = if let Some(kind) = icon {
        paint_settings_icon(ui.painter(), settings_icon_rect(full_rect), kind, colors);
        full_rect.left() + settings_text_left_with_icon()
    } else {
        full_rect.left() + pad_x
    };

    let text_clip = Rect::from_min_max(
        Pos2::new(text_x, full_rect.top()),
        Pos2::new(
            (text_x + text_max_w).min(right_rect.left() - SETTINGS_ROW_TEXT_RIGHT_GAP * 0.5),
            full_rect.bottom(),
        ),
    );
    let text_painter = ui.painter().with_clip_rect(text_clip);

    if let Some(sg) = sub_galley {
        let content_h = title_galley.size().y + sub_gap + sg.size().y;
        let top_y = (full_rect.center().y - content_h * 0.5).max(full_rect.top() + pad_v);
        let sub_y = top_y + title_galley.size().y + sub_gap;
        text_painter.galley(Pos2::new(text_x, top_y), title_galley, title_fg);
        text_painter.galley(Pos2::new(text_x, sub_y), sg, sub_fg);
    } else {
        let title_y = full_rect.center().y - title_galley.size().y * 0.5;
        text_painter.galley(Pos2::new(text_x, title_y), title_galley, title_fg);
    }

    // Subtle vertical divider line between main button and toggle.
    ui.painter().line_segment(
        [
            Pos2::new(right_rect.left(), full_rect.top() + 6.0),
            Pos2::new(right_rect.left(), full_rect.bottom() - 6.0),
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
    // Same right edge as plain toggle rows (`st::settings_button_right_skip`).
    let right_skip = st::settings_button_right_skip.scaled().max(pad_x as i32) as f32;
    let toggle_rect = Rect::from_min_size(
        Pos2::new(
            full_rect.right() - right_skip - tsize.x,
            full_rect.center().y - tsize.y * 0.5,
        ),
        tsize,
    );
    let anim_secs = if ui.style().animation_time <= 0.0 {
        0.0
    } else {
        (toggle_st.duration_ms as f32 / 1000.0).min(0.20)
    };
    let toggled_val = f64::from(ui.ctx().animate_bool_with_time(
        right_resp.id,
        *checked,
        anim_secs,
    ));
    paint_toggle_view(ui.painter(), toggle_rect, &toggle_st, toggled_val, 1.0);

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

    #[test]
    fn settings_toggle_row_wraps_privacy_subtitles_left_of_toggle_at_390px() {
        let ctx = egui::Context::default();
        tg_ui::fonts::register_tdesktop_fonts(&ctx);
        let colors = LiveThemeColors::default();
        let rows = [
            (
                "Archive and mute",
                "Automatically archive and mute new chats from people not in your contacts",
            ),
            (
                "Hide read time",
                "Hide the exact time you read messages in private chats",
            ),
            (
                "Require Premium for new non-contact chats",
                "Only contacts and Telegram Premium users can start new private chats with you.",
            ),
            (
                "Charge Stars for messages from non-contacts",
                "Require Telegram Stars for messages from users who are not your contacts.",
            ),
            (
                "Show 18+ Content",
                "Do not hide media that contains content suitable only for adults.",
            ),
        ];

        let mut narrow_heights = Vec::new();
        let mut narrow_gaps = Vec::new();
        let mut wide_heights = Vec::new();

        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(390.0, 900.0))),
                ..Default::default()
            },
            |ui| {
                ui.set_max_width(390.0);
                let pad_x = settings_row_pad_x();
                let toggle_st = ToggleStyle::default_toggle(&colors);
                let tsize = Toggle::toggle_size(&toggle_st);
                let right_skip = st::settings_button_right_skip.scaled().max(pad_x as i32) as f32;
                let toggle_left = 390.0 - right_skip - tsize.x;
                let text_max_w =
                    (390.0 - right_skip - tsize.x - SETTINGS_ROW_TEXT_RIGHT_GAP - pad_x).max(48.0);
                let title_font = font_id_from_style(st::settings_button.style.font);
                let sub_font = egui::FontId::new(
                    (title_font.size - 1.5).max(11.5),
                    tg_ui::fonts::family_regular(),
                );

                for &(title, sub) in &rows {
                    let y0 = ui.cursor().top();
                    let mut checked = false;
                    let _ = settings_toggle_row(
                        ui,
                        title,
                        Some(sub),
                        None,
                        &mut checked,
                        true,
                        &colors,
                    );
                    let y1 = ui.cursor().top();
                    narrow_heights.push(y1 - y0);

                    let tg = ui.painter().layout(
                        title.to_owned(),
                        title_font.clone(),
                        colors.color("windowFg"),
                        text_max_w,
                    );
                    let sg = ui.painter().layout(
                        sub.to_owned(),
                        sub_font.clone(),
                        colors.color("windowSubTextFg"),
                        text_max_w,
                    );
                    let max_text_right = pad_x + tg.size().x.max(sg.size().x);
                    narrow_gaps.push(toggle_left - max_text_right);
                }
            },
        );
        out.textures_delta.clear();

        let mut out_wide = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 900.0))),
                ..Default::default()
            },
            |ui| {
                ui.set_max_width(1000.0);
                for &(title, sub) in &rows {
                    let y0 = ui.cursor().top();
                    let mut checked = false;
                    let _ = settings_toggle_row(
                        ui,
                        title,
                        Some(sub),
                        None,
                        &mut checked,
                        true,
                        &colors,
                    );
                    wide_heights.push(ui.cursor().top() - y0);
                }
            },
        );
        out_wide.textures_delta.clear();

        for (i, &(title, _)) in rows.iter().enumerate() {
            assert!(
                narrow_gaps[i] >= SETTINGS_ROW_TEXT_RIGHT_GAP - 1.0,
                "row {title:?} must keep >= {SETTINGS_ROW_TEXT_RIGHT_GAP}px clearance left of toggle at 390px (got {:.1}px)",
                narrow_gaps[i]
            );
            assert!(
                narrow_heights[i] > wide_heights[i],
                "row {title:?} should wrap and grow taller at 390px ({:.1}px) than 1000px ({:.1}px)",
                narrow_heights[i],
                wide_heights[i]
            );
        }
    }

    #[test]
    fn settings_split_toggle_row_wraps_long_subtitle_left_of_divider_at_390px() {
        let ctx = egui::Context::default();
        tg_ui::fonts::register_tdesktop_fonts(&ctx);
        let colors = LiveThemeColors::default();
        let long_sub = "Custom notification alerts with exceptions across private chats and groups";
        let mut narrow_h = 0.0_f32;
        let mut gap = 0.0_f32;

        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(390.0, 300.0))),
                ..Default::default()
            },
            |ui| {
                ui.set_max_width(390.0);
                let y0 = ui.cursor().top();
                let mut checked = true;
                let _ = settings_split_toggle_row(
                    ui,
                    "Private chats",
                    long_sub,
                    Some(SettingsIconKind::Notifications),
                    &mut checked,
                    &colors,
                );
                narrow_h = ui.cursor().top() - y0;

                let text_x = settings_text_left_with_icon();
                let divider_x = 390.0 - 68.0;
                let text_max_w = (divider_x - SETTINGS_ROW_TEXT_RIGHT_GAP - text_x).max(48.0);
                let title_font = font_id_from_style(st::settings_button.style.font);
                let sub_font = egui::FontId::new(
                    (title_font.size - 1.5).max(11.5),
                    tg_ui::fonts::family_regular(),
                );
                let sg = ui.painter().layout(
                    long_sub.to_owned(),
                    sub_font,
                    colors.color("windowSubTextFg"),
                    text_max_w,
                );
                gap = divider_x - (text_x + sg.size().x);
            },
        );
        out.textures_delta.clear();

        assert!(
            narrow_h > 56.0,
            "split toggle row must expand height to fit wrapped subtitle at 390px (got {narrow_h:.1}px)"
        );
        assert!(
            gap >= SETTINGS_ROW_TEXT_RIGHT_GAP - 1.0,
            "split toggle subtitle must stay left of vertical divider (got gap {gap:.1}px)"
        );
    }
}
