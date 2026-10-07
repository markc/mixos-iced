// SPDX-License-Identifier: MIT OR Apache-2.0
//! One process-wide registry for caller-supplied font collections and the
//! immutable selections made from them.
//!
//! [`FontRegistry`] is a bounded ledger in front of iced's shared font
//! system. A [`RegistrationBatch`] is validated and resolved entirely before
//! a single renderer transaction commits: source bytes are re-digested,
//! faces are parsed in scratch databases, every role and icon selection is
//! resolved (all eligible declared fallback groups in declaration order,
//! sealed weights, validated styles and intrinsic family claims), and all
//! process-wide capacities are checked with checked arithmetic. Only then
//! is one atomic [`iced_graphics::text::FontSystem::register_fonts`] call
//! made, and every error is produced before that call: a returned error
//! leaves the registry and the renderer exactly as they were. After the
//! seam commits, publication and receipt construction are infallible; the
//! only post-commit check is the seam's one-ID-per-staged-face contract,
//! which cannot fail by construction and is therefore asserted, never
//! returned as a recoverable error.
//!
//! Selections become private aliases (`mixos-pinned-<digest>`) in the
//! renderer: a selection's public family spelling never becomes the key iced
//! shapes with, so two collections whose fonts claim the same family names
//! can coexist and old paragraphs stay bound to their original bytes.
//! Two role keys that resolve to the exact same selection share one alias.
//!
//! Locking order: the registry lock is always taken before the shared font
//! system write lock; no reverse acquisition and no caller callbacks while
//! either is held. The production [`registry`] uses the process-wide iced
//! singleton; tests drive the same code through an isolated renderer owner
//! and injectable limits, without a public constructor that could bypass
//! the process caps.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fmt,
    sync::{Arc, Mutex, OnceLock},
};

use iced_core::{
    Font,
    font::{Family, Stretch, Style, Weight},
};
use iced_graphics::text::{
    cosmic_text::{
        self, FontRegistration, FontRegistrationError, FontRegistrationResult, PinnedFaceRef,
        PinnedFontPolicy, fontdb,
    },
    font_system,
};

/// One process-wide [`FontRegistry`], created on first use and never reset.
pub fn registry() -> &'static FontRegistry {
    static REGISTRY: OnceLock<FontRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| FontRegistry {
        state: Mutex::new(Ledger::default()),
    })
}

/// A bounded, process-total ledger in front of iced's shared font system.
///
/// [`FontRegistry::register_batch`] is the only production mutation entry:
/// sources, faces, collections, selections and instantiated face/weight
/// pairs are all accounted here, before and after the renderer transaction.
#[derive(Debug)]
pub struct FontRegistry {
    state: Mutex<Ledger>,
}

impl FontRegistry {
    /// Validate a whole batch and, if it passes, commit its fonts and
    /// selections to the renderer in one atomic transaction.
    ///
    /// Everything is resolved, capacity-checked and receipt-prepared before
    /// the renderer call, so every returned error leaves the registry, the
    /// renderer database, the pinned policies and iced's font-system
    /// version unchanged. After the seam commits, publication and receipt
    /// construction are infallible; the only post-commit check is the
    /// seam's one-ID-per-staged-face contract, which cannot fail by
    /// construction. Reused sources, faces, collections and selections do
    /// not grow anything, and a batch that adds no faces or policies does
    /// not bump the renderer version.
    pub fn register_batch(
        &self,
        batch: RegistrationBatch,
    ) -> Result<Registration, RegistrationError> {
        // Registry lock before the renderer write lock; recover from a
        // poisoned registry lock because the ledger is only ever published
        // after a successful commit and is therefore always consistent.
        let mut ledger = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut system = font_system()
            .write()
            .map_err(|_| RegistrationError::LockPoisoned)?;
        register_batch_in(&mut *system, &mut ledger, batch, &Limits::PROCESS)
    }

    /// The current process-wide registry usage, against the immutable caps.
    pub fn usage(&self) -> RegistryUsage {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .usage()
    }

    /// Return the immutable allocation already retained for these exact
    /// source bytes. This read-only lookup neither registers nor charges a
    /// second source; compact hosts use it after a successful batch so their
    /// reuse metadata shares the registry's actual allocation.
    pub fn retained_source(&self, bytes: &[u8]) -> Option<Arc<[u8]>> {
        let key = SourceKey {
            digest: *blake3::hash(bytes).as_bytes(),
            len: bytes.len() as u64,
        };
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sources
            .get(&key)
            .cloned()
    }

    /// Snapshot the renderer IDs of every face the registry has pinned and
    /// run `action` with it while holding the registry lock. Callers that
    /// mutate the shared font system inside `action` keep the documented
    /// registry-before-font-system lock order, so no registration can
    /// interleave between the snapshot and the mutation. The snapshot is
    /// read-only: it never creates, replaces or removes a pinned face.
    pub(crate) fn with_pinned_face_ids<R>(&self, action: impl FnOnce(&[fontdb::ID]) -> R) -> R {
        let ledger = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ids: Vec<fontdb::ID> = ledger.faces.values().map(|record| record.id).collect();
        action(&ids)
    }
}

/// Untrusted font bytes for one source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontBlob {
    pub bytes: Arc<[u8]>,
}

/// A face inside a [`FontCollection`], by source slot and face index.
///
/// The identity of a face is the digest of its source bytes plus this index,
/// never a public family name or a file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceFace {
    pub source: usize,
    pub index: u32,
}

/// A named family: the collection-local binding of a name to faces.
///
/// The name must match the parsed intrinsic family of every face it binds;
/// a mismatched claim is a preparation fault, not a silent alias.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyGroup {
    pub name: String,
    pub faces: Vec<SourceFace>,
}

/// A caller-supplied font collection: sources, family bindings, role chains
/// and icon catalogues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontCollection {
    pub sources: Vec<FontBlob>,
    pub families: Vec<FamilyGroup>,
    /// Role key → ordered declared family names, primary first.
    pub roles: BTreeMap<String, Vec<String>>,
    pub icons: Vec<IconCatalogue>,
}

/// A named icon catalogue: one face plus a name → codepoint table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconCatalogue {
    pub family: String,
    pub style: String,
    pub face: SourceFace,
    pub glyphs: BTreeMap<String, char>,
}

/// How a requested weight is satisfied.
///
/// [`WeightPolicy::Exact`] seals the requested weight and fails if a present
/// family cannot provide it; it never silently skips the family. An explicit
/// [`WeightPolicy::Substitute`] authorises advancing past a present family
/// that cannot provide the effective weight or the requested style/stretch,
/// with the reason recorded in the selection evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WeightPolicy {
    Exact,
    Substitute { effective: u16, reason: String },
}

/// A typography selection request: one role key and its declared chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionRequest {
    /// The role key. Not part of the selection identity: two roles making
    /// the exact same selection share one alias.
    pub key: String,
    /// Ordered declared families, primary first. A missing family advances
    /// to the next declared one; the chosen group is reported in the
    /// selection evidence.
    pub families: Vec<String>,
    pub requested_weight: u16,
    pub weight_policy: WeightPolicy,
    pub style: Style,
    pub stretch: Stretch,
}

/// An icon selection request: one key, one declared catalogue and the names
/// the host requires from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconSelectionRequest {
    pub key: String,
    pub family: String,
    pub style: String,
    /// The exact sealed weight, 1..=1000. Icons do not substitute.
    pub weight: u16,
    pub required_names: Vec<String>,
}

/// One atomic registration: a collection plus every selection to resolve
/// from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationBatch {
    pub collection: FontCollection,
    pub selections: Vec<SelectionRequest>,
    pub icons: Vec<IconSelectionRequest>,
}

/// The immutable identity of a registered collection: the hex BLAKE3 of the
/// canonical metadata, recomputed by the registry from the batch alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CollectionId {
    digest: String,
}

impl CollectionId {
    /// The 64 lowercase hex characters of the collection digest.
    pub fn as_str(&self) -> &str {
        &self.digest
    }
}

impl fmt::Display for CollectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.digest)
    }
}

/// A successful batch receipt. Internals are private validated values.
#[derive(Clone, Debug)]
pub struct Registration {
    inner: Arc<RegistrationInner>,
}

impl Registration {
    /// The identity of the registered collection.
    pub fn collection_id(&self) -> CollectionId {
        self.inner.collection.clone()
    }

    /// The selection made for a typography role key, if the batch requested it.
    pub fn font(&self, key: &str) -> Option<&Selection> {
        self.inner.fonts.get(key)
    }

    /// The glyph and aliased font of a named icon from an icon selection.
    pub fn icon(&self, key: &str, name: &str) -> Option<(char, Font)> {
        let icon = self.inner.icons.get(key)?;
        Some((*icon.glyphs.get(name)?, icon.selection.font))
    }

    /// Detailed evidence about what was registered and what was reused.
    pub fn evidence(&self) -> &RegistrationEvidence {
        &self.inner.evidence
    }
}

#[derive(Debug)]
struct RegistrationInner {
    collection: CollectionId,
    fonts: BTreeMap<String, Selection>,
    icons: BTreeMap<String, IconSelection>,
    evidence: RegistrationEvidence,
}

#[derive(Debug)]
struct IconSelection {
    selection: Selection,
    glyphs: Arc<BTreeMap<String, char>>,
}

/// One immutable selection: the aliased font to render with, the evidence of
/// how it was chosen and an owned view of the selected policy's faces.
#[derive(Clone, Debug)]
pub struct Selection {
    font: Font,
    evidence: Arc<SelectionEvidence>,
    owned: OwnedSelection,
}

impl Selection {
    /// The font to render this selection with. Its family is the private
    /// alias and its weight is the exact sealed numeric weight.
    pub fn font(&self) -> Font {
        self.font
    }

    /// How the selection was resolved: the declared chain, the chosen group,
    /// weights, substitution and the face identities involved.
    pub fn evidence(&self) -> &SelectionEvidence {
        &self.evidence
    }

    /// An immutable owned view of the selected policy: the ordered fallback
    /// groups of actual source bytes, face indices and intrinsic evidence,
    /// plus the exact effective weight. Clones share the ledger's source
    /// allocations; obtaining one never registers or allocates anything.
    pub fn owned(&self) -> OwnedSelection {
        self.owned.clone()
    }
}

/// An immutable view of one selection's ordered fallback groups: the actual
/// source bytes, face index and intrinsic evidence of every face the
/// renderer's pinned policy can shape with. Clones share the ledger's
/// source allocations; obtaining one never re-registers anything, allocates
/// renderer IDs or creates a parallel font cache. Constructors are private:
/// an [`OwnedSelection`] only comes from a registered [`Selection`].
#[derive(Clone, Debug)]
pub struct OwnedSelection {
    inner: Arc<OwnedSelectionInner>,
}

#[derive(Debug)]
struct OwnedSelectionInner {
    weight: u16,
    groups: Vec<Vec<OwnedFace>>,
}

impl OwnedSelection {
    /// The exact sealed numeric weight the selection was registered with.
    pub fn effective_weight(&self) -> u16 {
        self.inner.weight
    }

    /// The ordered eligible fallback groups: group 0 is the primary, later
    /// groups are the declared fallbacks, in declaration order. Absent and
    /// skipped declared families are omitted, so every group is non-empty.
    pub fn groups(&self) -> &[Vec<OwnedFace>] {
        &self.inner.groups
    }
}

/// One face of an [`OwnedSelection`]: the source bytes it was parsed from
/// (shared with the registry's retained allocation), its index inside those
/// bytes and its intrinsic evidence.
#[derive(Clone, Debug)]
pub struct OwnedFace {
    bytes: Arc<[u8]>,
    index: u32,
    evidence: FaceEvidence,
}

impl OwnedFace {
    /// The exact source bytes this face was parsed from. The allocation is
    /// shared with the registry ledger; cloning the [`Arc`] copies nothing.
    pub fn bytes(&self) -> Arc<[u8]> {
        self.bytes.clone()
    }

    /// The face index inside [`OwnedFace::bytes`].
    pub fn index(&self) -> u32 {
        self.index
    }

    /// The face's intrinsic evidence: source digest, byte length and index.
    pub fn evidence(&self) -> &FaceEvidence {
        &self.evidence
    }
}

/// Evidence for one selection.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionEvidence {
    /// The ordered declared family names, primary first.
    pub declared: Vec<String>,
    /// The index into `declared` that provided the first eligible group.
    pub chosen_group: usize,
    /// The parsed intrinsic family name of the chosen group.
    pub family: String,
    /// The eligible face identities of every eligible declared group, in
    /// declaration order. Absent and skipped families are omitted, so every
    /// group is non-empty.
    pub groups: Vec<Vec<FaceEvidence>>,
    pub requested_weight: u16,
    pub effective_weight: u16,
    /// Present when an explicit substitution authorised the effective weight.
    pub substitution: Option<SubstitutionEvidence>,
    /// Present families that could not provide the effective weight or the
    /// requested style/stretch and were skipped under an explicit
    /// substitution.
    pub skipped: Vec<String>,
    pub style: Style,
    pub stretch: Stretch,
}

/// The recorded reason for an explicit weight substitution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubstitutionEvidence {
    pub reason: String,
}

/// The identity of one face in evidence: its source digest, source byte
/// length and face index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceEvidence {
    pub source: String,
    pub bytes: u64,
    pub index: u32,
}

/// Evidence for a whole registration.
#[derive(Clone, Debug, PartialEq)]
pub struct RegistrationEvidence {
    /// Every source of the batch, in declared order.
    pub sources: Vec<SourceEvidence>,
    pub added_sources: usize,
    pub reused_sources: usize,
    pub added_faces: usize,
    pub reused_faces: usize,
    /// Newly installed aliases/policies.
    pub policies_added: usize,
    pub policies_reused: usize,
    /// The numeric iced font-system version immediately before and after the
    /// transaction, captured while holding the renderer write lock. The
    /// renderer bumps it once per real addition, so the delta is exactly
    /// one when the batch added faces or policies and zero otherwise.
    pub renderer_version_before: u32,
    pub renderer_version_after: u32,
    pub usage_before: RegistryUsage,
    pub usage_after: RegistryUsage,
}

/// One source in evidence: its recomputed digest and byte length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEvidence {
    pub digest: String,
    pub bytes: u64,
}

/// The process-wide registry usage, always within the immutable caps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegistryUsage {
    /// Bytes of unique retained sources, charged once per exact source.
    pub retained_bytes: u64,
    pub sources: usize,
    pub faces: usize,
    pub collections: usize,
    pub aliases: usize,
    /// Unique `(face, sealed weight)` instances across all policies.
    pub instantiated_pairs: usize,
}

/// Which capacity a batch exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    RetainedBytes,
    Faces,
    Collections,
    Aliases,
    InstantiatedPairs,
}

/// Why a [`FontRegistry::register_batch`] failed. Every error is produced
/// before the renderer transaction, so nothing is ever partially
/// registered: the registry and the renderer are left exactly as they were.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    TooManySources {
        limit: usize,
        have: usize,
    },
    SourceTooLarge {
        source: usize,
        bytes: u64,
        limit: u64,
    },
    BatchBytesTooLarge {
        bytes: u64,
        limit: u64,
    },
    /// The bytes hold no parseable face, declare zero faces, or are truncated.
    SourceUnparsable {
        source: usize,
    },
    TooManyFacesInSource {
        source: usize,
        faces: u32,
        limit: u32,
    },
    /// A declared face does not exist: either the source slot is absent
    /// (`faces == 0`) or the index was not among the parsed faces.
    FaceOutOfRange {
        source: usize,
        index: u32,
        faces: u32,
    },
    EmptyFamilyGroup {
        family: String,
    },
    EmptyFamilyChain {
        key: String,
    },
    /// A declared family name does not match the parsed intrinsic family of
    /// one of its faces.
    FamilyClaimMismatch {
        family: String,
        index: u32,
        parsed: Vec<String>,
    },
    NameEmpty {
        what: &'static str,
    },
    NameTooLong {
        what: &'static str,
        len: usize,
        limit: usize,
    },
    TooManyGroups {
        key: String,
        limit: usize,
        have: usize,
    },
    TooManyFaceRefs {
        key: String,
        limit: usize,
        have: usize,
    },
    TooManySelections {
        limit: usize,
        have: usize,
    },
    TooManyIconNames {
        limit: usize,
        have: usize,
    },
    DuplicateCatalogue {
        family: String,
        style: String,
    },
    DuplicateKey {
        key: String,
    },
    MetadataTooLarge {
        bytes: u64,
        limit: u64,
    },
    WeightOutOfRange {
        weight: u16,
    },
    /// No declared family exists in the collection.
    AllFamiliesAbsent {
        key: String,
    },
    /// A present family cannot provide the effective weight, and no explicit
    /// substitution authorises skipping it.
    UnsupportedWeight {
        key: String,
        family: String,
        weight: u16,
    },
    /// A present family can provide the effective weight but has no face
    /// with the requested style and stretch, and no explicit substitution
    /// authorises skipping it.
    UnsupportedStyle {
        key: String,
        family: String,
        style: Style,
        stretch: Stretch,
    },
    IconCatalogueNotFound {
        key: String,
        family: String,
        style: String,
    },
    IconNameMissing {
        key: String,
        name: String,
    },
    IconGlyphMissing {
        key: String,
        name: String,
        glyph: char,
    },
    Capacity {
        resource: Resource,
        have: u64,
        need: u64,
        limit: u64,
    },
    /// Distinct content collided with an existing cryptographic identity.
    SourceCollision {
        source: usize,
    },
    SelectionsCollision {
        alias: String,
    },
    /// The renderer rejected the transaction; its state is unchanged.
    Renderer(FontRegistrationError),
    /// The renderer's numeric font-system version is saturated; another
    /// registration cannot be represented. Checked before the transaction,
    /// so the renderer's own overflow handling is never reached.
    RendererVersionExhausted,
    LockPoisoned,
    /// An invariant the registry relies on was broken.
    Internal(&'static str),
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManySources { limit, have } => {
                write!(f, "collection holds {have} sources, at most {limit}")
            }
            Self::SourceTooLarge {
                source,
                bytes,
                limit,
            } => {
                write!(f, "source {source} holds {bytes} bytes, at most {limit}")
            }
            Self::BatchBytesTooLarge { bytes, limit } => {
                write!(f, "batch holds {bytes} incoming bytes, at most {limit}")
            }
            Self::SourceUnparsable { source } => {
                write!(f, "source {source} holds no parseable font face")
            }
            Self::TooManyFacesInSource {
                source,
                faces,
                limit,
            } => {
                write!(f, "source {source} declares {faces} faces, at most {limit}")
            }
            Self::FaceOutOfRange {
                source,
                index,
                faces,
            } => write!(f, "source {source} has no face {index} ({faces} faces)"),
            Self::EmptyFamilyGroup { family } => {
                write!(f, "family {family:?} binds no faces")
            }
            Self::EmptyFamilyChain { key } => {
                write!(f, "selection {key:?} declares no families")
            }
            Self::FamilyClaimMismatch {
                family,
                index,
                parsed,
            } => write!(
                f,
                "family {family:?} does not match face {index}'s intrinsic families {parsed:?}"
            ),
            Self::NameEmpty { what } => write!(f, "{what} name is empty"),
            Self::NameTooLong { what, len, limit } => {
                write!(f, "{what} name is {len} bytes, at most {limit}")
            }
            Self::TooManyGroups { key, limit, have } => {
                write!(
                    f,
                    "selection {key:?} declares {have} groups, at most {limit}"
                )
            }
            Self::TooManyFaceRefs { key, limit, have } => {
                write!(
                    f,
                    "selection {key:?} resolves {have} faces, at most {limit}"
                )
            }
            Self::TooManySelections { limit, have } => {
                write!(f, "batch holds {have} selections, at most {limit}")
            }
            Self::TooManyIconNames { limit, have } => {
                write!(f, "catalogue holds {have} icon names, at most {limit}")
            }
            Self::DuplicateCatalogue { family, style } => {
                write!(f, "duplicate icon catalogue {family:?} {style:?}")
            }
            Self::DuplicateKey { key } => write!(f, "duplicate selection key {key:?}"),
            Self::MetadataTooLarge { bytes, limit } => {
                write!(f, "collection metadata is {bytes} bytes, at most {limit}")
            }
            Self::WeightOutOfRange { weight } => {
                write!(f, "weight {weight} is outside 1..=1000")
            }
            Self::AllFamiliesAbsent { key } => {
                write!(
                    f,
                    "selection {key:?}: no declared family exists in the collection"
                )
            }
            Self::UnsupportedWeight {
                key,
                family,
                weight,
            } => write!(
                f,
                "selection {key:?}: family {family:?} cannot provide weight {weight}"
            ),
            Self::UnsupportedStyle {
                key,
                family,
                style,
                stretch,
            } => write!(
                f,
                "selection {key:?}: family {family:?} has no face with style {style:?} and stretch {stretch:?}"
            ),
            Self::IconCatalogueNotFound { key, family, style } => write!(
                f,
                "icon selection {key:?}: no catalogue {family:?} {style:?}"
            ),
            Self::IconNameMissing { key, name } => {
                write!(f, "icon selection {key:?}: catalogue has no icon {name:?}")
            }
            Self::IconGlyphMissing { key, name, glyph } => write!(
                f,
                "icon selection {key:?}: icon {name:?} ({glyph}) has no glyph in its face"
            ),
            Self::Capacity {
                resource,
                have,
                need,
                limit,
            } => write!(
                f,
                "capacity {resource:?}: {have} retained, {need} more needed, limit {limit}"
            ),
            Self::SourceCollision { source } => {
                write!(
                    f,
                    "source {source} collides with a distinct retained source"
                )
            }
            Self::SelectionsCollision { alias } => {
                write!(
                    f,
                    "selection identity {alias:?} already holds different content"
                )
            }
            Self::Renderer(error) => write!(f, "renderer registration failed: {error}"),
            Self::RendererVersionExhausted => {
                write!(f, "renderer font-system version is exhausted")
            }
            Self::LockPoisoned => write!(f, "font system lock poisoned"),
            Self::Internal(detail) => write!(f, "registry invariant broken: {detail}"),
        }
    }
}

impl std::error::Error for RegistrationError {}

/// The immutable process caps, with smaller limits injectable only into
/// isolated tests.
#[derive(Clone, Copy, Debug)]
struct Limits {
    source_bytes: u64,
    batch_bytes: u64,
    retained_bytes: u64,
    retained_faces: u64,
    retained_collections: u64,
    retained_selections: u64,
    instantiated_pairs: u64,
    sources_per_collection: u64,
    groups_per_policy: u64,
    face_refs_per_policy: u64,
    selections_per_batch: u64,
    metadata_bytes: u64,
    icon_names_per_catalogue: u64,
    family_name_bytes: u64,
    key_bytes: u64,
    faces_per_source: u32,
}

const MIB: u64 = 1024 * 1024;

impl Limits {
    const PROCESS: Limits = Limits {
        source_bytes: 32 * MIB,
        batch_bytes: 128 * MIB,
        retained_bytes: 256 * MIB,
        retained_faces: 512,
        retained_collections: 64,
        retained_selections: 1024,
        instantiated_pairs: 4096,
        sources_per_collection: 256,
        groups_per_policy: 16,
        face_refs_per_policy: 64,
        selections_per_batch: 256,
        metadata_bytes: 2 * MIB,
        icon_names_per_catalogue: 16_384,
        family_name_bytes: 256,
        key_bytes: 96,
        faces_per_source: 512,
    };
}

type Digest = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SourceKey {
    digest: Digest,
    len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct FaceKey {
    source: Digest,
    /// The byte length of the source, carried so that same-digest
    /// different-length sources can never alias one face to another.
    len: u64,
    index: u32,
}

#[derive(Clone, Debug)]
struct FaceRecord {
    /// The renderer database ID, assigned by the committed transaction.
    id: fontdb::ID,
    static_weight: u16,
    /// The `wght` variation axis range, when the face is variable.
    wght: Option<(f32, f32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PolicyRecord {
    alias: String,
    groups: Vec<Vec<FaceKey>>,
    weight: u16,
}

/// The registry's process-total ledger.
#[derive(Debug, Default)]
struct Ledger {
    sources: BTreeMap<SourceKey, Arc<[u8]>>,
    retained_bytes: u64,
    faces: BTreeMap<FaceKey, FaceRecord>,
    collections: BTreeSet<Digest>,
    selections: BTreeMap<Digest, PolicyRecord>,
    instantiated: BTreeSet<(FaceKey, u16)>,
    /// Interned aliases for `Family::Name`; bounded by the selection cap.
    alias_interns: BTreeMap<String, &'static str>,
}

impl Ledger {
    fn usage(&self) -> RegistryUsage {
        RegistryUsage {
            retained_bytes: self.retained_bytes,
            sources: self.sources.len(),
            faces: self.faces.len(),
            collections: self.collections.len(),
            aliases: self.selections.len(),
            instantiated_pairs: self.instantiated.len(),
        }
    }
}

struct ParsedSource {
    key: SourceKey,
    bytes: Arc<[u8]>,
    db: fontdb::Database,
    faces: BTreeMap<u32, ParsedFace>,
}

struct ParsedFace {
    info: fontdb::FaceInfo,
    static_weight: u16,
    wght: Option<(f32, f32)>,
    families: Vec<String>,
}

struct SelectionPlan {
    key: String,
    digest: Digest,
    declared: Vec<String>,
    groups: Vec<Vec<FaceKey>>,
    evidences: Vec<Vec<FaceEvidence>>,
    chosen_group: usize,
    family: String,
    requested: u16,
    effective: u16,
    substitution: Option<SubstitutionEvidence>,
    skipped: Vec<String>,
    style: Style,
    stretch: Stretch,
}

struct IconPlan {
    key: String,
    digest: Digest,
    /// The declared family as requested, for evidence.
    family: String,
    /// The parsed intrinsic family the catalogue's claim matched.
    intrinsic: String,
    face: FaceKey,
    weight: u16,
    glyphs: Arc<BTreeMap<String, char>>,
}

struct Staged {
    collection_digest: Digest,
    new_sources: Vec<(SourceKey, Arc<[u8]>)>,
    new_faces: Vec<(FaceKey, FaceRecord, fontdb::FaceInfo)>,
    new_policies: BTreeMap<Digest, PolicyRecord>,
    new_pairs: BTreeSet<(FaceKey, u16)>,
    /// Every policy of the batch, reused and new.
    policies: BTreeMap<Digest, PolicyRecord>,
}

/// The renderer transaction a batch commits through.
trait RendererOwner {
    fn register(
        &mut self,
        registration: FontRegistration,
    ) -> Result<FontRegistrationResult, FontRegistrationError>;
    /// The numeric font-system version, bumped once per real addition.
    fn version(&self) -> u32;
}

/// The production owner: iced's process-wide font system. The wrapper bumps
/// its own version once per real addition and rolls back on error.
impl RendererOwner for iced_graphics::text::FontSystem {
    fn register(
        &mut self,
        registration: FontRegistration,
    ) -> Result<FontRegistrationResult, FontRegistrationError> {
        self.register_fonts(registration)
    }

    fn version(&self) -> u32 {
        self.version().value()
    }
}

const ALIAS_PREFIX: &str = "mixos-pinned-";
const COLLECTION_DOMAIN: &[u8] = b"mixos-toolkit-font-collection-v1\0";
const SELECTION_DOMAIN: &[u8] = b"mixos-toolkit-font-selection-v1\0";
const ICON_SELECTION_DOMAIN: &[u8] = b"mixos-toolkit-font-icon-selection-v1\0";

fn digest_hex(digest: &Digest) -> String {
    blake3::Hash::from_bytes(*digest).to_hex().to_string()
}

fn alias_for(digest: &Digest) -> String {
    format!("{ALIAS_PREFIX}{}", digest_hex(digest))
}

fn write_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn style_index(style: Style) -> u8 {
    match style {
        Style::Normal => 0,
        Style::Italic => 1,
        Style::Oblique => 2,
    }
}

fn stretch_index(stretch: Stretch) -> u8 {
    match stretch {
        Stretch::UltraCondensed => 0,
        Stretch::ExtraCondensed => 1,
        Stretch::Condensed => 2,
        Stretch::SemiCondensed => 3,
        Stretch::Normal => 4,
        Stretch::SemiExpanded => 5,
        Stretch::Expanded => 6,
        Stretch::ExtraExpanded => 7,
        Stretch::UltraExpanded => 8,
    }
}

fn validate_weight(weight: u16) -> Result<(), RegistrationError> {
    if !(1..=1000).contains(&weight) {
        return Err(RegistrationError::WeightOutOfRange { weight });
    }
    Ok(())
}

fn check_name(what: &'static str, name: &str, limit: u64) -> Result<(), RegistrationError> {
    if name.is_empty() {
        return Err(RegistrationError::NameEmpty { what });
    }
    if name.len() as u64 > limit {
        return Err(RegistrationError::NameTooLong {
            what,
            len: name.len(),
            limit: limit as usize,
        });
    }
    Ok(())
}

fn add_metadata(bytes: u64, len: usize, limits: &Limits) -> Result<u64, RegistrationError> {
    let next = bytes
        .checked_add(len as u64)
        .ok_or(RegistrationError::MetadataTooLarge {
            bytes,
            limit: limits.metadata_bytes,
        })?;
    if next > limits.metadata_bytes {
        return Err(RegistrationError::MetadataTooLarge {
            bytes: next,
            limit: limits.metadata_bytes,
        });
    }
    Ok(next)
}

/// Every count, length and name bound of the batch, checked with checked
/// arithmetic before anything is hashed or parsed.
fn validate_shape(batch: &RegistrationBatch, limits: &Limits) -> Result<(), RegistrationError> {
    let collection = &batch.collection;
    let sources = collection.sources.len() as u64;
    if sources > limits.sources_per_collection {
        return Err(RegistrationError::TooManySources {
            limit: limits.sources_per_collection as usize,
            have: collection.sources.len(),
        });
    }
    let mut batch_bytes: u64 = 0;
    for (source, blob) in collection.sources.iter().enumerate() {
        let len = blob.bytes.len() as u64;
        if len > limits.source_bytes {
            return Err(RegistrationError::SourceTooLarge {
                source,
                bytes: len,
                limit: limits.source_bytes,
            });
        }
        batch_bytes =
            batch_bytes
                .checked_add(len)
                .ok_or(RegistrationError::BatchBytesTooLarge {
                    bytes: u64::MAX,
                    limit: limits.batch_bytes,
                })?;
    }
    if batch_bytes > limits.batch_bytes {
        return Err(RegistrationError::BatchBytesTooLarge {
            bytes: batch_bytes,
            limit: limits.batch_bytes,
        });
    }

    let mut metadata: u64 = 0;
    for group in &collection.families {
        check_name("family", &group.name, limits.family_name_bytes)?;
        metadata = add_metadata(metadata, group.name.len(), limits)?;
        if group.faces.is_empty() {
            return Err(RegistrationError::EmptyFamilyGroup {
                family: group.name.clone(),
            });
        }
        for face in &group.faces {
            if face.source as u64 >= sources {
                return Err(RegistrationError::FaceOutOfRange {
                    source: face.source,
                    index: face.index,
                    faces: 0,
                });
            }
        }
    }
    for (key, chain) in &collection.roles {
        check_name("role", key, limits.key_bytes)?;
        metadata = add_metadata(metadata, key.len(), limits)?;
        if chain.is_empty() {
            return Err(RegistrationError::EmptyFamilyChain { key: key.clone() });
        }
        if chain.len() as u64 > limits.groups_per_policy {
            return Err(RegistrationError::TooManyGroups {
                key: key.clone(),
                limit: limits.groups_per_policy as usize,
                have: chain.len(),
            });
        }
        for name in chain {
            check_name("family", name, limits.family_name_bytes)?;
            metadata = add_metadata(metadata, name.len(), limits)?;
        }
    }
    let mut catalogues: BTreeSet<(String, String)> = BTreeSet::new();
    for catalogue in &collection.icons {
        check_name("icon family", &catalogue.family, limits.key_bytes)?;
        check_name("icon style", &catalogue.style, limits.key_bytes)?;
        metadata = add_metadata(metadata, catalogue.family.len(), limits)?;
        metadata = add_metadata(metadata, catalogue.style.len(), limits)?;
        // Catalogue identity matches lookup: the family is case-folded, the
        // style is exact.
        if !catalogues.insert((
            catalogue.family.to_ascii_lowercase(),
            catalogue.style.clone(),
        )) {
            return Err(RegistrationError::DuplicateCatalogue {
                family: catalogue.family.clone(),
                style: catalogue.style.clone(),
            });
        }
        if catalogue.face.source as u64 >= sources {
            return Err(RegistrationError::FaceOutOfRange {
                source: catalogue.face.source,
                index: catalogue.face.index,
                faces: 0,
            });
        }
        if catalogue.glyphs.len() as u64 > limits.icon_names_per_catalogue {
            return Err(RegistrationError::TooManyIconNames {
                limit: limits.icon_names_per_catalogue as usize,
                have: catalogue.glyphs.len(),
            });
        }
        for name in catalogue.glyphs.keys() {
            check_name("icon name", name, limits.key_bytes)?;
            metadata = add_metadata(metadata, name.len(), limits)?;
        }
    }

    let selection_count = batch.selections.len() + batch.icons.len();
    if selection_count as u64 > limits.selections_per_batch {
        return Err(RegistrationError::TooManySelections {
            limit: limits.selections_per_batch as usize,
            have: selection_count,
        });
    }
    let mut selection_keys: BTreeSet<&str> = BTreeSet::new();
    for selection in &batch.selections {
        check_name("selection key", &selection.key, limits.key_bytes)?;
        if !selection_keys.insert(&selection.key) {
            return Err(RegistrationError::DuplicateKey {
                key: selection.key.clone(),
            });
        }
        metadata = add_metadata(metadata, selection.key.len(), limits)?;
        if selection.families.is_empty() {
            return Err(RegistrationError::EmptyFamilyChain {
                key: selection.key.clone(),
            });
        }
        if selection.families.len() as u64 > limits.groups_per_policy {
            return Err(RegistrationError::TooManyGroups {
                key: selection.key.clone(),
                limit: limits.groups_per_policy as usize,
                have: selection.families.len(),
            });
        }
        for name in &selection.families {
            check_name("family", name, limits.family_name_bytes)?;
            metadata = add_metadata(metadata, name.len(), limits)?;
        }
        validate_weight(selection.requested_weight)?;
        if let WeightPolicy::Substitute { effective, reason } = &selection.weight_policy {
            validate_weight(*effective)?;
            check_name("substitution reason", reason, limits.family_name_bytes)?;
            metadata = add_metadata(metadata, reason.len(), limits)?;
        }
    }
    let mut icon_keys: BTreeSet<&str> = BTreeSet::new();
    for icon in &batch.icons {
        check_name("icon key", &icon.key, limits.key_bytes)?;
        if !icon_keys.insert(&icon.key) {
            return Err(RegistrationError::DuplicateKey {
                key: icon.key.clone(),
            });
        }
        metadata = add_metadata(metadata, icon.key.len(), limits)?;
        check_name("icon family", &icon.family, limits.key_bytes)?;
        check_name("icon style", &icon.style, limits.key_bytes)?;
        metadata = add_metadata(metadata, icon.family.len(), limits)?;
        metadata = add_metadata(metadata, icon.style.len(), limits)?;
        validate_weight(icon.weight)?;
        for name in &icon.required_names {
            check_name("icon name", name, limits.key_bytes)?;
            metadata = add_metadata(metadata, name.len(), limits)?;
        }
    }
    Ok(())
}

/// Precheck a source's TTC header before fontdb allocates anything, then
/// hash and parse its faces into a scratch database.
fn parse_sources(
    batch: &RegistrationBatch,
    limits: &Limits,
) -> Result<Vec<ParsedSource>, RegistrationError> {
    let mut parsed = Vec::with_capacity(batch.collection.sources.len());
    for (source, blob) in batch.collection.sources.iter().enumerate() {
        let bytes = &blob.bytes;
        if bytes.len() < 4 {
            return Err(RegistrationError::SourceUnparsable { source });
        }
        if &bytes[0..4] == b"ttcf" {
            if bytes.len() < 12 {
                return Err(RegistrationError::SourceUnparsable { source });
            }
            let faces = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
            if faces == 0 {
                return Err(RegistrationError::SourceUnparsable { source });
            }
            if faces > limits.faces_per_source {
                return Err(RegistrationError::TooManyFacesInSource {
                    source,
                    faces,
                    limit: limits.faces_per_source,
                });
            }
        }
        let digest = *blake3::hash(bytes).as_bytes();
        let key = SourceKey {
            digest,
            len: bytes.len() as u64,
        };
        let mut db = fontdb::Database::new();
        db.load_font_source(fontdb::Source::Binary(blob.bytes.clone()));
        if db.is_empty() {
            return Err(RegistrationError::SourceUnparsable { source });
        }
        let mut faces = BTreeMap::new();
        for face in db.faces() {
            // Probe at the face's own static weight, which is always
            // instantiable for the default instance; a variable face whose
            // range excludes NORMAL would otherwise be misread as failing.
            let wght = cosmic_text::Font::new(&db, face.id, face.weight).map(|font| {
                font.as_swash()
                    .variations()
                    .find(|axis| axis.tag() == u32::from_be_bytes(*b"wght"))
                    .map(|axis| (axis.min_value(), axis.max_value()))
            });
            let wght = wght.flatten();
            faces.insert(
                face.index,
                ParsedFace {
                    info: face.clone(),
                    static_weight: face.weight.0,
                    wght,
                    families: face.families.iter().map(|(name, _)| name.clone()).collect(),
                },
            );
        }
        parsed.push(ParsedSource {
            key,
            bytes: blob.bytes.clone(),
            db,
            faces,
        });
    }
    Ok(parsed)
}

fn collection_digest(collection: &FontCollection, parsed: &[ParsedSource]) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(COLLECTION_DOMAIN);
    hasher.update(&(parsed.len() as u64).to_le_bytes());
    for source in parsed {
        hasher.update(&source.key.len.to_le_bytes());
        hasher.update(&source.key.digest);
    }
    hasher.update(&(collection.families.len() as u64).to_le_bytes());
    for group in &collection.families {
        write_bytes(&mut hasher, group.name.as_bytes());
        hasher.update(&(group.faces.len() as u64).to_le_bytes());
        for face in &group.faces {
            hasher.update(&(face.source as u64).to_le_bytes());
            hasher.update(&face.index.to_le_bytes());
        }
    }
    hasher.update(&(collection.roles.len() as u64).to_le_bytes());
    for (key, chain) in &collection.roles {
        write_bytes(&mut hasher, key.as_bytes());
        hasher.update(&(chain.len() as u64).to_le_bytes());
        for name in chain {
            write_bytes(&mut hasher, name.as_bytes());
        }
    }
    hasher.update(&(collection.icons.len() as u64).to_le_bytes());
    for catalogue in &collection.icons {
        write_bytes(&mut hasher, catalogue.family.as_bytes());
        write_bytes(&mut hasher, catalogue.style.as_bytes());
        hasher.update(&(catalogue.face.source as u64).to_le_bytes());
        hasher.update(&catalogue.face.index.to_le_bytes());
        hasher.update(&(catalogue.glyphs.len() as u64).to_le_bytes());
        for (name, glyph) in &catalogue.glyphs {
            write_bytes(&mut hasher, name.as_bytes());
            hasher.update(&u32::from(*glyph).to_le_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

fn selection_digest(
    collection: &Digest,
    groups: &[Vec<FaceKey>],
    requested: u16,
    effective: u16,
    policy: &WeightPolicy,
    style: Style,
    stretch: Stretch,
) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SELECTION_DOMAIN);
    hasher.update(collection);
    hasher.update(&(groups.len() as u64).to_le_bytes());
    for group in groups {
        hasher.update(&(group.len() as u64).to_le_bytes());
        for face in group {
            hasher.update(&face.source);
            hasher.update(&face.len.to_le_bytes());
            hasher.update(&face.index.to_le_bytes());
        }
    }
    hasher.update(&requested.to_le_bytes());
    hasher.update(&effective.to_le_bytes());
    match policy {
        WeightPolicy::Exact => hasher.update(&[0]),
        WeightPolicy::Substitute { reason, .. } => {
            hasher.update(&[1]);
            write_bytes(&mut hasher, reason.as_bytes());
        }
    }
    hasher.update(&[style_index(style)]);
    hasher.update(&[stretch_index(stretch)]);
    *hasher.finalize().as_bytes()
}

fn icon_selection_digest(
    collection: &Digest,
    family: &str,
    style: &str,
    face: FaceKey,
    weight: u16,
) -> Digest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ICON_SELECTION_DOMAIN);
    hasher.update(collection);
    write_bytes(&mut hasher, family.as_bytes());
    write_bytes(&mut hasher, style.as_bytes());
    hasher.update(&face.source);
    hasher.update(&face.len.to_le_bytes());
    hasher.update(&face.index.to_le_bytes());
    hasher.update(&weight.to_le_bytes());
    hasher.update(&[0, 4]); // Normal style and stretch, constant for icons
    *hasher.finalize().as_bytes()
}

fn resolve_face(
    parsed: &[ParsedSource],
    face_ref: &SourceFace,
) -> Result<(FaceKey, &ParsedFace), RegistrationError> {
    let Some(source) = parsed.get(face_ref.source) else {
        return Err(RegistrationError::FaceOutOfRange {
            source: face_ref.source,
            index: face_ref.index,
            faces: 0,
        });
    };
    let Some(face) = source.faces.get(&face_ref.index) else {
        return Err(RegistrationError::FaceOutOfRange {
            source: face_ref.source,
            index: face_ref.index,
            faces: source.faces.len() as u32,
        });
    };
    Ok((
        FaceKey {
            source: source.key.digest,
            len: source.key.len,
            index: face_ref.index,
        },
        face,
    ))
}

/// Every face a family group binds must intrinsically carry the group's
/// name: a mismatched claim is a preparation fault, never a silent alias.
fn validate_family_claims(
    collection: &FontCollection,
    parsed: &[ParsedSource],
) -> Result<(), RegistrationError> {
    for group in &collection.families {
        for face_ref in &group.faces {
            let (_, face) = resolve_face(parsed, face_ref)?;
            if !face
                .families
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&group.name))
            {
                return Err(RegistrationError::FamilyClaimMismatch {
                    family: group.name.clone(),
                    index: face_ref.index,
                    parsed: face.families.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Why a present family was skipped under an explicit substitution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SkipReason {
    Weight,
    Style,
}

fn fontdb_style(style: Style) -> fontdb::Style {
    match style {
        Style::Normal => fontdb::Style::Normal,
        Style::Italic => fontdb::Style::Italic,
        Style::Oblique => fontdb::Style::Oblique,
    }
}

fn fontdb_stretch(stretch: Stretch) -> fontdb::Stretch {
    match stretch {
        Stretch::UltraCondensed => fontdb::Stretch::UltraCondensed,
        Stretch::ExtraCondensed => fontdb::Stretch::ExtraCondensed,
        Stretch::Condensed => fontdb::Stretch::Condensed,
        Stretch::SemiCondensed => fontdb::Stretch::SemiCondensed,
        Stretch::Normal => fontdb::Stretch::Normal,
        Stretch::SemiExpanded => fontdb::Stretch::SemiExpanded,
        Stretch::Expanded => fontdb::Stretch::Expanded,
        Stretch::ExtraExpanded => fontdb::Stretch::ExtraExpanded,
        Stretch::UltraExpanded => fontdb::Stretch::UltraExpanded,
    }
}

impl ParsedFace {
    fn supports(&self, weight: u16) -> bool {
        if self.static_weight == weight {
            return true;
        }
        self.wght
            .is_some_and(|(min, max)| f32::from(weight) >= min && f32::from(weight) <= max)
    }

    fn eligible(&self, weight: u16, style: Style, stretch: Stretch) -> bool {
        self.supports(weight)
            && self.info.style == fontdb_style(style)
            && self.info.stretch == fontdb_stretch(stretch)
    }
}

fn resolve_selection(
    request: &SelectionRequest,
    collection: &FontCollection,
    parsed: &[ParsedSource],
    collection_digest: &Digest,
    limits: &Limits,
) -> Result<SelectionPlan, RegistrationError> {
    let (effective, substitution) = match &request.weight_policy {
        WeightPolicy::Exact => (request.requested_weight, None),
        WeightPolicy::Substitute { effective, reason } => (
            *effective,
            Some(SubstitutionEvidence {
                reason: reason.clone(),
            }),
        ),
    };
    let mut groups: Vec<Vec<FaceKey>> = Vec::new();
    let mut evidences: Vec<Vec<FaceEvidence>> = Vec::new();
    let mut chosen: Option<(usize, String)> = None;
    let mut skipped: Vec<String> = Vec::new();
    let mut last_skipped: Option<(String, SkipReason)> = None;
    for (position, name) in request.families.iter().enumerate() {
        let Some(group) = collection
            .families
            .iter()
            .find(|group| group.name.eq_ignore_ascii_case(name))
        else {
            // Absent families advance without leaving an empty policy group.
            continue;
        };
        let mut faces = Vec::new();
        let mut face_evidences = Vec::new();
        let mut weight_capable = false;
        let mut intrinsic: Option<String> = None;
        for face_ref in &group.faces {
            let (key, face) = resolve_face(parsed, face_ref)?;
            if face.supports(effective) {
                weight_capable = true;
            }
            if !face.eligible(effective, request.style, request.stretch) {
                continue;
            }
            if intrinsic.is_none() {
                intrinsic = face
                    .families
                    .iter()
                    .find(|family| family.eq_ignore_ascii_case(&group.name))
                    .cloned();
            }
            face_evidences.push(FaceEvidence {
                source: digest_hex(&key.source),
                bytes: key.len,
                index: key.index,
            });
            faces.push(key);
        }
        if faces.is_empty() {
            let reason = if weight_capable {
                SkipReason::Style
            } else {
                SkipReason::Weight
            };
            skipped.push(name.clone());
            last_skipped = Some((name.clone(), reason));
            // Exact never skips a present but ineligible family silently.
            if matches!(request.weight_policy, WeightPolicy::Exact) {
                return Err(match reason {
                    SkipReason::Style => RegistrationError::UnsupportedStyle {
                        key: request.key.clone(),
                        family: name.clone(),
                        style: request.style,
                        stretch: request.stretch,
                    },
                    SkipReason::Weight => RegistrationError::UnsupportedWeight {
                        key: request.key.clone(),
                        family: name.clone(),
                        weight: effective,
                    },
                });
            }
            continue;
        }
        groups.push(faces);
        evidences.push(face_evidences);
        if chosen.is_none() {
            // The intrinsic claim was validated for the whole collection
            // before resolution, so a matched name exists; a missing one is
            // an invariant break reported pre-commit.
            let intrinsic = intrinsic.ok_or(RegistrationError::Internal(
                "validated family claim missing from chosen group",
            ))?;
            chosen = Some((position, intrinsic));
        }
        // No break: every eligible declared group joins the policy in order.
    }
    let Some((chosen_group, family)) = chosen else {
        return Err(match last_skipped {
            Some((family, reason)) => match reason {
                SkipReason::Weight => RegistrationError::UnsupportedWeight {
                    key: request.key.clone(),
                    family,
                    weight: effective,
                },
                SkipReason::Style => RegistrationError::UnsupportedStyle {
                    key: request.key.clone(),
                    family,
                    style: request.style,
                    stretch: request.stretch,
                },
            },
            None => RegistrationError::AllFamiliesAbsent {
                key: request.key.clone(),
            },
        });
    };
    let refs: usize = groups.iter().map(Vec::len).sum();
    if refs as u64 > limits.face_refs_per_policy {
        return Err(RegistrationError::TooManyFaceRefs {
            key: request.key.clone(),
            limit: limits.face_refs_per_policy as usize,
            have: refs,
        });
    }
    let digest = selection_digest(
        collection_digest,
        &groups,
        request.requested_weight,
        effective,
        &request.weight_policy,
        request.style,
        request.stretch,
    );
    Ok(SelectionPlan {
        key: request.key.clone(),
        digest,
        declared: request.families.clone(),
        groups,
        evidences,
        chosen_group,
        family,
        requested: request.requested_weight,
        effective,
        substitution,
        skipped,
        style: request.style,
        stretch: request.stretch,
    })
}

fn resolve_icon(
    request: &IconSelectionRequest,
    collection: &FontCollection,
    parsed: &[ParsedSource],
    collection_digest: &Digest,
) -> Result<IconPlan, RegistrationError> {
    let catalogue = collection
        .icons
        .iter()
        .find(|catalogue| {
            catalogue.family.eq_ignore_ascii_case(&request.family)
                && catalogue.style == request.style
        })
        .ok_or_else(|| RegistrationError::IconCatalogueNotFound {
            key: request.key.clone(),
            family: request.family.clone(),
            style: request.style.clone(),
        })?;
    let (face_key, face) = resolve_face(parsed, &catalogue.face)?;
    // The catalogue's family claim is validated like a group's: it must be
    // one of the face's parsed intrinsic families, so the receipt never
    // reports an unverified family string.
    let intrinsic = face
        .families
        .iter()
        .find(|family| family.eq_ignore_ascii_case(&catalogue.family))
        .cloned()
        .ok_or_else(|| RegistrationError::FamilyClaimMismatch {
            family: catalogue.family.clone(),
            index: catalogue.face.index,
            parsed: face.families.clone(),
        })?;
    if !face.supports(request.weight) {
        return Err(RegistrationError::UnsupportedWeight {
            key: request.key.clone(),
            family: request.family.clone(),
            weight: request.weight,
        });
    }
    if !face.eligible(request.weight, Style::Normal, Stretch::Normal) {
        return Err(RegistrationError::UnsupportedStyle {
            key: request.key.clone(),
            family: request.family.clone(),
            style: Style::Normal,
            stretch: Stretch::Normal,
        });
    }
    let source = &parsed[catalogue.face.source];
    let font = cosmic_text::Font::new(&source.db, face.info.id, face.info.weight).ok_or(
        RegistrationError::Internal("icon face did not parse in scratch"),
    )?;
    for name in &request.required_names {
        let Some(&glyph) = catalogue.glyphs.get(name) else {
            return Err(RegistrationError::IconNameMissing {
                key: request.key.clone(),
                name: name.clone(),
            });
        };
        if font.as_swash().charmap().map(glyph) == 0 {
            return Err(RegistrationError::IconGlyphMissing {
                key: request.key.clone(),
                name: name.clone(),
                glyph,
            });
        }
    }
    let digest = icon_selection_digest(
        collection_digest,
        &request.family,
        &request.style,
        face_key,
        request.weight,
    );
    Ok(IconPlan {
        key: request.key.clone(),
        digest,
        family: request.family.clone(),
        intrinsic,
        face: face_key,
        weight: request.weight,
        glyphs: Arc::new(catalogue.glyphs.clone()),
    })
}

/// Dedup the batch against the ledger and stage everything that is new,
/// without touching the renderer or the ledger.
fn stage(
    ledger: &Ledger,
    parsed: &[ParsedSource],
    collection_digest: Digest,
    plans: &[SelectionPlan],
    icon_plans: &[IconPlan],
) -> Result<Staged, RegistrationError> {
    let mut policies: BTreeMap<Digest, PolicyRecord> = BTreeMap::new();
    for plan in plans {
        let record = PolicyRecord {
            alias: alias_for(&plan.digest),
            groups: plan.groups.clone(),
            weight: plan.effective,
        };
        if let Some(existing) = policies.get(&plan.digest) {
            if existing != &record {
                return Err(RegistrationError::SelectionsCollision {
                    alias: record.alias,
                });
            }
        } else {
            policies.insert(plan.digest, record);
        }
    }
    for plan in icon_plans {
        let record = PolicyRecord {
            alias: alias_for(&plan.digest),
            groups: vec![vec![plan.face]],
            weight: plan.weight,
        };
        if let Some(existing) = policies.get(&plan.digest) {
            if existing != &record {
                return Err(RegistrationError::SelectionsCollision {
                    alias: record.alias,
                });
            }
        } else {
            policies.insert(plan.digest, record);
        }
    }

    let mut new_sources: Vec<(SourceKey, Arc<[u8]>)> = Vec::new();
    let mut staged_sources: BTreeMap<SourceKey, Arc<[u8]>> = BTreeMap::new();
    for (source, parsed_source) in parsed.iter().enumerate() {
        let key = parsed_source.key;
        let known = ledger
            .sources
            .get(&key)
            .or_else(|| staged_sources.get(&key));
        if let Some(existing) = known {
            if existing.as_ref() != parsed_source.bytes.as_ref() {
                return Err(RegistrationError::SourceCollision { source });
            }
            continue;
        }
        staged_sources.insert(key, parsed_source.bytes.clone());
        new_sources.push((key, parsed_source.bytes.clone()));
    }

    let slot_by_source: BTreeMap<SourceKey, usize> = parsed
        .iter()
        .enumerate()
        .map(|(slot, source)| (source.key, slot))
        .collect();
    let mut new_face_keys: BTreeSet<FaceKey> = BTreeSet::new();
    for policy in policies.values() {
        for group in &policy.groups {
            for face in group {
                if !ledger.faces.contains_key(face) {
                    new_face_keys.insert(*face);
                }
            }
        }
    }
    let mut new_faces: Vec<(FaceKey, FaceRecord, fontdb::FaceInfo)> = Vec::new();
    for face_key in &new_face_keys {
        let source_key = SourceKey {
            digest: face_key.source,
            len: face_key.len,
        };
        let slot = *slot_by_source
            .get(&source_key)
            .ok_or(RegistrationError::Internal("face source digest not staged"))?;
        let parsed_face =
            parsed[slot]
                .faces
                .get(&face_key.index)
                .ok_or(RegistrationError::Internal(
                    "resolved face index not parsed",
                ))?;
        let record = FaceRecord {
            id: fontdb::ID::default(),
            static_weight: parsed_face.static_weight,
            wght: parsed_face.wght,
        };
        let canonical = ledger
            .sources
            .get(&source_key)
            .or_else(|| staged_sources.get(&source_key))
            .ok_or(RegistrationError::Internal(
                "new face source has no canonical allocation",
            ))?;
        let mut info = parsed_face.info.clone();
        info.source = fontdb::Source::Binary(Arc::new(canonical.clone()));
        new_faces.push((*face_key, record, info));
    }

    let mut new_policies: BTreeMap<Digest, PolicyRecord> = BTreeMap::new();
    for (digest, record) in &policies {
        match ledger.selections.get(digest) {
            Some(existing) if existing == record => {}
            Some(_) => {
                return Err(RegistrationError::SelectionsCollision {
                    alias: record.alias.clone(),
                });
            }
            None => {
                new_policies.insert(*digest, record.clone());
            }
        }
    }

    let mut new_pairs: BTreeSet<(FaceKey, u16)> = BTreeSet::new();
    for policy in policies.values() {
        for group in &policy.groups {
            for face in group {
                let pair = (*face, policy.weight);
                if !ledger.instantiated.contains(&pair) {
                    new_pairs.insert(pair);
                }
            }
        }
    }

    Ok(Staged {
        collection_digest,
        new_sources,
        new_faces,
        new_policies,
        new_pairs,
        policies,
    })
}

fn check_capacity(
    resource: Resource,
    have: u64,
    need: u64,
    limit: u64,
) -> Result<(), RegistrationError> {
    if have.checked_add(need).is_none_or(|total| total > limit) {
        return Err(RegistrationError::Capacity {
            resource,
            have,
            need,
            limit,
        });
    }
    Ok(())
}

fn preflight_capacity(
    ledger: &Ledger,
    staged: &Staged,
    limits: &Limits,
) -> Result<(), RegistrationError> {
    let need_bytes = staged
        .new_sources
        .iter()
        .fold(0u64, |total, (_, bytes)| total + bytes.len() as u64);
    check_capacity(
        Resource::RetainedBytes,
        ledger.retained_bytes,
        need_bytes,
        limits.retained_bytes,
    )?;
    check_capacity(
        Resource::Faces,
        ledger.faces.len() as u64,
        staged.new_faces.len() as u64,
        limits.retained_faces,
    )?;
    check_capacity(
        Resource::Collections,
        ledger.collections.len() as u64,
        u64::from(!ledger.collections.contains(&staged.collection_digest)),
        limits.retained_collections,
    )?;
    check_capacity(
        Resource::Aliases,
        ledger.selections.len() as u64,
        staged.new_policies.len() as u64,
        limits.retained_selections,
    )?;
    check_capacity(
        Resource::InstantiatedPairs,
        ledger.instantiated.len() as u64,
        staged.new_pairs.len() as u64,
        limits.instantiated_pairs,
    )?;
    Ok(())
}

/// One [`FontRegistration`] holding only new faces and newly needed
/// policies; reused faces are referenced by their retained IDs. Lookups run
/// pre-commit, so an invariant break is an [`RegistrationError::Internal`]
/// error that leaves everything unchanged, never a post-commit panic.
fn build_registration(
    ledger: &Ledger,
    staged: &Staged,
) -> Result<FontRegistration, RegistrationError> {
    let mut faces: Vec<fontdb::FaceInfo> = Vec::with_capacity(staged.new_faces.len());
    let mut added_index: BTreeMap<FaceKey, usize> = BTreeMap::new();
    for (position, (face_key, _, info)) in staged.new_faces.iter().enumerate() {
        faces.push(info.clone());
        added_index.insert(*face_key, position);
    }
    let mut policies: Vec<PinnedFontPolicy> = Vec::with_capacity(staged.new_policies.len());
    for (_digest, policy) in &staged.new_policies {
        let mut groups = Vec::with_capacity(policy.groups.len());
        for group in &policy.groups {
            let mut references = Vec::with_capacity(group.len());
            for face in group {
                let reference =
                    match ledger.faces.get(face) {
                        Some(record) => PinnedFaceRef::Existing(record.id),
                        None => {
                            let position = added_index.get(face).copied().ok_or(
                                RegistrationError::Internal("staged face missing from added index"),
                            )?;
                            PinnedFaceRef::Added(position)
                        }
                    };
                references.push(reference);
            }
            groups.push(references);
        }
        policies.push(PinnedFontPolicy {
            alias: policy.alias.clone(),
            groups,
            weight: fontdb::Weight(policy.weight),
        });
    }
    Ok(FontRegistration { faces, policies })
}

fn intern_alias(ledger: &mut Ledger, alias: String) -> &'static str {
    match ledger.alias_interns.entry(alias) {
        // A reused alias keeps its original pointer; only the first
        // registration of an alias leaks one small string, bounded by the
        // selection cap.
        Entry::Occupied(occupied) => occupied.get(),
        Entry::Vacant(vacant) => {
            let leaked: &'static str = Box::leak(vacant.key().clone().into_boxed_str());
            vacant.insert(leaked)
        }
    }
}

/// Publish the staged state after the renderer committed. Only infallible
/// map inserts and bounded leaks happen here.
fn publish(ledger: &mut Ledger, staged: Staged, added_faces: Vec<fontdb::ID>) {
    for (key, bytes) in staged.new_sources {
        ledger.retained_bytes += bytes.len() as u64;
        ledger.sources.insert(key, bytes);
    }
    for ((key, mut record, _), id) in staged.new_faces.into_iter().zip(added_faces) {
        record.id = id;
        ledger.faces.insert(key, record);
    }
    ledger.collections.insert(staged.collection_digest);
    for (digest, policy) in staged.new_policies {
        intern_alias(ledger, policy.alias.clone());
        ledger.selections.insert(digest, policy);
    }
    for pair in staged.new_pairs {
        ledger.instantiated.insert(pair);
    }
}

/// One receipt-ready typography selection, fully resolved pre-commit.
struct ReceiptPlan {
    key: String,
    alias: String,
    weight: u16,
    style: Style,
    stretch: Stretch,
    evidence: Arc<SelectionEvidence>,
    owned: OwnedSelection,
}

/// One receipt-ready icon selection, fully resolved pre-commit.
struct ReceiptIcon {
    key: String,
    alias: String,
    weight: u16,
    evidence: Arc<SelectionEvidence>,
    owned: OwnedSelection,
    glyphs: Arc<BTreeMap<String, char>>,
}

/// The ordered owned faces for a selection's eligible groups, sharing the
/// ledger's retained source allocations.
fn owned_groups(
    bytes_by_source: &BTreeMap<SourceKey, Arc<[u8]>>,
    groups: &[Vec<FaceKey>],
    evidences: &[Vec<FaceEvidence>],
) -> Result<Vec<Vec<OwnedFace>>, RegistrationError> {
    groups
        .iter()
        .zip(evidences)
        .map(|(group, group_evidences)| {
            group
                .iter()
                .zip(group_evidences)
                .map(|(face, evidence)| {
                    let bytes = bytes_by_source
                        .get(&SourceKey {
                            digest: face.source,
                            len: face.len,
                        })
                        .cloned()
                        .ok_or(RegistrationError::Internal(
                            "selection face source not retained or staged",
                        ))?;
                    Ok(OwnedFace {
                        bytes,
                        index: face.index,
                        evidence: evidence.clone(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect()
}

/// Resolve everything the receipt needs before the renderer transaction:
/// alias strings, evidence and the owned face views. All fallible lookups
/// happen here, against the ledger and the staged state, so a returned
/// error leaves everything unchanged; after the commit only infallible
/// construction remains.
fn prepare_receipt(
    ledger: &Ledger,
    staged: &Staged,
    plans: Vec<SelectionPlan>,
    icon_plans: Vec<IconPlan>,
) -> Result<(Vec<ReceiptPlan>, Vec<ReceiptIcon>), RegistrationError> {
    // The canonical source allocations: retained ledger sources first, then
    // this batch's staged additions. Owned faces share these Arcs, so an
    // identical re-registration keeps one allocation.
    let mut bytes_by_source: BTreeMap<SourceKey, Arc<[u8]>> = ledger.sources.clone();
    bytes_by_source.extend(staged.new_sources.iter().cloned());

    let mut fonts = Vec::with_capacity(plans.len());
    for plan in plans {
        let policy = staged
            .policies
            .get(&plan.digest)
            .ok_or(RegistrationError::Internal(
                "selection policy missing from staging",
            ))?;
        let evidence = Arc::new(SelectionEvidence {
            declared: plan.declared,
            chosen_group: plan.chosen_group,
            family: plan.family,
            groups: plan.evidences.clone(),
            requested_weight: plan.requested,
            effective_weight: plan.effective,
            substitution: plan.substitution,
            skipped: plan.skipped,
            style: plan.style,
            stretch: plan.stretch,
        });
        let owned = OwnedSelection {
            inner: Arc::new(OwnedSelectionInner {
                weight: plan.effective,
                groups: owned_groups(&bytes_by_source, &plan.groups, &plan.evidences)?,
            }),
        };
        fonts.push(ReceiptPlan {
            key: plan.key,
            alias: policy.alias.clone(),
            weight: plan.effective,
            style: plan.style,
            stretch: plan.stretch,
            evidence,
            owned,
        });
    }
    let mut icons = Vec::with_capacity(icon_plans.len());
    for plan in icon_plans {
        let policy = staged
            .policies
            .get(&plan.digest)
            .ok_or(RegistrationError::Internal(
                "icon policy missing from staging",
            ))?;
        let face_evidence = FaceEvidence {
            source: digest_hex(&plan.face.source),
            bytes: plan.face.len,
            index: plan.face.index,
        };
        let evidence = Arc::new(SelectionEvidence {
            declared: vec![plan.family.clone()],
            chosen_group: 0,
            family: plan.intrinsic,
            groups: vec![vec![face_evidence.clone()]],
            requested_weight: plan.weight,
            effective_weight: plan.weight,
            substitution: None,
            skipped: Vec::new(),
            style: Style::Normal,
            stretch: Stretch::Normal,
        });
        let owned = OwnedSelection {
            inner: Arc::new(OwnedSelectionInner {
                weight: plan.weight,
                groups: owned_groups(&bytes_by_source, &[vec![plan.face]], &[vec![face_evidence]])?,
            }),
        };
        icons.push(ReceiptIcon {
            key: plan.key,
            alias: policy.alias.clone(),
            weight: plan.weight,
            evidence,
            owned,
            glyphs: plan.glyphs,
        });
    }
    Ok((fonts, icons))
}

/// Assemble the receipt after publication. Everything was resolved
/// pre-commit; the only ledger access here is idempotent alias interning,
/// which returns the existing pointer without indexing.
#[allow(clippy::too_many_arguments)]
fn build_receipt(
    ledger: &mut Ledger,
    parsed: &[ParsedSource],
    collection_id: CollectionId,
    usage_before: RegistryUsage,
    renderer_version_before: u32,
    renderer_version_after: u32,
    fonts: Vec<ReceiptPlan>,
    icons: Vec<ReceiptIcon>,
    added_sources: usize,
    added_faces: usize,
    reused_faces: usize,
    policies_added: usize,
    policies_reused: usize,
) -> Registration {
    let fonts = fonts
        .into_iter()
        .map(|plan| {
            let alias = intern_alias(ledger, plan.alias);
            let font = Font {
                family: Family::Name(alias),
                weight: Weight::Numeric(plan.weight),
                stretch: plan.stretch,
                style: plan.style,
            };
            (
                plan.key,
                Selection {
                    font,
                    evidence: plan.evidence,
                    owned: plan.owned,
                },
            )
        })
        .collect();
    let icons = icons
        .into_iter()
        .map(|plan| {
            let alias = intern_alias(ledger, plan.alias);
            let font = Font {
                family: Family::Name(alias),
                weight: Weight::Numeric(plan.weight),
                ..Font::DEFAULT
            };
            (
                plan.key,
                IconSelection {
                    selection: Selection {
                        font,
                        evidence: plan.evidence,
                        owned: plan.owned,
                    },
                    glyphs: plan.glyphs,
                },
            )
        })
        .collect();
    let sources = parsed
        .iter()
        .map(|source| SourceEvidence {
            digest: digest_hex(&source.key.digest),
            bytes: source.key.len,
        })
        .collect();
    let usage_after = ledger.usage();
    let evidence = RegistrationEvidence {
        sources,
        added_sources,
        reused_sources: parsed.len() - added_sources,
        added_faces,
        reused_faces,
        policies_added,
        policies_reused,
        renderer_version_before,
        renderer_version_after,
        usage_before,
        usage_after,
    };
    Registration {
        inner: Arc::new(RegistrationInner {
            collection: collection_id,
            fonts,
            icons,
            evidence,
        }),
    }
}

/// The whole batch path, shared by the production singleton and the
/// isolated test backend.
fn register_batch_in(
    owner: &mut impl RendererOwner,
    ledger: &mut Ledger,
    batch: RegistrationBatch,
    limits: &Limits,
) -> Result<Registration, RegistrationError> {
    let usage_before = ledger.usage();
    validate_shape(&batch, limits)?;
    let parsed = parse_sources(&batch, limits)?;
    let collection_digest = collection_digest(&batch.collection, &parsed);
    validate_family_claims(&batch.collection, &parsed)?;

    let mut plans = Vec::with_capacity(batch.selections.len());
    for request in &batch.selections {
        plans.push(resolve_selection(
            request,
            &batch.collection,
            &parsed,
            &collection_digest,
            limits,
        )?);
    }
    let mut icon_plans = Vec::with_capacity(batch.icons.len());
    for request in &batch.icons {
        icon_plans.push(resolve_icon(
            request,
            &batch.collection,
            &parsed,
            &collection_digest,
        )?);
    }

    let staged = stage(ledger, &parsed, collection_digest, &plans, &icon_plans)?;
    preflight_capacity(ledger, &staged, limits)?;
    let registration = build_registration(ledger, &staged)?;
    let (fonts, icons) = prepare_receipt(ledger, &staged, plans, icon_plans)?;

    let reused_faces = staged
        .policies
        .values()
        .flat_map(|policy| policy.groups.iter().flatten())
        .collect::<BTreeSet<_>>()
        .len()
        - staged.new_faces.len();
    let policies_total = staged.policies.len();
    let added_sources = staged.new_sources.len();
    let added_faces = staged.new_faces.len();
    let policies_added = staged.new_policies.len();

    let renderer_version_before = owner.version();
    if renderer_version_before == u32::MAX {
        // The iced wrapper cannot represent another registration once its
        // version saturates; report it before the transaction instead of
        // reaching the wrapper's own overflow handling.
        return Err(RegistrationError::RendererVersionExhausted);
    }
    let result = owner
        .register(registration)
        .map_err(RegistrationError::Renderer)?;
    let renderer_version_after = owner.version();
    // The seam stages one face per committed ID infallibly, so the committed
    // count must equal the staged count. A violation would leave the ledger
    // unable to record correct IDs; it is a seam-contract break with no
    // recoverable outcome, so it is asserted rather than returned.
    assert_eq!(
        result.added_faces.len(),
        added_faces,
        "font seam violated its contract: one committed ID per staged face"
    );

    publish(ledger, staged, result.added_faces);
    let collection_id = CollectionId {
        digest: digest_hex(&collection_digest),
    };
    Ok(build_receipt(
        ledger,
        &parsed,
        collection_id,
        usage_before,
        renderer_version_before,
        renderer_version_after,
        fonts,
        icons,
        added_sources,
        added_faces,
        reused_faces,
        policies_added,
        policies_total - policies_added,
    ))
}

/// An isolated renderer for tests: a private cosmic font system plus the
/// version bump semantics of iced's wrapper.
#[cfg(test)]
struct IsolatedRenderer {
    raw: cosmic_text::FontSystem,
    version: u32,
}

#[cfg(test)]
impl RendererOwner for IsolatedRenderer {
    fn register(
        &mut self,
        registration: FontRegistration,
    ) -> Result<FontRegistrationResult, FontRegistrationError> {
        let result = self.raw.register_fonts(registration)?;
        if !result.added_faces.is_empty() || result.policies_added > 0 {
            self.version += 1;
        }
        Ok(result)
    }

    // Test-only owner: the isolated counter is the version, asserted
    // numerically by the tests.
    fn version(&self) -> u32 {
        self.version
    }
}

/// A renderer that rejects everything, to prove the registry publishes
/// nothing when the transaction itself fails.
#[cfg(test)]
struct RejectingOwner;

#[cfg(test)]
impl RendererOwner for RejectingOwner {
    fn register(
        &mut self,
        _registration: FontRegistration,
    ) -> Result<FontRegistrationResult, FontRegistrationError> {
        Err(FontRegistrationError::TooManyFaces { limit: 0 })
    }

    fn version(&self) -> u32 {
        0
    }
}

/// A renderer whose version is already saturated, to prove the registry
/// preflights the wrapper's overflow condition before the transaction.
#[cfg(test)]
struct MaxVersionOwner;

#[cfg(test)]
impl RendererOwner for MaxVersionOwner {
    fn register(
        &mut self,
        _registration: FontRegistration,
    ) -> Result<FontRegistrationResult, FontRegistrationError> {
        // Never reached: the registry must refuse before committing.
        Err(FontRegistrationError::TooManyFaces { limit: 0 })
    }

    fn version(&self) -> u32 {
        u32::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTER_VARIABLE: &[u8] =
        include_bytes!("../../../../vendor/font/Inter-VariableFont_opsz,wght.ttf");
    const FIRA: &[u8] = iced_graphics::text::FIRA_SANS_REGULAR;
    const NOTO_SANS: &[u8] =
        include_bytes!("../../../../vendor/cosmic-text/fonts/NotoSans-Regular.ttf");
    const NOTO_ARABIC: &[u8] =
        include_bytes!("../../../../vendor/cosmic-text/fonts/NotoSansArabic.ttf");
    const INTER_REGULAR: &[u8] =
        include_bytes!("../../../../vendor/cosmic-text/fonts/Inter-Regular.ttf");

    fn isolated() -> (IsolatedRenderer, Ledger) {
        (
            IsolatedRenderer {
                raw: cosmic_text::FontSystem::new_with_locale_and_db(
                    "en-US".into(),
                    fontdb::Database::new(),
                ),
                version: 0,
            },
            Ledger::default(),
        )
    }

    fn blob(bytes: &'static [u8]) -> FontBlob {
        FontBlob {
            bytes: Arc::from(bytes),
        }
    }

    fn group(name: &str, faces: Vec<SourceFace>) -> FamilyGroup {
        FamilyGroup {
            name: name.into(),
            faces,
        }
    }

    fn selection(key: &str, families: Vec<String>, weight: u16) -> SelectionRequest {
        SelectionRequest {
            key: key.into(),
            families,
            requested_weight: weight,
            weight_policy: WeightPolicy::Exact,
            style: Style::Normal,
            stretch: Stretch::Normal,
        }
    }

    fn batch_for(bytes: &'static [u8], family: &str, role: &str, weight: u16) -> RegistrationBatch {
        RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(bytes)],
                families: vec![group(
                    family,
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::from([(role.into(), vec![family.into()])]),
                icons: Vec::new(),
            },
            selections: vec![selection(role, vec![family.into()], weight)],
            icons: Vec::new(),
        }
    }

    fn alias_of(selection: &Selection) -> &'static str {
        match selection.font().family {
            Family::Name(name) => name,
            _ => panic!("registry selections must name an alias"),
        }
    }

    fn shape(
        raw: &mut cosmic_text::FontSystem,
        text: &str,
        alias: &str,
        shaping: cosmic_text::Shaping,
    ) -> Vec<(fontdb::ID, fontdb::Weight, u16, f32)> {
        let metrics = cosmic_text::Metrics::new(16.0, 20.0);
        let mut buffer = cosmic_text::Buffer::new(raw, metrics);
        let attrs = cosmic_text::Attrs::new().family(cosmic_text::Family::Name(alias));
        let glyphs = {
            let mut buffer = buffer.borrow_with(raw);
            buffer.set_size(Some(300.0), Some(100.0));
            buffer.set_text(text, &attrs, shaping, None);
            buffer.shape_until_scroll(true);
            buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .map(|glyph| (glyph.font_id, glyph.font_weight, glyph.glyph_id, glyph.w))
                .collect::<Vec<_>>()
        };
        glyphs
    }

    fn snapshot(renderer: &IsolatedRenderer, ledger: &Ledger) -> (u32, usize, RegistryUsage) {
        (renderer.version, renderer.raw.db().len(), ledger.usage())
    }

    #[test]
    fn registered_selection_shapes_basic_and_advanced_from_the_same_bytes() {
        let (mut renderer, mut ledger) = isolated();
        let registration = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "ui", 400),
            &Limits::PROCESS,
        )
        .unwrap();
        let selection = registration.font("ui").unwrap();
        assert_eq!(selection.font().weight, Weight::Numeric(400));
        let alias = alias_of(selection);
        assert!(alias.starts_with("mixos-pinned-"));
        let evidence = selection.evidence();
        assert_eq!(evidence.declared, vec!["Inter"]);
        assert_eq!(evidence.chosen_group, 0);
        assert_eq!(evidence.family, "Inter");
        assert_eq!(evidence.requested_weight, 400);
        assert_eq!(evidence.effective_weight, 400);
        assert!(evidence.substitution.is_none());
        assert!(evidence.skipped.is_empty());
        let face_key = FaceKey {
            source: *blake3::hash(INTER_VARIABLE).as_bytes(),
            len: INTER_VARIABLE.len() as u64,
            index: 0,
        };
        let added_id = ledger.faces[&face_key].id;
        for shaping in [cosmic_text::Shaping::Basic, cosmic_text::Shaping::Advanced] {
            let glyphs = shape(&mut renderer.raw, "Hello MixOS", alias, shaping);
            assert!(!glyphs.is_empty());
            for (id, _, glyph_id, _) in &glyphs {
                assert_eq!(
                    *id, added_id,
                    "{shaping:?}: only the registered face may shape"
                );
                assert_ne!(*glyph_id, 0);
            }
        }
    }

    #[test]
    fn declared_fallback_advances_and_absent_chains_fail() {
        let (mut renderer, mut ledger) = isolated();
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(INTER_VARIABLE)],
                families: vec![group(
                    "Inter",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![selection("ui", vec!["Missing".into(), "Inter".into()], 400)],
            icons: Vec::new(),
        };
        let registration =
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS).unwrap();
        let evidence = registration.font("ui").unwrap().evidence();
        assert_eq!(evidence.chosen_group, 1);
        assert_eq!(evidence.family, "Inter");
        assert!(evidence.skipped.is_empty());
        assert_eq!(
            evidence.groups.len(),
            1,
            "absent families leave no empty group"
        );
        assert_eq!(evidence.groups[0].len(), 1);

        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(INTER_VARIABLE)],
                families: vec![group(
                    "Inter",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![selection("ui", vec!["A".into(), "B".into()], 400)],
            icons: Vec::new(),
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS),
            Err(RegistrationError::AllFamiliesAbsent { key }) if key == "ui"
        ));
    }

    #[test]
    fn static_weight_is_exact_unless_explicitly_substituted() {
        let (mut renderer, mut ledger) = isolated();
        let before = snapshot(&renderer, &ledger);
        // Fira Sans is static at 400; an exact 700 request must fail.
        let err = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(FIRA, "Fira Sans", "ui", 700),
            &Limits::PROCESS,
        )
        .unwrap_err();
        assert!(matches!(
            &err,
            RegistrationError::UnsupportedWeight { key, family, weight: 700 }
                if key == "ui" && family == "Fira Sans"
        ));
        assert_eq!(
            snapshot(&renderer, &ledger),
            before,
            "failed batch must leave everything unchanged: {err}"
        );

        // An explicit substitution authorises the effective 400.
        let mut batch = batch_for(FIRA, "Fira Sans", "ui", 700);
        batch.selections[0].weight_policy = WeightPolicy::Substitute {
            effective: 400,
            reason: "no bold face in packaged set".into(),
        };
        let registration =
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS).unwrap();
        let selection = registration.font("ui").unwrap();
        let evidence = selection.evidence();
        assert_eq!(evidence.requested_weight, 700);
        assert_eq!(evidence.effective_weight, 400);
        assert_eq!(
            evidence.substitution.as_ref().unwrap().reason,
            "no bold face in packaged set"
        );
        assert_eq!(selection.font().weight, Weight::Numeric(400));
    }

    #[test]
    fn variable_weight_is_exact_numeric_and_advances_metrics() {
        let (mut renderer, mut ledger) = isolated();
        let light = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "light", 350),
            &Limits::PROCESS,
        )
        .unwrap();
        let heavy = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "heavy", 650),
            &Limits::PROCESS,
        )
        .unwrap();
        let light_alias = alias_of(light.font("light").unwrap());
        let heavy_alias = alias_of(heavy.font("heavy").unwrap());
        assert_ne!(light_alias, heavy_alias);
        assert_eq!(
            light.font("light").unwrap().evidence().requested_weight,
            350
        );
        assert_eq!(
            light.font("light").unwrap().evidence().effective_weight,
            350
        );
        for shaping in [cosmic_text::Shaping::Basic, cosmic_text::Shaping::Advanced] {
            let light_width: f32 = shape(&mut renderer.raw, "Hello", light_alias, shaping)
                .iter()
                .map(|(_, _, _, w)| w)
                .sum();
            let heavy_width: f32 = shape(&mut renderer.raw, "Hello", heavy_alias, shaping)
                .iter()
                .map(|(_, _, _, w)| w)
                .sum();
            assert!(
                light_width < heavy_width,
                "{shaping:?}: heavier weight must advance wider ({light_width} >= {heavy_width})"
            );
        }
    }

    #[test]
    fn late_failures_leave_renderer_and_registry_untouched() {
        let (mut renderer, mut ledger) = isolated();
        register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "ui", 400),
            &Limits::PROCESS,
        )
        .unwrap();
        let before = snapshot(&renderer, &ledger);

        // Valid roles, then the final icon fails on its last name.
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Fira Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: vec![IconCatalogue {
                    family: "Fira Sans".into(),
                    style: "default".into(),
                    face: SourceFace {
                        source: 0,
                        index: 0,
                    },
                    glyphs: BTreeMap::from([("home".into(), 'a')]),
                }],
            },
            selections: vec![selection("ui", vec!["Fira Sans".into()], 400)],
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "Fira Sans".into(),
                style: "default".into(),
                weight: 400,
                required_names: vec!["home".into(), "missing".into()],
            }],
        };
        let err =
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS).unwrap_err();
        assert!(matches!(
            err,
            RegistrationError::IconNameMissing { name, .. } if name == "missing"
        ));
        assert_eq!(snapshot(&renderer, &ledger), before);

        // A capacity failure is also all-or-nothing.
        let tight = Limits {
            retained_faces: 0,
            ..Limits::PROCESS
        };
        let err = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(FIRA, "Fira Sans", "other", 400),
            &tight,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RegistrationError::Capacity {
                resource: Resource::Faces,
                ..
            }
        ));
        assert_eq!(snapshot(&renderer, &ledger), before);

        // A renderer rejection publishes nothing either.
        let mut rejecting = RejectingOwner;
        let mut other_ledger = Ledger::default();
        let err = register_batch_in(
            &mut rejecting,
            &mut other_ledger,
            batch_for(INTER_VARIABLE, "Inter", "ui", 400),
            &Limits::PROCESS,
        )
        .unwrap_err();
        assert!(matches!(err, RegistrationError::Renderer(_)));
        assert_eq!(other_ledger.usage(), RegistryUsage::default());
    }

    #[test]
    fn identical_and_shared_selections_do_not_grow_the_registry() {
        let (mut renderer, mut ledger) = isolated();
        let first = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "ui", 400),
            &Limits::PROCESS,
        )
        .unwrap();
        let evidence = first.evidence();
        assert_eq!(evidence.added_sources, 1);
        assert_eq!(evidence.added_faces, 1);
        assert_eq!(evidence.policies_added, 1);
        assert_eq!(
            evidence.renderer_version_after,
            evidence.renderer_version_before + 1,
            "one transaction, one numeric version step"
        );

        let again = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "ui", 400),
            &Limits::PROCESS,
        )
        .unwrap();
        let evidence = again.evidence();
        assert_eq!(evidence.added_sources, 0);
        assert_eq!(evidence.reused_sources, 1);
        assert_eq!(evidence.added_faces, 0);
        assert_eq!(evidence.reused_faces, 1);
        assert_eq!(evidence.policies_added, 0);
        assert_eq!(evidence.policies_reused, 1);
        assert_eq!(evidence.usage_before, evidence.usage_after);
        assert_eq!(
            evidence.renderer_version_after,
            evidence.renderer_version_before
        );
        assert_eq!(
            again.font("ui").unwrap().font(),
            first.font("ui").unwrap().font()
        );

        // Two roles making the exact same selection share one alias.
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Fira Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![
                selection("body", vec!["Fira Sans".into()], 400),
                selection("copy", vec!["Fira Sans".into()], 400),
            ],
            icons: Vec::new(),
        };
        let registration =
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS).unwrap();
        assert_eq!(
            registration.font("body").unwrap().font(),
            registration.font("copy").unwrap().font()
        );
        assert_eq!(
            registration.evidence().policies_added,
            1,
            "one policy serves both roles"
        );
    }

    #[test]
    fn same_family_different_bytes_pins_the_old_paragraph() {
        let (mut renderer, mut ledger) = isolated();
        let old = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "ui", 400),
            &Limits::PROCESS,
        )
        .unwrap();
        let old_alias = alias_of(old.font("ui").unwrap());
        let old_shape = shape(
            &mut renderer.raw,
            "Hello world",
            old_alias,
            cosmic_text::Shaping::Advanced,
        );
        assert!(!old_shape.is_empty());

        // Different bytes that claim the same public family name.
        let fresh = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_REGULAR, "Inter", "body", 400),
            &Limits::PROCESS,
        )
        .unwrap();
        let fresh_alias = alias_of(fresh.font("body").unwrap());
        assert_ne!(old_alias, fresh_alias);

        // The old paragraph re-shapes identically...
        assert_eq!(
            shape(
                &mut renderer.raw,
                "Hello world",
                old_alias,
                cosmic_text::Shaping::Advanced,
            ),
            old_shape
        );

        // ...and the new selection shapes the new bytes.
        let fresh_key = FaceKey {
            source: *blake3::hash(INTER_REGULAR).as_bytes(),
            len: INTER_REGULAR.len() as u64,
            index: 0,
        };
        let fresh_id = ledger.faces[&fresh_key].id;
        for (id, _, glyph_id, _) in shape(
            &mut renderer.raw,
            "Hello world",
            fresh_alias,
            cosmic_text::Shaping::Advanced,
        ) {
            assert_eq!(id, fresh_id);
            assert_ne!(glyph_id, 0);
        }

        // The retained old selection still owns its original bytes; the new
        // selection owns the new ones.
        let old_owned = old.font("ui").unwrap().owned();
        assert_eq!(&old_owned.groups()[0][0].bytes()[..], INTER_VARIABLE);
        let fresh_owned = fresh.font("body").unwrap().owned();
        assert_eq!(&fresh_owned.groups()[0][0].bytes()[..], INTER_REGULAR);
    }

    #[test]
    fn declared_fallback_covers_arabic_and_exhaustion_stays_missing() {
        let (mut renderer, mut ledger) = isolated();
        let collection = FontCollection {
            sources: vec![blob(NOTO_SANS), blob(NOTO_ARABIC)],
            families: vec![
                group(
                    "Noto Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                ),
                group(
                    "Noto Sans Arabic",
                    vec![SourceFace {
                        source: 1,
                        index: 0,
                    }],
                ),
            ],
            roles: BTreeMap::new(),
            icons: Vec::new(),
        };
        let with_b = RegistrationBatch {
            collection: collection.clone(),
            selections: vec![selection(
                "ar",
                vec!["Noto Sans".into(), "Noto Sans Arabic".into()],
                400,
            )],
            icons: Vec::new(),
        };
        let registration =
            register_batch_in(&mut renderer, &mut ledger, with_b, &Limits::PROCESS).unwrap();
        let alias = alias_of(registration.font("ar").unwrap());
        let arabic_key = FaceKey {
            source: *blake3::hash(NOTO_ARABIC).as_bytes(),
            len: NOTO_ARABIC.len() as u64,
            index: 0,
        };
        let arabic_id = ledger.faces[&arabic_key].id;
        for shaping in [cosmic_text::Shaping::Basic, cosmic_text::Shaping::Advanced] {
            let glyphs = shape(&mut renderer.raw, "مرحبا", alias, shaping);
            assert!(!glyphs.is_empty());
            for (id, _, glyph_id, _) in &glyphs {
                assert_eq!(*id, arabic_id, "{shaping:?}: declared fallback must cover");
                assert_ne!(*glyph_id, 0);
            }
        }

        // The owned view preserves the declared order and effective weight.
        let owned = registration.font("ar").unwrap().owned();
        assert_eq!(owned.effective_weight(), 400);
        assert_eq!(owned.groups().len(), 2, "declared order preserved");
        assert_eq!(owned.groups()[0][0].index(), 0);
        assert_eq!(owned.groups()[1][0].index(), 0);
        assert_eq!(&owned.groups()[0][0].bytes()[..], NOTO_SANS);
        assert_eq!(&owned.groups()[1][0].bytes()[..], NOTO_ARABIC);

        let without = RegistrationBatch {
            collection,
            selections: vec![selection("la", vec!["Noto Sans".into()], 400)],
            icons: Vec::new(),
        };
        let registration =
            register_batch_in(&mut renderer, &mut ledger, without, &Limits::PROCESS).unwrap();
        let alias = alias_of(registration.font("la").unwrap());
        for shaping in [cosmic_text::Shaping::Basic, cosmic_text::Shaping::Advanced] {
            let glyphs = shape(&mut renderer.raw, "مرحبا", alias, shaping);
            assert!(!glyphs.is_empty());
            for (_, _, glyph_id, _) in &glyphs {
                assert_eq!(
                    *glyph_id, 0,
                    "{shaping:?}: exhausted coverage must remain missing"
                );
            }
        }
    }

    #[test]
    fn icon_selections_require_declared_names_and_glyphs() {
        let (mut renderer, mut ledger) = isolated();
        let collection = FontCollection {
            sources: vec![blob(FIRA)],
            families: vec![group(
                "Fira Sans",
                vec![SourceFace {
                    source: 0,
                    index: 0,
                }],
            )],
            roles: BTreeMap::new(),
            icons: vec![IconCatalogue {
                family: "Fira Sans".into(),
                style: "default".into(),
                face: SourceFace {
                    source: 0,
                    index: 0,
                },
                glyphs: BTreeMap::from([("home".into(), 'a'), ("emoji".into(), '\u{1F600}')]),
            }],
        };
        let batch = RegistrationBatch {
            collection: collection.clone(),
            selections: Vec::new(),
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "Fira Sans".into(),
                style: "default".into(),
                weight: 400,
                required_names: vec!["home".into()],
            }],
        };
        let registration =
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS).unwrap();
        let (glyph, font) = registration.icon("icons", "home").unwrap();
        assert_eq!(glyph, 'a');
        assert_eq!(font.weight, Weight::Numeric(400));
        assert_eq!(registration.icon("icons", "unknown"), None);
        assert_eq!(registration.icon("other", "home"), None);
        let alias = match font.family {
            Family::Name(name) => name,
            _ => panic!("icon selections must name an alias"),
        };
        let glyphs = shape(&mut renderer.raw, "a", alias, cosmic_text::Shaping::Basic);
        assert_eq!(glyphs.len(), 1);
        assert_ne!(glyphs[0].2, 0);

        let missing_style = RegistrationBatch {
            collection: collection.clone(),
            selections: Vec::new(),
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "Fira Sans".into(),
                style: "rounded".into(),
                weight: 400,
                required_names: vec!["home".into()],
            }],
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, missing_style, &Limits::PROCESS),
            Err(RegistrationError::IconCatalogueNotFound { .. })
        ));
        let missing_name = RegistrationBatch {
            collection: collection.clone(),
            selections: Vec::new(),
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "Fira Sans".into(),
                style: "default".into(),
                weight: 400,
                required_names: vec!["nope".into()],
            }],
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, missing_name, &Limits::PROCESS),
            Err(RegistrationError::IconNameMissing { .. })
        ));
        let uncovered = RegistrationBatch {
            collection,
            selections: Vec::new(),
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "Fira Sans".into(),
                style: "default".into(),
                weight: 400,
                required_names: vec!["emoji".into()],
            }],
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, uncovered, &Limits::PROCESS),
            Err(RegistrationError::IconGlyphMissing {
                glyph: '\u{1F600}',
                ..
            })
        ));
    }

    #[test]
    fn source_bounds_are_checked_before_parse() {
        let (mut renderer, mut ledger) = isolated();
        let ttc = |faces: u32| {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"ttcf");
            bytes.extend_from_slice(&[0, 1, 0, 0]);
            bytes.extend_from_slice(&faces.to_be_bytes());
            bytes
        };
        let batch_with = |bytes: Vec<u8>| RegistrationBatch {
            collection: FontCollection {
                sources: vec![FontBlob {
                    bytes: Arc::from(bytes),
                }],
                families: vec![group(
                    "x",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![selection("ui", vec!["x".into()], 400)],
            icons: Vec::new(),
        };
        assert!(matches!(
            register_batch_in(
                &mut renderer,
                &mut ledger,
                batch_with(ttc(0)),
                &Limits::PROCESS
            ),
            Err(RegistrationError::SourceUnparsable { source: 0 })
        ));
        assert!(matches!(
            register_batch_in(
                &mut renderer,
                &mut ledger,
                batch_with(ttc(700)),
                &Limits::PROCESS
            ),
            Err(RegistrationError::TooManyFacesInSource {
                source: 0,
                faces: 700,
                ..
            })
        ));
        assert!(matches!(
            register_batch_in(
                &mut renderer,
                &mut ledger,
                batch_with(vec![0u8; 64]),
                &Limits::PROCESS
            ),
            Err(RegistrationError::SourceUnparsable { source: 0 })
        ));
        let tight = Limits {
            source_bytes: 16,
            ..Limits::PROCESS
        };
        assert!(matches!(
            register_batch_in(
                &mut renderer,
                &mut ledger,
                batch_with(vec![0u8; 17]),
                &tight
            ),
            Err(RegistrationError::SourceTooLarge {
                source: 0,
                bytes: 17,
                ..
            })
        ));
    }

    #[test]
    fn capacity_dimensions_fail_honestly() {
        let (mut renderer, mut ledger) = isolated();
        let tight_bytes = Limits {
            retained_bytes: 10,
            ..Limits::PROCESS
        };
        let err = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(FIRA, "Fira Sans", "ui", 400),
            &tight_bytes,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RegistrationError::Capacity {
                resource: Resource::RetainedBytes,
                have: 0,
                ..
            }
        ));

        let two_source_batch = |weight: u16| RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA), blob(INTER_VARIABLE)],
                families: vec![
                    group(
                        "Fira Sans",
                        vec![SourceFace {
                            source: 0,
                            index: 0,
                        }],
                    ),
                    group(
                        "Inter",
                        vec![SourceFace {
                            source: 1,
                            index: 0,
                        }],
                    ),
                ],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![
                selection("a", vec!["Fira Sans".into()], 400),
                selection("b", vec!["Inter".into()], weight),
            ],
            icons: Vec::new(),
        };
        let tight_faces = Limits {
            retained_faces: 1,
            ..Limits::PROCESS
        };
        let err = register_batch_in(
            &mut renderer,
            &mut ledger,
            two_source_batch(400),
            &tight_faces,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RegistrationError::Capacity {
                resource: Resource::Faces,
                have: 0,
                need: 2,
                ..
            }
        ));
        let tight_aliases = Limits {
            retained_selections: 1,
            ..Limits::PROCESS
        };
        let err = register_batch_in(
            &mut renderer,
            &mut ledger,
            two_source_batch(500),
            &tight_aliases,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RegistrationError::Capacity {
                resource: Resource::Aliases,
                need: 2,
                ..
            }
        ));
        let tight_pairs = Limits {
            instantiated_pairs: 1,
            ..Limits::PROCESS
        };
        let err = register_batch_in(
            &mut renderer,
            &mut ledger,
            two_source_batch(500),
            &tight_pairs,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            RegistrationError::Capacity {
                resource: Resource::InstantiatedPairs,
                need: 2,
                ..
            }
        ));
    }

    #[test]
    fn name_metadata_and_shape_bounds_are_enforced() {
        let (mut renderer, mut ledger) = isolated();
        let long_family = "x".repeat(257);
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    &long_family,
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![selection("ui", vec![long_family], 400)],
            icons: Vec::new(),
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS),
            Err(RegistrationError::NameTooLong {
                what: "family",
                len: 257,
                ..
            })
        ));

        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Fira Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![selection(&"k".repeat(97), vec!["Fira Sans".into()], 400)],
            icons: Vec::new(),
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS),
            Err(RegistrationError::NameTooLong {
                what: "selection key",
                len: 97,
                ..
            })
        ));

        let mut heavy = batch_for(FIRA, "Fira Sans", "ui", 400);
        heavy.selections[0].requested_weight = 1001;
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, heavy, &Limits::PROCESS),
            Err(RegistrationError::WeightOutOfRange { weight: 1001 })
        ));

        let tight = Limits {
            selections_per_batch: 1,
            ..Limits::PROCESS
        };
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Fira Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![
                selection("a", vec!["Fira Sans".into()], 400),
                selection("b", vec!["Fira Sans".into()], 400),
            ],
            icons: Vec::new(),
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &tight),
            Err(RegistrationError::TooManySelections { limit: 1, have: 2 })
        ));

        let tight_meta = Limits {
            metadata_bytes: 8,
            ..Limits::PROCESS
        };
        let batch = batch_for(FIRA, "Fira Sans", "ui", 400);
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &tight_meta),
            Err(RegistrationError::MetadataTooLarge { .. })
        ));

        let glyphs: BTreeMap<String, char> = (0..16_385)
            .map(|index| (format!("n{index}"), 'a'))
            .collect();
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Fira Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: vec![IconCatalogue {
                    family: "Fira Sans".into(),
                    style: "default".into(),
                    face: SourceFace {
                        source: 0,
                        index: 0,
                    },
                    glyphs,
                }],
            },
            selections: Vec::new(),
            icons: Vec::new(),
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS),
            Err(RegistrationError::TooManyIconNames {
                limit: 16_384,
                have: 16_385
            })
        ));
    }

    #[test]
    fn mismatched_family_claims_are_rejected() {
        let (mut renderer, mut ledger) = isolated();
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Wrong Family",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![selection("ui", vec!["Wrong Family".into()], 400)],
            icons: Vec::new(),
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS),
            Err(RegistrationError::FamilyClaimMismatch { family, index: 0, .. })
                if family == "Wrong Family"
        ));
    }

    #[test]
    fn collection_identity_and_usage_are_reported() {
        let (mut renderer, mut ledger) = isolated();
        let registration = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "ui", 400),
            &Limits::PROCESS,
        )
        .unwrap();
        let id = registration.collection_id();
        assert_eq!(id.as_str().len(), 64);
        assert!(
            id.as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(
            registration.evidence().usage_after.retained_bytes,
            INTER_VARIABLE.len() as u64
        );
        let usage = ledger.usage();
        assert_eq!(usage, registration.evidence().usage_after);
        assert_eq!(usage.sources, 1);
        assert_eq!(usage.faces, 1);
        assert_eq!(usage.collections, 1);
        assert_eq!(usage.aliases, 1);
        assert_eq!(usage.instantiated_pairs, 1);
    }

    #[test]
    fn owned_selections_share_source_allocations_across_registrations() {
        let (mut renderer, mut ledger) = isolated();
        let batch_with = |bytes: Arc<[u8]>| RegistrationBatch {
            collection: FontCollection {
                sources: vec![FontBlob { bytes }],
                families: vec![group(
                    "Inter",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![selection("ui", vec!["Inter".into()], 400)],
            icons: Vec::new(),
        };
        let original: Arc<[u8]> = Arc::from(INTER_VARIABLE);
        let registration = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_with(original.clone()),
            &Limits::PROCESS,
        )
        .unwrap();
        let owned = registration.font("ui").unwrap().owned();
        assert_eq!(owned.effective_weight(), 400);
        assert_eq!(owned.groups().len(), 1);
        assert!(Arc::ptr_eq(&owned.groups()[0][0].bytes(), &original));

        // An identical batch with a fresh allocation reuses the retained
        // one: the ledger's canonical source is the first registration's.
        let fresh: Arc<[u8]> = Arc::from(INTER_VARIABLE);
        let again = register_batch_in(
            &mut renderer,
            &mut ledger,
            batch_with(fresh.clone()),
            &Limits::PROCESS,
        )
        .unwrap();
        let again_owned = again.font("ui").unwrap().owned();
        assert!(Arc::ptr_eq(&again_owned.groups()[0][0].bytes(), &original));
        assert!(!Arc::ptr_eq(&again_owned.groups()[0][0].bytes(), &fresh));
    }

    #[test]
    fn icon_catalogue_family_claims_are_validated() {
        let (mut renderer, mut ledger) = isolated();
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Fira Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: vec![IconCatalogue {
                    family: "Wrong Icons".into(),
                    style: "default".into(),
                    face: SourceFace {
                        source: 0,
                        index: 0,
                    },
                    glyphs: BTreeMap::from([("home".into(), 'a')]),
                }],
            },
            selections: Vec::new(),
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "Wrong Icons".into(),
                style: "default".into(),
                weight: 400,
                required_names: vec!["home".into()],
            }],
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS),
            Err(RegistrationError::FamilyClaimMismatch { family, index: 0, .. })
                if family == "Wrong Icons"
        ));
    }

    #[test]
    fn a_new_face_of_a_retained_source_uses_the_canonical_renderer_allocation() {
        let (mut renderer, mut ledger) = isolated();
        let canonical: Arc<[u8]> = Arc::from(INTER_VARIABLE);
        let first = RegistrationBatch {
            collection: FontCollection {
                sources: vec![
                    blob(FIRA),
                    FontBlob {
                        bytes: canonical.clone(),
                    },
                ],
                families: vec![
                    group(
                        "Fira Sans",
                        vec![SourceFace {
                            source: 0,
                            index: 0,
                        }],
                    ),
                    group(
                        "Inter",
                        vec![SourceFace {
                            source: 1,
                            index: 0,
                        }],
                    ),
                ],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![selection("ui", vec!["Fira Sans".into()], 400)],
            icons: Vec::new(),
        };
        register_batch_in(&mut renderer, &mut ledger, first, &Limits::PROCESS).unwrap();
        let before_bytes = ledger.usage().retained_bytes;
        let fresh: Arc<[u8]> = Arc::from(INTER_VARIABLE);
        let mut second = batch_for(INTER_VARIABLE, "Inter", "ui", 400);
        second.collection.sources[0].bytes = fresh.clone();
        let registration =
            register_batch_in(&mut renderer, &mut ledger, second, &Limits::PROCESS).unwrap();
        assert_eq!(ledger.usage().retained_bytes, before_bytes);
        let owned = registration.font("ui").unwrap().owned();
        assert!(Arc::ptr_eq(&owned.groups()[0][0].bytes(), &canonical));
        let key = FaceKey {
            source: *blake3::hash(INTER_VARIABLE).as_bytes(),
            len: INTER_VARIABLE.len() as u64,
            index: 0,
        };
        let id = ledger.faces[&key].id;
        let renderer_pointer = renderer
            .raw
            .db()
            .with_face_data(id, |bytes, _| bytes.as_ptr())
            .expect("committed renderer face");
        assert_eq!(renderer_pointer, canonical.as_ptr());
        assert_ne!(renderer_pointer, fresh.as_ptr());
    }

    #[test]
    fn icon_typographic_style_and_stretch_are_refused_before_commit() {
        let (renderer, ledger) = isolated();
        let before = snapshot(&renderer, &ledger);
        let mut batch = batch_for(FIRA, "Fira Sans", "ui", 400);
        batch.collection.icons.push(IconCatalogue {
            family: "Fira Sans".into(),
            style: "rounded".into(),
            face: SourceFace {
                source: 0,
                index: 0,
            },
            glyphs: BTreeMap::from([("home".into(), 'a')]),
        });
        let request = IconSelectionRequest {
            key: "icons".into(),
            family: "Fira Sans".into(),
            style: "rounded".into(),
            weight: 400,
            required_names: vec!["home".into()],
        };
        for (style, stretch) in [
            (fontdb::Style::Italic, fontdb::Stretch::Normal),
            (fontdb::Style::Normal, fontdb::Stretch::Condensed),
        ] {
            let mut parsed = parse_sources(&batch, &Limits::PROCESS).unwrap();
            let digest = collection_digest(&batch.collection, &parsed);
            let face = parsed[0].faces.get_mut(&0).unwrap();
            face.info.style = style;
            face.info.stretch = stretch;
            assert!(matches!(
                resolve_icon(&request, &batch.collection, &parsed, &digest),
                Err(RegistrationError::UnsupportedStyle { .. })
            ));
            assert_eq!(snapshot(&renderer, &ledger), before);
        }
    }

    #[test]
    fn family_lookups_are_case_insensitive() {
        let (mut renderer, mut ledger) = isolated();
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Fira Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: vec![IconCatalogue {
                    family: "Fira Sans".into(),
                    style: "default".into(),
                    face: SourceFace {
                        source: 0,
                        index: 0,
                    },
                    glyphs: BTreeMap::from([("home".into(), 'a')]),
                }],
            },
            selections: vec![selection("ui", vec!["fira sans".into()], 400)],
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "FIRA SANS".into(),
                style: "default".into(),
                weight: 400,
                required_names: vec!["home".into()],
            }],
        };
        let registration =
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS).unwrap();
        // The evidence reports the parsed intrinsic name, not the request's
        // spelling.
        assert_eq!(
            registration.font("ui").unwrap().evidence().family,
            "Fira Sans"
        );
        assert!(registration.icon("icons", "home").is_some());

        // Two catalogues differing only in family case are one catalogue.
        let duplicate = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA)],
                families: vec![group(
                    "Fira Sans",
                    vec![SourceFace {
                        source: 0,
                        index: 0,
                    }],
                )],
                roles: BTreeMap::new(),
                icons: vec![
                    IconCatalogue {
                        family: "Icons".into(),
                        style: "default".into(),
                        face: SourceFace {
                            source: 0,
                            index: 0,
                        },
                        glyphs: BTreeMap::from([("home".into(), 'a')]),
                    },
                    IconCatalogue {
                        family: "icons".into(),
                        style: "default".into(),
                        face: SourceFace {
                            source: 0,
                            index: 0,
                        },
                        glyphs: BTreeMap::from([("home".into(), 'a')]),
                    },
                ],
            },
            selections: Vec::new(),
            icons: Vec::new(),
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, duplicate, &Limits::PROCESS),
            Err(RegistrationError::DuplicateCatalogue { .. })
        ));
    }

    #[test]
    fn renderer_version_exhaustion_is_preflighted() {
        let mut owner = MaxVersionOwner;
        let mut ledger = Ledger::default();
        let err = register_batch_in(
            &mut owner,
            &mut ledger,
            batch_for(INTER_VARIABLE, "Inter", "ui", 400),
            &Limits::PROCESS,
        )
        .unwrap_err();
        assert!(matches!(err, RegistrationError::RendererVersionExhausted));
        assert_eq!(ledger.usage(), RegistryUsage::default());
    }

    #[test]
    fn style_and_stretch_mismatches_are_honest() {
        let (mut renderer, mut ledger) = isolated();
        let mut batch = batch_for(FIRA, "Fira Sans", "ui", 400);
        batch.selections[0].style = Style::Italic;
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS),
            Err(RegistrationError::UnsupportedStyle { key, family, style: Style::Italic, .. })
                if key == "ui" && family == "Fira Sans"
        ));

        // With nothing eligible behind it, a substitution still fails
        // honestly, naming the skipped family.
        let mut batch = batch_for(FIRA, "Fira Sans", "ui", 400);
        batch.selections[0].style = Style::Italic;
        batch.selections[0].weight_policy = WeightPolicy::Substitute {
            effective: 400,
            reason: "no italic face".into(),
        };
        assert!(matches!(
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS),
            Err(RegistrationError::UnsupportedStyle { family, .. }) if family == "Fira Sans"
        ));
    }

    #[test]
    fn substitution_skips_the_ineligible_family_and_keeps_the_declared_closure() {
        let (mut renderer, mut ledger) = isolated();
        // Fira Sans is static at 400, so a 700 request skips it under an
        // explicit substitution; the variable Inter family is eligible and
        // becomes both the primary and the whole policy closure.
        let batch = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(FIRA), blob(INTER_VARIABLE)],
                families: vec![
                    group(
                        "Fira Sans",
                        vec![SourceFace {
                            source: 0,
                            index: 0,
                        }],
                    ),
                    group(
                        "Inter",
                        vec![SourceFace {
                            source: 1,
                            index: 0,
                        }],
                    ),
                ],
                roles: BTreeMap::new(),
                icons: Vec::new(),
            },
            selections: vec![SelectionRequest {
                key: "ui".into(),
                families: vec!["Fira Sans".into(), "Inter".into()],
                requested_weight: 700,
                weight_policy: WeightPolicy::Substitute {
                    effective: 700,
                    reason: "bold face".into(),
                },
                style: Style::Normal,
                stretch: Stretch::Normal,
            }],
            icons: Vec::new(),
        };
        let registration =
            register_batch_in(&mut renderer, &mut ledger, batch, &Limits::PROCESS).unwrap();
        let evidence = registration.font("ui").unwrap().evidence();
        assert_eq!(evidence.skipped, vec!["Fira Sans"]);
        assert_eq!(evidence.chosen_group, 1);
        assert_eq!(evidence.family, "Inter");
        assert_eq!(evidence.groups.len(), 1);
        assert_eq!(evidence.effective_weight, 700);
        let owned = registration.font("ui").unwrap().owned();
        assert_eq!(owned.effective_weight(), 700);
        assert_eq!(owned.groups().len(), 1);
        assert_eq!(&owned.groups()[0][0].bytes()[..], INTER_VARIABLE);
    }
}
