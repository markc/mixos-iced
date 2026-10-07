# Changelog

## Unreleased

- `Prepared` exposes `ui_text()` and `small_text()` typed accessors for the
  validated required `ui` and `small` roles, replacing repeated string
  lookups in hosts. Buttons with an authored cell keep using
  `Prepared::button_text(cell, part)`. Additive: no existing accessor or
  prepared record changes.
