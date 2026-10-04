use crate::persist::envelope::base as envelope;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub orphans_removed: usize,
    pub dangling_pruned: usize,
    pub corrupt_quarantined: usize,
}

/// Tighten a table directory. `keep(id)` decides whether a record should exist
/// (cross-table integrity is the caller's call — e.g. a placeholder whose world
/// is gone returns false); rejected records are removed. Corrupt `record.json`
/// is quarantined to `record.json.corrupt.{now}`. Partition symlinks whose
/// primary record is missing (gone, rejected, or corrupt) are pruned.
pub fn reconcile(table_dir: &Path, now: u64, keep: impl Fn(&str) -> bool) -> ReconcileReport {
    let mut report = ReconcileReport::default();
    let id_dir = table_dir.join("id");

    if let Ok(rd) = std::fs::read_dir(&id_dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let name = entry.file_name().into_string().unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let rec = entry.path().join("record.json");
            let parses = std::fs::read(&rec)
                .ok()
                .and_then(|b| envelope::unwrap(&b).ok())
                .is_some();
            if !parses {
                let _ = std::fs::rename(&rec, entry.path().join(format!("record.json.corrupt.{now}")));
                report.corrupt_quarantined += 1;
            } else if !keep(&name) {
                let _ = std::fs::remove_dir_all(entry.path());
                report.orphans_removed += 1;
            }
        }
    }

    // Partition symlinks: prune any whose target `id/{id}/record.json` is gone.
    if let Ok(top) = std::fs::read_dir(table_dir) {
        for part in top.filter_map(|e| e.ok()) {
            if part.file_name() == *"id" || !part.path().is_dir() {
                continue;
            }
            walk_partition(&part.path(), &id_dir, &mut report);
        }
    }
    report
}

fn walk_partition(part: &Path, id_dir: &Path, report: &mut ReconcileReport) {
    let Ok(values) = std::fs::read_dir(part) else { return };
    for value in values.filter_map(|e| e.ok()) {
        let Ok(links) = std::fs::read_dir(value.path()) else { continue };
        for link in links.filter_map(|e| e.ok()) {
            let id = link.file_name().into_string().unwrap_or_default();
            if !id_dir.join(&id).join("record.json").exists() {
                let _ = std::fs::remove_file(link.path());
                report.dangling_pruned += 1;
            }
        }
    }
}
