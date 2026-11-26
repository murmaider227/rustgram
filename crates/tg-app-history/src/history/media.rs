//! Media element painting and asset caches: stripped thumbnails, albums, photos, videos,
//! documents, voice waveforms, Lottie animated stickers, and interactive polls.
//!
//! Port origin: `Telegram/SourceFiles/history/view/media/history_view_photo.cpp`,
//! `history_view_document.cpp`, `history_view_sticker.cpp`, `history_view_gif.cpp`,
//! `history_view_poll.cpp`, and `history_view_media_grouped.cpp`.

use std::collections::HashMap;
use std::fmt;

use egui::{
    Align2, Color32, ColorImage, CornerRadius, CursorIcon, FontId, Id, Painter, Pos2, Rect, Sense,
    Shape, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};
use tg_history::consts::{
    HISTORY_POLL_ANSWERS_SKIP, HISTORY_POLL_QUESTION_TOP, HISTORY_POLL_SUBTITLE_SKIP,
    HISTORY_POLL_TOTAL_VOTES_SKIP, HISTORY_TRANSCRIBE_SIZE, HISTORY_TRANSCRIBE_SKIP,
    MSG_FILE_LAYOUT, MSG_FILE_THUMB_LAYOUT, MSG_PADDING, MSG_WAVEFORM_MAX, MSG_WAVEFORM_MIN,
};
use tg_history::{
    BubbleCornerRounding, BubbleRounding, GroupedMediaLayout, MediaLayout, apply_rounding_sides,
};
use tg_lottie::{Composition, Size as LottieSize, render_composition};
use tg_media::image::inline::from_inline_bytes;
use tg_ui::painter::image_prepare;

use crate::history::backend::{
    AlbumItem, DocumentAttachment, GameAttachment, GiveawayAttachment, GiveawayResultsAttachment,
    HistoryMedia, PhotoAttachment, PollAttachment, StickerAttachment, StoryAttachment,
    TranscribeLimitsView, TranscribeUiEntry, VideoAttachment, VideoStreamAttachment,
    VoiceAttachment, WidgetAttachment,
};
use crate::history::inline_media::{
    HistoryMediaRuntime, LottieFrameState, MSG_FILE_OVER_DURATION_SECS, MediaDownloadAction,
    PhotoLink, is_music_document, paint_music_document_bubble, paint_round_video_inline,
};
use crate::history::photo_frame::{PhotoFit, cover_uv};
use crate::history::theme::HistoryColors;
use crate::history::voice_view::{
    MEDIA_UNREAD_SIZE, MEDIA_UNREAD_SKIP, MEDIA_UNREAD_TOP, VoiceStatus, WaveformBarsCache,
    locale_decimal_point, paint_waveform_bars, voice_status_text, waveform_available_width,
    waveform_progress,
};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_media::media::inline::InlinePlaybackKind;

/// Interaction emitted from clicking inside a media block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaInteraction {
    /// Open photo/video/album tile in media viewer.
    OpenMedia {
        /// Message id.
        msg_id: MsgId,
        /// Optional sub-item index for albums.
        album_index: Option<usize>,
    },
    /// Save file/media to disk.
    SaveAs {
        /// Message id.
        msg_id: MsgId,
        /// Suggested filename.
        file_name: String,
    },
    /// Toggle play/pause on a voice message or seek to fraction `[0..=1000]`.
    ToggleVoicePlay {
        /// Message id.
        msg_id: MsgId,
        /// Optional seek fraction in permille (`0..=1000`).
        seek_permille: Option<u16>,
    },
    /// Vote on a poll option.
    VotePoll {
        /// Message id.
        msg_id: MsgId,
        /// Selected option index.
        option_index: usize,
    },
    /// Open poll voters list for a non-anonymous poll (`messages.getPollVotes`).
    OpenPollVoters(MsgId),
    /// User clicked an HTML5 game card to play it (`history_view_game.cpp` `_openl`).
    PlayGame(MsgId),
    /// User clicked a checklist task to toggle completion (`history_view_todo_list.cpp`).
    ToggleTodoItem {
        /// Checklist message id.
        msg_id: MsgId,
        /// Task id inside the checklist.
        item_id: i32,
    },
    /// User clicked "+ Add task" on a checklist bubble.
    AddTodoTasks(MsgId),
    /// User clicked "Edit" on an outgoing checklist bubble.
    EditTodoList(MsgId),
    /// User clicked the `"→A"` voice/round-video transcription toggle button.
    ToggleTranscribe {
        /// Message id.
        msg_id: MsgId,
        /// Duration of the voice or round video in seconds (for trial duration check).
        duration_secs: u32,
        /// True if triggered from a round video message.
        is_round_video: bool,
    },
    /// User clicked thumbs-up / thumbs-down to rate a completed transcription.
    RateTranscribe {
        /// Message id.
        msg_id: MsgId,
        /// `true` for good rating, `false` for bad rating.
        good: bool,
    },
    /// User clicked a sandboxed HTML widget card to launch / interact with it (`history_view_html_widget.cpp`).
    OpenWidget(MsgId),
    /// User clicked "+ Add Option" on a Layer-233 open-answers poll (`messages.addPollAnswer`).
    OpenAddPollOption(MsgId),
    /// User clicked a self-destructing / view-once media plate (`ttl_media_layer_widget.cpp`).
    OpenViewOnceMedia {
        /// Message id.
        msg_id: MsgId,
        /// Media kind.
        kind: tg_app_info::boxes::self_destruct::ViewOnceMediaKind,
        /// Wire `ttl_seconds` (`i32::MAX` = view once, `1..=60` = timed seconds).
        ttl_seconds: i32,
    },
    /// User clicked an interactive Story share / mention card (`history_view_story_mention.cpp`).
    OpenStory {
        /// Story owner peer ID.
        peer_id: PeerId,
        /// Story identifier.
        story_id: i32,
    },
    /// User clicked an interactive Giveaway / GiveawayResults card (`history_view_giveaway.cpp`).
    OpenGiveawayInfo {
        /// Owning chat peer ID.
        peer_id: PeerId,
        /// Message ID of the giveaway bubble.
        msg_id: MsgId,
        /// Primary host channel peer ID, if known.
        channel_id: Option<PeerId>,
        /// Original launch message ID (populated on `GiveawayResults`).
        launch_msg_id: Option<MsgId>,
    },
    /// User clicked an interactive Live Video Stream card (`media_view_video_stream.cpp`).
    OpenVideoStream {
        /// Message ID carrying the video stream attachment.
        msg_id: MsgId,
        /// Group call ID.
        call_id: i64,
        /// Group call access hash.
        access_hash: u64,
        /// Whether this is an RTMP/HLS stream.
        rtmp_stream: bool,
        /// Display title of the stream.
        title: String,
    },
}

/// Active voice playback UI state tracked across frames.
#[derive(Debug, Clone, Default)]
pub struct VoicePlaybackState {
    /// Currently playing message id, if any.
    pub playing_msg_id: Option<MsgId>,
    /// Playback progress per message id in `0.0..=1.0`.
    pub progress: HashMap<MsgId, f32>,
    /// Expanded transcription toggles per message id (fallback for offline/mock voice items).
    pub transcription_expanded: HashMap<MsgId, bool>,
    /// Live per-message speech-to-text transcription states synced from `HistoryBackend`.
    pub transcriptions: HashMap<MsgId, TranscribeUiEntry>,
    /// Current Premium / free-trial transcription limits synced from `HistoryBackend`.
    pub transcribe_limits: TranscribeLimitsView,
    /// Waveform bars per message, recomputed only when the waveform or the width changes.
    pub wave_bars: WaveformBarsCache,
}

impl VoicePlaybackState {
    /// Advances active voice playback by `dt_secs` given its total duration.
    pub fn advance(&mut self, msg_id: MsgId, duration_secs: u32, dt_secs: f32) -> bool {
        if self.playing_msg_id != Some(msg_id) || duration_secs == 0 {
            return false;
        }
        let entry = self.progress.entry(msg_id).or_insert(0.0);
        *entry += dt_secs / duration_secs as f32;
        if *entry >= 1.0 {
            *entry = 0.0;
            self.playing_msg_id = None;
            false
        } else {
            true
        }
    }

    /// Toggles playback or seeks within `msg_id`.
    pub fn handle_interaction(&mut self, msg_id: MsgId, seek_permille: Option<u16>) {
        if let Some(permille) = seek_permille {
            let frac = (f32::from(permille) / 1000.0).clamp(0.0, 1.0);
            self.progress.insert(msg_id, frac);
            self.playing_msg_id = Some(msg_id);
        } else if self.playing_msg_id == Some(msg_id) {
            self.playing_msg_id = None;
        } else {
            self.playing_msg_id = Some(msg_id);
        }
    }
}

/// Stripped-thumbnail textures kept (tiny blurred placeholders, rebuilt on demand).
const STRIPPED_TEXTURES_CAP: usize = 1024;
/// Lottie compositions / frames kept by the runtime-less fallback renderer.
const FALLBACK_LOTTIE_COMPS_CAP: usize = 128;
const FALLBACK_LOTTIE_FRAMES_CAP: usize = 512;

/// Cached GPU textures and decoded Lottie compositions for smooth 60fps painting.
#[derive(Default)]
pub struct MediaAssetCache {
    stripped_textures: HashMap<u64, Option<TextureHandle>>,
    lottie_comps: HashMap<u64, Option<Composition>>,
    lottie_frames: HashMap<(u64, u32), TextureHandle>,
    /// The badge shown for a sticker whose Lottie JSON does not parse (same for every sticker).
    sticker_fallback: Option<TextureHandle>,
    emoji: tg_emoji::egui_ext::EmojiTextureCache,
    /// Downloaded reply header thumbnails (`history::reply_preview`).
    pub(crate) reply_previews: crate::history::reply_preview::ReplyPreviewTextures,
}

impl fmt::Debug for MediaAssetCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaAssetCache")
            .field("stripped_textures", &self.stripped_textures.len())
            .field("lottie_comps", &self.lottie_comps.len())
            .field("lottie_frames", &self.lottie_frames.len())
            .finish()
    }
}

impl MediaAssetCache {
    /// Creates an empty media cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Emoji sprite-sheet textures (reaction buttons, reaction strip).
    pub fn emoji(&mut self) -> &mut tg_emoji::egui_ext::EmojiTextureCache {
        &mut self.emoji
    }

    /// Decodes (once) and returns a GPU texture for Telegram stripped thumbnail bytes via `tg-media`,
    /// blurred like `Images::Prepare(..., Images::Option::Blur)`.
    pub fn get_stripped_texture(
        &mut self,
        ctx: &egui::Context,
        cache_key: u64,
        bytes: &[u8],
    ) -> Option<TextureHandle> {
        if let Some(cached) = self.stripped_textures.get(&cache_key) {
            return cached.clone();
        }
        if self.stripped_textures.len() >= STRIPPED_TEXTURES_CAP {
            self.stripped_textures.clear();
        }
        let decoded = from_inline_bytes(bytes).ok().map(|rgba| {
            let rgba = blur_small_rgba(&rgba);
            let (w, h) = rgba.dimensions();
            let raw = rgba.into_raw();
            let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &raw);
            ctx.load_texture(
                format!("tg_stripped_thumb_{cache_key}"),
                img,
                TextureOptions::LINEAR,
            )
        });
        self.stripped_textures.insert(cache_key, decoded.clone());
        decoded
    }

    /// Renders and caches the current animation frame of a Lottie sticker via `tg-lottie`.
    pub fn get_lottie_frame(
        &mut self,
        ctx: &egui::Context,
        sticker: &StickerAttachment,
        time_secs: f64,
        render_px: u32,
    ) -> Option<(TextureHandle, bool)> {
        if self.lottie_comps.len() >= FALLBACK_LOTTIE_COMPS_CAP
            && !self.lottie_comps.contains_key(&sticker.sticker_id)
        {
            self.lottie_comps.clear();
        }
        self.lottie_comps
            .entry(sticker.sticker_id)
            .or_insert_with(|| Composition::from_json(&sticker.lottie_json).ok());
        let comp = self.lottie_comps.get(&sticker.sticker_id)?.as_ref()?;
        let frame_span = (comp.out_point - comp.in_point).max(1.0);
        let fps = comp.frame_rate.clamp(1.0, 60.0);
        // Quantize to 24 discrete frames max for crisp smooth animation with bounded cache memory.
        let total_steps = (frame_span as u32).clamp(1, 24);
        let elapsed_frames = (time_secs * fps) % frame_span;
        let step_idx =
            ((elapsed_frames / frame_span) * f64::from(total_steps)).floor() as u32 % total_steps;
        let sample_frame =
            comp.in_point + (f64::from(step_idx) / f64::from(total_steps)) * frame_span;

        let key = (sticker.sticker_id, step_idx);
        if let Some(tex) = self.lottie_frames.get(&key) {
            return Some((tex.clone(), total_steps > 1));
        }

        if self.lottie_frames.len() >= FALLBACK_LOTTIE_FRAMES_CAP {
            self.lottie_frames.clear();
        }
        let side = render_px.clamp(64, 224);
        let fb = render_composition(comp, sample_frame, LottieSize::new(side, side));
        let img =
            ColorImage::from_rgba_premultiplied([fb.width as usize, fb.height as usize], &fb.data);
        let tex = ctx.load_texture(
            format!("tg_lottie_{}_{}", sticker.sticker_id, step_idx),
            img,
            TextureOptions::LINEAR,
        );
        self.lottie_frames.insert(key, tex.clone());
        Some((tex, total_steps > 1))
    }

    /// Texture of the badge painted for a sticker whose Lottie JSON does not parse.
    fn sticker_fallback(&mut self, ctx: &egui::Context) -> &TextureHandle {
        self.sticker_fallback.get_or_insert_with(|| {
            let circle = image_prepare::vertical_gradient_circle(
                64,
                Color32::from_rgb(0x40, 0xA7, 0xE3),
                Color32::from_rgb(0x2A, 0x7A, 0xB8),
            );
            ctx.load_texture("tg_sticker_fallback", circle, TextureOptions::LINEAR)
        })
    }
}

/// Converts a `tg-history` [`BubbleRounding`] into an `egui::CornerRadius`.
#[must_use]
pub fn bubble_rounding_to_egui(rounding: BubbleRounding) -> CornerRadius {
    let map = |c: BubbleCornerRounding| -> u8 {
        match c {
            BubbleCornerRounding::None => 0,
            BubbleCornerRounding::Small => 6,
            BubbleCornerRounding::Large => 16,
            BubbleCornerRounding::Tail => 4,
        }
    };
    CornerRadius {
        nw: map(rounding.top_left),
        ne: map(rounding.top_right),
        sw: map(rounding.bottom_left),
        se: map(rounding.bottom_right),
    }
}

pub use tg_app_core::core_types::emoji_sets::{format_duration, format_file_size};

/// Paints a [`HistoryMedia`] inside `rect` using the computed `media_layout` and returns any interaction.
#[allow(clippy::too_many_arguments)]
pub fn paint_history_media(
    ui: &mut Ui,
    msg_id: MsgId,
    media: &HistoryMedia,
    media_layout: Option<&MediaLayout>,
    rect: Rect,
    rounding: BubbleRounding,
    outgoing: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
    voice_state: &mut VoicePlaybackState,
) -> Option<MediaInteraction> {
    paint_history_media_internal(
        ui,
        PeerId::ZERO,
        msg_id,
        "",
        media,
        media_layout,
        rect,
        rounding,
        outgoing,
        colors,
        assets,
        voice_state,
        None,
    )
}

/// Paints a [`HistoryMedia`] wired to [`HistoryMediaRuntime`] (`tg-video` inline GIF/round-video/
/// WebM-sticker playback, `tg-audio::PlayerInstance` voice/music playback, per-size `tg-lottie`
/// caching, `tg-video` thumbnail generation, photo loading progress/blur-up, and spoiler veils).
#[allow(clippy::too_many_arguments)]
pub fn paint_history_media_with_runtime(
    ui: &mut Ui,
    peer_id: PeerId,
    msg_id: MsgId,
    sender_name: &str,
    media: &HistoryMedia,
    media_layout: Option<&MediaLayout>,
    rect: Rect,
    rounding: BubbleRounding,
    outgoing: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
    voice_state: &mut VoicePlaybackState,
    runtime: &mut HistoryMediaRuntime,
) -> Option<MediaInteraction> {
    paint_history_media_internal(
        ui,
        peer_id,
        msg_id,
        sender_name,
        media,
        media_layout,
        rect,
        rounding,
        outgoing,
        colors,
        assets,
        voice_state,
        Some(runtime),
    )
}

#[allow(clippy::too_many_arguments)]
fn paint_history_media_internal(
    ui: &mut Ui,
    peer_id: PeerId,
    msg_id: MsgId,
    sender_name: &str,
    media: &HistoryMedia,
    media_layout: Option<&MediaLayout>,
    rect: Rect,
    rounding: BubbleRounding,
    outgoing: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
    voice_state: &mut VoicePlaybackState,
    runtime: Option<&mut HistoryMediaRuntime>,
) -> Option<MediaInteraction> {
    match media {
        HistoryMedia::Photo(photo) => {
            paint_photo(ui, msg_id, photo, rect, rounding, assets, runtime)
        }
        HistoryMedia::Album(items) => {
            let grouped = media_layout.and_then(|ml| ml.grouped.as_ref());
            paint_album(ui, msg_id, items, grouped, rect, rounding, assets, runtime)
        }
        HistoryMedia::Video(video) => {
            paint_video(ui, msg_id, video, rect, rounding, assets, runtime)
        }
        HistoryMedia::Document(doc) => {
            if let Some(rt) = runtime
                && is_music_document(doc, rt.descriptor(msg_id))
            {
                paint_music_document_bubble(ui, peer_id, msg_id, doc, rect, outgoing, colors, rt);
                return None;
            }
            paint_document(ui, msg_id, doc, rect, outgoing, colors)
        }
        HistoryMedia::Voice(voice) => paint_voice(
            ui,
            peer_id,
            msg_id,
            sender_name,
            voice,
            rect,
            outgoing,
            colors,
            voice_state,
            runtime,
        ),
        HistoryMedia::Sticker(sticker) => paint_sticker(ui, msg_id, sticker, rect, assets, runtime),
        HistoryMedia::Poll(poll) => paint_poll(ui, msg_id, poll, rect, outgoing, colors),
        HistoryMedia::Location(loc) => paint_location(ui, msg_id, loc, rect, colors),
        HistoryMedia::Contact(contact) => paint_contact(ui, msg_id, contact, rect, colors),
        HistoryMedia::RoundVideo(rv) => {
            let base = if let Some(rt) = runtime {
                paint_round_video_inline(ui, peer_id, msg_id, sender_name, rv, rect, colors, rt);
                None
            } else {
                paint_round_video(ui, msg_id, rv, rect, colors)
            };
            paint_round_video_transcribe_overlay(
                ui,
                msg_id,
                rv.duration_secs,
                rect,
                colors,
                voice_state,
            )
            .or(base)
        }
        HistoryMedia::Game(game) => paint_game(ui, msg_id, game, rect, outgoing, colors, assets),
        HistoryMedia::TodoList(todo) => {
            match crate::history::todo::paint_todo_list_bubble(
                ui,
                msg_id,
                todo,
                rect,
                outgoing,
                false,
                false,
                tg_data::PeerId(peer_id.0).is_user(),
                colors,
            ) {
                Some(crate::history::todo::TodoBubbleAction::ToggleItem { msg_id, item_id }) => {
                    Some(MediaInteraction::ToggleTodoItem { msg_id, item_id })
                }
                Some(crate::history::todo::TodoBubbleAction::OpenAddTasks(msg_id)) => {
                    Some(MediaInteraction::AddTodoTasks(msg_id))
                }
                Some(crate::history::todo::TodoBubbleAction::OpenEdit(msg_id)) => {
                    Some(MediaInteraction::EditTodoList(msg_id))
                }
                None => None,
            }
        }
        HistoryMedia::Widget(widget) => paint_widget(ui, msg_id, widget, rect, outgoing, colors),
        HistoryMedia::Story(story) => paint_story_card(ui, msg_id, story, rect, outgoing, colors),
        HistoryMedia::Giveaway(giveaway) => {
            paint_giveaway_card(ui, peer_id, msg_id, giveaway, rect, outgoing, colors)
        }
        HistoryMedia::GiveawayResults(results) => {
            paint_giveaway_results_card(ui, peer_id, msg_id, results, rect, outgoing, colors)
        }
        HistoryMedia::VideoStream(stream) => {
            paint_video_stream_card(ui, msg_id, stream, rect, outgoing, colors)
        }
    }
}

/// `FillBlurredBackground` (`media_streaming_utility.cpp`): `QColor(0, 0, 0, 48)` over the
/// blurred thumbnail of a single photo.
const BLURRED_BACKGROUND_DIM: u8 = 48;

#[allow(clippy::too_many_arguments)]
fn paint_photo_surface(
    ctx: &egui::Context,
    painter: &Painter,
    rect: Rect,
    cr: CornerRadius,
    cache_key: u64,
    stripped_thumb: Option<&[u8]>,
    fallback_rgb: ([u8; 3], [u8; 3]),
    assets: &mut MediaAssetCache,
) {
    let top = Color32::from_rgb(fallback_rgb.0[0], fallback_rgb.0[1], fallback_rgb.0[2]);
    let bot = Color32::from_rgb(fallback_rgb.1[0], fallback_rgb.1[1], fallback_rgb.1[2]);
    let mid = tg_ui::anim::color_lerp(top, bot, 0.5);

    // Rich vertical gradient backdrop.
    painter.rect_filled(rect, cr, mid);
    // Subtle top light band and bottom horizon silhouette so placeholders look like photography.
    let top_half = Rect::from_min_max(rect.min, Pos2::new(rect.max.x, rect.center().y));
    painter.rect_filled(
        top_half,
        CornerRadius {
            nw: cr.nw,
            ne: cr.ne,
            sw: 0,
            se: 0,
        },
        Color32::from_rgba_unmultiplied(top.r(), top.g(), top.b(), 150),
    );
    let bot_band = Rect::from_min_max(
        Pos2::new(rect.min.x, rect.min.y + rect.height() * 0.62),
        rect.max,
    );
    painter.rect_filled(
        bot_band,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: cr.sw,
            se: cr.se,
        },
        Color32::from_rgba_unmultiplied(bot.r(), bot.g(), bot.b(), 195),
    );

    // Decoded Telegram stripped thumbnail, drawn opaque and blurred, cropped to the box's
    // aspect ratio (`PrepareBlurredBackground` / `Images::Prepare` with `Images::Option::Blur`).
    if let Some(bytes) = stripped_thumb
        && let Some(tex) = assets.get_stripped_texture(ctx, cache_key, bytes)
    {
        painter.add(
            egui::epaint::RectShape::filled(rect, cr, Color32::WHITE)
                .with_texture(tex.id(), cover_uv(rect.size(), tex.size_vec2())),
        );
        return;
    }

    // Demo-only placeholder when no thumbnail bytes exist: a crisp mountain/sun emblem.
    let c = rect.center();
    let s = rect.width().min(rect.height());
    let sun_pos = Pos2::new(
        rect.left() + rect.width() * 0.72,
        rect.top() + rect.height() * 0.30,
    );
    painter.circle_filled(
        sun_pos,
        (s * 0.08).clamp(6.0, 18.0),
        Color32::from_rgba_unmultiplied(255, 245, 210, 165),
    );
    let hill1 = vec![
        Pos2::new(rect.left() + rect.width() * 0.12, rect.bottom() - 8.0),
        Pos2::new(c.x - rect.width() * 0.08, rect.top() + rect.height() * 0.48),
        Pos2::new(rect.right() - rect.width() * 0.25, rect.bottom() - 8.0),
    ];
    painter.add(Shape::convex_polygon(
        hill1,
        Color32::from_rgba_unmultiplied(255, 255, 255, 42),
        Stroke::NONE,
    ));
    let hill2 = vec![
        Pos2::new(c.x - rect.width() * 0.02, rect.bottom() - 8.0),
        Pos2::new(
            rect.right() - rect.width() * 0.28,
            rect.top() + rect.height() * 0.54,
        ),
        Pos2::new(rect.right() - rect.width() * 0.08, rect.bottom() - 8.0),
    ];
    painter.add(Shape::convex_polygon(
        hill2,
        Color32::from_rgba_unmultiplied(255, 255, 255, 64),
        Stroke::NONE,
    ));
}

/// Two passes of a 3x3 box blur over a small decoded thumbnail (stripped sizes are ~40px).
pub(crate) fn blur_small_rgba(src: &image::RgbaImage) -> image::RgbaImage {
    let (w, h) = src.dimensions();
    if w < 3 || h < 3 {
        return src.clone();
    }
    let mut cur = src.clone();
    for _ in 0..2 {
        let prev = cur.clone();
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0u32; 4];
                let mut n = 0u32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let sx = (x as i32 + dx).clamp(0, w as i32 - 1) as u32;
                        let sy = (y as i32 + dy).clamp(0, h as i32 - 1) as u32;
                        let p = prev.get_pixel(sx, sy).0;
                        for k in 0..4 {
                            acc[k] += u32::from(p[k]);
                        }
                        n += 1;
                    }
                }
                cur.put_pixel(x, y, image::Rgba(acc.map(|v| (v / n) as u8)));
            }
        }
    }
    cur
}

fn paint_photo(
    ui: &mut Ui,
    msg_id: MsgId,
    photo: &PhotoAttachment,
    rect: Rect,
    rounding: BubbleRounding,
    assets: &mut MediaAssetCache,
    mut runtime: Option<&mut HistoryMediaRuntime>,
) -> Option<MediaInteraction> {
    let cr = bubble_rounding_to_egui(rounding);
    let now_secs = ui.ctx().input(|i| i.time);
    paint_photo_surface(
        ui.ctx(),
        ui.painter(),
        rect,
        cr,
        (msg_id.0 as u64).wrapping_mul(31) ^ 0x101,
        photo.stripped_thumb.as_deref(),
        photo.fallback_rgb,
        assets,
    );
    if photo.stripped_thumb.is_some() {
        // `FillBlurredBackground`: the blurred thumbnail is dimmed; the full picture covers it
        // or sits centered over it.
        ui.painter()
            .rect_filled(rect, cr, Color32::from_black_alpha(BLURRED_BACKGROUND_DIM));
    }

    // `Photo::textState`: the whole photo is one link, open / cancel / download by its state.
    let resp = ui.interact(rect, Id::unique(("tg_photo", msg_id.0)), Sense::click());
    let link = runtime
        .as_deref()
        .map_or(PhotoLink::Open, |rt| rt.photo_link(msg_id));
    let over = photo_link_over(ui.ctx(), msg_id, link, resp.hovered());

    let mut spoiler_hidden = false;
    if let Some(rt) = runtime.as_deref_mut() {
        rt.paint_photo_load_and_blur_up(
            ui.ctx(),
            ui.painter(),
            msg_id,
            rect,
            cr,
            PhotoFit::Single,
            now_secs,
            over,
        );
        spoiler_hidden =
            rt.paint_spoiler_overlay(ui.ctx(), ui.painter(), msg_id, rect, cr, now_secs);
    }

    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        if spoiler_hidden {
            if let Some(rt) = runtime {
                rt.reveal_spoiler(msg_id, now_secs);
            }
            return None;
        }
        return photo_link_clicked(runtime, msg_id, link).then_some(MediaInteraction::OpenMedia {
            msg_id,
            album_index: None,
        });
    }
    None
}

/// How far the circle of a loading / not loaded photo is towards `msgDateImgBgOver`
/// (`Photo::_animation->a_thumbOver`, `st::msgFileOverDuration`): the pointer is over the
/// cancel or download link.
fn photo_link_over(ctx: &egui::Context, msg_id: MsgId, link: PhotoLink, hovered: bool) -> f32 {
    if link == PhotoLink::Open {
        return 0.0;
    }
    ctx.animate_bool_with_time(
        Id::unique(("tg_photo_over", msg_id.0)),
        hovered,
        MSG_FILE_OVER_DURATION_SECS,
    )
}

/// Handles a click on a photo's link. Returns `true` when the click opens the media viewer;
/// a cancel or download click is queued for the backend instead.
fn photo_link_clicked(
    runtime: Option<&mut HistoryMediaRuntime>,
    msg_id: MsgId,
    link: PhotoLink,
) -> bool {
    let action = match link {
        PhotoLink::Open => return true,
        PhotoLink::Cancel => MediaDownloadAction::Cancel,
        PhotoLink::Save => MediaDownloadAction::Start,
    };
    if let Some(rt) = runtime {
        rt.queue_download(msg_id, action);
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn paint_album(
    ui: &mut Ui,
    msg_id: MsgId,
    items: &[AlbumItem],
    grouped: Option<&GroupedMediaLayout>,
    rect: Rect,
    rounding: BubbleRounding,
    assets: &mut MediaAssetCache,
    mut runtime: Option<&mut HistoryMediaRuntime>,
) -> Option<MediaInteraction> {
    let mut interaction = None;
    let now_secs = ui.ctx().input(|i| i.time);

    if let Some(layout) = grouped {
        for (idx, (item, part)) in items.iter().zip(layout.parts.iter()).enumerate() {
            let tile_rect = Rect::from_min_size(
                Pos2::new(
                    rect.min.x + part.geometry.x as f32,
                    rect.min.y + (part.geometry.y + layout.padding.top) as f32,
                ),
                Vec2::new(part.geometry.width as f32, part.geometry.height as f32),
            );
            let tile_rounding = apply_rounding_sides(rounding, part.sides);
            let cr = bubble_rounding_to_egui(tile_rounding);
            let cache_key = (item.msg_id.0 as u64).wrapping_mul(37) ^ (idx as u64 + 1);
            paint_photo_surface(
                ui.ctx(),
                ui.painter(),
                tile_rect,
                cr,
                cache_key,
                item.stripped_thumb.as_deref(),
                item.fallback_rgb,
                assets,
            );

            if let Some(dur) = item.video_duration_secs {
                paint_video_badge(ui.painter(), tile_rect, &format_duration(dur));
                paint_play_circle(ui.painter(), tile_rect.center(), 18.0);
            }

            let resp = ui.interact(
                tile_rect,
                Id::unique(("tg_album_tile", msg_id.0, idx)),
                Sense::click(),
            );
            let link = runtime
                .as_deref()
                .map_or(PhotoLink::Open, |rt| rt.photo_link(item.msg_id));
            let over = photo_link_over(ui.ctx(), item.msg_id, link, resp.hovered());

            let mut spoiler_hidden = false;
            if let Some(rt) = runtime.as_deref_mut() {
                rt.paint_photo_load_and_blur_up(
                    ui.ctx(),
                    ui.painter(),
                    item.msg_id,
                    tile_rect,
                    cr,
                    PhotoFit::Cover,
                    now_secs,
                    over,
                );
                spoiler_hidden = rt.paint_spoiler_overlay(
                    ui.ctx(),
                    ui.painter(),
                    item.msg_id,
                    tile_rect,
                    cr,
                    now_secs,
                );
            }

            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().rect_filled(
                    tile_rect,
                    cr,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                );
            }
            if resp.clicked() {
                if spoiler_hidden {
                    if let Some(rt) = runtime.as_deref_mut() {
                        rt.reveal_spoiler(item.msg_id, now_secs);
                    }
                } else if photo_link_clicked(runtime.as_deref_mut(), item.msg_id, link) {
                    interaction = Some(MediaInteraction::OpenMedia {
                        msg_id,
                        album_index: Some(idx),
                    });
                }
            }
        }
    }

    interaction
}

fn paint_video_badge(painter: &Painter, rect: Rect, text: &str) {
    let font = FontId::proportional(11.0);
    let galley = painter.layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let badge_size = Vec2::new(galley.size().x + 12.0, galley.size().y + 4.0);
    let badge_rect =
        Rect::from_min_size(Pos2::new(rect.left() + 6.0, rect.top() + 6.0), badge_size);
    painter.rect_filled(
        badge_rect,
        CornerRadius::same(8),
        Color32::from_rgba_unmultiplied(0, 0, 0, 128),
    );
    painter.galley(
        Pos2::new(badge_rect.left() + 6.0, badge_rect.top() + 2.0),
        galley,
        Color32::WHITE,
    );
}

fn paint_play_circle(painter: &Painter, center: Pos2, radius: f32) {
    painter.circle_filled(
        center,
        radius,
        Color32::from_rgba_unmultiplied(0, 0, 0, 124),
    );
    let r = radius * 0.42;
    let pts = vec![
        Pos2::new(center.x - r * 0.55, center.y - r * 0.88),
        Pos2::new(center.x + r * 0.95, center.y),
        Pos2::new(center.x - r * 0.55, center.y + r * 0.88),
    ];
    painter.add(Shape::convex_polygon(pts, Color32::WHITE, Stroke::NONE));
}

fn paint_video(
    ui: &mut Ui,
    msg_id: MsgId,
    video: &VideoAttachment,
    rect: Rect,
    rounding: BubbleRounding,
    assets: &mut MediaAssetCache,
    mut runtime: Option<&mut HistoryMediaRuntime>,
) -> Option<MediaInteraction> {
    let cr = bubble_rounding_to_egui(rounding);
    let now_secs = ui.ctx().input(|i| i.time);
    let now_ms = (now_secs * 1000.0).max(0.0) as i64;

    let mut inline_playing = false;
    let mut painted_live_or_thumb = false;

    if let Some(rt) = runtime.as_deref_mut() {
        let target_w = rect.width().round().clamp(64.0, 480.0) as u32;
        let target_h = rect.height().round().clamp(64.0, 480.0) as u32;
        if video.is_gif
            && let Some(frame_state) = rt.inline_clip_frame(
                ui.ctx(),
                msg_id,
                InlinePlaybackKind::Gif,
                Some((target_w, target_h)),
                now_ms,
            )
            && let Some(tex) = frame_state.texture
        {
            ui.painter().image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            inline_playing = frame_state.is_playing;
            painted_live_or_thumb = true;
        } else if video.stripped_thumb.is_none()
            && let Some(gen_thumb) =
                rt.get_or_generate_video_thumb(ui.ctx(), msg_id, target_w.max(target_h), false)
        {
            ui.painter().image(
                gen_thumb.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            painted_live_or_thumb = true;
        }
    }

    if !painted_live_or_thumb {
        paint_photo_surface(
            ui.ctx(),
            ui.painter(),
            rect,
            cr,
            (msg_id.0 as u64).wrapping_mul(53) ^ 0x202,
            video.stripped_thumb.as_deref(),
            video.fallback_rgb,
            assets,
        );
    }

    let badge = if video.is_gif {
        if inline_playing {
            "GIF".to_owned()
        } else {
            format!("GIF  {}", format_file_size(video.size_bytes))
        }
    } else {
        format!(
            "{}  {}",
            format_duration(video.duration_secs),
            format_file_size(video.size_bytes)
        )
    };
    paint_video_badge(ui.painter(), rect, &badge);
    if !inline_playing {
        paint_play_circle(ui.painter(), rect.center(), 24.0);
    }

    let mut spoiler_hidden = false;
    if let Some(rt) = runtime.as_deref() {
        spoiler_hidden =
            rt.paint_spoiler_overlay(ui.ctx(), ui.painter(), msg_id, rect, cr, now_secs);
    }

    let resp = ui.interact(rect, Id::unique(("tg_video", msg_id.0)), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        if spoiler_hidden {
            if let Some(rt) = runtime {
                rt.reveal_spoiler(msg_id, now_secs);
            }
            return None;
        }
        return Some(MediaInteraction::OpenMedia {
            msg_id,
            album_index: None,
        });
    }
    None
}

fn paint_document(
    ui: &mut Ui,
    msg_id: MsgId,
    doc: &DocumentAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let thumbed = doc.thumb_rgb.is_some();
    let st = if thumbed {
        MSG_FILE_THUMB_LAYOUT
    } else {
        MSG_FILE_LAYOUT
    };
    let thumb_size = st.thumb_size as f32;
    let thumb_rect = Rect::from_min_size(
        Pos2::new(
            rect.left() + st.padding.left as f32,
            rect.top() + st.padding.top as f32,
        ),
        Vec2::splat(thumb_size),
    );

    let accent = if outgoing && !colors.is_night {
        colors.wave_out_active
    } else {
        colors.accent
    };

    if let Some([r, g, b]) = doc.thumb_rgb {
        let base = Color32::from_rgb(r, g, b);
        ui.painter()
            .rect_filled(thumb_rect, CornerRadius::same(8), base);
        // Document sheet glyph inside thumbnail.
        let inner = thumb_rect.shrink(16.0);
        ui.painter().rect_filled(
            inner,
            CornerRadius::same(4),
            Color32::from_rgba_unmultiplied(255, 255, 255, 225),
        );
        let ext = doc
            .file_name
            .rsplit('.')
            .next()
            .unwrap_or("FILE")
            .to_uppercase();
        ui.painter().text(
            inner.center(),
            Align2::CENTER_CENTER,
            ext,
            FontId::proportional(11.0),
            base,
        );
    } else {
        ui.painter()
            .circle_filled(thumb_rect.center(), thumb_size * 0.5, accent);
        // Down-arrow download glyph.
        let c = thumb_rect.center();
        let st_white = Stroke::new(2.0, Color32::WHITE);
        ui.painter().line_segment(
            [Pos2::new(c.x, c.y - 7.0), Pos2::new(c.x, c.y + 6.0)],
            st_white,
        );
        ui.painter().add(Shape::line(
            vec![
                Pos2::new(c.x - 5.0, c.y + 1.5),
                Pos2::new(c.x, c.y + 6.5),
                Pos2::new(c.x + 5.0, c.y + 1.5),
            ],
            st_white,
        ));
    }

    let text_left = thumb_rect.right() + st.thumb_skip as f32;
    let max_text_w = (rect.right() - text_left - st.padding.right as f32).max(40.0);

    // Filename (`st::semiboldTextStyle`, history_view_document.cpp)
    let name_galley = ui.painter().layout(
        doc.file_name.clone(),
        FontId::new(13.0, tg_ui::fonts::family_semibold()),
        colors.bold_fg,
        max_text_w,
    );
    ui.painter().galley(
        Pos2::new(text_left, rect.top() + st.name_top as f32),
        name_galley,
        colors.bold_fg,
    );

    // Size & MIME status
    let status = format_file_size(doc.size_bytes);
    let status_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.sub_text_fg
    };
    ui.painter().text(
        Pos2::new(text_left, rect.top() + st.status_top as f32),
        Align2::LEFT_TOP,
        status,
        FontId::proportional(12.0),
        status_fg,
    );

    if thumbed && st.link_top > 0 {
        ui.painter().text(
            Pos2::new(text_left, rect.top() + st.link_top as f32),
            Align2::LEFT_TOP,
            "SAVE AS",
            FontId::proportional(11.5),
            accent,
        );
    }

    let resp = ui.interact(rect, Id::unique(("tg_doc", msg_id.0)), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        return Some(MediaInteraction::SaveAs {
            msg_id,
            file_name: doc.file_name.clone(),
        });
    }
    None
}

/// Whether the status line of a voice message leaves the file size out
/// (`Ui::FileStatusSizeLoaded`, `Document::dataLoaded`): the file is in the media cache. Data
/// without a cache (demo) counts as loaded once the message was started.
fn voice_file_loaded(
    runtime: Option<&HistoryMediaRuntime>,
    voice_state: &VoicePlaybackState,
    msg_id: MsgId,
) -> bool {
    runtime
        .and_then(|rt| rt.voice_loaded(msg_id))
        .unwrap_or_else(|| voice_state.progress.contains_key(&msg_id))
}

#[allow(clippy::too_many_arguments)]
fn paint_voice(
    ui: &mut Ui,
    peer_id: PeerId,
    msg_id: MsgId,
    sender_name: &str,
    voice: &VoiceAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
    voice_state: &mut VoicePlaybackState,
    mut runtime: Option<&mut HistoryMediaRuntime>,
) -> Option<MediaInteraction> {
    let st = MSG_FILE_LAYOUT;
    let btn_size = st.thumb_size as f32;
    let btn_rect = Rect::from_min_size(
        Pos2::new(
            rect.left() + st.padding.left as f32,
            rect.top() + st.padding.top as f32,
        ),
        Vec2::splat(btn_size),
    );

    let active_color = if outgoing {
        colors.wave_out_active
    } else {
        colors.wave_in_active
    };
    let inactive_color = if outgoing {
        colors.wave_out_inactive
    } else {
        colors.wave_in_inactive
    };

    let (is_active_audio, is_playing, progress, speed) = if let Some(rt) = runtime.as_deref() {
        let (active, playing, prog, spd, _) =
            rt.audio_item_state(msg_id, tg_audio::AudioType::Voice);
        if rt.voice_play_pending(msg_id) {
            // Started, its file is still downloading: shown like a playing message at 00:00
            // (tdesktop's streamed voice while it buffers).
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(40));
            (true, true, 0.0, spd)
        } else if active {
            voice_state.progress.insert(msg_id, prog);
            if playing {
                voice_state.playing_msg_id = Some(msg_id);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(40));
            } else if voice_state.playing_msg_id == Some(msg_id) {
                voice_state.playing_msg_id = None;
            }
            (true, playing, prog, spd)
        } else {
            let legacy_playing = voice_state.playing_msg_id == Some(msg_id);
            if legacy_playing {
                let dt = ui.ctx().input(|i| i.stable_dt).clamp(0.0, 0.1);
                if voice_state.advance(msg_id, voice.duration_secs, dt) {
                    ui.ctx().request_repaint();
                }
            }
            let prog = voice_state.progress.get(&msg_id).copied().unwrap_or(0.0);
            (legacy_playing, legacy_playing, prog, rt.policy.voice_speed)
        }
    } else {
        let legacy_playing = voice_state.playing_msg_id == Some(msg_id);
        if legacy_playing {
            let dt = ui.ctx().input(|i| i.stable_dt).clamp(0.0, 0.1);
            if voice_state.advance(msg_id, voice.duration_secs, dt) {
                ui.ctx().request_repaint();
            }
        }
        let prog = voice_state.progress.get(&msg_id).copied().unwrap_or(0.0);
        (legacy_playing, legacy_playing, prog, 1.0)
    };

    // Play / Pause circle button
    ui.painter()
        .circle_filled(btn_rect.center(), btn_size * 0.5, active_color);
    let c = btn_rect.center();
    if is_playing {
        let bar_w = 3.2;
        let bar_h = 13.0;
        ui.painter().rect_filled(
            Rect::from_center_size(Pos2::new(c.x - 3.5, c.y), Vec2::new(bar_w, bar_h)),
            CornerRadius::same(1),
            Color32::WHITE,
        );
        ui.painter().rect_filled(
            Rect::from_center_size(Pos2::new(c.x + 3.5, c.y), Vec2::new(bar_w, bar_h)),
            CornerRadius::same(1),
            Color32::WHITE,
        );
    } else {
        let pts = vec![
            Pos2::new(c.x - 4.0, c.y - 7.5),
            Pos2::new(c.x + 8.0, c.y),
            Pos2::new(c.x - 4.0, c.y + 7.5),
        ];
        ui.painter()
            .add(Shape::convex_polygon(pts, Color32::WHITE, Stroke::NONE));
    }

    let btn_resp = ui.interact(
        btn_rect,
        Id::unique(("tg_voice_btn", msg_id.0)),
        Sense::click(),
    );
    if btn_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if btn_resp.clicked() {
        if let Some(rt) = runtime.as_deref_mut() {
            rt.toggle_or_seek_voice(peer_id, msg_id, sender_name, voice.duration_secs, None);
        }
        voice_state.handle_interaction(msg_id, None);
        return Some(MediaInteraction::ToggleVoicePlay {
            msg_id,
            seek_permille: None,
        });
    }

    // Waveform (`PaintWaveform`): bars resampled to the name column, scaled by the loudest
    // sample and centred on one line; the bars come from `voice_state.wave_bars`.
    let wave_left = btn_rect.right() + st.thumb_skip as f32;
    let status_width = (rect.right() - wave_left - st.padding.right as f32).floor() as i32;
    let name_width =
        (status_width - HISTORY_TRANSCRIBE_SKIP - HISTORY_TRANSCRIBE_SIZE.width).max(0);
    let wave_avail = waveform_available_width(name_width);
    let wave_top = btn_rect.top();
    let wave_rect = Rect::from_min_size(
        Pos2::new(wave_left, wave_top),
        Vec2::new(
            name_width as f32,
            (MSG_WAVEFORM_MAX + MSG_WAVEFORM_MIN) as f32,
        ),
    );
    // `voice->playback`: the player has this message (playing or paused).
    let has_playback = is_active_audio || (runtime.is_none() && progress > 0.0);
    let bars = voice_state
        .wave_bars
        .bars(msg_id, &voice.waveform_5bit, wave_avail);
    paint_waveform_bars(
        ui.painter(),
        Pos2::new(wave_left, wave_top),
        &bars,
        wave_avail,
        waveform_progress(outgoing, has_playback, voice.is_unread, progress),
        active_color,
        inactive_color,
    );

    let wave_resp = ui.interact(
        wave_rect,
        Id::unique(("tg_voice_wave", msg_id.0)),
        Sense::click_and_drag(),
    );
    if wave_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if (wave_resp.clicked() || wave_resp.dragged())
        && let Some(pos) = wave_resp.interact_pointer_pos()
    {
        let frac = ((pos.x - wave_rect.left()) / wave_rect.width().max(1.0)).clamp(0.0, 1.0);
        let permille = (frac * 1000.0).round() as u16;
        if let Some(rt) = runtime.as_deref_mut() {
            rt.toggle_or_seek_voice(
                peer_id,
                msg_id,
                sender_name,
                voice.duration_secs,
                Some(f64::from(frac)),
            );
        }
        voice_state.handle_interaction(msg_id, Some(permille));
        return Some(MediaInteraction::ToggleVoicePlay {
            msg_id,
            seek_permille: Some(permille),
        });
    }

    // Status line (`File::setStatusSize`): "00:04, 15.9 KB" until the voice message was
    // started, "00:01 / 00:04" while the player has it, "00:04" afterwards; then the unread
    // dot and the inline playback speed button (`1x`/`1.5x`/`2x`).
    let status = if has_playback {
        VoiceStatus::Played {
            position_secs: (progress * voice.duration_secs as f32).floor() as u32,
        }
    } else if voice_file_loaded(runtime.as_deref(), voice_state, msg_id) {
        VoiceStatus::Loaded
    } else {
        VoiceStatus::Ready
    };
    // `stm->mediaFg`.
    let status_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.msg_in_date_fg
    };
    let dur_galley = ui.painter().layout_no_wrap(
        voice_status_text(
            status,
            voice.duration_secs,
            voice.size_bytes,
            locale_decimal_point(),
        ),
        FontId::proportional(13.0),
        status_fg,
    );
    let dur_pos = Pos2::new(
        wave_left,
        btn_rect.top() + (st.status_top - st.padding.top) as f32,
    );
    let mut dur_w = dur_galley.size().x;
    ui.painter().galley(dur_pos, dur_galley, status_fg);
    if voice.is_unread && dur_w + MEDIA_UNREAD_SKIP + MEDIA_UNREAD_SIZE <= status_width as f32 {
        ui.painter().circle_filled(
            Pos2::new(
                dur_pos.x + dur_w + MEDIA_UNREAD_SKIP + MEDIA_UNREAD_SIZE / 2.0,
                dur_pos.y + MEDIA_UNREAD_TOP + MEDIA_UNREAD_SIZE / 2.0,
            ),
            MEDIA_UNREAD_SIZE / 2.0,
            active_color,
        );
        dur_w += MEDIA_UNREAD_SKIP + MEDIA_UNREAD_SIZE;
    }

    if is_active_audio {
        let spd_rect = Rect::from_min_size(
            Pos2::new(dur_pos.x + dur_w + 10.0, dur_pos.y - 1.0),
            Vec2::new(32.0, 17.0),
        );
        let spd_fast = (speed - 1.0).abs() > 0.05;
        ui.painter().rect_filled(
            spd_rect,
            CornerRadius::same(5),
            if spd_fast {
                active_color
            } else {
                inactive_color
            },
        );
        let spd_label = format!("{:.1}x", speed).replace(".0x", "x");
        ui.painter().text(
            spd_rect.center(),
            Align2::CENTER_CENTER,
            spd_label,
            FontId::proportional(10.0),
            if spd_fast {
                Color32::WHITE
            } else {
                active_color
            },
        );
        let spd_resp = ui.interact(
            spd_rect,
            Id::unique(("tg_voice_speed", msg_id.0)),
            Sense::click(),
        );
        if spd_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if spd_resp
            .on_hover_text("Cycle voice playback speed")
            .clicked()
            && let Some(rt) = runtime
        {
            rt.cycle_audio_speed(tg_audio::AudioType::Voice);
        }
    }

    // Voice-to-text transcribe chip button ("→A") + pending spinner + expandable transcript
    // (`history_view_transcribe_button.cpp` & `api_transcribes.cpp`).
    let live_entry = voice_state.transcriptions.get(&msg_id).cloned();
    let limits = voice_state.transcribe_limits;
    let is_pending = live_entry.as_ref().is_some_and(|e| e.pending);
    let expanded = if let Some(ref e) = live_entry {
        e.shown
    } else {
        voice_state
            .transcription_expanded
            .get(&msg_id)
            .copied()
            .unwrap_or(false)
    };
    // `voice->transcribe->paint(p, nameleft + namewidth + st::historyTranscribeSkip, top)`.
    let chip_rect = Rect::from_min_size(
        Pos2::new(
            wave_left + (name_width + HISTORY_TRANSCRIBE_SKIP) as f32,
            wave_top,
        ),
        Vec2::new(
            HISTORY_TRANSCRIBE_SIZE.width as f32,
            HISTORY_TRANSCRIBE_SIZE.height as f32,
        ),
    );
    let has_lock = limits.locked && live_entry.is_none() && voice.transcription.is_none();
    ui.painter().rect_filled(
        chip_rect,
        CornerRadius::same(4),
        if expanded {
            active_color
        } else {
            inactive_color
        },
    );
    if is_pending {
        paint_transcribe_pending_ring(ui, chip_rect, active_color, false);
    }
    let chip_fg = if expanded {
        Color32::WHITE
    } else {
        active_color
    };
    if expanded && !is_pending {
        tg_app_core::ui_icons::paint_triangle(
            ui.painter(),
            chip_rect.center(),
            9.0,
            tg_app_core::ui_icons::ArrowDir::Up,
            chip_fg,
        );
    } else if has_lock {
        let c = chip_rect.center();
        tg_app_core::ui_icons::paint_arrow(
            ui.painter(),
            Pos2::new(c.x - 9.0, c.y),
            8.0,
            tg_app_core::ui_icons::ArrowDir::Right,
            chip_fg,
        );
        ui.painter().text(
            Pos2::new(c.x - 1.0, c.y),
            Align2::CENTER_CENTER,
            "A",
            FontId::proportional(10.0),
            chip_fg,
        );
        tg_app_core::ui_icons::paint_lock(ui.painter(), Pos2::new(c.x + 8.5, c.y), 9.5, chip_fg);
    } else {
        let c = chip_rect.center();
        tg_app_core::ui_icons::paint_arrow(
            ui.painter(),
            Pos2::new(c.x - 5.5, c.y),
            9.0,
            tg_app_core::ui_icons::ArrowDir::Right,
            chip_fg,
        );
        ui.painter().text(
            Pos2::new(c.x + 5.0, c.y),
            Align2::CENTER_CENTER,
            "A",
            FontId::proportional(10.5),
            chip_fg,
        );
    }
    let hover_tip = if has_lock {
        "Voice-to-Text (Telegram Premium or wait for weekly trial reset)"
    } else if !limits.is_premium && limits.trials_remaining > 0 && live_entry.is_none() {
        "Transcribe voice message (free trial available)"
    } else if expanded {
        "Hide transcription"
    } else {
        "Transcribe voice message"
    };
    let tr_resp = ui.interact(
        chip_rect,
        Id::unique(("tg_voice_tr", msg_id.0)),
        Sense::click(),
    );
    if tr_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if tr_resp.on_hover_text(hover_tip).clicked() {
        if live_entry.is_none() && voice.transcription.is_some() {
            voice_state.transcription_expanded.insert(msg_id, !expanded);
        }
        return Some(MediaInteraction::ToggleTranscribe {
            msg_id,
            duration_secs: voice.duration_secs,
            is_round_video: false,
        });
    }

    if expanded {
        let tr_y = rect.top() + st.padding.top as f32 + btn_size + 6.0;
        let tr_w = (rect.width() - MSG_PADDING.horizontal() as f32).max(60.0);
        if let Some(rate_ev) = paint_expanded_transcript(
            ui,
            msg_id,
            Pos2::new(rect.left() + MSG_PADDING.left as f32, tr_y),
            tr_w,
            live_entry.as_ref(),
            voice.transcription.as_deref(),
            colors,
            active_color,
        ) {
            return Some(rate_ev);
        }
    }

    None
}

fn paint_transcribe_pending_ring(ui: &Ui, rect: Rect, color: Color32, circular: bool) {
    let time = ui.ctx().input(|i| i.time);
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));
    let angle = (time * 4.2) % std::f64::consts::TAU;
    let center = rect.center();
    let r = if circular {
        rect.width().min(rect.height()) * 0.5 - 1.5
    } else {
        rect.height() * 0.5 - 1.5
    };
    let segs = 12usize;
    let arc_len = std::f64::consts::PI * 1.15;
    let mut pts = Vec::with_capacity(segs + 1);
    for i in 0..=segs {
        let a = angle + arc_len * (i as f64 / segs as f64);
        let px = center.x + (a.cos() as f32) * if circular { r } else { rect.width() * 0.45 };
        let py = center.y + (a.sin() as f32) * r;
        pts.push(Pos2::new(px, py));
    }
    ui.painter().add(Shape::line(pts, Stroke::new(2.0, color)));
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_expanded_transcript(
    ui: &mut Ui,
    msg_id: MsgId,
    pos: Pos2,
    max_w: f32,
    live_entry: Option<&TranscribeUiEntry>,
    fallback_text: Option<&str>,
    colors: &HistoryColors,
    accent: Color32,
) -> Option<MediaInteraction> {
    let time = ui.ctx().input(|i| i.time);
    let dots = match ((time * 3.0) as usize) % 4 {
        0 => "",
        1 => ".",
        2 => "..",
        _ => "...",
    };
    let (display_text, is_error, can_rate) = if let Some(entry) = live_entry {
        if entry.failed || entry.too_long {
            (
                tg_app_core::user_error::transcribe_bubble_text(entry.too_long),
                true,
                false,
            )
        } else if entry.pending {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(120));
            let base = if entry.text.is_empty() {
                format!("Transcribing{dots}")
            } else {
                format!("{} {dots}", entry.text)
            };
            (base, false, false)
        } else {
            (entry.text.clone(), false, entry.can_rate)
        }
    } else {
        (fallback_text?.to_owned(), false, false)
    };

    // A failure is italic in the message text colour (`tr::italic`,
    // `history_view_document.cpp:533-538`).
    let text_col = colors.text_fg;
    let galley = ui
        .painter()
        .layout_job(transcript_job(display_text, is_error, text_col, max_w));
    let text_h = galley.size().y;
    ui.painter().galley(pos, galley, text_col);

    if can_rate {
        let rate_y = pos.y + text_h + 4.0;
        let up_rect = Rect::from_min_size(Pos2::new(pos.x, rate_y), Vec2::new(24.0, 18.0));
        let dn_rect = Rect::from_min_size(Pos2::new(pos.x + 28.0, rate_y), Vec2::new(24.0, 18.0));
        for (btn_rect, label, good, id_tag) in [
            (up_rect, "👍", true, "tg_tr_rate_up"),
            (dn_rect, "👎", false, "tg_tr_rate_dn"),
        ] {
            let r = ui.interact(btn_rect, Id::unique((id_tag, msg_id.0)), Sense::click());
            if r.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().rect_filled(
                    btn_rect,
                    CornerRadius::same(4),
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 30),
                );
            }
            ui.painter().text(
                btn_rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(11.0),
                colors.sub_text_fg,
            );
            if r.on_hover_text(if good {
                "Good transcription"
            } else {
                "Poor transcription"
            })
            .clicked()
            {
                return Some(MediaInteraction::RateTranscribe { msg_id, good });
            }
        }
    }
    None
}

/// The layout of a transcript: italic for a failure text.
pub(crate) fn transcript_job(
    text: String,
    italic: bool,
    color: Color32,
    max_w: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::single_section(
        text,
        egui::TextFormat {
            font_id: FontId::proportional(12.5),
            color,
            italics: italic,
            ..egui::TextFormat::default()
        },
    );
    job.wrap.max_width = max_w;
    job
}

fn round_transcribe_key(msg_id: MsgId) -> Id {
    Id::unique(("tg_round_transcribe_rect", msg_id.0))
}

/// Records where the message painter placed a round video's transcribe button for this frame
/// (it sits outside the media rect, next to the info pill and right action).
pub(crate) fn set_round_transcribe_rect(ctx: &egui::Context, msg_id: MsgId, rect: Rect) {
    ctx.data_mut(|d| d.insert_temp(round_transcribe_key(msg_id), rect));
}

fn round_transcribe_rect(ctx: &egui::Context, msg_id: MsgId) -> Option<Rect> {
    let key = round_transcribe_key(msg_id);
    ctx.data_mut(|d| {
        let rect = d.get_temp::<Rect>(key);
        d.remove::<Rect>(key);
        rect
    })
}

fn paint_round_video_transcribe_overlay(
    ui: &mut Ui,
    msg_id: MsgId,
    duration_secs: u32,
    rect: Rect,
    colors: &HistoryColors,
    voice_state: &VoicePlaybackState,
) -> Option<MediaInteraction> {
    let entry = voice_state.transcriptions.get(&msg_id).cloned();
    let limits = voice_state.transcribe_limits;
    let is_pending = entry.as_ref().is_some_and(|e| e.pending);
    let expanded = entry.as_ref().is_some_and(|e| e.shown);
    let has_lock = limits.locked && entry.is_none();

    let diameter = rect.width().min(rect.height()).max(1.0);
    let radius = diameter * 0.5;
    let circle_center = rect.min + Vec2::splat(radius);
    let btn_size = 31.0;
    let btn_rect = round_transcribe_rect(ui.ctx(), msg_id).unwrap_or_else(|| {
        Rect::from_center_size(
            Pos2::new(
                (circle_center.x + radius - 14.0).min(rect.right() - 16.0),
                circle_center.y + radius - 14.0,
            ),
            Vec2::splat(btn_size),
        )
    });
    let btn_center = btn_rect.center();

    ui.painter()
        .circle_filled(btn_center, btn_size * 0.5, colors.service_bg);
    if is_pending {
        paint_transcribe_pending_ring(ui, btn_rect, Color32::WHITE, true);
    }
    if expanded && !is_pending {
        tg_app_core::ui_icons::paint_triangle(
            ui.painter(),
            btn_center,
            9.0,
            tg_app_core::ui_icons::ArrowDir::Up,
            Color32::WHITE,
        );
    } else if has_lock {
        tg_app_core::ui_icons::paint_arrow(
            ui.painter(),
            Pos2::new(btn_center.x - 8.5, btn_center.y),
            7.5,
            tg_app_core::ui_icons::ArrowDir::Right,
            Color32::WHITE,
        );
        ui.painter().text(
            Pos2::new(btn_center.x - 1.0, btn_center.y),
            Align2::CENTER_CENTER,
            "A",
            FontId::proportional(9.5),
            Color32::WHITE,
        );
        tg_app_core::ui_icons::paint_lock(
            ui.painter(),
            Pos2::new(btn_center.x + 8.0, btn_center.y),
            9.0,
            Color32::WHITE,
        );
    } else {
        tg_app_core::ui_icons::paint_arrow(
            ui.painter(),
            Pos2::new(btn_center.x - 5.0, btn_center.y),
            8.5,
            tg_app_core::ui_icons::ArrowDir::Right,
            Color32::WHITE,
        );
        ui.painter().text(
            Pos2::new(btn_center.x + 4.5, btn_center.y),
            Align2::CENTER_CENTER,
            "A",
            FontId::proportional(10.5),
            Color32::WHITE,
        );
    }
    let resp = ui.interact(
        btn_rect,
        Id::unique(("tg_round_tr_btn", msg_id.0)),
        Sense::click(),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.on_hover_text("Transcribe video message").clicked() {
        return Some(MediaInteraction::ToggleTranscribe {
            msg_id,
            duration_secs,
            is_round_video: true,
        });
    }

    if expanded && let Some(ref e) = entry {
        let card_top = circle_center.y + radius + 6.0;
        let card_w = rect.width().max(180.0);
        let card_rect = Rect::from_min_size(
            Pos2::new(rect.left(), card_top),
            Vec2::new(card_w, (rect.bottom() - card_top).max(36.0)),
        );
        ui.painter()
            .rect_filled(card_rect, CornerRadius::same(10), colors.msg_in_bg);
        if let Some(rate_ev) = paint_expanded_transcript(
            ui,
            msg_id,
            Pos2::new(card_rect.left() + 8.0, card_rect.top() + 6.0),
            (card_rect.width() - 16.0).max(60.0),
            Some(e),
            None,
            colors,
            colors.accent,
        ) {
            return Some(rate_ev);
        }
    }
    None
}

fn paint_sticker(
    ui: &mut Ui,
    msg_id: MsgId,
    sticker: &StickerAttachment,
    rect: Rect,
    assets: &mut MediaAssetCache,
    mut runtime: Option<&mut HistoryMediaRuntime>,
) -> Option<MediaInteraction> {
    let time = ui.ctx().input(|i| i.time);
    let now_ms = (time * 1000.0).max(0.0) as i64;
    let render_px = rect.width().max(rect.height()).round() as u32;

    let mut painted = false;
    let is_video_sticker = runtime
        .as_deref()
        .and_then(|rt| rt.descriptor(msg_id))
        .is_some_and(|d| d.is_video_sticker);

    // 1. WebM VP9 alpha video sticker via `tg-video` (`InlinePlaybackKind::VideoSticker`).
    if is_video_sticker
        && let Some(rt) = runtime.as_deref_mut()
        && let Some(frame_state) = rt.inline_clip_frame(
            ui.ctx(),
            msg_id,
            InlinePlaybackKind::VideoSticker,
            Some((render_px.clamp(64, 256), render_px.clamp(64, 256))),
            now_ms,
        )
        && let Some(tex) = frame_state.texture
    {
        ui.painter().image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        painted = true;
    }

    // 2. Animated Lottie sticker cached per size via `HistoryMediaRuntime` (`tg-lottie`),
    //    parsed and rasterised on the decode workers.
    if !painted && let Some(rt) = runtime.as_deref_mut() {
        match rt.lottie_frame_per_size(
            ui.ctx(),
            msg_id,
            sticker.sticker_id,
            &sticker.lottie_json,
            time,
            render_px,
        ) {
            LottieFrameState::Ready { texture, animated } => {
                ui.painter().image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                if animated {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(33));
                }
                painted = true;
            }
            // The first frame is still being rendered in the background; it appears on the
            // frame it lands (the worker requests the repaint).
            LottieFrameState::Pending => painted = true,
            LottieFrameState::Unavailable => {}
        }
    }

    if !painted {
        if let Some((tex, animated)) = assets.get_lottie_frame(ui.ctx(), sticker, time, render_px) {
            ui.painter().image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            if animated {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(33));
            }
        } else {
            // Fallback crisp vector badge if Lottie JSON fails.
            let tex = assets.sticker_fallback(ui.ctx()).clone();
            ui.painter().image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    let resp = ui.interact(rect, Id::unique(("tg_sticker", msg_id.0)), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp
        .on_hover_text(format!("{} — {}", sticker.set_title, sticker.emoji))
        .clicked()
    {
        if let Some(rt) = runtime
            && !rt.policy.allows_lottie_loop()
        {
            rt.restart_lottie(msg_id, time);
        }
        return Some(MediaInteraction::OpenMedia {
            msg_id,
            album_index: None,
        });
    }
    None
}

fn paint_poll(
    ui: &mut Ui,
    msg_id: MsgId,
    poll: &PollAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let left = rect.left() + MSG_PADDING.left as f32;
    let inner_w = (rect.width() - MSG_PADDING.horizontal() as f32).max(80.0);
    let accent = if outgoing && !colors.is_night {
        colors.wave_out_active
    } else {
        colors.accent
    };
    let sub_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.sub_text_fg
    };

    let mut y = rect.top() + HISTORY_POLL_QUESTION_TOP as f32;

    // Question
    let q_galley = ui.painter().layout(
        poll.question.clone(),
        FontId::proportional(14.0),
        colors.bold_fg,
        inner_w,
    );
    let q_h = q_galley.size().y;
    ui.painter()
        .galley(Pos2::new(left, y), q_galley, colors.bold_fg);
    y += q_h + HISTORY_POLL_SUBTITLE_SKIP as f32;

    // Subtitle ("Anonymous Poll" / "Quiz")
    let subtitle = match (poll.is_quiz, poll.is_anonymous, poll.closed) {
        (true, _, true) => "Quiz (Closed)",
        (true, true, false) => "Anonymous Quiz",
        (true, false, false) => "Quiz",
        (false, _, true) => "Final Results",
        (false, true, false) => "Anonymous Poll",
        (false, false, false) => "Poll",
    };
    ui.painter().text(
        Pos2::new(left, y),
        Align2::LEFT_TOP,
        subtitle,
        FontId::proportional(12.0),
        sub_fg,
    );
    y += 18.0 + HISTORY_POLL_ANSWERS_SKIP as f32;

    let show_results = poll.closed || poll.options.iter().any(|o| o.chosen);
    let total_votes: u32 = poll.options.iter().map(|o| o.votes).sum::<u32>().max(1);
    let mut voted_option = None;

    for (idx, opt) in poll.options.iter().enumerate() {
        let pct = ((f64::from(opt.votes) / f64::from(total_votes)) * 100.0).round() as u32;
        let text_left = left + 32.0;
        let text_w = (inner_w - 32.0).max(40.0);
        let opt_galley = ui.painter().layout(
            opt.text.clone(),
            FontId::proportional(13.0),
            colors.text_fg,
            text_w,
        );
        let row_h = (opt_galley.size().y + 18.0).max(36.0);
        let row_rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(inner_w, row_h));

        let resp = ui.interact(
            row_rect,
            Id::unique(("tg_poll_opt", msg_id.0, idx)),
            Sense::click(),
        );
        if resp.hovered() && !poll.closed {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            ui.painter().rect_filled(
                row_rect,
                CornerRadius::same(6),
                Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18),
            );
        }

        // Left column: percentage or radio circle
        let left_center = Pos2::new(left + 13.0, row_rect.center().y - 2.0);
        if show_results {
            ui.painter().text(
                left_center,
                Align2::CENTER_CENTER,
                format!("{pct}%"),
                FontId::proportional(11.5),
                if opt.chosen { accent } else { colors.bold_fg },
            );
            if opt.chosen {
                ui.painter().circle_filled(
                    Pos2::new(left + 13.0, row_rect.bottom() - 8.0),
                    4.0,
                    accent,
                );
            }
        } else {
            ui.painter()
                .circle_stroke(left_center, 8.0, Stroke::new(1.5, sub_fg));
        }

        // Option text
        ui.painter()
            .galley(Pos2::new(text_left, y + 5.0), opt_galley, colors.text_fg);

        // Progress bar track
        let bar_y = row_rect.bottom() - 7.0;
        let bar_track = Rect::from_min_size(Pos2::new(text_left, bar_y), Vec2::new(text_w, 4.0));
        ui.painter().rect_filled(
            bar_track,
            CornerRadius::same(2),
            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 40),
        );
        if show_results {
            let fill_w = (bar_track.width() * (pct as f32 / 100.0)).max(if opt.votes > 0 {
                6.0
            } else {
                0.0
            });
            let bar_fill = Rect::from_min_size(bar_track.min, Vec2::new(fill_w, 4.0));
            ui.painter()
                .rect_filled(bar_fill, CornerRadius::same(2), accent);
        }

        if resp.clicked() && !poll.closed {
            voted_option = Some(idx);
        }

        y += row_h + 2.0;
    }

    y += HISTORY_POLL_TOTAL_VOTES_SKIP as f32;
    let footer = if !poll.is_anonymous && poll.total_voters > 0 {
        if poll.total_voters == 1 {
            "1 vote · View results".to_owned()
        } else {
            format!("{} votes · View results", poll.total_voters)
        }
    } else if poll.total_voters == 1 {
        "1 vote".to_owned()
    } else {
        format!("{} votes", poll.total_voters)
    };
    let footer_rect = Rect::from_min_size(Pos2::new(left, y), Vec2::new(inner_w, 18.0));
    let footer_color = if !poll.is_anonymous && poll.total_voters > 0 {
        accent
    } else {
        sub_fg
    };
    ui.painter().text(
        Pos2::new(left, y),
        Align2::LEFT_TOP,
        footer,
        FontId::proportional(12.0),
        footer_color,
    );
    let mut open_voters = false;
    if !poll.is_anonymous && poll.total_voters > 0 {
        let f_resp = ui.interact(
            footer_rect,
            Id::unique(("tg_poll_voters_btn", msg_id.0)),
            Sense::click(),
        );
        if f_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if f_resp.on_hover_text("View poll voters").clicked() {
            open_voters = true;
        }
    }

    // Show quiz explanation banner once the user has voted in quiz mode.
    if poll.is_quiz
        && show_results
        && let Some(ref expl) = poll.explanation
        && !expl.is_empty()
    {
        tg_app_core::ui_icons::paint_style_tinted(
            ui.painter(),
            Rect::from_center_size(Pos2::new(left + 78.0, y + 6.5), Vec2::splat(13.0)),
            tg_ui::tg_style::st::menu_icon_info,
            accent,
        );
        ui.painter().text(
            Pos2::new(left + 88.0, y),
            Align2::LEFT_TOP,
            expl,
            FontId::proportional(11.5),
            accent,
        );
    }

    // Subtle border separator if poll sits inside bubble.
    let _ = StrokeKind::Inside;

    if open_voters {
        return Some(MediaInteraction::OpenPollVoters(msg_id));
    }
    voted_option.map(|option_index| MediaInteraction::VotePoll {
        msg_id,
        option_index,
    })
}

fn paint_location(
    ui: &mut Ui,
    msg_id: MsgId,
    loc: &crate::history::sections_model::LocationAttachment,
    rect: Rect,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let map_h = (rect.height() - 42.0).max(96.0);
    let map_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), map_h));
    let bg = if colors.is_night {
        Color32::from_rgb(0x23, 0x33, 0x42)
    } else {
        Color32::from_rgb(0xE4, 0xEE, 0xE5)
    };
    ui.painter()
        .rect_filled(map_rect, CornerRadius::same(8), bg);
    ui.painter()
        .circle_filled(map_rect.center(), 10.0, colors.accent);
    ui.painter()
        .circle_filled(map_rect.center(), 4.0, Color32::WHITE);

    let title = loc
        .venue_title
        .clone()
        .unwrap_or_else(|| format!("Location ({:.4}, {:.4})", loc.lat, loc.lon));
    let sub = loc.venue_address.clone().unwrap_or_else(|| {
        if let Some(secs) = loc.live_period_secs {
            format!("Live location · {} min", secs / 60)
        } else {
            "Static map pin".to_owned()
        }
    });
    ui.painter().text(
        Pos2::new(rect.left() + 10.0, map_rect.bottom() + 6.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(13.0),
        colors.bold_fg,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 10.0, map_rect.bottom() + 23.0),
        Align2::LEFT_TOP,
        sub,
        FontId::proportional(11.5),
        colors.sub_text_fg,
    );
    let resp = ui.interact(rect, Id::unique(("tg_loc_media", msg_id.0)), Sense::click());
    if resp.clicked() {
        return Some(MediaInteraction::OpenMedia {
            msg_id,
            album_index: None,
        });
    }
    None
}

fn paint_contact(
    ui: &mut Ui,
    msg_id: MsgId,
    contact: &crate::history::sections_model::ContactAttachment,
    rect: Rect,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let av_c = Pos2::new(rect.left() + 28.0, rect.center().y);
    ui.painter().circle_filled(av_c, 20.0, colors.accent);
    let initial = contact
        .first_name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    ui.painter().text(
        av_c,
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(15.0),
        Color32::WHITE,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 56.0, rect.top() + 10.0),
        Align2::LEFT_TOP,
        contact.full_name(),
        FontId::proportional(13.5),
        colors.bold_fg,
    );
    ui.painter().text(
        Pos2::new(rect.left() + 56.0, rect.top() + 29.0),
        Align2::LEFT_TOP,
        &contact.phone_number,
        FontId::proportional(12.0),
        colors.sub_text_fg,
    );
    let resp = ui.interact(
        rect,
        Id::unique(("tg_contact_media", msg_id.0)),
        Sense::click(),
    );
    if resp.clicked() {
        return Some(MediaInteraction::OpenMedia {
            msg_id,
            album_index: None,
        });
    }
    None
}

fn paint_round_video(
    ui: &mut Ui,
    msg_id: MsgId,
    rv: &crate::history::sections_model::RoundVideoAttachment,
    rect: Rect,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let radius = (rect.width().min(rect.height()) * 0.48).max(40.0);
    let [r, g, b] = rv.accent_rgb.0;
    ui.painter()
        .circle_filled(rect.center(), radius, Color32::from_rgb(r, g, b));
    ui.painter()
        .circle_stroke(rect.center(), radius, Stroke::new(2.5, colors.accent));
    tg_app_core::ui_icons::paint_triangle(
        ui.painter(),
        rect.center() - Vec2::new(-2.0, 6.0),
        22.0,
        tg_app_core::ui_icons::ArrowDir::Right,
        Color32::WHITE,
    );
    ui.painter().text(
        rect.center() + Vec2::new(0.0, 20.0),
        Align2::CENTER_CENTER,
        format!("Video note · {}", format_duration(rv.duration_secs)),
        FontId::proportional(11.5),
        Color32::WHITE,
    );
    let resp = ui.interact(rect, Id::unique(("tg_round_vid", msg_id.0)), Sense::click());
    if resp.clicked() {
        return Some(MediaInteraction::OpenMedia {
            msg_id,
            album_index: None,
        });
    }
    None
}

/// Paints an HTML5 bot game preview card inside a message bubble.
///
/// Port of `HistoryView::Game::draw` (`Telegram/SourceFiles/history/view/media/history_view_game.cpp`):
/// tinted quote-style card with left accent bar, bold game title, wrapped description, embedded
/// cover photo/animation preview, and bottom-right `"GAME"` pill badge.
fn paint_game(
    ui: &mut Ui,
    msg_id: MsgId,
    att: &GameAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
    assets: &mut MediaAssetCache,
) -> Option<MediaInteraction> {
    if rect.width() < 40.0 || rect.height() < 40.0 {
        return None;
    }

    let outer = Rect::from_min_max(
        Pos2::new(rect.left() + 10.0, rect.top() + 4.0),
        Pos2::new(
            (rect.right() - 10.0).max(rect.left() + 30.0),
            (rect.bottom() - 8.0).max(rect.top() + 30.0),
        ),
    );
    let bar_color = if outgoing {
        colors.msg_out_reply_bar
    } else {
        colors.msg_in_reply_bar
    };
    let bg_color = if outgoing {
        colors.msg_out_reply_bg
    } else {
        colors.msg_in_reply_bg
    };

    ui.painter()
        .rect_filled(outer, CornerRadius::same(6), bg_color);
    let accent_bar = Rect::from_min_size(outer.min, Vec2::new(3.0, outer.height()));
    ui.painter()
        .rect_filled(accent_bar, CornerRadius::same(2), bar_color);

    let inner_left = outer.left() + 11.0;
    let inner_w = (outer.width() - 19.0).max(60.0);
    let mut y = outer.top() + 6.0;

    // 1. Game title (`st::webPageTitleStyle`, accent color, up to 2 lines).
    if !att.game.title.is_empty() {
        let title_font = FontId::new(13.5, tg_ui::fonts::family_semibold());
        let title_galley =
            ui.painter()
                .layout(att.game.title.clone(), title_font, bar_color, inner_w);
        let title_h = title_galley.size().y.min(36.0);
        ui.painter()
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(inner_left, y),
                Vec2::new(inner_w, title_h + 2.0),
            ))
            .galley(Pos2::new(inner_left, y), title_galley, bar_color);
        y += title_h + 3.0;
    }

    // 2. Game description (`st::webPageDescriptionStyle`).
    if !att.game.description.is_empty() {
        let desc_font = FontId::proportional(12.5);
        let desc_galley = ui.painter().layout(
            att.game.description.clone(),
            desc_font,
            colors.text_fg,
            inner_w,
        );
        let max_desc_h = (outer.bottom() - y - 92.0).max(20.0);
        let desc_h = desc_galley.size().y.min(max_desc_h);
        ui.painter()
            .with_clip_rect(Rect::from_min_size(
                Pos2::new(inner_left, y),
                Vec2::new(inner_w, desc_h + 2.0),
            ))
            .galley(Pos2::new(inner_left, y), desc_galley, colors.text_fg);
        y += desc_h + 6.0;
    }

    // 3. Embedded cover photo / animation preview + bottom-right "GAME" tag.
    let attach_h = (outer.bottom() - 8.0 - y).max(0.0);
    if attach_h >= 40.0 {
        let attach_rect =
            Rect::from_min_size(Pos2::new(inner_left, y), Vec2::new(inner_w, attach_h));
        let cr = CornerRadius::same(6);
        paint_photo_surface(
            ui.ctx(),
            ui.painter(),
            attach_rect,
            cr,
            att.game.id.0.wrapping_mul(73) ^ (msg_id.0 as u64),
            att.stripped_thumb.as_deref(),
            att.fallback_rgb,
            assets,
        );

        // Crisp procedural arcade emblem (starfield + orbital glider; subtly animates if `is_animation`).
        let t = if att.is_animation {
            ui.ctx().input(|i| i.time) as f32
        } else {
            0.0
        };
        let center = attach_rect.center();
        let bob = if att.is_animation {
            (t * 2.2).sin() * 4.0
        } else {
            0.0
        };
        for (i, &(fx, fy, r)) in [
            (0.18_f32, 0.24_f32, 2.0_f32),
            (0.82, 0.22, 2.5),
            (0.74, 0.68, 1.8),
            (0.24, 0.72, 2.2),
            (0.56, 0.18, 1.6),
        ]
        .iter()
        .enumerate()
        {
            let twinkle = if att.is_animation {
                let phase = ((t * 2.5 + i as f32 * 1.1).sin() * 0.5 + 0.5) * 120.0;
                (120.0 + phase) as u8
            } else {
                200
            };
            ui.painter().circle_filled(
                Pos2::new(
                    attach_rect.left() + attach_rect.width() * fx,
                    attach_rect.top() + attach_rect.height() * fy,
                ),
                r,
                Color32::from_rgba_unmultiplied(255, 255, 255, twinkle),
            );
        }
        // Center play badge over the game artwork.
        let play_center = Pos2::new(center.x, center.y + bob);
        paint_play_circle(ui.painter(), play_center, 20.0);

        if att.is_animation {
            paint_video_badge(ui.painter(), attach_rect, "GIF");
        }

        // Bottom-right "GAME" pill badge (`history_view_game.cpp:322-331`).
        let tag_font = FontId::proportional(10.5);
        let tag_galley = ui
            .painter()
            .layout_no_wrap("GAME".to_owned(), tag_font, Color32::WHITE);
        let tag_size = Vec2::new(tag_galley.size().x + 12.0, tag_galley.size().y + 4.0);
        let tag_rect = Rect::from_min_size(
            Pos2::new(
                attach_rect.right() - 6.0 - tag_size.x,
                attach_rect.bottom() - 6.0 - tag_size.y,
            ),
            tag_size,
        );
        ui.painter()
            .rect_filled(tag_rect, CornerRadius::same(8), colors.date_img_bg);
        ui.painter().galley(
            Pos2::new(tag_rect.left() + 6.0, tag_rect.top() + 2.0),
            tag_galley,
            Color32::WHITE,
        );
    }

    let resp = ui.interact(
        outer,
        Id::unique(("tg_game_media", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, att.game.title.clone())
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        return Some(MediaInteraction::PlayGame(msg_id));
    }
    None
}

/// Paints a sandboxed HTML widget bubble (`HistoryView::HtmlWidget`, `history_view_html_widget.cpp`).
fn paint_widget(
    ui: &mut Ui,
    msg_id: MsgId,
    att: &WidgetAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let painter = ui.painter();
    let cr = CornerRadius::same(10);
    let accent = if outgoing {
        colors.msg_out_reply_bar
    } else {
        colors.accent
    };
    let card_bg = if outgoing {
        colors.msg_out_reply_bg
    } else {
        colors.msg_in_reply_bg
    };
    let text_fg = colors.text_fg;
    let sub_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.msg_in_date_fg
    };

    painter.rect_filled(rect, cr, card_bg);
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0, accent.linear_multiply(0.45)),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(10.0);
    let header_y = inner.top();

    // Header badge + title.
    let badge_text = if att.html.is_some() {
        "HTML WIDGET"
    } else {
        "WIDGET STOPPED"
    };
    let badge_galley = painter.layout_no_wrap(
        badge_text.to_owned(),
        FontId::proportional(10.0),
        Color32::WHITE,
    );
    let badge_size = Vec2::new(badge_galley.size().x + 10.0, badge_galley.size().y + 4.0);
    let badge_rect = Rect::from_min_size(Pos2::new(inner.left(), header_y), badge_size);
    let badge_color = if att.html.is_some() {
        accent
    } else {
        Color32::from_rgb(0xC6, 0x3A, 0x3A)
    };
    painter.rect_filled(badge_rect, CornerRadius::same(6), badge_color);
    painter.galley(
        Pos2::new(badge_rect.left() + 5.0, badge_rect.top() + 2.0),
        badge_galley,
        Color32::WHITE,
    );

    // Optional capability/copy pill on top-right.
    let cap_label = if att.is_copy {
        "Copy"
    } else if att.can_query {
        "Interactive"
    } else {
        "Sandboxed"
    };
    let cap_galley =
        painter.layout_no_wrap(cap_label.to_owned(), FontId::proportional(10.0), sub_fg);
    painter.galley(
        Pos2::new(inner.right() - cap_galley.size().x, header_y + 2.0),
        cap_galley,
        sub_fg,
    );

    let title = if att.title.is_empty() {
        "Interactive Widget"
    } else {
        att.title.as_str()
    };
    let title_y = badge_rect.bottom() + 6.0;
    let title_galley = painter.layout(
        title.to_owned(),
        FontId::proportional(14.0),
        text_fg,
        inner.width().max(60.0),
    );
    let title_h = title_galley.size().y;
    painter.galley(Pos2::new(inner.left(), title_y), title_galley, text_fg);

    // Preview body or stopped status pill (`HtmlWidget::drawStopped`).
    let body_y = title_y + title_h + 4.0;
    let btn_h = 28.0;
    let body_bottom = (inner.bottom() - btn_h - 6.0).max(body_y);
    if att.html.is_some() {
        let preview = if !att.preview_text.is_empty() {
            att.preview_text.clone()
        } else if let Some(ref pub_data) = att.public_data {
            format!("State: {pub_data}")
        } else {
            format!("{} bytes sandboxed HTML", att.payload.len())
        };
        let preview_galley = painter.layout(
            preview,
            FontId::proportional(12.0),
            sub_fg,
            inner.width().max(60.0),
        );
        let clip = Rect::from_min_max(
            Pos2::new(inner.left(), body_y),
            Pos2::new(inner.right(), body_bottom),
        );
        painter.with_clip_rect(clip).galley(
            Pos2::new(inner.left(), body_y),
            preview_galley,
            sub_fg,
        );
    } else {
        let stopped_galley = painter.layout_no_wrap(
            "Widget payload could not be unpacked".to_owned(),
            FontId::proportional(11.5),
            Color32::WHITE,
        );
        let pill_size = Vec2::new(
            (stopped_galley.size().x + 16.0).min(inner.width()),
            stopped_galley.size().y + 6.0,
        );
        let pill_rect = Rect::from_center_size(
            Pos2::new(inner.center().x, (body_y + body_bottom) * 0.5),
            pill_size,
        );
        painter.rect_filled(pill_rect, CornerRadius::same(8), colors.date_img_bg);
        painter.galley(
            Pos2::new(pill_rect.left() + 8.0, pill_rect.top() + 3.0),
            stopped_galley,
            Color32::WHITE,
        );
    }

    // Launch / Open Widget action bar at bottom.
    let btn_rect = Rect::from_min_max(
        Pos2::new(inner.left(), (inner.bottom() - btn_h).max(body_y)),
        Pos2::new(inner.right(), inner.bottom()),
    );
    painter.rect_filled(
        btn_rect,
        CornerRadius::same(6),
        accent.linear_multiply(0.18),
    );
    let btn_label = if att.html.is_some() {
        "Open Sandboxed Widget"
    } else {
        "Inspect Widget"
    };
    let btn_galley =
        painter.layout_no_wrap(btn_label.to_owned(), FontId::proportional(12.0), accent);
    painter.galley(
        Pos2::new(
            btn_rect.center().x - btn_galley.size().x * 0.5,
            btn_rect.center().y - btn_galley.size().y * 0.5,
        ),
        btn_galley,
        accent,
    );

    let resp = ui.interact(
        rect,
        Id::unique(("tg_widget_media", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, title.to_owned()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        return Some(MediaInteraction::OpenWidget(msg_id));
    }
    None
}

/// Paints an interactive Story share / mention card (`HistoryView::StoryMention`,
/// `history/view/media/history_view_story_mention.cpp`).
fn paint_story_card(
    ui: &mut Ui,
    msg_id: MsgId,
    att: &StoryAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let painter = ui.painter();
    let cr = CornerRadius::same(12);
    let accent = Color32::from_rgb(att.accent_rgb[0], att.accent_rgb[1], att.accent_rgb[2]);
    let card_bg = if outgoing {
        colors.msg_out_reply_bg
    } else {
        colors.msg_in_reply_bg
    };
    let text_fg = colors.text_fg;
    let sub_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.msg_in_date_fg
    };

    painter.rect_filled(rect, cr, card_bg);
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0, accent.linear_multiply(0.45)),
        StrokeKind::Inside,
    );

    // Circular story preview thumbnail with gradient-like unread ring (`StoryMention::draw`).
    let ring_center = Pos2::new(rect.left() + 42.0, rect.center().y);
    let ring_color = if att.expired {
        sub_fg
    } else {
        Color32::from_rgb(0x4E, 0xA8, 0xF5)
    };
    painter.circle_stroke(ring_center, 25.0, Stroke::new(2.5, ring_color));
    painter.circle_filled(ring_center, 21.0, accent);
    let initial = att
        .owner_name
        .chars()
        .next()
        .unwrap_or('S')
        .to_uppercase()
        .to_string();
    painter.text(
        ring_center,
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(17.0),
        Color32::WHITE,
    );

    let text_x = rect.left() + 78.0;
    let badge = if att.via_mention {
        "STORY MENTION"
    } else {
        "STORY"
    };
    painter.text(
        Pos2::new(text_x, rect.top() + 12.0),
        Align2::LEFT_TOP,
        badge,
        FontId::proportional(10.0),
        ring_color,
    );
    let title = format!("{}'s Story", att.owner_name);
    painter.text(
        Pos2::new(text_x, rect.top() + 26.0),
        Align2::LEFT_TOP,
        &title,
        FontId::proportional(14.0),
        text_fg,
    );
    let subtitle = if att.expired {
        format!("Story #{} · Expired", att.story_id)
    } else {
        format!("Story #{} · Tap to view", att.story_id)
    };
    painter.text(
        Pos2::new(text_x, rect.top() + 45.0),
        Align2::LEFT_TOP,
        subtitle,
        FontId::proportional(11.5),
        sub_fg,
    );

    // Bottom CTA pill ("View Story").
    let cta_rect = Rect::from_min_max(
        Pos2::new(text_x, rect.bottom() - 32.0),
        Pos2::new(
            (rect.right() - 12.0).max(text_x + 84.0),
            rect.bottom() - 8.0,
        ),
    );
    painter.rect_filled(
        cta_rect,
        CornerRadius::same(12),
        ring_color.linear_multiply(0.2),
    );
    painter.text(
        cta_rect.center(),
        Align2::CENTER_CENTER,
        "View Story",
        FontId::proportional(12.0),
        ring_color,
    );

    let resp = ui.interact(
        rect,
        Id::unique(("tg_story_media", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, title.clone()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        return Some(MediaInteraction::OpenStory {
            peer_id: att.peer_id,
            story_id: att.story_id,
        });
    }
    None
}

/// Paints an interactive Telegram Giveaway card (`GenerateGiveawayStart`,
/// `history/view/media/history_view_giveaway.cpp`).
#[allow(clippy::too_many_arguments)]
fn paint_giveaway_card(
    ui: &mut Ui,
    peer_id: PeerId,
    msg_id: MsgId,
    att: &GiveawayAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let painter = ui.painter();
    let cr = CornerRadius::same(12);
    let accent = if att.stars.is_some() {
        Color32::from_rgb(0xE6, 0x9F, 0x00)
    } else {
        Color32::from_rgb(0x7B, 0x5C, 0xF5)
    };
    let card_bg = if outgoing {
        colors.msg_out_reply_bg
    } else {
        colors.msg_in_reply_bg
    };
    let text_fg = colors.text_fg;
    let sub_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.msg_in_date_fg
    };

    painter.rect_filled(rect, cr, card_bg);
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0, accent.linear_multiply(0.5)),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(12.0);
    let mut y = inner.top();

    // Gift badge + prize quantity pill (`PushGiveawayBadge`).
    let badge_rect =
        Rect::from_min_size(Pos2::new(inner.center().x - 36.0, y), Vec2::new(72.0, 24.0));
    painter.rect_filled(badge_rect, CornerRadius::same(12), accent);
    if let Some(stars) = att.stars {
        let num = format!("{stars}");
        let galley = painter.layout_no_wrap(num, FontId::proportional(12.0), Color32::WHITE);
        let total_w = 12.0 + 4.0 + galley.size().x;
        let left_x = badge_rect.center().x - total_w * 0.5;
        tg_app_core::ui_icons::paint_star(
            painter,
            Pos2::new(left_x + 6.0, badge_rect.center().y),
            5.5,
            Color32::WHITE,
        );
        painter.galley(
            Pos2::new(left_x + 16.0, badge_rect.center().y - galley.size().y * 0.5),
            galley,
            Color32::WHITE,
        );
    } else {
        painter.text(
            badge_rect.center(),
            Align2::CENTER_CENTER,
            format!("×{}", att.quantity.max(1)),
            FontId::proportional(12.0),
            Color32::WHITE,
        );
    }
    y += 30.0;

    // Title + prize summary.
    let title = "Giveaway Prizes";
    painter.text(
        Pos2::new(inner.center().x, y),
        Align2::CENTER_TOP,
        title,
        FontId::proportional(14.5),
        text_fg,
    );
    y += 20.0;

    let prize_line = if let Some(stars) = att.stars {
        format!(
            "{stars} Telegram Stars among {} winners",
            att.quantity.max(1)
        )
    } else if let Some(months) = att.months {
        format!("{} × Telegram Premium ({months} mo)", att.quantity.max(1))
    } else {
        format!("{} Giveaway Prizes", att.quantity.max(1))
    };
    let prize_galley = painter.layout(
        prize_line,
        FontId::proportional(12.5),
        text_fg,
        inner.width(),
    );
    painter.galley(
        Pos2::new(inner.center().x - prize_galley.size().x * 0.5, y),
        prize_galley.clone(),
        text_fg,
    );
    y += prize_galley.size().y + 4.0;

    if let Some(ref extra) = att.prize_description
        && !extra.is_empty()
    {
        painter.text(
            Pos2::new(inner.center().x, y),
            Align2::CENTER_TOP,
            format!("+ {extra}"),
            FontId::proportional(11.5),
            accent,
        );
        y += 16.0;
    }

    // Participating channels summary (`PeerBubbleListPart`).
    if !att.channel_names.is_empty() {
        let ch_line = format!("Channels: {}", att.channel_names.join(", "));
        let ch_galley = painter.layout(ch_line, FontId::proportional(11.5), sub_fg, inner.width());
        painter.galley(
            Pos2::new(inner.center().x - ch_galley.size().x * 0.5, y),
            ch_galley.clone(),
            sub_fg,
        );
        y += ch_galley.size().y + 4.0;
    }

    // Eligibility + end date.
    let mut meta_parts = Vec::new();
    if att.only_new_subscribers {
        meta_parts.push("New subscribers only".to_owned());
    }
    if !att.countries_iso2.is_empty() {
        meta_parts.push(att.countries_iso2.join(", "));
    }
    if !att.until_date_text.is_empty() {
        meta_parts.push(format!("Ends {}", att.until_date_text));
    }
    if !meta_parts.is_empty() && y < inner.bottom() - 34.0 {
        painter.text(
            Pos2::new(inner.center().x, y),
            Align2::CENTER_TOP,
            meta_parts.join(" · "),
            FontId::proportional(11.0),
            sub_fg,
        );
    }

    // Interactive CTA button ("View Giveaway Info").
    let btn_rect = Rect::from_min_max(
        Pos2::new(inner.left(), (inner.bottom() - 28.0).max(y + 4.0)),
        Pos2::new(inner.right(), inner.bottom()),
    );
    painter.rect_filled(
        btn_rect,
        CornerRadius::same(8),
        accent.linear_multiply(0.18),
    );
    painter.text(
        btn_rect.center(),
        Align2::CENTER_CENTER,
        "View Giveaway & Boosts",
        FontId::proportional(12.0),
        accent,
    );

    let resp = ui.interact(
        rect,
        Id::unique(("tg_giveaway_media", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, "Telegram Giveaway".to_owned())
    });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        return Some(MediaInteraction::OpenGiveawayInfo {
            peer_id,
            msg_id,
            channel_id: att.channels.first().copied(),
            launch_msg_id: None,
        });
    }
    None
}

/// Paints an interactive Telegram Giveaway Results card (`GenerateGiveawayResults`,
/// `history/view/media/history_view_giveaway.cpp`).
#[allow(clippy::too_many_arguments)]
fn paint_giveaway_results_card(
    ui: &mut Ui,
    peer_id: PeerId,
    msg_id: MsgId,
    att: &GiveawayResultsAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let painter = ui.painter();
    let cr = CornerRadius::same(12);
    let accent = if att.refunded {
        Color32::from_rgb(0xC6, 0x4B, 0x4B)
    } else {
        Color32::from_rgb(0x2E, 0xA0, 0x56)
    };
    let card_bg = if outgoing {
        colors.msg_out_reply_bg
    } else {
        colors.msg_in_reply_bg
    };
    let text_fg = colors.text_fg;
    let sub_fg = if outgoing {
        colors.msg_out_date_fg
    } else {
        colors.msg_in_date_fg
    };

    painter.rect_filled(rect, cr, card_bg);
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0, accent.linear_multiply(0.5)),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(12.0);
    let mut y = inner.top();

    let badge_rect =
        Rect::from_min_size(Pos2::new(inner.center().x - 48.0, y), Vec2::new(96.0, 24.0));
    painter.rect_filled(badge_rect, CornerRadius::same(12), accent);
    if att.refunded {
        painter.text(
            badge_rect.center(),
            Align2::CENTER_CENTER,
            "REFUNDED",
            FontId::proportional(11.5),
            Color32::WHITE,
        );
    } else if let Some(stars) = att.stars {
        let galley = painter.layout_no_wrap(
            format!("{stars}"),
            FontId::proportional(11.5),
            Color32::WHITE,
        );
        let total_w = 13.0 + 4.0 + 11.0 + 3.0 + galley.size().x;
        let left_x = badge_rect.center().x - total_w * 0.5;
        tg_app_core::ui_icons::paint_trophy(
            painter,
            Pos2::new(left_x + 6.5, badge_rect.center().y),
            12.0,
            Color32::WHITE,
        );
        tg_app_core::ui_icons::paint_star(
            painter,
            Pos2::new(left_x + 22.0, badge_rect.center().y),
            5.0,
            Color32::WHITE,
        );
        painter.galley(
            Pos2::new(left_x + 31.0, badge_rect.center().y - galley.size().y * 0.5),
            galley,
            Color32::WHITE,
        );
    } else {
        let galley = painter.layout_no_wrap(
            format!("×{}", att.winners_count.max(0)),
            FontId::proportional(11.5),
            Color32::WHITE,
        );
        let total_w = 13.0 + 4.0 + galley.size().x;
        let left_x = badge_rect.center().x - total_w * 0.5;
        tg_app_core::ui_icons::paint_trophy(
            painter,
            Pos2::new(left_x + 6.5, badge_rect.center().y),
            12.0,
            Color32::WHITE,
        );
        painter.galley(
            Pos2::new(left_x + 17.0, badge_rect.center().y - galley.size().y * 0.5),
            galley,
            Color32::WHITE,
        );
    }
    y += 30.0;

    let heading = if att.refunded {
        "Giveaway Refunded".to_owned()
    } else if att.winners_count == 1 {
        "1 Winner Selected!".to_owned()
    } else {
        format!("{} Winners Selected!", att.winners_count.max(0))
    };
    painter.text(
        Pos2::new(inner.center().x, y),
        Align2::CENTER_TOP,
        &heading,
        FontId::proportional(14.5),
        text_fg,
    );
    y += 22.0;

    if !att.winners.is_empty() {
        let shown: Vec<&str> = att
            .winners
            .iter()
            .take(5)
            .map(|(_, name)| name.as_str())
            .collect();
        let remaining = (att.winners_count.max(0) as usize).saturating_sub(shown.len());
        let winners_line = if remaining > 0 {
            format!("Winners: {} +{remaining} more", shown.join(", "))
        } else {
            format!("Winners: {}", shown.join(", "))
        };
        let g = painter.layout(
            winners_line,
            FontId::proportional(12.0),
            sub_fg,
            inner.width(),
        );
        painter.galley(
            Pos2::new(inner.center().x - g.size().x * 0.5, y),
            g.clone(),
            sub_fg,
        );
        y += g.size().y + 4.0;
    }

    if att.unclaimed_count > 0 && y < inner.bottom() - 34.0 {
        painter.text(
            Pos2::new(inner.center().x, y),
            Align2::CENTER_TOP,
            format!("{} prizes unclaimed", att.unclaimed_count),
            FontId::proportional(11.0),
            sub_fg,
        );
    }

    let btn_rect = Rect::from_min_max(
        Pos2::new(inner.left(), (inner.bottom() - 28.0).max(y + 4.0)),
        Pos2::new(inner.right(), inner.bottom()),
    );
    painter.rect_filled(
        btn_rect,
        CornerRadius::same(8),
        accent.linear_multiply(0.18),
    );
    painter.text(
        btn_rect.center(),
        Align2::CENTER_CENTER,
        "View Giveaway Results",
        FontId::proportional(12.0),
        accent,
    );

    let resp = ui.interact(
        rect,
        Id::unique(("tg_giveaway_results_media", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, heading.clone()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        return Some(MediaInteraction::OpenGiveawayInfo {
            peer_id,
            msg_id,
            channel_id: Some(att.channel_id),
            launch_msg_id: Some(att.launch_msg_id),
        });
    }
    None
}

/// Paints an interactive Live Video Stream card (`MessageMedia::VideoStream`,
/// `media/view/media_view_video_stream.cpp`).
fn paint_video_stream_card(
    ui: &mut Ui,
    msg_id: MsgId,
    att: &VideoStreamAttachment,
    rect: Rect,
    outgoing: bool,
    colors: &HistoryColors,
) -> Option<MediaInteraction> {
    let painter = ui.painter();
    let cr = CornerRadius::same(12);
    let live_red = Color32::from_rgb(0xE5, 0x39, 0x35);
    let bg = Color32::from_rgb(0x17, 0x20, 0x2E);
    let _ = (outgoing, colors);

    painter.rect_filled(rect, cr, bg);
    painter.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0, live_red.linear_multiply(0.6)),
        StrokeKind::Inside,
    );

    let badge = if att.rtmp_stream {
        "LIVE · RTMP"
    } else {
        "LIVE STREAM"
    };
    let badge_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 12.0, rect.top() + 12.0),
        Vec2::new(92.0, 22.0),
    );
    painter.rect_filled(badge_rect, CornerRadius::same(11), live_red);
    painter.text(
        badge_rect.center(),
        Align2::CENTER_CENTER,
        badge,
        FontId::proportional(10.5),
        Color32::WHITE,
    );

    // Center play circle.
    let play_center = Pos2::new(rect.center().x, rect.center().y - 6.0);
    paint_play_circle(painter, play_center, 22.0);

    // Bottom title + CTA hint.
    let title = if att.title.is_empty() {
        "Live Video Stream"
    } else {
        att.title.as_str()
    };
    painter.text(
        Pos2::new(rect.left() + 12.0, rect.bottom() - 34.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(13.0),
        Color32::WHITE,
    );
    painter.text(
        Pos2::new(rect.left() + 12.0, rect.bottom() - 18.0),
        Align2::LEFT_TOP,
        "Click to watch live broadcast",
        FontId::proportional(11.0),
        Color32::from_rgb(0xB8, 0xC4, 0xD4),
    );

    let resp = ui.interact(
        rect,
        Id::unique(("tg_video_stream_media", msg_id.0)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, title.to_owned()));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if resp.clicked() {
        return Some(MediaInteraction::OpenVideoStream {
            msg_id,
            call_id: att.call_id,
            access_hash: att.access_hash,
            rtmp_stream: att.rtmp_stream,
            title: title.to_owned(),
        });
    }
    None
}

#[cfg(test)]
mod tests_wave4_downloads;
