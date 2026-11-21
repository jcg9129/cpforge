//! CLI foundation for cpforge.

mod add;
pub mod build;
pub mod cli;
pub mod compare;
pub mod config;
pub mod error;
pub mod exec;
pub mod exit;
pub mod jobs;
pub mod names;
pub mod report;
pub mod selection;
pub mod storage;
pub mod supervisor;
mod test_commands;
pub mod tests_store;
mod workspace;

use clap::{CommandFactory, error::ErrorKind};

use crate::{
    cli::{Cli, Command},
    error::Error,
};

/// Run the parsed CLI.
pub fn run(cli: Cli) -> Result<(), Error> {
    if cli.problem.is_some() && matches!(&cli.command, Some(Command::Switch { .. })) {
        return Err(clap::Error::raw(
            ErrorKind::ArgumentConflict,
            "--problem cannot be used with switch",
        )
        .into());
    }
    match cli.command {
        Some(Command::Test { command }) => {
            return test_commands::run(cli.problem.as_deref(), command);
        }
        Some(Command::Current { path }) => return selection::current(cli.problem.as_deref(), path),
        Some(Command::Switch { reference }) => return selection::switch(&reference),
        Some(Command::Init { path }) => return workspace::init(&path),
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
            );
        }
        None => {}
    }
    let help = Cli::command().render_help().to_string();
    Err(clap::Error::raw(ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, help).into())
}
