use crate::platform;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug)]
pub(crate) enum Step {
    Ready,
    Exit(u32),
    Signal(String),
    Sleep(u64),
    Spin(u64),
    Write(Stream, usize, u8),
    Flood(usize),
    Echo,
    EchoArgv(Vec<String>),
    PingPong(usize),
    Pong(usize),
    Drain,
    ReadExactly(usize),
    DefaultSigpipe,
    Close(Stream),
    Memory(usize),
    Spawn(usize, Vec<String>),
    Escape,
    IgnoreTerm,
    Hold,
    WaitFile(std::path::PathBuf),
    #[cfg(unix)]
    IdentityFile(std::path::PathBuf),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Stream {
    Stdin,
    Stdout,
    Stderr,
}

fn number<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("invalid unsigned integer: {text}"));
    }
    text.parse().map_err(|_| format!("out of range: {text}"))
}

fn stream(text: &str) -> Result<Stream, String> {
    match text {
        "stdin" => Ok(Stream::Stdin),
        "stdout" => Ok(Stream::Stdout),
        "stderr" => Ok(Stream::Stderr),
        _ => Err(format!("unknown stream: {text}")),
    }
}

pub(crate) fn parse(args: &[String], depth: usize) -> Result<Vec<Step>, String> {
    if args.is_empty() {
        return Err("expected at least one step".into());
    }
    // This mode treats every subsequent element as data, including empty
    // strings and text that would otherwise be step syntax.
    if args[0] == "echo-argv" {
        return Ok(vec![Step::EchoArgv(args[1..].to_vec())]);
    }
    args.iter().map(|arg| parse_step(arg, depth)).collect()
}

fn parse_step(arg: &str, depth: usize) -> Result<Step, String> {
    if let Some(rest) = arg.strip_prefix("spawn:") {
        if depth >= 2 {
            return Err("only one nested spawn is supported".into());
        }
        let (count, script) = rest.split_once(':').ok_or("spawn needs count:script")?;
        let delimiter = if depth == 0 { ',' } else { ';' };
        let args: Vec<_> = script.split(delimiter).map(String::from).collect();
        parse(&args, depth + 1)?;
        // The child receives its script as argv, so its nested spawn becomes a
        // top-level spawn and uses commas at that point.
        let args = if depth == 0 {
            args.into_iter()
                .map(|step| step.replace(';', ","))
                .collect()
        } else {
            args
        };
        return Ok(Step::Spawn(number(count)?, args));
    }
    #[cfg(unix)]
    if let Some(path) = arg.strip_prefix("identity-file:") {
        return Ok(Step::IdentityFile(path.into()));
    }
    if let Some(path) = arg.strip_prefix("wait-file:") {
        return Ok(Step::WaitFile(path.into()));
    }
    let parts: Vec<_> = arg.split(':').collect();
    let step = match parts.as_slice() {
        ["ready"] => {
            platform::validate_channel()?;
            Step::Ready
        }
        ["exit", code] => {
            let code: u32 = number(code)?;
            if cfg!(unix) && code > 255 {
                return Err("exit code must be 0..255 on Unix".into());
            }
            Step::Exit(code)
        }
        ["signal", name] => {
            let name = name.strip_prefix("SIG").unwrap_or(name);
            platform::validate_signal(name)?;
            Step::Signal(name.into())
        }
        ["sleep", ms] => Step::Sleep(number(ms)?),
        ["spin", ms] => Step::Spin(number(ms)?),
        ["write", target, count] | ["write", target, count, _] => {
            let target = stream(target)?;
            if matches!(target, Stream::Stdin) {
                return Err("cannot write stdin".into());
            }
            let byte = if let Some(byte) = parts.get(3) {
                if byte.len() != 1 {
                    return Err("write byte must be exactly one byte".into());
                }
                byte.as_bytes()[0]
            } else {
                b'x'
            };
            Step::Write(target, number(count)?, byte)
        }
        ["write-hex", target, count, hex] => {
            let target = stream(target)?;
            if matches!(target, Stream::Stdin) {
                return Err("cannot write stdin".into());
            }
            let byte = u8::from_str_radix(hex, 16).map_err(|_| "invalid hex byte")?;
            Step::Write(target, number(count)?, byte)
        }
        ["flood", count] => Step::Flood(number(count)?),
        ["echo"] => Step::Echo,
        ["pingpong", count] => Step::PingPong(number(count)?),
        ["pong", count] => Step::Pong(number(count)?),
        ["drain-stdin"] => Step::Drain,
        ["read-exactly", count] => Step::ReadExactly(number(count)?),
        ["default-sigpipe"] if cfg!(unix) => Step::DefaultSigpipe,
        ["close", target] => Step::Close(stream(target)?),
        ["touch-mem", mib] => Step::Memory(
            number::<usize>(mib)?
                .checked_mul(1024 * 1024)
                .ok_or("memory size overflow")?,
        ),
        ["escape"] if cfg!(unix) => Step::Escape,
        ["ignore-term"] => Step::IgnoreTerm,
        ["hold"] => Step::Hold,
        _ => return Err(format!("unknown step or bad arguments: {arg}")),
    };
    Ok(step)
}

fn write_bytes(mut output: impl Write, mut count: usize, byte: u8) -> io::Result<()> {
    let chunk = [byte; 64 * 1024];
    while count > 0 {
        let size = count.min(chunk.len());
        output.write_all(&chunk[..size])?;
        count -= size;
    }
    output.flush()
}

pub(crate) fn execute(steps: Vec<Step>) -> io::Result<()> {
    let mut memory = Vec::new();
    for step in steps {
        match step {
            Step::Ready => platform::ready()?,
            Step::Exit(code) => platform::exit(code),
            Step::Signal(name) => {
                platform::signal(&name)?;
                // Rust's stack-overflow handler may restore SIG_DFL and return
                // from a software SIGSEGV. Redeliver if the first raise returns
                // so this fixture step reliably terminates by SIGSEGV.
                #[cfg(unix)]
                if name == "SEGV" {
                    platform::signal(&name)?;
                }
            }
            Step::Sleep(ms) => std::thread::sleep(Duration::from_millis(ms)),
            Step::Write(Stream::Stdout, count, byte) => {
                write_bytes(io::stdout().lock(), count, byte)?;
            }
            Step::Write(Stream::Stderr, count, byte) => {
                write_bytes(io::stderr().lock(), count, byte)?;
            }
            Step::Write(Stream::Stdin, _, _) => unreachable!("validated output stream"),
            Step::Flood(count) => {
                std::thread::scope(|scope| {
                    let out = scope.spawn(|| write_bytes(io::stdout().lock(), count, b'x'));
                    let err = scope.spawn(|| write_bytes(io::stderr().lock(), count, b'x'));
                    let out = out.join().expect("stdout writer panicked");
                    let err = err.join().expect("stderr writer panicked");
                    out.and(err)
                })?;
            }
            Step::PingPong(count) | Step::Pong(count) => {
                let ping = matches!(step, Step::PingPong(_));
                let mut input = io::stdin().lock();
                let mut output = io::stdout().lock();
                for _ in 0..count {
                    let mut byte = *b"x";
                    if !ping {
                        input.read_exact(&mut byte)?;
                    }
                    output.write_all(&byte)?;
                    output.flush()?;
                    if ping {
                        input.read_exact(&mut byte)?;
                    }
                    if byte != *b"x" {
                        return Err(io::Error::other("bad pingpong byte"));
                    }
                }
            }
            Step::Echo => {
                let mut input = io::stdin().lock();
                let mut output = io::stdout().lock();
                let mut line = Vec::new();
                loop {
                    line.clear();
                    if input.read_until(b'\n', &mut line)? == 0 {
                        break;
                    }
                    output.write_all(&line)?;
                    output.flush()?;
                }
            }
            Step::EchoArgv(args) => {
                let mut output = io::stdout().lock();
                for arg in args {
                    output.write_all(arg.as_bytes())?;
                    output.write_all(&[0])?;
                }
                output.flush()?;
            }
            Step::DefaultSigpipe => {
                #[cfg(unix)]
                platform::default_sigpipe()?;
            }
            Step::ReadExactly(mut remaining) => {
                let mut buffer = [0; 64 * 1024];
                let mut input = io::stdin().lock();
                while remaining != 0 {
                    let count = remaining.min(buffer.len());
                    input.read_exact(&mut buffer[..count])?;
                    remaining -= count;
                }
            }
            Step::Drain => {
                let mut input = io::stdin().lock();
                let mut buffer = [0; 64 * 1024];
                let mut total = 0_u64;
                loop {
                    match input.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => total += n as u64,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    }
                }
                writeln!(io::stdout(), "{total}")?;
                io::stdout().flush()?;
            }
            Step::Close(target) => platform::close(target)?,
            Step::Spin(ms) => {
                let start = std::time::Instant::now();
                while start.elapsed() < Duration::from_millis(ms) {
                    std::hint::spin_loop();
                }
            }
            Step::Memory(bytes) => {
                let mut allocation = Vec::new();
                allocation
                    .try_reserve_exact(bytes)
                    .map_err(io::Error::other)?;
                allocation.resize(bytes, 0_u8);
                for offset in (0..bytes).step_by(4096) {
                    allocation[offset] = 1;
                }
                std::hint::black_box(&allocation);
                memory.push(allocation);
            }
            Step::Spawn(count, args) => {
                for _ in 0..count {
                    // Deliberately do not wait: tests exercise orphan cleanup.
                    #[allow(clippy::zombie_processes)]
                    let _child = Command::new(std::env::current_exe()?)
                        .args(&args)
                        .stdin(Stdio::inherit())
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .spawn()?;
                }
            }
            Step::Escape => platform::escape()?,
            Step::IgnoreTerm => platform::ignore_term()?,
            #[cfg(unix)]
            Step::IdentityFile(path) => {
                let pid = std::process::id();
                let (start, _) = platform::process_identity(pid)?;
                // Publish complete contents: existence is the readiness barrier.
                let temporary = path.with_extension(format!("{pid}.tmp"));
                std::fs::write(&temporary, format!("{pid} {start}\n"))?;
                std::fs::rename(temporary, path)?;
            }
            Step::WaitFile(path) => {
                while !path.try_exists()? {
                    std::thread::yield_now();
                }
            }
            Step::Hold => loop {
                std::thread::park();
            },
        }
    }
    // Prevent optimizing away page touches, including in release builds.
    std::hint::black_box(&memory);
    // The fixture retains allocations until process exit, even if its script
    // ends immediately after touching memory.
    std::mem::forget(memory);
    Ok(())
}
