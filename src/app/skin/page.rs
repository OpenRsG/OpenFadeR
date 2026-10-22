//! PREFS > THEMES ↔ ui/themes.slint (global `ThemeState`): the gallery of themes, the actions on them (apply, duplicate, delete,
//! import, open the folder, rescan) and the editor over a [`draft`]. Rust owns the state; the editor's rows are updated in place while
//! a slider is dragged (rebuilding them would drop the drag) and the windows restyle at most every 33 ms. [`wire`] also starts the
//! folder watcher, which rescans within a second of a theme file being added, edited or removed.

use super::{
    BUILTIN, Base, Skin, TOKENS, Value, complete, draft, file, found, names, rescan, store,
    values_of,
};
use crate::app::{
    App, ThemeCard, ThemeState, TokenRow,
    library::{Pick, browse, chooser, show_in_folder},
    prefs::{self, view},
};
use slint::{Color, ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    path::Path,
    time::Duration,
};

/// The tokens a gallery card paints its miniature with, in the order ui/themes.slint `ThemeCard.colors` documents.
const PREVIEW: [&str; 17] = [
    "bg",
    "bg-deep",
    "panel",
    "panel-raised",
    "text",
    "text-dim",
    "accent",
    "play",
    "cue",
    "danger",
    "wave-bg",
    "wave-low",
    "wave-mid",
    "wave-high",
    "select",
    "deck1",
    "deck2",
];

thread_local! {
    static APP: RefCell<Option<slint::Weak<App>>> = const { RefCell::new(None) };
    /// A restyle of the windows is already scheduled.
    static PENDING: Cell<bool> = const { Cell::new(false) };
    static DELAY: Timer = Timer::default();
}

fn app() -> Option<App> {
    APP.with_borrow(|a| a.as_ref().and_then(slint::Weak::upgrade))
}

/// The colour of token `name` among `values`.
fn color_of(values: &[Value], name: &str) -> Color {
    TOKENS
        .iter()
        .position(|t| t.0 == name)
        .and_then(|i| match values.get(i) {
            Some(Value::Color(c)) => Some(*c),
            _ => None,
        })
        .unwrap_or_default()
}

/// The chosen skin's name.
pub(super) fn chosen() -> String {
    prefs::with(|p| view::skin_name(p).to_owned())
}

/// The colours a gallery card paints its miniature with, for skin `name` among `skins` (none before a window has been styled).
pub(super) fn colors(skins: &[Skin], name: &str) -> Vec<Color> {
    values_of(skins, name)
        .map(|(_, v)| PREVIEW.iter().map(|n| color_of(&v, n)).collect())
        .unwrap_or_default()
}

/// The gallery's cards, one per skin ([`names`] order), with the chosen one marked.
fn cards(skins: &[Skin], chosen: &str) -> Vec<ThemeCard> {
    names()
        .into_iter()
        .map(|name| {
            let file = skins.iter().find(|s| s.name == name);
            let colors = colors(skins, &name);
            ThemeCard {
                title: name.clone().into(),
                author: file
                    .map_or_else(|| "Catppuccin".into(), |s| s.author.clone())
                    .into(),
                description: file
                    .map_or_else(String::new, Skin::shown_description)
                    .into(),
                user: file.is_some_and(store::is_user),
                active: name.eq_ignore_ascii_case(chosen),
                colors: ModelRc::new(VecModel::from(colors)),
                name: name.into(),
            }
        })
        .collect()
}

/// Rebuilds the gallery and the buttons that depend on the chosen theme (the message and the editor stay).
pub fn refresh() {
    let Some(app) = app() else { return };
    let s = app.global::<ThemeState>();
    let (skins, chosen) = (found(), chosen());
    s.set_cards(ModelRc::new(VecModel::from(cards(&skins, &chosen))));
    s.set_current_user(
        skins
            .iter()
            .any(|k| k.name.eq_ignore_ascii_case(&chosen) && store::is_user(k)),
    );
    s.set_current(chosen.into());
}

fn say(s: &ThemeState, text: impl Into<String>, failed: bool) {
    s.set_message(text.into().into());
    s.set_failed(failed);
}

/// Makes the windows restyle, soon: the draft or a file changed. Calls that come while one is scheduled join it.
fn restyle() {
    if !PENDING.replace(true) {
        DELAY.with(|t| {
            t.start(TimerMode::SingleShot, Duration::from_millis(33), || {
                PENDING.set(false);
                prefs::notify();
            })
        });
    }
}

fn row(r: draft::Row) -> TokenRow {
    TokenRow {
        index: r.index as i32,
        name: r.name.into(),
        hint: r.hint.into(),
        kind: r.kind,
        text: r.text.into(),
        color: r.color,
        changed: r.changed,
    }
}

/// Shows the editor's rows of the open group (a whole new list).
fn show_rows(s: &ThemeState) {
    let rows = draft::with(|d| d.rows(s.get_group().max(0) as usize)).unwrap_or_default();
    s.set_rows(ModelRc::new(VecModel::from(
        rows.into_iter().map(row).collect::<Vec<_>>(),
    )));
}

/// Shows token `i` again where it is in the open group, keeping the other rows (and the drag in progress) as they are.
fn show_row(s: &ThemeState, i: usize) {
    let Some(r) = draft::with(|d| d.row(i)) else {
        return;
    };
    let rows = s.get_rows();
    if let Some(n) =
        (0..rows.row_count()).find(|&n| rows.row_data(n).is_some_and(|x| x.index == i as i32))
    {
        rows.set_row_data(n, row(r));
    }
}

/// Shows the picker's state; the palette (the draft's distinct colours) only when asked, it does not change while a slider moves.
fn show_picker(s: &ThemeState, palette: bool) {
    let Some((picked, hsva, values)) = draft::with(|d| (d.picked, d.hsva, d.values.clone())) else {
        return;
    };
    s.set_picked(picked.map_or(-1, |i| i as i32));
    if let Some(Value::Color(c)) = picked.map(|i| &values[i]) {
        s.set_picked_color(*c);
        s.set_picked_hex(file::hex(*c).into());
    }
    let [h, sat, v, a] = hsva;
    s.set_hue(h);
    s.set_sat(sat);
    s.set_val(v);
    s.set_alpha(a);
    if palette {
        let mut colors: Vec<Color> = Vec::new();
        for v in &values {
            if let Value::Color(c) = v {
                if !colors.contains(c) && colors.len() < 60 {
                    colors.push(*c);
                }
            }
        }
        s.set_palette(ModelRc::new(VecModel::from(colors)));
    }
}

/// Opens or refreshes the whole editor view from the draft.
fn show_editor(s: &ThemeState) {
    s.set_editing(draft::is_open());
    let Some(light) = draft::with(|d| d.base == Base::Light) else {
        return;
    };
    s.set_draft_light(light);
    let groups: Vec<_> = draft::groups()
        .into_iter()
        .map(|g| g.to_uppercase().into())
        .collect();
    s.set_groups(ModelRc::new(VecModel::from(groups)));
    show_rows(s);
    show_picker(s, true);
}

/// Closes the editor (the draft too) and restyles the windows with the chosen skin.
fn close_editor(s: &ThemeState) {
    if draft::is_open() {
        draft::close();
        prefs::notify();
    }
    s.set_editing(false);
    s.set_picked(-1);
}

/// Chooses the skin `name` from the gallery (what the SKIN row does): any open draft is dropped.
pub(super) fn choose(name: &str) {
    let Some(app) = app() else { return };
    let s = app.global::<ThemeState>();
    close_editor(&s);
    let mut bad = Vec::new();
    prefs::update(|p| bad = view::select_skin(p, name));
    refresh();
    prefs::refresh();
    match bad.is_empty() {
        true => say(&s, "", false),
        false => say(
            &s,
            tr!(
                "{}: these layout settings are not valid and were left as they are: {}",
                name,
                bad.join(", ")
            ),
            true,
        ),
    }
}

/// A copy of skin `name` called `copy` (a built-in or bundled skin has every token written out).
fn copy_of(skins: &[Skin], name: &str, copy: &str) -> Option<Skin> {
    let mut out = match skins.iter().find(|s| s.name.eq_ignore_ascii_case(name)) {
        Some(s) => s.clone(),
        None => {
            let (base, values) = values_of(skins, name)?;
            Skin::new(name, base, complete(values))
        }
    };
    out.name = copy.to_owned();
    out.path = None;
    Some(out)
}

fn customize(s: &ThemeState) {
    let (skins, name) = (found(), chosen());
    if draft::is_open() || !draft::open(&skins, &name) {
        return;
    }
    let user = skins
        .iter()
        .find(|k| k.name.eq_ignore_ascii_case(&name))
        .is_some_and(store::is_user);
    let all = names();
    s.set_draft_name(
        if user {
            name
        } else {
            store::copy_name(&name, &all)
        }
        .into(),
    );
    s.set_group(0);
    say(s, "", false);
    show_editor(s);
}

/// Saves the draft as a theme called `name`, makes it the chosen skin and closes the editor.
fn save(s: &ThemeState, name: &str) {
    let Some(skin) = draft::with(|d| d.to_skin(name)) else {
        return;
    };
    let saved = store::ensure_user_dir().and_then(|dir| store::write_into(&dir, &found(), &skin));
    match saved {
        Err(e) => say(s, e, true),
        Ok(path) => {
            draft::close();
            s.set_editing(false);
            // the layout is already what the user has: choosing the saved theme must not set its hints again
            prefs::update(|p| p.set_extra("skin", &skin.name));
            rescan();
            say(s, tr!("Saved {} to {}", skin.name, path.display()), false);
        }
    }
}

fn duplicate(s: &ThemeState) {
    let (skins, name) = (found(), chosen());
    let copy = store::copy_name(&name, &names());
    let made = copy_of(&skins, &name, &copy)
        .ok_or_else(|| tr!("{} cannot be copied now", format!("{name:?}")))
        .and_then(|c| {
            Ok((
                store::write_into(&store::ensure_user_dir()?, &skins, &c)?,
                c,
            ))
        });
    match made {
        Err(e) => say(s, e, true),
        Ok((path, c)) => {
            rescan();
            choose(&c.name);
            say(s, tr!("Made {} in {}", c.name, path.display()), false);
        }
    }
}

fn remove(s: &ThemeState) {
    let name = chosen();
    let done = store::ensure_user_dir().and_then(|dir| store::remove_in(&dir, &found(), &name));
    match done {
        Err(e) => say(s, e, true),
        Ok(()) => {
            close_editor(s);
            prefs::update(|p| p.set_extra("skin", BUILTIN[0]));
            rescan();
            say(s, tr!("Deleted {}; back to {}", name, BUILTIN[0]), false);
        }
    }
}

/// IMPORT: the file chooser runs on a thread (it blocks until the user answers) and the answer is installed on the UI thread.
fn import_file(s: &ThemeState) {
    if chooser().is_none() {
        say(
            s,
            tr!(
                "IMPORT needs zenity or kdialog; without one, put the theme file in the themes folder (OPEN FOLDER)."
            ),
            true,
        );
        return;
    }
    std::thread::spawn(|| {
        let path = browse(Pick::File, "");
        slint::invoke_from_event_loop(move || {
            if let (Some(p), Some(_)) = (path, app()) {
                // the result shows in the page's message line
                let _ = install_and_choose(Path::new(&p));
            }
        })
        .ok();
    });
}

/// Installs the theme file `path` ([`store::install`]: validated, copied into the user's skin folder, folders re-read) and applies
/// it (what a click in the gallery does, layout hints included). Returns the theme's name; the answer is also in PREFS > THEMES's
/// message line. UI thread only; the entry point for a theme file dropped on the window.
pub fn install_and_choose(path: &Path) -> Result<String, String> {
    let done = store::install(path);
    match &done {
        Ok(name) => {
            choose(name);
            if let Some(app) = app() {
                say(&app.global::<ThemeState>(), tr!("Added {}", name), false);
            }
        }
        Err(e) => {
            if let Some(app) = app() {
                say(&app.global::<ThemeState>(), e.clone(), true);
            }
        }
    }
    done
}

fn open_folder(s: &ThemeState) {
    match store::ensure_user_dir() {
        Ok(dir) => show_in_folder(&dir),
        Err(e) => say(s, e, true),
    }
}

/// A theme file edited by hand, added or deleted while the app runs shows within a second: the folders are compared by file name,
/// size and time of change once a second.
fn watch() -> Timer {
    let last = RefCell::new(store::signature(&super::folders()));
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        let now = store::signature(&super::folders());
        if std::mem::replace(&mut *last.borrow_mut(), now.clone()) != now {
            rescan();
        }
    });
    timer
}

/// Connects PREFS > THEMES and starts the folder watcher; keep the returned timer alive for as long as the window runs. Call after
/// `prefs::wire` has styled the window.
pub fn wire(window: &App) -> Timer {
    APP.with_borrow_mut(|a| *a = Some(window.as_weak()));
    let s = window.global::<ThemeState>();
    let on = |f: fn(&ThemeState)| {
        move || {
            if let Some(app) = app() {
                f(&app.global::<ThemeState>());
            }
        }
    };
    s.on_opened(rescan);
    s.on_apply(|n| choose(&n));
    s.on_customize(on(customize));
    s.on_duplicate(on(duplicate));
    s.on_remove(on(remove));
    s.on_import_file(on(import_file));
    s.on_open_folder(on(open_folder));
    s.on_rescan(|| {
        rescan();
        if let Some(app) = app() {
            say(
                &app.global::<ThemeState>(),
                tr!("Theme folders read again"),
                false,
            );
        }
    });
    s.on_save(|n| {
        if let Some(app) = app() {
            save(&app.global::<ThemeState>(), &n);
        }
    });
    s.on_discard(on(close_editor));
    s.on_set_base(|light| {
        draft::with(|d| d.set_base(if light { Base::Light } else { Base::Dark }));
        if let Some(app) = app() {
            let s = app.global::<ThemeState>();
            s.set_draft_light(light);
            show_rows(&s);
        }
        restyle();
    });
    s.on_reset_all(|| {
        draft::with(|d| d.reset_all());
        if let Some(app) = app() {
            let s = app.global::<ThemeState>();
            draft::with(|d| d.pick(d.picked));
            show_rows(&s);
            show_picker(&s, true);
        }
        restyle();
    });
    s.on_group_chosen(|g| {
        if let Some(app) = app() {
            let s = app.global::<ThemeState>();
            s.set_group(g);
            draft::with(|d| d.pick(None));
            show_rows(&s);
            show_picker(&s, false);
        }
    });
    s.on_pick(|i| {
        draft::with(|d| {
            d.pick(if d.picked == Some(i as usize) {
                None
            } else {
                Some(i as usize)
            })
        });
        if let Some(app) = app() {
            show_picker(&app.global::<ThemeState>(), true);
        }
    });
    s.on_edit(|i, text| {
        let Some(app) = app() else { return };
        let s = app.global::<ThemeState>();
        let i = i.max(0) as usize;
        match draft::with(|d| d.set_text(i, &text)) {
            Some(Err(e)) => say(
                &s,
                format!("{}: {e}", TOKENS.get(i).map_or("", |t| t.0)),
                true,
            ),
            _ => say(&s, "", false),
        }
        draft::with(|d| d.pick(d.picked));
        show_row(&s, i);
        show_picker(&s, false);
        restyle();
    });
    s.on_step(|i, dir| {
        draft::with(|d| d.step(i.max(0) as usize, dir));
        if let Some(app) = app() {
            show_row(&app.global::<ThemeState>(), i.max(0) as usize);
        }
        restyle();
    });
    s.on_reset(|i| {
        draft::with(|d| d.reset(i.max(0) as usize));
        if let Some(app) = app() {
            let s = app.global::<ThemeState>();
            show_row(&s, i.max(0) as usize);
            draft::with(|d| d.pick(d.picked));
            show_picker(&s, false);
        }
        restyle();
    });
    s.on_set_hsva(|h, sat, v, a| {
        draft::with(|d| d.set_hsva([h, sat, v, a]));
        if let Some(app) = app() {
            let s = app.global::<ThemeState>();
            if let Some(i) = draft::with(|d| d.picked).flatten() {
                show_row(&s, i);
            }
            show_picker(&s, false);
        }
        restyle();
    });
    s.on_set_color(|c| {
        draft::with(|d| d.set_picked(c));
        if let Some(app) = app() {
            let s = app.global::<ThemeState>();
            if let Some(i) = draft::with(|d| d.picked).flatten() {
                show_row(&s, i);
            }
            show_picker(&s, false);
        }
        restyle();
    });
    prefs::on_change(|_| {
        if !draft::is_open() {
            refresh();
        }
    });
    refresh();
    watch()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cards_miniature_uses_colour_tokens_that_exist() {
        for n in PREVIEW {
            let t = TOKENS.iter().find(|t| t.0 == n);
            assert_eq!(t.map(|t| t.1), Some(super::super::Ty::Color), "{n}");
        }
    }

    fn window() -> crate::app::App {
        crate::app::headless::init();
        let app = crate::app::App::new().unwrap();
        super::super::restyle(&app, &[], BUILTIN[0]);
        app
    }

    #[test]
    fn the_gallery_has_a_painted_card_per_theme_and_marks_the_chosen_one() {
        let _app = window();
        let skins = found();
        let cards = cards(&skins, &BUILTIN[1].to_uppercase());
        assert_eq!(cards.len(), names().len());
        // The OpenFadeR-Themes checkout in `extension/skin/` (start.sh fetches it) adds its themes after the built-ins.
        let themes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extension/skin");
        let themes = themes.is_dir();
        assert!(
            !themes || cards.len() > 10,
            "the built-ins and the bundled themes"
        );
        assert!(cards.iter().all(|c| c.colors.row_count() == PREVIEW.len()));
        let active: Vec<_> = cards
            .iter()
            .filter(|c| c.active)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(active, [BUILTIN[1]], "case-insensitive");
        assert_eq!(cards[0].title, BUILTIN[0]);
        assert!(
            !themes
                || cards
                    .iter()
                    .any(|c| c.title == "Rek" && !c.user && !c.author.is_empty())
        );
    }

    #[test]
    fn a_copy_of_a_built_in_theme_lists_every_token_and_a_file_theme_keeps_its_hints() {
        let _app = window();
        let c = copy_of(&[], BUILTIN[1], "Mine").unwrap();
        assert_eq!(
            (c.name.as_str(), c.base, c.tokens.len(), c.path.is_none()),
            ("Mine", Base::Light, TOKENS.len(), true)
        );
        assert!(copy_of(&[], "nope", "x").is_none());
        let mut s = Skin::new(
            "Sample",
            Base::Dark,
            vec![("accent", Value::Color(Color::from_rgb_u8(1, 2, 3)))],
        );
        s.layout = vec![("deck_layout".into(), "vertical".into())];
        s.path = Some("/x/sample.json".into());
        let c = copy_of(&[s], "sample", "Sample copy").unwrap();
        assert_eq!((c.tokens.len(), c.layout.len(), c.path), (1, 1, None));
    }
}
