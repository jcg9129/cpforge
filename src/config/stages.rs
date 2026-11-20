use std::collections::{BTreeMap, BTreeSet};

use super::compose::merge_stage_row;
use super::{
    EffectiveNode, EffectiveValue, EventKind, InvalidMarker, MergedConfig, NodeKind, Origin,
    Placeholder, Provenance, ProvenanceEvent, RawValue, ResetMode, Token, TypedValue,
};

pub(super) type KeyPath = Vec<String>;
pub type StageHistory = BTreeMap<Vec<String>, Vec<ProvenanceEvent>>;

/// Which invocation supplies row 6. Other operations ignore `deadline_ms`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Operation {
    #[default]
    Check,
    Test,
    Run,
    Other,
}

/// A finite published limit, already expressed in milliseconds by the problem
/// reader. Absence includes an unpublished or unlimited judge limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedLimit {
    pub milliseconds: u64,
    pub provenance: Provenance,
    pub origin: Origin,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// A language-only target, outside an existing or prospective problem.
    /// For a selected problem, config.language/default_language select instead.
    pub language: Option<String>,
    pub profile: Option<String>,
    /// Also set for a problem whose document has no config table.
    pub problem: bool,
    pub operation: Operation,
    /// Parsed CLI duration; validated against §7.1 before use.
    pub deadline_ms: Option<u64>,
    pub published_time_limit: Option<PublishedLimit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageKind {
    Build,
    Run,
    Command,
    Hook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapPolicy {
    Kill,
    Discard,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContextRequirements {
    pub problem: bool,
    pub workspace: bool,
}

/// An occurrence awaiting phase-3 context and expansion; no paths are allocated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeferredValue {
    pub placeholder: Placeholder,
    pub origin: Origin,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComposedStage {
    pub kind: StageKind,
    pub node: EffectiveNode,
    /// Top-level keys supplied by configuration or CLI, excluding defaults.
    pub explicit_fields: BTreeSet<String>,
    /// Relative paths; includes fields removed by a reset or replacement.
    pub history: StageHistory,
    pub stdout_policy: CapPolicy,
    pub stderr_policy: CapPolicy,
    pub context: ContextRequirements,
    pub deferred: Vec<DeferredValue>,
    pub(super) inputs: Vec<KeyPath>,
}

impl ComposedStage {
    pub fn explicitly_set(&self, key: &str) -> bool {
        self.explicit_fields.contains(key)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TargetSettings {
    pub sources: Option<EffectiveNode>,
    pub support_files: EffectiveNode,
    pub solution_files: EffectiveNode,
    pub artifacts: Option<EffectiveNode>,
    pub entry: Option<EffectiveNode>,
    pub template: Option<EffectiveNode>,
    pub compare: Option<EffectiveNode>,
    pub build_free: bool,
}

/// Declarative, inspectable output; it may still contain phase-1/2 errors.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposedTarget {
    pub language: String,
    pub profile: Option<String>,
    pub build: Option<ComposedStage>,
    /// Also retained when the last existence contribution is a reset.
    pub build_history: StageHistory,
    pub run: ComposedStage,
    pub settings: TargetSettings,
    /// Global/problem build modifiers ignored for a build-free target.
    pub ignored: StageHistory,
}

pub(super) fn path(parts: &[&str]) -> KeyPath {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

pub(super) fn origin(key: &str, source: &str) -> Origin {
    Origin {
        file: source.into(),
        line: None,
        col: None,
        key_path: key.into(),
        span: None,
    }
}

fn scalar(key: &str, value: RawValue, typed: Option<TypedValue>) -> EffectiveNode {
    EffectiveNode::new(
        key,
        NodeKind::Other,
        EffectiveValue::Scalar(value),
        typed,
        Provenance::BuiltIn,
        origin(key, "<built-in>"),
    )
}

fn duration(ms: u64) -> EffectiveNode {
    scalar(
        "deadline_s",
        RawValue::String(format!("{ms}ms")),
        Some(TypedValue::Milliseconds(ms)),
    )
}

fn table(key: &str, kind: NodeKind) -> EffectiveNode {
    EffectiveNode::new(
        key,
        kind,
        EffectiveValue::Table(BTreeMap::new()),
        None,
        Provenance::BuiltIn,
        origin(key, "<built-in>"),
    )
}

fn insert(parent: &mut EffectiveNode, child: EffectiveNode) {
    let EffectiveValue::Table(values) = &mut parent.value else {
        return;
    };
    values.insert(child.name.clone(), child);
}

/// Row-1 defaults (execution contract §5.5). `raw_run` selects the raw
/// `cpf run` caps: 64 MiB on both streams of the run stage.
fn defaults(kind: StageKind, raw_run: bool) -> EffectiveNode {
    let mut node = table("stage", NodeKind::Stage);
    let deadline = match kind {
        StageKind::Run => 10_000,
        StageKind::Hook => 30_000,
        StageKind::Build | StageKind::Command => 60_000,
    };
    insert(&mut node, duration(deadline));
    for key in ["stdout_cap", "stderr_cap"] {
        let bytes = if kind == StageKind::Run && (key == "stdout_cap" || raw_run) {
            64 << 20
        } else {
            1 << 20
        };
        insert(
            &mut node,
            scalar(
                key,
                RawValue::Integer(bytes),
                Some(TypedValue::Bytes(bytes as u64)),
            ),
        );
    }
    let mut cwd = table("cwd", NodeKind::Cwd);
    insert(
        &mut cwd,
        scalar("selector", RawValue::String("run".into()), None),
    );
    insert(&mut node, cwd);
    if matches!(kind, StageKind::Command | StageKind::Hook) {
        for key in ["needs_build", "for_each_test", "keep_going"] {
            insert(&mut node, scalar(key, RawValue::Boolean(false), None));
        }
        insert(
            &mut node,
            scalar("repeat", RawValue::Integer(1), Some(TypedValue::Count(1))),
        );
    }
    node
}

fn node_history(node: &EffectiveNode, prefix: &[String], history: &mut StageHistory) {
    history.insert(prefix.to_vec(), vec![node.event(EventKind::Set)]);
    match &node.value {
        EffectiveValue::Table(t) => {
            for (key, child) in t {
                let mut next = prefix.to_vec();
                next.push(key.clone());
                node_history(child, &next, history);
            }
        }
        EffectiveValue::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                let mut next = prefix.to_vec();
                next.push(i.to_string());
                node_history(child, &next, history);
            }
        }
        EffectiveValue::Scalar(_) => {}
    }
}

fn row_history(merged: &MergedConfig, prefix: &[String]) -> StageHistory {
    merged
        .history
        .iter()
        .filter_map(|(p, events)| {
            p.strip_prefix(prefix)
                .map(|relative| (relative.to_vec(), events.clone()))
        })
        .collect()
}

fn apply_row(
    node: &mut EffectiveNode,
    history: &mut StageHistory,
    merged: &MergedConfig,
    prefix: &[String],
    mode: ResetMode,
) {
    let keys: Vec<_> = prefix.iter().map(String::as_str).collect();
    if let Some(higher) = merged.at(&keys) {
        if history.is_empty() {
            // The first existence contribution has no lower stage to reset.
            *node = higher.clone();
            *history = row_history(merged, prefix);
            return;
        }
        merge_stage_row(
            node,
            history,
            higher.clone(),
            row_history(merged, prefix),
            mode,
        );
    }
}

/// A marker affects only checks that read its field (or its containing node).
/// Rejected keys cannot provide any valid input to a phase-2 check.
pub(super) fn marked(merged: &MergedConfig, paths: &[KeyPath]) -> bool {
    merged.markers.iter().any(|m| {
        m.applies
            && !m.rejected_key
            && paths
                .iter()
                .any(|p| p.starts_with(&m.path) || m.path.starts_with(p))
    })
}

pub(super) fn fields(inputs: &[KeyPath], suffixes: &[&[&str]]) -> Vec<KeyPath> {
    inputs
        .iter()
        .flat_map(|input| {
            suffixes.iter().map(move |suffix| {
                let mut p = input.clone();
                p.extend(suffix.iter().map(|s| (*s).to_owned()));
                p
            })
        })
        .collect()
}

fn annotate(
    node: &mut EffectiveNode,
    relative: &[String],
    history: &StageHistory,
    inputs: &[KeyPath],
    markers: &[InvalidMarker],
) {
    node.events = history.get(relative).cloned().unwrap_or_default();
    node.markers = markers
        .iter()
        .filter(|m| {
            m.applies
                && inputs.iter().any(|input| {
                    let mut p = input.clone();
                    p.extend_from_slice(relative);
                    if m.rejected_key {
                        m.path.len() > p.len() && m.path.starts_with(&p)
                    } else {
                        p.starts_with(&m.path) || m.path.starts_with(&p)
                    }
                })
        })
        .cloned()
        .collect();
    let children: Vec<_> = match &mut node.value {
        EffectiveValue::Table(t) => t.iter_mut().map(|(k, n)| (k.clone(), n)).collect(),
        EffectiveValue::Array(a) => a
            .iter_mut()
            .enumerate()
            .map(|(i, n)| (i.to_string(), n))
            .collect(),
        EffectiveValue::Scalar(_) => vec![],
    };
    for (key, child) in children {
        let mut next = relative.to_vec();
        next.push(key);
        annotate(child, &next, history, inputs, markers);
    }
}

pub(super) fn placeholders(node: &EffectiveNode) -> Vec<DeferredValue> {
    let mut result = vec![];
    if let Some(TypedValue::Text(text)) = &node.typed {
        for token in &text.tokens {
            if let Token::Placeholder(placeholder) = token {
                result.push(DeferredValue {
                    placeholder: *placeholder,
                    origin: node.origin.clone(),
                    provenance: node.provenance.clone(),
                });
            }
        }
    }
    match &node.value {
        EffectiveValue::Table(t) => result.extend(t.values().flat_map(placeholders)),
        EffectiveValue::Array(a) => result.extend(a.iter().flat_map(placeholders)),
        EffectiveValue::Scalar(_) => {}
    }
    result
}

fn finish(
    mut node: EffectiveNode,
    mut history: StageHistory,
    kind: StageKind,
    explicit_fields: BTreeSet<String>,
    inputs: Vec<KeyPath>,
    merged: &MergedConfig,
) -> ComposedStage {
    // These defaults depend on the effective command, not its individual layers.
    if matches!(kind, StageKind::Command | StageKind::Hook) && node.get("stdin").is_none() {
        let stdin = if node.get("for_each_test").and_then(EffectiveNode::boolean) == Some(true) {
            "test"
        } else {
            "null"
        };
        let child = scalar("stdin", RawValue::String(stdin.into()), None);
        node_history(&child, &path(&["stdin"]), &mut history);
        insert(&mut node, child);
    }
    if let EffectiveValue::Table(t) = &mut node.value {
        if let Some(shell) = t.get_mut("shell") {
            if shell.table().is_some() && shell.get("args").is_none() {
                let child = EffectiveNode::new(
                    "args",
                    NodeKind::Atomic,
                    EffectiveValue::Array(vec![]),
                    None,
                    Provenance::BuiltIn,
                    origin("shell.args", "<built-in>"),
                );
                node_history(&child, &path(&["shell", "args"]), &mut history);
                insert(shell, child);
            }
        }
    }
    annotate(&mut node, &[], &history, &inputs, &merged.markers);
    let deferred = placeholders(&node);
    let problem = deferred.iter().any(|v| {
        !matches!(
            v.placeholder,
            Placeholder::Workspace | Placeholder::Iteration
        )
    }) || node
        .at(&["cwd", "selector"])
        .and_then(EffectiveNode::string)
        == Some("problem")
        || ["needs_build", "for_each_test"]
            .iter()
            .any(|key| node.get(key).and_then(EffectiveNode::boolean) == Some(true));
    let workspace = problem
        || deferred
            .iter()
            .any(|v| v.placeholder == Placeholder::Workspace)
        || node
            .at(&["cwd", "selector"])
            .and_then(EffectiveNode::string)
            == Some("workspace");
    ComposedStage {
        kind,
        node,
        explicit_fields,
        history,
        stdout_policy: if kind == StageKind::Run {
            CapPolicy::Kill
        } else {
            CapPolicy::Discard
        },
        stderr_policy: CapPolicy::Discard,
        context: ContextRequirements { problem, workspace },
        deferred,
        inputs,
    }
}

pub(super) fn command(merged: &MergedConfig, name: &str, hook: bool) -> ComposedStage {
    let kind = if hook {
        StageKind::Hook
    } else {
        StageKind::Command
    };
    let mut node = defaults(kind, false);
    let mut history = BTreeMap::new();
    node_history(&node, &[], &mut history);
    let input = path(&["commands", name]);
    let explicit = merged
        .command(name)
        .and_then(|c| c.node.table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    apply_row(&mut node, &mut history, merged, &input, ResetMode::Preserve);
    finish(node, history, kind, explicit, vec![input], merged)
}

/// Returns a checked derivation rather than a saturated or fallback deadline.
pub(super) fn derived_deadline(limit: &PublishedLimit) -> Option<u64> {
    limit
        .milliseconds
        .checked_mul(2)?
        .checked_add(1000)
        .filter(|ms| (1..=86_400_000).contains(ms))
}

/// A supplied problem layer selects a problem, also when it is unreadable.
pub(super) fn has_problem(merged: &MergedConfig, selection: &Selection) -> bool {
    selection.problem
        || merged.problem().is_some()
        || merged
            .unknown_inputs
            .iter()
            .any(|u| u.layer == super::LayerKind::Problem)
}

pub(super) fn target(
    merged: &MergedConfig,
    language: &str,
    profile: Option<&str>,
    selection: Option<&Selection>,
) -> ComposedTarget {
    let problem = selection.is_some_and(|s| has_problem(merged, s));
    let mut ignored: StageHistory = BTreeMap::new();
    let mut build_history = BTreeMap::new();
    let mut compose_stage = |kind| {
        let key = if kind == StageKind::Build {
            "build"
        } else {
            "run"
        };
        let row2 = path(&[key]);
        let row3 = path(&["languages", language, key]);
        let row4 = profile.map(|p| path(&["languages", language, "profiles", p, key]));
        let row5 = path(&["config", key]);
        let mut inputs = vec![row2.clone(), row3.clone()];
        if let Some(p) = &row4 {
            inputs.push(p.clone());
        }
        if problem {
            inputs.push(row5.clone());
        }
        // Isolate rows 3/4 so a reset never discards modifier rows 2/5.
        let mut surviving = table(
            key,
            if kind == StageKind::Build {
                NodeKind::Build
            } else {
                NodeKind::Stage
            },
        );
        if let Some(lang) = merged.language(language) {
            surviving.origin = lang.node.origin.clone();
            surviving.provenance = lang.node.provenance.clone();
            surviving.origin.key_path = format!("{}.{}", surviving.origin.key_path, key);
        }
        let mut surviving_history = BTreeMap::new();
        apply_row(
            &mut surviving,
            &mut surviving_history,
            merged,
            &row3,
            ResetMode::Discard,
        );
        if let Some(p) = &row4 {
            apply_row(
                &mut surviving,
                &mut surviving_history,
                merged,
                p,
                ResetMode::Discard,
            );
        }
        let exists = kind != StageKind::Build || {
            surviving.table().is_some()
                && (merged.at(&["languages", language, key]).is_some()
                    || profile.is_some_and(|p| {
                        merged
                            .at(&["languages", language, "profiles", p, key])
                            .is_some()
                    }))
        };
        if !exists {
            build_history = surviving_history;
            for p in [&row2, &row5].into_iter().take(if problem { 2 } else { 1 }) {
                let keys: Vec<_> = p.iter().map(String::as_str).collect();
                if let Some(n) = merged.at(&keys) {
                    let mut events = BTreeMap::new();
                    node_history(n, &[], &mut events);
                    for (relative, mut e) in events {
                        for event in &mut e {
                            event.kind = EventKind::Ignored;
                        }
                        let mut full = p.clone();
                        full.extend(relative);
                        ignored.entry(full).or_default().extend(e);
                    }
                }
            }
            return None;
        }
        let raw_run = selection.is_some_and(|s| s.operation == Operation::Run);
        let mut node = defaults(kind, raw_run);
        if kind == StageKind::Run {
            if let Some(limit) = selection
                .filter(|_| problem)
                .and_then(|s| s.published_time_limit.as_ref())
            {
                if let Some(ms) = derived_deadline(limit) {
                    let mut deadline = duration(ms);
                    deadline.provenance = Provenance::Derived {
                        inputs: vec![limit.provenance.clone()],
                    };
                    deadline.origin = limit.origin.clone();
                    insert(&mut node, deadline);
                }
            }
        }
        let mut history = BTreeMap::new();
        node_history(&node, &[], &mut history);
        apply_row(&mut node, &mut history, merged, &row2, ResetMode::Preserve);
        merge_stage_row(
            &mut node,
            &mut history,
            surviving,
            surviving_history,
            ResetMode::Preserve,
        );
        if problem {
            apply_row(&mut node, &mut history, merged, &row5, ResetMode::Preserve);
        }
        let mut explicit: BTreeSet<_> = node
            .table()
            .into_iter()
            .flat_map(|t| t.iter())
            .filter(|(_, v)| match v.provenance {
                Provenance::BuiltIn => v.origin.line.is_some(),
                Provenance::Derived { .. } => false,
                _ => true,
            })
            .map(|(k, _)| k.clone())
            .collect();
        if kind == StageKind::Run {
            if let Some(ms) = selection
                .filter(|s| matches!(s.operation, Operation::Test | Operation::Run))
                .and_then(|s| s.deadline_ms)
                .filter(|ms| (1..=86_400_000).contains(ms))
            {
                let mut cli = table(key, NodeKind::Stage);
                let mut deadline = duration(ms);
                deadline.provenance = Provenance::Cli {
                    flag: "--deadline".into(),
                };
                deadline.origin = origin("deadline_s", "<cli:--deadline>");
                insert(&mut cli, deadline);
                let mut cli_history = BTreeMap::new();
                node_history(&cli, &[], &mut cli_history);
                merge_stage_row(
                    &mut node,
                    &mut history,
                    cli,
                    cli_history,
                    ResetMode::Preserve,
                );
                explicit.insert("deadline_s".into());
            }
        }
        let stage = finish(node, history, kind, explicit, inputs, merged);
        if kind == StageKind::Build {
            build_history = stage.history.clone();
        }
        Some(stage)
    };
    let build = compose_stage(StageKind::Build);
    let run = compose_stage(StageKind::Run).expect("run stage always exists for validation");
    let lang = merged
        .language(language)
        .expect("language selected from merged config");
    let problem_settings = if problem { merged.problem() } else { None };
    let entry = problem_settings
        .and_then(|p| p.entry())
        .or_else(|| lang.entry())
        .cloned();
    let sources = problem_settings
        .and_then(|p| p.sources())
        .or_else(|| lang.sources())
        .cloned();
    let support_files = problem_settings
        .and_then(|p| p.support_files())
        .cloned()
        .unwrap_or_else(|| {
            EffectiveNode::new(
                "support_files",
                NodeKind::Other,
                EffectiveValue::Array(Vec::new()),
                None,
                Provenance::BuiltIn,
                origin("config.support_files", "<built-in>"),
            )
        });
    let solution_files = problem_settings
        .and_then(|p| p.solution_files())
        .cloned()
        .unwrap_or_else(|| default_solution_files(sources.as_ref(), &support_files));
    let mut artifacts = lang.artifacts().cloned();
    if language == "java" {
        if let Some(artifact) = &mut artifacts {
            if artifact.get("dir").and_then(EffectiveNode::boolean) == Some(true)
                && artifact.get("required").is_none()
            {
                if let Some(entry) = &entry {
                    if let Some(text) = entry.string() {
                        let provenance = Provenance::Derived {
                            inputs: vec![entry.provenance.clone()],
                        };
                        let mut leaf = scalar(
                            "0",
                            RawValue::String(format!("{}.class", text.replace('.', "/"))),
                            None,
                        );
                        leaf.provenance = provenance.clone();
                        leaf.origin = entry.origin.clone();
                        leaf.markers = entry.markers.clone();
                        leaf.events.push(leaf.event(EventKind::Set));
                        let mut required = EffectiveNode::new(
                            "required",
                            NodeKind::Atomic,
                            EffectiveValue::Array(vec![leaf]),
                            None,
                            provenance,
                            entry.origin.clone(),
                        );
                        required.markers = entry.markers.clone();
                        required.events.push(required.event(EventKind::Set));
                        insert(artifact, required);
                    }
                }
            }
        }
    }
    ComposedTarget {
        language: language.into(),
        profile: profile.map(str::to_owned),
        settings: TargetSettings {
            sources,
            support_files,
            solution_files,
            artifacts,
            entry,
            template: lang.template().cloned(),
            compare: problem_settings
                .and_then(|p| p.compare())
                .or_else(|| merged.compare())
                .cloned(),
            build_free: build.is_none(),
        },
        build,
        build_history,
        run,
        ignored,
    }
}

fn default_solution_files(
    sources: Option<&EffectiveNode>,
    support_files: &EffectiveNode,
) -> EffectiveNode {
    let inputs: Vec<&EffectiveNode> = sources.into_iter().chain([support_files]).collect();
    let mut files: Vec<EffectiveNode> = inputs
        .iter()
        .filter_map(|node| node.array())
        .flatten()
        .cloned()
        .collect();
    for file in &mut files {
        file.provenance = Provenance::Derived {
            inputs: vec![file.provenance.clone()],
        };
        file.events = vec![file.event(EventKind::Set)];
    }
    let provenance = Provenance::Derived {
        inputs: inputs.iter().map(|node| node.provenance.clone()).collect(),
    };
    let mut node = EffectiveNode::new(
        "solution_files",
        NodeKind::Other,
        EffectiveValue::Array(files),
        None,
        provenance,
        origin("config.solution_files", "<default>"),
    );
    node.markers = inputs
        .iter()
        .flat_map(|node| node.markers.clone())
        .collect();
    node.events.push(node.event(EventKind::Set));
    node
}
