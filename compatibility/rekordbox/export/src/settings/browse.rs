//! The browse lists of a device: which categories and sort options the player shows, in which order, and the
//! eight colour comments. Pure data, no dependencies.
//!
//! Source of every table: the real rekordbox 7 export (observed), where `export.pdb` tables 16/17/18 and OneLibrary
//! `menuItem` / `category` / `sort` hold the same lists (see `.local/re/export/notes.md`):
//! - table 16 `columns` = `menuItem`: `(id, field, label)`, 27 rows;
//! - table 17 row = `[menuItem, category id, word, sequence]`; `sequence` 0 = inactive, `1..` = position in the active
//!   list (OneLibrary `category.sequenceNo` / `isVisible = sequence != 0`). `word` is constant per category (observed; for
//!   some it equals a sort id, 99 = none: inferred "default sort", unobserved semantics) and is carried over unchanged;
//! - table 18 row = `[menuItem, sort id, word, 0]`; `word = 1` = inactive, `position << 8` (1-based) = active
//!   (OneLibrary `sort.sequenceNo = word >> 8`, `isVisible = word != 1`). The fourth word is 0 in every observed row;
//!   OneLibrary `sort.isSelectedAsSubColumn` (inferred to be rekordbox's "Column" setting: the sort item shown right of
//!   the title) is the only place we write the Column choice.
//!
//! The observed rows list the inactive ones first, then the active ones in order: [`table17`] / [`table18`] emit the
//! same layout, so the default lists reproduce the observed tables byte for byte.

/// `(id, field, label)` of the 27 browse columns = OneLibrary `menuItem` (observed, identical in every rekordbox export).
pub const COLUMNS: [(u16, u16, &str); 27] = [
    (1, 128, "GENRE"),
    (2, 129, "ARTIST"),
    (3, 130, "ALBUM"),
    (4, 131, "TRACK"),
    (5, 133, "BPM"),
    (6, 134, "RATING"),
    (7, 135, "YEAR"),
    (8, 136, "REMIXER"),
    (9, 137, "LABEL"),
    (10, 138, "ORIGINAL ARTIST"),
    (11, 139, "KEY"),
    (12, 141, "CUE"),
    (13, 142, "COLOR"),
    (14, 146, "TIME"),
    (15, 147, "BITRATE"),
    (16, 148, "FILE NAME"),
    (17, 132, "PLAYLIST"),
    (18, 152, "HOT CUE BANK"),
    (19, 149, "HISTORY"),
    (20, 145, "SEARCH"),
    (21, 150, "COMMENTS"),
    (22, 140, "DATE ADDED"),
    (23, 151, "DJ PLAY COUNT"),
    (24, 144, "FOLDER"),
    (25, 161, "DEFAULT"),
    (26, 162, "ALPHABET"),
    (27, 170, "MATCHING"),
];

/// `(menuItem, category id, word)` in the observed table order (inactive ones of the default export first).
pub const CATEGORIES: [(u16, u16, u16); 22] = [
    (1, 1, 355),
    (5, 6, 261),
    (6, 7, 355),
    (7, 8, 355),
    (8, 9, 355),
    (9, 10, 355),
    (10, 11, 355),
    (13, 15, 355),
    (14, 19, 260),
    (15, 20, 262),
    (16, 21, 355),
    (18, 23, 355),
    (2, 2, 2),
    (3, 3, 3),
    (4, 4, 1),
    (11, 12, 99),
    (17, 5, 99),
    (19, 22, 99),
    (20, 18, 99),
    (27, 26, 611),
    (24, 17, 99),
    (22, 27, 99),
];

/// Category ids active in the observed export, in display order.
pub(super) const DEFAULT_CATEGORIES: [u16; 10] = [2, 3, 4, 12, 5, 22, 18, 26, 17, 27];

/// `(menuItem, sort id)` in the observed table order.
pub const SORTS: [(u16, u16); 17] = [
    (1, 6),
    (21, 7),
    (14, 8),
    (8, 9),
    (9, 10),
    (10, 11),
    (15, 13),
    (13, 15),
    (23, 16),
    (22, 17),
    (25, 0),
    (26, 1),
    (2, 2),
    (3, 3),
    (5, 4),
    (6, 5),
    (11, 12),
];

/// Sort ids active in the observed export, in display order (DEFAULT, ALPHABET, ARTIST, ALBUM, BPM, RATING, KEY).
pub(super) const DEFAULT_SORTS: [u16; 7] = [0, 1, 2, 3, 4, 5, 12];

/// The menu item ids that are not a real column of a track (DEFAULT, ALPHABET): not offered as the "Column" choice.
const NO_COLUMN: [u16; 2] = [25, 26];

/// The eight colour comments of an export (observed names), ids 1..=8.
pub const COLORS: [&str; 8] = [
    "Pink", "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple",
];

/// Label of a menu item, empty for an unknown id.
pub fn label(menu: u16) -> &'static str {
    COLUMNS.iter().find(|c| c.0 == menu).map_or("", |c| c.2)
}

/// Menu item of a category id.
pub fn category_menu(id: u16) -> Option<u16> {
    CATEGORIES.iter().find(|c| c.1 == id).map(|c| c.0)
}

/// Menu item of a sort id.
pub fn sort_menu(id: u16) -> Option<u16> {
    SORTS.iter().find(|s| s.1 == id).map(|s| s.0)
}

/// Whether sort `id` can be chosen as the "Column" (a real track column).
pub fn is_column(id: u16) -> bool {
    sort_menu(id).is_some_and(|m| !NO_COLUMN.contains(&m))
}

/// Rows of table 17 / 18: the inactive rows of `all` in table order, then the active ones in `active` order. `id` is a row's
/// id; `row(r, n)` makes its four words, `n` being 0 for an inactive row and the 1-based position for an active one.
/// Unknown ids in `active` are skipped.
fn rows<T: Copy>(
    all: &[T],
    id: fn(&T) -> u16,
    active: &[u16],
    row: fn(T, u16) -> [u16; 4],
) -> Vec<[u16; 4]> {
    let inactive = all
        .iter()
        .filter(|r| !active.contains(&id(r)))
        .map(|&r| row(r, 0));
    let on = active
        .iter()
        .filter_map(|a| all.iter().find(|r| id(r) == *a));
    inactive
        .chain((1..).zip(on).map(|(n, &r)| row(r, n)))
        .collect()
}

/// Table 17 rows for the active category ids.
pub(super) fn table17(active: &[u16]) -> Vec<[u16; 4]> {
    rows(
        CATEGORIES.as_slice(),
        |c| c.1,
        active,
        |(m, id, w), n| [m, id, w, n],
    )
}

/// Table 18 rows for the active sort ids. The "Column" choice is not in the table (OneLibrary only, see the module doc).
pub(super) fn table18(active: &[u16]) -> Vec<[u16; 4]> {
    rows(
        SORTS.as_slice(),
        |s| s.1,
        active,
        |(m, id), n| [m, id, if n == 0 { 1 } else { n << 8 }, 0],
    )
}
