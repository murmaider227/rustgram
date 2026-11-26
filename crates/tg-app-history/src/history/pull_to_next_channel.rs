//! Pull to the next unread channel: the gesture, its indicator and the hint over the bottom bar.
//!
//! Port origin:
//! - `Telegram/SourceFiles/history/history_view_pull_to_next_channel.h`
//! - `Telegram/SourceFiles/history/history_view_pull_to_next_channel.cpp`
//! - `Telegram/lib_ui/ui/widgets/elastic_scroll.cpp` (the bottom pull edge the gesture rides on)
//! - `Telegram/SourceFiles/chat_helpers/chat_helpers.style` (`historyPullNext*`)
//!
//! tdesktop reads the gesture from the scroll area's overscroll: only a phased scroll (a finger
//! on a touch screen, or a trackpad that reports begin and end) can stretch the list past its
//! bottom. A classic mouse wheel has no phase and never stretches it, so wheel scrolling, however
//! fast, never switches the channel. The switch needs the stretch to pass the threshold, stay
//! there for [`READY_DWELL_SECS`] with the fingers still down, and then the fingers to lift.
//!
//! Internal design: tdesktop spreads this over `ElasticScroll` (a widget), two child widgets and
//! reactive streams. Here it is two plain state machines ([`BottomStretch`] and the widget's
//! gesture fields) fed once per frame; nothing is allocated or rebuilt between frames.

use egui::accesskit::Role;
use egui::{
    Color32, CornerRadius, Event, FontId, Id, MouseWheelSource, MouseWheelUnit, Pos2, Rect, Sense,
    Shape, Stroke, TouchPhase, Ui, Vec2, WidgetInfo,
};
use tg_app_core::stand_in::types::PeerId;
use tg_lang::tr;
use tg_ui::anim::Transition;
use tg_ui::fonts::{family_regular, family_semibold};
use tg_ui::tg_style::st;
use tg_ui::userpic::EmptyUserpic;

use crate::history::theme::HistoryColors;

/// `kResetReachedOn`.
const RESET_REACHED_ON: f32 = 0.95;
/// `kReverseEase`.
const REVERSE_EASE: f32 = 1.5;
/// `kReadyDwell`: how long the stretch stays past the threshold, fingers down, before it arms.
pub const READY_DWELL_SECS: f64 = 0.120;
/// `kExpandDuration`.
const EXPAND_DURATION_SECS: f64 = 0.250;
/// `kReleaseShowDuration` / `kReleaseHideDuration`.
const RELEASE_DURATION_SECS: f64 = 0.250;
/// `kPanelDuration`.
const PANEL_DURATION_SECS: f64 = 0.320;
/// `kBounceDuration`.
const BOUNCE_DURATION_SECS: f64 = 0.400;

/// `kPullBandStiffness`: input travel per pixel of stretch within the pull distance.
const PULL_BAND_STIFFNESS: f32 = 2.5;
/// `kRubberBandStiffness`: input travel per pixel of stretch past the pull distance.
const RUBBER_BAND_STIFFNESS: f32 = 20.0;
/// `kRubberBandAmplitude`.
const RUBBER_BAND_AMPLITUDE: f32 = 0.31;
/// `kRubberBandPeriod`.
const RUBBER_BAND_PERIOD: f32 = 1.6;
/// `kRubberBandMaxReturnDuration`.
const MAX_RETURN_SECS: f64 = 1.0;
/// `kVelocityZeroingTimeout`.
const VELOCITY_ZEROING_SECS: f64 = 0.100;
/// `kMaxTrackedVelocity`.
const MAX_TRACKED_VELOCITY: f32 = 6000.0;
/// `kTouchOverscrollMultiplier`.
const TOUCH_OVERSCROLL_MULTIPLIER: f32 = 2.0;
/// `st::historyScroll.width`: the indicator is centred in the list without its scroll bar.
const HISTORY_SCROLL_WIDTH: f32 = 12.0;
/// The growing userpic shows its initials from this size on (smaller, the text would spill).
const MIN_SIZE_WITH_INITIALS: f32 = 20.0;
/// egui's size of one wheel line, for a phased gesture that reports lines.
const WHEEL_LINE_POINTS: f32 = 50.0;

/// Summary of the next unread broadcast channel candidate (`FindNextUnreadChannel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextUnreadChannelHint {
    /// Destination broadcast channel peer ID.
    pub peer_id: PeerId,
    /// Display title of the next unread broadcast channel.
    pub title: String,
    /// Unread message count shown inside the floating unread badge.
    pub unread_count: u32,
    /// Avatar palette color index (`0..7`).
    pub color_index: u8,
}

/// Exact tdesktop cubic bounce curve (`BounceProgress` in `history_view_pull_to_next_channel.cpp:219-233`).
#[must_use]
pub fn bounce_progress(t: f64) -> f64 {
    let ease = |x: f64| -> f64 {
        if x < 0.5 {
            4.0 * x * x * x
        } else {
            let u = -2.0 * x + 2.0;
            1.0 - u * u * u / 2.0
        }
    };
    let t = t.clamp(0.0, 1.0);
    if t < 0.45 {
        ease(t / 0.45)
    } else if t < 0.75 {
        1.0 - ease((t - 0.45) / 0.3) * 1.5
    } else {
        -0.5 + ease((t - 0.75) / 0.25) * 0.5
    }
}

/// Stretch past the bottom that arms the switch: `PullThreshold()`
/// (`st::historyPullNextThreshold * 2`, `history_view_pull_to_next_channel.cpp:57-63`).
#[must_use]
pub fn pull_threshold_px() -> f32 {
    st::history_pull_next_threshold.0 as f32 * 2.0
}

/// Phase of one scroll event (`Qt::ScrollPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelPhase {
    /// A classic mouse wheel step (`Qt::NoScrollPhase`): it never stretches the list.
    Unphased,
    /// The fingers touched down (`Qt::ScrollBegin`).
    Begin,
    /// The fingers moved (`Qt::ScrollUpdate`).
    Update,
    /// Inertia after the fingers lifted (`Qt::ScrollMomentum`).
    Momentum,
    /// The gesture ended (`Qt::ScrollEnd`).
    End,
}

/// What the scroll is doing (`Ui::ElasticScrollMovement`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollMovement {
    /// At rest.
    #[default]
    None,
    /// Fingers down.
    Progress,
    /// Inertia.
    Momentum,
    /// The stretch springs back.
    Returning,
}

/// `PullOverscrollFromAccumulated`: the stretch that `accumulated` input travel gives.
fn stretch_from_accumulated(accumulated: f32, distance: f32) -> f32 {
    let soft = distance * PULL_BAND_STIFFNESS;
    if accumulated <= soft {
        accumulated / PULL_BAND_STIFFNESS
    } else {
        distance + (accumulated - soft) / RUBBER_BAND_STIFFNESS
    }
}

/// `PullAccumulatedFromOverscroll`: the input travel that gives `stretch`.
fn accumulated_from_stretch(stretch: f32, distance: f32) -> f32 {
    if stretch <= distance {
        stretch * PULL_BAND_STIFFNESS
    } else {
        distance * PULL_BAND_STIFFNESS + (stretch - distance) * RUBBER_BAND_STIFFNESS
    }
}

#[derive(Debug, Clone, Copy)]
struct ReturnSpring {
    start: f64,
    x0: f32,
    v0: f32,
    peak_time: f64,
}

/// The bottom edge of `Ui::ElasticScroll` as a pull edge: turns phased scroll input past the
/// bottom into a stretch, and springs it back when the fingers lift.
#[derive(Debug, Default)]
struct BottomStretch {
    /// Input travel past the bottom (`_overscrollAccumulated`).
    accumulated: f32,
    /// Stretch past the bottom (`_overscroll`).
    stretch: f32,
    movement: ScrollMovement,
    spring: Option<ReturnSpring>,
    /// `_ignoreMomentumFromOverscroll`: inertia that follows a release adds no stretch.
    ignore_momentum: bool,
    velocity: f32,
    velocity_time: Option<f64>,
    last_event_time: Option<f64>,
    last_was_momentum: bool,
}

impl BottomStretch {
    /// `ElasticScroll::handleWheelEvent` for input at the bottom edge. `down` is the travel
    /// towards newer messages; `allowed` says the pull edge is live (`setOverscrollEdges`,
    /// `setOverscrollPullDistances`).
    fn feed(&mut self, phase: WheelPhase, down: f32, touch: bool, allowed: bool, now: f64) {
        if phase == WheelPhase::Unphased {
            if self.stretch > 0.0 {
                if self.spring.is_none() {
                    self.start_return(now);
                }
            } else {
                self.movement = ScrollMovement::None;
            }
            return;
        }
        let momentum = matches!(phase, WheelPhase::Momentum | WheelPhase::End);
        self.track_velocity(phase, down, now);
        if self.ignore_momentum {
            if !momentum {
                self.ignore_momentum = false;
            } else if down >= 0.0 {
                return;
            } else if self.spring.is_some() {
                // Opposite input during the bounce is a new gesture whose begin was lost.
                self.ignore_momentum = false;
                self.cancel_return();
            }
        }
        if !momentum {
            self.cancel_return();
        }
        let distance = pull_threshold_px();
        // Scrolling away from the stretch consumes it 1:1 (`ReconcileStretchAndScroll`).
        if down < 0.0 && self.stretch > 0.0 && self.spring.is_none() {
            self.stretch = (self.stretch + down).max(0.0);
            self.accumulated = accumulated_from_stretch(self.stretch, distance);
        }
        if momentum {
            if self.spring.is_none() {
                if self.stretch > 0.0 {
                    self.start_return(now);
                } else if phase == WheelPhase::End {
                    self.movement = ScrollMovement::None;
                } else {
                    self.movement = ScrollMovement::Momentum;
                }
            }
            // Inertia that reaches the edge gives tdesktop a short plain bounce, never a pull;
            // that bounce is not drawn here.
            return;
        }
        if down <= 0.0 {
            return;
        }
        if !allowed {
            self.accumulated = 0.0;
            self.stretch = 0.0;
            return;
        }
        self.accumulated += if touch {
            down * TOUCH_OVERSCROLL_MULTIPLIER
        } else {
            down
        };
        self.stretch = stretch_from_accumulated(self.accumulated, distance);
    }

    /// `ElasticScroll::trackWheelVelocity`.
    fn track_velocity(&mut self, phase: WheelPhase, down: f32, now: f64) {
        let momentum = phase == WheelPhase::Momentum;
        if phase != WheelPhase::Update && !momentum {
            if phase == WheelPhase::Begin {
                self.velocity = 0.0;
                self.velocity_time = None;
                self.last_event_time = Some(now);
            }
            self.last_was_momentum = false;
            return;
        }
        // Events of one frame share its time; they keep the velocity of the frame before.
        let elapsed = self.last_event_time.map_or(0.0, |last| now - last);
        if elapsed > 0.0 && elapsed < VELOCITY_ZEROING_SECS {
            self.velocity =
                (down / elapsed as f32).clamp(-MAX_TRACKED_VELOCITY, MAX_TRACKED_VELOCITY);
            self.velocity_time = Some(now);
        } else if elapsed > 0.0 && (!momentum || self.last_was_momentum) {
            self.velocity = 0.0;
            self.velocity_time = Some(now);
        }
        self.last_event_time = Some(now);
        self.last_was_momentum = momentum;
    }

    /// `ElasticScroll::overscrollReturn` and `overscrollSpringStart`.
    fn start_return(&mut self, now: f64) {
        self.ignore_momentum = true;
        if self.stretch <= 0.0 {
            self.finish_return();
            return;
        }
        self.movement = ScrollMovement::Returning;
        let x0 = self.stretch;
        let v0 = match self.velocity_time {
            Some(at) if now - at <= VELOCITY_ZEROING_SECS => self.velocity,
            _ => 0.0,
        };
        let push = v0 * RUBBER_BAND_AMPLITUDE;
        let decay = RUBBER_BAND_STIFFNESS / RUBBER_BAND_PERIOD;
        let peak_time = if push > 0.0 {
            f64::from((1.0 / decay - x0 / push).max(0.0))
        } else {
            0.0
        };
        self.spring = Some(ReturnSpring {
            start: now,
            x0,
            v0,
            peak_time,
        });
    }

    /// `ElasticScroll::overscrollReturnCancel`.
    fn cancel_return(&mut self) {
        self.movement = ScrollMovement::Progress;
        if self.spring.take().is_some() {
            self.accumulated = accumulated_from_stretch(self.stretch, pull_threshold_px());
        }
    }

    fn finish_return(&mut self) {
        self.spring = None;
        self.accumulated = 0.0;
        self.stretch = 0.0;
        self.movement = ScrollMovement::None;
    }

    /// `ElasticScroll::overscrollSpringUpdate`: a critically damped spring back to the edge.
    fn tick(&mut self, now: f64) {
        let Some(spring) = self.spring else {
            return;
        };
        let time = now - spring.start;
        let t = time as f32;
        let decay = RUBBER_BAND_STIFFNESS / RUBBER_BAND_PERIOD;
        let value = (spring.x0 + spring.v0 * RUBBER_BAND_AMPLITUDE * t) * (-decay * t).exp();
        if time >= MAX_RETURN_SECS || value <= 0.0 || (value < 0.5 && time >= spring.peak_time) {
            self.finish_return();
        } else {
            self.stretch = value;
        }
    }
}

/// One value animation (`Ui::Animations::Simple`).
#[derive(Debug, Clone, Copy)]
struct Anim {
    start: f64,
    from: f64,
    to: f64,
    duration: f64,
    ease: Transition,
}

impl Anim {
    fn value(anim: Option<Self>, now: f64, rest: f64) -> f64 {
        let Some(a) = anim else {
            return rest;
        };
        if a.duration <= 0.0 || now >= a.start + a.duration {
            return a.to;
        }
        let dt = ((now - a.start) / a.duration).clamp(0.0, 1.0);
        a.from + a.ease.eval(a.to - a.from, dt)
    }

    fn running(anim: Option<Self>, now: f64) -> bool {
        anim.is_some_and(|a| now < a.start + a.duration)
    }
}

fn flag(on: bool) -> f64 {
    if on { 1.0 } else { 0.0 }
}

/// What the gesture needs to know about the open chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullContext {
    /// `PullToNextChannel::active()`: the chat is a broadcast channel and the list shows the
    /// end of a history loaded to its newest message.
    pub active: bool,
    /// Another channel has unread messages to switch to.
    pub has_next: bool,
}

/// The pull-to-next-channel gesture and its two overlays (`HistoryView::PullToNextChannel`).
#[derive(Debug, Default)]
pub struct PullToNextChannelWidget {
    edge: BottomStretch,
    /// The "pull at the bottom of a channel" setting is off (`Core::Settings::pullToNextChannel`).
    disabled: bool,
    /// A phased wheel gesture is between its begin and its end.
    wheel_gesture: bool,
    /// A finger that went down on the list is still down.
    touch_gesture: bool,
    pulling: bool,
    holding: bool,
    committed: bool,
    reached: bool,
    expand_to: bool,
    pull: f32,
    peak_pull: f32,
    expand: Option<Anim>,
    dwell_until: Option<f64>,
    /// The `ready` state the indicator and the hint last animated to.
    shown_ready: bool,
    release: Option<Anim>,
    bounce_start: Option<f64>,
    hint_visible: bool,
    hint_panel: Option<Anim>,
    reduced_motion: bool,
    /// A fixed state set by [`Self::set_pull_preview`]; input is ignored while it is shown.
    preview: bool,
}

impl PullToNextChannelWidget {
    /// Creates an idle pull-to-next-channel controller.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops the gesture and hides both overlays at once (`reset(anim::type::instant)`).
    pub fn reset(&mut self) {
        *self = Self {
            disabled: self.disabled,
            ..Self::default()
        };
    }

    /// Follows the "pull at the bottom of a channel for the next unread one" setting.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.disabled = !enabled;
    }

    /// Shows a held pull of `pull_px` (snapshots and tests); stays until [`Self::reset`].
    pub fn set_pull_preview(&mut self, pull_px: f32, _now_secs: f64) {
        self.reset();
        let pull = pull_px.max(0.0);
        let ready = pull >= pull_threshold_px();
        self.preview = true;
        self.pulling = pull > 0.0;
        self.holding = pull > 0.0;
        self.pull = pull;
        self.peak_pull = pull;
        self.reached = ready;
        self.expand_to = ready;
        self.shown_ready = ready;
        self.hint_visible = pull > 0.0;
    }

    /// Current stretch past the bottom of the list, logical pixels.
    #[must_use]
    pub const fn pull_px(&self) -> f32 {
        self.pull
    }

    /// `true` once the stretch stayed past the threshold long enough: lifting the fingers now
    /// switches the channel.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.reached
    }

    /// `true` while the hint over the bottom bar is shown or fading.
    #[must_use]
    pub fn hint_shown(&self, now_secs: f64) -> bool {
        Anim::value(self.hint_panel, now_secs, flag(self.hint_visible)) > 0.0
    }

    fn duration(&self, secs: f64) -> f64 {
        if self.reduced_motion { 0.0 } else { secs }
    }

    /// Feeds one scroll event that arrived while the list was at its bottom. `down_px` is the
    /// travel towards newer messages. Returns `true` when the gesture switches the channel.
    pub fn feed_scroll(
        &mut self,
        phase: WheelPhase,
        down_px: f32,
        touch: bool,
        now_secs: f64,
        cx: PullContext,
    ) -> bool {
        let allowed = cx.active && !self.disabled;
        self.edge.feed(phase, down_px, touch, allowed, now_secs);
        self.handle_overscroll(now_secs, cx)
    }

    /// Advances the return spring and the dwell timer. Returns `true` when the gesture switches
    /// the channel.
    pub fn advance(&mut self, now_secs: f64, cx: PullContext) -> bool {
        self.edge.tick(now_secs);
        if self.dwell_until.is_some_and(|at| now_secs >= at) {
            self.dwell_until = None;
            if self.pulling && self.pull >= pull_threshold_px() {
                self.reached = true;
                self.peak_pull = self.pull;
                self.start_expand(true, now_secs);
                self.sync_overlays(now_secs, cx);
            }
        }
        self.handle_overscroll(now_secs, cx)
    }

    /// `PullToNextChannel::handleOverscroll`.
    fn handle_overscroll(&mut self, now: f64, cx: PullContext) -> bool {
        let active = cx.active && !self.disabled;
        let pull = self.edge.stretch.max(0.0);
        let movement = self.edge.movement;
        let threshold = pull_threshold_px();
        if !self.pulling {
            if movement != ScrollMovement::Progress || pull <= 0.0 || !active {
                return false;
            }
            self.pulling = true;
            self.committed = false;
        }
        self.pull = pull;
        self.holding = movement == ScrollMovement::Progress;
        if self.reached {
            // Collapse on a peak-relative reverse, not down to the threshold.
            self.peak_pull = self.peak_pull.max(pull);
            let floor = threshold * RESET_REACHED_ON;
            let reset_at = self.peak_pull - (self.peak_pull - floor) / REVERSE_EASE;
            if pull < reset_at {
                self.reached = false;
                self.peak_pull = 0.0;
                self.start_expand(false, now);
            }
        } else if pull >= threshold {
            // Arm the dwell with the fingers down; a flick lifts them before it fires.
            if self.dwell_until.is_none() && movement == ScrollMovement::Progress {
                self.dwell_until = Some(now + READY_DWELL_SECS);
            }
        } else if pull < threshold * RESET_REACHED_ON {
            self.dwell_until = None;
        }
        self.sync_overlays(now, cx);
        if movement == ScrollMovement::Progress {
            return false;
        }
        self.dwell_until = None;
        if !self.committed {
            self.committed = true;
            if self.reached && cx.has_next && active {
                self.pulling = false;
                return true;
            }
        }
        if pull <= 0.0 {
            self.finish(now);
        }
        false
    }

    /// `reset(anim::type::normal)`: the indicator goes at once, the hint fades out.
    fn finish(&mut self, now: f64) {
        self.dwell_until = None;
        self.pulling = false;
        self.holding = false;
        self.committed = false;
        self.reached = false;
        self.peak_pull = 0.0;
        self.pull = 0.0;
        self.expand_to = false;
        self.expand = None;
        self.bounce_start = None;
        self.toggle_hint(false, now);
    }

    /// `PullToNextChannel::startExpand`.
    fn start_expand(&mut self, ready: bool, now: f64) {
        let from = Anim::value(self.expand, now, flag(self.expand_to));
        self.expand_to = ready;
        self.expand = Some(Anim {
            start: now,
            from,
            to: flag(ready),
            duration: self.duration(EXPAND_DURATION_SECS),
            ease: if ready {
                Transition::EaseOutQuint
            } else {
                Transition::SineInOut
            },
        });
    }

    /// `PullToNextChannel::pushIndicator`: `Indicator::setProgress` and `HintOverlay::setData`.
    fn sync_overlays(&mut self, now: f64, cx: PullContext) {
        if self.shown_ready != self.reached {
            let from = Anim::value(self.release, now, flag(self.shown_ready));
            self.shown_ready = self.reached;
            self.release = Some(Anim {
                start: now,
                from,
                to: flag(self.reached),
                duration: self.duration(RELEASE_DURATION_SECS),
                ease: if self.reached {
                    Transition::EaseOutQuint
                } else {
                    Transition::SineInOut
                },
            });
            if self.reached && !self.reduced_motion {
                self.bounce_start = Some(now);
            }
        }
        self.toggle_hint(self.holding && self.pull > 0.0 && cx.has_next, now);
    }

    /// `HintOverlay::toggle`.
    fn toggle_hint(&mut self, visible: bool, now: f64) {
        if self.hint_visible == visible {
            return;
        }
        let from = Anim::value(self.hint_panel, now, flag(self.hint_visible));
        self.hint_visible = visible;
        self.hint_panel = Some(Anim {
            start: now,
            from,
            to: flag(visible),
            duration: self.duration(PANEL_DURATION_SECS),
            ease: Transition::EaseOutQuint,
        });
    }

    /// Height the indicator is drawn for (`_effective`): the stretch, plus the room the card
    /// opens once the pull is armed.
    fn effective(&self, now: f64) -> f32 {
        let card = st::history_pull_next_expand.0 as f32;
        let expand = Anim::value(self.expand, now, flag(self.expand_to)) as f32;
        (self.pull + expand * (card - pull_threshold_px()))
            .min(st::history_pull_next_max_height.0 as f32)
    }

    /// How far the messages are lifted above the bottom of the list: the stretch plus the room
    /// the armed card opens (`ElasticScroll` real overscroll + `setContentBottomInset`).
    #[must_use]
    pub fn content_shift(&self, now_secs: f64) -> f32 {
        let offset = self.effective(now_secs);
        if offset > st::line_width.0 as f32 {
            offset
        } else {
            0.0
        }
    }

    fn animating(&self, now: f64) -> bool {
        self.edge.spring.is_some()
            || Anim::running(self.expand, now)
            || Anim::running(self.release, now)
            || Anim::running(self.hint_panel, now)
            || self
                .bounce_start
                .is_some_and(|at| now < at + BOUNCE_DURATION_SECS)
    }

    /// Reads this frame's scroll input over `list_rect` and feeds the gesture.
    fn read_input(&mut self, ui: &Ui, list_rect: Rect, now: f64, cx: PullContext) -> bool {
        let over_list = ui.ctx().input(|i| {
            i.pointer
                .latest_pos()
                .is_some_and(|pos| list_rect.contains(pos))
        });
        let mut commit = false;
        let wheel: Vec<(WheelPhase, f32)> = ui.ctx().input(|i| {
            let mut gesture = self.wheel_gesture;
            let out: Vec<(WheelPhase, f32)> = i
                .events
                .iter()
                .filter_map(|event| {
                    let Event::MouseWheel {
                        unit,
                        delta,
                        phase,
                        source,
                        ..
                    } = event
                    else {
                        return None;
                    };
                    let down = -delta.y
                        * match unit {
                            MouseWheelUnit::Point => 1.0,
                            MouseWheelUnit::Line => WHEEL_LINE_POINTS,
                            MouseWheelUnit::Page => list_rect.height(),
                        };
                    let ended = matches!(phase, TouchPhase::End | TouchPhase::Cancel);
                    let wheel_phase = if *source == MouseWheelSource::Momentum {
                        if ended {
                            WheelPhase::End
                        } else {
                            WheelPhase::Momentum
                        }
                    } else if *phase == TouchPhase::Start {
                        gesture = true;
                        WheelPhase::Begin
                    } else if ended {
                        gesture = false;
                        WheelPhase::End
                    } else if gesture {
                        WheelPhase::Update
                    } else {
                        WheelPhase::Unphased
                    };
                    Some((wheel_phase, down))
                })
                .collect();
            self.wheel_gesture = gesture;
            out
        });
        if over_list || self.pulling {
            for (phase, down) in wheel {
                commit |= self.feed_scroll(phase, down, false, now, cx);
            }
        }

        // A finger on a touch screen drags the list; past the bottom it stretches it.
        let (touching, drag_up) = ui.ctx().input(|i| {
            (
                i.any_touches() && i.pointer.primary_down(),
                -i.pointer.delta().y,
            )
        });
        if touching {
            if !self.touch_gesture && over_list {
                self.touch_gesture = true;
                commit |= self.feed_scroll(WheelPhase::Begin, 0.0, true, now, cx);
            } else if self.touch_gesture && drag_up != 0.0 {
                commit |= self.feed_scroll(WheelPhase::Update, drag_up, true, now, cx);
            }
        } else if self.touch_gesture {
            self.touch_gesture = false;
            commit |= self.feed_scroll(WheelPhase::End, 0.0, true, now, cx);
        }
        commit
    }

    /// Runs the gesture for this frame and paints the indicator over the bottom of `list_rect`.
    ///
    /// Returns the channel to switch to when the user lifts the fingers from an armed pull.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        list_rect: Rect,
        at_bottom: bool,
        next_channel: Option<&NextUnreadChannelHint>,
        colors: &HistoryColors,
        reduced_motion: bool,
    ) -> Option<PeerId> {
        let now = ui.ctx().input(|i| i.time);
        self.reduced_motion = reduced_motion;
        let cx = PullContext {
            active: at_bottom,
            has_next: next_channel.is_some_and(|next| next.unread_count > 0),
        };
        if !self.preview {
            let mut commit = self.read_input(ui, list_rect, now, cx);
            commit |= self.advance(now, cx);
            if commit {
                self.reset();
                return next_channel.map(|next| next.peer_id);
            }
        }
        if self.animating(now) {
            ui.ctx().request_repaint();
        } else if let Some(at) = self.dwell_until {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64((at - now).max(0.0)));
        }
        let offset = self.effective(now);
        if offset > st::line_width.0 as f32 {
            self.paint_indicator(ui, list_rect, offset, next_channel, colors, now);
        }
        None
    }

    /// `PullToNextChannel::Indicator::paintEvent`.
    fn paint_indicator(
        &self,
        ui: &Ui,
        list_rect: Rect,
        offset: f32,
        next_channel: Option<&NextUnreadChannelHint>,
        colors: &HistoryColors,
        now: f64,
    ) {
        let card = st::history_pull_next_expand.0 as f32;
        let skip = st::history_pull_next_skip.0 as f32;
        let padding = st::history_pull_next_padding.0 as f32;
        let avatar = st::history_pull_next_avatar.0 as f32;
        let inset = st::history_pull_next_inset.0 as f32;
        let rise = st::history_pull_next_rise.0 as f32;
        let arrow_size = st::history_pull_next_arrow_size.0 as f32;
        let max_height = st::history_pull_next_max_height.0 as f32;

        // The indicator widget covers the lowest `historyPullNextMaxHeight` of the list.
        let h = list_rect.bottom();
        let area = Rect::from_min_max(
            Pos2::new(list_rect.left(), (h - max_height).max(list_rect.top())),
            list_rect.right_bottom(),
        );
        let painter = ui.painter().with_clip_rect(area.intersect(ui.clip_rect()));
        let center_width = list_rect.width() - HISTORY_SCROLL_WIDTH;
        let cx = list_rect.left() + center_width / 2.0;

        let progress = (offset / card).clamp(0.0, 1.0);
        let alpha = progress;
        let release = Anim::value(self.release, now, flag(self.shown_ready)) as f32;
        let bounce = self.bounce_start.map_or(1.0, |at| {
            ((now - at) / BOUNCE_DURATION_SECS).clamp(0.0, 1.0)
        });
        let bounce_offset =
            -(bounce_progress(bounce) as f32) * st::history_pull_next_bounce.0 as f32;
        let circle_radius = avatar / 2.0;
        let bg = colors.service_bg;
        let fg = colors.service_fg;

        let width_radius =
            (circle_radius * (offset - skip) / (card - skip)).clamp(0.0, circle_radius);
        let width_radius2 = (circle_radius * progress)
            .min(offset / 2.0 - skip * progress)
            .max(0.0);
        let size = (width_radius2 * 2.0 - inset) * (1.0 - release) + avatar * release;

        if release < 1.0 {
            let capsule_top = h - offset;
            let capsule_bottom = h - skip * (1.0 - release) + (avatar - offset) * release;
            let mut rect = Rect::from_min_max(
                Pos2::new(cx - width_radius, capsule_top),
                Pos2::new(cx + width_radius, capsule_bottom),
            );
            let mut bg_alpha = alpha;
            if release > 0.0 {
                rect = rect.shrink(inset * release);
                bg_alpha = alpha * (1.0 - release);
            }
            if rect.width() > 0.0 && rect.height() > 0.0 {
                let radius = width_radius
                    .min(rect.width() / 2.0)
                    .min(rect.height() / 2.0);
                painter.rect_filled(
                    rect,
                    CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8),
                    bg.gamma_multiply(bg_alpha),
                );
                let arrow_cy = h - offset + arrow_size + skip * (1.0 - progress) - rise * release;
                paint_arrow(
                    &painter.with_clip_rect(rect.intersect(painter.clip_rect())),
                    Pos2::new(cx, arrow_cy),
                    progress,
                    fg.gamma_multiply(alpha * (1.0 - release)),
                );
            }
        }

        let name = next_channel.map_or_else(tr::lng_pull_no_unread_channels, |c| c.title.clone());
        if release > 0.0 && !name.is_empty() {
            let avail = (center_width - 4.0 * skip).max(1.0);
            let text_color = fg.gamma_multiply(alpha * release);
            let mut job = egui::text::LayoutJob::single_section(
                name,
                egui::TextFormat {
                    font_id: FontId::new(13.0, family_semibold()),
                    color: text_color,
                    ..Default::default()
                },
            );
            job.wrap = egui::text::TextWrapping {
                max_width: avail,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = painter.layout_job(job);
            let name_w = galley.size().x;
            let name_h = galley.size().y;
            let y = h + st::history_pull_next_name_top.0 as f32 * (1.0 - release) - rise * release
                + bounce_offset;
            let left = list_rect.left() + (center_width - name_w) / 2.0;
            let pill = Rect::from_min_size(
                Pos2::new(left - skip, y - padding),
                Vec2::new(name_w + 2.0 * skip, name_h + skip),
            );
            painter.rect_filled(
                pill,
                CornerRadius::same(st::history_pull_next_name_radius.0.clamp(0, 255) as u8),
                bg.gamma_multiply(alpha * release),
            );
            painter.galley(Pos2::new(left, y), galley, text_color);
        }

        if size >= 1.0 {
            let top = h
                + (-skip - skip * progress - size) * (1.0 - release)
                + (-offset.min(card) + padding) * release
                + bounce_offset;
            let av_rect = Rect::from_min_size(Pos2::new(cx - size / 2.0, top), Vec2::splat(size));
            if let Some(next) = next_channel {
                let mut faded = painter.clone();
                faded.set_opacity(alpha);
                let ui_colors = colors.ui_colors();
                let userpic =
                    EmptyUserpic::for_color_index(next.color_index, &next.title, &ui_colors);
                if size >= MIN_SIZE_WITH_INITIALS {
                    userpic.paint_circle_or_photo(&faded, av_rect, next.peer_id.0);
                } else if !tg_ui::userpic_images::paint_userpic_image(
                    &faded,
                    av_rect,
                    next.peer_id.0,
                ) {
                    // tdesktop scales the whole userpic; its initials are unreadable this small.
                    let bg = userpic.bg_colors();
                    tg_ui::userpic::paint_vertical_gradient_circle(
                        &faded, av_rect, bg.color1, bg.color2,
                    );
                }
                if release > 0.05 && next.unread_count > 0 {
                    paint_badge(&faded, av_rect, next.unread_count, release, colors);
                }
            } else {
                painter.circle_filled(av_rect.center(), size / 2.0, bg.gamma_multiply(alpha));
                paint_check(
                    &painter,
                    av_rect.center(),
                    size / 2.0,
                    fg.gamma_multiply(alpha),
                );
            }
        }
    }

    /// Paints the hint over the bottom bar (`PullToNextChannel::HintOverlay::paintEvent`):
    /// "pull up" while the fingers hold the stretch, "release" once the pull is armed.
    ///
    /// `rect` is everything below the list (the area of the bottom bar). The hint takes no
    /// clicks.
    pub fn paint_hint(&self, ui: &mut Ui, rect: Rect, colors: &HistoryColors) {
        let now = ui.ctx().input(|i| i.time);
        let panel = Anim::value(self.hint_panel, now, flag(self.hint_visible)) as f32;
        if panel <= 0.0 || !rect.is_positive() {
            return;
        }
        if Anim::running(self.hint_panel, now) || Anim::running(self.release, now) {
            ui.ctx().request_repaint();
        }
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        painter.rect_filled(
            rect,
            CornerRadius::ZERO,
            colors.bar_bg.gamma_multiply(panel),
        );

        let release = Anim::value(self.release, now, flag(self.shown_ready)) as f32;
        let slide = st::history_pull_next_hint_slide.0 as f32;
        let font = FontId::new(14.0, family_regular());
        let pull_text = tr::lng_pull_next_channel();
        let release_text = tr::lng_release_next_channel();
        let label = if release > 0.5 {
            release_text.clone()
        } else {
            pull_text.clone()
        };
        ui.interact(
            rect,
            Id::unique("tg_hist_pull_next_channel_hint"),
            Sense::hover(),
        )
        .widget_info(|| WidgetInfo::labeled(Role::Label, true, label.clone()));

        let line = |text: String, shift: f32, opacity: f32| {
            let color = colors.sub_text_fg.gamma_multiply(opacity);
            let galley = painter.layout_no_wrap(text, font.clone(), color);
            let pos = Pos2::new(
                rect.center().x - galley.size().x / 2.0,
                rect.center().y - galley.size().y / 2.0 + shift,
            );
            painter.galley(pos, galley, color);
        };
        if release < 1.0 {
            line(pull_text, -slide * release, panel * (1.0 - release));
        }
        if release > 0.0 {
            line(release_text, slide * (1.0 - release), panel * release);
        }
    }
}

/// `PaintArrow`: the up arrow in the capsule, scaled by `progress` around its anchor.
fn paint_arrow(painter: &egui::Painter, center: Pos2, progress: f32, fg: Color32) {
    if progress <= 0.0 {
        return;
    }
    let raise = st::history_pull_next_arrow_raise.0 as f32;
    let map = |x: f32, y: f32| {
        Pos2::new(
            center.x + progress * (x - 12.0),
            center.y - raise + progress * (y + 8.0),
        )
    };
    let stroke = Stroke::new(st::history_pull_next_arrow_stroke.0 as f32 * progress, fg);
    painter.line_segment([map(12.5, 4.0), map(12.5, 22.0)], stroke);
    painter.add(Shape::line(
        vec![map(3.5, 12.0), map(12.5, 3.5), map(21.5, 12.0)],
        stroke,
    ));
}

/// `PaintCheck`: the check mark shown when no channel has unread messages.
fn paint_check(painter: &egui::Painter, center: Pos2, size: f32, fg: Color32) {
    if size <= 0.0 {
        return;
    }
    let s = size / 2.4;
    painter.add(Shape::line(
        vec![
            Pos2::new(center.x - s, center.y),
            Pos2::new(center.x - s / 3.0, center.y + s * 0.7),
            Pos2::new(center.x + s, center.y - s * 0.7),
        ],
        Stroke::new(st::history_pull_next_arrow_stroke.0 as f32, fg),
    ));
}

/// The unread badge at the top right of the next channel's userpic; it grows with `release`.
fn paint_badge(
    painter: &egui::Painter,
    av_rect: Rect,
    count: u32,
    release: f32,
    colors: &HistoryColors,
) {
    let height = st::history_pull_next_badge.0 as f32;
    let text = if count > 999 {
        "999+".to_owned()
    } else {
        count.to_string()
    };
    let galley = painter.layout_no_wrap(
        text,
        FontId::new(12.0 * release, family_semibold()),
        Color32::WHITE,
    );
    let text_size = galley.size() / release.max(0.01);
    let width = (text_size.x + height - text_size.y).max(height);
    let full = Rect::from_min_size(
        Pos2::new(
            av_rect.right() - width + height / 3.0,
            av_rect.top() - height / 4.0,
        ),
        Vec2::new(width, height),
    );
    let badge = Rect::from_center_size(full.center(), full.size() * release);
    painter.rect_filled(
        badge,
        CornerRadius::same((badge.height() / 2.0).round().clamp(0.0, 255.0) as u8),
        colors.accent,
    );
    painter.galley(badge.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: PullContext = PullContext {
        active: true,
        has_next: true,
    };
    const FRAME: f64 = 1.0 / 60.0;

    /// A phased gesture at the bottom of the list: `travel` px of input in 10 px steps, one
    /// step per frame. Returns the time after the last step.
    fn drag(pull: &mut PullToNextChannelWidget, travel: f32, start: f64) -> f64 {
        let mut now = start;
        assert!(!pull.feed_scroll(WheelPhase::Begin, 0.0, false, now, LIVE));
        let mut sent = 0.0;
        while sent < travel {
            now += FRAME;
            assert!(!pull.feed_scroll(WheelPhase::Update, 10.0, false, now, LIVE));
            assert!(!pull.advance(now, LIVE));
            sent += 10.0;
        }
        now
    }

    fn hold(pull: &mut PullToNextChannelWidget, from: f64, secs: f64) -> f64 {
        let mut now = from;
        while now < from + secs {
            now += FRAME;
            assert!(!pull.advance(now, LIVE));
        }
        now
    }

    #[test]
    fn bounce_progress_matches_tdesktop_endpoints_and_overshoot() {
        assert!((bounce_progress(0.0) - 0.0).abs() < 1e-6);
        assert!((bounce_progress(0.45) - 1.0).abs() < 1e-6);
        assert!((bounce_progress(0.75) - (-0.5)).abs() < 1e-6);
        assert!((bounce_progress(1.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn the_threshold_is_96_px_of_stretch_which_takes_240_px_of_travel() {
        assert_eq!(pull_threshold_px(), 96.0);
        let mut pull = PullToNextChannelWidget::new();
        let now = drag(&mut pull, 230.0, 1.0);
        assert!((pull.pull_px() - 92.0).abs() < 0.01, "{}", pull.pull_px());
        pull.feed_scroll(WheelPhase::Update, 10.0, false, now + FRAME, LIVE);
        assert!((pull.pull_px() - 96.0).abs() < 0.01, "{}", pull.pull_px());
        // Past the pull distance the edge is twenty times stiffer.
        pull.feed_scroll(WheelPhase::Update, 100.0, false, now + 2.0 * FRAME, LIVE);
        assert!((pull.pull_px() - 101.0).abs() < 0.01, "{}", pull.pull_px());
    }

    #[test]
    fn a_classic_mouse_wheel_never_pulls_however_far_it_scrolls() {
        let mut pull = PullToNextChannelWidget::new();
        let mut now = 1.0;
        for _ in 0..400 {
            now += FRAME;
            assert!(!pull.feed_scroll(WheelPhase::Unphased, 120.0, false, now, LIVE));
            assert!(!pull.advance(now, LIVE));
            assert_eq!(pull.pull_px(), 0.0);
        }
        assert!(!pull.is_ready());
    }

    #[test]
    fn inertia_that_reaches_the_bottom_never_pulls() {
        let mut pull = PullToNextChannelWidget::new();
        let mut now = 1.0;
        // The fingers lifted while the list was still scrolling; the inertia arrives at the
        // bottom and keeps coming.
        for _ in 0..200 {
            now += FRAME;
            assert!(!pull.feed_scroll(WheelPhase::Momentum, 60.0, false, now, LIVE));
            assert!(!pull.advance(now, LIVE));
            assert_eq!(pull.pull_px(), 0.0);
        }
        assert!(!pull.feed_scroll(WheelPhase::End, 0.0, false, now + FRAME, LIVE));
        assert!(!pull.is_ready());
    }

    #[test]
    fn a_pull_past_the_threshold_held_and_released_switches() {
        let mut pull = PullToNextChannelWidget::new();
        let now = drag(&mut pull, 300.0, 1.0);
        assert!(pull.pull_px() >= pull_threshold_px());
        assert!(!pull.is_ready(), "the pull arms only after the dwell");
        let now = hold(&mut pull, now, READY_DWELL_SECS + 2.0 * FRAME);
        assert!(pull.is_ready());
        assert!(
            pull.feed_scroll(WheelPhase::End, 0.0, false, now + FRAME, LIVE),
            "lifting the fingers from an armed pull switches the channel"
        );
    }

    #[test]
    fn a_flick_past_the_threshold_does_not_switch() {
        let mut pull = PullToNextChannelWidget::new();
        let mut now = 1.0;
        pull.feed_scroll(WheelPhase::Begin, 0.0, false, now, LIVE);
        // 300 px of travel in three frames, then the fingers lift 50 ms later.
        for _ in 0..3 {
            now += FRAME;
            assert!(!pull.feed_scroll(WheelPhase::Update, 100.0, false, now, LIVE));
        }
        assert!(pull.pull_px() >= pull_threshold_px());
        now += 0.050;
        assert!(!pull.advance(now, LIVE));
        assert!(!pull.feed_scroll(WheelPhase::End, 0.0, false, now, LIVE));
        // The stretch springs back to rest and nothing switches on the way.
        let end = now + MAX_RETURN_SECS + 0.1;
        while now < end {
            now += FRAME;
            assert!(!pull.advance(now, LIVE));
        }
        assert_eq!(pull.pull_px(), 0.0);
        assert!(!pull.is_ready());
    }

    #[test]
    fn a_pull_short_of_the_threshold_does_not_switch_however_long_it_is_held() {
        let mut pull = PullToNextChannelWidget::new();
        let now = drag(&mut pull, 200.0, 1.0);
        assert!(pull.pull_px() < pull_threshold_px());
        let now = hold(&mut pull, now, 2.0);
        assert!(!pull.is_ready());
        assert!(!pull.feed_scroll(WheelPhase::End, 0.0, false, now + FRAME, LIVE));
    }

    #[test]
    fn pulling_back_before_the_release_disarms_the_pull() {
        let mut pull = PullToNextChannelWidget::new();
        let now = drag(&mut pull, 300.0, 1.0);
        let mut now = hold(&mut pull, now, READY_DWELL_SECS + 2.0 * FRAME);
        assert!(pull.is_ready());
        // Scrolling back consumes the stretch 1:1.
        for _ in 0..4 {
            now += FRAME;
            assert!(!pull.feed_scroll(WheelPhase::Update, -10.0, false, now, LIVE));
        }
        assert!(!pull.is_ready(), "stretch {}", pull.pull_px());
        assert!(!pull.feed_scroll(WheelPhase::End, 0.0, false, now + FRAME, LIVE));
    }

    #[test]
    fn an_armed_pull_with_no_unread_channel_left_does_not_switch() {
        let none = PullContext {
            active: true,
            has_next: false,
        };
        let mut pull = PullToNextChannelWidget::new();
        let mut now = 1.0;
        pull.feed_scroll(WheelPhase::Begin, 0.0, false, now, none);
        for _ in 0..30 {
            now += FRAME;
            pull.feed_scroll(WheelPhase::Update, 10.0, false, now, none);
        }
        for _ in 0..12 {
            now += FRAME;
            assert!(!pull.advance(now, none));
        }
        assert!(pull.is_ready());
        assert!(!pull.feed_scroll(WheelPhase::End, 0.0, false, now + FRAME, none));
    }

    #[test]
    fn nothing_pulls_away_from_the_bottom_or_with_the_setting_off() {
        let away = PullContext {
            active: false,
            has_next: true,
        };
        let mut pull = PullToNextChannelWidget::new();
        pull.feed_scroll(WheelPhase::Begin, 0.0, false, 1.0, away);
        pull.feed_scroll(WheelPhase::Update, 300.0, false, 1.1, away);
        assert_eq!(pull.pull_px(), 0.0);

        let mut off = PullToNextChannelWidget::new();
        off.set_enabled(false);
        off.feed_scroll(WheelPhase::Begin, 0.0, false, 1.0, LIVE);
        off.feed_scroll(WheelPhase::Update, 300.0, false, 1.1, LIVE);
        assert_eq!(off.pull_px(), 0.0);
    }

    #[test]
    fn a_finger_on_a_touch_screen_travels_half_as_far() {
        let mut pull = PullToNextChannelWidget::new();
        pull.feed_scroll(WheelPhase::Begin, 0.0, true, 1.0, LIVE);
        pull.feed_scroll(WheelPhase::Update, 120.0, true, 1.0 + FRAME, LIVE);
        assert!((pull.pull_px() - 96.0).abs() < 0.01, "{}", pull.pull_px());
    }
}
