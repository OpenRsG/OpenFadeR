//! Native RAM budgets shared by device profiles and inference, including Linux cgroup v2 limits.

use std::sync::LazyLock;

pub const SMALL_BYTES: u64 = 8 << 30;

/// Effective RAM capacity, cached for the process's startup device profile.
pub fn total_bytes() -> Option<u64> {
    static TOTAL: LazyLock<Option<u64>> = LazyLock::new(|| budget().0);
    *TOTAL
}

/// Currently available RAM, limited by the process's cgroup headroom on Linux.
pub fn available_bytes() -> Option<u64> {
    budget().1
}

/// Unknown hardware takes the conservative inference admission path.
pub fn constrained() -> bool {
    total_bytes().is_none_or(|bytes| bytes <= SMALL_BYTES)
}

#[cfg(not(target_os = "linux"))]
fn budget() -> (Option<u64>, Option<u64>) {
    native()
}

#[cfg(target_os = "linux")]
fn budget() -> (Option<u64>, Option<u64>) {
    let (mut total, mut available) = native();
    if let Some(mut group) = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|text| cgroup_path(&text))
    {
        let root = std::path::Path::new("/sys/fs/cgroup");
        group = root.join(group);
        loop {
            let number = |name| {
                std::fs::read_to_string(group.join(name))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            };
            if let Some(limit) = number("memory.max") {
                cap(&mut total, &mut available, limit, number("memory.current"));
            }
            if group == root || !group.pop() {
                break;
            }
        }
    }
    (total, available)
}

#[cfg(target_os = "linux")]
fn cap(total: &mut Option<u64>, free: &mut Option<u64>, limit: u64, used: Option<u64>) {
    *total = Some(total.map_or(limit, |value| value.min(limit)));
    *free = free
        .zip(used)
        .map(|(free, used)| free.min(limit.saturating_sub(used)));
}

#[cfg(target_os = "linux")]
fn cgroup_path(text: &str) -> Option<std::path::PathBuf> {
    use std::path::{Component, Path};
    let path = Path::new(text.lines().find_map(|line| line.strip_prefix("0::"))?);
    let relative = path.strip_prefix("/").ok()?;
    relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
        .then(|| relative.to_owned())
}

#[cfg(target_os = "linux")]
fn kib_field(text: &str, field: &str) -> Option<u64> {
    let mut words = text
        .lines()
        .find_map(|line| line.strip_prefix(field))?
        .split_whitespace();
    let kib = words.next()?.parse::<u64>().ok()?;
    if words.next()? != "kB" || words.next().is_some() {
        return None;
    }
    kib.checked_mul(1024)
}

#[cfg(target_os = "linux")]
fn native() -> (Option<u64>, Option<u64>) {
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return (None, None);
    };
    (
        kib_field(&text, "MemTotal:"),
        kib_field(&text, "MemAvailable:"),
    )
}

#[cfg(windows)]
fn native() -> (Option<u64>, Option<u64>) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: the documented zeroed structure and its exact size are passed to Windows.
    let mut info: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    info.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut info) } == 0 {
        return (None, None);
    }
    (Some(info.ullTotalPhys), Some(info.ullAvailPhys))
}

#[cfg(target_os = "macos")]
fn native() -> (Option<u64>, Option<u64>) {
    let mut total = 0_u64;
    let mut size = std::mem::size_of::<u64>();
    // SAFETY: hw.memsize returns a u64 into an exactly sized writable buffer.
    let ok = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut total as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } == 0
        && size == std::mem::size_of::<u64>();
    (ok.then_some(total), mac_available())
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn mac_available() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::vm_statistics64_data_t>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: libc supplies the Mach ABI; release the acquired host send right on every outcome.
    let host = unsafe { libc::mach_host_self() };
    let status = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    unsafe { libc::mach_port_deallocate(libc::mach_task_self(), host) };
    if status != libc::KERN_SUCCESS || count != libc::HOST_VM_INFO64_COUNT {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .ok()
        .filter(|n| *n != 0)?;
    // free_count already includes speculative pages; inactive pages can also be reclaimed.
    (u64::from(info.free_count) + u64::from(info.inactive_count)).checked_mul(page)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native() -> (Option<u64>, Option<u64>) {
    (None, None)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn proc_values_are_checked_binary_bytes() {
        assert_eq!(
            kib_field("MemTotal: 2097152 kB\n", "MemTotal:"),
            Some(2 << 30)
        );
        assert_eq!(kib_field("MemTotal: 8 MB", "MemTotal:"), None);
        assert_eq!(
            kib_field("MemTotal: 18446744073709551615 kB", "MemTotal:"),
            None
        );
        assert_eq!(kib_field("MemFree: 8 kB", "MemTotal:"), None);
    }

    #[test]
    fn cgroup_capacity_and_headroom_take_the_strictest_ancestor() {
        let (mut total, mut free) = (Some(64 << 30), Some(32 << 30));
        cap(&mut total, &mut free, 4 << 30, Some(1 << 30));
        assert_eq!((total, free), (Some(4 << 30), Some(3 << 30)));
        cap(&mut total, &mut free, 8 << 30, Some(7 << 30));
        assert_eq!((total, free), (Some(4 << 30), Some(1 << 30)));
        cap(&mut total, &mut free, 2 << 30, Some(3 << 30));
        assert_eq!((total, free), (Some(2 << 30), Some(0)));
        cap(&mut total, &mut free, 2 << 30, None);
        assert_eq!(
            free, None,
            "unknown usage cannot promise room for preprocessing"
        );
    }

    #[test]
    fn cgroup_membership_is_relative_and_cannot_escape_its_mount() {
        assert_eq!(
            cgroup_path("0::/user.slice/app.scope\n"),
            Some("user.slice/app.scope".into())
        );
        assert_eq!(cgroup_path("0::/\n"), Some("".into()));
        assert_eq!(cgroup_path("0::/../../outside\n"), None);
        assert_eq!(cgroup_path("1:memory:/legacy"), None);
    }
}
