use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Stdio;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::AsyncReadExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_BROKEN_PIPE, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

pub(in crate::supervisor) type Writer = OwnedHandle;

#[derive(Debug)]
pub(in crate::supervisor) struct Reader {
    server: NamedPipeServer,
    connected: bool,
    writer_closed: bool,
}

pub(in crate::supervisor) fn pipe() -> io::Result<(Reader, Writer)> {
    static NEXT_PIPE: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        r"\\.\pipe\cpforge-{}-{}-{:016x}",
        std::process::id(),
        NEXT_PIPE.fetch_add(1, Ordering::Relaxed),
        RandomState::new().build_hasher().finish(),
    );
    let server = ServerOptions::new()
        .access_inbound(true)
        .access_outbound(false)
        .pipe_mode(PipeMode::Byte)
        .reject_remote_clients(true)
        .first_pipe_instance(true)
        .max_instances(1)
        .create(&name)?;
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: name is NUL-terminated and live. Null security attributes make
    // the new writer non-inheritable; other optional pointers/handles are null.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a valid, newly owned handle above.
    let child = unsafe { OwnedHandle::from_raw_handle(handle) };
    Ok((
        Reader {
            server,
            connected: false,
            writer_closed: false,
        },
        child,
    ))
}

// The pipe layer never initiates CancelIoEx. Capture must recognise
// ERROR_OPERATION_ABORTED from its own cancellation as expected stop before
// returning ReadResult; it must not report that stop as a pipe failure. Tokio/Mio retain the I/O
// buffers and OVERLAPPED storage until completion, including on drop.
pub(in crate::supervisor) async fn read(reader: &mut Reader, buf: &mut [u8]) -> io::Result<usize> {
    if !reader.connected {
        // The client is already open. Tokio accepts ERROR_PIPE_CONNECTED (or
        // ERROR_NO_DATA if it already closed), and owns any pending overlapped
        // connect through completion, even on drop.
        reader.server.connect().await?;
        reader.connected = true;
    }
    loop {
        match reader.server.read(buf).await {
            Ok(0) if reader.writer_closed => return Ok(0),
            Ok(0) => {
                // Mio also translates ERROR_BROKEN_PIPE to Ok(0). Probe closure
                // so a successful zero-byte write is retried rather than EOF.
                // SAFETY: the server handle is borrowed and live; no buffer or
                // output pointers are supplied, so this only probes pipe state.
                if unsafe {
                    PeekNamedPipe(
                        reader.server.as_raw_handle(),
                        null_mut(),
                        0,
                        null_mut(),
                        null_mut(),
                        null_mut(),
                    )
                } == 0
                {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(ERROR_BROKEN_PIPE as i32) {
                        return Err(error);
                    }
                    // Drain any data already prefetched by Tokio before EOF.
                    reader.writer_closed = true;
                }
                tokio::task::yield_now().await;
            }
            Err(error) if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => return Ok(0),
            result => return result,
        }
    }
}

pub(in crate::supervisor) fn into_stdio(writer: Writer) -> Stdio {
    writer.into()
}

pub(in crate::supervisor) fn into_stdio_for_tests(writer: Writer) -> io::Result<Stdio> {
    let mut duplicate = null_mut();
    // SAFETY: the current-process pseudo handle and borrowed writer are live;
    // DuplicateHandle writes a new owned handle and explicitly enables inheritance.
    if unsafe {
        let process = GetCurrentProcess();
        DuplicateHandle(
            process,
            writer.as_raw_handle(),
            process,
            &mut duplicate,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle succeeded and transferred a new owned handle.
    Ok(Stdio::from(unsafe {
        OwnedHandle::from_raw_handle(duplicate)
    }))
}
