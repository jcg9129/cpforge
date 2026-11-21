//! CLI foundation for cpforge.

// Share integration fixtures with portable library tests.
#[cfg(test)]
extern crate self as cpforge;

mod add;
pub mod build;
pub mod cancel_signals;
pub mod cli;
pub mod compare;
pub mod config;
pub mod error;
pub mod exec;
pub mod exit;
mod hooks;
pub mod job_command;
pub mod jobs;
pub mod names;
pub mod process;
pub mod progress;
pub mod report;
pub mod selection;
pub mod storage;
pub mod supervisor;
mod test_commands;
pub mod tests_store;
pub mod transport;
mod workspace;

use clap::{CommandFactory, error::ErrorKind};

use crate::{
    cli::{Cli, Command},
    error::Error,
};

/// Run the parsed CLI.
pub fn run(cli: Cli) -> Result<exit::ExitCode, Error> {
    if cli.problem.is_some() && matches!(&cli.command, Some(Command::Switch { .. })) {
        return Err(clap::Error::raw(
            ErrorKind::ArgumentConflict,
            "--problem cannot be used with switch",
        )
        .into());
    }
    match cli.command {
        Some(Command::Cmd {
            name,
            profile,
            machine,
            extra,
        }) => {
            return job_command::run_with_config(
                &job_command::NamedCommand { name, extra },
                cli.problem.as_deref(),
                profile,
                machine,
                cli.config,
            );
        }
        Some(Command::Build { profile, machine }) => {
            return job_command::run_with_config(
                &job_command::BuildCommand,
                cli.problem.as_deref(),
                profile,
                machine,
                cli.config,
            );
        }
        Some(Command::Run { profile, deadline }) => {
            // Raw mode does not flatten machine options: clap rejects them (2).
            return job_command::run_with_config(
                &job_command::RunCommand { deadline },
                cli.problem.as_deref(),
                profile,
                transport::MachineArgs::default(),
                cli.config,
            );
        }
        Some(Command::Test {
            command,
            cases,
            profile,
            deadline,
            machine,
        }) => {
            if let Some(command) = command {
                if !cases.is_empty() || profile.is_some() || deadline.is_some() {
                    return Err(clap::Error::raw(
                        ErrorKind::ArgumentConflict,
                        "--case, --profile and --deadline apply to test jobs, not catalog subcommands",
                    )
                    .into());
                }
                return test_commands::run(cli.problem.as_deref(), command, machine)
                    .map(|()| exit::ExitCode::Success);
            }
            let command = job_command::TestRunCommand::new(cases, deadline)?;
            return job_command::run_with_config(
                &command,
                cli.problem.as_deref(),
                profile,
                machine,
                cli.config,
            );
        }
        Some(Command::Current { path }) => {
            return selection::current(cli.problem.as_deref(), path)
                .map(|()| exit::ExitCode::Success);
        }
        Some(Command::Switch { reference }) => {
            return selection::switch(&reference, cli.config);
        }
        Some(Command::Init { path }) => {
            return workspace::init(&path).map(|()| exit::ExitCode::Success);
        }
        Some(Command::Add {
            name,
            contest,
            label,
            language,
        }) => {
            return add::add(
                &name,
                contest.as_deref(),
                label.as_deref(),
                language.as_deref(),
            )
            .map(|()| exit::ExitCode::Success);
        }
        None => {}
    }
    let help = Cli::command().render_help().to_string();
    Err(clap::Error::raw(ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, help).into())
}
