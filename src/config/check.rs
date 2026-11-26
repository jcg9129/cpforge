use std::collections::BTreeMap;

use super::stages::{self, KeyPath, fields, marked, path};
use super::{
    ComposedStage, ComposedTarget, Diagnostic, EffectiveNode, EffectiveValue, LayerKind,
    MergedConfig, Operation, Origin, Placeholder, Selection, Severity, StageKind,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComposedUniverse {
    /// Each language without a profile and with every profile, through rows 1–4.
    pub languages: BTreeMap<(String, Option<String>), ComposedTarget>,
    pub commands: BTreeMap<String, ComposedStage>,
    /// Commands named by hooks, recomposed with hook defaults and restrictions.
    pub hooks: BTreeMap<String, ComposedStage>,
    pub selected: Option<ComposedTarget>,
    pub check: Option<Box<super::CheckConfig>>,
}

/// Inspection is always available. The executable capability is issued only
/// when the complete phase-1/2 report has no errors; warnings do not block it.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckReport {
    diagnostics: Vec<Diagnostic>,
    universe: ComposedUniverse,
}

impl CheckReport {
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn universe(&self) -> &ComposedUniverse {
        &self.universe
    }
    pub fn executable(&self) -> Option<ExecutableConfig<'_>> {
        (!self
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error))
        .then_some(ExecutableConfig { report: self })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExecutableConfig<'a> {
    report: &'a CheckReport,
}

impl<'a> ExecutableConfig<'a> {
    pub fn universe(self) -> &'a ComposedUniverse {
        &self.report.universe
    }
    /// A command for `cpf cmd`, composed with command defaults.
    pub fn command(self, name: &str) -> Option<ExecutableStage<'a>> {
        self.report
            .universe
            .commands
            .get(name)
            .map(|stage| ExecutableStage { stage })
    }
    /// A command named by a hook, composed with hook defaults and restrictions.
    pub fn hook(self, name: &str) -> Option<ExecutableStage<'a>> {
        self.report
            .universe
            .hooks
            .get(name)
            .map(|stage| ExecutableStage { stage })
    }
}

/// A command or hook stage from a configuration without phase-1/2 errors.
/// Planning takes this rather than a bare `ComposedStage`.
#[derive(Clone, Copy, Debug)]
pub struct ExecutableStage<'a> {
    stage: &'a ComposedStage,
}

impl<'a> ExecutableStage<'a> {
    pub fn stage(self) -> &'a ComposedStage {
        self.stage
    }
}

/// Validated declarative target. Phase 3 must still supply context, resolve
/// programs and materialize deferred values before any execution.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTarget {
    /// Clean report whose `universe.selected` is present.
    report: CheckReport,
}

impl ResolvedTarget {
    pub fn target(&self) -> &ComposedTarget {
        self.report
            .universe
            .selected
            .as_ref()
            .expect("resolved target has a selection")
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.report.diagnostics
    }
    /// The same clean report, e.g. for the hooks of `cpf test`.
    pub fn executable(&self) -> ExecutableConfig<'_> {
        ExecutableConfig {
            report: &self.report,
        }
    }
}

pub(super) struct Validator<'a> {
    pub(super) merged: &'a MergedConfig,
    pub(super) diagnostics: Vec<Diagnostic>,
}

impl Validator<'_> {
    pub(super) fn error(
        &mut self,
        origin: Origin,
        message: impl Into<String>,
        other: Option<Origin>,
    ) {
        let d = Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            origin,
            second_origin: other,
        };
        // The same inherited defect may occur in several universe members.
        if !self.diagnostics.contains(&d) {
            self.diagnostics.push(d);
        }
    }

    fn unknown_definitions(&self) -> bool {
        self.merged
            .unknown_inputs
            .iter()
            .any(|u| u.layer != LayerKind::Problem)
    }

    fn rank(&self, node: &EffectiveNode) -> u8 {
        match &node.provenance {
            super::Provenance::File { path, .. } => self
                .merged
                .layer_files
                .get(path)
                .map_or(0, |kind| rank(*kind)),
            super::Provenance::Cli { .. } => 4,
            _ => 0,
        }
    }

    // Replace-as-unit choices are dependencies as a whole: a known higher
    // shell cannot acquire its missing script from an unreadable lower file.
    fn unknown_field(&self, stage: &ComposedStage, field: &str) -> bool {
        if field == "deadline_s"
            && stage
                .node
                .get(field)
                .is_some_and(|n| matches!(n.provenance, super::Provenance::Cli { .. }))
        {
            return false;
        }
        let choices: &[&str] = if matches!(field, "argv" | "shell") {
            &["argv", "shell"]
        } else {
            std::slice::from_ref(&field)
        };
        self.merged.unknown_inputs.iter().any(|unknown| {
            stage.inputs.iter().enumerate().any(|(i, input)| {
                let problem_row = input.first().is_some_and(|s| s == "config");
                if problem_row != (unknown.layer == LayerKind::Problem) {
                    return false;
                }
                !stage.inputs.iter().enumerate().skip(i).any(|(j, higher)| {
                    let keys: Vec<_> = higher.iter().map(String::as_str).collect();
                    let Some(row) = self.merged.at(&keys) else {
                        return false;
                    };
                    let reset_blocks = stage.kind == StageKind::Build
                        && input.first().is_some_and(|s| s == "languages")
                        && higher.first().is_some_and(|s| s == "languages")
                        && row.reset_occurred
                        && (j > i
                            || row.events.iter().any(|event| {
                                event.kind == super::EventKind::Reset
                                    && self
                                        .merged
                                        .layer_files
                                        .get(&event.origin.file)
                                        .is_some_and(|kind| rank(*kind) > rank(unknown.layer))
                            }));
                    reset_blocks
                        || choices.iter().any(|key| {
                            row.get(key)
                                .is_some_and(|n| j > i || self.rank(n) > rank(unknown.layer))
                        })
                })
            })
        })
    }

    fn unknown_atomic(&self, node: &EffectiveNode) -> bool {
        self.merged.unknown_inputs.iter().any(|unknown| {
            unknown.layer != LayerKind::Problem && rank(unknown.layer) >= self.rank(node)
        })
    }

    fn blocked(&self, stage: &ComposedStage, suffixes: &[&[&str]]) -> bool {
        marked(self.merged, &fields(&stage.inputs, suffixes))
    }

    pub(super) fn stage(&mut self, stage: &ComposedStage) {
        let node = &stage.node;
        let command_blocked = self.blocked(stage, &[&["argv"], &["shell"]]);
        let argv = node.get("argv");
        let shell = node.get("shell");
        if !command_blocked {
            match (argv, shell) {
                (None, None) if !self.unknown_field(stage, "argv") => {
                    self.error(node.origin.clone(), "stage requires argv or shell", None);
                }
                (Some(a), Some(b)) => self.error(
                    a.origin.clone(),
                    "argv and shell are mutually exclusive",
                    Some(b.origin.clone()),
                ),
                _ => {}
            }
        }
        if let Some(shell) = shell {
            for key in ["interpreter", "script"] {
                // Invalid shell.args does not suppress a missing interpreter.
                if shell.get(key).is_none()
                    && !self.blocked(stage, &[&["argv"], &["shell", key]])
                    && !self.unknown_field(stage, "shell")
                {
                    self.error(
                        missing_origin(shell, key),
                        format!("shell requires {key}"),
                        None,
                    );
                }
            }
        }
        if let Some(cwd) = node.get("cwd") {
            if !self.blocked(stage, &[&["cwd", "path"], &["cwd", "selector"]]) {
                match (cwd.get("path"), cwd.get("selector")) {
                    (None, None) if !self.unknown_field(stage, "cwd") => {
                        self.error(cwd.origin.clone(), "cwd requires path or selector", None)
                    }
                    (Some(a), Some(b)) => self.error(
                        a.origin.clone(),
                        "path and selector are mutually exclusive",
                        Some(b.origin.clone()),
                    ),
                    _ => {}
                }
            }
        }
        if !matches!(stage.kind, StageKind::Command | StageKind::Hook) {
            return;
        }
        if !self.blocked(stage, &[&["stdin"], &["for_each_test"]])
            && node.get("stdin").and_then(EffectiveNode::string) == Some("test")
            && node.get("for_each_test").and_then(EffectiveNode::boolean) != Some(true)
            && !self.unknown_field(stage, "for_each_test")
            && !self.unknown_field(stage, "stdin")
        {
            self.error(
                node.get("stdin").expect("test stdin").origin.clone(),
                "stdin = test requires for_each_test = true",
                node.get("for_each_test")
                    .filter(|_| stage.explicitly_set("for_each_test"))
                    .map(|n| n.origin.clone()),
            );
        }
        if stage.kind == StageKind::Hook {
            for key in ["for_each_test", "repeat", "needs_build"] {
                if stage.explicitly_set(key) && !self.blocked(stage, &[&[key]]) {
                    self.error(
                        node.get(key).expect("explicit key").origin.clone(),
                        format!("{key} is not allowed in hook context (even when false or 1)"),
                        None,
                    );
                }
            }
        }
        // Walk only tokenized fields. Scripts, interpreters and env are literal.
        self.command_placeholders(stage, node, &[]);
    }

    fn command_placeholders(
        &mut self,
        stage: &ComposedStage,
        node: &EffectiveNode,
        relative: &[String],
    ) {
        if let Some(super::TypedValue::Text(text)) = &node.typed {
            let paths: Vec<_> = stage
                .inputs
                .iter()
                .map(|input| {
                    let mut p = input.clone();
                    p.extend_from_slice(relative);
                    p
                })
                .collect();
            let unknown_text = relative
                .first()
                .is_some_and(|field| self.unknown_field(stage, field));
            if !marked(self.merged, &paths) && !unknown_text {
                for token in &text.tokens {
                    let super::Token::Placeholder(placeholder) = token else {
                        continue;
                    };
                    if stage.kind == StageKind::Hook
                        && !matches!(
                            placeholder,
                            Placeholder::ProblemDir
                                | Placeholder::ProblemId
                                | Placeholder::Workspace
                        )
                    {
                        self.error(
                            node.origin.clone(),
                            format!(
                                "placeholder {{{}}} is not allowed in hook context",
                                placeholder.name()
                            ),
                            None,
                        );
                    }
                    let requirement = match placeholder {
                        Placeholder::Artifact | Placeholder::ArtifactDir => Some((
                            "needs_build",
                            "artifact placeholders require needs_build = true",
                        )),
                        Placeholder::Sources | Placeholder::Source => Some((
                            "needs_build",
                            "source placeholders require needs_build = true",
                        )),
                        Placeholder::TestInput | Placeholder::TestAnswer => Some((
                            "for_each_test",
                            "test placeholders require for_each_test = true",
                        )),
                        Placeholder::Iteration => {
                            Some(("repeat", "iteration placeholder requires repeat"))
                        }
                        _ => None,
                    };
                    if let Some((key, message)) = requirement {
                        let declared = if key == "repeat" {
                            // The implicit count of one is not a declaration.
                            stage.explicitly_set(key)
                        } else {
                            stage.node.get(key).and_then(EffectiveNode::boolean) == Some(true)
                        };
                        if !declared
                            && !self.blocked(stage, &[&[key]])
                            && !self.unknown_field(stage, key)
                        {
                            self.error(
                                node.origin.clone(),
                                message,
                                stage
                                    .node
                                    .get(key)
                                    .filter(|_| stage.explicitly_set(key))
                                    .map(|n| n.origin.clone()),
                            );
                        }
                    }
                }
            }
        }
        match &node.value {
            EffectiveValue::Table(t) => {
                for (key, child) in t {
                    let mut next = relative.to_vec();
                    next.push(key.clone());
                    self.command_placeholders(stage, child, &next);
                }
            }
            EffectiveValue::Array(a) => {
                for (i, child) in a.iter().enumerate() {
                    let mut next = relative.to_vec();
                    next.push(i.to_string());
                    self.command_placeholders(stage, child, &next);
                }
            }
            EffectiveValue::Scalar(_) => {}
        }
    }

    fn file_io_cwd(&mut self, io: &EffectiveNode, stage: &ComposedStage) {
        // Invalid cwd declarations have their own phase-1/2 diagnostics; do
        // not infer a shared directory from an invalid or unknown selector.
        if self.blocked(stage, &[&["cwd"], &["cwd", "path"], &["cwd", "selector"]])
            || self.unknown_field(stage, "cwd")
        {
            return;
        }
        let Some(cwd) = stage.node.get("cwd") else {
            return;
        };
        let shared = match (cwd.get("path"), cwd.get("selector")) {
            (Some(_), None) => true,
            (None, Some(selector)) => selector.string().is_some_and(|s| s != "run"),
            _ => false,
        };
        if shared {
            self.error(
                io.origin.clone(),
                "file-based I/O needs the run's own directory",
                Some(cwd.origin.clone()),
            );
        }
    }

    fn target(&mut self, target: &ComposedTarget) {
        self.stage(&target.run);
        if let Some(build) = &target.build {
            self.stage(build);
        }
        self.file_roles(target);
        let Some(artifacts) = &target.settings.artifacts else {
            return;
        };
        let input = path(&["languages", &target.language, "artifacts"]);
        if !marked(
            self.merged,
            &fields(std::slice::from_ref(&input), &[&["file"], &["dir"]]),
        ) {
            match (artifacts.get("file"), artifacts.get("dir")) {
                (None, None) if !self.unknown_atomic(artifacts) => self.error(
                    artifacts.origin.clone(),
                    "artifacts requires file or dir = true",
                    None,
                ),
                (Some(file), Some(dir)) => self.error(
                    file.origin.clone(),
                    "file and dir are mutually exclusive",
                    Some(dir.origin.clone()),
                ),
                _ => {}
            }
        }
        if artifacts.get("dir").and_then(EffectiveNode::boolean) == Some(true) {
            let uses_entry = target.language == "java"
                && artifacts
                    .get("required")
                    .is_none_or(|n| matches!(n.provenance, super::Provenance::Derived { .. }));
            let mut dependencies = fields(std::slice::from_ref(&input), &[&["dir"], &["required"]]);
            if uses_entry {
                dependencies.push(path(&["languages", "java", "entry"]));
                if target.run.inputs.contains(&path(&["config", "run"])) {
                    dependencies.push(path(&["config", "entry"]));
                }
            }
            let unknown_entry = uses_entry
                && target.settings.entry.as_ref().map_or_else(
                    || self.unknown_definitions(),
                    |entry| self.unknown_atomic(entry),
                );
            if !marked(self.merged, &dependencies)
                && !self.unknown_atomic(artifacts)
                && !unknown_entry
            {
                let required = artifacts.get("required").and_then(EffectiveNode::array);
                if required.is_none_or(|r| r.is_empty()) {
                    self.error(
                        artifacts
                            .get("required")
                            .unwrap_or(artifacts)
                            .origin
                            .clone(),
                        "directory artifact requires non-empty required paths after defaults",
                        None,
                    );
                }
                if let Some(required) = required {
                    for item in required {
                        let derived = matches!(item.provenance, super::Provenance::Derived { .. });
                        if item.string().is_some_and(|s| {
                            !super::syntax::output_relative(s, self.merged.platform)
                                || (derived
                                    && s.split(|c| {
                                        c == '/'
                                            || (self.merged.platform == super::Platform::Windows
                                                && c == '\\')
                                    })
                                    .any(|c| c.is_empty() || c == "."))
                        }) {
                            self.error(
                                item.origin.clone(),
                                "derived artifact must be output-relative",
                                None,
                            );
                        }
                    }
                }
            }
        }
        if let (Some(file), Some(required)) = (artifacts.get("file"), artifacts.get("required")) {
            if !marked(self.merged, &fields(&[input], &[&["file"], &["required"]])) {
                self.error(
                    file.origin.clone(),
                    "file and required are mutually exclusive",
                    Some(required.origin.clone()),
                );
            }
        }
    }

    fn file_roles(&mut self, target: &ComposedTarget) {
        let settings = &target.settings;
        let mut dependencies = vec![path(&["languages", &target.language, "sources"])];
        if target.run.inputs.contains(&path(&["config", "run"])) {
            dependencies.extend([
                path(&["config", "sources"]),
                path(&["config", "support_files"]),
            ]);
        }
        let unknown_sources = settings.sources.as_ref().map_or_else(
            || self.unknown_definitions(),
            |node| self.unknown_atomic(node),
        );
        if marked(self.merged, &dependencies) || unknown_sources {
            return;
        }
        let mut captured: BTreeMap<String, &EffectiveNode> = BTreeMap::new();
        for source in settings
            .sources
            .iter()
            .filter_map(EffectiveNode::array)
            .flatten()
        {
            if let Some(text) = source.string() {
                captured.insert(
                    super::syntax::source_key(text, self.merged.platform),
                    source,
                );
            }
        }
        for support in settings.support_files.array().into_iter().flatten() {
            let Some(text) = support.string() else {
                continue;
            };
            let key = super::syntax::source_key(text, self.merged.platform);
            if let Some(source) = captured.get(&key) {
                self.error(
                    support.origin.clone(),
                    "path appears in both sources and support_files",
                    Some(source.origin.clone()),
                );
            } else {
                captured.insert(key, support);
            }
        }
        // Default solution files are captured files by construction. An invalid
        // explicit list cannot supply inputs for the membership check.
        if matches!(
            settings.solution_files.provenance,
            super::Provenance::Derived { .. }
        ) || marked(self.merged, &[path(&["config", "solution_files"])])
        {
            return;
        }
        for solution in settings.solution_files.array().into_iter().flatten() {
            if let Some(text) = solution.string() {
                if !captured.contains_key(&super::syntax::source_key(text, self.merged.platform)) {
                    self.error(
                        solution.origin.clone(),
                        "solution_files member must be one of the captured files (sources or support_files)",
                        None,
                    );
                }
            }
        }
    }

    fn language_reference(&mut self, node: Option<&EffectiveNode>, input: KeyPath) {
        if marked(self.merged, &[input]) {
            return;
        }
        if let Some(node) = node {
            if let Some(name) = node.string() {
                if self.merged.language(name).is_none()
                    && !self.unknown_definitions()
                    && !marked(self.merged, &[path(&["languages", name])])
                {
                    self.error(
                        node.origin.clone(),
                        format!("unknown language {name:?}"),
                        None,
                    );
                }
            }
        }
    }
}

fn rank(kind: LayerKind) -> u8 {
    match kind {
        LayerKind::BuiltIn => 0,
        LayerKind::DefaultUser | LayerKind::OverrideUser => 1,
        LayerKind::Workspace => 2,
        LayerKind::Problem => 3,
    }
}

fn missing_origin(parent: &EffectiveNode, key: &str) -> Origin {
    let key = toml_edit::Key::new(key).display_repr().into_owned();
    let mut o = parent.origin.clone();
    o.key_path = if o.key_path.is_empty() {
        key
    } else {
        format!("{}.{key}", o.key_path)
    };
    o
}

/// Validate the whole phase-2 universe, including unused and disabled hooks.
/// Does not require a target; use `resolve` when an operation requires one.
pub fn check(merged: &MergedConfig, selection: &Selection) -> CheckReport {
    let mut validator = Validator {
        merged,
        diagnostics: merged.diagnostics.clone(),
    };
    let mut universe = ComposedUniverse {
        check: super::judging::validate(&mut validator, selection),
        ..ComposedUniverse::default()
    };
    if let Some(languages) = merged.languages() {
        for name in languages.keys() {
            let language = merged.language(name).expect("language map member");
            for profile in std::iter::once(None).chain(
                language
                    .profiles()
                    .into_iter()
                    .flat_map(|p| p.keys().map(|p| Some(p.as_str()))),
            ) {
                let target = stages::target(merged, name, profile, None);
                validator.target(&target);
                universe
                    .languages
                    .insert((name.clone(), profile.map(str::to_owned)), target);
            }
        }
    }
    if let Some(commands) = merged.commands() {
        for name in commands.keys() {
            let stage = stages::command(merged, name, false);
            validator.stage(&stage);
            universe.commands.insert(name.clone(), stage);
        }
    }
    if let Some(hooks) = merged.hooks() {
        for (key, list) in [
            ("on_switch", hooks.on_switch()),
            ("after_test", hooks.after_test()),
        ] {
            if let Some(items) = list.and_then(EffectiveNode::array) {
                for (i, item) in items.iter().enumerate() {
                    let index = i.to_string();
                    if marked(merged, &[path(&["hooks", key, &index])]) {
                        continue;
                    }
                    if let Some(name) = item.string() {
                        if merged.command(name).is_some() {
                            if !universe.hooks.contains_key(name) {
                                let stage = stages::command(merged, name, true);
                                validator.stage(&stage);
                                universe.hooks.insert(name.into(), stage);
                            }
                        } else if !validator.unknown_definitions()
                            && !marked(merged, &[path(&["commands", name])])
                        {
                            validator.error(
                                item.origin.clone(),
                                format!("hook names unknown command {name:?}"),
                                None,
                            );
                        }
                    }
                }
            }
        }
    }
    validator.language_reference(merged.default_language(), path(&["default_language"]));
    validator.language_reference(
        merged.problem().and_then(|p| p.language()),
        path(&["config", "language"]),
    );
    let problem = stages::has_problem(merged, selection);
    // An unreadable problem file could select the language (§3.2): leave the
    // target unresolved rather than validate a guess.
    let problem_unknown = problem
        && merged
            .unknown_inputs
            .iter()
            .any(|u| u.layer == LayerKind::Problem);
    let selected_node = if problem {
        merged
            .problem()
            .and_then(|p| p.language())
            .or_else(|| merged.default_language())
    } else {
        merged.default_language()
    };
    let selected_language = if problem_unknown {
        None
    } else if problem {
        selected_node.and_then(EffectiveNode::string)
    } else {
        selection
            .language
            .as_deref()
            .or_else(|| selected_node.and_then(EffectiveNode::string))
    };
    let language_inputs = if problem && merged.problem().and_then(|p| p.language()).is_some() {
        vec![path(&["config", "language"])]
    } else if !problem && selection.language.is_some() {
        vec![]
    } else {
        vec![path(&["default_language"])]
    };
    if let Some(name) = selected_language {
        if let Some(language) = merged.language(name) {
            if let Some(profile) = &selection.profile {
                if language.profile(profile).is_none()
                    && !marked(merged, &language_inputs)
                    && !validator.unknown_definitions()
                    && !marked(merged, &[path(&["languages", name, "profiles", profile])])
                {
                    validator.error(
                        stages::origin("profile", "<cli:--profile>"),
                        format!("unknown profile {profile:?} for language {name:?}"),
                        Some(language.node.origin.clone()),
                    );
                }
            }
            let target =
                stages::target(merged, name, selection.profile.as_deref(), Some(selection));
            if !marked(merged, &language_inputs) {
                validator.target(&target);
            }
            if let Some(limit) = selection.published_time_limit.as_ref().filter(|_| problem) {
                if stages::derived_deadline(limit).is_none()
                    && !marked(merged, &language_inputs)
                    && !target.run.explicitly_set("deadline_s")
                    && !validator.blocked(&target.run, &[&["deadline_s"]])
                    && !validator.unknown_field(&target.run, "deadline_s")
                {
                    validator.error(
                        limit.origin.clone(),
                        "derived run deadline overflows or exceeds 24 hours",
                        None,
                    );
                }
            }
            universe.selected = Some(target);
        } else if selection.language.is_some()
            && !problem
            && !validator.unknown_definitions()
            && !marked(merged, &[path(&["languages", name])])
        {
            validator.error(
                stages::origin("language", "<selection>"),
                format!("unknown language {name:?}"),
                None,
            );
        }
    }
    if let Some(io) = merged
        .at(&["config", "io"])
        .filter(|io| io.markers.is_empty())
    {
        if selection.operation == Operation::Stress {
            validator.error(
                io.origin.clone(),
                "stress with file-based I/O is unsupported in this version",
                None,
            );
        }
        if let Some(target) = &universe.selected {
            validator.file_io_cwd(io, &target.run);
            // A check covers every profile, composed for this problem. The
            // language-only universe above intentionally excludes config.run.
            if selection.operation == Operation::Check {
                let language = merged
                    .language(&target.language)
                    .expect("selected language");
                for profile in language.profiles().into_iter().flat_map(|p| p.keys()) {
                    let mut profile_selection = selection.clone();
                    profile_selection.profile = Some(profile.clone());
                    let target = stages::target(
                        merged,
                        &target.language,
                        Some(profile),
                        Some(&profile_selection),
                    );
                    validator.file_io_cwd(io, &target.run);
                }
            }
        }
    }
    if matches!(selection.operation, Operation::Test | Operation::Run)
        && selection
            .deadline_ms
            .is_some_and(|ms| !(1..=86_400_000).contains(&ms))
    {
        validator.error(
            stages::origin("deadline_s", "<cli:--deadline>"),
            "CLI deadline must be in 1..=86400000 milliseconds",
            None,
        );
    }
    CheckReport {
        diagnostics: validator.diagnostics,
        universe,
    }
}

/// Require a selected target and a clean phase-1/2 universe. Warnings are
/// retained in the successful value for the future consumer's report.
pub fn resolve(
    merged: &MergedConfig,
    selection: &Selection,
) -> Result<ResolvedTarget, Vec<Diagnostic>> {
    let mut report = check(merged, selection);
    if report.executable().is_none() {
        return Err(report.diagnostics);
    }
    if report.universe.selected.is_some() {
        return Ok(ResolvedTarget { report });
    }
    report.diagnostics.push(Diagnostic {
        severity: Severity::Error,
        message: "target requires a language (config.language or default_language)".into(),
        origin: merged.problem().map_or_else(
            || stages::origin("default_language", "<selection>"),
            |p| missing_origin(p.node, "language"),
        ),
        second_origin: None,
    });
    Err(report.diagnostics)
}
