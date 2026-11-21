//! Workspace inspection and housekeeping through the portable CLI.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cpforge::config::{ConfigRequest, Environment, load_config};
use cpforge::jobs::CaptureCheck;
use cpforge::storage::facts::FactRecord;
use cpforge::storage::facts::fingerprint::{SourceFile, source_fingerprint, tests_fingerprint};
use cpforge::storage::inputs::FrozenFile;
use cpforge::storage::job::{Epoch, JobId, JobLease, JobOwner};
use cpforge::tests_store::{FrozenAnswer, FrozenTest, Origin, TestSettings};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("workspace-fixtures")
            .join(format!("{:032x}", u128::from_le_bytes(random)));
        fs::create_dir_all(&root).unwrap();
        let root = cpforge::storage::paths::plain(&fs::canonicalize(root).unwrap());
        let workspace = Self(root);
        fs::create_dir_all(workspace.path(&["user"])).unwrap();
        workspace.ok(&["init"]);
        workspace.write(
            &["cpforge.toml"],
            "schema = 1\ndefault_language = 'python'\n",
        );
        workspace.problem("A");
        workspace.write(
            &[".cpforge", "state.toml"],
            "schema = 1\ncurrent_problem = 'local:A'\n",
        );
        workspace
    }
    fn path(&self, parts: &[&str]) -> PathBuf {
        parts
            .iter()
            .fold(self.0.clone(), |path, part| path.join(part))
    }
    fn write(&self, parts: &[&str], bytes: impl AsRef<[u8]>) {
        let path = self.path(parts);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
        command
            .current_dir(&self.0)
            .env("HOME", self.path(&["user"]))
            .env("USERPROFILE", self.path(&["user"]))
            .env("XDG_CONFIG_HOME", self.path(&["user"]))
            .env("APPDATA", self.path(&["user"]))
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        output
    }
    fn problem(&self, name: &str) {
        self.write(
            &["problems", name, "problem.toml"],
            format!(
                "schema = 1\n[problem]\nid = 'local:{name}'\ntags = ['dp']\nstatus = 'solved'\n"
            ),
        );
        self.write(&["problems", name, "main.py"], "print(input())\n");
        self.write(&["problems", name, "tests", "one.in"], "hello\n");
        self.write(&["problems", name, "tests.toml"], "schema = 1\n[[test]]\nid = 'one'\ninput = 'tests/one.in'\norigin = 'custom'\nenabled = true\n");
    }
    fn fact(&self) {
        let dir = self.path(&["problems", "A"]);
        let target = load_config(&ConfigRequest {
            environment: Environment {
                home: Some(self.path(&["user"])),
                xdg_config_home: Some(self.path(&["user"]).into_os_string()),
                appdata: Some(self.path(&["user"]).into_os_string()),
            },
            current_dir: self.0.clone(),
            workspace_root: Some(self.0.clone()),
            problem_file: Some(dir.join("problem.toml")),
            ..ConfigRequest::default()
        })
        .target()
        .unwrap();
        let source = source_fingerprint(
            &[SourceFile {
                problem_path: PathBuf::from("main.py"),
                content_path: dir.join("main.py"),
            }],
            &target,
        )
        .unwrap();
        let tests = tests_fingerprint(&[FrozenTest {
            id: cpforge::names::Identifier::new("one").unwrap(),
            origin: Origin::Custom,
            input: FrozenFile {
                path: dir.join("tests").join("one.in"),
                len: 6,
                sha256: [0; 32],
            },
            answer: FrozenAnswer::Absent,
            settings: TestSettings::default(),
        }])
        .unwrap();
        let record = FactRecord {
            epoch: Epoch::parse("0123456789abcdef0123456789abcdef").unwrap(),
            n: 1,
            job: JobId::parse("20261120230104123-0000000000000001").unwrap(),
            finished_at: "2026-11-20T23:01:04.123Z".into(),
            verdict: cpforge::report::JobVerdict::Unchecked,
            source,
            tests,
            capture_check: CaptureCheck::Checked,
        };
        self.write(
            &[".cpforge", "facts", "A", "solution.toml"],
            record.encode(),
        );
    }
    fn status(&self) -> serde_json::Value {
        let output = self.ok(&["status", "--json"]);
        assert!(
            output
                .stdout
                .starts_with(b"{\"schema\":1,\"document\":\"status\",")
        );
        assert!(output.stdout.ends_with(b"}\n"));
        assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                let relative = path.strip_prefix(root).unwrap().to_owned();
                if metadata.is_dir() {
                    out.insert(relative, Vec::new());
                    visit(root, &path, out);
                } else if relative == Path::new(".cpforge").join("lock")
                    || (relative.starts_with(Path::new(".cpforge").join("runs"))
                        && relative.file_name() == Some(std::ffi::OsStr::new("lease")))
                {
                    // Windows refuses reads of locked byte ranges. Preserve
                    // the workspace lock and lease entries in the snapshot.
                    out.insert(relative, Vec::new());
                } else {
                    out.insert(relative, fs::read(path).unwrap());
                }
            }
        }
        let mut snapshot = BTreeMap::new();
        visit(&self.0, &self.0, &mut snapshot);
        snapshot
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn comparison(value: &serde_json::Value) -> &str {
    value["problems"][0]["targets"][0]["comparison"]
        .as_str()
        .unwrap()
}
fn fact_state(value: &serde_json::Value) -> &str {
    value["problems"][0]["targets"][0]["fact_state"]
        .as_str()
        .unwrap()
}

#[test]
fn status_compares_current_inputs_and_keeps_tags_separate() {
    let workspace = Workspace::new();
    assert_eq!(fact_state(&workspace.status()), "missing");
    workspace.fact();
    let value = workspace.status();
    assert_eq!(comparison(&value), "unchanged");
    assert_eq!(value["problems"][0]["tags"][0], "dp");
    assert_eq!(value["problems"][0]["user_status"], "solved");
    assert_eq!(
        value["problems"][0]["targets"][0]["last_verdict"],
        "unchecked"
    );
    assert_eq!(
        value["problems"][0]["targets"][0]["finished_at"],
        "2026-11-20T23:01:04.123Z"
    );
    workspace.write(&["problems", "A", "main.py"], "print('changed')\n");
    assert_eq!(comparison(&workspace.status()), "source_changed");
    workspace.write(&["problems", "A", "main.py"], "print(input())\n");
    workspace.write(&["problems", "A", "tests", "two.in"], "two\n");
    let catalog = workspace.path(&["problems", "A", "tests.toml"]);
    let original = fs::read_to_string(&catalog).unwrap();
    fs::write(
        &catalog,
        format!(
            "{original}\n[[test]]\nid='two'\ninput='tests/two.in'\norigin='custom'\nenabled=true\n"
        ),
    )
    .unwrap();
    assert_eq!(comparison(&workspace.status()), "tests_changed");
    workspace.write(&["problems", "A", "main.py"], "print('changed')\n");
    assert_eq!(comparison(&workspace.status()), "source_and_tests_changed");
}

#[cfg(unix)]
#[test]
fn status_matches_a_real_published_test_fact() {
    let workspace = Workspace::new();
    workspace.write(&["cpforge.toml"], "schema=1\ndefault_language='python'\n[languages.python.run]\nargv=['/bin/cat', '{source}']\n");
    // No answer: the completed job publishes an unchecked verdict (exit 4).
    let output = workspace.run(&["test"]);
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    assert_eq!(comparison(&workspace.status()), "unchanged");
    workspace.write(&["problems", "A", "main.py"], "changed\n");
    assert_eq!(comparison(&workspace.status()), "source_changed");
}

#[test]
fn status_classifies_bad_facts_and_reports_missing_and_untracked_files() {
    let workspace = Workspace::new();
    workspace.write(
        &[".cpforge", "facts", "A", "solution.toml"],
        "schema=1\nn='01'\n",
    );
    assert_eq!(fact_state(&workspace.status()), "malformed");
    workspace.write(
        &[".cpforge", "facts", "A", "solution.toml"],
        "schema=2\nn=[]\n",
    );
    assert_eq!(fact_state(&workspace.status()), "too_new");
    fs::remove_file(workspace.path(&[".cpforge", "facts", "A", "solution.toml"])).unwrap();
    fs::create_dir(workspace.path(&[".cpforge", "facts", "A", "solution.toml"])).unwrap();
    assert_eq!(fact_state(&workspace.status()), "unreadable");
    fs::remove_file(workspace.path(&["problems", "A", "main.py"])).unwrap();
    workspace.write(&["problems", "A", "notes.txt"], "mine");
    let value = workspace.status();
    assert_eq!(
        value["problems"][0]["missing_registered"][0]["utf8"],
        "main.py"
    );
    assert_eq!(value["problems"][0]["untracked"][0]["utf8"], "notes.txt");
}

#[test]
fn status_is_read_only_even_while_the_lock_is_held() {
    let workspace = Workspace::new();
    workspace.fact();
    let runs = workspace.path(&[".cpforge", "runs"]);
    fs::create_dir(runs.join("20261120230104123-0000000000000001")).unwrap();
    workspace.write(&[".cpforge", "cache", "entry"], "cache");
    let lock = cpforge::storage::WorkspaceLock::new(&workspace.0).unwrap();
    let guard = lock.acquire("reader test").unwrap();
    // Planted after the fixture's own recovery: status must not recover it.
    workspace.write(
        &[".cpforge", ".cpf-tmp-00000000000000000000000000000000"],
        "leftover",
    );
    let before = workspace.snapshot();
    workspace.ok(&["status"]);
    workspace.status();
    assert_eq!(workspace.snapshot(), before);
    drop(guard);
    drop(lock);
    fs::remove_file(workspace.path(&[".cpforge", "lock"])).unwrap();
    let before = workspace.snapshot();
    workspace.ok(&["status"]);
    workspace.status();
    assert_eq!(workspace.snapshot(), before);
}

#[test]
fn clean_plan_takes_the_lock_and_recovers_but_removes_nothing() {
    let workspace = Workspace::new();
    let runs = workspace.path(&[".cpforge", "runs"]);
    let finished = runs.join("20261120230104123-0000000000000001");
    fs::create_dir(&finished).unwrap();
    fs::write(finished.join("job.toml"), "malformed [").unwrap();
    workspace.write(&[".cpforge", "cache", "entry"], "cache");
    let leftover = workspace.path(&[".cpforge", ".cpf-tmp-00000000000000000000000000000000"]);
    fs::write(&leftover, "leftover").unwrap();
    let output = workspace.ok(&["clean"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("would remove .cpforge/runs/20261120230104123-0000000000000001/"),
        "{stdout}"
    );
    assert!(stdout.contains("would remove .cpforge/cache/"), "{stdout}");
    // The plan performs the universal leftover recovery of the lock...
    assert!(!leftover.exists());
    // ...but selects and renames nothing.
    let before = workspace.snapshot();
    workspace.ok(&["clean"]);
    assert_eq!(workspace.snapshot(), before);
    assert_eq!(fs::read(finished.join("job.toml")).unwrap(), b"malformed [");
    assert!(workspace.path(&[".cpforge", "cache", "entry"]).is_file());
}

#[test]
fn clean_apply_removes_only_inactive_jobs_and_cache() {
    let workspace = Workspace::new();
    workspace.fact();
    let runs = workspace.path(&[".cpforge", "runs"]);
    let finished = runs.join("20261120230104123-0000000000000001");
    fs::create_dir(&finished).unwrap();
    fs::write(finished.join("job.toml"), "malformed [").unwrap();
    let live = runs.join("20261120230104123-0000000000000002");
    fs::create_dir(&live).unwrap();
    let owner: JobOwner = JobLease::create_and_lock(live.clone()).unwrap();
    workspace.write(&[".cpforge", "cache", "entry"], "cache");
    let protected = [
        vec![".cpforge", "facts", "A", "solution.toml"],
        vec![".cpforge", "job-seq"],
        vec![".cpforge", "lock"],
        vec![".cpforge", "state.toml"],
        vec!["problems", "A", "main.py"],
    ];
    let before: Vec<_> = protected
        .iter()
        .map(|p| fs::read(workspace.path(p)).unwrap())
        .collect();
    workspace.ok(&["clean", "--apply"]);
    assert!(!finished.exists());
    assert!(live.is_dir());
    assert!(!workspace.path(&[".cpforge", "cache"]).exists());
    for (path, bytes) in protected.iter().zip(before) {
        assert_eq!(fs::read(workspace.path(path)).unwrap(), bytes);
    }
    drop(owner);
    workspace.ok(&["clean", "--apply"]);
    assert!(!live.exists());
}

#[test]
fn gitignore_print_create_replace_append_and_refuse_bad_markers() {
    let workspace = Workspace::new();
    let patterns =
        b".cpforge/runs/\n.cpforge/cache/\n.cpforge/staging/\n# optional: .cpforge/facts/\n";
    let before = workspace.snapshot();
    assert_eq!(workspace.ok(&["gitignore"]).stdout, patterns);
    assert_eq!(workspace.snapshot(), before);
    workspace.ok(&["gitignore", "--write"]);
    let block = fs::read(workspace.path(&[".gitignore"])).unwrap();
    assert_eq!(
        block,
        [b"# >>> cpforge\n".as_slice(), patterns, b"# <<< cpforge\n"].concat()
    );
    let original = b"user\r\n# >>> cpforge\r\nold\n# <<< cpforge\r\ntail-without-newline";
    workspace.write(&[".gitignore"], original);
    workspace.ok(&["gitignore", "--write"]);
    let expected = [
        b"user\r\n# >>> cpforge\r\n".as_slice(),
        patterns,
        b"# <<< cpforge\r\ntail-without-newline",
    ]
    .concat();
    assert_eq!(fs::read(workspace.path(&[".gitignore"])).unwrap(), expected);
    workspace.ok(&["gitignore", "--write"]);
    assert_eq!(fs::read(workspace.path(&[".gitignore"])).unwrap(), expected);
    workspace.write(&[".gitignore"], b"user-no-newline");
    workspace.ok(&["gitignore", "--write"]);
    assert_eq!(
        fs::read(workspace.path(&[".gitignore"])).unwrap(),
        [b"user-no-newline\n".as_slice(), &block].concat()
    );
    for bytes in [
        "# >>> cpforge\n",
        "# <<< cpforge\n# >>> cpforge\n",
        "# >>> cpforge\n# >>> cpforge\n# <<< cpforge\n",
    ] {
        workspace.write(&[".gitignore"], bytes);
        assert_eq!(
            workspace.run(&["gitignore", "--write"]).status.code(),
            Some(8)
        );
        assert_eq!(
            fs::read(workspace.path(&[".gitignore"])).unwrap(),
            bytes.as_bytes()
        );
    }
}

#[test]
fn selection_and_error_documents_and_flag_availability() {
    let workspace = Workspace::new();
    workspace.problem("B");
    workspace.write(
        &["contests", "round", "contest.toml"],
        "schema=1\n[[problem]]\nid='local:B'\n[[problem]]\nid='local:A'\n",
    );
    let output = workspace.ok(&["status", "round", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["problems"][0]["problem"], "local:B");
    assert_eq!(value["problems"][1]["problem"], "local:A");
    let output = workspace.ok(&["status", "contest:round", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["problems"].as_array().unwrap().len(), 2);
    // A problem name wins over a contest of the same name.
    workspace.write(
        &["contests", "A", "contest.toml"],
        "schema=1\n[[problem]]\nid='local:B'\n",
    );
    let output = workspace.ok(&["status", "A", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["problems"][0]["problem"], "local:A");
    assert_eq!(workspace.run(&["status", "nothing"]).status.code(), Some(8));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &workspace.ok(&["status", "--all", "--json"]).stdout
        )
        .unwrap()["problems"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let output = workspace.run(&["status", "local:missing", "--json"]);
    assert_eq!(output.status.code(), Some(8));
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["document"], "error");
    assert_eq!(error["command"], "status");
    assert_eq!(error["exit"], 8);
    for args in [
        vec!["clean", "--json"],
        vec!["gitignore", "--json"],
        vec!["status", "A", "--all"],
        vec!["status", "--events"],
        vec!["status", "A", "--problem", "B"],
    ] {
        assert_eq!(workspace.run(&args).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn clean_skips_newer_schema_and_reports_rename_failure_as_partial() {
    let workspace = Workspace::new();
    let runs = workspace.path(&[".cpforge", "runs"]);
    let protected = runs.join("20261120230104123-0000000000000001");
    fs::create_dir(&protected).unwrap();
    fs::write(protected.join("job.toml"), "schema=2\n").unwrap();
    let conflict = runs.join("20261120230104123-0000000000000002");
    let reclaim = runs.join(".reclaim-20261120230104123-0000000000000002");
    fs::create_dir(&conflict).unwrap();
    fs::create_dir(&reclaim).unwrap();
    let output = workspace.run(&["clean", "--apply"]);
    assert_eq!(output.status.code(), Some(9), "{output:?}");
    assert!(protected.is_dir());
    assert!(conflict.is_dir());
    assert!(!reclaim.exists());
    workspace.ok(&["clean", "--apply"]);
    assert!(protected.is_dir());
    assert!(!conflict.exists());
}

#[test]
fn visibility_and_provenance_hide_only_tracked_or_ignored_paths() {
    let workspace = Workspace::new();
    workspace.write(
        &["cpforge.toml"],
        "schema=1\ndefault_language='python'\n[workspace]\nignore=['*.swp', 'scratch/**']\n",
    );
    workspace.write(&["problems", "A", "problem.toml"], "schema=1\n[problem]\nid='local:A'\n[files]\ncreated=[{path='generated.txt',command='add'}]\nuser=['adopted.txt']\n");
    for name in ["generated.txt", "adopted.txt", "visible.txt", "hidden.swp"] {
        workspace.write(&["problems", "A", name], "kept");
    }
    workspace.write(
        &["problems", "A", "scratch", "nested", "hidden.txt"],
        "kept",
    );
    let before = workspace.snapshot();
    let value = workspace.status();
    assert_eq!(
        value["problems"][0]["untracked"].as_array().unwrap().len(),
        1
    );
    assert_eq!(value["problems"][0]["untracked"][0]["utf8"], "visible.txt");
    assert_eq!(workspace.snapshot(), before);
}

#[test]
fn gitignore_refuses_multiple_hard_links_and_nonregular_files() {
    let workspace = Workspace::new();
    workspace.write(&["user", "ignore"], "user text\n");
    let path = workspace.path(&[".gitignore"]);
    fs::hard_link(workspace.path(&["user", "ignore"]), &path).unwrap();
    assert_eq!(
        workspace.run(&["gitignore", "--write"]).status.code(),
        Some(8)
    );
    assert_eq!(fs::read(&path).unwrap(), b"user text\n");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        workspace.run(&["gitignore", "--write"]).status.code(),
        Some(8)
    );
    fs::remove_dir(&path).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(workspace.path(&["user", "ignore"]), &path).unwrap();
        assert_eq!(
            workspace.run(&["gitignore", "--write"]).status.code(),
            Some(8)
        );
        assert_eq!(
            fs::read(workspace.path(&["user", "ignore"])).unwrap(),
            b"user text\n"
        );
    }
}

// Unix: Rust's stdout treats EBADF as success, so a read-only descriptor
// would not fail; a stdout closed at startup is the undeliverable case.
#[cfg(unix)]
#[test]
fn status_json_delivery_failure_exits_eleven() {
    use std::os::unix::process::CommandExt;
    let workspace = Workspace::new();
    let mut command = workspace.command();
    command.args(["status", "--json"]);
    // SAFETY: only closes the child's stdout before exec; cpf repairs it at startup.
    unsafe {
        command.pre_exec(|| {
            libc::close(1);
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(11), "{output:?}");
}

#[cfg(windows)]
#[test]
fn status_json_delivery_failure_exits_eleven() {
    let workspace = Workspace::new();
    workspace.write(&["read-only-output"], "kept");
    let output = workspace
        .command()
        .args(["status", "--json"])
        .stdout(fs::File::open(workspace.path(&["read-only-output"])).unwrap())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(11), "{output:?}");
    assert_eq!(
        fs::read(workspace.path(&["read-only-output"])).unwrap(),
        b"kept"
    );
}

#[cfg(unix)]
#[test]
fn status_does_not_traverse_untracked_directory_links() {
    let workspace = Workspace::new();
    workspace.write(&["user", "hidden", "note.txt"], "user file");
    std::os::unix::fs::symlink(
        workspace.path(&["user", "hidden"]),
        workspace.path(&["problems", "A", "linked"]),
    )
    .unwrap();
    let value = workspace.status();
    assert_eq!(
        value["problems"][0]["untracked"].as_array().unwrap().len(),
        1
    );
    assert_eq!(value["problems"][0]["untracked"][0]["utf8"], "linked");
}

#[test]
fn adopt_hides_an_untracked_file_without_changing_its_bytes_and_is_idempotent() {
    let workspace = Workspace::new();
    workspace.write(&["problems", "A", "notes", "draft.txt"], b"my notes\r\n\0");
    let untracked = &workspace.status()["problems"][0]["untracked"];
    assert_eq!(untracked[0]["utf8"], "notes/draft.txt");
    let record = workspace.path(&["problems", "A", "problem.toml"]);
    let original = fs::read_to_string(&record).unwrap();
    fs::write(
        &record,
        format!("# keep this comment\n{original}\n[extra]\nvalue = 17 # keep this too\n"),
    )
    .unwrap();
    workspace.ok(&["adopt", "notes/draft.txt"]);
    assert_eq!(
        fs::read(workspace.path(&["problems", "A", "notes", "draft.txt"])).unwrap(),
        b"my notes\r\n\0"
    );
    assert!(
        workspace.status()["problems"][0]["untracked"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let text = fs::read_to_string(&record).unwrap();
    assert!(text.contains("# keep this comment"));
    assert!(text.contains("value = 17 # keep this too"));
    let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        doc["files"]["user"]
            .as_array()
            .unwrap()
            .get(0)
            .unwrap()
            .as_str(),
        Some("notes/draft.txt")
    );
    let before = workspace.snapshot();
    workspace.ok(&["adopt", "notes/draft.txt"]);
    assert_eq!(workspace.snapshot(), before);
}

#[test]
fn adopt_uses_problem_relative_paths_and_explicit_selection() {
    let workspace = Workspace::new();
    workspace.problem("B");
    workspace.write(&["problems", "B", "notes.txt"], "B notes");
    let output = workspace
        .command()
        .current_dir(workspace.path(&["problems", "A", "tests"]))
        .args(["--problem", "B", "adopt", "notes.txt"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let doc = fs::read_to_string(workspace.path(&["problems", "B", "problem.toml"])).unwrap();
    assert!(doc.contains("notes.txt"));
    assert!(
        !fs::read_to_string(workspace.path(&["problems", "A", "problem.toml"]))
            .unwrap()
            .contains("notes.txt")
    );
}

#[test]
fn adopt_refuses_outside_reserved_missing_nonregular_registered_and_colliding_paths() {
    let workspace = Workspace::new();
    workspace.write(&["outside.txt"], "outside");
    workspace.write(&[".cpforge", "cache", "entry"], "cache");
    workspace.write(&["problems", "A", "tests", "ONE.in"], "collision");
    let before = workspace.snapshot();
    for path in [
        "../outside.txt",
        "../../.cpforge/cache/entry",
        ".cpforge/runs/job",
        "absent.txt",
        "tests",
        "problem.toml",
        "tests.toml",
        "main.py",
        "tests/one.in",
        "tests/ONE.in",
        "/outside.txt",
        "./main.py",
        "bad/../main.py",
        ".cpf-tmp-file",
        "NUL.txt",
    ] {
        let output = workspace.run(&["adopt", path]);
        assert_eq!(output.status.code(), Some(8), "{path}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(workspace.snapshot(), before, "{path}");
    }
    let absolute = workspace.path(&["outside.txt"]);
    let output = workspace
        .command()
        .arg("adopt")
        .arg(&absolute)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    assert_eq!(workspace.snapshot(), before);
}

#[cfg(unix)]
#[test]
fn adopt_and_provenance_refuse_link_components_and_nonregular_files() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    symlink("main.py", workspace.path(&["problems", "A", "link.py"])).unwrap();
    symlink("tests", workspace.path(&["problems", "A", "linked-tests"])).unwrap();
    for path in ["link.py", "linked-tests/one.in"] {
        let output = workspace.run(&["adopt", path]);
        assert_eq!(output.status.code(), Some(8), "{output:?}");
    }
    let fifo = workspace.path(&["problems", "A", "pipe"]);
    use std::os::unix::ffi::OsStrExt;
    let native = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: native is a terminated pathname, with no existing entry.
    assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
    let output = workspace.run(&["adopt", "pipe"]);
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    // A hand edit cannot hide the link from status either.
    let record = workspace.path(&["problems", "A", "problem.toml"]);
    let original = fs::read_to_string(&record).unwrap();
    fs::write(&record, format!("{original}\n[files]\nuser=['link.py']\n")).unwrap();
    let status = workspace.status();
    assert!(
        status["problems"][0]["untracked"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["utf8"] == "link.py")
    );
    assert!(
        status["problems"][0]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.as_str().unwrap().contains("invalid [files]"))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn adopt_refuses_portable_spelling_collisions_at_each_component() {
    let workspace = Workspace::new();
    workspace.write(&["problems", "A", "notes.txt"], "lower");
    workspace.write(&["problems", "A", "NOTES.TXT"], "upper");
    workspace.write(&["problems", "A", "Dir", "file"], "nested");
    fs::create_dir(workspace.path(&["problems", "A", "dir"])).unwrap();
    for path in ["notes.txt", "NOTES.TXT", "Dir/file"] {
        let output = workspace.run(&["adopt", path]);
        assert_eq!(output.status.code(), Some(8), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("collision"));
    }
}

#[test]
fn adopt_refuses_unsafe_record_classifications_and_preserves_records() {
    for text in [
        "[",
        "schema=2\n",
        "schema=0\n",
        "schema='1'\n",
        "[problem]\nid='bad'\n",
        "[problem]\nid='local:A'\n[files]\nuser=17\n",
        "files=17\n[problem]\nid='local:A'\n",
        "files=[]\n[problem]\nid='local:A'\n",
    ] {
        let workspace = Workspace::new();
        workspace.write(&["problems", "A", "notes.txt"], "notes");
        workspace.write(&["problems", "A", "problem.toml"], text);
        let before = workspace.snapshot();
        let output = workspace.run(&["adopt", "notes.txt"]);
        assert_eq!(output.status.code(), Some(8), "{text}: {output:?}");
        assert_eq!(workspace.snapshot(), before);
    }
    let workspace = Workspace::new();
    workspace.write(&["problems", "A", "notes.txt"], "notes");
    workspace.write(&["problems", "A", "tests.toml"], "schema=99\n");
    let before = workspace.snapshot();
    let output = workspace.run(&["adopt", "notes.txt"]);
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    assert_eq!(workspace.snapshot(), before);
}

#[test]
fn ignore_is_an_idempotent_validated_suggestion_and_status_honours_it() {
    let workspace = Workspace::new();
    workspace.write(&["problems", "A", "notes.swp"], "swap");
    workspace.write(&["problems", "A", "nested", "notes.swp"], "nested swap");
    workspace.write(&["problems", "A", "scratch", "deep", "notes"], "scratch");
    workspace.write(&["problems", "A", "notes.txt"], "visible");
    fs::remove_file(workspace.path(&[".cpforge", "lock"])).unwrap();
    let before = workspace.snapshot();
    let first = workspace.ok(&["ignore", "*.swp"]);
    assert_eq!(workspace.ok(&["ignore", "*.swp"]).stdout, first.stdout);
    assert_eq!(workspace.snapshot(), before);
    let doc = String::from_utf8(first.stdout)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    assert_eq!(
        doc["workspace"]["ignore"]
            .as_array()
            .unwrap()
            .get(0)
            .unwrap()
            .as_str(),
        Some("*.swp")
    );
    for pattern in [
        "", "/root", "tail/", "a//b", ".", "..", "a/../b", "ab**", "**b", "[a]", "!no", "a\\b",
    ] {
        let output = workspace.run(&["ignore", pattern]);
        assert_eq!(output.status.code(), Some(8), "{pattern}: {output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(workspace.snapshot(), before);
    }
    for pattern in ["**", "a/**/b", "a?b", "a'b", "a\"b", "é?"] {
        let output = workspace.ok(&["ignore", pattern]);
        let doc = String::from_utf8(output.stdout)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(
            doc["workspace"]["ignore"]
                .as_array()
                .unwrap()
                .get(0)
                .unwrap()
                .as_str(),
            Some(pattern)
        );
    }
    workspace.write(
        &["cpforge.toml"],
        "schema=1\ndefault_language='python'\n[workspace]\nignore=['*.swp','scratch/**']\n",
    );
    let status = workspace.status();
    assert_eq!(
        status["problems"][0]["untracked"].as_array().unwrap().len(),
        1
    );
    assert_eq!(status["problems"][0]["untracked"][0]["utf8"], "notes.txt");
    assert_eq!(
        fs::read(workspace.path(&["problems", "A", "notes.swp"])).unwrap(),
        b"swap"
    );
}

#[test]
fn fsck_reports_all_categories_and_apply_changes_only_authorized_records() {
    let workspace = Workspace::new();
    workspace.fact();
    let valid_fact =
        fs::read(workspace.path(&[".cpforge", "facts", "A", "solution.toml"])).unwrap();
    workspace.write(&[".cpforge", "facts", "A", "other.toml"], &valid_fact);
    workspace.write(&[".cpforge", "facts", "gone", "solution.toml"], &valid_fact);
    workspace.write(
        &[".cpforge", "facts", "gone", "bad.toml"],
        "schema=1\nn='01'\n",
    );
    workspace.write(
        &[".cpforge", "facts", "gone", "new.toml"],
        "schema=99\nn=[]\n",
    );
    fs::create_dir(workspace.path(&[".cpforge", "facts", "gone", "unreadable.toml"])).unwrap();
    workspace.write(
        &[".cpforge", "facts", "A", "unexpected"],
        "keep fact-tree entry",
    );
    workspace.write(&[".cpforge", "unexpected"], "keep reserved entry");
    workspace.write(&[".cpforge", "runs", "unexpected"], "keep run-tree entry");
    workspace.write(&["contests", "round", "contest.toml"], "# preserve contest comment\nschema=1\nfuture='keep'\n[[problem]]\nid='local:gone'\nlabel='A'\n[[problem]]\nid='local:A'\nlabel='B'\nfuture={limit=3}\n[[problem]]\nid='local:broken'\nlabel='C'\n");
    workspace.write(
        &["problems", "broken", "notes.txt"],
        "directory without metadata",
    );
    workspace.write(&["problems", "future", "problem.toml"], "schema=99\n");
    workspace.write(
        &["problems", "malformed", "problem.toml"],
        "[problem]\nid=17\n",
    );
    workspace.write(&["problems", "A", "notes.txt"], "overlap");
    let record = workspace.path(&["problems", "A", "problem.toml"]);
    let original = fs::read_to_string(&record).unwrap();
    fs::write(&record, format!("{original}\n[files]\ncreated=[{{path='notes.txt',command='add'}}]\nuser=['notes.txt','main.py','tests/one.in','../outside','absent']\n")).unwrap();
    let before = workspace.snapshot();
    let output = workspace.ok(&["fsck"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    for finding in [
        "dangling contest reference: local:gone",
        "problem directory without problem.toml",
        "too_new: schema 99",
        "malformed:",
        "provenance overlap:",
        "invalid [files] user entry",
        "fact for deleted problem",
        "fact for unknown target",
        "unexpected entry",
        "job-seq",
        "valid (schema 1)",
        "unreadable:",
    ] {
        assert!(stdout.contains(finding), "missing {finding}: {stdout}");
    }
    assert_eq!(workspace.snapshot(), before);
    workspace.ok(&["fsck", "--apply"]);
    let after = workspace.snapshot();
    let contest = Path::new("contests").join("round").join("contest.toml");
    let removed = [
        Path::new(".cpforge")
            .join("facts")
            .join("gone")
            .join("solution.toml"),
        Path::new(".cpforge")
            .join("facts")
            .join("gone")
            .join("bad.toml"),
    ];
    for (path, bytes) in &before {
        if removed.contains(path) {
            assert!(!after.contains_key(path));
        } else if path != &contest {
            assert_eq!(after.get(path), Some(bytes), "{}", path.display());
        }
    }
    assert_eq!(after.len(), before.len() - removed.len());
    let text = fs::read_to_string(workspace.0.join(&contest)).unwrap();
    assert!(text.contains("# preserve contest comment"));
    assert!(text.contains("future='keep'"));
    assert!(text.contains("future={limit=3}"));
    let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
    let members = doc["problem"].as_array_of_tables().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members.get(0).unwrap()["id"].as_str(), Some("local:A"));
    assert_eq!(members.get(0).unwrap()["label"].as_str(), Some("B"));
    assert_eq!(members.get(1).unwrap()["id"].as_str(), Some("local:broken"));
    let before = workspace.snapshot();
    workspace.ok(&["fsck", "--apply"]);
    assert_eq!(workspace.snapshot(), before);
}

#[test]
fn fsck_repairs_only_missing_or_malformed_sequences_with_a_fresh_epoch() {
    use cpforge::storage::{Record, job_seq::read_job_sequence};
    for malformed in [
        None,
        Some("schema=1\nepoch='bad'\nnext=0\n"),
        Some("schema=0\n"),
        Some("schema='one'\n"),
    ] {
        let workspace = Workspace::new();
        let sequence = workspace.path(&[".cpforge", "job-seq"]);
        let Record::Valid(old) = read_job_sequence(&sequence) else {
            panic!("initial sequence")
        };
        if let Some(text) = malformed {
            fs::write(&sequence, text).unwrap();
        } else {
            fs::remove_file(&sequence).unwrap();
        }
        let before = workspace.snapshot();
        workspace.ok(&["fsck"]);
        assert_eq!(workspace.snapshot(), before);
        workspace.ok(&["fsck", "--apply"]);
        let Record::Valid(new) = read_job_sequence(&sequence) else {
            panic!("repaired sequence")
        };
        assert_ne!(new.epoch, old.epoch);
        assert_eq!(new.next, 0);
        let before = workspace.snapshot();
        workspace.ok(&["fsck", "--apply"]);
        assert_eq!(workspace.snapshot(), before);
    }
    for unreadable in [false, true] {
        let workspace = Workspace::new();
        let sequence = workspace.path(&[".cpforge", "job-seq"]);
        fs::remove_file(&sequence).unwrap();
        if unreadable {
            fs::create_dir(&sequence).unwrap();
        } else {
            fs::write(&sequence, "schema=99\n").unwrap();
        }
        let before = workspace.snapshot();
        workspace.ok(&["fsck", "--apply"]);
        assert_eq!(workspace.snapshot(), before);
    }
}

#[test]
fn fsck_sequence_repair_probes_every_directory_and_refuses_held_or_unopenable_leases() {
    for name in [
        "20261120230104123-0000000000000001",
        ".reclaim-interrupted",
        "unexpected-directory",
    ] {
        for held in [false, true] {
            let workspace = Workspace::new();
            let sequence = workspace.path(&[".cpforge", "job-seq"]);
            fs::remove_file(&sequence).unwrap();
            let dir = workspace.path(&[".cpforge", "runs", name]);
            fs::create_dir(&dir).unwrap();
            // Newer job metadata cannot exempt a directory from the probe.
            fs::write(dir.join("job.toml"), "schema=99\n").unwrap();
            let owner = if held {
                Some(JobLease::create_and_lock(dir.clone()).unwrap())
            } else {
                fs::create_dir(dir.join("lease")).unwrap();
                None
            };
            let before = workspace.snapshot();
            let output = workspace.run(&["fsck", "--apply"]);
            assert_eq!(output.status.code(), Some(8), "{name}: {output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("jobs may be live"), "{stderr}");
            assert!(stderr.contains(name), "{stderr}");
            assert_eq!(workspace.snapshot(), before);
            drop(owner);
            if !held {
                fs::remove_dir(dir.join("lease")).unwrap();
            }
            workspace.ok(&["fsck", "--apply"]);
            assert!(sequence.is_file());
        }
    }
}

#[test]
fn fsck_takes_the_workspace_lock_and_never_replaces_it() {
    let workspace = Workspace::new();
    let lock = cpforge::storage::WorkspaceLock::new(&workspace.0).unwrap();
    let guard = lock.acquire("held for fsck test").unwrap();
    let before = workspace.snapshot();
    for args in [vec!["fsck"], vec!["fsck", "--apply"]] {
        let output = workspace.run(&args);
        assert_eq!(output.status.code(), Some(8), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("busy"),
            "{output:?}"
        );
        assert_eq!(workspace.snapshot(), before);
    }
    drop(guard);
    workspace.ok(&["fsck"]);
}

#[test]
fn workspace_record_commands_have_no_machine_document_options() {
    let workspace = Workspace::new();
    let before = workspace.snapshot();
    for args in [
        vec!["adopt", "main.py", "--json"],
        vec!["ignore", "*.swp", "--json"],
        vec!["fsck", "--json"],
        vec!["fsck", "--events"],
    ] {
        let output = workspace.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert_eq!(workspace.snapshot(), before);
    }
}

#[test]
fn fsck_reports_job_record_schemas_and_unexpected_entries_without_removing_jobs() {
    let workspace = Workspace::new();
    let name = "20261120230104123-0000000000000001";
    workspace.write(&[".cpforge", "runs", name, "job.toml"], "schema=99\n");
    workspace.write(
        &[".cpforge", "runs", name, "report.json"],
        r#"{"schema":2}"#,
    );
    workspace.write(
        &[".cpforge", "runs", name, "r00000", "spec.json"],
        r#"{"schema":0}"#,
    );
    workspace.write(
        &[".cpforge", "runs", name, "r00000", "record.json"],
        r#"{"schema":"one"}"#,
    );
    workspace.write(&[".cpforge", "runs", name, "r00000", "extra"], "keep");
    workspace.write(&[".cpforge", "runs", name, "extra"], "keep job entry");
    workspace.write(
        &[".cpforge", "runs", name, "inputs", "extra"],
        "keep inputs entry",
    );
    workspace.write(
        &[".cpforge", "runs", name, "builds", "extra"],
        "keep builds entry",
    );
    let before = workspace.snapshot();
    let output = workspace.ok(&["fsck", "--apply"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    for text in [
        "too_new: schema 99",
        "too_new: schema 2",
        "spec.json: malformed schema",
        "record.json: malformed schema",
        "unexpected entry in run directory",
        "unexpected entry in job directory",
        "unexpected entry in inputs directory",
        "unexpected entry in builds directory",
    ] {
        assert!(stdout.contains(text), "missing {text}: {stdout}");
    }
    assert_eq!(workspace.snapshot(), before);
}

#[test]
fn fsck_preserves_contests_with_unsupported_or_malformed_records() {
    let workspace = Workspace::new();
    for (name, text) in [
        ("future", "schema=99\n[[problem]]\nid='local:gone'\n"),
        (
            "duplicate",
            "schema=1\n[[problem]]\nid='local:gone'\n[[problem]]\nid='local:gone'\n",
        ),
        (
            "bad-label",
            "schema=1\n[[problem]]\nid='local:gone'\nlabel=1\n",
        ),
        ("bad-id", "schema=1\n[[problem]]\nid='not-an-identity'\n"),
    ] {
        workspace.write(&["contests", name, "contest.toml"], text);
    }
    let before = workspace.snapshot();
    workspace.ok(&["fsck", "--apply"]);
    assert_eq!(workspace.snapshot(), before);
}

#[cfg(unix)]
#[test]
fn fsck_never_follows_or_removes_links_in_reserved_trees() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    workspace.write(&["keep.toml"], "schema=1\nbad='fact'\n");
    let facts = workspace.path(&[".cpforge", "facts", "gone"]);
    fs::create_dir_all(&facts).unwrap();
    symlink(workspace.path(&["keep.toml"]), facts.join("solution.toml")).unwrap();
    symlink(
        workspace.path(&["problems", "A"]),
        workspace.path(&[".cpforge", "runs", "linked-job"]),
    )
    .unwrap();
    workspace.ok(&["fsck", "--apply"]);
    assert!(
        fs::symlink_metadata(facts.join("solution.toml"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(workspace.path(&[".cpforge", "runs", "linked-job"]))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(workspace.path(&["keep.toml"])).unwrap(),
        b"schema=1\nbad='fact'\n"
    );
    fs::remove_file(workspace.path(&[".cpforge", "job-seq"])).unwrap();
    let output = workspace.run(&["fsck", "--apply"]);
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("link cannot be probed"));
}

#[test]
fn fsck_both_modes_perform_the_shared_leftover_recovery() {
    for apply in [false, true] {
        let workspace = Workspace::new();
        let temporary = ".cpf-tmp-00000000000000000000000000000000";
        workspace.write(&[".cpforge", temporary], "metadata leftover");
        workspace.write(&[".cpforge", "facts", "A", temporary], "fact leftover");
        workspace.write(
            &[".cpforge", "staging", "old", "content"],
            "staging leftover",
        );
        if apply {
            workspace.ok(&["fsck", "--apply"]);
        } else {
            workspace.ok(&["fsck"]);
        }
        assert!(!workspace.path(&[".cpforge", temporary]).exists());
        assert!(
            !workspace
                .path(&[".cpforge", "facts", "A", temporary])
                .exists()
        );
        assert!(!workspace.path(&[".cpforge", "staging", "old"]).exists());
        assert!(workspace.path(&["problems", "A", "main.py"]).is_file());
    }
}

#[test]
fn adopt_refuses_created_files_and_hardlinked_metadata_without_rewriting_either() {
    let workspace = Workspace::new();
    workspace.write(&["problems", "A", "notes.txt"], "notes");
    let record = workspace.path(&["problems", "A", "problem.toml"]);
    let original = fs::read_to_string(&record).unwrap();
    fs::write(
        &record,
        format!("{original}\n[files]\ncreated=[{{path='notes.txt',command='add'}}]\n"),
    )
    .unwrap();
    let before = workspace.snapshot();
    let output = workspace.run(&["adopt", "notes.txt"]);
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    assert_eq!(workspace.snapshot(), before);
    fs::write(&record, original).unwrap();
    let link = workspace.path(&["problem-record-link"]);
    fs::hard_link(&record, &link).unwrap();
    let before = workspace.snapshot();
    let output = workspace.run(&["adopt", "notes.txt"]);
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    assert_eq!(workspace.snapshot(), before);
}

#[test]
fn concurrent_adoptions_keep_both_entries() {
    use std::process::Stdio;
    let workspace = Workspace::new();
    workspace.write(&["problems", "A", "first.txt"], "first");
    workspace.write(&["problems", "A", "second.txt"], "second");
    let first = workspace
        .command()
        .args(["adopt", "first.txt"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = workspace
        .command()
        .args(["adopt", "second.txt"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    for child in [first, second] {
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
    }
    let text = fs::read_to_string(workspace.path(&["problems", "A", "problem.toml"])).unwrap();
    let document = text.parse::<toml_edit::DocumentMut>().unwrap();
    let adopted = document["files"]["user"].as_array().unwrap();
    assert_eq!(adopted.len(), 2);
    for name in ["first.txt", "second.txt"] {
        assert!(adopted.iter().any(|v| v.as_str() == Some(name)));
    }
    assert!(
        workspace.status()["problems"][0]["untracked"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[cfg(feature = "storage-test-hooks")]
mod record_command_crashes {
    use super::*;
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn run_bounded(mut command: Command) -> Output {
        fn reader(mut stream: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
            let (send, receive) = mpsc::channel();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).unwrap();
                let _ = send.send(bytes);
            });
            receive
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stdout = reader(child.stdout.take().unwrap());
        let stderr = reader(child.stderr.take().unwrap());
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("record command exceeded its deadline");
            }
            std::thread::yield_now();
        };
        Output {
            status,
            stdout: stdout
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap(),
            stderr: stderr
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap(),
        }
    }

    fn abort_at(workspace: &Workspace, args: &[&str], point: &str) -> Output {
        let mut command = workspace.command();
        command.args(args).env("CPF_STORAGE_CRASH_AT", point);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: the child calls only setrlimit before exec.
            unsafe {
                command.pre_exec(|| {
                    let limit = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::setrlimit(libc::RLIMIT_CORE, &limit) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
        }
        #[cfg(windows)]
        {
            static ERROR_MODE: std::sync::Once = std::sync::Once::new();
            ERROR_MODE.call_once(|| {
                // SAFETY: child processes inherit the crash-dialog suppression.
                unsafe {
                    windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                        windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
                    );
                }
            });
        }
        let output = run_bounded(command);
        assert!(!output.status.success(), "{point}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&format!("CPF-CRASH-AT {point}")),
            "{point}: {output:?}"
        );
        output
    }

    #[test]
    fn gitignore_crash_boundaries() {
        let patterns =
            b".cpforge/runs/\n.cpforge/cache/\n.cpforge/staging/\n# optional: .cpforge/facts/\n";
        let block = [b"# >>> cpforge\n".as_slice(), patterns, b"# <<< cpforge\n"].concat();
        let prefix = b"# user rules\r\n\xffcache/\r\n# >>> cpforge\r\n";
        let suffix = b"# <<< cpforge\r\n!keep\r\nlast-without-newline";
        let original = [prefix.as_slice(), b"old-rule\n", suffix].concat();
        let replaced = [prefix.as_slice(), patterns, suffix].concat();
        let append = b"# user rules\r\n\xffcache/\r\nlast-without-newline";
        let appended = [append.as_slice(), b"\n", &block].concat();
        for (old, new) in [
            (None, block.as_slice()),
            (Some(append.as_slice()), appended.as_slice()),
            (Some(original.as_slice()), replaced.as_slice()),
        ] {
            let mut points = vec![("gitignore.locked", false), ("gitignore.published", true)];
            if old.is_some() {
                points.extend([
                    ("atomic_replace.temp_synced", false),
                    ("atomic_replace.renamed", true),
                    ("atomic_replace.directory_synced", true),
                ]);
            } else {
                points.extend([
                    ("file_noclobber.before_publish", false),
                    ("file_noclobber.after_publish", true),
                    ("file_noclobber.directory_synced", true),
                ]);
                #[cfg(target_os = "linux")]
                points.push(("file_noclobber.linked", true));
            }
            for (point, committed) in points {
                let workspace = Workspace::new();
                let path = workspace.path(&[".gitignore"]);
                if let Some(bytes) = old {
                    workspace.write(&[".gitignore"], bytes);
                }
                let output = abort_at(&workspace, &["gitignore", "--write"], point);
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    assert_eq!(
                        output.status.signal(),
                        Some(libc::SIGABRT),
                        "{point}: {output:?}"
                    );
                }
                #[cfg(windows)]
                {
                    let code = output.status.code().unwrap() as u32;
                    assert!(
                        code == 3 || code == 0x4000_0015 || code >= 0x8000_0000,
                        "{point}: {output:?}"
                    );
                }
                let expected = if committed { Some(new) } else { old };
                match expected {
                    Some(bytes) => assert_eq!(fs::read(&path).unwrap(), bytes, "{point}"),
                    None => assert!(!path.try_exists().unwrap(), "{point}"),
                }
                // Exact byte equality above includes CRLF markers, non-UTF-8
                // user rules and the suffix's missing trailing newline.
                for _ in 0..2 {
                    let mut command = workspace.command();
                    command.args(["gitignore", "--write"]);
                    let output = run_bounded(command);
                    assert_eq!(output.status.code(), Some(0), "{point}: {output:?}");
                    assert_eq!(fs::read(&path).unwrap(), new, "{point}");
                    assert!(fs::read_dir(&workspace.0).unwrap().all(|entry| {
                        !entry
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".cpf-tmp-")
                    }));
                }
            }
        }
    }

    #[test]
    fn adopt_has_only_complete_old_or_new_records_at_every_commit_boundary() {
        for (point, committed) in [
            ("atomic_replace.temp_synced", false),
            ("edit_toml.validated", false),
            ("atomic_replace.renamed", true),
            ("atomic_replace.directory_synced", true),
            ("edit_toml.published", true),
        ] {
            let workspace = Workspace::new();
            workspace.write(&["problems", "A", "notes.txt"], "my notes");
            let path = workspace.path(&["problems", "A", "problem.toml"]);
            let old = fs::read(&path).unwrap();
            abort_at(&workspace, &["adopt", "notes.txt"], point);
            let bytes = fs::read(&path).unwrap();
            if committed {
                let doc = String::from_utf8(bytes)
                    .unwrap()
                    .parse::<toml_edit::DocumentMut>()
                    .unwrap();
                assert_eq!(doc["files"]["user"].as_array().unwrap().len(), 1);
            } else {
                assert_eq!(bytes, old);
            }
            assert_eq!(
                fs::read(workspace.path(&["problems", "A", "notes.txt"])).unwrap(),
                b"my notes"
            );
            workspace.ok(&["adopt", "notes.txt"]);
            let before = workspace.snapshot();
            workspace.ok(&["adopt", "notes.txt"]);
            assert_eq!(workspace.snapshot(), before);
        }
    }

    #[test]
    fn fsck_contest_repairs_have_old_or_new_membership_and_rerun_to_completion() {
        for (point, committed) in [
            ("edit_toml.validated", false),
            ("atomic_replace.renamed", true),
            ("edit_toml.published", true),
        ] {
            let workspace = Workspace::new();
            workspace.write(
                &["contests", "round", "contest.toml"],
                "schema=1\n[[problem]]\nid='local:A'\n[[problem]]\nid='local:gone'\n",
            );
            let path = workspace.path(&["contests", "round", "contest.toml"]);
            let old = fs::read(&path).unwrap();
            abort_at(&workspace, &["fsck", "--apply"], point);
            let bytes = fs::read(&path).unwrap();
            if committed {
                let doc = String::from_utf8(bytes)
                    .unwrap()
                    .parse::<toml_edit::DocumentMut>()
                    .unwrap();
                assert_eq!(doc["problem"].as_array_of_tables().unwrap().len(), 1);
            } else {
                assert_eq!(bytes, old);
            }
            workspace.ok(&["fsck", "--apply"]);
            assert_eq!(
                fs::read(workspace.path(&["problems", "A", "main.py"])).unwrap(),
                b"print(input())\n"
            );
        }
    }

    #[test]
    fn fsck_sequence_recovery_is_atomic_and_does_not_reuse_a_surviving_sequence() {
        use cpforge::storage::{Record, job_seq::read_job_sequence};
        for (point, committed) in [
            ("atomic_replace.temp_synced", false),
            ("atomic_replace.renamed", true),
            ("atomic_replace.directory_synced", true),
            ("fsck.sequence_replaced", true),
        ] {
            let workspace = Workspace::new();
            let path = workspace.path(&[".cpforge", "job-seq"]);
            fs::remove_file(&path).unwrap();
            abort_at(&workspace, &["fsck", "--apply"], point);
            assert_eq!(path.exists(), committed);
            let saved = if committed {
                Some(fs::read(&path).unwrap())
            } else {
                None
            };
            workspace.ok(&["fsck", "--apply"]);
            let Record::Valid(sequence) = read_job_sequence(&path) else {
                panic!("valid recovered sequence")
            };
            assert_eq!(sequence.next, 0);
            if let Some(bytes) = saved {
                assert_eq!(fs::read(&path).unwrap(), bytes);
            }
        }
        let workspace = Workspace::new();
        workspace.write(
            &[".cpforge", "facts", "gone", "solution.toml"],
            "schema=1\n",
        );
        abort_at(&workspace, &["fsck", "--apply"], "fsck.fact_removed");
        assert!(
            !workspace
                .path(&[".cpforge", "facts", "gone", "solution.toml"])
                .exists()
        );
        workspace.ok(&["fsck", "--apply"]);
    }
}

#[test]
fn template_solution_recorded_as_created_is_not_a_provenance_overlap() {
    let workspace = Workspace::new();
    let record = workspace.path(&["problems", "A", "problem.toml"]);
    let original = fs::read_to_string(&record).unwrap();
    // `cpf add` records its template solution this way; the solution is named
    // only by resolved defaults, not by another record.
    fs::write(
        &record,
        format!("{original}\n[files]\ncreated=[{{path='main.py',command='add'}}]\n"),
    )
    .unwrap();
    let status = workspace.status();
    let diagnostics = status["problems"][0]["diagnostics"].as_array().unwrap();
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.as_str().unwrap().contains("overlap")),
        "{diagnostics:?}"
    );
    let output = workspace.ok(&["fsck"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("overlap"), "{stdout}");
    // An explicit declaration plus a created entry is a real overlap.
    fs::write(
        &record,
        format!(
            "{original}\n[config]\nsources=['main.py']\n[files]\ncreated=[{{path='main.py',command='add'}}]\n"
        ),
    )
    .unwrap();
    let output = workspace.ok(&["fsck"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("provenance overlap"), "{stdout}");
}
