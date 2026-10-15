//! The current settings, the change listeners and the handlers of action and text rows. UI thread only.

use super::{Prefs, save};
use std::{cell::RefCell, rc::Rc};

type Listener = Rc<dyn Fn(&Prefs)>;

/// Handlers registered under a row key.
struct Keyed<F: ?Sized>(RefCell<Vec<(String, Rc<F>)>>);

impl<F: ?Sized> Keyed<F> {
    const fn new() -> Self {
        Self(RefCell::new(Vec::new()))
    }

    fn add(&self, key: &str, f: Rc<F>) {
        self.0.borrow_mut().push((key.to_owned(), f));
    }

    /// Cloned out, so a handler may register more handlers or edit the settings while it runs.
    fn of(&self, key: &str) -> Vec<Rc<F>> {
        self.0
            .borrow()
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, f)| f.clone())
            .collect()
    }
}

thread_local! {
    static CURRENT: RefCell<Prefs> = RefCell::default();
    static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
    static ACTIONS: Keyed<dyn Fn()> = const { Keyed::new() };
    static TEXTS: Keyed<dyn Fn(&str)> = const { Keyed::new() };
}

/// The current settings (defaults until [`super::wire`] has loaded the file). UI thread only.
pub fn get() -> Prefs {
    CURRENT.with(|c| c.borrow().clone())
}

/// Runs `f` on the current settings without cloning them. `f` must not call [`update`].
pub fn with<R>(f: impl FnOnce(&Prefs) -> R) -> R {
    CURRENT.with_borrow(f)
}

/// Replaces the current settings without saving or notifying (start-up).
pub(super) fn set(p: Prefs) {
    CURRENT.with(|c| *c.borrow_mut() = p);
}

/// Calls `f` after every change (not for a no-op edit), with the new settings.
pub fn on_change(f: impl Fn(&Prefs) + 'static) {
    LISTENERS.with_borrow_mut(|l| l.push(Rc::new(f)));
}

/// Registers the handler of a button row (a [`super::Row`] whose click is an action, not a value, e.g. `played_reset`).
pub fn on_action(key: &str, f: impl Fn() + 'static) {
    ACTIONS.with(|a| a.add(key, Rc::new(f)));
}

/// Runs the handlers registered for `key` (no handler: nothing happens).
pub fn run_action(key: &str) {
    ACTIONS.with(|a| a.of(key)).iter().for_each(|f| f());
}

/// Registers the handler of a text row ([`super::Row::text`]) with key `key`; it gets the accepted text and decides
/// what to do (usually `prefs::update(|p| p.set_extra(..))`).
pub fn on_text(key: &str, f: impl Fn(&str) + 'static) {
    TEXTS.with(|t| t.add(key, Rc::new(f)));
}

pub(super) fn run_text(key: &str, value: &str) {
    TEXTS.with(|t| t.of(key)).iter().for_each(|f| f(value));
}

pub(super) fn update_with(f: impl FnOnce(&mut Prefs), save: impl FnOnce(&Prefs)) {
    let new = CURRENT.with(|c| {
        let mut c = c.borrow_mut();
        let old = c.clone();
        f(&mut c);
        (*c != old).then(|| c.clone())
    });
    if let Some(p) = new {
        save(&p);
        let ls = LISTENERS.with_borrow(Clone::clone);
        ls.iter().for_each(|l| l(&p));
    }
}

/// Edits the settings; when something changed they are saved and the listeners run.
pub fn update(f: impl FnOnce(&mut Prefs)) {
    update_with(f, save);
}

/// Runs the change listeners with the current settings without changing or saving them: the look changed outside [`Prefs`] (a skin
/// file was edited, the theme editor's draft moved), so every window restyles itself.
pub fn notify() {
    let p = get();
    let ls = LISTENERS.with_borrow(Clone::clone);
    ls.iter().for_each(|l| l(&p));
}

/// Sets the current settings for a test without touching the settings file.
#[cfg(test)]
pub fn set_for_test(f: impl FnOnce(&mut Prefs)) {
    update_with(f, |_| {});
}
