//! Notifications for Reactions settings subsection.
//!
//! Port origin: `Telegram/SourceFiles/settings/sections/settings_notifications_reactions.cpp`
//! and `Telegram/SourceFiles/api/api_reactions_notify_settings.cpp`.

use egui::Ui;
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;

use super::backend::{NotificationEventsSettings, ReactionsNotifyFrom, SettingsBackend};
use super::widgets::{
    SettingsIconKind, settings_card, settings_divider, settings_radio_row,
    settings_split_toggle_row, settings_toggle_row, subsection_title,
};

/// Which reactions notification category is currently open in the audience picker (`ShowFromBox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionNotifyTarget {
    /// Reactions to the user's messages (`messages_notify_from`).
    Messages,
    /// Votes in the user's polls (`poll_votes_notify_from`).
    PollVotes,
}

impl ReactionNotifyTarget {
    /// Localized section heading for the audience picker.
    #[must_use]
    pub fn localized_label(self) -> String {
        match self {
            Self::Messages => tr::lng_notification_reactions_messages_full(),
            Self::PollVotes => tr::lng_notification_reactions_poll_votes_full(),
        }
    }
}

/// Persistent UI state for `Settings -> Notifications -> Reactions`.
#[derive(Debug, Clone, Default)]
pub struct ReactionsNotifyState {
    /// Currently open `ShowFromBox` audience selector, if any.
    pub editing_target: Option<ReactionNotifyTarget>,
    /// Staged selection inside `ShowFromBox` (`All` or `Contacts`).
    pub draft_from: ReactionsNotifyFrom,
}

impl ReactionsNotifyState {
    /// Opens the audience picker (`ShowFromBox`) for `target`, defaulting `None` to `All`
    /// just like `settings_notifications_reactions.cpp:53-55`.
    pub fn open_picker(&mut self, target: ReactionNotifyTarget, current: ReactionsNotifyFrom) {
        self.editing_target = Some(target);
        self.draft_from = if current == ReactionsNotifyFrom::None {
            ReactionsNotifyFrom::All
        } else {
            current
        };
    }

    /// Closes the audience picker without applying changes.
    pub fn close_picker(&mut self) {
        self.editing_target = None;
    }

    /// Applies the currently selected radio option in `ShowFromBox` to `events`.
    pub fn apply_picker(&mut self, events: &mut NotificationEventsSettings) -> bool {
        let Some(target) = self.editing_target.take() else {
            return false;
        };
        let chosen = if self.draft_from == ReactionsNotifyFrom::None {
            ReactionsNotifyFrom::All
        } else {
            self.draft_from
        };
        match target {
            ReactionNotifyTarget::Messages => events.message_reactions = chosen,
            ReactionNotifyTarget::PollVotes => events.poll_votes = chosen,
        }
        true
    }
}

/// Renders the `Settings -> Notifications -> Reactions` section
/// (`BuildNotificationsReactionsContent` in `settings_notifications_reactions.cpp`).
///
/// Returns an optional toast message if an action produces user feedback.
pub fn render_notifications_reactions_section(
    ui: &mut Ui,
    state: &mut ReactionsNotifyState,
    backend: &mut dyn SettingsBackend,
    colors: &dyn Colors,
) -> Option<String> {
    let mut events = backend.notification_events();
    let mut changed = false;

    // 1. "Notify me about" split toggle rows (`Messages` and `Poll votes`).
    subsection_title(ui, &tr::lng_notification_reactions_notify_about(), colors);

    // Messages reactions split toggle row.
    {
        let title = tr::lng_notification_reactions_messages_full();
        let subtitle = events.message_reactions.localized_label();
        let mut enabled = events.message_reactions != ReactionsNotifyFrom::None;
        let (clicked, toggled) = settings_split_toggle_row(
            ui,
            &title,
            &subtitle,
            Some(SettingsIconKind::Chat),
            &mut enabled,
            colors,
        );
        if toggled {
            events.message_reactions = if enabled {
                ReactionsNotifyFrom::All
            } else {
                ReactionsNotifyFrom::None
            };
            if state.editing_target == Some(ReactionNotifyTarget::Messages) && !enabled {
                state.close_picker();
            }
            changed = true;
        } else if clicked {
            if events.message_reactions == ReactionsNotifyFrom::None {
                events.message_reactions = ReactionsNotifyFrom::All;
                changed = true;
            } else if state.editing_target == Some(ReactionNotifyTarget::Messages) {
                state.close_picker();
            } else {
                state.open_picker(ReactionNotifyTarget::Messages, events.message_reactions);
            }
        }
    }

    // Poll votes split toggle row.
    {
        let title = tr::lng_notification_reactions_poll_votes_full();
        let subtitle = events.poll_votes.localized_label();
        let mut enabled = events.poll_votes != ReactionsNotifyFrom::None;
        let (clicked, toggled) = settings_split_toggle_row(
            ui,
            &title,
            &subtitle,
            Some(SettingsIconKind::Notifications),
            &mut enabled,
            colors,
        );
        if toggled {
            events.poll_votes = if enabled {
                ReactionsNotifyFrom::All
            } else {
                ReactionsNotifyFrom::None
            };
            if state.editing_target == Some(ReactionNotifyTarget::PollVotes) && !enabled {
                state.close_picker();
            }
            changed = true;
        } else if clicked {
            if events.poll_votes == ReactionsNotifyFrom::None {
                events.poll_votes = ReactionsNotifyFrom::All;
                changed = true;
            } else if state.editing_target == Some(ReactionNotifyTarget::PollVotes) {
                state.close_picker();
            } else {
                state.open_picker(ReactionNotifyTarget::PollVotes, events.poll_votes);
            }
        }
    }

    // Inline `ShowFromBox` card when choosing Everyone vs Contacts.
    if let Some(target) = state.editing_target {
        ui.add_space(6.0);
        settings_card(ui, colors, |ui| {
            ui.label(
                egui::RichText::new(tr::lng_notification_reactions_from())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.color("windowBoldFg")),
            );
            ui.label(
                egui::RichText::new(target.localized_label())
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("windowSubTextFg")),
            );
            ui.add_space(4.0);
            settings_radio_row(
                ui,
                &mut state.draft_from,
                ReactionsNotifyFrom::All,
                &tr::lng_notification_reactions_from_all(),
                true,
                colors,
            );
            settings_radio_row(
                ui,
                &mut state.draft_from,
                ReactionsNotifyFrom::Contacts,
                &tr::lng_notification_reactions_from_contacts(),
                true,
                colors,
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if RoundButton::new(&tr::lng_box_ok(), RoundButtonStyle::default_active(colors))
                    .show(ui)
                    .clicked()
                    && state.apply_picker(&mut events)
                {
                    changed = true;
                }
                if RoundButton::new(&tr::lng_cancel(), RoundButtonStyle::default_light(colors))
                    .show(ui)
                    .clicked()
                {
                    state.close_picker();
                }
            });
        });
    }

    settings_divider(ui, None, colors);

    // 2. "Settings" subsection: Show sender's name (`show_previews`).
    subsection_title(ui, &tr::lng_notification_reactions_settings(), colors);
    let show_sender_lbl = tr::lng_notification_reactions_show_sender();
    if settings_toggle_row(
        ui,
        &show_sender_lbl,
        None,
        None,
        &mut events.show_reaction_previews,
        true,
        colors,
    ) {
        changed = true;
    }

    if changed {
        backend.set_notification_events(events);
    }

    ui.add_space(16.0);
    None
}
