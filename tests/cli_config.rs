//! Portable read-only inspection fixtures. Tools are probed, never executed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Fixture(PathBuf);
impl Fixture {
    fn new(workspace: bool) -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let root = std::env::temp_dir().join(format!(
            "cpforge-inspect-{:032x}",
            u128::from_le_bytes(random)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(root).unwrap(),
        ));
        fixture.write("user/cpforge/config.toml", "schema = 1\n");
        if workspace {
            fixture.write("cpforge.toml", "schema = 1\n");
        }
        fixture
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
        command
            .current_dir(&self.0)
            .env("HOME", self.0.join("user"))
            .env("USERPROFILE", self.0.join("user"))
            .env("XDG_CONFIG_HOME", self.0.join("user"))
            .env("APPDATA", self.0.join("user"))
            .env("INSPECTION_INHERITED_SENTINEL", "never-display-this")
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_HOOK_DEPTH");
        command
    }
    fn run(&self, args: &[&str], status: i32) -> Output {
        let before = snapshot(&self.0);
        let output = self.command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        assert_eq!(
            before,
            snapshot(&self.0),
            "inspection changed workspace: {args:?}"
        );
        assert!(!under(&self.0, ".cpforge/lock").exists());
        output
    }
    fn problem(&self, name: &str, config: &str) {
        self.write(
            &format!("problems/{name}/problem.toml"),
            &format!(
                "schema=1\n[problem]\nid='local:{name}'\nkind='unknown-kind'\n[config]\n{config}"
            ),
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, directory: &Path, result: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.insert(path.strip_prefix(root).unwrap().to_owned(), None);
                walk(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    Some(fs::read(path).unwrap()),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}
fn json(output: &Output) -> Value {
    assert!(output.stdout.starts_with(b"{\"schema\":1,\"document\":\""));
    assert_eq!(output.stdout.last(), Some(&b'\n'));
    serde_json::from_slice(&output.stdout).unwrap()
}
fn text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}
/// Join a `/`-separated fixture path per component, so Windows paths use the
/// native separator like the paths cpf reports.
fn under(root: &Path, relative: &str) -> std::path::PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

fn native(path: &Path) -> Value {
    serde_json::from_slice(&cpforge::report::wire::encode_native(path.as_os_str())).unwrap()
}

#[test]
fn show_layers_profile_provenance_deferred_and_native_values() {
    let f = Fixture::new(true);
    f.write("user/cpforge/config.toml", "schema=1\ndefault_language='cpp'\n[languages.cpp.run]\nenv={USER_SET='one', REMOVE_ME='user'}\n[languages.cpp.profiles.fast.run]\ndeadline_s=3\n");
    f.write(
        "cpforge.toml",
        "schema=1\n[languages.cpp.run]\nenv={REMOVE_ME=false, WORKSPACE_SET='two'}\n",
    );
    f.problem(
        "A",
        "language='cpp'\n[config.run]\ndeadline_s=7\nenv={PROBLEM_SET='three'}\n",
    );
    let args = [
        "--problem",
        "A",
        "config",
        "show",
        "--profile",
        "fast",
        "--json",
        "--origin",
    ];
    let output = f.run(&args, 0);
    assert!(
        output
            .stdout
            .starts_with(b"{\"schema\":1,\"document\":\"config_show\",")
    );
    assert_eq!(output.stdout, f.run(&args, 0).stdout, "JSON must be stable");
    let doc = json(&output);
    assert_eq!(doc["config"]["target"]["run"]["deadline_s"], "7");
    assert_eq!(doc["config"]["target"]["profile"], "fast");
    assert_eq!(doc["config"]["target"]["run"]["env"]["REMOVE_ME"], false);
    assert_eq!(
        doc["config"]["target"]["run"]["env"]["USER_SET"]["utf8"],
        "one"
    );
    assert_eq!(
        doc["config"]["target"]["run"]["env"]["PROBLEM_SET"]["utf8"],
        "three"
    );
    for path in [
        under(&f.0, "user/cpforge/config.toml"),
        f.0.join("cpforge.toml"),
        under(&f.0, "problems/A/problem.toml"),
    ] {
        assert!(
            doc["origins"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["origin"]["path"] == native(&path) && o["origin"]["line"].is_u64()),
            "{path:?}: {doc}"
        );
    }
    assert!(
        doc["deferred"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["placeholder"] == "artifact")
    );
    assert!(
        doc["deferred"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["placeholder"] == "run_cwd")
    );
    assert!(
        doc["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["event"] == "delete")
    );
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(!rendered.contains("INSPECTION_INHERITED_SENTINEL"));
    assert!(!rendered.contains("never-display-this"));
    let human = text(&f.run(
        &[
            "config",
            "show",
            "--origin",
            "--profile",
            "fast",
            "--problem",
            "A",
        ],
        0,
    ));
    for expected in [
        "deferred {artifact}",
        "problem.toml:",
        "config.toml:",
        "cpforge.toml:",
        "built-in",
        "profile",
        "capture check checked",
        "plan-time",
    ] {
        assert!(human.contains(expected), "missing {expected}: {human}");
    }
}

#[test]
fn outside_workspace_uses_only_builtin_and_user_layers() {
    let f = Fixture::new(false);
    f.write(
        "user/cpforge/config.toml",
        "[languages.python.run]\nenv={ONLY_USER='yes'}\n",
    );
    let doc = json(&f.run(&["config", "show", "--json"], 0));
    assert_eq!(
        doc["config"]["languages"]["python"]["run"]["env"]["ONLY_USER"]["utf8"],
        "yes"
    );
    assert!(doc["config"].get("config").is_none());
    let paths = text(&f.run(&["config", "paths"], 0));
    assert_eq!(paths.lines().count(), 2);
    assert!(paths.contains("built-in:") && paths.contains("user:") && paths.contains("exists"));
    f.run(&["config", "check"], 0);
}

#[test]
fn check_collects_invalid_unused_layers_and_json_failure_has_error_document() {
    let f = Fixture::new(true);
    f.write(
        "user/cpforge/config.toml",
        "[languages.cpp]\nunknown_user_key = true\n",
    );
    f.write(
        "cpforge.toml",
        "[languages.python]\nunknown_workspace_key = true\n",
    );
    let output = f.run(&["config", "check"], 7);
    assert!(output.stdout.is_empty());
    let diagnostics = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "config.toml:",
        "cpforge.toml:",
        "languages.cpp.unknown_user_key",
        "languages.python.unknown_workspace_key",
    ] {
        assert!(diagnostics.contains(expected), "{diagnostics}");
    }
    let output = f.run(&["config", "show", "--json"], 7);
    let doc = json(&output);
    assert_eq!(doc["document"], "error");
    assert_eq!(doc["class"], "config");
    assert_eq!(doc["exit"], 7);
    assert!(doc["diagnostics"].as_array().unwrap().len() >= 2);
    f.run(&["doctor"], 7);
    f.run(&["config", "paths"], 0);
}

#[test]
fn tools_report_found_missing_generated_and_unavailable_without_executing() {
    let f = Fixture::new(true);
    let executable = if cfg!(windows) { "probe.exe" } else { "probe" };
    let tools = f.0.join("tools");
    fs::create_dir(&tools).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_cpf"), tools.join(executable)).unwrap();
    let path = toml_edit::Value::from(tools.to_str().unwrap()).to_string();
    f.write(
        "cpforge.toml",
        &format!(
            "\
[commands.present]\nargv=['{executable}']\nenv={{PATH={path}}}\n\
[commands.missing]\nargv=['cpf-impossible-tool-4967']\nenv={{PATH={path}}}\n\
[commands.generated]\nargv=['{{artifact}}']\nneeds_build=true\n\
[commands.context]\nargv=['{{problem_dir}}/tool']\n\
[commands.tests]\nargv=['{executable}']\nfor_each_test=true\n\
[languages.cpp.profiles.fast.build]\nargv=['{executable}', '{{sources}}']\nenv={{PATH={path}}}\n"
        ),
    );
    let output = text(&f.run(&["config", "check", "--tools"], 0));
    assert!(output.contains("commands.present: found "), "{output}");
    assert!(output.contains("commands.missing: not found"), "{output}");
    assert!(
        output.contains("commands.context: not checkable here"),
        "{output}"
    );
    assert!(
        output.contains("commands.tests: not checkable here"),
        "{output}"
    );
    assert!(
        output.contains("languages.cpp.profiles.fast.build: found "),
        "{output}"
    );
    f.problem("A", "language='cpp'\n");
    let output = text(&f.run(&["--problem", "A", "config", "check", "--tools"], 0));
    assert!(
        output.contains("commands.generated: deferred (generated)"),
        "{output}"
    );
    assert!(
        output.contains("languages.cpp.run: deferred (generated)"),
        "{output}"
    );
}

#[test]
fn paths_report_selected_files_and_missing_user_file() {
    let f = Fixture::new(true);
    fs::remove_file(under(&f.0, "user/cpforge/config.toml")).unwrap();
    f.problem("B", "language='python'\n");
    let output = text(&f.run(&["--problem", "B", "config", "paths"], 0));
    assert_eq!(output.lines().count(), 4, "{output}");
    assert!(
        output
            .lines()
            .any(|l| l.starts_with("user:") && l.contains("missing"))
    );
    assert!(
        output
            .lines()
            .any(|l| l.starts_with("problem:") && l.contains("exists"))
    );
}

#[test]
fn doctor_is_informative_on_every_platform_and_reports_capture_plans() {
    let f = Fixture::new(true);
    f.write("cpforge.toml", "[languages.shelllang]\nextensions=['sl']\nsources=['main.sl']\nbuild=false\n[languages.shelllang.run]\nshell={interpreter=['sh','-c'], script='true'}\n");
    // Leftover recovery data must remain untouched, even without a lock file.
    f.write(".cpforge/staging/unrelated", "do not recover");
    let output = text(&f.run(&["doctor"], 0));
    let supervisor = if cfg!(any(target_os = "linux", target_os = "macos")) {
        "supervisor: supported"
    } else {
        "supervisor: unavailable"
    };
    for expected in [
        supervisor,
        "configuration: valid",
        "languages.cpp.build:",
        "measurement PeakRss:",
        "method=",
        "compiled=",
        "probe=",
        "limit AddressSpace:",
        "cleanup verification:",
        "collision key Unicode version: 16.0.0",
        "cgroup accounting: unavailable feature",
        "build cache: unavailable feature",
        "capture check unchecked{shell_stage}",
        "informative only",
    ] {
        assert!(output.contains(expected), "missing {expected}: {output}");
    }
    // Schema 1 only defines a machine document for config show.
    f.run(&["doctor", "--json"], 2);
    f.run(&["config", "check", "--json"], 2);
}

#[test]
fn explicit_problem_overrides_cwd_and_unresolved_language_is_valid() {
    let f = Fixture::new(true);
    f.problem("A", "language='cpp'\n");
    f.problem("B", "language='python'\n");
    let output = f
        .command()
        .current_dir(under(&f.0, "problems/A"))
        .args(["--problem", "B", "config", "show", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(json(&output)["config"]["target"]["language"], "python");
    f.problem("Unresolved", "");
    f.run(&["--problem", "Unresolved", "config", "check"], 0);
    let doc = json(&f.run(&["--problem", "Unresolved", "config", "show", "--json"], 0));
    assert!(doc["config"].get("target").is_none());
    let doc = json(&f.run(&["--problem", "Missing", "config", "show", "--json"], 7));
    assert_eq!(doc["document"], "error");
}

#[test]
fn phase_two_checks_unused_profiles_and_commands() {
    let f = Fixture::new(true);
    f.write("cpforge.toml", "[languages.extra]\nextensions=['x']\nbuild=false\n[languages.extra.run]\nargv=['unused']\n[languages.extra.profiles.bad.run]\nshell={args=['partial']}\n[commands.incomplete]\ndeadline_s=2\n");
    let output = f.run(&["config", "check"], 7);
    let diagnostics = String::from_utf8(output.stderr).unwrap();
    assert!(
        diagnostics.contains("interpreter") && diagnostics.contains("script"),
        "{diagnostics}"
    );
    assert!(diagnostics.contains("incomplete"), "{diagnostics}");
}

#[test]
fn float_mapping_and_control_character_environment_values_use_canonical_forms() {
    let f = Fixture::new(true);
    f.write(
        "cpforge.toml",
        "[languages.cpp.run]\ndeadline_s=0.0005\nenv={CONTROL='''first\nsecond'''}\n",
    );
    let output = f.run(&["config", "show", "--json"], 0);
    let doc = json(&output);
    let duration = &doc["config"]["languages"]["cpp"]["run"]["deadline_s"];
    assert_eq!(duration["bits"], format!("{:016x}", 0.0005_f64.to_bits()));
    assert!(duration["text"].is_string());
    assert_eq!(
        doc["config"]["languages"]["cpp"]["run"]["env"]["CONTROL"]["bytes_b64"],
        "Zmlyc3QKc2Vjb25k"
    );
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains(":0.0005")
    );
}

#[test]
fn check_reports_warnings_with_provenance_and_never_runs_trusted_hooks() {
    let f = Fixture::new(true);
    let workspace = toml_edit::Value::from(f.0.to_str().unwrap()).to_string();
    let missing =
        toml_edit::Value::from(f.0.join("missing-trusted-workspace").to_str().unwrap()).to_string();
    f.write("user/cpforge/config.toml", &format!("[hooks]\ntrusted_workspaces=[{workspace}, {missing}]\non_switch=['side_effect']\nafter_test=['side_effect']\n[commands.side_effect]\nargv=['cpf', 'init', 'unexpected']\n"));
    let output = f.run(&["config", "check"], 0);
    let diagnostics = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "warning",
        "config.toml:",
        "hooks.trusted_workspaces",
        "cannot be canonicalised",
    ] {
        assert!(diagnostics.contains(expected), "{diagnostics}");
    }
    f.run(&["config", "show"], 0);
    f.run(&["doctor"], 0);
}

#[test]
fn override_config_does_not_read_default_user_and_paths_stay_informative() {
    let f = Fixture::new(true);
    f.write("user/cpforge/config.toml", "this is invalid TOML !");
    f.write(
        "replacement.toml",
        "[languages.cpp.run]\nenv={REPLACEMENT='yes'}\n",
    );
    let doc = json(&f.run(
        &["--config", "replacement.toml", "config", "show", "--json"],
        0,
    ));
    assert_eq!(doc["hooks_disabled_by_config_override"], true);
    assert_eq!(
        doc["config"]["languages"]["cpp"]["run"]["env"]["REPLACEMENT"]["utf8"],
        "yes"
    );
    let paths = text(&f.run(&["--config", "replacement.toml", "config", "paths"], 0));
    assert!(paths.contains("user (--config):") && paths.contains("replacement.toml"));
    f.run(&["--config", "missing.toml", "config", "check"], 7);
    let doc = json(&f.run(&["--config", "missing.toml", "config", "show", "--json"], 7));
    assert_eq!(doc["document"], "error");
    f.run(&["--problem", "Missing", "config", "paths"], 0);
}

#[test]
fn malformed_problem_does_not_hide_other_layer_errors() {
    let f = Fixture::new(true);
    f.write("user/cpforge/config.toml", "unknown_user_key=true\n");
    f.write("cpforge.toml", "unknown_workspace_key=true\n");
    f.write(
        "problems/A/problem.toml",
        "[problem]\nid='local:A'\n[config\n",
    );
    let output = f.run(&["--problem", "A", "config", "check"], 7);
    let diagnostics = String::from_utf8(output.stderr).unwrap();
    for expected in ["unknown_user_key", "unknown_workspace_key", "problem.toml"] {
        assert!(diagnostics.contains(expected), "{diagnostics}");
    }
    let doc = json(&f.run(&["--problem", "A", "config", "show", "--json"], 7));
    assert_eq!(doc["document"], "error");
    assert_eq!(doc["diagnostics"].as_array().unwrap().len(), 3, "{doc}");
    let paths = text(&f.run(&["--problem", "A", "config", "paths"], 0));
    assert!(
        paths
            .lines()
            .any(|line| line.starts_with("problem:") && line.contains("exists"))
    );
}
