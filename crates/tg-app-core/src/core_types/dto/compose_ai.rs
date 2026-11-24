//! Pure AI compose/translate DTOs, backend trait, and mock store.

use tg_iv::rich_page::{RichBlock, RichPage, RichTextNode, TextWithEntities as IvText};
use tg_text::TextWithEntities;

use crate::stand_in::types::{MsgId, PeerId};

/// Maximum AI tone title length in Unicode scalar values.
pub const AI_TONE_TITLE_LIMIT: usize = 32;
/// Maximum AI tone prompt length in Unicode scalar values.
pub const AI_TONE_PROMPT_LIMIT: usize = 500;
/// Remaining-character threshold below which the prompt counter is shown (`kPromptRemainingThreshold = 100`).
pub const AI_TONE_PROMPT_WARN_THRESHOLD: usize = 100;

/// Built-in target languages for `TranslateBox` and `ComposeAiBox` Translate tab.
pub const TRANSLATE_TARGET_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("uk", "Ukrainian"),
    ("pl", "Polish"),
    ("nl", "Dutch"),
    ("tr", "Turkish"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("ar", "Arabic"),
];

/// Built-in tone hints accepted by `messages.translateText` (`tone` field).
pub const TRANSLATE_TONES: &[(&str, &str)] =
    &[("", "Neutral"), ("formal", "Formal"), ("casual", "Casual")];

/// Default emoji icon options for custom AI tones (`CreateAiToneBox`).
pub const AI_TONE_ICON_CHOICES: &[(i64, &str)] = &[
    (5368324170671202286, "✨"),
    (5368324170671202287, "💼"),
    (5368324170671202288, "😊"),
    (5368324170671202289, "📝"),
    (5368324170671202290, "🚀"),
    (5368324170671202291, "🎯"),
    (5368324170671202292, "💡"),
    (5368324170671202293, "🏴‍☠️"),
];

// ---------------------------------------------------------------------------
// Domain types (`api_compose_with_ai.h` & `data_ai_compose_tones.h`)
// ---------------------------------------------------------------------------

/// Before/after example pair for an AI compose tone (`AiComposeToneExample`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiComposeToneExample {
    /// Original sample text.
    pub from: TextWithEntities,
    /// Rewritten sample text demonstrating this tone.
    pub to: TextWithEntities,
}

/// Built-in or user-created AI compose tone (`Data::AiComposeTone`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiComposeTone {
    /// Custom tone ID (`0` for built-in default tones).
    pub id: i64,
    /// Custom tone access hash (`0` for built-in default tones).
    pub access_hash: i64,
    /// Shareable slug (`tg://addstyle?slug=<slug>`).
    pub slug: String,
    /// Display title.
    pub title: String,
    /// Telegram custom emoji document ID (`0` if none).
    pub emoji_id: i64,
    /// Fallback Unicode emoji badge rendered alongside the title.
    pub emoji_label: String,
    /// Custom prompt instructions (present for custom tones).
    pub prompt: String,
    /// Number of users who installed this custom tone.
    pub installs_count: i32,
    /// Optional author user ID (`0` if hidden or default).
    pub author_id: i64,
    /// `true` if the signed-in account created this tone.
    pub creator: bool,
    /// `true` if this is a server-provided default tone (`aiComposeToneDefault`).
    pub is_default: bool,
    /// Canonical default tone key (e.g. `"formal"`, `"friendly"`, `"concise"`).
    pub default_type: String,
    /// Optional English before/after preview example.
    pub first_example: Option<AiComposeToneExample>,
}

impl AiComposeTone {
    /// Converts this tone into a [`AiComposeToneRef`] for RPC calls.
    #[must_use]
    pub fn to_ref(&self) -> AiComposeToneRef {
        if self.is_default {
            AiComposeToneRef::DefaultTone(self.default_type.clone())
        } else if self.id != 0 {
            AiComposeToneRef::Custom {
                id: self.id,
                access_hash: self.access_hash,
            }
        } else {
            AiComposeToneRef::Slug(self.slug.clone())
        }
    }

    /// Returns a stable identifier string matching `ResolveStyleDescriptor` in `compose_ai_box.cpp`.
    #[must_use]
    pub fn stable_id(&self) -> String {
        if self.is_default {
            self.default_type.clone()
        } else {
            self.id.to_string()
        }
    }
}

/// Tone reference passed to `messages.composeMessageWithAI` (`InputAiComposeTone`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiComposeToneRef {
    /// Server-provided default tone (`inputAiComposeToneDefault`).
    DefaultTone(String),
    /// Saved/custom tone by ID + access hash (`inputAiComposeToneID`).
    Custom {
        /// Tone ID.
        id: i64,
        /// Tone access hash.
        access_hash: i64,
    },
    /// Custom tone by shareable slug (`inputAiComposeToneSlug`).
    Slug(String),
    /// One-off free-form prompt (`inputAiComposeToneSingleUse`).
    SingleUsePrompt(String),
}

/// Classification of one diff span inside `messages.composedMessageWithAI.diff_text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeAiDiffKind {
    /// Newly inserted text (`messageEntityDiffInsert`).
    Insert,
    /// Replaced text (`messageEntityDiffReplace`) carrying the removed `old_text`.
    Replace {
        /// Previous text before replacement.
        old_text: String,
    },
    /// Deleted text (`messageEntityDiffDelete`).
    Delete,
}

/// One diff entity returned by `Api::ComposeWithAi::ParseDiff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeAiDiffEntity {
    /// Diff operation kind.
    pub kind: ComposeAiDiffKind,
    /// UTF-16 offset in `diff_text`.
    pub offset: i32,
    /// UTF-16 length in `diff_text`.
    pub length: i32,
}

/// Parsed diff result returned by `messages.composeMessageWithAI`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComposeAiDiff {
    /// Base diff text with entities.
    pub text: TextWithEntities,
    /// Ordered diff spans.
    pub entities: Vec<ComposeAiDiffEntity>,
}

/// Flattened display segment emitted by [`build_diff_display`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffDisplaySegment {
    /// Unchanged text.
    Plain(String),
    /// Added or replacement text (highlighted/underlined).
    Inserted(String),
    /// Removed text (strikethrough).
    Deleted(String),
}

/// Converts a [`ComposeAiDiff`] into display segments matching `BuildDiffDisplay`
/// in `Telegram/SourceFiles/boxes/compose_ai_box.cpp:123-164`.
#[must_use]
pub fn build_diff_display(diff: &ComposeAiDiff) -> Vec<DiffDisplaySegment> {
    let mut entities = diff.entities.clone();
    entities.sort_by_key(|e| e.offset);

    let utf16_units: Vec<u16> = diff.text.text.encode_utf16().collect();
    let total_len = utf16_units.len() as i32;
    let slice_u16 = |from: i32, len: i32| -> String {
        let start = from.clamp(0, total_len) as usize;
        let end = (from.saturating_add(len)).clamp(0, total_len) as usize;
        if end <= start {
            String::new()
        } else {
            String::from_utf16_lossy(&utf16_units[start..end])
        }
    };

    let mut out = Vec::new();
    let mut taken = 0i32;
    for ent in &entities {
        let offset = ent.offset.clamp(0, total_len);
        let length = ent.length.clamp(0, total_len - offset);
        if offset > taken {
            let chunk = slice_u16(taken, offset - taken);
            if !chunk.is_empty() {
                out.push(DiffDisplaySegment::Plain(chunk));
            }
        }
        let part = slice_u16(offset, length);
        match &ent.kind {
            ComposeAiDiffKind::Insert => {
                if !part.is_empty() {
                    out.push(DiffDisplaySegment::Inserted(part));
                }
            }
            ComposeAiDiffKind::Replace { old_text } => {
                if !old_text.is_empty() {
                    out.push(DiffDisplaySegment::Deleted(old_text.clone()));
                }
                if !part.is_empty() {
                    out.push(DiffDisplaySegment::Inserted(part));
                }
            }
            ComposeAiDiffKind::Delete => {
                if !part.is_empty() {
                    out.push(DiffDisplaySegment::Deleted(part));
                }
            }
        }
        taken = taken.max(offset + length);
    }
    if taken < total_len {
        let tail = slice_u16(taken, total_len - taken);
        if !tail.is_empty() {
            out.push(DiffDisplaySegment::Plain(tail));
        }
    }
    out
}

/// Request payload for `messages.composeMessageWithAI` (`Api::ComposeWithAi::Request`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComposeAiRequest {
    /// Input text with entities.
    pub text: TextWithEntities,
    /// Proofread / grammar-fix mode.
    pub proofread: bool,
    /// Add expressive emojis to the output (`emojify`).
    pub emojify: bool,
    /// Optional ISO-639-1 target language for translation.
    pub translate_to_lang: Option<String>,
    /// Optional style/tone reference.
    pub tone: Option<AiComposeToneRef>,
}

/// Result payload from `messages.composeMessageWithAI` (`Api::ComposeWithAi::Result`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComposeAiResult {
    /// Resulting composed text with entities.
    pub result_text: TextWithEntities,
    /// Optional structured diff (returned in proofread mode).
    pub diff: Option<ComposeAiDiff>,
}

/// Request payload for Layer-233 `messages.composeRichMessageWithAI`.
///
/// Mirrors `Api::ComposeWithAi::Request` (`richPage` branch in
/// `Telegram/SourceFiles/api/api_compose_with_ai.cpp:177-225` and
/// `Telegram/SourceFiles/boxes/create_ai_box.cpp:620-645`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComposeRichAiRequest {
    /// Optional source rich message page (`flags.4`: omitted for prompt-only generation).
    pub rich_page: Option<RichPage>,
    /// Proofread / grammar-fix mode (`flags.0`).
    pub proofread: bool,
    /// Add expressive emojis to the output (`flags.3`).
    pub emojify: bool,
    /// Optional ISO-639-1 target language for translation (`flags.1`).
    pub translate_to_lang: Option<String>,
    /// Optional style/tone reference (`flags.2`).
    pub tone: Option<AiComposeToneRef>,
}

/// Result payload from Layer-233 `messages.composeRichMessageWithAI`
/// (`messages.ComposedRichMessageWithAI`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComposeRichAiResult {
    /// Clean resulting rich page (`RichParseMode::Normal`).
    pub page: RichPage,
    /// Display rich page with inline proofread diff markers (`RichParseMode::DisplayTextDiff`
    /// when `proofread == true`, otherwise identical to `page`).
    pub display_page: RichPage,
}

/// Input target for Layer-233 `messages.translateRichMessage`.
#[derive(Debug, Clone, PartialEq)]
pub enum TranslateRichMessageTarget {
    /// Translate existing chat rich messages by `(peer_id, msg_ids)` (`flags.0`, chunked by 20).
    Messages {
        /// Chat peer owning the messages.
        peer_id: PeerId,
        /// Server message IDs to translate.
        msg_ids: Vec<MsgId>,
    },
    /// Translate inline rich pages without a peer/message reference (`flags.1`).
    RichPages(Vec<RichPage>),
}

/// Request payload for Layer-233 `messages.translateRichMessage`
/// (`Telegram/SourceFiles/history/view/history_view_translate_tracker.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct TranslateRichMessageRequest {
    /// Target messages or inline rich pages.
    pub target: TranslateRichMessageTarget,
    /// Target ISO-639-1 language code (e.g. `"en"`, `"fr"`).
    pub to_lang: String,
    /// Optional translation tone hint (`flags.2`, e.g. `"formal"`, `"casual"`).
    pub tone: Option<String>,
}

/// Result payload from Layer-233 `messages.translateRichMessage`
/// (`messages.TranslatedRichMessage`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TranslateRichMessageResult {
    /// Translated rich pages in the same order as the requested message IDs or inline pages.
    pub pages: Vec<RichPage>,
}

/// Backend contract decoupling `TranslateBox`, `ComposeAiBox`, `CreateAiToneBox`, and
/// `PreviewAiToneBox` from live MTProto RPCs.
pub trait ComposeAiBackend {
    /// Returns cached or fetched default + user custom AI compose tones (`aicompose.getTones`).
    fn list_ai_tones(&mut self) -> Result<Vec<AiComposeTone>, String>;

    /// Executes Telegram's server-side `messages.composeMessageWithAI`.
    fn compose_with_ai(&mut self, request: ComposeAiRequest) -> Result<ComposeAiResult, String>;

    /// Executes Telegram's Layer-233 `messages.composeRichMessageWithAI`.
    fn compose_rich_message_with_ai(
        &mut self,
        _request: ComposeRichAiRequest,
    ) -> Result<ComposeRichAiResult, String> {
        Err("Rich message AI compose is not supported by this backend.".to_owned())
    }

    /// Executes Telegram's Layer-233 `messages.translateRichMessage`.
    fn translate_rich_message(
        &mut self,
        _request: TranslateRichMessageRequest,
    ) -> Result<TranslateRichMessageResult, String> {
        Err("Rich message translation is not supported by this backend.".to_owned())
    }

    /// Creates a new custom AI tone (`aicompose.createTone`).
    fn create_ai_tone(
        &mut self,
        title: &str,
        prompt: &str,
        emoji_id: i64,
        display_author: bool,
    ) -> Result<AiComposeTone, String>;

    /// Updates an existing custom AI tone (`aicompose.updateTone`).
    fn update_ai_tone(
        &mut self,
        tone: &AiComposeTone,
        title: Option<&str>,
        prompt: Option<&str>,
        emoji_id: Option<i64>,
        display_author: Option<bool>,
    ) -> Result<AiComposeTone, String>;

    /// Saves (`unsave = false`) or removes (`unsave = true`) a shared AI tone (`aicompose.saveTone`).
    fn save_ai_tone(&mut self, tone: &AiComposeTone, unsave: bool) -> Result<(), String>;

    /// Deletes a user-created custom AI tone (`aicompose.deleteTone`).
    fn delete_ai_tone(&mut self, tone: &AiComposeTone) -> Result<(), String>;

    /// Resolves a shared AI tone by slug (`aicompose.getTone` for `tg://addstyle?slug=...`).
    fn resolve_ai_tone(&mut self, slug: &str) -> Result<AiComposeTone, String>;

    /// Returns `true` if `tone` is already saved in the local cached tones list without making
    /// an extra RPC.
    fn is_ai_tone_installed(&self, _tone: &AiComposeTone) -> bool {
        false
    }

    /// Loads example `#num` for `tone` (`aicompose.getToneExample`).
    fn get_ai_tone_example(
        &mut self,
        tone: &AiComposeTone,
        num: i32,
    ) -> Result<AiComposeToneExample, String>;

    /// Translates either a chat message (`peer_id` + `msg_id`) or arbitrary text via
    /// `messages.translateText` with optional `tone`.
    fn translate_text_modal(
        &mut self,
        peer_id: Option<PeerId>,
        msg_id: Option<MsgId>,
        text: &TextWithEntities,
        to_lang: &str,
        tone: Option<&str>,
    ) -> Result<TextWithEntities, String>;
}

// ---------------------------------------------------------------------------
// Deterministic offline mock store (`MockSession` demo data)
// ---------------------------------------------------------------------------

/// Offline demo store for `ComposeAiBackend`, embedded in `MockSession` and standalone tests.
#[derive(Debug, Clone)]
pub struct MockComposeAiStore {
    tones: Vec<AiComposeTone>,
    next_id: i64,
}

impl Default for MockComposeAiStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MockComposeAiStore {
    /// Creates a demo store populated with default tones and a fictional custom engineering tone.
    #[must_use]
    pub fn new() -> Self {
        let tones = vec![
            AiComposeTone {
                id: 0,
                access_hash: 0,
                slug: String::new(),
                title: "Formal".to_owned(),
                emoji_id: 5368324170671202287,
                emoji_label: "💼".to_owned(),
                prompt: String::new(),
                installs_count: 0,
                author_id: 0,
                creator: false,
                is_default: true,
                default_type: "formal".to_owned(),
                first_example: None,
            },
            AiComposeTone {
                id: 0,
                access_hash: 0,
                slug: String::new(),
                title: "Friendly".to_owned(),
                emoji_id: 5368324170671202288,
                emoji_label: "😊".to_owned(),
                prompt: String::new(),
                installs_count: 0,
                author_id: 0,
                creator: false,
                is_default: true,
                default_type: "friendly".to_owned(),
                first_example: None,
            },
            AiComposeTone {
                id: 0,
                access_hash: 0,
                slug: String::new(),
                title: "Concise".to_owned(),
                emoji_id: 5368324170671202291,
                emoji_label: "🎯".to_owned(),
                prompt: String::new(),
                installs_count: 0,
                author_id: 0,
                creator: false,
                is_default: true,
                default_type: "concise".to_owned(),
                first_example: None,
            },
            AiComposeTone {
                id: 0,
                access_hash: 0,
                slug: String::new(),
                title: "Enthusiastic".to_owned(),
                emoji_id: 5368324170671202290,
                emoji_label: "🚀".to_owned(),
                prompt: String::new(),
                installs_count: 0,
                author_id: 0,
                creator: false,
                is_default: true,
                default_type: "enthusiastic".to_owned(),
                first_example: None,
            },
            AiComposeTone {
                id: 901_001,
                access_hash: 77_001_001,
                slug: "clear_eng_note".to_owned(),
                title: "Clear Engineering Note".to_owned(),
                emoji_id: 5368324170671202289,
                emoji_label: "📝".to_owned(),
                prompt: "Rewrite as a concise, crisp status update with action items.".to_owned(),
                installs_count: 142,
                author_id: 1000,
                creator: true,
                is_default: false,
                default_type: String::new(),
                first_example: Some(AiComposeToneExample {
                    from: TextWithEntities::simple(
                        "hey we fixed the build thing and tests pass now",
                    ),
                    to: TextWithEntities::simple(
                        "Status update: Resolved the build failure; full test suite now passes.",
                    ),
                }),
            },
        ];
        Self {
            tones,
            next_id: 901_100,
        }
    }

    /// Returns a read-only view of all tones in the demo store.
    #[must_use]
    pub fn tones(&self) -> &[AiComposeTone] {
        &self.tones
    }
}

impl ComposeAiBackend for MockComposeAiStore {
    fn list_ai_tones(&mut self) -> Result<Vec<AiComposeTone>, String> {
        Ok(self.tones.clone())
    }

    fn compose_with_ai(&mut self, request: ComposeAiRequest) -> Result<ComposeAiResult, String> {
        let src = request.text.text.trim();
        if src.is_empty() {
            return Err("Enter some text to rewrite or proofread.".to_owned());
        }

        if request.proofread {
            let fixed = proofread_demo_text(src);
            let mut diff_entities = Vec::new();
            if fixed != src {
                diff_entities.push(ComposeAiDiffEntity {
                    kind: ComposeAiDiffKind::Replace {
                        old_text: src.to_owned(),
                    },
                    offset: 0,
                    length: fixed.encode_utf16().count() as i32,
                });
            }
            let out_text = if request.emojify {
                format!("{fixed} ✨")
            } else {
                fixed.clone()
            };
            return Ok(ComposeAiResult {
                result_text: TextWithEntities::simple(out_text),
                diff: Some(ComposeAiDiff {
                    text: TextWithEntities::simple(fixed),
                    entities: diff_entities,
                }),
            });
        }

        let mut transformed = src.to_owned();
        if let Some(ref lang) = request.translate_to_lang {
            let lang_label = language_display_name(lang);
            transformed = format!("[{lang_label}] {transformed}");
        }
        if let Some(ref tone) = request.tone {
            transformed = match tone {
                AiComposeToneRef::DefaultTone(t) => match t.as_str() {
                    "formal" => format!("Please note: {transformed}."),
                    "friendly" => format!("Hey! {transformed}"),
                    "concise" => transformed
                        .split_whitespace()
                        .take(18)
                        .collect::<Vec<_>>()
                        .join(" "),
                    "enthusiastic" => format!("Awesome news — {transformed}!"),
                    other => format!("[{other}] {transformed}"),
                },
                AiComposeToneRef::Custom { id, .. } => {
                    let title = self
                        .tones
                        .iter()
                        .find(|t| t.id == *id)
                        .map_or("Custom Style", |t| t.title.as_str());
                    format!("[{title}] {transformed}")
                }
                AiComposeToneRef::Slug(slug) => format!("[{slug}] {transformed}"),
                AiComposeToneRef::SingleUsePrompt(prompt) => {
                    let short: String = prompt.trim().chars().take(24).collect();
                    format!("[Prompt: {short}] {transformed}")
                }
            };
        }
        if request.emojify {
            transformed.push_str(" ✨");
        }
        Ok(ComposeAiResult {
            result_text: TextWithEntities::simple(transformed),
            diff: None,
        })
    }

    fn compose_rich_message_with_ai(
        &mut self,
        request: ComposeRichAiRequest,
    ) -> Result<ComposeRichAiResult, String> {
        let mut page = match request.rich_page {
            Some(p) if !p.blocks.is_empty() => p,
            None => {
                let prompt = match request.tone {
                    Some(AiComposeToneRef::SingleUsePrompt(ref p)) if !p.trim().is_empty() => {
                        p.trim().to_owned()
                    }
                    _ => return Err("Provide rich message content or a custom prompt.".to_owned()),
                };
                RichPage {
                    blocks: vec![RichBlock::paragraph(format!("[Generated] {prompt}"))],
                    ..RichPage::default()
                }
            }
            Some(_) => return Err("Rich message page cannot be empty.".to_owned()),
        };

        for block in &mut page.blocks {
            if !block.text.text.text.trim().is_empty() {
                let mut text = if request.proofread {
                    proofread_demo_text(&block.text.text.text)
                } else {
                    block.text.text.text.clone()
                };
                if let Some(ref lang) = request.translate_to_lang
                    && !lang.trim().is_empty()
                {
                    text = format!("[{}] {text}", language_display_name(lang));
                }
                if request.emojify {
                    text.push_str(" ✨");
                }
                block.text = RichTextNode::from_text(IvText::simple(text));
            }
        }
        Ok(ComposeRichAiResult {
            display_page: page.clone(),
            page,
        })
    }

    fn translate_rich_message(
        &mut self,
        request: TranslateRichMessageRequest,
    ) -> Result<TranslateRichMessageResult, String> {
        let lang_name = language_display_name(&request.to_lang);
        let tone_tag = request
            .tone
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| format!(", {t}"))
            .unwrap_or_default();
        let pages = match request.target {
            TranslateRichMessageTarget::Messages { msg_ids, .. } => msg_ids
                .into_iter()
                .map(|mid| RichPage {
                    blocks: vec![RichBlock::paragraph(format!(
                        "[{lang_name}{tone_tag}] Translated rich message #{}",
                        mid.0
                    ))],
                    ..RichPage::default()
                })
                .collect(),
            TranslateRichMessageTarget::RichPages(pages) => pages
                .into_iter()
                .map(|mut p| {
                    for block in &mut p.blocks {
                        if !block.text.text.text.trim().is_empty() {
                            block.text = RichTextNode::from_text(IvText::simple(format!(
                                "[{lang_name}{tone_tag}] {}",
                                block.text.text.text
                            )));
                        }
                    }
                    p
                })
                .collect(),
        };
        Ok(TranslateRichMessageResult { pages })
    }

    fn create_ai_tone(
        &mut self,
        title: &str,
        prompt: &str,
        emoji_id: i64,
        display_author: bool,
    ) -> Result<AiComposeTone, String> {
        validate_ai_tone_fields(title, prompt)?;
        self.next_id += 1;
        let id = self.next_id;
        let slug = title
            .trim()
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        let tone = AiComposeTone {
            id,
            access_hash: id.wrapping_mul(31),
            slug,
            title: title.trim().to_owned(),
            emoji_id,
            emoji_label: emoji_label_for_id(emoji_id).to_owned(),
            prompt: prompt.trim().to_owned(),
            installs_count: 1,
            author_id: if display_author { 1000 } else { 0 },
            creator: true,
            is_default: false,
            default_type: String::new(),
            first_example: Some(AiComposeToneExample {
                from: TextWithEntities::simple("Quick check-in on tomorrow's release."),
                to: TextWithEntities::simple(format!(
                    "[{}] Quick check-in on tomorrow's release.",
                    title.trim()
                )),
            }),
        };
        self.tones.insert(0, tone.clone());
        Ok(tone)
    }

    fn update_ai_tone(
        &mut self,
        tone: &AiComposeTone,
        title: Option<&str>,
        prompt: Option<&str>,
        emoji_id: Option<i64>,
        display_author: Option<bool>,
    ) -> Result<AiComposeTone, String> {
        let Some(existing) = self
            .tones
            .iter_mut()
            .find(|t| t.id == tone.id && !t.is_default)
        else {
            return Err("AI tone not found.".to_owned());
        };
        let next_title = title.unwrap_or(&existing.title);
        let next_prompt = prompt.unwrap_or(&existing.prompt);
        validate_ai_tone_fields(next_title, next_prompt)?;
        existing.title = next_title.trim().to_owned();
        existing.prompt = next_prompt.trim().to_owned();
        if let Some(eid) = emoji_id {
            existing.emoji_id = eid;
            existing.emoji_label = emoji_label_for_id(eid).to_owned();
        }
        if let Some(show_author) = display_author {
            existing.author_id = if show_author { 1000 } else { 0 };
        }
        Ok(existing.clone())
    }

    fn save_ai_tone(&mut self, tone: &AiComposeTone, unsave: bool) -> Result<(), String> {
        if unsave {
            self.tones.retain(|t| t.id != tone.id || t.is_default);
        } else if !self.tones.iter().any(|t| t.id == tone.id && !t.is_default) {
            self.tones.insert(0, tone.clone());
        }
        Ok(())
    }

    fn delete_ai_tone(&mut self, tone: &AiComposeTone) -> Result<(), String> {
        if tone.is_default {
            return Err("Default tones cannot be deleted.".to_owned());
        }
        self.tones.retain(|t| t.id != tone.id);
        Ok(())
    }

    fn resolve_ai_tone(&mut self, slug: &str) -> Result<AiComposeTone, String> {
        let cleaned = slug.trim();
        if let Some(found) = self
            .tones
            .iter()
            .find(|t| !t.is_default && t.slug.eq_ignore_ascii_case(cleaned))
        {
            return Ok(found.clone());
        }
        if cleaned.is_empty() {
            return Err("TONE_NOT_FOUND".to_owned());
        }
        Ok(AiComposeTone {
            id: 909_001,
            access_hash: 909_001_777,
            slug: cleaned.to_owned(),
            title: format!("Style: {cleaned}"),
            emoji_id: AI_TONE_ICON_CHOICES[0].0,
            emoji_label: "✨".to_owned(),
            prompt: format!("Rewrite messages using the '{cleaned}' style."),
            installs_count: 27,
            author_id: 1001,
            creator: false,
            is_default: false,
            default_type: String::new(),
            first_example: Some(AiComposeToneExample {
                from: TextWithEntities::simple("See you at 15:00 for the design review."),
                to: TextWithEntities::simple(format!(
                    "[{cleaned}] Looking forward to our 15:00 design review!"
                )),
            }),
        })
    }

    fn is_ai_tone_installed(&self, tone: &AiComposeTone) -> bool {
        self.tones.iter().any(|t| {
            !t.is_default
                && ((tone.id != 0 && t.id == tone.id)
                    || (!tone.slug.is_empty() && t.slug.eq_ignore_ascii_case(&tone.slug)))
        })
    }

    fn get_ai_tone_example(
        &mut self,
        tone: &AiComposeTone,
        num: i32,
    ) -> Result<AiComposeToneExample, String> {
        if num == 0
            && let Some(ref ex) = tone.first_example
        {
            return Ok(ex.clone());
        }
        Ok(AiComposeToneExample {
            from: TextWithEntities::simple(format!(
                "Example #{}: Let's ship the release notes before Friday.",
                num + 1
            )),
            to: TextWithEntities::simple(format!(
                "[{}] Let's finalize and publish the release notes ahead of Friday.",
                tone.title
            )),
        })
    }

    fn translate_text_modal(
        &mut self,
        _peer_id: Option<PeerId>,
        _msg_id: Option<MsgId>,
        text: &TextWithEntities,
        to_lang: &str,
        tone: Option<&str>,
    ) -> Result<TextWithEntities, String> {
        let src = text.text.trim();
        if src.is_empty() {
            return Err("Nothing to translate.".to_owned());
        }
        let lang_name = language_display_name(to_lang);
        let tone_tag = tone
            .filter(|t| !t.trim().is_empty())
            .map(|t| format!(", {t}"))
            .unwrap_or_default();
        Ok(TextWithEntities::simple(format!(
            "[{lang_name}{tone_tag}] {src}"
        )))
    }
}

/// Validates title and prompt for `CreateAiToneBox`.
pub fn validate_ai_tone_fields(title: &str, prompt: &str) -> Result<(), String> {
    let t = title.trim();
    let p = prompt.trim();
    if t.is_empty() {
        return Err("Please enter a style name.".to_owned());
    }
    if t.chars().count() > AI_TONE_TITLE_LIMIT {
        return Err(format!(
            "Style name must be at most {AI_TONE_TITLE_LIMIT} characters."
        ));
    }
    if p.is_empty() {
        return Err("Please enter style instructions.".to_owned());
    }
    if p.chars().count() > AI_TONE_PROMPT_LIMIT {
        return Err(format!(
            "Instructions must be at most {AI_TONE_PROMPT_LIMIT} characters."
        ));
    }
    Ok(())
}

/// Maps an emoji document ID to a readable Unicode fallback glyph.
#[must_use]
pub fn emoji_label_for_id(emoji_id: i64) -> &'static str {
    AI_TONE_ICON_CHOICES
        .iter()
        .find(|&&(id, _)| id == emoji_id)
        .map_or("✨", |&(_, glyph)| glyph)
}

/// Maps an ISO-639-1 language code to a human-readable language name.
#[must_use]
pub fn language_display_name(code: &str) -> &str {
    let lower = code.trim();
    TRANSLATE_TARGET_LANGUAGES
        .iter()
        .find(|&&(c, _)| c.eq_ignore_ascii_case(lower))
        .map_or(lower, |&(_, name)| name)
}

fn proofread_demo_text(src: &str) -> String {
    let mut s = src
        .replace("teh ", "the ")
        .replace("recieve", "receive")
        .replace("dont ", "don't ")
        .replace("cant ", "can't ")
        .replace("  ", " ");
    if let Some(first) = s.chars().next()
        && first.is_ascii_lowercase()
    {
        let mut chars = s.chars();
        if let Some(ch) = chars.next() {
            s = format!("{}{}", ch.to_ascii_uppercase(), chars.as_str());
        }
    }
    if !s.ends_with(['.', '!', '?']) {
        s.push('.');
    }
    s
}

// ---------------------------------------------------------------------------
// 1. TranslateBox Modal (`boxes/translate_box.cpp`)
// ---------------------------------------------------------------------------
