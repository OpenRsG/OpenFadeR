//! Choice, Text, Bool and Info rows of a schema tab (the other kinds: [`super::lists`], [`super::files`]).
//!
//! A Choice with `from_list` offers `none` and the active entries of that List field of the tab, in the list's order; when the
//! field also names `choices`, only the active entries that are among them (a list may hold entries the field cannot show).

use super::{Ctx, Done, Row, all_fields, done};
use openfader_plugin::library::{Choice, Field, FieldKind, Tab};

/// The options of a Choice field, in the order shown.
fn choices(c: &Ctx, tab: &Tab, f: &Field) -> Vec<Choice> {
    let FieldKind::Choice {
        choices,
        from_list: Some(list),
        none,
    } = &f.kind
    else {
        return match &f.kind {
            FieldKind::Choice { choices, .. } => choices.clone(),
            _ => Vec::new(),
        };
    };
    let none = Choice {
        value: String::new(),
        label: none.clone().unwrap_or_else(|| "NONE".into()),
    };
    let Some(list) = all_fields(tab).find(|l| l.key == *list) else {
        return vec![none];
    };
    let FieldKind::List { items, .. } = &list.kind else {
        return vec![none];
    };
    let active = c.list(list);
    let allowed = |v: &str| choices.is_empty() || choices.iter().any(|c| c.value == v);
    std::iter::once(none)
        .chain(
            active
                .iter()
                .filter(|v| allowed(v))
                .filter_map(|v| items.iter().find(|i| i.value == *v))
                .cloned(),
        )
        .collect()
}

pub fn row(c: &Ctx, tab: &Tab, f: &Field) -> Row {
    let key = format!("djs.f.{}", f.key);
    let v = c.value(f);
    match &f.kind {
        FieldKind::Choice { from_list, .. } => {
            let options = choices(c, tab, f);
            let names: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
            // A value the options do not list is shown as none selected; a list's stale value reads as `none`.
            let cur = options
                .iter()
                .position(|o| o.value == v)
                .unwrap_or(if from_list.is_some() { 0 } else { usize::MAX });
            let few =
                names.len() <= 3 && names.iter().map(|n| n.chars().count()).sum::<usize>() <= 28;
            let row = if few {
                Row::chips(&key, &f.label, &names, cur)
            } else {
                Row::dropdown(&key, &f.label, &names, cur)
            };
            row.hint(&f.hint)
        }
        FieldKind::Bool => Row::check(&key, &f.label, v == "1").hint(&f.hint),
        FieldKind::Text => Row::text(&key, &f.label, &v),
        FieldKind::Info => Row::info(&f.label, &f.hint),
        FieldKind::List { .. } | FieldKind::File { .. } => Row::info(&f.label, &v),
    }
}

/// The click on option `i` of a Choice or Bool row.
pub fn choose(c: &mut Ctx, tab: &Tab, f: &Field, i: usize) -> Done {
    let v = match &f.kind {
        FieldKind::Choice { .. } => {
            choices(c, tab, f)
                .into_iter()
                .nth(i)
                .ok_or("no such choice")?
                .value
        }
        FieldKind::Bool => i.min(1).to_string(),
        _ => return Err(format!("{}: not a choice", f.key)),
    };
    done(c.put(f, &v))
}

/// A text row was accepted; an empty entry restores the default.
pub fn text(c: &mut Ctx, f: &Field, v: &str) -> Done {
    let v = v.trim();
    done(if v.is_empty() {
        c.reset([f])
    } else {
        c.put(f, v)
    })
}
