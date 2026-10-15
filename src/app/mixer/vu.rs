//! VU meter ballistics on the player's peak level: instant attack, 20 dB/s release, and a peak
//! marker that holds for 1 s, then falls at the same rate. Display range −60..0 dB.

const FLOOR_DB: f32 = -60.0;
const FALL_DB_PER_S: f32 = 20.0;
const HOLD_S: f32 = 1.0;

pub struct Vu {
    db: f32,
    peak_db: f32,
    hold_s: f32,
}

impl Vu {
    pub fn new() -> Self {
        Vu { db: FLOOR_DB, peak_db: FLOOR_DB, hold_s: 0.0 }
    }

    /// Feed the latest linear peak (0..1) and the seconds since the previous call.
    pub fn update(&mut self, level: f32, dt: f32) {
        let input = if level > 0.0 { (20.0 * level.log10()).max(FLOOR_DB) } else { FLOOR_DB };
        self.db = input.max(self.db - FALL_DB_PER_S * dt);
        if input >= self.peak_db {
            self.peak_db = input;
            self.hold_s = HOLD_S;
        } else {
            let held = self.hold_s.min(dt);
            self.hold_s -= held;
            self.peak_db = (self.peak_db - FALL_DB_PER_S * (dt - held)).max(self.db);
        }
    }

    /// Bar height 0..1.
    pub fn level(&self) -> f32 {
        pos(self.db)
    }

    /// Peak marker height 0..1.
    pub fn peak(&self) -> f32 {
        pos(self.peak_db)
    }
}

fn pos(db: f32) -> f32 {
    (1.0 - db / FLOOR_DB).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attacks_instantly_and_decays_20_db_per_s() {
        let mut v = Vu::new();
        v.update(1.0, 0.03); // 0 dBFS
        assert_eq!(v.level(), 1.0);
        for _ in 0..50 {
            v.update(0.0, 0.01); // silence for 0.5 s → −10 dB
        }
        assert!((v.level() - (1.0 - 10.0 / 60.0)).abs() < 1e-3, "{}", v.level());
    }

    #[test]
    fn peak_holds_one_second_then_falls() {
        let mut v = Vu::new();
        v.update(0.5, 0.01); // −6.02 dB
        let top = v.peak();
        for _ in 0..99 {
            v.update(0.0, 0.01);
        }
        assert_eq!(v.peak(), top, "still held at 0.99 s");
        for _ in 0..51 {
            v.update(0.0, 0.01); // 0.5 s past the hold → −10 dB
        }
        assert!((top - v.peak() - 10.0 / 60.0).abs() < 2e-3, "{} {}", top, v.peak());
        assert!(v.peak() >= v.level());
    }

    #[test]
    fn a_new_louder_peak_restarts_the_hold() {
        let mut v = Vu::new();
        v.update(0.1, 0.01);
        for _ in 0..80 {
            v.update(0.0, 0.01);
        }
        v.update(0.4, 0.01);
        let top = v.peak();
        for _ in 0..90 {
            v.update(0.0, 0.01);
        }
        assert_eq!(v.peak(), top);
    }

    #[test]
    fn silence_sits_at_the_floor() {
        let mut v = Vu::new();
        v.update(0.0, 1.0);
        assert_eq!((v.level(), v.peak()), (0.0, 0.0));
        v.update(f32::NAN, 1.0); // a bad reading must not poison the meter
        assert_eq!(v.level(), 0.0);
    }
}
