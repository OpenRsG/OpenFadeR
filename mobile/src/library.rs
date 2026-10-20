//! The private imported music directory is indexed by the existing Collection store.
use openfader::{
    library::{folder::Folder, Library, Track},
    store,
};
use std::{
    path::PathBuf,
    sync::{
        mpsc::{self, Receiver},
        Arc,
    },
};

pub fn scan(music: PathBuf) -> Receiver<Result<Arc<Vec<Track>>, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let tracks = Folder::open(&music)?.tracks(None)?;
            let store = store::global();
            store.collection_put(&tracks, false)?;
            let tracks = store.collection_tracks()?;
            Ok(Arc::new(
                tracks
                    .iter()
                    .filter(|t| {
                        t.file
                            .as_ref()
                            .is_some_and(|p| p.starts_with(&music) && p.is_file())
                    })
                    .cloned()
                    .collect(),
            ))
        })();
        let _ = tx.send(result);
    });
    rx
}
