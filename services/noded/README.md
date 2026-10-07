# noded

The native ABP broker owns service discovery, routing, subscriptions and
authenticated local session grants. Mesh peers use the same frozen ABP wire
and signed-inventory admission contract as existing nodes.

Set `MIXOS` to an isolated runtime root and supply `etc/node.conf.mix` as
strict data. An explicit `MIXOS` or `MIXOS_ETC` prevents host config fallback.
For example:

```text
node: "example"
noded: { port: 4200, unix_socket: "/run/mixos/noded/bus.sock" }
```

Run `noded serve`; `--no-monitor --no-log` disables its optional observation
clients. The Unix broker endpoint verifies peer credentials before accepting
native session proofs. It needs no session D-Bus. The operating system
supervises the process; application requests and node-to-node control use ABP.

Tests cover broker verbs, scope isolation, replay refusal, authority reload,
mesh admission and recovery. Run `cargo test -p noded -p bus -p config -p props
-p mesh -p mesh-trust` on a configured build worker.

This source was transplanted from the noded, mesh and mesh-trust components
of Cosmix at `e0297242305f3a3c3de09f1ca01e8faa771768da`. Cryptographic domain
bytes remain unchanged. Version 0.18.3 adds the structured
registration-rejection body (additive only; see CHANGELOG.md); the Bus
contracts otherwise match 0.18.2.
