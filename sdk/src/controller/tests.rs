use super::*;

/// What the test host saw, behind `Host::ctx`.
#[derive(Default)]
struct Seen {
    set: Vec<(String, f64)>,
    out: Vec<Vec<u8>>,
    log: Vec<String>,
    label: String,
    watched: Vec<(String, bool)>,
    timers: Vec<(u32, bool)>,
}

fn seen<'a>(ctx: *mut c_void) -> &'a mut Seen {
    unsafe { &mut *ctx.cast::<Seen>() }
}

unsafe extern "C" fn get(_: *mut c_void, key: Str) -> f64 {
    if unsafe { key.as_str() } == "play/1" {
        1.0
    } else {
        f64::NAN
    }
}
unsafe extern "C" fn set(ctx: *mut c_void, key: Str, v: f64) -> bool {
    seen(ctx).set.push((unsafe { key.as_str() }.to_owned(), v));
    true
}
unsafe extern "C" fn midi_out(ctx: *mut c_void, m: *const u8, n: usize) {
    seen(ctx)
        .out
        .push(unsafe { std::slice::from_raw_parts(m, n) }.to_vec());
}
unsafe extern "C" fn timer(ctx: *mut c_void, ms: u32, repeat: bool) -> u64 {
    seen(ctx).timers.push((ms, repeat));
    7
}
unsafe extern "C" fn cancel(_: *mut c_void, _: u64) {}
unsafe extern "C" fn watch(ctx: *mut c_void, key: Str, on: bool) {
    seen(ctx)
        .watched
        .push((unsafe { key.as_str() }.to_owned(), on));
}
unsafe extern "C" fn takeover(_: *mut c_void, _: Str, _: bool) {}
unsafe extern "C" fn label(ctx: *mut c_void, t: Str) {
    seen(ctx).label = unsafe { t.as_str() }.to_owned();
}
unsafe extern "C" fn log(ctx: *mut c_void, t: Str) {
    seen(ctx).log.push(unsafe { t.as_str() }.to_owned());
}
unsafe extern "C" fn settings(_: *mut c_void) -> Str {
    Str::new(r#"{"dir":"/x"}"#)
}

fn host_of(s: &mut Seen) -> Host {
    Host {
        ctx: (s as *mut Seen).cast(),
        get,
        set,
        midi_out,
        timer,
        cancel,
        watch,
        takeover,
        label,
        log,
        settings,
    }
}

/// Mirrors play to the output as a light, watches `play/1`, panics on a message starting with 0xFF.
struct Echo;
struct Driver;

impl ControllerPlugin for Driver {
    fn probe(port: &str) -> u32 {
        if port.contains("Echo") {
            900
        } else {
            0
        }
    }
    fn open(port: &str, forced: bool) -> Result<Box<dyn Controller>, String> {
        if !port.contains("Echo") && !forced {
            return Err("not an echo".into());
        }
        host::label("Echo pad");
        host::watch("play/1", true);
        host::timer(20, true);
        assert_eq!(host::settings(), r#"{"dir":"/x"}"#);
        Ok(Box::new(Echo))
    }
}

impl Controller for Echo {
    fn midi(&mut self, msg: &[u8]) {
        assert_ne!(msg[0], 0xFF, "boom");
        // note 36 on = press play, off = release it
        host::set("play/1", f64::from(msg[0] & 0xF0 == 0x90 && msg[2] > 0));
    }
    fn changed(&mut self, key: &str, value: f64) {
        assert_eq!(key, "play/1");
        host::midi_out(&[0x90, 36, if value > 0.5 { 127 } else { 0 }]);
    }
    fn timer(&mut self, id: u64) {
        host::log(&format!("tick {id}"));
    }
    fn close(&mut self) {
        host::midi_out(&[0x90, 36, 0]);
    }
}

static API: ControllerApi = api::<Driver>();

#[test]
fn a_session_gets_messages_changes_and_timers_and_its_callbacks_reach_the_host() {
    let mut s = Seen::default();
    let h = host_of(&mut s);
    let port = Str::new("Echo Pad 20:0");
    unsafe {
        assert_eq!(API.abi, CONTROLLER_ABI);
        assert_eq!((API.probe)(port, &h), 100, "a score is clamped to 100");
        assert_eq!((API.probe)(Str::new("Other 21:0"), &h), 0);
        let id = (API.open)(port, false, &h);
        assert_ne!(id, 0);
        (API.midi)(id, [0x90, 36, 100].as_ptr(), 3, &h);
        (API.midi)(id, [0x80, 36, 0].as_ptr(), 3, &h);
        (API.changed)(id, Str::new("play/1"), 1.0, &h);
        (API.timer)(id, 7, &h);
        (API.close)(id, &h);
        (API.midi)(id, [0x90, 36, 100].as_ptr(), 3, &h); // a dead session ignores messages
    }
    assert_eq!(
        s.set,
        [("play/1".to_owned(), 1.0), ("play/1".to_owned(), 0.0)]
    );
    assert_eq!(s.out, [vec![0x90, 36, 127], vec![0x90, 36, 0]]);
    assert_eq!(s.label, "Echo pad");
    assert_eq!(s.watched, [("play/1".to_owned(), true)]);
    assert_eq!(s.timers, [(20, true)]);
    assert_eq!(s.log, ["tick 7"]);
}

#[test]
fn a_refused_open_says_why_and_forcing_it_succeeds() {
    let mut s = Seen::default();
    let h = host_of(&mut s);
    let port = Str::new("Plain 21:0");
    unsafe {
        assert_eq!((API.open)(port, false, &h), 0);
        assert_ne!(
            (API.open)(port, true, &h),
            0,
            "the user assigned this plugin"
        );
    }
    assert_eq!(s.log, ["not an echo"]);
}

#[test]
fn a_panic_in_a_call_is_logged_and_the_session_lives_on() {
    let mut s = Seen::default();
    let h = host_of(&mut s);
    unsafe {
        let id = (API.open)(Str::new("Echo 20:0"), false, &h);
        (API.midi)(id, [0xFF, 0, 0].as_ptr(), 3, &h);
        (API.midi)(id, [0x90, 36, 1].as_ptr(), 3, &h);
    }
    assert_eq!(s.log, ["the plugin panicked in midi"]);
    assert_eq!(s.set, [("play/1".to_owned(), 1.0)]);
}

#[test]
fn the_host_functions_answer_nothing_outside_a_call() {
    assert!(host::get("play/1").is_nan());
    assert!(!host::set("play/1", 1.0));
    assert_eq!(host::timer(10, false), None);
    assert_eq!(host::settings(), "{}");
}
