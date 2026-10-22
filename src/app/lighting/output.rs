//! The UDP output (Art-Net / sACN), the status line and saving `lighting.json`.

use super::{Lighting, index_of};
use openfader::lighting::{
    config,
    output::{Output, Protocol},
};

impl Lighting {
    /// Saves settings and patch; a failure goes to the status line.
    pub(super) fn changed(&self) {
        if let Some(p) = config::path() {
            if let Err(e) = config::save(&p, &self.cfg.borrow()) {
                self.say(&tr!("settings not saved: {}", e));
            }
        }
    }

    /// (Re)opens the UDP output for the saved protocol and IP.
    pub(super) fn reopen(&self) {
        let c = self.cfg.borrow();
        self.out_err.replace(String::new());
        self.out.replace(None);
        if c.protocol != Protocol::Off {
            match Output::new(c.protocol, &c.ip) {
                Ok(o) => _ = self.out.replace(Some(o)),
                Err(e) => _ = self.out_err.replace(e.to_string()),
            }
        }
        self.state(|s| s.set_protocol(index_of(&Protocol::ALL, &c.protocol)));
        drop(c);
        self.show_status(0);
    }

    pub(super) fn show_status(&self, universes: usize) {
        let c = self.cfg.borrow();
        let text = match (&*self.out_err.borrow(), c.protocol) {
            (e, _) if !e.is_empty() => e.clone(),
            (_, Protocol::Off) => {
                tr!("Output off: nothing is sent (the stage view still runs).").into()
            }
            _ => tr!(
                "Sending {} to {} · {} universe(s) · 40 Hz",
                c.protocol.label(),
                c.ip,
                universes
            ),
        };
        self.state(|s| s.set_status(text.into()));
    }
}
