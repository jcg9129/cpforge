use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;

fn layer(text: &str, kind: LayerKind, platform: Platform) -> RawLayer {
    let file = match kind {
        LayerKind::BuiltIn => "builtin.toml",
        LayerKind::DefaultUser | LayerKind::OverrideUser => "user.toml",
        LayerKind::Workspace => "workspace.toml",
        LayerKind::Problem => "problem.toml",
    };
    parse(
        text.as_bytes(),
        &LayerSource {
            kind,
            file: file.into(),
        },
        &Environment::default(),
        platform,
    )
}

fn merged(texts: &[(&str, LayerKind)], platform: Platform) -> MergedConfig {
    compose(
        &texts
            .iter()
            .map(|(text, kind)| layer(text, *kind, platform))
            .collect::<Vec<_>>(),
        platform,
    )
}

fn workspace(text: &str) -> MergedConfig {
    merged(&[(text, LayerKind::Workspace)], Platform::Linux)
}

fn selection(language: &str) -> Selection {
    Selection {
        language: Some(language.into()),
        ..Selection::default()
    }
}

fn value(node: &EffectiveNode) -> Value {
    match &node.value {
        EffectiveValue::Table(t) => {
            Value::Object(t.iter().map(|(k, v)| (k.clone(), value(v))).collect())
        }
        EffectiveValue::Array(a) => Value::Array(a.iter().map(value).collect()),
        EffectiveValue::Scalar(v) => match v {
            RawValue::String(s) | RawValue::Datetime(s) => json!(s),
            RawValue::Integer(i) => json!(i),
            RawValue::Float(f) => json!(f),
            RawValue::Boolean(b) => json!(b),
            _ => panic!("unexpected scalar"),
        },
    }
}

fn ms(stage: &ComposedStage) -> u64 {
    match stage.node.get("deadline_s").unwrap().typed {
        Some(TypedValue::Milliseconds(ms)) => ms,
        _ => panic!("deadline not typed"),
    }
}

fn clean(report: &CheckReport) {
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    assert!(report.executable().is_some());
}

fn diagnostics(report: &CheckReport) -> Vec<(&str, &str)> {
    report
        .diagnostics()
        .iter()
        .map(|d| (d.origin.key_path.as_str(), d.message.as_str()))
        .collect()
}

#[test]
fn file_role_conflicts_and_membership_errors_report_exact_origins_together() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for support in ["solution.cpp", "SOLUTION.cpp"] {
            let document = format!(
                "[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\nsources = ['solution.cpp']\nsupport_files = ['{support}']\nsolution_files = ['outside.cpp', 'missing.h']\n"
            );
            let raw = layer(&document, LayerKind::Problem, platform);
            assert!(raw.diagnostics.is_empty());
            let config = compose(&[raw], platform);
            let report = check(&config, &Selection::default());
            let overlaps = support == "solution.cpp" || platform != Platform::Linux;
            assert_eq!(report.diagnostics().len(), if overlaps { 3 } else { 2 });
            let problem = config.problem().unwrap();
            if overlaps {
                let diagnostic = &report.diagnostics()[0];
                assert_eq!(
                    diagnostic.message,
                    "path appears in both sources and support_files"
                );
                assert_eq!(
                    diagnostic.origin,
                    problem.support_files().unwrap().array().unwrap()[0].origin
                );
                assert_eq!(
                    diagnostic.second_origin,
                    Some(
                        problem.sources().unwrap().array().unwrap()[0]
                            .origin
                            .clone()
                    )
                );
            }
            for (diagnostic, member) in report.diagnostics()[usize::from(overlaps)..]
                .iter()
                .zip(problem.solution_files().unwrap().array().unwrap())
            {
                assert_eq!(
                    diagnostic.message,
                    "solution_files member must be one of the captured files (sources or support_files)"
                );
                assert_eq!(diagnostic.origin, member.origin);
                assert_eq!(diagnostic.second_origin, None);
            }
            assert!(resolve(&config, &Selection::default()).is_err());
        }
    }
}

#[test]
fn solution_membership_uses_the_source_case_rule_and_allows_empty_lists() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for roles in [
            "sources = ['solution.cpp']\nsolution_files = ['SOLUTION.cpp']",
            "sources = []\nsupport_files = ['solution.cpp']\nsolution_files = ['SOLUTION.cpp']",
            "sources = ['solution.cpp']\nsupport_files = ['header.h']\nsolution_files = []",
        ] {
            let document =
                format!("[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\n{roles}");
            let config = merged(&[(&document, LayerKind::Problem)], platform);
            let report = check(&config, &Selection::default());
            assert_eq!(
                report.diagnostics().len(),
                usize::from(platform == Platform::Linux && !roles.ends_with("[]")),
                "{platform:?} {roles}"
            );
            if platform != Platform::Linux || roles.ends_with("[]") {
                clean(&report);
            }
        }
    }
}

#[test]
fn invalid_role_inputs_skip_dependent_checks_but_keep_independent_errors() {
    for (roles, expected_key, expected_message) in [
        (
            "sources = ['../bad']\nsupport_files = ['../bad']\nsolution_files = ['outside.cpp']",
            "config.sources[0]",
            "expected a non-empty problem-relative source with / components, without . or ..",
        ),
        (
            "support_files = ['../x.h']\nsolution_files = ['outside.cpp']",
            "config.support_files[0]",
            "expected a non-empty problem-relative source with / components, without . or ..",
        ),
        (
            "support_files = ['x.h','x.h']\nsolution_files = ['outside.cpp']",
            "config.support_files[1]",
            "duplicate source",
        ),
        (
            "solution_files = ['outside.cpp','outside.cpp']",
            "config.solution_files[1]",
            "duplicate source",
        ),
        (
            "solution_files = 17",
            "config.solution_files",
            "expected an array of strings",
        ),
    ] {
        let document = format!(
            "[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\n{roles}\n[config.run]\ncwd = {{}}\n"
        );
        let raw = layer(&document, LayerKind::Problem, Platform::Linux);
        let raw_diagnostics = raw.diagnostics.clone();
        assert_eq!(raw_diagnostics[0].origin.key_path, expected_key);
        assert_eq!(raw_diagnostics[0].message, expected_message);
        let config = compose(&[raw], Platform::Linux);
        let report = check(&config, &Selection::default());
        assert_eq!(
            report.diagnostics().len(),
            raw_diagnostics.len() + 1,
            "{roles}: {:?}",
            report.diagnostics()
        );
        assert_eq!(
            &report.diagnostics()[..raw_diagnostics.len()],
            raw_diagnostics
        );
        assert_eq!(
            report.diagnostics().last().unwrap().message,
            "cwd requires path or selector"
        );
    }
    // A malformed solution list does not hide an independent capture conflict.
    let config = merged(
        &[(
            "[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\nsources = ['same.cpp']\nsupport_files = ['same.cpp']\nsolution_files = 17",
            LayerKind::Problem,
        )],
        Platform::Linux,
    );
    assert_eq!(
        diagnostics(&check(&config, &Selection::default())),
        [
            ("config.solution_files", "expected an array of strings"),
            (
                "config.support_files[0]",
                "path appears in both sources and support_files"
            ),
        ]
    );
}

#[test]
fn java_required_derivation_validates_problem_entry_in_phase_two_only() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for entry in [
            "/tmp/Grader",
            "a..b",
            ".Grader",
            "judge/./Grader",
            "judge//Grader",
        ] {
            let document = format!(
                "[problem]\nid = 'local:p'\n[config]\nlanguage = 'java'\nentry = '{entry}'"
            );
            let raw = layer(&document, LayerKind::Problem, platform);
            assert!(raw.diagnostics.is_empty(), "{platform:?} {entry}");
            let config = compose(&[raw], platform);
            let report = check(&config, &Selection::default());
            assert_eq!(
                diagnostics(&report),
                [("config.entry", "derived artifact must be output-relative")],
                "{platform:?} {entry}"
            );
            assert_eq!(
                report.diagnostics()[0].origin,
                config.problem().unwrap().entry().unwrap().origin
            );
            // An explicit artifact requirement does not depend on the entry.
            let config = merged(
                &[
                    (
                        "[languages.java]\nartifacts = { dir = true, required = ['kept.class'] }",
                        LayerKind::Workspace,
                    ),
                    (&document, LayerKind::Problem),
                ],
                platform,
            );
            clean(&check(&config, &Selection::default()));
        }
        for entry in ["''", "17", "\"a\\u0000b\""] {
            let document =
                format!("[problem]\nid = 'local:p'\n[config]\nlanguage = 'java'\nentry = {entry}");
            let raw = layer(&document, LayerKind::Problem, platform);
            assert_eq!(raw.diagnostics.len(), 1);
            let expected = raw.diagnostics.clone();
            let config = compose(&[raw], platform);
            assert_eq!(
                check(&config, &Selection::default()).diagnostics(),
                expected
            );
        }
    }
    for entry in [r"C:\tmp\Grader", r"\Grader", r"judge\\Grader"] {
        let document =
            format!("[problem]\nid = 'local:p'\n[config]\nlanguage = 'java'\nentry = '{entry}'");
        let config = merged(&[(&document, LayerKind::Problem)], Platform::Windows);
        assert_eq!(
            diagnostics(&check(&config, &Selection::default())),
            [("config.entry", "derived artifact must be output-relative")]
        );
    }
}

#[test]
fn unknown_source_inputs_do_not_create_solution_membership_errors() {
    let unknown_user = layer("[", LayerKind::DefaultUser, Platform::Linux);
    let problem = layer(
        "[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\nsolution_files = ['could-be-in-user-sources.cpp']",
        LayerKind::Problem,
        Platform::Linux,
    );
    let expected = unknown_user.diagnostics.clone();
    let config = compose(&[unknown_user, problem], Platform::Linux);
    assert_eq!(
        check(&config, &Selection::default()).diagnostics(),
        expected
    );
}

#[test]
fn precedence_rows_one_to_six_and_specificity_over_layer() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for (use_profile, use_problem, cli, expected_ms, expected_source) in [
            (false, false, None, 10_000, "<built-in>"),
            (false, false, Some(8000), 8000, "<cli:--deadline>"),
            (true, false, None, 3000, "user.toml"),
            (true, true, None, 7000, "problem.toml"),
            (true, true, Some(8000), 8000, "<cli:--deadline>"),
        ] {
            let mut texts = vec![];
            if use_profile {
                texts.extend([
                    // Canonical keys follow the platform environment fold (§4.3).
                    ("[languages.cpp.profiles.fast.run]\ndeadline_s = 3\nenv = { x = 'profile' }", LayerKind::DefaultUser),
                    ("[run]\ndeadline_s = 1\nenv = { x = 'global', g = 'global' }\n[languages.cpp.run]\ndeadline_s = 2\nenv = { x = 'language', l = 'language' }", LayerKind::Workspace),
                ]);
            }
            if use_problem {
                texts.push(("[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\n[config.run]\ndeadline_s = 7\nenv = { x = 'problem' }", LayerKind::Problem));
            }
            let merged = merged(&texts, platform);
            let selected = Selection {
                profile: use_profile.then(|| "fast".into()),
                deadline_ms: cli,
                operation: Operation::Test,
                ..selection("cpp")
            };
            let report = check(&merged, &selected);
            clean(&report);
            let target = resolve(&merged, &selected).unwrap();
            let run = &target.target().run;
            assert_eq!(ms(run), expected_ms);
            assert_eq!(
                run.node.get("deadline_s").unwrap().origin.file,
                std::path::PathBuf::from(expected_source)
            );
            assert_eq!(
                run.explicitly_set("deadline_s"),
                use_profile || cli.is_some()
            );
            if use_profile {
                assert_eq!(
                    value(run.node.get("env").unwrap()),
                    if platform == Platform::Windows {
                        json!({"G":"global", "L":"language", "X":if use_problem { "problem" } else { "profile" }})
                    } else {
                        json!({"g":"global", "l":"language", "x":if use_problem { "problem" } else { "profile" }})
                    }
                );
                let events = &run.history[&vec!["env".into(), env_name_fold(platform, "x")]];
                assert_eq!(
                    events.iter().map(|e| e.kind).collect::<Vec<_>>(),
                    if use_problem {
                        vec![
                            EventKind::Set,
                            EventKind::Replace,
                            EventKind::Replace,
                            EventKind::Replace,
                        ]
                    } else {
                        vec![EventKind::Set, EventKind::Replace, EventKind::Replace]
                    }
                );
                assert_eq!(events[2].origin.file, std::path::PathBuf::from("user.toml"));
                assert_eq!(
                    events[2].removed[0].value,
                    EffectiveValue::Scalar(RawValue::String("language".into()))
                );
                assert_eq!(
                    run.node.get("deadline_s").unwrap().origin.line,
                    cli.is_none().then_some(if use_problem { 6 } else { 2 })
                );
            }
        }
    }
    for (text, expected) in [
        ("[run]\ndeadline_s = 1", 1000),
        (
            "[run]\ndeadline_s = 1\n[languages.cpp.run]\ndeadline_s = 2",
            2000,
        ),
    ] {
        let result = resolve(&workspace(text), &selection("cpp")).unwrap();
        assert_eq!(ms(&result.target().run), expected);
    }
}

#[test]
fn build_resets_modifiers_and_completeness() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let user = "[languages.python.build]\nargv = ['py-compile', '{sources}']";
        let enabled = merged(&[(user, LayerKind::DefaultUser)], platform);
        let result = resolve(&enabled, &selection("python")).unwrap();
        let build = result.target().build.as_ref().unwrap();
        assert_eq!(
            value(build.node.get("argv").unwrap()),
            json!(["py-compile", "{sources}"])
        );
        assert_eq!(ms(build), 60_000);
        assert!(build.node.reset_occurred);
        let disabled = merged(
            &[
                (user, LayerKind::DefaultUser),
                (
                    "[build]\ndeadline_s = 5\n[languages.python]\nbuild = false",
                    LayerKind::Workspace,
                ),
            ],
            platform,
        );
        let result = resolve(&disabled, &selection("python")).unwrap();
        assert!(result.target().settings.build_free);
        assert_eq!(
            result.target().build_history[&Vec::<String>::new()]
                .last()
                .unwrap()
                .kind,
            EventKind::Reset
        );
        let ignored = &result.target().ignored[&vec!["build".into(), "deadline_s".into()]];
        assert_eq!(ignored.len(), 1);
        assert_eq!(ignored[0].kind, EventKind::Ignored);
        assert_eq!(ignored[0].origin.key_path, "build.deadline_s");
        assert_eq!(
            ignored[0].origin.file,
            std::path::PathBuf::from("workspace.toml")
        );
        assert_eq!(
            ignored[0].value.as_ref().unwrap().typed,
            Some(TypedValue::Milliseconds(5000))
        );
    }
    let result = workspace(
        "[build]\nargv = ['cc']\n[languages.cpp]\nbuild = false\n[languages.cpp.profiles.p.build]\nenv = { A = '1' }",
    );
    let selected = Selection {
        profile: Some("p".into()),
        ..selection("cpp")
    };
    let report = check(&result, &selected);
    clean(&report);
    let target = report.universe().selected.as_ref().unwrap();
    let build = target.build.as_ref().unwrap();
    assert_eq!(value(build.node.get("argv").unwrap()), json!(["cc"]));
    assert_eq!(value(build.node.get("env").unwrap()), json!({"A":"1"}));

    let result = merged(
        &[
            (
                "[languages.cpp.build]\nenv = { A = '1' }",
                LayerKind::DefaultUser,
            ),
            (
                "[build]\ndeadline_s = 9\n[languages.cpp]\nbuild = false\n[languages.cpp.profiles.p.build]\nargv = ['c++', '{sources}', '-o', '{artifact}']",
                LayerKind::Workspace,
            ),
        ],
        Platform::Linux,
    );
    let target = resolve(&result, &selected).unwrap();
    let build = target.target().build.as_ref().unwrap();
    assert_eq!(ms(build), 9000);
    assert!(build.node.get("env").is_none());
    assert_eq!(
        build.history[&vec!["env".into(), "A".into()]]
            .last()
            .unwrap()
            .kind,
        EventKind::Discarded
    );
    let without_profile = resolve(&result, &selection("cpp")).unwrap();
    assert!(without_profile.target().build.is_none());
    let ignored = &without_profile.target().ignored[&vec!["build".into(), "deadline_s".into()]];
    assert_eq!(ignored[0].kind, EventKind::Ignored);
    assert_eq!(
        ignored[0].value.as_ref().unwrap().typed,
        Some(TypedValue::Milliseconds(9000))
    );

    let result = workspace(
        "[languages.cpp]\nbuild = false\n[languages.cpp.profiles.p.build]\nenv = { A = '1' }",
    );
    let report = check(&result, &selected);
    assert_eq!(
        diagnostics(&report),
        vec![(
            "languages.cpp.profiles.p.build",
            "stage requires argv or shell"
        )]
    );
    assert!(report.executable().is_none());
    assert!(resolve(&result, &selected).is_err());
}

#[test]
fn shell_replacement_is_atomic_and_partial_stage_layers_are_valid() {
    let result = merged(
        &[
            (
                "[commands.c]\nshell = { interpreter = ['sh', '-c'], script = 'true' }",
                LayerKind::DefaultUser,
            ),
            (
                "[commands.c]\nshell = { args = ['x'] }",
                LayerKind::Workspace,
            ),
        ],
        Platform::Linux,
    );
    let report = check(&result, &Selection::default());
    assert_eq!(
        diagnostics(&report),
        vec![
            ("commands.c.shell.interpreter", "shell requires interpreter"),
            ("commands.c.shell.script", "shell requires script"),
        ]
    );
    for d in report.diagnostics() {
        assert_eq!(d.origin.file, std::path::PathBuf::from("workspace.toml"));
        assert_eq!(d.origin.line, Some(2));
    }
    for texts in [
        vec![
            (
                "[languages.demo.run]\nenv = { X = '1' }",
                LayerKind::DefaultUser,
            ),
            (
                "[languages.demo.run]\nargv = ['demo']",
                LayerKind::Workspace,
            ),
        ],
        vec![
            ("[run]\nargv = ['demo']", LayerKind::Workspace),
            (
                "[languages.demo.run]\nenv = { X = '1' }",
                LayerKind::DefaultUser,
            ),
        ],
    ] {
        clean(&check(
            &merged(&texts, Platform::Linux),
            &Selection::default(),
        ));
    }
}

fn limit(milliseconds: u64) -> PublishedLimit {
    PublishedLimit {
        milliseconds,
        provenance: Provenance::File {
            path: "problem.toml".into(),
            line: Some(4),
            col: Some(1),
        },
        origin: Origin {
            line: Some(4),
            col: Some(1),
            ..super::stages::origin("limits.time", "problem.toml")
        },
    }
}

#[test]
fn default_deadlines_caps_presence_and_hook_recomposition() {
    let result = workspace(
        "default_language = 'cpp'\n[hooks]\non_switch = ['own', 'defaulted']\n[commands.own]\nargv = ['echo', 'hi']\ndeadline_s = 5\n[commands.defaulted]\nargv = ['echo', 'hi']",
    );
    for (published, expected_ms) in [(None, 10_000), (Some(limit(2500)), 6000)] {
        let selected = Selection {
            problem: true,
            published_time_limit: published.clone(),
            ..Selection::default()
        };
        let report = check(&result, &selected);
        clean(&report);
        let run = &report.universe().selected.as_ref().unwrap().run;
        assert_eq!(ms(run), expected_ms);
        assert!(!run.explicitly_set("deadline_s"));
        assert_eq!(
            run.node.get("deadline_s").unwrap().provenance,
            published.map_or(Provenance::BuiltIn, |l| Provenance::Derived {
                inputs: vec![l.provenance]
            })
        );
        let build = report
            .universe()
            .selected
            .as_ref()
            .unwrap()
            .build
            .as_ref()
            .unwrap();
        assert_eq!(ms(build), 60_000);
        for stage in [
            run,
            build,
            &report.universe().commands["defaulted"],
            &report.universe().hooks["defaulted"],
        ] {
            for (key, expected_bytes) in [
                (
                    "stdout_cap",
                    if stage.kind == StageKind::Run {
                        64 << 20
                    } else {
                        1 << 20
                    },
                ),
                ("stderr_cap", 1 << 20),
            ] {
                assert_eq!(
                    stage.node.get(key).unwrap().typed,
                    Some(TypedValue::Bytes(expected_bytes))
                );
                assert_eq!(stage.node.get(key).unwrap().provenance, Provenance::BuiltIn);
                assert!(!stage.explicitly_set(key));
            }
            assert_eq!(stage.stderr_policy, CapPolicy::Discard);
            assert_eq!(
                stage.stdout_policy,
                if stage.kind == StageKind::Run {
                    CapPolicy::Kill
                } else {
                    CapPolicy::Discard
                }
            );
        }
        for (name, command_ms, hook_ms, explicit) in [
            ("own", 5000, 5000, true),
            ("defaulted", 60_000, 30_000, false),
        ] {
            assert_eq!(ms(&report.universe().commands[name]), command_ms);
            assert_eq!(ms(&report.universe().hooks[name]), hook_ms);
            assert_eq!(
                report.universe().hooks[name].explicitly_set("deadline_s"),
                explicit
            );
            assert!(!report.universe().hooks[name].explicitly_set("repeat"));
            let executable = report.executable().unwrap();
            assert_eq!(ms(executable.command(name).unwrap().stage()), command_ms);
            assert_eq!(ms(executable.hook(name).unwrap().stage()), hook_ms);
        }
        let resolved = resolve(&result, &selected).unwrap();
        assert_eq!(resolved.executable().universe(), report.universe());
        assert_eq!(ms(resolved.executable().hook("own").unwrap().stage()), 5000);
        assert!(resolved.executable().command("missing").is_none());
        assert!(resolved.executable().hook("missing").is_none());
    }
    for published in [u64::MAX, u64::MAX / 2, 43_200_000] {
        let selected = Selection {
            problem: true,
            published_time_limit: Some(limit(published)),
            ..Selection::default()
        };
        let report = check(&result, &selected);
        assert_eq!(
            diagnostics(&report),
            vec![(
                "limits.time",
                "derived run deadline overflows or exceeds 24 hours"
            )]
        );
        assert!(resolve(&result, &selected).is_err());
    }
    let selected = Selection {
        problem: true,
        published_time_limit: Some(limit(u64::MAX)),
        deadline_ms: Some(5000),
        operation: Operation::Run,
        ..Selection::default()
    };
    clean(&check(&result, &selected));
}

#[test]
fn cli_deadline_only_applies_to_test_and_run_run_stages() {
    let result = workspace(
        "default_language = 'cpp'\n[run]\ndeadline_s = 2\n[build]\ndeadline_s = 3\n[hooks]\non_switch = ['c']\n[commands.c]\nargv = ['echo']",
    );
    for (operation, expected) in [
        (Operation::Check, 2000),
        (Operation::Other, 2000),
        (Operation::Test, 7000),
        (Operation::Run, 7000),
    ] {
        let selected = Selection {
            operation,
            deadline_ms: Some(7000),
            ..Selection::default()
        };
        let report = check(&result, &selected);
        clean(&report);
        let target = report.universe().selected.as_ref().unwrap();
        assert_eq!(ms(&target.run), expected);
        assert_eq!(ms(target.build.as_ref().unwrap()), 3000);
        assert_eq!(ms(&report.universe().commands["c"]), 60_000);
        assert_eq!(ms(&report.universe().hooks["c"]), 30_000);
        assert!(report.universe().commands["c"].node.get("env").is_none());
        // Raw `cpf run` caps (execution contract §5.5) apply to its run stage only.
        let stderr = if operation == Operation::Run {
            64 << 20
        } else {
            1 << 20
        };
        for (stage, expected) in [
            (&target.run, stderr),
            (target.build.as_ref().unwrap(), 1 << 20),
            (&report.universe().commands["c"], 1 << 20),
        ] {
            assert_eq!(
                stage.node.get("stderr_cap").unwrap().typed,
                Some(TypedValue::Bytes(expected))
            );
            assert!(!stage.explicitly_set("stderr_cap"));
        }
        assert_eq!(
            target.run.node.get("stdout_cap").unwrap().typed,
            Some(TypedValue::Bytes(64 << 20))
        );
        assert_eq!(target.run.stdout_policy, CapPolicy::Kill);
        assert_eq!(target.run.stderr_policy, CapPolicy::Discard);
    }
    for deadline_ms in [0, 86_400_001, u64::MAX] {
        let report = check(
            &result,
            &Selection {
                operation: Operation::Test,
                deadline_ms: Some(deadline_ms),
                ..Selection::default()
            },
        );
        assert_eq!(
            diagnostics(&report),
            vec![(
                "deadline_s",
                "CLI deadline must be in 1..=86400000 milliseconds"
            )]
        );
    }
}

#[test]
fn references_and_missing_target_have_exact_diagnostics() {
    for (text, selected, expected_key, expected_message) in [
        (
            "schema = 1",
            Selection {
                profile: Some("missing".into()),
                ..selection("cpp")
            },
            "profile",
            "unknown profile \"missing\" for language \"cpp\"",
        ),
        (
            "default_language = 'missing'",
            Selection::default(),
            "default_language",
            "unknown language \"missing\"",
        ),
        (
            "[hooks]\non_switch = ['notify']",
            Selection::default(),
            "hooks.on_switch[0]",
            "hook names unknown command \"notify\"",
        ),
    ] {
        let result = workspace(text);
        let report = check(&result, &selected);
        assert_eq!(diagnostics(&report), vec![(expected_key, expected_message)]);
        assert!(report.executable().is_none());
        assert!(!report.diagnostics()[0].origin.file.as_os_str().is_empty());
        if expected_key == "profile" {
            assert!(report.diagnostics()[0].second_origin.is_some());
        }
    }
    let result = merged(
        &[(
            "[problem]\nid = 'local:p'\n[config]\nlanguage = 'missing'",
            LayerKind::Problem,
        )],
        Platform::Linux,
    );
    assert_eq!(
        diagnostics(&check(&result, &Selection::default())),
        vec![("config.language", "unknown language \"missing\"")]
    );
    for (texts, selected) in [
        (
            vec![("schema = 1", LayerKind::Workspace)],
            Selection::default(),
        ),
        (
            vec![(
                "[problem]\nid = 'local:p'\n[config]\nsources = ['x.cpp']",
                LayerKind::Problem,
            )],
            Selection::default(),
        ),
        (
            vec![("[problem]\nid = 'local:p'", LayerKind::Problem)],
            Selection {
                problem: true,
                ..Selection::default()
            },
        ),
    ] {
        let result = merged(&texts, Platform::Linux);
        let report = check(&result, &selected);
        clean(&report);
        assert!(report.universe().selected.is_none());
        let errors = resolve(&result, &selected).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].message,
            "target requires a language (config.language or default_language)"
        );
    }
}

#[test]
fn hooks_and_command_cross_field_checks_use_explicit_presence() {
    for (extra, key, message) in [
        (
            "repeat = 1",
            "repeat",
            "repeat is not allowed in hook context (even when false or 1)",
        ),
        (
            "for_each_test = false",
            "for_each_test",
            "for_each_test is not allowed in hook context (even when false or 1)",
        ),
        (
            "needs_build = false",
            "needs_build",
            "needs_build is not allowed in hook context (even when false or 1)",
        ),
    ] {
        let result = merged(
            &[
                ("[hooks]\nenabled = false", LayerKind::DefaultUser),
                (
                    &format!("[hooks]\non_switch = ['h']\n[commands.h]\nargv = ['echo']\n{extra}"),
                    LayerKind::Workspace,
                ),
            ],
            Platform::Linux,
        );
        let report = check(&result, &Selection::default());
        assert_eq!(
            diagnostics(&report),
            vec![(format!("commands.h.{key}").as_str(), message)]
        );
        assert_eq!(report.diagnostics()[0].origin.line, Some(5));
    }
    let result = workspace(
        "[hooks]\nafter_test = ['h']\n[commands.h]\nargv = ['echo', '{test_input}']\nfor_each_test = true",
    );
    let report = check(&result, &Selection::default());
    assert_eq!(
        diagnostics(&report),
        vec![
            (
                "commands.h.for_each_test",
                "for_each_test is not allowed in hook context (even when false or 1)"
            ),
            (
                "commands.h.argv[1]",
                "placeholder {test_input} is not allowed in hook context"
            ),
        ]
    );
    for (text, expected, conflict) in [
        (
            "argv = ['echo', '{artifact}']",
            vec![(
                "commands.c.argv[1]",
                "artifact placeholders require needs_build = true",
            )],
            None,
        ),
        (
            "argv = ['echo', '{artifact_dir}']\nneeds_build = false",
            vec![(
                "commands.c.argv[1]",
                "artifact placeholders require needs_build = true",
            )],
            Some("commands.c.needs_build"),
        ),
        (
            "argv = ['echo']\nstdin = 'test'",
            vec![(
                "commands.c.stdin",
                "stdin = test requires for_each_test = true",
            )],
            None,
        ),
        (
            "argv = ['echo']\nstdin = 'test'\nfor_each_test = false",
            vec![(
                "commands.c.stdin",
                "stdin = test requires for_each_test = true",
            )],
            Some("commands.c.for_each_test"),
        ),
        (
            "argv = ['echo', '{artifact}']\nneeds_build = true",
            vec![],
            None,
        ),
        (
            "argv = ['echo']\nstdin = 'test'\nfor_each_test = true",
            vec![],
            None,
        ),
        (
            "shell = { interpreter = ['sh','-c'], script = '{artifact}' }",
            vec![],
            None,
        ),
        ("argv = ['echo', '{{artifact}}']", vec![], None),
    ] {
        let result = workspace(&format!("[commands.c]\n{text}"));
        let report = check(&result, &Selection::default());
        assert_eq!(diagnostics(&report), expected);
        // A second origin names only a conflicting key the user wrote.
        for d in report.diagnostics() {
            assert_eq!(
                d.second_origin.as_ref().map(|o| o.key_path.as_str()),
                conflict,
                "{text}"
            );
        }
    }
}

#[test]
fn effective_target_settings_and_java_artifacts() {
    let result = merged(
        &[
            (
                "default_language = 'java'\ncompare = { tokens = true }\n[languages.java]\nentry = 'a.b.Main'\ntemplate = { path = 'Main.tmpl' }",
                LayerKind::Workspace,
            ),
            (
                "[problem]\nid = 'local:p'\n[config]\nsources = ['src/Main.java']\ncompare = { exact = true }",
                LayerKind::Problem,
            ),
        ],
        Platform::Linux,
    );
    let result = resolve(&result, &Selection::default()).unwrap();
    let settings = &result.target().settings;
    assert_eq!(
        value(settings.sources.as_ref().unwrap()),
        json!(["src/Main.java"])
    );
    assert_eq!(
        settings.compare.as_ref().unwrap().typed,
        Some(TypedValue::Compare(Comparator::Exact))
    );
    assert_eq!(
        value(settings.artifacts.as_ref().unwrap()),
        json!({"dir":true,"required":["a/b/Main.class"]})
    );
    let required = settings
        .artifacts
        .as_ref()
        .unwrap()
        .get("required")
        .unwrap();
    assert_eq!(
        required.provenance,
        Provenance::Derived {
            inputs: vec![settings.entry.as_ref().unwrap().provenance.clone()]
        }
    );
    assert_eq!(required.array().unwrap()[0].provenance, required.provenance);
    assert_eq!(
        settings
            .template
            .as_ref()
            .unwrap()
            .get("path")
            .unwrap()
            .typed,
        Some(TypedValue::Path("Main.tmpl".into()))
    );
    assert!(!settings.build_free);
    for text in [
        "[languages.demo]\nartifacts = { dir = true }\n[languages.demo.run]\nargv = ['demo']",
        "[languages.demo]\nartifacts = { dir = true, required = [] }\n[languages.demo.run]\nargv = ['demo']",
        "[languages.java]\nartifacts = { dir = true, required = [] }",
    ] {
        let report = check(&workspace(text), &Selection::default());
        assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
        assert_eq!(
            report.diagnostics()[0].message,
            "directory artifact requires non-empty required paths after defaults"
        );
    }
    assert_eq!(
        diagnostics(&check(
            &workspace("[languages.java]\nentry = 'a..Main'"),
            &Selection::default()
        )),
        vec![(
            "languages.java.entry",
            "derived artifact must be output-relative"
        )]
    );
    for (text, key, message) in [
        (
            "[languages.demo]\nartifacts = {}\n[languages.demo.run]\nargv = ['demo']",
            "languages.demo.artifacts",
            "artifacts requires file or dir = true",
        ),
        (
            "[commands.c]\nargv = ['echo']\ncwd = {}",
            "commands.c.cwd",
            "cwd requires path or selector",
        ),
    ] {
        assert_eq!(
            diagnostics(&check(&workspace(text), &Selection::default())),
            vec![(key, message)]
        );
    }
}

#[test]
fn command_context_requirements_cover_all_tokenized_fields() {
    for (text, problem, workspace_required) in [
        ("argv = ['echo', '{problem_id}']", true, true),
        ("argv = ['echo', '{workspace}']", false, true),
        ("argv = ['echo', 'hi']", false, false),
        ("argv = ['echo']\nneeds_build = true", true, true),
        ("argv = ['echo']\nfor_each_test = true", true, true),
        (
            "argv = ['echo']\ncwd = { selector = 'problem' }",
            true,
            true,
        ),
        (
            "argv = ['echo']\ncwd = { selector = 'workspace' }",
            false,
            true,
        ),
        (
            "argv = ['echo']\ncwd = { path = '{problem_dir}' }",
            true,
            true,
        ),
        (
            "shell = { interpreter = ['sh','-c'], script = 'echo', args = ['{problem_id}'] }",
            true,
            true,
        ),
        ("argv = ['echo', '{iteration}']\nrepeat = 1", false, false),
        (
            "argv = ['echo']\nenv = { X = '{problem_id}' }",
            false,
            false,
        ),
    ] {
        let report = check(
            &workspace(&format!("[commands.c]\n{text}")),
            &Selection::default(),
        );
        clean(&report);
        let stage = &report.universe().commands["c"];
        assert_eq!(
            stage.context,
            ContextRequirements {
                problem,
                workspace: workspace_required
            },
            "{text}"
        );
        assert_eq!(
            stage
                .deferred
                .iter()
                .map(|v| &v.origin.file)
                .collect::<Vec<_>>(),
            vec![&std::path::PathBuf::from("workspace.toml"); stage.deferred.len()]
        );
    }
}

#[test]
fn invalid_markers_suppress_only_dependent_checks_including_overrides() {
    for (first, second, expected) in [
        (
            "[commands.c]\nargv = [1]",
            "[commands.c]\ncwd = {}",
            vec![
                ("commands.c.argv[0]", "expected a string"),
                ("commands.c.cwd", "cwd requires path or selector"),
            ],
        ),
        (
            "[commands.c]\ndeadline_s = false",
            "[commands.c]",
            vec![
                (
                    "commands.c.deadline_s",
                    "expected a number or duration string",
                ),
                ("commands.c", "stage requires argv or shell"),
            ],
        ),
        (
            "[commands.c]\nneeds_build = 'bad'",
            "[commands.c]\nargv = ['echo', '{artifact}']\nstdin = 'test'",
            vec![
                ("commands.c.needs_build", "expected a boolean"),
                (
                    "commands.c.stdin",
                    "stdin = test requires for_each_test = true",
                ),
            ],
        ),
        (
            "[commands.c]\nargv = [1]",
            "[commands.c]\nshell = { args = ['x'] }",
            vec![("commands.c.argv[0]", "expected a string")],
        ),
        (
            "[commands.c]\nargv = [1]",
            "[commands.c]\nargv = ['echo']",
            vec![("commands.c.argv[0]", "expected a string")],
        ),
        (
            "[commands.c.windows]\nargv = [1]",
            "[commands.c]",
            vec![
                ("commands.c.windows.argv[0]", "expected a string"),
                ("commands.c", "stage requires argv or shell"),
            ],
        ),
        (
            "[commands.c]\nshell = { args = [1] }",
            "[commands.c]",
            vec![
                ("commands.c.shell.args[0]", "expected a string"),
                ("commands.c.shell.interpreter", "shell requires interpreter"),
                ("commands.c.shell.script", "shell requires script"),
            ],
        ),
    ] {
        let result = merged(
            &[
                (first, LayerKind::DefaultUser),
                (second, LayerKind::Workspace),
            ],
            Platform::Linux,
        );
        let report = check(&result, &Selection::default());
        assert_eq!(diagnostics(&report), expected, "{first}; {second}");
        assert!(report.executable().is_none());
    }
}

#[test]
fn unreadable_input_suppresses_references_but_independent_errors_continue() {
    // Model the raw loader's read failure without OS-specific permission setup.
    let origin = super::stages::origin("", "user.toml");
    let unreadable = RawLayer {
        kind: LayerKind::DefaultUser,
        platform: Platform::Linux,
        file: "user.toml".into(),
        state: LayerState::Invalid(origin.clone()),
        diagnostics: vec![Diagnostic {
            severity: Severity::Error,
            message: "cannot read configuration: permission denied".into(),
            origin,
            second_origin: None,
        }],
    };
    let workspace = layer(
        "[hooks]\non_switch = ['notify']",
        LayerKind::Workspace,
        Platform::Linux,
    );
    let result = compose(&[unreadable.clone(), workspace.clone()], Platform::Linux);
    let report = check(&result, &Selection::default());
    assert_eq!(report.diagnostics(), unreadable.diagnostics);
    assert!(report.executable().is_none());
    let independent = layer(
        "[commands.c]\nshell = { args = ['x'] }\ncwd = {}\n[hooks]\nafter_test = ['c']",
        LayerKind::Workspace,
        Platform::Linux,
    );
    let result = compose(&[unreadable, workspace, independent], Platform::Linux);
    let report = check(&result, &Selection::default());
    assert_eq!(report.diagnostics().len(), 4, "{:?}", report.diagnostics());
    assert_eq!(
        &diagnostics(&report)[1..],
        &[
            ("commands.c.shell.interpreter", "shell requires interpreter"),
            ("commands.c.shell.script", "shell requires script"),
            ("commands.c.cwd", "cwd requires path or selector"),
        ]
    );
    // A problem file cannot define commands, languages or profiles.
    let result = compose(
        &[
            layer("schema = 2", LayerKind::Problem, Platform::Linux),
            layer(
                "[hooks]\non_switch = ['notify']",
                LayerKind::Workspace,
                Platform::Linux,
            ),
        ],
        Platform::Linux,
    );
    assert_eq!(check(&result, &Selection::default()).diagnostics().len(), 2);
}

#[test]
fn warnings_allow_capability_but_any_error_in_the_universe_blocks_resolution() {
    let mut result = workspace("default_language = 'cpp'");
    result.diagnostics.push(Diagnostic {
        severity: Severity::Warning,
        message: "warning".into(),
        origin: super::stages::origin("hooks", "workspace.toml"),
        second_origin: None,
    });
    let report = check(&result, &Selection::default());
    assert!(report.executable().is_some());
    assert_eq!(report.executable().unwrap().universe(), report.universe());
    assert_eq!(
        resolve(&result, &Selection::default())
            .unwrap()
            .diagnostics(),
        result.diagnostics
    );
    for text in [
        "default_language = 'cpp'\n[languages.unused]",
        "default_language = 'cpp'\n[languages.python.profiles.unused.run]\nshell = {}",
        "default_language = 'cpp'\n[commands.unused]",
    ] {
        let result = workspace(text);
        let report = check(&result, &Selection::default());
        assert!(report.executable().is_none());
        assert!(resolve(&result, &Selection::default()).is_err());
    }
}

#[test]
fn provenance_keeps_tombstones_and_unit_replacements_across_rows() {
    let result = merged(
        &[
            (
                "[run]\nenv = { X = '1' }\ncwd = { path = 'a' }\nlimits = { address_space = 4096 }",
                LayerKind::DefaultUser,
            ),
            (
                "[languages.cpp.run]\nenv = { X = false }\ncwd = { selector = 'problem' }\nlimits = {}",
                LayerKind::Workspace,
            ),
            (
                "[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\n[config.run]\nenv = { X = '2' }",
                LayerKind::Problem,
            ),
        ],
        Platform::Linux,
    );
    let report = check(&result, &Selection::default());
    clean(&report);
    let run = &report.universe().selected.as_ref().unwrap().run;
    assert_eq!(
        value(run.node.get("cwd").unwrap()),
        json!({"selector":"problem"})
    );
    assert_eq!(value(run.node.get("limits").unwrap()), json!({}));
    assert_eq!(value(run.node.get("env").unwrap()), json!({"X":"2"}));
    let events = &run.history[&vec!["env".into(), "X".into()]];
    assert_eq!(
        events.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![EventKind::Set, EventKind::Delete, EventKind::Replace]
    );
    assert_eq!(
        events[1].removed[0].value,
        EffectiveValue::Scalar(RawValue::String("1".into()))
    );
    assert_eq!(
        events[2].removed[0].value,
        EffectiveValue::Scalar(RawValue::Boolean(false))
    );
    assert_eq!(
        run.history[&vec!["cwd".into(), "path".into()]]
            .last()
            .unwrap()
            .kind,
        EventKind::Discarded
    );
    assert_eq!(
        run.node.get("limits").unwrap().value,
        EffectiveValue::Table(BTreeMap::new())
    );
}

#[test]
fn resets_preserve_both_modifier_rows_and_ignore_them_when_build_free() {
    let result = merged(
        &[
            (
                "[build]\ndeadline_s = 5\nenv = { GLOBAL = 'g' }\n[languages.python.profiles.p.build]\nargv = ['compile']",
                LayerKind::DefaultUser,
            ),
            (
                "[problem]\nid = 'local:p'\n[config]\nlanguage = 'python'\n[config.build]\nstdout_cap = 1234\nenv = { PROBLEM = 'p' }",
                LayerKind::Problem,
            ),
        ],
        Platform::Linux,
    );
    let selected = Selection {
        profile: Some("p".into()),
        ..Selection::default()
    };
    let report = check(&result, &selected);
    clean(&report);
    let build = report
        .universe()
        .selected
        .as_ref()
        .unwrap()
        .build
        .as_ref()
        .unwrap();
    assert_eq!(ms(build), 5000);
    assert_eq!(
        build.node.get("stdout_cap").unwrap().typed,
        Some(TypedValue::Bytes(1234))
    );
    assert_eq!(
        value(build.node.get("env").unwrap()),
        json!({"GLOBAL":"g", "PROBLEM":"p"})
    );
    // The Python reset belongs to row 3; it must not claim to discard row 2.
    for event in build
        .history
        .values()
        .flatten()
        .filter(|e| e.kind == EventKind::Reset)
    {
        for removed in &event.removed {
            if let EffectiveValue::Table(t) = &removed.value {
                assert!(!t.contains_key("deadline_s"));
            }
        }
    }
    let target = resolve(&result, &Selection::default()).unwrap();
    assert!(target.target().build.is_none());
    for key in [
        vec!["build".into(), "deadline_s".into()],
        vec!["config".into(), "build".into(), "stdout_cap".into()],
    ] {
        assert_eq!(target.target().ignored[&key][0].kind, EventKind::Ignored);
    }
}

#[test]
fn independent_checks_keep_both_conflict_origins_and_selection_dependencies() {
    let result = merged(
        &[
            ("[commands.c]\nneeds_build = false", LayerKind::DefaultUser),
            (
                "[commands.c]\nargv = ['echo', '{artifact}']",
                LayerKind::Workspace,
            ),
        ],
        Platform::Linux,
    );
    let report = check(&result, &Selection::default());
    assert_eq!(report.diagnostics().len(), 1);
    let error = &report.diagnostics()[0];
    assert_eq!(
        error.origin.file,
        std::path::PathBuf::from("workspace.toml")
    );
    assert_eq!(error.origin.key_path, "commands.c.argv[1]");
    let other = error.second_origin.as_ref().unwrap();
    assert_eq!(other.file, std::path::PathBuf::from("user.toml"));
    assert_eq!(other.key_path, "commands.c.needs_build");
    assert_eq!(other.line, Some(2));

    let result = merged(
        &[
            ("default_language = false", LayerKind::DefaultUser),
            (
                "[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\n[config.run]\nshell = {}",
                LayerKind::Problem,
            ),
        ],
        Platform::Linux,
    );
    let report = check(&result, &Selection::default());
    assert_eq!(report.diagnostics().len(), 3, "{:?}", report.diagnostics());
    assert_eq!(
        &diagnostics(&report)[1..],
        &[
            ("config.run.shell.interpreter", "shell requires interpreter"),
            ("config.run.shell.script", "shell requires script"),
        ]
    );

    let result =
        workspace("[languages.java]\nentry = false\nartifacts = { dir = true, required = [] }");
    let report = check(&result, &Selection::default());
    assert_eq!(report.diagnostics().len(), 2);
    assert_eq!(
        report.diagnostics()[1].message,
        "directory artifact requires non-empty required paths after defaults"
    );
}

#[test]
fn deadline_derivation_boundaries_and_problem_language_selection() {
    let result = merged(
        &[
            ("default_language = 'python'", LayerKind::Workspace),
            (
                "[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'",
                LayerKind::Problem,
            ),
        ],
        Platform::Linux,
    );
    for (published, expected) in [(0, 1000), (43_199_500, 86_400_000)] {
        let selected = Selection {
            published_time_limit: Some(limit(published)),
            ..Selection::default()
        };
        let target = resolve(&result, &selected).unwrap();
        assert_eq!(target.target().language, "cpp");
        assert_eq!(ms(&target.target().run), expected);
        assert!(!target.target().run.explicitly_set("deadline_s"));
    }
}

#[test]
fn surviving_profile_replacement_names_the_modifier_it_replaces() {
    let result = workspace(
        "[build]\nargv = ['cc']\n[languages.cpp]\nbuild = false\n[languages.cpp.profiles.p.build]\nargv = ['fresh']",
    );
    let selected = Selection {
        profile: Some("p".into()),
        ..selection("cpp")
    };
    let target = resolve(&result, &selected).unwrap();
    let build = target.target().build.as_ref().unwrap();
    let events = &build.history[&vec!["argv".into()]];
    let last = events.last().unwrap();
    assert_eq!(last.kind, EventKind::Replace);
    assert_eq!(last.origin.key_path, "languages.cpp.profiles.p.build.argv");
    assert_eq!(last.removed.len(), 1);
    assert_eq!(last.removed[0].origin.key_path, "build.argv");
    let EffectiveValue::Array(argv) = &last.removed[0].value else {
        panic!("removed argv array");
    };
    assert_eq!(argv.len(), 1);
    assert_eq!(argv[0].string(), Some("cc"));
}

#[test]
fn unreadable_problem_file_leaves_the_target_unresolved() {
    // The problem file could select another language (§3.2), so neither the
    // profile nor the derived deadline of the default language is checked.
    let origin = super::stages::origin("", "problem.toml");
    let unreadable = RawLayer {
        kind: LayerKind::Problem,
        platform: Platform::Linux,
        file: "problem.toml".into(),
        state: LayerState::Invalid(origin.clone()),
        diagnostics: vec![Diagnostic {
            severity: Severity::Error,
            message: "cannot read problem: permission denied".into(),
            origin,
            second_origin: None,
        }],
    };
    let result = compose(
        &[
            unreadable,
            layer(
                "default_language = 'cpp'\n[commands.c]\ncwd = {}",
                LayerKind::Workspace,
                Platform::Linux,
            ),
        ],
        Platform::Linux,
    );
    let selected = Selection {
        problem: true,
        profile: Some("missing".into()),
        published_time_limit: Some(limit(u64::MAX)),
        ..Selection::default()
    };
    for selected in [
        selected.clone(),
        Selection {
            problem: false,
            ..selected
        },
    ] {
        let report = check(&result, &selected);
        assert_eq!(
            diagnostics(&report),
            vec![
                ("", "cannot read problem: permission denied"),
                ("commands.c", "stage requires argv or shell"),
                ("commands.c.cwd", "cwd requires path or selector"),
            ]
        );
        assert!(report.universe().selected.is_none());
        assert!(resolve(&result, &selected).is_err());
    }
}

#[test]
fn command_placeholder_declarations_are_checked_in_phase_two() {
    for (placeholder, declaration, value, message) in [
        (
            "sources",
            "needs_build",
            "false",
            "source placeholders require needs_build = true",
        ),
        (
            "source",
            "needs_build",
            "false",
            "source placeholders require needs_build = true",
        ),
        (
            "test_input",
            "for_each_test",
            "false",
            "test placeholders require for_each_test = true",
        ),
        (
            "test_answer",
            "for_each_test",
            "false",
            "test placeholders require for_each_test = true",
        ),
        (
            "iteration",
            "repeat",
            "1",
            "iteration placeholder requires repeat",
        ),
    ] {
        for explicit in [false, true] {
            // An explicit repeat of one is a declaration; its default is not.
            if declaration == "repeat" && explicit {
                continue;
            }
            let extra = if explicit {
                format!("\n{declaration} = {value}")
            } else {
                String::new()
            };
            let result = workspace(&format!(
                "[commands.c]\nargv = ['echo', '{{{placeholder}}}']{extra}"
            ));
            let report = check(&result, &Selection::default());
            assert_eq!(diagnostics(&report), [("commands.c.argv[1]", message)]);
            assert_eq!(
                report.diagnostics()[0]
                    .second_origin
                    .as_ref()
                    .map(|o| o.key_path.as_str()),
                explicit
                    .then_some(format!("commands.c.{declaration}"))
                    .as_deref()
            );

            // Invalid declaration input suppresses the dependent error.
            let result = workspace(&format!(
                "[commands.c]\nargv = ['echo', '{{{placeholder}}}']\n{declaration} = 'invalid'"
            ));
            let report = check(&result, &Selection::default());
            assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
        }
    }
    let result = workspace(
        "[commands.c]\nargv = ['echo', '{sources}', '{source}', '{artifact}', '{artifact_dir}', '{test_input}', '{test_answer}', '{iteration}']\nneeds_build = true\nfor_each_test = true\nrepeat = 1",
    );
    clean(&check(&result, &Selection::default()));
}

#[test]
fn reserved_file_io_validates_in_phase_one_and_refuses_in_phase_two() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for fields in [
            "input = 'task.in'",
            "output = 'task.out'",
            "input = '输入.txt', output = 'two words.out'",
            "input = '.hidden', output = 'a..b'",
            "input = '{literal}.in'",
        ] {
            for settings in [
                "",
                "language = 'cpp'\n[config.run]\ncwd = { selector = 'problem' }\n",
            ] {
                let document =
                    format!("[problem]\nid = 'local:p'\n[config]\nio = {{ {fields} }}\n{settings}");
                let raw = layer(&document, LayerKind::Problem, platform);
                assert!(raw.diagnostics.is_empty(), "{:?}", raw.diagnostics);
                let config = compose(&[raw], platform);
                let report = check(&config, &Selection::default());
                assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
                let diagnostic = &report.diagnostics()[0];
                assert_eq!(
                    diagnostic.message,
                    "file-I/O problems are not supported in this version"
                );
                assert_eq!(
                    diagnostic.origin,
                    config.at(&["config", "io"]).unwrap().origin
                );
                assert_eq!(
                    diagnostic.origin.file,
                    std::path::PathBuf::from("problem.toml")
                );
                assert_eq!(diagnostic.origin.key_path, "config.io");
                assert_eq!(diagnostic.origin.line, Some(4));
                assert!(diagnostic.origin.col.is_some());
                assert!(report.executable().is_none());
                assert!(resolve(&config, &Selection::default()).is_err());
            }
        }
    }
}

#[test]
fn invalid_file_io_has_only_phase_one_errors_with_provenance() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for field in ["input", "output"] {
            for name in [
                "",
                ".",
                "..",
                "a/b",
                r"a\b",
                "/root",
                r"\root",
                "C:task.in",
                "a:b",
                "NUL",
                "con.txt",
                "COM¹.txt",
                "name.",
                "name ",
                ".cpf-input",
            ] {
                let document =
                    format!("[problem]\nid = 'local:p'\n[config]\nio = {{ {field} = '{name}' }}\n");
                let raw = layer(&document, LayerKind::Problem, platform);
                assert_eq!(
                    raw.diagnostics.len(),
                    1,
                    "{document}: {:?}",
                    raw.diagnostics
                );
                let diagnostic = &raw.diagnostics[0];
                assert!(diagnostic.message.contains("portable file-I/O name"));
                assert_eq!(diagnostic.origin.key_path, format!("config.io.{field}"));
                assert_eq!(
                    diagnostic.origin.file,
                    std::path::PathBuf::from("problem.toml")
                );
                assert_eq!(diagnostic.origin.line, Some(4));
                assert!(diagnostic.origin.col.is_some());
                let config = compose(std::slice::from_ref(&raw), platform);
                assert_eq!(
                    check(&config, &Selection::default()).diagnostics(),
                    raw.diagnostics
                );
            }
        }
        for (value, key, message) in [
            (
                "{}",
                "config.io",
                "io requires at least one of input or output",
            ),
            ("false", "config.io", "expected a table"),
            ("[]", "config.io", "expected a table"),
            ("'task.in'", "config.io", "expected a table"),
            ("{input = 1}", "config.io.input", "expected a string"),
            ("{output = []}", "config.io.output", "expected a string"),
            (
                "{input = 'ok.in', extra = 'bad'}",
                "config.io.extra",
                "unknown configuration key",
            ),
        ] {
            let raw = layer(
                &format!("[problem]\nid = 'local:p'\n[config]\nio = {value}\n"),
                LayerKind::Problem,
                platform,
            );
            assert_eq!(raw.diagnostics.len(), 1, "{:?}", raw.diagnostics);
            assert_eq!(raw.diagnostics[0].origin.key_path, key);
            assert_eq!(raw.diagnostics[0].message, message);
            let config = compose(std::slice::from_ref(&raw), platform);
            assert_eq!(
                check(&config, &Selection::default()).diagnostics(),
                raw.diagnostics
            );
        }
    }
}

#[test]
fn reserved_file_io_is_unknown_outside_the_problem_layer() {
    for kind in [
        LayerKind::DefaultUser,
        LayerKind::OverrideUser,
        LayerKind::Workspace,
    ] {
        let raw = layer("io = { input = 'task.in' }", kind, Platform::host());
        assert_eq!(raw.diagnostics.len(), 1);
        assert_eq!(raw.diagnostics[0].origin.key_path, "io");
        assert_eq!(raw.diagnostics[0].message, "unknown configuration key");
        let config = compose(std::slice::from_ref(&raw), Platform::host());
        assert!(config.at(&["io"]).is_none());
        assert_eq!(
            check(&config, &Selection::default()).diagnostics(),
            raw.diagnostics
        );
    }
}
