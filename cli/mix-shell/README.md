# mix-shell

The Mix shell binary, interactive line and editor facilities, strict command
classifier, native-session bootstrap and ABP citizen runtime. All application
calls use noded; the retired per-application socket fast path is omitted.
The client supervisor is the shared implementation in `bus`.

The bootstrap retains the frozen `COSMIX_SESSION_FD` marker, sealed memfd name
and native-session transcripts. Ordinary shells initialise no grant owner.
Bundled manuals are available offline and use the same directory rule as the
session's other installed resources. Explicit remote manual selection remains
available.

Transplanted from `src/crates/cosmix-mix` at cosmix
`e0297242305f3a3c3de09f1ca01e8faa771768da`, with the complete regression corpus.
The native integration fixture compiles noded's production modules and Term's
production sealed-FD implementation, rather than simulating session RPCs.
