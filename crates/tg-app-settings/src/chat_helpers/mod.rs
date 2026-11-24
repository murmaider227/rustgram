//! Chat helpers UI: Emoji / Stickers / GIFs tabbed panel and compose field autocomplete popup.
//!
//! Port origin:
//! - `Telegram/SourceFiles/chat_helpers/tabbed_panel.cpp`
//! - `Telegram/SourceFiles/chat_helpers/tabbed_selector.cpp`
//! - `Telegram/SourceFiles/chat_helpers/emoji_list_widget.cpp`
//! - `Telegram/SourceFiles/chat_helpers/stickers_list_widget.cpp`
//! - `Telegram/SourceFiles/chat_helpers/gifs_list_widget.cpp`
//! - `Telegram/SourceFiles/chat_helpers/field_autocomplete.cpp`
//! - `Telegram/SourceFiles/chat_helpers/emoji_suggestions_widget.cpp`

pub use tg_app_core::chat_helpers::backend;
pub mod emoji_list_widget;
pub mod field_autocomplete;
pub mod gifs_list_widget;
pub mod gift_box_pack;
pub mod mock;
pub mod showcase;
pub mod stickers_list_widget;
pub mod strings;
pub mod tabbed_panel;
pub mod tabbed_selector;

use std::sync::OnceLock;

use egui::{Color32, Painter, Rect};
use tg_ui::icons::paint_style_icon;
use tg_ui::tg_style::Icon;
use tg_ui::tg_theme::{EmbeddedType, Palette as ThemePalette};
use tg_ui::theme::ThemeManager;

static DAY_THEME_PALETTE: OnceLock<ThemePalette> = OnceLock::new();
static NIGHT_THEME_PALETTE: OnceLock<ThemePalette> = OnceLock::new();

/// Returns a cached `tg-theme` palette from `ThemeManager` for Day (`dark = false`) or Night (`dark = true`).
pub(crate) fn theme_palette_for_dark(dark: bool) -> &'static ThemePalette {
    if dark {
        NIGHT_THEME_PALETTE.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Night)
                .palette()
                .clone()
        })
    } else {
        DAY_THEME_PALETTE.get_or_init(|| {
            ThemeManager::with_embedded(EmbeddedType::Default)
                .palette()
                .clone()
        })
    }
}

/// Paints a `tg-style` icon resolved against the active `ThemeManager` palette.
pub(crate) fn paint_helper_icon(
    painter: &Painter,
    rect: Rect,
    icon: Icon,
    dark: bool,
    color_override: Option<Color32>,
) -> bool {
    paint_style_icon(
        painter,
        rect,
        icon,
        theme_palette_for_dark(dark),
        color_override,
    )
}

/// Builds a stand-in [`tg_app_core::stand_in::theme::Palette`] populated from `ThemeManager`'s Day/Night tokens
/// so `chat_helpers` widgets follow the active `tg-theme` palette accurately.
pub(crate) fn helper_palette_from_theme(dark: bool) -> tg_app_core::stand_in::theme::Palette {
    let mode = if dark {
        tg_app_core::stand_in::theme::ThemeMode::Night
    } else {
        tg_app_core::stand_in::theme::ThemeMode::Day
    };
    let mut out = tg_app_core::stand_in::theme::Palette::for_mode(mode);
    let tp = theme_palette_for_dark(dark);
    let get = |name: &str, fallback: Color32| -> Color32 {
        tp.color_by_name(name).map_or(fallback, |c| {
            Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
        })
    };
    out.window_bg = get("emojiPanBg", get("windowBg", out.window_bg));
    out.window_bg_over = get("windowBgOver", out.window_bg_over);
    out.window_bg_active = get("windowBgActive", out.window_bg_active);
    out.window_fg = get("windowFg", out.window_fg);
    out.window_bold_fg = get("windowBoldFg", out.window_bold_fg);
    out.window_sub_text_fg = get(
        "emojiPanHeaderFg",
        get("windowSubTextFg", out.window_sub_text_fg),
    );
    out.window_active_text_fg = get("windowActiveTextFg", out.window_active_text_fg);
    out.window_fg_active = get("activeButtonFg", out.window_fg_active);
    out.divider = get("menuSeparatorFg", get("shadowFg", out.divider));
    out.input_bg = get("filterInputInactiveBg", out.input_bg);
    out.input_border_active = get("activeLineFg", out.input_border_active);
    out.msg_in_bg = get("msgInBg", out.msg_in_bg);
    out.msg_out_bg = get("msgOutBg", out.msg_out_bg);
    out
}

pub use backend::{ChatHelpersBackend, GifCategory, GifItem, InlineGifQueryHook, StickerMedia};
pub use emoji_list_widget::{
    EMOJI_CELL_SIZE, EMOJI_COLUMNS, EMOJI_DRAW_SIZE, EMOJI_FOOTER_HEIGHT, EMOJI_SECTIONS,
    EmojiListEvent, EmojiListWidget, SkinTonePopup,
};
pub use field_autocomplete::{
    AUTOCOMPLETE_MAX_HEIGHT, AUTOCOMPLETE_ROW_HEIGHT, AutocompleteAction, AutocompleteRows,
    FieldAutocompleteView,
};
pub use gifs_list_widget::{
    GifsListEvent, GifsListWidget, MOSAIC_GAP, MOSAIC_TARGET_ROW_HEIGHT, MosaicTile,
    layout_mosaic_rows,
};
pub use mock::{MockChatHelpersBackend, MockInlineGifHook};
pub use stickers_list_widget::{
    LOTTIE_PREVIEW_PX, STICKER_CELL_SIZE, STICKER_COLUMNS, StickerMediaCache, StickersListEvent,
    StickersListWidget,
};
pub use tabbed_panel::{
    PanelVisibility, TABBED_PANEL_HEIGHT_RATIO, TABBED_PANEL_MAX_HEIGHT, TABBED_PANEL_MIN_HEIGHT,
    TABBED_PANEL_RADIUS, TABBED_PANEL_WIDTH, TabbedPanel,
};
pub use tabbed_selector::{ChatHelpersEvent, SELECTOR_TAB_BAR_HEIGHT, SelectorTab, TabbedSelector};
pub use tg_app_core::core_types::emoji_sets::{
    EMOJI_CLOUD_LOCATION_USERNAME, EMOJI_SETS, EmojiCloudLocation, EmojiSetDescriptor,
    EmojiSetState, EmojiSetsManager, ManageEmojiSetsAction, good_emoji_set_part_name,
    show_emoji_sets_modal_window, unpack_emoji_set_zip,
};

#[cfg(test)]
mod tests {
    use super::*;
    use tg_chat::autocomplete::ChooseMethod;
    use tg_chat::emoji::SkinTone;
    use tg_chat::{DocumentId, StickerSetId};
    use tg_lottie::{Composition, Size as LottieSize, render_composition};
    use tg_text::emoji::Section;

    #[test]
    fn mock_backend_emoji_search_recents_and_skin_tones() {
        let mut backend = MockChatHelpersBackend::new();
        let initial_recent = backend.recent_emoji();
        assert!(!initial_recent.is_empty());

        // Recording an emoji boosts it to the front.
        backend.record_emoji_used("🚀");
        let after = backend.recent_emoji();
        assert_eq!(after[0], "🚀");

        // Keyword search via `tg-chat::emoji::EmojiKeywords` + `tg-text` shortcodes.
        let smile_hits = backend.search_emoji("smile");
        assert!(
            smile_hits
                .iter()
                .any(|h| h.emoji == "😄" || h.emoji == "😀"),
            "expected smile emoji in results: {smile_hits:?}"
        );

        // Skin tone persistence (per-emoji and global).
        backend.save_emoji_variant("👍", SkinTone::MediumDark);
        assert_eq!(
            backend.emoji_variants().lookup_tone("👍"),
            Some(SkinTone::MediumDark)
        );
        backend.save_all_emoji_variants(SkinTone::Light);
        assert_eq!(
            backend.emoji_variants().lookup_tone("👋"),
            Some(SkinTone::Light)
        );
    }

    #[test]
    fn mock_backend_lottie_stickers_parse_and_rasterize_cleanly() {
        let backend = MockChatHelpersBackend::new();
        let sections = backend.sticker_sections();
        assert!(
            sections.len() >= 4,
            "expected Favorites + Recent + installed sets, got {}",
            sections.len()
        );

        // Verify every Lottie sticker in the mock backend parses & renders non-empty pixels via tg-lottie.
        let mut lottie_checked = 0usize;
        for sec in &sections {
            for &doc_id in &sec.stickers {
                if let Some(StickerMedia::LottieJson(json)) = backend.sticker_media(doc_id) {
                    let comp = Composition::from_json(&json)
                        .unwrap_or_else(|e| panic!("Lottie JSON for {doc_id:?} failed: {e}"));
                    let frame0 = render_composition(&comp, 0.0, LottieSize::new(64, 64));
                    let frame15 = render_composition(&comp, 15.0, LottieSize::new(64, 64));
                    assert!(
                        frame0.data.iter().skip(3).step_by(4).any(|&a| a > 0),
                        "expected visible pixels in frame 0 of {doc_id:?}"
                    );
                    assert_ne!(
                        frame0.data, frame15.data,
                        "expected animated difference between frame 0 and 15 of {doc_id:?}"
                    );
                    lottie_checked += 1;
                }
            }
        }
        assert!(lottie_checked >= 5, "expected >=5 animated Lottie stickers");

        // Emoji-based sticker search finds star stickers.
        let star_stickers = backend.search_stickers("⭐");
        assert!(!star_stickers.is_empty(), "expected stickers matching ⭐");
    }

    #[test]
    fn mock_backend_premium_lock_and_favorites_and_featured_install() {
        let mut backend = MockChatHelpersBackend::new();
        assert!(!backend.is_premium());

        // Document 3001 is a premium sticker.
        let doc = backend.sticker_document(DocumentId(3001)).unwrap();
        assert!(doc.is_premium_sticker);

        // Toggle favorite on sticker 1003.
        assert!(!backend.is_sticker_faved(DocumentId(1003)));
        assert!(backend.toggle_faved_sticker(DocumentId(1003)));
        assert!(backend.is_sticker_faved(DocumentId(1003)));
        assert!(!backend.toggle_faved_sticker(DocumentId(1003)));

        // Install trending featured set 10_004.
        let featured = backend.featured_sticker_sections();
        assert_eq!(featured.len(), 1);
        let feat_id = StickerSetId(10_004);
        assert!(!backend.is_set_installed(feat_id));
        assert!(backend.install_sticker_set(feat_id));
        assert!(backend.is_set_installed(feat_id));
    }

    #[test]
    fn gifs_mosaic_layout_and_inline_query_hook() {
        let mut backend = MockChatHelpersBackend::new();
        let saved = backend.saved_gifs();
        assert!(saved.len() >= 4);

        let party = backend.search_gifs("party");
        assert!(!party.is_empty());
        assert!(party.iter().any(|g| g.title.contains("Party")));

        let (tiles, total_h) = layout_mosaic_rows(&saved, 320.0, MOSAIC_TARGET_ROW_HEIGHT);
        assert_eq!(tiles.len(), saved.len());
        assert!(total_h > 80.0);
        for t in &tiles {
            assert!(t.rect.width() > 20.0);
            assert!(t.rect.height() >= 60.0);
            assert!(t.rect.right() <= 322.0);
        }

        // Procedural GIF frame rasterizer returns valid RGBA bytes.
        let (w, h, rgba) = backend.gif_frame_rgba(saved[0].id, 3).unwrap();
        assert_eq!(rgba.len(), (w * h * 4) as usize);

        // Toggle saved GIF removes then re-adds it.
        let first_id = saved[0].id;
        assert!(!backend.toggle_saved_gif(first_id));
        assert!(backend.toggle_saved_gif(first_id));
    }

    #[test]
    fn tabbed_panel_animation_and_geometry_bounds() {
        let mut panel = TabbedPanel::new();
        assert_eq!(panel.visibility(), PanelVisibility::Hidden);
        assert!(!panel.is_open());

        panel.show_animated(10.0);
        assert_eq!(panel.visibility(), PanelVisibility::Showing);
        assert!(panel.is_open());
        let mid = panel.current_progress(10.10);
        assert!(
            mid > 0.5 && mid < 1.0,
            "expected eased mid progress, got {mid}"
        );
        assert!((panel.current_progress(10.25) - 1.0).abs() < 1e-4);

        panel.hide_animated(10.25);
        assert_eq!(panel.visibility(), PanelVisibility::Hiding);
        assert!(!panel.is_open());
        assert!(panel.is_rendered());
        assert!(panel.current_progress(10.35) < 0.5);

        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 720.0));
        let anchor = egui::Rect::from_min_size(egui::pos2(600.0, 668.0), egui::vec2(380.0, 44.0));
        let rect = panel.compute_panel_rect(anchor, viewport);
        assert!((rect.width() - TABBED_PANEL_WIDTH).abs() < 1.0);
        assert!(rect.height() >= TABBED_PANEL_MIN_HEIGHT);
        assert!(rect.height() <= TABBED_PANEL_MAX_HEIGHT);
        assert!(rect.bottom() <= anchor.top());

        // Section jump helper on emoji widget.
        panel
            .selector_mut()
            .emoji_list_mut()
            .jump_to_section(Section::Food);
        assert_eq!(
            panel.selector().emoji_list().active_section(),
            Section::Food
        );
    }

    #[test]
    fn field_autocomplete_handles_mentions_hashtags_commands_emoji_and_stickers() {
        let mut backend = MockChatHelpersBackend::new();
        let mut ac = FieldAutocompleteView::new();

        // 1. Mentions (`@ke` -> Kestrel `@kestrel_dev`)
        ac.update_from_input("Hey @ke", 7, false, &mut backend);
        assert!(matches!(ac.rows(), AutocompleteRows::Mentions { rows, .. } if !rows.is_empty()));
        let act = ac
            .choose_index(0, ChooseMethod::ByEnter, false, "Hey @ke", 7, &mut backend)
            .expect("mention action");
        match act {
            AutocompleteAction::ReplaceText(ins) => {
                assert_eq!(ins.text, "Hey @kestrel_dev ");
            }
            other => panic!("expected ReplaceText, got {other:?}"),
        }

        // 2. Hashtags (`#ru` -> `#rust`)
        ac.update_from_input("Check #ru", 9, false, &mut backend);
        assert!(
            matches!(ac.rows(), AutocompleteRows::Hashtags { rows, .. } if rows.contains(&"rust".to_string()))
        );
        let act = ac
            .choose_index(0, ChooseMethod::ByTab, false, "Check #ru", 9, &mut backend)
            .expect("hashtag action");
        match act {
            AutocompleteAction::ReplaceText(ins) => {
                assert_eq!(ins.text, "Check #rust ");
            }
            other => panic!("expected ReplaceText, got {other:?}"),
        }

        // 3. Bot commands (`/he` -> Tab inserts, Enter sends immediately)
        ac.update_from_input("/he", 3, false, &mut backend);
        assert!(
            matches!(ac.rows(), AutocompleteRows::BotCommands { rows, .. } if !rows.is_empty())
        );
        let tab_act = ac
            .choose_index(0, ChooseMethod::ByTab, false, "/he", 3, &mut backend)
            .expect("tab command");
        assert!(
            matches!(tab_act, AutocompleteAction::ReplaceText(ins) if ins.text.starts_with("/help"))
        );

        ac.update_from_input("/he", 3, false, &mut backend);
        let enter_act = ac
            .choose_index(0, ChooseMethod::ByEnter, false, "/he", 3, &mut backend)
            .expect("enter command");
        assert!(
            matches!(enter_act, AutocompleteAction::SendBotCommand { command, .. } if command.starts_with("/help"))
        );

        // 4. Emoji suggestions (`:smi` -> `:smile:` / 😄)
        ac.update_from_input("Hello :smi", 10, false, &mut backend);
        assert!(matches!(ac.rows(), AutocompleteRows::Emoji { rows, .. } if !rows.is_empty()));
        let emoji_act = ac
            .choose_index(
                0,
                ChooseMethod::ByEnter,
                false,
                "Hello :smi",
                10,
                &mut backend,
            )
            .expect("emoji action");
        match emoji_act {
            AutocompleteAction::ReplaceText(ins) => {
                assert!(ins.text.starts_with("Hello "));
                assert!(!ins.text.contains(":smi"));
            }
            other => panic!("expected ReplaceText, got {other:?}"),
        }

        // 5. Single-emoji sticker suggestions (`⭐`)
        ac.update_from_input("⭐", "⭐".len(), false, &mut backend);
        assert!(matches!(ac.rows(), AutocompleteRows::Stickers { docs, .. } if !docs.is_empty()));

        // 6. Dismissal suppresses re-opening for the same query until edited.
        ac.update_from_input("Hey @ke", 7, false, &mut backend);
        assert!(ac.is_visible());
        ac.dismiss();
        assert!(!ac.is_visible());
        ac.update_from_input("Hey @ke", 7, false, &mut backend);
        assert!(!ac.is_visible());
        ac.update_from_input("Hey @kes", 8, false, &mut backend);
        assert!(ac.is_visible());
    }
}
