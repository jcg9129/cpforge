//! CLI foundation for cpforge.

mod add;
pub mod cli;
pub mod config;
pub mod error;
pub mod exec;
pub mod exit;
pub mod names;
pub mod storage;
pub mod supervisor;
mod workspace;

use clap::{CommandFactory, error::ErrorKind};

use crate::{
    cli::{Cli, Command},
    error::Error,
};

/// Run the parsed CLI.
pub fn run(cli: Cli) -> Result<(), Error> {
    match cli.command {
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
