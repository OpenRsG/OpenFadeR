//! Updates (base): asks GitHub's REST API for the latest release of OpenRsG/OpenFadeR, compares it with this build, and
//! picks the release archive for this machine. Children: [`install`] (download with SHA-256 check, unpack, swap, roll back)
//! and [`state`] (when the last check ran, the skipped version, the version just installed).
//!
//! Nothing here touches the window and everything may block: callers run it on a worker thread. The API base is
//! `$OPENFADER_UPDATE_API` (tests and a mirror), else `https://api.github.com`. The release's archives are named by
//! `.github/workflows/release.yml`: [`asset_for`] knows the four of them. A machine with no archive (macOS on Intel, Windows
//! on ARM), a package-manager or read-only install ([`install::locate`]) is sent to the release page instead.

pub mod install;
pub mod state;

use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

/// The release asset's name suffix for `(std::env::consts::OS, ARCH)`; the part before it is the product and version.
fn archive_suffix(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "x86_64") => "-linux-x86_64.tar.gz",
        // The Raspberry Pi archive is the aarch64 Linux build (Debian 13, docs/raspberry-pi.md).
        ("linux", "aarch64") => "-linux-aarch64-pi.tar.gz",
        ("windows", "x86_64") => "-windows-x86_64.zip",
        ("macos", "aarch64") => "-macos-arm64.zip",
        _ => return None,
    })
}

/// One file of a release. `digest` is GitHub's `sha256:<hex>`, absent on releases made before GitHub computed it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
}

/// A GitHub release as the REST API returns it (the fields we use).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Release {
    #[serde(rename = "tag_name")]
    pub tag: String,
    /// The release's web page.
    #[serde(default, rename = "html_url")]
    pub page: String,
    /// The release notes (Markdown, shown as plain text); null when the author wrote none.
    #[serde(default, rename = "body")]
    pub notes: Option<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
    /// A pre-release (GitHub's flag): only the Beta channel offers it.
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
}

impl Release {
    /// The version the tag names (`v0.3.0` → 0.3.0); `None` when the tag is no semantic version.
    pub fn version(&self) -> Option<semver::Version> {
        semver::Version::parse(self.tag.trim_start_matches('v')).ok()
    }

    /// Newer than `have` (semantic versioning: `0.3.0-rc.1` is older than `0.3.0`).
    pub fn newer_than(&self, have: &str) -> bool {
        match (self.version(), semver::Version::parse(have)) {
            (Some(tag), Ok(have)) => tag > have,
            _ => false,
        }
    }

    /// The archive for `os` / `arch` (`std::env::consts` names), if the release has one.
    pub fn asset_for(&self, os: &str, arch: &str) -> Option<&Asset> {
        let suffix = archive_suffix(os, arch)?;
        self.assets.iter().find(|a| a.name.ends_with(suffix))
    }

    /// [`Release::asset_for`] this machine.
    pub fn asset(&self) -> Option<&Asset> {
        self.asset_for(std::env::consts::OS, std::env::consts::ARCH)
    }
}

const REPO: &str = "OpenRsG/OpenFadeR";

/// `$OPENFADER_UPDATE_API`, else GitHub's API.
pub fn api_base() -> String {
    std::env::var("OPENFADER_UPDATE_API")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://api.github.com".to_owned())
}

/// What the updater follows: the latest stable release, or the latest of all releases including pre-releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
}

/// Of a releases list (GitHub's `releases`, newest date first) the one the `channel` offers: no draft, a tag that is a
/// semantic version, a pre-release only on Beta; the highest version, not the newest date (a fix of an old line can come
/// after a newer line's release). `0.3.0-rc.1` therefore beats `0.2.5` and loses to `0.3.0`.
pub fn pick(list: Vec<Release>, channel: Channel) -> Option<Release> {
    list.into_iter()
        .filter(|r| !r.draft && (channel == Channel::Beta || !r.prerelease))
        .filter_map(|r| Some((r.version()?, r)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r)| r)
}

/// GET `path` of the API at `base`, the answer's text.
fn get(base: &str, path: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let url = format!("{}/repos/{REPO}/{path}", base.trim_end_matches('/'));
    agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))
}

/// The release `channel` offers, from the API at `base`: Stable is GitHub's `releases/latest` (no draft, no pre-release),
/// Beta the best of the `releases` list ([`pick`]).
pub fn latest(base: &str, channel: Channel) -> Result<Release, String> {
    match channel {
        Channel::Stable => {
            let body = get(base, "releases/latest")?;
            serde_json::from_str(&body).map_err(|e| format!("not a release ({e})"))
        }
        Channel::Beta => {
            let body = get(base, "releases?per_page=30")?;
            let list: Vec<Release> =
                serde_json::from_str(&body).map_err(|e| format!("not a list of releases ({e})"))?;
            pick(list, channel).ok_or_else(|| "no release found".to_owned())
        }
    }
}

/// What [`roll_back`] did.
#[derive(Debug)]
pub struct RolledBack {
    pub install: install::Install,
    /// The library from before the update was put back (the new version had migrated the store past what the old one reads).
    pub library: Option<crate::store::backup::RolledBack>,
}

/// Goes back to the version kept in `<install>.prev` and forgets that an update was installed; the caller restarts the
/// program from the returned install. When the new version migrated the store past what the old one reads, the library
/// made before the update is put back first (the newer store stays beside it); without such a backup the roll-back is
/// refused, because the old version could not open the store.
pub fn roll_back() -> Result<RolledBack, String> {
    let install = install::locate().map_err(|w| w.to_string())?;
    let applied = state::file().and_then(|f| state::read(&f).applied);
    let db = crate::paths::data_dir().map(|d| d.join("store.db"));
    let done = roll_back_at(install, applied.as_ref(), db.as_deref())?;
    state::update(|s| s.applied = None);
    Ok(done)
}

/// [`roll_back`] with the install, the record of the update and the store file given.
fn roll_back_at(
    install: install::Install,
    applied: Option<&state::Applied>,
    db: Option<&Path>,
) -> Result<RolledBack, String> {
    use crate::store::backup;
    // The store is beyond what the old version reads (its schema is known from the record): it needs the backup.
    let newer = db
        .zip(applied.filter(|a| a.schema > 0))
        .and_then(|(db, a)| {
            let have = backup::file_schema(db).filter(|&h| h > a.schema)?;
            Some((db, a, have))
        });
    if let Some((_, a, have)) = newer {
        if !Path::new(&a.backup).is_file() {
            return Err(format!(
                "the library was changed by the newer version (store v{have}, version {} reads v{}) and no backup from before the update is kept: the old version could not open it, nothing was changed",
                a.from, a.schema
            ));
        }
    }
    install::rollback(&install)?;
    let library = match newer {
        Some((db, a, _)) => backup::roll_back_store(db, Path::new(&a.backup), a.schema)
            .inspect_err(|_| {
                // The old version would not open the store: stay on the new one.
                let _ = install::rollback(&install);
            })
            .map_err(|e| {
                format!("the library from before the update could not be put back ({e}), nothing was changed")
            })?,
        None => None,
    };
    Ok(RolledBack { install, library })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An asset of `data` with GitHub's `sha256:` digest.
    pub fn asset_of(name: &str, data: &[u8]) -> Asset {
        let hex: String = hmac_sha256::Hash::hash(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Asset {
            name: name.into(),
            url: format!("https://example.test/{name}"),
            size: data.len() as u64,
            digest: Some(format!("sha256:{hex}")),
        }
    }

    /// A local HTTP server answering `GET path` with the bytes listed (404 otherwise), for the rest of the test process.
    pub fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut lines = BufReader::new(conn.try_clone().unwrap()).lines();
                let request = lines.next().and_then(Result::ok).unwrap_or_default();
                while lines
                    .next()
                    .and_then(Result::ok)
                    .is_some_and(|l| !l.is_empty())
                {}
                let path = request.split(' ').nth(1).unwrap_or("");
                let (status, body) = match files.iter().find(|(p, _)| *p == path) {
                    Some((_, b)) => ("200 OK", b.as_slice()),
                    None => ("404 Not Found", &b""[..]),
                };
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = conn.write_all(body);
            }
        });
        base
    }

    pub fn release(tag: &str, names: &[&str]) -> Release {
        Release {
            tag: tag.into(),
            page: "https://example.test/release".into(),
            notes: None,
            prerelease: false,
            draft: false,
            assets: names
                .iter()
                .map(|n| Asset {
                    name: (*n).into(),
                    url: format!("https://example.test/{n}"),
                    size: 1,
                    digest: None,
                })
                .collect(),
        }
    }

    /// The tags `pick` offers `channel` from releases of `(tag, prerelease, draft)`.
    fn offered(list: &[(&str, bool, bool)], channel: Channel) -> Option<String> {
        let list = list
            .iter()
            .map(|&(tag, prerelease, draft)| Release {
                prerelease,
                draft,
                ..release(tag, &[])
            })
            .collect();
        pick(list, channel).map(|r| r.tag)
    }

    #[test]
    fn a_release_candidate_sorts_after_the_last_stable_and_before_its_release() {
        let v = |t: &str| semver::Version::parse(t).unwrap();
        assert!(v("0.2.5") < v("0.3.0-rc.1") && v("0.3.0-rc.1") < v("0.3.0"));
        assert!(v("0.3.0-rc.1") < v("0.3.0-rc.2") && v("0.3.0-beta.2") < v("0.3.0-rc.1"));
        assert!(release("v0.3.0-rc.1", &[]).newer_than("0.2.5"));
        assert!(!release("v0.3.0-rc.1", &[]).newer_than("0.3.0"));
        assert!(release("v0.3.0", &[]).newer_than("0.3.0-rc.1"));
    }

    #[test]
    fn each_channel_offers_its_own_release() {
        let list = [
            ("v0.2.5", false, false),
            ("v0.3.0-rc.1", true, false),
            ("v0.9.0", false, true),  // a draft is nobody's
            ("preview", true, false), // no version
            ("v0.2.4", false, false),
        ];
        assert_eq!(offered(&list, Channel::Stable).as_deref(), Some("v0.2.5"));
        assert_eq!(
            offered(&list, Channel::Beta).as_deref(),
            Some("v0.3.0-rc.1")
        );
        // The release of the candidate is the newest for both.
        let done = [list.as_slice(), &[("v0.3.0", false, false)]].concat();
        assert_eq!(offered(&done, Channel::Beta).as_deref(), Some("v0.3.0"));
        assert_eq!(offered(&done, Channel::Stable).as_deref(), Some("v0.3.0"));
        // Highest version, not newest date: a fix of the old line published last does not win.
        let late = [("v0.3.0", false, false), ("v0.2.6", false, false)];
        assert_eq!(offered(&late, Channel::Stable).as_deref(), Some("v0.3.0"));
        assert_eq!(offered(&[("v1.0.0", false, true)], Channel::Beta), None);
        assert_eq!(offered(&[], Channel::Stable), None);
    }

    /// An install of `1.0.0` (the old one, in `.prev`) and `2.0.0` (running) in a fresh folder.
    fn installs(name: &str) -> (std::path::PathBuf, install::Install) {
        let d =
            std::env::temp_dir().join(format!("openfader-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let install = install::Install {
            root: d.join("app"),
            exe: "bin".into(),
        };
        for (dir, v) in [(install.root.clone(), "2.0.0"), (install.prev(), "1.0.0")] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("bin"), v).unwrap();
        }
        (d, install)
    }

    /// A store file in `db` written by a build `extra` migrations ahead of this one.
    fn store_ahead(db: &Path, extra: usize) {
        drop(crate::store::Store::open_at(db).unwrap());
        rusqlite::Connection::open(db)
            .unwrap()
            .pragma_update(
                None,
                "user_version",
                (crate::store::backup::schema() + extra) as i64,
            )
            .unwrap();
    }

    #[test]
    fn a_roll_back_never_leaves_the_old_version_a_store_it_cannot_open() {
        use crate::store::backup::{file_schema, schema};
        let (d, install) = installs("roll");
        let (db, running) = (d.join("store.db"), |i: &install::Install| {
            std::fs::read_to_string(i.exe_path()).unwrap()
        });
        let backup = crate::store::Store::in_memory()
            .unwrap()
            .backup_auto(&d, None, "1.0.0", "t", 3)
            .unwrap();
        let applied = |schema, backup: &Path| state::Applied {
            from: "1.0.0".into(),
            to: "2.0.0".into(),
            schema,
            backup: backup.display().to_string(),
            ..Default::default()
        };

        // The new version migrated the store but no backup is kept: refused, nothing moves.
        store_ahead(&db, 1);
        let none = applied(schema(), &d.join("gone.zip"));
        let e = roll_back_at(install.clone(), Some(&none), Some(&db)).unwrap_err();
        assert!(e.contains("no backup"), "{e}");
        assert_eq!(
            (running(&install).as_str(), file_schema(&db)),
            ("2.0.0", Some(schema() + 1))
        );

        // A backup that cannot be used: the swap is undone, the new version stays.
        let bad = d.join("bad.zip");
        std::fs::write(&bad, b"junk").unwrap();
        let e =
            roll_back_at(install.clone(), Some(&applied(schema(), &bad)), Some(&db)).unwrap_err();
        assert!(e.contains("could not be put back"), "{e}");
        assert_eq!(
            (running(&install).as_str(), file_schema(&db)),
            ("2.0.0", Some(schema() + 1))
        );

        // With the backup: the old version, its library back, the newer one kept.
        let done = roll_back_at(
            install.clone(),
            Some(&applied(schema(), &backup)),
            Some(&db),
        )
        .unwrap();
        let library = done.library.expect("the library was put back");
        assert_eq!(
            (running(&install).as_str(), file_schema(&db)),
            ("1.0.0", Some(schema()))
        );
        assert_eq!(file_schema(&library.kept), Some(schema() + 1));

        // A store the old version reads, or an update whose schema was not recorded: only the program goes back.
        let (d2, install2) = installs("roll-plain");
        let db2 = d2.join("store.db");
        store_ahead(&db2, 0);
        let plain = roll_back_at(
            install2.clone(),
            Some(&applied(schema(), &backup)),
            Some(&db2),
        )
        .unwrap();
        assert!(plain.library.is_none() && running(&install2) == "1.0.0");
        let (d3, install3) = installs("roll-unknown");
        let db3 = d3.join("store.db");
        store_ahead(&db3, 1);
        let unknown =
            roll_back_at(install3.clone(), Some(&applied(0, &backup)), Some(&db3)).unwrap();
        assert!(unknown.library.is_none() && running(&install3) == "1.0.0");
        for d in [d, d2, d3] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn versions_compare_as_semver() {
        let newer = |tag: &str, have: &str| release(tag, &[]).newer_than(have);
        assert!(newer("v0.3.0", "0.2.5"));
        assert!(newer("v0.10.0", "0.9.9"), "numbers, not text");
        assert!(
            newer("0.3.0", "0.3.0-rc.1"),
            "a pre-release is older than its release"
        );
        assert!(!newer("v0.3.0-rc.1", "0.3.0"));
        assert!(!newer("v0.2.5", "0.2.5"));
        assert!(!newer("v0.2.4", "0.2.5"));
        assert!(
            !newer("nightly", "0.2.5"),
            "a tag that is no version is never an update"
        );
    }

    #[test]
    fn each_platform_gets_its_own_archive() {
        let r = release(
            "v0.3.0",
            &[
                "openfader-0.3.0-linux-x86_64.tar.gz",
                "openfader-0.3.0-linux-aarch64-pi.tar.gz",
                "openfader-0.3.0-windows-x86_64.zip",
                "OpenFadeR-0.3.0-macos-arm64.zip",
                "notes.txt",
            ],
        );
        let pick = |os, arch| r.asset_for(os, arch).map(|a| a.name.as_str());
        assert_eq!(
            pick("linux", "x86_64"),
            Some("openfader-0.3.0-linux-x86_64.tar.gz")
        );
        assert_eq!(
            pick("linux", "aarch64"),
            Some("openfader-0.3.0-linux-aarch64-pi.tar.gz")
        );
        assert_eq!(
            pick("windows", "x86_64"),
            Some("openfader-0.3.0-windows-x86_64.zip")
        );
        assert_eq!(
            pick("macos", "aarch64"),
            Some("OpenFadeR-0.3.0-macos-arm64.zip")
        );
        assert_eq!(pick("macos", "x86_64"), None, "no Intel Mac build");
        assert_eq!(pick("windows", "aarch64"), None);
        assert_eq!(release("v1", &["x.zip"]).asset_for("linux", "x86_64"), None);
    }

    #[test]
    fn the_api_answer_parses_with_null_notes_and_digests() {
        let r: Release = serde_json::from_str(
            r#"{"tag_name":"v1.0.0","html_url":"u","body":null,"draft":false,
                "assets":[{"name":"a.zip","browser_download_url":"d","size":7,"digest":null},
                          {"name":"b.zip","browser_download_url":"e","size":9,"digest":"sha256:ab"}]}"#,
        )
        .unwrap();
        assert_eq!((r.notes, r.assets[0].digest.clone()), (None, None));
        assert_eq!(r.assets[1].digest.as_deref(), Some("sha256:ab"));
    }
}

#[cfg(test)]
mod serve_tests {
    use super::tests::{asset_of, release, serve};
    use super::Channel;

    #[test]
    fn the_latest_release_is_read_from_the_api_base() {
        let mut r = release("v9.9.9", &["openfader-9.9.9-linux-x86_64.tar.gz"]);
        r.assets[0] = asset_of(&r.assets[0].name, b"archive");
        r.notes = Some("## Fixes\n- one".into());
        let json = serde_json::json!({
            "tag_name": r.tag, "html_url": r.page, "body": r.notes,
            "assets": [{"name": r.assets[0].name, "browser_download_url": r.assets[0].url,
                        "size": r.assets[0].size, "digest": r.assets[0].digest}],
        });
        let base = serve(vec![(
            "/repos/OpenRsG/OpenFadeR/releases/latest",
            json.to_string().into_bytes(),
        )]);
        assert_eq!(super::latest(&base, Channel::Stable).unwrap(), r);
        assert_eq!(
            super::latest(&format!("{base}/"), Channel::Stable)
                .unwrap()
                .tag,
            "v9.9.9",
            "a closing slash is fine"
        );
        let down = super::latest("http://127.0.0.1:9", Channel::Stable);
        assert!(
            down.is_err(),
            "offline is an error the caller shows or ignores"
        );
        let nothing = serve(Vec::new());
        assert!(
            super::latest(&nothing, Channel::Stable).is_err(),
            "no release (404)"
        );
    }

    #[test]
    fn the_beta_channel_reads_the_releases_list() {
        let list = serde_json::json!([
            {"tag_name": "v0.4.0", "draft": true, "prerelease": false},
            {"tag_name": "v0.3.0-rc.1", "prerelease": true, "html_url": "rc"},
            {"tag_name": "v0.2.6", "prerelease": false},
            {"tag_name": "nightly", "prerelease": true},
        ]);
        let base = serve(vec![(
            "/repos/OpenRsG/OpenFadeR/releases?per_page=30",
            list.to_string().into_bytes(),
        )]);
        let rc = super::latest(&base, Channel::Beta).unwrap();
        assert_eq!((rc.tag.as_str(), rc.page.as_str()), ("v0.3.0-rc.1", "rc"));
        assert!(rc.newer_than("0.2.6"), "offered to a stable 0.2.6");
        assert!(
            super::latest(&serve(vec![("/x", Vec::new())]), Channel::Beta).is_err(),
            "a 404 is an error"
        );
        let empty = serve(vec![(
            "/repos/OpenRsG/OpenFadeR/releases?per_page=30",
            b"[]".to_vec(),
        )]);
        assert!(super::latest(&empty, Channel::Beta).is_err(), "no release");
    }
}
