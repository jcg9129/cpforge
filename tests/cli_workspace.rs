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
                } else if relative == Path::new(".cpforge").join("lock") {
                    // Windows refuses reads of a byte range another handle
                    // has locked; the lock file's existence is what matters.
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
