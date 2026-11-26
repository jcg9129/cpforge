//! Fixed-path notes creation; existing user content is never opened for writing.
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::exit::ExitCode;
use crate::report::wire;
use crate::storage::{self, ErrorKind, WorkspaceLock};
use crate::{selection, transport};

fn state(path: &Path) -> Result<&'static str, Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !storage::is_link(&metadata) && metadata.is_file() => Ok("existing"),
        Ok(_) => Ok("not_regular"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok("absent"),
        Err(error) => Err(Error::Workspace(format!("{}: {error}", path.display()))),
    }
}

fn locate(
    reference: Option<&str>,
    problem: Option<&str>,
    path_only: bool,
) -> Result<(String, PathBuf, &'static str), Error> {
    let cwd = std::env::current_dir().map_err(|error| Error::Workspace(error.to_string()))?;
    let root = crate::workspace::find_root(&cwd)?;
    let selected = selection::resolve(&root, &cwd, reference.or(problem).filter(|r| *r != "."))?;
    let path = selected.dir.join("notes.md");
    if path_only {
        return Ok((selected.identity.to_string(), path.clone(), state(&path)?));
    }
    let lock = WorkspaceLock::new(&root)?;
    let guard = lock.acquire("notes")?;
    crate::workspace_commands::recovery_warnings(&guard);
    // Resolution precedes the lock by contract. Re-check the parent path after
    // recovery so a replacement with a link cannot redirect publication.
    guard.check_path(&selected.dir)?;
    let mut current = state(&path)?;
    if current == "absent" {
        match storage::publish_notes(&guard, &path) {
            Ok(published) => {
                crate::workspace_commands::publication_warnings(&path, &published);
                current = "created";
            }
            Err(error) if matches!(error.kind, ErrorKind::Exists { .. }) => {
                if error.cleanup != storage::Cleanup::Done {
                    eprintln!("cpf: warning: notes temp cleanup: {:?}", error.cleanup);
                }
                current = state(&path)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if !matches!(current, "created" | "existing") {
        return Err(Error::Workspace(format!(
            "{}: not a regular file",
            path.display()
        )));
    }
    Ok((selected.identity.to_string(), path, current))
}

pub(crate) fn run(
    reference: Option<&str>,
    problem: Option<&str>,
    path_only: bool,
    json: bool,
) -> Result<ExitCode, Error> {
    let mut delivery = transport::Delivery::stdout();
    if json {
        delivery.preflight()?;
    }
    let result = locate(reference, problem, path_only);
    let (identity, path, state) = match result {
        Ok(result) => result,
        Err(error) if json => {
            let exit = error.exit_code();
            let class = error.class();
            let bytes = wire::encode_error(&wire::ErrorDocument {
                platform: transport::platform(),
                command: "notes".into(),
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
            return Ok(exit);
        }
        Err(error) => return Err(error),
    };
    eprintln!(
        "{}",
        match state {
            "created" => "created",
            "existing" if !path_only => "exists",
            "not_regular" => "not a regular file",
            other => other,
        }
    );
    if json {
        let bytes = wire::encode_notes(transport::platform(), &identity, &path, state)
            .map_err(|error| Error::Internal(error.to_string()))?;
        delivery.document(&bytes)?;
    } else {
        let mut bytes = wire::native_bytes(path.as_os_str());
        bytes.push(b'\n');
        let mut output = io::stdout().lock();
        output
            .write_all(&bytes)
            .and_then(|()| output.flush())
            .map_err(|error| Error::Workspace(format!("cannot write notes path: {error}")))?;
    }
    Ok(ExitCode::Success)
}
