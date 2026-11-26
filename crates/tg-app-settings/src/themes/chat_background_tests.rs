//! Area-sized wallpaper textures: resizes render off the UI thread, pattern hashes are remembered,
//! and the texture cache drops least recently used entries (TGB-2893).

use std::time::{Duration, Instant};

use super::*;

impl ChatBackground {
    /// Waits for the worker render and any queued one, uploading each like a frame would.
    fn finish_background_renders(&mut self, ctx: &Context) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Some(pending) = &self.pending_render {
                if let Ok(image) = pending.result.recv_timeout(Duration::from_secs(30)) {
                    let handle = self.upload_sized(ctx, image);
                    if let Some(done) = self.pending_render.take() {
                        self.insert_texture(done.key, handle, true);
                    }
                } else {
                    self.pending_render = None;
                }
            }
            self.poll_background_render(ctx);
            if self.pending_render.is_none() && self.queued_render.is_none() {
                return;
            }
        }
        panic!("background wallpaper render did not finish");
    }

    fn cached_sizes(&self) -> Vec<(u32, u32)> {
        let mut sizes: Vec<_> = self
            .texture_cache
            .iter()
            .filter(|(_, t)| t.sized)
            .map(|(k, _)| (k.width, k.height))
            .collect();
        sizes.sort_unstable();
        sizes
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[test]
fn resize_keeps_the_old_texture_and_renders_the_new_size_off_thread() {
    let ctx = Context::default();
    let mut bg = ChatBackground::new();
    let cfg = BackgroundConfig::classic_meadow();

    let started = Instant::now();
    let (first, exact) = bg.texture_for_config_sized(&ctx, &cfg, 832, 656);
    let first_paint = started.elapsed();
    assert!(exact);
    assert_eq!(
        bg.sync_renders, 1,
        "the first size of a wallpaper renders in the frame"
    );
    assert_eq!(first.size(), [832, 656]);

    // The chat area grew by 16 px (another chat's bars): the frame keeps the old texture.
    let started = Instant::now();
    let (stand_in, exact) = bg.texture_for_config_sized(&ctx, &cfg, 832, 672);
    let resize_frame = started.elapsed();
    assert!(!exact);
    assert_eq!(stand_in.id(), first.id());
    assert_eq!(
        bg.sync_renders, 1,
        "a resize must not rasterize on the UI thread"
    );
    println!(
        "measured: first paint {:.2} ms (renders), resized frame {:.3} ms (stand-in)",
        ms(first_paint),
        ms(resize_frame)
    );
    assert!(
        resize_frame < first_paint / 4,
        "resized frame {resize_frame:?} vs render {first_paint:?}"
    );

    bg.finish_background_renders(&ctx);
    let (resized, exact) = bg.texture_for_config_sized(&ctx, &cfg, 832, 672);
    assert!(exact, "the exact size replaces the stand-in once rendered");
    assert_eq!(resized.size(), [832, 672]);
    assert_eq!(bg.sync_renders, 1);
}

#[test]
fn worker_render_matches_the_ui_thread_render() {
    let cfg = BackgroundConfig::classic_meadow();
    let on_ui = ChatBackground::new().render_image(&cfg, 320, 480, 0);
    let worker = std::thread::spawn(move || ChatBackground::new().render_image(&cfg, 320, 480, 0))
        .join()
        .expect("render thread");
    assert_eq!(on_ui.as_raw(), worker.as_raw());
}

#[test]
fn renders_requested_during_a_resize_are_coalesced_to_the_latest_size() {
    let ctx = Context::default();
    let mut bg = ChatBackground::new();
    let cfg = BackgroundConfig::classic_meadow();
    // Large sizes so each worker render outlasts the burst of resize frames below.
    let _ = bg.texture_for_config_sized(&ctx, &cfg, 1600, 1200);
    for width in [1616, 1632, 1648, 1664] {
        let (_, exact) = bg.texture_for_config_sized(&ctx, &cfg, width, 1200);
        assert!(!exact);
    }
    bg.finish_background_renders(&ctx);
    let sizes = bg.cached_sizes();
    assert!(
        sizes.contains(&(1600, 1200)) && sizes.contains(&(1664, 1200)),
        "{sizes:?}"
    );
    assert!(
        sizes.len() < 5,
        "sizes requested while a render runs collapse to the latest one: {sizes:?}"
    );
    assert_eq!(bg.sync_renders, 1);
    let (_, exact) = bg.texture_for_config_sized(&ctx, &cfg, 1664, 1200);
    assert!(exact);
}

#[test]
fn another_wallpaper_or_mode_never_stands_in() {
    let ctx = Context::default();
    let mut bg = ChatBackground::new();
    let cfg = BackgroundConfig::classic_meadow();
    let _ = bg.texture_for_config_sized(&ctx, &cfg, 400, 400);

    let mut other = cfg.clone();
    other.wallpaper.background_colors =
        vec![Color::rgb(0x20, 0x40, 0x60), Color::rgb(0x60, 0x40, 0x20)];
    let (_, exact) = bg.texture_for_config_sized(&ctx, &other, 400, 416);
    assert!(exact, "different colours render in the frame, as before");
    assert_eq!(bg.sync_renders, 2);

    bg.set_dark_mode(true);
    let (_, exact) = bg.texture_for_config_sized(&ctx, &cfg, 400, 432);
    assert!(exact, "a texture from the other mode is not a stand-in");
    assert_eq!(bg.sync_renders, 3);
}

#[test]
fn stand_in_is_drawn_evenly_scaled_never_stretched() {
    let ctx = Context::default();
    let mut bg = ChatBackground::new();
    let cfg = BackgroundConfig::classic_meadow();
    let (tex, _) = bg.texture_for_config_sized(&ctx, &cfg, 800, 600);
    let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 640.0));

    assert_eq!(
        sized_uv(rect, &tex, true),
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        "an exact texture is drawn whole, as before"
    );
    let uv = sized_uv(rect, &tex, false);
    let shown_w = uv.width() * 800.0;
    let shown_h = uv.height() * 600.0;
    let scale_x = rect.width() / shown_w;
    let scale_y = rect.height() / shown_h;
    assert!(
        (scale_x - scale_y).abs() < 1e-4,
        "uniform scale, got {scale_x} x {scale_y}"
    );
    assert!(uv.min.x >= 0.0 && uv.max.x <= 1.0 && uv.min.y >= 0.0 && uv.max.y <= 1.0);
}

#[test]
fn painting_does_not_rehash_the_pattern() {
    let cfg = BackgroundConfig::classic_meadow();
    let bytes = cfg.pattern_bytes.clone().expect("classic pattern");
    const CALLS: u32 = 2000;
    let started = Instant::now();
    for _ in 0..CALLS {
        let _ = hash_pattern_bytes(&bytes);
    }
    let rehash = started.elapsed();
    let started = Instant::now();
    for _ in 0..CALLS {
        let _ = cfg.cache_key(0, 736, 640, false);
    }
    let key = started.elapsed();
    println!(
        "measured: {} KB pattern, re-hash {:.4} ms/frame, whole cache key {:.5} ms/frame",
        bytes.len() / 1024,
        ms(rehash) / f64::from(CALLS),
        ms(key) / f64::from(CALLS)
    );
    assert!(key < rehash);
}

#[test]
fn cache_drops_least_recently_used_sized_textures_first() {
    let ctx = Context::default();
    let mut bg = ChatBackground::new();
    let cfg = BackgroundConfig::classic_meadow();
    let motion = BackgroundConfig::night_aurora();
    let motion_tex = bg.texture_for_config(&ctx, &motion, 64);

    for i in 0..MAX_CACHED_SIZED_TEXTURES as u32 + 3 {
        let mut c = cfg.clone();
        c.wallpaper.rotation = i as i32;
        let _ = bg.texture_for_config_sized(&ctx, &c, 64, 64);
    }
    assert_eq!(bg.cached_sizes().len(), MAX_CACHED_SIZED_TEXTURES);
    let again = bg.texture_for_config(&ctx, &motion, 64);
    assert_eq!(
        again.id(),
        motion_tex.id(),
        "the square texture survives the sized trim"
    );
}

#[test]
fn frames_pick_up_the_worker_render_without_waiting() {
    let ctx = Context::default();
    let mut bg = ChatBackground::new();
    let cfg = BackgroundConfig::classic_meadow();
    let _ = bg.texture_for_config_sized(&ctx, &cfg, 736, 688);
    let started = Instant::now();
    let mut frames = 0;
    loop {
        frames += 1;
        let (_, exact) = bg.texture_for_config_sized(&ctx, &cfg, 736, 672);
        if exact {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "stand-in never replaced"
        );
        std::thread::sleep(Duration::from_millis(16));
    }
    println!(
        "measured: exact texture after {frames} frames ({:.1} ms)",
        ms(started.elapsed())
    );
}
