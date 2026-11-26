//! TGB-2970: the loading arc over a downloading photo turns and asks for frames at the
//! animation rate while it is painted (`Photo::draw` with `Ui::RadialAnimation`), asks for none
//! while it is off screen, and stops after the photo is loaded. The old ring with a known
//! progress was static and asked for no frames, so it looked frozen.

use std::time::Duration;

use egui::{Context, CornerRadius, Pos2, RawInput, Rect, ViewportId};
use tg_ui::effects::RADIAL_DURATION_MS;

use super::*;
use crate::history::photo_frame::PhotoFit;

const BOX: Rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(320.0, 240.0));
const ID: MsgId = MsgId(77);

fn loading(progress: Option<f32>) -> InlineMediaDescriptor {
    InlineMediaDescriptor {
        photo_load: Some(PhotoLoadState::Loading {
            progress,
            bytes_loaded: 30_000,
            total_bytes: 100_000,
        }),
        ..InlineMediaDescriptor::default()
    }
}

/// One egui pass at `t` seconds; paints the photo when `paint`. Returns the repaint delay the
/// pass asked for (`Duration::MAX` when none).
fn pass(ctx: &Context, rt: &mut HistoryMediaRuntime, t: f64, paint: bool) -> Duration {
    let input = RawInput {
        time: Some(t),
        ..RawInput::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        if paint {
            let painter = ui.painter().clone();
            rt.paint_photo_load_and_blur_up(
                ui.ctx(),
                &painter,
                ID,
                BOX,
                CornerRadius::ZERO,
                PhotoFit::Single,
                t,
                0.0,
            );
        }
    });
    out.textures_delta.clear();
    out.viewport_output
        .get(&ViewportId::ROOT)
        .map_or(Duration::MAX, |v| v.repaint_delay)
}

fn warm_up(ctx: &Context, rt: &mut HistoryMediaRuntime) {
    for i in 0..3 {
        pass(ctx, rt, f64::from(i) * 0.01, false);
    }
}

#[test]
fn a_loading_photo_in_view_animates_at_the_frame_rate() {
    for progress in [Some(0.3), None] {
        let ctx = Context::default();
        let mut rt = HistoryMediaRuntime::new(false);
        rt.register_descriptor(ID, loading(progress));
        warm_up(&ctx, &mut rt);

        let first = pass(&ctx, &mut rt, 1.0, true);
        let a = rt
            .photo_radial_state(ID)
            .expect("radial runs while loading");
        let second = pass(&ctx, &mut rt, 1.5, true);
        let b = rt
            .photo_radial_state(ID)
            .expect("radial runs while loading");
        for delay in [first, second] {
            assert!(
                delay <= RADIAL_FRAME,
                "progress {progress:?}: a visible loading photo asks for the next frame, got {delay:?}"
            );
        }
        assert!(
            (a.arc_from_rad - b.arc_from_rad).abs() > 0.3,
            "progress {progress:?}: the arc turns with the same progress ({} vs {})",
            a.arc_from_rad,
            b.arc_from_rad
        );
    }
}

#[test]
fn a_loading_photo_out_of_view_asks_for_no_frames() {
    let ctx = Context::default();
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(ID, loading(Some(0.3)));
    warm_up(&ctx, &mut rt);
    assert!(pass(&ctx, &mut rt, 1.0, true) <= RADIAL_FRAME);
    // Scrolled away: the photo is not painted, nothing asks for frames.
    pass(&ctx, &mut rt, 1.02, false);
    assert_eq!(pass(&ctx, &mut rt, 1.04, false), Duration::MAX);
}

#[test]
fn the_arc_fades_out_and_stops_once_the_photo_is_loaded() {
    let ctx = Context::default();
    let mut rt = HistoryMediaRuntime::new(false);
    rt.register_descriptor(ID, loading(Some(0.6)));
    warm_up(&ctx, &mut rt);
    pass(&ctx, &mut rt, 1.0, true);
    pass(&ctx, &mut rt, 1.5, true);

    // Bytes that do not decode: the blur-up never starts, so only the arc asks for frames.
    rt.set_photo_load_state(
        ID,
        PhotoLoadState::Ready {
            bytes: vec![0_u8, 1, 2].into(),
        },
    );
    assert!(
        pass(&ctx, &mut rt, 1.6, true) <= RADIAL_FRAME,
        "the arc fades out"
    );
    assert!(rt.photo_radial_state(ID).is_some());
    pass(
        &ctx,
        &mut rt,
        1.6 + RADIAL_DURATION_MS / 1000.0 + 0.05,
        true,
    );
    assert!(rt.photo_radial_state(ID).is_none(), "the arc is gone");

    // Once the failed decode has landed, painting the photo asks for no more frames.
    let mut t = 2.2;
    let mut settled = false;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(10));
        t += 0.02;
        if pass(&ctx, &mut rt, t, true) == Duration::MAX {
            settled = true;
            break;
        }
    }
    assert!(settled, "a loaded photo stops asking for frames");
}
