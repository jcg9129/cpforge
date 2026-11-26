//! Portable organisation commands, including real-process edit and crash seams.
#![cfg(feature = "storage-test-hooks")]
mod storage_support;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::SystemTime;

use storage_support::{Directory, assert_no_temps};

const NOW: &str = "2026-11-24T10:00:00.000Z";
const LATER: &str = "2026-11-25T11:00:00.000Z";
const BASE: &str = "schema = 1\n[problem]\nid = 'local:p'\n";

struct Workspace(Directory);
impl Workspace {
    fn new() -> Self {
        let workspace = Self(Directory::new());
        workspace.write("cpforge.toml", "schema = 1\n");
        workspace.write("user.toml", "schema = 1\n");
        workspace.write("problems/p/problem.toml", BASE);
        // Bootstrap before taking byte-for-byte workspace snapshots.
        drop(workspace.0.lock().acquire("fixture").unwrap());
        workspace
    }
    fn path(&self, path: &str) -> PathBuf {
        self.0.path(path)
    }
    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
        command
            .current_dir(&self.0.0)
            .arg("--config")
            .arg(self.path("user.toml"))
            .args(args)
            .env("CPF_STORAGE_ORG_NOW", NOW)
            .env("HOME", &self.0.0)
            .env("USERPROFILE", &self.0.0)
            .env("XDG_CONFIG_HOME", &self.0.0)
            .env("APPDATA", &self.0.0)
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .env_remove("CPF_STORAGE_BARRIER_DIR");
        command
    }
    fn run(&self, args: &[&str], code: i32) -> Output {
        let output = self.command(args).output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{args:?}: {output:?}");
        output
    }
    fn record(&self) -> String {
        fs::read_to_string(self.path("problems/p/problem.toml")).unwrap()
    }
    fn fields(&self) -> toml_edit::DocumentMut {
        self.record().parse().unwrap()
    }
    fn unchanged(&self, args: &[&str], code: i32) -> Output {
        let before = snapshot(&self.0.0);
        let output = self.run(args, code);
        assert_eq!(snapshot(&self.0.0), before, "{args:?}");
        output
    }
}

fn identity(file: &File) -> [u64; 2] {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().unwrap();
        [metadata.dev(), metadata.ino()]
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: file owns a live handle and info is valid writable storage.
        assert_ne!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) },
            0
        );
        [
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        ]
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    bytes: Vec<u8>,
    record: Option<([u64; 2], SystemTime)>,
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn visit(root: &Path, dir: &Path, entries: &mut BTreeMap<PathBuf, Entry>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                visit(root, &path, entries);
            } else if metadata.is_file() {
                let record = matches!(
                    entry.file_name().to_str(),
                    Some("problem.toml" | "contest.toml")
                )
                .then(|| {
                    (
                        identity(&File::open(&path).unwrap()),
                        metadata.modified().unwrap(),
                    )
                });
                entries.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    Entry {
                        bytes: fs::read(&path).unwrap(),
                        record,
                    },
                );
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn paused(workspace: &Workspace, args: &[&str], point: &str) -> Child {
    let mut child = workspace
        .command(args)
        .env("CPF_STORAGE_PAUSE_AT", point)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    barrier(&mut child, point);
    child
}
fn barrier(child: &mut Child, point: &str) {
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim_end(), format!("CPF-PAUSED {point}"));
}
fn release(child: &mut Child) {
    child.stdin.as_mut().unwrap().write_all(b"x").unwrap();
}
fn finished(child: Child, code: i32) -> Output {
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    output
}
fn replace(workspace: &Workspace, bytes: &str, atomic: bool) {
    if atomic {
        workspace.write("problems/p/editor.toml", bytes);
        #[cfg(unix)]
        fs::rename(
            workspace.path("problems/p/editor.toml"),
            workspace.path("problems/p/problem.toml"),
        )
        .unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};
            let source: Vec<_> = workspace
                .path("problems/p/editor.toml")
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            let target: Vec<_> = workspace
                .path("problems/p/problem.toml")
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            // SAFETY: both buffers are NUL-terminated native paths.
            assert_ne!(
                unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_REPLACE_EXISTING) },
                0
            );
        }
    } else {
        workspace.write("problems/p/problem.toml", bytes);
    }
}

#[test]
fn argument_grammars_are_checked_without_workspace_io() {
    let outside = Directory::new();
    let invalid = [
        vec!["tag", "p", "a b"],
        vec!["tag", "p"],
        vec!["mark", "p", "Solved"],
        vec!["mark", "contest:c", "solved"],
        vec!["--problem", "a", "tag", ".", "dp"],
        vec!["notes", "contest:c"],
        vec!["notes", "p", "--problem", "q"],
        vec!["tag", "p", "dp", "--json"],
    ];
    for args in invalid {
        let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(&outside.0)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
    }
    for tag in [
        "",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "-x",
        "_x",
        "a b",
        "a.b",
        "a/b",
        "é",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(&outside.0)
            .args(["tag", "p", tag])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{tag}: {output:?}");
    }
    for status in [
        "In-progress",
        "-a",
        "a-",
        "1a",
        "a_b",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(&outside.0)
            .args(["mark", "p", status])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{status}: {output:?}");
    }
}

#[test]
fn tag_edits_preserve_exact_crlf_bytes_and_noops_keep_identity_and_mtime() {
    let w = Workspace::new();
    let before = "# header\r\nschema = 1\r\n\r\n[problem]\r\nid = 'local:p' # identity\r\nunknown = 42\r\n\r\n[config]\r\nlanguage = 'python' # config\r\n";
    w.write("problems/p/problem.toml", before);
    let output = w.run(&["tag", "p", "dp", "graphs"], 0);
    assert!(output.stdout.is_empty());
    assert_eq!(
        w.record(),
        before.replace(
            "unknown = 42\r\n",
            &format!("unknown = 42\r\ntags = [\"dp\", \"graphs\"]\r\nupdated_at = \"{NOW}\"\r\n")
        )
    );
    w.unchanged(&["tag", "p", "dp", "graphs"], 0);
    w.unchanged(&["tag", "p", "DP"], 0);
    w.run(&["tag", "p", "Graphs", "x", "x"], 0);
    assert!(
        w.record()
            .contains("tags = [\"dp\", \"graphs\", \"x\"]\r\n")
    );
    let output = w
        .command(&["untag", "p", "GRAPHS"])
        .env("CPF_STORAGE_ORG_NOW", LATER)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(w.fields()["problem"]["updated_at"].as_str(), Some(LATER));
    assert!(
        String::from_utf8_lossy(&w.unchanged(&["untag", "p", "nope"], 0).stderr)
            .contains("not present")
    );
    w.run(&["untag", "p", "dp", "x"], 0);
    assert!(w.fields()["problem"]["tags"].as_array().unwrap().is_empty());
    w.write(
        "problems/p/problem.toml",
        format!("{BASE}tags = [\n  'dp', # keep this comment\n]\n"),
    );
    w.run(&["tag", "p", "graphs"], 0);
    assert!(w.record().contains("  'dp', # keep this comment\n"));
    // Without a trailing comma the old last line keeps its comment, and the
    // closing bracket keeps its own line.
    w.write(
        "problems/p/problem.toml",
        format!("{BASE}tags = [\n  'dp',\n  'dfs' # last\n]\n"),
    );
    w.run(&["tag", "p", "graphs"], 0);
    assert_eq!(
        w.record(),
        format!(
            "{BASE}tags = [\n  'dp',\n  'dfs', # last\n  \"graphs\",\n]\n{}",
            updated(NOW)
        )
    );
    w.run(&["untag", "p", "dp"], 0);
    assert!(
        w.record()
            .contains("tags = [\n  'dfs', # last\n  \"graphs\",\n]\n")
    );
    w.write(
        "problems/p/problem.toml",
        format!("{BASE}tags = ['a', 'b']\n"),
    );
    w.run(&["untag", "p", "a"], 0);
    assert!(w.record().contains("tags = ['b']\n"));
}

fn updated(clock: &str) -> String {
    format!("updated_at = \"{clock}\"\n")
}

#[test]
fn mark_dates_and_vocabulary_obey_their_independent_rules() {
    let w = Workspace::new();
    // Only the value changes: the key's spacing and the comment stay.
    w.write(
        "problems/p/problem.toml",
        format!("{BASE}status   =   'new' # mine\n"),
    );
    w.run(&["mark", "p", "skipped"], 0);
    assert_eq!(
        w.record(),
        format!("{BASE}status   =   \"skipped\" # mine\n{}", updated(NOW))
    );
    w.write("problems/p/problem.toml", BASE);
    w.run(&["mark", "p", "new"], 0);
    assert_eq!(
        w.record(),
        format!("{BASE}status = \"new\"\n{}", updated(NOW))
    );
    w.write("problems/p/problem.toml", BASE);
    w.run(&["mark", "p", "solved"], 0);
    assert_eq!(w.fields()["problem"]["status"].as_str(), Some("solved"));
    for key in ["solved_at", "updated_at"] {
        assert_eq!(w.fields()["problem"][key].as_str(), Some(NOW));
    }
    let before = snapshot(&w.0.0);
    assert!(
        w.command(&["mark", "p", "solved"])
            .env("CPF_STORAGE_ORG_NOW", LATER)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(snapshot(&w.0.0), before);
    for status in ["in-progress", "solved"] {
        assert!(
            w.command(&["mark", "p", status])
                .env("CPF_STORAGE_ORG_NOW", LATER)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(w.fields()["problem"]["updated_at"].as_str(), Some(LATER));
        assert_eq!(w.fields()["problem"]["solved_at"].as_str(), Some(NOW));
    }
    w.write(
        "problems/p/problem.toml",
        format!("{BASE}status = 'solved'\n"),
    );
    w.unchanged(&["mark", "p", "solved"], 0);
    w.run(&["mark", "p", "new"], 0);
    w.run(&["mark", "p", "solved"], 0);
    assert_eq!(w.fields()["problem"]["solved_at"].as_str(), Some(NOW));
    let output = w.unchanged(&["mark", "p", "wa"], 7);
    assert!(String::from_utf8_lossy(&output.stderr).contains("new, in-progress, solved, skipped"));
    w.write("cpforge.toml", "schema = 1\n[status]\nvalues = ['wa']\n");
    w.run(&["mark", "p", "wa"], 0);
    w.write("user.toml", "schema = 1\n[status]\nvalues = ['wa']\n");
    w.write("cpforge.toml", "schema = 1\n[status]\nvalues = ['x']\n");
    w.unchanged(&["mark", "p", "wa"], 7);
    w.write("cpforge.toml", "schema = 1\n[status]\nvalues = [1]\n");
    w.unchanged(&["mark", "p", "new"], 7);
    w.write(
        "cpforge.toml",
        "schema = 1\n[languages.broken]\nsources = 5\n",
    );
    assert!(String::from_utf8_lossy(&w.run(&["mark", "p", "new"], 0).stderr).contains("warning"));
    w.write(
        "cpforge.toml",
        "schema = 1\n[status]\nvalues = ['a', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa']\n",
    );
    for status in ["a", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
        w.run(&["mark", "p", status], 0);
    }
    w.write("cpforge.toml", "[broken\n");
    w.unchanged(&["mark", "p", "solved"], 7);
    // Tags load no configuration, even when the entire workspace layer is bad.
    w.run(&["tag", "p", "dp"], 0);
}

#[test]
fn invalid_fields_block_only_their_writers_and_status_keeps_raw_values() {
    let w = Workspace::new();
    for (field, value, commands) in [
        (
            "tags",
            "'dp'",
            vec![vec!["tag", "p", "dp"], vec!["untag", "p", "dp"]],
        ),
        (
            "updated_at",
            "5",
            vec![
                vec!["tag", "p", "dp"],
                vec!["untag", "p", "x"],
                vec!["mark", "p", "solved"],
            ],
        ),
        ("solved_at", "'x'", vec![vec!["mark", "p", "solved"]]),
    ] {
        w.write(
            "problems/p/problem.toml",
            format!("{BASE}tags = ['x']\n").replace(
                "tags = ['x']\n",
                &format!(
                    "{}{} = {value}\n",
                    if field == "tags" {
                        ""
                    } else {
                        "tags = ['x']\n"
                    },
                    field
                ),
            ),
        );
        for command in commands {
            assert!(String::from_utf8_lossy(&w.unchanged(&command, 8).stderr).contains(field));
        }
        for command in [
            vec!["current", "--problem", "p"],
            vec!["switch", "p"],
            vec!["status", "p"],
        ] {
            w.run(&command, 0);
        }
        let output = w.run(&["status", "p", "--json"], 0);
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            document["problems"][0]["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["field"] == field)
        );
        if field == "tags" {
            w.run(&["mark", "p", "solved"], 0);
        }
        if field == "solved_at" {
            w.run(&["mark", "p", "skipped"], 0);
        }
    }
    w.write(
        "problems/p/problem.toml",
        format!("{BASE}tags = ['dp', 'DP']\nstatus = 'Working on it'\n"),
    );
    let output = w.run(&["status", "p", "--json"], 0);
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        document["problems"][0]["tags"],
        serde_json::json!(["dp", "DP"])
    );
    assert_eq!(document["problems"][0]["user_status"], "Working on it");
    assert_eq!(
        document["problems"][0]["findings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn stored_tag_and_timestamp_grammar_tables() {
    let w = Workspace::new();
    for tag in ["dp", "DP", "2-sat", "a", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
        w.run(&["tag", "p", tag], 0);
    }
    let sixty_four = (0..64)
        .map(|n| format!("'t{n}'"))
        .collect::<Vec<_>>()
        .join(", ");
    for (tags, valid) in [
        ("[]".to_owned(), true),
        (format!("[{sixty_four}]"), true),
        ("'dp'".into(), false),
        ("[1]".into(), false),
        ("['dp', 'DP']".into(), false),
        (format!("[{sixty_four}, 'extra']"), false),
    ] {
        w.write("problems/p/problem.toml", format!("{BASE}tags = {tags}\n"));
        w.unchanged(&["untag", "p", "absent"], if valid { 0 } else { 8 });
    }
    for (text, valid) in [
        ("2026-11-24T10:00:00.000Z", true),
        ("2026-11-24T10:00:00Z", true),
        ("2026-11-24", true),
        ("2024-02-29", true),
        ("1970-01-01", true),
        ("1970-01-01T00:00:00.000Z", true),
        ("9999-12-30T22:00:00.999Z", true),
        ("9999-12-31", true),
        ("9999-12-30T22:00:01.000Z", false),
        ("9999-12-31T00:00:00Z", false),
        ("9999-12-31T23:59:59.999Z", false),
        ("1969-12-31T23:59:59.999Z", false),
        ("2026-02-29", false),
        ("2026-13-01", false),
        ("2026-11-31", false),
        ("2026-11-24T24:00:00Z", false),
        ("2026-11-24T10:60:00Z", false),
        ("2026-11-24T10:00:60Z", false),
        ("2026-11-24T10:00:00+01:00", false),
        ("2026-11-24T10:00:00z", false),
        ("2026-11-24T10:00:00.12Z", false),
        ("2026-11-24T10:00:00", false),
        ("1969-12-31", false),
        ("10000-01-01", false),
    ] {
        w.write(
            "problems/p/problem.toml",
            format!("{BASE}updated_at = '{text}'\n"),
        );
        w.run(&["tag", "p", "dp"], if valid { 0 } else { 8 });
    }
    w.write(
        "problems/p/problem.toml",
        format!("{BASE}updated_at = 2026-11-24T10:00:00Z\n"),
    );
    w.unchanged(&["tag", "p", "dp"], 8);
}

#[test]
fn counts_and_binding_are_rechecked_after_every_editor_save() {
    let w = Workspace::new();
    let tags = (0..63)
        .map(|n| format!("'t{n}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let base = format!("{BASE}tags = [{tags}]\n");
    w.write("problems/p/problem.toml", &base);
    assert!(
        String::from_utf8_lossy(&w.unchanged(&["tag", "p", "a", "A", "b"], 8).stderr)
            .contains("too_many_tags")
    );
    w.run(&["tag", "p", "a", "A"], 0);
    assert_eq!(w.fields()["problem"]["tags"].as_array().unwrap().len(), 64);
    w.write("problems/p/problem.toml", &base);
    let mut child = paused(&w, &["tag", "p", "x"], "org.before_validation");
    let editor = base.replace("]\n", ", 'editor']\n");
    replace(&w, &editor, false);
    release(&mut child);
    finished(child, 8);
    assert_eq!(w.record(), editor);
    for point in ["org.after_resolve", "org.before_validation"] {
        w.write("problems/p/problem.toml", BASE);
        let mut child = paused(&w, &["tag", "p", "dp"], point);
        let editor = BASE.replace("local:p", "local:q");
        replace(&w, &editor, true);
        release(&mut child);
        assert!(String::from_utf8_lossy(&finished(child, 8).stderr).contains("IdentityMismatch"));
        assert_eq!(w.record(), editor);
    }
    w.write("contests/c/contest.toml", "schema = 1\n");
    let mut child = paused(&w, &["tag", "contest:c", "rated"], "org.after_resolve");
    let editor = "schema = 1\n[contest]\nid = 'atcoder:c'\n";
    w.write("contests/c/contest.toml", editor);
    release(&mut child);
    assert!(String::from_utf8_lossy(&finished(child, 8).stderr).contains("IdentityMismatch"));
    assert_eq!(
        fs::read_to_string(w.path("contests/c/contest.toml")).unwrap(),
        editor
    );
}

#[test]
fn editor_conflicts_replay_once_and_cooperating_commands_serialize() {
    let w = Workspace::new();
    for atomic in [false, true] {
        for conflicts in [1, 2] {
            w.write("problems/p/problem.toml", BASE);
            let mut child = paused(&w, &["tag", "p", "dp"], "org.before_validation");
            let mut editor = String::from(BASE);
            for count in 0..conflicts {
                if count > 0 {
                    barrier(&mut child, "org.before_validation");
                }
                editor.push_str("# editor saved\n");
                replace(&w, &editor, atomic);
                release(&mut child);
            }
            if conflicts == 1 {
                barrier(&mut child, "org.before_validation");
                release(&mut child);
                finished(child, 0);
                assert!(w.record().contains("# editor saved"));
                assert!(w.record().contains("tags = [\"dp\"]"));
            } else {
                finished(child, 8);
                assert_eq!(w.record(), editor);
            }
            assert_no_temps(&w.path("problems/p"));
        }
    }
    w.write("problems/p/problem.toml", BASE);
    let mut first = paused(&w, &["tag", "p", "first"], "org.before_validation");
    let mut second = w
        .command(&["tag", "p", "second"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(second.try_wait().unwrap().is_none());
    release(&mut first);
    finished(first, 0);
    finished(second, 0);
    let document = w.fields();
    let tags: Vec<_> = document["problem"]["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(tags, ["first", "second"]);
}

#[test]
fn targets_include_saved_selection_contests_and_remote_references() {
    let w = Workspace::new();
    w.run(&["switch", "p"], 0);
    w.run(&["tag", ".", "saved"], 0);
    assert!(
        w.command(&["tag", ".", "cwd"])
            .current_dir(w.path("problems/p"))
            .output()
            .unwrap()
            .status
            .success()
    );
    fs::remove_file(w.path(".cpforge/state.toml")).unwrap();
    assert!(
        String::from_utf8_lossy(&w.unchanged(&["tag", ".", "x"], 8).stderr).contains("NoSelection")
    );
    let contest = "schema = 1\n\n[[problem]]\nid = 'local:p'\nlabel = 'A' # member\n";
    w.write("contests/c/contest.toml", contest);
    assert!(
        String::from_utf8_lossy(&w.unchanged(&["tag", "c", "x"], 8).stderr).contains("contest")
    );
    w.run(&["tag", "contest:c", "rated"], 0);
    assert_eq!(
        fs::read_to_string(w.path("contests/c/contest.toml")).unwrap(),
        contest.replacen("schema = 1\n", "schema = 1\ntags = [\"rated\"]\n", 1)
    );
    w.run(&["tag", "c/A", "label"], 0);
    w.write(
        "problems/atcoder-abc400_a/problem.toml",
        "[problem]\nid = 'atcoder:abc400_a'\n",
    );
    w.run(&["tag", "atcoder:abc400_a", "remote"], 0);
    w.write("contests/c/contest.toml", "not toml");
    w.unchanged(&["tag", "contest:c", "x"], 8);
}

#[test]
fn size_clock_and_creation_dates() {
    let w = Workspace::new();
    let mut padded = format!("{BASE}#");
    padded.push_str(&"x".repeat(1024 * 1024 - 10 - padded.len()));
    w.write("problems/p/problem.toml", padded);
    assert!(
        String::from_utf8_lossy(&w.unchanged(&["tag", "p", "dp", "graphs"], 8).stderr)
            .contains("record_too_large")
    );
    for (clock, valid) in [
        ("9999-12-30T22:00:01.000Z", false),
        ("1969-12-31T23:59:59.999Z", false),
        ("9999-12-30T22:00:00.999Z", true),
        ("1970-01-01T00:00:00.000Z", true),
    ] {
        for args in [
            vec!["tag", "p", "dp"],
            vec!["untag", "p", "x"],
            vec!["mark", "p", "solved"],
            vec!["add", "q"],
        ] {
            w.write("problems/p/problem.toml", format!("{BASE}tags = ['x']\n"));
            if w.path("problems/q").exists() {
                fs::remove_dir_all(w.path("problems/q")).unwrap();
            }
            let before = snapshot(&w.0.0);
            let output = w
                .command(&args)
                .env("CPF_STORAGE_ORG_NOW", clock)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(if valid { 0 } else { 8 }),
                "{args:?}: {output:?}"
            );
            if valid {
                let (path, field) = if args[0] == "add" {
                    ("problems/q/problem.toml", "created_at")
                } else {
                    ("problems/p/problem.toml", "updated_at")
                };
                let document: toml_edit::DocumentMut =
                    fs::read_to_string(w.path(path)).unwrap().parse().unwrap();
                assert_eq!(document["problem"][field].as_str(), Some(clock));
                if args[0] == "mark" {
                    assert_eq!(document["problem"]["solved_at"].as_str(), Some(clock));
                }
                if args[0] == "add" {
                    assert!(document["problem"].get("updated_at").is_none());
                }
            } else {
                assert_eq!(snapshot(&w.0.0), before);
                assert!(String::from_utf8_lossy(&output.stderr).contains("clock"));
            }
            assert_no_temps(&w.path("problems/p"));
        }
    }
    w.run(&["add", "two-sum"], 0);
    let document: toml_edit::DocumentMut =
        fs::read_to_string(w.path("problems/two-sum/problem.toml"))
            .unwrap()
            .parse()
            .unwrap();
    assert_eq!(document["problem"]["created_at"].as_str(), Some(NOW));
    let before = fs::read(w.path("problems/two-sum/problem.toml")).unwrap();
    w.run(&["add", "--contest", "c", "two-sum"], 0);
    assert_eq!(
        fs::read(w.path("problems/two-sum/problem.toml")).unwrap(),
        before
    );
}

fn crash(workspace: &Workspace, args: &[&str], point: &str) -> Output {
    let mut command = workspace.command(args);
    command.env("CPF_STORAGE_CRASH_AT", point);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the child hook only invokes the async-signal-safe setrlimit.
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
        static MODE: std::sync::Once = std::sync::Once::new();
        MODE.call_once(|| {
            // SAFETY: children inherit this error mode; no handles are involved.
            unsafe {
                windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                    windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
                );
            }
        });
    }
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&format!("CPF-CRASH-AT {point}")),
        "{output:?}"
    );
    output
}

#[test]
fn crashes_leave_exact_old_or_new_record_bytes_and_recover_temps() {
    for args in [
        vec!["tag", "p", "dp"],
        vec!["untag", "p", "x"],
        vec!["mark", "p", "solved"],
    ] {
        let expected = Workspace::new();
        let base = format!("{BASE}tags = ['x']\n");
        expected.write("problems/p/problem.toml", &base);
        expected.run(&args, 0);
        for (point, committed) in [("org.edit_validated", false), ("org.edit_committed", true)] {
            let w = Workspace::new();
            w.write("problems/p/problem.toml", &base);
            let old_identity = identity(&File::open(w.path("problems/p/problem.toml")).unwrap());
            crash(&w, &args, point);
            assert_eq!(
                w.record(),
                if committed {
                    expected.record()
                } else {
                    base.clone()
                }
            );
            let temps = || {
                fs::read_dir(w.path("problems/p"))
                    .unwrap()
                    .filter(|e| {
                        e.as_ref()
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".cpf-tmp-")
                    })
                    .count()
            };
            assert_eq!(temps(), usize::from(!committed));
            if !committed {
                assert_eq!(
                    identity(&File::open(w.path("problems/p/problem.toml")).unwrap()),
                    old_identity
                );
            }
            w.run(&args, 0);
            assert_eq!(w.record(), expected.record());
            assert_no_temps(&w.path("problems/p"));
        }
    }
}

#[test]
fn notes_never_clobber_and_path_mode_is_a_lock_free_reader() {
    let w = Workspace::new();
    fs::remove_file(w.path(".cpforge/lock")).unwrap();
    let path = w.path("problems/p/notes.md");
    let mut line = cpforge::report::wire::native_bytes(path.as_os_str());
    line.push(b'\n');
    let output = w.unchanged(&["notes", "--path", "p"], 0);
    assert_eq!(output.stdout, line);
    assert_eq!(output.stderr, b"absent\n");
    assert!(!w.path(".cpforge/lock").exists());
    let output = w.run(&["notes", "p"], 0);
    assert_eq!(output.stdout, line);
    assert_eq!(output.stderr, b"created\n");
    assert_eq!(fs::read(&path).unwrap(), b"");
    let output = w.unchanged(&["notes", "p"], 0);
    assert_eq!(output.stderr, b"exists\n");
    fs::write(&path, b"user notes").unwrap();
    w.unchanged(&["notes", "p"], 0);
    w.unchanged(&["notes", "--path", "p"], 0);
    let output = w.run(&["status", "p", "--json"], 0);
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["problems"][0]["untracked"], serde_json::json!([]));
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let output = w.run(&["notes", "p"], 8);
    assert!(output.stdout.is_empty());
    let output = w.unchanged(&["notes", "--path", "p"], 0);
    assert_eq!(output.stderr, b"not a regular file\n");
    fs::remove_dir(&path).unwrap();
    for (exists, args, state) in [
        (false, vec!["notes", "--path", "p", "--json"], "absent"),
        (false, vec!["notes", "p", "--json"], "created"),
        (true, vec!["notes", "p", "--json"], "existing"),
    ] {
        assert_eq!(path.exists(), exists);
        let output = w.run(&args, 0);
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(document["document"], "notes");
        assert_eq!(document["problem"], "local:p");
        assert_eq!(document["state"], state);
        let encoded = serde_json::to_vec(&document["path"]).unwrap();
        assert_eq!(
            cpforge::report::wire::decode_native(&encoded).unwrap(),
            path.as_os_str()
        );
    }
    fs::remove_file(&path).unwrap();
    let mut child = paused(&w, &["notes", "p"], "notes.before_publish");
    fs::write(&path, b"x").unwrap();
    release(&mut child);
    assert_eq!(finished(child, 0).stderr, b"exists\n");
    assert_eq!(fs::read(&path).unwrap(), b"x");
    assert_no_temps(&w.path("problems/p"));
    fs::remove_file(&path).unwrap();
    let first = w
        .command(&["notes", "p"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = w
        .command(&["notes", "p"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(finished(first, 0).stdout, line);
    assert_eq!(finished(second, 0).stdout, line);
    for (point, published) in [("notes.before_publish", false), ("notes.published", true)] {
        fs::remove_file(&path).unwrap();
        crash(&w, &["notes", "p"], point);
        assert_eq!(path.exists(), published);
        if published {
            assert_eq!(fs::read(&path).unwrap(), b"");
        }
        w.run(&["notes", "p"], 0);
        assert_no_temps(&w.path("problems/p"));
    }
    // Neither notes nor test-catalog edits own updated_at.
    for date in ["", "updated_at = '2026-01-01T00:00:00.000Z'\n"] {
        w.write("problems/p/problem.toml", format!("{BASE}{date}"));
        let before = w.record();
        w.run(&["notes", "p"], 0);
        assert_eq!(w.record(), before);
        w.run(&["tag", "p", "x"], 0);
        assert_eq!(w.fields()["problem"]["updated_at"].as_str(), Some(NOW));
    }
}

#[cfg(unix)]
#[test]
fn notes_links_and_fifos_are_refused_without_following_or_blocking() {
    use std::os::unix::ffi::OsStrExt;
    let w = Workspace::new();
    let path = w.path("problems/p/notes.md");
    w.write("user-content", b"untouched");
    std::os::unix::fs::symlink(w.path("user-content"), &path).unwrap();
    w.unchanged(&["notes", "p"], 8);
    assert_eq!(fs::read(w.path("user-content")).unwrap(), b"untouched");
    fs::remove_file(&path).unwrap();
    let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: native is a NUL-terminated path; mkfifo owns no Rust storage.
    assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
    w.run(&["notes", "p"], 8);
    w.run(&["notes", "p", "--path"], 0);
}

#[cfg(windows)]
#[test]
fn windows_editor_sharing_denials_retry_within_the_documented_bound() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    for release_early in [false, true] {
        let w = Workspace::new();
        let mut child = paused(&w, &["tag", "p", "dp"], "org.before_validation");
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(w.path("problems/p/problem.toml"))
            .unwrap();
        release(&mut child);
        if release_early {
            std::thread::sleep(std::time::Duration::from_millis(500));
            drop(handle);
            finished(child, 0);
        } else {
            finished(child, 8);
            assert_eq!(w.record(), BASE);
            drop(handle);
        }
    }
}

#[test]
fn catalog_and_adoption_edits_preserve_dates_and_workflow_status() {
    for date in ["", "updated_at = '2026-01-01T00:00:00.000Z'\n"] {
        let w = Workspace::new();
        w.write("problems/p/problem.toml", format!("{BASE}{date}status = 'in-progress'\nsolved_at = '2026-01-01'\ncreated_at = '2025-01-01'\n"));
        w.write("problems/p/owned.txt", b"user source");
        w.write("problems/p/tests/registered.in", b"input");
        w.write("input", b"new input");
        let original = w.fields()["problem"].to_string();
        for args in [
            vec!["--problem", "p", "adopt", "owned.txt"],
            vec!["--problem", "p", "test", "add", "--input", "input"],
            vec![
                "--problem",
                "p",
                "test",
                "register",
                "problems/p/tests/registered.in",
                "--id",
                "registered",
                "--no-answer",
            ],
            vec!["--problem", "p", "test", "disable", "registered"],
            vec!["--problem", "p", "test", "enable", "registered"],
        ] {
            w.run(&args, 0);
            assert_eq!(w.fields()["problem"].to_string(), original);
        }
        w.run(&["tag", "p", "x"], 0);
        let fields = w.fields();
        assert_eq!(fields["problem"]["updated_at"].as_str(), Some(NOW));
        assert_eq!(fields["problem"]["status"].as_str(), Some("in-progress"));
        assert_eq!(fields["problem"]["solved_at"].as_str(), Some("2026-01-01"));
        assert_eq!(fields["problem"]["created_at"].as_str(), Some("2025-01-01"));
    }
}

#[test]
fn passing_test_jobs_never_write_the_workflow_status() {
    let w = Workspace::new();
    w.write(
        "cpforge.toml",
        "schema = 1\n[languages.script]\nextensions = ['sh']\nsources = ['main.sh']\nbuild = false\n[languages.script.run]\nargv = ['sh', '{source}']\n",
    );
    w.write(
        ".cpforge/job-seq",
        "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
    );
    for (name, status, workflow) in [
        ("p", "status = 'in-progress'\n", "in-progress"),
        ("q", "", "new"),
    ] {
        w.write(
            &format!("problems/{name}/problem.toml"),
            format!("schema = 1\n[problem]\nid = 'local:{name}'\n{status}[config]\nlanguage = 'script'\n"),
        );
        w.write(&format!("problems/{name}/main.sh"), "printf 'answer\\n'\n");
        w.write(
            &format!("problems/{name}/tests.toml"),
            "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n",
        );
        w.write(&format!("problems/{name}/tests/a.in"), "input\n");
        w.write(&format!("problems/{name}/tests/a.ans"), "answer\n");
        let record = w.path(&format!("problems/{name}/problem.toml"));
        let before = fs::read(&record).unwrap();
        let output = w
            .command(&["--problem", name, "test"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        // Windows runs no job processes yet: the same command line refuses.
        let expected = if cfg!(windows) { 10 } else { 0 };
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
        assert_eq!(fs::read(&record).unwrap(), before);
        let output = w.run(&["status", name], 0);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("workflow: {workflow}\n")),
            "{output:?}"
        );
        assert_eq!(fs::read(&record).unwrap(), before);
    }
}
