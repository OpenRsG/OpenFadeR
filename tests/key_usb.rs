//! Our own key detection against the library keys of a real device library (read by its plugin) named by
//! `OPENFADER_USB`; skips otherwise. Aggregate numbers only.

mod common;

use openfader::analysis::key;
use openfader::audio::decode;

/// "8A" -> (8, minor).
fn camelot(s: &str) -> Option<(i32, bool)> {
    let (n, l) = s.split_at(s.len().checked_sub(1)?);
    Some((
        n.parse().ok().filter(|n| (1..=12).contains(n))?,
        l.eq_ignore_ascii_case("a"),
    ))
}

#[test]
fn detected_key_agrees_with_library() {
    let Some((_, tracks)) = common::usb_library() else {
        return;
    };
    let (mut n, mut exact, mut relative, mut fifth, mut worst_ms) = (0, 0, 0, 0, 0);
    for t in &tracks {
        let Some(want) = camelot(&t.key) else {
            continue;
        };
        let pcm = decode(t.file.as_ref().unwrap()).unwrap();
        let started = std::time::Instant::now();
        let got = key::detect(&pcm.samples, pcm.rate)
            .and_then(|k| camelot(&k))
            .expect("a key for real audio");
        worst_ms = worst_ms.max(started.elapsed().as_millis());
        n += 1;
        let step = (got.0 - want.0).rem_euclid(12);
        match (got.1 == want.1, step) {
            (true, 0) => exact += 1,
            (false, 0) => relative += 1,
            (true, 1 | 11) => fifth += 1,
            _ => {}
        }
    }
    eprintln!(
        "keyed tracks={n} exact={exact} relative={relative} fifth={fifth} worst detect time={worst_ms} ms"
    );
    assert!(n > 0, "no track with a Camelot key");
    assert!(
        (exact + relative + fifth) * 5 >= n * 3,
        "agreement {}/{n} < 60%",
        exact + relative + fifth
    );
}
