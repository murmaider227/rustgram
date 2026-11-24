//! Field autocomplete popup UI (`@mention`, `#hashtag`, `/command`, `:emoji` suggestions,
//! and single-emoji sticker suggestions) layered over `tg-chat`'s autocomplete engine.
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/field_autocomplete.h`,
//! `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp`, and
//! `Telegram/SourceFiles/chat_helpers/emoji_suggestions_widget.cpp`.

use egui::{
    Align2, Area, Color32, CornerRadius, FontId, Frame, Id, Key, Margin, Order, Pos2, Rect,
    ScrollArea, Sense, Stroke, Vec2,
};
use tg_chat::autocomplete::{
    AutocompleteKind, AutocompletePeerContext, AutocompleteQuery, BotCommandChoiceAction,
    BotCommandRow, ChooseMethod, InsertionResult, MentionRow, apply_bot_command_choice,
    apply_hashtag_insertion, apply_mention_insertion, filter_bot_commands, filter_mentions,
    parse_cursor_autocomplete_query,
};
use tg_chat::emoji::{
    EmojiKeywordResult, EmojiSuggestionConfig, EmojiSuggestionQuery,
    apply_emoji_suggestion_replacement, parse_emoji_suggestion_query,
};
use tg_chat::{BotStatus, ComposeFeatures, DocumentId, UserInfo};
use tg_emoji::EmojiTextureCache;
use tg_lang::tr;
use tg_text::emoji as tg_emoji_db;
use tg_ui::fonts::ensure_tdesktop_fonts;
use tg_ui::tg_style::st;

use crate::chat_helpers::backend::ChatHelpersBackend;
use crate::chat_helpers::paint_helper_icon;
use crate::chat_helpers::stickers_list_widget::{StickerMediaCache, paint_premium_lock_badge};
use crate::chat_helpers::strings;
use tg_app_core::stand_in::theme::Palette;

/// Row height for `@mention`, `#hashtag`, and `/command` suggestions (`st::mentionHeight`).
pub const AUTOCOMPLETE_ROW_HEIGHT: f32 = st::mention_height.0 as f32;
/// Maximum popup height (`4.5 * st::mentionHeight = 180px`).
pub const AUTOCOMPLETE_MAX_HEIGHT: f32 = AUTOCOMPLETE_ROW_HEIGHT * 4.5;
/// Opacity fade-in/out duration (`st::emojiPanDuration`).
pub const AUTOCOMPLETE_ANIM_SECS: f64 = st::emoji_pan_duration as f64 / 1000.0;

/// Active suggestion rows computed from the compose field's text and cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutocompleteRows {
    /// No active autocomplete suggestions.
    None,
    /// `@mention` user rows.
    Mentions {
        /// Parsed trigger query.
        query: AutocompleteQuery,
        /// Ranked mention rows.
        rows: Vec<MentionRow>,
    },
    /// `#hashtag` rows.
    Hashtags {
        /// Parsed trigger query.
        query: AutocompleteQuery,
        /// Filtered recent hashtags (without leading `#`).
        rows: Vec<String>,
    },
    /// `/command` rows.
    BotCommands {
        /// Parsed trigger query.
        query: AutocompleteQuery,
        /// Filtered bot commands.
        rows: Vec<BotCommandRow>,
    },
    /// `:emoji` shortcode / keyword suggestions (e.g. typing `:smile`).
    Emoji {
        /// Parsed emoji suggestion trigger.
        query: EmojiSuggestionQuery,
        /// Ranked emoji keyword suggestions.
        rows: Vec<EmojiKeywordResult>,
    },
    /// Sticker suggestions matching a single emoji typed in the compose field.
    Stickers {
        /// Matched emoji string.
        emoji: String,
        /// Suggested sticker document IDs.
        docs: Vec<DocumentId>,
    },
}

impl AutocompleteRows {
    /// Returns the number of selectable items in the current popup.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Mentions { rows, .. } => rows.len(),
            Self::Hashtags { rows, .. } => rows.len(),
            Self::BotCommands { rows, .. } => rows.len(),
            Self::Emoji { rows, .. } => rows.len(),
            Self::Stickers { docs, .. } => docs.len(),
        }
    }

    /// Returns `true` if there are no active suggestions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Action returned when the user accepts an autocomplete suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutocompleteAction {
    /// Replace the compose field text & cursor position (mentions, hashtags, Tab-completed commands, `:emoji`).
    ReplaceText(InsertionResult),
    /// Immediately send a chosen bot command (`Enter` or `Click` on `/command`).
    SendBotCommand {
        /// Formatted command (e.g. `"/help@novabot"`).
        command: String,
        /// Any remaining text after the cursor.
        remaining_text: String,
    },
    /// Send a sticker chosen from the single-emoji sticker suggestion bar.
    SendSticker(DocumentId),
    /// User clicked a Premium-locked sticker suggestion without Premium.
    PremiumRequired(DocumentId),
}

/// Stateful field autocomplete popup view (`ChatHelpers::FieldAutocomplete`).
///
/// The history compose field calls [`FieldAutocompleteView::update_from_input`] whenever text or
/// cursor changes, [`FieldAutocompleteView::handle_keyboard`] before submitting Enter/Tab/Arrows,
/// and [`FieldAutocompleteView::show`] each frame to paint the popup directly above the compose bar.
#[derive(Debug)]
pub struct FieldAutocompleteView {
    rows: AutocompleteRows,
    selected: usize,
    dismissed_query: Option<String>,
    visible_since: Option<f64>,
    features: ComposeFeatures,
    sticker_cache: StickerMediaCache,
}

impl Default for FieldAutocompleteView {
    fn default() -> Self {
        Self::new()
    }
}

impl FieldAutocompleteView {
    /// Creates a new [`FieldAutocompleteView`] with all standard compose autocomplete features enabled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: AutocompleteRows::None,
            selected: 0,
            dismissed_query: None,
            visible_since: None,
            features: ComposeFeatures::default(),
            sticker_cache: StickerMediaCache::new(),
        }
    }

    /// Overrides the [`ComposeFeatures`] flags controlling which autocomplete triggers are active.
    pub fn set_features(&mut self, features: ComposeFeatures) {
        self.features = features;
    }

    /// Returns the currently active [`AutocompleteRows`].
    #[must_use]
    pub const fn rows(&self) -> &AutocompleteRows {
        &self.rows
    }

    /// Returns the currently highlighted suggestion index (`0..rows.len()`).
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// Returns `true` if the autocomplete popup currently has visible suggestions.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        !self.rows.is_empty()
    }

    /// Dismisses the popup for the current query (e.g. on Escape).
    pub fn dismiss(&mut self) {
        self.dismissed_query = self.current_raw_query();
        self.rows = AutocompleteRows::None;
        self.selected = 0;
        self.visible_since = None;
    }

    fn current_raw_query(&self) -> Option<String> {
        match &self.rows {
            AutocompleteRows::None => None,
            AutocompleteRows::Mentions { query, .. }
            | AutocompleteRows::Hashtags { query, .. }
            | AutocompleteRows::BotCommands { query, .. } => Some(query.query.clone()),
            AutocompleteRows::Emoji { query, .. } => Some(query.query.clone()),
            AutocompleteRows::Stickers { emoji, .. } => Some(emoji.clone()),
        }
    }

    /// Re-evaluates autocomplete suggestions for `(text, cursor_byte, has_selection)` using
    /// `tg-chat`'s parsing and ranking functions.
    pub fn update_from_input(
        &mut self,
        text: &str,
        cursor_byte: usize,
        has_selection: bool,
        backend: &mut dyn ChatHelpersBackend,
    ) {
        let cursor = cursor_byte.min(text.len());
        let was_empty = self.rows.is_empty();

        let next_rows = self.compute_rows(text, cursor, has_selection, backend);

        // Respect Escape dismissal until the query string changes.
        let next_raw = match &next_rows {
            AutocompleteRows::None => None,
            AutocompleteRows::Mentions { query, .. }
            | AutocompleteRows::Hashtags { query, .. }
            | AutocompleteRows::BotCommands { query, .. } => Some(query.query.as_str()),
            AutocompleteRows::Emoji { query, .. } => Some(query.query.as_str()),
            AutocompleteRows::Stickers { emoji, .. } => Some(emoji.as_str()),
        };
        if let (Some(dismissed), Some(raw)) = (self.dismissed_query.as_deref(), next_raw)
            && dismissed == raw
        {
            self.rows = AutocompleteRows::None;
            return;
        }
        if next_raw.is_none() || self.dismissed_query.as_deref() != next_raw {
            self.dismissed_query = None;
        }

        self.rows = next_rows;
        if self.rows.is_empty() {
            self.selected = 0;
            self.visible_since = None;
        } else {
            self.selected = self.selected.min(self.rows.len() - 1);
            if was_empty {
                self.selected = 0;
            }
        }
    }

    fn compute_rows(
        &self,
        text: &str,
        cursor_byte: usize,
        has_selection: bool,
        backend: &mut dyn ChatHelpersBackend,
    ) -> AutocompleteRows {
        // 1. Check single-emoji sticker suggestions when the entire field is 1 emoji.
        if !has_selection && self.features.suggest_stickers_by_emoji && cursor_byte == text.len() {
            let trimmed = text.trim();
            if !trimmed.is_empty()
                && let Some((er, matched)) = tg_emoji_db::find_emoji(trimmed, 0)
                && matched == trimmed.len()
            {
                let docs = backend.stickers_for_emoji(er.non_colored_id());
                if !docs.is_empty() {
                    return AutocompleteRows::Stickers {
                        emoji: er.text().into_owned(),
                        docs,
                    };
                }
            }
        }

        // 2. Parse `@mention`, `#hashtag`, `/command`, or `:emoji` under cursor via `tg-chat`.
        if let Some(q) =
            parse_cursor_autocomplete_query(text, cursor_byte, 0, has_selection, self.features, 32)
        {
            match q.kind {
                AutocompleteKind::Mentions => {
                    let rows = self.resolve_mentions(&q, backend);
                    if !rows.is_empty() {
                        return AutocompleteRows::Mentions { query: q, rows };
                    }
                }
                AutocompleteKind::Hashtags => {
                    let rows = backend.recent_hashtags().filter(&q.query);
                    if !rows.is_empty() {
                        return AutocompleteRows::Hashtags { query: q, rows };
                    }
                }
                AutocompleteKind::BotCommands => {
                    let rows = self.resolve_bot_commands(&q, backend);
                    if !rows.is_empty() {
                        return AutocompleteRows::BotCommands { query: q, rows };
                    }
                }
                AutocompleteKind::EmojiKeyword => {
                    let cfg = EmojiSuggestionConfig::default();
                    if let Some(eq) =
                        parse_emoji_suggestion_query(text, cursor_byte, 0, has_selection, cfg)
                    {
                        let rows = backend.search_emoji_suggestions(&eq.keyword, eq.exact);
                        if !rows.is_empty() {
                            return AutocompleteRows::Emoji { query: eq, rows };
                        }
                    }
                }
                AutocompleteKind::Stickers => {}
            }
        }

        // 3. Also check exact first-word emoji suggestion (e.g. typing `"fire"` as the sole word).
        let cfg = EmojiSuggestionConfig::default();
        if let Some(eq) = parse_emoji_suggestion_query(text, cursor_byte, 0, has_selection, cfg) {
            let rows = backend.search_emoji_suggestions(&eq.keyword, eq.exact);
            if !rows.is_empty() {
                return AutocompleteRows::Emoji { query: eq, rows };
            }
        }

        AutocompleteRows::None
    }

    fn resolve_mentions(
        &self,
        q: &AutocompleteQuery,
        backend: &dyn ChatHelpersBackend,
    ) -> Vec<MentionRow> {
        let users = backend.autocomplete_users();
        let participants: Vec<&UserInfo> = users.iter().collect();
        let last_authors: Vec<&UserInfo> = backend
            .autocomplete_last_authors()
            .iter()
            .filter_map(|id| users.iter().find(|u| u.id == *id))
            .collect();
        let inline_bots: Vec<&UserInfo> = backend
            .recent_inline_bot_ids()
            .iter()
            .filter_map(|id| users.iter().find(|u| u.id == *id))
            .collect();
        let peer = AutocompletePeerContext::BasicChat {
            participants: &participants,
            last_authors: &last_authors,
            bot_status: BotStatus::HasBots,
            bot_commands: backend.autocomplete_bot_commands(),
        };
        filter_mentions(
            &peer,
            &q.query,
            q.from_start && self.features.inline_bots,
            &inline_bots,
            &[],
            1_730_000_000,
        )
    }

    fn resolve_bot_commands(
        &self,
        q: &AutocompleteQuery,
        backend: &dyn ChatHelpersBackend,
    ) -> Vec<BotCommandRow> {
        let users = backend.autocomplete_users();
        let participants: Vec<&UserInfo> = users.iter().collect();
        let last_authors: Vec<&UserInfo> = backend
            .autocomplete_last_authors()
            .iter()
            .filter_map(|id| users.iter().find(|u| u.id == *id))
            .collect();
        let peer = AutocompletePeerContext::BasicChat {
            participants: &participants,
            last_authors: &last_authors,
            bot_status: BotStatus::HasBots,
            bot_commands: backend.autocomplete_bot_commands(),
        };
        filter_bot_commands(&peer, &q.query)
    }

    /// Moves keyboard selection up/down (or left/right for horizontal strips) and handles
    /// `Enter`, `Tab`, and `Escape`.
    pub fn handle_keyboard(
        &mut self,
        ctx: &egui::Context,
        text: &str,
        cursor_byte: usize,
        backend: &mut dyn ChatHelpersBackend,
    ) -> Option<AutocompleteAction> {
        if self.rows.is_empty() {
            return None;
        }
        ensure_tdesktop_fonts(ctx);
        let count = self.rows.len();

        let (up, down, left, right, enter, tab, esc) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Tab),
                i.key_pressed(Key::Escape),
            )
        });

        if esc {
            self.dismiss();
            return None;
        }

        let is_horizontal = matches!(self.rows, AutocompleteRows::Stickers { .. });
        if up || (is_horizontal && left) {
            self.selected = if self.selected == 0 {
                count - 1
            } else {
                self.selected - 1
            };
            return None;
        }
        if down || (is_horizontal && right) {
            self.selected = (self.selected + 1) % count;
            return None;
        }

        if enter {
            return self.choose_index(
                self.selected,
                ChooseMethod::ByEnter,
                false,
                text,
                cursor_byte,
                backend,
            );
        }
        if tab {
            return self.choose_index(
                self.selected,
                ChooseMethod::ByTab,
                false,
                text,
                cursor_byte,
                backend,
            );
        }
        None
    }

    /// Applies selection of row `index` using `method` and returns the resulting [`AutocompleteAction`].
    pub fn choose_index(
        &mut self,
        index: usize,
        method: ChooseMethod,
        ctrl_pressed: bool,
        text: &str,
        cursor_byte: usize,
        backend: &mut dyn ChatHelpersBackend,
    ) -> Option<AutocompleteAction> {
        let cursor = cursor_byte.min(text.len());
        match &self.rows {
            AutocompleteRows::None => None,
            AutocompleteRows::Mentions { rows, .. } => {
                let row = rows.get(index)?;
                let user = backend
                    .autocomplete_users()
                    .iter()
                    .find(|u| u.id == row.user_id)?
                    .clone();
                let res = apply_mention_insertion(
                    text,
                    cursor,
                    0,
                    &user,
                    backend.self_user_id(),
                    method,
                    ctrl_pressed,
                );
                self.rows = AutocompleteRows::None;
                Some(AutocompleteAction::ReplaceText(res))
            }
            AutocompleteRows::Hashtags { rows, .. } => {
                let tag = rows.get(index)?.clone();
                let res = apply_hashtag_insertion(text, cursor, 0, &tag);
                backend.record_hashtag(&tag);
                self.rows = AutocompleteRows::None;
                Some(AutocompleteAction::ReplaceText(res))
            }
            AutocompleteRows::BotCommands { rows, .. } => {
                let row = rows.get(index)?.clone();
                let action = apply_bot_command_choice(text, cursor, 0, &row, method);
                self.rows = AutocompleteRows::None;
                match action {
                    BotCommandChoiceAction::Insert(ins) => {
                        Some(AutocompleteAction::ReplaceText(ins))
                    }
                    BotCommandChoiceAction::SendImmediately {
                        command,
                        remaining_text,
                    } => Some(AutocompleteAction::SendBotCommand {
                        command,
                        remaining_text,
                    }),
                    BotCommandChoiceAction::ProcessShortcut { shortcut_name } => {
                        Some(AutocompleteAction::SendBotCommand {
                            command: format!("/{shortcut_name}"),
                            remaining_text: String::new(),
                        })
                    }
                }
            }
            AutocompleteRows::Emoji { query, rows } => {
                let item = rows.get(index)?.clone();
                let (new_text, new_cursor_byte) =
                    apply_emoji_suggestion_replacement(text, query, &item.emoji);
                let res = InsertionResult {
                    text: new_text,
                    new_cursor_byte,
                    replaced_start_byte: query.start_byte,
                    replaced_end_byte: query.end_byte,
                    mention_entity: None,
                };
                backend.record_emoji_used(&item.emoji);
                self.rows = AutocompleteRows::None;
                Some(AutocompleteAction::ReplaceText(res))
            }
            AutocompleteRows::Stickers { docs, .. } => {
                let doc_id = *docs.get(index)?;
                let locked = backend
                    .sticker_document(doc_id)
                    .is_some_and(|d| d.is_premium_sticker)
                    && !backend.is_premium();
                self.rows = AutocompleteRows::None;
                if locked {
                    Some(AutocompleteAction::PremiumRequired(doc_id))
                } else {
                    backend.record_sticker_used(doc_id);
                    Some(AutocompleteAction::SendSticker(doc_id))
                }
            }
        }
    }

    /// Paints the autocomplete popup directly above `compose_rect` when suggestions are active.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        compose_rect: Rect,
        text: &str,
        cursor_byte: usize,
        backend: &mut dyn ChatHelpersBackend,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
    ) -> Option<AutocompleteAction> {
        if self.rows.is_empty() {
            return None;
        }
        ensure_tdesktop_fonts(ctx);

        let now = ctx.input(|i| i.time);
        let start = *self.visible_since.get_or_insert(now);
        let progress = ((now - start) / AUTOCOMPLETE_ANIM_SECS).clamp(0.0, 1.0) as f32;
        if progress < 1.0 {
            ctx.request_repaint();
        }

        let popup_w = compose_rect.width().max(240.0);
        let content_h = match &self.rows {
            AutocompleteRows::Stickers { .. } => 76.0,
            other => (other.len() as f32 * AUTOCOMPLETE_ROW_HEIGHT).min(AUTOCOMPLETE_MAX_HEIGHT),
        };
        let popup_rect = Rect::from_min_size(
            Pos2::new(
                compose_rect.left(),
                (compose_rect.top() - content_h - 1.0).max(8.0),
            ),
            Vec2::new(popup_w, content_h),
        );

        let mut action = None;
        let ctrl_down = ctx.input(|i| i.modifiers.ctrl);
        let rows_snapshot = self.rows.clone();

        Area::new(Id::unique("tg_field_autocomplete_popup"))
            .order(Order::Foreground)
            .fixed_pos(popup_rect.min)
            .show(ctx, |ui| {
                ui.multiply_opacity(progress);

                // Subtle top shadow line (`FieldAutocomplete::Inner::paintShadow`)
                ui.painter().rect_filled(
                    popup_rect
                        .expand2(Vec2::new(0.0, 2.0))
                        .translate(Vec2::new(0.0, -2.0)),
                    CornerRadius {
                        nw: 8,
                        ne: 8,
                        sw: 0,
                        se: 0,
                    },
                    Color32::from_rgba_unmultiplied(0, 0, 0, (24.0 * progress).round() as u8),
                );

                Frame::NONE
                    .fill(palette.window_bg)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius {
                        nw: 8,
                        ne: 8,
                        sw: 0,
                        se: 0,
                    })
                    .show(ui, |ui| {
                        ui.set_min_size(popup_rect.size());
                        ui.set_max_size(popup_rect.size());

                        match &rows_snapshot {
                            AutocompleteRows::Stickers { docs, .. } => {
                                if let Some(chosen_idx) =
                                    self.paint_stickers_strip(ui, docs, now, backend, palette)
                                {
                                    action = self.choose_index(
                                        chosen_idx,
                                        ChooseMethod::ByClick,
                                        ctrl_down,
                                        text,
                                        cursor_byte,
                                        backend,
                                    );
                                }
                            }
                            _ => {
                                ScrollArea::vertical()
                                    .id_salt("tg_field_autocomplete_scroll")
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.y = 0.0;
                                        match &rows_snapshot {
                                            AutocompleteRows::Mentions { rows, .. } => {
                                                let mut remove_bot = None;
                                                for (idx, row) in rows.iter().enumerate() {
                                                    let (clicked, removed) = self
                                                        .paint_mention_row(ui, idx, row, palette);
                                                    if removed {
                                                        remove_bot = Some(row.user_id);
                                                    } else if clicked {
                                                        action = self.choose_index(
                                                            idx,
                                                            ChooseMethod::ByClick,
                                                            ctrl_down,
                                                            text,
                                                            cursor_byte,
                                                            backend,
                                                        );
                                                    }
                                                }
                                                if let Some(id) = remove_bot {
                                                    backend.remove_recent_inline_bot(id);
                                                    self.update_from_input(
                                                        text,
                                                        cursor_byte,
                                                        false,
                                                        backend,
                                                    );
                                                }
                                            }
                                            AutocompleteRows::Hashtags { rows, .. } => {
                                                let mut remove_tag = None;
                                                for (idx, tag) in rows.iter().enumerate() {
                                                    let (clicked, removed) = self
                                                        .paint_hashtag_row(ui, idx, tag, palette);
                                                    if removed {
                                                        remove_tag = Some(tag.clone());
                                                    } else if clicked {
                                                        action = self.choose_index(
                                                            idx,
                                                            ChooseMethod::ByClick,
                                                            ctrl_down,
                                                            text,
                                                            cursor_byte,
                                                            backend,
                                                        );
                                                    }
                                                }
                                                if let Some(tag) = remove_tag {
                                                    backend.remove_hashtag(&tag);
                                                    self.update_from_input(
                                                        text,
                                                        cursor_byte,
                                                        false,
                                                        backend,
                                                    );
                                                }
                                            }
                                            AutocompleteRows::BotCommands { rows, .. } => {
                                                for (idx, row) in rows.iter().enumerate() {
                                                    if self.paint_command_row(ui, idx, row, palette)
                                                    {
                                                        action = self.choose_index(
                                                            idx,
                                                            ChooseMethod::ByClick,
                                                            ctrl_down,
                                                            text,
                                                            cursor_byte,
                                                            backend,
                                                        );
                                                    }
                                                }
                                            }
                                            AutocompleteRows::Emoji { rows, .. } => {
                                                for (idx, item) in rows.iter().enumerate() {
                                                    if self.paint_emoji_row(
                                                        ui,
                                                        idx,
                                                        item,
                                                        emoji_cache,
                                                        palette,
                                                    ) {
                                                        action = self.choose_index(
                                                            idx,
                                                            ChooseMethod::ByClick,
                                                            ctrl_down,
                                                            text,
                                                            cursor_byte,
                                                            backend,
                                                        );
                                                    }
                                                }
                                            }
                                            AutocompleteRows::None
                                            | AutocompleteRows::Stickers { .. } => {}
                                        }
                                    });
                            }
                        }
                    });
            });

        action
    }

    fn paint_mention_row(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        row: &MentionRow,
        palette: &Palette,
    ) -> (bool, bool) {
        let width = ui.available_width().max(160.0);
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(width, AUTOCOMPLETE_ROW_HEIGHT), Sense::click());
        if resp.hovered() {
            self.selected = idx;
        }
        if self.selected == idx {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, palette.window_bg_over);
        }

        // Peer avatar circle
        let avatar_c = Pos2::new(rect.left() + 24.0, rect.center().y);
        ui.painter()
            .circle_filled(avatar_c, 15.0, peer_avatar_color(row.user_id.0));
        let initials = extract_initials(&row.display_name);
        ui.painter().text(
            avatar_c,
            Align2::CENTER_CENTER,
            &initials,
            FontId::proportional(11.5),
            Color32::WHITE,
        );

        // Display name + `@username`
        let text_x = rect.left() + 48.0;
        let name_galley = ui.painter().layout_no_wrap(
            row.display_name.clone(),
            FontId::proportional(13.5),
            palette.window_bold_fg,
        );
        let name_w = name_galley.size().x;
        ui.painter().galley(
            Pos2::new(text_x, rect.center().y - name_galley.size().y * 0.5),
            name_galley,
            palette.window_bold_fg,
        );
        if !row.username.is_empty() {
            ui.painter().text(
                Pos2::new(text_x + name_w + 8.0, rect.center().y),
                Align2::LEFT_CENTER,
                format!("@{}", row.username),
                FontId::proportional(12.5),
                palette.window_sub_text_fg,
            );
        }

        let mut removed = false;
        if row.removable() {
            let dark = ui.ctx().global_style().visuals.dark_mode;
            let del_r = Rect::from_min_size(
                Pos2::new(rect.right() - 32.0, rect.center().y - 11.0),
                Vec2::splat(22.0),
            );
            let del_resp = ui.interact(
                del_r,
                ui.make_persistent_id(("tg_ac_del_mention", idx, row.user_id.0)),
                Sense::click(),
            );
            let fg = if del_resp.hovered() {
                palette.window_fg
            } else {
                palette.window_sub_text_fg
            };
            if !paint_helper_icon(
                ui.painter(),
                del_r.shrink(4.0),
                st::small_close_icon,
                dark,
                Some(fg),
            ) {
                ui.painter().text(
                    del_r.center(),
                    Align2::CENTER_CENTER,
                    "×",
                    FontId::proportional(15.0),
                    fg,
                );
            }
            if del_resp.on_hover_text(strings::REMOVE_RECENT_BOT).clicked() {
                removed = true;
            }
        }

        (resp.clicked() && !removed, removed)
    }

    fn paint_hashtag_row(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        tag: &str,
        palette: &Palette,
    ) -> (bool, bool) {
        let width = ui.available_width().max(160.0);
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(width, AUTOCOMPLETE_ROW_HEIGHT), Sense::click());
        if resp.hovered() {
            self.selected = idx;
        }
        if self.selected == idx {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, palette.window_bg_over);
        }

        let badge_c = Pos2::new(rect.left() + 24.0, rect.center().y);
        ui.painter().circle_filled(badge_c, 13.0, palette.input_bg);
        ui.painter().text(
            badge_c,
            Align2::CENTER_CENTER,
            "#",
            FontId::proportional(14.0),
            palette.window_active_text_fg,
        );

        ui.painter().text(
            Pos2::new(rect.left() + 48.0, rect.center().y),
            Align2::LEFT_CENTER,
            format!("#{tag}"),
            FontId::proportional(13.5),
            palette.window_fg,
        );

        let dark = ui.ctx().global_style().visuals.dark_mode;
        let del_r = Rect::from_min_size(
            Pos2::new(rect.right() - 32.0, rect.center().y - 11.0),
            Vec2::splat(22.0),
        );
        let del_resp = ui.interact(
            del_r,
            ui.make_persistent_id(("tg_ac_del_hash", idx, tag)),
            Sense::click(),
        );
        let fg = if del_resp.hovered() {
            palette.window_fg
        } else {
            palette.window_sub_text_fg
        };
        if !paint_helper_icon(
            ui.painter(),
            del_r.shrink(4.0),
            st::small_close_icon,
            dark,
            Some(fg),
        ) {
            ui.painter().text(
                del_r.center(),
                Align2::CENTER_CENTER,
                "×",
                FontId::proportional(15.0),
                fg,
            );
        }
        let removed = del_resp
            .on_hover_text(strings::REMOVE_RECENT_HASHTAG)
            .clicked();

        (resp.clicked() && !removed, removed)
    }

    fn paint_command_row(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        row: &BotCommandRow,
        palette: &Palette,
    ) -> bool {
        let width = ui.available_width().max(160.0);
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(width, AUTOCOMPLETE_ROW_HEIGHT), Sense::click());
        if resp.hovered() {
            self.selected = idx;
        }
        if self.selected == idx {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, palette.window_bg_over);
        }

        let avatar_c = Pos2::new(rect.left() + 24.0, rect.center().y);
        ui.painter()
            .circle_filled(avatar_c, 14.0, peer_avatar_color(row.user_id.0));
        ui.painter().text(
            avatar_c,
            Align2::CENTER_CENTER,
            "/",
            FontId::proportional(14.0),
            Color32::WHITE,
        );

        let cmd_text = row.formatted_command();
        let cmd_galley = ui.painter().layout_no_wrap(
            cmd_text,
            FontId::proportional(13.5),
            palette.window_active_text_fg,
        );
        let cmd_w = cmd_galley.size().x;
        let text_x = rect.left() + 48.0;
        ui.painter().galley(
            Pos2::new(text_x, rect.center().y - cmd_galley.size().y * 0.5),
            cmd_galley,
            palette.window_active_text_fg,
        );

        if !row.description.is_empty() {
            ui.painter().text(
                Pos2::new(text_x + cmd_w + 10.0, rect.center().y),
                Align2::LEFT_CENTER,
                &row.description,
                FontId::proportional(12.5),
                palette.window_sub_text_fg,
            );
        }

        // Ephemeral bot command indicator pill (matches `FieldAutocomplete::ephemeralIconRect`).
        if row.ephemeral {
            let badge = Rect::from_min_size(
                Pos2::new(rect.right() - 72.0, rect.center().y - 9.0),
                Vec2::new(62.0, 18.0),
            );
            ui.painter()
                .rect_filled(badge, CornerRadius::same(9), palette.input_bg);
            ui.painter().text(
                badge.center(),
                Align2::CENTER_CENTER,
                strings::EPHEMERAL_BADGE,
                FontId::proportional(10.0),
                palette.window_sub_text_fg,
            );
            let _ = ui
                .interact(
                    badge,
                    ui.make_persistent_id(("tg_ac_ephemeral", idx, &row.command)),
                    Sense::hover(),
                )
                .on_hover_text(tr::lng_ephemeral_command_tooltip());
        }

        resp.clicked()
    }

    fn paint_emoji_row(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        item: &EmojiKeywordResult,
        emoji_cache: &mut EmojiTextureCache,
        palette: &Palette,
    ) -> bool {
        let width = ui.available_width().max(160.0);
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(width, AUTOCOMPLETE_ROW_HEIGHT), Sense::click());
        if resp.hovered() {
            self.selected = idx;
        }
        if self.selected == idx {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, palette.window_bg_over);
        }

        let icon_rect = Rect::from_center_size(
            Pos2::new(rect.left() + 26.0, rect.center().y),
            Vec2::splat(24.0),
        );
        emoji_cache.paint_emoji_str(
            ui.painter(),
            ui.ctx(),
            &item.emoji,
            icon_rect,
            Color32::WHITE,
        );

        let label = format_emoji_suggestion_label(item);
        ui.painter().text(
            Pos2::new(rect.left() + 50.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.5),
            palette.window_fg,
        );

        resp.clicked()
    }

    fn paint_stickers_strip(
        &mut self,
        ui: &mut egui::Ui,
        docs: &[DocumentId],
        time_secs: f64,
        backend: &dyn ChatHelpersBackend,
        palette: &Palette,
    ) -> Option<usize> {
        let mut clicked = None;
        let is_premium_user = backend.is_premium();
        let mut any_anim = false;

        ScrollArea::horizontal()
            .id_salt("tg_field_autocomplete_stickers")
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for (idx, &doc_id) in docs.iter().enumerate() {
                                let (rect, resp) =
                                    ui.allocate_exact_size(Vec2::splat(64.0), Sense::click());
                                if resp.hovered() {
                                    self.selected = idx;
                                }
                                if self.selected == idx {
                                    ui.painter().rect_filled(
                                        rect,
                                        CornerRadius::same(8),
                                        palette.window_bg_over,
                                    );
                                }
                                if let Some((tex_id, anim)) = self.sticker_cache.texture_for(
                                    ui.ctx(),
                                    doc_id,
                                    time_secs,
                                    backend,
                                ) {
                                    if anim {
                                        any_anim = true;
                                    }
                                    ui.painter().image(
                                        tex_id,
                                        rect.shrink(4.0),
                                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                        Color32::WHITE,
                                    );
                                }
                                let is_locked = backend
                                    .sticker_document(doc_id)
                                    .is_some_and(|d| d.is_premium_sticker)
                                    && !is_premium_user;
                                if is_locked {
                                    paint_premium_lock_badge(ui.painter(), rect);
                                }
                                if resp.clicked() {
                                    clicked = Some(idx);
                                }
                            }
                        });
                    });
            });

        if any_anim {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(33));
        }
        clicked
    }
}

/// Formats an emoji autocomplete row label, preferring the emoji's canonical `:shortcode:`
/// from `tg-text` when the keyword pack returned a shared generic keyword.
pub(crate) fn format_emoji_suggestion_label(item: &EmojiKeywordResult) -> String {
    if item.label.starts_with(':') {
        return item.label.clone();
    }
    if let Some(entry) = tg_emoji_db::all_suggestions()
        .iter()
        .find(|s| s.emoji == item.emoji)
    {
        return entry.replacement.to_string();
    }
    format!(":{}:", item.label)
}

fn peer_avatar_color(id: u64) -> Color32 {
    const COLORS: [Color32; 7] = [
        Color32::from_rgb(0xE1, 0x70, 0x76),
        Color32::from_rgb(0xFA, 0xA7, 0x74),
        Color32::from_rgb(0xA6, 0x95, 0xE7),
        Color32::from_rgb(0x7B, 0xC8, 0x62),
        Color32::from_rgb(0x6E, 0xC9, 0xCB),
        Color32::from_rgb(0x65, 0xAA, 0xDD),
        Color32::from_rgb(0xEE, 0x7A, 0xAE),
    ];
    COLORS[(id as usize) % COLORS.len()]
}

fn extract_initials(name: &str) -> String {
    let mut words = name.split_whitespace();
    let Some(first) = words.next() else {
        return "?".into();
    };
    let mut out = String::new();
    if let Some(ch) = first.chars().next() {
        out.extend(ch.to_uppercase());
    }
    if let Some(last) = words.last()
        && let Some(ch) = last.chars().next()
    {
        out.extend(ch.to_uppercase());
    }
    out
}
