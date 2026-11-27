//! Problem-owned judging declarations. This module never probes or starts a program.
use super::check::Validator;
use super::*;
use crate::report::{
    Aggregation, AnswerPolicy, CheckMode, DisplayProfile, InteractiveChecking, InteractiveMode,
    JudgeProfile, SeedMode,
};

#[derive(Clone, Debug, PartialEq)]
pub enum ScoringConfig {
    PassFail,
    Points(Box<PointsConfig>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointsConfig {
    pub scale: Sourced<u64>,
    pub aggregation: Sourced<Aggregation>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BuiltRole {
    pub sources: Vec<Sourced<String>>,
    pub support_files: Vec<Sourced<String>>,
    pub language: Sourced<String>,
    pub entry: Option<Sourced<String>>,
    pub artifacts: Option<Artifacts>,
    pub build: Option<ComposedStage>,
    pub run: ComposedStage,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RoleInput {
    Built(Box<BuiltRole>),
    Prebuilt(Box<PrebuiltRole>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrebuiltRole {
    pub program: Sourced<String>,
    pub run: ComposedStage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoleSpec {
    pub input: RoleInput,
    pub profile: Sourced<JudgeProfile>,
    pub assets: Vec<Sourced<String>>,
    pub deadline_ms: Option<Sourced<u64>>,
    pub stdout_cap: Option<Sourced<u64>>,
    pub stderr_cap: Option<Sourced<u64>>,
    pub seed: Option<Sourced<SeedMode>>,
}

impl RoleSpec {
    /// Configured capture order (§9.1.3); admission must still validate and copy files.
    pub fn captured_paths(&self) -> Vec<&Sourced<String>> {
        let mut paths: Vec<_> = match &self.input {
            RoleInput::Built(role) => role.sources.iter().chain(&role.support_files).collect(),
            RoleInput::Prebuilt(role) => vec![&role.program],
        };
        paths.extend(&self.assets);
        paths
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InteractiveSettings {
    pub interactor: RoleSpec,
    pub checking: Sourced<InteractiveChecking>,
    pub mode: Sourced<InteractiveMode>,
    /// Zero disables the watchdog.
    pub idle_ms: Sourced<u64>,
    pub early_reader_close: Sourced<bool>,
    pub interactor_allowance_ms: Sourced<u64>,
    pub peer_allowance_ms: Sourced<u64>,
    pub solution_seed: Sourced<SeedMode>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InteractorTest {
    pub good: Vec<BuiltRole>,
    pub bad: Vec<BuiltRole>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckConfig {
    pub kind: Sourced<CheckMode>,
    pub checker: Option<Box<RoleSpec>>,
    pub validator: Option<Box<RoleSpec>>,
    pub answer: Sourced<AnswerPolicy>,
    pub scoring: Sourced<ScoringConfig>,
    pub display: Sourced<DisplayProfile>,
    pub interactive: Option<Box<InteractiveSettings>>,
    pub interactor_test: Option<InteractorTest>,
}

impl CheckConfig {
    pub fn has_roles(&self) -> bool {
        self.checker.is_some()
            || self.validator.is_some()
            || self.kind.value == CheckMode::Interactive
    }

    /// Result-affecting settings and presentation, without finalized evidence.
    pub fn report_config(&self) -> crate::report::CheckConfig {
        use crate::report::{InteractiveConfig, Score, Scoring};
        crate::report::CheckConfig {
            kind: self.kind.value,
            answer: self.answer.value,
            display: self.display.value,
            scoring: match &self.scoring.value {
                ScoringConfig::PassFail => Scoring::PassFail,
                ScoringConfig::Points(p) => Scoring::Points {
                    scale: Score::new(i128::from(p.scale.value) * Score::UNIT)
                        .expect("validated points scale"),
                    aggregation: p.aggregation.value,
                },
            },
            interactive: self.interactive.as_ref().map(|i| InteractiveConfig {
                checking: i.checking.value,
                mode: i.mode.value,
                idle_us: (i.idle_ms.value != 0).then_some(i.idle_ms.value * 1000),
                early_reader_close: i.early_reader_close.value,
                interactor_allowance_us: i.interactor_allowance_ms.value * 1000,
                peer_allowance_us: i.peer_allowance_ms.value * 1000,
                solution_seed: i.solution_seed.value,
                interactor_seed: i
                    .interactor
                    .seed
                    .as_ref()
                    .map_or(SeedMode::None, |s| s.value),
            }),
        }
    }

    /// Phase 3, after run-count validation. Compare-only jobs retain their existing cap.
    pub fn validate_test_count(&self, count: usize) -> Result<(), &'static str> {
        if count > 1000
            && (self.checker.is_some()
                || self.validator.is_some()
                || self.kind.value == CheckMode::Interactive)
        {
            Err(
                "at most 1 000 tests per job on a problem with a checker, validator or interactor; select fewer with --case",
            )
        } else {
            Ok(())
        }
    }
}

pub(super) fn sourced<T>(n: &EffectiveNode, value: T) -> Sourced<T> {
    Sourced {
        value,
        origin: n.origin.clone(),
        provenance: n.provenance.clone(),
        overlay: n.overlay.clone(),
    }
}
fn leaf<'a>(n: &'a EffectiveNode, key: &str) -> &'a EffectiveNode {
    n.get(key).expect("validated judging field")
}
fn text(n: &EffectiveNode) -> Sourced<String> {
    sourced(n, n.string().expect("validated string").into())
}
fn paths(n: &EffectiveNode, key: &str) -> Vec<Sourced<String>> {
    n.get(key)
        .and_then(EffectiveNode::array)
        .unwrap_or(&[])
        .iter()
        .map(text)
        .collect()
}
pub(super) fn millis(n: &EffectiveNode) -> Sourced<u64> {
    let Some(TypedValue::Milliseconds(value)) = n.typed else {
        unreachable!("validated duration")
    };
    sourced(n, value)
}
fn count(n: &EffectiveNode) -> u64 {
    let Some(TypedValue::Count(value)) = n.typed else {
        unreachable!("validated count")
    };
    value
}
fn bytes(n: &EffectiveNode) -> Sourced<u64> {
    let Some(TypedValue::Bytes(value)) = n.typed else {
        unreachable!("validated size")
    };
    sourced(n, value)
}
fn seed(n: &EffectiveNode) -> Sourced<SeedMode> {
    sourced(
        n,
        match n.string() {
            Some("env") => SeedMode::Env,
            Some("argv") => SeedMode::Argv,
            _ => SeedMode::None,
        },
    )
}
pub(super) fn clean(n: &EffectiveNode) -> bool {
    n.markers.is_empty()
}
pub(super) fn error(
    v: &mut Validator<'_>,
    n: &EffectiveNode,
    message: impl Into<String>,
    other: Option<&EffectiveNode>,
) {
    v.error(n.origin.clone(), message, other.map(|n| n.origin.clone()));
}
pub(super) fn required(v: &mut Validator<'_>, n: &EffectiveNode, key: &str) {
    if n.get(key).is_none()
        && !n.markers.iter().any(|m| {
            m.origin.key_path == format!("{}.{key}", n.origin.key_path)
                || m.origin.key_path == n.origin.key_path
        })
    {
        let mut origin = n.origin.clone();
        origin.key_path = format!("{}.{key}", origin.key_path);
        v.error(origin, format!("required field {key}"), None);
    }
}

fn default_node(key: &str, value: RawValue, typed: Option<TypedValue>) -> EffectiveNode {
    EffectiveNode::new(
        key,
        NodeKind::Other,
        EffectiveValue::Scalar(value),
        typed,
        Provenance::BuiltIn,
        stages::origin(key, "<built-in>"),
    )
}
pub(super) fn insert_default(
    n: &mut EffectiveNode,
    key: &str,
    value: RawValue,
    typed: Option<TypedValue>,
) {
    if let EffectiveValue::Table(table) = &mut n.value {
        table
            .entry(key.into())
            .or_insert_with(|| default_node(key, value, typed));
    }
}
fn strings_default(n: &mut EffectiveNode, key: &str) {
    if let EffectiveValue::Table(table) = &mut n.value {
        table.entry(key.into()).or_insert_with(|| {
            EffectiveNode::new(
                key,
                NodeKind::Other,
                EffectiveValue::Array(vec![]),
                None,
                Provenance::BuiltIn,
                stages::origin(key, "<built-in>"),
            )
        });
    }
}
pub(super) fn role_defaults(n: &mut EffectiveNode, interactor: bool) {
    if n.get("program").is_none() {
        strings_default(n, "support_files");
    }
    strings_default(n, "assets");
    if interactor {
        insert_default(n, "seed", RawValue::String("none".into()), None);
    }
}
pub(super) fn entry_default(n: &mut EffectiveNode, entries: &BTreeMap<String, EffectiveNode>) {
    if let Some(entry) = n
        .get("language")
        .and_then(EffectiveNode::string)
        .and_then(|l| entries.get(l))
        .cloned()
    {
        if let EffectiveValue::Table(table) = &mut n.value {
            table.entry("entry".into()).or_insert(entry);
        }
    }
}

/// Inject defaults only into a selected problem record, never into a config layer.
pub(super) fn defaults(root: &mut EffectiveNode) {
    let EffectiveValue::Table(table) = &mut root.value else {
        return;
    };
    let n = table.entry("check".into()).or_insert_with(|| {
        EffectiveNode::new(
            "check",
            NodeKind::Other,
            EffectiveValue::Table(BTreeMap::new()),
            None,
            Provenance::BuiltIn,
            stages::origin("check", "<built-in>"),
        )
    });
    for (key, value) in [
        ("kind", "compare"),
        ("answer", "file"),
        ("scoring", "pass_fail"),
        ("display", "cpf"),
    ] {
        insert_default(n, key, RawValue::String(value.into()), None);
    }
    if let EffectiveValue::Table(t) = &mut n.value {
        if let Some(s) = t.get_mut("scoring") {
            insert_default(s, "aggregation", RawValue::String("sum".into()), None);
        }
        for key in ["checker", "validator"] {
            if let Some(r) = t.get_mut(key) {
                role_defaults(r, false);
            }
        }
        if let Some(i) = t.get_mut("interactive") {
            for (key, value) in [("mode", "relay"), ("solution_seed", "none")] {
                insert_default(i, key, RawValue::String(value.into()), None);
            }
            insert_default(i, "early_reader_close", RawValue::Boolean(false), None);
            for (key, seconds) in [
                ("idle_s", 0),
                ("interactor_allowance_s", 1),
                ("peer_allowance_s", 1),
            ] {
                insert_default(
                    i,
                    key,
                    RawValue::Integer(seconds),
                    Some(TypedValue::Milliseconds(seconds as u64 * 1000)),
                );
            }
            if let EffectiveValue::Table(t) = &mut i.value {
                if let Some(r) = t.get_mut("interactor") {
                    role_defaults(r, true);
                }
            }
        }
        if let Some(test) = t.get_mut("interactor_test") {
            for key in ["good", "bad"] {
                strings_default(test, key);
                if let EffectiveValue::Table(t) = &mut test.value {
                    if let Some(EffectiveNode {
                        value: EffectiveValue::Array(roles),
                        ..
                    }) = t.get_mut(key)
                    {
                        for role in roles {
                            strings_default(role, "support_files");
                        }
                    }
                }
            }
        }
    }
}

/// Inherited entries retain the language field's invalid markers and provenance.
pub(super) fn inherit_entries(root: &mut EffectiveNode) {
    let entries: BTreeMap<_, _> = root
        .get("languages")
        .and_then(EffectiveNode::table)
        .into_iter()
        .flat_map(|t| t.iter())
        .filter_map(|(language, n)| {
            n.get("entry")
                .map(|entry| (language.clone(), entry.clone()))
        })
        .collect();
    let EffectiveValue::Table(root) = &mut root.value else {
        return;
    };
    let Some(check) = root.get_mut("check") else {
        return;
    };
    let EffectiveValue::Table(check) = &mut check.value else {
        return;
    };
    for key in ["checker", "validator"] {
        if let Some(role) = check.get_mut(key).filter(|n| n.get("program").is_none()) {
            entry_default(role, &entries);
        }
    }
    if let Some(EffectiveNode {
        value: EffectiveValue::Table(interactive),
        ..
    }) = check.get_mut("interactive")
    {
        if let Some(role) = interactive
            .get_mut("interactor")
            .filter(|n| n.get("program").is_none())
        {
            entry_default(role, &entries);
        }
    }
    if let Some(EffectiveNode {
        value: EffectiveValue::Table(test),
        ..
    }) = check.get_mut("interactor_test")
    {
        for key in ["good", "bad"] {
            if let Some(EffectiveNode {
                value: EffectiveValue::Array(roles),
                ..
            }) = test.get_mut(key)
            {
                for role in roles {
                    entry_default(role, &entries);
                }
            }
        }
    }
}

fn overlaps(v: &mut Validator<'_>, n: &EffectiveNode) {
    let platform = v.merged.platform;
    let list = |key| {
        n.get(key)
            .filter(|n| clean(n))
            .and_then(EffectiveNode::array)
            .unwrap_or(&[])
    };
    for a in list("sources") {
        for b in list("support_files") {
            if syntax::source_key(a.string().unwrap(), platform)
                == syntax::source_key(b.string().unwrap(), platform)
            {
                error(
                    v,
                    b,
                    "path appears in both sources and support_files",
                    Some(a),
                );
            }
        }
    }
    for asset in list("assets") {
        let path = asset.string().unwrap();
        let first = syntax::source_key(path.split('/').next().unwrap(), platform);
        if ["input", "output", "answer", "tout", "report", "transcript"].contains(&first.as_str()) {
            error(v, asset, "reserved asset first component", None);
        }
        let files = list("sources")
            .iter()
            .chain(list("support_files"))
            .chain(n.get("program").filter(|p| clean(p)));
        for file in files {
            if syntax::source_key(path, platform)
                == syntax::source_key(file.string().unwrap(), platform)
            {
                error(
                    v,
                    asset,
                    "asset is already captured by the role",
                    Some(file),
                );
            }
        }
    }
}
fn restrict_stage(v: &mut Validator<'_>, role: &EffectiveNode, stage: &ComposedStage) {
    v.stage(stage);
    if let Some(cwd) = stage.node.get("cwd").filter(|n| clean(n)) {
        if cwd.get("selector").and_then(EffectiveNode::string) != Some("run") {
            error(
                v,
                role,
                "role stage cwd must select the private run directory",
                Some(cwd),
            );
        }
    }
    for deferred in &stage.deferred {
        if matches!(
            deferred.placeholder,
            Placeholder::ProblemDir
                | Placeholder::Workspace
                | Placeholder::TestInput
                | Placeholder::TestAnswer
                | Placeholder::Iteration
        ) {
            v.error(
                role.origin.clone(),
                "role stage uses a forbidden placeholder",
                Some(deferred.origin.clone()),
            );
        }
    }
}
fn built_role(v: &mut Validator<'_>, n: &EffectiveNode) -> Option<BuiltRole> {
    let before = v.diagnostics.len();
    for key in ["sources", "language"] {
        required(v, n, key);
    }
    if let Some(sources) = n.get("sources").filter(|n| clean(n)) {
        if sources.array().is_some_and(|a| a.is_empty()) {
            error(v, sources, "built role sources must not be empty", None);
        }
    }
    let language = n
        .get("language")
        .filter(|n| clean(n))
        .and_then(EffectiveNode::string);
    let mut target = language.and_then(|language| {
        if v.merged.language(language).is_some() {
            Some(stages::target(v.merged, language, None, None))
        } else {
            if v.merged
                .unknown_inputs
                .iter()
                .all(|u| u.layer == LayerKind::Problem)
                && !stages::marked(v.merged, &[stages::path(&["languages", language])])
            {
                error(
                    v,
                    leaf(n, "language"),
                    format!("unknown role language {language:?}"),
                    None,
                );
            }
            None
        }
    });
    if let Some(target) = &mut target {
        for key in ["deadline_s", "stdout_cap", "stderr_cap"] {
            if let Some(value) = n.get(key).filter(|n| clean(n)) {
                if let EffectiveValue::Table(table) = &mut target.run.node.value {
                    table.insert(key.into(), value.clone());
                    target.run.explicit_fields.insert(key.into());
                }
            }
        }
        if let Some(build) = &target.build {
            restrict_stage(v, n, build);
        }
        restrict_stage(v, n, &target.run);
    }
    if v.diagnostics.len() != before || !clean(n) {
        return None;
    }
    let target = target?;
    if target.settings.entry.as_ref().is_some_and(|n| !clean(n)) {
        return None;
    }
    let entry = n.get("entry").or(target.settings.entry.as_ref()).map(text);
    let mut artifacts = target
        .settings
        .artifacts
        .as_ref()
        .filter(|_| target.build.is_some())
        .and_then(super::api::role_artifacts);
    if language == Some("java") {
        if let (Some(Artifacts::Dir { required }), Some(entry)) = (&mut artifacts, &entry) {
            for path in required
                .iter_mut()
                .filter(|p| matches!(p.provenance, Provenance::Derived { .. }))
            {
                path.value = format!("{}.class", entry.value.replace('.', "/"));
            }
        }
    }
    Some(BuiltRole {
        sources: paths(n, "sources"),
        support_files: paths(n, "support_files"),
        language: text(leaf(n, "language")),
        entry,
        artifacts,
        build: target.build,
        run: target.run,
    })
}
pub(super) fn role(v: &mut Validator<'_>, n: &EffectiveNode, interactor: bool) -> Option<RoleSpec> {
    let before = v.diagnostics.len();
    required(v, n, "profile");
    overlaps(v, n);
    let input = if let Some(program) = n.get("program") {
        if !clean(n) {
            None
        } else {
            let mut node = stages::defaults(StageKind::Run, false);
            if let EffectiveValue::Table(table) = &mut node.value {
                let mut argv = program.clone();
                argv.name = "argv".into();
                let mut executable = program.clone();
                executable.typed = Some(TypedValue::Text(ParsedText {
                    unexpanded: "{source}".into(),
                    tokens: vec![Token::Placeholder(Placeholder::Source)],
                    path: None,
                    lookup: false,
                    expanded_home: None,
                }));
                argv.value = EffectiveValue::Array(vec![executable]);
                table.insert("argv".into(), argv);
                for key in ["deadline_s", "stdout_cap", "stderr_cap"] {
                    if let Some(value) = n.get(key) {
                        table.insert(key.into(), value.clone());
                    }
                }
            }
            Some(RoleInput::Prebuilt(Box::new(PrebuiltRole {
                program: text(program),
                run: ComposedStage {
                    kind: StageKind::Run,
                    node,
                    explicit_fields: ["deadline_s", "stdout_cap", "stderr_cap"]
                        .into_iter()
                        .filter(|key| n.get(key).is_some())
                        .map(str::to_owned)
                        .collect(),
                    history: Default::default(),
                    stdout_policy: CapPolicy::Kill,
                    stderr_policy: CapPolicy::Discard,
                    context: Default::default(),
                    deferred: vec![DeferredValue {
                        placeholder: Placeholder::Source,
                        origin: program.origin.clone(),
                        provenance: program.provenance.clone(),
                    }],
                    inputs: vec![],
                },
            })))
        }
    } else {
        built_role(v, n).map(|r| RoleInput::Built(Box::new(r)))
    };
    if before != v.diagnostics.len() || !clean(n) {
        return None;
    }
    Some(RoleSpec {
        input: input?,
        profile: sourced(
            leaf(n, "profile"),
            if leaf(n, "profile").string() == Some("simple") {
                JudgeProfile::Simple
            } else {
                JudgeProfile::Testlib
            },
        ),
        assets: paths(n, "assets"),
        deadline_ms: n.get("deadline_s").map(millis),
        stdout_cap: n.get("stdout_cap").map(bytes),
        stderr_cap: n.get("stderr_cap").map(bytes),
        seed: interactor.then(|| seed(leaf(n, "seed"))),
    })
}

/// Boxed: the typed declaration is held by every report and plan that carries a universe.
pub(super) fn validate(v: &mut Validator<'_>, selection: &Selection) -> Option<Box<CheckConfig>> {
    let merged = v.merged;
    let n = merged.at(&["check"])?;
    let before = v.diagnostics.len();
    let kind = n
        .get("kind")
        .filter(|n| clean(n))
        .and_then(EffectiveNode::string);
    let interactive = n.get("interactive");
    let checking = interactive
        .and_then(|i| i.get("checking"))
        .filter(|n| clean(n))
        .and_then(EffectiveNode::string);
    let needs_checker = match kind {
        Some("checker") => Some(true),
        Some("interactive") => checking.map(|c| c != "interactor"),
        Some("compare") => Some(false),
        _ => None,
    };
    if let Some(needed) = needs_checker {
        if needed {
            required(v, n, "checker");
        } else if let Some(checker) = n.get("checker") {
            error(
                v,
                checker,
                "checker is not allowed for this judging kind/checking",
                n.get("kind"),
            );
        }
    }
    if let Some(kind) = kind {
        if kind == "interactive" {
            required(v, n, "interactive");
        } else {
            for key in ["interactive", "interactor_test"] {
                if let Some(field) = n.get(key) {
                    error(
                        v,
                        field,
                        "allowed only with kind = interactive",
                        n.get("kind"),
                    );
                }
            }
        }
        if kind == "compare" {
            if let Some(answer) = n
                .get("answer")
                .filter(|a| clean(a) && a.string() == Some("none"))
            {
                error(
                    v,
                    answer,
                    "answer = none is not allowed with compare",
                    n.get("kind"),
                );
            }
            if let Some(scoring) = n.get("scoring").filter(|s| s.table().is_some()) {
                error(
                    v,
                    scoring,
                    "points scoring is not allowed with compare",
                    n.get("kind"),
                );
            }
        }
    }
    if let Some(scoring) = n.get("scoring").filter(|s| s.table().is_some()) {
        for key in ["points", "scale"] {
            required(v, scoring, key);
        }
    }
    let problem_kind = merged.problem_kind.as_deref();
    if selection.operation == Operation::Test
        && problem_kind.and_then(EffectiveNode::string) == Some("interactive")
        && kind.is_some_and(|k| k != "interactive")
    {
        error(
            v,
            problem_kind.unwrap(),
            "interactive problem needs [check] kind = \"interactive\" and an interactor",
            n.get("kind"),
        );
    }
    if kind == Some("interactive")
        || problem_kind.and_then(EffectiveNode::string) == Some("interactive")
    {
        if let Some(io) = v.merged.at(&["config", "io"]).filter(|io| clean(io)) {
            error(
                v,
                io,
                "file I/O is not allowed for interactive problems",
                n.get("kind")
                    .filter(|_| kind == Some("interactive"))
                    .or(problem_kind),
            );
        }
    }
    let checker = n
        .get("checker")
        .and_then(|r| role(v, r, false))
        .map(Box::new);
    let validator = n
        .get("validator")
        .and_then(|r| role(v, r, false))
        .map(Box::new);
    let interactive = interactive.and_then(|i| {
        for key in ["interactor", "checking"] {
            required(v, i, key);
        }
        if i.get("mode").and_then(EffectiveNode::string) == Some("direct") {
            if let Some(idle) = i.get("idle_s").filter(|n| clean(n)) {
                if millis(idle).value != 0 {
                    error(
                        v,
                        idle,
                        "idle watchdog is not allowed with direct mode",
                        i.get("mode"),
                    );
                }
            }
        }
        let interactor = i.get("interactor").and_then(|r| role(v, r, true));
        if !clean(i) || i.get("checking").is_none() {
            return None;
        }
        Some(Box::new(InteractiveSettings {
            interactor: interactor?,
            checking: sourced(
                leaf(i, "checking"),
                match checking? {
                    "interactor" => InteractiveChecking::Interactor,
                    "checker" => InteractiveChecking::Checker,
                    _ => InteractiveChecking::InteractorChecker,
                },
            ),
            mode: sourced(
                leaf(i, "mode"),
                if leaf(i, "mode").string() == Some("direct") {
                    InteractiveMode::Direct
                } else {
                    InteractiveMode::Relay
                },
            ),
            idle_ms: millis(leaf(i, "idle_s")),
            early_reader_close: sourced(
                leaf(i, "early_reader_close"),
                leaf(i, "early_reader_close").boolean().unwrap(),
            ),
            interactor_allowance_ms: millis(leaf(i, "interactor_allowance_s")),
            peer_allowance_ms: millis(leaf(i, "peer_allowance_s")),
            solution_seed: seed(leaf(i, "solution_seed")),
        }))
    });
    let interactor_test = n.get("interactor_test").map(|test| {
        let mut solutions = |key| {
            test.get(key)
                .and_then(EffectiveNode::array)
                .unwrap_or(&[])
                .iter()
                .filter_map(|s| {
                    overlaps(v, s);
                    built_role(v, s)
                })
                .collect()
        };
        InteractorTest {
            good: solutions("good"),
            bad: solutions("bad"),
        }
    });
    if before != v.diagnostics.len() || !clean(n) {
        return None;
    }
    // No declaration may be lost when an unknown upstream layer prevents role resolution.
    if n.get("checker").is_some() != checker.is_some()
        || n.get("validator").is_some() != validator.is_some()
        || n.get("interactive").is_some() != interactive.is_some()
    {
        return None;
    }
    let scoring = leaf(n, "scoring");
    Some(Box::new(CheckConfig {
        kind: sourced(
            leaf(n, "kind"),
            match kind? {
                "checker" => CheckMode::Checker,
                "interactive" => CheckMode::Interactive,
                _ => CheckMode::Compare,
            },
        ),
        checker,
        validator,
        answer: sourced(
            leaf(n, "answer"),
            if leaf(n, "answer").string() == Some("none") {
                AnswerPolicy::None
            } else {
                AnswerPolicy::File
            },
        ),
        scoring: sourced(
            scoring,
            if scoring.table().is_some() {
                ScoringConfig::Points(Box::new(PointsConfig {
                    scale: sourced(leaf(scoring, "scale"), count(leaf(scoring, "scale"))),
                    aggregation: sourced(
                        leaf(scoring, "aggregation"),
                        if leaf(scoring, "aggregation").string() == Some("min") {
                            Aggregation::Min
                        } else {
                            Aggregation::Sum
                        },
                    ),
                }))
            } else {
                ScoringConfig::PassFail
            },
        ),
        display: sourced(
            leaf(n, "display"),
            if leaf(n, "display").string() == Some("judge-cf") {
                DisplayProfile::JudgeCf
            } else {
                DisplayProfile::Cpf
            },
        ),
        interactive,
        interactor_test,
    }))
}
