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
}
impl Candidate {
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
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
}

#[derive(Clone, Debug)]
pub struct Prepared {
    pub(crate) request: Request,
    pub(crate) snapshot: Arc<Snapshot>,
    pub(crate) kind: PresentationKind,
    diagnostics: Vec<Diagnostic>,
}
impl Prepared {
    pub fn kind(&self) -> PresentationKind {
        self.kind
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
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
        mut resources: impl FnMut(&Snapshot, &str, bool) -> Result<(), Diagnostic>,
    ) -> Result<Prepared, Vec<Diagnostic>> {
        let mut diagnostics = Vec::new();
        if let Some(snapshot) = &self.retained {
            match validate_inline(snapshot, &self.binding, &self.context)
                .and_then(|_| resources(snapshot, &self.context, self.shell))
            {
                Ok(()) => {
                    return Ok(self.prepared(
                        Arc::new(snapshot.clone()),
                        PresentationKind::Retained,
                        diagnostics,
                    ));
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
                resources(&candidate.snapshot, &self.context, self.shell)
            } else {
                Err(Diagnostic::new(
                    "wrong_cache_target",
                    "cache",
                    "Cache belongs to another consumer binding/context/capability",
                ))
            };
            match check {
                Ok(()) => {
                    return Ok(self.prepared(
                        candidate.snapshot,
                        PresentationKind::Cached,
                        diagnostics,
                    ));
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
            resources(&snapshot, &self.context, self.shell)?;
            Ok::<_, Diagnostic>(Arc::new(snapshot))
        })();
        match embedded {
            Ok(snapshot) => Ok(self.prepared(snapshot, PresentationKind::Embedded, diagnostics)),
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
        diagnostics: Vec<Diagnostic>,
    ) -> Prepared {
        Prepared {
            request: self.clone(),
            snapshot,
            kind,
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
