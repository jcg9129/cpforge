use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::{
    Diagnostic, LayerKind, LayerState, Origin, OverlayRole, Platform, RawLayer, RawNode, RawValue,
    Severity, TypedValue, built_in,
};

type KeyPath = Vec<String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provenance {
    File {
        path: PathBuf,
        line: Option<usize>,
        col: Option<usize>,
    },
    BuiltIn,
    Cli {
        flag: String,
    },
    Environment {
        name: String,
    },
    Derived {
        inputs: Vec<Provenance>,
    },
}

fn provenance(origin: &Origin, layer: LayerKind) -> Provenance {
    if layer == LayerKind::BuiltIn {
        Provenance::BuiltIn
    } else {
        Provenance::File {
            path: origin.file.clone(),
            line: origin.line,
            col: origin.col,
        }
    }
}

/// Each kind has one meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A value supplied where nothing was inherited (or merged into a table).
    Set,
    /// A value that replaced an inherited value; `removed` holds the latter.
    Replace,
    /// An environment tombstone (`NAME = false`).
    Delete,
    /// `build = false`; `removed` holds the discarded stage.
    Reset,
    /// A value removed by a later replacement, reset or argv/shell switch,
    /// recorded at the removed value's own path.
    Discarded,
    /// A contribution present but not applied (§2.1: a modifier row on a
    /// build-free target). Reserved for row composition; never produced here.
    Ignored,
}

/// A value at the time of an event, including origins of all descendants.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueSnapshot {
    pub name: String,
    pub value: EffectiveValue,
    pub typed: Option<TypedValue>,
    pub origin: Origin,
    pub provenance: Provenance,
    pub overlay: Option<String>,
    pub reset_occurred: bool,
    replace_as_unit: bool,
}

impl ValueSnapshot {
    fn to_node(&self, kind: NodeKind, order: usize) -> EffectiveNode {
        EffectiveNode {
            name: self.name.clone(),
            value: self.value.clone(),
            typed: self.typed.clone(),
            origin: self.origin.clone(),
            provenance: self.provenance.clone(),
            overlay: self.overlay.clone(),
            reset_occurred: self.reset_occurred,
            markers: vec![],
            events: vec![],
            kind,
            order,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProvenanceEvent {
    pub kind: EventKind,
    pub provenance: Provenance,
    pub origin: Origin,
    /// Original document path of the supplying overlay, including its site.
    pub overlay: Option<String>,
    pub value: Option<ValueSnapshot>,
    pub removed: Vec<ValueSnapshot>,
    // Relative order inside a layer, including inner-first overlay resolution.
    order: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidMarker {
    /// Effective path, with overlay segments removed. Array indices are decimal
    /// segments; Windows environment names are ASCII lowercase.
    pub path: Vec<String>,
    pub origin: Origin,
    pub provenance: Provenance,
    pub overlay: Option<String>,
    pub layer: LayerKind,
    /// False for a marker inside an overlay branch that is inactive on the
    /// validation platform or forbidden. Such a marker is kept for reporting
    /// but is attached to no effective node: it is not an input there.
    pub applies: bool,
    /// The key itself was rejected (unknown or misplaced) and supplied no
    /// value. Its marker is attached to the containers above `path` only,
    /// never to effective values at or below it, which come from elsewhere.
    pub rejected_key: bool,
}

impl InvalidMarker {
    fn attaches_to(&self, path: &[String]) -> bool {
        if !self.applies {
            false
        } else if self.rejected_key {
            self.path.len() > path.len() && self.path.starts_with(path)
        } else {
            self.path.starts_with(path) || path.starts_with(&self.path)
        }
    }
}

/// A layer that could not be read at all (§3.2 unknown-input marker).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownInput {
    pub layer: LayerKind,
    pub origin: Origin,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EffectiveValue {
    Table(BTreeMap<String, EffectiveNode>),
    Array(Vec<EffectiveNode>),
    /// A scalar RawValue; tables and arrays use the variants above.
    Scalar(RawValue),
}

/// An effective value. A node exists only where some source supplied it:
/// `get`/`at` returning `None` means absent; an explicitly written empty table
/// is a present node with no entries.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveNode {
    /// The key as written by the source that supplied this node (array
    /// elements: the decimal index). On Windows, environment maps are keyed by
    /// the ASCII-lowercase name while `name` keeps the winning spelling.
    pub name: String,
    pub value: EffectiveValue,
    pub typed: Option<TypedValue>,
    pub origin: Origin,
    pub provenance: Provenance,
    pub overlay: Option<String>,
    /// A build reset remains significant even after a later table reintroduces it.
    pub reset_occurred: bool,
    pub markers: Vec<InvalidMarker>,
    pub events: Vec<ProvenanceEvent>,
    kind: NodeKind,
    order: usize,
}

/// How `EffectiveNode::merge_from` treats a build reset in the higher node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    /// A higher node with `reset_occurred` replaces the lower one whole
    /// (layers within one precedence row, and row 3 under row 4, §2.1).
    Discard,
    /// Tables merge key by key regardless of resets; the flag is kept
    /// (composing the surviving stage with modifier rows 1, 2, 5 and 6).
    Preserve,
}

impl EffectiveNode {
    /// A node supplied outside the configuration files, e.g. a CLI flag, a
    /// row-1 default or a derived value. `kind` selects the merge rules.
    pub fn new(
        name: impl Into<String>,
        kind: NodeKind,
        value: EffectiveValue,
        typed: Option<TypedValue>,
        provenance: Provenance,
        origin: Origin,
    ) -> Self {
        Self {
            name: name.into(),
            value,
            typed,
            origin,
            provenance,
            overlay: None,
            reset_occurred: false,
            markers: vec![],
            events: vec![],
            kind,
            order: 0,
        }
    }
    pub fn kind(&self) -> NodeKind {
        self.kind
    }
    pub fn table(&self) -> Option<&BTreeMap<String, Self>> {
        if let EffectiveValue::Table(table) = &self.value {
            Some(table)
        } else {
            None
        }
    }
    pub fn array(&self) -> Option<&[Self]> {
        if let EffectiveValue::Array(array) = &self.value {
            Some(array)
        } else {
            None
        }
    }
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.table()?.get(key)
    }
    pub fn at(&self, path: &[&str]) -> Option<&Self> {
        path.iter().try_fold(self, |node, key| node.get(key))
    }
    pub fn string(&self) -> Option<&str> {
        if let EffectiveValue::Scalar(RawValue::String(s)) = &self.value {
            Some(s)
        } else {
            None
        }
    }
    pub fn boolean(&self) -> Option<bool> {
        if let EffectiveValue::Scalar(RawValue::Boolean(b)) = self.value {
            Some(b)
        } else {
            None
        }
    }
    pub fn is_tombstone(&self) -> bool {
        self.kind == NodeKind::EnvValue && self.boolean() == Some(false)
    }
    /// Composes `higher` over `self` with the §2 rules used across layers:
    /// replace-as-unit nodes, argv/shell switching, tombstones replaced by a
    /// later value, and (per `resets`) build resets. Both nodes must describe
    /// the same kind of setting, e.g. two stages of different precedence
    /// rows. `path` is the document path reported for `self`. The result holds
    /// a `Discarded` event for each value of `self` removed without a
    /// counterpart in `higher` (by an argv/shell switch, a reset, or a unit
    /// replacement lacking that entry), keyed by `path` plus its relative key
    /// segments. Values that `higher` replaces directly are not recorded: the
    /// caller records that replacement. Events and markers already on the
    /// nodes are kept as they are; the merged node takes `higher`'s name,
    /// kind and origin.
    pub fn merge_from(
        &mut self,
        higher: Self,
        path: &[String],
        resets: ResetMode,
    ) -> BTreeMap<Vec<String>, Vec<ProvenanceEvent>> {
        let mut discarded = BTreeMap::new();
        merge_node(self, higher, path, resets, &mut discarded);
        discarded
    }
    fn snapshot(&self) -> ValueSnapshot {
        ValueSnapshot {
            name: self.name.clone(),
            value: self.value.clone(),
            typed: self.typed.clone(),
            origin: self.origin.clone(),
            provenance: self.provenance.clone(),
            overlay: self.overlay.clone(),
            reset_occurred: self.reset_occurred,
            replace_as_unit: self.kind.atomic(),
        }
    }
    pub(super) fn event(&self, kind: EventKind) -> ProvenanceEvent {
        ProvenanceEvent {
            kind,
            provenance: self.provenance.clone(),
            origin: self.origin.clone(),
            overlay: self.overlay.clone(),
            value: Some(self.snapshot()),
            removed: vec![],
            order: self.order,
        }
    }
}

/// Rows remain at their document paths; only the problem row has a `config` prefix.
#[derive(Clone, Debug, PartialEq)]
pub struct MergedConfig {
    pub platform: Platform,
    pub root: EffectiveNode,
    pub diagnostics: Vec<Diagnostic>,
    /// Every marker, including those that apply to no effective node.
    pub markers: Vec<InvalidMarker>,
    pub unknown_inputs: Vec<UnknownInput>,
    /// Includes deleted/replaced descendants no longer in the effective tree.
    pub history: BTreeMap<Vec<String>, Vec<ProvenanceEvent>>,
    /// Supplying file ranks for phase-2 unknown-input dependency analysis.
    pub(super) layer_files: BTreeMap<PathBuf, LayerKind>,
}

impl MergedConfig {
    pub fn at(&self, path: &[&str]) -> Option<&EffectiveNode> {
        self.root.at(path)
    }
    pub fn languages(&self) -> Option<&BTreeMap<String, EffectiveNode>> {
        self.at(&["languages"])?.table()
    }
    pub fn language(&self, name: &str) -> Option<Language<'_>> {
        Some(Language {
            node: self.at(&["languages", name])?,
        })
    }
    pub fn commands(&self) -> Option<&BTreeMap<String, EffectiveNode>> {
        self.at(&["commands"])?.table()
    }
    pub fn command(&self, name: &str) -> Option<Stage<'_>> {
        Some(Stage {
            node: self.at(&["commands", name])?,
        })
    }
    pub fn build(&self) -> Option<Stage<'_>> {
        Some(Stage {
            node: self.at(&["build"])?,
        })
    }
    pub fn run(&self) -> Option<Stage<'_>> {
        Some(Stage {
            node: self.at(&["run"])?,
        })
    }
    pub fn problem(&self) -> Option<Problem<'_>> {
        Some(Problem {
            node: self.at(&["config"])?,
        })
    }
    pub fn retention(&self) -> Option<Retention<'_>> {
        Some(Retention {
            node: self.at(&["retention"])?,
        })
    }
    pub fn workspace_ignore(&self) -> Option<&EffectiveNode> {
        self.at(&["workspace", "ignore"])
    }
    pub fn hooks(&self) -> Option<Hooks<'_>> {
        Some(Hooks {
            node: self.at(&["hooks"])?,
        })
    }
    pub fn compare(&self) -> Option<&EffectiveNode> {
        self.at(&["compare"])
    }
    pub fn default_language(&self) -> Option<&EffectiveNode> {
        self.at(&["default_language"])
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Retention<'a> {
    pub node: &'a EffectiveNode,
}
impl<'a> Retention<'a> {
    pub fn keep_jobs(self) -> Option<&'a EffectiveNode> {
        self.node.get("keep_jobs")
    }
    pub fn job_bytes(self) -> Option<&'a EffectiveNode> {
        self.node.get("job_bytes")
    }
    pub fn workspace_bytes(self) -> Option<&'a EffectiveNode> {
        self.node.get("workspace_bytes")
    }
    pub fn cwd_bytes(self) -> Option<&'a EffectiveNode> {
        self.node.get("cwd_bytes")
    }
    pub fn build_bytes(self) -> Option<&'a EffectiveNode> {
        self.node.get("build_bytes")
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Hooks<'a> {
    pub node: &'a EffectiveNode,
}
impl<'a> Hooks<'a> {
    pub fn on_switch(self) -> Option<&'a EffectiveNode> {
        self.node.get("on_switch")
    }
    pub fn after_test(self) -> Option<&'a EffectiveNode> {
        self.node.get("after_test")
    }
    pub fn enabled(self) -> Option<&'a EffectiveNode> {
        self.node.get("enabled")
    }
    pub fn trusted_workspaces(self) -> Option<&'a EffectiveNode> {
        self.node.get("trusted_workspaces")
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Language<'a> {
    pub node: &'a EffectiveNode,
}
impl<'a> Language<'a> {
    pub fn build(self) -> Option<Stage<'a>> {
        Some(Stage {
            node: self.node.get("build")?,
        })
    }
    pub fn run(self) -> Option<Stage<'a>> {
        Some(Stage {
            node: self.node.get("run")?,
        })
    }
    pub fn profiles(self) -> Option<&'a BTreeMap<String, EffectiveNode>> {
        self.node.get("profiles")?.table()
    }
    pub fn profile(self, name: &str) -> Option<Profile<'a>> {
        Some(Profile {
            node: self.node.at(&["profiles", name])?,
        })
    }
    pub fn sources(self) -> Option<&'a EffectiveNode> {
        self.node.get("sources")
    }
    pub fn artifacts(self) -> Option<&'a EffectiveNode> {
        self.node.get("artifacts")
    }
    pub fn template(self) -> Option<&'a EffectiveNode> {
        self.node.get("template")
    }
    pub fn entry(self) -> Option<&'a EffectiveNode> {
        self.node.get("entry")
    }
    pub fn extensions(self) -> Option<&'a EffectiveNode> {
        self.node.get("extensions")
    }
}

/// A profile contribution, kept separate from the language's own stages.
#[derive(Clone, Copy, Debug)]
pub struct Profile<'a> {
    pub node: &'a EffectiveNode,
}
impl<'a> Profile<'a> {
    pub fn build(self) -> Option<Stage<'a>> {
        Some(Stage {
            node: self.node.get("build")?,
        })
    }
    pub fn run(self) -> Option<Stage<'a>> {
        Some(Stage {
            node: self.node.get("run")?,
        })
    }
}

/// The problem row, distinct from all workspace and language settings.
#[derive(Clone, Copy, Debug)]
pub struct Problem<'a> {
    pub node: &'a EffectiveNode,
}
impl<'a> Problem<'a> {
    pub fn build(self) -> Option<Stage<'a>> {
        Some(Stage {
            node: self.node.get("build")?,
        })
    }
    pub fn run(self) -> Option<Stage<'a>> {
        Some(Stage {
            node: self.node.get("run")?,
        })
    }
    pub fn language(self) -> Option<&'a EffectiveNode> {
        self.node.get("language")
    }
    pub fn sources(self) -> Option<&'a EffectiveNode> {
        self.node.get("sources")
    }
    pub fn compare(self) -> Option<&'a EffectiveNode> {
        self.node.get("compare")
    }
}

/// A stage or command table. The command-only accessors return `None` for
/// other stages (phase 1 rejects those keys there).
#[derive(Clone, Copy, Debug)]
pub struct Stage<'a> {
    pub node: &'a EffectiveNode,
}
#[derive(Clone, Copy, Debug)]
pub enum CommandChoice<'a> {
    Argv(&'a EffectiveNode),
    Shell(&'a EffectiveNode),
}
impl<'a> Stage<'a> {
    /// None for absent or conflicting forms; consult markers before completeness.
    pub fn command(self) -> Option<CommandChoice<'a>> {
        match (self.node.get("argv"), self.node.get("shell")) {
            (Some(argv), None) => Some(CommandChoice::Argv(argv)),
            (None, Some(shell)) => Some(CommandChoice::Shell(shell)),
            _ => None,
        }
    }
    pub fn argv(self) -> Option<&'a EffectiveNode> {
        self.node.get("argv")
    }
    pub fn shell(self) -> Option<&'a EffectiveNode> {
        self.node.get("shell")
    }
    pub fn deadline_s(self) -> Option<&'a EffectiveNode> {
        self.node.get("deadline_s")
    }
    pub fn stdout_cap(self) -> Option<&'a EffectiveNode> {
        self.node.get("stdout_cap")
    }
    pub fn stderr_cap(self) -> Option<&'a EffectiveNode> {
        self.node.get("stderr_cap")
    }
    pub fn env_remove(self) -> Option<&'a EffectiveNode> {
        self.node.get("env_remove")
    }
    pub fn cwd(self) -> Option<&'a EffectiveNode> {
        self.node.get("cwd")
    }
    /// Keyed by the ASCII-lowercase name on Windows and by the name itself
    /// elsewhere; see `env_entries` for the spelling to install.
    pub fn env(self) -> Option<&'a BTreeMap<String, EffectiveNode>> {
        self.node.get("env")?.table()
    }
    /// Environment entries with the spelling of the source that set each one.
    /// Tombstones are included (`EffectiveNode::is_tombstone`).
    pub fn env_entries(self) -> impl Iterator<Item = (&'a str, &'a EffectiveNode)> {
        self.env()
            .into_iter()
            .flat_map(BTreeMap::values)
            .map(|node| (node.name.as_str(), node))
    }
    pub fn limits(self) -> Option<&'a EffectiveNode> {
        self.node.get("limits")
    }
    pub fn needs_build(self) -> Option<&'a EffectiveNode> {
        self.node.get("needs_build")
    }
    pub fn for_each_test(self) -> Option<&'a EffectiveNode> {
        self.node.get("for_each_test")
    }
    pub fn repeat(self) -> Option<&'a EffectiveNode> {
        self.node.get("repeat")
    }
    pub fn keep_going(self) -> Option<&'a EffectiveNode> {
        self.node.get("keep_going")
    }
    pub fn stdin(self) -> Option<&'a EffectiveNode> {
        self.node.get("stdin")
    }
}

/// The schema position of a node, which selects its merge rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// The document root of a layer.
    Root,
    /// The problem row `config`.
    Problem,
    Languages,
    Language,
    Profiles,
    Profile,
    Commands,
    /// A stage or command table: argv and shell replace each other.
    Stage,
    /// A language or profile `build`: also a stage; `false` is a reset.
    Build,
    /// An `env` map.
    Env,
    /// An `env` value; `false` is a tombstone.
    EnvValue,
    /// Replaced as a unit (arrays aside, which always are).
    Atomic,
    /// `cwd`, replaced as a unit.
    Cwd,
    /// `artifacts`, replaced as a unit.
    Artifacts,
    /// Any other value; tables merge key by key, scalars and arrays replace.
    Other,
}
impl NodeKind {
    fn atomic(self) -> bool {
        matches!(self, Self::Atomic | Self::Cwd | Self::Artifacts)
    }
    fn stage(self) -> bool {
        matches!(self, Self::Stage | Self::Build)
    }
    fn child(self, key: &str) -> Self {
        match (self, key) {
            (Self::Root, "languages") => Self::Languages,
            (Self::Root, "commands") => Self::Commands,
            (Self::Root, "config") => Self::Problem,
            (Self::Root | Self::Problem, "build" | "run") => Self::Stage,
            (Self::Root | Self::Problem, "compare") => Self::Atomic,
            (Self::Languages, _) => Self::Language,
            (Self::Language, "profiles") => Self::Profiles,
            (Self::Profiles, _) => Self::Profile,
            (Self::Language | Self::Profile, "build") => Self::Build,
            (Self::Language | Self::Profile, "run") | (Self::Commands, _) => Self::Stage,
            (Self::Language, "artifacts") => Self::Artifacts,
            (Self::Language, "template") => Self::Atomic,
            (Self::Stage | Self::Build, "env") => Self::Env,
            (Self::Stage | Self::Build, "cwd") => Self::Cwd,
            (Self::Stage | Self::Build, "argv" | "shell" | "limits" | "env_remove") => Self::Atomic,
            (Self::Env, _) => Self::EnvValue,
            _ => Self::Other,
        }
    }
}

#[derive(Debug)]
struct Tree {
    root: EffectiveNode,
    history: BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
}

/// Per-layer composition context.
#[derive(Clone, Copy)]
struct Context<'a> {
    layer: LayerKind,
    platform: Platform,
    /// Document path of the overlay branch being composed, if any.
    overlay: Option<&'a str>,
}

fn child_path(path: &[String], key: &str) -> KeyPath {
    let mut result = path.to_vec();
    result.push(key.into());
    result
}

/// Windows environment names compare ASCII case-insensitively (§4.3).
fn map_key(kind: NodeKind, platform: Platform, key: &str) -> String {
    if kind == NodeKind::Env && platform == Platform::Windows {
        key.to_ascii_lowercase()
    } else {
        key.to_owned()
    }
}

fn os_overlay(platform: Platform) -> &'static str {
    match platform {
        Platform::Linux => "linux",
        Platform::Macos => "macos",
        Platform::Windows => "windows",
    }
}

fn node_from_raw(
    raw: &RawNode,
    kind: NodeKind,
    ctx: Context<'_>,
    name: &str,
    order: usize,
) -> EffectiveNode {
    EffectiveNode {
        name: name.to_owned(),
        value: EffectiveValue::Scalar(raw.value.clone()),
        typed: raw.typed.clone(),
        origin: raw.origin.clone(),
        provenance: provenance(&raw.origin, ctx.layer),
        overlay: ctx.overlay.map(str::to_owned),
        reset_occurred: kind == NodeKind::Build && matches!(raw.value, RawValue::Boolean(false)),
        markers: vec![],
        events: vec![],
        kind,
        order,
    }
}

/// Composes one raw node of a layer for the validation platform: generic
/// fields, then `unix`, then the OS overlay, each overlay merged over the
/// node's own result after its children's overlays (inner-first, §2).
///
/// Returns `None` when the node contributes nothing on this platform: a
/// rejected key, or a non-empty table none of whose entries contributes
/// (only inactive or forbidden overlays, rejected keys, or such tables). An
/// explicitly written empty table is present.
fn compose_node(
    raw: &RawNode,
    kind: NodeKind,
    ctx: Context<'_>,
    path: &[String],
    name: &str,
    order: &mut usize,
) -> Option<Tree> {
    if raw.rejected {
        return None;
    }
    let mut root = node_from_raw(raw, kind, ctx, name, *order);
    *order += 1;
    let mut history = BTreeMap::new();
    let mut contributes = true;
    match &raw.value {
        RawValue::Table(table) => {
            let mut values = BTreeMap::new();
            for (key, child) in table.iter().filter(|(_, n)| n.overlay.is_none()) {
                let folded = map_key(kind, ctx.platform, key);
                let Some(child) = compose_node(
                    child,
                    kind.child(&folded),
                    ctx,
                    &child_path(path, &folded),
                    key,
                    order,
                ) else {
                    continue;
                };
                values.insert(folded, child.root);
                append_history(&mut history, child.history);
            }
            contributes = table.is_empty() || !values.is_empty();
            root.value = EffectiveValue::Table(values);
        }
        RawValue::Array(array) => {
            let mut values = vec![];
            for (index, child) in array.iter().enumerate() {
                let index = index.to_string();
                if let Some(child) = compose_node(
                    child,
                    NodeKind::Other,
                    ctx,
                    &child_path(path, &index),
                    &index,
                    order,
                ) {
                    values.push(child.root);
                    append_history(&mut history, child.history);
                }
            }
            root.value = EffectiveValue::Array(values);
        }
        _ => {}
    }
    let event_kind = if root.reset_occurred {
        EventKind::Reset
    } else if root.is_tombstone() {
        EventKind::Delete
    } else {
        EventKind::Set
    };
    history.insert(path.to_vec(), vec![root.event(event_kind)]);
    let mut result = Tree { root, history };
    if let Some(table) = raw.table() {
        for key in ["unix", os_overlay(ctx.platform)] {
            if !ctx.platform.overlay_active(key) {
                continue;
            }
            let Some(branch) = table
                .get(key)
                .filter(|n| n.overlay == Some(OverlayRole::Site))
            else {
                continue;
            };
            let branch_ctx = Context {
                layer: ctx.layer,
                platform: ctx.platform,
                overlay: Some(branch.origin.key_path.as_str()),
            };
            if let Some(incoming) = compose_node(branch, kind, branch_ctx, path, name, order) {
                merge_tree(&mut result, incoming, path);
                contributes = true;
            }
        }
    }
    contributes.then_some(result)
}

fn append_history(
    base: &mut BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
    incoming: BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
) {
    for (path, events) in incoming {
        base.entry(path).or_default().extend(events);
    }
}

fn lookup<'a>(node: &'a EffectiveNode, path: &[String]) -> Option<&'a EffectiveNode> {
    path.iter().try_fold(node, |n, key| match &n.value {
        EffectiveValue::Table(t) => t.get(key),
        EffectiveValue::Array(a) => key.parse::<usize>().ok().and_then(|i| a.get(i)),
        EffectiveValue::Scalar(_) => None,
    })
}

fn merge_tree(base: &mut Tree, mut incoming: Tree, path: &[String]) {
    merge_tree_mode(base, &mut incoming, path, ResetMode::Discard);
}

/// Compose a precedence row with the same history and replacement accounting
/// as a file layer. Histories use paths relative to the stage root.
pub(super) fn merge_stage_row(
    base: &mut EffectiveNode,
    history: &mut BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
    higher: EffectiveNode,
    higher_history: BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
    resets: ResetMode,
) {
    let mut tree = Tree {
        root: base.clone(),
        history: std::mem::take(history),
    };
    let mut incoming = Tree {
        root: higher,
        history: higher_history,
    };
    merge_tree_mode(&mut tree, &mut incoming, &[], resets);
    *base = tree.root;
    *history = tree.history;
}

fn merge_tree_mode(base: &mut Tree, incoming: &mut Tree, path: &[String], mode: ResetMode) {
    // The first event of a precomposed layer still needs the inherited value.
    // Later events already describe its generic -> unix -> OS transitions.
    let resets: Vec<_> = incoming
        .history
        .iter()
        .flat_map(|(key, events)| {
            events
                .iter()
                .filter(move |e| mode == ResetMode::Discard && e.kind == EventKind::Reset)
                .map(move |e| (key.clone(), e.order))
        })
        .collect();
    let mut fresh_paths = BTreeSet::new();
    for (key, events) in &mut incoming.history {
        if mode == ResetMode::Preserve && lookup(&incoming.root, &key[path.len()..]).is_none() {
            continue;
        }
        if events.first().is_some_and(|first| {
            resets.iter().any(|(ancestor, order)| {
                key.len() > ancestor.len() && key.starts_with(ancestor) && first.order > *order
            })
        }) {
            // This field was first supplied after a reset of its build stage;
            // the inherited field was discarded rather than overridden.
            fresh_paths.insert(key.clone());
            continue;
        }
        let relative = &key[path.len()..];
        let inherited = lookup(&base.root, relative).or_else(|| {
            // argv and shell are two spellings of one replaceable command.
            // Record a replacement on the new spelling as well as a discarded
            // event for the spelling that disappears.
            let (form, parent) = relative.split_last()?;
            let other = match form.as_str() {
                "argv" => "shell",
                "shell" => "argv",
                _ => return None,
            };
            let stage = lookup(&base.root, parent)?;
            if stage.kind.stage() {
                stage.get(other)
            } else {
                None
            }
        });
        if let Some(old) = inherited {
            if let Some(reset) = events
                .iter_mut()
                .skip(1)
                .find(|e| mode == ResetMode::Discard && e.kind == EventKind::Reset)
            {
                // A reset after a generic contribution discards the inherited
                // fields as well as that layer's generic fields. Capture the
                // table immediately before that reset, with all leaf origins.
                let mut removed = old.clone();
                if let Some(local) = reset.removed.first() {
                    merge_node(
                        &mut removed,
                        local.to_node(old.kind, reset.order),
                        key,
                        ResetMode::Discard,
                        &mut BTreeMap::new(),
                    );
                }
                reset.removed = vec![removed.snapshot()];
            }
            // Modifier rows never participate in the earlier row-3/4 resets.
            // Bind their inherited value to the first surviving contribution,
            // rather than to a historical value that a reset discarded.
            let start = if mode == ResetMode::Preserve {
                events
                    .iter()
                    .rposition(|e| matches!(e.kind, EventKind::Discarded | EventKind::Reset))
                    .map_or(0, |i| i + 1)
            } else {
                0
            };
            let Some(first) = events.get_mut(start) else {
                continue;
            };
            if mode == ResetMode::Preserve && first.kind == EventKind::Reset {
                continue;
            }
            let replaces = first.value.as_ref().is_some_and(|value| {
                value.replace_as_unit
                    || !matches!(value.value, EffectiveValue::Table(_))
                    || old.table().is_none()
            });
            if first.kind == EventKind::Set && replaces {
                first.kind = EventKind::Replace;
            }
            if replaces || matches!(first.kind, EventKind::Delete | EventKind::Reset) {
                first.removed.insert(0, old.snapshot());
            }
        }
    }
    let discarded = base.root.merge_from(incoming.root.clone(), path, mode);
    for (key, mut events) in discarded {
        // A precomposed history may already set and then discard this key.
        // Its first event now accounts for the inherited value. A field first
        // supplied after a reset still needs the earlier discard recorded.
        if mode == ResetMode::Discard
            && incoming.history.contains_key(&key)
            && !fresh_paths.contains(&key)
        {
            continue;
        }
        if let Some(reset) = incoming
            .history
            .iter()
            .filter(|(ancestor, _)| key.starts_with(ancestor))
            .flat_map(|(_, events)| events)
            .filter(|event| mode == ResetMode::Discard && event.kind == EventKind::Reset)
            .min_by_key(|event| event.order)
        {
            for event in &mut events {
                event.origin = reset.origin.clone();
                event.provenance = reset.provenance.clone();
                event.overlay = reset.overlay.clone();
                event.order = reset.order;
            }
        }
        base.history.entry(key).or_default().extend(events);
    }
    append_history(&mut base.history, std::mem::take(&mut incoming.history));
}

/// Records `node` and its descendants as discarded by `supplier`.
fn discard(
    node: &EffectiveNode,
    supplier: &EffectiveNode,
    path: &[String],
    history: &mut BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
) {
    let event = ProvenanceEvent {
        kind: EventKind::Discarded,
        provenance: supplier.provenance.clone(),
        origin: supplier.origin.clone(),
        overlay: supplier.overlay.clone(),
        value: None,
        removed: vec![node.snapshot()],
        order: supplier.order,
    };
    history.entry(path.to_vec()).or_default().push(event);
    match &node.value {
        EffectiveValue::Table(t) => {
            for (key, child) in t {
                discard(child, supplier, &child_path(path, key), history);
            }
        }
        EffectiveValue::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                discard(child, supplier, &child_path(path, &i.to_string()), history);
            }
        }
        EffectiveValue::Scalar(_) => {}
    }
}

fn merge_node(
    base: &mut EffectiveNode,
    mut incoming: EffectiveNode,
    path: &[String],
    resets: ResetMode,
    history: &mut BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
) {
    let reset = base.reset_occurred || incoming.reset_occurred;
    let discards = resets == ResetMode::Discard && incoming.reset_occurred;
    let recursive = base.table().is_some()
        && incoming.table().is_some()
        && !(incoming.kind.atomic() || discards);
    if recursive {
        let mut removals = vec![];
        if incoming.kind.stage() {
            for (form, other) in [("argv", "shell"), ("shell", "argv")] {
                if incoming.get(form).is_some() && incoming.get(other).is_none() {
                    removals.push(other);
                }
            }
        }
        let EffectiveValue::Table(table) = &mut base.value else {
            unreachable!()
        };
        let mut table = std::mem::take(table);
        for key in removals {
            if let Some(removed) = table.remove(key) {
                let supplier = incoming
                    .get(if key == "argv" { "shell" } else { "argv" })
                    .expect("command replacement");
                discard(&removed, supplier, &child_path(path, key), history);
            }
        }
        let EffectiveValue::Table(new_table) = &mut incoming.value else {
            unreachable!()
        };
        for (key, value) in std::mem::take(new_table) {
            if let Some(old) = table.get_mut(&key) {
                merge_node(old, value, &child_path(path, &key), resets, history);
            } else {
                table.insert(key, value);
            }
        }
        *base = incoming;
        base.value = EffectiveValue::Table(table);
    } else {
        discarded_children(base, &incoming, path, resets, history);
        *base = incoming;
    }
    base.reset_occurred = reset;
}

fn discarded_children(
    old: &EffectiveNode,
    incoming: &EffectiveNode,
    path: &[String],
    resets: ResetMode,
    history: &mut BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
) {
    let discards = resets == ResetMode::Discard && incoming.reset_occurred;
    let children: Vec<_> = match &old.value {
        EffectiveValue::Table(table) => table
            .iter()
            .map(|(key, node)| (key.clone(), node))
            .collect(),
        EffectiveValue::Array(array) => array
            .iter()
            .enumerate()
            .map(|(index, node)| (index.to_string(), node))
            .collect(),
        EffectiveValue::Scalar(_) => vec![],
    };
    for (key, child) in children {
        let next = child_path(path, &key);
        let replacement = if discards {
            None
        } else {
            lookup(incoming, std::slice::from_ref(&key))
        };
        if let Some(replacement) = replacement {
            discarded_children(child, replacement, &next, resets, history);
        } else {
            discard(child, incoming, &next, history);
        }
    }
}

/// Collects a layer's invalid markers. `applies` is false below an inactive
/// or forbidden overlay; a rejected key's subtree yields only its own marker.
fn collect_markers(
    raw: &RawNode,
    kind: NodeKind,
    ctx: Context<'_>,
    path: &[String],
    applies: bool,
    out: &mut Vec<InvalidMarker>,
) {
    if raw.invalid {
        out.push(InvalidMarker {
            path: path.to_vec(),
            origin: raw.origin.clone(),
            provenance: provenance(&raw.origin, ctx.layer),
            overlay: ctx.overlay.map(str::to_owned),
            layer: ctx.layer,
            applies,
            rejected_key: raw.rejected,
        });
    }
    if raw.rejected {
        return;
    }
    match &raw.value {
        RawValue::Table(t) => {
            for (key, child) in t {
                if let Some(role) = child.overlay {
                    let active = role == OverlayRole::Site && ctx.platform.overlay_active(key);
                    let branch_ctx = Context {
                        layer: ctx.layer,
                        platform: ctx.platform,
                        overlay: Some(child.origin.key_path.as_str()),
                    };
                    collect_markers(child, kind, branch_ctx, path, applies && active, out);
                } else {
                    let key = map_key(kind, ctx.platform, key);
                    collect_markers(
                        child,
                        kind.child(&key),
                        ctx,
                        &child_path(path, &key),
                        applies,
                        out,
                    );
                }
            }
        }
        RawValue::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                collect_markers(
                    child,
                    NodeKind::Other,
                    ctx,
                    &child_path(path, &i.to_string()),
                    applies,
                    out,
                );
            }
        }
        _ => {}
    }
}

/// Second exclusivity pass (§3 phase 1) on a layer composed for the
/// validation platform. Conflicts the raw pass already reported are skipped.
fn check_choices(
    node: &EffectiveNode,
    path: &[String],
    layer: LayerKind,
    diagnostics: &mut Vec<Diagnostic>,
    markers: &mut Vec<InvalidMarker>,
) {
    let pairs: &[(&str, &str)] = if node.kind.stage() {
        &[("argv", "shell")]
    } else if node.kind == NodeKind::Cwd {
        &[("path", "selector")]
    } else if node.kind == NodeKind::Artifacts {
        &[("file", "dir"), ("file", "required")]
    } else {
        &[]
    };
    for (a, b) in pairs {
        if let (Some(a_node), Some(b_node)) = (node.get(a), node.get(b)) {
            let message = format!("{a} and {b} are mutually exclusive");
            let duplicate = diagnostics.iter().any(|d| {
                d.message == message
                    && ((d.origin == a_node.origin
                        && d.second_origin.as_ref() == Some(&b_node.origin))
                        || (d.origin == b_node.origin
                            && d.second_origin.as_ref() == Some(&a_node.origin)))
            });
            if !duplicate {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    message,
                    origin: a_node.origin.clone(),
                    second_origin: Some(b_node.origin.clone()),
                });
                markers.push(InvalidMarker {
                    path: path.to_vec(),
                    origin: node.origin.clone(),
                    provenance: node.provenance.clone(),
                    overlay: node.overlay.clone(),
                    layer,
                    applies: true,
                    rejected_key: false,
                });
            }
        }
    }
    match &node.value {
        EffectiveValue::Table(t) => {
            for (key, child) in t {
                check_choices(child, &child_path(path, key), layer, diagnostics, markers);
            }
        }
        EffectiveValue::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                check_choices(
                    child,
                    &child_path(path, &i.to_string()),
                    layer,
                    diagnostics,
                    markers,
                );
            }
        }
        EffectiveValue::Scalar(_) => {}
    }
}

fn annotate(
    node: &mut EffectiveNode,
    path: &[String],
    history: &BTreeMap<KeyPath, Vec<ProvenanceEvent>>,
    markers: &[InvalidMarker],
) {
    node.events = history.get(path).cloned().unwrap_or_default();
    node.markers = markers
        .iter()
        .filter(|m| m.attaches_to(path))
        .cloned()
        .collect();
    match &mut node.value {
        EffectiveValue::Table(t) => {
            for (key, child) in t {
                annotate(child, &child_path(path, key), history, markers);
            }
        }
        EffectiveValue::Array(a) => {
            for (i, child) in a.iter_mut().enumerate() {
                annotate(child, &child_path(path, &i.to_string()), history, markers);
            }
        }
        EffectiveValue::Scalar(_) => {}
    }
}

fn empty_root() -> EffectiveNode {
    EffectiveNode::new(
        "",
        NodeKind::Root,
        EffectiveValue::Table(BTreeMap::new()),
        None,
        Provenance::BuiltIn,
        Origin {
            file: PathBuf::new(),
            line: None,
            col: None,
            key_path: String::new(),
            span: None,
        },
    )
}

/// Compose platform branches within each layer, then merge layers 0 -> 3.
/// Includes embedded layer 0 unless explicitly supplied. Equal-rank layers keep
/// input order; discovery normally supplies exactly one user layer.
///
/// # Panics
/// Layers must have been parsed for `platform`, since lexical path interpretation
/// is deliberately performed only for the active branches by the parser.
pub fn compose(layers: &[RawLayer], platform: Platform) -> MergedConfig {
    let builtin = built_in(platform);
    let mut ordered: Vec<_> = layers.iter().collect();
    if !layers.iter().any(|l| l.kind == LayerKind::BuiltIn) {
        ordered.push(&builtin);
    }
    ordered.sort_by_key(|l| match l.kind {
        LayerKind::BuiltIn => 0,
        LayerKind::DefaultUser | LayerKind::OverrideUser => 1,
        LayerKind::Workspace => 2,
        LayerKind::Problem => 3,
    });
    let mut merged = Tree {
        root: empty_root(),
        history: BTreeMap::new(),
    };
    let mut diagnostics = vec![];
    let mut markers = vec![];
    let mut unknown_inputs = vec![];
    let mut layer_files = BTreeMap::new();
    for layer in ordered {
        layer_files.insert(layer.file.clone(), layer.kind);
        assert_eq!(
            layer.platform, platform,
            "compose requires layers parsed for its platform"
        );
        diagnostics.extend(layer.diagnostics.clone());
        match &layer.state {
            LayerState::Empty => {}
            LayerState::Invalid(origin) => unknown_inputs.push(UnknownInput {
                layer: layer.kind,
                origin: origin.clone(),
                provenance: provenance(origin, layer.kind),
            }),
            LayerState::Loaded(raw) => {
                let problem = layer.kind == LayerKind::Problem;
                let (path, name, kind): (Vec<String>, &str, _) = if problem {
                    (vec!["config".into()], "config", NodeKind::Problem)
                } else {
                    (vec![], "", NodeKind::Root)
                };
                let ctx = Context {
                    layer: layer.kind,
                    platform,
                    overlay: None,
                };
                collect_markers(raw, kind, ctx, &path, true, &mut markers);
                let mut order = 0;
                let Some(mut tree) = compose_node(raw, kind, ctx, &path, name, &mut order) else {
                    continue;
                };
                check_choices(
                    &tree.root,
                    &path,
                    layer.kind,
                    &mut diagnostics,
                    &mut markers,
                );
                if problem {
                    let mut wrapper = node_from_raw(raw, NodeKind::Root, ctx, "", 0);
                    wrapper.value =
                        EffectiveValue::Table(BTreeMap::from([("config".into(), tree.root)]));
                    tree.root = wrapper;
                }
                merge_tree(&mut merged, tree, &[]);
            }
        }
    }
    annotate(&mut merged.root, &[], &merged.history, &markers);
    MergedConfig {
        platform,
        root: merged.root,
        diagnostics,
        markers,
        unknown_inputs,
        history: merged.history,
        layer_files,
    }
}
