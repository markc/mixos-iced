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

Resident GUI hosts can use `SupervisedClient::connect_options(name, url).start()`
inside their existing Tokio runtime to return immediately while the broker is
unavailable. The same supervisor handles initial dial, registration, topic replay
and later reconnects. Until first success the state is `Connecting`, generation
is zero and outbound work fails fast; first complete registration/replay publishes
generation one. The single incoming receiver remains usable across outages.
`connect().await` retains its five-attempt initial budget for callers that need
bounded startup. Neither API queues outbound work.

With `fatal_on_registration_rejection(true)`, `registration_rejection()` retains
the broker's exact return code and diagnostic before publishing `Fatal`, for both
initial registration and reconnect. Sampling it is non-consuming; transport
failure has no refusal diagnostic. Close, shutdown, deregister and drop are safe
before a first socket exists and fence late registration publication. These are
client lifecycle APIs; the frozen ABP wire format is unchanged.

A broker rejection is typed: `RegistrationRejected` keeps the public `rc` and
`message` fields — field reads and the tuple accessors stay source-compatible,
but old two-field struct literals no longer compile (construct with
`RegistrationRejected::new`) — and adds `kind()`, which decodes the structured
rejection body (`{"schema": "noded.registration-rejection.v1", "error_code":
"NAME_TAKEN", "message": …}`, sent alongside the unchanged `rc=10` and `error`
header).
`RegistrationRejectionKind::NameTaken` is produced only from that body;
an admission refusal, a legacy text-only broker, or a missing, malformed,
oversized or unrecognised body is `Unknown` — classification never string-matches
the wording. `ClientError::registration_rejection_typed()` and
`SupervisedError::registration_rejection_typed()` expose the typed value, and the
`(rc, message)` tuple accessors remain for compatibility.

Test: `cargo test -p bus`. The integration test drives a stub broker in
process; no noded is needed.
