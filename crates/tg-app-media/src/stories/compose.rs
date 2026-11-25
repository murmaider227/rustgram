//! Telegram Story Composer UI: [`SendStoryBox`], [`StoryPrivacyBox`], and [`StoryDraftsBox`].
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/send_story_box.{h,cpp}`
//! - `Telegram/SourceFiles/boxes/story_privacy_box.{h,cpp}`
//! - `Telegram/SourceFiles/boxes/story_drafts_box.{h,cpp}`
//! - `Telegram/SourceFiles/media/stories/media_stories_post.{h,cpp}`
//! - `Telegram/SourceFiles/media/stories/media_stories_discard_prompt.{h,cpp}`

use std::collections::BTreeSet;
use std::path::Path;

use egui::{
    Align2, Color32, ColorImage, CornerRadius, FontId, Frame, Id, Key, Margin, Pos2, Rect,
    RichText, ScrollArea, Sense, Stroke, TextEdit, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_data::peer::GeoPoint;
use tg_data::{FullStoryId, PeerId, ReactionId, TimeId};
use tg_stories::{
    Story, StoryArea, StoryLocation, StoryPrivacy, SuggestedReaction, UrlArea, WeatherArea,
};
use tg_text::{EntityInText, EntityType, TextWithEntities as UiText};

use crate::stories::backend::{
    DEFAULT_STORY_PERIOD_SECS, EditStoryRequest, PeerProfile, STORY_PERIOD_OPTIONS,
    SendStoryRequest, StoriesBackend, StoryDraft, StoryMediaAreasInput, StoryPrivacyInput,
    StoryUploadMedia,
};
use crate::stories::layout::format_story_timestamp;
use crate::stories::media_loader::decode_photo_via_tg_media;
use crate::stories::mock::{GradientPreset, generate_gradient_story_png};
use crate::stories::strip::paint_avatar_circle;

/// Standard story caption length limit for non-Premium accounts (`200` UTF-16 code units).
pub const STORY_CAPTION_LIMIT_DEFAULT: usize = 200;

/// Extended story caption length limit for Premium accounts (`2048` UTF-16 code units).
pub const STORY_CAPTION_LIMIT_PREMIUM: usize = 2048;

/// Quick reaction presets available when attaching a Reaction sticker area.
const COMPOSER_REACTION_PRESETS: &[&str] = &[
    "\u{2764}",
    "\u{1F525}",
    "\u{1F60D}",
    "\u{1F44F}",
    "\u{1F602}",
    "\u{1F389}",
];

/// Quick weather emoji presets for attaching a Weather sticker area.
const COMPOSER_WEATHER_PRESETS: &[(&str, u32)] = &[
    ("\u{2600}", 0xEB_FF_A7_26),
    ("\u{26C5}", 0xE6_1D_6F_A5),
    ("\u{1F327}", 0xE6_3B_5B_7A),
    ("\u{2744}", 0xE6_5C_A7_D6),
];

/// Formats a story lifetime period in seconds into a compact label (`6h`, `12h`, `24h`, `48h`).
#[must_use]
pub fn format_story_period(period_secs: i32) -> String {
    let hours = (period_secs / 3600).max(1);
    format!("{hours}h")
}

/// Returns the human-readable title of a [`StoryPrivacy`] audience mode.
#[must_use]
pub const fn privacy_mode_label(privacy: StoryPrivacy) -> &'static str {
    match privacy {
        StoryPrivacy::Public => "Everyone",
        StoryPrivacy::Contacts => "My Contacts",
        StoryPrivacy::CloseFriends => "Close Friends",
        StoryPrivacy::SelectedContacts | StoryPrivacy::Other => "Selected Contacts",
    }
}

/// Loads a photo (`png`/`jpg`/`jpeg`/`webp`) or video (`mp4`/`mov`/`webm`) file from disk into a
/// [`StoryUploadMedia`] payload without panicking on invalid or unreadable files.
pub fn load_story_media_from_path(path: &Path) -> Result<StoryUploadMedia, String> {
    let bytes =
        std::fs::read(path).map_err(|err| format!("Could not read {}: {err}", path.display()))?;
    if bytes.is_empty() {
        return Err("Selected media file is empty.".to_owned());
    }
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("story_media")
        .to_owned();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    match ext.as_str() {
        "mp4" | "mov" | "m4v" | "webm" => Ok(StoryUploadMedia::Video {
            bytes,
            file_name,
            width: 720,
            height: 1280,
            duration_secs: 5.0,
            has_audio: true,
            thumb_bytes: None,
        }),
        "jpg" | "jpeg" | "png" | "webp" | "bmp" => Ok(StoryUploadMedia::Photo { bytes, file_name }),
        _ => {
            // Try decoding as an image before rejecting unknown extensions.
            if decode_photo_via_tg_media(&bytes).is_ok() {
                Ok(StoryUploadMedia::Photo { bytes, file_name })
            } else {
                Err(format!(
                    "Unsupported story file format ({ext}). Pick a PNG/JPEG/WebP photo or MP4 video."
                ))
            }
        }
    }
}

/// Parses lightweight inline markdown markers (`**bold**`, `__underline__`, `_italic_`,
/// `~~strike~~`, `` `code` ``, `||spoiler||`) into a [`UiText`] with exact UTF-16 entity offsets
/// and merges any explicit `extra_entities` clamped to the resulting UTF-16 text length.
#[must_use]
pub fn parse_story_caption_entities(raw: &str, extra_entities: &[EntityInText]) -> UiText {
    let mut out_text = String::with_capacity(raw.len());
    let mut entities: Vec<EntityInText> = Vec::new();
    let mut rest = raw;

    const DELIMS: &[(&str, EntityType)] = &[
        ("**", EntityType::Bold),
        ("__", EntityType::Underline),
        ("~~", EntityType::StrikeOut),
        ("||", EntityType::Spoiler),
        ("`", EntityType::Code),
        ("_", EntityType::Italic),
    ];

    while !rest.is_empty() {
        let mut matched = false;
        for &(delim, entity_type) in DELIMS {
            if let Some(after_open) = rest.strip_prefix(delim)
                && let Some(close_idx) = after_open.find(delim)
                && close_idx > 0
            {
                let inner = &after_open[..close_idx];
                let start_utf16 = out_text.encode_utf16().count() as i32;
                let len_utf16 = inner.encode_utf16().count() as i32;
                out_text.push_str(inner);
                entities.push(EntityInText::new(entity_type, start_utf16, len_utf16));
                rest = &after_open[close_idx + delim.len()..];
                matched = true;
                break;
            }
        }
        if !matched {
            if let Some(ch) = rest.chars().next() {
                out_text.push(ch);
                rest = &rest[ch.len_utf8()..];
            } else {
                break;
            }
        }
    }

    let total_utf16 = out_text.encode_utf16().count() as i32;
    for ent in extra_entities {
        if ent.offset() >= 0 && ent.length() > 0 && ent.offset() < total_utf16 {
            let clamped_len = ent.length().min(total_utf16 - ent.offset());
            let candidate =
                EntityInText::with_data(ent.entity_type(), ent.offset(), clamped_len, ent.data());
            if !entities.contains(&candidate) {
                entities.push(candidate);
            }
        }
    }

    UiText {
        text: out_text,
        entities,
    }
}

/// Converts a `tg_data::TextWithEntities` back to a `tg_text::TextWithEntities` for editing.
#[must_use]
fn data_text_to_ui_text(src: &tg_data::TextWithEntities) -> UiText {
    let entities = src
        .entities
        .iter()
        .filter_map(|e| {
            let (entity_type, data) = match &e.kind {
                tg_data::EntityKind::Bold => (EntityType::Bold, String::new()),
                tg_data::EntityKind::Italic => (EntityType::Italic, String::new()),
                tg_data::EntityKind::Underline => (EntityType::Underline, String::new()),
                tg_data::EntityKind::StrikeOut => (EntityType::StrikeOut, String::new()),
                tg_data::EntityKind::Spoiler => (EntityType::Spoiler, String::new()),
                tg_data::EntityKind::Code => (EntityType::Code, String::new()),
                tg_data::EntityKind::Pre { language } => (EntityType::Pre, language.clone()),
                tg_data::EntityKind::TextUrl { url } => (EntityType::CustomUrl, url.clone()),
                tg_data::EntityKind::Url => (EntityType::Url, String::new()),
                tg_data::EntityKind::Mention => (EntityType::Mention, String::new()),
                tg_data::EntityKind::Hashtag => (EntityType::Hashtag, String::new()),
                tg_data::EntityKind::Blockquote { collapsed } => (
                    EntityType::Blockquote,
                    if *collapsed {
                        "1".to_owned()
                    } else {
                        String::new()
                    },
                ),
                _ => return None,
            };
            Some(EntityInText::with_data(
                entity_type,
                e.offset,
                e.length,
                data,
            ))
        })
        .collect();
    UiText {
        text: src.text.clone(),
        entities,
    }
}

// =============================================================================
// StoryPrivacyBox (`boxes/story_privacy_box.cpp` + `send_story_box.cpp`)
// =============================================================================

/// Which peer exception/inclusion list is currently being edited inside [`StoryPrivacyBox`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PrivacyPeerListKind {
    /// Users excluded from `Everyone`.
    ExcludedEveryone,
    /// Users excluded from `My Contacts`.
    ExcludedContacts,
    /// Explicitly allowed users for `Selected Contacts`.
    SelectedContacts,
    /// Account-wide Close Friends list (`contacts.editCloseFriends`).
    CloseFriends,
    /// Account-wide hidden-stories blocklist (`contacts.setBlocked(my_stories_from=true)`).
    StoriesBlocklist,
}

/// Outcome returned by [`StoryPrivacyBox::show`] / [`StoryPrivacyBox::show_modal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoryPrivacyOutcome {
    /// Keep dialog open.
    None,
    /// Close without applying changes.
    Cancel,
    /// Apply the chosen [`StoryPrivacyInput`].
    Apply(StoryPrivacyInput),
    /// Save updated Close Friends list (`contacts.editCloseFriends`).
    UpdateCloseFriends(Vec<PeerId>),
    /// Bulk-replace the hidden-stories blocklist (`contacts.setBlocked(my_stories_from=true)`).
    SaveStoriesBlocklist(Vec<PeerId>),
}

/// Interactive story audience & exceptions selector (`boxes/story_privacy_box.cpp`).
#[derive(Clone, Debug)]
pub struct StoryPrivacyBox {
    /// Active audience visibility option.
    pub privacy: StoryPrivacy,
    /// Excluded peer IDs when `privacy == StoryPrivacy::Public`.
    pub excluded_everyone: BTreeSet<PeerId>,
    /// Excluded peer IDs when `privacy == StoryPrivacy::Contacts`.
    pub excluded_contacts: BTreeSet<PeerId>,
    /// Allowed peer IDs when `privacy == StoryPrivacy::SelectedContacts`.
    pub selected_peers: BTreeSet<PeerId>,
    /// Current Close Friends peer IDs.
    pub close_friends: BTreeSet<PeerId>,
    /// Contacts bulk-blocked from viewing our stories (`contacts.setBlocked(my_stories_from=true)`).
    pub blocked_peers: BTreeSet<PeerId>,
    /// Whether [`Self::blocked_peers`] has been synced from `backend.is_viewer_stories_blocked`.
    blocklist_synced: bool,
    /// Active peer-list exception picker sub-view, if any.
    pub editing_list: Option<PrivacyPeerListKind>,
    /// Search filter inside the peer picker.
    pub search: String,
    just_opened: bool,
}

impl Default for StoryPrivacyBox {
    fn default() -> Self {
        Self {
            privacy: StoryPrivacy::Public,
            excluded_everyone: BTreeSet::new(),
            excluded_contacts: BTreeSet::new(),
            selected_peers: BTreeSet::new(),
            close_friends: BTreeSet::new(),
            blocked_peers: BTreeSet::new(),
            blocklist_synced: false,
            editing_list: None,
            search: String::new(),
            just_opened: true,
        }
    }
}

impl StoryPrivacyBox {
    /// Creates a [`StoryPrivacyBox`] initialized from an existing [`StoryPrivacyInput`] and
    /// Close Friends peer list (`ApplyPrivacy` in `send_story_box.cpp:635-653`).
    #[must_use]
    pub fn from_input(input: &StoryPrivacyInput, close_friends: &[PeerId]) -> Self {
        let mut box_state = Self {
            privacy: input.privacy,
            close_friends: close_friends.iter().copied().collect(),
            ..Self::default()
        };
        match input.privacy {
            StoryPrivacy::Public => {
                box_state.excluded_everyone = input.disallow_peers.iter().copied().collect();
            }
            StoryPrivacy::Contacts => {
                box_state.excluded_contacts = input.disallow_peers.iter().copied().collect();
            }
            StoryPrivacy::SelectedContacts | StoryPrivacy::Other => {
                box_state.privacy = StoryPrivacy::SelectedContacts;
                box_state.selected_peers = input.allow_peers.iter().copied().collect();
            }
            StoryPrivacy::CloseFriends => {}
        }
        box_state
    }

    /// Collects the selected audience and exception lists into a [`StoryPrivacyInput`]
    /// (`CollectPrivacy` in `send_story_box.cpp:109-126`).
    #[must_use]
    pub fn to_input(&self) -> StoryPrivacyInput {
        match self.privacy {
            StoryPrivacy::Public => StoryPrivacyInput {
                privacy: StoryPrivacy::Public,
                allow_peers: Vec::new(),
                disallow_peers: self.excluded_everyone.iter().copied().collect(),
            },
            StoryPrivacy::Contacts => StoryPrivacyInput {
                privacy: StoryPrivacy::Contacts,
                allow_peers: Vec::new(),
                disallow_peers: self.excluded_contacts.iter().copied().collect(),
            },
            StoryPrivacy::CloseFriends => StoryPrivacyInput {
                privacy: StoryPrivacy::CloseFriends,
                allow_peers: Vec::new(),
                disallow_peers: Vec::new(),
            },
            StoryPrivacy::SelectedContacts | StoryPrivacy::Other => StoryPrivacyInput {
                privacy: StoryPrivacy::SelectedContacts,
                allow_peers: self.selected_peers.iter().copied().collect(),
                disallow_peers: Vec::new(),
            },
        }
    }

    /// Formats a concise summary for the privacy row inside [`SendStoryBox`].
    #[must_use]
    pub fn summary_label(&self, backend: &dyn StoriesBackend) -> String {
        match self.privacy {
            StoryPrivacy::Public => {
                if self.excluded_everyone.is_empty() {
                    "Everyone".to_owned()
                } else {
                    format!("Everyone (except {})", self.excluded_everyone.len())
                }
            }
            StoryPrivacy::Contacts => {
                if self.excluded_contacts.is_empty() {
                    "My Contacts".to_owned()
                } else {
                    format!("My Contacts (except {})", self.excluded_contacts.len())
                }
            }
            StoryPrivacy::CloseFriends => {
                if self.close_friends.is_empty() {
                    "Close Friends".to_owned()
                } else {
                    format!("Close Friends ({})", self.close_friends.len())
                }
            }
            StoryPrivacy::SelectedContacts | StoryPrivacy::Other => {
                if self.selected_peers.is_empty() {
                    "Selected Contacts".to_owned()
                } else if self.selected_peers.len() == 1 {
                    let only = *self.selected_peers.iter().next().unwrap_or(&PeerId::ZERO);
                    format!("Selected: {}", backend.peer_profile(only).name)
                } else {
                    format!("Selected Contacts ({})", self.selected_peers.len())
                }
            }
        }
    }

    /// Renders the privacy box content inside `ui`.
    pub fn show(&mut self, ui: &mut Ui, backend: &dyn StoriesBackend) -> StoryPrivacyOutcome {
        let mut outcome = StoryPrivacyOutcome::None;
        if !self.blocklist_synced {
            self.blocked_peers = backend
                .contacts_for_privacy()
                .into_iter()
                .filter(|p| backend.is_viewer_stories_blocked(p.peer_id))
                .map(|p| p.peer_id)
                .collect();
            self.blocklist_synced = true;
        }

        if let Some(list_kind) = self.editing_list {
            return self.show_peer_list_editor(ui, list_kind, backend);
        }

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Who can see this story")
                    .size(16.0)
                    .strong()
                    .color(Color32::WHITE),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if close_x_button(ui) {
                    outcome = StoryPrivacyOutcome::Cancel;
                }
            });
        });

        ui.add_space(10.0);

        let rows: [(
            StoryPrivacy,
            &str,
            Color32,
            String,
            PrivacyPeerListKind,
            &str,
        ); 4] = [
            (
                StoryPrivacy::Public,
                "Everyone",
                Color32::from_rgb(0x40, 0xA7, 0xE3),
                excluded_subtitle(&self.excluded_everyone, backend),
                PrivacyPeerListKind::ExcludedEveryone,
                "Exclude...",
            ),
            (
                StoryPrivacy::Contacts,
                "My Contacts",
                Color32::from_rgb(0x8E, 0x6C, 0xE6),
                excluded_subtitle(&self.excluded_contacts, backend),
                PrivacyPeerListKind::ExcludedContacts,
                "Exclude...",
            ),
            (
                StoryPrivacy::CloseFriends,
                "Close Friends",
                Color32::from_rgb(0x4E, 0xB8, 0x57),
                people_subtitle(&self.close_friends, backend),
                PrivacyPeerListKind::CloseFriends,
                "Edit list...",
            ),
            (
                StoryPrivacy::SelectedContacts,
                "Selected Contacts",
                Color32::from_rgb(0xF5, 0x9B, 0x3A),
                people_subtitle(&self.selected_peers, backend),
                PrivacyPeerListKind::SelectedContacts,
                "Choose...",
            ),
        ];

        for (mode, title, badge_color, subtitle, list_kind, action_label) in rows {
            let selected = self.privacy == mode;
            Frame::NONE
                .fill(if selected {
                    Color32::from_rgb(0x25, 0x34, 0x45)
                } else {
                    Color32::from_rgb(0x1B, 0x22, 0x2B)
                })
                .stroke(Stroke::new(
                    1.0,
                    if selected {
                        Color32::from_rgb(0x40, 0xA7, 0xE3)
                    } else {
                        Color32::from_rgb(0x2D, 0x38, 0x45)
                    },
                ))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot_rect, _) =
                            ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                        ui.painter()
                            .circle_filled(dot_rect.center(), 12.0, badge_color);
                        let glyph = match mode {
                            StoryPrivacy::Public => "E",
                            StoryPrivacy::Contacts => "C",
                            StoryPrivacy::CloseFriends => "F",
                            StoryPrivacy::SelectedContacts | StoryPrivacy::Other => "S",
                        };
                        ui.painter().text(
                            dot_rect.center(),
                            Align2::CENTER_CENTER,
                            glyph,
                            FontId::proportional(11.5),
                            Color32::WHITE,
                        );

                        ui.vertical(|ui| {
                            if ui
                                .radio(
                                    selected,
                                    RichText::new(title)
                                        .size(13.5)
                                        .strong()
                                        .color(Color32::WHITE),
                                )
                                .clicked()
                            {
                                self.privacy = mode;
                            }
                            ui.label(
                                RichText::new(&subtitle)
                                    .size(11.5)
                                    .color(Color32::from_rgb(0x8D, 0x9F, 0xB0)),
                            );
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(action_label)
                                            .size(11.5)
                                            .color(Color32::from_rgb(0x70, 0xC8, 0xFA)),
                                    )
                                    .fill(Color32::from_rgb(0x22, 0x2D, 0x3A))
                                    .corner_radius(CornerRadius::same(6)),
                                )
                                .clicked()
                            {
                                self.privacy = mode;
                                self.editing_list = Some(list_kind);
                                self.search.clear();
                            }
                        });
                    });
                });
            ui.add_space(6.0);
        }

        // Bulk hidden-stories blocklist row (`contacts.setBlocked(my_stories_from=true)`).
        let blocklist_subtitle = if self.blocked_peers.is_empty() {
            "No contacts blocked from stories".to_owned()
        } else if self.blocked_peers.len() == 1 {
            let only = *self.blocked_peers.iter().next().unwrap_or(&PeerId::ZERO);
            format!("Hidden from {}", backend.peer_profile(only).name)
        } else {
            format!("Hidden from {} contacts", self.blocked_peers.len())
        };
        Frame::NONE
            .fill(Color32::from_rgb(0x1B, 0x22, 0x2B))
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x2D, 0x38, 0x45)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    ui.painter().circle_filled(
                        dot_rect.center(),
                        12.0,
                        Color32::from_rgb(0xD9, 0x53, 0x4F),
                    );
                    ui.painter().text(
                        dot_rect.center(),
                        Align2::CENTER_CENTER,
                        "H",
                        FontId::proportional(11.5),
                        Color32::WHITE,
                    );
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Hide My Stories From")
                                .size(13.5)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new(&blocklist_subtitle)
                                .size(11.5)
                                .color(Color32::from_rgb(0x8D, 0x9F, 0xB0)),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Manage Blocklist...")
                                        .size(11.5)
                                        .color(Color32::from_rgb(0xFF, 0x8A, 0x80)),
                                )
                                .fill(Color32::from_rgb(0x2B, 0x21, 0x26))
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            self.editing_list = Some(PrivacyPeerListKind::StoriesBlocklist);
                            self.search.clear();
                        }
                    });
                });
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                outcome = StoryPrivacyOutcome::Cancel;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let save_btn = egui::Button::new(
                    RichText::new("Save Privacy")
                        .size(13.0)
                        .strong()
                        .color(Color32::WHITE),
                )
                .fill(Color32::from_rgb(0x40, 0xA7, 0xE3))
                .corner_radius(CornerRadius::same(7));
                if ui.add(save_btn).clicked() {
                    outcome = StoryPrivacyOutcome::Apply(self.to_input());
                }
            });
        });

        outcome
    }

    fn show_peer_list_editor(
        &mut self,
        ui: &mut Ui,
        list_kind: PrivacyPeerListKind,
        backend: &dyn StoriesBackend,
    ) -> StoryPrivacyOutcome {
        let mut outcome = StoryPrivacyOutcome::None;
        let title = match list_kind {
            PrivacyPeerListKind::ExcludedEveryone => "Exclude from Everyone",
            PrivacyPeerListKind::ExcludedContacts => "Exclude from My Contacts",
            PrivacyPeerListKind::SelectedContacts => "Choose Selected Contacts",
            PrivacyPeerListKind::CloseFriends => "Edit Close Friends",
            PrivacyPeerListKind::StoriesBlocklist => "Hide My Stories From",
        };

        ui.horizontal(|ui| {
            if tg_app_core::ui_icons::back_button(ui, "Back").clicked() {
                self.editing_list = None;
                return;
            }
            ui.label(
                RichText::new(title)
                    .size(15.0)
                    .strong()
                    .color(Color32::WHITE),
            );
        });

        ui.add_space(8.0);
        ui.add_sized(
            Vec2::new(ui.available_width(), 28.0),
            TextEdit::singleline(&mut self.search)
                .background_color(Color32::from_rgb(0x14, 0x1B, 0x22))
                .text_color(Color32::WHITE)
                .hint_text(
                    RichText::new("Search contacts...").color(Color32::from_rgb(0x88, 0x99, 0xA8)),
                ),
        );
        ui.add_space(8.0);

        let q = self.search.trim().to_lowercase();
        let contacts: Vec<PeerProfile> = backend
            .contacts_for_privacy()
            .into_iter()
            .filter(|p| {
                q.is_empty()
                    || p.name.to_lowercase().contains(&q)
                    || p.username.to_lowercase().contains(&q)
            })
            .collect();

        let target_set = match list_kind {
            PrivacyPeerListKind::ExcludedEveryone => &mut self.excluded_everyone,
            PrivacyPeerListKind::ExcludedContacts => &mut self.excluded_contacts,
            PrivacyPeerListKind::SelectedContacts => &mut self.selected_peers,
            PrivacyPeerListKind::CloseFriends => &mut self.close_friends,
            PrivacyPeerListKind::StoriesBlocklist => &mut self.blocked_peers,
        };

        ScrollArea::vertical()
            .id_salt("story_privacy_peer_list_scroll")
            .max_height(220.0)
            .show(ui, |ui| {
                if contacts.is_empty() {
                    ui.label(
                        RichText::new("No matching contacts")
                            .size(12.5)
                            .color(Color32::from_rgb(0x88, 0x99, 0xA8)),
                    );
                    return;
                }
                for contact in &contacts {
                    let mut checked = target_set.contains(&contact.peer_id);
                    ui.horizontal(|ui| {
                        let (av_rect, _) =
                            ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                        paint_avatar_circle(ui.painter(), av_rect.center(), 13.0, contact, 10.0);
                        if ui
                            .checkbox(
                                &mut checked,
                                RichText::new(&contact.name)
                                    .size(13.0)
                                    .color(Color32::WHITE),
                            )
                            .changed()
                        {
                            if checked {
                                target_set.insert(contact.peer_id);
                            } else {
                                target_set.remove(&contact.peer_id);
                            }
                        }
                    });
                    ui.add_space(3.0);
                }
            });

        let selected_count = target_set.len();

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{selected_count} selected"))
                    .size(12.0)
                    .color(Color32::from_rgb(0x8D, 0x9F, 0xB0)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let done_label = if list_kind == PrivacyPeerListKind::StoriesBlocklist {
                    "Save Blocklist"
                } else {
                    "Done"
                };
                let done_btn = egui::Button::new(
                    RichText::new(done_label)
                        .size(12.5)
                        .strong()
                        .color(Color32::WHITE),
                )
                .fill(Color32::from_rgb(0x40, 0xA7, 0xE3))
                .corner_radius(CornerRadius::same(6));
                if ui.add(done_btn).clicked() {
                    if list_kind == PrivacyPeerListKind::CloseFriends {
                        outcome = StoryPrivacyOutcome::UpdateCloseFriends(
                            self.close_friends.iter().copied().collect(),
                        );
                    } else if list_kind == PrivacyPeerListKind::StoriesBlocklist {
                        outcome = StoryPrivacyOutcome::SaveStoriesBlocklist(
                            self.blocked_peers.iter().copied().collect(),
                        );
                    }
                    self.editing_list = None;
                }
            });
        });

        outcome
    }

    /// Renders [`StoryPrivacyBox`] centered as a standalone modal over `viewport`.
    pub fn show_modal(
        &mut self,
        ctx: &egui::Context,
        viewport: Rect,
        backend: &dyn StoriesBackend,
    ) -> StoryPrivacyOutcome {
        let mut outcome = StoryPrivacyOutcome::None;
        let card_w = 410.0_f32.min(viewport.width() - 32.0);
        let backdrop_clicked = paint_modal_backdrop(ctx, viewport, "story_privacy_modal_backdrop")
            && !self.just_opened;
        let pos = modal_card_pos(viewport, card_w);
        let card_resp = egui::Area::new(Id::unique("story_privacy_modal_card"))
            .order(egui::Order::Foreground)
            .fade_in(false)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                story_modal_frame().show(ui, |ui| {
                    ui.set_width(card_w);
                    let inner = self.show(ui, backend);
                    if inner != StoryPrivacyOutcome::None {
                        outcome = inner;
                    }
                });
            });
        if backdrop_clicked
            && outcome == StoryPrivacyOutcome::None
            && !pointer_inside_rect(ctx, card_resp.response.rect)
        {
            outcome = StoryPrivacyOutcome::Cancel;
        }
        let was_just_opened = self.just_opened;
        self.just_opened = false;
        if !was_just_opened && ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.editing_list.is_some() {
                self.editing_list = None;
                return StoryPrivacyOutcome::None;
            }
            return StoryPrivacyOutcome::Cancel;
        }
        outcome
    }
}

fn excluded_subtitle(peers: &BTreeSet<PeerId>, backend: &dyn StoriesBackend) -> String {
    if peers.is_empty() {
        "Exclude people".to_owned()
    } else if peers.len() == 1 {
        let only = *peers.iter().next().unwrap_or(&PeerId::ZERO);
        format!("Except {}", backend.peer_profile(only).name)
    } else {
        format!("{} excluded", peers.len())
    }
}

fn people_subtitle(peers: &BTreeSet<PeerId>, backend: &dyn StoriesBackend) -> String {
    if peers.is_empty() {
        "Edit list".to_owned()
    } else if peers.len() == 1 {
        let only = *peers.iter().next().unwrap_or(&PeerId::ZERO);
        backend.peer_profile(only).name
    } else {
        format!("{} people", peers.len())
    }
}

// =============================================================================
// StoryDraftsBox (`boxes/story_drafts_box.cpp`)
// =============================================================================

/// Outcome returned by [`StoryDraftsBox::show`] / [`StoryDraftsBox::show_modal`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StoryDraftsOutcome {
    /// Keep drafts box open.
    None,
    /// Close drafts box.
    Close,
    /// Resume composing draft with the given local `id`.
    ResumeDraft(u64),
    /// Delete draft with the given local `id`.
    DeleteDraft(u64),
    /// Start a brand-new story composer.
    CreateNew,
}

/// Modal dialog listing saved story drafts (`boxes/story_drafts_box.cpp`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StoryDraftsBox {
    /// Whether the box also offers a primary `"New Story"` action (`choosing` mode in tdesktop).
    pub choosing_media: bool,
    just_opened: bool,
}

impl Default for StoryDraftsBox {
    fn default() -> Self {
        Self::new(false)
    }
}

impl StoryDraftsBox {
    /// Creates a [`StoryDraftsBox`].
    #[must_use]
    pub const fn new(choosing_media: bool) -> Self {
        Self {
            choosing_media,
            just_opened: true,
        }
    }

    /// Renders the story drafts list inside `ui`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        drafts: &[StoryDraft],
        backend: &dyn StoriesBackend,
        now_secs: TimeId,
    ) -> StoryDraftsOutcome {
        let mut outcome = StoryDraftsOutcome::None;
        let title = if self.choosing_media {
            "Post Story"
        } else {
            "Story Drafts"
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
                    outcome = StoryDraftsOutcome::Close;
                }
            });
        });

        ui.add_space(10.0);

        if drafts.is_empty() {
            Frame::NONE
                .fill(Color32::from_rgb(0x18, 0x20, 0x28))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(16))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("No saved story drafts yet.")
                                .size(13.0)
                                .color(Color32::from_rgb(0x98, 0xA8, 0xB8)),
                        );
                    });
                });
        } else {
            ScrollArea::vertical()
                .id_salt("story_drafts_list_scroll")
                .max_height(260.0)
                .show(ui, |ui| {
                    for draft in drafts {
                        let media_badge = match &draft.media {
                            Some(StoryUploadMedia::Video { .. }) => "VIDEO",
                            Some(StoryUploadMedia::Photo { .. }) => "PHOTO",
                            None => "DRAFT",
                        };
                        let badge_bg = match &draft.media {
                            Some(StoryUploadMedia::Video { .. }) => {
                                Color32::from_rgb(0x6A, 0x4C, 0xC4)
                            }
                            Some(StoryUploadMedia::Photo { .. }) => {
                                Color32::from_rgb(0x3B, 0x82, 0xC4)
                            }
                            None => Color32::from_rgb(0x45, 0x5A, 0x64),
                        };
                        let subtitle = if !draft.caption.text.trim().is_empty() {
                            draft.caption.text.clone()
                        } else if let Some(media) = &draft.media {
                            format!(
                                "{} • {}",
                                if media.is_video() { "Video" } else { "Photo" },
                                media.file_name()
                            )
                        } else {
                            let peer = backend.peer_profile(draft.peer_id);
                            format!("Story for {}", peer.short_name)
                        };

                        Frame::NONE
                            .fill(Color32::from_rgb(0x22, 0x2B, 0x35))
                            .stroke(Stroke::new(1.0, Color32::from_rgb(0x32, 0x3E, 0x4C)))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let (pill, _) = ui
                                        .allocate_exact_size(Vec2::new(48.0, 32.0), Sense::hover());
                                    ui.painter()
                                        .rect_filled(pill, CornerRadius::same(6), badge_bg);
                                    ui.painter().text(
                                        pill.center(),
                                        Align2::CENTER_CENTER,
                                        media_badge,
                                        FontId::proportional(9.5),
                                        Color32::WHITE,
                                    );

                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} • {} • {}",
                                                format_story_timestamp(draft.created_at, now_secs),
                                                format_story_period(draft.period),
                                                privacy_mode_label(draft.privacy.privacy)
                                            ))
                                            .size(12.0)
                                            .strong()
                                            .color(Color32::WHITE),
                                        );
                                        ui.label(
                                            RichText::new(subtitle)
                                                .size(11.5)
                                                .color(Color32::from_rgb(0x98, 0xA8, 0xB8)),
                                        );
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new("Delete").size(11.5).color(
                                                            Color32::from_rgb(0xEC, 0x5B, 0x5B),
                                                        ),
                                                    )
                                                    .fill(Color32::from_rgb(0x2D, 0x22, 0x26))
                                                    .corner_radius(CornerRadius::same(6)),
                                                )
                                                .clicked()
                                            {
                                                outcome = StoryDraftsOutcome::DeleteDraft(draft.id);
                                            }
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new("Resume")
                                                            .size(11.5)
                                                            .color(Color32::WHITE),
                                                    )
                                                    .fill(Color32::from_rgb(0x3B, 0x82, 0xC4))
                                                    .corner_radius(CornerRadius::same(6)),
                                                )
                                                .clicked()
                                            {
                                                outcome = StoryDraftsOutcome::ResumeDraft(draft.id);
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Close").clicked() {
                outcome = StoryDraftsOutcome::Close;
            }
            if self.choosing_media {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn = egui::Button::new(
                        RichText::new("New Story")
                            .size(13.0)
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(Color32::from_rgb(0x40, 0xA7, 0xE3))
                    .corner_radius(CornerRadius::same(7));
                    if ui.add(btn).clicked() {
                        outcome = StoryDraftsOutcome::CreateNew;
                    }
                });
            }
        });

        outcome
    }

    /// Renders [`StoryDraftsBox`] centered as a modal dialog over `viewport`.
    pub fn show_modal(
        &mut self,
        ctx: &egui::Context,
        viewport: Rect,
        drafts: &[StoryDraft],
        backend: &dyn StoriesBackend,
        now_secs: TimeId,
    ) -> StoryDraftsOutcome {
        let mut outcome = StoryDraftsOutcome::None;
        let card_w = 430.0_f32.min(viewport.width() - 32.0);
        let backdrop_clicked =
            paint_modal_backdrop(ctx, viewport, "story_drafts_modal_backdrop") && !self.just_opened;
        let pos = modal_card_pos(viewport, card_w);
        let card_resp = egui::Area::new(Id::unique("story_drafts_modal_card"))
            .order(egui::Order::Foreground)
            .fade_in(false)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                story_modal_frame().show(ui, |ui| {
                    ui.set_width(card_w);
                    let inner = self.show(ui, drafts, backend, now_secs);
                    if inner != StoryDraftsOutcome::None {
                        outcome = inner;
                    }
                });
            });
        if backdrop_clicked
            && outcome == StoryDraftsOutcome::None
            && !pointer_inside_rect(ctx, card_resp.response.rect)
        {
            outcome = StoryDraftsOutcome::Close;
        }
        let was_just_opened = self.just_opened;
        self.just_opened = false;
        if !was_just_opened && ctx.input(|i| i.key_pressed(Key::Escape)) {
            return StoryDraftsOutcome::Close;
        }
        outcome
    }
}

// =============================================================================
// SendStoryBox (`boxes/send_story_box.cpp` + `media/stories/media_stories_post.cpp`)
// =============================================================================

/// Which interactive media-area sticker tab is active inside [`SendStoryBox`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum MediaAreaEditorKind {
    /// Suggested reaction sticker (`MediaAreaSuggestedReaction`).
    #[default]
    Reaction,
    /// Clickable URL sticker (`MediaAreaUrl`).
    Link,
    /// Geo/venue location sticker (`MediaAreaVenue` / `MediaAreaGeoPoint`).
    Location,
    /// Weather sticker (`MediaAreaWeather`).
    Weather,
}

/// Outcome returned by [`SendStoryBox::show`] / [`SendStoryBox::show_modal`].
#[derive(Clone, Debug)]
pub enum SendStoryOutcome {
    /// Keep composer open.
    None,
    /// Close composer without posting.
    Close,
    /// Open [`StoryDraftsBox`].
    OpenDrafts,
    /// Save the current composer state as a local draft.
    SaveDraft(StoryDraft),
    /// Submit a new story via [`StoriesBackend::send_story`].
    SubmitSend(SendStoryRequest),
    /// Submit edits to an existing story via [`StoriesBackend::edit_story`].
    SubmitEdit(EditStoryRequest),
    /// Save updated Close Friends list from the embedded privacy editor.
    UpdateCloseFriends(Vec<PeerId>),
    /// Bulk-replace the hidden-stories blocklist (`contacts.setBlocked(my_stories_from=true)`).
    SaveStoriesBlocklist(Vec<PeerId>),
}

/// Story composer & editor modal (`boxes/send_story_box.cpp` + `media_stories_post.cpp`).
pub struct SendStoryBox {
    /// Target peer ID (`self_peer_id` or channel).
    pub peer_id: PeerId,
    /// When `Some`, editing an existing story via `stories.editStory`.
    pub editing_story_id: Option<FullStoryId>,
    /// Selected photo or video payload to upload.
    pub media: Option<StoryUploadMedia>,
    /// Manual file path input for loading photo/video media without opening a native dialog.
    pub media_path_input: String,
    /// Caption text (supports inline markdown markers and explicit `caption_entities`).
    pub caption_text: String,
    /// Explicit UTF-16 caption formatting entities.
    pub caption_entities: Vec<EntityInText>,
    /// Selected expiry period in seconds (`21600`, `43200`, `86400`, `172800`).
    pub period_secs: i32,
    /// Whether "Keep on profile" (`pinned`) is enabled.
    pub pinned: bool,
    /// Whether screenshots & forwarding are allowed (`noforwards = !allow_forwards`).
    pub allow_forwards: bool,
    /// Optional album IDs when `pinned == true`.
    pub albums: Vec<i32>,
    /// Embedded audience & exceptions selector ([`StoryPrivacyBox`]).
    pub privacy_box: StoryPrivacyBox,
    /// Snapshot of `privacy_box` before entering the privacy editor so Cancel reverts edits.
    privacy_backup: Option<StoryPrivacyBox>,
    /// Whether the privacy editor sub-screen is currently displayed.
    pub show_privacy_editor: bool,
    /// Attached interactive media areas.
    pub areas: StoryMediaAreasInput,
    /// Active interactive area editor tab.
    pub area_tab: MediaAreaEditorKind,
    /// Whether the collapsible media-areas section is expanded.
    pub show_areas_editor: bool,
    /// Draft inputs for attaching a reaction area.
    pub draft_reaction_emoji: String,
    /// Dark theme flag for the draft reaction area.
    pub draft_reaction_dark: bool,
    /// Flipped orientation flag for the draft reaction area.
    pub draft_reaction_flipped: bool,
    /// Draft URL input for attaching a URL area.
    pub draft_url: String,
    /// Draft location title.
    pub draft_location_title: String,
    /// Draft location city.
    pub draft_location_city: String,
    /// Draft location latitude.
    pub draft_location_lat: f64,
    /// Draft location longitude.
    pub draft_location_lon: f64,
    /// Draft weather emoji.
    pub draft_weather_emoji: String,
    /// Draft weather ARGB color.
    pub draft_weather_color: u32,
    /// Draft weather temperature in Celsius.
    pub draft_weather_celsius: f32,
    /// Normalized horizontal center (0..100%) for newly added areas.
    pub draft_area_x_pct: f32,
    /// Normalized vertical center (0..100%) for newly added areas.
    pub draft_area_y_pct: f32,
    /// Optional local draft ID if resumed from [`StoryDraftsBox`].
    pub draft_id: Option<u64>,
    /// Inline validation / load error message.
    pub error_message: Option<String>,
    /// Cached preview texture for the selected photo.
    preview_texture: Option<TextureHandle>,
    preview_dirty: bool,
    just_opened: bool,
}

impl SendStoryBox {
    /// Creates a new story composer targeting `peer_id` with default 24h period and Everyone privacy.
    #[must_use]
    pub fn new(peer_id: PeerId, close_friends: &[PeerId]) -> Self {
        Self {
            peer_id,
            editing_story_id: None,
            media: None,
            media_path_input: String::new(),
            caption_text: String::new(),
            caption_entities: Vec::new(),
            period_secs: DEFAULT_STORY_PERIOD_SECS,
            pinned: true,
            allow_forwards: true,
            albums: Vec::new(),
            privacy_box: StoryPrivacyBox::from_input(&StoryPrivacyInput::default(), close_friends),
            privacy_backup: None,
            show_privacy_editor: false,
            areas: StoryMediaAreasInput::default(),
            area_tab: MediaAreaEditorKind::Reaction,
            show_areas_editor: false,
            draft_reaction_emoji: COMPOSER_REACTION_PRESETS[0].to_owned(),
            draft_reaction_dark: false,
            draft_reaction_flipped: false,
            draft_url: String::new(),
            draft_location_title: String::new(),
            draft_location_city: String::new(),
            draft_location_lat: 47.6062,
            draft_location_lon: -122.3321,
            draft_weather_emoji: COMPOSER_WEATHER_PRESETS[0].0.to_owned(),
            draft_weather_color: COMPOSER_WEATHER_PRESETS[0].1,
            draft_weather_celsius: 22.0,
            draft_area_x_pct: 50.0,
            draft_area_y_pct: 55.0,
            draft_id: None,
            error_message: None,
            preview_texture: None,
            preview_dirty: false,
            just_opened: true,
        }
    }

    /// Creates a [`SendStoryBox`] pre-populated for editing an existing [`Story`].
    #[must_use]
    pub fn for_edit(story: &Story, close_friends: &[PeerId]) -> Self {
        let mut composer = Self::new(story.peer_id, close_friends);
        composer.editing_story_id = Some(story.full_id());
        let ui_cap = data_text_to_ui_text(&story.caption);
        composer.caption_text = ui_cap.text;
        composer.caption_entities = ui_cap.entities;
        composer.pinned = story.in_profile;
        composer.allow_forwards = !story.no_forwards;
        composer.period_secs = (story.expires - story.date).max(21_600);
        let privacy_input = StoryPrivacyInput {
            privacy: story.privacy,
            allow_peers: story.privacy_allowed.clone(),
            disallow_peers: story.privacy_disallowed.clone(),
        };
        composer.privacy_box = StoryPrivacyBox::from_input(&privacy_input, close_friends);
        composer.areas = StoryMediaAreasInput {
            locations: story.locations.clone(),
            suggested_reactions: story.suggested_reactions.clone(),
            channel_posts: story.channel_posts.clone(),
            url_areas: story.url_areas.clone(),
            weather_areas: story.weather_areas.clone(),
        };
        composer
    }

    /// Restores a [`SendStoryBox`] from a saved [`StoryDraft`].
    #[must_use]
    pub fn from_draft(draft: &StoryDraft, close_friends: &[PeerId]) -> Self {
        let mut composer = Self::new(draft.peer_id, close_friends);
        composer.draft_id = Some(draft.id);
        composer.media = draft.media.clone();
        composer.preview_dirty = composer.media.is_some();
        composer.caption_text = draft.caption.text.clone();
        composer.caption_entities = draft.caption.entities.clone();
        composer.period_secs = draft.period;
        composer.pinned = draft.pinned;
        composer.allow_forwards = !draft.noforwards;
        composer.privacy_box = StoryPrivacyBox::from_input(&draft.privacy, close_friends);
        composer.areas = draft.areas.clone();
        composer
    }

    /// Attaches or replaces the selected media payload and marks the preview dirty.
    pub fn set_media(&mut self, media: StoryUploadMedia) {
        self.media = Some(media);
        self.preview_dirty = true;
        self.error_message = None;
    }

    /// Loads photo or video media from `path` into the composer.
    pub fn load_media_from_path(&mut self, path: &Path) -> Result<(), String> {
        let loaded = load_story_media_from_path(path)?;
        self.set_media(loaded);
        Ok(())
    }

    /// Attaches a procedurally generated sample story photo PNG (useful for quick local preview
    /// and headless UI tests).
    pub fn attach_sample_photo(&mut self) {
        let png = generate_gradient_story_png(
            360,
            640,
            GradientPreset {
                top: [0x14, 0x28, 0x50],
                mid: [0x2A, 0x6F, 0x97],
                bottom: [0x61, 0xA5, 0xC2],
                accent: [0xFF, 0xD1, 0x66],
                orb_x: 0.7,
                orb_y: 0.3,
                stripes: true,
            },
        );
        self.set_media(StoryUploadMedia::Photo {
            bytes: png,
            file_name: "sample_story.png".to_owned(),
        });
    }

    /// Applies a formatting [`EntityType`] span across the entire current caption text
    /// (or toggles it off if already covering the full caption).
    pub fn toggle_caption_entity(&mut self, entity_type: EntityType) {
        let len_utf16 = self.caption_text.encode_utf16().count() as i32;
        if len_utf16 == 0 {
            return;
        }
        let target = EntityInText::new(entity_type, 0, len_utf16);
        if let Some(pos) = self.caption_entities.iter().position(|e| {
            e.entity_type() == entity_type && e.offset() == 0 && e.length() == len_utf16
        }) {
            self.caption_entities.remove(pos);
        } else {
            self.caption_entities.push(target);
        }
    }

    /// Builds the final formatted [`UiText`] combining inline markdown and explicit entities.
    #[must_use]
    pub fn build_caption(&self) -> UiText {
        parse_story_caption_entities(&self.caption_text, &self.caption_entities)
    }

    /// Snapshots the current composer state into a [`StoryDraft`].
    #[must_use]
    pub fn to_draft(&self, id: u64, now_secs: TimeId) -> StoryDraft {
        StoryDraft {
            id,
            peer_id: self.peer_id,
            created_at: now_secs,
            media: self.media.clone(),
            caption: self.build_caption(),
            privacy: self.privacy_box.to_input(),
            period: self.period_secs,
            pinned: self.pinned,
            noforwards: !self.allow_forwards,
            areas: self.areas.clone(),
        }
    }

    /// Validates and builds a [`SendStoryRequest`] for publishing a new story.
    pub fn build_send_request(&self) -> Result<SendStoryRequest, String> {
        let media = self
            .media
            .clone()
            .ok_or_else(|| "Choose a photo or video before posting your story.".to_owned())?;
        Ok(SendStoryRequest {
            peer_id: self.peer_id,
            media,
            caption: self.build_caption(),
            privacy: self.privacy_box.to_input(),
            period: Some(self.period_secs),
            pinned: self.pinned,
            noforwards: !self.allow_forwards,
            albums: if self.pinned {
                self.albums.clone()
            } else {
                Vec::new()
            },
            locations: self.areas.locations.clone(),
            suggested_reactions: self.areas.suggested_reactions.clone(),
            channel_posts: self.areas.channel_posts.clone(),
            url_areas: self.areas.url_areas.clone(),
            weather_areas: self.areas.weather_areas.clone(),
        })
    }

    /// Validates and builds an [`EditStoryRequest`] for updating an existing story.
    pub fn build_edit_request(&self) -> Result<EditStoryRequest, String> {
        let full_id = self
            .editing_story_id
            .ok_or_else(|| "Composer is not in edit mode.".to_owned())?;
        Ok(EditStoryRequest {
            peer_id: full_id.peer,
            story_id: full_id.story,
            media: self.media.clone(),
            media_areas: Some(self.areas.clone()),
            caption: Some(self.build_caption()),
            privacy: Some(self.privacy_box.to_input()),
        })
    }

    /// Renders the story composer content inside `ui` (directly callable from `egui_kittest`).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        backend: &dyn StoriesBackend,
        drafts_count: usize,
        now_secs: TimeId,
    ) -> SendStoryOutcome {
        ui.spacing_mut().item_spacing.y = 4.0;
        if self.show_privacy_editor {
            match self.privacy_box.show(ui, backend) {
                StoryPrivacyOutcome::None => return SendStoryOutcome::None,
                StoryPrivacyOutcome::Cancel => {
                    if let Some(mut prev) = self.privacy_backup.take() {
                        prev.close_friends = self.privacy_box.close_friends.clone();
                        self.privacy_box = prev;
                    }
                    self.show_privacy_editor = false;
                    return SendStoryOutcome::None;
                }
                StoryPrivacyOutcome::Apply(_) => {
                    self.privacy_backup = None;
                    self.show_privacy_editor = false;
                    return SendStoryOutcome::None;
                }
                StoryPrivacyOutcome::UpdateCloseFriends(peers) => {
                    return SendStoryOutcome::UpdateCloseFriends(peers);
                }
                StoryPrivacyOutcome::SaveStoriesBlocklist(peers) => {
                    return SendStoryOutcome::SaveStoriesBlocklist(peers);
                }
            }
        }

        self.refresh_preview_texture(ui.ctx());

        let mut outcome = SendStoryOutcome::None;
        let is_edit = self.editing_story_id.is_some();
        let title = if is_edit { "Edit Story" } else { "New Story" };

        // Header bar.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(title)
                    .size(16.0)
                    .strong()
                    .color(Color32::WHITE),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if close_x_button(ui) {
                    outcome = SendStoryOutcome::Close;
                }
                if !is_edit {
                    let drafts_label = if drafts_count > 0 {
                        format!("Drafts ({drafts_count})")
                    } else {
                        "Drafts".to_owned()
                    };
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(drafts_label)
                                    .size(11.5)
                                    .color(Color32::from_rgb(0x88, 0xC8, 0xF8)),
                            )
                            .fill(Color32::from_rgb(0x25, 0x31, 0x3E))
                            .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        outcome = SendStoryOutcome::OpenDrafts;
                    }
                }
            });
        });

        ui.add_space(4.0);

        let scroll_max_h = (ui.ctx().content_rect().height() - 150.0).clamp(220.0, 540.0);
        ScrollArea::vertical()
            .id_salt("send_story_composer_scroll")
            .min_scrolled_height(scroll_max_h)
            .max_height(scroll_max_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // 1. Media picker + preview strip.
                self.paint_media_section(ui);
                ui.add_space(4.0);

                // 2. Caption editor with entity formatting toolbar.
                self.paint_caption_section(ui, backend.account_premium());
                ui.add_space(4.0);

                // 3. Period, Pinned, and No-Forwards options (for new stories).
                if !is_edit {
                    self.paint_options_section(ui, backend.account_premium());
                    ui.add_space(4.0);
                }

                // 4. Privacy summary row -> opens StoryPrivacyBox.
                self.paint_privacy_row(ui, backend);
                ui.add_space(4.0);

                // 5. Interactive media areas editor (Reaction / Link / Location / Weather).
                self.paint_media_areas_section(ui);
            });

        if let Some(err) = &self.error_message {
            ui.add_space(6.0);
            ui.label(
                RichText::new(err)
                    .size(12.0)
                    .color(Color32::from_rgb(0xF0, 0x6A, 0x6A)),
            );
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                outcome = SendStoryOutcome::Close;
            }
            if !is_edit
                && ui
                    .add(
                        egui::Button::new(
                            RichText::new("Save Draft")
                                .size(12.5)
                                .color(Color32::from_rgb(0xC2, 0xD2, 0xE2)),
                        )
                        .fill(Color32::from_rgb(0x26, 0x31, 0x3E))
                        .corner_radius(CornerRadius::same(7)),
                    )
                    .clicked()
            {
                let id = self.draft_id.unwrap_or(0);
                outcome = SendStoryOutcome::SaveDraft(self.to_draft(id, now_secs));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let submit_label = if is_edit {
                    "Save Changes"
                } else {
                    "Post Story"
                };
                let submit_btn = egui::Button::new(
                    RichText::new(submit_label)
                        .size(13.0)
                        .strong()
                        .color(Color32::WHITE),
                )
                .fill(Color32::from_rgb(0x40, 0xA7, 0xE3))
                .corner_radius(CornerRadius::same(7));
                if ui.add(submit_btn).clicked() {
                    let limit = if backend.account_premium() {
                        STORY_CAPTION_LIMIT_PREMIUM
                    } else {
                        STORY_CAPTION_LIMIT_DEFAULT
                    };
                    let caption_len = self.build_caption().text.encode_utf16().count();
                    if caption_len > limit {
                        self.error_message = Some(format!(
                            "Caption is too long ({caption_len}/{limit} characters)."
                        ));
                    } else if is_edit {
                        match self.build_edit_request() {
                            Ok(req) => {
                                self.error_message = None;
                                outcome = SendStoryOutcome::SubmitEdit(req);
                            }
                            Err(err) => self.error_message = Some(err),
                        }
                    } else {
                        match self.build_send_request() {
                            Ok(req) => {
                                self.error_message = None;
                                outcome = SendStoryOutcome::SubmitSend(req);
                            }
                            Err(err) => self.error_message = Some(err),
                        }
                    }
                }
            });
        });

        outcome
    }

    /// Renders [`SendStoryBox`] centered as a fullscreen-scrimmed modal dialog over `viewport`.
    pub fn show_modal(
        &mut self,
        ctx: &egui::Context,
        viewport: Rect,
        backend: &dyn StoriesBackend,
        drafts_count: usize,
        now_secs: TimeId,
    ) -> SendStoryOutcome {
        let mut outcome = SendStoryOutcome::None;
        let card_w = 460.0_f32.min(viewport.width() - 32.0);
        let backdrop_clicked =
            paint_modal_backdrop(ctx, viewport, "send_story_modal_backdrop") && !self.just_opened;
        let pos = modal_card_pos(viewport, card_w);
        let card_resp = egui::Area::new(Id::unique("send_story_modal_card"))
            .order(egui::Order::Foreground)
            .fade_in(false)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                story_modal_frame().show(ui, |ui| {
                    ui.set_width(card_w);
                    let inner = self.show(ui, backend, drafts_count, now_secs);
                    if !matches!(inner, SendStoryOutcome::None) {
                        outcome = inner;
                    }
                });
            });
        if backdrop_clicked
            && matches!(outcome, SendStoryOutcome::None)
            && !pointer_inside_rect(ctx, card_resp.response.rect)
        {
            outcome = SendStoryOutcome::Close;
        }
        let was_just_opened = self.just_opened;
        self.just_opened = false;
        if !was_just_opened && ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.show_privacy_editor {
                if self.privacy_box.editing_list.is_some() {
                    self.privacy_box.editing_list = None;
                } else {
                    if let Some(mut prev) = self.privacy_backup.take() {
                        prev.close_friends = self.privacy_box.close_friends.clone();
                        self.privacy_box = prev;
                    }
                    self.show_privacy_editor = false;
                }
                return SendStoryOutcome::None;
            }
            return SendStoryOutcome::Close;
        }
        outcome
    }

    fn refresh_preview_texture(&mut self, ctx: &egui::Context) {
        if !self.preview_dirty {
            return;
        }
        self.preview_dirty = false;
        self.preview_texture = None;
        if let Some(StoryUploadMedia::Photo { bytes, .. }) = &self.media
            && let Ok((decoded, _)) = decode_photo_via_tg_media(bytes)
        {
            let img =
                ColorImage::from_rgba_unmultiplied([decoded.width, decoded.height], &decoded.rgba);
            let tex = ctx.load_texture("story_composer_preview", img, TextureOptions::LINEAR);
            self.preview_texture = Some(tex);
        }
    }

    fn paint_media_section(&mut self, ui: &mut Ui) {
        Frame::NONE
            .fill(Color32::from_rgb(0x17, 0x1E, 0x26))
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x2D, 0x38, 0x45)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Portrait 9:16 miniature preview slot.
                    let (preview_rect, _) =
                        ui.allocate_exact_size(Vec2::new(56.0, 92.0), Sense::hover());
                    ui.painter().rect_filled(
                        preview_rect,
                        CornerRadius::same(6),
                        Color32::from_rgb(0x10, 0x15, 0x1C),
                    );
                    if let Some(tex) = &self.preview_texture {
                        ui.painter().image(
                            tex.id(),
                            preview_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        let placeholder = match &self.media {
                            Some(StoryUploadMedia::Video { .. }) => "VIDEO",
                            Some(StoryUploadMedia::Photo { .. }) => "PHOTO",
                            None => "9:16",
                        };
                        ui.painter().text(
                            preview_rect.center(),
                            Align2::CENTER_CENTER,
                            placeholder,
                            FontId::proportional(11.0),
                            Color32::from_rgb(0x7E, 0x90, 0xA2),
                        );
                    }
                    // Tiny dot markers for attached media areas on the preview thumbnail.
                    for r in &self.areas.suggested_reactions {
                        paint_mini_area_dot(
                            ui.painter(),
                            preview_rect,
                            r.area,
                            Color32::from_rgb(0xFF, 0x70, 0x70),
                        );
                    }
                    for u in &self.areas.url_areas {
                        paint_mini_area_dot(
                            ui.painter(),
                            preview_rect,
                            u.area,
                            Color32::from_rgb(0x54, 0xC3, 0xF3),
                        );
                    }
                    for l in &self.areas.locations {
                        paint_mini_area_dot(
                            ui.painter(),
                            preview_rect,
                            l.area,
                            Color32::from_rgb(0x66, 0xBB, 0x6A),
                        );
                    }
                    for w in &self.areas.weather_areas {
                        paint_mini_area_dot(
                            ui.painter(),
                            preview_rect,
                            w.area,
                            Color32::from_rgb(0xFF, 0xCA, 0x28),
                        );
                    }

                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Media")
                                .size(12.5)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        if let Some(media) = &self.media {
                            let summary = match media {
                                StoryUploadMedia::Photo { bytes, file_name } => format!(
                                    "Photo: {} ({} KiB)",
                                    file_name,
                                    (bytes.len() / 1024).max(1)
                                ),
                                StoryUploadMedia::Video {
                                    bytes,
                                    file_name,
                                    width,
                                    height,
                                    duration_secs,
                                    ..
                                } => format!(
                                    "Video: {} ({width}x{height}, {:.1}s, {} KiB)",
                                    file_name,
                                    duration_secs,
                                    (bytes.len() / 1024).max(1)
                                ),
                            };
                            ui.label(
                                RichText::new(summary)
                                    .size(11.5)
                                    .color(Color32::from_rgb(0x88, 0xC8, 0xF8)),
                            );
                        } else {
                            let hint = if self.editing_story_id.is_some() {
                                "Optional: keep current media or replace below."
                            } else {
                                "Pick a photo or video to post as your story."
                            };
                            ui.label(
                                RichText::new(hint)
                                    .size(11.5)
                                    .color(Color32::from_rgb(0x98, 0xA8, 0xB8)),
                            );
                        }

                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Choose Photo...").clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .set_title("Choose Story Photo")
                                    .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                                    .pick_file()
                                && let Err(err) = self.load_media_from_path(&path)
                            {
                                self.error_message = Some(err);
                            }
                            if ui.button("Choose Video...").clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .set_title("Choose Story Video")
                                    .add_filter("Videos", &["mp4", "mov", "webm"])
                                    .pick_file()
                                && let Err(err) = self.load_media_from_path(&path)
                            {
                                self.error_message = Some(err);
                            }
                            if ui.button("Sample Photo").clicked() {
                                self.attach_sample_photo();
                            }
                            if ui
                                .add_enabled(self.media.is_some(), egui::Button::new("Clear"))
                                .clicked()
                            {
                                self.media = None;
                                self.preview_texture = None;
                                self.preview_dirty = false;
                            }
                        });

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                Vec2::new((ui.available_width() - 56.0).max(100.0), 24.0),
                                TextEdit::singleline(&mut self.media_path_input)
                                    .background_color(Color32::from_rgb(0x12, 0x18, 0x1F))
                                    .text_color(Color32::WHITE)
                                    .hint_text("Or paste local photo/video path..."),
                            );
                            if ui.button("Load").clicked() {
                                let raw_path = self.media_path_input.trim().to_owned();
                                if let Err(err) = self.load_media_from_path(Path::new(&raw_path)) {
                                    self.error_message = Some(err);
                                }
                            }
                        });
                    });
                });
            });
    }

    fn paint_caption_section(&mut self, ui: &mut Ui, is_premium: bool) {
        let limit = if is_premium {
            STORY_CAPTION_LIMIT_PREMIUM
        } else {
            STORY_CAPTION_LIMIT_DEFAULT
        };
        let utf16_len = self.caption_text.encode_utf16().count();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Caption")
                    .size(12.5)
                    .strong()
                    .color(Color32::WHITE),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let count_color = if utf16_len > limit {
                    Color32::from_rgb(0xEC, 0x5B, 0x5B)
                } else {
                    Color32::from_rgb(0x8D, 0x9F, 0xB0)
                };
                ui.label(
                    RichText::new(format!("{utf16_len}/{limit}"))
                        .size(11.0)
                        .color(count_color),
                );
            });
        });

        ui.add_space(4.0);
        // Entity formatting toolbar.
        ui.horizontal(|ui| {
            for (label, etype, tip) in [
                ("B", EntityType::Bold, "Bold (**text**)"),
                ("I", EntityType::Italic, "Italic (_text_)"),
                ("U", EntityType::Underline, "Underline (__text__)"),
                ("S", EntityType::StrikeOut, "Strikethrough (~~text~~)"),
                ("<>", EntityType::Code, "Monospace (`code`)"),
                ("||", EntityType::Spoiler, "Spoiler (||text||)"),
            ] {
                let active = self
                    .caption_entities
                    .iter()
                    .any(|e| e.entity_type() == etype);
                let btn =
                    egui::Button::new(RichText::new(label).size(11.5).strong().color(if active {
                        Color32::WHITE
                    } else {
                        Color32::from_rgb(0xB4, 0xC4, 0xD4)
                    }))
                    .fill(if active {
                        Color32::from_rgb(0x3B, 0x82, 0xC4)
                    } else {
                        Color32::from_rgb(0x25, 0x30, 0x3C)
                    })
                    .corner_radius(CornerRadius::same(5))
                    .min_size(Vec2::new(26.0, 22.0));
                if ui.add(btn).on_hover_text(tip).clicked() {
                    self.toggle_caption_entity(etype);
                }
            }
            if !self.caption_entities.is_empty() {
                ui.label(
                    RichText::new(format!("{} entities", self.caption_entities.len()))
                        .size(11.0)
                        .color(Color32::from_rgb(0x88, 0xC8, 0xF8)),
                );
            }
        });

        ui.add_space(2.0);
        ui.add_sized(
            Vec2::new(ui.available_width(), 44.0),
            TextEdit::multiline(&mut self.caption_text)
                .background_color(Color32::from_rgb(0x14, 0x1B, 0x22))
                .text_color(Color32::WHITE)
                .hint_text(
                    RichText::new(
                        "Add a caption... (supports **bold**, _italic_, `code`, ||spoiler||)",
                    )
                    .color(Color32::from_rgb(0x88, 0x99, 0xA8)),
                ),
        );
    }

    fn paint_options_section(&mut self, ui: &mut Ui, is_premium: bool) {
        Frame::NONE
            .fill(Color32::from_rgb(0x17, 0x1E, 0x26))
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x2D, 0x38, 0x45)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Expires in:")
                            .size(12.5)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    for &period in STORY_PERIOD_OPTIONS {
                        let active = self.period_secs == period;
                        let label = format_story_period(period);
                        let btn =
                            egui::Button::new(RichText::new(label).size(12.0).color(if active {
                                Color32::WHITE
                            } else {
                                Color32::from_rgb(0x9A, 0xAA, 0xBA)
                            }))
                            .fill(if active {
                                Color32::from_rgb(0x3B, 0x82, 0xC4)
                            } else {
                                Color32::from_rgb(0x25, 0x30, 0x3C)
                            })
                            .corner_radius(CornerRadius::same(10));
                        if ui.add(btn).clicked() {
                            self.period_secs = period;
                        }
                    }
                    if !is_premium && self.period_secs != DEFAULT_STORY_PERIOD_SECS {
                        ui.label(
                            RichText::new("Premium")
                                .size(10.5)
                                .color(Color32::from_rgb(0xB3, 0x88, 0xFF)),
                        );
                    }
                });

                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.pinned,
                    RichText::new("Keep on profile after expiry (Pinned)")
                        .size(12.5)
                        .color(Color32::WHITE),
                );
                ui.checkbox(
                    &mut self.allow_forwards,
                    RichText::new("Allow screenshots & forwarding")
                        .size(12.5)
                        .color(Color32::WHITE),
                );
            });
    }

    fn paint_privacy_row(&mut self, ui: &mut Ui, backend: &dyn StoriesBackend) {
        let summary = self.privacy_box.summary_label(backend);
        Frame::NONE
            .fill(Color32::from_rgb(0x17, 0x1E, 0x26))
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x2D, 0x38, 0x45)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Privacy:")
                            .size(12.5)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new(summary)
                            .size(12.5)
                            .color(Color32::from_rgb(0x70, 0xC8, 0xFA)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Edit Privacy...")
                                        .size(11.5)
                                        .color(Color32::WHITE),
                                )
                                .fill(Color32::from_rgb(0x28, 0x34, 0x42))
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            self.privacy_backup = Some(self.privacy_box.clone());
                            self.show_privacy_editor = true;
                        }
                    });
                });
            });
    }

    fn paint_media_areas_section(&mut self, ui: &mut Ui) {
        Frame::NONE
            .fill(Color32::from_rgb(0x17, 0x1E, 0x26))
            .stroke(Stroke::new(1.0, Color32::from_rgb(0x2D, 0x38, 0x45)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Interactive Media Areas ({})", self.areas.len()))
                            .size(12.5)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let toggle_label = if self.show_areas_editor {
                            "Hide"
                        } else {
                            "Add Sticker / Area"
                        };
                        if ui.button(toggle_label).clicked() {
                            self.show_areas_editor = !self.show_areas_editor;
                        }
                    });
                });

                // Existing attached areas chips with remove actions.
                if !self.areas.is_empty() {
                    ui.add_space(6.0);
                    let mut remove_reaction = None;
                    let mut remove_url = None;
                    let mut remove_loc = None;
                    let mut remove_weather = None;
                    ui.horizontal_wrapped(|ui| {
                        for (idx, r) in self.areas.suggested_reactions.iter().enumerate() {
                            let label = match &r.reaction {
                                ReactionId::Emoji(e) => format!("{e} ×"),
                                ReactionId::Custom(id) => format!("Custom({id}) ×"),
                                ReactionId::Paid => "Paid ×".to_owned(),
                            };
                            if ui.small_button(label).clicked() {
                                remove_reaction = Some(idx);
                            }
                        }
                        for (idx, u) in self.areas.url_areas.iter().enumerate() {
                            if ui.small_button(format!("Link: {} ×", u.url)).clicked() {
                                remove_url = Some(idx);
                            }
                        }
                        for (idx, l) in self.areas.locations.iter().enumerate() {
                            if ui.small_button(format!("Pin: {} ×", l.title)).clicked() {
                                remove_loc = Some(idx);
                            }
                        }
                        for (idx, w) in self.areas.weather_areas.iter().enumerate() {
                            let c = (f64::from(w.millicelsius) / 1000.0).round() as i32;
                            if ui.small_button(format!("{} {c}°C ×", w.emoji)).clicked() {
                                remove_weather = Some(idx);
                            }
                        }
                    });
                    if let Some(i) = remove_reaction {
                        self.areas.suggested_reactions.remove(i);
                    }
                    if let Some(i) = remove_url {
                        self.areas.url_areas.remove(i);
                    }
                    if let Some(i) = remove_loc {
                        self.areas.locations.remove(i);
                    }
                    if let Some(i) = remove_weather {
                        self.areas.weather_areas.remove(i);
                    }
                }

                if self.show_areas_editor {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        for (tab, label) in [
                            (MediaAreaEditorKind::Reaction, "Reaction"),
                            (MediaAreaEditorKind::Link, "Link"),
                            (MediaAreaEditorKind::Location, "Location"),
                            (MediaAreaEditorKind::Weather, "Weather"),
                        ] {
                            let active = self.area_tab == tab;
                            if ui.selectable_label(active, label).clicked() {
                                self.area_tab = tab;
                            }
                        }
                    });
                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Pos X%:")
                                .size(11.0)
                                .color(Color32::from_rgb(0x98, 0xA8, 0xB8)),
                        );
                        ui.add(egui::Slider::new(&mut self.draft_area_x_pct, 15.0..=85.0));
                        ui.label(
                            RichText::new("Y%:")
                                .size(11.0)
                                .color(Color32::from_rgb(0x98, 0xA8, 0xB8)),
                        );
                        ui.add(egui::Slider::new(&mut self.draft_area_y_pct, 15.0..=85.0));
                    });

                    ui.add_space(4.0);
                    match self.area_tab {
                        MediaAreaEditorKind::Reaction => {
                            ui.horizontal(|ui| {
                                for &emoji in COMPOSER_REACTION_PRESETS {
                                    let active = self.draft_reaction_emoji == emoji;
                                    if ui.selectable_label(active, emoji).clicked() {
                                        self.draft_reaction_emoji = emoji.to_owned();
                                    }
                                }
                                ui.checkbox(&mut self.draft_reaction_dark, "Dark");
                                ui.checkbox(&mut self.draft_reaction_flipped, "Flip");
                                if ui.button("Add Reaction").clicked() {
                                    let area = StoryArea::from_tl_coords(
                                        f64::from(self.draft_area_x_pct),
                                        f64::from(self.draft_area_y_pct),
                                        26.0,
                                        13.0,
                                        0.0,
                                        Some(16.0),
                                    );
                                    self.areas.suggested_reactions.push(SuggestedReaction {
                                        area,
                                        reaction: ReactionId::Emoji(
                                            self.draft_reaction_emoji.clone(),
                                        ),
                                        count: 0,
                                        flipped: self.draft_reaction_flipped,
                                        dark: self.draft_reaction_dark,
                                    });
                                }
                            });
                        }
                        MediaAreaEditorKind::Link => {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    Vec2::new((ui.available_width() - 90.0).max(120.0), 24.0),
                                    TextEdit::singleline(&mut self.draft_url)
                                        .background_color(Color32::from_rgb(0x12, 0x18, 0x1F))
                                        .text_color(Color32::WHITE)
                                        .hint_text("https://example.org"),
                                );
                                if ui.button("Add Link").clicked() {
                                    let url = self.draft_url.trim().to_owned();
                                    if !url.is_empty() {
                                        let area = StoryArea::from_tl_coords(
                                            f64::from(self.draft_area_x_pct),
                                            f64::from(self.draft_area_y_pct),
                                            56.0,
                                            8.0,
                                            0.0,
                                            Some(12.0),
                                        );
                                        self.areas.url_areas.push(UrlArea { area, url });
                                        self.draft_url.clear();
                                    }
                                }
                            });
                        }
                        MediaAreaEditorKind::Location => {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    Vec2::new(130.0, 24.0),
                                    TextEdit::singleline(&mut self.draft_location_title)
                                        .background_color(Color32::from_rgb(0x12, 0x18, 0x1F))
                                        .text_color(Color32::WHITE)
                                        .hint_text("Place title"),
                                );
                                ui.add_sized(
                                    Vec2::new(110.0, 24.0),
                                    TextEdit::singleline(&mut self.draft_location_city)
                                        .background_color(Color32::from_rgb(0x12, 0x18, 0x1F))
                                        .text_color(Color32::WHITE)
                                        .hint_text("City"),
                                );
                                if ui.button("Add Location").clicked() {
                                    let title = self.draft_location_title.trim().to_owned();
                                    if !title.is_empty() {
                                        let area = StoryArea::from_tl_coords(
                                            f64::from(self.draft_area_x_pct),
                                            f64::from(self.draft_area_y_pct),
                                            52.0,
                                            8.5,
                                            0.0,
                                            Some(14.0),
                                        );
                                        self.areas.locations.push(StoryLocation {
                                            area,
                                            point: GeoPoint {
                                                lat: self.draft_location_lat,
                                                lon: self.draft_location_lon,
                                                accuracy_radius: Some(50),
                                            },
                                            title,
                                            city: self.draft_location_city.trim().to_owned(),
                                            ..StoryLocation::default()
                                        });
                                        self.draft_location_title.clear();
                                        self.draft_location_city.clear();
                                    }
                                }
                            });
                        }
                        MediaAreaEditorKind::Weather => {
                            ui.horizontal(|ui| {
                                for &(emoji, color) in COMPOSER_WEATHER_PRESETS {
                                    let active = self.draft_weather_emoji == emoji;
                                    if ui.selectable_label(active, emoji).clicked() {
                                        self.draft_weather_emoji = emoji.to_owned();
                                        self.draft_weather_color = color;
                                    }
                                }
                                ui.add(
                                    egui::DragValue::new(&mut self.draft_weather_celsius)
                                        .range(-40.0..=55.0)
                                        .suffix("°C"),
                                );
                                if ui.button("Add Weather").clicked() {
                                    let area = StoryArea::from_tl_coords(
                                        f64::from(self.draft_area_x_pct),
                                        f64::from(self.draft_area_y_pct),
                                        34.0,
                                        8.0,
                                        0.0,
                                        Some(14.0),
                                    );
                                    self.areas.weather_areas.push(WeatherArea {
                                        area,
                                        emoji: self.draft_weather_emoji.clone(),
                                        color: self.draft_weather_color,
                                        millicelsius: (self.draft_weather_celsius * 1000.0).round()
                                            as i32,
                                    });
                                }
                            });
                        }
                    }
                }
            });
    }
}

fn paint_mini_area_dot(painter: &egui::Painter, preview: Rect, area: StoryArea, color: Color32) {
    let cx = preview.left()
        + ((area.geometry.x + area.geometry.width * 0.5) as f32).clamp(0.05, 0.95)
            * preview.width();
    let cy = preview.top()
        + ((area.geometry.y + area.geometry.height * 0.5) as f32).clamp(0.05, 0.95)
            * preview.height();
    painter.circle_filled(Pos2::new(cx, cy), 4.0, color);
    painter.circle_stroke(Pos2::new(cx, cy), 4.0, Stroke::new(1.0, Color32::WHITE));
}

fn paint_modal_backdrop(ctx: &egui::Context, viewport: Rect, id_salt: &str) -> bool {
    let mut clicked = false;
    egui::Area::new(Id::unique(id_salt))
        .order(egui::Order::Middle)
        .fade_in(false)
        .fixed_pos(viewport.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(viewport.size(), Sense::click());
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 145));
            if resp.clicked() {
                clicked = true;
            }
        });
    clicked
}

fn pointer_inside_rect(ctx: &egui::Context, rect: Rect) -> bool {
    ctx.input(|i| {
        i.pointer
            .interact_pos()
            .or_else(|| i.pointer.latest_pos())
            .is_some_and(|p| rect.contains(p))
    })
}

fn modal_card_pos(viewport: Rect, card_w: f32) -> Pos2 {
    let outer_w = card_w + 28.0;
    let x = (viewport.center().x - outer_w * 0.5).max(viewport.left() + 8.0);
    let y = (viewport.top() + 16.0).min((viewport.bottom() - 120.0).max(viewport.top() + 8.0));
    Pos2::new(x, y)
}

fn story_modal_frame() -> Frame {
    Frame::NONE
        .fill(Color32::from_rgb(0x1E, 0x26, 0x30))
        .stroke(Stroke::new(1.0, Color32::from_rgb(0x32, 0x3E, 0x4C)))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
}

fn close_x_button(ui: &mut Ui) -> bool {
    ui.add(
        egui::Button::new(
            RichText::new("×")
                .size(18.0)
                .color(Color32::from_rgb(0xB8, 0xC6, 0xD4)),
        )
        .frame(false),
    )
    .on_hover_text("Close")
    .clicked()
}
