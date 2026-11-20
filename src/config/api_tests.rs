use std::fs;
use std::path::Path;

use super::*;
use crate::config::{EventKind, Placeholder};

#[derive(Debug)]
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        // Keep all fixture writes within this checkout, including on CI.
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("config-api-{:032x}", u128::from_le_bytes(bits)));
        fs::create_dir_all(&path).unwrap();
        let directory = Self(path);
        directory.write("user/cpforge/config.toml", "schema = 1\n");
        directory.write("ws/cpforge.toml", "schema = 1\n");
        directory
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn user(&self, text: &str) {
        self.write("user/cpforge/config.toml", text);
    }

    fn workspace(&self, text: &str) {
        self.write("ws/cpforge.toml", text);
    }

    fn problem(&self, config: &str) {
        self.write("ws/problems/sample/problem.toml", &problem_text(config));
    }

    fn request(&self, problem: bool) -> ConfigRequest {
        ConfigRequest {
            environment: Environment {
                home: Some(self.path("home")),
                xdg_config_home: Some(self.path("user").into_os_string()),
                appdata: Some(self.path("user").into_os_string()),
            },
            platform: Platform::host(),
            current_dir: self.0.clone(),
            workspace_root: Some(self.path("ws")),
            problem_file: problem.then(|| self.path("ws/problems/sample/problem.toml")),
            ..ConfigRequest::default()
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn problem_text(config: &str) -> String {
    format!("schema = 1\n[problem]\nid = 'local:sample'\n{config}")
}

fn clean(loaded: &LoadedConfig) {
    assert!(
        loaded.diagnostics().is_empty(),
        "{:?}",
        loaded.diagnostics()
    );
    assert!(!loaded.has_errors());
}

fn expected_origin(file: &Path, document: &str, needle: &str, key: &str) -> Origin {
    let offset = document.find(needle).unwrap();
    let before = &document[..offset];
    Origin {
        file: file.to_owned(),
        line: Some(before.bytes().filter(|byte| *byte == b'\n').count() + 1),
        col: Some(before.rsplit('\n').next().unwrap().len() + 1),
        key_path: key.into(),
        span: Some(offset..offset + needle.len()),
    }
}

fn file_value<T>(value: &Sourced<T>, file: &Path, document: &str, needle: &str, key: &str) {
    let origin = expected_origin(file, document, needle, key);
    assert_eq!(value.origin, origin);
    assert_eq!(
        value.provenance,
        Provenance::File {
            path: file.to_owned(),
            line: origin.line,
            col: origin.col,
        }
    );
    assert_eq!(value.overlay, None);
}

fn argv(stage: &StagePlan) -> &[TextValue] {
    let StageCommand::Argv(values) = &stage.command else {
        panic!("expected argv");
    };
    values
}

#[test]
fn template_destination_uses_defining_file_and_effective_solution_files() {
    let dir = Directory::new();
    let user = "default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n";
    dir.user(user);
    let loaded = load_config(&dir.request(false));
    clean(&loaded);
    let plan = loaded.target().unwrap();
    let (template, source) = plan.template_destination().unwrap().unwrap();
    assert_eq!(template.value, dir.path("user/cpforge/t.cpp"));
    file_value(
        template,
        &dir.path("user/cpforge/config.toml"),
        user,
        "path",
        "languages.cpp.template.path",
    );
    assert_eq!(source.value.components(), ["main.cpp"]);
    assert_eq!(
        source.provenance,
        Provenance::Derived {
            inputs: vec![Provenance::BuiltIn]
        }
    );
    assert_eq!(source.origin.key_path, "languages.cpp.sources[0]");
    assert!(!template.value.exists());

    for (roles, expected) in [
        ("sources = ['a.cpp', 'b.cpp']", 2),
        (
            "sources = ['a.cpp', 'b.cpp']\nsolution_files = ['b.cpp', 'a.cpp']",
            2,
        ),
        ("sources = []", 0),
        ("solution_files = []", 0),
    ] {
        dir.problem(&format!("[config]\n{roles}\n"));
        let loaded = load_config(&dir.request(true));
        clean(&loaded);
        let plan = loaded.target().unwrap();
        let error = plan.template_destination().unwrap_err();
        assert_eq!(error.solution_file_count, expected);
        assert_eq!(error.template_origin, template.origin);
        assert_eq!(
            error.to_string(),
            format!(
                "{}: template requires exactly one solution file, found {expected} solution files",
                template.origin,
            )
        );
    }
    dir.problem("[config]\nsources = ['src/main.cpp']\n");
    let plan = load_config(&dir.request(true)).target().unwrap();
    assert_eq!(plan.sources[0].value.components(), ["src", "main.cpp"]);
    file_value(
        &plan.sources[0],
        &dir.path("ws/problems/sample/problem.toml"),
        &problem_text("[config]\nsources = ['src/main.cpp']\n"),
        "'src/main.cpp'",
        "config.sources[0]",
    );

    dir.user("default_language = 'cpp'\n");
    dir.problem("[config]\nsources = []\n");
    let plan = load_config(&dir.request(true)).target().unwrap();
    assert_eq!(plan.template_destination(), Ok(None));
}

#[test]
fn cpp_grader_roles_keep_origins_and_only_sources_expand_as_tool_inputs() {
    use crate::exec::{
        Baseline, Context, InvocationDirs, PreparedCommand, ProblemBinding, ProgramRequest,
        TargetBindings, prepare,
    };

    let dir = Directory::new();
    dir.user("default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n");
    let config = "[config]\nsources = [\"solution.cpp\",\"grader.cpp\"]\nsupport_files = [\"grader.h\"]\nsolution_files = ['solution.cpp']\n";
    dir.problem(config);
    let loaded = load_config(&dir.request(true));
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert_eq!(plan.sources.len(), 2);
    assert_eq!(plan.sources[0].value.components(), ["solution.cpp"]);
    assert_eq!(plan.sources[1].value.components(), ["grader.cpp"]);
    assert_eq!(plan.support_files.len(), 1);
    assert_eq!(plan.support_files[0].value.components(), ["grader.h"]);
    assert_eq!(plan.solution_files.len(), 1);
    assert_eq!(plan.solution_files[0].value.components(), ["solution.cpp"]);
    for (file, needle, key) in [
        (&plan.sources[0], "\"solution.cpp\"", "config.sources[0]"),
        (&plan.sources[1], "\"grader.cpp\"", "config.sources[1]"),
        (
            &plan.support_files[0],
            "\"grader.h\"",
            "config.support_files[0]",
        ),
        (
            &plan.solution_files[0],
            "'solution.cpp'",
            "config.solution_files[0]",
        ),
    ] {
        file_value(
            file,
            &dir.path("ws/problems/sample/problem.toml"),
            &problem_text(config),
            needle,
            key,
        );
    }
    assert_eq!(
        plan.captured_files()
            .map(|file| file.value.components().join("/"))
            .collect::<Vec<String>>(),
        ["solution.cpp", "grader.cpp", "grader.h"]
    );
    let (template, destination) = plan.template_destination().unwrap().unwrap();
    assert_eq!(template.value, dir.path("user/cpforge/t.cpp"));
    assert_eq!(destination, &plan.solution_files[0]);
    let build = plan.build.as_ref().unwrap();
    assert_eq!(
        argv(build)
            .iter()
            .map(|value| value.tokens.clone())
            .collect::<Vec<Vec<Token>>>(),
        vec![
            vec![Token::Literal("g++".into())],
            vec![Token::Literal("-std=gnu++23".into())],
            vec![Token::Literal("-O2".into())],
            vec![Token::Literal("-Wall".into())],
            vec![Token::Placeholder(Placeholder::Sources)],
            vec![Token::Literal("-o".into())],
            vec![Token::Placeholder(Placeholder::Artifact)],
        ]
    );
    let sources: Vec<PathBuf> = ["solution.cpp", "grader.cpp"]
        .iter()
        .map(|name| dir.path(&format!("job/inputs/src/{name}")))
        .collect();
    let artifact = dir.path(if cfg!(windows) {
        "job/out/main.exe"
    } else {
        "job/out/main"
    });
    let context = Context::target_build(
        &plan,
        InvocationDirs {
            run_cwd: dir.path("job/cwd"),
            problem: Some(ProblemBinding {
                dir: dir.path("ws/problems/sample"),
                id: "local:sample".into(),
            }),
            workspace: Some(dir.path("ws")),
        },
        TargetBindings {
            sources: sources.clone(),
            artifact: Some(artifact.clone()),
            artifact_dir: Some(dir.path("job/out")),
            entry: None,
        },
    )
    .unwrap();
    let prepared = prepare(build, &context, &Baseline::default(), Platform::host(), &[]).unwrap();
    let mut expected: Vec<std::ffi::OsString> = ["-std=gnu++23", "-O2", "-Wall"]
        .iter()
        .map(|value: &&str| std::ffi::OsString::from(*value))
        .collect();
    expected.extend(sources.into_iter().map(PathBuf::into_os_string));
    expected.push("-o".into());
    expected.push(artifact.into_os_string());
    assert_eq!(
        prepared.command,
        PreparedCommand::Argv {
            program: ProgramRequest::Lookup("g++".into()),
            args: expected
        }
    );
}

#[test]
fn python_grader_uses_no_bytecode_flag_and_templates_the_support_solution() {
    let dir = Directory::new();
    dir.user("[languages.python]\ntemplate = { path = 't.py' }\n");
    let config = "[config]\nlanguage = 'python'\nsources = ['grader.py']\nsupport_files = [\"solution.py\"]\nsolution_files = ['solution.py']\n";
    dir.problem(config);
    let loaded = load_config(&dir.request(true));
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert!(plan.build.is_none());
    assert_eq!(plan.sources[0].value.components(), ["grader.py"]);
    assert_eq!(plan.support_files[0].value.components(), ["solution.py"]);
    assert_eq!(plan.solution_files[0].value.components(), ["solution.py"]);
    for (file, needle, key) in [
        (&plan.sources[0], "'grader.py'", "config.sources[0]"),
        (
            &plan.support_files[0],
            "\"solution.py\"",
            "config.support_files[0]",
        ),
        (
            &plan.solution_files[0],
            "'solution.py'",
            "config.solution_files[0]",
        ),
    ] {
        file_value(
            file,
            &dir.path("ws/problems/sample/problem.toml"),
            &problem_text(config),
            needle,
            key,
        );
    }
    assert_eq!(
        argv(&plan.run)
            .iter()
            .map(|value| value.tokens.clone())
            .collect::<Vec<Vec<Token>>>(),
        vec![
            vec![Token::Literal(
                if cfg!(windows) { "python" } else { "python3" }.into()
            )],
            vec![Token::Literal("-B".into())],
            vec![Token::Placeholder(Placeholder::Source)],
        ]
    );
    let (template, destination) = plan.template_destination().unwrap().unwrap();
    assert_eq!(template.value, dir.path("user/cpforge/t.py"));
    assert_eq!(destination, &plan.solution_files[0]);
}

#[test]
fn problem_java_entry_supplies_the_effective_entry_and_required_origin() {
    let dir = Directory::new();
    for (name, expected) in [
        ("Grader", "Grader.class"),
        ("judge.Grader", "judge/Grader.class"),
    ] {
        let config = format!(
            "[config]\nlanguage = 'java'\nsources = ['Solution.java','Grader.java']\nentry = '{name}'\n"
        );
        dir.problem(&config);
        let loaded = load_config(&dir.request(true));
        clean(&loaded);
        let plan = loaded.target().unwrap();
        let entry = plan.entry.as_ref().unwrap();
        assert_eq!(entry.value, name);
        file_value(
            entry,
            &dir.path("ws/problems/sample/problem.toml"),
            &problem_text(&config),
            "entry",
            "config.entry",
        );
        let Some(Artifacts::Dir { required }) = &plan.artifacts else {
            panic!("directory artifact")
        };
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].value, expected);
        assert_eq!(required[0].origin, entry.origin);
        assert_eq!(
            required[0].provenance,
            Provenance::Derived {
                inputs: vec![entry.provenance.clone()]
            }
        );
        assert_eq!(required[0].overlay, None);
    }
    let config = "[config]\nlanguage = 'java'\nentry = '/tmp/Grader'\n";
    dir.problem(config);
    let loaded = load_config(&dir.request(true));
    assert_eq!(loaded.diagnostics().len(), 1);
    assert_eq!(
        loaded.diagnostics()[0].message,
        "derived artifact must be output-relative"
    );
    assert_eq!(
        loaded.diagnostics()[0].origin,
        expected_origin(
            &dir.path("ws/problems/sample/problem.toml"),
            &problem_text(config),
            "entry",
            "config.entry"
        )
    );
    assert!(loaded.target().is_err());
    let user = "[languages.java]\nartifacts = { dir = true, required = ['Explicit.class'] }\n";
    dir.user(user);
    let loaded = load_config(&dir.request(true));
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert_eq!(plan.entry.unwrap().value, "/tmp/Grader");
    let Some(Artifacts::Dir { required }) = plan.artifacts else {
        panic!("directory artifact")
    };
    assert_eq!(required.len(), 1);
    assert_eq!(required[0].value, "Explicit.class");
    file_value(
        &required[0],
        &dir.path("user/cpforge/config.toml"),
        user,
        "'Explicit.class'",
        "languages.java.artifacts.required[0]",
    );
}

#[test]
fn default_roles_preserve_source_locations_with_derived_solution_provenance() {
    let dir = Directory::new();
    for (language, name) in [
        ("cpp", "main.cpp"),
        ("python", "main.py"),
        ("java", "Main.java"),
    ] {
        let mut request = dir.request(false);
        request.language = Some(language.into());
        let loaded = load_config(&request);
        clean(&loaded);
        let plan = loaded.target().unwrap();
        assert!(plan.support_files.is_empty());
        let settings = &loaded.universe().selected.as_ref().unwrap().settings;
        assert!(settings.support_files.array().unwrap().is_empty());
        assert_eq!(settings.support_files.provenance, Provenance::BuiltIn);
        assert_eq!(
            settings.support_files.origin,
            super::super::stages::origin("config.support_files", "<built-in>")
        );
        assert_eq!(
            settings.solution_files.provenance,
            Provenance::Derived {
                inputs: vec![Provenance::BuiltIn, Provenance::BuiltIn]
            }
        );
        assert_eq!(
            settings.solution_files.origin,
            super::super::stages::origin("config.solution_files", "<default>")
        );
        assert_eq!(plan.sources.len(), 1);
        assert_eq!(plan.sources[0].value.components(), [name]);
        assert_eq!(plan.sources[0].provenance, Provenance::BuiltIn);
        assert_eq!(plan.solution_files.len(), 1);
        assert_eq!(plan.solution_files[0].value, plan.sources[0].value);
        assert_eq!(plan.solution_files[0].origin, plan.sources[0].origin);
        assert_eq!(
            plan.solution_files[0].provenance,
            Provenance::Derived {
                inputs: vec![Provenance::BuiltIn]
            }
        );
        assert_eq!(
            plan.captured_files().collect::<Vec<_>>(),
            vec![&plan.sources[0]]
        );
    }
    dir.problem(
        "[config]\nlanguage = 'cpp'\nsources = ['a.cpp','b.cpp']\nsupport_files = ['header.h']\n",
    );
    let loaded = load_config(&dir.request(true));
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert_eq!(plan.solution_files.len(), 3);
    for (solution, captured) in plan.solution_files.iter().zip(plan.captured_files()) {
        assert_eq!(solution.value, captured.value);
        assert_eq!(solution.origin, captured.origin);
        assert_eq!(
            solution.provenance,
            Provenance::Derived {
                inputs: vec![captured.provenance.clone()]
            }
        );
    }
}

#[test]
fn prospective_problem_document_is_loaded_instead_of_the_existing_record() {
    let dir = Directory::new();
    dir.user("default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n");
    dir.problem("[config]\nunknown_key = true\n");
    let mut request = dir.request(true);
    let config = "[config]\nlanguage = 'cpp'\nsources = [\"solution.cpp\", 'grader.cpp']\nsolution_files = ['solution.cpp']\n";
    request.prospective_problem = Some(problem_text(config));
    let loaded = load_config(&request);
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert_eq!(plan.language_source, LanguageSource::Problem);
    file_value(
        &plan.language,
        &dir.path("ws/problems/sample/problem.toml"),
        &problem_text(config),
        "language",
        "config.language",
    );
    let (_, destination) = plan.template_destination().unwrap().unwrap();
    file_value(
        destination,
        &dir.path("ws/problems/sample/problem.toml"),
        &problem_text(config),
        "'solution.cpp'",
        "config.solution_files[0]",
    );
    assert_eq!(destination.value.components(), ["solution.cpp"]);
    assert_eq!(
        fs::read_to_string(dir.path("ws/problems/sample/problem.toml")).unwrap(),
        problem_text("[config]\nunknown_key = true\n")
    );
}

#[test]
fn problem_entry_is_available_to_preparation_without_a_language_entry() {
    use crate::exec::{
        Baseline, CommandBindings, Context, InvocationDirs, LanguageBinding, PreparedCommand,
        ProblemBinding, ProgramRequest, TargetBindings, prepare,
    };

    let dir = Directory::new();
    dir.user("[commands.show_entry]\nargv = ['echo', '{entry}']\n");
    dir.problem("[config]\nlanguage = 'python'\nentry = 'Grader'\n[config.run]\nargv = ['echo', '{entry}']\n");
    let loaded = load_config(&dir.request(true));
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert_eq!(plan.entry.as_ref().unwrap().origin.key_path, "config.entry");
    let dirs = InvocationDirs {
        run_cwd: dir.path("job/cwd"),
        problem: None,
        workspace: None,
    };
    // A problem's run stage needs the selected problem and its workspace.
    let target_dirs = InvocationDirs {
        run_cwd: dir.path("job/cwd"),
        problem: Some(ProblemBinding {
            dir: dir.path("ws/problems/sample"),
            id: "local:sample".into(),
        }),
        workspace: Some(dir.path("ws")),
    };
    let context = Context::target_run(
        &plan,
        target_dirs,
        TargetBindings {
            sources: vec![dir.path("job/inputs/src/main.py")],
            artifact: None,
            artifact_dir: None,
            entry: Some("Grader".into()),
        },
    )
    .unwrap();
    let expected = PreparedCommand::Argv {
        program: ProgramRequest::Lookup("echo".into()),
        args: vec!["Grader".into()],
    };
    assert_eq!(
        prepare(
            &plan.run,
            &context,
            &Baseline::default(),
            Platform::host(),
            &[]
        )
        .unwrap()
        .command,
        expected
    );
    let command = loaded.command("show_entry").unwrap();
    let context = Context::command(
        &command,
        dirs,
        CommandBindings {
            language: Some(LanguageBinding::from_target(&plan)),
            ..CommandBindings::default()
        },
    )
    .unwrap();
    assert_eq!(
        prepare(
            &command.stage,
            &context,
            &Baseline::default(),
            Platform::host(),
            &[]
        )
        .unwrap()
        .command,
        expected
    );
}

#[test]
fn language_selection_retains_problem_or_default_origin() {
    let dir = Directory::new();
    let user = "default_language = 'cpp'\n";
    dir.user(user);
    let problem = "[config]\nlanguage = 'python'\n";
    dir.problem(problem);
    let loaded = load_config(&dir.request(true));
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert_eq!(plan.language.value, "python");
    assert_eq!(plan.language_source, LanguageSource::Problem);
    file_value(
        &plan.language,
        &dir.path("ws/problems/sample/problem.toml"),
        &problem_text(problem),
        "language",
        "config.language",
    );
    assert!(plan.build.is_none());

    // A problem with no config table still selects a problem and uses the default.
    dir.problem("");
    let loaded = load_config(&dir.request(true));
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert_eq!(plan.language.value, "cpp");
    assert_eq!(plan.language_source, LanguageSource::DefaultLanguage);
    file_value(
        &plan.language,
        &dir.path("user/cpforge/config.toml"),
        user,
        "default_language",
        "default_language",
    );

    dir.user("[commands.anywhere]\nargv = ['echo']\n");
    let request = dir.request(true);
    let loaded = load_config(&request);
    clean(&loaded);
    assert!(
        loaded.target().unwrap_err().diagnostics[0]
            .message
            .contains("requires a language")
    );
    assert!(loaded.command("anywhere").is_ok());
    assert!(!loaded.has_errors());
    let loaded = load_config(&ConfigRequest {
        operation: Operation::Test,
        ..request
    });
    assert!(loaded.has_errors());
    assert_eq!(
        loaded.target().unwrap_err().diagnostics,
        loaded.diagnostics()
    );
    assert_eq!(
        loaded.command("anywhere").unwrap_err().diagnostics,
        loaded.diagnostics()
    );
    assert!(loaded.hooks().is_err());
}

#[test]
fn built_in_artifacts_java_derivation_and_build_free_python() {
    let dir = Directory::new();
    dir.user("default_language = 'cpp'\n");
    let cpp = load_config(&dir.request(false)).target().unwrap();
    let Some(Artifacts::File { name }) = cpp.artifacts else {
        panic!("file artifact")
    };
    assert_eq!(name.value, "main");
    assert_eq!(name.provenance, Provenance::BuiltIn);
    assert_eq!(name.origin.key_path, "languages.cpp.artifacts.file");
    assert_eq!(name.origin.file, PathBuf::from("<built-in>"));

    let user = "default_language = 'java'\n[languages.java]\nentry = 'a.b.Main'\n";
    dir.user(user);
    let loaded = load_config(&dir.request(false));
    clean(&loaded);
    let java = loaded.target().unwrap();
    let entry = java.entry.unwrap();
    file_value(
        &entry,
        &dir.path("user/cpforge/config.toml"),
        user,
        "entry",
        "languages.java.entry",
    );
    let Some(Artifacts::Dir { required }) = java.artifacts else {
        panic!("dir artifact")
    };
    assert_eq!(required.len(), 1);
    assert_eq!(required[0].value, "a/b/Main.class");
    assert_eq!(required[0].origin, entry.origin);
    assert_eq!(
        required[0].provenance,
        Provenance::Derived {
            inputs: vec![entry.provenance]
        }
    );

    dir.user("default_language = 'python'\n");
    let python = load_config(&dir.request(false)).target().unwrap();
    assert!(python.build.is_none());
    assert!(python.artifacts.is_none());
    assert_eq!(python.sources[0].value.components(), ["main.py"]);

    // A build-free target declares no artifacts, even when the language does.
    dir.user("default_language = 'python'\n[languages.python]\nartifacts = { file = 'x' }\n");
    let loaded = load_config(&dir.request(false));
    clean(&loaded);
    let python = loaded.target().unwrap();
    assert!(python.build.is_none());
    assert!(python.artifacts.is_none());
}

#[test]
fn parsed_argv_program_paths_and_shell_interpreter_remain_declarative() {
    let dir = Directory::new();
    let config = "default_language = 'cpp'\n[languages.cpp.build]\nargv = ['bin/x', '{sources}']\n\
        [commands.shell]\nshell = { interpreter = ['bin/sh', '-c'], script = 'echo {literal}', args = ['{problem_id}'] }\n";
    dir.write("cfg.toml", config);
    let mut request = dir.request(false);
    request.config_path = Some("cfg.toml".into());
    let loaded = load_config(&request);
    clean(&loaded);
    let plan = loaded.target().unwrap();
    let values = argv(plan.build.as_ref().unwrap());
    assert_eq!(values[0].tokens, [Token::Literal("bin/x".into())]);
    assert_eq!(values[0].path, Some(dir.path("bin/x")));
    assert!(!values[0].lookup);
    assert_eq!(
        values[0].origin,
        expected_origin(
            &dir.path("cfg.toml"),
            config,
            "'bin/x'",
            "languages.cpp.build.argv[0]"
        )
    );
    assert_eq!(
        values[0].provenance,
        Provenance::File {
            path: dir.path("cfg.toml"),
            line: Some(3),
            col: Some(9),
        }
    );
    assert_eq!(values[1].tokens, [Token::Placeholder(Placeholder::Sources)]);
    assert_eq!(values[1].path, None);
    let shell = loaded.command("shell").unwrap();
    let StageCommand::Shell {
        interpreter,
        script,
        args,
    } = shell.stage.command
    else {
        panic!("shell")
    };
    assert_eq!(interpreter.program.path, Some(dir.path("bin/sh")));
    assert_eq!(
        interpreter.program.tokens,
        [Token::Literal("bin/sh".into())]
    );
    assert_eq!(interpreter.args[0].value, "-c");
    assert_eq!(script.value, "echo {literal}");
    assert_eq!(args[0].tokens, [Token::Placeholder(Placeholder::ProblemId)]);
    file_value(
        &script,
        &dir.path("cfg.toml"),
        config,
        "script",
        "commands.shell.shell.script",
    );

    request.config_path = None;
    dir.user("default_language = 'cpp'\n");
    let loaded = load_config(&request);
    let plan = loaded.target().unwrap();
    let program = &argv(plan.build.as_ref().unwrap())[0];
    assert!(program.lookup);
    assert_eq!(program.path, None);
    assert_eq!(program.tokens, [Token::Literal("g++".into())]);
    assert_eq!(program.provenance, Provenance::BuiltIn);
}

#[test]
fn cwd_selectors_paths_and_native_home_prefix_are_preserved() {
    let dir = Directory::new();
    for (setting, expected, shared) in [
        ("", "run", false),
        ("cwd = { selector = 'run' }", "run", false),
        ("cwd = { selector = 'problem' }", "problem", true),
        ("cwd = { selector = 'workspace' }", "workspace", true),
        ("cwd = { path = 'out' }", "path", true),
    ] {
        let user = format!("[commands.x]\nargv = ['echo']\n{setting}\n");
        dir.user(&user);
        let loaded = load_config(&dir.request(false));
        clean(&loaded);
        let stage = loaded.command("x").unwrap().stage;
        assert_eq!(stage.shared_cwd(), shared);
        assert_eq!(stage.context.problem, expected == "problem");
        assert_eq!(
            stage.context.workspace,
            matches!(expected, "problem" | "workspace")
        );
        match (&stage.cwd.value, expected) {
            (Cwd::Run, "run") | (Cwd::Problem, "problem") | (Cwd::Workspace, "workspace") => {}
            (Cwd::Path(value), "path") => {
                assert_eq!(value.path, Some(dir.path("user/cpforge/out")));
                file_value(
                    &stage.cwd,
                    &dir.path("user/cpforge/config.toml"),
                    &user,
                    "path",
                    "commands.x.cwd.path",
                );
            }
            other => panic!("unexpected cwd {other:?}"),
        }
        if setting.is_empty() {
            assert_eq!(stage.cwd.provenance, Provenance::BuiltIn);
        }
    }
    let user = "[commands.x]\nargv = ['echo']\ncwd = { path = '~/{problem_id}' }\n";
    dir.user(user);
    let plan = load_config(&dir.request(false)).command("x").unwrap();
    let Cwd::Path(value) = plan.stage.cwd.value else {
        panic!("path")
    };
    assert_eq!(value.expanded_home, Some(dir.path("home")));
    assert_eq!(value.path, None);
    assert_eq!(
        value.tokens,
        [
            Token::Literal("~/".into()),
            Token::Placeholder(Placeholder::ProblemId)
        ]
    );
    assert_eq!(
        value.origin,
        expected_origin(
            &dir.path("user/cpforge/config.toml"),
            user,
            "path",
            "commands.x.cwd.path"
        )
    );
}

#[test]
fn environment_edits_remove_then_apply_tombstones_and_winning_spelling() {
    let dir = Directory::new();
    dir.user("[commands.edit]\nargv = ['echo']\nenv = { A = '1' }\n");
    // Windows folds keys for merging but installs the last source's spelling.
    let unset = if cfg!(windows) { "a" } else { "A" };
    let workspace =
        format!("[commands.edit]\nenv = {{ '{unset}' = false, B = '2' }}\nenv_remove = ['C']\n");
    dir.workspace(&workspace);
    let loaded = load_config(&dir.request(false));
    clean(&loaded);
    let edits = loaded.command("edit").unwrap().stage.environment;
    let [
        EnvEdit::Remove(remove),
        EnvEdit::Unset(tombstone),
        EnvEdit::Set { name, value },
    ] = edits.as_slice()
    else {
        panic!("unexpected edits {edits:?}");
    };
    assert_eq!(remove.value, "C");
    assert_eq!(tombstone.value, unset);
    assert_eq!(name.value, "B");
    assert_eq!(value.value, "2");
    let file = dir.path("ws/cpforge.toml");
    file_value(
        remove,
        &file,
        &workspace,
        "'C'",
        "commands.edit.env_remove[0]",
    );
    file_value(
        tombstone,
        &file,
        &workspace,
        &format!("'{unset}'"),
        &format!("commands.edit.env.{unset}"),
    );
    file_value(name, &file, &workspace, "B", "commands.edit.env.B");
    assert_eq!(value.origin, name.origin);
    assert!(
        loaded
            .command("edit")
            .unwrap()
            .stage
            .history
            .values()
            .flatten()
            .any(|event| event.kind == EventKind::Delete)
    );
}

#[test]
fn deadlines_caps_and_unrounded_limits_pass_through_c1c() {
    let dir = Directory::new();
    dir.user("default_language = 'cpp'\n");
    dir.problem("");
    let mut request = dir.request(true);
    request.operation = Operation::Test;
    let loaded = load_config(&request);
    clean(&loaded);
    let plan = loaded.target().unwrap();
    assert_eq!(plan.run.deadline.milliseconds.value, 10_000);
    assert!(!plan.run.deadline.explicit);
    assert_eq!(
        plan.run.deadline.milliseconds.provenance,
        Provenance::BuiltIn
    );
    assert_eq!(
        plan.build.as_ref().unwrap().deadline.milliseconds.value,
        60_000
    );
    assert_eq!(plan.run.stdout.bytes.value, 64 << 20);
    assert_eq!(plan.run.stderr.bytes.value, 1 << 20);
    assert_eq!(plan.run.stdout.policy, CapPolicy::Kill);
    assert_eq!(plan.run.stderr.policy, CapPolicy::Discard);

    let published_origin = super::super::stages::origin("limits.time", "problem.toml");
    request.published_limit = Some(PublishedLimit {
        milliseconds: 1200,
        provenance: Provenance::File {
            path: published_origin.file.clone(),
            line: None,
            col: None,
        },
        origin: published_origin,
    });
    let plan = load_config(&request).target().unwrap();
    assert_eq!(plan.run.deadline.milliseconds.value, 3400);
    assert!(!plan.run.deadline.explicit);
    assert_eq!(
        plan.run.deadline.milliseconds.provenance,
        Provenance::Derived {
            inputs: vec![request.published_limit.as_ref().unwrap().provenance.clone()],
        }
    );

    let user = "default_language = 'cpp'\n[run]\ndeadline_s = 2\nlimits = { address_space = 4097, job_commit = 8193 }\n";
    dir.user(user);
    let plan = load_config(&request).target().unwrap();
    assert!(plan.run.deadline.explicit);
    file_value(
        &plan.run.deadline.milliseconds,
        &dir.path("user/cpforge/config.toml"),
        user,
        "deadline_s",
        "run.deadline_s",
    );
    assert_eq!(plan.run.limits.address_space.as_ref().unwrap().value, 4097);
    assert_eq!(plan.run.limits.job_commit.as_ref().unwrap().value, 8193);
    file_value(
        plan.run.limits.address_space.as_ref().unwrap(),
        &dir.path("user/cpforge/config.toml"),
        user,
        "address_space",
        "run.limits.address_space",
    );

    request.deadline_ms = Some(700);
    let plan = load_config(&request).target().unwrap();
    assert_eq!(plan.run.deadline.milliseconds.value, 700);
    assert!(plan.run.deadline.explicit);
    assert_eq!(
        plan.run.deadline.milliseconds.provenance,
        Provenance::Cli {
            flag: "--deadline".into()
        }
    );
    assert_eq!(
        plan.run.deadline.milliseconds.origin,
        super::super::stages::origin("deadline_s", "<cli:--deadline>")
    );
    assert_eq!(plan.build.unwrap().deadline.milliseconds.value, 60_000);
    request.operation = Operation::Run;
    assert_eq!(
        load_config(&request)
            .target()
            .unwrap()
            .run
            .stderr
            .bytes
            .value,
        64 << 20
    );
    request.operation = Operation::Check;
    assert_eq!(
        load_config(&request)
            .target()
            .unwrap()
            .run
            .deadline
            .milliseconds
            .value,
        2000
    );
}

#[test]
fn commands_and_ordered_hook_contexts_have_independent_defaults() {
    let dir = Directory::new();
    let trusted = serde_json::to_string(dir.path("ws").to_str().unwrap()).unwrap();
    let user = format!(
        "default_language = 'cpp'\n[commands.tests]\nargv = ['echo', '{{artifact}}', '{{test_input}}', '{{iteration}}']\nneeds_build = true\nfor_each_test = true\nrepeat = 3\nkeep_going = true\n\
        [commands.quick]\nargv = ['echo', '{{problem_id}}']\ndeadline_s = 5\n\
        [commands.slow]\nargv = ['echo']\n\
        [hooks]\nenabled = false\ntrusted_workspaces = [{trusted}]\non_switch = ['quick', 'slow', 'quick']\nafter_test = ['slow']\n"
    );
    dir.user(&user);
    let mut request = dir.request(false);
    request.operation = Operation::Test;
    request.deadline_ms = Some(123);
    let loaded = load_config(&request);
    clean(&loaded);
    let command = loaded.command("tests").unwrap();
    assert!(command.needs_build.value);
    assert!(command.for_each_test.value);
    assert_eq!(command.repeat.value, 3);
    assert!(command.keep_going.value);
    assert_eq!(command.stdin.value, Stdin::Test);
    assert_eq!(
        command.context,
        ContextRequirements {
            problem: true,
            workspace: true
        }
    );
    assert_eq!(command.stage.context, command.context);
    assert_eq!(command.stage.deadline.milliseconds.value, 60_000);
    file_value(
        &command.repeat,
        &dir.path("user/cpforge/config.toml"),
        &user,
        "repeat",
        "commands.tests.repeat",
    );
    let hooks = loaded.hooks().unwrap();
    assert_eq!(
        hooks
            .on_switch
            .iter()
            .map(|hook| hook.name.value.as_str())
            .collect::<Vec<_>>(),
        ["quick", "slow", "quick"]
    );
    assert_eq!(hooks.after_test[0].name.value, "slow");
    let quick = &hooks.on_switch[0].stage;
    assert_eq!(quick.kind, StageKind::Hook);
    assert_eq!(quick.deadline.milliseconds.value, 5000);
    assert!(quick.deadline.explicit);
    file_value(
        &quick.deadline.milliseconds,
        &dir.path("user/cpforge/config.toml"),
        &user,
        "deadline_s",
        "commands.quick.deadline_s",
    );
    let slow = &hooks.on_switch[1].stage;
    assert_eq!(slow.deadline.milliseconds.value, 30_000);
    assert!(!slow.deadline.explicit);
    assert_eq!(slow.deadline.milliseconds.provenance, Provenance::BuiltIn);
    assert_eq!(
        loaded
            .command("slow")
            .unwrap()
            .stage
            .deadline
            .milliseconds
            .value,
        60_000
    );
    assert_eq!(
        loaded.command("slow").unwrap().context,
        ContextRequirements::default()
    );
    assert_eq!(loaded.command("slow").unwrap().stdin.value, Stdin::Null);
    assert!(!loaded.hook_settings().enabled.as_ref().unwrap().value);
    file_value(
        loaded.hook_settings().enabled.as_ref().unwrap(),
        &dir.path("user/cpforge/config.toml"),
        &user,
        "enabled",
        "hooks.enabled",
    );
    let paths = loaded.hook_settings().trusted_workspaces.as_ref().unwrap();
    assert_eq!(paths[0].value, dir.path("ws"));
    file_value(
        &paths[0],
        &dir.path("user/cpforge/config.toml"),
        &user,
        &trusted,
        "hooks.trusted_workspaces[0]",
    );
    assert!(!loaded.hook_settings().user_file_replaced);
}

#[test]
fn trust_settings_are_taken_only_from_the_default_user_document() {
    let dir = Directory::new();
    dir.user("[hooks]\nenabled = false\ntrusted_workspaces = []\n");
    dir.workspace("[hooks]\nenabled = true\ntrusted_workspaces = []\n");
    let loaded = load_config(&dir.request(false));
    assert_eq!(loaded.diagnostics().len(), 2);
    assert!(loaded.has_errors());
    assert!(!loaded.hook_settings().enabled.as_ref().unwrap().value);
    assert_eq!(
        loaded.hook_settings().enabled.as_ref().unwrap().origin.file,
        dir.path("user/cpforge/config.toml")
    );
    assert_eq!(loaded.hook_settings().trusted_workspaces, Some(vec![]));
    assert!(loaded.hooks().is_err());

    dir.workspace("schema = 1\n");
    dir.write(
        "override.toml",
        "[hooks]\nenabled = true\ntrusted_workspaces = []\n",
    );
    let mut request = dir.request(false);
    request.config_path = Some("override.toml".into());
    let loaded = load_config(&request);
    assert_eq!(loaded.diagnostics().len(), 2);
    assert_eq!(
        loaded.hook_settings(),
        &HookSettings {
            user_file_replaced: true,
            ..HookSettings::default()
        }
    );
    dir.write("override.toml", "[commands.x]\nargv = ['echo']\n");
    let loaded = load_config(&request);
    clean(&loaded);
    assert!(loaded.command("x").is_ok());
    assert_eq!(
        loaded.hook_settings(),
        &HookSettings {
            user_file_replaced: true,
            ..HookSettings::default()
        }
    );
}

#[test]
fn retention_defaults_overrides_and_validated_ignore_patterns() {
    let dir = Directory::new();
    let loaded = load_config(&dir.request(false));
    clean(&loaded);
    let retention = loaded.retention().unwrap();
    for (key, value, expected) in [
        ("keep_jobs", retention.keep_jobs, 20),
        ("job_bytes", retention.job_bytes, 1 << 30),
        ("workspace_bytes", retention.workspace_bytes, 4 << 30),
        ("cwd_bytes", retention.cwd_bytes, 256 << 20),
        ("build_bytes", retention.build_bytes, 1 << 30),
    ] {
        assert_eq!(value.value, expected);
        assert_eq!(value.provenance, Provenance::BuiltIn);
        assert_eq!(
            value.origin,
            super::super::stages::origin(&format!("retention.{key}"), "<built-in>")
        );
    }
    assert!(loaded.workspace_ignore().unwrap().is_empty());
    assert_eq!(loaded.hook_settings(), &HookSettings::default());
    dir.user("[retention]\nkeep_jobs = 7\njob_bytes = '2GiB'\n");
    let workspace =
        "[retention]\njob_bytes = '3GiB'\n[workspace]\nignore = ['*.swp', 'scratch/**']\n";
    dir.workspace(workspace);
    let loaded = load_config(&dir.request(false));
    clean(&loaded);
    let retention = loaded.retention().unwrap();
    assert_eq!(retention.keep_jobs.value, 7);
    file_value(
        &retention.keep_jobs,
        &dir.path("user/cpforge/config.toml"),
        "[retention]\nkeep_jobs = 7\njob_bytes = '2GiB'\n",
        "keep_jobs",
        "retention.keep_jobs",
    );
    assert_eq!(retention.job_bytes.value, 3 << 30);
    file_value(
        &retention.job_bytes,
        &dir.path("ws/cpforge.toml"),
        workspace,
        "job_bytes",
        "retention.job_bytes",
    );
    assert_eq!(retention.cwd_bytes.provenance, Provenance::BuiltIn);
    let ignore = loaded.workspace_ignore().unwrap();
    assert_eq!(
        ignore
            .iter()
            .map(|value| value.value.as_str())
            .collect::<Vec<_>>(),
        ["*.swp", "scratch/**"]
    );
    file_value(
        &ignore[1],
        &dir.path("ws/cpforge.toml"),
        workspace,
        "'scratch/**'",
        "workspace.ignore[1]",
    );
}

#[test]
fn compare_profile_and_ignored_build_modifiers_keep_provenance() {
    let dir = Directory::new();
    dir.user("default_language = 'python'\ncompare = { exact = true }\n[languages.python.profiles.fast.run]\ndeadline_s = 3\n[build]\ndeadline_s = 9\n");
    dir.problem("[config]\ncompare = { lines = true }\n");
    let mut request = dir.request(true);
    request.profile = Some("fast".into());
    let loaded = load_config(&request);
    clean(&loaded);
    let target = loaded.target().unwrap();
    assert_eq!(target.profile.as_ref().unwrap().value, "fast");
    assert_eq!(
        target.profile.unwrap().provenance,
        Provenance::Cli {
            flag: "--profile".into()
        }
    );
    assert_eq!(target.run.deadline.milliseconds.value, 3000);
    assert_eq!(target.compare.value, Comparator::Lines);
    file_value(
        &target.compare,
        &dir.path("ws/problems/sample/problem.toml"),
        &problem_text("[config]\ncompare = { lines = true }\n"),
        "compare",
        "config.compare",
    );
    assert!(
        target
            .ignored
            .values()
            .flatten()
            .any(|event| event.kind == EventKind::Ignored)
    );
    request.profile = None;
    request.problem_file = None;
    assert_eq!(
        load_config(&request).target().unwrap().compare.value,
        Comparator::Exact
    );
    dir.user("default_language = 'cpp'\n");
    let target = load_config(&request).target().unwrap();
    assert_eq!(target.compare.value, Comparator::Tokens);
    assert_eq!(target.compare.provenance, Provenance::BuiltIn);
}

#[test]
fn every_plan_is_gated_by_all_errors_but_inspection_is_available() {
    let dir = Directory::new();
    let user = "default_language = 'cpp'\nunknown_key = true\n[commands.x]\nargv = ['echo']\n";
    dir.user(user);
    let loaded = load_config(&dir.request(false));
    assert!(loaded.has_errors());
    assert_eq!(loaded.diagnostics().len(), 1);
    assert_eq!(
        loaded.diagnostics()[0].origin,
        expected_origin(
            &dir.path("user/cpforge/config.toml"),
            user,
            "unknown_key",
            "unknown_key"
        )
    );
    assert_eq!(
        loaded.target().unwrap_err().diagnostics,
        loaded.diagnostics()
    );
    assert_eq!(
        loaded.command("x").unwrap_err().diagnostics,
        loaded.diagnostics()
    );
    assert_eq!(
        loaded.hooks().unwrap_err().diagnostics,
        loaded.diagnostics()
    );
    assert!(loaded.retention().is_err());
    assert!(loaded.workspace_ignore().is_err());
    assert!(loaded.merged().default_language().is_some());
    assert!(loaded.universe().selected.is_some());

    dir.user("default_language = 'cpp'\n[commands.x]\nargv = ['echo']\n");
    let mut loaded = load_config(&dir.request(false));
    clean(&loaded);
    // Phases 1–2 currently emit only errors. Inject a warning at their report
    // boundary to exercise the consumer gate without adding a trust check.
    let warning = Diagnostic {
        severity: Severity::Warning,
        message: "example warning".into(),
        origin: super::super::stages::origin("hooks", "example.toml"),
        second_origin: None,
    };
    loaded.merged.diagnostics.push(warning.clone());
    loaded.report = check(&loaded.merged, &loaded.selection);
    loaded.diagnostics = loaded.report.diagnostics().to_vec();
    assert_eq!(loaded.diagnostics(), [warning]);
    assert!(!loaded.has_errors());
    assert!(loaded.target().is_ok());
    assert!(loaded.command("x").is_ok());
    assert!(loaded.hooks().is_ok());
    let missing = loaded.command("absent").unwrap_err();
    assert_eq!(missing.diagnostics.len(), 2);
    assert_eq!(missing.diagnostics[1].message, "unknown command \"absent\"");
}

#[test]
fn discovery_errors_are_diagnostics_and_do_not_prevent_other_file_checks() {
    let dir = Directory::new();
    let mut request = ConfigRequest {
        platform: Platform::Linux,
        current_dir: dir.0.clone(),
        ..ConfigRequest::default()
    };
    let loaded = load_config(&request);
    assert!(loaded.has_errors());
    assert_eq!(loaded.diagnostics().len(), 1);
    assert_eq!(
        loaded.diagnostics()[0].message,
        "cannot discover user configuration: no absolute XDG_CONFIG_HOME or HOME"
    );
    assert_eq!(
        loaded.diagnostics()[0].origin,
        super::super::stages::origin("", "<discovery>")
    );
    assert!(loaded.target().is_err());
    assert!(loaded.command("x").is_err());
    dir.workspace("unknown_key = 1\n");
    request.workspace_root = Some(dir.path("ws"));
    let loaded = load_config(&request);
    assert_eq!(loaded.diagnostics().len(), 2);
    assert_eq!(
        loaded.diagnostics()[1].origin.file,
        dir.path("ws/cpforge.toml")
    );
}

#[test]
fn relative_config_override_is_based_on_the_injected_current_directory() {
    let dir = Directory::new();
    dir.user("unknown_key = true\n");
    let config = "default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n";
    dir.write("invocation/settings/cfg.toml", config);
    let mut request = dir.request(false);
    request.current_dir = dir.path("invocation");
    request.config_path = Some("settings/cfg.toml".into());
    let loaded = load_config(&request);
    clean(&loaded);
    let target = loaded.target().unwrap();
    let template = target.template.unwrap();
    assert_eq!(template.value, dir.path("invocation/settings/t.cpp"));
    file_value(
        &template,
        &dir.path("invocation/settings/cfg.toml"),
        config,
        "path",
        "languages.cpp.template.path",
    );
    assert!(loaded.hook_settings().user_file_replaced);
}

#[test]
fn relative_config_override_needs_an_absolute_current_directory() {
    let dir = Directory::new();
    // `Cargo.toml` exists relative to the test process; it must not be read.
    for current_dir in ["", "invocation"] {
        let mut request = dir.request(false);
        request.current_dir = current_dir.into();
        request.config_path = Some("Cargo.toml".into());
        let loaded = load_config(&request);
        assert_eq!(loaded.diagnostics().len(), 1, "{:?}", loaded.diagnostics());
        assert_eq!(
            loaded.diagnostics()[0].message,
            "cannot resolve a relative --config path without the current directory"
        );
        assert_eq!(
            loaded.diagnostics()[0].origin,
            super::super::stages::origin("", "<discovery>")
        );
        assert!(loaded.target().is_err());
        assert_eq!(
            loaded.hook_settings(),
            &HookSettings {
                user_file_replaced: true,
                ..HookSettings::default()
            }
        );
    }
}

#[test]
fn phase_one_and_two_errors_keep_independent_file_origins() {
    let dir = Directory::new();
    let user = "default_language = 'cpp'\nunknown_key = true\n";
    let workspace = "[commands.bad]\nshell = { interpreter = ['sh', '-c'] }\n";
    dir.user(user);
    dir.workspace(workspace);
    let loaded = load_config(&dir.request(false));
    assert_eq!(loaded.diagnostics().len(), 2);
    assert_eq!(
        loaded.diagnostics()[0].origin,
        expected_origin(
            &dir.path("user/cpforge/config.toml"),
            user,
            "unknown_key",
            "unknown_key"
        )
    );
    assert_eq!(loaded.diagnostics()[1].message, "shell requires script");
    assert_eq!(
        loaded.diagnostics()[1].origin,
        expected_origin(
            &dir.path("ws/cpforge.toml"),
            workspace,
            "shell",
            "commands.bad.shell.script"
        )
    );
    assert_eq!(
        loaded.target().unwrap_err().diagnostics,
        loaded.diagnostics()
    );
    assert_eq!(
        loaded.command("bad").unwrap_err().diagnostics,
        loaded.diagnostics()
    );
}

#[test]
fn language_only_requests_can_select_without_a_default() {
    let dir = Directory::new();
    let mut request = dir.request(false);
    request.language = Some("cpp".into());
    let loaded = load_config(&request);
    clean(&loaded);
    let target = loaded.target().unwrap();
    assert_eq!(target.language.value, "cpp");
    assert_eq!(target.language_source, LanguageSource::Selection);
    assert_eq!(
        target.language.origin,
        super::super::stages::origin("language", "<selection>")
    );
    assert_eq!(
        target.language.provenance,
        Provenance::Cli {
            flag: "language".into()
        }
    );
}
