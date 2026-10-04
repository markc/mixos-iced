# MixOS

An agent-operable computing system: legible, modifiable, and reconstructible
by design. MixOS is a Linux userland where every app is a service and every
service speaks one message bus, driven by Mix, its shell.

## Try it beside your own desktop, at native speed

**This is how MixOS is meant to be tried, and it shapes the whole design.**

Keep the Linux desktop you already use. MixOS arrives as a small signed image
that runs as a `systemd-nspawn` machine with **its own virtual terminal**. Your
desktop stays on VT1, MixOS takes another VT, and you flip between them with
Ctrl+Alt+F1 and Ctrl+Alt+F5. Both keep running.

MixOS drives the **real GPU, keyboard, mouse and speakers**. There is no
hypervisor, no virtual GPU and no remote-display protocol in the way. That is
the difference from VirtualBox, virt-manager, Incus or Proxmox: they show a
guest desktop in a window through emulated hardware, and you feel it. A VT
gets the full machine.

It works on any mainstream systemd distro (Ubuntu, Fedora, Debian, Arch,
openSUSE, Mint), using parts already in the kernel and systemd: virtual
terminals, DRM master handover, `seatd`, and `systemd-nspawn`. Debian-family
systems need at most one extra package (`systemd-container`).

So every part of MixOS is built as a **self-contained session**. It has its own
seat manager, runtime directory and session bus, and never assumes it owns the
machine. Things to know up front:

- It needs `sudo`.
- It is not a sandbox: the image gets your real input devices and GPU, so
  trust it like any package you install as root.
- Intel and AMD graphics come first. NVIDIA's proprietary driver needs extra
  work.

**Status:** the mechanism runs daily on the developers' machines. The
download and the one-command installer are being built.

## Building MixOS

MixOS is built one component at a time. The layout and naming rules are in
[AGENTS.md](AGENTS.md).

The first component is **compd**, the Wayland compositor
([`services/compd/`](services/compd/README.md)). Its desktop-tier gates live in
`tests/desktop/` and the dependency gates in `tools/`.

```sh
cargo build --profile release-fast -p compd                    # nested backend
cargo build --profile release-fast -p compd --features backend-all   # nested + KMS
cargo test --workspace
```

- Site and manual: <https://mixos.dev> (`docs/`, GitHub Pages)
- For agents: [`/llms.txt`](https://mixos.dev/llms.txt) indexes every page,
  [`/llms-full.txt`](https://mixos.dev/llms-full.txt) is the whole manual, and
  every page `/x/` is also published as Markdown at `/x.md`.
- About: <https://mixos.au> · <https://mixos.nexus>

## The site

mixos.dev is a prerendered static site. Every `docs/**/*.md` becomes a complete
HTML page at a directory-style URL (`docs/try.md` → `/try/`), built locally by
Mix and committed; GitHub Pages only serves the result. Nothing renders in the
browser and there is no hash routing. `docs/web/nav.js` only swaps the content
area on same-site clicks, and every link works without it.

```sh
mix docs/build/gen-site.mix                 # rebuild whatever changed
mix docs/build/gen-site.mix --check         # verify: stale output or broken links -> exit 1
mix docs/build/gen-site.mix --install-hook  # run --check before every commit
mix -c 'http_serve("docs", {clean_urls: true})'   # preview locally
```

Sources live in `docs/build/`: `shell.html` (page chrome), `home.html` (landing
page) and `site.conf.mix` (sidebar, icons, redirects). The build needs
`hb-subset` (harfbuzz) and `woff2_compress` (woff2) to cut the icon font.

## Licence

MixOS is dual-licensed under the **MIT licence** or the **Apache License,
Version 2.0**, at your option (`MIT OR Apache-2.0`):

- [LICENSE-MIT](LICENSE-MIT)
- [LICENSE-APACHE](LICENSE-APACHE)

Unless you state otherwise, any contribution you submit for inclusion is
dual-licensed the same way, without additional terms. [NOTICE](NOTICE) lists
incorporated and third-party code, and per-file licensing is recorded in
[REUSE.toml](REUSE.toml).
