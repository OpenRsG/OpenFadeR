//! The theme cards of GET THEMES: install and apply go through [`skin::get`], which writes with the skin store like every other way
//! of adding a theme, so the gallery above shows the theme at once.

use super::{CatalogState, STATE, State, app, say};
use crate::app::{GetTheme, skin};
use openfader::catalog::Repo;
use skin::{Skin, get};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

/// Every theme of the repositories read, in list order; a name is listed once (the first repository's).
fn all(st: &State) -> Vec<(&Repo, &Skin)> {
    let mut out: Vec<(&Repo, &Skin)> = Vec::new();
    for (repo, read) in &st.repos {
        for skin in read.iter().flat_map(|s| &s.themes) {
            if !out
                .iter()
                .any(|(_, o)| o.name.eq_ignore_ascii_case(&skin.name))
            {
                out.push((repo, skin));
            }
        }
    }
    out
}

fn code(a: get::Action) -> i32 {
    match a {
        get::Action::Install => 0,
        get::Action::Update => 1,
        get::Action::Installed => 2,
    }
}

pub(super) fn views(st: &State) -> Vec<GetTheme> {
    all(st)
        .into_iter()
        .map(|(repo, skin)| GetTheme {
            name: skin.name.as_str().into(),
            author: if skin.author.is_empty() {
                repo.to_string()
            } else {
                skin.author.clone()
            }
            .into(),
            description: skin.description.as_str().into(),
            repo: repo.to_string().into(),
            action: code(get::action(skin)),
            active: get::is_chosen(&skin.name),
            colors: ModelRc::new(VecModel::from(get::colors(skin))),
        })
        .collect()
}

/// The theme of card `i`.
fn nth(i: i32) -> Option<Skin> {
    let at = usize::try_from(i).ok()?;
    STATE.with_borrow(|st| all(st).get(at).map(|(_, s)| (*s).clone()))
}

/// INSTALL / UPDATE of card `i`.
pub(super) fn install(i: i32) {
    let (Some(app), Some(skin)) = (app(), nth(i)) else {
        return;
    };
    let s = app.global::<CatalogState>();
    if s.get_busy() {
        return;
    }
    match get::install(&skin) {
        Ok(name) => say(
            &s,
            &tr!(
                "{} installed: it is in the gallery above, APPLY to use it",
                name
            ),
            false,
        ),
        Err(e) => say(&s, &e, true),
    }
    super::show(&app);
}

/// APPLY of card `i`.
pub(super) fn apply(i: i32) {
    if let Some(skin) = nth(i) {
        get::apply(&skin.name);
    }
}

/// The applied theme changed (the gallery, VIEW > Color, a setting): the cards' APPLIED marks follow, the rest stays.
pub(super) fn mark_active() {
    let Some(app) = app() else { return };
    let model = app.global::<CatalogState>().get_themes();
    for i in 0..model.row_count() {
        if let Some(mut t) = model.row_data(i) {
            let now = get::is_chosen(&t.name);
            if t.active != now {
                t.active = now;
                model.set_row_data(i, t);
            }
        }
    }
}
