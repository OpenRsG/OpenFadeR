use super::*;
use crate::library::Track;
use std::path::Path;

fn rating(s: &Store, key: &str) -> Option<u8> {
    s.edits(key).unwrap().rating
}

#[test]
fn rating_undo_redo() {
    let s = Store::in_memory().unwrap();
    s.set_rating("a|1", Some(3)).unwrap();
    s.set_rating("a|1", Some(5)).unwrap();
    assert_eq!(s.undo_label().unwrap().as_deref(), Some("Edit rating"));
    assert_eq!(s.undo().unwrap().as_deref(), Some("Edit rating"));
    assert_eq!(rating(&s, "a|1"), Some(3));
    assert_eq!(s.redo().unwrap().as_deref(), Some("Edit rating"));
    assert_eq!(rating(&s, "a|1"), Some(5));
    s.undo().unwrap();
    s.undo().unwrap();
    assert_eq!(
        rating(&s, "a|1"),
        None,
        "the first rating inserted the row: undo removes it"
    );
    assert_eq!(s.undo().unwrap(), None);
    s.redo().unwrap();
    assert_eq!(rating(&s, "a|1"), Some(3));
}

#[test]
fn playlist_delete_restores_node_and_entries() {
    let s = Store::in_memory().unwrap();
    let id = s
        .add_node(None, "Set", super::super::NodeKind::Playlist)
        .unwrap();
    let keys: Vec<String> = ["a|1", "b|2", "c|3"].map(String::from).to_vec();
    s.set_entries(id, &keys).unwrap();
    s.action("Delete playlist", || s.delete_node(id)).unwrap();
    assert!(s.entries(id).unwrap().is_empty());
    assert_eq!(s.undo().unwrap().as_deref(), Some("Delete playlist"));
    assert_eq!(s.entries(id).unwrap(), keys);
    assert!(s
        .nodes()
        .unwrap()
        .iter()
        .any(|n| n.id == id && n.name == "Set"));
    s.redo().unwrap();
    assert!(s.nodes().unwrap().is_empty());
    assert!(s.entries(id).unwrap().is_empty());
}

#[test]
fn blob_columns_survive_undo() {
    let s = Store::in_memory().unwrap();
    let beat = |t| crate::analysis::Beat {
        time_ms: t,
        bpm_x100: 12000,
        bar_pos: 1,
    };
    s.set_grid("a|1", &[beat(10), beat(500)], &[beat(10)])
        .unwrap();
    let before = s.edits("a|1").unwrap().grid;
    s.set_grid("a|1", &[beat(20)], &[beat(10)]).unwrap();
    s.undo().unwrap();
    assert_eq!(s.edits("a|1").unwrap().grid, before);
}

#[test]
fn system_writes_are_not_undone() {
    let s = Store::in_memory().unwrap();
    s.set_rating("a|1", Some(2)).unwrap();
    s.set_rating("a|1", Some(3)).unwrap();
    s.system(|| {
        s.set_comment("a|1", Some("hi")).unwrap();
        s.system(|| s.set_color("a|1", Some(2)).unwrap());
    });
    let seq = s.journal_seq().unwrap();
    s.system(|| s.set_rating("b|2", Some(1)).unwrap());
    assert_eq!(s.journal_seq().unwrap(), seq);
    assert_eq!(s.undo().unwrap().as_deref(), Some("Edit rating"));
    let e = s.edits("a|1").unwrap();
    assert_eq!(
        (e.rating, e.comment.as_deref(), e.color),
        (Some(2), Some("hi"), Some(2))
    );
    assert_eq!(s.edits("b|2").unwrap().rating, Some(1));
    s.undo().unwrap();
    assert_eq!(s.undo().unwrap(), None, "the system writes are no steps");
}

#[test]
fn new_action_after_undo_drops_redo() {
    let s = Store::in_memory().unwrap();
    s.set_rating("a|1", Some(2)).unwrap();
    s.set_rating("a|1", Some(3)).unwrap();
    s.undo().unwrap();
    assert!(s.redo_label().unwrap().is_some());
    s.set_comment("a|1", Some("x")).unwrap();
    assert_eq!(s.redo_label().unwrap(), None);
    assert_eq!(s.redo().unwrap(), None);
}

#[test]
fn old_actions_are_pruned() {
    let s = Store::in_memory().unwrap();
    for i in 0..KEEP + 5 {
        s.set_rating("a|1", Some((i % 5) as u8)).unwrap();
    }
    let n: i64 = scalar(
        &s.db(),
        "SELECT COUNT(*) FROM journal_action WHERE id > 0",
        [],
    )
    .unwrap()
    .unwrap();
    assert_eq!(n, KEEP);
    let rows: i64 = scalar(&s.db(), "SELECT COUNT(*) FROM journal", [])
        .unwrap()
        .unwrap();
    assert_eq!(rows, KEEP, "rows go with their action");
}

#[test]
fn changed_since_names_the_touched_keys() {
    let s = Store::in_memory().unwrap();
    let seq = s.journal_seq().unwrap();
    s.set_rating("t7", Some(2)).unwrap();
    let id = s
        .add_node(None, "L", super::super::NodeKind::Playlist)
        .unwrap();
    let got = s.changed_since(seq).unwrap();
    assert!(got.contains(&("track".into(), Some("t7".into()))));
    assert!(got.contains(&("node".into(), None)));
    assert_eq!(s.journal_touched_since(seq).unwrap(), vec![7]);
    let mid = s.journal_seq().unwrap();
    s.undo().unwrap();
    assert!(
        s.changed_since(mid)
            .unwrap()
            .contains(&("node".into(), None)),
        "undo is reported"
    );
    let _ = id;
}

#[test]
fn replacing_a_saved_sequence_is_undone_to_the_old_one() {
    use super::super::Sequence;
    let s = Store::in_memory().unwrap();
    let a = Sequence {
        bars: 2,
        bank: 1,
        events: vec![1, 2],
    };
    s.save_sequence(0, &a).unwrap();
    s.save_sequence(
        0,
        &Sequence {
            bars: 4,
            ..a.clone()
        },
    )
    .unwrap();
    s.undo().unwrap();
    assert_eq!(s.sequences().unwrap()[0], Some(a));
}

#[test]
fn deleting_a_mapped_track_is_undone_with_its_mapping() {
    let dir = std::env::temp_dir().join(format!("openfader-jmap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.mp3"), b"abc").unwrap();
    let t = Track {
        file: Some(dir.join("a.mp3")),
        ..Default::default()
    };
    let s = Store::in_memory().unwrap();
    let id = s.collection_upsert(&t, None).unwrap();
    let c = s.connection_add("p", Path::new("/x"), "L", 0).unwrap();
    s.system(|| s.map_put(c.id, "f", id, 7)).unwrap();
    s.collection_delete(id).unwrap();
    assert!(s.source_map(c.id).unwrap().is_empty());
    s.undo().unwrap();
    assert_eq!(s.source_map(c.id).unwrap().get("f"), Some(&(id, 7)));
}

#[test]
fn pruning_keeps_what_a_two_way_connection_has_not_written_back() {
    let s = Store::in_memory().unwrap();
    let c = s.connection_add("p", Path::new("/x"), "L", 0).unwrap();
    s.connection_set(
        c.id,
        super::super::connect::Mode::TwoWay,
        super::super::connect::Wins::Newest,
    )
    .unwrap();
    s.set_rating("t7", Some(2)).unwrap();
    for i in 0..KEEP + 5 {
        s.set_rating("a|1", Some((i % 5) as u8)).unwrap();
    }
    assert_eq!(s.journal_touched_since(0).unwrap(), vec![7]);
    // Written back: the old actions may go again.
    s.connection_synced(c.id, "", 0, Some(s.journal_seq().unwrap()))
        .unwrap();
    s.set_rating("a|1", Some(1)).unwrap();
    s.set_rating("a|1", Some(2)).unwrap();
    let n: i64 = scalar(
        &s.db(),
        "SELECT COUNT(*) FROM journal_action WHERE id > 0",
        [],
    )
    .unwrap()
    .unwrap();
    assert!(n <= KEEP + 1);
}
