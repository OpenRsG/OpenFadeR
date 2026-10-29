use super::*;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant, UNIX_EPOCH};

struct FileFixture(PathBuf);

impl FileFixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "openfader-shared-stems-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("track.wav");
        audio::write_wav(
            &path,
            &audio::Pcm {
                rate: 48_000,
                samples: vec![0.25; 32],
            },
            48_000,
        )
        .unwrap();
        Self(path)
    }
}

impl Drop for FileFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(self.0.parent().unwrap()).ok();
    }
}

fn pcm(rate: u32, frames: usize, sample: f32) -> Arc<audio::Track16> {
    Arc::new(audio::Track16::from(audio::Pcm {
        rate,
        samples: vec![sample; frames * 2],
    }))
}

fn key(file: &Path, pcm: &audio::Track16) -> Key {
    Key {
        source: Source::identify(file, pcm, false, Version::at(file), &|| false).unwrap(),
        rate: pcm.rate,
        frames: pcm.frames(),
        quality: Quality::Speed,
        multithread: false,
        from_file: false,
        parts: None,
    }
}

#[derive(Default)]
struct Probe {
    starts: Cell<usize>,
    drops: Cell<usize>,
    outcome: RefCell<Option<Result<(), String>>>,
    focus: RefCell<(usize, Vec<usize>)>,
}

struct Fake {
    probe: Rc<Probe>,
    stems: Arc<audio::Stems>,
}

impl Fake {
    fn new(probe: &Rc<Probe>) -> Self {
        probe.starts.set(probe.starts.get() + 1);
        Self {
            probe: probe.clone(),
            stems: Arc::new(audio::Stems::new(
                16,
                openfader::stems::separate::grid(48_000, Quality::Speed),
            )),
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.probe.drops.set(self.probe.drops.get() + 1);
    }
}

impl Producer for Fake {
    fn stems(&self) -> Option<Arc<audio::Stems>> {
        Some(self.stems.clone())
    }
    fn progress(&self) -> (Phase, f32) {
        (Phase::Separate, 0.5)
    }
    fn focus(&self, play: usize, cues: &[usize]) {
        *self.probe.focus.borrow_mut() = (play, cues.to_vec());
    }
    fn poll(&self) -> Option<Result<(), String>> {
        self.probe.outcome.borrow_mut().take()
    }
}

#[test]
fn independent_decodes_share_one_producer_and_one_output_with_independent_completion() {
    let file = FileFixture::new();
    let a_pcm = Arc::new(
        audio::decode_track16_cancellable(&file.0, &|| false)
            .unwrap()
            .0,
    );
    let b_pcm = Arc::new(
        audio::decode_track16_cancellable(&file.0, &|| false)
            .unwrap()
            .0,
    );
    assert!(!Arc::ptr_eq(&a_pcm, &b_pcm));
    let mut registry = Vec::new();
    let probe = Rc::new(Probe::default());
    let a = subscribe(&mut registry, Some(key(&file.0, &a_pcm)), || {
        Fake::new(&probe)
    });
    let b = subscribe(&mut registry, Some(key(&file.0, &b_pcm)), || {
        Fake::new(&probe)
    });
    assert_eq!(probe.starts.get(), 1);
    assert!(Arc::ptr_eq(&a.stems().unwrap(), &b.stems().unwrap()));
    assert_eq!(a.poll(), None);
    *probe.outcome.borrow_mut() = Some(Ok(()));
    assert_eq!(a.poll(), Some(Ok(())));
    assert_eq!(a.poll(), None);
    drop(a);
    assert_eq!(probe.drops.get(), 0);
    assert_eq!(
        b.poll(),
        Some(Ok(())),
        "first consumer cannot steal the outcome"
    );
    assert_eq!(b.poll(), None);
    drop(b);
    assert_eq!(probe.drops.get(), 1);
    assert!(
        registry[0].1.upgrade().is_none(),
        "registry must not retain output"
    );
}

#[test]
fn each_subscriber_receives_failure_once_even_after_first_owner_leaves() {
    let file = FileFixture::new();
    let pcm = pcm(48_000, 16, 0.25);
    let mut registry = Vec::new();
    let probe = Rc::new(Probe::default());
    let a = subscribe(&mut registry, Some(key(&file.0, &pcm)), || {
        Fake::new(&probe)
    });
    let b = subscribe(&mut registry, Some(key(&file.0, &pcm)), || {
        Fake::new(&probe)
    });
    *probe.outcome.borrow_mut() = Some(Err("worker failed".into()));
    assert_eq!(b.poll(), Some(Err("worker failed".into())));
    assert_eq!(b.poll(), None);
    drop(b);
    assert_eq!(a.poll(), Some(Err("worker failed".into())));
    assert_eq!(a.poll(), None);
}

#[test]
fn unload_removes_only_its_focus_and_last_subscriber_cancels() {
    let file = FileFixture::new();
    let pcm = pcm(48_000, 16, 0.25);
    let mut registry = Vec::new();
    let probe = Rc::new(Probe::default());
    let a = subscribe(&mut registry, Some(key(&file.0, &pcm)), || {
        Fake::new(&probe)
    });
    let b = subscribe(&mut registry, Some(key(&file.0, &pcm)), || {
        Fake::new(&probe)
    });
    a.focus(100, &[200, 300]);
    b.focus(700, &[800]);
    assert_eq!(*probe.focus.borrow(), (100, vec![200, 300, 700, 800]));
    a.focus(110, &[210]);
    assert_eq!(*probe.focus.borrow(), (110, vec![210, 700, 800]));
    drop(a);
    assert_eq!(*probe.focus.borrow(), (700, vec![800]));
    assert_eq!(
        probe.drops.get(),
        0,
        "one unload must not cancel the other deck"
    );
    let output = b.stems().unwrap();
    let weak = Arc::downgrade(&output);
    drop(output);
    drop(b);
    assert_eq!(probe.drops.get(), 1);
    assert!(weak.upgrade().is_none());
}

#[test]
fn incompatible_quality_policy_rate_length_content_and_source_versions_do_not_share() {
    let file = FileFixture::new();
    let pcm = pcm(48_000, 16, 0.25);
    let base = key(&file.0, &pcm);
    let probe = Rc::new(Probe::default());
    let mut registry = Vec::new();
    let _owner = subscribe(&mut registry, Some(base.clone()), || Fake::new(&probe));
    let mut variants = Vec::new();
    let mut variant = base.clone();
    variant.quality = Quality::Quality;
    variants.push(variant);
    let mut variant = base.clone();
    variant.multithread = true;
    variants.push(variant);
    let mut variant = base.clone();
    variant.from_file = true;
    variants.push(variant);
    let mut variant = base.clone();
    variant.rate = 44_100;
    variants.push(variant);
    let mut variant = base.clone();
    variant.frames += 1;
    variants.push(variant);
    let mut variant = base.clone();
    variant.source.pcm_digest[0] ^= 1;
    variants.push(variant);
    let mut variant = base.clone();
    variant.source.version.modified = UNIX_EPOCH;
    variants.push(variant);
    let mut variant = base.clone();
    variant.source.file_digest = Some([7; 32]);
    variants.push(variant);
    let mut variant = base.clone();
    variant.parts = Some(stemfile::Info {
        names: std::array::from_fn(|i| format!("part {i}")),
        slot: [0, 1, 2, 3],
    });
    variants.push(variant);
    for variant in variants {
        let before = probe.starts.get();
        let other = subscribe(&mut registry, Some(variant), || Fake::new(&probe));
        assert_eq!(probe.starts.get(), before + 1);
        drop(other);
    }
    let changed = super::tests::pcm(48_000, 16, -0.25);
    assert_ne!(
        base,
        key(&file.0, &changed),
        "same path/rate/length is insufficient"
    );
    std::fs::write(&file.0, b"source version two, replaced").unwrap();
    assert_ne!(
        base,
        key(&file.0, &pcm),
        "a changed source is not the loaded version"
    );
    assert!(Source::identify(&file.0, &pcm, false, Some(base.source.version), &|| false).is_none());
    assert!(Source::identify(&file.0, &pcm, false, Version::at(&file.0), &|| true).is_none());
}

#[test]
fn stem_file_identity_covers_other_tracks_even_when_mix_and_metadata_match() {
    let file = FileFixture::new();
    let pcm = pcm(48_000, 16, 0.25);
    let a = Source::identify(&file.0, &pcm, true, Version::at(&file.0), &|| false).unwrap();
    std::fs::write(&file.0, b"source version two").unwrap();
    let mut b = Source::identify(&file.0, &pcm, true, Version::at(&file.0), &|| false).unwrap();
    b.version = a.version.clone();
    assert_eq!(a.pcm_digest, b.pcm_digest);
    assert_ne!(a.file_digest, b.file_digest);
    assert_ne!(a, b);
}

#[test]
fn absent_identity_never_coalesces_and_a_completed_live_group_still_shares() {
    let file = FileFixture::new();
    let pcm = pcm(48_000, 16, 0.25);
    let probe = Rc::new(Probe::default());
    let mut registry = Vec::new();
    let a = subscribe(&mut registry, None, || Fake::new(&probe));
    let b = subscribe(&mut registry, None, || Fake::new(&probe));
    assert_eq!(probe.starts.get(), 2);
    drop((a, b));
    let a = subscribe(&mut registry, Some(key(&file.0, &pcm)), || {
        Fake::new(&probe)
    });
    *probe.outcome.borrow_mut() = Some(Ok(()));
    assert_eq!(a.poll(), Some(Ok(())));
    let b = subscribe(&mut registry, Some(key(&file.0, &pcm)), || {
        Fake::new(&probe)
    });
    assert_eq!(probe.starts.get(), 3);
    assert_eq!(b.poll(), Some(Ok(())));
    assert!(Arc::ptr_eq(&a.stems().unwrap(), &b.stems().unwrap()));
}

#[test]
fn union_focus_keeps_quality_sequential_and_prioritizes_both_speed_decks() {
    let grid = audio::Grid {
        len: 100,
        overlap: 25,
    };
    let mut quality = openfader::stems::schedule::Schedule::new(grid, 20, true);
    let mut speed = openfader::stems::schedule::Schedule::new(grid, 20, false);
    quality.focus(75 * 2 + 40, &[75 * 5 + 40, 75 * 12 + 40, 75 * 16 + 40]);
    speed.focus(75 * 2 + 40, &[75 * 5 + 40, 75 * 12 + 40, 75 * 16 + 40]);
    assert_eq!(
        (0..20).map(|_| quality.next().unwrap()).collect::<Vec<_>>(),
        (0..20).collect::<Vec<_>>()
    );
    let first: Vec<_> = (0..8).map(|_| speed.next().unwrap()).collect();
    for segment in [2, 5, 12, 16] {
        assert!(first.contains(&segment));
    }
}

// Run alone with disposable HOME/XDG_CACHE_HOME: this exercises the real app entry point and native cache worker.
#[test]
#[ignore = "native cache/output-sharing smoke; use isolated HOME/XDG_CACHE_HOME and --test-threads=1"]
fn native_cached_duplicate_loads_share_producer_output_and_deliver_both_results() {
    let file = FileFixture::new();
    let first_pcm = Arc::new(
        audio::decode_track16_cancellable(&file.0, &|| false)
            .unwrap()
            .0,
    );
    let second_pcm = Arc::new(
        audio::decode_track16_cancellable(&file.0, &|| false)
            .unwrap()
            .0,
    );
    assert!(!Arc::ptr_eq(&first_pcm, &second_pcm));
    let source_a = key(&file.0, &first_pcm).source;
    let source_b = key(&file.0, &second_pcm).source;
    let cached = audio::Stems::new(
        first_pcm.frames(),
        openfader::stems::separate::grid(first_pcm.rate, Quality::Speed),
    );
    for part in 0..audio::STEM_COUNT {
        cached.write(part, 0, &vec![1000 + part as i16; first_pcm.frames() * 2]);
    }
    cached.finish();
    openfader::stems::cache::store(&file.0, first_pcm.rate, &cached, Quality::Speed, &|| false)
        .unwrap();
    // Hold publication at the native cache read so both subscriptions join while the producer is in flight.
    let cache_lock = std::fs::File::options()
        .read(true)
        .write(true)
        .open(openfader::stems::cache::dir().unwrap().join("cache.lock"))
        .unwrap();
    cache_lock.lock().unwrap();
    let a = Subscription::start(
        file.0.clone(),
        first_pcm,
        Quality::Speed,
        false,
        false,
        None,
        Some(source_a),
    );
    let b = Subscription::start(
        file.0.clone(),
        second_pcm,
        Quality::Speed,
        false,
        false,
        None,
        Some(source_b),
    );
    assert!(Rc::ptr_eq(&a.group, &b.group), "one real native producer");
    assert_eq!(a.poll(), None);
    assert_eq!(b.poll(), None);
    drop(cache_lock);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = a.poll() {
            result.unwrap();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cached producer did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(b.poll(), Some(Ok(())));
    assert_eq!(a.poll(), None);
    assert_eq!(b.poll(), None);
    let output = a.stems().unwrap();
    assert!(
        Arc::ptr_eq(&output, &b.stems().unwrap()),
        "one native output allocation"
    );
    let mut samples = [0i16; 2];
    for part in 0..audio::STEM_COUNT {
        output.read(part, 0, &mut samples);
        assert_eq!(
            samples,
            [1000 + part as i16; 2],
            "exact cached samples, not model inference"
        );
    }
    let weak = Arc::downgrade(&output);
    drop(a);
    assert!(b.stems().unwrap().is_complete());
    drop((b, output));
    let deadline = Instant::now() + Duration::from_secs(10);
    while weak.upgrade().is_some() {
        assert!(
            Instant::now() < deadline,
            "no registry retention after final consumer"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!("native stem sharing: producers=1 outputs=1 consumers=2 cache_samples=exact");
}

#[test]
fn native_stem_file_failure_reaches_each_subscriber_once() {
    let file = FileFixture::new();
    let pcm = pcm(48_000, 16, 0.25);
    let source = Source::identify(&file.0, &pcm, true, Version::at(&file.0), &|| false).unwrap();
    let parts = stemfile::Info {
        names: std::array::from_fn(|i| format!("part {i}")),
        slot: [0, 1, 2, 3],
    };
    let a = Subscription::start(
        file.0.clone(),
        pcm.clone(),
        Quality::Speed,
        false,
        true,
        Some(parts.clone()),
        Some(source.clone()),
    );
    let b = Subscription::start(
        file.0.clone(),
        pcm,
        Quality::Speed,
        false,
        true,
        Some(parts),
        Some(source),
    );
    assert!(Rc::ptr_eq(&a.group, &b.group));
    let deadline = Instant::now() + Duration::from_secs(10);
    let error = loop {
        if let Some(result) = a.poll() {
            break result.unwrap_err();
        }
        assert!(Instant::now() < deadline, "stem-file worker did not finish");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(b.poll(), Some(Err(error)));
    assert_eq!(a.poll(), None);
    assert_eq!(b.poll(), None);
}
