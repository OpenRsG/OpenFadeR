use super::{Row, rows::Kind};
use crate::app::PrefRowData;
use slint::SharedString;

struct Entry {
    view: PrefRowData,
    section: usize,
    search: String,
}

#[derive(Default)]
pub(super) struct Index {
    rows: Vec<Entry>,
    pub sections: Vec<SharedString>,
}

fn append(out: &mut String, text: &str) {
    out.extend(text.chars().flat_map(char::to_lowercase));
    out.push(' ');
}

impl Index {
    pub fn new(rows: Vec<Row>) -> Self {
        let mut sections = vec![SharedString::from(tr!("All groups"))];
        let mut section = 0;
        let mut group = String::new();
        let rows = rows
            .into_iter()
            .map(|row| {
                if row.kind == Kind::Header {
                    section = sections.len();
                    sections.push(row.label.as_str().into());
                    group = row.label.clone();
                }
                let mut search = String::with_capacity(
                    group.len() + row.label.len() + row.help.len() + row.key.len() + 32,
                );
                for text in [&group, &row.label, &row.help, &row.key] {
                    append(&mut search, text);
                }
                // Text rows hold paths or private values in hint; search metadata, never entered values.
                if row.kind != Kind::Text {
                    append(&mut search, &row.hint);
                    for option in &row.options {
                        append(&mut search, option);
                    }
                }
                Entry {
                    view: row.into(),
                    section,
                    search,
                }
            })
            .collect();
        Self { rows, sections }
    }

    pub fn rows(&self, query: &str, section: usize) -> Vec<PrefRowData> {
        let query = query.trim().to_lowercase();
        let mut found = Vec::with_capacity(self.rows.len());
        let mut heading = None;
        for entry in &self.rows {
            if entry.view.kind == Kind::Header as i32 {
                heading = Some(&entry.view);
                continue;
            }
            if (section == 0 || section == entry.section)
                && query
                    .split_whitespace()
                    .all(|word| entry.search.contains(word))
            {
                if let Some(header) = heading.take() {
                    found.push(header.clone());
                }
                found.push(entry.view.clone());
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searching_explanations_keeps_the_disabled_control_and_its_group() {
        let rows = Index::new(vec![
            Row::header("Mixer"),
            Row::check("crossfader_enabled", "Enable crossfader", false).enabled(false),
            Row::header("Deck"),
            Row::check("quantize", "Quantize", true),
        ]);
        let result = rows.rows("AUTOMIX", 0);
        assert_eq!(
            result.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["", "crossfader_enabled"]
        );
        assert_eq!(result[0].label.as_str(), "Mixer");
        assert!(!result[1].enabled);
        assert_eq!(result[1].current, 0);
        assert_eq!(rows.rows("AUTOMIX", 2).len(), 0);
    }

    #[test]
    fn entered_values_are_not_part_of_the_search_index() {
        let rows = Index::new(vec![Row::text(
            "custom_location",
            "Recording location",
            "private-secret-value",
        )]);
        assert!(rows.rows("private-secret-value", 0).is_empty());
        assert_eq!(
            rows.rows("recording location", 0)[0].key.as_str(),
            "custom_location"
        );
    }

    #[test]
    fn a_group_search_shows_its_controls_without_leaking_other_sections() {
        let rows = Index::new(vec![
            Row::header("Mixer"),
            Row::check("a", "Level", true),
            Row::check("b", "Curve", false),
            Row::header("Deck"),
            Row::check("c", "Level", true),
        ]);
        let result = rows.rows("mixer", 0);
        assert_eq!(
            result.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["", "a", "b"]
        );
        assert_eq!(
            rows.rows("level", 2)
                .iter()
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            ["", "c"]
        );
    }
}
