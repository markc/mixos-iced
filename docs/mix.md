# Mix

Mix is the MixOS shell and scripting language. The interpreter lives in the
`mix` library; `cli/mix-shell` owns the binary, interactive shell and native ABP
citizen runtime. The current language version is 0.109.1 and the shell source
version is 0.109.4.

Use `mix builtins --names` to discover valid builtin names, and `mix builtins
<name>` for their contracts. `mix man overview` and `mix man syntax` explain the
language and command classifier. Discovery should inspect process status rather
than stop a sequence when a lookup is invalid.

Mix scripts use `run_argv` for local processes and native Bus verbs for application
control. `run_argv_must` is appropriate when a non-zero exit must stop the script.
The public language regression corpus accompanies the interpreter transplant.

## Programs and documents

The accepted target for the next major Mix refactor and deployment is two file
families:

| Extension | Meaning |
| --- | --- |
| `.mix` | Executable Mix program: macro, behaviour, service, module, installer or test |
| `.mx` | Declarative Mix document: configuration, scene, theme, catalogue, manifest or data specification |

All program roles use the same language. Documents use strict parsing and an
owning schema where applicable. Only declared binding fields may evaluate the
existing restricted, side-effect-free expression subset. Data strings otherwise
remain literal. A schema is a contract/validator, not necessarily another file.

This is an **accepted target, not a completed runtime migration**. Current
consumers still use `*.conf.mix`, `*.spec.mix`, `*.data.mix`, `scene.mix` and other
legacy data names. Keep their working paths until upgraded readers are deployed.
Do not rename executable helper modules such as a scene library's `data.mix`.

The refactor must refuse `.mx` through executable file loaders and retain strict
document errors without code/shell fallback. Scenes keep their current envelope
and pure bindings initially. Readers upgrade before files and writers; rollback
includes compatible binaries, scripts and documents.

New purposes use existing programs or versioned document schemas. Another file
family or evaluation mode requires an architectural decision. See the
[decision](decisions/2026-10-08-mix-programs-and-documents.md) and
[file-family contract](spec/mix/2026-10-08-programs-and-documents.md).
