//! GET THEMES (PREFS > THEMES, `app/catalog`): the themes a repository holds, as the gallery sees them. [`parse`] reads a candidate
//! file with the skin parser, [`action`] says whether it is installed, [`install`] writes it through the same [`store::write_into`] the
//! editor, IMPORT and the file drop use (so a built-in or bundled name is refused), and [`apply`] chooses it. UI thread only, like
//! the rest of the skin code (the folders are the thread's [`found`]); the repository itself was fetched on a worker thread.

use super::{BUILTIN, Skin, found, page, rescan, store};
use openfader::catalog::themes::File;
use slint::Color;

/// What the button of a theme card does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Install,
    /// Installed (a theme of the user's) but the repository's copy differs.
    Update,
    Installed,
}

/// The theme in a repository file; `None` when the file is no theme (it sets no token).
pub fn parse(f: &File) -> Option<Skin> {
    let (skin, _) = super::file::parse(&f.text, &f.stem).ok()?;
    (!skin.tokens.is_empty()).then_some(skin)
}

/// Whether `remote` is there already: a built-in or bundled theme of that name cannot be replaced, so it counts as installed.
pub fn action(remote: &Skin) -> Action {
    let same = |n: &str| n.eq_ignore_ascii_case(&remote.name);
    if BUILTIN.into_iter().any(same) {
        return Action::Installed;
    }
    match found().iter().find(|s| same(&s.name)) {
        None => Action::Install,
        Some(s)
            if store::is_user(s)
                && Skin {
                    path: None,
                    ..s.clone()
                } != *remote =>
        {
            Action::Update
        }
        Some(_) => Action::Installed,
    }
}

/// The miniature's colours, as the gallery paints them.
pub fn colors(remote: &Skin) -> Vec<Color> {
    page::colors(std::slice::from_ref(remote), &remote.name)
}

/// Whether `name` is the chosen theme.
pub fn is_chosen(name: &str) -> bool {
    page::chosen().eq_ignore_ascii_case(name)
}

/// Writes `remote` into the user's skin folder (replacing the user's own theme of that name) and re-reads the folders: the
/// gallery and the pickers show it at once. Returns the theme's name.
pub fn install(remote: &Skin) -> Result<String, String> {
    let dir = store::ensure_user_dir()?;
    store::write_into(&dir, &found(), remote)?;
    rescan();
    Ok(remote.name.clone())
}

/// Chooses theme `name` (what a click in the gallery does).
pub fn apply(name: &str) {
    page::choose(name);
}
