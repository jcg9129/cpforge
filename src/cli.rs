//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// Command-line arguments for `cpf`.
#[derive(Debug, Parser)]
#[command(name = "cpf", version, about)]
pub struct Cli {
    /// Control colored output.
    #[arg(long, global = true, value_enum, default_value_t = Color::Auto)]
    pub color: Color,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Workspace commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialise an existing directory as a workspace.
    Init {
        /// Workspace directory (defaults to the current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

/// Requested output color policy (stored for future commands).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Color {
    Auto,
    Always,
    Never,
}
