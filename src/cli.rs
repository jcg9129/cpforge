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
    /// Add a local problem, optionally to a contest.
    Add {
        /// Problem name (the identity becomes `local:<name>`).
        name: String,
        /// Contest to add the problem to.
        #[arg(long)]
        contest: Option<String>,
        /// Problem label within the contest (requires --contest).
        #[arg(long, requires = "contest")]
        label: Option<String>,
        /// Language recorded for the new problem and used for its template.
        #[arg(long)]
        language: Option<String>,
    },
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
