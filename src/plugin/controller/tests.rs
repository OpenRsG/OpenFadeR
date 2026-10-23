use super::*;
use openfader_plugin::controller::{api, host, Controller, ControllerPlugin};
use std::cell::{Cell, RefCell};

/// Drives a play control and a light, claims ports named `Pad`, watches `play/1`, panics on a 0xFF message.
struct Mock;
struct Pad;

impl ControllerPlugin for Mock {
    fn probe(port: &str) -> u32 {
        if port.starts_with("Pad") {
            80
        } else {
            0
        }
    }
    fn open(port: &str, _forced: bool) -> Result<Box<dyn Controller>, String> {
        if !port.starts_with("Pad") {
            return Err("no pad here".into());
        }
        host::label("Test pad");
        host::watch("play/1", true);
        host::takeover("eq-low/1", true);
        host::timer(10, false);
        host::log(&format!("settings {}", host::settings()));
        Ok(Box::new(Pad))
    }
}

impl Controller for Pad {
    fn midi(&mut self, msg: &[u8]) {
        assert_ne!(msg[0], 0xFF);
        host::set("play/1", f64::from(msg[2] > 0));
        host::set("nothing", 1.0);
        host::midi_out(&[0x90, 1, host::get("play/1") as u8]);
    }
    fn changed(&mut self, key: &str, value: f64) {
        host::midi_out(&[0x90, key.len() as u8, value as u8]);
    }
    fn timer(&mut self, id: u64) {
        host::midi_out(&[0xB0, id as u8, 1]);
    }
    fn close(&mut self) {
        host::midi_out(&[0x90, 1, 0]);
    }
}

static API: openfader_plugin::controller::ControllerApi = api::<Mock>();

/// A host with one control, `play/1`.
#[derive(Default)]
struct Rig {
    play: Cell<f64>,
    out: RefCell<Vec<Vec<u8>>>,
    label: RefCell<String>,
    watched: RefCell<Vec<String>>,
    takeover: RefCell<Vec<String>>,
    timers: RefCell<Vec<(u32, bool)>>,
    boom: bool,
}

impl Backend for Rig {
    fn get(&self, key: &str) -> f64 {
        assert!(!self.boom, "a backend panic stays on the host's side");
        if key == "play/1" {
            self.play.get()
        } else {
            f64::NAN
        }
    }
    fn set(&self, key: &str, v: f64) -> bool {
        (key == "play/1").then(|| self.play.set(v)).is_some()
    }
    fn midi_out(&self, msg: &[u8]) {
        self.out.borrow_mut().push(msg.to_vec());
    }
    fn timer(&self, ms: u32, repeat: bool) -> u64 {
        self.timers.borrow_mut().push((ms, repeat));
        5
    }
    fn cancel(&self, _: u64) {}
    fn watch(&self, key: &str, on: bool) {
        assert!(on);
        self.watched.borrow_mut().push(key.to_owned());
    }
    fn takeover(&self, key: &str, _: bool) {
        self.takeover.borrow_mut().push(key.to_owned());
    }
    fn label(&self, text: &str) {
        *self.label.borrow_mut() = text.to_owned();
    }
}

#[test]
fn a_plugin_probes_a_port_and_a_session_drives_the_hosts_controls_and_lights() {
    let lib = linked("mock", &API);
    assert_eq!((lib.probe("Pad 1"), lib.probe("Deck 2")), (80, 0));
    let rig = Rig::default();
    let s = lib.open("Pad 1", false, &rig).unwrap();
    assert_eq!(s.plugin(), "mock");
    assert_eq!(*rig.label.borrow(), "Test pad");
    assert_eq!(*rig.watched.borrow(), ["play/1"]);
    assert_eq!(*rig.takeover.borrow(), ["eq-low/1"]);
    assert_eq!(*rig.timers.borrow(), [(10, false)]);

    s.midi(&[0x90, 36, 127], &rig);
    assert_eq!(rig.play.get(), 1.0, "the plugin set the control");
    s.changed("play/1", 1.0, &rig);
    s.timer(5, &rig);
    s.close(&rig);
    assert_eq!(
        *rig.out.borrow(),
        [
            vec![0x90, 1, 1], // read back what it just set
            vec![0x90, 6, 1], // a change of "play/1" lit a light
            vec![0xB0, 5, 1], // the timer
            vec![0x90, 1, 0], // close: lights off
        ]
    );
}

#[test]
fn a_refused_open_gives_the_plugins_own_reason_and_probing_has_no_controls() {
    let lib = linked("mock", &API);
    assert_eq!(
        lib.open("Deck 2", false, &Rig::default()).err().as_deref(),
        Some("no pad here")
    );
    // `probe` of a plugin that asks for a control gets nothing: the session does not exist yet.
    struct Greedy;
    impl ControllerPlugin for Greedy {
        fn probe(_: &str) -> u32 {
            u32::from(host::get("play/1").is_nan() && !host::set("play/1", 1.0)) * 7
        }
        fn open(_: &str, _: bool) -> Result<Box<dyn Controller>, String> {
            Err(String::new())
        }
    }
    static GREEDY: openfader_plugin::controller::ControllerApi = api::<Greedy>();
    let rig = Rig::default();
    assert_eq!(linked("greedy", &GREEDY).probe("x"), 7);
    assert_eq!(rig.play.get(), 0.0);
    assert_eq!(
        linked("greedy", &GREEDY)
            .open("x", true, &rig)
            .err()
            .as_deref(),
        Some("greedy: refused the port"),
        "a refusal without a reason still names the plugin"
    );
}

#[test]
fn a_panic_of_the_host_side_does_not_unwind_into_the_plugin() {
    let lib = linked("mock", &API);
    let rig = Rig {
        boom: true,
        ..Rig::default()
    };
    let s = lib.open("Pad 1", false, &rig).unwrap();
    // `get` panics inside the host, the callback answers NaN (read as 0 by the plugin) and the call completes.
    s.midi(&[0x90, 36, 127], &rig);
    assert_eq!(rig.play.get(), 1.0);
    s.close(&rig);
}
