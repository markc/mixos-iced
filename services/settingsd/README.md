# settingsd

Headless native ABP desktop settings authority, contract 0.1.0. An established
profile has one writer and durable whole-document settings/operation receipts.
Native publication is retained and owner-stamped by noded. GUI integration is
not yet implemented.

```text
settingsd init --instance example
settingsd serve --instance example
```

The first command explicitly initialises new state. Serve fails for a missing or
unsupported established store. Both accept `--profile` and `--root` for an
explicit binding/test directory. Use an isolated broker when testing.

See [the settings contract](../../docs/spec/settings/README.md). Worker tests:
`cargo test -p settingsd`; real native gate:
`mix tests/settings/authority_test.mix` with an exact-revision noded binary.
