use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{ImDocument, Item, TableLike, Value};

use super::syntax;
use super::{
    Diagnostic, LayerKind, LayerState, Origin, Platform, RawLayer, RawNode, RawValue, Severity,
};
use crate::storage::{self, MalformedRecord, Record};

/// Only the environment used for discovery and home expansion; never reads the
/// process environment implicitly. Native path spellings are preserved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<OsString>,
    pub appdata: Option<OsString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerSource {
    pub kind: LayerKind,
    pub file: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveryOptions {
    pub config_override: Option<PathBuf>,
    /// Already discovered/canonicalised under storage §2; no ancestor walk here.
    pub workspace_root: Option<PathBuf>,
    pub problem_file: Option<PathBuf>,
    /// The process's current directory, against which a relative
    /// `config_override` is joined lexically (§4.2: a path in a CLI flag is
    /// relative to the current directory).
    pub current_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryError(pub String);

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DiscoveryError {}

pub fn discover(
    env: &Environment,
    platform: Platform,
    options: &DiscoveryOptions,
) -> Result<Vec<LayerSource>, DiscoveryError> {
    // `platform` decides the discovery rules (XDG vs APPDATA) and the
    // separator; callers outside tests pass the host platform, since the
    // result names real files.
    let (kind, file) = if let Some(file) = &options.config_override {
        let file = if syntax::native_absolute(file.as_os_str(), platform) {
            file.clone()
        } else {
            let cwd = options.current_dir.as_deref().ok_or_else(|| {
                DiscoveryError(
                    "cannot resolve a relative --config path without the current directory".into(),
                )
            })?;
            syntax::join(cwd, file.as_os_str(), platform)
        };
        (LayerKind::OverrideUser, file)
    } else {
        let usable = |p: &OsStr| !p.is_empty() && syntax::native_absolute(p, platform);
        let base = if platform == Platform::Windows {
            env.appdata
                .as_deref()
                .filter(|p| usable(p))
                .map(PathBuf::from)
        } else {
            env.xdg_config_home
                .as_deref()
                .filter(|p| usable(p))
                .map(PathBuf::from)
                .or_else(|| {
                    env.home
                        .as_deref()
                        .filter(|p| usable(p.as_os_str()))
                        .map(|p| syntax::join(p, OsStr::new(".config"), platform))
                })
        }
        .ok_or_else(|| {
            DiscoveryError(if platform == Platform::Windows {
                "cannot discover user configuration: APPDATA must be non-empty and absolute".into()
            } else {
                "cannot discover user configuration: no absolute XDG_CONFIG_HOME or HOME".into()
            })
        })?;
        let dir = syntax::join(&base, OsStr::new("cpforge"), platform);
        (
            LayerKind::DefaultUser,
            syntax::join(&dir, OsStr::new("config.toml"), platform),
        )
    };
    let mut sources = vec![LayerSource { kind, file }];
    if let Some(root) = &options.workspace_root {
        sources.push(LayerSource {
            kind: LayerKind::Workspace,
            file: syntax::join(root, OsStr::new("cpforge.toml"), platform),
        });
        if let Some(file) = &options.problem_file {
            sources.push(LayerSource {
                kind: LayerKind::Problem,
                file: file.clone(),
            });
        }
    }
    Ok(sources)
}

/// Load every requested file, retaining independent failures. No default file
/// is consulted here: `discover` chooses exactly one user source.
pub fn load(sources: &[LayerSource], env: &Environment, platform: Platform) -> Vec<RawLayer> {
    sources
        .iter()
        .map(|source| match storage::read_record_bytes(&source.file) {
            storage::RecordBytes::Present(bytes) => parse(&bytes, source, env, platform),
            storage::RecordBytes::TooLarge => finish(
                Record::Malformed(MalformedRecord::TooLarge {
                    limit: storage::RECORD_SIZE_LIMIT,
                }),
                source,
                env,
                platform,
                None,
            ),
            storage::RecordBytes::Missing => finish(Record::Missing, source, env, platform, None),
            storage::RecordBytes::Unreadable(e) => {
                finish(Record::Unreadable(e), source, env, platform, None)
            }
        })
        .collect()
}

/// In-memory counterpart of `load`, using the same user-record classifier.
pub fn parse(
    bytes: &[u8],
    source: &LayerSource,
    env: &Environment,
    platform: Platform,
) -> RawLayer {
    let record =
        storage::classify_user_record_spanned(bytes, 1, |document| decode(document, source));
    finish(
        record,
        source,
        env,
        platform,
        std::str::from_utf8(bytes).ok(),
    )
}

/// Raw, independently validated problem-record surfaces.
/// Judging composition and execution support are separate from this raw parser.
#[derive(Clone, Debug, Default)]
pub struct ProblemTables {
    pub check: Option<RawNode>,
    pub stress: Option<RawNode>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Validate only the record-owned tables, without composing or enabling roles.
/// `include_stress` is false for consumers to which stress does not apply.
pub fn parse_problem_tables(
    bytes: &[u8],
    source: &LayerSource,
    env: &Environment,
    platform: Platform,
    include_stress: bool,
) -> ProblemTables {
    assert_eq!(
        source.kind,
        LayerKind::Problem,
        "judging comes only from local problem configuration"
    );
    let record =
        storage::classify_user_record_spanned(bytes, 1, |document| decode(document, source));
    let Record::Valid(mut root) = record else {
        let layer = finish(
            record,
            source,
            env,
            platform,
            std::str::from_utf8(bytes).ok(),
        );
        return ProblemTables {
            diagnostics: layer.diagnostics,
            ..ProblemTables::default()
        };
    };
    let mut result = ProblemTables::default();
    let mut validator = super::validate::Validator::new(env, platform, source.kind);
    if let RawValue::Table(entries) = &mut root.value {
        result.check = entries.remove("check");
        if include_stress {
            result.stress = entries.remove("stress");
        }
    }
    if let Some(check) = &mut result.check {
        validator.record_table(check, false);
    }
    if let Some(stress) = &mut result.stress {
        validator.record_table(stress, true);
    }
    result.diagnostics = validator.diagnostics;
    result.diagnostics.sort_by(|a, b| {
        (&a.origin.key_path, a.origin.line, a.origin.col).cmp(&(
            &b.origin.key_path,
            b.origin.line,
            b.origin.col,
        ))
    });
    result
}

fn decode(document: &ImDocument<String>, source: &LayerSource) -> Result<RawNode, Box<Diagnostic>> {
    let text = document.raw();
    let root = table(
        document.as_table(),
        origin(&source.file, text, "", Some(0..text.len())),
        text,
    );
    if source.kind == LayerKind::Problem {
        let id = root.get("problem").and_then(|p| p.get("id"));
        if id
            .and_then(RawNode::string)
            .is_none_or(|s| crate::names::ProblemIdentity::parse(s).is_err())
        {
            return Err(Box::new(Diagnostic {
                severity: Severity::Error,
                message: "problem.id must be a valid problem identity".into(),
                origin: id.map_or_else(
                    || Origin {
                        key_path: "problem.id".into(),
                        ..root.origin.clone()
                    },
                    |n| n.origin.clone(),
                ),
                second_origin: None,
            }));
        }
    }
    Ok(root)
}

fn finish(
    record: Record<RawNode, Box<Diagnostic>>,
    source: &LayerSource,
    env: &Environment,
    platform: Platform,
    text: Option<&str>,
) -> RawLayer {
    let unknown = Origin {
        file: source.file.clone(),
        line: None,
        col: None,
        key_path: String::new(),
        span: None,
    };
    let mut result = RawLayer {
        kind: source.kind,
        platform,
        file: source.file.clone(),
        state: LayerState::Empty,
        problem_check: None,
        problem_kind: None,
        diagnostics: vec![],
    };
    let mut diagnostic = Diagnostic {
        severity: Severity::Error,
        message: String::new(),
        origin: unknown.clone(),
        second_origin: None,
    };
    match record {
        Record::Missing if source.kind == LayerKind::DefaultUser => return result,
        Record::Missing => diagnostic.message = "configuration file is absent".into(),
        Record::Unreadable(e) => diagnostic.message = format!("cannot read configuration: {e}"),
        Record::TooNew { schema, supported } => {
            diagnostic.message = format!("schema {schema} is newer than supported {supported}");
            diagnostic.origin = schema_origin(&source.file, text);
        }
        Record::Malformed(MalformedRecord::Fields(d)) => diagnostic = *d,
        Record::Malformed(MalformedRecord::Toml(e)) => {
            diagnostic.message = format!("invalid TOML: {}", e.message());
            diagnostic.origin.span = e.span();
            if let Some(text) = text {
                diagnostic.origin = origin(&source.file, text, "", e.span());
            }
        }
        Record::Malformed(e) => {
            diagnostic.message = format!("invalid configuration record: {e}");
            if !matches!(e, MalformedRecord::Encoding(_)) {
                diagnostic.origin = schema_origin(&source.file, text);
            }
        }
        Record::Valid(mut root) => {
            if source.kind == LayerKind::Problem {
                let RawValue::Table(mut entries) = root.value else {
                    unreachable!()
                };
                result.problem_kind = entries
                    .get("problem")
                    .and_then(|p| p.get("kind"))
                    .cloned()
                    .map(Box::new);
                result.problem_check = entries.remove("check").map(Box::new);
                if let Some(check) = &mut result.problem_check {
                    let mut validator = super::validate::Validator::new(env, platform, source.kind);
                    validator.record_table(check, false);
                    result.diagnostics.extend(validator.diagnostics);
                }
                let Some(config) = entries.remove("config") else {
                    return result;
                };
                root = config;
            }
            let mut validator = super::validate::Validator::new(env, platform, source.kind);
            validator.root(&mut root);
            validator.diagnostics.sort_by(|a, b| {
                (a.origin.line, a.origin.col, &a.origin.key_path).cmp(&(
                    b.origin.line,
                    b.origin.col,
                    &b.origin.key_path,
                ))
            });
            result.diagnostics.extend(validator.diagnostics);
            // `[check]` and `[config]` diagnostics are reported in document order.
            result.diagnostics.sort_by(|a, b| {
                (a.origin.line, a.origin.col, &a.origin.key_path).cmp(&(
                    b.origin.line,
                    b.origin.col,
                    &b.origin.key_path,
                ))
            });
            result.state = LayerState::Loaded(root);
            return result;
        }
    }
    result.state = LayerState::Invalid(unknown);
    result.diagnostics.push(diagnostic);
    result
}

fn schema_origin(file: &Path, text: Option<&str>) -> Origin {
    let span = text
        .and_then(|text| ImDocument::parse(text).ok())
        .and_then(|d| {
            d.as_table()
                .get_key_value("schema")
                .and_then(|(key, _)| key.span())
        });
    origin(file, text.unwrap_or(""), "schema", span)
}

fn origin(file: &Path, text: &str, key_path: &str, span: Option<std::ops::Range<usize>>) -> Origin {
    let position = span.as_ref().map(|s| s.start.min(text.len()));
    Origin {
        file: file.to_owned(),
        line: position.map(|p| text.as_bytes()[..p].iter().filter(|b| **b == b'\n').count() + 1),
        col: position.map(|p| {
            p - text.as_bytes()[..p]
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |n| n + 1)
                + 1
        }),
        key_path: key_path.into(),
        span,
    }
}

fn child_path(parent: &str, key: &str) -> String {
    // TOML key syntax: bare when possible, else a TOML basic string, so env
    // names containing dots stay unambiguous.
    let key = toml_edit::Key::new(key).display_repr().into_owned();
    if parent.is_empty() {
        key
    } else {
        format!("{parent}.{key}")
    }
}

fn table(t: &dyn TableLike, at: Origin, text: &str) -> RawNode {
    let entries: BTreeMap<_, _> = t
        .iter()
        .map(|(key, item)| {
            let key_path = child_path(&at.key_path, key);
            let span = t
                .get_key_value(key)
                .and_then(|(k, _)| k.span())
                .or_else(|| item.span());
            let at = origin(&at.file, text, &key_path, span);
            (key.to_owned(), node(item, at, text))
        })
        .collect();
    RawNode {
        origin: at,
        value: RawValue::Table(entries),
        invalid: false,
        typed: None,
        overlay: None,
        rejected: false,
    }
}

fn node(item: &Item, at: Origin, text: &str) -> RawNode {
    if let Some(t) = item.as_table_like() {
        return table(t, at, text);
    }
    if let Some(array) = item.as_array_of_tables() {
        let value = RawValue::Array(
            array
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    table(
                        t,
                        origin(&at.file, text, &format!("{}[{i}]", at.key_path), t.span()),
                        text,
                    )
                })
                .collect(),
        );
        return RawNode {
            origin: at,
            value,
            invalid: false,
            typed: None,
            overlay: None,
            rejected: false,
        };
    }
    match item.as_value() {
        Some(v) => value(v, at, text),
        None => unreachable!("parsed TOML contains no empty items"),
    }
}

fn value(v: &Value, at: Origin, text: &str) -> RawNode {
    if let Some(t) = v.as_inline_table() {
        return table(t, at, text);
    }
    let value = match v {
        Value::String(s) => RawValue::String(s.value().clone()),
        Value::Integer(i) => RawValue::Integer(*i.value()),
        Value::Float(f) => RawValue::Float(*f.value()),
        Value::Boolean(b) => RawValue::Boolean(*b.value()),
        Value::Datetime(d) => RawValue::Datetime(d.value().to_string()),
        Value::Array(a) => RawValue::Array(
            a.iter()
                .enumerate()
                .map(|(i, v)| {
                    value(
                        v,
                        origin(&at.file, text, &format!("{}[{i}]", at.key_path), v.span()),
                        text,
                    )
                })
                .collect(),
        ),
        Value::InlineTable(_) => unreachable!(),
    };
    RawNode {
        origin: at,
        value,
        invalid: false,
        typed: None,
        overlay: None,
        rejected: false,
    }
}
