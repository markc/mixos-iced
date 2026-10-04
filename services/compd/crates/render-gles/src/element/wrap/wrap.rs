//! Wraps any RenderElement<GlesRenderer> so it can be drawn through a
//! MultiRenderer whose primary backend is GlesRenderer. (Moved verbatim from
//! draw.scene/element_wrap.rs; the renderer alias now comes from
//! `gles.multigpu/multigpu.factory`.)

use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::multigpu::{gbm::GbmGlesBackend, MultiRenderer};
use smithay::backend::renderer::utils::{DamageSet, OpaqueRegions};
use smithay::backend::renderer::{
    element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
    gles::{GlesError, GlesFrame, GlesRenderer},
    utils::CommitCounter,
    RendererSuper,
};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};

/// Wraps any RenderElement<GlesRenderer> so it can be drawn through a
/// MultiRenderer whose primary backend is GlesRenderer.
pub struct GlesElementWrapper<E>(pub E)
where
    E: RenderElement<GlesRenderer>;

impl<E> Element for GlesElementWrapper<E>
where
    E: RenderElement<GlesRenderer>,
{
    fn id(&self) -> &Id {
        self.0.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.0.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.0.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.0.src()
    }

    fn transform(&self) -> Transform {
        self.0.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.0.geometry(scale)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.0.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.0.opaque_regions(_scale)
    }

    fn alpha(&self) -> f32 {
        self.0.alpha()
    }

    fn kind(&self) -> Kind {
        self.0.kind()
    }

    fn is_framebuffer_effect(&self) -> bool {
        self.0.is_framebuffer_effect()
    }
}

type UdevRenderer<'a> = MultiRenderer<
    'a,
    'a,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
>;

impl<'a, E> RenderElement<UdevRenderer<'a>> for GlesElementWrapper<E>
where
    E: RenderElement<GlesRenderer>,
{
    fn draw(
        &self,
        frame: &mut <UdevRenderer<'a> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), <UdevRenderer<'a> as RendererSuper>::Error> {
        let gles_frame: &mut GlesFrame<'_, '_> = frame.as_mut();
        self.0
            .draw(gles_frame, src, dst, damage, opaque_regions, cache)
            .map_err(|e: GlesError| e.into())
    }

    fn underlying_storage(&self, renderer: &mut UdevRenderer<'a>) -> Option<UnderlyingStorage<'_>> {
        // Borrow the inner gles renderer to ask the wrapped element.
        let gles: &mut GlesRenderer = renderer.as_mut();
        self.0.underlying_storage(gles)
    }

    fn capture_framebuffer(
        &self,
        frame: &mut <UdevRenderer<'a> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), <UdevRenderer<'a> as RendererSuper>::Error> {
        let gles_frame: &mut GlesFrame<'_, '_> = frame.as_mut();
        self.0
            .capture_framebuffer(gles_frame, src, dst, cache)
            .map_err(|e: GlesError| e.into())
    }
}
