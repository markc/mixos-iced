//! The renderer-neutral effects seam: how an optional
//! effects producer (Bevy, or anything else) feeds the scene without the engine
//! naming it. One trait, one draw-node payload, one null implementation.
pub mod host;

pub use host::{EffectBand, EffectElement, EffectFrame, EffectHost, NullEffects, EFFECTS, EFFECTS_MUT};
