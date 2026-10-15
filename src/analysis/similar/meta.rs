//! Year, composer and label from an audio file's tags (symphonia, metadata only: nothing is decoded). The libraries'
//! own `releaseYear` / composer columns are not in [`crate::library::Track`], so Era and Association read the files.

use std::path::Path;
use symphonia::core::{
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::{MetadataOptions, MetadataRevision, StandardTagKey},
    probe::Hint,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tags {
    /// Release year (`Date`, `ReleaseDate` or `OriginalDate`, in that order).
    pub year: Option<u16>,
    pub composer: String,
    /// Record label tag (ID3 `TPUB`, Vorbis `LABEL`); [`crate::library::Track::label`] wins when the library has one.
    pub label: String,
}

/// First plausible (1900..=2100) four-digit run of a date text: "2014", "2014-06-02", "June 2014".
pub fn year_of(text: &str) -> Option<u16> {
    let b = text.as_bytes();
    (0..b.len().saturating_sub(3)).find_map(|i| {
        let d = &b[i..i + 4];
        let edge = |j: Option<usize>| j.and_then(|j| b.get(j)).is_none_or(|c| !c.is_ascii_digit());
        (d.iter().all(u8::is_ascii_digit) && edge(i.checked_sub(1)) && edge(Some(i + 4)))
            .then(|| text[i..i + 4].parse::<u16>().ok())
            .flatten()
            .filter(|y| (1900..=2100).contains(y))
    })
}

fn apply(t: &mut Tags, years: &mut [Option<u16>; 3], rev: &MetadataRevision) {
    for tag in rev.tags() {
        let v = tag.value.to_string();
        let v = v.trim_matches(|c: char| c.is_whitespace() || c == '\0');
        match tag.std_key {
            _ if v.is_empty() => {}
            Some(StandardTagKey::Date) => years[0] = years[0].or(year_of(v)),
            Some(StandardTagKey::ReleaseDate) => years[1] = years[1].or(year_of(v)),
            Some(StandardTagKey::OriginalDate) => years[2] = years[2].or(year_of(v)),
            Some(StandardTagKey::Composer) if t.composer.is_empty() => t.composer = v.into(),
            Some(StandardTagKey::Label) if t.label.is_empty() => t.label = v.into(),
            _ => {}
        }
    }
}

/// The tags of `path`; empty when the file cannot be read or has none.
pub fn read_tags(path: &Path) -> Tags {
    let mut t = Tags::default();
    let mut years = [None; 3];
    let _ = (|| -> Option<()> {
        let mss = MediaSourceStream::new(
            Box::new(std::fs::File::open(path).ok()?),
            Default::default(),
        );
        let mut hint = Hint::new();
        hint.with_extension(path.extension()?.to_str()?);
        let mut probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .ok()?;
        if let Some(rev) = probed
            .metadata
            .get()
            .as_ref()
            .and_then(|m| m.current().cloned())
        {
            apply(&mut t, &mut years, &rev);
        }
        if let Some(rev) = probed.format.metadata().current() {
            apply(&mut t, &mut years, rev);
        }
        Some(())
    })();
    t.year = years.into_iter().flatten().next();
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn years_come_out_of_date_texts() {
        assert_eq!(year_of("2014"), Some(2014));
        assert_eq!(year_of("2014-06-02"), Some(2014));
        assert_eq!(year_of("June 1999"), Some(1999));
        assert_eq!(year_of("20140602"), None); // a run of 8 digits is a number, not a year
        assert_eq!(year_of("1066"), None);
        assert_eq!(year_of(""), None);
    }

    #[test]
    fn a_file_without_tags_reads_empty() {
        assert_eq!(read_tags(Path::new("/nonexistent/x.mp3")), Tags::default());
    }
}
