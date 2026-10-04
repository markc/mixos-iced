//! End-to-end document store: sync a collection to disk, rehydrate it filtered by
//! partition, then edit / dismiss / move records and confirm the table tracks each.
//! Drives a unique `$XDG_STATE_HOME` so the on-disk table is isolated.

use slots::persist::document::entry::base::DocumentEntry;
use slots::persist::document::rehydrate::base::rehydrate_documents;
use slots::persist::document::sync::base::sync_documents;
use slots::persist::document::trait_::base::Document;
use slots::define_document;
use slots::storage::slot::base::Storage;
use slots::define_storage;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Note {
    id: String,
    world: String,
    text: String,
}

#[derive(Default)]
struct Notes {
    items: Vec<Note>,
}

define_storage!(pub NOTES, NOTES_MUT: Notes);

struct NotesDoc;
impl Document for NotesDoc {
    type Slot = Notes;
    type Record = Note;
    const TABLE: &'static str = "test.note";
    const VERSION: u32 = 1;
    // This test manages its own `world_id` partition (one note's `world` field),
    // so opt out of the engine's automatic per-building-world injection.
    const WORLD_PARTITIONED: bool = false;
    fn rows(slot: &Notes) -> Vec<(String, Vec<(&'static str, String)>, Note)> {
        slot.items
            .iter()
            .map(|n| (n.id.clone(), vec![("world_id", n.world.clone())], n.clone()))
            .collect()
    }
    fn apply(slot: &mut Notes, _id: &str, record: Note) {
        slot.items.push(record);
    }
}
define_document!(NOTES_ENTRY, NotesDoc, NOTES, NOTES_MUT);

fn w1_ids(storage: &mut Storage, entries: &[&'static DocumentEntry]) -> Vec<(String, String)> {
    storage.insert(&NOTES, Notes::default());
    rehydrate_documents(entries, storage, Some(("world_id", "w1")));
    let mut got: Vec<(String, String)> = storage
        .get(&NOTES)
        .items
        .iter()
        .map(|n| (n.id.clone(), n.text.clone()))
        .collect();
    got.sort();
    got
}

#[test]
fn sync_rehydrate_edit_dismiss_move() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tmp = std::env::temp_dir().join(format!("compd-doc-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe { std::env::set_var("XDG_STATE_HOME", &tmp) };

    let entries: &[&'static DocumentEntry] = &[&NOTES_ENTRY];
    let mut s = Storage::new();
    s.insert(&NOTES, Notes {
        items: vec![
            Note { id: "n1".into(), world: "w1".into(), text: "a".into() },
            Note { id: "n2".into(), world: "w1".into(), text: "b".into() },
            Note { id: "n3".into(), world: "w2".into(), text: "c".into() },
        ],
    });
    sync_documents(entries, &s, uuid::Uuid::nil());

    // Rehydrate a fresh slot filtered to world w1 → only n1, n2.
    assert_eq!(
        w1_ids(&mut Storage::new(), entries),
        vec![("n1".into(), "a".into()), ("n2".into(), "b".into())],
    );

    // Edit n1, dismiss n2, move n3 from w2 to w1; sync again.
    {
        let notes = s.get_mut(&NOTES_MUT);
        notes.items.retain(|n| n.id != "n2");
        notes.items.iter_mut().find(|n| n.id == "n1").unwrap().text = "edited".into();
        notes.items.iter_mut().find(|n| n.id == "n3").unwrap().world = "w1".into();
    }
    sync_documents(entries, &s, uuid::Uuid::nil());

    // w1 now holds the edited n1 and the moved-in n3; n2 is gone.
    assert_eq!(
        w1_ids(&mut Storage::new(), entries),
        vec![("n1".into(), "edited".into()), ("n3".into(), "c".into())],
    );
}
