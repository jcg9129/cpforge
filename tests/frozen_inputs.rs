//! Frozen job data, exclusive capture, and filesystem identity boundaries.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use cpforge::config::{ConfigRequest, Environment, Platform, TargetPlan, load_config};
use cpforge::names::Identifier;
use cpforge::storage::admission::{RegisteredJob, register_job};
use cpforge::storage::inputs::{
    CaptureError, CapturedSource, FreezeInputsError, InputsError, SourceRole, capture_sources,
};
use cpforge::storage::job::{ProbeOutcome, probe};
use cpforge::storage::prune::PrunePolicy;
use cpforge::storage::{ErrorKind, WorkspaceLock};
use cpforge::tests_store::{FreezeError, FrozenAnswer, Health, TestStore};
use sha2::{Digest, Sha256};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/frozen-input-fixtures")
            .join(format!("{:032x}", u128::from_le_bytes(random)));
        fs::create_dir_all(root.join("problem/tests")).unwrap();
        let fixture = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(root).unwrap(),
        ));
        fixture.write("cpforge.toml", b"schema = 1\n");
        fixture.write("user.toml", b"schema = 1\n");
        fixture.write(
            ".cpforge/job-seq",
            b"schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
        );
        fixture
    }
    fn path(&self, relative: &str) -> PathBuf {
        // Join per component so `/` in fixtures becomes the native separator.
        Path::new(relative)
            .components()
            .fold(self.0.clone(), |path, part| path.join(part))
    }
    fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn store(&self) -> TestStore {
        TestStore::new(self.path("problem"))
    }
    fn job(&self, limit: u64) -> RegisteredJob {
        let lock = WorkspaceLock::new(&self.0).unwrap();
        register_job(
            lock.acquire("freeze test").unwrap(),
            PrunePolicy::default(),
            limit,
            SystemTime::now(),
        )
        .unwrap()
    }
    fn target(&self, sources: &[&str], support: &[&str]) -> TargetPlan {
        let list = |paths: &[&str]| {
            paths
                .iter()
                .map(|path| format!("'{path}'"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let text = format!(
            "schema = 1\n[problem]\nid = 'local:p'\n[config]\nlanguage = 'cpp'\nsources = [{}]\nsupport_files = [{}]\n",
            list(sources),
            list(support)
        );
        self.write("problem/problem.toml", text.as_bytes());
        load_config(&ConfigRequest {
            environment: Environment::default(),
            platform: Platform::host(),
            current_dir: self.0.clone(),
            config_path: Some(self.path("user.toml")),
            workspace_root: Some(self.0.clone()),
            problem_file: Some(self.path("problem/problem.toml")),
            ..ConfigRequest::default()
        })
        .target()
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn id(text: &str) -> Identifier {
    Identifier::new(text).unwrap()
}
fn entry(id: &str, answer: Option<&str>) -> String {
    format!(
        "[[test]]\nid = '{id}'\ninput = 'tests/{id}.in'\n{}origin = 'custom'\nenabled = true\n",
        answer.map_or(String::new(), |path| format!("answer = 'tests/{path}'\n"))
    )
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn link_directory(source: &Path, destination: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, destination).unwrap();
    #[cfg(windows)]
    {
        // Junctions need no symlink privilege. cmd requires ordinary paths.
        let ordinary = |path: &Path| {
            let path = path.as_os_str().to_string_lossy();
            if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
                PathBuf::from(format!(r"\\{unc}"))
            } else {
                PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(&path))
            }
        };
        assert!(
            std::process::Command::new("cmd.exe")
                .args(["/c", "mklink", "/J"])
                .arg(ordinary(destination))
                .arg(ordinary(source))
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn complete_inputs_preserve_bytes_hashes_answer_states_order_and_source_roles() {
    let f = Fixture::new();
    let input = b"\x00\xff\r\nabc\n";
    f.write(
        "problem/tests.toml",
        (entry("absent", None)
            + &entry("empty", Some("empty.ans"))
            + &entry("present", Some("present.ans")))
            .as_bytes(),
    );
    for name in ["absent", "empty", "present"] {
        f.write(&format!("problem/tests/{name}.in"), input);
    }
    // A sibling does not invent an answer for an absent catalog declaration.
    f.write("problem/tests/absent.ans", b"ignored");
    f.write("problem/tests/empty.ans", b"");
    f.write("problem/tests/present.ans", b"answer\r\n\xff");
    let source_files = [
        ("judge/Grader.java", &b"grader\0"[..]),
        ("Solution.java", &b"solution\r\n"[..]),
        ("grader.h", &b"header\xff"[..]),
    ];
    for (path, bytes) in source_files {
        f.write(&format!("problem/{path}"), bytes);
    }
    let target = f.target(&["judge/Grader.java", "Solution.java"], &["grader.h"]);
    let job = f.job(1 << 20);
    let before = job.budget.usage().retained;
    let selected = [id("present"), id("absent"), id("empty")];
    let frozen = job
        .inputs()
        .freeze(
            &f.store(),
            &selected,
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    assert_eq!(frozen.job_dir(), job.dir.as_path());
    let entries = frozen.manifest().entries();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>(),
        selected
    );
    for entry in entries {
        assert_eq!(
            entry.input.path,
            job.dir.join(format!("inputs/{}.in", entry.id))
        );
        assert_eq!(fs::read(&entry.input.path).unwrap(), input);
        assert_eq!(entry.input.len, input.len() as u64);
        assert_eq!(entry.input.sha256, hash(input));
    }
    assert_eq!(entries[1].answer, FrozenAnswer::Absent);
    for (entry, expected) in [
        (&entries[0], &b"answer\r\n\xff"[..]),
        (&entries[2], &b""[..]),
    ] {
        let FrozenAnswer::Present { path, len, sha256 } = &entry.answer else {
            panic!("answer must be present")
        };
        assert_eq!(path, &job.dir.join(format!("inputs/{}.ans", entry.id)));
        assert_eq!(fs::read(path).unwrap(), expected);
        assert_eq!(*len, expected.len() as u64);
        assert_eq!(*sha256, hash(expected));
    }
    let copies = frozen.sources().files();
    assert_eq!(copies.len(), 3);
    for (index, (path, bytes)) in source_files.iter().enumerate() {
        assert_eq!(copies[index].problem_path.components().join("/"), *path);
        assert_eq!(
            copies[index].copy_path,
            job.dir.join("inputs/src").join(path)
        );
        assert_eq!(copies[index].len, bytes.len() as u64);
        assert_eq!(fs::read(&copies[index].copy_path).unwrap(), *bytes);
        assert_eq!(
            copies[index].role,
            if index < 2 {
                SourceRole::Source
            } else {
                SourceRole::Support
            }
        );
    }
    assert_eq!(
        frozen.sources().source_bindings(),
        vec![copies[0].copy_path.clone(), copies[1].copy_path.clone()]
    );
    let expected_len = 3 * input.len() as u64
        + b"answer\r\n\xff".len() as u64
        + source_files
            .iter()
            .map(|(_, bytes)| bytes.len() as u64)
            .sum::<u64>();
    assert_eq!(job.budget.usage().retained, before + expected_len);
    assert_eq!(job.budget.usage().reserved, 0);
    f.write("problem/tests/present.in", b"edited input");
    f.write("problem/tests/present.ans", b"edited answer");
    f.write("problem/judge/Grader.java", b"edited source");
    assert_eq!(fs::read(&entries[0].input.path).unwrap(), input);
    assert_eq!(fs::read(&copies[0].copy_path).unwrap(), source_files[0].1);
    let FrozenAnswer::Present { path, .. } = &entries[0].answer else {
        unreachable!()
    };
    assert_eq!(fs::read(path).unwrap(), b"answer\r\n\xff");
    // The complete freeze also works without selected tests or captured files.
    let empty_job = f.job(1 << 20);
    let empty = empty_job
        .inputs()
        .freeze(&f.store(), &[], &[], &f.path("problem"))
        .unwrap();
    assert!(empty.manifest().entries().is_empty());
    assert!(empty.sources().files().is_empty());
}

#[test]
fn unknown_candidates_disabled_and_unhealthy_selections_fail_closed() {
    let f = Fixture::new();
    f.write(
        "problem/tests.toml",
        (entry("good", None)
            + &entry("missing", None)
            + &entry("disabled", None).replace("enabled = true", "enabled = false"))
            .as_bytes(),
    );
    f.write("problem/tests/good.in", b"good");
    f.write("problem/tests/candidate.in", b"unregistered");
    for name in ["unknown", "candidate", "disabled", "missing"] {
        let job = f.job(1 << 20);
        let error = f
            .store()
            .freeze_into(&[id(name)], &job.inputs())
            .unwrap_err();
        match name {
            "unknown" | "candidate" => assert!(matches!(error, FreezeError::UnknownId(_))),
            "disabled" => assert!(matches!(error, FreezeError::DisabledId(_))),
            _ => assert!(matches!(
                error,
                FreezeError::Unhealthy {
                    health: Health::Missing,
                    ..
                }
            )),
        }
        assert!(
            job.inputs()
                .freeze(&f.store(), &[], &[], &f.path("problem"))
                .is_err()
        );
    }
    // Selecting one entry twice is refused before anything is copied.
    let job = f.job(1 << 20);
    assert!(matches!(
        f.store().freeze_into(&[id("good"), id("good")], &job.inputs()),
        Err(FreezeError::DuplicateId(duplicate)) if duplicate == id("good")
    ));
    assert!(!job.dir.join("inputs/good.in").exists());
    // A disabled unhealthy entry never affects a separately selected good test.
    let job = f.job(1 << 20);
    assert!(f.store().freeze_into(&[id("good")], &job.inputs()).is_ok());
    let job = f.job(1 << 20);
    let dir = job.dir.clone();
    assert!(
        job.inputs()
            .freeze(
                &f.store(),
                &[id("good"), id("missing")],
                &[],
                &f.path("problem")
            )
            .is_err()
    );
    assert_eq!(fs::read(dir.join("inputs/good.in")).unwrap(), b"good");
    assert!(matches!(probe(&dir), ProbeOutcome::Held));
    drop(job);
    assert!(matches!(probe(&dir), ProbeOutcome::Acquired(_)));
    assert!(dir.exists());
}

#[test]
fn missing_named_answer_and_invalid_catalog_fail_without_a_manifest() {
    let f = Fixture::new();
    f.write(
        "problem/tests.toml",
        entry("one", Some("missing.ans")).as_bytes(),
    );
    f.write("problem/tests/one.in", b"input");
    let job = f.job(1 << 20);
    assert!(matches!(
        f.store().freeze_into(&[id("one")], &job.inputs()),
        Err(FreezeError::Unhealthy {
            health: Health::Missing,
            ..
        })
    ));
    for text in ["schema = 2", "schema = ["] {
        f.write("problem/tests.toml", text.as_bytes());
        let job = f.job(1 << 20);
        assert!(matches!(
            f.store().freeze_into(&[], &job.inputs()),
            Err(FreezeError::Catalog(_))
        ));
    }
}

#[test]
fn tiny_job_budget_refuses_copies_and_keeps_accounting_within_limit() {
    let f = Fixture::new();
    f.write("problem/tests.toml", entry("one", None).as_bytes());
    f.write("problem/tests/one.in", &[42; 4096]);
    let job = f.job(512);
    let before = job.budget.usage();
    assert!(matches!(
        job.inputs()
            .freeze(&f.store(), &[id("one")], &[], &f.path("problem")),
        Err(FreezeInputsError::Tests(FreezeError::Inputs(
            InputsError::Budget(_)
        )))
    ));
    assert_eq!(job.budget.usage(), before);
    assert_eq!(
        fs::metadata(job.dir.join("inputs/one.in")).unwrap().len(),
        0
    );
}

#[test]
fn source_failure_after_test_freeze_returns_no_complete_inputs() {
    let f = Fixture::new();
    f.write("problem/tests.toml", entry("one", None).as_bytes());
    f.write("problem/tests/one.in", b"input");
    f.write("problem/a.cpp", b"source");
    let target = f.target(&["a.cpp"], &["missing.h"]);
    let job = f.job(1 << 20);
    let before = job.budget.usage().retained;
    assert!(matches!(
        job.inputs().freeze(
            &f.store(),
            &[id("one")],
            &CapturedSource::from_target(&target),
            &f.path("problem")
        ),
        Err(FreezeInputsError::Sources(CaptureError::Unhealthy {
            health: Health::Missing,
            ..
        }))
    ));
    assert_eq!(fs::read(job.dir.join("inputs/one.in")).unwrap(), b"input");
    assert!(!job.dir.join("inputs/src/a.cpp").exists());
    assert_eq!(job.budget.usage().retained, before + 5);
    f.write("problem/missing.h", b"fixed");
    assert!(
        job.inputs()
            .freeze(&f.store(), &[], &[], &f.path("problem"))
            .is_err()
    );
}

#[test]
fn hard_link_aliases_fail_preflight_before_any_source_copy() {
    let f = Fixture::new();
    f.write("problem/a.cpp", b"source");
    fs::hard_link(f.path("problem/a.cpp"), f.path("problem/alias.cpp")).unwrap();
    // Same-file checks span source and support roles.
    let target = f.target(&["a.cpp"], &["alias.cpp"]);
    let job = f.job(1 << 20);
    assert!(matches!(
        capture_sources(
            &CapturedSource::from_target(&target),
            &f.path("problem"),
            &job.inputs()
        ),
        Err(CaptureError::SameFile { .. })
    ));
    assert!(!job.dir.join("inputs/src/a.cpp").exists());
}

#[test]
fn existing_destination_is_a_source_collision_and_is_never_overwritten() {
    let f = Fixture::new();
    f.write("problem/a.cpp", b"original");
    let target = f.target(&["a.cpp"], &[]);
    let job = f.job(1 << 20);
    fs::create_dir_all(job.dir.join("inputs/src")).unwrap();
    let destination = job.dir.join("inputs/src/a.cpp");
    fs::write(&destination, b"keep").unwrap();
    assert!(
        matches!(capture_sources(&CapturedSource::from_target(&target), &f.path("problem"), &job.inputs()), Err(CaptureError::SourceCollision { path }) if path == destination)
    );
    assert_eq!(fs::read(destination).unwrap(), b"keep");
    assert!(
        job.inputs()
            .freeze(&f.store(), &[], &[], &f.path("problem"))
            .is_err()
    );
}

#[test]
fn capture_rejects_reserved_storage_and_nonregular_sources() {
    let f = Fixture::new();
    for path in [
        ".cpforge/runs/a.cpp",
        ".cpforge/cache/a.cpp",
        ".cpforge/staging/a.cpp",
        ".cpforge/facts/a.cpp",
        ".cpforge/state.toml",
        ".cpforge/lock",
        ".cpforge/job-seq",
    ] {
        f.write(&format!("problem/{path}"), b"reserved");
        let target = f.target(&[path], &[]);
        let job = f.job(1 << 20);
        assert!(matches!(
            capture_sources(
                &CapturedSource::from_target(&target),
                &f.path("problem"),
                &job.inputs()
            ),
            Err(CaptureError::ReservedPath { .. })
        ));
    }
    for path in ["missing.cpp", "directory.cpp"] {
        if path == "directory.cpp" {
            fs::create_dir(f.path("problem/directory.cpp")).unwrap();
        }
        let target = f.target(&[path], &[]);
        let job = f.job(1 << 20);
        assert!(matches!(
            capture_sources(
                &CapturedSource::from_target(&target),
                &f.path("problem"),
                &job.inputs()
            ),
            Err(CaptureError::Unhealthy { .. })
        ));
    }
}

#[cfg(unix)]
#[test]
fn selected_fifo_is_refused_on_the_open_handle_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    let f = Fixture::new();
    f.write("problem/tests.toml", entry("pipe", None).as_bytes());
    let path =
        std::ffi::CString::new(f.path("problem/tests/pipe.in").as_os_str().as_bytes()).unwrap();
    // SAFETY: path is terminated, and mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let job = f.job(1 << 20);
    let started = std::time::Instant::now();
    assert!(matches!(
        f.store().freeze_into(&[id("pipe")], &job.inputs()),
        Err(FreezeError::Unhealthy {
            health: Health::NotRegular { kind: "FIFO" },
            ..
        })
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn source_link_components_and_destination_link_components_are_refused() {
    let f = Fixture::new();
    f.write("problem/real/a.cpp", b"source");
    link_directory(&f.path("problem/real"), &f.path("problem/link"));
    let target = f.target(&["link/a.cpp"], &[]);
    let job = f.job(1 << 20);
    assert!(
        matches!(capture_sources(&CapturedSource::from_target(&target), &f.path("problem"), &job.inputs()), Err(CaptureError::Inputs(InputsError::Storage(error))) if matches!(error.kind, ErrorKind::LinkInPath { .. }))
    );
    let target = f.target(&["real/a.cpp"], &[]);
    let job = f.job(1 << 20);
    link_directory(&f.path("problem/real"), &job.dir.join("inputs"));
    assert!(
        capture_sources(
            &CapturedSource::from_target(&target),
            &f.path("problem"),
            &job.inputs()
        )
        .is_err()
    );
    assert!(!f.path("problem/real/src").exists());
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn case_aliases_are_checked_by_file_identity_on_aliasing_filesystems() {
    let f = Fixture::new();
    f.write("problem/a.cpp", b"source");
    let aliases = f.path("problem/A.cpp").exists();
    if !aliases {
        f.write("problem/A.cpp", b"distinct");
    }
    // Phase 1 normally rejects this textual pair on these platforms. Load each
    // declaration separately to exercise phase 3's independent identity check.
    let mut configured = CapturedSource::from_target(&f.target(&["a.cpp"], &[]));
    configured.extend(CapturedSource::from_target(&f.target(&["A.cpp"], &[])));
    let job = f.job(1 << 20);
    let result = capture_sources(&configured, &f.path("problem"), &job.inputs());
    if aliases {
        assert!(matches!(result, Err(CaptureError::SameFile { .. })));
    } else {
        assert_eq!(result.unwrap().files().len(), 2);
    }
    eprintln!("case variants alias on this filesystem: {aliases}");
}

#[cfg(target_os = "macos")]
#[test]
fn normalization_alias_behavior_is_recorded_and_never_overwrites() {
    let f = Fixture::new();
    f.write("problem/e\u{301}.cpp", b"source");
    let aliases = f.path("problem/é.cpp").exists();
    if !aliases {
        f.write("problem/é.cpp", b"distinct");
    }
    let target = f.target(&["e\u{301}.cpp", "é.cpp"], &[]);
    let job = f.job(1 << 20);
    let result = capture_sources(
        &CapturedSource::from_target(&target),
        &f.path("problem"),
        &job.inputs(),
    );
    if aliases {
        assert!(matches!(
            result,
            Err(CaptureError::SameFile { .. } | CaptureError::SourceCollision { .. })
        ));
        eprintln!("macOS NFC/NFD observation: names alias; capture refused");
    } else {
        let captured = result.unwrap();
        assert_eq!(captured.files().len(), 2);
        assert_eq!(fs::read(&captured.files()[0].copy_path).unwrap(), b"source");
        assert_eq!(
            fs::read(&captured.files()[1].copy_path).unwrap(),
            b"distinct"
        );
        eprintln!("macOS NFC/NFD observation: names are distinct; both copies preserved");
    }
}
