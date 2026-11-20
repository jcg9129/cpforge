//! Local problem and contest transactions through the real command binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug)]
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        // TMPDIR may itself point inside a checkout. Choose a writable
        // ancestor outside every git tree so host git state cannot leak in.
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
                let candidate = base.join(format!(
                    "cpforge-add-c4d-{:032x}",
                    u128::from_le_bytes(bits)
                ));
                fs::create_dir(&candidate).ok().map(|()| candidate)
            })
            .expect("a writable temporary directory outside a git tree");
        let workspace = Self(fs::canonicalize(path).unwrap());
        assert!(
            !workspace
                .0
                .ancestors()
                .any(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok()),
            "fixture must be outside a git tree"
        );
        fs::create_dir_all(workspace.path("user/cpforge")).unwrap();
        success(&workspace.command().arg("init").output().unwrap());
        workspace
    }

    fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.0.join(relative)
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

    fn add(&self, args: &[&str]) -> Output {
        self.command().arg("add").args(args).output().unwrap()
    }

    fn user_config(&self, text: &str) {
        fs::write(self.path("user/cpforge/config.toml"), text).unwrap();
    }

    fn empty_staging(&self) {
        assert_eq!(
            fs::read_dir(self.path(".cpforge/staging")).unwrap().count(),
            0
        );
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn success(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

fn failure(output: &Output, code: i32, message: &str) {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Warnings (e.g. incomplete leftover recovery) may precede the error.
    assert!(
        stderr.lines().any(|line| line.starts_with("cpf: error: ")),
        "{output:?}"
    );
    assert!(stderr.contains(message), "expected {message:?}: {output:?}");
}

fn problem(name: &str) -> String {
    format!("schema = 1\n\n[problem]\nid = \"local:{name}\"\n")
}

fn contest_member(id: &str, label: Option<&str>) -> String {
    let mut text = format!("\n[[problem]]\nid = \"local:{id}\"\n");
    if let Some(label) = label {
        text.push_str(&format!("label = \"{label}\"\n"));
    }
    text
}

#[test]
fn unconfigured_add_has_exact_metadata_and_is_idempotent() {
    let workspace = Workspace::new();
    let output = workspace.add(&["two-sum"]);
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("created problems/two-sum/"));
    assert!(stdout.contains("language unresolved"));
    assert!(stdout.contains("without a solution file"));
    let path = workspace.path("problems/two-sum/problem.toml");
    assert_eq!(fs::read(&path).unwrap(), problem("two-sum").as_bytes());
    assert_eq!(
        fs::read_dir(workspace.path("problems/two-sum"))
            .unwrap()
            .count(),
        1
    );
    let bytes = fs::read(&path).unwrap();
    fs::write(workspace.path("problems/two-sum/notes.md"), b"user notes").unwrap();
    // Existing problems bypass config and preserve every file.
    workspace.user_config("schema = 99\n");
    let second = workspace.add(&["two-sum"]);
    success(&second);
    assert!(String::from_utf8_lossy(&second.stdout).contains("already exists"));
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(
        fs::read(workspace.path("problems/two-sum/notes.md")).unwrap(),
        b"user notes"
    );
    workspace.empty_staging();
}

#[test]
fn default_and_explicit_languages_copy_bytes_and_record_provenance() {
    let workspace = Workspace::new();
    workspace.user_config("schema = 1\ndefault_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n[languages.python]\ntemplate = { path = 't.py' }\n");
    let cpp = b"// template\r\nint main() {}\n\xff";
    let python = b"print('hello')\r\n";
    fs::write(workspace.path("user/cpforge/t.cpp"), cpp).unwrap();
    fs::write(workspace.path("user/cpforge/t.py"), python).unwrap();
    success(&workspace.add(&["one"]));
    assert_eq!(
        fs::read(workspace.path("problems/one/main.cpp")).unwrap(),
        cpp
    );
    assert_eq!(
        fs::read_to_string(workspace.path("problems/one/problem.toml")).unwrap(),
        format!(
            "{}\n[files]\ncreated = [{{ path = \"main.cpp\", command = \"add\" }}]\n",
            problem("one")
        )
    );
    success(&workspace.add(&["two", "--language", "python"]));
    assert_eq!(
        fs::read(workspace.path("problems/two/main.py")).unwrap(),
        python
    );
    assert_eq!(
        fs::read_to_string(workspace.path("problems/two/problem.toml")).unwrap(),
        format!(
            "{}\n[config]\nlanguage = \"python\"\n\n[files]\ncreated = [{{ path = \"main.py\", command = \"add\" }}]\n",
            problem("two")
        )
    );
    fs::write(workspace.path("problems/one/main.cpp"), b"edited by user").unwrap();
    success(&workspace.add(&["one", "--language", "python"]));
    assert_eq!(
        fs::read(workspace.path("problems/one/main.cpp")).unwrap(),
        b"edited by user"
    );
    assert!(!workspace.path("problems/one/main.py").exists());
    workspace.empty_staging();
}

#[test]
fn explicit_language_without_template_is_recorded_without_solution() {
    let workspace = Workspace::new();
    let output = workspace.add(&["one", "--language", "python"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("no template"));
    assert_eq!(
        fs::read_to_string(workspace.path("problems/one/problem.toml")).unwrap(),
        format!("{}\n[config]\nlanguage = \"python\"\n", problem("one"))
    );
    assert!(!workspace.path("problems/one/main.py").exists());
}

#[test]
fn new_problem_uses_its_prospective_config_when_invoked_inside_another_problem() {
    let workspace = Workspace::new();
    workspace.user_config("default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n[languages.python]\ntemplate = { path = 't.py' }\n");
    fs::write(workspace.path("user/cpforge/t.cpp"), b"cpp template").unwrap();
    fs::write(workspace.path("user/cpforge/t.py"), b"python template").unwrap();
    success(&workspace.add(&["grader"]));
    fs::write(workspace.path("problems/grader/problem.toml"), format!("{}\n[config]\nlanguage = 'python'\nsources = ['grader.py']\nsupport_files = ['solution.py']\nsolution_files = []\n", problem("grader"))).unwrap();
    success(
        &workspace
            .command()
            .current_dir(workspace.path("problems/grader"))
            .args(["add", "inherited"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(workspace.path("problems/inherited/main.cpp")).unwrap(),
        b"cpp template"
    );
    assert_eq!(
        fs::read_to_string(workspace.path("problems/inherited/problem.toml")).unwrap(),
        format!(
            "{}\n[files]\ncreated = [{{ path = \"main.cpp\", command = \"add\" }}]\n",
            problem("inherited")
        )
    );
    success(
        &workspace
            .command()
            .current_dir(workspace.path("problems/grader"))
            .args(["add", "explicit", "--language", "python"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(workspace.path("problems/explicit/main.py")).unwrap(),
        b"python template"
    );
    assert_eq!(
        fs::read_to_string(workspace.path("problems/explicit/problem.toml")).unwrap(),
        format!(
            "{}\n[config]\nlanguage = \"python\"\n\n[files]\ncreated = [{{ path = \"main.py\", command = \"add\" }}]\n",
            problem("explicit")
        )
    );
    workspace.empty_staging();
}

#[test]
fn configuration_and_template_errors_publish_nothing() {
    for (config, workspace_config, language, message) in [
        ("schema = 1\n", "schema = 1\n", "nosuch", "nosuch"),
        (
            "schema = 1\ndefault_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 'missing.cpp' }\n",
            "schema = 1\n",
            "cpp",
            "missing.cpp",
        ),
        (
            "schema = 1\ndefault_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n",
            "schema = 1\n[languages.cpp]\nsources = ['a.cpp', 'b.cpp']\n",
            "cpp",
            "exactly one solution file, found 2 solution files",
        ),
        (
            "schema = 1\ndefault_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n",
            "schema = 1\n[languages.cpp]\nsources = []\n",
            "cpp",
            "exactly one solution file, found 0 solution files",
        ),
        ("schema = 99\n", "schema = 1\n", "cpp", "schema"),
    ] {
        let workspace = Workspace::new();
        workspace.user_config(config);
        fs::write(workspace.path("cpforge.toml"), workspace_config).unwrap();
        fs::write(workspace.path("user/cpforge/t.cpp"), b"template").unwrap();
        failure(&workspace.add(&["one", "--language", language]), 1, message);
        assert!(!workspace.path("problems/one").exists());
        workspace.empty_staging();
    }
}

#[test]
fn template_directory_and_staging_destination_conflicts_are_refused() {
    let workspace = Workspace::new();
    workspace
        .user_config("default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n");
    fs::create_dir(workspace.path("user/cpforge/t.cpp")).unwrap();
    failure(&workspace.add(&["one"]), 1, "t.cpp");
    workspace.empty_staging();
    fs::remove_dir(workspace.path("user/cpforge/t.cpp")).unwrap();
    fs::write(workspace.path("user/cpforge/t.cpp"), b"template").unwrap();
    fs::write(
        workspace.path("cpforge.toml"),
        "[languages.cpp]\nsources = ['problem.toml']\n",
    )
    .unwrap();
    failure(&workspace.add(&["one"]), 1, "problem.toml");
    assert!(!workspace.path("problems/one").exists());
    workspace.empty_staging();
}

#[cfg(unix)]
#[test]
fn templates_are_read_through_links() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    fs::write(workspace.path("user/cpforge/real.cpp"), b"template").unwrap();
    symlink("real.cpp", workspace.path("user/cpforge/t.cpp")).unwrap();
    symlink(workspace.path("user/cpforge"), workspace.path("linked")).unwrap();
    for (name, template) in [("one", "t.cpp"), ("two", "../../linked/real.cpp")] {
        workspace.user_config(&format!(
            "default_language = 'cpp'\n[languages.cpp]\ntemplate = {{ path = '{template}' }}\n"
        ));
        success(&workspace.add(&[name]));
        assert_eq!(
            fs::read(workspace.path(format!("problems/{name}/main.cpp"))).unwrap(),
            b"template"
        );
    }
}

#[test]
fn identifiers_labels_and_contest_ids_are_validated_before_creation() {
    let workspace = Workspace::new();
    for name in [
        "", "../bad", "a..b", "a/b", "bad.", "CON", "nul.txt", "LPT9", "é",
    ] {
        failure(&workspace.add(&[name]), 1, "invalid problem name");
    }
    for contest in ["../bad", "COM1", "bad."] {
        failure(
            &workspace.add(&["one", "--contest", contest]),
            1,
            "invalid contest id",
        );
    }
    for label in ["", "has space", "é", "abcdefghijklmnopq", "A."] {
        failure(
            &workspace.add(&["one", "--contest", "round", "--label", label]),
            1,
            "label must be",
        );
    }
    failure(&workspace.add(&["one", "--label", "A"]), 2, "--contest");
    assert!(!workspace.path("problems").exists());
    assert!(!workspace.path("contests").exists());
}

#[test]
fn existing_problem_entries_are_classified_and_never_overwritten() {
    for (bytes, message) in [
        (
            Some(b"[problem]\nid = 'local:other'\n".as_slice()),
            "IdentityMismatch",
        ),
        (Some(b"schema = [".as_slice()), "malformed"),
        (Some(b"schema = 99\n".as_slice()), "too_new"),
        (
            Some(b"[problem]\nid = 'remote:one'\n".as_slice()),
            "malformed",
        ),
        (None, "missing"),
    ] {
        let workspace = Workspace::new();
        fs::create_dir_all(workspace.path("problems/one")).unwrap();
        let path = workspace.path("problems/one/problem.toml");
        if let Some(bytes) = bytes {
            fs::write(&path, bytes).unwrap();
        }
        fs::write(workspace.path("problems/one/keep"), b"protected").unwrap();
        failure(&workspace.add(&["one"]), 1, message);
        if let Some(bytes) = bytes {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert_eq!(
            fs::read(workspace.path("problems/one/keep")).unwrap(),
            b"protected"
        );
    }
    let workspace = Workspace::new();
    fs::create_dir_all(workspace.path("problems")).unwrap();
    fs::write(workspace.path("problems/one"), b"file").unwrap();
    failure(&workspace.add(&["one"]), 1, "NotADirectory");
}

#[test]
fn portable_spelling_collisions_name_the_existing_entry() {
    // These keys collide on every host, even when its filesystem distinguishes them.
    for existing in ["ONE", "K"] {
        let workspace = Workspace::new();
        fs::create_dir_all(workspace.path(format!("problems/{existing}"))).unwrap();
        let name = if existing == "ONE" { "one" } else { "K" };
        let actual = fs::read_dir(workspace.path("problems"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name();
        failure(&workspace.add(&[name]), 1, &actual.to_string_lossy());
        assert_eq!(fs::read_dir(workspace.path("problems")).unwrap().count(), 1);
    }
}

#[test]
fn contest_members_append_in_order_and_duplicates_do_not_rewrite() {
    let workspace = Workspace::new();
    success(&workspace.add(&["one", "--contest", "round", "--label", "A"]));
    let path = workspace.path("contests/round/contest.toml");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("schema = 1\n{}", contest_member("one", Some("A")))
    );
    success(&workspace.add(&["two", "--contest", "round"]));
    let bytes = format!(
        "schema = 1\n{}{}",
        contest_member("one", Some("A")),
        contest_member("two", None)
    );
    assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
    let before = fs::metadata(&path).unwrap().modified().unwrap();
    success(&workspace.add(&["one", "--contest", "round", "--label", "A"]));
    success(&workspace.add(&["two", "--contest", "round"]));
    assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), before);
}

#[test]
fn contest_label_conflicts_leave_membership_untouched() {
    let workspace = Workspace::new();
    success(&workspace.add(&["one", "--contest", "round", "--label", "A"]));
    let path = workspace.path("contests/round/contest.toml");
    let bytes = fs::read(&path).unwrap();
    success(&workspace.add(&["one", "--contest", "round"]));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    for args in [
        vec!["one", "--contest", "round", "--label", "B"],
        vec!["two", "--contest", "round", "--label", "A"],
    ] {
        failure(&workspace.add(&args), 1, "label conflict");
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    assert!(workspace.path("problems/two/problem.toml").is_file());
    success(&workspace.add(&["two", "--contest", "round", "--label", "B"]));
}

#[test]
fn comments_and_unknown_member_keys_survive_append() {
    let workspace = Workspace::new();
    fs::create_dir_all(workspace.path("contests/round")).unwrap();
    let original = "# contest\nschema=1\n\n[[problem]] # first\nid = 'local:one'\nlabel = 'A' # keep\nlimits = { time = 3 } # reserved override\n";
    let path = workspace.path("contests/round/contest.toml");
    fs::write(&path, original).unwrap();
    success(&workspace.add(&["two", "--contest", "round", "--label", "B"]));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("{original}{}", contest_member("two", Some("B")))
    );
    let bytes = fs::read(&path).unwrap();
    success(&workspace.add(&["one", "--contest", "round", "--label", "A"]));
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn bad_contest_records_allow_problem_publication_and_rerun_completes_membership() {
    for (bytes, classification) in [
        ("schema = [", "malformed"),
        ("schema = 99\n", "too_new"),
        ("schema = 1\n[[problem]]\nlabel = 'A'\n", "malformed"),
        (
            "[[problem]]\nid = 'local:one'\nlabel = 'bad label'\n",
            "malformed",
        ),
    ] {
        let workspace = Workspace::new();
        fs::create_dir_all(workspace.path("contests/round")).unwrap();
        let path = workspace.path("contests/round/contest.toml");
        fs::write(&path, bytes).unwrap();
        let output = workspace.add(&["two", "--contest", "round", "--label", "B"]);
        failure(&output, 1, "problem created; contest membership failed:");
        assert!(String::from_utf8_lossy(&output.stderr).contains(classification));
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
        let problem_path = workspace.path("problems/two/problem.toml");
        assert_eq!(fs::read(&problem_path).unwrap(), problem("two").as_bytes());
        fs::write(workspace.path("problems/two/main.cpp"), b"edited solution").unwrap();
        fs::write(&path, b"schema = 1\n").unwrap();
        // Invalid config must not prevent resuming membership of an existing problem.
        workspace.user_config("schema = 99\n");
        success(&workspace.add(&["two", "--contest", "round", "--label", "B"]));
        assert_eq!(fs::read(problem_path).unwrap(), problem("two").as_bytes());
        assert_eq!(
            fs::read(workspace.path("problems/two/main.cpp")).unwrap(),
            b"edited solution"
        );
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            format!("schema = 1\n{}", contest_member("two", Some("B")))
        );
    }
}

#[test]
fn discovery_uses_the_nearest_regular_marker_and_supports_nested_invocations() {
    let workspace = Workspace::new();
    fs::create_dir_all(workspace.path("nested/cpforge.toml")).unwrap();
    let output = workspace
        .command()
        .current_dir(workspace.path("nested"))
        .args(["add", "one"])
        .output()
        .unwrap();
    success(&output);
    assert!(workspace.path("problems/one/problem.toml").is_file());
    assert!(!workspace.path("nested/problems").exists());
    fs::remove_file(workspace.path("cpforge.toml")).unwrap();
    failure(
        &workspace
            .command()
            .current_dir(workspace.path("nested"))
            .args(["add", "two"])
            .output()
            .unwrap(),
        1,
        "outside a workspace",
    );
    assert!(!workspace.path("problems/two").exists());
}

#[cfg(unix)]
#[test]
fn linked_problem_and_contest_directories_are_refused() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    fs::create_dir_all(workspace.path("problems")).unwrap();
    fs::create_dir_all(workspace.path("protected")).unwrap();
    fs::write(workspace.path("protected/keep"), b"protected").unwrap();
    symlink(workspace.path("protected"), workspace.path("problems/one")).unwrap();
    failure(&workspace.add(&["one"]), 1, "LinkInPath");
    fs::create_dir_all(workspace.path("contests")).unwrap();
    symlink(
        workspace.path("protected"),
        workspace.path("contests/round"),
    )
    .unwrap();
    failure(
        &workspace.add(&["two", "--contest", "round"]),
        1,
        "problem created; contest membership failed:",
    );
    assert_eq!(
        fs::read_dir(workspace.path("protected")).unwrap().count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn linked_problems_and_contests_roots_are_refused() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    fs::create_dir_all(workspace.path("elsewhere/problems")).unwrap();
    fs::create_dir_all(workspace.path("elsewhere/contests")).unwrap();
    symlink(
        workspace.path("elsewhere/problems"),
        workspace.path("problems"),
    )
    .unwrap();
    failure(&workspace.add(&["one"]), 1, "refusing a link");
    assert_eq!(
        fs::read_dir(workspace.path("elsewhere/problems"))
            .unwrap()
            .count(),
        0
    );
    fs::remove_file(workspace.path("problems")).unwrap();
    symlink(
        workspace.path("elsewhere/contests"),
        workspace.path("contests"),
    )
    .unwrap();
    failure(
        &workspace.add(&["one", "--contest", "round"]),
        1,
        "problem created; contest membership failed:",
    );
    assert!(workspace.path("problems/one/problem.toml").is_file());
    assert_eq!(
        fs::read_dir(workspace.path("elsewhere/contests"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn non_portable_template_destinations_are_refused_before_staging() {
    for source in ["CON.cpp", "main.cpp:stream", "sub/aux.cpp", "Problem.toml"] {
        let workspace = Workspace::new();
        workspace.user_config(
            "default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n",
        );
        fs::write(workspace.path("user/cpforge/t.cpp"), b"template").unwrap();
        fs::write(
            workspace.path("cpforge.toml"),
            format!("[languages.cpp]\nsources = ['{source}']\n"),
        )
        .unwrap();
        failure(&workspace.add(&["one"]), 1, "template destination");
        assert!(!workspace.path("problems/one").exists(), "{source}");
        workspace.empty_staging();
    }
}

#[cfg(feature = "storage-test-hooks")]
mod crash_tests {
    use super::*;

    fn assert_aborted(output: &Output, selected: &str) {
        let point = selected.split('#').next().unwrap();
        let marker = format!("CPF-CRASH-AT {point}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.lines().any(|line| line == marker),
            "{selected}: {output:?}"
        );
        assert!(!output.status.success(), "{output:?}");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(output.status.signal(), Some(libc::SIGABRT), "{output:?}");
        }
        #[cfg(windows)]
        {
            let code = output.status.code().expect("Windows process exit code") as u32;
            assert!(
                code == 3 || code == 0x4000_0015 || code >= 0x8000_0000,
                "{output:?}"
            );
        }
    }

    fn aborting_add(workspace: &Workspace, point: &str) -> Output {
        let mut command = workspace.command();
        command
            .args(["add", "one", "--contest", "round", "--label", "A"])
            .env("CPF_STORAGE_CRASH_AT", point);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: the child hook calls only setrlimit and constructs an OS
            // error on failure; it takes no locks or allocating paths.
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
                // SAFETY: children inherit this test process's error mode;
                // suppress crash dialogs for unattended command-abort tests.
                unsafe {
                    windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                        windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
                    );
                }
            });
        }
        command.output().unwrap()
    }

    fn assert_no_temps(root: &Path) {
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            if metadata.is_dir() {
                assert_no_temps(&entry.path());
            } else {
                assert!(!entry.file_name().to_string_lossy().starts_with(".cpf-tmp-"));
            }
        }
    }

    #[test]
    fn every_add_and_primitive_boundary_preserves_allowed_states_and_converges() {
        // (point, complete problem published, contest file published, member present)
        let mut points = vec![
            ("lock.acquired", false, false, false),
            ("lock.recovered", false, false, false),
            ("add.locked", false, false, false),
            ("add.staged", false, false, false),
            ("dir_noclobber.staging_synced", false, false, false),
            ("dir_noclobber.renamed", true, false, false),
            ("dir_noclobber.directories_synced", true, false, false),
            ("add.published", true, false, false),
            ("file_noclobber.before_publish", true, false, false),
            ("file_noclobber.after_publish", true, true, false),
            ("file_noclobber.directory_synced", true, true, false),
            ("add.contest_created", true, true, false),
            ("atomic_replace.temp_synced", true, true, false),
            ("edit_toml.validated", true, true, false),
            ("atomic_replace.renamed", true, true, true),
            ("atomic_replace.directory_synced", true, true, true),
            ("edit_toml.published", true, true, true),
            ("add.member_added", true, true, true),
        ];
        if cfg!(target_os = "linux") {
            points.push(("file_noclobber.linked", true, true, false));
        }
        for (point, problem_present, contest_present, member_present) in points {
            let workspace = Workspace::new();
            workspace.user_config(
                "default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n",
            );
            let template = b"int main() {}\r\n";
            fs::write(workspace.path("user/cpforge/t.cpp"), template).unwrap();
            fs::create_dir_all(workspace.path(".cpforge/staging/old")).unwrap();
            fs::write(workspace.path(".cpforge/staging/old/leftover"), b"old").unwrap();
            let temp = workspace.path(format!(".cpf-tmp-{}", "a".repeat(32)));
            fs::write(&temp, b"leftover").unwrap();
            let output = aborting_add(&workspace, point);
            assert_aborted(&output, point);
            assert_eq!(temp.exists(), point == "lock.acquired", "{point}");
            assert_eq!(
                workspace.path(".cpforge/staging/old").exists(),
                point == "lock.acquired",
                "{point}"
            );
            let problem_path = workspace.path("problems/one");
            assert_eq!(problem_path.exists(), problem_present, "{point}");
            let problem_bytes = if problem_present {
                assert_eq!(
                    fs::read(problem_path.join("main.cpp")).unwrap(),
                    template,
                    "{point}"
                );
                let bytes = fs::read(problem_path.join("problem.toml")).unwrap();
                let text = std::str::from_utf8(&bytes).unwrap();
                let document: toml_edit::DocumentMut = text.parse().unwrap();
                assert_eq!(
                    document["problem"]["id"].as_str(),
                    Some("local:one"),
                    "{point}"
                );
                assert_eq!(
                    document["files"]["created"].as_array().unwrap().len(),
                    1,
                    "{point}"
                );
                Some(bytes)
            } else {
                None
            };
            let contest_path = workspace.path("contests/round/contest.toml");
            assert_eq!(contest_path.exists(), contest_present, "{point}");
            let final_contest = format!("schema = 1\n{}", contest_member("one", Some("A")));
            if contest_present {
                assert_eq!(
                    fs::read_to_string(&contest_path).unwrap(),
                    if member_present {
                        final_contest.as_str()
                    } else {
                        "schema = 1\n"
                    },
                    "{point}"
                );
            }
            // Editing a published user source also proves the rerun does not recopy it.
            if problem_present {
                fs::write(problem_path.join("main.cpp"), b"user edit").unwrap();
            }
            success(&workspace.add(&["one", "--contest", "round", "--label", "A"]));
            assert_eq!(
                fs::read_to_string(&contest_path).unwrap(),
                final_contest,
                "{point}"
            );
            assert_eq!(
                fs::read(problem_path.join("main.cpp")).unwrap(),
                if problem_present {
                    b"user edit".as_slice()
                } else {
                    template.as_slice()
                },
                "{point}"
            );
            if let Some(bytes) = problem_bytes {
                assert_eq!(
                    fs::read(problem_path.join("problem.toml")).unwrap(),
                    bytes,
                    "{point}"
                );
            }
            workspace.empty_staging();
            assert_no_temps(&workspace.0);
            let bytes = fs::read(&contest_path).unwrap();
            success(&workspace.add(&["one", "--contest", "round", "--label", "A"]));
            assert_eq!(fs::read(contest_path).unwrap(), bytes, "{point}");
        }
    }
}

#[cfg(not(feature = "storage-test-hooks"))]
#[test]
fn add_ignores_crash_environment_in_normal_builds() {
    let workspace = Workspace::new();
    success(
        &workspace
            .command()
            .args(["add", "one"])
            .env("CPF_STORAGE_CRASH_AT", "add.staged")
            .output()
            .unwrap(),
    );
    assert!(workspace.path("problems/one/problem.toml").is_file());
    workspace.empty_staging();
}

#[test]
fn simultaneous_adds_keep_one_problem_and_one_membership() {
    let workspace = Workspace::new();
    let first = workspace
        .command()
        .args(["add", "one", "--contest", "round", "--label", "A"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    success(&workspace.add(&["one", "--contest", "round", "--label", "A"]));
    success(&first.wait_with_output().unwrap());
    assert_eq!(
        fs::read(workspace.path("problems/one/problem.toml")).unwrap(),
        problem("one").as_bytes()
    );
    assert_eq!(
        fs::read_to_string(workspace.path("contests/round/contest.toml")).unwrap(),
        format!("schema = 1\n{}", contest_member("one", Some("A")))
    );
    workspace.empty_staging();
}

#[test]
fn oversized_records_are_refused_with_the_size_reason() {
    for target in [
        "cpforge.toml",
        "problems/one/problem.toml",
        "contests/round/contest.toml",
    ] {
        let workspace = Workspace::new();
        let path = workspace.path(target);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = vec![b'x'; 1024 * 1024 + 1];
        fs::write(&path, &bytes).unwrap();
        let output = workspace.add(&["one", "--contest", "round"]);
        failure(&output, 1, "1048576-byte size limit");
        assert_eq!(fs::read(path).unwrap(), bytes);
        if target == "contests/round/contest.toml" {
            assert!(workspace.path("problems/one/problem.toml").is_file());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("problem created; contest membership failed:")
            );
        } else if target == "cpforge.toml" {
            assert!(!workspace.path("problems/one").exists());
        }
        workspace.empty_staging();
    }
}

#[test]
fn all_configuration_diagnostics_are_reported_before_creating_a_problem() {
    let workspace = Workspace::new();
    workspace.user_config("default_language = 42\n[languages.cpp]\nsources = 17\n");
    fs::write(workspace.path("cpforge.toml"), "schema = 99\n").unwrap();
    let output = workspace.add(&["one"]);
    failure(&output, 1, "configuration failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("default_language"), "{stderr}");
    assert!(stderr.contains("sources"), "{stderr}");
    assert!(stderr.contains("cpforge.toml"), "{stderr}");
    assert!(!workspace.path("problems/one").exists());
    workspace.empty_staging();
}

#[cfg(unix)]
#[test]
fn unreadable_templates_and_records_are_reported_without_overwriting() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions and changes no process state.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let workspace = Workspace::new();
    workspace
        .user_config("default_language = 'cpp'\n[languages.cpp]\ntemplate = { path = 't.cpp' }\n");
    let template = workspace.path("user/cpforge/t.cpp");
    fs::write(&template, b"template").unwrap();
    fs::set_permissions(&template, fs::Permissions::from_mode(0o000)).unwrap();
    let output = workspace.add(&["one"]);
    fs::set_permissions(&template, fs::Permissions::from_mode(0o600)).unwrap();
    failure(&output, 1, "t.cpp");
    assert!(!workspace.path("problems/one").exists());
    workspace.empty_staging();
    fs::create_dir_all(workspace.path("problems/one")).unwrap();
    let record = workspace.path("problems/one/problem.toml");
    fs::write(&record, problem("one")).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o000)).unwrap();
    let output = workspace.add(&["one"]);
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    failure(&output, 1, "unreadable");
    assert_eq!(fs::read(record).unwrap(), problem("one").as_bytes());
}

#[test]
fn maximum_size_contest_can_be_kept_without_rendering_or_rewriting_it() {
    let workspace = Workspace::new();
    success(&workspace.add(&["one", "--contest", "round"]));
    let path = workspace.path("contests/round/contest.toml");
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(b'#');
    bytes.resize(1024 * 1024, b'x');
    fs::write(&path, &bytes).unwrap();
    success(&workspace.add(&["one", "--contest", "round"]));
    assert_eq!(fs::read(path).unwrap(), bytes);
}
