//! What a drag carries between the track table, the tree and the palettes: plain text built by the pure `drag-*` callbacks of
//! `BrowserState` (Slint cannot read a `data-transfer`) and read back by `palette-drop`.

use slint::SharedString;

#[derive(Debug, PartialEq)]
pub(super) enum Drag {
    /// A row of the track table.
    Track(usize),
    /// A row of the sidebar tree.
    Tree(usize),
    /// (palette, row) of a track in a palette.
    Palette(usize, usize),
}

impl Drag {
    pub fn parse(s: &str) -> Option<Drag> {
        let mut it = s.split(':');
        let n = |it: &mut std::str::Split<char>| it.next()?.parse().ok();
        let d = match it.next()? {
            "track" => Drag::Track(n(&mut it)?),
            "tree" => Drag::Tree(n(&mut it)?),
            "list" => Drag::Palette(n(&mut it)?, n(&mut it)?),
            _ => return None,
        };
        it.next().is_none().then_some(d)
    }

    pub fn text(&self) -> SharedString {
        match self {
            Drag::Track(r) => format!("track:{r}"),
            Drag::Tree(r) => format!("tree:{r}"),
            Drag::Palette(p, r) => format!("list:{p}:{r}"),
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drags_round_trip_through_their_text() {
        for d in [Drag::Track(4), Drag::Tree(12), Drag::Palette(2, 0)] {
            assert_eq!(Drag::parse(&d.text()), Some(d));
        }
        for bad in [
            "",
            "track",
            "track:x",
            "track:1:2",
            "list:1",
            "file:///a",
            "tree:-1",
        ] {
            assert_eq!(Drag::parse(bad), None, "{bad}");
        }
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::app::browser::select::Selection;

    /// A row drag is still just `track:<row>`: the selection it carries is resolved on drop ([`Selection::carried`]), so a
    /// drag text never goes stale when the selection changes before the drop.
    #[test]
    fn a_row_drag_names_only_its_row_and_the_drop_reads_the_selection() {
        let shown = [2, 0, 1];
        let mut s = Selection::default();
        s.pick(0);
        s.toggle(1);
        let Some(Drag::Track(row)) = Drag::parse(&Drag::Track(2).text()) else {
            panic!("not a track drag");
        };
        assert_eq!(s.carried(&shown, row), [0, 1]); // row 2 is list row 1, one of the two selected
        s.pick(2);
        assert_eq!(s.carried(&shown, row), [1]); // the selection changed before the drop: it goes alone
        assert_eq!(Drag::parse("track:99999999999999999999999"), None);
    }
}
