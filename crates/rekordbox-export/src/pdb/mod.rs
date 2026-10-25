//! DeviceSQL writer for `export.pdb` / `exportExt.pdb` (base): packs rows into 4096-byte pages and lays out the
//! tables the way a fresh rekordbox 7 export does. Page and index-page layouts: pinned crate-digger
//! `rekordbox_pdb.ksy` (spec) and a real export (observed); both are written up in `.local/re/export/notes.md`.
//!
//! Children: `rows` (one encoder per row type), [`db`] (the table sets of a library and of the MyTag file) and [`read`] (a file
//! back into a [`db::Db`]).

pub mod db;
pub mod read;
pub(crate) mod rows;

use rekordbox_common::pdb::HEAP;

const PAGE: usize = 4096;
/// Bytes of one row-index group (16 offsets, presence word, transaction word).
const GROUP: usize = 0x24;
/// Slots of an index page's page list (`(4096 - 0x3c - 20) / 4`, observed).
const INDEX_SLOTS: usize = 1004;

/// One table: its page type and its encoded rows (see [`rows`]).
struct Table {
    ty: u32,
    rows: Vec<Vec<u8>>,
    /// Rows carry an `index_shift` / `tag_index` u2 at offset 2 that is `0x20 * row index within its page`.
    shift: bool,
}

fn w16(p: &mut [u8], at: usize, v: u16) {
    p[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn w32(p: &mut [u8], at: usize, v: u32) {
    p[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn header(idx: u32, ty: u32, next: u32, seq: u32) -> Vec<u8> {
    let mut p = vec![0u8; PAGE];
    w32(&mut p, 4, idx);
    w32(&mut p, 8, ty);
    w32(&mut p, 0xc, next);
    w32(&mut p, 0x10, seq);
    p
}

/// The page every table starts with: no rows. Its page list names the data pages that still have free space (flag
/// 0x34); every page of a fresh export is full (0x24), so the list is empty (observed: tables nobody edited have none).
fn index_page(idx: u32, ty: u32, next: u32, first_data: Option<u32>, seq: u32) -> Vec<u8> {
    let mut p = header(idx, ty, next, seq);
    p[0x1b] = 0x64;
    w16(&mut p, 0x20, 0x1fff);
    w16(&mut p, 0x22, 0x1fff);
    w16(&mut p, 0x24, 0x03ec);
    w32(&mut p, HEAP, idx);
    w32(&mut p, HEAP + 4, first_data.unwrap_or(0x03ff_ffff));
    w32(&mut p, HEAP + 8, 0x03ff_ffff);
    w16(&mut p, HEAP + 18, 0x1fff);
    for i in 0..INDEX_SLOTS {
        w32(&mut p, HEAP + 20 + 4 * i, 0x1fff_fff8);
    }
    p
}

/// Whether `n` rows with `heap` bytes of row data fit one page.
fn fits(heap: usize, n: usize) -> bool {
    HEAP + heap + n.div_ceil(16) * GROUP <= PAGE
}

/// Splits `rows` into pages' worth, each a slice range; a single row too big for a page is an error.
fn split(rows: &[Vec<u8>]) -> Result<Vec<&[Vec<u8>]>, String> {
    let (mut out, mut from, mut heap) = (Vec::new(), 0, 0);
    for (i, r) in rows.iter().enumerate() {
        if !fits(r.len(), 1) {
            return Err(format!("a row of {} bytes does not fit a page", r.len()));
        }
        if !fits(heap + r.len(), i - from + 1) {
            out.push(&rows[from..i]);
            (from, heap) = (i, 0);
        }
        heap += r.len();
    }
    if from < rows.len() {
        out.push(&rows[from..]);
    }
    Ok(out)
}

fn data_page(idx: u32, ty: u32, next: u32, seq: u32, rows: &[Vec<u8>], shift: bool) -> Vec<u8> {
    let mut p = header(idx, ty, next, seq);
    let n = rows.len();
    let groups = n.div_ceil(16);
    let mut at = 0;
    for (i, r) in rows.iter().enumerate() {
        p[HEAP + at..HEAP + at + r.len()].copy_from_slice(r);
        if shift {
            w16(&mut p, HEAP + at + 2, (i * 0x20) as u16);
        }
        let base = PAGE - (i / 16) * GROUP;
        w16(&mut p, base - 6 - 2 * (i % 16), at as u16);
        at += r.len();
    }
    for g in 0..groups {
        let present = (1u32 << (n - g * 16).min(16)) - 1;
        let base = PAGE - g * GROUP;
        w16(&mut p, base - 4, present as u16);
        w16(&mut p, base - 2, present as u16);
    }
    w32(&mut p, 0x18, n as u32 | (n as u32) << 13 | 0x24 << 24);
    w16(&mut p, 0x1c, (PAGE - HEAP - at - 2 * n - 4 * groups) as u16);
    w16(&mut p, 0x1e, at as u16);
    w16(&mut p, 0x20, n as u16);
    p
}

/// A whole `export.pdb` / `exportExt.pdb` for `tables` (in the order they get table entries).
///
/// Table `t`: index page `2t+1`, first data page `2t+2`, further data pages and the next free ("empty
/// candidate") page from the tail after all `2t+2` pages. A table without rows keeps `2t+2` as its candidate; it
/// is written as a zero page (observed).
fn build(tables: &[Table]) -> Result<Vec<u8>, String> {
    let n = tables.len() as u32;
    let mut next_free = 2 * n + 1;
    let mut pages: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut entries = Vec::new(); // (type, empty_candidate, first, last)
    let mut seq = 1;
    let mut seq_of = || {
        seq += 1;
        seq
    };
    for (t, table) in tables.iter().enumerate() {
        let (index, first) = (2 * t as u32 + 1, 2 * t as u32 + 2);
        let chunks = split(&table.rows)?;
        let data: Vec<u32> = (0..chunks.len())
            .map(|i| {
                if i == 0 {
                    first
                } else {
                    next_free_inc(&mut next_free)
                }
            })
            .collect();
        let candidate = if chunks.is_empty() {
            first
        } else {
            next_free_inc(&mut next_free)
        };
        pages.push((
            index,
            index_page(
                index,
                table.ty,
                data.first().copied().unwrap_or(candidate),
                data.first().copied(),
                seq_of(),
            ),
        ));
        for (i, rows) in chunks.iter().enumerate() {
            let next = data.get(i + 1).copied().unwrap_or(candidate);
            pages.push((
                data[i],
                data_page(data[i], table.ty, next, seq_of(), rows, table.shift),
            ));
        }
        entries.push((
            table.ty,
            candidate,
            index,
            data.last().copied().unwrap_or(index),
        ));
    }
    let mut file = vec![0u8; PAGE];
    for (i, v) in [0, PAGE as u32, n, next_free, 5, seq + 1, 0]
        .into_iter()
        .enumerate()
    {
        w32(&mut file, 4 * i, v);
    }
    for (t, (ty, cand, first, last)) in entries.into_iter().enumerate() {
        for (i, v) in [ty, cand, first, last].into_iter().enumerate() {
            w32(&mut file, 28 + 16 * t + 4 * i, v);
        }
    }
    let end = pages.iter().map(|p| p.0).max().unwrap_or(0) as usize + 1;
    file.resize(end * PAGE, 0);
    for (idx, page) in pages {
        file[idx as usize * PAGE..(idx as usize + 1) * PAGE].copy_from_slice(&page);
    }
    Ok(file)
}

fn next_free_inc(next: &mut u32) -> u32 {
    *next += 1;
    *next - 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use rekordbox_common::pdb::{rows as read_rows, string_at, u16_at, u32_at};

    fn artists(n: u32) -> Table {
        Table {
            ty: 2,
            rows: (1..=n)
                .map(|i| rows::artist(i, &format!("Artist {i}")))
                .collect(),
            shift: true,
        }
    }

    #[test]
    fn rows_span_pages_and_read_back_in_order() {
        // 700 rows of ~24 bytes: several pages, more than one 16-row group per page.
        let empty = Table {
            ty: 7,
            rows: Vec::new(),
            shift: false,
        };
        let d = build(&[artists(700), empty]).unwrap();
        assert_eq!(d.len() % PAGE, 0);
        let at = read_rows(&d, 2).unwrap();
        assert_eq!(at.len(), 700);
        for (i, &r) in at.iter().enumerate() {
            assert_eq!(u32_at(&d, r + 4).unwrap(), i as u32 + 1);
            assert_eq!(
                string_at(&d, r + 0x0a).unwrap(),
                format!("Artist {}", i + 1)
            );
        }
        // index_shift = 0x20 * position in the page: restarts on every page.
        let pages: std::collections::BTreeSet<usize> = at.iter().map(|r| r / PAGE).collect();
        assert!(pages.len() > 3);
        for p in pages {
            let on_page: Vec<_> = at.iter().filter(|r| **r / PAGE == p).collect();
            for (k, &&r) in on_page.iter().enumerate() {
                assert_eq!(u16_at(&d, r + 2).unwrap() as usize, 0x20 * k);
            }
        }
        assert!(read_rows(&d, 7).unwrap().is_empty());
        // Header: page size, table count, next free page past every allocated one.
        assert_eq!(
            (u32_at(&d, 4).unwrap() as usize, u32_at(&d, 8).unwrap()),
            (PAGE, 2)
        );
        assert!(u32_at(&d, 12).unwrap() as usize >= d.len() / PAGE);
    }

    #[test]
    fn group_boundaries_and_empty_table_layout() {
        for n in [1, 15, 16, 17, 32, 33] {
            let d = build(&[artists(n)]).unwrap();
            assert_eq!(read_rows(&d, 2).unwrap().len(), n as usize, "{n} rows");
        }
        // An empty table is just its index page (+ the zero candidate page when something follows it).
        let d = build(&[
            Table {
                ty: 1,
                rows: Vec::new(),
                shift: false,
            },
            artists(1),
        ])
        .unwrap();
        assert!(read_rows(&d, 1).unwrap().is_empty());
        assert_eq!(u32_at(&d, 28 + 4).unwrap(), 2); // empty candidate = 2t+2
        assert!(d[2 * PAGE..3 * PAGE].iter().all(|&b| b == 0));
        assert_eq!(d[PAGE + 0x1b], 0x64); // index page flags (observed)
    }

    #[test]
    fn a_row_larger_than_a_page_is_an_error() {
        let t = Table {
            ty: 2,
            rows: vec![vec![0u8; PAGE]],
            shift: false,
        };
        assert!(build(&[t]).unwrap_err().contains("does not fit"));
    }
}
