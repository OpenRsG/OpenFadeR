//! Cue Bank in the browser: the sidebar's "Cue Bank" node (Preferences > View > Layout >
//! Media Browser) holds one entry per list, created / renamed / deleted like My Playlists. A list is up to 8 banks A to H, each the cue
//! or loop of some track; the table lists them in letter order (the trash icon takes a bank out, ▲ ▼ swap two letters). MORE
//! puts cues in (the playhead, active loop or a hot pad of a loaded deck; a hot cue of the chosen track), clears a bank, and
//! loads the list onto a deck's HOT CUE pads ([`crate::app::deck`]'s bank). The lists live in the [store](openfader::store);
//! the device export takes them from there ([`openfader::store::collect_banks`]).

use super::{App, View, playlists::Panel, sidebar::Entry};
use crate::app::{
    analyze,
    deck::{active, fmt_ms},
};
use openfader::{
    analysis::{COLORFUL, Cue},
    library::Track,
    plugin::device::MAX_BANKS,
    store::{self, BankSlot},
};

/// One row of the MORE panel.
#[derive(Clone, Debug)]
pub enum Act {
    /// A cue of a deck or of the chosen track goes into the next free bank.
    Add(BankSlot),
    /// Bank `n` is cleared; the later ones move up a letter.
    Clear(usize),
    /// The list goes onto deck `n`'s pads.
    Load(usize),
    /// Deck `n`'s pads go back to its track's own hot cues.
    Unload(usize),
}

/// Bank letter of list position `i`; positions stay below `MAX_BANKS` (8), so the cast cannot truncate.
fn letter(i: usize) -> char {
    (b'A' + i as u8) as char
}

/// "1:23.4", or "loop 1:23.4–1:30.0" for a loop.
fn span(c: &Cue) -> String {
    match c.loop_end_ms.filter(|&e| e > c.time_ms) {
        Some(e) => tr!("loop {}–{}", fmt_ms(c.time_ms), fmt_ms(e)),
        None => fmt_ms(c.time_ms),
    }
}

/// What `who` (a deck, the chosen track) can put in a bank: the playhead or active loop when `head` is given, then every
/// hot cue. A track without an audio file cannot go on a device and offers nothing.
fn adds(
    who: &str,
    t: &Track,
    cues: &[Cue],
    head: Option<(u32, Option<(u32, u32)>)>,
) -> Vec<(String, Act)> {
    let Some(file) = t.file.clone() else {
        return Vec::new();
    };
    let key = store::key(t);
    let slot = |cue: Cue| BankSlot {
        key: key.clone(),
        file: file.clone(),
        cue,
    };
    let mut rows = Vec::new();
    if let Some((at, looped)) = head {
        let cue = Cue {
            hot_cue: 0,
            time_ms: looped.map_or(at, |l| l.0),
            loop_end_ms: looped.map(|l| l.1),
            color: None,
            comment: None,
        };
        let what = if looped.is_some() {
            tr!("active")
        } else {
            tr!("playhead")
        };
        rows.push((
            tr!("Add {} · {} {}", who, what, span(&cue)),
            Act::Add(slot(cue)),
        ));
    }
    for c in cues.iter().filter(|c| (1..=8).contains(&c.hot_cue)) {
        let pad = letter(c.hot_cue as usize - 1);
        rows.push((
            tr!("Add {} · hot cue {} {}", who, pad, span(c)),
            Act::Add(slot(c.clone())),
        ));
    }
    rows
}

impl View {
    /// The banks of list `id`, as the tracks that hold them (a track the library lacks is skipped; `slots` maps rows to letters).
    pub(super) fn bank_list(&mut self, id: i64) -> Result<Vec<Track>, String> {
        let keys: Vec<String> = self
            .banks
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.slots.iter().map(|s| s.key.clone()).collect())
            .unwrap_or_default();
        self.tracks_of(&keys, true)
    }

    /// The store changed: sidebar counts and the list on screen follow, `msg` is the status. False when the store failed.
    fn bank_changed(&mut self, app: &App, r: Result<(), String>, msg: String) -> bool {
        let ok = self.try_store(app, r).is_some();
        if ok {
            self.rebuild(app);
            self.load(app);
            self.say(app, msg);
        }
        ok
    }

    /// The trash icon: takes the bank on table row `pos` out of the list on screen.
    pub(super) fn bank_remove_at(&mut self, app: &App, id: i64, pos: usize) -> bool {
        let Some(&at) = self.slots.get(pos) else {
            return false;
        };
        let r = self.store.bank_remove_cue(id, at);
        self.bank_changed(app, r, tr!("cleared bank {}", letter(at)))
    }

    /// ▲ ▼: the banks at list positions `a` and `b` swap letters.
    pub(super) fn bank_swap(&mut self, app: &App, id: i64, a: usize, b: usize) {
        let r = self.store.bank_swap(id, a, b);
        self.bank_changed(app, r, tr!("banks {} and {} swapped", letter(a), letter(b)));
    }

    /// MORE pressed on a Cue Bank.
    pub(super) fn open_bank_panel(&mut self, app: &App) {
        let Entry::Bank(id) = self.entry else { return };
        let Some(bank) = self.banks.iter().find(|b| b.id == id).cloned() else {
            return;
        };
        let cache = self.cache().ok();
        let title = |s: &BankSlot| {
            cache
                .as_ref()
                .and_then(|c| c.by_key.get(&s.key).map(|&i| c.tracks[i].title.clone()))
                .unwrap_or_else(|| {
                    s.file
                        .file_name()
                        .map_or_else(String::new, |f| f.to_string_lossy().into_owned())
                })
        };
        let mut rows: Vec<(String, Act)> = bank
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    tr!("✕ Clear {} · {} · {}", letter(i), title(s), span(&s.cue)),
                    Act::Clear(i),
                )
            })
            .collect();
        let decks = active().min(self.decks.len());
        if !bank.slots.is_empty() {
            rows.extend((0..decks).map(|d| (tr!("Load onto Deck {} pads", d + 1), Act::Load(d))));
        }
        rows.extend((0..decks).filter_map(|d| {
            let name = self.decks[d].bank_name()?;
            Some((
                tr!("Release Deck {} pads (“{}”)", d + 1, name),
                Act::Unload(d),
            ))
        }));
        if bank.slots.len() < MAX_BANKS {
            for d in 0..decks {
                if let Some(s) = self.decks[d].bank_source() {
                    rows.extend(adds(
                        &tr!("Deck {}", d + 1),
                        &s.track,
                        &s.cues,
                        Some((s.playhead, s.looped)),
                    ));
                }
            }
            if let Some(t) = self.selected.clone() {
                let cues = analyze::library_cues(&*self.lib, &t);
                rows.extend(adds(&format!("“{}”", t.title), &t, &cues, None));
            }
        }
        let (labels, acts) = rows.into_iter().unzip();
        let title = format!("{} · {}/{MAX_BANKS}", bank.name, bank.slots.len());
        self.show_panel(app, Panel::Bank(acts), &title, labels);
    }

    /// A MORE row of a Cue Bank was chosen.
    pub(super) fn bank_act(&mut self, app: &App, act: Act) {
        let Entry::Bank(id) = self.entry else { return };
        let Some(bank) = self.banks.iter().find(|b| b.id == id).cloned() else {
            return;
        };
        match act {
            Act::Add(mut slot) => {
                let pos = bank.slots.len();
                // The bank takes its letter's number and, unless the cue has its own, its pad colour.
                slot.cue.hot_cue = pos as u16 + 1;
                slot.cue.color = slot.cue.color.or(Some(COLORFUL[pos.min(7)]));
                let msg = tr!(
                    "added {} as bank {} of “{}”",
                    span(&slot.cue),
                    letter(pos),
                    bank.name
                );
                let r = self.store.bank_add_cue(id, &slot).map(drop);
                self.bank_changed(app, r, msg);
            }
            Act::Clear(i) => {
                let r = self.store.bank_remove_cue(id, i);
                self.bank_changed(app, r, tr!("cleared bank {}", letter(i)));
            }
            Act::Load(d) => {
                let cache = match self.cache() {
                    Ok(c) => c,
                    Err(e) => return self.say(app, tr!("library: {}", e)),
                };
                let slots: Vec<_> = bank
                    .slots
                    .iter()
                    .map(|s| {
                        let t = cache.by_key.get(&s.key)?;
                        Some((cache.tracks[*t].clone(), s.cue.clone()))
                    })
                    .collect();
                if let Some(deck) = self.decks.get(d) {
                    deck.load_bank(&bank.name, slots);
                }
            }
            Act::Unload(d) => {
                if let Some(deck) = self.decks.get(d) {
                    deck.unload_bank();
                    self.say(
                        app,
                        tr!("Deck {} pads show the track's own hot cues", d + 1),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(hot: u16, ms: u32, end: Option<u32>) -> Cue {
        Cue {
            hot_cue: hot,
            time_ms: ms,
            loop_end_ms: end,
            color: None,
            comment: None,
        }
    }

    fn track(file: Option<&str>) -> Track {
        Track {
            title: "T".into(),
            file: file.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn a_deck_offers_its_playhead_or_loop_then_its_hot_cues_and_skips_memory_cues() {
        let cues = [
            cue(0, 500, None),
            cue(2, 62_000, None),
            cue(3, 4_000, Some(8_000)),
        ];
        let at = adds(
            "Deck 1",
            &track(Some("/m/a.mp3")),
            &cues,
            Some((1_500, None)),
        );
        let labels: Vec<&str> = at.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Add Deck 1 · playhead 0:01.5",
                "Add Deck 1 · hot cue B 1:02.0",
                "Add Deck 1 · hot cue C loop 0:04.0–0:08.0"
            ]
        );
        let looped = adds(
            "Deck 1",
            &track(Some("/m/a.mp3")),
            &[],
            Some((1_500, Some((2_000, 3_000)))),
        );
        assert_eq!(looped[0].0, "Add Deck 1 · active loop 0:02.0–0:03.0");
        let Act::Add(s) = &looped[0].1 else { panic!() };
        assert_eq!((s.cue.time_ms, s.cue.loop_end_ms), (2_000, Some(3_000)));
        assert_eq!(s.file, std::path::Path::new("/m/a.mp3"));
    }

    #[test]
    fn a_track_without_a_file_cannot_go_on_a_device_so_it_offers_nothing() {
        assert!(adds("x", &track(None), &[cue(1, 1, None)], Some((0, None))).is_empty());
    }
}
