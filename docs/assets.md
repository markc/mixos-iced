# Installing immutable asset packages

`share/assets/install.mix` 0.2.0 publishes pinned font, icon, image and licence
files into an immutable `sets/<set_id>` directory. It changes `current` only
after the complete package passes validation. Reinstalling the same manifest
verifies and reuses its directory; different bytes or metadata under an existing
ID are refused. Publish changed packages under a new ID.

The installer accepts `mixos.static-assets.v1` and `mixos.static-assets.v2` strict
data manifests. V1 retains its five required font roles and derived icons
catalogue. V2 permits a nonempty declared font-role map and adds:

- `icon_default`: family, style and exact integer weight, 1–1000.
- `icon_catalogues`: unique family/style pairs referring to locked font and
  `.codepoints` files, with optional `face_index` defaulting to zero.
- `icon_assets`: optional named SVG/raster files, unique by name/style, with
  optional `symbolic` defaulting to false.

The default must name a declared catalogue. All catalogue files are checked,
including nondefault styles; duplicate names, malformed rows and invalid Unicode
scalars are refused. Metadata does not establish a font's intrinsic family,
supported weight or face index, nor an image's safe decoded dimensions. Native
resource preparation verifies those claims before application activation.

Every locked file carries its exact byte count, SHA-256, BLAKE3, upstream HTTPS
URL, URL pinned to a 40-hex upstream revision, and an `OFL-1.1` or `Apache-2.0`
licence declaration. Include the corresponding locked licence texts in the
package. The installer preserves their bytes; it does not grant a licence or
infer legal provenance from a file extension.

Bounds are 256 KiB of manifest text, 256 files, 32 font roles, 32 icon catalogues,
4096 named icon assets and 512 MiB of total locked payload. V2 permits at most
256 MiB per file; V1 retains the installer limit of 25 MB per file. Catalogue
files are limited to 1 MiB. Family names are bounded to 128 UTF-8 bytes and
catalogue face indexes to 65535. Unknown fields, explicit null typed metadata,
unlocked references, traversal paths, symlinks and unlocked package components
are refused. V2 relative paths have at most 256 ASCII bytes, with components of
at most 96 bytes; empty, `.` and `..` components are forbidden. V1 retains its
existing flat category layout. The installer never executes manifest contents.

Install the default downloaded package, inspect its lock, or verify the complete
installed set without downloading:

```text
mix share/assets/install.mix --root /opt/mixos/share/assets
mix share/assets/install.mix --list
mix share/assets/install.mix --root /opt/mixos/share/assets --verify
```

An offline package directory must already contain exactly its locked files,
`manifest.conf.mix` and the matching `fonts.css`. Use the same reviewed lock as
the installation input:

```text
mix share/assets/install.mix --manifest /tmp/package/manifest.conf.mix \
  --source /tmp/package --root /tmp/installed-assets
```

`--source` verifies the whole source package before copying its files into the
installer's private stage. Copied bytes are verified again before publication.
It performs no network requests and does not relax hashes, provenance or resource
limits. `--source` is an installation option, separate from `--list`/`--verify`.
`--user` chooses the user's isolated XDG data root; system installation uses
`MIXOS_SHARE/assets`, defaulting to `/opt/mixos/share/assets`. Neither mode adds a
daemon, runtime downloader or alternative settings authority.

Run the real Mix packaging acceptance gate on a worker:

```text
mix tests/assets/install_test.mix /absolute/repository /absolute/candidate/mix
```

The gate uses cleared bytes already in the repository. It checks fresh v1/v2
publication, idempotent immutable reuse, exact payload/licence hashes, all-style
catalogue validation, limits, malformed metadata, tampering, symlinks and retained
activation after refusal. These packaging tests do not certify untouched-core
mono 300 rendering or any application's presented frames; those require their
separate native resource and GUI gates.
