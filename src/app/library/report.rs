//! The report that ends the import of a collection, and File > Library > Last Import Report… ↔ ui/import-report.slint (global
//! `ImportReportState`). The window lists the first [`LISTED`] skipped entries; SAVE AS TEXT writes all of them. The report is
//! kept in a file ([`import_report::file`]) so it can be opened again after a restart.

use super::{Ask, Ctx};
use crate::app::{ImportReportState, ImportSkipRow};
use openfader::library::{
    import::{Reason, Report},
    import_report::{self, Saved},
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, io::Write, path::PathBuf, rc::Rc};

/// Skipped entries the window lists (a collection can skip thousands; the rest is in the saved text).
const LISTED: usize = 500;

thread_local! {
    /// The report the window shows, for SAVE AS TEXT.
    static SHOWN: RefCell<Option<Saved>> = const { RefCell::new(None) };
}

fn reason(r: Reason) -> &'static str {
    match r {
        Reason::MissingFile => tr!("missing file"),
        Reason::UnsupportedFormat => tr!("unsupported format"),
        Reason::Duplicate => tr!("duplicate"),
    }
}

fn headline(s: &Saved) -> String {
    tr!("Read from {} on {}", s.source, s.when)
}

/// What came in and how many entries were skipped, per reason, one per line.
fn summary(s: &Saved) -> String {
    let mut lines = vec![
        tr_n!("{n} track imported", "{n} tracks imported", s.added),
        tr_n!(
            "{n} playlist or folder imported",
            "{n} playlists and folders imported",
            s.playlists
        ),
        tr_n!("{n} cue point imported", "{n} cue points imported", s.cues),
        tr_n!("{n} beat grid imported", "{n} beat grids imported", s.grids),
    ];
    if s.absent > 0 {
        lines.push(tr_n!(
            "{n} imported track has no file at its path now (see Display All Missing Files)",
            "{n} imported tracks have no file at their path now (see Display All Missing Files)",
            s.absent
        ));
    }
    lines.push(tr_n!(
        "{n} entry skipped",
        "{n} entries skipped",
        s.skipped.len()
    ));
    for r in [
        Reason::MissingFile,
        Reason::UnsupportedFormat,
        Reason::Duplicate,
    ] {
        let n = s.count(r);
        if n > 0 {
            lines.push(format!("    {}: {n}", reason(r)));
        }
    }
    lines.join("\n")
}

/// The first [`LISTED`] skipped entries as window rows, and how many more there are.
fn rows(s: &Saved) -> (Vec<ImportSkipRow>, usize) {
    let shown = s
        .skipped
        .iter()
        .take(LISTED)
        .map(|k| ImportSkipRow {
            name: k.name.as_str().into(),
            reason: reason(k.reason).into(),
        })
        .collect();
    (shown, s.skipped.len().saturating_sub(LISTED))
}

/// The whole report as text, every skipped entry included.
fn text(s: &Saved) -> String {
    let mut out = format!("{}\n\n{}\n", headline(s), summary(s));
    if !s.skipped.is_empty() {
        out += &format!("\n{}\n", tr!("Skipped entries"));
        for k in &s.skipped {
            out += &format!("{}\t{}\n", reason(k.reason), k.name);
        }
    }
    out + &format!("\n{}\n", tr!("The source library was not changed."))
}

fn open(c: &Ctx, s: Saved) {
    let app = c.app();
    let st = app.global::<ImportReportState>();
    let (shown, more) = rows(&s);
    st.set_headline(headline(&s).into());
    st.set_summary(summary(&s).into());
    st.set_rows(ModelRc::new(VecModel::from(shown)));
    st.set_more(if more > 0 {
        tr!("and {} more (SAVE AS TEXT has them all)", more).into()
    } else {
        "".into()
    });
    st.set_open(true);
    SHOWN.set(Some(s));
}

/// An import of the collection file `source` is done: keeps its report and shows it.
pub fn record(c: &Ctx, source: &str, r: &Report) {
    let s = Saved::of(source, r);
    let kept = import_report::file()
        .ok_or_else(|| "no data directory (set XDG_DATA_HOME or HOME)".to_owned())
        .and_then(|p| s.save_to(&p));
    match kept {
        Ok(()) => c.app().global::<ImportReportState>().set_has_report(true),
        Err(e) => c.say(&tr!("import report not kept: {}", e)),
    }
    open(c, s);
}

/// File > Library > Last Import Report…
fn show(c: &Ctx) {
    match import_report::file().and_then(|p| Saved::load_from(&p)) {
        Some(s) => open(c, s),
        None => c.say(tr!("No import yet")),
    }
}

/// SAVE AS TEXT, after the path dialog: writes the report on screen to `path`, never over an existing file.
pub fn save(c: &Ctx, path: PathBuf) {
    let Some(s) = SHOWN.with_borrow(Clone::clone) else {
        return c.say(tr!("No import yet"));
    };
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut f| f.write_all(text(&s).as_bytes()));
    match written {
        Ok(()) => c.say(&tr!("import report saved to {}", path.display())),
        Err(e) => c.say(&tr!("{}: {}", path.display(), e)),
    }
}

pub fn wire(c: &Rc<Ctx>) {
    let app = c.app();
    let st = app.global::<ImportReportState>();
    st.set_has_report(import_report::file().is_some_and(|p| p.is_file()));
    let k = c.clone();
    st.on_show(move || show(&k));
    let k = c.clone();
    st.on_save(move || k.ask_path(Ask::SaveImportReport, None));
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfader::library::import::Skipped;

    fn skipped(n: usize) -> Vec<Skipped> {
        (0..n)
            .map(|i| Skipped {
                name: format!("/m/{i}.mp3"),
                reason: [
                    Reason::MissingFile,
                    Reason::UnsupportedFormat,
                    Reason::Duplicate,
                ][i % 3],
            })
            .collect()
    }

    fn saved(skipped: Vec<Skipped>) -> Saved {
        Saved {
            when: "2026-10-24 12:00".into(),
            source: "/lib/collection.xml".into(),
            added: 1,
            playlists: 2,
            cues: 3,
            grids: 1,
            absent: 0,
            skipped,
        }
    }

    #[test]
    fn the_window_lists_the_first_500_and_says_how_many_more_while_the_text_has_all() {
        let s = saved(skipped(1201));
        let (shown, more) = rows(&s);
        assert_eq!((shown.len(), more), (500, 701));
        assert_eq!(
            (shown[0].name.as_str(), shown[0].reason.as_str()),
            ("/m/0.mp3", "missing file")
        );
        assert_eq!(shown[4].reason.as_str(), "unsupported format");
        assert_eq!(
            text(&s)
                .lines()
                .filter(|l| l.starts_with("duplicate\t"))
                .count(),
            400
        );
        let (shown, more) = rows(&saved(skipped(500)));
        assert_eq!((shown.len(), more), (500, 0)); // exactly the cap: nothing to add
    }

    #[test]
    fn the_summary_counts_what_came_in_and_the_skips_by_reason() {
        let mut s = saved(skipped(7)); // 3 missing, 2 unsupported, 2 duplicates
        assert_eq!(
            summary(&s),
            "1 track imported\n2 playlists and folders imported\n3 cue points imported\n1 beat grid imported\n7 entries skipped\n    missing file: 3\n    unsupported format: 2\n    duplicate: 2"
        );
        s.skipped.clear();
        s.absent = 2;
        let text = summary(&s);
        assert!(text.contains("2 imported tracks have no file at their path now"));
        assert!(text.ends_with("0 entries skipped"));
        assert!(!text.contains("missing file"));
    }

    #[test]
    fn the_end_of_an_import_reports_what_the_fake_library_gave_and_survives_a_restart() {
        use openfader::{
            library::{import, Library, Playlist, Track},
            store::Store,
        };
        struct Fake(Vec<Track>);
        impl Library for Fake {
            fn kind(&self) -> &str {
                "fake"
            }
            fn playlists(&self) -> Result<Vec<Playlist>, String> {
                Ok(Vec::new())
            }
            fn tracks(&self, _: Option<&str>) -> Result<Vec<Track>, String> {
                Ok(self.0.clone())
            }
        }
        let dir = std::env::temp_dir().join(format!("openfader-report-end-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (good, text_file) = (dir.join("a.mp3"), dir.join("notes.txt"));
        let t = |f: &std::path::Path| Track {
            title: "t".into(),
            file: Some(f.to_path_buf()),
            ..Track::default()
        };
        let src = Fake(vec![t(&good), t(&text_file), t(&good), Track::default()]);
        let store = Store::in_memory().unwrap();
        let scope = import::Scope::All { playlists: false };
        let r = import::library(&store, &src, &Default::default(), scope, &|_, _| {}).unwrap();
        let kept = dir.join("last-import-report.json");
        Saved::of("fake.xml", &r).save_to(&kept).unwrap();
        let s = Saved::load_from(&kept).unwrap();
        assert_eq!((s.added, s.skipped.len()), (1, 3));
        let (shown, more) = rows(&s);
        assert_eq!(more, 0);
        assert_eq!(
            shown.iter().map(|r| r.reason.as_str()).collect::<Vec<_>>(),
            ["unsupported format", "duplicate", "missing file"]
        ); // source order
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
