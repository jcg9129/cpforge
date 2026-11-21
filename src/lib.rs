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
        Some(Command::Build { profile, machine }) => {
            return job_command::run(
                &job_command::BuildCommand,
                cli.problem.as_deref(),
                profile,
                machine,
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
            return job_command::run(&command, cli.problem.as_deref(), profile, machine);
        }
        Some(Command::Current { path }) => {
            return selection::current(cli.problem.as_deref(), path)
                .map(|()| exit::ExitCode::Success);
        }
        Some(Command::Switch { reference }) => {
            return selection::switch(&reference).map(|()| exit::ExitCode::Success);
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
