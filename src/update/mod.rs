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

/// The latest published release (GitHub's `releases/latest`: no draft, no pre-release) from the API at `base`.
pub fn latest(base: &str) -> Result<Release, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let url = format!(
        "{}/repos/{REPO}/releases/latest",
        base.trim_end_matches('/')
    );
    let body = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: not a release ({e})"))
}

/// Goes back to the version kept in `<install>.prev` and forgets that an update was installed; the caller restarts the
/// program from the returned install.
pub fn roll_back() -> Result<install::Install, String> {
    let install = install::locate().map_err(|w| w.to_string())?;
    install::rollback(&install)?;
    state::update(|s| s.applied = None);
    Ok(install)
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
        assert_eq!(super::latest(&base).unwrap(), r);
        assert_eq!(
            super::latest(&format!("{base}/")).unwrap().tag,
            "v9.9.9",
            "a closing slash is fine"
        );
        let down = super::latest("http://127.0.0.1:9");
        assert!(
            down.is_err(),
            "offline is an error the caller shows or ignores"
        );
        let nothing = serve(Vec::new());
        assert!(super::latest(&nothing).is_err(), "no release (404)");
    }
}
