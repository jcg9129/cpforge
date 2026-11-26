#[path = "surfaces.rs"]
pub(super) mod surfaces;
use surfaces::Surface;

use std::collections::BTreeMap;

use super::load::Environment;
use super::syntax;
use super::{
    Combine, Comparator, Diagnostic, FloatComparator, LayerKind, OverlayRole, ParsedText,
    Placeholder, Platform, RawNode, RawValue, RelativeBase, Severity, Token, TypedValue,
};

#[derive(Clone, Copy)]
enum Context {
    Root,
    Problem,
    Languages,
    Language,
    Profiles,
    Profile,
    Commands,
    Stage { command: bool, overlays: bool },
    Shell,
    Cwd,
    Env,
    Limits,
    Artifacts,
    Template,
    Io,
    Retention,
    Workspace,
    Hooks,
    Surface(Surface),
}

pub(super) struct Validator<'a> {
    env: &'a Environment,
    platform: Platform,
    kind: LayerKind,
    pub diagnostics: Vec<Diagnostic>,
}

impl<'a> Validator<'a> {
    pub fn new(env: &'a Environment, platform: Platform, kind: LayerKind) -> Self {
        Self {
            env,
            platform,
            kind,
            diagnostics: vec![],
        }
    }

    pub fn root(&mut self, node: &mut RawNode) {
        self.table(
            node,
            if self.kind == LayerKind::Problem {
                Context::Problem
            } else {
                Context::Root
            },
            true,
            false,
        );
    }

    fn error(&mut self, node: &mut RawNode, message: impl Into<String>) {
        node.invalid = true;
        self.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            message: message.into(),
            origin: node.origin.clone(),
            second_origin: None,
        });
    }

    fn conflict(&mut self, node: &mut RawNode, a: &str, b: &str) {
        let Some(t) = node.table() else {
            return;
        };
        if let (Some(a_node), Some(b_node)) = (t.get(a), t.get(b)) {
            let mut d = Diagnostic {
                severity: Severity::Error,
                message: format!("{a} and {b} are mutually exclusive"),
                origin: a_node.origin.clone(),
                second_origin: Some(b_node.origin.clone()),
            };
            // Diagnostics identify the first conflicting field; recovery marks
            // the containing choice, whose interpretation is invalid.
            if d.second_origin
                .as_ref()
                .and_then(|o| o.span.as_ref())
                .map(|s| s.start)
                < d.origin.span.as_ref().map(|s| s.start)
            {
                let second = d.second_origin.take().expect("conflict has two origins");
                d.second_origin = Some(std::mem::replace(&mut d.origin, second));
            }
            node.invalid = true;
            self.diagnostics.push(d);
        }
    }

    fn table(&mut self, node: &mut RawNode, ctx: Context, active: bool, overlaid: bool) {
        if node.table().is_none() {
            self.error(node, "expected a table");
            return;
        }
        match ctx {
            Context::Stage { .. } | Context::Surface(Surface::SubmitStage) => {
                self.conflict(node, "argv", "shell")
            }
            Context::Cwd => self.conflict(node, "path", "selector"),
            Context::Artifacts => {
                self.conflict(node, "file", "dir");
                self.conflict(node, "file", "required");
            }
            _ => {}
        }
        let RawValue::Table(t) = &mut node.value else {
            unreachable!()
        };
        if matches!(ctx, Context::Env) && active && self.platform == Platform::Windows {
            let mut seen: BTreeMap<String, super::Origin> = BTreeMap::new();
            // Source order decides which spelling is the original one.
            let mut entries: Vec<_> = t.iter_mut().collect();
            entries.sort_by_key(|(_, n)| n.origin.span.as_ref().map(|s| s.start));
            for (key, value) in entries {
                let folded = super::env_name_fold(self.platform, key);
                if let Some(other) = seen.get(&folded) {
                    value.invalid = true;
                    self.diagnostics.push(Diagnostic {
                        severity: Severity::Error,
                        message: "duplicate environment name (case-insensitive on Windows)".into(),
                        origin: value.origin.clone(),
                        second_origin: Some(other.clone()),
                    });
                } else {
                    seen.insert(folded, value.origin.clone());
                }
            }
        }
        for (key, field) in t.iter_mut() {
            let site = matches!(ctx, Context::Language | Context::Profile)
                || matches!(
                    ctx,
                    Context::Stage { overlays: true, .. } | Context::Surface(Surface::SubmitStage)
                );
            if syntax::overlay(key)
                && (site || (overlaid && !matches!(ctx, Context::Env | Context::Commands)))
            {
                if overlaid || !site {
                    self.error(
                        field,
                        "platform overlays cannot be nested or used at this site",
                    );
                    field.overlay = Some(OverlayRole::Forbidden);
                    // Still check known fields within a forbidden overlay.
                    self.table(
                        field,
                        ctx,
                        active && self.platform.overlay_active(key),
                        true,
                    );
                } else {
                    field.overlay = Some(OverlayRole::Site);
                    self.table(
                        field,
                        ctx,
                        active && self.platform.overlay_active(key),
                        true,
                    );
                }
                continue;
            }
            self.field(key, field, ctx, active, overlaid);
        }
        let missing_io_names =
            matches!(ctx, Context::Io) && !t.contains_key("input") && !t.contains_key("output");
        let colliding_io_names = matches!(ctx, Context::Io)
            && t.get("input")
                .zip(t.get("output"))
                .is_some_and(|(input, output)| {
                    !input.has_errors()
                        && !output.has_errors()
                        && input
                            .string()
                            .zip(output.string())
                            .is_some_and(|(input, output)| {
                                crate::names::collision_key(input)
                                    == crate::names::collision_key(output)
                            })
                });
        let missing_template_path = matches!(ctx, Context::Template) && !t.contains_key("path");
        if colliding_io_names {
            self.error(
                node,
                "io.input and io.output name the same file on some filesystems",
            );
        }
        if missing_io_names {
            self.error(node, "io requires at least one of input or output");
        }
        if let Context::Surface(surface) = ctx {
            self.surface_table_rules(node, surface);
        }
        // Completeness of stages, shell, cwd and artifacts belongs to phase 2.
        // Template is not composed as a stage and its sole path is required.
        if missing_template_path {
            self.error(node, "template requires path");
        }
    }

    fn field(&mut self, key: &str, n: &mut RawNode, ctx: Context, active: bool, overlaid: bool) {
        let lexical = if active {
            self.platform
        } else {
            Platform::Linux
        };
        match ctx {
            Context::Root | Context::Problem => {
                let problem = matches!(ctx, Context::Problem);
                match key {
                    "schema" if !problem => {
                        self.integer(n, 1, 1);
                    }
                    "language" if problem => self.name(n, true),
                    "sources" | "support_files" | "solution_files" if problem => {
                        self.strings(n, StringRule::Source, lexical, active)
                    }
                    "entry" if problem => self.nonempty(n),
                    "io" if problem => self.table(n, Context::Io, active, overlaid),
                    "compare" => self.compare(n),
                    "build" | "run" => self.table(
                        n,
                        Context::Stage {
                            command: false,
                            overlays: true,
                        },
                        active,
                        overlaid,
                    ),
                    "default_language" if !problem => self.name(n, true),
                    "languages" if !problem => self.table(n, Context::Languages, active, overlaid),
                    "commands" if !problem => self.table(n, Context::Commands, active, overlaid),
                    "retention" if !problem => self.table(n, Context::Retention, active, overlaid),
                    "workspace" if !problem => self.table(n, Context::Workspace, active, overlaid),
                    "hooks" if !problem => self.table(n, Context::Hooks, active, overlaid),
                    "status" | "archive" | "submit" | "http" | "sites" if !problem => {
                        let surface = match key {
                            "status" => Surface::Status,
                            "archive" => Surface::Archive,
                            "submit" => Surface::Submit,
                            "http" => Surface::Http,
                            _ => Surface::Sites,
                        };
                        self.table(n, Context::Surface(surface), active, overlaid);
                    }
                    "status" | "archive" | "http" | "sites" if problem => {
                        self.reject(n, "key is not allowed in the problem config layer")
                    }
                    "schema" | "default_language" | "languages" | "commands" | "retention"
                    | "workspace" | "hooks"
                        if problem =>
                    {
                        self.reject(n, "key is not allowed in the problem config layer")
                    }
                    _ => self.unknown(n),
                }
            }
            Context::Languages | Context::Profiles | Context::Commands => {
                let reserved = !matches!(ctx, Context::Commands);
                if !syntax::name_valid(key, reserved) {
                    self.error(n, "invalid configuration name (expected a letter followed by ASCII letters, digits, - or _) or reserved overlay name");
                }
                let next = match ctx {
                    Context::Languages => Context::Language,
                    Context::Profiles => Context::Profile,
                    _ => Context::Stage {
                        command: true,
                        overlays: true,
                    },
                };
                self.table(n, next, active, overlaid);
            }
            Context::Language | Context::Profile => match key {
                "build" if matches!(n.value, RawValue::Boolean(false)) => {}
                "build" | "run" => self.table(
                    n,
                    Context::Stage {
                        command: false,
                        overlays: matches!(ctx, Context::Language),
                    },
                    active,
                    overlaid,
                ),
                "submit" if matches!(ctx, Context::Language) => {
                    self.table(n, Context::Surface(Surface::SubmitStage), active, overlaid)
                }
                "extensions" if matches!(ctx, Context::Language) => {
                    self.strings(n, StringRule::Extension, lexical, active)
                }
                "sources" if matches!(ctx, Context::Language) => {
                    self.strings(n, StringRule::Source, lexical, active)
                }
                "entry" if matches!(ctx, Context::Language) => self.nonempty(n),
                "artifacts" if matches!(ctx, Context::Language) => {
                    self.table(n, Context::Artifacts, active, overlaid)
                }
                "template" if matches!(ctx, Context::Language) => {
                    self.table(n, Context::Template, active, overlaid)
                }
                "profiles" if matches!(ctx, Context::Language) => {
                    self.table(n, Context::Profiles, active, overlaid)
                }
                _ => self.unknown(n),
            },
            Context::Stage { command, .. } => match key {
                "argv" => self.strings(n, StringRule::Argv, lexical, active),
                "shell" => self.table(n, Context::Shell, active, overlaid),
                "cwd" => self.table(n, Context::Cwd, active, overlaid),
                "env" => self.table(n, Context::Env, active, overlaid),
                "env_remove" => self.strings(n, StringRule::EnvName, lexical, active),
                "deadline_s" => self.deadline(n),
                "stdout_cap" | "stderr_cap" => self.size(n, 1_u64 << 40),
                "limits" => self.table(n, Context::Limits, active, overlaid),
                "needs_build" | "for_each_test" | "keep_going" if command => self.boolean(n),
                "repeat" if command => self.integer(n, 1, 1000),
                "stdin" if command => self.choice(n, &["null", "test"]),
                "stdin" => self.reject(n, "stdin is allowed only in commands"),
                _ => self.unknown(n),
            },
            Context::Shell => match key {
                "interpreter" => self.strings(n, StringRule::Interpreter, lexical, active),
                "script" => {
                    self.string(n);
                }
                "args" => self.strings(n, StringRule::Args, lexical, active),
                _ => self.unknown(n),
            },
            Context::Cwd => match key {
                "path" => self.text(n, TextKind::Cwd, lexical, active),
                "selector" => self.choice(n, &["run", "problem", "workspace"]),
                _ => self.unknown(n),
            },
            Context::Env => {
                self.env_name(n, key, lexical);
                match &n.value {
                    RawValue::String(s) if s.contains('\0') => {
                        self.error(n, "environment value contains NUL")
                    }
                    RawValue::String(_) | RawValue::Boolean(false) => {}
                    _ => self.error(n, "environment value must be a string or false"),
                }
            }
            Context::Limits => match key {
                "address_space" | "job_commit" => self.size(n, u64::MAX),
                _ => self.unknown(n),
            },
            Context::Artifacts => {
                match key {
                    "file" => {
                        if let Some(s) = self.string(n) {
                            if !syntax::output_relative(&s, lexical) {
                                self.error(n, "expected an output-relative name without .., root or drive prefix");
                            }
                        }
                    }
                    "dir" => {
                        if !matches!(n.value, RawValue::Boolean(true)) {
                            self.error(n, "artifacts.dir must be true");
                        }
                    }
                    "required" => self.strings(n, StringRule::Output, lexical, active),
                    _ => self.unknown(n),
                }
            }
            Context::Template => match key {
                "path" => self.path(n, lexical, active),
                _ => self.unknown(n),
            },
            Context::Io => match key {
                "input" | "output" => {
                    if let Some(name) = self.string(n) {
                        if let Err(error) = crate::names::PortableComponent::new(name) {
                            self.error(n, format!("expected a portable file-I/O name: {error}"));
                        }
                    }
                }
                _ => self.unknown(n),
            },
            Context::Retention => match key {
                "keep_jobs" => self.integer(n, 1, 100_000),
                "job_bytes" | "workspace_bytes" | "cwd_bytes" | "build_bytes" => {
                    self.size(n, 1_u64 << 50)
                }
                _ => self.unknown(n),
            },
            Context::Workspace => match key {
                "ignore" => self.strings(n, StringRule::Ignore, lexical, active),
                _ => self.unknown(n),
            },
            Context::Surface(surface) => self.surface_field(key, n, surface, active, overlaid),
            Context::Hooks => match key {
                "on_switch" | "after_test" => {
                    self.strings(n, StringRule::CommandName, lexical, active)
                }
                "enabled" | "trusted_workspaces" => {
                    if self.kind != LayerKind::DefaultUser {
                        self.reject(
                            n,
                            "key is allowed only in the default user configuration file",
                        );
                    }
                    if key == "enabled" {
                        self.boolean(n);
                    } else {
                        self.strings(n, StringRule::Absolute, lexical, active);
                    }
                }
                _ => self.unknown(n),
            },
        }
    }

    fn unknown(&mut self, n: &mut RawNode) {
        self.reject(n, "unknown configuration key");
    }
    /// The key itself is not accepted where it was written; composition never
    /// uses its value.
    fn reject(&mut self, n: &mut RawNode, message: &str) {
        n.rejected = true;
        self.error(n, message);
    }
    fn string(&mut self, n: &mut RawNode) -> Option<String> {
        if let Some(s) = n.string() {
            Some(s.to_owned())
        } else {
            self.error(n, "expected a string");
            None
        }
    }
    fn boolean(&mut self, n: &mut RawNode) {
        if !matches!(n.value, RawValue::Boolean(_)) {
            self.error(n, "expected a boolean");
        }
    }
    fn nonempty(&mut self, n: &mut RawNode) {
        if let Some(text) = self.string(n) {
            if text.is_empty() {
                self.error(n, "string must not be empty");
            } else if text.contains('\0') {
                self.error(n, "string must contain no NUL");
            }
        }
    }
    fn name(&mut self, n: &mut RawNode, reserved: bool) {
        if let Some(s) = self.string(n) {
            if !syntax::name_valid(&s, reserved) {
                self.error(n, "invalid configuration name");
            }
        }
    }
    fn choice(&mut self, n: &mut RawNode, choices: &[&str]) {
        if let Some(s) = self.string(n) {
            if !choices.contains(&s.as_str()) {
                self.error(n, format!("expected one of {}", choices.join(", ")));
            }
        }
    }
    fn env_name(&mut self, n: &mut RawNode, name: &str, platform: Platform) {
        if name.contains(['\0', '=']) || (platform == Platform::Windows && name.is_empty()) {
            self.error(n, "invalid environment name");
        }
        if name.starts_with("CPF_")
            || (platform == Platform::Windows
                && name
                    .get(..4)
                    .is_some_and(|s| s.eq_ignore_ascii_case("CPF_")))
        {
            self.error(n, "environment names starting with CPF_ are reserved");
        }
    }

    fn strings(&mut self, n: &mut RawNode, rule: StringRule, platform: Platform, active: bool) {
        let RawValue::Array(values) = &mut n.value else {
            self.error(n, "expected an array of strings");
            return;
        };
        let empty = values.is_empty();
        let mut seen: BTreeMap<String, super::Origin> = BTreeMap::new();
        for (i, value) in values.iter_mut().enumerate() {
            let Some(s) = self.string(value) else {
                continue;
            };
            match rule {
                StringRule::Argv => self.text(
                    value,
                    if i == 0 {
                        TextKind::Program
                    } else {
                        TextKind::Argument
                    },
                    platform,
                    active,
                ),
                StringRule::Args => self.text(value, TextKind::Argument, platform, active),
                StringRule::Interpreter => {
                    if s.contains(['{', '}']) {
                        self.error(
                            value,
                            "shell interpreter must be literal (no placeholders or brace escapes)",
                        );
                    }
                    if i == 0 {
                        self.text(value, TextKind::Interpreter, platform, active);
                    }
                }
                StringRule::Source => {
                    if !syntax::source_valid(&s, platform) {
                        self.error(value, "expected a non-empty problem-relative source with / components, without . or ..");
                    }
                    let key = syntax::source_key(&s, platform);
                    if let Some(other) = seen.get(&key) {
                        value.invalid = true;
                        self.diagnostics.push(Diagnostic {
                            severity: Severity::Error,
                            message: "duplicate source".into(),
                            origin: value.origin.clone(),
                            second_origin: Some(other.clone()),
                        });
                    } else {
                        seen.insert(key, value.origin.clone());
                    }
                }
                StringRule::Extension => {
                    if s.is_empty() || s.contains(['.', '/']) {
                        self.error(
                            value,
                            "extension must be non-empty and contain neither . nor /",
                        );
                    }
                }
                StringRule::Output => {
                    if !syntax::output_relative(&s, platform) {
                        self.error(
                            value,
                            "expected an output-relative name without .., root or drive prefix",
                        );
                    }
                }
                StringRule::EnvName => self.env_name(value, &s, platform),
                StringRule::CommandName => {
                    if !syntax::name_valid(&s, false) {
                        self.error(value, "invalid command name");
                    }
                }
                StringRule::Ignore => {
                    if !syntax::ignore_valid(&s) {
                        self.error(value, "invalid workspace ignore pattern");
                    }
                }
                StringRule::Absolute => {
                    if !syntax::absolute(&s, platform) || s.contains('\0') {
                        self.error(value, "trusted workspace path must be absolute");
                    }
                }
            }
        }
        if matches!(rule, StringRule::Argv) && empty {
            self.error(n, "argv must not be empty");
        }
        if matches!(rule, StringRule::Interpreter) {
            let RawValue::Array(values) = &n.value else {
                unreachable!()
            };
            if values.len() != 2 || values.get(1).and_then(RawNode::string) != Some("-c") {
                self.error(n, "shell interpreter must have the form [program, \"-c\"]");
            }
        }
    }

    fn text(&mut self, n: &mut RawNode, kind: TextKind, platform: Platform, active: bool) {
        let Some(text) = self.string(n) else {
            return;
        };
        let tokens = if matches!(kind, TextKind::Interpreter) {
            vec![Token::Literal(text.clone())]
        } else {
            match super::tokenize(&text) {
                Ok(tokens) => tokens,
                Err(errors) => {
                    for error in errors {
                        self.error(n, error);
                    }
                    return;
                }
            }
        };
        let multi = tokens
            .iter()
            .any(|t| matches!(t, Token::Placeholder(Placeholder::Sources)));
        if multi && (tokens.len() != 1 || matches!(kind, TextKind::Program | TextKind::Cwd)) {
            self.error(n, "multi placeholder must be a whole argument and cannot occupy program or cwd position");
        }
        let has_placeholder = tokens.iter().any(|t| matches!(t, Token::Placeholder(_)));
        let mut parsed = ParsedText {
            unexpanded: text.clone().into(),
            tokens,
            path: None,
            lookup: false,
            expanded_home: None,
        };
        if !matches!(kind, TextKind::Argument) && text.is_empty() {
            self.error(n, "program or cwd path must not be empty");
        }
        if active && !matches!(kind, TextKind::Argument) {
            let literal = if has_placeholder {
                text.clone()
            } else {
                parsed
                    .tokens
                    .iter()
                    .filter_map(|token| match token {
                        Token::Literal(s) => Some(s.as_str()),
                        Token::Placeholder(_) => None,
                    })
                    .collect::<String>()
            };
            if has_placeholder
                && (text.starts_with("~/")
                    || (platform == Platform::Windows && text.starts_with("~\\")))
            {
                parsed.expanded_home = self.env.home.clone();
            }
            let expanded = match syntax::home_expand(&literal, self.env.home.as_deref(), platform) {
                Ok(s) => s,
                Err(e) => {
                    self.error(n, e);
                    return;
                }
            };
            let expanded_str = expanded.to_str();
            if platform == Platform::Windows && expanded_str.is_some_and(syntax::windows_relative) {
                self.error(
                    n,
                    "Windows drive-relative and root-relative paths are not allowed",
                );
            }
            let absolute = syntax::native_absolute(&expanded, platform);
            let starts = matches!(parsed.tokens.first(), Some(Token::Placeholder(_)));
            // Rule (3) concerns relative paths: a bare program name such as
            // `tool-{problem_id}` is a lookup token, not a path.
            let path_like = matches!(kind, TextKind::Cwd)
                || text.starts_with("~/")
                || (platform == Platform::Windows && text.starts_with("~\\"))
                || syntax::separator(&text, platform);
            if has_placeholder && !starts && !absolute && path_like {
                self.error(
                    n,
                    "relative path with a placeholder must start with a placeholder",
                );
            }
            if !has_placeholder {
                let is_path = matches!(kind, TextKind::Cwd)
                    || text.starts_with("~/")
                    || (platform == Platform::Windows && text.starts_with("~\\"))
                    || syntax::separator(&literal, platform);
                parsed.lookup = !is_path;
                if is_path {
                    parsed.path = Some(if absolute {
                        expanded.into()
                    } else {
                        syntax::join(
                            &syntax::directory(&n.origin.file, platform),
                            &expanded,
                            platform,
                        )
                    });
                }
            }
        }
        if let Some(path) = &parsed.path {
            parsed.unexpanded = path.as_os_str().to_owned();
        } else if parsed.expanded_home.is_some() {
            // Keep placeholders and brace escapes literal, using the validation
            // platform rather than the host platform of a config inspector.
            if let Ok(expanded) = syntax::home_expand(&text, self.env.home.as_deref(), platform) {
                parsed.unexpanded = expanded;
            }
        }
        n.typed = Some(TypedValue::Text(parsed));
    }

    fn path(&mut self, n: &mut RawNode, platform: Platform, active: bool) {
        let Some(text) = self.string(n) else {
            return;
        };
        if text.is_empty() || text.contains('\0') {
            self.error(n, "path must be non-empty and contain no NUL");
            return;
        }
        if !active {
            return;
        }
        let expanded = match syntax::home_expand(&text, self.env.home.as_deref(), platform) {
            Ok(s) => s,
            Err(e) => {
                self.error(n, e);
                return;
            }
        };
        if platform == Platform::Windows && expanded.to_str().is_some_and(syntax::windows_relative)
        {
            self.error(
                n,
                "Windows drive-relative and root-relative paths are not allowed",
            );
            return;
        }
        let absolute = syntax::native_absolute(&expanded, platform);
        n.typed = Some(TypedValue::Path(if absolute {
            expanded.into()
        } else {
            syntax::join(
                &syntax::directory(&n.origin.file, platform),
                &expanded,
                platform,
            )
        }));
    }

    fn integer(&mut self, n: &mut RawNode, min: u64, max: u64) {
        match n.value {
            RawValue::Integer(i) if i >= 0 && (min..=max).contains(&(i as u64)) => {
                n.typed = Some(TypedValue::Count(i as u64))
            }
            RawValue::Integer(_) => self.error(n, format!("integer must be in {min}..={max}")),
            _ => self.error(n, "expected an integer"),
        }
    }

    fn size(&mut self, n: &mut RawNode, max: u64) {
        let parsed = match &n.value {
            RawValue::Integer(i) => u64::try_from(*i).map_err(|_| "size must not be negative"),
            RawValue::String(s) => parse_size(s),
            _ => Err("expected an integer or size string"),
        };
        match parsed {
            Ok(bytes) if bytes >= 1 && bytes <= max => n.typed = Some(TypedValue::Bytes(bytes)),
            Ok(_) => self.error(n, format!("size must be in 1..={max} bytes")),
            Err(message) => self.error(n, message),
        }
    }

    fn deadline(&mut self, n: &mut RawNode) {
        // Scale straight to milliseconds so `"2003.5ms"` rounds half up exactly.
        let (number, factor) = match &n.value {
            RawValue::Integer(i) => (*i as f64, 1000.0),
            RawValue::Float(f) => (*f, 1000.0),
            RawValue::String(s) => {
                let (number, factor) = if let Some(s) = s.strip_suffix("ms") {
                    (s, 1.0)
                } else if let Some(s) = s.strip_suffix('s') {
                    (s, 1000.0)
                } else if let Some(s) = s.strip_suffix('m') {
                    (s, 60_000.0)
                } else {
                    self.error(n, "duration requires ms, s or m unit");
                    return;
                };
                match number.parse::<f64>() {
                    Ok(n) => (n, factor),
                    Err(_) => {
                        self.error(n, "invalid duration number");
                        return;
                    }
                }
            }
            _ => {
                self.error(n, "expected a number or duration string");
                return;
            }
        };
        match duration_milliseconds(number, factor) {
            Ok(ms) => n.typed = Some(TypedValue::Milliseconds(ms)),
            Err(message) => self.error(n, message),
        }
    }
}

#[derive(Clone, Copy)]
enum StringRule {
    Argv,
    Args,
    Interpreter,
    Source,
    Extension,
    Output,
    EnvName,
    CommandName,
    Ignore,
    Absolute,
}
#[derive(Clone, Copy)]
enum TextKind {
    Program,
    Argument,
    Cwd,
    Interpreter,
}

fn parse_size(text: &str) -> Result<u64, &'static str> {
    let units = [
        ("KiB", 1_u64 << 10),
        ("MiB", 1 << 20),
        ("GiB", 1 << 30),
        ("TiB", 1 << 40),
        ("kB", 1000),
        ("MB", 1_000_000),
        ("GB", 1_000_000_000),
        ("TB", 1_000_000_000_000),
    ];
    let Some((digits, factor)) = units
        .iter()
        .find_map(|(unit, factor)| text.strip_suffix(unit).map(|d| (d, *factor)))
    else {
        return Err("invalid size unit (expected KiB, MiB, GiB, TiB, kB, MB, GB or TB)");
    };
    let magnitude = digits.strip_prefix('-').unwrap_or(digits);
    if magnitude.is_empty() || !magnitude.bytes().all(|b| b.is_ascii_digit()) {
        return Err("size must be a whole number of units");
    }
    if digits.starts_with('-') {
        return Err("size must not be negative");
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(|v| v.checked_mul(factor))
        .ok_or("size overflows u64")
}

impl Validator<'_> {
    fn compare(&mut self, n: &mut RawNode) {
        let Some(t) = n.table() else {
            self.error(n, "expected a comparator table such as { tokens = true }");
            return;
        };
        let selectors: Vec<_> = t
            .keys()
            .filter(|k| {
                matches!(
                    k.as_str(),
                    "exact" | "tokens" | "lines" | "float" | "float_abs" | "float_rel"
                )
            })
            .cloned()
            .collect();
        if selectors.is_empty() {
            self.error(n, "compare requires exactly one comparator selector");
        }
        for selector in selectors.iter().skip(1) {
            self.conflict(n, &selectors[0], selector);
        }
        let RawValue::Table(t) = &mut n.value else {
            unreachable!()
        };
        let mut selected = None;
        for (key, value) in t {
            match key.as_str() {
                "exact" | "tokens" | "lines" => {
                    if !matches!(value.value, RawValue::Boolean(true)) {
                        self.error(value, "comparator selector must be true");
                    }
                    selected = Some(match key.as_str() {
                        "exact" => Comparator::Exact,
                        "tokens" => Comparator::Tokens,
                        _ => Comparator::Lines,
                    });
                }
                "float" if value.table().is_some() => {
                    selected = self.float_compare(value).map(Comparator::Float);
                }
                "float" | "float_abs" | "float_rel" => {
                    if let Some(bound) = self.bound(value) {
                        selected = Some(Comparator::Float(FloatComparator {
                            abs: (key != "float_rel").then_some(bound),
                            rel: (key != "float_abs").then_some(bound),
                            ulps: None,
                            rel_base: RelativeBase::Answer,
                            combine: Combine::Either,
                            int_exact: true,
                        }));
                    }
                }
                _ => self.unknown(value),
            }
        }
        if !n.has_errors() {
            n.typed = selected.map(TypedValue::Compare);
        }
    }

    fn bound(&mut self, n: &mut RawNode) -> Option<f64> {
        let bound = match n.value {
            RawValue::Integer(i) => i as f64,
            RawValue::Float(f) => f,
            _ => {
                self.error(n, "float bound must be a TOML float or integer");
                return None;
            }
        };
        if !bound.is_finite() {
            self.error(n, "float bound must be finite");
            None
        } else if bound < 0.0 {
            self.error(n, "float bound must not be negative");
            None
        } else {
            Some(if bound == 0.0 { 0.0 } else { bound })
        }
    }

    fn float_compare(&mut self, n: &mut RawNode) -> Option<FloatComparator> {
        let t = n.table().expect("float table checked by caller");
        let abs = t.contains_key("abs");
        let rel = t.contains_key("rel");
        let ulps = t.contains_key("ulps");
        if !abs && !rel && !ulps {
            self.error(n, "float comparator requires abs, rel or ulps");
        }
        if ulps {
            for key in ["abs", "rel", "rel_base", "combine"] {
                self.conflict(n, "ulps", key);
            }
        }
        let RawValue::Table(t) = &mut n.value else {
            unreachable!()
        };
        let mut result = FloatComparator {
            abs: None,
            rel: None,
            ulps: None,
            rel_base: RelativeBase::Answer,
            combine: Combine::Either,
            int_exact: true,
        };
        for (key, value) in t {
            match key.as_str() {
                "abs" => result.abs = self.bound(value),
                "rel" => result.rel = self.bound(value),
                "ulps" => {
                    self.integer(value, 0, i64::MAX as u64);
                    if let Some(TypedValue::Count(count)) = value.typed {
                        result.ulps = Some(count);
                    }
                }
                "rel_base" => {
                    if !rel && !ulps {
                        self.error(value, "rel_base requires rel");
                    }
                    self.choice(value, &["answer", "max", "max1"]);
                    result.rel_base = match value.string() {
                        Some("max") => RelativeBase::Max,
                        Some("max1") => RelativeBase::Max1,
                        _ => RelativeBase::Answer,
                    };
                }
                "combine" => {
                    if !(ulps || abs && rel) {
                        self.error(value, "combine requires both abs and rel");
                    }
                    self.choice(value, &["either", "both"]);
                    if value.string() == Some("both") {
                        result.combine = Combine::Both;
                    }
                }
                "int_exact" => {
                    self.boolean(value);
                    if let RawValue::Boolean(b) = value.value {
                        result.int_exact = b;
                    }
                }
                _ => self.unknown(value),
            }
        }
        if n.has_errors() { None } else { Some(result) }
    }
}

/// Parse a CLI duration with the same units, rounding and bounds as deadline_s.
/// A bare number is seconds, as for numeric configuration values.
pub fn parse_deadline(value: &str) -> Result<u64, &'static str> {
    let (number, factor) = if let Some(n) = value.strip_suffix("ms") {
        (n, 1.0)
    } else if let Some(n) = value.strip_suffix('s') {
        (n, 1000.0)
    } else if let Some(n) = value.strip_suffix('m') {
        (n, 60_000.0)
    } else {
        (value, 1000.0)
    };
    let number = number
        .parse::<f64>()
        .map_err(|_| "invalid duration; use a number of seconds or ms, s or m")?;
    duration_milliseconds(number, factor)
}

fn duration_milliseconds(number: f64, factor: f64) -> Result<u64, &'static str> {
    // Check the written value before rounding; overflowing products fail range.
    if !number.is_finite() {
        return Err("duration must be finite");
    }
    if number < 0.0 {
        return Err("duration must not be negative");
    }
    let ms = (number * factor).round();
    if !(1.0..=86_400_000.0).contains(&ms) {
        return Err("rounded duration must be in 1..=86400000 milliseconds");
    }
    Ok(ms as u64)
}
