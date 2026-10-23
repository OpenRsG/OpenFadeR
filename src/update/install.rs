//! Installing a release archive over the running install, and rolling back.
//!
//! [`locate`] finds what to replace: the folder the program runs from (the `install.sh` layout `~/.local/lib/openfader`, or an
//! unpacked archive folder) or, on macOS, its `.app` bundle. A package-manager or read-only install, or a build in a source
//! tree, is refused ([`Why`]): the caller opens the release page. [`download`] streams the asset and refuses it unless its
//! SHA-256 is the `digest` GitHub gave the asset. [`apply`] unpacks it next to the install (`<install>.new`), moves it to
//! `<install>.prev` and exchanges the two folders, so the old version is kept there for [`rollback`] (which exchanges again).
//! The user's settings, library, add-ons and caches live in the per-user folders ([`crate::paths`]), never in the install.
//!
//! A running program's files cannot be deleted or overwritten on Windows, but they can be renamed: there the exchange moves
//! file by file ([`exchange_files`]) and the running `openfader.exe` ends up in `<install>.prev`. Elsewhere the two folders are
//! renamed. ponytail: two renames, not one atomic `renameat2(RENAME_EXCHANGE)`: a crash between them leaves `<install>.swap`
//! to rename back by hand.

use super::Asset;
use crate::{paths, zip::Zip};
use flate2::read::GzDecoder;
use hmac_sha256::Hash;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The install to replace: its root folder (a `.app` bundle on macOS) and the program inside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Install {
    pub root: PathBuf,
    /// The executable, relative to `root`.
    pub exe: PathBuf,
}

/// Why this install is not replaced by the app: the release page is opened instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// Under `/usr`, `/nix`, a snap or a Flatpak: the package manager owns it.
    Package,
    /// A build in a source tree (`target/release/openfader`).
    Development,
    /// The folder or the one around it is not writable by this user.
    ReadOnly,
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            Why::Package => "this install belongs to a package manager",
            Why::Development => "this is a build in a source tree",
            Why::ReadOnly => "this install is not writable by you",
        })
    }
}

impl Install {
    pub fn exe_path(&self) -> PathBuf {
        self.root.join(&self.exe)
    }

    /// Where the previous version is kept.
    pub fn prev(&self) -> PathBuf {
        sibling(&self.root, "prev")
    }

    /// A previous version this program installed is there to roll back to.
    pub fn can_roll_back(&self) -> bool {
        self.prev().join(&self.exe).is_file()
    }
}

/// `<root>.<suffix>` next to `root`.
fn sibling(root: &Path, suffix: &str) -> PathBuf {
    let mut name = root.file_name().unwrap_or_default().to_owned();
    name.push(".");
    name.push(suffix);
    root.with_file_name(name)
}

/// Whether a file can be made in `dir`.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".openfader-write-test-{}", std::process::id()));
    let ok = fs::File::create(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

/// The install the running program belongs to.
pub fn locate() -> Result<Install, Why> {
    locate_from(&std::env::current_exe().map_err(|_| Why::ReadOnly)?)
}

/// `p` when its last component is `n`.
fn named<'a>(p: Option<&'a Path>, n: &str) -> Option<&'a Path> {
    p.filter(|p| p.file_name().is_some_and(|f| f == n))
}

/// The install of the executable `exe`.
pub fn locate_from(exe: &Path) -> Result<Install, Why> {
    // `<name>.app/Contents/MacOS/<exe>`
    let bundle = named(exe.parent(), "MacOS")
        .and_then(|d| named(d.parent(), "Contents"))
        .and_then(Path::parent)
        .filter(|b| b.extension().is_some_and(|e| e == "app"));
    let (root, rel) = match bundle {
        Some(b) => (b, exe.strip_prefix(b).map_err(|_| Why::ReadOnly)?),
        None => (
            exe.parent().ok_or(Why::ReadOnly)?,
            Path::new(exe.file_name().ok_or(Why::ReadOnly)?),
        ),
    };
    if ["/usr", "/nix", "/snap", "/app", "/var/lib/flatpak"]
        .iter()
        .any(|p| root.starts_with(p))
    {
        return Err(Why::Package);
    }
    if named(root.parent(), "target").is_some() {
        return Err(Why::Development);
    }
    if !root.parent().is_some_and(writable) || !writable(root) {
        return Err(Why::ReadOnly);
    }
    Ok(Install {
        root: root.to_owned(),
        exe: rel.to_owned(),
    })
}

/// Most an update archive may be.
const MAX_DOWNLOAD: u64 = 1 << 30;

/// The SHA-256 the asset must have, as lower-case hex: GitHub's `digest` field. A release without one is refused.
fn expected(asset: &Asset) -> Result<String, String> {
    asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .map(str::to_ascii_lowercase)
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| {
            format!(
                "{}: the release gives no SHA-256 checksum to verify it by",
                asset.name
            )
        })
}

/// Copies `r` to `out` in chunks, reporting the bytes so far, and fails unless the size and the SHA-256 are the asset's
/// (`cancel` stops it between chunks).
fn copy_checked(
    mut r: impl Read,
    out: &mut impl Write,
    asset: &Asset,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let want = expected(asset)?;
    let (mut hash, mut buf, mut got) = (Hash::new(), vec![0u8; 64 << 10], 0u64);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        if got > MAX_DOWNLOAD.max(asset.size) {
            return Err(format!("{}: larger than the release says", asset.name));
        }
        progress(got);
    }
    if asset.size != 0 && got != asset.size {
        return Err(format!(
            "{}: {got} bytes arrived, the release says {}",
            asset.name, asset.size
        ));
    }
    let have: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if have != want {
        return Err(format!(
            "{}: the download is not the file the release lists (SHA-256 {have}, expected {want}); it was discarded",
            asset.name
        ));
    }
    Ok(())
}

/// Downloads `asset` to `to` through a `.part` file, so a failed, cancelled or tampered download leaves nothing there.
pub fn download(
    asset: &Asset,
    to: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    expected(asset)?; // refused before a byte is fetched
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30 * 60)))
        .build()
        .into();
    let resp = agent
        .get(&asset.url)
        .call()
        .map_err(|e| format!("{}: {e}", asset.url))?;
    if let Some(dir) = to.parent() {
        paths::at(fs::create_dir_all(dir), dir)?;
    }
    let part = to.with_extension("part");
    let mut out = paths::at(fs::File::create(&part), &part)?;
    let done = copy_checked(
        resp.into_body().into_reader(),
        &mut out,
        asset,
        cancel,
        progress,
    )
    .and_then(|_| paths::at(fs::rename(&part, to), to));
    if done.is_err() {
        let _ = fs::remove_file(&part);
    }
    done
}

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Unpacks a release archive (`.tar.gz` or `.zip`, one top folder) into `to`, without the top folder.
pub fn unpack(archive: &Path, to: &Path) -> Result<(), String> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".tar.gz") {
        unpack_tar_gz(archive, to)
    } else if name.ends_with(".zip") {
        let mut z = Zip::open(archive).map_err(|e| format!("{name}: {e:?}"))?;
        let top = z
            .names()
            .next()
            .and_then(|n| n.split('/').next())
            .filter(|t| !t.is_empty())
            .ok_or("the archive is empty")?
            .to_owned();
        let prefix = format!("{top}/");
        if z.names()
            .any(|n| !n.starts_with(&prefix) && !n.starts_with("__MACOSX/"))
        {
            return Err("the archive holds more than one top folder".into());
        }
        z.unpack(&prefix, to, &[])
    } else {
        Err(format!("{name}: not a .tar.gz or .zip archive"))
    }
}

fn unpack_tar_gz(archive: &Path, to: &Path) -> Result<(), String> {
    let mut tar = tar::Archive::new(GzDecoder::new(paths::at(fs::File::open(archive), archive)?));
    tar.set_preserve_permissions(true);
    let mut top: Option<OsString> = None;
    for entry in tar.entries().map_err(text)? {
        let mut entry = entry.map_err(text)?;
        let kind = entry.header().entry_type();
        if kind == tar::EntryType::XGlobalHeader {
            continue;
        }
        let path = entry.path().map_err(text)?.into_owned();
        let mut parts = path
            .components()
            .filter(|c| !matches!(c, Component::CurDir));
        let Some(Component::Normal(first)) = parts.next() else {
            return Err(format!("entry {path:?} is not in a folder"));
        };
        if *top.get_or_insert_with(|| first.to_owned()) != *first {
            return Err("the archive holds more than one top folder".into());
        }
        let mut rel = PathBuf::new();
        for c in parts {
            let Component::Normal(n) = c else {
                return Err(format!("entry {path:?} leaves the folder"));
            };
            rel.push(n);
        }
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dst = to.join(&rel);
        if kind.is_dir() {
            paths::at(fs::create_dir_all(&dst), &dst)?;
        } else if kind.is_file() {
            if let Some(dir) = dst.parent() {
                paths::at(fs::create_dir_all(dir), dir)?;
            }
            paths::at(entry.unpack(&dst), &dst)?;
        } else {
            return Err(format!("entry {path:?} is neither a file nor a folder"));
        }
    }
    top.map(|_| ()).ok_or_else(|| "the archive is empty".into())
}

/// Replaces the install by the release in `archive` and keeps the old one in [`Install::prev`]; the install is untouched
/// unless everything before the exchange worked.
pub fn apply(install: &Install, archive: &Path) -> Result<(), String> {
    let (new, prev) = (sibling(&install.root, "new"), install.prev());
    let _ = fs::remove_dir_all(&new);
    let staged = unpack(archive, &new).and_then(|_| {
        new.join(&install.exe)
            .is_file()
            .then_some(())
            .ok_or_else(|| format!("the archive holds no {}", install.exe.display()))
    });
    if let Err(e) = staged {
        let _ = fs::remove_dir_all(&new);
        return Err(e);
    }
    if prev.exists() {
        paths::at(fs::remove_dir_all(&prev), &prev)?;
    }
    paths::at(fs::rename(&new, &prev), &new)?;
    exchange(&install.root, &prev, cfg!(windows)).inspect_err(|_| {
        let _ = fs::remove_dir_all(&prev);
    })
}

/// Puts the previous version back; the version rolled back from takes its place in `<install>.prev`.
pub fn rollback(install: &Install) -> Result<(), String> {
    if !install.can_roll_back() {
        return Err("there is no previous version to go back to".into());
    }
    exchange(&install.root, &install.prev(), cfg!(windows))
}

/// Swaps the contents of the folders `root` and `other`.
fn exchange(root: &Path, other: &Path, by_files: bool) -> Result<(), String> {
    let tmp = sibling(root, "swap");
    let _ = fs::remove_dir_all(&tmp);
    if by_files {
        return exchange_files(root, other, &tmp);
    }
    let mv = |a: &Path, b: &Path| paths::at(fs::rename(a, b), a);
    mv(root, &tmp)?;
    if let Err(e) = mv(other, root) {
        let _ = fs::rename(&tmp, root);
        return Err(e);
    }
    mv(&tmp, other)
}

/// [`exchange`] one file at a time (a running program's files and folders with an open file cannot be renamed on Windows, a
/// file can): `root` → `tmp`, `other` → `root`, `tmp` → `other`; any failure puts the files already moved back.
fn exchange_files(root: &Path, other: &Path, tmp: &Path) -> Result<(), String> {
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    let moved = move_tree(root, tmp, &mut done)
        .and_then(|_| move_tree(other, root, &mut done))
        .and_then(|_| move_tree(tmp, other, &mut done));
    if moved.is_err() {
        for (from, to) in done.iter().rev() {
            if let Some(dir) = from.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::rename(to, from);
        }
    }
    let _ = fs::remove_dir_all(tmp);
    moved
}

/// Moves every file under `from` to the same place under `to` (`from`'s emptied folders go, `from` stays), noting each move.
fn move_tree(from: &Path, to: &Path, done: &mut Vec<(PathBuf, PathBuf)>) -> Result<(), String> {
    paths::at(fs::create_dir_all(to), to)?;
    let names: Vec<_> = paths::at(fs::read_dir(from), from)?
        .filter_map(|e| e.ok().map(|e| (e.file_name(), e.path().is_dir())))
        .collect();
    for (name, is_dir) in names {
        let (a, b) = (from.join(&name), to.join(&name));
        if is_dir {
            move_tree(&a, &b, done)?;
            let _ = fs::remove_dir(&a);
        } else {
            paths::at(fs::rename(&a, &b), &a)?;
            done.push((a, b));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::tests::{asset_of, serve};
    use std::io::Cursor;

    fn temp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("openfader-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    /// The install.sh / archive layout holding `version` in a text file, next to a user's data that must never change.
    fn make_install(base: &Path, version: &str) -> Install {
        let root = base.join("openfader");
        fs::create_dir_all(root.join("extension/plugin/p")).unwrap();
        fs::write(root.join("openfader"), format!("program {version}")).unwrap();
        fs::write(root.join("extension/plugin/p/plugin.json"), version).unwrap();
        Install {
            root,
            exe: "openfader".into(),
        }
    }

    fn version_of(root: &Path) -> String {
        fs::read_to_string(root.join("extension/plugin/p/plugin.json")).unwrap()
    }

    /// A release archive: `top/` holding `openfader` (executable) and the plugin folder, as `release.yml` packs it.
    fn tar_gz(top: &str, version: &str) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut add = |path: String, data: &[u8], mode: u32| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            b.append_data(&mut h, path, data).unwrap();
        };
        add(
            format!("{top}/openfader"),
            format!("program {version}").as_bytes(),
            0o755,
        );
        add(
            format!("{top}/extension/plugin/p/plugin.json"),
            version.as_bytes(),
            0o644,
        );
        add(format!("{top}/LICENSE"), b"AGPL", 0o644);
        b.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn a_download_is_refused_unless_it_has_the_sha256_the_release_lists() {
        let body = b"the release archive".to_vec();
        let good = asset_of("a.zip", &body);
        let copy = |asset: &Asset, data: &[u8]| {
            let mut out = Vec::new();
            copy_checked(
                Cursor::new(data.to_vec()),
                &mut out,
                asset,
                &no_cancel(),
                &mut |_| {},
            )
            .map(|_| out)
        };
        assert_eq!(copy(&good, &body).unwrap(), body);
        let err = copy(&good, b"the RELEASE archive").unwrap_err();
        assert!(
            err.contains("SHA-256") && err.contains("discarded"),
            "{err}"
        );
        let same_size = Asset {
            digest: Some(format!("sha256:{}", "0".repeat(64))),
            ..good.clone()
        };
        assert!(
            copy(&same_size, &body)
                .unwrap_err()
                .contains("not the file"),
            "right size, wrong digest"
        );
        let none = Asset {
            digest: None,
            ..good.clone()
        };
        assert!(copy(&none, &body)
            .unwrap_err()
            .contains("no SHA-256 checksum"));
        let short = Asset {
            size: body.len() as u64 + 1,
            ..good.clone()
        };
        assert!(copy(&short, &body).unwrap_err().contains("bytes arrived"));
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            copy_checked(
                Cursor::new(body),
                &mut Vec::new(),
                &good,
                &cancelled,
                &mut |_| {}
            )
            .unwrap_err(),
            "cancelled"
        );
    }

    #[test]
    fn download_over_http_keeps_a_good_file_and_leaves_nothing_of_a_bad_one() {
        let dir = temp("download");
        let body = tar_gz("openfader-9.9.9-linux-x86_64", "9.9.9");
        let base = serve(vec![
            ("/a.tar.gz", body.clone()),
            ("/evil.tar.gz", b"something else".to_vec()),
        ]);
        let good = Asset {
            url: format!("{base}/a.tar.gz"),
            ..asset_of("a.tar.gz", &body)
        };
        let to = dir.join("cache/a.tar.gz");
        let mut seen = 0;
        download(&good, &to, &no_cancel(), &mut |n| seen = n).unwrap();
        assert_eq!(
            (fs::read(&to).unwrap(), seen),
            (body.clone(), body.len() as u64)
        );
        let evil = Asset {
            url: format!("{base}/evil.tar.gz"),
            size: 0,
            ..good.clone()
        };
        let to = dir.join("cache/evil.tar.gz");
        assert!(download(&evil, &to, &no_cancel(), &mut |_| {}).is_err());
        assert!(!to.exists() && !to.with_extension("part").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_update_replaces_the_install_keeps_the_old_one_and_rolls_back() {
        let base = temp("apply");
        let data = base.join("config/library.db");
        fs::create_dir_all(data.parent().unwrap()).unwrap();
        fs::write(&data, "my library").unwrap();
        let install = make_install(&base, "1.0.0");
        let archive = base.join("openfader-2.0.0-linux-x86_64.tar.gz");
        fs::write(&archive, tar_gz("openfader-2.0.0-linux-x86_64", "2.0.0")).unwrap();

        apply(&install, &archive).unwrap();
        assert_eq!(version_of(&install.root), "2.0.0");
        assert_eq!(
            fs::read_to_string(install.exe_path()).unwrap(),
            "program 2.0.0"
        );
        assert_eq!(
            version_of(&install.prev()),
            "1.0.0",
            "the old version stays for a roll back"
        );
        assert!(install.can_roll_back());
        assert!(
            install.root.join("LICENSE").is_file(),
            "the top folder is dropped, the rest unpacked"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(install.exe_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "the program stays executable");
        }
        assert!(
            !sibling(&install.root, "new").exists() && !sibling(&install.root, "swap").exists()
        );

        rollback(&install).unwrap();
        assert_eq!(version_of(&install.root), "1.0.0");
        assert_eq!(
            fs::read_to_string(install.exe_path()).unwrap(),
            "program 1.0.0"
        );
        assert_eq!(
            fs::read_to_string(&data).unwrap(),
            "my library",
            "user data is outside the install"
        );

        let none = make_install(&temp("apply-none"), "1.0.0");
        assert!(rollback(&none).is_err(), "nothing to go back to");
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_broken_archive_changes_nothing() {
        let base = temp("broken");
        let install = make_install(&base, "1.0.0");
        let prev_before = install.prev();
        fs::create_dir_all(&prev_before).unwrap();
        fs::write(prev_before.join("keep"), "older").unwrap();
        let archive = base.join("x-linux-x86_64.tar.gz");
        // no `openfader` inside
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_cksum();
        b.append_data(&mut h, "top/readme", &b"x"[..]).unwrap();
        fs::write(&archive, b.into_inner().unwrap().finish().unwrap()).unwrap();
        assert!(apply(&install, &archive)
            .unwrap_err()
            .contains("holds no openfader"));
        // two top folders, and a file that is no archive at all
        let two = base.join("two-linux-x86_64.tar.gz");
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for p in ["a/openfader", "b/openfader"] {
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_cksum();
            b.append_data(&mut h, p, &b"x"[..]).unwrap();
        }
        fs::write(&two, b.into_inner().unwrap().finish().unwrap()).unwrap();
        assert!(apply(&install, &two)
            .unwrap_err()
            .contains("more than one top folder"));
        let junk = base.join("junk.tar.gz");
        fs::write(&junk, "not gzip").unwrap();
        assert!(apply(&install, &junk).is_err());
        assert_eq!(version_of(&install.root), "1.0.0");
        assert_eq!(
            fs::read_to_string(prev_before.join("keep")).unwrap(),
            "older",
            "the kept version survives a failed update"
        );
        assert!(!sibling(&install.root, "new").exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_exchange_by_files_swaps_two_folders_as_the_windows_update_does() {
        let base = temp("files");
        let (a, b) = (base.join("a"), base.join("b"));
        for (d, v) in [(&a, "one"), (&b, "two")] {
            fs::create_dir_all(d.join("deep/er")).unwrap();
            fs::write(d.join("top.txt"), v).unwrap();
            fs::write(d.join("deep/er/leaf.txt"), v).unwrap();
        }
        fs::write(b.join("only-new.txt"), "x").unwrap();
        exchange(&a, &b, true).unwrap();
        let read = |d: &Path, f: &str| fs::read_to_string(d.join(f)).unwrap();
        assert_eq!(
            (read(&a, "top.txt"), read(&a, "deep/er/leaf.txt")),
            ("two".into(), "two".into())
        );
        assert_eq!(
            (read(&b, "top.txt"), read(&b, "deep/er/leaf.txt")),
            ("one".into(), "one".into())
        );
        assert!(a.join("only-new.txt").is_file() && !b.join("only-new.txt").exists());
        assert!(!sibling(&a, "swap").exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_zip_archive_unpacks_and_keeps_the_executable_bit() {
        let base = temp("zip");
        let mut z = crate::zip::write(&[
            (
                "OpenFadeR.app/Contents/MacOS/openfader",
                b"program".to_vec(),
            ),
            ("OpenFadeR.app/Contents/Info.plist", b"plist".to_vec()),
        ]);
        // mark the first entry "made on Unix, mode 0755" in the central directory (the writer makes DOS entries)
        let at = z.windows(4).position(|w| w == [0x50, 0x4b, 1, 2]).unwrap();
        z[at + 4..at + 6].copy_from_slice(&0x031eu16.to_le_bytes());
        z[at + 38..at + 42].copy_from_slice(&(0o100755u32 << 16).to_le_bytes());
        let archive = base.join("OpenFadeR-9-macos-arm64.zip");
        fs::write(&archive, z).unwrap();
        let to = base.join("out");
        unpack(&archive, &to).unwrap();
        assert_eq!(fs::read(to.join("Contents/Info.plist")).unwrap(), b"plist");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(to.join("Contents/MacOS/openfader"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0o111
            );
            assert_eq!(
                fs::metadata(to.join("Contents/Info.plist"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        let two = base.join("two.zip");
        fs::write(
            &two,
            crate::zip::write(&[("a/x", vec![1]), ("b/y", vec![2])]),
        )
        .unwrap();
        assert!(unpack(&two, &base.join("out2"))
            .unwrap_err()
            .contains("more than one top folder"));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn only_a_writable_own_install_is_replaced() {
        let base = temp("locate");
        let root = base.join("lib/openfader");
        fs::create_dir_all(&root).unwrap();
        let exe = root.join("openfader");
        assert_eq!(
            locate_from(&exe),
            Ok(Install {
                root: root.clone(),
                exe: "openfader".into()
            })
        );
        assert_eq!(
            locate_from(Path::new("/usr/lib/openfader/openfader")),
            Err(Why::Package)
        );
        assert_eq!(
            locate_from(Path::new("/nix/store/x-openfader/bin/openfader")),
            Err(Why::Package)
        );
        let dev = base.join("target/release");
        fs::create_dir_all(&dev).unwrap();
        assert_eq!(locate_from(&dev.join("openfader")), Err(Why::Development));
        let missing = base.join("nowhere/openfader");
        assert_eq!(
            locate_from(&missing),
            Err(Why::ReadOnly),
            "a folder that cannot take a file"
        );
        let app = base.join("Applications/OpenFadeR.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        let found = locate_from(&app.join("Contents/MacOS/openfader")).unwrap();
        assert_eq!(
            (found.root, found.exe),
            (app, PathBuf::from("Contents/MacOS/openfader")),
            "a macOS bundle is replaced whole"
        );
        fs::remove_dir_all(&base).unwrap();
    }
}
