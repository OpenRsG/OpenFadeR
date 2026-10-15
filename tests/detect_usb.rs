//! Our own beat detection against the beat grids of a real device library (read by its plugin) named by
//! `OPENFADER_USB`; skips otherwise.

mod common;

use openfader::analysis::{Beat, detect};
use openfader::audio::decode;

/// The reference beat nearest to `time_ms` (`reference` is sorted by time).
fn nearest(reference: &[Beat], time_ms: u32) -> &Beat {
    let i = reference.partition_point(|r| r.time_ms < time_ms);
    [i.checked_sub(1), Some(i)]
        .into_iter()
        .flatten()
        .filter_map(|j| reference.get(j))
        .min_by_key(|r| r.time_ms.abs_diff(time_ms))
        .unwrap()
}

#[test]
fn detected_beats_agree_with_the_device() {
    let Some((lib, tracks)) = common::usb_library() else {
        return;
    };
    let (mut exact, mut octave, mut phase_ok, mut aligned, mut down_ok, mut constant) =
        (0, 0, 0, 0, 0, 0);
    let (mut total, mut slowest) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    for t in &tracks {
        let reference = lib.analysis(t).unwrap().beats;
        let pcm = decode(t.file.as_ref().unwrap()).unwrap();
        let start = std::time::Instant::now();
        let beats = detect::beat_grid(&pcm.samples, pcm.rate);
        let took = start.elapsed();
        (total, slowest) = (total + took, slowest.max(took));
        let (want, got) = (
            reference[0].bpm_x100 as f64 / 100.0,
            beats.first().map_or(0.0, |b| b.bpm_x100 as f64 / 100.0),
        );
        let ratio = got / want;
        let is_exact = (ratio - 1.0).abs() < 0.005;
        let is_octave = [0.5, 2.0].iter().any(|r| (ratio / r - 1.0).abs() < 0.005);
        exact += is_exact as usize;
        octave += is_octave as usize;
        // Phase: our first beat within 30 ms of a device beat; over the whole grid: share of our
        // beats within 30 ms of one (a drifting grid scores low).
        let first = nearest(&reference, beats[0].time_ms)
            .time_ms
            .abs_diff(beats[0].time_ms);
        let on_grid: Vec<(&Beat, &Beat)> = beats
            .iter()
            .map(|b| (b, nearest(&reference, b.time_ms)))
            .filter(|(b, r)| r.time_ms.abs_diff(b.time_ms) <= 30)
            .collect();
        let share = on_grid.len() as f64 / beats.len() as f64;
        phase_ok += (first <= 30) as usize;
        aligned += (share >= 0.95) as usize;
        // Bar position: share of our beats on a device beat that carry the same bar position.
        let same = on_grid
            .iter()
            .filter(|(b, r)| b.bar_pos == r.bar_pos)
            .count() as f64
            / on_grid.len().max(1) as f64;
        down_ok += (same >= 0.95) as usize;
        constant += beats.iter().all(|b| b.bpm_x100 == beats[0].bpm_x100) as usize;
        eprintln!(
            "ref {want:7.2} detected {got:7.2} {} first beat {first:3} ms off, on grid {:5.1} %, bar position agrees {:5.1} %",
            if is_exact {
                "exact"
            } else if is_octave {
                "octave"
            } else {
                "MISS"
            },
            share * 100.0,
            same * 100.0
        );
    }
    let n = tracks.len();
    eprintln!(
        "tracks={n} exact={exact} octave={octave} first-beat within 30 ms={phase_ok} grid on the device's (>=95 %)={aligned} bar position (>=95 %)={down_ok} constant grid={constant}"
    );
    eprintln!(
        "beat_grid: {:?} per track on average, {slowest:?} at most",
        total / n as u32
    );
    assert!(
        (exact + octave) * 5 >= n * 4,
        "agreement {}/{n} < 80%",
        exact + octave
    );
    assert!(
        phase_ok * 12 >= n * 11,
        "first beat within 30 ms of the device's: {phase_ok}/{n}"
    );
    assert!(
        aligned * 12 >= n * 11,
        "grid on the device's beats: {aligned}/{n}"
    );
    assert!(
        down_ok * 6 >= n * 5,
        "bar position agrees with the device's: {down_ok}/{n}"
    );
    assert_eq!(constant, n, "every USB track has a constant tempo");
}
