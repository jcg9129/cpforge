use super::{HANDSHAKE_TIMEOUT, MAX_MESSAGE, channel_value, retry_pause};
use crate::script::Stream;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::{Child, Command};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    FILETIME, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    CreateEventW, ExitProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, SetEvent, WaitForSingleObject,
};

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: each caller transfers a newly created, valid, uniquely owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn raw(handle: &impl AsRawHandle) -> HANDLE {
    handle.as_raw_handle()
}

fn security() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    }
}

fn pipe() -> io::Result<(File, File)> {
    let mut read = null_mut();
    let mut write = null_mut();
    // SAFETY: CreatePipe gets valid output pointers and security attributes.
    if unsafe { CreatePipe(&mut read, &mut write, &security(), 4096) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((File::from(owned(read)?), File::from(owned(write)?)))
}

fn noninheritable(handle: &File) -> io::Result<()> {
    // SAFETY: handle is live, and these flags only change inheritance.
    if unsafe { SetHandleInformation(raw(handle), HANDLE_FLAG_INHERIT, 0) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Server {
    read: File,
    write: File,
}

#[derive(Debug)]
pub(crate) struct ChildChannel {
    read: File,
    write: File,
}

pub(crate) fn channel() -> io::Result<(Server, ChildChannel)> {
    let (server_read, child_write) = pipe()?;
    let (child_read, server_write) = pipe()?;
    noninheritable(&server_read)?;
    noninheritable(&server_write)?;
    Ok((
        Server {
            read: server_read,
            write: server_write,
        },
        ChildChannel {
            read: child_read,
            write: child_write,
        },
    ))
}

impl ChildChannel {
    pub(crate) fn configure(&self, command: &mut Command) {
        command.env("CPF_FIXTURE_CTRL", (raw(&self.write) as usize).to_string());
        command.env("CPF_FIXTURE_ACK", (raw(&self.read) as usize).to_string());
    }
}

fn peek(handle: HANDLE, buffer: &mut [u8]) -> io::Result<usize> {
    let mut read = 0;
    // SAFETY: buffer and count pointer are valid; optional outputs are null.
    if unsafe {
        PeekNamedPipe(
            handle,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut read,
            null_mut(),
            null_mut(),
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(read as usize)
    }
}

impl Server {
    pub(crate) fn receive(&self, within: Duration) -> io::Result<String> {
        let until = Instant::now() + within;
        let mut message = Vec::new();
        let mut byte = [0];
        let mut input = &self.read;
        loop {
            if peek(raw(&self.read), &mut byte)? > 0 {
                input.read_exact(&mut byte)?;
                message.push(byte[0]);
                if message.len() > MAX_MESSAGE {
                    return Err(io::Error::other("oversized READY message"));
                }
                if byte[0] == b'\n' {
                    return String::from_utf8(message).map_err(io::Error::other);
                }
            } else {
                retry_pause(until)?;
            }
        }
    }

    pub(crate) fn acknowledge(&self, pid: u32) -> io::Result<()> {
        (&self.write).write_all(format!("ACK {pid}\n").as_bytes())
    }
}

fn inherited_file(name: &str) -> io::Result<File> {
    let handle = channel_value(name)? as HANDLE;
    // The fixture calls ready sequentially. Duplicate the inherited handle so
    // this temporary File's drop cannot close the channel needed by a later ready.
    let mut duplicate = null_mut();
    // SAFETY: the current-process pseudo handle is valid; DuplicateHandle checks
    // the inherited value and transfers a new owning handle on success.
    unsafe {
        use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let current = GetCurrentProcess();
        if DuplicateHandle(
            current,
            handle,
            current,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(File::from(owned(duplicate)?))
}

pub(crate) fn ready() -> io::Result<()> {
    let pid = std::process::id();
    let handle = open_process(pid)?;
    let start_id = creation_time(raw(&handle))?;
    let mut output = inherited_file("CPF_FIXTURE_CTRL")?;
    output.write_all(format!("READY {pid} {start_id}\n").as_bytes())?;
    let mut input = inherited_file("CPF_FIXTURE_ACK")?;
    let until = Instant::now() + HANDSHAKE_TIMEOUT;
    let expected = format!("ACK {pid}\n");
    let mut buffer = [0; MAX_MESSAGE];
    loop {
        let n = peek(raw(&input), &mut buffer)?;
        if let Some(end) = buffer[..n].iter().position(|&byte| byte == b'\n') {
            if buffer[..=end] == *expected.as_bytes() {
                // Other processes only peek at this PID's token. Only this
                // process consumes it, including the terminating newline.
                input.read_exact(&mut buffer[..=end])?;
                return Ok(());
            }
        }
        retry_pause(until)?;
    }
}

#[derive(Debug)]
pub(crate) struct Tree {
    job: OwnedHandle,
    gate: OwnedHandle,
}

impl Tree {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: null security makes the job noninheritable; no name is used.
        let job = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        // SAFETY: the Windows structure has a valid all-zero initial state.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: a live job and the correctly sized structure are supplied.
        if unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: create an inheritable manual-reset event, initially unsignalled.
        let gate = owned(unsafe { CreateEventW(&security(), 1, 0, null()) })?;
        Ok(Self { job, gate })
    }

    pub(crate) fn configure(&self, command: &mut Command) {
        command.env("CPF_FIXTURE_GATE", (raw(&self.gate) as usize).to_string());
    }

    pub(crate) fn attach(&mut self, child: &Child) -> io::Result<()> {
        // SAFETY: both handles are live. The fixture is blocked on gate before
        // parsing/executing its script, so it cannot spawn outside the job.
        if unsafe { AssignProcessToJobObject(raw(&self.job), raw(child)) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the event is live; descendants inherit its signalled state.
        if unsafe { SetEvent(raw(&self.gate)) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn kill(&self) -> io::Result<()> {
        // SAFETY: the job is live and owned by this harness.
        if unsafe { TerminateJobObject(raw(&self.job), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

pub(crate) fn startup() -> io::Result<()> {
    if std::env::var_os("CPF_FIXTURE_GATE").is_some() {
        let gate = channel_value("CPF_FIXTURE_GATE")? as HANDLE;
        // SAFETY: WaitForSingleObject validates the inherited event handle.
        let result = unsafe { WaitForSingleObject(gate, 5000) };
        if result != WAIT_OBJECT_0 {
            return Err(if result == WAIT_TIMEOUT {
                super::timeout()
            } else {
                io::Error::last_os_error()
            });
        }
    }
    Ok(())
}

fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: no pointers are passed; request only query and wait access.
    owned(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    })
}

fn creation_time(handle: HANDLE) -> io::Result<u64> {
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let [creation, exit, kernel, user] = &mut times;
    // SAFETY: all four outputs are valid and distinct; the handle is borrowed.
    if unsafe { GetProcessTimes(handle, creation, exit, kernel, user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

#[derive(Clone, Debug)]
pub(crate) struct Process(Arc<OwnedHandle>);

impl Process {
    pub(crate) fn open(pid: u32, expected: u64) -> io::Result<Self> {
        let handle = open_process(pid)?;
        if creation_time(raw(&handle))? != expected {
            return Err(io::Error::other("READY identity changed before ACK"));
        }
        Ok(Self(Arc::new(handle)))
    }

    pub(crate) fn is_live(&self, _pid: u32, _start_id: u64) -> bool {
        // SAFETY: the held process handle remains valid for this zero-time wait.
        unsafe { WaitForSingleObject(raw(self.0.as_ref()), 0) != WAIT_OBJECT_0 }
    }
}

pub(crate) fn validate_signal(name: &str) -> Result<(), String> {
    match name {
        "SEGV" | "ABRT" => Ok(()),
        _ => Err(format!("unsupported Windows signal: {name}")),
    }
}

pub(crate) fn signal(name: &str) -> io::Result<()> {
    if name == "SEGV" {
        exit(0xC000_0005);
    }
    std::process::abort();
}

pub(crate) fn exit(code: u32) -> ! {
    // SAFETY: the fixture explicitly requests process termination with this u32.
    unsafe { ExitProcess(code) }
}

pub(crate) fn close(stream: Stream) -> io::Result<()> {
    let id = match stream {
        Stream::Stdin => STD_INPUT_HANDLE,
        Stream::Stdout => STD_OUTPUT_HANDLE,
        Stream::Stderr => STD_ERROR_HANDLE,
    };
    // SAFETY: GetStdHandle borrows the standard handle; the step transfers it to
    // an owning wrapper for one close and clears the standard slot first.
    let handle = unsafe { GetStdHandle(id) };
    // SAFETY: null is a supported value for an absent standard handle.
    if unsafe { SetStdHandle(id, null_mut()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(owned(handle)?);
    Ok(())
}

pub(crate) fn escape() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "escape is Unix-only",
    ))
}

pub(crate) fn ignore_term() -> io::Result<()> {
    Ok(())
}
