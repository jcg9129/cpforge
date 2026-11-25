//! Configuration loading, stage composition and phases 1–2, without tool probing.
//!
//! In raw layers, table membership is explicit field presence. Generic tables and overlay
//! sub-tables remain separate; consumers must carry `invalid` markers even when
//! replacing values. `LayerState::Invalid` is a layer-wide unknown-input marker.
//!
//! `discover` accepts an injected environment/platform and an already discovered
//! workspace root; `load` and `parse` return independent `RawLayer`s.
//! Table membership records explicit presence, `RawValue` retains input values,
//! and `TypedValue` holds parsed quantities, paths, templates and comparators.
//! Path normalization is performed only for generic and active overlay values;
//! inactive branches retain their raw paths and structurally validated tokens.
//! `parse` and `load` perform phase 1 only; `check` adds phase 2.
//!
//! Composition: `built_in(platform)` exposes layer 0; `compose(&layers,
//! platform)` includes it automatically, resolves each layer's nested overlays
//! inner-first, and merges in layer order. Parse files for that same platform:
//! inactive paths deliberately have no platform-specific interpretation.
//! `MergedConfig::language`, `Language::profile`, global stages and `problem`
//! keep precedence rows separate. `check(&merged, &Selection)` composes all
//! languages/profiles, commands, named hook commands and the selected problem,
//! then returns all phase-1/2 diagnostics and the inspectable universe. Defaults
//! retain BuiltIn/Derived provenance; `ComposedStage::explicitly_set` distinguishes
//! supplied fields from defaults. Histories have paths relative to each stage;
//! ignored build modifiers retain their document paths on `ComposedTarget`.
//!
//! The consumer API wraps `resolve(&merged, &Selection)` for target operations and uses
//! `CheckReport::executable` for command-only operations. Both capabilities are
//! unavailable on any phase-1/2 error, while warnings remain reportable. Plan
//! from `ResolvedTarget` and from `ExecutableStage` (`ExecutableConfig::command`
//! or `hook`; `ResolvedTarget::executable` gives the same report for the hooks of
//! a target operation), never from a bare `ComposedStage`. The run stage of
//! `Operation::Run` (raw `cpf run`) gets 64 MiB caps on both streams. An unreadable
//! problem file leaves the target unresolved. Pass
//! `Selection::problem` even for a problem with no config table; language-only
//! consumers can set `Selection::language`. Published limits use integer
//! milliseconds; absent includes unlimited. Derived deadlines outside §7.1's
//! range are refused unless a higher row supplies the deadline. Java required
//! artifacts default only when `required` is absent; an explicit empty array
//! stays empty and fails validation. Values retain typed text tokens and origins,
//! and stages expose deferred occurrences and context requirements for C12a.
//! The consumer remains responsible for phase-3 context, source existence,
//! template/solution-file cardinality, tool resolution, expansion and job allocation,
//! and phase-4 outputs. No CLI, filesystem or trust checks occur in `check`.
//!
//! Semantic accessors return `EffectiveNode`s so invalid and partial input
//! remains inspectable. A node exists only where a source supplied it: `get`
//! and `at` returning `None` means absent, and a table holding only overlays
//! that are inactive on the platform is absent like the `<site>.<os>.<key>`
//! spelling, while an explicitly empty table is present. Keys that phase 1
//! rejected (unknown or misplaced) supply no value. Every node carries
//! `provenance`, the supplying `overlay`, parsed `typed` data, `events` and
//! the `markers` that apply to it. `reset_occurred` survives a later build
//! table. Environment `false` values remain tombstones, not absent keys.
//! `MergedConfig::history` also retains events for removed keys; paths are
//! vectors of literal key segments; array indices are decimal segments.
//! Windows environment maps and history paths use `env_name_fold`, while each
//! node's `name` keeps the spelling of the source that set it
//! (`Stage::env_entries`). `MergedConfig::markers` holds every marker; markers
//! from inactive or forbidden branches (`applies == false`) are attached to no
//! node, and a rejected key's marker only to its containers. `unknown_inputs`
//! names each unreadable layer. Consumers must consult markers and unknown
//! inputs before dependent checks.
//!
//! Non-float selectors are written `{ exact = true }`, `{ tokens = true }` or
//! `{ lines = true }` (compare spec §5.1).
//! Configuration names follow the configuration contract's letter-first rule,
//! rather than the broader filesystem `Identifier` policy in `crate::names`.
//!
//! # Consumers
//!
//! [`load_config`] returns diagnostics and inspection even after errors. C2
//! expands its typed stage plans; C4d uses template destinations; C5 selects
//! targets; C8 uses source/artifact declarations; C10 reads retention and ignore
//! patterns; C12a plans commands/hooks; C13 inspects merged values and histories.
//! Targets expose sources (tool inputs), support files and effective solution
//! files. Captured files are sources followed by support files; only sources
//! expand into tool arguments. The effective entry comes from the problem or
//! language, including Java's derived required artifact paths.
//! See [`api`] for the owned plans. The typed [`RetentionPlan`] is also available
//! as `api::Retention`; [`Retention`] remains the lower-level composition view.

pub mod api;
mod built_in;
mod check;
mod compose;
mod env;
mod judging;
mod load;
mod stages;
pub(crate) mod syntax;
mod validate;

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::path::PathBuf;

pub use api::{
    Artifacts, CommandPlan, ConfigErrors, ConfigRequest, Cwd, Deadline, EnvEdit, HookPlan,
    HookPlans, HookSettings, LanguageSource, Limits, LoadedConfig, ProblemPath, ProgramValue,
    Retention as RetentionPlan, Sourced, StageCommand, StagePlan, Stdin, StreamCap, TargetPlan,
    TemplateDestination, TemplateDestinationError, TextValue, load_config,
};
pub use built_in::built_in;
pub use check::{
    CheckReport, ComposedUniverse, ExecutableConfig, ExecutableStage, ResolvedTarget, check,
    resolve,
};
pub use compose::{
    CommandChoice, EffectiveNode, EffectiveValue, EventKind, Hooks, InvalidMarker, Language,
    MergedConfig, NodeKind, Problem, Profile, Provenance, ProvenanceEvent, ResetMode, Retention,
    Stage, UnknownInput, ValueSnapshot, compose,
};
pub use env::{env_name_fold, env_name_fold_os};
pub use judging::*;
pub use load::{DiscoveryError, DiscoveryOptions, Environment, LayerSource, discover, load, parse};
pub use load::{ProblemTables, parse_problem_tables};
pub use stages::{
    CapPolicy, ComposedStage, ComposedTarget, ContextRequirements, DeferredValue, Operation,
    PublishedLimit, Selection, StageHistory, StageKind, TargetSettings,
};
pub use syntax::{Placeholder, Token, tokenize};
pub use validate::parse_deadline;
pub use validate::surfaces::{
    CHECK_DEFAULTS, INTERACTIVE_DEFAULTS, INTERACTOR_SEED_DEFAULTS, INTERACTOR_TEST_DEFAULTS,
    ROLE_DEFAULTS, SCORING_DEFAULTS, STRESS_DEFAULTS, SUBMIT_STAGE_DEFAULTS, WORKFLOW_DEFAULTS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Default for Platform {
    fn default() -> Self {
        Self::host()
    }
}

impl Platform {
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    pub fn overlay_active(self, name: &str) -> bool {
        matches!(
            (self, name),
            (Self::Linux | Self::Macos, "unix")
                | (Self::Linux, "linux")
                | (Self::Macos, "macos")
                | (Self::Windows, "windows")
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    BuiltIn,
    DefaultUser,
    OverrideUser,
    Workspace,
    Problem,
}

/// File provenance, with 1-based byte columns and the original source range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub file: PathBuf,
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub key_path: String,
    pub span: Option<Range<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub origin: Origin,
    pub second_origin: Option<Origin>,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file.display())?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
            if let Some(col) = self.col {
                write!(f, ":{col}")?;
            }
        }
        if !self.key_path.is_empty() {
            write!(f, " [{}]", self.key_path)?;
        }
        Ok(())
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{}: {severity}: {}", self.origin, self.message)?;
        if let Some(other) = &self.second_origin {
            write!(f, " (conflicts with {other})")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawLayer {
    pub kind: LayerKind,
    pub platform: Platform,
    pub file: PathBuf,
    pub state: LayerState,
    /// Record-owned judging declarations, read only from local problem.toml.
    pub problem_check: Option<Box<RawNode>>,
    pub problem_kind: Option<Box<RawNode>>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerState {
    /// Missing optional file or problem document with no config table.
    Empty,
    Loaded(RawNode),
    /// Unknown input: later phases cannot infer absent definitions from this layer.
    Invalid(Origin),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawNode {
    pub origin: Origin,
    pub value: RawValue,
    /// Error at this key/container; descendant errors stay on their own nodes.
    pub invalid: bool,
    /// Phase-1 interpretation, retaining `value` exactly as written.
    pub typed: Option<TypedValue>,
    /// Set on a `unix`/`linux`/`macos`/`windows` child that phase 1 read as a
    /// platform overlay: `Site` where §2 allows one (compose it), `Forbidden`
    /// for a nested or misplaced one (already reported; never compose it).
    /// `None` for ordinary keys, e.g. a command or env name `linux`.
    pub overlay: Option<OverlayRole>,
    /// Phase 1 rejected the key itself (unknown, or not allowed in this layer
    /// or table). Its diagnostic and marker remain; it supplies no effective
    /// value.
    pub rejected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayRole {
    Site,
    Forbidden,
}

impl RawNode {
    pub fn table(&self) -> Option<&BTreeMap<String, RawNode>> {
        match &self.value {
            RawValue::Table(table) => Some(table),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Self> {
        self.table()?.get(key)
    }

    pub fn string(&self) -> Option<&str> {
        match &self.value {
            RawValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn has_errors(&self) -> bool {
        self.invalid
            || match &self.value {
                RawValue::Table(t) => t.values().any(Self::has_errors),
                RawValue::Array(a) => a.iter().any(Self::has_errors),
                _ => false,
            }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RawValue {
    Table(BTreeMap<String, RawNode>),
    Array(Vec<RawNode>),
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Datetime(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypedValue {
    Milliseconds(u64),
    Bytes(u64),
    Count(u64),
    Path(PathBuf),
    Text(ParsedText),
    Compare(Comparator),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedText {
    /// Phase-1 spelling, including lexical path joining and home expansion,
    /// before placeholder expansion or executable resolution.
    pub unexpanded: std::ffi::OsString,
    pub tokens: Vec<Token>,
    /// Expanded home and lexical config-relative path, when resolved at load.
    /// Placeholder-led and absolute placeholder paths remain in `tokens`.
    pub path: Option<PathBuf>,
    pub lookup: bool,
    /// Captured native home prefix for a deferred `~/...{placeholder}` path.
    /// Replace the leading `~/` (or Windows `~\`) literal during expansion;
    /// never tokenize or expand the native home itself.
    pub expanded_home: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Comparator {
    Exact,
    Tokens,
    Lines,
    Float(FloatComparator),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FloatComparator {
    pub abs: Option<f64>,
    pub rel: Option<f64>,
    pub ulps: Option<u64>,
    pub rel_base: RelativeBase,
    pub combine: Combine,
    pub int_exact: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativeBase {
    Answer,
    Max,
    Max1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Either,
    Both,
}

pub(crate) use syntax::ignore_valid;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod compose_tests;

#[cfg(test)]
mod check_tests;

#[cfg(test)]
mod surface_tests;

#[cfg(test)]
mod judging_tests;
