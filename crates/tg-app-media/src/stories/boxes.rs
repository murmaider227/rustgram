//! Modal boxes hosted over the story viewer: Own-Story Stats/Views Box,
//! Stealth Mode Box, Share Story Box, Report Story Box, and Delete Confirmation Box.
//!
//! Port origin:
//! - `Telegram/SourceFiles/media/stories/media_stories_recent_views.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_stealth.cpp` & `media_stories.style`
//! - `Telegram/SourceFiles/media/stories/media_stories_share.cpp`
//! - `Telegram/SourceFiles/media/stories/media_stories_controller.cpp` (delete/report prompts)

use std::collections::BTreeSet;

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Id, Margin, Pos2, Rect, RichText, ScrollArea,
    Sense, Stroke, TextEdit, Ui, Vec2,
};
use tg_data::{FullStoryId, PeerId, TimeId};
use tg_stories::{
    ShareCapability, StealthActivationDecision, StealthMode, StoryView, StoryViewKind, StoryViews,
    evaluate_stealth_mode, time_left_text,
};

use crate::stories::areas::reaction_display_text;
use crate::stories::backend::{PeerProfile, StoriesBackend};
use crate::stories::layout::{format_compact_count, format_story_timestamp};
use crate::stories::strip::paint_avatar_circle;

/// Filter tab inside the Own-Story Stats/Views Box.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ViewsFilter {
    /// All viewers, reactors, and reposters.
    #[default]
    All,
    /// Only viewers who sent a reaction.
    Reactions,
    /// Only public reposts and forwards.
    Reposts,
}

/// Active modal dialog displayed on top of the story viewer.
#[derive(Clone, Debug)]
pub enum StoryModalBox {
    /// Own-story statistics and paginated viewer list (`media_stories_recent_views.cpp`).
    ViewsStats {
        /// Target story ID.
        story_id: FullStoryId,
        /// Whether this is a broadcast channel reactions list instead of self views.
        is_channel: bool,
        /// Active tab filter.
        filter: ViewsFilter,
        /// Viewer name search query.
        search: String,
    },
    /// Stealth Mode explanation & activation box (`media_stories_stealth.cpp`).
    StealthMode {
        /// Optional peer to open immediately after activation.
        pending_open_peer: Option<PeerId>,
    },
    /// Share / forward story dialog (`media_stories_share.cpp`).
    Share {
        /// Share capability returned by `StoryViewerController::share_requested`.
        capability: ShareCapability,
        /// Selected recipient peers.
        selected: BTreeSet<PeerId>,
        /// Search filter text.
        search: String,
        /// Optional accompanying comment.
        comment: String,
    },
    /// Report story prompt.
    Report {
        /// Story being reported.
        story_id: FullStoryId,
        /// Selected reason index.
        selected_reason: usize,
    },
    /// Delete own story confirmation box.
    ConfirmDelete {
        /// Story to delete.
        story_id: FullStoryId,
    },
}

/// Outcome returned after rendering a [`StoryModalBox`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoryModalOutcome {
    /// Keep dialog open.
    None,
    /// Close dialog.
    Close,
    /// Request loading the next views page.
    LoadMoreViews,
    /// Toggle blocking `peer_id` from viewing our own stories.
    ToggleBlockViewer {
        /// Target viewer peer ID.
        peer_id: PeerId,
        /// New blocked state.
        blocked: bool,
    },
    /// Activate Stealth Mode (`stories.activateStealthMode`).
    ActivateStealthMode {
        /// Optional peer to open right after activation.
        pending_open_peer: Option<PeerId>,
    },
    /// Toggle mock Premium status so offline/demo users can test locked vs unlocked Stealth Mode
    /// (never emitted in live sessions).
    ToggleMockPremium,
    /// Open the Telegram Premium overview (emitted in live sessions when Stealth Mode requires Premium).
    OpenPremium,
    /// Share story to selected recipients.
    SubmitShare {
        /// Story ID.
        story_id: FullStoryId,
        /// Target peer IDs.
        targets: Vec<PeerId>,
        /// Optional comment.
        comment: String,
    },
    /// Copy direct `t.me/<user>/s/<id>` link.
    CopyStoryLink(FullStoryId),
    /// Submit story report.
    SubmitReport {
        /// Story ID.
        story_id: FullStoryId,
        /// Reason string.
        reason: String,
    },
    /// Confirm story deletion.
    ConfirmDelete(FullStoryId),
}

const REPORT_REASONS: &[&str] = &[
    "Spam",
    "Violence",
    "Copyright infringement",
    "Illegal content",
    "Other",
];

/// Filters a `StoryViews` list by [`ViewsFilter`] and case-insensitive peer name query.
#[must_use]
pub fn filter_story_views<'a>(
    views: &'a [StoryView],
    filter: ViewsFilter,
    query: &str,
    peer_name: impl Fn(PeerId) -> String,
) -> Vec<&'a StoryView> {
    let q = query.trim().to_lowercase();
    views
        .iter()
        .filter(|v| {
            let kind_ok = match filter {
                ViewsFilter::All => true,
                ViewsFilter::Reactions => v.reaction.is_some(),
                ViewsFilter::Reposts => v.kind() != StoryViewKind::Viewed,
            };
            if !kind_ok {
                return false;
            }
            if q.is_empty() {
                return true;
            }
            peer_name(v.peer_id).to_lowercase().contains(&q)
        })
        .collect()
}

/// Renders the active [`StoryModalBox`] centered inside `viewport`.
#[allow(clippy::too_many_arguments)]
pub fn show_story_modal(
    ctx: &egui::Context,
    viewport: Rect,
    modal: &mut StoryModalBox,
    views_snapshot: &StoryViews,
    stealth_mode: StealthMode,
    backend: &dyn StoriesBackend,
    now_secs: TimeId,
) -> StoryModalOutcome {
    let mut outcome = StoryModalOutcome::None;

    // Dimming backdrop over the viewer.
    egui::Area::new(Id::unique("story_modal_backdrop"))
        .order(egui::Order::Foreground)
        .fade_in(false)
        .fixed_pos(viewport.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(viewport.size(), Sense::click());
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 145));
            if resp.clicked() {
                outcome = StoryModalOutcome::Close;
            }
        });

    let card_w = match modal {
        StoryModalBox::ViewsStats { .. } => 420.0_f32.min(viewport.width() - 32.0),
        StoryModalBox::StealthMode { .. } => 388.0_f32.min(viewport.width() - 32.0),
        StoryModalBox::Share { .. } => 400.0_f32.min(viewport.width() - 32.0),
        StoryModalBox::Report { .. } | StoryModalBox::ConfirmDelete { .. } => {
            340.0_f32.min(viewport.width() - 32.0)
        }
    };

    egui::Area::new(Id::unique("story_modal_card"))
        .order(egui::Order::Tooltip)
        .fade_in(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            Frame::NONE
                .fill(Color32::from_rgb(0x1E, 0x26, 0x30))
                .stroke(Stroke::new(1.0, Color32::from_rgb(0x32, 0x3E, 0x4C)))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(card_w);
                    ui.visuals_mut().weak_text_color = Some(Color32::from_rgb(0x98, 0xA8, 0xB8));
                    let inner_out = match modal {
                        StoryModalBox::ViewsStats {
                            is_channel,
                            filter,
                            search,
                            ..
                        } => show_views_stats_box(
                            ui,
                            views_snapshot,
                            *is_channel,
                            filter,
                            search,
                            backend,
                            now_secs,
                        ),
                        StoryModalBox::StealthMode { pending_open_peer } => show_stealth_mode_box(
                            ui,
                            stealth_mode,
                            backend.account_premium(),
                            !backend.supports_mock_premium_toggle(),
                            *pending_open_peer,
                            now_secs,
                        ),
                        StoryModalBox::Share {
                            capability,
                            selected,
                            search,
                            comment,
                        } => show_share_box(ui, *capability, selected, search, comment, backend),
                        StoryModalBox::Report {
                            story_id,
                            selected_reason,
                        } => show_report_box(ui, *story_id, selected_reason),
                        StoryModalBox::ConfirmDelete { story_id } => {
                            show_confirm_delete_box(ui, *story_id)
                        }
                    };
                    if inner_out != StoryModalOutcome::None {
                        outcome = inner_out;
                    }
                });
        });

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return StoryModalOutcome::Close;
    }

    outcome
}

fn show_views_stats_box(
    ui: &mut Ui,
    views: &StoryViews,
    is_channel: bool,
    filter: &mut ViewsFilter,
    search: &mut String,
    backend: &dyn StoriesBackend,
    now_secs: TimeId,
) -> StoryModalOutcome {
    let mut outcome = StoryModalOutcome::None;
    let title = if is_channel {
        "Story Reactions & Stats"
    } else {
        "Story Views & Statistics"
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .size(16.0)
                .strong()
                .color(Color32::WHITE),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if close_x_button(ui) {
                outcome = StoryModalOutcome::Close;
            }
        });
    });

    ui.add_space(10.0);

    // 3-column summary stats strip: Views, Reactions, Reposts/Forwards.
    ui.columns(3, |cols| {
        paint_stat_tile(
            &mut cols[0],
            "Views",
            format_compact_count(views.views),
            Color32::from_rgb(0x54, 0xC3, 0xF3),
        );
        paint_stat_tile(
            &mut cols[1],
            "Reactions",
            format_compact_count(views.reactions),
            Color32::from_rgb(0xF4, 0x6A, 0x7A),
        );
        paint_stat_tile(
            &mut cols[2],
            "Shares",
            format_compact_count(views.forwards),
            Color32::from_rgb(0x66, 0xBB, 0x6A),
        );
    });

    ui.add_space(10.0);

    // Filter tabs.
    ui.horizontal(|ui| {
        for (tab, label) in [
            (ViewsFilter::All, format!("All ({})", views.list.len())),
            (
                ViewsFilter::Reactions,
                format!(
                    "Reactions ({})",
                    views.list.iter().filter(|v| v.reaction.is_some()).count()
                ),
            ),
            (
                ViewsFilter::Reposts,
                format!(
                    "Reposts ({})",
                    views
                        .list
                        .iter()
                        .filter(|v| v.kind() != StoryViewKind::Viewed)
                        .count()
                ),
            ),
        ] {
            let active = *filter == tab;
            let btn = egui::Button::new(RichText::new(label).size(12.0).color(if active {
                Color32::WHITE
            } else {
                Color32::from_rgb(0x9A, 0xAA, 0xBA)
            }))
            .fill(if active {
                Color32::from_rgb(0x3B, 0x82, 0xC4)
            } else {
                Color32::from_rgb(0x28, 0x33, 0x40)
            })
            .corner_radius(CornerRadius::same(12));
            if ui.add(btn).clicked() {
                *filter = tab;
            }
        }
    });

    ui.add_space(8.0);
    ui.add_sized(
        Vec2::new(ui.available_width(), 30.0),
        TextEdit::singleline(search)
            .background_color(Color32::from_rgb(0x14, 0x1B, 0x22))
            .text_color(Color32::WHITE)
            .hint_text(
                RichText::new("Search viewers...").color(Color32::from_rgb(0x88, 0x99, 0xA8)),
            ),
    );
    ui.add_space(8.0);

    let filtered = filter_story_views(&views.list, *filter, search, |pid| {
        backend.peer_profile(pid).name
    });

    ScrollArea::vertical()
        .id_salt("story_views_stats_scroll")
        .max_height(260.0)
        .show(ui, |ui| {
            if filtered.is_empty() {
                ui.add_space(18.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("No matching viewers yet")
                            .size(13.0)
                            .color(Color32::from_rgb(0x88, 0x99, 0xA8)),
                    );
                });
                ui.add_space(18.0);
                return;
            }
            for view in filtered {
                let profile = backend.peer_profile(view.peer_id);
                let blocked =
                    view.blocked_my_stories_from || backend.is_viewer_stories_blocked(view.peer_id);
                if let Some(toggle) =
                    paint_viewer_row(ui, view, &profile, blocked, is_channel, now_secs)
                {
                    outcome = StoryModalOutcome::ToggleBlockViewer {
                        peer_id: view.peer_id,
                        blocked: toggle,
                    };
                }
            }
        });

    if !views.next_offset.is_empty() || (views.total as usize) > views.list.len() {
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            if ui.button("Load more viewers").clicked() {
                outcome = StoryModalOutcome::LoadMoreViews;
            }
        });
    }

    outcome
}

fn paint_stat_tile(ui: &mut Ui, label: &str, value: String, accent: Color32) {
    Frame::NONE
        .fill(Color32::from_rgb(0x26, 0x30, 0x3C))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(value).size(18.0).strong().color(accent));
                ui.label(
                    RichText::new(label)
                        .size(11.0)
                        .color(Color32::from_rgb(0x98, 0xA8, 0xB8)),
                );
            });
        });
}

fn paint_viewer_row(
    ui: &mut Ui,
    view: &StoryView,
    profile: &PeerProfile,
    blocked: bool,
    is_channel: bool,
    now_secs: TimeId,
) -> Option<bool> {
    let mut toggle_block = None;
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 48.0), Sense::hover());
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            Color32::from_rgb(0x27, 0x32, 0x40),
        );
    }

    let avatar_c = Pos2::new(rect.left() + 24.0, rect.center().y);
    paint_avatar_circle(ui.painter(), avatar_c, 18.0, profile, 12.0);

    let text_x = rect.left() + 50.0;
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        &profile.name,
        FontId::proportional(13.5),
        Color32::WHITE,
    );

    let kind_label = match view.kind() {
        StoryViewKind::Viewed => format!("Viewed {}", format_story_timestamp(view.date, now_secs)),
        StoryViewKind::Reposted => {
            format!("Reposted • {}", format_story_timestamp(view.date, now_secs))
        }
        StoryViewKind::Forwarded => {
            format!(
                "Forwarded • {}",
                format_story_timestamp(view.date, now_secs)
            )
        }
    };
    let sub_color = match view.kind() {
        StoryViewKind::Viewed => Color32::from_rgb(0x8E, 0x9F, 0xB0),
        StoryViewKind::Reposted => Color32::from_rgb(0x66, 0xBB, 0x6A),
        StoryViewKind::Forwarded => Color32::from_rgb(0x54, 0xC3, 0xF3),
    };
    ui.painter().text(
        Pos2::new(text_x, rect.top() + 33.0),
        Align2::LEFT_CENTER,
        kind_label,
        FontId::proportional(11.5),
        sub_color,
    );

    // Reaction glyph pill on the right.
    let mut right_cursor = rect.right() - 8.0;
    if !is_channel {
        let hide_w = 56.0;
        let hide_rect = Rect::from_min_size(
            Pos2::new(right_cursor - hide_w, rect.center().y - 11.0),
            Vec2::new(hide_w, 22.0),
        );
        let btn_resp = ui.interact(
            hide_rect,
            Id::unique(("story_viewer_block", view.peer_id.raw())),
            Sense::click(),
        );
        let fill = if blocked {
            Color32::from_rgb(0x6E, 0x2C, 0x2C)
        } else if btn_resp.hovered() {
            Color32::from_rgb(0x36, 0x45, 0x56)
        } else {
            Color32::from_rgb(0x2A, 0x35, 0x43)
        };
        ui.painter()
            .rect_filled(hide_rect, CornerRadius::same(6), fill);
        ui.painter().text(
            hide_rect.center(),
            Align2::CENTER_CENTER,
            if blocked { "Hidden" } else { "Hide" },
            FontId::proportional(10.5),
            Color32::WHITE,
        );
        if btn_resp
            .on_hover_text(if blocked {
                "Allow this user to see your stories"
            } else {
                "Hide your stories from this user"
            })
            .clicked()
        {
            toggle_block = Some(!blocked);
        }
        right_cursor -= hide_w + 8.0;
    }

    if let Some(reaction) = &view.reaction {
        ui.painter().text(
            Pos2::new(right_cursor - 10.0, rect.center().y),
            Align2::CENTER_CENTER,
            reaction_display_text(reaction),
            FontId::proportional(16.0),
            Color32::WHITE,
        );
    }

    toggle_block
}

fn show_stealth_mode_box(
    ui: &mut Ui,
    stealth_mode: StealthMode,
    is_premium: bool,
    is_live_session: bool,
    pending_open_peer: Option<PeerId>,
    now_secs: TimeId,
) -> StoryModalOutcome {
    let mut outcome = StoryModalOutcome::None;
    let decision = evaluate_stealth_mode(stealth_mode, now_secs, is_premium);

    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if close_x_button(ui) {
                outcome = StoryModalOutcome::Close;
            }
        });
    });

    // Centered stealth emblem circle (`storiesStealthStyleDefault.logoBg`).
    ui.vertical_centered(|ui| {
        let (logo_rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
        ui.painter().circle_filled(
            logo_rect.center(),
            32.0,
            Color32::from_rgb(0x40, 0xA7, 0xE3),
        );
        ui.painter().text(
            logo_rect.center(),
            Align2::CENTER_CENTER,
            "\u{1F576}",
            FontId::proportional(28.0),
            Color32::WHITE,
        );
        ui.add_space(10.0);
        ui.label(
            RichText::new("Stealth Mode")
                .size(18.0)
                .strong()
                .color(Color32::WHITE),
        );
        ui.add_space(4.0);
        let subtitle = if is_premium {
            "View stories anonymously without appearing in viewers lists."
        } else {
            "Unlock Stealth Mode with Telegram Premium to watch stories anonymously."
        };
        ui.label(
            RichText::new(subtitle)
                .size(12.5)
                .color(Color32::from_rgb(0x98, 0xA8, 0xB8)),
        );
    });

    ui.add_space(14.0);

    // Feature rows (`FeaturePast` & `FeatureNext` in `media_stories_stealth.cpp:139-155`).
    paint_stealth_feature_row(
        ui,
        "5m",
        "Hide Recent Views",
        "Erase your views from any stories you opened in the last 5 minutes.",
    );
    ui.add_space(8.0);
    paint_stealth_feature_row(
        ui,
        "25m",
        "Hide Future Views",
        "Watch stories anonymously for the next 25 minutes.",
    );

    ui.add_space(16.0);

    let (btn_text, btn_fill, enabled) = match decision {
        StealthActivationDecision::ReadyToActivate => (
            if pending_open_peer.is_some() {
                "ENABLE STEALTH MODE & OPEN".to_owned()
            } else {
                "ENABLE STEALTH MODE".to_owned()
            },
            Color32::from_rgb(0x40, 0xA7, 0xE3),
            true,
        ),
        StealthActivationDecision::AlreadyActive { seconds_left } => (
            format!("ACTIVE • {}", time_left_text(seconds_left)),
            Color32::from_rgb(0x2E, 0x7D, 0x32),
            false,
        ),
        StealthActivationDecision::InCooldown { seconds_left } => (
            format!("COOLDOWN • {}", time_left_text(seconds_left)),
            Color32::from_rgb(0x36, 0x45, 0x56),
            false,
        ),
        StealthActivationDecision::RequiresPremium => (
            "\u{1F512} UNLOCK WITH TELEGRAM PREMIUM".to_owned(),
            Color32::from_rgb(0x6A, 0x4C, 0xC4),
            true,
        ),
    };

    let btn = egui::Button::new(
        RichText::new(btn_text)
            .size(13.0)
            .strong()
            .color(if enabled {
                Color32::WHITE
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 130)
            }),
    )
    .fill(btn_fill)
    .min_size(Vec2::new(ui.available_width(), 42.0))
    .corner_radius(CornerRadius::same(8));

    if ui.add(btn).clicked() {
        match decision {
            StealthActivationDecision::ReadyToActivate => {
                outcome = StoryModalOutcome::ActivateStealthMode { pending_open_peer };
            }
            StealthActivationDecision::RequiresPremium => {
                outcome = if is_live_session {
                    StoryModalOutcome::OpenPremium
                } else {
                    StoryModalOutcome::ToggleMockPremium
                };
            }
            StealthActivationDecision::AlreadyActive { .. }
            | StealthActivationDecision::InCooldown { .. } => {
                outcome = StoryModalOutcome::Close;
            }
        }
    }

    outcome
}

fn paint_stealth_feature_row(ui: &mut Ui, badge: &str, title: &str, about: &str) {
    ui.horizontal(|ui| {
        let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
        ui.painter().circle_filled(
            icon_rect.center(),
            16.0,
            Color32::from_rgb(0x28, 0x3A, 0x4E),
        );
        ui.painter().text(
            icon_rect.center(),
            Align2::CENTER_CENTER,
            badge,
            FontId::proportional(11.5),
            Color32::from_rgb(0x54, 0xC3, 0xF3),
        );
        ui.vertical(|ui| {
            ui.label(
                RichText::new(title)
                    .size(13.5)
                    .strong()
                    .color(Color32::WHITE),
            );
            ui.label(
                RichText::new(about)
                    .size(12.0)
                    .color(Color32::from_rgb(0x98, 0xA8, 0xB8)),
            );
        });
    });
}

fn show_share_box(
    ui: &mut Ui,
    capability: ShareCapability,
    selected: &mut BTreeSet<PeerId>,
    search: &mut String,
    comment: &mut String,
    backend: &dyn StoriesBackend,
) -> StoryModalOutcome {
    let mut outcome = StoryModalOutcome::None;
    let (story_id, can_copy_link) = match capability {
        ShareCapability::Unavailable => {
            ui.label(
                RichText::new("This story cannot be shared.")
                    .size(14.0)
                    .color(Color32::WHITE),
            );
            ui.add_space(10.0);
            if ui.button("Close").clicked() {
                return StoryModalOutcome::Close;
            }
            return StoryModalOutcome::None;
        }
        ShareCapability::DirectLinkOnly { story_id } => (story_id, true),
        ShareCapability::ShareStory {
            story_id,
            can_copy_link,
        } => (story_id, can_copy_link),
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Share Story")
                .size(16.0)
                .strong()
                .color(Color32::WHITE),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if close_x_button(ui) {
                outcome = StoryModalOutcome::Close;
            }
        });
    });

    ui.add_space(8.0);
    ui.add_sized(
        Vec2::new(ui.available_width(), 30.0),
        TextEdit::singleline(search)
            .background_color(Color32::from_rgb(0x14, 0x1B, 0x22))
            .text_color(Color32::WHITE)
            .hint_text(RichText::new("Search chats...").color(Color32::from_rgb(0x88, 0x99, 0xA8))),
    );
    ui.add_space(8.0);

    let q = search.trim().to_lowercase();
    let targets: Vec<_> = backend
        .share_targets()
        .into_iter()
        .filter(|t| t.can_send && (q.is_empty() || t.name.to_lowercase().contains(&q)))
        .collect();

    ScrollArea::vertical()
        .id_salt("story_share_targets_scroll")
        .max_height(210.0)
        .show(ui, |ui| {
            for target in &targets {
                let is_sel = selected.contains(&target.peer_id);
                let (row_rect, resp) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
                if is_sel || resp.hovered() {
                    ui.painter().rect_filled(
                        row_rect,
                        CornerRadius::same(6),
                        if is_sel {
                            Color32::from_rgb(0x2B, 0x52, 0x78)
                        } else {
                            Color32::from_rgb(0x27, 0x32, 0x40)
                        },
                    );
                }
                let [r, g, b] = target.avatar_rgb;
                let c = Pos2::new(row_rect.left() + 20.0, row_rect.center().y);
                ui.painter()
                    .circle_filled(c, 15.0, Color32::from_rgb(r, g, b));
                ui.painter().text(
                    c,
                    Align2::CENTER_CENTER,
                    &target.initials,
                    FontId::proportional(11.0),
                    Color32::WHITE,
                );
                ui.painter().text(
                    Pos2::new(row_rect.left() + 44.0, row_rect.center().y),
                    Align2::LEFT_CENTER,
                    &target.name,
                    FontId::proportional(13.5),
                    Color32::WHITE,
                );
                if is_sel {
                    ui.painter().text(
                        Pos2::new(row_rect.right() - 16.0, row_rect.center().y),
                        Align2::CENTER_CENTER,
                        "\u{2713}",
                        FontId::proportional(14.0),
                        Color32::from_rgb(0x54, 0xC3, 0xF3),
                    );
                }
                if resp.clicked() {
                    if is_sel {
                        selected.remove(&target.peer_id);
                    } else {
                        selected.insert(target.peer_id);
                    }
                }
            }
        });

    ui.add_space(8.0);
    ui.add_sized(
        Vec2::new(ui.available_width(), 28.0),
        TextEdit::singleline(comment).hint_text("Write a comment..."),
    );
    ui.add_space(12.0);

    ui.horizontal(|ui| {
        if can_copy_link && ui.button("Copy Link").clicked() {
            outcome = StoryModalOutcome::CopyStoryLink(story_id);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can_send = !selected.is_empty();
            let send_btn = egui::Button::new(
                RichText::new(if selected.is_empty() {
                    "Send".to_owned()
                } else {
                    format!("Send ({})", selected.len())
                })
                .color(Color32::WHITE),
            )
            .fill(if can_send {
                Color32::from_rgb(0x40, 0xA7, 0xE3)
            } else {
                Color32::from_rgb(0x36, 0x45, 0x56)
            });
            if ui.add_enabled(can_send, send_btn).clicked() {
                outcome = StoryModalOutcome::SubmitShare {
                    story_id,
                    targets: selected.iter().copied().collect(),
                    comment: comment.clone(),
                };
            }
        });
    });

    outcome
}

fn show_report_box(
    ui: &mut Ui,
    story_id: FullStoryId,
    selected_reason: &mut usize,
) -> StoryModalOutcome {
    let mut outcome = StoryModalOutcome::None;
    ui.label(
        RichText::new("Report Story")
            .size(16.0)
            .strong()
            .color(Color32::WHITE),
    );
    ui.add_space(8.0);
    for (idx, reason) in REPORT_REASONS.iter().enumerate() {
        if ui
            .radio(
                *selected_reason == idx,
                RichText::new(*reason).color(Color32::WHITE),
            )
            .clicked()
        {
            *selected_reason = idx;
        }
    }
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui.button("Cancel").clicked() {
            outcome = StoryModalOutcome::Close;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = egui::Button::new(RichText::new("Report").color(Color32::WHITE))
                .fill(Color32::from_rgb(0xD1, 0x4E, 0x4E));
            if ui.add(btn).clicked() {
                let reason = REPORT_REASONS
                    .get(*selected_reason)
                    .copied()
                    .unwrap_or("Other")
                    .to_owned();
                outcome = StoryModalOutcome::SubmitReport { story_id, reason };
            }
        });
    });
    outcome
}

fn show_confirm_delete_box(ui: &mut Ui, story_id: FullStoryId) -> StoryModalOutcome {
    let mut outcome = StoryModalOutcome::None;
    ui.label(
        RichText::new("Delete Story")
            .size(16.0)
            .strong()
            .color(Color32::WHITE),
    );
    ui.add_space(8.0);
    ui.label(
        RichText::new("Are you sure you want to delete this story? This cannot be undone.")
            .size(13.0)
            .color(Color32::from_rgb(0xB0, 0xBE, 0xCB)),
    );
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if ui.button("Cancel").clicked() {
            outcome = StoryModalOutcome::Close;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = egui::Button::new(RichText::new("Delete").color(Color32::WHITE))
                .fill(Color32::from_rgb(0xD1, 0x4E, 0x4E));
            if ui.add(btn).clicked() {
                outcome = StoryModalOutcome::ConfirmDelete(story_id);
            }
        });
    });
    outcome
}

fn close_x_button(ui: &mut Ui) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .circle_filled(rect.center(), 12.0, Color32::from_rgb(0x30, 0x3C, 0x4A));
    }
    let c = rect.center();
    let s = Stroke::new(1.6, Color32::from_rgb(0xB0, 0xBE, 0xCB));
    ui.painter().line_segment(
        [
            Pos2::new(c.x - 4.5, c.y - 4.5),
            Pos2::new(c.x + 4.5, c.y + 4.5),
        ],
        s,
    );
    ui.painter().line_segment(
        [
            Pos2::new(c.x + 4.5, c.y - 4.5),
            Pos2::new(c.x - 4.5, c.y + 4.5),
        ],
        s,
    );
    resp.clicked()
}
