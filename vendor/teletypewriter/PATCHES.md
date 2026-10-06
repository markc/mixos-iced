# teletypewriter patches

Upstream: `raphamorim/rio`, revision
`932c1a7d9e07b4db5924f7a0dd689e823c3a1442`, package 0.5.27, MIT.
Imported from the frozen source revision
`e0297242305f3a3c3de09f1ca01e8faa771768da` with its licence and patch record.

The inherited patch maps a sealed session descriptor only in the child
between fork and exec. The parent keeps CLOEXEC throughout. It also retains
the source's child exit status and login argv handling. This is required by
Term's real PTY session handoff; a separate launcher would bypass that path.

On entry, `src/unix/mod.rs` and `src/windows/mod.rs` move to `src/unix.rs` and
`src/windows.rs` to follow the module naming rule. The Unix source bytes and
recorded SHA-256 remain unchanged; the guard and record reference its new path.
No second PTY implementation is introduced.

`patch_guard.rs` runs in term-core's unit tests and verifies the exact upstream
revision pins, workspace patch, complete Unix source hash and handoff hunks.
It also includes mutations proving those checks reject drift. Real PTY
integration tests verify sealed descriptor delivery and isolation.
