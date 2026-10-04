// Type-erased handle for a TABLE-flavoured persistable: a storage slot holding a
// COLLECTION of records (vs `PersistEntry`'s single value). Built by `define_document!`
// and returned from `System::documents()`; the engine diffs it per-record at the
// buffer boundary, emitting put/delete/re-link to the filesystem store.
pub mod base;
