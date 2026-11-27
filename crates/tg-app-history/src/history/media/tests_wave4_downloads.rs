//! TGB-2982: a click on a photo that is downloading cancels the download, the next click
//! starts it again, and the circle turns `msgDateImgBgOver` under the pointer
//! (`Photo::textState` / `Photo::draw`). The old code opened the media viewer on every click
//! and had one circle colour.
//!
//! TGB-2958: the status line of a voice message shows the file size only while the file is
//! not in the media cache (`Document::updateStatusText` with `dataLoaded()`), and a play of a
//! voice message that is not cached asks for the download instead of playing a stand-in. The
//! old code took "loaded" from whether the message was started in the open chat.

use egui::{Context, Event, Modifiers, PointerButton, RawInput};
use tg_app_core::stand_in::theme::{Palette, ThemeMode};

use super::*;
use crate::history::inline_media::{
    InlineMediaDescriptor, PhotoLoadState, VoiceLoadState, synthesize_tone_wav,
};

const BOX: Rect = Rect::from_min_max(Pos2::new(20.0, 20.0), Pos2::new(340.0, 260.0));
const ID: MsgId = MsgId(41);
const PEER: PeerId = PeerId(77);

fn photo() -> PhotoAttachment {
    PhotoAttachment {
        width: 640,
        height: 480,
        stripped_thumb: None,
        fallback_rgb: ([0x5E, 0x72, 0xE4], [0x82, 0x5E, 0xE4]),
        file_name: "picture.jpg".to_owned(),
        size_bytes: 120_000,
    }
}

fn photo_state(state: PhotoLoadState) -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        photo_load: Some(state),
        ..InlineMediaDescriptor::default()
    }
}

fn loading() -> PhotoLoadState {
    PhotoLoadState::Loading {
        progress: Some(0.3),
        bytes_loaded: 36_000,
        total_bytes: 120_000,
    }
}

/// An egui context driven frame by frame with pointer events.
struct Stage {
    ctx: Context,
    time: f64,
    assets: MediaAssetCache,
    rt: HistoryMediaRuntime,
}

impl Stage {
    fn new(desc: Option<InlineMediaDescriptor>) -> Self {
        let mut rt = HistoryMediaRuntime::new(false);
        if let Some(desc) = desc {
            rt.register_descriptor(ID, desc);
        }
        Self {
            ctx: Context::default(),
            time: 0.0,
            assets: MediaAssetCache::new(),
            rt,
        }
    }

    /// One frame painting the photo; returns what the paint reported and the frame's shapes.
    fn frame(&mut self, events: Vec<Event>) -> (Option<MediaInteraction>, Vec<egui::Shape>) {
        self.time += 0.05;
        let input = RawInput {
            time: Some(self.time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
            events,
            ..RawInput::default()
        };
        let mut interaction = None;
        let (assets, rt) = (&mut self.assets, &mut self.rt);
        let mut out = self.ctx.run_ui(input, |ui| {
            interaction = paint_photo(
                ui,
                ID,
                &photo(),
                BOX,
                BubbleRounding::default(),
                assets,
                Some(rt),
            );
        });
        out.textures_delta.clear();
        let mut shapes = Vec::new();
        for clipped in out.shapes {
            flatten(clipped.shape, &mut shapes);
        }
        (interaction, shapes)
    }

    fn hover(&mut self, pos: Pos2) -> Vec<egui::Shape> {
        self.frame(vec![Event::PointerMoved(pos)]).1
    }

    /// Moves to `pos`, presses and releases there; returns every interaction reported.
    fn click(&mut self, pos: Pos2) -> Vec<MediaInteraction> {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut seen = Vec::new();
        for events in [
            vec![Event::PointerMoved(pos)],
            vec![button(true)],
            vec![button(false)],
            vec![],
        ] {
            seen.extend(self.frame(events).0);
        }
        seen
    }
}

fn flatten(shape: egui::Shape, out: &mut Vec<egui::Shape>) {
    match shape {
        egui::Shape::Vec(list) => list.into_iter().for_each(|s| flatten(s, out)),
        other => out.push(other),
    }
}

/// Fill of the 44 px circle in the photo's centre, if one was painted.
fn circle_fill(shapes: &[egui::Shape]) -> Option<Color32> {
    shapes.iter().find_map(|shape| match shape {
        egui::Shape::Circle(c) if (c.radius - 22.0).abs() < 0.01 && c.center == BOX.center() => {
            Some(c.fill)
        }
        _ => None,
    })
}

fn line_count(shapes: &[egui::Shape]) -> usize {
    shapes
        .iter()
        .filter(|s| matches!(s, egui::Shape::LineSegment { .. }))
        .count()
}

#[test]
fn a_click_on_a_loading_photo_cancels_and_the_next_click_downloads_again() {
    let mut stage = Stage::new(Some(photo_state(loading())));
    assert_eq!(stage.rt.photo_link(ID), PhotoLink::Cancel);

    let seen = stage.click(BOX.center());
    assert!(
        seen.is_empty(),
        "a loading photo must not open the viewer: {seen:?}"
    );
    assert_eq!(
        stage.rt.take_download_actions(),
        vec![(ID, MediaDownloadAction::Cancel)]
    );

    // What the backend reports after the cancel.
    stage.rt.set_photo_load_state(ID, PhotoLoadState::NotLoaded);
    assert_eq!(stage.rt.photo_link(ID), PhotoLink::Save);
    let shapes = stage.hover(Pos2::new(390.0, 290.0));
    assert!(
        circle_fill(&shapes).is_some(),
        "a cancelled photo shows the circle"
    );
    assert_eq!(line_count(&shapes), 3, "with the download arrow in it");

    let seen = stage.click(BOX.center());
    assert!(
        seen.is_empty(),
        "a click on the arrow downloads, not opens: {seen:?}"
    );
    assert_eq!(
        stage.rt.take_download_actions(),
        vec![(ID, MediaDownloadAction::Start)]
    );
}

#[test]
fn a_click_anywhere_on_the_loading_photo_cancels_like_tdesktop() {
    // `Photo::textState`: the link covers the whole photo, not only the circle.
    let mut stage = Stage::new(Some(photo_state(loading())));
    let corner = BOX.min + Vec2::new(6.0, 6.0);
    assert!(stage.click(corner).is_empty());
    assert_eq!(
        stage.rt.take_download_actions(),
        vec![(ID, MediaDownloadAction::Cancel)]
    );
}

#[test]
fn a_loaded_photo_still_opens_the_viewer() {
    let mut stage = Stage::new(None);
    assert_eq!(stage.rt.photo_link(ID), PhotoLink::Open);
    let seen = stage.click(BOX.center());
    assert_eq!(
        seen,
        vec![MediaInteraction::OpenMedia {
            msg_id: ID,
            album_index: None,
        }]
    );
    assert!(stage.rt.take_download_actions().is_empty());
}

#[test]
fn the_circle_turns_to_the_over_colour_under_the_pointer() {
    let normal = Color32::from_rgba_premultiplied(0, 0, 0, 0x54);
    let over = Color32::from_rgba_premultiplied(0, 0, 0, 0x74);
    for state in [loading(), PhotoLoadState::NotLoaded] {
        let mut stage = Stage::new(Some(photo_state(state.clone())));
        let away = stage.hover(Pos2::new(390.0, 290.0));
        assert_eq!(circle_fill(&away), Some(normal), "{state:?} without hover");

        // `st::msgFileOverDuration` is 200 ms; frames are 50 ms apart.
        let mut last = Vec::new();
        for _ in 0..8 {
            last = stage.hover(BOX.center());
        }
        assert_eq!(circle_fill(&last), Some(over), "{state:?} hovered");

        for _ in 0..8 {
            last = stage.hover(Pos2::new(390.0, 290.0));
        }
        assert_eq!(circle_fill(&last), Some(normal), "{state:?} after leaving");
    }
}

// --- voice (TGB-2958) --------------------------------------------------------------------

fn voice() -> VoiceAttachment {
    VoiceAttachment {
        duration_secs: 4,
        waveform_5bit: vec![0x55; 63],
        size_bytes: 16_300,
        is_unread: false,
        transcription: None,
    }
}

/// Paints the voice bubble once and returns every text it laid out.
fn voice_texts(
    rt: &mut HistoryMediaRuntime,
    voice_state: &mut VoicePlaybackState,
    time: f64,
) -> Vec<String> {
    let ctx = Context::default();
    let colors = HistoryColors::from_palette(&Palette::for_mode(ThemeMode::Day));
    let input = RawInput {
        time: Some(time),
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        let _ = paint_voice(
            ui,
            PEER,
            ID,
            "Ivo Marten",
            &voice(),
            Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(300.0, 60.0)),
            false,
            &colors,
            voice_state,
            Some(rt),
            None,
        );
    });
    out.textures_delta.clear();
    let mut shapes = Vec::new();
    for clipped in out.shapes {
        flatten(clipped.shape, &mut shapes);
    }
    shapes
        .into_iter()
        .filter_map(|shape| match shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

fn has(texts: &[String], wanted: &str) -> bool {
    texts.iter().any(|t| t == wanted)
}

#[test]
fn voice_status_shows_the_size_only_while_the_file_is_not_cached() {
    // Reopened chat (or a restart) with the file in the cache: never started in this chat
    // view, and still no size.
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(
        ID,
        InlineMediaDescriptor {
            audio_bytes: Some(synthesize_tone_wav(300.0, 2.0)),
            audio_ext: Some("wav".to_owned()),
            ..InlineMediaDescriptor::default()
        },
    );
    let mut state = VoicePlaybackState::default();
    let texts = voice_texts(&mut rt, &mut state, 1.0);
    assert!(
        has(&texts, "00:04"),
        "cached voice shows no size: {texts:?}"
    );

    // Not cached, although it was started here before (e.g. the cache was cleared): the size.
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(
        ID,
        InlineMediaDescriptor {
            voice_load: Some(VoiceLoadState::NotLoaded),
            ..InlineMediaDescriptor::default()
        },
    );
    let mut state = VoicePlaybackState::default();
    state.progress.insert(ID, 0.0);
    let texts = voice_texts(&mut rt, &mut state, 1.0);
    // The decimal separator follows the system locale.
    let with_size = format!("00:04, 15{}9 KB", locale_decimal_point());
    assert!(
        has(&texts, &with_size),
        "voice that is not cached shows its size: {texts:?}"
    );
}

/// The first play of a voice message asks for a stream (TGB-3000); a backend that cannot
/// stream answers `None`, and the play falls back to downloading the file.
fn play_without_streaming(rt: &mut HistoryMediaRuntime) {
    rt.toggle_or_seek_voice(PEER, ID, "Ivo Marten", 4, None);
    assert_eq!(
        rt.take_download_actions(),
        vec![(ID, MediaDownloadAction::Stream)]
    );
    rt.provide_voice_stream(ID, None);
}

#[test]
fn a_play_of_a_voice_that_is_not_cached_downloads_it_and_plays_when_it_arrives() {
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(
        ID,
        InlineMediaDescriptor {
            voice_load: Some(VoiceLoadState::NotLoaded),
            ..InlineMediaDescriptor::default()
        },
    );
    play_without_streaming(&mut rt);
    assert_eq!(
        rt.take_download_actions(),
        vec![(ID, MediaDownloadAction::Start)],
        "without streaming the play asks for the file"
    );
    assert!(rt.voice_play_pending(ID));
    assert!(
        !rt.audio_item_state(ID, tg_audio::AudioType::Voice).0,
        "nothing plays before the file is there (no stand-in tone)"
    );
    // Shown like a playing message at 00:00 while it loads.
    let mut state = VoicePlaybackState::default();
    let texts = voice_texts(&mut rt, &mut state, 1.0);
    assert!(has(&texts, "00:00 / 00:04"), "{texts:?}");

    rt.register_descriptor(
        ID,
        InlineMediaDescriptor {
            audio_bytes: Some(synthesize_tone_wav(300.0, 2.0)),
            audio_ext: Some("wav".to_owned()),
            ..InlineMediaDescriptor::default()
        },
    );
    assert!(!rt.voice_play_pending(ID));
    let (active, playing, ..) = rt.audio_item_state(ID, tg_audio::AudioType::Voice);
    assert!(active && playing, "the message plays once its file arrived");
    assert!(rt.take_download_actions().is_empty());
}

#[test]
fn a_second_click_before_the_file_arrives_takes_the_play_back() {
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(
        ID,
        InlineMediaDescriptor {
            voice_load: Some(VoiceLoadState::NotLoaded),
            ..InlineMediaDescriptor::default()
        },
    );
    play_without_streaming(&mut rt);
    assert!(rt.voice_play_pending(ID));
    rt.toggle_or_seek_voice(PEER, ID, "Ivo Marten", 4, None);
    assert!(!rt.voice_play_pending(ID));
    rt.register_descriptor(
        ID,
        InlineMediaDescriptor {
            audio_bytes: Some(synthesize_tone_wav(300.0, 2.0)),
            audio_ext: Some("wav".to_owned()),
            ..InlineMediaDescriptor::default()
        },
    );
    assert!(!rt.audio_item_state(ID, tg_audio::AudioType::Voice).0);
}
