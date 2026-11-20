//! CLI foundation for cpforge.

pub mod cli;
pub mod error;
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
    if let Some(Command::Init { path }) = cli.command {
        return workspace::init(&path);
    }
    let help = Cli::command().render_help().to_string();
    Err(clap::Error::raw(ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, help).into())
}
