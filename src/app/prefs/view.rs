//! VIEW category (Preferences > View: Display Type, Layout, Color; values and defaults are
//! our choice). Typed section [`View`] plus the page rows. Consumers read `prefs::with(|p| p.view.*)` (the owners are named
//! per field); [`super::look`] pushes the look (skin, tooltips, browse font and line space) into the Slint `Theme` global.

use super::{Prefs, Row, Section, choice, section};
use crate::app::skin;

/// The chosen skin's name (a built-in one ([`skin::BUILTIN`], the first is the default) or a skin file's `name`; the old names
/// "dark" and "light" are unknown and read as the default). It is a
/// generic setting under the key `skin` (old settings files keep working) rather than a [`View`] field: the per-frame
/// renderers clone `View`, and a `String` there would allocate every frame.
pub fn skin_name(p: &Prefs) -> &str {
    p.extra("skin").unwrap_or(skin::BUILTIN[0])
}

choice!(
    /// How keys are written in the browser, track info and deck header ([`crate::app::camelot::display`]):
    /// Alphanumeric (Camelot, the default), Classic (musical names), Open Key (ours) or as stored in the
    /// database / tag.
    KeyNotation { #[default] Camelot = "camelot", Classic = "classic", OpenKey = "openkey", Database = "database" }
);
choice!(
    /// Which stored waveform the decks draw ([`crate::app::render`]): 3-band, colour, or
    /// the classic blue mono one. A mode the track has no data for falls back to the next one.
    WaveColors { #[default] ThreeBand = "threeband", Rgb = "rgb", Blue = "blue" }
);
choice!(
    /// Hot cue colours: CLASSIC (hot cues green, hot loops amber), COLD1 (blue and green), COLD2 (blue
    /// gradation), PADS (eight distinct pad colours, by pad) or COLORFUL (the track's own colours). The palettes are
    /// ours, see [`crate::app::render::hot_cue_rgb`].
    HotCueColors { #[default] Colorful = "colorful", Classic = "classic", Cold1 = "cold1", Cold2 = "cold2", Pads = "pads" }
);
choice!(
    /// Character size of the browse lists (values ours: 10 / 11 / 13 px).
    FontSize { Small = "small", #[default] Normal = "normal", Large = "large" }
);
choice!(
    /// Row spacing of the browse lists (values ours: 18 / 22 / 28 px rows).
    LineSpace { Small = "small", #[default] Medium = "medium", Large = "large" }
);
choice!(
    /// Waveform Drawing Rate: how often the waveforms and playheads are redrawn.
    DrawRate { Fps30 = "30", #[default] Fps60 = "60", Fps120 = "120" }
);
choice!(
    /// Overview and preview waveform: the whole waveform, or only its upper half.
    OverviewSize { #[default] Full = "full", Half = "half" }
);
choice!(
    /// Beat Count Display: bars since the start of the track, or the bars / beats to the next memory cue.
    BeatCount { #[default] Position = "position", NextBars = "next-bars", NextBeats = "next-beats" }
);
choice!(
    /// Switch JOG Display: the cue / slip rings, or the distance to the next hot cue.
    JogDisplay { #[default] CueSlip = "cue-slip", HotCueCountdown = "hot-cue-countdown" }
);
choice!(
    /// Traffic Light ("Same Key", "Related Key 1–3"): which keys count as matching the master deck's key.
    TrafficLight { Same = "same", Related1 = "related1", #[default] Related2 = "related2", Related3 = "related3" }
);
choice!(
    /// Which effect panel the FX strip opens with.
    EffectPanel { #[default] BeatFx = "beat-fx", ReleaseFx = "release-fx", MergeFx = "merge-fx" }
);
choice!(
    /// PAD Mode: Auto = switch to the PAD display when a DJ controller connects.
    PadMode { #[default] Auto = "auto", Manual = "manual" }
);
choice!(
    /// PAD FX colour.
    PadFxColor { #[default] Blue = "blue", Green = "green", Orange = "orange", Red = "red", Purple = "purple" }
);
choice!(
    /// SAMPLER colour: buttons follow the sample's frequency colour, or one fixed colour.
    SamplerColor { #[default] Frequency = "frequency", Purple = "purple" }
);
choice!(
    /// 2Deck / 4Deck Horizontal or Vertical: the enlarged waveforms stacked full width above the decks, or standing
    /// side by side between them ("easy for scratching"). The deck count is the `decks` setting. Chosen in the global bar's
    /// layout selector and the View menu (ui/app.slint), applied by ui/app.slint through `PrefsState.vertical`.
    DeckLayout { #[default] Horizontal = "horizontal", Vertical = "vertical" }
);
choice!(
    /// The EXPORT mode layout: 1 PLAYER, 2 PLAYER, DUAL PLAYER, SIMPLE PLAYER (one 69 px player strip over the
    /// browser) or FULL BROWSER (no player). The default is 2 PLAYER.
    ExportLayout { OnePlayer = "1-player", #[default] TwoPlayer = "2-player", DualPlayer = "dual-player", SimplePlayer = "simple-player", FullBrowser = "full-browser" }
);

section!(
    /// The VIEW settings; every field is stored under its own name.
    View {
        /// Tooltips: hover texts of the buttons.
        tooltips: bool = true;
        /// Browse > FontSize / bold, Line Space; read by the browser lists via `Theme`.
        browse_font: FontSize = FontSize::Normal;
        browse_bold: bool = false;
        line_space: LineSpace = LineSpace::Medium;
        /// Show the selected track to the centre: the browser keeps the cursor row mid-list while moving.
        center_selected: bool = false;
        /// Coloring of played tracks and "reset the colour when quitting".
        played_color: bool = true;
        played_reset_on_quit: bool = false;
        /// Waveform Drawing Rate and Full/Half overview.
        draw_rate: DrawRate = DrawRate::Fps60;
        overview: OverviewSize = OverviewSize::Full;
        /// Beat Count Display, Click on the waveform for PLAY and CUE, JOG display.
        beat_count: BeatCount = BeatCount::Position;
        click_wave: bool = true;
        jog_display: JogDisplay = JogDisplay::CueSlip;
        /// Traffic Light: the key highlight range of the browser's KEY column and the deck key.
        traffic_light: TrafficLight = TrafficLight::Related2;
        /// Layout > Media Browser: which entries the browser tree lists. Cue Bank is on by default so the lists are found; the checkbox hides it.
        media_related: bool = true;
        media_hot_cue_bank: bool = true;
        media_track_suggestion: bool = true;
        media_explorer: bool = false;
        /// Layout > Browser panel.
        panel_cue_analysis: bool = false;
        panel_palette: bool = false;
        panel_preview_cues: bool = false;
        panel_all_tracks: bool = false;
        /// Layout > Deck: SYNC RATE.
        deck_sync_rate: bool = true;
        /// Layout > Phrases and Vocal.
        phrase_enlarged: bool = true;
        phrase_full: bool = true;
        /// Names drawn on the phrase band; off = hidden. On by default: the look before this setting existed.
        phrase_names_always: bool = true;
        vocal_enlarged: bool = false;
        vocal_full: bool = false;
        /// Layout > Effect Panel, Enlarged Waveform order (a permutation of 1234), PAD Mode.
        effect_panel: EffectPanel = EffectPanel::BeatFx;
        wave_order: u16 = 1234;
        pad_mode: PadMode = PadMode::Auto;
        /// Color > HOT CUE colour, PAD FX colour, SAMPLER colour. The skin is [`skin_name`].
        hot_cue_colors: HotCueColors = HotCueColors::Colorful;
        pad_fx_color: PadFxColor = PadFxColor::Blue;
        sampler_color: SamplerColor = SamplerColor::Frequency;
        /// Layout selector (global bar, View menu): the orientation of the deck layouts and the EXPORT mode layout.
        deck_layout: DeckLayout = DeckLayout::Horizontal;
        export_layout: ExportLayout = ExportLayout::TwoPlayer;
        /// The mixer column and strip (View menu > Mixer, the global bar's MIXER button, a theme's layout hint `show_mixer`).
        show_mixer: bool = true;
        /// Pane sizes dragged with the Splitters (ui/panes.slint `PaneState`), in px from the default size, 0 = the default: the
        /// enlarged-waveform area, the EXPORT player panel, the browser's tree column, track info and palette, and the track table's
        /// column widths (the title takes the rest). Written by [`panes_read`], restored by [`panes_apply`].
        pane_stage: i32 = 0;
        pane_player: i32 = 0;
        pane_tree: i32 = 0;
        pane_info: i32 = 0;
        pane_palette: i32 = 0;
        col_artist: i32 = 0;
        col_genre: i32 = 0;
        col_comment: i32 = 0;
        col_bpm: i32 = 0;
        col_rating: i32 = 0;
        col_time: i32 = 0;
        col_key: i32 = 0;
    }
);

/// The 24 orders of the four enlarged waveforms, as the digits of deck numbers.
fn orders() -> [u16; 24] {
    let mut v = [0; 24];
    let mut n = 0;
    for a in 1..=4u16 {
        for b in (1..=4).filter(|&b| b != a) {
            for c in (1..=4).filter(|&c| c != a && c != b) {
                v[n] = a * 1000 + b * 100 + c * 10 + (10 - a - b - c);
                n += 1;
            }
        }
    }
    v
}

impl View {
    /// The deck shown in each enlarged-waveform slot, top to bottom (0-based deck indices); a stored value
    /// that is not a permutation of 1 to 4 reads as 1234.
    pub fn wave_order(&self) -> [usize; 4] {
        let o = if orders().contains(&self.wave_order) {
            self.wave_order
        } else {
            1234
        };
        [o / 1000, o / 100 % 10, o / 10 % 10, o % 10].map(|d| d as usize - 1)
    }
}

fn pick_order(v: &mut View, i: usize) {
    if let Some(&o) = orders().get(i) {
        v.wave_order = o;
    }
}

pub fn rows(p: &Prefs) -> Vec<Row> {
    let v = &p.view;
    let all = orders();
    let at = all.iter().position(|&o| o == v.wave_order).unwrap_or(0);
    let order: Vec<String> = all
        .iter()
        .map(|o| {
            o.to_string()
                .chars()
                .map(String::from)
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect();
    let key = |c: KeyNotation| {
        Row::chips(
            "key",
            "KEY DISPLAY",
            &["ALPHANUMERIC", "CLASSIC", "OPEN KEY", "DATABASE"],
            c as usize,
        )
    };
    vec![
        Row::header("DISPLAY TYPE"),
        Row::info("LANGUAGE", "English: no other UI translations are bundled"),
        Row::check("tooltips", "Tooltips", v.tooltips),
        Row::header("BROWSE"),
        Row::of("browse_font", "FONT SIZE", v.browse_font),
        Row::check("browse_bold", "Bold characters", v.browse_bold),
        Row::of("line_space", "LINE SPACE", v.line_space),
        Row::check(
            "center_selected",
            "Show the selected track to the center",
            v.center_selected,
        ),
        Row::check("played_color", "Coloring of played tracks", v.played_color),
        Row::check(
            "played_reset_on_quit",
            "Reset the colour of played tracks when quitting",
            v.played_reset_on_quit,
        )
        .enabled(v.played_color),
        Row::chips("played_reset", "PLAYED TRACKS", &["RESET NOW"], usize::MAX)
            .enabled(v.played_color),
        key(p.key_notation),
        Row::header("WAVEFORM"),
        Row::of("draw_rate", "DRAWING RATE", v.draw_rate).hint("frames / s"),
        Row::chips(
            "overview",
            "OVERVIEW / PREVIEW",
            &["FULL", "HALF"],
            v.overview as usize,
        ),
        Row::of("beat_count", "BEAT COUNT", v.beat_count),
        Row::check(
            "click_wave",
            "Click on the waveform for PLAY and CUE",
            v.click_wave,
        ),
        Row::chips(
            "jog_display",
            "JOG DISPLAY",
            &["CUE / SLIP", "HOT CUE COUNTDOWN"],
            v.jog_display as usize,
        ),
        Row::of("traffic_light", "TRAFFIC LIGHT", v.traffic_light),
        Row::header("LAYOUT: MEDIA BROWSER"),
        Row::check("media_related", "Related Tracks", v.media_related),
        Row::check(
            "media_hot_cue_bank",
            "Cue Bank",
            v.media_hot_cue_bank,
        ),
        Row::check(
            "media_track_suggestion",
            "Track Suggestion",
            v.media_track_suggestion,
        ),
        Row::check("media_explorer", "Explorer", v.media_explorer),
        Row::header("LAYOUT: BROWSER PANEL"),
        Row::check(
            "panel_cue_analysis",
            "CUE Analysis Playlist",
            v.panel_cue_analysis,
        ),
        Row::check("panel_palette", "Playlist Palette", v.panel_palette),
        Row::check(
            "panel_preview_cues",
            "Display Cue Markers on Preview",
            v.panel_preview_cues,
        ),
        Row::check(
            "panel_all_tracks",
            "Display All Tracks in the Playlists",
            v.panel_all_tracks,
        ),
        Row::header("LAYOUT: DECK"),
        Row::check("deck_sync_rate", "Display SYNC RATE", v.deck_sync_rate),
        Row::check(
            "phrase_enlarged",
            "Phrase (Enlarged Waveform)",
            v.phrase_enlarged,
        ),
        Row::check("phrase_full", "Phrase (Full Waveform)", v.phrase_full),
        Row::check(
            "phrase_names_always",
            "Always show the types of phrases",
            v.phrase_names_always,
        ),
        Row::check(
            "vocal_enlarged",
            "Vocal (Enlarged Waveform)",
            v.vocal_enlarged,
        ),
        Row::check("vocal_full", "Vocal (Full Waveform)", v.vocal_full),
        Row::of("effect_panel", "EFFECT PANEL", v.effect_panel),
        Row::dropdown_of("wave_order", "ENLARGED WAVEFORMS", order, at).hint("deck order, 4 decks"),
        Row::chips(
            "pad_mode",
            "PAD MODE",
            &["AUTO", "MANUAL"],
            v.pad_mode as usize,
        ),
        Row::header("COLOR"),
        skin_row(p),
        Row::chips(
            "wave",
            "WAVEFORM COLOR",
            &["3BAND", "RGB", "BLUE"],
            p.waveform_colors as usize,
        ),
        Row::of("hot_cue_colors", "HOT CUE COLOR", v.hot_cue_colors),
        Row::of("pad_fx_color", "PAD FX COLOR", v.pad_fx_color),
        Row::chips(
            "sampler_color",
            "SAMPLER COLOR",
            &["SYNC WITH FREQUENCY", "PURPLE"],
            v.sampler_color as usize,
        ),
        Row::of("deck_layout", "DECK LAYOUT", v.deck_layout).hint("enlarged waveforms"),
        Row::check("show_mixer", "Mixer", v.show_mixer),
        Row::of("export_layout", "EXPORT LAYOUT", v.export_layout),
        Row::header("DECKS"),
        Row::chips(
            "decks",
            "DECKS",
            &["2 DECKS", "4 DECKS"],
            (p.decks == 4) as usize,
        ),
    ]
}

/// The Skin row: the two built-in skins and every skin file found ([`crate::app::skin::names`]), shown upper-case. PREFS > THEMES shows
/// the same choice as a gallery.
fn skin_row(p: &Prefs) -> Row {
    let names: Vec<String> = skin::names().iter().map(|n| n.to_uppercase()).collect();
    let at = skin::position(skin_name(p));
    if names.len() <= 4 {
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        Row::chips("skin", "SKIN", &names, at)
    } else {
        Row::dropdown_of("skin", "SKIN", names, at).hint("see THEMES")
    }
}

/// Chooses the skin `name` (the SKIN row, the theme gallery): stores it, then sets the VIEW settings the skin's `layout` asks for
/// (see [`apply_layout`]; a skin that has none changes nothing else). Returns the hints that are not valid.
pub fn select_skin(p: &mut Prefs, name: &str) -> Vec<String> {
    p.set_extra("skin", name);
    apply_layout(p, &skin::layout_of(name))
}

/// Sets the VIEW settings a skin's `layout` hints name (file names and values as in the settings file: `deck_layout = vertical`,
/// `decks = 4`, `waveform_colors = rgb`, …; `skin::LAYOUT_KEYS` is the list). Returns the hints that are not valid values of
/// their setting, as `key = value`; those settings are left as they were.
pub fn apply_layout(p: &mut Prefs, layout: &[(String, String)]) -> Vec<String> {
    let mut bad = Vec::new();
    for (key, value) in layout {
        let ok = match key.as_str() {
            "waveform_colors" => super::choice::<WaveColors>(value)
                .map(|c| p.waveform_colors = c)
                .is_some(),
            "decks" => matches!(value.as_str(), "2" | "4")
                .then(|| p.decks = if value == "4" { 4 } else { 2 })
                .is_some(),
            k => {
                // an invalid value makes `parse` reset the field: try on a copy
                let mut v = p.view.clone();
                let valid = v.parse(k, value) == Some(true);
                if valid {
                    p.view = v;
                }
                valid
            }
        };
        if !ok {
            bad.push(format!("{key} = {value}"));
        }
    }
    bad
}

pub fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    match key {
        "wave_order" => pick_order(&mut p.view, i),
        "played_reset" => super::run_action(key),
        "skin" => {
            if let Some(n) = skin::names().get(i) {
                let bad = select_skin(p, n);
                bad.iter()
                    .for_each(|b| eprintln!("skin {n:?}: layout hint {b} is not a valid setting"));
            }
        }
        _ => return p.view.pick(key, i),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn layout_hints_set_the_view_settings_and_report_the_ones_that_do_not_fit() {
        let mut p = Prefs::default();
        let all = hints(&[
            ("deck_layout", "vertical"),
            ("decks", "4"),
            ("waveform_colors", "rgb"),
            ("hot_cue_colors", "pads"),
            ("line_space", "small"),
            ("browse_bold", "true"),
            ("overview", "half"),
        ]);
        assert_eq!(apply_layout(&mut p, &all), Vec::<String>::new());
        assert_eq!(p.view.deck_layout, DeckLayout::Vertical);
        assert_eq!(p.decks, 4);
        assert_eq!(p.waveform_colors, WaveColors::Rgb);
        assert_eq!(p.view.hot_cue_colors, HotCueColors::Pads);
        assert_eq!(p.view.line_space, LineSpace::Small);
        assert!(p.view.browse_bold);
        let before = p.clone();
        let none = hints(&[
            ("deck_layout", "diagonal"),
            ("decks", "3"),
            ("waveform_colors", "neon"),
            ("browse_font", "huge"),
        ]);
        assert_eq!(apply_layout(&mut p, &none).len(), 4);
        assert_eq!(p, before, "invalid hints change nothing");
    }
}
