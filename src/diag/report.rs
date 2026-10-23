//! The problem report: markdown with fixed headings, written for a person and for a program that reads it.
//!
//! ```text
//! # OpenFadeR problem report
//! ## Environment   fenced `key: value` lines: report_format, kind (crash | manual), area, version, commit, build, os, arch, cpu, ram,
//!                  then what the app told [`crate::diag::set`] (renderer, gpu, audio_*, controller_*), time, uptime
//! ## Error         fenced `key: value`: message, location, thread (a manual report says `message: none`)
//! ## Backtrace     fenced; `none` in a manual report
//! ## Plugins       fenced `name: version (on|off)`
//! ## Settings      fenced `key: value`: only the settings that differ from the defaults
//! ## Log           fenced; the last lines of the log file
//! ```
//!
//! `area` is the first source folder of ours on the panic's location or backtrace (`app/deck`, `audio/player`), the place to
//! start looking. Paths and secrets are taken out ([`super::redact`]); backtrace paths are cut to `src/…`. [`block`] reads a
//! fenced `key: value` section back, [`issue_url`] opens a GitHub issue with the short environment block.

use super::{redact::redact, sys::Sys};

/// The `report_format` value: raised when a heading or a key changes meaning.
pub const FORMAT: u32 = 1;
pub const ISSUE_URL_MAX: usize = 6000;
const ISSUE_URL: &str = "https://github.com/OpenRsG/OpenFadeR/issues/new";
const BACKTRACE_LINES: usize = 150;

/// A panic: what the hook saw.
#[derive(Clone, Debug, Default)]
pub struct Panic {
    pub message: String,
    pub location: String,
    pub thread: String,
    pub backtrace: String,
}

/// Everything a report is made of.
pub struct Input<'a> {
    pub panic: Option<&'a Panic>,
    pub sys: &'a Sys,
    /// What the app set (renderer, audio, controllers…), in the order it set them.
    pub facts: &'a [(String, String)],
    pub plugins: &'a str,
    pub settings: &'a str,
    pub log: &'a [String],
    pub uptime_secs: u64,
}

fn one_line(v: &str) -> String {
    let v = redact(v).replace(['\n', '\r'], " ");
    if v.trim().is_empty() { "none".into() } else { v.trim().to_owned() }
}

fn unknown(v: &str) -> &str {
    if v.is_empty() { "unknown" } else { v }
}

/// A source path as the report shows it: ours `src/app/x.rs:1:2`, the standard library `library/std/src/…`, a dependency
/// `crate-1.2.3/src/…`; anything else its file name. No folder of the user's survives.
pub fn short_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    if p.starts_with("/rustc/") && let Some(i) = p.find("/library/") {
        return p[i + 1..].to_owned();
    }
    if let Some(i) = p.find("/registry/src/") {
        let rest = &p[i + "/registry/src/".len()..];
        return rest.split_once('/').map_or(rest, |(_, r)| r).to_owned();
    }
    if let Some(i) = p.rfind("/src/") {
        return p[i + 1..].to_owned();
    }
    if p.starts_with("src/") {
        return p;
    }
    p.rsplit('/').next().unwrap_or("").to_owned()
}

/// The backtrace with every `at <path>` shortened and cut to [`BACKTRACE_LINES`].
fn clean_backtrace(bt: &str) -> String {
    bt.lines()
        .take(BACKTRACE_LINES)
        .map(|l| match l.trim_start().strip_prefix("at ") {
            Some(path) => format!("{}at {}", &l[..l.len() - l.trim_start().len()], short_path(path)),
            None => l.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `app/deck` for `src/app/deck/load.rs:12:5`: the first two folders under our `src/`.
fn area_of(path: &str) -> Option<String> {
    let rest = short_path(path).strip_prefix("src/")?.to_owned();
    let parts: Vec<&str> = rest.split(':').next()?.split('/').collect();
    let (file, dirs) = parts.split_last()?;
    Some(if dirs.is_empty() {
        file.split('.').next()?.to_owned()
    } else {
        dirs.iter().take(2).copied().collect::<Vec<_>>().join("/")
    })
}

fn area(p: Option<&Panic>) -> String {
    let Some(p) = p else { return "unknown".into() };
    let frames = p.backtrace.lines().filter_map(|l| l.trim_start().strip_prefix("at "));
    std::iter::once(p.location.as_str())
        .chain(frames)
        .find_map(area_of)
        .unwrap_or_else(|| {
            let dep = short_path(&p.location);
            format!("dependency {}", dep.split('/').next().unwrap_or("unknown"))
        })
}

fn fence(s: &mut String, heading: &str, body: &str) {
    s.push_str(&format!("## {heading}\n\n```text\n{}\n```\n\n", body.trim_end().replace("```", "'''")));
}

pub fn build(i: &Input) -> String {
    let crash = i.panic.is_some();
    let mut env: Vec<(String, String)> = [
        ("report_format", FORMAT.to_string()),
        ("kind", if crash { "crash" } else { "manual" }.into()),
        ("area", area(i.panic)),
        ("version", env!("CARGO_PKG_VERSION").into()),
        ("commit", option_env!("OPENFADER_COMMIT").unwrap_or("unknown").into()),
        ("build", if cfg!(debug_assertions) { "debug" } else { "release" }.into()),
        ("os", unknown(&i.sys.os).into()),
        ("arch", std::env::consts::ARCH.into()),
        ("cpu", unknown(&i.sys.cpu).into()),
        ("ram", unknown(&i.sys.ram).into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    env.extend(i.facts.iter().cloned());
    env.push(("time".into(), chrono::Local::now().to_rfc3339()));
    env.push(("uptime".into(), format!("{} s", i.uptime_secs)));

    let mut s = String::from("# OpenFadeR problem report\n\n");
    let lines = |v: &[(String, String)]| v.iter().map(|(k, v)| format!("{k}: {}", one_line(v))).collect::<Vec<_>>().join("\n");
    fence(&mut s, "Environment", &lines(&env));
    match i.panic {
        Some(p) => {
            let err = [("message", &p.message), ("location", &short_path(&p.location)), ("thread", &p.thread)];
            fence(&mut s, "Error", &lines(&err.map(|(k, v)| (k.to_owned(), v.clone()))));
            fence(&mut s, "Backtrace", &clean_backtrace(&p.backtrace));
        }
        None => {
            fence(&mut s, "Error", "message: none");
            fence(&mut s, "Backtrace", "none");
        }
    }
    fence(&mut s, "Plugins", if i.plugins.is_empty() { "none" } else { i.plugins });
    fence(&mut s, "Settings", &if i.settings.is_empty() { "none".into() } else { redact(i.settings) });
    fence(&mut s, "Log", &if i.log.is_empty() { "none".into() } else { redact(&i.log.join("\n")) });
    s
}

/// The `key: value` lines of the fenced block under `## heading`.
pub fn block(report: &str, heading: &str) -> Vec<(String, String)> {
    let mut lines = report.lines().skip_while(|l| l.trim_end() != format!("## {heading}"));
    lines.next();
    lines
        .skip_while(|l| !l.starts_with("```"))
        .skip(1)
        .take_while(|l| !l.starts_with("```"))
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The new-issue address of the bug form with the title, the labels, the version and the environment block filled in (the
/// form's `environment` field). The block loses its last lines until the address fits [`ISSUE_URL_MAX`].
pub fn issue_url(report: &str) -> String {
    let env = block(report, "Environment");
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map_or("", |(_, v)| v.as_str());
    let crash = get("kind") == "crash";
    let message = block(report, "Error").into_iter().find(|(k, _)| k == "message").map_or(String::new(), |(_, v)| v);
    let title = if crash {
        format!("Crash: {}", message.chars().take(90).collect::<String>())
    } else {
        format!("Problem report ({})", get("version"))
    };
    let mut lines: Vec<String> = env.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    loop {
        let url = format!(
            "{ISSUE_URL}?template=bug_report.yml&title={}&labels={}&version={}&environment={}",
            encode(&title),
            if crash { "bug,crash" } else { "bug" },
            encode(get("version")),
            encode(&lines.join("\n")),
        );
        if url.len() <= ISSUE_URL_MAX || lines.len() <= 1 {
            return url;
        }
        lines.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panic() -> Panic {
        Panic {
            message: "index out of bounds: the len is 3 but the index is 7 (/home/alice/Music/x.mp3)".into(),
            location: "src/app/deck/load.rs:12:5".into(),
            thread: "main".into(),
            backtrace: "   0: std::panicking::begin_panic\n             at /rustc/abc123/library/std/src/panicking.rs:1:2\n   1: openfader::app::deck::load\n             at /home/alice/build/openfader/src/app/deck/load.rs:12:5".into(),
        }
    }

    fn input<'a>(p: Option<&'a Panic>, sys: &'a Sys, facts: &'a [(String, String)], log: &'a [String]) -> Input<'a> {
        Input { panic: p, sys, facts, plugins: "example: 1.0 (on)", settings: "key_lock: on", log, uptime_secs: 7 }
    }

    #[test]
    fn a_report_has_its_headings_and_an_environment_block_a_program_can_read_back() {
        let sys = Sys { os: "Test OS 1".into(), cpu: "Test CPU, 8 threads".into(), ram: "16.0 GiB".into() };
        let facts = vec![("renderer".to_owned(), "femtovg".to_owned()), ("controller_ports".to_owned(), "A; B".to_owned())];
        let log = vec!["2026-01-01 00:00:00.000 [err] audio: device lost".to_owned()];
        let p = panic();
        let r = build(&input(Some(&p), &sys, &facts, &log));
        let headings: Vec<&str> = r.lines().filter(|l| l.starts_with("## ")).collect();
        assert_eq!(headings, ["## Environment", "## Error", "## Backtrace", "## Plugins", "## Settings", "## Log"]);
        let env = block(&r, "Environment");
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("report_format"), Some("1"));
        assert_eq!(get("kind"), Some("crash"));
        assert_eq!(get("area"), Some("app/deck"));
        assert_eq!((get("os"), get("renderer"), get("controller_ports")), (Some("Test OS 1"), Some("femtovg"), Some("A; B")));
        assert_eq!(get("version"), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(get("uptime"), Some("7 s"));
        assert_eq!(block(&r, "Settings"), [("key_lock".to_owned(), "on".to_owned())]);
        assert!(r.contains("device lost") && r.contains("at src/app/deck/load.rs:12:5") && r.contains("at library/std/src/panicking.rs"));
        assert!(!r.contains("alice"), "no user folder in the report: {r}");
        let manual = build(&input(None, &sys, &facts, &[]));
        assert_eq!(block(&manual, "Environment")[1], ("kind".to_owned(), "manual".to_owned()));
        assert_eq!(block(&manual, "Error"), [("message".to_owned(), "none".to_owned())]);
    }

    #[test]
    fn the_issue_address_stays_under_the_limit_and_keeps_the_start_of_the_environment() {
        let sys = Sys::default();
        let facts: Vec<(String, String)> = (0..400).map(|n| (format!("fact_{n}"), "äöü & = ? # value ".repeat(5))).collect();
        let mut p = panic();
        p.message = "x".repeat(5000);
        let url = issue_url(&build(&input(Some(&p), &sys, &facts, &[])));
        assert!(url.len() <= ISSUE_URL_MAX, "{} bytes", url.len());
        assert!(url.starts_with("https://github.com/OpenRsG/OpenFadeR/issues/new?template=bug_report.yml&title=Crash%3A%20xxx"));
        assert!(url.contains("&labels=bug,crash&") && url.contains("environment=report_format%3A%201%0Akind%3A%20crash%0Aarea%3A%20app%2Fdeck"));
        assert!(!url.contains(' ') && !url.contains('\n') && !url.contains('#'), "encoded");
        let short = issue_url(&build(&input(None, &sys, &[], &[])));
        assert!(short.contains("title=Problem%20report%20%28") && short.contains("&labels=bug&") && short.contains("uptime%3A"), "{short}");
    }
}
