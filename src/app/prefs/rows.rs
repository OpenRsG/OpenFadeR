//! One line of a dialog page ↔ `PrefRowData` (ui/prefs-state.slint). Pages build `Vec<Row>` from the settings;
//! a click comes back as `choose(key, index)`.

use crate::app::PrefRowData;
use slint::{ModelRc, SharedString, VecModel};

/// How a row is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A row of mutually exclusive chips (few, short options).
    Chips = 0,
    /// A drop-down list (many options, or long names).
    Dropdown = 1,
    /// A checkbox: option 0 = unchecked, 1 = checked; the label is the checkbox text.
    Check = 2,
    /// A section heading.
    Header = 3,
    /// A read-only line: label, then `hint` as value text.
    Info = 4,
    /// A text field holding `hint`; Enter reports the text to the handler registered with `prefs::on_text(key, …)`.
    Text = 5,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub label: String,
    pub hint: String,
    pub kind: Kind,
    pub options: Vec<String>,
    pub current: usize,
    pub enabled: bool,
}

impl Row {
    fn new(kind: Kind, key: &str, label: &str, options: &[&str], current: usize) -> Row {
        Row {
            key: key.into(),
            label: label.into(),
            hint: String::new(),
            kind,
            options: options.iter().map(|s| s.to_string()).collect(),
            current,
            enabled: true,
        }
    }

    /// A row for a [`Choice`] value: chips for up to 4 options, else a drop-down; names are shown upper-case.
    pub fn of<T: super::Choice>(key: &str, label: &str, current: T) -> Row {
        let names: Vec<String> = T::ALL.iter().map(|c| c.name().to_uppercase()).collect();
        let at = T::ALL.iter().position(|&c| c == current).unwrap_or(0);
        let kind = if names.len() <= 4 {
            Kind::Chips
        } else {
            Kind::Dropdown
        };
        Row {
            options: names,
            ..Row::new(kind, key, label, &[], at)
        }
    }

    pub fn chips(key: &str, label: &str, options: &[&str], current: usize) -> Row {
        Row::new(Kind::Chips, key, label, options, current)
    }

    pub fn dropdown(key: &str, label: &str, options: &[&str], current: usize) -> Row {
        Row::new(Kind::Dropdown, key, label, options, current)
    }

    /// Same as [`Row::dropdown`] for options built at run time (device lists).
    pub fn dropdown_of(key: &str, label: &str, options: Vec<String>, current: usize) -> Row {
        Row {
            options,
            ..Row::new(Kind::Dropdown, key, label, &[], current)
        }
    }

    pub fn check(key: &str, label: &str, on: bool) -> Row {
        Row::new(Kind::Check, key, label, &[], on as usize)
    }

    pub fn header(label: &str) -> Row {
        Row::new(Kind::Header, "", label, &[], 0)
    }

    /// A text entry; edits go to [`super::on_text`] handlers (the key is the handler's name).
    pub fn text(key: &str, label: &str, value: &str) -> Row {
        Row {
            hint: value.into(),
            ..Row::new(Kind::Text, key, label, &[], 0)
        }
    }

    pub fn info(label: &str, value: &str) -> Row {
        Row {
            hint: value.into(),
            ..Row::new(Kind::Info, "", label, &[], 0)
        }
    }

    /// Short grey text at the right end of the row.
    pub fn hint(mut self, hint: &str) -> Row {
        self.hint = hint.into();
        self
    }

    /// Greyed out and unclickable while `on` is false.
    pub fn enabled(mut self, on: bool) -> Row {
        self.enabled = on;
        self
    }
}

impl From<Row> for PrefRowData {
    fn from(r: Row) -> PrefRowData {
        PrefRowData {
            key: r.key.into(),
            label: r.label.into(),
            hint: r.hint.into(),
            kind: r.kind as i32,
            options: ModelRc::new(VecModel::from(
                r.options
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            )),
            current: r.current as i32,
            enabled: r.enabled,
        }
    }
}
