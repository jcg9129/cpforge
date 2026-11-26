//! User-owned organisation fields and replayable record mutations.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use toml_edit::{Array, DocumentMut, Item, TableLike, Value, value};

use crate::config::{ConfigRequest, Environment, Severity, load_config};
use crate::error::Error;
use crate::names::Identifier;
use crate::selection;
use crate::storage::{self, EditError, WorkspaceLock, crash};
use crate::workspace::find_root;
use crate::workspace_commands::{publication_warnings, recovery_warnings};

const MAX_INSTANT: &str = "9999-12-30T22:00:00.999Z";
const BUILT_INS: [&str; 4] = ["new", "in-progress", "solved", "skipped"];

#[derive(Debug)]
pub(crate) struct Finding {
    pub field: &'static str,
    pub reason: String,
}

pub(crate) fn tags(item: Option<&Item>) -> Result<Vec<String>, String> {
    let Some(item) = item else {
        return Ok(Vec::new());
    };
    let array = item.as_array().ok_or("expected an array of tags")?;
    let mut keys = BTreeSet::new();
    let mut result = Vec::new();
    for (index, value) in array.iter().enumerate() {
        let reason = |reason: &str| format!("element {}: {reason}", index + 1);
        if index >= 64 {
            return Err(reason("too_many_tags"));
        }
        let tag = value.as_str().ok_or_else(|| reason("expected a string"))?;
        crate::cli::tag_name(tag).map_err(|error| reason(&error))?;
        if !keys.insert(tag.to_ascii_lowercase()) {
            return Err(reason("duplicate tag key"));
        }
        result.push(tag.to_owned());
    }
    Ok(result)
}

fn date_valid(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return false;
    }
    let year = text[..4].parse::<u32>().unwrap_or(0);
    let month = text[5..7].parse::<u32>().unwrap_or(0);
    let day = text[8..].parse::<u32>().unwrap_or(0);
    let days = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    year >= 1970 && (1..=days).contains(&day)
}

fn timestamp_valid(text: &str) -> bool {
    if text.len() == 10 {
        return date_valid(text);
    }
    let bytes = text.as_bytes();
    if !matches!(bytes.len(), 20 | 24)
        || !text.is_ascii()
        || !date_valid(&text[..10])
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[bytes.len() - 1] != b'Z'
    {
        return false;
    }
    for (start, limit) in [(11, 23), (14, 59), (17, 59)] {
        let part = &text[start..start + 2];
        if !part.bytes().all(|b| b.is_ascii_digit()) || part.parse::<u32>().unwrap_or(100) > limit {
            return false;
        }
    }
    if bytes.len() == 24 && (bytes[19] != b'.' || !bytes[20..23].iter().all(u8::is_ascii_digit)) {
        return false;
    }
    let canonical = if bytes.len() == 20 {
        format!("{}.000Z", &text[..19])
    } else {
        text.to_owned()
    };
    canonical.as_str() <= MAX_INSTANT
}

fn validate_field(field: &str, item: Option<&Item>) -> Result<(), String> {
    let Some(item) = item else {
        return Ok(());
    };
    match field {
        "tags" => tags(Some(item)).map(|_| ()),
        "status" => item
            .as_str()
            .ok_or_else(|| "expected a status string".to_owned())
            .and_then(|status| crate::cli::status_name(status).map(|_| ())),
        _ => {
            if item.as_str().is_some_and(timestamp_valid) {
                Ok(())
            } else {
                Err("expected a timestamp string in the supported domain".into())
            }
        }
    }
}

pub(crate) fn findings(fields: Option<&Item>) -> Vec<Finding> {
    [
        "tags",
        "status",
        "created_at",
        "updated_at",
        "solved_at",
        "archived_at",
    ]
    .into_iter()
    .filter_map(|field| {
        validate_field(field, fields.and_then(|fields| fields.get(field)))
            .err()
            .map(|reason| Finding { field, reason })
    })
    .collect()
}

/// One UTC clock sample, checked before the invocation can write anything.
pub(crate) fn now() -> Result<String, Error> {
    #[cfg(feature = "storage-test-hooks")]
    if let Some(clock) = std::env::var_os("CPF_STORAGE_ORG_NOW") {
        let clock = clock
            .to_str()
            .ok_or_else(|| Error::Workspace("clock is not Unicode".into()))?;
        if clock.len() == 24 && timestamp_valid(clock) {
            return Ok(clock.to_owned());
        }
        return Err(Error::Workspace(format!(
            "clock outside the supported instant domain: {clock}"
        )));
    }
    let clock = crate::report::wire::timestamp(SystemTime::now())
        .map_err(|error| Error::Workspace(format!("clock: {error}")))?;
    if timestamp_valid(&clock) {
        Ok(clock)
    } else {
        Err(Error::Workspace(format!(
            "clock outside the supported instant domain: {clock}"
        )))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Operation<'a> {
    Tag(&'a [String]),
    Untag(&'a [String]),
    Mark(&'a str),
}

fn vocabulary(
    root: &Path,
    cwd: PathBuf,
    config_path: Option<PathBuf>,
    status: &str,
) -> Result<(), Error> {
    let loaded = load_config(&ConfigRequest {
        current_dir: cwd,
        workspace_root: Some(root.to_path_buf()),
        config_path,
        environment: Environment {
            home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            appdata: std::env::var_os("APPDATA"),
        },
        ..ConfigRequest::default()
    });
    let blocked = !loaded.merged().unknown_inputs.is_empty()
        || loaded.diagnostics().iter().any(|d| {
            d.severity == Severity::Error
                && (d.origin.key_path == "status" || d.origin.key_path.starts_with("status.values"))
        });
    if blocked {
        return Err(Error::Config(format!(
            "cannot determine status.values: {}",
            loaded
                .diagnostics()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )));
    }
    for diagnostic in loaded.diagnostics() {
        eprintln!("cpf: warning: {diagnostic}");
    }
    let mut values: Vec<_> = BUILT_INS.into_iter().map(str::to_owned).collect();
    if let Some(custom) = loaded
        .merged()
        .at(&["status", "values"])
        .and_then(|node| node.array())
    {
        values.extend(
            custom
                .iter()
                .filter_map(|node| node.string())
                .map(str::to_owned),
        );
    }
    if values.iter().any(|value| value == status) {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "unknown status {status:?}; vocabulary: {}",
            values.join(", ")
        )))
    }
}

fn require_valid(fields: &dyn TableLike, field: &str) -> Result<(), String> {
    validate_field(field, fields.get(field)).map_err(|reason| format!("{field}: {reason}"))
}

// Replace the value in place: the key keeps its spelling and spacing, the
// value its prefix/suffix (including an end-of-line comment).
fn set_string(fields: &mut dyn TableLike, key: &str, text: &str) {
    let mut replacement = Value::from(text);
    if let Some(item) = fields.get_mut(key) {
        if let Some(old) = item.as_value() {
            *replacement.decor_mut() = old.decor().clone();
        }
        *item = Item::Value(replacement);
    } else {
        fields.insert(key, Item::Value(replacement));
    }
}

// Move trailing array comments in front of the appended element so an old
// last element keeps its exact line, including a comment after its comma.
fn append_tag(array: &mut Array, tag: &str) {
    if !array.to_string().contains('\n') {
        array.push(tag);
        return;
    }
    let raw = |decor: Option<&toml_edit::RawString>| {
        decor.and_then(|raw| raw.as_str()).unwrap_or("").to_owned()
    };
    // Without a trailing comma the layout before `]` is the last element's
    // suffix; it moves behind the new comma so the old line stays intact.
    let mut trailing = String::new();
    if let Some(last) = array.len().checked_sub(1).and_then(|i| array.get_mut(i)) {
        trailing = raw(last.decor().suffix());
        last.decor_mut().set_suffix("");
    }
    trailing.push_str(array.trailing().as_str().unwrap_or(""));
    let indent = array
        .iter()
        .next()
        .and_then(|value| value.decor().prefix())
        .and_then(|raw| raw.as_str())
        .and_then(|prefix| prefix.rsplit_once('\n'))
        .map_or("  ", |(_, indent)| indent)
        .to_owned();
    let mut value = Value::from(tag);
    if let Some((comments, closing_indent)) = trailing.rsplit_once('\n') {
        value
            .decor_mut()
            .set_prefix(format!("{comments}\n{indent}"));
        array.set_trailing(format!("\n{closing_indent}"));
    } else {
        value.decor_mut().set_prefix(format!("\n{indent}"));
        array.set_trailing(trailing);
    }
    array.push_formatted(value);
    array.set_trailing_comma(true);
}

// Comments inside the edited array remain user content when their value is
// removed. Attach that decor to the following element, or to the closing bracket.
fn remove_tag(array: &mut Array, index: usize) {
    let removed = array.remove(index);
    let raw = |decor: Option<&toml_edit::RawString>| {
        decor.and_then(|raw| raw.as_str()).unwrap_or("").to_owned()
    };
    let leading = raw(removed.decor().prefix());
    let comments = format!("{leading}{}", raw(removed.decor().suffix()));
    if !comments.contains('#') {
        // A new first element takes the old first element's layout, so
        // `["a", "b"]` becomes `["b"]` rather than `[ "b"]`.
        let first = array
            .get_mut(0)
            .filter(|next| index == 0 && !raw(next.decor().prefix()).contains('#'));
        if let Some(next) = first {
            next.decor_mut().set_prefix(leading);
        }
        return;
    }
    if let Some(next) = array.get_mut(index) {
        let prefix = raw(next.decor().prefix());
        next.decor_mut().set_prefix(format!("{comments}{prefix}"));
    } else {
        let trailing = array.trailing().as_str().unwrap_or("").to_owned();
        array.set_trailing(format!("{comments}{trailing}"));
    }
}

fn edit(
    fields: &mut dyn TableLike,
    operation: Operation<'_>,
    now: &str,
    problem: bool,
) -> Result<(bool, String), String> {
    let (changed, result) = match operation {
        Operation::Tag(arguments) | Operation::Untag(arguments) => {
            let old = tags(fields.get("tags")).map_err(|reason| format!("tags: {reason}"))?;
            let mut seen = BTreeSet::new();
            let arguments: Vec<_> = arguments
                .iter()
                .filter(|tag| seen.insert(tag.to_ascii_lowercase()))
                .collect();
            let mut next = old.clone();
            let mut missing = Vec::new();
            for tag in arguments {
                let found = next
                    .iter()
                    .position(|existing| existing.eq_ignore_ascii_case(tag));
                match operation {
                    Operation::Tag(_) if found.is_none() => next.push(tag.clone()),
                    Operation::Untag(_) => {
                        if let Some(index) = found {
                            next.remove(index);
                        } else {
                            missing.push(tag.as_str());
                        }
                    }
                    _ => {}
                }
            }
            if next.len() > 64 {
                return Err("tags: too_many_tags".into());
            }
            let changed = old != next;
            if changed {
                if fields.get("tags").is_none() {
                    fields.insert("tags", value(Array::new()));
                }
                let array = fields
                    .get_mut("tags")
                    .and_then(Item::as_array_mut)
                    .expect("validated tags");
                match operation {
                    Operation::Tag(_) => {
                        for tag in &next[old.len()..] {
                            append_tag(array, tag);
                        }
                    }
                    Operation::Untag(_) => {
                        for index in (0..old.len()).rev() {
                            if !next.iter().any(|tag| tag.eq_ignore_ascii_case(&old[index])) {
                                remove_tag(array, index);
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            let note = if missing.is_empty() {
                String::new()
            } else {
                format!("; tags not present: {}", missing.join(", "))
            };
            (changed, format!("tags: [{}]{note}", next.join(", ")))
        }
        Operation::Mark(status) => {
            require_valid(fields, "status")?;
            if status == "solved" {
                require_valid(fields, "solved_at")?;
            }
            let changed = fields.get("status").and_then(Item::as_str) != Some(status);
            if changed {
                set_string(fields, "status", status);
                if status == "solved" && fields.get("solved_at").is_none() {
                    set_string(fields, "solved_at", now);
                }
            }
            (changed, format!("workflow: {status}"))
        }
    };
    if changed && problem {
        require_valid(fields, "updated_at")?;
        set_string(fields, "updated_at", now);
    }
    Ok((changed, result))
}

/// The record cap is reported by its contract name (storage §5.9.4).
fn edit_error(error: EditError<String>) -> Error {
    let too_large = match &error {
        EditError::TooLarge { .. } => true,
        EditError::Replay { error, .. } => matches!(**error, EditError::TooLarge { .. }),
        _ => false,
    };
    Error::Workspace(if too_large {
        format!("record_too_large: {error}")
    } else {
        error.to_string()
    })
}

pub(crate) fn run(
    target: &str,
    operation: Operation<'_>,
    config_path: Option<PathBuf>,
) -> Result<(), Error> {
    let now = now()?;
    let cwd = std::env::current_dir().map_err(|error| Error::Workspace(error.to_string()))?;
    let root = find_root(&cwd)?;
    if let Operation::Mark(status) = operation {
        vocabulary(&root, cwd.clone(), config_path, status)?;
    }
    let lock = WorkspaceLock::new(&root)?;
    let guard = lock.acquire("organisation")?;
    recovery_warnings(&guard);
    let contest = target.strip_prefix("contest:");
    let (path, expected) = if let Some(name) = contest {
        let document = selection::read_contest(guard.root(), name)?;
        (
            guard
                .root()
                .join("contests")
                .join(name)
                .join("contest.toml"),
            document
                .get("contest")
                .and_then(|table| table.get("id"))
                .and_then(Item::as_str)
                .map(str::to_owned),
        )
    } else {
        let selected = if target == "." {
            selection::resolve(guard.root(), &cwd, None).map(|s| selection::Problem {
                dir: s.dir,
                identity: s.identity,
            })
        } else {
            selection::lookup(guard.root(), target)
        };
        let selected = selected.map_err(|error| {
            if matches!(error, selection::SelectionError::NotFound { .. })
                && !target.contains([':', '/'])
            {
                let note = if guard.root().join("contests").join(target).is_dir() {
                    "; a contest of that name exists; use contest:<name>"
                } else {
                    ""
                };
                Error::Workspace(format!("no problem `{target}`{note}"))
            } else {
                error.into()
            }
        })?;
        (
            selected.dir.join("problem.toml"),
            Some(selected.identity.to_string()),
        )
    };
    crash::barrier("org.after_resolve");
    let result = RefCell::new(String::new());
    let published = storage::edit_organisation(
        &guard,
        &path,
        |document| {
            if let Some(schema) = document.get("schema") {
                match schema.as_integer() {
                    Some(1) => {}
                    Some(n) if n > 1 => return Err("too_new schema".to_owned()),
                    _ => return Err("malformed schema".to_owned()),
                }
            }
            let actual = if contest.is_some() {
                crate::add::contest_members(document)?;
                document
                    .get("contest")
                    .and_then(|t| t.get("id"))
                    .and_then(Item::as_str)
                    .map(str::to_owned)
            } else {
                Some(selection::problem_identity(document)?)
            };
            if actual != expected {
                return Err("IdentityMismatch: target binding changed".into());
            }
            if let Some(name) = contest {
                selection::contest_binding(
                    document,
                    &Identifier::new(name).map_err(|e| e.to_string())?,
                )?;
            }
            Ok(())
        },
        |document: &mut DocumentMut| {
            let fields: &mut dyn TableLike = if contest.is_some() {
                document.as_table_mut()
            } else {
                document
                    .get_mut("problem")
                    .and_then(Item::as_table_like_mut)
                    .ok_or("problem must be a table")?
            };
            let (changed, message) = edit(fields, operation, &now, contest.is_none())?;
            *result.borrow_mut() = format!(
                "{target}: {message}{}",
                if changed {
                    ""
                } else {
                    "; already had that state"
                }
            );
            Ok::<_, String>(())
        },
    )
    .map_err(edit_error)?;
    publication_warnings(&path, &published);
    eprintln!("{}", result.borrow());
    Ok(())
}
