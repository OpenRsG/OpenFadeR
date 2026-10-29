//! rekordbox smart playlists: the `djmdPlaylist.SmartList` XML → a parameterised SQL `WHERE` over
//! `djmdContent c`.
//!
//! Evidence (labels: spec = pinned public reference, observed = strings in `rekordbox.exe` 7,
//! inferred = reasoning from the two):
//! - spec: pyrekordbox 0.4.x `pyrekordbox/masterdb/smartlist.py` @ 43137cb91f8297c768d2a8bdce5b27b2225f0a2d
//!   (<https://raw.githubusercontent.com/dylanljones/pyrekordbox/43137cb91f8297c768d2a8bdce5b27b2225f0a2d/pyrekordbox/masterdb/smartlist.py>)
//!   and `docs/source/formats/db6.md` there (`Attribute` 4 = smart playlist, `SmartList` = XML string).
//! - spec: shape is `<NODE Id=".." LogicalOperator="1|2" AutomaticUpdate="0|1"><CONDITION PropertyName=".."
//!   Operator="1..11" ValueUnit=".." ValueLeft=".." ValueRight=".."/>…</NODE>`; `LogicalOperator` 1 = all, 2 = any;
//!   operators 1 equal, 2 not equal, 3 greater, 4 less, 5 in range, 6 in last, 7 not in last, 8 contains,
//!   9 not contains, 10 starts with, 11 ends with; valid operators per property kind as pyrekordbox's `VALID_OPS`.
//! - observed: `rekordbox.exe` holds the literals `smartlist.xml NODE CONDITION LogicalOperator AutomaticUpdate
//!   PropertyName Operator ValueUnit ValueLeft ValueRight`, the property names `name artist album genre albumArtist
//!   comments producer mixName remixedBy label originalArtist fileName duration year counter rating dateReleased
//!   stockDate dateCreated myTag grouping`, the units `day(s)`/`month(s)`/`month`, and the operator captions
//!   `is in the range / is in the last / is not in the last / contains / does not contain / starts with / ends with`,
//!   plus "all of the / any of the following conditions" (the all/any switch).
//! - inferred: the UI order "Release Date, Date Added, Date Created" next to `dateReleased, stockDate, dateCreated`
//!   makes `stockDate` = "Date Added" = `djmdContent.StockDate`, and `dateCreated` = `djmdContent.DateCreated`
//!   (pyrekordbox maps it to `created_at`; the column named like the property won here).
//! - inferred: `bpm` values are compared raw against `djmdContent.BPM` (BPM × 100), as pyrekordbox does;
//!   `duration` against `Length` (seconds); `rating` against `Rating`. Dates are `YYYY-MM-DD`.
//! - inferred: text matching is ASCII case-insensitive (SQLite `LIKE`/`NOCASE`); NULL text counts as `''`
//!   and NULL numbers as 0, so "does not contain" matches tracks without that tag.
//!
//! Supported properties: `name` (title), `artist`, `album`, `genre`, `key`, `comments`, `fileName` (text: operators
//! 1 2 8 9 10 11); `bpm`, `rating`, `duration` (number: 1 2 3 4 5); `stockDate`, `dateCreated` (date: 1 to 7, units
//! `day`, `month`). Every other property or operator is an `Err` naming it, never "match everything".
//! `album` is `djmdAlbum.Name` via `djmdContent.AlbumID` (inferred from the pyrekordbox schema, like the `Track` album join).
//! Not supported (ceiling, add as map entries): albumArtist, originalArtist, producer, mixName,
//! remixedBy, label, year, counter, dateReleased, myTag, grouping.

use openfader_plugin::markup::{Edge, tags};
use rusqlite::types::Value;
use std::collections::HashMap;

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Num,
    Date,
}

const OPS: [&str; 11] = [
    "equal",
    "not equal",
    "greater",
    "less",
    "in range",
    "in last",
    "not in last",
    "contains",
    "does not contain",
    "starts with",
    "ends with",
];

/// SQL expression over `c` = `djmdContent` and the property's kind.
fn property(name: &str) -> Option<(&'static str, Kind)> {
    use Kind::*;
    Some(match name {
        "name" => ("c.Title", Text),
        "artist" => ("(select Name from djmdArtist where ID = c.ArtistID)", Text),
        "album" => ("(select Name from djmdAlbum where ID = c.AlbumID)", Text),
        "genre" => ("(select Name from djmdGenre where ID = c.GenreID)", Text),
        "key" => ("(select ScaleName from djmdKey where ID = c.KeyID)", Text),
        "comments" => ("c.Commnt", Text),
        "fileName" => ("c.FileNameL", Text),
        "bpm" => ("c.BPM", Num),
        "rating" => ("c.Rating", Num),
        "duration" => ("c.Length", Num),
        "stockDate" => ("c.StockDate", Date),
        "dateCreated" => ("c.DateCreated", Date),
        _ => return None,
    })
}

struct Cond {
    prop: String,
    op: u32,
    unit: String,
    left: String,
    right: String,
}

/// `(any, conditions)`; `any` = `LogicalOperator` 2.
fn parse(xml: &str) -> Result<(bool, Vec<Cond>), String> {
    let mut it = tags(xml)?
        .into_iter()
        .filter(|t| t.edge != Edge::Close)
        .map(|t| (t.name, t.attrs));
    let (name, root) = it.next().ok_or("empty smart list XML")?;
    if name != "NODE" {
        return Err(format!(
            "smart list XML starts with <{name}>, expected <NODE>"
        ));
    }
    let any = match attr(&root, "LogicalOperator")? {
        "1" => false,
        "2" => true,
        o => return Err(format!("unsupported LogicalOperator '{o}'")),
    };
    let mut conds = Vec::new();
    for (name, a) in it {
        if name != "CONDITION" {
            return Err(format!("unsupported smart list element <{name}>"));
        }
        let op = attr(&a, "Operator")?;
        conds.push(Cond {
            prop: attr(&a, "PropertyName")?.to_string(),
            op: op
                .parse()
                .map_err(|_| format!("operator '{op}' is not a number"))?,
            unit: a.get("ValueUnit").cloned().unwrap_or_default(),
            left: a.get("ValueLeft").cloned().unwrap_or_default(),
            right: a.get("ValueRight").cloned().unwrap_or_default(),
        });
    }
    if conds.is_empty() {
        return Err("smart list has no conditions".into());
    }
    Ok((any, conds))
}

fn attr<'a>(a: &'a HashMap<String, String>, k: &str) -> Result<&'a str, String> {
    a.get(k)
        .map(String::as_str)
        .ok_or_else(|| format!("missing attribute {k}"))
}

fn cmp(op: u32) -> &'static str {
    ["=", "<>", ">", "<"][op as usize - 1]
}

fn date(s: &str) -> Result<Value, String> {
    let b = s.as_bytes();
    if b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
    {
        Ok(Value::Text(s.into()))
    } else {
        Err(format!("'{s}' is not a YYYY-MM-DD date"))
    }
}

/// SQL for one condition; its parameters are appended to `out` in placeholder order.
fn condition(c: &Cond, out: &mut Vec<Value>) -> Result<String, String> {
    let (col, kind) =
        property(&c.prop).ok_or_else(|| format!("unsupported smart list property '{}'", c.prop))?;
    let name = OPS
        .get((c.op as usize).wrapping_sub(1))
        .ok_or_else(|| format!("unknown smart list operator {}", c.op))?;
    let valid: &[u32] = match kind {
        Kind::Text => &[1, 2, 8, 9, 10, 11],
        Kind::Num => &[1, 2, 3, 4, 5],
        Kind::Date => &[1, 2, 3, 4, 5, 6, 7],
    };
    if !valid.contains(&c.op) {
        return Err(format!(
            "unsupported smart list operator {} ({name}) for property '{}'",
            c.op, c.prop
        ));
    }
    match kind {
        Kind::Text => {
            let e = format!("coalesce({col}, '')");
            let esc = c
                .left
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let (sql, p) = match c.op {
                1 | 2 => (
                    format!("{e} {} ? collate nocase", cmp(c.op)),
                    c.left.clone(),
                ),
                8 => (format!("{e} like ? escape '\\'"), format!("%{esc}%")),
                9 => (format!("{e} not like ? escape '\\'"), format!("%{esc}%")),
                10 => (format!("{e} like ? escape '\\'"), format!("{esc}%")),
                _ => (format!("{e} like ? escape '\\'"), format!("%{esc}")),
            };
            out.push(Value::Text(p));
            Ok(sql)
        }
        Kind::Num => {
            let num = |s: &str| {
                s.trim()
                    .parse::<f64>()
                    .map(Value::Real)
                    .map_err(|_| format!("'{s}' is not a number (property '{}')", c.prop))
            };
            out.push(num(&c.left)?);
            if c.op == 5 {
                out.push(num(&c.right)?);
                return Ok(format!("coalesce({col}, 0) between ? and ?"));
            }
            Ok(format!("coalesce({col}, 0) {} ?", cmp(c.op)))
        }
        Kind::Date => {
            let e = format!("coalesce(substr({col}, 1, 10), '')");
            if c.op >= 6 {
                let n: u32 = c
                    .left
                    .trim()
                    .parse()
                    .map_err(|_| format!("'{}' is not a day/month count", c.left))?;
                let unit = match c.unit.as_str() {
                    "day" => "days",
                    "month" => "months",
                    u => return Err(format!("unsupported smart list unit '{u}'")),
                };
                out.push(Value::Text(format!("-{n} {unit}")));
                return Ok(format!(
                    "{e} {} date('now', ?)",
                    if c.op == 6 { ">=" } else { "<" }
                ));
            }
            out.push(date(&c.left)?);
            if c.op == 5 {
                out.push(date(&c.right)?);
                return Ok(format!("{e} between ? and ?"));
            }
            Ok(format!("{e} {} ?", cmp(c.op)))
        }
    }
}

/// `WHERE` fragment (parenthesised, `?` placeholders) over `djmdContent c`, and its parameters.
/// The caller adds the `rb_local_deleted` filter and the ordering.
pub fn where_clause(xml: &str) -> Result<(String, Vec<Value>), String> {
    let (any, conds) = parse(xml)?;
    let mut params = Vec::new();
    let parts = conds
        .iter()
        .map(|c| condition(c, &mut params))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((
        format!("({})", parts.join(if any { " or " } else { " and " })),
        params,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params_from_iter};

    fn node(op: u8, conds: &[(&str, u8, &str, &str, &str)]) -> String {
        let c: String = conds
            .iter()
            .map(|(p, o, u, l, r)| format!("<CONDITION PropertyName=\"{p}\" Operator=\"{o}\" ValueUnit=\"{u}\" ValueLeft=\"{l}\" ValueRight=\"{r}\"/>"))
            .collect();
        format!(
            "<NODE Id=\"-4294967294\" LogicalOperator=\"{op}\" AutomaticUpdate=\"0\">{c}</NODE>"
        )
    }

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "create table djmdContent(ID, Title, ArtistID, AlbumID, GenreID, KeyID, BPM, Length, Commnt, FileNameL, StockDate, DateCreated, Rating, rb_local_deleted default 0);
             create table djmdArtist(ID, Name); create table djmdAlbum(ID, Name); create table djmdGenre(ID, Name); create table djmdKey(ID, ScaleName);
             insert into djmdArtist values ('1', 'Alpha'), ('2', 'Beta Beat');
             insert into djmdAlbum values ('1', 'Warm Nights');
             insert into djmdGenre values ('1', 'House');
             insert into djmdKey values ('1', '8A');
             insert into djmdContent values
              ('a', 'Sunrise', '1', '1', '1', '1', 12800, 300, 'warm', 'sunrise.mp3', date('now'), '2019-05-01', 4, 0),
              ('b', '100% Pure', '2', NULL, NULL, NULL, 14000, 200, NULL, 'pure.wav', date('now', '-100 days'), '2019-06-01', 0, 0),
              ('c', 'Night_Drive', NULL, NULL, '1', '1', 9000, 100, 'dark', 'night.flac', date('now', '-400 days'), NULL, 5, 0),
              ('d', 'Gone', '1', '1', '1', '1', 12800, 300, NULL, 'gone.mp3', date('now'), NULL, 4, 1);",
        )
        .unwrap();
        db
    }

    fn ids(db: &Connection, xml: &str) -> Result<Vec<String>, String> {
        let (w, p) = where_clause(xml)?;
        let mut st = db
            .prepare(&format!(
                "select c.ID from djmdContent c where c.rb_local_deleted = 0 and {w} order by c.ID"
            ))
            .map_err(|e| e.to_string())?;
        st.query_map(params_from_iter(p), |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())
    }

    fn one(db: &Connection, p: &str, o: u8, u: &str, l: &str, r: &str) -> Vec<String> {
        ids(db, &node(1, &[(p, o, u, l, r)])).unwrap()
    }

    #[test]
    fn text_operators() {
        let db = db();
        assert_eq!(one(&db, "artist", 1, "", "alpha", ""), ["a"]); // case-insensitive
        assert_eq!(one(&db, "artist", 2, "", "Alpha", ""), ["b", "c"]); // NULL artist is "not equal"
        assert_eq!(one(&db, "name", 8, "", "NIGHT", ""), ["c"]);
        assert_eq!(one(&db, "name", 8, "", "%", ""), ["b"]); // % is literal, not a wildcard
        assert_eq!(one(&db, "name", 8, "", "_", ""), ["c"]); // _ is literal too
        assert_eq!(one(&db, "genre", 9, "", "house", ""), ["b"]); // NULL genre "does not contain"
        assert_eq!(one(&db, "name", 10, "", "sun", ""), ["a"]);
        assert_eq!(one(&db, "fileName", 11, "", ".wav", ""), ["b"]);
        assert_eq!(one(&db, "key", 1, "", "8a", ""), ["a", "c"]);
        assert_eq!(one(&db, "comments", 8, "", "dar", ""), ["c"]);
        assert_eq!(one(&db, "album", 8, "", "NIGHTS", ""), ["a"]);
        assert_eq!(one(&db, "album", 9, "", "nights", ""), ["b", "c"]); // NULL album "does not contain"
    }

    #[test]
    fn number_operators() {
        let db = db();
        assert_eq!(one(&db, "bpm", 5, "", "9000", "12800"), ["a", "c"]); // inclusive
        assert_eq!(one(&db, "bpm", 3, "", "12800", ""), ["b"]);
        assert_eq!(one(&db, "bpm", 4, "", "12800", ""), ["c"]);
        assert_eq!(one(&db, "bpm", 1, "", "12800", ""), ["a"]);
        assert_eq!(one(&db, "bpm", 2, "", "12800", ""), ["b", "c"]);
        assert_eq!(one(&db, "rating", 3, "", "3", ""), ["a", "c"]);
        assert_eq!(one(&db, "duration", 4, "", "250", ""), ["b", "c"]);
    }

    #[test]
    fn date_operators() {
        let db = db();
        assert_eq!(one(&db, "stockDate", 6, "day", "30", ""), ["a"]);
        assert_eq!(one(&db, "stockDate", 7, "day", "30", ""), ["b", "c"]);
        assert_eq!(one(&db, "stockDate", 6, "month", "6", ""), ["a", "b"]);
        assert_eq!(one(&db, "dateCreated", 1, "", "2019-05-01", ""), ["a"]);
        assert_eq!(
            one(&db, "dateCreated", 5, "", "2019-05-01", "2019-06-01"),
            ["a", "b"]
        );
        assert_eq!(one(&db, "dateCreated", 4, "", "2019-06-01", ""), ["a", "c"]); // NULL date sorts as ''
        assert!(
            where_clause(&node(1, &[("stockDate", 6, "week", "1", "")]))
                .unwrap_err()
                .contains("'week'")
        );
        assert!(
            where_clause(&node(1, &[("dateCreated", 1, "", "May 2019", "")]))
                .unwrap_err()
                .contains("May 2019")
        );
    }

    #[test]
    fn all_and_any() {
        let db = db();
        let c = [
            ("genre", 1, "", "House", ""),
            ("bpm", 5, "", "10000", "13000"),
        ];
        assert_eq!(ids(&db, &node(1, &c)).unwrap(), ["a"]);
        assert_eq!(ids(&db, &node(2, &c)).unwrap(), ["a", "c"]);
        assert!(
            ids(&db, &node(3, &c))
                .unwrap_err()
                .contains("LogicalOperator '3'")
        );
    }

    #[test]
    fn unsupported_is_an_error_not_a_match_all() {
        let e = where_clause(&node(1, &[("mixName", 1, "", "x", "")])).unwrap_err();
        assert!(e.contains("'mixName'"), "{e}");
        let e = where_clause(&node(1, &[("bpm", 8, "", "1", "")])).unwrap_err();
        assert!(
            e.contains("operator 8 (contains)") && e.contains("'bpm'"),
            "{e}"
        );
        assert!(
            where_clause(&node(1, &[("name", 12, "", "x", "")]))
                .unwrap_err()
                .contains("operator 12")
        );
        assert!(
            where_clause(&node(1, &[("name", 6, "", "1", "")]))
                .unwrap_err()
                .contains("in last")
        );
        assert!(
            where_clause(&node(1, &[("bpm", 1, "", "fast", "")]))
                .unwrap_err()
                .contains("'fast'")
        );
        // one bad condition poisons the whole list
        assert!(
            where_clause(&node(
                2,
                &[("name", 1, "", "x", ""), ("label", 1, "", "x", "")]
            ))
            .is_err()
        );
        assert!(
            where_clause(&node(1, &[]))
                .unwrap_err()
                .contains("no conditions")
        );
        assert!(where_clause("").is_err());
        assert!(where_clause("<FOO/>").unwrap_err().contains("<FOO>"));
        assert!(
            where_clause("<NODE LogicalOperator=\"1\"><CONDITION PropertyName=\"name\">").is_err()
        );
    }

    #[test]
    fn xml_entities_and_quotes() {
        let db = db();
        db.execute(
            "update djmdContent set Title = 'A & B <1>' where ID = 'a'",
            [],
        )
        .unwrap();
        let xml = "<?xml version=\"1.0\"?><NODE Id=\"1\" LogicalOperator=\"1\" AutomaticUpdate=\"0\">\
                   <CONDITION PropertyName='name' Operator=\"1\" ValueUnit=\"\" ValueLeft=\"A &amp; B &lt;1&gt;\" ValueRight=\"\" /></NODE>";
        assert_eq!(ids(&db, xml).unwrap(), ["a"]);
    }
}
