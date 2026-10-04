//! Exercises the filesystem document-store: put/get/list/partition/delete and the
//! three reconcile behaviours (dangling-symlink prune, corrupt quarantine, orphan
//! removal). Uses a unique temp directory; no external crates.

use slots::persist::store::base::Store;
use slots::persist::store::reconcile::base::reconcile;
use std::path::PathBuf;

fn temp_table(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("compd-store-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("world.placeholder")
}

const TABLE: &str = "world.placeholder";

#[test]
fn put_list_partition_get_delete() {
    let store = Store::new(temp_table("crud"));
    store.put("rec1", TABLE, 1, b"{\"a\":1}", &[("world_id", "w1")]).unwrap();
    store.put("rec2", TABLE, 1, b"{\"a\":2}", &[("world_id", "w1")]).unwrap();
    store.put("rec3", TABLE, 1, b"{\"a\":3}", &[("world_id", "w2")]).unwrap();

    let mut all = store.list().unwrap();
    all.sort();
    assert_eq!(all, vec!["rec1", "rec2", "rec3"]);

    let mut w1 = store.list_partition("world_id", "w1").unwrap();
    w1.sort();
    assert_eq!(w1, vec!["rec1", "rec2"]);

    assert_eq!(store.get("rec1").unwrap(), Some((1, b"{\"a\":1}".to_vec())));
    assert_eq!(store.get("missing").unwrap(), None);

    store.delete("rec1", &[("world_id", "w1")]).unwrap();
    assert_eq!(store.get("rec1").unwrap(), None);
    let mut w1 = store.list_partition("world_id", "w1").unwrap();
    w1.sort();
    assert_eq!(w1, vec!["rec2"], "partition symlink removed on delete");
}

#[test]
fn reconcile_prunes_dangling_quarantines_corrupt_and_removes_orphans() {
    let table = temp_table("reconcile");
    let store = Store::new(table.clone());
    store.put("good", TABLE, 1, b"{}", &[("world_id", "w1")]).unwrap();
    store.put("corrupt", TABLE, 1, b"{}", &[("world_id", "w1")]).unwrap();
    store.put("orphan", TABLE, 1, b"{}", &[("world_id", "wX")]).unwrap();

    // Make `corrupt`'s record.json unparseable.
    std::fs::write(table.join("id").join("corrupt").join("record.json"), b"not json").unwrap();
    // Hand-create a dangling partition symlink (no primary record).
    let dangle_dir = table.join("world_id").join("w1");
    std::os::unix::fs::symlink("../../id/ghost", dangle_dir.join("ghost")).unwrap();

    // `orphan` is rejected by the keep predicate (its world wX no longer exists).
    let report = reconcile(&table, 12345, |id| id != "orphan");

    assert_eq!(report.corrupt_quarantined, 1);
    assert_eq!(report.orphans_removed, 1);
    // ghost symlink + corrupt's now-missing record + orphan's symlink all pruned.
    assert!(report.dangling_pruned >= 1);

    assert!(store.get("good").unwrap().is_some());
    assert!(store.get("orphan").unwrap().is_none());
    assert!(table.join("id").join("corrupt").join("record.json.corrupt.12345").exists());
    assert!(!dangle_dir.join("ghost").exists(), "dangling symlink pruned");
    let w1: Vec<String> = store.list_partition("world_id", "w1").unwrap();
    assert_eq!(w1, vec!["good"], "only the live record's symlink remains under w1");
}
