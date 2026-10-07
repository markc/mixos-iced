# Changelog

## 0.1.2

- Opt-in `settings` shares captured worker preparation and synchronous fenced
  whole-presentation activation/acknowledgement, including deliberate app content.
  Superseded work cannot swap; resource failures retain the last presentation.
  The bridge owns no transport, runtime, receiver or timer.

## 0.1.1

- Opt-in `test-support` exposes the pinned UI simulator to app tests through
  the shared host, keeping app manifests free of iced-family dependencies.

## 0.1.0

- Share native bootstrap, take-once state ownership, single task worker, window
  configuration and renderer feature selection across Ced, DOpus and Term.
- Share native CPU grid drawing and persistent GPU textures over caller-owned
  frame sources. Retain sparse damage and explicit frame lifetime identities.
