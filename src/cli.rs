//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

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

    /// Select a problem (<name>, local:<name>, site:native or <contest>/<label>).
    #[arg(long, global = true)]
    pub problem: Option<String>,

    /// Replace the default user configuration for this invocation (disables hooks).
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Workspace commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Download or refetch problems and sample tests, or inspect fetch history.
    Fetch(FetchArgs),
    /// Add tags to a problem or contest.
    Tag(TagArgs),
    /// Remove tags from a problem or contest.
    Untag(TagArgs),
    /// Set a problem's workflow status.
    Mark {
        target: String,
        #[arg(value_parser = status_name)]
        status: String,
    },
    /// Locate or create a problem's notes.
    Notes {
        reference: Option<String>,
        #[arg(long)]
        path: bool,
        #[arg(long)]
        json: bool,
    },
    /// List active or archived problems and contests.
    Ls(ListArgs),
    /// Search problem metadata and notes.
    Search(SearchArgs),
    /// Search for failing inputs or replay a saved failure.
    Stress(StressArgs),
    /// Check an interactor against declared good and bad solutions.
    InteractorTest {
        #[arg(long = "case")]
        cases: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
        /// Override only the solution run deadline, validated as for `cpf test`.
        #[arg(long, allow_hyphen_values = true)]
        deadline: Option<String>,
    },
    /// Prepare a submission version.
    SubmitPrep(SubmitArgs),
    /// Plan or apply an archive, or finish an archive transaction.
    Archive(ArchiveArgs),
    /// Inspect or purge quarantined archive files.
    Quarantine {
        #[command(subcommand)]
        command: QuarantineCommand,
    },
    /// Restore a verified archive snapshot.
    Restore {
        snapshot: String,
        #[arg(long)]
        into: Option<String>,
        #[arg(long)]
        partial: bool,
        #[arg(long)]
        root: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Inspect configuration without creating jobs or changing the workspace.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect configuration, toolchains and machine capabilities.
    Doctor,

    /// Run a configured command in a managed job.
    Cmd {
        name: String,
        #[arg(long)]
        profile: Option<String>,
        #[command(flatten)]
        machine: MachineArgs,
        /// Arguments appended verbatim to the configured command.
        #[arg(last = true)]
        extra: Vec<std::ffi::OsString>,
    },
    /// Compile the selected problem in a fresh job directory.
    Build {
        /// Select a language profile for this build.
        #[arg(long)]
        profile: Option<String>,
        #[command(flatten)]
        machine: MachineArgs,
    },
    /// Build and run the selected problem once with console I/O.
    Run {
        /// Select a language profile.
        #[arg(long)]
        profile: Option<String>,
        /// Override only the run deadline (e.g. 500ms, 2s, 1m).
        #[arg(long, allow_hyphen_values = true)]
        deadline: Option<String>,
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
    /// Compare current files with the last completed run (read-only).
    Status {
        /// A problem (<name>, local:<name>, site:native or <contest>/<label>),
        /// or a contest (contest:<name>, or a bare name naming no problem).
        #[arg(conflicts_with = "all")]
        reference: Option<String>,
        /// Every problem of the workspace.
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        filters: Filters,
        /// Write the status document to stdout.
        #[arg(long)]
        json: bool,
    },
    /// Adopt an existing file, relative to the selected problem directory.
    Adopt { path: PathBuf },
    /// Validate and print a workspace ignore setting (does not edit configuration).
    Ignore { pattern: String },
    /// Inspect workspace records and optionally repair supported inconsistencies.
    Fsck {
        /// Repair dangling membership, orphan facts and an unavailable sequence.
        #[arg(long)]
        apply: bool,
    },
    /// Plan removal of inactive job directories and the cache.
    Clean {
        /// Remove them (active jobs are always kept).
        #[arg(long)]
        apply: bool,
    },
    /// Print ignore patterns for tool-owned content.
    Gitignore {
        /// Maintain the cpforge block in <root>/.gitignore.
        #[arg(long)]
        write: bool,
    },
    /// Print the problem selected for this invocation.
    Current {
        /// Print the absolute problem directory instead of its identity.
        #[arg(long)]
        path: bool,
    },
    /// Change the saved problem selection.
    Switch {
        /// Existing problem name, identity, or <contest>/<label> reference.
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
        #[arg(
            long,
            required_unless_present = "from_stress",
            conflicts_with = "from_stress"
        )]
        input: Option<PathBuf>,
        /// Register the input and validated answer of a saved stress failure.
        #[arg(long, value_parser = failure_id, conflicts_with = "answer")]
        from_stress: Option<String>,
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

/// Read-only configuration inspection.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Show effective declarative values and job-time requirements.
    Show {
        #[arg(long)]
        origin: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Validate all layers, languages, profiles and commands.
    Check {
        /// Also locate external programs; missing programs are informative.
        #[arg(long)]
        tools: bool,
    },
    /// List discovered configuration files and their existence.
    Paths,
}

#[derive(Debug, Args)]
pub struct FetchArgs {
    #[arg(required_unless_present = "history")]
    pub reference: Option<String>,
    #[arg(long, conflicts_with_all = ["only", "all_in_section", "disable_removed", "offline", "json"])]
    pub history: bool,
    #[arg(long)]
    pub only: Vec<String>,
    #[arg(long)]
    pub all_in_section: bool,
    #[arg(long)]
    pub disable_removed: bool,
    #[arg(long)]
    pub offline: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TagArgs {
    pub target: String,
    #[arg(required = true, num_args = 1.., value_parser = tag_name)]
    pub tags: Vec<String>,
}

/// Shared problem filters. Empty filters preserve the existing status path.
#[derive(Debug, Default, Args)]
pub struct Filters {
    #[arg(long, value_parser = status_name)]
    pub status: Vec<String>,
    #[arg(long, value_parser = tag_name)]
    pub tag: Vec<String>,
    #[arg(long, value_parser = site_name)]
    pub site: Vec<String>,
    #[arg(long, value_parser = identifier)]
    pub contest: Vec<String>,
    #[arg(long, value_parser = date_range)]
    pub created: Option<String>,
    #[arg(long, value_parser = date_range)]
    pub updated: Option<String>,
    #[arg(long, value_parser = date_range)]
    pub solved: Option<String>,
    #[arg(long, value_parser = date_range)]
    pub archived_in: Option<String>,
}
impl Filters {
    pub fn is_empty(&self) -> bool {
        self.status.is_empty()
            && self.tag.is_empty()
            && self.site.is_empty()
            && self.contest.is_empty()
            && self.created.is_none()
            && self.updated.is_none()
            && self.solved.is_none()
            && self.archived_in.is_none()
    }
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[command(flatten)]
    pub filters: Filters,
    #[arg(long, conflicts_with_all = ["status", "contest", "created", "updated", "solved", "archived_in"])]
    pub contests: bool,
    #[arg(long)]
    pub archived: bool,
    #[arg(long, requires = "archived")]
    pub root: Vec<PathBuf>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    #[arg(required = true, num_args = 1..=16, value_parser = nonempty_term)]
    pub terms: Vec<String>,
    #[command(flatten)]
    pub filters: Filters,
    #[arg(long)]
    pub no_notes: bool,
    #[arg(long)]
    pub no_archives: bool,
    #[arg(long)]
    pub root: Vec<PathBuf>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct StressArgs {
    #[arg(long, conflicts_with = "replay_binaries")]
    pub profile: Option<String>,
    /// Validated as for `cpf test` (a configuration error, not a usage error).
    #[arg(long, allow_hyphen_values = true, conflicts_with = "replay_binaries")]
    pub deadline: Option<String>,
    #[arg(long, value_parser = failure_id, conflicts_with = "replay_binaries")]
    pub replay: Option<String>,
    #[arg(long, value_parser = failure_id)]
    pub replay_binaries: Option<String>,
    #[arg(long, value_parser = seed, conflicts_with_all = ["replay", "replay_binaries"])]
    pub seed: Option<u64>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..), conflicts_with_all = ["replay", "replay_binaries"])]
    pub iterations: Option<u64>,
    #[arg(long, value_parser = duration, conflicts_with_all = ["replay", "replay_binaries"])]
    pub time: Option<String>,
    #[arg(long, conflicts_with_all = ["replay", "replay_binaries"])]
    pub keep_going: bool,
    #[command(flatten)]
    pub machine: MachineArgs,
    #[command(subcommand)]
    pub command: Option<StressCommand>,
}
#[derive(Debug, Subcommand)]
pub enum StressCommand {
    /// List saved stress failures without checking their hashes.
    Ls {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
pub struct SubmitArgs {
    #[arg(long, conflicts_with_all = ["profile", "judge", "events"])]
    pub source_copy: bool,
    #[arg(long)]
    pub no_test: bool,
    #[arg(long)]
    pub profile: Option<String>,
    #[arg(long)]
    pub judge: Option<String>,
    #[arg(long, conflicts_with = "no_copy")]
    pub copy: bool,
    #[arg(long)]
    pub no_copy: bool,
    #[arg(long, conflicts_with = "events")]
    pub json: bool,
    #[arg(long)]
    pub events: bool,
}

#[derive(Debug, Args)]
pub struct ArchiveArgs {
    #[arg(required_unless_present_any = ["resume", "close"], conflicts_with_all = ["resume", "close"])]
    pub target: Option<String>,
    #[arg(long, conflicts_with_all = ["resume", "close"])]
    pub to: Option<PathBuf>,
    #[arg(long, conflicts_with_all = ["resume", "close"])]
    pub keep_profile: Option<String>,
    #[arg(long, value_parser = ["trash", "quarantine", "delete", "keep"], conflicts_with_all = ["resume", "close"])]
    pub cleanup: Option<String>,
    #[arg(long, value_parser = ["error", "reuse-identical", "new"], conflicts_with_all = ["resume", "close"])]
    pub on_existing: Option<String>,
    #[arg(long)]
    pub apply: bool,
    #[arg(long, requires = "apply", conflicts_with_all = ["resume", "close"])]
    pub plan: Option<String>,
    #[arg(long, conflicts_with = "close")]
    pub resume: bool,
    #[arg(long, conflicts_with = "json")]
    pub close: Option<String>,
    #[arg(long)]
    pub json: bool,
}
#[derive(Debug, Subcommand)]
pub enum QuarantineCommand {
    /// List entries and their origin, age and size.
    Ls,
    /// Plan or apply removal of ready quarantine entries.
    Purge {
        ids: Vec<String>,
        #[arg(long)]
        apply: bool,
    },
}

fn duration(value: &str) -> Result<String, String> {
    crate::config::parse_deadline(value)
        .map(|_| value.to_owned())
        .map_err(str::to_owned)
}
fn nonempty_term(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        Err("search terms must be non-empty text of at most 256 bytes".into())
    } else {
        Ok(value.to_owned())
    }
}
pub(crate) fn tag_name(value: &str) -> Result<String, String> {
    if (1..=32).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        Ok(value.to_owned())
    } else {
        Err(
            "expected a tag of 1–32 ASCII letters, digits, - or _, starting with a letter or digit"
                .into(),
        )
    }
}
pub(crate) fn status_name(value: &str) -> Result<String, String> {
    if (1..=32).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Ok(value.to_owned())
    } else {
        Err("expected a status of 1–32 lowercase ASCII letters, digits or -, starting with a letter and not ending in -".into())
    }
}
/// `local` or the general site grammar (storage §2.1).
fn site_name(value: &str) -> Result<String, String> {
    // `local` satisfies the general grammar; one definition lives in `names`.
    crate::names::Site::new(value)
        .map(|_| value.to_owned())
        .map_err(|_| "expected local or a site of 1–32 lowercase ASCII letters, digits or -, starting with a letter".into())
}
fn identifier(value: &str) -> Result<String, String> {
    crate::names::Identifier::new(value)
        .map(|_| value.to_owned())
        .map_err(|error| error.to_string())
}
fn seed(value: &str) -> Result<u64, String> {
    if value.is_empty()
        || !value.bytes().all(|b| b.is_ascii_digit())
        || value.len() > 1 && value.starts_with('0')
    {
        return Err("expected a decimal u64 without a sign or leading zeroes".into());
    }
    value.parse().map_err(|_| "seed exceeds u64::MAX".into())
}
fn failure_id(value: &str) -> Result<String, String> {
    let parts: Vec<_> = value.split('-').collect();
    let decimal = |s: &str| {
        !s.is_empty()
            && (s == "0" || !s.starts_with('0'))
            && s.bytes().all(|b| b.is_ascii_digit())
            && s.parse::<u64>().is_ok()
    };
    if parts.len() == 4
        && parts[0] == "f"
        && parts[1].len() == 32
        && parts[1]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && decimal(parts[2])
        && decimal(parts[3])
    {
        Ok(value.to_owned())
    } else {
        Err("expected f-<32 lowercase hex epoch>-<job>-<iteration>".into())
    }
}
/// Classifies a fetch operand for the `--only` / `--all-in-section` usage
/// rules: (contest selection, CSES task URL). URLs are normalized as in sites
/// contract §8.4: `http` or `https`, trailing `/`, query and fragment dropped.
fn fetch_selection(reference: &str) -> (bool, bool) {
    if reference.starts_with("contest:") {
        return (true, false);
    }
    let Some(rest) = reference
        .strip_prefix("https://")
        .or_else(|| reference.strip_prefix("http://"))
    else {
        return (false, false);
    };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let segments: Vec<_> = rest.split('/').collect();
    let nonempty = |s: &&str| !s.is_empty();
    match segments.as_slice() {
        ["atcoder.jp", "contests", c] | ["atcoder.jp", "contests", c, "tasks"] => {
            (nonempty(c), false)
        }
        ["cses.fi", "problemset", "task" | "view", n] => (false, nonempty(n)),
        _ => (false, false),
    }
}
fn date_range(value: &str) -> Result<String, String> {
    fn day(s: &str) -> bool {
        let b = s.as_bytes();
        if b.len() != 10
            || b[4] != b'-'
            || b[7] != b'-'
            || !b
                .iter()
                .enumerate()
                .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        {
            return false;
        }
        let y = s[..4].parse::<u32>().unwrap_or(0);
        let m = s[5..7].parse::<u32>().unwrap_or(0);
        let d = s[8..].parse::<u32>().unwrap_or(0);
        let days = match m {
            4 | 6 | 9 | 11 => 30,
            2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
            2 => 28,
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            _ => 0,
        };
        y > 0 && d > 0 && d <= days
    }
    let valid = if let Some((a, b)) = value.split_once("..") {
        (!a.is_empty() || !b.is_empty())
            && (a.is_empty() || day(a))
            && (b.is_empty() || day(b))
            && (a.is_empty() || b.is_empty() || a <= b)
    } else {
        day(value)
    };
    if valid {
        Ok(value.to_owned())
    } else {
        Err("expected a day or inclusive date range (YYYY-MM-DD[..YYYY-MM-DD])".into())
    }
}

impl Cli {
    /// Cross-command combinations involving the global selector are usage
    /// checks, performed before the unavailable-command boundary or any I/O.
    pub(crate) fn validate_surfaces(&self) -> Result<(), clap::Error> {
        let reason = match self.command.as_ref() {
            Some(
                Command::Fetch(_)
                | Command::Tag(_)
                | Command::Untag(_)
                | Command::Mark { .. }
                | Command::Archive(_)
                | Command::Restore { .. }
                | Command::Quarantine { .. },
            ) if self.problem.is_some() => {
                Some("--problem cannot be used with this command; use its target operand")
            }
            Some(Command::Notes {
                reference: None, ..
            }) if self
                .problem
                .as_deref()
                .is_some_and(|reference| reference.starts_with("contest:")) =>
            {
                Some("this command requires a problem, not a contest")
            }
            Some(Command::Notes {
                reference: Some(_), ..
            }) if self.problem.is_some() => {
                Some("a notes operand cannot be combined with --problem")
            }
            Some(
                Command::Notes {
                    reference: Some(reference),
                    ..
                }
                | Command::Mark {
                    target: reference, ..
                },
            ) if reference.starts_with("contest:") => {
                Some("this command requires a problem, not a contest")
            }
            Some(Command::Fetch(args)) if !args.only.is_empty() || args.all_in_section => {
                let reference = args.reference.as_deref().unwrap_or("");
                let (contest, cses) = fetch_selection(reference);
                let only_allowed = contest || (cses && args.all_in_section);
                if (args.all_in_section && !cses) || (!args.only.is_empty() && !only_allowed) {
                    Some(
                        "--only requires a contest or section; --all-in-section requires a CSES task URL",
                    )
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(reason) = reason {
            use clap::CommandFactory;
            Err(Self::command().error(clap::error::ErrorKind::ArgumentConflict, reason))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn strict_seed_failure_ids_and_date_ranges() {
        for value in ["", "+1", "01", "-1", "18446744073709551616"] {
            assert!(seed(value).is_err(), "{value}");
        }
        assert_eq!(seed("0"), Ok(0));
        assert_eq!(seed("18446744073709551615"), Ok(u64::MAX));
        assert!(failure_id("f-0123456789abcdef0123456789abcdef-0-0").is_ok());
        for value in ["f-0123456789abcdef0123456789abcdef-01-0", "f-ABC-0-0"] {
            assert!(failure_id(value).is_err());
        }
        for (value, expected) in [
            ("https://atcoder.jp/contests/abc300", (true, false)),
            (
                "http://atcoder.jp/contests/abc300/tasks/?lang=en",
                (true, false),
            ),
            (
                "https://atcoder.jp/contests/abc300/tasks/abc300_a",
                (false, false),
            ),
            ("https://cses.fi/problemset/task/1068#x", (false, true)),
            ("https://cses.fi/problemset/view/1068/", (false, true)),
            ("https://cses.fi/problemset/list", (false, false)),
            ("contest:abc300", (true, false)),
            ("abc300", (false, false)),
        ] {
            assert_eq!(fetch_selection(value), expected, "{value}");
        }
        for value in ["local", "atcoder", "cses", "a-1"] {
            assert!(site_name(value).is_ok(), "{value}");
        }
        for value in ["", "AtCoder", "1a", "a_b"] {
            assert!(site_name(value).is_err(), "{value}");
        }
        assert!(identifier("abc300").is_ok());
        assert!(identifier("a..b").is_err());
        assert!(date_range("2024-02-29").is_ok());
        for value in ["..", "2026-02-29", "2026-12-01..2026-11-01"] {
            assert!(date_range(value).is_err());
        }
    }
}
