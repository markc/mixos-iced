# vendor/input: local patches

input-rs (`Smithay/input.rs`) 0.10.0 from crates.io, plus one local fix: the
tablet-pad DIAL event is routed and parsed. Wired through
`[patch.crates-io] input = { path = "vendor/input" }`.

## Upstream base: crates.io `input` 0.10.0

- Archive: `https://static.crates.io/crates/input/input-0.10.0.crate`
- SHA-256: `f9793345a65d71317763a33066b5d8351f8760dde8d4930fe9e39b5f14a7959d`
- Upstream commit (`.cargo_vcs_info.json`): `691c8502d4ed7e6de89338d3f6a4cf6dec36483c`,
  2026-04-05, "Merge pull request #96 from Smithay/fixup-the-libinput-version".
  On 2026-10-03 this was still `HEAD` of `Smithay/input.rs` master, and
  `git log -S LIBINPUT_EVENT_TABLET_PAD_DIAL` over its `src` finds nothing:
  upstream has never routed the DIAL event type.

Method: download the archive, `sha256sum` it, untar, `diff -r` against this
directory. The only differences are `.cargo-ok` (cargo's unpack marker, not
source) and the two hunks below. `Cargo.toml`, `Cargo.toml.orig`,
`Cargo.lock`, `CHANGELOG.md` and every other source file are byte-identical.

## Local edits

| file | +/− | marked | what |
|---|---|---|---|
| `src/event.rs` | +10/−0 | `local patch` | `Event::try_from_raw`: under `libinput_1_26`, `LIBINPUT_EVENT_TABLET_PAD_DIAL` is routed to `Event::TabletPad(TabletPadEvent::try_from_raw(libinput_event_get_tablet_pad_event(event), …))`. Upstream falls through to the catch-all and drops the event |
| `src/event/tablet_pad.rs` | +5/−0 | `local patch` | `TabletPadEvent::try_from_raw`: under `libinput_1_26`, `LIBINPUT_EVENT_TABLET_PAD_DIAL` becomes `TabletPadEvent::Dial(TabletPadDialEvent::try_from_raw(…))`. Upstream already has the `Dial` variant and its accessors but never constructs it |

Both arms are gated on `libinput_1_26`, which `vendor/smithay/Cargo.toml`
turns on (`features = ["libinput_1_19", "libinput_1_26"]`). Without that
feature the patch compiles away and dial events are dropped again.

compd's own addition: the `compd_dial_patch_guard` test module at the end
of `src/event/tablet_pad.rs` (+45 lines, marked `compd`).

## Guards

A dial event cannot be built without a libinput device, so the guard is a
source check. Two unit tests in `src/event/tablet_pad.rs`
(`mod compd_dial_patch_guard`) `include_str!` both files, strip whitespace and
assert each DIAL match arm is present, with its `libinput_1_26` gate:

- `event::tablet_pad::compd_dial_patch_guard::event_rs_routes_pad_dial_into_tablet_pad_event`
- `event::tablet_pad::compd_dial_patch_guard::tablet_pad_rs_parses_pad_dial_into_dial_variant`

```
cargo test --manifest-path vendor/input/Cargo.toml compd_dial_patch_guard
```

The needles are assembled from fragments at run time, so the test's own source
cannot satisfy them. Checked without cargo: the same needles match this
directory and do not match the pristine 0.10.0 archive, nor this
`tablet_pad.rs` with its DIAL arm removed.

The in-crate test goes away if the directory is replaced wholesale on a
re-vendor. So after any re-vendor, also run this check from the repo root
(Mix). It must print `true true`:

```
mix -c 'fn squash($s) = re_replace($s, "\\s+", "")
$g = squash("#[cfg(feature = \"libinput_1_26\")]ffi::libinput_event_type_LIBINPUT_EVENT_TABLET_PAD_")
$a = contains(squash(read_file("vendor/input/src/event.rs")), $g + "DIAL=>{Some(Event::TabletPad(TabletPadEvent::try_from_raw(ffi::libinput_event_get_tablet_pad_event(event),")
$b = contains(squash(read_file("vendor/input/src/event/tablet_pad.rs")), $g + "DIAL=>Some(TabletPadEvent::Dial(TabletPadDialEvent::try_from_raw(event,context)?,")
print("${a} ${b}")'
```

Drop the patch once an upstream release routes `LIBINPUT_EVENT_TABLET_PAD_DIAL`
in both places. Check with `git log -S LIBINPUT_EVENT_TABLET_PAD_DIAL -- src`
on `Smithay/input.rs`.
