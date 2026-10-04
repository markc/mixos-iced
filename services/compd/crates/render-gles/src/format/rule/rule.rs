//! The rules every format decision obeys, stated once.
//!
//! Modifier classification and ranking (moved verbatim from the former
//! `bridge.negotiate/negotiate.classify`), plus the single vocabulary for what an
//! empty answer MEANS.
//!
//! # Why the outcome vocabulary is here and not at the call sites
//!
//! Before this crate the tree had four mutually incompatible answers to "the
//! negotiation came back empty": the scanout narrowing aborted the process at
//! startup, the producer paths returned an empty list that turned fatal later at
//! the allocator, the client advertisement logged an error and advertised nothing
//! (silently dropping every client to shm), and the overview blur alone degraded
//! gracefully. Four call sites, four policies, no shared word for the condition —
//! so each was one edit from diverging further. [`Outcome`] is that word.

use smithay::backend::allocator::Modifier;

/// Coarse class of a DRM modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModClass {
    /// `DRM_FORMAT_MOD_LINEAR` (0).
    Linear,
    /// `DRM_FORMAT_MOD_INVALID` (implicit / driver-negotiated).
    ///
    /// NOT a member of the lattice: it names "ask the driver", so it cannot be
    /// meaningfully intersected with anything. It may be carried through where a
    /// set is unpublished, and must be dropped before a set reaches KMS.
    Invalid,
    /// A vendor tiled modifier, no compression metadata.
    Tiled,
    /// A tiled modifier carrying compression: AMD DCC (`AMD_FMT_MOD_DCC` bit) or any
    /// Intel CCS layout (aux-plane RC/MC/CC on gen12-MTL, flat CCS on Xe2). Every one
    /// of these is readable only by a consumer that explicitly lists it, so it must
    /// never survive an intersection by being mistaken for plain tiling.
    TiledCompressed,
}

const VENDOR_INTEL: u64 = 0x01; // DRM_FORMAT_MOD_VENDOR_INTEL
const VENDOR_AMD: u64 = 0x02; // DRM_FORMAT_MOD_VENDOR_AMD
const AMD_FMT_MOD_DCC_SHIFT: u64 = 13;

/// Intel compressed modifiers, as the low 56 bits of `fourcc_mod_code(INTEL, n)`
/// (`drm_fourcc.h`): Y/Yf CCS (4, 5), gen12 RC/MC/RC-CC (6, 7, 8), DG2 RC/MC/RC-CC
/// (10, 11, 12), MTL RC/MC/RC-CC (13, 14, 15), BMG and LNL flat CCS (16, 17).
/// X (1), Y (2), Yf (3) and 4-tiled (9) are plain tiling.
const INTEL_CCS: &[u64] = &[4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 16, 17];

fn vendor(m: u64) -> u64 {
    (m >> 56) & 0xff
}

/// Classify a modifier. Compression covers AMD DCC (a bit in the AMD_FMT_MOD
/// encoding) and every Intel CCS layout (an enumerated code, because Intel keeps
/// the metadata in an extra plane or in flat CCS rather than in a vendor bit).
///
/// History: this used to recognise AMD DCC only, so Intel CCS classified as plain
/// [`ModClass::Tiled`]. That is the class of mistake behind an Intel dmabuf
/// defect seen on 2026-10-03: a background worker allocated
/// `4_TILED_MTL_RC_CCS_CC` (`0x010000000000000f`) for a buffer its Vulkan importer
/// never listed, and the import failed every frame. The registrar guard makes
/// a missing consumer term fatal; this makes compression visible in every log and
/// to `FORCE_MULTIPLANE`, which used to empty the candidate list on Intel.
pub fn classify(m: Modifier) -> ModClass {
    match m {
        Modifier::Linear => ModClass::Linear,
        Modifier::Invalid => ModClass::Invalid,
        _ => {
            let v: u64 = m.into();
            let amd_dcc = vendor(v) == VENDOR_AMD && (v >> AMD_FMT_MOD_DCC_SHIFT) & 1 == 1;
            let intel_ccs = vendor(v) == VENDOR_INTEL && INTEL_CCS.contains(&(v & 0x00ff_ffff_ffff_ffff));
            if amd_dcc || intel_ccs {
                ModClass::TiledCompressed
            } else {
                ModClass::Tiled
            }
        }
    }
}

/// A tiled (non-linear, non-invalid) modifier.
pub fn is_tiled(m: Modifier) -> bool {
    matches!(classify(m), ModClass::Tiled | ModClass::TiledCompressed)
}

/// A compressed modifier: AMD DCC or Intel CCS (see [`ModClass::TiledCompressed`]).
pub fn is_compressed(m: Modifier) -> bool {
    matches!(classify(m), ModClass::TiledCompressed)
}

/// Selection rank, best first: tiled > linear > invalid.
pub fn rank(m: Modifier) -> u8 {
    match classify(m) {
        ModClass::Tiled | ModClass::TiledCompressed => 3,
        ModClass::Linear => 2,
        ModClass::Invalid => 1,
    }
}

/// Short human label for the developer tool.
pub fn label(c: ModClass) -> &'static str {
    match c {
        ModClass::Linear => "linear",
        ModClass::Invalid => "invalid",
        ModClass::Tiled => "tiled",
        ModClass::TiledCompressed => "tiled+compressed",
    }
}

/// One modifier, as `class:hex` — `linear`, `invalid`, `tiled:0x300000000606014`.
///
/// The class alone is ambiguous (a device reports six distinct tiled modifiers)
/// and the raw u64 alone is unreadable, so every log that names a modifier names
/// both, the same way everywhere.
pub fn describe(m: Modifier) -> String {
    match classify(m) {
        ModClass::Linear => "linear".into(),
        ModClass::Invalid => "invalid".into(),
        c => format!("{}:{:#x}", label(c), u64::from(m)),
    }
}

/// A modifier list, best-first order preserved. `-` when empty, so an empty list
/// is visibly empty rather than a blank at the end of a line.
pub fn describe_all(mods: &[Modifier]) -> String {
    if mods.is_empty() {
        return "-".into();
    }
    mods.iter().map(|m| describe(*m)).collect::<Vec<_>>().join(" ")
}

/// "No modifier is known here." Used by diagnostic paths that must print or seed
/// a modifier before one has been chosen — NOT a claim that a buffer is implicit.
pub const UNKNOWN: Modifier = Modifier::Invalid;

/// Whether a modifier survives the Law-7 legacy filter: `LINEAR` and the
/// driver-negotiated implicit modifier only, the documented safe set for hardware
/// classes whose compression modifiers fail under load.
pub fn legacy(m: Modifier) -> bool {
    matches!(m, Modifier::Linear | Modifier::Invalid)
}

/// What an answer from this layer MEANS when it is not a plain success.
///
/// The distinction that matters is [`Self::Degraded`] versus [`Self::Refused`]:
/// degraded means a constraint was DROPPED because nothing had published it yet,
/// so the answer is wider than it should be and the caller may proceed; refused
/// means the constraints were all present and genuinely share nothing, so there
/// is no safe buffer to allocate and proceeding would produce the unimportable
/// modifier this layer exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every term was published and the result is non-empty.
    Ok,
    /// A term was unpublished and dropped. The answer is wider than the truth.
    Degraded(&'static str),
    /// Every term was present and they intersect in nothing.
    Refused(&'static str),
}

impl Outcome {
    /// Whether a caller may allocate on this answer.
    pub fn usable(self) -> bool {
        !matches!(self, Self::Refused(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(v: u64) -> Modifier {
        Modifier::from(v)
    }

    #[test]
    fn intel_ccs_is_compressed() {
        // MTL RC CCS with clear colour: the modifier a background worker chose
        // for a buffer its Vulkan importer could not read (2026-10-03).
        assert_eq!(classify(m(0x0100_0000_0000_000f)), ModClass::TiledCompressed);
        for code in [4u64, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 16, 17] {
            let v = (VENDOR_INTEL << 56) | code;
            assert!(is_compressed(m(v)), "Intel modifier {v:#x} must classify as compressed");
            assert!(is_tiled(m(v)), "a compressed modifier is still tiled: {v:#x}");
        }
    }

    #[test]
    fn intel_plain_tiling_is_not_compressed() {
        for code in [1u64, 2, 3, 9] {
            let v = (VENDOR_INTEL << 56) | code;
            assert_eq!(classify(m(v)), ModClass::Tiled, "Intel modifier {v:#x} is plain tiling");
        }
    }

    #[test]
    fn amd_dcc_still_compressed_and_linear_invalid_unchanged() {
        let amd_dcc = (VENDOR_AMD << 56) | (1 << AMD_FMT_MOD_DCC_SHIFT);
        assert_eq!(classify(m(amd_dcc)), ModClass::TiledCompressed);
        assert_eq!(classify(m(VENDOR_AMD << 56)), ModClass::Tiled);
        assert_eq!(classify(Modifier::Linear), ModClass::Linear);
        assert_eq!(classify(Modifier::Invalid), ModClass::Invalid);
        assert_eq!(describe(m(0x0100_0000_0000_000f)), "tiled+compressed:0x10000000000000f");
    }
}
