//! Live WP-28 glue for [`LiveSession`]:
//! - `ComposeAiBackend` (`messages.composeMessageWithAI`, `messages.translateText` with tone,
//!   `aicompose.{getTones, createTone, updateTone, saveTone, deleteTone, getTone, getToneExample}`)
//! - `StickerCreatorBackend` (`stickers.{suggestShortName, checkShortName, createStickerSet,
//!   addStickerToSet, deleteStickerSet}` + `upload.saveFilePart` + `messages.uploadMedia`)
//! - `SelfDestructBackend` (`account.{getAccountTTL, setAccountTTL, setAuthorizationTTL}`,
//!   `messages.{getDefaultHistoryTTL, setDefaultHistoryTTL, readMessageContents}`)
//! - Layer-233 Poll extensions (`messages.sendMedia(inputMediaPoll)` with option media/links and
//!   `open_answers`/`shuffle_answers`/`revoting_disabled`/`hide_results_until_close`,
//!   `messages.addPollAnswer`, `messages.deletePollAnswer`)
//! - Headless-safe real webcam round-video capture through `tg-voip::video_mf` and `tg-video`.
//!
//! Port origin:
//! - `Telegram/SourceFiles/api/api_compose_with_ai.cpp`
//! - `Telegram/SourceFiles/data/data_ai_compose_tones.cpp`
//! - `Telegram/SourceFiles/api/api_stickers_creator.cpp`
//! - `Telegram/SourceFiles/api/api_self_destruct.cpp`
//! - `Telegram/SourceFiles/api/api_polls.cpp`
//! - `Telegram/SourceFiles/ui/controls/round_video_recorder.cpp`

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, entities_from_tl, input_peer_for};
use tg_data::Session;
use tg_text::TextWithEntities;
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns, types as tl_types};

use crate::bridge::LiveBridgeState;
use crate::convert::{
    from_data_msg, now_unix_i32, peer_display_title, to_data_peer, ui_text_to_tl_entities,
};
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::core_types::dto::compose_ai::{
    AiComposeTone, AiComposeToneExample, AiComposeToneRef, ComposeAiBackend, ComposeAiDiff,
    ComposeAiDiffEntity, ComposeAiDiffKind, ComposeAiRequest, ComposeAiResult,
    ComposeRichAiRequest, ComposeRichAiResult, TranslateRichMessageRequest,
    TranslateRichMessageResult, TranslateRichMessageTarget, emoji_label_for_id,
    validate_ai_tone_fields,
};
use tg_app_core::core_types::dto::self_destruct::{SelfDestructBackend, SelfDestructSnapshot};
use tg_app_core::core_types::dto::sticker_creator::{
    AddStickerToSetRequest, CreateStickerSetRequest, OwnedStickerSetSummary,
    STICKER_CREATOR_SOFTWARE, StickerCreatorBackend, StickerSetKind, normalize_sticker_emoji,
    validate_sticker_set_meta,
};
use tg_app_core::history::backend::{HistoryMessage, MessageExtra, PollAttachment, PollOption};
use tg_app_core::history::sections_model::{
    AddPollOptionRequest, CreatePollRequest, POLL_MAX_OPTIONS_EXTENDED, POLL_OPTION_LIMIT,
    PollMessageExtras, PollOptionAttachment, ThreadKey, validate_create_poll,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_iv::rich_page::{
    InMemoryMediaResolver, RichParseMode, SerializeRichMessageMode, SerializeRichMessageStatus,
    rich_page_from_tl_rich_message, serialize_input_rich_message,
};

/// Default timeout for synchronous WP-28 RPC calls.
const WP28_RPC_TIMEOUT: Duration = Duration::from_secs(12);
/// File upload part chunk size for sticker / poll option uploads (`512 KiB`, divisible by 1024).
const UPLOAD_PART_BYTES: usize = 512 * 1024;

/// Per-session mutable cache for WP-28 subsystems.
#[derive(Default)]
struct Wp28LiveState {
    tones_hash: i64,
    tones: Vec<AiComposeTone>,
    recent_custom_tone_ids: Vec<i64>,
    owned_sticker_sets: Vec<OwnedStickerSetSummary>,
    self_destruct: SelfDestructSnapshot,
    viewed_ttl_messages: Vec<(PeerId, MsgId)>,
    poll_extras_cache: HashMap<(PeerId, MsgId), PollMessageExtras>,
    random_seq: u64,
}

impl Wp28LiveState {
    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.random_seq = self.random_seq.wrapping_add(1);
        ((now << 24) ^ self.random_seq ^ 0x5750_2800_9911) as i64
    }

    fn promote_custom_tone(&mut self, tone: AiComposeTone) {
        if tone.is_default {
            return;
        }
        self.tones.retain(|t| t.id != tone.id || t.is_default);
        self.recent_custom_tone_ids.retain(|&id| id != tone.id);
        self.recent_custom_tone_ids.insert(0, tone.id);
        self.tones.insert(0, tone);
    }

    fn remove_custom_tone(&mut self, id: i64) {
        self.tones.retain(|t| t.id != id || t.is_default);
        self.recent_custom_tone_ids.retain(|&tid| tid != id);
    }

    fn reapply_recent_custom_order(&mut self) {
        let mut reordered = Vec::with_capacity(self.tones.len());
        let mut kept_recent = Vec::with_capacity(self.recent_custom_tone_ids.len());
        for &id in &self.recent_custom_tone_ids {
            if let Some(tone) = self.tones.iter().find(|t| !t.is_default && t.id == id) {
                reordered.push(tone.clone());
                kept_recent.push(id);
            }
        }
        for tone in &self.tones {
            let promoted = !tone.is_default && kept_recent.contains(&tone.id);
            if !promoted {
                reordered.push(tone.clone());
            }
        }
        self.recent_custom_tone_ids = kept_recent;
        self.tones = reordered;
    }
}

struct Wp28StateEntry {
    owner: Weak<LiveBridgeState>,
    state: Wp28LiveState,
}

fn wp28_registry() -> &'static Mutex<HashMap<usize, Wp28StateEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Wp28StateEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_wp28_state<R>(live: &LiveSession, f: impl FnOnce(&mut Wp28LiveState) -> R) -> R
where
{
    let key = Arc::as_ptr(live.bridge_state()) as usize;
    let mut map = wp28_registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| Wp28StateEntry {
        owner: Arc::downgrade(live.bridge_state()),
        state: Wp28LiveState::default(),
    });
    f(&mut entry.state)
}

fn invoke_wp28<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(WP28_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
    })
}

fn read_session<R>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session<R>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R
where
    R: Send,
{
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn resolve_input_peer(live: &LiveSession, peer_id: PeerId) -> Option<tl_enums::InputPeer> {
    let data_peer = to_data_peer(peer_id);
    let peer = read_session(live, |s| input_peer_for(s, data_peer));
    match peer {
        tl_enums::InputPeer::Empty => None,
        other => Some(other),
    }
}

fn serialize_text_with_entities(
    live: &LiveSession,
    text: &TextWithEntities,
) -> tl_enums::TextWithEntities {
    let entities = read_session(live, |s| ui_text_to_tl_entities(text, s));
    tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
        text: text.text.clone(),
        entities,
    })
}

fn parse_tl_text_with_entities(tl: &tl_enums::TextWithEntities) -> TextWithEntities {
    let tl_enums::TextWithEntities::TextWithEntities(inner) = tl;
    let data =
        tg_data::TextWithEntities::new(inner.text.clone(), entities_from_tl(&inner.entities));
    crate::convert::data_text_to_ui_text(&data)
}

// ============================================================================
// 1. Compose with AI, AI Tones & TranslateBox (`api_compose_with_ai.cpp`)
// ============================================================================

fn serialize_ai_tone_ref(tone: &AiComposeToneRef) -> tl_enums::InputAiComposeTone {
    match tone {
        AiComposeToneRef::DefaultTone(t) => {
            tl_enums::InputAiComposeTone::Default(tl_types::InputAiComposeToneDefault {
                tone: t.clone(),
            })
        }
        AiComposeToneRef::Custom { id, access_hash } => {
            tl_enums::InputAiComposeTone::ID(tl_types::InputAiComposeToneID {
                id: *id,
                access_hash: *access_hash,
            })
        }
        AiComposeToneRef::Slug(slug) => {
            tl_enums::InputAiComposeTone::Slug(tl_types::InputAiComposeToneSlug {
                slug: slug.clone(),
            })
        }
        AiComposeToneRef::SingleUsePrompt(prompt) => {
            tl_enums::InputAiComposeTone::SingleUse(tl_types::InputAiComposeToneSingleUse {
                custom_prompt: prompt.clone(),
            })
        }
    }
}

fn parse_ai_tone_example(tl: &tl_enums::AiComposeToneExample) -> AiComposeToneExample {
    let tl_enums::AiComposeToneExample::AiComposeToneExample(ex) = tl;
    AiComposeToneExample {
        from: parse_tl_text_with_entities(&ex.from),
        to: parse_tl_text_with_entities(&ex.to),
    }
}

fn parse_ai_tone_tl(tl: &tl_enums::AiComposeTone) -> AiComposeTone {
    match tl {
        tl_enums::AiComposeTone::AiComposeTone(d) => {
            let emoji_id = d.emoji_id.unwrap_or(0);
            AiComposeTone {
                id: d.id,
                access_hash: d.access_hash,
                slug: d.slug.clone(),
                title: d.title.clone(),
                emoji_id,
                emoji_label: emoji_label_for_id(emoji_id).to_owned(),
                prompt: d.prompt.clone().unwrap_or_default(),
                installs_count: d.installs_count.unwrap_or(0),
                author_id: d.author_id.unwrap_or(0),
                creator: d.creator,
                is_default: false,
                default_type: String::new(),
                first_example: d.example_english.as_ref().map(parse_ai_tone_example),
            }
        }
        tl_enums::AiComposeTone::Default(d) => AiComposeTone {
            id: 0,
            access_hash: 0,
            slug: String::new(),
            title: d.title.clone(),
            emoji_id: d.emoji_id,
            emoji_label: emoji_label_for_id(d.emoji_id).to_owned(),
            prompt: String::new(),
            installs_count: 0,
            author_id: 0,
            creator: false,
            is_default: true,
            default_type: d.tone.clone(),
            first_example: None,
        },
    }
}

/// Parses `messages.composedMessageWithAI.diff_text` into [`ComposeAiDiff`]
/// (`Api::ComposeWithAi::ParseDiff`).
#[must_use]
pub fn parse_compose_ai_diff(tl: &tl_enums::TextWithEntities) -> ComposeAiDiff {
    let tl_enums::TextWithEntities::TextWithEntities(inner) = tl;
    let mut entities = Vec::new();
    for ent in &inner.entities {
        match ent {
            tl_enums::MessageEntity::DiffInsert(d) => entities.push(ComposeAiDiffEntity {
                kind: ComposeAiDiffKind::Insert,
                offset: d.offset,
                length: d.length,
            }),
            tl_enums::MessageEntity::DiffReplace(d) => entities.push(ComposeAiDiffEntity {
                kind: ComposeAiDiffKind::Replace {
                    old_text: d.old_text.clone(),
                },
                offset: d.offset,
                length: d.length,
            }),
            tl_enums::MessageEntity::DiffDelete(d) => entities.push(ComposeAiDiffEntity {
                kind: ComposeAiDiffKind::Delete,
                offset: d.offset,
                length: d.length,
            }),
            _ => {}
        }
    }
    ComposeAiDiff {
        text: parse_tl_text_with_entities(tl),
        entities,
    }
}

impl ComposeAiBackend for LiveSession {
    fn list_ai_tones(&mut self) -> Result<Vec<AiComposeTone>, String> {
        let current_hash = with_wp28_state(self, |st| st.tones_hash);
        let resp = invoke_wp28(self, tl_fns::aicompose::GetTones { hash: current_hash })?;
        match resp {
            tl_enums::aicompose::Tones::NotModified => {
                Ok(with_wp28_state(self, |st| st.tones.clone()))
            }
            tl_enums::aicompose::Tones::Tones(data) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &data.users, &[]);
                });
                let parsed: Vec<AiComposeTone> = data.tones.iter().map(parse_ai_tone_tl).collect();
                let list = with_wp28_state(self, |st| {
                    st.tones_hash = data.hash;
                    st.tones = parsed;
                    st.reapply_recent_custom_order();
                    st.tones.clone()
                });
                self.bridge_state().notify_ui();
                Ok(list)
            }
        }
    }

    fn compose_with_ai(&mut self, request: ComposeAiRequest) -> Result<ComposeAiResult, String> {
        if request.text.text.trim().is_empty() {
            return Err("Text cannot be empty.".to_owned());
        }
        let tl_req = tl_fns::messages::ComposeMessageWithAI {
            proofread: request.proofread,
            emojify: request.emojify,
            text: serialize_text_with_entities(self, &request.text),
            translate_to_lang: request.translate_to_lang.filter(|s| !s.trim().is_empty()),
            tone: request.tone.as_ref().map(serialize_ai_tone_ref),
        };
        let tl_enums::messages::ComposedMessageWithAI::ComposedMessageWithAI(res) =
            invoke_wp28(self, tl_req)?;
        Ok(ComposeAiResult {
            result_text: parse_tl_text_with_entities(&res.result_text),
            diff: res.diff_text.as_ref().map(parse_compose_ai_diff),
        })
    }

    fn compose_rich_message_with_ai(
        &mut self,
        request: ComposeRichAiRequest,
    ) -> Result<ComposeRichAiResult, String> {
        let resolver = InMemoryMediaResolver::default();
        let tl_text = match request.rich_page {
            Some(ref page) => {
                if page.blocks.is_empty() {
                    return Err("Rich message page cannot be empty.".to_owned());
                }
                let ser =
                    serialize_input_rich_message(page, SerializeRichMessageMode::Draft, &resolver);
                if ser.status != SerializeRichMessageStatus::Success {
                    return Err("Failed to serialize rich message for AI compose.".to_owned());
                }
                let Some(irm) = ser.value else {
                    return Err("Serialized rich message payload was empty.".to_owned());
                };
                Some(tl_enums::InputRichMessage::InputRichMessage(irm))
            }
            None => {
                if request.tone.is_none() {
                    return Err("Provide rich message content or a custom prompt.".to_owned());
                }
                None
            }
        };

        // Mirrors `Telegram/SourceFiles/api/api_compose_with_ai.cpp:193-220` and
        // `Telegram/SourceFiles/boxes/create_ai_box.cpp:620-645`.
        let tl_req = tl_fns::messages::ComposeRichMessageWithAI {
            proofread: request.proofread,
            emojify: request.emojify,
            text: tl_text,
            translate_to_lang: request
                .translate_to_lang
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            tone: request.tone.as_ref().map(serialize_ai_tone_ref),
        };
        let tl_enums::messages::ComposedRichMessageWithAI::ComposedRichMessageWithAI(res) =
            invoke_wp28(self, tl_req)?;
        let tl_enums::RichMessage::RichMessage(ref tl_rm) = res.result;
        let page = rich_page_from_tl_rich_message(tl_rm, RichParseMode::Normal);
        let display_page = if request.proofread {
            rich_page_from_tl_rich_message(tl_rm, RichParseMode::DisplayTextDiff)
        } else {
            page.clone()
        };
        Ok(ComposeRichAiResult { page, display_page })
    }

    fn translate_rich_message(
        &mut self,
        request: TranslateRichMessageRequest,
    ) -> Result<TranslateRichMessageResult, String> {
        /// Maximum items per `messages.translateRichMessage` request
        /// (`kRequestCountLimit = 20` in `history_view_translate_tracker.cpp:27`).
        const TRANSLATE_RICH_BATCH_LIMIT: usize = 20;

        let to_lang = request.to_lang.trim().to_owned();
        if to_lang.is_empty() {
            return Err("Target language cannot be empty.".to_owned());
        }
        let tone = request
            .tone
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let mut pages = Vec::new();
        match request.target {
            TranslateRichMessageTarget::Messages { peer_id, msg_ids } => {
                if msg_ids.is_empty() {
                    return Ok(TranslateRichMessageResult { pages });
                }
                let Some(tl_peer) = resolve_input_peer(self, peer_id) else {
                    return Err("Peer not found in session.".to_owned());
                };
                for chunk in msg_ids.chunks(TRANSLATE_RICH_BATCH_LIMIT) {
                    let ids: Vec<i32> = chunk.iter().map(|m| m.0).collect();
                    let tl_enums::messages::TranslatedRichMessage::TranslatedRichMessage(res) =
                        invoke_wp28(
                            self,
                            tl_fns::messages::TranslateRichMessage {
                                peer: Some(tl_peer.clone()),
                                id: Some(ids),
                                text: None,
                                to_lang: to_lang.clone(),
                                tone: tone.clone(),
                            },
                        )?;
                    for tl_enums::RichMessage::RichMessage(rm) in &res.result {
                        pages.push(rich_page_from_tl_rich_message(rm, RichParseMode::Normal));
                    }
                }
            }
            TranslateRichMessageTarget::RichPages(src_pages) => {
                if src_pages.is_empty() {
                    return Ok(TranslateRichMessageResult { pages });
                }
                let resolver = InMemoryMediaResolver::default();
                let mut serialized = Vec::with_capacity(src_pages.len());
                for p in &src_pages {
                    let ser =
                        serialize_input_rich_message(p, SerializeRichMessageMode::Draft, &resolver);
                    if ser.status != SerializeRichMessageStatus::Success {
                        return Err("Failed to serialize rich message for translation.".to_owned());
                    }
                    let Some(irm) = ser.value else {
                        return Err("Serialized rich message payload was empty.".to_owned());
                    };
                    serialized.push(tl_enums::InputRichMessage::InputRichMessage(irm));
                }
                for chunk in serialized.chunks(TRANSLATE_RICH_BATCH_LIMIT) {
                    let tl_enums::messages::TranslatedRichMessage::TranslatedRichMessage(res) =
                        invoke_wp28(
                            self,
                            tl_fns::messages::TranslateRichMessage {
                                peer: None,
                                id: None,
                                text: Some(chunk.to_vec()),
                                to_lang: to_lang.clone(),
                                tone: tone.clone(),
                            },
                        )?;
                    for tl_enums::RichMessage::RichMessage(rm) in &res.result {
                        pages.push(rich_page_from_tl_rich_message(rm, RichParseMode::Normal));
                    }
                }
            }
        }
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
        let resp = invoke_wp28(
            self,
            tl_fns::aicompose::CreateTone {
                display_author,
                emoji_id,
                title: title.trim().to_owned(),
                prompt: prompt.trim().to_owned(),
            },
        )?;
        let parsed = parse_ai_tone_tl(&resp);
        with_wp28_state(self, |st| {
            st.tones_hash = 0;
            st.promote_custom_tone(parsed.clone());
        });
        self.bridge_state().notify_ui();
        Ok(parsed)
    }

    fn update_ai_tone(
        &mut self,
        tone: &AiComposeTone,
        title: Option<&str>,
        prompt: Option<&str>,
        emoji_id: Option<i64>,
        display_author: Option<bool>,
    ) -> Result<AiComposeTone, String> {
        if let (Some(t), Some(p)) = (title, prompt) {
            validate_ai_tone_fields(t, p)?;
        }
        let resp = invoke_wp28(
            self,
            tl_fns::aicompose::UpdateTone {
                tone: serialize_ai_tone_ref(&tone.to_ref()),
                display_author,
                emoji_id,
                title: title.map(|s| s.trim().to_owned()),
                prompt: prompt.map(|s| s.trim().to_owned()),
            },
        )?;
        let parsed = parse_ai_tone_tl(&resp);
        with_wp28_state(self, |st| {
            st.tones_hash = 0;
            st.promote_custom_tone(parsed.clone());
        });
        self.bridge_state().notify_ui();
        Ok(parsed)
    }

    fn save_ai_tone(&mut self, tone: &AiComposeTone, unsave: bool) -> Result<(), String> {
        let _ = invoke_wp28(
            self,
            tl_fns::aicompose::SaveTone {
                tone: serialize_ai_tone_ref(&tone.to_ref()),
                unsave,
            },
        )?;
        with_wp28_state(self, |st| {
            st.tones_hash = 0;
            if unsave {
                st.remove_custom_tone(tone.id);
            } else {
                st.promote_custom_tone(tone.clone());
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn delete_ai_tone(&mut self, tone: &AiComposeTone) -> Result<(), String> {
        let _ = invoke_wp28(
            self,
            tl_fns::aicompose::DeleteTone {
                tone: serialize_ai_tone_ref(&tone.to_ref()),
            },
        )?;
        with_wp28_state(self, |st| {
            st.tones_hash = 0;
            st.remove_custom_tone(tone.id);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn resolve_ai_tone(&mut self, slug: &str) -> Result<AiComposeTone, String> {
        let cleaned = slug.trim();
        if cleaned.is_empty() {
            return Err("TONE_NOT_FOUND".to_owned());
        }
        let resp = invoke_wp28(
            self,
            tl_fns::aicompose::GetTone {
                tone: tl_enums::InputAiComposeTone::Slug(tl_types::InputAiComposeToneSlug {
                    slug: cleaned.to_owned(),
                }),
            },
        )?;
        match resp {
            tl_enums::aicompose::Tones::Tones(data) => {
                write_session(self, |s| {
                    apply_tl_users_and_chats(s, &data.users, &[]);
                });
                data.tones
                    .first()
                    .map(parse_ai_tone_tl)
                    .ok_or_else(|| "TONE_NOT_FOUND".to_owned())
            }
            tl_enums::aicompose::Tones::NotModified => Err("TONE_NOT_MODIFIED".to_owned()),
        }
    }

    fn is_ai_tone_installed(&self, tone: &AiComposeTone) -> bool {
        with_wp28_state(self, |st| {
            st.tones.iter().any(|t| {
                !t.is_default
                    && ((tone.id != 0 && t.id == tone.id)
                        || (!tone.slug.is_empty() && t.slug.eq_ignore_ascii_case(&tone.slug)))
            })
        })
    }

    fn get_ai_tone_example(
        &mut self,
        tone: &AiComposeTone,
        num: i32,
    ) -> Result<AiComposeToneExample, String> {
        let resp = invoke_wp28(
            self,
            tl_fns::aicompose::GetToneExample {
                tone: serialize_ai_tone_ref(&tone.to_ref()),
                num,
            },
        )?;
        Ok(parse_ai_tone_example(&resp))
    }

    fn translate_text_modal(
        &mut self,
        peer_id: Option<PeerId>,
        msg_id: Option<MsgId>,
        text: &TextWithEntities,
        to_lang: &str,
        tone: Option<&str>,
    ) -> Result<TextWithEntities, String> {
        let tl_peer = peer_id.and_then(|pid| resolve_input_peer(self, pid));
        let (peer, id, raw_text) = match (tl_peer, msg_id) {
            (Some(p), Some(mid)) => (Some(p), Some(vec![mid.0]), None),
            _ => (
                None,
                None,
                Some(vec![serialize_text_with_entities(self, text)]),
            ),
        };
        let tl_enums::messages::TranslatedText::TranslateResult(res) = invoke_wp28(
            self,
            tl_fns::messages::TranslateText {
                peer,
                id,
                text: raw_text,
                to_lang: to_lang.trim().to_owned(),
                tone: tone
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            },
        )?;
        res.result
            .first()
            .map(parse_tl_text_with_entities)
            .ok_or_else(|| "Translation returned empty result.".to_owned())
    }
}

// ============================================================================
// 2. Sticker Creator (`api_stickers_creator.cpp`)
// ============================================================================

fn upload_raw_file_parts(
    live: &LiveSession,
    file_name: &str,
    bytes: &[u8],
) -> Result<tl_enums::InputFile, String> {
    if bytes.is_empty() {
        return Err("Cannot upload empty file.".to_owned());
    }
    let file_id = with_wp28_state(live, Wp28LiveState::next_random_id);
    let total_parts = bytes.len().div_ceil(UPLOAD_PART_BYTES);
    for (part_idx, chunk) in bytes.chunks(UPLOAD_PART_BYTES).enumerate() {
        let ok = invoke_wp28(
            live,
            tl_fns::upload::SaveFilePart {
                file_id,
                file_part: part_idx as i32,
                bytes: chunk.to_vec(),
            },
        )?;
        if !ok {
            return Err(format!("upload.saveFilePart rejected part {part_idx}."));
        }
    }
    Ok(tl_enums::InputFile::InputFile(tl_types::InputFile {
        id: file_id,
        parts: total_parts as i32,
        name: file_name.to_owned(),
        md5_checksum: String::new(),
    }))
}

#[allow(clippy::too_many_arguments)]
fn upload_sticker_document(
    live: &LiveSession,
    file_name: &str,
    mime_type: &str,
    bytes: &[u8],
    width: u32,
    height: u32,
    video_duration_ms: u32,
    emoji: &str,
) -> Result<tl_enums::InputDocument, String> {
    let input_file = upload_raw_file_parts(live, file_name, bytes)?;
    let mut attributes = vec![
        tl_enums::DocumentAttribute::Filename(tl_types::DocumentAttributeFilename {
            file_name: file_name.to_owned(),
        }),
        tl_enums::DocumentAttribute::Sticker(tl_types::DocumentAttributeSticker {
            mask: false,
            alt: emoji.to_owned(),
            stickerset: tl_enums::InputStickerSet::Empty,
            mask_coords: None,
        }),
    ];
    if video_duration_ms > 0 {
        attributes.push(tl_enums::DocumentAttribute::Video(
            tl_types::DocumentAttributeVideo {
                round_message: false,
                supports_streaming: false,
                nosound: true,
                duration: f64::from(video_duration_ms) / 1000.0,
                w: width as i32,
                h: height as i32,
                preload_prefix_size: None,
                video_start_ts: None,
                video_codec: None,
            },
        ));
    } else {
        attributes.push(tl_enums::DocumentAttribute::ImageSize(
            tl_types::DocumentAttributeImageSize {
                w: width as i32,
                h: height as i32,
            },
        ));
    }

    let uploaded_media = invoke_wp28(
        live,
        tl_fns::messages::UploadMedia {
            business_connection_id: None,
            peer: tl_enums::InputPeer::Self_,
            media: tl_enums::InputMedia::UploadedDocument(tl_types::InputMediaUploadedDocument {
                nosound_video: video_duration_ms > 0,
                force_file: false,
                spoiler: false,
                file: input_file,
                thumb: None,
                mime_type: mime_type.to_owned(),
                attributes,
                stickers: None,
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: None,
            }),
        },
    )?;

    match uploaded_media {
        tl_enums::MessageMedia::Document(doc_media) => match doc_media.document {
            Some(tl_enums::Document::Document(doc)) => Ok(tl_enums::InputDocument::InputDocument(
                tl_types::InputDocument {
                    id: doc.id,
                    access_hash: doc.access_hash,
                    file_reference: doc.file_reference,
                },
            )),
            _ => Err("messages.uploadMedia returned empty document.".to_owned()),
        },
        _ => Err("messages.uploadMedia did not return a document.".to_owned()),
    }
}

fn parse_sticker_set_summary(
    tl: &tl_enums::messages::StickerSet,
    fallback_kind: StickerSetKind,
) -> Result<OwnedStickerSetSummary, String> {
    match tl {
        tl_enums::messages::StickerSet::StickerSet(full) => {
            let tl_enums::StickerSet::StickerSet(set) = &full.set;
            let kind = if set.emojis {
                StickerSetKind::CustomEmoji
            } else if set.masks {
                StickerSetKind::Masks
            } else {
                fallback_kind
            };
            Ok(OwnedStickerSetSummary {
                id: set.id,
                access_hash: set.access_hash,
                short_name: set.short_name.clone(),
                title: set.title.clone(),
                count: set.count.max(0) as usize,
                kind,
            })
        }
        tl_enums::messages::StickerSet::NotModified => {
            Err("Sticker set response was not modified.".to_owned())
        }
    }
}

impl StickerCreatorBackend for LiveSession {
    fn owned_sticker_sets(&mut self, kind: StickerSetKind) -> Vec<OwnedStickerSetSummary> {
        let fetched = match kind {
            StickerSetKind::CustomEmoji => {
                invoke_wp28(self, tl_fns::messages::GetEmojiStickers { hash: 0 })
            }
            StickerSetKind::Masks => {
                invoke_wp28(self, tl_fns::messages::GetMaskStickers { hash: 0 })
            }
            StickerSetKind::Regular => {
                invoke_wp28(self, tl_fns::messages::GetAllStickers { hash: 0 })
            }
        };
        if let Ok(tl_enums::messages::AllStickers::AllStickers(all)) = fetched {
            let owned: Vec<OwnedStickerSetSummary> = all
                .sets
                .iter()
                .filter_map(|raw| {
                    let tl_enums::StickerSet::StickerSet(set) = raw;
                    if !set.creator {
                        return None;
                    }
                    Some(OwnedStickerSetSummary {
                        id: set.id,
                        access_hash: set.access_hash,
                        short_name: set.short_name.clone(),
                        title: set.title.clone(),
                        count: set.count.max(0) as usize,
                        kind,
                    })
                })
                .collect();
            with_wp28_state(self, |st| {
                st.owned_sticker_sets.retain(|s| s.kind != kind);
                st.owned_sticker_sets.extend(owned.clone());
            });
            return owned;
        }
        with_wp28_state(self, |st| {
            st.owned_sticker_sets
                .iter()
                .filter(|s| s.kind == kind)
                .cloned()
                .collect()
        })
    }

    fn suggest_sticker_short_name(&mut self, title: &str) -> Result<String, String> {
        let tl_enums::stickers::SuggestedShortName::SuggestedShortName(res) = invoke_wp28(
            self,
            tl_fns::stickers::SuggestShortName {
                title: title.trim().to_owned(),
            },
        )?;
        Ok(res.short_name)
    }

    fn check_sticker_short_name(&mut self, short_name: &str) -> Result<bool, String> {
        validate_sticker_set_meta("Check", short_name)?;
        invoke_wp28(
            self,
            tl_fns::stickers::CheckShortName {
                short_name: short_name.trim().to_owned(),
            },
        )
    }

    fn create_sticker_set(
        &mut self,
        request: CreateStickerSetRequest,
    ) -> Result<OwnedStickerSetSummary, String> {
        validate_sticker_set_meta(&request.title, &request.short_name)?;
        let emoji = normalize_sticker_emoji(&request.emoji);
        let input_doc = upload_sticker_document(
            self,
            &request.asset.file_name,
            &request.asset.mime_type,
            &request.asset.bytes,
            request.asset.width,
            request.asset.height,
            request.asset.video_duration_ms,
            &emoji,
        )?;
        let item =
            tl_enums::InputStickerSetItem::InputStickerSetItem(tl_types::InputStickerSetItem {
                document: input_doc,
                emoji,
                mask_coords: None,
                keywords: (!request.keywords.trim().is_empty())
                    .then(|| request.keywords.trim().to_owned()),
            });
        let resp = invoke_wp28(
            self,
            tl_fns::stickers::CreateStickerSet {
                masks: request.kind == StickerSetKind::Masks,
                emojis: request.kind == StickerSetKind::CustomEmoji,
                text_color: false,
                user_id: tl_enums::InputUser::Self_,
                title: request.title.trim().to_owned(),
                short_name: request.short_name.trim().to_owned(),
                thumb: None,
                stickers: vec![item],
                software: Some(STICKER_CREATOR_SOFTWARE.to_owned()),
            },
        )?;
        let summary = parse_sticker_set_summary(&resp, request.kind)?;
        with_wp28_state(self, |st| {
            st.owned_sticker_sets.retain(|s| s.id != summary.id);
            st.owned_sticker_sets.insert(0, summary.clone());
        });
        self.bridge_state().notify_ui();
        Ok(summary)
    }

    fn add_sticker_to_set(
        &mut self,
        request: AddStickerToSetRequest,
    ) -> Result<OwnedStickerSetSummary, String> {
        let emoji = normalize_sticker_emoji(&request.emoji);
        let input_doc = upload_sticker_document(
            self,
            &request.asset.file_name,
            &request.asset.mime_type,
            &request.asset.bytes,
            request.asset.width,
            request.asset.height,
            request.asset.video_duration_ms,
            &emoji,
        )?;
        let stickerset = if request.set_id != 0 {
            tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: request.set_id,
                access_hash: request.set_access_hash,
            })
        } else {
            tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
                short_name: request.set_short_name.clone(),
            })
        };
        let item =
            tl_enums::InputStickerSetItem::InputStickerSetItem(tl_types::InputStickerSetItem {
                document: input_doc,
                emoji,
                mask_coords: None,
                keywords: (!request.keywords.trim().is_empty())
                    .then(|| request.keywords.trim().to_owned()),
            });
        let resp = invoke_wp28(
            self,
            tl_fns::stickers::AddStickerToSet {
                stickerset,
                sticker: item,
            },
        )?;
        let summary = parse_sticker_set_summary(&resp, request.kind)?;
        with_wp28_state(self, |st| {
            st.owned_sticker_sets.retain(|s| s.id != summary.id);
            st.owned_sticker_sets.insert(0, summary.clone());
        });
        self.bridge_state().notify_ui();
        Ok(summary)
    }

    fn delete_sticker_set(
        &mut self,
        set_id: i64,
        access_hash: i64,
        short_name: &str,
    ) -> Result<(), String> {
        let stickerset = if set_id != 0 {
            tl_enums::InputStickerSet::ID(tl_types::InputStickerSetID {
                id: set_id,
                access_hash,
            })
        } else {
            tl_enums::InputStickerSet::ShortName(tl_types::InputStickerSetShortName {
                short_name: short_name.to_owned(),
            })
        };
        let _ = invoke_wp28(self, tl_fns::stickers::DeleteStickerSet { stickerset })?;
        with_wp28_state(self, |st| {
            st.owned_sticker_sets
                .retain(|s| s.id != set_id && s.short_name != short_name);
        });
        self.bridge_state().notify_ui();
        Ok(())
    }
}

// ============================================================================
// 3. Self-Destruct (`api_self_destruct.cpp` + `messages.readMessageContents`)
// ============================================================================

impl SelfDestructBackend for LiveSession {
    fn reload_self_destruct(&mut self) -> Result<SelfDestructSnapshot, String> {
        let tl_enums::AccountDaysTTL::AccountDaysTTL(acct) =
            invoke_wp28(self, tl_fns::account::GetAccountTTL {})?;
        let tl_enums::DefaultHistoryTTL::DefaultHistoryTTL(hist) =
            invoke_wp28(self, tl_fns::messages::GetDefaultHistoryTTL {})?;
        let snap = with_wp28_state(self, |st| {
            st.self_destruct.account_ttl_days = acct.days;
            st.self_destruct.default_history_ttl_secs = hist.period;
            st.self_destruct
        });
        Ok(snap)
    }

    fn update_account_ttl(&mut self, days: i32) -> Result<(), String> {
        if days <= 0 {
            return Err("Account TTL days must be positive.".to_owned());
        }
        let _ = invoke_wp28(
            self,
            tl_fns::account::SetAccountTTL {
                ttl: tl_enums::AccountDaysTTL::AccountDaysTTL(tl_types::AccountDaysTTL { days }),
            },
        )?;
        with_wp28_state(self, |st| {
            st.self_destruct.account_ttl_days = days;
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn update_sessions_ttl(&mut self, days: i32) -> Result<(), String> {
        if days <= 0 {
            return Err("Session TTL days must be positive.".to_owned());
        }
        let _ = invoke_wp28(
            self,
            tl_fns::account::SetAuthorizationTTL {
                authorization_ttl_days: days,
            },
        )?;
        with_wp28_state(self, |st| {
            st.self_destruct.sessions_ttl_days = days;
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn update_default_history_ttl(&mut self, period_secs: i32) -> Result<(), String> {
        if period_secs < 0 {
            return Err("Default history TTL cannot be negative.".to_owned());
        }
        let _ = invoke_wp28(
            self,
            tl_fns::messages::SetDefaultHistoryTTL {
                period: period_secs,
            },
        )?;
        with_wp28_state(self, |st| {
            st.self_destruct.default_history_ttl_secs = period_secs;
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn mark_ttl_media_viewed(&mut self, peer_id: PeerId, msg_id: MsgId) -> Result<(), String> {
        let tl_enums::messages::AffectedMessages::AffectedMessages(aff) = invoke_wp28(
            self,
            tl_fns::messages::ReadMessageContents { id: vec![msg_id.0] },
        )?;
        let _ = self.apply_affected_history(None, aff.pts, aff.pts_count);
        with_wp28_state(self, |st| {
            if !st.viewed_ttl_messages.contains(&(peer_id, msg_id)) {
                st.viewed_ttl_messages.push((peer_id, msg_id));
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }
}

// ============================================================================
// 4. Layer-233 Poll Option Media / Links & Add/Delete Poll Option (`api_polls.cpp`)
// ============================================================================

fn poll_attachment_to_input_media(
    live: &LiveSession,
    att: &PollOptionAttachment,
) -> Result<tl_enums::InputMedia, String> {
    match att {
        PollOptionAttachment::Link { url, .. } => {
            Ok(tl_enums::InputMedia::WebPage(tl_types::InputMediaWebPage {
                force_large_media: false,
                force_small_media: false,
                optional: true,
                url: url.clone(),
            }))
        }
        PollOptionAttachment::Media {
            file_name,
            mime_type,
            bytes,
        } => {
            let input_file = upload_raw_file_parts(live, file_name, bytes)?;
            if mime_type.starts_with("image/") && !mime_type.ends_with("webp") {
                Ok(tl_enums::InputMedia::UploadedPhoto(
                    tl_types::InputMediaUploadedPhoto {
                        spoiler: false,
                        live_photo: false,
                        file: input_file,
                        stickers: None,
                        ttl_seconds: None,
                        video: None,
                    },
                ))
            } else {
                Ok(tl_enums::InputMedia::UploadedDocument(
                    tl_types::InputMediaUploadedDocument {
                        nosound_video: false,
                        force_file: false,
                        spoiler: false,
                        file: input_file,
                        thumb: None,
                        mime_type: mime_type.clone(),
                        attributes: vec![tl_enums::DocumentAttribute::Filename(
                            tl_types::DocumentAttributeFilename {
                                file_name: file_name.clone(),
                            },
                        )],
                        stickers: None,
                        video_cover: None,
                        video_timestamp: None,
                        ttl_seconds: None,
                    },
                ))
            }
        }
    }
}

impl LiveSession {
    /// Creates and sends a Layer-233 poll with `open_answers`, `shuffle_answers`,
    /// `revoting_disabled`, `hide_results_until_close`, and per-option media/link attachments.
    pub fn send_poll_with_extras_live(
        &mut self,
        peer_id: PeerId,
        thread_key: Option<ThreadKey>,
        request: CreatePollRequest,
        extras: PollMessageExtras,
    ) -> Result<HistoryMessage, String> {
        validate_create_poll(&request, POLL_MAX_OPTIONS_EXTENDED).map_err(|e| e.message())?;
        let Some(tl_peer) = resolve_input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let random_id = with_wp28_state(self, Wp28LiveState::next_random_id);

        let mut answers = Vec::with_capacity(request.options.len());
        let mut option_bytes = Vec::with_capacity(request.options.len());
        for (idx, opt_text) in request.options.iter().enumerate() {
            let opt_media = match extras.option_attachments.get(idx).and_then(|o| o.as_ref()) {
                Some(att) => Some(poll_attachment_to_input_media(self, att)?),
                None => None,
            };
            option_bytes.push(vec![idx as u8]);
            answers.push(tl_enums::PollAnswer::InputPollAnswer(
                tl_types::InputPollAnswer {
                    text: tl_enums::TextWithEntities::TextWithEntities(
                        tl_types::TextWithEntities {
                            text: opt_text.trim().to_owned(),
                            entities: Vec::new(),
                        },
                    ),
                    media: opt_media,
                },
            ));
        }

        let attached_media = match extras.attached_media.as_ref() {
            Some(att) => Some(poll_attachment_to_input_media(self, att)?),
            None => None,
        };
        let solution_media = match extras.solution_media.as_ref() {
            Some(att) => Some(poll_attachment_to_input_media(self, att)?),
            None => None,
        };

        let correct_answers = if request.is_quiz {
            request.correct_option.map(|i| vec![i as i32])
        } else {
            None
        };
        let solution = if request.is_quiz {
            request
                .explanation
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        } else {
            None
        };

        let tl_poll = tl_enums::Poll::Poll(tl_types::Poll {
            id: random_id,
            closed: false,
            public_voters: !request.is_anonymous,
            multiple_choice: request.multiple_choice && !request.is_quiz,
            quiz: request.is_quiz,
            open_answers: extras.open_answers && !request.is_quiz,
            revoting_disabled: extras.revoting_disabled,
            shuffle_answers: extras.shuffle_answers,
            hide_results_until_close: extras.hide_results_until_close,
            creator: true,
            subscribers_only: false,
            question: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: request.question.trim().to_owned(),
                entities: Vec::new(),
            }),
            answers,
            close_period: None,
            close_date: None,
            countries_iso2: None,
            hash: 0,
        });

        let reply_to = match thread_key {
            Some(ThreadKey::Topic { topic_root_id, .. }) => Some(tl_enums::InputReplyTo::Message(
                tl_types::InputReplyToMessage {
                    reply_to_msg_id: topic_root_id.0,
                    top_msg_id: Some(topic_root_id.0),
                    reply_to_peer_id: None,
                    quote_text: None,
                    quote_entities: None,
                    quote_offset: None,
                    quote_rich: None,
                    monoforum_peer_id: None,
                    todo_item_id: None,
                    poll_option: None,
                },
            )),
            _ => None,
        };

        let updates = invoke_wp28(
            self,
            tl_fns::messages::SendMedia {
                silent: request.silent,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: tl_peer,
                reply_to,
                media: tl_enums::InputMedia::Poll(tl_types::InputMediaPoll {
                    poll: tl_poll,
                    correct_answers,
                    attached_media: attached_media.map(Box::new),
                    solution: solution.clone(),
                    solution_entities: solution.as_ref().map(|_| Vec::new()),
                    solution_media: solution_media.map(Box::new),
                }),
                message: String::new(),
                random_id,
                reply_markup: None,
                entities: None,
                schedule_date: request.schedule_date,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            },
        )?;

        let sent_msg_id =
            extract_sent_msg_id(&updates).unwrap_or(MsgId((random_id & 0x7FFF) as i32));
        let _ = self.apply_updates_to_session(&updates);
        let mut stored_extras = extras;
        stored_extras.option_bytes = option_bytes;

        with_wp28_state(self, |st| {
            st.poll_extras_cache
                .insert((peer_id, sent_msg_id), stored_extras.clone());
        });

        let (self_id, self_name) = read_session(self, |s| {
            let sid = PeerId::from_user(s.self_user_id().0);
            let name = peer_display_title(s, s.self_peer_id());
            (sid, name)
        });

        let poll_att = PollAttachment {
            question: request.question.trim().to_owned(),
            is_quiz: request.is_quiz,
            is_anonymous: request.is_anonymous,
            multiple_choice: request.multiple_choice && !request.is_quiz,
            closed: false,
            options: request
                .options
                .iter()
                .enumerate()
                .map(|(i, t)| PollOption {
                    text: t.trim().to_owned(),
                    votes: 0,
                    chosen: false,
                    correct: request.is_quiz && request.correct_option == Some(i),
                })
                .collect(),
            total_voters: 0,
            explanation: solution,
        };

        self.bridge_state().notify_ui();
        Ok(HistoryMessage {
            id: sent_msg_id,
            peer_id,
            from_id: self_id,
            sender_name: self_name,
            date_unix: now_unix_i32(),
            outgoing: true,
            is_post: false,
            text: TextWithEntities::default(),
            media: Some(tg_app_core::history::backend::HistoryMedia::Poll(poll_att)),
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra {
                scheduled_date: request.schedule_date,
                silent: request.silent,
                poll_extras: Some(stored_extras),
                ..MessageExtra::default()
            },
        })
    }

    /// Appends a new option (with optional Layer-233 link/media attachment) to an existing poll
    /// via `messages.addPollAnswer`.
    pub fn add_poll_option_live(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        request: AddPollOptionRequest,
    ) -> Result<HistoryMessage, String> {
        let trimmed = request.text.trim();
        if trimmed.is_empty() && request.attachment.is_none() {
            return Err("Poll option cannot be empty.".to_owned());
        }
        if trimmed.chars().count() > POLL_OPTION_LIMIT {
            return Err(format!(
                "Poll option exceeds {POLL_OPTION_LIMIT} characters."
            ));
        }
        let Some(tl_peer) = resolve_input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let media = match request.attachment.as_ref() {
            Some(att) => Some(poll_attachment_to_input_media(self, att)?),
            None => None,
        };
        let answer = tl_enums::PollAnswer::InputPollAnswer(tl_types::InputPollAnswer {
            text: tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
                text: trimmed.to_owned(),
                entities: Vec::new(),
            }),
            media,
        });
        let updates = invoke_wp28(
            self,
            tl_fns::messages::AddPollAnswer {
                peer: tl_peer,
                msg_id: msg_id.0,
                answer,
            },
        )?;
        let _ = self.apply_updates_to_session(&updates);

        let self_name = read_session(self, |s| peer_display_title(s, s.self_peer_id()));
        let updated_extras = with_wp28_state(self, |st| {
            let entry = st.poll_extras_cache.entry((peer_id, msg_id)).or_default();
            entry.open_answers = true;
            let next_byte = entry.option_bytes.len() as u8;
            entry.option_bytes.push(vec![next_byte]);
            entry.option_attachments.push(request.attachment.clone());
            entry.option_added_by.push(Some(self_name.clone()));
            entry.clone()
        });

        self.bridge_state().notify_ui();
        Ok(HistoryMessage {
            id: msg_id,
            peer_id,
            from_id: PeerId::from_user(0),
            sender_name: self_name,
            date_unix: now_unix_i32(),
            outgoing: true,
            is_post: false,
            text: TextWithEntities::default(),
            media: Some(tg_app_core::history::backend::HistoryMedia::Poll(
                PollAttachment {
                    question: String::new(),
                    is_quiz: false,
                    is_anonymous: false,
                    multiple_choice: false,
                    closed: false,
                    options: vec![PollOption {
                        text: trimmed.to_owned(),
                        votes: 0,
                        chosen: false,
                        correct: false,
                    }],
                    total_voters: 0,
                    explanation: None,
                },
            )),
            service: None,
            reply: None,
            forward: None,
            reactions: Vec::new(),
            views: None,
            edited: false,
            is_read: false,
            is_sending: false,
            is_pinned: false,
            extra: MessageExtra {
                poll_extras: Some(updated_extras),
                ..MessageExtra::default()
            },
        })
    }

    /// Removes a previously appended option from a poll via `messages.deletePollAnswer`.
    pub fn delete_poll_option_live(
        &mut self,
        peer_id: PeerId,
        msg_id: MsgId,
        option_bytes: &[u8],
    ) -> Result<(), String> {
        let Some(tl_peer) = resolve_input_peer(self, peer_id) else {
            return Err("Peer not found in session.".to_owned());
        };
        let updates = invoke_wp28(
            self,
            tl_fns::messages::DeletePollAnswer {
                peer: tl_peer,
                msg_id: msg_id.0,
                option: option_bytes.to_vec(),
            },
        )?;
        let _ = self.apply_updates_to_session(&updates);
        with_wp28_state(self, |st| {
            if let Some(extras) = st.poll_extras_cache.get_mut(&(peer_id, msg_id))
                && let Some(pos) = extras
                    .option_bytes
                    .iter()
                    .position(|b| b.as_slice() == option_bytes)
            {
                extras.option_bytes.remove(pos);
                if pos < extras.option_attachments.len() {
                    extras.option_attachments.remove(pos);
                }
                if pos < extras.option_added_by.len() {
                    extras.option_added_by.remove(pos);
                }
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    /// Returns whether `(peer_id, msg_id)` has been marked viewed in this session.
    #[must_use]
    pub fn is_ttl_media_viewed(&self, peer_id: PeerId, msg_id: MsgId) -> bool {
        with_wp28_state(self, |st| {
            st.viewed_ttl_messages.contains(&(peer_id, msg_id))
        })
    }

    /// Returns cached Layer-233 poll extras for `(peer_id, msg_id)`, if any.
    #[must_use]
    pub fn cached_poll_extras(&self, peer_id: PeerId, msg_id: MsgId) -> Option<PollMessageExtras> {
        with_wp28_state(self, |st| {
            st.poll_extras_cache.get(&(peer_id, msg_id)).cloned()
        })
    }
}

fn extract_sent_msg_id(updates: &tl_enums::Updates) -> Option<MsgId> {
    match updates {
        tl_enums::Updates::UpdateShortSentMessage(s) => Some(MsgId(s.id)),
        tl_enums::Updates::Updates(u) => {
            for upd in &u.updates {
                match upd {
                    tl_enums::Update::MessageID(mid) => return Some(MsgId(mid.id)),
                    tl_enums::Update::NewMessage(nm) => {
                        if let tl_enums::Message::Message(m) = &nm.message {
                            return Some(from_data_msg(tg_data::MsgId(i64::from(m.id))));
                        }
                    }
                    tl_enums::Update::NewChannelMessage(nm) => {
                        if let tl_enums::Message::Message(m) = &nm.message {
                            return Some(from_data_msg(tg_data::MsgId(i64::from(m.id))));
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

// ============================================================================
// 5. Real Webcam Round-Video Capture (`tg-voip::video_mf` + `tg-video`)
// ============================================================================
