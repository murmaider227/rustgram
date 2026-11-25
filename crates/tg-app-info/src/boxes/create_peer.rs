//! Create Group and Create Channel multi-step wizard modal boxes.
//!
//! Port origin: `Telegram/SourceFiles/boxes/add_contact_box.cpp`
//! (`GroupInfoBox`, `SetupChannelBox`) and `Telegram/SourceFiles/boxes/peers/add_participants_box.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2,
};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::PopupMenuStyle;
use tg_ui::stand_in::{
    CheckboxStyle, Colors, FlatLabelStyle, InputFieldStyle, RadioStyle, ToggleStyle,
};
use tg_ui::style::px;
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;
use tg_ui::userpic::EmptyUserpic;
use tg_ui::widgets::checkbox::{Radio, Toggle};
use tg_ui::widgets::input_field::InputField;
use tg_ui::widgets::labels::FlatLabel;

use crate::boxes::peer_list::{PeerListBoxState, PeerListPurpose, PeerSelectionMode};
use crate::boxes::strings;
use crate::dialogs::entry_menu::{EntryMenu, EntryMenuRow};
use crate::info::backend::{CreateChannelRequest, CreateGroupRequest, InfoBackend, PeerProfile};
use crate::info::values::{
    AUTO_DELETE_TTL_PRESETS, MAX_CHANNEL_DESCRIPTION, MAX_GROUP_CHANNEL_TITLE, MAX_USERNAME_LENGTH,
    UsernameAvailability, format_auto_delete_ttl, format_username_link,
};
use tg_app_core::stand_in::types::PeerId;

/// Active step in the Create Group wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateGroupStep {
    /// Step 1: Group name, avatar photo, optional auto-delete timer.
    Details,
    /// Step 2: Select initial members from contacts.
    AddMembers,
}

/// Action returned from [`CreateGroupBoxState::show_body`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatePeerBodyAction {
    /// Clicked the avatar circle to open the Photo Crop box.
    OpenPhotoCrop,
}

/// Stateful 2-step wizard for creating a new Telegram group (`GroupInfoBox` -> `AddParticipantsBox`).
#[derive(Debug, Clone)]
pub struct CreateGroupBoxState {
    /// Current wizard step.
    pub step: CreateGroupStep,
    /// Group name (`1..=128` chars).
    pub name: String,
    /// Whether a custom photo has been chosen/cropped.
    pub has_custom_photo: bool,
    /// Selected index into [`AUTO_DELETE_TTL_PRESETS`] (`0` = Off).
    pub auto_delete_preset_idx: usize,
    /// Member picker state used in Step 2.
    pub member_picker: PeerListBoxState,
    /// Validation error text.
    pub error: Option<String>,
    /// Top-right `⋮` menu with the Auto-Delete timer (`GroupInfoBox::prepare`).
    pub ttl_menu: EntryMenu,
}

impl Default for CreateGroupBoxState {
    fn default() -> Self {
        Self::new()
    }
}

impl CreateGroupBoxState {
    /// Creates a fresh Create Group wizard starting at Step 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            step: CreateGroupStep::Details,
            name: String::new(),
            has_custom_photo: false,
            auto_delete_preset_idx: 0,
            member_picker: PeerListBoxState::new(
                tr::lng_profile_add_participant(),
                tr::lng_create_group_create(),
                PeerSelectionMode::Multi,
                PeerListPurpose::Generic,
            ),
            error: None,
            ttl_menu: EntryMenu::default(),
        }
    }

    /// Validates Step 1 and advances to Step 2 (`AddMembers`).
    pub fn advance_to_members(&mut self) -> Result<(), String> {
        let trimmed = self.name.trim();
        if trimmed.is_empty() {
            let msg = strings::GROUP_NAME_REQUIRED.to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        if trimmed.chars().count() > MAX_GROUP_CHANNEL_TITLE {
            let msg = format!("Group name must be at most {MAX_GROUP_CHANNEL_TITLE} characters.");
            self.error = Some(msg.clone());
            return Err(msg);
        }
        self.error = None;
        self.step = CreateGroupStep::AddMembers;
        Ok(())
    }

    /// Steps back from Step 2 to Step 1.
    pub fn back_to_details(&mut self) {
        self.step = CreateGroupStep::Details;
        self.error = None;
    }

    /// Submits the group creation request to `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<PeerId, String> {
        let ttl = AUTO_DELETE_TTL_PRESETS
            .get(self.auto_delete_preset_idx)
            .copied()
            .unwrap_or(0);
        let req = CreateGroupRequest {
            title: self.name.trim().to_owned(),
            member_ids: self.member_picker.selected.clone(),
            auto_delete_ttl_seconds: ttl,
            has_custom_photo: self.has_custom_photo,
        };
        match backend.create_group(req) {
            Ok(id) => {
                self.error = None;
                Ok(id)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor for the active step.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        match self.step {
            CreateGroupStep::Details => {
                // `GroupInfoBox` has no title; its only top button is the `⋮` TTL menu.
                let mut modal = BoxModal::new(id, String::new())
                    .width(st::box_wide_width.0 as f32)
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::default_btn("next", tr::lng_create_group_next()));
                modal.show_close_button = false;
                modal
            }
            CreateGroupStep::AddMembers => {
                let count = self.member_picker.selected.len();
                let create_base = tr::lng_create_group_create();
                let create_label = if count > 0 {
                    format!("{create_base} ({count})")
                } else {
                    create_base
                };
                BoxModal::new(id, tr::lng_profile_add_participant())
                    .additional_title(format!(
                        "{} · {count} {}",
                        strings::STEP_2_OF_2,
                        strings::SELECTED_SUFFIX
                    ))
                    .width(st::box_wide_width.0 as f32 + 24.0)
                    .with_left_button(BoxButton::default_btn("back", tr::lng_create_group_back()))
                    .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                    .add_button(BoxButton::default_btn("create", create_label))
            }
        }
    }

    /// Renders the active wizard step inside `ui`.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        contacts: &[PeerProfile],
        colors: &dyn Colors,
    ) -> Option<CreatePeerBodyAction> {
        match self.step {
            CreateGroupStep::Details => {
                let mut act = None;
                let input_st = InputFieldStyle::from_tg(&st::default_input_field, colors);
                let fallback_title = tr::lng_create_group_title();
                let top = ui.cursor().min;
                let width = ui.available_width();

                ui.horizontal(|ui| {
                    if photo_picker_circle(
                        ui,
                        if self.name.trim().is_empty() {
                            fallback_title.as_str()
                        } else {
                            self.name.trim()
                        },
                        self.has_custom_photo,
                        0,
                        colors,
                    ) {
                        act = Some(CreatePeerBodyAction::OpenPhotoCrop);
                    }
                    ui.add_space(px(st::new_group_name_position.x));
                    ui.vertical(|ui| {
                        ui.add_space(px(st::new_group_name_position.y));
                        InputField::single_line(
                            "create_group_name",
                            &mut self.name,
                            &tr::lng_dlg_new_group_name(),
                            input_st,
                        )
                        .error(self.error.is_some())
                        .width((ui.available_width() - 34.0).max(120.0))
                        .show(ui);
                        // `setMaxLength(kMaxGroupChannelTitle)` without a visible counter.
                        if self.name.chars().count() > MAX_GROUP_CHANNEL_TITLE {
                            self.name = self.name.chars().take(MAX_GROUP_CHANNEL_TITLE).collect();
                        }
                    });
                });

                // `⋮` -> "Auto-Delete" submenu (`st::infoTopBarMenu` top button).
                let dots = Rect::from_center_size(
                    Pos2::new(top.x + width - 12.0, top.y + 6.0),
                    Vec2::splat(32.0),
                );
                let dots_resp = ui.interact(
                    dots,
                    ui.make_persistent_id("create_group_ttl"),
                    Sense::click(),
                );
                dots_resp.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::Role::Button,
                        true,
                        tr::lng_manage_messages_ttl_menu(),
                    )
                });
                if dots_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let resolver = ColorsResolver(colors);
                paint_style_icon(
                    ui.painter(),
                    dots,
                    st::info_top_bar_menu.icon,
                    &resolver,
                    Some(colors.color("boxTitleCloseFg")),
                );
                if dots_resp.clicked() {
                    self.ttl_menu
                        .open_at(ui.ctx(), Pos2::new(dots.max.x - 180.0, dots.max.y));
                }
                let presets = [0usize, 1, 7, 10];
                let options = presets
                    .iter()
                    .map(|&idx| {
                        let secs = AUTO_DELETE_TTL_PRESETS.get(idx).copied().unwrap_or(0);
                        let label = if secs == 0 {
                            tr::lng_manage_messages_ttl_disable()
                        } else {
                            format_auto_delete_ttl(secs)
                        };
                        EntryMenuRow::action(
                            format!("ttl:{idx}"),
                            label,
                            (self.auto_delete_preset_idx == idx)
                                .then_some(st::media_player_menu_check),
                        )
                    })
                    .collect();
                let ttl_secs = AUTO_DELETE_TTL_PRESETS
                    .get(self.auto_delete_preset_idx)
                    .copied()
                    .unwrap_or(0);
                let ttl_label = if ttl_secs == 0 {
                    tr::lng_manage_messages_ttl_menu()
                } else {
                    format!(
                        "{} \u{2022} {}",
                        tr::lng_manage_messages_ttl_menu(),
                        format_auto_delete_ttl(ttl_secs)
                    )
                };
                let rows = vec![EntryMenuRow::submenu(
                    ttl_label,
                    Some(st::menu_icon_ttl),
                    options,
                )];
                let menu_style = PopupMenuStyle::from_tg(&st::popup_menu_with_icons, colors);
                if let Some(picked) = self.ttl_menu.show(
                    ui.ctx(),
                    ui.make_persistent_id("create_group_ttl_menu"),
                    &rows,
                    &menu_style,
                    &resolver,
                ) && let Some(idx) = picked
                    .strip_prefix("ttl:")
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    self.auto_delete_preset_idx = idx;
                }

                if let Some(ref err) = self.error {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(err)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("boxTextFgError")),
                    );
                }
                act
            }
            CreateGroupStep::AddMembers => {
                self.member_picker
                    .show_body(ui, "create_group", contacts, colors);
                None
            }
        }
    }
}

/// Active step in the Create Channel wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateChannelStep {
    /// Step 1: Channel name, description, and avatar photo.
    Details,
    /// Step 2: Public vs Private channel type + live username availability check.
    ChannelType,
}

/// Channel visibility radio value (`EditPeerTypeBox::Privacy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelVisibility {
    /// Public channel reachable by `t.me/<username>`.
    #[default]
    Public,
    /// Private channel reachable only by invite link.
    Private,
}

/// Stateful 2-step wizard for creating a new broadcast channel (`GroupInfoBox` -> `SetupChannelBox`).
#[derive(Debug, Clone)]
pub struct CreateChannelBoxState {
    /// Current wizard step.
    pub step: CreateChannelStep,
    /// Channel title (`1..=128` chars).
    pub name: String,
    /// Optional channel description (`0..=255` chars).
    pub description: String,
    /// Whether a custom photo has been cropped.
    pub has_custom_photo: bool,
    /// Public or private channel.
    pub visibility: ChannelVisibility,
    /// Public handle without `@`.
    pub username: String,
    /// Cached username check status.
    pub username_status: UsernameAvailability,
    /// Restrict saving/forwarding content (`noforwards`).
    pub no_forwards: bool,
    /// Synthetic private invite link preview.
    pub invite_link_preview: String,
    /// Validation error text.
    pub error: Option<String>,
}

impl Default for CreateChannelBoxState {
    fn default() -> Self {
        Self::new()
    }
}

impl CreateChannelBoxState {
    /// Creates a new Create Channel wizard at Step 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            step: CreateChannelStep::Details,
            name: String::new(),
            description: String::new(),
            has_custom_photo: false,
            visibility: ChannelVisibility::Public,
            username: String::new(),
            username_status: UsernameAvailability::Empty,
            no_forwards: false,
            invite_link_preview: "https://t.me/+NewChannelPrivateLink".to_owned(),
            error: None,
        }
    }

    /// Validates Step 1 and moves to Step 2 (`ChannelType`).
    pub fn advance_to_type(&mut self) -> Result<(), String> {
        let trimmed = self.name.trim();
        if trimmed.is_empty() {
            let msg = strings::CHANNEL_NAME_REQUIRED.to_owned();
            self.error = Some(msg.clone());
            return Err(msg);
        }
        if trimmed.chars().count() > MAX_GROUP_CHANNEL_TITLE {
            let msg = format!("Channel name must be at most {MAX_GROUP_CHANNEL_TITLE} characters.");
            self.error = Some(msg.clone());
            return Err(msg);
        }
        self.error = None;
        self.step = CreateChannelStep::ChannelType;
        Ok(())
    }

    /// Returns from Step 2 back to Step 1.
    pub fn back_to_details(&mut self) {
        self.step = CreateChannelStep::Details;
        self.error = None;
    }

    /// Refreshes live username availability via `backend`.
    pub fn check_username(&mut self, backend: &dyn InfoBackend) {
        self.username_status = backend.check_username(&self.username, None);
    }

    /// Submits channel creation to `backend`.
    pub fn submit(&mut self, backend: &mut dyn InfoBackend) -> Result<PeerId, String> {
        let is_public = self.visibility == ChannelVisibility::Public;
        if is_public {
            self.check_username(backend);
            if !self.username_status.is_valid_public() {
                let msg = if self.username.trim().is_empty() {
                    strings::CHANNEL_ENTER_USERNAME_OR_PRIVATE.to_owned()
                } else {
                    self.username_status.message(&self.username)
                };
                self.error = Some(msg.clone());
                return Err(msg);
            }
        }
        let req = CreateChannelRequest {
            title: self.name.trim().to_owned(),
            description: self.description.trim().to_owned(),
            is_public,
            username: is_public.then(|| self.username.trim().to_owned()),
            no_forwards: self.no_forwards,
            has_custom_photo: self.has_custom_photo,
        };
        match backend.create_channel(req) {
            Ok(id) => {
                self.error = None;
                Ok(id)
            }
            Err(err) => {
                self.error = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        match self.step {
            CreateChannelStep::Details => BoxModal::new(id, tr::lng_create_channel_title())
                .additional_title(strings::STEP_1_OF_2)
                .width(st::box_wide_width.0 as f32 + 16.0)
                .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                .add_button(BoxButton::default_btn(
                    "next",
                    tr::lng_create_group_create(),
                )),
            CreateChannelStep::ChannelType => BoxModal::new(id, strings::CHANNEL_SETTINGS_TITLE)
                .additional_title(strings::STEP_2_OF_2)
                .width(st::box_wide_width.0 as f32 + 20.0)
                .with_left_button(BoxButton::default_btn("back", tr::lng_create_group_back()))
                .add_button(BoxButton::default_btn(
                    "cancel",
                    tr::lng_create_group_skip(),
                ))
                .add_button(BoxButton::default_btn("save", tr::lng_settings_save())),
        }
    }

    /// Renders the active Create Channel step.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        backend: &dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Option<CreatePeerBodyAction> {
        let input_st = InputFieldStyle::default_input(colors);
        match self.step {
            CreateChannelStep::Details => {
                let mut act = None;
                let fallback_title = tr::lng_create_channel_title();
                ui.horizontal(|ui| {
                    if photo_picker_circle(
                        ui,
                        if self.name.trim().is_empty() {
                            fallback_title.as_str()
                        } else {
                            self.name.trim()
                        },
                        self.has_custom_photo,
                        3,
                        colors,
                    ) {
                        act = Some(CreatePeerBodyAction::OpenPhotoCrop);
                    }
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        InputField::single_line(
                            "create_channel_name",
                            &mut self.name,
                            strings::CHANNEL_NAME_PH,
                            input_st,
                        )
                        .max_length(MAX_GROUP_CHANNEL_TITLE)
                        .error(self.error.is_some())
                        .show(ui);
                    });
                });

                ui.add_space(10.0);
                let desc_ph = tr::lng_create_group_description();
                InputField::multi_line(
                    "create_channel_desc",
                    &mut self.description,
                    &desc_ph,
                    input_st,
                )
                .max_length(MAX_CHANNEL_DESCRIPTION)
                .show(ui);

                if let Some(ref err) = self.error {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(err)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("boxTextFgError")),
                    );
                }
                act
            }
            CreateChannelStep::ChannelType => {
                let cb_st = CheckboxStyle::box_checkbox(colors);
                let rad_st = RadioStyle::default_radio(colors);

                Radio::new(
                    &mut self.visibility,
                    ChannelVisibility::Public,
                    strings::CHANNEL_PUBLIC_LABEL,
                    cb_st,
                    rad_st,
                )
                .show(ui);
                FlatLabel::plain(
                    strings::CHANNEL_PUBLIC_HINT,
                    FlatLabelStyle::sub_text(colors),
                )
                .id_salt("ch_pub_hint")
                .show(ui);

                ui.add_space(6.0);
                Radio::new(
                    &mut self.visibility,
                    ChannelVisibility::Private,
                    strings::CHANNEL_PRIVATE_LABEL,
                    cb_st,
                    rad_st,
                )
                .show(ui);
                FlatLabel::plain(
                    strings::CHANNEL_PRIVATE_HINT,
                    FlatLabelStyle::sub_text(colors),
                )
                .id_salt("ch_priv_hint")
                .show(ui);

                ui.add_space(12.0);
                match self.visibility {
                    ChannelVisibility::Public => {
                        let uname_err =
                            !self.username.is_empty() && !self.username_status.is_valid_public();
                        let resp = InputField::single_line(
                            "create_channel_username",
                            &mut self.username,
                            strings::CHANNEL_PUBLIC_LINK_PH,
                            input_st,
                        )
                        .max_length(MAX_USERNAME_LENGTH)
                        .error(uname_err)
                        .show(ui);
                        if resp.changed() || !self.username.is_empty() {
                            self.check_username(backend);
                        }
                        if self.username_status != UsernameAvailability::Empty {
                            let fg = if self.username_status.is_valid_public() {
                                colors.color("boxTextFgGood")
                            } else {
                                colors.color("boxTextFgError")
                            };
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(self.username_status.message(&self.username))
                                    .font(font_id_from_style(st::normal_font))
                                    .color(fg),
                            );
                            if self.username_status.is_valid_public() {
                                ui.label(
                                    egui::RichText::new(format_username_link(&self.username))
                                        .font(FontId::new(12.0, family_regular()))
                                        .color(colors.color("windowActiveTextFg")),
                                );
                            }
                        }
                    }
                    ChannelVisibility::Private => {
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 36.0),
                            Sense::hover(),
                        );
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            colors.color("windowBgOver"),
                        );
                        ui.painter().text(
                            Pos2::new(rect.min.x + 12.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            &self.invite_link_preview,
                            font_id_from_style(st::normal_font),
                            colors.color("windowActiveTextFg"),
                        );
                    }
                }

                ui.add_space(12.0);
                Toggle::new(&mut self.no_forwards, ToggleStyle::default_toggle(colors))
                    .with_label(strings::RESTRICT_SAVING_CONTENT, colors.color("windowFg"))
                    .show(ui);

                if let Some(ref err) = self.error {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(err)
                            .font(font_id_from_style(st::normal_font))
                            .color(colors.color("boxTextFgError")),
                    );
                }
                None
            }
        }
    }
}

/// Renders a clickable 66px circular photo picker button used by Create Group/Channel & Edit Peer,
/// backed by `tg_style::st::menu_icon_photo_set`.
pub fn photo_picker_circle(
    ui: &mut Ui,
    title: &str,
    has_photo: bool,
    color_index: u8,
    colors: &dyn Colors,
) -> bool {
    // `st::defaultUserpicButton` (72 px, `defaultChangeUserpicIcon` on `activeButtonBg`).
    let size = px(st::default_userpic_button.photo_size);
    let radius = size * 0.5;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if has_photo {
        EmptyUserpic::for_color_index(color_index, title, colors).paint_circle(ui.painter(), rect);
        ui.painter().circle_stroke(
            rect.center(),
            radius,
            Stroke::new(2.0, colors.color("windowBgActive")),
        );
    } else {
        ui.painter()
            .circle_filled(rect.center(), radius, colors.color("activeButtonBg"));
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(30.0));
        paint_style_icon(
            ui.painter(),
            icon_rect,
            st::default_userpic_button.change_icon,
            &ColorsResolver(colors),
            Some(colors.color("activeButtonFg")),
        );
    }
    if resp.hovered() {
        ui.painter().circle_filled(
            rect.center(),
            radius,
            Color32::from_rgba_unmultiplied(0, 0, 0, 50),
        );
    }
    resp.on_hover_text(strings::PHOTO_PICKER_TOOLTIP).clicked()
}
