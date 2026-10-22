//! MIX POINT LINK tab: Enable, MIX IN/MIX OUT, Time scale / Grids display / Automatic BEAT/BPM SYNC
//! Setting / Zoom range. Consumed by `app/deck/mixpoint`.

use super::{Controller, Row};

pub(super) fn rows(c: &Controller) -> Vec<Row> {
    vec![
        Row::header(tr!("MIX POINT LINK")),
        Row::check(
            "mix_point_link",
            tr!("Enable MIX POINT LINK Function"),
            c.mix_point_link,
        ),
        Row::chips(
            "mix_point_default",
            tr!("DEFAULT MIX POINT SETTING"),
            &[tr!("MIX IN"), tr!("MIX OUT")],
            c.mix_point_default as usize,
        ),
        Row::check(
            "mix_point_auto_switch",
            tr!("Automatically switches between MIX IN/MIX OUT"),
            c.mix_point_auto_switch,
        ),
        Row::check("mix_point_time", tr!("Display time"), c.mix_point_time),
        Row::check("mix_point_grids", tr!("Display grids"), c.mix_point_grids),
        Row::check(
            "mix_point_grids_enlarged",
            tr!("Display only when waveform is enlarged"),
            c.mix_point_grids_enlarged,
        )
        .enabled(c.mix_point_grids),
        Row::check(
            "mix_point_auto_sync",
            tr!("Automatically changes the BEAT/BPM SYNC setting"),
            c.mix_point_auto_sync,
        ),
        Row::check(
            "mix_point_zoom_limit",
            tr!("Automatically limits the zoom out range"),
            c.mix_point_zoom_limit,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::rows;
    use crate::app::prefs::Prefs;

    #[test]
    fn only_when_enlarged_is_greyed_while_grids_are_off() {
        let enabled = |p: &Prefs| {
            rows(p)
                .iter()
                .find(|r| r.key == "mix_point_grids_enlarged")
                .map(|r| r.enabled)
        };
        let mut p = Prefs::default();
        assert_eq!(enabled(&p), Some(true));
        p.controller.mix_point_grids = false;
        assert_eq!(enabled(&p), Some(false));
    }
}
