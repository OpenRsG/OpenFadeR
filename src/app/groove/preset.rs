//! Drum Presets: the eight drum loops of DRUM LOOP BANK A and B at once. Factory presets are the playlists of the sampler pack set
//! in PREFS > Extensions, read through the library plugin that opens it (its first eight tracks per list, in list order); user presets are files in `<config>/drum-presets/<name>.preset`, one line
//! per slot: `-` (empty) or `file<TAB>bpm<TAB>title<TAB>path`. Files written by older versions may also hold
//! `pack<TAB>bpm<TAB>title<TAB>pack<TAB>entry` lines (a sample named by its entry in the pack): loading opens the pack through
//! the library and takes the sample whose file ends in that entry.

use openfader::{audio::groove::SLOTS, library, paths};
use std::path::{Path, PathBuf};

const EXT: &str = "preset";

/// What one slot holds: where it came from, its tempo and the title the slot shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub file: PathBuf,
    pub bpm: f32,
    pub title: String,
}

/// A Drum Preset: the eight slots of both banks.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub name: String,
    pub slots: Vec<Option<Entry>>,
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

impl Preset {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for s in self.slots.iter().take(SLOTS) {
            match s {
                None => out.push('-'),
                Some(Entry { file, bpm, title }) => {
                    out += &format!(
                        "file\t{bpm}\t{}\t{}",
                        clean(title),
                        clean(&file.to_string_lossy())
                    )
                }
            }
            out.push('\n');
        }
        out
    }

    /// `None` when a line is neither `-` nor a well-formed slot.
    pub fn from_text(name: &str, text: &str) -> Option<Preset> {
        Self::parse(name, text, &mut |pack| {
            library::open(pack)
                .and_then(|l| l.tracks(None))
                .unwrap_or_default()
        })
    }

    /// [`from_text`](Self::from_text) with `tracks_of(pack)` listing the samples of a sampler pack (the library plugin that
    /// opens it unpacks them to plain files). An old `pack` line names an entry of the pack: it becomes the sample whose file
    /// ends in that entry; a pack that cannot be read leaves the slot empty. Each pack is listed once per call.
    fn parse(
        name: &str,
        text: &str,
        tracks_of: &mut dyn FnMut(&Path) -> Vec<library::Track>,
    ) -> Option<Preset> {
        let mut packs: Vec<(PathBuf, Vec<library::Track>)> = Vec::new();
        let mut slots: Vec<Option<Entry>> = Vec::new();
        for line in text.lines().take(SLOTS) {
            let f: Vec<&str> = line.split('\t').collect();
            slots.push(match f.as_slice() {
                ["-"] => None,
                ["file", bpm, title, path] => Some(Entry {
                    file: PathBuf::from(path),
                    bpm: bpm.parse().ok()?,
                    title: title.to_string(),
                }),
                ["pack", bpm, title, pack, entry] => {
                    let bpm = bpm.parse().ok()?;
                    let pack = PathBuf::from(pack);
                    let at = match packs.iter().position(|(p, _)| *p == pack) {
                        Some(at) => at,
                        None => {
                            let tracks = tracks_of(&pack);
                            packs.push((pack, tracks));
                            packs.len() - 1
                        }
                    };
                    packs[at]
                        .1
                        .iter()
                        .filter_map(|t| t.file.clone())
                        .find(|f| f.ends_with(entry))
                        .map(|file| Entry {
                            file,
                            bpm,
                            title: title.to_string(),
                        })
                }
                _ => return None,
            });
        }
        slots.resize(SLOTS, None);
        Some(Preset {
            name: name.to_owned(),
            slots,
        })
    }
}

/// The factory presets of the pack at `pack`: one per playlist, the first eight tracks (more make another preset). Empty when it cannot be read.
pub fn factory(pack: &Path) -> Vec<Preset> {
    let Ok(lib) = library::open(pack) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for list in lib
        .playlists()
        .unwrap_or_default()
        .iter()
        .filter(|l| !l.is_folder)
    {
        let tracks = lib.tracks(Some(&list.id)).unwrap_or_default();
        for (n, chunk) in tracks.chunks(SLOTS).enumerate() {
            let mut slots: Vec<Option<Entry>> = chunk
                .iter()
                .map(|t| {
                    Some(Entry {
                        file: t.file.clone()?,
                        bpm: t.bpm_x100 as f32 / 100.0,
                        title: t.title.clone(),
                    })
                })
                .collect();
            slots.resize(SLOTS, None);
            let name = if n == 0 {
                list.name.clone()
            } else {
                format!("{} ({})", list.name, n + 1)
            };
            out.push(Preset { name, slots });
        }
    }
    out
}

/// `<config>/drum-presets`.
fn dir() -> Option<PathBuf> {
    Some(paths::config_dir()?.join("drum-presets"))
}

/// The saved presets, by name.
pub fn user() -> Vec<Preset> {
    dir().map(|d| load_dir(&d)).unwrap_or_default()
}

fn load_dir(dir: &Path) -> Vec<Preset> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<Preset> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == EXT))
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_string_lossy().into_owned();
            Preset::from_text(&name, &std::fs::read_to_string(e.path()).ok()?)
        })
        .collect();
    v.sort_by_key(|p| p.name.to_lowercase());
    v
}

/// A file-system safe preset name (empty names are refused).
fn file_name(name: &str) -> Option<String> {
    let n: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    (!n.trim_matches(['.', ' ']).is_empty()).then_some(n)
}

/// Saves the slots as the user preset `name` (replacing one of that name).
pub fn save(name: &str, slots: Vec<Option<Entry>>) -> Result<Preset, String> {
    save_in(
        &dir().ok_or("no settings folder: set HOME or XDG_CONFIG_HOME")?,
        name,
        slots,
    )
}

fn save_in(dir: &Path, name: &str, slots: Vec<Option<Entry>>) -> Result<Preset, String> {
    let file = file_name(name).ok_or("give the Drum Preset a name")?;
    let p = Preset {
        name: file.clone(),
        slots,
    };
    paths::at(std::fs::create_dir_all(dir), dir)?;
    let path = dir.join(format!("{file}.{EXT}"));
    paths::at(std::fs::write(&path, p.to_text()), &path)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Preset {
        let mut slots = vec![None; SLOTS];
        slots[0] = Some(Entry {
            file: "/x/a b/Kick-8b-120.00bpm.wav".into(),
            bpm: 120.0,
            title: "Kick".into(),
        });
        slots[5] = Some(Entry {
            file: "/p/cache/Kit/House1.wav".into(),
            bpm: 123.5,
            title: "House1".into(),
        });
        Preset {
            name: "mine".into(),
            slots,
        }
    }

    #[test]
    fn a_preset_survives_its_file_format() {
        let p = sample();
        assert_eq!(Preset::from_text("mine", &p.to_text()), Some(p));
        assert_eq!(
            Preset::from_text("x", "-\nfile\tnot-a-number\tt\tp\n"),
            None
        );
        assert_eq!(
            Preset::from_text("x", "").unwrap().slots,
            vec![None; SLOTS],
            "missing lines are empty slots"
        );
    }

    #[test]
    fn an_old_pack_line_becomes_the_sample_of_the_unpacked_pack() {
        let mut opened = Vec::new();
        let mut tracks_of = |pack: &Path| {
            opened.push(pack.to_owned());
            if pack != Path::new("/gone.pk") {
                ["Kit/House1.wav", "Kit/House2.wav"]
                    .map(|f| library::Track {
                        file: Some(Path::new("/cache/My Pack").join(f)),
                        ..Default::default()
                    })
                    .to_vec()
            } else {
                Vec::new()
            }
        };
        let text = "pack\t123.5\tHouse1\t/p.pk\tKit/House1.wav\npack\t90\tH2\t/p.pk\tKit/House2.wav\n\
                    pack\t90\tx\t/p.pk\tKit/none.wav\npack\t90\ty\t/gone.pk\tKit/House1.wav\nfile\t90\tt\t/f.wav\n";
        let p = Preset::parse("old", text, &mut tracks_of).unwrap();
        assert_eq!(
            p.slots[0],
            Some(Entry {
                file: "/cache/My Pack/Kit/House1.wav".into(),
                bpm: 123.5,
                title: "House1".into()
            })
        );
        assert_eq!(p.slots[1].as_ref().unwrap().title, "H2");
        assert!(
            p.slots[2].is_none() && p.slots[3].is_none(),
            "unknown entry or unreadable pack: empty"
        );
        assert!(p.slots[4].is_some());
        assert_eq!(opened.len(), 2, "each pack is listed once");
    }

    #[test]
    fn preset_names_are_file_names() {
        assert_eq!(file_name(" Deep House 2 ").as_deref(), Some("Deep House 2"));
        assert_eq!(file_name("a/b:c").as_deref(), Some("a_b_c"));
        assert_eq!(file_name("  "), None);
        assert_eq!(file_name("..."), None);
    }

    #[test]
    fn user_presets_round_trip_through_a_folder() {
        let tmp = std::env::temp_dir().join(format!("rk-groove-presets-{}", std::process::id()));
        let saved = save_in(&tmp, "Warehouse", sample().slots).unwrap();
        save_in(&tmp, "afterhours", vec![]).unwrap();
        let found = load_dir(&tmp);
        assert_eq!(
            found.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["afterhours", "Warehouse"],
            "by name, ignoring case"
        );
        assert_eq!(found[1], saved);
        assert!(save_in(&tmp, "", vec![]).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
