//! Text prepared once and painted on many frames.
//!
//! Like tdesktop's `Ui::Text::String`: the layout job, the galley and the link / spoiler
//! rectangles of a text are built when it is first drawn and kept by the caller. A later frame
//! reuses them while nothing they depend on has changed: the text itself, the style, the
//! colours, the wrap width, the hover / spoiler state and the font atlas ([`fonts_epoch`]).
//! What is painted is the same galley the per-frame code laid out, so pixels do not change.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use egui::text::LayoutJob;
use egui::{Color32, CursorIcon, FontId, Galley, Id, Pos2, Rect, Sense, Ui};
use tg_style::Icon as TgIcon;
use tg_text::{EntityInText, EntityType, TextWithEntities};

use super::{
    EntityTextResponse, LaidOutRichText, LegacySegment, RichText, RichTextOptions,
    RichTextResponse, byte_range_rects, layout_job_for_segments,
    layout_text_with_entities_expanded, split_legacy_segments, spoiler_reveal_progress,
};
use crate::custom_emoji::{CustomEmojiRenderer, has_registered_custom_emoji};
use crate::effects::{merge_spoiler_rects, paint_text_spoiler_selected};
use crate::style::{QuoteStyle, TextPalette, TextStyle};

thread_local! {
    static PREPARATIONS: Cell<u64> = const { Cell::new(0) };
}

/// How many times text was prepared on this thread: a layout job built from formatted text
/// ([`super::build_layout_job`], a [`PreparedRichText`] galley) or a whole entity layout
/// ([`super::layout_text_with_entities_expanded`]). Tests and profiling read the difference
/// across frames: an idle frame of a prepared text adds nothing.
#[must_use]
pub fn text_preparation_count() -> u64 {
    PREPARATIONS.with(Cell::get)
}

/// Counts one text preparation done outside this module (e.g. converting entities to spans).
pub fn note_text_preparation() {
    PREPARATIONS.with(|count| count.set(count.get().wrapping_add(1)));
}

#[derive(Clone)]
struct FontsEpochState {
    pass: u64,
    epoch: u64,
    canary: Arc<Galley>,
}

static NEXT_FONTS_EPOCH: AtomicU64 = AtomicU64::new(1);

/// A value that changes whenever a galley laid out earlier may no longer be painted: the font
/// atlas was rebuilt (fonts replaced, text options changed, atlas full), the scale changed, or
/// egui dropped its galley cache for another reason.
///
/// A galley holds positions in the font atlas, so one kept across frames must be laid out again
/// after such a change. egui has no counter for it; this lays out a one-glyph canary job once
/// per pass and watches whether egui's galley cache still returns the same galley. The cache is
/// cleared in every case above. If the canary is merely evicted (no caller asked during a pass)
/// the epoch moves too, which only costs one extra layout of the texts shown next.
#[must_use]
pub fn fonts_epoch(ctx: &egui::Context) -> u64 {
    let id = Id::unique(("tg_ui_fonts_epoch", ctx.viewport_id()));
    let pass = ctx.cumulative_pass_nr();
    let known = ctx.data(|d| d.get_temp::<FontsEpochState>(id));
    if let Some(state) = &known
        && state.pass == pass
    {
        return state.epoch;
    }
    let canary = ctx.fonts_mut(|f| {
        f.layout_job(LayoutJob::simple_singleline(
            "a".to_owned(),
            FontId::proportional(13.0),
            Color32::WHITE,
        ))
    });
    let epoch = match known {
        Some(state) if Arc::ptr_eq(&state.canary, &canary) => state.epoch,
        _ => NEXT_FONTS_EPOCH.fetch_add(1, Ordering::Relaxed),
    };
    ctx.data_mut(|d| {
        d.insert_temp(
            id,
            FontsEpochState {
                pass,
                epoch,
                canary,
            },
        );
    });
    epoch
}

fn same_style(a: &TextStyle, b: &TextStyle) -> bool {
    let TextStyle {
        font,
        link_underline,
        line_height,
    } = *a;
    font == b.font && link_underline == b.link_underline && line_height == b.line_height
}

fn same_palette(a: &TextPalette, b: &TextPalette) -> bool {
    let TextPalette {
        link_fg,
        mono_fg,
        mark_bg,
        spoiler_fg,
        select_bg,
        select_fg,
        select_link_fg,
        select_mono_fg,
        select_spoiler_fg,
        select_overlay,
        link_always_active,
    } = *a;
    link_fg == b.link_fg
        && mono_fg == b.mono_fg
        && mark_bg == b.mark_bg
        && spoiler_fg == b.spoiler_fg
        && select_bg == b.select_bg
        && select_fg == b.select_fg
        && select_link_fg == b.select_link_fg
        && select_mono_fg == b.select_mono_fg
        && select_spoiler_fg == b.select_spoiler_fg
        && select_overlay == b.select_overlay
        && link_always_active == b.link_always_active
}

/// Icons are `'static` style data: the same icon is the same slice.
fn same_icon(a: TgIcon, b: TgIcon) -> bool {
    std::ptr::eq(a.parts, b.parts)
}

fn same_quote_style(a: &QuoteStyle, b: &QuoteStyle) -> bool {
    let QuoteStyle {
        padding,
        vertical_skip,
        header,
        header_position,
        icon,
        icon_position,
        expand,
        expand_position,
        collapse,
        collapse_position,
        outline,
        outline_shift,
        radius,
        scrollable,
    } = *a;
    padding == b.padding
        && vertical_skip == b.vertical_skip
        && header == b.header
        && header_position == b.header_position
        && same_icon(icon, b.icon)
        && icon_position == b.icon_position
        && same_icon(expand, b.expand)
        && expand_position == b.expand_position
        && same_icon(collapse, b.collapse)
        && collapse_position == b.collapse_position
        && outline == b.outline
        && outline_shift == b.outline_shift
        && radius == b.radius
        && scrollable == b.scrollable
}

/// Whether two option sets lay text out the same way. `selection`, `selectable` and
/// `pause_custom_emoji` only matter when painting.
fn same_layout_options(a: &RichTextOptions, b: &RichTextOptions) -> bool {
    let RichTextOptions {
        max_width,
        max_lines,
        elide,
        selectable: _,
        base_color,
        selection: _,
        spoilers_revealed,
        hovered_link_index,
        quote_bg,
        quote_outline,
        pre_bg,
        pre_outline,
        blockquote_style,
        pre_style,
        syntax_palette,
        pause_custom_emoji: _,
        // The caller's reveal progress and expanded quotes reach the layout as
        // `spoilers_revealed` and the `expanded_blockquotes` argument.
        spoiler_reveal: _,
        expanded_blockquotes: _,
        external_selection: _,
    } = a;
    *max_width == b.max_width
        && *max_lines == b.max_lines
        && *elide == b.elide
        && *base_color == b.base_color
        && *spoilers_revealed == b.spoilers_revealed
        && *hovered_link_index == b.hovered_link_index
        && *quote_bg == b.quote_bg
        && *quote_outline == b.quote_outline
        && *pre_bg == b.pre_bg
        && *pre_outline == b.pre_outline
        && same_quote_style(blockquote_style, &b.blockquote_style)
        && same_quote_style(pre_style, &b.pre_style)
        && *syntax_palette == b.syntax_palette
}

/// What the galleys of a [`RichTextCache`] were laid out for.
#[derive(Clone, Copy)]
struct PreparedFor {
    style: TextStyle,
    palette: TextPalette,
    base_color: Color32,
    wrap_w: f32,
    epoch: u64,
}

impl PreparedFor {
    fn matches(
        &self,
        style: &TextStyle,
        palette: &TextPalette,
        base_color: Color32,
        wrap_w: f32,
        epoch: u64,
    ) -> bool {
        self.epoch == epoch
            && self.wrap_w == wrap_w
            && self.base_color == base_color
            && same_style(&self.style, style)
            && same_palette(&self.palette, palette)
    }
}

/// Link and spoiler rectangles of the shown galley placed at `origin`.
struct HitRects {
    origin: Pos2,
    /// `(rect, index of the link's segment)`.
    links: Vec<(Rect, usize)>,
    spoilers: Vec<Rect>,
}

/// The galley painted last, for one hover / spoiler state.
struct ShownGalley {
    hovered: Option<String>,
    revealed: bool,
    galley: Arc<Galley>,
    hits: Option<HitRects>,
}

/// Everything derived from one [`RichText`] that later frames can reuse.
#[derive(Default)]
pub(super) struct RichTextCache {
    /// Segments depend only on the text, so they survive style and width changes.
    segments: Option<Vec<LegacySegment>>,
    prepared_for: Option<PreparedFor>,
    shown: Option<ShownGalley>,
    /// Galley with no hover underline and revealed spoilers (selection and drag hit-tests).
    plain: Option<Arc<Galley>>,
}

impl RichTextCache {
    /// Drops the galleys laid out for other parameters.
    fn validate(
        &mut self,
        ctx: &egui::Context,
        style: &TextStyle,
        palette: &TextPalette,
        base_color: Color32,
        wrap_w: f32,
    ) {
        let epoch = fonts_epoch(ctx);
        if self
            .prepared_for
            .as_ref()
            .is_some_and(|p| p.matches(style, palette, base_color, wrap_w, epoch))
        {
            return;
        }
        self.shown = None;
        self.plain = None;
        self.prepared_for = Some(PreparedFor {
            style: *style,
            palette: *palette,
            base_color,
            wrap_w,
            epoch,
        });
    }
}

/// A [`RichText`] with its prepared layout: build it once (when the message is laid out or first
/// painted), keep it next to the item, and paint it with [`render_prepared_rich_text`].
pub struct PreparedRichText {
    rich: RichText,
    cache: RichTextCache,
}

impl std::fmt::Debug for PreparedRichText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedRichText")
            .field("text_len", &self.rich.text.len())
            .field("spans", &self.rich.spans.len())
            .field("has_galley", &self.cache.shown.is_some())
            .finish()
    }
}

impl PreparedRichText {
    /// Takes the text; nothing is laid out until it is painted.
    #[must_use]
    pub fn new(rich: RichText) -> Self {
        Self {
            rich,
            cache: RichTextCache::default(),
        }
    }

    /// The text this was prepared from.
    #[must_use]
    pub const fn rich(&self) -> &RichText {
        &self.rich
    }

    /// The galley of [`super::build_layout_job`] with no hovered link and revealed spoilers,
    /// wrapped at `wrap_width`: what selection rectangles and drag hit-tests are computed on.
    pub fn plain_galley(
        &mut self,
        ui: &Ui,
        style: &TextStyle,
        palette: &TextPalette,
        base_color: Color32,
        wrap_width: f32,
    ) -> Arc<Galley> {
        // The same width `render_prepared_rich_text` lays out for, so the two share one cache.
        let wrap_w = wrap_width.max(1.0);
        self.cache
            .validate(ui.ctx(), style, palette, base_color, wrap_w);
        if let Some(galley) = &self.cache.plain {
            return Arc::clone(galley);
        }
        let rich = &self.rich;
        let segments = self
            .cache
            .segments
            .get_or_insert_with(|| split_legacy_segments(rich));
        let job = layout_job_for_segments(
            &rich.text, segments, style, palette, base_color, wrap_w, None, true,
        );
        let galley = ui.painter().layout_job(job);
        self.cache.plain = Some(Arc::clone(&galley));
        galley
    }
}

/// [`super::render_rich_text`] for a text prepared earlier: same widget, same pixels, but the
/// layout job and the hit rectangles are only rebuilt when their inputs change.
pub fn render_prepared_rich_text(
    ui: &mut Ui,
    id: Id,
    prepared: &mut PreparedRichText,
    style: &TextStyle,
    palette: &TextPalette,
    base_color: Color32,
    max_width: f32,
) -> RichTextResponse {
    render_rich_text_cached(
        ui,
        id,
        &prepared.rich,
        &mut prepared.cache,
        style,
        palette,
        base_color,
        max_width,
        None,
        &[],
    )
}

/// [`render_prepared_rich_text`] for a caller that keeps the spoiler state itself (a chat
/// message: tdesktop hides its spoilers again when the chat changes). `reveal` is how far the
/// spoilers have faded in (`0` hidden, `1` revealed, see [`super::spoiler_reveal_progress`]);
/// a click on a hidden spoiler only sets [`RichTextResponse::spoiler_toggled`].
#[allow(clippy::too_many_arguments)]
pub fn render_prepared_rich_text_with_reveal(
    ui: &mut Ui,
    id: Id,
    prepared: &mut PreparedRichText,
    style: &TextStyle,
    palette: &TextPalette,
    base_color: Color32,
    max_width: f32,
    reveal: f32,
) -> RichTextResponse {
    render_rich_text_cached(
        ui,
        id,
        &prepared.rich,
        &mut prepared.cache,
        style,
        palette,
        base_color,
        max_width,
        Some(reveal),
        &[],
    )
}

/// [`render_prepared_rich_text_with_reveal`] (or the self-kept spoiler state with `reveal`
/// `None`) for text of which the `selected` screen rectangles are selected: the spoiler cover
/// over them is drawn in `palette.select_spoiler_fg` (`Renderer::pushSpoilerRange`).
#[allow(clippy::too_many_arguments)]
pub fn render_prepared_rich_text_selected(
    ui: &mut Ui,
    id: Id,
    prepared: &mut PreparedRichText,
    style: &TextStyle,
    palette: &TextPalette,
    base_color: Color32,
    max_width: f32,
    reveal: Option<f32>,
    selected: &[Rect],
) -> RichTextResponse {
    render_rich_text_cached(
        ui,
        id,
        &prepared.rich,
        &mut prepared.cache,
        style,
        palette,
        base_color,
        max_width,
        reveal,
        selected,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_rich_text_cached(
    ui: &mut Ui,
    id: Id,
    rich: &RichText,
    cache: &mut RichTextCache,
    style: &TextStyle,
    palette: &TextPalette,
    base_color: Color32,
    max_width: f32,
    external_reveal: Option<f32>,
    selected: &[Rect],
) -> RichTextResponse {
    let reveal_id = id.with("_tg_spoiler_revealed_at");
    let hover_link_id = id.with("_tg_hover_link");
    let now = ui.input(|i| i.time);
    let reveal = match external_reveal {
        Some(progress) => progress.clamp(0.0, 1.0),
        None => ui
            .ctx()
            .data_mut(|d| d.get_temp::<f64>(reveal_id))
            .map_or(0.0, |at| spoiler_reveal_progress(now - at)),
    };
    let revealed = reveal >= 1.0;
    // A spoiler takes the click only while it is fully hidden.
    let spoilers_clickable = reveal <= 0.0;
    if !revealed && !spoilers_clickable {
        ui.ctx().request_repaint();
    }
    let prev_hovered = ui
        .ctx()
        .data_mut(|d| d.get_temp::<Option<String>>(hover_link_id))
        .unwrap_or(None);

    let wrap_w = max_width.max(1.0);
    cache.validate(ui.ctx(), style, palette, base_color, wrap_w);
    let RichTextCache {
        segments,
        shown,
        plain,
        ..
    } = cache;
    let segments: &[LegacySegment] = segments.get_or_insert_with(|| split_legacy_segments(rich));

    // The hover underline and the spoiler reveal are part of the job, so each state has its own
    // galley; a state change lays the text out again, an unchanged state reuses the galley.
    if !shown
        .as_ref()
        .is_some_and(|s| s.revealed == revealed && s.hovered == prev_hovered)
    {
        let job = layout_job_for_segments(
            &rich.text,
            segments,
            style,
            palette,
            base_color,
            wrap_w,
            prev_hovered.as_deref(),
            revealed,
        );
        *shown = Some(ShownGalley {
            hovered: prev_hovered.clone(),
            revealed,
            galley: ui.painter().layout_job(job),
            hits: None,
        });
    }
    let Some(shown) = shown.as_mut() else {
        return RichTextResponse::default();
    };
    let galley = Arc::clone(&shown.galley);
    let desired = galley.size();
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());

    // Rectangles are computed for the galley's position, exactly as the per-frame code did, so
    // they are kept while the text stays where it is (an idle frame) and redone when it moves.
    if !shown.hits.as_ref().is_some_and(|h| h.origin == rect.min) {
        let mut links: Vec<(Rect, usize)> = Vec::new();
        let mut spoilers: Vec<Rect> = Vec::new();
        for (index, seg) in segments.iter().enumerate() {
            if seg.link.is_none() && !seg.spoiler {
                continue;
            }
            let rects = byte_range_rects(&galley, rich.text.as_str(), seg.start, seg.end, rect.min);
            if seg.link.is_some() {
                links.extend(rects.iter().map(|r| (*r, index)));
            }
            if seg.spoiler {
                spoilers.extend(rects.iter().copied());
            }
        }
        shown.hits = Some(HitRects {
            origin: rect.min,
            links,
            spoilers: merge_spoiler_rects(&spoilers),
        });
    }
    let (link_hits, spoiler_rects): (&[(Rect, usize)], &[Rect]) = match &shown.hits {
        Some(hits) => (&hits.links, &hits.spoilers),
        None => (&[], &[]),
    };
    let link_at = |pos: Pos2| -> Option<&str> {
        link_hits
            .iter()
            .find(|(r, _)| r.contains(pos))
            .and_then(|(_, index)| segments.get(*index))
            .and_then(|seg| seg.link.as_deref())
    };

    let pointer = ui.ctx().pointer_hover_pos();
    let mut new_hovered_link: Option<&str> = None;
    let mut hovering_spoiler = false;
    if let Some(pos) = pointer
        && rect.contains(pos)
    {
        if spoilers_clickable && spoiler_rects.iter().any(|r| r.contains(pos)) {
            hovering_spoiler = true;
        } else {
            new_hovered_link = link_at(pos);
        }
    }

    if hovering_spoiler || new_hovered_link.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    if new_hovered_link != prev_hovered.as_deref() {
        let stored = new_hovered_link.map(str::to_owned);
        ui.ctx().data_mut(|d| d.insert_temp(hover_link_id, stored));
        ui.ctx().request_repaint();
    }

    let mut out = RichTextResponse {
        clicked_link: None,
        spoiler_toggled: false,
        rect,
    };

    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        if spoilers_clickable && spoiler_rects.iter().any(|r| r.contains(pos)) {
            out.spoiler_toggled = true;
            if external_reveal.is_none() {
                ui.ctx().data_mut(|d| d.insert_temp(reveal_id, now));
            }
            ui.ctx().request_repaint();
        } else {
            out.clicked_link = link_at(pos).map(str::to_owned);
        }
    }

    if ui.is_rect_visible(rect) {
        ui.painter().galley(rect.min, galley, base_color);

        if !revealed && !spoiler_rects.is_empty() {
            if !spoilers_clickable {
                // The spoilers fade in: the revealed text over them (`_spoilerOpacity`).
                let revealed_galley = Arc::clone(plain.get_or_insert_with(|| {
                    let job = layout_job_for_segments(
                        &rich.text, segments, style, palette, base_color, wrap_w, None, true,
                    );
                    ui.painter().layout_job(job)
                }));
                for srect in spoiler_rects {
                    let mut fade = ui
                        .painter()
                        .with_clip_rect(ui.painter().clip_rect().intersect(*srect));
                    fade.multiply_opacity(reveal);
                    fade.galley(rect.min, Arc::clone(&revealed_galley), base_color);
                }
            }
            paint_text_spoiler_selected(
                ui.ctx(),
                ui.painter(),
                spoiler_rects,
                selected,
                rect.min,
                palette.spoiler_fg,
                palette.select_spoiler_fg,
                1.0 - reveal,
            );
        }
    }

    out
}

/// Which custom emoji of `source` have a registered texture: the layout hides the fallback
/// glyph of those, so it has to be redone when one of them gets its texture.
fn custom_emoji_registered(entities: &[EntityInText]) -> Vec<bool> {
    entities
        .iter()
        .filter(|e| e.entity_type() == EntityType::CustomEmoji)
        .map(|e| has_registered_custom_emoji(e.data()))
        .collect()
}

struct EntityLayoutEntry {
    style: TextStyle,
    palette: TextPalette,
    options: RichTextOptions,
    expanded_blockquotes: Vec<usize>,
    emoji_registered: Vec<bool>,
    epoch: u64,
    laid: LaidOutRichText,
}

impl EntityLayoutEntry {
    fn is_for(
        &self,
        epoch: u64,
        emoji_registered: &[bool],
        style: &TextStyle,
        palette: &TextPalette,
        options: &RichTextOptions,
        expanded_blockquotes: &[usize],
    ) -> bool {
        self.epoch == epoch
            && self.expanded_blockquotes == expanded_blockquotes
            && self.emoji_registered == emoji_registered
            && same_style(&self.style, style)
            && same_palette(&self.palette, palette)
            && same_layout_options(&self.options, options)
    }
}

/// The last [`LaidOutRichText`] of one text, reused while its inputs stay the same.
#[derive(Default)]
pub(super) struct EntityLayoutCache {
    entry: Option<EntityLayoutEntry>,
    /// The revealed layout painted over the spoilers while they fade in; it becomes `entry`
    /// when the fade ends.
    fade: Option<EntityLayoutEntry>,
}

impl EntityLayoutCache {
    /// The revealed layout for the spoiler fade, kept in its own slot so the hidden layout in
    /// `entry` stays too: neither is laid out again on the frames of the fade.
    pub(super) fn fade_layout(
        &mut self,
        ctx: &egui::Context,
        source: &TextWithEntities,
        style: &TextStyle,
        palette: &TextPalette,
        options: &RichTextOptions,
        expanded_blockquotes: &[usize],
    ) -> &LaidOutRichText {
        let epoch = fonts_epoch(ctx);
        let emoji_registered = custom_emoji_registered(&source.entities);
        if !self.fade.as_ref().is_some_and(|e| {
            e.is_for(
                epoch,
                &emoji_registered,
                style,
                palette,
                options,
                expanded_blockquotes,
            )
        }) {
            self.fade = None;
        }
        let entry = self.fade.get_or_insert_with(|| EntityLayoutEntry {
            style: *style,
            palette: *palette,
            options: options.clone(),
            expanded_blockquotes: expanded_blockquotes.to_vec(),
            emoji_registered,
            epoch,
            laid: layout_text_with_entities_expanded(
                ctx,
                source,
                style,
                palette,
                options,
                expanded_blockquotes,
            ),
        });
        &entry.laid
    }

    /// Drops the fade layout once no fade is running.
    pub(super) fn clear_fade(&mut self) {
        self.fade = None;
    }

    /// The layout of `source` for these parameters: the kept one, or a new one.
    ///
    /// The caller guarantees `source` is the text this cache belongs to.
    pub(super) fn layout(
        &mut self,
        ctx: &egui::Context,
        source: &TextWithEntities,
        style: &TextStyle,
        palette: &TextPalette,
        options: &RichTextOptions,
        expanded_blockquotes: &[usize],
    ) -> &LaidOutRichText {
        let epoch = fonts_epoch(ctx);
        let emoji_registered = custom_emoji_registered(&source.entities);
        let fits = |e: &EntityLayoutEntry| {
            e.is_for(
                epoch,
                &emoji_registered,
                style,
                palette,
                options,
                expanded_blockquotes,
            )
        };
        if !self.entry.as_ref().is_some_and(fits) {
            // The fade's revealed layout is the one wanted when the fade ends.
            self.entry = self.fade.take().filter(fits);
        }
        let entry = self.entry.get_or_insert_with(|| EntityLayoutEntry {
            style: *style,
            palette: *palette,
            options: options.clone(),
            expanded_blockquotes: expanded_blockquotes.to_vec(),
            emoji_registered,
            epoch,
            laid: layout_text_with_entities_expanded(
                ctx,
                source,
                style,
                palette,
                options,
                expanded_blockquotes,
            ),
        });
        &entry.laid
    }
}

/// A [`TextWithEntities`] with its prepared layout, for [`render_prepared_text_with_entities`].
pub struct PreparedEntityText {
    source: TextWithEntities,
    cache: EntityLayoutCache,
}

impl std::fmt::Debug for PreparedEntityText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedEntityText")
            .field("text_len", &self.source.text.len())
            .field("entities", &self.source.entities.len())
            .field("has_layout", &self.cache.entry.is_some())
            .finish()
    }
}

impl PreparedEntityText {
    /// Takes the text; nothing is laid out until it is painted.
    #[must_use]
    pub fn new(source: TextWithEntities) -> Self {
        Self {
            source,
            cache: EntityLayoutCache::default(),
        }
    }

    /// The text this was prepared from.
    #[must_use]
    pub const fn source(&self) -> &TextWithEntities {
        &self.source
    }
}

/// [`super::render_text_with_entities`] for a text prepared earlier: same widget, same pixels,
/// but the blocks are only laid out again when the style, colours, width, hover / spoiler /
/// expanded state, custom emoji textures or the font atlas change.
pub fn render_prepared_text_with_entities(
    ui: &mut Ui,
    id: Id,
    prepared: &mut PreparedEntityText,
    style: &TextStyle,
    palette: &TextPalette,
    options: &RichTextOptions,
    emoji_renderer: Option<&mut CustomEmojiRenderer>,
) -> EntityTextResponse {
    super::render_entity_text_cached(
        ui,
        id,
        &prepared.source,
        &mut prepared.cache,
        style,
        palette,
        options,
        emoji_renderer,
    )
}

#[cfg(test)]
mod tests {
    use egui::{
        CentralPanel, Context, Event, Frame, Modifiers, PointerButton, RawInput, pos2, vec2,
    };
    use tg_text::utf16_len;

    use super::*;
    use crate::effects::paint_text_spoiler;
    use crate::fonts::register_tdesktop_fonts;
    use crate::text::{build_layout_job, render_rich_text, render_text_with_entities};

    const WIDTH: f32 = 260.0;

    struct TestColors;

    impl crate::theme::Colors for TestColors {
        fn get(&self, _name: &str) -> Option<Color32> {
            Some(Color32::from_rgb(0x40, 0x80, 0xC0))
        }
    }

    fn style() -> TextStyle {
        TextStyle::default_style()
    }

    fn palette() -> TextPalette {
        TextPalette::default_palette(&TestColors)
    }

    fn new_context() -> Context {
        let ctx = Context::default();
        register_tdesktop_fonts(&ctx);
        let mut init = ctx.run_ui(RawInput::default(), |_| {});
        init.textures_delta.clear();
        ctx
    }

    fn sample_rich() -> RichText {
        RichText::plain("Plain start, ")
            .push_bold("bold words, ")
            .push_link("a link to follow", "https://example.org/a")
            .push_plain(" then ")
            .push_spoiler("a hidden spoiler")
            .push_plain(" and a tail long enough to wrap over several lines, with ")
            .push_link("a second link", "https://example.org/b")
            .push_plain(" and ")
            .push_code("code()")
            .push_plain(" at the end.")
    }

    fn sample_entities() -> TextWithEntities {
        let intro = "Intro with a link and a spoiler.\n";
        let code = "let answer = 42;\n";
        let quote = "A quoted line that is long enough to wrap inside the block.";
        let text = format!("{intro}{code}{quote}");
        let at = |part: &str| utf16_len(&text[..text.find(part).unwrap_or(0)]) as i32;
        let entities = vec![
            EntityInText::with_data(
                EntityType::CustomUrl,
                at("link"),
                4,
                "https://example.org/c",
            ),
            EntityInText::new(EntityType::Spoiler, at("spoiler"), 7),
            EntityInText::with_data(
                EntityType::Pre,
                at(code),
                utf16_len(code) as i32 - 1,
                "rust",
            ),
            EntityInText::new(EntityType::Blockquote, at(quote), utf16_len(quote) as i32),
        ];
        TextWithEntities::with_entities(text, entities)
    }

    /// One frame; returns what would be drawn, as tessellated meshes.
    fn frame(
        ctx: &Context,
        time: f64,
        top: f32,
        events: Vec<Event>,
        mut paint: impl FnMut(&mut Ui),
    ) -> Vec<(Rect, egui::epaint::Mesh)> {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 600.0))),
            time: Some(time),
            events,
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
                ui.add_space(top);
                paint(ui);
            });
        });
        out.textures_delta.clear();
        ctx.tessellate(out.shapes, out.pixels_per_point)
            .into_iter()
            .filter_map(|clipped| match clipped.primitive {
                egui::epaint::Primitive::Mesh(mesh) => Some((clipped.clip_rect, mesh)),
                egui::epaint::Primitive::Callback(_) => None,
            })
            .collect()
    }

    /// Pointer moves, clicks and scrolling over the text: every hover and spoiler state.
    fn script() -> Vec<(f32, Vec<Event>)> {
        let mut frames = vec![(10.0, Vec::new()), (10.0, Vec::new())];
        // Sweep the pointer over the text block, clicking at each point.
        for row in 0..7 {
            for col in 0..9 {
                let pos = pos2(6.0 + col as f32 * 30.0, 16.0 + row as f32 * 17.0);
                frames.push((10.0, vec![Event::PointerMoved(pos)]));
                frames.push((10.0, Vec::new()));
                let button = |pressed| Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                };
                frames.push((10.0, vec![button(true)]));
                frames.push((10.0, vec![button(false)]));
            }
        }
        // Then move the text (a scroll) while the pointer rests.
        for step in 0..12 {
            frames.push((10.0 + step as f32 * 7.0, Vec::new()));
        }
        frames
    }

    /// `render_rich_text` as it was before texts were prepared, with the spoiler cover and
    /// reveal fade of TGB-3009: what the prepared path has to paint.
    #[allow(clippy::too_many_arguments)]
    fn reference_render_rich_text(
        ui: &mut Ui,
        id: Id,
        rich: &RichText,
        style: &TextStyle,
        palette: &TextPalette,
        base_color: Color32,
        max_width: f32,
    ) -> RichTextResponse {
        let reveal_id = id.with("_tg_spoiler_revealed_at");
        let hover_link_id = id.with("_tg_hover_link");
        let now = ui.input(|i| i.time);
        let reveal = ui
            .ctx()
            .data_mut(|d| d.get_temp::<f64>(reveal_id))
            .map_or(0.0, |at| spoiler_reveal_progress(now - at));
        let revealed = reveal >= 1.0;
        let clickable = reveal <= 0.0;
        let prev_hovered = ui
            .ctx()
            .data_mut(|d| d.get_temp::<Option<String>>(hover_link_id))
            .unwrap_or(None);

        let wrap_w = max_width.max(1.0);
        let job = build_layout_job(
            rich,
            style,
            palette,
            base_color,
            wrap_w,
            prev_hovered.as_deref(),
            revealed,
        );
        let galley = ui.painter().layout_job(job);
        let desired = galley.size();
        let (rect, response) = ui.allocate_exact_size(desired, Sense::click());

        let segments = split_legacy_segments(rich);
        let mut link_hits: Vec<(Rect, String)> = Vec::new();
        let mut spoiler_rects: Vec<Rect> = Vec::new();

        for seg in &segments {
            if seg.link.is_none() && !seg.spoiler {
                continue;
            }
            let rects = byte_range_rects(&galley, rich.text.as_str(), seg.start, seg.end, rect.min);
            if let Some(url) = &seg.link {
                for r in &rects {
                    link_hits.push((*r, url.clone()));
                }
            }
            if seg.spoiler {
                spoiler_rects.extend(rects.iter().copied());
            }
        }
        let spoiler_rects = merge_spoiler_rects(&spoiler_rects);

        let pointer = ui.ctx().pointer_hover_pos();
        let mut new_hovered_link: Option<String> = None;
        let mut hovering_spoiler = false;
        if let Some(pos) = pointer
            && rect.contains(pos)
        {
            if clickable && spoiler_rects.iter().any(|r| r.contains(pos)) {
                hovering_spoiler = true;
            } else {
                for (r, url) in &link_hits {
                    if r.contains(pos) {
                        new_hovered_link = Some(url.clone());
                        break;
                    }
                }
            }
        }

        if hovering_spoiler || new_hovered_link.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        if new_hovered_link != prev_hovered {
            ui.ctx()
                .data_mut(|d| d.insert_temp(hover_link_id, new_hovered_link.clone()));
            ui.ctx().request_repaint();
        }

        let mut out = RichTextResponse {
            clicked_link: None,
            spoiler_toggled: false,
            rect,
        };

        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            if clickable && spoiler_rects.iter().any(|r| r.contains(pos)) {
                out.spoiler_toggled = true;
                ui.ctx().data_mut(|d| d.insert_temp(reveal_id, now));
                ui.ctx().request_repaint();
            } else {
                for (r, url) in &link_hits {
                    if r.contains(pos) {
                        out.clicked_link = Some(url.clone());
                        break;
                    }
                }
            }
        }

        if ui.is_rect_visible(rect) {
            ui.painter().galley(rect.min, galley, base_color);

            if !revealed && !spoiler_rects.is_empty() {
                if !clickable {
                    let job =
                        build_layout_job(rich, style, palette, base_color, wrap_w, None, true);
                    let revealed_galley = ui.painter().layout_job(job);
                    for srect in &spoiler_rects {
                        let mut fade = ui
                            .painter()
                            .with_clip_rect(ui.painter().clip_rect().intersect(*srect));
                        fade.multiply_opacity(reveal);
                        fade.galley(rect.min, Arc::clone(&revealed_galley), base_color);
                    }
                }
                paint_text_spoiler(
                    ui.ctx(),
                    ui.painter(),
                    &spoiler_rects,
                    rect.min,
                    palette.spoiler_fg,
                    1.0 - reveal,
                );
            }
        }

        out
    }

    #[test]
    fn prepared_rich_text_paints_what_the_widget_painted_before_the_change() {
        let (ctx_a, ctx_b) = (new_context(), new_context());
        let rich = sample_rich();
        let mut prepared = PreparedRichText::new(rich.clone());
        let (style, palette) = (style(), palette());
        let id = Id::unique("reference_vs_prepared");

        let mut clicked_a = Vec::new();
        let mut clicked_b = Vec::new();
        let mut toggled = (0usize, 0usize);
        for (index, (top, events)) in script().into_iter().enumerate() {
            let time = index as f64 / 60.0;
            let a = frame(&ctx_a, time, top, events.clone(), |ui| {
                // The prepared path lays out its one-glyph canary before the text; the same
                // here keeps the glyphs at the same places in the font atlas, so the meshes
                // can be compared whole (positions, colours and texture coordinates).
                let _ = fonts_epoch(ui.ctx());
                let r = reference_render_rich_text(
                    ui,
                    id,
                    &rich,
                    &style,
                    &palette,
                    Color32::BLACK,
                    WIDTH,
                );
                clicked_a.extend(r.clicked_link);
                toggled.0 += usize::from(r.spoiler_toggled);
            });
            let b = frame(&ctx_b, time, top, events, |ui| {
                let r = render_prepared_rich_text(
                    ui,
                    id,
                    &mut prepared,
                    &style,
                    &palette,
                    Color32::BLACK,
                    WIDTH,
                );
                clicked_b.extend(r.clicked_link);
                toggled.1 += usize::from(r.spoiler_toggled);
            });
            assert_eq!(a, b, "frame {index} differs from the reference");
        }
        assert_eq!(clicked_a, clicked_b);
        assert_eq!(toggled.0, toggled.1);
        assert_eq!(toggled.0, 1, "the sweep revealed the spoiler once");
    }

    #[test]
    fn prepared_rich_text_paints_the_same_meshes_as_the_per_frame_path() {
        let (ctx_a, ctx_b) = (new_context(), new_context());
        let rich = sample_rich();
        let mut prepared = PreparedRichText::new(rich.clone());
        let (style, palette) = (style(), palette());
        let id = Id::unique("prepared_vs_plain");

        let mut clicked_a = Vec::new();
        let mut clicked_b = Vec::new();
        let mut preparations = (0u64, 0u64);
        for (index, (top, events)) in script().into_iter().enumerate() {
            let time = index as f64 / 60.0;
            let before = text_preparation_count();
            let a = frame(&ctx_a, time, top, events.clone(), |ui| {
                let r = render_rich_text(ui, id, &rich, &style, &palette, Color32::BLACK, WIDTH);
                clicked_a.extend(r.clicked_link);
            });
            let mid = text_preparation_count();
            let b = frame(&ctx_b, time, top, events, |ui| {
                let r = render_prepared_rich_text(
                    ui,
                    id,
                    &mut prepared,
                    &style,
                    &palette,
                    Color32::BLACK,
                    WIDTH,
                );
                clicked_b.extend(r.clicked_link);
            });
            preparations.0 += mid - before;
            preparations.1 += text_preparation_count() - mid;
            assert_eq!(a, b, "frame {index} differs");
        }
        assert_eq!(clicked_a, clicked_b);
        assert!(
            clicked_a.iter().any(|url| url == "https://example.org/a"),
            "the sweep clicked a link: {clicked_a:?}"
        );
        // Hover on / off each link and the spoiler reveal are the only new layouts.
        assert!(
            preparations.1 >= 3 && preparations.1 * 10 < preparations.0,
            "per-frame path {} preparations, prepared path {}",
            preparations.0,
            preparations.1
        );
    }

    #[test]
    fn prepared_entity_text_paints_the_same_meshes_as_the_per_frame_path() {
        let (ctx_a, ctx_b) = (new_context(), new_context());
        let source = sample_entities();
        let mut prepared = PreparedEntityText::new(source.clone());
        let (style, palette) = (style(), palette());
        let options = RichTextOptions::new(Color32::BLACK, WIDTH).selectable(true);
        let id = Id::unique("prepared_vs_plain_entities");

        let mut preparations = (0u64, 0u64);
        let mut clicked = (Vec::new(), Vec::new());
        for (index, (top, events)) in script().into_iter().enumerate() {
            let time = index as f64 / 60.0;
            let before = text_preparation_count();
            let a = frame(&ctx_a, time, top, events.clone(), |ui| {
                let r = render_text_with_entities(ui, id, &source, &style, &palette, &options);
                clicked.0.extend(r.clicked_link.map(|l| l.target));
            });
            let mid = text_preparation_count();
            let b = frame(&ctx_b, time, top, events, |ui| {
                let r = render_prepared_text_with_entities(
                    ui,
                    id,
                    &mut prepared,
                    &style,
                    &palette,
                    &options,
                    None,
                );
                clicked.1.extend(r.clicked_link.map(|l| l.target));
            });
            preparations.0 += mid - before;
            preparations.1 += text_preparation_count() - mid;
            assert_eq!(a, b, "frame {index} differs");
        }
        assert_eq!(clicked.0, clicked.1);
        assert!(
            preparations.1 >= 2 && preparations.1 * 10 < preparations.0,
            "per-frame path {} layouts, prepared path {}",
            preparations.0,
            preparations.1
        );
    }

    #[test]
    fn plain_galley_is_the_galley_of_build_layout_job() {
        let ctx = new_context();
        let rich = sample_rich();
        let mut prepared = PreparedRichText::new(rich.clone());
        let (style, palette) = (style(), palette());
        let mut same = false;
        let mut out = ctx.run_ui(RawInput::default(), |ui| {
            let direct = ui.painter().layout_job(build_layout_job(
                &rich,
                &style,
                &palette,
                Color32::BLACK,
                WIDTH,
                None,
                true,
            ));
            let kept = prepared.plain_galley(ui, &style, &palette, Color32::BLACK, WIDTH);
            // egui hands out one galley per job, so the same job gives the same galley.
            same = Arc::ptr_eq(&direct, &kept);
            let again = prepared.plain_galley(ui, &style, &palette, Color32::BLACK, WIDTH);
            assert!(Arc::ptr_eq(&kept, &again));
        });
        out.textures_delta.clear();
        assert!(same, "the kept galley comes from the same layout job");
    }

    #[test]
    fn fonts_epoch_moves_when_the_scale_or_the_fonts_change() {
        let ctx = new_context();
        let run = |ctx: &Context| {
            let mut epoch = 0;
            let mut out = ctx.run_ui(RawInput::default(), |ui| {
                epoch = fonts_epoch(ui.ctx());
                assert_eq!(epoch, fonts_epoch(ui.ctx()), "stable within a pass");
            });
            out.textures_delta.clear();
            epoch
        };
        let first = run(&ctx);
        assert_eq!(first, run(&ctx), "unchanged fonts keep the epoch");

        ctx.set_pixels_per_point(2.0);
        let scaled = run(&ctx);
        assert_ne!(first, scaled, "a new scale lays text out again");
        assert_eq!(scaled, run(&ctx));

        ctx.set_fonts(egui::FontDefinitions::default());
        let replaced = run(&ctx);
        assert_ne!(scaled, replaced, "replaced fonts rebuild the atlas");
    }
}
