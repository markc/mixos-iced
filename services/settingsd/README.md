# settingsd

Headless native ABP desktop settings authority, contract 0.1.0. An established
profile has one writer and durable whole-document settings/operation receipts.
Native publication is retained and owner-stamped by noded. GUI integration is
not yet implemented.

```text
settingsd init --instance example
settingsd seed --instance example
settingsd serve --instance example
```

Init explicitly initialises new state. Seed provisions a first-run session
profile or validates an existing one unchanged; a retained writer lock prevents
seed from recreating an established missing primary, including without a backup.
Serve fails for a missing or
unsupported established store. Both accept `--profile` and `--root` for an
explicit binding/test directory. Use an isolated broker when testing.

Profiles pin their initial package design source. Reload checks a content digest;
a binary upgrade cannot change effective settings within the same revision.
An effective interpretation digest fences compiler drift for explicit migration.
Present corrupt primary data can restore a validated backup under a new
incarnation. A missing primary fails visibly, including when a backup exists.
I/O faults and intact unsupported documents fail without automatic rollback.
The packaged unit binds the instance to its machine hostname; its session/VT
number is a supervision attribute and never a second instance identity.
The session target now wants/upholds the authority. The unit seeds before
serving, and application/compositor startup does not wait for settings readiness.
An image must provision its service account's writable `MIXOS_ETC/settings` root.

See [the settings contract](../../docs/spec/settings/README.md). Worker tests:
`cargo test -p settingsd`; real native gate:
`mix tests/settings/authority_test.mix` with an exact-revision noded binary.
