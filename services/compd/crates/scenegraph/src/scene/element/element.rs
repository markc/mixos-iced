use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::{ImportAll, ImportMem};
use dispatcher::frame::frame::SceneDispatch;
use world::seat::pointer::element::element::PointerRenderElement;
use ui::IcedRenderElement;

pub use crate::scene::preimported::preimported::PreImported;

smithay::render_elements! {
    pub SceneElement<R> where R: ImportAll + ImportMem + SceneDispatch;
    Canvas = world::canvas::draw::element::element::Element<R>,
    Layershell = WaylandSurfaceRenderElement<R>,
    Surface = IcedRenderElement,
    /// World-space iced surface clipped to a viewport pane (GLES path).
    SurfaceCropped = smithay::backend::renderer::element::utils::CropRenderElement<IcedRenderElement>,
    /// World-space iced surface (dmabuf-imported) clipped to a pane (native path).
    TextureCropped = smithay::backend::renderer::element::utils::CropRenderElement<PreImported<R>>,
    Pointer = PointerRenderElement<R>,
    Texture = PreImported<R>,
    Sentinel = SolidColorRenderElement,
}

impl<R> SceneElement<R>
where
    R: ImportAll + ImportMem + SceneDispatch,
{
    /// A cursor representation: the pointer sprite and drag icon (`Pointer`),
    /// or the canvas cursor, the translucent box drawn in the world band at
    /// the same point (`world::canvas::cursor`, the only `SolidBox` a canvas
    /// element is). What a screencopy with `overlay_cursor = 0` leaves out; the
    /// one place that classifies it, so every backend leaves out the same set.
    pub fn is_cursor(&self) -> bool {
        matches!(
            self,
            SceneElement::Pointer(_)
                | SceneElement::Canvas(world::canvas::draw::element::element::Element::SolidBox(_))
        )
    }
}
