# Changelog

## Unreleased

- Serialize immutable resource, registry and image evidence from the existing
  receipt. Keep renderer registry accounting independent of serialization and
  expose no source payloads or operator paths.

- `Prepared` exposes `ui_text()` and `small_text()` typed accessors for the
  validated required `ui` and `small` roles, replacing repeated string
  lookups in hosts. Buttons with an authored cell keep using
  `Prepared::button_text(cell, part)`. Additive: no existing accessor or
  prepared record changes.

## 0.1.1

- Add the opt-in `resources` feature: the central verified resource host
  (`appearance::resources`) runs inside the host's one serial preparation job.
  It resolves the authored resource reference or the recorded expected
  binding, reads the set once through the verified reader, extracts bounded
  compact records, decodes required image variants, submits one atomic
  toolkit batch and attaches an immutable receipt (binding, ready icons and
  selection evidence) to `Prepared`. A fresh omission is pinned only after a
  complete successful preparation and never drifts to another current set;
  exact compact records reuse retained selections without re-registration.
  Without a set, preparation is an honest generic rescue with no binding.
- The receipt exposes `PreparedResources::binding`, `PreparedResources::icon`
  `PreparedResources::owned_text` and `PreparedResources::evidence`, and
  `Prepared::resources` carries it. Owned text retains the registry's exact
  source handles and effective weight for non-Iced painters, including compact
  reuse. Generic rescue carries no verified owned selection.
  `Projection::prepare_with_resources` and `Prepared::attach_resources`
  remain crate-private so public callers cannot forge a receipt.
