use std::path::PathBuf;

use tg_editor::video::{
    ClipId, ItemVisibilityRange, MultiClipExportOptions, MultiClipPlayerEvent,
    MultiClipSegmentPlayer, MultiClipTimeline, OverlayVisibilityTrack, SEEK_THROTTLE_MS,
    SegmentPlaybackEvent, SegmentPlaybackMode, SegmentPlayer, SegmentPlayerOptions, TimelineSeeker,
    TrimTimelineDescriptor, TrimTimelineModel, VideoClip, VideoClipSource, VideoItemTimelineModel,
    VideoTrim, build_multi_clip_export_plan, is_ffmpeg_available, probe_video_source,
};
use tg_editor::{
    AudioTrackClip, BrushStroke, BrushTool, Color, CropEnhanceParams, CropMode, CropType,
    EditorError, ItemId, ItemPlacement, LocationData, LocationItem, PaintScene, PhotoModifications,
    PointF, Rect, RoundedCornersLevel, SCENE_DRAFT_BINARY_MAGIC, SCENE_DRAFT_VERSION, SceneDraft,
    ShapeItem, ShapeType, Size, StickerItem, StickerRef, StoryChannelPostItem, StoryLinkItem,
    StoryReactionItem, StrokePoint, TextAlignment, TextItem, TextStyle, TextTypeface,
    VideoTimelineDraft, WeatherData, WeatherItem, collect_scene_items, parse_scene_items_json,
    serialize_scene_items_json,
};

fn clip_source(duration_ms: i64, has_audio: bool, name: &str) -> VideoClipSource {
    VideoClipSource {
        path: Some(PathBuf::from(format!("/fake/media/{name}.mp4"))),
        content: Vec::new(),
        dimensions: Size::new(1920, 1080),
        duration_ms,
        fps: 30.0,
        has_audio,
        initial_trim: VideoTrim::default(),
        initial_volume: 1.0,
        round: false,
    }
}

fn build_rich_scene() -> (PaintScene, Vec<ItemId>) {
    let mut scene = PaintScene::from_size(Size::new(1080, 1920));
    let mut ids = Vec::new();

    let stroke_id = scene
        .add_stroke(BrushStroke::new(
            BrushTool::Pen,
            Color::rgb(234, 39, 57),
            8.0,
            vec![
                StrokePoint::new(100.0, 200.0),
                StrokePoint::new(400.0, 260.0),
            ],
        ))
        .expect("stroke added");
    ids.push(stroke_id);

    let sticker_id = scene.add_sticker(StickerItem {
        sticker: StickerRef {
            document_id: 9_001_002,
            set_short_name: "sample_pack".to_owned(),
            set_id: 42_000,
            set_access_hash: 99_887_766,
        },
        placement: ItemPlacement {
            position: PointF::new(540.0, 600.0),
            rotation: 15.0,
            scale: 1.25,
            z_value: 0.0,
            size: 220.0,
            aspect_ratio: 1.0,
            flipped: true,
        },
        image: None,
    });
    ids.push(sticker_id);

    let text_id = scene.add_text(TextItem::new(
        "Hello Timeline!",
        Color::rgb(255, 220, 80),
        36.0,
        TextStyle::Framed,
        TextTypeface::Condensed,
        TextAlignment::Left,
        Size::new(1080, 1920),
        PointF::new(540.0, 960.0),
    ));
    ids.push(text_id);

    let mut shape = ShapeItem::new(
        ShapeType::Arrow,
        Color::rgb(64, 167, 227),
        6.0,
        false,
        PointF::new(300.0, 1200.0),
        240.0,
    );
    shape.set_bend(0.45);
    let shape_id = scene.add_shape(shape);
    ids.push(shape_id);

    let loc_id = scene.add_location(LocationItem::new(
        LocationData {
            lat: 47.3769,
            lon: 8.5417,
            title: "Central Square".to_owned(),
            address: "Main St 1".to_owned(),
            country_iso2: "CH".to_owned(),
            city: "Sample City".to_owned(),
            provider: "sample_provider".to_owned(),
            venue_id: "v-100".to_owned(),
            venue_type: "plaza".to_owned(),
        },
        Size::new(1080, 1920),
        PointF::new(500.0, 1400.0),
    ));
    ids.push(loc_id);

    let weather_id = scene.add_weather(WeatherItem::new(
        WeatherData {
            emoji: "☀️".to_owned(),
            celsius: 23.5,
            color: Color::rgb(52, 152, 219),
        },
        Size::new(1080, 1920),
        PointF::new(720.0, 1450.0),
    ));
    ids.push(weather_id);

    let link_id = scene.add_story_link(StoryLinkItem::new(
        "https://example.invalid/story",
        "Example Link",
        Size::new(1080, 1920),
        PointF::new(540.0, 1600.0),
    ));
    ids.push(link_id);

    let reaction_id = scene.add_story_reaction(StoryReactionItem::new(
        "🔥",
        true,
        120.0,
        PointF::new(220.0, 1650.0),
    ));
    ids.push(reaction_id);

    let post_id = scene.add_story_channel_post(StoryChannelPostItem::new(
        1_000_001,
        77,
        "Channel update",
        Size::new(1080, 1920),
        PointF::new(540.0, 400.0),
    ));
    ids.push(post_id);

    (scene, ids)
}

#[test]
fn multi_clip_split_reorder_mute_trim_and_exact_boundary_mapping() {
    let mut tl = MultiClipTimeline::new();
    let id_a = tl.append(clip_source(4_000, true, "clip_a"));
    let id_b = tl.append(clip_source(5_000, true, "clip_b"));
    let id_c = tl.append(clip_source(3_000, false, "clip_c"));

    // Trim each clip:
    // clip A: [500..2500]  -> 2000ms
    // clip B: [1000..4000] -> 3000ms
    // clip C: [0..1500]    -> 1500ms
    tl.set_clip_trim(0, VideoTrim::new(500, 2_500))
        .expect("trim A");
    tl.set_clip_trim(1, VideoTrim::new(1_000, 4_000))
        .expect("trim B");
    tl.set_clip_trim(2, VideoTrim::new(0, 1_500))
        .expect("trim C");
    tl.set_clip_volume(0, 0.75).expect("volume A");
    tl.set_clip_muted(1, true).expect("mute B");

    assert_eq!(tl.total_duration_ms(), 6_500);
    assert!(tl.get(0).unwrap().sounding());
    assert_eq!(tl.get(0).unwrap().effective_volume(), 0.75);
    assert!(!tl.get(1).unwrap().sounding());
    assert_eq!(tl.get(1).unwrap().effective_volume(), 0.0);
    assert!(!tl.get(2).unwrap().sounding());

    // Invalid trims must be rejected.
    assert!(tl.set_clip_trim(0, VideoTrim::new(2_000, 2_000)).is_err());
    assert!(tl.set_clip_trim(0, VideoTrim::new(2_500, 1_000)).is_err());
    assert!(tl.set_clip_trim(0, VideoTrim::new(-10, 1_000)).is_err());
    assert!(tl.set_clip_trim(9, VideoTrim::new(0, 1_000)).is_err());

    // Split clip B at global 3200ms (1200ms into clip B -> local 2200ms).
    // Resulting 4 clips:
    // 0: A [500..2500]   -> global [0..2000)
    // 1: B1 [1000..2200] -> global [2000..3200) (muted preserved)
    // 2: B2 [2200..4000] -> global [3200..5000) (muted preserved)
    // 3: C [0..1500]     -> global [5000..6500]
    let (b_left, b_right) = tl.split_at_global_ms(3_200).expect("split clip B");
    assert_eq!(b_left, id_b);
    assert_ne!(b_right, id_b);
    assert_eq!(tl.len(), 4);
    assert_eq!(tl.total_duration_ms(), 6_500);
    assert!(tl.get(1).unwrap().is_muted());
    assert!(tl.get(2).unwrap().is_muted());

    // Splitting directly on boundaries or out of bounds must fail cleanly.
    assert!(tl.split_at_global_ms(0).is_err());
    assert!(tl.split_at_global_ms(2_000).is_err());
    assert!(tl.split_at_global_ms(3_200).is_err());
    assert!(tl.split_at_global_ms(6_500).is_err());
    assert!(tl.split_at_global_ms(-50).is_err());
    assert!(tl.split_at_global_ms(9_999).is_err());

    // Reorder: move clip C (index 3) to front (index 0).
    // New order:
    // 0: C  [0..1500]    -> global [0..1500)
    // 1: A  [500..2500]  -> global [1500..3500)
    // 2: B1 [1000..2200] -> global [3500..4700)
    // 3: B2 [2200..4000] -> global [4700..6500]
    tl.reorder(3, 0).expect("reorder C to front");
    assert!(tl.reorder(0, 10).is_err());
    assert_eq!(tl.total_duration_ms(), 6_500);

    let spans = tl.spans();
    assert_eq!(spans.len(), 4);
    assert_eq!(spans[0].clip_id, id_c);
    assert_eq!(spans[1].clip_id, id_a);
    assert_eq!(spans[2].clip_id, b_left);
    assert_eq!(spans[3].clip_id, b_right);

    // Monotonic contiguous global spans.
    let mut prev_end = 0;
    for span in &spans {
        assert_eq!(span.global_start_ms, prev_end);
        assert!(span.global_end_ms > span.global_start_ms);
        prev_end = span.global_end_ms;
    }
    assert_eq!(prev_end, 6_500);

    // Exact (clip_index, local_ms) resolution across every boundary and clamped out-of-range seeks.
    let cases = [
        (-500, 0, 0, 0),           // clamped < 0
        (0, 0, 0, 0),              // start of C
        (1_499, 0, 1_499, 1_499),  // last ms of C
        (1_500, 1, 500, 1_500),    // boundary C -> A
        (3_499, 1, 2_499, 3_499),  // last ms of A
        (3_500, 2, 1_000, 3_500),  // boundary A -> B1
        (4_699, 2, 2_199, 4_699),  // last ms of B1
        (4_700, 3, 2_200, 4_700),  // boundary B1 -> B2
        (6_499, 3, 3_999, 6_499),  // 1ms before end
        (6_500, 3, 4_000, 6_500),  // exact end
        (99_999, 3, 4_000, 6_500), // clamped > total
    ];
    for (query_ms, exp_idx, exp_local, exp_global) in cases {
        let pos = tl.resolve_global_ms(query_ms).expect("resolved position");
        assert_eq!(
            (pos.clip_index, pos.local_ms, pos.global_ms),
            (exp_idx, exp_local, exp_global),
            "mismatch for global_ms={query_ms}"
        );
        // Round-trip back via global_ms_for.
        assert_eq!(
            tl.global_ms_for(pos.clip_index, pos.local_ms),
            Some(exp_global)
        );
    }

    // Verify monotonicity of resolved global timestamps across a fine sweep.
    let mut last_global = -1;
    for t in (-200..=7_000).step_by(50) {
        let pos = tl.resolve_global_ms(t).unwrap();
        assert!(pos.global_ms >= last_global);
        last_global = pos.global_ms;
    }
}

#[test]
fn headless_multi_clip_player_clock_transitions_scrub_and_single_clip_seeker() {
    let mut tl = MultiClipTimeline::new();
    let id0 = tl.append(clip_source(2_000, true, "c0"));
    let id1 = tl.append(clip_source(2_000, true, "c1"));
    let id2 = tl.append(clip_source(2_000, true, "c2"));
    tl.set_clip_trim(0, VideoTrim::new(200, 1_200)).unwrap(); // 1000ms
    tl.set_clip_trim(1, VideoTrim::new(500, 1_500)).unwrap(); // 1000ms
    tl.set_clip_trim(2, VideoTrim::new(0, 1_000)).unwrap(); // 1000ms
    tl.set_clip_volume(0, 0.5).unwrap();
    tl.set_clip_muted(1, true).unwrap();
    tl.set_clip_volume(2, 0.9).unwrap();

    let mut player = MultiClipSegmentPlayer::new(tl);
    assert_eq!(player.total_duration_ms(), 3_000);
    assert!(!player.is_playing());
    assert_eq!(player.active_audio_gain(), 0.5);

    // Advancing while paused is a no-op.
    let p = player.advance(500).unwrap();
    assert_eq!(p.global_ms, 0);

    // Start playing and advance across clip 0 -> clip 1 -> clip 2 in one jump (2300ms).
    player.play();
    let pos = player.advance(2_300).unwrap();
    assert_eq!(pos.clip_index, 2);
    assert_eq!(pos.clip_id, id2);
    assert_eq!(pos.local_ms, 300);
    assert_eq!(pos.global_ms, 2_300);
    assert_eq!(player.active_audio_gain(), 0.9);

    let events = player.drain_events();
    assert_eq!(
        events,
        vec![
            MultiClipPlayerEvent::ClipTransition {
                from_clip: Some(0),
                to_clip: 1,
                to_clip_id: id1,
                global_ms: 1_000,
                local_ms: 500,
            },
            MultiClipPlayerEvent::ClipTransition {
                from_clip: Some(1),
                to_clip: 2,
                to_clip_id: id2,
                global_ms: 2_000,
                local_ms: 0,
            },
        ]
    );

    // Advance past total duration with looping enabled -> wraps cleanly to clip 0.
    let wrapped = player.advance(950).unwrap(); // 2300 + 950 = 3250 -> 250ms into clip 0 (local 450)
    assert_eq!(wrapped.clip_index, 0);
    assert_eq!(wrapped.clip_id, id0);
    assert_eq!(wrapped.local_ms, 450);
    assert_eq!(wrapped.global_ms, 250);
    let loop_events = player.drain_events();
    assert!(loop_events.contains(&MultiClipPlayerEvent::Looped));

    // Disable looping and advance to end -> clamps at 3000ms and pauses.
    player.set_looping(false);
    let ended = player.advance(5_000).unwrap();
    assert_eq!(ended.global_ms, 3_000);
    assert_eq!(ended.clip_index, 2);
    assert_eq!(ended.local_ms, 1_000);
    assert!(!player.is_playing());
    assert!(player.drain_events().contains(&MultiClipPlayerEvent::Ended));

    // Throttled scrub gesture on multi-clip player.
    player.set_looping(true);
    player.begin_scrub();
    let first = player.scrub_to(1_250, 1_000).expect("initial scrub fires");
    assert_eq!(first.clip_index, 1);
    assert_eq!(first.local_ms, 750);
    assert_eq!(player.active_audio_gain(), 0.0); // clip 1 is muted
    // Within 120ms throttle window -> coalesced.
    assert!(player.scrub_to(1_600, 1_050).is_none());
    assert!(player.scrub_to(1_800, 1_090).is_none());
    assert!(player.flush_scrub(1_119).is_none());
    let flushed = player
        .flush_scrub(1_000 + SEEK_THROTTLE_MS)
        .expect("coalesced scrub flushes");
    assert_eq!(flushed.global_ms, 1_800);
    let done = player.end_scrub().unwrap();
    assert_eq!(done.global_ms, 1_800);

    // Mutating timeline shortens total duration -> player position re-clamps cleanly.
    player.mutate_timeline(|tl| {
        tl.remove(2).unwrap();
        tl.remove(1).unwrap();
    });
    assert_eq!(player.total_duration_ms(), 1_000);
    assert_eq!(player.global_position_ms(), 1_000);
    assert_eq!(player.current_position().unwrap().clip_index, 0);

    // Single-clip SegmentPlayer + TimelineSeeker sound-fallback & drag behavior.
    let mut sp = SegmentPlayer::new(SegmentPlayerOptions {
        keep_alpha: false,
        audio: false,
        volume: 0.8,
    });
    sp.start();
    sp.set_segment(500, 2_000);
    sp.set_sound(true);
    assert_eq!(sp.mode(), SegmentPlaybackMode::Both);
    assert!(sp.paused_other_playback());
    sp.handle_error();
    assert_eq!(sp.mode(), SegmentPlaybackMode::Video);
    let sp_events = sp.drain_events();
    assert!(sp_events.contains(&SegmentPlaybackEvent::SoundFallback));

    // Reaching segment end loops back to from_ms.
    sp.handle_position(2_000);
    assert_eq!(sp.position_ms(), 500);

    let mut seeker = TimelineSeeker::new();
    seeker.begin_dragging(&mut sp);
    assert!(sp.held());
    seeker.request_seek(1_200, 100);
    seeker.request_seek(1_950, 140);
    assert!(seeker.tick(219, &mut sp).is_none());
    assert_eq!(seeker.tick(100 + SEEK_THROTTLE_MS, &mut sp), Some(1_950));
    // Finishing drag at till_ms wraps cleanly to from_ms.
    seeker.request_seek(2_000, 300);
    let resumed = seeker.finish_dragging(500, 2_000, &mut sp);
    assert_eq!(resumed, 500);
    assert!(!sp.held());
}

#[test]
fn overlay_item_visibility_windows_and_video_item_timeline() {
    let (mut scene, ids) = build_rich_scene();
    let item_a = ids[0]; // stroke
    let item_b = ids[1]; // sticker
    let item_c = ids[2]; // text
    let item_d = ids[3]; // shape (left unbounded -> visible at all timestamps)

    // Invalid visibility ranges must be rejected.
    assert!(matches!(
        ItemVisibilityRange::new(1_000, 1_000),
        Err(EditorError::InvalidVisibilityRange {
            start_ms: 1_000,
            end_ms: 1_000
        })
    ));
    assert!(matches!(
        ItemVisibilityRange::new(2_500, 1_000),
        Err(EditorError::InvalidVisibilityRange {
            start_ms: 2_500,
            end_ms: 1_000
        })
    ));
    assert!(matches!(
        ItemVisibilityRange::new(-100, 500),
        Err(EditorError::InvalidVisibilityRange {
            start_ms: -100,
            end_ms: 500
        })
    ));

    let mut trim_tl = TrimTimelineModel::new(TrimTimelineDescriptor {
        duration_ms: 6_000,
        ..TrimTimelineDescriptor::default()
    });
    trim_tl.set_item_visibility(item_a, 0, 2_000).unwrap();
    trim_tl.set_item_visibility(item_b, 2_000, 4_000).unwrap();
    trim_tl.set_item_visibility(item_c, 1_000, 3_500).unwrap();

    // Reject invalid range through TrimTimelineModel as well.
    assert!(trim_tl.set_item_visibility(item_d, 3_000, 3_000).is_err());
    assert!(trim_tl.set_item_visibility(item_d, 4_000, 2_000).is_err());

    // At 500ms: item_a visible, item_b hidden, item_c hidden, item_d (unbounded) visible.
    let at_500 = trim_tl.active_overlay_items_at(&scene, 500);
    assert!(at_500.contains(&item_a));
    assert!(!at_500.contains(&item_b));
    assert!(!at_500.contains(&item_c));
    assert!(at_500.contains(&item_d));

    // At 1500ms: item_a and item_c visible, item_b hidden.
    let at_1500 = trim_tl.active_overlay_items_at(&scene, 1_500);
    assert!(at_1500.contains(&item_a));
    assert!(!at_1500.contains(&item_b));
    assert!(at_1500.contains(&item_c));

    // Exact boundary 2000ms: [0, 2000) ends and [2000, 4000) begins -> no double activation.
    let at_2000 = trim_tl.active_overlay_items_at(&scene, 2_000);
    assert!(!at_2000.contains(&item_a));
    assert!(at_2000.contains(&item_b));
    assert!(at_2000.contains(&item_c));

    // At 3500ms: item_c [1000, 3500) ends; item_b still active.
    let at_3500 = trim_tl.active_overlay_items_at(&scene, 3_500);
    assert!(!at_3500.contains(&item_a));
    assert!(at_3500.contains(&item_b));
    assert!(!at_3500.contains(&item_c));

    // At 4000ms: all three bounded items hidden, unbounded item_d still active.
    let at_4000 = trim_tl.active_overlay_items_at(&scene, 4_000);
    assert!(!at_4000.contains(&item_a));
    assert!(!at_4000.contains(&item_b));
    assert!(!at_4000.contains(&item_c));
    assert!(at_4000.contains(&item_d));

    // Removing a scene item excludes it from active overlays and prunes cleanly.
    assert!(scene.remove_item(item_b));
    let at_2500 = trim_tl.active_overlay_items_at(&scene, 2_500);
    assert!(!at_2500.contains(&item_b));
    trim_tl.overlay_visibility_mut().prune_missing(&scene);
    assert!(trim_tl.overlay_visibility().range_for(item_b).is_none());

    // VideoItemTimelineModel controls embedded video item clip trim, volume, and seeker.
    let mut item_tl = VideoItemTimelineModel::new();
    let clip = VideoClip::new(ClipId(10), clip_source(5_000, true, "overlay_pip"));
    item_tl.set_clip(Some(clip));
    assert_eq!(item_tl.apply_trim(1_000, 3_500, 0), Some(2_500));
    assert_eq!(item_tl.clip().unwrap().loop_duration_ms(), 2_500);
    assert_eq!(item_tl.drain_length_changes(), vec![2_500]);
    item_tl.set_volume(0.4);
    assert_eq!(item_tl.clip().unwrap().volume(), 0.4);
    assert_eq!(item_tl.player().unwrap().volume(), 0.4);
    item_tl.begin_drag();
    assert!(item_tl.player().unwrap().held());
    assert_eq!(item_tl.end_drag(), Some(1_000));
    assert!(!item_tl.player().unwrap().held());
}

#[test]
fn scene_draft_json_and_binary_round_trip_and_future_version_rejection() {
    let (scene, ids) = build_rich_scene();

    let mut timeline = MultiClipTimeline::new();
    timeline.append(clip_source(4_000, true, "intro"));
    timeline.append(clip_source(6_000, true, "main"));
    timeline.append(clip_source(2_500, false, "outro"));
    timeline
        .set_clip_trim(0, VideoTrim::new(300, 3_300))
        .unwrap();
    timeline
        .set_clip_trim(1, VideoTrim::new(1_000, 5_000))
        .unwrap();
    timeline.set_clip_volume(0, 0.85).unwrap();
    timeline.set_clip_muted(1, true).unwrap();

    let mut visibility = OverlayVisibilityTrack::new();
    visibility.set_range(ids[0], 0, 2_500).unwrap();
    visibility.set_range(ids[2], 1_500, 6_000).unwrap();

    let mut audio_track = AudioTrackClip::new("Sample Beat", "Synth Artist", 15_000);
    audio_track.trim_from_ms = 2_000;
    audio_track.trim_till_ms = 10_000;
    audio_track.volume = 0.65;

    let crop_enhance = CropEnhanceParams {
        angle: 90,
        flipped: true,
        crop: Some(Rect::new(40, 80, 960, 1440)),
        crop_type: CropType::RoundedRect,
        crop_mode: CropMode::Mask,
        corners_level: RoundedCornersLevel::Medium,
        canvas_media_rect: Some(Rect::new(0, 0, 1080, 1920)),
        enhance: 65,
    };

    let draft = SceneDraft::new(
        crop_enhance,
        scene.clone(),
        Some(VideoTimelineDraft {
            timeline,
            overlay_visibility: visibility,
            quality: 720,
            gif: false,
            cover_ms: 1_250,
            audio_track: Some(audio_track),
        }),
    );
    let orig_fp = draft.fingerprint();

    // JSON round-trip must be identical.
    let json = draft.to_json().expect("serialize JSON draft");
    let from_json = SceneDraft::from_json(&json).expect("deserialize JSON draft");
    assert_eq!(from_json, draft);
    assert_eq!(from_json.fingerprint(), orig_fp);

    // Binary round-trip must be identical.
    let bytes = draft.to_bytes().expect("serialize binary draft");
    assert_eq!(&bytes[0..4], &SCENE_DRAFT_BINARY_MAGIC);
    let from_bytes = SceneDraft::from_bytes(&bytes).expect("deserialize binary draft");
    assert_eq!(from_bytes, draft);
    assert_eq!(from_bytes.fingerprint(), orig_fp);

    // Editing draft changes fingerprint deterministically.
    let mut edited = draft.clone();
    edited.crop_enhance.enhance = 90;
    assert_ne!(edited.fingerprint(), orig_fp);

    // Unsupported future versions in JSON (even with unknown future fields) must fail cleanly.
    let future_json = format!(
        r#"{{"version": {}, "brand_new_field": [1, 2, 3]}}"#,
        SCENE_DRAFT_VERSION + 1
    );
    assert!(matches!(
        SceneDraft::from_json(&future_json),
        Err(EditorError::UnsupportedDraftVersion {
            found,
            max_supported
        }) if found == SCENE_DRAFT_VERSION + 1 && max_supported == SCENE_DRAFT_VERSION
    ));
    let zero_version_json = r#"{"version": 0}"#;
    assert!(matches!(
        SceneDraft::from_json(zero_version_json),
        Err(EditorError::UnsupportedDraftVersion { found: 0, .. })
    ));

    // Unsupported future version in binary envelope must fail cleanly before payload parsing.
    let mut future_bin = bytes.clone();
    let future_ver = SCENE_DRAFT_VERSION + 5;
    future_bin[4..8].copy_from_slice(&future_ver.to_le_bytes());
    assert!(matches!(
        SceneDraft::from_bytes(&future_bin),
        Err(EditorError::UnsupportedDraftVersion {
            found,
            max_supported
        }) if found == future_ver && max_supported == SCENE_DRAFT_VERSION
    ));

    // Corrupted checksum and bad magic in binary envelope must be rejected cleanly.
    let mut bad_crc = bytes.clone();
    let last = bad_crc.len() - 1;
    bad_crc[last] ^= 0xFF;
    assert!(matches!(
        SceneDraft::from_bytes(&bad_crc),
        Err(EditorError::DraftDeserialization(_))
    ));
    let mut bad_magic = bytes;
    bad_magic[0] = b'X';
    assert!(matches!(
        SceneDraft::from_bytes(&bad_magic),
        Err(EditorError::DraftDeserialization(_))
    ));

    // tdesktop-compatible flat SceneItemsDraft JSON round-trip.
    let flat = collect_scene_items(&scene);
    assert!(!flat.is_empty());
    assert!(flat.max_z() >= 9000.0);
    let flat_json = serialize_scene_items_json(&flat);
    let parsed_flat = parse_scene_items_json(&flat_json).expect("parse flat scene items");
    assert_eq!(parsed_flat, flat);
}

#[test]
fn multi_clip_ffmpeg_export_plan_filtergraph_concat_and_overlays() {
    let (scene, ids) = build_rich_scene();
    let mut tl = MultiClipTimeline::new();
    tl.append(clip_source(3_000, true, "part1"));
    tl.append(clip_source(4_000, true, "part2"));
    tl.append(clip_source(2_000, false, "part3"));

    tl.set_clip_trim(0, VideoTrim::new(500, 2_500)).unwrap(); // 2.0s audio @ 0.8
    tl.set_clip_volume(0, 0.8).unwrap();
    tl.set_clip_trim(1, VideoTrim::new(1_000, 3_500)).unwrap(); // 2.5s muted -> anullsrc
    tl.set_clip_muted(1, true).unwrap();
    tl.set_clip_trim(2, VideoTrim::new(0, 1_500)).unwrap(); // 1.5s no audio -> anullsrc

    let mut vis = OverlayVisibilityTrack::new();
    vis.set_range(ids[2], 1_250, 4_500).unwrap();

    let options = MultiClipExportOptions {
        geometry: PhotoModifications {
            angle: 90,
            flipped: true,
            crop: Some(Rect::new(100, 50, 1280, 720)),
            ..PhotoModifications::default()
        },
        quality: 480,
        fps_limit: 30.0,
        gif: false,
        remove_audio: false,
        audio_sample_rate: 48_000,
        target_dimensions: None,
    };

    let out_path = PathBuf::from("/fake/out/merged.mp4");
    let plan = build_multi_clip_export_plan(&tl, Some((&scene, &vis)), &options, &out_path)
        .expect("build multi-clip export plan");

    assert_eq!(plan.inputs.len(), 3);
    assert_eq!(plan.total_duration_ms, 6_000);
    assert!(plan.has_audio);
    assert_eq!(plan.video_map_label, "[vout]");
    assert_eq!(plan.audio_map_label.as_deref(), Some("[aout]"));
    // 1280x720 rotated 90° -> 720x1280 -> shorter side 480 -> 480x852 (even_down(853) = 852).
    assert_eq!(plan.output_dimensions, Size::new(480, 852));

    assert!(
        plan.filter_complex
            .contains("[0:v]trim=start=0.500:end=2.500")
    );
    assert!(
        plan.filter_complex
            .contains("crop=1280:720:100:50,transpose=1,hflip")
    );
    assert!(
        plan.filter_complex
            .contains("[0:a]atrim=start=0.500:end=2.500")
    );
    assert!(plan.filter_complex.contains("volume=0.800"));
    // Clip 1 (muted) and Clip 2 (no audio) must synthesize silence so concat succeeds.
    assert!(plan.filter_complex.contains("anullsrc=channel_layout=stereo:sample_rate=48000,atrim=duration=2.500,asetpts=PTS-STARTPTS[a1]"));
    assert!(plan.filter_complex.contains("anullsrc=channel_layout=stereo:sample_rate=48000,atrim=duration=1.500,asetpts=PTS-STARTPTS[a2]"));
    assert!(
        plan.filter_complex
            .contains("[v0][a0][v1][a1][v2][a2]concat=n=3:v=1:a=1[vout][aout]")
    );

    // Overlay visibility expression generated for the timed text item.
    let text_overlay = plan
        .overlays
        .iter()
        .find(|o| o.item_id == ids[2])
        .expect("text overlay spec");
    assert_eq!(text_overlay.kind, "text");
    assert_eq!(
        text_overlay.enable_expr.as_deref(),
        Some("between(t,1.250,4.500)")
    );

    // GIF mode strips audio and produces concat=n=3:v=1:a=0.
    let gif_options = MultiClipExportOptions {
        gif: true,
        ..MultiClipExportOptions::default()
    };
    let gif_plan = build_multi_clip_export_plan(&tl, None, &gif_options, &out_path)
        .expect("build GIF export plan");
    assert!(!gif_plan.has_audio);
    assert!(gif_plan.audio_map_label.is_none());
    assert!(
        gif_plan
            .filter_complex
            .contains("[v0][v1][v2]concat=n=3:v=1:a=0[vout]")
    );
    assert!(gif_plan.ffmpeg_args.contains(&"-an".to_owned()));

    // Empty timeline must return an error.
    let empty = MultiClipTimeline::new();
    assert!(build_multi_clip_export_plan(&empty, None, &options, &out_path).is_err());
}

#[test]
fn multi_clip_timeline_with_small_generated_local_clips() {
    if !is_ffmpeg_available() {
        eprintln!("skipping local fixture clip test: FFmpeg runtime DLLs unavailable");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let clip1_path = dir.path().join("c1.mp4");
    let clip2_path = dir.path().join("c2.mp4");

    tg_video::generate_test_clip(&clip1_path, 160, 120, 15, 1_000, true).expect("generate clip1");
    tg_video::generate_test_clip(&clip2_path, 160, 120, 15, 1_200, false).expect("generate clip2");

    let info1 = probe_video_source(&clip1_path).expect("probe clip1");
    let info2 = probe_video_source(&clip2_path).expect("probe clip2");

    let mut tl = MultiClipTimeline::new();
    tl.append(VideoClipSource::from_path(
        &clip1_path,
        info1.duration_ms,
        info1.dimensions,
        info1.audio.has_audio,
    ));
    tl.append(VideoClipSource::from_path(
        &clip2_path,
        info2.duration_ms,
        info2.dimensions,
        info2.audio.has_audio,
    ));

    assert_eq!(tl.len(), 2);
    assert!(tl.total_duration_ms() >= 1_800);

    let out_path = dir.path().join("concat.mp4");
    let plan =
        build_multi_clip_export_plan(&tl, None, &MultiClipExportOptions::default(), &out_path)
            .expect("build plan for generated fixtures");
    assert_eq!(plan.output_dimensions, Size::new(160, 120));
    assert!(plan.has_audio);
    assert!(
        plan.filter_complex
            .contains("concat=n=2:v=1:a=1[vout][aout]")
    );
}
