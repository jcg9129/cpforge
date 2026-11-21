//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

// Flatten into each job command as it is implemented. Existing catalog and
// metadata commands have no schema-1 machine document.
pub use crate::transport::MachineArgs;

/// Command-line arguments for `cpf`.
#[derive(Debug, Parser)]
#[command(name = "cpf", version, about)]
pub struct Cli {
    /// Control colored output.
    #[arg(long, global = true, value_enum, default_value_t = Color::Auto)]
    pub color: Color,

    /// Select a problem for this invocation (<name> or local:<name>).
    #[arg(long, global = true)]
    pub problem: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Workspace commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Compile the selected problem in a fresh job directory.
    Build {
        /// Select a language profile for this build.
        #[arg(long)]
        profile: Option<String>,
        #[command(flatten)]
        machine: MachineArgs,
    },
    /// Run selected tests, or manage the problem's test catalog.
    Test {
        /// Run exactly these registered ids, including disabled tests, in order.
        #[arg(long = "case", action = clap::ArgAction::Append)]
        cases: Vec<String>,
        /// Select a language profile.
        #[arg(long)]
        profile: Option<String>,
        /// Override only the run deadline (e.g. 500ms, 2s, 1m).
        #[arg(long, allow_hyphen_values = true)]
        deadline: Option<String>,
        #[command(flatten)]
        machine: MachineArgs,
        #[command(subcommand)]
        command: Option<TestCommand>,
    },
    /// Print the problem selected for this invocation.
    Current {
        /// Print the absolute problem directory instead of its identity.
        #[arg(long)]
        path: bool,
    },
    /// Change the saved problem selection.
    Switch {
        /// Existing problem name or local:<name> identity.
        reference: String,
    },
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

#[derive(Debug, Subcommand)]
pub enum TestCommand {
    /// Copy a new custom test; paths (including FIFOs) are read as streams.
    Add {
        /// Input path, or - for stdin (captured before locking).
        #[arg(long)]
        input: PathBuf,
        /// Answer path, or - for stdin; omit for no reference answer.
        #[arg(long)]
        answer: Option<PathBuf>,
    },
    /// List registered tests, candidates and strays (human-readable, unstable format).
    Ls,
    /// Register existing files; paths are relative to the current directory.
    Register {
        input: PathBuf,
        /// Reference answer; otherwise use a sibling .ans if it exists.
        #[arg(long, conflicts_with = "no_answer")]
        answer: Option<PathBuf>,
        /// Deliberately register without a reference answer.
        #[arg(long)]
        no_answer: bool,
        /// Stable test id (defaults to a valid, available input stem).
        #[arg(long)]
        id: Option<String>,
    },
    /// Enable a registered test.
    Enable { id: String },
    /// Disable a registered test.
    Disable { id: String },
}

/// Requested output color policy (stored for future commands).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Color {
    Auto,
    Always,
    Never,
}
