use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::{ImportAll, ImportMem, Texture};
use smithay::utils::{Physical, Point, Rectangle, Size};
use crate::scene::identity::identity::SolidBank;
use crate::canvas::draw::context::context::Context;
use crate::state::{Loop, Transform};
use crate::state::state::CoordinateTrait;

thread_local! {
    /// One canvas cursor exists, so it gets one identity that outlives the frame.
    /// A fresh `Id::new()` per frame reads to smithay's damage tracker as the old
    /// box vanishing and a new one appearing — and, on the DRM path, changes the
    /// element id occupying a hardware plane every frame. See `scene.identity`.
    static CURSOR: SolidBank = SolidBank::default();
}

/// The canvas cursor: a translucent box in the world band, drawn on the pane the
/// physical cursor is over (the caller gates that).
pub fn scene<R>(
    state: &mut Loop,
    _renderer: &mut R,
    _size: Size<i32, Physical>,
    context: &Context,
) -> Vec<SolidColorRenderElement>
where
    R: smithay::backend::renderer::Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    /// Edge length in world-logical units. A point extracted as a `Rectangle` is
    /// zero-sized, so the rect is built with a size rather than from a position.
    const CURSOR_SIZE: f64 = 20.0;

    // No shader pipeline, so nothing displaces the box; the corrected world
    // point is where it is drawn.
    let viewport = state.viewport_context();
    let at = context.cursor.position;
    let rect: Transform = (
        (at.x - CURSOR_SIZE / 2.0, at.y - CURSOR_SIZE / 2.0, CURSOR_SIZE, CURSOR_SIZE),
        viewport,
    )
        .into();

    vec![CURSOR.with(|bank| {
        bank.solid(
            0,
            Rectangle::<i32, Physical>::from(rect),
            [137.0 / 255.0, 250.0 / 255.0, 222.0 / 255.0, 0.5],
        )
    })]
}
