// Developer logging: bring error!/warn!/info!/trace!/abort! into scope.

// World-start rehydration: load each persisted slot from disk and write the
// reconstructed live value back. Missing files are a normal first run; corrupt
// files are quarantined and defaults kept — never fatal.
pub mod base;
