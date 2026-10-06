# Edit

Shared headless text buffers used by editd and Ced. This promotion has two
external owners: the service is the authoritative buffer store; the app
uses the same revision, operation, anchor and position contracts for local
echo and rebasing. No Bus connection, clock, renderer or async runtime is
required by this library.

The Microsoft Edit buffer implementation remains in `vendor/msedit`, with
its licence, upstream revision and retained commit-failure, Unicode and
convergence tests. This library supplies the revision CAS, transactions,
single-authority OT, per-origin undo, anchors and search API above it.

Source: frozen cosmix revision `e0297242305f3a3c3de09f1ca01e8faa771768da`.
