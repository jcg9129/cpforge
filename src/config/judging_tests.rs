//! Judging parsing is entirely in memory on every validation platform.
use super::*;
use crate::report::{AnswerPolicy, CheckMode, DisplayProfile, JudgeProfile, SeedMode};

const CHECKER: &str =
    "[check]\nkind = 'checker'\nchecker = { program = 'checker', profile = 'simple' }\n";
const INTERACTIVE: &str = "[check]\nkind = 'interactive'\n[check.interactive]\nchecking = 'interactor'\ninteractor = { program = 'interactor', profile = 'testlib' }\n";

fn raw(text: &str, kind: LayerKind, platform: Platform) -> RawLayer {
    parse(
        text.as_bytes(),
        &LayerSource {
            kind,
            file: match kind {
                LayerKind::Problem => "problem.toml",
                _ => "workspace.toml",
            }
            .into(),
        },
        &Environment::default(),
        platform,
    )
}
fn problem(text: &str, platform: Platform) -> MergedConfig {
    compose(
        &[raw(
            &format!("schema = 1\n[problem]\nid = 'local:p'\n{text}"),
            LayerKind::Problem,
            platform,
        )],
        platform,
    )
}
fn checked(text: &str, platform: Platform) -> CheckConfig {
    let config = problem(text, platform);
    let report = check(&config, &Selection::default());
    assert!(
        report.diagnostics().is_empty(),
        "{text}: {:?}",
        report.diagnostics()
    );
    report
        .universe()
        .check
        .as_deref()
        .cloned()
        .expect("typed judging config")
}
fn platforms() -> [Platform; 3] {
    [Platform::Linux, Platform::Macos, Platform::Windows]
}

#[test]
fn defaults_and_all_batch_fields_have_typed_values_and_provenance() {
    for platform in platforms() {
        let default = checked("", platform);
        assert_eq!(default.kind.value, CheckMode::Compare);
        assert_eq!(default.answer.value, AnswerPolicy::File);
        assert_eq!(default.display.value, DisplayProfile::Cpf);
        assert_eq!(default.scoring.value, ScoringConfig::PassFail);
        assert_eq!(default.kind.provenance, Provenance::BuiltIn);
        for profile in ["simple", "testlib"] {
            let text = format!(
                "[check]\nkind = 'checker'\nanswer = 'none'\ndisplay = 'judge-cf'\nscoring = {{ points = true, scale = 100, aggregation = 'min' }}\nchecker = {{ sources = ['judge.py'], support_files = ['helper.py'], language = 'python', entry = 'judge', profile = '{profile}', assets = ['dict/words'], deadline_s = '500ms', stdout_cap = '1MiB', stderr_cap = 23 }}\nvalidator = {{ program = 'validator', profile = '{profile}', assets = ['validation/data'] }}\n"
            );
            let config = problem(&text, platform);
            let report = check(&config, &Selection::default());
            assert!(
                report.diagnostics().is_empty(),
                "{:?}",
                report.diagnostics()
            );
            let check = report.universe().check.as_ref().unwrap();
            assert_eq!(check.kind.value, CheckMode::Checker);
            assert_eq!(check.answer.value, AnswerPolicy::None);
            assert_eq!(check.display.value, DisplayProfile::JudgeCf);
            let ScoringConfig::Points(points) = &check.scoring.value else {
                panic!("points")
            };
            assert_eq!(points.scale.value, 100);
            let role = check.checker.as_ref().unwrap();
            assert_eq!(
                role.profile.value,
                if profile == "simple" {
                    JudgeProfile::Simple
                } else {
                    JudgeProfile::Testlib
                }
            );
            assert_eq!(role.deadline_ms.as_ref().unwrap().value, 500);
            assert_eq!(role.stdout_cap.as_ref().unwrap().value, 1 << 20);
            assert_eq!(role.stderr_cap.as_ref().unwrap().value, 23);
            assert_eq!(
                role.captured_paths()
                    .iter()
                    .map(|p| p.value.as_str())
                    .collect::<Vec<_>>(),
                ["judge.py", "helper.py", "dict/words"]
            );
            assert_eq!(
                check
                    .validator
                    .as_ref()
                    .unwrap()
                    .captured_paths()
                    .iter()
                    .map(|p| p.value.as_str())
                    .collect::<Vec<_>>(),
                ["validator", "validation/data"]
            );
            let RoleInput::Built(built) = &role.input else {
                panic!("built")
            };
            assert!(built.build.is_none());
            assert_eq!(built.entry.as_ref().unwrap().value, "judge");
            assert_eq!(
                built.run.node.get("deadline_s").unwrap().typed,
                Some(TypedValue::Milliseconds(500))
            );
            fn origins(n: &EffectiveNode) {
                if let EffectiveValue::Table(t) = &n.value {
                    for n in t.values() {
                        origins(n);
                    }
                } else if let EffectiveValue::Array(a) = &n.value {
                    for n in a {
                        origins(n);
                    }
                } else if let Provenance::File { path, line, col } = &n.provenance {
                    assert_eq!(path, &std::path::PathBuf::from("problem.toml"));
                    assert!(line.is_some() && col.is_some(), "{n:?}");
                    assert!(n.origin.key_path.starts_with("check."));
                } else {
                    assert_eq!(n.provenance, Provenance::BuiltIn);
                }
            }
            origins(config.at(&["check"]).unwrap());
        }
    }
}

#[test]
fn interactive_defaults_seeds_and_solution_specs_are_inspectable() {
    for platform in platforms() {
        let config = checked(INTERACTIVE, platform);
        let interactive = config.interactive.unwrap();
        assert_eq!(interactive.idle_ms.value, 0);
        assert_eq!(interactive.interactor_allowance_ms.value, 1000);
        assert_eq!(interactive.peer_allowance_ms.value, 1000);
        assert!(!interactive.early_reader_close.value);
        assert_eq!(interactive.solution_seed.value, SeedMode::None);
        assert_eq!(interactive.interactor.seed.unwrap().value, SeedMode::None);
        for seed in ["none", "env", "argv"] {
            let document = format!(
                "[check]\nkind = 'interactive'\n[check.interactive]\nchecking = 'interactor'\ninteractor = {{ sources = ['i.cpp'], language = 'cpp', profile = 'testlib', seed = '{seed}', assets = ['data'], stderr_cap = 12 }}\nmode = 'relay'\nidle_s = '500ms'\nearly_reader_close = true\ninteractor_allowance_s = '2s'\npeer_allowance_s = '3s'\nsolution_seed = 'env'\n[check.interactor_test]\ngood = [{{ sources = ['ok.cpp'], support_files = ['lib.h'], language = 'cpp', entry = 'ok' }}]\nbad = [{{ sources = ['bad.cpp'], language = 'cpp' }}]\n"
            );
            let check = checked(&document, platform);
            assert_eq!(check.interactive.as_ref().unwrap().idle_ms.value, 500);
            assert_eq!(check.interactor_test.as_ref().unwrap().good.len(), 1);
            assert_eq!(check.interactor_test.as_ref().unwrap().bad.len(), 1);
        }
        for checking in ["checker", "interactor+checker"] {
            let text = INTERACTIVE
                .replace(
                    "checking = 'interactor'",
                    &format!("checking = '{checking}'"),
                )
                .replace(
                    "kind = 'interactive'",
                    "kind = 'interactive'\nchecker = { program = 'c', profile = 'simple' }",
                );
            checked(&text, platform);
        }
        checked(
            &INTERACTIVE.replace(
                "checking = 'interactor'",
                "checking = 'interactor'\nmode = 'direct'\nidle_s = 0",
            ),
            platform,
        );
    }
}

#[test]
fn phase_two_presence_completeness_and_cross_field_errors() {
    let cases = [
        ("[check]\nkind = 'checker'\n", "check.checker"),
        (
            "[check]\nchecker = { program = 'c', profile = 'simple' }\n",
            "check.checker",
        ),
        ("[check]\ninteractive = {}\n", "check.interactive"),
        ("[check]\ninteractor_test = {}\n", "check.interactor_test"),
        ("[check]\nanswer = 'none'\n", "check.answer"),
        (
            "[check]\nscoring = { points = true, scale = 100 }\n",
            "check.scoring",
        ),
        (
            "[check]\nkind = 'checker'\nchecker = {}\n",
            "check.checker.profile",
        ),
        (
            "[check]\nvalidator = { sources = [], language = 'cpp', profile = 'simple' }\n",
            "check.validator.sources",
        ),
        (
            "[check]\nvalidator = { sources = ['v'], profile = 'simple' }\n",
            "check.validator.language",
        ),
        (
            "[check]\nvalidator = { language = 'cpp', profile = 'simple' }\n",
            "check.validator.sources",
        ),
        (
            "[check]\nvalidator = { sources = ['v'], language = 'missing', profile = 'simple' }\n",
            "check.validator.language",
        ),
        (
            "[check]\nvalidator = { sources = ['v'], support_files = ['v'], language = 'cpp', profile = 'simple' }\n",
            "check.validator.support_files[0]",
        ),
        (
            "[check]\nvalidator = { program = 'v', assets = ['v'], profile = 'simple' }\n",
            "check.validator.assets[0]",
        ),
        (
            "[check]\nvalidator = { sources = ['v'], support_files = ['h'], assets = ['h'], language = 'cpp', profile = 'simple' }\n",
            "check.validator.assets[0]",
        ),
        (
            "[check]\nkind = 'interactive'\n[check.interactive]\ninteractor = { program = 'i', profile = 'testlib' }\n",
            "check.interactive.checking",
        ),
        (
            "[check]\nkind = 'interactive'\n[check.interactive]\nchecking = 'interactor'\n",
            "check.interactive.interactor",
        ),
        ("[check]\nkind = 'interactive'\n", "check.interactive"),
    ];
    for platform in platforms() {
        for (text, key) in cases {
            let config = problem(text, platform);
            assert!(
                config.diagnostics.is_empty(),
                "raw: {text}: {:?}",
                config.diagnostics
            );
            let report = check(&config, &Selection::default());
            assert!(
                report
                    .diagnostics()
                    .iter()
                    .any(|d| d.origin.key_path == key),
                "{text}: {:?}",
                report.diagnostics()
            );
            assert!(report.executable().is_none());
        }
        for reserved in ["input", "output", "answer", "tout", "report", "transcript"] {
            let config = problem(
                &format!(
                    "[check]\nvalidator = {{ program = 'v', profile = 'simple', assets = ['{reserved}/data'] }}"
                ),
                platform,
            );
            assert!(
                check(&config, &Selection::default())
                    .diagnostics()
                    .iter()
                    .any(|d| d.message.contains("reserved asset"))
            );
        }
        let config = problem(
            &INTERACTIVE.replace(
                "checking = 'interactor'",
                "checking = 'interactor'\nmode = 'direct'\nidle_s = 1",
            ),
            platform,
        );
        assert!(
            check(&config, &Selection::default())
                .diagnostics()
                .iter()
                .any(|d| d.origin.key_path == "check.interactive.idle_s")
        );
        let config = problem(
            &CHECKER.replace("checker =", "scoring = {}\nchecker ="),
            platform,
        );
        let report = check(&config, &Selection::default());
        for key in ["check.scoring.points", "check.scoring.scale"] {
            assert!(
                report
                    .diagnostics()
                    .iter()
                    .any(|d| d.origin.key_path == key)
            );
        }
    }
}

#[test]
fn invalid_fields_skip_only_dependent_phase_two_checks() {
    for platform in platforms() {
        let config = problem(
            "[check]\nkind = false\nvalidator = { sources = ['v'], language = 'unknown', profile = 'simple' }\n",
            platform,
        );
        let report = check(&config, &Selection::default());
        assert_eq!(report.diagnostics().len(), 2, "{:?}", report.diagnostics());
        assert!(
            report
                .diagnostics()
                .iter()
                .any(|d| d.origin.key_path == "check.kind")
        );
        assert!(
            report
                .diagnostics()
                .iter()
                .any(|d| d.origin.key_path == "check.validator.language")
        );
        let config = problem(
            "[check]\nkind = 'checker'\nchecker = { sources = false, language = false, profile = false }\n",
            platform,
        );
        let report = check(&config, &Selection::default());
        assert_eq!(report.diagnostics().len(), 3, "{:?}", report.diagnostics());
    }
}

#[test]
fn role_stages_use_rows_one_through_three_and_enforce_private_context() {
    for platform in platforms() {
        let workspace = raw(
            "[languages.cpp]\nentry = 'base'\n[languages.cpp.profiles.fast.run]\ncwd = { selector = 'problem' }\ndeadline_s = 2\n",
            LayerKind::Workspace,
            platform,
        );
        let problem = raw(
            "[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\nentry = 'solution'\n[config.run]\ncwd = { selector = 'problem' }\ndeadline_s = 9\n[check]\nvalidator = { sources = ['v.cpp'], language = 'cpp', profile = 'simple', deadline_s = '500ms' }\n",
            LayerKind::Problem,
            platform,
        );
        let config = compose(&[workspace, problem], platform);
        let report = check(
            &config,
            &Selection {
                profile: Some("fast".into()),
                ..Selection::default()
            },
        );
        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );
        let RoleInput::Built(role) = &report
            .universe()
            .check
            .as_ref()
            .unwrap()
            .validator
            .as_ref()
            .unwrap()
            .input
        else {
            panic!("built")
        };
        assert_eq!(role.entry.as_ref().unwrap().value, "base");
        assert_eq!(
            role.run.node.at(&["cwd", "selector"]).unwrap().string(),
            Some("run")
        );
        assert_eq!(
            role.run.node.get("deadline_s").unwrap().typed,
            Some(TypedValue::Milliseconds(500))
        );
        for bad in [
            "cwd = { selector = 'problem' }",
            "cwd = { selector = 'workspace' }",
            "cwd = { path = 'private' }",
            "argv = ['judge', '{workspace}']",
            "argv = ['judge', '{problem_dir}']",
            "argv = ['judge', '{test_input}']",
            "argv = ['judge', '{test_answer}']",
            "argv = ['judge', '{iteration}']",
        ] {
            let workspace = raw(
                &format!("[languages.python.run]\n{bad}\n"),
                LayerKind::Workspace,
                platform,
            );
            let problem = raw(
                "[problem]\nid = 'local:p'\n[check]\nvalidator = { sources = ['v.py'], language = 'python', profile = 'simple' }",
                LayerKind::Problem,
                platform,
            );
            let config = compose(&[workspace, problem], platform);
            let report = check(&config, &Selection::default());
            assert!(
                report
                    .diagnostics()
                    .iter()
                    .any(|d| d.message.contains("role stage")),
                "{bad}: {:?}",
                report.diagnostics()
            );
        }
        let workspace = raw(
            "[languages.python.run]\nargv = ['judge', '{problem_id}']",
            LayerKind::Workspace,
            platform,
        );
        let problem = raw(
            "[problem]\nid = 'local:p'\n[check]\nvalidator = { sources = ['v.py'], language = 'python', profile = 'simple' }",
            LayerKind::Problem,
            platform,
        );
        assert!(
            check(
                &compose(&[workspace, problem], platform),
                &Selection::default()
            )
            .diagnostics()
            .is_empty()
        );
    }
}

#[test]
fn judged_test_cap_preserves_compare_only_jobs() {
    for platform in platforms() {
        for text in [
            CHECKER,
            INTERACTIVE,
            "[check]\nvalidator = { program = 'v', profile = 'simple' }",
        ] {
            let config = checked(text, platform);
            assert!(config.validate_test_count(1000).is_ok());
            assert!(
                config
                    .validate_test_count(1001)
                    .unwrap_err()
                    .contains("select fewer with --case")
            );
        }
        assert!(checked("", platform).validate_test_count(9999).is_ok());
    }
}

#[test]
fn metadata_never_supplies_role_specs_and_non_problem_layers_refuse_them() {
    for platform in platforms() {
        let config = checked(
            "[fetched]\nchecker = { program = 'untrusted', profile = 'simple' }\n[metadata]\nvalidator = { program = 'untrusted', profile = 'testlib' }",
            platform,
        );
        assert!(config.checker.is_none() && config.validator.is_none());
        for kind in [LayerKind::Workspace, LayerKind::DefaultUser] {
            let raw = raw(CHECKER, kind, platform);
            assert!(raw.diagnostics.iter().any(|d| d.origin.key_path == "check"));
            assert!(raw.problem_check.is_none());
        }
    }
}

#[test]
#[should_panic(expected = "judging comes only from local problem configuration")]
fn record_parser_rejects_non_problem_sources() {
    parse_problem_tables(
        b"[check]",
        &LayerSource {
            kind: LayerKind::Workspace,
            file: "fetched.toml".into(),
        },
        &Environment::default(),
        Platform::Linux,
        false,
    );
}

#[test]
fn phase_one_boundaries_unknown_keys_and_exclusive_role_forms() {
    for platform in platforms() {
        for (field, values) in [
            (
                "scoring",
                vec![
                    "{ points = true, scale = 0 }",
                    "{ points = true, scale = 1000000001 }",
                    "{ points = true, scale = 1.5 }",
                    "{ points = true, scale = '100' }",
                    "{ points = false, scale = 1 }",
                    "{ points = true, scale = 1, aggregation = 'max' }",
                ],
            ),
            (
                "checker",
                vec![
                    "{ program = 'c', sources = ['c.cpp'], profile = 'simple' }",
                    "{ program = 'c', language = 'cpp', profile = 'simple' }",
                    "{ program = 'c', entry = 'main', profile = 'simple' }",
                    "{ program = 'c', support_files = [], profile = 'simple' }",
                    "{ program = '../c', profile = 'simple' }",
                    "{ program = 'c', profile = 'custom' }",
                    "{ sources = ['c','c'], language = 'cpp', profile = 'simple' }",
                    "{ program = 'c', profile = 'simple', assets = ['a','a'] }",
                    "{ program = 'c', profile = 'simple', seed = 'env' }",
                    "{ program = 'c', profile = 'simple', extra = 1 }",
                ],
            ),
        ] {
            for value in values {
                let text = if field == "checker" {
                    format!("[check]\nkind = 'checker'\nchecker = {value}\n")
                } else {
                    CHECKER.replace(
                        "kind = 'checker'",
                        &format!("kind = 'checker'\n{field} = {value}"),
                    )
                };
                let config = problem(&text, platform);
                assert!(!config.diagnostics.is_empty(), "{text}");
                assert!(config.diagnostics.iter().all(|d| d.origin.file
                    == std::path::Path::new("problem.toml")
                    && d.origin.key_path.starts_with("check.")
                    && d.origin.line.is_some()
                    && d.origin.col.is_some()));
                assert!(check(&config, &Selection::default()).executable().is_none());
            }
        }
        for scale in [1, 1_000_000_000] {
            checked(
                &CHECKER.replace(
                    "kind = 'checker'",
                    &format!("kind = 'checker'\nscoring = {{ points = true, scale = {scale} }}"),
                ),
                platform,
            );
        }
        for field in [
            "deadline_s = 0",
            "deadline_s = '0.0004s'",
            "stdout_cap = 0",
            "stderr_cap = '1.5MiB'",
        ] {
            let text = CHECKER.replace(
                "profile = 'simple'",
                &format!("profile = 'simple', {field}"),
            );
            assert!(!problem(&text, platform).diagnostics.is_empty());
        }
        let config = checked(
            &CHECKER.replace(
                "profile = 'simple'",
                "profile = 'simple', deadline_s = '0.0005s'",
            ),
            platform,
        );
        assert_eq!(
            config
                .checker
                .as_ref()
                .unwrap()
                .deadline_ms
                .as_ref()
                .unwrap()
                .value,
            1
        );
        for (field, value) in [
            ("idle_s", "-1"),
            ("idle_s", "'0.0004s'"),
            ("checking", "'both'"),
            ("mode", "'other'"),
            ("solution_seed", "'argv'"),
        ] {
            let text = if field == "checking" {
                INTERACTIVE.replace("checking = 'interactor'", &format!("checking = {value}"))
            } else {
                INTERACTIVE.replace(
                    "checking = 'interactor'",
                    &format!("checking = 'interactor'\n{field} = {value}"),
                )
            };
            assert!(!problem(&text, platform).diagnostics.is_empty(), "{text}");
        }
        for field in [
            "deadline_s = 1",
            "stdout_cap = 1",
            "profile = 'simple'",
            "seed = 'random'",
        ] {
            let text = if field.starts_with("profile") {
                INTERACTIVE.replace("profile = 'testlib'", field)
            } else {
                INTERACTIVE.replace(
                    "profile = 'testlib'",
                    &format!("profile = 'testlib', {field}"),
                )
            };
            assert!(!problem(&text, platform).diagnostics.is_empty(), "{text}");
        }
    }
}

#[test]
fn solution_spec_fields_validate_types_and_keep_element_origins() {
    for platform in platforms() {
        for list in ["good", "bad"] {
            for (key, good) in [
                ("sources", "['ok.cpp']"),
                ("support_files", "['lib.h']"),
                ("language", "'cpp'"),
                ("entry", "'Main'"),
            ] {
                for (value, valid) in [(good, true), ("false", false)] {
                    let text = format!(
                        "{INTERACTIVE}[check.interactor_test]\n{list} = [{{ {key} = {value} }}]\n"
                    );
                    let config = problem(&text, platform);
                    assert_eq!(
                        config.diagnostics.is_empty(),
                        valid,
                        "{text}: {:?}",
                        config.diagnostics
                    );
                    let node = &config
                        .at(&["check", "interactor_test", list])
                        .unwrap()
                        .array()
                        .unwrap()[0];
                    let value = node.get(key).unwrap();
                    assert!(matches!(
                        value.provenance,
                        Provenance::File {
                            line: Some(_),
                            col: Some(_),
                            ..
                        }
                    ));
                }
            }
        }
    }
}

#[test]
fn problem_kind_rules_and_path_equality_follow_the_validation_platform() {
    for platform in platforms() {
        let document = "[problem]\nid = 'local:p'\nkind = 'interactive'\n";
        let config = compose(&[raw(document, LayerKind::Problem, platform)], platform);
        assert!(
            check(&config, &Selection::default())
                .diagnostics()
                .is_empty()
        );
        assert!(
            check(
                &config,
                &Selection {
                    operation: Operation::Test,
                    ..Selection::default()
                }
            )
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("interactive problem needs"))
        );
        for text in [
            "[problem]\nid = 'local:p'\nkind = 'interactive'\n[config]\nio = { input = 'in' }\n"
                .to_owned(),
            format!("[problem]\nid = 'local:p'\n[config]\nio = {{ input = 'in' }}\n{INTERACTIVE}"),
        ] {
            let config = compose(&[raw(&text, LayerKind::Problem, platform)], platform);
            assert!(
                check(&config, &Selection::default())
                    .diagnostics()
                    .iter()
                    .any(|d| d
                        .message
                        .contains("file I/O is not allowed for interactive"))
            );
        }
        for assets in ["['INPUT/data']", "['V']"] {
            let config = problem(
                &format!(
                    "[check]\nvalidator = {{ program = 'v', profile = 'simple', assets = {assets} }}"
                ),
                platform,
            );
            assert_eq!(
                check(&config, &Selection::default())
                    .diagnostics()
                    .is_empty(),
                platform == Platform::Linux
            );
        }
    }
}

#[test]
fn invalid_inherited_role_entry_is_not_decoded_as_a_valid_string() {
    for platform in platforms() {
        let workspace = raw(
            "[languages.python]\nentry = false",
            LayerKind::Workspace,
            platform,
        );
        let problem = raw(
            "[problem]\nid = 'local:p'\n[check]\nvalidator = { sources = ['v.py'], language = 'python', profile = 'simple' }",
            LayerKind::Problem,
            platform,
        );
        let report = check(
            &compose(&[workspace, problem], platform),
            &Selection::default(),
        );
        assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
        assert_eq!(
            report.diagnostics()[0].origin.key_path,
            "languages.python.entry"
        );
        assert!(report.executable().is_none());
    }
}

#[test]
fn invalid_inherited_artifacts_are_diagnosed_before_role_planning() {
    for platform in platforms() {
        for artifacts in ["{ file = false }", "{ dir = true, required = [false] }"] {
            let merged = compose(
                &[
                    raw(
                        &format!("schema = 1\n[languages.cpp]\nartifacts = {artifacts}"),
                        LayerKind::Workspace,
                        platform,
                    ),
                    raw(
                        "schema = 1\n[problem]\nid = 'local:p'\n[check]\nvalidator = { sources = ['v.cpp'], language = 'cpp', profile = 'simple' }",
                        LayerKind::Problem,
                        platform,
                    ),
                ],
                platform,
            );
            assert!(
                check(&merged, &Selection::default())
                    .diagnostics()
                    .iter()
                    .any(|d| d.severity == Severity::Error)
            );
        }
    }
}
