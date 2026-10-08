# noded changes

## 0.18.4

Mint registration incarnations from actual sender-channel ownership. Preserve
same-channel refresh and expose atomic services.incarnations changes even when
the registered names are unchanged. Stamp reserved owner observation topics
with the actual registration identity and native Unix principal; strip forged
inner provenance. ABP framing and existing property leaves remain unchanged.

## 0.18.3

Add a structured rejection body to the different-channel `noded.register`
collision refusal: `{"schema": "noded.registration-rejection.v1",
"error_code": "NAME_TAKEN", "message": …}` alongside the unchanged `rc=10`
reply and `error` header. The body is additive only — ABP framing, return
codes, admission refusals and session transcript bytes are untouched — and
lets clients classify a collision without matching diagnostic text. The bus
client decodes it through a bounded typed envelope whose classification is
pinned by noded and bus tests.

## 0.18.2

Transplant the native broker, session authority and mesh routing to the MixOS
workspace. Preserve ABP verbs, admission domains and session transcript bytes.
Use strict MixOS configuration and owned shutdown signalling.
