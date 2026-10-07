// SPDX-License-Identifier: MIT OR Apache-2.0
//! Presentation preparation on the host's worker, never authority ordering.
use crate::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationKind {
    Current,
    Retained,
    Cached,
    LastGood,
    Embedded,
}

/// Validated cache data, still requiring the host's resource readiness check.
/// This is not an authority read or a renderer activation token.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub(crate) snapshot: Arc<Snapshot>,
    pub(crate) context: String,
    pub(crate) shell: bool,
    pub(crate) binding: Option<crate::ResourceBinding>,
}
impl Candidate {
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    /// The renderer-neutral resource binding recorded with this cached
    /// activation: an exact candidate identity for the host to reopen and
    /// verify, never authority. Legacy schema-1 cache files carry none.
    pub fn binding(&self) -> Option<&crate::ResourceBinding> {
        self.binding.as_ref()
    }
}

/// Capture on the UI event loop while authority starts, prepare off-thread.
/// The host owns the one preparation job and cancels it on connection/rebind.
#[derive(Clone, Debug)]
pub struct Request {
    pub(crate) owner: u64,
    pub(crate) serial: u64,
    pub(crate) generation: Option<u64>,
    pub(crate) binding: Binding,
    pub(crate) context: String,
    pub(crate) shell: bool,
    pub(crate) retained: Option<Snapshot>,
    /// The binding captured with the retained snapshot: the expected resource
    /// identity the retained presentation was activated against. None means
    /// the retained data has no recorded binding.
    pub(crate) retained_binding: Option<crate::ResourceBinding>,
}

#[derive(Clone, Debug)]
pub struct Prepared {
    pub(crate) request: Request,
    pub(crate) snapshot: Arc<Snapshot>,
    pub(crate) kind: PresentationKind,
    pub(crate) resources: Option<crate::ResourceBinding>,
    diagnostics: Vec<Diagnostic>,
}
impl Prepared {
    pub fn kind(&self) -> PresentationKind {
        self.kind
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    /// The renderer-neutral resource binding this preparation was checked
    /// against. `Consumer::complete_fallback` carries it on the staged
    /// update, so the ordinary `acknowledge` preserves it and the captured
    /// cache save records exactly what was prepared; a host may also pass it
    /// to `Consumer::acknowledge_resources` explicitly. None means no
    /// binding was produced by the resource check.
    pub fn resources(&self) -> Option<&crate::ResourceBinding> {
        self.resources.as_ref()
    }
}
impl Request {
    pub fn same_request(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.serial == other.serial
            && self.generation == other.generation
    }
    /// Resource validation covers every reference used by this context/host.
    /// It may inspect preloaded assets/fonts on the worker; it must only succeed
    /// when the complete generation is usable. No I/O occurs on the UI loop.
    pub fn prepare(
        &self,
        cached: Option<Candidate>,
        resources: impl FnMut(&Snapshot, &str, bool) -> Result<(), Diagnostic>,
    ) -> Result<Prepared, Vec<Diagnostic>> {
        self.prepare_with_cache(|| Ok(cached), resources)
    }

    /// Load persistent data only after retained data fails its resource check.
    /// A load failure is retained as a diagnostic while embedded may succeed.
    pub fn prepare_with_cache(
        &self,
        cache: impl FnOnce() -> Result<Option<Candidate>, Diagnostic>,
        resources: impl FnMut(&Snapshot, &str, bool) -> Result<(), Diagnostic>,
    ) -> Result<Prepared, Vec<Diagnostic>> {
        self.prepare_resources_with_cache(cache, |snapshot, context, shell, _expected| {
            resources(snapshot, context, shell).map(|()| None)
        })
    }

    /// Resource-aware variant of `prepare_with_cache`: the readiness check
    /// receives the expected binding of the candidate being checked (the
    /// retained activation binding, the cached envelope binding, or None for
    /// embedded) and returns the renderer-neutral binding of whatever it
    /// actually verified. When an expected binding is present, a successful
    /// check must return exactly that binding; any disagreement rejects the
    /// candidate and continues the fallback ladder, so a different
    /// current-default resolution is never labelled as the cached candidate.
    pub fn prepare_resources_with_cache(
        &self,
        cache: impl FnOnce() -> Result<Option<Candidate>, Diagnostic>,
        mut resources: impl FnMut(
            &Snapshot,
            &str,
            bool,
            Option<&crate::ResourceBinding>,
        ) -> Result<Option<crate::ResourceBinding>, Diagnostic>,
    ) -> Result<Prepared, Vec<Diagnostic>> {
        let mut diagnostics = Vec::new();
        if let Some(snapshot) = &self.retained {
            match validate_inline(snapshot, &self.binding, &self.context).and_then(|_| {
                resources(
                    snapshot,
                    &self.context,
                    self.shell,
                    self.retained_binding.as_ref(),
                )
            }) {
                Ok(binding) => {
                    if self.retained_binding.is_some()
                        && binding.as_ref() != self.retained_binding.as_ref()
                    {
                        diagnostics.push(Diagnostic::new(
                            "binding_mismatch",
                            "resources",
                            "Prepared binding differs from the retained activation binding",
                        ));
                    } else {
                        return Ok(self.prepared(
                            Arc::new(snapshot.clone()),
                            PresentationKind::Retained,
                            binding,
                            diagnostics,
                        ));
                    }
                }
                Err(error) => diagnostics.push(error),
            }
        }
        let cached = match cache() {
            Ok(candidate) => candidate,
            Err(error) => {
                diagnostics.push(error);
                None
            }
        };
        if let Some(candidate) = cached {
            let check = if candidate.snapshot.binding == self.binding
                && candidate.context == self.context
                && candidate.shell == self.shell
            {
                resources(
                    &candidate.snapshot,
                    &self.context,
                    self.shell,
                    candidate.binding.as_ref(),
                )
            } else {
                Err(Diagnostic::new(
                    "wrong_cache_target",
                    "cache",
                    "Cache belongs to another consumer binding/context/capability",
                ))
            };
            match check {
                Ok(binding) => {
                    if candidate.binding.is_some() && binding.as_ref() != candidate.binding.as_ref()
                    {
                        diagnostics.push(Diagnostic::new(
                            "binding_mismatch",
                            "resources",
                            "Prepared binding differs from the cached activation binding",
                        ));
                    } else {
                        return Ok(self.prepared(
                            candidate.snapshot,
                            PresentationKind::Cached,
                            binding,
                            diagnostics,
                        ));
                    }
                }
                Err(error) => diagnostics.push(error),
            }
        }
        let embedded = (|| {
            let mut desktop = Desktop::default();
            if let Some(app) = self.context.strip_prefix("app:") {
                desktop.apps.insert(app.into(), AppOverride::default());
            }
            let effective =
                resolve(&desktop).map_err(|errors| errors.into_iter().next().unwrap())?;
            let snapshot = Snapshot {
                schema: SCHEMA,
                binding: self.binding.clone(),
                incarnation: "embedded-presentation".into(),
                revision: Revision(0),
                design_revision: Revision(0),
                source_digest: source_digest(EMBEDDED_DEFAULT_SOURCE),
                desktop,
                effective,
            };
            let binding = resources(&snapshot, &self.context, self.shell, None)?;
            Ok::<_, Diagnostic>((Arc::new(snapshot), binding))
        })();
        match embedded {
            Ok((snapshot, binding)) => {
                Ok(self.prepared(snapshot, PresentationKind::Embedded, binding, diagnostics))
            }
            Err(error) => {
                diagnostics.push(error);
                Err(diagnostics)
            }
        }
    }
    fn prepared(
        &self,
        snapshot: Arc<Snapshot>,
        kind: PresentationKind,
        resources: Option<crate::ResourceBinding>,
        diagnostics: Vec<Diagnostic>,
    ) -> Prepared {
        Prepared {
            request: self.clone(),
            snapshot,
            kind,
            resources,
            diagnostics,
        }
    }
}

/// Recompile the whole inline document before trusting persisted/retained data.
/// An old pinned package source absent from this binary is deliberately refused.
pub(crate) fn validate_inline(
    snapshot: &Snapshot,
    binding: &Binding,
    context: &str,
) -> Result<(), Diagnostic> {
    binding.validate()?;
    if snapshot.binding != *binding {
        return Err(Diagnostic::new(
            "wrong_cache_target",
            "binding",
            "Snapshot binding differs",
        ));
    }
    if snapshot.schema != SCHEMA {
        return Err(Diagnostic::new(
            "unsupported_schema",
            "schema",
            "Unsupported cache snapshot schema",
        ));
    }
    if snapshot.incarnation.is_empty()
        || snapshot.incarnation.len() > 128
        || snapshot.revision.0 == 0
        || snapshot.design_revision.0 == 0
    {
        return Err(Diagnostic::new(
            "invalid_cache_snapshot",
            "snapshot",
            "Missing authority identity/revision",
        ));
    }
    if snapshot
        .encoded_len()
        .map_err(|e| Diagnostic::new("invalid_cache_snapshot", "snapshot", e.to_string()))?
        > MAX_SNAPSHOT_BYTES
    {
        return Err(Diagnostic::new(
            "snapshot_too_large",
            "snapshot",
            "Inline byte budget exceeded",
        ));
    }
    let source = snapshot
        .desktop
        .appearance
        .source
        .as_deref()
        .unwrap_or(EMBEDDED_DEFAULT_SOURCE);
    if snapshot.source_digest != source_digest(source) {
        return Err(Diagnostic::new(
            "unsupported_interpretation",
            "source_digest",
            "Pinned source is unavailable or differs",
        ));
    }
    let effective =
        resolve(&snapshot.desktop).map_err(|errors| errors.into_iter().next().unwrap())?;
    if effective != snapshot.effective || !effective.contains_key(context) {
        return Err(Diagnostic::new(
            "unsupported_interpretation",
            "effective",
            "Authored inputs and cached projections disagree",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(set_id: &str) -> crate::ResourceBinding {
        crate::ResourceBinding {
            schema: crate::RESOURCE_SCHEMA,
            set_id: set_id.into(),
            manifest_blake3: "a".repeat(64),
            interpretation: crate::resource_interpretation(),
            icons: None,
        }
    }
    fn fixture() -> Binding {
        Binding {
            instance: "fixture".into(),
            profile: "default".into(),
        }
    }
    fn snapshot() -> Snapshot {
        let desktop = Desktop::default();
        Snapshot {
            schema: SCHEMA,
            binding: fixture(),
            incarnation: "authority".into(),
            revision: Revision(1),
            design_revision: Revision(1),
            source_digest: source_digest(EMBEDDED_DEFAULT_SOURCE),
            effective: resolve(&desktop).unwrap(),
            desktop,
        }
    }
    fn request(
        retained: Option<Snapshot>,
        retained_binding: Option<crate::ResourceBinding>,
    ) -> Request {
        Request {
            owner: 1,
            serial: 2,
            generation: None,
            binding: fixture(),
            context: "app:ced".into(),
            shell: false,
            retained,
            retained_binding,
        }
    }
    fn candidate(binding: Option<crate::ResourceBinding>) -> Candidate {
        Candidate {
            snapshot: Arc::new(snapshot()),
            context: "app:ced".into(),
            shell: false,
            binding,
        }
    }
    #[test]
    fn expected_binding_must_be_returned_exactly_for_retained_candidates() {
        let expected = binding("expected");
        // A different returned binding rejects the retained candidate and the
        // ladder continues; the diagnostic names the disagreement.
        let request = request(Some(snapshot()), Some(expected.clone()));
        let prepared = request
            .prepare_resources_with_cache(
                || Ok(None),
                |_, _, _, expected_binding| match expected_binding {
                    Some(expected_binding) => {
                        assert_eq!(expected_binding, &expected);
                        Ok(Some(binding("different")))
                    }
                    None => Ok(None),
                },
            )
            .unwrap();
        assert_eq!(prepared.kind(), PresentationKind::Embedded);
        assert_eq!(prepared.diagnostics()[0].code, "binding_mismatch");
        assert!(prepared.resources().is_none());
        // Returning the exact expected binding accepts the retained candidate.
        let request = request(Some(snapshot()), Some(expected.clone()));
        let prepared = request
            .prepare_resources_with_cache(
                || panic!("a validated retained candidate must win"),
                |_, _, _, expected_binding| {
                    assert_eq!(expected_binding, Some(&expected));
                    Ok(Some(expected.clone()))
                },
            )
            .unwrap();
        assert_eq!(prepared.kind(), PresentationKind::Retained);
        assert_eq!(prepared.resources(), Some(&expected));
        assert!(prepared.diagnostics().is_empty());
    }
    #[test]
    fn cached_expected_binding_must_match_exactly() {
        let pinned = binding("pinned");
        let request = request(None, None);
        // The exact recorded binding accepts the cached candidate.
        let prepared = request
            .prepare_resources_with_cache(
                || Ok(Some(candidate(Some(pinned.clone())))),
                |_, _, _, expected_binding| {
                    assert_eq!(expected_binding, Some(&pinned));
                    Ok(Some(pinned.clone()))
                },
            )
            .unwrap();
        assert_eq!(prepared.kind(), PresentationKind::Cached);
        assert_eq!(prepared.resources(), Some(&pinned));
        assert!(prepared.diagnostics().is_empty());
        // A default resolved differently now is never labelled as the cached
        // candidate: the candidate is rejected and the ladder continues.
        let prepared = request
            .prepare_resources_with_cache(
                || Ok(Some(candidate(Some(pinned.clone())))),
                |_, _, _, expected_binding| match expected_binding {
                    Some(expected_binding) => {
                        assert_eq!(expected_binding, &pinned);
                        Ok(Some(binding("current-default")))
                    }
                    None => Ok(None),
                },
            )
            .unwrap();
        assert_eq!(prepared.kind(), PresentationKind::Embedded);
        assert_eq!(prepared.diagnostics()[0].code, "binding_mismatch");
    }
    #[test]
    fn legacy_prepare_with_cache_cannot_claim_a_resource_bound_candidate() {
        let request = request(None, None);
        // The legacy wrapper returns no binding, so a schema-2 candidate's
        // expected binding can never be satisfied: it falls through instead of
        // pretending the recorded resources were verified.
        let prepared = request
            .prepare_with_cache(
                || Ok(Some(candidate(Some(binding("pinned"))))),
                |_, _, _| Ok(()),
            )
            .unwrap();
        assert_eq!(prepared.kind(), PresentationKind::Embedded);
        assert_eq!(prepared.diagnostics()[0].code, "binding_mismatch");
        // A legacy envelope without a binding behaves exactly as before.
        let prepared = request
            .prepare_with_cache(|| Ok(Some(candidate(None))), |_, _, _| Ok(()))
            .unwrap();
        assert_eq!(prepared.kind(), PresentationKind::Cached);
        assert!(prepared.diagnostics().is_empty());
    }
}
