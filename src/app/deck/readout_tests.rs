//! Tempo formatting is checked on the real JogWheel, not a second implementation of its maths.
use crate::app::headless;
use slint::{ComponentHandle, Model, Rgb8Pixel, VecModel};
use std::rc::Rc;

slint::slint! {
    import { JogWheel } from "../../../ui/deck-controls.slint";
    export { DeckState, DeckView } from "../../../ui/deck-state.slint";
    export { Theme } from "../../../ui/theme.slint";
    export component TempoReadoutTest inherits Window {
        preferred-width: 240px;
        preferred-height: 120px;
        min-width: 0px;
        min-height: 0px;
        JogWheel { index: 0; x: 0px; y: 0px; width: root.width / 2; }
        JogWheel { index: 1; x: root.width / 2; y: 0px; width: root.width / 2; }
    }
}

#[test]
fn tempo_readout_rounds_both_precisions_without_losing_neighbouring_values() {
    headless::init();
    let app = TempoReadoutTest::new().unwrap();
    let decks = Rc::new(VecModel::from(vec![DeckView::default(); 2]));
    app.global::<DeckState>().set_decks(decks.clone().into());
    app.show().unwrap();
    let window = headless::last_window();

    // The same component serves normal decks and the compact C/D decks (which hide the decimals).
    for (digits, size) in [(2, 120usize), (2, 72), (1, 120), (1, 72)] {
        app.global::<Theme>().set_tempo_decimals(digits);
        window.set_size(slint::PhysicalSize::new((2 * size) as u32, size as u32));
        let mut pixels = vec![Rgb8Pixel::default(); 2 * size * size];
        let mut same_readout = |actual: f32, expected: f32| {
            for (i, bpm) in [actual, expected].into_iter().enumerate() {
                decks.set_row_data(
                    i,
                    DeckView {
                        bpm,
                        ..Default::default()
                    },
                );
            }
            app.window().request_redraw();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, 2 * size);
            });
            pixels
                .chunks_exact(2 * size)
                .all(|row| row[..size] == row[size..])
        };
        let mut check = |actual, expected| {
            assert!(
                same_readout(actual, expected),
                "{actual:?} must display as {expected:.2} BPM in a {size}px jog"
            );
        };

        if digits == 1 {
            check(126.96, 127.0);
            check(127.04, 127.0);
            check(126.94, 126.9);
            assert!(
                !same_readout(126.94, 127.0),
                "tenths must carry into the integer"
            );
            if size >= 80 {
                assert!(
                    !same_readout(127.04, 127.06),
                    "neighbouring tenths must remain distinguishable"
                );
            }
            continue;
        }

        // Reported failure: 126.99 -> 126.100 -> 127.01. Values below 127 must carry as well.
        check(126.999, 127.0);
        check(127.0f32.next_down(), 127.0);
        // The stored BPM is u16 hundredths and playback is capped at 2x (transport::set_rate).
        // Cover every whole-BPM rollover through that maximum, including the float just below it.
        let max_bpm = (2.0 * f32::from(u16::MAX) / 100.0).ceil() as u32;
        for whole in 1..=max_bpm {
            let bpm = whole as f32;
            for (actual, expected) in [
                (bpm - 0.006, bpm - 0.01),
                (bpm - 0.004, bpm),
                (bpm.next_down(), bpm),
                (bpm + 0.004, bpm),
                (bpm + 0.006, bpm + 0.01),
            ] {
                check(actual, expected);
            }
        }
        // Each fractional hundredth, on either side of its half-cent rounding boundary;
        // include changes in the integer's digit count and binary floating-point precision.
        for whole in [1, 9, 10, 99, 100, 126, 127, 255, 300, 655, 999, 1000, 1310] {
            for cent in 0..100 {
                let hundredths = whole * 100 + cent;
                let lower = hundredths as f32 / 100.0;
                let upper = (hundredths + 1) as f32 / 100.0;
                check(lower + 0.004, lower);
                check(lower + 0.006, upper);
            }
        }
        // An always-blank or frozen display must not make the equality checks pass.
        assert!(
            !same_readout(126.99, 127.0),
            "126.99 and 127.00 must remain distinguishable"
        );
        if size >= 80 {
            assert!(
                !same_readout(127.0, 127.01),
                "neighbouring hundredths must remain distinguishable"
            );
        }
    }
}
