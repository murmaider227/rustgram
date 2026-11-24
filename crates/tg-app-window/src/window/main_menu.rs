//! Slide-out hamburger main menu panel.
//!
//! Port origin: `Telegram/SourceFiles/window/window_main_menu.h`,
//! `Telegram/SourceFiles/window/window_main_menu.cpp`,
//! and `Telegram/SourceFiles/window/window_main_menu.style`.

use egui::{
    Align, Area, Color32, CursorIcon, Frame, Id, Key, Layout, Margin, Order, Pos2, Rect, RichText,
    Sense, Stroke, Ui, Vec2,
};

use tg_app_core::core_types::startup_types::UpdateSnapshot;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::AuthUser;

/// Main menu drawer width (`st::mainMenuWidth`).
pub const MAIN_MENU_WIDTH: f32 = 274.0;
/// Main menu top cover height (`st::mainMenuCoverHeight`).
pub const MAIN_MENU_COVER_HEIGHT: f32 = 134.0;

/// One signed-in account entry rendered inside the expandable Main Menu accounts list
/// (`Window::MainMenu::setupAccounts` / `Settings::SetupAccounts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainMenuAccountItem {
    /// Slot index inside `AccountManager` / `Domain` (`0`, `1`, ...).
    pub slot_index: i32,
    /// Full display name shown next to the avatar.
    pub name: String,
    /// Two-letter avatar initials fallback.
    pub initials: String,
    /// RGB accent tint for the circular avatar.
    pub avatar_rgb: [u8; 3],
    /// Formatted phone number (used for tooltips / accessibility).
    pub phone: String,
    /// Whether this entry is the currently active account (draws accent ring around avatar).
    pub is_active: bool,
    /// Whether this account has Telegram Premium.
    pub is_premium: bool,
    /// Unread badge count for this account.
    pub unread_count: u32,
    /// Whether all unread items for this account are muted (renders muted gray badge).
    pub unread_muted: bool,
}

impl MainMenuAccountItem {
    /// Constructs a single active account entry from an [`AuthUser`].
    #[must_use]
    pub fn from_active_user(user: &AuthUser, slot_index: i32) -> Self {
        Self {
            slot_index,
            name: user.name.clone(),
            initials: user.initials.clone(),
            avatar_rgb: user.avatar_rgb,
            phone: user.phone.clone(),
            is_active: true,
            is_premium: false,
            unread_count: 0,
            unread_muted: false,
        }
    }
}

/// User actions triggered from the main menu drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainMenuAction {
    /// Switch active account to the given slot index (`Main::Domain::activate`).
    SwitchAccount(i32),
    /// Open login intro to add a new account slot (`Main::Domain::addActivated`).
    AddAccount,
    /// Open the user's own profile in Info.
    MyProfile,
    /// Open New Group placeholder box.
    NewGroup,
    /// Open New Channel placeholder box.
    NewChannel,
    /// Open Contacts placeholder box.
    Contacts,
    /// Open Calls placeholder box.
    Calls,
    /// Jump to Saved Messages history.
    SavedMessages,
    /// Open Downloads Manager (`InfoSection::Downloads`).
    Downloads,
    /// Open Settings section.
    Settings,
    /// Toggle Day/Night theme mode.
    ToggleNightMode,
    /// Lock the app with the configured passcode (or prompt to set one).
    LockApp,
    /// Show Log Out confirmation box.
    Logout,
    /// Open About Rustgram modal box.
    About,
    /// Restart to install a staged update (`Update Rustgram` footer row).
    RestartToUpdate,
    /// Dismiss the menu drawer.
    Close,
}

/// Returns `(title, subtitle)` for the highlighted main-menu update row when a verified update is
/// staged and ready to install, or `None` otherwise.
#[must_use]
pub fn main_menu_update_row_text(update_snapshot: &UpdateSnapshot) -> Option<(String, String)> {
    let version = update_snapshot.ready_version()?;
    let title = tg_lang::tr::lng_update_telegram().replace("Telegram", "Rustgram");
    let subtitle = format!("{} · v{version}", tg_lang::tr::lng_settings_update_ready());
    Some((title, subtitle))
}

/// Stateful slide-out main menu drawer (`Window::MainMenu`).
#[derive(Debug, Default)]
pub struct MainMenu {
    accounts_expanded: bool,
    last_update_row_rect: Option<Rect>,
    live_mode: bool,
}

impl MainMenu {
    /// Configures whether the drawer renders live-session copy rather than demo/mock copy.
    pub fn set_live_mode(&mut self, live_mode: bool) {
        self.live_mode = live_mode;
    }

    /// Returns whether the accounts switcher section is currently expanded.
    #[must_use]
    pub const fn accounts_expanded(&self) -> bool {
        self.accounts_expanded
    }

    /// Explicitly expands or collapses the accounts switcher section.
    pub fn set_accounts_expanded(&mut self, expanded: bool) {
        self.accounts_expanded = expanded;
    }

    /// Returns the screen rectangle of the rendered "Update Rustgram" footer row from the last frame,
    /// if an update was ready.
    #[must_use]
    pub const fn last_update_row_rect(&self) -> Option<Rect> {
        self.last_update_row_rect
    }

    /// Renders the main menu overlay drawer + dimmed backdrop (single-account compatibility wrapper).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        user: &AuthUser,
        theme_mode: ThemeMode,
        has_passcode: bool,
        update_snapshot: &UpdateSnapshot,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<MainMenuAction> {
        let single = [MainMenuAccountItem::from_active_user(user, 0)];
        self.show_with_accounts(
            ctx,
            user,
            &single,
            self.live_mode,
            3,
            theme_mode,
            has_passcode,
            update_snapshot,
            palette,
            strings,
        )
    }

    /// Renders the main menu overlay drawer + dimmed backdrop with the full multi-account
    /// switcher list (avatar, name, unread badge, active ring) and `"Add Account"` button.
    #[allow(clippy::too_many_arguments)]
    pub fn show_with_accounts(
        &mut self,
        ctx: &egui::Context,
        user: &AuthUser,
        accounts: &[MainMenuAccountItem],
        can_add_account: bool,
        max_accounts: usize,
        theme_mode: ThemeMode,
        has_passcode: bool,
        update_snapshot: &UpdateSnapshot,
        palette: &Palette,
        strings: &dyn Strings,
    ) -> Option<MainMenuAction> {
        let mut action = None;
        self.last_update_row_rect = None;
        let screen = ctx.content_rect();
        let drawer_width = MAIN_MENU_WIDTH.min(screen.width() * 0.85);
        let update_row = main_menu_update_row_text(update_snapshot);
        let footer_reserve = if update_row.is_some() { 118.0 } else { 64.0 };
        let menu_scroll_max_h =
            (screen.height() - MAIN_MENU_COVER_HEIGHT - footer_reserve).max(80.0);

        // Sum unmuted unread across OTHER (inactive) accounts for the collapsed cover badge
        // (`MainMenu::ToggleAccountsButton::computeUnreadBadge` in `window_main_menu.cpp:257`).
        let other_accounts_unmuted_unread: u32 = accounts
            .iter()
            .filter(|a| !a.is_active && !a.unread_muted)
            .fold(0u32, |acc, item| acc.saturating_add(item.unread_count));

        // Backdrop scrim covering everything to the right of the drawer.
        Area::new(Id::unique("tg_main_menu_backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter().rect_filled(rect, 0.0, palette.layer_backdrop);
                if resp.clicked() {
                    action = Some(MainMenuAction::Close);
                }
            });

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            action = Some(MainMenuAction::Close);
        }

        // Left drawer panel.
        Area::new(Id::unique("tg_main_menu_drawer"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(drawer_width, screen.height()));
                        ui.set_max_size(Vec2::new(drawer_width, screen.height()));

                        // Top profile cover (height = 134px, `st::mainMenuCoverHeight`).
                        let (cover_rect, cover_resp) = ui.allocate_exact_size(
                            Vec2::new(drawer_width, MAIN_MENU_COVER_HEIGHT),
                            Sense::click(),
                        );
                        cover_resp.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::Button,
                                true,
                                format!("{} ({})", user.name, user.phone),
                            )
                        });
                        paint_cover(
                            ui,
                            cover_rect,
                            user,
                            self.accounts_expanded,
                            other_accounts_unmuted_unread,
                            palette,
                        );
                        if cover_resp.clicked() {
                            self.accounts_expanded = !self.accounts_expanded;
                        }

                        // Divider line below cover.
                        ui.painter().line_segment(
                            [
                                Pos2::new(cover_rect.left(), cover_rect.bottom()),
                                Pos2::new(cover_rect.right(), cover_rect.bottom()),
                            ],
                            Stroke::new(1.0, palette.divider),
                        );

                        // Scrollable accounts + menu list (`MainMenu::_scroll`) so the footer
                        // and update banner remain pinned and visible even on shorter windows.
                        egui::ScrollArea::vertical()
                            .id_salt("tg_main_menu_items_scroll")
                            .max_height(menu_scroll_max_h)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                // Expandable accounts switcher (`MainMenu::setupAccounts` / `Settings::SetupAccounts`).
                                if self.accounts_expanded {
                                    ui.add_space(4.0);
                                    for acc in accounts {
                                        if account_row(ui, drawer_width, acc, palette) {
                                            if acc.is_active {
                                                action = Some(MainMenuAction::Close);
                                            } else {
                                                action = Some(MainMenuAction::SwitchAccount(
                                                    acc.slot_index,
                                                ));
                                            }
                                        }
                                    }

                                    let add_label = tg_lang::tr::lng_menu_add_account();
                                    if add_account_row(
                                        ui,
                                        drawer_width,
                                        &add_label,
                                        can_add_account,
                                        max_accounts,
                                        palette,
                                    ) {
                                        action = Some(MainMenuAction::AddAccount);
                                    }
                                    ui.add_space(4.0);
                                    ui.add(egui::Separator::default().spacing(4.0));
                                }

                                // Menu actions matching `MainMenu::setupMenu` order.
                                ui.add_space(6.0);
                                if menu_row(
                                    ui,
                                    drawer_width,
                                    "MP",
                                    strings.menu_my_profile(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::MyProfile);
                                }
                                ui.add(egui::Separator::default().spacing(8.0));

                                if menu_row(
                                    ui,
                                    drawer_width,
                                    "NG",
                                    strings.menu_new_group(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::NewGroup);
                                }
                                if menu_row(
                                    ui,
                                    drawer_width,
                                    "NC",
                                    strings.menu_new_channel(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::NewChannel);
                                }
                                if menu_row(
                                    ui,
                                    drawer_width,
                                    "CT",
                                    strings.menu_contacts(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::Contacts);
                                }
                                if menu_row(ui, drawer_width, "CL", strings.menu_calls(), palette) {
                                    action = Some(MainMenuAction::Calls);
                                }
                                if menu_row(
                                    ui,
                                    drawer_width,
                                    "SM",
                                    strings.menu_saved_messages(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::SavedMessages);
                                }
                                if menu_row(ui, drawer_width, "DL", "Downloads", palette) {
                                    action = Some(MainMenuAction::Downloads);
                                }
                                if menu_row(
                                    ui,
                                    drawer_width,
                                    "ST",
                                    strings.menu_settings(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::Settings);
                                }

                                // Night Mode toggle row.
                                let night_label = if theme_mode.is_night() {
                                    format!("{}  [ON]", strings.menu_night_mode())
                                } else {
                                    format!("{}  [OFF]", strings.menu_night_mode())
                                };
                                if menu_row(ui, drawer_width, "NM", &night_label, palette) {
                                    action = Some(MainMenuAction::ToggleNightMode);
                                }

                                let lock_label = if has_passcode {
                                    strings.menu_lock().to_owned()
                                } else {
                                    format!("{} (Set...)", strings.menu_lock())
                                };
                                if menu_row(ui, drawer_width, "LK", &lock_label, palette) {
                                    action = Some(MainMenuAction::LockApp);
                                }

                                ui.add(egui::Separator::default().spacing(8.0));
                                if menu_row_tinted(
                                    ui,
                                    drawer_width,
                                    "LO",
                                    strings.menu_logout(),
                                    palette.attention_fg,
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::Logout);
                                }
                                if tg_app_settings::wallet::AppWalletHost::render_main_menu_entry(
                                    ui,
                                    drawer_width,
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::Close);
                                }
                            });

                        // Footer anchored at the bottom (`MainMenu::_footer`).
                        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                            let version_text = tg_lang::tr::lng_settings_current_version(
                                &update_snapshot.current_version.to_string(),
                            );
                            let footer_resp = Frame::NONE
                                .inner_margin(Margin::symmetric(20, 14))
                                .show(ui, |ui| {
                                    let r1 = ui.add(
                                        egui::Label::new(
                                            RichText::new(version_text)
                                                .size(11.5)
                                                .color(palette.window_sub_text_fg),
                                        )
                                        .sense(Sense::click()),
                                    );
                                    let r2 = ui.add(
                                        egui::Label::new(
                                            RichText::new("Rustgram Desktop")
                                                .size(12.5)
                                                .strong()
                                                .color(palette.window_sub_text_fg),
                                        )
                                        .sense(Sense::click()),
                                    );
                                    if r1.hovered() || r2.hovered() {
                                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    }
                                    r1.clicked() || r2.clicked()
                                })
                                .inner;
                            if footer_resp {
                                action = Some(MainMenuAction::About);
                            }

                            // Highlighted "Update Rustgram" row directly above the version footer
                            // when a verified update is staged and ready (`window_main_menu.cpp`).
                            if let Some((title, subtitle)) = &update_row {
                                let (rect, clicked) =
                                    update_ready_row(ui, drawer_width, title, subtitle, palette);
                                self.last_update_row_rect = Some(rect);
                                if clicked {
                                    action = Some(MainMenuAction::RestartToUpdate);
                                }
                            }
                        });
                    });
            });

        action
    }
}

fn paint_cover(
    ui: &Ui,
    rect: Rect,
    user: &AuthUser,
    accounts_expanded: bool,
    other_accounts_unmuted_unread: u32,
    palette: &Palette,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, palette.window_bg);

    // 48px circular avatar at (21, 20), matching `st::mainMenuUserpic`.
    let center = Pos2::new(rect.left() + 45.0, rect.top() + 44.0);
    let [r, g, b] = user.avatar_rgb;
    painter.circle_filled(center, 24.0, Color32::from_rgb(r, g, b));
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        &user.initials,
        egui::FontId::proportional(16.0),
        Color32::WHITE,
    );

    painter.text(
        Pos2::new(rect.left() + 21.0, rect.top() + 86.0),
        egui::Align2::LEFT_TOP,
        &user.name,
        egui::FontId::proportional(15.0),
        palette.window_bold_fg,
    );
    painter.text(
        Pos2::new(rect.left() + 21.0, rect.top() + 107.0),
        egui::Align2::LEFT_TOP,
        &user.phone,
        egui::FontId::proportional(12.5),
        palette.window_sub_text_fg,
    );

    if !accounts_expanded && other_accounts_unmuted_unread > 0 {
        paint_unread_pill(
            painter,
            Pos2::new(rect.right() - 40.0, rect.top() + 98.0),
            other_accounts_unmuted_unread,
            false,
            palette,
        );
    }

    let chevron = if accounts_expanded { "^" } else { "v" };
    painter.text(
        Pos2::new(rect.right() - 22.0, rect.top() + 98.0),
        egui::Align2::CENTER_CENTER,
        chevron,
        egui::FontId::proportional(13.0),
        palette.window_sub_text_fg,
    );
}

fn format_unread_badge_short(count: u32) -> String {
    if count >= 10_000 {
        format!("{}K", count / 1000)
    } else {
        count.to_string()
    }
}

fn paint_unread_pill(
    painter: &egui::Painter,
    right_center: Pos2,
    count: u32,
    muted: bool,
    palette: &Palette,
) {
    let text = format_unread_badge_short(count);
    let pill_h = 20.0;
    let pill_w = (text.len() as f32 * 7.5 + 12.0).max(pill_h);
    let pill_rect = Rect::from_min_size(
        Pos2::new(right_center.x - pill_w, right_center.y - pill_h * 0.5),
        Vec2::new(pill_w, pill_h),
    );
    let bg = if muted {
        palette.unread_badge_muted_bg
    } else {
        palette.unread_badge_bg
    };
    painter.rect_filled(pill_rect, pill_h * 0.5, bg);
    painter.text(
        pill_rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(11.0),
        Color32::WHITE,
    );
}

fn account_row(ui: &mut Ui, width: f32, acc: &MainMenuAccountItem, palette: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
    let accessible_label = if acc.unread_count > 0 {
        format!("{} ({} unread)", acc.name, acc.unread_count)
    } else {
        acc.name.clone()
    };
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, accessible_label.clone())
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
    }

    let avatar_center = Pos2::new(rect.left() + 31.0, rect.center().y);
    let [r, g, b] = acc.avatar_rgb;
    ui.painter()
        .circle_filled(avatar_center, 12.0, Color32::from_rgb(r, g, b));
    if acc.is_active {
        ui.painter().circle_stroke(
            avatar_center,
            15.0,
            Stroke::new(2.0, palette.window_bg_active),
        );
    }
    ui.painter().text(
        avatar_center,
        egui::Align2::CENTER_CENTER,
        &acc.initials,
        egui::FontId::proportional(10.0),
        Color32::WHITE,
    );

    let name_color = if acc.is_active {
        palette.window_bold_fg
    } else {
        palette.window_fg
    };
    ui.painter().text(
        Pos2::new(rect.left() + 58.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &acc.name,
        egui::FontId::proportional(13.5),
        name_color,
    );

    if acc.unread_count > 0 {
        paint_unread_pill(
            ui.painter(),
            Pos2::new(rect.right() - 16.0, rect.center().y),
            acc.unread_count,
            acc.unread_muted,
            palette,
        );
    }

    resp.clicked()
}

fn add_account_row(
    ui: &mut Ui,
    width: f32,
    label: &str,
    can_add_account: bool,
    max_accounts: usize,
    palette: &Palette,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
    }
    let badge_center = Pos2::new(rect.left() + 31.0, rect.center().y);
    let badge_bg = if can_add_account {
        palette.window_bg_active
    } else {
        palette.input_bg
    };
    let plus_fg = if can_add_account {
        palette.window_fg_active
    } else {
        palette.window_sub_text_fg
    };
    ui.painter().circle_filled(badge_center, 12.0, badge_bg);
    ui.painter().text(
        badge_center,
        egui::Align2::CENTER_CENTER,
        "+",
        egui::FontId::proportional(15.0),
        plus_fg,
    );
    let text_color = if can_add_account {
        palette.window_bold_fg
    } else {
        palette.window_sub_text_fg
    };
    ui.painter().text(
        Pos2::new(rect.left() + 58.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.5),
        text_color,
    );
    if !can_add_account {
        ui.painter().text(
            Pos2::new(rect.right() - 16.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("max {max_accounts}"),
            egui::FontId::proportional(11.0),
            palette.window_sub_text_fg,
        );
    }
    resp.clicked()
}

fn menu_row(ui: &mut Ui, width: f32, badge: &str, label: &str, palette: &Palette) -> bool {
    menu_row_tinted(ui, width, badge, label, palette.window_bold_fg, palette)
}

fn menu_row_tinted(
    ui: &mut Ui,
    width: f32,
    badge: &str,
    label: &str,
    text_color: Color32,
    palette: &Palette,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
    }
    let badge_center = Pos2::new(rect.left() + 31.0, rect.center().y);
    ui.painter()
        .circle_filled(badge_center, 11.0, palette.input_bg);
    ui.painter().text(
        badge_center,
        egui::Align2::CENTER_CENTER,
        badge,
        egui::FontId::proportional(9.5),
        palette.window_sub_text_fg,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 58.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(14.0),
        text_color,
    );
    resp.clicked()
}

/// Paints tdesktop's highlighted accent "Update Rustgram" banner row anchored above the
/// main menu version footer when a verified update is ready to install.
fn update_ready_row(
    ui: &mut Ui,
    width: f32,
    title: &str,
    subtitle: &str,
    palette: &Palette,
) -> (Rect, bool) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 52.0), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{title} — {subtitle}"))
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if resp.hovered() {
        Color32::from_rgb(
            palette.window_bg_active.r().saturating_add(12),
            palette.window_bg_active.g().saturating_add(12),
            palette.window_bg_active.b().saturating_add(12),
        )
    } else {
        palette.window_bg_active
    };
    ui.painter().rect_filled(rect, 0.0, fill);

    let badge_center = Pos2::new(rect.left() + 31.0, rect.center().y);
    let badge_bg = Color32::from_rgba_unmultiplied(
        palette.window_fg_active.r(),
        palette.window_fg_active.g(),
        palette.window_fg_active.b(),
        48,
    );
    ui.painter().circle_filled(badge_center, 13.0, badge_bg);
    tg_app_core::ui_icons::paint_arrow(
        ui.painter(),
        badge_center,
        14.0,
        tg_app_core::ui_icons::ArrowDir::Up,
        palette.window_fg_active,
    );

    ui.painter().text(
        Pos2::new(rect.left() + 56.0, rect.top() + 11.0),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::proportional(14.0),
        palette.window_fg_active,
    );
    let sub_color = Color32::from_rgba_unmultiplied(
        palette.window_fg_active.r(),
        palette.window_fg_active.g(),
        palette.window_fg_active.b(),
        215,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 56.0, rect.top() + 30.0),
        egui::Align2::LEFT_TOP,
        subtitle,
        egui::FontId::proportional(11.5),
        sub_color,
    );
    (rect, resp.clicked())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tg_updater::version::Version;

    use super::*;
    use tg_app_core::core_types::startup_types::UpdateState;
    use tg_app_core::stand_in::strings::EnglishStrings;
    use tg_app_core::stand_in::types::PeerId;

    fn sample_user() -> AuthUser {
        AuthUser {
            id: PeerId::from_user(1),
            name: "Taylor Kim".to_owned(),
            username: "taylor_kim".to_owned(),
            phone: "+1 555 010 0199".to_owned(),
            bio: String::new(),
            initials: "TK".to_owned(),
            avatar_rgb: [0x40, 0xA7, 0xE3],
        }
    }

    #[test]
    fn update_row_text_shown_only_when_update_is_ready() {
        let idle = UpdateSnapshot::default();
        assert!(main_menu_update_row_text(&idle).is_none());

        let checking = UpdateSnapshot {
            state: UpdateState::Checking,
            ..UpdateSnapshot::default()
        };
        assert!(main_menu_update_row_text(&checking).is_none());

        let ready = UpdateSnapshot {
            state: UpdateState::Ready {
                version: Version::new(0, 2, 0),
                staging_dir: PathBuf::from("tupdates/temp"),
                updater_promoted: true,
            },
            ..UpdateSnapshot::default()
        };
        let (title, subtitle) = main_menu_update_row_text(&ready).expect("ready row text");
        assert!(title.contains("Rustgram"), "unexpected title: {title}");
        assert!(
            subtitle.contains("0.2.0"),
            "unexpected subtitle: {subtitle}"
        );
        assert!(
            subtitle.contains(&tg_lang::tr::lng_settings_update_ready()),
            "missing tg_lang ready phrase: {subtitle}"
        );
    }

    #[test]
    fn clicking_ready_update_row_emits_restart_to_update() {
        let ctx = egui::Context::default();
        let user = sample_user();
        let palette = Palette::day();
        let strings = EnglishStrings;
        let ready = UpdateSnapshot {
            state: UpdateState::Ready {
                version: Version::new(0, 2, 0),
                staging_dir: PathBuf::from("tupdates/temp"),
                updater_promoted: true,
            },
            ..UpdateSnapshot::default()
        };
        let mut menu = MainMenu::default();

        // Two layout passes let the ScrollArea and Foreground Area settle their exact rects.
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            ..Default::default()
        };
        for _ in 0..2 {
            let mut out = ctx.run_ui(raw.clone(), |ui| {
                let act = menu.show(
                    ui.ctx(),
                    &user,
                    ThemeMode::Day,
                    false,
                    &ready,
                    &palette,
                    &strings,
                );
                assert_eq!(act, None);
            });
            out.textures_delta.clear();
        }

        // Click inside the highlighted "Update Rustgram" banner returned by the settled layout pass.
        let update_rect = menu
            .last_update_row_rect()
            .expect("update row rendered when UpdateState::Ready");
        assert!(update_rect.bottom() <= 600.0);
        let click_pos = update_rect.center();
        raw.events = vec![
            egui::Event::PointerMoved(click_pos),
            egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let mut out = ctx.run_ui(raw.clone(), |ui| {
            let _ = menu.show(
                ui.ctx(),
                &user,
                ThemeMode::Day,
                false,
                &ready,
                &palette,
                &strings,
            );
        });
        out.textures_delta.clear();

        raw.events = vec![
            egui::Event::PointerMoved(click_pos),
            egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let mut emitted = None;
        let mut out = ctx.run_ui(raw, |ui| {
            emitted = menu.show(
                ui.ctx(),
                &user,
                ThemeMode::Day,
                false,
                &ready,
                &palette,
                &strings,
            );
        });
        out.textures_delta.clear();
        assert_eq!(emitted, Some(MainMenuAction::RestartToUpdate));
    }
}
