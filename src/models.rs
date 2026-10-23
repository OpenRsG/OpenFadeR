//! Model files fetched on first use (never shipped in the repository): the cache folder they live in and one verified
//! download shared by STEMS ([`crate::stems::model`]) and the NEURAL key detector ([`crate::analysis::key`]).

use ring::digest::{Context, SHA256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `<cache folder>/models`.
pub fn dir() -> Option<PathBuf> {
    Some(crate::paths::cache_dir()?.join("models"))
}

/// Downloads `url` to `dest` (through `dest` + `.part`, renamed on success, so a failure never leaves a truncated model),
/// calling `got` with the bytes received so far. The result must be `size` bytes and, when `sha256` (lower-case hex) is
/// given, hash to it: anything else is deleted and an error naming what was seen.
pub fn download(
    url: &str,
    dest: &Path,
    size: u64,
    sha256: Option<&str>,
    got: &dyn Fn(u64),
) -> Result<(), String> {
    std::fs::create_dir_all(dest.parent().ok_or("model path has no folder")?)
        .map_err(|e| e.to_string())?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .build()
        .into();
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut body = resp.into_body().into_reader();
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let fetched = (|| {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&part)?);
        let (mut buf, mut n, mut hash) = (vec![0u8; 1 << 16], 0u64, Context::new(&SHA256));
        loop {
            let k = body.read(&mut buf)?;
            if k == 0 {
                break;
            }
            out.write_all(&buf[..k])?;
            hash.update(&buf[..k]);
            n += k as u64;
            got(n);
        }
        out.flush()?;
        Ok::<_, std::io::Error>((n, hash.finish()))
    })();
    let verdict = fetched.map_err(|e| e.to_string()).and_then(|(n, digest)| {
        let seen = digest
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if n != size {
            Err(format!("got {n} of {size} bytes"))
        } else if sha256.is_some_and(|want| want != seen) {
            Err(format!("sha256 mismatch: {seen}"))
        } else {
            Ok(())
        }
    });
    match verdict {
        Ok(()) => std::fs::rename(&part, dest).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves `body` once on a local port, then returns its URL.
    fn serve(body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/m.onnx", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut b = [0u8; 512];
            while !seen.ends_with(b"\r\n\r\n") {
                let n = c.read(&mut b).unwrap();
                seen.extend_from_slice(&b[..n]);
            }
            write!(
                c,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            c.write_all(body).unwrap();
        });
        url
    }

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("openfader-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// sha256("abc"), the FIPS 180 example.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn a_download_is_kept_only_when_its_size_and_hash_match() {
        let dir = scratch("ok");
        let dest = dir.join("sub").join("m.onnx");
        let seen = std::sync::atomic::AtomicU64::new(0);
        let note = |n| seen.store(n, std::sync::atomic::Ordering::Relaxed);
        download(&serve(b"abc"), &dest, 3, Some(ABC), &note).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "progress reached the end"
        );

        for (name, size, sha, why) in [
            ("hash", 3, Some(&ABC[..63]), "sha256 mismatch"),
            ("short", 4, Some(ABC), "got 3 of 4 bytes"),
        ] {
            let bad = dir.join(format!("{name}.onnx"));
            let e = download(&serve(b"abc"), &bad, size, sha, &|_| {}).unwrap_err();
            assert!(e.starts_with(why), "{e}");
            assert!(!bad.exists(), "a wrong file is not installed");
            assert!(
                !dir.join(format!("{name}.onnx.part")).exists(),
                "and leaves no part file"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreachable_server_is_an_error_not_a_file() {
        let dir = scratch("down");
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/m", dead.local_addr().unwrap());
        drop(dead);
        assert!(download(&url, &dir.join("m"), 1, None, &|_| {}).is_err());
        assert!(!dir.join("m").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
