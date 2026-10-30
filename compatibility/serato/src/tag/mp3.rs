//! MP3: the ID3v2 tag at the head of the file (<https://github.com/Holzhaus/serato-tags/blob/main/docs/fileformats.md>).

use super::{id3, read_n, Edit, Error, Stored, Tags};
use std::fs::File;
use std::io::{Seek, SeekFrom};

/// The Serato objects of the ID3v2 tag at the head of the file.
pub fn read(f: &mut File) -> Result<Stored, Error> {
    let Some(total) = id3::total(&read_n(f, 10)?) else {
        return Ok(Stored::default());
    };
    f.seek(SeekFrom::Start(0))?;
    Ok(id3::parse(&read_n(f, total as u64)?).map_or_else(Stored::default, |t| t.stored()))
}

pub fn plan(f: &mut File, tags: &Tags, fixed: bool) -> Result<Vec<Edit>, Error> {
    let (mut room, mut old) = (0, None);
    if let Some(total) = id3::total(&read_n(f, 10)?) {
        f.seek(SeekFrom::Start(0))?;
        let buf = read_n(f, total as u64)?;
        if buf.len() != total {
            return Err(Error::Unsupported);
        }
        (room, old) = (total, id3::parse(&buf));
        if old.is_none() && tags.blank() {
            return Ok(vec![]);
        }
        if old.is_none() && fixed {
            return Err(Error::NoRoom);
        }
    }
    Ok(id3::rebuild(old.as_ref(), room, tags, fixed)?
        .map(|data| Edit {
            start: 0,
            end: room as u64,
            data,
        })
        .into_iter()
        .collect())
}
