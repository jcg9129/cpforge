use super::*;

fn source(kind: LayerKind, platform: Platform) -> LayerSource {
    LayerSource {
        kind,
        file: if platform == Platform::Windows {
            r"C:\config.toml".into()
        } else {
            "/config.toml".into()
        },
    }
}

#[test]
fn every_new_layer_key_accepts_its_type_and_rejects_another_type() {
    // Each entry exercises one registry leaf, including nested submit stage fields.
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let absolute = if platform == Platform::Windows {
            "'C:/archive'"
        } else {
            "'/archive'"
        };
        let roots = format!("[{absolute}]");
        let cases = [
            ("status.values", "['wa']", "false"),
            ("archive.roots", roots.as_str(), "false"),
            ("archive.default_root", absolute, "false"),
            ("archive.layout", "'{site}/{problem_id}'", "false"),
            (
                "archive.contest_layout",
                "'{site}/contests/{contest_id}'",
                "false",
            ),
            ("archive.keep", "['sources']", "false"),
            ("archive.discard", "['submit']", "false"),
            ("archive.cleanup", "'trash'", "false"),
            ("archive.on_existing", "'error'", "false"),
            ("archive.profiles.small.keep", "['sources']", "false"),
            ("archive.profiles.small.discard", "['submit']", "false"),
            ("archive.profiles.small.cleanup", "'keep'", "false"),
            ("archive.quarantine.retention_days", "30", "false"),
            ("submit.clipboard", "true", "'true'"),
            ("submit.test_dir", "'tmp'", "false"),
            ("http.offline", "true", "'true'"),
            ("http.cache_dir", absolute, "false"),
            ("sites.atcoder.language", "'en'", "false"),
            (
                "languages.cpp.submit.argv",
                "['bundle', '{solution_sources}', '{submit_file}', '{judge}', '{submit_dir}']",
                "false",
            ),
            (
                "languages.cpp.submit.shell.interpreter",
                "['sh', '-c']",
                "true",
            ),
            (
                "languages.cpp.submit.shell.script",
                "'bundle {solution_sources}'",
                "true",
            ),
            ("languages.cpp.submit.shell.args", "[]", "true"),
            ("languages.cpp.submit.cwd.selector", "'run'", "true"),
            ("languages.cpp.submit.cwd.path", "'.'", "true"),
            ("languages.cpp.submit.env.X", "'value'", "17"),
            ("languages.cpp.submit.env_remove", "['X']", "true"),
            ("languages.cpp.submit.deadline_s", "'1s'", "true"),
            ("languages.cpp.submit.stdout_cap", "'1MiB'", "true"),
            ("languages.cpp.submit.stderr_cap", "'1MiB'", "true"),
            (
                "languages.cpp.submit.limits.address_space",
                "'1GiB'",
                "true",
            ),
            ("languages.cpp.submit.limits.job_commit", "'1GiB'", "true"),
            (
                "languages.cpp.submit.version",
                "{ argv = ['bundle', '--version'], expect = '^bundle 1' }",
                "{ argv = false, expect = '^bundle 1' }",
            ),
            (
                "languages.cpp.submit.version",
                "{ argv = ['bundle', '--version'], expect = '^bundle 1' }",
                "{ argv = ['bundle', '--version'], expect = false }",
            ),
            ("languages.cpp.submit.test", "true", "'true'"),
            ("languages.cpp.submit.judge", "'atcoder'", "false"),
        ];
        for (key, good, bad) in cases {
            let kind = if key == "http.cache_dir" {
                LayerKind::OverrideUser
            } else {
                LayerKind::Workspace
            };
            for (value, valid) in [(good, true), (bad, false)] {
                let text = format!("{key} = {value}\n");
                let layer = parse(
                    text.as_bytes(),
                    &source(kind, platform),
                    &Environment::default(),
                    platform,
                );
                assert_eq!(
                    layer.diagnostics.is_empty(),
                    valid,
                    "{platform:?}: {text}: {:?}",
                    layer.diagnostics
                );
                assert!(
                    layer
                        .diagnostics
                        .iter()
                        .all(|d| !d.message.contains("unknown")),
                    "{key}: {:?}",
                    layer.diagnostics
                );
            }
        }
    }
}

fn tables(text: &str, platform: Platform, include_stress: bool) -> ProblemTables {
    parse_problem_tables(
        format!("[problem]\nid = 'local:p'\n{text}").as_bytes(),
        &source(LayerKind::Problem, platform),
        &Environment::default(),
        platform,
        include_stress,
    )
}

#[test]
fn every_record_key_accepts_its_type_and_rejects_another_type() {
    let cases = [
        ("check.kind", "'checker'", "false"),
        ("check.answer", "'none'", "false"),
        ("check.display", "'judge-cf'", "false"),
        ("check.scoring", "'pass_fail'", "true"),
        ("check.scoring.points", "true", "'true'"),
        ("check.scoring.scale", "100", "false"),
        ("check.scoring.aggregation", "'min'", "false"),
        (
            "check.interactive.checking",
            "'interactor+checker'",
            "false",
        ),
        ("check.interactive.mode", "'relay'", "false"),
        ("check.interactive.idle_s", "0", "false"),
        ("check.interactive.early_reader_close", "false", "'false'"),
        ("check.interactive.interactor_allowance_s", "'1s'", "false"),
        ("check.interactive.peer_allowance_s", "'1s'", "false"),
        ("check.interactive.solution_seed", "'env'", "false"),
        (
            "check.interactor_test.good",
            "[{ sources = ['ok.cpp'], language = 'cpp' }]",
            "false",
        ),
        ("check.interactor_test.bad", "[]", "false"),
        ("stress.iterations", "10000", "false"),
        ("stress.time_s", "'1m'", "false"),
        ("stress.min_score", "'90.125'", "false"),
    ];
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for (key, good, bad) in cases {
            for (value, valid) in [(good, true), (bad, false)] {
                let parsed = tables(
                    &format!(
                        "[{}]\n{} = {value}",
                        key.rsplit_once('.').unwrap().0,
                        key.rsplit_once('.').unwrap().1
                    ),
                    platform,
                    true,
                );
                assert_eq!(
                    parsed.diagnostics.is_empty(),
                    valid,
                    "{key}={value}: {:?}",
                    parsed.diagnostics
                );
                assert!(
                    parsed
                        .diagnostics
                        .iter()
                        .all(|d| !d.message.contains("unknown"))
                );
                if key.starts_with("check.") {
                    let document = format!(
                        "[problem]\nid = 'local:p'\n[{}]\n{} = {value}",
                        key.rsplit_once('.').unwrap().0,
                        key.rsplit_once('.').unwrap().1
                    );
                    let layer = parse(
                        document.as_bytes(),
                        &source(LayerKind::Problem, platform),
                        &Environment::default(),
                        platform,
                    );
                    assert_eq!(
                        layer.diagnostics, parsed.diagnostics,
                        "loader integration: {key}"
                    );
                    for diagnostic in &layer.diagnostics {
                        assert!(
                            diagnostic.origin.line.is_some() && diagnostic.origin.col.is_some()
                        );
                    }
                }
            }
        }
        for role in [
            "check.checker",
            "check.validator",
            "check.interactive.interactor",
            "stress.generator",
            "stress.brute",
        ] {
            for (field, good) in [
                ("sources", "['a.cpp']"),
                ("support_files", "['a.h']"),
                ("language", "'cpp'"),
                ("entry", "'main'"),
                ("program", "'judge'"),
                ("assets", "['data.txt']"),
                ("stderr_cap", "'1MiB'"),
            ] {
                for (value, valid) in [(good, true), ("false", false)] {
                    let parsed = tables(&format!("[{role}]\n{field} = {value}"), platform, true);
                    assert_eq!(
                        parsed.diagnostics.is_empty(),
                        valid,
                        "{role}.{field}: {:?}",
                        parsed.diagnostics
                    );
                    assert!(
                        parsed
                            .diagnostics
                            .iter()
                            .all(|d| !d.message.contains("unknown"))
                    );
                }
            }
            let extra = if role.ends_with("interactor") {
                vec![("profile", "'testlib'"), ("seed", "'argv'")]
            } else if role.starts_with("check.") {
                vec![
                    ("profile", "'simple'"),
                    ("deadline_s", "'1s'"),
                    ("stdout_cap", "'1MiB'"),
                ]
            } else {
                vec![("deadline_s", "'1s'"), ("stdout_cap", "'1MiB'")]
            };
            for (field, good) in extra {
                assert!(
                    tables(&format!("[{role}]\n{field} = {good}"), platform, true)
                        .diagnostics
                        .is_empty()
                );
                assert!(
                    !tables(&format!("[{role}]\n{field} = false"), platform, true)
                        .diagnostics
                        .is_empty()
                );
            }
        }
    }
}

#[test]
fn layer_restrictions_unknown_keys_ranges_and_defaults() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for text in [
            "[http]\ncache_dir = '/cache'",
            "[archive]\nroots = ['relative']",
            "[archive]\nlayout = '{unknown}'",
            "[archive]\ndiscard = ['metadata']",
            "[archive]\nkeep = ['notes']\ndiscard = ['notes']",
            "[archive.quarantine]\nretention_days = 36501",
            "[status]\nvalues = ['solved']",
            "[status]\nvalues = ['wa', 'wa']",
            "[status]\nvalues = ['bad_status']",
            "[sites.atcoder]\nlanguage = 'fr'",
            "[archive]\nunknown = true",
            "[languages.cpp.submit.version]\nargv = ['bundle', '--version']",
            "[languages.cpp.submit.version]\nargv = ['bundle', '{judge}']\nexpect = 'x'",
        ] {
            assert!(
                !parse(
                    text.as_bytes(),
                    &source(LayerKind::Workspace, platform),
                    &Environment::default(),
                    platform
                )
                .diagnostics
                .is_empty(),
                "{text}"
            );
        }
        for text in [
            "[config.status]\nvalues = []",
            "[config.archive]\nroots = []",
            "[config.submit]\nclipboard = false",
            "[config.http]\noffline = false",
            "[config.sites.atcoder]\nlanguage = 'en'",
        ] {
            let text = format!("[problem]\nid = 'local:p'\n{text}");
            assert!(
                !parse(
                    text.as_bytes(),
                    &source(LayerKind::Problem, platform),
                    &Environment::default(),
                    platform
                )
                .diagnostics
                .is_empty()
            );
        }
        for text in [
            "[check]\nsubtasks = []",
            "[check]\ninteractor = {}",
            "[stress.generator]\nprofile = 'simple'",
            "[stress.brute]\nseed = 'env'",
            "[check.checker]\nprogram = 'a'\nsources = ['a.cpp']",
        ] {
            assert!(
                !tables(text, platform, true).diagnostics.is_empty(),
                "{text}"
            );
        }
        assert!(
            tables("[stress]\nunknown = true", platform, false)
                .diagnostics
                .is_empty()
        );
        for (header, defaults) in [
            ("check.scoring", SCORING_DEFAULTS),
            ("check.checker", ROLE_DEFAULTS),
            ("check.interactor_test", INTERACTOR_TEST_DEFAULTS),
            ("check.interactive.interactor", INTERACTOR_SEED_DEFAULTS),
        ] {
            assert!(
                tables(&format!("[{header}]\n{defaults}"), platform, true)
                    .diagnostics
                    .is_empty()
            );
        }
        let env = Environment::default();
        assert!(
            parse(
                WORKFLOW_DEFAULTS.as_bytes(),
                &source(LayerKind::Workspace, platform),
                &env,
                platform
            )
            .diagnostics
            .is_empty()
        );
        assert!(tables(&format!("[check]\n{CHECK_DEFAULTS}\n[check.interactive]\n{INTERACTIVE_DEFAULTS}\n[stress]\n{STRESS_DEFAULTS}"), platform, true).diagnostics.is_empty());
        assert!(
            parse(
                format!("[languages.cpp.submit]\n{SUBMIT_STAGE_DEFAULTS}").as_bytes(),
                &source(LayerKind::Workspace, platform),
                &env,
                platform
            )
            .diagnostics
            .is_empty()
        );
    }
}

#[test]
fn submit_stage_keeps_phase_one_results_on_the_written_nodes() {
    let text = "[languages.cpp.submit]\nargv = ['bundle', '{submit_file}']\ndeadline_s = '2s'\nshell = { script = 'x', bogus = 1 }\n";
    let layer = parse(
        text.as_bytes(),
        &source(LayerKind::Workspace, Platform::Linux),
        &Environment::default(),
        Platform::Linux,
    );
    let LayerState::Loaded(root) = &layer.state else {
        panic!("{:?}", layer.state);
    };
    let submit = root
        .get("languages")
        .and_then(|n| n.get("cpp"))
        .and_then(|n| n.get("submit"))
        .unwrap();
    assert_eq!(
        submit.get("deadline_s").unwrap().typed,
        Some(TypedValue::Milliseconds(2000))
    );
    assert!(
        submit
            .get("shell")
            .and_then(|n| n.get("bogus"))
            .unwrap()
            .rejected
    );
    // The written spelling is kept; the substitute placeholder never leaks.
    let RawValue::Array(argv) = &submit.get("argv").unwrap().value else {
        panic!("argv");
    };
    assert_eq!(argv[1].string(), Some("{submit_file}"));
    assert!(argv[1].typed.is_none());
}
