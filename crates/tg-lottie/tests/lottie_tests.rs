//! Comprehensive integration and pixel-verification tests for `tg-lottie`.

use std::io::Write;
use std::sync::{Arc, Mutex};

use flate2::Compression;
use flate2::write::GzEncoder;

use tg_lottie::{
    AspectRatioMode, Cache, Color, ColorReplacements, Composition, FrameBuffer, FrameGenerator,
    FrameProvider, FrameProviderCached, FrameProviderCachedMulti, FrameProviderDirect,
    FrameProviderShared, FrameRenderResult, FrameRequest, Instance, MAX_FILE_SIZE, MultiCachePutFn,
    MultiPlayer, Quality, SinglePlayer, Size, SkinModifier, read_thumbnail, read_utf8,
    render_composition,
};

fn render_json(json: &str, frame: f64, width: u32, height: u32) -> FrameBuffer {
    let comp = Composition::from_json(json).expect("valid hand-written test Lottie JSON");
    render_composition(&comp, frame, Size::new(width, height))
}

fn assert_pixel_near(buf: &FrameBuffer, x: u32, y: u32, expected: [u8; 4], tol: u8) {
    let actual = buf.pixel(x, y).unwrap_or_else(|| {
        panic!(
            "pixel ({x}, {y}) out of bounds {}x{}",
            buf.width, buf.height
        )
    });
    for c in 0..4 {
        let diff = actual[c].abs_diff(expected[c]);
        assert!(
            diff <= tol,
            "pixel ({x}, {y}) channel {c}: actual={actual:?}, expected={expected:?} (tol={tol})"
        );
    }
}

fn gzip_bytes(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).expect("gzip write");
    enc.finish().expect("gzip finish")
}

// ---------------------------------------------------------------------------
// 1. Solid layer, hidden layer, in/out points, and markers
// ---------------------------------------------------------------------------

#[test]
fn solid_layer_timing_hidden_and_markers() {
    let json = r##"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 30,
        "w": 100,
        "h": 100,
        "markers": [
            { "cm": "intro", "tm": 0, "dr": 10 },
            { "cm": "main", "tm": 10, "dr": 20 }
        ],
        "layers": [
            {
                "ty": 1,
                "ind": 1,
                "hd": true,
                "sw": 100,
                "sh": 100,
                "sc": "#0000ff",
                "ip": 0,
                "op": 30,
                "ks": {}
            },
            {
                "ty": 1,
                "ind": 2,
                "sw": 100,
                "sh": 100,
                "sc": "#00ff00",
                "ip": 10,
                "op": 20,
                "ks": { "o": { "a": 0, "k": 100 } }
            },
            {
                "ty": 1,
                "ind": 3,
                "sw": 100,
                "sh": 100,
                "sc": "#ff0000",
                "ip": 0,
                "op": 30,
                "ks": {}
            }
        ]
    }"##;

    let comp = Composition::from_json(json).expect("parse");
    let m = comp.marker("main").expect("marker main");
    assert_eq!(m.time, 10.0);
    assert_eq!(m.duration, 20.0);

    // Frame 5: only bottom red solid active (blue is hidden, green starts at frame 10).
    let f5 = render_composition(&comp, 5.0, Size::new(100, 100));
    assert_pixel_near(&f5, 50, 50, [255, 0, 0, 255], 1);

    // Frame 15: green solid is above red solid.
    let f15 = render_composition(&comp, 15.0, Size::new(100, 100));
    assert_pixel_near(&f15, 50, 50, [0, 255, 0, 255], 1);

    // Frame 25: green solid ended at op=20, back to red.
    let f25 = render_composition(&comp, 25.0, Size::new(100, 100));
    assert_pixel_near(&f25, 50, 50, [255, 0, 0, 255], 1);
}

// ---------------------------------------------------------------------------
// 2. Shapes: Rect (rounded), Ellipse, Path (morphing), Polystar (Star & Polygon), EvenOdd
// ---------------------------------------------------------------------------

#[test]
fn rect_ellipse_path_and_polystar_rendering() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 30,
        "w": 200,
        "h": 200,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 30,
                "ks": {},
                "shapes": [
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "rc",
                                "p": { "a": 0, "k": [50, 50] },
                                "s": { "a": 0, "k": [60, 60] },
                                "r": { "a": 0, "k": 20 }
                            },
                            {
                                "ty": "fl",
                                "c": { "a": 0, "k": [1, 0, 0, 1] },
                                "o": { "a": 0, "k": 100 }
                            }
                        ]
                    },
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "el",
                                "p": { "a": 0, "k": [150, 50] },
                                "s": { "a": 0, "k": [60, 60] }
                            },
                            {
                                "ty": "fl",
                                "c": { "a": 0, "k": [0, 1, 0, 1] },
                                "o": { "a": 0, "k": 100 }
                            }
                        ]
                    },
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "sr",
                                "sy": 1,
                                "pt": { "a": 0, "k": 5 },
                                "p": { "a": 0, "k": [50, 150] },
                                "r": { "a": 0, "k": 0 },
                                "or": { "a": 0, "k": 35 },
                                "ir": { "a": 0, "k": 15 },
                                "os": { "a": 0, "k": 0 },
                                "is": { "a": 0, "k": 0 }
                            },
                            {
                                "ty": "fl",
                                "c": { "a": 0, "k": [0, 0, 1, 1] },
                                "o": { "a": 0, "k": 100 }
                            }
                        ]
                    },
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "sr",
                                "sy": 2,
                                "pt": { "a": 0, "k": 6 },
                                "p": { "a": 0, "k": [150, 150] },
                                "r": { "a": 0, "k": 0 },
                                "or": { "a": 0, "k": 30 },
                                "os": { "a": 0, "k": 0 }
                            },
                            {
                                "ty": "fl",
                                "c": { "a": 0, "k": [1, 1, 0, 1] },
                                "o": { "a": 0, "k": 100 }
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;

    let img = render_json(json, 0.0, 200, 200);
    // Rounded rect center is red; extreme corner (21, 21) is clipped by roundness r=20.
    assert_pixel_near(&img, 50, 50, [255, 0, 0, 255], 1);
    assert_pixel_near(&img, 21, 21, [0, 0, 0, 0], 1);
    // Ellipse center is green; outside corner (121, 21) is transparent.
    assert_pixel_near(&img, 150, 50, [0, 255, 0, 255], 1);
    assert_pixel_near(&img, 121, 21, [0, 0, 0, 0], 1);
    // Star center & top tip (50, 120) are blue; far outside is transparent.
    assert_pixel_near(&img, 50, 150, [0, 0, 255, 255], 1);
    assert_pixel_near(&img, 50, 120, [0, 0, 255, 255], 1);
    assert_pixel_near(&img, 10, 110, [0, 0, 0, 0], 1);
    // Hexagon center is yellow.
    assert_pixel_near(&img, 150, 150, [255, 255, 0, 255], 1);
}

#[test]
fn bezier_path_morph_and_evenodd_fill_rule() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 30,
        "w": 100,
        "h": 100,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 30,
                "ks": {},
                "shapes": [
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "sh",
                                "ks": {
                                    "a": 1,
                                    "k": [
                                        {
                                            "t": 0,
                                            "s": [{
                                                "c": true,
                                                "v": [[10,10],[90,10],[90,90],[10,90]],
                                                "i": [[0,0],[0,0],[0,0],[0,0]],
                                                "o": [[0,0],[0,0],[0,0],[0,0]]
                                            }],
                                            "e": [{
                                                "c": true,
                                                "v": [[30,30],[70,30],[70,70],[30,70]],
                                                "i": [[0,0],[0,0],[0,0],[0,0]],
                                                "o": [[0,0],[0,0],[0,0],[0,0]]
                                            }]
                                        },
                                        { "t": 10 }
                                    ]
                                }
                            },
                            {
                                "ty": "rc",
                                "p": { "a": 0, "k": [50, 50] },
                                "s": { "a": 0, "k": [20, 20] },
                                "r": { "a": 0, "k": 0 }
                            },
                            {
                                "ty": "fl",
                                "r": 2,
                                "c": { "a": 0, "k": [1, 0, 1, 1] },
                                "o": { "a": 0, "k": 100 }
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;

    // Frame 0: outer square [10..90] with inner [40..60] punched out by EvenOdd (r=2).
    let f0 = render_json(json, 0.0, 100, 100);
    assert_pixel_near(&f0, 20, 50, [255, 0, 255, 255], 1);
    assert_pixel_near(&f0, 50, 50, [0, 0, 0, 0], 1);

    // Frame 10: morphed outer square shrank to [30..70], so (20, 50) is now transparent, (35, 50) is magenta.
    let f10 = render_json(json, 10.0, 100, 100);
    assert_pixel_near(&f10, 20, 50, [0, 0, 0, 0], 1);
    assert_pixel_near(&f10, 35, 50, [255, 0, 255, 255], 1);
    assert_pixel_near(&f10, 50, 50, [0, 0, 0, 0], 1);
}

// ---------------------------------------------------------------------------
// 3. Strokes (caps, joins, dashes) and Trim Paths (Simultaneous & Individual)
// ---------------------------------------------------------------------------

#[test]
fn stroke_dashes_and_trim_paths() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 30,
        "w": 120,
        "h": 80,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 30,
                "ks": {},
                "shapes": [
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "sh",
                                "ks": {
                                    "a": 0,
                                    "k": {
                                        "c": false,
                                        "v": [[10, 20], [110, 20]],
                                        "i": [[0,0],[0,0]],
                                        "o": [[0,0],[0,0]]
                                    }
                                }
                            },
                            {
                                "ty": "st",
                                "c": { "a": 0, "k": [1, 0, 0, 1] },
                                "o": { "a": 0, "k": 100 },
                                "w": { "a": 0, "k": 8 },
                                "lc": 1,
                                "lj": 1,
                                "ml": 4,
                                "d": [
                                    { "n": "d", "v": { "a": 0, "k": 20 } },
                                    { "n": "g", "v": { "a": 0, "k": 20 } },
                                    { "n": "o", "v": { "a": 0, "k": 0 } }
                                ]
                            }
                        ]
                    },
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "sh",
                                "ks": {
                                    "a": 0,
                                    "k": {
                                        "c": false,
                                        "v": [[10, 60], [110, 60]],
                                        "i": [[0,0],[0,0]],
                                        "o": [[0,0],[0,0]]
                                    }
                                }
                            },
                            {
                                "ty": "tm",
                                "s": { "a": 0, "k": 0 },
                                "e": { "a": 0, "k": 50 },
                                "o": { "a": 0, "k": 0 },
                                "m": 1
                            },
                            {
                                "ty": "st",
                                "c": { "a": 0, "k": [0, 0, 1, 1] },
                                "o": { "a": 0, "k": 100 },
                                "w": { "a": 0, "k": 8 },
                                "lc": 1,
                                "lj": 2
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;

    let img = render_json(json, 0.0, 120, 80);
    // Dashed red line at y=20: [10..30] dash, [30..50] gap, [50..70] dash.
    assert_pixel_near(&img, 20, 20, [255, 0, 0, 255], 1);
    assert_pixel_near(&img, 40, 20, [0, 0, 0, 0], 1);
    assert_pixel_near(&img, 60, 20, [255, 0, 0, 255], 1);

    // Trimmed blue line at y=60: [10..60] drawn, [60..110] trimmed away.
    assert_pixel_near(&img, 35, 60, [0, 0, 255, 255], 1);
    assert_pixel_near(&img, 85, 60, [0, 0, 0, 0], 1);
}

// ---------------------------------------------------------------------------
// 4. Linear & Radial Gradient Fill and Gradient Stroke (with alpha stops)
// ---------------------------------------------------------------------------

#[test]
fn linear_and_radial_gradients() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 30,
        "w": 100,
        "h": 100,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 30,
                "ks": {},
                "shapes": [
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "rc",
                                "p": { "a": 0, "k": [50, 25] },
                                "s": { "a": 0, "k": [100, 50] },
                                "r": { "a": 0, "k": 0 }
                            },
                            {
                                "ty": "gf",
                                "t": 1,
                                "s": { "a": 0, "k": [0, 25] },
                                "e": { "a": 0, "k": [100, 25] },
                                "o": { "a": 0, "k": 100 },
                                "g": {
                                    "p": 2,
                                    "k": { "a": 0, "k": [0, 1, 0, 0, 1, 0, 0, 1] }
                                }
                            }
                        ]
                    },
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "rc",
                                "p": { "a": 0, "k": [50, 75] },
                                "s": { "a": 0, "k": [100, 50] },
                                "r": { "a": 0, "k": 0 }
                            },
                            {
                                "ty": "gf",
                                "t": 2,
                                "s": { "a": 0, "k": [50.5, 75.5] },
                                "e": { "a": 0, "k": [90.5, 75.5] },
                                "o": { "a": 0, "k": 100 },
                                "g": {
                                    "p": 2,
                                    "k": { "a": 0, "k": [0, 0, 1, 0, 1, 0, 0, 0, 0, 1, 1, 0] }
                                }
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;

    let img = render_json(json, 0.0, 100, 100);
    // Top half: linear gradient red (x=0) -> purple (x=50) -> blue (x=99).
    assert_pixel_near(&img, 1, 25, [252, 0, 3, 255], 6);
    assert_pixel_near(&img, 50, 25, [127, 0, 128, 255], 6);
    assert_pixel_near(&img, 99, 25, [3, 0, 252, 255], 6);

    // Bottom half: radial gradient opaque green at center (50, 75) fading to transparent at r=40.
    assert_pixel_near(&img, 50, 75, [0, 255, 0, 255], 6);
    assert_pixel_near(&img, 95, 75, [0, 0, 0, 0], 4);
}

// ---------------------------------------------------------------------------
// 5. Repeater (copies, offset, progressive transform, opacity ramp)
// ---------------------------------------------------------------------------

#[test]
fn repeater_copies_and_opacity_ramp() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 30,
        "w": 120,
        "h": 40,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 30,
                "ks": {},
                "shapes": [
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "rc",
                                "p": { "a": 0, "k": [20, 20] },
                                "s": { "a": 0, "k": [20, 20] },
                                "r": { "a": 0, "k": 0 }
                            },
                            {
                                "ty": "fl",
                                "c": { "a": 0, "k": [1, 0, 0, 1] },
                                "o": { "a": 0, "k": 100 }
                            },
                            {
                                "ty": "rp",
                                "c": { "a": 0, "k": 3 },
                                "o": { "a": 0, "k": 0 },
                                "m": 1,
                                "tr": {
                                    "p": { "a": 0, "k": [35, 0] },
                                    "s": { "a": 0, "k": [100, 100] },
                                    "r": { "a": 0, "k": 0 },
                                    "so": { "a": 0, "k": 100 },
                                    "eo": { "a": 0, "k": 20 }
                                }
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;

    let img = render_json(json, 0.0, 120, 40);
    // Copy 0 at x=20 (opacity 100%)
    assert_pixel_near(&img, 20, 20, [255, 0, 0, 255], 2);
    // Copy 1 at x=55 (opacity 60% -> premultiplied ~153)
    assert_pixel_near(&img, 55, 20, [153, 0, 0, 153], 4);
    // Copy 2 at x=90 (opacity 20% -> premultiplied ~51)
    assert_pixel_near(&img, 90, 20, [51, 0, 0, 51], 4);
    // Gap between copies is transparent
    assert_pixel_near(&img, 37, 20, [0, 0, 0, 0], 1);
}

// ---------------------------------------------------------------------------
// 6. Transforms: Parenting (Null), Separated Dimensions, Skew, Easing, Hold, Spatial Bezier
// ---------------------------------------------------------------------------

#[test]
fn parenting_separated_dimensions_hold_and_easing() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 30,
        "w": 120,
        "h": 120,
        "layers": [
            {
                "ty": 3,
                "ind": 1,
                "ip": 0,
                "op": 30,
                "ks": {
                    "o": { "a": 0, "k": 0 },
                    "p": {
                        "s": true,
                        "x": {
                            "a": 1,
                            "k": [
                                { "t": 0, "s": [20], "e": [80], "o": { "x": [0.0], "y": [0.0] }, "i": { "x": [1.0], "y": [1.0] } },
                                { "t": 10 }
                            ]
                        },
                        "y": {
                            "a": 1,
                            "k": [
                                { "t": 0, "s": [30], "h": 1 },
                                { "t": 10, "s": [90] }
                            ]
                        }
                    }
                }
            },
            {
                "ty": 4,
                "ind": 2,
                "parent": 1,
                "ip": 0,
                "op": 30,
                "ks": {
                    "p": { "a": 0, "k": [10, 0] },
                    "o": { "a": 0, "k": 100 }
                },
                "shapes": [
                    {
                        "ty": "rc",
                        "p": { "a": 0, "k": [0, 0] },
                        "s": { "a": 0, "k": [16, 16] },
                        "r": { "a": 0, "k": 0 }
                    },
                    {
                        "ty": "fl",
                        "c": { "a": 0, "k": [0, 1, 1, 1] },
                        "o": { "a": 0, "k": 100 }
                    }
                ]
            }
        ]
    }"#;

    // Note: parent Null has opacity 0, but Lottie parenting inherits transform only, NOT opacity!
    // Frame 5: parent x=50 (linear), y=30 (hold until frame 10), child local +10 -> center (60, 30).
    let f5 = render_json(json, 5.0, 120, 120);
    assert_pixel_near(&f5, 60, 30, [0, 255, 255, 255], 1);
    assert_pixel_near(&f5, 60, 90, [0, 0, 0, 0], 1);

    // Frame 10: parent x=80, y=90, child center (90, 90).
    let f10 = render_json(json, 10.0, 120, 120);
    assert_pixel_near(&f10, 90, 90, [0, 255, 255, 255], 1);
    assert_pixel_near(&f10, 60, 30, [0, 0, 0, 0], 1);
}

#[test]
fn spatial_bezier_position_and_cubic_easing() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 20,
        "w": 120,
        "h": 120,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 20,
                "ks": {
                    "p": {
                        "a": 1,
                        "k": [
                            {
                                "t": 0,
                                "s": [20, 90],
                                "e": [100, 90],
                                "to": [0, -80],
                                "ti": [0, -80],
                                "o": { "x": 0.0, "y": 0.0 },
                                "i": { "x": 1.0, "y": 1.0 }
                            },
                            { "t": 10 }
                        ]
                    }
                },
                "shapes": [
                    {
                        "ty": "el",
                        "p": { "a": 0, "k": [0, 0] },
                        "s": { "a": 0, "k": [20, 20] }
                    },
                    {
                        "ty": "fl",
                        "c": { "a": 0, "k": [1, 0.5, 0, 1] },
                        "o": { "a": 0, "k": 100 }
                    }
                ]
            }
        ]
    }"#;

    // At midpoint t=5, the spatial cubic bezier with to=(0,-80), ti=(0,-80) arches up to y = 90 - 0.75*80 = 30!
    let f5 = render_json(json, 5.0, 120, 120);
    assert_pixel_near(&f5, 60, 30, [255, 128, 0, 255], 2);
    // Straight chord midpoint (60, 90) must be empty.
    assert_pixel_near(&f5, 60, 90, [0, 0, 0, 0], 1);
}

#[test]
fn skew_transform_tilts_rectangle() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 10,
        "w": 100,
        "h": 100,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 10,
                "ks": {
                    "p": { "a": 0, "k": [50, 50] },
                    "sk": { "a": 0, "k": 45 },
                    "sa": { "a": 0, "k": 0 }
                },
                "shapes": [
                    {
                        "ty": "rc",
                        "p": { "a": 0, "k": [0, 0] },
                        "s": { "a": 0, "k": [20, 60] },
                        "r": { "a": 0, "k": 0 }
                    },
                    {
                        "ty": "fl",
                        "c": { "a": 0, "k": [1, 1, 1, 1] },
                        "o": { "a": 0, "k": 100 }
                    }
                ]
            }
        ]
    }"#;

    // With sk=+45 deg, a point at local (0, -25) shifts right by +25 to world (75, 25),
    // while world (50, 25) is now outside the tilted bar!
    let img = render_json(json, 0.0, 100, 100);
    assert_pixel_near(&img, 50, 50, [255, 255, 255, 255], 1);
    assert_pixel_near(&img, 75, 25, [255, 255, 255, 255], 1);
    assert_pixel_near(&img, 50, 25, [0, 0, 0, 0], 1);
}

// ---------------------------------------------------------------------------
// 7. Precomps, Time Remapping, Stretch & Start Time
// ---------------------------------------------------------------------------

#[test]
fn precomp_clipping_time_remapping_and_stretch() {
    let json = r##"{
        "v": "5.5.2",
        "fr": 10,
        "ip": 0,
        "op": 20,
        "w": 100,
        "h": 100,
        "assets": [
            {
                "id": "comp_0",
                "w": 50,
                "h": 50,
                "layers": [
                    {
                        "ty": 1,
                        "sw": 100,
                        "sh": 100,
                        "sc": "#0000ff",
                        "ip": 5,
                        "op": 20,
                        "ks": {}
                    },
                    {
                        "ty": 1,
                        "sw": 100,
                        "sh": 100,
                        "sc": "#ff0000",
                        "ip": 0,
                        "op": 5,
                        "ks": {}
                    }
                ]
            }
        ],
        "layers": [
            {
                "ty": 0,
                "refId": "comp_0",
                "w": 50,
                "h": 50,
                "ip": 0,
                "op": 20,
                "st": 0,
                "sr": 1,
                "tm": {
                    "a": 1,
                    "k": [
                        { "t": 0, "s": [0.8], "e": [0.1] },
                        { "t": 10 }
                    ]
                },
                "ks": { "p": { "a": 0, "k": [10, 10] } }
            }
        ]
    }"##;

    // Frame 0: tm=0.8s * 10fps = sub-frame 8 -> blue solid, clipped to [10..60, 10..60].
    let f0 = render_json(json, 0.0, 100, 100);
    assert_pixel_near(&f0, 30, 30, [0, 0, 255, 255], 1);
    // Outside precomp clip (70, 30) must be transparent even though inner solid is 100x100.
    assert_pixel_near(&f0, 70, 30, [0, 0, 0, 0], 1);

    // Frame 10: tm=0.1s * 10fps = sub-frame 1 -> red solid!
    let f10 = render_json(json, 10.0, 100, 100);
    assert_pixel_near(&f10, 30, 30, [255, 0, 0, 255], 1);
}

// ---------------------------------------------------------------------------
// 8. Masks (Add, Subtract, Intersect, Inverted, Opacity, Expansion)
// ---------------------------------------------------------------------------

#[test]
fn masks_add_subtract_intersect_inverted_and_expansion() {
    let json = r##"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 10,
        "w": 100,
        "h": 100,
        "layers": [
            {
                "ty": 1,
                "sw": 100,
                "sh": 100,
                "sc": "#ff0000",
                "ip": 0,
                "op": 10,
                "ks": {},
                "masksProperties": [
                    {
                        "mode": "a",
                        "inv": false,
                        "o": { "a": 0, "k": 100 },
                        "x": { "a": 0, "k": 10 },
                        "pt": {
                            "a": 0,
                            "k": {
                                "c": true,
                                "v": [[20,20],[80,20],[80,80],[20,80]],
                                "i": [[0,0],[0,0],[0,0],[0,0]],
                                "o": [[0,0],[0,0],[0,0],[0,0]]
                            }
                        }
                    },
                    {
                        "mode": "s",
                        "inv": false,
                        "o": { "a": 0, "k": 100 },
                        "x": { "a": 0, "k": 0 },
                        "pt": {
                            "a": 0,
                            "k": {
                                "c": true,
                                "v": [[40,40],[60,40],[60,60],[40,60]],
                                "i": [[0,0],[0,0],[0,0],[0,0]],
                                "o": [[0,0],[0,0],[0,0],[0,0]]
                            }
                        }
                    }
                ]
            }
        ]
    }"##;

    let img = render_json(json, 0.0, 100, 100);
    // Mask 1 is [20..80] expanded by +10 -> [10..90]; pixel (15, 50) must be red!
    assert_pixel_near(&img, 15, 50, [255, 0, 0, 255], 2);
    // Outside expanded mask (5, 50) is transparent.
    assert_pixel_near(&img, 5, 50, [0, 0, 0, 0], 1);
    // Center (50, 50) was subtracted by Mask 2 -> transparent!
    assert_pixel_near(&img, 50, 50, [0, 0, 0, 0], 1);
}

// ---------------------------------------------------------------------------
// 9. Track Mattes (Alpha, AlphaInverted, Luma, LumaInverted)
// ---------------------------------------------------------------------------

#[test]
fn track_mattes_alpha_and_luma() {
    let make_matte_json = |tt: i32, matte_hex: &str| {
        format!(
            r##"{{
                "v": "5.5.2",
                "fr": 30,
                "ip": 0,
                "op": 10,
                "w": 100,
                "h": 100,
                "layers": [
                    {{
                        "ty": 1,
                        "ind": 1,
                        "td": 1,
                        "sw": 50,
                        "sh": 100,
                        "sc": "{matte_hex}",
                        "ip": 0,
                        "op": 10,
                        "ks": {{}}
                    }},
                    {{
                        "ty": 1,
                        "ind": 2,
                        "tt": {tt},
                        "sw": 100,
                        "sh": 100,
                        "sc": "#00ff00",
                        "ip": 0,
                        "op": 10,
                        "ks": {{}}
                    }}
                ]
            }}"##
        )
    };

    // tt=1 (Alpha): left half visible, right half transparent.
    let alpha = render_json(&make_matte_json(1, "#000000"), 0.0, 100, 100);
    assert_pixel_near(&alpha, 25, 50, [0, 255, 0, 255], 1);
    assert_pixel_near(&alpha, 75, 50, [0, 0, 0, 0], 1);

    // tt=2 (AlphaInverted): left half transparent, right half visible.
    let alpha_inv = render_json(&make_matte_json(2, "#000000"), 0.0, 100, 100);
    assert_pixel_near(&alpha_inv, 25, 50, [0, 0, 0, 0], 1);
    assert_pixel_near(&alpha_inv, 75, 50, [0, 255, 0, 255], 1);

    // tt=3 (Luma) with white left half: left visible, right transparent.
    let luma_white = render_json(&make_matte_json(3, "#ffffff"), 0.0, 100, 100);
    assert_pixel_near(&luma_white, 25, 50, [0, 255, 0, 255], 1);
    assert_pixel_near(&luma_white, 75, 50, [0, 0, 0, 0], 1);

    // tt=3 (Luma) with black left half: both transparent!
    let luma_black = render_json(&make_matte_json(3, "#000000"), 0.0, 100, 100);
    assert_pixel_near(&luma_black, 25, 50, [0, 0, 0, 0], 1);
    assert_pixel_near(&luma_black, 75, 50, [0, 0, 0, 0], 1);

    // tt=4 (LumaInverted) with white left half: left transparent, right visible.
    let luma_inv = render_json(&make_matte_json(4, "#ffffff"), 0.0, 100, 100);
    assert_pixel_near(&luma_inv, 25, 50, [0, 0, 0, 0], 1);
    assert_pixel_near(&luma_inv, 75, 50, [0, 255, 0, 255], 1);
}

// ---------------------------------------------------------------------------
// 10. Color Replacements & Emoji Skin Tone Modifiers
// ---------------------------------------------------------------------------

#[test]
fn color_replacements_and_fitzpatrick_skin_modifiers() {
    let json = r##"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 10,
        "w": 64,
        "h": 64,
        "layers": [
            {
                "ty": 1,
                "sw": 32,
                "sh": 64,
                "sc": "#ffffff",
                "ip": 0,
                "op": 10,
                "ks": {}
            },
            {
                "ty": 1,
                "sw": 32,
                "sh": 64,
                "sc": "#ffd140",
                "ip": 0,
                "op": 10,
                "ks": { "p": { "a": 0, "k": [32, 0] } }
            }
        ]
    }"##;

    let repl = ColorReplacements {
        replacements: vec![(0xFFFFFF, 0x123456)],
        modifier: SkinModifier::Color5,
        tag: 5,
    };
    let inst = Instance::from_json(json, Some(&repl)).expect("parse");
    let mut buf = FrameBuffer::new(Size::new(64, 64));
    inst.render_to_prepared(&mut buf, 0);

    // Left half replaced 0xFFFFFF -> 0x123456.
    assert_pixel_near(&buf, 16, 32, [0x12, 0x34, 0x56, 255], 1);
    // Right half replaced yellow 0xFFD140 -> Fitzpatrick Type 6 (0x6C4C39).
    assert_pixel_near(&buf, 48, 32, [0x6C, 0x4C, 0x39, 255], 1);
}

// ---------------------------------------------------------------------------
// 11. Gzip .tgs Loading, BOM Stripping, Size Limits, and Thumbnail
// ---------------------------------------------------------------------------

#[test]
fn tgs_gzip_loading_bom_and_size_limits() {
    let raw_json = r##"{
        "v": "5.5.2",
        "fr": 60,
        "ip": 0,
        "op": 60,
        "w": 128,
        "h": 128,
        "layers": [
            { "ty": 1, "sw": 128, "sh": 128, "sc": "#2288ee", "ip": 0, "op": 60, "ks": {} }
        ]
    }"##;

    // Prepend UTF-8 BOM and gzip compress (.tgs).
    let mut with_bom = vec![0xEF, 0xBB, 0xBF];
    with_bom.extend_from_slice(raw_json.as_bytes());
    assert!(read_utf8(&with_bom).unwrap().starts_with('{'));

    let tgs = gzip_bytes(&with_bom);
    let thumb = read_thumbnail(&tgs).expect("read_thumbnail from .tgs");
    assert_eq!(thumb.size(), Size::new(512, 512));
    assert_pixel_near(&thumb, 256, 256, [0x22, 0x88, 0xEE, 255], 1);

    // Oversized input (> MAX_FILE_SIZE) rejected.
    let huge = vec![b' '; MAX_FILE_SIZE + 1];
    assert!(Instance::from_bytes(&huge, None).is_err());

    // Gzip bomb (> MAX_FILE_SIZE uncompressed) rejected cleanly without OOM.
    let bomb_raw = vec![b' '; MAX_FILE_SIZE + 1024];
    let bomb_gz = gzip_bytes(&bomb_raw);
    assert!(bomb_gz.len() < MAX_FILE_SIZE);
    assert!(Instance::from_bytes(&bomb_gz, None).is_err());

    // Quality::Default halves 60fps -> 30fps (30 frames), Quality::High keeps 60fps (60 frames).
    let p_default = SinglePlayer::from_bytes(
        &tgs,
        FrameRequest::new(Size::new(64, 64)),
        Quality::Default,
        None,
    )
    .expect("default player");
    assert_eq!(p_default.information().frame_rate, 30);
    assert_eq!(p_default.frames_count(), 30);

    let p_high = SinglePlayer::from_bytes(
        &tgs,
        FrameRequest::new(Size::new(64, 64)),
        Quality::High,
        None,
    )
    .expect("high player");
    assert_eq!(p_high.information().frame_rate, 60);
    assert_eq!(p_high.frames_count(), 60);
}

// ---------------------------------------------------------------------------
// 12. Compressed YUV420A4 + LZ4 Frame Cache (Single, Multi, Shared)
// ---------------------------------------------------------------------------

#[test]
fn frame_cache_roundtrip_and_multi_cache() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 6,
        "w": 64,
        "h": 64,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 6,
                "ks": {
                    "p": {
                        "a": 1,
                        "k": [
                            { "t": 0, "s": [16, 32], "e": [48, 32] },
                            { "t": 5 }
                        ]
                    }
                },
                "shapes": [
                    { "ty": "rc", "p": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [20, 20] }, "r": { "a": 0, "k": 0 } },
                    { "ty": "fl", "c": { "a": 0, "k": [0, 1, 0, 1] }, "o": { "a": 0, "k": 100 } }
                ]
            }
        ]
    }"#;

    let req = FrameRequest::new(Size::new(64, 64));
    let saved_blob: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let saved_clone = Arc::clone(&saved_blob);

    {
        let mut provider = FrameProviderCached::new(
            json.as_bytes().to_vec(),
            Some(Box::new(move |bytes| {
                *saved_clone.lock().unwrap() = bytes;
            })),
            Vec::new(),
            req,
            Quality::High,
            None,
        );
        assert!(provider.valid());
        let mut token = None;
        let f0 = provider.construct(&mut token, req);
        assert_pixel_near(&f0, 16, 32, [0, 255, 0, 255], 4);

        let mut buf = FrameBuffer::new(Size::new(64, 64));
        for idx in 1..6 {
            assert!(provider.render(&mut token, &mut buf, req, idx));
        }
        // Once all 6 frames are cached, direct provider unloads automatically.
        assert!(!provider.direct_loaded());
    }

    let blob = saved_blob.lock().unwrap().clone();
    assert!(
        !blob.is_empty(),
        "cache put callback must have received bytes"
    );

    // Reopen from cache blob ONLY (empty content bytes proves zero fallback to direct renderer!).
    let mut cached_only =
        FrameProviderCached::new(Vec::new(), None, blob, req, Quality::High, None);
    assert!(cached_only.valid());
    assert!(!cached_only.direct_loaded());
    let mut token = None;
    let f0 = cached_only.construct(&mut token, req);
    assert_pixel_near(&f0, 16, 32, [0, 255, 0, 255], 12);
    let mut f5 = FrameBuffer::new(Size::new(64, 64));
    assert!(cached_only.render(&mut token, &mut f5, req, 5));
    assert_pixel_near(&f5, 48, 32, [0, 255, 0, 255], 12);
    assert_pixel_near(&f5, 8, 8, [0, 0, 0, 0], 1);

    // Multi-cache across 2 partitions + FrameProviderShared across 2 players.
    let parts: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(vec![Vec::new(); 2]));
    let parts_put = Arc::clone(&parts);
    let put_cb: MultiCachePutFn = Arc::new(Mutex::new(Box::new(move |idx, data| {
        parts_put.lock().unwrap()[idx] = data;
    })));

    let multi = FrameProviderCachedMulti::new(
        json.as_bytes().to_vec(),
        Some(put_cb),
        vec![Vec::new(); 2],
        req,
        Quality::High,
        None,
    );
    assert!(multi.valid());
    assert_eq!(multi.caches_count(), 2);
    assert_eq!(multi.frames_per_cache(), 3);

    let shared = FrameProviderShared::new(Box::new(multi));
    let mut p1 = SinglePlayer::from_provider(Box::new(shared.clone_handle()), req).expect("p1");
    let mut p2 = SinglePlayer::from_provider(Box::new(shared), req).expect("p2");
    p1.start(0);
    p2.start(0);
    for step in 1..=6 {
        let t = step * 35;
        let _ = p1.advance(t);
        let _ = p2.advance(t);
    }
    let stored = parts.lock().unwrap();
    assert!(!stored[0].is_empty());
    assert!(!stored[1].is_empty());
}

// ---------------------------------------------------------------------------
// 13. Player Scheduling, Frame Skipping, Request Transform, MultiPlayer, FrameGenerator
// ---------------------------------------------------------------------------

#[test]
fn single_player_multi_player_and_frame_generator() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 10,
        "ip": 0,
        "op": 5,
        "w": 64,
        "h": 64,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 5,
                "ks": {},
                "shapes": [
                    { "ty": "rc", "p": { "a": 0, "k": [16, 32] }, "s": { "a": 0, "k": [16, 16] }, "r": { "a": 0, "k": 0 } },
                    { "ty": "fl", "c": { "a": 0, "k": [1, 1, 1, 1] }, "o": { "a": 0, "k": 100 } }
                ]
            }
        ]
    }"#;

    let req = FrameRequest::new(Size::new(64, 64));
    let mut player =
        SinglePlayer::from_bytes(json.as_bytes(), req, Quality::High, None).expect("player");
    assert_eq!(player.frame_index_for_time(0), 0);
    assert_eq!(player.frame_index_for_time(100), 1);
    assert_eq!(player.frame_index_for_time(250), 2);
    assert_eq!(player.frame_index_for_time(500), 0); // wrapped

    player.start(1000);
    assert_eq!(player.frame_index(), 0);
    // Before 1100ms, frame 1 is not due yet.
    assert_eq!(player.advance(1050), None);
    // At 1100ms, advances to frame 1.
    assert_eq!(player.advance(1100), Some(1));
    // Mirror + color tint request: square originally at x=16 moves to x=48 and becomes red!
    let mirrored_tinted = FrameRequest {
        box_size: Size::new(64, 64),
        colored: Color::rgb(255, 0, 0),
        mirror_horizontal: true,
    };
    let img = player.frame_with_request(mirrored_tinted);
    assert_pixel_near(&img, 48, 32, [255, 0, 0, 255], 2);
    assert_pixel_near(&img, 16, 32, [0, 0, 0, 0], 1);

    // MultiPlayer: two animations, pause/unpause catch-up.
    let mut multi = MultiPlayer::new(Quality::High);
    let a1 = multi.append(json.as_bytes(), req, None, 0).expect("a1");
    let a2 = multi.append(json.as_bytes(), req, None, 0).expect("a2");
    assert_eq!(multi.active_count(), 2);
    assert!(multi.check_step(100));
    assert!(multi.mark_frame_shown());
    assert_eq!(multi.frame_index(a1), Some(1));
    assert_eq!(multi.frame_index(a2), Some(1));

    multi.pause(a2);
    assert_eq!(multi.paused_count(), 1);
    assert!(multi.check_step(200));
    assert!(multi.mark_frame_shown());
    assert_eq!(multi.frame_index(a1), Some(2));
    assert_eq!(multi.frame_index(a2), Some(1)); // stayed paused

    multi.unpause(a2, 200);
    assert!(multi.check_step(300));
    assert!(multi.mark_frame_shown());
    assert_eq!(multi.active_count(), 2);

    // FrameGenerator sequential iteration.
    let mut generator = FrameGenerator::from_bytes(json.as_bytes()).expect("gen");
    assert_eq!(generator.count(), 5);
    assert_eq!(generator.frame_duration_ms(), 100);
    for i in 0..5 {
        let f = generator
            .render_next(Size::new(32, 32), AspectRatioMode::KeepAspectRatio)
            .expect("frame");
        assert_eq!(f.last, i == 4);
        assert_eq!(f.image.size(), Size::new(32, 32));
    }
    assert!(
        generator
            .render_next(Size::new(32, 32), AspectRatioMode::KeepAspectRatio)
            .is_none()
    );
    generator.jump_to_start();
    assert!(
        generator
            .render_next(Size::new(32, 32), AspectRatioMode::KeepAspectRatio)
            .is_some()
    );
}

#[test]
fn gradient_stroke_trim_individual_and_per_dim_easing() {
    let json = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 20,
        "w": 120,
        "h": 80,
        "layers": [
            {
                "ty": 4,
                "ip": 0,
                "op": 20,
                "ks": {
                    "p": {
                        "a": 1,
                        "k": [
                            {
                                "t": 0,
                                "s": [0, 0],
                                "e": [20, 20],
                                "o": { "x": [0.8, 0.2], "y": [0.0, 1.0] },
                                "i": { "x": [1.0, 0.5], "y": [0.2, 1.0] }
                            },
                            { "t": 10 }
                        ]
                    }
                },
                "shapes": [
                    {
                        "ty": "gr",
                        "it": [
                            {
                                "ty": "sh",
                                "ks": {
                                    "a": 0,
                                    "k": { "c": false, "v": [[10, 20], [110, 20]], "i": [[0,0],[0,0]], "o": [[0,0],[0,0]] }
                                }
                            },
                            {
                                "ty": "sh",
                                "ks": {
                                    "a": 0,
                                    "k": { "c": false, "v": [[10, 50], [110, 50]], "i": [[0,0],[0,0]], "o": [[0,0],[0,0]] }
                                }
                            },
                            {
                                "ty": "tm",
                                "s": { "a": 0, "k": 0 },
                                "e": { "a": 0, "k": 50 },
                                "o": { "a": 0, "k": 0 },
                                "m": 2
                            },
                            {
                                "ty": "gs",
                                "t": 1,
                                "s": { "a": 0, "k": [10, 20] },
                                "e": { "a": 0, "k": [110, 20] },
                                "o": { "a": 0, "k": 100 },
                                "w": { "a": 0, "k": 8 },
                                "lc": 1,
                                "lj": 1,
                                "g": {
                                    "p": 2,
                                    "k": { "a": 0, "k": [0, 1, 0, 0, 1, 0, 0, 1] }
                                }
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;

    // Frame 0: Individual trim [0..50%] across 2 equal lines draws ALL of line 1 (y=20)
    // with red->blue gradient stroke, and NONE of line 2 (y=50)!
    let f0 = render_json(json, 0.0, 120, 80);
    assert_pixel_near(&f0, 15, 20, [242, 0, 13, 255], 12);
    assert_pixel_near(&f0, 105, 20, [13, 0, 242, 255], 12);
    assert_pixel_near(&f0, 50, 50, [0, 0, 0, 0], 1);

    // Per-dimension cubic easing at frame 5: X eases slow-start (< 10), Y eases fast-start (> 10).
    let comp = Composition::from_json(json).expect("parse");
    let pos5 = comp.layers[0].transform.position.eval(5.0);
    assert!(pos5[0] < 6.0, "expected slow-start X, got {}", pos5[0]);
    assert!(pos5[1] > 16.0, "expected fast-start Y, got {}", pos5[1]);
}

#[test]
fn mask_intersect_inverted_opacity_and_frame_request_rounding() {
    let json = r##"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 10,
        "w": 100,
        "h": 100,
        "layers": [
            {
                "ty": 1,
                "sw": 100,
                "sh": 100,
                "sc": "#0000ff",
                "ip": 0,
                "op": 10,
                "ks": {},
                "masksProperties": [
                    {
                        "mode": "a",
                        "inv": true,
                        "o": { "a": 0, "k": 50 },
                        "x": { "a": 0, "k": 0 },
                        "pt": {
                            "a": 0,
                            "k": {
                                "c": true,
                                "v": [[40,40],[60,40],[60,60],[40,60]],
                                "i": [[0,0],[0,0],[0,0],[0,0]],
                                "o": [[0,0],[0,0],[0,0],[0,0]]
                            }
                        }
                    },
                    {
                        "mode": "i",
                        "inv": false,
                        "o": { "a": 0, "k": 100 },
                        "x": { "a": 0, "k": 0 },
                        "pt": {
                            "a": 0,
                            "k": {
                                "c": true,
                                "v": [[0,0],[70,0],[70,100],[0,100]],
                                "i": [[0,0],[0,0],[0,0],[0,0]],
                                "o": [[0,0],[0,0],[0,0],[0,0]]
                            }
                        }
                    }
                ]
            }
        ]
    }"##;

    let img = render_json(json, 0.0, 100, 100);
    // Inside [0..70] and outside [40..60]: inverted 50% opacity blue -> [0, 0, 128, 128].
    assert_pixel_near(&img, 20, 50, [0, 0, 128, 128], 3);
    // Inside [40..60]: inverted mask is 0 -> transparent.
    assert_pixel_near(&img, 50, 50, [0, 0, 0, 0], 1);
    // Outside [0..70] (intersect mask): transparent.
    assert_pixel_near(&img, 85, 50, [0, 0, 0, 0], 1);

    // Verify FrameRequest::size rounding (matches lottie_common.cpp).
    let req = FrameRequest::new(Size::new(100, 100));
    assert_eq!(req.size(Size::new(512, 512), 8), Size::new(96, 96));
    assert_eq!(req.size(Size::new(512, 512), 2), Size::new(100, 100));
    assert_eq!(req.size(Size::new(512, 256), 8), Size::new(96, 48));
}

// ---------------------------------------------------------------------------
// 14. Parse & Render Robustness Against Malformed / Hostile Inputs (No Panics)
// ---------------------------------------------------------------------------

#[test]
fn malformed_and_hostile_inputs_never_panic() {
    let invalid_inputs: &[&str] = &[
        "",
        "{",
        "null",
        "[]",
        "42",
        "\"hello\"",
        "{}",
        r#"{"fr":0,"ip":0,"op":10,"w":100,"h":100}"#,
        r#"{"fr":-30,"ip":0,"op":10,"w":100,"h":100}"#,
        r#"{"fr":30,"ip":10,"op":5,"w":100,"h":100}"#,
        r#"{"fr":30,"ip":0,"op":10,"w":0,"h":100}"#,
        r#"{"fr":30,"ip":0,"op":10,"w":100,"h":-10}"#,
    ];
    for s in invalid_inputs {
        assert!(Composition::from_json(s).is_err(), "expected error for {s}");
    }

    // Valid header but pathological layer contents: cyclic parenting, cyclic precomp,
    // zero stretch, broken keyframes, mismatched path vertices, unknown types.
    let hostile = r#"{
        "v": "5.5.2",
        "fr": 30,
        "ip": 0,
        "op": 10,
        "w": 64,
        "h": 64,
        "assets": [
            {
                "id": "self_loop",
                "layers": [
                    { "ty": 0, "refId": "self_loop", "w": 64, "h": 64, "ip": 0, "op": 10, "ks": {} }
                ]
            }
        ],
        "layers": [
            { "ty": 0, "refId": "self_loop", "w": 64, "h": 64, "ip": 0, "op": 10, "sr": 0, "ks": {} },
            { "ty": 3, "ind": 1, "parent": 2, "ip": 0, "op": 10, "ks": {} },
            { "ty": 3, "ind": 2, "parent": 1, "ip": 0, "op": 10, "ks": {} },
            {
                "ty": 4,
                "ind": 3,
                "parent": 1,
                "ip": 0,
                "op": 10,
                "ks": { "p": "not_an_object", "s": null, "r": [] },
                "shapes": [
                    { "ty": "unknown_shape", "foo": "bar" },
                    { "ty": "sh", "ks": { "a": 1, "k": [{ "t": 0, "s": "bad" }, { "t": 5 }] } },
                    { "ty": "gf", "g": { "p": 999999, "k": { "a": 0, "k": [0.0, 1.0] } } },
                    { "ty": "rp", "c": { "a": 0, "k": 999999 }, "tr": {} },
                    { "ty": "sr", "pt": { "a": 0, "k": -5 }, "or": { "a": 0, "k": 10 } },
                    { "ty": "fl", "c": { "a": 0, "k": [999, -50, 0.5] } }
                ]
            }
        ]
    }"#;
    let comp = Composition::from_json(hostile).expect("hostile parses without panic");
    let img = render_composition(&comp, 2.5, Size::new(64, 64));
    assert_eq!(img.size(), Size::new(64, 64));

    // Corrupt gzip bytes.
    assert!(Instance::from_bytes(&[0x1f, 0x8b, 0x08, 0xff, 0x00, 0xde, 0xad], None).is_err());

    // Corrupt cache blobs must never panic and must safely invalidate.
    let bad_caches: &[Vec<u8>] = &[vec![], vec![0; 10], vec![0xFF; 64], {
        // Valid-looking header with truncated frame payload
        let mut b = Vec::new();
        for v in [0i32, 16, 16, 16, 16, 30, 5, 2] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&(-99999i32).to_le_bytes());
        b
    }];
    for blob in bad_caches {
        let mut c = Cache::new(blob.clone(), FrameRequest::new(Size::new(16, 16)), None);
        assert_eq!(c.frames_ready(), 0);
        let mut dst = FrameBuffer::new(Size::new(16, 16));
        assert_eq!(
            c.render_frame(&mut dst, FrameRequest::new(Size::new(16, 16)), 0),
            FrameRenderResult::NotReady
        );
    }

    // FrameProviderDirect bounds checks (> MAX_FRAMES_COUNT rejected).
    let too_many_frames = r#"{"v":"5.5.2","fr":30,"ip":0,"op":500,"w":64,"h":64,"layers":[]}"#;
    let mut direct = FrameProviderDirect::new(Quality::High);
    assert!(direct.load(too_many_frames.as_bytes(), None).is_err());
}
