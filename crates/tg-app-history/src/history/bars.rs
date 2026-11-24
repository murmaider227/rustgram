//! Top bar, multi-select action bar, pinned message bar, contact status banner,
//! and floating scroll-to-bottom button.
//!
//! Port origin: `Telegram/SourceFiles/history/view/history_view_top_bar_widget.cpp`,
//! `Telegram/SourceFiles/history/view/history_view_pinned_bar.cpp`,
//! `Telegram/SourceFiles/history/view/history_view_contact_status.cpp`,
//! and `Telegram/SourceFiles/history/history_widget.cpp` (`_scrollDown`).

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Role, Sense, Shape, Stroke,
    Ui, Vec2, WidgetInfo,
};
use tg_ui::painter::BoxShadow;
use tg_ui::stand_in::{Align, BoxShadowStyle, Point, UnreadBadgeStyle};
use tg_ui::unread_badge::{UnreadBadgeKind, paint_unread_badge};
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::buttons::{IconKind, paint_icon};

use crate::history::backend::{
    ContactStatusBanner, HistoryPeerInfo, MediaUploadStatus, PinnedMessageSummary,
};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::types::{MsgId, PeerKind};

/// Pinned message bar height matching `st::historyPinnedBarHeight` (`49px`).
pub const PINNED_BAR_HEIGHT: f32 = 49.0;
/// Contact status bar height (`st::historyContactStatusMinHeight = 42px`).
pub const CONTACT_STATUS_HEIGHT: f32 = 42.0;
/// Join requests bar height matching `st::historyRequestsHeight` (`44px`).
pub const JOIN_REQUESTS_BAR_HEIGHT: f32 = 44.0;
/// Connected Telegram Business chatbot bar height (`history_view_contact_status.cpp`, `44px`).
pub const BUSINESS_BOT_BAR_HEIGHT: f32 = 44.0;
/// Height of one active outgoing upload progress row docked above the compose bar.
pub const UPLOAD_PROGRESS_ROW_HEIGHT: f32 = 44.0;

/// Events emitted from the top bar or multi-select bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopBarEvent {
    /// Back button clicked.
    Back,
    /// Peer header or Info button clicked.
    ToggleInfo,
    /// Search icon clicked.
    ToggleSearch,
    /// Mute / unmute toggled.
    ToggleMute,
    /// Channel Direct Messages (monoforum) button clicked.
    OpenDirectMessages,
    /// Toggle 1:1 protected-chat sharing (`messages.toggleNoForwards`).
    ToggleNoForwards,
    /// Forward selected messages.
    ForwardSelected,
    /// Delete selected messages.
    DeleteSelected,
    /// Copy selected messages.
    CopySelected,
    /// Cancel multi-selection mode.
    CancelSelection,
}

/// Events emitted from the pinned message bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinnedBarEvent {
    /// Jump to the currently displayed pinned message and cycle index.
    JumpAndCycle(MsgId),
    /// Unpin the currently displayed pinned message.
    Unpin(MsgId),
}

/// Events emitted from the contact status banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactStatusEvent {
    /// Add peer to contacts.
    AddContact,
    /// Block or unblock peer.
    ToggleBlock,
    /// Dismiss banner.
    Dismiss,
}

/// Paints the normal history top bar (`history_view_top_bar_widget.cpp`, height `54px`).
#[allow(clippy::too_many_arguments)]
pub fn paint_top_bar(
    ui: &mut Ui,
    rect: Rect,
    peer: &HistoryPeerInfo,
    can_go_back: bool,
    info_open: bool,
    search_open: bool,
    colors: &HistoryColors,
) -> Option<TopBarEvent> {
    let mut event = None;
    let ui_colors = colors.ui_colors();

    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    let mut left_x = rect.left() + 10.0;

    // Optional Back button
    if can_go_back {
        let back_rect =
            Rect::from_center_size(Pos2::new(left_x + 16.0, rect.center().y), Vec2::splat(32.0));
        let resp = ui.interact(back_rect, Id::unique("tg_hist_top_back"), Sense::click());
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Back"));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(back_rect.center(), 16.0, colors.bar_bg_over);
        }
        paint_icon(
            ui.painter(),
            back_rect.shrink(7.0),
            IconKind::Back,
            colors.sub_text_fg,
        );
        if resp.on_hover_text("Back").clicked() {
            event = Some(TopBarEvent::Back);
        }
        left_x = back_rect.right() + 6.0;
    }

    // Right-side action icons (Info, Mute, Search)
    let mut right_x = rect.right() - 10.0;

    // Info toggle button
    let info_rect = Rect::from_center_size(
        Pos2::new(right_x - 16.0, rect.center().y),
        Vec2::splat(32.0),
    );
    let info_resp = ui.interact(info_rect, Id::unique("tg_hist_top_info"), Sense::click());
    info_resp
        .widget_info(|| WidgetInfo::selected(Role::Button, true, info_open, "Toggle chat info"));
    if info_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(info_rect.center(), 16.0, colors.bar_bg_over);
    }
    paint_icon(
        ui.painter(),
        info_rect.shrink(7.0),
        IconKind::Info,
        if info_open {
            colors.accent
        } else {
            colors.sub_text_fg
        },
    );
    if info_resp.on_hover_text("Toggle chat info").clicked() {
        event = Some(TopBarEvent::ToggleInfo);
    }
    right_x = info_rect.left() - 6.0;

    // Search toggle button
    let search_rect = Rect::from_center_size(
        Pos2::new(right_x - 16.0, rect.center().y),
        Vec2::splat(32.0),
    );
    let search_resp = ui.interact(
        search_rect,
        Id::unique("tg_hist_top_search"),
        Sense::click(),
    );
    search_resp
        .widget_info(|| WidgetInfo::selected(Role::Button, true, search_open, "Search in chat"));
    if search_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(search_rect.center(), 16.0, colors.bar_bg_over);
    }
    paint_icon(
        ui.painter(),
        search_rect.shrink(7.0),
        IconKind::Search,
        if search_open {
            colors.accent
        } else {
            colors.sub_text_fg
        },
    );
    if search_resp.on_hover_text("Search in chat").clicked() {
        event = Some(TopBarEvent::ToggleSearch);
    }
    right_x = search_rect.left() - 6.0;

    // Mute toggle pill
    let mute_rect = Rect::from_center_size(
        Pos2::new(right_x - 24.0, rect.center().y),
        Vec2::new(48.0, 26.0),
    );
    let mute_resp = ui.interact(mute_rect, Id::unique("tg_hist_top_mute"), Sense::click());
    let mute_label = if peer.summary.muted { "Muted" } else { "Mute" };
    mute_resp
        .widget_info(|| WidgetInfo::selected(Role::Button, true, peer.summary.muted, mute_label));
    if mute_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(mute_rect, CornerRadius::same(6), colors.bar_bg_over);
    }
    ui.painter().text(
        mute_rect.center(),
        Align2::CENTER_CENTER,
        mute_label,
        FontId::proportional(11.5),
        if peer.summary.muted {
            colors.accent
        } else {
            colors.sub_text_fg
        },
    );
    if mute_resp.on_hover_text("Toggle notifications").clicked() {
        event = Some(TopBarEvent::ToggleMute);
    }
    right_x = mute_rect.left() - 6.0;

    // 1:1 protected-chat sharing toggle (`messages.toggleNoForwards`)
    if matches!(
        peer.summary.kind,
        tg_app_core::stand_in::types::PeerKind::User
    ) && !peer.summary.is_saved_messages
    {
        let nf_rect = Rect::from_center_size(
            Pos2::new(right_x - 16.0, rect.center().y),
            Vec2::splat(30.0),
        );
        let nf_resp = ui.interact(
            nf_rect,
            Id::unique("tg_hist_top_noforwards"),
            Sense::click(),
        );
        nf_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Protect chat sharing"));
        if nf_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(nf_rect.center(), 15.0, colors.bar_bg_over);
        }
        tg_app_core::ui_icons::paint_lock(ui.painter(), nf_rect.center(), 16.0, colors.sub_text_fg);
        if nf_resp
            .on_hover_text("Toggle protected sharing in this chat")
            .clicked()
        {
            event = Some(TopBarEvent::ToggleNoForwards);
        }
        right_x = nf_rect.left() - 6.0;
    }

    // Channel Direct Messages button (`history_view_top_bar_widget.cpp` monoforum entry).
    if matches!(peer.summary.kind, PeerKind::Channel) {
        let dm_w = 112.0;
        let dm_rect = Rect::from_center_size(
            Pos2::new(right_x - dm_w * 0.5, rect.center().y),
            Vec2::new(dm_w, 26.0),
        );
        let dm_resp = ui.interact(
            dm_rect,
            Id::unique("tg_hist_top_direct_messages"),
            Sense::click(),
        );
        dm_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Direct Messages"));
        if dm_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(dm_rect, CornerRadius::same(6), colors.bar_bg_over);
        }
        ui.painter().text(
            dm_rect.center(),
            Align2::CENTER_CENTER,
            "Direct Messages",
            FontId::proportional(11.5),
            colors.accent,
        );
        if dm_resp
            .on_hover_text("Open channel Direct Messages")
            .clicked()
        {
            event = Some(TopBarEvent::OpenDirectMessages);
        }
        right_x = dm_rect.left() - 8.0;
    }

    // Clickable peer avatar + title + status/typing region
    let hdr_rect = Rect::from_min_max(
        Pos2::new(left_x, rect.top() + 4.0),
        Pos2::new(right_x.max(left_x + 80.0), rect.bottom() - 4.0),
    );
    let avatar_rect = Rect::from_center_size(
        Pos2::new(hdr_rect.left() + 19.0, hdr_rect.center().y),
        Vec2::splat(36.0),
    );
    if peer.summary.is_saved_messages {
        EmptyUserpic::paint_saved_messages(ui.painter(), avatar_rect, &ui_colors);
    } else {
        EmptyUserpic::for_peer(peer.summary.id.bare(), &peer.summary.title, &ui_colors)
            .paint_circle(ui.painter(), avatar_rect);
    }

    let text_x = avatar_rect.right() + 10.0;
    ui.painter().text(
        Pos2::new(text_x, hdr_rect.top() + 5.0),
        Align2::LEFT_TOP,
        &peer.summary.title,
        FontId::proportional(14.5),
        colors.bold_fg,
    );

    let (status_str, status_fg) = if let Some(ref typing) = peer.typing_text {
        (typing.as_str(), colors.active_text_fg)
    } else if peer.summary.status == "online" {
        (peer.summary.status.as_str(), colors.active_text_fg)
    } else {
        (peer.summary.status.as_str(), colors.sub_text_fg)
    };
    ui.painter().text(
        Pos2::new(text_x, hdr_rect.top() + 24.0),
        Align2::LEFT_TOP,
        status_str,
        FontId::proportional(12.0),
        status_fg,
    );

    let hdr_resp = ui.interact(hdr_rect, Id::unique("tg_hist_top_peer"), Sense::click());
    hdr_resp.widget_info(|| {
        WidgetInfo::labeled(
            Role::Button,
            true,
            format!("Chat info: {}", peer.summary.title),
        )
    });
    if hdr_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if hdr_resp
        .on_hover_text("Click to toggle chat info")
        .clicked()
    {
        event = Some(TopBarEvent::ToggleInfo);
    }

    // Bottom 1px divider
    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    event
}

/// Paints the multi-select top bar (`TopBarWidget::showSelected`, height `54px`).
pub fn paint_selection_top_bar(
    ui: &mut Ui,
    rect: Rect,
    selected_count: usize,
    colors: &HistoryColors,
) -> Option<TopBarEvent> {
    let mut event = None;
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    let btn_h = 34.0;
    let cy = rect.center().y;
    let mut x = rect.left() + 14.0;

    let fwd_label = format!("Forward  {selected_count}");
    let fwd_rect = Rect::from_min_size(Pos2::new(x, cy - btn_h * 0.5), Vec2::new(112.0, btn_h));
    if paint_bar_action_pill(
        ui,
        "tg_sel_fwd",
        fwd_rect,
        &fwd_label,
        colors.accent,
        Color32::WHITE,
    ) {
        event = Some(TopBarEvent::ForwardSelected);
    }
    x = fwd_rect.right() + 8.0;

    let copy_rect = Rect::from_min_size(Pos2::new(x, cy - btn_h * 0.5), Vec2::new(72.0, btn_h));
    if paint_bar_action_pill(
        ui,
        "tg_sel_copy",
        copy_rect,
        "Copy",
        colors.bar_bg_over,
        colors.active_text_fg,
    ) {
        event = Some(TopBarEvent::CopySelected);
    }
    x = copy_rect.right() + 8.0;

    let del_label = format!("Delete  {selected_count}");
    let del_rect = Rect::from_min_size(Pos2::new(x, cy - btn_h * 0.5), Vec2::new(104.0, btn_h));
    if paint_bar_action_pill(
        ui,
        "tg_sel_del",
        del_rect,
        &del_label,
        Color32::from_rgba_unmultiplied(
            colors.attention_fg.r(),
            colors.attention_fg.g(),
            colors.attention_fg.b(),
            28,
        ),
        colors.attention_fg,
    ) {
        event = Some(TopBarEvent::DeleteSelected);
    }

    let cancel_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 84.0, cy - btn_h * 0.5),
        Vec2::new(72.0, btn_h),
    );
    if paint_bar_action_pill(
        ui,
        "tg_sel_cancel",
        cancel_rect,
        "Cancel",
        Color32::TRANSPARENT,
        colors.active_text_fg,
    ) {
        event = Some(TopBarEvent::CancelSelection);
    }

    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    event
}

fn paint_bar_action_pill(
    ui: &mut Ui,
    id_salt: &str,
    rect: Rect,
    label: &str,
    bg: Color32,
    fg: Color32,
) -> bool {
    let resp = ui.interact(rect, Id::unique(id_salt), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if resp.hovered() && bg == Color32::TRANSPARENT {
        Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 22)
    } else {
        bg
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        fg,
    );
    resp.clicked()
}

/// Paints the pinned message bar (`history_view_pinned_bar.cpp`, height `49px`).
pub fn paint_pinned_bar(
    ui: &mut Ui,
    rect: Rect,
    pinned: &[PinnedMessageSummary],
    active_index: usize,
    can_unpin: bool,
    colors: &HistoryColors,
) -> Option<PinnedBarEvent> {
    if pinned.is_empty() {
        return None;
    }
    let idx = active_index.min(pinned.len() - 1);
    let current = &pinned[idx];
    let mut event = None;

    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    // Vertical segmented progress indicator on the left (one segment per pinned message, max 4 visible).
    let seg_count = pinned.len().min(4);
    let bar_x = rect.left() + 14.0;
    let bar_top = rect.top() + 8.0;
    let bar_total_h = rect.height() - 16.0;
    let gap = 2.0;
    let seg_h =
        ((bar_total_h - gap * (seg_count.saturating_sub(1) as f32)) / seg_count as f32).max(4.0);
    let active_seg = idx % seg_count;
    for s in 0..seg_count {
        let sy = bar_top + s as f32 * (seg_h + gap);
        let seg_rect = Rect::from_min_size(Pos2::new(bar_x, sy), Vec2::new(2.5, seg_h));
        let col = if s == active_seg {
            colors.accent
        } else {
            Color32::from_rgba_unmultiplied(
                colors.accent.r(),
                colors.accent.g(),
                colors.accent.b(),
                64,
            )
        };
        ui.painter()
            .rect_filled(seg_rect, CornerRadius::same(1), col);
    }

    let close_w = if can_unpin { 36.0 } else { 10.0 };
    let click_rect = Rect::from_min_max(
        Pos2::new(rect.left(), rect.top()),
        Pos2::new(rect.right() - close_w, rect.bottom()),
    );
    let click_resp = ui.interact(
        click_rect,
        Id::unique("tg_pinned_bar_click"),
        Sense::click(),
    );
    if click_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(click_rect, CornerRadius::ZERO, colors.bar_bg_over);
    }

    let text_x = bar_x + 12.0;
    let title = if pinned.len() > 1 {
        format!("Pinned Message #{}", pinned.len() - idx)
    } else {
        "Pinned Message".to_owned()
    };
    click_resp.widget_info(|| {
        WidgetInfo::labeled(Role::Button, true, format!("{title}: {}", current.preview))
    });
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 7.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(12.5),
        colors.active_text_fg,
    );
    let clip_w = (click_rect.right() - text_x - 8.0).max(40.0);
    let preview_galley = ui.painter().layout_no_wrap(
        current.preview.clone(),
        FontId::proportional(12.5),
        colors.sub_text_fg,
    );
    ui.painter()
        .with_clip_rect(Rect::from_min_size(
            Pos2::new(text_x, rect.top() + 24.0),
            Vec2::new(clip_w, 19.0),
        ))
        .galley(
            Pos2::new(text_x, rect.top() + 24.0),
            preview_galley,
            colors.sub_text_fg,
        );

    if click_resp
        .on_hover_text("Click to jump to pinned message")
        .clicked()
    {
        event = Some(PinnedBarEvent::JumpAndCycle(current.msg_id));
    }

    if can_unpin {
        let close_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 20.0, rect.center().y),
            Vec2::splat(26.0),
        );
        let close_resp = ui.interact(
            close_rect,
            Id::unique("tg_pinned_bar_unpin"),
            Sense::click(),
        );
        close_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Unpin message"));
        if close_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(close_rect.center(), 13.0, colors.bar_bg_over);
        }
        paint_icon(
            ui.painter(),
            close_rect.shrink(6.0),
            IconKind::Close,
            colors.sub_text_fg,
        );
        if close_resp.on_hover_text("Unpin message").clicked() {
            event = Some(PinnedBarEvent::Unpin(current.msg_id));
        }
    }

    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    event
}

/// Paints the contact status banner (`history_view_contact_status.cpp`, height `42px`).
pub fn paint_contact_status_bar(
    ui: &mut Ui,
    rect: Rect,
    banner: &ContactStatusBanner,
    colors: &HistoryColors,
) -> Option<ContactStatusEvent> {
    let mut event = None;
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    let dismiss_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 20.0, rect.center().y),
        Vec2::splat(26.0),
    );
    let content_right = dismiss_rect.left() - 6.0;

    match banner {
        ContactStatusBanner::AddOrBlock { .. } => {
            let half_w = ((content_right - rect.left()) * 0.5).max(60.0);
            let left_btn = Rect::from_min_size(rect.min, Vec2::new(half_w, rect.height()));
            let right_btn = Rect::from_min_max(
                Pos2::new(left_btn.right(), rect.top()),
                Pos2::new(content_right, rect.bottom()),
            );

            let add_resp = ui.interact(left_btn, Id::unique("tg_cs_add"), Sense::click());
            add_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "ADD TO CONTACTS"));
            if add_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(left_btn, CornerRadius::ZERO, colors.bar_bg_over);
            }
            ui.painter().text(
                left_btn.center(),
                Align2::CENTER_CENTER,
                "ADD TO CONTACTS",
                FontId::proportional(12.5),
                colors.active_text_fg,
            );
            if add_resp.clicked() {
                event = Some(ContactStatusEvent::AddContact);
            }

            // Center vertical divider
            ui.painter().line_segment(
                [
                    Pos2::new(left_btn.right(), rect.top() + 8.0),
                    Pos2::new(left_btn.right(), rect.bottom() - 8.0),
                ],
                Stroke::new(1.0, colors.divider),
            );

            let blk_resp = ui.interact(right_btn, Id::unique("tg_cs_block"), Sense::click());
            blk_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "BLOCK USER"));
            if blk_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(right_btn, CornerRadius::ZERO, colors.bar_bg_over);
            }
            ui.painter().text(
                right_btn.center(),
                Align2::CENTER_CENTER,
                "BLOCK USER",
                FontId::proportional(12.5),
                colors.attention_fg,
            );
            if blk_resp.clicked() {
                event = Some(ContactStatusEvent::ToggleBlock);
            }
        }
        ContactStatusBanner::Blocked => {
            let btn = Rect::from_min_max(rect.min, Pos2::new(content_right, rect.bottom()));
            let resp = ui.interact(btn, Id::unique("tg_cs_unblock"), Sense::click());
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "UNBLOCK USER"));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(btn, CornerRadius::ZERO, colors.bar_bg_over);
            }
            ui.painter().text(
                btn.center(),
                Align2::CENTER_CENTER,
                "UNBLOCK USER",
                FontId::proportional(12.5),
                colors.active_text_fg,
            );
            if resp.clicked() {
                event = Some(ContactStatusEvent::ToggleBlock);
            }
        }
        ContactStatusBanner::SharePhoneNumber { .. } => {
            let btn = Rect::from_min_max(rect.min, Pos2::new(content_right, rect.bottom()));
            let resp = ui.interact(btn, Id::unique("tg_cs_share"), Sense::click());
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "SHARE MY PHONE NUMBER"));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(btn, CornerRadius::ZERO, colors.bar_bg_over);
            }
            ui.painter().text(
                btn.center(),
                Align2::CENTER_CENTER,
                "SHARE MY PHONE NUMBER",
                FontId::proportional(12.5),
                colors.active_text_fg,
            );
            if resp.clicked() {
                event = Some(ContactStatusEvent::AddContact);
            }
        }
    }

    // Dismiss cross
    let dismiss_resp = ui.interact(dismiss_rect, Id::unique("tg_cs_dismiss"), Sense::click());
    dismiss_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Dismiss contact banner"));
    if dismiss_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(dismiss_rect.center(), 13.0, colors.bar_bg_over);
    }
    paint_icon(
        ui.painter(),
        dismiss_rect.shrink(6.0),
        IconKind::Close,
        colors.sub_text_fg,
    );
    if dismiss_resp.clicked() {
        event = Some(ContactStatusEvent::Dismiss);
    }

    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    event
}

/// Paints the floating scroll-to-bottom button with optional unread counter pill
/// (`HistoryWidget::_scrollDown`, `history_widget.cpp`).
pub fn paint_scroll_to_bottom_button(
    ui: &mut Ui,
    list_rect: Rect,
    unread_count: u32,
    colors: &HistoryColors,
) -> bool {
    let diameter = 42.0;
    let center = Pos2::new(
        list_rect.right() - 16.0 - diameter * 0.5,
        list_rect.bottom() - 14.0 - diameter * 0.5,
    );
    let btn_rect = Rect::from_center_size(center, Vec2::splat(diameter));

    let resp = ui.interact(btn_rect, Id::unique("tg_hist_scroll_down"), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Scroll to bottom"));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let shadow = BoxShadow::new(BoxShadowStyle {
        blur_radius: 6.0,
        offset: Point::new(0.0, 2.0),
        opacity: 0.22,
    });
    shadow.paint(ui.ctx(), ui.painter(), btn_rect, diameter * 0.5);

    let bg = if resp.hovered() {
        colors.to_down_bg_over
    } else {
        colors.to_down_bg
    };
    ui.painter().circle_filled(center, diameter * 0.5, bg);
    ui.painter()
        .circle_stroke(center, diameter * 0.5, Stroke::new(0.5, colors.divider));

    // Downward chevron icon
    let chev = Stroke::new(2.0, colors.to_down_fg);
    ui.painter().add(Shape::line(
        vec![
            Pos2::new(center.x - 6.5, center.y - 2.5),
            Pos2::new(center.x, center.y + 4.0),
            Pos2::new(center.x + 6.5, center.y - 2.5),
        ],
        chev,
    ));

    // Unread counter badge floating above the button
    if unread_count > 0 {
        let ui_colors = colors.ui_colors();
        let badge_style = UnreadBadgeStyle {
            align: Align::Center,
            ..UnreadBadgeStyle::default()
        };
        paint_unread_badge(
            ui.painter(),
            &unread_count.to_string(),
            Pos2::new(center.x, btn_rect.top() - 9.0),
            &badge_style,
            UnreadBadgeKind::Dialogs,
            4,
            &ui_colors,
        );
    }

    resp.on_hover_text("Scroll to bottom").clicked()
}

/// Paints the in-chat join-requests bar (`ui/chat/requests_bar.cpp`, height `44px`),
/// showing up to 3 overlapping requester avatars and the pending request count/name.
///
/// Returns `true` when the user clicks the bar to open `JoinRequestsBox`.
pub fn paint_join_requests_bar(
    ui: &mut Ui,
    rect: Rect,
    summary: &tg_app_info::info::backend::JoinRequestsSummary,
    colors: &HistoryColors,
) -> bool {
    if summary.count == 0 {
        return false;
    }
    let ui_colors = colors.ui_colors();
    let resp = ui.interact(
        rect,
        Id::unique("tg_hist_join_requests_bar"),
        Sense::click(),
    );

    let bg = if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        colors.bar_bg_over
    } else {
        colors.bar_bg
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, bg);

    // Up to 3 overlapping miniature avatars on the left (`GroupCallUserpics` style)
    let avatar_size = 26.0;
    let step = 16.0;
    let left_pad = 14.0;
    let shown = summary.recent_requesters.len().min(3);
    for (idx, (_, name, color_idx)) in summary.recent_requesters.iter().take(3).enumerate() {
        let cx = rect.left() + left_pad + avatar_size * 0.5 + idx as f32 * step;
        let av_rect =
            Rect::from_center_size(Pos2::new(cx, rect.center().y), Vec2::splat(avatar_size));
        // Subtle ring separating overlapping circles
        ui.painter()
            .circle_filled(av_rect.center(), avatar_size * 0.5 + 1.5, bg);
        EmptyUserpic::for_color_index(*color_idx, name, &ui_colors)
            .paint_circle(ui.painter(), av_rect);
    }

    let avatars_w = if shown > 0 {
        avatar_size + (shown.saturating_sub(1) as f32) * step
    } else {
        0.0
    };
    let text_x = rect.left() + left_pad + avatars_w + if shown > 0 { 12.0 } else { 0.0 };
    let label = if summary.count == 1
        && let Some((_, name, _)) = summary.recent_requesters.first()
        && !name.is_empty()
    {
        format!("1 request to join from {name}")
    } else {
        format!("{} requests to join", summary.count)
    };
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));

    ui.painter().text(
        Pos2::new(text_x, rect.center().y),
        Align2::LEFT_CENTER,
        &label,
        FontId::proportional(13.0),
        colors.active_text_fg,
    );

    // Right-side "Review" hint chevron
    ui.painter().text(
        Pos2::new(rect.right() - 16.0, rect.center().y),
        Align2::RIGHT_CENTER,
        "Review ›",
        FontId::proportional(12.0),
        colors.sub_text_fg,
    );

    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    resp.on_hover_text("Review pending join requests").clicked()
}

/// Events emitted from the connected Telegram Business chatbot bar
/// (`BusinessBotStatus` in `history_view_contact_status.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusinessBotBarEvent {
    /// Pause the connected Business bot in this chat (`account.toggleConnectedBotPaused(true)`).
    Pause,
    /// Resume the connected Business bot in this chat (`account.toggleConnectedBotPaused(false)`).
    Resume,
    /// Disconnect the Business bot from this chat (`account.disablePeerConnectedBot`).
    Remove,
    /// Open the Business bot management URL (`manage_url`).
    Manage(String),
}

/// Paints the connected Telegram Business chatbot pause/resume bar (`history_view_contact_status.cpp`,
/// height [`BUSINESS_BOT_BAR_HEIGHT`] = `44px`).
pub fn paint_business_bot_bar(
    ui: &mut Ui,
    rect: Rect,
    state: &crate::history::backend::BusinessBotBarState,
    colors: &HistoryColors,
) -> Option<BusinessBotBarEvent> {
    let mut event = None;
    let ui_colors = colors.ui_colors();

    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    // Bot avatar circle on the left
    let av_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 26.0, rect.center().y),
        Vec2::splat(28.0),
    );
    EmptyUserpic::for_peer(state.bot_id.bare(), &state.bot_name, &ui_colors)
        .paint_circle(ui.painter(), av_rect);

    // Right-side action controls: Remove (×), Manage (optional), Pause/Resume pill
    let mut right_x = rect.right() - 10.0;

    let remove_rect = Rect::from_center_size(
        Pos2::new(right_x - 13.0, rect.center().y),
        Vec2::splat(26.0),
    );
    let remove_resp = ui.interact(
        remove_rect,
        Id::unique("tg_hist_bizbot_remove"),
        Sense::click(),
    );
    remove_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Remove business bot"));
    if remove_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(remove_rect.center(), 13.0, colors.bar_bg_over);
    }
    paint_icon(
        ui.painter(),
        remove_rect.shrink(6.0),
        IconKind::Close,
        colors.sub_text_fg,
    );
    if remove_resp
        .on_hover_text("Disconnect business bot from this chat")
        .clicked()
    {
        event = Some(BusinessBotBarEvent::Remove);
    }
    right_x = remove_rect.left() - 6.0;

    if let Some(ref url) = state.manage_url
        && !url.is_empty()
    {
        let manage_rect = Rect::from_center_size(
            Pos2::new(right_x - 32.0, rect.center().y),
            Vec2::new(62.0, 26.0),
        );
        if paint_bar_action_pill(
            ui,
            "tg_hist_bizbot_manage",
            manage_rect,
            "Manage",
            colors.bar_bg_over,
            colors.active_text_fg,
        ) {
            event = Some(BusinessBotBarEvent::Manage(url.clone()));
        }
        right_x = manage_rect.left() - 6.0;
    }

    if state.can_reply {
        let (lbl, bg, fg, ev) = if state.paused {
            (
                "Resume",
                colors.accent,
                Color32::WHITE,
                BusinessBotBarEvent::Resume,
            )
        } else {
            (
                "Pause",
                colors.bar_bg_over,
                colors.active_text_fg,
                BusinessBotBarEvent::Pause,
            )
        };
        let toggle_rect = Rect::from_center_size(
            Pos2::new(right_x - 34.0, rect.center().y),
            Vec2::new(66.0, 26.0),
        );
        if paint_bar_action_pill(ui, "tg_hist_bizbot_toggle", toggle_rect, lbl, bg, fg) {
            event = Some(ev);
        }
        right_x = toggle_rect.left() - 8.0;
    }

    // Bot title & status subtitle clipped cleanly before the right buttons
    let text_x = av_rect.right() + 10.0;
    let text_clip = Rect::from_min_max(
        Pos2::new(text_x, rect.top()),
        Pos2::new(right_x.max(text_x + 40.0), rect.bottom()),
    );
    let p = ui.painter().with_clip_rect(text_clip);
    p.text(
        Pos2::new(text_x, rect.top() + 6.0),
        Align2::LEFT_TOP,
        &state.bot_name,
        FontId::proportional(13.0),
        colors.bold_fg,
    );
    let status_text = if !state.can_reply {
        "Can view messages"
    } else if state.paused {
        "Replies paused"
    } else {
        "Manages chat"
    };
    p.text(
        Pos2::new(text_x, rect.top() + 23.0),
        Align2::LEFT_TOP,
        status_text,
        FontId::proportional(11.5),
        if state.paused {
            colors.attention_fg
        } else {
            colors.sub_text_fg
        },
    );

    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    event
}
/// Paints active outgoing media upload progress rows docked above the compose bar.
///
/// Returns `Some(upload_id)` when the user clicks a row's cancel button.
pub fn paint_upload_progress_bar(
    ui: &mut Ui,
    rect: Rect,
    uploads: &[MediaUploadStatus],
    colors: &HistoryColors,
) -> Option<u64> {
    let mut cancelled_id = None;
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);
    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.top()),
            Pos2::new(rect.right(), rect.top()),
        ],
        Stroke::new(1.0, colors.divider),
    );

    for (idx, upload) in uploads.iter().enumerate() {
        let row_top = rect.top() + idx as f32 * UPLOAD_PROGRESS_ROW_HEIGHT;
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.left(), row_top),
            Vec2::new(rect.width(), UPLOAD_PROGRESS_ROW_HEIGHT),
        );
        let cancel_rect = Rect::from_center_size(
            Pos2::new(row_rect.right() - 22.0, row_rect.center().y),
            Vec2::splat(26.0),
        );
        let cancel_label = format!("Cancel upload {}", upload.file_name);
        let cancel_resp = ui.interact(
            cancel_rect,
            Id::unique(("tg_upload_cancel", upload.upload_id)),
            Sense::click(),
        );
        cancel_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, cancel_label.clone()));
        if cancel_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .circle_filled(cancel_rect.center(), 13.0, colors.bar_bg_over);
        }
        paint_icon(
            ui.painter(),
            cancel_rect.shrink(6.0),
            IconKind::Close,
            colors.sub_text_fg,
        );
        if cancel_resp.on_hover_text("Cancel upload").clicked() {
            cancelled_id = Some(upload.upload_id);
        }

        let left = row_rect.left() + 14.0;
        let right = (cancel_rect.left() - 10.0).max(left + 80.0);
        let progress = upload.progress().clamp(0.0, 1.0) as f32;
        let pct = (progress * 100.0).round() as u32;
        let status_text = if upload.failed {
            "Upload failed".to_owned()
        } else if upload.total_bytes > 0 {
            format!(
                "{pct}%  ({} / {})",
                format_upload_bytes(upload.uploaded_bytes),
                format_upload_bytes(upload.total_bytes),
            )
        } else {
            format!("{pct}%")
        };

        let title_clip = Rect::from_min_max(
            Pos2::new(left, row_rect.top() + 5.0),
            Pos2::new((right - 120.0).max(left + 40.0), row_rect.top() + 23.0),
        );
        let title_galley = ui.painter().layout_no_wrap(
            format!("Uploading {}", upload.file_name),
            FontId::proportional(12.5),
            colors.bold_fg,
        );
        ui.painter().with_clip_rect(title_clip).galley(
            title_clip.min,
            title_galley,
            colors.bold_fg,
        );

        ui.painter().text(
            Pos2::new(right, row_rect.top() + 14.0),
            Align2::RIGHT_CENTER,
            &status_text,
            FontId::proportional(11.5),
            if upload.failed {
                Color32::from_rgb(0xE0, 0x52, 0x52)
            } else {
                colors.sub_text_fg
            },
        );

        let bar_rect = Rect::from_min_max(
            Pos2::new(left, row_rect.top() + 28.0),
            Pos2::new(right, row_rect.top() + 34.0),
        );
        ui.painter()
            .rect_filled(bar_rect, CornerRadius::same(3), colors.bar_bg_over);
        let fill_w =
            (bar_rect.width() * progress).max(if upload.uploaded_bytes > 0 { 4.0 } else { 0.0 });
        if fill_w > 0.0 {
            let fill_rect = Rect::from_min_size(bar_rect.min, Vec2::new(fill_w, bar_rect.height()));
            let fill_color = if upload.failed {
                Color32::from_rgb(0xE0, 0x52, 0x52)
            } else {
                colors.accent
            };
            ui.painter()
                .rect_filled(fill_rect, CornerRadius::same(3), fill_color);
        }

        if idx + 1 < uploads.len() {
            ui.painter().line_segment(
                [
                    Pos2::new(row_rect.left() + 14.0, row_rect.bottom()),
                    Pos2::new(row_rect.right() - 14.0, row_rect.bottom()),
                ],
                Stroke::new(1.0, colors.divider),
            );
        }
    }

    cancelled_id
}

fn format_upload_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}
