use crate::persist::entry::base::PersistError;
use crate::storage::slot::base::Storage;
use std::any::Any;

/// One record of a collection slot, projected for the store: its id, the partition
/// indexes it belongs to, the typed record (for `PartialEq` change detection), and
/// the serialized DATA bytes (for the write).
pub struct DocRow {
    pub id: String,
    pub partitions: Vec<(&'static str, String)>,
    pub record: Box<dyn Any + Send>,
    pub bytes: Vec<u8>,
}

/// A table-flavoured persist entry. `rows` projects the whole collection slot;
/// `eq` compares two typed records by `PartialEq`; `apply` reconstructs one record
/// into the slot during rehydration.
pub struct DocumentEntry {
    pub table: &'static str,
    pub version: u32,
    /// When true, rehydration at world build loads only this world's partition
    /// (`world_id` = the world's UUID); when false the table is global (loaded
    /// whole, e.g. the `world` registry).
    pub world_partitioned: bool,
    pub rows: fn(&Storage) -> Option<Vec<DocRow>>,
    pub eq: fn(&(dyn Any + Send), &(dyn Any + Send)) -> bool,
    pub apply: fn(&mut Storage, &str, &[u8], u32) -> Result<(), PersistError>,
}
