# config

Where MixOS keeps its files, and how a `*.conf.mix` file is read.

- `config::path(Dir::Etc | Var | Run | Share)`: one rule for every
  directory. A `MIXOS_ETC` / `MIXOS_VAR` / `MIXOS_RUN` / `MIXOS_SHARE`
  override wins; else `$MIXOS/etc`, `/var`, `/run` when a root is set; else
  the XDG directory for a user (`~/.config/mixos`, `~/.local/share/mixos`,
  `$XDG_RUNTIME_DIR/mixos`) or the system directory for root (`/etc/mixos`,
  `/var/lib/mixos`, `/run/mixos`). `Share` is installed read-only data at
  `/opt/mixos/share` whatever the root or user. Resolved once per process
  and cached; `Dirs::resolve(&Environment)` applies the rule to explicit
  inputs for tests.
- `config::parse` / `config::parse_file` and `Value`, `Map`, `Error`,
  `ErrorKind`: the `strict` crate's strict-data loader, re-exported so a
  config reader needs one dependency. Typed loading is `strict::from_file`.

No dependency on the Mix language. Test with `cargo test -p config`.

`atomic::open_directory` refuses symlinks in every component.
`atomic::create_directory` uses the same held-descriptor walk and provisions
missing components with mode 0700. It validates the entire absolute path before
creation, refuses parent traversal and leaves existing permissions unchanged.
