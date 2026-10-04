//! The ONE fourcc vocabulary. Which formats this compositor is willing to use,
//! and how each is named in the other APIs it has to speak.
//!
//! Before this crate the tree had four disjoint answers to "which fourccs do we
//! deal in", hand-kept in lockstep with no compile-time link between them: the
//! exhaustive Vulkan table, the wgpu probe's five candidates (which omitted
//! `Argb2101010`), two byte-identical copies of a five-arm `Fourcc → gbm::Format`
//! map, and the scanout ladder's six. A fifth existed for `wl_shm` and had already
//! drifted — `Bgr888` was advertised to clients and could not be uploaded.
//!
//! The compositor is GLES only, so this crate names no Vulkan type, and with
//! wgpu on GL over the renderer's own context no wgpu type either: there is no
//! adapter probe and no `Fourcc -> wgpu::TextureFormat` pin. The extended-range
//! test is stated here.
//! What lives here is every CHOICE: which fourccs to offer, in what order, and to
//! whom.

use smithay::backend::allocator::Fourcc;

/// The scanout ladder, best first. smithay walks it and takes the first rung the
/// plane accepts.
///
/// The 8-bit tail keeps `Argb8888` first — that path works today and reordering it
/// would change a format every producer is currently pinned to. B-first leads at
/// 10 bits because that is the better-supported order across this tree: smithay's
/// GLES tables map only `Abgr2101010`, and the wgpu probe has no `TextureFormat`
/// for `Argb2101010` at all. NVIDIA's primary plane offers `AB30`/`XB30` and no
/// `AR30`/`XR30`, which is what made "R-first at 10-bit too" such a natural and
/// wrong inference.
pub fn scanout_ladder(ten_bit: bool) -> Vec<Fourcc> {
    if ten_bit {
        vec![
            Fourcc::Xbgr2101010,
            Fourcc::Abgr2101010,
            Fourcc::Xrgb2101010,
            Fourcc::Argb2101010,
            Fourcc::Argb8888,
            Fourcc::Abgr8888,
        ]
    } else {
        vec![Fourcc::Argb8888, Fourcc::Abgr8888]
    }
}

/// The fourccs a producer's buffer may be allocated with through gbm.
///
/// `gbm::Format` IS `drm_fourcc::DrmFourcc` — the same type as [`Fourcc`] — so the
/// two former copies of this map were an identity function wrapped around a
/// five-entry allow-list. It is expressed as the allow-list it always was, and
/// needs no gbm dependency to say so.
pub fn allocatable(fourcc: Fourcc) -> bool {
    matches!(
        fourcc,
        Fourcc::Argb8888
            | Fourcc::Xrgb8888
            | Fourcc::Abgr8888
            | Fourcc::Xbgr8888
            | Fourcc::Abgr2101010
    )
}

/// The background worker's candidate ladder, best first.
///
/// The worker owns both ends of its buffer, so WHICH fourccs are worth trying is
/// a vocabulary question that belongs here; whether this device can actually
/// render one stays with the worker, which has the `PhysicalDevice` to ask.
///
/// The session's own channel order leads when it names a deep rung: a plane may
/// expose 10-bit in one order only, so following the session beats guessing. The
/// 8-bit floor is always last.
pub fn background_ladder(session: Option<Fourcc>, deep: bool) -> Vec<Fourcc> {
    use smithay::backend::allocator::format::{get_transparent, has_alpha};
    const DEEP: [Fourcc; 2] = [Fourcc::Abgr2101010, Fourcc::Argb2101010];
    let with_alpha = |f: Fourcc| if has_alpha(f) { Some(f) } else { get_transparent(f) };
    let matching = session.and_then(with_alpha).filter(|f| DEEP.contains(f));
    let mut ladder = Vec::with_capacity(DEEP.len() + 1);
    if deep {
        ladder.extend(matching);
        ladder.extend(DEEP.iter().copied().filter(|f| Some(*f) != matching));
    }
    ladder.push(FLOOR);
    ladder
}

/// The 8-bit format every path falls back to. Named once so no consumer has to
/// spell it.
pub const FLOOR: Fourcc = Fourcc::Argb8888;

// There is no producer fourcc -> wgpu format pin: iced renders into
// renderer-allocated GL textures and capture wraps its EGLImage texture as
// `runtime.surface`'s fixed `FORMAT`.

/// Bits per colour channel. `0` for anything this compositor does not rank —
/// enough to order candidates, not a general property table.
pub fn depth(fourcc: Fourcc) -> u8 {
    match fourcc {
        Fourcc::Xrgb2101010 | Fourcc::Argb2101010 | Fourcc::Xbgr2101010 | Fourcc::Abgr2101010 => 10,
        Fourcc::Argb8888 | Fourcc::Xrgb8888 | Fourcc::Abgr8888 | Fourcc::Xbgr8888 => 8,
        _ => 0,
    }
}

/// More than 8 bits per channel.
pub fn is_deep(fourcc: Fourcc) -> bool {
    matches!(
        fourcc,
        Fourcc::Xrgb2101010 | Fourcc::Argb2101010 | Fourcc::Xbgr2101010 | Fourcc::Abgr2101010
    )
}

/// Whether the compositor can EXPRESS this format's colour, as opposed to merely
/// import its memory.
///
/// The float formats (fp16) carry linear-light, extended-range values. The SDR
/// composite — and the GLES renderer, which has no transfer handling at all —
/// passes sampled colour through as if it were sRGB-encoded, so a linear 0.21
/// mid-grey is emitted as encoded 0.21 and lands at ~0.03 of the intended light:
/// far too dark, with everything above 1.0 clipped flat. Importing that is worse
/// than declining it, because declining leaves the client on 8-bit sRGB, which IS
/// expressed exactly — right colour at 8 bits beats wrong colour at fp16.
///
/// A RENDERER-INDEPENDENT policy: not "can Vulkan sample this" (it can, exactly as
/// well as GLES) but "can this compositor say what it means". When the
/// colour-managed composite is running, it can, and these formats are offered.
///
/// `color_managed` is a PARAMETER rather than a lookup so this crate stays pure
/// vocabulary. It read the registrar directly once, which made the registrar
/// unable to consult the vocabulary in turn — a cycle, and the sign that a
/// dictionary had started holding state.
pub fn expressible(fourcc: Fourcc, color_managed: bool) -> bool {
    !extended_range(fourcc) || color_managed
}

/// The float (fp16) formats: linear-light, extended-range values rather than
/// sRGB-encoded ones. Stated here rather than read off a driver format table.
pub fn extended_range(fourcc: Fourcc) -> bool {
    matches!(fourcc, Fourcc::Abgr16161616f | Fourcc::Xbgr16161616f)
}
