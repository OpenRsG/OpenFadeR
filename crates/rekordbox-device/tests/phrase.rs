//! PSSI song structure of a real Device Library named by `OPENFADER_USB`; skips otherwise.
//! rekordbox only writes PSSI when phrase analysis ran, so the count of tracks with it is reported.

mod common;

use openfader_plugin::library::Mood;
use rekordbox_common::anlz::load_track;

#[test]
fn phrases_are_ordered_and_inside_the_beat_grid() {
    let Some(root) = common::usb_root() else {
        return;
    };
    let tracks = common::open(&root, None).tracks(None).unwrap();
    let (mut with, mut phrases, mut fills, mut high, mut overshoot) = (0, 0, 0, 0, 0);
    for t in &tracks {
        let a = load_track(std::path::Path::new(t.analysis_ref.as_ref().unwrap())).unwrap();
        let Some(p) = &a.phrases else { continue };
        with += 1;
        high += (p.mood == Mood::High) as usize;
        assert!(!p.list.is_empty());
        let beats = a.beats.len() as u32;
        for w in p.list.windows(2) {
            assert!(
                w[0].beat < w[1].beat,
                "phrases out of order: {} then {}",
                w[0].beat,
                w[1].beat
            );
            assert_eq!(w[0].end_beat, w[1].beat);
        }
        for ph in &p.list {
            assert!(
                (1..=beats).contains(&ph.beat),
                "phrase beat {} outside grid of {beats} beats",
                ph.beat
            );
            assert!(ph.end_beat > ph.beat);
            if let Some(f) = ph.fill {
                assert!(
                    (ph.beat..=ph.end_beat).contains(&f),
                    "fill {f} outside phrase {}..{}",
                    ph.beat,
                    ph.end_beat
                );
                fills += 1;
            }
            let (s, e) = ph.span_ms(&a.beats);
            assert!(s < e);
        }
        // The last phrase may end a few beats past the grid (observed); report how often.
        overshoot += (p.list.last().unwrap().end_beat > beats) as usize;
        phrases += p.list.len();
    }
    eprintln!(
        "tracks={} with_PSSI={with} phrases={phrases} fill-ins={fills} high_mood={high} last_phrase_ends_past_grid={overshoot}",
        tracks.len()
    );
}
