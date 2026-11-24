//! Hunspell dictionary manager, Telegram cloud dictionary channel loader, and
//! `ManageDictionariesBox` UI.
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/dictionaries_manager.cpp`
//! - `Telegram/SourceFiles/chat_helpers/spellchecker_common.cpp`
//! - `Telegram/lib_spellcheck/spellcheck/third_party/hunspell_controller.cpp`

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use egui::Ui;
use tg_core::CoreSettings;
use tg_lang::tr;
use tg_ui::fonts::font_id_from_style;
use tg_ui::stand_in::{Colors, InputFieldStyle, RoundButtonStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::input_field::InputField;

use crate::settings::widgets::{settings_card, settings_toggle_row};

pub use tg_app_core::core_types::dto::dictionaries::*;

/// Persistent state for `Settings -> Advanced -> Manage dictionaries` (`ManageDictionariesBox`).
#[derive(Debug, Clone)]
pub struct ManageDictionariesState {
    /// Live search query (clamped to [`MAX_DICTIONARY_QUERY_LEN`]).
    pub query: String,
    /// Combined system + cloud manifest entries (system entries first, deduplicated by `id`).
    pub entries: Vec<DictionaryEntry>,
    /// Per-dictionary current download/activation state.
    pub states: BTreeMap<i32, DictionaryState>,
    /// Pending enabled dictionary IDs (written to `CoreSettings::dictionaries_enabled` on Save).
    pub enabled_ids: Vec<i32>,
    /// Whether the manifest has been loaded at least once.
    pub manifest_loaded: bool,
    /// Last manifest or download error banner, if any.
    pub last_error: Option<String>,
    /// Root directory where downloaded Hunspell dictionaries are stored (`tdata/dictionaries`).
    pub dictionaries_dir: PathBuf,
}

impl Default for ManageDictionariesState {
    fn default() -> Self {
        Self::new(PathBuf::from("tdata/dictionaries"))
    }
}

impl ManageDictionariesState {
    /// Creates a new manager rooted at `dictionaries_dir`.
    #[must_use]
    pub fn new(dictionaries_dir: PathBuf) -> Self {
        Self {
            query: String::new(),
            entries: Vec::new(),
            states: BTreeMap::new(),
            enabled_ids: Vec::new(),
            manifest_loaded: false,
            last_error: None,
            dictionaries_dir,
        }
    }

    /// Populates system dictionaries (from WinRT `ISpellChecker`) and cloud manifest dictionaries
    /// (from `source`), preserving `core_enabled` states.
    pub fn refresh(
        &mut self,
        system_bcp47_tags: &[String],
        core_enabled: &[i32],
        source: &mut dyn DictionaryCloudSource,
    ) -> Result<(), String> {
        self.enabled_ids = core_enabled.to_vec();
        let mut combined = Vec::new();
        let mut seen = HashSet::new();

        // System rows go first (`Inner::setupContent` in `dictionaries_manager.cpp:373-382`).
        for tag in system_bcp47_tags {
            let id = lang_id_from_bcp47(tag);
            if seen.insert(id) {
                combined.push(DictionaryEntry {
                    id,
                    locale_code: tag.replace('-', "_"),
                    name: format!("{tag} (System)"),
                    size_bytes: 0,
                    channel: String::new(),
                    post_id: 0,
                    is_system: true,
                });
            }
        }

        match source.fetch_manifest() {
            Ok(cloud) => {
                self.last_error = None;
                self.manifest_loaded = true;
                for dict in cloud {
                    if seen.insert(dict.id) {
                        combined.push(dict);
                    }
                }
            }
            Err(err) => {
                self.last_error = Some(err.clone());
                self.entries = combined;
                self.recompute_states();
                return Err(err);
            }
        }

        self.entries = combined;
        self.recompute_states();
        Ok(())
    }

    /// Recomputes each row's [`DictionaryState`] from disk + system availability + `enabled_ids`.
    pub fn recompute_states(&mut self) {
        for entry in &self.entries {
            let exists = entry.is_system
                || is_dictionary_downloaded(
                    &self.dictionaries_dir,
                    entry.id,
                    Some(&entry.locale_code),
                );
            let enabled = exists && self.enabled_ids.contains(&entry.id);
            let state = if !exists {
                DictionaryState::Available {
                    size_bytes: entry.size_bytes,
                }
            } else if enabled {
                DictionaryState::Active
            } else {
                DictionaryState::Ready
            };
            self.states.insert(entry.id, state);
        }
    }

    /// Toggles dictionary `lang_id` on or off; downloads & unpacks from `source` when toggling on
    /// a cloud dictionary that is not yet on disk.
    pub fn set_enabled(
        &mut self,
        lang_id: i32,
        enable: bool,
        source: &mut dyn DictionaryCloudSource,
    ) -> Result<(), String> {
        let Some(entry) = self.entries.iter().find(|e| e.id == lang_id).cloned() else {
            return Err(format!("Unknown dictionary ID {lang_id}."));
        };

        if !enable {
            self.enabled_ids.retain(|&id| id != lang_id);
            let exists = entry.is_system
                || is_dictionary_downloaded(
                    &self.dictionaries_dir,
                    lang_id,
                    Some(&entry.locale_code),
                );
            self.states.insert(
                lang_id,
                if exists {
                    DictionaryState::Ready
                } else {
                    DictionaryState::Available {
                        size_bytes: entry.size_bytes,
                    }
                },
            );
            return Ok(());
        }

        let exists = entry.is_system
            || is_dictionary_downloaded(&self.dictionaries_dir, lang_id, Some(&entry.locale_code));
        if !exists {
            self.states.insert(
                lang_id,
                DictionaryState::Loading {
                    loaded_bytes: 0,
                    total_bytes: entry.size_bytes,
                },
            );
            match source.download_dictionary_archive(&entry.channel, entry.post_id) {
                Ok(zip_bytes) => {
                    unpack_dictionary_zip(
                        &zip_bytes,
                        &self.dictionaries_dir,
                        lang_id,
                        Some(&entry.locale_code),
                    )?;
                    let _ = write_dictionaries_rev(&self.dictionaries_dir);
                }
                Err(err) => {
                    self.states
                        .insert(lang_id, DictionaryState::Failed(err.clone()));
                    return Err(err);
                }
            }
        }

        if !self.enabled_ids.contains(&lang_id) {
            self.enabled_ids.push(lang_id);
        }
        self.states.insert(lang_id, DictionaryState::Active);
        Ok(())
    }

    /// Deletes a downloaded dictionary from disk and disables it (`RemoveDictionary`).
    pub fn remove_downloaded(&mut self, lang_id: i32) -> Result<bool, String> {
        let entry = self.entries.iter().find(|e| e.id == lang_id).cloned();
        let locale = entry.as_ref().map(|e| e.locale_code.as_str());
        let removed = remove_dictionary(&self.dictionaries_dir, lang_id, locale)?;
        self.enabled_ids.retain(|&id| id != lang_id);
        if let Some(entry) = entry {
            self.states.insert(
                lang_id,
                DictionaryState::Available {
                    size_bytes: entry.size_bytes,
                },
            );
        }
        Ok(removed)
    }

    /// Filters [`Self::entries`] by `self.query` (case-insensitive prefix/substring match).
    #[must_use]
    pub fn filtered_entries(&self) -> Vec<&DictionaryEntry> {
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return self.entries.iter().collect();
        }
        self.entries
            .iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&q) || e.locale_code.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Returns only those enabled IDs that actually exist on disk or via the system spellchecker
    /// (`FilterEnabledDict` in `dictionaries_manager.cpp:68-72`).
    #[must_use]
    pub fn committed_enabled_ids(&self) -> Vec<i32> {
        self.enabled_ids
            .iter()
            .copied()
            .filter(|&id| {
                self.entries.iter().any(|e| {
                    e.id == id
                        && (e.is_system
                            || is_dictionary_downloaded(
                                &self.dictionaries_dir,
                                id,
                                Some(&e.locale_code),
                            ))
                })
            })
            .collect()
    }

    /// Commits enabled dictionary IDs into `core.dictionaries_enabled`.
    pub fn save_to_core_settings(&self, core: &mut CoreSettings) {
        core.dictionaries_enabled = self.committed_enabled_ids();
    }
}

/// Renders the `Manage dictionaries` panel inside `Settings -> Advanced`
/// (`ManageDictionariesBox` in `boxes/dictionaries_manager.cpp`).
///
/// Returns an optional toast message when the user saves or removes a dictionary.
pub fn render_manage_dictionaries_panel(
    ui: &mut Ui,
    state: &mut ManageDictionariesState,
    system_langs: &[String],
    core: &mut CoreSettings,
    source: &mut dyn DictionaryCloudSource,
    colors: &dyn Colors,
) -> Option<String> {
    if !state.manifest_loaded {
        let _ = state.refresh(system_langs, &core.dictionaries_enabled, source);
    }

    let mut toast = None;
    settings_card(ui, colors, |ui| {
        ui.label(
            egui::RichText::new(tr::lng_settings_manage_dictionaries())
                .font(font_id_from_style(st::semibold_font))
                .color(colors.color("windowBoldFg")),
        );
        ui.add_space(6.0);

        let avail = ui.available_width().max(180.0);
        InputField::single_line(
            "manage_dictionaries_search",
            &mut state.query,
            "Search dictionaries...",
            InputFieldStyle::rounded_search(colors),
        )
        .width(avail)
        .show(ui);
        if state.query.chars().count() > MAX_DICTIONARY_QUERY_LEN {
            state.query = state.query.chars().take(MAX_DICTIONARY_QUERY_LEN).collect();
        }

        if let Some(ref err) = state.last_error {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(err)
                    .font(font_id_from_style(st::normal_font))
                    .color(colors.color("attentionButtonFg")),
            );
        }

        ui.add_space(6.0);
        let visible: Vec<DictionaryEntry> = state.filtered_entries().into_iter().cloned().collect();
        let mut toggle_action: Option<(i32, bool)> = None;
        let mut remove_action: Option<i32> = None;

        for entry in &visible {
            let row_state =
                state
                    .states
                    .get(&entry.id)
                    .cloned()
                    .unwrap_or(DictionaryState::Available {
                        size_bytes: entry.size_bytes,
                    });
            let mut active = matches!(
                row_state,
                DictionaryState::Active | DictionaryState::Loading { .. }
            );
            let sub = row_state.description();
            if settings_toggle_row(ui, &entry.name, Some(&sub), None, &mut active, true, colors) {
                toggle_action = Some((entry.id, active));
            }
            if !entry.is_system
                && is_dictionary_downloaded(
                    &state.dictionaries_dir,
                    entry.id,
                    Some(&entry.locale_code),
                )
            {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    if ui.small_button("Remove downloaded dictionary").clicked() {
                        remove_action = Some(entry.id);
                    }
                });
            }
        }

        if let Some((lang_id, enable)) = toggle_action {
            if let Err(err) = state.set_enabled(lang_id, enable, source) {
                toast = Some(err);
            } else {
                state.save_to_core_settings(core);
            }
        }
        if let Some(lang_id) = remove_action {
            match state.remove_downloaded(lang_id) {
                Ok(_) => {
                    state.save_to_core_settings(core);
                    toast = Some("Dictionary removed.".to_owned());
                }
                Err(err) => toast = Some(err),
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if RoundButton::new(
                &tr::lng_settings_save(),
                RoundButtonStyle::default_active(colors),
            )
            .show(ui)
            .clicked()
            {
                state.save_to_core_settings(core);
                toast = Some(format!(
                    "Saved {} enabled dictionaries.",
                    core.dictionaries_enabled.len()
                ));
            }
            if RoundButton::new("Refresh list", RoundButtonStyle::default_light(colors))
                .show(ui)
                .clicked()
            {
                if let Err(err) = state.refresh(system_langs, &core.dictionaries_enabled, source) {
                    toast = Some(err);
                } else {
                    toast = Some("Dictionary list refreshed.".to_owned());
                }
            }
        });
    });

    toast
}
