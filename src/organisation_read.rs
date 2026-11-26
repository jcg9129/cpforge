//! Lock-free organisation snapshots and the shared problem-filter evaluator.
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use crate::cli::Filters;
use crate::error::Error;
use crate::names::{self, Identifier, ProblemIdentity};
use crate::organisation::{self, Finding};
use crate::storage::{self, Record};

#[derive(Clone, Debug)]
pub(crate) struct Source {
    pub path: PathBuf,
    pub kind: &'static str,
    pub field: Option<&'static str>,
    pub message: String,
}

#[derive(Debug)]
pub(crate) struct Problem {
    pub selected: crate::selection::Problem,
    pub document: DocumentMut,
}
impl Problem {
    pub fn fields(&self) -> Option<&Item> {
        self.document.get("problem")
    }
    pub fn status(&self) -> Option<&str> {
        effective_status(self.fields())
    }
}

pub(crate) fn effective_status(fields: Option<&Item>) -> Option<&str> {
    match fields.and_then(|fields| fields.get("status")) {
        None => Some("new"),
        Some(item) => item
            .as_str()
            .filter(|status| crate::cli::status_name(status).is_ok()),
    }
}

pub(crate) fn timestamp(fields: Option<&Item>, field: &str) -> Option<String> {
    let text = fields?.get(field)?.as_str()?;
    if !organisation::timestamp_valid(text) {
        return None;
    }
    Some(match text.len() {
        10 => format!("{text}T00:00:00.000Z"),
        20 => format!("{}.000Z", &text[..19]),
        _ => text.to_owned(),
    })
}

#[derive(Debug)]
pub(crate) struct Contest {
    pub name: String,
    pub document: DocumentMut,
}
impl Contest {
    pub fn identity(&self) -> Option<&str> {
        self.document.get("contest")?.get("id")?.as_str()
    }
    pub fn members(&self) -> Vec<&str> {
        self.document
            .get("problem")
            .and_then(Item::as_array_of_tables)
            .into_iter()
            .flatten()
            .filter_map(|record| record.get("id").and_then(Item::as_str))
            .collect()
    }
}

fn entries(root: &Path, directory: &str) -> Result<Vec<OsString>, Error> {
    let dir = storage::checked_path(root, Path::new(directory), false)?;
    let listing = match fs::read_dir(&dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::Workspace(format!("{}: {error}", dir.display()))),
    };
    let mut result = listing
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| Error::Workspace(format!("{}: {error}", dir.display())))?;
    result.sort_by(|left, right| left.as_encoded_bytes().cmp(right.as_encoded_bytes()));
    Ok(result)
}

fn entry_name(
    root: &Path,
    directory: &str,
    name: &OsStr,
    names: &[OsString],
) -> Result<Identifier, Source> {
    let path = Path::new(directory).join(name);
    let invalid = |kind, message| Source {
        path: path.clone(),
        kind,
        field: None,
        message,
    };
    let id = Identifier::from_os_str(name)
        .map_err(|error| invalid("invalid_name", error.to_string()))?;
    if names
        .iter()
        .filter(|other| names::collides_with_native(id.as_str(), other))
        .count()
        != 1
    {
        return Err(invalid(
            "name_collision",
            "colliding directory names".into(),
        ));
    }
    let metadata = fs::symlink_metadata(root.join(&path))
        .map_err(|error| invalid("metadata_unreadable", error.to_string()))?;
    if storage::is_link(&metadata) {
        return Err(invalid("link", "entry is a link".into()));
    }
    if !metadata.is_dir() {
        return Err(invalid(
            "not_a_directory",
            "entry is not a directory".into(),
        ));
    }
    Ok(id)
}

pub(crate) fn record<T>(
    path: PathBuf,
    record: Record<T, String>,
    contest: bool,
) -> Result<T, Source> {
    let (kind, message) = match record {
        Record::Valid(value) => return Ok(value),
        Record::Missing => ("metadata_missing", "record missing".into()),
        Record::Unreadable(error) => (
            if contest {
                "contest_unreadable"
            } else {
                "metadata_unreadable"
            },
            error.to_string(),
        ),
        Record::Malformed(error) => (
            if contest {
                "contest_malformed"
            } else {
                "metadata_malformed"
            },
            error.to_string(),
        ),
        Record::TooNew { schema, supported } => (
            if contest {
                "contest_too_new"
            } else {
                "metadata_too_new"
            },
            format!("schema {schema}, supported {supported}"),
        ),
    };
    Err(Source {
        path,
        kind,
        field: None,
        message,
    })
}

fn problem_entry(root: &Path, name: &OsStr, names: &[OsString]) -> Result<Problem, Source> {
    let id = entry_name(root, "problems", name, names)?;
    let path = Path::new("problems").join(name).join("problem.toml");
    let (identity, document) = record(
        path.clone(),
        storage::read_user_record(root.join(&path), 1, |doc| {
            let identity = crate::selection::problem_identity(doc)?;
            Ok::<_, String>((
                ProblemIdentity::parse(&identity).expect("validated identity"),
                doc.clone(),
            ))
        }),
        false,
    )?;
    identity.check_binding(&id).map_err(|reason| Source {
        path,
        kind: "identity_mismatch",
        field: None,
        message: format!("{reason:?}: {identity}"),
    })?;
    Ok(Problem {
        selected: crate::selection::Problem {
            dir: root.join("problems").join(name),
            identity,
        },
        document,
    })
}

/// Every entry of `problems/`, valid problems and the reasons for the others,
/// both in directory-name byte order.
pub(crate) fn problems(root: &Path) -> Result<(Vec<Problem>, Vec<Source>), Error> {
    let names = entries(root, "problems")?;
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for name in &names {
        match problem_entry(root, name, &names) {
            Ok(problem) => valid.push(problem),
            Err(source) => invalid.push(source),
        }
    }
    Ok((valid, invalid))
}

fn contest_entry(root: &Path, name: &OsStr, names: &[OsString]) -> Result<Contest, Source> {
    let id = entry_name(root, "contests", name, names)?;
    let path = Path::new("contests").join(name).join("contest.toml");
    let document = record(
        path.clone(),
        storage::read_user_record(root.join(&path), 1, |doc| {
            crate::add::contest_members(doc)?;
            Ok::<_, String>(doc.clone())
        }),
        true,
    )?;
    crate::selection::contest_binding(&document, &id).map_err(|message| Source {
        path,
        kind: "identity_mismatch",
        field: None,
        message,
    })?;
    Ok(Contest {
        name: id.to_string(),
        document,
    })
}

/// Every entry of `contests/`, as `problems` does for `problems/`.
pub(crate) fn contests(root: &Path) -> Result<(Vec<Contest>, Vec<Source>), Error> {
    let names = entries(root, "contests")?;
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for name in &names {
        match contest_entry(root, name, &names) {
            Ok(contest) => valid.push(contest),
            Err(source) => invalid.push(source),
        }
    }
    Ok((valid, invalid))
}

pub(crate) fn field_sources(path: PathBuf, findings: Vec<Finding>) -> Vec<Source> {
    findings
        .into_iter()
        .map(|finding| Source {
            path: path.clone(),
            kind: "invalid_field",
            field: Some(finding.field),
            message: finding.reason,
        })
        .collect()
}

pub(crate) fn validate_contests(
    filters: &Filters,
    contests: &[Contest],
    invalid: &[Source],
) -> Result<(), Error> {
    for name in &filters.contest {
        if !contests.iter().any(|contest| contest.name == *name) {
            let path = Path::new("contests").join(name);
            let detail = invalid
                .iter()
                .find(|source| source.path.starts_with(&path))
                .map_or("no valid contest", |source| source.message.as_str());
            return Err(Error::Workspace(format!("contest {name}: {detail}")));
        }
    }
    Ok(())
}

fn tags_match(filters: &Filters, tags: Result<Vec<String>, String>) -> bool {
    filters.tag.is_empty()
        || tags.is_ok_and(|tags| {
            filters
                .tag
                .iter()
                .all(|wanted| tags.iter().any(|tag| tag.eq_ignore_ascii_case(wanted)))
        })
}
fn site_match(filters: &Filters, identity: &str) -> bool {
    filters.site.is_empty()
        || filters.site.iter().any(|site| {
            identity
                .split_once(':')
                .is_some_and(|(actual, _)| actual == site)
        })
}
pub(crate) fn matches(
    filters: &Filters,
    problem: &Problem,
    contests: &[Contest],
    zone: &jiff::tz::TimeZone,
) -> bool {
    let identity = problem.selected.identity.to_string();
    (filters.status.is_empty()
        || problem
            .status()
            .is_some_and(|status| filters.status.iter().any(|wanted| wanted == status)))
        && tags_match(
            filters,
            organisation::tags(problem.fields().and_then(|fields| fields.get("tags"))),
        )
        && crate::organisation_dates::matches(filters, problem.fields(), zone)
        && site_match(filters, &identity)
        && (filters.contest.is_empty()
            || contests.iter().any(|contest| {
                filters.contest.contains(&contest.name)
                    && contest.members().contains(&identity.as_str())
            }))
}
pub(crate) fn matches_contest(filters: &Filters, contest: &Contest) -> bool {
    tags_match(filters, organisation::tags(contest.document.get("tags")))
        && site_match(filters, contest.identity().unwrap_or("local:"))
}

pub(crate) fn counts(contest: &Contest, problems: &[Problem]) -> (Vec<(String, u32)>, u32, u32) {
    let mut counts = BTreeMap::<String, u32>::new();
    let mut unresolved = 0;
    let mut invalid = 0;
    for identity in contest.members() {
        match problems
            .iter()
            .find(|problem| problem.selected.identity.to_string() == identity)
        {
            Some(problem) => match problem.status() {
                Some(status) => *counts.entry(status.to_owned()).or_default() += 1,
                None => invalid += 1,
            },
            None => unresolved += 1,
        }
    }
    let mut ordered = Vec::new();
    for status in organisation::BUILT_INS {
        if let Some(count) = counts.remove(status) {
            ordered.push((status.to_owned(), count));
        }
    }
    ordered.extend(counts);
    (ordered, unresolved, invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_status_keeps_absent_invalid_and_removed_values_distinct() {
        for (stored, expected) in [
            ("", Some("new")),
            ("status = '5'", None),
            ("status = 5", None),
            ("status = 'new'", Some("new")),
            ("status = 'wa'", Some("wa")),
        ] {
            let document: DocumentMut = format!("[problem]\n{stored}\n").parse().unwrap();
            assert_eq!(effective_status(document.get("problem")), expected);
        }
    }
}
