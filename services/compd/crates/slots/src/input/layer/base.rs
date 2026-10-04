/// Input priority layer. Higher layers receive input first, regardless of
/// registration order; within a layer, registration order wins ("first
/// registered takes input first").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InputLayer(pub u16);

/// Screen-anchored overlays (e.g. the iced screen overlay) always see input
/// first, no matter when they were spawned.
pub const OVERLAY: InputLayer = InputLayer(3000);

/// Screen-anchored UI below overlays (launcher-style surfaces).
pub const SCREEN: InputLayer = InputLayer(2000);

/// The world's content: canvas, windows, selection.
pub const WORLD: InputLayer = InputLayer(1000);

/// Catch-alls that only act when nothing else claimed the event.
pub const FALLBACK: InputLayer = InputLayer(0);
