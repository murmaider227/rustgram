//! Session navigation controller: section stack, peer history, third column, and adaptive layout.
//!
//! Port origin: `Telegram/SourceFiles/window/window_session_controller.h`,
//! `Telegram/SourceFiles/window/window_session_controller.cpp`,
//! and `Telegram/SourceFiles/mainwidget.cpp` (`showHistory`, `showSection`,
//! `showBackFromStack`, `updateWindowAdaptiveLayout`, `updateThirdColumnToCurrentChat`).

use std::time::Instant;

use crate::window::adaptive::{
    Adaptive, ColumnLayout, LayoutInputs, WindowColumnsState, WindowLayout, compute_column_layout,
    minimal_three_column_width,
};
use crate::window::section::{
    InfoTab, SectionMemento, SectionShow, SectionWay, SettingsPage, SlideAnimation, SlideDirection,
    StackItem,
};
use tg_app_core::stand_in::settings::SavedWindowColumns;
use tg_app_core::stand_in::types::{FilterId, MsgId, PeerId};
use tg_app_info::statistics::StatisticsScreen;

/// Owns navigation state for an authenticated session window.
///
/// Mirrors `Window::SessionController` + `MainWidget` navigation stack.
#[derive(Debug)]
pub struct SessionController {
    adaptive: Adaptive,
    columns: WindowColumnsState,
    last_layout: ColumnLayout,
    active_filter: FilterId,
    search_query: String,
    active_peer: PeerId,
    active_msg_id: MsgId,
    main_section: Option<SectionMemento>,
    third_section: Option<SectionMemento>,
    stack: Vec<StackItem>,
    slide_animation: Option<SlideAnimation>,
    has_dialogs: bool,
    filters_enabled: bool,
}

impl SessionController {
    /// Creates a new `SessionController` initialized from saved column settings.
    #[must_use]
    pub fn new(saved_columns: SavedWindowColumns, filters_enabled: bool) -> Self {
        let columns = WindowColumnsState::from_saved(saved_columns);
        let initial_inputs = LayoutInputs {
            window_width: 1024,
            has_dialogs: true,
            filters_enabled,
            main_section_shown: false,
            chats_force_display_wide: false,
            extending_for_third_section: false,
        };
        let last_layout = compute_column_layout(initial_inputs, &columns);
        let mut adaptive = Adaptive::new();
        adaptive.set_window_layout(last_layout.window_layout);
        adaptive.set_chat_layout(last_layout.chat_layout);

        Self {
            adaptive,
            columns,
            last_layout,
            active_filter: FilterId::ALL,
            search_query: String::new(),
            active_peer: PeerId::ZERO,
            active_msg_id: MsgId::SHOW_AT_UNREAD,
            main_section: None,
            third_section: None,
            stack: Vec::new(),
            slide_animation: None,
            has_dialogs: true,
            filters_enabled,
        }
    }

    /// Returns a reference to the adaptive layout state.
    #[must_use]
    pub const fn adaptive(&self) -> &Adaptive {
        &self.adaptive
    }

    /// Returns a reference to the column width/toggle manager.
    #[must_use]
    pub const fn columns(&self) -> &WindowColumnsState {
        &self.columns
    }

    /// Returns a mutable reference to the column width/toggle manager.
    pub fn columns_mut(&mut self) -> &mut WindowColumnsState {
        &mut self.columns
    }

    /// Returns the most recently computed column layout.
    #[must_use]
    pub const fn layout(&self) -> ColumnLayout {
        self.last_layout
    }

    /// Returns the active chat folder filter.
    #[must_use]
    pub const fn active_filter(&self) -> FilterId {
        self.active_filter
    }

    /// Selects a chat folder filter.
    pub fn set_active_filter(&mut self, filter: FilterId) {
        self.active_filter = filter;
    }

    /// Returns the current dialogs search query.
    #[must_use]
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Returns a mutable handle to the dialogs search query.
    pub fn search_query_mut(&mut self) -> &mut String {
        &mut self.search_query
    }

    /// Clears the dialogs search query and returns `true` if it was non-empty.
    pub fn cancel_search(&mut self) -> bool {
        if self.search_query.is_empty() {
            false
        } else {
            self.search_query.clear();
            true
        }
    }

    /// Enables or disables the vertical folders sidebar.
    pub fn set_filters_enabled(&mut self, enabled: bool) {
        self.filters_enabled = enabled;
    }

    /// Returns whether the vertical folders sidebar preference is enabled.
    #[must_use]
    pub const fn filters_enabled(&self) -> bool {
        self.filters_enabled
    }

    /// Returns the currently active chat peer (or `PeerId::ZERO` when none is selected).
    #[must_use]
    pub const fn active_peer(&self) -> PeerId {
        self.active_peer
    }

    /// Returns the target message id for the active chat history.
    #[must_use]
    pub const fn active_msg_id(&self) -> MsgId {
        self.active_msg_id
    }

    /// Returns the currently displayed non-history main section (e.g. `Settings` or stacked `PeerInfo`).
    #[must_use]
    pub const fn main_section(&self) -> Option<&SectionMemento> {
        self.main_section.as_ref()
    }

    /// Returns a mutable reference to the current main section.
    pub fn main_section_mut(&mut self) -> Option<&mut SectionMemento> {
        self.main_section.as_mut()
    }

    /// Returns the currently displayed third-column section, if any.
    #[must_use]
    pub const fn third_section(&self) -> Option<&SectionMemento> {
        self.third_section.as_ref()
    }

    /// Returns a mutable reference to the third-column section.
    pub fn third_section_mut(&mut self) -> Option<&mut SectionMemento> {
        self.third_section.as_mut()
    }

    /// Returns `true` if either a custom main section or an active peer history is shown.
    ///
    /// Mirrors `MainWidget::isMainSectionShown()`.
    #[must_use]
    pub const fn is_main_section_shown(&self) -> bool {
        self.main_section.is_some() || !self.active_peer.is_zero()
    }

    /// Returns the number of saved entries in the navigation back stack.
    #[must_use]
    pub fn stack_len(&self) -> usize {
        self.stack.len()
    }

    /// Returns `true` when the navigation stack is empty.
    #[must_use]
    pub fn stack_is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Returns `true` if the UI should show a Back button in the main header.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        !self.stack.is_empty()
            || self.main_section.is_some()
            || (self.adaptive.is_one_column() && !self.active_peer.is_zero())
    }

    /// Returns `true` if the current window body is wide enough for a third column without resize.
    ///
    /// Mirrors `SessionController::canShowThirdSectionWithoutResize()`.
    #[must_use]
    pub const fn can_show_third_section_without_resize(&self) -> bool {
        self.last_layout.body_width >= minimal_three_column_width(self.has_dialogs)
    }

    /// Recomputes column geometry for `window_width` and reconciles the third column.
    ///
    /// Mirrors `MainWidget::updateControlsGeometry()` + `updateWindowAdaptiveLayout()`.
    pub fn update_layout(&mut self, window_width: i32) -> ColumnLayout {
        let inputs = LayoutInputs {
            window_width,
            has_dialogs: self.has_dialogs,
            filters_enabled: self.filters_enabled,
            main_section_shown: self.is_main_section_shown(),
            chats_force_display_wide: !self.search_query.is_empty(),
            extending_for_third_section: false,
        };
        let layout = compute_column_layout(inputs, &self.columns);
        self.adaptive.set_window_layout(layout.window_layout);
        self.adaptive.set_chat_layout(layout.chat_layout);
        self.last_layout = layout;

        // Reconcile third column with ThreeColumn availability (`MainWidget::updateControlsGeometry`).
        if matches!(layout.window_layout, WindowLayout::ThreeColumn) {
            if self.third_section.is_none()
                && self.columns.third_section_info_enabled()
                && !self.active_peer.is_zero()
            {
                self.third_section = Some(SectionMemento::PeerInfo {
                    peer_id: self.active_peer,
                    tab: InfoTab::Overview,
                });
            }
        } else {
            self.third_section = None;
        }
        layout
    }

    /// Returns the active slide animation offset if one is in progress, clearing it when done.
    pub fn active_slide_offset(&mut self, now: Instant, span: f32) -> Option<f32> {
        let anim = self.slide_animation.as_ref()?;
        if anim.is_finished(now) {
            self.slide_animation = None;
            None
        } else {
            Some(anim.incoming_offset(now, span))
        }
    }

    /// Returns `true` if a section transition animation is currently running.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.slide_animation
            .as_ref()
            .is_some_and(|anim| !anim.is_finished(tg_app_core::clock::now()))
    }

    /// Immediately settles any running section slide animation (used by deterministic screenshots/tests).
    pub fn finish_slide_animation(&mut self) {
        self.slide_animation = None;
    }

    /// Navigates to a peer's chat history (`SessionController::showPeerHistory` / `MainWidget::showHistory`).
    pub fn show_peer_history(&mut self, peer_id: PeerId, params: SectionShow, msg_id: MsgId) {
        let mut way = params.way;

        if peer_id.is_zero() || way == SectionWay::ClearStack {
            self.stack.clear();
        } else if way == SectionWay::Forward && !params.allow_duplicate_in_stack {
            // Unwind stack back to existing entry for this peer, matching `mainwidget.cpp:1516-1550`.
            if let Some(pos) = self.stack.iter().position(|item| {
                matches!(
                    item.memento,
                    SectionMemento::PeerHistory { peer_id: p, .. } if p == peer_id
                )
            }) {
                self.stack.truncate(pos);
                way = SectionWay::Backward;
            } else if self.active_peer == peer_id {
                way = if self.main_section.is_some() {
                    SectionWay::Backward
                } else {
                    SectionWay::ClearStack
                };
            }
        }

        if way == SectionWay::Forward {
            self.save_current_in_stack(
                &SectionMemento::PeerHistory { peer_id, msg_id },
                params.drop_same_from_stack,
            );
        }

        let was_main_shown = self.is_main_section_shown();
        let was_peer = self.active_peer;
        let had_section = self.main_section.is_some();

        self.main_section = None;
        self.active_peer = peer_id;
        self.active_msg_id = msg_id;

        // Update third column to follow the active peer (`updateThirdColumnToCurrentChat`).
        if peer_id.is_zero() {
            self.third_section = None;
        } else if self.adaptive.is_three_column() && self.columns.third_section_info_enabled() {
            self.third_section = Some(SectionMemento::PeerInfo {
                peer_id,
                tab: InfoTab::Overview,
            });
        }

        if params.animated {
            let should_animate = if params.slide_from_bottom {
                was_main_shown
            } else if peer_id.is_zero() {
                self.adaptive.is_one_column() && was_main_shown
            } else if way == SectionWay::ClearStack && !self.adaptive.is_one_column() {
                false
            } else {
                had_section
                    || was_peer != peer_id
                    || (self.adaptive.is_one_column() && !was_main_shown)
            };

            if should_animate {
                let dir = if params.slide_from_bottom {
                    SlideDirection::FromBottom
                } else if way == SectionWay::Backward || peer_id.is_zero() {
                    SlideDirection::FromLeft
                } else {
                    SlideDirection::FromRight
                };
                self.slide_animation = Some(SlideAnimation::start(dir));
            }
        }
    }

    /// Shows a general section memento (`SessionController::showSection` / `MainWidget::showNewSection`).
    pub fn show_section(&mut self, memento: SectionMemento, params: SectionShow) {
        if let SectionMemento::PeerHistory { peer_id, msg_id } = memento {
            self.show_peer_history(peer_id, params, msg_id);
            return;
        }

        // If already displaying an identical section in the main slot, no-op.
        if !params.third_column && self.main_section.as_ref() == Some(&memento) {
            return;
        }

        // Third-column placement when wide enough and requested.
        if params.third_column && self.adaptive.is_three_column() {
            self.third_section = Some(memento);
            return;
        }

        if params.way == SectionWay::ClearStack {
            self.stack.clear();
        } else if params.way == SectionWay::Forward {
            self.save_current_in_stack(&memento, params.drop_same_from_stack);
        }

        if let Some(peer) = memento.peer_id() {
            self.active_peer = peer;
        }
        self.main_section = Some(memento);

        if params.animated {
            let dir = if params.slide_from_bottom {
                SlideDirection::FromBottom
            } else if params.way == SectionWay::Backward {
                SlideDirection::FromLeft
            } else {
                SlideDirection::FromRight
            };
            self.slide_animation = Some(SlideAnimation::start(dir));
        }
    }

    /// Opens peer info: in the third column if the window is wide enough, otherwise on the main stack.
    ///
    /// Mirrors `SessionNavigation::showPeerInfo` + `MainWidget::showNewSection`.
    pub fn show_peer_info(&mut self, peer_id: PeerId, params: SectionShow) {
        if peer_id.is_zero() {
            return;
        }
        let memento = SectionMemento::PeerInfo {
            peer_id,
            tab: InfoTab::Overview,
        };
        if self.can_show_third_section_without_resize() {
            self.columns.set_third_section_info_enabled(true);
            self.update_layout(self.last_layout.body_width + self.last_layout.filters_width);
            self.show_section(memento, params.with_third_column());
        } else {
            self.show_section(memento, params);
        }
    }

    /// Toggles the peer info view when the user clicks the chat header or info button.
    pub fn toggle_peer_info(&mut self, peer_id: PeerId) {
        if peer_id.is_zero() {
            return;
        }
        if self.can_show_third_section_without_resize() {
            if self.columns.third_section_info_enabled() && self.third_section.is_some() {
                self.close_third_section();
            } else {
                self.show_peer_info(peer_id, SectionShow::forward());
            }
        } else if matches!(
            self.main_section,
            Some(SectionMemento::PeerInfo { peer_id: p, .. }) if p == peer_id
        ) {
            self.show_back_from_stack(SectionShow::forward());
        } else {
            self.show_section(
                SectionMemento::PeerInfo {
                    peer_id,
                    tab: InfoTab::Overview,
                },
                SectionShow::forward(),
            );
        }
    }

    /// Opens the Settings section (`SessionNavigation::showSettings`).
    pub fn show_settings(&mut self, page: SettingsPage) {
        let params = SectionShow {
            drop_same_from_stack: true,
            ..SectionShow::forward()
        };
        self.show_section(SectionMemento::Settings { page }, params);
    }

    /// Opens the Downloads Manager (`InfoSection::Downloads`) on the main section stack.
    pub fn show_downloads(&mut self, peer_id: PeerId) {
        let params = SectionShow {
            drop_same_from_stack: true,
            ..SectionShow::forward()
        };
        self.show_section(
            SectionMemento::PeerInfo {
                peer_id,
                tab: InfoTab::Downloads,
            },
            params,
        );
    }

    /// Opens the Statistics / Boosts / Monetization section for `peer_id`
    /// (`Info::Statistics::Make` / `Info::Boosts::Make`).
    pub fn show_statistics(&mut self, peer_id: PeerId, screen: StatisticsScreen) {
        if peer_id.is_zero() {
            return;
        }
        let params = SectionShow {
            drop_same_from_stack: true,
            ..SectionShow::forward()
        };
        self.show_section(SectionMemento::Statistics { peer_id, screen }, params);
    }

    /// Routes a `tg://`, `t.me/`, `@username`, `internal:`, or `tonsite://` deep link through the
    /// full deep-links router ([`crate::window::deep_links::dispatch_deep_link`]).
    pub fn route_deep_link(
        &mut self,
        uri: &str,
        session_backend: &mut dyn tg_app_core::session_api::AppSession,
    ) -> crate::window::deep_links::DeepLinkOutcome {
        crate::window::deep_links::dispatch_deep_link(self, uri, session_backend)
    }

    /// Routes a `tg://`, `t.me/`, or `@username` internal link into the session navigation stack.
    ///
    /// Returns `true` if the link was handled internally (no external browser / Instant View needed).
    pub fn open_telegram_link<F>(&mut self, uri: &str, resolve_username: F) -> bool
    where
        F: FnOnce(&str) -> Option<PeerId>,
    {
        use crate::window::deep_links::{DeepLink, RepliesByLink, RouterDeepLink, parse_deep_link};

        let Some(parsed) = parse_deep_link(uri) else {
            return false;
        };

        match parsed {
            DeepLink::Router(RouterDeepLink::Settings { page, .. }) => {
                self.show_settings(page);
                true
            }
            DeepLink::Resolve(info) => {
                if let Some(ref domain) = info.domain
                    && let Some(peer_id) = resolve_username(domain)
                {
                    let msg_id = match info.replies {
                        Some(RepliesByLink::Comment(cid)) => MsgId(cid),
                        Some(RepliesByLink::Thread(tid)) => info.post_id.map_or(MsgId(tid), MsgId),
                        None => info.post_id.map_or(MsgId::SHOW_AT_UNREAD, MsgId),
                    };
                    self.show_peer_history(peer_id, SectionShow::forward(), msg_id);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Pops one entry from the navigation stack (`MainWidget::showBackFromStack`).
    ///
    /// Returns `true` if navigation state changed.
    pub fn show_back_from_stack(&mut self, params: SectionShow) -> bool {
        if let Some(item) = self.stack.pop() {
            let back_params = params.with_way(SectionWay::Backward);
            match item.memento {
                SectionMemento::PeerHistory { peer_id, msg_id } => {
                    self.show_peer_history(peer_id, back_params, msg_id);
                }
                other => {
                    self.show_section(other, back_params);
                }
            }
            if let Some(saved_third) = item.third_section
                && self.adaptive.is_three_column()
            {
                self.third_section = Some(saved_third);
            }
            return true;
        }

        // Stack is empty: if a custom main section is open, return to active peer or clear.
        if self.main_section.is_some() {
            self.main_section = None;
            if params.animated {
                self.slide_animation = Some(SlideAnimation::start(SlideDirection::FromLeft));
            }
            return true;
        }

        // In OneColumn or with an active chat, clearing returns to the dialogs list.
        if !self.active_peer.is_zero() {
            self.clear_section_stack(params.with_way(SectionWay::Backward));
            return true;
        }
        false
    }

    /// Clears the navigation stack and active peer (`SessionNavigation::clearSectionStack`).
    pub fn clear_section_stack(&mut self, params: SectionShow) {
        self.show_peer_history(PeerId::ZERO, params, MsgId::SHOW_AT_UNREAD);
    }

    /// Closes the third column and clears its settings toggles (`SessionController::closeThirdSection`).
    pub fn close_third_section(&mut self) {
        self.third_section = None;
        self.columns.set_tabbed_selector_section_enabled(false);
        self.columns.set_third_section_info_enabled(false);
        self.update_layout(self.last_layout.body_width + self.last_layout.filters_width);
    }

    fn save_current_in_stack(&mut self, incoming: &SectionMemento, drop_same: bool) {
        let current = if let Some(section) = self.main_section.clone() {
            if drop_same && section.same_type_as(incoming) {
                return;
            }
            Some(section)
        } else if !self.active_peer.is_zero() {
            Some(SectionMemento::PeerHistory {
                peer_id: self.active_peer,
                msg_id: self.active_msg_id,
            })
        } else {
            None
        };

        if let Some(memento) = current {
            self.stack.push(StackItem {
                memento,
                third_section: self.third_section.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_and_back_navigation_restores_history_and_sections() {
        let mut ctrl = SessionController::new(SavedWindowColumns::default(), false);
        ctrl.update_layout(800); // Normal two-column (not wide enough for 3-col).

        let p1 = PeerId::from_user(10);
        let p2 = PeerId::from_user(20);

        ctrl.show_peer_history(p1, SectionShow::clear_stack(), MsgId::SHOW_AT_UNREAD);
        assert_eq!(ctrl.active_peer(), p1);
        assert!(ctrl.stack_is_empty());

        // Forward to PeerInfo on narrow-ish 800px window -> pushes onto main stack.
        ctrl.show_peer_info(p1, SectionShow::forward());
        assert_eq!(ctrl.stack_len(), 1);
        assert_eq!(
            ctrl.main_section(),
            Some(&SectionMemento::PeerInfo {
                peer_id: p1,
                tab: InfoTab::Overview,
            })
        );

        // Forward to another peer history -> pushes PeerInfo onto stack.
        ctrl.show_peer_history(p2, SectionShow::forward(), MsgId(42));
        assert_eq!(ctrl.stack_len(), 2);
        assert_eq!(ctrl.active_peer(), p2);
        assert!(ctrl.main_section().is_none());

        // Back pops to PeerInfo(p1).
        assert!(ctrl.show_back_from_stack(SectionShow::forward()));
        assert_eq!(ctrl.stack_len(), 1);
        assert_eq!(
            ctrl.main_section(),
            Some(&SectionMemento::PeerInfo {
                peer_id: p1,
                tab: InfoTab::Overview,
            })
        );

        // Back again pops to PeerHistory(p1).
        assert!(ctrl.show_back_from_stack(SectionShow::forward()));
        assert!(ctrl.stack_is_empty());
        assert!(ctrl.main_section().is_none());
        assert_eq!(ctrl.active_peer(), p1);
    }

    #[test]
    fn forward_to_existing_peer_in_stack_unwinds_duplicates() {
        let mut ctrl = SessionController::new(SavedWindowColumns::default(), false);
        ctrl.update_layout(800);

        let p1 = PeerId::from_user(1);
        let p2 = PeerId::from_user(2);
        let p3 = PeerId::from_user(3);

        ctrl.show_peer_history(p1, SectionShow::clear_stack(), MsgId::SHOW_AT_UNREAD);
        ctrl.show_peer_history(p2, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
        ctrl.show_peer_history(p3, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
        assert_eq!(ctrl.stack_len(), 2);

        // Navigating forward to p1 unwinds stack instead of duplicating p1.
        ctrl.show_peer_history(p1, SectionShow::forward(), MsgId::SHOW_AT_UNREAD);
        assert!(ctrl.stack_is_empty());
        assert_eq!(ctrl.active_peer(), p1);
    }

    #[test]
    fn wide_window_opens_peer_info_in_third_column_without_pushing_stack() {
        let mut ctrl = SessionController::new(SavedWindowColumns::default(), false);
        ctrl.update_layout(1200);

        let p1 = PeerId::from_user(5);
        ctrl.show_peer_history(p1, SectionShow::clear_stack(), MsgId::SHOW_AT_UNREAD);
        ctrl.show_peer_info(p1, SectionShow::forward());

        assert!(ctrl.adaptive().is_three_column());
        assert!(ctrl.stack_is_empty());
        assert!(ctrl.main_section().is_none());
        assert_eq!(
            ctrl.third_section(),
            Some(&SectionMemento::PeerInfo {
                peer_id: p1,
                tab: InfoTab::Overview,
            })
        );

        // Resizing below 932 hides third section cleanly; widening restores it.
        ctrl.update_layout(800);
        assert!(ctrl.adaptive().is_normal());
        assert!(ctrl.third_section().is_none());

        ctrl.update_layout(1200);
        assert!(ctrl.adaptive().is_three_column());
        assert!(ctrl.third_section().is_some());

        // Closing third section drops back to Normal layout.
        ctrl.close_third_section();
        assert!(ctrl.adaptive().is_normal());
        assert!(ctrl.third_section().is_none());
    }

    #[test]
    fn settings_subpage_navigation_drops_same_type_on_stack() {
        let mut ctrl = SessionController::new(SavedWindowColumns::default(), false);
        ctrl.update_layout(900);

        let p1 = PeerId::from_user(7);
        ctrl.show_peer_history(p1, SectionShow::clear_stack(), MsgId::SHOW_AT_UNREAD);
        ctrl.show_settings(SettingsPage::Main);
        assert_eq!(ctrl.stack_len(), 1);

        // Switching Settings pages replaces Settings rather than stacking multiple Settings entries.
        ctrl.show_settings(SettingsPage::PrivacySecurity);
        assert_eq!(ctrl.stack_len(), 1);
        assert_eq!(
            ctrl.main_section(),
            Some(&SectionMemento::Settings {
                page: SettingsPage::PrivacySecurity,
            })
        );

        // Single Back returns straight to peer history.
        assert!(ctrl.show_back_from_stack(SectionShow::forward()));
        assert!(ctrl.main_section().is_none());
        assert_eq!(ctrl.active_peer(), p1);
    }

    #[test]
    fn one_column_back_clears_chat_to_dialogs_list() {
        let mut ctrl = SessionController::new(SavedWindowColumns::default(), false);
        ctrl.update_layout(480);
        assert!(ctrl.adaptive().is_one_column());

        let p1 = PeerId::from_user(99);
        ctrl.show_peer_history(p1, SectionShow::clear_stack(), MsgId::SHOW_AT_UNREAD);
        assert!(ctrl.is_main_section_shown());
        assert!(ctrl.can_go_back());

        assert!(ctrl.show_back_from_stack(SectionShow::forward()));
        assert!(!ctrl.is_main_section_shown());
        assert_eq!(ctrl.active_peer(), PeerId::ZERO);
    }
}
