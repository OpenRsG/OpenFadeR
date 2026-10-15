//! The two-list editors of a tab: one per List field it shows. An inactive and an active list, an item moves between them with
//! the arrow buttons and is shifted up or down inside the active list. The lists are shown by `DjSystemState`; the field's
//! value is the active entries joined by `,`. The editor's item ids are indices into the field's `items` (its table order).

use super::{Ctx, Done, Lists, all_fields};
use openfader_plugin::library::{Choice, Field, FieldKind, Tab};

/// The List fields the tab shows, with their two titles and their items.
fn fields<'t>(c: &Ctx, tab: &'t Tab) -> Vec<(&'t Field, [&'t str; 2], &'t [Choice])> {
    all_fields(tab)
        .filter(|f| c.shows(f))
        .filter_map(|f| match &f.kind {
            FieldKind::List {
                inactive,
                active,
                items,
            } => Some((f, [inactive.as_str(), active.as_str()], items.as_slice())),
            _ => None,
        })
        .collect()
}

/// The editors of the tab, in field order (none on a tab without a List field).
pub fn items(c: &Ctx, tab: &Tab) -> Vec<Lists> {
    fields(c, tab)
        .into_iter()
        .map(|(f, titles, items)| {
            let entry = |i: usize| (i as i32, items[i].label.clone());
            let active: Vec<usize> = c
                .list(f)
                .iter()
                .filter_map(|v| items.iter().position(|i| i.value == *v))
                .collect();
            Lists {
                key: f.key.clone(),
                titles: titles.map(String::from),
                inactive: (0..items.len())
                    .filter(|i| !active.contains(i))
                    .map(entry)
                    .collect(),
                active: active.into_iter().map(entry).collect(),
            }
        })
        .collect()
}

/// A button of the editor of the List field `key`: `activate` / `deactivate` / `up` / `down` of item `id`, or `reset`.
pub fn act(c: &mut Ctx, tab: &Tab, key: &str, action: &str, id: i32) -> Done {
    let (f, _, items) = fields(c, tab)
        .into_iter()
        .find(|(f, ..)| f.key == key)
        .ok_or("no such list on this tab")?;
    if action == "reset" {
        return c.reset([f]).map(|()| "reset to defaults".into());
    }
    let value = usize::try_from(id)
        .ok()
        .and_then(|i| items.get(i))
        .ok_or("no item selected")?
        .value
        .clone();
    let mut active = c.list(f);
    let at = active.iter().position(|v| *v == value);
    match (action, at) {
        ("activate", None) => active.push(value),
        ("deactivate", Some(a)) => drop(active.remove(a)),
        ("up", Some(a @ 1..)) => active.swap(a, a - 1),
        ("down", Some(a)) if a + 1 < active.len() => active.swap(a, a + 1),
        ("activate" | "deactivate" | "up" | "down", _) => {}
        _ => return Err(format!("unknown action {action}")),
    }
    c.put(f, &active.join(","))?;
    // A choice fed by this list drops an entry that is no longer active.
    for x in all_fields(tab) {
        if let FieldKind::Choice {
            from_list: Some(l), ..
        } = &x.kind
        {
            let v = c.value(x);
            if *l == f.key && !v.is_empty() && !active.contains(&v) {
                c.put(x, "")?;
            }
        }
    }
    Ok(String::new())
}
