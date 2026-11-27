//! Whole-process observations for isolated, process-free lifecycle fixtures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessSample {
    pub threads: usize,
    pub descriptors: usize,
    pub resident: u64,
}

#[cfg(target_os = "linux")]
pub(crate) fn process_sample() -> ProcessSample {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let rss = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .unwrap();
    ProcessSample {
        threads: std::fs::read_dir("/proc/self/task").unwrap().count(),
        descriptors: std::fs::read_dir("/proc/self/fd").unwrap().count(),
        resident: rss
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            * 1024,
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn process_sample() -> ProcessSample {
    // Mach task_basic_info_64 (MACH_TASK_BASIC_INFO = 20). Declare the native
    // entry points here to avoid deprecated libc compatibility aliases.
    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code, reason = "native layout; only the resident size is read")]
    struct BasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [i32; 2],
        system_time: [i32; 2],
        policy: i32,
        suspend_count: i32,
    }
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn task_threads(task: u32, threads: *mut *mut u32, count: *mut u32) -> i32;
        fn task_info(task: u32, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
        fn mach_port_deallocate(task: u32, port: u32) -> i32;
        fn vm_deallocate(task: u32, address: usize, size: usize) -> i32;
    }
    let mut ports = std::ptr::null_mut();
    let mut threads = 0;
    let mut info = BasicInfo::default();
    let mut count = (std::mem::size_of::<BasicInfo>() / 4) as u32;
    // SAFETY: pointers name writable buffers of the declared sizes. Mach owns
    // the returned port array; release every send right and its VM allocation.
    unsafe {
        let task = mach_task_self_;
        assert_eq!(task_threads(task, &mut ports, &mut threads), 0);
        for port in std::slice::from_raw_parts(ports, threads as usize) {
            assert_eq!(mach_port_deallocate(task, *port), 0);
        }
        assert_eq!(vm_deallocate(task, ports as usize, threads as usize * 4), 0);
        assert_eq!(
            task_info(task, 20, (&mut info as *mut BasicInfo).cast(), &mut count),
            0
        );
    }
    assert_eq!(count as usize * 4, std::mem::size_of::<BasicInfo>());
    ProcessSample {
        threads: threads as usize,
        descriptors: std::fs::read_dir("/dev/fd").unwrap().count(),
        resident: info.resident_size,
    }
}

#[cfg(windows)]
pub(crate) fn process_sample() -> ProcessSample {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    // SAFETY: all output buffers are initialized to their native sizes. The
    // snapshot is a newly owned handle, closed before counting process handles.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        assert_ne!(snapshot, INVALID_HANDLE_VALUE);
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut next = Thread32First(snapshot, &mut entry);
        assert_ne!(next, 0);
        let mut threads = 0;
        while next != 0 {
            threads += usize::from(entry.th32OwnerProcessID == std::process::id());
            next = Thread32Next(snapshot, &mut entry);
        }
        assert_ne!(CloseHandle(snapshot), 0);
        let process = GetCurrentProcess();
        let mut handles = 0;
        assert_ne!(GetProcessHandleCount(process, &mut handles), 0);
        let mut memory: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        assert_ne!(
            GetProcessMemoryInfo(
                process,
                &mut memory,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32
            ),
            0
        );
        ProcessSample {
            threads,
            descriptors: handles as usize,
            resident: memory.WorkingSetSize as u64,
        }
    }
}
