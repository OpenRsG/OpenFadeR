//! The [INFO] panel's lines and the artwork of a track: rating, colour, comment, file type, file size, sample rate
//! and bit rate. Read when a track is loaded; the panel is display only.

use openfader::{library::Track, store};
use slint::{Color, Image};

pub struct Info {
    pub art: Option<Image>,
    pub rating: i32,
    pub color: Option<(&'static str, Color)>,
    pub comment: String,
    pub file_type: String,
    pub size: String,
    pub sample_rate: String,
    pub bit_rate: String,
}

pub fn of(t: &Track) -> Info {
    let color = store::global()
        .track_color(&store::key(t))
        .ok()
        .flatten()
        .and_then(|id| store::TRACK_COLORS.get(usize::from(id).checked_sub(1)?))
        .map(|&(name, [r, g, b])| (name, Color::from_rgb_u8(r, g, b)));
    Info {
        art: t
            .artwork
            .as_deref()
            .and_then(|p| Image::load_from_path(p).ok()),
        rating: i32::from(t.rating),
        color,
        comment: t.comment.clone(),
        file_type: t
            .file
            .as_deref()
            .and_then(|f| f.extension())
            .map_or_else(String::new, |e| e.to_string_lossy().to_uppercase()),
        size: t
            .file
            .as_deref()
            .and_then(|f| std::fs::metadata(f).ok())
            .map_or_else(String::new, |m| size_text(m.len())),
        sample_rate: rate_text(t.sample_rate),
        bit_rate: if t.bitrate == 0 {
            String::new()
        } else {
            format!("{} kbps", t.bitrate)
        },
    }
}

/// "7.4 MB", "812 KB".
pub fn size_text(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// "44.1 kHz", "48 kHz"; empty when unknown.
pub fn rate_text(hz: u32) -> String {
    match hz {
        0 => String::new(),
        hz if hz % 1000 == 0 => format!("{} kHz", hz / 1000),
        hz => format!("{:.1} kHz", f64::from(hz) / 1000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_rates_read_like_the_manual_shows_them() {
        assert_eq!(size_text(7_759_462), "7.4 MB");
        assert_eq!(size_text(831_489), "813 KB");
        assert_eq!(rate_text(44_100), "44.1 kHz");
        assert_eq!(rate_text(48_000), "48 kHz");
        assert_eq!(rate_text(0), "");
    }
}
