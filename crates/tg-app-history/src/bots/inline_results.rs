//! Inline bot results panel floating above the compose field.
//!
//! Port origin: `Telegram/SourceFiles/inline_bots/inline_results_widget.cpp`,
//! `Telegram/SourceFiles/inline_bots/inline_results_inner.cpp`, and
//! `Telegram/SourceFiles/inline_bots/inline_bot_layout_internal.cpp`.
//!
//! Uses `tg-bots`'s [`InlineQueryController`] (400 ms debounce, cache, `next_offset` pagination)
//! and [`layout_inline_results`] (backed by `tg_bots::mosaic::MosaicLayout`) to render both
//! list mode and gallery mosaic mode across all 12 inline result kinds:
//! article, photo, gif, video, audio, voice, document, sticker, contact, location, venue, game.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};
use tg_bots::inline::{
    GetInlineBotResultsRequest, InlineLayoutKind, InlineQueryAction, InlineQueryController,
    InlineResult, InlineResultKind, InlineResultsPage, SwitchPmButton, layout_inline_results,
    parse_inline_bot_query, style as inline_style,
};
use tg_bots::stand_in::{PeerId, UserId};

use crate::bots::backend::BotsBackend;
use crate::bots::ui_helpers::{
    format_duration, paint_map_thumbnail, paint_procedural_art, with_alpha,
};
use tg_app_core::stand_in::theme::Palette;

/// Height of the `switch_pm` / `switch_webview` header strip (`38px`).
pub const SWITCH_PM_BUTTON_HEIGHT: i32 = 38;

/// Event emitted by [`InlineResultsPanel`] to the hosting chat view.
#[derive(Debug, Clone, PartialEq)]
pub enum InlineResultsEvent {
    /// User clicked an inline result to send into the active chat.
    SendResult {
        /// Inline bot user id.
        bot_id: UserId,
        /// Selected inline result.
        result: Box<InlineResult>,
    },
    /// User clicked the `switch_pm` or `switch_webview` header button.
    SwitchPm {
        /// Inline bot user id.
        bot_id: UserId,
        /// Button payload.
        button: SwitchPmButton,
    },
    /// User clicked the close button on the inline panel.
    Dismissed,
}

/// Stateful inline bot results panel combining [`InlineQueryController`] with egui painting.
#[derive(Debug, Default, Clone)]
pub struct InlineResultsPanel {
    /// Pure debounce / cache / pagination state machine from `tg-bots`.
    pub controller: InlineQueryController,
    /// Active bot username (without `@`).
    pub bot_username: String,
    /// Optional forced gallery override (`None` uses the server's `entry.gallery` flag).
    pub gallery_override: Option<bool>,
    /// Last error string from a failed query, if any.
    pub last_error: Option<String>,
    /// Number of paginated loads performed for diagnostics/tests.
    pub page_loads: usize,
    /// Whether the panel is open.
    pub visible: bool,
}

impl InlineResultsPanel {
    /// Creates an empty, hidden [`InlineResultsPanel`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses compose text (`@bot query`) and feeds changes into [`InlineQueryController`],
    /// scheduling the 400 ms debounce timer when needed.
    ///
    /// Returns `true` if the compose text currently addresses `expected_bot_username`.
    pub fn sync_from_compose_text(
        &mut self,
        compose_text: &str,
        bot_id: UserId,
        expected_bot_username: &str,
        peer_id: PeerId,
        now_ms: u64,
    ) -> bool {
        let Some(parsed) = parse_inline_bot_query(compose_text, None) else {
            self.visible = false;
            return false;
        };
        if !parsed.username.eq_ignore_ascii_case(expected_bot_username) {
            self.visible = false;
            return false;
        }
        self.bot_username = parsed.username;
        self.visible = true;
        match self
            .controller
            .on_query_changed(bot_id, peer_id, parsed.query, now_ms)
        {
            InlineQueryAction::NoOp | InlineQueryAction::ShowCached { .. } => {}
            InlineQueryAction::ScheduleDebounce { .. } => {
                self.last_error = None;
            }
        }
        true
    }

    /// Advances the 400 ms debounce timer against `now_ms`, firing `backend.query_inline_results`
    /// once the deadline expires. Returns `true` if a new page was fetched.
    pub fn poll_debounce(&mut self, now_ms: u64, backend: &mut dyn BotsBackend) -> bool {
        let Some(req) = self.controller.on_timer_tick(now_ms) else {
            return false;
        };
        self.execute_request(&req, backend)
    }

    /// Immediately triggers a query (bypassing the 400 ms wait; useful for demo presets & tests).
    pub fn fetch_immediately(
        &mut self,
        bot_id: UserId,
        bot_username: impl Into<String>,
        peer_id: PeerId,
        query: impl Into<String>,
        now_ms: u64,
        backend: &mut dyn BotsBackend,
    ) -> bool {
        self.bot_username = bot_username.into();
        self.visible = true;
        let action = self
            .controller
            .on_query_changed(bot_id, peer_id, query, now_ms);
        match action {
            InlineQueryAction::ShowCached { .. } | InlineQueryAction::NoOp => true,
            InlineQueryAction::ScheduleDebounce { delay_ms, .. } => {
                self.poll_debounce(now_ms.saturating_add(delay_ms), backend)
            }
        }
    }

    /// Requests the next page using `next_offset` (`InlineQueryController::on_scroll_bottom`).
    pub fn load_more(&mut self, backend: &mut dyn BotsBackend) -> bool {
        let Some(req) = self.controller.on_scroll_bottom() else {
            return false;
        };
        self.execute_request(&req, backend)
    }

    /// Applies a pre-built [`InlineResultsPage`] directly into the controller.
    pub fn apply_page(&mut self, page: InlineResultsPage) -> usize {
        self.last_error = None;
        self.page_loads += 1;
        self.controller.on_results_received(page)
    }

    fn execute_request(
        &mut self,
        req: &GetInlineBotResultsRequest,
        backend: &mut dyn BotsBackend,
    ) -> bool {
        match backend.query_inline_results(req) {
            Ok(page) => {
                self.apply_page(page);
                true
            }
            Err(err) => {
                self.controller.on_request_failed();
                let _ = tg_app_core::user_error::inline_query(&err);
                self.last_error = Some(err); // local error: a failed-state flag, never drawn
                false
            }
        }
    }

    /// Returns `true` when the panel has a cached entry with visible content or is loading.
    #[must_use]
    pub fn should_render(&self) -> bool {
        if !self.visible {
            return false;
        }
        self.controller.is_requesting()
            || self.controller.debounce_deadline_ms().is_some()
            || self.last_error.is_some()
            || self
                .controller
                .current_entry()
                .is_some_and(|e| e.has_visible_content())
    }

    /// Renders the floating inline results panel above the compose area.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        max_width: f32,
        max_height: f32,
        palette: &Palette,
        backend: &mut dyn BotsBackend,
    ) -> Option<InlineResultsEvent> {
        if !self.should_render() {
            return None;
        }

        let panel_w = max_width.clamp(320.0, 560.0);
        let entry = self.controller.current_entry().cloned();
        let gallery = self
            .gallery_override
            .unwrap_or_else(|| entry.as_ref().is_some_and(|e| e.gallery));
        let switch_pm = entry.as_ref().and_then(|e| e.switch_pm.clone());
        let results = entry.as_ref().map_or(&[][..], |e| e.results.as_slice());
        let has_next_offset = entry.as_ref().is_some_and(|e| !e.next_offset.is_empty());

        let inner_w = (panel_w - inline_style::EMOJI_SCROLL_WIDTH as f32).max(280.0);
        let big_w = (inner_w as i32 - inline_style::INLINE_RESULTS_LEFT).max(240);
        let layout_out =
            layout_inline_results(results, gallery, inner_w.round() as i32, big_w, None);

        let header_h = 34.0;
        let switch_h = if switch_pm.is_some() {
            SWITCH_PM_BUTTON_HEIGHT as f32 + 4.0
        } else {
            0.0
        };
        let footer_h = if has_next_offset { 36.0 } else { 0.0 };
        let body_h = (layout_out.total_height as f32 + footer_h)
            .clamp(72.0, (max_height - header_h - switch_h).max(120.0));
        let total_panel_h = header_h + switch_h + body_h;

        let mut emitted = None;
        let mut trigger_load_more = false;
        let bot_id = self.controller.bot_id().unwrap_or(UserId(0));

        let avail_h = ui.available_height();
        if avail_h > total_panel_h {
            ui.add_space(avail_h - total_panel_h);
        }
        let (panel_rect, _) =
            ui.allocate_exact_size(Vec2::new(panel_w, total_panel_h), Sense::hover());

        // Soft drop shadow + panel frame
        ui.painter().rect_filled(
            panel_rect.expand(1.5),
            CornerRadius::same(11),
            with_alpha(palette.divider, 0.9),
        );
        ui.painter()
            .rect_filled(panel_rect, CornerRadius::same(10), palette.window_bg);

        // Top status bar: `@bot` query, List / Gallery indicator, close button
        let header_rect = Rect::from_min_size(panel_rect.min, Vec2::new(panel_w, header_h));
        ui.painter().rect_filled(
            header_rect,
            CornerRadius {
                nw: 10,
                ne: 10,
                sw: 0,
                se: 0,
            },
            palette.window_bg_over,
        );
        ui.painter().line_segment(
            [header_rect.left_bottom(), header_rect.right_bottom()],
            Stroke::new(1.0, palette.divider),
        );

        let mode_tag = if gallery { "Gallery Mosaic" } else { "List" };
        let status_label = if self.controller.debounce_deadline_ms().is_some() {
            format!("@{} — waiting (400ms debounce)…", self.bot_username)
        } else if self.controller.is_requesting() {
            format!("@{} — loading results…", self.bot_username)
        } else {
            format!(
                "@{} • {} • {} results",
                self.bot_username,
                mode_tag,
                results.len()
            )
        };
        ui.painter().text(
            Pos2::new(header_rect.left() + 12.0, header_rect.center().y),
            Align2::LEFT_CENTER,
            status_label,
            FontId::proportional(12.0),
            palette.window_sub_text_fg,
        );

        // Toggle List / Gallery mode pill in header
        let mode_btn_rect = Rect::from_min_size(
            Pos2::new(header_rect.right() - 108.0, header_rect.top() + 6.0),
            Vec2::new(72.0, 22.0),
        );
        let mode_resp = ui.interact(
            mode_btn_rect,
            Id::unique("tg_inline_mode_toggle"),
            Sense::click(),
        );
        mode_resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                true,
                if gallery { "List view" } else { "Mosaic" },
            )
        });
        if mode_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        ui.painter().rect_filled(
            mode_btn_rect,
            CornerRadius::same(11),
            if mode_resp.hovered() {
                with_alpha(palette.window_bg_active, 0.18)
            } else {
                palette.window_bg
            },
        );
        ui.painter().rect_stroke(
            mode_btn_rect,
            CornerRadius::same(11),
            Stroke::new(1.0, palette.divider),
            StrokeKind::Inside,
        );
        ui.painter().text(
            mode_btn_rect.center(),
            Align2::CENTER_CENTER,
            if gallery { "List view" } else { "Mosaic" },
            FontId::proportional(11.0),
            palette.window_active_text_fg,
        );
        if mode_resp.clicked() {
            self.gallery_override = Some(!gallery);
        }

        // Close button
        let close_rect = Rect::from_min_size(
            Pos2::new(header_rect.right() - 28.0, header_rect.top() + 6.0),
            Vec2::splat(22.0),
        );
        let close_resp = ui.interact(close_rect, Id::unique("tg_inline_close"), Sense::click());
        close_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, "Close inline results")
        });
        if close_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(close_rect.center(), 10.0, palette.window_bg);
        }
        ui.painter().text(
            close_rect.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::proportional(14.5),
            palette.window_sub_text_fg,
        );
        if close_resp.clicked() {
            self.visible = false;
            emitted = Some(InlineResultsEvent::Dismissed);
        }

        // Optional `switch_pm` / `switch_webview` button strip
        let mut body_top = header_rect.bottom();
        if let Some(ref btn) = switch_pm {
            let strip_rect = Rect::from_min_size(
                Pos2::new(panel_rect.left() + 10.0, body_top + 4.0),
                Vec2::new(panel_w - 20.0, SWITCH_PM_BUTTON_HEIGHT as f32 - 4.0),
            );
            let resp = ui.interact(
                strip_rect,
                Id::unique("tg_inline_switch_pm"),
                Sense::click(),
            );
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, btn.text()));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let bg = if resp.hovered() {
                with_alpha(palette.window_bg_active, 0.16)
            } else {
                with_alpha(palette.window_bg_active, 0.09)
            };
            ui.painter()
                .rect_filled(strip_rect, CornerRadius::same(8), bg);
            let icon = match btn {
                SwitchPmButton::StartPm { .. } => tg_ui::tg_style::st::menu_icon_new_window,
                SwitchPmButton::WebView { .. } => tg_ui::tg_style::st::menu_icon_bot,
            };
            let galley = ui.painter().layout_no_wrap(
                btn.text().to_owned(),
                FontId::proportional(13.0),
                palette.window_active_text_fg,
            );
            let icon_w = 14.0;
            let gap = 5.0;
            let total_w = icon_w + gap + galley.size().x;
            let left_x = strip_rect.center().x - total_w * 0.5;
            let icon_rect = Rect::from_center_size(
                Pos2::new(left_x + icon_w * 0.5, strip_rect.center().y),
                Vec2::splat(icon_w),
            );
            tg_app_core::ui_icons::paint_style_tinted(
                ui.painter(),
                icon_rect,
                icon,
                palette.window_active_text_fg,
            );
            ui.painter().galley(
                Pos2::new(
                    left_x + icon_w + gap,
                    strip_rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                palette.window_active_text_fg,
            );
            if resp.clicked() {
                emitted = Some(InlineResultsEvent::SwitchPm {
                    bot_id,
                    button: btn.clone(),
                });
            }
            body_top += switch_h;
        }

        // Scrollable results area laid out by `tg_bots::inline::layout_inline_results`
        let body_rect = Rect::from_min_size(
            Pos2::new(panel_rect.left(), body_top),
            Vec2::new(panel_w, body_h),
        );

        ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
            let scroll_out = ScrollArea::vertical()
                .id_salt(("tg_inline_results_scroll", &self.bot_username, gallery))
                .max_height(body_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // A failed query shows nothing (`inline_results_widget.cpp:476-480`).
                    if self.last_error.is_some() {
                        return;
                    }

                    if results.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new(
                                    if self.controller.debounce_deadline_ms().is_some() {
                                        "Typing… querying bot shortly"
                                    } else {
                                        "No inline results for this query"
                                    },
                                )
                                .color(palette.window_sub_text_fg),
                            );
                        });
                        return;
                    }

                    let canvas_h = layout_out.total_height.max(40) as f32;
                    let (canvas_rect, _) =
                        ui.allocate_exact_size(Vec2::new(inner_w, canvas_h), Sense::hover());

                    for &(idx, layout_kind, r) in &layout_out.items {
                        let Some(res) = results.get(idx) else {
                            continue;
                        };
                        let item_w = if layout_kind.is_full_line() {
                            (inner_w - inline_style::INLINE_RESULTS_LEFT as f32 - 8.0)
                                .max(r.width as f32)
                        } else {
                            r.width as f32
                        };
                        let item_rect = Rect::from_min_size(
                            Pos2::new(
                                canvas_rect.left() + r.x as f32,
                                canvas_rect.top() + r.y as f32,
                            ),
                            Vec2::new(item_w, r.height as f32),
                        );
                        let resp = ui.interact(
                            item_rect,
                            Id::unique(("tg_inline_result_item", idx, &res.id)),
                            Sense::click(),
                        );
                        resp.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Button, true, res.title.clone())
                        });
                        let hovered = resp.hovered();
                        if hovered {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        paint_inline_result_item(
                            ui.painter(),
                            item_rect,
                            idx,
                            res,
                            layout_kind,
                            hovered,
                            palette,
                        );
                        if resp.clicked() {
                            emitted = Some(InlineResultsEvent::SendResult {
                                bot_id,
                                result: Box::new(res.clone()),
                            });
                        }
                    }

                    if has_next_offset {
                        let (more_rect, more_resp) =
                            ui.allocate_exact_size(Vec2::new(inner_w - 12.0, 30.0), Sense::click());
                        more_resp.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::Button,
                                true,
                                "Load more results…",
                            )
                        });
                        if more_resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        ui.painter().rect_filled(
                            more_rect.shrink2(Vec2::new(11.0, 2.0)),
                            CornerRadius::same(6),
                            if more_resp.hovered() {
                                palette.window_bg_over
                            } else {
                                Color32::TRANSPARENT
                            },
                        );
                        ui.painter().text(
                            more_rect.center(),
                            Align2::CENTER_CENTER,
                            "Load more results…",
                            FontId::proportional(12.5),
                            palette.window_active_text_fg,
                        );
                        if more_resp.clicked() {
                            trigger_load_more = true;
                        }
                    }
                });

            // Automatic scroll-near-bottom pagination matching `Widget::onScroll`
            let max_scroll = (scroll_out.content_size.y - scroll_out.inner_rect.height()).max(0.0);
            if has_next_offset
                && max_scroll > 40.0
                && scroll_out.state.clamped_offset().y >= max_scroll - 32.0
            {
                trigger_load_more = true;
            }
        });

        if trigger_load_more {
            let _ = self.load_more(backend);
        }

        emitted
    }
}

/// Paints a single laid-out inline result cell (mosaic gallery tile or full-line list row).
fn paint_inline_result_item(
    painter: &Painter,
    rect: Rect,
    idx: usize,
    res: &InlineResult,
    layout_kind: InlineLayoutKind,
    hovered: bool,
    palette: &Palette,
) {
    let seed = idx as u64 + res.query_id + res.id.len() as u64 * 7;

    match layout_kind {
        InlineLayoutKind::Photo | InlineLayoutKind::Gif | InlineLayoutKind::Thumbnail => {
            let media_rect = Rect::from_min_size(
                rect.min,
                Vec2::new(
                    rect.width(),
                    (rect.height() - inline_style::INLINE_RESULTS_SKIP as f32).max(24.0),
                ),
            );
            let badge = match res.kind {
                InlineResultKind::Gif => Some("GIF"),
                InlineResultKind::Video => Some("VID"),
                InlineResultKind::Game => Some("GAME"),
                _ => None,
            };
            paint_procedural_art(painter, media_rect, seed, CornerRadius::same(5), badge);
            if hovered {
                painter.rect_filled(
                    media_rect,
                    CornerRadius::same(5),
                    Color32::from_rgba_unmultiplied(0, 0, 0, 48),
                );
                if !res.title.is_empty() {
                    let cap_rect = Rect::from_min_max(
                        Pos2::new(media_rect.left(), media_rect.bottom() - 20.0),
                        media_rect.max,
                    );
                    painter.rect_filled(
                        cap_rect,
                        CornerRadius {
                            nw: 0,
                            ne: 0,
                            sw: 5,
                            se: 5,
                        },
                        Color32::from_rgba_unmultiplied(0, 0, 0, 140),
                    );
                    painter.text(
                        Pos2::new(cap_rect.left() + 6.0, cap_rect.center().y),
                        Align2::LEFT_CENTER,
                        &res.title,
                        FontId::proportional(10.5),
                        Color32::WHITE,
                    );
                }
            }
        }
        InlineLayoutKind::Sticker => {
            let cell = rect.shrink(4.0);
            if hovered {
                painter.rect_filled(cell, CornerRadius::same(8), palette.window_bg_over);
            }
            // Vector sticker badge art (star / robot mascot emblem)
            let c = cell.center();
            let r = (cell.width().min(cell.height()) * 0.36).max(14.0);
            let sticker_col = if idx.is_multiple_of(2) {
                Color32::from_rgb(0x40, 0xA7, 0xE3)
            } else {
                Color32::from_rgb(0xF5, 0xA6, 0x23)
            };
            painter.circle_filled(c, r, with_alpha(sticker_col, 0.22));
            painter.circle_stroke(c, r, Stroke::new(2.0, sticker_col));
            painter.circle_filled(Pos2::new(c.x - 5.0, c.y - 3.0), 2.3, sticker_col);
            painter.circle_filled(Pos2::new(c.x + 5.0, c.y - 3.0), 2.3, sticker_col);
            painter.line_segment(
                [
                    Pos2::new(c.x - 5.5, c.y + 4.5),
                    Pos2::new(c.x + 5.5, c.y + 4.5),
                ],
                Stroke::new(2.0, sticker_col),
            );
        }
        InlineLayoutKind::Video => {
            paint_list_row_chrome(painter, rect, hovered, palette);
            let thumb = Rect::from_min_size(
                Pos2::new(
                    rect.left() + 4.0,
                    rect.top() + inline_style::INLINE_ROW_MARGIN as f32,
                ),
                Vec2::splat(inline_style::INLINE_THUMB_SIZE as f32),
            );
            paint_procedural_art(painter, thumb, seed + 3, CornerRadius::same(6), None);
            // Play triangle inside dark circle
            painter.circle_filled(
                thumb.center(),
                13.0,
                Color32::from_rgba_unmultiplied(0, 0, 0, 145),
            );
            let tc = thumb.center();
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(tc.x - 4.0, tc.y - 6.0),
                    Pos2::new(tc.x - 4.0, tc.y + 6.0),
                    Pos2::new(tc.x + 6.5, tc.y),
                ],
                Color32::WHITE,
                Stroke::NONE,
            ));
            if let Some(dur) = res.duration_secs {
                let dur_rect = Rect::from_min_size(
                    Pos2::new(thumb.left() + 4.0, thumb.bottom() - 18.0),
                    Vec2::new(36.0, 14.0),
                );
                painter.rect_filled(
                    dur_rect,
                    CornerRadius::same(3),
                    Color32::from_rgba_unmultiplied(0, 0, 0, 160),
                );
                painter.text(
                    dur_rect.center(),
                    Align2::CENTER_CENTER,
                    format_duration(dur),
                    FontId::proportional(9.5),
                    Color32::WHITE,
                );
            }
            paint_list_row_text(
                painter,
                rect,
                thumb.right() + inline_style::INLINE_THUMB_SKIP as f32,
                &res.title,
                &res.description,
                Some("Video"),
                palette,
            );
        }
        InlineLayoutKind::File => {
            paint_list_row_chrome(painter, rect, hovered, palette);
            let diam = inline_style::INLINE_FILE_SIZE as f32;
            let circle_center = Pos2::new(
                rect.left() + 4.0 + diam / 2.0,
                rect.top() + inline_style::INLINE_ROW_MARGIN as f32 + diam / 2.0,
            );
            let (circle_bg, tag) = match res.kind {
                InlineResultKind::Audio => (Color32::from_rgb(0x40, 0xA7, 0xE3), "Audio"),
                InlineResultKind::Voice => (Color32::from_rgb(0x2E, 0xA6, 0x52), "Voice"),
                _ => (Color32::from_rgb(0x6B, 0x7A, 0x8F), "File"),
            };
            painter.circle_filled(circle_center, diam / 2.0, circle_bg);
            match res.kind {
                InlineResultKind::Audio => {
                    // Play triangle
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            Pos2::new(circle_center.x - 4.0, circle_center.y - 7.0),
                            Pos2::new(circle_center.x - 4.0, circle_center.y + 7.0),
                            Pos2::new(circle_center.x + 8.0, circle_center.y),
                        ],
                        Color32::WHITE,
                        Stroke::NONE,
                    ));
                }
                InlineResultKind::Voice => {
                    // Waveform bars
                    for (dx, h) in [
                        (-8.0_f32, 5.0_f32),
                        (-4.0, 10.0),
                        (0.0, 13.0),
                        (4.0, 8.0),
                        (8.0, 5.0),
                    ] {
                        painter.line_segment(
                            [
                                Pos2::new(circle_center.x + dx, circle_center.y - h / 2.0),
                                Pos2::new(circle_center.x + dx, circle_center.y + h / 2.0),
                            ],
                            Stroke::new(2.2, Color32::WHITE),
                        );
                    }
                }
                _ => {
                    // Document glyph
                    let doc = Rect::from_center_size(circle_center, Vec2::new(14.0, 18.0));
                    painter.rect_stroke(
                        doc,
                        CornerRadius::same(2),
                        Stroke::new(1.8, Color32::WHITE),
                        StrokeKind::Inside,
                    );
                    painter.line_segment(
                        [
                            Pos2::new(doc.left() + 3.0, doc.center().y - 2.0),
                            Pos2::new(doc.right() - 3.0, doc.center().y - 2.0),
                        ],
                        Stroke::new(1.4, Color32::WHITE),
                    );
                    painter.line_segment(
                        [
                            Pos2::new(doc.left() + 3.0, doc.center().y + 3.0),
                            Pos2::new(doc.right() - 4.0, doc.center().y + 3.0),
                        ],
                        Stroke::new(1.4, Color32::WHITE),
                    );
                }
            }
            let desc = if let Some(dur) = res.duration_secs {
                if res.description.is_empty() {
                    format_duration(dur)
                } else {
                    format!("{} • {}", format_duration(dur), res.description)
                }
            } else {
                res.description.clone()
            };
            paint_list_row_text(
                painter,
                rect,
                rect.left() + 4.0 + diam + inline_style::INLINE_THUMB_SKIP as f32,
                &res.title,
                &desc,
                Some(tag),
                palette,
            );
        }
        InlineLayoutKind::Contact => {
            paint_list_row_chrome(painter, rect, hovered, palette);
            let diam = inline_style::INLINE_FILE_SIZE as f32;
            let c = Pos2::new(
                rect.left() + 4.0 + diam / 2.0,
                rect.top() + inline_style::INLINE_ROW_MARGIN as f32 + diam / 2.0,
            );
            painter.circle_filled(c, diam / 2.0, Color32::from_rgb(0x52, 0x9E, 0xD8));
            let letter = res
                .title
                .chars()
                .next()
                .unwrap_or('C')
                .to_ascii_uppercase()
                .to_string();
            painter.text(
                c,
                Align2::CENTER_CENTER,
                letter,
                FontId::proportional(17.0),
                Color32::WHITE,
            );
            paint_list_row_text(
                painter,
                rect,
                rect.left() + 4.0 + diam + inline_style::INLINE_THUMB_SKIP as f32,
                &res.title,
                &res.description,
                Some("Contact"),
                palette,
            );
        }
        InlineLayoutKind::Game => {
            paint_list_row_chrome(painter, rect, hovered, palette);
            let thumb = Rect::from_min_size(
                Pos2::new(
                    rect.left() + 4.0,
                    rect.top() + inline_style::INLINE_ROW_MARGIN as f32,
                ),
                Vec2::splat(inline_style::INLINE_THUMB_SIZE as f32),
            );
            paint_procedural_art(
                painter,
                thumb,
                seed + 5,
                CornerRadius::same(8),
                Some("PLAY"),
            );
            paint_list_row_text(
                painter,
                rect,
                thumb.right() + inline_style::INLINE_THUMB_SKIP as f32,
                &res.title,
                &res.description,
                Some("Game"),
                palette,
            );
        }
        InlineLayoutKind::Article { with_thumb } => {
            paint_list_row_chrome(painter, rect, hovered, palette);
            let mut text_left = rect.left() + 6.0;
            if with_thumb {
                let thumb = Rect::from_min_size(
                    Pos2::new(
                        rect.left() + 4.0,
                        rect.top() + inline_style::INLINE_ROW_MARGIN as f32,
                    ),
                    Vec2::splat(inline_style::INLINE_THUMB_SIZE as f32),
                );
                if matches!(
                    res.kind,
                    InlineResultKind::Location | InlineResultKind::Venue
                ) || res.location.is_some()
                {
                    paint_map_thumbnail(painter, thumb, CornerRadius::same(6));
                } else if res.has_thumbnail || res.photo_id.is_some() {
                    paint_procedural_art(painter, thumb, seed + 1, CornerRadius::same(6), None);
                } else {
                    // Fallback monogram letter tile (`ItemBase::getResultThumbLetter`)
                    painter.rect_filled(
                        thumb,
                        CornerRadius::same(6),
                        with_alpha(palette.window_bg_active, 0.16),
                    );
                    let letter = res.thumb_letter().unwrap_or('A').to_string();
                    painter.text(
                        thumb.center(),
                        Align2::CENTER_CENTER,
                        letter,
                        FontId::proportional(24.0),
                        palette.window_active_text_fg,
                    );
                }
                text_left = thumb.right() + inline_style::INLINE_THUMB_SKIP as f32;
            }
            let badge = match res.kind {
                InlineResultKind::Location => Some("Location"),
                InlineResultKind::Venue => Some("Venue"),
                InlineResultKind::Article => Some("Article"),
                _ => None,
            };
            paint_list_row_text(
                painter,
                rect,
                text_left,
                &res.title,
                &res.description,
                badge,
                palette,
            );
        }
    }
}

fn paint_list_row_chrome(painter: &Painter, rect: Rect, hovered: bool, palette: &Palette) {
    if hovered {
        painter.rect_filled(rect, CornerRadius::same(6), palette.window_bg_over);
    }
    painter.line_segment(
        [
            Pos2::new(rect.left() + 4.0, rect.bottom() - 0.5),
            Pos2::new(rect.right() - 4.0, rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, with_alpha(palette.divider, 0.7)),
    );
}

fn paint_list_row_text(
    painter: &Painter,
    row_rect: Rect,
    text_left: f32,
    title: &str,
    description: &str,
    badge: Option<&str>,
    palette: &Palette,
) {
    let title_y = row_rect.top() + 14.0;
    painter.text(
        Pos2::new(text_left, title_y),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(13.5),
        palette.window_bold_fg,
    );
    if !description.is_empty() {
        painter.text(
            Pos2::new(text_left, title_y + 20.0),
            Align2::LEFT_CENTER,
            description,
            FontId::proportional(12.0),
            palette.window_sub_text_fg,
        );
    }
    if let Some(tag) = badge {
        let tag_w = tag.len() as f32 * 6.5 + 12.0;
        let tag_rect = Rect::from_min_size(
            Pos2::new(row_rect.right() - tag_w - 8.0, row_rect.top() + 8.0),
            Vec2::new(tag_w, 18.0),
        );
        painter.rect_filled(
            tag_rect,
            CornerRadius::same(9),
            with_alpha(palette.window_bg_active, 0.13),
        );
        painter.text(
            tag_rect.center(),
            Align2::CENTER_CENTER,
            tag,
            FontId::proportional(10.0),
            palette.window_active_text_fg,
        );
    }
}
