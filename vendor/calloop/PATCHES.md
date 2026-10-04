# vendor/calloop: local patches

calloop 0.14.4 from crates.io, plus a composed-channel wakeup fix. Vendored
without its `target/`. Wired through
`[patch.crates-io] calloop = { path = "vendor/calloop" }`.

## Upstream base: crates.io `calloop` 0.14.4

- Archive: `https://static.crates.io/crates/calloop/calloop-0.14.4.crate`
- SHA-256: `4dbf9978365bac10f54d1d4b04f7ce4427e51f71d61f2fe15e3fed5166474df7`
- Upstream commit (`.cargo_vcs_info.json`): `7e4d3ac507b0b49dea2bc3b92d7eb2cc54dcd168`

Method: download the archive, `sha256sum` it, untar, `diff -r` against this
directory (verified 2026-10-03). Exactly two files differ. `Cargo.toml`,
`Cargo.toml.orig` and `Cargo.lock` are byte-identical, so dependency
resolution is the same as stock 0.14.4.

## Local edits

| file | +/− | what |
|---|---|---|
| `src/sources/channel.rs` | +57/−2 | Runtime fix plus its regression test, described below |
| `CHANGELOG.md` | +1/−1 | Trailing space removed on the "Bump `nix` to v0.31" line. Editor noise with no effect |

The fix is in `<Channel<T> as EventSource>::process_events`. It adds a
`matched` flag that is set inside the ping callback. The bounded-batch re-ping
(`self.ping.ping()`) now fires only when `matched && !clear_readiness`, which
means this channel serviced its own token and hit the batch limit.

Stock 0.14.4 also re-pinged when the readiness token was not its own. A
composite source forwards every token to every child, so two child channels
pinged each other forever. That is a busy loop in an event-driven compositor.
The rest of the hunk (+51) is the regression test.

## Guard

The regression test ships inside the patched file:
`sources::channel::tests::composed_channels_do_not_ping_each_other_for_unrelated_tokens`.
It wraps two channels in one composite source, sends one message and runs
eight non-blocking dispatches. It asserts one message and exactly one
composite dispatch. Stock 0.14.4 produces eight dispatches.

```
cargo test --manifest-path vendor/calloop/Cargo.toml composed_channels
```

A wholesale re-vendor removes the test along with the fix. After any
re-vendor, check from the repo root that the fix is still there. It must
print `true`:

```
mix -c 'print(contains(read_file("vendor/calloop/src/sources/channel.rs"), "} else if !matched || clear_readiness {"))'
```

Also check that `cargo tree -i calloop@0.14.4` resolves to `vendor/calloop`.
Drop the patch once upstream's channel stops re-pinging on a token it does not
own.
