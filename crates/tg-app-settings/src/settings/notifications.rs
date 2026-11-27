//! Notifications and Sounds settings section (including interactive corner position picker).
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_notifications.cpp`
//! and `Telegram/SourceFiles/settings/sections/settings_notifications_type.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};
use tg_core::CoreSettings;
use tg_core::settings::{NotifyView, ScreenCorner};
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, MediaSliderStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::sliders::MediaSlider;

use super::backend::{
    ActiveSession, DEFAULT_RINGTONE_MAX_SIZE, MAX_NOTIFICATIONS_COUNT, NotifyException,
    NotifyScope, ReactionsNotifyFrom, ScopeNotifySettings, SettingsBackend, SettingsRead,
    format_bytes,
};
use super::strings;
use super::widgets::{
    SETTINGS_LIST_SKIP, SettingsIconKind, settings_card, settings_checkbox_row,
    settings_divider_text, settings_row_pad_x, settings_section_divider, settings_split_toggle_row,
    settings_toggle_row, subsection_title,
};

/// Persistent UI state for the Notifications & Sounds section.
#[derive(Debug, Clone, Default)]
pub struct NotificationsState {
    /// Currently expanded per-scope editor (`PrivateChats` / `Groups` / `Channels`).
    pub editing_scope: Option<NotifyScope>,
}

/// Output emitted by [`render_notifications_section`].
#[derive(Debug, Clone, Default)]
pub struct NotificationsOutput {
    /// Navigate to `Settings -> Notifications -> Reactions` (`settings_notifications_reactions.cpp`).
    pub open_reactions: bool,
    /// Optional toast message (e.g. after resetting all notification settings).
    pub toast: Option<String>,
}

/// Renders the Notifications and Sounds section (`settings_notifications.cpp`).
pub fn render_notifications_section(
    ui: &mut Ui,
    state: &mut NotificationsState,
    core: &mut CoreSettings,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> NotificationsOutput {
    let mut out = NotificationsOutput::default();
    let toast = &mut out.toast;
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.add_space(st::settings_privacy_skip.scaled() as f32);

    // 1. Multi-account (`BuildMultiAccountSection`): only with more than one account.
    if core.accounts_order.len() > 1 {
        subsection_title(ui, &tr::lng_settings_show_from(), colors);
        let all_accounts_lbl = tr::lng_settings_notify_all();
        settings_toggle_row(
            ui,
            &all_accounts_lbl,
            None,
            None,
            &mut core.notify_from_all,
            true,
            colors,
        );
        ui.add_space(SETTINGS_LIST_SKIP);
        settings_divider_text(ui, &tr::lng_settings_notify_all_about(), colors);
        ui.add_space(SETTINGS_LIST_SKIP);
    }

    // 2. Global settings (`BuildGlobalNotificationsSection`).
    subsection_title(ui, &tr::lng_settings_notify_global(), colors);
    let desktop_notify_lbl = tr::lng_settings_desktop_notify();
    settings_toggle_row(
        ui,
        &desktop_notify_lbl,
        None,
        Some(SettingsIconKind::Notifications),
        &mut core.desktop_notify,
        true,
        colors,
    );
    settings_toggle_row(
        ui,
        &tr::lng_settings_alert_windows(),
        None,
        Some(SettingsIconKind::DockBounce),
        &mut core.flash_bounce_notify,
        true,
        colors,
    );
    let allow_sound_lbl = tr::lng_settings_sound_allowed();
    settings_toggle_row(
        ui,
        &allow_sound_lbl,
        None,
        Some(SettingsIconKind::Mic),
        &mut core.sound_notify,
        true,
        colors,
    );

    // Volume (`Ui::AddRingtonesVolumeSlider`): subsection title, slider, value label.
    subsection_title(ui, &tr::lng_settings_master_volume_notifications(), colors);
    let volume = if core.notifications_volume == 0 {
        100
    } else {
        core.notifications_volume.min(100)
    };
    let mut vol = f64::from(volume) / 100.0;
    if labeled_slider_row(
        ui,
        &mut vol,
        &format!("{volume}%"),
        !core.sound_notify,
        colors,
    ) {
        core.notifications_volume = (vol * 100.0).round().clamp(1.0, 100.0) as u16;
    }
    ui.add_space(SETTINGS_LIST_SKIP);

    // 3. Notification view preview (`SetupNotifyViewOptions`): sample toast on the chat
    // wallpaper with the "Name" / "Text" pills; a plain divider when notifications are off.
    if core.desktop_notify {
        render_notify_preview(ui, &mut core.notify_view, colors);
    } else {
        settings_section_divider(ui, colors);
    }

    // 4. Notifications for chats (`BuildNotifyTypeSection`).
    ui.add_space(st::notify_preview_bottom_skip.scaled() as f32);
    subsection_title(ui, &tr::lng_settings_notify_title(), colors);
    for scope in NotifyScope::ALL {
        let mut scope_cfg = backend.scope_notifications(scope);
        // Until the scope's settings arrive the row shows no status and keeps its toggle, so
        // the defaults shown meanwhile are never saved over the server's values.
        let loading = backend.settings_read_pending(SettingsRead::ScopeNotifications(scope));
        let subtitle = if loading {
            String::new()
        } else {
            scope_status(&scope_cfg)
        };
        let icon = match scope {
            NotifyScope::PrivateChats => SettingsIconKind::Account,
            NotifyScope::Groups => SettingsIconKind::Groups,
            NotifyScope::Channels => SettingsIconKind::Channel,
        };
        let scope_title = scope.localized_title();
        let (clicked, toggled) = settings_split_toggle_row(
            ui,
            &scope_title,
            &subtitle,
            Some(icon),
            &mut scope_cfg.enabled,
            colors,
        );
        if toggled && !loading {
            backend.set_scope_notifications(scope, scope_cfg);
        }
        if clicked {
            if state.editing_scope == Some(scope) {
                state.editing_scope = None;
            } else {
                state.editing_scope = Some(scope);
            }
        }
    }

    // Reactions split toggle (`AddReactionsButton`) -> `settings_notifications_reactions.cpp`.
    let mut events = backend.notification_events();
    let mut events_changed = false;
    let react_title = tr::lng_notification_reactions();
    let react_subtitle = if events.reactions_enabled() {
        events.reactions_summary()
    } else {
        tr::lng_notification_click_to_change()
    };
    let mut react_enabled = events.reactions_enabled();
    let (react_clicked, react_toggled) = settings_split_toggle_row(
        ui,
        &react_title,
        &react_subtitle,
        Some(SettingsIconKind::Reactions),
        &mut react_enabled,
        colors,
    );
    if react_toggled {
        events.set_all_reactions_from(if react_enabled {
            ReactionsNotifyFrom::All
        } else {
            ReactionsNotifyFrom::None
        });
        events_changed = true;
    } else if react_clicked {
        out.open_reactions = true;
    }

    if let Some(scope) = state.editing_scope {
        ui.add_space(6.0);
        render_scope_editor(ui, scope, backend, colors, toast);
    }

    // 5. Events (`BuildEventNotificationsSection`).
    ui.add_space(SECTION_SKIP);
    settings_section_divider(ui, colors);
    ui.add_space(SECTION_SKIP);
    subsection_title(ui, &tr::lng_settings_events_title(), colors);
    let joined_lbl = tr::lng_settings_events_joined();
    if settings_toggle_row(
        ui,
        &joined_lbl,
        None,
        Some(SettingsIconKind::Invite),
        &mut events.contact_joined,
        true,
        colors,
    ) {
        events_changed = true;
    }
    let pinned_lbl = tr::lng_settings_events_pinned();
    settings_toggle_row(
        ui,
        &pinned_lbl,
        None,
        Some(SettingsIconKind::Pin),
        &mut core.notify_about_pinned,
        true,
        colors,
    );
    if events_changed {
        backend.set_notification_events(events);
    }

    // 6. Calls (`BuildCallNotificationsSection`): `callsDisabledHere` of the current session.
    ui.add_space(SECTION_SKIP);
    settings_section_divider(ui, colors);
    ui.add_space(SECTION_SKIP);
    subsection_title(ui, &tr::lng_settings_notifications_calls_title(), colors);
    let current = backend
        .active_sessions()
        .into_iter()
        .find(ActiveSession::is_current);
    let mut accept_calls = current.as_ref().is_none_or(|s| s.accept_calls);
    if settings_toggle_row(
        ui,
        &tr::lng_settings_call_accept_calls(),
        None,
        Some(SettingsIconKind::Calls),
        &mut accept_calls,
        true,
        colors,
    ) {
        let accept_secret = current.as_ref().is_none_or(|s| s.accept_secret_chats);
        backend.update_session_permissions(0, accept_calls, accept_secret);
    }

    // 7. Badge counter (`BuildBadgeCounterSection`).
    ui.add_space(SECTION_SKIP);
    settings_section_divider(ui, colors);
    ui.add_space(SECTION_SKIP);
    subsection_title(ui, &tr::lng_settings_badge_title(), colors);
    settings_checkbox_row(
        ui,
        &mut core.include_muted_counter,
        &tr::lng_settings_include_muted(),
        true,
        colors,
    );
    settings_checkbox_row(
        ui,
        &mut core.include_muted_counter_folders,
        &tr::lng_settings_include_muted_folders(),
        true,
        colors,
    );
    settings_checkbox_row(
        ui,
        &mut core.count_unread_messages,
        &tr::lng_settings_count_unread(),
        true,
        colors,
    );

    // 8. System integration (`BuildSystemIntegrationAndAdvancedSection`): the native toggle;
    // with native notifications off, focus mode, corner and count.
    ui.add_space(SECTION_SKIP);
    settings_section_divider(ui, colors);
    ui.add_space(SECTION_SKIP);
    subsection_title(ui, &tr::lng_settings_native_title(), colors);
    let mut native = core.native_notifications.unwrap_or(true);
    if settings_toggle_row(
        ui,
        &tr::lng_settings_use_windows(),
        None,
        None,
        &mut native,
        true,
        colors,
    ) {
        core.native_notifications = Some(native);
    }
    if !native {
        settings_toggle_row(
            ui,
            &tr::lng_settings_skip_in_focus(),
            None,
            None,
            &mut core.skip_toasts_in_focus,
            true,
            colors,
        );
        ui.add_space(SECTION_SKIP);
        settings_section_divider(ui, colors);
        ui.add_space(SECTION_SKIP);
        subsection_title(ui, &tr::lng_settings_notifications_position(), colors);
        render_corner_picker(
            ui,
            &mut core.notifications_corner,
            core.desktop_notify,
            colors,
        );
        ui.add_space(SECTION_SKIP);
        subsection_title(ui, &tr::lng_settings_notifications_count(), colors);
        let clamped_count = core.notifications_count.clamp(1, MAX_NOTIFICATIONS_COUNT);
        core.notifications_count = clamped_count;
        let mut count_norm = f64::from(clamped_count - 1) / f64::from(MAX_NOTIFICATIONS_COUNT - 1);
        if labeled_slider_row(
            ui,
            &mut count_norm,
            &clamped_count.to_string(),
            !core.desktop_notify,
            colors,
        ) {
            core.notifications_count =
                1 + (count_norm * f64::from(MAX_NOTIFICATIONS_COUNT - 1)).round() as i32;
        }
    }

    ui.add_space(16.0);
    out
}

/// Vertical skip around the dividers of this page (`st::settings_checkboxes_skip`).
const SECTION_SKIP: f32 = 12.0;

/// Status line under a per-chat-type split toggle (`AddTypeButton`): "Click here to change"
/// without exceptions, otherwise "On, 2 exceptions" / "Off, 2 exceptions".
fn scope_status(cfg: &ScopeNotifySettings) -> String {
    let count = cfg.exceptions.len();
    if count == 0 {
        return tr::lng_notification_click_to_change();
    }
    let exc = tr::lng_notification_exceptions(count as i64);
    if cfg.enabled {
        tr::lng_notification_on(&exc)
    } else {
        tr::lng_notification_off(&exc)
    }
}

/// Full-width slider row: slider from the row padding, value label right-aligned
/// (`Ui::AddRingtonesVolumeSlider`, `NotificationsCount`). Returns `true` when changed.
fn labeled_slider_row(
    ui: &mut Ui,
    value: &mut f64,
    label: &str,
    disabled: bool,
    colors: &dyn Colors,
) -> bool {
    // `Ui::AddRingtonesVolumeSlider`: `MakeSliderWithLabel(settingsScale, settingsScaleLabel,
    // spacew * 2, width("100%"))` inside `settingsBigScalePadding`.
    let pad = st::settings_big_scale_padding;
    let pad_left = pad.left.scaled() as f32;
    let pad_right = pad.right.scaled() as f32;
    let slider_st = MediaSliderStyle::from_tg(&st::settings_scale, colors);
    let label_font = font_id_from_style(st::settings_scale_label.style.font);
    let label_fg = colors.color("windowActiveTextFg");
    let min_label_w = ui
        .painter()
        .layout_no_wrap("100%".to_owned(), label_font.clone(), label_fg)
        .size()
        .x;
    let label_galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), label_font.clone(), label_fg);
    let space_w = ui
        .painter()
        .layout_no_wrap(
            " ".to_owned(),
            font_id_from_style(st::normal_font),
            label_fg,
        )
        .size()
        .x;
    let row_h = slider_st
        .seek_size
        .height
        .max(ui.fonts_mut(|f| f.row_height(&label_font)));
    let full_w = ui.available_width();
    ui.add_space(pad.top.scaled() as f32);
    let (row_rect, _) = ui.allocate_exact_size(Vec2::new(full_w, row_h), Sense::hover());
    ui.add_space(pad.bottom.scaled() as f32);
    let label_right = label_galley.size().x.max(min_label_w) + space_w * 2.0;
    let slider_w = (full_w - pad_left - pad_right - label_right).max(slider_st.seek_size.width);
    let slider_rect = Rect::from_min_size(
        Pos2::new(row_rect.left() + pad_left, row_rect.top()),
        Vec2::new(slider_w, row_h),
    );
    let changed = ui
        .scope_builder(
            egui::UiBuilder::new()
                .max_rect(slider_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                MediaSlider::new(value, slider_st)
                    .always_display_marker(true)
                    .disabled(disabled)
                    .width(slider_w)
                    .show(ui)
                    .changed()
            },
        )
        .inner;
    ui.painter().galley(
        Pos2::new(
            row_rect.right() - pad_right - label_galley.size().x,
            row_rect.center().y - label_galley.size().y * 0.5,
        ),
        label_galley,
        label_fg,
    );
    changed
}

/// tdesktop's sample-notification userpic (`icons/settings/dino.svg`, GPL-3.0).
const DINO_SVG: &[u8] = include_bytes!("assets/dino.svg");

/// Rasterizes (once per context) the Dino Rex userpic used by the notification preview.
fn dino_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let id = egui::Id::unique("tg_settings_notify_preview_dino");
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(tex);
    }
    let size = 96_u32;
    let tree = resvg::usvg::Tree::from_data(DINO_SVG, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let tree_size = tree.size();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(
            size as f32 / tree_size.width().max(1.0),
            size as f32 / tree_size.height().max(1.0),
        ),
        &mut pixmap.as_mut(),
    );
    let image =
        egui::ColorImage::from_rgba_premultiplied([size as usize, size as usize], pixmap.data());
    let tex = ctx.load_texture(
        "tg_settings_notify_dino",
        image,
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    Some(tex)
}

/// Notification preview card (`NotifyPreview` + `SetupNotifyViewOptions`): the chat wallpaper,
/// a `msgInBg` sample toast ("Dino Rex" / "It's morning in Tokyo"), and the "Name" / "Text"
/// service pills that set [`NotifyView`].
fn render_notify_preview(ui: &mut Ui, view: &mut NotifyView, colors: &dyn Colors) {
    // `st::notify_preview_margins` (40, 20, 40, 58), userpic 36 px at (14, 13), text at 64.
    let (m_left, m_top, m_right, m_bottom) = (40.0_f32, 20.0_f32, 40.0_f32, 58.0_f32);
    let card_h = 58.0_f32;
    let width = ui.available_width();
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(width, m_top + card_h + m_bottom), Sense::hover());
    {
        let bg = crate::themes::active_chat_background(ui.ctx());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        if let Ok(mut bg) = bg.lock() {
            // `SectionWidget::PaintBackground` with `QSize(width, window height)`: the strip
            // shows the top of a window-tall wallpaper, so the pattern keeps its chat scale.
            let window_h = ui.ctx().content_rect().height().max(rect.height());
            let area = Rect::from_min_size(rect.min, Vec2::new(rect.width(), window_h));
            bg.paint(&mut child, area, None);
        };
    }

    let card = Rect::from_min_size(
        Pos2::new(rect.left() + m_left, rect.top() + m_top),
        Vec2::new((width - m_left - m_right).max(120.0), card_h),
    );
    let radius = st::box_radius.scaled().clamp(2, 16) as u8;
    ui.painter()
        .rect_filled(card, CornerRadius::same(radius), colors.color("msgInBg"));

    let name_shown = *view != NotifyView::ShowNothing;
    let text_shown = *view == NotifyView::ShowPreview;
    let userpic = Rect::from_min_size(card.min + Vec2::new(14.0, 11.0), Vec2::splat(36.0));
    if let Some(tex) = name_shown.then(|| dino_texture(ui.ctx())).flatten() {
        ui.painter().image(
            tex.id(),
            userpic,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        ui.painter()
            .circle_filled(userpic.center(), 18.0, colors.color("windowBgActive"));
    }
    let text_fg = colors.color("historyTextInFg");
    let painter = ui.painter().with_clip_rect(card.shrink(4.0));
    let title = if name_shown {
        tr::lng_notification_preview_title()
    } else {
        "Rustgram".to_owned()
    };
    painter.text(
        Pos2::new(card.left() + 64.0, card.top() + 9.0),
        Align2::LEFT_TOP,
        title,
        font_id_from_style(st::default_subsection_title.style.font),
        text_fg,
    );
    let body = if text_shown {
        tr::lng_notification_preview_text()
    } else {
        tr::lng_notification_preview()
    };
    painter.text(
        Pos2::new(card.left() + 64.0, card.top() + 30.0),
        Align2::LEFT_TOP,
        body,
        font_id_from_style(st::box_text_style.font),
        text_fg,
    );

    // "Name" / "Text" pills (`Ui::MakeChatServiceCheckbox`, `st::background_checkbox`).
    let pill_font = font_id_from_style(st::semibold_font);
    let labels = [
        tr::lng_notification_show_name(),
        tr::lng_notification_show_text(),
    ];
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap(l.clone(), pill_font.clone(), Color32::WHITE)
                .size()
                .x
                + 52.0
        })
        .collect();
    let skip = 12.0_f32;
    let total = widths.iter().sum::<f32>() + skip;
    let pill_h = 30.0_f32;
    let pill_top = card.bottom() + (m_bottom - pill_h) * 0.5;
    let mut x = rect.center().x - total * 0.5;
    let service_bg = colors.color("msgServiceBg");
    let service_fg = colors.color("msgServiceFg");
    for (i, label) in labels.iter().enumerate() {
        let pill = Rect::from_min_size(Pos2::new(x, pill_top), Vec2::new(widths[i], pill_h));
        x += widths[i] + skip;
        let resp = ui.interact(
            pill,
            ui.scope_id().with(("notify_view_pill", i)),
            Sense::click(),
        );
        let checked = if i == 0 { name_shown } else { text_shown };
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::CheckBox, true, checked, label.clone())
        });
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            *view = match (i, checked) {
                (0, true) => NotifyView::ShowNothing,
                (0, false) => NotifyView::ShowName,
                (_, true) => NotifyView::ShowName,
                (_, false) => NotifyView::ShowPreview,
            };
        }
        ui.painter()
            .rect_filled(pill, CornerRadius::same((pill_h * 0.5) as u8), service_bg);
        let check_c = Pos2::new(pill.left() + 18.0, pill.center().y);
        if checked {
            ui.painter().circle_filled(check_c, 9.0, service_fg);
            ui.painter().add(egui::Shape::line(
                vec![
                    Pos2::new(check_c.x - 4.0, check_c.y + 0.5),
                    Pos2::new(check_c.x - 1.0, check_c.y + 3.5),
                    Pos2::new(check_c.x + 4.5, check_c.y - 3.0),
                ],
                Stroke::new(1.8, service_bg),
            ));
        } else {
            ui.painter()
                .circle_stroke(check_c, 8.0, Stroke::new(1.6, service_fg));
        }
        ui.painter().text(
            Pos2::new(pill.left() + 36.0, pill.center().y),
            Align2::LEFT_CENTER,
            label,
            pill_font.clone(),
            service_fg,
        );
    }
}

/// Interactive mini-monitor 4-corner notification position picker (`settings_notifications.cpp`).
pub fn render_corner_picker(
    ui: &mut Ui,
    corner: &mut ScreenCorner,
    enabled: bool,
    colors: &dyn Colors,
) {
    let pad_x = settings_row_pad_x();
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        let monitor_w = 240.0;
        let monitor_h = 136.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(monitor_w, monitor_h), Sense::hover());

        let screen_bg = colors.color("windowBgOver");
        let border_col = colors.color("windowSubTextFg");
        let active_col = colors.color("windowBgActive");
        let inactive_col = colors.color("inputBorderFg");

        // Monitor bezel.
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), screen_bg);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(2.0, border_col),
            StrokeKind::Inside,
        );

        let pad = 12.0;
        let box_w = 88.0;
        let box_h = 44.0;
        let corners = [
            (
                ScreenCorner::TopLeft,
                Rect::from_min_size(
                    Pos2::new(rect.left() + pad, rect.top() + pad),
                    Vec2::new(box_w, box_h),
                ),
                strings::CORNER_TOP_LEFT,
            ),
            (
                ScreenCorner::TopRight,
                Rect::from_min_size(
                    Pos2::new(rect.right() - pad - box_w, rect.top() + pad),
                    Vec2::new(box_w, box_h),
                ),
                strings::CORNER_TOP_RIGHT,
            ),
            (
                ScreenCorner::BottomLeft,
                Rect::from_min_size(
                    Pos2::new(rect.left() + pad, rect.bottom() - pad - box_h),
                    Vec2::new(box_w, box_h),
                ),
                strings::CORNER_BOTTOM_LEFT,
            ),
            (
                ScreenCorner::BottomRight,
                Rect::from_min_size(
                    Pos2::new(rect.right() - pad - box_w, rect.bottom() - pad - box_h),
                    Vec2::new(box_w, box_h),
                ),
                strings::CORNER_BOTTOM_RIGHT,
            ),
        ];

        for (candidate, c_rect, label) in corners {
            let sense = if enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let resp = ui.interact(
                c_rect,
                ui.scope_id().with(("notify_corner", candidate as i32)),
                sense,
            );
            let selected = *corner == candidate;
            if enabled && resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if enabled && resp.clicked() {
                *corner = candidate;
            }

            let fill = if selected {
                active_col
            } else if resp.hovered() && enabled {
                colors.color("lightButtonBgOver")
            } else {
                colors.color("windowBg")
            };
            let stroke = if selected {
                Stroke::new(1.5, active_col)
            } else {
                Stroke::new(1.0, inactive_col)
            };
            ui.painter()
                .rect_filled(c_rect, CornerRadius::same(5), fill);
            ui.painter()
                .rect_stroke(c_rect, CornerRadius::same(5), stroke, StrokeKind::Inside);
            ui.painter().text(
                c_rect.center(),
                Align2::CENTER_CENTER,
                label,
                egui::FontId::new(11.0, tg_ui::fonts::family_regular()),
                if selected {
                    Color32::WHITE
                } else {
                    colors.color("windowFg")
                },
            );
        }

        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(strings::CORNER_SCREEN_CORNER)
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.add_space(4.0);
            let name = match corner {
                ScreenCorner::TopLeft => strings::CORNER_TOP_LEFT,
                ScreenCorner::TopRight => strings::CORNER_TOP_RIGHT,
                ScreenCorner::BottomLeft => strings::CORNER_BOTTOM_LEFT,
                ScreenCorner::BottomRight => strings::CORNER_BOTTOM_RIGHT,
            };
            ui.label(
                egui::RichText::new(format!("{} {name}", strings::CORNER_SELECTED_PREFIX))
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowActiveTextFg")),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(strings::CORNER_CLICK_HINT)
                    .font(egui::FontId::new(11.5, tg_ui::fonts::family_regular()))
                    .color(colors.color("windowSubTextFg")),
            );
        });
    });
}

/// Stored value of the "Default" sound (tdesktop's `msg_incoming.mp3`).
pub const SOUND_DEFAULT: &str = "Default";
/// Stored value of "No sound".
pub const SOUND_NONE: &str = "None";

/// The built-in entries of tdesktop's notification sound list, in its order (`RingtonesBox`,
/// `boxes/ringtones_box.cpp:237-247`): "Default", then "No sound". Saved cloud ringtones follow.
#[must_use]
pub fn scope_sound_choices() -> [(&'static str, String); 2] {
    [
        (SOUND_DEFAULT, tr::lng_ringtones_box_default()),
        (SOUND_NONE, tr::lng_ringtones_box_no_sound()),
    ]
}

fn render_scope_editor(
    ui: &mut Ui,
    scope: NotifyScope,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    toast: &mut Option<String>,
) {
    let mut cfg = backend.scope_notifications(scope);
    let contacts = backend.contacts();
    let mut changed = false;

    let heading = match scope {
        NotifyScope::PrivateChats => tr::lng_notification_title_private_chats(),
        NotifyScope::Groups => tr::lng_notification_title_groups(),
        NotifyScope::Channels => tr::lng_notification_title_channels(),
    };

    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(heading)
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(6.0);
        if ui
            .checkbox(&mut cfg.show_preview, strings::SCOPE_SHOW_PREVIEW)
            .changed()
        {
            changed = true;
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!("{}:", tr::lng_notification_sound()))
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowFg")),
            );
            for (value, label) in scope_sound_choices() {
                let selected = cfg.sound.eq_ignore_ascii_case(value)
                    || (value == SOUND_DEFAULT && cfg.sound.is_empty());
                if ui.selectable_label(selected, label).clicked() {
                    cfg.sound = value.to_owned();
                    changed = true;
                }
            }
            for tone in backend.saved_ringtones() {
                let label = tone.display_name();
                let key = format!("Ringtone {}", tone.id);
                let is_selected = cfg.sound == label || cfg.sound == key;
                if ui.selectable_label(is_selected, &label).clicked() {
                    cfg.sound = label;
                    changed = true;
                }
            }
        });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!(
                "{} ({}):",
                tr::lng_notification_exceptions_title(),
                cfg.exceptions.len()
            ))
            .font(font_id_from_style(st::semibold_font))
            .color(colors.color("windowBoldFg")),
        );
        let mut remove_idx = None;
        let muted_lbl = tr::lng_notification_exceptions_muted();
        let unmuted_lbl = tr::lng_notification_exceptions_unmuted();
        let remove_lbl = tr::lng_notification_exceptions_remove();
        for (idx, exc) in cfg.exceptions.iter().enumerate() {
            ui.horizontal(|ui| {
                let status = if exc.muted { &muted_lbl } else { &unmuted_lbl };
                ui.label(
                    egui::RichText::new(format!("{} — {status}", exc.peer.name))
                        .font(font_id_from_style(st::normal_font))
                        .color(colors.color("windowFg")),
                );
                if ui.small_button(&remove_lbl).clicked() {
                    remove_idx = Some(idx);
                }
            });
        }
        if let Some(idx) = remove_idx {
            cfg.exceptions.remove(idx);
            changed = true;
        }
        // `lng_notification_exceptions_clear` (`settings_notifications_type.cpp`).
        if !cfg.exceptions.is_empty() {
            ui.add_space(4.0);
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(tr::lng_notification_exceptions_clear())
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("attentionButtonFg")),
                    )
                    .frame(false),
                )
                .clicked()
            {
                cfg.exceptions.clear();
                changed = true;
            }
        }

        // Quick-add exception from known contacts matching scope.
        if let Some(candidate) = contacts.into_iter().find(|c| {
            let matches_scope = match scope {
                NotifyScope::PrivateChats => !c.is_group_or_channel,
                NotifyScope::Groups | NotifyScope::Channels => c.is_group_or_channel,
            };
            matches_scope && !cfg.exceptions.iter().any(|e| e.peer.id == c.id)
        }) {
            ui.add_space(6.0);
            let add_label = format!(
                "{}: {}",
                tr::lng_notification_exceptions_add(),
                candidate.name
            );
            if RoundButton::new(&add_label, RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                cfg.exceptions.push(NotifyException {
                    peer: candidate,
                    muted: cfg.enabled,
                    sound: "None".to_owned(),
                });
                changed = true;
                *toast = Some(strings::EXCEPTION_ADDED_TOAST.to_owned());
            }
        }
    });

    if changed && !backend.settings_read_pending(SettingsRead::ScopeNotifications(scope)) {
        backend.set_scope_notifications(scope, cfg);
    }

    // `lng_notification_tone` opens `RingtonesBox`: saved cloud tones + upload.
    ui.add_space(6.0);
    subsection_title(ui, &tr::lng_notification_tone(), colors);
    render_ringtones_subsection(ui, backend, colors, toast);
}

fn render_ringtones_subsection(
    ui: &mut Ui,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
    toast: &mut Option<String>,
) {
    let ringtones = backend.saved_ringtones();
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(tr::lng_ringtones_box_cloud_subtitle())
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(4.0);

        if ringtones.is_empty() {
            ui.label(
                egui::RichText::new(strings::RINGTONES_EMPTY_HINT)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
        } else {
            let delete_lbl = tr::lng_box_delete();
            for tone in &ringtones {
                ui.horizontal(|ui| {
                    let meta = if tone.duration_secs > 0.0 {
                        format!(
                            "{}  ·  {:.1}s  ·  {}",
                            tone.display_name(),
                            tone.duration_secs,
                            format_bytes(tone.size)
                        )
                    } else {
                        format!("{}  ·  {}", tone.display_name(), format_bytes(tone.size))
                    };
                    ui.label(
                        egui::RichText::new(meta)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("windowFg")),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(&delete_lbl).clicked() {
                            match backend.save_ringtone(tone.id, true) {
                                Ok(()) => *toast = Some(strings::RINGTONE_REMOVED_TOAST.to_owned()),
                                Err(err) => *toast = Some(err),
                            }
                        }
                    });
                });
            }
        }

        ui.add_space(8.0);
        if RoundButton::new(
            &tr::lng_ringtones_box_upload_button(),
            RoundButtonStyle::default_light(colors),
        )
        .show(ui)
        .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .set_title(tr::lng_ringtones_box_upload_choose())
                .add_filter("Audio files", &["mp3", "ogg", "opus", "wav"])
                .pick_file()
        {
            match std::fs::read(&path) {
                Ok(bytes) if bytes.len() > DEFAULT_RINGTONE_MAX_SIZE => {
                    let limit = format_bytes(DEFAULT_RINGTONE_MAX_SIZE as i64);
                    *toast = Some(tr::lng_ringtones_error_max_size(&limit));
                }
                Ok(bytes) => {
                    let file_name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("ringtone.mp3")
                        .to_owned();
                    let mime = ringtone_mime_for_filename(&file_name);
                    match backend.upload_ringtone(&file_name, mime, &bytes) {
                        Ok(_) => *toast = Some(tr::lng_ringtones_toast_added()),
                        Err(err) => *toast = Some(err),
                    }
                }
                Err(err) => *toast = Some(format!("Failed to read ringtone file: {err}")),
            }
        }

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(tr::lng_ringtones_box_about())
                .font(egui::FontId::new(11.5, tg_ui::fonts::family_regular()))
                .color(colors.color("windowSubTextFg")),
        );
    });
}

fn ringtone_mime_for_filename(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".ogg") || lower.ends_with(".opus") {
        "audio/ogg"
    } else if lower.ends_with(".wav") {
        "audio/wav"
    } else {
        "audio/mpeg"
    }
}

#[cfg(test)]
mod sound_list_tests {
    use super::*;

    #[test]
    fn scope_sound_list_matches_tdesktop_ringtones_box() {
        let choices = scope_sound_choices();
        let values: Vec<&str> = choices.iter().map(|(value, _)| *value).collect();
        assert_eq!(values, [SOUND_DEFAULT, SOUND_NONE]);
        assert_eq!(choices[0].1, tr::lng_ringtones_box_default());
        assert_eq!(choices[1].1, tr::lng_ringtones_box_no_sound());
    }
}
