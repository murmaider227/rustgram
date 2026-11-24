use image::{Rgba, RgbaImage};
use tg_editor::{
    Brush, BrushSizeSlider, BrushState, BrushStroke, BrushTool, CROP_MIN_SIZE, Color,
    ColorSliderMode, ColorSliderState, CropController, CropCursor, CropEdge, CropMode, CropPreset,
    CropType, DEFAULT_PALETTE, EditorData, EditorError, HslColor, HsvColor, ItemPlacement,
    MAX_BRUSH_WIDTH, MIN_BRUSH_SIZE_RATIO, MIN_BRUSH_WIDTH, PHOTO_JPEG_QUALITY,
    PHOTO_SIDE_LIMIT_LARGE, PHOTO_SIDE_LIMIT_STANDARD, PROFILE_PHOTO_SIZE, PaintScene,
    PhotoExportSettings, PhotoModifications, PointF, Rect, RoundedCornersLevel, SaveState, Size,
    SizeF, StickerItem, StickerRef, StrokeBuilder, StrokePoint, TextAlignment, TextPrefs,
    TextStyle, TextTypeface, apply, apply_and_export_jpeg, apply_shape_mask, apply_with_stickers,
    compute_arrow_head_wings, compute_inner_layout, enhance_image, export_jpeg,
    finalize_profile_photo, hsl_to_rgba, hsv_to_rgba, opaque_on_white, photo_side_limit,
    pixmap_to_rgba, prepare_profile_photo_input, rasterize_stroke, rgb_to_hsl, rgb_to_hsv,
    scale_to_fit, smooth_stroke, stack_blur_rgba, validate_profile_photo_dimensions,
    validate_thumb_dimensions,
};
use tiny_skia::{Pixmap, Transform};

/// Builds a WxH image where every pixel uniquely encodes `(x, y)` in `(R, G, B, A)`.
fn unique_pixel_image(w: u32, h: u32) -> RgbaImage {
    let mut img = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.put_pixel(x, y, Rgba([x as u8, y as u8, (x * 7 + y * 13) as u8, 255]));
        }
    }
    img
}

#[test]
fn apply_rotate_and_flip_all_eight_orientations_match_qt() {
    let w = 4u32;
    let h = 3u32;
    let src = unique_pixel_image(w, h);

    for &angle in &[0, 90, 180, 270] {
        for &flipped in &[false, true] {
            let mods = PhotoModifications {
                angle,
                flipped,
                ..Default::default()
            };
            let out = apply(&src, &mods);

            let (expected_w, expected_h) = match angle {
                90 | 270 => (h, w),
                _ => (w, h),
            };
            assert_eq!(
                (out.width(), out.height()),
                (expected_w, expected_h),
                "size mismatch for angle={angle}, flipped={flipped}"
            );

            for y in 0..h {
                for x in 0..w {
                    let expected_px = *src.get_pixel(x, y);
                    // Qt order: rotate CW first, then flip horizontally.
                    let (rx, ry) = match angle {
                        0 => (x, y),
                        90 => (h - 1 - y, x),
                        180 => (w - 1 - x, h - 1 - y),
                        270 => (y, w - 1 - x),
                        _ => unreachable!(),
                    };
                    let fx = if flipped { expected_w - 1 - rx } else { rx };
                    assert_eq!(
                        *out.get_pixel(fx, ry),
                        expected_px,
                        "pixel mismatch at src=({x},{y}) -> dst=({fx},{ry}) for angle={angle}, flipped={flipped}"
                    );
                }
            }
        }
    }

    // Explicit known-answer check for angle=90 + flipped=true (transpose: (x, y) -> (y, x)).
    let mods = PhotoModifications {
        angle: 90,
        flipped: true,
        ..Default::default()
    };
    let out = apply(&src, &mods);
    assert_eq!(*out.get_pixel(0, 0), *src.get_pixel(0, 0));
    assert_eq!(*out.get_pixel(2, 3), *src.get_pixel(3, 2));
    assert_eq!(*out.get_pixel(0, 3), *src.get_pixel(3, 0));
    assert_eq!(*out.get_pixel(2, 0), *src.get_pixel(0, 2));
}

#[test]
fn apply_crop_then_rotate_and_flip_known_pixels() {
    let src = unique_pixel_image(10, 8);
    let mods = PhotoModifications {
        crop: Some(Rect::new(2, 1, 4, 3)),
        angle: 90,
        flipped: false,
        ..Default::default()
    };
    let out = apply(&src, &mods);
    // Cropped region is 4x3 covering src x in 2..6, y in 1..4.
    // Rotated 90° CW -> 3x4:
    // src (2, 1) [crop (0, 0)] -> (2, 0)
    // src (5, 1) [crop (3, 0)] -> (2, 3)
    // src (2, 3) [crop (0, 2)] -> (0, 0)
    // src (5, 3) [crop (3, 2)] -> (0, 3)
    assert_eq!((out.width(), out.height()), (3, 4));
    assert_eq!(*out.get_pixel(2, 0), *src.get_pixel(2, 1));
    assert_eq!(*out.get_pixel(2, 3), *src.get_pixel(5, 1));
    assert_eq!(*out.get_pixel(0, 0), *src.get_pixel(2, 3));
    assert_eq!(*out.get_pixel(0, 3), *src.get_pixel(5, 3));
}

#[test]
fn apply_expanded_crop_fills_canvas_background() {
    let src = RgbaImage::from_pixel(40, 40, Rgba([40, 120, 200, 255]));
    let mods = PhotoModifications {
        crop: Some(Rect::new(-10, -10, 60, 60)),
        ..Default::default()
    };
    let out = apply(&src, &mods);
    assert_eq!((out.width(), out.height()), (60, 60));
    // Original image sits at (10..50, 10..50).
    assert_eq!(*out.get_pixel(25, 25), Rgba([40, 120, 200, 255]));
    // Expanded margin pixel at (2, 2) must be opaque dominant canvas background (not transparent).
    let border = *out.get_pixel(2, 2);
    assert_eq!(border[3], 255);
}

#[test]
fn apply_shape_mask_ellipse_and_rounded_rect() {
    let mut img = RgbaImage::from_pixel(100, 100, Rgba([200, 50, 80, 255]));
    let mods = PhotoModifications {
        crop_type: CropType::Ellipse,
        crop_mode: CropMode::Mask,
        ..Default::default()
    };
    apply_shape_mask(&mut img, &mods);
    // Corner outside circle is transparent; center is fully opaque.
    assert_eq!(img.get_pixel(0, 0)[3], 0);
    assert_eq!(img.get_pixel(99, 99)[3], 0);
    assert_eq!(img.get_pixel(50, 50)[3], 255);

    let mut rounded = RgbaImage::from_pixel(100, 100, Rgba([10, 200, 30, 255]));
    let rmods = PhotoModifications {
        crop_type: CropType::RoundedRect,
        crop_mode: CropMode::Mask,
        corners_level: RoundedCornersLevel::Large,
        ..Default::default()
    };
    apply_shape_mask(&mut rounded, &rmods);
    assert_eq!(rounded.get_pixel(0, 0)[3], 0);
    assert_eq!(rounded.get_pixel(50, 2)[3], 255);
    assert_eq!(rounded.get_pixel(50, 50)[3], 255);

    // Hint mode must not alter alpha.
    let mut hint = RgbaImage::from_pixel(40, 40, Rgba([10, 20, 30, 255]));
    let hmods = PhotoModifications {
        crop_type: CropType::Ellipse,
        crop_mode: CropMode::Hint,
        ..Default::default()
    };
    apply_shape_mask(&mut hint, &hmods);
    assert_eq!(hint.get_pixel(0, 0)[3], 255);
}

#[test]
fn brush_builder_smoothing_and_deterministic_rasterization() {
    // 1. StrokeBuilder filters tiny moves, subdivides long jumps, and handles origin jump.
    let mut builder = StrokeBuilder::new(
        BrushTool::Pen,
        Color::rgb(255, 0, 0),
        8.0,
        1.0,
        SizeF::new(200.0, 200.0),
    );
    builder.push_point(PointF::new(10.0, 50.0), 1.0, 0);
    builder.push_point(PointF::new(10.5, 50.0), 1.0, 1); // < 2.0 -> ignored
    assert_eq!(builder.points().len(), 1);
    builder.push_point(PointF::new(70.0, 50.0), 1.0, 10); // 60px -> subdivided into 4 steps
    assert_eq!(builder.points().len(), 5);
    let stroke = builder.finish().expect("valid stroke");

    // 2. Smoothing keeps indices 0, 1, and last fixed while smoothing interior points.
    let zigzag = vec![
        StrokePoint::new(0.0, 0.0),
        StrokePoint::new(10.0, 0.0),
        StrokePoint::new(20.0, 20.0),
        StrokePoint::new(30.0, 0.0),
        StrokePoint::new(40.0, 0.0),
    ];
    let smoothed = smooth_stroke(&zigzag);
    assert_eq!(smoothed[0].pos, zigzag[0].pos);
    assert_eq!(smoothed[1].pos, zigzag[1].pos);
    assert_eq!(smoothed[4].pos, zigzag[4].pos);
    assert!(smoothed[2].pos.y < 20.0 && smoothed[2].pos.y > 0.0);

    // 3. Deterministic rasterization check (two independent runs are byte-identical).
    let mut pm1 = Pixmap::new(100, 100).unwrap();
    let mut pm2 = Pixmap::new(100, 100).unwrap();
    rasterize_stroke(&mut pm1, &stroke, Transform::identity());
    rasterize_stroke(&mut pm2, &stroke, Transform::identity());
    assert_eq!(pm1.data(), pm2.data());

    let rgba = pixmap_to_rgba(&pm1);
    // Midpoint on the line is solid red.
    assert_eq!(*rgba.get_pixel(40, 50), Rgba([255, 0, 0, 255]));
    // Distant pixel is untouched transparent.
    assert_eq!(*rgba.get_pixel(40, 10), Rgba([0, 0, 0, 0]));
}

#[test]
fn brush_marker_arrow_eraser_and_blur_tools() {
    // Marker: semi-transparent (0.35 alpha) and does not double-darken self-overlap.
    let marker = BrushStroke::new(
        BrushTool::Marker,
        Color::rgb(0, 255, 0),
        10.0,
        vec![
            StrokePoint::new(10.0, 50.0),
            StrokePoint::new(90.0, 50.0),
            StrokePoint::new(10.0, 50.0), // retraces over itself
        ],
    );
    let mut pm = Pixmap::new(100, 100).unwrap();
    rasterize_stroke(&mut pm, &marker, Transform::identity());
    let marker_rgba = pixmap_to_rgba(&pm);
    let mid = *marker_rgba.get_pixel(50, 50);
    // 0.35 * 255 = 89.25 -> ~89 (not ~147 from double compositing)
    assert!(
        (mid[3] as i32 - 89).abs() <= 2,
        "expected ~89 alpha, got {}",
        mid[3]
    );
    assert_eq!(mid[1], 255);

    // Arrowhead geometry: horizontal stroke pointing right -> wings point back-up and back-down.
    let arrow_pts = vec![StrokePoint::new(10.0, 50.0), StrokePoint::new(80.0, 50.0)];
    let (left, right) = compute_arrow_head_wings(&arrow_pts, 10.0).unwrap();
    assert!(left.x < 80.0 && right.x < 80.0);
    assert!((left.x - right.x).abs() < 1e-6);
    assert!((left.y + right.y - 100.0).abs() < 1e-6);
    assert!(left.y < 50.0 && right.y > 50.0);

    // Scene with Pen + Eraser: eraser removes pen pixels, undo brings them back.
    let bg = RgbaImage::from_pixel(100, 100, Rgba([20, 20, 20, 255]));
    let mut scene = PaintScene::from_size(Size::new(100, 100));
    scene.add_stroke(BrushStroke::new(
        BrushTool::Pen,
        Color::rgb(255, 0, 0),
        10.0,
        vec![StrokePoint::new(10.0, 50.0), StrokePoint::new(90.0, 50.0)],
    ));
    let before_erase = apply(
        &bg,
        &PhotoModifications {
            paint: Some(scene.clone()),
            ..Default::default()
        },
    );
    assert_eq!(*before_erase.get_pixel(50, 50), Rgba([255, 0, 0, 255]));

    scene.add_stroke(BrushStroke::new(
        BrushTool::Eraser,
        Color::BLACK,
        16.0,
        vec![StrokePoint::new(50.0, 30.0), StrokePoint::new(50.0, 70.0)],
    ));
    let after_erase = apply(
        &bg,
        &PhotoModifications {
            paint: Some(scene.clone()),
            ..Default::default()
        },
    );
    // Center of stroke was erased back to background; ends remain red.
    assert_eq!(*after_erase.get_pixel(50, 50), Rgba([20, 20, 20, 255]));
    assert_eq!(*after_erase.get_pixel(20, 50), Rgba([255, 0, 0, 255]));

    assert!(scene.undo());
    let after_undo = apply(
        &bg,
        &PhotoModifications {
            paint: Some(scene.clone()),
            ..Default::default()
        },
    );
    assert_eq!(*after_undo.get_pixel(50, 50), Rgba([255, 0, 0, 255]));

    // Blur brush blurs sharp black/white edge along the stroke only.
    let mut split = RgbaImage::new(100, 100);
    for y in 0..100 {
        for x in 0..100 {
            let v = if x < 50 { 0 } else { 255 };
            split.put_pixel(x, y, Rgba([v, v, v, 255]));
        }
    }
    let mut blur_scene = PaintScene::from_size(Size::new(100, 100));
    blur_scene.add_stroke(BrushStroke::new(
        BrushTool::Blur,
        Color::BLACK,
        10.0,
        vec![StrokePoint::new(30.0, 50.0), StrokePoint::new(70.0, 50.0)],
    ));
    let blurred_out = apply(
        &split,
        &PhotoModifications {
            paint: Some(blur_scene),
            ..Default::default()
        },
    );
    // Near the vertical seam on the stroke (y=50), pixels are blended gray.
    let seam_px = blurred_out.get_pixel(49, 50)[0];
    assert!(
        seam_px > 20 && seam_px < 235,
        "expected blurred pixel, got {seam_px}"
    );
    // Away from the stroke (y=5), the sharp seam is untouched.
    assert_eq!(blurred_out.get_pixel(49, 5)[0], 0);
    assert_eq!(blurred_out.get_pixel(50, 5)[0], 255);
}

#[test]
fn sticker_compositing_transforms_zorder_and_external_resolver() {
    // 20x20 sticker with distinct left (red) and right (blue) halves.
    let mut sticker_img = RgbaImage::new(20, 20);
    for y in 0..20 {
        for x in 0..20 {
            let color = if x < 10 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            };
            sticker_img.put_pixel(x, y, color);
        }
    }
    let green_sticker = RgbaImage::from_pixel(20, 20, Rgba([0, 255, 0, 255]));

    let bg = RgbaImage::from_pixel(100, 100, Rgba([10, 10, 10, 255]));
    let mut scene = PaintScene::from_size(Size::new(100, 100));

    let ref1 = StickerRef {
        document_id: 1001,
        set_short_name: "test_pack".into(),
        set_id: 42,
        set_access_hash: 99,
    };
    let ref2 = StickerRef {
        document_id: 1002,
        ..Default::default()
    };

    let s1 = scene.add_sticker(StickerItem {
        sticker: ref1.clone(),
        placement: ItemPlacement {
            position: PointF::new(50.0, 50.0),
            rotation: 0.0,
            scale: 1.0,
            z_value: 0.0,
            size: 20.0,
            aspect_ratio: 1.0,
            flipped: false,
        },
        image: None, // resolved externally!
    });

    // External resolver provides pixels for ref1.
    let out = apply_with_stickers(
        &bg,
        &PhotoModifications {
            paint: Some(scene.clone()),
            ..Default::default()
        },
        |r| match r.document_id {
            1001 => Some(&sticker_img),
            1002 => Some(&green_sticker),
            _ => None,
        },
    );
    // Unflipped: left side (44, 50) is red, right side (56, 50) is blue.
    assert_eq!(*out.get_pixel(44, 50), Rgba([255, 0, 0, 255]));
    assert_eq!(*out.get_pixel(56, 50), Rgba([0, 0, 255, 255]));

    // Flip horizontally -> left side becomes blue, right side becomes red.
    assert!(scene.flip_sticker(s1));
    let out_flipped = apply_with_stickers(
        &bg,
        &PhotoModifications {
            paint: Some(scene.clone()),
            ..Default::default()
        },
        |r| (r.document_id == 1001).then_some(&sticker_img),
    );
    assert_eq!(*out_flipped.get_pixel(44, 50), Rgba([0, 0, 255, 255]));
    assert_eq!(*out_flipped.get_pixel(56, 50), Rgba([255, 0, 0, 255]));

    // Add second overlapping green sticker on top, then lower it below s1.
    let s2 = scene.add_sticker(StickerItem::with_image(
        ref2,
        ItemPlacement {
            position: PointF::new(50.0, 50.0),
            size: 20.0,
            ..Default::default()
        },
        green_sticker.clone(),
    ));
    let top_green = apply_with_stickers(
        &bg,
        &PhotoModifications {
            paint: Some(scene.clone()),
            ..Default::default()
        },
        |r| (r.document_id == 1001).then_some(&sticker_img),
    );
    assert_eq!(*top_green.get_pixel(44, 50), Rgba([0, 255, 0, 255]));

    assert!(scene.lower_item(s2));
    let s1_on_top = apply_with_stickers(
        &bg,
        &PhotoModifications {
            paint: Some(scene.clone()),
            ..Default::default()
        },
        |r| (r.document_id == 1001).then_some(&sticker_img),
    );
    assert_eq!(*s1_on_top.get_pixel(44, 50), Rgba([0, 0, 255, 255]));

    // Undo z-order swap puts green back on top.
    assert!(scene.undo());
    let green_again = apply_with_stickers(
        &bg,
        &PhotoModifications {
            paint: Some(scene.clone()),
            ..Default::default()
        },
        |r| (r.document_id == 1001).then_some(&sticker_img),
    );
    assert_eq!(*green_again.get_pixel(44, 50), Rgba([0, 255, 0, 255]));
}

#[test]
fn scene_undo_redo_checkpoints_and_fingerprint() {
    let mut scene = PaintScene::from_size(Size::new(200, 200));
    let fp0 = scene.fingerprint();
    assert!(!scene.has_undo());
    assert!(!scene.has_redo());

    let stroke1 = BrushStroke::new(
        BrushTool::Pen,
        Color::rgb(255, 0, 0),
        5.0,
        vec![StrokePoint::new(10.0, 10.0), StrokePoint::new(50.0, 50.0)],
    );
    let id1 = scene.add_stroke(stroke1).unwrap();
    let fp1 = scene.fingerprint();
    assert_ne!(fp0, fp1);
    assert!(scene.has_undo());
    assert!(!scene.has_redo());

    scene.save(SaveState::Save);
    scene.save(SaveState::Keep);

    let s1 = scene.add_sticker(StickerItem {
        sticker: StickerRef {
            document_id: 777,
            ..Default::default()
        },
        placement: ItemPlacement {
            position: PointF::new(100.0, 100.0),
            size: 40.0,
            ..Default::default()
        },
        image: None,
    });
    let s1_z = match &scene.get(s1).unwrap().item {
        tg_editor::SceneItemKind::Sticker(s) => s.placement.z_value,
        _ => unreachable!(),
    };
    assert!(scene.set_sticker_placement(
        s1,
        ItemPlacement {
            position: PointF::new(120.0, 110.0),
            rotation: 45.0,
            scale: 1.5,
            z_value: s1_z,
            size: 40.0,
            ..Default::default()
        },
    ));
    assert!(scene.raise_item(id1));
    assert!(scene.remove_item(s1));
    assert_eq!(scene.active_items_by_z().len(), 1);

    // Undo remove -> sticker returns.
    assert!(scene.undo());
    assert_eq!(scene.active_items_by_z().len(), 2);
    // Undo raise -> s1 back on top.
    assert!(scene.undo());
    // Undo move -> sticker back at (100, 100).
    assert!(scene.undo());
    // Undo add sticker -> back to fp1.
    assert!(scene.undo());
    assert_eq!(scene.fingerprint(), fp1);
    assert!(scene.has_redo());

    // Redo all 4 steps.
    for _ in 0..4 {
        assert!(scene.redo());
    }
    assert!(!scene.has_redo());
    assert_eq!(scene.active_items_by_z().len(), 1);

    // Adding a new stroke after an undo clears the redo list.
    assert!(scene.undo());
    assert!(scene.has_redo());
    scene.add_stroke(BrushStroke::new(
        BrushTool::Pen,
        Color::rgb(0, 0, 255),
        4.0,
        vec![StrokePoint::new(5.0, 5.0), StrokePoint::new(15.0, 15.0)],
    ));
    assert!(!scene.has_redo());

    // Restore SaveState::Keep rolls back to checkpoint fp1.
    assert!(scene.restore(SaveState::Keep));
    assert_eq!(scene.fingerprint(), fp1);
}

#[test]
fn crop_controller_hit_test_drag_min_size_aspect_lock_and_rotation() {
    let image_size = Size::new(400, 200);
    let mods = PhotoModifications::default();
    let mut crop = CropController::new(&mods, image_size, EditorData::default());
    crop.apply_transform(
        Rect::new(0, 0, 600, 400),
        PointF::new(100.0, 100.0),
        0,
        false,
        SizeF::new(400.0, 200.0),
    );

    // Hit-test corners, edges, interior, and outside.
    assert_eq!(crop.hit_test(PointF::new(100.0, 100.0)), CropEdge::TOP_LEFT);
    assert_eq!(
        crop.hit_test(PointF::new(500.0, 100.0)),
        CropEdge::TOP_RIGHT
    );
    assert_eq!(
        crop.hit_test(PointF::new(100.0, 300.0)),
        CropEdge::BOTTOM_LEFT
    );
    assert_eq!(
        crop.hit_test(PointF::new(500.0, 300.0)),
        CropEdge::BOTTOM_RIGHT
    );
    assert_eq!(crop.hit_test(PointF::new(100.0, 200.0)), CropEdge::LEFT);
    assert_eq!(crop.hit_test(PointF::new(500.0, 200.0)), CropEdge::RIGHT);
    assert_eq!(crop.hit_test(PointF::new(300.0, 100.0)), CropEdge::TOP);
    assert_eq!(crop.hit_test(PointF::new(300.0, 300.0)), CropEdge::BOTTOM);
    assert_eq!(crop.hit_test(PointF::new(300.0, 200.0)), CropEdge::ALL);
    assert_eq!(crop.hit_test(PointF::new(10.0, 10.0)), CropEdge::NONE);
    assert_eq!(
        CropController::cursor_for_edge(CropEdge::TOP_LEFT),
        CropCursor::SizeFDiag
    );
    assert_eq!(
        CropController::cursor_for_edge(CropEdge::TOP_RIGHT),
        CropCursor::SizeBDiag
    );
    assert_eq!(crop.save_crop_rect(), None);

    // Drag top-left corner inward by (+50, +40).
    assert_eq!(
        crop.begin_drag(PointF::new(100.0, 100.0), false),
        CropEdge::TOP_LEFT
    );
    crop.update_drag(PointF::new(150.0, 140.0));
    assert!(crop.end_drag());
    assert_eq!(crop.crop_rect(), Rect::new(50, 40, 350, 160));
    assert_eq!(crop.save_crop_rect(), Some(Rect::new(50, 40, 350, 160)));

    // Try dragging top-left past minimum size -> clamped to CROP_MIN_SIZE (20px).
    assert_eq!(
        crop.begin_drag(PointF::new(150.0, 140.0), false),
        CropEdge::TOP_LEFT
    );
    crop.update_drag(PointF::new(600.0, 600.0));
    crop.end_drag();
    assert_eq!(crop.crop_rect().width, CROP_MIN_SIZE);
    assert_eq!(crop.crop_rect().height, CROP_MIN_SIZE);

    // Drag interior to move without resizing.
    let before_move = crop.crop_rect();
    let center = crop.crop_paint_f().center();
    assert_eq!(crop.begin_drag(center, false), CropEdge::ALL);
    crop.update_drag(PointF::new(center.x - 100.0, center.y - 50.0));
    crop.end_drag();
    let after_move = crop.crop_rect();
    assert_eq!(after_move.size(), before_move.size());
    assert_eq!(after_move.x, before_move.x - 100);
    assert_eq!(after_move.y, before_move.y - 50);

    // Lock to square (1:1) aspect ratio (userpic mode): side handles disappear.
    crop.set_aspect_ratio(CropPreset::Square.ratio(image_size));
    assert!(crop.keep_aspect_ratio());
    assert_eq!(crop.edge_rects().len(), 4);
    let sq = crop.crop_rect();
    assert_eq!(sq.width, sq.height);

    // Drag bottom-right corner inward: stays square!
    let br = PointF::new(crop.crop_paint_f().right(), crop.crop_paint_f().bottom());
    assert_eq!(crop.begin_drag(br, false), CropEdge::BOTTOM_RIGHT);
    crop.update_drag(PointF::new(br.x - 60.0, br.y - 30.0));
    crop.end_drag();
    let sq2 = crop.crop_rect();
    assert_eq!(sq2.width, sq2.height);
    assert!(sq2.width < sq.width);

    // Ctrl-expanded drag allows extending beyond original image bounds.
    crop.set_aspect_ratio(0.0); // freeform
    let tl = crop.crop_paint_f().top_left();
    assert_eq!(crop.begin_drag(tl, true), CropEdge::TOP_LEFT);
    crop.update_drag(PointF::new(50.0, 50.0));
    crop.end_drag();
    assert!(crop.crop_rect().x < 0);
    assert!(crop.crop_rect().y < 0);
}

#[test]
fn crop_controller_rotated_and_flipped_roundtrip() {
    let image_size = Size::new(200, 100);
    let mods = PhotoModifications {
        angle: 90,
        flipped: true,
        crop: Some(Rect::new(20, 10, 80, 40)),
        ..Default::default()
    };
    let mut crop = CropController::new(&mods, image_size, EditorData::default());
    crop.apply_transform(
        Rect::new(0, 0, 500, 500),
        PointF::new(100.0, 50.0),
        90,
        true,
        SizeF::new(200.0, 100.0),
    );
    // Starting and ending a zero-delta drag should preserve original crop coordinates.
    let center = crop.crop_paint_f().center();
    assert_eq!(crop.begin_drag(center, false), CropEdge::ALL);
    crop.update_drag(center);
    crop.end_drag();
    assert_eq!(crop.crop_rect(), Rect::new(20, 10, 80, 40));
}

#[test]
fn color_picker_hsl_hsv_size_slider_and_tdesktop_brush_serialization() {
    assert_eq!(DEFAULT_PALETTE.len(), 10);

    // Brush size slider math (matches ColorPicker::sizeControlYFromRatio / sizeControlRatioFromY).
    let slider = BrushSizeSlider::default();
    assert!((slider.ratio_from_y(slider.top()) - 1.0).abs() < 1e-9);
    assert!((slider.ratio_from_y(slider.bottom()) - MIN_BRUSH_SIZE_RATIO).abs() < 1e-9);
    assert_eq!(slider.y_from_ratio(1.0), slider.top());
    assert_eq!(slider.y_from_ratio(MIN_BRUSH_SIZE_RATIO), slider.bottom());
    // Midpoint ratio (0.55 = halfway between 0.1 and 1.0) maps to exact vertical midpoint.
    let mid_y = slider.y_from_ratio(0.55);
    assert!((mid_y - (slider.top() + slider.bottom()) / 2).abs() <= 1);
    assert!((slider.ratio_from_y(mid_y) - 0.5).abs() < 0.01);
    assert!((slider.handle_size(1.0) - MAX_BRUSH_WIDTH).abs() < 1e-9);
    assert!((slider.handle_size(0.0) - 4.0).abs() < 1e-9);
    assert!((Brush::default().stroke_width() - (MIN_BRUSH_WIDTH + 24.0 * 0.9)).abs() < 1e-9);

    // HSL & HSV conversions for primary colors.
    let red = Color::rgb(255, 0, 0);
    assert_eq!(
        rgb_to_hsl(red),
        HslColor {
            hue: 0,
            saturation: 255,
            lightness: 128
        }
    );
    // 8-bit HSL lightness 128/255 = 0.50196 quantizes pure red (255, 0, 0) to (255, 1, 1),
    // matching QColor::fromHsl(0, 255, 128).
    let roundtrip_red = hsl_to_rgba(rgb_to_hsl(red), 255);
    assert!((roundtrip_red.r as i32 - red.r as i32).abs() <= 1);
    assert!((roundtrip_red.g as i32 - red.g as i32).abs() <= 1);
    assert!((roundtrip_red.b as i32 - red.b as i32).abs() <= 1);
    assert_eq!(
        rgb_to_hsv(red),
        HsvColor {
            hue: 0,
            saturation: 255,
            value: 255
        }
    );
    assert_eq!(hsv_to_rgba(rgb_to_hsv(red), 255), red);

    // ColorSliderState HSL mode with lightness limits.
    let mut cs = ColorSliderState::new(ColorSliderMode::Hsl, Color::rgb(255, 255, 255));
    cs.set_lightness_limits(40, 200);
    assert!(rgb_to_hsl(cs.color()).lightness <= 200);
    cs.set_picker_position(120.0 / 360.0, 0.0); // pure green hue, full saturation
    cs.set_lightness_slider(0.5); // lightness = 120
    assert!(cs.color().g > cs.color().r && cs.color().g > cs.color().b);

    // BrushState tdesktop QDataStream roundtrip.
    let mut state = BrushState::default();
    state.select_tool(BrushTool::Arrow);
    state.set_color(Color::rgb(12, 34, 56));
    state.set_size_ratio(0.42);
    state.select_tool(BrushTool::Eraser);
    state.set_color(Color::rgb(255, 0, 0)); // normalized to black for fixed-color tool
    assert_eq!(state.active_brush().color, Color::BLACK);
    state.fill_shapes = true;
    state.text_prefs = TextPrefs {
        style: TextStyle::Framed,
        typeface: TextTypeface::Monospace,
        alignment: TextAlignment::Left,
        size_ratio: 0.75,
    };

    let bytes = state.serialize_tdesktop();
    let decoded = BrushState::deserialize_tdesktop(&bytes);
    assert_eq!(decoded.tool, BrushTool::Eraser);
    assert!(decoded.fill_shapes);
    assert_eq!(decoded.text_prefs.style, TextStyle::Framed);
    assert_eq!(decoded.text_prefs.typeface, TextTypeface::Monospace);
    assert_eq!(decoded.text_prefs.alignment, TextAlignment::Left);
    assert!((decoded.text_prefs.size_ratio - 0.75).abs() < 1e-4);
    assert_eq!(
        decoded.brushes[BrushTool::Arrow.ui_index()].color,
        Color::rgb(12, 34, 56)
    );
    assert!((decoded.brushes[BrushTool::Arrow.ui_index()].size_ratio - 0.42).abs() < 1e-4);

    // Truncated / malformed binary data gracefully falls back to defaults without panicking.
    assert_eq!(BrushState::deserialize_tdesktop(&[]), BrushState::default());
    assert_eq!(
        BrushState::deserialize_tdesktop(&[0xFF, 0xFF, 0xFF, 0xFE, 0x00]),
        BrushState::default()
    );
}

#[test]
fn photo_modifications_draft_json_roundtrip_renders_identically() {
    let bg = unique_pixel_image(80, 60);
    let mut scene = PaintScene::from_size(Size::new(80, 60));
    scene.add_stroke(BrushStroke::new(
        BrushTool::Pen,
        Color::rgb(250, 100, 20),
        6.0,
        vec![
            StrokePoint::with_pressure(10.0, 10.0, 0.8, 0),
            StrokePoint::with_pressure(40.0, 30.0, 1.0, 10),
            StrokePoint::with_pressure(70.0, 20.0, 0.9, 20),
        ],
    ));
    scene.add_stroke(BrushStroke::new(
        BrushTool::Marker,
        Color::rgb(252, 222, 101),
        5.0,
        vec![StrokePoint::new(15.0, 45.0), StrokePoint::new(65.0, 45.0)],
    ));

    let mods = PhotoModifications {
        angle: 270,
        flipped: true,
        crop: Some(Rect::new(5, 5, 70, 50)),
        crop_type: CropType::Rect,
        crop_mode: CropMode::Hint,
        corners_level: RoundedCornersLevel::Medium,
        paint: Some(scene),
        canvas_media_rect: None,
        enhance: 35,
    };

    let json = mods.to_json().expect("serialize draft");
    let restored = PhotoModifications::from_json(&json).expect("deserialize draft");
    assert_eq!(restored, mods);

    let rendered_orig = apply(&bg, &mods);
    let rendered_restored = apply(&bg, &restored);
    assert_eq!(rendered_orig.as_raw(), rendered_restored.as_raw());
}

#[test]
fn userpic_prepare_and_finalize_flow() {
    assert!(!validate_profile_photo_dimensions(0, 100));
    assert!(!validate_profile_photo_dimensions(1100, 100));
    assert!(validate_profile_photo_dimensions(1000, 100));

    let bad = RgbaImage::new(1100, 100);
    assert!(matches!(
        prepare_profile_photo_input(&bad, EditorData::default()),
        Err(EditorError::InvalidPhotoDimensions { .. })
    ));

    // Small 300x200 image upscales expanding to 960x640 with centered 640x640 square crop.
    let small = RgbaImage::from_pixel(300, 200, Rgba([60, 120, 180, 255]));
    let prepared = prepare_profile_photo_input(
        &small,
        EditorData {
            crop_type: CropType::Ellipse,
            crop_mode: CropMode::Mask,
            ..Default::default()
        },
    )
    .expect("valid userpic");
    assert_eq!(prepared.image.width(), 960);
    assert_eq!(prepared.image.height(), PROFILE_PHOTO_SIZE);
    assert_eq!(
        prepared.modifications.crop,
        Some(Rect::new(160, 0, 640, 640))
    );
    assert!(prepared.editor_data.keep_aspect_ratio);

    let final_img =
        finalize_profile_photo(&prepared.image, &prepared.modifications).expect("finalize");
    assert_eq!((final_img.width(), final_img.height()), (640, 640));
    assert_eq!(final_img.get_pixel(0, 0)[3], 0);
    assert_eq!(final_img.get_pixel(320, 320)[3], 255);
}

#[test]
fn jpeg_export_settings_scaling_opaque_and_validation() {
    assert_eq!(photo_side_limit(false), PHOTO_SIDE_LIMIT_STANDARD);
    assert_eq!(photo_side_limit(true), PHOTO_SIDE_LIMIT_LARGE);
    assert_eq!(PhotoExportSettings::large().max_side, 2560);
    assert_eq!(PhotoExportSettings::large().quality, PHOTO_JPEG_QUALITY);
    assert_eq!(scale_to_fit(4000, 2000, 2560), (2560, 1280));
    assert_eq!(scale_to_fit(800, 600, 2560), (800, 600));
    assert!(validate_thumb_dimensions(2000, 100));
    assert!(!validate_thumb_dimensions(2001, 100));

    // Semi-transparent image flattens onto white.
    let transparent = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 0]));
    let rgb = opaque_on_white(&transparent);
    assert_eq!(rgb.get_pixel(0, 0).0, [255, 255, 255]);

    // Export oversized photo with large (2560) and standard (1280) settings.
    let big = RgbaImage::from_pixel(3000, 1500, Rgba([100, 150, 200, 255]));
    let hd = export_jpeg(&big, &PhotoExportSettings::large()).expect("export HD jpeg");
    assert_eq!((hd.width, hd.height), (2560, 1280));
    assert_eq!(&hd.bytes[0..2], &[0xFF, 0xD8]); // JPEG SOI marker
    let decoded = image::load_from_memory(&hd.bytes).expect("decode JPEG");
    assert_eq!((decoded.width(), decoded.height()), (2560, 1280));

    let std_out = apply_and_export_jpeg(
        &big,
        &PhotoModifications {
            angle: 90,
            ..Default::default()
        },
        &PhotoExportSettings::standard(),
    )
    .expect("export standard rotated jpeg");
    assert_eq!((std_out.width, std_out.height), (640, 1280));

    // Reject aspect ratio > 20:1 when validate_photo_aspect is enabled.
    let skinny = RgbaImage::new(2100, 100);
    assert!(matches!(
        export_jpeg(&skinny, &PhotoExportSettings::large()),
        Err(EditorError::InvalidPhotoDimensions { .. })
    ));
}

#[test]
fn layout_enhance_and_stack_blur_helpers() {
    let layout_normal = compute_inner_layout(
        Size::new(1000, 800),
        Size::new(800, 600),
        &PhotoModifications::default(),
        tg_editor::Margins::uniform(5),
        false,
    )
    .expect("layout");
    let layout_room = compute_inner_layout(
        Size::new(1000, 800),
        Size::new(800, 600),
        &PhotoModifications::default(),
        tg_editor::Margins::uniform(5),
        true,
    )
    .expect("layout room");
    assert!(layout_room.image_geometry.width < layout_normal.image_geometry.width);
    assert!(layout_normal.controls_rect.height > 0);

    // Auto-enhance on a realistic 128x128 low-contrast gradient increases dynamic range.
    let mut low_contrast = RgbaImage::new(128, 128);
    for y in 0..128 {
        for x in 0..128 {
            let v = 60 + ((x + y) / 4) as u8;
            low_contrast.put_pixel(x, y, Rgba([v, v, v, 255]));
        }
    }
    let unchanged = enhance_image(&low_contrast, 0);
    assert_eq!(unchanged.as_raw(), low_contrast.as_raw());
    let enhanced = enhance_image(&low_contrast, 100);
    let orig_min = low_contrast.pixels().map(|p| p[0]).min().unwrap();
    let orig_max = low_contrast.pixels().map(|p| p[0]).max().unwrap();
    let min_v = enhanced.pixels().map(|p| p[0]).min().unwrap();
    let max_v = enhanced.pixels().map(|p| p[0]).max().unwrap();
    assert!(
        max_v - min_v > orig_max - orig_min,
        "expected wider dynamic range: {min_v}..={max_v} vs {orig_min}..={orig_max}"
    );

    // StackBlur preserves uniform images exactly.
    let uniform = RgbaImage::from_pixel(30, 30, Rgba([77, 133, 211, 255]));
    let blurred = stack_blur_rgba(&uniform, 5);
    assert_eq!(blurred.as_raw(), uniform.as_raw());
}
