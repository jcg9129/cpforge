use std::path::PathBuf;

use super::*;

fn source(kind: LayerKind, platform: Platform) -> LayerSource {
    LayerSource {
        kind,
        file: PathBuf::from(if platform == Platform::Windows {
            r"C:\u\cfg.toml"
        } else {
            "/u/cfg.toml"
        }),
    }
}
fn env() -> Environment {
    Environment {
        home: Some("/home/test".into()),
        ..Environment::default()
    }
}
fn raw(text: &str, platform: Platform) -> RawLayer {
    parse(
        text.as_bytes(),
        &source(LayerKind::Workspace, platform),
        &env(),
        platform,
    )
}
fn root(layer: &RawLayer) -> &RawNode {
    let LayerState::Loaded(root) = &layer.state else {
        panic!("expected loaded layer: {layer:?}")
    };
    root
}
fn at<'a>(layer: &'a RawLayer, keys: &[&str]) -> &'a RawNode {
    keys.iter()
        .fold(root(layer), |node, key| node.get(key).unwrap())
}
fn array(n: &RawNode, index: usize) -> &RawNode {
    let RawValue::Array(a) = &n.value else {
        panic!("expected array")
    };
    &a[index]
}
fn valid(layer: &RawLayer) {
    assert!(layer.diagnostics.is_empty(), "{:?}", layer.diagnostics);
}
fn errors(layer: &RawLayer) -> Vec<(&str, Option<usize>, &str)> {
    layer
        .diagnostics
        .iter()
        .map(|d| {
            (
                d.origin.key_path.as_str(),
                d.origin.line,
                d.message.as_str(),
            )
        })
        .collect()
}

#[test]
fn names_placement_and_nested_types() {
    for (text, key, line, message) in [
        (
            "[languages.cpp]\nextension = ['cpp']",
            "languages.cpp.extension",
            2,
            "unknown configuration key",
        ),
        (
            "[run]\nstdin = 'null'",
            "run.stdin",
            2,
            "stdin is allowed only in commands",
        ),
        (
            "[languages.cpp]\nrun = false",
            "languages.cpp.run",
            2,
            "expected a table",
        ),
        (
            "[build]\nargv = false",
            "build.argv",
            2,
            "expected an array of strings",
        ),
        ("[run]\nargv = [1]", "run.argv[0]", 2, "expected a string"),
        (
            "[run.shell]\nscript = 2",
            "run.shell.script",
            2,
            "expected a string",
        ),
        (
            "[run.shell]\nextra = 1",
            "run.shell.extra",
            2,
            "unknown configuration key",
        ),
        (
            "[run.cwd]\nselector = 'elsewhere'",
            "run.cwd.selector",
            2,
            "expected one of run, problem, workspace",
        ),
        (
            "[languages.cpp.template]\nextra = 1\npath = 'x'",
            "languages.cpp.template.extra",
            2,
            "unknown configuration key",
        ),
        (
            "[languages.cpp.artifacts]\ndir = false",
            "languages.cpp.artifacts.dir",
            2,
            "artifacts.dir must be true",
        ),
        (
            "[run.limits]\nother = 1",
            "run.limits.other",
            2,
            "unknown configuration key",
        ),
        (
            "[hooks]\non_switch = ['bad.name']",
            "hooks.on_switch[0]",
            2,
            "invalid command name",
        ),
        (
            "[commands.x]\nneeds_build = 1",
            "commands.x.needs_build",
            2,
            "expected a boolean",
        ),
        (
            "[commands.x]\nstdin = 'file'",
            "commands.x.stdin",
            2,
            "expected one of null, test",
        ),
        (
            "[languages.cpp]\nentry = ''",
            "languages.cpp.entry",
            2,
            "string must not be empty",
        ),
        (
            "[languages.cpp]\nextensions = ['c.pp']",
            "languages.cpp.extensions[0]",
            2,
            "extension must be non-empty and contain neither . nor /",
        ),
    ] {
        let layer = raw(text, Platform::Linux);
        assert_eq!(errors(&layer), vec![(key, Some(line), message)], "{text}");
        assert!(root(&layer).has_errors());
    }
    for name in [
        "", "1cpp", "cpp.foo", "é", "a b", "linux", "unix", "macos", "windows",
    ] {
        let text = format!("[languages.{name:?}]\nentry = 'x'");
        let layer = raw(&text, Platform::Linux);
        assert_eq!(layer.diagnostics.len(), 1, "{text}");
        assert_eq!(layer.diagnostics[0].origin.line, Some(1));
    }
    for name in ["C", "cpp23", "a-b_c", "CON"] {
        valid(&raw(
            &format!("[languages.{name}]\nentry = 'x'"),
            Platform::Linux,
        ));
    }
    let problem = parse(
        b"[problem]\nid = 'local:p'\n[config]\ncommands = {}",
        &source(LayerKind::Problem, Platform::Linux),
        &env(),
        Platform::Linux,
    );
    assert_eq!(
        errors(&problem),
        vec![(
            "config.commands",
            Some(4),
            "key is not allowed in the problem config layer"
        )]
    );
    for kind in [
        LayerKind::Workspace,
        LayerKind::OverrideUser,
        LayerKind::Problem,
    ] {
        let text = if kind == LayerKind::Problem {
            "[problem]\nid = 'local:p'\n[config]\nhooks = {}"
        } else {
            "[hooks]\nenabled = true\ntrusted_workspaces = ['/x']"
        };
        let layer = parse(
            text.as_bytes(),
            &source(kind, Platform::Linux),
            &env(),
            Platform::Linux,
        );
        let expected = if kind == LayerKind::Problem {
            vec![(
                "config.hooks",
                Some(4),
                "key is not allowed in the problem config layer",
            )]
        } else {
            vec![
                (
                    "hooks.enabled",
                    Some(2),
                    "key is allowed only in the default user configuration file",
                ),
                (
                    "hooks.trusted_workspaces",
                    Some(3),
                    "key is allowed only in the default user configuration file",
                ),
            ]
        };
        assert_eq!(errors(&layer), expected, "{kind:?}");
    }
    valid(&parse(
        b"[hooks]\nenabled = false\ntrusted_workspaces = ['/x']",
        &source(LayerKind::DefaultUser, Platform::Linux),
        &env(),
        Platform::Linux,
    ));
}

#[test]
fn raw_choices_overlays_and_presence_are_preserved() {
    let layer = raw(
        "[run]\nargv = ['x']\n[run.windows]\nshell = { interpreter = ['sh', '-c'], script = 'true' }\n[commands.linux]\nrepeat = 1",
        Platform::Linux,
    );
    valid(&layer);
    assert!(at(&layer, &["run"]).get("shell").is_none());
    assert!(at(&layer, &["run", "windows"]).get("shell").is_some());
    assert!(at(&layer, &["commands", "linux"]).get("repeat").is_some());
    assert!(
        at(&layer, &["commands", "linux"])
            .get("needs_build")
            .is_none()
    );
    for path in [
        "run",
        "languages.cpp.windows.run",
        "languages.cpp.profiles.debug.run",
        "commands.linux",
    ] {
        let text = format!("[{path}]\nargv = ['x']\nshell = {{ script = 'true' }}");
        let layer = raw(&text, Platform::Linux);
        assert_eq!(
            errors(&layer),
            vec![(
                format!("{path}.argv").as_str(),
                Some(2),
                "argv and shell are mutually exclusive"
            )]
        );
        assert_eq!(
            layer.diagnostics[0].second_origin.as_ref().unwrap().line,
            Some(3)
        );
        let keys: Vec<_> = path.split('.').collect();
        assert!(at(&layer, &keys).invalid);
    }
    for (text, key, other) in [
        (
            "[run.cwd]\npath = 'x'\nselector = 'run'",
            "run.cwd.path",
            "run.cwd.selector",
        ),
        (
            "[languages.cpp.artifacts]\nfile = 'x'\ndir = true",
            "languages.cpp.artifacts.file",
            "languages.cpp.artifacts.dir",
        ),
        (
            "[languages.cpp.artifacts]\nfile = 'x'\nrequired = ['x']",
            "languages.cpp.artifacts.file",
            "languages.cpp.artifacts.required",
        ),
    ] {
        let layer = raw(text, Platform::Linux);
        assert_eq!(layer.diagnostics.len(), 1);
        assert_eq!(layer.diagnostics[0].origin.key_path, key);
        assert_eq!(
            layer.diagnostics[0]
                .second_origin
                .as_ref()
                .unwrap()
                .key_path,
            other
        );
    }
    for (path, body) in [
        ("run.unix.linux", "deadline_s = 1"),
        ("languages.cpp.windows.run.linux", "deadline_s = 1"),
        ("languages.cpp.profiles.p.build.windows", "deadline_s = 1"),
        // A profile holds only build/run, so give the nested overlay a valid body.
        (
            "languages.cpp.windows.profiles.p.unix",
            "run = { argv = ['x'] }",
        ),
    ] {
        let layer = raw(&format!("[{path}]\n{body}"), Platform::Linux);
        assert_eq!(
            layer.diagnostics.len(),
            1,
            "{path}: {:?}",
            layer.diagnostics
        );
        assert_eq!(layer.diagnostics[0].origin.key_path, path);
    }
    for path in [
        "languages.cpp.windows.run",
        "languages.cpp.windows.profiles.debug.build",
        "languages.cpp.profiles.debug.windows.build",
        "languages.cpp.build.windows",
        "build.unix",
        "commands.linux.windows",
    ] {
        valid(&raw(&format!("[{path}]\nargv = ['x']"), Platform::Linux));
    }
    let layer = raw(
        "[languages.cpp]\nbuild = false\n[languages.cpp.profiles.p]\nbuild = false\n[run]\nenv = { X = false }\nlimits = {}",
        Platform::Linux,
    );
    valid(&layer);
    assert_eq!(
        at(&layer, &["languages", "cpp", "build"]).value,
        RawValue::Boolean(false)
    );
    for (text, key, line, message) in [
        ("build = false", "build", 1, "expected a table"),
        ("[run]\nenv = false", "run.env", 2, "expected a table"),
        ("[run]\nlimits = false", "run.limits", 2, "expected a table"),
        (
            "[run]\nenv = { X = true }",
            "run.env.X",
            2,
            "environment value must be a string or false",
        ),
        (
            "[languages.cpp.profiles.p]\nrun = false",
            "languages.cpp.profiles.p.run",
            2,
            "expected a table",
        ),
        ("[run]\ncwd = false", "run.cwd", 2, "expected a table"),
    ] {
        assert_eq!(
            errors(&raw(text, Platform::Linux)),
            vec![(key, Some(line), message)],
            "{text}"
        );
    }
    let problem = parse(
        b"[problem]\nid = 'local:p'\n[config]\nbuild = false",
        &source(LayerKind::Problem, Platform::Linux),
        &env(),
        Platform::Linux,
    );
    assert_eq!(
        errors(&problem),
        vec![("config.build", Some(4), "expected a table")]
    );
}

#[test]
fn placeholder_grammar_and_program_rebasing() {
    assert_eq!(
        tokenize("{{x}}/{source}"),
        Ok(vec![
            Token::Literal("{x}/".into()),
            Token::Placeholder(Placeholder::Source)
        ])
    );
    assert_eq!(
        tokenize("x}{unknown}{"),
        Err(vec![
            "lone closing brace".into(),
            "unknown placeholder {unknown}".into(),
            "lone opening brace".into()
        ])
    );
    for (program, expected, lookup) in [
        ("bin/x", Some("/u/bin/x"), false),
        ("./bin/x", Some("/u/./bin/x"), false),
        ("g++", None, true),
        ("{workspace}/{problem_id}/tool", None, false),
        ("/opt/{problem_id}/tool", None, false),
        ("{problem_dir}/x", None, false),
        ("~/bin/x", Some("/home/test/bin/x"), false),
        ("tool-{problem_id}", None, false),
    ] {
        let layer = raw(&format!("[run]\nargv = [{program:?}]"), Platform::Linux);
        valid(&layer);
        let Some(TypedValue::Text(parsed)) = &array(at(&layer, &["run", "argv"]), 0).typed else {
            panic!()
        };
        // Compare raw spellings: `Path` equality ignores `.` components, and
        // config-relative paths must never be canonicalised.
        assert_eq!(
            parsed.path.as_ref().map(|p| p.as_os_str()),
            expected.map(std::ffi::OsStr::new),
            "{program}"
        );
        assert_eq!(parsed.lookup, lookup, "{program}");
    }
    for (text, key, message) in [
        (
            "[run]\nargv = ['./{problem_id}/tool']",
            "run.argv[0]",
            "relative path with a placeholder must start with a placeholder",
        ),
        (
            "[run]\nargv = ['']",
            "run.argv[0]",
            "program or cwd path must not be empty",
        ),
        ("[run]\nargv = []", "run.argv", "argv must not be empty"),
        (
            "[run]\nargv = ['{sources}']",
            "run.argv[0]",
            "multi placeholder must be a whole argument and cannot occupy program or cwd position",
        ),
        (
            "[run]\nargv = ['x', '-I{sources}']",
            "run.argv[1]",
            "multi placeholder must be a whole argument and cannot occupy program or cwd position",
        ),
        (
            "[run]\nargv = ['x', '{nope}']",
            "run.argv[1]",
            "unknown placeholder {nope}",
        ),
        (
            "[run]\nargv = ['x', '{']",
            "run.argv[1]",
            "lone opening brace",
        ),
        (
            "[run]\nargv = ['x', '}']",
            "run.argv[1]",
            "lone closing brace",
        ),
        (
            "[run]\nshell = { interpreter = ['{workspace}/sh', '-c'] }",
            "run.shell.interpreter[0]",
            "shell interpreter must be literal (no placeholders or brace escapes)",
        ),
        (
            "[run]\nshell = { interpreter = ['bash'] }",
            "run.shell.interpreter",
            "shell interpreter must have the form [program, \"-c\"]",
        ),
    ] {
        let layer = raw(text, Platform::Linux);
        assert_eq!(errors(&layer), vec![(key, Some(2), message)], "{text}");
    }
    let layer = raw(
        "[run]\nargv = ['x', '{sources}', '-o{artifact}']\ncwd = { path = 'out' }\nenv = { X = '{unexpanded}' }\nshell = { script = '{literal}', args = ['{sources}'] }",
        Platform::Linux,
    );
    // Only the raw argv/shell choice conflicts; literal env and script are accepted.
    assert_eq!(layer.diagnostics.len(), 1);
    let Some(TypedValue::Text(cwd)) = &at(&layer, &["run", "cwd", "path"]).typed else {
        panic!()
    };
    assert_eq!(cwd.path, Some("/u/out".into()));
    valid(&raw(
        "[run.shell]\ninterpreter = ['unknown-tool', '-c']",
        Platform::Linux,
    ));
}

#[test]
fn typed_paths_and_source_rules_on_each_platform() {
    for (path, expected) in [
        ("t.cpp", "/u/t.cpp"),
        ("~/t.cpp", "/home/test/t.cpp"),
        ("/t.cpp", "/t.cpp"),
        ("../t.cpp", "/u/../t.cpp"),
    ] {
        let layer = raw(
            &format!("[languages.cpp.template]\npath = {path:?}"),
            Platform::Linux,
        );
        valid(&layer);
        assert_eq!(
            at(&layer, &["languages", "cpp", "template", "path"]).typed,
            Some(TypedValue::Path(expected.into()))
        );
    }
    for path in [r"C:foo", r"\foo", "/foo"] {
        let layer = raw(
            &format!("[run]\ncwd = {{ path = '{path}' }}"),
            Platform::Windows,
        );
        assert_eq!(
            errors(&layer),
            vec![(
                "run.cwd.path",
                Some(2),
                "Windows drive-relative and root-relative paths are not allowed"
            )]
        );
    }
    for (path, expected) in [
        (r"bin\x", r"C:\u\bin\x"),
        (r"C:\tools\x", r"C:\tools\x"),
        (r"\\host\share\x", r"\\host\share\x"),
    ] {
        let layer = raw(&format!("[run]\nargv = ['{path}']"), Platform::Windows);
        valid(&layer);
        let Some(TypedValue::Text(text)) = &array(at(&layer, &["run", "argv"]), 0).typed else {
            panic!()
        };
        assert_eq!(text.path, Some(expected.into()));
    }
    let home = Environment {
        home: Some(r"C:\Home".into()),
        ..Environment::default()
    };
    let layer = parse(
        b"[run]\ncwd = { path = '~\\out' }",
        &source(LayerKind::Workspace, Platform::Windows),
        &home,
        Platform::Windows,
    );
    valid(&layer);
    let Some(TypedValue::Text(text)) = &at(&layer, &["run", "cwd", "path"]).typed else {
        panic!()
    };
    assert_eq!(text.path, Some(r"C:\Home\out".into()));
    let layer = parse(
        b"[run]\ncwd = { path = '~/x' }",
        &source(LayerKind::Workspace, Platform::Linux),
        &Environment::default(),
        Platform::Linux,
    );
    assert_eq!(
        layer.diagnostics[0].message,
        "home directory is unavailable for ~ expansion"
    );
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let layer = raw("[languages.cpp]\nsources = ['a.cpp', 'A.cpp']", platform);
        if platform == Platform::Linux {
            valid(&layer);
        } else {
            assert_eq!(
                errors(&layer),
                vec![("languages.cpp.sources[1]", Some(2), "duplicate source")]
            );
            assert_eq!(
                layer.diagnostics[0]
                    .second_origin
                    .as_ref()
                    .unwrap()
                    .key_path,
                "languages.cpp.sources[0]"
            );
        }
        for source in ["", "/x", "x/", "a//b", ".", "..", "a/./b", "a/../b", "a\0b"] {
            let text = format!(
                "[languages.cpp]\nsources = [{}]",
                serde_json::to_string(source).unwrap()
            );
            assert_eq!(
                errors(&raw(&text, platform)),
                vec![(
                    "languages.cpp.sources[0]",
                    Some(2),
                    "expected a non-empty problem-relative source with / components, without . or .."
                )],
                "{platform:?} {source:?}"
            );
        }
    }
    for source in [r"a\b", "C:foo", "C:/foo"] {
        let text = format!("[languages.cpp]\nsources = ['{source}']");
        valid(&raw(&text, Platform::Linux));
        assert_eq!(
            errors(&raw(&text, Platform::Windows)),
            vec![(
                "languages.cpp.sources[0]",
                Some(2),
                "expected a non-empty problem-relative source with / components, without . or .."
            )]
        );
    }
    for output in ["../x", "/x", "a/../x", ""] {
        assert_eq!(
            raw(
                &format!("[languages.cpp]\nartifacts = {{ file = '{output}' }}"),
                Platform::Linux
            )
            .diagnostics
            .len(),
            1
        );
    }
    for output in [r"C:\x", r"\x", "C:x", r"a\..\b"] {
        assert_eq!(
            raw(
                &format!("[languages.cpp]\nartifacts = {{ dir = true, required = ['{output}'] }}"),
                Platform::Windows
            )
            .diagnostics
            .len(),
            1
        );
    }
    valid(&raw(
        "[languages.cpp]\nsources = []\nartifacts = { dir = true, required = ['a/b'] }",
        Platform::Linux,
    ));
}

#[test]
fn environment_names_values_and_platform_activity() {
    for (text, key, message) in [
        (
            "[run]\nenv = { CPF_X = '1' }",
            "run.env.CPF_X",
            "environment names starting with CPF_ are reserved",
        ),
        (
            "[run]\nenv_remove = ['CPF_X']",
            "run.env_remove[0]",
            "environment names starting with CPF_ are reserved",
        ),
        (
            "[run]\nenv = { 'A=B' = '1' }",
            "run.env.\"A=B\"",
            "invalid environment name",
        ),
        (
            "[run]\nenv = { X = \"a\\u0000b\" }",
            "run.env.X",
            "environment value contains NUL",
        ),
        (
            "[run]\nenv_remove = [\"a\\u0000b\"]",
            "run.env_remove[0]",
            "invalid environment name",
        ),
    ] {
        for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
            assert_eq!(errors(&raw(text, platform)), vec![(key, Some(2), message)]);
        }
    }
    let text = "[run]\nenv = { path = 'first', PATH = 'second' }";
    for platform in [Platform::Linux, Platform::Macos] {
        valid(&raw(text, platform));
    }
    let layer = raw(text, Platform::Windows);
    assert_eq!(
        errors(&layer),
        vec![(
            "run.env.PATH",
            Some(2),
            "duplicate environment name (case-insensitive on Windows)"
        )]
    );
    assert_eq!(
        layer.diagnostics[0]
            .second_origin
            .as_ref()
            .unwrap()
            .key_path,
        "run.env.path"
    );
    assert!(at(&layer, &["run", "env", "PATH"]).invalid);
    for text in [
        "[run]\nenv = { cpf_X = '1' }",
        "[run]\nenv_remove = ['cpf_X']",
        "[run]\nenv = { '' = '1' }",
    ] {
        valid(&raw(text, Platform::Linux));
        assert_eq!(raw(text, Platform::Windows).diagnostics.len(), 1, "{text}");
    }
    let text = "[run.linux]\nenv = { PATH = '1', path = '2' }\ncwd = { path = 'C:foo' }\n[run.windows]\nenv = { PATH = '1', path = '2' }\ncwd = { path = 'C:foo' }";
    valid(&raw(text, Platform::Linux));
    let layer = raw(text, Platform::Windows);
    assert_eq!(layer.diagnostics.len(), 2);
    assert!(
        layer
            .diagnostics
            .iter()
            .all(|d| d.origin.key_path.starts_with("run.windows."))
    );
    let layer = raw(
        "[languages.cpp.linux]\nsources = ['a', 'A']",
        Platform::Macos,
    );
    valid(&layer);
    // Grammar, numeric validity and unknown keys are checked even when inactive.
    for (body, key, message) in [
        (
            "argv = ['{bad}']",
            "run.windows.argv[0]",
            "unknown placeholder {bad}",
        ),
        (
            "deadline_s = 0",
            "run.windows.deadline_s",
            "rounded duration must be in 1..=86400000 milliseconds",
        ),
        (
            "limits = { job_commit = 0 }",
            "run.windows.limits.job_commit",
            "size must be in 1..=18446744073709551615 bytes",
        ),
        (
            "unknown = 1",
            "run.windows.unknown",
            "unknown configuration key",
        ),
    ] {
        assert_eq!(
            errors(&raw(&format!("[run.windows]\n{body}"), Platform::Linux)),
            vec![(key, Some(2), message)]
        );
    }
    valid(&raw(
        "[run.windows]\nlimits = { job_commit = 1 }",
        Platform::Linux,
    ));
}

#[test]
fn numeric_rounding_ranges_units_and_overflow() {
    for (value, expected) in [
        ("0.0005", 1),
        ("0.0015", 2),
        ("'500ms'", 500),
        ("'2s'", 2000),
        ("'1m'", 60_000),
        ("'2003.5ms'", 2004),
        ("'0.5ms'", 1),
        ("86400", 86_400_000),
        ("86400.0004", 86_400_000),
    ] {
        let layer = raw(&format!("[run]\ndeadline_s = {value}"), Platform::Linux);
        valid(&layer);
        assert_eq!(
            at(&layer, &["run", "deadline_s"]).typed,
            Some(TypedValue::Milliseconds(expected))
        );
    }
    for (value, message) in [
        ("false", "expected a number or duration string"),
        ("'2h'", "duration requires ms, s or m unit"),
        ("'xms'", "invalid duration number"),
        ("nan", "duration must be finite"),
        ("inf", "duration must be finite"),
        ("-inf", "duration must be finite"),
        ("-0.0001", "duration must not be negative"),
        (
            "0.0004",
            "rounded duration must be in 1..=86400000 milliseconds",
        ),
        ("0", "rounded duration must be in 1..=86400000 milliseconds"),
        (
            "86400.0005",
            "rounded duration must be in 1..=86400000 milliseconds",
        ),
        (
            "1e308",
            "rounded duration must be in 1..=86400000 milliseconds",
        ),
    ] {
        assert_eq!(
            errors(&raw(
                &format!("[run]\ndeadline_s = {value}"),
                Platform::Linux
            )),
            vec![("run.deadline_s", Some(2), message)]
        );
    }
    for (unit, multiplier) in [
        ("KiB", 1_u64 << 10),
        ("MiB", 1 << 20),
        ("GiB", 1 << 30),
        ("TiB", 1 << 40),
        ("kB", 1000),
        ("MB", 1_000_000),
        ("GB", 1_000_000_000),
        ("TB", 1_000_000_000_000),
    ] {
        let layer = raw(&format!("[run]\nstdout_cap = '1{unit}'"), Platform::Linux);
        valid(&layer);
        assert_eq!(
            at(&layer, &["run", "stdout_cap"]).typed,
            Some(TypedValue::Bytes(multiplier))
        );
    }
    for (table, key, max) in [
        ("run", "stdout_cap", 1_u64 << 40),
        ("run", "stderr_cap", 1 << 40),
        ("retention", "job_bytes", 1 << 50),
        ("retention", "workspace_bytes", 1 << 50),
        ("retention", "cwd_bytes", 1 << 50),
        ("retention", "build_bytes", 1 << 50),
    ] {
        for value in ["1".to_owned(), max.to_string()] {
            valid(&raw(
                &format!("[{table}]\n{key} = {value}"),
                Platform::Linux,
            ));
        }
        for value in [
            "0".to_owned(),
            (max + 1).to_string(),
            "-1".into(),
            "1.0".into(),
            "'1.5MiB'".into(),
        ] {
            let layer = raw(&format!("[{table}]\n{key} = {value}"), Platform::Linux);
            assert_eq!(layer.diagnostics.len(), 1);
            assert_eq!(
                layer.diagnostics[0].origin.key_path,
                format!("{table}.{key}")
            );
            assert_eq!(layer.diagnostics[0].origin.line, Some(2));
        }
    }
    for key in ["address_space", "job_commit"] {
        let layer = raw(
            &format!("[run.limits]\n{key} = '18014398509481983KiB'"),
            Platform::Linux,
        );
        valid(&layer);
        assert_eq!(
            at(&layer, &["run", "limits", key]).typed,
            Some(TypedValue::Bytes(u64::MAX / 1024 * 1024))
        );
        for (value, message) in [
            ("0", "size must be in 1..=18446744073709551615 bytes"),
            ("-1", "size must not be negative"),
            ("'18014398509481984KiB'", "size overflows u64"),
            ("'18446744073709551616KiB'", "size overflows u64"),
            ("'1.5MiB'", "size must be a whole number of units"),
        ] {
            assert_eq!(
                errors(&raw(
                    &format!("[run.limits]\n{key} = {value}"),
                    Platform::Linux
                )),
                vec![(format!("run.limits.{key}").as_str(), Some(2), message)]
            );
        }
        // No page rounding or platform maximum is applied at load.
        let layer = raw(&format!("[run.limits]\n{key} = 1"), Platform::Linux);
        valid(&layer);
        assert_eq!(
            at(&layer, &["run", "limits", key]).typed,
            Some(TypedValue::Bytes(1))
        );
    }
    for (table, key, max) in [
        ("commands.x", "repeat", 1000),
        ("retention", "keep_jobs", 100_000),
    ] {
        for value in [1, max] {
            valid(&raw(
                &format!("[{table}]\n{key} = {value}"),
                Platform::Linux,
            ));
        }
        for value in [
            "0".into(),
            "-1".into(),
            (max + 1).to_string(),
            "1.0".into(),
            "'1'".into(),
        ] {
            let message = if value == "1.0" || value == "'1'" {
                "expected an integer".to_owned()
            } else {
                format!("integer must be in 1..={max}")
            };
            assert_eq!(
                errors(&raw(
                    &format!("[{table}]\n{key} = {value}"),
                    Platform::Linux
                )),
                vec![(format!("{table}.{key}").as_str(), Some(2), message.as_str())]
            );
        }
    }
    let layer = raw("[run]\nstdout_cap = '1.5MiB'", Platform::Linux);
    assert_eq!(
        errors(&layer),
        vec![(
            "run.stdout_cap",
            Some(2),
            "size must be a whole number of units"
        )]
    );
}

#[test]
fn ignore_pattern_syntax() {
    for pattern in [
        "a[b]", "a]", "!a", r"a\b", "", "/a", "a/../b", "a//b", ".", "..", "a/./b", "ab**", "**b",
        "a/", "\0",
    ] {
        let quoted = serde_json::to_string(pattern).unwrap();
        let layer = raw(
            &format!("[workspace]\nignore = [{quoted}]"),
            Platform::Linux,
        );
        assert_eq!(
            errors(&layer),
            vec![(
                "workspace.ignore[0]",
                Some(2),
                "invalid workspace ignore pattern"
            )],
            "{pattern:?}"
        );
    }
    for pattern in [
        "*.swp",
        "scratch/**",
        "**",
        "x/**/a.o",
        "?.txt",
        "é.txt",
        "a*b?c",
        "x/*/a",
    ] {
        valid(&raw(
            &format!("[workspace]\nignore = ['{pattern}']"),
            Platform::Linux,
        ));
    }
}

#[test]
fn compare_configuration_rows_and_expanded_values() {
    for (text, abs, rel, ulps, base, combine, int_exact) in [
        (
            "{ float = 1 }",
            Some(1.0),
            Some(1.0),
            None,
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float = 1e-6 }",
            Some(1e-6),
            Some(1e-6),
            None,
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float_abs = 1e-6 }",
            Some(1e-6),
            None,
            None,
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float_rel = 1e-6 }",
            None,
            Some(1e-6),
            None,
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float = { abs = -1e-400 } }",
            Some(0.0),
            None,
            None,
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float = { abs = -0.0 } }",
            Some(0.0),
            None,
            None,
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float = { abs = 1e-400 } }",
            Some(0.0),
            None,
            None,
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float = { ulps = 9007199254740993 } }",
            None,
            None,
            Some(9_007_199_254_740_993),
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float = { ulps = 9223372036854775807 } }",
            None,
            None,
            Some(i64::MAX as u64),
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float = { ulps = 0 } }",
            None,
            None,
            Some(0),
            RelativeBase::Answer,
            Combine::Either,
            true,
        ),
        (
            "{ float = { abs = 1, rel = 0, rel_base = 'max', combine = 'both', int_exact = false } }",
            Some(1.0),
            Some(0.0),
            None,
            RelativeBase::Max,
            Combine::Both,
            false,
        ),
        (
            "{ float = { rel = 1e-6, rel_base = 'max1' } }",
            None,
            Some(1e-6),
            None,
            RelativeBase::Max1,
            Combine::Either,
            true,
        ),
    ] {
        let layer = raw(&format!("compare = {text}"), Platform::Linux);
        valid(&layer);
        assert_eq!(
            at(&layer, &["compare"]).typed,
            Some(TypedValue::Compare(Comparator::Float(FloatComparator {
                abs,
                rel,
                ulps,
                rel_base: base,
                combine,
                int_exact
            })))
        );
        if abs == Some(0.0) {
            let Some(TypedValue::Compare(Comparator::Float(f))) = &at(&layer, &["compare"]).typed
            else {
                panic!()
            };
            assert_eq!(f.abs.unwrap().to_bits(), 0);
        }
    }
    for (text, key, message) in [
        (
            "{ float_abs = 1e-6, float_rel = 1e-6 }",
            "compare.float_abs",
            "float_abs and float_rel are mutually exclusive",
        ),
        (
            "{ float = { rel_base = 'max', abs = 1 } }",
            "compare.float.rel_base",
            "rel_base requires rel",
        ),
        (
            "{ float = { abs = -1e-6 } }",
            "compare.float.abs",
            "float bound must not be negative",
        ),
        (
            "{ float = { ulps = 1, abs = 1e-6 } }",
            "compare.float.ulps",
            "ulps and abs are mutually exclusive",
        ),
        (
            "{ float = { combine = 'either', abs = 1e-6 } }",
            "compare.float.combine",
            "combine requires both abs and rel",
        ),
        (
            "{ float = {} }",
            "compare.float",
            "float comparator requires abs, rel or ulps",
        ),
        (
            "{ float = { abs = inf } }",
            "compare.float.abs",
            "float bound must be finite",
        ),
        (
            "{ float = { abs = nan } }",
            "compare.float.abs",
            "float bound must be finite",
        ),
        (
            "{ float_abs = '1e-6' }",
            "compare.float_abs",
            "float bound must be a TOML float or integer",
        ),
        (
            "{ float = { ulps = -1 } }",
            "compare.float.ulps",
            "integer must be in 0..=9223372036854775807",
        ),
        (
            "{ float = { ulps = 1.0 } }",
            "compare.float.ulps",
            "expected an integer",
        ),
        (
            "{ float = { abs = 1, int_exact = 1 } }",
            "compare.float.int_exact",
            "expected a boolean",
        ),
        (
            "{ float = { rel = 1, rel_base = 'other' } }",
            "compare.float.rel_base",
            "expected one of answer, max, max1",
        ),
        (
            "{ float = { abs = 1, rel = 1, combine = 'other' } }",
            "compare.float.combine",
            "expected one of either, both",
        ),
        (
            "{ float = { abs = 1, unknown = 1 } }",
            "compare.float.unknown",
            "unknown configuration key",
        ),
        (
            "{ float_abs = 1, int_exact = false }",
            "compare.int_exact",
            "unknown configuration key",
        ),
    ] {
        let layer = raw(&format!("compare = {text}"), Platform::Linux);
        assert_eq!(errors(&layer), vec![(key, Some(1), message)], "{text}");
        assert!(at(&layer, &["compare"]).typed.is_none());
    }
    for (name, expected) in [
        ("exact", Comparator::Exact),
        ("tokens", Comparator::Tokens),
        ("lines", Comparator::Lines),
    ] {
        let layer = raw(&format!("compare = {{ {name} = true }}"), Platform::Linux);
        valid(&layer);
        assert_eq!(
            at(&layer, &["compare"]).typed,
            Some(TypedValue::Compare(expected.clone()))
        );
        // No string shorthand.
        assert!(
            !raw(&format!("compare = '{name}'"), Platform::Linux)
                .diagnostics
                .is_empty()
        );
    }
    for value in [
        "{}",
        "{ tokens = false }",
        "{ unknown = 1 }",
        "false",
        "{ float = { ulps = 1, rel_base = 'answer' } }",
        "{ float = { ulps = 1, combine = 'either' } }",
    ] {
        assert!(
            !raw(&format!("compare = {value}"), Platform::Linux)
                .diagnostics
                .is_empty(),
            "{value}"
        );
    }
}

#[test]
fn discovery_is_injected_and_override_replaces_default() {
    let options = DiscoveryOptions::default();
    for platform in [Platform::Linux, Platform::Macos] {
        for (xdg, home, expected) in [
            (Some("/xdg"), Some("/home/u"), "/xdg/cpforge/config.toml"),
            (
                Some("relative"),
                Some("/home/u"),
                "/home/u/.config/cpforge/config.toml",
            ),
            (
                Some(""),
                Some("/home/u"),
                "/home/u/.config/cpforge/config.toml",
            ),
            (None, Some("/home/u"), "/home/u/.config/cpforge/config.toml"),
            (Some("/xdg"), None, "/xdg/cpforge/config.toml"),
        ] {
            let env = Environment {
                home: home.map(PathBuf::from),
                xdg_config_home: xdg.map(Into::into),
                appdata: None,
            };
            assert_eq!(
                discover(&env, platform, &options).unwrap(),
                vec![LayerSource {
                    kind: LayerKind::DefaultUser,
                    file: expected.into()
                }]
            );
        }
        for (home, xdg) in [
            (None, None),
            (None, Some("relative")),
            (Some("relative"), None),
        ] {
            let env = Environment {
                home: home.map(Into::into),
                xdg_config_home: xdg.map(Into::into),
                appdata: None,
            };
            assert_eq!(
                discover(&env, platform, &options).unwrap_err().to_string(),
                "cannot discover user configuration: no absolute XDG_CONFIG_HOME or HOME"
            );
        }
    }
    for appdata in [
        None,
        Some(""),
        Some("relative"),
        Some(r"C:relative"),
        Some(r"\rooted"),
    ] {
        let env = Environment {
            appdata: appdata.map(Into::into),
            ..env()
        };
        assert_eq!(
            discover(&env, Platform::Windows, &options)
                .unwrap_err()
                .to_string(),
            "cannot discover user configuration: APPDATA must be non-empty and absolute"
        );
    }
    for base in [r"C:\Users\u\AppData\Roaming", r"\\host\share"] {
        let env = Environment {
            appdata: Some(base.into()),
            ..Environment::default()
        };
        assert_eq!(
            discover(&env, Platform::Windows, &options).unwrap()[0].file,
            PathBuf::from(format!(r"{base}\cpforge\config.toml"))
        );
    }
    let options = DiscoveryOptions {
        config_override: Some("override.toml".into()),
        workspace_root: Some("/ws".into()),
        problem_file: Some("/ws/problems/p/problem.toml".into()),
        current_dir: Some("/c".into()),
    };
    let discovered = discover(&Environment::default(), Platform::Linux, &options).unwrap();
    assert_eq!(
        discovered,
        vec![
            LayerSource {
                kind: LayerKind::OverrideUser,
                file: "/c/override.toml".into()
            },
            LayerSource {
                kind: LayerKind::Workspace,
                file: "/ws/cpforge.toml".into()
            },
            LayerSource {
                kind: LayerKind::Problem,
                file: "/ws/problems/p/problem.toml".into()
            }
        ]
    );
    let options = DiscoveryOptions {
        workspace_root: None,
        ..options
    };
    assert_eq!(
        discover(&Environment::default(), Platform::Windows, &options)
            .unwrap()
            .len(),
        1
    );
    let options = DiscoveryOptions {
        current_dir: None,
        ..options
    };
    assert!(discover(&Environment::default(), Platform::Linux, &options).is_err());
    let options = DiscoveryOptions {
        config_override: Some("/abs/o.toml".into()),
        ..options
    };
    assert_eq!(
        discover(&Environment::default(), Platform::Linux, &options).unwrap()[0].file,
        PathBuf::from("/abs/o.toml")
    );
}

#[derive(Debug)]
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Keep fixture writes in the worktree, including on Windows.
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/config-raw-fixtures");
        std::fs::create_dir_all(&parent).unwrap();
        let directory = parent.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn source(&self, name: &str, kind: LayerKind) -> LayerSource {
        LayerSource {
            file: self.0.join(name),
            kind,
        }
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.join(name), text).unwrap();
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn independent_files_recovery_classification_and_display() {
    let dir = Directory::new();
    let user = dir.source("user.toml", LayerKind::DefaultUser);
    let workspace = dir.source("cpforge.toml", LayerKind::Workspace);
    let layers = load(std::slice::from_ref(&user), &env(), Platform::Linux);
    assert_eq!(layers[0].state, LayerState::Empty);
    valid(&layers[0]);
    let required = dir.source("missing.toml", LayerKind::OverrideUser);
    let layers = load(&[required], &env(), Platform::Linux);
    assert!(matches!(layers[0].state, LayerState::Invalid(_)));
    assert_eq!(
        errors(&layers[0]),
        vec![("", None, "configuration file is absent")]
    );
    dir.write("user.toml", "schema = 2\nunknown = false");
    let layers = load(std::slice::from_ref(&user), &env(), Platform::Linux);
    assert!(matches!(layers[0].state, LayerState::Invalid(_)));
    assert_eq!(
        errors(&layers[0]),
        vec![("schema", Some(1), "schema 2 is newer than supported 1")]
    );
    dir.write("user.toml", "schema = 1\nbad = ]");
    dir.write(
        "cpforge.toml",
        "schema = 1\nunknown = 1\n[run]\ndeadline_s = false",
    );
    let layers = load(&[user.clone(), workspace.clone()], &env(), Platform::Linux);
    assert_eq!(layers[0].diagnostics.len(), 1);
    assert_eq!(layers[0].diagnostics[0].origin.line, Some(2));
    assert_eq!(layers[0].diagnostics[0].origin.col, Some(7));
    assert!(matches!(layers[0].state, LayerState::Invalid(_)));
    assert_eq!(
        errors(&layers[1]),
        vec![
            ("unknown", Some(2), "unknown configuration key"),
            (
                "run.deadline_s",
                Some(4),
                "expected a number or duration string"
            )
        ]
    );
    assert!(at(&layers[1], &["run", "deadline_s"]).invalid);
    dir.write("user.toml", "[run]\ndeadline_s = 'wrong'");
    dir.write("cpforge.toml", "[run]\ndeadline_s = 1");
    let layers = load(&[user.clone(), workspace.clone()], &env(), Platform::Linux);
    assert_eq!(
        errors(&layers[0]),
        vec![(
            "run.deadline_s",
            Some(2),
            "duration requires ms, s or m unit"
        )]
    );
    valid(&layers[1]);
    dir.write("cpforge.toml", "schema = 1");
    valid(&load(std::slice::from_ref(&workspace), &env(), Platform::Linux)[0]);
    // A directory is a portable unreadable-file fixture, independent of uid.
    let unreadable = dir.source("directory.toml", LayerKind::DefaultUser);
    std::fs::create_dir(&unreadable.file).unwrap();
    let layers = load(&[unreadable], &env(), Platform::Linux);
    assert_eq!(layers[0].diagnostics.len(), 1);
    assert!(
        layers[0].diagnostics[0]
            .message
            .starts_with("cannot read configuration:")
    );
    assert_eq!(layers[0].diagnostics[0].origin.line, None);
    assert_eq!(layers[0].diagnostics[0].origin.col, None);
    assert!(matches!(layers[0].state, LayerState::Invalid(_)));
    for bytes in [b"\xff".as_slice(), b"schema = '1'", b"schema = 0"] {
        let layer = parse(bytes, &user, &env(), Platform::Linux);
        assert_eq!(layer.diagnostics.len(), 1);
        assert!(matches!(layer.state, LayerState::Invalid(_)));
    }
    let layer = raw("[run]\nstdin = 'test'", Platform::Linux);
    assert_eq!(
        layer.diagnostics[0].to_string(),
        "/u/cfg.toml:2:1 [run.stdin]: error: stdin is allowed only in commands"
    );
    let mut diagnostic = layer.diagnostics[0].clone();
    diagnostic.severity = Severity::Warning;
    diagnostic.second_origin = Some(Origin {
        file: "other.toml".into(),
        line: None,
        col: None,
        key_path: "run.argv".into(),
        span: None,
    });
    assert_eq!(
        diagnostic.to_string(),
        "/u/cfg.toml:2:1 [run.stdin]: warning: stdin is allowed only in commands (conflicts with other.toml [run.argv])"
    );
}

#[test]
fn problem_config_isolated_and_empty_is_distinct_from_invalid() {
    let problem = source(LayerKind::Problem, Platform::Linux);
    for text in [
        "[problem]\nid = 'local:p'",
        "schema = 1\n[problem]\nid = 'local:p'\n[future]\ncommands = false",
    ] {
        let layer = parse(text.as_bytes(), &problem, &env(), Platform::Linux);
        valid(&layer);
        assert_eq!(layer.state, LayerState::Empty);
    }
    let layer = parse(b"[problem]\nid = 'local:p'\n[config.run]\nargv = ['x']\n[config.run.windows]\nargv = ['y']", &problem, &env(), Platform::Linux);
    valid(&layer);
    assert_eq!(root(&layer).origin.key_path, "config");
    assert_eq!(
        at(&layer, &["run", "argv"]).origin.key_path,
        "config.run.argv"
    );
    assert_eq!(at(&layer, &["run", "argv"]).origin.line, Some(4));
    assert_eq!(array(at(&layer, &["run", "argv"]), 0).origin.col, Some(9));
    let layer = parse(
        b"[problem]\nid = 'local:p'\nconfig = false",
        &problem,
        &env(),
        Platform::Linux,
    );
    // This is problem.config, not document-root config, and is outside our layer.
    valid(&layer);
    assert_eq!(layer.state, LayerState::Empty);
    let layer = parse(
        b"config = false\n[problem]\nid = 'local:p'",
        &problem,
        &env(),
        Platform::Linux,
    );
    assert_eq!(
        errors(&layer),
        vec![("config", Some(1), "expected a table")]
    );
    assert!(root(&layer).invalid);
    let layer = parse(
        b"[problem]\nid = 'remote:p'",
        &problem,
        &env(),
        Platform::Linux,
    );
    assert!(matches!(layer.state, LayerState::Invalid(_)));
    assert_eq!(
        errors(&layer),
        vec![(
            "problem.id",
            Some(2),
            "problem.id must be a valid problem identity"
        )]
    );
}

#[test]
fn explicit_override_never_reads_default_file() {
    let dir = Directory::new();
    std::fs::create_dir(dir.0.join("cpforge")).unwrap();
    std::fs::write(dir.0.join("cpforge/config.toml"), "not TOML = ]").unwrap();
    dir.write("override.toml", "schema = 1");
    let env = Environment {
        xdg_config_home: Some(dir.0.as_os_str().to_owned()),
        ..Environment::default()
    };
    let options = DiscoveryOptions {
        config_override: Some(dir.0.join("override.toml")),
        ..DiscoveryOptions::default()
    };
    // Real files: use the host's path rules.
    let sources = discover(&env, Platform::host(), &options).unwrap();
    assert_eq!(sources.len(), 1);
    let layers = load(&sources, &env, Platform::host());
    valid(&layers[0]);
    assert_eq!(layers[0].kind, LayerKind::OverrideUser);
}

#[test]
fn inactive_paths_and_native_home_do_not_change_grammar_checks() {
    let no_home = Environment::default();
    let source = source(LayerKind::Workspace, Platform::Linux);
    let text = r"[languages.cpp.windows]
template = { path = '~/template.cpp' }
[languages.cpp.windows.run]
argv = ['C:\tools\{problem_id}\tool']
cwd = { path = 'C:\out\{iteration}' }
";
    let layer = parse(text.as_bytes(), &source, &no_home, Platform::Linux);
    valid(&layer);
    assert!(
        at(&layer, &["languages", "cpp", "windows", "template", "path"])
            .typed
            .is_none()
    );
    let Some(TypedValue::Text(text)) = &array(
        at(&layer, &["languages", "cpp", "windows", "run", "argv"]),
        0,
    )
    .typed
    else {
        panic!()
    };
    assert!(text.path.is_none());
    assert!(
        text.tokens
            .contains(&Token::Placeholder(Placeholder::ProblemId))
    );
    let layer = raw("[run]\nargv = ['bin/{{literal}}']", Platform::Linux);
    valid(&layer);
    let Some(TypedValue::Text(text)) = &array(at(&layer, &["run", "argv"]), 0).typed else {
        panic!()
    };
    assert_eq!(text.path, Some("/u/bin/{literal}".into()));
    let layer = raw("[run]\nargv = ['~/{problem_id}/tool']", Platform::Linux);
    valid(&layer);
    let Some(TypedValue::Text(text)) = &array(at(&layer, &["run", "argv"]), 0).typed else {
        panic!()
    };
    assert_eq!(text.expanded_home, env().home);
    assert_eq!(text.path, None);
}

#[cfg(unix)]
#[test]
fn native_non_unicode_directories_are_preserved() {
    use std::os::unix::ffi::OsStringExt;
    let home = PathBuf::from(std::ffi::OsString::from_vec(b"/home/u\xff".to_vec()));
    let env = Environment {
        home: Some(home.clone()),
        ..Environment::default()
    };
    let paths = discover(&env, Platform::Linux, &DiscoveryOptions::default()).unwrap();
    assert_eq!(paths[0].file, home.join(".config/cpforge/config.toml"));
    let source = LayerSource {
        kind: LayerKind::Workspace,
        file: home.join("cfg.toml"),
    };
    let layer = parse(
        b"[run]\nargv = ['bin/x']\ncwd = { path = '~/out' }",
        &source,
        &env,
        Platform::Linux,
    );
    valid(&layer);
    let Some(TypedValue::Text(program)) = &array(at(&layer, &["run", "argv"]), 0).typed else {
        panic!()
    };
    assert_eq!(program.path, Some(home.join("bin/x")));
    let Some(TypedValue::Text(cwd)) = &at(&layer, &["run", "cwd", "path"]).typed else {
        panic!()
    };
    assert_eq!(cwd.path, Some(home.join("out")));
}

#[test]
fn review_cases_cwd_interpreter_overlay_roles_and_key_quoting() {
    // Rule (3) applies to cwd.path; a placeholder-led cwd is not rebased.
    assert_eq!(
        errors(&raw(
            "[run]\ncwd = { path = 'out/{problem_id}' }",
            Platform::Linux
        )),
        vec![(
            "run.cwd.path",
            Some(2),
            "relative path with a placeholder must start with a placeholder"
        )]
    );
    let layer = raw("[run]\ncwd = { path = '{workspace}/out' }", Platform::Linux);
    valid(&layer);
    let Some(TypedValue::Text(parsed)) = &at(&layer, &["run", "cwd", "path"]).typed else {
        panic!()
    };
    assert_eq!(parsed.path, None);

    // The interpreter's program is rebased like argv[0]; a bare name is a lookup.
    for (program, expected, lookup) in [
        ("bin/sh", Some("/u/bin/sh"), false),
        ("sh", None, true),
        ("~/bin/bash", Some("/home/test/bin/bash"), false),
    ] {
        let layer = raw(
            &format!(
                "[commands.c]\nshell = {{ interpreter = [{program:?}, '-c'], script = 'true' }}"
            ),
            Platform::Linux,
        );
        valid(&layer);
        let interpreter = at(&layer, &["commands", "c", "shell", "interpreter"]);
        let Some(TypedValue::Text(parsed)) = &array(interpreter, 0).typed else {
            panic!("{program}: {:?}", array(interpreter, 0))
        };
        assert_eq!(
            parsed.path.as_ref().map(|p| p.as_os_str()),
            expected.map(std::ffi::OsStr::new),
            "{program}"
        );
        assert_eq!(parsed.lookup, lookup, "{program}");
    }

    // Overlay roles: a site overlay, a forbidden nested one, and ordinary names.
    let layer = raw(
        "[run.linux]\ndeadline_s = 1\n[run.unix.linux]\ndeadline_s = 2\n\
         [commands.linux]\nargv = ['x']\n[run.env]\nlinux = '1'",
        Platform::Linux,
    );
    assert_eq!(layer.diagnostics.len(), 1, "{:?}", layer.diagnostics);
    assert_eq!(
        at(&layer, &["run", "linux"]).overlay,
        Some(OverlayRole::Site)
    );
    assert_eq!(
        at(&layer, &["run", "unix"]).overlay,
        Some(OverlayRole::Site)
    );
    assert_eq!(
        at(&layer, &["run", "unix", "linux"]).overlay,
        Some(OverlayRole::Forbidden)
    );
    assert_eq!(at(&layer, &["commands", "linux"]).overlay, None);
    assert_eq!(at(&layer, &["run", "env", "linux"]).overlay, None);

    // Key paths use TOML key syntax.
    let layer = raw("[run.env]\n'A.B' = 1", Platform::Linux);
    assert_eq!(layer.diagnostics.len(), 1, "{:?}", layer.diagnostics);
    assert_eq!(layer.diagnostics[0].origin.key_path, "run.env.\"A.B\"");
}

#[test]
fn review_template_trust_and_problem_overlay_diagnostics() {
    for path in [r"C:foo", r"\foo", "/foo"] {
        assert_eq!(
            errors(&raw(
                &format!("[languages.cpp]\ntemplate = {{ path = '{path}' }}"),
                Platform::Windows
            )),
            vec![(
                "languages.cpp.template.path",
                Some(2),
                "Windows drive-relative and root-relative paths are not allowed"
            )]
        );
    }
    let layer = parse(
        b"[hooks]\ntrusted_workspaces = ['rel']",
        &source(LayerKind::DefaultUser, Platform::Linux),
        &env(),
        Platform::Linux,
    );
    assert_eq!(
        errors(&layer),
        vec![(
            "hooks.trusted_workspaces[0]",
            Some(2),
            "trusted workspace path must be absolute"
        )]
    );
    let layer = parse(
        b"[problem]\nid = 'local:p'\n[config.run.windows.linux]\nargv = ['x']",
        &source(LayerKind::Problem, Platform::Linux),
        &env(),
        Platform::Linux,
    );
    assert_eq!(
        errors(&layer),
        vec![(
            "config.run.windows.linux",
            Some(3),
            "platform overlays cannot be nested or used at this site"
        )]
    );
}

#[test]
fn windows_non_ascii_environment_duplicates_name_both_origins() {
    let layer = raw("[run]\nenv = { 'Ä' = '1', 'ä' = '2' }", Platform::Windows);
    assert_eq!(layer.diagnostics.len(), 1);
    let error = &layer.diagnostics[0];
    assert_eq!(
        error.message,
        "duplicate environment name (case-insensitive on Windows)"
    );
    assert!(error.origin.key_path.contains('ä'));
    assert!(error.second_origin.as_ref().unwrap().key_path.contains('Ä'));
}
