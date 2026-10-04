use smithay::backend::renderer::gles::{
    GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, UniformValue,
};
use smithay::backend::renderer::{Frame, Renderer, RendererSuper};
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle, Size};
use std::sync::Arc;

/// Renderer-agnostic uniform values for the parallax background shader. The GLES
/// path ignores these (it uses named `Uniform`s + its compiled `GlesPixelProgram`);
/// a renderer with a native background shader (Vulkan) drives its pipeline from them.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParallaxUniforms {
    pub resolution: [f32; 2],
    pub zoom: f32,
    pub time: f32,
    pub pan: [f32; 2],
    pub flow_offset: [f32; 2],
    /// Smoothed pan velocity (world px/s). Fed to native shaders as two f16
    /// halves packed into the `lock_alpha.w` push lane (`lock_alpha.z` carries
    /// the sRGB flag) so velocity-reactive backgrounds (metaballs) can stretch
    /// along motion. Shaders that ignore it are unaffected.
    pub velocity: [f32; 2],
    pub lock_amount: f32,
    pub alpha: f32,
    pub srgb: f32, // push lock_alpha.z: 0 = raw output, 1 = gamma-encode to sRGB
}

/// One compiled fullscreen-shader variant a renderer can run: a SPIR-V module
/// plus the push-constant payload for this frame. Renderer-agnostic and
/// shader-agnostic — a renderer that owns a native fullscreen pipeline (Vulkan)
/// builds/caches a pipeline keyed by `id` and draws it with `push`; renderers
/// without that path ignore it. The producing scene element owns the shader
/// bytes and the push layout, so no shader-specific knowledge leaks into the
/// renderer.
///
/// The module bytes are `Arc`, not `Cow`: this seam is crossed on every draw
/// call of every frame, but the renderer only reads the bytes once, when its
/// pipeline cache misses. `Arc` lets both built-in (`'static`) and
/// runtime-compiled shaders flow through the same seam while making the
/// per-frame hand-off a refcount bump rather than a deep copy of the blob.
/// `push` is an `Arc` too — it is ~112 bytes and differs per draw, but every
/// consumer took ownership of it anyway, and the lifetime a `Cow` needed is what
/// kept this seam off `'static`.
#[derive(Clone)]
pub struct ShaderVariant {
    /// Stable per-shader id, used as the renderer's pipeline-cache key.
    pub id: u64,
    /// SPIR-V module bytes. Holds both entry points unless `vert_spv` is set.
    pub spv: Arc<[u8]>,
    /// Separate vertex-stage SPIR-V module (set when the fragment was compiled
    /// alone, e.g. a `glsl/` bundle paired with a fullscreen vertex).
    pub vert_spv: Option<Arc<[u8]>>,
    pub vert_entry: Arc<str>,
    pub frag_entry: Arc<str>,
    /// Push-constant bytes for this draw (already packed by the producer).
    ///
    /// `Arc`, not `Cow`. The borrow saved nothing — every consumer called
    /// `into_owned()` on it immediately — while the lifetime it introduced spread
    /// through `PipelinePass` and `ShaderPipeline` and kept the whole seam off
    /// `'static`, which is what an opaque handle needs.
    pub push: Arc<[u8]>,
}

/// A renderer-native fullscreen-shader draw handed through the dispatch seam:
/// the standard (SDR) variant plus an optional variant the renderer selects
/// when compositing for HDR output. `pipeline`, when set, is a multipass graph
/// the renderer runs INSTEAD of the single `sdr` pass (Vulkan only; renderers
/// without a graph executor ignore it and use `sdr`).
#[derive(Clone)]
pub struct NativeShaderPass {
    pub sdr: ShaderVariant,
    pub hdr: Option<ShaderVariant>,
    /// A multipass bundle for the renderer to run INSTEAD of `sdr`, as an OPAQUE
    /// handle.
    ///
    /// This crate describes how the scene hands a renderer a shader pass; it does
    /// not know what a multipass pipeline is, and it used to declare seven types
    /// that existed for nothing else. They live in `pipeline.abi/abi.seam` now,
    /// and travel through here without being named — which is what lets the
    /// pipeline grow what it carries without touching orchestration at all.
    ///
    /// `'static` by construction: the seam types hold `Arc`s, so a renderer that
    /// understands the handle downcasts it and one that does not ignores it.
    pub pipeline: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
}

/// Per-renderer draw seam for scene elements that carry GLES-produced resources
/// (iced UI, bevy 3D, parallax pixel shader).
///
/// The trait is on the **renderer** `R` (a plain bound — `R: SceneDispatch`),
/// not on `R::Frame`, to avoid the GAT higher-ranked-lifetime limitation
/// (rust#100013) that a `for<'a,'b> R::Frame<'a,'b>: Trait` bound runs into at
/// the use site. The methods take the frame as a parameter.
///
/// - `GlesRenderer` implements it for real (renders the texture / runs the pixel

/// GLES body for `SceneDispatch::draw_prerendered_texture` (delegated from the
/// trait impl in dispatch.frame, which the orphan rule pins to the trait crate).
pub fn draw_prerendered_texture(
    frame: &mut GlesFrame<'_, '_>,
    texture: &GlesTexture,
    src: Rectangle<f64, BufferCoord>,
    dst: Rectangle<i32, Physical>,
    damage: &[Rectangle<i32, Physical>],
    alpha: f32,
) -> Result<(), <GlesRenderer as RendererSuper>::Error> {
    Frame::render_texture_from_to(
        frame, texture, src, dst, damage, &[], smithay::utils::Transform::Normal, alpha,
    )
}

/// GLES body for `SceneDispatch::draw_pixel_program`.
#[allow(clippy::too_many_arguments)]
pub fn draw_pixel_program(
    frame: &mut GlesFrame<'_, '_>,
    program: Option<&GlesPixelProgram>,
    src: Rectangle<f64, BufferCoord>,
    dst: Rectangle<i32, Physical>,
    size: Size<i32, BufferCoord>,
    damage: &[Rectangle<i32, Physical>],
    alpha: f32,
    uniforms: &[Uniform<'_>],
) -> Result<(), <GlesRenderer as RendererSuper>::Error> {
    // The program is `None` when the element was built while the compositor
    // preferred dmabuf/Vulkan (the GLES pixel program is skipped then). If the
    // GLES path is nonetheless reached — e.g. a runtime Vulkan→GLES fallback flips
    // the render path after the element was prepared — skip the parallax this frame
    // instead of crashing; the next prepare() rebuilds it with a compiled program.
    let Some(program) = program else {
        return Ok(());
    };
    frame.render_pixel_shader_to(program, src, dst, size, Some(damage), alpha, uniforms)
}

/// The texture programs a renderer compiled (`SceneDispatch::tex_program`),
/// by key, in its EGL context's user data: a new renderer or context compiles
/// afresh, and a failed compile is remembered (as `None`) so it is tried and
/// logged once.
#[derive(Default)]
struct TexPrograms(std::cell::RefCell<std::collections::HashMap<&'static str, Option<GlesTexProgram>>>);

/// GLES body for `SceneDispatch::tex_program`.
pub fn tex_program(
    renderer: &mut GlesRenderer,
    key: &'static str,
    source: &'static str,
    uniforms: &[UniformName<'static>],
) -> Option<GlesTexProgram> {
    let cached = renderer
        .egl_context()
        .user_data()
        .get::<TexPrograms>()
        .and_then(|programs| programs.0.borrow().get(key).cloned());
    if let Some(program) = cached {
        return program;
    }
    // The seam owns `u_frag_to_phys` (set by `push_tex_program`), so it is
    // declared here, not by each caller; a program compiled without it made the
    // push fail with UnknownUniform at draw time.
    let mut names: Vec<UniformName<'static>> = uniforms.to_vec();
    names.push(UniformName::new("u_frag_to_phys", UniformType::Matrix3x3));
    let compiled = match renderer.compile_custom_texture_shader(source, &names) {
        Ok(program) => Some(program),
        Err(err) => {
            warn!("texture program {key} did not compile ({err:?}); drawing with the default program");
            None
        }
    };
    let data = renderer.egl_context().user_data();
    data.insert_if_missing(TexPrograms::default);
    if let Some(programs) = data.get::<TexPrograms>() {
        programs.0.borrow_mut().insert(key, compiled.clone());
    }
    compiled
}

/// GLES body for `SceneDispatch::push_tex_program`: `program` overrides the
/// frame's default texture program, with `uniforms` plus `u_frag_to_phys`.
pub fn push_tex_program(frame: &mut GlesFrame<'_, '_>, program: Option<&GlesTexProgram>, mut uniforms: Vec<Uniform<'static>>) {
    let Some(program) = program else {
        return;
    };
    let projection = *frame.projection();
    let mut viewport = [0i32; 4];
    let _ = frame.with_context(|gl| unsafe {
        gl.GetIntegerv(smithay::backend::renderer::gles::ffi::VIEWPORT, viewport.as_mut_ptr())
    });
    let matrix = frag_to_phys(projection, viewport);
    let Some(matrix) = matrix else {
        // A degenerate viewport would place every fragment wrongly: draw
        // without the program rather than with a garbage mapping. Logged once.
        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            warn!("tex program push: degenerate viewport {viewport:?}; drawing without the program");
        }
        return;
    };
    uniforms.push(Uniform::new(
        "u_frag_to_phys",
        UniformValue::Matrix3x3 { matrices: vec![matrix], transpose: false },
    ));
    frame.override_default_tex_program(program.clone(), uniforms);
}

/// The column-major 3x3 that takes `gl_FragCoord.xy` (window coordinates of
/// `viewport`) to the frame's output physical pixels: viewport to NDC, then
/// the inverse of the frame's `projection` (output physical px to NDC, the
/// output transform included). `None` for a degenerate viewport/projection.
pub fn frag_to_phys(projection: [f32; 9], viewport: [i32; 4]) -> Option<[f32; 9]> {
    let (vx, vy, vw, vh) = (viewport[0] as f32, viewport[1] as f32, viewport[2] as f32, viewport[3] as f32);
    if vw <= 0.0 || vh <= 0.0 {
        return None;
    }
    // Viewport (window px) -> NDC, column-major.
    let v = [2.0 / vw, 0.0, 0.0, 0.0, 2.0 / vh, 0.0, -1.0 - 2.0 * vx / vw, -1.0 - 2.0 * vy / vh, 1.0];
    Some(mul3(invert3(projection)?, v))
}

fn invert3(m: [f32; 9]) -> Option<[f32; 9]> {
    // Column-major: m[col * 3 + row].
    let a = |r: usize, c: usize| m[c * 3 + r];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0))
        + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = |r: usize, c: usize| -> f32 {
        // Cofactor of (c, r) over det (the adjugate is the transposed cofactors).
        let (r0, r1) = match c { 0 => (1, 2), 1 => (0, 2), _ => (0, 1) };
        let (c0, c1) = match r { 0 => (1, 2), 1 => (0, 2), _ => (0, 1) };
        let minor = a(r0, c0) * a(r1, c1) - a(r0, c1) * a(r1, c0);
        let sign = if (r + c) % 2 == 0 { 1.0 } else { -1.0 };
        sign * minor / det
    };
    let mut out = [0.0; 9];
    for c in 0..3 {
        for r in 0..3 {
            out[c * 3 + r] = inv(r, c);
        }
    }
    Some(out)
}

fn mul3(x: [f32; 9], y: [f32; 9]) -> [f32; 9] {
    let mut out = [0.0; 9];
    for c in 0..3 {
        for r in 0..3 {
            out[c * 3 + r] = (0..3).map(|k| x[k * 3 + r] * y[c * 3 + k]).sum();
        }
    }
    out
}

#[cfg(test)]
mod frag_to_phys_tests {
    use super::*;

    fn apply(m: [f32; 9], p: (f32, f32)) -> (f32, f32) {
        (m[0] * p.0 + m[3] * p.1 + m[6], m[1] * p.0 + m[4] * p.1 + m[7])
    }

    /// An untransformed 200x100 output: smithay's projection maps physical
    /// (x, y) to NDC with y flipped (top-left origin), the viewport is the full
    /// target; a fragment at window (x, y) is physical (x, 100 - y).
    #[test]
    fn a_normal_projection_flips_window_y_back_to_physical() {
        // NDC = (2x/200 - 1, 1 - 2y/100), column-major.
        let projection = [2.0 / 200.0, 0.0, 0.0, 0.0, -2.0 / 100.0, 0.0, -1.0, 1.0, 1.0];
        let m = frag_to_phys(projection, [0, 0, 200, 100]).unwrap();
        let (x, y) = apply(m, (10.5, 20.5));
        assert!((x - 10.5).abs() < 1e-3 && (y - 79.5).abs() < 1e-3, "{x} {y}");
    }

    #[test]
    fn inverse_times_matrix_is_identity() {
        let m = [2.0, 0.5, 0.0, -1.0, 3.0, 0.0, 4.0, 5.0, 1.0];
        let p = mul3(invert3(m).unwrap(), m);
        for (i, v) in p.iter().enumerate() {
            let want = if i % 4 == 0 { 1.0 } else { 0.0 };
            assert!((v - want).abs() < 1e-5, "{p:?}");
        }
        assert_eq!(frag_to_phys(m, [0, 0, 0, 10]), None);
    }
}
