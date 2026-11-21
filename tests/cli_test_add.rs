//! Custom test publication through the real command binary.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        let root = std::env::temp_dir().join(format!(
            "cpforge-test-add-{:032x}",
            u128::from_le_bytes(bits)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(root).unwrap(),
        ));
        fs::create_dir_all(fixture.path("problems/A")).unwrap();
        fs::create_dir(fixture.0.join("captures")).unwrap();
        fs::create_dir(fixture.0.join("user")).unwrap();
        fs::write(fixture.path("cpforge.toml"), "schema = 1\n").unwrap();
        fs::write(
            fixture.path("problems/A/problem.toml"),
            "[problem]\nid = 'local:A'\n",
        )
        .unwrap();
        fs::write(fixture.path("problems/A/input"), INPUT).unwrap();
        fs::write(fixture.path("problems/A/answer"), ANSWER).unwrap();
        fixture
    }

    fn path(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join("workspace").join(path)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
        command
            .current_dir(self.path("problems/A"))
            .args(["test"])
            .args(args)
            .env("TMPDIR", self.0.join("captures"))
            .env("TMP", self.0.join("captures"))
            .env("TEMP", self.0.join("captures"))
            .env("HOME", self.0.join("user"))
            .env("USERPROFILE", self.0.join("user"))
            .env("XDG_CONFIG_HOME", self.0.join("user"))
            .env("APPDATA", self.0.join("user"))
            .env_remove("CPF_STORAGE_CRASH_AT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        wait(self.command(args).spawn().unwrap())
    }

    fn add(&self, with_answer: bool) -> Output {
        self.run(if with_answer {
            &["add", "--input", "input", "--answer", "answer"]
        } else {
            &["add", "--input", "input"]
        })
    }

    fn catalog(&self) -> toml_edit::DocumentMut {
        fs::read_to_string(self.path("problems/A/tests.toml"))
            .unwrap()
            .parse()
            .unwrap()
    }

    fn no_captures(&self) {
        assert_eq!(fs::read_dir(self.0.join("captures")).unwrap().count(), 0);
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const INPUT: &[u8] = b"\0input\r\n\xff\n";
const ANSWER: &[u8] = b"\0answer\r\n\xfe\n";

fn wait(mut child: Child) -> Output {
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(20) {
            child.kill().unwrap();
            panic!("command blocked: {:?}", child.wait_with_output().unwrap());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

fn success(output: Output, id: &str) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, format!("{id}\n").as_bytes(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

fn failure(output: Output, code: i32, words: &[&str]) {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let message = String::from_utf8_lossy(&output.stderr);
    for word in words {
        assert!(message.contains(word), "{word}: {output:?}");
    }
}

fn entry(id: &str, input: &str, answer: Option<&str>) -> String {
    format!(
        "\n[[test]]\nid = '{id}'\ninput = 'tests/{input}'\n{}origin = 'custom'\nenabled = true\n",
        answer.map_or(String::new(), |answer| format!(
            "answer = 'tests/{answer}'\n"
        ))
    )
}

#[test]
fn binary_bytes_absent_and_empty_answers_and_first_catalog_creation() {
    let workspace = Workspace::new();
    assert!(!workspace.path("problems/A/tests").exists());
    success(workspace.add(true), "custom-1");
    assert_eq!(
        fs::read(workspace.path("problems/A/tests/custom-1.in")).unwrap(),
        INPUT
    );
    assert_eq!(
        fs::read(workspace.path("problems/A/tests/custom-1.ans")).unwrap(),
        ANSWER
    );
    success(workspace.add(false), "custom-2");
    fs::write(workspace.path("problems/A/answer"), b"").unwrap();
    success(workspace.add(true), "custom-3");
    let catalog = workspace.catalog();
    assert_eq!(catalog["schema"].as_integer(), Some(1));
    let records = catalog["test"].as_array_of_tables().unwrap();
    assert_eq!(records.len(), 3);
    for (index, record) in records.iter().enumerate() {
        let id = format!("custom-{}", index + 1);
        assert_eq!(record["id"].as_str(), Some(id.as_str()));
        assert_eq!(
            record["input"].as_str(),
            Some(format!("tests/{id}.in").as_str())
        );
        assert_eq!(record["origin"].as_str(), Some("custom"));
        assert_eq!(record["enabled"].as_bool(), Some(true));
    }
    assert!(records.get(1).unwrap().get("answer").is_none());
    assert_eq!(
        records.get(2).unwrap()["answer"].as_str(),
        Some("tests/custom-3.ans")
    );
    assert_eq!(
        fs::read(workspace.path("problems/A/tests/custom-3.ans")).unwrap(),
        b""
    );
    let listing = workspace.run(&["ls"]);
    assert_eq!(listing.status.code(), Some(0), "{listing:?}");
    assert_eq!(listing.stdout, b"custom-1 enabled answer present\ncustom-2 enabled answer absent\ncustom-3 enabled answer empty\n");
    workspace.no_captures();
}

#[test]
fn stdin_can_supply_either_stream_but_not_both() {
    for answer_stdin in [false, true] {
        let workspace = Workspace::new();
        let args = if answer_stdin {
            ["add", "--input", "input", "--answer", "-"]
        } else {
            ["add", "--input", "-", "--answer", "answer"]
        };
        let mut child = workspace
            .command(&args)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(if answer_stdin { ANSWER } else { INPUT })
            .unwrap();
        success(wait(child), "custom-1");
        assert_eq!(
            fs::read(workspace.path("problems/A/tests/custom-1.in")).unwrap(),
            INPUT
        );
        assert_eq!(
            fs::read(workspace.path("problems/A/tests/custom-1.ans")).unwrap(),
            ANSWER
        );
        workspace.no_captures();
    }
    let workspace = Workspace::new();
    failure(
        workspace.run(&["add", "--input", "-", "--answer", "-"]),
        2,
        &["both", "stdin"],
    );
    assert!(!workspace.path(".cpforge").exists());
    assert!(!workspace.path("problems/A/tests").exists());
    workspace.no_captures();
}

#[test]
fn allocation_reserves_ids_both_path_roles_and_all_entry_types_by_key() {
    for (catalog, occupied) in [
        (entry("CUSTOM-1", "other.in", None), None),
        (entry("other", "custom-1.in", None), None),
        (entry("other", "other.in", Some("CUSTOM-1.ANS")), None),
        (String::new(), Some(("custom-1.in", true))),
        (String::new(), Some(("CUSTOM-1.ANS", false))),
    ] {
        let workspace = Workspace::new();
        fs::create_dir(workspace.path("problems/A/tests")).unwrap();
        fs::write(workspace.path("problems/A/tests.toml"), catalog).unwrap();
        if let Some((name, directory)) = occupied {
            let path = workspace.path(format!("problems/A/tests/{name}"));
            if directory {
                fs::create_dir(path).unwrap();
            } else {
                fs::write(path, b"keep").unwrap();
            }
        }
        success(workspace.add(false), "custom-2");
        assert_eq!(
            fs::read(workspace.path("problems/A/tests/custom-2.in")).unwrap(),
            INPUT
        );
        workspace.no_captures();
    }
}

#[test]
fn invalid_catalogs_and_capture_errors_publish_nothing_and_remove_captures() {
    for text in ["schema = [", "schema = 2\ntest = false\n"] {
        let workspace = Workspace::new();
        fs::write(workspace.path("problems/A/tests.toml"), text).unwrap();
        let lock = cpforge::storage::WorkspaceLock::new(workspace.path("")).unwrap();
        drop(lock.acquire("fixture").unwrap());
        let before = snapshot(&workspace.path(""));
        failure(
            workspace.add(true),
            1,
            &[
                "tests.toml",
                if text.contains('[') {
                    "malformed"
                } else {
                    "too_new"
                },
            ],
        );
        assert_eq!(snapshot(&workspace.path("")), before);
        assert!(!workspace.path("problems/A/tests").exists());
        workspace.no_captures();
    }
    for args in [
        vec!["add", "--input", "missing"],
        vec!["add", "--input", "input", "--answer", "missing"],
        vec!["add", "--input", "input", "--answer", "."],
    ] {
        let workspace = Workspace::new();
        let before = snapshot(&workspace.path(""));
        failure(workspace.run(&args), 1, &["cannot capture"]);
        assert_eq!(snapshot(&workspace.path("")), before);
        assert!(!workspace.path(".cpforge").exists());
        workspace.no_captures();
    }
}

fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), files);
            } else {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().into(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn concurrent_processes_register_distinct_numbers_and_large_input_has_no_cap() {
    let workspace = Workspace::new();
    let large: Vec<u8> = (0..8 * 1024 * 1024).map(|n| (n % 251) as u8).collect();
    fs::write(workspace.path("problems/A/input"), &large).unwrap();
    let args = ["add", "--input", "input", "--answer", "answer"];
    let first = workspace.command(&args).spawn().unwrap();
    let second = workspace.command(&args).spawn().unwrap();
    let first = wait(first);
    let second = wait(second);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    assert!(
        first.stderr.is_empty() && second.stderr.is_empty(),
        "{first:?} {second:?}"
    );
    let mut ids = [first.stdout, second.stdout];
    ids.sort();
    assert_eq!(ids, [b"custom-1\n".to_vec(), b"custom-2\n".to_vec()]);
    assert_eq!(
        workspace.catalog()["test"]
            .as_array_of_tables()
            .unwrap()
            .len(),
        2
    );
    for n in [1, 2] {
        assert_eq!(
            fs::read(workspace.path(format!("problems/A/tests/custom-{n}.in"))).unwrap(),
            large
        );
        assert_eq!(
            fs::read(workspace.path(format!("problems/A/tests/custom-{n}.ans"))).unwrap(),
            ANSWER
        );
    }
    workspace.no_captures();
}

#[test]
fn a_slow_stdin_capture_does_not_hold_the_lock_or_enter_workspace_recovery() {
    let workspace = Workspace::new();
    let mut child = workspace
        .command(&["add", "--input", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while fs::read_dir(workspace.0.join("captures")).unwrap().count() == 0 {
        assert!(child.try_wait().unwrap().is_none());
        if start.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            panic!(
                "capture did not start: {:?}",
                child.wait_with_output().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!workspace.path(".cpforge").exists());
    // This process can acquire the lock and run recovery while stdin waits.
    success(workspace.add(false), "custom-1");
    assert_eq!(
        fs::read_dir(workspace.0.join("captures")).unwrap().count(),
        1
    );
    child.stdin.take().unwrap().write_all(INPUT).unwrap();
    success(wait(child), "custom-2");
    assert_eq!(
        fs::read(workspace.path("problems/A/tests/custom-2.in")).unwrap(),
        INPUT
    );
    workspace.no_captures();
}

#[cfg(unix)]
#[test]
fn an_explicit_fifo_is_streamed_and_tool_written_links_are_refused() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{OpenOptionsExt, symlink};
    let workspace = Workspace::new();
    let path = workspace.path("problems/A/fifo");
    let native = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: native is a live NUL-terminated path; mkfifo retains no pointers.
    assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
    let mut child = workspace
        .command(&["add", "--input", "fifo"])
        .spawn()
        .unwrap();
    let start = Instant::now();
    let mut writer = loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path.as_path())
        {
            Ok(file) => break file,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                if start.elapsed() > Duration::from_secs(10) {
                    child.kill().unwrap();
                    panic!(
                        "FIFO reader did not start: {:?}",
                        child.wait_with_output().unwrap()
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("FIFO writer: {error}"),
        }
    };
    writer.write_all(INPUT).unwrap();
    drop(writer);
    success(wait(child), "custom-1");
    assert_eq!(
        fs::read(workspace.path("problems/A/tests/custom-1.in")).unwrap(),
        INPUT
    );
    workspace.no_captures();

    let workspace = Workspace::new();
    fs::create_dir(workspace.path("elsewhere")).unwrap();
    symlink(
        workspace.path("elsewhere"),
        workspace.path("problems/A/tests"),
    )
    .unwrap();
    failure(workspace.add(true), 1, &["link"]);
    assert_eq!(
        fs::read_dir(workspace.path("elsewhere")).unwrap().count(),
        0
    );
    workspace.no_captures();
}

#[cfg(feature = "storage-test-hooks")]
mod crash_tests {
    use super::*;

    // (input present, answer present, catalog present, registration committed)
    type State = (bool, bool, bool, bool);

    fn points(answer: bool, catalog: bool) -> Vec<(String, State)> {
        let mut points = vec![
            ("lock.acquired".into(), (false, false, catalog, false)),
            ("lock.recovered".into(), (false, false, catalog, false)),
            ("test_add.locked".into(), (false, false, catalog, false)),
            (
                "test_add.input_published".into(),
                (true, false, catalog, false),
            ),
        ];
        if answer {
            points.push((
                "test_add.answer_published".into(),
                (true, true, catalog, false),
            ));
        }
        let data_count = 1 + usize::from(answer);
        let publish_count = data_count + usize::from(!catalog);
        for occurrence in 1..=publish_count {
            for (phase, committed) in [
                ("before_publish", false),
                ("after_publish", true),
                ("directory_synced", true),
                #[cfg(target_os = "linux")]
                ("linked", true),
            ] {
                let count = occurrence - usize::from(!committed);
                points.push((
                    format!("file_noclobber.{phase}#{occurrence}"),
                    (
                        count >= 1,
                        answer && count >= 2,
                        catalog || count > data_count,
                        false,
                    ),
                ));
            }
        }
        for (point, registered) in [
            ("atomic_replace.temp_synced", false),
            ("edit_toml.validated", false),
            ("atomic_replace.renamed", true),
            ("atomic_replace.directory_synced", true),
            ("edit_toml.published", true),
            ("test_add.registered", true),
        ] {
            points.push((point.into(), (true, answer, true, registered)));
        }
        points
    }

    fn aborting_add(workspace: &Workspace, answer: bool, point: &str) -> Output {
        let mut command = workspace.command(if answer {
            &["add", "--input", "input", "--answer", "answer"]
        } else {
            &["add", "--input", "input"]
        });
        command.env("CPF_STORAGE_CRASH_AT", point);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: the child hook only sets the resource limit; no locks or
            // allocation are used on its successful path.
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
                // SAFETY: children inherit the test process's error mode.
                unsafe {
                    windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                        windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
                    );
                }
            });
        }
        let output = wait(command.spawn().unwrap());
        let marker = format!("CPF-CRASH-AT {}", point.split('#').next().unwrap());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .any(|line| line == marker),
            "{point}: {output:?}"
        );
        assert!(!output.status.success(), "{output:?}");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(output.status.signal(), Some(libc::SIGABRT), "{output:?}");
        }
        #[cfg(windows)]
        {
            let code = output.status.code().unwrap() as u32;
            assert!(
                code == 3 || code == 0x4000_0015 || code >= 0x8000_0000,
                "{output:?}"
            );
        }
        output
    }

    fn no_workspace_temps(root: &Path) {
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            assert!(!entry.file_name().to_string_lossy().starts_with(".cpf-tmp-"));
            if entry.file_type().unwrap().is_dir() {
                no_workspace_temps(&entry.path());
            }
        }
    }

    #[test]
    fn every_commit_point_with_and_without_answer_and_first_catalog_recovers() {
        for answer in [false, true] {
            for catalog in [false, true] {
                for (point, (input_present, answer_present, catalog_present, registered)) in
                    points(answer, catalog)
                {
                    let workspace = Workspace::new();
                    fs::create_dir(workspace.path("problems/A/tests")).unwrap();
                    fs::create_dir_all(workspace.path(".cpforge/staging/old")).unwrap();
                    fs::write(workspace.path(".cpforge/staging/old/leftover"), b"old").unwrap();
                    let old_temp =
                        workspace.path(format!("problems/A/tests/.cpf-tmp-{}", "a".repeat(32)));
                    fs::write(&old_temp, b"old").unwrap();
                    if catalog {
                        fs::write(workspace.path("problems/A/tests.toml"), "schema = 1\n").unwrap();
                    }
                    aborting_add(&workspace, answer, &point);
                    assert_eq!(old_temp.exists(), point == "lock.acquired", "{point}");
                    assert_eq!(
                        workspace.path(".cpforge/staging/old").exists(),
                        point == "lock.acquired",
                        "{point}"
                    );
                    let input = workspace.path("problems/A/tests/custom-1.in");
                    let reference = workspace.path("problems/A/tests/custom-1.ans");
                    assert_eq!(input.exists(), input_present, "{point}");
                    assert_eq!(reference.exists(), answer_present, "{point}");
                    assert_eq!(
                        workspace.path("problems/A/tests.toml").exists(),
                        catalog_present,
                        "{point}"
                    );
                    if input_present {
                        assert_eq!(fs::read(&input).unwrap(), INPUT, "{point}");
                    }
                    if answer_present {
                        assert_eq!(fs::read(&reference).unwrap(), ANSWER, "{point}");
                    }
                    if catalog_present {
                        let document = workspace.catalog();
                        assert_eq!(document["schema"].as_integer(), Some(1), "{point}");
                        let entries = document
                            .get("test")
                            .and_then(toml_edit::Item::as_array_of_tables);
                        assert_eq!(
                            entries.map_or(0, |entries| entries.len()),
                            usize::from(registered),
                            "{point}"
                        );
                        if registered {
                            let record = entries.unwrap().get(0).unwrap();
                            assert_eq!(record["id"].as_str(), Some("custom-1"));
                            assert_eq!(record["input"].as_str(), Some("tests/custom-1.in"));
                            assert_eq!(record["origin"].as_str(), Some("custom"));
                            assert_eq!(record["enabled"].as_bool(), Some(true));
                            assert_eq!(
                                record.get("answer").and_then(toml_edit::Item::as_str),
                                answer.then_some("tests/custom-1.ans")
                            );
                        }
                    }
                    let listing = workspace.run(&["ls"]);
                    assert_eq!(listing.status.code(), Some(0), "{listing:?}");
                    let text = String::from_utf8(listing.stdout).unwrap();
                    assert_eq!(
                        text.contains("candidate tests/custom-1.in"),
                        input_present && !registered,
                        "{point}: {text}"
                    );
                    assert_eq!(
                        text.contains("custom-1 enabled answer"),
                        registered,
                        "{point}: {text}"
                    );
                    // Once published, even an editor's later bytes are permanent.
                    if input_present {
                        fs::write(&input, b"user edit\0\r\n").unwrap();
                    }
                    let crashed_captures =
                        fs::read_dir(workspace.0.join("captures")).unwrap().count();
                    success(
                        workspace.add(answer),
                        if input_present {
                            "custom-2"
                        } else {
                            "custom-1"
                        },
                    );
                    assert_eq!(
                        fs::read_dir(workspace.0.join("captures")).unwrap().count(),
                        crashed_captures
                    );
                    if input_present {
                        assert_eq!(fs::read(&input).unwrap(), b"user edit\0\r\n");
                    }
                    if answer_present {
                        assert_eq!(fs::read(reference).unwrap(), ANSWER);
                    }
                    no_workspace_temps(&workspace.path(""));
                    assert_eq!(
                        fs::read_dir(workspace.path(".cpforge/staging"))
                            .unwrap()
                            .count(),
                        0
                    );
                    let document = workspace.catalog();
                    let entries = document["test"].as_array_of_tables().unwrap();
                    assert_eq!(entries.len(), 1 + usize::from(registered));
                    let new = entries.get(entries.len() - 1).unwrap();
                    assert_eq!(
                        fs::read(
                            workspace
                                .path("problems/A")
                                .join(new["input"].as_str().unwrap())
                        )
                        .unwrap(),
                        INPUT
                    );
                    if answer {
                        assert_eq!(
                            fs::read(
                                workspace
                                    .path("problems/A")
                                    .join(new["answer"].as_str().unwrap())
                            )
                            .unwrap(),
                            ANSWER
                        );
                    } else {
                        assert!(new.get("answer").is_none());
                    }
                }
            }
        }
    }
}

#[test]
fn partial_registration_failure_reports_paths_step_and_cause_and_removes_captures() {
    let workspace = Workspace::new();
    let original = b"# shared record\nschema = 1\n";
    fs::write(workspace.path("problems/A/tests.toml"), original).unwrap();
    fs::hard_link(
        workspace.path("problems/A/tests.toml"),
        workspace.path("catalog-link"),
    )
    .unwrap();
    let output = workspace.add(true);
    failure(
        output,
        1,
        &[
            "tests/custom-1.in",
            "tests/custom-1.ans",
            "not registered",
            "step 3",
            "one link",
        ],
    );
    assert_eq!(
        fs::read(workspace.path("problems/A/tests/custom-1.in")).unwrap(),
        INPUT
    );
    assert_eq!(
        fs::read(workspace.path("problems/A/tests/custom-1.ans")).unwrap(),
        ANSWER
    );
    assert_eq!(
        fs::read(workspace.path("problems/A/tests.toml")).unwrap(),
        original
    );
    let listing = workspace.run(&["ls"]);
    assert_eq!(listing.status.code(), Some(0));
    assert_eq!(
        listing.stdout,
        b"candidate tests/custom-1.in answer tests/custom-1.ans\n"
    );
    workspace.no_captures();
    fs::remove_file(workspace.path("catalog-link")).unwrap();
    success(workspace.add(true), "custom-2");
    assert_eq!(
        fs::read(workspace.path("problems/A/tests/custom-1.in")).unwrap(),
        INPUT
    );
    workspace.no_captures();
}
