//! Dialogs search state, hashtag normalization, top peers strip, recent searches, and result sections.
//!
//! Port origin: `Telegram/SourceFiles/dialogs/dialogs_search_tags.cpp`,
//! `Telegram/SourceFiles/dialogs/dialogs_search_posts.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/chat_search_in.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/top_peers_strip.cpp`,
//! `Telegram/SourceFiles/dialogs/ui/dialogs_suggestions.cpp`,
//! and the filtered/search sections of `Telegram/SourceFiles/dialogs/dialogs_inner_widget.cpp`.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use tg_history::{LocalDateTime, elide_text_right};
use tg_ui::fonts::{family_regular, font_id_from_style};
use tg_ui::icons::paint_style_icon;
use tg_ui::stand_in::{Align, UnreadBadgeStyle};
use tg_ui::tg_style::st;
use tg_ui::theme::Colors;
use tg_ui::unread_badge::{UnreadBadgeKind, paint_unread_badge};

use crate::dialogs::backend::{
    ChatTypeFilter, DialogsBackend, SearchRequest, SearchResults, SearchTab, SpecialRowKind,
    TopPeerItem,
};
use crate::dialogs::layout::{
    DialogColors, RowPaintState, SEARCHED_BAR_HEIGHT, measure_text_i32, paint_dialog_row,
    paint_global_peer_row, paint_message_search_row, paint_peer_avatar, paint_section_bar,
    paint_semibold_text,
};
use crate::dialogs::strings;
use tg_app_core::stand_in::theme::Palette;
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Height of a hashtag suggestion row (`st::mentionHeight = 40px`).
pub const HASHTAG_ROW_HEIGHT: f32 = 40.0;
/// Height of a recent peer row (`st::recentPeersItem.height = 56px`).
pub const RECENT_PEER_ROW_HEIGHT: f32 = 56.0;
/// Width of one avatar cell in [`TopPeersStrip`] (`st::topPeers`).
pub const TOP_PEER_CELL_WIDTH: f32 = 66.0;
/// Height of one avatar cell in [`TopPeersStrip`].
pub const TOP_PEER_CELL_HEIGHT: f32 = 76.0;

/// Classification of a hashtag or cashtag search query (`Dialogs::HashOrCashtag`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HashOrCashtag {
    /// Ordinary text search query.
    #[default]
    None,
    /// `#hashtag` query with no internal whitespace.
    Hashtag,
    /// `$CASHTAG` query with no internal whitespace.
    Cashtag,
}

/// Classifies whether `query` is a `#hashtag` or `$cashtag` query.
///
/// Exact port of `IsHashOrCashtagSearchQuery` (`dialogs/ui/chat_search_in.cpp:241-262`).
#[must_use]
pub fn is_hash_or_cashtag_search_query(query: &str) -> HashOrCashtag {
    let trimmed = query.trim();
    let Some(first) = trimmed.chars().next() else {
        return HashOrCashtag::None;
    };
    if trimmed.chars().any(char::is_whitespace) {
        return HashOrCashtag::None;
    }
    match first {
        '#' => HashOrCashtag::Hashtag,
        '$' => HashOrCashtag::Cashtag,
        _ => HashOrCashtag::None,
    }
}

/// Normalizes a hashtag/cashtag query by stripping whitespace and ensuring a single leading sigil.
///
/// Exact port of `FixHashtagSearchQuery` (`dialogs/ui/chat_search_in.cpp:207-239`).
#[must_use]
pub fn fix_hashtag_search_query(query: &str, tag: HashOrCashtag) -> String {
    let first = match tag {
        HashOrCashtag::Cashtag => '$',
        HashOrCashtag::Hashtag | HashOrCashtag::None => '#',
    };
    let mut out = String::with_capacity(query.len() + 1);
    out.push(first);
    for ch in query.chars() {
        if !ch.is_whitespace() && ch != first {
            out.push(ch);
        }
    }
    out
}

/// Events emitted by the search results / suggestions renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchViewAction {
    /// Open a peer conversation (from local chats, global search, top peers, or recent peers).
    OpenPeer(PeerId),
    /// Jump to a specific message inside a peer's history.
    OpenMessage {
        /// Owning peer.
        peer_id: PeerId,
        /// Target message ID.
        msg_id: MsgId,
    },
    /// Autocomplete the search query with the chosen `#hashtag` or `$cashtag`.
    CompleteHashtag(String),
    /// Delete a recent hashtag from history.
    RemoveHashtag(String),
    /// Remove a single peer from recent search peers.
    RemoveRecentPeer(PeerId),
    /// Clear all recent search peers.
    ClearRecentPeers,
    /// Remove a peer from the Top Peers strip.
    RemoveTopPeer(PeerId),
    /// Switch active search tab (`Chats` / `Channels` / `Apps` / `Posts`).
    SetTab(SearchTab),
    /// Cycle or set the message chat-type filter.
    CycleChatTypeFilter,
    /// Toggle a Saved Messages search tag chip.
    ToggleTag(String),
    /// Load more message results (pagination).
    LoadMoreMessages,
}

/// Selectable row key inside search mode for keyboard Up/Down navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchKeyboardItem {
    /// Hashtag completion row at index.
    Hashtag(String),
    /// Local chat hit.
    LocalChat(PeerId),
    /// Global search peer hit.
    GlobalPeer(PeerId),
    /// Message search hit.
    Message {
        /// Owning peer.
        peer_id: PeerId,
        /// Matched message ID.
        msg_id: MsgId,
    },
    /// Recent peer hit (when query is empty).
    RecentPeer(PeerId),
}

/// Persistent UI state for search tabs, top peers expansion, keyboard selection, and pagination.
#[derive(Debug, Clone)]
pub struct DialogsSearchState {
    /// Active search scope tab (`Chats`, `Channels`, `Apps`, `Posts`).
    pub tab: SearchTab,
    /// Message results chat-type filter (`All`, `Private`, `Groups`, `Channels`).
    pub chat_type_filter: ChatTypeFilter,
    /// Optional peer restriction ("Search in this chat").
    pub in_peer: Option<PeerId>,
    /// True when searching inside the Archive folder.
    pub in_archive: bool,
    /// Currently selected Saved Messages search tag.
    pub selected_tag: Option<String>,
    /// Whether the Top Peers strip is expanded into a multi-row grid.
    pub top_peers_expanded: bool,
    /// Current pagination limit for message search results.
    pub messages_limit: usize,
    /// Keyboard-highlighted flat row index in the current search view.
    pub keyboard_index: Option<usize>,
}

impl Default for DialogsSearchState {
    fn default() -> Self {
        Self {
            tab: SearchTab::Chats,
            chat_type_filter: ChatTypeFilter::All,
            in_peer: None,
            in_archive: false,
            selected_tag: None,
            top_peers_expanded: false,
            messages_limit: 40,
            keyboard_index: None,
        }
    }
}

impl DialogsSearchState {
    /// Builds a [`SearchRequest`] from the current query and filter state.
    #[must_use]
    pub fn build_request(&self, query: &str) -> SearchRequest {
        SearchRequest {
            query: query.to_owned(),
            tab: self.tab,
            chat_type_filter: self.chat_type_filter,
            in_peer: self.in_peer,
            in_archive: self.in_archive,
            selected_tag: self.selected_tag.clone(),
            messages_offset: 0,
            messages_limit: self.messages_limit,
        }
    }

    /// Resets transient search filters when leaving search mode.
    pub fn reset(&mut self) {
        self.tab = SearchTab::Chats;
        self.chat_type_filter = ChatTypeFilter::All;
        self.in_peer = None;
        self.in_archive = false;
        self.selected_tag = None;
        self.top_peers_expanded = false;
        self.messages_limit = 40;
        self.keyboard_index = None;
    }

    /// Moves keyboard selection across `total_items` by `delta` (`+1` or `-1`).
    pub fn move_keyboard_selection(&mut self, delta: isize, total_items: usize) {
        if total_items == 0 {
            self.keyboard_index = None;
            return;
        }
        let next = match self.keyboard_index {
            None => {
                if delta >= 0 {
                    0
                } else {
                    total_items - 1
                }
            }
            Some(cur) => {
                let raw = cur as isize + delta;
                raw.clamp(0, (total_items - 1) as isize) as usize
            }
        };
        self.keyboard_index = Some(next);
    }
}

/// Builds the flat ordered list of keyboard-selectable items for the current search results.
#[must_use]
pub fn collect_search_keyboard_items(
    query: &str,
    results: &SearchResults,
    recent_peers: &[crate::dialogs::backend::RecentPeerItem],
) -> Vec<SearchKeyboardItem> {
    if query.trim().is_empty() && results.is_empty() {
        return recent_peers
            .iter()
            .map(|r| SearchKeyboardItem::RecentPeer(r.peer_id))
            .collect();
    }
    let mut items = Vec::with_capacity(
        results.hashtags.len()
            + results.local_chats.len()
            + results.global_peers.len()
            + results.messages.len(),
    );
    for h in &results.hashtags {
        items.push(SearchKeyboardItem::Hashtag(format!("{}{}", h.sigil, h.tag)));
    }
    for c in &results.local_chats {
        items.push(SearchKeyboardItem::LocalChat(c.peer_id));
    }
    for g in &results.global_peers {
        items.push(SearchKeyboardItem::GlobalPeer(g.peer_id));
    }
    for m in &results.messages {
        items.push(SearchKeyboardItem::Message {
            peer_id: m.peer_id,
            msg_id: m.msg_id,
        });
    }
    items
}

/// Renders the search scope tab strip (`Chats | Channels | Apps | Posts`) and optional search tags.
pub fn show_search_header_tabs(
    ui: &mut Ui,
    state: &mut DialogsSearchState,
    backend: &dyn DialogsBackend,
    palette: &Palette,
) -> Option<SearchViewAction> {
    let colors = DialogColors::new(palette);
    let mut action = None;
    let width = ui.available_width();
    let (_, tab_bar) = ui.allocate_space(Vec2::new(width, 34.0));
    ui.painter()
        .rect_filled(tab_bar, CornerRadius::ZERO, colors.window_bg());

    let tab_w = (width / SearchTab::ALL.len() as f32).max(52.0);
    let font = font_id_from_style(st::searched_bar_font);
    for (idx, tab) in SearchTab::ALL.iter().copied().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(tab_bar.left() + idx as f32 * tab_w, tab_bar.top()),
            Vec2::new(tab_w, tab_bar.height()),
        );
        let resp = ui.interact(r, ui.scope_id().with(("search_tab", idx)), Sense::click());
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let active = state.tab == tab;
        let fg = if active {
            colors.active_text_fg()
        } else if resp.hovered() {
            colors.bold_fg()
        } else {
            colors.sub_text_fg()
        };
        let label = tab.label();
        if active {
            paint_semibold_text(
                ui.painter(),
                r.center(),
                Align2::CENTER_CENTER,
                &label,
                font.clone(),
                fg,
            );
            ui.painter().rect_filled(
                Rect::from_min_max(
                    Pos2::new(r.left() + 8.0, r.bottom() - 2.5),
                    Pos2::new(r.right() - 8.0, r.bottom()),
                ),
                CornerRadius::same(2),
                colors.color("windowBgActive"),
            );
        } else {
            ui.painter()
                .text(r.center(), Align2::CENTER_CENTER, label, font.clone(), fg);
        }
        if resp.clicked() {
            action = Some(SearchViewAction::SetTab(tab));
        }
    }
    ui.painter().line_segment(
        [
            Pos2::new(tab_bar.left(), tab_bar.bottom() - 0.5),
            Pos2::new(tab_bar.right(), tab_bar.bottom() - 0.5),
        ],
        Stroke::new(1.0, colors.divider()),
    );

    // Optional Saved Messages / tag filter strip (`dialogs_search_tags.cpp`).
    let tags = backend.search_tags();
    if !tags.is_empty() && state.in_peer == Some(backend.saved_messages_peer_id()) {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_space(10.0);
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
            for tag in &tags {
                let selected = state.selected_tag.as_deref() == Some(tag.id.as_str());
                let label = format!("#{} {}", tag.label, tag.count);
                let fill = if selected {
                    palette.window_bg_active
                } else {
                    palette.input_bg
                };
                let fg = if selected {
                    Color32::WHITE
                } else {
                    palette.window_bold_fg
                };
                let resp = ui.add(
                    egui::Button::new(egui::RichText::new(label).size(11.5).color(fg))
                        .fill(fill)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(11)),
                );
                if resp.clicked() {
                    action = Some(SearchViewAction::ToggleTag(tag.id.clone()));
                }
            }
        });
        ui.add_space(4.0);
    }

    action
}

/// Renders either the empty-query Search Suggestions (`TopPeersStrip` + `Recent` peers)
/// or the live grouped Search Results (`Hashtags`, `Chats`, `Global Search`, `Messages`).
#[allow(clippy::too_many_arguments)]
pub fn show_search_body(
    ui: &mut Ui,
    query: &str,
    state: &mut DialogsSearchState,
    results: &SearchResults,
    backend: &dyn DialogsBackend,
    active_peer: PeerId,
    now: LocalDateTime,
    palette: &Palette,
) -> Option<SearchViewAction> {
    let colors = DialogColors::new(palette);
    let width = ui.available_width();

    if query.trim().is_empty() && state.selected_tag.is_none() {
        if state.tab == SearchTab::PublicPosts {
            show_public_posts_intro(ui, width, results.posts_flood.as_ref(), &colors);
            return None;
        }
        return show_search_suggestions(ui, width, state, backend, palette, &colors);
    }

    let mut action = None;
    let mut flat_idx = 0usize;

    if state.tab == SearchTab::PublicPosts
        && let Some(ref flood) = results.posts_flood
    {
        show_public_posts_flood_banner(ui, width, flood, &colors);
    }

    // 1. Hashtags / cashtags section (`dialogs_inner_widget.cpp:1551-1604`).
    if !results.hashtags.is_empty() {
        let typed_prefix = query.trim().trim_start_matches(['#', '$']);
        for h in &results.hashtags {
            let full_tag = format!("{}{}", h.sigil, h.tag);
            let (row_rect, resp) =
                ui.allocate_exact_size(Vec2::new(width, HASHTAG_ROW_HEIGHT), Sense::click());
            let selected = resp.hovered() || state.keyboard_index == Some(flat_idx);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let bg = if selected {
                colors.color("dialogsBgOver")
            } else {
                colors.window_bg()
            };
            ui.painter().rect_filled(row_rect, CornerRadius::ZERO, bg);

            // Split prefix (highlighted in accent blue) + suffix (`dialogs_inner_widget.cpp:1576-1597`).
            let font = font_id_from_style(st::dialogs_text_font);
            let match_len = typed_prefix.len().min(h.tag.len());
            let (first, second) = if match_len > 0 && h.tag.is_char_boundary(match_len) {
                (
                    format!("{}{}", h.sigil, &h.tag[..match_len]),
                    h.tag[match_len..].to_owned(),
                )
            } else {
                (String::new(), full_tag.clone())
            };

            let mut x = row_rect.left() + 14.0;
            if !first.is_empty() {
                let g = ui
                    .painter()
                    .layout_no_wrap(first, font.clone(), colors.active_text_fg());
                let w = g.size().x;
                ui.painter().galley(
                    Pos2::new(x, row_rect.center().y - g.size().y * 0.5),
                    g,
                    colors.active_text_fg(),
                );
                x += w;
            }
            if !second.is_empty() {
                ui.painter().text(
                    Pos2::new(x, row_rect.center().y),
                    Align2::LEFT_CENTER,
                    second,
                    font,
                    colors.bold_fg(),
                );
            }

            // Hover close `x` to remove recent hashtag.
            let mut removed_clicked = false;
            if selected {
                let close_rect = Rect::from_center_size(
                    Pos2::new(row_rect.right() - 20.0, row_rect.center().y),
                    Vec2::splat(20.0),
                );
                let close_resp = ui.interact(
                    close_rect,
                    ui.scope_id().with(("htag_rm", flat_idx)),
                    Sense::click(),
                );
                paint_style_icon(
                    ui.painter(),
                    close_rect,
                    st::box_title_close_icon,
                    &colors,
                    Some(colors.sub_text_fg()),
                );
                if close_resp.clicked() {
                    action = Some(SearchViewAction::RemoveHashtag(h.tag.clone()));
                    removed_clicked = true;
                }
            }
            if !removed_clicked && resp.clicked() {
                action = Some(SearchViewAction::CompleteHashtag(full_tag));
            }
            flat_idx += 1;
        }
    }

    // 2. Local chats & contacts (`_filterResults` in `dialogs_inner_widget.cpp:1605-1621`).
    if !results.local_chats.is_empty() {
        let (_, bar_rect) = ui.allocate_space(Vec2::new(width, SEARCHED_BAR_HEIGHT));
        paint_section_bar(
            ui.painter(),
            bar_rect,
            &tg_lang::tr::lng_recent_chats(),
            None,
            &colors,
        );

        let row_h = st::dialogs_row_height.0 as f32;
        for item in &results.local_chats {
            let (row_rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
            let selected = resp.hovered() || state.keyboard_index == Some(flat_idx);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let paint_state = RowPaintState {
                active: item.peer_id == active_peer,
                selected,
                narrow: false,
                dragging: false,
                show_folder_tags: false,
                anim_time_secs: ui.ctx().input(|i| i.time),
                use_24h: true,
            };
            paint_dialog_row(
                ui.painter(),
                row_rect,
                item,
                now,
                paint_state,
                palette,
                &colors,
            );
            if resp.clicked() {
                action = Some(SearchViewAction::OpenPeer(item.peer_id));
            }
            flat_idx += 1;
        }
    }

    // 3. Global search section (`_peerSearchResults`, `lng_search_global_results`).
    if !results.global_peers.is_empty() {
        let (_, bar_rect) = ui.allocate_space(Vec2::new(width, SEARCHED_BAR_HEIGHT));
        paint_section_bar(
            ui.painter(),
            bar_rect,
            &tg_lang::tr::lng_search_global_results(),
            None,
            &colors,
        );

        let row_h = st::dialogs_row_height.0 as f32;
        for peer in &results.global_peers {
            let (row_rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
            let selected = resp.hovered() || state.keyboard_index == Some(flat_idx);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            paint_global_peer_row(
                ui.painter(),
                row_rect,
                peer,
                peer.peer_id == active_peer,
                selected,
                palette,
                &colors,
            );
            if resp.clicked() {
                action = Some(SearchViewAction::OpenPeer(peer.peer_id));
            }
            flat_idx += 1;
        }
    }

    // 4. Messages / Public Posts section with highlighted query (`_searchResults`).
    if !results.messages.is_empty() {
        let (_, bar_rect) = ui.allocate_space(Vec2::new(width, SEARCHED_BAR_HEIGHT));
        let heading = if state.tab == SearchTab::PublicPosts {
            if results.total_messages == 1 {
                "Found 1 public post".to_owned()
            } else {
                format!("Found {} public posts", results.total_messages)
            }
        } else {
            tg_lang::tr::lng_search_found_results(results.total_messages as i64)
        };
        let filter_label = state.chat_type_filter.label();
        let show_chat_filter = state.tab != SearchTab::PublicPosts;
        let filter_hovered = show_chat_filter
            && ui
                .ctx()
                .pointer_hover_pos()
                .is_some_and(|p| bar_rect.contains(p) && p.x > bar_rect.right() - 90.0);
        if let Some(filter_rect) = paint_section_bar(
            ui.painter(),
            bar_rect,
            &heading,
            show_chat_filter.then_some((filter_label.as_str(), filter_hovered)),
            &colors,
        ) {
            let f_resp = ui.interact(
                filter_rect,
                ui.scope_id().with("msg_chat_type_filter"),
                Sense::click(),
            );
            if f_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if f_resp.clicked() {
                action = Some(SearchViewAction::CycleChatTypeFilter);
            }
        }

        let row_h = st::dialogs_row_height.0 as f32;
        for msg in &results.messages {
            let (row_rect, resp) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::click());
            let selected = resp.hovered() || state.keyboard_index == Some(flat_idx);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            paint_message_search_row(
                ui.painter(),
                row_rect,
                msg,
                query,
                now,
                false,
                selected,
                true,
                palette,
                &colors,
            );
            if resp.clicked() {
                action = Some(SearchViewAction::OpenMessage {
                    peer_id: msg.peer_id,
                    msg_id: msg.msg_id,
                });
            }
            flat_idx += 1;
        }

        if results.has_more_messages {
            ui.add_space(6.0);
            let more_label = if state.tab == SearchTab::PublicPosts {
                "Load more posts"
            } else {
                strings::LOAD_MORE_MESSAGES
            };
            ui.vertical_centered(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(more_label)
                                .font(font_id_from_style(st::searched_bar_font))
                                .color(colors.active_text_fg()),
                        )
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::NONE),
                    )
                    .clicked()
                {
                    action = Some(SearchViewAction::LoadMoreMessages);
                }
            });
            ui.add_space(6.0);
        }
    }

    if results.is_empty() {
        ui.add_space(36.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(tg_lang::tr::lng_search_tab_no_results())
                    .font(font_id_from_style(st::semibold_font))
                    .color(colors.bold_fg()),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tg_lang::tr::lng_search_tab_no_results_text(query.trim()))
                    .font(font_id_from_style(st::searched_bar_font))
                    .color(colors.sub_text_fg()),
            );
        });
    }

    action
}

/// Renders the empty-query Public Posts intro & daily quota notice (`dialogs_search_posts.cpp`).
fn show_public_posts_intro(
    ui: &mut Ui,
    width: f32,
    flood: Option<&crate::dialogs::backend::PostsSearchFloodInfo>,
    colors: &DialogColors<'_>,
) {
    if let Some(info) = flood {
        show_public_posts_flood_banner(ui, width, info, colors);
    }
    ui.add_space(28.0);
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new("Search Public Posts")
                .font(font_id_from_style(st::semibold_font))
                .color(colors.bold_fg()),
        );
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Enter keywords or a #hashtag to search posts across all public Telegram channels.",
            )
            .font(font_id_from_style(st::searched_bar_font))
            .color(colors.sub_text_fg()),
        );
    });
}

/// Renders the daily free-search quota / Stars status bar for `channels.searchPosts`.
fn show_public_posts_flood_banner(
    ui: &mut Ui,
    width: f32,
    flood: &crate::dialogs::backend::PostsSearchFloodInfo,
    colors: &DialogColors<'_>,
) {
    let text = if flood.query_is_free || flood.remains > 0 {
        if flood.total_daily > 0 {
            format!(
                "Public posts search: {}/{} free searches remaining today",
                flood.remains.max(0),
                flood.total_daily
            )
        } else {
            "Public posts search: free search available".to_owned()
        }
    } else if flood.stars_amount > 0 {
        format!(
            "Daily free searches used · Extra search costs {} Stars",
            flood.stars_amount
        )
    } else {
        "Daily free public posts search limit reached".to_owned()
    };
    let (_, bar_rect) = ui.allocate_space(Vec2::new(width, SEARCHED_BAR_HEIGHT));
    paint_section_bar(ui.painter(), bar_rect, &text, None, colors);
}

/// Renders the focused-empty search view: `TopPeersStrip` ("Frequent contacts") + `Recent` peers list.
fn show_search_suggestions(
    ui: &mut Ui,
    width: f32,
    state: &mut DialogsSearchState,
    backend: &dyn DialogsBackend,
    palette: &Palette,
    colors: &DialogColors<'_>,
) -> Option<SearchViewAction> {
    let mut action = None;
    let top_peers = backend.top_peers();
    if !top_peers.is_empty() {
        let (_, bar_rect) = ui.allocate_space(Vec2::new(width, SEARCHED_BAR_HEIGHT));
        let in_row = (width / TOP_PEER_CELL_WIDTH).floor().max(1.0) as usize;
        let can_expand = top_peers.len() > in_row;
        let toggle_label = if !can_expand {
            None
        } else if state.top_peers_expanded {
            Some(tg_lang::tr::lng_recent_frequent_collapse())
        } else {
            Some(tg_lang::tr::lng_recent_frequent_all())
        };
        let hovered_toggle = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| bar_rect.contains(p) && p.x > bar_rect.right() - 80.0);
        if let Some(toggle_rect) = paint_section_bar(
            ui.painter(),
            bar_rect,
            &tg_lang::tr::lng_recent_frequent(),
            toggle_label.as_deref().map(|l| (l, hovered_toggle)),
            colors,
        ) {
            let resp = ui.interact(
                toggle_rect,
                ui.scope_id().with("top_peers_toggle"),
                Sense::click(),
            );
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                state.top_peers_expanded = !state.top_peers_expanded;
            }
        }

        if let Some(act) = show_top_peers_strip(
            ui,
            width,
            &top_peers,
            state.top_peers_expanded,
            palette,
            colors,
        ) {
            action = Some(act);
        }
    }

    // Recent search peers section (`RecentsController` in `dialogs_suggestions.cpp`).
    let recent = backend.recent_search_peers();
    if !recent.is_empty() {
        let (_, bar_rect) = ui.allocate_space(Vec2::new(width, SEARCHED_BAR_HEIGHT));
        let clear_hovered = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|p| bar_rect.contains(p) && p.x > bar_rect.right() - 80.0);
        let clear_all = tg_lang::tr::lng_recent_clear_all();
        if let Some(clear_rect) = paint_section_bar(
            ui.painter(),
            bar_rect,
            &tg_lang::tr::lng_recent_title(),
            Some((clear_all.as_str(), clear_hovered)),
            colors,
        ) {
            let resp = ui.interact(
                clear_rect,
                ui.scope_id().with("recent_clear_all"),
                Sense::click(),
            );
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if resp.clicked() {
                action = Some(SearchViewAction::ClearRecentPeers);
            }
        }

        for (idx, item) in recent.iter().enumerate() {
            let (row_rect, resp) =
                ui.allocate_exact_size(Vec2::new(width, RECENT_PEER_ROW_HEIGHT), Sense::click());
            let selected = resp.hovered() || state.keyboard_index == Some(idx);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            ui.painter().rect_filled(
                row_rect,
                CornerRadius::ZERO,
                if selected {
                    colors.color("dialogsBgOver")
                } else {
                    colors.window_bg()
                },
            );

            let photo_rect = Rect::from_min_size(
                Pos2::new(row_rect.left() + 12.0, row_rect.top() + 8.0),
                Vec2::splat(40.0),
            );
            paint_peer_avatar(
                ui.painter(),
                photo_rect,
                item.special,
                item.color_index,
                &item.title,
                colors,
            );

            let text_left = row_rect.left() + 62.0;
            let right_edge = row_rect.right() - if selected { 34.0 } else { 12.0 };

            let name_font = font_id_from_style(st::semibold_font);
            let title = elide_text_right(
                &item.title,
                (right_edge - text_left).max(16.0) as i32,
                |s| measure_text_i32(ui.painter(), s, name_font.clone()),
            );
            paint_semibold_text(
                ui.painter(),
                Pos2::new(text_left, row_rect.top() + 18.0),
                Align2::LEFT_CENTER,
                &title,
                name_font,
                colors.bold_fg(),
            );

            let status_fg = if item.status == "online" {
                colors.active_text_fg()
            } else {
                colors.sub_text_fg()
            };
            let status_text = if item.status == "online" {
                tg_lang::tr::lng_status_online()
            } else {
                item.status.clone()
            };
            ui.painter().text(
                Pos2::new(text_left, row_rect.top() + 38.0),
                Align2::LEFT_CENTER,
                status_text,
                font_id_from_style(st::dialogs_text_font),
                status_fg,
            );

            if !selected && item.unread_count > 0 {
                let badge_st = UnreadBadgeStyle {
                    muted: item.muted,
                    align: Align::Right,
                    ..UnreadBadgeStyle::default()
                };
                paint_unread_badge(
                    ui.painter(),
                    &item.unread_count.to_string(),
                    Pos2::new(row_rect.right() - 12.0, row_rect.center().y - 9.5),
                    &badge_st,
                    UnreadBadgeKind::Dialogs,
                    0,
                    colors,
                );
            }

            let mut rm_clicked = false;
            if selected {
                let rm_rect = Rect::from_center_size(
                    Pos2::new(row_rect.right() - 20.0, row_rect.center().y),
                    Vec2::splat(20.0),
                );
                let rm_resp = ui.interact(
                    rm_rect,
                    ui.scope_id().with(("recent_rm", item.peer_id.0)),
                    Sense::click(),
                );
                paint_style_icon(
                    ui.painter(),
                    rm_rect,
                    st::box_title_close_icon,
                    colors,
                    Some(colors.sub_text_fg()),
                );
                if rm_resp.clicked() {
                    action = Some(SearchViewAction::RemoveRecentPeer(item.peer_id));
                    rm_clicked = true;
                }
            }

            if !rm_clicked && resp.clicked() {
                action = Some(SearchViewAction::OpenPeer(item.peer_id));
            }
        }
    }

    action
}

fn show_top_peers_strip(
    ui: &mut Ui,
    width: f32,
    peers: &[TopPeerItem],
    expanded: bool,
    palette: &Palette,
    colors: &DialogColors<'_>,
) -> Option<SearchViewAction> {
    let mut action = None;
    let in_row = (width / TOP_PEER_CELL_WIDTH).floor().max(1.0) as usize;

    if expanded {
        let rows = peers.len().div_ceil(in_row);
        for row_idx in 0..rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let start = row_idx * in_row;
                let end = (start + in_row).min(peers.len());
                for peer in &peers[start..end] {
                    if let Some(act) = paint_top_peer_cell(ui, peer, palette, colors) {
                        action = Some(act);
                    }
                }
            });
        }
    } else {
        ScrollArea::horizontal()
            .id_salt("tg_top_peers_strip")
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for peer in peers {
                        if let Some(act) = paint_top_peer_cell(ui, peer, palette, colors) {
                            action = Some(act);
                        }
                    }
                });
            });
    }

    action
}

fn paint_top_peer_cell(
    ui: &mut Ui,
    peer: &TopPeerItem,
    palette: &Palette,
    colors: &DialogColors<'_>,
) -> Option<SearchViewAction> {
    let (cell_rect, resp) = ui.allocate_exact_size(
        Vec2::new(TOP_PEER_CELL_WIDTH, TOP_PEER_CELL_HEIGHT),
        Sense::click(),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(
            cell_rect.shrink(3.0),
            CornerRadius::same(8),
            colors.color("dialogsBgOver"),
        );
    }

    let photo_rect = Rect::from_center_size(
        Pos2::new(cell_rect.center().x, cell_rect.top() + 30.0),
        Vec2::splat(st::top_peers_selector_userpic_size.0.min(44) as f32),
    );
    paint_peer_avatar(
        ui.painter(),
        photo_rect,
        SpecialRowKind::None,
        peer.color_index,
        &peer.full_title,
        colors,
    );

    if peer.unread_count > 0 {
        let badge_st = UnreadBadgeStyle {
            muted: peer.muted,
            align: Align::Right,
            ..UnreadBadgeStyle::default()
        };
        paint_unread_badge(
            ui.painter(),
            &peer.unread_count.to_string(),
            Pos2::new(photo_rect.right() + 2.0, photo_rect.bottom() - 18.0),
            &badge_st,
            UnreadBadgeKind::Dialogs,
            0,
            colors,
        );
    } else if peer.online {
        let dot_c = Pos2::new(photo_rect.right() - 5.0, photo_rect.bottom() - 5.0);
        ui.painter().circle_filled(dot_c, 6.5, colors.window_bg());
        ui.painter()
            .circle_filled(dot_c, 4.5, colors.online_badge_fg(false));
    }

    let name_font = FontId::new(11.0, family_regular());
    let label = elide_text_right(&peer.short_name, (TOP_PEER_CELL_WIDTH - 8.0) as i32, |s| {
        measure_text_i32(ui.painter(), s, name_font.clone())
    });
    ui.painter().text(
        Pos2::new(cell_rect.center().x, cell_rect.top() + 63.0),
        Align2::CENTER_CENTER,
        label,
        name_font,
        colors.bold_fg(),
    );

    let _ = palette;
    let resp = resp.on_hover_text(&peer.full_title);
    if resp.clicked() {
        Some(SearchViewAction::OpenPeer(peer.peer_id))
    } else if resp.secondary_clicked() {
        Some(SearchViewAction::RemoveTopPeer(peer.peer_id))
    } else {
        None
    }
}

/// Renders a chip pill for an active search scope restriction (e.g. searching inside Archive or a peer).
pub fn paint_search_scope_chip(ui: &mut Ui, label: &str, palette: &Palette) -> bool {
    let colors = DialogColors::new(palette);
    let accent = colors.color("windowBgActive");
    let active_fg = colors.active_text_fg();
    let font = FontId::new(11.5, family_regular());
    let g = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), active_fg);
    let size = Vec2::new(g.size().x + 26.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(11),
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 36),
    );
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(11),
        Stroke::new(1.0, accent),
        StrokeKind::Inside,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        active_fg,
    );
    let x_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 10.0, rect.center().y),
        Vec2::splat(10.0),
    );
    paint_style_icon(
        ui.painter(),
        x_rect,
        st::box_title_close_icon,
        &colors,
        Some(active_fg),
    );
    resp.clicked()
}
