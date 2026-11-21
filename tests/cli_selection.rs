//! Selection contract through the command binary and the shared resolver API.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cpforge::config::{ConfigRequest, Environment, load_config};
use cpforge::selection::{self, Source};

#[derive(Debug)]
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        // Match the CLI add fixtures: avoid inheriting a host git tree or config.
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = temporary
            .ancestors()
            .find_map(|base| {
                if base
                    .ancestors()
                    .any(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())
                {
                    return None;
                }
                let path = base.join(format!(
                    "cpforge-selection-{:032x}",
                    u128::from_le_bytes(bits)
                ));
                fs::create_dir(&path).ok().map(|()| path)
            })
            .expect("a writable temporary directory outside a git tree");
        let workspace = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(path).unwrap(),
        ));
        fs::create_dir_all(workspace.path("user/cpforge")).unwrap();
        let output = workspace.command().arg("init").output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        workspace
    }

    fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        // Join per component so `/` in fixtures becomes the native separator.
        relative
            .as_ref()
            .components()
            .fold(self.0.clone(), |path, part| path.join(part))
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
        command
            .current_dir(&self.0)
            .env("HOME", self.path("user"))
            .env("USERPROFILE", self.path("user"))
            .env("XDG_CONFIG_HOME", self.path("user"))
            .env("APPDATA", self.path("user"))
            .env_remove("CPF_STORAGE_CRASH_AT");
        command
    }

    fn run(&self, cwd: impl AsRef<Path>, args: &[&str]) -> Output {
        self.command()
            .current_dir(self.path(cwd))
            .args(args)
            .output()
            .unwrap()
    }

    fn problem(&self, name: &str) {
        fs::create_dir_all(self.path(format!("problems/{name}/nested"))).unwrap();
        self.metadata(name, &format!("[problem]\nid = 'local:{name}'\n"));
    }

    fn metadata(&self, name: &str, text: &str) {
        fs::write(self.path(format!("problems/{name}/problem.toml")), text).unwrap();
    }

    fn state(&self, text: &str) {
        fs::write(self.path(".cpforge/state.toml"), text).unwrap();
    }

    fn saved(&self, name: &str) {
        self.state(&format!("schema = 1\ncurrent_problem = \"local:{name}\"\n"));
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn selected(output: &Output, name: &str) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        output.stdout,
        format!("local:{name}\n").as_bytes(),
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

fn failure(output: &Output, code: i32, messages: &[&str]) {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.lines().any(|line| line.starts_with("cpf: error: ")),
        "{output:?}"
    );
    for message in messages {
        assert!(stderr.contains(message), "expected {message:?}: {output:?}");
    }
}

#[test]
fn fresh_workspace_and_empty_saved_state_have_no_selection() {
    let workspace = Workspace::new();
    for args in [vec!["current"], vec!["current", "--path"]] {
        failure(&workspace.run("", &args), 8, &["NoSelection"]);
    }
    workspace.state("schema = 1\n");
    failure(&workspace.run("", &["current"]), 8, &["NoSelection"]);
    // An unresolvable flag inside the workspace is an error, not NoSelection.
    failure(
        &workspace.run("user", &["current", "--problem", "missing"]),
        8,
        &["--problem", "not found"],
    );
    let output = workspace
        .command()
        .current_dir(workspace.0.parent().unwrap())
        .arg("current")
        .output()
        .unwrap();
    // Outside a workspace the existing workspace-required error is reported.
    failure(&output, 8, &["outside a workspace"]);
}

#[test]
fn flag_wins_without_reading_broken_cwd_problem_or_state() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.problem("B");
    workspace.metadata("A", "not valid TOML [");
    for state in ["broken = [", "schema = 2\ncurrent_problem = []\n"] {
        workspace.state(state);
        selected(
            &workspace.run("problems/A/nested", &["--problem", "B", "current"]),
            "B",
        );
        selected(
            &workspace.run("problems/A", &["current", "--problem", "local:B"]),
            "B",
        );
    }
    fs::remove_file(workspace.path("problems/A/problem.toml")).unwrap();
    fs::create_dir(workspace.path("problems/A/problem.toml")).unwrap();
    fs::remove_file(workspace.path(".cpforge/state.toml")).unwrap();
    fs::create_dir(workspace.path(".cpforge/state.toml")).unwrap();
    selected(
        &workspace.run("problems/A", &["current", "--problem", "B"]),
        "B",
    );
    // A failed flag must not inspect lower sources, even when both are broken.
    failure(
        &workspace.run("problems/A", &["current", "--problem", "missing"]),
        8,
        &["--problem flag", "local:missing", "not found"],
    );
}

#[test]
fn invalid_flag_never_falls_back_to_valid_cwd_or_saved_selection() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.saved("A");
    for reference in [
        "other:x",
        "Local:A",
        "local:",
        "local:a:b",
        "a/b",
        "../A",
        "./A",
        "/A",
        "a\\b",
        "a..b",
    ] {
        failure(
            &workspace.run("problems/A", &["current", "--problem", reference]),
            8,
            &["--problem flag", "invalid problem reference"],
        );
        failure(
            &workspace.run("", &["switch", reference]),
            8,
            &["invalid problem reference"],
        );
    }
    failure(
        &workspace.run("problems/A", &["current", "--problem", "missing"]),
        8,
        &["--problem flag", "local:missing", "not found"],
    );
}

#[test]
fn cwd_wins_over_broken_state_and_a_broken_cwd_never_falls_back() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.problem("B");
    for state in ["broken = [", "schema = 2\n"] {
        workspace.state(state);
        selected(&workspace.run("problems/A/nested", &["current"]), "A");
    }
    fs::remove_file(workspace.path(".cpforge/state.toml")).unwrap();
    fs::create_dir(workspace.path(".cpforge/state.toml")).unwrap();
    selected(&workspace.run("problems/A", &["current"]), "A");
    fs::remove_dir(workspace.path(".cpforge/state.toml")).unwrap();
    workspace.saved("B");
    workspace.metadata("A", "[problem]\nid = 'local:other'\n");
    failure(
        &workspace.run("problems/A/nested", &["current"]),
        8,
        &["this directory problems/A", "IdentityMismatch"],
    );
    fs::create_dir(workspace.path("problems/invalid name")).unwrap();
    failure(
        &workspace.run("problems/invalid name", &["current"]),
        8,
        &[
            "this directory problems/invalid name",
            "invalid problem reference",
        ],
    );
}

#[test]
fn outside_problem_ancestry_uses_saved_selection_component_wise() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.saved("A");
    fs::create_dir_all(workspace.path("problems-extra/invalid")).unwrap();
    for cwd in ["", "user", "problems", "problems-extra/invalid"] {
        selected(&workspace.run(cwd, &["current"]), "A");
        assert_eq!(
            selection::resolve(&workspace.0, &workspace.path(cwd), None)
                .unwrap()
                .source,
            Source::Saved
        );
    }
    assert_eq!(
        selection::resolve(&workspace.0, &workspace.path("problems/A/nested"), None)
            .unwrap()
            .source,
        Source::Cwd
    );
    assert_eq!(
        selection::resolve(&workspace.0, &workspace.path("problems/A"), Some("A"))
            .unwrap()
            .source,
        Source::Flag
    );
}

#[test]
fn stale_saved_problem_is_named_and_can_be_replaced_without_resolving_it() {
    let workspace = Workspace::new();
    workspace.problem("gone");
    workspace.problem("other");
    selected(&workspace.run("", &["switch", "gone"]), "gone");
    fs::remove_dir_all(workspace.path("problems/gone")).unwrap();
    failure(
        &workspace.run("", &["current"]),
        8,
        &["saved identity local:gone", "not found"],
    );
    selected(&workspace.run("", &["switch", "local:other"]), "other");
    assert_eq!(
        fs::read(workspace.path(".cpforge/state.toml")).unwrap(),
        b"schema = 1\ncurrent_problem = \"local:other\"\n"
    );
    selected(&workspace.run("", &["current"]), "other");
}

#[test]
fn invalid_state_is_refused_byte_identically_only_when_needed_by_current() {
    let workspace = Workspace::new();
    workspace.problem("A");
    for (text, message) in [
        ("bad = [", "malformed"),
        ("current_problem = 'local:A'\n", "schema` is missing"),
        ("schema = '1'\n", "not an integer"),
        ("schema = 0\n", "older than supported"),
        ("schema = 1\nunknown = true\n", "unknown key"),
        ("schema = 1\ncurrent_problem = []\n", "must be a string"),
        ("schema = 1\ncurrent_problem = 'other:A'\n", "malformed"),
        (
            "schema = 2\ncurrent_problem = []\nunknown = true\n",
            "too_new",
        ),
    ] {
        workspace.state(text);
        failure(
            &workspace.run("", &["current"]),
            8,
            &["saved selection", message],
        );
        failure(&workspace.run("", &["switch", "A"]), 8, &[message]);
        assert_eq!(
            fs::read(workspace.path(".cpforge/state.toml")).unwrap(),
            text.as_bytes()
        );
        selected(&workspace.run("", &["current", "--problem", "A"]), "A");
        selected(&workspace.run("problems/A", &["current"]), "A");
    }
    let oversized = vec![b' '; 1024 * 1024 + 1];
    fs::write(workspace.path(".cpforge/state.toml"), &oversized).unwrap();
    failure(&workspace.run("", &["current"]), 8, &["size limit"]);
    failure(&workspace.run("", &["switch", "A"]), 8, &["size limit"]);
    assert_eq!(
        fs::read(workspace.path(".cpforge/state.toml")).unwrap(),
        oversized
    );
    fs::remove_file(workspace.path(".cpforge/state.toml")).unwrap();
    fs::create_dir(workspace.path(".cpforge/state.toml")).unwrap();
    failure(&workspace.run("", &["current"]), 8, &["unreadable"]);
    failure(&workspace.run("", &["switch", "A"]), 8, &["unreadable"]);
    assert!(workspace.path(".cpforge/state.toml").is_dir());
}

#[test]
fn problem_metadata_errors_are_the_same_for_references_and_cwd() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.problem("B");
    workspace.saved("B");
    for (text, reason) in [
        ("[problem]\nid = 'local:other'\n", "IdentityMismatch"),
        ("[problem]\nid = 'other:A'\n", "MetadataMalformed"),
        ("[problem]\nid = 1\n", "MetadataMalformed"),
        ("schema = [", "MetadataMalformed"),
        ("schema = '1'\n", "MetadataMalformed"),
        ("schema = 0\n", "MetadataMalformed"),
        ("schema = 2\n[problem]\nid = []\n", "MetadataTooNew"),
        ("", "MetadataMalformed"),
    ] {
        workspace.metadata("A", text);
        failure(
            &workspace.run("", &["current", "--problem", "A"]),
            8,
            &["--problem flag", reason],
        );
        failure(
            &workspace.run("problems/A", &["current"]),
            8,
            &["this directory problems/A", reason],
        );
        failure(&workspace.run("", &["switch", "A"]), 8, &[reason]);
    }
    fs::remove_file(workspace.path("problems/A/problem.toml")).unwrap();
    for (directory, reason) in [(false, "MetadataMissing"), (true, "MetadataUnreadable")] {
        if directory {
            fs::create_dir(workspace.path("problems/A/problem.toml")).unwrap();
        }
        failure(&workspace.run("", &["switch", "A"]), 8, &[reason]);
        failure(&workspace.run("problems/A", &["current"]), 8, &[reason]);
    }
    fs::write(workspace.path("problems/file"), b"not a directory").unwrap();
    failure(
        &workspace.run("", &["switch", "file"]),
        8,
        &["NotADirectory"],
    );
    failure(
        &workspace.run("", &["current", "--problem", "file"]),
        8,
        &["NotADirectory"],
    );
}

#[test]
fn different_spelling_names_the_existing_entry() {
    let workspace = Workspace::new();
    workspace.problem("CamelCase");
    failure(
        &workspace.run("", &["switch", "camelcase"]),
        8,
        &["DifferentSpelling", "CamelCase"],
    );
    failure(
        &workspace.run("", &["current", "--problem", "local:camelcase"]),
        8,
        &["DifferentSpelling", "CamelCase"],
    );
    workspace.saved("camelcase");
    failure(
        &workspace.run("", &["current"]),
        8,
        &[
            "saved identity local:camelcase",
            "DifferentSpelling",
            "CamelCase",
        ],
    );
}

#[cfg(target_os = "linux")]
#[test]
fn colliding_entries_cannot_be_selected_by_any_source() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.problem("a");
    for name in ["A", "a"] {
        failure(
            &workspace.run("", &["switch", name]),
            8,
            &["SeveralEntries", "problems/A", "problems/a"],
        );
        failure(
            &workspace.run("", &["current", "--problem", name]),
            8,
            &["SeveralEntries", "problems/A", "problems/a"],
        );
        failure(
            &workspace.run(format!("problems/{name}"), &["current"]),
            8,
            &[
                "this directory",
                "SeveralEntries",
                "problems/A",
                "problems/a",
            ],
        );
        workspace.saved(name);
        failure(
            &workspace.run("", &["current"]),
            8,
            &[
                "saved identity",
                "SeveralEntries",
                "problems/A",
                "problems/a",
            ],
        );
    }
}

#[cfg(unix)]
#[test]
fn links_are_refused_by_reference_and_cwd_ancestry_is_physical() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    workspace.problem("b");
    workspace.problem("saved");
    workspace.saved("saved");
    symlink("b", workspace.path("problems/a")).unwrap();
    failure(
        &workspace.run("", &["switch", "a"]),
        8,
        &["LinkInPath", "problems/a"],
    );
    failure(
        &workspace.run("", &["current", "--problem", "a"]),
        8,
        &["LinkInPath"],
    );
    workspace.saved("a");
    failure(
        &workspace.run("", &["current"]),
        8,
        &["saved identity local:a", "LinkInPath"],
    );
    workspace.saved("saved");
    selected(&workspace.run("problems/a/nested", &["current"]), "b");
    symlink("../user", workspace.path("problems/outside")).unwrap();
    selected(&workspace.run("problems/outside", &["current"]), "saved");
    // Resolving an explicitly supplied logical cwd follows the same rule.
    let resolved = selection::resolve(&workspace.0, &workspace.path("problems/a"), None).unwrap();
    assert_eq!(resolved.identity.to_string(), "local:b");
    assert_eq!(resolved.source, Source::Cwd);
    let resolved =
        selection::resolve(&workspace.0, &workspace.path("problems/outside"), None).unwrap();
    assert_eq!(resolved.source, Source::Saved);
    symlink(&workspace.0, workspace.path("root-link")).unwrap();
    let resolved = selection::resolve(
        &workspace.path("root-link"),
        &workspace.path("root-link/problems/b"),
        None,
    )
    .unwrap();
    assert_eq!(resolved.root, workspace.0);
    assert_eq!(resolved.identity.to_string(), "local:b");
    fs::remove_file(workspace.path("root-link")).unwrap();
}

#[test]
fn unknown_kind_and_unconfigured_problem_are_selectable() {
    let workspace = Workspace::new();
    workspace.problem("unknown");
    workspace.metadata(
        "unknown",
        "[problem]\nid = 'local:unknown'\nkind = 'weird'\n",
    );
    selected(&workspace.run("", &["switch", "unknown"]), "unknown");
    selected(&workspace.run("", &["current"]), "unknown");
    selected(&workspace.run("problems/unknown", &["current"]), "unknown");
}

#[derive(Debug, PartialEq, Eq)]
enum TreeEntry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

// Capture names, types, and contents without following links; omit access times.
fn tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, relative: &Path, result: &mut BTreeMap<PathBuf, TreeEntry>) {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).unwrap();
        let entry = if metadata.is_dir() {
            let mut entries: Vec<_> = fs::read_dir(&path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            entries.sort();
            for entry in entries {
                visit(root, &relative.join(entry), result);
            }
            TreeEntry::Directory
        } else if metadata.file_type().is_symlink() {
            TreeEntry::Link(fs::read_link(&path).unwrap())
        } else {
            TreeEntry::File(fs::read(&path).unwrap())
        };
        result.insert(relative.to_path_buf(), entry);
    }
    let mut result = BTreeMap::new();
    visit(root, Path::new(""), &mut result);
    result
}

#[test]
fn current_never_creates_lock_recovers_or_changes_workspace_bytes() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.saved("A");
    fs::remove_file(workspace.path(".cpforge/lock")).unwrap();
    let leftover = ".cpf-tmp-0123456789abcdef0123456789abcdef";
    fs::write(workspace.path(format!(".cpforge/{leftover}")), b"keep").unwrap();
    fs::create_dir_all(workspace.path(".cpforge/staging/abandoned")).unwrap();
    fs::write(workspace.path(".cpforge/staging/abandoned/file"), b"keep").unwrap();
    let before = tree(&workspace.0);
    selected(&workspace.run("", &["current"]), "A");
    selected(&workspace.run("problems/A", &["current"]), "A");
    selected(&workspace.run("", &["current", "--problem", "A"]), "A");
    let output = workspace.run("", &["current", "--path"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    failure(
        &workspace.run("", &["current", "--problem", "missing"]),
        8,
        &["not found"],
    );
    assert_eq!(tree(&workspace.0), before);
    assert!(!workspace.path(".cpforge/lock").exists());
    // Switch uses the existing acquisition/recovery primitive.
    selected(&workspace.run("", &["switch", "A"]), "A");
    assert!(!workspace.path(format!(".cpforge/{leftover}")).exists());
    assert!(!workspace.path(".cpforge/staging/abandoned").exists());
}

#[test]
fn same_selection_switch_preserves_state_bytes_and_metadata() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.state("# preserve this representation\nschema = 1\ncurrent_problem = 'local:A'\n");
    let path = workspace.path(".cpforge/state.toml");
    let bytes = fs::read(&path).unwrap();
    let before = fs::metadata(&path).unwrap();
    selected(&workspace.run("", &["switch", "A"]), "A");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        before.modified().unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&path).unwrap().ino(), before.ino());
        assert_eq!(fs::metadata(&path).unwrap().dev(), before.dev());
    }
}

#[test]
fn switch_inside_another_or_broken_problem_prints_only_identity_and_a_note() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.problem("B");
    for broken in [false, true] {
        if broken {
            workspace.metadata("A", "broken = [");
        }
        let output = workspace.run("problems/A/nested", &["switch", "B"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(output.stdout, b"local:B\n");
        let note = String::from_utf8_lossy(&output.stderr);
        assert!(note.starts_with("cpf: note: "), "{output:?}");
        assert!(
            note.contains(if broken {
                "could not be resolved"
            } else {
                "this directory still selects local:A"
            }),
            "{output:?}"
        );
    }
    for args in [
        vec!["--problem", "A", "switch", "B"],
        vec!["switch", "B", "--problem", "A"],
    ] {
        failure(&workspace.run("", &args), 2, &["--problem", "switch"]);
    }
    selected(&workspace.run("problems/B", &["switch", "B"]), "B");
}

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        cpforge::storage::paths::plain(path)
            .display()
            .to_string()
            .into_bytes()
    }
}

#[test]
fn current_path_is_the_canonical_problem_directory_plus_newline() {
    let workspace = Workspace::new();
    workspace.problem("A");
    let expected = fs::canonicalize(workspace.path("problems/A")).unwrap();
    let mut expected = path_bytes(&expected);
    expected.push(b'\n');
    let output = workspace.run("problems/A/nested", &["current", "--path"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(output.stdout, expected);
}

#[cfg(windows)]
#[test]
fn current_path_and_selection_from_a_canonicalized_temp_root_are_plain() {
    let workspace = Workspace::new();
    workspace.problem("A");
    let canonical = fs::canonicalize(&workspace.0).unwrap();
    let cwd = canonical.join(r"problems\A\nested");
    let selected = selection::resolve(&canonical, &cwd, None).unwrap();
    assert!(
        !selected
            .root
            .as_os_str()
            .as_encoded_bytes()
            .starts_with(br"\\?\")
    );
    assert!(
        !selected
            .dir
            .as_os_str()
            .as_encoded_bytes()
            .starts_with(br"\\?\")
    );
    assert_eq!(selected.source, Source::Cwd);
    let output = workspace.run(&cwd, &["current", "--path"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let mut expected = selected.dir.display().to_string().into_bytes();
    expected.push(b'\n');
    assert_eq!(output.stdout, expected);
    assert!(!output.stdout.starts_with(br"\\?\"));
}

#[cfg(unix)]
#[test]
fn current_path_preserves_non_unicode_bytes_and_newlines() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let workspace = Workspace::new();
    // APFS refuses names that are not UTF-8; the newline is still covered there.
    let name: &[u8] = if cfg!(target_os = "macos") {
        b"workspace-\nroot"
    } else {
        b"workspace-\xff\nroot"
    };
    let root = workspace.path(OsString::from_vec(name.to_vec()));
    fs::create_dir_all(root.join("problems/A")).unwrap();
    fs::write(root.join("cpforge.toml"), "schema = 1\n").unwrap();
    fs::write(
        root.join("problems/A/problem.toml"),
        "[problem]\nid = 'local:A'\n",
    )
    .unwrap();
    let output = workspace
        .command()
        .current_dir(root.join("problems/A"))
        .args(["current", "--path"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    let mut expected = path_bytes(&fs::canonicalize(root.join("problems/A")).unwrap());
    expected.push(b'\n');
    assert_eq!(output.stdout, expected);
}

#[test]
fn configuration_uses_flag_selected_problem_and_its_language_origin() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.problem("B");
    workspace.metadata(
        "A",
        "[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\n",
    );
    workspace.metadata(
        "B",
        "[problem]\nid = 'local:B'\n[config]\nlanguage = 'python'\n",
    );
    let selected =
        selection::resolve(&workspace.0, &workspace.path("problems/A"), Some("B")).unwrap();
    let request = selected.config_request(ConfigRequest {
        environment: Environment {
            home: Some(workspace.path("user")),
            xdg_config_home: Some(workspace.path("user").into_os_string()),
            appdata: Some(workspace.path("user").into_os_string()),
        },
        current_dir: workspace.path("problems/A"),
        ..ConfigRequest::default()
    });
    let loaded = load_config(&request);
    let target = loaded.target().unwrap();
    assert_eq!(target.language.value, "python");
    assert_eq!(
        target.language.origin.file,
        workspace.path("problems/B/problem.toml")
    );
    assert_eq!(target.language.origin.key_path, "config.language");
}

#[test]
fn concurrent_current_observes_only_complete_old_or_new_saved_identities() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.problem("B");
    workspace.saved("A");
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            for name in ["B", "A", "B", "A", "B", "A", "B", "A"] {
                selected(&workspace.run("", &["switch", name]), name);
            }
        });
        for _ in 0..16 {
            let output = workspace.run("", &["current"]);
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            assert!(
                output.stdout == b"local:A\n" || output.stdout == b"local:B\n",
                "{output:?}"
            );
        }
        writer.join().unwrap();
    });
}

#[test]
fn current_reads_saved_selection_while_the_workspace_lock_is_held() {
    let workspace = Workspace::new();
    workspace.problem("A");
    workspace.saved("A");
    let lock = cpforge::storage::WorkspaceLock::new(&workspace.0).unwrap();
    let guard = lock.acquire("selection reader test").unwrap();
    selected(&workspace.run("", &["current"]), "A");
    drop(guard);
}

#[test]
fn add_never_changes_the_saved_selection() {
    let workspace = Workspace::new();
    let output = workspace.run("", &["add", "A"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(!workspace.path(".cpforge/state.toml").exists());
    workspace.saved("A");
    let before = fs::read(workspace.path(".cpforge/state.toml")).unwrap();
    let output = workspace.run("", &["add", "B"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        fs::read(workspace.path(".cpforge/state.toml")).unwrap(),
        before
    );
    selected(&workspace.run("", &["current"]), "A");
}

#[cfg(unix)]
#[test]
fn read_only_metadata_links_are_allowed_and_state_writes_refuse_links() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    workspace.problem("A");
    fs::rename(
        workspace.path("problems/A/problem.toml"),
        workspace.path("user/problem.toml"),
    )
    .unwrap();
    symlink(
        "../../user/problem.toml",
        workspace.path("problems/A/problem.toml"),
    )
    .unwrap();
    selected(&workspace.run("", &["current", "--problem", "A"]), "A");
    workspace.saved("A");
    fs::rename(
        workspace.path(".cpforge/state.toml"),
        workspace.path("user/state.toml"),
    )
    .unwrap();
    symlink("../user/state.toml", workspace.path(".cpforge/state.toml")).unwrap();
    let before = fs::read(workspace.path("user/state.toml")).unwrap();
    selected(&workspace.run("", &["current"]), "A");
    failure(
        &workspace.run("", &["switch", "A"]),
        8,
        &["refusing a link"],
    );
    assert_eq!(fs::read(workspace.path("user/state.toml")).unwrap(), before);
    assert!(
        fs::symlink_metadata(workspace.path(".cpforge/state.toml"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(windows)]
#[test]
fn current_path_uses_canonical_structural_directory_spelling_on_windows() {
    let workspace = Workspace::new();
    workspace.problem("A");
    fs::rename(
        workspace.path("problems"),
        workspace.path("renamed-problems"),
    )
    .unwrap();
    fs::rename(
        workspace.path("renamed-problems"),
        workspace.path("PROBLEMS"),
    )
    .unwrap();
    let output = workspace.run("", &["current", "--path", "--problem", "A"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    let mut expected = path_bytes(&fs::canonicalize(workspace.path("PROBLEMS/A")).unwrap());
    expected.push(b'\n');
    assert_eq!(output.stdout, expected);
    selected(&workspace.run("PROBLEMS/A", &["current"]), "A");
}
