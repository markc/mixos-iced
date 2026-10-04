use protocols::space::state::SpaceState;
use slots::storage::token::base::{Token, TokenMut};
use crate::host::window::base::WindowHost;
use smithay::desktop::{Space, Window};

/// The spatial world's window-host slice: it OWNS the `Space<Window>` and
/// implements [`WindowHost`]. Held in a spatial world's storage under [`SPACE`]
/// (seeded by the spatial-world builder). `SPACE_MUT` is `pub` because the
/// driver/smithay handlers are the writers of this slice — space mutation is
/// smithay plumbing, NOT the per-system buffer rule; systems only read it.
pub struct SpaceHost {
    pub inner: SpaceState,
}

impl SpaceHost {
    pub fn new(inner: SpaceState) -> Self {
        Self { inner }
    }
}

pub static SPACE: Token<SpaceHost> = Token::new();
pub static SPACE_MUT: TokenMut<SpaceHost> = TokenMut::new(&SPACE);

impl WindowHost for SpaceHost {
    fn space(&self) -> &Space<Window> {
        &self.inner.state
    }
    fn space_mut(&mut self) -> &mut Space<Window> {
        &mut self.inner.state
    }
}
