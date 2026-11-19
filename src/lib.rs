//! CLI foundation for cpforge.

pub mod cli;
pub mod error;
pub mod exit;
pub mod names;
pub mod storage;
pub mod supervisor;

use clap::{CommandFactory, error::ErrorKind};

use crate::{cli::Cli, error::Error};

/// Run the parsed CLI. Commands will be added in later tasks.
pub fn run(_cli: Cli) -> Result<(), Error> {
    let help = Cli::command().render_help().to_string();
    Err(clap::Error::raw(ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, help).into())
}
