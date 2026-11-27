//! TGB-3181: the voice message bubble follows `Document::draw` of tdesktop 7.4.7
//! (`history/view/media/history_view_document.cpp`, voice branch, and
//! `history_view_transcribe_button.cpp`, `TranscribeButton::paint`). Mock data only.
//!
//! `voice_bubble_tgb3181_shots` writes `voice_<theme>.png` into `$TGFORK_SHOTS_DIR`:
//!
//! ```sh
//! TGFORK_SHOTS_DIR=<dir> cargo test -p tg-app-history voice_bubble_tgb3181_shots -- --ignored
//! ```

use egui::{Id, Pos2, Rect, Vec2};
use egui_kittest::HarnessBuilder;
use tg_history::consts::{HISTORY_TRANSCRIBE_SIZE, MSG_FILE_LAYOUT, MSG_FILE_TOP_MINUS};
use tg_history::{BubbleRounding, MediaLayout, Size};

use crate::history::backend::{HistoryMedia, VoiceAttachment};
use crate::history::media::{MediaAssetCache, VoicePlaybackState, paint_history_media};
use crate::history::theme::HistoryColors;
use tg_app_core::stand_in::theme::{Palette, ThemeMode};
use tg_app_core::stand_in::types::MsgId;

const MSG: MsgId = MsgId(7);

fn voice_media() -> HistoryMedia {
    HistoryMedia::Voice(VoiceAttachment {
        duration_secs: 7,
        waveform_5bit: Vec::new(),
        size_bytes: 16_300,
        is_unread: false,
        transcription: None,
    })
}

/// The layout `tg_history` gives a voice message under a sender name (`!isBubbleTop()`).
fn layout(is_bubble_top: bool) -> MediaLayout {
    let st = MSG_FILE_LAYOUT;
    let height = st.padding.top + st.thumb_size + st.padding.bottom
        - if is_bubble_top { 0 } else { MSG_FILE_TOP_MINUS };
    MediaLayout {
        optimal_size: Size::new(415, height),
        current_size: Size::new(415, height),
        needs_bubble: true,
        unwrapped: false,
        enforce_bubble_width: false,
        skip_bubble_tail: false,
        is_bubble_top,
        is_bubble_bottom: true,
        grouped: None,
    }
}

/// Paints one playing voice message at `media_rect` and returns the widget rects by id name.
fn painted(is_bubble_top: bool, media_rect: Rect) -> egui::Context {
    let colors = HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));
    let media = voice_media();
    let media_layout = layout(is_bubble_top);
    let mut assets = MediaAssetCache::new();
    let mut voice_state = VoicePlaybackState {
        playing_msg_id: Some(MSG),
        ..VoicePlaybackState::default()
    };
    voice_state.progress.insert(MSG, 0.4);
    let mut harness = HarnessBuilder::default()
        .with_size(Vec2::new(480.0, 140.0))
        .build_ui(move |ui| {
            let _ = paint_history_media(
                ui,
                MSG,
                &media,
                Some(&media_layout),
                media_rect,
                BubbleRounding::default(),
                false,
                &colors,
                &mut assets,
                &mut voice_state,
            );
        });
    harness.step();
    harness.ctx.clone()
}

fn widget(ctx: &egui::Context, name: &str) -> Option<Rect> {
    ctx.read_response(Id::unique((name, MSG.0))).map(|r| r.rect)
}

#[test]
fn voice_under_a_sender_name_is_shifted_up_by_file_top_minus() {
    // `Document::draw`: `topMinus = isBubbleTop() ? 0 : st::msgFileTopMinus`; the play button,
    // the waveform (`p.translate(nameleft, st.padding.top() - topMinus)`) and the transcribe
    // button (`y = st.padding.top() - topMinus`) all move up with it.
    let media_rect = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(415.0, 54.0));
    let ctx = painted(false, media_rect);
    let expected_top = media_rect.top() + (MSG_FILE_LAYOUT.padding.top - MSG_FILE_TOP_MINUS) as f32;
    let button = widget(&ctx, "tg_voice_btn").expect("play button");
    assert_eq!(button.top(), expected_top, "play button");
    let transcribe = widget(&ctx, "tg_voice_tr").expect("transcribe button");
    assert_eq!(transcribe.top(), expected_top, "transcribe button");
    // `x = nameleft + namewidth + st::historyTranscribeSkip`: the right edge of the content.
    assert_eq!(
        transcribe.right(),
        media_rect.right() - MSG_FILE_LAYOUT.padding.right as f32
    );
    assert_eq!(transcribe.width(), HISTORY_TRANSCRIBE_SIZE.width as f32);

    let top = painted(true, media_rect);
    let button = widget(&top, "tg_voice_btn").expect("play button");
    assert_eq!(
        button.top(),
        media_rect.top() + MSG_FILE_LAYOUT.padding.top as f32
    );
}

#[test]
fn a_playing_voice_bubble_has_no_speed_button() {
    // tdesktop keeps the playback speed in the player bar (`Media::Player::Widget`); the
    // voice branch of `Document::draw` paints no speed control in the bubble.
    let media_rect = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(415.0, 60.0));
    let ctx = painted(true, media_rect);
    assert!(widget(&ctx, "tg_voice_btn").is_some());
    assert!(widget(&ctx, "tg_voice_speed").is_none());
}

#[cfg(windows)]
mod shots {
    use egui::{Pos2, Rect, Vec2};
    use egui_kittest::HarnessBuilder;
    use tg_text::TextWithEntities;
    use tg_ui::fonts::register_tdesktop_fonts;

    use crate::history::backend::{
        HistoryMedia, HistoryMessage, HistoryPeerInfo, HistorySlice, MessageExtra, VoiceAttachment,
    };
    use crate::history::inner::HistoryInnerWidget;
    use crate::history::mock::MockHistoryStore;
    use crate::history::theme::HistoryColors;
    use tg_app_core::stand_in::theme::{Palette, ThemeMode};
    use tg_app_core::stand_in::types::{MsgId, PeerId, PeerKind, PeerSummary};
    use tg_media::audio::waveform_encode_5bit;

    const BASE_UNIX: i32 = 1_795_340_000;

    fn peer() -> PeerId {
        PeerId::from_channel(73_181)
    }

    fn info() -> HistoryPeerInfo {
        HistoryPeerInfo {
            summary: PeerSummary {
                id: peer(),
                kind: PeerKind::Group,
                title: "Voice Crew".to_owned(),
                status: "3 members".to_owned(),
                username: None,
                phone_or_link: None,
                about: String::new(),
                initials: "VC".to_owned(),
                avatar_rgb: [0x5E, 0x72, 0xE4],
                muted: false,
                can_send: true,
                is_saved_messages: false,
            },
            typing_text: None,
            contact_status: None,
            unread_count: 0,
            can_pin: false,
            can_delete: false,
            link_base: String::new(),
        }
    }

    fn waveform(seed: f32) -> Vec<u8> {
        let samples: Vec<u8> = (0..100u32)
            .map(|i| {
                let x = i as f32;
                let wave = ((x * 0.37 + seed).sin() * 0.5 + 0.5) * ((x * 0.05).cos().abs());
                (wave * 28.0).round() as u8
            })
            .collect();
        waveform_encode_5bit(&samples)
    }

    fn voice_message(id: i32, from: u64, name: &str, outgoing: bool) -> HistoryMessage {
        HistoryMessage {
            id: MsgId(id),
            peer_id: peer(),
            from_id: PeerId::from_user(from),
            sender_name: name.to_owned(),
            date_unix: BASE_UNIX + id * 60,
            outgoing,
            is_post: false,
            text: TextWithEntities::simple(""),
            media: Some(HistoryMedia::Voice(VoiceAttachment {
                duration_secs: 7,
                waveform_5bit: waveform(id as f32),
                size_bytes: 16_300,
                is_unread: id == 1,
                transcription: (id == 4).then(|| "Sample transcript of the note.".to_owned()),
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
        }
    }

    struct Snap {
        inner: HistoryInnerWidget,
        store: MockHistoryStore,
        info: HistoryPeerInfo,
        rect: Rect,
        colors: HistoryColors,
    }

    fn render(mode: ThemeMode) -> image::RgbaImage {
        let messages = vec![
            voice_message(1, 3101, "Ivo Marten", false),
            voice_message(2, 3102, "Mira Vance", false),
            voice_message(3, 3101, "Ivo Marten", true),
            voice_message(4, 3101, "Ivo Marten", true),
        ];
        let mut inner = HistoryInnerWidget::new();
        let total_count = messages.len();
        inner.apply_slice(
            peer(),
            HistorySlice {
                messages,
                loaded_at_top: true,
                loaded_at_bottom: true,
                unread_count: 0,
                first_unread_id: None,
                total_count,
            },
            PeerKind::Group,
            true,
        );
        // Message 2 is playing at 40 %, message 4 shows its transcript.
        let voice = inner.voice_state_mut();
        voice.playing_msg_id = Some(MsgId(2));
        voice.progress.insert(MsgId(2), 0.4);
        voice.transcription_expanded.insert(MsgId(4), true);
        let size = Vec2::new(620.0, 460.0);
        let snap = Snap {
            inner,
            store: MockHistoryStore::new(),
            info: info(),
            rect: Rect::from_min_size(Pos2::ZERO, size),
            colors: HistoryColors::from_palette(&Palette::for_mode(mode)),
        };
        let mut harness = HarnessBuilder::default()
            .with_size(size)
            .with_step_dt(0.0)
            .with_max_steps(8)
            .allow_missing_glyphs()
            .with_accessibility_check(false)
            .wgpu()
            .with_context_setup(register_tdesktop_fonts)
            .build_ui_state(
                |ui, st: &mut Snap| {
                    ui.painter().rect_filled(st.rect, 0.0, st.colors.canvas_bg);
                    let _ =
                        st.inner
                            .show(ui, st.rect, &st.info, false, "", &st.colors, &mut st.store);
                },
                snap,
            );
        for _ in 0..6 {
            harness.step();
        }
        harness.render().expect("wgpu render")
    }

    #[test]
    #[ignore = "writes PNGs; run with TGFORK_SHOTS_DIR set"]
    fn voice_bubble_tgb3181_shots() {
        let Some(dir) = std::env::var_os("TGFORK_SHOTS_DIR").map(std::path::PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).expect("shots dir");
        for (tag, mode) in [("day", ThemeMode::Day), ("night", ThemeMode::Night)] {
            render(mode)
                .save(dir.join(format!("voice_{tag}.png")))
                .expect("save png");
        }
    }
}
