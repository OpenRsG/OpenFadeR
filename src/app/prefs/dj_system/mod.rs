//! PREFERENCES > DJ SYSTEM ↔ `ui/prefs-djsystem.slint` (global `DjSystemState`): the settings of the devices, drawn from the
//! schemas the device plugins answer (`Provider::schema`). Nothing here knows a setting: every tab, group header, label,
//! choice and default is the plugin's.
//!
//! The settings are not in `settings.conf` but in the store (`store::device_settings`): the same page edits either the
//! **defaults of every device** (scope [`GLOBAL`], what PREFERENCES shows: the tabs and fields of scope `Global` / `Both`) or
//! the overrides of one device ([`open_scope`]; scope `Device` / `Both`). A device without an override follows the defaults.
//! A tab is a plugin's schema tab; the host adds one tab of its own, DEVICE ([`behaviour`]), once any provider exists.
//!
//! Fields by kind: Choice → chips or drop-down ([`fields`]), Text, Bool, Info → one row each; List → the two-list editor
//! [`DjSystemState`] carries ([`lists`]); File → import / delete / copy rows ([`files`]). Per tab: "Reset to defaults", the
//! plugin's `apply_label` button (Preferences) and `read_label` button (a device), and its `auto_key` ([`connected`], the
//! work with the connected devices). Row keys start with `djs.`.
//!
//! Children: [`fields`], [`lists`], [`files`], [`behaviour`], [`connected`]; each depends on this base only.

mod behaviour;
mod connected;
mod fields;
mod files;
mod lists;
#[cfg(test)]
mod tests;

pub use connected::sync_from_device;

use super::{Prefs, Row};
use crate::app::{App, DjEditor, DjItem, DjSystemState};
use openfader::device::{self, Device};
use openfader::plugin::device::{self as plugin, Provider};
use openfader::store::{
    self, Store,
    device_settings::{self, GLOBAL},
};
use openfader_plugin::library::{Field, FieldKind, Scope, Tab};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::RefCell, collections::BTreeMap, path::PathBuf, rc::Rc};

/// What an action reports: the status line text, or why it failed.
type Done = Result<String, String>;

/// A finished action without a status message.
fn done(r: Result<(), String>) -> Done {
    r.map(|()| String::new())
}

/// One tab of the page.
enum Page {
    /// The host's own tab: [`behaviour`].
    Device,
    Schema {
        provider: Provider,
        tab: Tab,
    },
}

struct Entry {
    label: String,
    page: Page,
}

struct State {
    scope: String,
    tab: usize,
    /// Result of the last action, shown at the top of the tab.
    status: String,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { scope: GLOBAL.into(), tab: 0, status: String::new() });
    /// The list editors of the open tab: one model for the window's life, so that an edit does not rebuild the widgets.
    static EDITORS: Rc<VecModel<DjEditor>> = Rc::default();
}

/// Whether something of scope `s` is shown for the defaults (`device` false) or for one device.
fn shows(s: Scope, device: bool) -> bool {
    match s {
        Scope::Both => true,
        Scope::Global => !device,
        Scope::Device => device,
    }
}

/// Every field of `tab`.
fn all_fields(tab: &Tab) -> impl Iterator<Item = &Field> {
    tab.groups.iter().flat_map(|g| &g.fields)
}

/// The field `key` of `tab`.
fn find<'t>(tab: &'t Tab, key: &str) -> Option<&'t Field> {
    all_fields(tab).find(|f| f.key == key)
}

/// What a page needs to build its rows and handle a click: the store, the scope and its rows. Effective value of a field =
/// the schema's default, then the global row, then the scope's own row.
pub struct Ctx<'a> {
    pub store: &'a Store,
    pub scope: &'a str,
    /// Where file settings are copied (`None` = nowhere: no data directory).
    pub files: Option<PathBuf>,
    global: BTreeMap<String, String>,
    own: BTreeMap<String, String>,
}

impl<'a> Ctx<'a> {
    pub fn new(store: &'a Store, scope: &'a str, files: Option<PathBuf>) -> Self {
        let rows = |s: &str| {
            store.device_settings(s).unwrap_or_else(|e| {
                eprintln!("device settings: {e}");
                BTreeMap::new()
            })
        };
        let global = rows(GLOBAL);
        let own = if scope == GLOBAL {
            global.clone()
        } else {
            rows(scope)
        };
        Self {
            store,
            scope,
            files,
            global,
            own,
        }
    }

    pub fn is_device(&self) -> bool {
        self.scope != GLOBAL
    }

    /// Whether the page shows `f` in this scope.
    fn shows(&self, f: &Field) -> bool {
        shows(f.scope, self.is_device())
    }

    /// What the scope has when it overrides nothing: the default, and for a device the global row.
    fn base(&self, f: &Field) -> String {
        self.is_device()
            .then(|| self.global.get(&f.key))
            .flatten()
            .unwrap_or(&f.default)
            .clone()
    }

    pub fn value(&self, f: &Field) -> String {
        self.own
            .get(&f.key)
            .cloned()
            .unwrap_or_else(|| self.base(f))
    }

    /// Whether the scope overrides `key`.
    fn has_own(&self, key: &str) -> bool {
        self.own.contains_key(key)
    }

    /// The entries (values) of a List field, in order.
    fn list(&self, f: &Field) -> Vec<String> {
        self.value(f)
            .split(',')
            .filter(|v| !v.is_empty())
            .map(String::from)
            .collect()
    }

    /// Stores `f = value` for the scope, or forgets the override when it equals what the scope would have anyway.
    pub fn put(&mut self, f: &Field, value: &str) -> Result<(), String> {
        if value == self.base(f) {
            self.store.clear_device_setting(self.scope, &f.key)?;
            self.own.remove(&f.key);
        } else {
            self.store.set_device_setting(self.scope, &f.key, value)?;
            self.own.insert(f.key.clone(), value.to_owned());
        }
        Ok(())
    }

    /// Forgets the overrides of the scope for `fields` ("Reset to defaults"), and the copy of a file the scope imported.
    pub fn reset<'f>(&mut self, fields: impl IntoIterator<Item = &'f Field>) -> Result<(), String> {
        for f in fields {
            if let (FieldKind::File { .. }, Some(root), Some(path)) =
                (&f.kind, &self.files, self.own.get(&f.key))
            {
                // Only our own copies: a stored path may point anywhere else (a value read from a device).
                if std::path::Path::new(path).starts_with(root) {
                    let _ = std::fs::remove_file(path);
                }
            }
            self.store.clear_device_setting(self.scope, &f.key)?;
            self.own.remove(&f.key);
        }
        Ok(())
    }
}

/// The tabs of `scope`: for the defaults every provider's, for a device those of its provider.
fn pages(scope: &str, devices: &[Device]) -> Vec<Entry> {
    let providers = match devices.iter().find(|d| d.id == scope) {
        Some(d) => d.provider().into_iter().collect(),
        None => plugin::providers(),
    };
    let named = providers.len() > 1;
    let mut pages = Vec::new();
    for p in &providers {
        // A provider without settings answers an error: it has no tab.
        let Ok(schema) = p.schema() else { continue };
        for tab in schema
            .tabs
            .into_iter()
            .filter(|t| shows(t.scope, scope != GLOBAL))
        {
            let label = if named {
                format!("{} {}", p.name(), tab.label)
            } else {
                tab.label.clone()
            };
            pages.push(Entry {
                label: label.to_uppercase(),
                page: Page::Schema {
                    provider: p.clone(),
                    tab,
                },
            });
        }
    }
    if scope == GLOBAL && !providers.is_empty() {
        pages.push(Entry {
            label: "DEVICE".into(),
            page: Page::Device,
        });
    }
    pages
}

/// The rows of a schema tab: its groups, then the buttons.
fn schema_rows(c: &Ctx, tab: &Tab) -> Vec<Row> {
    let mut rows = Vec::new();
    for g in &tab.groups {
        let shown: Vec<&Field> = g.fields.iter().filter(|f| c.shows(f)).collect();
        if shown.is_empty() {
            continue;
        }
        if !g.label.is_empty() {
            rows.push(Row::header(&g.label));
        }
        for f in shown {
            match f.kind {
                FieldKind::List { .. } => {} // the editor above the rows
                FieldKind::File { .. } => rows.extend(files::rows(c, f)),
                _ => rows.push(fields::row(c, tab, f)),
            }
        }
    }
    let button = |key: &str, label: &str, text: &str| Row::chips(key, label, &[text], usize::MAX);
    if let Some(l) = tab.apply_label.as_deref().filter(|_| !c.is_device()) {
        rows.push(button("djs.apply", l, "APPLY"));
    }
    if let Some(l) = tab.read_label.as_deref().filter(|_| c.is_device()) {
        rows.push(button("djs.read", l, "READ"));
    }
    rows.push(button("djs.reset", "Reset to defaults", "RESET"));
    rows
}

/// What one list editor shows.
pub struct Lists {
    /// The List field's key.
    pub key: String,
    /// Titles of the inactive and the active list.
    pub titles: [String; 2],
    /// `(index in the field's items, label)`.
    pub inactive: Vec<(i32, String)>,
    pub active: Vec<(i32, String)>,
}

fn snapshot() -> (String, usize, String) {
    STATE.with(|s| {
        let s = s.borrow();
        (s.scope.clone(), s.tab, s.status.clone())
    })
}

/// Index of the open tab among `n`.
fn open(tab: usize, n: usize) -> usize {
    tab.min(n.saturating_sub(1))
}

/// The rows of the open tab (the dialog calls this after every click); also refreshes `DjSystemState`.
pub fn rows(_: &Prefs) -> Vec<Row> {
    let (scope, tab, status) = snapshot();
    let devices = device::scan();
    let pages = pages(&scope, &devices);
    let i = open(tab, pages.len());
    let store = store::global();
    let c = Ctx::new(&store, &scope, device_settings::files_dir());
    let (mut rows, lists) = match pages.get(i).map(|e| &e.page) {
        None => (
            vec![Row::info(
                "No device plugin",
                "install or enable one in EXTENSIONS",
            )],
            Vec::new(),
        ),
        Some(Page::Device) => (behaviour::rows(&store), Vec::new()),
        Some(Page::Schema { tab, .. }) => (schema_rows(&c, tab), lists::items(&c, tab)),
    };
    if !status.is_empty() {
        rows.insert(0, Row::info("STATUS", &status));
    }
    if scope == GLOBAL && i == 0 && !pages.is_empty() {
        rows.splice(0..0, connected::rows(&devices));
    }
    show(
        &scope,
        &devices,
        pages.iter().map(|e| e.label.as_str()),
        i,
        lists,
    );
    rows
}

/// Runs `f` on the open tab with a fresh context and remembers its outcome for the status line.
fn run(f: impl FnOnce(&mut Ctx, &Page, &[Device]) -> Done) {
    let (scope, tab, _) = snapshot();
    let devices = device::scan();
    let pages = pages(&scope, &devices);
    let store = store::global();
    let mut c = Ctx::new(&store, &scope, device_settings::files_dir());
    let r = pages
        .get(open(tab, pages.len()))
        .map_or(Ok(String::new()), |e| f(&mut c, &e.page, &devices));
    STATE.with(|s| s.borrow_mut().status = r.unwrap_or_else(|e| format!("ERROR: {e}")));
}

/// A successful edit of a device's setting is written to the connected device at once when its tab says so
/// ([`connected::push`]).
fn pushed(c: &Ctx, page: &Page, devices: &[Device], r: Done) -> Done {
    if let (Ok(_), Page::Schema { provider, tab }) = (&r, page) {
        connected::push(c, provider, tab, devices)?;
    }
    r
}

/// Opens the editor on `scope`'s first tab.
fn go(scope: &str) {
    STATE.with(|s| {
        *s.borrow_mut() = State {
            scope: scope.to_owned(),
            tab: 0,
            status: String::new(),
        }
    });
}

/// One click on the row `key` (`djs.…`); false = not ours.
pub fn choose(_: &mut Prefs, key: &str, i: usize) -> bool {
    let Some(key) = key.strip_prefix("djs.") else {
        return false;
    };
    if let Some(id) = key.strip_prefix("dev.") {
        go(id); // "DEVICE SETTINGS" of a connected device: the same editor on that device's overrides
        return true;
    }
    run(|c, page, devices| {
        let Page::Schema { provider, tab } = page else {
            return match key {
                "reset" => done(device_settings::Behaviour::reset(c.store)),
                _ => behaviour::choose(c.store, key, i),
            };
        };
        let r = match key {
            "apply" => return connected::apply(c, provider, tab, devices),
            "read" => return connected::read(c, provider, devices),
            "reset" => {
                let fs: Vec<&Field> = all_fields(tab)
                    .filter(|f| c.shows(f) && f.kind != FieldKind::Info)
                    .collect();
                done(c.reset(fs))
            }
            _ => {
                let (kind, field) = key.split_once('.').ok_or("unknown row")?;
                let f = find(tab, field).ok_or("unknown field")?;
                match (kind, &f.kind) {
                    ("f", FieldKind::Choice { .. } | FieldKind::Bool) => {
                        fields::choose(c, tab, f, i)
                    }
                    ("del", FieldKind::File { .. }) => files::delete(c, f),
                    ("copy", FieldKind::File { .. }) => files::copy(c, f),
                    _ => Err(format!("{field}: not clickable")),
                }
            }
        };
        pushed(c, page, devices, r)
    });
    true
}

/// A text row was accepted (`djs.f.<key>`, `djs.beh.<name>`).
pub fn text(key: &str, value: &str) {
    let Some(key) = key.strip_prefix("djs.") else {
        return;
    };
    run(|c, page, devices| {
        let Page::Schema { tab, .. } = page else {
            let name = key.strip_prefix("beh.").ok_or("unknown row")?;
            return done(device_settings::Behaviour::set(c.store, name, value.trim()));
        };
        let f = key
            .strip_prefix("f.")
            .and_then(|k| find(tab, k))
            .ok_or("unknown field")?;
        let r = match f.kind {
            FieldKind::Text => fields::text(c, f, value),
            FieldKind::File { .. } => files::import(c, tab, f, value.trim()),
            _ => Err(format!("{}: not a text field", f.key)),
        };
        pushed(c, page, devices, r)
    });
}

fn show<'a>(
    scope: &str,
    devices: &[Device],
    tabs: impl Iterator<Item = &'a str>,
    open: usize,
    lists: Vec<Lists>,
) {
    let Some(app) = crate::app::prefs::dialog::app() else {
        return;
    };
    let s = app.global::<DjSystemState>();
    s.set_tabs(ModelRc::new(VecModel::from(
        tabs.map(Into::into).collect::<Vec<slint::SharedString>>(),
    )));
    s.set_tab(open as i32);
    s.set_title(if scope == GLOBAL {
        "DEFAULTS FOR EVERY DEVICE".into()
    } else {
        let name = devices
            .iter()
            .find(|d| d.id == scope)
            .map_or_else(|| scope.to_owned(), |d| format!("{} ({})", d.label, d.id));
        format!("DEVICE {name}").into()
    });
    s.set_device(scope != GLOBAL);
    sync_editors(lists);
}

/// Brings the editors' model in line with `lists`, row by row: an editor that stays keeps its widget (and the user's
/// selection in it); from the first one that differs on, the rest are rebuilt.
fn sync_editors(lists: Vec<Lists>) {
    let items = |v: Vec<(i32, String)>| {
        ModelRc::new(VecModel::from(
            v.into_iter()
                .map(|(id, text)| DjItem {
                    id,
                    text: text.into(),
                })
                .collect::<Vec<_>>(),
        ))
    };
    EDITORS.with(|m| {
        let n = lists.len();
        for (i, l) in lists.into_iter().enumerate() {
            let [inactive_title, active_title] = l.titles;
            let row = DjEditor {
                key: l.key.into(),
                inactive_title: inactive_title.into(),
                active_title: active_title.into(),
                inactive: items(l.inactive),
                active: items(l.active),
            };
            if m.row_data(i).is_some_and(|r| r.key != row.key) {
                (i..m.row_count()).rev().for_each(|j| drop(m.remove(j)));
            }
            if i < m.row_count() {
                m.set_row_data(i, row);
            } else {
                m.push(row);
            }
        }
        (n..m.row_count()).rev().for_each(|j| drop(m.remove(j)));
    });
}

/// Connects the page's callbacks; call once from `prefs::wire`. Text rows are routed by [`text`] (the dialog calls it).
pub fn init(app: &App) {
    let s = app.global::<DjSystemState>();
    EDITORS.with(|m| s.set_editors(m.clone().into()));
    s.on_tab_chosen(|i| {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.tab = i.max(0) as usize;
            s.status.clear();
        });
        super::refresh();
    });
    s.on_list_action(|key, action, id| {
        run(|c, page, devices| {
            let Page::Schema { tab, .. } = page else {
                return Err("no list on this tab".into());
            };
            let r = lists::act(c, tab, &key, &action, id);
            pushed(c, page, devices, r)
        });
        super::refresh();
    });
    s.on_defaults(|| {
        go(GLOBAL);
        super::refresh();
    });
}

/// Opens PREFERENCES on DJ SYSTEM for one device (`scope` = `device::Device::id`; [`GLOBAL`] = the defaults).
pub fn open_scope(app: &App, scope: &str) {
    go(scope);
    let p = app.global::<crate::app::PrefsState>();
    p.set_category(6);
    p.set_visible(true);
    super::refresh();
}
