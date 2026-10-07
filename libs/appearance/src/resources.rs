// SPDX-License-Identifier: MIT OR Apache-2.0
//! The central verified resource host for a prepared presentation.
//!
//! [`ResourceHost`] runs exclusively inside a host's one serial blocking
//! preparation job. It owns the captured lookup policy, at most one successful
//! omission pin and bounded reusable compact records. It never owns a
//! `assets::VerifiedSet`, a directory descriptor or unrelated asset bytes once
//! one preparation has completed: a set is read through held root descriptors,
//! the needed verified bytes and compact metadata are extracted, and every
//! descriptor is dropped before the registry transaction. Toolkit owns the
//! immutable registered font bytes, the actual renderer IDs, the aliases and
//! the process caps; compact font sources share its canonical allocations.
//!
//! One preparation:
//!
//! 1. Resolve the authored [`settings::ResourceReference`] or the recorded
//!    expected [`settings::ResourceBinding`]; disagreement is a fault.
//! 2. Reuse an exact compact record when its identity, icon selector and text
//!    signature all match and every required glyph alias is retained;
//!    otherwise open the approved roots and capture the set through the
//!    verified reader, whose absence falls through only for a fresh omission
//!    and whose corrupt/mismatched set is always terminal.
//! 3. Translate the verified bytes into one complete toolkit
//!    `FontCollection` — every text record, the selected icon catalogue with
//!    its locked glyph table — resolve every required image variant against
//!    the process image ledger, then submit exactly one atomic
//!    `FontRegistry::register_batch` after all preflight. The registry
//!    validates intrinsic family claims, face indexes, sealed weights and
//!    glyph cmap presence; appearance only reports its evidence.
//! 4. Assemble the prepared typography with the immutable receipt.
//!    Cancellation is checked between the bounded stages, immediately before
//!    the batch and again before the candidate returns; it never interrupts
//!    one parser call.
//!
//! Process accounting: decoded image variants live in a bounded, no-eviction
//! process ledger (512 variants, 32 MiB retained encoded sources, 64 MiB
//! retained decoded pixels). It permanently retains canonical source bytes
//! and renderer handles. Equal captures share actual allocations, and public
//! image handles remain charged after receipts are dropped. Exhaustion is a
//! refusal until process restart. Verified-read and decoder staging is serial behind one
//! fixed-bound permit, acquired inside the worker and released on every exit.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use assets::{ExplicitRequest, IconDefault, Lookup, ReadLimits, VerifiedFile, VerifiedSet};
use design::{ResolvedTypeRecord, TypographyGeneric, TypographyRole};
use iced_core::font::{Stretch, Style, Weight};
use settings::{Diagnostic, IconReference, RESOURCE_SCHEMA, ResourceBinding, ResourceReference};
use toolkit::{
    Icon,
    fonts::{
        FontChoice, FontSelection,
        registry::{
            FamilyGroup, FontBlob, FontCollection, IconCatalogue, IconSelectionRequest,
            OwnedSelection, RegistrationBatch, RegistryUsage, SelectionRequest, SourceFace,
            WeightPolicy, registry as process_registry,
        },
    },
    icons::{
        Ready,
        assets::{ImageFormat, decode_owned},
    },
};

use crate::settings::{Prepared, Projection};

/// The most compact records one host retains.
const MAX_COMPACT_SETS: usize = 64;
/// The most icon requirements one preparation accepts.
const MAX_REQUIREMENTS: usize = 256;
/// The longest host-local icon key or catalogue name, in bytes.
const MAX_KEY_BYTES: usize = 96;
/// The largest physical side a requirement may request, in pixels.
const MAX_IMAGE_SIDE: f32 = 2048.0;
/// The largest encoded icon asset the host decodes (8 MiB).
const MAX_ENCODED_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
/// The tighter appearance per-file bound of the verified reader (32 MiB).
const MAX_PER_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// The most compact metadata one retained set may hold (2 MiB).
const MAX_COMPACT_METADATA_BYTES: u64 = 2 * 1024 * 1024;
/// The icon selection key of the one catalogue selection per batch.
const ICON_KEY: &str = "icons";

fn fault(path: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("unsupported_resources", path, message)
}

/// A process image-ledger refusal: an immutable cap was exhausted. Never an
/// eviction decision, because the ledger never evicts.
enum ImageError {
    Sources { have: usize, limit: usize },
    Collision,
    Variants { have: usize, limit: usize },
    Encoded { have: u64, need: u64, limit: u64 },
    Decoded { have: u64, need: u64, limit: u64 },
}

impl ImageError {
    fn message(&self) -> String {
        match self {
            Self::Sources { have, limit } => {
                format!("image store would retain {have} sources; the limit is {limit}")
            }
            Self::Collision => "equal image digests identify different source bytes".into(),
            Self::Variants { have, limit } => {
                format!("decoded image ledger holds {have} variants; the limit is {limit}")
            }
            Self::Encoded { have, need, limit } => format!(
                "decoded image ledger holds {have} encoded bytes; {need} more needed, limit {limit}"
            ),
            Self::Decoded { have, need, limit } => format!(
                "decoded image ledger holds {have} decoded bytes; {need} more needed, limit {limit}"
            ),
        }
    }
}

/// The process-wide image ledger usage, always within the immutable caps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageUsage {
    /// Distinct retained encoded image sources, charged once per exact digest.
    pub sources: usize,
    pub encoded_bytes: u64,
    /// Distinct decoded variants, charged once per exact
    /// (digest, dimensions, symbolic tint).
    pub variants: usize,
    pub decoded_bytes: u64,
}

/// The decoded-image caps, shared by every host in the process.
pub const MAX_RETAINED_VARIANTS: usize = 512;
/// The maximum permanent source records, including their metadata.
pub const MAX_RETAINED_SOURCES: usize = 4096;
pub const MAX_RETAINED_ENCODED_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_RETAINED_DECODED_BYTES: u64 = 64 * 1024 * 1024;

/// One charged decoded variant identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct VariantKey {
    source: String,
    svg: bool,
    side: u32,
    tint: Option<[u8; 4]>,
}

/// A permanently retained canonical renderer handle. Widget/adapter clones
/// cannot escape its accounting because the store never releases entries.
#[derive(Debug)]
struct VariantCharge {
    key: VariantKey,
    decoded: u64,
    handle: iced_core::image::Handle,
}

#[derive(Default)]
struct ImageLedger {
    sources: BTreeMap<String, Arc<[u8]>>,
    encoded_bytes: u64,
    variants: BTreeMap<VariantKey, Arc<VariantCharge>>,
    decoded_bytes: u64,
}

impl ImageLedger {
    fn usage(&self) -> ImageUsage {
        ImageUsage {
            sources: self.sources.len(),
            encoded_bytes: self.encoded_bytes,
            variants: self.variants.len(),
            decoded_bytes: self.decoded_bytes,
        }
    }
}

struct ImageStore {
    state: Mutex<ImageLedger>,
}

/// A bounded private admission plan. Nothing in it is process-visible until
/// the complete resource batch succeeds. The enclosing STAGING permit excludes
/// other admissions between preflight and publication.
struct ImageAdmission {
    sources: BTreeMap<String, Arc<[u8]>>,
    variants: BTreeMap<VariantKey, Arc<VariantCharge>>,
    usage_after: ImageUsage,
}

impl ImageStore {
    fn source(&self, digest: &str) -> Option<Arc<[u8]>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sources
            .get(digest)
            .cloned()
    }

    fn variant(&self, key: &VariantKey) -> Option<Arc<VariantCharge>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .variants
            .get(key)
            .cloned()
    }

    fn preflight(
        &self,
        compact: &CompactSet,
        variants: &[Arc<VariantCharge>],
    ) -> Result<ImageAdmission, ImageError> {
        let ledger = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sources: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
        for source in &compact.assets {
            if let Some(previous) = ledger
                .sources
                .get(&source.blake3)
                .or_else(|| sources.get(&source.blake3))
            {
                if previous.as_ref() != source.bytes.as_ref() {
                    return Err(ImageError::Collision);
                }
            } else {
                sources.insert(source.blake3.clone(), Arc::clone(&source.bytes));
            }
        }
        let count = ledger.sources.len() + sources.len();
        if count > MAX_RETAINED_SOURCES {
            return Err(ImageError::Sources {
                have: count,
                limit: MAX_RETAINED_SOURCES,
            });
        }
        let encoded: u64 = sources.values().map(|source| source.len() as u64).sum();
        if ledger
            .encoded_bytes
            .checked_add(encoded)
            .is_none_or(|total| total > MAX_RETAINED_ENCODED_BYTES)
        {
            return Err(ImageError::Encoded {
                have: ledger.encoded_bytes,
                need: encoded,
                limit: MAX_RETAINED_ENCODED_BYTES,
            });
        }
        let variants: BTreeMap<_, _> = variants
            .iter()
            .filter(|variant| !ledger.variants.contains_key(&variant.key))
            .map(|variant| (variant.key.clone(), Arc::clone(variant)))
            .collect();
        let count = ledger.variants.len() + variants.len();
        if count > MAX_RETAINED_VARIANTS {
            return Err(ImageError::Variants {
                have: count,
                limit: MAX_RETAINED_VARIANTS,
            });
        }
        let decoded: u64 = variants.values().map(|variant| variant.decoded).sum();
        if ledger
            .decoded_bytes
            .checked_add(decoded)
            .is_none_or(|total| total > MAX_RETAINED_DECODED_BYTES)
        {
            return Err(ImageError::Decoded {
                have: ledger.decoded_bytes,
                need: decoded,
                limit: MAX_RETAINED_DECODED_BYTES,
            });
        }
        let usage_after = ImageUsage {
            sources: ledger.sources.len() + sources.len(),
            encoded_bytes: ledger.encoded_bytes + encoded,
            variants: ledger.variants.len() + variants.len(),
            decoded_bytes: ledger.decoded_bytes + decoded,
        };
        Ok(ImageAdmission {
            sources,
            variants,
            usage_after,
        })
    }

    fn publish(&self, admission: ImageAdmission) {
        let mut ledger = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (key, bytes) in admission.sources {
            assert!(
                !ledger.sources.contains_key(&key),
                "STAGING excludes competing admissions"
            );
            ledger.encoded_bytes += bytes.len() as u64;
            ledger.sources.insert(key, bytes);
        }
        for (key, variant) in admission.variants {
            assert!(
                !ledger.variants.contains_key(&key),
                "STAGING excludes competing admissions"
            );
            ledger.decoded_bytes += variant.decoded;
            ledger.variants.insert(key, variant);
        }
    }

    fn usage(&self) -> ImageUsage {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .usage()
    }
}
/// One process-wide image ledger, created on first use and never reset.
fn image_store() -> &'static ImageStore {
    static STORE: OnceLock<ImageStore> = OnceLock::new();
    STORE.get_or_init(|| ImageStore {
        state: Mutex::new(ImageLedger::default()),
    })
}

/// The current process-wide image ledger usage, against the immutable caps.
pub fn image_usage() -> ImageUsage {
    image_store().usage()
}

/// Test-only isolation, called under TESTS with no other test accessing the
/// store. Production has no reset operation and no destructor accounting.
#[cfg(test)]
fn reset_image_ledger_for_tests() {
    let mut ledger = image_store()
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *ledger = ImageLedger::default();
}

/// The process staging gate: verified-read and decoder staging is globally
/// serial, so at most one preparation allocates captured-set and decode
/// scratch at once. That one read is itself bounded by the verified reader's
/// limits (32 MiB per file, 128 MiB total). The permit is released on every
/// exit. Contending workers sleep on the mutex, then check cancellation again;
/// contention does not become a permanent resource fault.
static STAGING: Mutex<()> = Mutex::new(());

struct StagingPermit {
    _guard: MutexGuard<'static, ()>,
}

fn staging_permit() -> StagingPermit {
    StagingPermit {
        _guard: STAGING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    }
}

/// One finite icon variant a presentation requires.
///
/// `key` is the host-local lookup key (unique in one request); `name` is the
/// verified catalogue or asset name. The requirement carries no path: the
/// selected catalogue and style resolve the name against verified bytes.
#[derive(Clone, Debug)]
pub struct IconRequirement {
    pub key: String,
    pub name: String,
    pub logical_size: f32,
    pub scale: f32,
    pub tint: iced_core::Color,
}

/// Validated finite icon requirements of one preparation. Construction is
/// pure: it never reads files, fonts or the registry.
#[derive(Clone, Debug, Default)]
pub struct ResourceRequirements {
    icons: Vec<IconRequirement>,
}

impl ResourceRequirements {
    /// Validate the icon list: bounded count, unique keys, bounded key and
    /// name lengths, finite positive sizes/scales and bounded output
    /// dimensions.
    pub fn new(icons: Vec<IconRequirement>) -> Result<Self, Diagnostic> {
        if icons.len() > MAX_REQUIREMENTS {
            return Err(fault(
                "resources.requirements",
                format!("at most {MAX_REQUIREMENTS} icon requirements"),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for requirement in &icons {
            let path = format!("resources.requirements.{}", requirement.key);
            if requirement.key.is_empty() || requirement.key.len() > MAX_KEY_BYTES {
                return Err(fault(
                    &path,
                    format!("key must be 1..={MAX_KEY_BYTES} bytes"),
                ));
            }
            if !seen.insert(requirement.key.as_str()) {
                return Err(fault(&path, "duplicate icon key"));
            }
            if requirement.name.is_empty() || requirement.name.len() > MAX_KEY_BYTES {
                return Err(fault(
                    &path,
                    format!("name must be 1..={MAX_KEY_BYTES} bytes"),
                ));
            }
            for (field, value) in [
                ("logical_size", requirement.logical_size),
                ("scale", requirement.scale),
            ] {
                if !value.is_finite() || value <= 0.0 {
                    return Err(fault(&path, format!("{field} must be finite and positive")));
                }
            }
            let side = (requirement.logical_size * requirement.scale).ceil();
            if !side.is_finite() || !(1.0..=MAX_IMAGE_SIDE).contains(&side) {
                return Err(fault(
                    &path,
                    format!("output dimensions exceed the {MAX_IMAGE_SIDE} pixel bound"),
                ));
            }
            for (channel, value) in [
                ("r", requirement.tint.r),
                ("g", requirement.tint.g),
                ("b", requirement.tint.b),
                ("a", requirement.tint.a),
            ] {
                if !value.is_finite() {
                    return Err(fault(&path, format!("tint {channel} must be finite")));
                }
            }
        }
        Ok(Self { icons })
    }

    /// No icon variants.
    pub fn empty() -> Self {
        Self { icons: Vec::new() }
    }

    /// The validated requirements, in request order.
    pub fn icons(&self) -> &[IconRequirement] {
        &self.icons
    }
}

/// Renderer-neutral evidence the toolkit registry reported for the batch this
/// receipt was built from: actual iced font-system versions, added/reused
/// counts and process usage, never a success boolean. The image usage is the
/// process ledger after this preparation.
#[derive(Clone, Debug, Default)]
pub struct RegistryEvidence {
    pub renderer_version_before: u32,
    pub renderer_version_after: u32,
    pub sources_added: usize,
    pub sources_reused: usize,
    pub faces_added: usize,
    pub faces_reused: usize,
    pub policies_added: usize,
    pub policies_reused: usize,
    pub usage_before: RegistryUsage,
    pub usage_after: RegistryUsage,
    pub image: ImageUsage,
}

impl RegistryEvidence {
    fn without_registration(mut self) -> Self {
        let version = toolkit::graphics::text::font_system()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .version()
            .value();
        let usage = process_registry().usage();
        self.renderer_version_before = version;
        self.renderer_version_after = version;
        self.sources_added = 0;
        self.sources_reused = 0;
        self.faces_added = 0;
        self.faces_reused = 0;
        self.policies_added = 0;
        self.policies_reused = 0;
        self.usage_before = usage;
        self.usage_after = usage;
        self
    }
    fn with_image_usage(mut self) -> Self {
        self.image = image_store().usage();
        self
    }
}

/// The actual intrinsic selection evidence of one text record: the family,
/// face index and source digest the renderer really resolved, and the
/// requested versus effective numeric weight. No filesystem paths.
#[derive(Clone, Debug)]
pub struct TextEvidence {
    pub record: String,
    pub family: String,
    pub face_index: u32,
    pub source_blake3: String,
    pub requested_weight: u16,
    pub effective_weight: u16,
    pub reason: String,
}

/// The evidence of one declared non-font icon asset: descriptor identity and
/// source digest, without pretending the SVG or raster has a font family or
/// weight.
#[derive(Clone, Debug)]
pub struct AssetEvidence {
    pub name: String,
    pub style: String,
    pub symbolic: bool,
    pub source_blake3: String,
}

/// The selection evidence of one required icon: the selected catalogue
/// family/style for a glyph, the declared asset descriptor for an image, and
/// the verified source digest either way. No filesystem paths.
#[derive(Clone, Debug)]
pub struct IconEvidence {
    pub key: String,
    pub name: String,
    pub family: Option<String>,
    pub style: Option<String>,
    pub glyph: Option<char>,
    pub weight: Option<u16>,
    pub asset: Option<AssetEvidence>,
    pub source_blake3: Option<String>,
    pub reason: String,
}

/// Read-only resource evidence of one successful preparation, for acceptance
/// diagnostics. Never a retained duplicate of source payload.
#[derive(Clone, Debug)]
pub struct ResourceEvidence {
    pub set_id: Option<String>,
    pub manifest_blake3: Option<String>,
    pub text: Vec<TextEvidence>,
    pub icons: Vec<IconEvidence>,
    pub registry: RegistryEvidence,
}

#[derive(Debug)]
struct Receipt {
    binding: Option<ResourceBinding>,
    texts: BTreeMap<String, FontSelection>,
    owned_texts: BTreeMap<String, OwnedSelection>,
    icons: BTreeMap<String, Ready>,
    evidence: ResourceEvidence,
    /// Shared canonical payloads, also permanently retained by the store.
    _charges: Vec<Arc<VariantCharge>>,
}

/// An immutable resource receipt attached to a [`Prepared`] presentation:
/// the exact binding a successful activation may acknowledge, the ready icons
/// by host-local key and the selection evidence. Applications consume the
/// receipt; they do not rediscover resources from draw callbacks.
#[derive(Clone, Debug)]
pub struct PreparedResources {
    receipt: Arc<Receipt>,
}

impl PreparedResources {
    fn assemble(
        binding: Option<ResourceBinding>,
        texts: BTreeMap<String, FontSelection>,
        owned_texts: BTreeMap<String, OwnedSelection>,
        icons: BTreeMap<String, Ready>,
        evidence: ResourceEvidence,
        charges: Vec<Arc<VariantCharge>>,
    ) -> Self {
        Self {
            receipt: Arc::new(Receipt {
                binding,
                texts,
                owned_texts,
                icons,
                evidence,
                _charges: charges,
            }),
        }
    }

    /// The renderer-neutral binding of the exact verified identity this
    /// receipt was prepared from. `None` when no verified set exists: the
    /// presentation is then an honest generic rescue, never a fake Current.
    pub fn binding(&self) -> Option<&ResourceBinding> {
        self.receipt.binding.as_ref()
    }

    /// The ready icon for a requirement key, or `None` when the request was
    /// satisfied without a verified set.
    pub fn icon(&self, key: &str) -> Option<&Ready> {
        self.receipt.icons.get(key)
    }

    /// The selection evidence of this preparation.
    pub fn evidence(&self) -> &ResourceEvidence {
        &self.receipt.evidence
    }

    /// Immutable source handles for a verified text record. Non-Iced
    /// painters consume these exact effective weights and ordered faces;
    /// generic rescue has no verified owned selection.
    pub fn owned_text(&self, record: &str) -> Option<&OwnedSelection> {
        self.receipt.owned_texts.get(record)
    }

    /// The resolved selection of one prepared text record. Every record of
    /// the projection is present after a successful preparation.
    pub(crate) fn text_selection(&self, record: &str) -> Option<FontSelection> {
        self.receipt.texts.get(record).copied()
    }
}

/// One successful omission pin: the exact set ID and manifest digest this
/// host captured successfully at the resource stage. This pin survives a
/// subsequent application-builder refusal or cancellation: it is lookup
/// policy, not proof of activation. Omission never drifts during the
/// host lifetime once this is set.
#[derive(Clone, Debug)]
struct Pin {
    set_id: String,
    digest: [u8; 32],
}

/// An authored icon selector (family, style, exact weight).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selector {
    family: String,
    style: String,
    weight: u16,
}

impl From<&IconReference> for Selector {
    fn from(reference: &IconReference) -> Self {
        Self {
            family: reference.family.clone(),
            style: reference.style.clone(),
            weight: reference.weight,
        }
    }
}

impl From<&IconDefault> for Selector {
    fn from(default: &IconDefault) -> Self {
        Self {
            family: default.family.clone(),
            style: default.style.clone(),
            weight: default.weight,
        }
    }
}

/// One retained font source: the verified shared bytes plus the metadata the
/// registry needs. After commit they share the registry's canonical source
/// allocation, including sources not chosen as a primary face.
#[derive(Clone)]
struct CompactSource {
    blake3: String,
    bytes: Arc<[u8]>,
}

/// One retained selected icon catalogue with its parsed glyph table and the
/// index of its font source.
#[derive(Clone)]
struct CompactCatalogue {
    family: String,
    style: String,
    source: usize,
    face_index: u32,
    weight: u16,
    source_blake3: String,
    glyphs: BTreeMap<String, char>,
}

/// One retained encoded image source of the selected style, charged to the
/// process image ledger. The format comes from the locked extension; decoding
/// verifies the parse.
#[derive(Clone)]
struct CompactAsset {
    name: String,
    style: String,
    symbolic: bool,
    format: ImageFormat,
    blake3: String,
    bytes: Arc<[u8]>,
}

/// A bounded compact set record: the verified identity, the retained source
/// bytes and metadata, and the selections of its last successful batch.
/// Reuse never reads the filesystem and, on an unchanged effective text
/// signature with every required glyph alias retained, never re-registers.
#[derive(Clone)]
struct CompactSet {
    set_id: String,
    digest: [u8; 32],
    selector: Option<Selector>,
    /// Packaged-omission semantics (role remapping) of the batch this record
    /// stored; part of the reuse key so explicit and packaged preparations of
    /// the same set never cross-reuse.
    packaged: bool,
    text_signature: BTreeMap<String, (Vec<String>, u16)>,
    texts: BTreeMap<String, FontSelection>,
    owned_texts: BTreeMap<String, OwnedSelection>,
    text_evidence: Vec<TextEvidence>,
    glyph_fonts: BTreeMap<String, iced_core::Font>,
    registry: RegistryEvidence,
    sources: Vec<CompactSource>,
    /// Manifest role → (source slot, claimed family). Role fonts without a
    /// family claim are not retained: no honest group can be declared.
    roles: BTreeMap<String, (usize, String)>,
    catalogue: Option<CompactCatalogue>,
    assets: Vec<CompactAsset>,
}

impl CompactSet {
    fn matches(
        &self,
        set_id: &str,
        digest: [u8; 32],
        selector: Option<&Selector>,
        packaged: bool,
    ) -> bool {
        self.set_id == set_id
            && self.digest == digest
            && self.selector.as_ref() == selector
            && self.packaged == packaged
    }
}

/// The resolved identity of one preparation request.
struct RequestIdentity {
    set_id: String,
    digest: Option<[u8; 32]>,
    selector: Option<Selector>,
    explicit: bool,
    /// Packaged-omission semantics: no authored reference and no authored
    /// icon selector, so documented role remapping may apply.
    packaged: bool,
    expected: Option<ResourceBinding>,
}

/// One glyph resolved against the selected catalogue.
struct PlannedGlyph {
    key: String,
    name: String,
    glyph: char,
    weight: u16,
    family: String,
    style: String,
    source_blake3: String,
}

/// One image resolved against the selected style's declared assets.
struct PlannedImage {
    key: String,
    name: String,
    style: String,
    format: ImageFormat,
    side: u32,
    tint: Option<[u8; 4]>,
    symbolic: bool,
    logical_size: f32,
    blake3: String,
    bytes: Arc<[u8]>,
}

struct IconPlan {
    glyphs: Vec<PlannedGlyph>,
    images: Vec<PlannedImage>,
}

/// The central verified resource host. Construct it with the captured host
/// lookup policy; it performs no I/O until [`ResourceHost::prepare`].
pub struct ResourceHost {
    roots: Vec<PathBuf>,
    pin: Option<Pin>,
    compact: Vec<CompactSet>,
}

impl ResourceHost {
    /// Capture the lookup's roots. No I/O, no global state.
    pub fn new(lookup: Lookup) -> Self {
        Self {
            roots: lookup.roots().to_vec(),
            pin: None,
            compact: Vec::new(),
        }
    }

    /// Prepare the whole projection against the authored reference or the
    /// recorded expected binding, satisfying every required icon. Cancellation
    /// is checked between the bounded stages, immediately before the atomic
    /// registration and again before the candidate returns. The process
    /// staging permit is held for the whole preparation and released on every
    /// exit.
    pub fn prepare(
        &mut self,
        projection: Projection,
        reference: Option<&ResourceReference>,
        expected: Option<&ResourceBinding>,
        requirements: ResourceRequirements,
        check: &mut dyn FnMut() -> Result<(), Diagnostic>,
    ) -> Result<Prepared, Diagnostic> {
        check()?;
        // Validate all font-independent typography geometry before any image
        // admission or registry commit. Resolving a different Font cannot
        // change these sizes, line heights or record names.
        projection.clone().prepare(|_, _| {
            Ok(FontSelection {
                font: iced_core::Font::DEFAULT,
                choice: FontChoice::Generic,
            })
        })?;
        let _permit = staging_permit();
        check()?;
        let identity = request_identity(reference, expected, self.pin.as_ref())?;
        let compact_index = identity.digest.and_then(|digest| {
            self.compact.iter().position(|compact| {
                compact.matches(
                    &identity.set_id,
                    digest,
                    identity.selector.as_ref(),
                    identity.packaged,
                )
            })
        });
        match compact_index {
            Some(index) => {
                let mut compact = self.compact[index].clone();
                // Retain encoded payloads only for requested images. A new
                // image requirement rereads this exact pinned manifest,
                // never `current`, and replaces compact reuse metadata.
                if requirements.icons().iter().any(|requirement| {
                    !compact
                        .catalogue
                        .as_ref()
                        .is_some_and(|catalogue| catalogue.glyphs.contains_key(&requirement.name))
                        && !compact
                            .assets
                            .iter()
                            .any(|asset| asset.name == requirement.name)
                }) {
                    compact = self
                        .read(&identity, &requirements, check)?
                        .ok_or_else(|| fault("resources", "pinned resource set is unavailable"))?;
                }
                let plan = icon_plan(&requirements, &compact)?;
                let signature = text_signature(projection.type_records(), &compact);
                let reusable = signature == compact.text_signature
                    && plan
                        .glyphs
                        .iter()
                        .all(|glyph| compact.glyph_fonts.contains_key(&glyph.name));
                if reusable {
                    reuse(projection, &plan, &compact, &identity, check)
                } else {
                    let (prepared, updated) =
                        register(projection, &plan, compact, &identity, check)?;
                    self.store(updated, Some(index));
                    check()?;
                    Ok(prepared)
                }
            }
            None => {
                let Some(compact) = self.read(&identity, &requirements, check)? else {
                    return generic_prepared(projection, &requirements, check);
                };
                let plan = icon_plan(&requirements, &compact)?;
                let (prepared, updated) = register(projection, &plan, compact, &identity, check)?;
                if reference.is_none() && self.pin.is_none() {
                    self.pin = Some(Pin {
                        set_id: updated.set_id.clone(),
                        digest: updated.digest,
                    });
                }
                self.store(updated, None);
                check()?;
                Ok(prepared)
            }
        }
    }

    /// One bounded verified read for this identity, followed by bounded
    /// extraction. The `VerifiedSet` and every root descriptor are dropped
    /// before the registry transaction. An explicit request resolves
    /// `sets/<id>` directly and never `current`; only a fresh omission
    /// consults `current`, descriptor-owned and exactly once.
    fn read(
        &self,
        identity: &RequestIdentity,
        requirements: &ResourceRequirements,
        check: &mut dyn FnMut() -> Result<(), Diagnostic>,
    ) -> Result<Option<CompactSet>, Diagnostic> {
        let mut roots = Vec::new();
        for root in &self.roots {
            match config::atomic::open_directory(root) {
                Ok(directory) => roots.push(directory),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(fault(
                        "resources",
                        format!("cannot open asset root: {error}"),
                    ));
                }
            }
        }
        let limits = ReadLimits {
            max_file_bytes: MAX_PER_FILE_BYTES,
            ..ReadLimits::new()
        };
        let set = if identity.explicit {
            let request = ExplicitRequest {
                set_id: &identity.set_id,
                manifest_blake3: identity.digest,
            };
            match VerifiedSet::read_explicit(&roots, &request, limits) {
                Ok(Some(set)) => set,
                Ok(None) => return Err(fault("resources", "requested asset set is unavailable")),
                Err(error) => return Err(fault("resources", error.to_string())),
            }
        } else {
            let mut found = None;
            for root in &roots {
                match VerifiedSet::read_current(root, limits) {
                    Ok(Some(set)) => {
                        found = Some(set);
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => return Err(fault("resources", error.to_string())),
                }
            }
            match found {
                Some(set) => set,
                None => return Ok(None),
            }
        };
        let compact = extract(&set, identity, requirements)?;
        check()?;
        Ok(Some(compact))
    }

    /// Store a compact record, evicting the oldest when the bound is reached.
    /// Compact records are reuse metadata; the registry and the process image
    /// ledger own the actual retained resources.
    fn store(&mut self, compact: CompactSet, index: Option<usize>) {
        if let Some(index) = index {
            self.compact[index] = compact;
            return;
        }
        if self.compact.len() >= MAX_COMPACT_SETS {
            self.compact.remove(0);
        }
        self.compact.push(compact);
    }
}

/// Disagreement between the authored reference and the recorded binding, or
/// an invalid identity, is refused before any read.
fn request_identity(
    reference: Option<&ResourceReference>,
    expected: Option<&ResourceBinding>,
    pin: Option<&Pin>,
) -> Result<RequestIdentity, Diagnostic> {
    if let Some(binding) = expected {
        binding.validate()?;
        let digest = hex_digest(&binding.manifest_blake3)?;
        if reference.is_none()
            && pin.is_some_and(|pin| pin.set_id != binding.set_id || pin.digest != digest)
        {
            return Err(fault(
                "resources",
                "recorded omission binding conflicts with the lifetime pin",
            ));
        }
        let selector = binding.icons.as_ref().map(Selector::from);
        if let Some(reference) = reference {
            reference.validate("appearance.resources")?;
            if reference.set_id != binding.set_id
                || reference.manifest_blake3 != binding.manifest_blake3
                || reference.icons != binding.icons
            {
                return Err(fault(
                    "resources",
                    "authored reference and recorded binding disagree",
                ));
            }
        }
        Ok(RequestIdentity {
            set_id: binding.set_id.clone(),
            digest: Some(digest),
            selector,
            explicit: true,
            packaged: reference.is_none() && binding.icons.is_none(),
            expected: Some(binding.clone()),
        })
    } else if let Some(reference) = reference {
        reference.validate("appearance.resources")?;
        Ok(RequestIdentity {
            set_id: reference.set_id.clone(),
            digest: Some(hex_digest(&reference.manifest_blake3)?),
            selector: reference.icons.as_ref().map(Selector::from),
            explicit: true,
            packaged: false,
            expected: None,
        })
    } else if let Some(pin) = pin {
        Ok(RequestIdentity {
            set_id: pin.set_id.clone(),
            digest: Some(pin.digest),
            selector: None,
            explicit: true,
            packaged: true,
            expected: None,
        })
    } else {
        Ok(RequestIdentity {
            set_id: String::new(),
            digest: None,
            selector: None,
            explicit: false,
            packaged: true,
            expected: None,
        })
    }
}

/// The slot of a verified font file among the compact sources, deduplicated by
/// exact source digest so shared files share one retained allocation.
fn source_slot(sources: &mut Vec<CompactSource>, file: &VerifiedFile) -> usize {
    if let Some(index) = sources
        .iter()
        .position(|source| source.blake3 == file.blake3())
    {
        return index;
    }
    sources.push(CompactSource {
        blake3: file.blake3().to_owned(),
        bytes: file.shared_bytes(),
    });
    sources.len() - 1
}

/// Bounded extraction of the compact record: role and catalogue font bytes
/// with their digests, the selected catalogue and the selected style's
/// encoded assets, charged to the process image ledger. Only role fonts with
/// a declared family claim and the selected icon catalogue source are
/// retained; unrelated locked files never enter the compact record. The
/// verified set is dropped by the caller afterwards.
fn extract(
    set: &VerifiedSet,
    identity: &RequestIdentity,
    requirements: &ResourceRequirements,
) -> Result<CompactSet, Diagnostic> {
    let default = set.icon_default().cloned();
    let selected = identity
        .selector
        .clone()
        .or_else(|| default.as_ref().map(Selector::from));
    let mut sources = Vec::new();
    let mut roles = BTreeMap::new();
    for (role, path) in &set.manifest().fonts {
        let Some(family) = set.manifest().font_families.get(role) else {
            continue;
        };
        if family.is_empty() {
            continue;
        }
        let file = set.file(path).ok_or_else(|| {
            fault(
                "resources",
                format!("manifest role {role:?} file is not locked"),
            )
        })?;
        let slot = source_slot(&mut sources, file);
        roles.insert(role.clone(), (slot, family.clone()));
    }
    let catalogue = match &selected {
        Some(selected) => {
            let resolved = set
                .icon_catalogue(&selected.family, &selected.style)
                .map_err(|error| fault("resources", error.to_string()))?;
            let Some(resolved) = resolved else {
                return Err(fault(
                    "resources",
                    format!(
                        "asset set declares no icon catalogue for {:?}/{:?}",
                        selected.family, selected.style
                    ),
                ));
            };
            if resolved.catalogue.family.is_empty() {
                return Err(fault(
                    "resources",
                    "asset set icon catalogue declares no family name",
                ));
            }
            let font = set
                .icon_catalogue_font(resolved)
                .ok_or_else(|| fault("resources", "icon catalogue font is not locked"))?;
            let slot = source_slot(&mut sources, font);
            Some(CompactCatalogue {
                family: resolved.catalogue.family.clone(),
                style: resolved.catalogue.style.clone(),
                source: slot,
                face_index: resolved.catalogue.face_index,
                weight: selected.weight,
                source_blake3: font.blake3().to_owned(),
                glyphs: resolved.glyphs.clone(),
            })
        }
        None if !requirements.icons().is_empty() => {
            return Err(fault("resources", "asset set declares no icon catalogue"));
        }
        None => None,
    };
    let mut assets = Vec::new();
    if let Some(style) = selected.as_ref().map(|selector| selector.style.as_str()) {
        for asset in set.icon_assets() {
            if asset.style == style
                && requirements
                    .icons()
                    .iter()
                    .any(|requirement| requirement.name == asset.name)
                && !catalogue
                    .as_ref()
                    .is_some_and(|catalogue| catalogue.glyphs.contains_key(&asset.name))
            {
                let file = set.icon_asset_file(asset).ok_or_else(|| {
                    fault(
                        "resources",
                        format!("icon asset {:?} is not locked", asset.name),
                    )
                })?;
                if file.bytes().len() as u64 > MAX_ENCODED_IMAGE_BYTES {
                    return Err(fault(
                        "resources",
                        format!(
                            "icon asset {:?} exceeds the encoded image bound",
                            asset.name
                        ),
                    ));
                }
                assets.push(CompactAsset {
                    name: asset.name.clone(),
                    style: asset.style.clone(),
                    symbolic: asset.symbolic,
                    format: image_format(&asset.path),
                    blake3: file.blake3().to_owned(),
                    bytes: file.shared_bytes(),
                });
            }
        }
    }
    let compact = CompactSet {
        set_id: set.identity().set_id().to_owned(),
        digest: set.identity().manifest_blake3(),
        selector: identity.selector.clone(),
        packaged: identity.packaged,
        text_signature: BTreeMap::new(),
        texts: BTreeMap::new(),
        owned_texts: BTreeMap::new(),
        text_evidence: Vec::new(),
        glyph_fonts: BTreeMap::new(),
        registry: RegistryEvidence::default(),
        sources,
        roles,
        catalogue,
        assets,
    };
    if metadata_charge(&compact) > MAX_COMPACT_METADATA_BYTES {
        return Err(fault(
            "resources",
            "set metadata exceeds the compact record bound",
        ));
    }
    Ok(compact)
}

/// The locked extension decides the declared image format; the decoder
/// verifies the parse over the exact verified bytes.
fn image_format(path: &str) -> ImageFormat {
    if path.ends_with(".svg") {
        ImageFormat::Svg
    } else {
        ImageFormat::Raster
    }
}

/// Resolve every required icon against the compact record: a glyph in the
/// selected catalogue's table (its cmap presence and sealed weight are the
/// registry's preflight), else the declared image asset of the same style.
/// Nothing silently becomes a themed icon: an unsatisfiable requirement is a
/// fault.
fn icon_plan(
    requirements: &ResourceRequirements,
    compact: &CompactSet,
) -> Result<IconPlan, Diagnostic> {
    let mut plan = IconPlan {
        glyphs: Vec::new(),
        images: Vec::new(),
    };
    if requirements.icons().is_empty() {
        return Ok(plan);
    }
    let catalogue = compact
        .catalogue
        .as_ref()
        .ok_or_else(|| fault("resources", "asset set declares no icon catalogue"))?;
    for requirement in requirements.icons() {
        let path = format!("resources.icons.{}", requirement.key);
        if let Some(glyph) = catalogue.glyphs.get(&requirement.name) {
            plan.glyphs.push(PlannedGlyph {
                key: requirement.key.clone(),
                name: requirement.name.clone(),
                glyph: *glyph,
                weight: catalogue.weight,
                family: catalogue.family.clone(),
                style: catalogue.style.clone(),
                source_blake3: catalogue.source_blake3.clone(),
            });
        } else if let Some(asset) = compact
            .assets
            .iter()
            .find(|asset| asset.name == requirement.name && asset.style == catalogue.style)
        {
            let side = (requirement.logical_size * requirement.scale).ceil() as u32;
            plan.images.push(PlannedImage {
                key: requirement.key.clone(),
                name: requirement.name.clone(),
                style: asset.style.clone(),
                format: asset.format,
                side,
                tint: asset.symbolic.then(|| requirement.tint.into_rgba8()),
                symbolic: asset.symbolic,
                logical_size: requirement.logical_size,
                blake3: asset.blake3.clone(),
                bytes: Arc::clone(&asset.bytes),
            });
        } else {
            return Err(fault(
                &path,
                format!(
                    "required icon has no glyph or declared image asset in catalogue {:?}",
                    catalogue.style
                ),
            ));
        }
    }
    Ok(plan)
}

/// Decode every required image variant before the font transaction. The
/// process ledger charges each distinct source/variant once; capacity
/// exhaustion is a fault, never a visible-name substitution. The returned
/// charge tokens stay attached to the receipt.
fn decode_images(
    plan: &IconPlan,
    check: &mut dyn FnMut() -> Result<(), Diagnostic>,
) -> Result<(Vec<(String, Ready, IconEvidence)>, Vec<Arc<VariantCharge>>), Diagnostic> {
    let mut images = Vec::new();
    let mut charges = Vec::new();
    let usage = image_store().usage();
    let mut staged_bytes = 0u64;
    let mut staged_variants = 0usize;
    for image in &plan.images {
        check()?;
        let key = VariantKey {
            source: image.blake3.clone(),
            svg: image.format == ImageFormat::Svg,
            side: if image.format == ImageFormat::Svg {
                image.side
            } else {
                0
            },
            tint: image.tint,
        };
        let charge = match image_store().variant(&key).or_else(|| {
            charges
                .iter()
                .find(|variant: &&Arc<VariantCharge>| variant.key == key)
                .cloned()
        }) {
            Some(charge) => charge,
            None => {
                if usage.variants + staged_variants >= MAX_RETAINED_VARIANTS {
                    return Err(Diagnostic::new(
                        "image_capacity",
                        "resources",
                        "decoded variant capacity exhausted",
                    ));
                }
                let decoded = decode_owned(
                    Arc::clone(&image.bytes),
                    image.format,
                    image.side,
                    image.tint,
                )
                .map_err(|error| {
                    fault(&format!("resources.icons.{}", image.key), error.to_string())
                })?;
                staged_bytes += decoded.byte_charge();
                if usage.decoded_bytes + staged_bytes > MAX_RETAINED_DECODED_BYTES {
                    return Err(Diagnostic::new(
                        "image_capacity",
                        "resources",
                        "decoded pixel capacity exhausted",
                    ));
                }
                staged_variants += 1;
                Arc::new(VariantCharge {
                    key,
                    decoded: decoded.byte_charge(),
                    handle: decoded.into_handle(),
                })
            }
        };
        let handle = charge.handle.clone();
        charges.push(charge);
        let evidence = IconEvidence {
            key: image.key.clone(),
            name: image.name.clone(),
            family: None,
            style: Some(image.style.clone()),
            glyph: None,
            weight: None,
            asset: Some(AssetEvidence {
                name: image.name.clone(),
                style: image.style.clone(),
                symbolic: image.symbolic,
                source_blake3: image.blake3.clone(),
            }),
            source_blake3: Some(image.blake3.clone()),
            reason: "declared image asset of the selected style".into(),
        };
        images.push((
            image.key.clone(),
            Ready::Image {
                handle,
                logical_size: image.logical_size,
            },
            evidence,
        ));
    }
    Ok((images, charges))
}

/// The binding this compact identity produces: the exact verified set ID and
/// digest, the authored icon selector (None records the descriptor default),
/// and the settings sibling's selection interpretation.
fn binding_for(compact: &CompactSet) -> ResourceBinding {
    ResourceBinding {
        schema: RESOURCE_SCHEMA,
        set_id: compact.set_id.clone(),
        manifest_blake3: hex::encode(compact.digest),
        interpretation: settings::resource_interpretation(),
        icons: compact.selector.as_ref().map(|selector| IconReference {
            family: selector.family.clone(),
            style: selector.style.clone(),
            weight: selector.weight,
        }),
    }
}

/// The packaged set role a historical record remaps to, when the record still
/// matches the packaged design default and the set declares that role.
fn packaged_role(name: &str) -> Option<(TypographyRole, &'static str)> {
    match name {
        "ui" => Some((TypographyRole::Ui, "sans")),
        "ui_display" => Some((TypographyRole::UiDisplay, "display")),
        "small" => Some((TypographyRole::Small, "sans")),
        "mono" => Some((TypographyRole::Mono, "mono")),
        "terminal" => Some((TypographyRole::Terminal, "mono")),
        _ => None,
    }
}

/// The declared chain a record resolves with, and whether the packaged role
/// remapping applied. Remapping prepends the set role's claimed family to a
/// record that still matches the packaged design default; it applies only
/// under packaged-omission semantics and is reported in the evidence.
fn effective_chain(
    name: &str,
    record: &ResolvedTypeRecord,
    compact: &CompactSet,
) -> (Vec<String>, bool) {
    let mut chain = Vec::new();
    let mut remapped = false;
    if compact.packaged {
        if let Some((role, set_role)) = packaged_role(name) {
            let default = design::default_typography(role);
            if default.family == record.family
                && default.fallbacks == record.fallbacks
                && default.generic == record.generic
            {
                if let Some((_, claim)) = compact.roles.get(set_role) {
                    if claim != &record.family {
                        remapped = true;
                        chain.push(claim.clone());
                    }
                }
            }
        }
    }
    let mut push = |value: &str| {
        if !chain.iter().any(|entry| entry == value) {
            chain.push(value.to_owned());
        }
    };
    push(&record.family);
    for fallback in &record.fallbacks {
        push(fallback);
    }
    (chain, remapped)
}

/// A stable signature of every record's effective chain and weight, used to
/// decide whether retained selections still cover the projection.
fn text_signature(
    records: &BTreeMap<String, ResolvedTypeRecord>,
    compact: &CompactSet,
) -> BTreeMap<String, (Vec<String>, u16)> {
    records
        .iter()
        .map(|(name, record)| {
            let (chain, _) = effective_chain(name, record, compact);
            (name.clone(), (chain, record.weight))
        })
        .collect()
}

/// One complete toolkit batch: every role font with a declared family claim,
/// the selected icon catalogue with its locked glyph table, and one selection
/// per projection type record with its effective chain and exact weight.
fn registration_batch(
    projection: &Projection,
    compact: &CompactSet,
    plan: &IconPlan,
) -> RegistrationBatch {
    let mut families: BTreeMap<String, BTreeSet<SourceFace>> = BTreeMap::new();
    for (_, (slot, claim)) in &compact.roles {
        families
            .entry(claim.clone())
            .or_default()
            .insert(SourceFace {
                source: *slot,
                index: 0,
            });
    }
    if let Some(catalogue) = &compact.catalogue {
        if !catalogue.family.is_empty() {
            families
                .entry(catalogue.family.clone())
                .or_default()
                .insert(SourceFace {
                    source: catalogue.source,
                    index: catalogue.face_index,
                });
        }
    }
    let sources = compact
        .sources
        .iter()
        .map(|source| FontBlob {
            bytes: Arc::clone(&source.bytes),
        })
        .collect();
    let family_groups = families
        .into_iter()
        .map(|(name, faces)| FamilyGroup {
            name,
            faces: faces.into_iter().collect(),
        })
        .collect();
    let catalogues = compact
        .catalogue
        .iter()
        .filter(|catalogue| !catalogue.family.is_empty())
        .map(|catalogue| IconCatalogue {
            family: catalogue.family.clone(),
            style: catalogue.style.clone(),
            face: SourceFace {
                source: catalogue.source,
                index: catalogue.face_index,
            },
            glyphs: catalogue.glyphs.clone(),
        })
        .collect();
    let selections = projection
        .type_records()
        .iter()
        .map(|(name, record)| {
            let (families, _) = effective_chain(name, record, compact);
            SelectionRequest {
                key: name.clone(),
                families,
                requested_weight: record.weight,
                weight_policy: WeightPolicy::Exact,
                style: Style::Normal,
                stretch: Stretch::Normal,
            }
        })
        .collect();
    let mut icon_requests = Vec::new();
    if !plan.glyphs.is_empty() {
        if let Some(catalogue) = &compact.catalogue {
            let mut names = BTreeSet::new();
            for glyph in &plan.glyphs {
                names.insert(glyph.name.clone());
            }
            icon_requests.push(IconSelectionRequest {
                key: ICON_KEY.to_owned(),
                family: catalogue.family.clone(),
                style: catalogue.style.clone(),
                weight: catalogue.weight,
                required_names: names.into_iter().collect(),
            });
        }
    }
    RegistrationBatch {
        collection: FontCollection {
            sources,
            families: family_groups,
            roles: BTreeMap::new(),
            icons: catalogues,
        },
        selections,
        icons: icon_requests,
    }
}

/// Reuse an exact compact record: retained selections and glyph aliases, no
/// filesystem read and no registration. Image variants are decoded afresh
/// where size or tint changed, charged to the process ledger as usual.
fn reuse(
    projection: Projection,
    plan: &IconPlan,
    compact: &CompactSet,
    identity: &RequestIdentity,
    check: &mut dyn FnMut() -> Result<(), Diagnostic>,
) -> Result<Prepared, Diagnostic> {
    let binding = binding_for(compact);
    if let Some(expected) = &identity.expected {
        if binding != *expected {
            return Err(fault(
                "resources",
                "verified resource identity differs from the recorded binding",
            ));
        }
    }
    image_store()
        .preflight(compact, &[])
        .map_err(|error| Diagnostic::new("image_capacity", "resources", error.message()))?;
    let (images, charges) = decode_images(plan, check)?;
    let admission = image_store()
        .preflight(compact, &charges)
        .map_err(|error| Diagnostic::new("image_capacity", "resources", error.message()))?;
    let mut icons = BTreeMap::new();
    let mut icon_evidence = Vec::new();
    for glyph in &plan.glyphs {
        let font = compact.glyph_fonts.get(&glyph.name).ok_or_else(|| {
            fault(
                &format!("resources.icons.{}", glyph.key),
                "retained glyph alias is missing",
            )
        })?;
        icons.insert(
            glyph.key.clone(),
            Ready::Text(Icon::with_glyph(glyph.name.clone(), glyph.glyph, *font)),
        );
        icon_evidence.push(IconEvidence {
            key: glyph.key.clone(),
            name: glyph.name.clone(),
            family: Some(glyph.family.clone()),
            style: Some(glyph.style.clone()),
            glyph: Some(glyph.glyph),
            weight: Some(glyph.weight),
            asset: None,
            source_blake3: Some(glyph.source_blake3.clone()),
            reason: "catalogue glyph in the declared face".into(),
        });
    }
    for (key, ready, evidence) in images {
        icons.insert(key, ready);
        icon_evidence.push(evidence);
    }
    let mut registry = compact.registry.clone().without_registration();
    registry.image = admission.usage_after;
    let evidence = ResourceEvidence {
        set_id: Some(compact.set_id.clone()),
        manifest_blake3: Some(hex::encode(compact.digest)),
        text: compact.text_evidence.clone(),
        icons: icon_evidence,
        registry,
    };
    check()?;
    let receipt = PreparedResources::assemble(
        Some(binding),
        compact.texts.clone(),
        compact.owned_texts.clone(),
        icons,
        evidence,
        charges,
    );
    let prepared = projection.prepare_with_resources(receipt)?;
    image_store().publish(admission);
    Ok(prepared)
}

/// Submit exactly one atomic toolkit batch after all image decoding and
/// preflight, and attach the immutable receipt to the prepared typography.
/// Returns the updated compact record so the caller stores it and, for a
/// fresh omission, pins the verified identity.
fn register(
    projection: Projection,
    plan: &IconPlan,
    compact: CompactSet,
    identity: &RequestIdentity,
    check: &mut dyn FnMut() -> Result<(), Diagnostic>,
) -> Result<(Prepared, CompactSet), Diagnostic> {
    let binding = binding_for(&compact);
    if let Some(expected) = &identity.expected {
        if binding != *expected {
            return Err(fault(
                "resources",
                "verified resource identity differs from the recorded binding",
            ));
        }
    }
    image_store()
        .preflight(&compact, &[])
        .map_err(|error| Diagnostic::new("image_capacity", "resources", error.message()))?;
    let (images, charges) = decode_images(plan, check)?;
    let admission = image_store()
        .preflight(&compact, &charges)
        .map_err(|error| Diagnostic::new("image_capacity", "resources", error.message()))?;
    // The final cancellation fence before the registry/renderer mutation
    // locks are acquired.
    check()?;
    let batch = registration_batch(&projection, &compact, plan);
    let receipt = process_registry()
        .register_batch(batch)
        .map_err(|error| fault("resources", error.to_string()))?;
    let mut texts = BTreeMap::new();
    let mut owned_texts = BTreeMap::new();
    let mut text_evidence = Vec::new();
    for (name, record) in projection.type_records() {
        let selection = receipt
            .font(name)
            .expect("successful batch resolves every requested record");
        let font = selection.font();
        let evidence = selection.evidence();
        let (chain, remapped) = effective_chain(name, record, &compact);
        let face = evidence.groups.first().and_then(|group| group.first());
        let family = evidence.family.clone();
        let choice = if family == record.family {
            FontChoice::Declared
        } else if remapped && chain.first() == Some(&family) {
            FontChoice::InstalledRole
        } else {
            FontChoice::DeclaredFallback
        };
        let reason = if let Some(substitution) = &evidence.substitution {
            format!("weight substituted: {}", substitution.reason)
        } else if remapped && chain.first() == Some(&family) {
            format!("packaged role compatibility: {name} → {family}")
        } else if evidence.chosen_group > 0 {
            "declared fallback".to_owned()
        } else {
            String::new()
        };
        texts.insert(name.clone(), FontSelection { font, choice });
        owned_texts.insert(name.clone(), selection.owned());
        text_evidence.push(TextEvidence {
            record: name.clone(),
            family,
            face_index: face.map_or(0, |face| face.index),
            source_blake3: face.map_or(String::new(), |face| face.source.clone()),
            requested_weight: evidence.requested_weight,
            effective_weight: evidence.effective_weight,
            reason,
        });
    }
    let mut glyph_fonts = BTreeMap::new();
    let mut icons = BTreeMap::new();
    let mut icon_evidence = Vec::new();
    for glyph in &plan.glyphs {
        let (character, font) = receipt
            .icon(ICON_KEY, &glyph.name)
            .expect("successful batch resolves every validated required glyph");
        glyph_fonts.insert(glyph.name.clone(), font);
        icons.insert(
            glyph.key.clone(),
            Ready::Text(Icon::with_glyph(glyph.name.clone(), character, font)),
        );
        icon_evidence.push(IconEvidence {
            key: glyph.key.clone(),
            name: glyph.name.clone(),
            family: Some(glyph.family.clone()),
            style: Some(glyph.style.clone()),
            glyph: Some(character),
            weight: Some(glyph.weight),
            asset: None,
            source_blake3: Some(glyph.source_blake3.clone()),
            reason: "catalogue glyph in the declared face".into(),
        });
    }
    for (key, ready, evidence) in images {
        icons.insert(key, ready);
        icon_evidence.push(evidence);
    }
    let batch_evidence = receipt.evidence();
    let registry = RegistryEvidence {
        renderer_version_before: batch_evidence.renderer_version_before,
        renderer_version_after: batch_evidence.renderer_version_after,
        sources_added: batch_evidence.added_sources,
        sources_reused: batch_evidence.reused_sources,
        faces_added: batch_evidence.added_faces,
        faces_reused: batch_evidence.reused_faces,
        policies_added: batch_evidence.policies_added,
        policies_reused: batch_evidence.policies_reused,
        usage_before: batch_evidence.usage_before,
        usage_after: batch_evidence.usage_after,
        image: admission.usage_after,
    };
    let evidence = ResourceEvidence {
        set_id: Some(compact.set_id.clone()),
        manifest_blake3: Some(hex::encode(compact.digest)),
        text: text_evidence,
        icons: icon_evidence,
        registry: registry.clone(),
    };
    let mut updated = compact;
    // Every batch source is committed and permanently retained by the
    // registry. Reuse metadata shares that exact allocation, including any
    // source not selected as a primary face.
    for source in &mut updated.sources {
        source.bytes = process_registry()
            .retained_source(&source.bytes)
            .expect("successful registry batch retains every source");
    }
    updated.text_signature = text_signature(projection.type_records(), &updated);
    updated.texts = texts.clone();
    updated.owned_texts = owned_texts.clone();
    updated.text_evidence = evidence.text.clone();
    updated.glyph_fonts = glyph_fonts;
    updated.registry = registry;
    let receipt =
        PreparedResources::assemble(Some(binding), texts, owned_texts, icons, evidence, charges);
    let prepared = projection.prepare_with_resources(receipt)?;
    image_store().publish(admission);
    for asset in &mut updated.assets {
        asset.bytes = image_store()
            .source(&asset.blake3)
            .expect("successful admission retains every compact image source");
    }
    Ok((prepared, updated))
}

/// The honest no-set rescue: generic registered families per record, no
/// binding, no ready icons and per-requirement unavailable evidence. This is
/// never a fake Current; it is exactly what the host can prove.
fn generic_prepared(
    projection: Projection,
    requirements: &ResourceRequirements,
    check: &mut dyn FnMut() -> Result<(), Diagnostic>,
) -> Result<Prepared, Diagnostic> {
    let mut texts = BTreeMap::new();
    let mut text_evidence = Vec::new();
    let mut prepared = projection.prepare(|record, resolved| {
        check()?;
        let selection = toolkit::fonts::try_font_for(
            &resolved.family,
            &resolved.fallbacks,
            resolved.weight,
            resolved.generic == TypographyGeneric::Monospace,
            None,
            true,
        )
        .map_err(|error| fault(&format!("typography.{record}"), error))?;
        text_evidence.push(TextEvidence {
            record: record.to_owned(),
            family: resolved.family.clone(),
            face_index: 0,
            source_blake3: String::new(),
            requested_weight: resolved.weight,
            effective_weight: weight_value(selection.font.weight),
            reason: "generic rescue: no verified asset set".into(),
        });
        texts.insert(record.to_owned(), selection);
        Ok(selection)
    })?;
    let icon_evidence = requirements
        .icons()
        .iter()
        .map(|requirement| IconEvidence {
            key: requirement.key.clone(),
            name: requirement.name.clone(),
            family: None,
            style: None,
            glyph: None,
            weight: None,
            asset: None,
            source_blake3: None,
            reason: "no verified asset set".into(),
        })
        .collect();
    let evidence = ResourceEvidence {
        set_id: None,
        manifest_blake3: None,
        text: text_evidence,
        icons: icon_evidence,
        registry: RegistryEvidence::default().with_image_usage(),
    };
    let receipt = PreparedResources::assemble(
        None,
        texts,
        BTreeMap::new(),
        BTreeMap::new(),
        evidence,
        Vec::new(),
    );
    prepared.attach_resources(receipt);
    Ok(prepared)
}

fn weight_value(weight: Weight) -> u16 {
    match weight {
        Weight::Thin => 100,
        Weight::ExtraLight => 200,
        Weight::Light => 300,
        Weight::Normal => 400,
        Weight::Medium => 500,
        Weight::Semibold => 600,
        Weight::Bold => 700,
        Weight::ExtraBold => 800,
        Weight::Black => 900,
        Weight::Numeric(value) => value,
    }
}

fn hex_digest(hex: &str) -> Result<[u8; 32], Diagnostic> {
    let mut digest = [0u8; 32];
    hex::decode_to_slice(hex, &mut digest)
        .map_err(|error| fault("resources", format!("invalid manifest digest: {error}")))?;
    Ok(digest)
}

/// The compact metadata charge: names and tables, never the retained font or
/// encoded image bytes, which are charged separately by the registry and the
/// process image ledger.
fn metadata_charge(compact: &CompactSet) -> u64 {
    let mut bytes = (compact.set_id.len() + 32) as u64;
    if let Some(selector) = &compact.selector {
        bytes += (selector.family.len() + selector.style.len() + 2) as u64;
    }
    for (role, (_, claim)) in &compact.roles {
        bytes += (role.len() + claim.len() + 2) as u64;
    }
    if let Some(catalogue) = &compact.catalogue {
        bytes += (catalogue.family.len() + catalogue.style.len() + 64) as u64;
        for name in catalogue.glyphs.keys() {
            bytes += name.len() as u64 + 4;
        }
    }
    for asset in &compact.assets {
        bytes += (asset.name.len() + asset.style.len() + 32) as u64;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::Color;
    use settings::{Desktop, resolve};
    use sha2::Digest as _;
    static TESTS: Mutex<()> = Mutex::new(());

    /// The real variable Inter and Noto Sans the fixtures register:
    /// genuine bytes, so the registry parses intrinsic families and weights.
    const INTER: &[u8] = include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf");
    const NOTO: &[u8] = include_bytes!("../assets/test-fonts/NotoSans.ttf");
    const MONO: &[u8] = include_bytes!("../assets/test-fonts/JetBrainsMono.ttf");

    fn write_file(dir: &Path, relative: &str, bytes: &[u8]) -> serde_json::Value {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        serde_json::json!({
            "path": relative, "bytes": bytes.len(),
            "url": "https://example.org/font", "upstream": "https://example.org/",
            "revision": "pinned", "licence": "OFL-1.1",
            "sha256": hex::encode(sha2::Sha256::digest(bytes)),
            "blake3": blake3::hash(bytes).to_hex().to_string(),
        })
    }

    fn write_manifest(dir: &Path, json: &serde_json::Value) {
        let text = strict::encode_pretty(&strict::from_json(json)).unwrap();
        std::fs::write(dir.join(assets::MANIFEST_FILE), text).unwrap();
    }

    /// A distinct nonblank SVG per name: every named asset has its own exact
    /// source digest, so the ledger can tell its variants apart.
    fn svg(name: &str) -> String {
        let value = name.bytes().fold(0u32, |total, byte| {
            total.wrapping_mul(31).wrapping_add(u32::from(byte))
        });
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#{:06x}"/></svg>"##,
            value & 0xFF_FFFF
        )
    }

    /// A set covering every embedded design record: Inter doubles as the
    /// sans/display/mono roles (the packaged remap), variable Noto Sans covers the
    /// button records, and — with `icons` — one v2 icon catalogue whose
    /// declared family is the true intrinsic Inter family, a codepoints table
    /// holding a real glyph (`home` → 'a') and a deliberate missing glyph
    /// (`emoji` → U+1F600, absent from Inter's cmap), plus the named image
    /// assets. Returns the exact manifest bytes.
    fn publish_full(root: &Path, id: &str, icons: bool, images: &[&str]) -> Vec<u8> {
        let dir = root.join("sets").join(id);
        let mut entries = vec![
            write_file(&dir, "fonts/Sans.ttf", INTER),
            write_file(&dir, "fonts/Noto.ttf", NOTO),
            write_file(&dir, "fonts/Mono.ttf", MONO),
        ];
        let mut manifest = serde_json::json!({
            "fonts": {
                "sans": "fonts/Sans.ttf", "display": "fonts/Sans.ttf",
                "mono": "fonts/Mono.ttf", "extra": "fonts/Noto.ttf"
            },
            "font_families": {
                "sans": "Inter", "display": "Inter", "mono": "JetBrains Mono", "extra": "Noto Sans"
            },
            "web_css": "/* fixture */\n"
        });
        if icons {
            entries.push(write_file(
                &dir,
                "icons/Symbols.codepoints",
                b"home 61\nemoji 1F600\n",
            ));
            let assets = images
                .iter()
                .map(|name| {
                    entries.push(write_file(
                        &dir,
                        &format!("icons/{name}.svg"),
                        svg(name).as_bytes(),
                    ));
                    serde_json::json!({
                        "name": name, "style": "default",
                        "path": format!("icons/{name}.svg"), "symbolic": true
                    })
                })
                .collect::<Vec<_>>();
            manifest["schema"] = serde_json::json!(assets::SCHEMA_V2);
            manifest["set_id"] = serde_json::json!(id);
            manifest["icon_default"] =
                serde_json::json!({"family": "Inter", "style": "default", "weight": 400});
            manifest["icon_catalogues"] = serde_json::json!([{
                "family": "Inter", "style": "default", "font": "fonts/Sans.ttf",
                "face_index": 0, "codepoints": "icons/Symbols.codepoints"
            }]);
            manifest["icon_assets"] = serde_json::Value::Array(assets);
        } else {
            manifest["schema"] = serde_json::json!(assets::SCHEMA);
            manifest["set_id"] = serde_json::json!(id);
        }
        manifest["files"] = serde_json::Value::Array(entries);
        write_manifest(&dir, &manifest);
        std::fs::write(dir.join(assets::STYLESHEET_FILE), "/* fixture */\n").unwrap();
        std::fs::read(dir.join(assets::MANIFEST_FILE)).unwrap()
    }

    fn activate(root: &Path, id: &str) {
        std::os::unix::fs::symlink(Path::new("sets").join(id), root.join(assets::CURRENT_LINK))
            .unwrap();
    }

    fn projection() -> Projection {
        let effective = resolve(&Desktop::default()).unwrap();
        Projection::new(&effective["desktop"]).unwrap()
    }

    /// A projection whose every record resolves to the one family an explicit
    /// fixture declares: explicit references never remap packaged roles.
    fn explicit_projection() -> Projection {
        let mut effective = resolve(&Desktop::default())
            .unwrap()
            .remove("desktop")
            .unwrap();
        for record in effective.design.typography.values_mut() {
            record.family = "Inter".into();
            record.fallbacks = Vec::new();
        }
        Projection::new(&effective).unwrap()
    }

    fn host(root: &Path) -> ResourceHost {
        ResourceHost::new(vec![root.to_path_buf()].into_iter().collect())
    }

    fn check_ok() -> impl FnMut() -> Result<(), Diagnostic> {
        || Ok(())
    }

    fn digest(manifest: &[u8]) -> String {
        blake3::hash(manifest).to_hex().to_string()
    }

    fn reference(set_id: &str, manifest: &[u8]) -> ResourceReference {
        ResourceReference {
            schema: RESOURCE_SCHEMA,
            set_id: set_id.into(),
            manifest_blake3: digest(manifest),
            icons: None,
        }
    }

    fn requirement(key: &str, name: &str, tint: Color) -> ResourceRequirements {
        ResourceRequirements::new(vec![IconRequirement {
            key: key.into(),
            name: name.into(),
            logical_size: 16.0,
            scale: 1.0,
            tint,
        }])
        .unwrap()
    }

    fn image_handle(ready: Option<&Ready>) -> iced_core::image::Handle {
        match ready {
            Some(Ready::Image { handle, .. }) => handle.clone(),
            other => panic!("expected a decoded image, got {other:?}"),
        }
    }

    #[test]
    fn descriptor_default_keeps_authored_omission_across_warm_and_cold_captures() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        let manifest = publish_full(directory.path(), "default-icons", true, &["picture"]);
        activate(directory.path(), "default-icons");
        let tint = Color::from_rgba8(255, 255, 255, 1.0);
        let mut first_host = host(directory.path());
        let first = first_host
            .prepare(
                projection(),
                None,
                None,
                requirement("slot", "picture", tint),
                &mut check_ok(),
            )
            .unwrap();
        let binding = first.resources().unwrap().binding().unwrap().clone();
        assert_eq!(
            first.resources().unwrap().evidence().registry.image,
            image_usage()
        );
        assert!(binding.icons.is_none());
        let first_handle = image_handle(first.resources().unwrap().icon("slot"));
        first_host.roots.clear();
        let warm = first_host
            .prepare(
                projection(),
                None,
                Some(&binding),
                requirement("slot", "picture", tint),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(warm.resources().unwrap().binding(), Some(&binding));
        assert_eq!(
            image_handle(warm.resources().unwrap().icon("slot")).id(),
            first_handle.id()
        );
        let mut cold_host = host(directory.path());
        let cold = cold_host
            .prepare(
                projection(),
                None,
                Some(&binding),
                requirement("slot", "picture", tint),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(cold.resources().unwrap().binding(), Some(&binding));
        assert_eq!(
            cold.resources().unwrap().evidence().registry.image,
            image_usage()
        );
        let cold_handle = image_handle(cold.resources().unwrap().icon("slot"));
        assert_eq!(
            cold_handle.id(),
            first_handle.id(),
            "two hosts share the actual decoded payload"
        );
        assert!(Arc::ptr_eq(
            &first_host.compact[0].assets[0].bytes,
            &cold_host.compact[0].assets[0].bytes
        ));
        for source in &cold_host.compact[0].sources {
            assert!(Arc::ptr_eq(
                &source.bytes,
                &process_registry().retained_source(&source.bytes).unwrap()
            ));
        }
        let reference = reference("default-icons", &manifest);
        assert!(reference.icons.is_none());
        let explicit = host(directory.path())
            .prepare(
                explicit_projection(),
                Some(&reference),
                Some(&binding),
                requirement("slot", "picture", tint),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(explicit.resources().unwrap().binding(), Some(&binding));
        let usage = image_usage();
        drop((first, warm, cold, explicit, first_host, cold_host));
        assert_eq!(
            image_usage(),
            usage,
            "escaped handles stay permanently charged"
        );
        assert!(matches!(cold_handle, iced_core::image::Handle::Rgba { .. }));
    }

    #[test]
    fn cold_omission_pins_a_while_current_points_at_b() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        let manifest = publish_full(directory.path(), "cached-a", true, &["picture"]);
        publish_full(directory.path(), "current-b", true, &["picture"]);
        activate(directory.path(), "current-b");
        let reference_a = reference("cached-a", &manifest);
        let mut explicit_host = host(directory.path());
        let a = explicit_host
            .prepare(
                explicit_projection(),
                Some(&reference_a),
                None,
                ResourceRequirements::empty(),
                &mut check_ok(),
            )
            .unwrap();
        assert!(
            explicit_host.pin.is_none(),
            "authored references do not establish omission policy"
        );
        let binding = a.resources().unwrap().binding().unwrap().clone();
        let tint = Color::from_rgba8(255, 255, 255, 1.0);
        let mut cold = host(directory.path());
        let cached = cold
            .prepare(
                projection(),
                None,
                Some(&binding),
                requirement("slot", "picture", tint),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(cold.pin.as_ref().unwrap().set_id, "cached-a");
        let live = cold
            .prepare(
                projection(),
                None,
                None,
                requirement("slot", "picture", tint),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(live.resources().unwrap().binding(), Some(&binding));
        assert_eq!(
            image_handle(cached.resources().unwrap().icon("slot")).id(),
            image_handle(live.resources().unwrap().icon("slot")).id()
        );
        let unpinned = explicit_host
            .prepare(
                projection(),
                None,
                None,
                ResourceRequirements::empty(),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(
            unpinned.resources().unwrap().binding().unwrap().set_id,
            "current-b"
        );
    }

    #[test]
    fn rejected_batches_and_unused_images_never_publish_partial_image_admissions() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_image_ledger_for_tests();
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "staged", true, &["good", "later"]);
        activate(directory.path(), "staged");
        let mut host = host(directory.path());
        host.prepare(
            projection(),
            None,
            None,
            ResourceRequirements::empty(),
            &mut check_ok(),
        )
        .unwrap();
        assert_eq!(
            image_usage(),
            ImageUsage::default(),
            "text-only capture retains no unrequested image payloads"
        );
        let tint = Color::from_rgba8(255, 255, 255, 1.0);
        let requirements = ResourceRequirements::new(vec![
            IconRequirement {
                key: "good".into(),
                name: "good".into(),
                logical_size: 16.0,
                scale: 1.0,
                tint,
            },
            IconRequirement {
                key: "missing".into(),
                name: "absent".into(),
                logical_size: 16.0,
                scale: 1.0,
                tint,
            },
        ])
        .unwrap();
        let before = image_usage();
        assert!(
            host.prepare(projection(), None, None, requirements, &mut check_ok())
                .is_err()
        );
        assert_eq!(image_usage(), before);
        let mut effective = resolve(&Desktop::default())
            .unwrap()
            .remove("desktop")
            .unwrap();
        effective.design.typography.get_mut("ui").unwrap().family = "Missing Family".into();
        effective
            .design
            .typography
            .get_mut("ui")
            .unwrap()
            .fallbacks
            .clear();
        assert!(
            host.prepare(
                Projection::new(&effective).unwrap(),
                None,
                None,
                requirement("good", "good", tint),
                &mut check_ok()
            )
            .is_err()
        );
        assert_eq!(
            image_usage(),
            before,
            "registry refusal publishes neither encoded nor decoded images"
        );
        let identity = request_identity(None, None, host.pin.as_ref()).unwrap();
        let mut compact = host
            .read(
                &identity,
                &ResourceRequirements::new(vec![
                    IconRequirement {
                        key: "good".into(),
                        name: "good".into(),
                        logical_size: 16.0,
                        scale: 1.0,
                        tint,
                    },
                    IconRequirement {
                        key: "later".into(),
                        name: "later".into(),
                        logical_size: 16.0,
                        scale: 1.0,
                        tint,
                    },
                ])
                .unwrap(),
                &mut check_ok(),
            )
            .unwrap()
            .unwrap();
        compact
            .assets
            .iter_mut()
            .find(|asset| asset.name == "later")
            .unwrap()
            .bytes = Arc::from(&b"malformed SVG"[..]);
        let requirements = ResourceRequirements::new(vec![
            IconRequirement {
                key: "good".into(),
                name: "good".into(),
                logical_size: 16.0,
                scale: 1.0,
                tint,
            },
            IconRequirement {
                key: "later".into(),
                name: "later".into(),
                logical_size: 16.0,
                scale: 1.0,
                tint,
            },
        ])
        .unwrap();
        let plan = icon_plan(&requirements, &compact).unwrap();
        assert!(decode_images(&plan, &mut check_ok()).is_err());
        assert_eq!(
            image_usage(),
            before,
            "successful first decode remains private when the next decode fails"
        );
    }

    #[test]
    fn reuse_validates_changed_glyph_names_and_structured_family_chains() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "glyph-proof", true, &[]);
        activate(directory.path(), "glyph-proof");
        let tint = Color::from_rgba8(255, 255, 255, 1.0);
        let mut host = host(directory.path());
        host.prepare(
            projection(),
            None,
            None,
            requirement("action", "home", tint),
            &mut check_ok(),
        )
        .unwrap();
        let before = process_registry().usage();
        let version = toolkit::graphics::text::font_system()
            .read()
            .unwrap()
            .version();
        let error = host
            .prepare(
                projection(),
                None,
                None,
                requirement("action", "emoji", tint),
                &mut check_ok(),
            )
            .unwrap_err();
        assert!(error.message.contains("no glyph in its face"), "{error:?}");
        assert_eq!(process_registry().usage(), before);
        assert_eq!(
            toolkit::graphics::text::font_system()
                .read()
                .unwrap()
                .version(),
            version
        );
        let mut records = projection().type_records().clone();
        let record = records.get_mut("ui").unwrap();
        record.family = "A,B".into();
        record.fallbacks.clear();
        let one = text_signature(&records, &host.compact[0]);
        let record = records.get_mut("ui").unwrap();
        record.family = "A".into();
        record.fallbacks.push("B".into());
        let two = text_signature(&records, &host.compact[0]);
        assert_ne!(
            one, two,
            "family delimiters cannot forge a retained selection proof"
        );
    }

    #[test]
    fn cancellation_after_registry_commit_retains_the_reusable_capture() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "cancelled", true, &["picture"]);
        activate(directory.path(), "cancelled");
        let mut host = host(directory.path());
        let mut checks = 0;
        let mut count = || {
            checks += 1;
            Ok(())
        };
        host.prepare(
            projection(),
            None,
            None,
            ResourceRequirements::empty(),
            &mut count,
        )
        .unwrap();
        let final_check = checks;
        let mut host = super::tests::host(directory.path());
        let mut checks = 0;
        let mut cancel = || {
            checks += 1;
            if checks == final_check {
                Err(fault("resources", "cancelled after capture"))
            } else {
                Ok(())
            }
        };
        assert!(
            host.prepare(
                projection(),
                None,
                None,
                ResourceRequirements::empty(),
                &mut cancel
            )
            .is_err()
        );
        assert_eq!(host.compact.len(), 1);
        assert!(host.pin.is_some());
        host.roots.clear();
        let version = toolkit::graphics::text::font_system()
            .read()
            .unwrap()
            .version();
        host.prepare(
            projection(),
            None,
            None,
            ResourceRequirements::empty(),
            &mut check_ok(),
        )
        .unwrap();
        assert_eq!(
            toolkit::graphics::text::font_system()
                .read()
                .unwrap()
                .version(),
            version
        );
    }

    #[test]
    fn requirements_are_finite_unique_and_bounded() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(ResourceRequirements::new(Vec::new()).is_ok());
        let tint = Color::from_rgba8(255, 255, 255, 1.0);
        let fine = ResourceRequirements::new(vec![IconRequirement {
            key: "delete".into(),
            name: "delete".into(),
            logical_size: 16.0,
            scale: 2.0,
            tint,
        }])
        .unwrap();
        assert_eq!(fine.icons().len(), 1);
        for icons in [
            vec![
                IconRequirement {
                    key: "dup".into(),
                    name: "a".into(),
                    logical_size: 16.0,
                    scale: 1.0,
                    tint,
                };
                2
            ],
            vec![IconRequirement {
                key: String::new(),
                name: "a".into(),
                logical_size: 16.0,
                scale: 1.0,
                tint,
            }],
            vec![IconRequirement {
                key: "nan".into(),
                name: "a".into(),
                logical_size: f32::NAN,
                scale: 1.0,
                tint,
            }],
            vec![IconRequirement {
                key: "side".into(),
                name: "a".into(),
                logical_size: 2048.0,
                scale: 2.0,
                tint,
            }],
        ] {
            assert!(ResourceRequirements::new(icons).is_err());
        }
    }

    #[test]
    fn explicit_reference_verifies_bytes_and_returns_the_exact_binding() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        let manifest = publish_full(directory.path(), "one", false, &[]);
        let reference = reference("one", &manifest);
        let prepared = host(directory.path())
            .prepare(
                explicit_projection(),
                Some(&reference),
                None,
                ResourceRequirements::empty(),
                &mut check_ok(),
            )
            .unwrap();
        let receipt = prepared.resources().unwrap();
        let binding = receipt.binding().unwrap();
        assert_eq!(binding.set_id, "one");
        assert_eq!(binding.manifest_blake3, digest(&manifest));
        assert!(binding.icons.is_none());
        assert_eq!(binding.interpretation, settings::resource_interpretation());
        let evidence = receipt.evidence();
        assert_eq!(evidence.set_id.as_deref(), Some("one"));
        assert!(evidence.text.iter().any(|text| {
            text.record == "ui" && text.family == "Inter" && text.requested_weight == 300
        }));
        assert!(
            evidence.text.iter().all(|text| text.family == "Inter"),
            "explicit text resolves only the requested intrinsic family"
        );
    }

    #[test]
    fn omission_discovers_pins_and_reuses_without_registration() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "default", false, &[]);
        activate(directory.path(), "default");
        let mut host = host(directory.path());
        let first = host
            .prepare(
                projection(),
                None,
                None,
                ResourceRequirements::empty(),
                &mut check_ok(),
            )
            .unwrap();
        let binding = first.resources().unwrap().binding().unwrap().clone();
        assert_eq!(binding.set_id, "default");
        let first_registry = first.resources().unwrap().evidence().registry.clone();
        assert_ne!(
            first_registry.renderer_version_before,
            first_registry.renderer_version_after
        );
        let second = host
            .prepare(
                projection(),
                None,
                None,
                ResourceRequirements::empty(),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(second.resources().unwrap().binding(), Some(&binding));
        let second_registry = second.resources().unwrap().evidence().registry.clone();
        assert_eq!(
            second_registry.renderer_version_before, second_registry.renderer_version_after,
            "reuse registers nothing"
        );
        assert_eq!(
            second_registry.renderer_version_after,
            first_registry.renderer_version_after
        );
        // The pin never drifts: removing the set changes what the next
        // cold reader sees, never what this host already retains.
        std::fs::remove_dir_all(directory.path().join("sets/default")).unwrap();
        let third = host
            .prepare(
                projection(),
                None,
                None,
                ResourceRequirements::empty(),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(third.resources().unwrap().binding(), Some(&binding));
    }

    #[test]
    fn packaged_omission_remaps_default_roles_with_reported_evidence() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "default", false, &[]);
        activate(directory.path(), "default");
        let prepared = host(directory.path())
            .prepare(
                projection(),
                None,
                None,
                ResourceRequirements::empty(),
                &mut check_ok(),
            )
            .unwrap();
        let choices = prepared.font_choices();
        assert_eq!(choices["ui"], FontChoice::InstalledRole);
        assert_eq!(choices["ui_display"], FontChoice::InstalledRole);
        assert_eq!(choices["mono"], FontChoice::InstalledRole);
        assert_eq!(choices["terminal"], FontChoice::InstalledRole);
        assert_eq!(choices["button.md"], FontChoice::Declared);
        let evidence = prepared.resources().unwrap().evidence();
        let mono = evidence
            .text
            .iter()
            .find(|text| text.record == "mono")
            .unwrap();
        assert_eq!(mono.family, "JetBrains Mono");
        assert!(
            mono.reason.contains("packaged role compatibility"),
            "the remap must be reported: {}",
            mono.reason
        );
        assert_eq!(mono.requested_weight, 300);
        assert_eq!(
            mono.effective_weight, 300,
            "numeric weights are not bucketed"
        );
        let button = evidence
            .text
            .iter()
            .find(|text| text.record == "button.md")
            .unwrap();
        assert_eq!(button.family, "Noto Sans");
        assert!(button.reason.is_empty(), "{}", button.reason);
    }

    #[test]
    fn corrupt_or_missing_explicit_resources_are_terminal() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        let manifest = publish_full(directory.path(), "one", false, &[]);
        let mismatched = ResourceReference {
            schema: RESOURCE_SCHEMA,
            set_id: "one".into(),
            manifest_blake3: "0".repeat(64),
            icons: None,
        };
        assert!(
            host(directory.path())
                .prepare(
                    explicit_projection(),
                    Some(&mismatched),
                    None,
                    ResourceRequirements::empty(),
                    &mut check_ok(),
                )
                .is_err()
        );
        let missing = ResourceReference {
            schema: RESOURCE_SCHEMA,
            set_id: "absent".into(),
            manifest_blake3: digest(&manifest),
            icons: None,
        };
        assert!(
            host(directory.path())
                .prepare(
                    explicit_projection(),
                    Some(&missing),
                    None,
                    ResourceRequirements::empty(),
                    &mut check_ok(),
                )
                .is_err()
        );
    }

    #[test]
    fn ready_icons_carry_verified_glyphs_and_tinted_image_variants() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_image_ledger_for_tests();
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "icons", true, &["picture"]);
        activate(directory.path(), "icons");
        let tint = Color::from_rgba8(255, 255, 255, 1.0);
        let requirements = ResourceRequirements::new(vec![
            IconRequirement {
                key: "glyph".into(),
                name: "home".into(),
                logical_size: 16.0,
                scale: 1.0,
                tint,
            },
            IconRequirement {
                key: "image".into(),
                name: "picture".into(),
                logical_size: 24.0,
                scale: 1.0,
                tint,
            },
        ])
        .unwrap();
        let prepared = host(directory.path())
            .prepare(projection(), None, None, requirements, &mut check_ok())
            .unwrap();
        let receipt = prepared.resources().unwrap();
        match receipt.icon("glyph") {
            Some(Ready::Text(icon)) => assert_eq!(icon.name(), "home"),
            other => panic!("expected a text glyph, got {other:?}"),
        }
        assert!(matches!(receipt.icon("image"), Some(Ready::Image { .. })));
        let evidence = receipt.evidence();
        assert!(
            evidence
                .icons
                .iter()
                .any(|icon| icon.key == "image" && icon.asset.is_some())
        );
        assert!(
            evidence
                .icons
                .iter()
                .any(|icon| icon.key == "glyph" && icon.glyph == Some('a'))
        );
        let usage = image_usage();
        assert_eq!(usage.variants, 1, "one decoded image variant");
        assert_eq!(usage.sources, 1, "one encoded source charged once");
    }

    #[test]
    fn colour_only_change_reuses_selections_and_adds_only_the_new_variant() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_image_ledger_for_tests();
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "icons", true, &["picture"]);
        activate(directory.path(), "icons");
        let mut host = host(directory.path());
        let first = host
            .prepare(
                projection(),
                None,
                None,
                requirement("image", "picture", Color::from_rgba8(255, 255, 255, 1.0)),
                &mut check_ok(),
            )
            .unwrap();
        let version = first
            .resources()
            .unwrap()
            .evidence()
            .registry
            .renderer_version_after;
        let first_handle = image_handle(first.resources().unwrap().icon("image"));
        let second = host
            .prepare(
                projection(),
                None,
                None,
                requirement("image", "picture", Color::from_rgba8(0, 0, 0, 1.0)),
                &mut check_ok(),
            )
            .unwrap();
        let second_registry = second.resources().unwrap().evidence().registry.clone();
        assert_eq!(
            second_registry.renderer_version_before, second_registry.renderer_version_after,
            "colour-only reuse must not re-register"
        );
        assert_eq!(second_registry.renderer_version_after, version);
        let second_handle = image_handle(second.resources().unwrap().icon("image"));
        assert_ne!(first_handle.id(), second_handle.id());
        assert_eq!(image_usage().variants, 2);
        assert_eq!(image_usage().sources, 1);
    }

    #[test]
    fn repeated_a_b_colour_variants_charge_each_variant_once() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_image_ledger_for_tests();
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "icons", true, &["picture"]);
        activate(directory.path(), "icons");
        let mut host = host(directory.path());
        let white = Color::from_rgba8(255, 255, 255, 1.0);
        let black = Color::from_rgba8(0, 0, 0, 1.0);
        let mut kept = Vec::new();
        let mut white_handles = Vec::new();
        let mut black_handles = Vec::new();
        for _ in 0..4 {
            for tint in [white, black] {
                let prepared = host
                    .prepare(
                        projection(),
                        None,
                        None,
                        requirement("image", "picture", tint),
                        &mut check_ok(),
                    )
                    .unwrap();
                let handle = image_handle(prepared.resources().unwrap().icon("image"));
                if tint == white {
                    white_handles.push(handle);
                } else {
                    black_handles.push(handle);
                }
                kept.push(prepared);
            }
        }
        let usage = image_usage();
        assert_eq!(
            usage.variants, 2,
            "A/B alternation must never grow capacity"
        );
        assert_eq!(usage.sources, 1, "one encoded source charged once");
        for pair in white_handles.windows(2) {
            assert_eq!(pair[0].id(), pair[1].id(), "the same variant is shared");
        }
        for pair in black_handles.windows(2) {
            assert_eq!(pair[0].id(), pair[1].id(), "the same variant is shared");
        }
        assert_ne!(white_handles[0].id(), black_handles[0].id());
    }

    #[test]
    fn late_icon_failure_leaves_font_version_aliases_and_usage_unchanged() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let directory = tempfile::tempdir().unwrap();
        publish_full(directory.path(), "icons", true, &[]);
        activate(directory.path(), "icons");
        let mut host = host(directory.path());
        host.prepare(
            projection(),
            None,
            None,
            ResourceRequirements::empty(),
            &mut check_ok(),
        )
        .unwrap();
        let usage_before = process_registry().usage();
        let version_before = toolkit::graphics::text::font_system()
            .read()
            .unwrap()
            .version();
        // A weight change routes through a fresh batch; its last icon name is
        // declared by the catalogue but absent from the face's cmap.
        let mut effective = resolve(&Desktop::default())
            .unwrap()
            .remove("desktop")
            .unwrap();
        effective.design.typography.get_mut("ui").unwrap().weight = 350;
        let changed = Projection::new(&effective).unwrap();
        let tint = Color::from_rgba8(255, 255, 255, 1.0);
        let requirements = ResourceRequirements::new(vec![IconRequirement {
            key: "emoji".into(),
            name: "emoji".into(),
            logical_size: 16.0,
            scale: 1.0,
            tint,
        }])
        .unwrap();
        let error = host
            .prepare(changed, None, None, requirements, &mut check_ok())
            .unwrap_err();
        assert!(error.message.contains("no glyph in its face"), "{error:?}");
        assert_eq!(process_registry().usage(), usage_before);
        assert_eq!(
            toolkit::graphics::text::font_system()
                .read()
                .unwrap()
                .version(),
            version_before
        );
    }

    #[test]
    fn two_hosts_share_the_process_image_ledger_and_old_handles_force_refusal() {
        let _test = TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_image_ledger_for_tests();
        let tint = Color::from_rgba8(255, 255, 255, 1.0);
        // Host A: one variant under a fresh omission; the receipt stays alive
        // for the whole test so its charge cannot be reclaimed.
        let root_a = tempfile::tempdir().unwrap();
        publish_full(root_a.path(), "alpha", true, &["pict-single"]);
        activate(root_a.path(), "alpha");
        let mut host_a = host(root_a.path());
        let kept = host_a
            .prepare(
                projection(),
                None,
                None,
                requirement("pict-single", "pict-single", tint),
                &mut check_ok(),
            )
            .unwrap();
        assert_eq!(image_usage().variants, 1);
        // Host B: three explicit sets with 250/250/12 distinct assets.
        let root_b = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..512).map(|index| format!("pict-{index:03}")).collect();
        let slices = [&names[0..250], &names[250..500], &names[500..512]];
        let mut references = Vec::new();
        for (index, slice) in slices.iter().enumerate() {
            let id = format!("beta-{index}");
            let listed: Vec<&str> = slice.iter().map(String::as_str).collect();
            let manifest = publish_full(root_b.path(), &id, true, &listed);
            references.push(reference(&id, &manifest));
        }
        let mut host_b = host(root_b.path());
        let requirements = |listed: &[String]| {
            ResourceRequirements::new(
                listed
                    .iter()
                    .map(|name| IconRequirement {
                        key: name.clone(),
                        name: name.clone(),
                        logical_size: 16.0,
                        scale: 1.0,
                        tint,
                    })
                    .collect(),
            )
            .unwrap()
        };
        let mut kept_b = Vec::new();
        for (reference, slice) in [
            (&references[0], &names[0..250]),
            (&references[1], &names[250..500]),
            (&references[2], &names[500..511]),
        ] {
            kept_b.push(
                host_b
                    .prepare(
                        explicit_projection(),
                        Some(reference),
                        None,
                        requirements(slice),
                        &mut check_ok(),
                    )
                    .unwrap(),
            );
        }
        assert_eq!(image_usage().variants, MAX_RETAINED_VARIANTS);
        // The 513th distinct variant is refused while the old handles live:
        // the ledger never evicts, so retained charges stay charged.
        let refusal = host_b
            .prepare(
                explicit_projection(),
                Some(&references[2]),
                None,
                requirements(&names[511..512]),
                &mut check_ok(),
            )
            .unwrap_err();
        assert_eq!(refusal.code, "image_capacity");
        assert_eq!(
            image_usage().variants,
            MAX_RETAINED_VARIANTS,
            "refusal must not evict or uncharge anything"
        );
        // Escaped public handles and discarded receipts remain accounted for:
        // the canonical store retains each admitted variant until exit.
        let escaped = image_handle(kept.resources().unwrap().icon("pict-single"));
        drop(kept);
        assert_eq!(image_usage().variants, MAX_RETAINED_VARIANTS);
        assert!(matches!(escaped, iced_core::image::Handle::Rgba { .. }));
        let refusal = host_b
            .prepare(
                explicit_projection(),
                Some(&references[2]),
                None,
                requirements(&names[511..512]),
                &mut check_ok(),
            )
            .unwrap_err();
        assert_eq!(refusal.code, "image_capacity");
        assert_eq!(image_usage().variants, MAX_RETAINED_VARIANTS);
        drop(kept_b);
    }
}
