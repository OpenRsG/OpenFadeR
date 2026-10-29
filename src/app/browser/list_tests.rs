use super::*;
use crate::app::browser::{sources::test_source, timer_test_view};
use openfader::library::{Library, Playlist, Remote};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};

#[derive(Clone)]
struct LibraryRows {
    rows: Rc<RefCell<Arc<Vec<Track>>>>,
    reads: Rc<Cell<usize>>,
    failed: bool,
}
impl LibraryRows {
    fn new(rows: Vec<Track>) -> Self {
        Self {
            rows: Rc::new(RefCell::new(Arc::new(rows))),
            reads: Rc::default(),
            failed: false,
        }
    }
}
impl Library for LibraryRows {
    fn kind(&self) -> &str {
        "metadata fixture"
    }
    fn playlists(&self) -> Result<Vec<Playlist>, String> {
        Ok(Vec::new())
    }
    fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
        self.tracks_all().map(|rows| (*rows).clone())
    }
    fn tracks_all(&self) -> Result<Arc<Vec<Track>>, String> {
        self.reads.set(self.reads.get() + 1);
        if self.failed {
            Err("source unavailable".into())
        } else {
            Ok(self.rows.borrow().clone())
        }
    }
}
fn track(id: Option<i64>, file: Option<&str>, title: &str) -> Track {
    Track {
        id,
        file: file.map(Into::into),
        title: title.into(),
        ..Default::default()
    }
}
fn rig() -> (App, Rc<RefCell<View>>) {
    crate::app::headless::init();
    let app = App::new().unwrap();
    let view = timer_test_view(&app);
    (app, view)
}

#[test]
fn explicit_collection_identity_skips_source_reads_on_restoration() {
    let (_app, v) = rig();
    let collection = LibraryRows::new(vec![track(Some(17), Some("/collection.wav"), "Collection")]);
    let source = LibraryRows::new(vec![track(Some(17), Some("/external.wav"), "Source")]);
    let mut v = v.borrow_mut();
    v.lib = Rc::new(collection);
    v.sources = vec![test_source(Some(Rc::new(source.clone())))];
    v.home = Home::Source(0, None);
    v.tags = vec![track(Some(17), Some("/external.wav"), "Old")];
    v.history = v.tags.clone();
    v.selected = v.tags.first().cloned();
    v.related = v.selected.clone();
    v.reread_metadata();
    assert_eq!(v.tags[0].title, "Collection");
    assert_eq!(
        v.history[0].file.as_deref(),
        Some(Path::new("/collection.wav"))
    );
    assert_eq!(v.selected.as_ref().unwrap().title, "Collection");
    assert_eq!(v.related.as_ref().unwrap().title, "Collection");
    assert_eq!(
        source.reads.get(),
        0,
        "Collection-only restoration never queries a source"
    );
}

#[test]
fn active_source_first_duplicate_and_ordered_fallback_match_single_refresh() {
    let (_app, v) = rig();
    let file = "/tmp/openfader-metadata-precedence.wav";
    let collection = LibraryRows::new(vec![track(None, Some(file), "Collection")]);
    let first = LibraryRows::new(vec![track(None, Some(file), "Earlier source")]);
    let active = LibraryRows::new(vec![
        track(None, Some(file), "Active first"),
        track(None, Some(file), "Active duplicate"),
    ]);
    let mut v = v.borrow_mut();
    v.lib = Rc::new(collection);
    v.sources = vec![
        test_source(Some(Rc::new(first.clone()))),
        test_source(Some(Rc::new(active.clone()))),
    ];
    v.home = Home::Source(1, None);
    let old = track(None, Some(file), "Retained old");
    assert_eq!(v.fresh_track(&old).unwrap().title, "Active first");
    first.reads.set(0);
    active.reads.set(0);
    v.tags = vec![old.clone(); 100];
    v.history = vec![old.clone(); 120];
    v.selected = Some(old.clone());
    v.related = Some(old);
    v.reread_metadata();
    assert!(
        v.tags
            .iter()
            .chain(&v.history)
            .all(|t| t.title == "Active first")
    );
    assert_eq!(
        first.reads.get(),
        0,
        "an active-source hit must not query an unrelated source"
    );
    assert_eq!(active.reads.get(), 1);
    *active.rows.borrow_mut() = Arc::new(Vec::new());
    v.reread_metadata();
    assert_eq!(
        v.tags[0].title, "Earlier source",
        "source replacement must not reuse an old index"
    );
    assert_eq!(first.reads.get(), 1);
    assert_eq!(active.reads.get(), 2);
}

#[test]
fn source_path_predicate_failures_and_fileless_remote_collisions_are_preserved() {
    let (_app, v) = rig();
    let mut failed = LibraryRows::new(Vec::new());
    failed.failed = true;
    let mut remote_a = track(None, None, "Remote name");
    remote_a.remote = Some(Remote {
        plugin: "first".into(),
        source: "library".into(),
        id: "a".into(),
    });
    remote_a.comment = "first remote".into();
    let mut remote_b = remote_a.clone();
    remote_b.remote.as_mut().unwrap().plugin = "second".into();
    remote_b.comment = "second remote".into();
    let rows = LibraryRows::new(vec![
        track(Some(23), Some("/wrong-path.wav"), "Wrong path"),
        remote_a,
    ]);
    let mut v = v.borrow_mut();
    v.lib = Rc::new(LibraryRows::new(vec![track(
        None,
        Some("/collection-fallback.wav"),
        "Collection fallback",
    )]));
    v.sources = vec![
        test_source(None),
        test_source(Some(Rc::new(failed.clone()))),
        test_source(Some(Rc::new(rows.clone()))),
    ];
    v.home = Home::Source(0, None);
    v.tags = vec![
        track(Some(23), Some("/right-path.wav"), "Keep absent"),
        remote_b,
        track(None, Some("/collection-fallback.wav"), "Old fallback"),
    ];
    v.reread_metadata();
    assert_eq!(v.tags[0].title, "Keep absent");
    assert_eq!(v.tags[1].comment, "first remote");
    assert_eq!(v.tags[2].title, "Collection fallback");
    assert_eq!(failed.reads.get(), 1);
    assert_eq!(rows.reads.get(), 1);
}

#[test]
fn restoration_reloads_overlays_and_undo_restores_inherited_metadata() {
    let (_app, v) = rig();
    let original = Track {
        artist: "Inherited artist".into(),
        album: "Inherited album".into(),
        comment: "Inherited comment".into(),
        rating: 3,
        ..track(None, Some("/source-undo.wav"), "Inherited title")
    };
    let key = store::key(&original);
    let source = LibraryRows::new(vec![original.clone()]);
    let mut v = v.borrow_mut();
    let overlay = openfader::store::Overlay::new(Box::new(source.clone()), v.store.clone());
    v.sources = vec![test_source(Some(Rc::new(overlay)))];
    v.tags = vec![original.clone()];
    v.history = vec![original.clone()];
    v.selected = Some(original.clone());
    v.related = Some(original);
    v.store
        .action("Edit metadata", || {
            v.store
                .set_metadata(&key, "Edited title", "Edited artist", "", "", "", "")?;
            v.store.set_rating(&key, Some(0))
        })
        .unwrap();
    v.reread_metadata();
    assert_eq!(v.tags[0].title, "Edited title");
    assert_eq!(v.tags[0].album, "");
    assert_eq!(v.tags[0].rating, 0);
    v.store.undo().unwrap();
    v.reread_metadata();
    for t in v
        .tags
        .iter()
        .chain(&v.history)
        .chain(v.selected.iter())
        .chain(v.related.iter())
    {
        assert_eq!(
            (&*t.title, &*t.artist, &*t.album, &*t.comment, t.rating),
            (
                "Inherited title",
                "Inherited artist",
                "Inherited album",
                "Inherited comment",
                3
            )
        );
    }
    assert_eq!(source.reads.get(), 2);
}

#[test]
#[ignore = "isolated matched source-metadata restoration benchmark; use private HOME"]
fn benchmark_source_metadata_restoration() {
    let (_app, view) = rig();
    let count = 100_000;
    let retained = 1000;
    let rows: Vec<_> = (0..count)
        .map(|i| {
            track(
                None,
                Some(&format!(
                    "/tmp/openfader-metadata-bench-{}/{i:06}.wav",
                    std::process::id()
                )),
                &format!("Source {i}"),
            )
        })
        .collect();
    let source = LibraryRows::new(rows);
    let mut v = view.borrow_mut();
    v.sources = vec![test_source(Some(Rc::new(source.clone())))];
    let old: Vec<_> = (0..retained)
        .map(|i| {
            let mut t = source.rows.borrow()[i * (count / retained)].clone();
            t.title = "Stale".into();
            t
        })
        .collect();
    let start = Instant::now();
    let eager: Vec<_> = old.iter().map(|t| v.fresh_track(t).unwrap()).collect();
    let before = start.elapsed();
    let before_reads = source.reads.replace(0);
    v.history = old;
    let start = Instant::now();
    v.reread_metadata();
    let after = start.elapsed();
    assert_eq!(v.history, eager);
    assert_eq!(source.reads.get(), 1);
    eprintln!(
        "source metadata restoration: source_rows={count} retained={retained} scan={before:?} indexed={after:?} source_reads_before={before_reads} source_reads_after={} indexed_paths={}",
        source.reads.get(),
        retained
    );
}
