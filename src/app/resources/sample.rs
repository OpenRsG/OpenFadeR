//! Worker-thread-only process memory reads. VRAM is driver-reported resident/local memory, not
//! whole-device usage. Shared allocations retain the driver's accounting semantics.
//! Linux: DRM fdinfo and optional NVML; Windows: optional NVML (WDDM often reports unavailable).
//! macOS and system-memory-only GPUs have no VRAM estimate.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Sample {
    pub ram: Option<u64>,
    pub vram: Option<u64>,
}

pub(super) struct Reader {
    #[cfg(target_os = "linux")]
    drm: drm::Reader,
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    nvml: Option<nvml::Reader>,
}

impl Reader {
    // Construct inside the sampling worker: loading a driver can perform blocking I/O too.
    pub(super) fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            drm: drm::Reader::default(),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            nvml: nvml::Reader::new(),
        }
    }

    pub(super) fn sample(&mut self) -> Sample {
        #[cfg(target_os = "linux")]
        let ram = self.drm.ram();
        #[cfg(not(target_os = "linux"))]
        let ram = resident_memory();

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        let vram = self.nvml.as_mut().and_then(nvml::Reader::sample);
        #[cfg(target_os = "linux")]
        let vram = if self.nvml.is_some() && vram.is_none() {
            // Do not present a partial DRM total after an NVIDIA query failed.
            None
        } else {
            self.drm.vram(vram)
        };
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let vram = None;

        Sample { ram, vram }
    }
}

#[cfg(target_os = "windows")]
fn resident_memory() -> Option<u64> {
    // psapi.h PROCESS_MEMORY_COUNTERS; K32 entry is in kernel32 since Windows 7.
    #[repr(C)]
    struct Counters {
        cb: u32,
        faults: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged: usize,
        quota_paged: usize,
        quota_peak_nonpaged: usize,
        quota_nonpaged: usize,
        pagefile: usize,
        peak_pagefile: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn K32GetProcessMemoryInfo(
            process: windows_sys::Win32::Foundation::HANDLE,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    // SAFETY: all-zero Counters is valid and the OS receives its exact writable size.
    let mut counters: Counters = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<Counters>() as u32;
    let success = unsafe {
        K32GetProcessMemoryInfo(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<Counters>() as u32,
        )
    };
    (success != 0).then_some(counters.working_set as u64)
}

#[cfg(target_os = "macos")]
fn resident_memory() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info_data_t>::zeroed();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    // SAFETY: libc mirrors XNU's packed task_info.h structure and count is in natural_t units.
    #[allow(deprecated)]
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS || count != libc::MACH_TASK_BASIC_INFO_COUNT {
        return None;
    }
    Some(unsafe { info.assume_init() }.resident_size)
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn resident_memory() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
mod drm {
    use std::fs::File;
    use std::io::Read;

    #[derive(Default)]
    pub(super) struct Reader {
        text: String,
        clients: Vec<Client>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Client {
        device: Option<[u32; 4]>,
        id: u64,
        nvidia: bool,
        bytes: Option<u64>,
    }

    impl Reader {
        pub(super) fn ram(&mut self) -> Option<u64> {
            self.text.clear();
            File::open("/proc/self/status")
                .ok()?
                .read_to_string(&mut self.text)
                .ok()?;
            rss(&self.text)
        }

        pub(super) fn vram(&mut self, nvml: Option<u64>) -> Option<u64> {
            self.clients.clear();
            for entry in std::fs::read_dir("/proc/self/fdinfo").ok()? {
                let entry = entry.ok()?;
                self.text.clear();
                let read = File::open(entry.path())
                    .and_then(|mut file| file.read_to_string(&mut self.text));
                match read {
                    Ok(_) => {}
                    // Descriptors may close while we walk the directory.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => return None,
                }
                if let Some(client) = parse(&self.text).ok()? {
                    add_client(&mut self.clients, client);
                }
            }
            total(&self.clients, nvml)
        }
    }

    fn rss(text: &str) -> Option<u64> {
        let value = text.lines().find_map(|line| line.strip_prefix("VmRSS:"))?;
        let mut words = value.split_whitespace();
        let kib = words.next()?.parse::<u64>().ok()?;
        if words.next()? != "kB" || words.next().is_some() {
            return None;
        }
        kib.checked_mul(1024)
    }

    fn bytes(text: &str) -> Option<u64> {
        let mut words = text.split_whitespace();
        let value = words.next()?.parse::<u64>().ok()?;
        let factor = match words.next() {
            None => 1,
            Some("KiB") => 1024,
            Some("MiB") => 1024 * 1024,
            _ => return None,
        };
        if words.next().is_some() {
            return None;
        }
        value.checked_mul(factor)
    }

    fn pci(text: &str) -> Option<[u32; 4]> {
        let mut words = text.split([':', '.']);
        let mut result = [0; 4];
        for part in &mut result {
            *part = u32::from_str_radix(words.next()?, 16).ok()?;
        }
        words.next().is_none().then_some(result)
    }

    fn local_region(name: &str, driver: &str) -> bool {
        let suffix = name.strip_prefix("vram").or_else(|| {
            (driver == "i915")
                .then(|| {
                    name.strip_prefix("local")
                        .or_else(|| name.strip_prefix("stolen-local"))
                })
                .flatten()
        });
        suffix.is_some_and(|suffix| suffix.bytes().all(|byte| byte.is_ascii_digit()))
    }

    // https://www.kernel.org/doc/html/latest/gpu/drm-usage-stats.html
    // AMD's drm-memory-* aliases resident-*; Intel localN/xe vramN exclude system/GTT memory.
    fn parse(text: &str) -> Result<Option<Client>, ()> {
        let driver = text
            .lines()
            .find_map(|line| line.strip_prefix("drm-driver:"));
        let Some(driver) = driver.map(str::trim) else {
            return Ok(None);
        };
        let mut id = None;
        let mut device = None;
        // shortcut: cap at 32 local-memory regions/client; unknown larger layouts are unavailable.
        let mut regions = [("", None::<u64>, None::<u64>); 32];
        let mut region_count = 0;
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key {
                "drm-client-id" => id = Some(value.parse::<u64>().map_err(|_| ())?),
                "drm-pdev" => device = Some(pci(value).ok_or(())?),
                _ => {
                    let (region, modern) = if let Some(region) = key.strip_prefix("drm-resident-") {
                        (region, true)
                    } else if let Some(region) = key.strip_prefix("drm-memory-") {
                        (region, false)
                    } else {
                        continue;
                    };
                    if !local_region(region, driver) {
                        continue;
                    }
                    let index = if let Some(index) = regions[..region_count]
                        .iter()
                        .position(|entry| entry.0 == region)
                    {
                        index
                    } else {
                        if region_count == regions.len() {
                            return Err(());
                        }
                        regions[region_count].0 = region;
                        region_count += 1;
                        region_count - 1
                    };
                    let value = bytes(value).ok_or(())?;
                    let slot = if modern {
                        &mut regions[index].1
                    } else {
                        &mut regions[index].2
                    };
                    if slot.replace(value).is_some() {
                        return Err(());
                    }
                }
            }
        }
        let used = regions[..region_count]
            .iter()
            .try_fold(0_u64, |sum, region| sum.checked_add(region.1.or(region.2)?))
            .ok_or(())?;
        // Without an identity duplicated fds cannot be counted safely.
        Ok(Some(Client {
            device,
            id: id.ok_or(())?,
            nvidia: driver == "nvidia" || driver == "nvidia-drm",
            bytes: (region_count != 0).then_some(used),
        }))
    }

    fn add_client(clients: &mut Vec<Client>, client: Client) {
        if let Some(previous) = clients
            .iter_mut()
            .find(|old| old.device == client.device && old.id == client.id)
        {
            // Repeated fds can straddle an allocation update; retain the newest complete read.
            *previous = client;
        } else {
            clients.push(client);
        }
    }

    fn total(clients: &[Client], nvml: Option<u64>) -> Option<u64> {
        let mut result = nvml;
        for client in clients {
            // The two backends cover the same NVIDIA allocation, not separate memory.
            if client.nvidia && nvml.is_some() {
                continue;
            }
            result = Some(result.unwrap_or(0).checked_add(client.bytes?)?);
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rss_is_resident_bytes_not_virtual_or_peak() {
            assert_eq!(
                rss("VmPeak: 999 kB\nVmSize: 888 kB\nVmRSS:\t123 kB\n"),
                Some(125952)
            );
            assert_eq!(rss("VmRSS: 0 kB"), Some(0));
            assert_eq!(rss("VmRSS: 12 MB"), None);
            assert_eq!(rss("VmRSS: 18446744073709551615 kB"), None);
            assert_eq!(rss("VmSize: 10 kB"), None);
        }

        #[test]
        fn resident_regions_exclude_gtt_and_aliases() {
            let text = "drm-driver: amdgpu\ndrm-client-id: 7\ndrm-pdev: 0000:03:00.0\ndrm-resident-vram: 2 MiB\ndrm-memory-vram: 2 MiB\ndrm-resident-gtt: 500 MiB\ndrm-total-vram: 900 MiB\n";
            assert_eq!(parse(text).unwrap().unwrap().bytes, Some(2 * 1024 * 1024));
            let xe = "drm-driver: xe\ndrm-client-id: 1\ndrm-resident-vram0: 4 KiB\ndrm-resident-vram1: 8 KiB\n";
            assert_eq!(parse(xe).unwrap().unwrap().bytes, Some(12 * 1024));
            let intel = "drm-driver: i915\ndrm-client-id: 1\ndrm-resident-local0: 1024\ndrm-resident-system0: 8192\n";
            assert_eq!(parse(intel).unwrap().unwrap().bytes, Some(1024));
        }

        #[test]
        fn missing_invalid_and_zero_are_distinct() {
            assert_eq!(parse("pos: 0\n").unwrap(), None);
            let prefix = "drm-driver: amdgpu\ndrm-client-id: 1\n";
            assert_eq!(parse(prefix).unwrap().unwrap().bytes, None);
            assert_eq!(
                parse(&format!("{prefix}drm-memory-vram: 0 KiB"))
                    .unwrap()
                    .unwrap()
                    .bytes,
                Some(0)
            );
            assert!(parse(&format!("{prefix}drm-memory-vram: 1 GB")).is_err());
            assert!(parse("drm-driver: amdgpu\ndrm-memory-vram: 12 KiB").is_err());
        }

        #[test]
        fn clients_are_unique_per_device_and_nvml_is_not_added_twice() {
            let mut clients = Vec::new();
            let client = Client {
                device: Some([0, 3, 0, 0]),
                id: 1,
                nvidia: false,
                bytes: Some(10),
            };
            add_client(&mut clients, client);
            add_client(&mut clients, client);
            add_client(
                &mut clients,
                Client {
                    device: Some([0, 4, 0, 0]),
                    ..client
                },
            );
            assert_eq!(total(&clients, None), Some(20));
            add_client(
                &mut clients,
                Client {
                    id: 2,
                    nvidia: true,
                    bytes: Some(30),
                    ..client
                },
            );
            assert_eq!(total(&clients, Some(40)), Some(60));
            assert_eq!(total(&clients, None), Some(50));
            assert_eq!(total(&[], None), None);
            assert_eq!(total(&[], Some(0)), Some(0));
            add_client(
                &mut clients,
                Client {
                    id: 3,
                    bytes: None,
                    ..client
                },
            );
            assert_eq!(total(&clients, Some(40)), None);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod nvml {
    use libloading::Library;
    use std::ffi::c_void;

    type Device = *mut c_void;
    type Call = unsafe extern "C" fn() -> i32;
    type Count = unsafe extern "C" fn(*mut u32) -> i32;
    type Handle = unsafe extern "C" fn(u32, *mut Device) -> i32;
    type Processes = unsafe extern "C" fn(Device, *mut u32, *mut Process) -> i32;

    // nvml.h nvmlProcessInfo_v2_t == nvmlProcessInfo_t, used by both _v2 and _v3 APIs.
    // https://github.com/NVIDIA/go-nvml/blob/main/pkg/nvml/nvml.h (NVIDIA-distributed header).
    // Never bind the unversioned symbol: it writes the smaller nvmlProcessInfo_v1_t.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Process {
        pid: u32,
        used: u64,
        gpu_instance: u32,
        compute_instance: u32,
    }

    #[derive(Clone, Copy)]
    struct Allocation {
        gpu_instance: u32,
        compute_instance: u32,
        used: u64,
    }

    pub(super) struct Reader {
        _library: Library,
        shutdown: Call,
        graphics: Processes,
        compute: Processes,
        devices: Vec<Device>,
        buffer: Vec<Process>,
        allocations: Vec<Allocation>,
        pid: u32,
    }

    impl Reader {
        pub(super) fn new() -> Option<Self> {
            let library = load()?;
            // SAFETY: exact versioned signatures from nvml.h; library stays alive until after Drop.
            let (init, shutdown, count, handle, graphics, compute) = unsafe {
                (
                    *library.get::<Call>(b"nvmlInit_v2\0").ok()?,
                    *library.get::<Call>(b"nvmlShutdown\0").ok()?,
                    *library.get::<Count>(b"nvmlDeviceGetCount_v2\0").ok()?,
                    *library
                        .get::<Handle>(b"nvmlDeviceGetHandleByIndex_v2\0")
                        .ok()?,
                    *library
                        .get::<Processes>(b"nvmlDeviceGetGraphicsRunningProcesses_v3\0")
                        .or_else(|_| {
                            library.get::<Processes>(b"nvmlDeviceGetGraphicsRunningProcesses_v2\0")
                        })
                        .ok()?,
                    *library
                        .get::<Processes>(b"nvmlDeviceGetComputeRunningProcesses_v3\0")
                        .or_else(|_| {
                            library.get::<Processes>(b"nvmlDeviceGetComputeRunningProcesses_v2\0")
                        })
                        .ok()?,
                )
            };
            if unsafe { init() } != 0 {
                return None;
            }
            // Establish shutdown ownership before any fallible enumeration.
            let mut reader = Self {
                _library: library,
                shutdown,
                graphics,
                compute,
                devices: Vec::new(),
                buffer: vec![Process::default(); 16],
                allocations: Vec::new(),
                pid: std::process::id(),
            };
            let mut devices = 0;
            if unsafe { count(&mut devices) } != 0 || devices == 0 || devices > 64 {
                return None;
            }
            reader.devices.reserve(devices as usize);
            for index in 0..devices {
                let mut device = std::ptr::null_mut();
                if unsafe { handle(index, &mut device) } != 0 || device.is_null() {
                    return None;
                }
                reader.devices.push(device);
            }
            Some(reader)
        }

        pub(super) fn sample(&mut self) -> Option<u64> {
            let mut total = 0_u64;
            for index in 0..self.devices.len() {
                self.allocations.clear();
                for query in [self.graphics, self.compute] {
                    self.query(self.devices[index], query)?;
                }
                for allocation in &self.allocations {
                    total = total.checked_add(allocation.used)?;
                }
            }
            Some(total)
        }

        fn query(&mut self, device: Device, query: Processes) -> Option<()> {
            // shortcut: cap at 16K processes/device and three calls, report unavailable above this.
            for _ in 0..3 {
                let mut count = self.buffer.len() as u32;
                // SAFETY: buffer has initialized, correctly aligned entries for this exact ABI.
                let result = unsafe { query(device, &mut count, self.buffer.as_mut_ptr()) };
                if result == 7 {
                    let needed = count as usize;
                    if needed <= self.buffer.len() || needed > 16 * 1024 {
                        return None;
                    }
                    self.buffer.resize(needed, Process::default());
                    continue;
                }
                if result != 0 || count as usize > self.buffer.len() {
                    return None;
                }
                for process in &self.buffer[..count as usize] {
                    if process.pid == self.pid {
                        record(&mut self.allocations, *process)?;
                    }
                }
                return Some(());
            }
            None
        }
    }

    fn record(allocations: &mut Vec<Allocation>, process: Process) -> Option<()> {
        // NVML_VALUE_NOT_AVAILABLE is (unsigned long long)-1, notably under Windows WDDM.
        if process.used == u64::MAX {
            return None;
        }
        if let Some(old) = allocations.iter_mut().find(|old| {
            old.gpu_instance == process.gpu_instance
                && old.compute_instance == process.compute_instance
        }) {
            // Both lists report all memory used by the application, not additive portions.
            old.used = old.used.max(process.used);
        } else {
            allocations.push(Allocation {
                gpu_instance: process.gpu_instance,
                compute_instance: process.compute_instance,
                used: process.used,
            });
        }
        Some(())
    }

    impl Drop for Reader {
        fn drop(&mut self) {
            // SAFETY: exactly one successful init is owned here; _library has not dropped yet.
            unsafe { (self.shutdown)() };
        }
    }

    #[cfg(target_os = "linux")]
    fn load() -> Option<Library> {
        // SAFETY: NVML is an optional system driver library, never a plugin or per-frame load.
        unsafe { Library::new("libnvidia-ml.so.1") }.ok()
    }

    #[cfg(target_os = "windows")]
    fn load() -> Option<Library> {
        // Search System32, not the working directory. Older installations use NVSMI instead.
        if let Ok(library) =
            unsafe { libloading::os::windows::Library::load_with_flags("nvml.dll", 0x00000800) }
        {
            return Some(library.into());
        }
        let root = std::env::var_os("ProgramW6432").or_else(|| std::env::var_os("ProgramFiles"))?;
        let path = std::path::PathBuf::from(root).join("NVIDIA Corporation/NVSMI/nvml.dll");
        if !path.is_absolute() {
            return None;
        }
        // SAFETY: absolute driver path; restrict dependency resolution to this directory/System32.
        unsafe { libloading::os::windows::Library::load_with_flags(path, 0x00000100 | 0x00000800) }
            .ok()
            .map(Into::into)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn overlapping_graphics_compute_records_are_not_additive() {
            let mut allocations = Vec::new();
            let process = Process {
                pid: 7,
                used: 10,
                gpu_instance: u32::MAX,
                compute_instance: u32::MAX,
            };
            record(&mut allocations, process).unwrap();
            record(
                &mut allocations,
                Process {
                    used: 12,
                    ..process
                },
            )
            .unwrap();
            assert_eq!(allocations.len(), 1);
            assert_eq!(allocations[0].used, 12);
            record(
                &mut allocations,
                Process {
                    gpu_instance: 1,
                    compute_instance: 2,
                    ..process
                },
            )
            .unwrap();
            assert_eq!(allocations.iter().map(|item| item.used).sum::<u64>(), 22);
        }

        #[test]
        fn unavailable_is_not_zero_and_abi_has_v2_tail() {
            let mut allocations = Vec::new();
            assert!(
                record(
                    &mut allocations,
                    Process {
                        used: u64::MAX,
                        ..Process::default()
                    }
                )
                .is_none()
            );
            assert!(allocations.is_empty());
            record(&mut allocations, Process::default()).unwrap();
            assert_eq!(allocations[0].used, 0);
            assert_eq!(
                std::mem::offset_of!(Process, used),
                std::mem::align_of::<u64>()
            );
            assert_eq!(
                std::mem::offset_of!(Process, compute_instance),
                std::mem::offset_of!(Process, gpu_instance) + 4
            );
        }
    }
}
