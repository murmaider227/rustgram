//! Language picker modal box with live search and radio selection.
//!
//! Port origin: `Telegram/SourceFiles/boxes/language_box.h`
//! and `Telegram/SourceFiles/boxes/language_box.cpp`.

use egui::{Align2, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, InputFieldStyle, RadioStyle};
use tg_ui::tg_style::st;
use tg_ui::widgets::checkbox::paint_radio_view;
use tg_ui::widgets::input_field::InputField;

use crate::boxes::strings;
use crate::info::backend::{InfoBackend, LanguageEntry};

/// Stateful Language Picker modal (`LanguageBox`).
#[derive(Debug, Clone)]
pub struct LanguageBoxState {
    /// Filter query matching native name, English name, or code.
    pub search_query: String,
    /// Currently chosen language pack ID (e.g. `"en"`).
    pub selected_lang_id: String,
}

impl LanguageBoxState {
    /// Initializes from the backend's active language code.
    #[must_use]
    pub fn new(active_lang_id: impl Into<String>) -> Self {
        Self {
            search_query: String::new(),
            selected_lang_id: active_lang_id.into(),
        }
    }

    /// Filters languages by [`Self::search_query`].
    #[must_use]
    pub fn filter_languages<'a>(&self, languages: &'a [LanguageEntry]) -> Vec<&'a LanguageEntry> {
        let q = self.search_query.trim().to_lowercase();
        languages
            .iter()
            .filter(|lang| {
                q.is_empty()
                    || lang.name.to_lowercase().contains(&q)
                    || lang.native_name.to_lowercase().contains(&q)
                    || lang.id.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Applies the selected language to `backend`.
    pub fn apply(&self, backend: &mut dyn InfoBackend) -> Result<(), String> {
        backend.set_language(&self.selected_lang_id)
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, tr::lng_languages())
            .width(st::box_wide_width.0 as f32)
            .add_button(BoxButton::default_btn("close", tr::lng_close()))
    }

    /// Renders the search bar and radio list of languages; returns `Some(lang_id)` when selection changes.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        languages: &[LanguageEntry],
        colors: &dyn Colors,
    ) -> Option<String> {
        InputField::single_line(
            "lang_box_search",
            &mut self.search_query,
            strings::SEARCH_LANGUAGES_PH,
            InputFieldStyle::rounded_search(colors),
        )
        .show(ui);
        ui.add_space(8.0);

        let filtered: Vec<LanguageEntry> = self
            .filter_languages(languages)
            .into_iter()
            .cloned()
            .collect();
        if filtered.is_empty() {
            let (r, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::hover());
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                tr::lng_languages_none(),
                font_id_from_style(st::normal_font),
                colors.color("windowSubTextFg"),
            );
            return None;
        }

        let rad_st = RadioStyle::default_radio(colors);
        let mut changed = None;
        for lang in &filtered {
            let selected = self.selected_lang_id == lang.id;
            let width = ui.available_width().max(180.0);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 48.0), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(6), colors.color("windowBgOver"));
            }

            let radio_rect = Rect::from_center_size(
                Pos2::new(rect.min.x + 18.0, rect.center().y),
                Vec2::splat(rad_st.diameter),
            );
            paint_radio_view(
                ui.painter(),
                radio_rect,
                &rad_st,
                if selected { 1.0 } else { 0.0 },
                1.0,
            );

            let tx = radio_rect.max.x + 12.0;
            ui.painter().text(
                Pos2::new(tx, rect.min.y + 7.0),
                Align2::LEFT_TOP,
                &lang.native_name,
                font_id_from_style(st::semibold_font),
                colors.color("windowBoldFg"),
            );
            let subtitle = if lang.progress_percent < 100 {
                format!("{} · {}%", lang.name, lang.progress_percent)
            } else {
                lang.name.clone()
            };
            ui.painter().text(
                Pos2::new(tx, rect.min.y + 26.0),
                Align2::LEFT_TOP,
                subtitle,
                FontId::new(12.0, family_regular()),
                colors.color("windowSubTextFg"),
            );

            if resp.clicked() && !selected {
                self.selected_lang_id = lang.id.clone();
                changed = Some(lang.id.clone());
            }
        }
        changed
    }
}
