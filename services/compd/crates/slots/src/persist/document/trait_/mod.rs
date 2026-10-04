// The `Document` trait + the `define_document!` macro. A collection slot opts into the
// filesystem table store by implementing `Document` (projecting its records) and
// invoking `define_document!`. Re-exports the entry types + serde_json so an owning
// crate needs only THIS crate as a dependency.
pub use crate::persist::document::entry::base::{DocRow, DocumentEntry};
pub use crate::persist::entry::base::PersistError;
pub use serde_json;

#[macro_use]
pub mod base;
