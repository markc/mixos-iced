// Keep a table tight and accurate at load: quarantine corrupt records, remove
// records the caller rejects (cross-table integrity), and prune partition
// symlinks whose primary record is gone. Pure `std::fs`.
pub mod base;
