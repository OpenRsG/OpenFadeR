//! Building blocks for typed settings sections: [`Choice`] enums ([`choice!`]), [`Setting`] values and the
//! [`section!`] macro that turns a field list into a struct with defaults, file parsing / writing and dialog clicks.

/// A closed set of settings values with file names, in the order the dialog lists them.
pub trait Choice: Copy + PartialEq + 'static {
    const ALL: &'static [Self];
    fn name(self) -> &'static str;
}

macro_rules! choice {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub enum $name { $($(#[$vm])* $v),+ }
        impl $crate::app::prefs::Choice for $name {
            const ALL: &'static [Self] = &[$(Self::$v),+];
            fn name(self) -> &'static str { match self { $(Self::$v => $s),+ } }
        }
    };
}
pub(crate) use choice;

/// A typed group of settings lines owned by one dialog category.
pub trait Section {
    /// Reads `k = v`: `None` = not this section's key, `Some(false)` = invalid value (the default is kept).
    fn parse(&mut self, k: &str, v: &str) -> Option<bool>;
    /// Calls `line(key, value)` for every setting, in file order.
    fn write(&self, line: &mut dyn FnMut(&str, &str));
}

/// One value of a [`section!`] field: how it is read from and written to the file and how a dialog click on
/// option `i` sets it (a checkbox: option 1 = checked; a [`Choice`]: its index; others ignore clicks).
pub trait Setting: Sized {
    fn parse(v: &str) -> Option<Self>;
    fn text(&self) -> String;
    fn pick(&mut self, _i: usize) {}
}

impl Setting for bool {
    fn parse(v: &str) -> Option<bool> {
        v.parse().ok()
    }
    fn text(&self) -> String {
        self.to_string()
    }
    fn pick(&mut self, i: usize) {
        *self = i == 1;
    }
}

impl<T: Choice> Setting for T {
    fn parse(v: &str) -> Option<T> {
        super::choice(v)
    }
    fn text(&self) -> String {
        self.name().to_owned()
    }
    fn pick(&mut self, i: usize) {
        if let Some(&v) = T::ALL.get(i) {
            *self = v;
        }
    }
}

/// Numbers are set by the owner's own `choose` (the dialog offers fixed steps), the file holds the plain number.
macro_rules! number_setting {
    ($($t:ty),*) => {$(
        impl Setting for $t {
            fn parse(v: &str) -> Option<$t> { v.parse().ok() }
            fn text(&self) -> String { self.to_string() }
        }
    )*};
}
number_setting!(u8, u16, u32, i32);

/// Free text (a device name, a folder); an empty value is valid.
impl Setting for String {
    fn parse(v: &str) -> Option<String> {
        Some(v.to_owned())
    }
    fn text(&self) -> String {
        self.clone()
    }
}

/// `section!(Name { field: Type = default; … })`: a settings struct whose fields are stored under their own
/// names (snake_case) in the settings file; also generates `pick(key, option) -> bool` for dialog clicks.
macro_rules! section {
    ($(#[$m:meta])* $name:ident { $($(#[$fm:meta])* $f:ident : $t:ty = $d:expr ;)* }) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq)]
        pub struct $name { $($(#[$fm])* pub $f: $t),* }
        impl Default for $name {
            fn default() -> Self { Self { $($f: $d),* } }
        }
        impl $crate::app::prefs::Section for $name {
            fn parse(&mut self, k: &str, v: &str) -> Option<bool> {
                match k {
                    $(stringify!($f) => Some(match <$t as $crate::app::prefs::Setting>::parse(v) {
                        Some(x) => { self.$f = x; true }
                        None => { self.$f = $d; false }
                    }),)*
                    _ => None,
                }
            }
            fn write(&self, line: &mut dyn FnMut(&str, &str)) {
                $(line(stringify!($f), &$crate::app::prefs::Setting::text(&self.$f));)*
            }
        }
        impl $name {
            /// A dialog click on option `i` of the row `key`; false = not a field of this section.
            #[allow(dead_code)] // not every section routes clicks through it
            pub fn pick(&mut self, key: &str, i: usize) -> bool {
                match key {
                    $(stringify!($f) => $crate::app::prefs::Setting::pick(&mut self.$f, i),)*
                    _ => return false,
                }
                true
            }
        }
    };
}
pub(crate) use section;
