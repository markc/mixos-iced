---
title: Mix application macros
description: Reusable ordinary Mix scripts launched from applications, with local and global homes.
---

# Mix application macros

Mix macros make application operations reusable. A macro can work on the current
selection, ask another application to process something, or coordinate several
services over the ABP Bus. It is an ordinary executable `.mix` script, using the
same language as a script launched directly. Calling it a macro describes its
use by an application, not another language or file format.

Every native GUI app is now required to expose the shared macro facility. The
[decision](decisions/2026-10-08-application-macros.md) is accepted; implementation
across the desktop is pending. **Ced already has its own macro runner.** Other
apps must adopt the shared implementation; this guide does not claim they can
run the new contract today.

## Where scripts belong

| Use | Home |
| --- | --- |
| Macros for one app | `<AppDirs component>/config/macros/` |
| Global reusable scripts/macros | `/opt/mixos/mix` |

The local folder belongs to the resolved app data root, not the directory of the
document being edited. The app will show its resolved path. Existing profile and
image-root configuration can change that root. The global home is the initial
standard until use demonstrates a better location and a migration is agreed.

Only explicitly labelled eligible scripts appear in menus. Generic scripts and
helper libraries can remain available without filling every app's menu. Existing
service, build and private control scripts retain their owning directories.

## Ced today

Ced discovers local `.mix` files with a leading `ced-macro` label and an optional
`ced-key` shortcut:

```mix
-- ced-macro: Report current file
print(env("CED_PATH", ""))
```

Selecting that entry invokes `/opt/mixos/bin/mix`. Ced supplies buffer, epoch,
revision, path, language, selection offsets and edit origin through `CED_*`
variables. Output and failures appear in its Output panel. The script can invoke
other native services as well as the edit service. Editor mutations must respect
the edit service's target/revision and undo contract.

## Shared target contract

The rollout adds common `macro`/`macro-key`/`macro-apps` headers, a versioned
`MIX_MACRO_CONTEXT`, local and global menu groups, creation/folder/reload actions,
and common output and stop handling. Ced's old headers and context remain
supported. These additions are specified, **not yet generally available**.

Apps expose their domain operations on the native Bus. A menu-launched macro
and an agent-launched macro use the same execution path. A sequence should check
each reply before depending on its result. Cancelling the script does not undo
an operation already accepted by another service.

See the [application macro contract](spec/macros/2026-10-08-application-macros.md)
for the exact target rules and acceptance criteria.
