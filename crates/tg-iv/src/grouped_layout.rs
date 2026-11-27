//! Port of `ui/grouped_layout.h` and `ui/grouped_layout.cpp`.
//!
//! Computes Telegram's album / collage grid layout (`LayoutMediaGroup`) for
//! `pageBlockCollage`, matching `iv_rich_message_html_export.cpp`.

/// A 2D integer size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaSize {
    /// Width in logical pixels.
    pub width: i32,
    /// Height in logical pixels.
    pub height: i32,
}

impl MediaSize {
    /// Constructs a new `MediaSize`.
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }
}

/// A 2D integer rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaRect {
    /// Left coordinate.
    pub x: i32,
    /// Top coordinate.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

impl MediaRect {
    /// Constructs a new `MediaRect`.
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns true if the rectangle has non-positive width or height.
    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Returns the bounding box uniting `self` and `other`.
    pub fn united(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.width).max(other.x + other.width);
        let y2 = (self.y + self.height).max(other.y + other.height);
        Self::new(x1, y1, x2 - x1, y2 - y1)
    }
}

/// Layout placement for one item in a media collage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupMediaLayout {
    /// Item geometry inside the collage bounding box.
    pub geometry: MediaRect,
}

/// Default maximum width used by tdesktop (`st::historyGroupWidthMax`).
pub const DEFAULT_GROUP_WIDTH_MAX: i32 = 430;
/// Default minimum width used by tdesktop (`st::historyGroupWidthMin`).
pub const DEFAULT_GROUP_WIDTH_MIN: i32 = 100;
/// Default item spacing used by tdesktop (`st::historyGroupSkip`).
pub const DEFAULT_GROUP_SPACING: i32 = 4;

fn safe_round(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32
}

/// Computes Telegram Desktop's grouped media layout (`Ui::LayoutMediaGroup`).
pub fn layout_media_group(
    sizes: &[MediaSize],
    max_width: i32,
    min_width: i32,
    spacing: i32,
) -> Vec<GroupMediaLayout> {
    if sizes.is_empty() || max_width <= 0 {
        return Vec::new();
    }

    let normalized: Vec<MediaSize> = sizes
        .iter()
        .map(|s| MediaSize::new(s.width.max(1), s.height.max(1)))
        .collect();
    let ratios: Vec<f64> = normalized
        .iter()
        .map(|s| f64::from(s.width) / f64::from(s.height))
        .collect();
    let proportions: String = ratios
        .iter()
        .map(|&r| {
            if r > 1.2 {
                'w'
            } else if r < 0.8 {
                'n'
            } else {
                'q'
            }
        })
        .collect();

    let count = ratios.len();
    let max_height = max_width;
    let average_ratio = (1.0 + ratios.iter().sum::<f64>()) / (count as f64);
    let max_size_ratio = f64::from(max_width) / f64::from(max_height);

    if count == 1 {
        let width = max_width;
        // i64: a hostile server height times our width overflows i32.
        let height =
            i64::from(normalized[0].height) * i64::from(width) / i64::from(normalized[0].width);
        let height = i32::try_from(height).unwrap_or(i32::MAX);
        return vec![GroupMediaLayout {
            geometry: MediaRect::new(0, 0, width, height.max(1)),
        }];
    }

    if count >= 5 || ratios.iter().any(|&r| r > 2.0) {
        return layout_complex(&ratios, average_ratio, max_width, min_width, spacing);
    }

    match count {
        2 => layout_two(
            &ratios,
            &proportions,
            average_ratio,
            max_size_ratio,
            max_width,
            max_height,
            min_width,
            spacing,
        ),
        3 => layout_three(
            &ratios,
            &proportions,
            max_width,
            max_height,
            min_width,
            spacing,
        ),
        _ => layout_four(
            &ratios,
            &proportions,
            max_width,
            max_height,
            min_width,
            spacing,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn layout_two(
    ratios: &[f64],
    proportions: &str,
    average_ratio: f64,
    max_size_ratio: f64,
    max_width: i32,
    max_height: i32,
    min_width: i32,
    spacing: i32,
) -> Vec<GroupMediaLayout> {
    if proportions == "ww" && average_ratio > 1.4 * max_size_ratio && (ratios[1] - ratios[0]) < 0.2
    {
        let width = max_width;
        let height = safe_round(
            (f64::from(width) / ratios[0])
                .min(f64::from(width) / ratios[1])
                .min(f64::from(max_height - spacing) / 2.0),
        );
        return vec![
            GroupMediaLayout {
                geometry: MediaRect::new(0, 0, width, height),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(0, height + spacing, width, height),
            },
        ];
    }

    if proportions == "ww" || proportions == "qq" {
        let width = (max_width - spacing) / 2;
        let height = safe_round(
            (f64::from(width) / ratios[0])
                .min(f64::from(width) / ratios[1])
                .min(f64::from(max_height)),
        );
        return vec![
            GroupMediaLayout {
                geometry: MediaRect::new(0, 0, width, height),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(width + spacing, 0, width, height),
            },
        ];
    }

    let minimal_width = safe_round(f64::from(min_width) * 1.5);
    let second_width = safe_round(
        (0.4 * f64::from(max_width - spacing))
            .max(f64::from(max_width - spacing) / ratios[0] / (1.0 / ratios[0] + 1.0 / ratios[1])),
    )
    .min(max_width - spacing - minimal_width);
    let first_width = max_width - second_width - spacing;
    let height = max_height.min(safe_round(
        (f64::from(first_width) / ratios[0]).min(f64::from(second_width) / ratios[1]),
    ));
    vec![
        GroupMediaLayout {
            geometry: MediaRect::new(0, 0, first_width, height),
        },
        GroupMediaLayout {
            geometry: MediaRect::new(first_width + spacing, 0, second_width, height),
        },
    ]
}

fn layout_three(
    ratios: &[f64],
    proportions: &str,
    max_width: i32,
    max_height: i32,
    min_width: i32,
    spacing: i32,
) -> Vec<GroupMediaLayout> {
    if proportions.starts_with('n') {
        let first_height = max_height;
        let third_height = safe_round(
            (f64::from(max_height - spacing) / 2.0)
                .min(ratios[1] * f64::from(max_width - spacing) / (ratios[2] + ratios[1])),
        );
        let second_height = first_height - third_height - spacing;
        let right_width =
            min_width.max(safe_round((f64::from(max_width - spacing) / 2.0).min(
                (f64::from(third_height) * ratios[2]).min(f64::from(second_height) * ratios[1]),
            )));
        let left_width =
            safe_round(f64::from(first_height) * ratios[0]).min(max_width - spacing - right_width);
        vec![
            GroupMediaLayout {
                geometry: MediaRect::new(0, 0, left_width, first_height),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(left_width + spacing, 0, right_width, second_height),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(
                    left_width + spacing,
                    second_height + spacing,
                    right_width,
                    third_height,
                ),
            },
        ]
    } else {
        let first_width = max_width;
        let first_height = safe_round(
            (f64::from(first_width) / ratios[0]).min(f64::from(max_height - spacing) * 0.66),
        );
        let second_width = (max_width - spacing) / 2;
        let second_height = (max_height - first_height - spacing).min(safe_round(
            (f64::from(second_width) / ratios[1]).min(f64::from(second_width) / ratios[2]),
        ));
        let third_width = first_width - second_width - spacing;
        vec![
            GroupMediaLayout {
                geometry: MediaRect::new(0, 0, first_width, first_height),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(0, first_height + spacing, second_width, second_height),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(
                    second_width + spacing,
                    first_height + spacing,
                    third_width,
                    second_height,
                ),
            },
        ]
    }
}

fn layout_four(
    ratios: &[f64],
    proportions: &str,
    max_width: i32,
    max_height: i32,
    min_width: i32,
    spacing: i32,
) -> Vec<GroupMediaLayout> {
    if proportions.starts_with('w') {
        let w = max_width;
        let h0 = safe_round((f64::from(w) / ratios[0]).min(f64::from(max_height - spacing) * 0.66));
        let h =
            safe_round(f64::from(max_width - 2 * spacing) / (ratios[1] + ratios[2] + ratios[3]));
        let w0 = min_width.max(safe_round(
            (f64::from(max_width - 2 * spacing) * 0.4).min(f64::from(h) * ratios[1]),
        ));
        let w2 = safe_round(
            f64::from(min_width)
                .max(f64::from(max_width - 2 * spacing) * 0.33)
                .max(f64::from(h) * ratios[3]),
        );
        let w1 = w - w0 - w2 - 2 * spacing;
        let h1 = (max_height - h0 - spacing).min(h);
        vec![
            GroupMediaLayout {
                geometry: MediaRect::new(0, 0, w, h0),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(0, h0 + spacing, w0, h1),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(w0 + spacing, h0 + spacing, w1, h1),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(w0 + spacing + w1 + spacing, h0 + spacing, w2, h1),
            },
        ]
    } else {
        let h = max_height;
        let w0 = safe_round((f64::from(h) * ratios[0]).min(f64::from(max_width - spacing) * 0.6));
        let w = safe_round(
            f64::from(max_height - 2 * spacing)
                / (1.0 / ratios[1] + 1.0 / ratios[2] + 1.0 / ratios[3]),
        );
        let h0 = safe_round(f64::from(w) / ratios[1]);
        let h1 = safe_round(f64::from(w) / ratios[2]);
        let h2 = h - h0 - h1 - 2 * spacing;
        let w1 = min_width.max((max_width - w0 - spacing).min(w));
        vec![
            GroupMediaLayout {
                geometry: MediaRect::new(0, 0, w0, h),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(w0 + spacing, 0, w1, h0),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(w0 + spacing, h0 + spacing, w1, h1),
            },
            GroupMediaLayout {
                geometry: MediaRect::new(w0 + spacing, h0 + h1 + 2 * spacing, w1, h2),
            },
        ]
    }
}

fn layout_complex(
    raw_ratios: &[f64],
    average_ratio: f64,
    max_width: i32,
    min_width: i32,
    spacing: i32,
) -> Vec<GroupMediaLayout> {
    const MAX_RATIO: f64 = 2.75;
    const MIN_RATIO: f64 = 0.6667;

    let ratios: Vec<f64> = raw_ratios
        .iter()
        .map(|&r| {
            if average_ratio > 1.1 {
                r.clamp(1.0, MAX_RATIO)
            } else {
                r.clamp(MIN_RATIO, 1.0)
            }
        })
        .collect();
    let count = ratios.len();
    let max_height = max_width * 4 / 3;

    let multi_height = |offset: usize, row_count: usize| -> f64 {
        let sum: f64 = ratios[offset..offset + row_count].iter().sum();
        f64::from(max_width - ((row_count as i32) - 1) * spacing) / sum
    };

    struct Attempt {
        line_counts: Vec<usize>,
        heights: Vec<f64>,
    }

    let mut attempts: Vec<Attempt> = Vec::new();
    let push_attempt = |attempts: &mut Vec<Attempt>, line_counts: Vec<usize>| {
        let mut heights = Vec::with_capacity(line_counts.len());
        let mut offset = 0;
        for &c in &line_counts {
            heights.push(multi_height(offset, c));
            offset += c;
        }
        attempts.push(Attempt {
            line_counts,
            heights,
        });
    };

    for first in 1..count {
        let second = count - first;
        if first <= 3 && second <= 3 {
            push_attempt(&mut attempts, vec![first, second]);
        }
    }
    if count >= 3 {
        let max_second = if average_ratio < 0.85 { 4 } else { 3 };
        for first in 1..(count - 1) {
            for second in 1..(count - first) {
                let third = count - first - second;
                if first <= 3 && second <= max_second && third <= 3 {
                    push_attempt(&mut attempts, vec![first, second, third]);
                }
            }
        }
    }
    if count >= 4 {
        for first in 1..(count - 1) {
            for second in 1..(count - first) {
                for third in 1..(count - first - second) {
                    let fourth = count - first - second - third;
                    if first <= 3 && second <= 3 && third <= 3 && fourth <= 3 {
                        push_attempt(&mut attempts, vec![first, second, third, fourth]);
                    }
                }
            }
        }
    }

    // Fallback for >12 items (Telegram limits collages to 10 items, but guard cleanly).
    if attempts.is_empty() {
        let mut line_counts = Vec::new();
        let mut rem = count;
        while rem > 0 {
            let take = rem.min(3);
            line_counts.push(take);
            rem -= take;
        }
        push_attempt(&mut attempts, line_counts);
    }

    let mut optimal: Option<(&Attempt, f64)> = None;
    for attempt in &attempts {
        let line_count = attempt.line_counts.len();
        let total_height: f64 =
            attempt.heights.iter().sum::<f64>() + f64::from(spacing * (line_count as i32 - 1));
        let min_line_height = attempt
            .heights
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        let bad1 = if min_line_height < f64::from(min_width) {
            1.5
        } else {
            1.0
        };
        let mut bad2 = 1.0;
        for line in 1..line_count {
            if attempt.line_counts[line - 1] > attempt.line_counts[line] {
                bad2 = 1.5;
                break;
            }
        }
        let diff = (total_height - f64::from(max_height)).abs() * bad1 * bad2;
        if optimal.is_none_or(|(_, best)| diff < best) {
            optimal = Some((attempt, diff));
        }
    }

    let Some((best, _)) = optimal else {
        return Vec::new();
    };

    let mut result = vec![
        GroupMediaLayout {
            geometry: MediaRect::default(),
        };
        count
    ];
    let mut index = 0;
    let mut y = 0;
    for (&col_count, &line_height) in best.line_counts.iter().zip(best.heights.iter()) {
        let height = safe_round(line_height);
        let mut x = 0;
        for col in 0..col_count {
            let width = if col + 1 == col_count {
                max_width - x
            } else {
                safe_round(ratios[index] * line_height)
            };
            result[index] = GroupMediaLayout {
                geometry: MediaRect::new(x, y, width, height),
            };
            x += width + spacing;
            index += 1;
        }
        y += height + spacing;
    }

    result
}
