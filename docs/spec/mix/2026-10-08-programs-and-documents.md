---
title: Mix program and document file-family contract
description: Execution refusal, strict document parsing, schema bindings and migration requirements for .mix and .mx files.
---

# Mix program and document file-family contract

Status: accepted target contract, version 1.0.0, 2026-10-08. Planned for the next
major Mix refactor/deployment; no current-binary conformance is claimed. Authority:
[programs and documents decision](../../decisions/2026-10-08-mix-programs-and-documents.md).

## 1. Families, not dialects

`.mix` is executable Mix. `.mx` is a declarative Mix document. Macro, behaviour,
module, task, installer and resident citizen are executable roles; configuration,
theme, spec, catalogue, manifest, policy and scene are document schemas/purposes.
The public API vocabulary is program/document, not safe/unsafe script flavours.

The same interpreter and program grammar serve every executable role. Full
language access remains subject to caller OS permissions and existing host
capability policy; it does not imply privileged execution. Schema binding
evaluation uses the existing pure expression implementation, never a second
general interpreter or a quietly expanded builtin allowlist.

Canonical new data filenames end in `.mx`, for example `node.mx`, `settings.mx`,
`theme.mx`, `scene.mx`, `template.mx` and `authority.mx`. Stems follow existing
repository naming rules and describe purpose where useful; no new compound
suffix is required to select a trust class. Executable test gates remain `.mix`;
their declarative fixtures become `.mx`.

## 2. Program/file admission

All file-based program entry points refuse `.mx` before evaluating any content:

- Direct CLI program execution, `--serve`, interactive file execution and every
  serve initial load, reload candidate and committed generation.
- `source`, `include`, `require`, prelude/startup file loading and any equivalent
  executable file reader in the library or an embedder.
- App macro runners, scene behaviour launchers and automation dispatchers.

Provide a stable structured refusal code and diagnostic saying that the file is
a document and requires a document reader. Exact code registration occurs with
the implementation. `--no-lint` and normal script flags do not bypass admission;
there is no general execute-document override.

Check both the requested path and resolved file target, including symlinks, with
one shared policy used across callers. `.MX` is also refused on executable paths;
writers emit lowercase `.mx`. Filename admission is not a comprehensive security
boundary: renaming, explicit source-string evaluation and intentionally running
a program remain under the caller's authority. Strict parsing is what keeps
document consumption non-executing.

Anonymous program source (`-c`, stdin, library source-string APIs) remains
explicitly executable input, with existing capability policy. Document readers
must never pass document bytes to those APIs or reclassify a failed parse as
program source. Do not disable ordinary stdin/REPL workflows to simulate content
provenance that the runtime cannot establish.

Document checking/linting is non-executing. CLI `--check`/check/lint handling of
`.mx` must validate as a document, or return a clear unsupported-schema/kind
diagnostic. It must never report executable syntax checks as document validation.
The implementation must define this UX and its machine-readable output, without
silently guessing an executable fallback.

## 3. Document readers and schemas

Generic `.mx` documents use the existing strict parser: literals, lists, maps,
comments and the existing round-trip encoding. It refuses variables, calls,
command substitutions, interpolation, statements and executable separators.
`load_data` remains a data-only API and is documented for `.mx`; parsing failure
is terminal for that load. It does not invent filenames or dispatch script code.

Application/service-owned documents additionally validate their owning schema.
The caller may supply schema identity and version, or the document may declare
them; if both do, disagreement is refused. Generic literal data need not carry
an app schema. Schema identity/version are semantic, not another filename family.
Existing numeric `schema` fields do not have to be converted to strings merely
to rename files. Migration must preserve each owning schema's version semantics.

Scene `.mx` documents retain their current `---` header envelope and fenced
strict-data widget map in this first refactor. The existing scene reader handles
that structure; the generic strict reader does not become a scene interpreter.
A consumer explicitly selects its document reader/kind. An unknown kind/schema,
invalid envelope, malformed widget map or invalid binding produces a document
error. No candidate reader is ever the executable parser.

Do not couple extension migration to a rewrite of scene metadata or layouts.
If a common strict-data scene envelope is useful later, version it and provide
a migration independently. Existing layout, model, action-name, diff and source
round-trip contracts must remain intact in this release.

## 4. Restricted bindings and action authority

Only schema-declared fields can interpret restricted expression strings. In all
other fields the identical string is literal data. The scene binding compiler
retains its model/item roots, allowed operations, dependency tracking and size,
time and aggregate list-row limits. Calls, assignments, shell forms, process
launches, file writes and Bus requests cannot be smuggled in through a binding.

Data may name an action implemented by a trusted application/program, but cannot
supply a general executable action body. A workflow document or rule catalogue
is not a bypass for the normal activation/authority of the program consuming it.
Do not introduce automatic command-string evaluation, executable includes or
download-and-run behaviour into document readers.

Non-executing is not synonymous with consequence-free: schemas must still bound
resource use and validate referenced paths/assets/settings according to their
own contracts. The file-family change does not promise that every configuration
value or action reference is appropriate for every consumer.

## 5. Legacy compatibility and migration

Inventory every data consumer and producer, not just files with recognised
suffixes. Include generated runtime state, embedded include_str fixtures, startup
configuration, tests, asset manifests, scenes, watched paths, install collectors,
public guides and private hub automation. Distinguish code/data from its actual
reader; names such as `data.mix` can be executable helper modules and must not be
renamed blindly. No bulk suffix replacement or executable grammar change.

Use explicit rollout phases:

1. Readers gain `.mx` support and legacy data compatibility. Program paths refuse
   `.mx`. Record capability/version evidence on every deployment target.
2. Migrate source fixtures and literals, install collectors and app/service path
   resolvers. Deploy coupled binary/script bundles while retaining legacy files
   needed by unmigrated callers.
3. Switch owning writers to canonical `.mx` only after their readers are ready.
   Migrate user files explicitly and retain recoverable originals and a receipt.
4. Remove legacy spellings only after every supported consumer and tested rollback
   is accounted for, through a documented compatibility-removal release.

Generic `load_data(path)` uses the literal supplied path; it must not silently
rewrite arbitrary `.mix` filenames. Owning configuration/scene resolvers may
support old/new alternatives explicitly. If both exist, compare typed semantic
values under the owning schema: equivalent copies get a visible migration
diagnostic; differing copies are a conflict requiring explicit resolution. A
malformed canonical file does not cause a quiet fallback to an old file.
Declared precedence or explicit operator selection must be observable.

Migration tools run in Mix through native facilities, with a preview/report,
idempotent apply, ownership checks, originals and bounded failure recovery. Do
not rewrite schema versions, erase comments, overwrite user edits or convert
operational identities without a specific migration requirement. Preserve state
and registry format identities, hashes/signature domains and frozen ABP bytes.
Any affected signed artefact is reissued through its existing native authority;
renaming a signed path is not permission to edit signed content in place.

## 6. Component integration and deployment

`libs/strict`, `libs/expr` and `libs/scene` keep their headless separation from
the full Mix interpreter. `libs/mix` and `cli/mix-shell` share program admission;
`libs/config` and owning services/apps handle document paths/schema validation.
Use existing install collectors and native deployment/lifecycle facilities. No
new broker, alternate node-control transport or duplicate parser is introduced.

This is a coordinated binary/library/consumer refactor, not a stand-alone CLI
rename. Build provenance must identify the exact revision, language/library and
binary versions, enabled features, test receipts and installed document formats.
Select the actual release number and compatibility window in its release record.
The current source baseline is language 0.109.1 / binary 0.109.4; that is not the
new release or evidence about every installed target.

Qualify an isolated native target before any working desktop, then roll through
explicitly selected native targets. Stage readers before documents and writers;
verify the real ABP path, loaded scenes/configuration and executable macros. The
self-contained VT-native image carries all readers, interpreter and documents.
It must not depend on host packages or host desktop/session resources.

Rollback includes compatible binary, consumer scripts and document/state copies.
Restoring only the previous binary after a filename migration is insufficient.
Candidate reload failure must preserve the accepted evaluator and managed children.
Frozen Cosmix nodes keep their old spellings until separately migrated; no broad
fleet deploy or production restart follows merely from accepting this spec.

## 7. Required acceptance matrix

| Boundary | Required evidence |
| --- | --- |
| Program admission | Valid and malicious `.mx` refused by direct CLI, serve/reload, REPL file launch, source/include/require, macro and library file APIs before a marker effect occurs; cover symlinks, uppercase and `--no-lint` |
| Strict data | Valid `.mx` round-trips; variables, calls, interpolation, substitutions and statements refuse; errors cannot enter executable fallback |
| Scenes | Existing fixtures renamed without semantic change; pure bindings resolve, effectful bindings refuse, resource bounds and layout/model diffs still pass |
| Schema selection | Known versions work; unknown/mismatched schema/kind, invalid fields and malformed canonical files refuse visibly |
| Legacy coexistence | Old/new readers, generated writers, equal/conflicting duplicate files, missing paths, idempotent migration and interruption recovery behave as specified |
| Native applications | Ced macro/undo remains executable `.mix`; all six app consumers, Quoin scenes and relevant service configuration load through their real native path |
| Packaging/identity | New image has required documents/readers; signed path changes use the native authority; freeze checks confirm no ABP wire/domain-byte changes |
| Deployment/recovery | Exact candidate build runs on isolated target; selected-target rollout reports loaded formats and final state; old binary-plus-data rollback actually restores service/scene/macro behaviour |

Implement declarative fixtures as `.mx` and executable integration gates as `.mix`.
Use negative tests with observable marker effects rather than tests that merely
check an extension helper. Successful docs builds, renamed files or mock readers
do not establish runtime conformance. No extra script families may be introduced
as a shortcut around these boundaries.
