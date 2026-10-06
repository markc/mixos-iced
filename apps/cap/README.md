# Cap

This keeps the existing capture application's short name during its native
transplant. Debian's Capistrano also installs `/usr/bin/cap`
([package file list](https://packages.debian.org/es/sid/all/capistrano/filelist)).
MixOS's desktop entry invokes `/opt/mixos/bin/cap` explicitly; the installation
does not replace a host distribution's binary.

Native Wayland screenshots and editable annotations. compd owns capture and
interactive region selection; Cap provides an iced window and a Bus service.
There is no D-Bus or portal dependency.

Run `cap`, or `cap IMAGE.png`. A second invocation asks the existing instance
to show itself or open the image. Unsaved annotations are never discarded by
a remote request. `cap --headless` exposes the same document commands without
a window. `--service`, `--comp` and `--noded-url` select native service endpoints.

The [manual](https://github.com/markc/mixos/blob/main/docs/cap.md) describes
the controls, commands and current limits. `cargo test -p cap` tests document
history, geometry, export and capture restoration; use the workspace's
authorised build workers for compilation.
