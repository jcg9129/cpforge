use std::ffi::OsString;
use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;

use crate::config::{
    Artifacts, CommandPlan, ConfigRequest, EnvEdit, Environment, HookPlan, LoadedConfig,
    Placeholder, Platform, Provenance, StageCommand, StagePlan, TargetPlan, load_config, tokenize,
};

use super::*;

#[derive(Debug)]
pub(super) struct Directory(PathBuf);

impl Directory {
    pub(super) fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("exec-prepare-{:032x}", u128::from_le_bytes(bits)));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub(super) fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn home_tail(&self, tail: &str) -> PathBuf {
        let mut path = self.path("home-{source}").into_os_string();
        path.push(if cfg!(windows) { "\\" } else { "/" });
        path.push(tail);
        path.into()
    }

    fn load(&self, toml: &str, language: Option<&str>) -> LoadedConfig {
        fs::write(self.path("config.toml"), toml).unwrap();
        load_config(&ConfigRequest {
            environment: Environment {
                // Braces in the captured home must never be rescanned.
                home: Some(self.path("home-{source}")),
                ..Environment::default()
            },
            current_dir: self.0.clone(),
            config_path: Some(self.path("config.toml")),
            language: language.map(str::to_owned),
            ..ConfigRequest::default()
        })
    }

    pub(super) fn command(&self, body: &str) -> CommandPlan {
        self.load(&format!("[commands.c]\n{body}\n"), None)
            .command("c")
            .unwrap()
    }

    pub(super) fn target(&self, language: &str, extra: &str) -> TargetPlan {
        self.load(extra, Some(language)).target().unwrap()
    }

    pub(super) fn dirs(&self) -> InvocationDirs {
        InvocationDirs {
            run_cwd: self.path("job/cwd"),
            problem: Some(ProblemBinding {
                dir: self.path("ws/problems/p"),
                id: "p".into(),
            }),
            workspace: Some(self.path("ws")),
        }
    }

    pub(super) fn bindings(&self, target: &TargetPlan, count: usize) -> TargetBindings {
        TargetBindings {
            sources: (0..count)
                .map(|i| self.path(&format!("job/inputs/src/{i}.cpp")))
                .collect(),
            artifact: matches!(target.artifacts, Some(Artifacts::File { .. }))
                .then(|| self.path("job/out/main")),
            artifact_dir: target.build.as_ref().map(|_| self.path("job/out")),
            entry: target.entry.as_ref().map(|entry| entry.value.clone()),
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn strings(values: &[&str]) -> Vec<OsString> {
    values
        .iter()
        .map(|value: &&str| OsString::from(*value))
        .collect()
}

fn pairs(values: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    values
        .iter()
        .map(|(name, value)| (OsString::from(*name), OsString::from(*value)))
        .collect()
}

pub(super) fn prepared(stage: &StagePlan, ctx: &Context) -> PreparedStage {
    prepare(stage, ctx, &Baseline::default(), Platform::host(), &[]).unwrap()
}

fn program(stage: &PreparedStage) -> &ProgramRequest {
    let PreparedCommand::Argv { program, .. } = &stage.command else {
        panic!("argv expected")
    };
    program
}

fn args(stage: &PreparedStage) -> &[OsString] {
    match &stage.command {
        PreparedCommand::Argv { args, .. } | PreparedCommand::Shell { args, .. } => args,
    }
}

pub(super) fn command_context(plan: &CommandPlan, dirs: InvocationDirs) -> Context {
    Context::command(plan, dirs, CommandBindings::default()).unwrap()
}

fn kinds(errors: &PrepareErrors) -> Vec<PrepareErrorKind> {
    errors
        .errors
        .iter()
        .map(|error| error.kind.clone())
        .collect()
}

fn replace_argument(stage: &mut StagePlan, value: &str) {
    let StageCommand::Argv(argv) = &mut stage.command else {
        panic!("argv expected")
    };
    argv[1].tokens = tokenize(value).unwrap();
}

#[test]
fn argv_quoting_and_metacharacters_are_single_literal_elements() {
    let dir = Directory::new();
    let plan = dir.command(r#"argv = ['prog', 'a b', '"q"', 'back\slash', '$HOME', 'a;b|c&d>e<f', '*', '%PATH%', '^', '{{x}}']"#);
    let ctx = command_context(&plan, dir.dirs());
    let result = prepared(&plan.stage, &ctx);
    assert_eq!(program(&result), &ProgramRequest::Lookup("prog".into()));
    assert_eq!(
        args(&result),
        strings(&[
            "a b",
            "\"q\"",
            "back\\slash",
            "$HOME",
            "a;b|c&d>e<f",
            "*",
            "%PATH%",
            "^",
            "{x}"
        ])
    );
    assert_eq!(result.plan, plan.stage);
    assert_eq!(result.deadline, plan.stage.deadline);
    assert_eq!(result.stdout, plan.stage.stdout);
    assert_eq!(result.stderr, plan.stage.stderr);
    assert_eq!(result.limits, plan.stage.limits);
}

#[test]
fn sources_expand_to_zero_one_or_many_supplied_captured_paths() {
    let dir = Directory::new();
    let target = dir.target(
        "cpp",
        "[languages.cpp.build]\nargv = ['cc', '{sources}', '-o{artifact}']",
    );
    for count in [0, 1, 3] {
        let mut bindings = dir.bindings(&target, count);
        // Substituted braces are native data, not another expansion pass.
        if let Some(source) = bindings.sources.first_mut() {
            *source = dir.path("job/inputs/src/{test_input} a.cpp");
        }
        let expected_sources: Vec<OsString> = bindings
            .sources
            .iter()
            .map(|p| p.as_os_str().to_owned())
            .collect();
        let mut output = OsString::from("-o");
        output.push(bindings.artifact.as_ref().unwrap());
        let ctx = Context::target_build(&target, dir.dirs(), bindings).unwrap();
        let result = prepared(target.build.as_ref().unwrap(), &ctx);
        let mut expected = expected_sources;
        expected.push(output);
        assert_eq!(args(&result), expected);
    }
}

#[test]
fn source_requires_exactly_one_and_reports_the_count() {
    let dir = Directory::new();
    let target = dir.target("python", "");
    for count in [0, 1, 2] {
        let bindings = dir.bindings(&target, count);
        let expected: Vec<OsString> = bindings
            .sources
            .iter()
            .map(|p| p.as_os_str().to_owned())
            .collect();
        let ctx = Context::target_run(&target, dir.dirs(), bindings).unwrap();
        let result = prepare(
            &target.run,
            &ctx,
            &Baseline::default(),
            Platform::host(),
            &[],
        );
        if count == 1 {
            assert_eq!(args(&result.unwrap()), expected);
        } else {
            let errors = result.unwrap_err();
            assert_eq!(kinds(&errors), [PrepareErrorKind::SourceCount(count)]);
            assert!(errors.to_string().contains(&format!("found {count}")));
            assert_eq!(errors.errors[0].placeholder, Some(Placeholder::Source));
        }
    }
}

#[test]
fn artifact_capabilities_and_language_entry_have_distinct_failures() {
    let dir = Directory::new();
    for (language, placeholder, expected) in [
        ("java", "artifact", PrepareErrorKind::NoFileArtifact),
        ("python", "artifact_dir", PrepareErrorKind::NoBuild),
        ("python", "entry", PrepareErrorKind::NoEntry),
        ("python", "test_input", PrepareErrorKind::CommandOnly),
        ("python", "iteration", PrepareErrorKind::CommandOnly),
    ] {
        let target = dir.target(
            language,
            &format!("[languages.{language}.run]\nargv = ['prog', '{{{placeholder}}}']"),
        );
        let ctx = Context::target_run(&target, dir.dirs(), dir.bindings(&target, 1)).unwrap();
        let errors = prepare(
            &target.run,
            &ctx,
            &Baseline::default(),
            Platform::host(),
            &[],
        )
        .unwrap_err();
        assert_eq!(kinds(&errors), [expected]);
        assert_eq!(errors.errors[0].origin.file, dir.path("config.toml"));
        assert_eq!(errors.errors[0].stage, crate::config::StageKind::Run);
    }
    let python = dir.target("python", "");
    let error = Context::target_build(&python, dir.dirs(), dir.bindings(&python, 1)).unwrap_err();
    assert_eq!(kinds(&error), [PrepareErrorKind::NoBuild]);
}

#[test]
fn program_requests_use_phase_one_paths_and_classify_expanded_text() {
    let dir = Directory::new();
    for (text, expected) in [
        ("bin/tool", ProgramRequest::Path(dir.path("bin/tool"))),
        ("g++", ProgramRequest::Lookup("g++".into())),
        (
            "{workspace}/tool",
            ProgramRequest::Path(PathBuf::from({
                let mut path = dir.path("ws").into_os_string();
                path.push("/tool");
                path
            })),
        ),
        ("tool-{problem_id}", ProgramRequest::Lookup("tool-p".into())),
        (
            "~/bin/{problem_id}",
            ProgramRequest::Path(dir.home_tail("bin/p")),
        ),
    ] {
        let plan = dir.command(&format!("argv = ['{text}']"));
        let ctx = command_context(&plan, dir.dirs());
        let result = prepared(&plan.stage, &ctx);
        assert_eq!(program(&result), &expected, "{text}");
        if let (ProgramRequest::Path(actual), ProgramRequest::Path(expected)) =
            (program(&result), &expected)
        {
            assert_eq!(actual.as_os_str(), expected.as_os_str(), "{text}");
        }
    }
    let plan = dir.command("argv = ['{problem_id}/tool']");
    let ctx = command_context(&plan, dir.dirs());
    let errors = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(kinds(&errors), [PrepareErrorKind::NotAbsolute]);

    let plan = dir.command("argv = ['{problem_id}']");
    let mut dirs = dir.dirs();
    dirs.problem.as_mut().unwrap().id.clear();
    let ctx = command_context(&plan, dirs);
    let errors = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(kinds(&errors), [PrepareErrorKind::EmptyProgram]);
}

#[test]
fn windows_expanded_paths_reject_drive_relative_and_root_relative_forms() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['{problem_id}']");
    for value in ["C:tool", "C:dir\\tool", "\\tool", "/tool"] {
        let dirs = InvocationDirs {
            run_cwd: "C:\\job\\cwd".into(),
            problem: Some(ProblemBinding {
                dir: "C:\\problems\\p".into(),
                id: value.into(),
            }),
            workspace: Some("C:\\ws".into()),
        };
        let ctx = command_context(&plan, dirs);
        let errors = prepare(
            &plan.stage,
            &ctx,
            &Baseline::default(),
            Platform::Windows,
            &[],
        )
        .unwrap_err();
        assert_eq!(kinds(&errors), [PrepareErrorKind::NotAbsolute], "{value}");
    }
    for value in ["C:\\bin\\tool", "C:/bin/tool", "\\\\server\\share\\tool"] {
        let ctx = command_context(
            &plan,
            InvocationDirs {
                run_cwd: "C:\\job\\cwd".into(),
                problem: Some(ProblemBinding {
                    dir: "C:\\p".into(),
                    id: value.into(),
                }),
                workspace: Some("C:\\ws".into()),
            },
        );
        let result = prepare(
            &plan.stage,
            &ctx,
            &Baseline::default(),
            Platform::Windows,
            &[],
        )
        .unwrap();
        assert_eq!(program(&result), &ProgramRequest::Path(value.into()));
    }
}

#[test]
fn undeclared_bindings_are_refused_and_cannot_enable_placeholders() {
    let dir = Directory::new();
    let loaded = dir.load("[commands.c]\nargv = ['prog', '{test_input}']", None);
    assert!(loaded.command("c").is_err());
    assert!(
        loaded
            .diagnostics()
            .iter()
            .any(|d| d.message == "test placeholders require for_each_test = true")
    );
    let mut plan = dir.command("argv = ['prog', 'safe']");
    // Plans have editable public fields. Exercise the preparation defence even
    // when a caller has bypassed the phase-two declaration check by editing one.
    replace_argument(&mut plan.stage, "{test_input}");
    let errors = Context::command(
        &plan,
        dir.dirs(),
        CommandBindings {
            test: Some(TestBinding {
                input: dir.path("test/in"),
                answer: None,
            }),
            ..CommandBindings::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        kinds(&errors),
        [PrepareErrorKind::Undeclared {
            declaration: "for_each_test = true"
        }]
    );
    let ctx = command_context(&plan, dir.dirs());
    let errors = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(
        kinds(&errors),
        [PrepareErrorKind::Undeclared {
            declaration: "for_each_test = true"
        }]
    );

    for (placeholder, declaration) in [
        ("sources", "needs_build = true"),
        ("source", "needs_build = true"),
        ("iteration", "repeat"),
    ] {
        replace_argument(&mut plan.stage, &format!("{{{placeholder}}}"));
        let ctx = command_context(&plan, dir.dirs());
        let errors = prepare(
            &plan.stage,
            &ctx,
            &Baseline::default(),
            Platform::host(),
            &[],
        )
        .unwrap_err();
        assert_eq!(
            kinds(&errors),
            [PrepareErrorKind::Undeclared { declaration }]
        );
    }
}

#[test]
fn all_supplied_undeclared_bindings_are_reported_at_construction() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog']");
    let target = dir.target("cpp", "");
    let errors = Context::command(
        &plan,
        dir.dirs(),
        CommandBindings {
            target: Some(dir.bindings(&target, 1)),
            test: Some(TestBinding {
                input: dir.path("test/in"),
                answer: None,
            }),
            iteration: NonZeroU64::new(1),
            language: None,
        },
    )
    .unwrap_err();
    assert_eq!(errors.errors.len(), 3);
    assert_eq!(
        errors
            .errors
            .iter()
            .map(|e| e.field.as_str())
            .collect::<Vec<_>>(),
        ["target", "test", "iteration"]
    );
    assert_eq!(
        kinds(&errors),
        [
            PrepareErrorKind::Undeclared {
                declaration: "needs_build = true"
            },
            PrepareErrorKind::Undeclared {
                declaration: "for_each_test = true"
            },
            PrepareErrorKind::Undeclared {
                declaration: "repeat"
            },
        ]
    );
}

#[test]
fn hooks_expose_only_problem_and_workspace_identity() {
    let dir = Directory::new();
    let loaded = dir.load(
        "[hooks]\non_switch = ['h']\n[commands.h]\nargv = ['prog', '{problem_id}', '{workspace}']",
        None,
    );
    let hook = loaded.hooks().unwrap().on_switch.remove(0);
    let ctx = Context::hook(&hook, dir.dirs()).unwrap();
    assert_eq!(
        args(&prepared(&hook.stage, &ctx)),
        [OsString::from("p"), dir.path("ws").into_os_string()]
    );
    let mut hook: HookPlan = hook;
    replace_argument(&mut hook.stage, "{sources}");
    let ctx = Context::hook(&hook, dir.dirs()).unwrap();
    let errors = prepare(
        &hook.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(kinds(&errors), [PrepareErrorKind::HookPlaceholder]);
}

#[test]
fn test_answer_is_unavailable_for_a_test_without_an_answer() {
    let dir = Directory::new();
    let plan =
        dir.command("argv = ['prog', '{test_input}', '{test_answer}']\nfor_each_test = true");
    let ctx = Context::command(
        &plan,
        dir.dirs(),
        CommandBindings {
            test: Some(TestBinding {
                input: dir.path("job/test/in"),
                answer: None,
            }),
            ..CommandBindings::default()
        },
    )
    .unwrap();
    let errors = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(kinds(&errors), [PrepareErrorKind::NoTestAnswer]);
    assert_eq!(errors.errors[0].placeholder, Some(Placeholder::TestAnswer));
}

#[test]
fn command_bindings_use_declarations_including_explicit_repeat_one() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog', '{sources}', '{source}', '-o{artifact}', '{artifact_dir}', '{entry}', '{test_input}', '{test_answer}', '{iteration}']\nneeds_build = true\nfor_each_test = true\nrepeat = 1");
    let target = dir.target("cpp", "[languages.cpp]\nentry = 'Main'");
    let target_bindings = dir.bindings(&target, 1);
    let source = target_bindings.sources[0].as_os_str().to_owned();
    let ctx = Context::command(
        &plan,
        dir.dirs(),
        CommandBindings {
            target: Some(target_bindings),
            test: Some(TestBinding {
                input: dir.path("test/in"),
                answer: Some(dir.path("test/ans")),
            }),
            iteration: NonZeroU64::new(1),
            language: None,
        },
    )
    .unwrap();
    let mut output = OsString::from("-o");
    output.push(dir.path("job/out/main"));
    assert_eq!(
        args(&prepared(&plan.stage, &ctx)),
        [
            source.clone(),
            source,
            output,
            dir.path("job/out").into_os_string(),
            "Main".into(),
            dir.path("test/in").into_os_string(),
            dir.path("test/ans").into_os_string(),
            "1".into()
        ]
    );
    let errors = Context::command(&plan, dir.dirs(), CommandBindings::default()).unwrap_err();
    assert_eq!(errors.errors.len(), 3);
}

#[test]
fn command_entry_is_independent_of_needs_build() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog', '{entry}']");
    let ctx = Context::command(
        &plan,
        dir.dirs(),
        CommandBindings {
            language: Some(LanguageBinding::from_target(
                &dir.target("java", "[languages.java]\nentry = 'a.b.Main'"),
            )),
            ..CommandBindings::default()
        },
    )
    .unwrap();
    assert_eq!(args(&prepared(&plan.stage, &ctx)), strings(&["a.b.Main"]));
}

#[test]
fn missing_identity_and_cwd_report_independent_origins_together() {
    let dir = Directory::new();
    let plan = dir
        .command("argv = ['prog', '{problem_dir}', '{workspace}']\ncwd = { selector = 'problem' }");
    let ctx = command_context(
        &plan,
        InvocationDirs {
            run_cwd: dir.path("cwd"),
            problem: None,
            workspace: None,
        },
    );
    let errors = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(
        kinds(&errors),
        [
            PrepareErrorKind::NoProblem,
            PrepareErrorKind::NoWorkspace,
            PrepareErrorKind::NoProblem
        ]
    );
    assert_eq!(
        errors
            .errors
            .iter()
            .map(|e| e.placeholder)
            .collect::<Vec<_>>(),
        [
            Some(Placeholder::ProblemDir),
            Some(Placeholder::Workspace),
            None
        ]
    );
    assert_eq!(
        errors
            .errors
            .iter()
            .map(|e| e.origin.key_path.as_str())
            .collect::<Vec<_>>(),
        [
            "commands.c.argv[1]",
            "commands.c.argv[2]",
            "commands.c.cwd.selector"
        ]
    );
    for error in &errors.errors {
        assert_eq!(error.stage, crate::config::StageKind::Command);
        assert_eq!(error.origin.file, dir.path("config.toml"));
        assert!(matches!(error.provenance, Provenance::File { .. }));
        assert!(errors.to_string().contains(&error.field));
    }
}

#[test]
fn expanded_nul_is_rejected_in_identity_and_native_source_values() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog', '{problem_id}']");
    let mut dirs = dir.dirs();
    dirs.problem.as_mut().unwrap().id = "p\0q".into();
    let ctx = command_context(&plan, dirs);
    let errors = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(kinds(&errors), [PrepareErrorKind::Nul]);
    assert_eq!(errors.errors[0].placeholder, Some(Placeholder::ProblemId));
    let target = dir.target("cpp", "");
    let mut bindings = dir.bindings(&target, 1);
    bindings.sources[0] = dir.path("src/a\0.cpp");
    let ctx = Context::target_build(&target, dir.dirs(), bindings).unwrap();
    let errors = prepare(
        target.build.as_ref().unwrap(),
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(kinds(&errors), [PrepareErrorKind::Nul]);
    assert_eq!(errors.errors[0].placeholder, Some(Placeholder::Sources));
}

#[test]
fn cwd_selectors_and_paths_record_shared_cwd() {
    let dir = Directory::new();
    for (cwd, expected, shared) in [
        ("", dir.path("job/cwd"), false),
        ("cwd = { selector = 'run' }", dir.path("job/cwd"), false),
        (
            "cwd = { selector = 'problem' }",
            dir.path("ws/problems/p"),
            true,
        ),
        ("cwd = { selector = 'workspace' }", dir.path("ws"), true),
        ("cwd = { path = 'work' }", dir.path("work"), true),
        (
            "cwd = { path = '{problem_dir}' }",
            dir.path("ws/problems/p"),
            true,
        ),
        (
            "cwd = { path = '~/work/{problem_id}' }",
            dir.home_tail("work/p"),
            true,
        ),
    ] {
        let plan = dir.command(&format!("argv = ['prog']\n{cwd}"));
        let ctx = command_context(&plan, dir.dirs());
        let result = prepared(&plan.stage, &ctx);
        assert_eq!(result.cwd.as_os_str(), expected.as_os_str());
        assert_eq!(result.shared_cwd, shared);
    }
    let plan = dir.command("argv = ['prog']\ncwd = { path = '{problem_id}/work' }");
    let ctx = command_context(&plan, dir.dirs());
    assert_eq!(
        kinds(
            &prepare(
                &plan.stage,
                &ctx,
                &Baseline::default(),
                Platform::host(),
                &[]
            )
            .unwrap_err()
        ),
        [PrepareErrorKind::NotAbsolute]
    );
    let plan = dir.command("argv = ['prog']\ncwd = { selector = 'workspace' }");
    let mut dirs = dir.dirs();
    dirs.workspace = None;
    let ctx = command_context(&plan, dirs);
    assert_eq!(
        kinds(
            &prepare(
                &plan.stage,
                &ctx,
                &Baseline::default(),
                Platform::host(),
                &[]
            )
            .unwrap_err()
        ),
        [PrepareErrorKind::NoWorkspace]
    );
}

#[test]
fn environment_edits_replace_in_place_and_append_new_names() {
    let dir = Directory::new();
    let plan =
        dir.command("argv = ['prog']\nenv_remove = ['C']\nenv = { A = false, B = 'x', D = 'y' }");
    assert!(matches!(
        &plan.stage.environment[..],
        [
            EnvEdit::Remove(_),
            EnvEdit::Unset(_),
            EnvEdit::Set { .. },
            EnvEdit::Set { .. }
        ]
    ));
    let baseline = Baseline::from_entries(pairs(&[("A", "1"), ("B", "2"), ("C", "3")]));
    let snapshot = baseline.clone();
    let ctx = command_context(&plan, dir.dirs());
    let result = prepare(&plan.stage, &ctx, &baseline, Platform::host(), &[]).unwrap();
    assert_eq!(result.environment, pairs(&[("B", "x"), ("D", "y")]));
    assert_eq!(baseline, snapshot);
}

#[test]
fn environment_name_equality_and_winning_spelling_follow_the_platform() {
    let dir = Directory::new();
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let plan = dir.command("argv = ['prog']\nenv = { PATH = 'z', 'ä' = 'new' }");
        let mut dirs = dir.dirs();
        dirs.run_cwd = if platform == Platform::Windows {
            "C:\\job\\cwd".into()
        } else {
            "/job/cwd".into()
        };
        let ctx = command_context(&plan, dirs.clone());
        let baseline = Baseline::from_entries(pairs(&[("Path", "old"), ("Ä", "old")]));
        let result = prepare(&plan.stage, &ctx, &baseline, platform, &[]).unwrap();
        let expected = if platform == Platform::Windows {
            pairs(&[("PATH", "z"), ("ä", "new")])
        } else {
            pairs(&[("Path", "old"), ("Ä", "old"), ("PATH", "z"), ("ä", "new")])
        };
        assert_eq!(result.environment, expected);
        let plan = dir.command("argv = ['prog']\nenv_remove = ['path']");
        let ctx = command_context(&plan, dirs);
        let result = prepare(&plan.stage, &ctx, &baseline, platform, &[]).unwrap();
        assert_eq!(
            result.environment,
            if platform == Platform::Windows {
                pairs(&[("Ä", "old")])
            } else {
                baseline.entries().to_vec()
            }
        );
    }
    let plan = dir.command("argv = ['prog']\nenv = { PATH = 'upper' }");
    let mut dirs = dir.dirs();
    dirs.run_cwd = "/job/cwd".into();
    let ctx = command_context(&plan, dirs);
    let baseline = Baseline::from_entries(pairs(&[("path", "lower")]));
    let result = prepare(&plan.stage, &ctx, &baseline, Platform::Linux, &[]).unwrap();
    assert_eq!(
        result.environment,
        pairs(&[("path", "lower"), ("PATH", "upper")])
    );
}

#[test]
fn environment_tombstones_and_baseline_duplicates_use_the_same_fold() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog']\nenv = { path = false }");
    let mut dirs = dir.dirs();
    dirs.run_cwd = "C:\\job\\cwd".into();
    let ctx = command_context(&plan, dirs);
    let baseline =
        Baseline::from_entries(pairs(&[("Path", "first"), ("B", "2"), ("PATH", "last")]));
    assert_eq!(
        prepare(&plan.stage, &ctx, &baseline, Platform::Windows, &[])
            .unwrap()
            .environment,
        pairs(&[("B", "2")])
    );
}

#[test]
fn extra_args_and_environment_values_are_verbatim() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog', 'configured']\nenv = { E = '{sources} $HOME' }");
    let ctx = command_context(&plan, dir.dirs());
    let extra = strings(&["{sources}", "{{x}}", "a b", "$HOME", "a;b"]);
    let result = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &extra,
    )
    .unwrap();
    let mut expected = strings(&["configured"]);
    expected.extend(extra);
    assert_eq!(args(&result), expected);
    assert_eq!(result.environment, pairs(&[("E", "{sources} $HOME")]));
}

#[test]
fn shell_args_expand_while_interpreter_and_script_remain_literal() {
    let dir = Directory::new();
    let plan = dir.command("shell = { interpreter = ['bin/sh', '-c'], script = 'echo {x} \"$1\"', args = ['{problem_id}', 'a b', '{{x}}'] }");
    let StageCommand::Shell {
        interpreter,
        script,
        ..
    } = &plan.stage.command
    else {
        panic!("shell expected")
    };
    assert_eq!(interpreter.program.path, Some(dir.path("bin/sh")));
    let ctx = command_context(&plan, dir.dirs());
    let result = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &strings(&["{sources}", "$1"]),
    )
    .unwrap();
    let PreparedCommand::Shell {
        interpreter: actual,
        script: actual_script,
        args,
    } = result.command
    else {
        panic!("shell expected")
    };
    assert_eq!(actual, *interpreter);
    assert_eq!(actual_script, *script);
    assert_eq!(actual_script.value, "echo {x} \"$1\"");
    assert_eq!(actual.args[0].value, "-c");
    assert_eq!(args, strings(&["p", "a b", "{x}", "{sources}", "$1"]));
}

#[test]
fn context_cannot_be_reused_with_another_stage_plan() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog']");
    let ctx = command_context(&plan, dir.dirs());
    let other = dir.command("argv = ['other']");
    let errors = prepare(
        &other.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(kinds(&errors), [PrepareErrorKind::ContextMismatch]);
}

#[cfg(unix)]
#[test]
fn native_non_utf8_bindings_and_baselines_survive_without_loss() {
    use std::os::unix::ffi::OsStringExt;
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog', '{problem_dir}']");
    let mut dirs = dir.dirs();
    let mut native_path = dir.path("ws/").into_os_string();
    native_path.push(OsString::from_vec(vec![0xff, b'{', b'x', b'}']));
    dirs.problem.as_mut().unwrap().dir = native_path.clone().into();
    let ctx = command_context(&plan, dirs);
    let native_name = OsString::from_vec(vec![b'A', 0xff]);
    let native_value = OsString::from_vec(vec![0xfe, b'X']);
    let baseline = Baseline::from_entries(vec![(native_name.clone(), native_value.clone())]);
    let result = prepare(&plan.stage, &ctx, &baseline, Platform::Linux, &[]).unwrap();
    assert_eq!(args(&result), [native_path]);
    assert_eq!(result.environment, [(native_name, native_value)]);
}

#[cfg(windows)]
#[test]
fn native_surrogate_bindings_and_baselines_survive_without_loss() {
    use std::os::windows::ffi::OsStringExt;
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog', '{problem_dir}']\nenv_remove = ['ä']");
    let mut dirs = dir.dirs();
    let mut native_path = dir.path("ws").into_os_string();
    native_path.push(OsString::from_wide(&[b'\\' as u16, 0xd800]));
    dirs.problem.as_mut().unwrap().dir = native_path.clone().into();
    let ctx = command_context(&plan, dirs);
    let name = OsString::from_wide(&[0x00e4, 0xd800]);
    let value = OsString::from_wide(&[0xdc00]);
    let baseline = Baseline::from_entries(vec![
        (name.clone(), value.clone()),
        ("Ä".into(), "removed".into()),
    ]);
    let result = prepare(&plan.stage, &ctx, &baseline, Platform::Windows, &[]).unwrap();
    assert_eq!(args(&result), [native_path]);
    assert_eq!(result.environment, [(name, value)]);
}

#[test]
fn declared_command_dependencies_require_identity_even_without_placeholders() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog']\nneeds_build = true");
    let target = dir.target("cpp", "");
    let ctx = Context::command(
        &plan,
        InvocationDirs {
            run_cwd: dir.path("job/cwd"),
            problem: None,
            workspace: None,
        },
        CommandBindings {
            target: Some(dir.bindings(&target, 1)),
            ..CommandBindings::default()
        },
    )
    .unwrap();
    let errors = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert_eq!(
        kinds(&errors),
        [PrepareErrorKind::NoProblem, PrepareErrorKind::NoWorkspace]
    );
    assert!(
        errors
            .errors
            .iter()
            .all(|error| error.origin == plan.needs_build.origin
                && error.provenance == plan.needs_build.provenance)
    );
}

#[test]
fn iteration_is_one_based_and_bounded_by_the_declared_count() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog', '{iteration}']\nrepeat = 3");
    let ctx = Context::command(
        &plan,
        dir.dirs(),
        CommandBindings {
            iteration: NonZeroU64::new(3),
            ..CommandBindings::default()
        },
    )
    .unwrap();
    assert_eq!(args(&prepared(&plan.stage, &ctx)), strings(&["3"]));
    let errors = Context::command(
        &plan,
        dir.dirs(),
        CommandBindings {
            iteration: NonZeroU64::new(4),
            ..CommandBindings::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        kinds(&errors),
        [PrepareErrorKind::InvalidBinding(
            "iteration exceeds the declared repeat count"
        )]
    );
    assert_eq!(errors.errors[0].origin, plan.repeat.origin);
}

#[test]
fn target_bindings_must_match_the_declared_artifact_and_entry_capabilities() {
    let dir = Directory::new();
    let target = dir.target("java", "");
    let mut bindings = dir.bindings(&target, 1);
    bindings.artifact = Some(dir.path("out/file"));
    bindings.artifact_dir = None;
    bindings.entry = None;
    let errors = Context::target_run(&target, dir.dirs(), bindings).unwrap_err();
    assert_eq!(
        errors
            .errors
            .iter()
            .map(|error| error.field.as_str())
            .collect::<Vec<_>>(),
        ["artifact_dir", "artifact", "entry"]
    );
}

#[test]
fn cwd_expansion_obeys_windows_absolute_path_rules() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog']\ncwd = { path = '{problem_id}' }");
    for (value, valid) in [
        ("C:work", false),
        ("\\work", false),
        ("/work", false),
        ("C:\\work", true),
        ("C:/work", true),
        ("\\\\server\\share\\work", true),
    ] {
        let ctx = command_context(
            &plan,
            InvocationDirs {
                run_cwd: "C:\\job\\cwd".into(),
                problem: Some(ProblemBinding {
                    dir: "C:\\p".into(),
                    id: value.into(),
                }),
                workspace: Some("C:\\ws".into()),
            },
        );
        let result = prepare(
            &plan.stage,
            &ctx,
            &Baseline::default(),
            Platform::Windows,
            &[],
        );
        if valid {
            let result = result.unwrap();
            assert_eq!(result.cwd.as_os_str(), std::ffi::OsStr::new(value));
            assert!(result.shared_cwd);
        } else {
            assert_eq!(kinds(&result.unwrap_err()), [PrepareErrorKind::NotAbsolute]);
        }
    }
}

#[test]
fn shell_multi_args_expand_without_lowering_the_script() {
    let dir = Directory::new();
    let plan = dir.command("needs_build = true\nshell = { interpreter = ['sh', '-c'], script = 'echo {x} \"$1\"', args = ['{sources}', '-o{artifact}'] }");
    let target = dir.target("cpp", "");
    let bindings = dir.bindings(&target, 3);
    let mut expected: Vec<OsString> = bindings
        .sources
        .iter()
        .map(|source| source.as_os_str().to_owned())
        .collect();
    let mut output = OsString::from("-o");
    output.push(bindings.artifact.as_ref().unwrap());
    expected.push(output);
    let ctx = Context::command(
        &plan,
        dir.dirs(),
        CommandBindings {
            target: Some(bindings),
            ..CommandBindings::default()
        },
    )
    .unwrap();
    let result = prepared(&plan.stage, &ctx);
    assert_eq!(args(&result), expected);
    let PreparedCommand::Shell {
        script,
        interpreter,
        ..
    } = &result.command
    else {
        panic!("shell expected")
    };
    assert_eq!(script.value, "echo {x} \"$1\"");
    assert!(interpreter.program.lookup);
    assert_eq!(
        interpreter.program.tokens,
        [crate::config::Token::Literal("sh".into())]
    );
}

#[test]
fn program_argument_cwd_and_extra_arg_failures_are_collected_together() {
    let dir = Directory::new();
    let plan = dir.command(
        "argv = ['{problem_id}/tool', '{problem_dir}']\ncwd = { path = '{problem_id}/work' }",
    );
    let mut dirs = dir.dirs();
    dirs.problem.as_mut().unwrap().dir = PathBuf::from("relative");
    let ctx = command_context(&plan, dirs);
    let errors = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &strings(&["nul\0extra"]),
    )
    .unwrap_err();
    assert_eq!(
        kinds(&errors),
        [
            PrepareErrorKind::NotAbsolute,
            PrepareErrorKind::NotAbsolute,
            PrepareErrorKind::Nul,
            PrepareErrorKind::NotAbsolute
        ]
    );
    assert_eq!(errors.errors[2].field, "extra_args[0]");
    assert_eq!(
        errors.errors[2].provenance,
        Provenance::Cli { flag: "--".into() }
    );
}

#[test]
fn remove_then_set_and_repeated_baseline_names_have_documented_order() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['prog']\nenv_remove = ['A']\nenv = { A = 'new', C = '3' }");
    let ctx = command_context(&plan, dir.dirs());
    let baseline = Baseline::from_entries(pairs(&[("A", "1"), ("B", "old"), ("B", "last")]));
    let result = prepare(&plan.stage, &ctx, &baseline, Platform::host(), &[]).unwrap();
    assert_eq!(
        result.environment,
        pairs(&[("B", "last"), ("A", "new"), ("C", "3")])
    );
}
