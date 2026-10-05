# Native node broker

`noded` is MixOS's ABP broker. Services register with it, discover other
services and exchange commands or topic events. Communication between nodes
uses ABP peer connections between brokers.

Native applications connect to the broker's configured Unix endpoint.
Credential checks and native session proofs identify the caller; a service
name alone does not confer authority. Mesh admission verifies signed
inventory and an unchanged cross-version admission transcript.

Configuration is strict data in `node.conf.mix`, resolved beneath `MIXOS` or
an explicit `MIXOS_ETC`. Isolated roots never fall back to host configuration.
Run `noded --version` to inspect the package version and build provenance.

The broker does not require a D-Bus session. See the component README for
configuration, optional monitor/logger switches and test commands.
