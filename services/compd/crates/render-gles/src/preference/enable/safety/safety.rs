//! Typed enablement values for the Law-7 safety-net crates: the second half of
//! the double gate (cargo feature + this value). All off by default — disabled
//! means structurally absent from the hot path.

#[derive(Debug, Clone, Copy, Default)]
pub struct SafetyEnable {
    /// scanout.timing/timing.throttle — re-time early vblanks from buggy drivers.
    pub vblank_throttle: bool,
    /// scanout.timing/timing.predict — next-presentation-time estimation.
    pub presentation_predict: bool,
    /// scanout.flip/flip.estimate — estimated-vblank callback pacing on empty damage.
    pub estimate_pacing: bool,
    /// scanout.framebuffer/framebuffer.modifier — modifier filtering/downgrade.
    pub modifier_fallback: bool,
    /// drm.mode/mode.synthesize — CVT/modeline synthesis of non-advertised modes.
    pub mode_synthesize: bool,
}

pub fn get() -> SafetyEnable {
    SafetyEnable::default()
}
