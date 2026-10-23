//! Output routing: which device channels carry what. Pure (no cpal), so the maths is unit-tested.
//!
//! Preferences > Audio > Input/Output. **Internal** mixer mode = our mixer (the bus): Master / Headphones /
//! Booth each go to a stereo channel pair of the device. **External** mixer mode = no mixing here: Decks 1-4, the
//! Sampler and the Preview each go, unprocessed (no EQ, filter, effects, fader, crossfader or master gain), to
//! their own pair. Pairs are numbered from 1: pair 1 = device channels 1/2, pair 2 = 3/4, ... `0` = not routed.
//!
//! Booth: a copy of the stereo master at its own level.

use super::mono;

/// External outputs: Decks 1-4, Sampler, Preview, in this order.
pub const EXTERNAL: usize = 6;
pub const EXTERNAL_NAMES: [&str; EXTERNAL] =
    ["DECK 1", "DECK 2", "DECK 3", "DECK 4", "SAMPLER", "PREVIEW"];

/// Where the mixing happens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mixer {
    #[default]
    Internal,
    External,
}

/// What the user asked for (Preferences > Audio). The first block needs the stream reopened when it changes, the
/// second is read by the audio thread through [`super::Bus::apply`].
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Output device name; empty = the system default.
    pub device: String,
    /// Sample rate in Hz, 0 = the device's default.
    pub rate: u32,
    /// Buffer size in frames, 0 = the driver's default.
    pub buffer: u32,
    pub mixer: Mixer,
    /// Internal mode: pair numbers (see the module doc). `phones` 0 = automatic (`OPENFADER_HP_CHANNELS`, a
    /// controller interface with four outputs, else SPLIT on the master pair); `booth` 0 = off.
    pub master: u8,
    pub phones: u8,
    pub booth: u8,
    /// External mode: the pair of each output ([`EXTERNAL_NAMES`] order), 0 = not routed.
    pub external: [u8; EXTERNAL],
    /// Linear booth level.
    pub booth_gain: f32,
    pub metronome_tone: usize,
    pub metronome_volume: f32,
    pub preview_gain: f32,
    /// Microphone: linear level (0 = off), Advanced (mid-range only) talkover, linear gain of the music while talking (1 = no talkover).
    pub mic_gain: f32,
    pub talkover_advanced: bool,
    pub talkover_gain: f32,
    /// AUX channel ducking: linear gain of the decks while the AUX is heard (1 = no ducking) and the seconds they take to come back.
    pub aux_duck: f32,
    pub aux_release: f32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            device: String::new(),
            rate: 0,
            buffer: 0,
            mixer: Mixer::Internal,
            master: 1,
            phones: 0,
            booth: 0,
            external: [1, 2, 3, 4, 5, 6],
            booth_gain: 1.0,
            metronome_tone: 0,
            metronome_volume: 0.5,
            preview_gain: 1.0,
            mic_gain: 0.0,
            talkover_advanced: true,
            talkover_gain: 1.0,
            aux_duck: 1.0,
            aux_release: 0.4,
        }
    }
}

impl Settings {
    /// Whether `o` asks for the same stream (device, rate, buffer, channel layout); the knobs do not matter.
    pub fn same_stream(&self, o: &Settings) -> bool {
        (
            &self.device,
            self.rate,
            self.buffer,
            self.mixer,
            self.master,
            self.phones,
            self.booth,
            self.external,
        ) == (
            &o.device, o.rate, o.buffer, o.mixer, o.master, o.phones, o.booth, o.external,
        )
    }
}

/// Zero-based device channels of pair `n` (1-based); `None` for 0.
pub fn pair(n: u8) -> Option<[usize; 2]> {
    (n > 0).then(|| {
        let a = 2 * (n as usize - 1);
        [a, a + 1]
    })
}

/// Pairs a device with `channels` channels offers (a mono device offers one: the mean of the stereo signal).
pub fn pairs(channels: usize) -> usize {
    (channels / 2).max(1)
}

/// "1/2", "3/4", ... for pair `n`.
pub fn pair_name(n: u8) -> String {
    let [a, b] = pair(n).unwrap_or([0, 1]);
    format!("{}/{}", a + 1, b + 1)
}

fn fits(p: [usize; 2], channels: usize) -> bool {
    p[1] < channels || (channels == 1 && p == [0, 1])
}

/// The resolved layout of one stream: device channel pairs, decided when the stream opens.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub external: bool,
    pub master: [usize; 2],
    pub phones: Option<[usize; 2]>,
    pub booth: Option<[usize; 2]>,
    /// External mode: where each output goes ([`EXTERNAL_NAMES`] order).
    pub sources: [Option<[usize; 2]>; EXTERNAL],
}

impl Plan {
    /// Master on channels 1/2, nothing else (the fallback when the asked layout cannot be had).
    pub fn stereo() -> Plan {
        Plan {
            external: false,
            master: [0, 1],
            phones: None,
            booth: None,
            sources: [None; EXTERNAL],
        }
    }

    /// The layout `s` asks for on a device with `channels` channels. `phones` = the headphone pair already decided
    /// (settings, environment or controller). Internal mode with a pair the device lacks, or two roles on one pair,
    /// is an error naming the problem; External drops the outputs the device lacks and says so in the notes.
    pub fn new(
        s: &Settings,
        phones: Option<[usize; 2]>,
        channels: usize,
    ) -> Result<(Plan, Vec<String>), String> {
        if s.mixer == Mixer::External {
            let mut notes = Vec::new();
            let sources = std::array::from_fn(|k| {
                let p = pair(s.external[k]).filter(|&p| fits(p, channels));
                if s.external[k] > 0 && p.is_none() {
                    notes.push(format!(
                        "{} output: pair {} is not on this device ({channels} channels)",
                        EXTERNAL_NAMES[k],
                        pair_name(s.external[k])
                    ));
                }
                p
            });
            return Ok((
                Plan {
                    external: true,
                    master: [0, 1],
                    phones: None,
                    booth: None,
                    sources,
                },
                notes,
            ));
        }
        let master = pair(s.master.max(1)).unwrap_or([0, 1]);
        let booth = pair(s.booth);
        for (role, p) in [
            ("Master", Some(master)),
            ("Headphones", phones),
            ("Booth", booth),
        ] {
            if let Some(p) = p.filter(|&p| !fits(p, channels)) {
                return Err(format!(
                    "{role} output: channels {}/{} are not on this device ({channels} channels)",
                    p[0] + 1,
                    p[1] + 1
                ));
            }
        }
        for (a, b, names) in [
            (Some(master), phones, "Master and Headphones"),
            (Some(master), booth, "Master and Booth"),
            (phones, booth, "Headphones and Booth"),
        ] {
            if a.is_some() && a == b {
                return Err(format!("{names} are on the same output pair"));
            }
        }
        Ok((
            Plan {
                external: false,
                master,
                phones,
                booth,
                sources: [None; EXTERNAL],
            },
            Vec::new(),
        ))
    }

    /// Device channels the stream must have (even, at least 2).
    pub fn channels(&self) -> usize {
        let used = [
            Some(self.master).filter(|_| !self.external),
            self.phones,
            self.booth,
        ]
        .into_iter()
        .chain(self.sources)
        .flatten()
        .map(|p| p[1] + 1)
        .max()
        .unwrap_or(2);
        (used + used % 2).max(2)
    }
}

/// Adds the stereo `[l, r]` to the device-frame channels `pair`; a mono frame gets the mean, channels the frame lacks are skipped.
fn add_pair(frame: &mut [f32], pair: [usize; 2], [l, r]: [f32; 2]) {
    if frame.len() == 1 {
        frame[0] += mono([l, r]);
        return;
    }
    for (c, v) in pair.into_iter().zip([l, r]) {
        if let Some(o) = frame.get_mut(c) {
            *o += v;
        }
    }
}

/// One Internal-mode device frame: `device` (the master, or the SPLIT feed) on the master pair, the headphone feed
/// `phones` on its pair, `master` x `booth` on the booth pair; every other channel silent.
pub fn place(
    frame: &mut [f32],
    plan: &Plan,
    device: [f32; 2],
    master: [f32; 2],
    phones: [f32; 2],
    booth: f32,
) {
    frame.fill(0.0);
    add_pair(frame, plan.master, device);
    if let Some(p) = plan.phones {
        add_pair(frame, p, phones);
    }
    if let Some(p) = plan.booth {
        add_pair(frame, p, master.map(|x| x * booth));
    }
}

/// One External-mode device frame: each output's unprocessed stereo signal on its own pair.
pub fn place_external(frame: &mut [f32], plan: &Plan, outs: &[[f32; 2]; EXTERNAL]) {
    frame.fill(0.0);
    for (p, o) in plan.sources.iter().zip(outs) {
        if let Some(p) = p {
            add_pair(frame, *p, *o);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(f: impl FnOnce(&mut Settings)) -> Settings {
        let mut s = Settings::default();
        f(&mut s);
        s
    }

    #[test]
    fn pairs_are_numbered_from_one_over_adjacent_channels() {
        assert_eq!(
            (pair(0), pair(1), pair(2), pair(6)),
            (None, Some([0, 1]), Some([2, 3]), Some([10, 11]))
        );
        assert_eq!((pairs(1), pairs(2), pairs(5), pairs(8)), (1, 1, 2, 4));
        assert_eq!(pair_name(3), "5/6");
    }

    #[test]
    fn internal_plan_needs_the_channels_of_its_pairs_and_rejects_clashes() {
        let (p, notes) = Plan::new(&s(|_| {}), None, 2).unwrap();
        assert_eq!(
            (p.master, p.phones, p.booth, p.channels(), notes.len()),
            ([0, 1], None, None, 2, 0)
        );
        let (p, _) = Plan::new(&s(|s| s.booth = 3), Some([2, 3]), 8).unwrap();
        assert_eq!(
            (p.phones, p.booth, p.channels()),
            (Some([2, 3]), Some([4, 5]), 6)
        );
        assert!(
            Plan::new(&s(|_| {}), Some([2, 3]), 2)
                .unwrap_err()
                .contains("Headphones")
        );
        assert!(
            Plan::new(&s(|_| {}), Some([0, 1]), 4)
                .unwrap_err()
                .contains("same")
        );
        assert!(
            Plan::new(&s(|s| s.booth = 1), None, 4)
                .unwrap_err()
                .contains("Master and Booth")
        );
        assert!(Plan::new(&s(|s| s.master = 2), None, 2).is_err());
        assert!(
            Plan::new(&s(|_| {}), None, 1).is_ok(),
            "a mono device carries the mean"
        );
    }

    #[test]
    fn external_plan_drops_outputs_the_device_lacks() {
        let ext = s(|s| s.mixer = Mixer::External);
        let (p, notes) = Plan::new(&ext, None, 8).unwrap();
        assert_eq!(
            p.sources,
            [
                Some([0, 1]),
                Some([2, 3]),
                Some([4, 5]),
                Some([6, 7]),
                None,
                None
            ]
        );
        assert_eq!(notes.len(), 2);
        assert!(notes[0].contains("SAMPLER"));
        assert_eq!(p.channels(), 8);
        let (p, _) = Plan::new(
            &s(|s| (s.mixer, s.external) = (Mixer::External, [0; EXTERNAL])),
            None,
            2,
        )
        .unwrap();
        assert_eq!(p.channels(), 2);
    }

    #[test]
    fn internal_frame_puts_master_phones_and_booth_on_their_pairs() {
        let plan = Plan {
            external: false,
            master: [0, 1],
            phones: Some([2, 3]),
            booth: Some([4, 5]),
            sources: [None; EXTERNAL],
        };
        let mut f = [9.0f32; 7]; // stale data and an odd channel count: the spare channel is silenced
        place(&mut f, &plan, [0.5, 0.25], [0.5, 0.25], [0.1, 0.2], 0.5);
        assert_eq!(f, [0.5, 0.25, 0.1, 0.2, 0.25, 0.125, 0.0]);
        let split = Plan {
            phones: None,
            booth: None,
            ..plan.clone()
        };
        place(&mut f, &split, [0.3, 0.4], [1.0, 1.0], [1.0, 1.0], 1.0);
        assert_eq!(f[..4], [0.3, 0.4, 0.0, 0.0]);
        let swapped = Plan {
            master: [3, 2],
            phones: None,
            booth: None,
            ..plan
        };
        place(&mut f[..4], &swapped, [0.3, 0.4], [0.0; 2], [0.0; 2], 1.0);
        assert_eq!(f[..4], [0.0, 0.0, 0.4, 0.3]);
    }

    #[test]
    fn mono_device_gets_the_mean_and_missing_channels_are_skipped() {
        let mut f = [0.0f32; 1];
        place(
            &mut f,
            &Plan::stereo(),
            [0.5, 0.25],
            [0.0; 2],
            [0.0; 2],
            1.0,
        );
        assert_eq!(f, [0.375]);
        let mut f = [0.0f32; 2];
        add_pair(&mut f, [2, 3], [1.0, 1.0]);
        assert_eq!(f, [0.0, 0.0]);
    }

    #[test]
    fn external_frame_keeps_each_output_on_its_pair_untouched() {
        let (plan, _) = Plan::new(
            &s(|s| (s.mixer, s.external) = (Mixer::External, [2, 1, 0, 4, 0, 3])),
            None,
            8,
        )
        .unwrap();
        let outs = [
            [0.1, 0.2],
            [0.3, 0.4],
            [0.5, 0.5],
            [0.6, 0.7],
            [0.8, 0.8],
            [0.9, 1.0],
        ];
        let mut f = [7.0f32; 8];
        place_external(&mut f, &plan, &outs);
        assert_eq!(f, [0.3, 0.4, 0.1, 0.2, 0.9, 1.0, 0.6, 0.7]); // deck 3 and sampler not routed
    }

    #[test]
    fn stream_equality_ignores_the_knobs() {
        let a = Settings::default();
        assert!(a.same_stream(&s(
            |s| (s.booth_gain, s.preview_gain, s.metronome_tone) = (0.1, 0.2, 2)
        )));
        assert!(!a.same_stream(&s(|s| s.rate = 96_000)));
        assert!(!a.same_stream(&s(|s| s.external[5] = 0)));
    }
}
