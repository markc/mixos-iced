//! `DrawNode` — the owned, renderer-agnostic draw currency. Scene contributors
//! (and, increasingly, systems' `draw()`) describe WHAT to draw and at WHICH
//! `Layer`; the single `lower()` seam turns a node into the renderer's
//! `SceneElement` at the backend boundary (importing dmabuf into a native
//! texture on renderers that prefer it, passthrough on GLES). This replaces the
//! old implicit push-order assembly: layering is now explicit and the
//! node→element lowering lives in exactly one place.

use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement};
use smithay::backend::renderer::element::{Element, Kind};
use smithay::utils::{Physical, Point, Scale};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::backend::renderer::{ImportAll, ImportDma, ImportMem, Renderer, Texture};
use dispatcher::frame::frame::SceneDispatch;
use crate::scene::element::element::{PreImported, SceneElement};
use slots::world::frame::base::Layer;

type Iced = ui::IcedRenderElement;

/// One unit of drawable content, generic over the active renderer `R`.
/// A renderer-agnostic wl_surface tree to draw: systems/contributors carry the
/// surface + placement, and the backend builds the `WaylandSurfaceRenderElement`s
/// at `lower()` time (one tree → many elements for subsurfaces). This is the
/// goal-(B) shape — no `<R>`, no smithay render element constructed by the
/// contributor.
pub struct SurfaceNode {
    pub surface: WlSurface,
    pub location: Point<i32, Physical>,
    pub scale: f64,
    pub alpha: f32,
}

pub enum DrawNode<R: Renderer> {
    /// Renderer-agnostic surface tree (lowered to layershell elements).
    Surface(SurfaceNode),
    Pointer(world::seat::pointer::element::element::PointerRenderElement<R>),
    Layershell(WaylandSurfaceRenderElement<R>),
    /// Canvas content: a window's surfaces and decorations, the canvas cursor.
    Canvas {
        elem: world::canvas::draw::element::element::Element<R>,
    },
    /// iced UI surface (world or screen); imported via dmabuf on native renderers.
    Iced(Iced),
    /// World iced surface clipped to a viewport pane's physical rect.
    IcedCropped {
        elem: Iced,
        crop: smithay::utils::Rectangle<i32, Physical>,
    },
    /// An effects-host element (`EffectHost::produce`), imported from its dmabuf
    /// into the composing renderer at `lower()`. Renderer-neutral: the node is
    /// not typed on any effects renderer.
    Effect(effects::EffectElement),
    /// A texture already imported into `R`.
    Texture(PreImported<R>),
    Solid(SolidColorRenderElement),
}

/// A layered collection of draw nodes. Contributors push at explicit `Layer`
/// bands (BACKGROUND..POINTER); `lower()` orders them topmost-first and turns
/// them into the renderer's `SceneElement` list.
pub struct Plan<R: Renderer> {
    nodes: Vec<(Layer, DrawNode<R>)>,
}

impl<R: Renderer> Default for Plan<R> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
}

impl<R> Plan<R>
where
    R: Renderer + ImportAll + ImportDma + ImportMem + SceneDispatch,
    R::TextureId: Texture + Clone + Send + 'static,
{
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, layer: Layer, node: DrawNode<R>) {
        self.nodes.push((layer, node));
    }

    pub fn extend<I: IntoIterator<Item = DrawNode<R>>>(&mut self, layer: Layer, nodes: I) {
        for node in nodes {
            self.nodes.push((layer, node));
        }
    }

    /// Order topmost-first (higher Layer drawn on top → emitted first, matching
    /// smithay's first-is-front element order) and lower each node. Nodes whose
    /// dmabuf import fails are dropped for this frame. Returns the elements plus a
    /// lockstep [`ElementMeta`] per element: its space (`World` for client
    /// windows + iced-world panels, so the renderer can restrict AA to world
    /// content) and whether it's a client `window` (only those feed the shader
    /// pipeline's window-rects/window-textures set).
    pub fn lower(
        mut self,
        renderer: &mut R,
    ) -> (Vec<SceneElement<R>>, Vec<dispatcher::frame::frame::ElementMeta>) {
        use dispatcher::frame::frame::ElementMeta;
        self.nodes.sort_by(|a, b| b.0.cmp(&a.0));
        let mut elements = Vec::new();
        let mut meta = Vec::new();
        for (_, node) in self.nodes {
            // World content is exactly windows + iced-world panels; everything
            // else (bevy, parallax, screen iced, layershell, pointer, solids) is
            // screen-space. Client windows are tagged WINDOW (a subset of world):
            // only they feed the shader pipeline's window-rects/window-textures
            // set. Iced-world panels/placeholders stay WORLD — AA-eligible, but
            // kept out of the window-set so glass/window-glow never treat a
            // placeholder as a window.
            let m = match &node {
                DrawNode::Canvas { .. } => ElementMeta::WINDOW,
                DrawNode::IcedCropped { .. } => ElementMeta::WORLD,
                _ => ElementMeta::SCREEN,
            };
            for e in node.lower(renderer) {
                elements.push(e);
                meta.push(m);
            }
        }
        (elements, meta)
    }
}

impl<R> DrawNode<R>
where
    R: Renderer + ImportAll + ImportDma + ImportMem + SceneDispatch,
    R::TextureId: Texture + Clone + Send + 'static,
{
    pub fn lower(self, renderer: &mut R) -> Vec<SceneElement<R>> {
        match self {
            DrawNode::Surface(n) => render_elements_from_surface_tree::<R, WaylandSurfaceRenderElement<R>>(
                renderer,
                &n.surface,
                n.location,
                Scale::from(n.scale),
                n.alpha,
                Kind::Unspecified,
            )
            .into_iter()
            .map(SceneElement::Layershell)
            .collect(),
            DrawNode::Pointer(e) => vec![SceneElement::Pointer(e)],
            DrawNode::Layershell(e) => vec![SceneElement::Layershell(e)],
            DrawNode::Canvas { elem, .. } => vec![SceneElement::Canvas(elem)],
            DrawNode::Texture(e) => vec![SceneElement::Texture(e)],
            DrawNode::Solid(e) => vec![SceneElement::Sentinel(e)],
            // An iced element is always a GL texture on the renderer's own
            // context (wgpu-GL); there is no dmabuf behind it.
            DrawNode::Iced(e) => vec![SceneElement::Surface(e)],
            DrawNode::IcedCropped { elem, crop } => {
                use smithay::backend::renderer::element::utils::CropRenderElement;
                // Geometry is physical and scale-independent, so the crop scale is
                // irrelevant.
                CropRenderElement::from_element(elem, Scale::from(1.0), crop)
                    .map(SceneElement::SurfaceCropped)
                    .into_iter()
                    .collect()
            }
            DrawNode::Effect(e) => {
                import_texture(renderer, &e.dmabuf, e.location, e.size, 1.0, e.id, e.commit).into_iter().collect()
            }
        }
    }
}

/// Import a dmabuf into a native `PreImported` texture (drops the node on failure).
#[allow(clippy::too_many_arguments)]
fn import_texture<R>(
    renderer: &mut R,
    dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf,
    location: smithay::utils::Point<i32, smithay::utils::Physical>,
    size: smithay::utils::Size<i32, smithay::utils::Physical>,
    world_zoom: f64,
    id: smithay::backend::renderer::element::Id,
    commit: smithay::backend::renderer::utils::CommitCounter,
) -> Option<SceneElement<R>>
where
    R: Renderer + ImportAll + ImportDma + ImportMem + SceneDispatch,
    R::TextureId: Texture + Clone + Send + 'static,
{
    match renderer.import_dmabuf(dmabuf, None) {
        Ok(texture) => Some(SceneElement::Texture(PreImported { texture, location, size, world_zoom, id, commit })),
        Err(err) => {
            error!("draw.node: dmabuf import into the active renderer failed: {err}");
            None
        }
    }
}
