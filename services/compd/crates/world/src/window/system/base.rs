use slots::storage::token::base::{Token, TokenMut};
use slots::trait_::system::base::{System, WorldBuilder};
use crate::window::lifecycle::state::lifecycle::WindowLifecycle;

pub static WINDOW_LIFECYCLE: Token<WindowLifecycle> = Token::new();
/// TRANSITIONAL pub: the wire glue and lifecycle interface still write the
/// incoming queue directly until they become events.
pub static WINDOW_LIFECYCLE_MUT: TokenMut<WindowLifecycle> = TokenMut::new(&WINDOW_LIFECYCLE);

/// Owns the window-lifecycle slot (incoming map/destroy/fullscreen events).
#[derive(Default)]
pub struct WindowSystem;

impl System for WindowSystem {
    fn name(&self) -> &'static str {
        "window"
    }

    fn register(&mut self, builder: &mut WorldBuilder) {
        builder.storage.insert(&WINDOW_LIFECYCLE, WindowLifecycle::new());
    }
}
