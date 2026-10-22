//! Library preferences (Preferences > Advanced > Database / Browse): the rows for the two pages, and the typed reads the
//! rest of the app uses. All values live in [`Prefs`] extras under `lib_…` keys.
//!
//! | key | item | effect here |
//! | --- | --- | --- |
//! | `lib_search_user`, `lib_search` | Auto Relocate Search Folders, "User specified folders" | folders Auto Relocate searches |
//! | `lib_db` | Database management | a library connected at start-up when no path is given ([`crate::app::connect`]) |
//! | `lib_mytag_comment` | Add Tags to the Comments | assigning tags rewrites the comment |
//! | `lib_dropdown` | Drop-down menu | holding a track for 1 s opens its menu |
//! | `lib_taglist_load`, `lib_taglist_quit` | Tag List | removal on load / on quit |
//! | `lib_protect`, `lib_edit` | Library Protection / Edit Library | no accidental deletion / rename by double click |
//!
//! The settings of the library formats (the library file of a Media Browser source, path maps, what an export writes) belong
//! to the library plugins: Preferences > Extensions.

use super::super::prefs::{self, Prefs, Row};
use std::path::PathBuf;

/// Folder list separator inside the one-line settings value.
const SEP: char = ';';

/// Everything the library code reads (see the module table).
#[derive(Clone, Debug, PartialEq)]
pub struct Lib {
    pub search_user: bool,
    pub search: Vec<PathBuf>,
    pub db: String,
    pub mytag_comment: bool,
    pub dropdown: bool,
    pub taglist_load: bool,
    pub taglist_quit: bool,
    pub protect: bool,
    pub edit: bool,
}

impl Lib {
    pub fn of(p: &Prefs) -> Lib {
        let text = |k: &str| p.extra(k).unwrap_or("").to_owned();
        Lib {
            search_user: p.extra_bool("lib_search_user", true),
            search: text("lib_search")
                .split(SEP)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
            db: text("lib_db"),
            mytag_comment: p.extra_bool("lib_mytag_comment", false),
            dropdown: p.extra_bool("lib_dropdown", false),
            taglist_load: p.extra_bool("lib_taglist_load", false),
            taglist_quit: p.extra_bool("lib_taglist_quit", false),
            protect: p.extra_bool("lib_protect", false),
            edit: p.extra_bool("lib_edit", true),
        }
    }
}

/// The current settings.
pub fn get() -> Lib {
    prefs::with(Lib::of)
}

/// The user's Music folder: `XDG_MUSIC_DIR`, else `~/Music`.
pub fn music_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_MUSIC_DIR")
        .map(PathBuf::from)
        .or_else(|| openfader::paths::home().map(|h| h.join("Music")))
}

/// Folders Auto Relocate searches: the user's list when "User specified folders" is on, else the Music folder.
pub fn search_folders(l: &Lib) -> Vec<PathBuf> {
    if l.search_user && !l.search.is_empty() {
        return l.search.clone();
    }
    music_dir().into_iter().collect()
}

/// The `library database` chosen in Database management (an existing file), for start-up without a path argument.
pub fn database_path() -> Option<PathBuf> {
    let p = PathBuf::from(Lib::of(&prefs::load()).db);
    p.is_file().then_some(p)
}

/// Registers the text rows' handlers (call once, UI thread).
pub fn wire() {
    for key in ["lib_search", "lib_db"] {
        prefs::on_text(key, move |v| prefs::update(|p| p.set_extra(key, v.trim())));
    }
}

pub fn database_rows(p: &Prefs) -> Vec<Row> {
    let l = Lib::of(p);
    vec![
        Row::header(tr!("AUTO RELOCATE SEARCH FOLDERS")),
        Row::check(
            "lib_search_user",
            tr!("User specified folders"),
            l.search_user,
        ),
        Row::text(
            "lib_search",
            tr!("FOLDERS"),
            &l.search
                .iter()
                .map(|f| f.display().to_string())
                .collect::<Vec<_>>()
                .join(&format!("{SEP} ")),
        )
        .hint(tr!("separate with ;"))
        .enabled(l.search_user),
        Row::header(tr!("DATABASE MANAGEMENT")),
        Row::text("lib_db", tr!("LIBRARY DATABASE"), &l.db)
            .hint(tr!("opened at start-up when no path is given")),
    ]
}

pub fn database_choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    check(p, key, i, &["lib_search_user"])
}

pub fn browse_rows(p: &Prefs) -> Vec<Row> {
    let l = Lib::of(p);
    vec![
        Row::header(tr!("TAGS")),
        Row::check(
            "lib_mytag_comment",
            tr!("Add Tags to the Comments"),
            l.mytag_comment,
        )
        .hint(tr!("writes /*Tag Tag*/ into the comment")),
        Row::header(tr!("TRACK LIST")),
        Row::check("lib_dropdown", tr!("Drop-down menu"), l.dropdown)
            .hint(tr!("hold a track for 1 s")),
        Row::header(tr!("TAG LIST")),
        Row::check(
            "lib_taglist_load",
            tr!("Delete the track from the Tag List when loading it to a deck"),
            l.taglist_load,
        ),
        Row::check(
            "lib_taglist_quit",
            tr!("Delete all tracks from the Tag List when quitting"),
            l.taglist_quit,
        ),
        Row::header(tr!("LIBRARY")),
        Row::check("lib_protect", tr!("Library Protection"), l.protect)
            .hint(tr!("no accidental edit / delete")),
        Row::check("lib_edit", tr!("Edit Library"), l.edit).hint(tr!("rename by double click")),
    ]
}

pub fn browse_choose(p: &mut Prefs, key: &str, i: usize) -> bool {
    check(
        p,
        key,
        i,
        &[
            "lib_mytag_comment",
            "lib_dropdown",
            "lib_taglist_load",
            "lib_taglist_quit",
            "lib_protect",
            "lib_edit",
        ],
    )
}

fn check(p: &mut Prefs, key: &str, i: usize, keys: &[&str]) -> bool {
    let mine = keys.contains(&key);
    if mine {
        p.set_extra(key, i == 1);
    }
    mine
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_search_folders_follow_the_checkbox() {
        let mut p = Prefs::default();
        let l = Lib::of(&p);
        assert!(l.search_user && l.edit && !l.protect && !l.dropdown && l.search.is_empty());
        p.set_extra("lib_search", " /a/b ; ;/c ");
        p.set_extra("lib_protect", true);
        let l = Lib::of(&p);
        assert_eq!(l.search, [PathBuf::from("/a/b"), PathBuf::from("/c")]);
        assert_eq!(search_folders(&l), l.search);
        p.set_extra("lib_search_user", false);
        assert_ne!(search_folders(&Lib::of(&p)), l.search); // the Music folder instead
        assert!(l.protect);
    }

    #[test]
    fn checks_write_through_and_foreign_keys_are_left_alone() {
        let mut p = Prefs::default();
        assert!(browse_choose(&mut p, "lib_protect", 1) && Lib::of(&p).protect);
        assert!(browse_choose(&mut p, "lib_protect", 0) && !Lib::of(&p).protect);
        assert!(!browse_choose(&mut p, "lib_search_user", 0)); // a Database key is not a Browse key
        assert!(database_choose(&mut p, "lib_search_user", 0) && !Lib::of(&p).search_user);
        assert!(!database_choose(&mut p, "nope", 1));
        assert!(
            browse_rows(&p).iter().any(|r| r.key == "lib_edit")
                && database_rows(&p).iter().any(|r| r.key == "lib_db")
        );
    }
}
