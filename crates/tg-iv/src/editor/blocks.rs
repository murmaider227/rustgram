//! Port of `iv/editor/iv_editor_page_blocks.h`, `iv/editor/iv_editor_page_blocks.cpp`,
//! `iv/editor/iv_editor_page_list.h`, and `iv/editor/iv_editor_page_list.cpp`.
//!
//! Block constructors, editability predicates, list metadata manipulation, and edit-mode
//! entity/button sanitization rules.

use serde::{Deserialize, Serialize};

use crate::rich_page::{
    EntityInText, EntityType, OrderedListData, OrderedListItemData, RichBlock, RichBlockKind,
    RichButton, RichButtonType, RichListItem, RichListKind, RichPage, RichTableCell, RichTableRow,
    RichTaskState, RichTextNode, TextWithEntities, ordered_roman_supported,
};

/// Block type that can be inserted or converted via editor commands (`Iv::Editor::InsertBlockType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InsertBlockType {
    /// Plain paragraph.
    Paragraph,
    /// Heading levels 1..=6.
    Heading,
    /// Standard blockquote.
    Blockquote,
    /// Pullquote.
    Pullquote,
    /// Fenced code block.
    Code,
    /// Display math formula block.
    Math,
    /// Footer block.
    Footer,
    /// Horizontal divider.
    Divider,
    /// Named anchor.
    Anchor,
    /// Ordered list.
    OrderedList,
    /// Unordered bullet list.
    BulletList,
    /// Task checklist.
    TaskList,
    /// Photo block.
    Photo,
    /// Video block.
    Video,
    /// Audio block.
    Audio,
    /// Collapsible details block.
    Details,
    /// Table block (default 3x3 with header row 0).
    Table,
    /// Geographic map block.
    Map,
}

/// Parameters for inserting or converting a block (`Iv::Editor::InsertAction`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsertAction {
    /// Target block kind.
    pub block_type: InsertBlockType,
    /// Heading level `1..=6` (when `Heading`).
    pub heading_level: u8,
    /// Starting ordinal (when `OrderedList`).
    pub ordered_start: i32,
    /// True if `ordered_start` was explicitly specified by the user.
    pub ordered_start_explicit: bool,
    /// Initial checkbox state (when `TaskList`).
    pub task_checked: bool,
    /// Code language tag (when `Code`).
    pub code_language: String,
    /// Map latitude (when `Map`).
    pub latitude: f64,
    /// Map longitude (when `Map`).
    pub longitude: f64,
}

impl InsertAction {
    /// Creates a default [`InsertAction`] for the given [`InsertBlockType`].
    #[must_use]
    pub fn new(block_type: InsertBlockType) -> Self {
        Self {
            block_type,
            heading_level: 1,
            ordered_start: 1,
            ordered_start_explicit: false,
            task_checked: false,
            code_language: String::new(),
            latitude: 0.0,
            longitude: 0.0,
        }
    }

    /// Convenience constructor for a heading at `level` (`1..=6`).
    #[must_use]
    pub fn heading(level: u8) -> Self {
        Self {
            heading_level: level.clamp(1, 6),
            ..Self::new(InsertBlockType::Heading)
        }
    }

    /// Convenience constructor for a code block with `language`.
    #[must_use]
    pub fn code(language: impl Into<String>) -> Self {
        Self {
            code_language: language.into(),
            ..Self::new(InsertBlockType::Code)
        }
    }
}

/// High-level list style selector (`Iv::Editor::ListStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ListStyle {
    /// Bullet list (`-`).
    Bullet,
    /// Ordered numbered list (`1.`).
    Ordered,
    /// Interactive task checklist (`- [ ]`).
    Task,
}

/// Ordered list marker type (`Iv::Markdown::PreparedOrderedListType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum OrderedListType {
    /// Decimal numbers (`1.`, `2.`, ...).
    #[default]
    Decimal,
    /// Lowercase Latin letters (`a.`, `b.`, ...).
    LowerAlpha,
    /// Uppercase Latin letters (`A.`, `B.`, ...).
    UpperAlpha,
    /// Lowercase Roman numerals (`i.`, `ii.`, ...).
    LowerRoman,
    /// Uppercase Roman numerals (`I.`, `II.`, ...).
    UpperRoman,
}

/// Returns true if `block` owns nested child `blocks` (`BlockCanOwnChildContainer`).
#[must_use]
pub const fn block_can_own_child_container(block: &RichBlock) -> bool {
    matches!(block.kind, RichBlockKind::Quote | RichBlockKind::Details)
}

/// Returns true if `block` has an editable `text` leaf (`BlockSupportsBlockText`).
#[must_use]
pub const fn block_supports_block_text(block: &RichBlock) -> bool {
    matches!(
        block.kind,
        RichBlockKind::Heading
            | RichBlockKind::Paragraph
            | RichBlockKind::Footer
            | RichBlockKind::Code
            | RichBlockKind::Quote
            | RichBlockKind::Table
            | RichBlockKind::Details
    )
}

/// Returns true if `block` has an editable `caption` leaf (`BlockSupportsBlockCaption`).
#[must_use]
pub const fn block_supports_block_caption(block: &RichBlock) -> bool {
    matches!(
        block.kind,
        RichBlockKind::Quote
            | RichBlockKind::Photo
            | RichBlockKind::Video
            | RichBlockKind::Audio
            | RichBlockKind::File
            | RichBlockKind::Map
            | RichBlockKind::GroupedMedia
    )
}

/// Returns true if `kind` can be joined seamlessly with adjacent text blocks (`JoinableTextBlockKind`).
#[must_use]
pub const fn joinable_text_block_kind(kind: RichBlockKind) -> bool {
    matches!(
        kind,
        RichBlockKind::Heading | RichBlockKind::Paragraph | RichBlockKind::Footer
    )
}

/// Returns true if `block` can be edited in the rich editor (`CanEditBlock`).
#[must_use]
pub fn can_edit_block(block: &RichBlock) -> bool {
    match block.kind {
        RichBlockKind::Heading
        | RichBlockKind::Paragraph
        | RichBlockKind::Footer
        | RichBlockKind::Code
        | RichBlockKind::Divider
        | RichBlockKind::Anchor
        | RichBlockKind::ButtonRow
        | RichBlockKind::GroupedMedia
        | RichBlockKind::Photo
        | RichBlockKind::Video
        | RichBlockKind::Audio
        | RichBlockKind::File
        | RichBlockKind::Math
        | RichBlockKind::Table
        | RichBlockKind::Map => true,
        RichBlockKind::Quote | RichBlockKind::Details => can_edit_blocks(&block.blocks),
        RichBlockKind::List => block
            .list_items
            .iter()
            .all(|item| can_edit_blocks(&item.blocks)),
        RichBlockKind::Unsupported
        | RichBlockKind::Thinking
        | RichBlockKind::AuthorDate
        | RichBlockKind::Embed
        | RichBlockKind::EmbedPost
        | RichBlockKind::Channel
        | RichBlockKind::RelatedArticles => false,
    }
}

/// Returns true if every block in `blocks` is editable (`CanEditBlocks`).
#[must_use]
pub fn can_edit_blocks(blocks: &[RichBlock]) -> bool {
    blocks.iter().all(can_edit_block)
}

/// Returns true if `page` is editable in the rich editor (`CanEditRichPage`).
#[must_use]
pub fn can_edit_rich_page(page: &RichPage) -> bool {
    can_edit_blocks(&page.blocks)
}

/// Returns true if converting to `block_type` expands to the entire active line
/// (`BlockConversionExpandsToActiveLine`).
#[must_use]
pub const fn block_conversion_expands_to_active_line(block_type: InsertBlockType) -> bool {
    matches!(
        block_type,
        InsertBlockType::Paragraph
            | InsertBlockType::Heading
            | InsertBlockType::Blockquote
            | InsertBlockType::Pullquote
            | InsertBlockType::Code
            | InsertBlockType::Footer
            | InsertBlockType::OrderedList
            | InsertBlockType::BulletList
            | InsertBlockType::TaskList
    )
}

/// Creates a blank paragraph block (`MakeParagraphBlock`).
#[must_use]
pub fn make_paragraph_block() -> RichBlock {
    RichBlock::new(RichBlockKind::Paragraph)
}

/// Creates a blank footer block (`MakeFooterBlock`).
#[must_use]
pub fn make_footer_block() -> RichBlock {
    RichBlock::new(RichBlockKind::Footer)
}

/// Creates a blank heading block clamped to `1..=6` (`MakeHeadingBlock`).
#[must_use]
pub fn make_heading_block(level: u8) -> RichBlock {
    let mut b = RichBlock::new(RichBlockKind::Heading);
    b.heading_level = level.clamp(1, 6);
    b
}

/// Creates a quote or pullquote block (`MakeQuoteBlock`).
#[must_use]
pub fn make_quote_block(pullquote: bool) -> RichBlock {
    let mut b = RichBlock::new(RichBlockKind::Quote);
    b.pullquote = pullquote;
    b
}

/// Creates a code block (`MakeCodeBlock`).
#[must_use]
pub fn make_code_block(language: impl Into<String>) -> RichBlock {
    let mut b = RichBlock::new(RichBlockKind::Code);
    b.language = language.into();
    b
}

/// Creates an empty display math block (`MakeMathBlock`).
#[must_use]
pub fn make_math_block() -> RichBlock {
    RichBlock::new(RichBlockKind::Math)
}

/// Creates a divider block (`MakeDividerBlock`).
#[must_use]
pub fn make_divider_block() -> RichBlock {
    RichBlock::new(RichBlockKind::Divider)
}

/// Creates a named anchor block (`MakeAnchorBlock`).
#[must_use]
pub fn make_anchor_block(anchor_id: impl Into<String>) -> RichBlock {
    let mut b = RichBlock::new(RichBlockKind::Anchor);
    b.anchor_id = anchor_id.into();
    b
}

/// Creates a single-item list block (`MakeListBlock`).
#[must_use]
pub fn make_list_block(kind: RichListKind, task_state: RichTaskState) -> RichBlock {
    let mut b = RichBlock::new(RichBlockKind::List);
    b.list_kind = kind;
    b.list_items.push(RichListItem {
        task_state,
        ..RichListItem::default()
    });
    b
}

/// Creates a collapsible details block containing one empty paragraph (`MakeDetailsBlock`).
#[must_use]
pub fn make_details_block() -> RichBlock {
    let mut b = RichBlock::new(RichBlockKind::Details);
    b.open = true;
    b.blocks.push(make_paragraph_block());
    b
}

/// Creates a default 3x3 bordered table with header row 0 (`MakeTableBlock`).
#[must_use]
pub fn make_table_block() -> RichBlock {
    let mut b = RichBlock::new(RichBlockKind::Table);
    b.bordered = true;
    b.table_rows.reserve(3);
    for row_idx in 0..3 {
        let mut cells = Vec::with_capacity(3);
        for _ in 0..3 {
            cells.push(RichTableCell {
                header: row_idx == 0,
                ..RichTableCell::default()
            });
        }
        b.table_rows.push(RichTableRow { cells });
    }
    b
}

/// Creates a map block (`MakeMapBlock`).
#[must_use]
pub fn make_map_block(latitude: f64, longitude: f64) -> RichBlock {
    let mut b = RichBlock::new(RichBlockKind::Map);
    b.latitude = latitude;
    b.longitude = longitude;
    b.zoom = 14;
    b
}

/// Builds a new [`RichBlock`] for an [`InsertAction`].
#[must_use]
pub fn make_block_for_action(action: &InsertAction) -> RichBlock {
    match action.block_type {
        InsertBlockType::Paragraph => make_paragraph_block(),
        InsertBlockType::Heading => make_heading_block(action.heading_level),
        InsertBlockType::Blockquote => make_quote_block(false),
        InsertBlockType::Pullquote => make_quote_block(true),
        InsertBlockType::Code => make_code_block(&action.code_language),
        InsertBlockType::Math => make_math_block(),
        InsertBlockType::Footer => make_footer_block(),
        InsertBlockType::Divider => make_divider_block(),
        InsertBlockType::Anchor => make_anchor_block(""),
        InsertBlockType::OrderedList => {
            let mut b = make_list_block(RichListKind::Ordered, RichTaskState::None);
            if action.ordered_start_explicit || action.ordered_start != 1 {
                b.ordered_list.start = Some(action.ordered_start);
            }
            b
        }
        InsertBlockType::BulletList => make_list_block(RichListKind::Bullet, RichTaskState::None),
        InsertBlockType::TaskList => make_list_block(
            RichListKind::Bullet,
            if action.task_checked {
                RichTaskState::Checked
            } else {
                RichTaskState::Unchecked
            },
        ),
        InsertBlockType::Photo => RichBlock::new(RichBlockKind::Photo),
        InsertBlockType::Video => RichBlock::new(RichBlockKind::Video),
        InsertBlockType::Audio => RichBlock::new(RichBlockKind::Audio),
        InsertBlockType::Details => make_details_block(),
        InsertBlockType::Table => make_table_block(),
        InsertBlockType::Map => make_map_block(action.latitude, action.longitude),
    }
}

/// Returns true if a list item contains no visible content (`ListItemIsEmpty`).
#[must_use]
pub fn list_item_is_empty(item: &RichListItem) -> bool {
    item.text.is_structurally_empty()
        && item.anchor_id.is_empty()
        && item.blocks.iter().all(block_is_empty)
}

/// Returns true if a block contains no visible text, media, or non-empty children (`BlockIsEmpty`).
#[must_use]
pub fn block_is_empty(block: &RichBlock) -> bool {
    if !block.text.is_structurally_empty()
        || !block.caption.is_structurally_empty()
        || !block.formula.trim().is_empty()
        || !block.language.is_empty()
        || !block.anchor_id.is_empty()
        || !block.url.is_empty()
        || !block.html.is_empty()
        || !block.author.is_empty()
        || !block.username.is_empty()
        || !block.channel_title.is_empty()
        || !block.audio_title.is_empty()
        || !block.audio_performer.is_empty()
        || !block.file_name.is_empty()
        || block.photo_id != 0
        || block.document_id != 0
        || block.channel_id != 0
        || block.access_hash != 0
        || block.latitude != 0.0
        || block.longitude != 0.0
        || !block.media_items.is_empty()
        || !block.buttons.is_empty()
    {
        return false;
    }
    if !block.blocks.iter().all(block_is_empty) {
        return false;
    }
    if !block.list_items.iter().all(list_item_is_empty) {
        return false;
    }
    block
        .table_rows
        .iter()
        .all(|r| r.cells.iter().all(|c| c.text.is_structurally_empty()))
}

// ---- List helpers (`iv_editor_page_list.cpp`) ----

/// Returns true if any item in `items` has a task checkbox (`IsTaskList`).
#[must_use]
pub fn is_task_list(items: &[RichListItem]) -> bool {
    items.iter().any(|i| i.task_state != RichTaskState::None)
}

/// Returns the current [`ListStyle`] of a list block (`CurrentListStyle`).
#[must_use]
pub fn current_list_style(block: &RichBlock) -> ListStyle {
    if block.list_kind == RichListKind::Ordered {
        ListStyle::Ordered
    } else if is_task_list(&block.list_items) {
        ListStyle::Task
    } else {
        ListStyle::Bullet
    }
}

/// Resolves an optional TL list type string to [`OrderedListType`] (`ResolvePreparedOrderedListType`).
#[must_use]
pub fn resolve_ordered_list_type(list_type: Option<&str>) -> OrderedListType {
    let Some(v) = list_type else {
        return OrderedListType::Decimal;
    };
    if v == "a" || v.eq_ignore_ascii_case("lower-alpha") || v.eq_ignore_ascii_case("lower-latin") {
        OrderedListType::LowerAlpha
    } else if v == "A"
        || v.eq_ignore_ascii_case("upper-alpha")
        || v.eq_ignore_ascii_case("upper-latin")
    {
        OrderedListType::UpperAlpha
    } else if v == "i" || v.eq_ignore_ascii_case("lower-roman") {
        OrderedListType::LowerRoman
    } else if v == "I" || v.eq_ignore_ascii_case("upper-roman") {
        OrderedListType::UpperRoman
    } else {
        OrderedListType::Decimal
    }
}

/// Serializes an [`OrderedListType`] back to its stored TL marker type string (`StoredOrderedListType`).
#[must_use]
pub fn stored_ordered_list_type(ty: OrderedListType, explicit_decimal: bool) -> Option<String> {
    match ty {
        OrderedListType::LowerAlpha => Some("a".to_string()),
        OrderedListType::UpperAlpha => Some("A".to_string()),
        OrderedListType::LowerRoman => Some("i".to_string()),
        OrderedListType::UpperRoman => Some("I".to_string()),
        OrderedListType::Decimal => explicit_decimal.then(|| "1".to_string()),
    }
}

/// Computes the effective numeric value of `item_index` in an ordered list (`EffectiveOrderedItemValue`).
#[must_use]
pub fn effective_ordered_item_value(block: &RichBlock, item_index: usize) -> Option<i32> {
    if block.kind != RichBlockKind::List
        || block.list_kind != RichListKind::Ordered
        || item_index >= block.list_items.len()
    {
        return None;
    }
    let mut next = block
        .ordered_list
        .start
        .unwrap_or(if block.ordered_list.reversed {
            block.list_items.len() as i32
        } else {
            1
        });
    let step = if block.ordered_list.reversed { -1 } else { 1 };
    for (i, item) in block.list_items.iter().enumerate() {
        let val = item.number.value.unwrap_or(next);
        if i == item_index {
            return Some(val);
        }
        next = val + step;
    }
    None
}

/// Returns true if any item in `[from, till)` has a value supporting Roman numerals
/// (`OrderedItemsSupportRoman`).
#[must_use]
pub fn ordered_items_support_roman(block: &RichBlock, from: usize, till: usize) -> bool {
    let mut next = block
        .ordered_list
        .start
        .unwrap_or(if block.ordered_list.reversed {
            block.list_items.len() as i32
        } else {
            1
        });
    let step = if block.ordered_list.reversed { -1 } else { 1 };
    for (i, item) in block.list_items.iter().enumerate().take(till) {
        let val = item.number.value.unwrap_or(next);
        if i >= from && ordered_roman_supported(val) {
            return true;
        }
        next = val + step;
    }
    false
}

/// Checkbox state assigned to a newly split list item (`SplitTaskState`): Checked becomes Unchecked.
#[must_use]
pub const fn split_task_state(state: RichTaskState) -> RichTaskState {
    match state {
        RichTaskState::Checked => RichTaskState::Unchecked,
        other => other,
    }
}

/// Returns true if two adjacent lists can be joined seamlessly (`ListsJoinSeamlessly`).
#[must_use]
pub fn lists_join_seamlessly(
    first: &RichBlock,
    second: &RichBlock,
    second_start_explicit: bool,
) -> bool {
    if first.kind != RichBlockKind::List
        || second.kind != RichBlockKind::List
        || first.list_kind != second.list_kind
        || is_task_list(&first.list_items) != is_task_list(&second.list_items)
        || first.list_items.is_empty()
        || second.list_items.is_empty()
    {
        return false;
    }
    if first.list_kind != RichListKind::Ordered {
        return true;
    }
    if first.ordered_list.reversed || second.ordered_list.reversed {
        return false;
    }
    if resolve_ordered_list_type(first.ordered_list.list_type.as_deref())
        != resolve_ordered_list_type(second.ordered_list.list_type.as_deref())
    {
        return false;
    }
    let own_numbering = second_start_explicit
        || second.ordered_list.start.is_some()
        || second
            .list_items
            .iter()
            .any(|i| i.number != OrderedListItemData::default());
    if !own_numbering {
        return true;
    }
    let last = effective_ordered_item_value(first, first.list_items.len() - 1);
    let next = effective_ordered_item_value(second, 0);
    matches!((last, next), (Some(l), Some(n)) if l + 1 == n)
}

/// Clears ordered-list metadata when converting a list to bullet or task style
/// (`ResetNonOrderedListMetadata`).
pub fn reset_non_ordered_list_metadata(block: &mut RichBlock) -> bool {
    if block.kind != RichBlockKind::List {
        return false;
    }
    let mut changed = block.ordered_list != OrderedListData::default();
    block.ordered_list = OrderedListData::default();
    for item in &mut block.list_items {
        if item.number != OrderedListItemData::default() {
            item.number = OrderedListItemData::default();
            changed = true;
        }
    }
    changed
}

// ---- Edit-mode entity sanitization (`iv_editor_page_blocks.cpp`) ----

const fn is_edit_mode_wrapper_entity(kind: EntityType) -> bool {
    matches!(
        kind,
        EntityType::Url
            | EntityType::Email
            | EntityType::Hashtag
            | EntityType::Cashtag
            | EntityType::Mention
            | EntityType::BotCommand
            | EntityType::Phone
            | EntityType::BankCard
            | EntityType::TonAddress
    )
}

/// Strips auto-detected wrapper entities (`Url`, `Email`, `Hashtag`, etc.) that the server
/// re-detects on send (`StripEditModeWrapperEntities`).
pub fn strip_edit_mode_wrapper_entities(text: &mut TextWithEntities) {
    text.entities
        .retain(|e| !is_edit_mode_wrapper_entity(e.kind));
}

/// Strips auto-detected wrapper entities recursively across a block slice.
pub fn strip_edit_mode_wrapper_entities_in_blocks(blocks: &mut [RichBlock]) {
    for block in blocks {
        strip_edit_mode_wrapper_entities(&mut block.text.text);
        strip_edit_mode_wrapper_entities(&mut block.caption.text);
        strip_edit_mode_wrapper_entities_in_blocks(&mut block.blocks);
        for item in &mut block.list_items {
            strip_edit_mode_wrapper_entities(&mut item.text.text);
            strip_edit_mode_wrapper_entities_in_blocks(&mut item.blocks);
        }
        for row in &mut block.table_rows {
            for cell in &mut row.cells {
                strip_edit_mode_wrapper_entities(&mut cell.text.text);
            }
        }
    }
}

/// Degrades block-only entities (`Pre` -> `Code`, drops `Blockquote`) when pasting into inline text
/// (`DegradeBlockOnlyEntities`).
pub fn degrade_block_only_entities(text: &mut TextWithEntities) -> bool {
    let mut changed = false;
    let mut out = Vec::with_capacity(text.entities.len());
    for entity in std::mem::take(&mut text.entities) {
        match entity.kind {
            EntityType::Pre => {
                out.push(EntityInText::new(
                    EntityType::Code,
                    entity.offset,
                    entity.length,
                ));
                changed = true;
            }
            EntityType::Blockquote => {
                changed = true;
            }
            _ => out.push(entity),
        }
    }
    text.entities = out;
    changed
}

/// Degrades non-user-sendable buttons (e.g. bot callbacks or `internal:` URLs) to `Disabled`
/// (`DegradeEditModeButtons`).
pub fn degrade_edit_mode_buttons(blocks: &mut [RichBlock]) -> bool {
    let mut changed = false;
    for block in blocks {
        for btn in &mut block.buttons {
            if degrade_button(btn) {
                changed = true;
            }
        }
        changed |= degrade_edit_mode_buttons(&mut block.blocks);
        for item in &mut block.list_items {
            changed |= degrade_edit_mode_buttons(&mut item.blocks);
        }
    }
    changed
}

fn degrade_button(btn: &mut RichButton) -> bool {
    let sendable = match btn.button_type {
        RichButtonType::Disabled | RichButtonType::UserProfile | RichButtonType::CopyText => true,
        RichButtonType::Url => {
            let s = String::from_utf8_lossy(&btn.data);
            !s.trim().to_ascii_lowercase().starts_with("internal:")
        }
        _ => false,
    };
    if sendable {
        return false;
    }
    btn.button_type = RichButtonType::Disabled;
    btn.data.clear();
    true
}

/// Merges anchor IDs from `source` into `target` (`MergeRichTextAnchors`).
pub fn merge_rich_text_anchors(target: &mut RichTextNode, source: RichTextNode) {
    target.add_anchor(source.anchor_id);
    for extra in source.anchor_ids {
        target.add_anchor(extra);
    }
}
