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
    // A held workspace lock: Windows refuses reads of its locked range, so it
    // is compared by length and modification time, which any write changes.
    Locked(u64, std::time::SystemTime),
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
        } else if relative == Path::new(".cpforge").join("lock") {
            TreeEntry::Locked(metadata.len(), metadata.modified().unwrap())
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

#[cfg(windows)]
#[test]
fn trusted_switch_warns_without_running_hooks_on_windows() {
    let workspace = Workspace::new();
    workspace.problem("A");
    fs::write(workspace.path("cpforge.toml"), "schema = 1\n[commands.h]\nargv = ['cmd.exe', '/c', 'echo unexpected']\n[hooks]\non_switch = ['h']\n").unwrap();
    let trusted = serde_json::to_string(workspace.0.to_str().unwrap()).unwrap();
    fs::write(
        workspace.path("user/cpforge/config.toml"),
        format!("schema = 1\n[hooks]\ntrusted_workspaces = [{trusted}]\n"),
    )
    .unwrap();
    let output = workspace.run("", &["switch", "A"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"local:A\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("hooks are unsupported"));
    assert_eq!(
        fs::read_dir(workspace.path(".cpforge/runs"))
            .unwrap()
            .count(),
        0
    );
}

impl Workspace {
    fn remote(&self, directory: &str, identity: &str) {
        self.problem(directory);
        self.metadata(
            directory,
            &format!("schema = 1\n[problem]\nid = '{identity}'\n"),
        );
        fs::write(
            self.path("problems").join(directory).join("tests.toml"),
            "schema = 1\n",
        )
        .unwrap();
    }

    fn contest(&self, directory: &str, text: &str) {
        let dir = self.path("contests").join(directory);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("contest.toml"), text).unwrap();
    }

    fn remote_fixture() -> Self {
        let workspace = Self::new();
        workspace.remote("atcoder-abc400_a", "atcoder:abc400_a");
        workspace.remote("cses-1068", "cses:1068");
        workspace.problem("two-sum");
        workspace.contest("atcoder-abc400", "schema = 1\n[contest]\nid = 'atcoder:abc400'\n[[problem]]\nid = 'atcoder:abc400_a'\nlabel = 'A'\n[[problem]]\nid = 'atcoder:abc400_b'\nlabel = 'B'\n");
        workspace.contest(
            "c",
            "schema = 1\n[[problem]]\nid = 'local:two-sum'\nlabel = 'A'\n",
        );
        workspace
    }
}

fn selected_identity(output: &Output, identity: &str) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        output.stdout,
        format!("{identity}\n").as_bytes(),
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn remote_problems_and_contest_labels_use_the_shared_selection_in_every_reader() {
    let workspace = Workspace::remote_fixture();
    for (reference, identity, directory) in [
        ("atcoder:abc400_a", "atcoder:abc400_a", "atcoder-abc400_a"),
        ("atcoder-abc400_a", "atcoder:abc400_a", "atcoder-abc400_a"),
        ("atcoder-abc400/A", "atcoder:abc400_a", "atcoder-abc400_a"),
        ("cses:1068", "cses:1068", "cses-1068"),
        ("cses-1068", "cses:1068", "cses-1068"),
        ("c/A", "local:two-sum", "two-sum"),
    ] {
        selected_identity(
            &workspace.run("", &["current", "--problem", reference]),
            identity,
        );
        let path = workspace.run("", &["current", "--path", "--problem", reference]);
        let mut expected =
            path_bytes(&fs::canonicalize(workspace.path("problems").join(directory)).unwrap());
        expected.push(b'\n');
        assert_eq!(path.status.code(), Some(0), "{path:?}");
        assert_eq!(path.stdout, expected);
        assert!(path.stderr.is_empty(), "{path:?}");
        for args in [
            vec!["status", reference],
            vec!["status", "--problem", reference],
        ] {
            let output = workspace.run("", &args);
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            assert!(
                output
                    .stdout
                    .starts_with(format!("{identity}\n").as_bytes()),
                "{output:?}"
            );
            assert!(output.stderr.is_empty(), "{output:?}");
        }
        let json = workspace.run("", &["status", "--json", reference]);
        assert_eq!(json.status.code(), Some(0), "{json:?}");
        let document: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(document["problems"][0]["problem"], identity);
        assert!(json.stderr.is_empty(), "{json:?}");
    }
    // Stored selections are identities, never the label or derived directory.
    selected_identity(
        &workspace.run("", &["switch", "atcoder-abc400/A"]),
        "atcoder:abc400_a",
    );
    assert_eq!(
        fs::read(workspace.path(".cpforge/state.toml")).unwrap(),
        b"schema = 1\ncurrent_problem = \"atcoder:abc400_a\"\n"
    );
    selected_identity(&workspace.run("", &["current"]), "atcoder:abc400_a");
    selected_identity(
        &workspace.run("problems/cses-1068/nested", &["current"]),
        "cses:1068",
    );
    // Explicit local identities cannot borrow a remote directory's binding.
    failure(
        &workspace.run("", &["current", "--problem", "local:cses-1068"]),
        8,
        &["local:cses-1068", "IdentityMismatch"],
    );
    let all = workspace.run("", &["status", "--all", "--json"]);
    assert_eq!(all.status.code(), Some(0), "{all:?}");
    let document: serde_json::Value = serde_json::from_slice(&all.stdout).unwrap();
    assert_eq!(document["problems"].as_array().unwrap().len(), 3);
    let contest = workspace.run("", &["status", "contest:c"]);
    assert_eq!(contest.status.code(), Some(0), "{contest:?}");
    assert!(contest.stdout.starts_with(b"local:two-sum\n"));
}

#[test]
fn every_remote_reference_failure_names_the_reference_and_cause_without_fallback() {
    let workspace = Workspace::remote_fixture();
    workspace.saved("two-sum");
    workspace.problem("a");
    workspace.problem("Z");
    for (reference, cause) in [
        ("atcoder-abc400/a", "no record with label"),
        ("atcoder-abc400/B", "atcoder:abc400_b not found"),
        ("atcoder-abc400/Z", "no record with label"),
        ("nope/A", "unknown contest"),
        ("Atcoder-abc400/A", "DifferentSpelling"),
        ("atcoder:abc400_c", "not found"),
        ("atcoder:ABC400_A", "invalid identity name"),
        ("other:a", "unknown identity site"),
        ("c/", "label must be"),
        ("c/A/extra", "label must be"),
        ("c/../A", "label must be"),
    ] {
        let before = tree(&workspace.0);
        for args in [
            vec!["current", "--problem", reference],
            vec!["switch", reference],
            vec!["status", reference],
            vec!["status", "--problem", reference],
            vec!["test", "ls", "--problem", reference],
        ] {
            failure(
                &workspace.run("problems/two-sum", &args),
                8,
                &[reference, cause],
            );
            assert_eq!(tree(&workspace.0), before, "{args:?}");
        }
    }
    // JSON status defines an error document on stdout; diagnostics stay on stderr.
    let output = workspace.run("", &["status", "--json", "nope/A"]);
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["document"], "error");
    assert_eq!(document["class"], "workspace");
    assert!(String::from_utf8_lossy(&output.stderr).contains("nope/A"));
}

#[test]
fn contest_classification_precedes_label_lookup_and_binding_is_never_corrected() {
    let workspace = Workspace::remote_fixture();
    for (text, cause) in [
        (
            "schema = 1\n[[problem]]\nid = 'local:two-sum'\nlabel = 'A'\n[[problem]]\nid = 'cses:1068'\nlabel = 'A'\n",
            "malformed",
        ),
        (
            "schema = 1\n[[problem]]\nid = 'cses:1068'\nlabel = 'A'\n[[problem]]\nid = 'cses:1068'\nlabel = 'B'\n",
            "malformed",
        ),
        ("schema = 2\n[contest]\nid = []\nproblem = 5\n", "too_new"),
        (
            "schema = 1\n[[problem]]\nid = 'cses:0'\nlabel = 'A'\n",
            "malformed",
        ),
        (
            "schema = 1\n[[problem]]\nid = 'cses:1068'\nlabel = 1\n",
            "malformed",
        ),
        ("schema = 1\n[contest]\n", "malformed"),
        ("schema = 1\n[contest]\nid = 'local:c'\n", "malformed"),
        (
            "schema = 1\n[contest]\nid = 'atcoder:abc400'\n[[problem]]\nid = 'cses:1068'\nlabel = 'A'\n",
            "IdentityMismatch",
        ),
    ] {
        workspace.contest("c", text);
        let before = tree(&workspace.0);
        for args in [
            vec!["current", "--problem", "c/A"],
            vec!["switch", "c/A"],
            vec!["status", "c/A"],
            vec!["status", "contest:c"],
        ] {
            failure(&workspace.run("", &args), 8, &["c", "contest.toml", cause]);
            assert_eq!(tree(&workspace.0), before);
        }
    }
    // A valid remote contest with no members remains usable for status.
    workspace.contest(
        "atcoder-abc400",
        "schema = 1\n[contest]\nid = 'atcoder:abc400'\n",
    );
    let status = workspace.run("", &["status", "contest:atcoder-abc400"]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    assert_eq!(status.stdout, b"no problems\n");
    let path = workspace.path("contests/c/contest.toml");
    fs::remove_file(&path).unwrap();
    failure(
        &workspace.run("", &["current", "--problem", "c/A"]),
        8,
        &["c/A", "contest.toml", "missing"],
    );
    fs::create_dir(&path).unwrap();
    failure(
        &workspace.run("", &["current", "--problem", "c/A"]),
        8,
        &["c/A", "contest.toml", "unreadable"],
    );
    fs::write(workspace.path("contests/file"), b"file").unwrap();
    failure(
        &workspace.run("", &["current", "--problem", "file/A"]),
        8,
        &["file/A", "NotADirectory"],
    );
}

#[test]
fn status_and_fsck_report_remote_binding_mismatches_and_keep_records() {
    let workspace = Workspace::remote_fixture();
    // Keep every member present so apply has no dangling membership to repair.
    workspace.remote("atcoder-abc400_b", "atcoder:abc400_b");
    let healthy = workspace.run("", &["fsck"]);
    assert_eq!(healthy.status.code(), Some(0), "{healthy:?}");
    assert!(healthy.stderr.is_empty(), "{healthy:?}");
    assert!(!String::from_utf8_lossy(&healthy.stdout).contains("IdentityMismatch"));
    for text in ["atcoder:abc400_b", "cses:1068"] {
        workspace.metadata("atcoder-abc400_a", &format!("[problem]\nid = '{text}'\n"));
        let before = tree(&workspace.0);
        for reference in ["atcoder:abc400_a", "atcoder-abc400_a", "atcoder-abc400/A"] {
            for args in [
                vec!["current", "--problem", reference],
                vec!["switch", reference],
                vec!["status", reference],
            ] {
                failure(
                    &workspace.run("", &args),
                    8,
                    &[reference, "IdentityMismatch"],
                );
            }
        }
        failure(
            &workspace.run("problems/atcoder-abc400_a", &["current"]),
            8,
            &["IdentityMismatch"],
        );
        for args in [vec!["fsck"], vec!["fsck", "--apply"]] {
            let output = workspace.run("", &args);
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let path = Path::new("problems")
                .join("atcoder-abc400_a")
                .join("problem.toml");
            assert!(
                stdout
                    .lines()
                    .any(|line| line.contains(&path.display().to_string())
                        && line.contains("IdentityMismatch")),
                "{output:?}"
            );
            assert!(output.stderr.is_empty(), "{output:?}");
            assert_eq!(tree(&workspace.0), before);
        }
    }
    workspace.contest(
        "atcoder-abc400",
        "schema = 1\n[contest]\nid = 'atcoder:abc401'\n",
    );
    failure(
        &workspace.run("", &["status", "contest:atcoder-abc400"]),
        8,
        &["IdentityMismatch", "contest.toml"],
    );
    let output = workspace.run("", &["fsck", "--apply"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.contains("contest.toml") && line.contains("IdentityMismatch")),
        "{output:?}"
    );
    assert!(
        fs::read_to_string(workspace.path("contests/atcoder-abc400/contest.toml"))
            .unwrap()
            .contains("atcoder:abc401")
    );
}

#[test]
fn remote_looking_local_names_remain_local_and_creation_does_not_reuse_remote_problems() {
    let workspace = Workspace::new();
    for (name, remote) in [
        ("cses-1068", "cses:1068"),
        ("atcoder-abc400_a", "atcoder:abc400_a"),
    ] {
        let created = workspace.run("", &["add", name]);
        assert_eq!(created.status.code(), Some(0), "{created:?}");
        for reference in [name.to_owned(), format!("local:{name}")] {
            selected(
                &workspace.run("", &["current", "--problem", &reference]),
                name,
            );
            let status = workspace.run("", &["status", &reference]);
            assert_eq!(status.status.code(), Some(0), "{status:?}");
            assert!(
                status
                    .stdout
                    .starts_with(format!("local:{name}\n").as_bytes())
            );
        }
        let inspection = workspace.run("", &["fsck"]);
        assert_eq!(inspection.status.code(), Some(0), "{inspection:?}");
        assert!(!String::from_utf8_lossy(&inspection.stdout).contains("IdentityMismatch"));
        let before = tree(&workspace.0);
        failure(
            &workspace.run("", &["current", "--problem", remote]),
            8,
            &[remote, "IdentityMismatch"],
        );
        failure(
            &workspace.run("", &["switch", remote]),
            8,
            &[remote, "IdentityMismatch"],
        );
        assert_eq!(tree(&workspace.0), before);
        workspace.remote(name, remote);
        let before = tree(&workspace.0);
        failure(&workspace.run("", &["add", name]), 8, &["IdentityMismatch"]);
        assert_eq!(tree(&workspace.0), before);
    }
}

#[test]
fn remote_and_label_readers_take_no_lock_and_create_nothing() {
    let workspace = Workspace::remote_fixture();
    workspace.state("schema = 1\ncurrent_problem = 'atcoder:abc400_a'\n");
    let lock = cpforge::storage::WorkspaceLock::new(&workspace.0).unwrap();
    let guard = lock.acquire("remote reader test").unwrap();
    let before = tree(&workspace.0);
    for reference in ["atcoder:abc400_a", "atcoder-abc400/A", "cses:1068", "c/A"] {
        let expected = if reference == "c/A" {
            "local:two-sum"
        } else if reference == "cses:1068" {
            "cses:1068"
        } else {
            "atcoder:abc400_a"
        };
        selected_identity(
            &workspace.run("", &["current", "--problem", reference]),
            expected,
        );
        let status = workspace.run("", &["status", reference]);
        assert_eq!(status.status.code(), Some(0), "{status:?}");
        let listing = workspace.run("", &["test", "ls", "--problem", reference]);
        assert_eq!(listing.status.code(), Some(0), "{listing:?}");
        assert!(listing.stdout.is_empty(), "{listing:?}");
        assert!(listing.stderr.is_empty(), "{listing:?}");
    }
    assert_eq!(tree(&workspace.0), before);
    drop(guard);
    fs::remove_file(workspace.path(".cpforge/lock")).unwrap();
    let before = tree(&workspace.0);
    selected_identity(&workspace.run("", &["current"]), "atcoder:abc400_a");
    selected_identity(
        &workspace.run("", &["current", "--problem", "atcoder-abc400/A"]),
        "atcoder:abc400_a",
    );
    let status = workspace.run("", &["status", "atcoder-abc400/A"]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    failure(
        &workspace.run("", &["current", "--problem", "atcoder-abc400/Z"]),
        8,
        &["no record"],
    );
    assert_eq!(tree(&workspace.0), before);
    assert!(!workspace.path(".cpforge/lock").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn colliding_contest_entries_refuse_label_selection() {
    let workspace = Workspace::remote_fixture();
    workspace.contest("C", "schema = 1\n");
    for args in [
        vec!["current", "--problem", "c/A"],
        vec!["switch", "c/A"],
        vec!["status", "c/A"],
    ] {
        failure(&workspace.run("", &args), 8, &["c/A", "SeveralEntries"]);
    }
}

#[cfg(any(unix, windows))]
#[test]
fn linked_contest_entries_are_refused() {
    let workspace = Workspace::remote_fixture();
    let link = workspace.path("contests/l");
    #[cfg(unix)]
    std::os::unix::fs::symlink("c", &link).unwrap();
    #[cfg(windows)]
    {
        let output = Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(workspace.path("contests/c"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    for args in [
        vec!["current", "--problem", "l/A"],
        vec!["switch", "l/A"],
        vec!["status", "l/A"],
    ] {
        failure(&workspace.run("", &args), 8, &["l/A", "LinkInPath"]);
    }
    // Remove the link explicitly; never let fixture cleanup traverse a junction.
    #[cfg(unix)]
    fs::remove_file(link).unwrap();
    #[cfg(windows)]
    fs::remove_dir(link).unwrap();
}

#[test]
fn label_references_report_invalid_member_problems_and_preserve_the_workspace() {
    let workspace = Workspace::remote_fixture();
    for (text, cause) in [
        ("[problem]\nid = 'atcoder:abc400_b'\n", "IdentityMismatch"),
        ("[problem]\nid = 'atcoder:ABC400_A'\n", "MetadataMalformed"),
        ("schema = 2\n[problem]\nid = []\n", "MetadataTooNew"),
        ("not = [", "MetadataMalformed"),
    ] {
        workspace.metadata("atcoder-abc400_a", text);
        let before = tree(&workspace.0);
        for args in [
            vec!["current", "--problem", "atcoder-abc400/A"],
            vec!["switch", "atcoder-abc400/A"],
            vec!["status", "atcoder-abc400/A"],
            vec!["test", "ls", "--problem", "atcoder-abc400/A"],
        ] {
            failure(&workspace.run("", &args), 8, &["atcoder-abc400/A", cause]);
            assert_eq!(tree(&workspace.0), before);
        }
    }
    fs::remove_file(workspace.path("problems/atcoder-abc400_a/problem.toml")).unwrap();
    failure(
        &workspace.run("", &["current", "--problem", "atcoder-abc400/A"]),
        8,
        &["atcoder-abc400/A", "MetadataMissing"],
    );
    fs::create_dir(workspace.path("problems/atcoder-abc400_a/problem.toml")).unwrap();
    failure(
        &workspace.run("", &["current", "--problem", "atcoder-abc400/A"]),
        8,
        &["atcoder-abc400/A", "MetadataUnreadable"],
    );
    // Directory spelling remains exact even when the filesystem ignores case.
    failure(
        &workspace.run("", &["current", "--problem", "Atcoder-abc400_a"]),
        8,
        &["Atcoder-abc400_a", "DifferentSpelling"],
    );
}
