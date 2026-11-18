use std::io;
use std::time::{Duration, Instant};

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "windows.rs"]
mod imp;
pub(crate) use imp::*;

pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const MAX_MESSAGE: usize = 256;

pub(crate) fn timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "fixture control channel timed out")
}

pub(crate) fn channel_value(name: &str) -> io::Result<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("missing/invalid {name}"),
            )
        })
}

pub(crate) fn validate_channel() -> Result<(), String> {
    channel_value("CPF_FIXTURE_CTRL").map_err(|error| error.to_string())?;
    channel_value("CPF_FIXTURE_ACK").map_err(|error| error.to_string())?;
    Ok(())
}

/// Poll only inside the fixture/harness implementation, never to coordinate tests.
pub(crate) fn retry_pause(until: Instant) -> io::Result<()> {
    if Instant::now() >= until {
        return Err(timeout());
    }
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}
