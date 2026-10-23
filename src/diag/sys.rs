//! What machine the app runs on, for the report: OS, CPU and RAM, read once in the background (a command or a file each).

#[derive(Clone, Debug, Default)]
pub struct Sys {
    pub os: String,
    pub cpu: String,
    pub ram: String,
}

#[cfg(any(target_os = "macos", windows))]
fn command(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// The value of the first `key<spaces>:` line of /proc/cpuinfo or /proc/meminfo style text.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim())
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

#[cfg(target_os = "linux")]
pub fn read() -> Sys {
    let file = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let release = file("/etc/os-release");
    let distro = release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map_or("Linux", |v| v.trim_matches('"'));
    let info = file("/proc/cpuinfo");
    // x86 says `model name`; Raspberry Pi and other ARM boards `Model` or `Hardware`.
    let cpu = ["model name", "Model", "Hardware"]
        .iter()
        .find_map(|k| field(&info, k))
        .unwrap_or("unknown");
    let ram = field(&file("/proc/meminfo"), "MemTotal")
        .and_then(|v| v.trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or_else(|| "unknown".into(), |kb| gib(kb * 1024));
    Sys {
        os: format!(
            "{distro}, kernel {}",
            file("/proc/sys/kernel/osrelease").trim()
        ),
        cpu: format!("{cpu}, {} threads", threads()),
        ram,
    }
}

#[cfg(target_os = "macos")]
pub fn read() -> Sys {
    let sysctl = |k: &str| command("sysctl", &["-n", k]);
    Sys {
        os: format!(
            "macOS {}",
            command("sw_vers", &["-productVersion"]).unwrap_or_default()
        ),
        cpu: format!(
            "{}, {} threads",
            sysctl("machdep.cpu.brand_string").unwrap_or_else(|| "unknown".into()),
            threads()
        ),
        ram: sysctl("hw.memsize")
            .and_then(|v| v.parse().ok())
            .map_or_else(|| "unknown".into(), gib),
    }
}

#[cfg(windows)]
pub fn read() -> Sys {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: an all-zero MEMORYSTATUSEX with its length set is what the call asks for.
    let ram = unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        (GlobalMemoryStatusEx(&mut m) != 0).then_some(m.ullTotalPhys)
    };
    // `ver` prints "Microsoft Windows [Version 10.0.22631.4037]".
    let ver = command("cmd", &["/C", "ver"]).unwrap_or_else(|| "Windows".into());
    let cpu = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".into());
    Sys {
        os: ver.trim_matches(['[', ']']).to_owned(),
        cpu: format!("{cpu}, {} threads", threads()),
        ram: ram.map_or_else(|| "unknown".into(), gib),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn read() -> Sys {
    let unknown = || "unknown".to_owned();
    Sys {
        os: std::env::consts::OS.to_owned(),
        cpu: format!("{}, {} threads", unknown(), threads()),
        ram: unknown(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_and_memory_fields_are_read_from_proc_style_text() {
        let info = "processor\t: 0\nmodel name\t: Test CPU @ 3.0GHz\nMemTotal:       16384000 kB\n";
        assert_eq!(field(info, "model name"), Some("Test CPU @ 3.0GHz"));
        assert_eq!(field(info, "MemTotal"), Some("16384000 kB"));
        assert_eq!(gib(16 * (1 << 30)), "16.0 GiB");
    }
}
