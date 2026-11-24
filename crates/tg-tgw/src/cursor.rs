//! CSS cursor style resolution, HTML `data-twid` shadow ancestor chaining, pointer/hover target
//! state, and touch gesture arbitration.
//!
//! Port origin: `Telegram/SourceFiles/tgw/tgw_cursor.h/.cpp`, `tgw_renderer_rules.h`,
//! `tgw_renderer_input.h/.cpp`, and `tgw_touch.h/.cpp`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Touch drag slop radius in CSS pixels (`twidget::gesture::TOUCH_SLOP_PX`).
pub const TOUCH_SLOP_PX: f64 = 8.0;

/// Minimum allowed device pixel ratio (`RendererRules::kMinRatio`).
pub const MIN_DEVICE_RATIO: f64 = 0.25;

/// Maximum allowed device pixel ratio (`RendererRules::kMaxRatio`).
pub const MAX_DEVICE_RATIO: f64 = 64.0;

/// HTML void element tags that do not push onto the open stack (`kVoidTags` in `tgw_cursor.cpp:43-58`).
const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Clamps device pixel ratio to `[0.25, 64.0]`, mapping NaN to `0.25` (`RendererRules::DeviceRatio`).
#[must_use]
pub fn clamp_device_ratio(ratio: f64) -> f64 {
    if ratio.is_nan() {
        MIN_DEVICE_RATIO
    } else {
        ratio.clamp(MIN_DEVICE_RATIO, MAX_DEVICE_RATIO)
    }
}

/// Returns `true` when `(dx, dy)` exceeds the touch slop threshold (`RendererRules::OutsideSlop`).
#[must_use]
pub fn outside_touch_slop(dx: f64, dy: f64) -> bool {
    dx.hypot(dy) > TOUCH_SLOP_PX
}

/// CSS cursor style reported by a widget (`tgw_cursor.cpp` / `tgw_hosted_glue.cpp`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorKind {
    /// Browser/host default (`"auto"`).
    #[default]
    Auto,
    /// Arrow cursor (`"default"`).
    Default,
    /// Pointing hand cursor (`"pointer"`).
    Pointer,
    /// Text I-beam cursor (`"text"`).
    Text,
    /// Action forbidden (`"not-allowed"`).
    NotAllowed,
    /// Grabbable (`"grab"`).
    Grab,
    /// Actively grabbing (`"grabbing"`).
    Grabbing,
    /// Wait/spinner (`"wait"`).
    Wait,
    /// Crosshair (`"crosshair"`).
    Crosshair,
    /// Move (`"move"`).
    Move,
}

impl CursorKind {
    /// Parses a CSS cursor keyword.
    #[must_use]
    pub fn from_css(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "pointer" => Self::Pointer,
            "default" => Self::Default,
            "text" => Self::Text,
            "not-allowed" => Self::NotAllowed,
            "grab" => Self::Grab,
            "grabbing" => Self::Grabbing,
            "wait" => Self::Wait,
            "crosshair" => Self::Crosshair,
            "move" => Self::Move,
            _ => Self::Auto,
        }
    }

    /// Formats this cursor as its CSS keyword.
    #[must_use]
    pub const fn as_css(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Default => "default",
            Self::Pointer => "pointer",
            Self::Text => "text",
            Self::NotAllowed => "not-allowed",
            Self::Grab => "grab",
            Self::Grabbing => "grabbing",
            Self::Wait => "wait",
            Self::Crosshair => "crosshair",
            Self::Move => "move",
        }
    }

    /// Returns `true` if this cursor should display as a pointing hand.
    #[must_use]
    pub const fn is_hand(self) -> bool {
        matches!(self, Self::Pointer)
    }
}

/// Computed `:hover` and `:active` element IDs (`RendererRules::HoverTargets` in `tgw_renderer_rules.h:58-61`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoverTargets {
    /// Element ID receiving `:hover` (`0` = none).
    pub hover: i64,
    /// Element ID receiving `:active` (`0` = none).
    pub active: i64,
}

/// Computes effective `:hover` and `:active` targets given current hit and press state
/// (`RendererRules::PointerTargets` in `tgw_renderer_rules.h:124-135`).
#[must_use]
pub const fn pointer_targets(
    hit: i64,
    press_held: bool,
    synced: bool,
    press_hit: i64,
    active: bool,
) -> HoverTargets {
    let hover = if press_held {
        if synced { press_hit } else { 0 }
    } else {
        hit
    };
    HoverTargets {
        hover,
        active: if synced && active && hover == press_hit {
            hover
        } else {
            0
        },
    }
}

/// Extracts the ancestor `data-twid` chain (from target up to root) from serialized widget HTML
/// (`ShadowChain` in `tgw_cursor.cpp:103-140`).
#[must_use]
pub fn shadow_chain(html: &str, target_id: i64) -> Vec<i64> {
    const SERIALIZED_ID: &str = " data-twid=\"";
    let mut open = Vec::new();
    let bytes = html.as_bytes();
    let mut at = match html.find('<') {
        Some(pos) => pos,
        None => return Vec::new(),
    };

    while at < bytes.len() {
        let tag = &html[at + 1..];
        if tag.starts_with('/') {
            let _ = open.pop();
        } else if !tag.starts_with('!') {
            let name_end = tag.find([' ', '>']).unwrap_or(tag.len());
            let name = &tag[..name_end];
            let attrs = &tag[name_end..];
            let own = if let Some(rest) = attrs.strip_prefix(SERIALIZED_ID) {
                let end = rest.find('"').unwrap_or(0);
                rest[..end].parse::<i64>().unwrap_or(0)
            } else {
                0
            };

            if target_id == 0 || own == target_id {
                let mut result = Vec::with_capacity(open.len() + 1);
                result.push(own);
                result.extend(open.iter().rev().copied());
                return result;
            }

            let is_void = VOID_TAGS.iter().any(|v| v.eq_ignore_ascii_case(name));
            if !is_void {
                open.push(own);
            }

            if name.eq_ignore_ascii_case("style") {
                if let Some(close) = html[at..].find("</") {
                    at += close;
                    continue;
                }
                break;
            }
        }
        match html[at + 1..].find('<') {
            Some(next) => at += 1 + next,
            None => break,
        }
    }

    Vec::new()
}

/// Per-element cursor rule with optional `:hover` and `:active` overrides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ElementCursorRule {
    /// Base cursor style (`cursor` property).
    pub base: Option<CursorKind>,
    /// Override when `:hover` matches.
    pub hover: Option<CursorKind>,
    /// Override when `:active` matches.
    pub active: Option<CursorKind>,
}

/// Evaluates inherited CSS cursor rules over a `data-twid` element hierarchy with `:hover` and
/// `:active` pseudo-class propagation (`Tgw::CursorStyles` in `tgw_cursor.h/.cpp`).
#[derive(Debug, Default, Clone)]
pub struct CursorStyles {
    html: String,
    rules: HashMap<i64, ElementCursorRule>,
    hover_id: i64,
    active_id: i64,
}

impl CursorStyles {
    /// Creates an empty cursor stylesheet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates the serialized HTML tree used for ancestor lookup.
    pub fn set_html(&mut self, html: impl Into<String>) {
        self.html = html.into();
    }

    /// Registers a cursor rule for element `twid`.
    pub fn set_rule(&mut self, twid: i64, rule: ElementCursorRule) {
        self.rules.insert(twid, rule);
    }

    /// Resolves the effective [`CursorKind`] at element `hit` given active `:hover` and `:active`
    /// target IDs, inheriting up the [`shadow_chain`] just like CSS `cursor`.
    #[must_use]
    pub fn resolve(&mut self, hit: i64, hover: i64, active: i64) -> CursorKind {
        self.hover_id = hover;
        self.active_id = active;

        let hover_chain: HashSet<i64> = if hover != 0 {
            shadow_chain(&self.html, hover).into_iter().collect()
        } else {
            HashSet::new()
        };
        let active_chain: HashSet<i64> = if active != 0 {
            shadow_chain(&self.html, active).into_iter().collect()
        } else {
            HashSet::new()
        };

        let chain = shadow_chain(&self.html, hit);
        for id in chain {
            if let Some(rule) = self.rules.get(&id) {
                if active_chain.contains(&id)
                    && let Some(c) = rule.active
                {
                    return c;
                }
                if hover_chain.contains(&id)
                    && let Some(c) = rule.hover
                {
                    return c;
                }
                if let Some(c) = rule.base {
                    return c;
                }
            }
        }
        CursorKind::Auto
    }

    /// Convenience helper returning `true` if [`Self::resolve`] yields [`CursorKind::Pointer`].
    #[must_use]
    pub fn is_pointer(&mut self, hit: i64, hover: i64, active: i64) -> bool {
        self.resolve(hit, hover, active).is_hand()
    }
}

/// Touch action codes matching `RendererRules::kTouch*` (`tgw_renderer_rules.h:30-33`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum TouchAction {
    /// Finger/button pressed (`0`).
    Down = 0,
    /// Finger/button moved (`1`).
    Move = 1,
    /// Finger/button released (`2`).
    Up = 2,
    /// Gesture cancelled (`3`).
    Cancel = 3,
}

/// Hover action codes matching `RendererRules::kHover*` (`tgw_renderer_rules.h:34-35`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum HoverAction {
    /// Pointer moved inside card (`0`).
    Move = 0,
    /// Pointer left card (`1`).
    Leave = 1,
}

/// Gesture claim returned by a widget on finger down (`twidget::WidgetGestureClaim`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GestureClaim {
    /// Widget does not claim drag gestures (chat scroll/swipe wins).
    #[default]
    None,
    /// Widget claims horizontal drags.
    Horizontal,
    /// Widget claims vertical drags.
    Vertical,
    /// Widget claims all drag directions.
    All,
}

/// Touch ownership state machine (`WidgetTouch::Owner` in `tgw_touch.h:69-77`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TouchOwner {
    /// No active touch.
    #[default]
    None,
    /// Waiting for widget claim / movement past slop.
    Pending,
    /// Widget owns the gesture.
    Widget,
    /// Context menu owns the gesture.
    Menu,
    /// Chat reply swipe owns the gesture.
    Swipe,
    /// Chat list vertical scroll owns the gesture.
    Scroll,
    /// Touch abandoned/cancelled.
    Lost,
}
