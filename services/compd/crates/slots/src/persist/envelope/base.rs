use crate::persist::entry::base::PersistError;

/// The framework envelope format version (distinct from a payload's schema
/// version). Bump only if the envelope shape itself changes.
pub const ENVELOPE_VERSION: u32 = 1;

/// The on-disk wrapper. `data` is the storage's `Persisted` value as nested JSON
/// (NOT a string), so the saved file is fully readable. `version` is the payload
/// schema version; `key`/`saved_at_unix` are advisory (debugging).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Envelope {
    pub persist_version: u32,
    pub key: String,
    pub version: u32,
    pub saved_at_unix: u64,
    pub data: serde_json::Value,
}

/// Wrap serialized `Persisted` DATA bytes into pretty envelope file bytes.
pub fn wrap(key: &str, version: u32, data_bytes: &[u8]) -> Result<Vec<u8>, PersistError> {
    let data: serde_json::Value =
        serde_json::from_slice(data_bytes).map_err(|e| PersistError::parse(e.to_string()))?;
    let env = Envelope {
        persist_version: ENVELOPE_VERSION,
        key: key.to_string(),
        version,
        saved_at_unix: now_unix(),
        data,
    };
    serde_json::to_vec_pretty(&env).map_err(|e| PersistError::parse(e.to_string()))
}

/// Parse envelope file bytes into `(payload_version, payload_data_bytes)`. The
/// returned bytes feed `PersistEntry::rehydrate`.
pub fn unwrap(file_bytes: &[u8]) -> Result<(u32, Vec<u8>), PersistError> {
    let env: Envelope =
        serde_json::from_slice(file_bytes).map_err(|e| PersistError::parse(e.to_string()))?;
    if env.persist_version != ENVELOPE_VERSION {
        return Err(PersistError::parse(format!(
            "unsupported envelope format v{} (expected v{ENVELOPE_VERSION})",
            env.persist_version
        )));
    }
    let data_bytes =
        serde_json::to_vec(&env.data).map_err(|e| PersistError::parse(e.to_string()))?;
    Ok((env.version, data_bytes))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
