// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::store::{Accepted, Store, valid_operation_id};
use serde_json::{Value, json};
use settings::*;

pub struct Authority {
    pub store: Store,
    pub accepted: Accepted,
    pub snapshot: Snapshot,
    pub published: Option<Revision>,
}
impl Authority {
    pub fn new(store: Store, accepted: Accepted) -> anyhow::Result<Self> {
        accepted.check(&accepted.binding)?;
        let effective = accepted.effective()?;
        let snapshot = snapshot(&accepted, effective)?;
        Ok(Self {
            store,
            accepted,
            snapshot,
            published: None,
        })
    }
    fn target(&self, binding: &Binding) -> Result<(), Value> {
        if binding != &self.accepted.binding {
            Err(json!({"status":"wrong_target","binding":self.accepted.binding}))
        } else {
            Ok(())
        }
    }
    pub fn read(&self, request: ReadRequest) -> Result<Value, Value> {
        self.target(&request.binding)?;
        Ok(
            json!({"status":"current","snapshot":self.snapshot,"publication_pending":self.published != Some(self.accepted.revision),"recovering":self.store.recovering,"restored_from_backup":self.store.restored}),
        )
    }
    pub fn status(&self, binding: &Binding, operation: Option<&str>) -> Result<Value, Value> {
        self.target(binding)?;
        let receipt =
            operation.and_then(|id| self.accepted.receipts.iter().find(|r| r.operation_id == id));
        Ok(
            json!({"status": if operation.is_some() && receipt.is_none() {"unknown_operation"} else {"current"},
            "receipt":receipt,"binding":self.accepted.binding,"incarnation":self.accepted.incarnation,
            "revision":self.accepted.revision,"published_revision":self.published,
            "publication_pending":self.published != Some(self.accepted.revision),"recovering":self.store.recovering,
            "restored_from_backup":self.store.restored,"consumers":[],"renderer_integration":"pending"}),
        )
    }
    pub fn validate(&self, request: &ApplyRequest) -> Result<Value, Value> {
        self.target(&request.binding)?;
        self.fence(request)?;
        let next =
            settings::resolve::patch(&self.accepted.desktop, &request.changes, &request.reset)
                .map_err(diagnostic)?;
        let effective = settings::resolve_with_embedded(&next, &self.accepted.embedded_source)
            .map_err(diagnostics)?;
        let mut candidate = self.accepted.clone();
        candidate.desktop = next;
        let candidate_snapshot = snapshot(&candidate, effective).map_err(snapshot_error)?;
        Ok(
            json!({"status":"valid","incarnation":self.accepted.incarnation,"revision":self.accepted.revision,"source_digest":candidate_snapshot.source_digest,"effective":candidate_snapshot.effective}),
        )
    }
    fn fence(&self, request: &ApplyRequest) -> Result<(), Value> {
        if request.expected_incarnation != self.accepted.incarnation
            || request.expected_revision != self.accepted.revision
        {
            Err(
                json!({"status":"conflict","incarnation":self.accepted.incarnation,"revision":self.accepted.revision}),
            )
        } else {
            Ok(())
        }
    }
    /// Serialized by the owning worker. Deduplication before revision fencing
    /// makes a lost reply retry retrieve its original receipt after later edits.
    pub fn apply(&mut self, request: ApplyRequest) -> Result<Value, Value> {
        self.target(&request.binding)?;
        if self.store.recovering {
            return Err(
                json!({"status":"outcome_unknown","message":"Recovery required before another mutation"}),
            );
        }
        if request.expected_incarnation != self.accepted.incarnation {
            return Err(
                json!({"status":"conflict","incarnation":self.accepted.incarnation,"revision":self.accepted.revision}),
            );
        }
        if !valid_operation_id(&request.operation_id) {
            return Err(json!({"status":"validation_failed","path":"operation_id"}));
        }
        let digest = request
            .digest()
            .map_err(|e| json!({"status":"validation_failed","message":e.to_string()}))?;
        if request
            .request_digest
            .as_ref()
            .is_some_and(|given| given != &digest)
        {
            return Err(
                json!({"status":"validation_failed","path":"request_digest","message":"Digest mismatch"}),
            );
        }
        if let Some(receipt) = self
            .accepted
            .receipts
            .iter()
            .find(|r| r.operation_id == request.operation_id)
        {
            if receipt.request_digest != digest {
                return Err(json!({"status":"operation_id_reused","receipt":receipt}));
            }
            return Ok(
                json!({"status":receipt.outcome,"receipt":receipt,"publication_pending":self.published != Some(self.accepted.revision),"replayed":true}),
            );
        }
        self.fence(&request)?;
        let desktop =
            settings::resolve::patch(&self.accepted.desktop, &request.changes, &request.reset)
                .map_err(diagnostic)?;
        let unchanged = desktop == self.accepted.desktop;
        let mut next = self.accepted.clone();
        if !unchanged {
            next.revision = Revision(
                next.revision
                    .0
                    .checked_add(1)
                    .ok_or_else(|| json!({"status":"revision_exhausted"}))?,
            );
            if desktop.appearance != next.desktop.appearance || desktop.apps != next.desktop.apps {
                next.design_revision = Revision(
                    next.design_revision
                        .0
                        .checked_add(1)
                        .ok_or_else(|| json!({"status":"revision_exhausted"}))?,
                );
            }
        }
        next.desktop = desktop;
        let effective = if unchanged {
            self.snapshot.effective.clone()
        } else {
            settings::resolve_with_embedded(&next.desktop, &next.embedded_source)
                .map_err(diagnostics)?
        };
        let next_snapshot = snapshot(&next, effective).map_err(snapshot_error)?;
        next.effective_digest = settings::digest(&next_snapshot.effective)
            .map_err(|e| json!({"status":"validation_failed","message":e.to_string()}))?;
        let receipt = Receipt {
            operation_id: request.operation_id,
            request_digest: digest,
            incarnation: next.incarnation.clone(),
            revision: next.revision,
            outcome: if unchanged {
                Outcome::Unchanged
            } else {
                Outcome::Changed
            },
        };
        next.receipts.push(receipt.clone());
        if next.receipts.len() > MAX_RECEIPTS {
            next.receipts.remove(0);
        }
        next.seal()
            .map_err(|e| json!({"status":"storage_failed","message":e.to_string()}))?;
        if let Err(error) = self.store.commit(&self.accepted, &next) {
            return Err(
                json!({"status":if self.store.recovering {"outcome_unknown"} else {"storage_failed"},"message":error.to_string()}),
            );
        }
        self.accepted = next;
        self.snapshot = next_snapshot;
        Ok(
            json!({"status":receipt.outcome,"receipt":receipt,"publication_pending":self.published != Some(self.accepted.revision),"replayed":false}),
        )
    }
}
fn diagnostic(error: Diagnostic) -> Value {
    diagnostics(vec![error])
}
fn diagnostics(errors: Vec<Diagnostic>) -> Value {
    json!({"status":"validation_failed","diagnostics":errors})
}
#[derive(Debug)]
struct SnapshotTooLarge {
    bytes: usize,
}
impl std::fmt::Display for SnapshotTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "snapshot {} bytes exceeds inline budget {}; native artifacts required",
            self.bytes, MAX_SNAPSHOT_BYTES
        )
    }
}
impl std::error::Error for SnapshotTooLarge {}
fn snapshot_error(error: anyhow::Error) -> Value {
    if let Some(limit) = error.downcast_ref::<SnapshotTooLarge>() {
        json!({"status":"snapshot_too_large","bytes":limit.bytes,"maximum":MAX_SNAPSHOT_BYTES,"message":error.to_string()})
    } else {
        json!({"status":"validation_failed","message":error.to_string()})
    }
}
pub(crate) fn snapshot(
    accepted: &Accepted,
    effective: std::collections::BTreeMap<String, Effective>,
) -> anyhow::Result<Snapshot> {
    let source = accepted
        .desktop
        .appearance
        .source
        .as_deref()
        .unwrap_or(&accepted.embedded_source);
    let result = Snapshot {
        schema: SCHEMA,
        binding: accepted.binding.clone(),
        incarnation: accepted.incarnation.clone(),
        revision: accepted.revision,
        design_revision: accepted.design_revision,
        source_digest: settings::source_digest(source),
        desktop: accepted.desktop.clone(),
        effective,
    };
    let bytes = result.encoded_len()?;
    if bytes > MAX_SNAPSHOT_BYTES {
        return Err(SnapshotTooLarge { bytes }.into());
    }
    Ok(result)
}
