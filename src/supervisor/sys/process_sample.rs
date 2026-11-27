//! Whole-process observations for isolated, process-free lifecycle fixtures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessSample {
    /// Every thread on Linux and macOS; on Windows only the threads whose
    /// start address lies in the cpf executable image (stress ST-L06).
    pub threads: usize,
    /// Windows: threads starting in `ntdll.dll` (the system thread pool,
    /// started and retired by Windows itself); 0 elsewhere.
    pub system_threads: usize,
    /// Windows: threads starting in any other module, or of unknown start;
    /// 0 elsewhere.
    pub foreign_threads: usize,
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
        system_threads: 0,
        foreign_threads: 0,
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
        system_threads: 0,
        foreign_threads: 0,
        descriptors: std::fs::read_dir("/dev/fd").unwrap().count(),
        resident: info.resident_size,
    }
}

/// One line per thread of this process, for a failed thread-count equality.
#[cfg(target_os = "linux")]
pub(crate) fn thread_report() -> String {
    let mut report = String::new();
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        report.push_str(&format!(
            "thread {} name {:?}\n",
            task.file_name().to_string_lossy(),
            name.trim_end()
        ));
    }
    report
}

/// One line per thread of this process, for a failed thread-count equality.
#[cfg(target_os = "macos")]
pub(crate) fn thread_report() -> String {
    String::from("(per-thread details are not collected on macOS)\n")
}

/// Where a thread of this process starts (Windows).
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Origin {
    /// The cpf executable image.
    Image,
    /// `ntdll.dll`: the system thread pool.
    System,
    /// Any other module, or a start address that could not be read.
    Foreign,
}

#[cfg(windows)]
#[derive(Debug)]
struct ThreadStart {
    id: u32,
    start: usize,
    origin: Origin,
    module: String,
    name: String,
}

/// Every thread of this process from a Toolhelp snapshot, with its Win32
/// start address (`NtQueryInformationThread`, ThreadQuerySetWin32StartAddress),
/// the module containing it and the thread description.
#[cfg(windows)]
fn thread_starts() -> Vec<ThreadStart> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, HMODULE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW, GetProcAddress,
    };
    use windows_sys::Win32::System::Threading::{
        GetThreadDescription, OpenThread, THREAD_QUERY_INFORMATION,
        THREAD_QUERY_LIMITED_INFORMATION,
    };
    type QueryThread =
        unsafe extern "system" fn(HANDLE, i32, *mut core::ffi::c_void, u32, *mut u32) -> i32;
    const THREAD_QUERY_SET_WIN32_START_ADDRESS: i32 = 9;
    let mut threads = Vec::new();
    let ntdll_name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    // SAFETY: ntdll is mapped into every process; the transmuted pointer has
    // NtQueryInformationThread's documented signature. Every buffer below is
    // writable for the size passed; opened handles are closed, the described
    // string is freed with LocalFree, and module lookups take no reference.
    unsafe {
        let image = GetModuleHandleW(std::ptr::null());
        let ntdll = GetModuleHandleW(ntdll_name.as_ptr());
        assert!(!image.is_null() && !ntdll.is_null());
        let query = GetProcAddress(ntdll, c"NtQueryInformationThread".as_ptr().cast())
            .map(|f| std::mem::transmute::<unsafe extern "system" fn() -> isize, QueryThread>(f));
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        assert_ne!(snapshot, INVALID_HANDLE_VALUE);
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut next = Thread32First(snapshot, &mut entry);
        assert_ne!(next, 0);
        while next != 0 {
            if entry.th32OwnerProcessID == std::process::id() {
                let mut start = 0usize;
                let mut name = String::new();
                let thread = OpenThread(
                    THREAD_QUERY_INFORMATION | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ThreadID,
                );
                if !thread.is_null() {
                    if let Some(query) = query {
                        query(
                            thread,
                            THREAD_QUERY_SET_WIN32_START_ADDRESS,
                            (&mut start as *mut usize).cast(),
                            std::mem::size_of::<usize>() as u32,
                            std::ptr::null_mut(),
                        );
                    }
                    let mut description = std::ptr::null_mut();
                    if GetThreadDescription(thread, &mut description) >= 0 && !description.is_null()
                    {
                        let mut len = 0;
                        while *description.add(len) != 0 {
                            len += 1;
                        }
                        name =
                            String::from_utf16_lossy(std::slice::from_raw_parts(description, len));
                        LocalFree(description.cast());
                    }
                    CloseHandle(thread);
                }
                let mut handle: HMODULE = std::ptr::null_mut();
                let mut module = String::new();
                if start != 0
                    && GetModuleHandleExW(
                        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                            | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                        start as *const u16,
                        &mut handle,
                    ) != 0
                {
                    let mut path = [0u16; 512];
                    let n = GetModuleFileNameW(handle, path.as_mut_ptr(), path.len() as u32);
                    module = String::from_utf16_lossy(&path[..(n as usize).min(path.len())]);
                } else {
                    handle = std::ptr::null_mut();
                }
                // A null handle (no start address or no module) is foreign.
                let origin = if handle == image {
                    Origin::Image
                } else if handle == ntdll {
                    Origin::System
                } else {
                    Origin::Foreign
                };
                threads.push(ThreadStart {
                    id: entry.th32ThreadID,
                    start,
                    origin,
                    module,
                    name,
                });
            }
            next = Thread32Next(snapshot, &mut entry);
        }
        assert_ne!(CloseHandle(snapshot), 0);
    }
    threads
}

#[cfg(windows)]
pub(crate) fn process_sample() -> ProcessSample {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let starts = thread_starts();
    let count = |origin| starts.iter().filter(|t| t.origin == origin).count();
    // SAFETY: the output buffers are initialized to their native sizes; the
    // current-process pseudo handle needs no closing.
    unsafe {
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
            threads: count(Origin::Image),
            system_threads: count(Origin::System),
            foreign_threads: count(Origin::Foreign),
            descriptors: handles as usize,
            resident: memory.WorkingSetSize as u64,
        }
    }
}

/// One line per thread of this process, for a failed thread-count check.
#[cfg(windows)]
pub(crate) fn thread_report() -> String {
    let mut report = String::new();
    for thread in thread_starts() {
        report.push_str(&format!(
            "thread {} {:?} start {:#x} module {:?} name {:?}\n",
            thread.id, thread.origin, thread.start, thread.module, thread.name
        ));
    }
    report
}
