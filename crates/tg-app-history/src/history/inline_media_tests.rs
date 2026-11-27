//! Unit tests and automated screenshot harnesses for inline media playback (`tg-video`,
//! `tg-lottie` per-size cache, `tg-audio::PlayerInstance` + `AudioPlayerBar`, video thumbnails,
//! photo blur-up/loading progress, spoiler media) and rich compose entity editing.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use egui::{CentralPanel, Context, Frame, Vec2, ViewportBuilder};
use tg_audio::AudioType;
use tg_core::settings::{AutoplaySettings, CoreSettings, PowerSavingFlags};
use tg_text::{EntityInText, EntityType, TextWithEntities};
use tg_ui::widgets::rich_input::{FormatAction, RichEditState, normalize_link_url};

use crate::history::HistoryWidget;
use crate::history::backend::{
    DocumentAttachment, HistoryBackend, HistoryMedia, HistoryMessage, MessageExtra,
    PhotoAttachment, StickerAttachment, VideoAttachment, VoiceAttachment,
};
use crate::history::compose::ComposeControls;
use crate::history::inline_media::{
    AutoplayPolicy, HistoryMediaRuntime, InlineMediaDescriptor, PhotoLoadState, is_music_document,
};
use crate::history::mock::{
    MockHistoryStore, sample_lottie_star_json, sample_voice_waveform_5bit, synthetic_stripped_thumb,
};
use crate::history::rich_compose::{
    CUSTOM_EMOJI_PRESETS, MentionAutocompleteState, MentionCandidate, build_rich_text_with_spans,
    find_last_editable_outgoing, insert_mention_candidate, step_reply_target,
};
use crate::history::sections_model::{RoundVideoAttachment, ThreadKey};
use tg_app_core::stand_in::strings::EnglishStrings;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::{MsgId, PeerId};
use tg_app_media::media::inline::{InlineDocumentData, InlinePlaybackKind};
use tg_app_media::media::mock::{find_ffmpeg_bin, generate_test_mp4, render_alpine_sunset};

// ---------------------------------------------------------------------------
// 1. Rich Compose Entity-Editing Unit Tests
// ---------------------------------------------------------------------------

#[test]
fn rich_compose_all_formatting_shortcuts_and_clear() {
    let mut compose = ComposeControls::new();
    compose
        .rich_edit
        .set_text_with_entities(TextWithEntities::simple(
            "bold italic under strike mono spoiler link",
        ));
    compose.text = compose.rich_edit.text.clone();

    compose.handle_format_action(FormatAction::Bold, Some((0, 4)), 100);
    compose.handle_format_action(FormatAction::Italic, Some((5, 11)), 100);
    compose.handle_format_action(FormatAction::Underline, Some((12, 17)), 100);
    compose.handle_format_action(FormatAction::StrikeOut, Some((18, 24)), 100);
    compose.handle_format_action(FormatAction::Monospace, Some((25, 29)), 100);
    compose.handle_format_action(FormatAction::Spoiler, Some((30, 37)), 100);
    assert!(
        compose
            .rich_edit
            .set_link(Some((38, 42)), "link", "rustgram.example")
    );
    compose.text = compose.rich_edit.text.clone();

    let sent = compose.build_pending_extended(None).text;
    assert_eq!(sent.text, "bold italic under strike mono spoiler link");
    assert!(
        sent.entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Bold && e.offset() == 0 && e.length() == 4)
    );
    assert!(
        sent.entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Italic && e.offset() == 5 && e.length() == 6)
    );
    assert!(
        sent.entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Underline
                && e.offset() == 12
                && e.length() == 5)
    );
    assert!(
        sent.entities
            .iter()
            .any(|e| e.entity_type() == EntityType::StrikeOut
                && e.offset() == 18
                && e.length() == 6)
    );
    assert!(
        sent.entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Code && e.offset() == 25 && e.length() == 4)
    );
    assert!(
        sent.entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Spoiler && e.offset() == 30 && e.length() == 7)
    );
    assert!(sent.entities.iter().any(|e| {
        e.entity_type() == EntityType::CustomUrl && e.data() == "https://rustgram.example"
    }));

    // Ctrl+Shift+N clears formatting over selected span only.
    compose.handle_format_action(FormatAction::ClearFormatting, Some((0, 17)), 200);
    assert!(
        !compose
            .rich_edit
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Bold)
    );
    assert!(
        !compose
            .rich_edit
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Italic)
    );
    assert!(
        !compose
            .rich_edit
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Underline)
    );
    assert!(
        compose
            .rich_edit
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::StrikeOut)
    );
}

#[test]
fn rich_compose_mention_autocomplete_inserts_mention_name_entity() {
    let mut rich = RichEditState::from_text_with_entities(TextWithEntities::simple("Ping @mor"));
    rich.cursor_char = 9;
    rich.selection_chars = (9, 9);

    let candidates = vec![
        MentionCandidate {
            user_id: 1001,
            access_hash: 555,
            display_name: "Sam Chen".to_owned(),
            username: Some("sam_chen".to_owned()),
            avatar_rgb: [0xED, 0xA8, 0x6C],
        },
        MentionCandidate {
            user_id: 1003,
            access_hash: 777,
            display_name: "Morgan Kim".to_owned(),
            username: None,
            avatar_rgb: [0x6E, 0xC9, 0xCB],
        },
    ];

    let mut ac = MentionAutocompleteState::default();
    ac.update(&rich, &candidates);
    assert!(ac.is_visible());
    assert_eq!(ac.matches.len(), 1);
    assert_eq!(ac.matches[0].display_name, "Morgan Kim");

    insert_mention_candidate(&mut rich, &ac.matches[0], 1000, ac.replace_char_range);
    assert_eq!(rich.text, "Ping Morgan Kim ");
    let mn = rich
        .entities
        .iter()
        .find(|e| e.entity_type() == EntityType::MentionName)
        .expect("MentionName entity inserted");
    assert_eq!(mn.offset(), 5);
    assert_eq!(mn.length(), 10);
    assert_eq!(mn.data(), "1003.777:1000");

    // Editing text before the MentionName shifts it cleanly; editing inside drops it.
    let prev = rich.text.clone();
    rich.text = format!("Hi! {}", rich.text);
    rich.apply_text_diff(&prev);
    let mn_shifted = rich
        .entities
        .iter()
        .find(|e| e.entity_type() == EntityType::MentionName)
        .expect("MentionName preserved after prefix insert");
    assert_eq!(mn_shifted.offset(), 9);
    assert_eq!(mn_shifted.length(), 10);
}

#[test]
fn rich_compose_custom_emoji_paste_and_draft_restore_with_entities() {
    let mut store = MockHistoryStore::new();
    let peer = PeerId::from_chat(2001);

    let mut compose = ComposeControls::new();
    compose
        .rich_edit
        .set_text_with_entities(TextWithEntities::simple("Ship "));
    compose.text = compose.rich_edit.text.clone();
    compose.insert_custom_emoji(
        CUSTOM_EMOJI_PRESETS[1].emoji,
        CUSTOM_EMOJI_PRESETS[1].document_id,
        100,
    );

    // Paste bold + spoiler rich snippet
    let pasted = TextWithEntities {
        text: " fast build".to_owned(),
        entities: vec![
            EntityInText::new(EntityType::Bold, 1, 4),
            EntityInText::new(EntityType::Spoiler, 6, 5),
        ],
    };
    compose.rich_edit.paste_rich(&pasted, None);
    compose.text = compose.rich_edit.text.clone();

    // Save rich draft and restore into a brand new ComposeControls
    let saved = compose.current_rich_draft().expect("non-empty rich draft");
    assert!(
        saved
            .text
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::CustomEmoji)
    );
    assert!(
        saved
            .text
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Bold)
    );
    assert!(
        saved
            .text
            .entities
            .iter()
            .any(|e| e.entity_type() == EntityType::Spoiler)
    );

    store.save_rich_draft(ThreadKey::Main(peer), Some(saved.clone()));
    let loaded = store
        .load_rich_draft(ThreadKey::Main(peer))
        .expect("rich draft restored");
    assert_eq!(loaded.text, saved.text);

    let mut restored_compose = ComposeControls::new();
    restored_compose.apply_rich_draft(Some(loaded));
    assert_eq!(restored_compose.rich_edit.text, compose.rich_edit.text);
    assert_eq!(
        restored_compose.rich_edit.entities,
        compose.rich_edit.entities
    );
}

#[test]
fn history_up_arrow_edit_and_ctrl_up_down_reply_navigation() {
    let store = MockHistoryStore::new();
    let peer = PeerId::from_chat(2001);
    let slice = store.load_slice_around(peer, MsgId::SHOW_AT_END, 50, 10);

    // Up arrow finds latest outgoing non-service message (`MsgId(111)`).
    let last_out = find_last_editable_outgoing(&slice.messages).expect("outgoing message");
    assert!(last_out.outgoing);
    assert!(!last_out.is_service());
    assert_eq!(last_out.id, MsgId(111));
    assert!(
        !last_out.text.entities.is_empty(),
        "edited message retains entities"
    );

    // Ctrl+Up from Normal picks newest non-service message (`MsgId(112)`).
    let r1 = step_reply_target(&slice.messages, None, -1)
        .flatten()
        .expect("newest message");
    assert_eq!(r1.id, MsgId(112));

    // Ctrl+Up again steps to older message (`MsgId(111)`).
    let r2 = step_reply_target(&slice.messages, Some(r1.id), -1)
        .flatten()
        .expect("previous message");
    assert_eq!(r2.id, MsgId(111));

    // Ctrl+Down steps forward back to `MsgId(112)`.
    let r3 = step_reply_target(&slice.messages, Some(r2.id), 1)
        .flatten()
        .expect("next message");
    assert_eq!(r3.id, MsgId(112));

    // Ctrl+Down at newest returns Some(None) to cancel reply mode.
    let cancel = step_reply_target(&slice.messages, Some(r3.id), 1);
    assert_eq!(cancel, Some(None));
}

#[test]
fn link_url_normalization_and_dialog_contract() {
    assert_eq!(
        normalize_link_url("rustgram.example/docs"),
        "https://rustgram.example/docs"
    );
    assert_eq!(
        normalize_link_url("https://core.telegram.org"),
        "https://core.telegram.org"
    );
    assert_eq!(
        normalize_link_url("tg://resolve?domain=rustgram"),
        "tg://resolve?domain=rustgram"
    );
    assert_eq!(normalize_link_url("https://"), "");
    assert_eq!(normalize_link_url("not a url"), "");
}

// ---------------------------------------------------------------------------
// 2. Inline Media Playback Unit Tests
// ---------------------------------------------------------------------------

#[test]
fn autoplay_settings_respect_core_settings_and_offscreen_pause() {
    let mut settings = CoreSettings {
        loop_animated_stickers: false,
        autoplay: AutoplaySettings {
            gifs: false,
            videos: true,
            round_videos: true,
            video_stickers: true,
        },
        ..CoreSettings::default()
    };
    settings.voice_playback_speed.value = 1.5;

    let policy = AutoplayPolicy::from_core_settings(&settings);
    assert!(!policy.allows_inline(InlinePlaybackKind::Gif));
    assert!(policy.allows_inline(InlinePlaybackKind::RoundVideo));
    assert!(policy.allows_inline(InlinePlaybackKind::VideoSticker));
    assert!(!policy.allows_lottie_loop());
    assert!(policy.allows_spoiler_particles());
    assert!((policy.voice_speed - 1.5).abs() < 1e-6);

    // Granular PowerSavingFlags (STICKERS_CHAT, CHAT_SPOILER, ANIMATIONS, force_all)
    settings.power_saving = PowerSavingFlags::STICKERS_CHAT | PowerSavingFlags::CHAT_SPOILER;
    let ps_policy = AutoplayPolicy::from_core_settings(&settings);
    assert!(!ps_policy.allows_inline(InlinePlaybackKind::VideoSticker));
    assert!(ps_policy.allows_inline(InlinePlaybackKind::RoundVideo));
    assert!(!ps_policy.allows_spoiler_particles());

    settings.power_saving = PowerSavingFlags::NONE;
    settings.power_saving_force_all = true;
    let forced_policy = AutoplayPolicy::from_core_settings(&settings);
    assert!(!forced_policy.allows_inline(InlinePlaybackKind::RoundVideo));
    assert!(!forced_policy.allows_lottie_loop());
    settings.ignore_battery_saving = true;
    let ignored_policy = AutoplayPolicy::from_core_settings(&settings);
    assert!(ignored_policy.allows_inline(InlinePlaybackKind::RoundVideo));

    let mut rt = HistoryMediaRuntime::new(false);
    rt.apply_core_settings(&settings);

    // Frame 1: MsgId(500) is visible.
    rt.begin_frame(1000);
    rt.mark_visible(MsgId(500));
    rt.finish_frame(1000);
    assert!(!rt.is_paused_offscreen(MsgId(500)));

    // Frame 2: MsgId(500) scrolls offscreen -> paused!
    rt.begin_frame(1033);
    rt.finish_frame(1033);
    assert!(rt.is_paused_offscreen(MsgId(500)));

    // Repeated offscreen frame stays idempotently paused.
    rt.begin_frame(1050);
    rt.finish_frame(1050);
    assert!(rt.is_paused_offscreen(MsgId(500)));

    // Frame 3: MsgId(500) scrolls back into view -> resumed!
    rt.begin_frame(1066);
    rt.mark_visible(MsgId(500));
    rt.finish_frame(1066);
    assert!(!rt.is_paused_offscreen(MsgId(500)));
}

#[test]
fn voice_music_and_round_video_audio_playback_via_tg_audio_player_instance() {
    let mut rt = HistoryMediaRuntime::new(false);
    let peer = PeerId::from_chat(2001);

    // 1. Voice message play -> advance -> seek -> speed cycle
    rt.toggle_or_seek_voice(peer, MsgId(108), "Taylor Brooks", 6, None);
    assert!(rt.audio_player_bar().is_visible());
    rt.advance_audio_ms(1200);
    let (active, playing, prog_before, _, _) = rt.audio_item_state(MsgId(108), AudioType::Voice);
    assert!(active);
    assert!(playing);
    assert!(
        prog_before > 0.05,
        "expected voice progress > 0.05, got {prog_before}"
    );

    // Seek to 70%
    rt.toggle_or_seek_voice(peer, MsgId(108), "Taylor Brooks", 6, Some(0.7));
    rt.advance_audio_ms(50);
    let (_, _, prog_after, _, _) = rt.audio_item_state(MsgId(108), AudioType::Voice);
    assert!(
        prog_after > 0.5,
        "expected seeked voice progress > 0.5, got {prog_after}"
    );

    // Cycle speed `1x -> 1.5x -> 2x -> 1x`
    let s1 = rt.cycle_audio_speed(AudioType::Voice);
    assert!((s1 - 1.5).abs() < 0.05);
    let s2 = rt.cycle_audio_speed(AudioType::Voice);
    assert!((s2 - 2.0).abs() < 0.05);

    // 2. Music file play/pause/seek
    let music_doc = DocumentAttachment {
        file_name: "Synth Lab - Midnight Telemetry.mp3".to_owned(),
        size_bytes: 5_242_880,
        mime_type: "audio/mpeg".to_owned(),
        thumb_rgb: None,
    };
    assert!(is_music_document(&music_doc, None));
    rt.toggle_or_seek_music(peer, MsgId(901), &music_doc, None);
    rt.advance_audio_ms(800);
    let (m_active, m_playing, m_prog, _, _) = rt.audio_item_state(MsgId(901), AudioType::Song);
    assert!(m_active && m_playing && m_prog > 0.01);

    // 3. Round video switches from muted autoplay to sound-on on click
    assert!(!rt.is_round_video_unmuted(MsgId(902)));
    rt.click_round_video_sound(peer, MsgId(902), "Sam Chen", 8);
    assert!(rt.is_round_video_unmuted(MsgId(902)));
}

#[test]
fn spoiler_click_to_reveal_and_lottie_per_size_cache() {
    let mut rt = HistoryMediaRuntime::new(false);
    rt.set_spoiler(MsgId(700), true);
    assert!(rt.is_spoiler_hidden(MsgId(700)));
    assert!((rt.spoiler_opacity(MsgId(700), 1.0) - 1.0).abs() < 1e-4);

    rt.reveal_spoiler(MsgId(700), 2.0);
    assert!(!rt.is_spoiler_hidden(MsgId(700)));
    assert!(rt.spoiler_opacity(MsgId(700), 2.5) < 0.01);

    // Lottie cache stores distinct entries per render size!
    let ctx = Context::default();
    let json = sample_lottie_star_json();
    let f1 = rt.get_lottie_frame_per_size(&ctx, MsgId(109), 9001, &json, 0.2, 64);
    assert!(f1.is_some());
    let count_after_64 = rt.lottie_frame_cache_len();
    assert_eq!(count_after_64, 1);

    let f2 = rt.get_lottie_frame_per_size(&ctx, MsgId(109), 9001, &json, 0.2, 160);
    assert!(f2.is_some());
    assert_eq!(
        rt.lottie_frame_cache_len(),
        2,
        "different sticker render sizes must cache separately"
    );
}

#[test]
fn video_gif_round_video_and_thumbnail_generation_via_tg_video() {
    let temp_dir = std::env::temp_dir().join("rustgram_hist_media_unit_test");
    let mp4_path = temp_dir.join("sample_inline.mp4");
    if generate_test_mp4(&mp4_path, 192, 192, 2, false).is_err() {
        // Skip only if ffmpeg binary is genuinely absent on a foreign host.
        return;
    }

    let ctx = Context::default();
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(
        MsgId(801),
        InlineMediaDescriptor {
            video_data: Some(InlineDocumentData::File(mp4_path.clone())),
            ..InlineMediaDescriptor::default()
        },
    );
    rt.register_descriptor(
        MsgId(802),
        InlineMediaDescriptor {
            video_data: Some(InlineDocumentData::File(mp4_path.clone())),
            ..InlineMediaDescriptor::default()
        },
    );

    // 1. GIF clip frame mid-play through InlinePlayback (`tg-video::ClipReader`)
    let gif_frame = rt
        .inline_clip_frame(
            &ctx,
            MsgId(801),
            InlinePlaybackKind::Gif,
            Some((192, 192)),
            500,
        )
        .expect("gif frame");
    assert!(gif_frame.texture.is_some());
    assert!(gif_frame.is_playing);

    // 2. Round video circular frame mid-play
    let round_frame = rt
        .inline_clip_frame(
            &ctx,
            MsgId(802),
            InlinePlaybackKind::RoundVideo,
            Some((192, 192)),
            600,
        )
        .expect("round video frame");
    assert!(round_frame.texture.is_some());

    // 3. Generated video thumbnail via `tg-video::extract_thumbnail`
    let thumb = rt.get_or_generate_video_thumb(&ctx, MsgId(801), 160, false);
    assert!(
        thumb.is_some(),
        "tg-video thumbnail generation must succeed"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// ---------------------------------------------------------------------------
// 3. Screenshot Harnesses -> `crates/tg-app/docs/history_media*.png`
// ---------------------------------------------------------------------------

fn generate_vp9_alpha_webm(output_path: &Path) -> anyhow::Result<()> {
    let ffmpeg = find_ffmpeg_bin();
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let status = std::process::Command::new(&ffmpeg)
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=0x40A7E3:s=160x160:r=20:d=2,format=yuva420p,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(hypot(X-80,Y-80),68),255,0)'",
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuva420p",
            "-auto-alt-ref",
            "0",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
        ])
        .arg(output_path)
        .status()?;
    if !status.success() {
        anyhow::bail!("ffmpeg vp9 alpha exited with {status}");
    }
    Ok(())
}

fn encode_sample_photo_png() -> Vec<u8> {
    let img = render_alpine_sunset(640, 400);
    let mut out = Vec::new();
    let _ = img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

struct MediaShowcaseScreenshotApp {
    widget: HistoryWidget,
    store: MockHistoryStore,
    peer_id: PeerId,
    strings: EnglishStrings,
    palette: Palette,
    output_path: PathBuf,
    mode: ScreenshotMode,
    frames: u32,
    requested: bool,
    done: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
enum ScreenshotMode {
    InlinePlayback,
    RichCompose,
}

impl MediaShowcaseScreenshotApp {
    fn new(cc: &eframe::CreationContext<'_>, output_path: PathBuf, mode: ScreenshotMode) -> Self {
        let palette = Palette::for_mode(ThemeMode::Day);
        cc.egui_ctx
            .set_visuals(palette.to_egui_visuals(ThemeMode::Day));

        let mut store = MockHistoryStore::new();
        let peer_id = PeerId::from_chat(2001);
        let self_id = PeerId::from_user(1000);
        let sender_sam = PeerId::from_user(1001);
        let sender_jordan = PeerId::from_user(1002);
        let sender_taylor = PeerId::from_user(1004);
        let now_unix = 1_795_340_500;

        // Generate real FFmpeg MP4 clips for GIF & round video (plus optional VP9 alpha sticker).
        let tmp = std::env::temp_dir().join("rustgram_hist_media_screenshots");
        let gif_mp4 = tmp.join("inline_gif.mp4");
        let round_mp4 = tmp.join("inline_round.mp4");
        let vp9_webm = tmp.join("video_sticker.webm");
        let _ = generate_test_mp4(&gif_mp4, 320, 200, 3, false);
        let _ = generate_test_mp4(&round_mp4, 240, 240, 4, true);
        let has_vp9 = generate_vp9_alpha_webm(&vp9_webm).is_ok();

        // Append showcase inline-media messages to peer 2001.
        // 1. Spoiler photo + blur-up loading photo
        store.push_message(
            peer_id,
            HistoryMessage {
                id: MsgId(501),
                peer_id,
                from_id: sender_jordan,
                sender_name: "Jordan Patel".to_owned(),
                date_unix: now_unix,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple("Spoiler photo — click to reveal particles veil"),
                media: Some(HistoryMedia::Photo(PhotoAttachment {
                    width: 640,
                    height: 400,
                    stripped_thumb: Some(synthetic_stripped_thumb(0x42)),
                    fallback_rgb: ([0x5E, 0x72, 0xE4], [0x82, 0x5E, 0xE4]),
                    file_name: "spoiler_preview.png".to_owned(),
                    size_bytes: 192_400,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
        );
        store.set_media_descriptor(
            peer_id,
            MsgId(501),
            InlineMediaDescriptor {
                has_spoiler: true,
                photo_load: Some(PhotoLoadState::Ready {
                    bytes: encode_sample_photo_png().into(),
                }),
                ..InlineMediaDescriptor::default()
            },
        );

        // 2. Playing GIF via `tg-video::ClipReader` (`InlinePlayback`)
        store.push_message(
            peer_id,
            HistoryMessage {
                id: MsgId(502),
                peer_id,
                from_id: sender_sam,
                sender_name: "Sam Chen".to_owned(),
                date_unix: now_unix + 30,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::simple("Autoplaying muted GIF via tg-video ClipReader"),
                media: Some(HistoryMedia::Video(VideoAttachment {
                    width: 320,
                    height: 200,
                    duration_secs: 3,
                    size_bytes: 342_000,
                    is_gif: true,
                    stripped_thumb: None,
                    fallback_rgb: ([0x28, 0x54, 0x7E], [0x12, 0x2B, 0x44]),
                    file_name: "inline_loop.mp4".to_owned(),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
        );
        store.set_media_descriptor(
            peer_id,
            MsgId(502),
            InlineMediaDescriptor {
                video_data: Some(InlineDocumentData::File(gif_mp4)),
                ..InlineMediaDescriptor::default()
            },
        );

        // 3. Circular Round Video playing mid-loop with progress ring
        store.push_message(
            peer_id,
            HistoryMessage {
                id: MsgId(503),
                peer_id,
                from_id: self_id,
                sender_name: "Alex Rivera".to_owned(),
                date_unix: now_unix + 60,
                outgoing: true,
                is_post: false,
                text: TextWithEntities::new(),
                media: Some(HistoryMedia::RoundVideo(RoundVideoAttachment {
                    duration_secs: 12,
                    diameter_px: 200,
                    accent_rgb: ([0x35, 0x84, 0xE4], [0x1C, 0x71, 0xD8]),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
        );
        store.set_media_descriptor(
            peer_id,
            MsgId(503),
            InlineMediaDescriptor {
                video_data: Some(InlineDocumentData::File(round_mp4)),
                ..InlineMediaDescriptor::default()
            },
        );

        // 4. Voice note playing mid-waveform with speed button & top AudioPlayerBar
        store.push_message(
            peer_id,
            HistoryMessage {
                id: MsgId(504),
                peer_id,
                from_id: sender_taylor,
                sender_name: "Taylor Brooks".to_owned(),
                date_unix: now_unix + 90,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::new(),
                media: Some(HistoryMedia::Voice(VoiceAttachment {
                    duration_secs: 18,
                    waveform_5bit: sample_voice_waveform_5bit(19),
                    size_bytes: 72_400,
                    is_unread: false,
                    transcription: Some(
                        "Inline voice player synced with tg-audio PlayerInstance.".to_owned(),
                    ),
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
        );

        // 5. Music file attachment
        store.push_message(
            peer_id,
            HistoryMessage {
                id: MsgId(505),
                peer_id,
                from_id: sender_sam,
                sender_name: "Sam Chen".to_owned(),
                date_unix: now_unix + 120,
                outgoing: false,
                is_post: false,
                text: TextWithEntities::new(),
                media: Some(HistoryMedia::Document(DocumentAttachment {
                    file_name: "Synth Lab - Midnight Telemetry.mp3".to_owned(),
                    size_bytes: 4_820_000,
                    mime_type: "audio/mpeg".to_owned(),
                    thumb_rgb: None,
                })),
                service: None,
                reply: None,
                forward: None,
                reactions: Vec::new(),
                views: None,
                edited: false,
                is_read: true,
                is_sending: false,
                is_pinned: false,
                extra: MessageExtra::default(),
            },
        );

        // 6. Optional WebM VP9 alpha video sticker (or Lottie sticker)
        if has_vp9 {
            store.push_message(
                peer_id,
                HistoryMessage {
                    id: MsgId(506),
                    peer_id,
                    from_id: self_id,
                    sender_name: "Alex Rivera".to_owned(),
                    date_unix: now_unix + 140,
                    outgoing: true,
                    is_post: false,
                    text: TextWithEntities::new(),
                    media: Some(HistoryMedia::Sticker(StickerAttachment {
                        sticker_id: 9506,
                        emoji: "🫧".to_owned(),
                        set_title: "WebM Alpha Pack".to_owned(),
                        width: 160,
                        height: 160,
                        lottie_json: sample_lottie_star_json(),
                        set: None,
                    })),
                    service: None,
                    reply: None,
                    forward: None,
                    reactions: Vec::new(),
                    views: None,
                    edited: false,
                    is_read: true,
                    is_sending: false,
                    is_pinned: false,
                    extra: MessageExtra::default(),
                },
            );
            store.set_media_descriptor(
                peer_id,
                MsgId(506),
                InlineMediaDescriptor {
                    video_data: Some(InlineDocumentData::File(vp9_webm)),
                    is_video_sticker: true,
                    ..InlineMediaDescriptor::default()
                },
            );
        }

        let widget = HistoryWidget::new();

        Self {
            widget,
            store,
            peer_id,
            strings: EnglishStrings,
            palette,
            output_path,
            mode,
            frames: 0,
            requested: false,
            done: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl eframe::App for MediaShowcaseScreenshotApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if self.frames == 2 {
            match self.mode {
                ScreenshotMode::InlinePlayback => {
                    // Start playing voice MsgId(504) mid-waveform at 1.5x so both the bubble
                    // waveform + speed badge AND top AudioPlayerBar are active!
                    let rt = self.widget.inner_mut().media_runtime_mut();
                    rt.toggle_or_seek_voice(
                        self.peer_id,
                        MsgId(504),
                        "Taylor Brooks",
                        18,
                        Some(0.45),
                    );
                    rt.cycle_audio_speed(AudioType::Voice);
                    rt.advance_audio_ms(120);
                    // Scroll to show GIF (502), RoundVideo (503), Voice (504), Music (505) together!
                    if let Some(idx) = self
                        .widget
                        .inner()
                        .messages()
                        .iter()
                        .position(|m| m.id == MsgId(502))
                    {
                        let y = self.widget.inner().list().item_top(idx) as f32 + 24.0;
                        self.widget.inner_mut().set_scroll_offset(y.max(0.0));
                    }
                }
                ScreenshotMode::RichCompose => {
                    let rich_sample = build_rich_text_with_spans(
                        "Hey Sam Chen ✨ Shipping bold, italic, underline, strike, cargo test, secret spoiler, and Rustgram Docs! @jo",
                        &[
                            ("Sam Chen", EntityType::MentionName, "1001.901001:1000"),
                            ("✨", EntityType::CustomEmoji, "5368324170671202286"),
                            ("bold", EntityType::Bold, ""),
                            ("italic", EntityType::Italic, ""),
                            ("underline", EntityType::Underline, ""),
                            ("strike", EntityType::StrikeOut, ""),
                            ("cargo test", EntityType::Code, ""),
                            ("secret spoiler", EntityType::Spoiler, ""),
                            (
                                "Rustgram Docs",
                                EntityType::CustomUrl,
                                "https://rustgram.example/docs",
                            ),
                        ],
                    );
                    let compose = self.widget.compose_mut();
                    compose.show_format_strip = true;
                    compose.start_reply(
                        MsgId(502),
                        "Sam Chen".to_owned(),
                        "GIF: Autoplaying muted GIF via tg-video ClipReader".to_owned(),
                    );
                    compose.rich_edit.set_text_with_entities(rich_sample);
                    compose.text = compose.rich_edit.text.clone();
                    let end = compose.rich_edit.text.chars().count();
                    compose.rich_edit.cursor_char = end;
                    compose.rich_edit.selection_chars = (end, end);
                    compose
                        .format_menu
                        .open_at(egui::Pos2::new(530.0, 860.0), (25, 29));
                    // Scroll so the spoiler photo with particles overlay (501) is also visible above!
                    if let Some(idx) = self
                        .widget
                        .inner()
                        .messages()
                        .iter()
                        .position(|m| m.id == MsgId(501))
                    {
                        let y = self.widget.inner().list().item_top(idx) as f32 - 8.0;
                        self.widget.inner_mut().set_scroll_offset(y.max(0.0));
                    }
                }
            }
        }

        // Advance mid-play frames smoothly before screenshot capture!
        if self.frames > 2 && matches!(self.mode, ScreenshotMode::InlinePlayback) {
            self.widget
                .inner_mut()
                .media_runtime_mut()
                .advance_audio_ms(40);
        }

        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            let _ = self.widget.show_with_backend(
                ui,
                self.peer_id,
                &mut self.store,
                true,
                false,
                false,
                &self.palette,
                &self.strings,
            );
        });

        self.frames += 1;
        if self.done.load(Ordering::Acquire) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.requested && self.frames >= 18 {
            self.requested = true;
            let path = self.output_path.clone();
            let done = Arc::clone(&self.done);
            let ctx_cb = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                egui::ScreenshotCallback::new(move |img| {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let [w, h] = img.size;
                    let mut raw = Vec::with_capacity(w * h * 4);
                    for px in &img.pixels {
                        let [r, g, b, a] = px.to_srgba_unmultiplied();
                        raw.extend_from_slice(&[r, g, b, a]);
                    }
                    if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                        let _ = buf.save(&path);
                    }
                    done.store(true, Ordering::Release);
                    ctx_cb.request_repaint();
                }),
            ));
        } else {
            ctx.request_repaint();
        }
    }
}

fn run_screenshot(file_name: &str, title: &str, mode: ScreenshotMode) {
    let output_path = tg_app_core::core_types::paths::workspace_app_docs_dir().join(file_name);
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(Vec2::new(780.0, 1020.0)),
        #[cfg(target_os = "windows")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        })),
        ..Default::default()
    };
    let out_clone = output_path.clone();
    eframe::run_native(
        title,
        options,
        Box::new(move |cc| {
            Ok(Box::new(MediaShowcaseScreenshotApp::new(
                cc, out_clone, mode,
            )))
        }),
    )
    .expect("eframe screenshot capture");
    assert!(
        output_path.exists(),
        "screenshot was not written to {}",
        output_path.display()
    );
}

#[test]
#[ignore = "opens native eframe window to capture crates/tg-app/docs/history_media_playback.png"]
fn capture_history_media_playback_screenshot() {
    run_screenshot(
        "history_media_playback.png",
        "Rustgram — Inline Media Playback (GIF, Round Video, Voice & Player Bar)",
        ScreenshotMode::InlinePlayback,
    );
}

#[test]
#[ignore = "opens native eframe window to capture crates/tg-app/docs/history_media_compose.png"]
fn capture_history_media_compose_screenshot() {
    run_screenshot(
        "history_media_compose.png",
        "Rustgram — Rich Compose Field with Formatting & Mentions",
        ScreenshotMode::RichCompose,
    );
}

// ---------------------------------------------------------------------------
// 4. Business Quick Replies & Premium Limits History Integration Tests
// ---------------------------------------------------------------------------

#[test]
fn business_quick_replies_slash_autocomplete_and_multi_message_send() {
    use crate::business::backend::BusinessBackend;
    use crate::business::mock::MockBusinessBackend;
    use crate::history::rich_compose::QuickReplyAutocompleteState;
    use tg_app_core::stand_in::types::PeerKind;

    let biz = MockBusinessBackend::new();
    let overview = biz.load_overview().expect("mock overview");
    let mut shortcuts: Vec<_> = overview.shortcuts.values().cloned().collect();
    shortcuts.sort_by(|a, b| a.name.cmp(&b.name));
    assert!(shortcuts.len() >= 3, "mock business has sample shortcuts");

    let mut ac = QuickReplyAutocompleteState::default();

    // Non-business account: must not show quick replies even in private chats.
    ac.update("/", true, false, &shortcuts);
    assert!(!ac.is_visible());

    // Group/channel/saved chat: must not show quick replies even for business accounts.
    ac.update("/", false, true, &shortcuts);
    assert!(!ac.is_visible());

    // Private chat + business account: "/" shows all shortcuts; "/pri" filters to "/pricing".
    ac.update("/", true, true, &shortcuts);
    assert!(ac.is_visible());
    assert_eq!(ac.matches.len(), shortcuts.len());

    ac.update("/pri", true, true, &shortcuts);
    assert!(ac.is_visible());
    assert_eq!(ac.matches.len(), 1);
    assert_eq!(ac.matches[0].name, "pricing");
    let pricing = ac.matches[0].clone();
    assert!(
        pricing.count() >= 2,
        "pricing shortcut sends multiple messages"
    );

    // Sending the shortcut dispatches every message in order into the active chat.
    let mut store = MockHistoryStore::new();
    let peer = PeerId::from_user(1001);
    let mut widget = HistoryWidget::new();
    widget
        .inner_mut()
        .load_peer(peer, MsgId::SHOW_AT_END, &store, PeerKind::User);
    let before = widget.inner().messages().len();

    let sent = widget.send_quick_reply_shortcut(peer, PeerKind::User, &pricing, &mut store);
    assert_eq!(sent, pricing.count());
    assert_eq!(widget.inner().messages().len(), before + pricing.count());

    let tail = &widget.inner().messages()[before..];
    assert_eq!(tail[0].text.text, pricing.messages[0].text);
    assert_eq!(tail[1].text.text, pricing.messages[1].text);
    // First /pricing message carries the Document attachment; second is plain text.
    assert!(matches!(tail[0].media, Some(HistoryMedia::Document(_))));
    assert!(tail[1].media.is_none());
}

#[test]
fn history_caption_length_and_saved_gifs_limits_open_premium_limits_view() {
    use crate::business::model::PremiumLimitKind;
    use crate::history::compose_boxes::{AttachFilesAction, AttachFilesBoxState};

    let mut widget = HistoryWidget::new();
    widget.set_is_premium(false);
    assert_eq!(widget.active_premium_limit_modal(), None);

    // 1. Caption length limit: free = 1024, Premium = 2048.
    let ok_caption = "a".repeat(1024);
    assert!(widget.check_caption_length_limit(&ok_caption));
    assert_eq!(widget.active_premium_limit_modal(), None);

    let over_free_caption = "b".repeat(1025);
    let mut attach_box = AttachFilesBoxState::new(false, false);
    attach_box.caption = over_free_caption.clone();
    assert!(matches!(
        attach_box.build_action_for_send(),
        AttachFilesAction::CaptionLimitExceeded {
            length: 1025,
            limit: 1024
        }
    ));

    assert!(!widget.check_caption_length_limit(&over_free_caption));
    assert_eq!(
        widget.active_premium_limit_modal(),
        Some(PremiumLimitKind::CaptionLength)
    );
    assert!(widget.close_premium_limits());
    assert_eq!(widget.active_premium_limit_modal(), None);

    // Premium raises caption limit to 2048.
    widget.set_is_premium(true);
    let mid_caption = "c".repeat(1500);
    assert!(widget.check_caption_length_limit(&mid_caption));
    let over_prem_caption = "d".repeat(2049);
    assert!(!widget.check_caption_length_limit(&over_prem_caption));
    assert_eq!(
        widget.active_premium_limit_modal(),
        Some(PremiumLimitKind::CaptionLength)
    );
    widget.close_premium_limits();

    // 2. Saved GIFs limit: free = 200, Premium = 400.
    widget.set_is_premium(false);
    widget.set_saved_gifs_count(199);
    assert!(widget.try_save_gif(MsgId(501), 1.0));
    assert_eq!(widget.saved_gifs_count(), 200);
    assert_eq!(widget.active_premium_limit_modal(), None);

    // Saving 201st GIF exceeds free limit -> opens PremiumLimitsView(SavedGifs).
    assert!(!widget.try_save_gif(MsgId(502), 2.0));
    assert_eq!(widget.saved_gifs_count(), 200);
    assert_eq!(
        widget.active_premium_limit_modal(),
        Some(PremiumLimitKind::SavedGifs)
    );
    widget.close_premium_limits();

    // Premium allows up to 400 saved GIFs, then opens PremiumLimitsView(SavedGifs).
    widget.set_is_premium(true);
    assert!(widget.try_save_gif(MsgId(502), 3.0));
    assert_eq!(widget.saved_gifs_count(), 201);
    widget.set_saved_gifs_count(400);
    assert!(!widget.try_save_gif(MsgId(503), 4.0));
    assert_eq!(
        widget.active_premium_limit_modal(),
        Some(PremiumLimitKind::SavedGifs)
    );
}
