use super::{snap::{self, Line, Snap}, *};
use crate::analysis::Beat;

/// `n` frames whose left sample is the frame index and right sample its negative: a frame identifies itself.
fn ramp(n: usize, rate: u32) -> Arc<Pcm> {
    Arc::new(Pcm { rate, samples: (0..n).flat_map(|i| [i as f32, -(i as f32)]).collect() })
}

/// What a rendered arrangement holds, frame by frame: `Some(source frame)` or silence.
fn heard(e: &Edit) -> Vec<Option<usize>> {
    e.render().samples.chunks(2).enumerate().map(|(i, f)| (f[0] != 0.0 || f[1] != 0.0 || e.edl().src_at(i).is_some()).then_some(f[0] as usize)).collect()
}

fn model(n: usize) -> Vec<Option<usize>> {
    (0..n).map(Some).collect()
}

#[test]
fn every_edit_renders_the_expected_samples() {
    let mut e = Edit::new(ramp(100, 1000));
    let mut m = model(100);
    assert_eq!(heard(&e), m);

    assert_eq!(e.cut(10..20), Some(10..10));
    m.drain(10..20);
    assert_eq!(heard(&e), m);

    assert!(e.copy(0..5));
    assert_eq!(e.paste(30), Some(30..35)); // overwrite
    m.splice(30..35, (0..5).map(Some));
    assert_eq!(heard(&e), m);

    assert_eq!(e.insert(50), Some(50..55)); // shifts
    m.splice(50..50, (0..5).map(Some));
    assert_eq!(heard(&e), m);

    assert_eq!(e.clear(60..70), Some(60..70)); // mutes, length kept
    m.splice(60..70, vec![None; 10]);
    assert_eq!(heard(&e), m);

    assert_eq!(e.insert_silence(0, 7), Some(0..7));
    m.splice(0..0, vec![None; 7]);
    assert_eq!(heard(&e), m);

    assert_eq!(e.duplicate(20..24), Some(24..28));
    let dup: Vec<_> = m[20..24].to_vec();
    m.splice(24..24, dup);
    assert_eq!(heard(&e), m);

    assert_eq!(e.delete(0..7), Some(0..0));
    m.drain(0..7);
    assert_eq!(heard(&e), m);
    assert_eq!(e.len(), m.len());
}

#[test]
fn paste_at_the_end_extends_and_edits_that_change_nothing_are_not_recorded() {
    let mut e = Edit::new(ramp(10, 1000));
    assert!(e.copy(2..4));
    assert_eq!(e.paste(9), Some(9..11));
    assert_eq!(e.len(), 11);
    assert_eq!(heard(&e)[9..], [Some(2), Some(3)]);
    let mut e = Edit::new(ramp(10, 1000));
    assert!(!e.copy(5..5), "empty selection copies nothing");
    assert_eq!((e.paste(0), e.delete(3..3), e.duplicate(4..4), e.insert_silence(1, 0)), (None, None, None, None));
    assert!(!e.can_undo() && !e.is_edited());
}

#[test]
fn undo_redo_walk_the_history_and_a_new_edit_drops_redo() {
    let mut e = Edit::new(ramp(40, 1000));
    e.delete(0..10);
    e.clear(5..8);
    let (arranged, wanted) = (e.edl().clone(), heard(&e));
    assert!(e.undo() && e.undo() && !e.undo());
    assert!(!e.is_edited() && e.can_redo());
    assert_eq!(heard(&e), model(40));
    assert!(e.redo() && e.redo() && !e.redo());
    assert_eq!((e.edl(), heard(&e)), (&arranged, wanted));
    e.undo();
    e.insert_silence(0, 3); // a new edit: the undone `clear` is gone
    assert!(!e.can_redo());
    assert_eq!(e.len(), 33);
}

#[test]
fn a_clip_survives_undo_and_the_edl_stays_normalised() {
    let mut e = Edit::new(ramp(20, 1000));
    e.cut(5..10);
    e.undo();
    assert_eq!(e.clip_len(), 5);
    // Pasting the cut-out frames back over themselves changes nothing: not an edit, nothing recorded.
    assert_eq!(e.paste(5), None);
    assert!(!e.is_edited());
    e.cut(5..10);
    assert_eq!(e.insert(5), Some(5..10));
    assert_eq!(e.edl().segs(), &[Seg::Src { start: 0, len: 20 }], "contiguous runs merge back");
}

/// 120 BPM at 1 kHz: a beat every 500 frames, bar line every 4th, `beats` beats.
fn grid(beats: usize) -> Vec<Beat> {
    (0..beats).map(|i| Beat { bar_pos: (i % 4) as u16 + 1, bpm_x100: 12000, time_ms: i as u32 * 500 }).collect()
}

fn frames(l: &[Line]) -> Vec<usize> {
    l.iter().map(|l| l.frame).collect()
}

#[test]
fn the_beat_grid_follows_the_arrangement_and_snapping_uses_it() {
    let beats = grid(16); // 8 s
    let mut e = Edit::new(ramp(8000, 1000));
    let l = snap::lines(e.edl(), &beats, 1000);
    assert_eq!((l.len(), l[4], l[5].bar), (16, Line { frame: 2000, bar: true }, false));

    assert_eq!(snap::snap(&l, Snap::Off, 1234), 1234);
    assert_eq!(snap::snap(&l, Snap::Beat, 1234), 1000);
    assert_eq!(snap::snap(&l, Snap::Beat, 1250), 1000, "ties go to the earlier line");
    assert_eq!(snap::snap(&l, Snap::Beat, 1251), 1500);
    assert_eq!(snap::snap(&l, Snap::Bar, 1234), 2000);
    assert_eq!(snap::snap(&l, Snap::Bar, 1900), 2000);
    assert_eq!(snap::snap(&l, Snap::Bar, 99999), 6000, "past the last bar: the last bar line");
    assert_eq!(snap::snap(&[], Snap::Bar, 77), 77);

    // Delete bar 2 (2000..4000): the third bar line now sits at 2000, silence has no beats.
    let sel = snap::range(&l, Snap::Bar, 2100, 3900, e.len());
    assert_eq!(sel, 2000..4000);
    e.delete(sel);
    let l = snap::lines(e.edl(), &beats, 1000);
    assert_eq!(l.len(), 12);
    assert_eq!(frames(&l)[4..8], [2000, 2500, 3000, 3500]);
    assert_eq!(l[4], Line { frame: 2000, bar: true });
    e.insert_silence(2000, 1000);
    let l = snap::lines(e.edl(), &beats, 1000);
    assert!(frames(&l).iter().all(|f| !(2000..3000).contains(f)));
    assert_eq!(l[4].frame, 3000);
}

#[test]
fn a_drag_always_selects_something_and_never_past_the_end() {
    let l = snap::lines(&Edl::whole(4000), &grid(8), 1000);
    assert_eq!(snap::range(&l, Snap::Beat, 1100, 1150, 4000), 1000..1500, "both ends snap to one line: up to the next");
    assert_eq!(snap::range(&l, Snap::Bar, 3900, 100, 4000), 0..2000, "ends in either order");
    assert_eq!(snap::range(&l, Snap::Bar, 2100, 2200, 4000), 2000..4000, "the next bar line, or the end");
    assert_eq!(snap::range(&l, Snap::Off, 10, 20, 4000), 10..20);
    assert_eq!(snap::range(&l, Snap::Off, 20, 20, 4000), 20..20);
    assert_eq!(snap::range(&l, Snap::Beat, 3000, 9000, 4000), 3000..4000, "dragged past the end: to the end");
    assert_eq!(snap::range(&l, Snap::Beat, 0, 400, 4000), 0..500);
}

#[test]
fn remap_lays_per_frame_data_out_like_the_arrangement() {
    let mut e = Edit::new(ramp(1000, 1000));
    let data: Vec<u8> = (0..10).map(|i| i as u8 + 1).collect(); // 10 entries for 1000 frames: 100 frames each
    e.delete(200..400);
    e.insert_silence(500, 100);
    let out = e.edl().remap(&data, 0.01);
    assert_eq!(out.len(), 9);
    assert_eq!(out, [1, 2, 5, 6, 7, 0, 8, 9, 10]);
}

#[test]
fn export_writes_the_arrangement_as_a_wav_and_never_overwrites() {
    let dir = std::env::temp_dir().join(format!("rk_edit_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut e = Edit::new(Arc::new(Pcm { rate: 44100, samples: (0..2000).flat_map(|i| [(i as f32 / 2000.0) - 0.5, 0.25]).collect() }));
    e.cut(100..300);
    e.insert_silence(0, 50);
    let path = dir.join("edit.wav");
    wav::write(&path, &e).unwrap();
    assert!(wav::write(&path, &e).is_err(), "an existing file is never overwritten");

    let back = crate::audio::decode(&path).unwrap();
    let want = e.render();
    assert_eq!((back.rate, back.samples.len()), (44100, want.samples.len()));
    let worst = back.samples.iter().zip(&want.samples).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(worst < 1e-3, "16-bit quantisation only, worst {worst}");
    assert!(back.samples[..100].iter().all(|s| *s == 0.0), "inserted silence");
    std::fs::remove_dir_all(dir).unwrap();
}
