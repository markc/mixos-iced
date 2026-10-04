use smithay::desktop::{Space, Window};

/// A world's window-hosting capability — the spatial world implements it over
/// the `Space<Window>` it owns. Neutral by design: smithay handlers reach it via `WireTrait`,
/// orchestration thin-routes to `worlds.spawn_host()`, and systems read the
/// space as a driverdata token slice — none of which makes this crate (or
/// `support.smithay`) depend on orchestration.
///
/// The space-mutating ops (`map`, `raise`, `element_under`, …) are added as
/// methods here as call sites migrate off direct `Space` access; for now it
/// exposes the owned space so the host is the single point of access.
pub trait WindowHost {
    /// The window space this world hosts.
    fn space(&self) -> &Space<Window>;
    /// Mutable access for mapping/restacking/geometry updates.
    fn space_mut(&mut self) -> &mut Space<Window>;
}
