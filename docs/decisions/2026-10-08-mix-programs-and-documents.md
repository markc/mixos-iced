---
title: Decision — .mix programs and .mx documents
description: Two Mix file families distinguish executable programs from declarative documents without multiplying script dialects.
---

# Decision — `.mix` programs and `.mx` documents

Status: accepted 2026-10-08. Target of the **next major Mix binary refactor and
deployment**; implementation and filename migration are pending. The release
number is assigned during implementation, not inferred from this decision.

## The decision

| Family | Canonical extension | Meaning |
| --- | --- | --- |
| Mix program | `.mix` | Executable Mix with its ordinary language capabilities and caller authority |
| Mix document | `.mx` | Strict declarative data consumed and validated by a defined reader/schema |

Macros, scene behaviours, services, installers, tests and modules are roles of
ordinary Mix programs. They introduce no separate grammar or extension. Themes,
configuration, specifications, manifests, catalogues and scenes are document
purposes. Different schemas do not make different script languages.

An `.mx` document cannot contain general executable Mix. A schema may allow
the existing restricted, side-effect-free expression subset in precisely defined
fields. Scene bindings remain permitted under their current limits; function
calls, assignment, process/file effects and Bus operations remain refused.

The first migration preserves existing strict-data and scene-envelope syntax.
This decision unifies file families and execution rules, not every document's
grammar. Changing the scene envelope later is a separate schema migration, not
a prerequisite or excuse to add another parser now.

## Enforcement and evolution

1. Ordinary program execution and executable file/module loading refuse `.mx`,
   including serve/reload paths. A malformed document never falls back to the
   full interpreter or shell. Extension checks complement strict parsing; they
   are not a malware detector or sandbox for intentionally run programs.
2. Generic documents use strict parsing. App-owned documents also validate their
   schema, supplied by the owning reader and/or declared in the document. A
   schema can be a Rust type/validator; it need not be another file.
3. Data strings stay literal except at schema-declared restricted binding fields.
   A document describing commands does not gain execution authority by being
   data; running those actions is an executable program responsibility.
4. New purposes use existing Mix programs or versioned document schemas. A new
   family, suffix or evaluation mode requires an accepted decision explaining
   why existing facilities fail, authority/resource limits, negative tests and
   compatibility. There is no macro or workflow dialect to introduce.
5. Legacy `.conf.mix`, `.spec.mix`, `.data.mix`, `scene.mix` and other inventoried
   data names remain readable during a measured migration. They are transitional
   spellings, not additional permanent families. Accepting this decision does
   not make current binaries recognise or enforce the new extension.
6. Install upgraded readers before switching files/writers. Preserve user data,
   expose conflicts between old/new files, and retain a tested binary-plus-data
   rollback. Frozen Cosmix deployments are not silently upgraded or renamed.
   No frozen ABP bytes or cross-node transport contracts change.

Canonical examples are `behaviour.mix`, `report_selection.mix`, `settings.mx`,
`scene.mx` and `authority.mx`. Keep script homes from the macro decision:
app-local `config/macros/` and global `/opt/mixos/mix` hold executable macros;
documents remain in their owning component/app directories.

The [file-family contract](../spec/mix/2026-10-08-programs-and-documents.md)
specifies boundaries and acceptance. The [Mix guide](../mix.md) distinguishes
current behaviour from the target. This decision updates the source-layout
file conventions and the macro contract's planned data-fixture spelling while
preserving their existing compatibility obligations.
