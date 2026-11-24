//! End-to-end `egui_kittest` tests for the Rich Document Editor window (`webview::rich_editor`),
//! the reusable TeX formula widget (`webview::formula_widget`), and rich message bubble rendering
//! & edit/send round-trips in `HistoryWidget` + `MockSession`.

use egui::{Color32, Key, Modifiers, Vec2};
use egui_kittest::kittest::Queryable;
use tg_app::history::HistoryWidget;
use tg_app::history::backend::HistoryBackend;
use tg_app::stand_in::mock_session::{MockSession, session_peers};
use tg_app::stand_in::strings::EnglishStrings;
use tg_app::stand_in::theme::Palette;
use tg_app::stand_in::types::MsgId;
use tg_app::webview::formula_widget::{
    FormulaRenderMetrics, FormulaWidgetStyle, show_formula_widget,
};
use tg_app::webview::rich_editor::{RichEditorAction, RichEditorTab, RichEditorWindow};
use tg_app::webview::rich_page_view::{RichMessagePayload, RichPageViewStyle, paint_rich_page};
use tg_iv::editor::{EditorState, InsertAction, InsertBlockType};
use tg_iv::markdown::MathLayoutEngine;
use tg_iv::rich_page::{
    EntityType, InMemoryMediaResolver, InlineTextObjectEntity, OBJECT_REPLACEMENT_CHAR, RichBlock,
    RichBlockKind, RichMessageLimits, RichPage, SerializeRichMessageMode,
    SerializeRichMessageStatus, parse_inline_text_object_entity,
};

use crate::common::{build_widget_harness, press_key, type_text};

const EDITOR_SIZE: Vec2 = Vec2::new(820.0, 620.0);

struct EditorTestState {
    editor: RichEditorWindow,
    last_action: Option<RichEditorAction>,
}

impl EditorTestState {
    fn new() -> Self {
        Self {
            editor: RichEditorWindow::new_compose(session_peers::GROUP_CORE),
            last_action: None,
        }
    }

    fn with_limits(limits: RichMessageLimits) -> Self {
        let state = EditorState::from_rich_page(RichPage::default(), limits);
        Self {
            editor: RichEditorWindow::from_editor_state(session_peers::GROUP_CORE, None, state),
            last_action: None,
        }
    }
}

fn build_editor_harness(state: EditorTestState) -> egui_kittest::Harness<'static, EditorTestState> {
    build_widget_harness(state, EDITOR_SIZE, |ui, st| {
        if let Some(act) = st.editor.show_contents(ui, false) {
            st.last_action = Some(act);
        }
    })
}

#[test]
fn rich_editor_type_bold_formula_undo_and_table_add_row() {
    let mut harness = build_editor_harness(EditorTestState::new());
    harness.step();

    // 1. Type text into the active paragraph leaf.
    type_text(&mut harness, "Hello math");
    harness.step();
    assert_eq!(
        harness.state().editor.state().active_raw_text(),
        "Hello math"
    );

    // 2. Select all text with Ctrl+A and toggle Bold via the toolbar button.
    press_key(&mut harness, Modifiers::COMMAND, Key::A);
    harness.step();
    assert!(harness.state().editor.state().cursor().has_selection());
    assert!(harness.state().editor.state().toolbar_state().bold.enabled);

    harness.get_by_label("Bold").click();
    harness.step();
    let active = harness.state().editor.state().active_text();
    assert!(
        active
            .entities
            .iter()
            .any(|e| e.kind == EntityType::Bold && e.offset == 0 && e.length == 10),
        "expected Bold entity spanning 'Hello math', got {:?}",
        active.entities
    );
    assert!(harness.state().editor.state().toolbar_state().bold.active);

    // Collapse caret to end before inserting inline formula.
    press_key(&mut harness, Modifiers::NONE, Key::End);
    harness.step();

    // 3. Insert an inline TeX formula via the Math toolbar button + prompt.
    harness.get_by_label("Math").click();
    harness.step();

    let math_input = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .expect("math TeX input should be visible");
    math_input.click();
    harness.step();
    type_text(&mut harness, r"\frac{a}{b}");
    harness.step();

    harness.get_by_label("Insert Formula").click();
    harness.step();

    let after_math = harness.state().editor.state().active_text();
    assert!(
        after_math.text.contains(OBJECT_REPLACEMENT_CHAR),
        "inline formula must insert U+FFFC placeholder, got {:?}",
        after_math.text
    );
    let has_formula_obj = after_math.entities.iter().any(|e| {
        e.kind == EntityType::CustomEmoji
            && matches!(
                parse_inline_text_object_entity(&e.data),
                Some(InlineTextObjectEntity::Formula(f)) if f.trimmed_tex == r"\frac{a}{b}"
            )
    });
    assert!(
        has_formula_obj,
        "expected inline Formula entity for \\frac{{a}}{{b}}, got {:?}",
        after_math.entities
    );

    // 4. Undo removes the inserted inline formula and restores "Hello math".
    harness.get_by_label("Undo").click();
    harness.step();
    assert_eq!(
        harness.state().editor.state().active_raw_text(),
        "Hello math"
    );

    // 5. Insert a Table block and add a row via the Table toolbar ("+ Row Below").
    let _ = harness
        .state_mut()
        .editor
        .state_mut()
        .insert_block_after_active(InsertAction::new(InsertBlockType::Table));
    // Focus cell (0, 0) so the contextual table toolbar renders.
    let table_block_idx = harness
        .state()
        .editor
        .state()
        .rich_page()
        .blocks
        .iter()
        .position(|b| b.kind == RichBlockKind::Table)
        .expect("table block inserted");
    let initial_rows = harness.state().editor.state().rich_page().blocks[table_block_idx]
        .table_rows
        .len();
    assert_eq!(initial_rows, 3);

    let cell_ord = harness
        .state()
        .editor
        .state()
        .text_nodes()
        .iter()
        .position(|d| {
            d.leaf.kind == tg_iv::editor::LeafKind::TableCellText
                && d.leaf.table_row_index == Some(0)
                && d.leaf.table_cell_index == Some(0)
        })
        .expect("cell (0,0) leaf ordinal");
    harness
        .state_mut()
        .editor
        .state_mut()
        .set_cursor(cell_ord, 0);
    harness.step();

    harness.get_by_label("+ Row Below").click();
    harness.step();

    let updated_rows = harness.state().editor.state().rich_page().blocks[table_block_idx]
        .table_rows
        .len();
    assert_eq!(
        updated_rows,
        initial_rows + 1,
        "clicking '+ Row Below' must append a table row"
    );
}

#[test]
fn rich_editor_limit_error_toast_markdown_import_and_preview() {
    let tight_limits = RichMessageLimits {
        length_limit: 8,
        ..RichMessageLimits::default()
    };
    let mut harness = build_editor_harness(EditorTestState::with_limits(tight_limits));
    harness.step();

    // Exceed the 8-char limit -> triggers last_limit_error() toast.
    type_text(&mut harness, "1234567890");
    harness.step();
    assert!(
        harness
            .state()
            .editor
            .active_toast()
            .is_some_and(|t| t.contains("limit")),
        "expected limit error toast, got {:?}",
        harness.state().editor.active_toast()
    );

    // Switch to Markdown tab and verify export + preview rendering on a normal editor.
    let mut normal = build_editor_harness(EditorTestState::new());
    normal.step();
    type_text(&mut normal, "Euler identity");
    normal.step();
    let _ = normal
        .state_mut()
        .editor
        .state_mut()
        .insert_block_after_active(InsertAction::new(InsertBlockType::Math));
    let _ = normal
        .state_mut()
        .editor
        .state_mut()
        .edit_active_display_math(r"e^{i\pi} + 1 = 0".to_owned());
    normal.step();

    normal.get_by_label("Markdown").click();
    normal.step();
    assert_eq!(normal.state().editor.active_tab(), RichEditorTab::Markdown);
    assert!(
        normal
            .state()
            .editor
            .state()
            .to_markdown()
            .contains("Euler identity")
    );

    normal.get_by_label("Preview").click();
    normal.step();
    assert_eq!(normal.state().editor.active_tab(), RichEditorTab::Preview);

    // Submit produces serialized InputRichMessage via FinalSubmit + InMemoryMediaResolver.
    normal.get_by_label("Send Rich Message").click();
    normal.step();
    assert!(
        matches!(
            normal.state().last_action,
            Some(RichEditorAction::Submit { .. })
        ),
        "expected Submit action on Send Rich Message click"
    );
}

#[test]
fn formula_widget_size_and_baseline_in_kittest() {
    struct FormulaTestState {
        engine: MathLayoutEngine,
        inline_metrics: Option<FormulaRenderMetrics>,
        display_metrics: Option<FormulaRenderMetrics>,
        inline_rect: egui::Rect,
        display_rect: egui::Rect,
    }

    let state = FormulaTestState {
        engine: MathLayoutEngine::default(),
        inline_metrics: None,
        display_metrics: None,
        inline_rect: egui::Rect::NOTHING,
        display_rect: egui::Rect::NOTHING,
    };

    let mut harness = build_widget_harness(state, Vec2::new(420.0, 220.0), |ui, st| {
        let inline_style = FormulaWidgetStyle::inline(14.0, Color32::BLACK);
        let (r1, m1) = show_formula_widget(ui, &mut st.engine, r"x^2 + y^2", inline_style);
        st.inline_rect = r1.rect;
        st.inline_metrics = Some(m1);

        let display_style = FormulaWidgetStyle::display(16.0, Color32::BLACK);
        let (r2, m2) = show_formula_widget(
            ui,
            &mut st.engine,
            r"\frac{\sqrt{a^2 + b^2}}{2}",
            display_style,
        );
        st.display_rect = r2.rect;
        st.display_metrics = Some(m2);
    });
    harness.step();

    let im = harness.state().inline_metrics.expect("inline metrics");
    let dm = harness.state().display_metrics.expect("display metrics");
    assert!(im.success && dm.success);
    assert!(im.baseline_y > 0.0 && im.baseline_y < im.size.y);
    assert!(dm.baseline_y > im.baseline_y);
    assert!(dm.size.y > im.size.y);
    assert!((harness.state().inline_rect.width() - im.size.x).abs() < 0.5);
    assert!((harness.state().display_rect.height() - dm.size.y).abs() < 0.5);
}

#[test]
fn mock_session_rich_message_send_edit_and_bubble_render() {
    let mut session = MockSession::new(true);
    let peer = session_peers::GROUP_CORE;

    // Build a rich page with a heading, paragraph, and display math block.
    let page = RichPage {
        blocks: vec![
            RichBlock::heading(2, "Quadratic Formula"),
            RichBlock::paragraph("The roots of ax^2 + bx + c = 0 are:"),
            RichBlock::math(r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}"),
        ],
        ..RichPage::default()
    };
    let editor_state = EditorState::from_rich_page(page.clone(), RichMessageLimits::default());
    let resolver = InMemoryMediaResolver::default();
    let ser = editor_state.to_input_rich_message(SerializeRichMessageMode::FinalSubmit, &resolver);
    assert_eq!(ser.status, SerializeRichMessageStatus::Success);
    let input = ser.value.expect("serialized InputRichMessage");

    // Send into MockSession.
    let sent = session
        .send_rich_message(peer, input, page, None, false)
        .expect("send rich message");
    assert!(sent.extra.rich_message.is_some());

    // Round-trip through `to_editable_page()` (which calls `rich_page_from_tl_rich_message`).
    let editable = sent
        .extra
        .rich_message
        .as_ref()
        .expect("rich payload")
        .to_editable_page();
    assert_eq!(editable.blocks.len(), 3);
    assert_eq!(editable.blocks[0].kind, RichBlockKind::Heading);
    assert_eq!(editable.blocks[2].kind, RichBlockKind::Math);

    // Edit the rich message in MockSession.
    let updated_page = RichPage {
        blocks: vec![
            RichBlock::heading(2, "Updated Formula"),
            RichBlock::math(r"E = mc^2"),
        ],
        ..RichPage::default()
    };
    let ed2 = EditorState::from_rich_page(updated_page.clone(), RichMessageLimits::default());
    let input2 = ed2
        .to_input_rich_message(SerializeRichMessageMode::FinalSubmit, &resolver)
        .value
        .expect("serialized edit");
    let edited = session
        .edit_rich_message(peer, sent.id, input2, updated_page)
        .expect("edit rich message");
    assert!(edited.edited);
    assert_eq!(
        edited
            .extra
            .rich_message
            .as_ref()
            .expect("updated payload")
            .page
            .blocks
            .len(),
        2
    );

    // Render the chat history containing the rich message bubble via `HistoryWidget`.
    struct BubbleState {
        history: HistoryWidget,
        session: MockSession,
        palette: Palette,
        strings: EnglishStrings,
    }
    let st = BubbleState {
        history: HistoryWidget::new(),
        session,
        palette: Palette::day(),
        strings: EnglishStrings,
    };
    let mut h = build_widget_harness(st, Vec2::new(760.0, 540.0), move |ui, s| {
        let _ = s.history.show_with_backend(
            ui,
            peer,
            &mut s.session,
            false,
            false,
            false,
            &s.palette,
            &s.strings,
        );
    });
    h.step();
    h.step();

    // Verify rich message is loaded in history and renders cleanly with RichPageView.
    assert!(
        h.state()
            .history
            .inner()
            .messages()
            .iter()
            .any(|m| m.id == sent.id && m.extra.rich_message.is_some()),
        "rich message must be present in HistoryInnerWidget"
    );

    // Also smoke-test direct `paint_rich_page` on a `RichMessagePayload` from TL.
    let payload: RichMessagePayload = edited.extra.rich_message.expect("payload");
    let mut engine = MathLayoutEngine::default();
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
        let _ = paint_rich_page(
            ui,
            "bubble_smoke",
            &mut engine,
            &payload.page,
            320.0,
            &RichPageViewStyle::day(),
        );
    });
    out.textures_delta.clear();
    assert!(
        engine.debug_counters().rendered >= 1,
        "MathLayoutEngine must render the rich message formula"
    );
    let _ = MsgId::SHOW_AT_END;
}
