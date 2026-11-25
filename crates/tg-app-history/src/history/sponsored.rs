//! Sponsored messages in channels & bots: request/view throttling, Premium "hide ads" respect,
//! channel banner/bubble painting, `AboutSponsoredBox`, and multi-step `ReportSponsoredBox`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/data/components/sponsored_messages.cpp`
//! - `Telegram/SourceFiles/boxes/about_sponsored_box.cpp`
//! - `Telegram/SourceFiles/ui/chat/sponsored_message_bar.cpp`

use egui::{Align2, CornerRadius, CursorIcon, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use crate::history::theme::HistoryColors;

pub use tg_app_core::core_types::dto::sponsored::*;

/// User action emitted by [`paint_sponsored_banner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SponsoredBannerAction {
    /// User clicked the CTA button or ad body (`messages.clickSponsoredMessage`).
    Click {
        /// Sponsored message `random_id`.
        random_id: Vec<u8>,
        /// Target URL to navigate/open.
        url: String,
        /// Whether the click originated on media.
        is_media: bool,
        /// Whether the click originated in fullscreen media view.
        is_fullscreen: bool,
    },
    /// User clicked "What's this?" to open [`AboutSponsoredBoxState`].
    OpenAbout,
    /// User clicked "Report" on a reportable ad.
    OpenReport(Vec<u8>),
    /// User clicked "Hide Ads" (Premium toggle or Premium prompt).
    HideAds,
    /// User dismissed this ad locally.
    Dismiss(Vec<u8>),
}

/// Resolved header bounding boxes for [`paint_sponsored_banner`] (used for narrow-viewport checks).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SponsoredHeaderLayout {
    /// Clipped title region.
    pub title_rect: Rect,
    /// `"Sponsored"` / `"Recommended"` badge rect.
    pub badge_rect: Rect,
    /// `"What's this?"` / compact `"?"` info button rect.
    pub about_rect: Rect,
    /// Optional `"Report"` button rect.
    pub report_rect: Option<Rect>,
    /// `"Hide Ads"` / `"Remove Ads"` (or compact `"Hide"`) button rect.
    pub hide_rect: Rect,
}

/// Computes non-overlapping header rects for [`paint_sponsored_banner`].
#[must_use]
pub fn compute_sponsored_header_layout(
    rect: Rect,
    recommended: bool,
    can_report: bool,
) -> SponsoredHeaderLayout {
    let card_rect = rect.shrink2(Vec2::new(10.0, 4.0));
    let compact = card_rect.width() < 460.0;
    let left = card_rect.left() + 12.0;
    let top = card_rect.top() + 8.0;

    let mut rx = card_rect.right() - 10.0;
    let hide_w = if compact { 38.0 } else { 70.0 };
    let hide_rect = Rect::from_min_size(Pos2::new(rx - hide_w, top - 1.0), Vec2::new(hide_w, 19.0));
    rx -= hide_w + 6.0;

    let report_rect = if can_report {
        let rep_w = 42.0;
        let r = Rect::from_min_size(Pos2::new(rx - rep_w, top - 1.0), Vec2::new(rep_w, 19.0));
        rx -= rep_w + 6.0;
        Some(r)
    } else {
        None
    };

    let about_w = if compact { 20.0 } else { 68.0 };
    let about_rect =
        Rect::from_min_size(Pos2::new(rx - about_w, top - 1.0), Vec2::new(about_w, 19.0));
    let header_right = (about_rect.left() - 8.0).max(left + 72.0);

    let badge_w = if recommended { 84.0 } else { 64.0 };
    let max_title_w = (header_right - left - badge_w - 6.0).max(24.0);
    let title_rect = Rect::from_min_size(Pos2::new(left, top), Vec2::new(max_title_w, 18.0));
    let badge_rect = Rect::from_min_size(
        Pos2::new(title_rect.right() + 6.0, top),
        Vec2::new(
            badge_w.min((header_right - title_rect.right() - 6.0).max(0.0)),
            17.0,
        ),
    );

    SponsoredHeaderLayout {
        title_rect,
        badge_rect,
        about_rect,
        report_rect,
        hide_rect,
    }
}

/// Paints a channel/bot sponsored message banner inside `rect`.
pub fn paint_sponsored_banner(
    ui: &mut Ui,
    rect: Rect,
    item: &SponsoredMessageItem,
    is_premium: bool,
    colors: &HistoryColors,
) -> Option<SponsoredBannerAction> {
    let mut action = None;
    let card_rect = rect.shrink2(Vec2::new(10.0, 4.0));
    let compact = card_rect.width() < 460.0;
    ui.painter()
        .rect_filled(card_rect, CornerRadius::same(10), colors.msg_in_bg);
    ui.painter().rect_stroke(
        card_rect,
        CornerRadius::same(10),
        Stroke::new(1.0, colors.divider),
        egui::StrokeKind::Inside,
    );

    // Left accent stripe
    let stripe = Rect::from_min_size(card_rect.min, Vec2::new(3.5, card_rect.height()));
    ui.painter()
        .rect_filled(stripe, CornerRadius::same(2), colors.accent);

    let left = card_rect.left() + 12.0;
    let top = card_rect.top() + 8.0;
    let hdr = compute_sponsored_header_layout(rect, item.recommended, item.can_report);

    // Header: ellipsized Title + "Sponsored" / "Recommended" pill cleanly bounded left of actions
    let title_font = FontId::proportional(13.0);
    let (title_display, title_truncated) = crate::history::bars::ellipsize_to_width(
        ui.painter(),
        &item.title,
        &title_font,
        hdr.title_rect.width(),
    );
    let title_galley = ui
        .painter()
        .layout_no_wrap(title_display, title_font, colors.bold_fg);
    let actual_title_w = title_galley.size().x.min(hdr.title_rect.width());
    ui.painter().with_clip_rect(hdr.title_rect).galley(
        hdr.title_rect.min,
        title_galley,
        colors.bold_fg,
    );
    if title_truncated {
        let _ = ui
            .interact(
                hdr.title_rect,
                Id::unique(("tg_sponsored_title", &item.random_id)),
                Sense::hover(),
            )
            .on_hover_text(&item.title);
    }

    let badge_w = hdr.badge_rect.width();
    let badge_x = (left + actual_title_w + 6.0).min(hdr.about_rect.left() - 8.0 - badge_w);
    let badge_rect =
        Rect::from_min_size(Pos2::new(badge_x.max(left), top), Vec2::new(badge_w, 17.0));
    ui.painter()
        .rect_filled(badge_rect, CornerRadius::same(5), colors.reaction_in_bg);
    ui.painter().with_clip_rect(badge_rect).text(
        badge_rect.center(),
        Align2::CENTER_CENTER,
        item.badge_label(),
        FontId::proportional(10.5),
        colors.active_text_fg,
    );

    // Top-right small controls: "What's this?" (compact "?" at narrow), "Report", "Hide/Remove Ads"
    let full_hide_label = if is_premium { "Hide Ads" } else { "Remove Ads" };
    let visible_hide_label = if compact { "Hide" } else { full_hide_label };
    let hide_rect = hdr.hide_rect;
    let hide_resp = ui.interact(
        hide_rect,
        Id::unique(("tg_sponsored_hide", &item.random_id)),
        Sense::click(),
    );
    hide_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, full_hide_label));
    if hide_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter()
            .rect_filled(hide_rect, CornerRadius::same(4), colors.bar_bg_over);
    }
    ui.painter().text(
        hide_rect.center(),
        Align2::CENTER_CENTER,
        visible_hide_label,
        FontId::proportional(11.0),
        colors.sub_text_fg,
    );
    if hide_resp.on_hover_text(full_hide_label).clicked() {
        action = Some(SponsoredBannerAction::HideAds);
    }

    if let Some(rep_rect) = hdr.report_rect {
        let rep_resp = ui.interact(
            rep_rect,
            Id::unique(("tg_sponsored_report", &item.random_id)),
            Sense::click(),
        );
        rep_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Report Ad"));
        if rep_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(rep_rect, CornerRadius::same(4), colors.bar_bg_over);
        }
        ui.painter().text(
            rep_rect.center(),
            Align2::CENTER_CENTER,
            "Report",
            FontId::proportional(11.0),
            colors.sub_text_fg,
        );
        if rep_resp.on_hover_text("Report Ad").clicked() {
            action = Some(SponsoredBannerAction::OpenReport(item.random_id.clone()));
        }
    }

    let about_rect = hdr.about_rect;
    let about_resp = ui.interact(
        about_rect,
        Id::unique(("tg_sponsored_about", &item.random_id)),
        Sense::click(),
    );
    about_resp
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "About Sponsored"));
    if about_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        if compact {
            ui.painter()
                .circle_filled(about_rect.center(), 9.5, colors.bar_bg_over);
        } else {
            ui.painter()
                .rect_filled(about_rect, CornerRadius::same(4), colors.bar_bg_over);
        }
    } else if compact {
        ui.painter().circle_stroke(
            about_rect.center(),
            8.5,
            Stroke::new(1.0, colors.active_text_fg),
        );
    }
    ui.painter().text(
        about_rect.center(),
        Align2::CENTER_CENTER,
        if compact { "?" } else { "What's this?" },
        FontId::proportional(11.0),
        colors.active_text_fg,
    );
    if about_resp.on_hover_text("What's this?").clicked() {
        action = Some(SponsoredBannerAction::OpenAbout);
    }

    // Body snippet + CTA button
    let cta_w = if compact { 92.0 } else { 112.0 };
    let body_w = (card_rect.width() - cta_w - 30.0).max(80.0);
    let body_rect = Rect::from_min_size(Pos2::new(left, top + 22.0), Vec2::new(body_w, 34.0));
    let body_galley = ui.painter().layout(
        item.text.text.clone(),
        FontId::proportional(12.5),
        colors.text_fg,
        body_w,
    );
    ui.painter()
        .with_clip_rect(body_rect)
        .galley(body_rect.min, body_galley, colors.text_fg);

    // Optional sponsor attribution line (clipped cleanly to body_w so it never touches CTA)
    if let Some(ref info) = item.sponsor_info {
        let info_font = FontId::proportional(10.5);
        let (info_text, _) =
            crate::history::bars::ellipsize_to_width(ui.painter(), info, &info_font, body_w);
        let info_clip = Rect::from_min_size(
            Pos2::new(left, card_rect.bottom() - 16.0),
            Vec2::new(body_w, 15.0),
        );
        ui.painter().with_clip_rect(info_clip).text(
            info_clip.min,
            Align2::LEFT_TOP,
            info_text,
            info_font,
            colors.sub_text_fg,
        );
    }

    // CTA button
    let cta_rect = Rect::from_min_size(
        Pos2::new(card_rect.right() - cta_w - 10.0, top + 24.0),
        Vec2::new(cta_w, 28.0),
    );
    let cta_resp = ui.interact(
        cta_rect,
        Id::unique(("tg_sponsored_cta", &item.random_id)),
        Sense::click(),
    );
    let cta_label = if item.button_text.is_empty() {
        "View"
    } else {
        item.button_text.as_str()
    };
    cta_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, cta_label));
    if cta_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter()
        .rect_filled(cta_rect, CornerRadius::same(7), colors.accent);
    ui.painter().text(
        cta_rect.center(),
        Align2::CENTER_CENTER,
        cta_label,
        FontId::proportional(12.0),
        egui::Color32::WHITE,
    );
    if cta_resp.clicked() {
        action = Some(SponsoredBannerAction::Click {
            random_id: item.random_id.clone(),
            url: item.url.clone(),
            is_media: false,
            is_fullscreen: false,
        });
    }

    action
}

// ---------------------------------------------------------------------------
// AboutSponsoredBox (`boxes/about_sponsored_box.cpp`)
// ---------------------------------------------------------------------------

/// Action emitted by [`AboutSponsoredBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AboutSponsoredBoxAction {
    /// User clicked the `https://ads.telegram.org` button.
    OpenAdsPlatform(String),
    /// User closed the box.
    Close,
}

/// Modal dialog explaining Telegram Sponsored Messages (`Ui::AboutSponsoredBox`).
#[derive(Debug, Clone, Default)]
pub struct AboutSponsoredBoxState {
    /// Whether the current account is Telegram Premium.
    pub is_premium: bool,
}

impl AboutSponsoredBoxState {
    /// Creates an `AboutSponsoredBox` modal state.
    #[must_use]
    pub const fn new(is_premium: bool) -> Self {
        Self { is_premium }
    }

    /// Renders `AboutSponsoredBox`.
    pub fn show(&mut self, ui: &mut Ui, colors: &HistoryColors) -> Option<AboutSponsoredBoxAction> {
        let mut action = None;
        egui::Window::new("What's a Sponsored Message?")
            .id(Id::unique("tg_about_sponsored_box"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(380.0, 260.0))
            .show(ui.ctx(), |ui| {
                ui.label(
                    egui::RichText::new(
                        "Sponsored Messages are shown exclusively in large public channels and \
                         selected bots. Unlike other platforms, ads are never based on personal \
                         user data — they depend solely on the topic of the public channel.",
                    )
                    .color(colors.text_fg),
                );

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space(60.0);
                    if ui
                        .button(egui::RichText::new(ADS_PLATFORM_URL).color(colors.active_text_fg))
                        .clicked()
                    {
                        action = Some(AboutSponsoredBoxAction::OpenAdsPlatform(
                            ADS_PLATFORM_URL.to_owned(),
                        ));
                    }
                });
                ui.add_space(10.0);

                let note2 = if self.is_premium {
                    "As a Telegram Premium subscriber, you can disable Sponsored Messages at any \
                     time from Privacy & Security or Business settings."
                } else {
                    "Sponsored Messages help support Telegram's development while keeping your \
                     private chats completely ad-free. Subscribe to Telegram Premium to hide ads."
                };
                ui.label(egui::RichText::new(note2).color(colors.sub_text_fg));

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        action = Some(AboutSponsoredBoxAction::Close);
                    }
                });
            });
        action
    }
}

// ---------------------------------------------------------------------------
// ReportSponsoredBox (multi-step `messages.reportSponsoredMessage`)
// ---------------------------------------------------------------------------

/// Action emitted by [`ReportSponsoredBoxState::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportSponsoredBoxAction {
    /// Submit a chosen report option byte slice to `messages.reportSponsoredMessage`.
    ChooseOption {
        /// Sponsored message random id.
        random_id: Vec<u8>,
        /// Selected option payload.
        option: Vec<u8>,
    },
    /// User closed the report dialog.
    Close,
}

/// Interactive multi-step sponsored message reporting modal.
#[derive(Debug, Clone)]
pub struct ReportSponsoredBoxState {
    /// Sponsored message `random_id`.
    pub random_id: Vec<u8>,
    /// Current step heading returned by the server.
    pub title: String,
    /// Current selectable report options.
    pub options: Vec<SponsoredReportOption>,
    /// Optional terminal result message (`Reported`, `AdsHidden`, `Silence`, `Error`).
    pub status_message: Option<String>,
    /// True if server returned `PREMIUM_ACCOUNT_REQUIRED`.
    pub premium_required: bool,
}

impl ReportSponsoredBoxState {
    /// Creates a report modal seeded with an initial server outcome.
    #[must_use]
    pub fn from_outcome(random_id: Vec<u8>, outcome: SponsoredReportOutcome) -> Self {
        let mut state = Self {
            random_id,
            title: "Report Ad".to_owned(),
            options: Vec::new(),
            status_message: None,
            premium_required: false,
        };
        state.apply_outcome(outcome);
        state
    }

    /// Updates the modal from a follow-up [`SponsoredReportOutcome`].
    pub fn apply_outcome(&mut self, outcome: SponsoredReportOutcome) {
        match outcome {
            SponsoredReportOutcome::ChooseOption { title, options } => {
                self.title = if title.is_empty() {
                    "Report Ad".to_owned()
                } else {
                    title
                };
                self.options = options;
                self.status_message = None;
                self.premium_required = false;
            }
            SponsoredReportOutcome::AdsHidden => {
                self.options.clear();
                self.status_message = Some("Sponsored messages are now hidden.".to_owned());
            }
            SponsoredReportOutcome::Reported => {
                self.options.clear();
                self.status_message = Some("Thank you. This ad has been reported.".to_owned());
            }
            SponsoredReportOutcome::Silence => {
                self.options.clear();
                self.status_message = Some("This ad has been hidden.".to_owned());
            }
            SponsoredReportOutcome::PremiumRequired => {
                self.options.clear();
                self.premium_required = true;
                self.status_message =
                    Some("Telegram Premium is required to hide all ads.".to_owned());
            }
            SponsoredReportOutcome::Error(err) => {
                self.status_message = Some(err);
            }
        }
    }

    /// Renders the report dialog.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        colors: &HistoryColors,
    ) -> Option<ReportSponsoredBoxAction> {
        let mut action = None;
        egui::Window::new(self.title.clone())
            .id(Id::unique("tg_report_sponsored_box"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(340.0, 260.0))
            .show(ui.ctx(), |ui| {
                if let Some(ref msg) = self.status_message {
                    ui.label(egui::RichText::new(msg).color(colors.text_fg));
                    ui.add_space(10.0);
                    if ui.button("Close").clicked() {
                        action = Some(ReportSponsoredBoxAction::Close);
                    }
                    return;
                }
                for opt in &self.options {
                    if ui.button(&opt.text).clicked() {
                        action = Some(ReportSponsoredBoxAction::ChooseOption {
                            random_id: self.random_id.clone(),
                            option: opt.option.clone(),
                        });
                    }
                }
                ui.add_space(8.0);
                if ui.button("Cancel").clicked() {
                    action = Some(ReportSponsoredBoxAction::Close);
                }
            });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tg_app_core::stand_in::types::PeerId;
    use tg_text::TextWithEntities;

    fn sample_ad(id: &[u8]) -> SponsoredMessageItem {
        SponsoredMessageItem {
            random_id: id.to_vec(),
            url: "https://t.me/demo_sponsor".to_owned(),
            title: "Demo Sponsor".to_owned(),
            text: TextWithEntities::simple("Check out our open-source developer tools."),
            button_text: "Open".to_owned(),
            sponsor_info: Some("Sponsored by Demo Tools".to_owned()),
            additional_info: None,
            recommended: false,
            can_report: true,
            color_index: 1,
            min_display_duration_ms: 0,
            max_display_duration_ms: 0,
            is_link_internal: true,
            viewed: false,
        }
    }

    #[test]
    fn premium_hide_ads_suppresses_requests_and_clears_cached_ads() {
        let mut state = SponsoredMessagesState::new(false, false);
        let chan = PeerId::from_channel(3001);
        assert!(state.should_request(chan, 1_000));
        state.store_list(
            chan,
            SponsoredPeerList {
                entries: vec![sample_ad(b"ad1")],
                mode: SponsoredPlacementMode::AppendToEnd,
                ..SponsoredPeerList::default()
            },
            1_000,
        );
        assert!(state.active_for_peer(chan).is_some());
        // Within 5-minute window: throttled
        assert!(!state.should_request(chan, 1_000 + 60_000));
        // After 5 minutes: eligible again
        assert!(state.should_request(chan, 1_000 + SPONSORED_REQUEST_TIME_LIMIT_MS));

        // Enabling Premium hide-ads wipes cached ads and blocks new requests
        state.set_premium_hide_ads(true, true);
        assert!(state.ads_suppressed());
        assert!(state.active_for_peer(chan).is_none());
        assert!(!state.should_request(chan, 1_000 + SPONSORED_REQUEST_TIME_LIMIT_MS * 2));
    }

    #[test]
    fn view_throttling_deduplicates_within_five_minutes() {
        let mut state = SponsoredMessagesState::new(false, false);
        assert!(state.record_view(b"ad-xyz", 10_000));
        assert!(!state.record_view(b"ad-xyz", 10_000 + 10_000));
        assert!(state.record_view(b"ad-xyz", 10_000 + SPONSORED_REQUEST_TIME_LIMIT_MS));
    }

    #[test]
    fn injection_indices_respect_posts_between() {
        assert_eq!(
            SponsoredMessagesState::compute_injection_indices(12, Some(4), 3),
            vec![3, 7, 11]
        );
        assert_eq!(
            SponsoredMessagesState::compute_injection_indices(2, Some(5), 2),
            vec![1]
        );
    }
}
