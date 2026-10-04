---
title: Try it beside your desktop
description: How MixOS runs as a small signed systemd-nspawn image on its own virtual terminal, next to the Linux desktop you already use, with native GPU, input and audio.
---

# Try it beside your desktop

Keep the Linux desktop you already use. MixOS runs next to it on another
virtual terminal (VT): your desktop stays on VT1, MixOS takes another VT, and
you flip between them with **Ctrl+Alt+F1** and **Ctrl+Alt+F5**. Both keep
running.

This is how MixOS is meant to be tried, and it shapes the whole design.

> **Status:** the mechanism runs daily on the developers' machines. The
> download and the one-command installer are being built. This page
> describes what they will do.

## Why not a virtual machine?

VirtualBox, virt-manager, Incus and Proxmox show a guest desktop in a window,
through a virtual GPU and emulated or remote-display input. You can feel that,
and a desktop is exactly the kind of software where you notice. Getting native
graphics out of a VM means passing through a second GPU, which most people
can't do.

On its own VT, MixOS drives the **real GPU, keyboard, mouse and speakers**.
There is no hypervisor, virtual GPU, remote-display protocol or second kernel
in the way. What you try is what you would get.

## How it works

Three things already in the Linux kernel and systemd do the work:

1. **Virtual terminals.** Each VT is a separate console. When you switch, the
   kernel asks the current owner to let go before handing over.
2. **GPU and input handover.** Only one program can drive the display at a
   time. Your desktop's compositor is managed by `systemd-logind` on VT1;
   MixOS's compositor is managed by `seatd` on its own VT. Each lets go of the
   display and input devices when its VT goes to the background, and takes
   them back when it returns.
3. **Separate sessions.** MixOS keeps its own runtime directory and its own
   session bus, so nothing it runs collides with your desktop's.

The container is what makes it a download: `systemd-nspawn` runs MixOS as a
self-contained machine, `importctl` fetches and verifies the signed image, and
`machinectl` starts, stops and removes it.

## What you need

- A mainstream Linux distribution running systemd: Ubuntu, Fedora, Debian,
  Arch, openSUSE or Mint, among others.
- `systemd-nspawn`. Debian-family systems need the `systemd-container`
  package; most others already have it.
- Intel or AMD graphics. NVIDIA's proprietary driver needs extra work and
  comes later.
- `sudo`, for the one install command.

## What the installer will do

1. Check your system: systemd and kernel versions, GPU vendor, and a free VT.
2. Fetch the signed MixOS image and verify its signature.
3. Describe the machine in one small configuration file: which GPU, which VT,
   your audio, and a separate home directory with chosen folders shared.
4. Reserve the VT so no login prompt starts there.
5. Start MixOS and switch to its VT. If MixOS doesn't come up in time, it
   switches you back to VT1 automatically.

Removal undoes every step and leaves your system exactly as it was.

## Things to know first

- **It is not a sandbox.** MixOS gets your real input devices and GPU, so
  trust it as you would any package you install as root.
- **Your desktop keeps running** in the background, and so does everything in
  it. While MixOS is in front, the installer stops your desktop from deciding
  you have gone idle and suspending the machine.
- **Apps can't be in both places at once.** Firefox and Thunderbird already
  open on one desktop refuse to open the same profile on the other. That is
  their profile lock protecting your data, not a fault.
- **If a screen ever goes black,** SSH in from another machine, or use
  Alt+SysRq+R followed by Ctrl+Alt+F1 where your kernel allows it.

## For agents

Every page on this site is static HTML, and also published as Markdown: this
page is [`/try.md`](/try.md). [`/llms.txt`](/llms.txt) indexes the whole
manual and [`/llms-full.txt`](/llms-full.txt) is all of it in one file.
