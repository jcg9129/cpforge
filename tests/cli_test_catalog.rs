//! Test catalog commands through the binary, including read-only listings.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        let path = std::env::temp_dir().join(format!(
            "cpforge-test-catalog-{:032x}",
            u128::from_le_bytes(bits)
        ));
        fs::create_dir(&path).unwrap();
        let workspace = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(path).unwrap(),
        ));
        fs::write(workspace.path("cpforge.toml"), "schema = 1\n").unwrap();
        for name in ["A", "B"] {
            fs::create_dir_all(workspace.path(format!("problems/{name}/tests"))).unwrap();
            fs::create_dir_all(workspace.path(format!("problems/{name}/nested"))).unwrap();
            fs::write(
                workspace.path(format!("problems/{name}/problem.toml")),
                format!("[problem]\nid = 'local:{name}'\n"),
            )
            .unwrap();
        }
        workspace
    }

    fn path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }

    fn file(&self, name: &str, bytes: &[u8]) {
        fs::write(self.path(format!("problems/A/tests/{name}")), bytes).unwrap();
    }

    fn catalog(&self, text: &str) {
        fs::write(self.path("problems/A/tests.toml"), text).unwrap();
    }

    fn catalog_bytes(&self) -> Vec<u8> {
        fs::read(self.path("problems/A/tests.toml")).unwrap()
    }

    fn run(&self, cwd: &str, args: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(self.path(cwd))
            .env_remove("CPF_STORAGE_CRASH_AT")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > Duration::from_secs(10) {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!("command blocked: {args:?}: {output:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }

    fn test(&self, args: &[&str]) -> Output {
        let mut command = vec!["test"];
        command.extend_from_slice(args);
        self.run("problems/A", &command)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn success(output: Output) -> Output {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    output
}

fn failure(output: Output, words: &[&str]) {
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    let message = String::from_utf8_lossy(&output.stderr);
    for word in words {
        assert!(message.contains(word), "missing {word:?}: {output:?}");
    }
}

fn entry(id: &str, input: &str, answer: Option<&str>, enabled: bool) -> String {
    format!(
        "\n[[test]]\nid = '{id}'\ninput = 'tests/{input}'\n{}origin = 'custom'\nenabled = {enabled}\n",
        answer.map_or(String::new(), |answer| format!(
            "answer = 'tests/{answer}'\n"
        ))
    )
}

#[test]
fn register_infers_sibling_or_deliberately_omits_it_and_prints_only_id() {
    let workspace = Workspace::new();
    workspace.file("one.in", b"input");
    workspace.file("one.ans", b"");
    let output = success(workspace.test(&["register", "tests/one.in"]));
    assert_eq!(output.stdout, b"one\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("using sibling answer tests/one.ans"));
    let text = String::from_utf8(workspace.catalog_bytes()).unwrap();
    assert!(text.starts_with("schema = 1\n"));
    assert!(text.contains("answer = \"tests/one.ans\""));
    workspace.file("two.in", b"input");
    workspace.file("two.ans", b"ignored");
    let output = success(workspace.test(&["register", "tests/two.in", "--no-answer"]));
    assert_eq!(output.stdout, b"two\n");
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(workspace.catalog_bytes()).unwrap();
    assert!(!text.contains("tests/two.ans"));
    let listing = success(workspace.test(&["ls"]));
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("one enabled answer empty"));
    assert!(listing.contains("two enabled answer absent"));
    assert!(listing.contains("stray tests/two.ans"));
    // An input ending in .ans is never its own sibling answer.
    workspace.file("solo.ans", b"data");
    let output = success(workspace.test(&["register", "tests/solo.ans"]));
    assert_eq!(output.stdout, b"solo\n");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn default_stem_must_be_valid_and_free_and_explicit_id_works() {
    let workspace = Workspace::new();
    workspace.file("one.in", b"1");
    workspace.file("one.dat", b"2");
    workspace.file("bad name.in", b"3");
    success(workspace.test(&["register", "tests/one.in", "--no-answer"]));
    let before = workspace.catalog_bytes();
    failure(
        workspace.test(&["register", "tests/one.dat"]),
        &["taken", "--id"],
    );
    failure(
        workspace.test(&["register", "tests/bad name.in"]),
        &["valid id", "--id"],
    );
    assert_eq!(workspace.catalog_bytes(), before);
    assert_eq!(
        success(workspace.test(&["register", "tests/one.dat", "--id", "second"])).stdout,
        b"second\n"
    );
    assert_eq!(
        success(workspace.test(&["register", "tests/bad name.in", "--id", "third"])).stdout,
        b"third\n"
    );
}

#[test]
fn register_refuses_every_already_named_role_and_duplicate_roles() {
    let workspace = Workspace::new();
    workspace.file("one.in", b"1");
    workspace.file("one.ans", b"2");
    success(workspace.test(&["register", "tests/one.in"]));
    let before = workspace.catalog_bytes();
    for path in ["tests/one.in", "tests/one.ans"] {
        failure(
            workspace.test(&["register", path, "--no-answer", "--id", "other"]),
            &["already named"],
        );
        assert_eq!(workspace.catalog_bytes(), before);
    }
    workspace.file("new.in", b"3");
    failure(
        workspace.test(&["register", "tests/new.in", "--answer", "tests/one.in"]),
        &["already named"],
    );
    failure(
        workspace.test(&["register", "tests/new.in", "--answer", "tests/new.in"]),
        &["both roles"],
    );
    assert_eq!(workspace.catalog_bytes(), before);
}

#[test]
fn paths_are_lexical_relative_to_cwd_and_problem_flag_wins() {
    let workspace = Workspace::new();
    workspace.file("one.in", b"1");
    failure(
        workspace.run("", &["--problem", "A", "test", "register", "tests/one.in"]),
        &["current directory"],
    );
    assert!(!workspace.path("problems/A/tests.toml").exists());
    let output = success(workspace.run(
        "problems/B",
        &["test", "register", "../A/tests/./one.in", "--problem", "A"],
    ));
    assert_eq!(output.stdout, b"one\n");
    assert!(!workspace.path("problems/B/tests.toml").exists());
    workspace.file("two.in", b"2");
    success(workspace.run(
        "problems/A/nested",
        &["test", "register", "../tests/two.in"],
    ));
    workspace.file("three.in", b"3");
    workspace.file("reference.txt", b"answer");
    let path = workspace.path("problems/A/tests/three.in");
    success(workspace.test(&[
        "register",
        path.to_str().unwrap(),
        "--answer",
        "tests/reference.txt",
        "--id",
        "third",
    ]));
    assert!(
        String::from_utf8(workspace.catalog_bytes())
            .unwrap()
            .contains("tests/reference.txt")
    );
}

#[test]
fn enable_disable_find_ids_after_reordering_and_removed_id_changes_nothing() {
    let workspace = Workspace::new();
    let one = entry("one", "one.in", None, true);
    let two = entry("two", "two.in", None, true);
    workspace.catalog(&(one.clone() + &two));
    success(workspace.test(&["disable", "one"]));
    // The editor rearranges the entries between commands.
    let reordered = two + &one.replace("enabled = true", "enabled = false");
    workspace.catalog(&reordered);
    success(workspace.test(&["enable", "one"]));
    assert_eq!(
        workspace.catalog_bytes(),
        reordered
            .replace("enabled = false", "enabled = true")
            .as_bytes()
    );
    workspace.catalog(&entry("two", "two.in", None, true));
    let before = workspace.catalog_bytes();
    failure(
        workspace.test(&["disable", "one"]),
        &["unknown registered test id", "one"],
    );
    assert_eq!(workspace.catalog_bytes(), before);
}

#[test]
fn no_op_enable_disable_do_not_rewrite_bytes_timestamp_or_identity() {
    let workspace = Workspace::new();
    for (enabled, command) in [(true, "enable"), (false, "disable")] {
        workspace.catalog(
            &("# keep exact spelling\nschema=1\n".to_owned()
                + &entry("one", "one.in", None, enabled)),
        );
        let path = workspace.path("problems/A/tests.toml");
        let before = workspace.catalog_bytes();
        let metadata = fs::metadata(&path).unwrap();
        success(workspace.test(&[command, "one"]));
        assert_eq!(workspace.catalog_bytes(), before);
        let after = fs::metadata(&path).unwrap();
        assert_eq!(after.modified().unwrap(), metadata.modified().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!((after.dev(), after.ino()), (metadata.dev(), metadata.ino()));
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TreeEntry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

fn tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, relative: &Path, output: &mut BTreeMap<PathBuf, TreeEntry>) {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).unwrap();
        let entry = if metadata.file_type().is_symlink() {
            TreeEntry::Link(fs::read_link(&path).unwrap())
        } else if metadata.is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                visit(root, &relative.join(entry.unwrap().file_name()), output);
            }
            TreeEntry::Directory
        } else {
            TreeEntry::File(fs::read(&path).unwrap())
        };
        output.insert(relative.to_path_buf(), entry);
    }
    let mut output = BTreeMap::new();
    visit(root, Path::new(""), &mut output);
    output
}

#[test]
fn listing_is_lock_free_leaves_all_bytes_and_leftovers_and_orders_discovery() {
    let workspace = Workspace::new();
    workspace.catalog(&entry("registered", "registered.in", None, false));
    for name in [
        "registered.in",
        "registered.ans",
        "z.in",
        "z.ans",
        "A.in",
        "orphan.ans",
        ".cpf-tmp-ignore.in",
    ] {
        workspace.file(name, b"data");
    }
    fs::create_dir_all(workspace.path("problems/A/tests/nested")).unwrap();
    fs::write(
        workspace.path("problems/A/tests/nested/hidden.in"),
        b"hidden",
    )
    .unwrap();
    fs::create_dir_all(workspace.path(".cpforge/staging/abandoned")).unwrap();
    fs::write(workspace.path(".cpforge/staging/abandoned/file"), b"keep").unwrap();
    workspace.file(".cpf-tmp-0123456789abcdef0123456789abcdef", b"keep");
    let before = tree(&workspace.0);
    let output = success(workspace.test(&["ls"]));
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        text,
        "registered disabled answer absent\ncandidate tests/A.in\ncandidate tests/z.in answer tests/z.ans\nstray tests/orphan.ans\nstray tests/registered.ans\n"
    );
    assert!(output.stderr.is_empty());
    assert_eq!(tree(&workspace.0), before);
    assert!(!workspace.path(".cpforge/lock").exists());
    let lock = cpforge::storage::WorkspaceLock::new(&workspace.0).unwrap();
    let guard = lock.acquire("listing test").unwrap();
    success(workspace.test(&["ls"]));
    drop(guard);
}

#[test]
fn catalog_classifications_refuse_mutations_but_allow_discovery() {
    let workspace = Workspace::new();
    workspace.file("new.in", b"data");
    for (text, reason) in [
        ("schema = [", "malformed"),
        ("[[test]]\nid = 'bad id'", "malformed"),
        ("schema = 2\ntest = false\n", "too_new"),
    ] {
        workspace.catalog(text);
        for args in [
            vec!["register", "tests/new.in"],
            vec!["enable", "new"],
            vec!["disable", "new"],
        ] {
            failure(workspace.test(&args), &[reason, "tests.toml"]);
            assert_eq!(workspace.catalog_bytes(), text.as_bytes());
        }
        let output = workspace.test(&["ls"]);
        assert_eq!(output.status.code(), Some(8), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains(reason));
        assert!(String::from_utf8_lossy(&output.stdout).contains("candidate tests/new.in"));
        assert_eq!(workspace.catalog_bytes(), text.as_bytes());
    }
    fs::remove_file(workspace.path("problems/A/tests.toml")).unwrap();
    fs::create_dir(workspace.path("problems/A/tests.toml")).unwrap();
    failure(
        workspace.test(&["register", "tests/new.in"]),
        &["unreadable"],
    );
    failure(workspace.test(&["enable", "new"]), &["unreadable"]);
    failure(workspace.test(&["disable", "new"]), &["unreadable"]);
    let output = workspace.test(&["ls"]);
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("unreadable"));
    assert!(workspace.path("problems/A/tests.toml").is_dir());
}

#[test]
fn listing_distinguishes_absent_empty_present_and_missing_answers() {
    let workspace = Workspace::new();
    let text = entry("absent", "absent.in", None, true)
        + &entry("empty", "empty.in", Some("empty.ans"), true)
        + &entry("present", "present.in", Some("present.ans"), false)
        + &entry("missing", "missing.in", Some("missing.ans"), true);
    workspace.catalog(&text);
    for name in [
        "absent.in",
        "empty.in",
        "present.in",
        "missing.in",
        "absent.ans",
    ] {
        workspace.file(name, b"data");
    }
    workspace.file("empty.ans", b"");
    workspace.file("present.ans", b"answer");
    let output = success(workspace.test(&["ls"]));
    let text = String::from_utf8(output.stdout).unwrap();
    for line in [
        "absent enabled answer absent",
        "empty enabled answer empty",
        "present disabled answer present",
        "missing enabled answer present; tests/missing.ans: missing",
        "stray tests/absent.ans",
    ] {
        assert!(text.contains(line), "{text}");
    }
}

#[test]
fn unregistered_candidates_are_never_enabled() {
    let workspace = Workspace::new();
    workspace.file("new.in", b"data");
    failure(
        workspace.test(&["enable", "new"]),
        &["registered", "register"],
    );
    assert!(!workspace.path("problems/A/tests.toml").exists());
    failure(workspace.run("", &["test", "ls"]), &["NoSelection"]);
    failure(
        workspace.test(&["ls", "--problem", "missing"]),
        &["not found"],
    );
    assert_eq!(
        workspace
            .test(&[
                "register",
                "tests/new.in",
                "--answer",
                "tests/new.in",
                "--no-answer"
            ])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn missing_tests_directory_is_empty_but_a_file_in_its_place_is_an_error() {
    let workspace = Workspace::new();
    let path = workspace.path("problems/A/tests");
    fs::remove_dir(&path).unwrap();
    let output = success(workspace.test(&["ls"]));
    assert!(output.stdout.is_empty());
    fs::write(&path, b"not a directory").unwrap();
    failure(workspace.test(&["ls"]), &["tests"]);
}

#[test]
fn comments_and_unknown_fields_survive_register_and_enable() {
    let workspace = Workspace::new();
    let original = "# header\nschema = 1 # version\nfuture = 'keep'\n\n[[test]] # first\nid = 'first' # stable\ninput = 'tests/first.in'\norigin = 'imported'\nenabled = false # toggle\ncustom  =  [ 1,  2, ] # keep this\n";
    workspace.catalog(original);
    workspace.file("second.in", b"data");
    success(workspace.test(&["register", "tests/second.in"]));
    let registered = String::from_utf8(workspace.catalog_bytes()).unwrap();
    assert!(registered.starts_with(original), "{registered}");
    success(workspace.test(&["enable", "first"]));
    assert_eq!(
        String::from_utf8(workspace.catalog_bytes()).unwrap(),
        registered.replace("enabled = false # toggle", "enabled = true # toggle")
    );
}

#[cfg(unix)]
#[test]
fn fifo_input_is_refused_without_waiting_and_links_to_regular_data_are_allowed() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    workspace.file("one.in", b"original");
    assert!(
        fs::metadata(workspace.path("problems/A/tests/one.in"))
            .unwrap()
            .is_file()
    );
    fs::remove_file(workspace.path("problems/A/tests/one.in")).unwrap();
    let fifo = CString::new(
        workspace
            .path("problems/A/tests/one.in")
            .as_os_str()
            .as_bytes(),
    )
    .unwrap();
    // SAFETY: fifo is a live NUL-terminated path; mkfifo retains no pointers.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    failure(
        workspace.test(&["register", "tests/one.in"]),
        &["not a regular file", "FIFO"],
    );
    assert!(!workspace.path("problems/A/tests.toml").exists());
    workspace.catalog(&entry("one", "one.in", None, false));
    let output = success(workspace.test(&["ls"]));
    assert!(String::from_utf8_lossy(&output.stdout).contains("not a regular file (FIFO)"));
    workspace.file("real.in", b"data");
    symlink("real.in", workspace.path("problems/A/tests/link.in")).unwrap();
    success(workspace.test(&["register", "tests/link.in"]));
}

#[test]
fn listing_uses_saved_cwd_and_flag_selection_in_the_shared_precedence_order() {
    let workspace = Workspace::new();
    workspace.file("from_a.in", b"data");
    fs::write(workspace.path("problems/B/tests/from_b.in"), b"data").unwrap();
    fs::create_dir(workspace.path(".cpforge")).unwrap();
    fs::write(
        workspace.path(".cpforge/state.toml"),
        "schema = 1\ncurrent_problem = 'local:B'\n",
    )
    .unwrap();
    assert_eq!(
        success(workspace.run("", &["test", "ls"])).stdout,
        b"candidate tests/from_b.in\n"
    );
    assert_eq!(
        success(workspace.test(&["ls"])).stdout,
        b"candidate tests/from_a.in\n"
    );
    fs::write(workspace.path(".cpforge/state.toml"), "broken = [").unwrap();
    assert_eq!(
        success(workspace.test(&["ls", "--problem", "B"])).stdout,
        b"candidate tests/from_b.in\n"
    );
    assert!(!workspace.path(".cpforge/lock").exists());
}
