//! The client corner clip (phase 2): a decorated window's content drawn through
//! a texture program that cuts it to the frame's rounded rectangle, so the
//! window is rounded all the way round, not only along the titlebar.
//!
//! One element type, [`Clipped`], wraps each of the toplevel's content
//! elements (and its subsurfaces; popups are not clipped). Its
//! draw sets the frame's default texture program to [`SHADER`] for the inner
//! element's own texture draw and restores it after: the clip is part of the
//! content's draw, so it costs no extra frame and no extra pass. The program is
//! reached through the renderer seam (`SceneDispatch::tex_program` /
//! `push_tex_program` / `pop_tex_program`); a renderer without one draws the
//! content unclipped (the square-bottom look), never nothing.
//!
//! The shader locates each fragment in OUTPUT PHYSICAL pixels through
//! `u_frag_to_phys`, which the GLES seam derives from the frame's own
//! projection and viewport, so the clip is right on every target the frame
//! draws into (the nested window's flipped surface, a KMS buffer, an offscreen
//! screencopy texture).

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesTexProgram, Uniform, UniformName, UniformType};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};

use dispatcher::frame::frame::SceneDispatch;

/// The cache key the renderer seam compiles [`SHADER`] under.
pub const KEY: &str = "decor:rounded-clip";

/// The program's own uniforms (the seam adds `u_frag_to_phys`).
pub const UNIFORMS: &[(&str, UniformType)] = &[
    ("u_clip", UniformType::_4f),
    ("u_radius", UniformType::_1f),
    ("u_debug", UniformType::_1f),
];

/// The rounded-rect clip as a smithay custom texture shader (the default
/// texture shader, plus coverage of a rounded rectangle in output physical
/// pixels).
pub const SHADER: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform mat3 u_frag_to_phys;
uniform vec4 u_clip;
uniform float u_radius;
// Diagnosis only (COMPD_CHROME_CLIP_DEBUG=1): the content is replaced by its
// own placement: red = p.x mod 256, green = p.y mod 256, blue = coverage.
uniform float u_debug;

float rounded_box(vec2 p, vec4 r, float radius) {
    vec2 half_size = r.zw * 0.5;
    vec2 q = p - r.xy - half_size;
    float rad = min(radius, min(half_size.x, half_size.y));
    vec2 d = abs(q) - half_size + vec2(rad);
    return length(max(d, 0.0)) + min(max(d.x, d.y), 0.0) - rad;
}

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    vec2 p = (u_frag_to_phys * vec3(gl_FragCoord.xy, 1.0)).xy;
    float coverage = clamp(0.5 - rounded_box(p, u_clip, u_radius), 0.0, 1.0);
    color = color * coverage;
    if (u_debug > 0.5)
        color = vec4(fract(p.x / 256.0), fract(p.y / 256.0), coverage, 1.0);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

/// `COMPD_CHROME_CLIP_DEBUG=1`: draw the clipped content as its own placement
/// (see the shader), for diagnosing the clip on a live output. Read once.
pub fn debug() -> bool {
    use std::sync::OnceLock;
    static DEBUG: OnceLock<bool> = OnceLock::new();
    *DEBUG.get_or_init(|| std::env::var_os("COMPD_CHROME_CLIP_DEBUG").is_some_and(|v| v == "1"))
}

/// The rounded rectangle content is cut to: the decorated window's whole
/// frame (output physical px) and its corner radius (physical px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub rect: Rectangle<i32, Physical>,
    pub radius: f32,
}

impl Clip {
    fn uniforms(&self) -> Vec<Uniform<'static>> {
        vec![
            Uniform::new(
                "u_clip",
                (
                    self.rect.loc.x as f32,
                    self.rect.loc.y as f32,
                    self.rect.size.w as f32,
                    self.rect.size.h as f32,
                ),
            ),
            Uniform::new("u_radius", self.radius),
            Uniform::new("u_debug", if debug() { 1.0f32 } else { 0.0 }),
        ]
    }

    /// The four corner squares the clip may cut, in physical px.
    fn corners(&self) -> [Rectangle<i32, Physical>; 4] {
        let r = self.radius.ceil() as i32;
        let (x0, y0) = (self.rect.loc.x, self.rect.loc.y);
        let (x1, y1) = (x0 + self.rect.size.w - r, y0 + self.rect.size.h - r);
        [(x0, y0), (x1, y0), (x0, y1), (x1, y1)]
            .map(|(x, y)| Rectangle::new((x, y).into(), (r, r).into()))
    }
}

/// The program, compiled once per renderer by the seam (`None` on a renderer
/// without texture programs: content then draws unclipped).
pub fn program<R: SceneDispatch>(renderer: &mut R) -> Option<GlesTexProgram> {
    let names: Vec<UniformName<'static>> = UNIFORMS
        .iter()
        .map(|(name, kind)| UniformName::new(*name, *kind))
        .collect();
    renderer.tex_program(KEY, SHADER, &names)
}

/// A content element drawn through the rounded clip.
pub struct Clipped<E> {
    inner: E,
    program: Option<GlesTexProgram>,
    clip: Clip,
}

impl<E> Clipped<E> {
    pub fn new(inner: E, program: Option<GlesTexProgram>, clip: Clip) -> Self {
        Clipped {
            inner,
            program,
            clip,
        }
    }
}

impl<E: Element> Element for Clipped<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    /// The inner element's opaque region less the corners the clip cuts: what
    /// shows through a cut corner must still be drawn behind it.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let regions = self.inner.opaque_regions(scale);
        if self.program.is_none() || self.clip.radius <= 0.0 {
            return regions;
        }
        // Element-local, like the regions.
        let origin = self.inner.geometry(scale).loc;
        let corners = self
            .clip
            .corners()
            .map(|c| Rectangle::new(c.loc - origin, c.size));
        let kept = Rectangle::subtract_rects_many(regions.iter().copied(), corners);
        OpaqueRegions::from_slice(&kept)
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<R, E> RenderElement<R> for Clipped<E>
where
    R: Renderer + ImportAll + ImportMem + SceneDispatch,
    E: RenderElement<R>,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        if self.program.is_none() || self.clip.radius <= 0.0 {
            return self
                .inner
                .draw(frame, src, dst, damage, opaque_regions, cache);
        }
        R::push_tex_program(frame, self.program.as_ref(), self.clip.uniforms());
        let drawn = self
            .inner
            .draw(frame, src, dst, damage, opaque_regions, cache);
        R::pop_tex_program(frame);
        drawn
    }

    /// Never a plane: scanning the client buffer out directly would skip the
    /// clip.
    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_keeps_smithays_defines_marker() {
        assert!(SHADER.lines().any(|l| l.trim() == "//_DEFINES_"));
        assert!(
            !SHADER.contains("#version 3"),
            "smithay compiles texture shaders as GLSL ES 1.00"
        );
    }

    #[test]
    fn the_corner_squares_sit_in_the_corners() {
        let clip = Clip {
            rect: Rectangle::new((10, 20).into(), (100, 50).into()),
            radius: 7.5,
        };
        let [tl, tr, bl, br] = clip.corners();
        assert_eq!(tl, Rectangle::new((10, 20).into(), (8, 8).into()));
        assert_eq!(tr.loc, (102, 20).into());
        assert_eq!(bl.loc, (10, 62).into());
        assert_eq!(br.loc, (102, 62).into());
    }
}
