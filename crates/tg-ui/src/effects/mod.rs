//! Visual effects ported from `Telegram/lib_ui/ui/effects/` and
//! `Telegram/SourceFiles/ui/effects/`.

pub mod fly;
pub mod radial;
pub mod ripple;
pub mod spoiler;
pub mod thanos;

pub use fly::{
    EmojiFlyAnimation, FlyEffectPolicy, FlyEffectsManager, FlyParabola, MessageSendingTextFlight,
    MiniCopy, ReactionFlyAnimation, ReactionFlyArgs, ReactionFlyStage, TextFlightSample,
    compute_fly_parabola, cubic_bezier_ease, reveal_height, rise_progress,
};
pub use radial::{InfiniteRadialAnimation, RadialState, paint_arc, paint_static_loading};
pub use ripple::{RippleMask, RippleState, clamp_point_to_round_rect, update_and_paint_ripple};
pub use spoiler::paint_spoiler_particles;
pub use thanos::{
    ThanosEffect, ThanosItem, ThanosParticle, ThanosPolicy, animation_speed_multiplier,
    compute_particle_grid, disappear_progress,
};
