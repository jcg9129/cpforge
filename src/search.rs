//! Bounded, lock-free search. Root traversal is isolated from matching so archive
//! sources can later feed the same candidate publication and delivery path.
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::cli::SearchArgs;
use crate::error::Error;
use crate::exit::ExitCode;
use crate::jobs::engine::CancelState;
use crate::organisation_read::Source;
use crate::{report::wire, transport};

mod reader;

pub(super) const REASONS: [&str; 5] = [
    "entry_limit",
    "byte_limit",
    "notes_truncated",
    "unreadable_source",
    "cancelled",
];

#[derive(Clone, Debug)]
pub(crate) struct Hit {
    pub problem: String,
    pub origin: &'static str,
    pub fields: Vec<&'static str>,
    pub notes_lines: Vec<u32>,
    pub notes_lines_dropped: u32,
    pub notes_truncated: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Results {
    pub hits: Vec<Hit>,
    pub findings: Vec<Source>,
    pub reasons: [bool; 5],
    pub entries: u32,
    pub bytes_read: u64,
}
impl Results {
    pub fn reasons(&self) -> Vec<&'static str> {
        REASONS
            .into_iter()
            .zip(self.reasons)
            .filter_map(|(name, set)| set.then_some(name))
            .collect()
    }
}

pub(crate) fn run(args: SearchArgs) -> Result<ExitCode, Error> {
    let json = args.json;
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deliver(args))) {
        Ok(result) => result,
        Err(payload) => {
            if json {
                let _ = transport::Delivery::stdout().escaped_panic("search");
            }
            std::panic::resume_unwind(payload)
        }
    }
}

fn deliver(args: SearchArgs) -> Result<ExitCode, Error> {
    // Explicit archive roots stay at the unavailable surface until root
    // containment and snapshot traversal are implemented together.
    if !args.root.is_empty() {
        return Err(crate::unavailable_command("search --root"));
    }
    let mut delivery = transport::Delivery::stdout();
    if args.json {
        delivery.preflight()?;
    }
    let cancel = CancelState::new();
    let signals = crate::cancel_signals::CancelSignals::install(cancel.clone())
        .map_err(|error| Error::Workspace(format!("cannot listen for cancellation: {error}")))?;
    let terms = args.terms.clone();
    let notes = !args.no_notes;
    let json = args.json;
    let shared = Arc::new(Mutex::new(Results::default()));
    let published = shared.clone();
    let stopping = cancel.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("cpf-search".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let root = crate::workspace_commands::root()?;
                reader::workspace(&root, &args, &stopping, &published)
            }));
            let _ = tx.send(result);
        })
        .map_err(|error| Error::Workspace(format!("cannot start search reader: {error}")))?;
    let result = loop {
        if cancel.cancellation().is_some() {
            break Ok(());
        }
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(Ok(result)) => break result,
            Ok(Err(payload)) => std::panic::resume_unwind(payload),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Internal("search worker disconnected".into()));
            }
        }
    };
    let mut results = shared
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    if cancel.cancellation().is_some() {
        results.reasons[4] = true;
    }
    let status = match result {
        Ok(()) => {
            let bytes = wire::encode_search(transport::platform(), &terms, notes, &results);
            crate::storage::crash::barrier("search.stdout");
            if json {
                delivery.document(&bytes)?;
            } else {
                let output = human(&results);
                io::stdout()
                    .lock()
                    .write_all(output.as_bytes())
                    .and_then(|()| io::stdout().flush())
                    .map_err(|error| Error::Workspace(format!("cannot write search: {error}")))?;
            }
            if results.reasons.iter().any(|set| *set) {
                eprintln!("search incomplete: {}", results.reasons().join(", "));
            }
            if results.reasons[4] {
                ExitCode::Cancelled
            } else {
                ExitCode::Success
            }
        }
        Err(error) if json => {
            let exit = error.exit_code();
            let class = error.class();
            let bytes = wire::encode_error(&wire::ErrorDocument {
                platform: transport::platform(),
                command: "search".into(),
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
            exit
        }
        Err(error) => return Err(error),
    };
    Ok(signals.finish(status))
}

fn human(results: &Results) -> String {
    let mut output = String::new();
    for hit in &results.hits {
        let _ = writeln!(
            output,
            "{}\t{}\t{}\tnotes lines: {:?}\tnotes_truncated: {}",
            hit.problem,
            hit.origin,
            hit.fields.join(", "),
            hit.notes_lines,
            hit.notes_truncated
        );
    }
    if results.hits.is_empty() {
        output.push_str("no matches\n");
    }
    for finding in &results.findings {
        let _ = writeln!(
            output,
            "{}{}: {}: {}",
            finding.path.display(),
            finding
                .field
                .map_or(String::new(), |field| format!(" {field}")),
            finding.kind,
            finding.message
        );
    }
    output
}

fn contains(text: &str, terms: &[String]) -> Vec<bool> {
    let key = crate::names::collision_key(text);
    terms.iter().map(|term| key.contains(term)).collect()
}

fn match_problem(
    problem: &crate::organisation_read::Problem,
    notes: Option<&str>,
    truncated: bool,
    terms: &[String],
) -> Option<Hit> {
    let identity = problem.selected.identity.to_string();
    let fields = problem.fields();
    let mut matched = vec![false; terms.len()];
    let mut names = Vec::new();
    let mut lines = Vec::new();
    let mut dropped = 0;
    let tags =
        crate::organisation::tags(fields.and_then(|fields| fields.get("tags"))).unwrap_or_default();
    let stored = |field: &str| {
        fields
            .and_then(|fields| fields.get(field))
            .and_then(toml_edit::Item::as_str)
    };
    let mut values = vec![("identity", identity.clone())];
    for field in ["title", "url"] {
        if let Some(value) = stored(field) {
            values.push((field, value.to_owned()));
        }
    }
    // Tags are matched separately: substrings must not cross element boundaries.
    for value in tags {
        values.push(("tags", value));
    }
    if let Some(status) = stored("status").filter(|status| crate::cli::status_name(status).is_ok())
    {
        values.push(("status", status.to_owned()));
    }
    if let Some(notes) = notes {
        values.push(("notes", notes.to_owned()));
    }
    for (field, value) in values {
        let matches = contains(&value, terms);
        if matches.iter().any(|found| *found) && !names.contains(&field) {
            names.push(field);
        }
        for (found, value) in matched.iter_mut().zip(matches) {
            *found |= value;
        }
        if field == "notes" {
            for (index, line) in value.split('\n').enumerate() {
                if contains(line.strip_suffix('\r').unwrap_or(line), terms)
                    .iter()
                    .any(|found| *found)
                {
                    if lines.len() < 16 {
                        lines.push(index as u32 + 1);
                    } else {
                        dropped += 1;
                    }
                }
            }
        }
    }
    matched.iter().all(|found| *found).then_some(Hit {
        problem: identity,
        origin: "active",
        fields: names,
        notes_lines: lines,
        notes_lines_dropped: dropped,
        notes_truncated: truncated,
    })
}

pub(super) fn source(path: &Path, kind: &'static str, message: impl ToString) -> Source {
    Source {
        path: path.to_owned(),
        kind,
        field: None,
        message: message.to_string(),
    }
}
