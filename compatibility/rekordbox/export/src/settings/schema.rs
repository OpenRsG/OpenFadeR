//! The pages the host draws for a device's settings (`settings_schema`): the tabs of rekordbox's Preferences > DJ SYSTEM that
//! configure a device, minus the host's own behaviour flags (delete tracks, history, backup, computer name). A key is the
//! key of [`DeviceConfig::get`] / [`DeviceConfig::set`]; a default is what [`DeviceConfig::default`] answers.
//!
//! Labels: "Waveform Color", "Overview Waveform", "Key display format", "Waveform Position", "Image On-Jog Display" with
//! ARTWORK / CUSTOMIMAGE are the strings of rekordbox (observed). Background Color: the values are the eight track colours,
//! inferred (the exe says only "Color of device displayed on the player"); "Column": the sort item shown right of the
//! titles, inferred to be OneLibrary's `sort.isSelectedAsSubColumn` (see [`super::browse`]).
//!
//! The Column field is a [`FieldKind::Choice`] `from_list` `sorts`: its `choices` are the sort ids that are a real track
//! column ([`browse::is_column`]); the host offers the active entries of the list that are among them.

use super::{DeviceConfig, Field as Dat, browse, fields};
use openfader_plugin::library::{Choice, Field, FieldKind, Group, Scope, SettingsSchema, Tab};
use serde_json::Value;

fn choice(value: impl ToString, label: &str) -> Choice {
    Choice {
        value: value.to_string(),
        label: label.into(),
    }
}

fn group(label: &str, fields: Vec<Field>) -> Group {
    Group {
        label: label.into(),
        fields,
    }
}

fn info(key: &str, label: &str, hint: &str) -> Field {
    Field {
        key: key.into(),
        label: label.into(),
        hint: hint.into(),
        default: String::new(),
        scope: Scope::Both,
        kind: FieldKind::Info,
    }
}

/// A field of `key` with its default.
fn field(key: &str, label: &str, scope: Scope, kind: FieldKind) -> Field {
    Field {
        key: key.into(),
        label: label.into(),
        hint: String::new(),
        default: DeviceConfig::default().get(key).unwrap_or_default(),
        scope,
        kind,
    }
}

fn hinted(mut f: Field, hint: &str) -> Field {
    f.hint = hint.into();
    f
}

fn bool_field(key: &str, label: &str, scope: Scope) -> Field {
    field(key, label, scope, FieldKind::Bool)
}

/// The `dat.<id>` field of a settings byte: a choice of its values (stored as the byte).
fn dat(f: &Dat, label: &str) -> Field {
    field(
        &format!("dat.{}", f.id),
        label,
        Scope::Both,
        FieldKind::Choice {
            choices: f.choices.iter().map(|c| choice(c.0, c.1)).collect(),
            from_list: None,
            none: None,
        },
    )
}

fn dat_by_id(id: &str, label: Option<&str>) -> Field {
    let f = fields()
        .find(|f| f.id == id)
        .expect("a field of the tables");
    dat(f, label.unwrap_or(f.label))
}

/// A list editor over the `(id, menu item)` rows of a browse table.
fn list(
    key: &str,
    label: &str,
    titles: [&str; 2],
    rows: impl Iterator<Item = (u16, u16)>,
) -> Field {
    field(
        key,
        label,
        Scope::Both,
        FieldKind::List {
            inactive: titles[0].into(),
            active: titles[1].into(),
            items: rows
                .map(|(id, menu)| choice(id, browse::label(menu)))
                .collect(),
        },
    )
}

fn tab(label: &str, scope: Scope, groups: Vec<Group>) -> Tab {
    Tab {
        label: label.into(),
        scope,
        groups,
        ..Tab::default()
    }
}

pub fn schema(_settings: &Value) -> Result<SettingsSchema, String> {
    let general = tab(
        "GENERAL",
        Scope::Both,
        vec![
            group(
                "DEVICE",
                vec![
                    field("device_name", "Device Name", Scope::Device, FieldKind::Text),
                    hinted(
                        field(
                            "background",
                            "Background Color",
                            Scope::Device,
                            FieldKind::Choice {
                                choices: std::iter::once(choice(0, "NONE"))
                                    .chain(
                                        browse::COLORS
                                            .iter()
                                            .enumerate()
                                            .map(|(i, c)| choice(i + 1, c)),
                                    )
                                    .collect(),
                                from_list: None,
                                none: None,
                            },
                        ),
                        "on the player",
                    ),
                ],
            ),
            group(
                "GENERAL",
                [
                    ("waveform_color", None),
                    ("overview_waveform", None),
                    ("key_display", None),
                    ("waveform_position", None),
                    ("waveform_divisions", Some("Waveform Divisions")),
                ]
                .map(|(id, label)| dat_by_id(id, label))
                .into(),
            ),
            group(
                "IMAGE ON-JOG DISPLAY",
                vec![
                    field(
                        "jog_custom",
                        "Image On-Jog Display",
                        Scope::Both,
                        FieldKind::Choice {
                            choices: vec![choice(0, "ARTWORK"), choice(1, "CUSTOM IMAGE")],
                            from_list: None,
                            none: None,
                        },
                    ),
                    field(
                        "jog_image",
                        "Custom image",
                        Scope::Both,
                        FieldKind::File {
                            extensions: vec!["jpg".into(), "jpeg".into()],
                            sets: [("jog_custom".to_owned(), "1".to_owned())].into(),
                        },
                    ),
                ],
            ),
        ],
    );
    let category = tab(
        "CATEGORY",
        Scope::Both,
        vec![group(
            "",
            vec![list(
                "categories",
                "Categories",
                ["Inactive Categories", "Active Categories"],
                browse::CATEGORIES.iter().map(|c| (c.1, c.0)),
            )],
        )],
    );
    let sort = tab(
        "SORT",
        Scope::Both,
        vec![group(
            "",
            vec![list(
                "sorts",
                "Sort options",
                ["Inactive Sort Options", "Active Sort Options"],
                browse::SORTS.iter().map(|s| (s.1, s.0)),
            )],
        )],
    );
    let column = tab(
        "COLUMN",
        Scope::Both,
        vec![group(
            "",
            vec![
                info(
                    "info.column",
                    "COLUMN",
                    "the sort item shown right of the titles",
                ),
                hinted(
                    field(
                        "column",
                        "Column",
                        Scope::Both,
                        FieldKind::Choice {
                            choices: browse::SORTS
                                .iter()
                                .filter(|s| browse::is_column(s.1))
                                .map(|s| choice(s.1, browse::label(s.0)))
                                .collect(),
                            from_list: Some("sorts".into()),
                            none: Some("NONE".into()),
                        },
                    ),
                    "on the player",
                ),
            ],
        )],
    );
    let color = tab(
        "COLOR",
        Scope::Both,
        vec![group(
            "",
            std::iter::once(info(
                "info.color",
                "COLOR",
                "a comment for each of the eight track colours",
            ))
            .chain(browse::COLORS.iter().enumerate().map(|(i, c)| {
                field(
                    &format!("color.{}", i + 1),
                    &c.to_uppercase(),
                    Scope::Both,
                    FieldKind::Text,
                )
            }))
            .collect(),
        )],
    );
    let mut my_settings = tab(
        "MY SETTINGS",
        Scope::Both,
        std::iter::once(group(
            "",
            vec![bool_field(
                "auto_apply",
                "Apply all settings automatically on export",
                Scope::Device,
            )],
        ))
        .chain(
            [
                "PLAYER > DJ SETTING",
                "PLAYER > DISPLAY (LCD)",
                "PLAYER > DISPLAY (INDICATOR)",
                "MIXER > DJ SETTING",
                "MIXER > BRIGHTNESS",
            ]
            .map(|g| {
                group(
                    g,
                    fields()
                        .filter(|f| f.kind != super::Kind::Dev && f.group == g)
                        .map(|f| dat(f, f.label))
                        .collect(),
                )
            }),
        )
        .collect(),
    );
    my_settings.apply_label = Some("APPLY ON ALL DEVICES".into());
    my_settings.read_label = Some("READ FROM DEVICE".into());
    my_settings.auto_key = Some("auto_apply".into());
    let others = tab(
        "OTHERS",
        Scope::Global,
        vec![
            group(
                "HOT CUE AUTO LOAD",
                vec![bool_field(
                    "hotcue_autoload",
                    "Prepare Hot Cue Auto Load setting for player use (on import)",
                    Scope::Both,
                )],
            ),
            group("EXPORT", vec![dat_by_id("quantize_beat_value", None)]),
            group(
                "LINK MONITOR",
                vec![info(
                    "info.link",
                    "PRO DJ LINK",
                    "needs a network of players: not available offline",
                )],
            ),
        ],
    );
    Ok(SettingsSchema {
        tabs: vec![general, category, sort, column, color, my_settings, others],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn all() -> Vec<(String, Tab, Field)> {
        let s = schema(&Value::Null).unwrap();
        s.tabs
            .into_iter()
            .flat_map(|t| {
                t.groups
                    .iter()
                    .flat_map(|g| g.fields.clone())
                    .map(|f| (t.label.clone(), t.clone(), f))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn tab_named(name: &str) -> Tab {
        schema(&Value::Null)
            .unwrap()
            .tabs
            .into_iter()
            .find(|t| t.label == name)
            .unwrap_or_else(|| panic!("no tab {name}"))
    }

    fn field_of(key: &str) -> Field {
        all().into_iter().find(|f| f.2.key == key).unwrap().2
    }

    #[test]
    fn the_tabs_are_the_old_pages_without_the_host_flags() {
        let s = schema(&Value::Null).unwrap();
        let tabs: Vec<_> = s.tabs.iter().map(|t| (t.label.as_str(), t.scope)).collect();
        assert_eq!(
            tabs,
            [
                ("GENERAL", Scope::Both),
                ("CATEGORY", Scope::Both),
                ("SORT", Scope::Both),
                ("COLUMN", Scope::Both),
                ("COLOR", Scope::Both),
                ("MY SETTINGS", Scope::Both),
                ("OTHERS", Scope::Global),
            ]
        );
        let my = tab_named("MY SETTINGS");
        assert_eq!(
            (
                my.apply_label.as_deref(),
                my.read_label.as_deref(),
                my.auto_key.as_deref()
            ),
            (
                Some("APPLY ON ALL DEVICES"),
                Some("READ FROM DEVICE"),
                Some("auto_apply")
            )
        );
        let headers: Vec<_> = my.groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(
            headers,
            [
                "",
                "PLAYER > DJ SETTING",
                "PLAYER > DISPLAY (LCD)",
                "PLAYER > DISPLAY (INDICATOR)",
                "MIXER > DJ SETTING",
                "MIXER > BRIGHTNESS",
            ]
        );
    }

    #[test]
    fn every_old_key_has_a_field_and_every_byte_setting_has_a_row() {
        let keys: BTreeSet<String> = all()
            .into_iter()
            .filter(|f| f.2.kind != FieldKind::Info)
            .map(|f| f.2.key)
            .collect();
        let mut want: BTreeSet<String> = [
            "device_name",
            "background",
            "categories",
            "sorts",
            "column",
            "hotcue_autoload",
            "jog_custom",
            "jog_image",
            "auto_apply",
        ]
        .map(String::from)
        .into();
        want.extend((1..=8).map(|i| format!("color.{i}")));
        want.extend(fields().map(|f| format!("dat.{}", f.id)));
        assert_eq!(keys, want);
    }

    #[test]
    fn a_key_is_a_field_once_per_tab_and_the_device_page_has_the_device_rows() {
        for t in schema(&Value::Null).unwrap().tabs {
            let mut seen = BTreeSet::new();
            for f in t.groups.iter().flat_map(|g| &g.fields) {
                assert!(seen.insert(&f.key), "{}: {} twice", t.label, f.key);
            }
        }
        for k in ["device_name", "background", "auto_apply"] {
            assert_eq!(field_of(k).scope, Scope::Device, "{k}");
        }
        assert_eq!(field_of("dat.sync").scope, Scope::Both);
        assert_eq!(tab_named("GENERAL").groups[0].label, "DEVICE");
    }

    #[test]
    fn defaults_and_choices_are_what_the_config_takes() {
        for (tab, _, f) in all() {
            if f.kind == FieldKind::Info {
                continue;
            }
            let mut c = DeviceConfig::default();
            if f.key != "jog_image" {
                assert_eq!(
                    c.get(&f.key).as_deref(),
                    Some(f.default.as_str()),
                    "{}",
                    f.key
                );
                c.set(&f.key, &f.default)
                    .unwrap_or_else(|e| panic!("{tab}/{}: {e}", f.key));
            }
            match f.kind {
                FieldKind::Choice {
                    choices,
                    from_list: None,
                    ..
                } => {
                    assert!(choices.iter().any(|c| c.value == f.default), "{}", f.key);
                    for ch in choices {
                        c.set(&f.key, &ch.value)
                            .unwrap_or_else(|e| panic!("{}: {e}", f.key));
                    }
                }
                FieldKind::Bool => {
                    assert!(c.set(&f.key, "0").is_ok() && c.set(&f.key, "1").is_ok());
                }
                _ => {}
            }
        }
    }

    #[test]
    fn dat_choices_are_the_tables_with_decimal_bytes() {
        for f in fields() {
            let sf = field_of(&format!("dat.{}", f.id));
            let FieldKind::Choice { choices, .. } = sf.kind else {
                panic!("{} is not a choice", f.id)
            };
            let want: Vec<_> = f.choices.iter().map(|c| (c.0.to_string(), c.1)).collect();
            let got: Vec<_> = choices
                .iter()
                .map(|c| (c.value.clone(), c.label.as_str()))
                .collect();
            assert_eq!(got, want, "{}", f.id);
        }
        let general = tab_named("GENERAL");
        let labels: Vec<_> = general.groups[1]
            .fields
            .iter()
            .map(|f| f.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Waveform Color",
                "Overview Waveform",
                "Key display format",
                "Waveform Position",
                "Waveform Divisions"
            ]
        );
        let others = tab_named("OTHERS");
        assert_eq!(others.groups[1].fields[0].key, "dat.quantize_beat_value");
    }

    #[test]
    fn the_list_editors_hold_every_table_row() {
        let FieldKind::List {
            inactive,
            active,
            items,
        } = field_of("categories").kind
        else {
            panic!("categories is not a list")
        };
        assert_eq!(
            (inactive.as_str(), active.as_str()),
            ("Inactive Categories", "Active Categories")
        );
        assert_eq!(items.len(), 22);
        let defaults: Vec<_> = field_of("categories")
            .default
            .split(',')
            .map(String::from)
            .collect();
        assert_eq!((defaults.len(), items.len() - defaults.len()), (10, 12));
        assert_eq!(
            items.iter().find(|c| c.value == defaults[0]).unwrap().label,
            "ARTIST"
        );
        let FieldKind::List {
            inactive,
            active,
            items,
        } = field_of("sorts").kind
        else {
            panic!("sorts is not a list")
        };
        assert_eq!(
            (inactive.as_str(), active.as_str()),
            ("Inactive Sort Options", "Active Sort Options")
        );
        assert_eq!(items.len(), 17);
        assert_eq!(field_of("sorts").default, "0,1,2,3,4,5,12");
    }

    #[test]
    fn the_column_offers_the_real_columns_of_the_sort_list() {
        let FieldKind::Choice {
            choices,
            from_list,
            none,
        } = field_of("column").kind
        else {
            panic!("column is not a choice")
        };
        assert_eq!(
            (from_list.as_deref(), none.as_deref()),
            (Some("sorts"), Some("NONE"))
        );
        let values: Vec<_> = choices.iter().map(|c| c.value.as_str()).collect();
        assert!(
            !values.contains(&"0") && !values.contains(&"1"),
            "DEFAULT / ALPHABET: {values:?}"
        );
        // the default active sorts that are columns: ARTIST, ALBUM, BPM, RATING, KEY
        for id in ["2", "3", "4", "5", "12"] {
            assert!(values.contains(&id), "{id}");
        }
        assert_eq!(
            choices.iter().find(|c| c.value == "4").unwrap().label,
            "BPM"
        );
    }

    #[test]
    fn colours_and_the_jog_image() {
        let color = tab_named("COLOR");
        let f = &color.groups[0].fields;
        assert_eq!(f[0].kind, FieldKind::Info);
        assert_eq!(
            (
                f[2].key.as_str(),
                f[2].label.as_str(),
                f[2].default.as_str()
            ),
            ("color.2", "RED", "Red")
        );
        assert_eq!(f.len(), 9);
        assert_eq!(
            field_of("jog_image").kind,
            FieldKind::File {
                extensions: vec!["jpg".into(), "jpeg".into()],
                sets: [("jog_custom".to_owned(), "1".to_owned())].into(),
            }
        );
        assert_eq!(field_of("background").hint, "on the player");
        assert_eq!(field_of("auto_apply").default, "1");
    }

    #[test]
    fn the_schema_survives_the_wire() {
        let s = schema(&Value::Null).unwrap();
        let back: SettingsSchema =
            serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
