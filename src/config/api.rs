//! Owned, typed consumer plans for phases 1–2.
//!
//! [`load_config`] is the filesystem entry point. Inspection remains available
//! after errors; every plan accessor checks the complete diagnostic report.
//! Plans are declarative: programs, placeholders, source files and output paths
//! still need phase 3, and artifacts still need phase 4.
//!
//! # Consumers
//!
//! C2 takes [`StagePlan`] and [`TextValue`] for resolution and expansion. C4d
//! calls [`TargetPlan::template_destination`] before publishing a new problem.
//! C5 takes the selected language/profile and stages from [`LoadedConfig::target`].
//! C8 takes sources, artifacts and entry from that target. C10 takes
//! [`LoadedConfig::retention`] and [`LoadedConfig::workspace_ignore`]. C12a uses
//! [`LoadedConfig::command`], [`LoadedConfig::hooks`] and their context requirements;
//! the separate raw [`HookSettings`] are inputs to the later trust decision.
//! C13 uses diagnostics and [`LoadedConfig::merged`] for inspection, including
//! histories, explicit fields and ignored modifiers; it need not obtain a plan.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use super::{
    CapPolicy, CheckReport, Comparator, ContextRequirements, DeferredValue, Diagnostic,
    DiscoveryOptions, EffectiveNode, Environment, ExecutableStage, LayerKind, LayerSource,
    LayerState, MergedConfig, Operation, Origin, Platform, Provenance, PublishedLimit, RawLayer,
    RawNode, ResolvedTarget, Selection, Severity, StageHistory, StageKind, Token, TypedValue,
    check, compose, discover, load, resolve,
};

/// Invocation inputs. Paths are native paths; `current_dir` and discovered
/// workspace/problem paths should be absolute. Only `config_path` is rebased
/// against `current_dir`; a relative `config_path` with a `current_dir` that
/// is not absolute for `platform` is a discovery diagnostic. No process
/// environment or current directory is read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigRequest {
    pub environment: Environment,
    pub platform: Platform,
    pub current_dir: PathBuf,
    pub config_path: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    pub problem_file: Option<PathBuf>,
    /// In-memory prospective problem document, parsed at `problem_file` instead
    /// of reading it. Requires a workspace and problem path, just like loading
    /// an existing problem; used before `cpf add` publishes the record.
    pub prospective_problem: Option<String>,
    pub published_limit: Option<PublishedLimit>,
    pub operation: Operation,
    /// Language-only selection; ignored for an existing or prospective problem.
    pub language: Option<String>,
    pub profile: Option<String>,
    /// Already parsed CLI duration, in milliseconds.
    pub deadline_ms: Option<u64>,
}

/// All diagnostics accompanying a refused plan, including any warnings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigErrors {
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index != 0 {
                writeln!(f)?;
            }
            write!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}
impl std::error::Error for ConfigErrors {}

/// An effective leaf with its supplying location and provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct Sourced<T> {
    pub value: T,
    pub origin: Origin,
    pub provenance: Provenance,
    pub overlay: Option<String>,
}

fn sourced<T>(node: &EffectiveNode, value: T) -> Sourced<T> {
    Sourced {
        value,
        origin: node.origin.clone(),
        provenance: node.provenance.clone(),
        overlay: node.overlay.clone(),
    }
}

fn built_in<T>(key: &str, value: T) -> Sourced<T> {
    Sourced {
        value,
        origin: super::stages::origin(key, "<built-in>"),
        provenance: Provenance::BuiltIn,
        overlay: None,
    }
}

/// Parsed text, without phase-3 expansion. Paths and native home prefixes are
/// copied from phase 1 without converting them through UTF-8.
#[derive(Clone, Debug, PartialEq)]
pub struct TextValue {
    /// Phase-1 text for input fingerprints: joined paths and expanded home,
    /// retaining literal placeholders and brace escaping before phase 3.
    pub unexpanded: std::ffi::OsString,
    pub tokens: Vec<Token>,
    pub path: Option<PathBuf>,
    pub lookup: bool,
    pub expanded_home: Option<PathBuf>,
    pub origin: Origin,
    pub provenance: Provenance,
    pub overlay: Option<String>,
}

fn text(node: &EffectiveNode) -> TextValue {
    let Some(TypedValue::Text(parsed)) = &node.typed else {
        unreachable!("validated execution text has parsed tokens")
    };
    TextValue {
        unexpanded: parsed.unexpanded.clone(),
        tokens: parsed.tokens.clone(),
        path: parsed.path.clone(),
        lookup: parsed.lookup,
        expanded_home: parsed.expanded_home.clone(),
        origin: node.origin.clone(),
        provenance: node.provenance.clone(),
        overlay: node.overlay.clone(),
    }
}

/// Literal shell program and its fixed `-c` argument (§5.3). Resolution is C2's
/// responsibility; the interpreter is never expanded for placeholders.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramValue {
    pub program: TextValue,
    pub args: Vec<Sourced<String>>,
}

/// The two explicit command forms; no implicit shell is introduced.
#[derive(Clone, Debug, PartialEq)]
// Keep the owned command fields directly accessible to expansion consumers.
#[allow(clippy::large_enum_variant)]
pub enum StageCommand {
    Argv(Vec<TextValue>),
    Shell {
        interpreter: ProgramValue,
        /// Literal script; braces are not placeholder syntax here.
        script: Sourced<String>,
        args: Vec<TextValue>,
    },
}

/// Symbolic working directory or phase-1 parsed path text.
#[derive(Clone, Debug, PartialEq)]
// Path text carries full provenance; selectors need no allocation.
#[allow(clippy::large_enum_variant)]
pub enum Cwd {
    Run,
    Problem,
    Workspace,
    Path(TextValue),
}

/// Apply in list order after capturing the baseline environment. `name` is
/// the spelling to install, including the winning Windows spelling; on Windows
/// a removal or unset matches baseline names using `env_name_fold_os` (§4.3).
/// Reserved `CPF_*` variables are injected later by the job consumer.
#[derive(Clone, Debug, PartialEq)]
// A set retains separate metadata for the installed name and literal value.
#[allow(clippy::large_enum_variant)]
pub enum EnvEdit {
    Remove(Sourced<String>),
    Unset(Sourced<String>),
    Set {
        name: Sourced<String>,
        /// Literal, never tokenized or expanded.
        value: Sourced<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Deadline {
    pub milliseconds: Sourced<u64>,
    /// A supplied configuration field or CLI override, rather than a default.
    pub explicit: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamCap {
    pub bytes: Sourced<u64>,
    pub policy: CapPolicy,
}

/// Limits from the active platform's composition, in unrounded bytes. Both
/// mechanisms are retained so phase 3 can diagnose unsupported mechanisms;
/// this API neither rounds nor checks platform support.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Limits {
    pub address_space: Option<Sourced<u64>>,
    pub job_commit: Option<Sourced<u64>>,
}

/// Private proof that a plan was built by this module from a clean report;
/// it keeps plans from being assembled from inspectable, unchecked stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Validated;

/// A stage obtained from C1c's executable capability. It still needs C2's
/// context, program resolution and expansion before launch. Only this module
/// constructs plans; their fields are readable and may be adjusted by the
/// planner (e.g. appended `cpf cmd` arguments).
#[derive(Clone, Debug, PartialEq)]
pub struct StagePlan {
    pub kind: StageKind,
    pub command: StageCommand,
    pub cwd: Sourced<Cwd>,
    pub environment: Vec<EnvEdit>,
    pub deadline: Deadline,
    pub stdout: StreamCap,
    pub stderr: StreamCap,
    pub limits: Limits,
    pub context: ContextRequirements,
    pub deferred: Vec<DeferredValue>,
    pub explicit_fields: BTreeSet<String>,
    pub history: StageHistory,
    _validated: Validated,
}

// A conversion cannot accept an inspectable, potentially invalid stage.
#[derive(Clone, Copy, Debug)]
enum ValidatedStage<'a> {
    Build(&'a ResolvedTarget),
    Run(&'a ResolvedTarget),
    Executable(ExecutableStage<'a>),
}

impl StagePlan {
    /// Every cwd except the fresh run directory is shared (§4.2).
    pub fn shared_cwd(&self) -> bool {
        !matches!(self.cwd.value, Cwd::Run)
    }

    fn from_executable(stage: ExecutableStage<'_>) -> Self {
        Self::validated(ValidatedStage::Executable(stage))
    }

    fn validated(stage: ValidatedStage<'_>) -> Self {
        let stage = match stage {
            ValidatedStage::Build(resolved) => {
                resolved.target().build.as_ref().expect("existing build")
            }
            ValidatedStage::Run(resolved) => &resolved.target().run,
            ValidatedStage::Executable(executable) => executable.stage(),
        };
        Self::from_composed(stage)
    }

    fn from_composed(stage: &super::ComposedStage) -> Self {
        let node = &stage.node;
        let command = if let Some(argv) = node.get("argv") {
            StageCommand::Argv(array(argv).iter().map(text).collect())
        } else {
            let shell = field(node, "shell");
            let interpreter = array(field(shell, "interpreter"));
            StageCommand::Shell {
                interpreter: ProgramValue {
                    program: text(&interpreter[0]),
                    args: interpreter[1..].iter().map(string).collect(),
                },
                script: string(field(shell, "script")),
                args: array(field(shell, "args")).iter().map(text).collect(),
            }
        };
        let cwd_node = field(node, "cwd");
        let cwd = if let Some(path) = cwd_node.get("path") {
            sourced(path, Cwd::Path(text(path)))
        } else {
            let selector = field(cwd_node, "selector");
            let value = match selector.string() {
                Some("run") => Cwd::Run,
                Some("problem") => Cwd::Problem,
                Some("workspace") => Cwd::Workspace,
                _ => unreachable!("validated cwd selector"),
            };
            sourced(selector, value)
        };
        let mut environment: Vec<EnvEdit> = node
            .get("env_remove")
            .into_iter()
            .flat_map(array)
            .map(|entry| EnvEdit::Remove(string(entry)))
            .collect();
        if let Some(env) = node.get("env").and_then(EffectiveNode::table) {
            for entry in env.values() {
                let name = sourced(entry, entry.name.clone());
                environment.push(if entry.is_tombstone() {
                    EnvEdit::Unset(name)
                } else {
                    EnvEdit::Set {
                        name,
                        value: string(entry),
                    }
                });
            }
        }
        Self {
            kind: stage.kind,
            command,
            cwd,
            environment,
            deadline: Deadline {
                milliseconds: quantity(field(node, "deadline_s")),
                explicit: stage.explicitly_set("deadline_s"),
            },
            stdout: StreamCap {
                bytes: quantity(field(node, "stdout_cap")),
                policy: stage.stdout_policy,
            },
            stderr: StreamCap {
                bytes: quantity(field(node, "stderr_cap")),
                policy: stage.stderr_policy,
            },
            limits: Limits {
                address_space: node.at(&["limits", "address_space"]).map(quantity),
                job_commit: node.at(&["limits", "job_commit"]).map(quantity),
            },
            context: stage.context,
            deferred: stage.deferred.clone(),
            explicit_fields: stage.explicit_fields.clone(),
            history: stage.history.clone(),
            _validated: Validated,
        }
    }
}

/// Validated problem-relative components. Constructed only from phase-1
/// validated source strings; no host-path parsing or filesystem access occurs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemPath {
    components: Vec<String>,
}

impl ProblemPath {
    pub(crate) fn from_role(file: &Sourced<String>) -> Self {
        role_path(file).value
    }

    /// Ordered UTF-8 components, validated without interpreting host separators.
    pub fn components(&self) -> &[String] {
        &self.components
    }
}

/// The selector that supplied the effective language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageSource {
    Problem,
    DefaultLanguage,
    /// A language-only request, outside a selected problem.
    Selection,
}

/// Declarative output names; file extensions and fresh output roots are
/// handled by the execution consumer. Java's derived required class is included.
#[derive(Clone, Debug, PartialEq)]
pub enum Artifacts {
    File { name: Sourced<String> },
    Dir { required: Vec<Sourced<String>> },
}

/// Effective language settings and stages, obtained only after target resolution.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetPlan {
    pub language: Sourced<String>,
    pub language_source: LanguageSource,
    pub profile: Option<Sourced<String>>,
    pub build: Option<StagePlan>,
    pub run: StagePlan,
    /// Tool inputs; these alone supply `{sources}` and `{source}`.
    pub sources: Vec<Sourced<ProblemPath>>,
    /// Captured companions, excluded from tool inputs. Defaults to an empty list.
    pub support_files: Vec<Sourced<ProblemPath>>,
    /// Effective contestant files. Defaults derive from sources then support
    /// files, with Derived provenance on each member.
    pub solution_files: Vec<Sourced<ProblemPath>>,
    /// Absent for a language without an artifact declaration (e.g. Python)
    /// and for every build-free target, whose artifacts are never checked (§6.3).
    pub artifacts: Option<Artifacts>,
    /// Effective entry: problem `config.entry`, otherwise the language's entry.
    pub entry: Option<Sourced<String>>,
    /// Config-relative path already joined at load, without canonicalisation.
    pub template: Option<Sourced<PathBuf>>,
    pub compare: Sourced<Comparator>,
    pub ignored: StageHistory,
    pub build_history: StageHistory,
    _validated: Validated,
}

/// `cpf add` needs exactly one solution file for its template (§6.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateDestinationError {
    pub solution_file_count: usize,
    pub template_origin: Origin,
}

impl fmt::Display for TemplateDestinationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: template requires exactly one solution file, found {} solution files",
            self.template_origin, self.solution_file_count
        )
    }
}
impl std::error::Error for TemplateDestinationError {}

/// Borrowed template and validated destination, ready for C4d's copy checks.
pub type TemplateDestination<'a> = (&'a Sourced<PathBuf>, &'a Sourced<ProblemPath>);

impl TargetPlan {
    /// Adapt a validated role to the ordinary language build/run planner.
    pub(crate) fn for_role(role: &super::RoleSpec) -> Self {
        let (language, sources, support_files, entry, artifacts, build, run) = match &role.input {
            super::RoleInput::Built(built) => (
                built.language.clone(),
                built.sources.iter().map(role_path).collect(),
                built.support_files.iter().map(role_path).collect(),
                built.entry.clone(),
                built.artifacts.clone(),
                built.build.as_ref().map(StagePlan::from_composed),
                StagePlan::from_composed(&built.run),
            ),
            super::RoleInput::Prebuilt(prebuilt) => (
                Sourced {
                    value: "prebuilt".into(),
                    origin: prebuilt.program.origin.clone(),
                    provenance: prebuilt.program.provenance.clone(),
                    overlay: prebuilt.program.overlay.clone(),
                },
                vec![role_path(&prebuilt.program)],
                vec![],
                None,
                None,
                None,
                StagePlan::from_composed(&prebuilt.run),
            ),
        };
        Self {
            language,
            language_source: LanguageSource::Problem,
            profile: None,
            build,
            run,
            sources,
            support_files,
            solution_files: vec![],
            artifacts,
            entry,
            template: None,
            compare: built_in("compare", Comparator::Tokens),
            ignored: StageHistory::default(),
            build_history: StageHistory::default(),
            _validated: Validated,
        }
    }

    /// Captured file declarations, in sources-then-support order (§6.2).
    pub fn captured_files(&self) -> impl Iterator<Item = &Sourced<ProblemPath>> {
        self.sources.iter().chain(&self.support_files)
    }

    /// Check the add-time solution-file count without reading or copying files.
    pub fn template_destination(
        &self,
    ) -> Result<Option<TemplateDestination<'_>>, TemplateDestinationError> {
        let Some(template) = &self.template else {
            return Ok(None);
        };
        if let [solution] = self.solution_files.as_slice() {
            Ok(Some((template, solution)))
        } else {
            Err(TemplateDestinationError {
                solution_file_count: self.solution_files.len(),
                template_origin: template.origin.clone(),
            })
        }
    }

    fn from_resolved(
        resolved: &ResolvedTarget,
        merged: &MergedConfig,
        selection: &Selection,
    ) -> Self {
        let target = resolved.target();
        let problem_language = merged.problem().and_then(|p| p.language());
        let (language, language_source) = if super::stages::has_problem(merged, selection) {
            if let Some(node) = problem_language {
                (string(node), LanguageSource::Problem)
            } else {
                (
                    string(merged.default_language().expect("resolved language")),
                    LanguageSource::DefaultLanguage,
                )
            }
        } else if selection.language.is_some() {
            (
                Sourced {
                    value: target.language.clone(),
                    origin: super::stages::origin("language", "<selection>"),
                    provenance: Provenance::Cli {
                        flag: "language".into(),
                    },
                    overlay: None,
                },
                LanguageSource::Selection,
            )
        } else {
            (
                string(merged.default_language().expect("resolved language")),
                LanguageSource::DefaultLanguage,
            )
        };
        let settings = &target.settings;
        Self {
            language,
            language_source,
            profile: target.profile.as_ref().map(|value| Sourced {
                value: value.clone(),
                origin: super::stages::origin("profile", "<cli:--profile>"),
                provenance: Provenance::Cli {
                    flag: "--profile".into(),
                },
                overlay: None,
            }),
            build: target
                .build
                .as_ref()
                .map(|_| StagePlan::validated(ValidatedStage::Build(resolved))),
            run: StagePlan::validated(ValidatedStage::Run(resolved)),
            sources: problem_paths(settings.sources.as_ref()),
            support_files: problem_paths(Some(&settings.support_files)),
            solution_files: problem_paths(Some(&settings.solution_files)),
            artifacts: settings
                .artifacts
                .as_ref()
                .filter(|_| target.build.is_some())
                .and_then(role_artifacts),
            entry: settings.entry.as_ref().map(string),
            template: settings.template.as_ref().map(|node| {
                let path = field(node, "path");
                let Some(TypedValue::Path(value)) = &path.typed else {
                    unreachable!("validated template path")
                };
                sourced(path, value.clone())
            }),
            compare: settings.compare.as_ref().map_or_else(
                || built_in("compare", Comparator::Tokens),
                |node| {
                    let Some(TypedValue::Compare(value)) = &node.typed else {
                        unreachable!("validated comparator")
                    };
                    sourced(node, value.clone())
                },
            ),
            ignored: target.ignored.clone(),
            build_history: target.build_history.clone(),
            _validated: Validated,
        }
    }
}

/// Commands receive either null input or the current test's frozen input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stdin {
    Null,
    Test,
}

/// A command with its effective repetition, input and context settings (§8.1).
#[derive(Clone, Debug, PartialEq)]
pub struct CommandPlan {
    pub stage: StagePlan,
    pub needs_build: Sourced<bool>,
    pub for_each_test: Sourced<bool>,
    pub repeat: Sourced<u64>,
    pub keep_going: Sourced<bool>,
    pub stdin: Sourced<Stdin>,
    pub context: ContextRequirements,
    _validated: Validated,
}

impl CommandPlan {
    fn from_executable(executable: ExecutableStage<'_>) -> Self {
        let node = &executable.stage().node;
        let stdin = field(node, "stdin");
        Self {
            stage: StagePlan::from_executable(executable),
            needs_build: boolean(field(node, "needs_build")),
            for_each_test: boolean(field(node, "for_each_test")),
            repeat: quantity(field(node, "repeat")),
            keep_going: boolean(field(node, "keep_going")),
            stdin: sourced(
                stdin,
                match stdin.string() {
                    Some("null") => Stdin::Null,
                    Some("test") => Stdin::Test,
                    _ => unreachable!("validated stdin"),
                },
            ),
            context: executable.stage().context,
            _validated: Validated,
        }
    }
}

/// Raw values read only from the default user file. Absence is preserved;
/// §8.3's enabled default and trust/canonicalisation belong to C12c.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HookSettings {
    pub enabled: Option<Sourced<bool>>,
    pub trusted_workspaces: Option<Vec<Sourced<PathBuf>>>,
    pub user_file_replaced: bool,
}

/// One named hook invocation in the event's configured order.
#[derive(Clone, Debug, PartialEq)]
pub struct HookPlan {
    pub name: Sourced<String>,
    /// Hook-context stage, including its own deadline or the 30-second default.
    pub stage: StagePlan,
}

/// Both hook event lists, independent of enabled/trust decisions.
#[derive(Clone, Debug, PartialEq)]
pub struct HookPlans {
    pub on_switch: Vec<HookPlan>,
    pub after_test: Vec<HookPlan>,
}

/// Retention quantities with §7.3 defaults, without allocating job paths.
#[derive(Clone, Debug, PartialEq)]
pub struct Retention {
    pub keep_jobs: Sourced<u64>,
    pub job_bytes: Sourced<u64>,
    pub workspace_bytes: Sourced<u64>,
    pub cwd_bytes: Sourced<u64>,
    pub build_bytes: Sourced<u64>,
}

/// Inspection plus gated access to owned plans. Errors anywhere in the
/// phase-1/2 universe prevent every plan, even for an unrelated command.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedConfig {
    merged: MergedConfig,
    report: CheckReport,
    selection: Selection,
    diagnostics: Vec<Diagnostic>,
    hook_settings: HookSettings,
}

#[derive(Debug)]
pub(crate) struct InspectionStage {
    pub name: String,
    pub stage: StagePlan,
    pub entry: Option<String>,
}

impl LoadedConfig {
    /// Every phase-1/2 diagnostic, including errors in unused definitions.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Warnings never block a plan.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Inspect even invalid or partial effective values and their histories.
    pub fn merged(&self) -> &MergedConfig {
        &self.merged
    }

    /// Typed judging declarations after all phase-1/2 checks; grants no role execution.
    pub fn judging(&self) -> Result<Option<&super::CheckConfig>, ConfigErrors> {
        self.clean()?;
        Ok(self.report.universe().check.as_deref())
    }

    /// Validated literal file names, with the problem-layer declaration's
    /// provenance. Execution consumers apply these only to test solutions.
    pub fn file_io(&self) -> Result<Option<Sourced<crate::report::FileIo>>, ConfigErrors> {
        self.clean()?;
        Ok(self.merged.at(&["config", "io"]).map(|io| {
            let name = |key| {
                io.get(key).map(|node| {
                    crate::names::PortableComponent::new(node.string().expect("validated io name"))
                        .expect("phase-1 portable component")
                })
            };
            sourced(
                io,
                crate::report::FileIo {
                    input: name("input"),
                    output: name("output"),
                },
            )
        }))
    }

    /// Inspect composed stages and derived defaults without an execution capability.
    pub fn universe(&self) -> &super::ComposedUniverse {
        self.report.universe()
    }

    /// All validated inspection stages, including every profile and the selected
    /// problem. This grants no phase-3 context and performs no tool probes.
    pub(crate) fn inspection_stages(&self) -> Result<Vec<InspectionStage>, ConfigErrors> {
        self.clean()?;
        let mut stages = Vec::new();
        let mut target_stages = |name: String, target: &super::ComposedTarget| {
            let entry = target
                .settings
                .entry
                .as_ref()
                .and_then(EffectiveNode::string)
                .map(str::to_owned);
            if let Some(build) = &target.build {
                stages.push(InspectionStage {
                    name: format!("{name}.build"),
                    stage: StagePlan::from_composed(build),
                    entry: entry.clone(),
                });
            }
            stages.push(InspectionStage {
                name: format!("{name}.run"),
                stage: StagePlan::from_composed(&target.run),
                entry,
            });
        };
        for ((language, profile), target) in &self.universe().languages {
            let name = profile.as_ref().map_or_else(
                || format!("languages.{language}"),
                |p| format!("languages.{language}.profiles.{p}"),
            );
            target_stages(name, target);
        }
        if let Some(target) = &self.universe().selected {
            target_stages("target".into(), target);
        }
        for (name, stage) in &self.universe().commands {
            stages.push(InspectionStage {
                name: format!("commands.{name}"),
                stage: StagePlan::from_composed(stage),
                entry: self
                    .universe()
                    .selected
                    .as_ref()
                    .and_then(|target| target.settings.entry.as_ref())
                    .and_then(EffectiveNode::string)
                    .map(str::to_owned),
            });
        }
        Ok(stages)
    }

    /// Raw default-user trust inputs, available for inspection even after errors.
    pub fn hook_settings(&self) -> &HookSettings {
        &self.hook_settings
    }

    fn clean(&self) -> Result<(), ConfigErrors> {
        if self.has_errors() {
            Err(ConfigErrors {
                diagnostics: self.diagnostics.clone(),
            })
        } else {
            Ok(())
        }
    }

    /// Require a selected target, even if loading for a plain check permitted
    /// an unresolved language. Uses C1c's resolution capability.
    pub fn target(&self) -> Result<TargetPlan, ConfigErrors> {
        self.clean()?;
        let resolved = resolve(&self.merged, &self.selection)
            .map_err(|diagnostics| ConfigErrors { diagnostics })?;
        Ok(TargetPlan::from_resolved(
            &resolved,
            &self.merged,
            &self.selection,
        ))
    }

    /// Require a clean report and a named command; no target is needed here.
    pub fn command(&self, name: &str) -> Result<CommandPlan, ConfigErrors> {
        self.clean()?;
        let executable = self.report.executable().expect("clean report");
        executable
            .command(name)
            .map(CommandPlan::from_executable)
            .ok_or_else(|| self.missing(format!("unknown command {name:?}"), "commands"))
    }

    /// Preserve hook event order, including repeated command references.
    /// Enabled, trust and recursion decisions are deliberately deferred.
    pub fn hooks(&self) -> Result<HookPlans, ConfigErrors> {
        self.clean()?;
        let executable = self.report.executable().expect("clean report");
        let event = |key: &str| -> Vec<HookPlan> {
            self.merged
                .at(&["hooks", key])
                .into_iter()
                .flat_map(array)
                .map(|node| {
                    let name = string(node);
                    let stage = executable
                        .hook(&name.value)
                        .expect("validated hook reference");
                    HookPlan {
                        name,
                        stage: StagePlan::from_executable(stage),
                    }
                })
                .collect()
        };
        Ok(HookPlans {
            on_switch: event("on_switch"),
            after_test: event("after_test"),
        })
    }

    /// Effective retention values with all absent fields supplied by §7.3.
    pub fn retention(&self) -> Result<Retention, ConfigErrors> {
        self.clean()?;
        let value = |key: &str, default: u64| -> Sourced<u64> {
            self.merged
                .at(&["retention", key])
                .map_or_else(|| built_in(&format!("retention.{key}"), default), quantity)
        };
        Ok(Retention {
            keep_jobs: value("keep_jobs", 20),
            job_bytes: value("job_bytes", 1 << 30),
            workspace_bytes: value("workspace_bytes", 4 << 30),
            cwd_bytes: value("cwd_bytes", 256 << 20),
            build_bytes: value("build_bytes", 1 << 30),
        })
    }

    /// Validated, uncompiled patterns; matching and visibility belong to the consumer.
    pub fn workspace_ignore(&self) -> Result<Vec<Sourced<String>>, ConfigErrors> {
        self.clean()?;
        Ok(self
            .merged
            .workspace_ignore()
            .into_iter()
            .flat_map(array)
            .map(string)
            .collect())
    }

    fn missing(&self, message: String, key: &str) -> ConfigErrors {
        let mut diagnostics = self.diagnostics.clone();
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            message,
            origin: super::stages::origin(key, "<selection>"),
            second_origin: None,
        });
        ConfigErrors { diagnostics }
    }
}

/// Discover, load, compose and check every phase-1/2 definition. Test and run
/// requests require a target immediately; other operations may call `target`
/// later (e.g. a build/add consumer), while command-only checks need none.
/// Discovery failures become diagnostics and an unknown user input; known
/// workspace/problem files are still loaded independently.
pub fn load_config(request: &ConfigRequest) -> LoadedConfig {
    crate::storage::crash::barrier("config.load");
    let options = DiscoveryOptions {
        config_override: request.config_path.clone(),
        workspace_root: request.workspace_root.clone(),
        problem_file: request
            .problem_file
            .clone()
            .filter(|_| request.prospective_problem.is_none()),
        // A relative base would silently resolve against the process directory.
        current_dir: Some(request.current_dir.clone())
            .filter(|dir| super::syntax::native_absolute(dir.as_os_str(), request.platform)),
    };
    let mut layers: Vec<RawLayer> = match discover(&request.environment, request.platform, &options)
    {
        Ok(sources) => load(&sources, &request.environment, request.platform),
        Err(error) => {
            let origin = super::stages::origin("", "<discovery>");
            let kind = if request.config_path.is_some() {
                LayerKind::OverrideUser
            } else {
                LayerKind::DefaultUser
            };
            let mut layers = vec![RawLayer {
                kind,
                platform: request.platform,
                file: origin.file.clone(),
                state: LayerState::Invalid(origin.clone()),
                problem_check: None,
                problem_kind: None,
                diagnostics: vec![Diagnostic {
                    severity: Severity::Error,
                    message: error.to_string(),
                    origin,
                    second_origin: None,
                }],
            }];
            let mut sources: Vec<LayerSource> = Vec::new();
            if let Some(root) = &request.workspace_root {
                sources.push(LayerSource {
                    kind: LayerKind::Workspace,
                    file: super::syntax::join(
                        root,
                        std::ffi::OsStr::new("cpforge.toml"),
                        request.platform,
                    ),
                });
                if let Some(file) = &options.problem_file {
                    sources.push(LayerSource {
                        kind: LayerKind::Problem,
                        file: file.clone(),
                    });
                }
            }
            layers.extend(load(&sources, &request.environment, request.platform));
            layers
        }
    };
    if let (Some(_), Some(file), Some(document)) = (
        &request.workspace_root,
        &request.problem_file,
        &request.prospective_problem,
    ) {
        layers.push(super::parse(
            document.as_bytes(),
            &LayerSource {
                kind: LayerKind::Problem,
                file: file.clone(),
            },
            &request.environment,
            request.platform,
        ));
    }
    let hook_settings = raw_hook_settings(&layers, request.config_path.is_some());
    let merged = compose(&layers, request.platform);
    let selection = Selection {
        language: request.language.clone(),
        profile: request.profile.clone(),
        problem: request.workspace_root.is_some() && request.problem_file.is_some(),
        operation: request.operation,
        deadline_ms: request.deadline_ms,
        published_time_limit: request.published_limit.clone(),
    };
    let report = check(&merged, &selection);
    let diagnostics = if matches!(request.operation, Operation::Test | Operation::Run) {
        match resolve(&merged, &selection) {
            Ok(resolved) => resolved.diagnostics().to_vec(),
            Err(diagnostics) => diagnostics,
        }
    } else {
        report.diagnostics().to_vec()
    };
    LoadedConfig {
        merged,
        report,
        selection,
        diagnostics,
        hook_settings,
    }
}

fn raw_hook_settings(layers: &[RawLayer], user_file_replaced: bool) -> HookSettings {
    let mut settings = HookSettings {
        user_file_replaced,
        ..HookSettings::default()
    };
    let Some(layer) = layers
        .iter()
        .find(|layer| layer.kind == LayerKind::DefaultUser)
    else {
        return settings;
    };
    let LayerState::Loaded(root) = &layer.state else {
        return settings;
    };
    let Some(hooks) = root.get("hooks") else {
        return settings;
    };
    // These are raw default-user values, never values from the merged tree.
    if let Some(node) = hooks.get("enabled").filter(|node| !node.invalid) {
        if let super::RawValue::Boolean(value) = node.value {
            settings.enabled = Some(raw_sourced(node, value));
        }
    }
    if let Some(node) = hooks
        .get("trusted_workspaces")
        .filter(|node| !node.has_errors())
    {
        if let super::RawValue::Array(entries) = &node.value {
            settings.trusted_workspaces = Some(
                entries
                    .iter()
                    .map(|entry| {
                        raw_sourced(
                            entry,
                            PathBuf::from(entry.string().expect("validated trusted path")),
                        )
                    })
                    .collect(),
            );
        }
    }
    settings
}

fn raw_sourced<T>(node: &RawNode, value: T) -> Sourced<T> {
    Sourced {
        value,
        origin: node.origin.clone(),
        provenance: Provenance::File {
            path: node.origin.file.clone(),
            line: node.origin.line,
            col: node.origin.col,
        },
        overlay: None,
    }
}

fn field<'a>(node: &'a EffectiveNode, key: &str) -> &'a EffectiveNode {
    node.get(key).expect("validated stage field")
}
fn array(node: &EffectiveNode) -> &[EffectiveNode] {
    node.array().expect("validated array")
}
fn role_path(file: &Sourced<String>) -> Sourced<ProblemPath> {
    Sourced {
        value: ProblemPath {
            components: file.value.split('/').map(str::to_owned).collect(),
        },
        origin: file.origin.clone(),
        provenance: file.provenance.clone(),
        overlay: file.overlay.clone(),
    }
}

pub(crate) fn role_artifacts(node: &EffectiveNode) -> Option<Artifacts> {
    if let Some(file) = node.get("file") {
        file.string()?;
        Some(Artifacts::File { name: string(file) })
    } else {
        let required = node
            .get("required")?
            .array()?
            .iter()
            .map(|file| {
                file.string()?;
                Some(string(file))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Artifacts::Dir { required })
    }
}

fn problem_paths(node: Option<&EffectiveNode>) -> Vec<Sourced<ProblemPath>> {
    node.into_iter()
        .flat_map(array)
        .map(|file| {
            sourced(
                file,
                ProblemPath {
                    components: file
                        .string()
                        .expect("validated problem path")
                        .split('/')
                        .map(str::to_owned)
                        .collect(),
                },
            )
        })
        .collect()
}
fn string(node: &EffectiveNode) -> Sourced<String> {
    sourced(node, node.string().expect("validated string").to_owned())
}
fn boolean(node: &EffectiveNode) -> Sourced<bool> {
    sourced(node, node.boolean().expect("validated boolean"))
}
fn quantity(node: &EffectiveNode) -> Sourced<u64> {
    let value = match node.typed {
        Some(
            TypedValue::Milliseconds(value) | TypedValue::Bytes(value) | TypedValue::Count(value),
        ) => value,
        _ => unreachable!("validated quantity"),
    };
    sourced(node, value)
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
