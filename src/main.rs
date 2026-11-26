use clap::{Parser, error::ErrorKind};
use cpforge::{
    cli::{Cli, Command},
    error::Error,
    exit::ExitCode,
};

// The Rust runtime reopens closed standard descriptors before `main`; a
// constructor therefore records a closed stdout first.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
#[used]
#[unsafe(link_section = ".init_array")]
static PROBE_STANDARD_STREAMS: extern "C" fn() = cpforge::process::probe_standard_streams;
#[cfg(target_vendor = "apple")]
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static PROBE_STANDARD_STREAMS: extern "C" fn() = cpforge::process::probe_standard_streams;

fn main() -> std::process::ExitCode {
    // This must precede parsing, opening workspace files, or starting workers.
    if let Err(error) = cpforge::process::initialize() {
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("cpf: error: standard stream setup: {error}\n"),
        );
        return ExitCode::Internal.into();
    }
    // Caught job panics become report evidence; the catch boundary below emits
    // escaped-panic diagnostics. Avoid an independent, interleaved panic writer.
    std::panic::set_hook(Box::new(|_| {}));
    match std::panic::catch_unwind(command) {
        Ok(code) => code.into(),
        Err(payload) => {
            cpforge::process::escaped_panic();
            let reason = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown cause");
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("cpf: error: internal: command panicked: {reason}\n"),
            );
            ExitCode::Internal.into()
        }
    }
}
fn command() -> ExitCode {
    let parsed = Cli::try_parse().map_err(Error::from);
    if let Ok(cli) = &parsed {
        let filters = match &cli.command {
            Some(Command::Ls(args)) => Some(&args.filters),
            Some(Command::Search(args)) => Some(&args.filters),
            Some(Command::Status { filters, .. }) => Some(filters),
            _ => None,
        };
        // The contract treats empty TZ as unset; jiff instead treats it as UTC.
        // Normalize only date-filter invocations, before supervisor workers start.
        if filters.is_some_and(|filters| filters.has_dates())
            && std::env::var_os("TZ").is_some_and(|value| value.is_empty())
        {
            // SAFETY: startup and argument parsing have not spawned any threads.
            unsafe { std::env::remove_var("TZ") };
        }
    }
    if let Err(error) = cpforge::supervisor::init_process() {
        eprintln!("cpf: error: cannot initialize process supervisor: {error}");
        return ExitCode::Internal;
    }
    let result = parsed.and_then(cpforge::run);
    match result {
        Ok(code) => code,
        Err(error) => {
            let code = error.exit_code();
            match error {
                Error::Cli(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::DisplayHelp
                            | ErrorKind::DisplayVersion
                            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    ) =>
                {
                    if let Err(error) = error.print() {
                        eprintln!("cpf: error: {error}");
                        return ExitCode::Internal;
                    }
                }
                error => eprintln!("cpf: error: {error}"),
            }
            code
        }
    }
}
