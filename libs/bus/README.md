# bus

The Bus wire format and the broker client.

- `bus::wire` (always built): `BusMessage`, `parse`, `parse_strict`,
  `parse_lenient` and the size caps. A message is `---`-fenced `key: value`
  headers (sorted) plus an optional body. The format is a frozen contract
  with every running peer.
- `bus::client` (the default `client` feature): `Connection`, one registered
  WebSocket to the broker (noded); `SupervisedClient`, the same under a
  reconnect supervisor whose incoming stream survives broker restarts and
  which replays every recorded topic subscription before reporting
  `Connected`; `IncomingCommand`; and `noded_url()`, which finds the broker
  (`MIXOS_NODED_URL`, then `node.conf.mix` via `MIXOS_NODE_CONFIG`,
  `$MIXOS_ETC`, `~/.config/mixos` or `/etc/mixos`, then
  `ws://127.0.0.1:4200/ws`).

A wire-only consumer depends on `bus` with `default-features = false` and
pulls in nothing but `serde_json`.

Test: `cargo test -p bus`. The integration test drives a stub broker in
process; no noded is needed.
