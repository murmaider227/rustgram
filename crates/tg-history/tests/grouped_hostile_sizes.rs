//! TGB-3082: album and playlist layout must not overflow i32 on hostile media sizes from the server.

use tg_history::geometry::Size;
use tg_history::grouped::{layout_media_group, layout_playlist};

#[test]
fn album_layout_with_hostile_sizes_does_not_overflow() {
    let sizes = [
        Size {
            width: 4002,
            height: -1,
        },
        Size {
            width: 255,
            height: i32::MAX,
        },
        Size {
            width: 218,
            height: 0,
        },
    ];
    let _ = layout_media_group(&sizes, 430, 100, 4);
    let _ = layout_playlist(&sizes);
    let sizes = [
        Size {
            width: i32::MAX,
            height: -1,
        },
        Size {
            width: 1406,
            height: 712,
        },
        Size {
            width: 488,
            height: 255,
        },
        Size {
            width: 255,
            height: 0,
        },
    ];
    let _ = layout_media_group(&sizes, 430, 100, 4);
    let sizes = [
        Size {
            width: 4002,
            height: -1,
        },
        Size {
            width: 255,
            height: i32::MAX,
        },
        Size {
            width: 218,
            height: 0,
        },
        Size {
            width: 2044,
            height: 1,
        },
        Size {
            width: 1311,
            height: 3524,
        },
        Size {
            width: 728,
            height: 1728,
        },
    ];
    let _ = layout_media_group(&sizes, 430, 100, 4);
    let _ = layout_playlist(&sizes);
}

#[test]
fn single_hostile_photo_keeps_a_finite_height() {
    let one = layout_media_group(&[Size::new(1, i32::MAX)], 430, 100, 4);
    assert_eq!(one.len(), 1);
    assert!(one[0].geometry.height > 0, "{:?}", one[0].geometry);
    // Sides below 1 are 1 (tdesktop `sizeForGroupingOptimal`): a 1x1 square.
    let unknown = layout_media_group(&[Size::new(0, -5)], 430, 100, 4);
    assert_eq!(unknown[0].geometry.width, 430);
    assert_eq!(unknown[0].geometry.height, 430);
}

/// Deterministic edge-value sweep (xorshift), the same shape the PANICS fuzz used.
#[test]
fn album_layout_survives_edge_value_sweep() {
    const EDGES: [i32; 10] = [i32::MIN, -1, 0, 1, 2, 255, 1280, 4096, 1 << 20, i32::MAX];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..20_000 {
        let count = (next() % 12) as usize + 1;
        let sizes: Vec<Size> = (0..count)
            .map(|_| {
                let pick = |r: u64| {
                    if r.is_multiple_of(3) {
                        EDGES[(r / 3 % EDGES.len() as u64) as usize]
                    } else {
                        (r % 5000) as i32
                    }
                };
                Size::new(pick(next()), pick(next()))
            })
            .collect();
        let max_width = 100 + (next() % 1500) as i32;
        let _ = layout_media_group(&sizes, max_width, 100, 4);
        let _ = layout_playlist(&sizes);
    }
}

#[test]
fn real_album_sizes_lay_out_as_before() {
    // Golden geometry from the code before the TGB-3082 clamp: real sizes lay out unchanged.
    type Rects = Vec<(i32, i32, i32, i32)>;
    let cases: Vec<(Vec<Size>, Rects)> = vec![
        (
            vec![
                Size::new(1280, 853),
                Size::new(853, 1280),
                Size::new(1280, 720),
                Size::new(960, 1280),
            ],
            vec![
                (0, 0, 430, 281),
                (0, 285, 100, 132),
                (104, 285, 183, 132),
                (291, 285, 139, 132),
            ],
        ),
        (
            vec![
                Size::new(1280, 853),
                Size::new(1280, 853),
                Size::new(720, 1280),
            ],
            vec![(0, 0, 430, 281), (0, 285, 213, 142), (217, 285, 213, 142)],
        ),
        (
            vec![
                Size::new(853, 1280),
                Size::new(1280, 853),
                Size::new(1280, 853),
            ],
            vec![(0, 0, 213, 430), (217, 0, 213, 213), (217, 217, 213, 213)],
        ),
        (
            vec![Size::new(1280, 720); 7],
            vec![
                (0, 0, 430, 242),
                (0, 246, 213, 120),
                (217, 246, 213, 120),
                (0, 370, 213, 120),
                (217, 370, 213, 120),
                (0, 494, 213, 120),
                (217, 494, 213, 120),
            ],
        ),
        (vec![Size::new(720, 1280)], vec![(0, 0, 430, 764)]),
        (vec![Size::new(5000, 1)], vec![(0, 0, 430, 0)]),
    ];
    for (sizes, expected) in cases {
        let got: Rects = layout_media_group(&sizes, 430, 100, 4)
            .iter()
            .map(|l| {
                (
                    l.geometry.x,
                    l.geometry.y,
                    l.geometry.width,
                    l.geometry.height,
                )
            })
            .collect();
        assert_eq!(got, expected, "{sizes:?}");
    }
}
