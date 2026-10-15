//! DECK tab and the start-up defaults of the decks (our choice).

use super::{AUTO_CUE_DB, LOOP_BEATS, Prefs, Row, VINYL_SECONDS};

pub(super) fn rows(p: &Prefs) -> Vec<Row> {
    let c = &p.controller;
    let secs = VINYL_SECONDS.map(|s| format!("{s} s"));
    let steps: Vec<String> = secs
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{} ({s})", i + 1))
        .collect();
    let db: Vec<String> = AUTO_CUE_DB.iter().map(|d| format!("{d} dB")).collect();
    let beats: Vec<String> = LOOP_BEATS.iter().map(|b| format!("{b} beats")).collect();
    let at = |list: &[i32], v: i32| list.iter().position(|&x| x == v).unwrap_or(3);
    vec![
        Row::header("DECK"),
        Row::check(
            "eject_lock",
            "Eject/Load Lock: no LOAD while a track is playing",
            c.eject_lock,
        ),
        Row::check(
            "needle_lock",
            "Needle Lock: no needle search while a track is playing",
            c.needle_lock,
        ),
        Row::check(
            "memory_cue_lock",
            "Memory Cue Call Lock: no memory cue call while a track is playing",
            c.memory_cue_lock,
        ),
        Row::check(
            "load_nearest_cue",
            "Load Setting: playback starts at the Memory/Hot Cue nearest to the beginning",
            c.load_nearest_cue,
        ),
        Row::check(
            "auto_cue",
            "Auto Cue: the cue point is the first sound above the level",
            c.auto_cue,
        ),
        Row::dropdown_of(
            "auto_cue_level",
            "AUTO CUE LEVEL",
            db,
            at(&AUTO_CUE_DB, c.auto_cue_level),
        )
        .enabled(c.auto_cue),
        Row::dropdown_of(
            "vinyl_brake",
            "VINYL SPEED: TOUCH/BRAKE",
            steps.clone(),
            (c.vinyl_brake.clamp(1, 5) - 1) as usize,
        )
        .hint("time to stop"),
        Row::dropdown_of(
            "vinyl_start",
            "VINYL SPEED: RELEASE/START",
            steps,
            (c.vinyl_start.clamp(1, 5) - 1) as usize,
        )
        .hint("time to full speed"),
        Row::of("jog_speed", "JOG ROTATION SPEED", c.jog_speed),
        Row::check(
            "jog_cue_return",
            "After spinning the jog on CUE, return to the cue point and pause",
            c.jog_cue_return,
        ),
        Row::check(
            "continuous_play",
            "Continuous Playing: load the next track of the list when one ends",
            c.continuous_play,
        ),
        Row::of("sync_type", "SYNC TYPE", c.sync_type),
        Row::check(
            "sync_half_double",
            "Allow BEAT/BPM SYNC with double/half BPM",
            c.sync_half_double,
        ),
        Row::check(
            "hot_cue_gate",
            "HOT CUE: during pause, GATE playback is applied",
            c.hot_cue_gate,
        ),
        Row::dropdown_of(
            "auto_loop_beats",
            "AUTO BEAT LOOP",
            beats,
            LOOP_BEATS
                .iter()
                .position(|&b| b == c.loop_beats())
                .unwrap_or(2),
        )
        .hint("length"),
        Row::header("START-UP (OpenFadeR)"),
        Row::chips(
            "range",
            "TEMPO RANGE",
            &["±6%", "±10%", "±16%", "WIDE"],
            p.tempo_range_default,
        ),
        Row::chips(
            "quantize",
            "QUANTIZE",
            &["ON", "OFF"],
            !p.quantize_default as usize,
        ),
        Row::chips(
            "keylock",
            "KEY LOCK",
            &["ON", "OFF"],
            !p.key_lock_default as usize,
        ),
    ]
}

/// The start-up rows live in the base fields of [`Prefs`].
pub(super) fn choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    ["range", "quantize", "keylock"].contains(&key) && p.choose(key, i)
}
