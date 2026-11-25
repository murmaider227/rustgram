//! Slide-out hamburger main menu panel.
//!
//! Port origin: `Telegram/SourceFiles/window/window_main_menu.h`,
//! `Telegram/SourceFiles/window/window_main_menu.cpp`,
//! and `Telegram/SourceFiles/window/window_main_menu.style`.

use egui::{
    Align, Area, Color32, CursorIcon, Frame, Id, Key, Layout, Margin, Order, Pos2, Rect, RichText,
    Sense, Stroke, Ui, Vec2,
};
use tg_ui::fonts::font_id_from_style;
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::ToggleStyle;
use tg_ui::tg_style::{DefaultPaletteResolver, Icon, st};
use tg_ui::widgets::checkbox::paint_toggle_view;

use tg_app_core::core_types::startup_types::UpdateSnapshot;
use tg_app_core::stand_in::strings::Strings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{AuthUser, PeerId};

/// Main menu drawer width (`st::mainMenuWidth`).
pub const MAIN_MENU_WIDTH: f32 = 274.0;
/// Main menu top cover height (`st::mainMenuCoverHeight`).
pub const MAIN_MENU_COVER_HEIGHT: f32 = 134.0;
/// Main menu row height (`st::mainMenuButton`: 11 px + 20 px line + 9 px).
pub const MAIN_MENU_ROW_HEIGHT: f32 = 40.0;
/// Vertical gap around separators and below the cover (`st::mainMenuSkip`).
pub const MAIN_MENU_SKIP: f32 = 6.0;
/// Row icon left offset (`st::mainMenuButton.iconLeft`).
const MAIN_MENU_ICON_LEFT: f32 = 21.0;
/// Row text left offset (`st::mainMenuButton.padding.left`).
const MAIN_MENU_TEXT_LEFT: f32 = 61.0;
/// Night Mode toggle right offset (`st::mainMenuButton.toggleSkip`).
const MAIN_MENU_TOGGLE_SKIP: f32 = 19.0;

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
    /// The cover's "Set Emoji Status" link (`MainMenu::_setEmojiStatus`).
    SetEmojiStatus,
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
    /// Open a side-menu bot's Mini App (`SetupMenuBots`, `WebViewSourceMainMenu`).
    OpenMenuBot(PeerId),
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
    body_rect: Option<Rect>,
    menu_bots: Vec<MainMenuBot>,
}

/// One attach-menu bot shown in the main menu (`AttachWebView::attachBots()` with `inMainMenu`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainMenuBot {
    /// The bot's peer.
    pub peer_id: PeerId,
    /// The attach-menu short name (`bot.name`).
    pub name: String,
}

/// The side-menu bots of `store` in server order, as `SetupMenuBots` filters them.
#[must_use]
pub fn main_menu_bots(store: &tg_bots::mini_app::AttachMenuStore) -> Vec<MainMenuBot> {
    store
        .bots
        .iter()
        .filter(|bot| bot.in_side_menu && !bot.inactive)
        .map(|bot| MainMenuBot {
            peer_id: PeerId(bot.bot_peer_id.0),
            name: bot.short_name.clone(),
        })
        .collect()
}

impl MainMenu {
    /// Sets the window body the drawer slides over: tdesktop's `MainMenu` is a child of the
    /// window body, so it starts below the title bar. `None` uses the whole viewport.
    pub fn set_body_rect(&mut self, body: Option<Rect>) {
        self.body_rect = body;
    }

    /// Replaces the attach-menu bots listed under "My Profile".
    pub fn set_menu_bots(&mut self, bots: Vec<MainMenuBot>) {
        self.menu_bots = bots;
    }

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
        let screen = self.body_rect.unwrap_or_else(|| ctx.content_rect());
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
                    .shadow(egui::Shadow {
                        offset: [0, 0],
                        blur: 8,
                        spread: 0,
                        color: Color32::from_black_alpha(40),
                    })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
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
                        let status_rect = paint_cover(
                            ui,
                            cover_rect,
                            user,
                            self.accounts_expanded,
                            other_accounts_unmuted_unread,
                            palette,
                        );
                        let status_resp = ui.interact(
                            status_rect,
                            ui.make_persistent_id("tg_main_menu_set_status"),
                            Sense::click(),
                        );
                        let status_label = tg_lang::tr::lng_menu_set_status();
                        status_resp.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Link, true, &status_label)
                        });
                        if status_resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if status_resp.clicked() {
                            action = Some(MainMenuAction::SetEmojiStatus);
                        } else if cover_resp.clicked() {
                            self.accounts_expanded = !self.accounts_expanded;
                        }

                        // Divider line below cover.
                        ui.painter().line_segment(
                            [
                                Pos2::new(cover_rect.left(), cover_rect.bottom() - 0.5),
                                Pos2::new(cover_rect.right(), cover_rect.bottom() - 0.5),
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
                                ui.spacing_mut().item_spacing.y = 0.0;
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
                                    menu_separator(ui, drawer_width, palette);
                                }

                                // Rows and order of `MainMenu::setupMenu` (7.4.6).
                                ui.add_space(MAIN_MENU_SKIP);
                                if menu_row(
                                    ui,
                                    drawer_width,
                                    st::menu_icon_profile,
                                    &tg_lang::tr::lng_menu_my_profile(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::MyProfile);
                                }
                                // `SetupMenuBots`: bots added to the side menu, with the
                                // `menuIconBot` glyph until their own icon is available.
                                for bot in &self.menu_bots {
                                    if menu_row(
                                        ui,
                                        drawer_width,
                                        st::menu_icon_bot,
                                        &bot.name,
                                        palette,
                                    ) {
                                        action = Some(MainMenuAction::OpenMenuBot(bot.peer_id));
                                    }
                                }
                                menu_separator(ui, drawer_width, palette);

                                let rows = [
                                    (
                                        st::menu_icon_groups,
                                        tg_lang::tr::lng_create_group_title(),
                                        MainMenuAction::NewGroup,
                                    ),
                                    (
                                        st::menu_icon_channel,
                                        tg_lang::tr::lng_create_channel_title(),
                                        MainMenuAction::NewChannel,
                                    ),
                                    (
                                        st::menu_icon_user_show,
                                        tg_lang::tr::lng_menu_contacts(),
                                        MainMenuAction::Contacts,
                                    ),
                                    (
                                        st::menu_icon_phone,
                                        tg_lang::tr::lng_menu_calls(),
                                        MainMenuAction::Calls,
                                    ),
                                    (
                                        st::menu_icon_saved_messages,
                                        tg_lang::tr::lng_saved_messages(),
                                        MainMenuAction::SavedMessages,
                                    ),
                                ];
                                for (icon, label, row_action) in rows {
                                    if menu_row(ui, drawer_width, icon, &label, palette) {
                                        action = Some(row_action);
                                    }
                                }
                                if tg_app_settings::wallet::AppWalletHost::is_available(ui.ctx())
                                    && menu_row(
                                        ui,
                                        drawer_width,
                                        st::wallet_menu_icon,
                                        &tg_lang::tr::lng_wallet_menu(),
                                        palette,
                                    )
                                {
                                    tg_app_settings::wallet::AppWalletHost::open(ui.ctx());
                                    action = Some(MainMenuAction::Close);
                                }
                                if menu_row(
                                    ui,
                                    drawer_width,
                                    st::menu_icon_settings,
                                    &tg_lang::tr::lng_menu_settings(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::Settings);
                                }
                                if night_mode_row(
                                    ui,
                                    drawer_width,
                                    &tg_lang::tr::lng_menu_night_mode(),
                                    theme_mode.is_night(),
                                    palette,
                                ) {
                                    action = Some(MainMenuAction::ToggleNightMode);
                                }
                                let _ = (has_passcode, strings);
                            });

                        // Footer anchored at the bottom (`MainMenu::_footer`).
                        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                            let version_text = format!(
                                "{} \u{2013} {}",
                                tg_lang::tr::lng_settings_current_version(
                                    &current_version_short_text(
                                        &update_snapshot.current_version.to_string(),
                                    )
                                ),
                                tg_lang::tr::lng_menu_about(),
                            );
                            let footer_resp = Frame::NONE
                                .inner_margin(Margin {
                                    left: 25,
                                    right: 20,
                                    top: 8,
                                    bottom: 12,
                                })
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    let r1 = ui.add(
                                        egui::Label::new(
                                            RichText::new(version_text)
                                                .font(font_id_from_style(st::normal_font))
                                                .color(palette.window_sub_text_fg),
                                        )
                                        .sense(Sense::click()),
                                    );
                                    let r2 = ui.add(
                                        egui::Label::new(
                                            RichText::new("Rustgram Desktop")
                                                .font(font_id_from_style(st::semibold_font))
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

/// Paints the 134 px cover (`MainMenu::paintEvent` + `setupUserpicButton`): 48 px userpic at
/// (24, 20), the name at (26, 84), the "Set Emoji Status" link at (26, 103) and the accounts
/// chevron on the right. Returns the clickable status link rect.
fn paint_cover(
    ui: &Ui,
    rect: Rect,
    user: &AuthUser,
    accounts_expanded: bool,
    other_accounts_unmuted_unread: u32,
    palette: &Palette,
) -> Rect {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, palette.window_bg);

    let center = Pos2::new(rect.left() + 24.0 + 24.0, rect.top() + 20.0 + 24.0);
    let [r, g, b] = user.avatar_rgb;
    painter.circle_filled(center, 24.0, Color32::from_rgb(r, g, b));
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        &user.initials,
        font_id_from_style(st::semibold_font),
        Color32::WHITE,
    );

    let semibold = font_id_from_style(st::semibold_font);
    let normal = font_id_from_style(st::normal_font);
    let text_left = rect.left() + 26.0;
    let chevron_x = rect.right() - 25.0;
    let name_max_w = (chevron_x - 16.0 - text_left).max(40.0);
    let name_galley = painter.layout(
        user.name.clone(),
        semibold,
        palette.window_bold_fg,
        name_max_w,
    );
    painter.galley(
        Pos2::new(text_left, rect.top() + 84.0),
        name_galley,
        palette.window_bold_fg,
    );

    let status = tg_lang::tr::lng_menu_set_status();
    let status_galley = painter.layout_no_wrap(status, normal, palette.window_active_text_fg);
    let status_rect = Rect::from_min_size(
        Pos2::new(text_left, rect.top() + 103.0),
        status_galley.size(),
    );
    painter.galley(
        status_rect.min,
        status_galley,
        palette.window_active_text_fg,
    );

    if !accounts_expanded && other_accounts_unmuted_unread > 0 {
        paint_unread_pill(
            painter,
            Pos2::new(chevron_x - 14.0, rect.top() + 102.0),
            other_accounts_unmuted_unread,
            false,
            palette,
        );
    }

    // `MainMenu::ToggleAccountsButton::paintEvent`: a thin chevron that flips when expanded.
    let c = Pos2::new(chevron_x, rect.top() + 102.0);
    let (dy_tip, dy_ends) = if accounts_expanded {
        (-2.5, 2.5)
    } else {
        (2.5, -2.5)
    };
    let stroke = Stroke::new(1.5, palette.window_sub_text_fg);
    painter.line_segment(
        [
            Pos2::new(c.x - 5.0, c.y + dy_ends),
            Pos2::new(c.x, c.y + dy_tip),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(c.x, c.y + dy_tip),
            Pos2::new(c.x + 5.0, c.y + dy_ends),
        ],
        stroke,
    );
    status_rect
}

/// `currentVersionShortText()`: the version followed by the build architecture.
fn current_version_short_text(version: &str) -> String {
    if cfg!(target_pointer_width = "64") {
        format!("{version} x64")
    } else {
        version.to_owned()
    }
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

/// One `st::mainMenuButton` row: a 24 px outline `menuIcon*` glyph tinted `menuIconFg` at
/// `iconLeft` and the semibold label at `padding.left`.
fn menu_row(ui: &mut Ui, width: f32, icon: Icon, label: &str, palette: &Palette) -> bool {
    let (rect, resp) = menu_row_base(ui, width, icon, label, palette);
    let _ = rect;
    resp.clicked()
}

fn menu_row_base(
    ui: &mut Ui,
    width: f32,
    icon: Icon,
    label: &str,
    palette: &Palette,
) -> (Rect, egui::Response) {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(width, MAIN_MENU_ROW_HEIGHT), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, palette.window_bg_over);
    }
    let icon_rect = Rect::from_min_size(
        Pos2::new(rect.left() + MAIN_MENU_ICON_LEFT, rect.center().y - 12.0),
        Vec2::splat(24.0),
    );
    paint_style_icon(
        ui.painter(),
        icon_rect,
        icon,
        &DefaultPaletteResolver,
        Some(palette.window_sub_text_fg),
    );
    ui.painter().text(
        Pos2::new(rect.left() + MAIN_MENU_TEXT_LEFT, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font_id_from_style(st::semibold_font),
        palette.window_bold_fg,
    );
    (rect, resp)
}

/// The Night Mode row: `menuIconNightMode` plus a `defaultMenuToggle` switch on the right whose
/// toggled color is `mainMenuCoverBg` (`st::mainMenu.itemToggle`).
fn night_mode_row(ui: &mut Ui, width: f32, label: &str, night: bool, palette: &Palette) -> bool {
    let (rect, resp) = menu_row_base(ui, width, st::menu_icon_night_mode, label, palette);
    let toggled = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("toggle"), night, 0.12);
    let mut style =
        ToggleStyle::from_tg(&st::default_menu_toggle, &tg_ui::theme::DayPalette::new());
    style.toggled_fg = palette.dialogs_bg_active;
    style.untoggled_fg = palette.window_sub_text_fg;
    style.toggled_bg = palette.window_bg;
    style.untoggled_bg = palette.window_bg;
    let size = Vec2::new(
        2.0 * style.border + style.diameter + style.width,
        2.0 * style.border + style.diameter,
    );
    let toggle_rect = Rect::from_min_size(
        Pos2::new(
            rect.right() - MAIN_MENU_TOGGLE_SKIP - size.x,
            rect.center().y - size.y * 0.5,
        ),
        size,
    );
    paint_toggle_view(ui.painter(), toggle_rect, &style, f64::from(toggled), 1.0);
    resp.clicked()
}

/// `Ui::PlainShadow` between menu groups with `st::mainMenuSkip` above and below.
fn menu_separator(ui: &mut Ui, width: f32, palette: &Palette) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(width, MAIN_MENU_SKIP * 2.0 + 1.0), Sense::hover());
    let y = rect.top() + MAIN_MENU_SKIP + 0.5;
    ui.painter().line_segment(
        [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
        Stroke::new(1.0, palette.divider),
    );
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
