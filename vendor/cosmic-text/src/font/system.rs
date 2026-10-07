use crate::{Attrs, Font, FontMatchAttrs, HashMap, ShapeBuffer};
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::{Deref, DerefMut};
use fontdb::{FaceInfo, Family, Query, Style};
use skrifa::raw::{ReadError, TableProvider as _};
use skrifa::MetadataProvider;

// re-export fontdb and harfrust
pub use fontdb;
pub use harfrust;

use super::fallback::{Fallback, Fallbacks, MonospaceFallbackInfo, PlatformFallback};

// The fields are used in the derived Ord implementation for sorting fallback candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontMatchKey {
    pub(crate) not_emoji: bool,
    pub(crate) font_weight_diff: u16,
    pub(crate) font_stretch_diff: u16,
    pub(crate) font_style_diff: u8,
    pub(crate) font_weight: u16,
    pub(crate) font_stretch: u16,
    pub(crate) id: fontdb::ID,
    pub(crate) variable_weight_match: bool,
}

impl FontMatchKey {
    fn new(attrs: &Attrs, face: &FaceInfo, db: &fontdb::Database) -> FontMatchKey {
        // TODO: smarter way of detecting emoji
        let not_emoji = !face.post_script_name.contains("Emoji");
        let font_weight_diff = attrs.weight.0.abs_diff(face.weight.0);

        let variable_weight_match = font_weight_diff != 0
            && db.with_face_data(face.id, |font_data, face_index| {
                let font_ref = skrifa::FontRef::from_index(font_data, face_index).ok()?;
                let axis = font_ref.axes().get_by_tag(skrifa::Tag::new(b"wght"))?;
                let w = attrs.weight.0 as f32;
                Some(w >= axis.min_value() && w <= axis.max_value())
            }) == Some(Some(true));
        let font_weight = face.weight.0;
        let font_stretch_diff = attrs.stretch.to_number().abs_diff(face.stretch.to_number());
        let font_stretch = face.stretch.to_number();
        let font_style_diff = match (attrs.style, face.style) {
            (Style::Normal, Style::Normal)
            | (Style::Italic, Style::Italic)
            | (Style::Oblique, Style::Oblique) => 0,
            (Style::Italic, Style::Oblique) | (Style::Oblique, Style::Italic) => 1,
            (Style::Normal, Style::Italic)
            | (Style::Normal, Style::Oblique)
            | (Style::Italic, Style::Normal)
            | (Style::Oblique, Style::Normal) => 2,
        };
        let id = face.id;
        FontMatchKey {
            not_emoji,
            font_weight_diff,
            font_stretch_diff,
            font_style_diff,
            font_weight,
            font_stretch,
            id,
            variable_weight_match,
        }
    }
}

struct FontCachedCodepointSupportInfo {
    supported: Vec<u32>,
    not_supported: Vec<u32>,
}

impl FontCachedCodepointSupportInfo {
    const SUPPORTED_MAX_SZ: usize = 512;
    const NOT_SUPPORTED_MAX_SZ: usize = 1024;

    fn new() -> Self {
        Self {
            supported: Vec::with_capacity(Self::SUPPORTED_MAX_SZ),
            not_supported: Vec::with_capacity(Self::NOT_SUPPORTED_MAX_SZ),
        }
    }

    #[inline(always)]
    fn unknown_has_codepoint(
        &mut self,
        font_codepoints: &[u32],
        codepoint: u32,
        supported_insert_pos: usize,
        not_supported_insert_pos: usize,
    ) -> bool {
        let ret = font_codepoints.contains(&codepoint);
        if ret {
            // don't bother inserting if we are going to truncate the entry away
            if supported_insert_pos != Self::SUPPORTED_MAX_SZ {
                self.supported.insert(supported_insert_pos, codepoint);
                self.supported.truncate(Self::SUPPORTED_MAX_SZ);
            }
        } else {
            // don't bother inserting if we are going to truncate the entry away
            if not_supported_insert_pos != Self::NOT_SUPPORTED_MAX_SZ {
                self.not_supported
                    .insert(not_supported_insert_pos, codepoint);
                self.not_supported.truncate(Self::NOT_SUPPORTED_MAX_SZ);
            }
        }
        ret
    }

    #[inline(always)]
    fn has_codepoint(&mut self, font_codepoints: &[u32], codepoint: u32) -> bool {
        match self.supported.binary_search(&codepoint) {
            Ok(_) => true,
            Err(supported_insert_pos) => match self.not_supported.binary_search(&codepoint) {
                Ok(_) => false,
                Err(not_supported_insert_pos) => self.unknown_has_codepoint(
                    font_codepoints,
                    codepoint,
                    supported_insert_pos,
                    not_supported_insert_pos,
                ),
            },
        }
    }
}

/// How many faces a single [`FontRegistration`] may add.
const MAX_REGISTRATION_FACES: usize = 512;
/// How many pinned alias policies the system may hold in total.
const MAX_PINNED_POLICIES: usize = 1024;
/// How many fallback groups a single policy may declare.
const MAX_POLICY_GROUPS: usize = 16;
/// How many face references a single policy may hold in total.
const MAX_POLICY_FACE_REFS: usize = 64;
/// How many characters an alias name may have.
const MAX_ALIAS_CHARS: usize = 256;

/// A face reference inside a [`FontRegistration`]: either a face already in
/// the database (by ID) or a face added by the same transaction (by index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinnedFaceRef {
    /// A face that already exists in the database.
    Existing(fontdb::ID),
    /// A face from this transaction's [`FontRegistration::faces`], by index.
    Added(usize),
}

/// A pinned alias policy: an alias name, ordered fallback groups of face
/// references, and the immutable effective weight every group face is
/// instantiated at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedFontPolicy {
    /// The alias family name that selects this policy.
    ///
    /// Aliases are internal, caller-generated names. Conflict checks compare
    /// them ASCII-case-insensitively (against other aliases and against
    /// public family names), but pinned lookup itself is exact, so an alias
    /// must always be used verbatim, as generated.
    pub alias: String,
    /// Ordered fallback groups. Group order is preserved exactly; within a
    /// group faces are ranked by requested style and stretch with declared
    /// order breaking ties.
    pub groups: Vec<Vec<PinnedFaceRef>>,
    /// The sealed effective weight of every face in this policy.
    pub weight: fontdb::Weight,
}

/// A registration transaction: faces to add plus alias policies to install,
/// committed atomically by [`FontSystem::register_fonts`].
#[derive(Clone, Debug)]
pub struct FontRegistration {
    /// Faces to append to the database. Sources must be
    /// [`fontdb::Source::Binary`].
    ///
    /// Face metadata (families, post-script name, style, weight, stretch,
    /// monospaced flag) is caller-supplied and trusted: the seam validates
    /// that the source bytes parse at the declared index and can provide the
    /// sealed policy weight, not that the declared metadata matches the
    /// bytes. The toolkit registry derives intrinsic metadata from the bytes
    /// before constructing these records; the guard tests' impostor faces
    /// are test-only examples of caller-supplied metadata.
    pub faces: Vec<fontdb::FaceInfo>,
    /// Pinned alias policies to install.
    pub policies: Vec<PinnedFontPolicy>,
}

/// The outcome of a successful [`FontSystem::register_fonts`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontRegistrationResult {
    /// The database IDs of the newly added faces.
    pub added_faces: Vec<fontdb::ID>,
    /// How many policies were newly installed. A policy identical to an
    /// already installed one is a no-op and is not counted.
    pub policies_added: usize,
}

/// Why [`FontSystem::register_fonts`] failed. The font system is unchanged on
/// error: database, policies, derived indexes and caches are all left as they
/// were.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontRegistrationError {
    /// One transaction holds more faces than allowed.
    TooManyFaces {
        /// The allowed maximum.
        limit: usize,
    },
    /// The system would hold more policies than allowed.
    TooManyPolicies {
        /// The allowed maximum.
        limit: usize,
    },
    /// A face source is not [`fontdb::Source::Binary`].
    NonBinarySource {
        /// Index into [`FontRegistration::faces`].
        face_index: usize,
    },
    /// A face's bytes do not parse at its declared index.
    UnconstructibleFace {
        /// Index into [`FontRegistration::faces`].
        face_index: usize,
    },
    /// A policy references an existing ID that is not in the database.
    MissingExistingFace {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// The referenced ID.
        id: fontdb::ID,
    },
    /// A policy references an added face index outside its transaction.
    OutOfRangeAddedFace {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// The out-of-range [`PinnedFaceRef::Added`] index.
        face_index: usize,
    },
    /// A policy has no groups.
    EmptyPolicy {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
    },
    /// A policy has an empty group.
    EmptyGroup {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// Index into the policy's groups.
        group_index: usize,
    },
    /// A policy holds more groups than allowed.
    TooManyGroups {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// The allowed maximum.
        limit: usize,
    },
    /// A policy references more faces than allowed.
    TooManyFaceRefs {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// The allowed maximum.
        limit: usize,
    },
    /// An alias is empty or longer than allowed.
    InvalidAlias {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// The alias length in characters.
        len: usize,
    },
    /// Two policies in the same transaction use the same alias.
    DuplicateAlias(String),
    /// An alias conflicts with an installed pinned alias (different resolved
    /// policy) or with an existing public family name.
    ConflictingAlias(String),
    /// A policy weight is outside the supported 1..=1000 range.
    InvalidWeight {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// The rejected weight.
        weight: fontdb::Weight,
    },
    /// A policy face cannot provide the sealed weight: its static weight does
    /// not match and it has no covering `wght` variation axis. No
    /// nearest-weight substitution is performed.
    UnsupportedWeight {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// Index into the policy's groups.
        group_index: usize,
        /// Index into the group's face references.
        ref_index: usize,
        /// The sealed weight that could not be provided.
        weight: fontdb::Weight,
    },
    /// A policy face could not be instantiated at the sealed weight.
    UnconstructiblePolicyFace {
        /// Index into [`FontRegistration::policies`].
        policy_index: usize,
        /// The face ID that failed to instantiate.
        id: fontdb::ID,
        /// The sealed weight.
        weight: fontdb::Weight,
    },
}

/// A resolved pinned policy: owned groups of actual database IDs plus the
/// sealed weight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PinnedPolicy {
    pub(crate) groups: Vec<Vec<fontdb::ID>>,
    pub(crate) weight: fontdb::Weight,
}

/// Access to the system fonts.
pub struct FontSystem {
    /// The locale of the system.
    locale: String,

    /// The underlying font database.
    db: fontdb::Database,

    /// Cache for loaded fonts from the database.
    font_cache: HashMap<(fontdb::ID, fontdb::Weight), Option<Arc<Font>>>,

    /// Sorted unique ID's of all Monospace fonts in DB
    monospace_font_ids: Vec<fontdb::ID>,

    /// Sorted unique ID's of all Monospace fonts in DB per script.
    /// A font may support multiple scripts of course, so the same ID
    /// may appear in multiple map value vecs.
    per_script_monospace_font_ids: HashMap<[u8; 4], Vec<fontdb::ID>>,

    /// Installed pinned alias policies, keyed by alias. Lookup distinguishes
    /// a pinned alias from ordinary public families; a policy's resolved
    /// groups and weight never change after installation.
    pinned_policies: HashMap<String, PinnedPolicy>,

    /// Cache for font codepoint support info
    font_codepoint_support_info_cache: HashMap<fontdb::ID, FontCachedCodepointSupportInfo>,

    /// Cache for font matches.
    font_matches_cache: HashMap<FontMatchAttrs, Arc<Vec<FontMatchKey>>>,

    /// Scratch buffer for shaping and laying out.
    pub(crate) shape_buffer: ShapeBuffer,

    /// Buffer for use in `FontFallbackIter`.
    pub(crate) monospace_fallbacks_buffer: BTreeSet<MonospaceFallbackInfo>,

    /// Cache for shaped runs
    #[cfg(feature = "shape-run-cache")]
    pub shape_run_cache: crate::ShapeRunCache,

    /// List of fallbacks
    pub(crate) dyn_fallback: Box<dyn Fallback>,

    /// List of fallbacks
    pub(crate) fallbacks: Fallbacks,
}

impl fmt::Debug for FontSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontSystem")
            .field("locale", &self.locale)
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl FontSystem {
    const FONT_MATCHES_CACHE_SIZE_LIMIT: usize = 256;
    /// Create a new [`FontSystem`], that allows access to any installed system fonts
    ///
    /// # Timing
    ///
    /// This function takes some time to run. On the release build, it can take up to a second,
    /// while debug builds can take up to ten times longer. For this reason, it should only be
    /// called once, and the resulting [`FontSystem`] should be shared.
    pub fn new() -> Self {
        Self::new_with_fonts(core::iter::empty())
    }

    /// Create a new [`FontSystem`] with a pre-specified set of fonts.
    pub fn new_with_fonts(fonts: impl IntoIterator<Item = fontdb::Source>) -> Self {
        let locale = Self::get_locale();
        log::debug!("Locale: {locale}");

        let mut db = fontdb::Database::new();

        Self::load_fonts(&mut db, fonts.into_iter());

        //TODO: configurable default fonts
        db.set_monospace_family("Noto Sans Mono");
        db.set_sans_serif_family("Open Sans");
        db.set_serif_family("DejaVu Serif");

        Self::new_with_locale_and_db_and_fallback(locale, db, PlatformFallback)
    }

    /// Create a new [`FontSystem`] with a pre-specified locale, font database and font fallback list.
    pub fn new_with_locale_and_db_and_fallback(
        locale: String,
        db: fontdb::Database,
        impl_fallback: impl Fallback + 'static,
    ) -> Self {
        let (monospace_font_ids, per_script_monospace_font_ids) =
            Self::derive_monospace_indexes(&db);

        let fallbacks = Fallbacks::new(&impl_fallback, &[], &locale);

        Self {
            locale,
            db,
            monospace_font_ids,
            per_script_monospace_font_ids,
            pinned_policies: HashMap::default(),
            font_cache: HashMap::default(),
            font_matches_cache: HashMap::default(),
            font_codepoint_support_info_cache: HashMap::default(),
            monospace_fallbacks_buffer: BTreeSet::default(),
            #[cfg(feature = "shape-run-cache")]
            shape_run_cache: crate::ShapeRunCache::default(),
            shape_buffer: ShapeBuffer::default(),
            dyn_fallback: Box::new(impl_fallback),
            fallbacks,
        }
    }

    /// Create a new [`FontSystem`] with a pre-specified locale and font database.
    pub fn new_with_locale_and_db(locale: String, db: fontdb::Database) -> Self {
        Self::new_with_locale_and_db_and_fallback(locale, db, PlatformFallback)
    }

    /// Derive the sorted monospace ID list and the per-script monospace map
    /// from a database. GPOS and GSUB are read independently so a face that
    /// carries only one of the tables still contributes its script tags.
    fn derive_monospace_indexes(
        db: &fontdb::Database,
    ) -> (Vec<fontdb::ID>, HashMap<[u8; 4], Vec<fontdb::ID>>) {
        let mut monospace_font_ids = db
            .faces()
            .filter(|face_info| {
                face_info.monospaced && !face_info.post_script_name.contains("Emoji")
            })
            .map(|face_info| face_info.id)
            .collect::<Vec<_>>();
        monospace_font_ids.sort();
        monospace_font_ids.dedup();

        let mut per_script_monospace_font_ids: HashMap<[u8; 4], BTreeSet<fontdb::ID>> =
            HashMap::default();

        if cfg!(feature = "monospace_fallback") {
            for &id in &monospace_font_ids {
                db.with_face_data(id, |font_data, face_index| {
                    let face = skrifa::FontRef::from_index(font_data, face_index)?;

                    // Read both layout tables independently and merge the
                    // script tags: the chained `gpos()?`/`gsub()?` extraction
                    // loses valid scripts from one table when the other is
                    // missing.
                    let mut scripts = Vec::new();
                    if let Some(gpos) = face.gpos().ok().and_then(|table| table.script_list().ok())
                    {
                        scripts.extend(
                            gpos.script_records()
                                .iter()
                                .map(|script| script.script_tag().into_bytes()),
                        );
                    }
                    if let Some(gsub) = face.gsub().ok().and_then(|table| table.script_list().ok())
                    {
                        scripts.extend(
                            gsub.script_records()
                                .iter()
                                .map(|script| script.script_tag().into_bytes()),
                        );
                    }
                    scripts.sort();
                    scripts.dedup();

                    for script in scripts {
                        per_script_monospace_font_ids
                            .entry(script)
                            .or_default()
                            .insert(id);
                    }
                    Ok::<_, ReadError>(())
                });
            }
        }

        let per_script_monospace_font_ids = per_script_monospace_font_ids
            .into_iter()
            .map(|(k, v)| (k, Vec::from_iter(v)))
            .collect();

        (monospace_font_ids, per_script_monospace_font_ids)
    }

    /// Rebuild the derived database indexes and clear the caches that depend
    /// on them after the database was mutated through [`FontSystem::db_mut`].
    ///
    /// `db_mut` can only clear the match cache before it hands out its
    /// reference; it cannot rebuild the monospace indexes or the shape caches
    /// after a later mutation. Callers that add or remove faces must call
    /// this after their last mutation. Loaded fonts and glyph caches are
    /// retained.
    pub fn refresh_database(&mut self) {
        let (monospace_font_ids, per_script_monospace_font_ids) =
            Self::derive_monospace_indexes(&self.db);
        self.monospace_font_ids = monospace_font_ids;
        self.per_script_monospace_font_ids = per_script_monospace_font_ids;
        self.font_matches_cache.clear();
        self.font_codepoint_support_info_cache.clear();
        self.monospace_fallbacks_buffer.clear();
        #[cfg(feature = "shape-run-cache")]
        {
            self.shape_run_cache = crate::ShapeRunCache::default();
        }
    }

    /// The installed pinned alias names.
    pub fn pinned_aliases(&self) -> impl Iterator<Item = &str> {
        self.pinned_policies.keys().map(String::as_str)
    }

    /// The resolved policy of a pinned alias, if one is installed.
    pub(crate) fn pinned_policy(&self, alias: &str) -> Option<&PinnedPolicy> {
        self.pinned_policies.get(alias)
    }

    /// Atomically add faces and install pinned alias policies.
    ///
    /// Every face is validated, the whole transaction is staged against a
    /// clone of the database, and every new `(face, policy weight)` instance
    /// is constructed *before* any live state is touched. On success the new
    /// database, policy map, derived monospace indexes and instantiated fonts
    /// are committed together; existing IDs, loaded fonts and glyph caches
    /// are retained, while the match and shape caches are cleared. On any
    /// returned error nothing has changed.
    ///
    /// Added face sources must be [`fontdb::Source::Binary`]. A policy face
    /// must provide the sealed weight exactly (static weight) or through a
    /// covering `wght` variation axis; no nearest-weight substitution is
    /// implicit. A policy identical to an installed one is a no-op; a
    /// conflicting alias fails and can never be rebound. An empty or
    /// identical transaction does not mutate or invalidate anything.
    ///
    /// Allocation failure aborts the process, as elsewhere in Rust; no other
    /// failure can occur after the commit point.
    ///
    /// # Errors
    ///
    /// See [`FontRegistrationError`].
    pub fn register_fonts(
        &mut self,
        registration: FontRegistration,
    ) -> Result<FontRegistrationResult, FontRegistrationError> {
        if registration.faces.len() > MAX_REGISTRATION_FACES {
            return Err(FontRegistrationError::TooManyFaces {
                limit: MAX_REGISTRATION_FACES,
            });
        }
        if registration.policies.len() > MAX_PINNED_POLICIES {
            return Err(FontRegistrationError::TooManyPolicies {
                limit: MAX_PINNED_POLICIES,
            });
        }

        // Validate faces: binary sources only, parseable at the declared index.
        for (face_index, face) in registration.faces.iter().enumerate() {
            let fontdb::Source::Binary(data) = &face.source else {
                return Err(FontRegistrationError::NonBinarySource { face_index });
            };
            if skrifa::FontRef::from_index((*data).as_ref(), face.index).is_err() {
                return Err(FontRegistrationError::UnconstructibleFace { face_index });
            }
        }

        // Validate policies against the live state, without mutating it.
        let mut aliases_seen: HashMap<&str, usize> = HashMap::default();
        for (policy_index, policy) in registration.policies.iter().enumerate() {
            let alias_chars = policy.alias.chars().count();
            if alias_chars == 0 || alias_chars > MAX_ALIAS_CHARS {
                return Err(FontRegistrationError::InvalidAlias {
                    policy_index,
                    len: alias_chars,
                });
            }
            if aliases_seen.insert(&policy.alias, policy_index).is_some() {
                return Err(FontRegistrationError::DuplicateAlias(policy.alias.clone()));
            }
            if policy.weight.0 == 0 || policy.weight.0 > 1000 {
                return Err(FontRegistrationError::InvalidWeight {
                    policy_index,
                    weight: policy.weight,
                });
            }
            if policy.groups.is_empty() {
                return Err(FontRegistrationError::EmptyPolicy { policy_index });
            }
            if policy.groups.len() > MAX_POLICY_GROUPS {
                return Err(FontRegistrationError::TooManyGroups {
                    policy_index,
                    limit: MAX_POLICY_GROUPS,
                });
            }
            let total_refs = policy.groups.iter().map(Vec::len).sum::<usize>();
            if total_refs > MAX_POLICY_FACE_REFS {
                return Err(FontRegistrationError::TooManyFaceRefs {
                    policy_index,
                    limit: MAX_POLICY_FACE_REFS,
                });
            }
            for (group_index, group) in policy.groups.iter().enumerate() {
                if group.is_empty() {
                    return Err(FontRegistrationError::EmptyGroup {
                        policy_index,
                        group_index,
                    });
                }
                for (ref_index, face_ref) in group.iter().enumerate() {
                    let supported = match face_ref {
                        PinnedFaceRef::Existing(id) => {
                            let Some(face) = self.db.face(*id) else {
                                return Err(FontRegistrationError::MissingExistingFace {
                                    policy_index,
                                    id: *id,
                                });
                            };
                            self.db.with_face_data(*id, |data, index| {
                                Some(face_data_supports_weight(
                                    data,
                                    index,
                                    face.weight,
                                    policy.weight,
                                ))
                            }) == Some(Some(true))
                        }
                        PinnedFaceRef::Added(face_index) => {
                            let Some(face) = registration.faces.get(*face_index) else {
                                return Err(FontRegistrationError::OutOfRangeAddedFace {
                                    policy_index,
                                    face_index: *face_index,
                                });
                            };
                            let fontdb::Source::Binary(data) = &face.source else {
                                return Err(FontRegistrationError::NonBinarySource {
                                    face_index: *face_index,
                                });
                            };
                            face_data_supports_weight(
                                (*data).as_ref(),
                                face.index,
                                face.weight,
                                policy.weight,
                            )
                        }
                    };
                    if !supported {
                        return Err(FontRegistrationError::UnsupportedWeight {
                            policy_index,
                            group_index,
                            ref_index,
                            weight: policy.weight,
                        });
                    }
                }
            }
            // An alias may not shadow an ordinary public family name —
            // neither one already in the database nor one declared by a face
            // added in this same transaction — or the pinned policy would
            // capture selections that predate the registration. The
            // comparison is ASCII-case-insensitive, matching the alias
            // conflict semantics; pinned lookup itself stays exact.
            let family_capture = self.db.faces().any(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(&policy.alias))
            }) || registration.faces.iter().any(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(&policy.alias))
            });
            if family_capture {
                return Err(FontRegistrationError::ConflictingAlias(
                    policy.alias.clone(),
                ));
            }
        }

        // Stage: cloning the database preserves every existing slotmap ID,
        // and `push_face_info` assigns fresh IDs to the new faces.
        let mut staged_db = self.db.clone();
        let mut added_faces = Vec::with_capacity(registration.faces.len());
        for face in &registration.faces {
            added_faces.push(staged_db.push_face_info(face.clone()));
        }

        let (monospace_font_ids, per_script_monospace_font_ids) =
            Self::derive_monospace_indexes(&staged_db);

        // Resolve policies against the staged database and instantiate every
        // distinct policy `(face, weight)` instance at the sealed weight.
        // Identical installed policies are no-ops; conflicting ones fail
        // here, before any live mutation. `Font::new` is deterministic per
        // `(id, weight)` on the fixed staged database, so shared instances
        // are validated once.
        let mut staged_policies: Vec<(String, PinnedPolicy)> = Vec::new();
        let mut instantiated_fonts: Vec<(fontdb::ID, fontdb::Weight, Arc<Font>)> = Vec::new();
        let mut staged_instances: HashMap<(fontdb::ID, fontdb::Weight), ()> = HashMap::default();
        for (policy_index, policy) in registration.policies.iter().enumerate() {
            let mut groups = Vec::with_capacity(policy.groups.len());
            for group in &policy.groups {
                let mut ids = Vec::with_capacity(group.len());
                for face_ref in group {
                    ids.push(match face_ref {
                        PinnedFaceRef::Existing(id) => *id,
                        PinnedFaceRef::Added(index) => added_faces[*index],
                    });
                }
                groups.push(ids);
            }
            let resolved = PinnedPolicy {
                groups,
                weight: policy.weight,
            };
            match self.pinned_policies.get(&policy.alias) {
                Some(installed) if installed == &resolved => continue,
                Some(_) => {
                    return Err(FontRegistrationError::ConflictingAlias(
                        policy.alias.clone(),
                    ));
                }
                None => {}
            }
            for group in &resolved.groups {
                for &id in group {
                    if staged_instances.insert((id, policy.weight), ()).is_some() {
                        continue;
                    }
                    let font = Font::new(&staged_db, id, policy.weight).ok_or(
                        FontRegistrationError::UnconstructiblePolicyFace {
                            policy_index,
                            id,
                            weight: policy.weight,
                        },
                    )?;
                    instantiated_fonts.push((id, policy.weight, Arc::new(font)));
                }
            }
            staged_policies.push((policy.alias.clone(), resolved));
        }

        // The policy cap bounds the total held policies, not merely this
        // batch. `staged_policies` holds only genuinely new policies (no-ops
        // and identical aliases were skipped), so repeated no-op transactions
        // stay stable and this check happens before any mutation.
        if self.pinned_policies.len() > MAX_PINNED_POLICIES - staged_policies.len() {
            return Err(FontRegistrationError::TooManyPolicies {
                limit: MAX_PINNED_POLICIES,
            });
        }

        // An empty or identical transaction does not mutate or invalidate.
        if added_faces.is_empty() && staged_policies.is_empty() {
            return Ok(FontRegistrationResult {
                added_faces,
                policies_added: 0,
            });
        }

        // Commit: from the first assignment on, every remaining operation is
        // infallible (only plain hash-map inserts and clears).
        let policies_added = staged_policies.len();
        self.db = staged_db;
        for (alias, policy) in staged_policies {
            self.pinned_policies.insert(alias, policy);
        }
        self.monospace_font_ids = monospace_font_ids;
        self.per_script_monospace_font_ids = per_script_monospace_font_ids;
        self.font_matches_cache.clear();
        self.monospace_fallbacks_buffer.clear();
        #[cfg(feature = "shape-run-cache")]
        {
            self.shape_run_cache = crate::ShapeRunCache::default();
        }
        self.font_cache.extend(
            instantiated_fonts
                .into_iter()
                .map(|(id, weight, font)| ((id, weight), Some(font))),
        );

        Ok(FontRegistrationResult {
            added_faces,
            policies_added,
        })
    }

    /// Get the locale.
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// Get the database.
    pub const fn db(&self) -> &fontdb::Database {
        &self.db
    }

    /// Get a mutable reference to the database.
    ///
    /// This clears the font match cache, but it cannot rebuild the derived
    /// indexes after a later mutation. Call [`FontSystem::refresh_database`]
    /// after adding or removing faces.
    pub fn db_mut(&mut self) -> &mut fontdb::Database {
        self.font_matches_cache.clear();
        &mut self.db
    }

    /// Consume this [`FontSystem`] and return the locale and database.
    pub fn into_locale_and_db(self) -> (String, fontdb::Database) {
        (self.locale, self.db)
    }

    /// Get a font by its ID and weight.
    pub fn get_font(&mut self, id: fontdb::ID, weight: fontdb::Weight) -> Option<Arc<Font>> {
        self.font_cache
            .entry((id, weight))
            .or_insert_with(|| {
                #[cfg(feature = "std")]
                unsafe {
                    self.db.make_shared_face_data(id);
                }
                if let Some(font) = Font::new(&self.db, id, weight) {
                    Some(Arc::new(font))
                } else {
                    log::warn!(
                        "failed to load font '{}'",
                        self.db.face(id)?.post_script_name
                    );
                    None
                }
            })
            .clone()
    }

    pub fn is_monospace(&self, id: fontdb::ID) -> bool {
        self.monospace_font_ids.binary_search(&id).is_ok()
    }

    pub fn get_monospace_ids_for_scripts(
        &self,
        scripts: impl Iterator<Item = [u8; 4]>,
    ) -> Vec<fontdb::ID> {
        let mut ret = scripts
            .filter_map(|script| self.per_script_monospace_font_ids.get(&script))
            .flat_map(|ids| ids.iter().copied())
            .collect::<Vec<_>>();
        ret.sort();
        ret.dedup();
        ret
    }

    #[inline(always)]
    pub fn get_font_supported_codepoints_in_word(
        &mut self,
        id: fontdb::ID,
        weight: fontdb::Weight,
        word: &str,
    ) -> Option<usize> {
        self.get_font(id, weight).map(|font| {
            let code_points = font.unicode_codepoints();
            let cache = self
                .font_codepoint_support_info_cache
                .entry(id)
                .or_insert_with(FontCachedCodepointSupportInfo::new);
            word.chars()
                .filter(|ch| cache.has_codepoint(code_points, u32::from(*ch)))
                .count()
        })
    }

    pub fn get_font_matches(&mut self, attrs: &Attrs<'_>) -> Arc<Vec<FontMatchKey>> {
        // Clear the cache first if it reached the size limit
        if self.font_matches_cache.len() >= Self::FONT_MATCHES_CACHE_SIZE_LIMIT {
            log::trace!("clear font mache cache");
            self.font_matches_cache.clear();
        }

        self.font_matches_cache
            //TODO: do not create AttrsOwned unless entry does not already exist
            .entry(attrs.into())
            .or_insert_with(|| {
                // Pinned aliases produce only their policy IDs: group order
                // is preserved, faces within a group are ranked by requested
                // style/stretch with declared order breaking ties, and the
                // global `db.query` promotion never runs for them.
                if let Family::Name(name) = attrs.family {
                    if let Some(policy) = self.pinned_policies.get(name) {
                        let mut font_match_keys = Vec::new();
                        for group in &policy.groups {
                            let mut group_keys = group
                                .iter()
                                .filter_map(|id| {
                                    self.db
                                        .face(*id)
                                        .map(|face| FontMatchKey::new(attrs, face, &self.db))
                                })
                                .collect::<Vec<_>>();
                            group_keys.sort_by_key(|key| {
                                (key.font_style_diff, key.font_stretch_diff)
                            });
                            font_match_keys.extend(group_keys);
                        }
                        return Arc::new(font_match_keys);
                    }
                }

                #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
                let now = std::time::Instant::now();

                let mut font_match_keys = self
                    .db
                    .faces()
                    .map(|face| FontMatchKey::new(attrs, face, &self.db))
                    .collect::<Vec<_>>();

                // Sort so we get the keys with weight_offset=0 first
                font_match_keys.sort();

                // db.query is better than above, but returns just one font
                let query = Query {
                    families: &[attrs.family],
                    weight: attrs.weight,
                    stretch: attrs.stretch,
                    style: attrs.style,
                };

                if let Some(id) = self.db.query(&query) {
                    if let Some(i) = font_match_keys
                        .iter()
                        .enumerate()
                        .find(|(_i, key)| key.id == id)
                        .map(|(i, _)| i)
                    {
                        // if exists move to front
                        let match_key = font_match_keys.remove(i);
                        font_match_keys.insert(0, match_key);
                    } else if let Some(face) = self.db.face(id) {
                        // else insert in front
                        let match_key = FontMatchKey::new(attrs, face, &self.db);
                        font_match_keys.insert(0, match_key);
                    } else {
                        log::error!("Could not get face from db, that should've been there.");
                    }
                }

                #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
                {
                    let elapsed = now.elapsed();
                    log::debug!("font matches for {attrs:?} in {elapsed:?}");
                }

                Arc::new(font_match_keys)
            })
            .clone()
    }

    #[cfg(feature = "std")]
    fn get_locale() -> String {
        sys_locale::get_locale().unwrap_or_else(|| {
            log::warn!("failed to get system locale, falling back to en-US");
            String::from("en-US")
        })
    }

    #[cfg(not(feature = "std"))]
    fn get_locale() -> String {
        String::from("en-US")
    }

    #[cfg(feature = "std")]
    fn load_fonts(db: &mut fontdb::Database, fonts: impl Iterator<Item = fontdb::Source>) {
        #[cfg(not(target_arch = "wasm32"))]
        let now = std::time::Instant::now();

        db.load_system_fonts();

        for source in fonts {
            db.load_font_source(source);
        }

        #[cfg(not(target_arch = "wasm32"))]
        log::debug!(
            "Parsed {} font faces in {}ms.",
            db.len(),
            now.elapsed().as_millis()
        );
    }

    #[cfg(not(feature = "std"))]
    fn load_fonts(db: &mut fontdb::Database, fonts: impl Iterator<Item = fontdb::Source>) {
        for source in fonts {
            db.load_font_source(source);
        }
    }
}

/// Whether raw face data can provide `sealed` weight: the static weight
/// matches exactly, or a `wght` variation axis covers it. No nearest-weight
/// substitution is performed.
fn face_data_supports_weight(
    data: &[u8],
    index: u32,
    static_weight: fontdb::Weight,
    sealed: fontdb::Weight,
) -> bool {
    if static_weight.0 == sealed.0 {
        return true;
    }
    let Ok(font_ref) = skrifa::FontRef::from_index(data, index) else {
        return false;
    };
    let Some(axis) = font_ref
        .axes()
        .get_by_tag(skrifa::Tag::new(b"wght"))
    else {
        return false;
    };
    let weight = sealed.0 as f32;
    weight >= axis.min_value() && weight <= axis.max_value()
}

/// A value borrowed together with an [`FontSystem`]
#[derive(Debug)]
pub struct BorrowedWithFontSystem<'a, T> {
    pub(crate) inner: &'a mut T,
    pub(crate) font_system: &'a mut FontSystem,
}

impl<T> Deref for BorrowedWithFontSystem<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl<T> DerefMut for BorrowedWithFontSystem<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
    }
}
