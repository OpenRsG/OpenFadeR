//! What a write to a device would do, before it does it: the preview card of EXPORT, SYNC, the first-mount sync and Transfer.
//!
//! A format whose plugin lists `Feature::Plan` is asked a dry run (`ExportOptions::dry_run`, which writes nothing); any other gets
//! a host estimate from the audio files (SDK `audio_slot`: where each track would go and whether the device holds it already,
//! no copy). A real export is never run for a preview. The formats' answers are merged: the device holds one copy of each track
//! whatever formats are written, so its bytes count once.
//!
//! The time is `bytes / write speed + files * time per small file + analyses * time per analysis`, with the speeds measured on
//! earlier jobs ([`Speeds`], kept per device in the store) or, until there are any, [`Speeds::DEFAULT`].

use crate::library::snapshot::Snapshot;
use crate::plugin::device::{Control, ExportOptions, ExportPlan, PlanItem, Provider};
use crate::store::{device_settings, Store};
use openfader_plugin::library::{audio_slot, track_key};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// What one format answered.
#[derive(Clone, Debug)]
pub enum Part {
    /// The plugin's dry run.
    Plan(ExportPlan),
    /// The host's own reckoning ([`estimate`]), for a plugin without dry runs.
    Estimate(Estimate),
    /// The dry run failed (no room, no readable library, ...): the same would stop the export.
    Failed(String),
}

/// The host's reckoning of a write: which tracks the device lacks and which it holds.
#[derive(Clone, Debug, Default)]
pub struct Estimate {
    pub add: Vec<PlanItem>,
    pub update: Vec<PlanItem>,
    /// Tracks over the file system's largest file: left out.
    pub too_big: Vec<PlanItem>,
    /// Titles of tracks without a readable audio file: left out.
    pub no_file: Vec<String>,
    pub playlists: usize,
}

/// Files an estimated format writes per track (analysis files) and besides them (library files), and the bytes of those per
/// track: what the rekordbox and Engine DJ layouts write, rounded.
const FILES_PER_TRACK: usize = 2;
const FILES_PER_FORMAT: usize = 4;
const BYTES_PER_TRACK: u64 = 150_000;

/// Everything the card shows, all formats merged.
#[derive(Clone, Debug, Default)]
pub struct Preview {
    /// The formats, in the order they are written (plugin names).
    pub formats: Vec<String>,
    /// Tracks whose audio is copied.
    pub add: Vec<PlanItem>,
    /// Tracks the device holds already: refreshed, audio not copied.
    pub update: Vec<PlanItem>,
    /// Tracks analysed first (Transfer; the caller knows).
    pub analyse: usize,
    pub playlists: usize,
    pub copy_bytes: u64,
    pub write_bytes: u64,
    /// Small files to write.
    pub files: usize,
    /// Why some tracks are left out or the write cannot run, as the plugins word it.
    pub blocked: Vec<String>,
    /// Estimated tracks over the file system's largest file / without audio (the card words them).
    pub too_big: Vec<PlanItem>,
    pub no_file: Vec<String>,
    /// `(format, error)` of a dry run that failed.
    pub failed: Vec<(String, String)>,
    /// Some format was not asked a dry run: the numbers are the host's estimate.
    pub estimated: bool,
}

impl Preview {
    /// Whether START can do anything: the first format (which the others build on) answered, and something is written.
    pub fn can_start(&self) -> bool {
        let first_failed = self
            .failed
            .first()
            .is_some_and(|(f, _)| self.formats.first() == Some(f));
        !first_failed && (!self.add.is_empty() || !self.update.is_empty() || self.playlists > 0)
    }

    /// Bytes the device must have room for.
    pub fn needed(&self) -> u64 {
        self.copy_bytes + self.write_bytes
    }

    /// Seconds the job takes at `s`.
    pub fn seconds(&self, s: &Speeds) -> f64 {
        self.needed() as f64 / s.copy()
            + self.files as f64 * s.small()
            + self.analyse as f64 * s.analyse()
    }
}

/// Where each track of `snap` would go on the device at `root`, and whether the device holds it.
pub fn estimate(root: &Path, snap: &Snapshot, max_file: Option<u64>) -> Estimate {
    let mut e = Estimate {
        playlists: snap.playlists().count(),
        ..Estimate::default()
    };
    for (t, _) in snap.tracks() {
        let size = t
            .file
            .as_deref()
            .and_then(|f| std::fs::metadata(f).ok())
            .map_or(0, |m| m.len());
        let item = |bytes| PlanItem {
            key: track_key(t),
            title: t.title.clone(),
            artist: t.artist.clone(),
            bytes,
        };
        match audio_slot(root, t) {
            Ok((_, true)) => e.update.push(item(0)),
            Ok(_) if max_file.is_some_and(|m| size > m) => e.too_big.push(item(size)),
            Ok(_) => e.add.push(item(size)),
            Err(_) => e.no_file.push(t.title.clone()),
        }
    }
    e
}

/// The merged answers of `parts` (format name, answer), in write order; `analyse` tracks are analysed first.
pub fn merge(parts: &[(String, Part)], analyse: usize) -> Preview {
    let mut p = Preview {
        analyse,
        formats: parts.iter().map(|(f, _)| f.clone()).collect(),
        ..Preview::default()
    };
    let (mut add, mut update) = (HashMap::new(), Vec::new());
    let mut declared = 0;
    let mut take = |p: &mut Preview, a: &[PlanItem], u: &[PlanItem]| {
        for i in a {
            let at = *add.entry(i.key.clone()).or_insert_with(|| {
                p.add.push(i.clone());
                p.add.len() - 1
            });
            p.add[at].bytes = p.add[at].bytes.max(i.bytes);
        }
        update.extend(u.iter().cloned());
    };
    for (format, part) in parts {
        match part {
            Part::Plan(x) => {
                take(&mut p, &x.add, &x.update);
                (p.playlists, p.files) = (p.playlists.max(x.playlists), p.files + x.files);
                (declared, p.write_bytes) =
                    (declared.max(x.copy_bytes), p.write_bytes + x.write_bytes);
                for b in &x.blocked {
                    if !p.blocked.contains(b) {
                        p.blocked.push(b.clone());
                    }
                }
            }
            Part::Estimate(x) => {
                take(&mut p, &x.add, &x.update);
                let n = x.add.len() + x.update.len();
                p.playlists = p.playlists.max(x.playlists);
                p.files += n * FILES_PER_TRACK + FILES_PER_FORMAT;
                p.write_bytes += n as u64 * BYTES_PER_TRACK;
                p.estimated = true;
                for i in &x.too_big {
                    if !p.too_big.iter().any(|t| t.key == i.key) {
                        p.too_big.push(i.clone());
                    }
                }
                for t in &x.no_file {
                    if !p.no_file.contains(t) {
                        p.no_file.push(t.clone());
                    }
                }
            }
            Part::Failed(e) => p.failed.push((format.clone(), e.clone())),
        }
    }
    update.retain(|i: &PlanItem| !add.contains_key(&i.key));
    let mut seen = std::collections::HashSet::new();
    update.retain(|i| seen.insert(i.key.clone()));
    p.update = update;
    p.copy_bytes = p.add.iter().map(|i| i.bytes).sum::<u64>().max(declared);
    p
}

/// Asks every writer (`can_plan(name)`: its plugin lists `Feature::Plan`) a dry run of writing `snap` to `dest`, estimates for
/// the others, and merges. `Err("cancelled")` on a cancel.
pub fn build(
    writers: &[Provider],
    can_plan: &dyn Fn(&str) -> bool,
    dest: &Path,
    snap: &Snapshot,
    o: &ExportOptions,
    analyse: usize,
    ctl: &Arc<Control>,
) -> Result<Preview, String> {
    let dry = ExportOptions {
        dry_run: true,
        ..o.clone()
    };
    let mut guess: Option<Estimate> = None;
    let mut parts = Vec::new();
    for w in writers {
        let mut estimated = || {
            Part::Estimate(
                guess
                    .get_or_insert_with(|| estimate(dest, snap, o.max_file))
                    .clone(),
            )
        };
        let part = if can_plan(w.name()) {
            match w.export(dest, snap, &dry, ctl) {
                Ok(r) => r.plan.map_or_else(&mut estimated, Part::Plan),
                Err(_) if ctl.cancelled() => return Err("cancelled".into()),
                Err(e) => Part::Failed(e),
            }
        } else {
            estimated()
        };
        if ctl.cancelled() {
            return Err("cancelled".into());
        }
        parts.push((w.name().to_owned(), part));
    }
    Ok(merge(&parts, analyse))
}

/// How fast a device writes, as measured on earlier jobs; `None` = not measured yet.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Speeds {
    /// Audio bytes per second.
    pub copy_bps: Option<f64>,
    /// Seconds per small file (analysis files, library files).
    pub small_s: Option<f64>,
    /// Seconds per track analysed (the host's pool; the same for every device).
    pub analyse_s: Option<f64>,
}

// Under `behaviour.` so that `device_settings::config` keeps them from the plugins.
const COPY_KEY: &str = "behaviour.prepare.copy_bps";
const SMALL_KEY: &str = "behaviour.prepare.small_ms";
const ANALYSE_KEY: &str = "behaviour.prepare.analyse_ms";

impl Speeds {
    /// 8 MB/s, 20 ms per small file, 8 s per analysed track.
    pub const DEFAULT: Self = Self {
        copy_bps: Some(8e6),
        small_s: Some(0.020),
        analyse_s: Some(8.0),
    };

    pub fn copy(&self) -> f64 {
        self.copy_bps.or(Self::DEFAULT.copy_bps).unwrap_or(8e6)
    }

    pub fn small(&self) -> f64 {
        self.small_s.or(Self::DEFAULT.small_s).unwrap_or(0.02)
    }

    pub fn analyse(&self) -> f64 {
        self.analyse_s.or(Self::DEFAULT.analyse_s).unwrap_or(8.0)
    }

    /// Whether the write speed of the device was measured (else the time is a guess).
    pub fn measured(&self) -> bool {
        self.copy_bps.is_some() || self.small_s.is_some()
    }

    /// What the store knows of `device` (and of the analysis, which is the computer's).
    pub fn load(store: &Store, device: &str) -> Self {
        let get = |scope: &str, key: &str| {
            store
                .device_settings(scope)
                .ok()
                .and_then(|r| r.get(key)?.parse::<f64>().ok())
                .filter(|v| *v > 0.0)
        };
        Self {
            copy_bps: get(device, COPY_KEY),
            small_s: get(device, SMALL_KEY).map(|ms| ms / 1e3),
            analyse_s: get(device_settings::GLOBAL, ANALYSE_KEY).map(|ms| ms / 1e3),
        }
    }

    /// Keeps a measurement: `copy_bps` / `small_s` of `device`, `analyse_s` of the computer. An old value counts half, so one
    /// odd job (a busy computer) does not make the next estimate wild.
    pub fn remember(store: &Store, device: &str, seen: Self) {
        let old = Self::load(store, device);
        let mix = |old: Option<f64>, new: f64| old.map_or(new, |o| (o + new) / 2.0);
        let put = |scope: &str, key: &str, v: f64| {
            if let Err(e) = store.set_device_setting(scope, key, &format!("{v:.3}")) {
                eprintln!("write speed: {e}");
            }
        };
        if let Some(v) = seen.copy_bps {
            put(device, COPY_KEY, mix(old.copy_bps, v));
        }
        if let Some(v) = seen.small_s {
            put(device, SMALL_KEY, mix(old.small_s, v) * 1e3);
        }
        if let Some(v) = seen.analyse_s {
            put(
                device_settings::GLOBAL,
                ANALYSE_KEY,
                mix(old.analyse_s, v) * 1e3,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Track;

    fn item(key: &str, bytes: u64) -> PlanItem {
        PlanItem {
            key: key.into(),
            title: key.into(),
            bytes,
            ..PlanItem::default()
        }
    }

    fn plan(add: &[(&str, u64)], update: &[&str], blocked: &[&str]) -> Part {
        Part::Plan(ExportPlan {
            add: add.iter().map(|(k, b)| item(k, *b)).collect(),
            update: update.iter().map(|k| item(k, 0)).collect(),
            playlists: 2,
            copy_bytes: add.iter().map(|(_, b)| b).sum(),
            write_bytes: 1000,
            files: 30,
            blocked: blocked.iter().map(|b| (*b).to_owned()).collect(),
            ..ExportPlan::default()
        })
    }

    #[test]
    fn formats_share_one_copy_of_the_audio_and_keep_every_blocked_line() {
        let a = plan(
            &[("a", 100), ("b", 200)],
            &["c"],
            &["big.wav is too big", "x"],
        );
        let b = plan(&[("b", 200), ("d", 50)], &["a", "c"], &["x", "y"]);
        let p = merge(&[("f1".into(), a), ("f2".into(), b)], 3);
        let keys = |v: &[PlanItem]| v.iter().map(|i| i.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&p.add), ["a", "b", "d"]);
        assert_eq!(
            keys(&p.update),
            ["c"],
            "a track that is copied is not also refreshed"
        );
        assert_eq!(p.copy_bytes, 350, "b counts once");
        assert_eq!(
            (p.files, p.write_bytes, p.playlists, p.analyse),
            (60, 2000, 2, 3)
        );
        assert_eq!(p.blocked, ["big.wav is too big", "x", "y"]);
        assert!(!p.estimated && p.can_start());
    }

    #[test]
    fn a_dry_run_and_an_estimate_merge_into_one_preview_marked_as_estimated() {
        let dry = plan(&[("a", 100)], &["b"], &[]);
        let guess = Part::Estimate(Estimate {
            add: vec![item("a", 100), item("c", 40)],
            update: vec![item("b", 0)],
            too_big: vec![item("huge", 9000)],
            no_file: vec!["gone".into()],
            playlists: 1,
        });
        let p = merge(&[("f1".into(), dry), ("f2".into(), guess)], 0);
        let keys = |v: &[PlanItem]| v.iter().map(|i| i.key.clone()).collect::<Vec<_>>();
        assert!(p.estimated);
        assert_eq!(
            (keys(&p.add), keys(&p.update)),
            (vec!["a".to_owned(), "c".into()], vec!["b".to_owned()])
        );
        assert_eq!((p.copy_bytes, p.playlists), (140, 2));
        assert_eq!(
            p.files,
            30 + 3 * FILES_PER_TRACK + FILES_PER_FORMAT,
            "the dry run's count plus the guess"
        );
        assert_eq!(
            (p.too_big.len(), p.no_file.as_slice()),
            (1, ["gone".to_owned()].as_slice())
        );
    }

    #[test]
    fn a_failed_first_format_stops_the_start_a_failed_later_one_does_not() {
        let ok = plan(&[("a", 1)], &[], &[]);
        let bad = Part::Failed("no room".into());
        let first = merge(&[("f1".into(), bad.clone()), ("f2".into(), ok.clone())], 0);
        assert!(!first.can_start());
        let later = merge(&[("f1".into(), ok), ("f2".into(), bad)], 0);
        assert!(later.can_start());
        assert_eq!(later.failed, [("f2".to_owned(), "no room".to_owned())]);
        let nothing = Part::Plan(ExportPlan {
            blocked: vec!["all left out".into()],
            ..ExportPlan::default()
        });
        assert!(!merge(&[("f1".into(), nothing)], 0).can_start());
    }

    fn wav(dir: &Path, name: &str, len: usize) -> Track {
        let f = dir.join(name);
        std::fs::write(&f, vec![0u8; len]).unwrap();
        Track {
            title: name.into(),
            file: Some(f),
            ..Track::default()
        }
    }

    #[test]
    fn the_estimate_tells_new_tracks_from_held_ones_and_leaves_out_what_cannot_be_written() {
        let dir = std::env::temp_dir().join(format!("openfader-plan-{}", std::process::id()));
        let (music, stick) = (dir.join("music"), dir.join("stick"));
        std::fs::create_dir_all(&music).unwrap();
        std::fs::create_dir_all(&stick).unwrap();
        let (a, b, huge) = (
            wav(&music, "a.wav", 1000),
            wav(&music, "b.wav", 2000),
            wav(&music, "huge.wav", 9000),
        );
        let gone = Track {
            title: "gone".into(),
            ..Track::default()
        };
        let snap = Snapshot::of_tracks([&a, &b, &huge, &gone].map(|t| (t.clone(), None)).into());
        let e = estimate(&stick, &snap, Some(5000));
        assert_eq!(
            e.add
                .iter()
                .map(|i| (i.title.as_str(), i.bytes))
                .collect::<Vec<_>>(),
            [("a.wav", 1000), ("b.wav", 2000)]
        );
        assert_eq!(
            (e.too_big.len(), e.no_file.as_slice()),
            (1, ["gone".to_owned()].as_slice())
        );
        // `a` is on the stick now: the second look calls it a refresh, not an add.
        let (to, _) = audio_slot(&stick, &a).unwrap();
        std::fs::create_dir_all(stick.join(&to).parent().unwrap()).unwrap();
        std::fs::copy(a.file.as_ref().unwrap(), stick.join(&to)).unwrap();
        let e = estimate(&stick, &snap, Some(5000));
        assert_eq!((e.add.len(), e.update.len()), (1, 1));
        let p = merge(&[("f".into(), Part::Estimate(e))], 0);
        assert!(p.estimated && p.can_start());
        assert_eq!(
            (p.copy_bytes, p.files),
            (2000, 2 * FILES_PER_TRACK + FILES_PER_FORMAT)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_time_adds_bytes_small_files_and_analyses_at_the_measured_speeds() {
        let p = Preview {
            copy_bytes: 80_000_000,
            files: 100,
            analyse: 2,
            ..Preview::default()
        };
        let d = Speeds::DEFAULT;
        assert!((p.seconds(&d) - (10.0 + 2.0 + 16.0)).abs() < 1e-9);
        let fast = Speeds {
            copy_bps: Some(40e6),
            small_s: Some(0.001),
            analyse_s: Some(1.0),
        };
        assert!((p.seconds(&fast) - (2.0 + 0.1 + 2.0)).abs() < 1e-9);
        assert!(!Speeds::default().measured() && fast.measured());
    }

    #[test]
    fn measured_speeds_are_kept_per_device_and_blend_with_the_last() {
        let s = Store::in_memory().unwrap();
        assert_eq!(Speeds::load(&s, "d1"), Speeds::default());
        Speeds::remember(
            &s,
            "d1",
            Speeds {
                copy_bps: Some(10e6),
                small_s: Some(0.05),
                analyse_s: Some(4.0),
            },
        );
        let k = Speeds::load(&s, "d1");
        assert_eq!(
            (k.copy_bps, k.small_s, k.analyse_s),
            (Some(10e6), Some(0.05), Some(4.0))
        );
        Speeds::remember(
            &s,
            "d1",
            Speeds {
                copy_bps: Some(20e6),
                ..Speeds::default()
            },
        );
        assert_eq!(Speeds::load(&s, "d1").copy_bps, Some(15e6));
        let other = Speeds::load(&s, "d2");
        assert_eq!(
            (other.copy_bps, other.analyse_s),
            (None, Some(4.0)),
            "analysis is the computer's"
        );
        assert!(
            device_settings::config(&s, "d1").is_empty(),
            "plugins never see them"
        );
    }
}
