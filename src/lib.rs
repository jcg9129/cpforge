//! CLI foundation for cpforge.

// Share integration fixtures with portable library tests.
#[cfg(test)]
extern crate self as cpforge;

mod add;
pub mod build;
pub mod cancel_signals;
pub mod cli;
pub mod compare;
pub mod config;
mod config_commands;
pub mod error;
pub mod exec;
pub mod exit;
mod fsck;
mod hooks;
pub mod job_command;
pub mod jobs;
mod list;
pub mod names;
mod notes;
mod organisation;
mod organisation_dates;
mod organisation_read;
pub mod process;
pub mod progress;
pub mod report;
mod search;
pub mod selection;
mod status;
pub mod storage;
pub mod supervisor;
mod test_commands;
pub mod tests_store;
pub mod transport;
mod workspace;
mod workspace_commands;
mod workspace_files;

use clap::{CommandFactory, error::ErrorKind};

use crate::{
    cli::{Cli, Command},
    error::Error,
};

/// Run the parsed CLI.
pub fn run(cli: Cli) -> Result<exit::ExitCode, Error> {
    cli.validate_surfaces()?;
    if cli.problem.is_some() && matches!(&cli.command, Some(Command::Switch { .. })) {
        return Err(clap::Error::raw(
            ErrorKind::ArgumentConflict,
            "--problem cannot be used with switch",
        )
        .into());
    }
    if let Some(command) = cli.command.as_ref() {
        let unavailable = match command {
            Command::Fetch(_) => Some("fetch"),
            Command::Stress(args) => {
                if args.command.is_none() {
                    config_commands::stress_preflight(
                        cli.problem.as_deref(),
                        cli.config.clone(),
                        args.profile.clone(),
                    )?;
                }
                Some(if args.command.is_some() {
                    "stress ls"
                } else {
                    "stress"
                })
            }
            Command::InteractorTest { .. } => Some("interactor-test"),
            Command::SubmitPrep(_) => Some("submit-prep"),
            Command::Archive(_) => Some("archive"),
            Command::Quarantine { command } => Some(match command {
                cli::QuarantineCommand::Ls => "quarantine ls",
                cli::QuarantineCommand::Purge { .. } => "quarantine purge",
            }),
            Command::Restore { .. } => Some("restore"),
            _ => None,
        };
        if let Some(name) = unavailable {
            return Err(unavailable_command(name));
        }
    }
    match cli.command {
        Some(Command::Ls(args)) => return list::run(args),
        Some(Command::Search(args)) => return search::run(args),
        Some(Command::Tag(args)) => {
            return organisation::run(&args.target, organisation::Operation::Tag(&args.tags), None)
                .map(|()| exit::ExitCode::Success);
        }
        Some(Command::Untag(args)) => {
            return organisation::run(
                &args.target,
                organisation::Operation::Untag(&args.tags),
                None,
            )
            .map(|()| exit::ExitCode::Success);
        }
        Some(Command::Mark { target, status }) => {
            return organisation::run(&target, organisation::Operation::Mark(&status), cli.config)
                .map(|()| exit::ExitCode::Success);
        }
        Some(Command::Notes {
            reference,
            path,
            json,
        }) => return notes::run(reference.as_deref(), cli.problem.as_deref(), path, json),
        Some(Command::Config { command }) => {
            return config_commands::run(command, cli.problem.as_deref(), cli.config);
        }
        Some(Command::Doctor) => {
            return config_commands::doctor(cli.problem.as_deref(), cli.config);
        }

        Some(Command::Cmd {
            name,
            profile,
            machine,
            extra,
        }) => {
            return job_command::run_with_config(
                &job_command::NamedCommand { name, extra },
                cli.problem.as_deref(),
                profile,
                machine,
                cli.config,
            );
        }
        Some(Command::Build { profile, machine }) => {
            return job_command::run_with_config(
                &job_command::BuildCommand,
                cli.problem.as_deref(),
                profile,
                machine,
                cli.config,
            );
        }
        Some(Command::Run { profile, deadline }) => {
            // Raw mode does not flatten machine options: clap rejects them (2).
            return job_command::run_with_config(
                &job_command::RunCommand { deadline },
                cli.problem.as_deref(),
                profile,
                transport::MachineArgs::default(),
                cli.config,
            );
        }
        Some(Command::Test {
            command,
            cases,
            profile,
            deadline,
            machine,
        }) => {
            if let Some(command) = command {
                if !cases.is_empty() || profile.is_some() || deadline.is_some() {
                    return Err(clap::Error::raw(
                        ErrorKind::ArgumentConflict,
                        "--case, --profile and --deadline apply to test jobs, not catalog subcommands",
                    )
                    .into());
                }
                return test_commands::run(cli.problem.as_deref(), command, machine)
                    .map(|()| exit::ExitCode::Success);
            }
            let command = job_command::TestRunCommand::new(cases, deadline)?;
            return job_command::run_with_config(
                &command,
                cli.problem.as_deref(),
                profile,
                machine,
                cli.config,
            );
        }
        Some(Command::Status {
            reference,
            all,
            json,
            filters,
        }) => {
            if cli.problem.is_some() && (reference.is_some() || all) {
                return Err(clap::Error::raw(
                    ErrorKind::ArgumentConflict,
                    "--problem cannot be combined with a status reference or --all",
                )
                .into());
            }
            return status::run(
                reference.as_deref(),
                all,
                json,
                cli.problem.as_deref(),
                cli.config,
                &filters,
            );
        }
        Some(Command::Adopt { path }) => {
            return workspace_commands::adopt(&path, cli.problem.as_deref(), cli.config);
        }
        Some(Command::Ignore { pattern }) => {
            return workspace_commands::ignore(&pattern);
        }
        Some(Command::Fsck { apply }) => {
            return fsck::run(apply, cli.config);
        }
        Some(Command::Clean { apply }) => {
            return workspace_commands::clean(apply);
        }
        Some(Command::Gitignore { write }) => {
            return workspace_commands::gitignore(write);
        }
        Some(Command::Current { path }) => {
            return selection::current(cli.problem.as_deref(), path)
                .map(|()| exit::ExitCode::Success);
        }
        Some(Command::Switch { reference }) => {
            return selection::switch(&reference, cli.config);
        }
        Some(Command::Init { path }) => {
            return workspace::init(&path).map(|()| exit::ExitCode::Success);
        }
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
            )
            .map(|()| exit::ExitCode::Success);
        }
        None => {}
        // Unfinished surfaces returned above; stress first checks its I/O declaration.
        Some(_) => unreachable!("unfinished command dispatched above"),
    }
    let help = Cli::command().render_help().to_string();
    Err(clap::Error::raw(ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand, help).into())
}

/// Contract §9.9 R-U02: parsing succeeds, but no feature work may start.
pub(crate) fn unavailable_command(name: &str) -> Error {
    clap::Error::raw(
        ErrorKind::InvalidValue,
        format!("cpf {name}: not available yet"),
    )
    .into()
}
