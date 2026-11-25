//! Human-readable catalog commands.

use crate::cli::TestCommand;
use crate::error::Error;
use crate::selection;
use crate::storage::WorkspaceLock;
use crate::tests_store::{
    Answer, CapturedFile, Health, Mutation, Origin, RegisterRequest, StoreError, TestStore,
};
use crate::workspace::find_root;

fn finish<T>(result: Result<Mutation<T>, StoreError>) -> Result<T, Error> {
    match result {
        Ok(outcome) => {
            for warning in outcome.warnings {
                eprintln!("cpf: warning: {warning}");
            }
            Ok(outcome.value)
        }
        Err(error) => Err(store_error(error)),
    }
}

fn store_error(error: StoreError) -> Error {
    for warning in error.warnings {
        eprintln!("cpf: warning: {warning}");
    }
    Error::Workspace(error.message)
}

pub(crate) fn run(
    problem: Option<&str>,
    command: TestCommand,
    machine: crate::cli::MachineArgs,
) -> Result<(), Error> {
    if matches!(
        &command,
        TestCommand::Add {
            from_stress: Some(_),
            ..
        }
    ) {
        return Err(crate::unavailable_command("test add --from-stress"));
    }
    if machine.mode().requested() {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "machine output is available for test jobs, not catalog subcommands",
        )
        .into());
    }
    if let TestCommand::Add {
        input: Some(input),
        answer: Some(answer),
        ..
    } = &command
    {
        if input == std::path::Path::new("-") && answer == std::path::Path::new("-") {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--input and --answer cannot both read stdin (-)",
            )
            .into());
        }
    }
    let cwd = std::env::current_dir()
        .map_err(|error| Error::Workspace(format!("cannot read current directory: {error}")))?;
    let selected = selection::resolve(&find_root(&cwd)?, &cwd, problem)?;
    let store = TestStore::new(selected.dir);
    if matches!(command, TestCommand::Ls) {
        let listing = store.list()?;
        let healthy = matches!(
            &listing.catalog,
            crate::storage::Record::Missing | crate::storage::Record::Valid(_)
        );
        for entry in &listing.registered {
            print!(
                "{} {} answer {}",
                entry.entry.id,
                if entry.entry.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                entry.answer_state()
            );
            if !matches!(entry.input, Health::Ok { .. }) {
                print!("; {}: {}", entry.entry.input, entry.input);
            }
            if let (Some(health), Answer::Named(path)) = (&entry.answer, &entry.entry.answer) {
                if !matches!(health, Health::Ok { .. }) {
                    print!("; {path}: {health}");
                }
            }
            println!();
        }
        for candidate in listing.candidates {
            print!("candidate {}", candidate.input);
            if let Answer::Named(answer) = candidate.answer {
                print!(" answer {answer}");
            }
            println!();
        }
        for stray in listing.strays {
            println!("stray {stray}");
        }
        for diagnostic in listing.diagnostics {
            eprintln!("cpf: {diagnostic}");
        }
        return if healthy {
            Ok(())
        } else {
            Err(Error::Workspace("test catalog is unhealthy".into()))
        };
    }
    // Capture both streams before creating or acquiring a workspace lock.
    // A failure drops any earlier capture and has published nothing.
    let captures = if let TestCommand::Add {
        input: Some(input),
        answer,
        ..
    } = &command
    {
        let capture = |path: &std::path::Path| {
            CapturedFile::from_source(path).map_err(|error| {
                Error::Workspace(format!("cannot capture {}: {error}", path.display()))
            })
        };
        Some((capture(input)?, answer.as_deref().map(capture).transpose()?))
    } else {
        None
    };
    // Register paths are lexical from the physical current directory (§5.7).
    let cwd = std::fs::canonicalize(&cwd)
        .map(|path| crate::storage::paths::plain(&path))
        .map_err(|error| Error::Workspace(format!("cannot resolve current directory: {error}")))?;
    let lock = WorkspaceLock::new(&selected.root)?;
    let guard = lock.acquire("test catalog")?;
    if let Some(warning) = guard.recovery_warning() {
        for (path, error) in warning.paths.iter().zip(&warning.errors) {
            eprintln!(
                "cpf: warning: leftover recovery incomplete at {}: {error}",
                path.display()
            );
        }
    }
    match command {
        TestCommand::Add { .. } => {
            let (input, answer) = captures.as_ref().expect("pre-lock captures");
            let outcome = store
                .add(&guard, input, answer.as_ref(), Origin::Custom)
                .map_err(store_error)?;
            for warning in outcome.warnings {
                eprintln!("cpf: warning: {warning}");
            }
            if let Some(failure) = outcome.failure {
                let paths = if outcome.published.is_empty() {
                    "none".to_owned()
                } else {
                    outcome
                        .published
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let message = format!(
                    "published: {paths}; not registered; {} failed: {}",
                    failure.step, failure.cause,
                );
                return Err(if outcome.published.is_empty() {
                    Error::Workspace(message)
                } else {
                    Error::Partial(message)
                });
            }
            println!("{}", outcome.id);
        }
        TestCommand::Register {
            input,
            answer,
            no_answer,
            id,
        } => {
            let registration = finish(store.register(
                &guard,
                &RegisterRequest {
                    current_dir: cwd,
                    input,
                    answer,
                    no_answer,
                    id,
                },
            ))?;
            if let Some(answer) = registration.inferred_answer {
                eprintln!("cpf: using sibling answer {answer}");
            }
            println!("{}", registration.id);
        }
        TestCommand::Enable { id } => {
            finish(store.set_enabled(&guard, &id, true))?;
        }
        TestCommand::Disable { id } => {
            finish(store.set_enabled(&guard, &id, false))?;
        }
        TestCommand::Ls => unreachable!("listing returned before lock acquisition"),
    }
    Ok(())
}
