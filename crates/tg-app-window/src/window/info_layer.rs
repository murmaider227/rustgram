//! The profile as a layer over the window (`Info::LayerWidget`, `info/info_layer_widget.cpp`).
//!
//! `MainWidget::showNewSection` turns an `Info::Memento` shown without `thirdColumn` into a
//! layer whenever the window is at least `LayerWidget::MinimalSupportedWidth()` wide; only the
//! top bar's info button (`toggleInfoSection`) uses the third column. Rustgram keeps such a
//! profile as the main section and draws it here, over the chat.

use egui::{Area, CornerRadius, Id, Order, Pos2, Rect, Sense, Ui, UiBuilder, Vec2};
use tg_app_core::session_api::AppSession;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_info::info::{InfoWrapEvent, InfoWrapWidget};
use tg_ui::tg_style::st;
use tg_ui::theme::Colors;

use crate::window::main_widget::{
    MainWidgetAction, SETTINGS_LAYER_MIN_WIDTH, SETTINGS_LAYER_WIDTH,
};
use crate::window::section::{InfoTab, SectionMemento, SectionShow};
use crate::window::session_controller::SessionController;

/// Whether the main section is a profile drawn as a layer this frame: a profile (not the
/// Downloads manager) in a window at least `LayerWidget::MinimalSupportedWidth()` wide.
#[must_use]
pub fn info_shown_as_layer(session: &SessionController, window_width: f32) -> bool {
    matches!(
        session.main_section(),
        Some(SectionMemento::PeerInfo { tab, .. }) if *tab != InfoTab::Downloads
    ) && window_width >= SETTINGS_LAYER_MIN_WIDTH
}

/// `LayerWidget::countGeometry`: `st::infoDesiredWidth` wide (at least
/// `st::infoMinimalLayerMargin` from each side), `windowHeight / 24` clamped to
/// `infoLayerTopMinimal..infoLayerTopMaximal` from the top and the same from the bottom.
#[must_use]
pub fn info_layer_rect(body: Rect) -> Rect {
    let width = (body.width() - 2.0 * 48.0).clamp(0.0, SETTINGS_LAYER_WIDTH);
    let top = (body.height() / 24.0).floor().clamp(20.0, 40.0);
    let height = (body.height() - 2.0 * top).max(0.0);
    Rect::from_min_size(
        Pos2::new((body.center().x - width * 0.5).round(), body.top() + top),
        Vec2::new(width, height),
    )
}

/// Closes the layer (`LayerWidget::closeLayer`): back to the chat under it, without a slide.
pub fn close_info_layer(session: &mut SessionController) {
    session.show_back_from_stack(SectionShow {
        animated: false,
        ..SectionShow::forward()
    });
}

/// Draws `info` for `peer_id` as a layer over `body`: the dimmed backdrop (a click on it
/// closes the layer) and the rounded profile card with the x in its top bar.
#[allow(clippy::too_many_arguments)]
pub fn render_info_layer(
    ui: &mut Ui,
    body: Rect,
    info: &mut InfoWrapWidget,
    peer_id: PeerId,
    session: &mut SessionController,
    session_backend: &mut dyn AppSession,
    palette: &Palette,
    theme_colors: &dyn Colors,
    actions: &mut Vec<MainWidgetAction>,
) {
    let ctx = ui.ctx().clone();
    let layer = info_layer_rect(body);
    let radius = st::box_radius.0.clamp(0, 32) as u8;
    let area_id = Id::unique("tg_info_layer");
    let mut events = Vec::new();
    let mut close = false;
    info.set_layer_wrap(true);
    info.set_peer(peer_id);
    Area::new(area_id)
        .order(Order::Middle)
        .fixed_pos(body.min)
        .constrain(false)
        .show(&ctx, |ui| {
            ui.set_clip_rect(body);
            let backdrop = ui.interact(body, area_id.with("backdrop"), Sense::click());
            ui.painter()
                .rect_filled(body, CornerRadius::ZERO, palette.layer_backdrop);
            ui.painter()
                .rect_filled(layer, CornerRadius::same(radius), palette.window_bg);
            ui.scope_builder(UiBuilder::new().max_rect(layer), |ui| {
                ui.set_clip_rect(layer);
                events = crate::window::main_widget::show_info_with_sound_toggle(
                    info,
                    peer_id,
                    ui,
                    session_backend,
                    false,
                    theme_colors,
                );
            });
            close = backdrop.clicked()
                && ctx
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| !layer.contains(p));
        });
    info.set_layer_wrap(false);
    for ev in events {
        match ev {
            InfoWrapEvent::CloseOrBack => close = true,
            InfoWrapEvent::OpenHistory(pid) => {
                session.show_peer_history(pid, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
            }
            other => actions.push(MainWidgetAction::InfoEvent(other)),
        }
    }
    if close {
        close_info_layer(session);
    }
}
