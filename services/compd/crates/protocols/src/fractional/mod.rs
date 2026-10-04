pub mod state {
    pub use crate::fractional::scale::{
        Fractional, FractionalScaleConfig, DebounceCycle, Published,
    };
    pub use crate::fractional::emit::{
        emit_to_surfaces, NestedCompositorSurface,
    };
}

pub mod config;
pub mod debounce;
pub mod emit;
pub mod scale;
