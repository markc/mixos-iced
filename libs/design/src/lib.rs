//! The MixOS design-token system: the closed family schemas, the resolved
//! design model, and the compiler that turns a strict-data design source into
//! a resolved design for one scheme, mode and contrast.
//!
//! The crate is headless: it has no renderer dependency, and every rendering
//! adapter lives in its consumer. A consumer compiles a source with
//! [`parse_design_source`] and [`compile_design`], or starts from the embedded
//! default ([`EMBEDDED_DEFAULT_SOURCE`]), then reads colours, metrics,
//! typography and the button table off the resolved design.
//!
//! The context-free mapping compiler is deliberately not a public entry point:
//! ```compile_fail
//! use design::MappingCompileFailure;
//! ```
//! ```compile_fail
//! use design::compile_button_mapping;
//! ```
//! A compiled candidate exposes its artifact through read-only accessors:
//! ```compile_fail
//! fn mutate_candidate(candidate: &mut design::UnstampedResolvedDesign) {
//!     candidate.dictionary.colours.pairs.clear();
//! }
//! ```
//! Compiled pair policies likewise cannot be fabricated by consumers:
//! ```compile_fail
//! let _ = design::PairSubstitutionPolicy {
//!     slot: 0,
//!     decisions: std::collections::BTreeMap::new(),
//! };
//! ```

/// The revision-1 strict-data default design embedded into every build.
pub const EMBEDDED_DEFAULT_SOURCE: &str = include_str!("defaults/revision-1.theme.conf.mix");
pub const EMBEDDED_DEFAULT_REVISION: DesignRevision = DesignRevision::FIRST;

mod axis;
mod colour;
mod colour_model;
mod compiler;
mod context;
mod design_model;
mod diagnostic;
mod equivalence;
pub mod family;
mod mapping;
mod mapping_model;
mod projection;
mod recipe;
mod recipe_compiler;
mod source;
mod state;
#[cfg(test)]
mod trial;
mod typography;

pub use colour::{ColourCompileFailure, compile_colour_tokens};
pub use colour_model::{
    FocusRingProvenance, LinearRgba, NON_TEXT_NAMES, ResolvedColours, ResolvedNonTextColour,
    ResolvedPair, TEXT_PAIR_NAMES, contrast_ratio,
};
pub use compiler::compile_design;
pub use context::{Contrast, DesignContext, Mode, Scheme};
pub use design_model::{
    AuthoredMetric, DesignApplyDecision, DesignApplyTransition, DesignCompileFailure,
    DesignCompileOutcome, DesignCompileResult, DesignCompileStatus, DesignCompileSuccess,
    DesignProvenance, DesignRevision, DesignValueId, PairOverrideDisposition, PairOverrideRoute,
    ResolvedDesign, ResolvedDictionary, ResolvedMetric, ResolvedMetricKind, ResolvedTables,
    ResolvedTypography, ResolvedTypographyRef, SourceIdentity, UnstampedResolvedDesign,
    ValueProvenance, apply_compiled_design,
};
pub use diagnostic::{CompileSuccess, DesignDiagnostic, DiagnosticSeverity};
pub use equivalence::parse_legacy_v0_hex_colour;
pub use family::button::{ButtonPart, ButtonSize, ButtonVariant};
pub use family::{FAMILY_SCHEMAS, FamilyId, FamilyPart, FamilySchema};
pub use mapping_model::{
    BUTTON_CELL_COUNT, BUTTON_TYPOGRAPHY_COUNT, ButtonCellKey, ButtonProperty, ButtonTypographyKey,
    ButtonTypographyTable, ResolvedButtonCell, ResolvedButtonTable, ResolvedTypeRecord,
    ResolvedTypographyAssignment,
};
pub use projection::{DesignReadProjection, ReadButton, ReadMetric, ReadPair, ReadType};
pub use recipe::{
    DerivationRecipe, PairRefDecision, PairRefExclusion, PairSubstitutionPolicy, REGISTRY,
    RecipeBinding, RecipeImplicitBinding, RecipeImplicitInput, RecipeMovement, RecipeOutput,
    RecipePairDomain, RecipeParam, RecipeSignature, RecipeSubstitutionDomainConstraint,
};
pub use source::{
    AuthoredPairSource, ButtonInheritanceSource, ButtonMappingSource, ColourSpace, CoveragePolicy,
    DerivationCallSource, DesignSourceDocument, DesignSourceError, DesignSourceErrorCode,
    DesignV1Source, FamilyMappingsSource, LegacyTypographySource, LegacyV0Source,
    MappingRuleSource, MappingSelectorSource, MappingValueSource, MetricSource, ModifierAxis,
    ModifierBlockSource, NonTextColourSource, OklchSource, PairSource, PrimitiveSource,
    RecipeArgumentSource, SemanticSource, SourceKind, TaggedMetricSource, TypeRecordSource,
    TypographySource, V0CrosswalkExpressionSource, V0MappingProperty, V0PairMember,
    parse_design_source, parse_legacy_v0_source,
};
pub use state::{InteractionState, StyleStateKey};
pub use typography::{
    TypographyGeneric, TypographyRole, active_typography, default_typography, family_font_weight,
};
