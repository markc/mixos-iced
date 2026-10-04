use crate::persist::entry::base::PersistError;

/// Opt a COLLECTION storage slot into the filesystem table store. `Slot` is the
/// whole collection (e.g. a placeholder map); `Record` is one serializable row.
/// Pure data — `define_document!` wraps an impl into a type-erased `DocumentEntry`.
pub trait Document: 'static {
    type Slot: 'static;
    type Record: serde::Serialize + serde::de::DeserializeOwned + PartialEq + Send + 'static;

    /// On-disk table name, e.g. `"world.placeholder"`.
    const TABLE: &'static str;
    /// Record schema version, written into each record's envelope.
    const VERSION: u32;
    /// True (default) = per-world table; rehydration loads only the building
    /// world's `world_id` partition. False = global table (e.g. the `world`
    /// registry), loaded whole.
    const WORLD_PARTITIONED: bool = true;

    /// `(id, partitions, record)` for every record currently in the slot.
    fn rows(slot: &Self::Slot) -> Vec<(String, Vec<(&'static str, String)>, Self::Record)>;
    /// Insert/replace a rehydrated record into the slot.
    fn apply(slot: &mut Self::Slot, id: &str, record: Self::Record);

    /// Migrate an older record's JSON up to `VERSION` (default accepts current only).
    fn migrate(
        from: u32,
        json: serde_json::Value,
    ) -> Result<serde_json::Value, PersistError> {
        if from == Self::VERSION {
            Ok(json)
        } else {
            Err(PersistError::unknown_version(Self::TABLE, from, Self::VERSION))
        }
    }
}

/// Build a `pub static <NAME>: DocumentEntry` for an `impl Document` and its slot
/// tokens. Invoke in the crate owning the (crate-private) write token.
#[macro_export]
macro_rules! define_document {
    ($name:ident, $ty:ty, $read:path, $write:path) => {
        pub static $name: $crate::persist::document::trait_::DocumentEntry = $crate::persist::document::trait_::DocumentEntry {
            table: <$ty as $crate::persist::document::trait_::base::Document>::TABLE,
            version: <$ty as $crate::persist::document::trait_::base::Document>::VERSION,
            world_partitioned: <$ty as $crate::persist::document::trait_::base::Document>::WORLD_PARTITIONED,
            rows: |storage| {
                let slot = storage.try_get(&$read)?;
                let rows = <$ty as $crate::persist::document::trait_::base::Document>::rows(slot);
                ::core::option::Option::Some(
                    rows.into_iter()
                        .map(|(id, partitions, record)| {
                            let bytes = $crate::persist::document::trait_::serde_json::to_vec(&record)
                                .expect("record serializes to JSON");
                            $crate::persist::document::trait_::DocRow {
                                id,
                                partitions,
                                record: ::std::boxed::Box::new(record),
                                bytes,
                            }
                        })
                        .collect(),
                )
            },
            eq: |a, b| {
                match (
                    a.downcast_ref::<<$ty as $crate::persist::document::trait_::base::Document>::Record>(),
                    b.downcast_ref::<<$ty as $crate::persist::document::trait_::base::Document>::Record>(),
                ) {
                    (::core::option::Option::Some(a), ::core::option::Option::Some(b)) => a == b,
                    _ => false,
                }
            },
            apply: |storage, id, bytes, from_version| {
                let value: $crate::persist::document::trait_::serde_json::Value = $crate::persist::document::trait_::serde_json::from_slice(bytes)
                    .map_err(|e| $crate::persist::document::trait_::PersistError::parse(e.to_string()))?;
                let value = <$ty as $crate::persist::document::trait_::base::Document>::migrate(from_version, value)?;
                let record = $crate::persist::document::trait_::serde_json::from_value(value)
                    .map_err(|e| $crate::persist::document::trait_::PersistError::parse(e.to_string()))?;
                <$ty as $crate::persist::document::trait_::base::Document>::apply(storage.get_mut(&$write), id, record);
                ::core::result::Result::Ok(())
            },
        };
    };
}
