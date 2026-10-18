//! Help > About ↔ ui/about.slint (`AboutState`): the version, and the project links opened in the browser. At each launch
//! a background thread asks GitHub for the newest release (pre-releases included, drafts are not public) and, when it is
//! newer than this build, `UpdateOverlay` suggests it. Offline or any error: nothing is shown.

use super::{library, AboutState, App};
use slint::ComponentHandle;
use std::time::Duration;

const RELEASES: &str = "https://api.github.com/repos/OpenRsG/OpenFadeR/releases?per_page=1";
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn wire(app: &App) {
    let s = app.global::<AboutState>();
    s.set_version(VERSION.into());
    s.on_open(|url| {
        let _ = library::open_external(&url);
    });
    let weak = app.as_weak();
    std::thread::spawn(move || {
        if let Some((tag, url)) = latest().filter(|(tag, _)| newer(tag, VERSION)) {
            let _ = weak.upgrade_in_event_loop(move |app| {
                let s = app.global::<AboutState>();
                s.set_update_url(url.into());
                s.set_update(tag.trim_start_matches('v').into());
            });
        }
    });
}

/// The newest release's tag and page.
fn latest() -> Option<(String, String)> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let body = agent
        .get(RELEASES)
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let r = v.get(0)?;
    Some((
        r["tag_name"].as_str()?.to_owned(),
        r["html_url"].as_str()?.to_owned(),
    ))
}

/// `tag` (`v1.2.3`, a `-suffix` ignored) is a higher version than `have`.
fn newer(tag: &str, have: &str) -> bool {
    let nums = |s: &str| -> Vec<u64> {
        let s = s.trim_start_matches('v');
        s.split('-')
            .next()
            .unwrap_or(s)
            .split('.')
            .map(|n| n.parse().unwrap_or(0))
            .collect()
    };
    nums(tag) > nums(have)
}

#[test]
fn versions() {
    assert!(newer("v0.2.0", "0.1.0"));
    assert!(newer("v0.10.0", "0.9.9"));
    assert!(newer("1.0.0-beta", "0.9.0"));
    assert!(!newer("v0.1.0", "0.1.0"));
    assert!(!newer("v0.1.0", "0.2.0"));
}
