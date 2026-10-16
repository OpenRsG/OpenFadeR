//! EXTENSIONS category (Preferences > Extensions: STEMS, BEAT LAB, Video, Lighting). The STEMS switches that existed before are base fields of
//! [`Prefs`] (`stems`, `stems_four`, `active_part_solo`, `stems_reset`, `stems_fast`, `stem_wave`); the rest is the typed
//! section [`Extensions`]. BEAT LAB (the panel, needs STEMS), DRUM SWAP (Eject/Load Lock, track titles on the slots) and
//! DRUM CAPTURE (DRUMS stem or original track, Location of Files) drive `app/groove` and `audio/groove`; `drum_pack` is ours (where the
//! factory sample pack lives, for the Factory Presets).
//! The plugin manager
//! under these rows is `app/plugins` (`PluginManager` in ui/plugins.slint).

use super::{Prefs, Row, choice, section};

choice!(
    /// STEMS > Layout: the ACTIVE STEM / STEM ISO / STEM FX displays follow the layout (Auto: all), show the
    /// ones ticked below (Custom), or are hidden.
    StemsLayout { #[default] Auto = "auto", Custom = "custom", Hidden = "hidden" }
);
choice!(
    /// STEM Instant Doubles: the deck whose track a deck takes
    /// (the neighbour on the mixer: A↔B, C↔D, or one fixed deck).
    DoublesFrom { #[default] Neighbour = "neighbour", Deck1 = "deck1", Deck2 = "deck2", Deck3 = "deck3", Deck4 = "deck4" }
);
choice!(
    /// DRUM CAPTURE: what a capture extracts from the track on the deck.
    DrumSource { #[default] Drums = "drums", Original = "original" }
);

section!(
    /// The EXTENSIONS settings that are not base fields of [`Prefs`].
    Extensions {
        /// STEMS > Layout and the displays kept by the Custom layout.
        stems_layout: StemsLayout = StemsLayout::Auto;
        layout_active: bool = true;
        layout_iso: bool = true;
        layout_fx: bool = true;
        /// STEM Instant Doubles.
        doubles_from: DoublesFrom = DoublesFrom::Neighbour;
        /// Multi-thread: the separation uses every core; off = one thread.
        stems_multithread: bool = true;
        /// BEAT LAB: the panel and its toggle in the global bar; the function also needs STEMS.
        groove: bool = false;
        /// The downloaded factory sample pack the Factory Presets come from (empty = none).
        drum_pack: String = String::new();
        /// DRUM SWAP: [Lock] refuses a load into a DRUM SWAP SLOT that is playing; the slots show the loaded track's title.
        drum_lock: bool = false;
        drum_titles: bool = false;
        /// DRUM CAPTURE: what is extracted, and the folder the files go to (empty = `~/Music/OpenFadeR/Drums`).
        drum_source: DrumSource = DrumSource::Drums;
        drum_dir: String = String::new();
        /// Video and Lighting: the panels' switches in the global bar appear only when enabled.
        video: bool = false;
        lighting: bool = false;
    }
);

impl Extensions {
    /// Whether the ACTIVE STEM buttons / STEM ISO / STEM FX are shown, in that order.
    pub fn stem_displays(&self) -> [bool; 3] {
        match self.stems_layout {
            StemsLayout::Auto => [true; 3],
            StemsLayout::Custom => [self.layout_active, self.layout_iso, self.layout_fx],
            StemsLayout::Hidden => [false; 3],
        }
    }

    /// BEAT LAB is on: its own switch and STEMS (`stems_on`, the base field of [`Prefs`]).
    pub fn groove_on(&self, stems_on: bool) -> bool {
        self.groove && stems_on
    }

    /// The deck (0-based) whose track a deck takes with INSTANT DOUBLES; `None` = the neighbour.
    pub fn doubles_deck(&self) -> Option<usize> {
        match self.doubles_from {
            DoublesFrom::Neighbour => None,
            DoublesFrom::Deck1 => Some(0),
            DoublesFrom::Deck2 => Some(1),
            DoublesFrom::Deck3 => Some(2),
            DoublesFrom::Deck4 => Some(3),
        }
    }
}

pub fn rows(p: &Prefs) -> Vec<Row> {
    let e = &p.extensions;
    let custom = e.stems_layout == StemsLayout::Custom;
    vec![
        Row::header("STEMS"),
        Row::chips(
            "stems",
            "ENABLE THE STEMS FUNCTION",
            &["OFF", "ON"],
            p.stems as usize,
        )
        .hint("model on first use"),
        Row::chips(
            "stems_mode",
            "STEMS MODE",
            &["3 STEMS", "4 STEMS"],
            p.stems_four as usize,
        ),
        Row::chips(
            "stems_analysis",
            "STEMS ANALYSIS PROCESS",
            &["QUALITY", "SPEED"],
            p.stems_fast as usize,
        )
        .hint("next track"),
        Row::of("stems_layout", "STEMS LAYOUT", e.stems_layout),
        Row::check(
            "layout_active",
            "Custom layout: ACTIVE STEM buttons",
            e.layout_active,
        )
        .enabled(custom),
        Row::check("layout_iso", "Custom layout: STEM ISO knobs", e.layout_iso).enabled(custom),
        Row::check("layout_fx", "Custom layout: STEM FX", e.layout_fx).enabled(custom),
        Row::chips(
            "stem_waveform",
            "STEM WAVEFORM DISPLAY",
            &["ORIGINAL", "STEM"],
            p.stem_wave as usize,
        ),
        Row::chips(
            "active_part",
            "ACTIVE STEM SETTING",
            &["MUTE", "SOLO"],
            p.active_part_solo as usize,
        )
        .hint("pads and controller"),
        Row::chips(
            "stems_reset",
            "MUTE/SOLO ON LOAD",
            &["RESET", "KEEP"],
            !p.stems_reset as usize,
        ),
        Row::of("doubles_from", "STEM INSTANT DOUBLES", e.doubles_from).hint("source deck"),
        Row::check(
            "stems_multithread",
            "Apply multi-thread to the analysis process",
            e.stems_multithread,
        )
        .hint("next track"),
        Row::header("BEAT LAB"),
        Row::check("groove", "Enable the BEAT LAB Function", e.groove).hint(if p.stems {
            ""
        } else {
            "needs STEMS"
        }),
        Row::text(
            "drum_pack",
            "FACTORY SAMPLES PACK (sample pack)",
            &e.drum_pack,
        ),
        Row::header("DRUM SWAP"),
        Row::chips(
            "drum_lock",
            "EJECT/LOAD LOCK",
            &["UNLOCK", "LOCK"],
            e.drum_lock as usize,
        )
        .hint("no load into a playing slot"),
        Row::check(
            "drum_titles",
            "Display track titles on DRUM SWAP SLOT",
            e.drum_titles,
        ),
        Row::header("DRUM CAPTURE"),
        Row::of("drum_source", "EXTRACT", e.drum_source),
        Row::text("drum_dir", "LOCATION OF FILES", &e.drum_dir),
        Row::info("", "empty = ~/Music/OpenFadeR/Drums"),
        Row::header("VIDEO"),
        Row::check("video", "Enable Video Function", e.video),
        Row::header("LIGHTING"),
        Row::check("lighting", "Enable Lighting Function", e.lighting),
    ]
}

pub fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    p.extensions.pick(key, i)
        || [
            "stems",
            "stems_mode",
            "stems_analysis",
            "stem_waveform",
            "active_part",
            "stems_reset",
        ]
        .contains(&key)
            && p.choose(key, i)
}

/// The text rows (Enter in the field): the pack file and the capture folder.
pub fn wire() {
    super::on_text("drum_pack", |v| {
        super::update(|p| p.extensions.drum_pack = v.trim().to_owned())
    });
    super::on_text("drum_dir", |v| {
        super::update(|p| p.extensions.drum_dir = v.trim().to_owned())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_decide_the_displays() {
        let mut e = Extensions::default();
        assert_eq!(e.stem_displays(), [true; 3]);
        e.stems_layout = StemsLayout::Custom;
        e.layout_iso = false;
        assert_eq!(e.stem_displays(), [true, false, true]);
        e.stems_layout = StemsLayout::Hidden;
        assert_eq!(e.stem_displays(), [false; 3]);
    }

    #[test]
    fn groove_circuit_needs_its_switch_and_stems() {
        let mut p = Prefs::default();
        assert!(!p.extensions.groove_on(true), "off until it is enabled");
        assert!(choose(&mut p, "groove", 1) && p.extensions.groove);
        assert!(
            p.extensions.groove_on(true) && !p.extensions.groove_on(false),
            "the function also needs STEMS"
        );
    }

    #[test]
    fn doubles_source_and_clicks() {
        let mut p = Prefs::default();
        assert_eq!(p.extensions.doubles_deck(), None);
        assert!(choose(&mut p, "doubles_from", 3) && p.extensions.doubles_deck() == Some(2));
        assert!(
            choose(&mut p, "stems", 1) && p.stems,
            "the existing STEMS rows still work"
        );
        assert!(choose(&mut p, "video", 1) && p.extensions.video);
        assert!(!choose(&mut p, "nope", 1));
    }

    #[test]
    fn the_page_has_a_handler_for_every_key() {
        for r in rows(&Prefs::default()).iter().filter(|r| !r.key.is_empty()) {
            assert!(choose(&mut Prefs::default(), &r.key, 0), "{}", r.key);
        }
    }
}
