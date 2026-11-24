//! Video quality / resolution choice levels and slider selection state.
//!
//! Port of `Telegram/SourceFiles/editor/video/video_editor_quality.{h,cpp}` and
//! `Telegram/SourceFiles/editor/video/video_quality_slider.{h,cpp}`.

use crate::geometry::Size;
use crate::video::common::downscaled_size;

/// Canonical shorter-side resolution steps offered by tdesktop (`kLevels`).
pub const QUALITY_LEVEL_SIDES: [i32; 4] = [360, 480, 720, 1080];

/// One selectable output quality tier (`Editor::VideoQualityLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoQualityLevel {
    /// Target shorter side in pixels (`0` means original resolution is kept).
    pub shorter_side: i32,
    /// Output resolution for this tier.
    pub resolution: Size,
}

impl VideoQualityLevel {
    /// Returns `true` when this level represents the original resolution (`shorter_side == 0`).
    #[must_use]
    pub const fn is_original(&self) -> bool {
        self.shorter_side == 0
    }
}

/// Builds the ordered list of selectable quality levels for an edited frame size
/// (`Editor::VideoQualityLevels`). Always ends with the `Original` tier (`shorter_side = 0`).
#[must_use]
pub fn video_quality_levels(edited: Size) -> Vec<VideoQualityLevel> {
    let mut result = Vec::new();
    if edited.is_empty() {
        return result;
    }
    let shorter = edited.width.min(edited.height);
    for side in QUALITY_LEVEL_SIDES {
        if side >= shorter {
            break;
        }
        if let Some(resolution) = downscaled_size(edited, side)
            && !resolution.is_empty()
        {
            result.push(VideoQualityLevel {
                shorter_side: side,
                resolution,
            });
        }
    }
    result.push(VideoQualityLevel {
        shorter_side: 0,
        resolution: edited,
    });
    result
}

/// Human-readable label for a quality level (`"360p"`, `"720p"`, `"Original"`),
/// matching `Editor::VideoQualityLabel`.
#[must_use]
pub fn video_quality_label(level: &VideoQualityLevel) -> String {
    if level.shorter_side > 0 {
        format!("{}p", level.shorter_side)
    } else {
        "Original".to_owned()
    }
}

/// UI-independent quality selector state (`Editor::VideoQualitySlider` logic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoQualitySelection {
    levels: Vec<VideoQualityLevel>,
    selected_index: usize,
}

impl VideoQualitySelection {
    /// Creates a quality selector for `edited` dimensions, defaulting to `"Original"`.
    #[must_use]
    pub fn for_size(edited: Size) -> Self {
        let levels = video_quality_levels(edited);
        let selected_index = levels.len().saturating_sub(1);
        Self {
            levels,
            selected_index,
        }
    }

    /// Returns all available quality levels (lowest to Original).
    #[must_use]
    pub fn levels(&self) -> &[VideoQualityLevel] {
        &self.levels
    }

    /// Returns `true` when more than one quality tier is available to pick.
    #[must_use]
    pub fn has_choice(&self) -> bool {
        self.levels.len() > 1
    }

    /// Returns the currently selected index.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Returns the currently selected [`VideoQualityLevel`], if any.
    #[must_use]
    pub fn selected_level(&self) -> Option<VideoQualityLevel> {
        self.levels.get(self.selected_index).copied()
    }

    /// Returns the selected target shorter side (`0` for Original).
    #[must_use]
    pub fn selected_shorter_side(&self) -> i32 {
        self.selected_level().map_or(0, |l| l.shorter_side)
    }

    /// Selects a tier by index (clamped to valid range).
    pub fn set_index(&mut self, index: usize) {
        if self.levels.is_empty() {
            self.selected_index = 0;
        } else {
            self.selected_index = index.min(self.levels.len() - 1);
        }
    }

    /// Selects the tier matching `shorter_side` (`0` = Original), or falls back to Original.
    pub fn set_shorter_side(&mut self, shorter_side: i32) {
        if let Some(pos) = self
            .levels
            .iter()
            .position(|l| l.shorter_side == shorter_side)
        {
            self.selected_index = pos;
        } else {
            self.selected_index = self.levels.len().saturating_sub(1);
        }
    }

    /// Recomputes available tiers after a crop or rotation change, preserving the active
    /// shorter-side preference if still available.
    pub fn update_for_size(&mut self, edited: Size) {
        let prev = self.selected_shorter_side();
        self.levels = video_quality_levels(edited);
        self.set_shorter_side(prev);
    }
}
