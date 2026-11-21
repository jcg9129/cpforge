//! Machine output for `--json` and `--events` (report contract §§6–7).
//!
//! stdout carries only machine output in these modes: one document and `\n`
//! for `--json`; for `--events`, the stream header, event envelopes,
//! `job_finished` and the final `report` envelope, one compact line each.
//! Any write or flush error is a delivery failure (class `delivery`, exit 11)
//! and nothing more is written. Job sinks never write to stdout.
//!
//! Job command integration order:
//!
//! 1. `MachineArgs::prepare(kind)` before registration, freezing or capture.
//!    It arms the process panic boundary and refuses a stdout closed at
//!    startup with class `delivery` before anything is admitted. Tests inject
//!    a writer with `Delivery::new(writer, closed_at_start)` and `preflight`.
//! 2. One `progress::ProgressWriter` for stderr; attach its `queue()` with
//!    `Engine::progress` and set `Engine::output_events(mode.output_chunks())`.
//! 3. `cancel_signals::CancelSignals::install(engine.cancellation())`, kept
//!    alive through delivery and the final stderr summary.
//! 4. Events mode: subscribe before running the engine and call
//!    `Delivery::start_events`; its blocking writer thread never delays P–S.
//! 5. Phase D: `EventsDelivery::finish`, or `Delivery::report` for `--json`.
//!    Refusals use `Delivery::refusal`.
//! 6. `delivered_status(report, mode, &delivery)`, then
//!    `ProgressWriter::finish(summary)` (a worker panic there is class
//!    `internal`), then `CancelSignals::finish(status)`, which on Unix may end
//!    the process by re-raising the cancelling signal.
//!
//! Raw `run` and commands without a schema-1 document do not flatten
//! `MachineArgs`, so clap rejects the flags there (exit 2), as it does for
//! `--json` together with `--events`.
use crate::error::{Error, FailureClass};
use crate::exit::{ExitCode, ExitInputs, exit_status};
use crate::jobs::engine::{EventReceiver, FinalizedReport};
use crate::report::{JobKind, wire};
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MachineMode {
    #[default]
    Human,
    Json,
    Events {
        output: bool,
    },
}
impl MachineMode {
    pub fn requested(self) -> bool {
        self != Self::Human
    }
    pub fn output_chunks(self) -> bool {
        self == Self::Events { output: true }
    }
}

/// Reusable per-job-command clap arguments. Flatten into build/test/cmd only;
/// raw run and metadata commands without a document must not flatten these.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum EventMode {
    Output,
}

#[derive(Clone, Debug, Default, clap::Args)]
pub struct MachineArgs {
    #[arg(long, conflicts_with = "events")]
    pub json: bool,
    #[arg(long, num_args = 0..=1, require_equals = true, value_enum, conflicts_with = "json")]
    pub events: Option<Option<EventMode>>,
}
impl MachineArgs {
    /// Command entry point: call before registering or preparing a job. No
    /// existing metadata command defines a schema-1 document; those commands
    /// intentionally do not accept these flags.
    pub fn prepare(&self, command: JobKind) -> Result<Delivery<io::Stdout>, Error> {
        crate::process::arm_machine(self.mode(), command);
        let mut delivery = Delivery::stdout();
        if self.mode().requested() {
            delivery.preflight()?;
        }
        Ok(delivery)
    }

    pub fn mode(&self) -> MachineMode {
        if self.json {
            MachineMode::Json
        } else if let Some(mode) = &self.events {
            MachineMode::Events {
                output: mode.is_some(),
            }
        } else {
            MachineMode::Human
        }
    }
}

#[derive(Debug)]
pub struct Delivery<W> {
    writer: W,
    closed_at_start: bool,
    failed: bool,
    begun: bool,
    finished: bool,
}
impl<W: Write> Delivery<W> {
    /// `closed_at_start` is injectable; production callers use `stdout`.
    pub fn new(writer: W, closed_at_start: bool) -> Self {
        Self {
            writer,
            closed_at_start,
            failed: false,
            begun: false,
            finished: false,
        }
    }
    /// Must precede registration, capture, and execution. A zero-length write
    /// cannot detect a broken pipe; actual write failures are handled in D.
    pub fn preflight(&mut self) -> Result<(), Error> {
        if self.closed_at_start {
            self.failed = true;
            crate::process::fail_delivery();
            Err(Error::Delivery(
                "stdout was closed at process startup; job not admitted".into(),
            ))
        } else if self.failed || self.finished {
            Err(Error::Delivery(
                "machine output is no longer writable".into(),
            ))
        } else {
            Ok(())
        }
    }
    pub fn begun(&self) -> bool {
        self.begun
    }
    pub fn failed(&self) -> bool {
        self.failed
    }
    pub fn into_inner(self) -> W {
        self.writer
    }
    fn line(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.preflight()?;
        for part in [bytes, b"\n".as_slice()] {
            let mut remaining = part;
            while !remaining.is_empty() {
                if crate::cancel_signals::force_exit_started() {
                    return Err(Error::Cancelled("forced exit".into()));
                }
                match self.writer.write(remaining) {
                    Ok(0) => {
                        return self.fail(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "stdout write returned zero",
                        ));
                    }
                    Ok(written) => {
                        self.begun = true;
                        crate::process::begin_output();
                        remaining = &remaining[written..];
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return self.fail(error),
                }
            }
        }
        Ok(())
    }
    fn fail<T>(&mut self, error: io::Error) -> Result<T, Error> {
        self.failed = true;
        crate::process::fail_delivery();
        Err(Error::Delivery(format!(
            "cannot deliver machine output: {error}"
        )))
    }
    fn flush(&mut self) -> Result<(), Error> {
        if crate::cancel_signals::force_exit_started() {
            return Err(Error::Cancelled("forced exit".into()));
        }
        if let Err(error) = self.writer.flush() {
            return self.fail(error);
        }
        self.finished = true;
        Ok(())
    }
    /// Exactly one encoded document followed by a newline and a successful flush.
    pub fn document(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.line(bytes)?;
        self.flush()
    }
    pub fn report(&mut self, report: &FinalizedReport) -> Result<(), Error> {
        crate::process::freeze_report();
        if report.post_freeze_internal() {
            return Err(Error::Internal("job panicked after report freeze".into()));
        }
        self.document(&encoded(wire::encode_stdout_report(report))?)
    }
    /// Header, all queued events, the reserved post-S terminal, final report.
    /// Takes ownership so any failure immediately disconnects the receiver.
    pub async fn events(
        &mut self,
        mut receiver: EventReceiver,
        command: JobKind,
        output: bool,
    ) -> Result<(), Error> {
        self.event_header(command, output)?;
        while let Some(event) = receiver.recv().await {
            self.line(&encoded(wire::encode_event(&event))?)?;
        }
        let terminal = receiver.terminal().await;
        crate::process::freeze_report();
        if terminal.report.post_freeze_internal() {
            return Err(Error::Internal("job panicked after report freeze".into()));
        }
        self.line(&encoded(wire::encode_job_finished(&terminal))?)?;
        self.line(&encoded(wire::encode_final_report(&terminal.report))?)?;
        self.flush()
    }
    pub fn refusal(&mut self, refusal: &wire::Refusal, mode: MachineMode) -> Result<(), Error> {
        match mode {
            MachineMode::Human => Ok(()),
            MachineMode::Json => self.document(&encoded(wire::encode_refusal(refusal))?),
            MachineMode::Events { output } => {
                self.event_header(refusal.command, output)?;
                self.line(&encoded(wire::encode_final_refusal(refusal))?)?;
                self.flush()
            }
        }
    }
    /// Before-ready job panic: header + final refusal in events mode, one
    /// refusal in JSON mode. A report frozen at F forbids any replacement.
    pub fn escaped_job_panic(&mut self, command: JobKind, mode: MachineMode) -> Result<(), Error> {
        if self.begun
            || crate::process::output_begun()
            || crate::process::report_frozen()
            || crate::process::delivery_failed()
            || self.failed
        {
            return Ok(());
        }
        self.refusal(
            &early_refusal(command, &Error::Internal("command panicked".into())),
            mode,
        )
    }

    pub fn event_header(&mut self, command: JobKind, output: bool) -> Result<(), Error> {
        self.line(&encoded(wire::encode_event_header(
            platform(),
            command,
            env!("CARGO_PKG_VERSION"),
            output,
        ))?)
    }
    /// Best-effort escaped panic document before any output and before F. Once
    /// F has happened the frozen report is authoritative, even with empty stdout.
    pub fn escaped_panic(&mut self, command: &str) -> Result<(), Error> {
        if self.begun
            || crate::process::output_begun()
            || crate::process::report_frozen()
            || crate::process::delivery_failed()
            || self.failed
        {
            return Ok(());
        }
        self.document(&encoded(wire::encode_error(&wire::ErrorDocument {
            platform: platform(),
            command: command.into(),
            class: FailureClass::Internal,
            exit: 70,
            error: wire::DocumentError {
                kind: "panic",
                message: "command panicked".into(),
                path: None,
            },
            diagnostics: Vec::new(),
            diagnostics_dropped: 0,
        }))?)
    }
}
impl Delivery<io::Stdout> {
    pub fn stdout() -> Self {
        Self::new(io::stdout(), crate::process::stdout_closed())
    }
}
fn encoded(result: wire::Result<Vec<u8>>) -> Result<Vec<u8>, Error> {
    result.map_err(|error| Error::Internal(format!("cannot encode machine output: {error}")))
}
pub const fn platform() -> &'static str {
    if cfg!(windows) { "windows" } else { "unix" }
}

/// Refusal before registration. Commands that have registered a job must fill
/// its identity and admission observations in the returned value before delivery.
pub fn early_refusal(command: JobKind, error: &Error) -> wire::Refusal {
    let now = std::time::SystemTime::now();
    wire::Refusal {
        platform: platform(),
        cpf_version: env!("CARGO_PKG_VERSION").into(),
        command,
        job: None,
        workspace: None,
        problem: None,
        class: error.class(),
        error: wire::DocumentError {
            kind: error.class().token(),
            message: error.to_string(),
            path: None,
        },
        diagnostics: Vec::new(),
        diagnostics_dropped: 0,
        warnings: Vec::new(),
        warnings_dropped: 0,
        events_dropped: 0,
        timing: crate::jobs::engine::Timing {
            started_at: now,
            finished_at: now,
            duration: std::time::Duration::ZERO,
        },
        cancellation: None,
        pre_delivery_exit: crate::jobs::engine::PreDeliveryExit {
            code: error.exit_code() as i64,
            reraise: None,
        },
    }
}

/// Apply late delivery/internal errors without mutating the finalized report.
/// Call `CancelSignals::finish` with the result after the stderr summary.
pub fn delivered_status(
    report: &FinalizedReport,
    mode: MachineMode,
    delivery: &Result<(), Error>,
) -> ExitCode {
    exit_status(&ExitInputs {
        failure: if report.post_freeze_internal() {
            Some(FailureClass::Internal)
        } else {
            delivery.as_ref().err().map(Error::class)
        },
        verdict: Some(report.data().result.verdict),
        cancellation: report.data().result.cancellation.is_some(),
        machine_output: mode.requested(),
        delivery_failed: delivery
            .as_ref()
            .err()
            .is_some_and(|error| error.class() == FailureClass::Delivery),
        switch_hook: report.data().command == JobKind::Hook,
        ..ExitInputs::default()
    })
}

/// Dedicated blocking stdout worker. Join only in D, after the engine has
/// returned its finalized report; a blocked stdout then cannot slow phases P–S.
#[derive(Debug)]
pub struct EventsDelivery(std::thread::JoinHandle<Result<(), Error>>);
impl EventsDelivery {
    pub fn finish(self) -> Result<(), Error> {
        match self.0.join() {
            Ok(result) => result,
            Err(_) => Err(Error::Internal("machine-output worker panicked".into())),
        }
    }
}
impl<W: Write + Send + 'static> Delivery<W> {
    pub fn start_events(
        mut self,
        receiver: EventReceiver,
        command: JobKind,
        output: bool,
    ) -> Result<EventsDelivery, Error> {
        self.preflight()?;
        let worker = std::thread::Builder::new()
            .name("cpf-stdout".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .map_err(|error| Error::Internal(format!("machine-output runtime: {error}")))?;
                runtime.block_on(self.events(receiver, command, output))
            })
            .map_err(|error| Error::Infrastructure(format!("machine-output worker: {error}")))?;
        Ok(EventsDelivery(worker))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Failing {
        bytes: Vec<u8>,
        limit: usize,
        flush_fails: bool,
    }
    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let size = bytes.len().min(self.limit.saturating_sub(self.bytes.len()));
            if size == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "injected EPIPE"));
            }
            self.bytes.extend_from_slice(&bytes[..size]);
            Ok(size)
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.flush_fails {
                Err(io::Error::other("injected flush"))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn failures_are_sticky_and_never_admit_closed_stdout() {
        for (closed, limit, flush_fails) in [
            (true, 100, false),
            (false, 0, false),
            (false, 4, false),
            (false, 100, true),
        ] {
            let mut delivery = Delivery::new(
                Failing {
                    bytes: Vec::new(),
                    limit,
                    flush_fails,
                },
                closed,
            );
            let mut admitted = false;
            let result = delivery.preflight().and_then(|()| {
                admitted = true;
                delivery.document(b"{\"ok\":true}")
            });
            assert_eq!(result.unwrap_err().exit_code(), ExitCode::Delivery);
            assert_eq!(admitted, !closed);
            let length = delivery.writer.bytes.len();
            assert_eq!(
                delivery.document(b"second").unwrap_err().exit_code(),
                ExitCode::Delivery
            );
            assert_eq!(delivery.writer.bytes.len(), length);
        }
    }
    #[test]
    fn one_document_and_newline_only() {
        let mut delivery = Delivery::new(Vec::new(), false);
        delivery.document(b"{}").unwrap();
        assert_eq!(delivery.writer, b"{}\n");
        assert!(delivery.document(b"{}").is_err());
    }
    #[test]
    fn flags_are_per_command_and_exclusive() {
        use clap::Parser;
        #[derive(Debug, Parser)]
        struct Job {
            #[command(flatten)]
            machine: MachineArgs,
        }
        assert_eq!(
            Job::try_parse_from(["job", "--json", "--events"])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            Job::try_parse_from(["job", "--events"])
                .unwrap()
                .machine
                .mode(),
            MachineMode::Events { output: false }
        );
        assert_eq!(
            Job::try_parse_from(["job", "--events=status"])
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(
            Job::try_parse_from(["job", "--events=output"])
                .unwrap()
                .machine
                .mode(),
            MachineMode::Events { output: true }
        );
        for command in ["current", "init", "add", "switch"] {
            assert!(crate::cli::Cli::try_parse_from(["cpf", command, "--json"]).is_err());
        }
    }
}
