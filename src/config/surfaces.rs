//! Raw schemas reserved for later consumers. No execution capability is granted.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum Surface {
    Status,
    Archive,
    ArchiveProfiles,
    ArchiveProfile,
    Quarantine,
    Submit,
    SubmitStage,
    Version,
    Http,
    Sites,
    Atcoder,
    Check,
    Scoring,
    Interactive,
    InteractorTest,
    Role,
    Interactor,
    Solution,
    Stress,
    StressRole,
}

/// Declarative defaults for future consumers; deliberately not injected into
/// gate-1 inspection output or execution plans. Optional/derived values are absent.
pub const WORKFLOW_DEFAULTS: &str = r#"
[status]
values = []
[archive]
roots = []
layout = "{site}/{problem_id}"
contest_layout = "{site}/contests/{contest_id}"
keep = []
discard = []
cleanup = "trash"
on_existing = "error"
[archive.quarantine]
retention_days = 30
[submit]
clipboard = false
[http]
offline = false
[sites.atcoder]
language = "en"
"#;
/// `checking` and role specs have no defaults; the table itself is conditional.
pub const CHECK_DEFAULTS: &str = r#"
kind = "compare"
answer = "file"
scoring = "pass_fail"
display = "cpf"
"#;
pub const INTERACTIVE_DEFAULTS: &str = r#"
mode = "relay"
idle_s = 0
early_reader_close = false
interactor_allowance_s = 1
peer_allowance_s = 1
solution_seed = "none"
"#;
pub const SCORING_DEFAULTS: &str = "aggregation = \"sum\"\n";
pub const ROLE_DEFAULTS: &str = "support_files = []\nassets = []\n";
pub const INTERACTOR_TEST_DEFAULTS: &str = "good = []\nbad = []\n";
pub const INTERACTOR_SEED_DEFAULTS: &str = "seed = \"none\"\n";
pub const STRESS_DEFAULTS: &str = "iterations = 10000\ntime_s = 60\n";
pub const SUBMIT_STAGE_DEFAULTS: &str = "test = true\ndeadline_s = 60\n";
// Remaining defaults derive from context: archive.default_root = first root,
// submit.test_dir = OS temporary directory, submit.judge = site, stress.min_score
// = scoring scale. Role support_files/assets and interactor_test lists default
// to [], role entry to language entry, and interactor seed to "none". Stream
// caps of submit stages use the existing build caps (execution §5.5).

impl Validator<'_> {
    pub(in crate::config) fn record_table(&mut self, n: &mut RawNode, stress: bool) {
        self.table(
            n,
            Context::Surface(if stress {
                Surface::Stress
            } else {
                Surface::Check
            }),
            true,
            false,
        );
    }

    pub(super) fn surface_field(
        &mut self,
        key: &str,
        n: &mut RawNode,
        surface: Surface,
        active: bool,
        overlaid: bool,
    ) {
        use Surface as S;
        let platform = if active {
            self.platform
        } else {
            Platform::Linux
        };
        let nested = match (surface, key) {
            (S::Archive, "profiles") => Some(S::ArchiveProfiles),
            (S::Archive, "quarantine") => Some(S::Quarantine),
            (S::Sites, "atcoder") => Some(S::Atcoder),
            (S::SubmitStage, "version") => Some(S::Version),
            (S::Check, "checker" | "validator") => Some(S::Role),
            (S::Check, "interactive") => Some(S::Interactive),
            (S::Check, "interactor_test") => Some(S::InteractorTest),
            (S::Interactive, "interactor") => Some(S::Interactor),
            (S::Stress, "generator" | "brute") => Some(S::StressRole),
            _ => None,
        };
        if let Some(next) = nested {
            self.table(n, Context::Surface(next), active, overlaid);
            return;
        }
        match (surface, key) {
            (S::Status, "values") => self.surface_strings(
                n,
                32,
                |s| status_valid(s) && !["new", "in-progress", "solved", "skipped"].contains(&s),
                "expected a custom status name (not a built-in status)",
            ),
            (S::Archive, "roots") => {
                self.surface_paths(n, platform, active);
            }
            (S::Archive, "default_root") => self.root_path(n, platform, active),
            (S::Archive, "layout" | "contest_layout") => {
                if let Some(s) = self.string(n) {
                    if !layout_valid(&s) {
                        self.error(n, "invalid archive layout");
                    }
                }
            }
            (S::Archive | S::ArchiveProfile, "keep" | "discard") => self.surface_strings(
                n,
                9,
                |s| {
                    [
                        "sources",
                        "tests:sample",
                        "tests:custom",
                        "tests:stress",
                        "stress",
                        "notes",
                        "submit",
                        "metadata",
                        "fetch",
                    ]
                    .contains(&s)
                        && (key != "discard" || !["metadata", "fetch"].contains(&s))
                },
                "invalid archive class",
            ),
            (S::Archive | S::ArchiveProfile, "cleanup") => {
                self.choice(n, &["trash", "quarantine", "delete", "keep"])
            }
            (S::Archive, "on_existing") => self.choice(n, &["error", "reuse-identical", "new"]),
            (S::ArchiveProfiles, _) => {
                if !syntax::name_valid(key, true) {
                    self.error(n, "invalid archive profile name");
                }
                self.table(n, Context::Surface(S::ArchiveProfile), active, overlaid);
            }
            (S::Quarantine, "retention_days") => self.integer(n, 0, 36_500),
            (S::Submit, "clipboard")
            | (S::Http, "offline")
            | (S::SubmitStage, "test")
            | (S::Interactive, "early_reader_close") => self.boolean(n),
            (S::Submit, "test_dir") => self.path(n, platform, active),
            (S::Http, "cache_dir") => {
                if !matches!(self.kind, LayerKind::DefaultUser | LayerKind::OverrideUser) {
                    self.reject(n, "http.cache_dir is allowed only in user configuration");
                }
                if let Some(s) = self.string(n) {
                    if !syntax::absolute(&s, platform) || s.contains('\0') {
                        self.error(n, "expected an absolute path");
                    } else {
                        self.path(n, platform, active);
                    }
                }
            }
            (S::Atcoder, "language") => self.choice(n, &["en", "ja"]),
            (S::SubmitStage, "judge") => self.name(n, false),
            (S::Version, "argv") => {
                self.strings(n, StringRule::Argv, platform, active);
                if let RawValue::Array(values) = &mut n.value {
                    for v in values {
                        if v.string().is_some_and(|s| s.contains(['{', '}'])) {
                            self.error(v, "version argv must contain no placeholders or braces");
                        }
                    }
                }
            }
            (S::Version, "expect") => {
                self.string(n);
            }
            (S::SubmitStage, _) => {
                // Validate the future submit grammar without teaching gate-1
                // stages to accept submit-only placeholders or executing them.
                let mut view = n.clone();
                submit_grammar_view(&mut view);
                self.field(
                    key,
                    &mut view,
                    Context::Stage {
                        command: false,
                        overlays: false,
                    },
                    active,
                    overlaid,
                );
                adopt_view(view, n);
            }
            (S::Check, "kind") => self.choice(n, &["compare", "checker", "interactive"]),
            (S::Check, "answer") => self.choice(n, &["file", "none"]),
            (S::Check, "display") => self.choice(n, &["cpf", "judge-cf"]),
            (S::Check, "scoring") => {
                if matches!(n.value, RawValue::String(_)) {
                    self.choice(n, &["pass_fail"]);
                } else {
                    self.table(n, Context::Surface(S::Scoring), active, overlaid);
                }
            }
            (S::Scoring, "points") => {
                if !matches!(n.value, RawValue::Boolean(true)) {
                    self.error(n, "points must be true (or use scoring = 'pass_fail')");
                }
            }
            (S::Scoring, "scale") => self.integer(n, 1, 1_000_000_000),
            (S::Scoring, "aggregation") => self.choice(n, &["sum", "min"]),
            (S::Interactive, "checking") => {
                self.choice(n, &["interactor", "checker", "interactor+checker"])
            }
            (S::Interactive, "mode") => self.choice(n, &["relay", "direct"]),
            (S::Interactive, "solution_seed") => self.choice(n, &["none", "env"]),
            (S::Interactive, "idle_s")
                if matches!(n.value, RawValue::Integer(0) | RawValue::Float(0.0)) =>
            {
                n.typed = Some(TypedValue::Milliseconds(0));
            }
            (S::Interactive, "idle_s" | "interactor_allowance_s" | "peer_allowance_s")
            | (S::Stress, "time_s") => self.deadline(n),
            (S::InteractorTest, "good" | "bad") => {
                if let RawValue::Array(values) = &mut n.value {
                    for v in values {
                        self.table(v, Context::Surface(S::Solution), active, overlaid);
                    }
                } else {
                    self.error(n, "expected an array of solution specs");
                }
            }
            (
                S::Role | S::Interactor | S::Solution | S::StressRole,
                "sources" | "support_files",
            )
            | (S::Role | S::Interactor | S::StressRole, "assets") => {
                self.strings(n, StringRule::Source, platform, active)
            }
            (S::Role | S::Interactor | S::Solution | S::StressRole, "language") => {
                self.name(n, true)
            }
            (S::Role | S::Interactor | S::Solution | S::StressRole, "entry") => self.nonempty(n),
            (S::Role | S::Interactor | S::StressRole, "program") => {
                if let Some(s) = self.string(n) {
                    if !syntax::source_valid(&s, platform) {
                        self.error(n, "expected a problem-relative program path");
                    }
                }
            }
            (S::Role, "profile") => self.choice(n, &["testlib", "simple"]),
            (S::Interactor, "profile") => self.choice(n, &["testlib"]),
            (S::Interactor, "seed") => self.choice(n, &["none", "env", "argv"]),
            (S::Role | S::StressRole, "deadline_s") => self.deadline(n),
            (S::Role | S::StressRole, "stdout_cap" | "stderr_cap")
            | (S::Interactor, "stderr_cap") => self.size(n, 1_u64 << 40),
            (S::Stress, "iterations") => self.integer(n, 1, i64::MAX as u64),
            (S::Stress, "min_score") => {
                if let Some(s) = self.string(n) {
                    if !points_valid(&s) {
                        self.error(n, "expected a non-negative decimal points string with at most nine fractional digits");
                    }
                }
            }
            _ => self.unknown(n),
        }
    }

    pub(super) fn surface_table_rules(&mut self, n: &mut RawNode, surface: Surface) {
        if matches!(
            surface,
            Surface::Role | Surface::Interactor | Surface::StressRole
        ) {
            for field in ["sources", "support_files", "language", "entry"] {
                self.conflict(n, "program", field);
            }
        }
        if matches!(surface, Surface::Version)
            && (n.get("argv").is_none() || n.get("expect").is_none())
        {
            self.error(n, "version requires argv and expect");
        }
        if matches!(surface, Surface::Archive | Surface::ArchiveProfile) {
            let overlap =
                n.get("keep")
                    .zip(n.get("discard"))
                    .is_some_and(|(keep, discard)| match (&keep.value, &discard.value) {
                        (RawValue::Array(a), RawValue::Array(b)) => a.iter().any(|a| {
                            b.iter()
                                .any(|b| a.string().is_some() && a.string() == b.string())
                        }),
                        _ => false,
                    });
            if overlap {
                self.error(n, "a class cannot appear in both keep and discard");
            }
        }
    }

    fn surface_strings(
        &mut self,
        n: &mut RawNode,
        max: usize,
        valid: impl Fn(&str) -> bool,
        message: &str,
    ) {
        let RawValue::Array(values) = &mut n.value else {
            self.error(n, "expected an array of strings");
            return;
        };
        let too_many = values.len() > max;
        let mut seen = std::collections::BTreeSet::new();
        for v in values {
            if let Some(s) = self.string(v) {
                if !valid(&s) {
                    self.error(v, message);
                }
                if !seen.insert(s) {
                    self.error(v, "duplicate value");
                }
            }
        }
        if too_many {
            self.error(n, format!("expected at most {max} values"));
        }
    }
    fn surface_paths(&mut self, n: &mut RawNode, platform: Platform, active: bool) {
        let RawValue::Array(values) = &mut n.value else {
            self.error(n, "expected an array of root paths");
            return;
        };
        let too_many = values.len() > 16;
        for v in values {
            self.root_path(v, platform, active);
        }
        if too_many {
            self.error(n, "expected at most 16 archive roots");
        }
    }
    fn root_path(&mut self, n: &mut RawNode, platform: Platform, active: bool) {
        if let Some(s) = self.string(n) {
            if !(syntax::absolute(&s, platform) || s.starts_with("~/")) || s.contains('\0') {
                self.error(n, "archive root must be absolute or start with ~/");
            } else {
                self.path(n, platform, active);
            }
        }
    }
}
fn status_valid(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.as_bytes()[0].is_ascii_lowercase()
        && !s.ends_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn points_valid(s: &str) -> bool {
    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
    !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && s.bytes()
            .filter(u8::is_ascii_digit)
            .skip_while(|b| *b == b'0')
            .count()
            <= 18
        && (!s.contains('.')
            || (!fraction.is_empty()
                && fraction.len() <= 9
                && fraction.bytes().all(|b| b.is_ascii_digit())))
}
fn layout_valid(s: &str) -> bool {
    let segments: Vec<_> = s.split('/').collect();
    (1..=8).contains(&segments.len())
        && segments.iter().all(|segment| {
            let mut rendered = String::new();
            let mut chars = segment.chars();
            while let Some(c) = chars.next() {
                if c == '{' {
                    let name: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    if ![
                        "site",
                        "problem_id",
                        "contest_id",
                        "label",
                        "contest_date_year",
                    ]
                    .contains(&name.as_str())
                    {
                        return false;
                    }
                    rendered.push('x');
                } else if c == '}' {
                    return false;
                } else {
                    rendered.push(c);
                }
            }
            // Every opening brace must actually have a closing brace.
            segment.bytes().filter(|b| *b == b'{').count()
                == segment.bytes().filter(|b| *b == b'}').count()
                && crate::names::PortableComponent::new(rendered).is_ok()
        })
}
/// Copies the phase-1 results of the validated view back onto the written
/// node at every depth, so composition skips rejected keys and sees typed
/// values. Text parsed from a rewritten string is not copied: its tokens
/// would name the substitute placeholder.
fn adopt_view(view: RawNode, n: &mut RawNode) {
    n.invalid = view.invalid;
    n.rejected = view.rejected;
    n.overlay = view.overlay;
    let rewritten = matches!(
        (&view.value, &n.value),
        (RawValue::String(a), RawValue::String(b)) if a != b
    );
    if !rewritten {
        n.typed = view.typed;
    }
    match (view.value, &mut n.value) {
        (RawValue::Array(from), RawValue::Array(to)) => {
            for (from, to) in from.into_iter().zip(to.iter_mut()) {
                adopt_view(from, to);
            }
        }
        (RawValue::Table(from), RawValue::Table(to)) => {
            for (key, from) in from {
                if let Some(to) = to.get_mut(&key) {
                    adopt_view(from, to);
                }
            }
        }
        _ => {}
    }
}
fn submit_grammar_view(n: &mut RawNode) {
    match &mut n.value {
        RawValue::String(s) => {
            for (from, to) in [
                ("{submit_dir}", "{entry}"),
                ("{submit_file}", "{entry}"),
                ("{judge}", "{entry}"),
                ("{solution_sources}", "{sources}"),
            ] {
                *s = s.replace(from, to);
            }
        }
        RawValue::Array(a) => a.iter_mut().for_each(submit_grammar_view),
        RawValue::Table(t) => t.values_mut().for_each(submit_grammar_view),
        _ => {}
    }
}
