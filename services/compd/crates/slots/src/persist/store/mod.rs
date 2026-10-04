// A table in the filesystem-as-database: records are UUID-named folders holding
// `record.json` (an `Envelope`) under a primary `id/` index, with secondary
// indexes as symlinks under partition folders. `Store` is the table handle:
// put/get/delete/list/list_partition over `atomic_write` + `Envelope`.
pub mod base;

pub mod reconcile;
