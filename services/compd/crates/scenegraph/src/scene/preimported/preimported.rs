use smithay::backend::renderer::element::{
    Element as SmithayElement, Id, Kind, RenderElement,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{Frame, Renderer, RendererSuper};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

/// A texture already imported into the active renderer `R`, with on-screen
/// placement. Used on renderers that consume the iced/bevy/parallax output via
/// dmabuf import (their GLES-welded elements stay on the SceneDispatch seam).
/// Mirrors `IcedRenderElement`'s geometry, but generic over `R::TextureId` and
/// drawn with the plain `Frame::render_texture_from_to`.
pub struct PreImported<R: Renderer> {
    pub texture: R::TextureId,
    /// On-screen physical pixel position.
    pub location: Point<i32, Physical>,
    /// Natural texture size in physical pixels.
    pub size: Size<i32, Physical>,
    /// Destination zoom (1.0 for screen-space; camera zoom for world-space).
    pub world_zoom: f64,
    pub id: Id,
    pub commit: CommitCounter,
}

impl<R: Renderer> PreImported<R> {
    fn dest_size(&self) -> Size<i32, Physical> {
        if (self.world_zoom - 1.0).abs() < f64::EPSILON {
            self.size
        } else {
            Size::from((
                (self.size.w as f64 * self.world_zoom) as i32,
                (self.size.h as f64 * self.world_zoom) as i32,
            ))
        }
    }
}

impl<R: Renderer> SmithayElement for PreImported<R> {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_loc_and_size((0.0, 0.0), (self.size.w as f64, self.size.h as f64))
    }
    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::from_loc_and_size(self.location, self.dest_size())
    }
    fn location(&self, _scale: Scale<f64>) -> Point<i32, Physical> {
        self.location
    }
    fn transform(&self) -> Transform {
        Transform::Normal
    }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit != Some(self.commit) {
            vec![Rectangle::from_size(self.geometry(scale).size)].into_iter().collect()
        } else {
            DamageSet::default()
        }
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        1.0
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: Renderer> RenderElement<R> for PreImported<R> {
    fn draw(
        &self,
        frame: &mut <R as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), <R as RendererSuper>::Error> {
        Frame::render_texture_from_to(
            frame,
            &self.texture,
            src,
            dst,
            damage,
            &[],
            Transform::Normal,
            1.0,
        )
    }
}
