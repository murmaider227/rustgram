//! Inline clip pacing (TGB-3031): a playing GIF / round video uploads one texture per decoded
//! frame, not one per repaint.

use egui::Context;
use tg_video::generate_test_clip;

use crate::media::backend::MediaItemId;
use crate::media::inline::{
    InlineDocumentData, InlineMediaSource, InlinePlayback, InlinePlaybackKind,
};

fn take_uploads(ctx: &Context) -> usize {
    let mut delta = ctx.tex_manager().write().take_delta();
    let uploads = delta.set.len();
    delta.clear();
    uploads
}

#[test]
fn playing_clip_uploads_once_per_frame_not_per_repaint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gif.mp4");
    generate_test_clip(&path, 160, 120, 30, 2_000, false).expect("clip");
    let source = InlineMediaSource {
        id: MediaItemId(7),
        kind: InlinePlaybackKind::Gif,
        data: InlineDocumentData::File(path),
        target_size: None,
    };
    let ctx = Context::default();
    let mut playback = InlinePlayback::new();
    let _ = take_uploads(&ctx);

    // One second of 60 Hz repaints of a 30 fps clip.
    let mut uploads = 0;
    for step in 0..60 {
        let state = playback.frame_for(&ctx, &source, step * 16);
        assert!(state.error.is_none(), "{:?}", state.error);
        uploads += take_uploads(&ctx);
    }
    assert!(
        (28..=33).contains(&uploads),
        "{uploads} texture uploads for one second of a 30 fps clip at 60 Hz repaints"
    );
}
