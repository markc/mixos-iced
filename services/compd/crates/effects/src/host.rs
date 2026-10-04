//! The effects interface.
//!
//! Wiring an effects renderer (Bevy) into the scene directly would make every
//! renderer and the scene builder depend on it. Here the
//! dependency is inverted: the scene asks ONE host, held in kernel storage
//! under [`EFFECTS`], for elements, and an effects crate implements
//! [`EffectHost`]. The engine never names the producer.
//!
//! The payload is renderer-neutral: a dmabuf plus its placement, imported by
//! whichever renderer composes (GLES today) at the draw node's `lower()`. A
//! producer renders off the compositor's GL context into its own buffers, the
//! same contract the off-thread iced worker keeps.
//!
//! [`NullEffects`] is installed by default (the loader), so a build with no
//! effects crate pays one virtual call per output frame and draws nothing.

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::utils::{Physical, Point, Size};

use slots::storage::token::base::{Token, TokenMut};

/// The one effects host, in KERNEL storage (driver data, not per world).
pub static EFFECTS: Token<Box<dyn EffectHost>> = Token::new();
pub static EFFECTS_MUT: TokenMut<Box<dyn EffectHost>> = TokenMut::new(&EFFECTS);

/// What the scene knows about the output frame it is asking for.
#[derive(Clone, Debug)]
pub struct EffectFrame {
    /// The output's stable key (`state::output_key`), so a host can keep per-output state.
    pub output: std::sync::Arc<str>,
    /// The output's size in physical pixels.
    pub size: Size<i32, Physical>,
    /// The output's fractional scale.
    pub scale: f64,
}

/// Where an effect element is drawn relative to the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectBand {
    /// Behind every window: the `WORLD_3D` band, above the desktop fill and the
    /// layer-shell background/bottom surfaces.
    Background,
    /// Above windows (with layer-shell `top`), below the compositor's own screen
    /// UI and the pointer.
    Overlay,
}

/// One drawable an effect produced: a dmabuf and where it goes. Renderer-neutral;
/// the draw node imports it into the composing renderer at `lower()`.
#[derive(Clone, Debug)]
pub struct EffectElement {
    pub dmabuf: Dmabuf,
    pub location: Point<i32, Physical>,
    pub size: Size<i32, Physical>,
    /// Stable per element across frames, for damage tracking.
    pub id: Id,
    /// Bumped by the producer whenever the buffer's content changes.
    pub commit: CommitCounter,
}

/// An effects producer. Called on the compositor thread, once per output frame.
pub trait EffectHost {
    /// Short name for logs.
    fn name(&self) -> &'static str;
    /// Advance state for this output frame, before the scene is built.
    fn prepare(&mut self, frame: &EffectFrame);
    /// The elements to draw on this output this frame, with their band.
    fn produce(&mut self, frame: &EffectFrame) -> Vec<(EffectBand, EffectElement)>;
    /// Whether the host needs another frame soon (it is animating).
    fn wants_frame(&self) -> bool;
}

/// The default host: no effects.
#[derive(Default)]
pub struct NullEffects;

impl EffectHost for NullEffects {
    fn name(&self) -> &'static str {
        "null"
    }

    fn prepare(&mut self, _frame: &EffectFrame) {}

    fn produce(&mut self, _frame: &EffectFrame) -> Vec<(EffectBand, EffectElement)> {
        Vec::new()
    }

    fn wants_frame(&self) -> bool {
        false
    }
}
