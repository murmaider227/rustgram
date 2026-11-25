//! Live Telegram Stories backend (`StoriesBackend` for `LiveSession`).
//!
//! Port origin: `Telegram/SourceFiles/data/data_stories.cpp`,
//! `Telegram/SourceFiles/media/stories/media_stories_controller.cpp`,
//! `Telegram/SourceFiles/media/stories/media_stories_share.cpp`, and
//! `Telegram/SourceFiles/media/stories/media_stories_stealth.cpp`.
//!
//! Drives `tg-stories`'s [`StoriesStore`] and [`tg_stories::StoryViewerController`] over real
//! MTProto `stories.*` RPCs, consumes live `updateStory` / `updateReadStories` /
//! `updateStoriesStealthMode` updates from `tg-session`'s updates stream, and loads story
//! photos (`upload.getFile`) and videos ([`TelegramRangeLoader`] + `tg-video`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{apply_tl_users_and_chats, input_peer_for, input_user_for};
use tg_data::{
    ChannelFlags, DialogKey, FullStoryId, Peer, PeerId as DataPeerId, Session, StoryId, TimeId,
    UserFlags,
};
use tg_media::transfer::download::CancelToken;
use tg_stories::{
    ParseSource, PeerStoryMeta, RecentStoryInfo, STORIES_ALBUM_ID_ARCHIVE, STORIES_ALBUM_ID_SAVED,
    StoriesAction, StoriesStore, Story, StoryMedia, StoryPrivacy, StorySourcesList,
    StoryUploadStatus, build_activate_stealth_mode, build_delete_stories, build_export_story_link,
    build_get_album_stories, build_get_albums, build_get_all_read_peer_stories,
    build_get_all_stories, build_get_peer_max_ids, build_get_peer_stories,
    build_get_pinned_stories, build_get_stories_archive, build_get_stories_by_id,
    build_get_stories_views, build_get_story_reactions_list, build_get_story_views_list,
    build_increment_story_views, build_read_stories, build_reorder_album, build_report_story,
    build_send_reaction, build_toggle_peer_stories_hidden, build_toggle_pinned,
    build_toggle_pinned_to_top, reaction_to_tl, recent_story_from_tl, stories_page_from_tl,
    story_area_to_tl, story_reactions_list_from_tl, story_views_counts_from_tl,
    story_views_list_from_tl,
};
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tg_video::{FfmpegStreamDecoder, PlaybackOptions, RangeLoader, SliceConfig, StreamingPlayer};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::convert::{
    avatar_rgb_for_color_index, initials_from_name, known_peer_ids, now_unix_i32,
    peer_to_story_profile, ui_text_to_tl_entities,
};
use crate::media::{
    TelegramRangeLoader, block_on_runtime, document_to_location, download_file_bytes,
    photo_to_location, with_rpc_timeout,
};
use crate::session::LiveSession;
use crate::upload::{UploadedFileResult, invoke_with_upload_retry, upload_file_bytes_async};
pub use tg_app_core::stories::backend::{
    DEFAULT_STORY_PERIOD_SECS, EditStoryRequest, SendStoryRequest, StoryMediaAreasInput,
    StoryPrivacyInput, StoryUploadMedia,
};
use tg_app_core::stories::backend::{
    PeerProfile as StoryPeerProfile, ShareTarget, StoriesBackend, StoryMediaAsset,
};

/// Default timeout for synchronous stories RPCs triggered from UI trait methods.
const STORIES_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum video story bytes read into memory when adapting a [`TelegramRangeLoader`]
/// into [`StoryMediaAsset::VideoBytes`] (64 MiB safety cap).
const MAX_VIDEO_STORY_BYTES: u64 = 64 * 1024 * 1024;

struct PendingStoryUpload {
    id: u64,
    status: StoryUploadStatus,
    cancel: CancelToken,
}

/// Per-session mutable stories state kept synchronized across all clones of [`LiveSession`].
struct LiveStoriesState {
    account_premium_override: Option<bool>,
    blocked_viewers: HashSet<DataPeerId>,
    close_friends: HashSet<DataPeerId>,
    story_media: HashMap<FullStoryId, StoryMedia>,
    media_cache: HashMap<FullStoryId, StoryMediaAsset>,
    pending_uploads: Vec<PendingStoryUpload>,
    queued_updates: VecDeque<tl_enums::Update>,
    next_random_id: u64,
    next_upload_id: u64,
}

impl LiveStoriesState {
    fn new() -> Self {
        Self {
            account_premium_override: None,
            blocked_viewers: HashSet::new(),
            close_friends: HashSet::new(),
            story_media: HashMap::new(),
            media_cache: HashMap::new(),
            pending_uploads: Vec::new(),
            queued_updates: VecDeque::new(),
            next_random_id: 1,
            next_upload_id: 1,
        }
    }

    fn next_random_id(&mut self) -> i64 {
        let now = now_unix_i32() as u64;
        self.next_random_id = self.next_random_id.wrapping_add(1);
        ((now << 24) ^ self.next_random_id ^ 0x5707_1E50_ABCD) as i64
    }

    fn next_upload_id(&mut self) -> u64 {
        let id = self.next_upload_id;
        self.next_upload_id = self.next_upload_id.wrapping_add(1);
        id
    }
}

struct RegistryEntry {
    session_weak: Weak<RwLock<Session>>,
    state: Arc<Mutex<LiveStoriesState>>,
}

fn stories_registry() -> &'static Mutex<HashMap<usize, RegistryEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, RegistryEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn stories_state_for(live: &LiveSession) -> Arc<Mutex<LiveStoriesState>> {
    let session_arc = live.runtime().session();
    let key = Arc::as_ptr(session_arc) as usize;
    let session_weak = Arc::downgrade(session_arc);

    let mut map = stories_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.retain(|_, entry| entry.session_weak.strong_count() > 0);
    if let Some(entry) = map.get(&key)
        && entry.session_weak.ptr_eq(&session_weak)
    {
        Arc::clone(&entry.state)
    } else {
        let created = Arc::new(Mutex::new(LiveStoriesState::new()));
        map.insert(
            key,
            RegistryEntry {
                session_weak,
                state: Arc::clone(&created),
            },
        );
        created
    }
}

fn is_story_update(update: &tl_enums::Update) -> bool {
    matches!(
        update,
        tl_enums::Update::Story(_)
            | tl_enums::Update::ReadStories(_)
            | tl_enums::Update::StoriesStealthMode(_)
            | tl_enums::Update::StoryID(_)
    )
}

fn extract_story_updates_from_tl(
    updates: &tl_enums::Updates,
) -> (
    Vec<tl_enums::User>,
    Vec<tl_enums::Chat>,
    Vec<tl_enums::Update>,
) {
    match updates {
        tl_enums::Updates::UpdateShort(short) => {
            if is_story_update(&short.update) {
                (Vec::new(), Vec::new(), vec![short.update.clone()])
            } else {
                (Vec::new(), Vec::new(), Vec::new())
            }
        }
        tl_enums::Updates::Updates(u) => {
            let story_updates: Vec<tl_enums::Update> = u
                .updates
                .iter()
                .filter(|upd| is_story_update(upd))
                .cloned()
                .collect();
            if story_updates.is_empty() {
                (Vec::new(), Vec::new(), Vec::new())
            } else {
                (u.users.clone(), u.chats.clone(), story_updates)
            }
        }
        tl_enums::Updates::Combined(c) => {
            let story_updates: Vec<tl_enums::Update> = c
                .updates
                .iter()
                .filter(|upd| is_story_update(upd))
                .cloned()
                .collect();
            if story_updates.is_empty() {
                (Vec::new(), Vec::new(), Vec::new())
            } else {
                (c.users.clone(), c.chats.clone(), story_updates)
            }
        }
        _ => (Vec::new(), Vec::new(), Vec::new()),
    }
}

impl LiveSession {
    /// Ensures per-session stories state is initialized.
    ///
    /// Live and `updates.getDifference` story updates are forwarded as
    /// [`tg_session::SessionEvent::StoryUpdate`] by `LiveRuntimeBridge` (started in
    /// [`LiveSession::new`]), avoiding duplicate raw `Updates` subscriptions.
    pub fn attach_stories_updates_listener(&self) {
        let _ = stories_state_for(self);
    }

    /// Bootstraps a [`StoriesStore`] with peer metadata from `tg_data::Session`, requests
    /// initial read-tills and main/hidden story strips, and executes the resulting RPC actions.
    pub fn bootstrap_stories_store(&mut self, store: &mut StoriesStore, now_secs: TimeId) {
        self.attach_stories_updates_listener();
        self.sync_known_peers_to_store(store);
        store.request_read_tills();
        store.load_more(StorySourcesList::NotHidden);
        store.load_more(StorySourcesList::Hidden);
        let actions = store.take_actions();
        self.execute_actions(store, actions, now_secs);
    }

    /// Drains any queued `updateStory` / `updateReadStories` / `updateStoriesStealthMode` /
    /// `updateStoryID` updates delivered via [`tg_session::SessionEvent::StoryUpdate`] (both
    /// live updates and `updates.getDifference` / `updates.getChannelDifference`, matching
    /// `Updates::applyUpdate` in `api/api_updates.cpp:2947-2958`) plus any local RPC updates
    /// queued by `send_story` / `edit_story`, and applies them to `store`.
    ///
    /// Returns the number of story updates applied.
    pub fn drain_story_updates(&self, store: &mut StoriesStore, now_secs: TimeId) -> usize {
        let mut pending = self.bridge_state().drain_story_updates();
        let state = stories_state_for(self);
        if let Ok(mut g) = state.lock() {
            pending.extend(g.queued_updates.drain(..));
        }
        if pending.is_empty() {
            return 0;
        }
        self.sync_known_peers_to_store(store);
        let mut applied = 0usize;
        for upd in &pending {
            if store.apply_tl_update(upd, now_secs) {
                applied += 1;
                if let tl_enums::Update::Story(u) = upd {
                    let peer_id = tg_stories::peer_id_from_tl(&u.peer);
                    let sid = match &u.story {
                        tl_enums::StoryItem::StoryItem(s) => s.id,
                        tl_enums::StoryItem::Skipped(s) => s.id,
                        tl_enums::StoryItem::Deleted(d) => d.id,
                    };
                    self.index_story_from_store(store, FullStoryId::new(peer_id, sid));
                }
            }
        }
        applied
    }

    /// Registers a story's media metadata so [`StoriesBackend::load_story_media`] and
    /// [`Self::open_story_video_range_loader`] can download or stream it on demand.
    pub fn register_story_media(&self, story: &Story) {
        let state = stories_state_for(self);
        if let Ok(mut guard) = state.lock() {
            guard
                .story_media
                .insert(story.full_id(), story.media.clone());
        }
    }

    /// Opens a progressive [`TelegramRangeLoader`] for a video story backed by `upload.getFile`.
    #[must_use]
    pub fn open_story_video_range_loader(
        &self,
        story_id: FullStoryId,
    ) -> Option<TelegramRangeLoader<tg_app_core::core_types::erased_rpc::ErasedRpcClient>> {
        let state = stories_state_for(self);
        let media = state.lock().ok()?.story_media.get(&story_id).cloned()?;
        let StoryMedia::Video { document, .. } = media else {
            return None;
        };
        let resolved = document_to_location(&document);
        Some(self.story_range_loader(resolved))
    }

    /// Builds a story video loader that streams into the account's encrypted `streamed` cache
    /// directory. Before an account folder exists the loader encrypts with an ephemeral key.
    fn story_range_loader(
        &self,
        resolved: crate::media::ResolvedMediaLocation,
    ) -> TelegramRangeLoader<tg_app_core::core_types::erased_rpc::ErasedRpcClient> {
        let loader = TelegramRangeLoader::new(
            self.runtime().rpc().clone(),
            self.runtime().tokio_handle().clone(),
            resolved,
        );
        match self.media_cache().streamed_dir() {
            Some(dir) => loader.with_storage_dir(dir),
            None => loader,
        }
    }

    /// Opens a streaming [`tg_video::StreamingPlayer`] over [`TelegramRangeLoader`] for a video story.
    pub fn open_story_video_player(
        &self,
        story_id: FullStoryId,
        options: PlaybackOptions,
    ) -> Result<StreamingPlayer<FfmpegStreamDecoder>, String> {
        let loader = self
            .open_story_video_range_loader(story_id)
            .ok_or_else(|| format!("Video story {} media is not loaded", story_id.story))?;
        let decoder = FfmpegStreamDecoder::from_range_loader(
            Arc::new(loader),
            None,
            SliceConfig::default(),
            options.clone(),
        )
        .map_err(|e| format!("tg-video streaming decoder open failed: {e}"))?;
        StreamingPlayer::new(decoder, options)
            .map_err(|e| format!("tg-video streaming player init failed: {e}"))
    }

    /// Uploads story media (photo or video + optional thumbnail) and publishes it via
    /// `stories.sendStory`, reporting real progress through [`StoriesBackend::pending_uploads`].
    ///
    /// Port of `Api::Stories::send` / `uploaded` / `sendRequest` (`api/api_stories.cpp`).
    pub fn send_story(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: SendStoryRequest,
        now_secs: TimeId,
    ) -> Result<StoryId, String> {
        self.send_story_with_cancel(store, request, now_secs, CancelToken::new())
    }

    /// Same as [`Self::send_story`], accepting a caller-controlled [`CancelToken`].
    pub fn send_story_with_cancel(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: SendStoryRequest,
        now_secs: TimeId,
        cancel: CancelToken,
    ) -> Result<StoryId, String> {
        let input_peer = self
            .resolve_story_input_peer(request.peer_id)
            .ok_or_else(|| format!("Unknown story peer {}", request.peer_id.raw()))?;
        let peer_name = self.peer_profile(request.peer_id).short_name;

        let (entities, privacy_rules, media_areas) = self.read_session_stories(|s| {
            let entities = ui_text_to_tl_entities(&request.caption, s);
            let rules = story_privacy_to_tl(s, &request.privacy);
            let areas = story_media_areas_to_tl(s, &request);
            (entities, rules, areas)
        });

        let state = stories_state_for(self);
        let (upload_id, random_id) = {
            let mut guard = state
                .lock()
                .map_err(|_| "live stories state lock poisoned".to_owned())?;
            let uid = guard.next_upload_id();
            let rid = guard.next_random_id();
            guard.pending_uploads.push(PendingStoryUpload {
                id: uid,
                status: StoryUploadStatus {
                    peer_id: request.peer_id,
                    peer_name,
                    progress: 0.0,
                    failed: false,
                },
                cancel: cancel.clone(),
            });
            (uid, rid)
        };
        self.bridge_state().notify_ui();

        let total_bytes = request.media.total_bytes().max(1);
        let base_cfg = self.upload_config();
        let rpc = self.runtime().rpc().clone();
        let bridge = Arc::clone(self.bridge_state());
        let state_for_progress = Arc::clone(&state);

        let caption_text = request.caption.text.clone();
        let pinned = request.pinned;
        let noforwards = request.noforwards;
        let period = Some(request.period.unwrap_or(DEFAULT_STORY_PERIOD_SECS));
        let albums = (!request.albums.is_empty()).then_some(request.albums);
        let media_payload = request.media;

        let send_res: Result<tl_enums::Updates, String> =
            block_on_runtime(self.runtime().tokio_handle(), async move {
                let mut uploaded_so_far = 0u64;
                let mut update_progress = |delta: u64| {
                    uploaded_so_far = uploaded_so_far.saturating_add(delta).min(total_bytes);
                    let frac = (uploaded_so_far as f64 / total_bytes as f64).clamp(0.0, 1.0);
                    if let Ok(mut guard) = state_for_progress.lock()
                        && let Some(entry) =
                            guard.pending_uploads.iter_mut().find(|u| u.id == upload_id)
                    {
                        entry.status.progress = frac;
                    }
                    bridge.notify_ui();
                };

                let mut uploaded_files: Vec<UploadedFileResult> = Vec::new();
                let input_media = match media_payload {
                    StoryUploadMedia::Photo { bytes, file_name } => {
                        let mut cfg = base_cfg;
                        cfg.is_photo = true;
                        let mut prev = 0u64;
                        let res = upload_file_bytes_async(
                            &rpc,
                            if file_name.is_empty() {
                                "story.jpg".to_owned()
                            } else {
                                file_name
                            },
                            Arc::new(bytes),
                            cfg,
                            &cancel,
                            |p| {
                                if p.uploaded > prev {
                                    let d = p.uploaded - prev;
                                    prev = p.uploaded;
                                    update_progress(d);
                                }
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                        let file = res.input_file.clone();
                        uploaded_files.push(res);
                        tl_enums::InputMedia::UploadedPhoto(tl_types::InputMediaUploadedPhoto {
                            spoiler: false,
                            live_photo: false,
                            file,
                            stickers: None,
                            ttl_seconds: None,
                            video: None,
                        })
                    }
                    StoryUploadMedia::Video {
                        bytes,
                        file_name,
                        width,
                        height,
                        duration_secs,
                        has_audio,
                        thumb_bytes,
                    } => {
                        let thumb = if let Some(tb) = thumb_bytes
                            && !tb.is_empty()
                        {
                            let mut tcfg = base_cfg;
                            tcfg.is_photo = true;
                            tcfg.explicit_file_id = None;
                            let mut prev = 0u64;
                            let res = upload_file_bytes_async(
                                &rpc,
                                "story_thumb.jpg",
                                Arc::new(tb),
                                tcfg,
                                &cancel,
                                |p| {
                                    if p.uploaded > prev {
                                        let d = p.uploaded - prev;
                                        prev = p.uploaded;
                                        update_progress(d);
                                    }
                                },
                            )
                            .await
                            .map_err(|e| e.to_string())?;
                            let f = res.input_file.clone();
                            uploaded_files.push(res);
                            Some(f)
                        } else {
                            None
                        };
                        let effective_name = if file_name.is_empty() {
                            "story.mp4".to_owned()
                        } else {
                            file_name
                        };
                        let mut vcfg = base_cfg;
                        vcfg.is_photo = false;
                        let mut prev = 0u64;
                        let res = upload_file_bytes_async(
                            &rpc,
                            effective_name.clone(),
                            Arc::new(bytes),
                            vcfg,
                            &cancel,
                            |p| {
                                if p.uploaded > prev {
                                    let d = p.uploaded - prev;
                                    prev = p.uploaded;
                                    update_progress(d);
                                }
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                        let file = res.input_file.clone();
                        uploaded_files.push(res);
                        let attributes = vec![
                            tl_enums::DocumentAttribute::Filename(
                                tl_types::DocumentAttributeFilename {
                                    file_name: effective_name,
                                },
                            ),
                            tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
                                round_message: false,
                                supports_streaming: true,
                                nosound: !has_audio,
                                duration: duration_secs.max(0.0),
                                w: width.max(1),
                                h: height.max(1),
                                preload_prefix_size: None,
                                video_start_ts: None,
                                video_codec: None,
                            }),
                        ];
                        tl_enums::InputMedia::UploadedDocument(
                            tl_types::InputMediaUploadedDocument {
                                nosound_video: !has_audio,
                                force_file: false,
                                spoiler: false,
                                file,
                                thumb,
                                mime_type: "video/mp4".to_owned(),
                                attributes,
                                stickers: None,
                                video_cover: None,
                                video_timestamp: None,
                                ttl_seconds: None,
                            },
                        )
                    }
                };

                if let Ok(mut guard) = state_for_progress.lock()
                    && let Some(entry) =
                        guard.pending_uploads.iter_mut().find(|u| u.id == upload_id)
                {
                    entry.status.progress = 1.0;
                }
                bridge.notify_ui();

                let req = tl_fns::stories::SendStory {
                    pinned,
                    noforwards,
                    fwd_modified: false,
                    peer: input_peer,
                    media: input_media,
                    media_areas: (!media_areas.is_empty()).then_some(media_areas),
                    caption: (!caption_text.is_empty()).then_some(caption_text),
                    entities: (!entities.is_empty()).then_some(entities),
                    privacy_rules,
                    random_id,
                    period,
                    fwd_from_id: None,
                    fwd_from_story: None,
                    albums,
                    music: None,
                };
                let refs: Vec<&UploadedFileResult> = uploaded_files.iter().collect();
                invoke_with_upload_retry(&rpc, &req, &refs, base_cfg, &cancel).await
            });

        match send_res {
            Ok(updates) => {
                if let Ok(mut guard) = state.lock() {
                    guard.pending_uploads.retain(|u| u.id != upload_id);
                }
                let story_id = story_id_from_updates(&updates);
                if let Some(store) = store {
                    self.apply_tl_updates_to_store(store, &updates, now_secs);
                } else {
                    let (users, chats, story_updates) = extract_story_updates_from_tl(&updates);
                    if !users.is_empty() || !chats.is_empty() {
                        self.write_session_stories(|s| {
                            apply_tl_users_and_chats(s, &users, &chats);
                        });
                    }
                    if !story_updates.is_empty()
                        && let Ok(mut guard) = state.lock()
                    {
                        for upd in story_updates {
                            guard.queued_updates.push_back(upd);
                        }
                    }
                }
                self.bridge_state().notify_ui();
                Ok(story_id)
            }
            Err(err) => {
                if let Ok(mut guard) = state.lock()
                    && let Some(entry) =
                        guard.pending_uploads.iter_mut().find(|u| u.id == upload_id)
                {
                    entry.status.failed = true;
                    entry.status.progress = 0.0;
                }
                self.bridge_state().notify_ui();
                Err(err)
            }
        }
    }

    /// Cancels all active story uploads targeting `peer_id` (returning how many were cancelled).
    pub fn cancel_story_uploads(&self, peer_id: DataPeerId) -> usize {
        let state = stories_state_for(self);
        let mut count = 0usize;
        if let Ok(mut guard) = state.lock() {
            for entry in &mut guard.pending_uploads {
                if entry.status.peer_id == peer_id {
                    entry.cancel.cancel();
                    entry.status.failed = true;
                    count += 1;
                }
            }
        }
        if count > 0 {
            self.bridge_state().notify_ui();
        }
        count
    }

    /// Removes failed story upload indicators for `peer_id` from [`StoriesBackend::pending_uploads`].
    pub fn dismiss_failed_story_uploads(&self, peer_id: DataPeerId) {
        let state = stories_state_for(self);
        if let Ok(mut guard) = state.lock() {
            guard
                .pending_uploads
                .retain(|u| !(u.status.peer_id == peer_id && u.status.failed));
        }
        self.bridge_state().notify_ui();
    }

    /// Edits an existing story's media, media areas, caption/entities, or privacy rules via
    /// `stories.editStory` (`Api::Stories::sendRequest` with `editId != 0` in `api_stories.cpp`).
    pub fn edit_story(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: EditStoryRequest,
        now_secs: TimeId,
    ) -> Result<(), String> {
        self.edit_story_with_cancel(store, request, now_secs, CancelToken::new())
    }

    /// Same as [`Self::edit_story`], accepting a caller-controlled [`CancelToken`].
    pub fn edit_story_with_cancel(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: EditStoryRequest,
        now_secs: TimeId,
        cancel: CancelToken,
    ) -> Result<(), String> {
        let input_peer = self
            .resolve_story_input_peer(request.peer_id)
            .ok_or_else(|| format!("Unknown story peer {}", request.peer_id.raw()))?;
        let peer_name = self.peer_profile(request.peer_id).short_name;

        let (caption_and_entities, privacy_rules, media_areas) = self.read_session_stories(|s| {
            // In TL Layer 233 `stories.editStory`, `caption` and `entities` share `flags.1`,
            // so both must be `Some` together when editing caption, or both `None` when not.
            let cap_ent = request.caption.as_ref().map(|cap| {
                let entities = ui_text_to_tl_entities(cap, s);
                (cap.text.clone(), entities)
            });
            let rules = request.privacy.as_ref().map(|p| story_privacy_to_tl(s, p));
            let areas = request
                .media_areas
                .as_ref()
                .map(|a| story_media_areas_input_to_tl(s, a));
            (cap_ent, rules, areas)
        });

        let (tl_caption, tl_entities) = match caption_and_entities {
            Some((text, entities)) => (Some(text), Some(entities)),
            None => (None, None),
        };

        let state = stories_state_for(self);
        let upload_id = if request.media.is_some() {
            let mut guard = state
                .lock()
                .map_err(|_| "live stories state lock poisoned".to_owned())?;
            let uid = guard.next_upload_id();
            guard.pending_uploads.push(PendingStoryUpload {
                id: uid,
                status: StoryUploadStatus {
                    peer_id: request.peer_id,
                    peer_name,
                    progress: 0.0,
                    failed: false,
                },
                cancel: cancel.clone(),
            });
            self.bridge_state().notify_ui();
            Some(uid)
        } else {
            None
        };

        let edit_res: Result<tl_enums::Updates, String> = if let Some(media_payload) = request.media
        {
            let total_bytes = media_payload.total_bytes().max(1);
            let base_cfg = self.upload_config();
            let rpc = self.runtime().rpc().clone();
            let bridge = Arc::clone(self.bridge_state());
            let state_for_progress = Arc::clone(&state);
            let uid = upload_id.unwrap_or(0);
            let story_id = request.story_id;

            block_on_runtime(self.runtime().tokio_handle(), async move {
                let mut uploaded_so_far = 0u64;
                let mut update_progress = |delta: u64| {
                    uploaded_so_far = uploaded_so_far.saturating_add(delta).min(total_bytes);
                    let frac = (uploaded_so_far as f64 / total_bytes as f64).clamp(0.0, 1.0);
                    if let Ok(mut guard) = state_for_progress.lock()
                        && let Some(entry) = guard.pending_uploads.iter_mut().find(|u| u.id == uid)
                    {
                        entry.status.progress = frac;
                    }
                    bridge.notify_ui();
                };

                let mut uploaded_files: Vec<UploadedFileResult> = Vec::new();
                let input_media = match media_payload {
                    StoryUploadMedia::Photo { bytes, file_name } => {
                        let mut cfg = base_cfg;
                        cfg.is_photo = true;
                        let mut prev = 0u64;
                        let res = upload_file_bytes_async(
                            &rpc,
                            if file_name.is_empty() {
                                "story.jpg".to_owned()
                            } else {
                                file_name
                            },
                            Arc::new(bytes),
                            cfg,
                            &cancel,
                            |p| {
                                if p.uploaded > prev {
                                    let d = p.uploaded - prev;
                                    prev = p.uploaded;
                                    update_progress(d);
                                }
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                        let file = res.input_file.clone();
                        uploaded_files.push(res);
                        tl_enums::InputMedia::UploadedPhoto(tl_types::InputMediaUploadedPhoto {
                            spoiler: false,
                            live_photo: false,
                            file,
                            stickers: None,
                            ttl_seconds: None,
                            video: None,
                        })
                    }
                    StoryUploadMedia::Video {
                        bytes,
                        file_name,
                        width,
                        height,
                        duration_secs,
                        has_audio,
                        thumb_bytes,
                    } => {
                        let thumb = if let Some(tb) = thumb_bytes
                            && !tb.is_empty()
                        {
                            let mut tcfg = base_cfg;
                            tcfg.is_photo = true;
                            tcfg.explicit_file_id = None;
                            let mut prev = 0u64;
                            let res = upload_file_bytes_async(
                                &rpc,
                                "story_thumb.jpg",
                                Arc::new(tb),
                                tcfg,
                                &cancel,
                                |p| {
                                    if p.uploaded > prev {
                                        let d = p.uploaded - prev;
                                        prev = p.uploaded;
                                        update_progress(d);
                                    }
                                },
                            )
                            .await
                            .map_err(|e| e.to_string())?;
                            let f = res.input_file.clone();
                            uploaded_files.push(res);
                            Some(f)
                        } else {
                            None
                        };
                        let effective_name = if file_name.is_empty() {
                            "story.mp4".to_owned()
                        } else {
                            file_name
                        };
                        let mut vcfg = base_cfg;
                        vcfg.is_photo = false;
                        let mut prev = 0u64;
                        let res = upload_file_bytes_async(
                            &rpc,
                            effective_name.clone(),
                            Arc::new(bytes),
                            vcfg,
                            &cancel,
                            |p| {
                                if p.uploaded > prev {
                                    let d = p.uploaded - prev;
                                    prev = p.uploaded;
                                    update_progress(d);
                                }
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                        let file = res.input_file.clone();
                        uploaded_files.push(res);
                        let attributes = vec![
                            tl_enums::DocumentAttribute::Filename(
                                tl_types::DocumentAttributeFilename {
                                    file_name: effective_name,
                                },
                            ),
                            tl_enums::DocumentAttribute::Video(tl_types::DocumentAttributeVideo {
                                round_message: false,
                                supports_streaming: true,
                                nosound: !has_audio,
                                duration: duration_secs.max(0.0),
                                w: width.max(1),
                                h: height.max(1),
                                preload_prefix_size: None,
                                video_start_ts: None,
                                video_codec: None,
                            }),
                        ];
                        tl_enums::InputMedia::UploadedDocument(
                            tl_types::InputMediaUploadedDocument {
                                nosound_video: !has_audio,
                                force_file: false,
                                spoiler: false,
                                file,
                                thumb,
                                mime_type: "video/mp4".to_owned(),
                                attributes,
                                stickers: None,
                                video_cover: None,
                                video_timestamp: None,
                                ttl_seconds: None,
                            },
                        )
                    }
                };

                if let Ok(mut guard) = state_for_progress.lock()
                    && let Some(entry) = guard.pending_uploads.iter_mut().find(|u| u.id == uid)
                {
                    entry.status.progress = 1.0;
                }
                bridge.notify_ui();

                let req = tl_fns::stories::EditStory {
                    peer: input_peer,
                    id: story_id,
                    media: Some(input_media),
                    media_areas,
                    caption: tl_caption,
                    entities: tl_entities,
                    privacy_rules,
                    music: None,
                };
                let refs: Vec<&UploadedFileResult> = uploaded_files.iter().collect();
                invoke_with_upload_retry(&rpc, &req, &refs, base_cfg, &cancel).await
            })
        } else {
            let req = tl_fns::stories::EditStory {
                peer: input_peer,
                id: request.story_id,
                media: None,
                media_areas,
                caption: tl_caption,
                entities: tl_entities,
                privacy_rules,
                music: None,
            };
            self.invoke_stories_rpc(req)
        };

        match edit_res {
            Ok(updates) => {
                if let Some(uid) = upload_id
                    && let Ok(mut guard) = state.lock()
                {
                    guard.pending_uploads.retain(|u| u.id != uid);
                }
                if let Some(store) = store {
                    self.apply_tl_updates_to_store(store, &updates, now_secs);
                } else {
                    let (users, chats, story_updates) = extract_story_updates_from_tl(&updates);
                    if !users.is_empty() || !chats.is_empty() {
                        self.write_session_stories(|s| {
                            apply_tl_users_and_chats(s, &users, &chats);
                        });
                    }
                    if !story_updates.is_empty()
                        && let Ok(mut guard) = state.lock()
                    {
                        for upd in story_updates {
                            guard.queued_updates.push_back(upd);
                        }
                    }
                }
                self.bridge_state().notify_ui();
                Ok(())
            }
            Err(err) => {
                if let Some(uid) = upload_id
                    && let Ok(mut guard) = state.lock()
                    && let Some(entry) = guard.pending_uploads.iter_mut().find(|u| u.id == uid)
                {
                    entry.status.failed = true;
                    entry.status.progress = 0.0;
                }
                self.bridge_state().notify_ui();
                Err(err)
            }
        }
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    fn read_session_stories<R: Send>(&self, f: impl FnOnce(&Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(guard) = session_lock.try_read() {
            return f(&guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let guard = session_arc.read().await;
            f(&guard)
        })
    }

    fn write_session_stories<R: Send>(&self, f: impl FnOnce(&mut Session) -> R + Send) -> R {
        let session_lock = self.runtime().session();
        if let Ok(mut guard) = session_lock.try_write() {
            return f(&mut guard);
        }
        let session_arc = Arc::clone(session_lock);
        block_on_runtime(self.runtime().tokio_handle(), async move {
            let mut guard = session_arc.write().await;
            f(&mut guard)
        })
    }

    fn invoke_stories_rpc<R>(&self, req: R) -> Result<R::Return, String>
    where
        R: tg_tl::RemoteCall + Send + Sync + 'static,
        R::Return: Send + 'static,
    {
        let rpc = self.runtime().rpc().clone();
        block_on_runtime(self.runtime().tokio_handle(), async move {
            with_rpc_timeout(STORIES_RPC_TIMEOUT, async move { rpc.invoke(&req).await }).await
        })
    }

    fn resolve_story_input_peer(&self, peer_id: DataPeerId) -> Option<tl_enums::InputPeer> {
        let peer = self.read_session_stories(|s| input_peer_for(s, peer_id));
        if matches!(peer, tl_enums::InputPeer::Empty) {
            None
        } else {
            Some(peer)
        }
    }

    #[doc(hidden)]
    pub fn apply_users_chats_and_sync_store(
        &self,
        store: &mut StoriesStore,
        users: &[tl_enums::User],
        chats: &[tl_enums::Chat],
        force_hidden: bool,
    ) {
        if users.is_empty() && chats.is_empty() {
            return;
        }
        let mut peer_ids = Vec::with_capacity(users.len() + chats.len());
        let mut tl_hidden = HashMap::new();
        for u in users {
            match u {
                tl_enums::User::User(user) => {
                    let pid = DataPeerId::from_user(tg_data::UserId::from_tl(user.id));
                    peer_ids.push(pid);
                    tl_hidden.insert(pid, user.stories_hidden || force_hidden);
                }
                tl_enums::User::Empty(e) => {
                    peer_ids.push(DataPeerId::from_user(tg_data::UserId::from_tl(e.id)));
                }
            }
        }
        for c in chats {
            match c {
                tl_enums::Chat::Channel(ch) => {
                    let pid = DataPeerId::from_channel(tg_data::ChannelId::from_tl(ch.id));
                    peer_ids.push(pid);
                    tl_hidden.insert(pid, ch.stories_hidden || force_hidden);
                }
                tl_enums::Chat::ChannelForbidden(ch) => {
                    peer_ids.push(DataPeerId::from_channel(tg_data::ChannelId::from_tl(ch.id)));
                }
                tl_enums::Chat::Chat(ch) => {
                    peer_ids.push(DataPeerId::from_chat(tg_data::ChatId::from_tl(ch.id)));
                }
                tl_enums::Chat::Forbidden(ch) => {
                    peer_ids.push(DataPeerId::from_chat(tg_data::ChatId::from_tl(ch.id)));
                }
                tl_enums::Chat::Empty(e) => {
                    peer_ids.push(DataPeerId::from_chat(tg_data::ChatId::from_tl(e.id)));
                }
                tl_enums::Chat::Community(_) | tl_enums::Chat::CommunityForbidden(_) => {}
            }
        }

        let metas: Vec<(DataPeerId, PeerStoryMeta)> = self.write_session_stories(|s| {
            apply_tl_users_and_chats(s, users, chats);
            let mut out = Vec::with_capacity(peer_ids.len());
            for pid in peer_ids {
                if let Some(peer) = s.peer_mut(pid) {
                    if let Some(&hidden) = tl_hidden.get(&pid) {
                        match peer {
                            Peer::User(u) => u.flags.set(UserFlags::STORIES_HIDDEN, hidden),
                            Peer::Channel(ch) => {
                                ch.flags.set(ChannelFlags::STORIES_HIDDEN, hidden);
                            }
                            Peer::Chat(_) => {}
                        }
                    }
                    out.push((pid, PeerStoryMeta::from_peer(peer)));
                }
            }
            out
        });

        for (pid, meta) in metas {
            store.set_peer_meta(pid, meta);
        }
    }

    fn sync_known_peers_to_store(&self, store: &mut StoriesStore) {
        let metas: Vec<(DataPeerId, PeerStoryMeta)> = self.read_session_stories(|s| {
            known_peer_ids(s)
                .into_iter()
                .filter_map(|pid| s.peer(pid).map(|p| (pid, PeerStoryMeta::from_peer(p))))
                .collect()
        });
        for (pid, meta) in metas {
            store.set_peer_meta(pid, meta);
        }
    }

    fn apply_tl_updates_to_store(
        &self,
        store: &mut StoriesStore,
        updates: &tl_enums::Updates,
        now_secs: TimeId,
    ) {
        let (users, chats, story_updates) = extract_story_updates_from_tl(updates);
        self.apply_users_chats_and_sync_store(store, &users, &chats, false);
        for upd in &story_updates {
            store.apply_tl_update(upd, now_secs);
            if let tl_enums::Update::Story(u) = upd {
                let peer_id = tg_stories::peer_id_from_tl(&u.peer);
                let sid = match &u.story {
                    tl_enums::StoryItem::StoryItem(s) => s.id,
                    tl_enums::StoryItem::Skipped(s) => s.id,
                    tl_enums::StoryItem::Deleted(d) => d.id,
                };
                self.index_story_from_store(store, FullStoryId::new(peer_id, sid));
            }
        }
    }

    #[doc(hidden)]
    pub fn index_story_from_store(&self, store: &StoriesStore, story_id: FullStoryId) {
        if let Ok(story) = store.lookup(story_id) {
            self.register_story_media(story);
        }
    }

    fn index_peer_stories_from_store(&self, store: &StoriesStore, peer_id: DataPeerId) {
        if let Some(src) = store.source(peer_id) {
            let ids: Vec<_> = src.ids.iter().map(|d| d.id).collect();
            for sid in ids {
                self.index_story_from_store(store, FullStoryId::new(peer_id, sid));
            }
        }
    }

    fn index_all_sources_from_store(&self, store: &StoriesStore, list: StorySourcesList) {
        let peer_ids: Vec<DataPeerId> = store.sources(list).iter().map(|s| s.id).collect();
        for pid in peer_ids {
            self.index_peer_stories_from_store(store, pid);
        }
    }
}

impl StoriesBackend for LiveSession {
    fn self_peer_id(&self) -> DataPeerId {
        self.read_session_stories(|s| s.self_peer_id())
    }

    fn account_premium(&self) -> bool {
        let state = stories_state_for(self);
        if let Ok(guard) = state.lock()
            && let Some(override_val) = guard.account_premium_override
        {
            return override_val;
        }
        self.read_session_stories(|s| {
            s.peer(s.self_peer_id())
                .and_then(|p| p.as_user())
                .is_some_and(tg_data::User::is_premium)
        })
    }

    fn set_account_premium(&mut self, premium: bool) {
        let state = stories_state_for(self);
        if let Ok(mut guard) = state.lock() {
            guard.account_premium_override = Some(premium);
        }
    }

    fn supports_mock_premium_toggle(&self) -> bool {
        false
    }

    fn peer_profile(&self, peer_id: DataPeerId) -> StoryPeerProfile {
        self.read_session_stories(|s| {
            let mut profile = peer_to_story_profile(s, peer_id);
            if let Some(peer) = s.peer(peer_id)
                && let Some(ch) = peer.as_channel()
            {
                profile.can_view_channel_reactions = ch.am_creator() || ch.can_edit_stories();
            }
            profile
        })
    }

    fn share_targets(&self) -> Vec<ShareTarget> {
        self.read_session_stories(|s| {
            s.main_dialogs()
                .iter()
                .filter_map(|&k| {
                    let DialogKey::History(pid) = k else {
                        return None;
                    };
                    let peer = s.peer(pid)?;
                    let name = peer.name().to_owned();
                    let short_name = name.split_whitespace().next().unwrap_or(&name).to_owned();
                    Some(ShareTarget {
                        peer_id: pid,
                        initials: initials_from_name(&name),
                        name,
                        short_name,
                        avatar_rgb: avatar_rgb_for_color_index(peer.common().color_index % 7),
                        can_send: !peer.as_channel().is_some_and(|c| c.is_broadcast()),
                    })
                })
                .collect()
        })
    }

    fn execute_actions(
        &mut self,
        store: &mut StoriesStore,
        actions: Vec<StoriesAction>,
        now_secs: TimeId,
    ) {
        // First apply any story updates pushed asynchronously via tg-session's updates stream.
        let _ = self.drain_story_updates(store, now_secs);

        for action in actions {
            match action {
                StoriesAction::GetAllStories { list, state } => {
                    let hidden = list == StorySourcesList::Hidden;
                    let req = build_get_all_stories(hidden, state.as_deref());
                    match self.invoke_stories_rpc(req) {
                        Ok(all) => {
                            if let tl_enums::stories::AllStories::AllStories(ref data) = all {
                                self.apply_users_chats_and_sync_store(
                                    store,
                                    &data.users,
                                    &data.chats,
                                    hidden,
                                );
                            }
                            store.apply_tl_all_stories(list, &all, now_secs);
                            self.index_all_sources_from_store(store, list);
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(?list, error = %err, "stories.getAllStories failed");
                            store.fail_load_more(list);
                        }
                    }
                }
                StoriesAction::GetPeerStories { peer_id } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        store.fail_peer_stories(peer_id);
                        continue;
                    };
                    match self.invoke_stories_rpc(build_get_peer_stories(input_peer)) {
                        Ok(tl_enums::stories::PeerStories::PeerStories(resp)) => {
                            self.apply_users_chats_and_sync_store(
                                store,
                                &resp.users,
                                &resp.chats,
                                false,
                            );
                            store.apply_tl_peer_stories(
                                &resp.stories,
                                ParseSource::DirectRequest,
                                now_secs,
                            );
                            self.index_peer_stories_from_store(store, peer_id);
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(peer = peer_id.raw(), error = %err, "stories.getPeerStories failed");
                            store.fail_peer_stories(peer_id);
                        }
                    }
                }
                StoriesAction::GetStoriesById { peer_id, ids } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        store.apply_resolved_stories(peer_id, Vec::new(), now_secs);
                        continue;
                    };
                    let req = build_get_stories_by_id(input_peer, ids.clone());
                    match self.invoke_stories_rpc(req) {
                        Ok(resp) => {
                            let tl_enums::stories::Stories::Stories(ref inner) = resp;
                            self.apply_users_chats_and_sync_store(
                                store,
                                &inner.users,
                                &inner.chats,
                                false,
                            );
                            let ctx = store.peer_meta(peer_id).ctx;
                            let page = stories_page_from_tl(peer_id, ctx, &resp, now_secs);
                            store.apply_resolved_stories(peer_id, page.items, now_secs);
                            for sid in ids {
                                self.index_story_from_store(store, FullStoryId::new(peer_id, sid));
                            }
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(peer = peer_id.raw(), error = %err, "stories.getStoriesByID failed");
                            store.apply_resolved_stories(peer_id, Vec::new(), now_secs);
                        }
                    }
                }
                StoriesAction::ReadStories { peer_id, max_id } => {
                    if let Some(input_peer) = self.resolve_story_input_peer(peer_id)
                        && let Err(err) =
                            self.invoke_stories_rpc(build_read_stories(input_peer, max_id))
                    {
                        debug!(peer = peer_id.raw(), max_id, error = %err, "stories.readStories failed");
                    }
                    store.finish_mark_as_read(peer_id);
                }
                StoriesAction::IncrementStoryViews { peer_id, ids } => {
                    if let Some(input_peer) = self.resolve_story_input_peer(peer_id)
                        && let Err(err) =
                            self.invoke_stories_rpc(build_increment_story_views(input_peer, ids))
                    {
                        debug!(peer = peer_id.raw(), error = %err, "stories.incrementStoryViews failed");
                    }
                    store.finish_increment_views(peer_id);
                }
                StoriesAction::GetAllReadPeerStories => {
                    match self.invoke_stories_rpc(build_get_all_read_peer_stories()) {
                        Ok(updates) => {
                            self.apply_tl_updates_to_store(store, &updates, now_secs);
                        }
                        Err(err) => {
                            debug!(error = %err, "stories.getAllReadPeerStories failed");
                        }
                    }
                    store.finish_read_tills();
                }
                StoriesAction::GetPeerMaxIds { peer_ids } => {
                    let mut queried_peers = Vec::with_capacity(peer_ids.len());
                    let mut input_peers = Vec::with_capacity(peer_ids.len());
                    for pid in peer_ids {
                        if let Some(ip) = self.resolve_story_input_peer(pid) {
                            queried_peers.push(pid);
                            input_peers.push(ip);
                        }
                    }
                    if input_peers.is_empty() {
                        continue;
                    }
                    match self.invoke_stories_rpc(build_get_peer_max_ids(input_peers)) {
                        Ok(recent_list) => {
                            let pairs: Vec<(DataPeerId, RecentStoryInfo)> = queried_peers
                                .into_iter()
                                .zip(recent_list.iter().map(recent_story_from_tl))
                                .collect();
                            store.apply_peer_max_ids(&pairs);
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            debug!(error = %err, "stories.getPeerMaxIDs failed");
                        }
                    }
                }
                StoriesAction::GetStoryViewsList {
                    peer_id,
                    story_id,
                    offset,
                    limit,
                } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        continue;
                    };
                    let req =
                        build_get_story_views_list(input_peer, story_id, offset.clone(), limit);
                    match self.invoke_stories_rpc(req) {
                        Ok(resp) => {
                            let tl_enums::stories::StoryViewsList::StoryViewsList(ref inner) = resp;
                            self.apply_users_chats_and_sync_store(
                                store,
                                &inner.users,
                                &inner.chats,
                                false,
                            );
                            let parsed = story_views_list_from_tl(&resp, now_secs, |pid| {
                                store.peer_meta(pid).ctx
                            });
                            if let Ok(mut guard) = stories_state_for(self).lock() {
                                for v in &parsed.slice.list {
                                    if v.blocked_my_stories_from {
                                        guard.blocked_viewers.insert(v.peer_id);
                                    } else {
                                        guard.blocked_viewers.remove(&v.peer_id);
                                    }
                                }
                            }
                            for (_, repost_item) in parsed.repost_stories {
                                if let tg_stories::ParsedStoryItem::Full(story) = repost_item {
                                    self.register_story_media(&story);
                                    store.insert_story(*story, now_secs);
                                }
                            }
                            store.apply_views_slice(
                                FullStoryId::new(peer_id, story_id),
                                &offset,
                                parsed.slice,
                            );
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(peer = peer_id.raw(), story_id, error = %err, "stories.getStoryViewsList failed");
                        }
                    }
                }
                StoriesAction::GetStoryReactionsList {
                    peer_id,
                    story_id,
                    offset,
                    limit,
                } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        continue;
                    };
                    let req = build_get_story_reactions_list(
                        input_peer,
                        story_id,
                        (!offset.is_empty()).then(|| offset.clone()),
                        limit,
                    );
                    match self.invoke_stories_rpc(req) {
                        Ok(resp) => {
                            let tl_enums::stories::StoryReactionsList::StoryReactionsList(
                                ref inner,
                            ) = resp;
                            self.apply_users_chats_and_sync_store(
                                store,
                                &inner.users,
                                &inner.chats,
                                false,
                            );
                            let parsed = story_reactions_list_from_tl(&resp, now_secs, |pid| {
                                store.peer_meta(pid).ctx
                            });
                            for (_, repost_item) in parsed.repost_stories {
                                if let tg_stories::ParsedStoryItem::Full(story) = repost_item {
                                    self.register_story_media(&story);
                                    store.insert_story(*story, now_secs);
                                }
                            }
                            store.apply_channel_reactions_slice(
                                FullStoryId::new(peer_id, story_id),
                                &offset,
                                parsed.slice,
                            );
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(peer = peer_id.raw(), story_id, error = %err, "stories.getStoryReactionsList failed");
                        }
                    }
                }
                StoriesAction::GetStoriesViews { peer_id, ids } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        continue;
                    };
                    match self.invoke_stories_rpc(build_get_stories_views(input_peer, ids.clone()))
                    {
                        Ok(tl_enums::stories::StoryViews::StoryViews(resp)) => {
                            self.apply_users_chats_and_sync_store(store, &resp.users, &[], false);
                            let is_channel = store.peer_meta(peer_id).ctx.is_channel;
                            for (sid, tl_views) in ids.into_iter().zip(resp.views.iter()) {
                                let full_id = FullStoryId::new(peer_id, sid);
                                let mine = store
                                    .lookup(full_id)
                                    .ok()
                                    .and_then(|s| s.sent_reaction.clone());
                                let counts =
                                    story_views_counts_from_tl(tl_views, mine.as_ref(), is_channel);
                                store.apply_views_counts(full_id, counts);
                            }
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            debug!(peer = peer_id.raw(), error = %err, "stories.getStoriesViews failed");
                        }
                    }
                }
                StoriesAction::LoadAlbumStories {
                    peer_id,
                    album_id,
                    offset,
                    limit,
                } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        // Match `Stories::albumIdsLoadMore` `.fail` (`data/data_stories.cpp:1882-1891`).
                        store.fail_album_stories(peer_id, album_id);
                        continue;
                    };
                    let res = if album_id == STORIES_ALBUM_ID_SAVED {
                        self.invoke_stories_rpc(build_get_pinned_stories(input_peer, offset, limit))
                    } else if album_id == STORIES_ALBUM_ID_ARCHIVE {
                        self.invoke_stories_rpc(build_get_stories_archive(
                            input_peer, offset, limit,
                        ))
                    } else {
                        self.invoke_stories_rpc(build_get_album_stories(
                            input_peer, album_id, offset, limit,
                        ))
                    };
                    match res {
                        Ok(resp) => {
                            let tl_enums::stories::Stories::Stories(ref inner) = resp;
                            self.apply_users_chats_and_sync_store(
                                store,
                                &inner.users,
                                &inner.chats,
                                false,
                            );
                            store.apply_tl_album_stories(
                                peer_id,
                                album_id,
                                &resp,
                                offset == 0,
                                now_secs,
                            );
                            for sid in store.album_ids(peer_id, album_id).list {
                                self.index_story_from_store(store, FullStoryId::new(peer_id, sid));
                            }
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(peer = peer_id.raw(), album_id, error = %err, "load album stories failed");
                            // Match `Stories::albumIdsLoadMore` `.fail` (`data/data_stories.cpp:1882-1891`):
                            // clear in-flight state, mark loaded, and emit `AlbumIdsChanged`.
                            store.fail_album_stories(peer_id, album_id);
                            self.bridge_state().notify_ui();
                        }
                    }
                }
                StoriesAction::GetAlbums { peer_id, hash } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        // Match `Stories::loadAlbums` `.fail` (`data/data_stories.cpp:1958-1962`).
                        store.fail_albums(peer_id);
                        continue;
                    };
                    match self.invoke_stories_rpc(build_get_albums(input_peer, hash)) {
                        Ok(resp) => {
                            store.apply_tl_albums(peer_id, &resp);
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            debug!(peer = peer_id.raw(), error = %err, "stories.getAlbums failed");
                            // Match `Stories::loadAlbums` `.fail` (`data/data_stories.cpp:1958-1962`):
                            // clear in-flight state and reset hash to 0 so retries succeed.
                            store.fail_albums(peer_id);
                        }
                    }
                }
                StoriesAction::ActivateStealthMode => {
                    match self.invoke_stories_rpc(build_activate_stealth_mode(true, true)) {
                        Ok(updates) => {
                            self.apply_tl_updates_to_store(store, &updates, now_secs);
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(error = %err, "stories.activateStealthMode failed");
                        }
                    }
                }
                StoriesAction::SendReaction { story_id, reaction } => {
                    let Some(input_peer) = self.resolve_story_input_peer(story_id.peer) else {
                        continue;
                    };
                    // Match `Stories::sendReaction` (`data/data_stories.cpp:1117-1130`):
                    // sends `stories.sendReaction` with `MTP_flags(0)` (`add_to_recent: false`).
                    let req =
                        build_send_reaction(input_peer, story_id.story, reaction.as_ref(), false);
                    match self.invoke_stories_rpc(req) {
                        Ok(updates) => {
                            self.apply_tl_updates_to_store(store, &updates, now_secs);
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(
                                peer = story_id.peer.raw(),
                                story_id = story_id.story,
                                error = %err,
                                "stories.sendReaction failed"
                            );
                        }
                    }
                }
                StoriesAction::DeleteStories { peer_id, ids } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        continue;
                    };
                    match self.invoke_stories_rpc(build_delete_stories(input_peer, ids)) {
                        Ok(deleted_ids) => {
                            let state = stories_state_for(self);
                            for sid in deleted_ids {
                                let full_id = FullStoryId::new(peer_id, sid);
                                if let Ok(mut guard) = state.lock() {
                                    guard.story_media.remove(&full_id);
                                    guard.media_cache.remove(&full_id);
                                }
                                store.apply_deleted(peer_id, sid);
                            }
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(peer = peer_id.raw(), error = %err, "stories.deleteStories failed");
                        }
                    }
                }
                StoriesAction::ToggleInProfile {
                    peer_id,
                    ids,
                    in_profile,
                } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        continue;
                    };
                    match self.invoke_stories_rpc(build_toggle_pinned(input_peer, ids, in_profile))
                    {
                        Ok(updated_ids) => {
                            store.apply_toggle_in_profile_done(peer_id, &updated_ids, in_profile);
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(peer = peer_id.raw(), error = %err, "stories.togglePinned failed");
                        }
                    }
                }
                StoriesAction::TogglePinnedToTop { peer_id, ids } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        continue;
                    };
                    match self
                        .invoke_stories_rpc(build_toggle_pinned_to_top(input_peer, ids.clone()))
                    {
                        Ok(true) => {
                            store.set_pinned_to_top(peer_id, ids);
                            self.bridge_state().notify_ui();
                        }
                        Ok(false) => {}
                        Err(err) => {
                            warn!(peer = peer_id.raw(), error = %err, "stories.togglePinnedToTop failed");
                        }
                    }
                }
                StoriesAction::TogglePeerStoriesHidden { peer_id, hidden } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        continue;
                    };
                    match self
                        .invoke_stories_rpc(build_toggle_peer_stories_hidden(input_peer, hidden))
                    {
                        Ok(_) => {
                            self.write_session_stories(|s| {
                                if let Some(peer) = s.peer_mut(peer_id) {
                                    match peer {
                                        Peer::User(u) => {
                                            u.flags.set(UserFlags::STORIES_HIDDEN, hidden);
                                        }
                                        Peer::Channel(ch) => {
                                            ch.flags.set(ChannelFlags::STORIES_HIDDEN, hidden);
                                        }
                                        Peer::Chat(_) => {}
                                    }
                                }
                            });
                            self.bridge_state().notify_ui();
                        }
                        Err(err) => {
                            warn!(peer = peer_id.raw(), error = %err, "stories.togglePeerStoriesHidden failed");
                        }
                    }
                }
                StoriesAction::ReorderAlbumStories {
                    peer_id,
                    album_id,
                    order,
                } => {
                    let Some(input_peer) = self.resolve_story_input_peer(peer_id) else {
                        continue;
                    };
                    if let Err(err) =
                        self.invoke_stories_rpc(build_reorder_album(input_peer, album_id, order))
                    {
                        warn!(peer = peer_id.raw(), album_id, error = %err, "stories.updateAlbum reorder failed");
                    }
                }
                StoriesAction::StartPreload { story_id } => {
                    self.index_story_from_store(store, story_id);
                    let ok = self.load_story_media(story_id).is_some();
                    store.finish_preload(story_id, ok);
                }
                StoriesAction::CancelPreload { .. } => {}
            }
        }
    }

    fn load_story_media(&mut self, story_id: FullStoryId) -> Option<StoryMediaAsset> {
        let state = stories_state_for(self);
        if let Ok(guard) = state.lock()
            && let Some(cached) = guard.media_cache.get(&story_id)
        {
            return Some(cached.clone());
        }

        let media = state.lock().ok()?.story_media.get(&story_id).cloned()?;
        let asset = match media {
            StoryMedia::Photo(photo) => {
                let resolved = photo_to_location(&photo);
                let bytes = download_file_bytes(
                    self.runtime().rpc(),
                    self.runtime().tokio_handle(),
                    resolved.dc_id,
                    resolved.location,
                    (resolved.size > 0).then_some(resolved.size),
                )
                .ok()?;
                StoryMediaAsset::PhotoBytes(bytes)
            }
            StoryMedia::Video { document, .. } => {
                let resolved = document_to_location(&document);
                let total_size = resolved.size;
                let loader = self.story_range_loader(resolved);
                let bytes = read_all_via_range_loader(&loader, total_size)?;
                StoryMediaAsset::VideoBytes(bytes)
            }
            StoryMedia::VideoStream(_) | StoryMedia::Unsupported => return None,
        };

        if let Ok(mut guard) = state.lock() {
            guard.media_cache.insert(story_id, asset.clone());
        }
        Some(asset)
    }

    fn send_story_reply(
        &mut self,
        story_id: FullStoryId,
        text: &str,
        _now_secs: TimeId,
    ) -> Result<(), String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err("Reply text cannot be empty".to_owned());
        }
        let input_peer = self
            .resolve_story_input_peer(story_id.peer)
            .ok_or_else(|| format!("Unknown story peer {}", story_id.peer.raw()))?;
        let random_id = stories_state_for(self)
            .lock()
            .map(|mut g| g.next_random_id())
            .unwrap_or(1);

        let req = tl_fns::messages::SendMessage {
            no_webpage: false,
            silent: false,
            background: false,
            clear_draft: false,
            noforwards: false,
            update_stickersets_order: false,
            invert_media: false,
            allow_paid_floodskip: false,
            peer: input_peer.clone(),
            reply_to: Some(tl_enums::InputReplyTo::Story(tl_types::InputReplyToStory {
                peer: input_peer,
                story_id: story_id.story,
            })),
            message: trimmed.to_owned(),
            random_id,
            reply_markup: None,
            entities: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            allow_paid_stars: None,
            suggested_post: None,
            rich_message: None,
        };
        let updates = self.invoke_stories_rpc(req)?;
        let _ = self.apply_updates_to_session(&updates);
        Ok(())
    }

    fn share_story(
        &mut self,
        story_id: FullStoryId,
        targets: &[DataPeerId],
        comment: &str,
    ) -> Result<(), String> {
        if targets.is_empty() {
            return Err("Select at least one recipient".to_owned());
        }
        let source_peer = self
            .resolve_story_input_peer(story_id.peer)
            .ok_or_else(|| format!("Unknown source story peer {}", story_id.peer.raw()))?;
        let trimmed_comment = comment.trim().to_owned();

        for &target_peer_id in targets {
            let target_peer = self
                .resolve_story_input_peer(target_peer_id)
                .ok_or_else(|| format!("Unknown target peer {}", target_peer_id.raw()))?;
            let random_id = stories_state_for(self)
                .lock()
                .map(|mut g| g.next_random_id())
                .unwrap_or(1);
            let req = tl_fns::messages::SendMedia {
                silent: false,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: target_peer,
                reply_to: None,
                media: tl_enums::InputMedia::Story(tl_types::InputMediaStory {
                    peer: source_peer.clone(),
                    id: story_id.story,
                }),
                message: trimmed_comment.clone(),
                random_id,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            };
            let updates = self.invoke_stories_rpc(req)?;
            let _ = self.apply_updates_to_session(&updates);
        }
        Ok(())
    }

    fn export_story_link(&self, story_id: FullStoryId) -> Option<String> {
        let input_peer = self.resolve_story_input_peer(story_id.peer)?;
        match self.invoke_stories_rpc(build_export_story_link(input_peer, story_id.story)) {
            Ok(tl_enums::ExportedStoryLink::ExportedStoryLink(exported)) => Some(exported.link),
            Err(err) => {
                debug!(peer = story_id.peer.raw(), story_id = story_id.story, error = %err, "stories.exportStoryLink failed");
                let profile = self.peer_profile(story_id.peer);
                if profile.username.is_empty() {
                    None
                } else {
                    Some(format!(
                        "https://t.me/{}/s/{}",
                        profile.username, story_id.story
                    ))
                }
            }
        }
    }

    fn report_story(&mut self, story_id: FullStoryId, reason: &str) -> Result<(), String> {
        let input_peer = self
            .resolve_story_input_peer(story_id.peer)
            .ok_or_else(|| format!("Unknown story peer {}", story_id.peer.raw()))?;
        let req = build_report_story(
            input_peer.clone(),
            vec![story_id.story],
            Vec::new(),
            reason.to_owned(),
        );
        let first_res = self.invoke_stories_rpc(req)?;
        if let tl_enums::ReportResult::ChooseOption(choice) = first_res {
            let reason_lower = reason.to_ascii_lowercase();
            let chosen = choice
                .options
                .iter()
                .find_map(|opt| {
                    let tl_enums::MessageReportOption::MessageReportOption(inner) = opt;
                    if inner.text.to_ascii_lowercase().contains(&reason_lower)
                        || reason_lower.contains(&inner.text.to_ascii_lowercase())
                    {
                        Some(inner.option.clone())
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    choice.options.first().map(|opt| {
                        let tl_enums::MessageReportOption::MessageReportOption(inner) = opt;
                        inner.option.clone()
                    })
                });
            if let Some(option_bytes) = chosen {
                let follow_up = build_report_story(
                    input_peer,
                    vec![story_id.story],
                    option_bytes,
                    reason.to_owned(),
                );
                let _ = self.invoke_stories_rpc(follow_up)?;
            }
        }
        Ok(())
    }

    fn set_viewer_stories_blocked(&mut self, peer_id: DataPeerId, blocked: bool) {
        if let Some(input_peer) = self.resolve_story_input_peer(peer_id) {
            let rpc_res = if blocked {
                self.invoke_stories_rpc(tl_fns::contacts::Block {
                    my_stories_from: true,
                    id: input_peer,
                })
            } else {
                self.invoke_stories_rpc(tl_fns::contacts::Unblock {
                    my_stories_from: true,
                    id: input_peer,
                })
            };
            if let Err(err) = rpc_res {
                warn!(peer = peer_id.raw(), blocked, error = %err, "contacts.block/unblock my_stories_from failed");
                return;
            }
        }
        let state = stories_state_for(self);
        if let Ok(mut guard) = state.lock() {
            if blocked {
                guard.blocked_viewers.insert(peer_id);
            } else {
                guard.blocked_viewers.remove(&peer_id);
            }
        }
    }

    fn is_viewer_stories_blocked(&self, peer_id: DataPeerId) -> bool {
        stories_state_for(self)
            .lock()
            .map(|g| g.blocked_viewers.contains(&peer_id))
            .unwrap_or(false)
    }

    fn pending_uploads(&self) -> Vec<StoryUploadStatus> {
        stories_state_for(self)
            .lock()
            .map(|g| g.pending_uploads.iter().map(|u| u.status.clone()).collect())
            .unwrap_or_default()
    }

    fn send_story(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: SendStoryRequest,
        now_secs: TimeId,
    ) -> Result<StoryId, String> {
        LiveSession::send_story(self, store, request, now_secs)
    }

    fn edit_story(
        &mut self,
        store: Option<&mut StoriesStore>,
        request: EditStoryRequest,
        now_secs: TimeId,
    ) -> Result<(), String> {
        LiveSession::edit_story(self, store, request, now_secs)
    }

    fn dismiss_failed_uploads(&mut self, peer_id: DataPeerId) {
        self.dismiss_failed_story_uploads(peer_id);
    }

    fn close_friends(&self) -> Vec<DataPeerId> {
        let mut set: HashSet<DataPeerId> = stories_state_for(self)
            .lock()
            .map(|g| g.close_friends.clone())
            .unwrap_or_default();
        let from_session: Vec<DataPeerId> = self.read_session_stories(|s| {
            known_peer_ids(s)
                .into_iter()
                .filter(|&pid| {
                    s.peer(pid)
                        .and_then(|p| p.as_user())
                        .is_some_and(|u| u.flags.contains(UserFlags::CLOSE_FRIEND))
                })
                .collect()
        });
        set.extend(from_session);
        let mut out: Vec<DataPeerId> = set.into_iter().collect();
        out.sort_by_key(|p| p.raw());
        out
    }

    fn set_close_friends(&mut self, peers: Vec<DataPeerId>) -> Result<(), String> {
        let ids: Vec<i64> = peers
            .iter()
            .filter_map(|pid| pid.to_user().map(|u| u.0 as i64))
            .collect();
        let _ = self.invoke_stories_rpc(tl_fns::contacts::EditCloseFriends { id: ids })?;
        let selected: HashSet<DataPeerId> = peers.iter().copied().collect();
        if let Ok(mut guard) = stories_state_for(self).lock() {
            guard.close_friends = selected.clone();
        }
        self.write_session_stories(|s| {
            for pid in known_peer_ids(s).into_iter().chain(peers.iter().copied()) {
                if let Some(Peer::User(u)) = s.peer_mut(pid) {
                    u.flags
                        .set(UserFlags::CLOSE_FRIEND, selected.contains(&pid));
                }
            }
        });
        self.bridge_state().notify_ui();
        Ok(())
    }

    fn contacts_for_privacy(&self) -> Vec<StoryPeerProfile> {
        self.read_session_stories(|s| {
            let self_id = s.self_peer_id();
            let mut out: Vec<StoryPeerProfile> = known_peer_ids(s)
                .into_iter()
                .filter(|&pid| {
                    pid != self_id
                        && s.peer(pid)
                            .and_then(|p| p.as_user())
                            .is_some_and(|u| !u.is_bot() && !u.flags.contains(UserFlags::DELETED))
                })
                .map(|pid| peer_to_story_profile(s, pid))
                .collect();
            out.sort_by(|a, b| a.name.cmp(&b.name));
            out
        })
    }

    fn save_stories_blocklist(&mut self, peers: &[DataPeerId]) -> Result<bool, String> {
        crate::contacts_extras::set_blocked_peers(self, peers, true)
    }
}

/// Returns the cached count of peers blocked from viewing our stories (`Stories::_blocklist.size()`).
pub(crate) fn blocked_viewers_count(live: &LiveSession) -> usize {
    stories_state_for(live)
        .lock()
        .map(|g| g.blocked_viewers.len())
        .unwrap_or(0)
}

/// Replaces the cached hidden-stories blocklist (`Stories::saveBlocklist`).
pub(crate) fn sync_blocked_viewers(live: &LiveSession, peers: &[DataPeerId]) {
    if let Ok(mut guard) = stories_state_for(live).lock() {
        guard.blocked_viewers = peers.iter().copied().collect();
    }
}

/// Converts [`StoryPrivacyInput`] into outgoing TL `InputPrivacyRule`s, matching
/// `StoryPrivacyToInput` (`Telegram/SourceFiles/api/api_stories.cpp:252-317`).
#[must_use]
pub fn story_privacy_to_tl(
    session: &Session,
    privacy: &StoryPrivacyInput,
) -> Vec<tl_enums::InputPrivacyRule> {
    let collect_users = |peers: &[DataPeerId]| -> Vec<tl_enums::InputUser> {
        peers
            .iter()
            .filter_map(|pid| pid.to_user())
            .map(|uid| input_user_for(session, uid))
            .filter(|u| !matches!(u, tl_enums::InputUser::Empty))
            .collect()
    };
    let collect_chats = |peers: &[DataPeerId]| -> Vec<i64> {
        peers
            .iter()
            .filter(|pid| pid.to_user().is_none())
            .map(|pid| pid.bare() as i64)
            .collect()
    };

    let mut rules = Vec::new();
    match privacy.privacy {
        StoryPrivacy::Public => {
            rules.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowAll);
            let users = collect_users(&privacy.disallow_peers);
            let chats = collect_chats(&privacy.disallow_peers);
            if !users.is_empty() {
                rules.push(tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(
                    tl_types::InputPrivacyValueDisallowUsers { users },
                ));
            }
            if !chats.is_empty() {
                rules.push(
                    tl_enums::InputPrivacyRule::InputPrivacyValueDisallowChatParticipants(
                        tl_types::InputPrivacyValueDisallowChatParticipants { chats },
                    ),
                );
            }
        }
        StoryPrivacy::Contacts => {
            rules.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowContacts);
            let users = collect_users(&privacy.disallow_peers);
            let chats = collect_chats(&privacy.disallow_peers);
            if !users.is_empty() {
                rules.push(tl_enums::InputPrivacyRule::InputPrivacyValueDisallowUsers(
                    tl_types::InputPrivacyValueDisallowUsers { users },
                ));
            }
            if !chats.is_empty() {
                rules.push(
                    tl_enums::InputPrivacyRule::InputPrivacyValueDisallowChatParticipants(
                        tl_types::InputPrivacyValueDisallowChatParticipants { chats },
                    ),
                );
            }
        }
        StoryPrivacy::CloseFriends => {
            rules.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowCloseFriends);
        }
        StoryPrivacy::SelectedContacts | StoryPrivacy::Other => {
            let mut users = collect_users(&privacy.allow_peers);
            let chats = collect_chats(&privacy.allow_peers);
            if users.is_empty() && chats.is_empty() {
                users.push(input_user_for(session, session.self_user_id()));
            }
            if !users.is_empty() {
                rules.push(tl_enums::InputPrivacyRule::InputPrivacyValueAllowUsers(
                    tl_types::InputPrivacyValueAllowUsers { users },
                ));
            }
            if !chats.is_empty() {
                rules.push(
                    tl_enums::InputPrivacyRule::InputPrivacyValueAllowChatParticipants(
                        tl_types::InputPrivacyValueAllowChatParticipants { chats },
                    ),
                );
            }
        }
    }
    rules
}

/// Serializes interactive media areas from [`SendStoryRequest`] into TL `MediaArea` items,
/// matching `MediaAreasToMTP` (`Telegram/SourceFiles/api/api_stories.cpp:189-231`).
#[must_use]
pub fn story_media_areas_to_tl(
    session: &Session,
    request: &SendStoryRequest,
) -> Vec<tl_enums::MediaArea> {
    story_media_areas_slices_to_tl(
        session,
        &request.suggested_reactions,
        &request.url_areas,
        &request.locations,
        &request.weather_areas,
        &request.channel_posts,
    )
}

/// Serializes interactive media areas from [`StoryMediaAreasInput`] into TL `MediaArea` items.
#[must_use]
pub fn story_media_areas_input_to_tl(
    session: &Session,
    areas: &StoryMediaAreasInput,
) -> Vec<tl_enums::MediaArea> {
    story_media_areas_slices_to_tl(
        session,
        &areas.suggested_reactions,
        &areas.url_areas,
        &areas.locations,
        &areas.weather_areas,
        &areas.channel_posts,
    )
}

fn story_media_areas_slices_to_tl(
    session: &Session,
    suggested_reactions: &[tg_stories::SuggestedReaction],
    url_areas: &[tg_stories::UrlArea],
    locations: &[tg_stories::StoryLocation],
    weather_areas: &[tg_stories::WeatherArea],
    channel_posts: &[tg_stories::ChannelPost],
) -> Vec<tl_enums::MediaArea> {
    let mut out = Vec::with_capacity(
        suggested_reactions.len()
            + url_areas.len()
            + locations.len()
            + weather_areas.len()
            + channel_posts.len(),
    );
    for reaction in suggested_reactions {
        out.push(tl_enums::MediaArea::SuggestedReaction(
            tl_types::MediaAreaSuggestedReaction {
                dark: reaction.dark,
                flipped: reaction.flipped,
                coordinates: story_area_to_tl(reaction.area),
                reaction: reaction_to_tl(Some(&reaction.reaction)),
            },
        ));
    }
    for url in url_areas {
        out.push(tl_enums::MediaArea::Url(tl_types::MediaAreaUrl {
            coordinates: story_area_to_tl(url.area),
            url: url.url.clone(),
        }));
    }
    for loc in locations {
        let geo = tl_enums::GeoPoint::GeoPoint(tl_types::GeoPoint {
            long: loc.point.lon,
            lat: loc.point.lat,
            access_hash: 0,
            accuracy_radius: loc.point.accuracy_radius,
        });
        if !loc.venue_id.is_empty() {
            out.push(tl_enums::MediaArea::Venue(tl_types::MediaAreaVenue {
                coordinates: story_area_to_tl(loc.area),
                geo,
                title: loc.title.clone(),
                address: loc.address.clone(),
                provider: loc.provider.clone(),
                venue_id: loc.venue_id.clone(),
                venue_type: loc.venue_type.clone(),
            }));
        } else {
            let address = (!loc.country_iso2.is_empty() || !loc.city.is_empty()).then(|| {
                tl_enums::GeoPointAddress::GeoPointAddress(tl_types::GeoPointAddress {
                    country_iso2: loc.country_iso2.clone(),
                    state: (!loc.state.is_empty()).then(|| loc.state.clone()),
                    city: (!loc.city.is_empty()).then(|| loc.city.clone()),
                    street: (!loc.street.is_empty()).then(|| loc.street.clone()),
                })
            });
            out.push(tl_enums::MediaArea::GeoPoint(tl_types::MediaAreaGeoPoint {
                coordinates: story_area_to_tl(loc.area),
                geo,
                address,
            }));
        }
    }
    for weather in weather_areas {
        out.push(tl_enums::MediaArea::Weather(tl_types::MediaAreaWeather {
            coordinates: story_area_to_tl(weather.area),
            emoji: weather.emoji.clone(),
            temperature_c: f64::from(weather.millicelsius) / 1000.0,
            color: weather.color as i32,
        }));
    }
    for post in channel_posts {
        if let Some(peer) = session.peer(post.item_id.peer)
            && let Some(ch) = peer.as_channel()
        {
            out.push(tl_enums::MediaArea::InputMediaAreaChannelPost(
                tl_types::InputMediaAreaChannelPost {
                    coordinates: story_area_to_tl(post.area),
                    channel: tl_enums::InputChannel::InputChannel(tl_types::InputChannel {
                        channel_id: ch.channel_id().0 as i64,
                        access_hash: ch.access_hash as i64,
                    }),
                    msg_id: post.item_id.msg.0 as i32,
                },
            ));
        }
    }
    out
}

/// Extracts the created `StoryId` from a `stories.sendStory` `Updates` response
/// (`StoryIdFromUpdates` in `api_stories.cpp:89-115`).
fn story_id_from_updates(updates: &tl_enums::Updates) -> StoryId {
    let (_, _, story_updates) = extract_story_updates_from_tl(updates);
    let mut found: StoryId = 0;
    for upd in story_updates {
        if let tl_enums::Update::Story(u) = upd {
            match u.story {
                tl_enums::StoryItem::StoryItem(s) => found = s.id,
                tl_enums::StoryItem::Skipped(s) => found = s.id,
                tl_enums::StoryItem::Deleted(_) => {}
            }
        }
    }
    found
}

/// Reads an entire video payload progressively through [`TelegramRangeLoader`] in 128 KiB
/// chunks (bounded by [`MAX_VIDEO_STORY_BYTES`]).
fn read_all_via_range_loader(
    loader: &TelegramRangeLoader<tg_app_core::core_types::erased_rpc::ErasedRpcClient>,
    known_size: u64,
) -> Option<Vec<u8>> {
    const CHUNK_LEN: usize = 128 * 1024;
    if known_size > 0 {
        let capped = known_size.min(MAX_VIDEO_STORY_BYTES) as usize;
        return loader.load_range(0, capped).ok();
    }
    let mut out = Vec::new();
    let mut offset = 0u64;
    while offset < MAX_VIDEO_STORY_BYTES {
        match loader.load_range(offset, CHUNK_LEN) {
            Ok(chunk) => {
                let len = chunk.len();
                out.extend_from_slice(&chunk);
                offset = offset.saturating_add(len as u64);
                if len < CHUNK_LEN {
                    break;
                }
            }
            Err(_) if !out.is_empty() => break,
            Err(_) => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}
