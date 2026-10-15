//! The page over a fake schema: what the host stores and shows is generic, whichever plugin the schema comes from.

use super::*;
use openfader_plugin::library::{Choice, Group, SettingsSchema};

fn choice(value: &str, label: &str) -> Choice {
    Choice {
        value: value.into(),
        label: label.into(),
    }
}

fn field(key: &str, scope: Scope, default: &str, kind: FieldKind) -> Field {
    Field {
        key: key.into(),
        label: format!("label of {key}"),
        hint: format!("hint of {key}"),
        default: default.into(),
        scope,
        kind,
    }
}

fn choices(of: &[(&str, &str)]) -> Vec<Choice> {
    of.iter().map(|(v, l)| choice(v, l)).collect()
}

/// A schema with every kind of field: General (a choice, a device-only bool and text, a global-only text, info) and Lists
/// (a list, two choices fed by it, a file), the second tab with the three buttons.
fn schema() -> SettingsSchema {
    let from = |list: &str, allowed: &[(&str, &str)]| FieldKind::Choice {
        choices: choices(allowed),
        from_list: Some(list.into()),
        none: Some("NO COLUMN".into()),
    };
    SettingsSchema {
        tabs: vec![
            Tab {
                label: "General".into(),
                groups: vec![Group {
                    label: "BASICS".into(),
                    fields: vec![
                        field(
                            "lang",
                            Scope::Both,
                            "b",
                            FieldKind::Choice {
                                choices: choices(&[("a", "A"), ("b", "B"), ("c", "C")]),
                                from_list: None,
                                none: None,
                            },
                        ),
                        field("auto", Scope::Device, "1", FieldKind::Bool),
                        field("name", Scope::Device, "", FieldKind::Text),
                        field("computer", Scope::Global, "pc", FieldKind::Text),
                        field("note", Scope::Both, "", FieldKind::Info),
                    ],
                }],
                ..Tab::default()
            },
            Tab {
                label: "Lists".into(),
                groups: vec![Group {
                    label: String::new(),
                    fields: vec![
                        field(
                            "cats",
                            Scope::Both,
                            "1,2",
                            FieldKind::List {
                                inactive: "Off".into(),
                                active: "On".into(),
                                items: choices(&[
                                    ("1", "One"),
                                    ("2", "Two"),
                                    ("3", "Three"),
                                    ("4", "Four"),
                                ]),
                            },
                        ),
                        field("col", Scope::Both, "", from("cats", &[])),
                        field(
                            "col2",
                            Scope::Both,
                            "",
                            from("cats", &[("2", ""), ("3", "")]),
                        ),
                        field(
                            "pic",
                            Scope::Both,
                            "",
                            FieldKind::File {
                                extensions: vec!["jpg".into()],
                                sets: [("use_pic".to_owned(), "1".to_owned())].into(),
                            },
                        ),
                        field("use_pic", Scope::Both, "0", FieldKind::Bool),
                        field(
                            "more",
                            Scope::Both,
                            "",
                            FieldKind::List {
                                inactive: "Idle".into(),
                                active: "Used".into(),
                                items: choices(&[("x", "Ex"), ("y", "Why")]),
                            },
                        ),
                    ],
                }],
                apply_label: Some("Write to devices".into()),
                read_label: Some("Read the player".into()),
                auto_key: Some("auto".into()),
                ..Tab::default()
            },
        ],
    }
}

fn ctx<'a>(s: &'a Store, scope: &'a str) -> Ctx<'a> {
    Ctx::new(s, scope, None)
}

fn field_of<'a>(schema: &'a SettingsSchema, key: &str) -> &'a Field {
    schema.tabs.iter().find_map(|t| find(t, key)).unwrap()
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("openfader-djs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn rows_show_the_schemas_labels_and_only_the_fields_of_the_scope() {
    let (s, schema) = (Store::in_memory().unwrap(), schema());
    let labels = |scope: &str, tab: usize| {
        schema_rows(&ctx(&s, scope), &schema.tabs[tab])
            .into_iter()
            .map(|r| r.label)
            .collect::<Vec<_>>()
    };
    let g = labels(GLOBAL, 0);
    assert_eq!(
        g,
        [
            "BASICS",
            "label of lang",
            "label of computer",
            "label of note",
            "Reset to defaults"
        ]
    );
    let d = labels("dev", 0);
    assert_eq!(
        d,
        [
            "BASICS",
            "label of lang",
            "label of auto",
            "label of name",
            "label of note",
            "Reset to defaults"
        ]
    );
    // The buttons follow the scope: Apply in the Preferences, Read on a device.
    assert!(labels(GLOBAL, 1).contains(&"Write to devices".to_owned()));
    assert!(!labels(GLOBAL, 1).contains(&"Read the player".to_owned()));
    assert!(labels("dev", 1).contains(&"Read the player".to_owned()));
    for r in schema_rows(&ctx(&s, GLOBAL), &schema.tabs[1])
        .iter()
        .chain(&schema_rows(&ctx(&s, "dev"), &schema.tabs[0]))
        .filter(|r| !r.key.is_empty())
    {
        assert!(r.key.starts_with("djs."), "{}", r.key);
        assert!(
            r.current == usize::MAX || r.current <= r.options.len().max(1),
            "{}: current {}",
            r.key,
            r.current
        );
    }
}

#[test]
fn a_click_back_to_the_default_forgets_the_override() {
    let (s, schema) = (Store::in_memory().unwrap(), schema());
    let (tab, lang) = (&schema.tabs[0], field_of(&schema, "lang"));
    let mut c = ctx(&s, GLOBAL);
    assert_eq!(c.value(lang), "b");
    fields::choose(&mut c, tab, lang, 2).unwrap();
    assert_eq!(c.value(lang), "c");
    assert_eq!(s.device_settings(GLOBAL).unwrap().len(), 1);
    assert_eq!(ctx(&s, GLOBAL).value(lang), "c");
    fields::choose(&mut c, tab, lang, 1).unwrap();
    assert!(s.device_settings(GLOBAL).unwrap().is_empty());
    assert!(fields::choose(&mut c, tab, lang, 9).is_err());
    let row = fields::row(&c, tab, lang);
    assert_eq!((row.options.len(), row.current), (3, 1));
}

#[test]
fn a_device_follows_the_defaults_until_it_overrides() {
    let (s, schema) = (Store::in_memory().unwrap(), schema());
    let (tab, lang) = (&schema.tabs[0], field_of(&schema, "lang"));
    fields::choose(&mut ctx(&s, GLOBAL), tab, lang, 0).unwrap();
    assert_eq!(
        ctx(&s, "d1").value(lang),
        "a",
        "the global row beats the schema default"
    );
    // Back to the schema default is an override on the device: the defaults say otherwise.
    fields::choose(&mut ctx(&s, "d1"), tab, lang, 1).unwrap();
    assert_eq!(s.device_settings("d1").unwrap().get("lang").unwrap(), "b");
    assert_eq!(ctx(&s, GLOBAL).value(lang), "a");
    // The value of the global row is what the device does not need to store.
    fields::choose(&mut ctx(&s, "d1"), tab, lang, 0).unwrap();
    assert!(s.device_settings("d1").unwrap().is_empty());
    // Reset forgets the device's own values only.
    fields::choose(&mut ctx(&s, "d1"), tab, lang, 2).unwrap();
    let mut c = ctx(&s, "d1");
    c.reset([lang]).unwrap();
    assert_eq!(c.value(lang), "a");
    assert_eq!(ctx(&s, GLOBAL).value(lang), "a");
}

#[test]
fn texts_are_trimmed_and_an_empty_one_restores_the_default() {
    let (s, schema) = (Store::in_memory().unwrap(), schema());
    let name = field_of(&schema, "name");
    let mut c = ctx(&s, "d1");
    fields::text(&mut c, name, " BOOTH ").unwrap();
    assert_eq!(ctx(&s, "d1").value(name), "BOOTH");
    assert_eq!(ctx(&s, GLOBAL).value(name), "");
    fields::text(&mut c, name, "  ").unwrap();
    assert!(s.device_settings("d1").unwrap().is_empty());
    let r = fields::row(&c, &schema.tabs[0], name);
    assert_eq!((r.key.as_str(), r.hint.as_str()), ("djs.f.name", ""));
}

#[test]
fn the_list_editor_moves_shifts_resets_and_feeds_its_choices() {
    let (s, schema) = (Store::in_memory().unwrap(), schema());
    let (tab, cats) = (&schema.tabs[1], field_of(&schema, "cats"));
    let (col, col2) = (field_of(&schema, "col"), field_of(&schema, "col2"));
    let mut c = ctx(&s, GLOBAL);
    let show = |c: &Ctx| {
        let l = lists::items(c, tab).remove(0);
        let ids = |v: &[(i32, String)]| v.iter().map(|x| x.0).collect::<Vec<_>>();
        (ids(&l.inactive), ids(&l.active), l.titles)
    };
    assert_eq!(
        show(&c),
        (vec![2, 3], vec![0, 1], ["Off".into(), "On".into()])
    );
    lists::act(&mut c, tab, "cats", "activate", 3).unwrap();
    lists::act(&mut c, tab, "cats", "up", 3).unwrap();
    lists::act(&mut c, tab, "cats", "up", 3).unwrap();
    lists::act(&mut c, tab, "cats", "up", 3).unwrap(); // already first: nothing
    lists::act(&mut c, tab, "cats", "down", 0).unwrap();
    assert_eq!(c.value(cats), "4,2,1");
    assert_eq!(s.device_settings(GLOBAL).unwrap()["cats"], "4,2,1");
    // The column choices are `none` + the active entries; col2 only offers those it lists.
    let labels = |f: &Field| fields::row(&c, tab, f).options;
    assert_eq!(labels(col), ["NO COLUMN", "Four", "Two", "One"]);
    assert_eq!(labels(col2), ["NO COLUMN", "Two"]);
    fields::choose(&mut c, tab, col, 2).unwrap();
    assert_eq!(c.value(col), "2");
    assert_eq!(fields::row(&c, tab, col).current, 2);
    // Deactivating the entry a choice holds clears that choice.
    lists::act(&mut c, tab, "cats", "deactivate", 1).unwrap();
    assert_eq!((c.value(cats).as_str(), c.value(col).as_str()), ("4,1", ""));
    assert_eq!(fields::row(&c, tab, col).current, 0);
    lists::act(&mut c, tab, "cats", "reset", 0).unwrap();
    assert!(s.device_settings(GLOBAL).unwrap().is_empty());
    assert!(lists::act(&mut c, tab, "cats", "activate", -1).is_err());
    assert!(lists::act(&mut c, tab, "cats", "sideways", 1).is_err());
    assert!(lists::act(&mut c, tab, "nope", "up", 1).is_err());
    assert!(lists::act(&mut c, &schema.tabs[0], "cats", "up", 1).is_err());
    assert!(lists::items(&c, &schema.tabs[0]).is_empty());
    // Every List field has its own editor and value.
    lists::act(&mut c, tab, "more", "activate", 1).unwrap();
    lists::act(&mut c, tab, "more", "activate", 0).unwrap();
    assert_eq!(c.value(field_of(&schema, "more")), "y,x");
    assert_eq!(c.value(cats), "1,2");
    let both = lists::items(&c, tab);
    assert_eq!(
        both.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(),
        ["cats", "more"]
    );
    assert_eq!(
        both[1].active.iter().map(|x| x.0).collect::<Vec<_>>(),
        [1, 0]
    );
    assert!(both[1].inactive.is_empty());
}

#[test]
fn reset_forgets_only_the_open_tabs_rows_of_the_scope() {
    let (s, schema) = (Store::in_memory().unwrap(), schema());
    let (name, lang, cats) = (
        field_of(&schema, "name"),
        field_of(&schema, "lang"),
        field_of(&schema, "cats"),
    );
    let mut c = ctx(&s, "d1");
    c.put(name, "X").unwrap();
    c.put(lang, "c").unwrap();
    c.put(cats, "3").unwrap();
    let general: Vec<&Field> = all_fields(&schema.tabs[0]).collect();
    c.reset(general).unwrap();
    assert_eq!(
        s.device_settings("d1").unwrap().keys().collect::<Vec<_>>(),
        ["cats"]
    );
}

#[test]
fn a_file_is_imported_copied_from_the_defaults_and_deleted_with_its_row() {
    let (s, schema) = (Store::in_memory().unwrap(), schema());
    let (tab, pic, use_pic) = (
        &schema.tabs[1],
        field_of(&schema, "pic"),
        field_of(&schema, "use_pic"),
    );
    let dir = temp("files");
    let (jpg, png) = (dir.join("a.jpg"), dir.join("b.png"));
    std::fs::write(&jpg, [1, 2, 3]).unwrap();
    std::fs::write(&png, [4]).unwrap();
    let root = dir.join("store");
    let ctx = |scope| Ctx::new(&s, scope, Some(root.clone()));
    assert!(files::import(&mut ctx(GLOBAL), tab, pic, png.to_str().unwrap()).is_err());
    assert_eq!(
        ctx(GLOBAL).value(use_pic),
        "0",
        "a refused file sets nothing"
    );
    assert_eq!(files::import(&mut ctx(GLOBAL), tab, pic, "").unwrap(), "");
    assert!(
        files::copy(&mut ctx("d1"), pic).is_err(),
        "nothing to copy yet"
    );
    assert_eq!(ctx(GLOBAL).value(use_pic), "0");
    files::import(&mut ctx(GLOBAL), tab, pic, jpg.to_str().unwrap()).unwrap();
    assert_eq!(
        ctx(GLOBAL).value(use_pic),
        "1",
        "the import sets the keys the field names"
    );
    let global = ctx(GLOBAL).value(pic);
    assert_eq!(std::fs::read(&global).unwrap(), [1, 2, 3]);
    // A device follows the defaults' file until it has its own copy.
    assert_eq!(ctx("d1").value(pic), global);
    assert!(!ctx("d1").has_own("pic"));
    files::copy(&mut ctx("d1"), pic).unwrap();
    assert!(
        !ctx("d1").has_own("use_pic"),
        "a copy sets nothing but the file"
    );
    let own = ctx("d1").value(pic);
    assert_ne!(own, global);
    assert_eq!(std::fs::read(&own).unwrap(), [1, 2, 3]);
    files::delete(&mut ctx("d1"), pic).unwrap();
    assert!(!std::path::Path::new(&own).exists());
    assert_eq!(
        ctx("d1").value(pic),
        global,
        "back to following the defaults"
    );
    assert!(std::path::Path::new(&global).exists());
    // A path outside the store (a value read from a device) is never deleted.
    ctx("d1").put(pic, jpg.to_str().unwrap()).unwrap();
    ctx("d1").reset([pic]).unwrap();
    assert!(jpg.exists());
    files::delete(&mut ctx(GLOBAL), pic).unwrap();
    assert!(!std::path::Path::new(&global).exists());
    assert_eq!(ctx(GLOBAL).value(pic), "");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn what_the_player_changed_is_stored_as_the_differences_only() {
    let (s, schema) = (Store::in_memory().unwrap(), schema());
    let lang = field_of(&schema, "lang");
    fields::choose(&mut ctx(&s, GLOBAL), &schema.tabs[0], lang, 0).unwrap(); // the defaults say "a"
    let values = |v: &[(&str, &str)]| {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>()
    };
    let n = connected::import(
        &s,
        "d1",
        &schema,
        &values(&[("lang", "c"), ("cats", "1,2"), ("zzz", "9")]),
    )
    .unwrap();
    assert_eq!(
        n, 1,
        "only lang differs from what d1 had; cats is the schema's own"
    );
    assert_eq!(
        s.device_settings("d1")
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        [("lang".to_owned(), "c".to_owned())]
    );
    // What the defaults already say is no override; a key the device did not report is left alone.
    let n = connected::import(&s, "d1", &schema, &values(&[("cats", "1,2,3")])).unwrap();
    assert_eq!(n, 1);
    assert_eq!(s.device_settings("d1").unwrap().len(), 2);
    let n = connected::import(&s, "d1", &schema, &values(&[("lang", "a"), ("note", "x")])).unwrap();
    assert_eq!(n, 1, "an info line is not a setting");
    assert_eq!(
        s.device_settings("d1").unwrap().keys().collect::<Vec<_>>(),
        ["cats"]
    );
}

#[test]
fn the_device_tab_edits_the_host_flags() {
    let s = Store::in_memory().unwrap();
    let flag = |s: &Store, key: &str| {
        behaviour::rows(s)
            .into_iter()
            .find(|r| r.key == key)
            .unwrap()
            .current
    };
    assert_eq!(flag(&s, "djs.beh.import_history"), 1);
    behaviour::choose(&s, "beh.import_history", 0).unwrap();
    behaviour::choose(&s, "beh.auto_backup", 1).unwrap();
    assert_eq!(flag(&s, "djs.beh.import_history"), 0);
    assert_eq!(flag(&s, "djs.beh.auto_backup"), 1);
    assert!(behaviour::choose(&s, "beh.nope", 1).is_err());
    assert!(behaviour::choose(&s, "reset", 1).is_err());
    // The flags are not settings a device operation receives.
    assert!(device_settings::config(&s, "d1").is_empty());
}
