use super::*;
use crate::app::{prefs::set_for_test, App, MixerState};
use openfader::plugin::controller::linked;
use openfader::{
    library::{own::Own, Library},
    store::Store,
};
use openfader_plugin::controller::{
    api, host, Controller as Port, ControllerApi, ControllerPlugin,
};
use slint::{ComponentHandle, Model};
use std::sync::mpsc::channel;

/// A pad: CC 1 is the crossfader, CC 2 the LOW knob of deck 1 (soft takeover on), its lights follow the crossfader, one timer.
struct Pad;
struct Lights;

impl ControllerPlugin for Pad {
    fn probe(port: &str) -> u32 {
        if port.starts_with("Pad") {
            50
        } else {
            0
        }
    }
    fn open(_: &str, _: bool) -> Result<Box<dyn Port>, String> {
        host::label("Test pad");
        host::watch("crossfader", true);
        host::takeover("eq-low/1", true);
        host::timer(5, false);
        Ok(Box::new(Lights))
    }
}

impl Port for Lights {
    fn midi(&mut self, m: &[u8]) {
        match *m {
            [0xB0, 1, v] => host::set("crossfader", f64::from(v) / 127.0),
            [0xB0, 2, v] => host::set("eq-low/1", f64::from(v) / 127.0),
            _ => false,
        };
    }
    fn changed(&mut self, _: &str, v: f64) {
        host::midi_out(&[0xB0, 9, (v * 127.0).round() as u8]);
    }
    fn timer(&mut self, id: u64) {
        host::midi_out(&[0x90, id as u8, 1]);
    }
    fn close(&mut self) {
        host::midi_out(&[0xB0, 9, 0]);
    }
}

/// Answers a better score for the same ports.
struct Better;

impl ControllerPlugin for Better {
    fn probe(port: &str) -> u32 {
        if port.starts_with("Pad") {
            80
        } else {
            0
        }
    }
    fn open(_: &str, _: bool) -> Result<Box<dyn Port>, String> {
        Err("never".into())
    }
}

static PAD: ControllerApi = api::<Pad>();
static BETTER: ControllerApi = api::<Better>();

#[test]
fn a_port_goes_to_the_plugin_the_user_chose_else_the_best_one_that_knows_it() {
    let (pad, better) = (linked("pad", &PAD), linked("better", &BETTER));
    let libs = [&pad, &better];
    let names = ["Pad 20:0".to_owned(), "Deck 21:0".to_owned()];
    let who = |setting: &str| {
        let c = Controller {
            controller_plugins: setting.to_owned(),
            ..Controller::default()
        };
        decide(&names, &c, &libs)
            .iter()
            .map(|(n, l, forced)| (n.as_str(), l.name(), *forced))
            .map(|(n, l, f)| format!("{n}>{l}{}", if f { "!" } else { "" }))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        who(""),
        ["Pad 20:0>better"],
        "AUTO: the best score; Deck is nobody's"
    );
    assert_eq!(
        who("Pad=-"),
        Vec::<String>::new(),
        "the user keeps the port for our mapping files"
    );
    assert_eq!(
        who("Pad=pad;Deck=pad"),
        ["Pad 20:0>pad!", "Deck 21:0>pad!"],
        "a chosen plugin drives the port even when it did not recognise it"
    );
    assert_eq!(
        who("Pad=gone"),
        Vec::<String>::new(),
        "a plugin that is not there drives nothing"
    );
}

#[test]
fn a_claimed_port_works_our_controls_through_its_plugin_and_its_lights_follow() {
    crate::app::headless::init();
    let app = App::new().unwrap();
    let lib: Rc<dyn Library> = Rc::new(Own::new(Arc::new(Store::in_memory().unwrap())));
    let decks = crate::app::deck::wire(&app, lib, Default::default());
    crate::app::mixer::wire(&app, decks.clone());
    let guard = super::super::wire(&app, decks);
    let m = &guard.midi;
    m.close_claims(); // whatever this machine's ports gave

    let (tx, rx) = channel();
    let c = m
        .start(&linked("pad", &PAD), "Pad 20:0", false, rx, None, None)
        .unwrap();
    let lights = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
    let sink = lights.clone();
    *c.st.out.borrow_mut() = Some(Box::new(move |msg| sink.borrow_mut().push(msg.to_vec())));
    m.claimed.borrow_mut().list.push(c);
    assert_eq!(m.claimed_label().as_deref(), Some("pad: Test pad"));
    set_for_test(|p| p.controller.soft_takeover = true);

    let (mixer, send) = (app.global::<MixerState>(), |bytes: [u8; 3]| {
        tx.send(bytes.to_vec()).unwrap()
    });
    send([0xB0, 1, 127]);
    m.read_claimed();
    assert_eq!(mixer.get_crossfader(), 1.0, "the plugin set the crossfader");

    // soft takeover: the knob on screen is at 38, a hardware knob far from it waits until it crosses
    let low = || {
        mixer
            .get_channels()
            .row_data(0)
            .unwrap()
            .knobs
            .row_data(3)
            .unwrap()
    };
    mixer.invoke_knob(0, 3, 38.0 / 127.0);
    send([0xB0, 2, 100]);
    m.read_claimed();
    assert_eq!(low(), 38.0 / 127.0, "far from the value on screen: ignored");
    send([0xB0, 2, 30]);
    m.read_claimed();
    assert_eq!(low(), 30.0 / 127.0, "it crossed the value and takes over");

    // the lights: told at the first poll, and again when the control changes; the timer fires once
    std::thread::sleep(Duration::from_millis(20));
    m.claim_tick();
    assert_eq!(*lights.borrow(), [vec![0x90, 1, 1], vec![0xB0, 9, 127]]);
    mixer.set_crossfader(0.5);
    std::thread::sleep(Duration::from_millis(20));
    m.claim_tick();
    assert_eq!(lights.borrow().last().unwrap(), &[0xB0, 9, 64]);
    m.claim_tick();
    assert_eq!(lights.borrow().len(), 3, "nothing changed: nothing sent");

    m.close_claims();
    assert_eq!(
        lights.borrow().last().unwrap(),
        &[0xB0, 9, 0],
        "the plugin turned its lights off"
    );
    assert!(m.claimed.borrow().is_empty());
    set_for_test(|p| p.controller.soft_takeover = true);
}
