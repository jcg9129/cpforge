//! Read-only configuration and host inspection. No storage admission or hooks.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use crate::cli::ConfigCommand;
use crate::config::{
    self, ComposedTarget, ConfigRequest, EffectiveNode, EffectiveValue, Environment, LoadedConfig,
    Origin, Provenance, RawValue, StageHistory, TypedValue,
};
use crate::error::{Error, FailureClass};
use crate::exec::{self, ToolStatus};
use crate::exit::ExitCode;
use crate::report::wire::{self, ConfigValue as Value};
use crate::{selection, transport};

struct Inputs {
    request: ConfigRequest,
    selected: Option<selection::Selected>,
    selection_diagnostic: Option<config::Diagnostic>,
}

// A unique directory with bad metadata still identifies the file to validate.
// Keep the ordinary resolver's collision/link/spelling decisions authoritative.
fn invalid_problem_file(error: &selection::SelectionError) -> Option<PathBuf> {
    match error {
        selection::SelectionError::Context { cause, .. } => invalid_problem_file(cause),
        selection::SelectionError::Collision {
            reason:
                crate::names::CollisionReason::MetadataMissing
                | crate::names::CollisionReason::MetadataUnreadable
                | crate::names::CollisionReason::MetadataMalformed
                | crate::names::CollisionReason::MetadataTooNew
                | crate::names::CollisionReason::IdentityMismatch,
            entries,
            ..
        } => match entries.as_slice() {
            [directory] => Some(directory.join("problem.toml")),
            _ => None,
        },
        _ => None,
    }
}

fn request(
    problem: Option<&str>,
    config_path: Option<PathBuf>,
    profile: Option<String>,
    paths_only: bool,
) -> Result<Inputs, Error> {
    let cwd =
        std::env::current_dir().map_err(|e| Error::Config(format!("current directory: {e}")))?;
    let root = match crate::workspace::find_optional_root(&cwd) {
        Ok(root) => root,
        Err(error) if paths_only => {
            eprintln!("cpf: warning: {error}");
            None
        }
        Err(error) => return Err(Error::Config(error.to_string())),
    };
    let mut selection_diagnostic = None;
    let mut problem_file = None;
    let selected = if let Some(root) = &root {
        match selection::resolve(root, &cwd, problem) {
            Ok(selected) => Some(selected),
            Err(selection::SelectionError::NoSelection) if problem.is_none() => None,
            Err(error) => {
                problem_file = invalid_problem_file(&error);
                if paths_only {
                    eprintln!("cpf: warning: problem selection unavailable: {error}");
                } else {
                    selection_diagnostic = Some(config::Diagnostic {
                        severity: config::Severity::Error,
                        message: error.to_string(),
                        origin: Origin {
                            file: problem_file
                                .clone()
                                .unwrap_or_else(|| PathBuf::from("<selection>")),
                            line: None,
                            col: None,
                            key_path: "selection".into(),
                            span: None,
                        },
                        second_origin: None,
                    });
                }
                None
            }
        }
    } else {
        if problem.is_some() && !paths_only {
            selection_diagnostic = Some(config::Diagnostic {
                severity: config::Severity::Error,
                message: "--problem requires a workspace".into(),
                origin: Origin {
                    file: "<selection>".into(),
                    line: None,
                    col: None,
                    key_path: "selection".into(),
                    span: None,
                },
                second_origin: None,
            });
        }
        None
    };
    let mut request = ConfigRequest {
        environment: Environment {
            home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            appdata: std::env::var_os("APPDATA"),
        },
        current_dir: cwd,
        workspace_root: root,
        problem_file,
        config_path,
        platform: config::Platform::host(),
        profile,
        ..ConfigRequest::default()
    };
    if let Some(selected) = &selected {
        request = selected.config_request(request);
    }
    Ok(Inputs {
        request,
        selected,
        selection_diagnostic,
    })
}

/// Stress execution is unavailable, but a problem that declares `io` is
/// refused with its configuration error (exit 7) before that surface refusal
/// and before any admission. Every other outcome, including a missing
/// workspace or an unrelated configuration error, falls through to the
/// ordinary "not available yet" refusal.
pub(crate) fn stress_preflight(
    problem: Option<&str>,
    config_path: Option<PathBuf>,
    profile: Option<String>,
) -> Result<(), Error> {
    let Ok(Inputs { mut request, .. }) = request(problem, config_path, profile, false) else {
        return Ok(());
    };
    if request.problem_file.is_none() {
        return Ok(());
    }
    request.operation = config::Operation::Stress;
    let loaded = config::load_config(&request);
    let io_diagnostic = loaded.diagnostics().iter().any(|d| {
        d.severity == config::Severity::Error
            && (d.origin.key_path == "config.io" || d.origin.key_path.starts_with("config.io."))
    });
    if io_diagnostic {
        return Err(Error::Config(
            config::ConfigErrors {
                diagnostics: loaded.diagnostics().to_vec(),
            }
            .to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn run(
    command: ConfigCommand,
    problem: Option<&str>,
    config_path: Option<PathBuf>,
) -> Result<ExitCode, Error> {
    if !matches!(command, ConfigCommand::Show { json: true, .. }) {
        return run_command(command, problem, config_path);
    }
    // A panic before any machine output still delivers the `error` document
    // (report §5.4); the top-level boundary then reports it and exits 70.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_command(command, problem, config_path)
    })) {
        Ok(result) => result,
        Err(payload) => {
            let _ = transport::Delivery::stdout().escaped_panic("config_show");
            std::panic::resume_unwind(payload)
        }
    }
}

fn run_command(
    command: ConfigCommand,
    problem: Option<&str>,
    config_path: Option<PathBuf>,
) -> Result<ExitCode, Error> {
    let (origin, json, profile) = match &command {
        ConfigCommand::Show {
            origin,
            json,
            profile,
        } => (*origin, *json, profile.clone()),
        _ => (false, false, None),
    };
    let mut delivery = transport::Delivery::stdout();
    if json {
        delivery.preflight()?;
    }
    let Inputs {
        mut request,
        selected,
        selection_diagnostic,
    } = match request(
        problem,
        config_path,
        profile,
        matches!(command, ConfigCommand::Paths),
    ) {
        Ok(inputs) => inputs,
        Err(error) if json => return deliver_error(&mut delivery, &error, &[]),
        Err(error) => return Err(error),
    };
    if matches!(command, ConfigCommand::Paths) {
        return paths(&request);
    }
    // Show inspects the selected target. Check (and doctor) additionally
    // validate every profile's run stage for the selected problem.
    if matches!(command, ConfigCommand::Show { .. }) {
        request.operation = config::Operation::Other;
    }
    let loaded = config::load_config(&request);
    let diagnostics = report_diagnostics(&loaded, &request, selection_diagnostic);
    let invalid = diagnostics
        .iter()
        .any(|d| d.severity == config::Severity::Error);
    let status = if invalid {
        ExitCode::Config
    } else {
        ExitCode::Success
    };
    match command {
        ConfigCommand::Show { .. } => {
            if invalid {
                if json {
                    return deliver_error(
                        &mut delivery,
                        &Error::Config("configuration validation failed".into()),
                        &diagnostics,
                    );
                }
                return Ok(status);
            }
            let tree = effective_tree(&loaded);
            let mut inspection = Inspection::default();
            let value = inspection.node(&tree, &mut Vec::new());
            inspection.histories(&loaded);
            if json {
                let metadata: Vec<(String, Value)> = vec![
                    ("origins".into(), Value::Array(inspection.origins)),
                    ("deferred".into(), Value::Array(inspection.deferred)),
                    ("history".into(), Value::Array(inspection.history)),
                    ("capture_checks".into(), capture_values(&loaded)),
                    (
                        "hooks_disabled_by_config_override".into(),
                        Value::Boolean(loaded.hook_settings().user_file_replaced),
                    ),
                ];
                let bytes =
                    wire::encode_config_inspection(transport::platform(), &value, &metadata)
                        .map_err(|e| Error::Internal(e.to_string()))?;
                delivery.document(&bytes)?;
            } else {
                let mut output = String::new();
                if loaded.hook_settings().user_file_replaced {
                    output.push_str("# hooks disabled for this invocation: --config\n");
                }
                human_node(&tree, &mut Vec::new(), origin, &mut output);
                human_histories(&loaded, origin, &mut output);
                capture_lines(&loaded, &mut output);
                print_human(&output)?;
            }
        }
        ConfigCommand::Check { tools } => {
            if tools && !invalid {
                print_human(&tool_lines(&loaded, &request, selected.as_ref()))?;
            }
        }
        ConfigCommand::Paths => unreachable!(),
    }
    Ok(status)
}

fn report_diagnostics(
    loaded: &LoadedConfig,
    request: &ConfigRequest,
    selection: Option<config::Diagnostic>,
) -> Vec<config::Diagnostic> {
    let mut diagnostics = loaded.diagnostics().to_vec();
    if let Some(selection) = selection {
        if !diagnostics.iter().any(|d| {
            d.severity == config::Severity::Error && d.origin.file == selection.origin.file
        }) {
            diagnostics.push(selection);
        }
    }
    // Trust identity inspection is read-only. Failed canonicalisation never
    // authorizes a hook and is a warning even when hooks are disabled.
    for trusted in loaded.hook_settings().trusted_workspaces.iter().flatten() {
        if let Err(error) = std::fs::canonicalize(&trusted.value) {
            diagnostics.push(config::Diagnostic {
                severity: config::Severity::Warning,
                message: format!(
                    "trusted workspace {} cannot be canonicalised; untrusted: {error}",
                    trusted.value.display()
                ),
                origin: trusted.origin.clone(),
                second_origin: None,
            });
        }
    }
    if loaded.hook_settings().trusted_workspaces.is_some() {
        if let Some(root) = &request.workspace_root {
            if let Err(error) = std::fs::canonicalize(root) {
                diagnostics.push(config::Diagnostic {
                    severity: config::Severity::Warning,
                    message: format!("workspace cannot be canonicalised; untrusted: {error}"),
                    origin: empty("hooks.trusted_workspaces").origin,
                    second_origin: None,
                });
            }
        }
    }
    for diagnostic in &diagnostics {
        eprintln!("cpf: {diagnostic}");
    }
    diagnostics
}

fn deliver_error(
    delivery: &mut transport::Delivery<std::io::Stdout>,
    error: &Error,
    diagnostics: &[config::Diagnostic],
) -> Result<ExitCode, Error> {
    if diagnostics.is_empty() {
        eprintln!("cpf: error: {error}");
    }
    let mut converted = Vec::new();
    for diagnostic in diagnostics {
        for origin in std::iter::once(&diagnostic.origin).chain(diagnostic.second_origin.as_ref()) {
            converted.push(wire::Diagnostic {
                file: real_file(&origin.file),
                key: Some(origin.key_path.clone()),
                line: origin.line.and_then(|n| u32::try_from(n).ok()),
                col: origin.col.and_then(|n| u32::try_from(n).ok()),
                message: diagnostic.to_string(),
            });
        }
    }
    let dropped = converted.len().saturating_sub(64);
    converted.truncate(64);
    let bytes = wire::encode_error(&wire::ErrorDocument {
        platform: transport::platform(),
        command: "config_show".into(),
        class: FailureClass::Config,
        exit: 7,
        error: wire::DocumentError {
            kind: "configuration",
            message: error.to_string(),
            path: None,
        },
        diagnostics: converted,
        diagnostics_dropped: u32::try_from(dropped).unwrap_or(u32::MAX),
    })
    .map_err(|e| Error::Internal(e.to_string()))?;
    delivery.document(&bytes)?;
    Ok(ExitCode::Config)
}

// `<built-in>`, `<selection>` and similar markers name no file.
fn real_file(path: &std::path::Path) -> Option<PathBuf> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.starts_with(b"<") && bytes.ends_with(b">") {
        None
    } else {
        Some(path.to_owned())
    }
}

fn print_human(text: &str) -> Result<(), Error> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| Error::Internal(format!("cannot write inspection: {e}")))
}

fn paths(request: &ConfigRequest) -> Result<ExitCode, Error> {
    let mut output = String::from("built-in: <built-in> (exists, compiled)\n");
    let options = config::DiscoveryOptions {
        config_override: request.config_path.clone(),
        workspace_root: request.workspace_root.clone(),
        problem_file: request.problem_file.clone(),
        current_dir: Some(request.current_dir.clone()),
    };
    let sources = match config::discover(&request.environment, request.platform, &options) {
        Ok(sources) => sources,
        Err(error) => {
            let _ = writeln!(output, "user: <unavailable> ({error})");
            let mut sources = Vec::new();
            if let Some(root) = &request.workspace_root {
                sources.push(config::LayerSource {
                    kind: config::LayerKind::Workspace,
                    file: root.join("cpforge.toml"),
                });
                if let Some(file) = &request.problem_file {
                    sources.push(config::LayerSource {
                        kind: config::LayerKind::Problem,
                        file: file.clone(),
                    });
                }
            }
            sources
        }
    };
    for source in sources {
        let layer = match source.kind {
            config::LayerKind::DefaultUser => "user",
            config::LayerKind::OverrideUser => "user (--config)",
            config::LayerKind::Workspace => "workspace",
            config::LayerKind::Problem => "problem",
            config::LayerKind::BuiltIn => "built-in",
        };
        let state = match std::fs::metadata(&source.file) {
            Ok(_) => "exists".to_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing".to_owned(),
            Err(e) => format!("existence unknown: {e}"),
        };
        let _ = writeln!(output, "{layer}: {} ({state})", source.file.display());
    }
    print_human(&output)?;
    Ok(ExitCode::Success)
}

fn empty(name: &str) -> EffectiveNode {
    EffectiveNode::new(
        name,
        config::NodeKind::Other,
        EffectiveValue::Table(BTreeMap::new()),
        None,
        Provenance::BuiltIn,
        Origin {
            file: "<built-in>".into(),
            line: None,
            col: None,
            key_path: name.into(),
            span: None,
        },
    )
}
fn insert(parent: &mut EffectiveNode, key: &str, mut value: EffectiveNode) {
    value.name = key.into();
    if let EffectiveValue::Table(table) = &mut parent.value {
        table.insert(key.into(), value);
    }
}
fn set_stages(node: &mut EffectiveNode, target: &ComposedTarget) {
    if let Some(build) = &target.build {
        insert(node, "build", build.node.clone());
    } else {
        let absent = target
            .build_history
            .get(&Vec::new())
            .and_then(|events| {
                events
                    .iter()
                    .rev()
                    .filter_map(|event| event.value.as_ref())
                    .find(|value| {
                        matches!(
                            &value.value,
                            EffectiveValue::Scalar(RawValue::Boolean(false))
                        )
                    })
            })
            .map(snapshot_node)
            .or_else(|| {
                node.get("build")
                    .filter(|n| n.boolean() == Some(false))
                    .cloned()
            })
            .unwrap_or_else(|| {
                let mut absent = empty("build");
                absent.value = EffectiveValue::Scalar(RawValue::Boolean(false));
                absent
            });
        insert(node, "build", absent);
    }
    insert(node, "run", target.run.node.clone());
    for (name, setting) in [
        ("sources", target.settings.sources.as_ref()),
        ("support_files", Some(&target.settings.support_files)),
        ("solution_files", Some(&target.settings.solution_files)),
        ("artifacts", target.settings.artifacts.as_ref()),
        ("entry", target.settings.entry.as_ref()),
        ("template", target.settings.template.as_ref()),
        ("compare", target.settings.compare.as_ref()),
    ] {
        if let Some(setting) = setting {
            insert(node, name, setting.clone());
        }
    }
    if node.get("compare").is_none() {
        insert(node, "compare", default_compare());
    }
}
fn default_compare() -> EffectiveNode {
    let mut compare = empty("compare");
    let mut tokens = empty("tokens");
    tokens.value = EffectiveValue::Scalar(RawValue::Boolean(true));
    insert(&mut compare, "tokens", tokens);
    compare
}
fn effective_tree(loaded: &LoadedConfig) -> EffectiveNode {
    let mut root = loaded.merged().root.clone();
    if root.get("compare").is_none() {
        insert(&mut root, "compare", default_compare());
    }
    if root.at(&["workspace", "ignore"]).is_none() {
        let mut workspace = root
            .get("workspace")
            .cloned()
            .unwrap_or_else(|| empty("workspace"));
        let mut ignore = empty("ignore");
        ignore.value = EffectiveValue::Array(Vec::new());
        insert(&mut workspace, "ignore", ignore);
        insert(&mut root, "workspace", workspace);
    }
    let mut hooks = root.get("hooks").cloned().unwrap_or_else(|| empty("hooks"));
    if hooks.get("enabled").is_none() {
        let mut enabled = empty("enabled");
        enabled.value = EffectiveValue::Scalar(RawValue::Boolean(true));
        insert(&mut hooks, "enabled", enabled);
    }
    insert(&mut root, "hooks", hooks);
    let mut languages = loaded
        .merged()
        .at(&["languages"])
        .cloned()
        .unwrap_or_else(|| empty("languages"));
    if let EffectiveValue::Table(table) = &mut languages.value {
        for ((language, profile), target) in &loaded.universe().languages {
            let Some(node) = table.get_mut(language) else {
                continue;
            };
            if let Some(profile) = profile {
                if let EffectiveValue::Table(language) = &mut node.value {
                    if let Some(profiles) = language.get_mut("profiles") {
                        if let EffectiveValue::Table(profiles) = &mut profiles.value {
                            if let Some(profile) = profiles.get_mut(profile) {
                                set_stages(profile, target);
                            }
                        }
                    }
                }
            } else {
                set_stages(node, target);
            }
        }
    }
    insert(&mut root, "languages", languages);
    let mut commands = empty("commands");
    for (name, stage) in &loaded.universe().commands {
        insert(&mut commands, name, stage.node.clone());
    }
    insert(&mut root, "commands", commands);
    if let Some(target) = &loaded.universe().selected {
        let mut node = empty("target");
        set_stages(&mut node, target);
        let mut language = empty("language");
        language.value = EffectiveValue::Scalar(RawValue::String(target.language.clone()));
        if let Ok(plan) = loaded.target() {
            language.provenance = plan.language.provenance;
            language.origin = plan.language.origin;
        }
        insert(&mut node, "language", language);
        if let Some(profile) = &target.profile {
            let mut value = empty("profile");
            value.value = EffectiveValue::Scalar(RawValue::String(profile.clone()));
            value.provenance = Provenance::Cli {
                flag: "--profile".into(),
            };
            insert(&mut node, "profile", value);
        }
        insert(&mut root, "target", node);
    }
    if let Ok(retention) = loaded.retention() {
        let mut node = loaded
            .merged()
            .at(&["retention"])
            .cloned()
            .unwrap_or_else(|| empty("retention"));
        for (key, value) in [
            ("keep_jobs", retention.keep_jobs),
            ("job_bytes", retention.job_bytes),
            ("workspace_bytes", retention.workspace_bytes),
            ("cwd_bytes", retention.cwd_bytes),
            ("build_bytes", retention.build_bytes),
        ] {
            if node.get(key).is_none() {
                let mut child = empty(key);
                child.value = EffectiveValue::Scalar(RawValue::Integer(
                    i64::try_from(value.value).expect("default fits i64"),
                ));
                child.provenance = value.provenance;
                child.origin = value.origin;
                insert(&mut node, key, child);
            }
        }
        insert(&mut root, "retention", node);
    }
    root
}

fn table(fields: Vec<(&str, Value)>) -> Value {
    Value::Table(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn string(value: impl Into<String>) -> Value {
    Value::String(value.into())
}
fn key_path(path: &[String]) -> Value {
    Value::Array(path.iter().cloned().map(Value::String).collect())
}
fn provenance(value: &Provenance) -> Value {
    match value {
        Provenance::BuiltIn => table(vec![("kind", string("built_in"))]),
        Provenance::File { path, line, col } => {
            let mut fields = vec![
                ("kind".into(), string("file")),
                ("path".into(), Value::Native(path.as_os_str().to_owned())),
            ];
            if let Some(line) = line {
                fields.push((
                    "line".into(),
                    Value::U32(u32::try_from(*line).unwrap_or(u32::MAX)),
                ));
            }
            if let Some(col) = col {
                fields.push((
                    "col".into(),
                    Value::U32(u32::try_from(*col).unwrap_or(u32::MAX)),
                ));
            }
            Value::Table(fields)
        }
        Provenance::Cli { flag } => table(vec![("kind", string("cli")), ("flag", string(flag))]),
        Provenance::Environment { name } => table(vec![
            ("kind", string("environment")),
            ("name", string(name)),
        ]),
        Provenance::Derived { inputs } => table(vec![
            ("kind", string("derived")),
            (
                "inputs",
                Value::Array(inputs.iter().map(provenance).collect()),
            ),
        ]),
    }
}

#[derive(Default)]
struct Inspection {
    origins: Vec<Value>,
    deferred: Vec<Value>,
    history: Vec<Value>,
}
impl Inspection {
    fn node(&mut self, node: &EffectiveNode, path: &mut Vec<String>) -> Value {
        if !matches!(node.value, EffectiveValue::Table(_)) {
            let mut fields = vec![
                ("path".into(), key_path(path)),
                ("origin".into(), provenance(&node.provenance)),
            ];
            if let Some(overlay) = &node.overlay {
                fields.push(("overlay".into(), string(overlay)));
            }
            self.origins.push(Value::Table(fields));
        }
        if path.last().is_some_and(|p| p == "selector") && node.string() == Some("run") {
            self.deferred.push(table(vec![
                ("path", key_path(path)),
                ("placeholder", string("run_cwd")),
                ("origin", provenance(&node.provenance)),
            ]));
        }
        if let Some(TypedValue::Text(text)) = &node.typed {
            for token in &text.tokens {
                if let config::Token::Placeholder(placeholder) = token {
                    self.deferred.push(table(vec![
                        ("path", key_path(path)),
                        ("placeholder", string(placeholder.name())),
                        ("origin", provenance(&node.provenance)),
                    ]));
                }
            }
        }
        match &node.value {
            EffectiveValue::Table(values) => Value::Table(
                values
                    .iter()
                    .map(|(key, child)| {
                        let key = if path.last().is_some_and(|p| p == "env") {
                            child.name.clone()
                        } else {
                            key.clone()
                        };
                        path.push(key.clone());
                        let value = self.node(child, path);
                        path.pop();
                        (key, value)
                    })
                    .collect(),
            ),
            EffectiveValue::Array(values) => Value::Array(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, child)| {
                        path.push(index.to_string());
                        let value = self.node(child, path);
                        path.pop();
                        value
                    })
                    .collect(),
            ),
            EffectiveValue::Scalar(raw) => {
                if let Some(TypedValue::Path(path)) = &node.typed {
                    return Value::Native(path.as_os_str().to_owned());
                }
                if let Some(TypedValue::Text(text)) = &node.typed {
                    if let Some(path) = &text.path {
                        return Value::Native(path.as_os_str().to_owned());
                    }
                }
                match raw {
                    RawValue::String(value) if native_value(path) => {
                        Value::Native(value.as_str().into())
                    }
                    RawValue::String(value) => string(value),
                    RawValue::Integer(value) => Value::Integer(*value),
                    RawValue::Float(value) => Value::Float(*value),
                    RawValue::Boolean(value) => Value::Boolean(*value),
                    _ => unreachable!("validated scalar"),
                }
            }
        }
    }
    fn history(&mut self, prefix: &[String], history: &StageHistory) {
        for (path, events) in history {
            let path: Vec<_> = prefix.iter().chain(path).cloned().collect();
            for event in events {
                let snapshot = |v: &config::ValueSnapshot| {
                    table(vec![
                        ("origin", provenance(&v.provenance)),
                        (
                            "value",
                            Inspection::default().node(&snapshot_node(v), &mut path.clone()),
                        ),
                    ])
                };
                self.history.push(table(vec![
                    ("path", key_path(&path)),
                    ("event", string(event_name(event.kind))),
                    ("origin", provenance(&event.provenance)),
                    (
                        "values",
                        Value::Array(event.value.iter().map(snapshot).collect()),
                    ),
                    (
                        "removed",
                        Value::Array(event.removed.iter().map(snapshot).collect()),
                    ),
                ]));
            }
        }
    }
    fn histories(&mut self, loaded: &LoadedConfig) {
        self.history(&[], &loaded.merged().history);
        for (name, target) in targets(loaded) {
            let prefix: Vec<String> = name.split('.').map(str::to_owned).collect();
            if let Some(build) = &target.build {
                let mut path = prefix.clone();
                path.push("build".into());
                self.history(&path, &build.history);
            }
            let mut path = prefix.clone();
            path.push("run".into());
            self.history(&path, &target.run.history);
            self.history(&prefix, &target.build_history);
            self.history(&prefix, &target.ignored);
        }
        for (name, command) in &loaded.universe().commands {
            self.history(&["commands".into(), name.clone()], &command.history);
        }
    }
}
fn snapshot_node(value: &config::ValueSnapshot) -> EffectiveNode {
    let mut node = EffectiveNode::new(
        &value.name,
        config::NodeKind::Other,
        value.value.clone(),
        value.typed.clone(),
        value.provenance.clone(),
        value.origin.clone(),
    );
    node.overlay = value.overlay.clone();
    node
}

fn event_name(kind: config::EventKind) -> &'static str {
    match kind {
        config::EventKind::Set => "set",
        config::EventKind::Replace => "replace",
        config::EventKind::Delete => "delete",
        config::EventKind::Reset => "reset",
        config::EventKind::Discarded => "discarded",
        config::EventKind::Ignored => "ignored",
    }
}
fn native_value(path: &[String]) -> bool {
    let Some((leaf, parents)) = path.split_last() else {
        return false;
    };
    let parent = parents.last().map(String::as_str);
    match parent {
        Some("env") => true,
        Some(
            "sources" | "support_files" | "solution_files" | "assets" | "env_remove"
            | "trusted_workspaces",
        ) => leaf.parse::<usize>().is_ok(),
        Some("required") if parents.iter().any(|p| p == "artifacts") => {
            leaf.parse::<usize>().is_ok()
        }
        Some("argv" | "interpreter") => leaf == "0",
        Some("cwd" | "template") => leaf == "path",
        Some("artifacts") => leaf == "file",
        _ => leaf == "program" && parents.first().is_some_and(|p| p == "check"),
    }
}

fn origin_text(node: &EffectiveNode) -> String {
    let mut value = match &node.provenance {
        Provenance::BuiltIn => "built-in".into(),
        Provenance::Derived { inputs } => format!("derived from {inputs:?}"),
        Provenance::Cli { flag } => format!("CLI {flag}"),
        Provenance::Environment { name } => format!("environment {name}"),
        Provenance::File { .. } => node.origin.to_string(),
    };
    if let Some(overlay) = &node.overlay {
        let _ = write!(value, "; overlay {overlay}");
    }
    value
}
fn human_node(node: &EffectiveNode, path: &mut Vec<String>, origin: bool, output: &mut String) {
    if let EffectiveValue::Table(table) = &node.value {
        if table.is_empty() && !path.is_empty() {
            let key = path
                .iter()
                .map(|p| toml_edit::Key::new(p.as_str()).to_string())
                .collect::<Vec<_>>()
                .join(".");
            let _ = writeln!(output, "{key} = {{}}");
        }
        for (key, child) in table {
            path.push(if path.last().is_some_and(|p| p == "env") {
                child.name.clone()
            } else {
                key.clone()
            });
            human_node(child, path, origin, output);
            path.pop();
        }
        return;
    }
    let key = path
        .iter()
        .map(|p| toml_edit::Key::new(p.as_str()).to_string())
        .collect::<Vec<_>>()
        .join(".");
    let _ = write!(output, "{key} = {}", human_value(node));
    let mut notes = Vec::new();
    collect_deferred(node, &mut notes);
    if origin {
        notes.push(origin_text(node));
    }
    if !notes.is_empty() {
        let _ = write!(output, " # {}", notes.join("; ").replace(['\n', '\r'], " "));
    }
    output.push('\n');
    if origin {
        if let EffectiveValue::Array(children) = &node.value {
            for (index, child) in children.iter().enumerate() {
                let _ = writeln!(
                    output,
                    "# {key}[{index}]: {}",
                    origin_text(child).replace(['\n', '\r'], " ")
                );
            }
        }
    }
}
fn human_value(node: &EffectiveNode) -> String {
    if let Some(TypedValue::Path(path)) = &node.typed {
        return format!("{:?}", path.as_os_str());
    }
    if let Some(TypedValue::Text(text)) = &node.typed {
        if let Some(path) = &text.path {
            return format!("{:?}", path.as_os_str());
        }
    }
    match &node.value {
        EffectiveValue::Array(children) => format!(
            "[{}]",
            children
                .iter()
                .map(human_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        EffectiveValue::Scalar(RawValue::String(value)) => {
            toml_edit::Value::from(value.as_str()).to_string()
        }
        EffectiveValue::Scalar(RawValue::Integer(value)) => value.to_string(),
        EffectiveValue::Scalar(RawValue::Float(value)) => value.to_string(),
        EffectiveValue::Scalar(RawValue::Boolean(value)) => value.to_string(),
        EffectiveValue::Table(values) => format!(
            "{{ {} }}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{} = {}",
                    toml_edit::Key::new(key.as_str()),
                    human_value(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => unreachable!("validated scalar"),
    }
}
fn collect_deferred(node: &EffectiveNode, notes: &mut Vec<String>) {
    if node.name == "selector" && node.string() == Some("run") {
        notes.push("deferred fresh run directory".into());
    }
    if let Some(TypedValue::Text(text)) = &node.typed {
        for token in &text.tokens {
            if let config::Token::Placeholder(p) = token {
                notes.push(format!("deferred {{{}}}", p.name()));
            }
        }
    }
    if let EffectiveValue::Array(children) = &node.value {
        for child in children {
            collect_deferred(child, notes);
        }
    }
}
fn human_histories(loaded: &LoadedConfig, origin: bool, output: &mut String) {
    let mut histories = vec![(String::new(), &loaded.merged().history)];
    for (name, target) in targets(loaded) {
        if let Some(build) = &target.build {
            histories.push((format!("{name}.build"), &build.history));
        }
        histories.push((format!("{name}.run"), &target.run.history));
        histories.push((name.clone(), &target.build_history));
        histories.push((name, &target.ignored));
    }
    for (name, stage) in &loaded.universe().commands {
        histories.push((format!("commands.{name}"), &stage.history));
    }
    for (prefix, history) in histories {
        for (path, events) in history {
            for event in events {
                if !origin && event.kind != config::EventKind::Ignored {
                    continue;
                }
                let key = match (prefix.is_empty(), path.is_empty()) {
                    (true, _) => path.join("."),
                    (false, true) => prefix.clone(),
                    (false, false) => format!("{prefix}.{}", path.join(".")),
                };
                let _ = writeln!(
                    output,
                    "# history {key}: {} at {}",
                    event_name(event.kind),
                    event.origin
                );
                for removed in &event.removed {
                    let _ = writeln!(
                        output,
                        "# removed: {} at {}",
                        human_value(&snapshot_node(removed)),
                        removed.origin
                    );
                }
            }
        }
    }
}

fn targets(loaded: &LoadedConfig) -> Vec<(String, &ComposedTarget)> {
    let mut targets: Vec<_> = loaded
        .universe()
        .languages
        .iter()
        .map(|((language, profile), target)| {
            (
                profile.as_ref().map_or_else(
                    || format!("languages.{language}"),
                    |p| format!("languages.{language}.profiles.{p}"),
                ),
                target,
            )
        })
        .collect();
    if let Some(target) = &loaded.universe().selected {
        targets.push(("target".into(), target));
    }
    targets
}
// The source-consuming stage is the build, or the run of a build-free target.
fn planned_check(target: &ComposedTarget) -> crate::jobs::guard::CaptureCheck {
    crate::jobs::guard::planned_capture_check(target.build.as_ref().unwrap_or(&target.run))
}
fn capture_name(target: &ComposedTarget) -> &'static str {
    match planned_check(target) {
        crate::jobs::guard::CaptureCheck::Checked => "checked",
        crate::jobs::guard::CaptureCheck::Unchecked { .. } => "unchecked{shell_stage}",
    }
}
// The job report's tagged form (report §3.6), so readers share one decoder.
fn capture_value(target: &ComposedTarget) -> Value {
    match planned_check(target) {
        crate::jobs::guard::CaptureCheck::Checked => table(vec![("kind", string("checked"))]),
        crate::jobs::guard::CaptureCheck::Unchecked { reasons } => table(vec![
            ("kind", string("unchecked")),
            (
                "reasons",
                Value::Array(
                    reasons
                        .iter()
                        .map(|reason| match reason {
                            crate::jobs::guard::CaptureCheckReason::ShellStage => {
                                string("shell_stage")
                            }
                        })
                        .collect(),
                ),
            ),
        ]),
    }
}
fn capture_values(loaded: &LoadedConfig) -> Value {
    Value::Array(
        targets(loaded)
            .into_iter()
            .map(|(name, target)| {
                table(vec![
                    ("target", string(name)),
                    (
                        "stage",
                        string(if target.build.is_some() {
                            "build"
                        } else {
                            "run"
                        }),
                    ),
                    ("check", capture_value(target)),
                    ("scope", string("plan_time")),
                    ("guard", string("deferred_until_phase_3")),
                ])
            })
            .collect(),
    )
}
fn capture_lines(loaded: &LoadedConfig, output: &mut String) {
    for (name, target) in targets(loaded) {
        let stage = if target.build.is_some() {
            "build"
        } else {
            "run"
        };
        let _ = writeln!(
            output,
            "# {name}.{stage}: capture check {} (plan-time; operand guard deferred until phase 3)",
            capture_name(target)
        );
    }
}
fn tool_lines(
    loaded: &LoadedConfig,
    request: &ConfigRequest,
    selected: Option<&selection::Selected>,
) -> String {
    let baseline = exec::Baseline::capture();
    let identity = selected.map(|s| s.identity.to_string());
    let problem = selected
        .zip(identity.as_deref())
        .map(|(s, id)| (s.dir.as_path(), id));
    let mut output = String::new();
    if let Ok(stages) = loaded.inspection_stages() {
        for inspection in stages {
            let config::api::InspectionStage { name, stage, entry } = inspection;
            let status = exec::inspect_program(
                &stage,
                &request.current_dir,
                request.workspace_root.as_deref(),
                problem,
                entry.as_deref(),
                &baseline,
                &exec::NativeProbe,
                request.platform,
            );
            let text = match status {
                ToolStatus::Found(path) => format!("found {}", path.display()),
                ToolStatus::Generated => "deferred (generated)".into(),
                ToolStatus::Unavailable => "not checkable here".into(),
                ToolStatus::NotFound(reason) => format!("not found ({reason})"),
            };
            let _ = writeln!(output, "{name}: {text}");
        }
    }
    output
}

pub(crate) fn doctor(
    problem: Option<&str>,
    config_path: Option<PathBuf>,
) -> Result<ExitCode, Error> {
    let Inputs {
        request,
        selected,
        selection_diagnostic,
    } = request(problem, config_path, None, false)?;
    let loaded = config::load_config(&request);
    let diagnostics = report_diagnostics(&loaded, &request, selection_diagnostic);
    let invalid = diagnostics
        .iter()
        .any(|d| d.severity == config::Severity::Error);
    let mut output = format!(
        "configuration: {} (phases 1–2)\n",
        if invalid { "invalid" } else { "valid" }
    );
    if !invalid {
        output.push_str(&tool_lines(&loaded, &request, selected.as_ref()));
    }
    let supported = cfg!(any(target_os = "linux", target_os = "macos"));
    let _ = writeln!(
        output,
        "supervisor: {} (compiled support)",
        if supported {
            "supported"
        } else {
            "unavailable"
        }
    );
    output.push_str("supervisor runtime launch probe: not performed (no child launched)\n");
    let capabilities = crate::supervisor::capabilities();
    for capability in capabilities.metrics {
        let method = match capability.kind {
            _ if !capability.compiled => "unavailable on this platform",
            crate::supervisor::MetricKind::PeakRss if cfg!(target_os = "linux") => {
                "wait4 ru_maxrss ×1024; root and waited descendants; max of processes"
            }
            crate::supervisor::MetricKind::PeakRss => {
                "wait4 ru_maxrss bytes; root and waited descendants; max of processes"
            }
            crate::supervisor::MetricKind::CpuTime => {
                "wait4 ru_utime + ru_stime; root and waited descendants; sum"
            }
            _ => "unavailable on this platform",
        };
        let _ = writeln!(
            output,
            "measurement {:?}: compiled={}; probe={}; method={method}",
            capability.kind,
            capability.compiled,
            probe_text(&capability.probe)
        );
    }
    for capability in capabilities.limits {
        let method = match capability.kind {
            crate::supervisor::LimitKind::AddressSpace if capability.compiled => {
                "RLIMIT_AS; page-rounded; checked at each run"
            }
            _ => "unavailable on this platform",
        };
        let _ = writeln!(
            output,
            "limit {:?}: compiled={}; probe={}; method={method}",
            capability.kind,
            capability.compiled,
            probe_text(&capability.probe)
        );
    }
    for capability in capabilities.observers {
        let _ = writeln!(
            output,
            "observer {}: compiled={}; probe={}",
            capability.kind,
            capability.compiled,
            probe_text(&capability.probe)
        );
    }
    let connected = capabilities.connected_runs;
    let _ = writeln!(
        output,
        "{}: compiled={}; probe={}",
        connected.kind,
        connected.compiled,
        probe_text(&connected.probe)
    );
    let cleanup = if cfg!(target_os = "linux") {
        "procfs pgrp scan using pidfd fdinfo Pid; supported unfiltered view required"
    } else if cfg!(target_os = "macos") {
        "libproc process-group scan with identity checks"
    } else {
        "unavailable"
    };
    let _ = writeln!(output, "cleanup verification: {cleanup}");
    output.push_str(
        "probes are informative only; support and limit installation are checked for each run\n",
    );
    capture_lines(&loaded, &mut output);
    let _ = writeln!(
        output,
        "collision key Unicode version: {}",
        crate::names::UNICODE_VERSION
    );
    for capability in capabilities.deferred {
        let _ = writeln!(output, "{}: unavailable feature", capability.kind);
    }
    print_human(&output)?;
    Ok(if invalid {
        ExitCode::Config
    } else {
        ExitCode::Success
    })
}
fn probe_text(probe: &Result<(), String>) -> &str {
    match probe {
        Ok(()) => "available",
        Err(reason) => reason.as_str(),
    }
}
