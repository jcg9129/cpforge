//! Active organisation listings: no lock, recovery, configuration or process.
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::Path;

use crate::cli::ListArgs;
use crate::error::Error;
use crate::exit::ExitCode;
use crate::organisation::{self, Finding};
use crate::organisation_read::{self as read, Source};
use crate::{report::wire, transport};

pub(crate) fn run(args: ListArgs) -> Result<ExitCode, Error> {
    if !args.json {
        return deliver(args);
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deliver(args))) {
        Ok(result) => result,
        Err(payload) => {
            let _ = transport::Delivery::stdout().escaped_panic("ls");
            std::panic::resume_unwind(payload)
        }
    }
}

fn deliver(args: ListArgs) -> Result<ExitCode, Error> {
    if args.archived {
        return Err(crate::unavailable_command("ls --archived"));
    }
    let mut delivery = transport::Delivery::stdout();
    if args.json {
        delivery.preflight()?;
    }
    let result = inspect(&args);
    match result {
        Ok((bytes, output)) => {
            if args.json {
                delivery.document(&bytes)?;
            } else {
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(output.as_bytes())
                    .and_then(|()| stdout.flush())
                    .map_err(|error| Error::Workspace(format!("cannot write listing: {error}")))?;
            }
            Ok(ExitCode::Success)
        }
        Err(error) if args.json => {
            let exit = error.exit_code();
            let class = error.class();
            let bytes = wire::encode_error(&wire::ErrorDocument {
                platform: transport::platform(),
                command: "ls".into(),
                class,
                exit: i64::from(exit as u8),
                error: wire::DocumentError {
                    kind: class.token(),
                    message: error.to_string(),
                    path: None,
                },
                diagnostics: Vec::new(),
                diagnostics_dropped: 0,
            })
            .map_err(|error| Error::Internal(error.to_string()))?;
            eprintln!("cpf: error: {error}");
            delivery.document(&bytes)?;
            Ok(exit)
        }
        Err(error) => Err(error),
    }
}

fn inspect(args: &ListArgs) -> Result<(Vec<u8>, String), Error> {
    let zone = crate::organisation_dates::zone(&args.filters);
    let root = crate::workspace_commands::root()?;
    let (problems, mut problem_invalid) = read::problems(&root)?;
    let (mut contests, mut contest_invalid) = read::contests(&root)?;
    read::validate_contests(&args.filters, &contests, &contest_invalid)?;
    let mut findings = Vec::new();
    for problem in &problems {
        if !args.contests {
            findings.extend(read::field_sources(
                Path::new("problems")
                    .join(problem.selected.identity.name().as_str())
                    .join("problem.toml"),
                organisation::findings(problem.fields()),
            ));
        }
    }
    for contest in &contests {
        if let Err(reason) = organisation::tags(contest.document.get("tags")) {
            findings.extend(read::field_sources(
                Path::new("contests")
                    .join(&contest.name)
                    .join("contest.toml"),
                vec![Finding {
                    field: "tags",
                    reason,
                }],
            ));
        }
    }
    let selected = if args.contests {
        contests.retain(|contest| read::matches_contest(&args.filters, contest));
        Vec::new()
    } else {
        problems
            .iter()
            .filter(|problem| read::matches(&args.filters, problem, &contests, &zone))
            .collect::<Vec<_>>()
    };
    // Invalid source entries are never filtered out. Problem failures only
    // affect derived counts in contest mode, where they are unresolved.
    let mut invalid = Vec::<Source>::new();
    if !args.contests {
        invalid.append(&mut problem_invalid);
    }
    invalid.append(&mut contest_invalid);
    invalid.extend(findings);
    let bytes = wire::encode_ls(
        transport::platform(),
        args.contests,
        &selected,
        &contests,
        &problems,
        &invalid,
    )
    .map_err(|error| Error::Internal(error.to_string()))?;
    let mut output = String::new();
    for problem in &selected {
        let fields = problem.fields();
        let tags =
            organisation::tags(fields.and_then(|fields| fields.get("tags"))).unwrap_or_default();
        let identity = problem.selected.identity.to_string();
        let memberships = contests
            .iter()
            .filter(|contest| contest.members().contains(&identity.as_str()))
            .map(|contest| contest.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            output,
            "{identity}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{memberships}",
            problem.status().unwrap_or("invalid"),
            tags.join(", "),
            fields
                .and_then(|fields| fields.get("title"))
                .and_then(toml_edit::Item::as_str)
                .unwrap_or(""),
            read::timestamp(fields, "created_at").unwrap_or_default(),
            read::timestamp(fields, "updated_at").unwrap_or_default(),
            read::timestamp(fields, "solved_at").unwrap_or_default(),
            read::timestamp(fields, "archived_at").unwrap_or_default()
        );
    }
    if args.contests {
        for contest in &contests {
            let (counts, unresolved, invalid) = read::counts(contest, &problems);
            let mut summary = counts
                .into_iter()
                .map(|(status, count)| format!("{status} {count}"))
                .collect::<Vec<_>>();
            if invalid != 0 {
                summary.push(format!("invalid {invalid}"));
            }
            if unresolved != 0 {
                summary.push(format!("unresolved {unresolved}"));
            }
            let tags = organisation::tags(contest.document.get("tags")).unwrap_or_default();
            let _ = writeln!(
                output,
                "{}\t{}\t{}\t{}\t{}",
                contest.name,
                contest.identity().unwrap_or("local"),
                tags.join(", "),
                contest.members().len(),
                summary.join(", ")
            );
        }
    }
    if selected.is_empty() && (!args.contests || contests.is_empty()) {
        output.push_str("no matches\n");
    }
    for source in &invalid {
        let _ = writeln!(
            output,
            "invalid {}{}: {}: {}",
            source.path.display(),
            source
                .field
                .map_or(String::new(), |field| format!(" {field}")),
            source.kind,
            source.message
        );
    }
    Ok((bytes, output))
}
