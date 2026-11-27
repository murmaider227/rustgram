//! TGB-3185: a photo with a spoiler is covered like in tdesktop 7.4.7 (`Photo::draw`,
//! `Media::fillImageSpoiler`, `Ui::FillSpoilerRect` with `DefaultImageSpoiler()`): the
//! blurred thumbnail under black at `kImageSpoilerDarkenAlpha` and the dense white particles of
//! `DefaultDescriptorImage`, no label, and a click reveals it over `st::fadeWrapDuration`.
//! The old code laid a dark blue veil, a few drifting dots and a "Spoiler" pill over the sharp
//! photo and faded for 220 ms.

use egui::{Context, Event, Modifiers, PointerButton, RawInput};

use super::*;

const BOX: Rect = Rect::from_min_max(Pos2::new(20.0, 20.0), Pos2::new(340.0, 260.0));
const ID: MsgId = MsgId(51);

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

struct Stage {
    ctx: Context,
    time: f64,
    assets: MediaAssetCache,
    rt: HistoryMediaRuntime,
}

impl Stage {
    fn new() -> Self {
        let mut rt = HistoryMediaRuntime::new(false);
        rt.set_spoiler(ID, true);
        Self {
            ctx: Context::default(),
            time: 10.0,
            assets: MediaAssetCache::new(),
            rt,
        }
    }

    fn frame(&mut self, events: Vec<Event>) -> Vec<egui::Shape> {
        self.time += 0.05;
        let input = RawInput {
            time: Some(self.time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
            events,
            ..RawInput::default()
        };
        let (assets, rt) = (&mut self.assets, &mut self.rt);
        let mut out = self.ctx.run_ui(input, |ui| {
            let _ = paint_photo(
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
        shapes
    }

    fn click(&mut self, pos: Pos2) {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        for events in [
            vec![Event::PointerMoved(pos)],
            vec![button(true)],
            vec![button(false)],
        ] {
            self.frame(events);
        }
    }
}

fn flatten(shape: egui::Shape, out: &mut Vec<egui::Shape>) {
    match shape {
        egui::Shape::Vec(list) => list.into_iter().for_each(|s| flatten(s, out)),
        other => out.push(other),
    }
}

fn particle_quads(shapes: &[egui::Shape]) -> usize {
    shapes
        .iter()
        .map(|shape| match shape {
            egui::Shape::Mesh(mesh) => mesh.vertices.len() / 4,
            _ => 0,
        })
        .sum()
}

#[test]
fn a_hidden_photo_is_covered_by_the_image_spoiler() {
    let mut stage = Stage::new();
    let shapes = stage.frame(Vec::new());

    // No label: tdesktop draws a tag only for sensitive or paid media (`drawSpoilerTag`).
    let label = shapes.iter().any(|shape| match shape {
        egui::Shape::Text(text) => text.galley.text().contains("Spoiler"),
        _ => false,
    });
    assert!(!label, "a plain spoiler has no label");

    // `kImageSpoilerDarkenAlpha` black over the whole media.
    let darken = shapes.iter().any(|shape| match shape {
        egui::Shape::Rect(r) => r.rect == BOX && r.fill == Color32::from_black_alpha(32),
        _ => false,
    });
    assert!(darken, "the cover darkens the media by black at alpha 32");

    // `DefaultDescriptorImage`: about 909 particles per 128 px tile; the 320 x 240 box holds
    // about 4.7 tiles.
    let quads = particle_quads(&shapes);
    assert!(quads > 3000, "dense particle cover, got {quads}");
}

#[test]
fn a_click_reveals_the_photo_over_fade_wrap_duration() {
    let mut stage = Stage::new();
    stage.frame(Vec::new());
    stage.click(BOX.center());
    let revealed_at = stage.time;
    assert!(!stage.rt.is_spoiler_hidden(ID));
    // `revealAnimation.start(..., 0., 1., st::fadeWrapDuration)`: 200 ms, linear.
    let half = stage.rt.spoiler_opacity(ID, revealed_at + 0.1);
    assert!((half - 0.5).abs() < 0.01, "half way: {half}");
    assert_eq!(stage.rt.spoiler_opacity(ID, revealed_at + 0.2), 0.0);
    stage.time = revealed_at + 0.2;
    let shapes = stage.frame(Vec::new());
    assert_eq!(particle_quads(&shapes), 0, "no cover once revealed");
}

/// `spoiler_tgb3185_shots` writes `photo_spoiler.png` (hidden | half revealed | revealed) into
/// `$TGFORK_SHOTS_DIR`:
///
/// ```sh
/// TGFORK_SHOTS_DIR=<dir> cargo test -p tg-app-history spoiler_tgb3185_shots -- --ignored
/// ```
#[cfg(windows)]
mod shots {
    use std::sync::Arc;

    use egui_kittest::HarnessBuilder;
    use image::imageops::FilterType;

    use super::super::*;
    use crate::history::inline_media::{InlineMediaDescriptor, PhotoLoadState};
    const SIZE: Vec2 = Vec2::new(320.0, 240.0);
    const GAP: f32 = 16.0;

    /// A sharp picture: coloured tiles with a light diagonal band, so the blur shows.
    fn picture() -> image::RgbaImage {
        image::RgbaImage::from_fn(640, 480, |x, y| {
            let tile = (x / 80 + y / 80) % 4;
            let band = (x as i32 - y as i32).unsigned_abs() < 24;
            let rgb = match (band, tile) {
                (true, _) => [0xF2, 0xE8, 0xC8],
                (false, 0) => [0x2E, 0x86, 0xC1],
                (false, 1) => [0xE6, 0x7E, 0x22],
                (false, 2) => [0x27, 0xAE, 0x60],
                _ => [0x8E, 0x44, 0xAD],
            };
            image::Rgba([rgb[0], rgb[1], rgb[2], 255])
        })
    }

    fn stripped() -> Vec<u8> {
        tg_media::image::inline::encode_stripped_thumb(&image::imageops::resize(
            &picture(),
            40,
            30,
            FilterType::Triangle,
        ))
    }

    fn full_picture() -> Arc<[u8]> {
        let picture = picture();
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(picture)
            .to_rgb8()
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .expect("encode jpeg");
        out.into()
    }

    struct State {
        rt: HistoryMediaRuntime,
        assets: MediaAssetCache,
        photo: PhotoAttachment,
    }

    /// The three photos: hidden, revealed 100 ms ago, revealed.
    const IDS: [MsgId; 3] = [MsgId(61), MsgId(62), MsgId(63)];

    #[test]
    #[ignore = "writes PNGs; run with TGFORK_SHOTS_DIR set"]
    fn spoiler_tgb3185_shots() {
        let Some(dir) = std::env::var_os("TGFORK_SHOTS_DIR").map(std::path::PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).expect("shots dir");
        let bytes = full_picture();
        let mut rt = HistoryMediaRuntime::new(false);
        for id in IDS {
            rt.register_descriptor(
                id,
                InlineMediaDescriptor {
                    has_spoiler: true,
                    photo_load: Some(PhotoLoadState::Ready {
                        bytes: Arc::clone(&bytes),
                    }),
                    ..InlineMediaDescriptor::default()
                },
            );
        }
        let state = State {
            rt,
            assets: MediaAssetCache::new(),
            photo: PhotoAttachment {
                stripped_thumb: Some(stripped()),
                ..super::photo()
            },
        };
        let width = 3.0 * SIZE.x + 4.0 * GAP;
        let mut harness = HarnessBuilder::default()
            .with_size(Vec2::new(width, SIZE.y + 2.0 * GAP))
            .with_step_dt(0.05)
            .with_max_steps(4)
            .with_accessibility_check(false)
            .wgpu()
            .build_ui_state(
                |ui, st: &mut State| {
                    let now = ui.input(|i| i.time);
                    ui.painter().rect_filled(
                        ui.max_rect(),
                        0.0,
                        Color32::from_rgb(0x0E, 0x16, 0x21),
                    );
                    // Revealed 100 ms and 1 s before this frame.
                    for (id, ago) in [(IDS[1], 0.1), (IDS[2], 1.0)] {
                        st.rt.set_spoiler(id, false);
                        st.rt.set_spoiler(id, true);
                        st.rt.reveal_spoiler(id, now - ago);
                    }
                    for (index, id) in IDS.into_iter().enumerate() {
                        let min = Pos2::new(GAP + index as f32 * (SIZE.x + GAP), GAP);
                        let _ = paint_photo(
                            ui,
                            id,
                            &st.photo,
                            Rect::from_min_size(min, SIZE),
                            BubbleRounding::ALL_LARGE,
                            &mut st.assets,
                            Some(&mut st.rt),
                        );
                    }
                },
                state,
            );
        // The full picture is decoded off the paint call.
        for _ in 0..100 {
            harness.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        harness
            .render()
            .expect("wgpu render")
            .save(dir.join("photo_spoiler.png"))
            .expect("save png");
    }
}
