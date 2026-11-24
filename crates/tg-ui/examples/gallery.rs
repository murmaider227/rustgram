//! Runnable Telegram Desktop widget gallery for `tg-ui` with live theme & accent switcher,
//! bundled Open Sans + Hack fonts, real `tg-style` icons, and `tg-text` rich text rendering.
//!
//! Run with:
//! ```sh
//! cargo run -p tg-ui --example gallery
//! ```
//! Optional theme & screenshot capture:
//! ```sh
//! cargo run -p tg-ui --example gallery -- --theme day --screenshot crates/tg-ui/docs/gallery-day.png
//! cargo run -p tg-ui --example gallery -- --theme night --screenshot crates/tg-ui/docs/gallery-night.png
//! ```

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use eframe::egui;
use egui::{CornerRadius, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use tg_text::{EntityInText, EntityType, TextWithEntities, utf16_len};
use tg_theme::{Color as ThemeColor, EmbeddedType};
use tg_ui::effects::InfiniteRadialAnimation;
use tg_ui::layers::{BoxButton, BoxModal, LayerStack};
use tg_ui::painter::BoxShadow;
use tg_ui::platform::WindowTitleBar;
use tg_ui::style::{
    Align, BoxShadowStyle, BoxStyle, CheckStyle, CheckboxStyle, FilledSliderStyle, FlatButtonStyle,
    FlatLabelStyle, IconButtonStyle, ImportantTooltipStyle, InfiniteRadialAnimationStyle,
    InputFieldStyle, LinkButtonStyle, MediaSliderStyle, PopupMenuStyle, RadioStyle,
    RoundButtonStyle, ScrollAreaStyle, SettingsSliderStyle, TextPalette, TextStyle, ToastStyle,
    ToggleStyle, TooltipStyle, UnreadBadgeStyle, WindowTitleStyle,
};
use tg_ui::text::{RichTextOptions, render_text_with_entities};
use tg_ui::theme::{Colors, ThemeManager, theme_color_to_egui};
use tg_ui::toast::{ToastAttach, ToastConfig, ToastManager};
use tg_ui::unread_badge::{UnreadBadgeKind, paint_unread_badge, unread_badge};
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::buttons::{FlatButton, IconButton, IconKind, LinkButton, RoundButton};
use tg_ui::widgets::checkbox::{Checkbox, Radio, Toggle};
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::{FlatLabel, divider_bar};
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};
use tg_ui::widgets::scroll_area::TgScrollArea;
use tg_ui::widgets::sliders::{FilledSlider, MediaSlider, SettingsSlider};
use tg_ui::widgets::tooltip::{ImportantTooltipState, TooltipSide, show_tooltip_on_hover};

struct GalleryApp {
    theme: ThemeManager,
    tab: usize,
    // Inputs
    name_input: String,
    empty_input: String,
    error_input: String,
    search_input: String,
    bio_input: String,
    // Checks & toggles
    check_notifications: bool,
    check_preview: bool,
    check_disabled: bool,
    embedded_choice: EmbeddedType,
    toggle_sound: bool,
    toggle_xv: bool,
    // Sliders
    media_pos: f64,
    volume: f64,
    filled_val: f64,
    scale_idx: usize,
    // Overlays
    popup_menu: PopupMenuState,
    menu_mute_checked: bool,
    imp_tooltip: ImportantTooltipState,
    toasts: ToastManager,
    layers: LayerStack,
    box_note: String,
    status_line: String,
    maximized: bool,
    // Rich text sample
    rich_sample: TextWithEntities,
    elided_sample: TextWithEntities,
    // Optional screenshot automation
    screenshot_path: Option<PathBuf>,
    screenshot_requested: bool,
    screenshot_done: Arc<AtomicBool>,
    frames_rendered: u32,
}

fn build_rich_sample() -> TextWithEntities {
    // Rich message demonstrating UTF-16 entities, Cyrillic, emoji, inline formatting,
    // mentions, hashtags, links, spoilers, custom emoji placeholder, blockquote, and `<pre>`.
    let mut text = String::new();
    let mut entities = Vec::new();

    let mut push = |s: &str, kind: Option<(EntityType, &str)>| {
        let off = utf16_len(&text) as i32;
        text.push_str(s);
        let len = utf16_len(s) as i32;
        if let Some((et, data)) = kind {
            entities.push(EntityInText::with_data(et, off, len, data));
        }
    };

    push("🎉 ", None);
    push("Bold", Some((EntityType::Bold, "")));
    push(", ", None);
    push("italic", Some((EntityType::Italic, "")));
    push(", ", None);
    push("underlined", Some((EntityType::Underline, "")));
    push(", ", None);
    push("strike", Some((EntityType::StrikeOut, "")));
    push(", ", None);
    push("inline_code()", Some((EntityType::Code, "")));
    push(", ", None);
    push("Привет", Some((EntityType::Bold, "")));
    push(" ", None);
    push("@alex_doe", Some((EntityType::Mention, "")));
    push(" ", None);
    push("#rustgram", Some((EntityType::Hashtag, "")));
    push(" ", None);
    push(
        "docs link",
        Some((EntityType::CustomUrl, "https://example.org/rustgram")),
    );
    push(" ", None);
    push("hidden spoiler", Some((EntityType::Spoiler, "")));
    push(" ", None);
    push("⭐", Some((EntityType::CustomEmoji, "5368324170671202286")));
    push("\n", None);
    push(
        "Blockquote with left accent bar — Кириллица и UTF-16 работают точно.",
        Some((EntityType::Blockquote, "")),
    );
    push("\n", None);
    push(
        "fn greet(name: &str) {\n    println!(\"Привет, {name}!\");\n}",
        Some((EntityType::Pre, "rust")),
    );

    TextWithEntities::with_entities(text, entities)
}

impl GalleryApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        initial_theme: EmbeddedType,
        screenshot_path: Option<PathBuf>,
    ) -> Self {
        let theme = ThemeManager::with_embedded(initial_theme);
        theme.apply_to_egui(&cc.egui_ctx);

        let mut toasts = ToastManager::new();
        if screenshot_path.is_some() {
            toasts.show_toast(
                &cc.egui_ctx,
                ToastConfig::simple("Theme loaded — tg-style + tg-theme active")
                    .with_icon(IconKind::Check)
                    .with_attach(ToastAttach::Bottom)
                    .with_duration_ms(10_000),
            );
        }

        let elided_sample = TextWithEntities::simple(
            "Single-line elided preview: Long message text automatically truncates with an ellipsis when max_lines is reached.",
        );

        Self {
            theme,
            tab: 0,
            name_input: "Alex Doe".to_owned(),
            empty_input: String::new(),
            error_input: "invalid_handle!".to_owned(),
            search_input: String::new(),
            bio_input:
                "Multi-line input field (Кириллица работает!).\nGrows up to heightMax and scrolls smoothly like tdesktop."
                    .to_owned(),
            check_notifications: true,
            check_preview: false,
            check_disabled: true,
            embedded_choice: initial_theme,
            toggle_sound: true,
            toggle_xv: true,
            media_pos: 0.42,
            volume: 0.78,
            filled_val: 0.65,
            scale_idx: 0,
            popup_menu: PopupMenuState::new(),
            menu_mute_checked: false,
            imp_tooltip: ImportantTooltipState::new(),
            toasts,
            layers: LayerStack::new(),
            box_note: "Rustgram modal dialog".to_owned(),
            status_line: "Ready — switch themes, click spoilers/links, or drag-select text."
                .to_owned(),
            maximized: false,
            rich_sample: build_rich_sample(),
            elided_sample,
            screenshot_path,
            screenshot_requested: false,
            screenshot_done: Arc::new(AtomicBool::new(false)),
            frames_rendered: 0,
        }
    }

    fn apply_theme_choice(&mut self, ctx: &egui::Context, kind: EmbeddedType) {
        self.embedded_choice = kind;
        self.theme.set_embedded(kind, None);
        self.theme.apply_to_egui(ctx);
        self.status_line = format!("Switched theme to: {}", self.theme.embedded_scheme().name);
    }

    fn apply_accent_choice(&mut self, ctx: &egui::Context, accent: Option<ThemeColor>) {
        self.theme.set_accent(accent);
        self.theme.apply_to_egui(ctx);
        self.status_line = match accent {
            Some(c) => format!("Applied accent color: {}", c.to_hex_string()),
            None => "Reset accent color to theme default".to_owned(),
        };
    }

    fn section_title(&self, ui: &mut egui::Ui, title: &str) {
        let st = FlatLabelStyle::subsection_title(&self.theme);
        FlatLabel::plain(title, st).id_salt(title).show(ui);
        ui.add_space(6.0);
    }

    fn render_theme_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
            ui.label(
                egui::RichText::new("Theme:")
                    .size(12.0)
                    .color(self.theme.color("windowSubTextFg")),
            );
            for scheme in ThemeManager::available_embedded_themes() {
                let selected = self.theme.embedded_type() == scheme.kind;
                let btn_st = if selected {
                    RoundButtonStyle::default_active(&self.theme)
                } else {
                    RoundButtonStyle::default_light(&self.theme)
                };
                let mut compact = btn_st;
                compact.height = 26.0;
                compact.width = -20.0;
                if RoundButton::new(scheme.name, compact).show(ui).clicked() {
                    self.apply_theme_choice(&ctx, scheme.kind);
                }
            }

            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("Accent:")
                    .size(12.0)
                    .color(self.theme.color("windowSubTextFg")),
            );
            let presets = self.theme.preset_accent_colors();
            for preset in presets {
                let c = theme_color_to_egui(preset);
                let is_active = self.theme.accent() == Some(preset);
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                ui.painter().circle_filled(rect.center(), 7.5, c);
                if is_active {
                    ui.painter().circle_stroke(
                        rect.center(),
                        9.0,
                        Stroke::new(2.0, self.theme.color("windowFg")),
                    );
                }
                if resp.clicked() {
                    self.apply_accent_choice(&ctx, Some(preset));
                }
            }
            if self.theme.accent().is_some()
                && LinkButton::new("Reset", LinkButtonStyle::default_link(&self.theme))
                    .show(ui)
                    .clicked()
            {
                self.apply_accent_choice(&ctx, None);
            }
        });
    }

    fn render_buttons_section(&mut self, ui: &mut egui::Ui) {
        self.section_title(ui, "Buttons, Icons & Ripples");
        let active_st = RoundButtonStyle::default_active(&self.theme);
        let light_st = RoundButtonStyle::default_light(&self.theme);
        let attn_st = RoundButtonStyle::attention_box(&self.theme);

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            if RoundButton::new("Save Changes", active_st)
                .show(ui)
                .clicked()
            {
                self.status_line = "Clicked: Primary RoundButton".to_owned();
            }
            if RoundButton::new("Forward", active_st)
                .numbers_text("12")
                .show(ui)
                .clicked()
            {
                self.status_line = "Clicked: Counter RoundButton".to_owned();
            }
            if RoundButton::new("Cancel", light_st).show(ui).clicked() {
                self.status_line = "Clicked: Light RoundButton".to_owned();
            }
            if RoundButton::new("Log Out", attn_st).show(ui).clicked() {
                self.status_line = "Clicked: Attention RoundButton".to_owned();
            }
            if RoundButton::new("Send", active_st)
                .icon(IconKind::Send)
                .full_radius(true)
                .show(ui)
                .clicked()
            {
                self.status_line = "Clicked: Pill RoundButton".to_owned();
            }
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            let icon_st = IconButtonStyle::default_icon_button(&self.theme);
            for (icon, tip) in [
                (IconKind::Hamburger, "Main menu"),
                (IconKind::Back, "Back"),
                (IconKind::Search, "Search"),
                (IconKind::Attach, "Attach file"),
                (IconKind::Settings, "Settings"),
                (IconKind::MenuDots, "More options"),
            ] {
                let resp = IconButton::new(icon, icon_st).show(ui);
                show_tooltip_on_hover(
                    ui.ctx(),
                    &resp,
                    tip,
                    &TooltipStyle::default_tooltip(&self.theme),
                );
                if resp.clicked() {
                    self.status_line = format!("Clicked IconButton: {tip}");
                }
            }
            ui.add_space(6.0);
            let link_st = LinkButtonStyle::default_link(&self.theme);
            if LinkButton::new("Privacy Policy", link_st)
                .show(ui)
                .clicked()
            {
                self.status_line = "Clicked LinkButton: Privacy Policy".to_owned();
            }
        });

        ui.add_space(6.0);
        let mut flat_st = FlatButtonStyle::default_flat(&self.theme);
        flat_st.bg_color = self.theme.color("windowBgOver");
        flat_st.over_bg_color = self.theme.color("windowBgRipple");
        flat_st.height = 38.0;
        if FlatButton::new("JOIN CHANNEL", flat_st)
            .width(240.0)
            .show(ui)
            .clicked()
        {
            self.status_line = "Clicked FlatButton: JOIN CHANNEL".to_owned();
        }
    }

    fn render_inputs_section(&mut self, ui: &mut egui::Ui) {
        self.section_title(ui, "Input Fields");
        let input_st = InputFieldStyle::default_input(&self.theme);
        let search_st = InputFieldStyle::rounded_search(&self.theme);

        InputField::single_line(
            "inp_name",
            &mut self.name_input,
            "First & last name",
            input_st,
        )
        .show(ui);
        ui.add_space(6.0);

        InputField::single_line(
            "inp_empty",
            &mut self.empty_input,
            "Username (click to float placeholder)",
            input_st,
        )
        .show(ui);
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            let err_w = (ui.available_width() - 86.0).max(120.0);
            InputField::single_line("inp_err", &mut self.error_input, "Invalid handle", input_st)
                .error(true)
                .width(err_w)
                .show(ui);
            let shake_btn =
                RoundButton::new("Shake", RoundButtonStyle::attention_box(&self.theme)).show(ui);
            if shake_btn.clicked() {
                InputField::trigger_shake(ui.ctx(), ui.make_persistent_id("inp_err"));
            }
        });
        ui.add_space(8.0);

        InputField::single_line(
            "inp_search",
            &mut self.search_input,
            "Search chats...",
            search_st,
        )
        .show(ui);
        ui.add_space(8.0);

        InputField::multi_line("inp_bio", &mut self.bio_input, "Bio (multi-line)", input_st)
            .max_length(140)
            .show(ui);
    }

    fn render_selection_section(&mut self, ui: &mut egui::Ui) {
        self.section_title(ui, "Checkboxes, Radios & Toggles");
        let cb_st = CheckboxStyle::default_checkbox(&self.theme);
        let chk_st = CheckStyle::default_check(&self.theme);
        let rad_st = RadioStyle::default_radio(&self.theme);
        let tog_st = ToggleStyle::default_toggle(&self.theme);
        let tog_xv_st = ToggleStyle::with_xv(&self.theme);
        let ctx = ui.ctx().clone();
        let prev_choice = self.embedded_choice;

        ui.columns(2, |cols| {
            Checkbox::new(
                &mut self.check_notifications,
                "Desktop notifications",
                cb_st,
                chk_st,
            )
            .show(&mut cols[0]);
            Checkbox::new(
                &mut self.check_preview,
                "Show message preview",
                cb_st,
                chk_st,
            )
            .show(&mut cols[0]);
            Checkbox::new(&mut self.check_disabled, "Disabled option", cb_st, chk_st)
                .enabled(false)
                .show(&mut cols[0]);

            Radio::new(
                &mut self.embedded_choice,
                EmbeddedType::Default,
                "Classic theme",
                cb_st,
                rad_st,
            )
            .show(&mut cols[1]);
            Radio::new(
                &mut self.embedded_choice,
                EmbeddedType::DayBlue,
                "Day Blue theme",
                cb_st,
                rad_st,
            )
            .show(&mut cols[1]);
            Radio::new(
                &mut self.embedded_choice,
                EmbeddedType::Night,
                "Night (Tinted) theme",
                cb_st,
                rad_st,
            )
            .show(&mut cols[1]);
            Radio::new(
                &mut self.embedded_choice,
                EmbeddedType::NightGreen,
                "Night Green theme",
                cb_st,
                rad_st,
            )
            .show(&mut cols[1]);
        });
        if self.embedded_choice != prev_choice {
            let next = self.embedded_choice;
            self.apply_theme_choice(&ctx, next);
        }

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(18.0, 6.0);
            Toggle::new(&mut self.toggle_sound, tog_st)
                .with_label("Notification sound", self.theme.color("windowFg"))
                .show(ui);
            Toggle::new(&mut self.toggle_xv, tog_xv_st)
                .with_label("X / V morph toggle", self.theme.color("windowFg"))
                .show(ui);
        });
    }

    fn render_avatars_badges_section(&mut self, ui: &mut egui::Ui) {
        self.section_title(ui, "Userpics & Unread Badges");
        let peers = [
            (0u64, "Alex Doe"),
            (1, "Sam Rivera"),
            (2, "Jordan Lee"),
            (3, "Taylor Kim"),
            (4, "Morgan Patel"),
            (5, "Casey Novak"),
            (6, "Riley Chen"),
        ];
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(10.0, 8.0);
            for (peer_id, name) in peers {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
                EmptyUserpic::for_peer(peer_id, name, &self.theme).paint_circle(ui.painter(), rect);
                show_tooltip_on_hover(
                    ui.ctx(),
                    &resp,
                    &format!("{name} (color index {})", peer_id % 7),
                    &TooltipStyle::default_tooltip(&self.theme),
                );
            }
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
            EmptyUserpic::paint_saved_messages(ui.painter(), rect, &self.theme);
            show_tooltip_on_hover(
                ui.ctx(),
                &resp,
                "Saved Messages",
                &TooltipStyle::default_tooltip(&self.theme),
            );
        });

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
            unread_badge(ui, "4", &UnreadBadgeStyle::default(), &self.theme);
            unread_badge(ui, "128", &UnreadBadgeStyle::default(), &self.theme);
            unread_badge(
                ui,
                "99",
                &UnreadBadgeStyle {
                    muted: true,
                    ..Default::default()
                },
                &self.theme,
            );
            let (chip_rect, _) = ui.allocate_exact_size(Vec2::new(112.0, 27.0), Sense::hover());
            ui.painter().rect_filled(
                chip_rect,
                CornerRadius::same(6),
                self.theme.color("dialogsBgActive"),
            );
            ui.painter().text(
                Pos2::new(chip_rect.min.x + 10.0, chip_rect.center().y),
                egui::Align2::LEFT_CENTER,
                "Active chat",
                egui::FontId::proportional(12.0),
                self.theme.color("windowFgActive"),
            );
            paint_unread_badge(
                ui.painter(),
                "12",
                Pos2::new(chip_rect.max.x - 6.0, chip_rect.min.y + 4.0),
                &UnreadBadgeStyle {
                    active: true,
                    ..Default::default()
                },
                UnreadBadgeKind::Dialogs,
                0,
                &self.theme,
            );
            let (trunc_rect, _) = ui.allocate_exact_size(Vec2::new(40.0, 20.0), Sense::hover());
            paint_unread_badge(
                ui.painter(),
                "12345",
                Pos2::new(trunc_rect.min.x, trunc_rect.min.y),
                &UnreadBadgeStyle {
                    align: Align::Left,
                    ..Default::default()
                },
                UnreadBadgeKind::Dialogs,
                2,
                &self.theme,
            );
        });
    }

    fn render_sliders_section(&mut self, ui: &mut egui::Ui) {
        self.section_title(ui, "Sliders & Radial Loaders");
        let media_st = MediaSliderStyle::default_media_slider(&self.theme);
        let filled_st = FilledSliderStyle::default_filled_slider(&self.theme);
        let set_st = SettingsSliderStyle::default_settings_slider(&self.theme);

        ui.label(
            egui::RichText::new(format!("Media seek ({:.0}%)", self.media_pos * 100.0))
                .size(12.0)
                .color(self.theme.color("windowSubTextFg")),
        );
        MediaSlider::new(&mut self.media_pos, media_st)
            .received_till(0.75)
            .always_display_marker(true)
            .show(ui);

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("Hover-marker volume ({:.0}%)", self.volume * 100.0))
                .size(12.0)
                .color(self.theme.color("windowSubTextFg")),
        );
        MediaSlider::new(&mut self.volume, media_st).show(ui);

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Filled slider (expands on hover)")
                .size(12.0)
                .color(self.theme.color("windowSubTextFg")),
        );
        FilledSlider::new(&mut self.filled_val, filled_st).show(ui);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let sw = (ui.available_width() - 56.0).max(160.0);
            SettingsSlider::new(
                "scale_slider",
                &mut self.scale_idx,
                &["100%", "125%", "150%", "200%"],
                set_st,
            )
            .width(sw)
            .show(ui);

            ui.add_space(12.0);
            let rad_st = InfiniteRadialAnimationStyle::default_radial(&self.theme);
            let (rrect, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
            InfiniteRadialAnimation::new(rad_st).paint(ui.ctx(), ui.painter(), rrect);
        });
    }

    fn render_rich_text_section(&mut self, ui: &mut egui::Ui) {
        self.section_title(ui, "Rich Text (tg-text TextWithEntities)");
        let text_st = TextStyle::history_text();
        let text_pal = TextPalette::in_message(&self.theme);
        let opts = RichTextOptions::new(self.theme.color("windowFg"), ui.available_width())
            .selectable(true);

        let resp = render_text_with_entities(
            ui,
            ui.make_persistent_id("gallery_rich_entities"),
            &self.rich_sample,
            &text_st,
            &text_pal,
            &opts,
        );
        if let Some(link) = resp.clicked_link {
            self.status_line = format!("Clicked {:?}: {}", link.kind, link.target);
        }
        if resp.spoiler_toggled {
            self.status_line = "Revealed hidden spoiler span!".to_owned();
        }
        if let Some(code) = resp.copied_pre_code {
            self.status_line = format!("Copied <pre> block ({} bytes)", code.len());
            self.toasts.show_toast(
                ui.ctx(),
                ToastConfig::simple("Code copied to clipboard")
                    .with_icon(IconKind::Check)
                    .with_attach(ToastAttach::Bottom),
            );
        }

        ui.add_space(6.0);
        let elide_opts = RichTextOptions::new(
            self.theme.color("windowSubTextFg"),
            ui.available_width().min(360.0),
        )
        .max_lines(1)
        .elide(true);
        let _ = render_text_with_entities(
            ui,
            ui.make_persistent_id("gallery_elided_demo"),
            &self.elided_sample,
            &TextStyle::default_style(),
            &TextPalette::default_palette(&self.theme),
            &elide_opts,
        );
    }

    fn render_overlays_section(&mut self, ui: &mut egui::Ui) {
        self.section_title(ui, "Menus, Tooltips, Toasts & Modal Layers");
        let light_st = RoundButtonStyle::default_light(&self.theme);
        let active_st = RoundButtonStyle::default_active(&self.theme);
        let popup_st = PopupMenuStyle::default_popup(&self.theme);
        let imp_st = ImportantTooltipStyle::default_important(&self.theme);
        let box_st = BoxStyle::default_box(&self.theme);

        let mut imp_target_rect = Rect::NOTHING;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            let menu_btn = RoundButton::new("Popup Menu", light_st)
                .icon(IconKind::MenuDots)
                .show(ui);
            if menu_btn.clicked() {
                self.popup_menu.open_at(
                    ui.ctx(),
                    Pos2::new(menu_btn.rect.min.x, menu_btn.rect.max.y + 4.0),
                    &popup_st,
                );
            }

            let tip_btn = RoundButton::new("Callout Tooltip", light_st)
                .icon(IconKind::Info)
                .show(ui);
            imp_target_rect = tip_btn.rect;
            if tip_btn.clicked() {
                let next = !self.imp_tooltip.is_shown();
                self.imp_tooltip.toggle(ui.ctx(), next, &imp_st, Some(3000));
            }
            show_tooltip_on_hover(
                ui.ctx(),
                &tip_btn,
                "Classic Tooltip — click for ImportantTooltip",
                &TooltipStyle::default_tooltip(&self.theme),
            );

            if RoundButton::new("Show Toast", light_st)
                .icon(IconKind::Check)
                .show(ui)
                .clicked()
            {
                self.toasts.show_toast(
                    ui.ctx(),
                    ToastConfig::simple("Message link copied to clipboard")
                        .with_icon(IconKind::Check)
                        .with_attach(ToastAttach::Bottom),
                );
            }

            if RoundButton::new("Open Modal Box", active_st)
                .show(ui)
                .clicked()
            {
                self.layers.push(
                    ui.ctx(),
                    BoxModal::new("settings_box", "Edit Profile")
                        .additional_title("Layer 1")
                        .with_left_button(BoxButton::default_btn("stack", "Stack Confirm"))
                        .add_button(BoxButton::default_btn("cancel", "Cancel"))
                        .add_button(BoxButton::default_btn("save", "Save")),
                    &box_st,
                );
            }
        });

        self.imp_tooltip.show(
            ui.ctx(),
            Id::unique("gallery_imp_tip"),
            imp_target_rect,
            TooltipSide::Top,
            "ImportantTooltip callout with arrow",
            &imp_st,
        );

        ui.add_space(10.0);
        let (card_rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().min(420.0), 62.0),
            Sense::hover(),
        );
        let shadow_st = BoxShadowStyle::default_shadow();
        BoxShadow::new(shadow_st).paint(ui.ctx(), ui.painter(), card_rect, 8.0);
        ui.painter().rect_filled(
            card_rect,
            CornerRadius::same(8),
            self.theme.color("windowBg"),
        );
        ui.painter().rect_stroke(
            card_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, self.theme.color("shadowFg")),
            StrokeKind::Inside,
        );

        let mut preview_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(card_rect.shrink(6.0))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        WindowTitleBar::new(
            "Inactive Window Title Bar Preview",
            WindowTitleStyle::default_title(&self.theme),
        )
        .active(false)
        .issue_viewport_commands(false)
        .id_salt("preview_inactive_title")
        .show(&mut preview_ui);
        preview_ui.add_space(4.0);
        preview_ui.label(
            egui::RichText::new("9-slice Gaussian BoxShadow + live tg-theme Windows title chrome")
                .size(12.0)
                .color(self.theme.color("windowSubTextFg")),
        );
    }
}

impl eframe::App for GalleryApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let viewport = ctx.viewport_rect();
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        self.maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));

        let theme_name = self.theme.embedded_scheme().name;

        // 1. Custom Windows title bar + theme switcher + tabs
        egui::Panel::top("tg_title_bar")
            .frame(egui::Frame::NONE.fill(self.theme.color("windowBg")))
            .show(ui, |ui| {
                WindowTitleBar::new(
                    &format!("Rustgram — tg-ui Widget Gallery ({theme_name} Theme)"),
                    WindowTitleStyle::default_title(&self.theme),
                )
                .active(focused)
                .maximized(self.maximized)
                .show(ui);

                egui::Frame::NONE
                    .fill(self.theme.color("windowBgOver"))
                    .inner_margin(egui::Margin::symmetric(16, 6))
                    .show(ui, |ui| {
                        self.render_theme_bar(ui);
                    });

                let tabs_st = SettingsSliderStyle::default_tabs_slider(&self.theme);
                SettingsSlider::new(
                    "gallery_tabs",
                    &mut self.tab,
                    &["All Widgets", "Inputs & Sliders", "Avatars & Overlays"],
                    tabs_st,
                )
                .show(ui);
            });

        // Status footer bar
        egui::Panel::bottom("tg_status_bar")
            .frame(
                egui::Frame::NONE
                    .fill(self.theme.color("windowBgOver"))
                    .inner_margin(egui::Margin::symmetric(16, 6)),
            )
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(&self.status_line)
                        .size(12.0)
                        .color(self.theme.color("windowSubTextFgOver")),
                );
            });

        // 2. Main scrollable gallery body
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(self.theme.color("windowBg"))
                    .inner_margin(egui::Margin::symmetric(18, 12)),
            )
            .show(ui, |ui| {
                let scroll_st = ScrollAreaStyle::default_scroll(&self.theme);
                TgScrollArea::new("gallery_main_scroll", scroll_st)
                    .show_shadows(true)
                    .show(ui, |ui| match self.tab {
                        0 => {
                            ui.columns(2, |cols| {
                                cols[0].vertical(|ui| {
                                    self.render_buttons_section(ui);
                                    ui.add_space(8.0);
                                    divider_bar(ui, 8.0, &self.theme);
                                    ui.add_space(8.0);
                                    self.render_inputs_section(ui);
                                    ui.add_space(8.0);
                                    divider_bar(ui, 8.0, &self.theme);
                                    ui.add_space(8.0);
                                    self.render_selection_section(ui);
                                });
                                cols[1].vertical(|ui| {
                                    self.render_avatars_badges_section(ui);
                                    ui.add_space(8.0);
                                    divider_bar(ui, 8.0, &self.theme);
                                    ui.add_space(8.0);
                                    self.render_sliders_section(ui);
                                    ui.add_space(8.0);
                                    divider_bar(ui, 8.0, &self.theme);
                                    ui.add_space(8.0);
                                    self.render_rich_text_section(ui);
                                    ui.add_space(8.0);
                                    divider_bar(ui, 8.0, &self.theme);
                                    ui.add_space(8.0);
                                    self.render_overlays_section(ui);
                                });
                            });
                        }
                        1 => {
                            self.render_inputs_section(ui);
                            ui.add_space(12.0);
                            divider_bar(ui, 8.0, &self.theme);
                            ui.add_space(12.0);
                            self.render_sliders_section(ui);
                        }
                        _ => {
                            self.render_avatars_badges_section(ui);
                            ui.add_space(12.0);
                            divider_bar(ui, 8.0, &self.theme);
                            ui.add_space(12.0);
                            self.render_overlays_section(ui);
                        }
                    });
            });

        // 3. Floating PopupMenu
        let popup_st = PopupMenuStyle::default_popup(&self.theme);
        let toggle_st = ToggleStyle::default_toggle(&self.theme);
        let menu_items = vec![
            MenuItem::action("reply", "Reply")
                .with_icon(IconKind::Back)
                .with_shortcut("Ctrl+R"),
            MenuItem::action("edit", "Edit message")
                .with_icon(IconKind::Edit)
                .with_shortcut("Up"),
            MenuItem::action("more", "Forward to...")
                .with_icon(IconKind::Send)
                .with_submenu(),
            MenuItem::Toggle {
                id: "mute".to_owned(),
                label: "Mute notifications".to_owned(),
                checked: self.menu_mute_checked,
                icon: Some(IconKind::Settings),
            },
            MenuItem::Separator,
            MenuItem::action("disabled_act", "Disabled action").disabled(),
            MenuItem::action("delete", "Delete chat")
                .with_icon(IconKind::Trash)
                .attention(),
        ];
        if let Some(act) = self.popup_menu.show(
            &ctx,
            Id::unique("gallery_popup_menu"),
            &menu_items,
            &popup_st,
            &toggle_st,
        ) {
            if act == "mute" {
                self.menu_mute_checked = !self.menu_mute_checked;
            }
            self.status_line = format!("PopupMenu triggered: {act}");
        }

        // 4. Stacked Modal Layers (`LayerStack` + `BoxModal`)
        let box_st = BoxStyle::default_box(&self.theme);
        let mut open_confirm = false;
        let mut close_top = false;
        if let Some(ev) =
            self.layers
                .show(&ctx, viewport, &box_st, &self.theme, |box_id, ui| {
                    if box_id == "settings_box" {
                        FlatLabel::plain(
                            "This modal box uses tdesktop's LayerStack, 8px rounded corners, dimmed backdrop, and live tg-theme colors.",
                            FlatLabelStyle::box_label(&self.theme),
                        )
                        .id_salt("box_desc")
                        .show(ui);
                        ui.add_space(10.0);
                        InputField::single_line(
                            "box_note_input",
                            &mut self.box_note,
                            "Display note",
                            InputFieldStyle::default_input(&self.theme),
                        )
                        .show(ui);
                    } else if box_id == "confirm_box" {
                        FlatLabel::plain(
                            "Are you sure you want to log out? This is Layer 2 stacked over Layer 1.",
                            FlatLabelStyle::box_label(&self.theme),
                        )
                        .id_salt("confirm_desc")
                        .show(ui);
                    }
                })
        {
            match (ev.box_id.as_str(), ev.action_id.as_str()) {
                ("settings_box", "stack") => open_confirm = true,
                ("settings_box", "save") => {
                    self.status_line = format!("Saved box note: {}", self.box_note);
                    close_top = true;
                }
                (_, "cancel") | (_, "confirm_logout") => {
                    self.status_line = format!("Box '{}' action: {}", ev.box_id, ev.action_id);
                    close_top = true;
                }
                _ => {}
            }
        }
        if open_confirm {
            self.layers.push(
                &ctx,
                BoxModal::new("confirm_box", "Confirm Action")
                    .width(320.0)
                    .additional_title("Layer 2")
                    .add_button(BoxButton::default_btn("cancel", "Cancel"))
                    .add_button(BoxButton::attention("confirm_logout", "Log Out")),
                &box_st,
            );
        } else if close_top {
            self.layers.pop(&ctx, &box_st);
        }

        // 5. Toasts above everything
        self.toasts
            .paint(&ctx, viewport, &ToastStyle::default_toast(&self.theme));

        // 6. Optional automated screenshot capture via `--screenshot <path>`
        if let Some(path) = self.screenshot_path.clone() {
            self.frames_rendered += 1;
            if self.screenshot_done.load(Ordering::Acquire) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if !self.screenshot_requested && self.frames_rendered >= 10 {
                self.screenshot_requested = true;
                let done = Arc::clone(&self.screenshot_done);
                let ctx_cb = ctx.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::ScreenshotCallback::new(move |img| {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let [w, h] = img.size;
                        let mut raw = Vec::with_capacity(w * h * 4);
                        for px in &img.pixels {
                            let [r, g, b, a] = px.to_srgba_unmultiplied();
                            raw.extend_from_slice(&[r, g, b, a]);
                        }
                        if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                            let _ = buf.save(&path);
                        }
                        done.store(true, Ordering::Release);
                        ctx_cb.request_repaint();
                    }),
                ));
            } else {
                ctx.request_repaint();
            }
        }
    }
}

fn parse_theme_arg(val: &str) -> EmbeddedType {
    match val.to_ascii_lowercase().as_str() {
        "day-blue" | "day_blue" | "blue" => EmbeddedType::DayBlue,
        "night" | "tinted" | "dark" => EmbeddedType::Night,
        "night-green" | "night_green" | "green" => EmbeddedType::NightGreen,
        _ => EmbeddedType::Default,
    }
}

fn main() -> eframe::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut screenshot_path: Option<PathBuf> = None;
    let mut initial_theme = EmbeddedType::Default;

    while let Some(arg) = args.next() {
        if arg == "--screenshot" {
            screenshot_path = args.next().map(PathBuf::from);
        } else if arg == "--theme"
            && let Some(val) = args.next()
        {
            initial_theme = parse_theme_arg(&val);
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Rustgram — tg-ui Widget Gallery")
            .with_inner_size([1180.0, 960.0])
            .with_min_inner_size([720.0, 520.0])
            .with_decorations(false),
        ..Default::default()
    };

    eframe::run_native(
        "Rustgram UI Gallery",
        options,
        Box::new(move |cc| {
            Ok(Box::new(GalleryApp::new(
                cc,
                initial_theme,
                screenshot_path,
            )))
        }),
    )
}
