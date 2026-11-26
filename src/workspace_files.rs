//! Shared registered-file and provenance rules for workspace inspection and adoption.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use crate::config::{ConfigRequest, Environment, TargetPlan};
use crate::names::{self, PortableComponent, ProblemIdentity};
use crate::storage;
use crate::tests_store::{Answer, TestStore};

pub(crate) fn request(
    root: &Path,
    cwd: &Path,
    dir: &Path,
    config_path: Option<PathBuf>,
) -> ConfigRequest {
    ConfigRequest {
        environment: Environment {
            home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            appdata: std::env::var_os("APPDATA"),
        },
        platform: crate::config::Platform::host(),
        current_dir: cwd.into(),
        config_path,
        workspace_root: Some(root.into()),
        problem_file: Some(dir.join("problem.toml")),
        ..ConfigRequest::default()
    }
}

pub(crate) fn check_problem(document: &DocumentMut) -> Result<(), String> {
    check_schema(document)?;
    let id = document
        .get("problem")
        .and_then(|p| p.get("id"))
        .and_then(Item::as_str)
        .ok_or("[problem] id must be a string")?;
    ProblemIdentity::parse(id).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn check_schema(document: &DocumentMut) -> Result<(), String> {
    if let Some(schema) = document.get("schema") {
        let schema = schema.as_integer().ok_or("schema must be an integer")?;
        if schema != 1 {
            return Err(format!("unsupported schema {schema} (supported 1)"));
        }
    }
    Ok(())
}

/// Stored paths use `/` on every host; join components to form native paths.
pub(crate) fn relative(text: &str) -> Result<PathBuf, String> {
    if text.is_empty() || text.contains(['\\', '\0']) {
        return Err(format!("invalid problem-relative path {text:?}"));
    }
    let mut path = PathBuf::new();
    for part in text.split('/') {
        PortableComponent::new(part).map_err(|e| e.to_string())?;
        path.push(part);
    }
    Ok(path)
}

/// Declared paths (configuration fields, test catalog entries) keep the
/// structural checks status has always applied; portable-name validation
/// applies to `[files]` provenance entries only.
fn declared(text: &str) -> Result<PathBuf, String> {
    let structural = |part: &str| {
        let mut components = Path::new(part).components();
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none()
    };
    if text.is_empty()
        || text.contains(['\\', '\0'])
        || text
            .split('/')
            .any(|p| matches!(p, "" | "." | "..") || !structural(p))
    {
        return Err(format!("invalid registered path {text:?}"));
    }
    Ok(text.split('/').collect())
}

pub(crate) fn path_key(path: &Path) -> String {
    path.components()
        .map(|c| names::collision_key(c.as_os_str().to_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn validate_file(dir: &Path, text: &str) -> Result<PathBuf, String> {
    let relative = relative(text)?;
    let path = storage::checked_path(dir, &relative, false).map_err(|e| e.to_string())?;
    if !fs::symlink_metadata(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .is_file()
    {
        return Err(format!("{} is not a regular file", path.display()));
    }
    // A differently spelled sibling occupies a portable name even on a
    // filesystem that allows both entries. Check every component.
    let mut parent = dir.to_path_buf();
    for component in relative.components() {
        let name = component.as_os_str();
        for entry in fs::read_dir(&parent).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name() != name
                && names::collides_with_native(
                    name.to_str().expect("validated Unicode"),
                    &entry.file_name(),
                )
            {
                return Err(format!("spelling collision at {}", entry.path().display()));
            }
        }
        parent.push(name);
    }
    Ok(relative)
}

pub(crate) fn declarations(
    document: &DocumentMut,
    target: Option<&TargetPlan>,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut paths = BTreeSet::from([
        PathBuf::from("problem.toml"),
        PathBuf::from("tests.toml"),
        PathBuf::from("notes.md"),
    ]);
    if let Some(target) = target {
        for file in target.captured_files().chain(&target.solution_files) {
            paths.insert(file.value.components().iter().collect());
        }
    } else {
        for name in ["sources", "support_files", "solution_files"] {
            if let Some(array) = document
                .get("config")
                .and_then(|c| c.get(name))
                .and_then(Item::as_array)
            {
                for value in array {
                    if let Some(path) = value.as_str() {
                        paths.insert(declared(path)?);
                    }
                }
            }
        }
    }
    Ok(paths)
}

pub(crate) fn registered(
    dir: &Path,
    document: &DocumentMut,
    target: Option<&TargetPlan>,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut paths = declarations(document, target)?;
    for test in TestStore::new(dir.into()).catalog()?.entries {
        paths.insert(declared(&test.input)?);
        if let Answer::Named(path) = test.answer {
            paths.insert(declared(&path)?);
        }
    }
    Ok(paths)
}

/// Record references named explicitly by `problem.toml` fields or `tests.toml`
/// entries. Paths that only a resolved default names (such as the template
/// solution) are not a record list, so `[files] created` may name them.
fn explicit_references(dir: &Path, document: &DocumentMut) -> BTreeSet<PathBuf> {
    let mut paths = declarations(document, None).unwrap_or_default();
    if let Ok(catalog) = TestStore::new(dir.into()).catalog() {
        for test in catalog.entries {
            paths.extend(declared(&test.input).ok());
            if let Answer::Named(path) = test.answer {
                paths.extend(declared(&path).ok());
            }
        }
    }
    paths
}

/// Invalid entries do not establish ownership. Overlaps between record lists
/// (explicit references, `created`, `user`) are diagnostic only; registered
/// paths, then created paths, then adopted paths take precedence.
pub(crate) fn provenance(
    dir: &Path,
    document: &DocumentMut,
    registered: &BTreeSet<PathBuf>,
    diagnostics: &mut Vec<String>,
) -> BTreeSet<PathBuf> {
    let mut owned = registered.clone();
    let mut seen: BTreeMap<String, &'static str> = explicit_references(dir, document)
        .iter()
        .map(|p| (path_key(p), "registered"))
        .collect();
    let Some(files) = document.get("files") else {
        return owned;
    };
    if !files.is_table_like() {
        diagnostics.push("invalid [files]: expected a table".into());
        return owned;
    }
    for key in ["created", "user"] {
        let Some(item) = files.get(key) else {
            continue;
        };
        let Some(array) = item.as_array() else {
            diagnostics.push(format!("invalid [files] {key}: expected an array"));
            continue;
        };
        for entry in array {
            let text = if key == "created" {
                entry.as_inline_table().and_then(|t| {
                    t.get("command")
                        .and_then(toml_edit::Value::as_str)
                        .filter(|s| !s.is_empty())?;
                    t.get("path").and_then(toml_edit::Value::as_str)
                })
            } else {
                entry.as_str()
            };
            let path = text
                .ok_or_else(|| "expected a path (and command for created files)".to_owned())
                .and_then(relative);
            match path {
                Ok(path) => {
                    if let Some(previous) = seen.insert(path_key(&path), key) {
                        diagnostics.push(format!(
                            "provenance overlap: {} in {previous} and {key}",
                            path.display()
                        ));
                    }
                    match validate_file(dir, text.expect("parsed path")) {
                        Ok(path) => {
                            owned.insert(path);
                        }
                        Err(error) => {
                            diagnostics.push(format!("invalid [files] {key} entry: {error}"))
                        }
                    }
                }
                Err(error) => diagnostics.push(format!("invalid [files] {key} entry: {error}")),
            }
        }
    }
    owned
}
