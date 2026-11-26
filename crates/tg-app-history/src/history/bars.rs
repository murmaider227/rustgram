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
use tg_ui::stand_in::{
    Align, BoxShadowStyle, Point, PopupMenuStyle, ToggleStyle, UnreadBadgeStyle,
};
use tg_ui::unread_badge::{UnreadBadgeKind, paint_unread_badge};
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::buttons::{IconKind, paint_icon};
use tg_ui::widgets::media_marker::{paint_media_marker, split_media_marker};
use tg_ui::widgets::menu::{MenuItem, PopupMenuState};

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

/// `st::topBarSkip` (info.style): the chat menu toggle overlaps its left neighbour by 5px.
const TOP_BAR_SKIP: f32 = -5.0;

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

/// Events emitted from the contact status banner (`history_view_contact_status.cpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactStatusEvent {
    /// Add peer to contacts (`contacts.acceptContact` / `contacts.addContact`).
    AddContact,
    /// Share own phone number with peer (`contacts.acceptContact`).
    SharePhone,
    /// Block or unblock peer (`contacts.block` / `contacts.unblock`).
    ToggleBlock,
    /// User clicked `"Report Spam"` — prompts inline destructive confirmation.
    ReportSpam,
    /// User confirmed `"Report"` in the inline Report Spam confirmation strip (`messages.reportSpam`).
    ConfirmReportSpam,
    /// User cancelled the inline Report Spam confirmation strip.
    CancelReportSpam,
    /// Dismiss banner (`messages.hidePeerSettingsBar`).
    Dismiss,
}

/// Resolved bounding boxes for the contact status banner (used for painting and narrow-viewport tests).
#[derive(Debug, Clone, PartialEq)]
pub struct ContactStatusBarLayout {
    /// Action button rects in left-to-right order.
    pub action_rects: Vec<Rect>,
    /// Dismiss (`×`) button rect on the right edge.
    pub dismiss_rect: Rect,
    /// Whether the bar is in compact narrow-viewport mode (`< 430px`).
    pub compact: bool,
}

/// Computes non-overlapping bounding boxes for [`paint_contact_status_bar`] at any viewport width.
#[must_use]
pub fn compute_contact_status_bar_layout(
    rect: Rect,
    banner: &ContactStatusBanner,
) -> ContactStatusBarLayout {
    let compact = rect.width() < 430.0;
    let dismiss_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 20.0, rect.center().y),
        Vec2::splat(26.0),
    );
    let content_right = (dismiss_rect.left() - 6.0).max(rect.left() + 60.0);
    let content_w = (content_right - rect.left()).max(60.0);

    let action_rects = match banner {
        ContactStatusBanner::AddOrBlock { .. } => {
            // 3-way split: Add contact | Block user | Report Spam
            let w1 = (content_w * 0.34).floor();
            let w2 = (content_w * 0.31).floor();
            let r1 = Rect::from_min_size(rect.min, Vec2::new(w1, rect.height()));
            let r2 = Rect::from_min_size(
                Pos2::new(r1.right(), rect.top()),
                Vec2::new(w2, rect.height()),
            );
            let r3 = Rect::from_min_max(
                Pos2::new(r2.right(), rect.top()),
                Pos2::new(content_right, rect.bottom()),
            );
            vec![r1, r2, r3]
        }
        ContactStatusBanner::Blocked
        | ContactStatusBanner::SharePhoneNumber { .. }
        | ContactStatusBanner::ReportSpam { .. } => {
            vec![Rect::from_min_max(
                rect.min,
                Pos2::new(content_right, rect.bottom()),
            )]
        }
    };

    ContactStatusBarLayout {
        action_rects,
        dismiss_rect,
        compact,
    }
}

/// Resolved bounding boxes for the history top bar (used by painting and narrow-viewport tests).
///
/// Mirrors `TopBarWidget::updateControlsGeometry`: buttons are laid out from the right edge
/// (`_menuToggle`, `_infoToggle`, `_call` / `_groupCall`, `_search`) and the title starts after
/// the back button and userpic (one-column) or at `st::topBarArrowPadding.right()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TopBarLayout {
    /// Clickable peer title + status region (opens the profile).
    pub header_rect: Rect,
    /// Clipped bounding box reserved for peer title & status text.
    pub title_clip_rect: Rect,
    /// Back button (`st::historyTopBarBack`, 52px) when the history can go back.
    pub back_rect: Option<Rect>,
    /// One-column userpic button (`st::topBarInfoButton`, 52px with a 42px photo).
    pub avatar_rect: Option<Rect>,
    /// Search button (`st::topBarSearch`).
    pub search_rect: Rect,
    /// Call (users) or group call (groups / channels) button.
    pub call_rect: Option<Rect>,
    /// Third-column profile toggle (`st::topBarInfo`), hidden in one-column mode.
    pub info_rect: Option<Rect>,
    /// Chat menu toggle (`st::topBarMenuToggle`, three dots).
    pub menu_rect: Rect,
    /// Whether the top bar is in one-column narrow-viewport mode (`< 480px`).
    pub compact: bool,
}

/// Which call button `TopBarWidget` shows for a peer (`_call` / `_groupCall`).
fn top_bar_call_kind(kind: PeerKind, is_saved_messages: bool, can_manage: bool) -> bool {
    match kind {
        PeerKind::User => !is_saved_messages,
        PeerKind::Group | PeerKind::Channel => can_manage,
    }
}

/// Computes non-overlapping bounding boxes for [`paint_top_bar`] at any viewport width.
#[must_use]
pub fn compute_top_bar_layout(
    rect: Rect,
    kind: PeerKind,
    is_saved_messages: bool,
    can_go_back: bool,
) -> TopBarLayout {
    compute_top_bar_layout_ext(rect, kind, is_saved_messages, can_go_back, true)
}

/// [`compute_top_bar_layout`] with the group-call permission (`canManageGroupCall`) explicit.
#[must_use]
pub fn compute_top_bar_layout_ext(
    rect: Rect,
    kind: PeerKind,
    is_saved_messages: bool,
    can_go_back: bool,
    can_manage_group_call: bool,
) -> TopBarLayout {
    let compact = rect.width() < 480.0;
    let button = |right: f32, width: f32| {
        Rect::from_min_max(
            Pos2::new(right - width, rect.top()),
            Pos2::new(right, rect.bottom()),
        )
    };

    let mut left = rect.left();
    let back_rect = can_go_back.then(|| {
        let r = Rect::from_min_size(Pos2::new(left, rect.top()), Vec2::new(52.0, rect.height()));
        left = r.right();
        r
    });
    let avatar_rect = (can_go_back && compact).then(|| {
        let r = Rect::from_min_size(Pos2::new(left, rect.top()), Vec2::new(52.0, rect.height()));
        left = r.right();
        r
    });
    if back_rect.is_none() {
        left = rect.left() + 17.0;
    }

    let mut right_taken = 0.0;
    // `st::topBarMenuToggle` is 44px wide and the next button starts `topBarSkip` (-5px) inside
    // it; the hit area keeps only the 39px that is not covered, so neighbours never overlap.
    let menu_rect = button(rect.right(), 44.0 + TOP_BAR_SKIP);
    right_taken += 44.0 + TOP_BAR_SKIP;
    let info_rect = (!compact).then(|| {
        let r = button(rect.right() - right_taken, 40.0);
        right_taken += 40.0;
        r
    });
    let call_rect = top_bar_call_kind(kind, is_saved_messages, can_manage_group_call).then(|| {
        let r = button(rect.right() - right_taken, 40.0);
        right_taken += 40.0;
        r
    });
    let search_rect = button(rect.right() - right_taken, 40.0);
    right_taken += 40.0 - 1.0;

    let name_right = (rect.right() - right_taken - 3.0).max(left + 8.0);
    let header_rect = Rect::from_min_max(
        Pos2::new(left, rect.top()),
        Pos2::new(name_right, rect.bottom()),
    );
    TopBarLayout {
        header_rect,
        title_clip_rect: header_rect,
        back_rect,
        avatar_rect,
        search_rect,
        call_rect,
        info_rect,
        menu_rect,
        compact,
    }
}

/// Truncates `text` with `'…'` so it fits strictly within `max_width` pixels.
///
/// Returns `(display_text, was_truncated)`.
pub(crate) fn ellipsize_to_width(
    painter: &egui::Painter,
    text: &str,
    font: &FontId,
    max_width: f32,
) -> (String, bool) {
    if max_width <= 4.0 || text.is_empty() {
        return (String::new(), !text.is_empty());
    }
    let full = painter.layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE);
    if full.size().x <= max_width {
        return (text.to_owned(), false);
    }
    let ellipsis = "…";
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut candidate: String = chars[..mid].iter().collect();
        candidate.push_str(ellipsis);
        let w = painter
            .layout_no_wrap(candidate, font.clone(), Color32::WHITE)
            .size()
            .x;
        if w <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    let trimmed = out.trim_end().to_owned();
    out = trimmed;
    out.push_str(ellipsis);
    (out, true)
}

#[allow(clippy::too_many_arguments)]
/// Paints one top bar icon button (`IconButton` with `rippleAreaPosition(0, 7)`, size 40).
fn paint_top_bar_icon_button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    icon: tg_ui::tg_style::Icon,
    icon_rect: Rect,
    active: bool,
    label: &str,
    colors: &HistoryColors,
) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    let label_owned = label.to_owned();
    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, active, label_owned.clone()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        let ripple = Rect::from_min_size(
            Pos2::new(rect.left() + (rect.width() - 40.0) * 0.5, rect.top() + 7.0),
            Vec2::splat(40.0),
        );
        ui.painter()
            .circle_filled(ripple.center(), 20.0, colors.bar_bg_over);
    }
    let fg = if active {
        colors.active_text_fg
    } else {
        colors.sub_text_fg
    };
    tg_app_core::ui_icons::paint_style_tinted(ui.painter(), icon_rect, icon, fg);
    resp.on_hover_text(label)
}

/// Chat menu (`TopBarWidget::showPeerMenu`) items available from the top bar.
fn top_bar_menu_items(peer: &HistoryPeerInfo) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if !peer.summary.is_saved_messages {
        let label = if peer.summary.muted {
            tg_lang::tr::lng_context_unmute()
        } else {
            tg_lang::tr::lng_context_mute()
        };
        items.push(MenuItem::action("mute", label));
    }
    match peer.summary.kind {
        PeerKind::Channel => {
            items.push(MenuItem::action(
                "direct_messages",
                tg_lang::tr::lng_profile_direct_messages(),
            ));
        }
        PeerKind::User if !peer.summary.is_saved_messages => {
            items.push(MenuItem::action(
                "no_forwards",
                tg_lang::tr::lng_disable_sharing(),
            ));
        }
        _ => {}
    }
    items
}

/// Paints the normal history top bar (`history_view_top_bar_widget.cpp`, height `54px`):
/// title + status on the left (back button and userpic only in one-column mode), and
/// search, call / group call, profile toggle and chat menu buttons on the right.
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
    use tg_ui::tg_style::st;

    let mut event = None;
    let ui_colors = colors.ui_colors();
    let layout = compute_top_bar_layout_ext(
        rect,
        peer.summary.kind,
        peer.summary.is_saved_messages,
        can_go_back,
        peer.can_pin,
    );
    let base_id = Id::unique(("tg_hist_top", peer.summary.id.0));

    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    let centered_icon = |r: Rect, size: Vec2| Rect::from_center_size(r.center(), size);

    if let Some(back_rect) = layout.back_rect {
        let icon_rect = Rect::from_min_size(
            Pos2::new(back_rect.left() + 11.0, back_rect.center().y - 12.0),
            Vec2::splat(24.0),
        );
        if paint_top_bar_icon_button(
            ui,
            back_rect,
            base_id.with("back"),
            st::history_top_bar_back.icon,
            icon_rect,
            false,
            "Back",
            colors,
        )
        .clicked()
        {
            event = Some(TopBarEvent::Back);
        }
    }

    if let Some(avatar_rect) = layout.avatar_rect {
        let photo = Rect::from_center_size(
            Pos2::new(avatar_rect.left() + 2.0 + 21.0, avatar_rect.center().y),
            Vec2::splat(42.0),
        );
        if peer.summary.is_saved_messages {
            EmptyUserpic::paint_saved_messages(ui.painter(), photo, &ui_colors);
        } else if !tg_ui::userpic_images::paint_userpic_image(
            ui.painter(),
            photo,
            peer.summary.id.0,
        ) {
            EmptyUserpic::for_peer(peer.summary.id.bare(), &peer.summary.title, &ui_colors)
                .paint_circle(ui.painter(), photo);
        }
        let resp = ui.interact(avatar_rect, base_id.with("userpic"), Sense::click());
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Chat info"));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            event = Some(TopBarEvent::ToggleInfo);
        }
    }

    if paint_top_bar_icon_button(
        ui,
        layout.search_rect,
        base_id.with("search"),
        st::top_bar_search.icon,
        centered_icon(layout.search_rect, Vec2::splat(32.0)),
        search_open,
        "Search in chat",
        colors,
    )
    .clicked()
    {
        event = Some(TopBarEvent::ToggleSearch);
    }

    if let Some(call_rect) = layout.call_rect {
        let (icon, label) = if matches!(peer.summary.kind, PeerKind::User) {
            (
                st::top_bar_call.icon,
                tg_lang::tr::lng_profile_action_short_call(),
            )
        } else {
            (
                st::top_bar_group_call.icon,
                tg_lang::tr::lng_group_call_title(),
            )
        };
        // Calls start from the profile's Call action; the button opens it.
        if paint_top_bar_icon_button(
            ui,
            call_rect,
            base_id.with("call"),
            icon,
            centered_icon(call_rect, Vec2::splat(32.0)),
            false,
            &label,
            colors,
        )
        .clicked()
        {
            event = Some(TopBarEvent::ToggleInfo);
        }
    }

    if let Some(info_rect) = layout.info_rect
        && paint_top_bar_icon_button(
            ui,
            info_rect,
            base_id.with("info"),
            st::top_bar_info.icon,
            centered_icon(info_rect, Vec2::splat(32.0)),
            info_open,
            "Toggle chat info",
            colors,
        )
        .clicked()
    {
        event = Some(TopBarEvent::ToggleInfo);
    }

    let menu_icon_rect = Rect::from_min_size(
        Pos2::new(
            layout.menu_rect.left() + 16.0 + TOP_BAR_SKIP,
            layout.menu_rect.top() + 17.0,
        ),
        Vec2::new(8.0, 20.0),
    );
    let menu_items = top_bar_menu_items(peer);
    let menu_state_id = base_id.with("menu_state");
    let mut menu_state: PopupMenuState = ui
        .data_mut(|d| d.get_temp(menu_state_id))
        .unwrap_or_default();
    let menu_style = PopupMenuStyle::default_popup(&ui_colors);
    if paint_top_bar_icon_button(
        ui,
        layout.menu_rect,
        base_id.with("menu"),
        st::top_bar_menu_toggle.icon,
        menu_icon_rect,
        menu_state.is_open(),
        &tg_lang::tr::lng_chat_menu(),
        colors,
    )
    .clicked()
        && !menu_items.is_empty()
    {
        if menu_state.is_open() {
            menu_state.close();
        } else {
            menu_state.open_at(
                ui.ctx(),
                Pos2::new(
                    layout.menu_rect.right() - 6.0 - menu_style.menu.width_min,
                    rect.top() + 45.0,
                ),
                &menu_style,
            );
        }
    }
    let toggle_style = ToggleStyle::default_toggle(&ui_colors);
    if let Some(triggered) = menu_state.show(
        ui.ctx(),
        base_id.with("menu_popup"),
        &menu_items,
        &menu_style,
        &toggle_style,
    ) {
        menu_state.close();
        event = match triggered.as_str() {
            "mute" => Some(TopBarEvent::ToggleMute),
            "direct_messages" => Some(TopBarEvent::OpenDirectMessages),
            "no_forwards" => Some(TopBarEvent::ToggleNoForwards),
            _ => event,
        };
    }
    ui.data_mut(|d| d.insert_temp(menu_state_id, menu_state));

    // Title + status (`TopBarWidget::paintTopBar`).
    let text_clip = layout.title_clip_rect;
    let text_x = text_clip.left();
    let max_text_w = (text_clip.width() - 2.0).max(0.0);
    let clipped_painter = ui.painter().with_clip_rect(text_clip);
    let title_truncated;
    if peer.summary.is_saved_messages {
        let font = FontId::new(14.0, tg_ui::fonts::family_semibold());
        let (title, truncated) = ellipsize_to_width(
            ui.painter(),
            &tg_lang::tr::lng_saved_messages(),
            &font,
            max_text_w,
        );
        title_truncated = truncated;
        clipped_painter.text(
            Pos2::new(text_x, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            font,
            colors.bold_fg,
        );
    } else {
        let title_font = FontId::new(13.0, tg_ui::fonts::family_semibold());
        let (title_text, truncated) =
            ellipsize_to_width(ui.painter(), &peer.summary.title, &title_font, max_text_w);
        title_truncated = truncated;
        clipped_painter.text(
            Pos2::new(text_x, rect.top() + 8.0),
            Align2::LEFT_TOP,
            title_text,
            title_font,
            colors.bold_fg,
        );
        let (status_str, status_fg) = if let Some(ref typing) = peer.typing_text {
            (typing.as_str(), colors.active_text_fg)
        } else if peer.summary.status == "online" {
            (peer.summary.status.as_str(), colors.active_text_fg)
        } else {
            (peer.summary.status.as_str(), colors.sub_text_fg)
        };
        let status_font = FontId::proportional(13.0);
        let (status_text, _) =
            ellipsize_to_width(ui.painter(), status_str, &status_font, max_text_w);
        clipped_painter.text(
            Pos2::new(text_x, rect.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            status_text,
            status_font,
            status_fg,
        );
    }

    let hdr_rect = layout.header_rect;
    let hdr_resp = ui.interact(hdr_rect, base_id.with("peer"), Sense::click());
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
    let hdr_tooltip = if title_truncated {
        format!("{} — Click to toggle chat info", peer.summary.title)
    } else {
        "Click to toggle chat info".to_owned()
    };
    if hdr_resp.on_hover_text(hdr_tooltip).clicked() {
        event = Some(TopBarEvent::ToggleInfo);
    }

    // Bottom 1px shadow line (`st::shadowFg`).
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
    let preview_clip = ui.painter().with_clip_rect(Rect::from_min_size(
        Pos2::new(text_x, rect.top() + 24.0),
        Vec2::new(clip_w, 19.0),
    ));
    // `MediaPoll::toPreview` / `MediaTodoList::toPreview`: the pinned text starts with the
    // `dialogsMiniPollIcon` / `dialogsMiniTodoListIcon` icon emoji, not the "📊 " marker.
    let (marker, preview_text) = match split_media_marker(&current.preview) {
        Some((icon, rest)) => (Some(icon), rest),
        None => (None, current.preview.as_str()),
    };
    let mut preview_x = text_x;
    if let Some(icon) = marker {
        preview_x += paint_media_marker(
            &preview_clip,
            Pos2::new(text_x, rect.top() + 24.0),
            icon,
            colors.sub_text_fg,
            &tg_ui::tg_style::DefaultPaletteResolver,
        );
    }
    let preview_galley = ui.painter().layout_no_wrap(
        preview_text.to_owned(),
        FontId::proportional(12.5),
        colors.sub_text_fg,
    );
    preview_clip.galley(
        Pos2::new(preview_x, rect.top() + 24.0),
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
    paint_contact_status_bar_with_confirm(ui, rect, banner, false, colors)
}

/// Paints the contact status banner (`history_view_contact_status.cpp`, height `42px`),
/// with optional inline destructive confirmation for `"Report Spam"`.
pub fn paint_contact_status_bar_with_confirm(
    ui: &mut Ui,
    rect: Rect,
    banner: &ContactStatusBanner,
    confirming_report_spam: bool,
    colors: &HistoryColors,
) -> Option<ContactStatusEvent> {
    let mut event = None;
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, colors.bar_bg);

    let layout = compute_contact_status_bar_layout(rect, banner);

    if confirming_report_spam {
        // Inline destructive confirmation strip (explicit verb + Cancel, Escape dismisses).
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            event = Some(ContactStatusEvent::CancelReportSpam);
        }
        let tint = Color32::from_rgba_unmultiplied(
            colors.attention_fg.r(),
            colors.attention_fg.g(),
            colors.attention_fg.b(),
            18,
        );
        ui.painter().rect_filled(rect, CornerRadius::ZERO, tint);

        let btn_h = 28.0;
        let cy = rect.center().y;
        let cancel_label = tg_lang::tr::lng_cancel();
        let report_label = tg_lang::tr::lng_report_spam_ok();
        let cancel_w = 64.0;
        let report_w = 76.0;
        let cancel_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 10.0 - cancel_w, cy - btn_h * 0.5),
            Vec2::new(cancel_w, btn_h),
        );
        let report_rect = Rect::from_min_size(
            Pos2::new(cancel_rect.left() - 6.0 - report_w, cy - btn_h * 0.5),
            Vec2::new(report_w, btn_h),
        );
        let prompt_clip = Rect::from_min_max(
            Pos2::new(rect.left() + 12.0, rect.top() + 2.0),
            Pos2::new(
                (report_rect.left() - 8.0).max(rect.left() + 40.0),
                rect.bottom() - 2.0,
            ),
        );
        let prompt_text = match banner {
            ContactStatusBanner::ReportSpam {
                is_channel: true, ..
            } => "Report spam in this channel?",
            ContactStatusBanner::ReportSpam {
                is_channel: false, ..
            } => "Report this chat for spam?",
            _ => "Report this user for spam?",
        };
        let prompt_font = FontId::proportional(12.5);
        let (fitted_prompt, _) =
            ellipsize_to_width(ui.painter(), prompt_text, &prompt_font, prompt_clip.width());
        ui.painter().with_clip_rect(prompt_clip).text(
            Pos2::new(prompt_clip.left(), cy),
            Align2::LEFT_CENTER,
            fitted_prompt,
            prompt_font,
            colors.bold_fg,
        );

        let report_bg = Color32::from_rgba_unmultiplied(
            colors.attention_fg.r(),
            colors.attention_fg.g(),
            colors.attention_fg.b(),
            28,
        );
        if paint_bar_action_pill(
            ui,
            "tg_cs_confirm_report_spam",
            report_rect,
            &report_label,
            report_bg,
            colors.attention_fg,
        ) {
            event = Some(ContactStatusEvent::ConfirmReportSpam);
        }
        if paint_bar_action_pill(
            ui,
            "tg_cs_cancel_report_spam",
            cancel_rect,
            &cancel_label,
            Color32::TRANSPARENT,
            colors.sub_text_fg,
        ) {
            event = Some(ContactStatusEvent::CancelReportSpam);
        }

        ui.painter().line_segment(
            [
                Pos2::new(rect.left(), rect.bottom()),
                Pos2::new(rect.right(), rect.bottom()),
            ],
            Stroke::new(1.0, colors.divider),
        );
        return event;
    }

    let paint_cell = |ui: &mut Ui,
                      cell_rect: Rect,
                      id_salt: &str,
                      display_label: &str,
                      accessible_label: &str,
                      fg: Color32|
     -> bool {
        let resp = ui.interact(cell_rect, Id::unique(id_salt), Sense::click());
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, accessible_label));
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(cell_rect, CornerRadius::ZERO, colors.bar_bg_over);
        }
        if resp.has_focus() {
            ui.painter().rect_stroke(
                cell_rect.shrink(1.5),
                CornerRadius::same(4),
                Stroke::new(1.5, colors.accent),
                egui::StrokeKind::Inside,
            );
        }
        let font = FontId::proportional(if layout.compact { 12.0 } else { 12.5 });
        let text_pad = 6.0;
        let max_text_w = (cell_rect.width() - text_pad * 2.0).max(16.0);
        let (fitted, truncated) =
            ellipsize_to_width(ui.painter(), display_label, &font, max_text_w);
        ui.painter()
            .with_clip_rect(cell_rect.shrink2(Vec2::new(text_pad, 2.0)))
            .text(cell_rect.center(), Align2::CENTER_CENTER, fitted, font, fg);
        let resp = if truncated || display_label != accessible_label {
            resp.on_hover_text(accessible_label)
        } else {
            resp
        };
        resp.clicked()
    };

    match banner {
        ContactStatusBanner::AddOrBlock { .. } => {
            let add_a11y = tg_lang::tr::lng_new_contact_add();
            let block_a11y = tg_lang::tr::lng_new_contact_block();
            let spam_a11y = tg_lang::tr::lng_report_spam();
            let add_display = if layout.compact {
                "Add".to_owned()
            } else {
                add_a11y.clone()
            };
            let block_display = if layout.compact {
                "Block".to_owned()
            } else {
                block_a11y.clone()
            };
            let spam_display = if layout.compact {
                "Report".to_owned()
            } else {
                spam_a11y.clone()
            };

            if paint_cell(
                ui,
                layout.action_rects[0],
                "tg_cs_add",
                &add_display,
                &add_a11y,
                colors.active_text_fg,
            ) {
                event = Some(ContactStatusEvent::AddContact);
            }

            // Vertical divider between Add and Block
            ui.painter().line_segment(
                [
                    Pos2::new(layout.action_rects[0].right(), rect.top() + 8.0),
                    Pos2::new(layout.action_rects[0].right(), rect.bottom() - 8.0),
                ],
                Stroke::new(1.0, colors.divider),
            );

            if paint_cell(
                ui,
                layout.action_rects[1],
                "tg_cs_block",
                &block_display,
                &block_a11y,
                colors.attention_fg,
            ) {
                event = Some(ContactStatusEvent::ToggleBlock);
            }

            // Vertical divider between Block and Report Spam
            ui.painter().line_segment(
                [
                    Pos2::new(layout.action_rects[1].right(), rect.top() + 8.0),
                    Pos2::new(layout.action_rects[1].right(), rect.bottom() - 8.0),
                ],
                Stroke::new(1.0, colors.divider),
            );

            if paint_cell(
                ui,
                layout.action_rects[2],
                "tg_cs_report_spam",
                &spam_display,
                &spam_a11y,
                colors.attention_fg,
            ) {
                event = Some(ContactStatusEvent::ReportSpam);
            }
        }
        ContactStatusBanner::Blocked => {
            let unblock_label = tg_lang::tr::lng_blocked_list_unblock();
            if paint_cell(
                ui,
                layout.action_rects[0],
                "tg_cs_unblock",
                &unblock_label,
                &unblock_label,
                colors.active_text_fg,
            ) {
                event = Some(ContactStatusEvent::ToggleBlock);
            }
        }
        ContactStatusBanner::SharePhoneNumber { .. } => {
            let share_label = tg_lang::tr::lng_new_contact_share();
            if paint_cell(
                ui,
                layout.action_rects[0],
                "tg_cs_share",
                &share_label,
                &share_label,
                colors.active_text_fg,
            ) {
                event = Some(ContactStatusEvent::SharePhone);
            }
        }
        ContactStatusBanner::ReportSpam { .. } => {
            let spam_label = tg_lang::tr::lng_report_spam();
            if paint_cell(
                ui,
                layout.action_rects[0],
                "tg_cs_report_spam_only",
                &spam_label,
                &spam_label,
                colors.attention_fg,
            ) {
                event = Some(ContactStatusEvent::ReportSpam);
            }
        }
    }

    // Dismiss cross (`messages.hidePeerSettingsBar`)
    let dismiss_rect = layout.dismiss_rect;
    let dismiss_resp = ui.interact(dismiss_rect, Id::unique("tg_cs_dismiss"), Sense::click());
    dismiss_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Dismiss contact banner"));
    if dismiss_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .circle_filled(dismiss_rect.center(), 13.0, colors.bar_bg_over);
    }
    if dismiss_resp.has_focus() {
        ui.painter()
            .circle_stroke(dismiss_rect.center(), 13.0, Stroke::new(1.5, colors.accent));
    }
    paint_icon(
        ui.painter(),
        dismiss_rect.shrink(6.0),
        IconKind::Close,
        colors.sub_text_fg,
    );
    if dismiss_resp.on_hover_text("Dismiss").clicked() {
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
    for (idx, (peer_id, name, color_idx)) in summary.recent_requesters.iter().take(3).enumerate() {
        let cx = rect.left() + left_pad + avatar_size * 0.5 + idx as f32 * step;
        let av_rect =
            Rect::from_center_size(Pos2::new(cx, rect.center().y), Vec2::splat(avatar_size));
        // Subtle ring separating overlapping circles
        ui.painter()
            .circle_filled(av_rect.center(), avatar_size * 0.5 + 1.5, bg);
        EmptyUserpic::for_color_index(*color_idx, name, &ui_colors).paint_circle_or_photo(
            ui.painter(),
            av_rect,
            peer_id.0,
        );
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
    EmptyUserpic::for_peer(state.bot_id.bare(), &state.bot_name, &ui_colors).paint_circle_or_photo(
        ui.painter(),
        av_rect,
        state.bot_id.0,
    );

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

/// Height of the bottom START bar (`st::historyComposeButton.height`).
pub const BOT_START_BAR_HEIGHT: f32 = 46.0;
/// Text top offset inside the START bar (`st::historyComposeButton.textTop`).
const BOT_START_TEXT_TOP: f32 = 14.0;

/// Paints the bottom START bar shown in place of the composer for a bot chat with a pending
/// `start` token or an empty history (`HistoryWidget::_botStart`, `isBotStart()`): a full-width
/// `historyComposeButton` flat button with the upper-cased `lng_bot_start` label.
///
/// Returns `true` when the bar was clicked.
pub fn paint_bot_start_bar(ui: &mut Ui, rect: Rect, colors: &HistoryColors) -> bool {
    let label = tg_lang::tr::lng_bot_start().to_uppercase();
    let resp = ui.interact(
        rect,
        ui.scope_id().with("tg_history_bot_start_bar"),
        Sense::click(),
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label.clone()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let bg = if resp.hovered() {
        colors.bar_bg_over
    } else {
        colors.bar_bg
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, bg);
    painter.line_segment(
        [
            Pos2::new(rect.left(), rect.top()),
            Pos2::new(rect.right(), rect.top()),
        ],
        Stroke::new(1.0, colors.divider),
    );
    painter.text(
        Pos2::new(rect.center().x, rect.top() + BOT_START_TEXT_TOP),
        Align2::CENTER_TOP,
        label,
        FontId::new(
            tg_ui::fonts::font_id_from_style(tg_ui::tg_style::st::semibold_font).size,
            tg_ui::fonts::family_semibold(),
        ),
        colors.active_text_fg,
    );
    resp.clicked()
}
